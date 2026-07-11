//! CPU-resident preview frame storage with explicit memory budgets.
//!
//! Decode scheduling and Viewer rendering use this Module through one small
//! Interface. Entry counts remain a safety cap, while payload-byte budgets are
//! the authoritative cache admission and eviction policy.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;

use mondrian_core::types::SequenceId;
use mondrian_ui_widgets::ViewerFrameImage;

use super::preview::{MediaPreviewFrame, ScopedViewerFrame, ViewerPreviewCacheKey};
use super::preview_access_mode::MediaPreviewKey;

const MIB: usize = 1024 * 1024;

/// Product policy for CPU-resident preview payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewCpuFrameStoreConfig {
    /// Maximum decoded/media cache entries independent of byte size.
    pub media_entry_capacity: usize,
    /// Maximum reserved CPU bytes for decoded/media payloads.
    pub media_byte_budget: usize,
    /// Maximum final Viewer raster cache entries independent of byte size.
    pub viewer_entry_capacity: usize,
    /// Maximum encoded CPU bytes for final Viewer raster payloads.
    pub viewer_byte_budget: usize,
    /// Maximum remembered terminal failures.
    pub failure_entry_capacity: usize,
}

impl Default for PreviewCpuFrameStoreConfig {
    fn default() -> Self {
        Self {
            media_entry_capacity: 96,
            media_byte_budget: 384 * MIB,
            viewer_entry_capacity: 48,
            viewer_byte_budget: 192 * MIB,
            failure_entry_capacity: 192,
        }
    }
}

/// Point-in-time cache budgets and cumulative admission evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PreviewCpuFrameStoreDiagnostics {
    /// Current decoded/media cache entry count.
    pub media_entries: usize,
    /// Reserved CPU pixel bytes for decoded/media entries.
    pub media_reserved_bytes: usize,
    /// Maximum decoded/media CPU pixel bytes.
    pub media_byte_budget: usize,
    /// Decoded/media entries evicted by count or byte pressure.
    pub media_evictions: u64,
    /// Decoded/media payloads too large for admission.
    pub media_oversize_rejections: u64,
    /// Current final Viewer raster entry count.
    pub viewer_entries: usize,
    /// Reserved encoded bytes for final Viewer rasters.
    pub viewer_reserved_bytes: usize,
    /// Maximum encoded bytes for final Viewer rasters.
    pub viewer_byte_budget: usize,
    /// Viewer rasters evicted by count or byte pressure.
    pub viewer_evictions: u64,
    /// Viewer raster payloads too large for admission.
    pub viewer_oversize_rejections: u64,
    /// Encoded bytes retained by the current/stale Viewer pin.
    pub pinned_viewer_bytes: usize,
    /// CPU pixel bytes retained by an oversize current-media pin.
    pub pinned_media_bytes: usize,
    /// Current remembered terminal-failure key count.
    pub failure_entries: usize,
    /// Failure keys evicted by count pressure.
    pub failure_evictions: u64,
}

/// Deep Module owning CPU preview caches, failure memory, and stale-frame pinning.
pub(crate) struct PreviewCpuFrameStore {
    media: WeightedLruCache<MediaPreviewKey, MediaPreviewFrame>,
    viewer: WeightedLruCache<ViewerPreviewCacheKey, ViewerFrameImage>,
    failures: BoundedLruSet<MediaPreviewKey>,
    pinned_media: Option<(MediaPreviewKey, MediaPreviewFrame, usize)>,
    pinned_viewer: Option<ScopedViewerFrame>,
}

impl Default for PreviewCpuFrameStore {
    fn default() -> Self {
        Self::new(PreviewCpuFrameStoreConfig::default())
    }
}

impl PreviewCpuFrameStore {
    /// Create a store with an explicit product/test budget.
    pub(crate) fn new(config: PreviewCpuFrameStoreConfig) -> Self {
        Self {
            media: WeightedLruCache::new(config.media_entry_capacity, config.media_byte_budget),
            viewer: WeightedLruCache::new(config.viewer_entry_capacity, config.viewer_byte_budget),
            failures: BoundedLruSet::new(config.failure_entry_capacity),
            pinned_media: None,
            pinned_viewer: None,
        }
    }

    /// Return and touch one decoded/media frame.
    pub(crate) fn media_frame(&mut self, key: &MediaPreviewKey) -> Option<MediaPreviewFrame> {
        self.media.get(key).or_else(|| {
            self.pinned_media
                .as_ref()
                .filter(|(candidate, _, _)| candidate == key)
                .map(|(_, frame, _)| frame.clone())
        })
    }

    /// Admit a decoded/media frame, pinning an oversize current frame only.
    pub(crate) fn insert_media_frame(
        &mut self,
        key: MediaPreviewKey,
        frame: MediaPreviewFrame,
        pin_if_oversize: bool,
    ) -> bool {
        let reserved_bytes = frame.reserved_cpu_bytes();
        let oversize_pin = pin_if_oversize.then(|| (key.clone(), frame.clone(), reserved_bytes));
        let stored = self.media.insert(key, frame, reserved_bytes);
        if pin_if_oversize {
            self.pinned_media = if stored { None } else { oversize_pin };
        }
        stored
    }

    /// Return and touch one final Viewer raster.
    pub(crate) fn viewer_frame(&mut self, key: &ViewerPreviewCacheKey) -> Option<ViewerFrameImage> {
        self.viewer.get(key)
    }

    /// Admit one final Viewer raster when it fits the byte budget.
    pub(crate) fn insert_viewer_frame(
        &mut self,
        key: ViewerPreviewCacheKey,
        frame: ViewerFrameImage,
    ) -> bool {
        let reserved_bytes = frame.rgba.len();
        self.viewer.insert(key, frame, reserved_bytes)
    }

    /// Remember one terminal media failure.
    pub(crate) fn remember_failure(&mut self, key: MediaPreviewKey) {
        self.failures.insert(key);
    }

    /// Remove failure memory after a successful completion.
    pub(crate) fn forget_failure(&mut self, key: &MediaPreviewKey) {
        self.failures.remove(key);
    }

    /// Return whether the key has a remembered terminal failure and touch it.
    pub(crate) fn contains_failure(&mut self, key: &MediaPreviewKey) -> bool {
        self.failures.contains(key)
    }

    /// Pin the current raster so playback can present it as stale.
    pub(crate) fn pin_viewer_frame(&mut self, frame: ScopedViewerFrame) {
        self.pinned_viewer = Some(frame);
    }

    /// Return the pinned raster only when its presentation scope matches.
    pub(crate) fn stale_viewer_frame(
        &self,
        sequence_id: SequenceId,
        width: u32,
        height: u32,
    ) -> Option<ViewerFrameImage> {
        let frame = self.pinned_viewer.as_ref()?;
        (frame.sequence_id == sequence_id && frame.width == width && frame.height == height)
            .then(|| frame.frame.clone())
    }

    /// Release the non-evictable current/stale Viewer pin.
    pub(crate) fn clear_pinned_viewer_frame(&mut self) {
        self.pinned_viewer = None;
    }

    /// Release an oversize current-media pin after a newer current outcome.
    pub(crate) fn clear_pinned_media_frame(&mut self) {
        self.pinned_media = None;
    }

    /// Clear final Viewer rasters and their current/stale pin.
    pub(crate) fn clear_viewer_frames(&mut self) {
        self.viewer.clear();
        self.pinned_viewer = None;
    }

    /// Clear all cached payloads, failure memory, and the Viewer pin.
    pub(crate) fn clear_all(&mut self) {
        self.media.clear();
        self.viewer.clear();
        self.failures.clear();
        self.pinned_media = None;
        self.pinned_viewer = None;
    }

    /// Return point-in-time residency and cumulative admission evidence.
    pub(crate) fn diagnostics(&self) -> PreviewCpuFrameStoreDiagnostics {
        PreviewCpuFrameStoreDiagnostics {
            media_entries: self.media.len(),
            media_reserved_bytes: self.media.reserved_bytes,
            media_byte_budget: self.media.byte_budget,
            media_evictions: self.media.evictions,
            media_oversize_rejections: self.media.oversize_rejections,
            viewer_entries: self.viewer.len(),
            viewer_reserved_bytes: self.viewer.reserved_bytes,
            viewer_byte_budget: self.viewer.byte_budget,
            viewer_evictions: self.viewer.evictions,
            viewer_oversize_rejections: self.viewer.oversize_rejections,
            pinned_viewer_bytes: self
                .pinned_viewer
                .as_ref()
                .map(|frame| frame.frame.rgba.len())
                .unwrap_or(0),
            pinned_media_bytes: self
                .pinned_media
                .as_ref()
                .map(|(_, _, reserved_bytes)| *reserved_bytes)
                .unwrap_or(0),
            failure_entries: self.failures.len(),
            failure_evictions: self.failures.evictions,
        }
    }
}

struct WeightedEntry<V> {
    value: V,
    reserved_bytes: usize,
}

struct WeightedLruCache<K, V> {
    entry_capacity: usize,
    byte_budget: usize,
    reserved_bytes: usize,
    entries: HashMap<K, WeightedEntry<V>>,
    lru: VecDeque<K>,
    evictions: u64,
    oversize_rejections: u64,
}

impl<K, V> WeightedLruCache<K, V>
where
    K: Clone + Eq + Hash,
    V: Clone,
{
    fn new(entry_capacity: usize, byte_budget: usize) -> Self {
        Self {
            entry_capacity: entry_capacity.max(1),
            byte_budget: byte_budget.max(1),
            reserved_bytes: 0,
            entries: HashMap::new(),
            lru: VecDeque::new(),
            evictions: 0,
            oversize_rejections: 0,
        }
    }

    fn get(&mut self, key: &K) -> Option<V> {
        let value = self.entries.get(key)?.value.clone();
        self.touch(key);
        Some(value)
    }

    fn insert(&mut self, key: K, value: V, reserved_bytes: usize) -> bool {
        self.remove(&key);
        if reserved_bytes > self.byte_budget {
            self.oversize_rejections = self.oversize_rejections.saturating_add(1);
            return false;
        }
        self.reserved_bytes = self.reserved_bytes.saturating_add(reserved_bytes);
        self.entries.insert(key.clone(), WeightedEntry { value, reserved_bytes });
        self.touch(&key);
        self.enforce_budget();
        self.entries.contains_key(&key)
    }

    fn remove(&mut self, key: &K) {
        if let Some(entry) = self.entries.remove(key) {
            self.reserved_bytes = self.reserved_bytes.saturating_sub(entry.reserved_bytes);
        }
        self.lru.retain(|candidate| candidate != key);
    }

    fn enforce_budget(&mut self) {
        while self.entries.len() > self.entry_capacity || self.reserved_bytes > self.byte_budget {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            let Some(entry) = self.entries.remove(&oldest) else {
                continue;
            };
            self.reserved_bytes = self.reserved_bytes.saturating_sub(entry.reserved_bytes);
            self.evictions = self.evictions.saturating_add(1);
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.lru.clear();
        self.reserved_bytes = 0;
    }

    fn touch(&mut self, key: &K) {
        self.lru.retain(|candidate| candidate != key);
        self.lru.push_back(key.clone());
    }
}

struct BoundedLruSet<K> {
    capacity: usize,
    entries: HashSet<K>,
    lru: VecDeque<K>,
    evictions: u64,
}

impl<K> BoundedLruSet<K>
where
    K: Clone + Eq + Hash,
{
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: HashSet::new(),
            lru: VecDeque::new(),
            evictions: 0,
        }
    }

    fn contains(&mut self, key: &K) -> bool {
        let contains = self.entries.contains(key);
        if contains {
            self.touch(key);
        }
        contains
    }

    fn insert(&mut self, key: K) {
        self.entries.insert(key.clone());
        self.touch(&key);
        while self.entries.len() > self.capacity {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            if self.entries.remove(&oldest) {
                self.evictions = self.evictions.saturating_add(1);
            }
        }
    }

    fn remove(&mut self, key: &K) {
        self.entries.remove(key);
        self.lru.retain(|candidate| candidate != key);
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.lru.clear();
    }

    fn touch(&mut self, key: &K) {
        self.lru.retain(|candidate| candidate != key);
        self.lru.push_back(key.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weighted_lru_enforces_bytes_and_recency_across_one_hundred_regions() {
        let mut cache = WeightedLruCache::new(100, 24);
        for region in 0..100u64 {
            cache.insert(region, vec![region as u8; 8], 8);
            assert!(cache.reserved_bytes <= cache.byte_budget);
        }

        assert_eq!(cache.len(), 3);
        assert_eq!(cache.reserved_bytes, 24);
        assert_eq!(cache.evictions, 97);
        assert!(cache.get(&97).is_some());
        assert!(cache.get(&0).is_none());
    }

    #[test]
    fn oversize_payload_is_rejected_without_evicting_resident_frames() {
        let mut cache = WeightedLruCache::new(4, 16);
        assert!(cache.insert(1u64, vec![1u8; 8], 8));
        assert!(!cache.insert(2, vec![2u8; 32], 32));

        assert_eq!(cache.len(), 1);
        assert_eq!(cache.reserved_bytes, 8);
        assert_eq!(cache.oversize_rejections, 1);
        assert!(cache.get(&1).is_some());
    }

    #[test]
    fn replacement_and_clear_release_all_reserved_bytes() {
        let mut cache = WeightedLruCache::new(4, 32);
        assert!(cache.insert(1u64, vec![1u8; 8], 8));
        assert!(cache.insert(1, vec![1u8; 12], 12));
        assert_eq!(cache.reserved_bytes, 12);

        cache.clear();

        assert_eq!(cache.len(), 0);
        assert_eq!(cache.reserved_bytes, 0);
    }
}
