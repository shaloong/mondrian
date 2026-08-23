//! Program Output to concrete consumer audio delivery.

use crate::{
    AudioContinuityEpoch, AudioExecutionError, AudioMeterObserver, AudioProgramExecutionDemand,
    AudioProgramRuntime, AudioRenderContract, AudioRenderRequest, AudioRuntimeResourceFootprint,
    AudioStateEntry, PreparedAudioChannelMixer,
};
use mondrian_core::{
    AudioChannelLayout, AudioChannelMixMatrix, AudioChannelMixMatrixError,
    ExecutionCancellationToken,
};

/// Versioned mapping policy selected after one public Program Output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDeliveryMappingKind {
    /// Compiler proved the selected Program silent; target silence needs no semantic mix.
    ProvenSilence,
    /// Source and target layouts are equal and every ordinal is preserved.
    Identity,
    /// Mondrian's canonical standard matrix was selected for distinct layouts.
    Standard,
}

/// Immutable facts proving how Program PCM reaches one consumer layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioDeliveryEvidence {
    /// Authored Sequence Program Output layout.
    pub program_layout: AudioChannelLayout,
    /// Exact layout required by the device or encoding Adapter.
    pub target_layout: AudioChannelLayout,
    /// Mapping policy used after the public output.
    pub mapping_kind: AudioDeliveryMappingKind,
    /// Number of canonical non-zero coefficients in the selected matrix.
    pub coefficient_count: usize,
}

/// Failure to prepare or execute one Program Output delivery mapping.
#[derive(Debug, thiserror::Error)]
pub enum AudioDeliveryError {
    /// No standard policy exists for the exact semantic layout pair.
    #[error(
        "cannot deliver Program layout {program_layout} to target layout {target_layout}: {source}"
    )]
    MappingUnavailable {
        /// Program Output layout.
        program_layout: AudioChannelLayout,
        /// Consumer target layout.
        target_layout: AudioChannelLayout,
        /// Canonical matrix-policy failure.
        #[source]
        source: AudioChannelMixMatrixError,
    },
    /// Delivery scratch or output extent overflowed addressable memory.
    #[error("audio delivery sample extent is too large")]
    SampleExtentOverflow,
    /// One request exceeded the immutable prepared maximum.
    #[error("audio delivery requested {requested} frames but admits at most {maximum}")]
    BlockTooLarge { requested: usize, maximum: usize },
    /// Caller storage did not match the exact target layout extent.
    #[error("audio delivery target buffer has {actual} samples; expected {expected}")]
    TargetBufferSizeMismatch { expected: usize, actual: usize },
    /// The common Program Runtime rejected execution.
    #[error(transparent)]
    ProgramExecution(#[from] AudioExecutionError),
}

/// Deep runtime Module owning public-Program execution, delivery mapping, and scratch.
///
/// Playback and Export construct the Program Runtime differently, but both cross
/// this Interface for the sole post-output channel mapping and hot execution.
/// The mapping never changes authored Program semantics or normalizes/clips PCM.
pub struct AudioProgramDeliveryRuntime {
    runtime: AudioProgramRuntime,
    mixer: PreparedAudioChannelMixer,
    evidence: AudioDeliveryEvidence,
    program_pcm: Vec<f32>,
}

impl AudioProgramDeliveryRuntime {
    /// Prepare Mondrian's standard delivery mapping for one concrete target.
    pub fn prepare_standard(
        runtime: AudioProgramRuntime,
        target_layout: AudioChannelLayout,
    ) -> Result<Self, AudioDeliveryError> {
        let contract = runtime.render_contract();
        let program_layout = contract.channel_layout;
        let proven_silent = !runtime.execution_demand().requires_execution();
        let matrix = if proven_silent {
            AudioChannelMixMatrix::new(program_layout, target_layout, [])
                .expect("empty delivery matrix is canonical for valid layouts")
        } else {
            AudioChannelMixMatrix::standard(program_layout, target_layout).map_err(|source| {
                AudioDeliveryError::MappingUnavailable { program_layout, target_layout, source }
            })?
        };
        let mapping_kind = if proven_silent {
            AudioDeliveryMappingKind::ProvenSilence
        } else if matrix.is_identity() {
            AudioDeliveryMappingKind::Identity
        } else {
            AudioDeliveryMappingKind::Standard
        };
        let coefficient_count = matrix.entries().len();
        let program_samples = contract
            .max_block_frames
            .checked_mul(program_layout.channel_count())
            .ok_or(AudioDeliveryError::SampleExtentOverflow)?;
        Ok(Self {
            runtime,
            mixer: PreparedAudioChannelMixer::new(matrix),
            evidence: AudioDeliveryEvidence {
                program_layout,
                target_layout,
                mapping_kind,
                coefficient_count,
            },
            program_pcm: vec![0.0; program_samples],
        })
    }

    /// Exact Program-to-target mapping evidence.
    pub const fn evidence(&self) -> AudioDeliveryEvidence {
        self.evidence
    }

    /// Program-side Render Contract; target layout is carried by [`Self::evidence`].
    pub fn program_render_contract(&self) -> AudioRenderContract {
        self.runtime.render_contract()
    }

    /// Compiler-owned evidence for whether the selected Program requires execution.
    pub const fn execution_demand(&self) -> AudioProgramExecutionDemand {
        self.runtime.execution_demand()
    }

    /// Closure-wide resource evidence excluding this small delivery scratch.
    pub const fn resource_footprint(&self) -> AudioRuntimeResourceFootprint {
        self.runtime.resource_footprint()
    }

    /// Lock-free meter observation from the public Program Output before delivery mapping.
    pub fn meter_observer(&self) -> AudioMeterObserver {
        self.runtime.meter_observer()
    }

    /// Whether execution requires an explicit fresh continuity entry.
    pub fn requires_state_entry(&self) -> bool {
        self.runtime.requires_state_entry()
    }

    /// Enter one fresh Program continuity epoch.
    pub fn enter_state(
        &mut self,
        epoch: AudioContinuityEpoch,
        start_sample: i64,
    ) -> Result<(), AudioDeliveryError> {
        self.runtime.enter_state(AudioStateEntry { epoch, start_sample })?;
        Ok(())
    }

    /// Render exact target-layout interleaved PCM without allocation.
    pub fn render_into_cancellable(
        &mut self,
        request: AudioRenderRequest,
        destination: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), AudioDeliveryError> {
        let contract = self.runtime.render_contract();
        if request.frames > contract.max_block_frames {
            return Err(AudioDeliveryError::BlockTooLarge {
                requested: request.frames,
                maximum: contract.max_block_frames,
            });
        }
        let target_samples = request
            .frames
            .checked_mul(self.evidence.target_layout.channel_count())
            .ok_or(AudioDeliveryError::SampleExtentOverflow)?;
        if destination.len() != target_samples {
            return Err(AudioDeliveryError::TargetBufferSizeMismatch {
                expected: target_samples,
                actual: destination.len(),
            });
        }
        let program_samples = request
            .frames
            .checked_mul(self.evidence.program_layout.channel_count())
            .ok_or(AudioDeliveryError::SampleExtentOverflow)?;
        self.runtime.render_into_cancellable(
            request,
            &mut self.program_pcm[..program_samples],
            cancellation,
        )?;
        self.mixer
            .mix_into(
                crate::AudioKernelBackend::RuntimeVectorized,
                request.frames,
                &self.program_pcm[..program_samples],
                destination,
            )
            .map_err(AudioDeliveryError::ProgramExecution)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioDecodedSource, AudioMediaResolver, AudioProcessingMode, ResolvedAudioSource};
    use mondrian_core::{AssetId, AudioSourceComponentId, TimelineTime};
    use mondrian_timeline::{Clip, Sequence};
    use std::sync::Arc;

    struct SilentMedia(AudioChannelLayout);

    struct ZeroSource;

    impl AudioDecodedSource for ZeroSource {
        fn read_interleaved(
            &self,
            _start_frame: i64,
            _frames: usize,
            destination: &mut [f32],
            _cancellation: &ExecutionCancellationToken,
        ) -> Result<(), String> {
            destination.fill(0.0);
            Ok(())
        }
    }

    impl AudioMediaResolver for SilentMedia {
        fn resolve(
            &self,
            _asset_id: AssetId,
            _component_id: AudioSourceComponentId,
            _sample_rate: u32,
        ) -> Result<crate::ResolvedAudioSource, String> {
            Ok(ResolvedAudioSource::new(self.0, Arc::new(ZeroSource)))
        }
    }

    fn contract(layout: AudioChannelLayout) -> AudioRenderContract {
        AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: layout,
            max_block_frames: 64,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes:
                AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
            public_output_lookahead_budget_frames:
                AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
            compensation_delay_scratch_budget_bytes:
                AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
        }
    }

    fn silent_runtime(layout: AudioChannelLayout) -> AudioProgramRuntime {
        let mut sequence = Sequence::new("delivery");
        sequence.settings.audio_channel_layout = layout;
        AudioProgramRuntime::build(&sequence, &[], &SilentMedia(layout), contract(layout), None)
            .expect("silent runtime")
    }

    fn active_runtime(layout: AudioChannelLayout) -> AudioProgramRuntime {
        let mut sequence = Sequence::new("delivery source");
        sequence.settings.audio_channel_layout = layout;
        let track_id = sequence.audio_tracks[0].id;
        let clip =
            Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE).expect("audio Clip");
        sequence
            .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
            .expect("audio placement");
        AudioProgramRuntime::build(&sequence, &[], &SilentMedia(layout), contract(layout), None)
            .expect("active runtime")
    }

    #[test]
    fn standard_delivery_publishes_complete_mapping_evidence() {
        let runtime = active_runtime(AudioChannelLayout::Surround51Side);
        let delivery =
            AudioProgramDeliveryRuntime::prepare_standard(runtime, AudioChannelLayout::Stereo)
                .expect("5.1 to stereo delivery");
        assert_eq!(
            delivery.evidence(),
            AudioDeliveryEvidence {
                program_layout: AudioChannelLayout::Surround51Side,
                target_layout: AudioChannelLayout::Stereo,
                mapping_kind: AudioDeliveryMappingKind::Standard,
                coefficient_count: 6,
            }
        );
    }

    #[test]
    fn unsupported_delivery_pair_fails_with_both_layouts() {
        let custom = AudioChannelLayout::speakers([
            mondrian_core::AudioChannelPosition::FrontLeft,
            mondrian_core::AudioChannelPosition::FrontRight,
            mondrian_core::AudioChannelPosition::TopCenter,
        ])
        .expect("custom layout");
        let runtime = active_runtime(custom);
        let error =
            AudioProgramDeliveryRuntime::prepare_standard(runtime, AudioChannelLayout::Stereo)
                .err()
                .expect("mapping must fail");
        assert!(matches!(
            error,
            AudioDeliveryError::MappingUnavailable {
                program_layout,
                target_layout: AudioChannelLayout::Stereo,
                ..
            } if program_layout == custom
        ));
    }

    #[test]
    fn proven_silent_custom_program_delivers_target_silence_without_guessing() {
        let custom = AudioChannelLayout::speakers([
            mondrian_core::AudioChannelPosition::FrontLeft,
            mondrian_core::AudioChannelPosition::TopCenter,
        ])
        .expect("custom layout");
        let delivery = AudioProgramDeliveryRuntime::prepare_standard(
            silent_runtime(custom),
            AudioChannelLayout::Stereo,
        )
        .expect("silence needs no semantic mapping");
        assert_eq!(
            delivery.evidence().mapping_kind,
            AudioDeliveryMappingKind::ProvenSilence
        );
        assert_eq!(delivery.evidence().coefficient_count, 0);
    }

    #[test]
    fn delivery_rejects_wrong_target_extent_before_execution() {
        let runtime = silent_runtime(AudioChannelLayout::Mono);
        let mut delivery =
            AudioProgramDeliveryRuntime::prepare_standard(runtime, AudioChannelLayout::Stereo)
                .expect("mono to stereo delivery");
        let error = delivery
            .render_into_cancellable(
                AudioRenderRequest { start_sample: 0, frames: 2 },
                &mut [0.0; 3],
                &ExecutionCancellationToken::new(),
            )
            .expect_err("wrong target extent");
        assert!(matches!(
            error,
            AudioDeliveryError::TargetBufferSizeMismatch { expected: 4, actual: 3 }
        ));
    }
}
