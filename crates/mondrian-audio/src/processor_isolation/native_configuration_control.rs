//! Non-realtime retained-native configuration work, separate from plan installation.

use crate::{AudioProcessorExecutionContract, AudioProcessorHostError};
use mondrian_core::NativeAudioReprepareRequest;
use std::num::NonZeroU64;

/// Exact serial and recorded request flags from one exclusive native owner.
///
/// A cohort is only meaningful with the processor object that produced it. It is
/// not an author-instance identity or an occurrence/session installation ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeRequestCohort {
    /// Every nonempty callback increments this bounded serial.
    pub serial: u32,
    /// Coalesced native request, including VST3 bits unknown to the host.
    pub request: NativeAudioReprepareRequest,
}

impl NativeRequestCohort {
    /// Validate one nonempty recorded native request generation.
    pub fn new(
        serial: u32,
        request: NativeAudioReprepareRequest,
    ) -> Result<Self, AudioProcessorHostError> {
        if serial == 0
            || matches!(
                request,
                NativeAudioReprepareRequest::Vst3Restart { flags: 0 }
            )
        {
            return Err(AudioProcessorHostError::InvalidContract(
                "native request cohort is empty".into(),
            ));
        }
        Ok(Self { serial, request })
    }

    /// Sticky exhaustion cannot be acknowledged or wrapped into a new cohort.
    pub const fn exhausted(self) -> bool {
        self.serial == u32::MAX
    }
}

/// One pending fact from its original owner; a bare request has no cohort identity.
///
/// This is observation only, never permission to service, retire or resume DSP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeObservedRequest {
    /// A generic adapter or retained phase exposes no actual callback serial.
    RequestOnly(NativeAudioReprepareRequest),
    /// Exact callback generation observed on the same native owner.
    Cohort(NativeRequestCohort),
}

impl NativeObservedRequest {
    /// Diagnostic/product-notice projection; it does not establish cohort equality.
    pub const fn request(self) -> NativeAudioReprepareRequest {
        match self {
            Self::RequestOnly(request) => request,
            Self::Cohort(cohort) => cohort.request,
        }
    }
}

/// Borrowed non-RT command for the exact retained processor object.
///
/// The caller must quiesce admitted DSP/output before servicing. Constant values
/// occupy the full prepared lane order, in canonical definition coordinates; the
/// caller must establish that every present canonical curve is constant. None
/// lanes remain native-owned. This command does not evaluate curves or authorize
/// a new plan, PDC or Clock Master.
pub struct NativeConfigurationControlRequest<'a> {
    /// Non-consuming snapshot expected by the caller.
    pub expected_cohort: NativeRequestCohort,
    /// Distinguishes this service result from any other attempted installation.
    pub control_token: NonZeroU64,
    /// Full ordered ownership profile: None preserves native configuration;
    /// Some is an explicit canonical constant, never a sampled controller value.
    pub canonical_constant_values: &'a [Option<f64>],
    /// Upper bound on the returned canonical native configuration bytes.
    pub configuration_limit_bytes: usize,
    /// Additional request/capture/envelope byte buffers, excluding retained owners.
    pub control_buffer_grant_bytes: usize,
}

/// Typed capability refusals leave the native owner blocked and requests pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeConfigurationRefusal {
    /// The native adapter does not provide this retained-control capability.
    UnsupportedAdapter,
    /// The bounded serial space is exhausted and cannot be safely retired.
    SerialExhausted,
    /// Structural, parameter-schema or unknown flags need a broader rebuild.
    UnsupportedRequest,
    /// Exact constant scalar configuration is unavailable or not representable.
    UnsupportedConstantProfile,
    /// Activated buses or structural parameter metadata differ from admission.
    MetadataChanged,
    /// The native state extension does not expose reusable configuration.
    ConfigurationUnavailable,
    /// The native state writer exceeded its admitted fixed extent.
    ConfigurationTooLarge,
    /// Additional host byte buffers cannot fit the explicit control grant.
    BufferGrantExceeded,
    /// An already granted fixed host allocation could not be obtained.
    ///
    /// This is temporary host resource refusal, separate from an insufficient
    /// byte grant or missing plugin state capability; the child remains retained.
    HostAllocationUnavailable,
    /// This child already holds a serviced candidate and cannot process old PCM.
    AwaitingInstallation,
}

/// Native configuration codec; state bytes are configuration, never a DSP checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeConfigurationFormat {
    /// Canonical VST3 host component/controller envelope.
    Vst3ComponentAndController,
    /// Bytes written by CLAP PluginState.
    ClapState,
}

impl NativeConfigurationFormat {
    // Exact first-capability ABI/request mapping, not observed-instance authority.
    pub(crate) const fn for_retained_restart(request: NativeAudioReprepareRequest) -> Option<Self> {
        match request {
            NativeAudioReprepareRequest::ClapRestart => Some(Self::ClapState),
            NativeAudioReprepareRequest::Vst3Restart { flags: 8 } => {
                Some(Self::Vst3ComponentAndController)
            }
            _ => None,
        }
    }

    // Requested capture/envelope byte buffers only, excluding request values.
    // VST3 has at most two state streams plus one canonical envelope. Using 3N
    // avoids depending on the private SDK header size or single-component form.
    pub(crate) const fn capture_byte_buffer_bound(self, state_limit: usize) -> Option<usize> {
        match self {
            Self::ClapState => Some(state_limit),
            Self::Vst3ComponentAndController => state_limit.checked_mul(3),
        }
    }
}

/// Additional byte-buffer evidence; not a complete native resource admission.
///
/// Descriptors, retained payloads, COM/port/bus structures and vendor-private
/// allocations still require the full canonical owner ledger before installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeControlBufferReceipt {
    /// Child request values allocation, if owned by the control decoder.
    pub request_value_capacity_bytes: usize,
    /// Fixed native stream payload buffers while the state writer runs.
    pub capture_payload_bytes: usize,
    /// Actual capacity of the transferable state envelope/buffer.
    pub configuration_capacity_bytes: usize,
}

impl NativeControlBufferReceipt {
    /// Checked peak of the additional byte-buffer owners within the child.
    pub fn peak_bytes(self) -> Option<usize> {
        self.request_value_capacity_bytes
            .checked_add(self.capture_payload_bytes)?
            .checked_add(self.configuration_capacity_bytes)
    }
}

/// Observed retained-component candidate awaiting a separate exact installation.
#[derive(Debug, PartialEq, Eq)]
pub struct NativeConfigurationCandidate {
    /// Exact consumed cohort; subsequent callbacks remain independently pending.
    pub serviced_cohort: NativeRequestCohort,
    /// The control attempt that produced this candidate.
    pub control_token: NonZeroU64,
    /// Real latency/tail facts queried after legal activation in the unchanged mode.
    pub observed_contract: AudioProcessorExecutionContract,
    /// Identifies the state codec without guessing from opaque bytes.
    pub format: NativeConfigurationFormat,
    /// Transferred owned state allocation; not cloned during service publication.
    pub configuration: Vec<u8>,
    /// Additional capture/request/envelope owners and overlapping extent.
    pub buffers: NativeControlBufferReceipt,
    /// Parent decoded state allocation; zero for a child-local/direct candidate.
    ///
    /// Child and parent live in different processes and must be attributed to
    /// different accounting owners, even when their state bytes are identical.
    pub parent_configuration_capacity_bytes: usize,
}

/// Non-RT result; none of these variants unblocks DSP or installs a prepared plan.
#[derive(Debug, PartialEq, Eq)]
pub enum NativeConfigurationControlOutcome {
    /// Native component is activated but stopped; old contract remains blocked.
    ReadyForInstall(NativeConfigurationCandidate),
    /// Legally observed while inactive; no activation or mapping migration yet.
    ClapAudioInputsChanged(super::ClapAudioInputProposal),
    /// A newer callback invalidated the expected cohort; no automatic retry follows.
    StillPending(Option<NativeRequestCohort>),
    /// This bounded service cannot safely provide the requested capability.
    CapabilityRefused(NativeConfigurationRefusal),
}

/// Worker-local phase independent of authored generations and Undo/dirty state.
pub(super) enum NativeConfigurationPhase {
    /// Already admitted root-window work still follows its original contract.
    Admitted,
    /// A non-RT service has started; no further old-contract DSP is legal.
    Blocked(NativeAudioReprepareRequest),
    /// An exact inactive input observation remains owned by the stopped child.
    AudioInputsProposed(super::ClapAudioInputProposal),
    /// Replacement inputs are owned but inactive and not bound to a shared mapping.
    AudioInputsStaged(super::ClapAudioInputProposal),
    /// A complete serviced candidate exists, awaiting exact plan installation.
    ReadyForInstall {
        request: NativeAudioReprepareRequest,
        control_token: NonZeroU64,
        candidate: super::native_configuration_seed::ReadyNativeConfiguration,
    },
}

impl NativeConfigurationPhase {
    pub(super) fn issued(candidate: &NativeConfigurationCandidate) -> Self {
        Self::ReadyForInstall {
            request: candidate.serviced_cohort.request,
            control_token: candidate.control_token,
            candidate: super::native_configuration_seed::ReadyNativeConfiguration::issued(
                candidate,
            ),
        }
    }

    pub(super) fn seed_ticket(&self) -> Option<super::NativeConfigurationSeedTicket> {
        match self {
            Self::ReadyForInstall { candidate, .. } => Some(candidate.ticket()),
            _ => None,
        }
    }

    pub(super) fn stage_seed(
        &mut self,
        seed: super::NativeConfigurationOwnedSeed,
    ) -> super::NativeConfigurationSeedOutcome {
        match self {
            Self::ReadyForInstall { candidate, .. } => candidate.stage(seed),
            _ => super::NativeConfigurationSeedOutcome::Refused(
                super::NativeConfigurationSeedRefusal::StaleTicket,
            ),
        }
    }

    pub(super) fn seed_receipt(
        &self,
        ticket: super::NativeConfigurationSeedTicket,
    ) -> Result<super::NativeConfigurationSeedReceipt, super::NativeConfigurationSeedRefusal> {
        match self {
            Self::ReadyForInstall { candidate, .. } if candidate.ticket() == ticket => candidate
                .staged_receipt()
                .ok_or(super::NativeConfigurationSeedRefusal::SeedNotStaged),
            _ => Err(super::NativeConfigurationSeedRefusal::StaleTicket),
        }
    }

    pub(super) fn take_seed(
        &mut self,
        ticket: super::NativeConfigurationSeedTicket,
    ) -> Result<
        (Vec<u8>, super::NativeConfigurationSeedReceipt),
        super::NativeConfigurationSeedRefusal,
    > {
        match self {
            Self::ReadyForInstall { candidate, .. } => candidate.take_seed(ticket),
            _ => Err(super::NativeConfigurationSeedRefusal::StaleTicket),
        }
    }

    pub(super) const fn blocked_request(&self) -> Option<NativeAudioReprepareRequest> {
        match self {
            Self::Admitted => None,
            Self::Blocked(request) | Self::ReadyForInstall { request, .. } => Some(*request),
            Self::AudioInputsProposed(proposal) | Self::AudioInputsStaged(proposal) => {
                Some(proposal.observed_cohort.request)
            }
        }
    }

    pub(super) const fn ready_token(&self) -> Option<NonZeroU64> {
        match self {
            Self::ReadyForInstall { control_token, .. } => Some(*control_token),
            _ => None,
        }
    }
}

pub(super) fn observed_contract(
    original: AudioProcessorExecutionContract,
    latency: usize,
    tail: crate::AudioProcessorTail,
) -> Result<AudioProcessorExecutionContract, AudioProcessorHostError> {
    AudioProcessorExecutionContract::new(
        latency,
        tail,
        original.requires_state_entry(),
        original.realtime_capable(),
        original.offline_capable(),
        original.session_scratch_bytes(),
    )
}

pub(super) struct FixedConfigurationWriter {
    bytes: crate::NativeHostFixedBytes,
}

impl FixedConfigurationWriter {
    pub(super) fn new(limit: usize, grant: usize) -> Result<Self, NativeConfigurationRefusal> {
        let bytes =
            crate::NativeHostFixedBytes::new(limit, grant).map_err(|error| match error {
                crate::NativeHostBufferError::ExtentOverflow
                | crate::NativeHostBufferError::GrantExceeded { .. } => {
                    NativeConfigurationRefusal::BufferGrantExceeded
                }
                crate::NativeHostBufferError::AllocationUnavailable => {
                    NativeConfigurationRefusal::HostAllocationUnavailable
                }
            })?;
        Ok(Self { bytes })
    }

    pub(super) fn capacity(&self) -> usize {
        self.bytes.allocation_layout().bytes
    }

    pub(super) fn finish(self) -> Result<Vec<u8>, NativeConfigurationRefusal> {
        // Box-to-Vec transfer preserves the same allocation and full admitted
        // capacity. No shrink/copy moves resource demand outside this receipt.
        self.bytes
            .into_payload_vector()
            .map_err(|_| NativeConfigurationRefusal::ConfigurationTooLarge)
    }
}

impl std::io::Write for FixedConfigurationWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        std::io::Write::write(&mut self.bytes, bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(&mut self.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn retained_restart_format_requires_exact_abi_flags_and_preserves_capture_bounds() {
        use NativeAudioReprepareRequest::{ClapRestart, Vst3Restart};
        assert_eq!(
            NativeConfigurationFormat::for_retained_restart(ClapRestart),
            Some(NativeConfigurationFormat::ClapState)
        );
        assert_eq!(
            NativeConfigurationFormat::for_retained_restart(Vst3Restart { flags: 8 }),
            Some(NativeConfigurationFormat::Vst3ComponentAndController)
        );
        for flags in [0, 1, 2, 4, 9, 12, u32::MAX] {
            assert_eq!(
                NativeConfigurationFormat::for_retained_restart(Vst3Restart { flags }),
                None
            );
        }
        for n in [0, 1, 64 * 1024, usize::MAX / 3] {
            assert_eq!(
                NativeConfigurationFormat::ClapState.capture_byte_buffer_bound(n),
                Some(n)
            );
            assert_eq!(
                NativeConfigurationFormat::Vst3ComponentAndController.capture_byte_buffer_bound(n),
                Some(n * 3)
            );
        }
        assert_eq!(
            NativeConfigurationFormat::ClapState.capture_byte_buffer_bound(usize::MAX),
            Some(usize::MAX)
        );
        assert_eq!(
            NativeConfigurationFormat::Vst3ComponentAndController
                .capture_byte_buffer_bound(usize::MAX / 3 + 1),
            None
        );
        assert_eq!(
            NativeConfigurationFormat::Vst3ComponentAndController
                .capture_byte_buffer_bound(usize::MAX),
            None
        );
    }

    #[test]
    fn native_configuration_writer_ignored_overflow_never_publishes_truncated_state() {
        let mut writer = FixedConfigurationWriter::new(4, 4).expect("fixed grant");
        let capacity = writer.capacity();
        writer.write_all(&[1, 2, 3, 4]).expect("at limit");
        assert!(writer.write_all(&[5]).is_err());
        assert_eq!(writer.capacity(), capacity);
        assert_eq!(
            writer.finish(),
            Err(NativeConfigurationRefusal::ConfigurationTooLarge)
        );
    }

    #[test]
    fn native_configuration_writer_transfer_preserves_capacity_and_byte_budget() {
        assert!(matches!(
            FixedConfigurationWriter::new(4, 3),
            Err(NativeConfigurationRefusal::BufferGrantExceeded)
        ));
        let mut writer = FixedConfigurationWriter::new(4, 4).expect("grant");
        writer.write_all(&[1, 2]).expect("partial state");
        let capacity = writer.capacity();
        let state = writer.finish().expect("capture");
        assert_eq!(state, [1, 2]);
        assert_eq!(state.capacity(), capacity);
        assert_eq!(
            NativeControlBufferReceipt {
                request_value_capacity_bytes: 8,
                capture_payload_bytes: 4,
                configuration_capacity_bytes: capacity
            }
            .peak_bytes(),
            Some(12 + capacity)
        );
    }
}
