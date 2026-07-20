//! Probe-backed and session-observed keyframe index for Preview seeks.
//!
//! The index owns ordered keyframe evidence, seek-anchor lookup, and the
//! bounded cross-session probe cache. It never chooses an access policy;
//! callers supply the target and consume typed diagnostics.

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
}

static PREVIEW_SEEK_INDEX_CACHE: OnceLock<Mutex<VecDeque<PreviewSeekIndexCacheEntry>>> =
    OnceLock::new();

fn preview_seek_index_cache() -> &'static Mutex<VecDeque<PreviewSeekIndexCacheEntry>> {
    PREVIEW_SEEK_INDEX_CACHE.get_or_init(|| Mutex::new(VecDeque::new()))
}

pub(super) fn preview_seek_index_cache_get(
    path: &Path,
    fingerprint: MediaFileFingerprint,
    stream_index: usize,
) -> Option<PreviewSeekIndex> {
    let mut cache = preview_seek_index_cache().lock().ok()?;
    let position = cache.iter().position(|entry| {
        entry.path == path && entry.fingerprint == fingerprint && entry.stream_index == stream_index
    })?;
    let entry = cache.remove(position)?;
    let seek_index = PreviewSeekIndex::from_probe_keyframes(entry.keyframe_pts.clone());
    cache.push_front(entry);
    Some(seek_index)
}

pub(super) fn preview_seek_index_cache_put(
    path: &Path,
    fingerprint: MediaFileFingerprint,
    stream_index: usize,
    keyframe_pts: &[i64],
) {
    if keyframe_pts.is_empty() {
        return;
    }
    let Ok(mut cache) = preview_seek_index_cache().lock() else {
        return;
    };
    if let Some(position) = cache.iter().position(|entry| {
        entry.path == path && entry.fingerprint == fingerprint && entry.stream_index == stream_index
    }) {
        cache.remove(position);
    }
    cache.push_front(PreviewSeekIndexCacheEntry {
        path: path.to_path_buf(),
        fingerprint,
        stream_index,
        keyframe_pts: keyframe_pts.to_vec(),
    });
    while cache.len() > PREVIEW_SEEK_INDEX_CACHE_CAPACITY {
        cache.pop_back();
    }
}

pub(super) fn preview_seek_index_from_stream(
    stream: &ffmpeg::format::stream::Stream<'_>,
) -> PreviewSeekIndex {
    let stream_ptr = unsafe { stream.as_ptr() };
    if stream_ptr.is_null() {
        return PreviewSeekIndex::default();
    }
    let entry_count = unsafe { ffmpeg::ffi::avformat_index_get_entries_count(stream_ptr) };
    if entry_count <= 0 {
        return PreviewSeekIndex::default();
    }

    let mut keyframe_pts = Vec::with_capacity(entry_count as usize);
    for entry_index in 0..entry_count {
        let entry_ptr =
            unsafe { ffmpeg::ffi::avformat_index_get_entry(stream_ptr.cast_mut(), entry_index) };
        if entry_ptr.is_null() {
            continue;
        }
        let entry = unsafe { &*entry_ptr };
        if entry.timestamp == ffmpeg::ffi::AV_NOPTS_VALUE {
            continue;
        }
        if entry.flags() & ffmpeg::ffi::AVINDEX_KEYFRAME == 0 {
            continue;
        }
        keyframe_pts.push(entry.timestamp);
    }
    PreviewSeekIndex::from_probe_keyframes(keyframe_pts)
}
