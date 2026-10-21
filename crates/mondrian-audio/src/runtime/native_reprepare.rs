//! Retained restart capture owned by the actual installed Runtime.
//! Capture, preparation and one stopped same-child installation. Fresh Enter
//! remains a separate operation; none of these receipts permits DSP.
use super::*;
use crate::processor_host::native_reprepare::ReprepareSelection;
use crate::{NativeHostAllocationLedger, NativeHostAllocationOwner};
use std::num::NonZeroU64;

// Explicit first-capability host policy; longer native state is refused, never truncated.
const CONFIGURATION_LIMIT: usize = 64 * 1024;
const CAPTURE_OWNER_SLOTS: usize = 3;

/// Exact retained capture facts, not an installation lease or DSP permission.
/// Dropping this non-Clone value releases no Runtime candidate or native owner.
#[derive(Debug)]
pub struct NativeAudioReprepare {
    key: NonZeroU64,
    root: crate::AudioSessionCursorIdentity,
    facts: crate::NativeConfigurationSeedTicket,
    local_peak: crate::NativeHostLocalPeakSnapshot,
}
impl NativeAudioReprepare {
    /// Actual same-child configuration identity; this is not seed/install authority.
    pub const fn configuration_identity(&self) -> crate::NativeConfigurationSeedTicket {
        self.facts
    }
    /// Parent attempt Box, canonical-value Vec, decoded-state Vec and ledger peak.
    /// Child metadata scratch is a separate admitted main-inline lane obligation,
    /// not part of this parent-local snapshot. This is not complete peak coverage.
    pub const fn capture_local_peak(&self) -> crate::NativeHostLocalPeakSnapshot {
        self.local_peak
    }
}

/// Read-only preparation facts, not an author/Source/native installation handle.
/// All actual replacement owners remain private to the original held Runtime.
#[derive(Debug)]
pub struct NativeAudioRepreparePlanFacts {
    identity: crate::AudioSessionCursorIdentity,
    schedule: crate::PreparedAudioScheduleSummary,
    resources: crate::AudioSessionResourceFootprint,
}
impl NativeAudioRepreparePlanFacts {
    /// Actual newly allocated empty/unentered Session identity; no DSP permission.
    pub const fn identity(&self) -> crate::AudioSessionCursorIdentity {
        self.identity
    }
    /// Canonical schedule/PDC facts computed without creating processors.
    pub const fn schedule(&self) -> crate::PreparedAudioScheduleSummary {
        self.schedule
    }
    /// Original Session policy model for the private candidate, not whole-heap peak.
    pub const fn resources(&self) -> crate::AudioSessionResourceFootprint {
        self.resources
    }
}

/// Successful Source publication of a stopped replacement Session.
/// This non-Clone receipt retains retired owners until the caller consumes
/// `retire` outside Runtime, metadata and Source locks. It grants no PCM entry.
pub struct NativeAudioReprepareInstallReceipt {
    claim: mondrian_core::AudioOwnerInstallationClaim,
    identity: crate::AudioSessionCursorIdentity,
    previous_epoch: Option<crate::AudioContinuityEpoch>,
    key: NonZeroU64,
    _retired: RetiredReprepareOwners,
}

// Inline receipt storage, not another allocation/registry. Drop glue retires
// every old owner and preparation alias BEFORE the original logical fee.
struct RetiredReprepareOwners {
    _constants: Vec<Option<f64>>,
    _candidate: Option<crate::NativeConfigurationCandidate>,
    _factory: Option<Arc<dyn crate::AudioProcessorFactory>>,
    _plan: Option<Arc<PreparedAudioPlan>>,
    _session: Option<crate::render::PreparedAudioSessionStorage>,
    _claim: Option<mondrian_core::AudioOwnerInstallationClaim>,
    _verifier: Option<mondrian_core::AudioOwnerInstallationVerifier>,
    _destination: Option<mondrian_core::AudioOwnerInstallationVerifier>,
    _plan_fee: Option<super::native_author_logical_fees::LogicalPlanFee>,
}

/// Unforgeable evidence that this receipt's actual owners finished retirement.
/// It grants no Source, callback, parameter or DSP permission.
pub struct NativeAudioReprepareRetirement {
    key: NonZeroU64,
    identity: crate::AudioSessionCursorIdentity,
}

impl NativeAudioReprepareInstallReceipt {
    /// Actual newly published Source claim; no native binding is ready yet.
    pub fn claim(&self) -> &mondrian_core::AudioOwnerInstallationClaim {
        &self.claim
    }
    /// New empty/unentered Session containing the retained actual processors.
    pub const fn identity(&self) -> crate::AudioSessionCursorIdentity {
        self.identity
    }
    /// Consumer must explicitly select a fresh epoch, never infer a resume.
    pub const fn previous_epoch(&self) -> Option<crate::AudioContinuityEpoch> {
        self.previous_epoch
    }
    /// Consume on the existing retirement worker, outside all caller locks.
    /// If a destructor unwinds, no completion is issued and Root credit stays
    /// held. Merely dropping a receipt does not publish resource normalization.
    pub fn retire(self) -> NativeAudioReprepareRetirement {
        let key = self.key;
        let identity = self.identity;
        drop(self);
        NativeAudioReprepareRetirement { key, identity }
    }
}

/// A refused capture leaves the original exact Session blocked.
#[derive(Debug, thiserror::Error)]
pub enum NativeAudioReprepareError {
    /// The first supported domain or exact pending identity was not proven.
    #[error("native restart capture refused: {0}")]
    Admission(&'static str),
    /// Existing work cancelled; any actual captured owner stays on the Runtime.
    #[error("native restart capture cancelled")]
    Cancelled,
    /// Native object or Source installation authority changed.
    #[error(transparent)]
    Owner(#[from] crate::AudioNativeOwnerError),
    /// Existing native command failed, without author or Source publication.
    #[error(transparent)]
    Processor(#[from] crate::AudioProcessorHostError),
    /// A canonical prepared schedule could not identify this occurrence.
    #[error(transparent)]
    Compile(#[from] crate::AudioCompileError),
    /// Original empty Session allocator refused the candidate schedule.
    #[error(transparent)]
    Execution(#[from] crate::AudioExecutionError),
    /// Frozen root resource policy refused the actual capture overlap.
    #[error(transparent)]
    Resources(#[from] AudioRuntimeBuildError),
    /// Complete local allocation evidence could not be recorded.
    #[error(transparent)]
    Storage(#[from] crate::NativeHostLedgerError),
    /// Original immutable Factory inventory failed before any seed command.
    #[error(transparent)]
    FactoryStorage(#[from] crate::AudioProcessorFactoryStorageError),
    /// Adapter explicitly refused retained configuration capture.
    #[error("native restart configuration capability refused: {0:?}")]
    Capability(crate::NativeConfigurationRefusal),
    /// Another real native callback generation is pending.
    #[error("native restart cohort changed: {0:?}")]
    Cohort(Option<crate::NativeRequestCohort>),
    /// Stopped child refused the one explicit seed command; no retry is implied.
    #[error("native restart seed refused: {0:?}")]
    Seed(crate::NativeConfigurationSeedRefusal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapturePhase {
    Servicing,
    Captured,
    Preparing,
    Prepared,
    // Written BEFORE the RPC. Lost reply does not prove it was not executed.
    StageAttempted,
    SeedStaged,
    CommitAttempted,
    // Written immediately after authenticated Accepted, before any postcheck.
    SeedCommitted,
    InstalledAwaitingRetirement,
    Refused,
    Failed,
}

// These actual inline fields exist once in every Runtime, even before any
// capture. Their complete layout is charged by reserve_session, not by the
// later attempt Box/capture reservation.
pub(super) struct NativeReprepareState {
    pub(super) attempt: Option<Box<NativeReprepareAttempt>>,
    pub(super) next_attempt: u64,
    // Attribution of current execution-only Factory/seed/fee owners in the
    // ORIGINAL Root Fixed footprint, not a second grant or fee registry.
    pub(super) current_fixed_extra: usize,
}

pub(super) struct NativeReprepareAttempt {
    key: NonZeroU64,
    before_capture: AudioRuntimeResourceFootprint,
    factory_fixed_bound: usize,
    fee_scope_added: usize,
    retirement: Option<RestartRetirement>,
    ticket: crate::AudioProcessorReprepareTicket,
    serial: NonZeroU64,
    selection: ReprepareSelection,
    candidate: Option<crate::NativeConfigurationCandidate>,
    phase: CapturePhase,
    // Reserved before foreign work. An unexpectedly larger actual owner raises
    // this held charge even on refusal; no handle Drop can release it.
    held_fixed_bytes: usize,
    control_grant: usize,
    prepared_factory: Option<Arc<dyn crate::AudioProcessorFactory>>,
    prepared_plan: Option<Arc<PreparedAudioPlan>>,
    prepared_session: Option<crate::render::PreparedAudioSessionStorage>,
    destination: Option<mondrian_core::AudioOwnerInstallationVerifier>,
    staged_seed: Option<crate::NativeConfigurationSeedReceipt>,
    retired_claim: Option<mondrian_core::AudioOwnerInstallationClaim>,
    retired_verifier: Option<mondrian_core::AudioOwnerInstallationVerifier>,
    // Last: actual candidate/Factory/Plan/storage owners retire before credit.
    plan_fee: Option<super::native_author_logical_fees::LogicalPlanFee>,
}

#[derive(Clone, Copy)]
struct RestartRetirement {
    identity: crate::AudioSessionCursorIdentity,
    serial: NonZeroU64,
    high: AudioRuntimeResourceFootprint,
    final_footprint: AudioRuntimeResourceFootprint,
    fixed_extra: usize,
}

impl RestartRetirement {
    fn matches(
        self,
        key: NonZeroU64,
        phase: CapturePhase,
        current: crate::AudioSessionCursorIdentity,
        serial: NonZeroU64,
        footprint: AudioRuntimeResourceFootprint,
        completion: &NativeAudioReprepareRetirement,
    ) -> bool {
        phase == CapturePhase::InstalledAwaitingRetirement
            && key == completion.key
            && current == completion.identity
            && self.identity == completion.identity
            && footprint == self.high
            && serial == self.serial
    }
}

fn normalized_restart_footprint(
    mut before: AudioRuntimeResourceFootprint,
    old_extra: usize,
    old_session: crate::AudioSessionResourceFootprint,
    new_session: crate::AudioSessionResourceFootprint,
    new_extra: usize,
) -> Result<AudioRuntimeResourceFootprint, AudioRuntimeBuildError> {
    before.fixed_resident_bytes = before
        .fixed_resident_bytes
        .checked_sub(old_extra)
        .ok_or(AudioRuntimeBuildError::InvalidResourceReplacement)?;
    let mut next = super::native_author_scope::replace_weights(before, old_session, new_session)?;
    next.fixed_resident_bytes = next
        .fixed_resident_bytes
        .checked_add(new_extra)
        .ok_or(AudioRuntimeBuildError::InvalidResourceReplacement)?;
    Ok(next)
}

fn add(a: usize, b: usize) -> Result<usize, NativeAudioReprepareError> {
    a.checked_add(b)
        .ok_or_else(|| AudioRuntimeBuildError::InvalidResourceReplacement.into())
}

// Pure pre-service admission against the same actual Root grant. Returns
// (new fixed high water, incremental capture fee, child control byte grant).
// This reserves known byte buffers/parent owners, not all metadata or SDK heap.
fn check_capture_overlap(
    current: AudioRuntimeResourceFootprint,
    grant: AudioRuntimeResourceGrant,
    format: crate::NativeConfigurationFormat,
    state_limit: usize,
    values_bytes: usize,
) -> Result<(usize, usize, usize), NativeAudioReprepareError> {
    let child = format
        .capture_byte_buffer_bound(state_limit)
        .ok_or(AudioRuntimeBuildError::InvalidResourceReplacement)?;
    let control = add(child, values_bytes)?;
    let ledger = NativeHostAllocationLedger::storage_layout(CAPTURE_OWNER_SLOTS)?.bytes()?;
    let reserved = add(
        add(add(control, state_limit)?, values_bytes)?,
        add(std::mem::size_of::<NativeReprepareAttempt>(), ledger)?,
    )?;
    let fixed = add(current.fixed_resident_bytes, reserved)?;
    if fixed > grant.max_fixed_resident_bytes {
        return Err(AudioRuntimeBuildError::ResourceOverlapGrantExceeded {
            category: AudioRuntimeResourceCategory::FixedResidentBytes,
            required: fixed,
            granted: grant.max_fixed_resident_bytes,
        }
        .into());
    }
    Ok((fixed, reserved, control))
}

// The original Root grant/footprint (including H from the existing logical fee
// scope) is the only authority. This pure preflight runs before any new Factory,
// Plan, empty Session or fee-scope allocation; it cannot publish partial charges.
fn check_prepare_overlap(
    current: AudioRuntimeResourceFootprint,
    held_logical: usize,
    grant: AudioRuntimeResourceGrant,
    factory_bound: usize,
    session_bound: crate::AudioSessionResourceFootprint,
) -> Result<usize, AudioRuntimeBuildError> {
    let overflow = || AudioRuntimeBuildError::InvalidResourceReplacement;
    let fixed = current
        .fixed_resident_bytes
        .checked_add(factory_bound)
        .and_then(|bytes| bytes.checked_add(session_bound.fixed_resident_bytes))
        .ok_or_else(overflow)?;
    let logical = current
        .prepared_logical_bytes
        .checked_add(held_logical)
        .and_then(|bytes| bytes.checked_add(session_bound.prepared_logical_bytes))
        .ok_or_else(overflow)?;
    for (category, required, granted) in [
        (
            AudioRuntimeResourceCategory::FixedResidentBytes,
            fixed,
            grant.max_fixed_resident_bytes,
        ),
        (
            AudioRuntimeResourceCategory::PreparedLogicalBytes,
            logical,
            grant.max_prepared_logical_bytes,
        ),
    ] {
        if required > granted {
            return Err(AudioRuntimeBuildError::ResourceOverlapGrantExceeded {
                category,
                required,
                granted,
            });
        }
    }
    Ok(fixed)
}

// Additional one-owner child storage. The original lane already admits its
// decoded/reply/staged buffers; this conservative Root credit is NOT borrowed
// from the capture control grant. Successful exact retirement normalizes this
// transient peak; current committed seed attribution survives in the Root and
// replaces the prior selected seed on a later successful restart.
fn check_seed_overlap(
    current: AudioRuntimeResourceFootprint,
    grant: AudioRuntimeResourceGrant,
    bytes: usize,
) -> Result<usize, NativeAudioReprepareError> {
    if bytes > CONFIGURATION_LIMIT {
        return Err(NativeAudioReprepareError::Admission(
            "restart seed exceeds capture policy",
        ));
    }
    let fixed = add(current.fixed_resident_bytes, bytes)?;
    if fixed > grant.max_fixed_resident_bytes {
        return Err(AudioRuntimeBuildError::ResourceOverlapGrantExceeded {
            category: AudioRuntimeResourceCategory::FixedResidentBytes,
            required: fixed,
            granted: grant.max_fixed_resident_bytes,
        }
        .into());
    }
    Ok(fixed)
}

impl AudioProgramRuntime {
    /// Capture only the exact pending ticket already retained by this Session.
    /// The product notice is a rejection hint, never reconstructed authority.
    /// Physical output quiescence remains the caller's precondition.
    pub fn service_pending_native_reprepare(
        &mut self,
        expected: mondrian_core::NativeAudioReprepareNotice,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<NativeAudioReprepare, NativeAudioReprepareError> {
        if cancellation.is_canceled() {
            return Err(NativeAudioReprepareError::Cancelled);
        }
        let ticket =
            self.session
                .pending_reprepare_ticket()
                .ok_or(NativeAudioReprepareError::Admission(
                    "no actual Session restart is pending",
                ))?;
        if ticket.product_notice() != expected {
            return Err(NativeAudioReprepareError::Admission(
                "pending Session restart does not match the expected notice",
            ));
        }
        self.service_native_reprepare(ticket, cancellation)
    }

    /// Capture one exact retained CLAP or VST3 latency restart under the original root grant.
    /// Caller must have quiesced physical output; this Audio-only API cannot
    /// manufacture Media quiescence. No author edit, seed or new Plan is installed.
    ///
    /// The reservation covers the existing request/capture/envelope byte-buffer
    /// policy and the parent attempt Box, canonical-value Vec, decoded state Vec
    /// and local allocation-ledger storage. Successful and valid-schema-mismatch
    /// CLAP metadata reinspection uses one borrowed scan scratch instead of
    /// descriptor/port Vec/String containers; its actual layout is admitted in
    /// the existing worker main-inline lane obligation, separate from this fee.
    /// VST3 metadata query Vec/String owners and native stream COM wrappers are
    /// not part of this buffer reservation; it does not imply a full known-host
    /// control peak, whole heap or realtime qualification for that adapter.
    /// Cold error Strings, SDK storage and other owners still prevent a complete
    /// known-host control-peak or whole-service zero-allocation claim. A full
    /// canonical reprepare consumer must admit remaining owners before service,
    /// not after installation.
    pub fn service_native_reprepare(
        &mut self,
        ticket: crate::AudioProcessorReprepareTicket,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<NativeAudioReprepare, NativeAudioReprepareError> {
        use NativeAudioReprepareError as Error;
        if cancellation.is_canceled() {
            return Err(Error::Cancelled);
        }
        if self.native_reprepare.attempt.is_some() {
            return Err(Error::Admission(
                "an actual restart attempt is already retained",
            ));
        }
        if self.native_author_scope.is_some()
            || self.native_installation.prepared.is_some()
            || self.held_native_plan_logical_bytes() != 0
            || self
                .sources
                .entries
                .values()
                .any(|source| matches!(source, RuntimeSource::Nested(_)))
        {
            return Err(Error::Admission(
                "nested or another native handoff is retained",
            ));
        }
        let serial = self.native_author_installation_serial()?;
        let lanes = self.session.native_reprepare_lane_count(ticket)?;
        let values_bytes = std::alloc::Layout::array::<Option<f64>>(lanes)
            .map_err(|_| AudioRuntimeBuildError::InvalidResourceReplacement)?
            .size();
        let format =
            crate::NativeConfigurationFormat::for_retained_restart(ticket.request.request())
                .ok_or(Error::Admission("unsupported retained restart format"))?;
        let ledger_bytes =
            NativeHostAllocationLedger::storage_layout(CAPTURE_OWNER_SLOTS)?.bytes()?;
        let (reserved_fixed, reserved, control_grant) = check_capture_overlap(
            self.resource_footprint,
            self.resource_grant,
            format,
            CONFIGURATION_LIMIT,
            values_bytes,
        )?;
        let selection = self.session.inspect_native_reprepare(ticket, serial, cancellation)?;
        // Observation of peers may perform control I/O. Recheck the real Source
        // and cancellation before any service can stop/reactivate this child.
        if cancellation.is_canceled() {
            return Err(Error::Cancelled);
        }
        if self.native_author_installation_serial()? != serial {
            return Err(Error::Admission("Source changed during capture admission"));
        }
        if selection.constants.host_allocation()?.layout().size() > values_bytes {
            return Err(Error::Admission(
                "constant-profile actual capacity exceeds reservation",
            ));
        }
        let key = NonZeroU64::new(self.native_reprepare.next_attempt)
            .ok_or(Error::Admission("restart attempt serial exhausted"))?;
        self.native_reprepare.next_attempt = self
            .native_reprepare
            .next_attempt
            .checked_add(1)
            .ok_or(Error::Admission("restart attempt serial exhausted"))?;
        // The original Root footprint remains the sole fixed-policy authority.
        // Only exact successful installation retirement can normalize this
        // conservative reservation; failure/abandonment keeps it until Runtime
        // retirement. External handles own no allocations or releasable credit.
        let before_capture = self.resource_footprint;
        self.resource_footprint.fixed_resident_bytes = reserved_fixed;
        self.native_reprepare.attempt = Some(Box::new(NativeReprepareAttempt {
            key,
            before_capture,
            factory_fixed_bound: 0,
            fee_scope_added: 0,
            retirement: None,
            ticket,
            serial,
            selection,
            candidate: None,
            phase: CapturePhase::Servicing,
            held_fixed_bytes: reserved,
            control_grant,
            prepared_factory: None,
            prepared_plan: None,
            prepared_session: None,
            destination: None,
            staged_seed: None,
            retired_claim: None,
            retired_verifier: None,
            plan_fee: None,
        }));
        // This owner is installed BEFORE the first potentially destructive SDK
        // operation. Every error thereafter retains the actual attempt/charge.
        let attempt = self.native_reprepare.attempt.as_ref().expect("retained service owner");
        let result = self.session.service_native_reprepare(
            &attempt.selection,
            key,
            CONFIGURATION_LIMIT,
            control_grant,
        );
        let attempt = self.native_reprepare.attempt.as_mut().expect("same exclusive Runtime");
        match result {
            Ok(crate::NativeConfigurationControlOutcome::ReadyForInstall(candidate)) => {
                attempt.candidate = Some(candidate);
                attempt.phase = CapturePhase::Captured;
            }
            Ok(crate::NativeConfigurationControlOutcome::CapabilityRefused(reason)) => {
                attempt.phase = CapturePhase::Refused;
                return Err(Error::Capability(reason));
            }
            Ok(crate::NativeConfigurationControlOutcome::StillPending(cohort)) => {
                attempt.phase = CapturePhase::Refused;
                return Err(Error::Cohort(cohort));
            }
            Err(error) => {
                attempt.phase = CapturePhase::Failed;
                return Err(error);
            }
        }
        let attempt = self.native_reprepare.attempt.as_ref().expect("captured Runtime owner");
        let candidate =
            attempt.candidate.as_ref().expect("capture published no external authority");
        // Retain an upper charge before any fallible receipt construction. The
        // adapter's remote byte receipt is separate from these complete local
        // owners; no cross-process address union or early child credit is used.
        let remote = candidate.buffers.peak_bytes();
        let actual_peak = remote
            .and_then(|remote| remote.checked_add(candidate.configuration.capacity()))
            .and_then(|total| {
                total.checked_add(
                    attempt
                        .selection
                        .constants
                        .capacity()
                        .checked_mul(std::mem::size_of::<Option<f64>>())?,
                )
            })
            .and_then(|total| total.checked_add(std::mem::size_of::<NativeReprepareAttempt>()))
            .and_then(|total| total.checked_add(ledger_bytes));
        let attempt = self.native_reprepare.attempt.as_mut().unwrap();
        let held = reserved.max(actual_peak.unwrap_or(usize::MAX));
        self.resource_footprint.fixed_resident_bytes = self
            .resource_footprint
            .fixed_resident_bytes
            .saturating_add(held - attempt.held_fixed_bytes);
        attempt.held_fixed_bytes = held;
        let actual_peak = actual_peak.ok_or(AudioRuntimeBuildError::InvalidResourceReplacement)?;
        let remote = remote.ok_or(AudioRuntimeBuildError::InvalidResourceReplacement)?;
        let attempt = self.native_reprepare.attempt.as_ref().unwrap();
        let candidate = attempt.candidate.as_ref().unwrap();
        let mut ledger = NativeHostAllocationLedger::new(CAPTURE_OWNER_SLOTS, usize::MAX)?;
        ledger.record(self.native_reprepare.attempt.as_ref().unwrap().host_allocation()?)?;
        ledger.record(attempt.selection.constants.host_allocation()?)?;
        ledger.record(candidate.configuration.host_allocation()?)?;
        let local_peak = ledger.finish()?;
        debug_assert_eq!(
            Some(actual_peak),
            local_peak.allocation_bytes().checked_add(remote)
        );
        if candidate.serviced_cohort != attempt.selection.cohort
            || candidate.control_token != key
            || Some(candidate.format)
                != crate::NativeConfigurationFormat::for_retained_restart(
                    attempt.selection.cohort.request,
                )
            || candidate.configuration.len() > CONFIGURATION_LIMIT
            || candidate.configuration.capacity() > CONFIGURATION_LIMIT
            || candidate.parent_configuration_capacity_bytes != candidate.configuration.capacity()
            || remote > attempt.control_grant
            || actual_peak > reserved
            || !candidate.observed_contract.requires_state_entry()
            || !candidate.observed_contract.admits(self.session.contract().processing_mode)
        {
            return Err(Error::Admission(
                "actual captured identity, contract or capacity exceeded admission",
            ));
        }
        if cancellation.is_canceled() {
            return Err(Error::Cancelled);
        }
        if self.native_author_installation_serial()? != serial {
            return Err(Error::Admission("Source changed during native capture"));
        }
        Ok(NativeAudioReprepare {
            key,
            root: self.session.native_owner_identity(),
            facts: crate::NativeConfigurationSeedTicket::from_candidate(candidate),
            local_peak,
        })
    }

    /// Prepare a new execution-only Factory, canonical PDC schedule and empty
    /// Session for this actual retained capture. No create, seed RPC, Enter,
    /// author mutation or Source publication occurs. The current ticket remains
    /// HOLD. A wide original PDC contract may conservatively exceed the Root
    /// grant; no default budget is enlarged to admit this operation.
    ///
    /// Known codec/output/Factory owners and the original Session policy model
    /// are reserved before allocation. Path/Arc prefixes, allocator internals,
    /// cold error Strings and SDK storage remain outside a complete heap claim.
    pub fn prepare_native_reprepare_plan(
        &mut self,
        operation: &NativeAudioReprepare,
        destination: mondrian_core::AudioOwnerInstallationVerifier,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<NativeAudioRepreparePlanFacts, NativeAudioReprepareError> {
        use NativeAudioReprepareError as Error;
        if cancellation.is_canceled() {
            return Err(Error::Cancelled);
        }
        self.native_reprepare_configuration(operation)?;
        let attempt = self.native_reprepare.attempt.as_ref().unwrap();
        if attempt.phase != CapturePhase::Captured || attempt.prepared_factory.is_some() {
            return Err(Error::Admission(
                "restart Plan preparation already attempted",
            ));
        }
        if !self
            .native_installation
            .verifier
            .as_ref()
            .is_some_and(|old| old.same_source(&destination))
        {
            return Err(Error::Admission(
                "restart destination belongs to another Source",
            ));
        }
        let selection = &attempt.selection;
        self.session.validate_reprepare_capture(selection, cancellation)?;
        let attempt = self.native_reprepare.attempt.as_ref().unwrap();
        let candidate = attempt.candidate.as_ref().unwrap();
        let identity = crate::NativeConfigurationSeedTicket::from_candidate(candidate);
        let index = attempt.selection.index;
        let old_plan = self.session.shared_resource_plan();
        let factory_bound = old_plan.schedule.processors[index]
            .factory
            .native_reprepare_factory_bound(identity, candidate.configuration.len())
            .ok_or(Error::Admission(
                "execution-only restart Factory bound is unavailable",
            ))?;
        let session_bound = old_plan
            .native_reprepare_session_bound()
            .map_err(AudioRuntimeBuildError::from)?;
        // The existing lazy fee scope may allocate one AtomicUsize Arc payload;
        // include that known payload before calling the original fee helper.
        // Its std Arc prefix remains an explicit existing coverage boundary.
        let fee_scope_bytes = if self.native_held_plan_fees.is_none() {
            std::mem::size_of::<std::sync::atomic::AtomicUsize>()
        } else {
            0
        };
        let factory_and_fee_bound = add(factory_bound, fee_scope_bytes)?;
        let fixed_reservation = add(factory_and_fee_bound, session_bound.fixed_resident_bytes)?;
        let before_fixed = self.resource_footprint.fixed_resident_bytes;
        let reserved_fixed = check_prepare_overlap(
            self.resource_footprint,
            self.held_native_plan_logical_bytes(),
            self.resource_grant,
            factory_and_fee_bound,
            session_bound,
        )?;
        let old_logical = old_plan
            .session_resource_footprint()
            .map_err(AudioRuntimeBuildError::from)?
            .prepared_logical_bytes;
        let requested_logical = session_bound.prepared_logical_bytes;
        let mut next = self.resource_footprint;
        next.prepared_logical_bytes = next
            .prepared_logical_bytes
            .checked_sub(old_logical)
            .and_then(|bytes| bytes.checked_add(requested_logical))
            .ok_or(AudioRuntimeBuildError::InvalidResourceReplacement)?;
        if cancellation.is_canceled() {
            return Err(Error::Cancelled);
        }
        if self.native_author_installation_serial()?
            != self.native_reprepare.attempt.as_ref().unwrap().serial
        {
            return Err(Error::Admission("Source changed before restart allocation"));
        }
        let next_held_fixed = add(
            self.native_reprepare.attempt.as_ref().unwrap().held_fixed_bytes,
            fixed_reservation,
        )?;
        // Do NOT call reserve_session: this is another empty Session, not another
        // Runtime occurrence/inline state slot. The original Root remains sole
        // authority; conservative reservations survive failed preparation.
        self.resource_footprint.fixed_resident_bytes = reserved_fixed;
        let attempt = self.native_reprepare.attempt.as_mut().unwrap();
        attempt.held_fixed_bytes = next_held_fixed;
        attempt.phase = CapturePhase::Preparing;
        attempt.factory_fixed_bound = factory_bound;
        attempt.fee_scope_added = fee_scope_bytes;
        attempt.destination = Some(destination);
        let fee = self.prepare_native_plan_logical_fee(old_logical, requested_logical, next)?;
        let attempt = self.native_reprepare.attempt.as_mut().unwrap();
        attempt.plan_fee = Some(fee);
        attempt.plan_fee.as_mut().unwrap().enroll()?;
        let mut factory_peak = 0;
        let result = {
            let attempt = self.native_reprepare.attempt.as_ref().unwrap();
            let configuration = &attempt.candidate.as_ref().unwrap().configuration;
            self.session.resource_plan().schedule.processors[index]
                .factory
                .reprepare_factory(identity, configuration, &mut |bytes| {
                    factory_peak = factory_peak.max(bytes)
                })
        };
        // Adopt a formed Factory BEFORE cap/cancel/Source checks. On Err its
        // temporaries have already reported their peak; credit is not returned.
        let factory_result = match result {
            Ok(Some(factory)) => {
                self.native_reprepare.attempt.as_mut().unwrap().prepared_factory = Some(factory);
                Ok(())
            }
            Ok(None) => Err(Error::Admission("execution-only restart Factory refused")),
            Err(error) => Err(Error::Processor(error)),
        };
        let actual_fixed = factory_peak
            .checked_add(fee_scope_bytes)
            .and_then(|bytes| bytes.checked_add(session_bound.fixed_resident_bytes))
            .and_then(|bytes| bytes.checked_add(before_fixed))
            .unwrap_or(usize::MAX);
        if actual_fixed > self.resource_footprint.fixed_resident_bytes {
            let extra = actual_fixed - self.resource_footprint.fixed_resident_bytes;
            self.resource_footprint.fixed_resident_bytes = actual_fixed;
            let attempt = self.native_reprepare.attempt.as_mut().unwrap();
            attempt.held_fixed_bytes = attempt.held_fixed_bytes.saturating_add(extra);
        }
        if let Err(error) = factory_result {
            self.native_reprepare.attempt.as_mut().unwrap().phase = CapturePhase::Failed;
            return Err(error);
        }
        self.check_resource_ceiling(
            AudioRuntimeResourceCategory::FixedResidentBytes,
            self.resource_footprint.fixed_resident_bytes,
        )?;
        if factory_peak > factory_bound {
            return Err(Error::Admission(
                "actual restart Factory peak exceeded reservation",
            ));
        }
        if cancellation.is_canceled() {
            return Err(Error::Cancelled);
        }
        if self.native_author_installation_serial()?
            != self.native_reprepare.attempt.as_ref().unwrap().serial
        {
            return Err(Error::Admission(
                "Source changed during restart Factory preparation",
            ));
        }
        let factory = Arc::clone(
            self.native_reprepare
                .attempt
                .as_ref()
                .unwrap()
                .prepared_factory
                .as_ref()
                .unwrap(),
        );
        let result = self.session.resource_plan().prepare_native_reprepare_plan(index, factory);
        let plan = match result {
            Ok(plan) => Arc::new(plan),
            Err(error) => return Err(Error::Compile(error)),
        };
        self.native_reprepare.attempt.as_mut().unwrap().prepared_plan = Some(plan);
        let plan = self.native_reprepare.attempt.as_ref().unwrap().prepared_plan.as_ref().unwrap();
        let resources = plan.session_resource_footprint().map_err(AudioRuntimeBuildError::from)?;
        // An unexpected actual model excess is retained in the original charge
        // before refusal; no successful bound is inferred from a failed model.
        if resources.prepared_logical_bytes > requested_logical {
            self.resource_footprint.prepared_logical_bytes = self
                .resource_footprint
                .prepared_logical_bytes
                .saturating_add(resources.prepared_logical_bytes - requested_logical);
            return Err(Error::Admission(
                "actual restart Plan exceeded logical reservation",
            ));
        }
        if resources.fixed_resident_bytes > session_bound.fixed_resident_bytes {
            let extra = resources.fixed_resident_bytes - session_bound.fixed_resident_bytes;
            self.resource_footprint.fixed_resident_bytes =
                self.resource_footprint.fixed_resident_bytes.saturating_add(extra);
            let attempt = self.native_reprepare.attempt.as_mut().unwrap();
            attempt.held_fixed_bytes = attempt.held_fixed_bytes.saturating_add(extra);
            return Err(Error::Admission(
                "actual restart Session exceeded fixed reservation",
            ));
        }
        self.session.resource_plan().validate_native_reprepare_plan(plan, index)?;
        // The real Plan is already retained. Refine the SAME enrolled fee to
        // its checked actual policy amount before a future replacement can use
        // the original retirement equation; no owner or fee is dropped here.
        self.native_reprepare
            .attempt
            .as_mut()
            .unwrap()
            .plan_fee
            .as_mut()
            .unwrap()
            .shrink_prepared_to(resources.prepared_logical_bytes)?;
        let plan = self.native_reprepare.attempt.as_ref().unwrap().prepared_plan.as_ref().unwrap();
        let storage = AudioRenderSession::allocate_storage(Arc::clone(plan))?;
        self.native_reprepare.attempt.as_mut().unwrap().prepared_session = Some(storage);
        let attempt = self.native_reprepare.attempt.as_ref().unwrap();
        let storage = attempt.prepared_session.as_ref().unwrap();
        if !storage.is_empty_unentered() {
            return Err(Error::Admission(
                "restart storage is not empty and unentered",
            ));
        }
        let facts = NativeAudioRepreparePlanFacts {
            identity: storage.identity(),
            schedule: storage.plan().schedule_summary(),
            resources,
        };
        let mut actual_storage = 0_usize;
        let observed = storage.visit_storage(&mut |owner| {
            actual_storage = actual_storage
                .checked_add(owner.layout().size())
                .ok_or(crate::NativeHostLedgerError::ExtentOverflow)?;
            Ok(())
        });
        let actual_storage = if observed.is_ok() {
            actual_storage
        } else {
            usize::MAX
        };
        if actual_storage > session_bound.fixed_resident_bytes {
            let extra = actual_storage - session_bound.fixed_resident_bytes;
            self.resource_footprint.fixed_resident_bytes =
                self.resource_footprint.fixed_resident_bytes.saturating_add(extra);
            let attempt = self.native_reprepare.attempt.as_mut().unwrap();
            attempt.held_fixed_bytes = attempt.held_fixed_bytes.saturating_add(extra);
            return Err(Error::Admission(
                "actual empty Session storage exceeded reservation",
            ));
        }
        if cancellation.is_canceled() {
            return Err(Error::Cancelled);
        }
        let attempt = self.native_reprepare.attempt.as_ref().unwrap();
        self.session.validate_reprepare_capture(&attempt.selection, cancellation)?;
        if self.native_author_installation_serial()? != attempt.serial
            || self.session.pending_reprepare_ticket() != Some(attempt.ticket)
        {
            return Err(Error::Admission(
                "restart Source or ticket changed during preparation",
            ));
        }
        self.native_reprepare.attempt.as_mut().unwrap().phase = CapturePhase::Prepared;
        Ok(facts)
    }

    /// Stage/commit the exact stopped child seed and publish its prepared Session
    /// using the original one-shot Source authorization. Physical output must be
    /// quiescent at the real Media caller; this Audio API supplies no such proof.
    ///
    /// A first Stage consumes the attempt even on transport error. Commit errors
    /// are ambiguous and never retried: the child may already have committed.
    /// Old Session/ticket, actual owners and conservative fees remain held on
    /// every failure. Successful publication still requires explicit Fresh Enter.
    /// Drop the returned retirement receipt outside all caller-held locks.
    pub fn install_native_reprepare_plan(
        &mut self,
        operation: &NativeAudioReprepare,
        authorization: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<NativeAudioReprepareInstallReceipt, NativeAudioReprepareError> {
        use NativeAudioReprepareError as Error;
        if cancellation.is_canceled() {
            return Err(Error::Cancelled);
        }
        self.native_reprepare_configuration(operation)?;
        if self.native_reprepare.attempt.as_ref().unwrap().phase != CapturePhase::Prepared {
            return Err(Error::Admission(
                "restart installation already attempted or not prepared",
            ));
        }
        let mut attempt = self.native_reprepare.attempt.take().unwrap();
        let result: Result<_, Error> = (|| {
            if self.session.pending_reprepare_ticket() != Some(attempt.ticket)
                || self.session.native_owner_identity() != operation.root
                || self.native_author_installation_serial()? != attempt.serial
                || crate::NativeConfigurationSeedTicket::from_candidate(
                    attempt.candidate.as_ref().unwrap(),
                ) != operation.facts
            {
                return Err(Error::Admission("restart installation identity changed"));
            }
            self.session.validate_reprepare_capture(&attempt.selection, cancellation)?;
            let old = self
                .session
                .current_resource_footprint()
                .map_err(AudioRuntimeBuildError::from)?;
            let storage = attempt
                .prepared_session
                .as_ref()
                .ok_or(Error::Admission("restart Session was not prepared"))?;
            let next =
                storage.current_resource_footprint().map_err(AudioRuntimeBuildError::from)?;
            let mut footprint =
                super::native_author_scope::replace_weights(self.resource_footprint, old, next)?;
            // Both old and new storage already have conservative Fixed credit.
            // Do not release old storage under the CAS or charge new storage twice.
            footprint.fixed_resident_bytes = self.resource_footprint.fixed_resident_bytes;
            let seed = operation.facts;
            let mut factory_bytes = 0_usize;
            // Successful Factory owners use the existing inventory/kinds.
            // Alias double-count is conservative. Arc payload/encoded capacity
            // is not upgraded to complete prefix/platform/SDK heap evidence.
            attempt.prepared_plan.as_ref().unwrap().visit_factory_host_owned_storage(
                &mut |owner| {
                    factory_bytes = factory_bytes
                        .checked_add(owner.layout().size())
                        .ok_or(crate::NativeHostLedgerError::ExtentOverflow)?;
                    Ok(())
                },
            )?;
            let (old_seeds, selected_seed) =
                self.session.reprepare_seed_weights(attempt.selection.index)?;
            let next_seeds = old_seeds
                .checked_sub(selected_seed)
                .and_then(|bytes| bytes.checked_add(seed.configuration_bytes))
                .ok_or(AudioRuntimeBuildError::InvalidResourceReplacement)?;
            let fee_scope = std::mem::size_of::<std::sync::atomic::AtomicUsize>();
            let fixed_extra = add(add(factory_bytes, next_seeds)?, fee_scope)?;
            let final_footprint = normalized_restart_footprint(
                attempt.before_capture,
                self.native_reprepare.current_fixed_extra,
                old,
                next,
                fixed_extra,
            )?;
            // The original FB already admits the selected new Factory/codec.
            // Whole-Plan peer/binding metadata not covered by FB gets credit
            // now, BEFORE Stage, while all old owners remain alive.
            let metadata_extra = factory_bytes.saturating_sub(attempt.factory_fixed_bound);
            let scope_extra =
                if self.native_reprepare.current_fixed_extra == 0 && attempt.fee_scope_added == 0 {
                    fee_scope
                } else {
                    0
                };
            let mut preseed = self.resource_footprint;
            preseed.fixed_resident_bytes = add(
                preseed.fixed_resident_bytes,
                add(metadata_extra, scope_extra)?,
            )?;
            let reserved =
                check_seed_overlap(preseed, self.resource_grant, seed.configuration_bytes)?;
            if final_footprint.fixed_resident_bytes > reserved {
                return Err(Error::Admission(
                    "restart final owners exceed admitted peak",
                ));
            }
            let mut final_roles = final_footprint;
            final_roles.fixed_resident_bytes = footprint.fixed_resident_bytes;
            if final_roles != footprint {
                return Err(Error::Admission("restart final resource roles changed"));
            }
            let held = add(
                attempt.held_fixed_bytes,
                add(seed.configuration_bytes, add(metadata_extra, scope_extra)?)?,
            )?;
            let destination = attempt
                .destination
                .as_ref()
                .ok_or(Error::Admission("restart destination was not prepared"))?
                .clone();
            self.validate_native_plan_logical_fee(attempt.plan_fee.as_ref().unwrap())?;
            // Precheck the actual C-old+A relation before the first seed command.
            self.prepare_native_plan_fee_retirement(attempt.plan_fee.as_ref().unwrap(), footprint)?;
            if cancellation.is_canceled() {
                return Err(Error::Cancelled);
            }
            if self.native_author_installation_serial()? != attempt.serial {
                return Err(Error::Admission("Source changed before seed stage"));
            }
            self.resource_footprint.fixed_resident_bytes = reserved;
            attempt.held_fixed_bytes = held;
            footprint.fixed_resident_bytes = reserved;
            attempt.phase = CapturePhase::StageAttempted;
            let outcome = self.session.stage_reprepare_seed(
                &attempt.selection,
                crate::NativeConfigurationSeedRequest {
                    ticket: seed,
                    configuration: &attempt.candidate.as_ref().unwrap().configuration,
                    seed_capacity_grant_bytes: seed.configuration_bytes,
                },
            )?;
            let staged = match outcome {
                crate::NativeConfigurationSeedOutcome::Accepted(receipt) => {
                    // Retain visible actual ownership before identity/cap checks.
                    attempt.staged_seed = Some(receipt);
                    if receipt.child_seed_capacity_bytes > seed.configuration_bytes {
                        let extra = receipt.child_seed_capacity_bytes - seed.configuration_bytes;
                        self.resource_footprint.fixed_resident_bytes =
                            self.resource_footprint.fixed_resident_bytes.saturating_add(extra);
                        attempt.held_fixed_bytes = attempt.held_fixed_bytes.saturating_add(extra);
                    }
                    receipt
                }
                crate::NativeConfigurationSeedOutcome::Refused(reason) => {
                    return Err(Error::Seed(reason))
                }
            };
            // Wire Refused carries no capacity: this does not claim visibility
            // of rejected temporary allocator peaks or complete SDK heap usage.
            self.check_resource_ceiling(
                AudioRuntimeResourceCategory::FixedResidentBytes,
                self.resource_footprint.fixed_resident_bytes,
            )?;
            if staged.ticket != seed || staged.child_seed_capacity_bytes > seed.configuration_bytes
            {
                return Err(Error::Admission(
                    "actual staged seed exceeded its exact grant",
                ));
            }
            attempt.phase = CapturePhase::SeedStaged;
            self.session.validate_reprepare_capture(&attempt.selection, cancellation)?;
            if cancellation.is_canceled() {
                return Err(Error::Cancelled);
            }
            if self.native_author_installation_serial()? != attempt.serial {
                return Err(Error::Admission("Source changed before seed commit"));
            }
            attempt.phase = CapturePhase::CommitAttempted;
            let outcome = self.session.commit_reprepare_seed(&attempt.selection, seed)?;
            match outcome {
                crate::NativeConfigurationSeedOutcome::Accepted(receipt) => {
                    // This MUST precede every late validation, cancellation or
                    // CAS. An Err below can never grant a second Commit RPC.
                    attempt.phase = CapturePhase::SeedCommitted;
                    if receipt.child_seed_capacity_bytes > seed.configuration_bytes {
                        let extra = receipt.child_seed_capacity_bytes - seed.configuration_bytes;
                        self.resource_footprint.fixed_resident_bytes =
                            self.resource_footprint.fixed_resident_bytes.saturating_add(extra);
                        attempt.held_fixed_bytes = attempt.held_fixed_bytes.saturating_add(extra);
                    }
                    if receipt != staged {
                        return Err(Error::Admission("committed seed receipt changed"));
                    }
                }
                crate::NativeConfigurationSeedOutcome::Refused(reason) => {
                    return Err(Error::Seed(reason))
                }
            }
            let storage = attempt.prepared_session.as_mut().unwrap();
            storage.validate_reprepare_transfer(
                &mut self.session,
                &attempt.selection,
                seed.observed_contract,
                cancellation,
            )?;
            if cancellation.is_canceled() {
                return Err(Error::Cancelled);
            }
            if self.native_author_installation_serial()? != attempt.serial
                || self.session.pending_reprepare_ticket() != Some(attempt.ticket)
            {
                return Err(Error::Admission(
                    "Source or held ticket changed after seed commit",
                ));
            }
            let retirement = self.prepare_native_plan_fee_retirement(
                attempt.plan_fee.as_ref().unwrap(),
                footprint,
            )?;
            let (committed_seeds, _) =
                self.session.reprepare_seed_weights(attempt.selection.index)?;
            let actual_extra = add(add(factory_bytes, committed_seeds)?, fee_scope)?;
            let final_footprint = normalized_restart_footprint(
                attempt.before_capture,
                self.native_reprepare.current_fixed_extra,
                old,
                next,
                actual_extra,
            )?;
            if final_footprint.fixed_resident_bytes > footprint.fixed_resident_bytes {
                return Err(Error::Admission(
                    "actual restart owners exceed admitted peak",
                ));
            }
            let identity = storage.identity();
            let next_verifier = destination.clone();
            let commit = authorization
                .begin_commit(&destination)
                .map_err(crate::AudioNativeOwnerError::from)?;
            // No RPC, allocation, fallible work or retired owner Drop from here
            // to commit. The actual Hosted records move once into empty storage.
            storage.swap_retained(&mut self.session);
            self.resource_footprint = footprint;
            attempt.plan_fee.as_mut().unwrap().retire(retirement);
            attempt.retired_verifier = self.native_installation.verifier.replace(next_verifier);
            let claim = commit.commit();
            attempt.retired_claim = self.native_installation.claim.replace(claim.clone());
            attempt.phase = CapturePhase::InstalledAwaitingRetirement;
            attempt.retirement = Some(RestartRetirement {
                identity,
                serial: claim.generation(),
                high: footprint,
                final_footprint,
                fixed_extra: actual_extra,
            });
            // Outside the Source guard; new Session remains Unentered.
            self.refresh_native_entry_context();
            Ok(claim)
        })();
        let result = match result {
            Ok(claim) => {
                let receipt = NativeAudioReprepareInstallReceipt {
                    claim,
                    identity: self.session.native_owner_identity(),
                    previous_epoch: attempt.ticket.continuity_epoch,
                    key: attempt.key,
                    _retired: RetiredReprepareOwners {
                        _constants: std::mem::take(&mut attempt.selection.constants),
                        _candidate: attempt.candidate.take(),
                        _factory: attempt.prepared_factory.take(),
                        _plan: attempt.prepared_plan.take(),
                        _session: attempt.prepared_session.take(),
                        _claim: attempt.retired_claim.take(),
                        _verifier: attempt.retired_verifier.take(),
                        _destination: attempt.destination.take(),
                        _plan_fee: attempt.plan_fee.take(),
                    },
                };
                Ok(receipt)
            }
            Err(error) => Err(error),
        };
        // On success this SAME Box is now only a metadata fence. No second
        // capture can start until real retirement and exact completion below.
        // On failure it still holds all actual owners/phase and conservative fees.
        self.native_reprepare.attempt = Some(attempt);
        result
    }

    /// Complete resource normalization only after the receipt's real lock-free
    /// retirement. An unrelated/newer Session, Source claim or footprint refuses
    /// without credit. This operation grants no DSP or native owner permission.
    pub fn finish_native_reprepare_retirement(
        &mut self,
        completion: NativeAudioReprepareRetirement,
    ) -> Result<(), NativeAudioReprepareError> {
        use NativeAudioReprepareError as Error;
        let attempt = self
            .native_reprepare
            .attempt
            .as_ref()
            .ok_or(Error::Admission("no restart retirement is pending"))?;
        let retired = attempt
            .retirement
            .ok_or(Error::Admission("restart owners have not been published"))?;
        if !retired.matches(
            attempt.key,
            attempt.phase,
            self.session.native_owner_identity(),
            self.native_author_installation_serial()?,
            self.resource_footprint,
            &completion,
        ) || attempt.candidate.is_some()
            || attempt.prepared_factory.is_some()
            || attempt.prepared_plan.is_some()
            || attempt.prepared_session.is_some()
            || attempt.plan_fee.is_some()
            || attempt.retired_claim.is_some()
            || attempt.retired_verifier.is_some()
            || attempt.destination.is_some()
            || !attempt.selection.constants.is_empty()
        {
            return Err(Error::Admission(
                "stale or foreign restart retirement completion",
            ));
        }
        // The fence Box was in the original capture R. Destroy its now-empty
        // metadata allocation BEFORE publishing Final, which does not contain it.
        drop(self.native_reprepare.attempt.take());
        self.resource_footprint = retired.final_footprint;
        self.native_reprepare.current_fixed_extra = retired.fixed_extra;
        Ok(())
    }

    /// Borrow only the current Runtime's retained captured bytes. No allocation,
    /// native call, author permission or physical-output evidence is granted.
    pub fn native_reprepare_configuration<'a>(
        &'a self,
        operation: &NativeAudioReprepare,
    ) -> Result<&'a [u8], NativeAudioReprepareError> {
        use NativeAudioReprepareError as Error;
        let attempt = self
            .native_reprepare
            .attempt
            .as_ref()
            .ok_or(Error::Admission("no retained restart capture"))?;
        if attempt.key != operation.key
            || operation.root != self.session.native_owner_identity()
            || !matches!(
                attempt.phase,
                CapturePhase::Captured
                    | CapturePhase::Preparing
                    | CapturePhase::Prepared
                    | CapturePhase::StageAttempted
                    | CapturePhase::SeedStaged
                    | CapturePhase::CommitAttempted
                    | CapturePhase::SeedCommitted
                    | CapturePhase::Failed
            )
            || self.session.pending_reprepare_ticket() != Some(attempt.ticket)
            || self.native_author_installation_serial()? != attempt.serial
        {
            return Err(Error::Admission("stale or foreign restart capture handle"));
        }
        Ok(&attempt
            .candidate
            .as_ref()
            .ok_or(Error::Admission("capture unavailable"))?
            .configuration)
    }
}

#[cfg(test)]
mod inline_admission_tests {
    use super::*;

    fn session(fixed: usize) -> AudioSessionResourceFootprint {
        AudioSessionResourceFootprint {
            fixed_resident_bytes: fixed,
            prepared_logical_bytes: 11,
            ..AudioSessionResourceFootprint::default()
        }
    }

    #[test]
    fn vst3_capture_buffer_admission_uses_same_root_credit_before_rpc() {
        let current = AudioRuntimeResourceFootprint {
            runtime_occurrences: 1,
            fixed_resident_bytes: 17,
            prepared_logical_bytes: 23,
            ..AudioRuntimeResourceFootprint::default()
        };
        let n = CONFIGURATION_LIMIT;
        let v = std::mem::size_of::<Option<f64>>();
        let parent = std::mem::size_of::<NativeReprepareAttempt>()
            + NativeHostAllocationLedger::storage_layout(CAPTURE_OWNER_SLOTS)
                .unwrap()
                .bytes()
                .unwrap();
        for (format, multiplier) in [
            (crate::NativeConfigurationFormat::ClapState, 2),
            (
                crate::NativeConfigurationFormat::Vst3ComponentAndController,
                4,
            ),
        ] {
            let reserved = multiplier * n + 2 * v + parent;
            let required = current.fixed_resident_bytes + reserved;
            let grant = AudioRuntimeResourceGrant::new(1, required, 23);
            assert_eq!(
                check_capture_overlap(current, grant, format, n, v).unwrap(),
                (required, reserved, (multiplier - 1) * n + v)
            );
            assert!(
                matches!(check_capture_overlap(current,AudioRuntimeResourceGrant::new(1,required-1,23),format,n,v),
                Err(NativeAudioReprepareError::Resources(AudioRuntimeBuildError::ResourceOverlapGrantExceeded {
                    category:AudioRuntimeResourceCategory::FixedResidentBytes,required:actual,granted,
                })) if actual==required && granted==required-1)
            );
            let mut overflow = current;
            overflow.fixed_resident_bytes = usize::MAX;
            assert!(matches!(
                check_capture_overlap(
                    overflow,
                    AudioRuntimeResourceGrant::unbounded(),
                    format,
                    n,
                    v
                ),
                Err(NativeAudioReprepareError::Resources(
                    AudioRuntimeBuildError::InvalidResourceReplacement
                ))
            ));
        }
        assert!(matches!(
            check_capture_overlap(
                current,
                AudioRuntimeResourceGrant::unbounded(),
                crate::NativeConfigurationFormat::Vst3ComponentAndController,
                usize::MAX / 3 + 1,
                v
            ),
            Err(NativeAudioReprepareError::Resources(
                AudioRuntimeBuildError::InvalidResourceReplacement
            ))
        ));
        assert!(check_capture_overlap(
            current,
            AudioRuntimeResourceGrant::unbounded(),
            crate::NativeConfigurationFormat::ClapState,
            n,
            usize::MAX
        )
        .is_err());
        assert_eq!(current.fixed_resident_bytes, 17);
        assert_eq!(current.prepared_logical_bytes, 23);
        assert_eq!(current.runtime_occurrences, 1);
        // Production pure preflight only: no candidate, ticket, fee, native
        // command or mutable owner is created by these exact/one-byte controls.
    }

    #[test]
    fn restart_prepare_overlap_admits_exact_root_policy_and_refuses_one_byte_or_overflow() {
        let current = AudioRuntimeResourceFootprint {
            runtime_occurrences: 1,
            fixed_resident_bytes: 17,
            prepared_logical_bytes: 23,
            ..AudioRuntimeResourceFootprint::default()
        };
        let candidate = crate::AudioSessionResourceFootprint {
            fixed_resident_bytes: 31,
            prepared_logical_bytes: 37,
            ..crate::AudioSessionResourceFootprint::default()
        };
        let held = 5;
        let factory = 41;
        let fixed = current.fixed_resident_bytes + factory + candidate.fixed_resident_bytes;
        let logical = current.prepared_logical_bytes + held + candidate.prepared_logical_bytes;
        assert_eq!(
            check_prepare_overlap(
                current,
                held,
                AudioRuntimeResourceGrant::new(1, fixed, logical),
                factory,
                candidate
            )
            .unwrap(),
            fixed
        );
        assert!(
            matches!(check_prepare_overlap(current, held, AudioRuntimeResourceGrant::new(1, fixed - 1, logical), factory, candidate),
            Err(AudioRuntimeBuildError::ResourceOverlapGrantExceeded { category: AudioRuntimeResourceCategory::FixedResidentBytes, required, granted }) if required == fixed && granted == fixed - 1)
        );
        assert!(
            matches!(check_prepare_overlap(current, held, AudioRuntimeResourceGrant::new(1, fixed, logical - 1), factory, candidate),
            Err(AudioRuntimeBuildError::ResourceOverlapGrantExceeded { category: AudioRuntimeResourceCategory::PreparedLogicalBytes, required, granted }) if required == logical && granted == logical - 1)
        );
        assert!(matches!(
            check_prepare_overlap(
                current,
                held,
                AudioRuntimeResourceGrant::new(1, usize::MAX, usize::MAX),
                usize::MAX,
                candidate
            ),
            Err(AudioRuntimeBuildError::InvalidResourceReplacement)
        ));
        let mut overflowing = current;
        overflowing.prepared_logical_bytes = usize::MAX;
        assert!(matches!(
            check_prepare_overlap(
                overflowing,
                held,
                AudioRuntimeResourceGrant::new(1, usize::MAX, usize::MAX),
                factory,
                candidate
            ),
            Err(AudioRuntimeBuildError::InvalidResourceReplacement)
        ));
        assert_eq!(current.runtime_occurrences, 1);
        assert_eq!(current.fixed_resident_bytes, 17);
        assert_eq!(current.prepared_logical_bytes, 23);
        assert_eq!(candidate.fixed_resident_bytes, 31);
        // This is the production pure admission seam, not a native refusal or
        // allocator-peak test. It creates no candidate, fee or Runtime owner.
    }

    #[test]
    fn restart_seed_overlap_uses_exact_root_fixed_credit_before_any_command() {
        let current = AudioRuntimeResourceFootprint {
            runtime_occurrences: 1,
            fixed_resident_bytes: 17,
            prepared_logical_bytes: 23,
            ..AudioRuntimeResourceFootprint::default()
        };
        assert_eq!(
            check_seed_overlap(current, AudioRuntimeResourceGrant::new(1, 23, 23), 6).unwrap(),
            23
        );
        assert!(matches!(
            check_seed_overlap(current, AudioRuntimeResourceGrant::new(1, 22, 23), 6),
            Err(NativeAudioReprepareError::Resources(
                AudioRuntimeBuildError::ResourceOverlapGrantExceeded {
                    category: AudioRuntimeResourceCategory::FixedResidentBytes,
                    required: 23,
                    granted: 22,
                }
            ))
        ));
        assert_eq!(
            check_seed_overlap(current, AudioRuntimeResourceGrant::new(1, 17, 23), 0).unwrap(),
            17
        );
        let mut overflow = current;
        overflow.fixed_resident_bytes = usize::MAX;
        assert!(matches!(
            check_seed_overlap(overflow, AudioRuntimeResourceGrant::unbounded(), 1),
            Err(NativeAudioReprepareError::Resources(
                AudioRuntimeBuildError::InvalidResourceReplacement
            ))
        ));
        assert!(matches!(
            check_seed_overlap(
                current,
                AudioRuntimeResourceGrant::unbounded(),
                CONFIGURATION_LIMIT + 1
            ),
            Err(NativeAudioReprepareError::Admission(_))
        ));
        // Exact production pure preflight, not a native RPC refusal claim.
        assert_eq!(current.fixed_resident_bytes, 17);
        assert_eq!(current.prepared_logical_bytes, 23);
        assert_eq!(current.runtime_occurrences, 1);
    }

    #[test]
    fn restart_retirement_normalizes_two_cycles_without_reusing_old_extra_credit() {
        let mut first = session(40);
        first.prepared_logical_bytes = 10;
        let mut second = first;
        second.fixed_resident_bytes = 60;
        second.prepared_logical_bytes = 14;
        let mut third = second;
        third.fixed_resident_bytes = 80;
        third.prepared_logical_bytes = 18;
        let before = AudioRuntimeResourceFootprint {
            runtime_occurrences: 1,
            fixed_resident_bytes: 100,
            prepared_logical_bytes: 10,
            ..AudioRuntimeResourceFootprint::default()
        };
        let one = normalized_restart_footprint(before, 0, first, second, 26).unwrap();
        assert_eq!(one.fixed_resident_bytes, 146);
        assert_eq!(one.prepared_logical_bytes, 14);
        let two = normalized_restart_footprint(one, 26, second, third, 30).unwrap();
        assert_eq!(two.fixed_resident_bytes, (100 - 40) + 80 + 30);
        assert_eq!(two.prepared_logical_bytes, 18);
        assert_eq!(two.runtime_occurrences, 1);
        assert!(normalized_restart_footprint(before, 101, first, second, 26).is_err());
        assert!(normalized_restart_footprint(before, 0, first, second, usize::MAX).is_err());
        assert_eq!(before.fixed_resident_bytes, 100);
        // Real production role algebra, not a native two-restart claim.
    }

    #[test]
    fn restart_retirement_completion_matches_exact_owner_phase_claim_and_highwater() {
        let key = NonZeroU64::new(1).unwrap();
        let other = NonZeroU64::new(2).unwrap();
        let identity = crate::AudioSessionCursorIdentity {
            prepared_plan_id: mondrian_core::AudioPreparedPlanId::new(),
            session_id: mondrian_core::AudioRenderSessionId::new(),
        };
        let footprint = AudioRuntimeResourceFootprint {
            runtime_occurrences: 1,
            fixed_resident_bytes: 100,
            ..AudioRuntimeResourceFootprint::default()
        };
        let fence = RestartRetirement {
            identity,
            serial: key,
            high: footprint,
            final_footprint: footprint,
            fixed_extra: 0,
        };
        let completion = NativeAudioReprepareRetirement { key, identity };
        assert!(fence.matches(
            key,
            CapturePhase::InstalledAwaitingRetirement,
            identity,
            key,
            footprint,
            &completion
        ));
        assert!(!fence.matches(
            other,
            CapturePhase::InstalledAwaitingRetirement,
            identity,
            key,
            footprint,
            &completion
        ));
        assert!(!fence.matches(
            key,
            CapturePhase::SeedCommitted,
            identity,
            key,
            footprint,
            &completion
        ));
        let mut foreign = identity;
        foreign.session_id = mondrian_core::AudioRenderSessionId::new();
        assert!(!fence.matches(
            key,
            CapturePhase::InstalledAwaitingRetirement,
            foreign,
            key,
            footprint,
            &completion
        ));
        assert!(!fence.matches(
            key,
            CapturePhase::InstalledAwaitingRetirement,
            identity,
            other,
            footprint,
            &completion
        ));
        let mut changed = footprint;
        changed.fixed_resident_bytes += 1;
        assert!(!fence.matches(
            key,
            CapturePhase::InstalledAwaitingRetirement,
            identity,
            key,
            changed,
            &completion
        ));
        assert_eq!(
            footprint.fixed_resident_bytes, 100,
            "matching is read-only; no stale completion can calculate a credit"
        );
    }

    #[test]
    fn inline_slots_use_runtime_fixed_grant_once_per_occurrence() {
        let sequence = SequenceId::new();
        let output = ProgramOutputId::new();
        let inline = std::mem::size_of::<NativeReprepareState>();
        assert!(inline > 0);
        let original = session(7);
        let one = original.fixed_resident_bytes + inline;
        let mut exact =
            AudioRuntimeAdmissionLedger::new(AudioRuntimeResourceGrant::new(2, 2 * one, 22));
        for count in 1..=2 {
            exact.reserve_session(sequence, output, original).unwrap();
            assert_eq!(exact.footprint.runtime_occurrences, count);
            assert_eq!(exact.footprint.fixed_resident_bytes, count * one);
            assert_eq!(exact.footprint.prepared_logical_bytes, count * 11);
        }
        // A one-byte deficit is rejected at either the first occurrence or a
        // second occurrence after a completed sibling. No partial ledger publish.
        for count in 1..=2 {
            let required = count * one;
            let mut short = AudioRuntimeAdmissionLedger::new(AudioRuntimeResourceGrant::new(
                2,
                required - 1,
                22,
            ));
            for _ in 1..count {
                short.reserve_session(sequence, output, original).unwrap();
            }
            let before = short.footprint;
            assert!(matches!(short.reserve_session(sequence, output, original),
                Err(AudioRuntimeBuildError::ResourceGrantExceeded {
                    category: AudioRuntimeResourceCategory::FixedResidentBytes,
                    sequence_id, output_id, required: actual, granted,
                }) if sequence_id == sequence && output_id == output
                    && actual == required && granted == required - 1));
            assert_eq!(short.footprint, before);
        }
        assert_eq!(
            original.fixed_resident_bytes, 7,
            "the Session base is unchanged"
        );
    }

    #[test]
    fn inline_slot_overflow_and_session_replacement_keep_root_accounting() {
        let sequence = SequenceId::new();
        let output = ProgramOutputId::new();
        let mut overflow = AudioRuntimeAdmissionLedger::new(AudioRuntimeResourceGrant::unbounded());
        let before = overflow.footprint;
        assert!(
            matches!(overflow.reserve_session(sequence, output, session(usize::MAX)),
            Err(AudioRuntimeBuildError::ResourceExtentOverflow {
                category: AudioRuntimeResourceCategory::FixedResidentBytes, sequence_id, output_id,
            }) if sequence_id == sequence && output_id == output)
        );
        assert_eq!(overflow.footprint, before);

        let inline = std::mem::size_of::<NativeReprepareState>();
        let old = session(7);
        let next = session(13);
        let mut ledger = AudioRuntimeAdmissionLedger::new(AudioRuntimeResourceGrant::unbounded());
        ledger.reserve_session(sequence, output, old).unwrap();
        // Existing native/parameter replacement substitutes Session storage only;
        // it must retain this independently owned Runtime slot in the Root total.
        let replaced =
            super::super::native_author_scope::replace_weights(ledger.footprint, old, next)
                .unwrap();
        assert_eq!(
            replaced.fixed_resident_bytes,
            next.fixed_resident_bytes + inline
        );
        assert_eq!(replaced.runtime_occurrences, 1);
        assert_eq!(replaced.prepared_logical_bytes, next.prepared_logical_bytes);
        assert_eq!(
            ledger.footprint.fixed_resident_bytes,
            old.fixed_resident_bytes + inline
        );
    }
}
