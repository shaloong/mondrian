//! Probe-backed and session-observed keyframe index for Preview seeks.
//!
//! The index owns ordered keyframe evidence, seek-anchor lookup, and the
//! bounded cross-session probe cache. It never chooses an access policy;
//! callers supply the target and consume typed diagnostics.

use super::demux_protocol::MAX_KEYFRAME_ANCHORS;
use super::*;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct PreviewSeekIndexDiagnostics {
    pub(super) available: bool,
    pub(super) keyframes: u32,
    pub(super) observed_packets: u32,
    pub(super) source: PreviewSeekIndexSource,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct PreviewSeekResolution {
    pub(super) used_index: bool,
    pub(super) anchor_pts: Option<i64>,
}

#[derive(Debug, Default)]
pub(super) struct PreviewSeekIndex {
    pub(super) keyframe_pts: Vec<i64>,
    observed_packets: u32,
    pub(super) source: PreviewSeekIndexSource,
}

impl PreviewSeekIndex {
    pub(super) fn from_probe_keyframes(mut keyframe_pts: Vec<i64>) -> Self {
        keyframe_pts.sort_unstable();
        keyframe_pts.dedup();
        let source = if keyframe_pts.is_empty() {
            PreviewSeekIndexSource::None
        } else {
            PreviewSeekIndexSource::ProbeBacked
        };
        Self { keyframe_pts, observed_packets: 0, source }
    }

    pub(super) fn observe_packet(&mut self, packet: &ffmpeg::Packet) {
        self.observed_packets = self.observed_packets.saturating_add(1);
        if !packet.is_key() {
            return;
        }
        let Some(pts) = packet.pts().or_else(|| packet.dts()) else {
            return;
        };
        let inserted = self.insert_keyframe_pts(pts);
        if inserted && self.source == PreviewSeekIndexSource::None {
            self.source = PreviewSeekIndexSource::SessionObserved;
        }
    }

    pub(super) fn insert_keyframe_pts(&mut self, pts: i64) -> bool {
        match self.keyframe_pts.binary_search(&pts) {
            Ok(_) => false,
            Err(index) => {
                self.keyframe_pts.insert(index, pts);
                true
            }
        }
    }

    pub(super) fn keyframe_at_or_before(&self, target_pts: i64) -> Option<i64> {
        match self.keyframe_pts.binary_search(&target_pts) {
            Ok(index) => self.keyframe_pts.get(index).copied(),
            Err(0) => None,
            Err(index) => self.keyframe_pts.get(index - 1).copied(),
        }
    }

    pub(super) fn keyframe_after(&self, target_pts: i64) -> Option<i64> {
        match self.keyframe_pts.binary_search(&target_pts) {
            Ok(index) => self.keyframe_pts.get(index + 1).copied(),
            Err(index) => self.keyframe_pts.get(index).copied(),
        }
    }

    pub(super) fn nearest_keyframe(&self, target_pts: i64) -> Option<i64> {
        let before = self.keyframe_at_or_before(target_pts);
        let after = self.keyframe_after(target_pts);
        match (before, after) {
            (Some(before), Some(after)) => {
                if target_pts.saturating_sub(before) <= after.saturating_sub(target_pts) {
                    Some(before)
                } else {
                    Some(after)
                }
            }
            (Some(before), None) => Some(before),
            (None, Some(after)) => Some(after),
            (None, None) => None,
        }
    }

    pub(super) fn adjacent_keyframe_radius(&self, target_pts: i64) -> Option<i64> {
        let search = self.keyframe_pts.binary_search(&target_pts);
        let insertion = search.unwrap_or_else(|index| index);
        let before = insertion
            .checked_sub(1)
            .and_then(|index| self.keyframe_pts.get(index))
            .map(|pts| target_pts.saturating_sub(*pts).abs());
        let after_index = match search {
            Ok(index) => index.saturating_add(1),
            Err(index) => index,
        };
        let after = self
            .keyframe_pts
            .get(after_index)
            .map(|pts| pts.saturating_sub(target_pts).abs());
        match (before, after) {
            (Some(before), Some(after)) => Some(before.max(after)),
            (Some(distance), None) | (None, Some(distance)) => Some(distance),
            (None, None) => None,
        }
    }

    pub(super) fn diagnostics(&self) -> PreviewSeekIndexDiagnostics {
        PreviewSeekIndexDiagnostics {
            available: !self.keyframe_pts.is_empty(),
            keyframes: self.keyframe_pts.len().min(u32::MAX as usize) as u32,
            observed_packets: self.observed_packets,
            source: self.source,
        }
    }
}

#[derive(Debug, Clone)]
struct PreviewSeekIndexCacheEntry {
    path: PathBuf,
    fingerprint: MediaFileFingerprint,
    stream_index: usize,
    keyframe_pts: Vec<i64>,
    retained_bytes: usize,
}

impl PreviewSeekIndexCacheEntry {
    fn new(
        path: &Path,
        fingerprint: MediaFileFingerprint,
        stream_index: usize,
        mut keyframe_pts: Vec<i64>,
    ) -> Self {
        keyframe_pts.sort_unstable();
        keyframe_pts.dedup();
        let retained_bytes = std::mem::size_of::<Self>()
            .saturating_add(path.as_os_str().len())
            .saturating_add(keyframe_pts.capacity().saturating_mul(std::mem::size_of::<i64>()));
        Self {
            path: path.to_path_buf(),
            fingerprint,
            stream_index,
            keyframe_pts,
            retained_bytes,
        }
    }

    fn matches(&self, path: &Path, fingerprint: MediaFileFingerprint, stream_index: usize) -> bool {
        self.path == path && self.fingerprint == fingerprint && self.stream_index == stream_index
    }

    fn anchors(&self) -> usize {
        self.keyframe_pts.len()
    }
}

/// Aggregate residency policy for one worker-family seek-index cache.
///
/// All limits are enforced simultaneously. A zero limit disables retention
/// without affecting indexes already cloned into active decoder Sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewSeekIndexCachePolicy {
    /// Maximum number of distinct physical media-stream entries.
    pub max_entries: usize,
    /// Maximum total keyframe anchors retained by the cache.
    pub max_total_anchors: usize,
    /// Maximum approximate heap bytes retained by cache entries.
    pub max_retained_bytes: usize,
}

impl PreviewSeekIndexCachePolicy {
    /// Construct an explicit aggregate cache policy.
    pub const fn new(
        max_entries: usize,
        max_total_anchors: usize,
        max_retained_bytes: usize,
    ) -> Self {
        Self { max_entries, max_total_anchors, max_retained_bytes }
    }
}

impl Default for PreviewSeekIndexCachePolicy {
    fn default() -> Self {
        Self {
            max_entries: PREVIEW_SEEK_INDEX_CACHE_CAPACITY,
            max_total_anchors: 1_048_576,
            max_retained_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Point-in-time evidence for one worker-family seek-index cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PreviewSeekIndexCacheDiagnostics {
    /// Effective aggregate residency policy.
    pub policy: PreviewSeekIndexCachePolicy,
    /// Current policy revision; changes only when the effective policy changes.
    pub policy_revision: u64,
    /// Number of retained physical media-stream entries.
    pub entries: usize,
    /// Total keyframe anchors retained by those entries.
    pub retained_anchors: usize,
    /// Approximate heap bytes retained by those entries.
    pub retained_bytes: usize,
    /// Successful cache lookups.
    pub hits: u64,
    /// Cache lookups without a reusable entry.
    pub misses: u64,
    /// Entries removed to satisfy a policy or explicit trim.
    pub evictions: u64,
    /// Entries rejected because one entry exceeded the aggregate policy.
    pub rejected_entries: u64,
}

#[derive(Debug)]
struct PreviewSeekIndexCacheState {
    policy: PreviewSeekIndexCachePolicy,
    policy_revision: u64,
    entries: VecDeque<PreviewSeekIndexCacheEntry>,
    retained_anchors: usize,
    retained_bytes: usize,
    hits: u64,
    misses: u64,
    evictions: u64,
    rejected_entries: u64,
}

impl PreviewSeekIndexCacheState {
    fn new(policy: PreviewSeekIndexCachePolicy) -> Self {
        Self {
            policy,
            policy_revision: 1,
            entries: VecDeque::new(),
            retained_anchors: 0,
            retained_bytes: 0,
            hits: 0,
            misses: 0,
            evictions: 0,
            rejected_entries: 0,
        }
    }

    fn remove(&mut self, position: usize, eviction: bool) -> Option<PreviewSeekIndexCacheEntry> {
        let entry = self.entries.remove(position)?;
        self.retained_anchors = self.retained_anchors.saturating_sub(entry.anchors());
        self.retained_bytes = self.retained_bytes.saturating_sub(entry.retained_bytes);
        if eviction {
            self.evictions = self.evictions.saturating_add(1);
        }
        Some(entry)
    }

    fn entry_fits(&self, entry: &PreviewSeekIndexCacheEntry) -> bool {
        self.policy.max_entries > 0
            && entry.anchors() <= self.policy.max_total_anchors
            && entry.retained_bytes <= self.policy.max_retained_bytes
    }

    fn over_budget(&self) -> bool {
        self.entries.len() > self.policy.max_entries
            || self.retained_anchors > self.policy.max_total_anchors
            || self.retained_bytes > self.policy.max_retained_bytes
    }

    fn trim_to_policy(&mut self) {
        while self.over_budget() {
            let Some(position) = self.entries.len().checked_sub(1) else {
                break;
            };
            let _ = self.remove(position, true);
        }
    }

    fn diagnostics(&self) -> PreviewSeekIndexCacheDiagnostics {
        PreviewSeekIndexCacheDiagnostics {
            policy: self.policy,
            policy_revision: self.policy_revision,
            entries: self.entries.len(),
            retained_anchors: self.retained_anchors,
            retained_bytes: self.retained_bytes,
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
            rejected_entries: self.rejected_entries,
        }
    }
}

/// Explicit, shareable owner of reusable Preview seek indexes.
///
/// A Preview, Thumbnail, or Export worker family may clone this handle across
/// its decode workers. Dropping the last handle releases every cached index;
/// active decoder Sessions remain independent because they own cloned index
/// data. No process-global cache participates in lookup.
#[derive(Clone)]
pub struct PreviewSeekIndexCache {
    state: Arc<Mutex<PreviewSeekIndexCacheState>>,
}

impl std::fmt::Debug for PreviewSeekIndexCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreviewSeekIndexCache")
            .field("diagnostics", &self.diagnostics())
            .finish()
    }
}

impl PreviewSeekIndexCache {
    /// Create an owner with one explicit aggregate policy.
    pub fn new(policy: PreviewSeekIndexCachePolicy) -> Self {
        Self {
            state: Arc::new(Mutex::new(PreviewSeekIndexCacheState::new(policy))),
        }
    }

    /// Apply a new policy online and immediately evict least-recent entries.
    pub fn reconfigure(&self, policy: PreviewSeekIndexCachePolicy) {
        let mut state = self.lock_state();
        if state.policy == policy {
            return;
        }
        state.policy = policy;
        state.policy_revision = state.policy_revision.saturating_add(1);
        state.trim_to_policy();
    }

    /// Remove every retained index without invalidating active Sessions.
    pub fn clear(&self) {
        let mut state = self.lock_state();
        while !state.entries.is_empty() {
            let _ = state.remove(0, true);
        }
    }

    /// Return current residency and cache-operation evidence.
    pub fn diagnostics(&self) -> PreviewSeekIndexCacheDiagnostics {
        self.lock_state().diagnostics()
    }

    pub(super) fn get(
        &self,
        path: &Path,
        fingerprint: MediaFileFingerprint,
        stream_index: usize,
    ) -> Option<PreviewSeekIndex> {
        if !fingerprint.authorizes_reuse() {
            return None;
        }
        let mut state = self.lock_state();
        let Some(position) = state
            .entries
            .iter()
            .position(|entry| entry.matches(path, fingerprint, stream_index))
        else {
            state.misses = state.misses.saturating_add(1);
            return None;
        };
        let entry = state.remove(position, false)?;
        let seek_index = PreviewSeekIndex::from_probe_keyframes(entry.keyframe_pts.clone());
        state.retained_anchors = state.retained_anchors.saturating_add(entry.anchors());
        state.retained_bytes = state.retained_bytes.saturating_add(entry.retained_bytes);
        state.entries.push_front(entry);
        state.hits = state.hits.saturating_add(1);
        Some(seek_index)
    }

    pub(super) fn put(
        &self,
        path: &Path,
        fingerprint: MediaFileFingerprint,
        stream_index: usize,
        keyframe_pts: &[i64],
    ) {
        if keyframe_pts.is_empty() || !fingerprint.authorizes_reuse() {
            return;
        }
        let entry =
            PreviewSeekIndexCacheEntry::new(path, fingerprint, stream_index, keyframe_pts.to_vec());
        let mut state = self.lock_state();
        if !state.entry_fits(&entry) {
            state.rejected_entries = state.rejected_entries.saturating_add(1);
            return;
        }
        if let Some(position) = state
            .entries
            .iter()
            .position(|existing| existing.matches(path, fingerprint, stream_index))
        {
            let _ = state.remove(position, false);
        }
        state.retained_anchors = state.retained_anchors.saturating_add(entry.anchors());
        state.retained_bytes = state.retained_bytes.saturating_add(entry.retained_bytes);
        state.entries.push_front(entry);
        state.trim_to_policy();
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, PreviewSeekIndexCacheState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                tracing::warn!(
                    "Preview seek-index cache lock was poisoned; retaining bounded state"
                );
                poisoned.into_inner()
            }
        }
    }
}

impl Default for PreviewSeekIndexCache {
    fn default() -> Self {
        Self::new(PreviewSeekIndexCachePolicy::default())
    }
}

pub(super) fn preview_seek_index_contract_from_stream(
    stream: &ffmpeg::format::stream::Stream<'_>,
) -> (PreviewSeekIndex, bool) {
    let stream_ptr = unsafe { stream.as_ptr() };
    if stream_ptr.is_null() {
        return (PreviewSeekIndex::default(), false);
    }
    let entry_count = unsafe { ffmpeg::ffi::avformat_index_get_entries_count(stream_ptr) };
    if entry_count <= 0 {
        return (PreviewSeekIndex::default(), false);
    }

    let mut valid_count = 0_usize;
    for entry_index in 0..entry_count {
        if keyframe_entry_pts(stream_ptr, entry_index).is_some() {
            valid_count = valid_count.saturating_add(1);
        }
    }
    if valid_count == 0 {
        return (PreviewSeekIndex::default(), false);
    }

    // Large container indexes are sampled deterministically across their full
    // duration. This bounds both in-process cache residency and IPC while
    // retaining useful anchors near the head and tail of the source.
    let stride = valid_count.div_ceil(MAX_KEYFRAME_ANCHORS).max(1);
    let mut keyframe_pts = Vec::with_capacity(valid_count.min(MAX_KEYFRAME_ANCHORS));
    let mut valid_ordinal = 0_usize;
    let mut last_pts = None;
    for entry_index in 0..entry_count {
        let Some(pts) = keyframe_entry_pts(stream_ptr, entry_index) else {
            continue;
        };
        last_pts = Some(pts);
        if valid_ordinal.is_multiple_of(stride) && keyframe_pts.len() < MAX_KEYFRAME_ANCHORS {
            keyframe_pts.push(pts);
        }
        valid_ordinal = valid_ordinal.saturating_add(1);
    }
    if let Some(last_pts) = last_pts
        && keyframe_pts.last().copied() != Some(last_pts)
    {
        if keyframe_pts.len() == MAX_KEYFRAME_ANCHORS {
            if let Some(last) = keyframe_pts.last_mut() {
                *last = last_pts;
            }
        } else {
            keyframe_pts.push(last_pts);
        }
    }
    let truncated = valid_count > keyframe_pts.len();
    (
        PreviewSeekIndex::from_probe_keyframes(keyframe_pts),
        truncated,
    )
}

fn keyframe_entry_pts(stream_ptr: *const ffmpeg::ffi::AVStream, entry_index: i32) -> Option<i64> {
    // SAFETY: the stream belongs to the live input context and `entry_index`
    // is within the count reported by FFmpeg for this same stream.
    let entry_ptr =
        unsafe { ffmpeg::ffi::avformat_index_get_entry(stream_ptr.cast_mut(), entry_index) };
    if entry_ptr.is_null() {
        return None;
    }
    // SAFETY: FFmpeg returned a live entry owned by the stream.
    let entry = unsafe { &*entry_ptr };
    if entry.timestamp == ffmpeg::ffi::AV_NOPTS_VALUE
        || entry.flags() & ffmpeg::ffi::AVINDEX_KEYFRAME == 0
    {
        None
    } else {
        Some(entry.timestamp)
    }
}
