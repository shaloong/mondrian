//! Structurally shared containers and memory evidence for author snapshots.
//!
//! Authoring transactions clone a candidate before they mutate it. The
//! containers in this module keep that clone cheap while preserving ordinary
//! owned collection semantics at the mutation boundary. They deliberately use
//! no interior mutability: a write always detaches through [`Arc::make_mut`].
//!
//! Allocation identities and footprint manifests are process-local execution
//! evidence. They never enter persisted project data and are not cache keys.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    ffi::{OsStr, OsString},
    hash::Hash,
    iter::FromIterator,
    mem::{size_of, size_of_val},
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};
use uuid::Uuid;

/// Current conservative authoring-footprint accounting contract.
pub const AUTHORING_FOOTPRINT_VERSION: AuthoringFootprintVersion = AuthoringFootprintVersion::V1;

/// Version of the formulas used to build an [`AuthoringFootprintManifest`].
///
/// A manifest is comparable only with another manifest using the same version.
/// Version 1 charges retained allocation capacity plus deliberately
/// conservative container overhead. It is a logical retained-payload budget,
/// not an operating-system RSS measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuthoringFootprintVersion {
    /// Initial allocation-identity and capacity-based accounting contract.
    V1,
}

/// Process-local identity of one structurally shared authoring allocation.
///
/// Cloning a handle shares the identity. Copy-on-write detachment creates a new
/// identity, while mutation of an already unique allocation retains its
/// identity. The UUID is deliberately omitted from serialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthoringAllocationId(Uuid);

impl AuthoringAllocationId {
    fn fresh() -> Self {
        Self(Uuid::new_v4())
    }
}

/// One allocation claim contributed to an authoring-footprint manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthoringFootprintClaim {
    allocation_id: AuthoringAllocationId,
    conservative_bytes: u64,
}

impl AuthoringFootprintClaim {
    /// Describe the stable allocation and its complete direct charge.
    pub const fn new(allocation_id: AuthoringAllocationId, conservative_bytes: u64) -> Self {
        Self { allocation_id, conservative_bytes }
    }

    /// Allocation identity to which this direct charge belongs.
    pub const fn allocation_id(self) -> AuthoringAllocationId {
        self.allocation_id
    }

    /// Direct conservative charge, excluding independently identified children.
    pub const fn conservative_bytes(self) -> u64 {
        self.conservative_bytes
    }
}

/// Result of registering one allocation claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthoringFootprintClaimStatus {
    /// This allocation was not present and its owned children must be visited.
    Inserted,
    /// The exact allocation and direct charge were already registered.
    AlreadyPresent,
}

/// Failure to produce internally consistent authoring memory evidence.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthoringFootprintError {
    /// An allocation identity was reused with a different direct charge.
    #[error(
        "authoring allocation {allocation_id:?} was claimed as both \
         {existing_bytes} and {new_bytes} bytes"
    )]
    ConflictingAllocationCharge {
        /// Reused process-local allocation identity.
        allocation_id: AuthoringAllocationId,
        /// Direct charge recorded by the first claim.
        existing_bytes: u64,
        /// Incompatible direct charge supplied by the later claim.
        new_bytes: u64,
    },
    /// One process-local allocation identity resolved to incompatible cached
    /// ownership metadata.
    #[error("authoring allocation {allocation_id:?} has conflicting footprint descriptors")]
    ConflictingAllocationDescriptor {
        /// Reused process-local allocation identity.
        allocation_id: AuthoringAllocationId,
    },
    /// A footprint index could not reserve all storage before publication.
    #[error("authoring footprint index could not reserve required storage")]
    IndexReservationFailed,
    /// A size formula or aggregate exceeded the representable evidence range.
    #[error("authoring footprint byte accounting overflowed")]
    ByteCountOverflow,
}

#[derive(Debug)]
struct AuthoringAllocationFootprintInner {
    allocation_id: AuthoringAllocationId,
    direct_bytes: u64,
    exclusive_bytes: u64,
    local_bytes: u64,
    children: Box<[AuthoringAllocationFootprint]>,
}

/// Immutable footprint descriptor for one identified authoring allocation.
///
/// `local_bytes` contains the allocation's direct charge plus ordinary heap
/// allocations owned beneath it up to, but excluding, the next identified
/// authoring allocation. `children` retains allocation-edge multiplicity so a
/// reference-counted union can add and remove shared subgraphs exactly.
#[derive(Debug, Clone)]
pub struct AuthoringAllocationFootprint(Arc<AuthoringAllocationFootprintInner>);

impl AuthoringAllocationFootprint {
    /// Process-local allocation identity described by this node.
    pub fn allocation_id(&self) -> AuthoringAllocationId {
        self.0.allocation_id
    }

    /// Complete local charge excluding identified child allocations.
    pub fn local_bytes(&self) -> u64 {
        self.0.local_bytes
    }

    /// Direct allocation charge used by ordinary footprint manifests.
    pub fn direct_bytes(&self) -> u64 {
        self.0.direct_bytes
    }

    /// Ordinary heap charge attributed to this identified owner.
    pub fn exclusive_bytes(&self) -> u64 {
        self.0.exclusive_bytes
    }

    /// Identified child allocation edges, including repeated edges.
    pub fn children(&self) -> &[Self] {
        &self.0.children
    }

    fn structurally_matches(&self, other: &Self) -> bool {
        self.allocation_id() == other.allocation_id()
            && self.direct_bytes() == other.direct_bytes()
            && self.exclusive_bytes() == other.exclusive_bytes()
            && self.local_bytes() == other.local_bytes()
            && self.children().len() == other.children().len()
            && self
                .children()
                .iter()
                .zip(other.children())
                .all(|(left, right)| left.allocation_id() == right.allocation_id())
    }
}

/// History-owned cache of immutable allocation descriptors.
///
/// Prepared transactions read this cache but retain new descriptors privately.
/// The owner validates and reserves insertions during prepare, then applies the
/// exact insertion/removal set only after its own stale-revision check. A
/// descriptor may remain published only while the owner retains at least one
/// immutable handle to that allocation; it must be removed on the
/// zero-reference transition so a later unique mutation cannot reuse stale
/// ownership metadata.
#[derive(Debug, Clone, Default)]
pub struct AuthoringFootprintDescriptorCache {
    descriptors: Arc<Mutex<HashMap<AuthoringAllocationId, AuthoringAllocationFootprint>>>,
}

impl AuthoringFootprintDescriptorCache {
    /// Construct an empty descriptor cache.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<AuthoringAllocationId, AuthoringAllocationFootprint>> {
        self.descriptors.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn descriptor(
        &self,
        allocation_id: AuthoringAllocationId,
    ) -> Option<AuthoringAllocationFootprint> {
        self.lock().get(&allocation_id).cloned()
    }

    /// Validate descriptor identity and reserve every new cache entry.
    ///
    /// Capacity changes are non-semantic and may remain after a prepared
    /// transaction becomes stale. No descriptor is published by this method.
    pub fn validate_and_reserve(
        &self,
        insertions: &[AuthoringAllocationFootprint],
    ) -> Result<(), AuthoringFootprintError> {
        let mut descriptors = self.lock();
        let mut pending = HashMap::new();
        pending
            .try_reserve(insertions.len())
            .map_err(|_| AuthoringFootprintError::IndexReservationFailed)?;
        for insertion in insertions {
            if descriptors
                .get(&insertion.allocation_id())
                .is_some_and(|existing| !existing.structurally_matches(insertion))
            {
                return Err(AuthoringFootprintError::ConflictingAllocationDescriptor {
                    allocation_id: insertion.allocation_id(),
                });
            }
            if pending
                .insert(insertion.allocation_id(), insertion)
                .is_some_and(|existing| !existing.structurally_matches(insertion))
            {
                return Err(AuthoringFootprintError::ConflictingAllocationDescriptor {
                    allocation_id: insertion.allocation_id(),
                });
            }
        }
        let new_entries = pending
            .keys()
            .filter(|allocation_id| !descriptors.contains_key(*allocation_id))
            .count();
        descriptors
            .try_reserve(new_entries)
            .map_err(|_| AuthoringFootprintError::IndexReservationFailed)
    }

    /// Apply a descriptor transition whose identities and capacity were
    /// validated during prepare.
    ///
    /// This method performs no fallible allocation and is deliberately called
    /// only after the owning History revision has been checked.
    pub fn apply_prepared(
        &self,
        insertions: Vec<AuthoringAllocationFootprint>,
        removals: Vec<AuthoringAllocationId>,
    ) {
        let mut descriptors = self.lock();
        for allocation_id in removals {
            descriptors.remove(&allocation_id);
        }
        for insertion in insertions {
            match descriptors.get(&insertion.allocation_id()) {
                Some(existing) => {
                    debug_assert!(existing.structurally_matches(&insertion));
                }
                None => {
                    descriptors.insert(insertion.allocation_id(), insertion);
                }
            }
        }
    }

    /// Return the number of immutable allocation descriptors currently retained.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Return whether no immutable allocation descriptor remains retained.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }
}

/// One completed footprint traversal plus its identified allocation roots.
#[derive(Debug)]
pub struct AuthoringFootprintGraph {
    manifest: AuthoringFootprintManifest,
    roots: Vec<AuthoringAllocationFootprint>,
}

impl AuthoringFootprintGraph {
    /// Ordinary versioned footprint evidence produced by this traversal.
    pub fn manifest(&self) -> &AuthoringFootprintManifest {
        &self.manifest
    }

    /// Consume the traversal and return its top-level identified allocations.
    pub fn into_roots(self) -> Vec<AuthoringAllocationFootprint> {
        self.roots
    }
}

/// Immutable, versioned result of one authoring-footprint traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoringFootprintManifest {
    version: AuthoringFootprintVersion,
    total_bytes: u64,
    exclusive_bytes: u64,
    claims: BTreeMap<AuthoringAllocationId, u64>,
}

impl AuthoringFootprintManifest {
    /// Formula version used by this manifest.
    pub const fn version(&self) -> AuthoringFootprintVersion {
        self.version
    }

    /// Conservative logical bytes retained by all visited roots.
    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Bytes owned by ordinary, non-identified heap allocations.
    ///
    /// These bytes are traversed only under the first occurrence of their
    /// nearest identified owner, so they are not duplicated across shared
    /// snapshots.
    pub const fn exclusive_bytes(&self) -> u64 {
        self.exclusive_bytes
    }

    /// Number of distinct structurally shared allocations in the manifest.
    pub fn allocation_count(&self) -> usize {
        self.claims.len()
    }

    /// Direct charge recorded for one identified allocation.
    pub fn allocation_bytes(&self, allocation_id: AuthoringAllocationId) -> Option<u64> {
        self.claims.get(&allocation_id).copied()
    }

    /// Deterministically ordered allocation claims.
    pub fn claims(&self) -> impl ExactSizeIterator<Item = AuthoringFootprintClaim> + '_ {
        self.claims
            .iter()
            .map(|(allocation_id, bytes)| AuthoringFootprintClaim::new(*allocation_id, *bytes))
    }
}

/// Stateful collector that deduplicates structurally shared allocations.
///
/// Ordinary [`AuthoringFootprint`] implementations recursively collect their
/// fields. The COW handles in this Module establish identified ownership
/// boundaries and descriptor edges before visiting their contents.
#[derive(Debug)]
pub struct AuthoringFootprintCollector {
    version: AuthoringFootprintVersion,
    total_bytes: u64,
    exclusive_bytes: u64,
    claims: HashMap<AuthoringAllocationId, u64>,
    descriptor_cache: Option<AuthoringFootprintDescriptorCache>,
    merge_cached_descriptor_evidence: bool,
    local_descriptors: HashMap<AuthoringAllocationId, AuthoringAllocationFootprint>,
    descriptor_builders: Vec<AuthoringAllocationFootprintBuilder>,
    root_descriptors: Vec<AuthoringAllocationFootprint>,
}

/// Descriptor-only traversal used by retained immutable-root owners.
///
/// A cached allocation is already described by immutable ownership metadata,
/// so this collector records its edge without recursively rebuilding a
/// standalone footprint manifest. Newly encountered allocations are still
/// visited and validated in full. The retained-allocation owner remains
/// responsible for calculating the exact union and charge from the returned
/// roots.
#[derive(Debug)]
pub struct AuthoringFootprintDescriptorCollector {
    collector: AuthoringFootprintCollector,
}

impl AuthoringFootprintDescriptorCollector {
    /// Start a descriptor traversal that may reuse retained immutable roots.
    pub fn with_descriptor_cache(cache: AuthoringFootprintDescriptorCache) -> Self {
        Self {
            collector: AuthoringFootprintCollector::for_descriptor_graph(cache),
        }
    }

    /// Visit one author-model value and retain only its descriptor roots.
    pub fn collect<T: AuthoringFootprint + ?Sized>(
        &mut self,
        value: &T,
    ) -> Result<(), AuthoringFootprintError> {
        self.collector.collect(value)
    }

    /// Finish traversal and return its top-level identified allocations.
    pub fn finish_roots(self) -> Vec<AuthoringAllocationFootprint> {
        self.collector.root_descriptors
    }
}

#[derive(Debug)]
struct AuthoringAllocationFootprintBuilder {
    allocation_id: AuthoringAllocationId,
    direct_bytes: u64,
    exclusive_bytes: u64,
    children: Vec<AuthoringAllocationFootprint>,
}

impl AuthoringFootprintCollector {
    /// Start a collector using the current accounting contract.
    pub fn new() -> Self {
        Self::for_version(AUTHORING_FOOTPRINT_VERSION)
    }

    /// Start a collector for one supported accounting contract.
    pub fn for_version(version: AuthoringFootprintVersion) -> Self {
        Self {
            version,
            total_bytes: 0,
            exclusive_bytes: 0,
            claims: HashMap::new(),
            descriptor_cache: None,
            merge_cached_descriptor_evidence: true,
            local_descriptors: HashMap::new(),
            descriptor_builders: Vec::new(),
            root_descriptors: Vec::new(),
        }
    }

    /// Start a collector that may reuse immutable descriptors retained by the
    /// supplied cache.
    pub fn with_descriptor_cache(cache: AuthoringFootprintDescriptorCache) -> Self {
        let mut collector = Self::new();
        collector.descriptor_cache = Some(cache);
        collector
    }

    fn for_descriptor_graph(cache: AuthoringFootprintDescriptorCache) -> Self {
        let mut collector = Self::new();
        collector.descriptor_cache = Some(cache);
        collector.merge_cached_descriptor_evidence = false;
        collector
    }

    /// Register one low-level manifest claim and deduplicate exact repeats.
    ///
    /// This does not establish an allocation-descriptor ownership edge.
    /// Author-model code should normally compose [`AuthoringList`],
    /// [`AuthoringMap`], [`AuthoringSet`], and [`AuthoringSnapshot`] instead.
    pub fn claim(
        &mut self,
        claim: AuthoringFootprintClaim,
    ) -> Result<AuthoringFootprintClaimStatus, AuthoringFootprintError> {
        if let Some(existing_bytes) = self.claims.get(&claim.allocation_id).copied() {
            if existing_bytes == claim.conservative_bytes {
                return Ok(AuthoringFootprintClaimStatus::AlreadyPresent);
            }
            return Err(AuthoringFootprintError::ConflictingAllocationCharge {
                allocation_id: claim.allocation_id,
                existing_bytes,
                new_bytes: claim.conservative_bytes,
            });
        }
        self.total_bytes = checked_add(self.total_bytes, claim.conservative_bytes)?;
        self.claims
            .try_reserve(1)
            .map_err(|_| AuthoringFootprintError::IndexReservationFailed)?;
        self.claims.insert(claim.allocation_id, claim.conservative_bytes);
        Ok(AuthoringFootprintClaimStatus::Inserted)
    }

    /// Charge an ordinary heap allocation owned exclusively by the active
    /// identified parent.
    ///
    /// This method is public so author-model crates can implement the same
    /// contract for their own plain `String`, `Vec`, and map fields.
    pub fn charge_exclusive_bytes(
        &mut self,
        conservative_bytes: u64,
    ) -> Result<(), AuthoringFootprintError> {
        self.total_bytes = checked_add(self.total_bytes, conservative_bytes)?;
        self.exclusive_bytes = checked_add(self.exclusive_bytes, conservative_bytes)?;
        if let Some(owner) = self.descriptor_builders.last_mut() {
            owner.exclusive_bytes = checked_add(owner.exclusive_bytes, conservative_bytes)?;
        }
        Ok(())
    }

    /// Visit one author-model value.
    pub fn collect<T: AuthoringFootprint + ?Sized>(
        &mut self,
        value: &T,
    ) -> Result<(), AuthoringFootprintError> {
        value.collect_authoring_footprint(self)
    }

    /// Finish traversal and publish immutable evidence.
    pub fn finish(self) -> AuthoringFootprintManifest {
        self.finish_graph().manifest
    }

    /// Finish traversal while retaining its allocation-local ownership graph.
    pub fn finish_graph(self) -> AuthoringFootprintGraph {
        AuthoringFootprintGraph {
            manifest: AuthoringFootprintManifest {
                version: self.version,
                total_bytes: self.total_bytes,
                exclusive_bytes: self.exclusive_bytes,
                claims: self.claims.into_iter().collect(),
            },
            roots: self.root_descriptors,
        }
    }

    fn collect_identified_allocation(
        &mut self,
        allocation_id: AuthoringAllocationId,
        direct_bytes: u64,
        collect_children: impl FnOnce(&mut Self) -> Result<(), AuthoringFootprintError>,
    ) -> Result<(), AuthoringFootprintError> {
        if let Some(descriptor) = self.local_descriptors.get(&allocation_id).cloned() {
            if descriptor.direct_bytes() != direct_bytes {
                return Err(AuthoringFootprintError::ConflictingAllocationCharge {
                    allocation_id,
                    existing_bytes: descriptor.direct_bytes(),
                    new_bytes: direct_bytes,
                });
            }
            self.record_descriptor_edge(descriptor.clone())?;
            return self.merge_descriptor_evidence_if_required(&descriptor);
        }
        if let Some(descriptor) =
            self.descriptor_cache.as_ref().and_then(|cache| cache.descriptor(allocation_id))
        {
            if descriptor.direct_bytes() != direct_bytes {
                return Err(AuthoringFootprintError::ConflictingAllocationCharge {
                    allocation_id,
                    existing_bytes: descriptor.direct_bytes(),
                    new_bytes: direct_bytes,
                });
            }
            self.record_descriptor_edge(descriptor.clone())?;
            return self.merge_descriptor_evidence_if_required(&descriptor);
        }

        if self.claim(AuthoringFootprintClaim::new(allocation_id, direct_bytes))?
            != AuthoringFootprintClaimStatus::Inserted
        {
            return Err(AuthoringFootprintError::ConflictingAllocationDescriptor { allocation_id });
        }
        self.descriptor_builders
            .try_reserve(1)
            .map_err(|_| AuthoringFootprintError::IndexReservationFailed)?;
        self.descriptor_builders.push(AuthoringAllocationFootprintBuilder {
            allocation_id,
            direct_bytes,
            exclusive_bytes: 0,
            children: Vec::new(),
        });
        let collected = collect_children(self);
        let builder = self
            .descriptor_builders
            .pop()
            .ok_or(AuthoringFootprintError::ConflictingAllocationDescriptor { allocation_id })?;
        collected?;
        if builder.allocation_id != allocation_id || builder.direct_bytes != direct_bytes {
            return Err(AuthoringFootprintError::ConflictingAllocationDescriptor { allocation_id });
        }
        let local_bytes = checked_add(builder.direct_bytes, builder.exclusive_bytes)?;
        let mut children = builder.children;
        children.sort_unstable_by_key(AuthoringAllocationFootprint::allocation_id);
        let descriptor =
            AuthoringAllocationFootprint(Arc::new(AuthoringAllocationFootprintInner {
                allocation_id,
                direct_bytes,
                exclusive_bytes: builder.exclusive_bytes,
                local_bytes,
                children: children.into_boxed_slice(),
            }));
        self.local_descriptors
            .try_reserve(1)
            .map_err(|_| AuthoringFootprintError::IndexReservationFailed)?;
        if self
            .local_descriptors
            .insert(allocation_id, descriptor.clone())
            .is_some_and(|existing| !existing.structurally_matches(&descriptor))
        {
            return Err(AuthoringFootprintError::ConflictingAllocationDescriptor { allocation_id });
        }
        self.record_descriptor_edge(descriptor)
    }

    fn record_descriptor_edge(
        &mut self,
        descriptor: AuthoringAllocationFootprint,
    ) -> Result<(), AuthoringFootprintError> {
        if let Some(builder) = self.descriptor_builders.last_mut() {
            builder
                .children
                .try_reserve(1)
                .map_err(|_| AuthoringFootprintError::IndexReservationFailed)?;
            builder.children.push(descriptor);
        } else {
            self.root_descriptors
                .try_reserve(1)
                .map_err(|_| AuthoringFootprintError::IndexReservationFailed)?;
            self.root_descriptors.push(descriptor);
        }
        Ok(())
    }

    fn merge_descriptor_evidence_if_required(
        &mut self,
        descriptor: &AuthoringAllocationFootprint,
    ) -> Result<(), AuthoringFootprintError> {
        if self.merge_cached_descriptor_evidence {
            self.merge_cached_descriptor(descriptor)
        } else {
            Ok(())
        }
    }

    fn merge_cached_descriptor(
        &mut self,
        descriptor: &AuthoringAllocationFootprint,
    ) -> Result<(), AuthoringFootprintError> {
        match self.claim(AuthoringFootprintClaim::new(
            descriptor.allocation_id(),
            descriptor.direct_bytes(),
        ))? {
            AuthoringFootprintClaimStatus::AlreadyPresent => Ok(()),
            AuthoringFootprintClaimStatus::Inserted => {
                self.total_bytes = checked_add(self.total_bytes, descriptor.exclusive_bytes())?;
                self.exclusive_bytes =
                    checked_add(self.exclusive_bytes, descriptor.exclusive_bytes())?;
                for child in descriptor.children() {
                    self.merge_cached_descriptor(child)?;
                }
                Ok(())
            }
        }
    }
}

impl Default for AuthoringFootprintCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// Recursive conservative retained-payload accounting for author state.
///
/// Implementations charge only heap allocations, not the inline size of
/// `self`; the nearest owning allocation already includes that inline storage.
/// Identified COW/root allocations are the exception: they charge their own
/// heap node and visit children only for the first matching identity.
pub trait AuthoringFootprint {
    /// Add all heap allocations reachable from this value to `collector`.
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError>;
}

#[derive(Debug)]
struct AuthoringAllocation<T> {
    id: AuthoringAllocationId,
    value: T,
}

impl<T> AuthoringAllocation<T> {
    fn new(value: T) -> Self {
        Self { id: AuthoringAllocationId::fresh(), value }
    }
}

impl<T: Clone> Clone for AuthoringAllocation<T> {
    fn clone(&self) -> Self {
        Self::new(self.value.clone())
    }
}

/// An ordered authoring collection with copy-on-write snapshot semantics.
///
/// Cloning an `AuthoringList` shares its immutable backing allocation. The
/// first mutable access to either clone detaches that clone with
/// [`Arc::make_mut`], after which normal `Vec` operations apply. Serialization
/// is intentionally identical to `Vec<T>`: no sharing metadata enters the
/// persisted project schema.
#[derive(Debug, Clone)]
pub struct AuthoringList<T> {
    values: Arc<AuthoringAllocation<Vec<T>>>,
}

impl<T> AuthoringList<T> {
    /// Construct an empty authoring list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct an empty authoring list with at least the requested capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            values: Arc::new(AuthoringAllocation::new(Vec::with_capacity(capacity))),
        }
    }

    /// Stable identity of this list's current backing allocation.
    pub fn allocation_id(&self) -> AuthoringAllocationId {
        self.values.id
    }

    /// Whether two list handles share the exact immutable backing allocation.
    pub fn shares_allocation_with(&self, other: &Self) -> bool {
        self.allocation_id() == other.allocation_id()
    }

    /// Consume this list and return an ordinary `Vec`.
    ///
    /// A uniquely owned backing allocation moves without cloning. A shared
    /// allocation is detached so consuming one snapshot cannot affect another.
    pub fn into_vec(self) -> Vec<T>
    where
        T: Clone,
    {
        Arc::unwrap_or_clone(self.values).value
    }
}

impl<T> Default for AuthoringList<T> {
    fn default() -> Self {
        Self {
            values: Arc::new(AuthoringAllocation::new(Vec::new())),
        }
    }
}

impl<T> From<Vec<T>> for AuthoringList<T> {
    fn from(values: Vec<T>) -> Self {
        Self { values: Arc::new(AuthoringAllocation::new(values)) }
    }
}

impl<T, const N: usize> From<[T; N]> for AuthoringList<T> {
    fn from(values: [T; N]) -> Self {
        Vec::from(values).into()
    }
}

impl<T> FromIterator<T> for AuthoringList<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Vec::from_iter(iter).into()
    }
}

impl<T: Clone> Extend<T> for AuthoringList<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        Arc::make_mut(&mut self.values).value.extend(iter);
    }
}

impl<'a, T: Clone + 'a> Extend<&'a T> for AuthoringList<T> {
    fn extend<I: IntoIterator<Item = &'a T>>(&mut self, iter: I) {
        Arc::make_mut(&mut self.values).value.extend(iter.into_iter().cloned());
    }
}

impl<T> Deref for AuthoringList<T> {
    type Target = Vec<T>;

    fn deref(&self) -> &Self::Target {
        &self.values.value
    }
}

impl<T: Clone> DerefMut for AuthoringList<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut Arc::make_mut(&mut self.values).value
    }
}

impl<T> AsRef<[T]> for AuthoringList<T> {
    fn as_ref(&self) -> &[T] {
        self.values.value.as_slice()
    }
}

impl<T: Clone> AsMut<Vec<T>> for AuthoringList<T> {
    fn as_mut(&mut self) -> &mut Vec<T> {
        &mut Arc::make_mut(&mut self.values).value
    }
}

impl<T: PartialEq> PartialEq for AuthoringList<T> {
    fn eq(&self, other: &Self) -> bool {
        self.shares_allocation_with(other) || self.values.value == other.values.value
    }
}

impl<T: Eq> Eq for AuthoringList<T> {}

impl<T: Serialize> Serialize for AuthoringList<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.values.value.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for AuthoringList<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::<T>::deserialize(deserializer).map(Self::from)
    }
}

impl<T: Clone> IntoIterator for AuthoringList<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_vec().into_iter()
    }
}

impl<'a, T> IntoIterator for &'a AuthoringList<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.value.iter()
    }
}

impl<'a, T: Clone> IntoIterator for &'a mut AuthoringList<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        Arc::make_mut(&mut self.values).value.iter_mut()
    }
}

impl<T: AuthoringFootprint> AuthoringFootprint for AuthoringList<T> {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let direct_bytes = checked_allocation_bytes::<AuthoringAllocation<Vec<T>>>(
            self.values.value.capacity(),
            size_of::<T>(),
        )?;
        collector.collect_identified_allocation(self.allocation_id(), direct_bytes, |collector| {
            for value in &self.values.value {
                value.collect_authoring_footprint(collector)?;
            }
            Ok(())
        })
    }
}

/// Immutable root handle for one complete author snapshot.
///
/// A snapshot shares its root and every nested COW allocation when cloned. It
/// exposes only shared reads; edits must first create a detached authoring
/// candidate through the owning transaction API.
#[derive(Debug, Clone)]
pub struct AuthoringSnapshot<T> {
    root: Arc<AuthoringAllocation<T>>,
}

impl<T> AuthoringSnapshot<T> {
    /// Freeze one owned author value behind an immutable root allocation.
    pub fn new(value: T) -> Self {
        Self { root: Arc::new(AuthoringAllocation::new(value)) }
    }

    /// Stable process-local identity of the immutable root allocation.
    pub fn allocation_id(&self) -> AuthoringAllocationId {
        self.root.id
    }

    /// Read the immutable author value.
    pub fn value(&self) -> &T {
        &self.root.value
    }

    /// Whether two snapshots share the exact immutable root.
    pub fn shares_root_with(&self, other: &Self) -> bool {
        self.allocation_id() == other.allocation_id()
    }
}

impl<T> AsRef<T> for AuthoringSnapshot<T> {
    fn as_ref(&self) -> &T {
        self.value()
    }
}

impl<T> Deref for AuthoringSnapshot<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.value()
    }
}

impl<T: PartialEq> PartialEq for AuthoringSnapshot<T> {
    fn eq(&self, other: &Self) -> bool {
        self.shares_root_with(other) || self.root.value == other.root.value
    }
}

impl<T: Eq> Eq for AuthoringSnapshot<T> {}

impl<T: AuthoringFootprint> AuthoringFootprint for AuthoringSnapshot<T> {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let direct_bytes = checked_add(
            usize_to_u64(size_of::<AuthoringAllocation<T>>())?,
            conservative_arc_header_bytes()?,
        )?;
        collector.collect_identified_allocation(self.allocation_id(), direct_bytes, |collector| {
            self.root.value.collect_authoring_footprint(collector)
        })
    }
}

/// An ordered authoring map with copy-on-write snapshot semantics.
///
/// Serialization is intentionally identical to `BTreeMap<K, V>`: allocation
/// identity and sharing remain process-local execution evidence.
#[derive(Debug, Clone)]
pub struct AuthoringMap<K, V> {
    values: Arc<AuthoringAllocation<BTreeMap<K, V>>>,
}

impl<K, V> AuthoringMap<K, V> {
    /// Construct an empty authoring map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stable identity of this map's current backing allocation.
    pub fn allocation_id(&self) -> AuthoringAllocationId {
        self.values.id
    }

    /// Whether two map handles share the exact immutable backing allocation.
    pub fn shares_allocation_with(&self, other: &Self) -> bool {
        self.allocation_id() == other.allocation_id()
    }

    /// Consume this map and return an ordinary `BTreeMap`.
    pub fn into_map(self) -> BTreeMap<K, V>
    where
        K: Clone,
        V: Clone,
    {
        Arc::unwrap_or_clone(self.values).value
    }
}

impl<K, V> Default for AuthoringMap<K, V> {
    fn default() -> Self {
        Self {
            values: Arc::new(AuthoringAllocation::new(BTreeMap::new())),
        }
    }
}

impl<K, V> From<BTreeMap<K, V>> for AuthoringMap<K, V> {
    fn from(values: BTreeMap<K, V>) -> Self {
        Self { values: Arc::new(AuthoringAllocation::new(values)) }
    }
}

impl<K: Ord, V> FromIterator<(K, V)> for AuthoringMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        BTreeMap::from_iter(iter).into()
    }
}

impl<K: Clone + Ord, V: Clone> Extend<(K, V)> for AuthoringMap<K, V> {
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        Arc::make_mut(&mut self.values).value.extend(iter);
    }
}

impl<K, V> Deref for AuthoringMap<K, V> {
    type Target = BTreeMap<K, V>;

    fn deref(&self) -> &Self::Target {
        &self.values.value
    }
}

impl<K: Clone + Ord, V: Clone> DerefMut for AuthoringMap<K, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut Arc::make_mut(&mut self.values).value
    }
}

impl<'a, K, V> IntoIterator for &'a AuthoringMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = std::collections::btree_map::Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.value.iter()
    }
}

impl<'a, K: Clone + Ord, V: Clone> IntoIterator for &'a mut AuthoringMap<K, V> {
    type Item = (&'a K, &'a mut V);
    type IntoIter = std::collections::btree_map::IterMut<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        Arc::make_mut(&mut self.values).value.iter_mut()
    }
}

impl<K: Clone + Ord, V: Clone> IntoIterator for AuthoringMap<K, V> {
    type Item = (K, V);
    type IntoIter = std::collections::btree_map::IntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_map().into_iter()
    }
}

impl<K: PartialEq, V: PartialEq> PartialEq for AuthoringMap<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.shares_allocation_with(other) || self.values.value == other.values.value
    }
}

impl<K: Eq, V: Eq> Eq for AuthoringMap<K, V> {}

impl<K: Serialize, V: Serialize> Serialize for AuthoringMap<K, V> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.values.value.serialize(serializer)
    }
}

impl<'de, K, V> Deserialize<'de> for AuthoringMap<K, V>
where
    K: Deserialize<'de> + Ord,
    V: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        BTreeMap::<K, V>::deserialize(deserializer).map(Self::from)
    }
}

impl<K: AuthoringFootprint + Ord, V: AuthoringFootprint> AuthoringFootprint for AuthoringMap<K, V> {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let direct_bytes = checked_add(
            checked_add(
                usize_to_u64(size_of::<AuthoringAllocation<BTreeMap<K, V>>>())?,
                conservative_arc_header_bytes()?,
            )?,
            conservative_btree_storage::<K, V>(self.values.value.len())?,
        )?;
        collector.collect_identified_allocation(self.allocation_id(), direct_bytes, |collector| {
            for (key, value) in &self.values.value {
                key.collect_authoring_footprint(collector)?;
                value.collect_authoring_footprint(collector)?;
            }
            Ok(())
        })
    }
}

/// An ordered authoring set with copy-on-write snapshot semantics.
///
/// Serialization is intentionally identical to `BTreeSet<T>`: allocation
/// identity and sharing remain process-local execution evidence.
#[derive(Debug, Clone)]
pub struct AuthoringSet<T> {
    values: Arc<AuthoringAllocation<BTreeSet<T>>>,
}

impl<T> AuthoringSet<T> {
    /// Construct an empty authoring set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stable identity of this set's current backing allocation.
    pub fn allocation_id(&self) -> AuthoringAllocationId {
        self.values.id
    }

    /// Whether two set handles share the exact immutable backing allocation.
    pub fn shares_allocation_with(&self, other: &Self) -> bool {
        self.allocation_id() == other.allocation_id()
    }

    /// Borrow the ordinary ordered set represented by this handle.
    pub fn as_set(&self) -> &BTreeSet<T> {
        &self.values.value
    }

    /// Consume this set and return an ordinary `BTreeSet`.
    pub fn into_set(self) -> BTreeSet<T>
    where
        T: Clone,
    {
        Arc::unwrap_or_clone(self.values).value
    }
}

impl<T> Default for AuthoringSet<T> {
    fn default() -> Self {
        Self {
            values: Arc::new(AuthoringAllocation::new(BTreeSet::new())),
        }
    }
}

impl<T> From<BTreeSet<T>> for AuthoringSet<T> {
    fn from(values: BTreeSet<T>) -> Self {
        Self { values: Arc::new(AuthoringAllocation::new(values)) }
    }
}

impl<T: Ord> FromIterator<T> for AuthoringSet<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        BTreeSet::from_iter(iter).into()
    }
}

impl<T: Clone + Ord> Extend<T> for AuthoringSet<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        Arc::make_mut(&mut self.values).value.extend(iter);
    }
}

impl<T> Deref for AuthoringSet<T> {
    type Target = BTreeSet<T>;

    fn deref(&self) -> &Self::Target {
        &self.values.value
    }
}

impl<T: Clone + Ord> DerefMut for AuthoringSet<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut Arc::make_mut(&mut self.values).value
    }
}

impl<T> AsRef<BTreeSet<T>> for AuthoringSet<T> {
    fn as_ref(&self) -> &BTreeSet<T> {
        self.as_set()
    }
}

impl<'a, T> IntoIterator for &'a AuthoringSet<T> {
    type Item = &'a T;
    type IntoIter = std::collections::btree_set::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.value.iter()
    }
}

impl<T: Clone + Ord> IntoIterator for AuthoringSet<T> {
    type Item = T;
    type IntoIter = std::collections::btree_set::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_set().into_iter()
    }
}

impl<T: Ord> PartialEq for AuthoringSet<T> {
    fn eq(&self, other: &Self) -> bool {
        self.shares_allocation_with(other) || self.values.value == other.values.value
    }
}

impl<T: Ord> Eq for AuthoringSet<T> {}

impl<T: Serialize> Serialize for AuthoringSet<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.values.value.serialize(serializer)
    }
}

impl<'de, T> Deserialize<'de> for AuthoringSet<T>
where
    T: Deserialize<'de> + Ord,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        BTreeSet::<T>::deserialize(deserializer).map(Self::from)
    }
}

impl<T: AuthoringFootprint + Ord> AuthoringFootprint for AuthoringSet<T> {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let direct_bytes = checked_add(
            checked_add(
                usize_to_u64(size_of::<AuthoringAllocation<BTreeSet<T>>>())?,
                conservative_arc_header_bytes()?,
            )?,
            conservative_btree_storage::<T, ()>(self.values.value.len())?,
        )?;
        collector.collect_identified_allocation(self.allocation_id(), direct_bytes, |collector| {
            for value in &self.values.value {
                value.collect_authoring_footprint(collector)?;
            }
            Ok(())
        })
    }
}

macro_rules! no_heap_footprint {
    ($($type:ty),+ $(,)?) => {
        $(
            impl AuthoringFootprint for $type {
                fn collect_authoring_footprint(
                    &self,
                    _collector: &mut AuthoringFootprintCollector,
                ) -> Result<(), AuthoringFootprintError> {
                    Ok(())
                }
            }
        )+
    };
}

no_heap_footprint!(
    (),
    bool,
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    f32,
    f64,
    char,
    Uuid,
);

impl AuthoringFootprint for str {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        Ok(())
    }
}

impl AuthoringFootprint for String {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        collector.charge_exclusive_bytes(usize_to_u64(self.capacity())?)
    }
}

impl AuthoringFootprint for Path {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        Ok(())
    }
}

impl AuthoringFootprint for PathBuf {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        collector.charge_exclusive_bytes(usize_to_u64(self.capacity())?)
    }
}

impl AuthoringFootprint for OsStr {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        Ok(())
    }
}

impl AuthoringFootprint for OsString {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        collector.charge_exclusive_bytes(usize_to_u64(self.capacity())?)
    }
}

impl<T: AuthoringFootprint + ?Sized> AuthoringFootprint for Box<T> {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        collector.charge_exclusive_bytes(usize_to_u64(size_of_val(self.as_ref()))?)?;
        self.as_ref().collect_authoring_footprint(collector)
    }
}

impl<T: AuthoringFootprint> AuthoringFootprint for Option<T> {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        if let Some(value) = self {
            value.collect_authoring_footprint(collector)?;
        }
        Ok(())
    }
}

impl<T: AuthoringFootprint> AuthoringFootprint for Vec<T> {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        collector.charge_exclusive_bytes(checked_product(self.capacity(), size_of::<T>())?)?;
        for value in self {
            value.collect_authoring_footprint(collector)?;
        }
        Ok(())
    }
}

impl<T: AuthoringFootprint, const N: usize> AuthoringFootprint for [T; N] {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        for value in self {
            value.collect_authoring_footprint(collector)?;
        }
        Ok(())
    }
}

impl<A: AuthoringFootprint, B: AuthoringFootprint> AuthoringFootprint for (A, B) {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        self.0.collect_authoring_footprint(collector)?;
        self.1.collect_authoring_footprint(collector)
    }
}

impl<K: AuthoringFootprint + Ord, V: AuthoringFootprint> AuthoringFootprint for BTreeMap<K, V> {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        collector.charge_exclusive_bytes(conservative_btree_storage::<K, V>(self.len())?)?;
        for (key, value) in self {
            key.collect_authoring_footprint(collector)?;
            value.collect_authoring_footprint(collector)?;
        }
        Ok(())
    }
}

impl<T: AuthoringFootprint + Ord> AuthoringFootprint for BTreeSet<T> {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        collector.charge_exclusive_bytes(conservative_btree_storage::<T, ()>(self.len())?)?;
        for value in self {
            value.collect_authoring_footprint(collector)?;
        }
        Ok(())
    }
}

impl<K: AuthoringFootprint + Eq + Hash, V: AuthoringFootprint> AuthoringFootprint
    for HashMap<K, V>
{
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        collector.charge_exclusive_bytes(conservative_hash_storage::<K, V>(self.capacity())?)?;
        for (key, value) in self {
            key.collect_authoring_footprint(collector)?;
            value.collect_authoring_footprint(collector)?;
        }
        Ok(())
    }
}

impl<T: AuthoringFootprint + Eq + Hash> AuthoringFootprint for HashSet<T> {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        collector.charge_exclusive_bytes(conservative_hash_storage::<T, ()>(self.capacity())?)?;
        for value in self {
            value.collect_authoring_footprint(collector)?;
        }
        Ok(())
    }
}

impl AuthoringFootprint for serde_json::Value {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::Null | Self::Bool(_) | Self::Number(_) => Ok(()),
            Self::String(value) => value.collect_authoring_footprint(collector),
            Self::Array(values) => values.collect_authoring_footprint(collector),
            Self::Object(values) => {
                collector.charge_exclusive_bytes(conservative_btree_storage::<
                    String,
                    serde_json::Value,
                >(values.len())?)?;
                for (key, value) in values {
                    key.collect_authoring_footprint(collector)?;
                    value.collect_authoring_footprint(collector)?;
                }
                Ok(())
            }
        }
    }
}

fn checked_allocation_bytes<T>(
    capacity: usize,
    element_size: usize,
) -> Result<u64, AuthoringFootprintError> {
    checked_add(
        checked_add(
            usize_to_u64(size_of::<T>())?,
            conservative_arc_header_bytes()?,
        )?,
        checked_product(capacity, element_size)?,
    )
}

fn conservative_btree_storage<K, V>(len: usize) -> Result<u64, AuthoringFootprintError> {
    if len == 0 {
        return Ok(0);
    }
    // Version 1 charges one completely reserved B-tree node per live entry.
    // std currently packs multiple entries per node, so this deliberately
    // dominates both sparsely occupied leaf and internal-node storage without
    // making private node fan-out part of the public evidence contract.
    let node_bytes = size_of::<K>()
        .checked_add(size_of::<V>())
        .and_then(|bytes| bytes.checked_mul(11))
        .and_then(|bytes| bytes.checked_add(16 * size_of::<usize>()))
        .ok_or(AuthoringFootprintError::ByteCountOverflow)?;
    checked_product(len, node_bytes)
}

fn conservative_hash_storage<K, V>(capacity: usize) -> Result<u64, AuthoringFootprintError> {
    // Swiss-table control bytes and spare buckets are covered by a deliberately
    // generous four-word overhead per reported bucket.
    let bucket_bytes = size_of::<(K, V)>()
        .checked_add(4 * size_of::<usize>())
        .ok_or(AuthoringFootprintError::ByteCountOverflow)?;
    checked_product(capacity, bucket_bytes)
}

fn checked_product(count: usize, element_size: usize) -> Result<u64, AuthoringFootprintError> {
    let bytes = count
        .checked_mul(element_size)
        .ok_or(AuthoringFootprintError::ByteCountOverflow)?;
    usize_to_u64(bytes)
}

fn checked_add(left: u64, right: u64) -> Result<u64, AuthoringFootprintError> {
    left.checked_add(right).ok_or(AuthoringFootprintError::ByteCountOverflow)
}

fn conservative_arc_header_bytes() -> Result<u64, AuthoringFootprintError> {
    // Two reference counts plus allocator/alignment allowance.
    usize_to_u64(4 * size_of::<usize>())
}

fn usize_to_u64(value: usize) -> Result<u64, AuthoringFootprintError> {
    u64::try_from(value).map_err(|_| AuthoringFootprintError::ByteCountOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct PanicOnEquality;

    impl PartialEq for PanicOnEquality {
        fn eq(&self, _other: &Self) -> bool {
            panic!("shared allocation equality must not visit elements")
        }
    }

    impl Eq for PanicOnEquality {}

    impl PartialOrd for PanicOnEquality {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }

    impl Ord for PanicOnEquality {
        fn cmp(&self, _other: &Self) -> std::cmp::Ordering {
            std::cmp::Ordering::Equal
        }
    }

    #[test]
    fn clone_shares_id_until_mutation_then_detaches() {
        let original = AuthoringList::from(vec![1, 2, 3]);
        let original_id = original.allocation_id();
        let mut edited = original.clone();
        assert_eq!(edited.allocation_id(), original_id);

        edited.push(4);

        assert_eq!(original.as_slice(), &[1, 2, 3]);
        assert_eq!(edited.as_slice(), &[1, 2, 3, 4]);
        assert_ne!(edited.allocation_id(), original_id);
    }

    #[test]
    fn unique_mutation_retains_allocation_id() {
        let mut values = AuthoringList::from(vec![1, 2, 3]);
        let allocation_id = values.allocation_id();

        values.push(4);

        assert_eq!(values.allocation_id(), allocation_id);
    }

    #[test]
    fn vec_like_construction_iteration_and_mutation_are_complete() {
        let mut values = AuthoringList::with_capacity(4);
        values.extend([2, 3]);
        values.insert(0, 1);
        values.push(4);
        values.retain(|value| *value != 2);
        values.sort_by(|left, right| right.cmp(left));

        for value in &mut values {
            *value += 1;
        }
        let borrowed = (&values).into_iter().copied().collect::<Vec<_>>();
        assert_eq!(borrowed, vec![5, 4, 2]);
        assert_eq!(values.remove(1), 4);
        assert_eq!(values.into_iter().collect::<Vec<_>>(), vec![5, 2]);
    }

    #[test]
    fn serde_representation_is_identical_to_vec_and_renews_runtime_id() {
        let vec_value = vec!["picture", "sound"];
        let list = AuthoringList::from(vec_value.clone());

        let vec_json = serde_json::to_vec(&vec_value).expect("serialize Vec");
        let list_json = serde_json::to_vec(&list).expect("serialize AuthoringList");
        assert_eq!(list_json, vec_json);

        let restored: AuthoringList<String> =
            serde_json::from_slice(&list_json).expect("deserialize AuthoringList");
        assert_eq!(restored.as_slice(), vec_value.as_slice());
        assert_ne!(restored.allocation_id(), list.allocation_id());
    }

    #[test]
    fn consuming_a_shared_list_preserves_the_other_snapshot() {
        let retained = AuthoringList::from(vec![String::from("A"), String::from("B")]);
        let consumed = retained.clone().into_vec();

        assert_eq!(consumed, vec!["A", "B"]);
        assert_eq!(retained.as_slice(), &["A", "B"]);
    }

    #[test]
    fn shared_collections_short_circuit_equality_before_element_comparison() {
        let list = AuthoringList::from(vec![PanicOnEquality]);
        assert_eq!(list, list.clone());

        let map = AuthoringMap::from(BTreeMap::from([(1_u8, PanicOnEquality)]));
        assert_eq!(map, map.clone());

        let set = AuthoringSet::from(BTreeSet::from([PanicOnEquality]));
        assert_eq!(set, set.clone());
    }

    #[test]
    fn immutable_snapshot_clone_shares_root_and_exposes_only_reads() {
        let snapshot = AuthoringSnapshot::new(String::from("project"));
        let retained = snapshot.clone();

        assert!(snapshot.shares_root_with(&retained));
        assert_eq!(snapshot.value(), "project");
    }

    #[test]
    fn private_map_is_copy_on_write_and_json_object_shaped() {
        let mut original = AuthoringMap::<String, i32>::default();
        original.insert("gain".to_owned(), 1);
        let original_id = original.allocation_id();
        let mut edited = original.clone();
        assert_eq!(edited.allocation_id(), original_id);

        edited.insert("pan".to_owned(), 2);

        assert_eq!(original.len(), 1);
        assert_eq!(edited.len(), 2);
        assert_ne!(edited.allocation_id(), original_id);
        let edited_id = edited.allocation_id();
        edited.insert("width".to_owned(), 3);
        assert_eq!(edited.allocation_id(), edited_id);
        assert_eq!(
            serde_json::to_value(&edited).expect("serialize map"),
            serde_json::json!({"gain": 1, "pan": 2, "width": 3})
        );
    }

    #[test]
    fn private_set_is_copy_on_write_and_json_array_shaped() {
        let first = crate::AssetId::new();
        let second = crate::AssetId::new();
        let third = crate::AssetId::new();
        let original = AuthoringSet::from(BTreeSet::from([first, second]));
        let original_id = original.allocation_id();
        let mut edited = original.clone();

        assert!(edited.shares_allocation_with(&original));
        assert!(edited.insert(third));

        assert_eq!(original.len(), 2);
        assert_eq!(edited.len(), 3);
        assert_ne!(edited.allocation_id(), original_id);
        let edited_id = edited.allocation_id();
        assert!(edited.remove(&first));
        assert_eq!(edited.allocation_id(), edited_id);

        let ordinary = BTreeSet::from([first, second]);
        assert_eq!(
            serde_json::to_value(&original).expect("serialize set"),
            serde_json::to_value(&ordinary).expect("serialize BTreeSet")
        );
        let restored: AuthoringSet<crate::AssetId> =
            serde_json::from_value(serde_json::to_value(&original).expect("serialize set"))
                .expect("deserialize set");
        assert_eq!(restored.as_set(), &ordinary);
        assert_ne!(restored.allocation_id(), original_id);
    }

    #[test]
    fn nested_shared_allocations_are_deduplicated_across_distinct_roots() {
        let values = AuthoringList::from(vec![String::from("shared payload")]);
        let left = AuthoringSnapshot::new(values.clone());
        let right = AuthoringSnapshot::new(values);
        let mut collector = AuthoringFootprintCollector::new();

        collector.collect(&left).expect("collect left");
        let after_left = collector.total_bytes;
        collector.collect(&right).expect("collect right");
        let manifest = collector.finish();

        assert_eq!(manifest.allocation_count(), 3);
        assert_eq!(
            manifest.total_bytes() - after_left,
            size_of::<AuthoringAllocation<AuthoringList<String>>>() as u64
                + conservative_arc_header_bytes().expect("Arc header charge")
        );
    }

    #[test]
    fn allocation_graph_attributes_exclusive_bytes_and_preserves_edge_multiplicity() {
        struct RepeatedChildOwner {
            label: String,
            child: AuthoringList<String>,
        }

        impl AuthoringFootprint for RepeatedChildOwner {
            fn collect_authoring_footprint(
                &self,
                collector: &mut AuthoringFootprintCollector,
            ) -> Result<(), AuthoringFootprintError> {
                collector.collect(&self.label)?;
                collector.collect(&self.child)?;
                collector.collect(&self.child)
            }
        }

        let owner = RepeatedChildOwner {
            label: String::from("nearest identified owner"),
            child: AuthoringList::from(vec![String::from("shared child payload")]),
        };
        let expected_root_exclusive = owner.label.capacity() as u64;
        let expected_child_exclusive = owner.child[0].capacity() as u64;
        let child_id = owner.child.allocation_id();
        let snapshot = AuthoringSnapshot::new(owner);
        let mut collector = AuthoringFootprintCollector::new();
        collector.collect(&snapshot).expect("collect allocation graph");
        let graph = collector.finish_graph();
        let manifest_total = graph.manifest().total_bytes();
        let roots = graph.into_roots();

        assert_eq!(roots.len(), 1);
        let root = &roots[0];
        assert_eq!(root.exclusive_bytes(), expected_root_exclusive);
        assert_eq!(
            root.local_bytes(),
            root.direct_bytes() + expected_root_exclusive
        );
        assert_eq!(root.children().len(), 2);
        assert!(root.children().iter().all(|child| child.allocation_id() == child_id));
        assert_eq!(
            root.children()[0].exclusive_bytes(),
            expected_child_exclusive
        );
        assert_eq!(
            manifest_total,
            root.local_bytes() + root.children()[0].local_bytes()
        );
    }

    #[test]
    fn cached_child_descriptor_matches_a_cold_recomputation() {
        fn flatten(
            descriptor: &AuthoringAllocationFootprint,
            flattened: &mut Vec<AuthoringAllocationFootprint>,
        ) {
            if flattened
                .iter()
                .any(|candidate| candidate.allocation_id() == descriptor.allocation_id())
            {
                return;
            }
            flattened.push(descriptor.clone());
            for child in descriptor.children() {
                flatten(child, flattened);
            }
        }

        let shared = AuthoringList::from(vec![
            String::from("shared payload"),
            String::from("second payload"),
        ]);
        let left = AuthoringSnapshot::new(shared.clone());
        let right = AuthoringSnapshot::new(shared);
        let cache = AuthoringFootprintDescriptorCache::new();
        let mut initial = AuthoringFootprintCollector::new();
        initial.collect(&left).expect("collect initial descriptor graph");
        let initial_graph = initial.finish_graph();
        let mut descriptors = Vec::new();
        for root in initial_graph.into_roots() {
            flatten(&root, &mut descriptors);
        }
        cache.validate_and_reserve(&descriptors).expect("reserve cached descriptors");
        cache.apply_prepared(descriptors, Vec::new());

        let mut cold = AuthoringFootprintCollector::new();
        cold.collect(&right).expect("cold footprint");
        let cold_manifest = cold.finish();
        let mut cached = AuthoringFootprintCollector::with_descriptor_cache(cache);
        cached.collect(&right).expect("cached footprint");
        let cached_manifest = cached.finish();

        assert_eq!(cached_manifest, cold_manifest);
    }

    #[test]
    fn descriptor_only_collection_reuses_a_cached_root_without_merging_its_dag() {
        fn flatten(
            descriptor: &AuthoringAllocationFootprint,
            flattened: &mut Vec<AuthoringAllocationFootprint>,
        ) {
            if flattened
                .iter()
                .any(|candidate| candidate.allocation_id() == descriptor.allocation_id())
            {
                return;
            }
            flattened.push(descriptor.clone());
            for child in descriptor.children() {
                flatten(child, flattened);
            }
        }

        let snapshot = AuthoringSnapshot::new(AuthoringList::from(vec![
            String::from("first nested payload"),
            String::from("second nested payload"),
        ]));
        let mut cold = AuthoringFootprintCollector::new();
        cold.collect(&snapshot).expect("collect cold descriptor graph");
        let graph = cold.finish_graph();
        let mut descriptors = Vec::new();
        for root in graph.into_roots() {
            flatten(&root, &mut descriptors);
        }
        assert!(descriptors.len() > 1);
        let cache = AuthoringFootprintDescriptorCache::new();
        cache.validate_and_reserve(&descriptors).expect("reserve descriptor graph");
        cache.apply_prepared(descriptors, Vec::new());

        let mut collector = AuthoringFootprintDescriptorCollector::with_descriptor_cache(cache);
        collector.collect(&snapshot).expect("reuse cached immutable root");

        assert!(collector.collector.claims.is_empty());
        assert!(collector.collector.local_descriptors.is_empty());
        assert_eq!(collector.collector.total_bytes, 0);
        assert_eq!(collector.collector.exclusive_bytes, 0);
        let roots = collector.finish_roots();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].allocation_id(), snapshot.allocation_id());
        assert!(!roots[0].children().is_empty());
    }

    #[test]
    fn descriptor_only_collection_rejects_a_conflicting_cached_root() {
        let snapshot = AuthoringSnapshot::new(String::from("payload"));
        let mut cold = AuthoringFootprintCollector::new();
        cold.collect(&snapshot).expect("collect real root");
        let mut roots = cold.finish_graph().into_roots();
        let real = roots.pop().expect("real root descriptor");
        assert!(roots.is_empty());
        let conflicting_direct_bytes = real.direct_bytes() + 1;
        let conflicting =
            AuthoringAllocationFootprint(Arc::new(AuthoringAllocationFootprintInner {
                allocation_id: real.allocation_id(),
                direct_bytes: conflicting_direct_bytes,
                exclusive_bytes: real.exclusive_bytes(),
                local_bytes: real.local_bytes() + 1,
                children: real.children().to_vec().into_boxed_slice(),
            }));
        let cache = AuthoringFootprintDescriptorCache::new();
        cache
            .validate_and_reserve(std::slice::from_ref(&conflicting))
            .expect("reserve conflicting descriptor");
        cache.apply_prepared(vec![conflicting], Vec::new());
        let mut collector = AuthoringFootprintDescriptorCollector::with_descriptor_cache(cache);

        let error = collector
            .collect(&snapshot)
            .expect_err("cached direct charge mismatch must fail");

        assert!(matches!(
            error,
            AuthoringFootprintError::ConflictingAllocationCharge {
                allocation_id,
                existing_bytes,
                new_bytes,
            } if allocation_id == snapshot.allocation_id()
                && existing_bytes == conflicting_direct_bytes
                && new_bytes == real.direct_bytes()
        ));
    }

    #[test]
    fn descriptor_cache_rejects_conflicts_and_reclaims_removed_allocations() {
        fn descriptor(
            allocation_id: AuthoringAllocationId,
            direct_bytes: u64,
        ) -> AuthoringAllocationFootprint {
            AuthoringAllocationFootprint(Arc::new(AuthoringAllocationFootprintInner {
                allocation_id,
                direct_bytes,
                exclusive_bytes: 0,
                local_bytes: direct_bytes,
                children: Vec::new().into_boxed_slice(),
            }))
        }

        let allocation_id = AuthoringAllocationId::fresh();
        let retained = descriptor(allocation_id, 10);
        let conflicting = descriptor(allocation_id, 11);
        let cache = AuthoringFootprintDescriptorCache::new();
        cache
            .validate_and_reserve(std::slice::from_ref(&retained))
            .expect("reserve descriptor");
        cache.apply_prepared(vec![retained], Vec::new());

        assert_eq!(cache.len(), 1);
        assert!(matches!(
            cache.validate_and_reserve(&[conflicting]),
            Err(AuthoringFootprintError::ConflictingAllocationDescriptor {
                allocation_id: conflicting_id,
            }) if conflicting_id == allocation_id
        ));

        cache.apply_prepared(Vec::new(), vec![allocation_id]);
        assert_eq!(cache.len(), 0);
        let first = descriptor(allocation_id, 10);
        let second = descriptor(allocation_id, 11);
        assert!(matches!(
            cache.validate_and_reserve(&[first, second]),
            Err(AuthoringFootprintError::ConflictingAllocationDescriptor {
                allocation_id: conflicting_id,
            }) if conflicting_id == allocation_id
        ));
    }

    #[test]
    fn json_value_variants_charge_every_dynamic_branch() {
        let value = serde_json::json!({
            "null": null,
            "bool": true,
            "number": 42,
            "text": "payload",
            "array": ["nested", {"leaf": "value"}]
        });
        let snapshot = AuthoringSnapshot::new(value);
        let mut collector = AuthoringFootprintCollector::new();

        collector.collect(&snapshot).expect("collect Value");
        let manifest = collector.finish();

        assert_eq!(manifest.version(), AUTHORING_FOOTPRINT_VERSION);
        assert_eq!(manifest.allocation_count(), 1);
        assert!(manifest.exclusive_bytes() > 0);
        assert!(manifest.total_bytes() > manifest.exclusive_bytes());
    }

    #[test]
    fn generic_dynamic_allocations_cover_path_box_os_string_and_tree_set() {
        struct DynamicKinds {
            boxed: Box<str>,
            path: PathBuf,
            os: OsString,
            ids: BTreeSet<crate::AssetId>,
        }

        impl AuthoringFootprint for DynamicKinds {
            fn collect_authoring_footprint(
                &self,
                collector: &mut AuthoringFootprintCollector,
            ) -> Result<(), AuthoringFootprintError> {
                collector.collect(&self.boxed)?;
                collector.collect(&self.path)?;
                collector.collect(&self.os)?;
                collector.collect(&self.ids)
            }
        }

        let value = DynamicKinds {
            boxed: String::from("boxed payload").into_boxed_str(),
            path: PathBuf::from("project/cache/preview"),
            os: OsString::from("platform-name"),
            ids: BTreeSet::from([crate::AssetId::new()]),
        };
        let mut collector = AuthoringFootprintCollector::new();

        collector
            .collect(&AuthoringSnapshot::new(value))
            .expect("collect dynamic allocation kinds");

        assert!(collector.finish().exclusive_bytes() > 0);
    }

    #[test]
    fn persisted_core_author_types_form_one_footprint_trait_closure() {
        fn assert_footprint<T: AuthoringFootprint>() {}

        assert_footprint::<crate::ProjectMeta>();
        assert_footprint::<crate::ProjectSettings>();
        assert_footprint::<crate::ProjectColorEnvironment>();
        assert_footprint::<crate::timeline_data::ClipContent>();
        assert_footprint::<crate::effect_data::EffectNode>();
        assert_footprint::<crate::mask_data::MaskComponent>();
        assert_footprint::<crate::BasicTitle>();
        assert_footprint::<crate::PropertyBag>();
        assert_footprint::<crate::ExactAutomationCurve>();
        assert_footprint::<crate::AudioChannelMixMatrix>();
        assert_footprint::<crate::AssetSource>();
    }

    #[test]
    fn collector_rejects_conflicting_charge_for_same_id() {
        let values = AuthoringList::<u8>::new();
        let id = values.allocation_id();
        let mut collector = AuthoringFootprintCollector::new();

        assert_eq!(
            collector.claim(AuthoringFootprintClaim::new(id, 10)).expect("first claim"),
            AuthoringFootprintClaimStatus::Inserted
        );
        let error = collector
            .claim(AuthoringFootprintClaim::new(id, 11))
            .expect_err("conflicting claim");

        assert!(matches!(
            error,
            AuthoringFootprintError::ConflictingAllocationCharge {
                allocation_id,
                existing_bytes: 10,
                new_bytes: 11,
            } if allocation_id == id
        ));
    }

    #[test]
    fn collector_rejects_aggregate_overflow() {
        let mut collector = AuthoringFootprintCollector::new();
        collector
            .charge_exclusive_bytes(u64::MAX)
            .expect("maximum representable charge");

        assert_eq!(
            collector.charge_exclusive_bytes(1),
            Err(AuthoringFootprintError::ByteCountOverflow)
        );
    }
}
