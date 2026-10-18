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
    submitted_window: Option<AudioSubmittedWindowCursor>,
}

#[derive(Clone, Copy)]
struct AudioSubmittedWindowCursor {
    epoch: AudioContinuityEpoch,
    next_start_sample: i64,
}

impl AudioProgramDeliveryRuntime {
    /// Forward child-settlement evidence to the sole owned Program Runtime.
    /// This adds no delivery state, native execution or installation permission.
    pub fn bind_native_child_settlement(
        &mut self,
        registrar: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<(), mondrian_core::NativeChildSettlementError> {
        self.runtime.bind_native_child_settlement(registrar)
    }

    /// Read-only capability of the sole owned Program Runtime.
    pub fn native_observations_supported(&self) -> bool {
        self.runtime.native_observations_supported()
    }

    /// Drain one actual occurrence on the same serial native control lane.
    /// Delivery neither resets DSP nor introduces another observation store.
    pub fn poll_native_observations(
        &mut self,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<Option<mondrian_core::NativeAudioObservationBatch>, crate::AudioProcessorHostError>
    {
        self.runtime.poll_native_observations(cancellation)
    }

    /// Current actual Runtime registration; no wrapper-cached owner is inferred.
    pub fn native_owner_installation_verifier(
        &self,
    ) -> Option<mondrian_core::AudioOwnerInstallationVerifier> {
        self.runtime.native_owner_installation_verifier()
    }

    /// Register the exact sole owned Runtime without native work or graph install.
    pub fn bind_native_owner_installation(
        &mut self,
        verifier: mondrian_core::AudioOwnerInstallationVerifier,
    ) -> Result<(), mondrian_core::AudioOwnerInstallationError> {
        self.runtime.bind_native_owner_installation(verifier)
    }

    /// Bind existing root/nested native objects on the sole non-RT consumer.
    /// This is the same owned Runtime; no editor-only instance is constructed.
    pub fn install_native_owner_bindings(
        &mut self,
        claim: &mondrian_core::AudioOwnerInstallationClaim,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<usize, crate::AudioNativeOwnerError> {
        self.runtime.install_native_owner_bindings(claim, cancellation)
    }

    /// Observe current exact native ownership from the same owned Runtime.
    pub fn native_owner_lease(
        &self,
        address: crate::AudioNativeProcessorAddress,
    ) -> Result<crate::AudioNativeProcessorOwnerLease, crate::AudioNativeOwnerError> {
        self.runtime.native_owner_lease(address)
    }

    /// Refuse foreign/stale leases before accessing the actual retained object.
    pub fn validate_native_owner(
        &mut self,
        lease: crate::AudioNativeProcessorOwnerLease,
    ) -> Result<(), crate::AudioNativeOwnerError> {
        self.runtime.validate_native_owner(lease)
    }

    /// Read-only editor target from the sole owned Runtime's installed record.
    pub fn native_editor_target(
        &self,
        address: crate::AudioNativeProcessorAddress,
    ) -> Result<mondrian_core::NativeAudioEditorTarget, crate::AudioNativeOwnerError> {
        self.runtime.native_editor_target(address)
    }

    /// Forward one closed editor intent to the same owned Runtime.
    pub fn control_native_editor(
        &mut self,
        intent: mondrian_core::NativeAudioEditorControlIntent,
        cancellation: &mondrian_core::ExecutionCancellationToken,
    ) -> Result<mondrian_core::NativeAudioEditorControlOutcome, crate::AudioNativeOwnerError> {
        self.runtime.control_native_editor(intent, cancellation)
    }

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
            submitted_window: None,
        })
    }

    /// Exact Program-to-target mapping evidence.
    pub const fn evidence(&self) -> AudioDeliveryEvidence {
        self.evidence
    }

    /// Visit the sole Program Runtime's generated cursor facts without execution.
    ///
    /// Delivery mapping and target PCM do not become private DSP checkpoints.
    /// The caller separately owns renderer/generation/cohort and bounded storage.
    pub fn visit_continuity_evidence(
        &self,
        visitor: &mut impl crate::AudioContinuityEvidenceVisitor,
    ) -> Result<(), AudioExecutionError> {
        self.runtime.visit_continuity_evidence(visitor)
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

    /// Enter one fresh Program epoch and start its submitted-window sequence.
    /// The window cursor records public submissions, not private DSP position.
    pub fn enter_state(
        &mut self,
        epoch: AudioContinuityEpoch,
        start_sample: i64,
    ) -> Result<(), AudioDeliveryError> {
        self.enter_state_cancellable(epoch, start_sample, &ExecutionCancellationToken::new())
    }

    /// Enter with the actual queued consumer's cancellation token.
    pub fn enter_state_cancellable(
        &mut self,
        epoch: AudioContinuityEpoch,
        start_sample: i64,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), AudioDeliveryError> {
        self.runtime
            .enter_state_cancellable(AudioStateEntry { epoch, start_sample }, cancellation)?;
        self.submitted_window =
            Some(AudioSubmittedWindowCursor { epoch, next_start_sample: start_sample });
        Ok(())
    }

    /// Read-only actual-tree state-entry policy; does not authorize SDK work.
    pub fn native_author_state_entry_required(
        &self,
        proposal: &mondrian_core::NativeAudioAuthorProposal,
    ) -> Result<bool, crate::AudioNativeOwnerError> {
        self.runtime.native_author_state_entry_required(proposal)
    }

    /// Prepare a canonical parameter-only replacement on the SAME Runtime.
    /// No target-layout remix, state entry or DSP reset is performed.
    pub fn prepare_native_author_parameter_handoff(
        &mut self,
        programs: &[crate::NativeAudioAuthorProgram],
        proposal: &mondrian_core::NativeAudioAuthorProposal,
        destination: mondrian_core::AudioOwnerInstallationVerifier,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<crate::PreparedNativeAudioParameterHandoff, crate::NativeAudioParameterHandoffError>
    {
        self.runtime.prepare_native_author_parameter_handoff(
            programs,
            proposal,
            destination,
            cancellation,
        )
    }

    /// Publish only an already admitted same-object Plan/binding replacement.
    /// The caller retains prepared retirement owners until final guards ended.
    pub fn install_native_author_parameter_handoff(
        &mut self,
        prepared: &mut crate::PreparedNativeAudioParameterHandoff,
        authorization: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<crate::NativeAudioParameterHandoffReceipt, crate::NativeAudioParameterHandoffError>
    {
        let receipt = self.runtime.install_native_author_parameter_handoff(
            prepared,
            authorization,
            cancellation,
        )?;
        if receipt.continuity == crate::NativeAudioParameterHandoffContinuity::StateEntryRequired {
            self.submitted_window = None;
        }
        Ok(receipt)
    }

    /// Retry only exact native Ack on the same installed Runtime and delivery.
    /// No author, Plan, scratch, PDC or sample cursor changes occur.
    pub fn acknowledge_native_author_parameter_handoff(
        &mut self,
        proposal: &mondrian_core::NativeAudioAuthorProposal,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<crate::NativeAudioParameterHandoffReceipt, crate::NativeAudioParameterHandoffError>
    {
        self.runtime.acknowledge_native_author_parameter_handoff(proposal, cancellation)
    }

    /// Render exact target-layout interleaved PCM without allocation.
    ///
    /// After successful entry, every request must exactly continue its submitted
    /// public window sequence. Native holds and pre-canceled requests consume no
    /// window. A stateless ordinary execution failure consumes its admitted
    /// window so the consumer can substitute equal-duration silence and continue.
    /// Without entry, stateless direct callers retain independent random windows.
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
        // The same whole-root preflight must win over queued later-window
        // coordinate errors while native control retains the first window.
        self.runtime.preflight_render_cancellable(request, cancellation)?;
        let next_start_sample =
            crate::render::checked_advance_sample(request.start_sample, request.frames)?;
        if let Some(cursor) = self.submitted_window
            && cursor.next_start_sample != request.start_sample
        {
            return Err(AudioExecutionError::NonContiguousBlock {
                epoch: cursor.epoch,
                expected_start_sample: cursor.next_start_sample,
                actual_start_sample: request.start_sample,
            }
            .into());
        }
        let generation_state = self.runtime.requires_state_entry();
        let result = self
            .runtime
            .render_into_admitted_cancellable(
                request,
                &mut self.program_pcm[..program_samples],
                cancellation,
            )
            .map_err(AudioDeliveryError::ProgramExecution)
            .and_then(|()| {
                self.mixer
                    .mix_into(
                        crate::AudioKernelBackend::RuntimeVectorized,
                        request.frames,
                        &self.program_pcm[..program_samples],
                        destination,
                    )
                    .map_err(AudioDeliveryError::ProgramExecution)
            });
        // An independent failed attempt is still the exact submitted window
        // that Media replaces with silence. Stateful failures retain the sole
        // Session poison law; typed native holds never consume a submission.
        let consumed = result.is_ok()
            || (!generation_state
                && !cancellation.is_canceled()
                && !matches!(
                    &result,
                    Err(AudioDeliveryError::ProgramExecution(
                        AudioExecutionError::ReprepareRequired(_)
                            | AudioExecutionError::NativeAuthorPending(_)
                            | AudioExecutionError::EditorCleanupRequired(_)
                    ))
                ));
        if consumed && let Some(cursor) = &mut self.submitted_window {
            cursor.next_start_sample = next_start_sample;
        }
        result
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

    #[test]
    fn delivery_submitted_windows_entered_stateless_rejects_gap_without_consuming() {
        let mut delivery = AudioProgramDeliveryRuntime::prepare_standard(
            silent_runtime(AudioChannelLayout::Stereo),
            AudioChannelLayout::Stereo,
        )
        .expect("actual stateless delivery");
        assert!(!delivery.requires_state_entry());
        let epoch = AudioContinuityEpoch::new(7);
        delivery.enter_state(epoch, 0).expect("actual entry");
        let cancel = ExecutionCancellationToken::new();
        let mut pcm = [73.0; 8];
        let wrong_first = delivery.render_into_cancellable(
            AudioRenderRequest { start_sample: 1, frames: 4 },
            &mut pcm,
            &cancel,
        );
        assert!(matches!(
            wrong_first,
            Err(AudioDeliveryError::ProgramExecution(AudioExecutionError::NonContiguousBlock {
                epoch: actual_epoch,
                expected_start_sample: 0,
                actual_start_sample: 1,
            })) if actual_epoch == epoch
        ));
        assert_eq!(
            pcm, [73.0; 8],
            "rejected admission cannot mutate caller PCM"
        );
        for start_sample in [0, 4] {
            delivery
                .render_into_cancellable(
                    AudioRenderRequest { start_sample, frames: 4 },
                    &mut pcm,
                    &cancel,
                )
                .expect("exact submitted continuation");
        }
        assert!(matches!(
            delivery.render_into_cancellable(
                AudioRenderRequest { start_sample: 9, frames: 4 },
                &mut pcm,
                &cancel,
            ),
            Err(AudioDeliveryError::ProgramExecution(
                AudioExecutionError::NonContiguousBlock {
                    expected_start_sample: 8,
                    actual_start_sample: 9,
                    ..
                }
            ))
        ));
        delivery
            .render_into_cancellable(
                AudioRenderRequest { start_sample: 8, frames: 4 },
                &mut pcm,
                &cancel,
            )
            .expect("wrong-start rejection did not consume the exact window");
    }

    #[test]
    fn delivery_submitted_windows_unentered_stateless_keeps_random_window_contract() {
        let mut delivery = AudioProgramDeliveryRuntime::prepare_standard(
            silent_runtime(AudioChannelLayout::Stereo),
            AudioChannelLayout::Stereo,
        )
        .expect("actual independent delivery");
        for start_sample in [37, -11, 5] {
            let mut pcm = [73.0; 8];
            delivery
                .render_into_cancellable(
                    AudioRenderRequest { start_sample, frames: 4 },
                    &mut pcm,
                    &ExecutionCancellationToken::new(),
                )
                .expect("no submitted generation was entered");
            assert_eq!(pcm, [0.0; 8]);
        }
    }

    struct SingleSourceMedia(Arc<dyn AudioDecodedSource>);

    impl AudioMediaResolver for SingleSourceMedia {
        fn resolve(
            &self,
            _asset_id: AssetId,
            _component_id: AudioSourceComponentId,
            _sample_rate: u32,
        ) -> Result<ResolvedAudioSource, String> {
            Ok(ResolvedAudioSource::new(
                AudioChannelLayout::Stereo,
                Arc::clone(&self.0),
            ))
        }
    }

    struct FailOnceSource(std::sync::atomic::AtomicBool);

    impl AudioDecodedSource for FailOnceSource {
        fn read_interleaved(
            &self,
            _start_frame: i64,
            _frames: usize,
            destination: &mut [f32],
            _cancellation: &ExecutionCancellationToken,
        ) -> Result<(), String> {
            if self.0.swap(false, std::sync::atomic::Ordering::AcqRel) {
                return Err("submitted independent window source failure".to_owned());
            }
            destination.fill(0.375);
            Ok(())
        }
    }

    fn source_delivery(source: Arc<dyn AudioDecodedSource>) -> AudioProgramDeliveryRuntime {
        let mut sequence = Sequence::new("independent source admission");
        let clip = Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE)
            .expect("actual authored source");
        sequence
            .add_media_audio_clip(
                sequence.audio_tracks[0].id,
                clip,
                AudioSourceComponentId::primary(),
            )
            .expect("actual authored placement");
        let runtime = AudioProgramRuntime::build(
            &sequence,
            &[],
            &SingleSourceMedia(source),
            contract(AudioChannelLayout::Stereo),
            None,
        )
        .expect("actual independent Runtime");
        assert!(!runtime.requires_state_entry());
        AudioProgramDeliveryRuntime::prepare_standard(runtime, AudioChannelLayout::Stereo)
            .expect("actual delivery")
    }

    #[test]
    fn delivery_submitted_windows_independent_failure_consumes_only_admitted_window() {
        let mut delivery = source_delivery(Arc::new(FailOnceSource(
            std::sync::atomic::AtomicBool::new(true),
        )));
        delivery.enter_state(AudioContinuityEpoch::new(8), 0).expect("entry");
        let cancellation = ExecutionCancellationToken::new();
        let mut pcm = [73.0; 8];
        let failed = delivery.render_into_cancellable(
            AudioRenderRequest { start_sample: 0, frames: 4 },
            &mut pcm,
            &cancellation,
        );
        assert!(
            matches!(failed, Err(AudioDeliveryError::ProgramExecution(AudioExecutionError::SourceUnavailable(detail))) if detail.contains("submitted independent window source failure"))
        );
        // Media replaces this accepted failed window with exactly four silent
        // frames. Its already queued next request starts inside the same cache.
        assert!(matches!(
            delivery.render_into_cancellable(
                AudioRenderRequest { start_sample: 8, frames: 4 },
                &mut pcm,
                &cancellation,
            ),
            Err(AudioDeliveryError::ProgramExecution(
                AudioExecutionError::NonContiguousBlock {
                    expected_start_sample: 4,
                    actual_start_sample: 8,
                    ..
                }
            ))
        ));
        delivery
            .render_into_cancellable(
                AudioRenderRequest { start_sample: 4, frames: 4 },
                &mut pcm,
                &cancellation,
            )
            .expect("the ordinary failed independent window did not poison later PCM");
        assert_eq!(
            pcm, [0.375; 8],
            "actual source PCM resumes at the exact next window"
        );
    }

    struct CancelOnceSource(std::sync::atomic::AtomicBool);

    impl AudioDecodedSource for CancelOnceSource {
        fn read_interleaved(
            &self,
            _start_frame: i64,
            _frames: usize,
            destination: &mut [f32],
            cancellation: &ExecutionCancellationToken,
        ) -> Result<(), String> {
            if self.0.swap(false, std::sync::atomic::Ordering::AcqRel) {
                destination.fill(0.75);
                cancellation.cancel();
            } else {
                destination.fill(0.375);
            }
            Ok(())
        }
    }

    #[test]
    fn delivery_submitted_windows_canceled_successful_read_does_not_publish_or_consume() {
        let mut delivery = source_delivery(Arc::new(CancelOnceSource(
            std::sync::atomic::AtomicBool::new(true),
        )));
        delivery.enter_state(AudioContinuityEpoch::new(9), 0).expect("entry");
        let canceled = ExecutionCancellationToken::new();
        let mut pcm = [73.0; 8];
        assert!(matches!(
            delivery.render_into_cancellable(
                AudioRenderRequest { start_sample: 0, frames: 4 },
                &mut pcm,
                &canceled,
            ),
            Err(AudioDeliveryError::ProgramExecution(
                AudioExecutionError::SourceUnavailable(_)
            ))
        ));
        assert!(canceled.is_canceled());
        assert_eq!(pcm, [73.0; 8], "canceled source PCM cannot be delivered");
        delivery
            .render_into_cancellable(
                AudioRenderRequest { start_sample: 0, frames: 4 },
                &mut pcm,
                &ExecutionCancellationToken::new(),
            )
            .expect("the same unconsumed window retries with a fresh cancellation owner");
        assert_eq!(
            pcm, [0.375; 8],
            "retry must read actual PCM, not the canceled cache"
        );
    }
}
