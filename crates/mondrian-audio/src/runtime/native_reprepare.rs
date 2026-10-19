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
    /// Reported parent capture subset, including local ledger bookkeeping only.
    pub const fn capture_local_peak(&self) -> crate::NativeHostLocalPeakSnapshot {
        self.local_peak
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
    Refused,
    Failed,
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
}

fn add(a: usize, b: usize) -> Result<usize, NativeAudioReprepareError> {
    a.checked_add(b)
        .ok_or_else(|| AudioRuntimeBuildError::InvalidResourceReplacement.into())
}

impl AudioProgramRuntime {
    /// Capture one exact retained CLAP restart under the original root grant.
    /// Caller must have quiesced physical output; this Audio-only API cannot
    /// manufacture Media quiescence. No author edit, seed or new Plan is installed.
    pub fn service_native_reprepare(
        &mut self,
        ticket: crate::AudioProcessorReprepareTicket,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<NativeAudioReprepare, NativeAudioReprepareError> {
        use NativeAudioReprepareError as Error;
        if cancellation.is_canceled() {
            return Err(Error::Cancelled);
        }
        if self.native_reprepare.is_some() {
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
        let key = NonZeroU64::new(self.native_reprepare_next_attempt)
            .ok_or(Error::Admission("restart attempt serial exhausted"))?;
        self.native_reprepare_next_attempt = self
            .native_reprepare_next_attempt
            .checked_add(1)
            .ok_or(Error::Admission("restart attempt serial exhausted"))?;
        let control_grant = add(CONFIGURATION_LIMIT, values_bytes)?;
        // The original Root footprint remains the sole fixed-policy authority.
        // This conservative reservation is not credited until the actual Runtime
        // retires; external handles own no allocations or releasable credit.
        self.resource_footprint.fixed_resident_bytes = reserved_fixed;
        self.native_reprepare = Some(Box::new(NativeReprepareAttempt {
            key,
            ticket,
            serial,
            selection,
            candidate: None,
            phase: CapturePhase::Servicing,
            held_fixed_bytes: reserved,
            control_grant,
        }));
        // This owner is installed BEFORE the first potentially destructive SDK
        // operation. Every error thereafter retains the actual attempt/charge.
        let attempt = self.native_reprepare.as_ref().expect("retained service owner");
        let result = self.session.service_native_reprepare(
            &attempt.selection,
            key,
            CONFIGURATION_LIMIT,
            control_grant,
        );
        let attempt = self.native_reprepare.as_mut().expect("same exclusive Runtime");
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
        let attempt = self.native_reprepare.as_ref().expect("captured Runtime owner");
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
        let attempt = self.native_reprepare.as_mut().unwrap();
        let held = reserved.max(actual_peak.unwrap_or(usize::MAX));
        self.resource_footprint.fixed_resident_bytes = self
            .resource_footprint
            .fixed_resident_bytes
            .saturating_add(held - attempt.held_fixed_bytes);
        attempt.held_fixed_bytes = held;
        let actual_peak = actual_peak.ok_or(AudioRuntimeBuildError::InvalidResourceReplacement)?;
        let remote = remote.ok_or(AudioRuntimeBuildError::InvalidResourceReplacement)?;
        let attempt = self.native_reprepare.as_ref().unwrap();
        let candidate = attempt.candidate.as_ref().unwrap();
        let mut ledger = NativeHostAllocationLedger::new(CAPTURE_OWNER_SLOTS, usize::MAX)?;
        ledger.record(self.native_reprepare.as_ref().unwrap().host_allocation()?)?;
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

    /// Borrow only the current Runtime's retained captured bytes. No allocation,
    /// native call, author permission or physical-output evidence is granted.
    pub fn native_reprepare_configuration<'a>(
        &'a self,
        operation: &NativeAudioReprepare,
    ) -> Result<&'a [u8], NativeAudioReprepareError> {
        use NativeAudioReprepareError as Error;
        let attempt = self
            .native_reprepare
            .as_ref()
            .ok_or(Error::Admission("no retained restart capture"))?;
        if attempt.key != operation.key
            || operation.root != self.session.native_owner_identity()
            || attempt.phase != CapturePhase::Captured
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
