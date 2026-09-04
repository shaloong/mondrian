//! Raw lifetime evidence for the Media-owned native decoder startup lane.

/// Exact startup worker and move-only request inventory at consuming shutdown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AudioDecoderStartupShutdownEvidence {
    /// Whether this decoder requires the product native startup lane.
    pub required: bool,
    /// Whether startup worker creation was attempted.
    pub attempted: bool,
    /// Startup workers whose handles were returned.
    pub workers_started: u32,
    /// Startup workers synchronously joined by consuming shutdown.
    pub workers_joined: u32,
    /// Worker creation failures; no synchronous fallback is permitted.
    pub start_failures: u32,
    /// Startup execution or supervision panics.
    pub panics: u32,
    /// Joined workers without a terminal publication.
    pub publication_missing: u32,
    /// Foreign errors, panic payloads or owners deliberately abandoned.
    pub owner_abandonments: u32,
    /// Native owners whose state could not be verified after a panic.
    pub unverified_native_owners: usize,
    /// Move-only requests accepted by the startup lane.
    pub requests_admitted: u64,
    /// Results transferred to the read caller for install or explicit retirement.
    pub requests_claimed: u64,
    /// Unclaimed requests disposed by their owning completion envelope.
    pub requests_retired: u64,
    /// Requests canceled before native process creation was attempted.
    pub canceled_before_spawn: u64,
    /// Queued requests still owned at observation.
    pub queued_remaining: usize,
    /// Native startup operations still in flight at observation.
    pub in_flight_remaining: usize,
    /// Published results still awaiting claim or disposal at observation.
    pub unclaimed_results_remaining: usize,
    /// Request producer leases not yet installed or retired.
    pub producers_remaining: usize,
}

impl AudioDecoderStartupShutdownEvidence {
    /// Whether the required worker and all admitted request owners closed exactly.
    pub const fn all_resources_released(self) -> bool {
        let worker_closed = if self.required {
            self.attempted && self.workers_started == 1 && self.workers_joined == 1
        } else {
            !self.attempted
                && self.workers_started == 0
                && self.workers_joined == 0
                && self.requests_admitted == 0
        };
        worker_closed
            && self.start_failures == 0
            && self.panics == 0
            && self.publication_missing == 0
            && self.owner_abandonments == 0
            && self.unverified_native_owners == 0
            && self.requests_claimed <= self.requests_admitted
            && self.requests_retired == self.requests_admitted - self.requests_claimed
            && self.canceled_before_spawn <= self.requests_retired + self.requests_claimed
            && self.queued_remaining == 0
            && self.in_flight_remaining == 0
            && self.unclaimed_results_remaining == 0
            && self.producers_remaining == 0
    }
}
