//! Cross-domain admission for independently owned heavy execution queues.
//!
//! This Module allocates coarse machine slots to execution domains. It does
//! not own jobs, worker threads, queues, leases, cancellation, or completion
//! evidence. Each domain remains responsible for stopping dispatch before a
//! slot is handed to another domain and for reporting its aggregate facts on
//! the next observation.

use std::cmp::Reverse;
use std::time::{Duration, Instant};

const EXECUTION_RESOURCE_SLOT_DOMAIN_COUNT: usize = 7;
const DEFAULT_GRANT_GRACE: Duration = Duration::from_secs(2);
const MINIMUM_GRANT_GRACE: Duration = Duration::from_millis(1);

/// An independently scheduled domain that may perform memory- or codec-heavy
/// background work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum ExecutionResourceSlotDomain {
    Proxy = 0,
    Thumbnail = 1,
    Waveform = 2,
    AudioWarmup = 3,
    MediaImport = 4,
    MediaAssetMutation = 5,
    Export = 6,
}

impl ExecutionResourceSlotDomain {
    const ALL: [Self; EXECUTION_RESOURCE_SLOT_DOMAIN_COUNT] = [
        Self::Proxy,
        Self::Thumbnail,
        Self::Waveform,
        Self::AudioWarmup,
        Self::MediaImport,
        Self::MediaAssetMutation,
        Self::Export,
    ];

    const fn index(self) -> usize {
        self as usize
    }

    const fn successor_index(self) -> usize {
        (self.index() + 1) % EXECUTION_RESOURCE_SLOT_DOMAIN_COUNT
    }
}

/// Aggregate queue facts reported by one domain-owned scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ExecutionResourceSlotDomainFacts {
    /// Work accepted by the domain but not currently executing.
    pub(crate) queued: usize,
    /// Attempts currently executing inside the domain.
    pub(crate) running: usize,
    /// Queued or running work caused directly by a user command.
    pub(crate) user_initiated: usize,
    /// Monotonic terminal-attempt/file generation owned by the domain.
    ///
    /// Advancing this value proves that the domain crossed a safe scheduling
    /// boundary. It does not transfer ownership of domain terminal evidence to
    /// this allocator.
    pub(crate) terminal_generation: u64,
}

impl ExecutionResourceSlotDomainFacts {
    fn has_waiting_work(self) -> bool {
        self.queued > 0
    }

    fn has_any_work(self) -> bool {
        self.queued > 0 || self.running > 0
    }

    fn has_explicit_work(self) -> bool {
        self.user_initiated > 0
    }
}

/// Typed aggregate facts for all independently owned heavy domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ExecutionResourceSlotDemandSnapshot {
    pub(crate) proxy: ExecutionResourceSlotDomainFacts,
    pub(crate) thumbnail: ExecutionResourceSlotDomainFacts,
    pub(crate) waveform: ExecutionResourceSlotDomainFacts,
    pub(crate) audio_warmup: ExecutionResourceSlotDomainFacts,
    pub(crate) media_import: ExecutionResourceSlotDomainFacts,
    pub(crate) media_asset_mutation: ExecutionResourceSlotDomainFacts,
    pub(crate) export: ExecutionResourceSlotDomainFacts,
}

impl ExecutionResourceSlotDemandSnapshot {
    fn facts(self, domain: ExecutionResourceSlotDomain) -> ExecutionResourceSlotDomainFacts {
        match domain {
            ExecutionResourceSlotDomain::Proxy => self.proxy,
            ExecutionResourceSlotDomain::Thumbnail => self.thumbnail,
            ExecutionResourceSlotDomain::Waveform => self.waveform,
            ExecutionResourceSlotDomain::AudioWarmup => self.audio_warmup,
            ExecutionResourceSlotDomain::MediaImport => self.media_import,
            ExecutionResourceSlotDomain::MediaAssetMutation => self.media_asset_mutation,
            ExecutionResourceSlotDomain::Export => self.export,
        }
    }
}

/// One monotonic observation consumed by the slot allocator.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ExecutionResourceSlotInput {
    /// Monotonic observation time. Regressing values are conservatively
    /// clamped to the last observed value.
    pub(crate) now: Instant,
    /// Coarse cross-domain capacity selected by the product resource policy.
    ///
    /// The allocator intentionally does not infer a machine class. The caller
    /// supplies zero while realtime work or critical pressure pauses heavy
    /// background dispatch, and ordinarily supplies one, two, or four.
    pub(crate) machine_slot_capacity: usize,
    /// Whether the product policy currently permits any new heavy dispatch.
    pub(crate) dispatch_allowed: bool,
    /// Domain-owned aggregate queue facts.
    pub(crate) demand: ExecutionResourceSlotDemandSnapshot,
    /// Domains whose `demand` facts were sampled for this exact update.
    ///
    /// Pressure/profile changes may legitimately recompute capacity from the
    /// last published demand, and an App-internal refresh may reuse Window-owned
    /// facts. Neither cached subset can prove a post-close `running == 0`
    /// observation for a domain it did not sample.
    pub(crate) fresh_demand_observations: ExecutionResourceSlotDomains,
}

/// A compact set of execution domains used by transition evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ExecutionResourceSlotDomains(u16);

impl ExecutionResourceSlotDomains {
    /// Empty domain set.
    pub(crate) const fn empty() -> Self {
        Self(0)
    }

    /// Set containing every heavy execution domain.
    pub(crate) const fn all() -> Self {
        Self((1_u16 << EXECUTION_RESOURCE_SLOT_DOMAIN_COUNT) - 1)
    }

    /// Build a set from typed domain identities.
    pub(crate) fn from_domains(
        domains: impl IntoIterator<Item = ExecutionResourceSlotDomain>,
    ) -> Self {
        let mut result = Self::empty();
        for domain in domains {
            result.insert(domain);
        }
        result
    }

    fn insert(&mut self, domain: ExecutionResourceSlotDomain) {
        self.0 |= 1_u16 << domain.index();
    }

    fn remove(&mut self, domain: ExecutionResourceSlotDomain) {
        self.0 &= !(1_u16 << domain.index());
    }

    /// Whether the set contains `domain`.
    pub(crate) const fn contains(self, domain: ExecutionResourceSlotDomain) -> bool {
        (self.0 & (1_u16 << domain.index())) != 0
    }

    /// Number of domains in the set.
    pub(crate) const fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    /// Whether the set contains no domains.
    pub(crate) const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Iterates domains in their stable scheduling order.
    pub(crate) fn iter(self) -> impl Iterator<Item = ExecutionResourceSlotDomain> {
        ExecutionResourceSlotDomain::ALL
            .into_iter()
            .filter(move |domain| self.contains(*domain))
    }

    const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

/// Effective permission to cross each domain-owned dispatch seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ExecutionResourceSlotAllocation {
    admitted: ExecutionResourceSlotDomains,
}

impl ExecutionResourceSlotAllocation {
    /// Whether `domain` may start another attempt.
    ///
    /// A running domain may temporarily return `false` while it drains an
    /// already-started attempt. This method never grants ownership of the
    /// domain's queue or worker.
    pub(crate) const fn admits(self, domain: ExecutionResourceSlotDomain) -> bool {
        self.admitted.contains(domain)
    }

    /// Number of domains currently admitted for new dispatch.
    #[cfg(test)]
    pub(crate) const fn len(self) -> usize {
        self.admitted.len()
    }

    /// Whether no domain is currently admitted for new dispatch.
    #[cfg(test)]
    pub(crate) const fn is_empty(self) -> bool {
        self.admitted.is_empty()
    }
}

/// One deterministic allocation and its two-phase handoff evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ExecutionResourceSlotDecision {
    allocation: ExecutionResourceSlotAllocation,
    #[cfg(test)]
    revoked: ExecutionResourceSlotDomains,
    draining: ExecutionResourceSlotDomains,
    #[cfg(test)]
    domains_to_open: ExecutionResourceSlotDomains,
    /// Token for the currently outstanding close-dispatch transition.
    ///
    /// A composition root must apply every relevant close request and
    /// acknowledge it before a later, fresh demand observation may retire the
    /// corresponding draining domain.
    close_epoch: Option<u64>,
}

impl ExecutionResourceSlotDecision {
    /// Final dispatch allocation after applying this transition.
    pub(crate) const fn allocation(self) -> ExecutionResourceSlotAllocation {
        self.allocation
    }

    /// Domains whose prior grants were revoked by this observation.
    #[cfg(test)]
    pub(crate) const fn revoked(self) -> ExecutionResourceSlotDomains {
        self.revoked
    }

    /// Revoked domains that still report an executing attempt.
    #[cfg(test)]
    pub(crate) const fn draining(self) -> ExecutionResourceSlotDomains {
        self.draining
    }

    /// Domains whose dispatch seams must be closed before any returned opening
    /// is applied.
    ///
    /// Draining domains remain in this set on every observation, making an
    /// idempotent composition-root projection safe after a missed UI tick.
    pub(crate) const fn domains_to_close(self) -> ExecutionResourceSlotDomains {
        self.draining
    }

    /// Domains that may be opened only after `domains_to_close` has been
    /// applied.
    ///
    /// This set is always empty while any revoked domain still runs, which
    /// prevents a handoff from transiently exceeding machine capacity.
    #[cfg(test)]
    pub(crate) const fn domains_to_open_after_close(self) -> ExecutionResourceSlotDomains {
        self.domains_to_open
    }

    /// Token that binds close-dispatch acknowledgements to this transition.
    pub(crate) const fn close_epoch(self) -> Option<u64> {
        self.close_epoch
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct DomainAllocationState {
    observed_terminal_generation: Option<u64>,
    granted_at: Option<Instant>,
    grant_serial: u64,
}

/// Pure state machine that allocates coarse slots between domain-owned queues.
///
/// Existing running attempts are sticky unless capacity or product policy is
/// reduced. Revocation then closes only future dispatch and keeps the domain
/// in `draining` until it reports zero running attempts.
#[derive(Debug)]
pub(crate) struct ExecutionResourceSlotAllocator {
    active: ExecutionResourceSlotDomains,
    draining: ExecutionResourceSlotDomains,
    acknowledged_closures: ExecutionResourceSlotDomains,
    domains: [DomainAllocationState; EXECUTION_RESOURCE_SLOT_DOMAIN_COUNT],
    next_domain_index: usize,
    next_grant_serial: u64,
    next_close_epoch: u64,
    pending_close_epoch: Option<u64>,
    grant_grace: Duration,
    last_now: Option<Instant>,
}

impl Default for ExecutionResourceSlotAllocator {
    fn default() -> Self {
        Self::new(DEFAULT_GRANT_GRACE)
    }
}

impl ExecutionResourceSlotAllocator {
    /// Creates an allocator with a bounded grace period for a domain to start
    /// work or publish a new terminal generation after receiving a grant.
    pub(crate) fn new(grant_grace: Duration) -> Self {
        Self {
            active: ExecutionResourceSlotDomains::empty(),
            draining: ExecutionResourceSlotDomains::empty(),
            acknowledged_closures: ExecutionResourceSlotDomains::empty(),
            domains: [DomainAllocationState::default(); EXECUTION_RESOURCE_SLOT_DOMAIN_COUNT],
            next_domain_index: 0,
            next_grant_serial: 1,
            next_close_epoch: 1,
            pending_close_epoch: None,
            grant_grace: grant_grace.max(MINIMUM_GRANT_GRACE),
            last_now: None,
        }
    }

    /// Confirms that a composition root synchronously closed one domain's
    /// dispatch seam for the named transition.
    ///
    /// Acknowledgement alone never releases a slot. The allocator waits for a
    /// later `update` carrying a fresh `running == 0` observation, closing the
    /// snapshot-to-policy TOCTOU window.
    pub(crate) fn acknowledge_close(
        &mut self,
        close_epoch: u64,
        domain: ExecutionResourceSlotDomain,
    ) -> bool {
        if self.pending_close_epoch != Some(close_epoch) || !self.draining.contains(domain) {
            return false;
        }
        self.acknowledged_closures.insert(domain);
        true
    }

    /// Advances the allocator from one aggregate observation.
    pub(crate) fn update(
        &mut self,
        input: ExecutionResourceSlotInput,
    ) -> ExecutionResourceSlotDecision {
        let now = self.last_now.map_or(input.now, |last_now| last_now.max(input.now));
        self.last_now = Some(now);

        let previous_active = self.active;
        let mut revoked = ExecutionResourceSlotDomains::empty();
        let terminal_advanced =
            self.observe_terminal_generations(input.demand, input.fresh_demand_observations);
        self.finish_acknowledged_draining_domains(input.demand, input.fresh_demand_observations);

        let capacity = if input.dispatch_allowed {
            input.machine_slot_capacity.min(EXECUTION_RESOURCE_SLOT_DOMAIN_COUNT)
        } else {
            0
        };

        self.adopt_untracked_running_domains(
            input.demand,
            input.dispatch_allowed,
            now,
            &mut revoked,
        );
        self.revoke_for_capacity(capacity, input.demand, &mut revoked);
        if !self.draining.is_empty() {
            for domain in self.active.difference(previous_active).iter() {
                self.revoke(domain, &mut revoked);
            }
        }

        if capacity > 0 {
            self.revoke_idle_domains(input.demand, &mut revoked);
            self.preempt_automatic_grants_for_explicit_work(capacity, input.demand, &mut revoked);
            self.rotate_completed_or_stalled_grants(
                capacity,
                input.demand,
                terminal_advanced,
                now,
                &mut revoked,
            );
            self.refresh_continuing_grants(terminal_advanced, now);
        }

        if capacity > 0 && self.draining.is_empty() {
            self.fill_available_slots(capacity, input.demand, now);
        }

        if !revoked.is_empty() {
            self.begin_close_transition();
        }
        ExecutionResourceSlotDecision {
            allocation: ExecutionResourceSlotAllocation { admitted: self.active },
            #[cfg(test)]
            revoked,
            draining: self.draining,
            #[cfg(test)]
            domains_to_open: if self.draining.is_empty() {
                self.active.difference(previous_active)
            } else {
                ExecutionResourceSlotDomains::empty()
            },
            close_epoch: self.pending_close_epoch,
        }
    }

    fn observe_terminal_generations(
        &mut self,
        demand: ExecutionResourceSlotDemandSnapshot,
        fresh_demand_observations: ExecutionResourceSlotDomains,
    ) -> ExecutionResourceSlotDomains {
        let mut advanced = ExecutionResourceSlotDomains::empty();
        for domain in ExecutionResourceSlotDomain::ALL {
            if !fresh_demand_observations.contains(domain) {
                continue;
            }
            let generation = demand.facts(domain).terminal_generation;
            let state = &mut self.domains[domain.index()];
            if state.observed_terminal_generation.is_some_and(|observed| generation > observed) {
                advanced.insert(domain);
            }
            state.observed_terminal_generation = Some(generation);
        }
        advanced
    }

    fn finish_acknowledged_draining_domains(
        &mut self,
        demand: ExecutionResourceSlotDemandSnapshot,
        fresh_demand_observations: ExecutionResourceSlotDomains,
    ) {
        for domain in ExecutionResourceSlotDomain::ALL {
            if self.draining.contains(domain)
                && self.acknowledged_closures.contains(domain)
                && fresh_demand_observations.contains(domain)
                && demand.facts(domain).running == 0
            {
                self.draining.remove(domain);
                self.acknowledged_closures.remove(domain);
                self.domains[domain.index()].granted_at = None;
            }
        }
        if self.draining.is_empty() {
            self.pending_close_epoch = None;
            self.acknowledged_closures = ExecutionResourceSlotDomains::empty();
        }
    }

    fn begin_close_transition(&mut self) {
        let epoch = self.next_close_epoch;
        self.next_close_epoch = self.next_close_epoch.wrapping_add(1).max(1);
        self.pending_close_epoch = Some(epoch);
        // A new revocation invalidates partial acknowledgement of an older
        // close set. The next projection closes every still-draining domain
        // idempotently under the new token.
        self.acknowledged_closures = ExecutionResourceSlotDomains::empty();
    }

    fn adopt_untracked_running_domains(
        &mut self,
        demand: ExecutionResourceSlotDemandSnapshot,
        dispatch_allowed: bool,
        now: Instant,
        revoked: &mut ExecutionResourceSlotDomains,
    ) {
        for domain in ExecutionResourceSlotDomain::ALL {
            if demand.facts(domain).running == 0
                || self.active.contains(domain)
                || self.draining.contains(domain)
            {
                continue;
            }
            if dispatch_allowed && self.draining.is_empty() {
                self.grant(domain, now);
            } else {
                self.draining.insert(domain);
                revoked.insert(domain);
            }
        }
    }

    fn revoke_for_capacity(
        &mut self,
        capacity: usize,
        demand: ExecutionResourceSlotDemandSnapshot,
        revoked: &mut ExecutionResourceSlotDomains,
    ) {
        while self.active.len() > capacity {
            let victim = self.active.iter().min_by_key(|domain| {
                let facts = demand.facts(*domain);
                (
                    facts.running > 0,
                    facts.has_explicit_work(),
                    Reverse(self.domains[domain.index()].grant_serial),
                )
            });
            let Some(victim) = victim else {
                break;
            };
            self.revoke(victim, revoked);
        }
    }

    fn revoke_idle_domains(
        &mut self,
        demand: ExecutionResourceSlotDemandSnapshot,
        revoked: &mut ExecutionResourceSlotDomains,
    ) {
        for domain in ExecutionResourceSlotDomain::ALL {
            if self.active.contains(domain) && !demand.facts(domain).has_any_work() {
                self.revoke(domain, revoked);
            }
        }
    }

    fn preempt_automatic_grants_for_explicit_work(
        &mut self,
        capacity: usize,
        demand: ExecutionResourceSlotDemandSnapshot,
        revoked: &mut ExecutionResourceSlotDomains,
    ) {
        let explicit_waiters = ExecutionResourceSlotDomain::ALL
            .into_iter()
            .filter(|domain| {
                let facts = demand.facts(*domain);
                facts.has_waiting_work()
                    && facts.has_explicit_work()
                    && !self.active.contains(*domain)
                    && !self.draining.contains(*domain)
            })
            .count();
        let free_slots = capacity.saturating_sub(self.active.len());
        let mut preemptions_needed = explicit_waiters.saturating_sub(free_slots);

        while preemptions_needed > 0 {
            let victim = self.domains_from_cursor().find(|domain| {
                let facts = demand.facts(*domain);
                self.active.contains(*domain) && facts.running == 0 && !facts.has_explicit_work()
            });
            let Some(victim) = victim else {
                break;
            };
            self.revoke(victim, revoked);
            preemptions_needed -= 1;
        }
    }

    fn rotate_completed_or_stalled_grants(
        &mut self,
        capacity: usize,
        demand: ExecutionResourceSlotDemandSnapshot,
        terminal_advanced: ExecutionResourceSlotDomains,
        now: Instant,
        revoked: &mut ExecutionResourceSlotDomains,
    ) {
        let outside_waiters = ExecutionResourceSlotDomain::ALL
            .into_iter()
            .filter(|domain| {
                demand.facts(*domain).has_waiting_work()
                    && !self.active.contains(*domain)
                    && !self.draining.contains(*domain)
            })
            .count();
        let free_slots = capacity.saturating_sub(self.active.len());
        let mut rotations_needed = outside_waiters.saturating_sub(free_slots);

        while rotations_needed > 0 {
            let owner = self.domains_from_cursor().find(|domain| {
                if !self.active.contains(*domain) {
                    return false;
                }
                let terminal_boundary = terminal_advanced.contains(*domain);
                let grace_expired = demand.facts(*domain).running == 0
                    && self.domains[domain.index()]
                        .granted_at
                        .and_then(|granted_at| now.checked_duration_since(granted_at))
                        .is_some_and(|elapsed| elapsed >= self.grant_grace);
                terminal_boundary || grace_expired
            });
            let Some(owner) = owner else {
                break;
            };
            self.revoke(owner, revoked);
            rotations_needed -= 1;
        }
    }

    fn refresh_continuing_grants(
        &mut self,
        terminal_advanced: ExecutionResourceSlotDomains,
        now: Instant,
    ) {
        for domain in self.active.iter() {
            if terminal_advanced.contains(domain) {
                self.domains[domain.index()].granted_at = Some(now);
            }
        }
    }

    fn fill_available_slots(
        &mut self,
        capacity: usize,
        demand: ExecutionResourceSlotDemandSnapshot,
        now: Instant,
    ) {
        for explicit in [true, false] {
            while self.active.len() < capacity {
                let candidate = self.domains_from_cursor().find(|domain| {
                    let facts = demand.facts(*domain);
                    facts.has_waiting_work()
                        && facts.has_explicit_work() == explicit
                        && !self.active.contains(*domain)
                        && !self.draining.contains(*domain)
                });
                let Some(candidate) = candidate else {
                    break;
                };
                self.grant(candidate, now);
            }
        }
    }

    fn grant(&mut self, domain: ExecutionResourceSlotDomain, now: Instant) {
        self.active.insert(domain);
        let state = &mut self.domains[domain.index()];
        state.granted_at = Some(now);
        state.grant_serial = self.next_grant_serial;
        self.next_grant_serial = self.next_grant_serial.wrapping_add(1).max(1);
        self.next_domain_index = domain.successor_index();
    }

    fn revoke(
        &mut self,
        domain: ExecutionResourceSlotDomain,
        revoked: &mut ExecutionResourceSlotDomains,
    ) {
        self.active.remove(domain);
        revoked.insert(domain);
        self.domains[domain.index()].granted_at = None;
        self.next_domain_index = domain.successor_index();
        // Even an observation that reported `running == 0` is stale with
        // respect to the composition root's subsequent close-dispatch call:
        // a worker may cross the seam between those two operations. Every
        // revocation therefore enters the acknowledged two-phase drain.
        self.draining.insert(domain);
    }

    fn domains_from_cursor(&self) -> impl Iterator<Item = ExecutionResourceSlotDomain> + use<> {
        let cursor = self.next_domain_index;
        (0..EXECUTION_RESOURCE_SLOT_DOMAIN_COUNT).map(move |offset| {
            ExecutionResourceSlotDomain::ALL
                [(cursor + offset) % EXECUTION_RESOURCE_SLOT_DOMAIN_COUNT]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRACE: Duration = Duration::from_millis(100);

    fn queued(user_initiated: usize) -> ExecutionResourceSlotDomainFacts {
        ExecutionResourceSlotDomainFacts {
            queued: 1,
            user_initiated,
            ..ExecutionResourceSlotDomainFacts::default()
        }
    }

    fn input(
        now: Instant,
        capacity: usize,
        demand: ExecutionResourceSlotDemandSnapshot,
    ) -> ExecutionResourceSlotInput {
        ExecutionResourceSlotInput {
            now,
            machine_slot_capacity: capacity,
            dispatch_allowed: capacity > 0,
            demand,
            fresh_demand_observations: ExecutionResourceSlotDomains::all(),
        }
    }

    fn acknowledge_all_closures(
        allocator: &mut ExecutionResourceSlotAllocator,
        decision: ExecutionResourceSlotDecision,
    ) {
        let Some(epoch) = decision.close_epoch() else {
            assert!(decision.domains_to_close().is_empty());
            return;
        };
        for domain in decision.domains_to_close().iter() {
            assert!(allocator.acknowledge_close(epoch, domain));
        }
    }

    #[test]
    fn allocates_exactly_the_external_one_two_and_four_slot_capacities() {
        let now = Instant::now();
        let demand = ExecutionResourceSlotDemandSnapshot {
            proxy: queued(0),
            thumbnail: queued(0),
            waveform: queued(0),
            audio_warmup: queued(0),
            media_import: queued(0),
            media_asset_mutation: queued(0),
            export: queued(0),
        };

        for capacity in [1, 2, 4] {
            let mut allocator = ExecutionResourceSlotAllocator::new(GRACE);
            let decision = allocator.update(input(now, capacity, demand));
            assert_eq!(decision.allocation().len(), capacity);
        }
    }

    #[test]
    fn explicit_work_precedes_automatic_work_but_not_a_running_incumbent() {
        let now = Instant::now();
        let mut allocator = ExecutionResourceSlotAllocator::new(GRACE);
        let initial = allocator.update(input(
            now,
            1,
            ExecutionResourceSlotDemandSnapshot {
                proxy: queued(0),
                export: queued(1),
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));
        assert!(initial.allocation().admits(ExecutionResourceSlotDomain::Export));

        let mut sticky_allocator = ExecutionResourceSlotAllocator::new(GRACE);
        let granted = sticky_allocator.update(input(
            now,
            1,
            ExecutionResourceSlotDemandSnapshot {
                proxy: queued(0),
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));
        assert!(granted.allocation().admits(ExecutionResourceSlotDomain::Proxy));

        let sticky = sticky_allocator.update(input(
            now + Duration::from_millis(10),
            1,
            ExecutionResourceSlotDemandSnapshot {
                proxy: ExecutionResourceSlotDomainFacts { running: 1, ..queued(0) },
                export: queued(1),
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));
        assert!(sticky.allocation().admits(ExecutionResourceSlotDomain::Proxy));
        assert!(!sticky.allocation().admits(ExecutionResourceSlotDomain::Export));
    }

    #[test]
    fn automatic_domains_receive_slots_without_explicit_demand() {
        let now = Instant::now();
        let mut allocator = ExecutionResourceSlotAllocator::new(GRACE);
        let decision = allocator.update(input(
            now,
            2,
            ExecutionResourceSlotDemandSnapshot {
                thumbnail: queued(0),
                waveform: queued(0),
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));

        assert!(decision.allocation().admits(ExecutionResourceSlotDomain::Thumbnail));
        assert!(decision.allocation().admits(ExecutionResourceSlotDomain::Waveform));
    }

    #[test]
    fn continuous_import_yields_to_export_at_the_next_terminal_boundary() {
        let now = Instant::now();
        let mut allocator = ExecutionResourceSlotAllocator::new(GRACE);
        let initial_demand = ExecutionResourceSlotDemandSnapshot {
            media_import: queued(0),
            export: queued(0),
            ..ExecutionResourceSlotDemandSnapshot::default()
        };
        let first = allocator.update(input(now, 1, initial_demand));
        assert!(first.allocation().admits(ExecutionResourceSlotDomain::MediaImport));

        let rotated = allocator.update(input(
            now + Duration::from_millis(10),
            1,
            ExecutionResourceSlotDemandSnapshot {
                media_import: ExecutionResourceSlotDomainFacts {
                    terminal_generation: 1,
                    ..queued(0)
                },
                export: queued(0),
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));
        assert!(rotated.domains_to_close().contains(ExecutionResourceSlotDomain::MediaImport));
        assert!(rotated.domains_to_open_after_close().is_empty());
        assert!(!rotated.allocation().admits(ExecutionResourceSlotDomain::Export));
        acknowledge_all_closures(&mut allocator, rotated);

        let handed_off = allocator.update(input(
            now + Duration::from_millis(11),
            1,
            ExecutionResourceSlotDemandSnapshot {
                media_import: ExecutionResourceSlotDomainFacts {
                    terminal_generation: 1,
                    ..queued(0)
                },
                export: queued(0),
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));
        assert!(handed_off.allocation().admits(ExecutionResourceSlotDomain::Export));
    }

    #[test]
    fn a_worker_that_never_starts_loses_its_grant_after_bounded_grace() {
        let now = Instant::now();
        let mut allocator = ExecutionResourceSlotAllocator::new(GRACE);
        let first = allocator.update(input(
            now,
            1,
            ExecutionResourceSlotDemandSnapshot {
                proxy: queued(0),
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));
        assert!(first.allocation().admits(ExecutionResourceSlotDomain::Proxy));

        let before_expiry = allocator.update(input(
            now + GRACE - Duration::from_millis(1),
            1,
            ExecutionResourceSlotDemandSnapshot {
                proxy: queued(0),
                export: queued(0),
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));
        assert!(before_expiry.allocation().admits(ExecutionResourceSlotDomain::Proxy));

        let after_expiry = allocator.update(input(
            now + GRACE,
            1,
            ExecutionResourceSlotDemandSnapshot {
                proxy: queued(0),
                export: queued(0),
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));
        assert!(!after_expiry.allocation().admits(ExecutionResourceSlotDomain::Export));
        acknowledge_all_closures(&mut allocator, after_expiry);
        let handed_off = allocator.update(input(
            now + GRACE + Duration::from_millis(1),
            1,
            ExecutionResourceSlotDemandSnapshot {
                export: queued(0),
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));
        assert!(handed_off.allocation().admits(ExecutionResourceSlotDomain::Export));
    }

    #[test]
    fn capacity_shrink_keeps_one_incumbent_and_drains_the_rest() {
        let now = Instant::now();
        let mut allocator = ExecutionResourceSlotAllocator::new(GRACE);
        let queued_four = ExecutionResourceSlotDemandSnapshot {
            proxy: queued(0),
            thumbnail: queued(0),
            waveform: queued(0),
            audio_warmup: queued(0),
            ..ExecutionResourceSlotDemandSnapshot::default()
        };
        let first = allocator.update(input(now, 4, queued_four));
        assert_eq!(first.allocation().len(), 4);

        let running_four = ExecutionResourceSlotDemandSnapshot {
            proxy: ExecutionResourceSlotDomainFacts { running: 1, ..queued(0) },
            thumbnail: ExecutionResourceSlotDomainFacts { running: 1, ..queued(0) },
            waveform: ExecutionResourceSlotDomainFacts { running: 1, ..queued(0) },
            audio_warmup: ExecutionResourceSlotDomainFacts { running: 1, ..queued(0) },
            ..ExecutionResourceSlotDemandSnapshot::default()
        };
        let shrunk = allocator.update(input(now + Duration::from_millis(1), 1, running_four));
        assert_eq!(shrunk.allocation().len(), 1);
        assert_eq!(shrunk.revoked().len(), 3);
        assert_eq!(shrunk.draining().len(), 3);
    }

    #[test]
    fn a_draining_revocation_cannot_double_open_the_replacement() {
        let now = Instant::now();
        let mut allocator = ExecutionResourceSlotAllocator::new(GRACE);
        let _ = allocator.update(input(
            now,
            1,
            ExecutionResourceSlotDemandSnapshot {
                proxy: queued(0),
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));

        let draining = allocator.update(input(
            now + Duration::from_millis(1),
            1,
            ExecutionResourceSlotDemandSnapshot {
                proxy: ExecutionResourceSlotDomainFacts {
                    queued: 1,
                    running: 1,
                    terminal_generation: 1,
                    ..ExecutionResourceSlotDomainFacts::default()
                },
                export: queued(1),
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));
        assert!(draining.draining().contains(ExecutionResourceSlotDomain::Proxy));
        assert!(draining.domains_to_open_after_close().is_empty());
        assert!(!draining.allocation().admits(ExecutionResourceSlotDomain::Export));
        acknowledge_all_closures(&mut allocator, draining);

        let handed_off = allocator.update(input(
            now + Duration::from_millis(2),
            1,
            ExecutionResourceSlotDemandSnapshot {
                proxy: queued(0),
                export: queued(1),
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));
        assert!(handed_off.allocation().admits(ExecutionResourceSlotDomain::Export));
    }

    #[test]
    fn zero_capacity_pauses_and_a_later_capacity_restores_dispatch() {
        let now = Instant::now();
        let mut allocator = ExecutionResourceSlotAllocator::new(GRACE);
        let queued_demand = ExecutionResourceSlotDemandSnapshot {
            proxy: queued(0),
            ..ExecutionResourceSlotDemandSnapshot::default()
        };
        let admitted = allocator.update(input(now, 1, queued_demand));
        assert!(admitted.allocation().admits(ExecutionResourceSlotDomain::Proxy));

        let running_demand = ExecutionResourceSlotDemandSnapshot {
            proxy: ExecutionResourceSlotDomainFacts { running: 1, ..queued(0) },
            ..ExecutionResourceSlotDemandSnapshot::default()
        };
        let paused = allocator.update(input(now + Duration::from_millis(1), 0, running_demand));
        assert!(paused.allocation().is_empty());
        assert!(paused.draining().contains(ExecutionResourceSlotDomain::Proxy));
        assert!(paused.domains_to_close().contains(ExecutionResourceSlotDomain::Proxy));
        acknowledge_all_closures(&mut allocator, paused);

        let still_draining =
            allocator.update(input(now + Duration::from_millis(2), 1, running_demand));
        assert!(still_draining.allocation().is_empty());
        assert!(still_draining.domains_to_open_after_close().is_empty());

        let resumed = allocator.update(input(
            now + Duration::from_millis(3),
            1,
            ExecutionResourceSlotDemandSnapshot {
                proxy: ExecutionResourceSlotDomainFacts { terminal_generation: 1, ..queued(0) },
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));
        assert!(resumed.allocation().admits(ExecutionResourceSlotDomain::Proxy));
    }

    #[test]
    fn task_starting_between_snapshot_and_close_cannot_overlap_replacement() {
        let now = Instant::now();
        let mut allocator = ExecutionResourceSlotAllocator::new(GRACE);
        let initial = allocator.update(input(
            now,
            1,
            ExecutionResourceSlotDemandSnapshot {
                proxy: queued(0),
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));
        assert!(initial.allocation().admits(ExecutionResourceSlotDomain::Proxy));

        // The observation says the incumbent is idle, but it may start after
        // this snapshot and before the composition root applies the close.
        let close = allocator.update(input(
            now + GRACE,
            1,
            ExecutionResourceSlotDemandSnapshot {
                proxy: queued(0),
                export: queued(0),
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));
        assert!(close.allocation().is_empty());
        assert!(close.draining().contains(ExecutionResourceSlotDomain::Proxy));
        assert!(close.domains_to_open_after_close().is_empty());
        acknowledge_all_closures(&mut allocator, close);

        // The fresh post-close observation catches the racing incumbent.
        let raced = allocator.update(input(
            now + GRACE + Duration::from_millis(1),
            1,
            ExecutionResourceSlotDemandSnapshot {
                proxy: ExecutionResourceSlotDomainFacts { running: 1, ..queued(0) },
                export: queued(0),
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));
        assert!(raced.allocation().is_empty());
        assert!(raced.draining().contains(ExecutionResourceSlotDomain::Proxy));

        let drained = allocator.update(input(
            now + GRACE + Duration::from_millis(2),
            1,
            ExecutionResourceSlotDemandSnapshot {
                export: queued(0),
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));
        assert!(drained.allocation().admits(ExecutionResourceSlotDomain::Export));
    }

    #[test]
    fn global_dispatch_disallow_has_the_same_safe_pause_semantics_as_zero_capacity() {
        let now = Instant::now();
        let mut allocator = ExecutionResourceSlotAllocator::new(GRACE);
        let demand = ExecutionResourceSlotDemandSnapshot {
            export: queued(1),
            ..ExecutionResourceSlotDemandSnapshot::default()
        };
        let _ = allocator.update(input(now, 1, demand));

        let paused = allocator.update(ExecutionResourceSlotInput {
            now: now + Duration::from_millis(1),
            machine_slot_capacity: 4,
            dispatch_allowed: false,
            demand,
            fresh_demand_observations: ExecutionResourceSlotDomains::all(),
        });
        assert!(paused.allocation().is_empty());
        assert!(paused.domains_to_close().contains(ExecutionResourceSlotDomain::Export));
    }

    #[test]
    fn cached_zero_running_observation_cannot_complete_an_acknowledged_close() {
        let now = Instant::now();
        let mut allocator = ExecutionResourceSlotAllocator::new(GRACE);
        let demand = ExecutionResourceSlotDemandSnapshot {
            proxy: queued(0),
            ..ExecutionResourceSlotDemandSnapshot::default()
        };
        let admitted = allocator.update(input(now, 1, demand));
        assert!(admitted.allocation().admits(ExecutionResourceSlotDomain::Proxy));

        let close = allocator.update(input(now + GRACE, 0, demand));
        acknowledge_all_closures(&mut allocator, close);

        let cached = allocator.update(ExecutionResourceSlotInput {
            now: now + GRACE + Duration::from_millis(1),
            machine_slot_capacity: 1,
            dispatch_allowed: true,
            demand,
            fresh_demand_observations: ExecutionResourceSlotDomains::empty(),
        });
        assert!(cached.allocation().is_empty());
        assert!(cached.draining().contains(ExecutionResourceSlotDomain::Proxy));

        let fresh = allocator.update(input(now + GRACE + Duration::from_millis(2), 1, demand));
        assert!(fresh.allocation().admits(ExecutionResourceSlotDomain::Proxy));
    }

    #[test]
    fn untracked_running_work_under_closed_dispatch_receives_an_acknowledgeable_epoch() {
        let now = Instant::now();
        let mut allocator = ExecutionResourceSlotAllocator::new(GRACE);
        let decision = allocator.update(ExecutionResourceSlotInput {
            now,
            machine_slot_capacity: 0,
            dispatch_allowed: false,
            demand: ExecutionResourceSlotDemandSnapshot {
                export: ExecutionResourceSlotDomainFacts {
                    running: 1,
                    ..ExecutionResourceSlotDomainFacts::default()
                },
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
            fresh_demand_observations: ExecutionResourceSlotDomains::all(),
        });

        assert!(decision.domains_to_close().contains(ExecutionResourceSlotDomain::Export));
        let epoch = decision.close_epoch().expect("untracked running close epoch");
        assert!(allocator.acknowledge_close(epoch, ExecutionResourceSlotDomain::Export));
    }

    #[test]
    fn old_epoch_acknowledgements_cannot_release_an_expanded_close_set() {
        let now = Instant::now();
        let mut allocator = ExecutionResourceSlotAllocator::new(GRACE);
        let initial_demand = ExecutionResourceSlotDemandSnapshot {
            proxy: queued(0),
            thumbnail: queued(0),
            ..ExecutionResourceSlotDemandSnapshot::default()
        };
        let initial = allocator.update(input(now, 2, initial_demand));
        assert_eq!(initial.allocation().len(), 2);

        let first_close = allocator.update(input(
            now + Duration::from_millis(1),
            1,
            ExecutionResourceSlotDemandSnapshot {
                proxy: ExecutionResourceSlotDomainFacts { running: 1, ..queued(0) },
                thumbnail: ExecutionResourceSlotDomainFacts { running: 1, ..queued(0) },
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));
        let first_epoch = first_close.close_epoch().expect("first close epoch");
        let first_domain =
            first_close.domains_to_close().iter().next().expect("first revoked domain");
        assert!(allocator.acknowledge_close(first_epoch, first_domain));

        let expanded = allocator.update(input(
            now + Duration::from_millis(2),
            0,
            ExecutionResourceSlotDemandSnapshot {
                proxy: ExecutionResourceSlotDomainFacts { running: 1, ..queued(0) },
                thumbnail: ExecutionResourceSlotDomainFacts { running: 1, ..queued(0) },
                ..ExecutionResourceSlotDemandSnapshot::default()
            },
        ));
        let expanded_epoch = expanded.close_epoch().expect("expanded close epoch");
        assert_ne!(expanded_epoch, first_epoch);
        assert_eq!(expanded.domains_to_close().len(), 2);
        assert!(!allocator.acknowledge_close(first_epoch, first_domain));

        let second_domain = expanded
            .domains_to_close()
            .iter()
            .find(|domain| *domain != first_domain)
            .expect("second revoked domain");
        assert!(allocator.acknowledge_close(expanded_epoch, second_domain));
        let incomplete = allocator.update(input(now + Duration::from_millis(3), 2, initial_demand));
        assert!(incomplete.allocation().is_empty());
        assert!(incomplete.draining().contains(first_domain));

        assert!(allocator.acknowledge_close(expanded_epoch, first_domain));
        let complete = allocator.update(input(now + Duration::from_millis(4), 2, initial_demand));
        assert_eq!(complete.allocation().len(), 2);
    }
}
