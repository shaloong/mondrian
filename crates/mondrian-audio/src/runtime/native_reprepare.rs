//! Retained restart capture owned by the actual installed Runtime.
//! This first consumer never commits a seed, publishes a Source or resumes DSP.
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
    /// Adapter explicitly refused retained configuration capture.
    #[error("native restart configuration capability refused: {0:?}")]
    Capability(crate::NativeConfigurationRefusal),
    /// Another real native callback generation is pending.
    #[error("native restart cohort changed: {0:?}")]
    Cohort(Option<crate::NativeRequestCohort>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapturePhase {
    Servicing,
    Captured,
    Preparing,
    Prepared,
    Refused,
    Failed,
}

// These actual inline fields exist once in every Runtime, even before any
// capture. Their complete layout is charged by reserve_session, not by the
// later attempt Box/capture reservation.
pub(super) struct NativeReprepareState {
    pub(super) attempt: Option<Box<NativeReprepareAttempt>>,
    pub(super) next_attempt: u64,
}

pub(super) struct NativeReprepareAttempt {
    key: NonZeroU64,
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
    // Last: actual candidate/Factory/Plan/storage owners retire before credit.
    plan_fee: Option<super::native_author_logical_fees::LogicalPlanFee>,
}

fn add(a: usize, b: usize) -> Result<usize, NativeAudioReprepareError> {
    a.checked_add(b)
        .ok_or_else(|| AudioRuntimeBuildError::InvalidResourceReplacement.into())
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

impl AudioProgramRuntime {
    /// Capture one exact retained CLAP restart under the original root grant.
    /// Caller must have quiesced physical output; this Audio-only API cannot
    /// manufacture Media quiescence. No author edit, seed or new Plan is installed.
    ///
    /// The reservation covers the existing request/capture/envelope byte-buffer
    /// policy and the parent attempt Box, canonical-value Vec, decoded state Vec
    /// and local allocation-ledger storage. Successful and valid-schema-mismatch
    /// CLAP metadata reinspection uses one borrowed scan scratch instead of
    /// descriptor/port Vec/String containers; its actual layout is admitted in
    /// the existing worker main-inline lane obligation, separate from this fee.
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
        let values_bytes = std::alloc::Layout::array::<f64>(lanes)
            .map_err(|_| AudioRuntimeBuildError::InvalidResourceReplacement)?
            .size();
        let ledger_bytes =
            NativeHostAllocationLedger::storage_layout(CAPTURE_OWNER_SLOTS)?.bytes()?;
        let reserved = add(
            add(
                add(
                    add(CONFIGURATION_LIMIT, CONFIGURATION_LIMIT)?,
                    add(values_bytes, values_bytes)?,
                )?,
                std::mem::size_of::<NativeReprepareAttempt>(),
            )?,
            ledger_bytes,
        )?;
        let reserved_fixed = add(self.resource_footprint.fixed_resident_bytes, reserved)?;
        self.check_resource_ceiling(
            AudioRuntimeResourceCategory::FixedResidentBytes,
            reserved_fixed,
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
        let control_grant = add(CONFIGURATION_LIMIT, values_bytes)?;
        // The original Root footprint remains the sole fixed-policy authority.
        // This conservative reservation is not credited until the actual Runtime
        // retires; external handles own no allocations or releasable credit.
        self.resource_footprint.fixed_resident_bytes = reserved_fixed;
        self.native_reprepare.attempt = Some(Box::new(NativeReprepareAttempt {
            key,
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
                        .checked_mul(std::mem::size_of::<f64>())?,
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
            || candidate.format != crate::NativeConfigurationFormat::ClapState
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
