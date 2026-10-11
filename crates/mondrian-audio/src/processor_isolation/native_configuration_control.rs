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

/// Borrowed non-RT command for the exact retained processor object.
///
/// The caller must quiesce admitted DSP/output before servicing. Constant values
/// refer to the prepared lane order, in canonical definition coordinates; the
/// caller must establish that the canonical automation profile is constant. This
/// command does not evaluate curves or authorize a new plan, PDC or Clock Master.
pub struct NativeConfigurationControlRequest<'a> {
    /// Non-consuming snapshot expected by the caller.
    pub expected_cohort: NativeRequestCohort,
    /// Distinguishes this service result from any other attempted installation.
    pub control_token: NonZeroU64,
    /// Canonical scalar configuration in the admitted lane order.
    pub canonical_constant_values: &'a [f64],
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
