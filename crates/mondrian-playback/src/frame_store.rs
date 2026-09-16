//! Bounded CPU Preview Frame Store with payload-opaque admission.
//!
//! This Module owns byte/count budgets, LRU eviction, terminal-failure memory,
//! bounded current working-set overflow, and the current/stale Viewer pin.
//! Adapters provide opaque keys, payloads, presentation scopes, and exact byte
//! reservations; the Playback Module never interprets media or renderer types.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::hash::Hash;
use std::sync::{Arc, Mutex, MutexGuard};

use thiserror::Error;

use crate::{FrameDemandIdentity, PlaybackEpoch};

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
    /// Hard entry grant for one exact current Viewer working set.
    ///
    /// Unlike the optional cache budget, this grant is not reduced merely to
    /// create speculative headroom. It bounds multi-input current correctness.
    pub current_media_working_set_entry_limit: usize,
    /// Hard CPU-byte grant for one exact current Viewer working set.
    pub current_media_working_set_byte_limit: usize,
    /// Hard decoder/GPU-unit grant for one exact current Viewer working set.
    pub current_media_working_set_resource_unit_limit: usize,
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
            media_byte_budget: 640 * MIB,
            media_resource_unit_budget: 4,
            current_media_working_set_entry_limit: 16,
            current_media_working_set_byte_limit: 1024 * MIB,
            current_media_working_set_resource_unit_limit: 8,
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
    /// The payload resides in the bounded current working-set overflow.
    CurrentWorkingSetResidency,
    /// The payload exceeded the applicable physical working-set grant.
    RejectedCapacity,
}

impl FrameStoreAdmission {
    /// Return whether the payload entered ordinary evictable residency.
    pub const fn is_resident(self) -> bool {
        matches!(self, Self::Resident)
    }

    /// Return whether the payload remains available from the Store.
    pub const fn is_admitted(self) -> bool {
        matches!(self, Self::Resident | Self::CurrentWorkingSetResidency)
    }
}

/// Admission result for one physical decoded-media execution attempt.
#[derive(Debug)]
pub enum MediaWorkReservationAdmission {
    /// A new move-only physical work lease was admitted.
    Reserved(MediaWorkResourceLease),
    /// The requested payload is already resident and needs no new work.
    AlreadyResident,
    /// This exact current demand exceeds its machine-class working-set grant.
    ///
    /// Retrying the same demand cannot make this request admissible. Callers
    /// must surface a typed resource blocker rather than wait for unrelated
    /// aggregate capacity to change.
    RejectedCurrentDemandGrant,
    /// Other physical owners temporarily leave no aggregate admission capacity.
    ///
    /// Current work may reclaim or preempt speculative ownership before
    /// retrying. Prefetch work simply remains unadmitted.
    RejectedAggregateCapacity,
}

/// Atomic admission result for one complete speculative dependency closure.
#[derive(Debug)]
pub enum MediaPrefetchBatchReservationAdmission {
    /// Every requested attempt received one independently owned physical lease.
    Reserved(Vec<MediaWorkResourceLease>),
    /// At least one key became resident or the batch repeated a key.
    ///
    /// The caller must rebuild the dependency closure instead of reserving a
    /// stale subset.
    RetryPlan,
    /// The complete dependency closure does not fit the optional grant.
    RejectedAggregateCapacity,
}

/// Complete admission intent for one physical decoded-media execution attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaWorkReservationIntent {
    /// Visible work attributed to one exact current Viewer working set.
    Current(MediaWorkDemandId),
    /// Speculative work admitted only within optional cache policy.
    Prefetch,
}

impl MediaWorkReservationIntent {
    const fn class(self) -> MediaWorkReservationClass {
        match self {
            Self::Current(_) => MediaWorkReservationClass::Current,
            Self::Prefetch => MediaWorkReservationClass::Prefetch,
        }
    }

    const fn demand_id(self) -> Option<MediaWorkDemandId> {
        match self {
            Self::Current(demand_id) => Some(demand_id),
            Self::Prefetch => None,
        }
    }
}

/// Visibility priority attached to one decoded-media work reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaWorkReservationClass {
    /// Exact current-frame work; may request queued Prefetch preemption.
    Current,
    /// Speculative work; never displaces another outstanding reservation.
    Prefetch,
}

/// Process-local identity of one exact current Viewer working set.
///
/// Every media input in one resolved current-frame closure uses the same
/// identity. This lets the Store grant a bounded multi-key correctness set
/// without turning the optional cache budget into an unbounded exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaWorkDemandId {
    /// Realtime current set bound to complete Playback demand authority.
    Playback(FrameDemandIdentity),
    /// Paused/interactive set bound to one Preview generation and target frame.
    PreviewGeneration {
        /// Playback Session in which the generation was evaluated.
        epoch: PlaybackEpoch,
        /// Preview generation rotated by the exact Viewer plan binding.
        generation: u64,
        /// Exact timeline frame evaluated by that generation.
        target_frame: i64,
    },
}

impl MediaWorkDemandId {
    /// Bind a realtime current set to complete Playback demand authority.
    pub const fn for_playback(identity: FrameDemandIdentity) -> Self {
        Self::Playback(identity)
    }

    /// Bind a paused/interactive current set to one exact Preview generation.
    pub const fn for_preview_generation(
        epoch: PlaybackEpoch,
        generation: u64,
        target_frame: i64,
    ) -> Self {
        Self::PreviewGeneration { epoch, generation, target_frame }
    }
}

/// Move-only charge for one queued/in-flight/completed decode attempt.
///
/// Dropping the Broker payload, worker job, result, or rejected admission
/// releases this exact physical obligation. Semantic-key reconciliation is not
/// involved, so two attempts for the same key remain two independent charges.
pub struct MediaWorkResourceLease {
    allocation: Arc<MediaResourceAllocation>,
}

impl fmt::Debug for MediaWorkResourceLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MediaWorkResourceLease")
            .field("allocation_id", &self.allocation.id)
            .field("class", &self.allocation.class)
            .finish()
    }
}

/// Cloneable charge for one decoded allocation.
///
/// The Store, returned media frame, async visual payload, and GPU candidate all
/// clone this same allocation identity. The ledger charge disappears only
/// after the last physical owner drops.
#[derive(Clone)]
pub struct MediaFrameResourceLease {
    allocation: Arc<MediaResourceAllocation>,
}

impl fmt::Debug for MediaFrameResourceLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MediaFrameResourceLease")
            .field("allocation_id", &self.allocation.id)
            .field("class", &self.allocation.class)
            .finish()
    }
}

impl MediaFrameResourceLease {
    /// Return whether the Store is the only remaining owner of this allocation.
    fn store_is_exclusive_owner(&self) -> bool {
        Arc::strong_count(&self.allocation) == 1
    }

    /// Protect this allocation for one exact current Viewer demand.
    pub fn try_protect(
        &self,
        demand_id: MediaWorkDemandId,
    ) -> Result<MediaFrameProtectionLease, MediaFrameProtectionError> {
        let admitted = lock_media_ledger_from_allocation(&self.allocation)
            .try_add_protection(self.allocation.id, demand_id);
        if !admitted {
            return Err(MediaFrameProtectionError::CurrentWorkingSetCapacity);
        }
        Ok(MediaFrameProtectionLease {
            _guard: Arc::new(MediaFrameProtectionGuard { frame: self.clone(), demand_id }),
        })
    }
}

/// Structured refusal to extend one demand's protected media closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MediaFrameProtectionError {
    /// The unique allocation would exceed that demand's configured working set.
    #[error("current Viewer demand exceeds its media working-set grant")]
    CurrentWorkingSetCapacity,
}

/// Cloneable demand-scoped current-consumer guard.
#[derive(Clone)]
pub struct MediaFrameProtectionLease {
    _guard: Arc<MediaFrameProtectionGuard>,
}

impl fmt::Debug for MediaFrameProtectionLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MediaFrameProtectionLease")
            .field("allocation_id", &self._guard.frame.allocation.id)
            .field("demand_id", &self._guard.demand_id)
            .finish()
    }
}

/// Exact read-only speculative headroom after Store-exclusive LRU release.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MediaPrefetchHeadroom {
    /// Additional speculative allocation entries.
    pub entries: usize,
    /// Additional speculative CPU bytes.
    pub bytes: usize,
    /// Additional speculative decoder/GPU units.
    pub resource_units: usize,
}

impl MediaPrefetchHeadroom {
    /// Reserve one prospective distinct key in a low-frequency planning pass.
    pub fn try_reserve(&mut self, bytes: usize, resource_units: usize) -> bool {
        self.try_reserve_many(1, bytes, resource_units)
    }

    /// Reserve an aggregate prospective charge in a low-frequency planning pass.
    pub fn try_reserve_many(
        &mut self,
        entries: usize,
        bytes: usize,
        resource_units: usize,
    ) -> bool {
        if entries > self.entries || bytes > self.bytes || resource_units > self.resource_units {
            return false;
        }
        self.entries -= entries;
        self.bytes -= bytes;
        self.resource_units -= resource_units;
        true
    }
}

struct MediaFrameProtectionGuard {
    frame: MediaFrameResourceLease,
    demand_id: MediaWorkDemandId,
}

impl Drop for MediaFrameProtectionGuard {
    fn drop(&mut self) {
        lock_media_ledger_from_allocation(&self.frame.allocation)
            .remove_protection(self.frame.allocation.id, self.demand_id);
    }
}

/// Point-in-time residency plus cumulative admission evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviewFrameStoreDiagnostics {
    /// Current decoded-media cache entry count.
    pub media_entries: usize,
    /// Maximum optional decoded-media cache entry count.
    pub media_entry_capacity: usize,
    /// Reserved CPU bytes for decoded-media entries.
    pub media_reserved_bytes: usize,
    /// Maximum optional decoded-media cache CPU bytes.
    pub media_byte_budget: usize,
    /// Opaque decoder/GPU resource units currently retained by media entries.
    pub media_resource_units: usize,
    /// Maximum opaque decoder/GPU units in the optional media cache.
    pub media_resource_unit_budget: usize,
    /// Per-demand entry grant for one exact current Viewer working set.
    pub current_media_working_set_entry_limit: usize,
    /// Per-demand byte grant for one exact current Viewer working set.
    pub current_media_working_set_byte_limit: usize,
    /// Per-demand resource-unit grant for one exact current Viewer working set.
    pub current_media_working_set_resource_unit_limit: usize,
    /// Global physical entry limit after combining optional and current grants.
    pub media_aggregate_hard_entry_limit: usize,
    /// Global physical byte limit after combining optional and current grants.
    pub media_aggregate_hard_byte_limit: usize,
    /// Global physical resource-unit limit after combining optional and current grants.
    pub media_aggregate_hard_resource_unit_limit: usize,
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
    /// Current working-set overflow allocation count.
    pub current_media_overflow_entries: usize,
    /// Bytes retained by current working-set overflow allocations.
    pub current_media_overflow_bytes: usize,
    /// Resource units retained by current working-set overflow allocations.
    pub current_media_overflow_resource_units: usize,
    /// Independently charged queued, in-flight, or unconsumed completion attempts.
    pub media_work_reservations: usize,
    /// Outstanding exact current-frame media work reservations.
    pub media_current_work_reservations: usize,
    /// Distinct typed current sets represented by live work or protection guards.
    pub media_active_current_working_sets: usize,
    /// Outstanding speculative media work reservations.
    pub media_prefetch_work_reservations: usize,
    /// CPU bytes reserved before outstanding media work starts.
    pub media_work_reserved_bytes: usize,
    /// Decoder/GPU resource units reserved before outstanding media work starts.
    pub media_work_resource_units: usize,
    /// Ordinary media entries protected by an active current-frame consumer.
    pub protected_media_entries: usize,
    /// CPU bytes retained by protected current-frame media entries.
    pub protected_media_bytes: usize,
    /// Decoder/GPU units retained by protected current-frame media entries.
    pub protected_media_resource_units: usize,
    /// Aggregate resident, externally retained, and outstanding-work entries.
    pub media_aggregate_entries: usize,
    /// Aggregate resident, externally retained, and outstanding-work bytes.
    pub media_aggregate_reserved_bytes: usize,
    /// Aggregate resident, externally retained, and outstanding-work units.
    pub media_aggregate_resource_units: usize,
    /// Highest aggregate media entry charge observed by this Store.
    pub media_aggregate_entry_high_water: usize,
    /// Highest aggregate media byte charge observed by this Store.
    pub media_aggregate_byte_high_water: usize,
    /// Highest aggregate media resource-unit charge observed by this Store.
    pub media_aggregate_resource_unit_high_water: usize,
    /// Highest entry charge observed for one typed current working set.
    pub media_current_working_set_entry_high_water: usize,
    /// Highest byte charge observed for one typed current working set.
    pub media_current_working_set_byte_high_water: usize,
    /// Highest resource-unit charge observed for one typed current working set.
    pub media_current_working_set_resource_unit_high_water: usize,
    /// Current policy is below already admitted non-evictable obligations.
    pub media_capacity_overcommitted: bool,
    /// One typed current demand remains above its configured working-set grant.
    pub media_current_working_set_overcommitted: bool,
    /// One exact current demand is using its hard multi-input working-set grant.
    pub media_current_working_set_grant_active: bool,
    /// Number of work reservations admitted through the current working-set grant.
    pub media_current_working_set_grant_admissions: u64,
    /// Number of transitions beyond the hard aggregate media grant.
    pub media_capacity_overcommit_events: u64,
    /// Number of transitions beyond one demand's working-set grant.
    pub media_current_working_set_overcommit_events: u64,
    /// Ordinary media entries evicted early to create work-reservation headroom.
    pub media_work_reservation_evictions: u64,
    /// Work reservations rejected because bounded capacity was unavailable.
    pub media_work_reservation_rejections: u64,
    /// Work/protection admissions rejected by one demand's working-set grant.
    pub media_current_working_set_rejections: u64,
    /// Current remembered terminal-failure key count.
    pub failure_entries: usize,
    /// Failure keys evicted by count pressure.
    pub failure_evictions: u64,
}

impl PreviewFrameStoreDiagnostics {
    /// Return whether optional media cache residency obeys its trim policy.
    pub const fn optional_media_cache_within_policy(self) -> bool {
        self.media_entries <= self.media_entry_capacity
            && self.media_reserved_bytes <= self.media_byte_budget
            && self.media_resource_units <= self.media_resource_unit_budget
    }

    /// Return whether current physical media ownership obeys the hard grant.
    pub const fn media_aggregate_within_hard_grant(self) -> bool {
        self.media_aggregate_entries <= self.media_aggregate_hard_entry_limit
            && self.media_aggregate_reserved_bytes <= self.media_aggregate_hard_byte_limit
            && self.media_aggregate_resource_units <= self.media_aggregate_hard_resource_unit_limit
            && !self.media_capacity_overcommitted
    }

    /// Return whether all observed physical media ownership obeyed the hard grant.
    pub const fn media_high_water_within_hard_grant(self) -> bool {
        self.media_aggregate_entry_high_water <= self.media_aggregate_hard_entry_limit
            && self.media_aggregate_byte_high_water <= self.media_aggregate_hard_byte_limit
            && self.media_aggregate_resource_unit_high_water
                <= self.media_aggregate_hard_resource_unit_limit
            && self.media_current_working_set_entry_high_water
                <= self.current_media_working_set_entry_limit
            && self.media_current_working_set_byte_high_water
                <= self.current_media_working_set_byte_limit
            && self.media_current_working_set_resource_unit_high_water
                <= self.current_media_working_set_resource_unit_limit
            && !self.media_current_working_set_overcommitted
            && self.media_current_working_set_overcommit_events == 0
            && self.media_capacity_overcommit_events == 0
    }

    /// Return whether final Viewer cache and stale continuity residency obey policy.
    pub const fn viewer_residency_within_policy(self) -> bool {
        self.viewer_reserved_bytes <= self.viewer_byte_budget
            && self.pinned_viewer_bytes <= self.viewer_byte_budget
    }

    /// Return whether no queued, in-flight, or unconsumed media work lease remains.
    pub const fn media_work_is_quiescent(self) -> bool {
        self.media_work_reservations == 0
    }

    /// Evaluate the complete fail-closed production residency contract.
    pub const fn production_residency_contract_holds(self) -> bool {
        self.optional_media_cache_within_policy()
            && self.media_aggregate_within_hard_grant()
            && self.media_high_water_within_hard_grant()
            && self.viewer_residency_within_policy()
            && self.media_work_is_quiescent()
    }

    /// Return all decoded-media and Viewer oversize admission rejections.
    pub const fn oversize_rejections(self) -> u64 {
        self.media_oversize_rejections.saturating_add(self.viewer_oversize_rejections)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct MediaResourceCharge {
    entries: usize,
    bytes: usize,
    resource_units: usize,
}

impl MediaResourceCharge {
    const fn one(bytes: usize, resource_units: usize) -> Self {
        Self { entries: 1, bytes, resource_units }
    }

    fn saturating_add(self, other: Self) -> Self {
        Self {
            entries: self.entries.saturating_add(other.entries),
            bytes: self.bytes.saturating_add(other.bytes),
            resource_units: self.resource_units.saturating_add(other.resource_units),
        }
    }

    fn saturating_sub(self, other: Self) -> Self {
        Self {
            entries: self.entries.saturating_sub(other.entries),
            bytes: self.bytes.saturating_sub(other.bytes),
            resource_units: self.resource_units.saturating_sub(other.resource_units),
        }
    }

    const fn fits(self, limit: Self) -> bool {
        self.entries <= limit.entries
            && self.bytes <= limit.bytes
            && self.resource_units <= limit.resource_units
    }
}

#[derive(Debug, Clone, Copy)]
struct MediaResourcePolicy {
    optional: MediaResourceCharge,
    aggregate_hard: MediaResourceCharge,
    current_per_demand: MediaResourceCharge,
}

impl MediaResourcePolicy {
    fn from_config(config: PreviewFrameStoreConfig) -> Self {
        let optional = MediaResourceCharge {
            entries: config.media_entry_capacity.max(1),
            bytes: config.media_byte_budget.max(1),
            resource_units: config.media_resource_unit_budget.max(1),
        };
        let current_per_demand = MediaResourceCharge {
            entries: config.current_media_working_set_entry_limit.max(1),
            bytes: config.current_media_working_set_byte_limit.max(1),
            resource_units: config.current_media_working_set_resource_unit_limit.max(1),
        };
        Self {
            optional,
            aggregate_hard: MediaResourceCharge {
                entries: optional.entries.max(current_per_demand.entries),
                bytes: optional.bytes.max(current_per_demand.bytes),
                resource_units: optional.resource_units.max(current_per_demand.resource_units),
            },
            current_per_demand,
        }
    }

    const fn limit_for(self, class: MediaWorkReservationClass) -> MediaResourceCharge {
        match class {
            MediaWorkReservationClass::Current => self.aggregate_hard,
            MediaWorkReservationClass::Prefetch => self.optional,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaResourcePhase {
    Work,
    Frame,
}

#[derive(Debug)]
struct MediaResourceRecord {
    charge: MediaResourceCharge,
    class: MediaWorkReservationClass,
    demand_id: Option<MediaWorkDemandId>,
    phase: MediaResourcePhase,
    used_current_working_set_grant: bool,
    protections: HashMap<MediaWorkDemandId, usize>,
}

#[derive(Debug)]
struct MediaResourceLedger {
    policy: MediaResourcePolicy,
    next_id: u64,
    records: HashMap<u64, MediaResourceRecord>,
    aggregate: MediaResourceCharge,
    high_water: MediaResourceCharge,
    current_demand_high_water: MediaResourceCharge,
    reservation_rejections: u64,
    current_working_set_rejections: u64,
    current_working_set_grant_admissions: u64,
    hard_overcommit_active: bool,
    hard_overcommit_events: u64,
    current_demand_overcommit_active: bool,
    current_demand_overcommit_events: u64,
}

impl MediaResourceLedger {
    fn new(config: PreviewFrameStoreConfig) -> Self {
        Self {
            policy: MediaResourcePolicy::from_config(config),
            next_id: 1,
            records: HashMap::new(),
            aggregate: MediaResourceCharge::default(),
            high_water: MediaResourceCharge::default(),
            current_demand_high_water: MediaResourceCharge::default(),
            reservation_rejections: 0,
            current_working_set_rejections: 0,
            current_working_set_grant_admissions: 0,
            hard_overcommit_active: false,
            hard_overcommit_events: 0,
            current_demand_overcommit_active: false,
            current_demand_overcommit_events: 0,
        }
    }

    fn reconfigure(&mut self, config: PreviewFrameStoreConfig) {
        self.policy = MediaResourcePolicy::from_config(config);
        // High-water marks are meaningful only within one policy epoch. The
        // Store trims every releasable allocation before this point, so start
        // the new epoch from the irreducible physical obligations that remain.
        self.high_water = self.aggregate;
        self.current_demand_high_water = self.current_demand_peak();
        self.observe();
    }

    fn request_alone_fits(
        &self,
        class: MediaWorkReservationClass,
        charge: MediaResourceCharge,
    ) -> bool {
        charge.fits(self.policy.limit_for(class))
            && (class != MediaWorkReservationClass::Current
                || charge.fits(self.policy.current_per_demand))
    }

    fn can_reserve(&self, class: MediaWorkReservationClass, charge: MediaResourceCharge) -> bool {
        self.aggregate.saturating_add(charge).fits(self.policy.limit_for(class))
    }

    fn can_reserve_optional(&self, charge: MediaResourceCharge) -> bool {
        self.aggregate.saturating_add(charge).fits(self.policy.optional)
    }

    fn current_demand_charge(
        &self,
        demand_id: MediaWorkDemandId,
        excluded_id: Option<u64>,
    ) -> MediaResourceCharge {
        self.records
            .iter()
            .filter(|(id, record)| {
                Some(**id) != excluded_id
                    && ((record.class == MediaWorkReservationClass::Current
                        && record.demand_id == Some(demand_id))
                        || record.protections.contains_key(&demand_id))
            })
            .fold(MediaResourceCharge::default(), |aggregate, (_, record)| {
                aggregate.saturating_add(record.charge)
            })
    }

    fn current_demand_can_add(
        &self,
        demand_id: MediaWorkDemandId,
        excluded_id: Option<u64>,
        charge: MediaResourceCharge,
    ) -> bool {
        self.current_demand_charge(demand_id, excluded_id)
            .saturating_add(charge)
            .fits(self.policy.current_per_demand)
    }

    fn reserve(
        &mut self,
        class: MediaWorkReservationClass,
        demand_id: Option<MediaWorkDemandId>,
        charge: MediaResourceCharge,
    ) -> Option<u64> {
        let current_demand_fits = match (class, demand_id) {
            (MediaWorkReservationClass::Current, Some(demand_id)) => {
                self.current_demand_can_add(demand_id, None, charge)
            }
            (MediaWorkReservationClass::Current, None) => false,
            (MediaWorkReservationClass::Prefetch, _) => true,
        };
        if !self.can_reserve(class, charge) || !current_demand_fits {
            return None;
        }
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1)?;
        if self.records.contains_key(&id) {
            return None;
        }
        let used_current_working_set_grant = class == MediaWorkReservationClass::Current
            && !self.aggregate.saturating_add(charge).fits(self.policy.optional);
        self.records.insert(
            id,
            MediaResourceRecord {
                charge,
                class,
                demand_id,
                phase: MediaResourcePhase::Work,
                used_current_working_set_grant,
                protections: HashMap::new(),
            },
        );
        self.recompute_aggregate();
        if used_current_working_set_grant {
            self.current_working_set_grant_admissions =
                self.current_working_set_grant_admissions.saturating_add(1);
        }
        self.observe();
        Some(id)
    }

    fn reserve_batch(
        &mut self,
        class: MediaWorkReservationClass,
        demand_id: Option<MediaWorkDemandId>,
        charges: &[MediaResourceCharge],
    ) -> Option<Vec<u64>> {
        let aggregate_charge = charges.iter().copied().fold(
            MediaResourceCharge::default(),
            MediaResourceCharge::saturating_add,
        );
        let current_demand_fits = match (class, demand_id) {
            (MediaWorkReservationClass::Current, Some(demand_id)) => {
                self.current_demand_can_add(demand_id, None, aggregate_charge)
            }
            (MediaWorkReservationClass::Current, None) => false,
            (MediaWorkReservationClass::Prefetch, _) => true,
        };
        if charges.is_empty()
            || !aggregate_charge.fits(self.policy.limit_for(class))
            || !self.can_reserve(class, aggregate_charge)
            || !current_demand_fits
        {
            return None;
        }
        let count = u64::try_from(charges.len()).ok()?;
        let next_id = self.next_id.checked_add(count)?;
        let ids = (self.next_id..next_id).collect::<Vec<_>>();
        if ids.iter().any(|id| self.records.contains_key(id)) {
            return None;
        }
        self.next_id = next_id;
        for (id, charge) in ids.iter().copied().zip(charges.iter().copied()) {
            let used_current_working_set_grant = class == MediaWorkReservationClass::Current
                && !self.aggregate.saturating_add(charge).fits(self.policy.optional);
            self.records.insert(
                id,
                MediaResourceRecord {
                    charge,
                    class,
                    demand_id,
                    phase: MediaResourcePhase::Work,
                    used_current_working_set_grant,
                    protections: HashMap::new(),
                },
            );
            if used_current_working_set_grant {
                self.current_working_set_grant_admissions =
                    self.current_working_set_grant_admissions.saturating_add(1);
            }
        }
        self.recompute_aggregate();
        self.observe();
        Some(ids)
    }

    fn can_commit(&self, id: u64, charge: MediaResourceCharge) -> bool {
        let Some(record) = self.records.get(&id) else {
            return false;
        };
        let aggregate_fits = self
            .aggregate_without(id)
            .saturating_add(charge)
            .fits(self.policy.limit_for(record.class));
        let current_demand_fits = match (record.class, record.demand_id) {
            (MediaWorkReservationClass::Current, Some(demand_id)) => {
                self.current_demand_can_add(demand_id, Some(id), charge)
            }
            (MediaWorkReservationClass::Current, None) => false,
            (MediaWorkReservationClass::Prefetch, _) => true,
        };
        aggregate_fits && current_demand_fits
    }

    fn try_add_protection(&mut self, id: u64, demand_id: MediaWorkDemandId) -> bool {
        let Some(record) = self.records.get(&id) else {
            return false;
        };
        let already_protected = record.protections.contains_key(&demand_id);
        let already_attributed = record.class == MediaWorkReservationClass::Current
            && record.demand_id == Some(demand_id);
        let added_charge = if already_protected || already_attributed {
            MediaResourceCharge::default()
        } else {
            record.charge
        };
        if !self
            .current_demand_charge(demand_id, None)
            .saturating_add(added_charge)
            .fits(self.policy.current_per_demand)
        {
            self.record_current_working_set_rejection();
            return false;
        }
        let Some(record) = self.records.get_mut(&id) else {
            return false;
        };
        let count = record.protections.entry(demand_id).or_default();
        *count = count.saturating_add(1);
        self.observe();
        true
    }

    fn remove_protection(&mut self, id: u64, demand_id: MediaWorkDemandId) {
        let Some(record) = self.records.get_mut(&id) else {
            return;
        };
        let Some(count) = record.protections.get_mut(&demand_id) else {
            return;
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
            record.protections.remove(&demand_id);
        }
        self.observe();
    }

    fn commit(&mut self, id: u64, charge: MediaResourceCharge) -> bool {
        if !self.can_commit(id, charge) {
            return false;
        }
        let uses_current_working_set_grant = self.records.get(&id).is_some_and(|record| {
            record.class == MediaWorkReservationClass::Current
                && !self.aggregate_without(id).saturating_add(charge).fits(self.policy.optional)
        });
        let Some(record) = self.records.get_mut(&id) else {
            return false;
        };
        if uses_current_working_set_grant && !record.used_current_working_set_grant {
            record.used_current_working_set_grant = true;
            self.current_working_set_grant_admissions =
                self.current_working_set_grant_admissions.saturating_add(1);
        }
        record.charge = charge;
        record.phase = MediaResourcePhase::Frame;
        self.recompute_aggregate();
        self.observe();
        true
    }

    fn remove(&mut self, id: u64) {
        if self.records.remove(&id).is_some() {
            self.recompute_aggregate();
            self.observe();
        }
    }

    fn record_rejection(&mut self) {
        self.reservation_rejections = self.reservation_rejections.saturating_add(1);
    }

    fn record_current_working_set_rejection(&mut self) {
        self.current_working_set_rejections = self.current_working_set_rejections.saturating_add(1);
    }

    fn observe(&mut self) {
        self.high_water.entries = self.high_water.entries.max(self.aggregate.entries);
        self.high_water.bytes = self.high_water.bytes.max(self.aggregate.bytes);
        self.high_water.resource_units =
            self.high_water.resource_units.max(self.aggregate.resource_units);
        let current_demand_peak = self.current_demand_peak();
        self.current_demand_high_water.entries =
            self.current_demand_high_water.entries.max(current_demand_peak.entries);
        self.current_demand_high_water.bytes =
            self.current_demand_high_water.bytes.max(current_demand_peak.bytes);
        self.current_demand_high_water.resource_units = self
            .current_demand_high_water
            .resource_units
            .max(current_demand_peak.resource_units);
        let overcommitted = !self.aggregate.fits(self.policy.aggregate_hard);
        if overcommitted && !self.hard_overcommit_active {
            self.hard_overcommit_events = self.hard_overcommit_events.saturating_add(1);
        }
        self.hard_overcommit_active = overcommitted;
        let current_demand_overcommitted =
            !current_demand_peak.fits(self.policy.current_per_demand);
        if current_demand_overcommitted && !self.current_demand_overcommit_active {
            self.current_demand_overcommit_events =
                self.current_demand_overcommit_events.saturating_add(1);
        }
        self.current_demand_overcommit_active = current_demand_overcommitted;
    }

    fn current_demand_peak(&self) -> MediaResourceCharge {
        let demands = self
            .records
            .values()
            .flat_map(|record| {
                record
                    .demand_id
                    .filter(|_| record.class == MediaWorkReservationClass::Current)
                    .into_iter()
                    .chain(record.protections.keys().copied())
            })
            .collect::<HashSet<_>>();
        demands
            .into_iter()
            .map(|demand_id| self.current_demand_charge(demand_id, None))
            .fold(MediaResourceCharge::default(), |peak, charge| {
                MediaResourceCharge {
                    entries: peak.entries.max(charge.entries),
                    bytes: peak.bytes.max(charge.bytes),
                    resource_units: peak.resource_units.max(charge.resource_units),
                }
            })
    }

    fn aggregate_without(&self, excluded_id: u64) -> MediaResourceCharge {
        self.records
            .iter()
            .filter(|(id, _)| **id != excluded_id)
            .fold(MediaResourceCharge::default(), |aggregate, (_, record)| {
                aggregate.saturating_add(record.charge)
            })
    }

    fn aggregate_excluding(&self, excluded_ids: &HashSet<u64>) -> MediaResourceCharge {
        self.records
            .iter()
            .filter(|(id, _)| !excluded_ids.contains(id))
            .fold(MediaResourceCharge::default(), |aggregate, (_, record)| {
                aggregate.saturating_add(record.charge)
            })
    }

    fn recompute_aggregate(&mut self) {
        self.aggregate =
            self.records.values().fold(MediaResourceCharge::default(), |aggregate, record| {
                aggregate.saturating_add(record.charge)
            });
    }

    fn work_charge(&self) -> (usize, usize, usize, usize, usize) {
        self.records
            .values()
            .filter(|record| record.phase == MediaResourcePhase::Work)
            .fold(
                (0usize, 0usize, 0usize, 0usize, 0usize),
                |aggregate, record| {
                    (
                        aggregate.0.saturating_add(1),
                        aggregate.1.saturating_add(usize::from(
                            record.class == MediaWorkReservationClass::Current,
                        )),
                        aggregate.2.saturating_add(usize::from(
                            record.class == MediaWorkReservationClass::Prefetch,
                        )),
                        aggregate.3.saturating_add(record.charge.bytes),
                        aggregate.4.saturating_add(record.charge.resource_units),
                    )
                },
            )
    }

    fn protected_charge(&self) -> MediaResourceCharge {
        self.records
            .values()
            .filter(|record| !record.protections.is_empty())
            .fold(MediaResourceCharge::default(), |aggregate, record| {
                aggregate.saturating_add(record.charge)
            })
    }

    fn current_grant_active(&self) -> bool {
        !self.aggregate.fits(self.policy.optional) && self.active_current_working_sets() > 0
    }

    fn active_current_working_sets(&self) -> usize {
        let mut demands = HashSet::new();
        for record in self.records.values() {
            if record.class == MediaWorkReservationClass::Current
                && record.phase == MediaResourcePhase::Work
                && let Some(demand_id) = record.demand_id
            {
                demands.insert(demand_id);
            }
            demands.extend(record.protections.keys().copied());
        }
        demands.len()
    }
}

struct MediaResourceAllocation {
    id: u64,
    class: MediaWorkReservationClass,
    ledger: Arc<Mutex<MediaResourceLedger>>,
}

impl Drop for MediaResourceAllocation {
    fn drop(&mut self) {
        lock_media_ledger(&self.ledger).remove(self.id);
    }
}

fn lock_media_ledger(
    ledger: &Arc<Mutex<MediaResourceLedger>>,
) -> MutexGuard<'_, MediaResourceLedger> {
    ledger.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock_media_ledger_from_allocation(
    allocation: &MediaResourceAllocation,
) -> MutexGuard<'_, MediaResourceLedger> {
    lock_media_ledger(&allocation.ledger)
}

impl MediaWorkResourceLease {
    fn belongs_to(&self, ledger: &Arc<Mutex<MediaResourceLedger>>) -> bool {
        Arc::ptr_eq(&self.allocation.ledger, ledger)
    }

    /// Physical admission class retained by this execution's resource owner.
    pub fn admission_class(&self) -> MediaWorkReservationClass {
        self.allocation.class
    }

    fn committed_charge_alone_fits(&self, reserved_bytes: usize, resource_units: usize) -> bool {
        let ledger = lock_media_ledger_from_allocation(&self.allocation);
        let Some(record) = ledger.records.get(&self.allocation.id) else {
            return false;
        };
        let charge = MediaResourceCharge::one(reserved_bytes, resource_units);
        charge.fits(ledger.policy.limit_for(record.class))
            && (record.class != MediaWorkReservationClass::Current
                || charge.fits(ledger.policy.current_per_demand))
    }

    fn current_demand_commit_fits(&self, reserved_bytes: usize, resource_units: usize) -> bool {
        let ledger = lock_media_ledger_from_allocation(&self.allocation);
        let Some(record) = ledger.records.get(&self.allocation.id) else {
            return false;
        };
        match (record.class, record.demand_id) {
            (MediaWorkReservationClass::Current, Some(demand_id)) => ledger.current_demand_can_add(
                demand_id,
                Some(self.allocation.id),
                MediaResourceCharge::one(reserved_bytes, resource_units),
            ),
            (MediaWorkReservationClass::Current, None) => false,
            (MediaWorkReservationClass::Prefetch, _) => true,
        }
    }

    fn can_commit(&self, reserved_bytes: usize, resource_units: usize) -> bool {
        lock_media_ledger_from_allocation(&self.allocation).can_commit(
            self.allocation.id,
            MediaResourceCharge::one(reserved_bytes, resource_units),
        )
    }

    fn record_current_working_set_rejection(&self) {
        lock_media_ledger_from_allocation(&self.allocation).record_current_working_set_rejection();
    }

    fn commit(
        self,
        reserved_bytes: usize,
        resource_units: usize,
    ) -> Result<MediaFrameResourceLease, Self> {
        let committed = lock_media_ledger_from_allocation(&self.allocation).commit(
            self.allocation.id,
            MediaResourceCharge::one(reserved_bytes, resource_units),
        );
        if !committed {
            return Err(self);
        }
        Ok(MediaFrameResourceLease { allocation: Arc::clone(&self.allocation) })
    }
}

/// Deep Module owning Preview Frame Store residency and failure invariants.
pub struct PreviewFrameStore<MK, M, VK, V, S> {
    media: WeightedLruCache<MK, M>,
    media_ledger: Arc<Mutex<MediaResourceLedger>>,
    viewer: WeightedLruCache<VK, V>,
    failures: BoundedLruSet<MK>,
    current_media_overflow: HashMap<MK, CurrentMediaOverflow<M>>,
    current_media_overflow_lru: VecDeque<MK>,
    pinned_viewer: Option<PinnedViewer<S, V>>,
    media_work_reservation_evictions: u64,
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
        let media_ledger = Arc::new(Mutex::new(MediaResourceLedger::new(config)));
        Self {
            media: WeightedLruCache::new(
                config.media_entry_capacity,
                config.media_byte_budget,
                config.media_resource_unit_budget,
            ),
            media_ledger,
            viewer: WeightedLruCache::new(
                config.viewer_entry_capacity,
                config.viewer_byte_budget,
                usize::MAX,
            ),
            failures: BoundedLruSet::new(config.failure_entry_capacity),
            current_media_overflow: HashMap::new(),
            current_media_overflow_lru: VecDeque::new(),
            pinned_viewer: None,
            media_work_reservation_evictions: 0,
        }
    }

    /// Replace residency budgets and immediately evict ordinary LRU entries
    /// until the new limits hold.
    ///
    /// Store-exclusive media is released before policy is allowed to report a
    /// hard overcommit. External frame owners and live work attempts remain
    /// charged until their physical leases are dropped.
    pub fn reconfigure(&mut self, config: PreviewFrameStoreConfig) {
        self.media.reconfigure(
            config.media_entry_capacity,
            config.media_byte_budget,
            config.media_resource_unit_budget,
        );
        let next_policy = MediaResourcePolicy::from_config(config);
        loop {
            let exceeds_prospective_policy = {
                let ledger = lock_media_ledger(&self.media_ledger);
                !ledger.aggregate.fits(next_policy.aggregate_hard)
                    || !ledger.current_demand_peak().fits(next_policy.current_per_demand)
            };
            if !exceeds_prospective_policy {
                break;
            }
            if !self.evict_one_releasable_media_for_work() {
                break;
            }
        }
        // Install and observe the new hard grant only after all immediately
        // releasable Store ownership has been retired. A transient trim step is
        // not a physical overcommit event.
        lock_media_ledger(&self.media_ledger).reconfigure(config);
        self.viewer.reconfigure(
            config.viewer_entry_capacity,
            config.viewer_byte_budget,
            usize::MAX,
        );
        self.failures.reconfigure(config.failure_entry_capacity);
    }

    /// Return and touch one decoded-media payload with its physical allocation.
    pub fn media_frame(&mut self, key: &MK) -> Option<(M, MediaFrameResourceLease)> {
        if let Some(result) = self.media.get_with_media_lease(key) {
            return Some(result);
        }
        let result = self
            .current_media_overflow
            .get(key)
            .map(|pinned| (pinned.payload.clone(), pinned.resource.clone()))?;
        self.touch_current_media_overflow(key);
        Some(result)
    }

    /// Return and protect one decoded-media payload for current-frame execution.
    pub fn protected_media_frame(
        &mut self,
        key: &MK,
        current_demand_id: MediaWorkDemandId,
    ) -> Result<
        Option<(M, MediaFrameResourceLease, MediaFrameProtectionLease)>,
        MediaFrameProtectionError,
    > {
        let Some((payload, resource)) = self.media_frame(key) else {
            return Ok(None);
        };
        let protection = resource.try_protect(current_demand_id)?;
        Ok(Some((payload, resource, protection)))
    }

    /// Reserve one independently owned physical decode-attempt charge.
    ///
    /// The returned move-only lease must enter the Broker payload. Queue
    /// replacement, eviction, reuse, cancellation, channel failure, and worker
    /// completion then release the exact attempt through ordinary ownership.
    pub fn reserve_media_work(
        &mut self,
        key: &MK,
        intent: MediaWorkReservationIntent,
        reserved_bytes: usize,
        resource_units: usize,
    ) -> MediaWorkReservationAdmission {
        if self.media.contains_key(key) || self.current_media_overflow.contains_key(key) {
            return MediaWorkReservationAdmission::AlreadyResident;
        }
        let class = intent.class();
        let demand_id = intent.demand_id();
        let charge = MediaResourceCharge::one(reserved_bytes, resource_units);
        {
            let mut ledger = lock_media_ledger(&self.media_ledger);
            let current_demand_rejected = class == MediaWorkReservationClass::Current
                && demand_id.is_some_and(|demand_id| {
                    !ledger.current_demand_can_add(demand_id, None, charge)
                });
            if current_demand_rejected {
                ledger.record_rejection();
                ledger.record_current_working_set_rejection();
                return MediaWorkReservationAdmission::RejectedCurrentDemandGrant;
            }
            if !ledger.request_alone_fits(class, charge) {
                ledger.record_rejection();
                return MediaWorkReservationAdmission::RejectedAggregateCapacity;
            }
        }

        while !lock_media_ledger(&self.media_ledger).can_reserve_optional(charge) {
            if !self.evict_one_releasable_media_for_work() {
                break;
            }
        }

        let id = {
            let mut ledger = lock_media_ledger(&self.media_ledger);
            if class == MediaWorkReservationClass::Current
                && demand_id.is_some_and(|demand_id| {
                    !ledger.current_demand_can_add(demand_id, None, charge)
                })
            {
                ledger.record_rejection();
                ledger.record_current_working_set_rejection();
                return MediaWorkReservationAdmission::RejectedCurrentDemandGrant;
            }
            let Some(id) = ledger.reserve(class, demand_id, charge) else {
                ledger.record_rejection();
                return MediaWorkReservationAdmission::RejectedAggregateCapacity;
            };
            id
        };
        MediaWorkReservationAdmission::Reserved(MediaWorkResourceLease {
            allocation: Arc::new(MediaResourceAllocation {
                id,
                class,
                ledger: Arc::clone(&self.media_ledger),
            }),
        })
    }

    /// Atomically reserve every attempt in one speculative media closure.
    ///
    /// No lease is published unless the complete batch fits after releasable
    /// Store-only LRU residency is retired. Each returned lease remains
    /// independently move-only so Broker cancellation and worker completion
    /// release the exact attempt through ordinary ownership.
    pub fn reserve_media_prefetch_work_batch(
        &mut self,
        requests: &[(&MK, usize, usize)],
    ) -> MediaPrefetchBatchReservationAdmission {
        if requests.is_empty() {
            return MediaPrefetchBatchReservationAdmission::RetryPlan;
        }
        let mut keys = HashSet::with_capacity(requests.len());
        if requests.iter().any(|(key, _, _)| {
            !keys.insert(*key)
                || self.media.contains_key(*key)
                || self.current_media_overflow.contains_key(*key)
        }) {
            return MediaPrefetchBatchReservationAdmission::RetryPlan;
        }
        let charges = requests
            .iter()
            .map(|(_, bytes, resource_units)| MediaResourceCharge::one(*bytes, *resource_units))
            .collect::<Vec<_>>();
        let aggregate_charge = charges.iter().copied().fold(
            MediaResourceCharge::default(),
            MediaResourceCharge::saturating_add,
        );
        {
            let mut ledger = lock_media_ledger(&self.media_ledger);
            if !aggregate_charge.fits(ledger.policy.optional) {
                ledger.record_rejection();
                return MediaPrefetchBatchReservationAdmission::RejectedAggregateCapacity;
            }
        }
        while !lock_media_ledger(&self.media_ledger).can_reserve_optional(aggregate_charge) {
            if !self.evict_one_releasable_media_for_work() {
                break;
            }
        }
        let ids = {
            let mut ledger = lock_media_ledger(&self.media_ledger);
            let Some(ids) =
                ledger.reserve_batch(MediaWorkReservationClass::Prefetch, None, &charges)
            else {
                ledger.record_rejection();
                return MediaPrefetchBatchReservationAdmission::RejectedAggregateCapacity;
            };
            ids
        };
        MediaPrefetchBatchReservationAdmission::Reserved(
            ids.into_iter()
                .map(|id| MediaWorkResourceLease {
                    allocation: Arc::new(MediaResourceAllocation {
                        id,
                        class: MediaWorkReservationClass::Prefetch,
                        ledger: Arc::clone(&self.media_ledger),
                    }),
                })
                .collect(),
        )
    }

    /// Commit one successful physical decode attempt into frame residency.
    pub fn admit_media_frame(
        &mut self,
        key: MK,
        payload: M,
        work: MediaWorkResourceLease,
        reserved_bytes: usize,
        resource_units: usize,
    ) -> FrameStoreAdmission {
        if !work.belongs_to(&self.media_ledger) {
            return FrameStoreAdmission::RejectedCapacity;
        }
        if self.media.contains_key(&key) {
            return FrameStoreAdmission::Resident;
        }
        if self.current_media_overflow.contains_key(&key) {
            return FrameStoreAdmission::CurrentWorkingSetResidency;
        }

        let admission_class = work.admission_class();
        if !work.current_demand_commit_fits(reserved_bytes, resource_units) {
            work.record_current_working_set_rejection();
            return FrameStoreAdmission::RejectedCapacity;
        }
        if !work.committed_charge_alone_fits(reserved_bytes, resource_units) {
            return FrameStoreAdmission::RejectedCapacity;
        }
        let work = loop {
            if work.can_commit(reserved_bytes, resource_units) {
                break work;
            }
            if !self.evict_one_releasable_media_for_work() {
                return FrameStoreAdmission::RejectedCapacity;
            }
        };
        let Ok(resource) = work.commit(reserved_bytes, resource_units) else {
            return FrameStoreAdmission::RejectedCapacity;
        };

        match self.media.insert_media(
            key.clone(),
            payload,
            resource,
            reserved_bytes,
            resource_units,
            admission_class == MediaWorkReservationClass::Prefetch,
        ) {
            Ok(()) => FrameStoreAdmission::Resident,
            Err((payload, resource)) if admission_class == MediaWorkReservationClass::Current => {
                self.current_media_overflow.insert(
                    key.clone(),
                    CurrentMediaOverflow { payload, resource, reserved_bytes, resource_units },
                );
                self.touch_current_media_overflow(&key);
                FrameStoreAdmission::CurrentWorkingSetResidency
            }
            Err((_payload, _resource)) => FrameStoreAdmission::RejectedCapacity,
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
            FrameStoreAdmission::RejectedCapacity
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

    /// Release Store-owned current working-set overflow residency.
    pub fn clear_current_media_overflow(&mut self) {
        self.current_media_overflow.clear();
        self.current_media_overflow_lru.clear();
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
        self.current_media_overflow.clear();
        self.current_media_overflow_lru.clear();
    }

    /// Release only media payloads that retain decoder-owned resources.
    ///
    /// CPU frames have zero resource units and remain reusable across
    /// transport-family changes. Native decoder frames carry nonzero units and
    /// must be removed before the owning worker destroys its codec context.
    pub fn clear_decoder_resource_media_frames(&mut self) {
        self.media.clear_resource_entries();
        self.current_media_overflow.retain(|_, overflow| overflow.resource_units == 0);
        self.current_media_overflow_lru
            .retain(|key| self.current_media_overflow.contains_key(key));
    }

    /// Clear every payload, failure key, and explicit pin.
    pub fn clear_all(&mut self) {
        self.clear_all_preserving_media_work();
    }

    /// Clear author-visible residency while preserving live execution charges.
    ///
    /// Work leases live in Broker/worker/result payloads, not in the Store.
    /// They therefore remain charged automatically while lifecycle-canceled
    /// workers unwind.
    pub fn clear_all_preserving_media_work(&mut self) {
        self.media.clear();
        self.viewer.clear();
        self.failures.clear();
        self.current_media_overflow.clear();
        self.current_media_overflow_lru.clear();
        self.pinned_viewer = None;
    }

    /// Return residency and cumulative admission evidence.
    pub fn diagnostics(&self) -> PreviewFrameStoreDiagnostics {
        let current_media_overflow_bytes =
            self.current_media_overflow.values().fold(0usize, |total, overflow| {
                total.saturating_add(overflow.reserved_bytes)
            });
        let current_media_overflow_resource_units =
            self.current_media_overflow.values().fold(0usize, |total, overflow| {
                total.saturating_add(overflow.resource_units)
            });
        let ledger = lock_media_ledger(&self.media_ledger);
        let (
            media_work_reservations,
            media_current_work_reservations,
            media_prefetch_work_reservations,
            media_work_reserved_bytes,
            media_work_resource_units,
        ) = ledger.work_charge();
        let media_active_current_working_sets = ledger.active_current_working_sets();
        let protected = ledger.protected_charge();
        PreviewFrameStoreDiagnostics {
            media_entries: self.media.len(),
            media_entry_capacity: self.media.entry_capacity,
            media_reserved_bytes: self.media.reserved_bytes,
            media_byte_budget: self.media.byte_budget,
            media_resource_units: self.media.resource_units,
            media_resource_unit_budget: self.media.resource_unit_budget,
            current_media_working_set_entry_limit: ledger.policy.current_per_demand.entries,
            current_media_working_set_byte_limit: ledger.policy.current_per_demand.bytes,
            current_media_working_set_resource_unit_limit: ledger
                .policy
                .current_per_demand
                .resource_units,
            media_aggregate_hard_entry_limit: ledger.policy.aggregate_hard.entries,
            media_aggregate_hard_byte_limit: ledger.policy.aggregate_hard.bytes,
            media_aggregate_hard_resource_unit_limit: ledger.policy.aggregate_hard.resource_units,
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
            current_media_overflow_entries: self.current_media_overflow.len(),
            current_media_overflow_bytes,
            current_media_overflow_resource_units,
            media_work_reservations,
            media_current_work_reservations,
            media_active_current_working_sets,
            media_prefetch_work_reservations,
            media_work_reserved_bytes,
            media_work_resource_units,
            protected_media_entries: protected.entries,
            protected_media_bytes: protected.bytes,
            protected_media_resource_units: protected.resource_units,
            media_aggregate_entries: ledger.aggregate.entries,
            media_aggregate_reserved_bytes: ledger.aggregate.bytes,
            media_aggregate_resource_units: ledger.aggregate.resource_units,
            media_aggregate_entry_high_water: ledger.high_water.entries,
            media_aggregate_byte_high_water: ledger.high_water.bytes,
            media_aggregate_resource_unit_high_water: ledger.high_water.resource_units,
            media_current_working_set_entry_high_water: ledger.current_demand_high_water.entries,
            media_current_working_set_byte_high_water: ledger.current_demand_high_water.bytes,
            media_current_working_set_resource_unit_high_water: ledger
                .current_demand_high_water
                .resource_units,
            media_capacity_overcommitted: ledger.hard_overcommit_active,
            media_current_working_set_overcommitted: ledger.current_demand_overcommit_active,
            media_current_working_set_grant_active: ledger.current_grant_active(),
            media_current_working_set_grant_admissions: ledger.current_working_set_grant_admissions,
            media_capacity_overcommit_events: ledger.hard_overcommit_events,
            media_current_working_set_overcommit_events: ledger.current_demand_overcommit_events,
            media_work_reservation_evictions: self.media_work_reservation_evictions,
            media_work_reservation_rejections: ledger.reservation_rejections,
            media_current_working_set_rejections: ledger.current_working_set_rejections,
            failure_entries: self.failures.len(),
            failure_evictions: self.failures.evictions,
        }
    }

    /// Project speculative headroom after every Store-exclusive media LRU can
    /// be physically released.
    pub fn media_prefetch_headroom(&self) -> MediaPrefetchHeadroom {
        let mut releasable = self.media.releasable_media_allocation_ids();
        releasable.extend(self.current_media_overflow.values().filter_map(|overflow| {
            overflow
                .resource
                .store_is_exclusive_owner()
                .then_some(overflow.resource.allocation.id)
        }));
        let ledger = lock_media_ledger(&self.media_ledger);
        let retained = ledger.aggregate_excluding(&releasable);
        MediaPrefetchHeadroom {
            entries: ledger.policy.optional.entries.saturating_sub(retained.entries),
            bytes: ledger.policy.optional.bytes.saturating_sub(retained.bytes),
            resource_units: ledger
                .policy
                .optional
                .resource_units
                .saturating_sub(retained.resource_units),
        }
    }

    fn evict_one_releasable_media_for_work(&mut self) -> bool {
        if self.media.evict_oldest_releasable() {
            self.media_work_reservation_evictions =
                self.media_work_reservation_evictions.saturating_add(1);
            return true;
        }
        let candidates = self.current_media_overflow_lru.len();
        for _ in 0..candidates {
            let Some(key) = self.current_media_overflow_lru.pop_front() else {
                return false;
            };
            let releasable = self
                .current_media_overflow
                .get(&key)
                .is_some_and(|overflow| overflow.resource.store_is_exclusive_owner());
            if !releasable {
                self.current_media_overflow_lru.push_back(key);
                continue;
            }
            self.current_media_overflow.remove(&key);
            self.media_work_reservation_evictions =
                self.media_work_reservation_evictions.saturating_add(1);
            return true;
        }
        false
    }

    fn touch_current_media_overflow(&mut self, key: &MK) {
        self.current_media_overflow_lru.retain(|candidate| candidate != key);
        self.current_media_overflow_lru.push_back(key.clone());
    }
}

struct CurrentMediaOverflow<V> {
    payload: V,
    resource: MediaFrameResourceLease,
    reserved_bytes: usize,
    resource_units: usize,
}

struct PinnedViewer<S, V> {
    scope: S,
    payload: V,
    reserved_bytes: usize,
}

struct WeightedEntry<V> {
    payload: V,
    media_resource: Option<MediaFrameResourceLease>,
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

    fn get_with_media_lease(&mut self, key: &K) -> Option<(V, MediaFrameResourceLease)> {
        let entry = self.entries.get(key)?;
        let result = (entry.payload.clone(), entry.media_resource.clone()?);
        self.touch(key);
        Some(result)
    }

    fn insert(&mut self, key: K, payload: V, reserved_bytes: usize, resource_units: usize) -> bool {
        self.insert_entry(key, payload, None, reserved_bytes, resource_units, true)
            .is_ok()
    }

    fn insert_media(
        &mut self,
        key: K,
        payload: V,
        resource: MediaFrameResourceLease,
        reserved_bytes: usize,
        resource_units: usize,
        record_oversize_rejection: bool,
    ) -> Result<(), (V, MediaFrameResourceLease)> {
        self.insert_entry(
            key,
            payload,
            Some(resource),
            reserved_bytes,
            resource_units,
            record_oversize_rejection,
        )
        .map_err(|(payload, resource)| {
            (
                payload,
                resource.expect("media insertion always carries a resource lease"),
            )
        })
    }

    fn insert_entry(
        &mut self,
        key: K,
        payload: V,
        media_resource: Option<MediaFrameResourceLease>,
        reserved_bytes: usize,
        resource_units: usize,
        record_oversize_rejection: bool,
    ) -> Result<(), (V, Option<MediaFrameResourceLease>)> {
        if reserved_bytes > self.byte_budget
            || resource_units > self.resource_unit_budget
            || self.entry_capacity == 0
        {
            if record_oversize_rejection {
                self.oversize_rejections = self.oversize_rejections.saturating_add(1);
            }
            return Err((payload, media_resource));
        }
        let Some(evictions) = self.plan_insert_evictions(&key, reserved_bytes, resource_units)
        else {
            return Err((payload, media_resource));
        };
        self.remove_entry(&key);
        for evicted in evictions {
            self.remove_entry(&evicted);
            self.evictions = self.evictions.saturating_add(1);
        }
        self.reserved_bytes = self.reserved_bytes.saturating_add(reserved_bytes);
        self.resource_units = self.resource_units.saturating_add(resource_units);
        self.entries.insert(
            key.clone(),
            WeightedEntry {
                payload,
                media_resource,
                reserved_bytes,
                resource_units,
            },
        );
        self.touch(&key);
        Ok(())
    }

    fn enforce_budget(&mut self) {
        while self.entries.len() > self.entry_capacity
            || self.reserved_bytes > self.byte_budget
            || self.resource_units > self.resource_unit_budget
        {
            if !self.evict_oldest_releasable() {
                break;
            }
        }
    }

    fn plan_insert_evictions(
        &self,
        replacement_key: &K,
        added_bytes: usize,
        added_resource_units: usize,
    ) -> Option<Vec<K>> {
        if self
            .entries
            .get(replacement_key)
            .is_some_and(|entry| !entry_is_releasable(entry))
        {
            return None;
        }
        let mut retained = self.entries.iter().filter(|(key, _)| *key != replacement_key).fold(
            MediaResourceCharge::default(),
            |aggregate, (_, entry)| {
                aggregate.saturating_add(MediaResourceCharge::one(
                    entry.reserved_bytes,
                    entry.resource_units,
                ))
            },
        );
        let added = MediaResourceCharge::one(added_bytes, added_resource_units);
        let limit = MediaResourceCharge {
            entries: self.entry_capacity,
            bytes: self.byte_budget,
            resource_units: self.resource_unit_budget,
        };
        if retained.saturating_add(added).fits(limit) {
            return Some(Vec::new());
        }
        let mut evictions = Vec::new();
        for key in &self.lru {
            if key == replacement_key {
                continue;
            }
            let Some(entry) = self.entries.get(key) else {
                continue;
            };
            if !entry_is_releasable(entry) {
                continue;
            }
            retained = retained.saturating_sub(MediaResourceCharge::one(
                entry.reserved_bytes,
                entry.resource_units,
            ));
            evictions.push(key.clone());
            if retained.saturating_add(added).fits(limit) {
                return Some(evictions);
            }
        }
        None
    }

    fn evict_oldest_releasable(&mut self) -> bool {
        let candidates = self.lru.len();
        for _ in 0..candidates {
            let Some(key) = self.lru.pop_front() else {
                return false;
            };
            if self.entries.get(&key).is_some_and(|entry| !entry_is_releasable(entry)) {
                self.lru.push_back(key);
            } else {
                self.remove_entry(&key);
                self.evictions = self.evictions.saturating_add(1);
                return true;
            }
        }
        false
    }

    fn reconfigure(
        &mut self,
        entry_capacity: usize,
        byte_budget: usize,
        resource_unit_budget: usize,
    ) {
        self.entry_capacity = entry_capacity.max(1);
        self.byte_budget = byte_budget.max(1);
        self.resource_unit_budget = resource_unit_budget.max(1);
        self.enforce_budget();
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn contains_key(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }

    fn releasable_media_allocation_ids(&self) -> HashSet<u64> {
        self.entries
            .values()
            .filter_map(|entry| {
                let resource = entry.media_resource.as_ref()?;
                resource.store_is_exclusive_owner().then_some(resource.allocation.id)
            })
            .collect()
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.lru.clear();
        self.reserved_bytes = 0;
        self.resource_units = 0;
    }

    fn clear_resource_entries(&mut self) {
        let keys = self
            .entries
            .iter()
            .filter_map(|(key, entry)| (entry.resource_units > 0).then_some(key.clone()))
            .collect::<Vec<_>>();
        for key in keys {
            self.remove_entry(&key);
        }
    }

    fn touch(&mut self, key: &K) {
        self.lru.retain(|candidate| candidate != key);
        self.lru.push_back(key.clone());
    }

    fn remove_entry(&mut self, key: &K) {
        if let Some(entry) = self.entries.remove(key) {
            self.reserved_bytes = self.reserved_bytes.saturating_sub(entry.reserved_bytes);
            self.resource_units = self.resource_units.saturating_sub(entry.resource_units);
        }
        self.lru.retain(|candidate| candidate != key);
    }
}

fn entry_is_releasable<V>(entry: &WeightedEntry<V>) -> bool {
    entry
        .media_resource
        .as_ref()
        .is_none_or(MediaFrameResourceLease::store_is_exclusive_owner)
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

    fn reconfigure(&mut self, capacity: usize) {
        self.capacity = capacity.max(1);
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
            media_entry_capacity: 4,
            media_byte_budget: media_bytes,
            media_resource_unit_budget: 4,
            current_media_working_set_entry_limit: 8,
            current_media_working_set_byte_limit: media_bytes.max(64),
            current_media_working_set_resource_unit_limit: 8,
            viewer_entry_capacity: 4,
            viewer_byte_budget: 32,
            failure_entry_capacity: 2,
        }
    }

    fn demand(sequence: u64) -> MediaWorkDemandId {
        MediaWorkDemandId::for_preview_generation(
            PlaybackEpoch(sequence),
            sequence,
            i64::try_from(sequence).unwrap_or(i64::MAX),
        )
    }

    fn reserve(
        store: &mut TestStore,
        key: u64,
        class: MediaWorkReservationClass,
        demand_sequence: u64,
        bytes: usize,
        resource_units: usize,
    ) -> MediaWorkResourceLease {
        let intent = match class {
            MediaWorkReservationClass::Current => {
                MediaWorkReservationIntent::Current(demand(demand_sequence))
            }
            MediaWorkReservationClass::Prefetch => MediaWorkReservationIntent::Prefetch,
        };
        match store.reserve_media_work(&key, intent, bytes, resource_units) {
            MediaWorkReservationAdmission::Reserved(lease) => lease,
            admission => panic!("expected physical work lease, received {admission:?}"),
        }
    }

    fn admit(
        store: &mut TestStore,
        key: u64,
        payload_byte: u8,
        class: MediaWorkReservationClass,
        demand_sequence: u64,
        bytes: usize,
        resource_units: usize,
    ) -> FrameStoreAdmission {
        let work = reserve(store, key, class, demand_sequence, bytes, resource_units);
        store.admit_media_frame(key, vec![payload_byte], work, bytes, resource_units)
    }

    fn payload(store: &mut TestStore, key: u64) -> Option<Vec<u8>> {
        store.media_frame(&key).map(|(payload, _resource)| payload)
    }

    #[test]
    fn enforces_byte_budget_and_recency_across_one_hundred_regions() {
        let mut store = TestStore::new(config(24));
        for region in 0..100u64 {
            assert_eq!(
                admit(
                    &mut store,
                    region,
                    region as u8,
                    MediaWorkReservationClass::Prefetch,
                    region,
                    8,
                    0,
                ),
                FrameStoreAdmission::Resident
            );
            assert!(store.diagnostics().media_reserved_bytes <= 24);
        }
        assert_eq!(store.diagnostics().media_entries, 3);
        assert_eq!(store.diagnostics().media_evictions, 97);
        assert!(payload(&mut store, 97).is_some());
        assert!(payload(&mut store, 0).is_none());
    }

    #[test]
    fn same_key_attempts_are_independently_charged_and_released() {
        let mut store = TestStore::new(config(32));
        let first = reserve(&mut store, 1, MediaWorkReservationClass::Prefetch, 1, 8, 1);
        let second = reserve(&mut store, 1, MediaWorkReservationClass::Prefetch, 1, 8, 1);
        let diagnostics = store.diagnostics();
        assert_eq!(diagnostics.media_work_reservations, 2);
        assert_eq!(diagnostics.media_aggregate_entries, 2);
        assert_eq!(diagnostics.media_aggregate_reserved_bytes, 16);
        assert_eq!(diagnostics.media_aggregate_resource_units, 2);

        drop(second);
        let diagnostics = store.diagnostics();
        assert_eq!(diagnostics.media_work_reservations, 1);
        assert_eq!(diagnostics.media_aggregate_reserved_bytes, 8);
        drop(first);
        assert_eq!(store.diagnostics().media_aggregate_entries, 0);
    }

    #[test]
    fn speculative_batch_reservation_is_all_or_none_across_resource_units() {
        let mut store = TestStore::new(config(32));
        let mut occupied = (1..=3)
            .map(|key| {
                reserve(
                    &mut store,
                    key,
                    MediaWorkReservationClass::Prefetch,
                    key,
                    1,
                    1,
                )
            })
            .collect::<Vec<_>>();
        let first_key = 10;
        let second_key = 11;
        assert!(matches!(
            store.reserve_media_prefetch_work_batch(&[(&first_key, 1, 1), (&second_key, 1, 1),]),
            MediaPrefetchBatchReservationAdmission::RejectedAggregateCapacity
        ));
        let rejected = store.diagnostics();
        assert_eq!(rejected.media_work_reservations, 3);
        assert_eq!(rejected.media_aggregate_resource_units, 3);

        drop(occupied.pop());
        let batch = match store
            .reserve_media_prefetch_work_batch(&[(&first_key, 1, 1), (&second_key, 1, 1)])
        {
            MediaPrefetchBatchReservationAdmission::Reserved(batch) => batch,
            admission => panic!("expected complete batch reservation, received {admission:?}"),
        };
        assert_eq!(batch.len(), 2);
        let admitted = store.diagnostics();
        assert_eq!(admitted.media_work_reservations, 4);
        assert_eq!(admitted.media_aggregate_resource_units, 4);

        drop(batch);
        drop(occupied);
        assert_eq!(store.diagnostics().media_aggregate_resource_units, 0);
    }

    #[test]
    fn media_resource_leases_are_send_sync_execution_payloads() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<MediaWorkResourceLease>();
        assert_send_sync::<MediaFrameResourceLease>();
        assert_send_sync::<MediaFrameProtectionLease>();
    }

    #[test]
    fn work_lease_cannot_cross_store_ledger_identity() {
        let mut source = TestStore::new(config(32));
        let mut destination = TestStore::new(config(32));
        let work = reserve(&mut source, 1, MediaWorkReservationClass::Prefetch, 1, 8, 0);

        assert_eq!(
            destination.admit_media_frame(1, vec![1], work, 8, 0),
            FrameStoreAdmission::RejectedCapacity
        );
        assert!(payload(&mut destination, 1).is_none());
        assert_eq!(source.diagnostics().media_aggregate_entries, 0);
        assert_eq!(destination.diagnostics().media_aggregate_entries, 0);
    }

    #[test]
    fn one_current_demand_cannot_exceed_its_exact_working_set_grant() {
        let mut policy = config(128);
        policy.media_entry_capacity = 8;
        policy.current_media_working_set_entry_limit = 2;
        policy.current_media_working_set_byte_limit = 128;
        let mut store = TestStore::new(policy);
        let first = reserve(&mut store, 1, MediaWorkReservationClass::Current, 7, 8, 0);
        let second = reserve(&mut store, 2, MediaWorkReservationClass::Current, 7, 8, 0);

        assert!(matches!(
            store.reserve_media_work(&3, MediaWorkReservationIntent::Current(demand(7)), 8, 0,),
            MediaWorkReservationAdmission::RejectedCurrentDemandGrant
        ));
        let diagnostics = store.diagnostics();
        assert_eq!(diagnostics.media_current_working_set_entry_high_water, 2);
        assert_eq!(diagnostics.media_current_working_set_rejections, 1);
        assert!(!diagnostics.media_capacity_overcommitted);
        drop(first);
        drop(second);
    }

    #[test]
    fn distinct_current_demands_remain_bounded_by_one_global_physical_grant() {
        let mut policy = config(128);
        policy.media_entry_capacity = 1;
        policy.current_media_working_set_entry_limit = 2;
        policy.current_media_working_set_byte_limit = 128;
        let mut store = TestStore::new(policy);
        let first = reserve(&mut store, 1, MediaWorkReservationClass::Current, 1, 8, 0);
        let second = reserve(&mut store, 2, MediaWorkReservationClass::Current, 2, 8, 0);

        assert!(matches!(
            store.reserve_media_work(&3, MediaWorkReservationIntent::Current(demand(3)), 8, 0,),
            MediaWorkReservationAdmission::RejectedAggregateCapacity
        ));
        let diagnostics = store.diagnostics();
        assert_eq!(diagnostics.media_aggregate_entries, 2);
        assert_eq!(diagnostics.media_aggregate_hard_entry_limit, 2);
        assert_eq!(diagnostics.media_work_reservation_rejections, 1);
        drop(first);
        drop(second);
    }

    #[test]
    fn successful_work_commit_becomes_one_frame_charge_until_last_owner_drops() {
        let mut store = TestStore::new(config(16));
        assert_eq!(
            admit(
                &mut store,
                1,
                1,
                MediaWorkReservationClass::Prefetch,
                1,
                8,
                1,
            ),
            FrameStoreAdmission::Resident
        );
        let diagnostics = store.diagnostics();
        assert_eq!(diagnostics.media_work_reservations, 0);
        assert_eq!(diagnostics.media_aggregate_entries, 1);
        assert_eq!(diagnostics.media_aggregate_reserved_bytes, 8);

        let (frame, external_lease) = store.media_frame(&1).expect("resident frame");
        assert_eq!(frame, vec![1]);
        store.clear_media_frames();
        assert!(payload(&mut store, 1).is_none());
        assert_eq!(store.diagnostics().media_aggregate_entries, 1);
        drop(external_lease);
        assert_eq!(store.diagnostics().media_aggregate_entries, 0);
    }

    #[test]
    fn current_working_set_can_exceed_optional_cache_but_not_hard_grant() {
        let mut policy = config(8);
        policy.media_entry_capacity = 1;
        policy.current_media_working_set_entry_limit = 2;
        policy.current_media_working_set_byte_limit = 32;
        let mut store = TestStore::new(policy);

        assert!(matches!(
            store.reserve_media_work(&1, MediaWorkReservationIntent::Prefetch, 16, 0),
            MediaWorkReservationAdmission::RejectedAggregateCapacity
        ));
        assert_eq!(
            admit(
                &mut store,
                1,
                1,
                MediaWorkReservationClass::Current,
                1,
                16,
                0,
            ),
            FrameStoreAdmission::CurrentWorkingSetResidency
        );
        let diagnostics = store.diagnostics();
        assert_eq!(diagnostics.media_entries, 0);
        assert_eq!(diagnostics.current_media_overflow_entries, 1);
        assert_eq!(diagnostics.current_media_overflow_bytes, 16);
        assert_eq!(diagnostics.media_aggregate_reserved_bytes, 16);
        assert!(
            diagnostics.media_aggregate_reserved_bytes
                <= diagnostics.current_media_working_set_byte_limit
        );
        assert!(!diagnostics.media_current_working_set_grant_active);
    }

    #[test]
    fn live_current_protection_activates_and_releases_hard_grant_evidence() {
        let mut policy = config(8);
        policy.current_media_working_set_byte_limit = 32;
        let mut store = TestStore::new(policy);
        assert_eq!(
            admit(
                &mut store,
                1,
                1,
                MediaWorkReservationClass::Current,
                1,
                16,
                0,
            ),
            FrameStoreAdmission::CurrentWorkingSetResidency
        );
        let (_payload, frame, protection) = store
            .protected_media_frame(&1, demand(9))
            .expect("protection admitted")
            .expect("protected frame");
        let active = store.diagnostics();
        assert!(active.media_current_working_set_grant_active);
        assert_eq!(active.media_active_current_working_sets, 1);
        drop(frame);
        drop(protection);
        let inactive = store.diagnostics();
        assert!(!inactive.media_current_working_set_grant_active);
        assert_eq!(inactive.media_active_current_working_sets, 0);
    }

    #[test]
    fn overflow_lru_reclaims_only_store_exclusive_allocations() {
        let mut policy = config(8);
        policy.current_media_working_set_byte_limit = 32;
        policy.current_media_working_set_entry_limit = 2;
        let mut store = TestStore::new(policy);
        assert_eq!(
            admit(
                &mut store,
                1,
                1,
                MediaWorkReservationClass::Current,
                1,
                16,
                0,
            ),
            FrameStoreAdmission::CurrentWorkingSetResidency
        );
        let (_payload, externally_owned) = store.media_frame(&1).expect("first overflow frame");
        assert_eq!(
            admit(
                &mut store,
                2,
                2,
                MediaWorkReservationClass::Current,
                2,
                16,
                0,
            ),
            FrameStoreAdmission::CurrentWorkingSetResidency
        );
        assert_eq!(
            admit(
                &mut store,
                3,
                3,
                MediaWorkReservationClass::Current,
                3,
                16,
                0,
            ),
            FrameStoreAdmission::CurrentWorkingSetResidency
        );
        let diagnostics = store.diagnostics();
        assert_eq!(diagnostics.current_media_overflow_entries, 2);
        assert_eq!(diagnostics.media_aggregate_reserved_bytes, 32);
        assert_eq!(payload(&mut store, 1), Some(vec![1]));
        assert_eq!(payload(&mut store, 2), None);
        assert_eq!(payload(&mut store, 3), Some(vec![3]));
        drop(externally_owned);
    }

    #[test]
    fn same_key_late_completion_preserves_existing_protected_frame() {
        let mut store = TestStore::new(config(24));
        let first = reserve(&mut store, 1, MediaWorkReservationClass::Prefetch, 1, 8, 0);
        let late = reserve(&mut store, 1, MediaWorkReservationClass::Prefetch, 1, 8, 0);
        assert_eq!(
            store.admit_media_frame(1, vec![1], first, 8, 0),
            FrameStoreAdmission::Resident
        );
        let (_payload, frame, protection) = store
            .protected_media_frame(&1, demand(1))
            .expect("protection admitted")
            .expect("protected frame");
        assert_eq!(
            store.admit_media_frame(1, vec![2], late, 8, 0),
            FrameStoreAdmission::Resident
        );
        assert_eq!(payload(&mut store, 1), Some(vec![1]));
        assert_eq!(store.diagnostics().media_aggregate_entries, 1);
        drop(frame);
        drop(protection);
    }

    #[test]
    fn impossible_request_does_not_evict_usable_cache() {
        let mut store = TestStore::new(config(16));
        assert_eq!(
            admit(
                &mut store,
                1,
                1,
                MediaWorkReservationClass::Prefetch,
                1,
                8,
                0,
            ),
            FrameStoreAdmission::Resident
        );
        let before = store.diagnostics();
        assert!(matches!(
            store.reserve_media_work(&2, MediaWorkReservationIntent::Prefetch, 32, 0),
            MediaWorkReservationAdmission::RejectedAggregateCapacity
        ));
        assert!(matches!(
            store.reserve_media_work(&3, MediaWorkReservationIntent::Current(demand(3)), 128, 0),
            MediaWorkReservationAdmission::RejectedCurrentDemandGrant
        ));
        assert_eq!(payload(&mut store, 1), Some(vec![1]));
        assert_eq!(store.diagnostics().media_evictions, before.media_evictions);
    }

    #[test]
    fn impossible_actual_growth_does_not_evict_usable_cache() {
        let mut store = TestStore::new(config(16));
        assert_eq!(
            admit(
                &mut store,
                1,
                1,
                MediaWorkReservationClass::Prefetch,
                1,
                8,
                0,
            ),
            FrameStoreAdmission::Resident
        );
        let work = reserve(&mut store, 2, MediaWorkReservationClass::Current, 2, 8, 0);
        let before = store.diagnostics();
        assert_eq!(
            store.admit_media_frame(2, vec![2], work, 128, 0),
            FrameStoreAdmission::RejectedCapacity
        );
        assert_eq!(payload(&mut store, 1), Some(vec![1]));
        assert_eq!(store.diagnostics().media_evictions, before.media_evictions);
    }

    #[test]
    fn actual_growth_is_rejected_before_exceeding_one_demand_grant() {
        let mut policy = config(64);
        policy.current_media_working_set_entry_limit = 2;
        policy.current_media_working_set_byte_limit = 12;
        let mut store = TestStore::new(policy);
        let first = reserve(&mut store, 1, MediaWorkReservationClass::Current, 5, 4, 0);
        let second = reserve(&mut store, 2, MediaWorkReservationClass::Current, 5, 4, 0);
        assert_eq!(
            store.admit_media_frame(1, vec![1], first, 8, 0),
            FrameStoreAdmission::Resident
        );
        let before = store.diagnostics();
        assert_eq!(
            store.admit_media_frame(2, vec![2], second, 8, 0),
            FrameStoreAdmission::RejectedCapacity
        );
        assert_eq!(payload(&mut store, 1), Some(vec![1]));
        let after = store.diagnostics();
        assert_eq!(after.media_evictions, before.media_evictions);
        assert_eq!(after.media_current_working_set_rejections, 1);
        assert_eq!(after.media_current_working_set_byte_high_water, 12);
    }

    #[test]
    fn actual_growth_records_first_use_of_current_working_set_grant() {
        let mut store = TestStore::new(config(16));
        let work = reserve(&mut store, 1, MediaWorkReservationClass::Current, 1, 8, 0);
        assert_eq!(
            store.diagnostics().media_current_working_set_grant_admissions,
            0
        );
        assert_eq!(
            store.admit_media_frame(1, vec![1], work, 24, 0),
            FrameStoreAdmission::CurrentWorkingSetResidency
        );
        assert_eq!(
            store.diagnostics().media_current_working_set_grant_admissions,
            1
        );
    }

    #[test]
    fn demand_protection_survives_store_clear_and_async_guard_clones() {
        let mut store = TestStore::new(config(16));
        assert_eq!(
            admit(
                &mut store,
                1,
                1,
                MediaWorkReservationClass::Prefetch,
                1,
                8,
                1,
            ),
            FrameStoreAdmission::Resident
        );
        let (_payload, frame, protection) = store
            .protected_media_frame(&1, demand(7))
            .expect("protection admitted")
            .expect("protected frame");
        let async_protection = protection.clone();
        let diagnostics = store.diagnostics();
        assert_eq!(diagnostics.media_active_current_working_sets, 1);
        assert_eq!(diagnostics.protected_media_entries, 1);
        assert_eq!(diagnostics.protected_media_bytes, 8);

        store.clear_media_frames();
        drop(frame);
        drop(protection);
        assert_eq!(store.diagnostics().media_aggregate_entries, 1);
        drop(async_protection);
        assert_eq!(store.diagnostics().media_aggregate_entries, 0);
        assert_eq!(store.diagnostics().media_active_current_working_sets, 0);
    }

    #[test]
    fn demand_protection_counts_unique_allocations_and_rejects_the_next_one() {
        let mut policy = config(128);
        policy.media_entry_capacity = 4;
        policy.current_media_working_set_entry_limit = 2;
        policy.current_media_working_set_byte_limit = 128;
        let mut store = TestStore::new(policy);
        for key in 1..=3 {
            assert_eq!(
                admit(
                    &mut store,
                    key,
                    key as u8,
                    MediaWorkReservationClass::Prefetch,
                    key,
                    8,
                    0,
                ),
                FrameStoreAdmission::Resident
            );
        }

        let (_first_payload, first_frame, first) = store
            .protected_media_frame(&1, demand(11))
            .expect("first protection admitted")
            .expect("first resident frame");
        let first_clone = first.clone();
        let (_same_payload, same_frame, same) = store
            .protected_media_frame(&1, demand(11))
            .expect("duplicate protection admitted")
            .expect("same resident frame");
        let (_second_payload, second_frame, second) = store
            .protected_media_frame(&2, demand(11))
            .expect("second protection admitted")
            .expect("second resident frame");
        assert!(matches!(
            store.protected_media_frame(&3, demand(11)),
            Err(MediaFrameProtectionError::CurrentWorkingSetCapacity)
        ));
        let diagnostics = store.diagnostics();
        assert_eq!(diagnostics.protected_media_entries, 2);
        assert_eq!(diagnostics.media_current_working_set_entry_high_water, 2);
        assert_eq!(diagnostics.media_current_working_set_rejections, 1);

        drop(first);
        drop(first_clone);
        drop(same);
        drop(second);
        drop(first_frame);
        drop(same_frame);
        drop(second_frame);
    }

    #[test]
    fn prefetch_headroom_excludes_only_physically_retained_allocations() {
        let mut policy = config(16);
        policy.media_entry_capacity = 2;
        let mut store = TestStore::new(policy);
        for key in 1..=2 {
            assert_eq!(
                admit(
                    &mut store,
                    key,
                    key as u8,
                    MediaWorkReservationClass::Prefetch,
                    key,
                    8,
                    0,
                ),
                FrameStoreAdmission::Resident
            );
        }
        assert_eq!(
            store.media_prefetch_headroom(),
            MediaPrefetchHeadroom { entries: 2, bytes: 16, resource_units: 4 }
        );

        let (_payload, external_lease) = store.media_frame(&1).expect("resident frame");
        assert_eq!(
            store.media_prefetch_headroom(),
            MediaPrefetchHeadroom { entries: 1, bytes: 8, resource_units: 4 }
        );
        drop(external_lease);
    }

    #[test]
    fn reconfigure_preserves_true_external_obligations_with_overcommit_evidence() {
        let mut policy = config(16);
        policy.media_entry_capacity = 2;
        policy.current_media_working_set_byte_limit = 32;
        let mut store = TestStore::new(policy);
        for key in 1..=2 {
            assert_eq!(
                admit(
                    &mut store,
                    key,
                    key as u8,
                    MediaWorkReservationClass::Prefetch,
                    key,
                    8,
                    0,
                ),
                FrameStoreAdmission::Resident
            );
        }
        let (_first_payload, first) = store.media_frame(&1).expect("first frame");
        let (_second_payload, second) = store.media_frame(&2).expect("second frame");

        let mut constrained = config(8);
        constrained.media_entry_capacity = 1;
        constrained.current_media_working_set_entry_limit = 1;
        constrained.current_media_working_set_byte_limit = 8;
        store.reconfigure(constrained);
        let overcommitted = store.diagnostics();
        assert!(overcommitted.media_capacity_overcommitted);
        assert_eq!(overcommitted.media_aggregate_entries, 2);
        assert_eq!(overcommitted.media_aggregate_reserved_bytes, 16);
        assert_eq!(overcommitted.media_capacity_overcommit_events, 1);

        store.clear_media_frames();
        assert_eq!(store.diagnostics().media_aggregate_entries, 2);
        drop(first);
        assert_eq!(store.diagnostics().media_aggregate_entries, 1);
        assert!(!store.diagnostics().media_capacity_overcommitted);
        drop(second);
        assert_eq!(store.diagnostics().media_aggregate_entries, 0);
    }

    #[test]
    fn reconfigure_reports_only_irreducible_per_demand_overcommit() {
        let mut policy = config(32);
        policy.media_entry_capacity = 2;
        policy.current_media_working_set_entry_limit = 2;
        policy.current_media_working_set_byte_limit = 16;
        let mut store = TestStore::new(policy);
        for key in 1..=2 {
            assert_eq!(
                admit(
                    &mut store,
                    key,
                    key as u8,
                    MediaWorkReservationClass::Prefetch,
                    key,
                    8,
                    0,
                ),
                FrameStoreAdmission::Resident
            );
        }
        let (_first_payload, first_frame, first_protection) = store
            .protected_media_frame(&1, demand(13))
            .expect("first protection admitted")
            .expect("first resident frame");
        let (_second_payload, second_frame, second_protection) = store
            .protected_media_frame(&2, demand(13))
            .expect("second protection admitted")
            .expect("second resident frame");

        let mut constrained = config(32);
        constrained.media_entry_capacity = 2;
        constrained.current_media_working_set_entry_limit = 1;
        constrained.current_media_working_set_byte_limit = 8;
        store.reconfigure(constrained);
        let overcommitted = store.diagnostics();
        assert!(!overcommitted.media_capacity_overcommitted);
        assert!(overcommitted.media_current_working_set_overcommitted);
        assert_eq!(overcommitted.media_current_working_set_overcommit_events, 1);

        drop(first_protection);
        drop(first_frame);
        let recovered = store.diagnostics();
        assert!(!recovered.media_current_working_set_overcommitted);
        assert_eq!(recovered.media_current_working_set_overcommit_events, 1);
        drop(second_protection);
        drop(second_frame);
    }

    #[test]
    fn clear_all_never_releases_broker_owned_work() {
        let mut store = TestStore::new(config(16));
        let work = reserve(&mut store, 1, MediaWorkReservationClass::Prefetch, 1, 8, 1);
        store.clear_all();
        let live = store.diagnostics();
        assert_eq!(live.media_work_reservations, 1);
        assert_eq!(live.media_aggregate_reserved_bytes, 8);
        drop(work);
        assert_eq!(store.diagnostics().media_aggregate_entries, 0);
    }

    #[test]
    fn decoder_resource_release_preserves_cpu_media() {
        let mut policy = config(64);
        policy.media_resource_unit_budget = 1;
        let mut store = TestStore::new(policy);
        assert_eq!(
            admit(
                &mut store,
                1,
                1,
                MediaWorkReservationClass::Prefetch,
                1,
                8,
                0,
            ),
            FrameStoreAdmission::Resident
        );
        assert_eq!(
            admit(
                &mut store,
                2,
                2,
                MediaWorkReservationClass::Prefetch,
                2,
                0,
                1,
            ),
            FrameStoreAdmission::Resident
        );

        store.clear_decoder_resource_media_frames();
        assert_eq!(payload(&mut store, 1), Some(vec![1]));
        assert_eq!(payload(&mut store, 2), None);
        assert_eq!(store.diagnostics().media_resource_units, 0);
    }

    #[test]
    fn viewer_stale_scope_and_media_clear_are_independent() {
        let mut store = TestStore::new(config(16));
        store.admit_viewer_frame(1, vec![3; 8], 8);
        store.pin_viewer_frame((7, 1920, 1080), vec![9; 4], 4);
        store.remember_failure(3);
        assert_eq!(store.stale_viewer_frame(&(7, 1920, 1080)), Some(vec![9; 4]));
        assert_eq!(store.stale_viewer_frame(&(8, 1920, 1080)), None);

        store.clear_media_frames();
        assert_eq!(store.viewer_frame(&1), Some(vec![3; 8]));
        assert_eq!(store.stale_viewer_frame(&(7, 1920, 1080)), Some(vec![9; 4]));
        assert!(store.contains_failure(&3));

        store.clear_all();
        let cleared = store.diagnostics();
        assert_eq!(cleared.viewer_entries, 0);
        assert_eq!(cleared.pinned_viewer_bytes, 0);
        assert_eq!(cleared.failure_entries, 0);
    }
}
