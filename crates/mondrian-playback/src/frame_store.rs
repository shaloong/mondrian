//! Bounded CPU Preview Frame Store with payload-opaque admission.
//!
//! This Module owns byte/count budgets, LRU eviction, terminal-failure memory,
//! the current-media oversize exception, and the current/stale Viewer pin.
//! Adapters provide opaque keys, payloads, presentation scopes, and exact byte
//! reservations; the Playback Module never interprets media or renderer types.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;

const MIB: usize = 1024 * 1024;

/// Product policy for CPU-resident preview payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewFrameStoreConfig {
    /// Maximum decoded-media entries independent of byte size.
    pub media_entry_capacity: usize,
    /// Maximum reserved CPU bytes for decoded-media payloads.
    pub media_byte_budget: usize,
    /// Maximum opaque decoder/GPU resource units retained by media payloads.
    pub media_resource_unit_budget: usize,
    /// Maximum final Viewer raster entries independent of byte size.
    pub viewer_entry_capacity: usize,
    /// Maximum reserved CPU bytes for final Viewer rasters.
    pub viewer_byte_budget: usize,
    /// Maximum remembered terminal failures.
    pub failure_entry_capacity: usize,
}

impl Default for PreviewFrameStoreConfig {
    fn default() -> Self {
        Self {
            media_entry_capacity: 96,
            media_byte_budget: 384 * MIB,
            media_resource_unit_budget: 4,
            viewer_entry_capacity: 48,
            viewer_byte_budget: 192 * MIB,
            failure_entry_capacity: 192,
        }
    }
}

/// Result of admitting one payload into bounded residency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameStoreAdmission {
    /// The payload is resident in its ordinary LRU cache.
    Resident,
    /// An oversize current-media payload is retained in the single explicit pin.
    PinnedCurrent,
    /// The payload exceeded its byte budget and policy did not permit pinning.
    RejectedOversize,
}

impl FrameStoreAdmission {
    /// Return whether the payload entered ordinary evictable residency.
    pub const fn is_resident(self) -> bool {
        matches!(self, Self::Resident)
    }
}

/// Point-in-time residency plus cumulative admission evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviewFrameStoreDiagnostics {
    /// Current decoded-media cache entry count.
    pub media_entries: usize,
    /// Reserved CPU bytes for decoded-media entries.
    pub media_reserved_bytes: usize,
    /// Maximum decoded-media CPU bytes.
    pub media_byte_budget: usize,
    /// Opaque decoder/GPU resource units currently retained by media entries.
    pub media_resource_units: usize,
    /// Maximum opaque decoder/GPU resource units retained by media entries.
    pub media_resource_unit_budget: usize,
    /// Decoded-media entries evicted by count or byte pressure.
    pub media_evictions: u64,
    /// Decoded-media payloads too large for ordinary admission.
    pub media_oversize_rejections: u64,
    /// Current final Viewer raster entry count.
    pub viewer_entries: usize,
    /// Reserved CPU bytes for final Viewer rasters.
    pub viewer_reserved_bytes: usize,
    /// Maximum final Viewer raster bytes.
    pub viewer_byte_budget: usize,
    /// Viewer rasters evicted by count or byte pressure.
    pub viewer_evictions: u64,
    /// Viewer rasters too large for admission.
    pub viewer_oversize_rejections: u64,
    /// Bytes retained by the non-evictable current/stale Viewer pin.
    pub pinned_viewer_bytes: usize,
    /// Bytes retained by the oversize current-media pin.
    pub pinned_media_bytes: usize,
    /// Current remembered terminal-failure key count.
    pub failure_entries: usize,
    /// Failure keys evicted by count pressure.
    pub failure_evictions: u64,
}

/// Deep Module owning Preview Frame Store residency and failure invariants.
pub struct PreviewFrameStore<MK, M, VK, V, S> {
    media: WeightedLruCache<MK, M>,
    viewer: WeightedLruCache<VK, V>,
    failures: BoundedLruSet<MK>,
    pinned_media: Option<PinnedMedia<MK, M>>,
    pinned_viewer: Option<PinnedViewer<S, V>>,
}

impl<MK, M, VK, V, S> Default for PreviewFrameStore<MK, M, VK, V, S>
where
    MK: Clone + Eq + Hash,
    M: Clone,
    VK: Clone + Eq + Hash,
    V: Clone,
    S: Eq,
{
    fn default() -> Self {
        Self::new(PreviewFrameStoreConfig::default())
    }
}

impl<MK, M, VK, V, S> PreviewFrameStore<MK, M, VK, V, S>
where
    MK: Clone + Eq + Hash,
    M: Clone,
    VK: Clone + Eq + Hash,
    V: Clone,
    S: Eq,
{
    /// Create a store with explicit budgets.
    ///
    /// Zero capacities and byte budgets are normalized to one so the Interface
    /// remains total while diagnostics expose the effective policy.
    pub fn new(config: PreviewFrameStoreConfig) -> Self {
        Self {
            media: WeightedLruCache::new(
                config.media_entry_capacity,
                config.media_byte_budget,
                config.media_resource_unit_budget,
            ),
            viewer: WeightedLruCache::new(
                config.viewer_entry_capacity,
                config.viewer_byte_budget,
                usize::MAX,
            ),
            failures: BoundedLruSet::new(config.failure_entry_capacity),
            pinned_media: None,
            pinned_viewer: None,
        }
    }

    /// Return and touch one decoded-media payload, including the current pin.
    pub fn media_frame(&mut self, key: &MK) -> Option<M> {
        self.media.get(key).or_else(|| {
            self.pinned_media
                .as_ref()
                .filter(|pinned| &pinned.key == key)
                .map(|pinned| pinned.payload.clone())
        })
    }

    /// Admit a decoded-media payload under its exact byte reservation.
    pub fn admit_media_frame(
        &mut self,
        key: MK,
        payload: M,
        reserved_bytes: usize,
        resource_units: usize,
        pin_if_oversize_current: bool,
    ) -> FrameStoreAdmission {
        let pinned = pin_if_oversize_current.then(|| PinnedMedia {
            key: key.clone(),
            payload: payload.clone(),
            reserved_bytes,
        });
        if self.media.insert(key, payload, reserved_bytes, resource_units) {
            if pin_if_oversize_current {
                self.pinned_media = None;
            }
            FrameStoreAdmission::Resident
        } else if let Some(pinned) = pinned {
            self.pinned_media = Some(pinned);
            FrameStoreAdmission::PinnedCurrent
        } else {
            FrameStoreAdmission::RejectedOversize
        }
    }

    /// Return and touch one final Viewer payload.
    pub fn viewer_frame(&mut self, key: &VK) -> Option<V> {
        self.viewer.get(key)
    }

    /// Admit one final Viewer payload under its exact byte reservation.
    pub fn admit_viewer_frame(
        &mut self,
        key: VK,
        payload: V,
        reserved_bytes: usize,
    ) -> FrameStoreAdmission {
        if self.viewer.insert(key, payload, reserved_bytes, 0) {
            FrameStoreAdmission::Resident
        } else {
            FrameStoreAdmission::RejectedOversize
        }
    }

    /// Remember one terminal media failure.
    pub fn remember_failure(&mut self, key: MK) {
        self.failures.insert(key);
    }

    /// Remove failure memory after a successful completion.
    pub fn forget_failure(&mut self, key: &MK) {
        self.failures.remove(key);
    }

    /// Return whether a key has a remembered failure and touch its recency.
    pub fn contains_failure(&mut self, key: &MK) -> bool {
        self.failures.contains(key)
    }

    /// Replace the non-evictable current/stale Viewer payload and its scope.
    pub fn pin_viewer_frame(&mut self, scope: S, payload: V, reserved_bytes: usize) {
        self.pinned_viewer = Some(PinnedViewer { scope, payload, reserved_bytes });
    }

    /// Return the pinned Viewer payload only for an exactly matching scope.
    pub fn stale_viewer_frame(&self, scope: &S) -> Option<V> {
        self.pinned_viewer
            .as_ref()
            .filter(|pinned| &pinned.scope == scope)
            .map(|pinned| pinned.payload.clone())
    }

    /// Release the non-evictable current/stale Viewer pin.
    pub fn clear_pinned_viewer_frame(&mut self) {
        self.pinned_viewer = None;
    }

    /// Release the oversize current-media pin.
    pub fn clear_pinned_media_frame(&mut self) {
        self.pinned_media = None;
    }

    /// Clear final Viewer residency and its current/stale pin.
    pub fn clear_viewer_frames(&mut self) {
        self.viewer.clear();
        self.pinned_viewer = None;
    }

    /// Release decoder-backed media residency while preserving final Viewer
    /// output, terminal-failure memory, and the exact stale-presentation pin.
    ///
    /// Preview coordinators use this after transport work is quiescent and a
    /// final Viewer output is independently usable. Decoder workers release
    /// their own session references at their separate idle boundary; retained
    /// Store frames must not keep that hardware surface pool alive afterward.
    pub fn clear_media_frames(&mut self) {
        self.media.clear();
        self.pinned_media = None;
    }

    /// Clear every payload, failure key, and explicit pin.
    pub fn clear_all(&mut self) {
        self.media.clear();
        self.viewer.clear();
        self.failures.clear();
        self.pinned_media = None;
        self.pinned_viewer = None;
    }

    /// Return residency and cumulative admission evidence.
    pub fn diagnostics(&self) -> PreviewFrameStoreDiagnostics {
        PreviewFrameStoreDiagnostics {
            media_entries: self.media.len(),
            media_reserved_bytes: self.media.reserved_bytes,
            media_byte_budget: self.media.byte_budget,
            media_resource_units: self.media.resource_units,
            media_resource_unit_budget: self.media.resource_unit_budget,
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
                .map(|pinned| pinned.reserved_bytes)
                .unwrap_or(0),
            pinned_media_bytes: self
                .pinned_media
                .as_ref()
                .map(|pinned| pinned.reserved_bytes)
                .unwrap_or(0),
            failure_entries: self.failures.len(),
            failure_evictions: self.failures.evictions,
        }
    }
}

struct PinnedMedia<K, V> {
    key: K,
    payload: V,
    reserved_bytes: usize,
}

struct PinnedViewer<S, V> {
    scope: S,
    payload: V,
    reserved_bytes: usize,
}

struct WeightedEntry<V> {
    payload: V,
    reserved_bytes: usize,
    resource_units: usize,
}

struct WeightedLruCache<K, V> {
    entry_capacity: usize,
    byte_budget: usize,
    reserved_bytes: usize,
    resource_unit_budget: usize,
    resource_units: usize,
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
    fn new(entry_capacity: usize, byte_budget: usize, resource_unit_budget: usize) -> Self {
        Self {
            entry_capacity: entry_capacity.max(1),
            byte_budget: byte_budget.max(1),
            reserved_bytes: 0,
            resource_unit_budget: resource_unit_budget.max(1),
            resource_units: 0,
            entries: HashMap::new(),
            lru: VecDeque::new(),
            evictions: 0,
            oversize_rejections: 0,
        }
    }

    fn get(&mut self, key: &K) -> Option<V> {
        let payload = self.entries.get(key)?.payload.clone();
        self.touch(key);
        Some(payload)
    }

    fn insert(&mut self, key: K, payload: V, reserved_bytes: usize, resource_units: usize) -> bool {
        self.remove(&key);
        if reserved_bytes > self.byte_budget || resource_units > self.resource_unit_budget {
            self.oversize_rejections = self.oversize_rejections.saturating_add(1);
            return false;
        }
        self.reserved_bytes = self.reserved_bytes.saturating_add(reserved_bytes);
        self.resource_units = self.resource_units.saturating_add(resource_units);
        self.entries.insert(
            key.clone(),
            WeightedEntry { payload, reserved_bytes, resource_units },
        );
        self.touch(&key);
        self.enforce_budget();
        self.entries.contains_key(&key)
    }

    fn remove(&mut self, key: &K) {
        if let Some(entry) = self.entries.remove(key) {
            self.reserved_bytes = self.reserved_bytes.saturating_sub(entry.reserved_bytes);
            self.resource_units = self.resource_units.saturating_sub(entry.resource_units);
        }
        self.lru.retain(|candidate| candidate != key);
    }

    fn enforce_budget(&mut self) {
        while self.entries.len() > self.entry_capacity
            || self.reserved_bytes > self.byte_budget
            || self.resource_units > self.resource_unit_budget
        {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            let Some(entry) = self.entries.remove(&oldest) else {
                continue;
            };
            self.reserved_bytes = self.reserved_bytes.saturating_sub(entry.reserved_bytes);
            self.resource_units = self.resource_units.saturating_sub(entry.resource_units);
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
        self.resource_units = 0;
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

    type TestStore = PreviewFrameStore<u64, Vec<u8>, u64, Vec<u8>, (u64, u32, u32)>;

    fn config(media_bytes: usize) -> PreviewFrameStoreConfig {
        PreviewFrameStoreConfig {
            media_entry_capacity: 100,
            media_byte_budget: media_bytes,
            media_resource_unit_budget: 4,
            viewer_entry_capacity: 4,
            viewer_byte_budget: 32,
            failure_entry_capacity: 2,
        }
    }

    #[test]
    fn enforces_byte_budget_and_recency_across_one_hundred_regions() {
        let mut store = TestStore::new(config(24));
        for region in 0..100u64 {
            assert_eq!(
                store.admit_media_frame(region, vec![region as u8; 8], 8, 0, false),
                FrameStoreAdmission::Resident
            );
            assert!(store.diagnostics().media_reserved_bytes <= 24);
        }
        assert_eq!(store.diagnostics().media_entries, 3);
        assert_eq!(store.diagnostics().media_evictions, 97);
        assert!(store.media_frame(&97).is_some());
        assert!(store.media_frame(&0).is_none());
    }

    #[test]
    fn oversize_current_media_is_explicitly_pinned_without_evicting_residents() {
        let mut store = TestStore::new(config(16));
        assert_eq!(
            store.admit_media_frame(1, vec![1; 8], 8, 0, false),
            FrameStoreAdmission::Resident
        );
        assert_eq!(
            store.admit_media_frame(2, vec![2; 32], 32, 0, true),
            FrameStoreAdmission::PinnedCurrent
        );
        assert_eq!(store.diagnostics().media_entries, 1);
        assert_eq!(store.diagnostics().pinned_media_bytes, 32);
        assert_eq!(store.media_frame(&2), Some(vec![2; 32]));
        assert_eq!(store.media_frame(&1), Some(vec![1; 8]));
    }

    #[test]
    fn decoder_resource_budget_evicts_oldest_native_payload_before_pool_exhaustion() {
        let mut config = config(64);
        config.media_resource_unit_budget = 2;
        let mut store = TestStore::new(config);

        assert_eq!(
            store.admit_media_frame(1, vec![1], 0, 1, false),
            FrameStoreAdmission::Resident
        );
        assert_eq!(
            store.admit_media_frame(2, vec![2], 0, 1, false),
            FrameStoreAdmission::Resident
        );
        assert_eq!(
            store.admit_media_frame(3, vec![3], 0, 1, false),
            FrameStoreAdmission::Resident
        );

        let diagnostics = store.diagnostics();
        assert_eq!(diagnostics.media_entries, 2);
        assert_eq!(diagnostics.media_resource_units, 2);
        assert_eq!(diagnostics.media_resource_unit_budget, 2);
        assert_eq!(diagnostics.media_evictions, 1);
        assert!(store.media_frame(&1).is_none());
        assert_eq!(store.media_frame(&2), Some(vec![2]));
        assert_eq!(store.media_frame(&3), Some(vec![3]));
    }

    #[test]
    fn stale_viewer_pin_requires_exact_scope() {
        let mut store = TestStore::new(config(16));
        store.pin_viewer_frame((7, 1920, 1080), vec![9; 4], 4);
        assert_eq!(store.stale_viewer_frame(&(7, 1920, 1080)), Some(vec![9; 4]));
        assert_eq!(store.stale_viewer_frame(&(8, 1920, 1080)), None);
        assert_eq!(store.stale_viewer_frame(&(7, 1280, 720)), None);
    }

    #[test]
    fn clear_releases_payloads_failures_and_both_pins() {
        let mut store = TestStore::new(config(16));
        store.admit_media_frame(1, vec![1; 8], 8, 0, false);
        store.admit_media_frame(2, vec![2; 32], 32, 0, true);
        store.admit_viewer_frame(1, vec![3; 8], 8);
        store.pin_viewer_frame((7, 1, 1), vec![4; 4], 4);
        store.remember_failure(3);

        store.clear_all();

        assert_eq!(
            store.diagnostics(),
            PreviewFrameStoreDiagnostics {
                media_byte_budget: 16,
                media_resource_unit_budget: 4,
                viewer_byte_budget: 32,
                media_oversize_rejections: 1,
                ..PreviewFrameStoreDiagnostics::default()
            }
        );
    }

    #[test]
    fn clear_media_residency_preserves_viewer_output_and_failure_memory() {
        let mut store = TestStore::new(config(16));
        store.admit_media_frame(1, vec![1; 8], 8, 1, false);
        store.admit_media_frame(2, vec![2; 32], 32, 0, true);
        store.admit_viewer_frame(1, vec![3; 8], 8);
        store.pin_viewer_frame((7, 1, 1), vec![4; 4], 4);
        store.remember_failure(3);

        store.clear_media_frames();

        let diagnostics = store.diagnostics();
        assert_eq!(diagnostics.media_entries, 0);
        assert_eq!(diagnostics.media_reserved_bytes, 0);
        assert_eq!(diagnostics.media_resource_units, 0);
        assert_eq!(diagnostics.pinned_media_bytes, 0);
        assert_eq!(diagnostics.viewer_entries, 1);
        assert_eq!(diagnostics.viewer_reserved_bytes, 8);
        assert_eq!(diagnostics.pinned_viewer_bytes, 4);
        assert_eq!(diagnostics.failure_entries, 1);
        assert_eq!(store.viewer_frame(&1), Some(vec![3; 8]));
        assert_eq!(store.stale_viewer_frame(&(7, 1, 1)), Some(vec![4; 4]));
        assert!(store.contains_failure(&3));
    }
}
