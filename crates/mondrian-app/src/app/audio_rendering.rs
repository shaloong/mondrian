use super::*;
mod host_output_fader;
mod native_author_terminal_batch;
#[cfg(any(test, feature = "validation"))]
use mondrian_audio::BuiltInAudioProcessorResolver;
use mondrian_audio::{
    AudioAuditionOverlay, AudioCompileRequest, AudioContinuityEpoch, AudioDecodedSource,
    AudioDeliveryEvidence, AudioMediaResolver, AudioMeterObserver, AudioProcessingMode,
    AudioProcessorResolver, AudioProgramDeliveryRuntime, AudioProgramExecutionDemand,
    AudioProgramRuntime, AudioRenderContract, AudioRenderRequest, AudioRuntimeResourceGrant,
    ResolvedAudioSource,
};
use mondrian_core::{AudioChannelLayout, AudioSourceComponentId, ExecutionCancellationToken};
use mondrian_media::{AudioPcmRenderOutcome, AudioSourceReader, AudioSourceSelection};
use parking_lot::Mutex;

pub(super) const MAX_AUDIO_RENDER_BLOCK_FRAMES: usize = 16_384;

pub(super) struct TimelineAudioPcmRenderer {
    state: Mutex<TimelineAudioRenderState>,
    author_snapshot:
        parking_lot::RwLock<Option<Arc<mondrian_editor_state::AuthorExecutionSnapshot>>>,
    audition: AudioAuditionOverlay,
    meter_metadata: parking_lot::RwLock<AudioMeterObserver>,
    delivery_metadata: AudioDeliveryEvidence,
    continuity_model: AudioPcmContinuityModel,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
}

struct TimelineAudioRenderState {
    delivery: AudioProgramDeliveryRuntime,
    generation: Option<AudioPcmRenderGeneration>,
    // Drop after Delivery and all its retained Sessions. Every strong renderer
    // alias therefore retains the same observation scope, without another lock.
    // This App payload is outside the Audio Runtime's Session resource grant.
    native_child_settlement: Option<mondrian_core::NativeChildSettlementRegistrar>,
}

impl TimelineAudioPcmRenderer {
    /// Prepare the canonical public Program without audition or channel remapping.
    #[cfg(any(test, feature = "validation"))]
    pub(super) fn new_public_program(
        sequence: Sequence,
        sequences: Vec<Sequence>,
        library: Arc<AssetLibrary>,
        source_cache: Arc<AudioSourceCache>,
        runtime_grant: AudioRuntimeResourceGrant,
    ) -> mondrian_core::Result<Self> {
        let sample_rate = sequence.settings.audio_sample_rate;
        let channel_layout = sequence.settings.audio_channel_layout;
        let renderer = Self::new(
            sequence,
            sequences,
            library,
            source_cache,
            runtime_grant,
            AudioAuditionOverlay::default(),
            sample_rate,
            channel_layout,
        )?;
        let evidence = renderer.delivery_evidence();
        if evidence.program_layout != channel_layout
            || evidence.target_layout != channel_layout
            || matches!(
                evidence.mapping_kind,
                mondrian_audio::AudioDeliveryMappingKind::Standard
            )
        {
            return Err(audio_render_error(
                "public_program_audio_delivery",
                "Reference Audio Program requires identity-layout delivery without audition or standard remapping",
            ));
        }
        Ok(renderer)
    }

    #[cfg(any(test, feature = "validation"))]
    pub(super) fn new(
        sequence: Sequence,
        sequences: Vec<Sequence>,
        library: Arc<AssetLibrary>,
        source_cache: Arc<AudioSourceCache>,
        runtime_grant: AudioRuntimeResourceGrant,
        audition: AudioAuditionOverlay,
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
    ) -> mondrian_core::Result<Self> {
        Self::new_with_processor_resolver(
            sequence,
            sequences,
            library,
            source_cache,
            runtime_grant,
            audition,
            sample_rate,
            channel_layout,
            MAX_AUDIO_RENDER_BLOCK_FRAMES,
            &BuiltInAudioProcessorResolver,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn new_with_processor_resolver(
        sequence: Sequence,
        sequences: Vec<Sequence>,
        library: Arc<AssetLibrary>,
        source_cache: Arc<AudioSourceCache>,
        runtime_grant: AudioRuntimeResourceGrant,
        audition: AudioAuditionOverlay,
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
        max_block_frames: usize,
        processor_resolver: &dyn AudioProcessorResolver,
        native_child_settlement: Option<mondrian_core::NativeChildSettlementRegistrar>,
    ) -> mondrian_core::Result<Self> {
        if max_block_frames == 0 {
            return Err(audio_render_error(
                "timeline_audio_render_contract",
                "prepared maximum block frames must be nonzero",
            ));
        }
        let program_channel_layout = sequence.settings.audio_channel_layout;
        let contract = AudioRenderContract {
            sample_rate,
            channel_layout: program_channel_layout,
            max_block_frames,
            processing_mode: AudioProcessingMode::Realtime,
            processor_session_scratch_budget_bytes:
                AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
            public_output_lookahead_budget_frames:
                AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
            compensation_delay_scratch_budget_bytes:
                AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
        };
        let captured_audition = audition.clone();
        let resolver = PlaybackMediaResolver { library, source_cache };
        let output_id =
            sequence.audio_program.outputs.first().map(|output| output.id).ok_or_else(|| {
                audio_render_error("timeline_audio_prepare", "Sequence has no Program Output")
            })?;
        let runtime = AudioProgramRuntime::build_observing_native_child_settlement(
            &sequence,
            &sequences,
            &resolver,
            processor_resolver,
            contract,
            AudioCompileRequest { output_id, audition },
            runtime_grant,
            native_child_settlement.as_ref(),
        )
        .map_err(|error| audio_render_error("timeline_audio_prepare", error.to_string()))?;
        let continuity_model = match runtime
            .state_entry_origin()
            .map_err(|error| audio_render_error("timeline_audio_prepare", error.to_string()))?
        {
            Some(origin) => AudioPcmContinuityModel::GenerationStateFromOrigin(origin),
            None if runtime.requires_state_entry() => AudioPcmContinuityModel::GenerationState,
            None => AudioPcmContinuityModel::IndependentWindows,
        };
        let delivery = AudioProgramDeliveryRuntime::prepare_standard(runtime, channel_layout)
            .map_err(|error| {
                audio_render_error("playback_audio_delivery_mapping", error.to_string())
            })?;
        Ok(Self {
            meter_metadata: parking_lot::RwLock::new(delivery.meter_observer()),
            delivery_metadata: delivery.evidence(),
            author_snapshot: parking_lot::RwLock::new(None),
            audition: captured_audition,
            state: Mutex::new(TimelineAudioRenderState {
                delivery,
                generation: None,
                native_child_settlement,
            }),
            continuity_model,
            sample_rate,
            channel_layout,
        })
    }

    /// Read the original prepared owner without admitting state entry or DSP.
    #[cfg(all(test, windows))]
    pub(in crate::app) fn visit_unentered_continuity_for_test(
        &self,
        visitor: &mut impl mondrian_audio::AudioContinuityEvidenceVisitor,
    ) -> Result<(), mondrian_audio::AudioExecutionError> {
        let state = self.state.lock();
        assert!(
            state.generation.is_none(),
            "the fresh Timeline source has not entered DSP"
        );
        state.delivery.visit_continuity_evidence(visitor)
    }

    /// Test-only observations of the existing delivery owner; no mutable callback.
    #[cfg(all(test, windows))]
    pub(in crate::app) fn with_delivery_observation_for_test<R>(
        &self,
        observe: impl FnOnce(&AudioProgramDeliveryRuntime) -> R,
    ) -> R {
        let state = self.state.lock();
        observe(&state.delivery)
    }

    /// Preserve the original close check's typed pending cause for bounded tests.
    /// `None` is only a busy observation; no close proof or author permission.
    #[cfg(all(test, windows))]
    pub(super) fn observe_native_author_source_closed_for_test(
        &self,
        cancel: &ExecutionCancellationToken,
    ) -> Option<Result<(), mondrian_audio::NativeAudioAuthorAdmissionError>> {
        let mut state = self.state.try_lock()?;
        Some(state.delivery.validate_native_author_source_closed(cancel))
    }

    /// Fixed observational IPC only; the original Delivery validates this lease.
    #[cfg(all(test, windows))]
    pub(in crate::app) fn observe_native_object_for_test(
        &self,
        lease: mondrian_audio::AudioNativeProcessorOwnerLease,
    ) -> Result<mondrian_audio::NativeAudioObjectObservation, mondrian_audio::AudioNativeOwnerError>
    {
        self.state.lock().delivery.observe_native_owner_object(lease)
    }

    pub(super) fn bind_author_snapshot(
        &mut self,
        snapshot: mondrian_editor_state::AuthorExecutionSnapshot,
    ) {
        *self.author_snapshot.get_mut() = Some(Arc::new(snapshot));
    }

    /// Refresh permission metadata only after App proves the audible binding
    /// unchanged. This neither installs a Program nor acquires native/DSP state.
    pub(super) fn rebind_unchanged_author_snapshot(
        &self,
        snapshot: mondrian_editor_state::AuthorExecutionSnapshot,
    ) -> std::result::Result<(), mondrian_editor_state::AuthorExecutionAdmissionError> {
        let next = Arc::new(snapshot);
        let mut metadata = self.author_snapshot.write();
        let author_guard = next.enter_commit()?;
        let retired = metadata.replace(Arc::clone(&next));
        drop(author_guard);
        drop(metadata);
        drop(retired);
        Ok(())
    }

    pub(super) fn native_author_snapshot_current(&self) -> bool {
        self.author_snapshot
            .read()
            .as_ref()
            .is_some_and(|snapshot| snapshot.enter_commit().is_ok())
    }

    /// Pure metadata comparison; no native/DSP or author permission is issued.
    pub(super) fn same_presentation_execution(
        &self,
        audition: &AudioAuditionOverlay,
        rate: u32,
        layout: AudioChannelLayout,
    ) -> bool {
        self.audition == *audition && self.sample_rate == rate && self.channel_layout == layout
    }

    /// Compiler-owned evidence for replacing this Program Output with silence.
    pub(super) fn execution_demand(&self) -> AudioProgramExecutionDemand {
        self.state.lock().delivery.execution_demand()
    }

    /// Lock-free meter observation bound to this exact prepared Runtime.
    pub(super) fn meter_observer(&self) -> AudioMeterObserver {
        self.meter_metadata.read().clone()
    }

    /// Exact Sequence Program Output to monitoring-target delivery evidence.
    pub(super) fn delivery_evidence(&self) -> AudioDeliveryEvidence {
        self.delivery_metadata
    }

    /// Immutable cross-window state contract captured before trait erasure.
    pub(super) const fn continuity_model(&self) -> AudioPcmContinuityModel {
        self.continuity_model
    }
}

impl AudioPcmRenderer for TimelineAudioPcmRenderer {
    fn validate_native_author_source_closed(
        &self,
        cancel: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<()> {
        // Media's serial SourceClosed operation orders earlier callbacks before
        // this check. An external alias may still hold the Runtime: refuse it
        // without an unbounded mutex wait or an atomic late-callback seal claim.
        let mut state = self.state.try_lock().ok_or_else(|| {
            audio_render_error("project_audio_handoff", "Timeline audio Runtime is busy")
        })?;
        state
            .delivery
            .validate_native_author_source_closed(cancel)
            .map_err(|error| audio_render_error("project_audio_handoff", error.to_string()))
    }

    fn bind_native_child_settlement(
        &self,
        registrar: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<(), mondrian_core::NativeChildSettlementError> {
        let mut state = self.state.lock();
        if let Some(current) = &state.native_child_settlement {
            if !current.same_domain(registrar) {
                current.mark_unavailable();
                registrar.mark_unavailable();
                return Err(mondrian_core::NativeChildSettlementError::ForeignDomain);
            }
        } else {
            // Retain the root before entering Delivery, including a failed or
            // panicking bind. Observation failure never grants native work.
            state.native_child_settlement = Some(registrar.try_fork()?);
        }
        let TimelineAudioRenderState { delivery, generation: _, native_child_settlement } =
            &mut *state;
        let Some(root) = native_child_settlement.as_ref() else {
            registrar.mark_unavailable();
            return Err(mondrian_core::NativeChildSettlementError::Unavailable);
        };
        delivery.bind_native_child_settlement(root)
    }

    fn native_observations_supported(&self) -> bool {
        self.state.lock().delivery.native_observations_supported()
    }

    fn poll_native_observations(
        &self,
        cancellation: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<Option<mondrian_core::NativeAudioObservationBatch>> {
        self.state
            .lock()
            .delivery
            .poll_native_observations(cancellation)
            .map_err(|error| audio_render_error("native_audio_diagnostics", error.to_string()))
    }

    fn owner_installation_verifier(&self) -> Option<mondrian_core::AudioOwnerInstallationVerifier> {
        self.state.lock().delivery.native_owner_installation_verifier()
    }

    fn bind_owner_installation(
        &self,
        verifier: mondrian_core::AudioOwnerInstallationVerifier,
    ) -> Result<bool, mondrian_core::AudioOwnerInstallationError> {
        self.state.lock().delivery.bind_native_owner_installation(verifier)?;
        Ok(true)
    }

    fn install_native_owner_bindings(
        &self,
        claim: &mondrian_core::AudioOwnerInstallationClaim,
        cancellation: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<mondrian_media::AudioPcmNativeOwnerBindingStatus> {
        use mondrian_media::{
            AudioPcmNativeOwnerBindingRefusal as Refusal,
            AudioPcmNativeOwnerBindingStatus as Status,
        };
        match self.state.lock().delivery.install_native_owner_bindings(claim, cancellation) {
            Ok(0) => Ok(Status::Unavailable(Refusal::UnsupportedAdapter)),
            Ok(_) => Ok(Status::Ready),
            Err(mondrian_audio::AudioNativeOwnerError::Cancelled) => {
                Ok(Status::Unavailable(Refusal::Cancelled))
            }
            Err(mondrian_audio::AudioNativeOwnerError::Installation(_)) => {
                Ok(Status::Unavailable(Refusal::Superseded))
            }
            Err(error) => Err(audio_render_error(
                "native_audio_owner_bindings",
                error.to_string(),
            )),
        }
    }

    fn query_native_parameter(
        &self,
        query: &mondrian_core::NativeAudioParameterQuery,
        cancellation: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<mondrian_core::NativeAudioParameterQueryOutcome> {
        use mondrian_core::{
            NativeAudioParameterQueryOutcome as Out, NativeAudioParameterQueryRefusal as Refusal,
        };
        if cancellation.is_canceled() {
            return Err(mondrian_core::MondrianError::Cancelled);
        }
        if !self.native_author_snapshot_current() {
            return Ok(Out::Refused(Refusal::OwnerChanged));
        }
        let outcome = self.state.lock().delivery.query_native_parameter(query, cancellation);
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(mondrian_audio::AudioNativeOwnerError::Cancelled) => {
                return Err(mondrian_core::MondrianError::Cancelled)
            }
            Err(error) => {
                return Err(audio_render_error(
                    "native_audio_parameter_query",
                    error.to_string(),
                ))
            }
        };
        if cancellation.is_canceled() {
            return Err(mondrian_core::MondrianError::Cancelled);
        }
        if !self.native_author_snapshot_current() {
            return Ok(Out::Refused(Refusal::OwnerChanged));
        }
        Ok(outcome)
    }
    fn control_native_editor(
        &self,
        intent: mondrian_core::NativeAudioEditorControlIntent,
        cancellation: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<mondrian_core::NativeAudioEditorControlOutcome> {
        use mondrian_core::{
            NativeAudioEditorControlIntent as Intent, NativeAudioEditorControlOutcome as Outcome,
            NativeAudioEditorControlRefusal as Refusal,
        };
        let opening = matches!(intent, Intent::Open { .. } | Intent::ResolveAndOpen { .. });
        if opening && !self.native_author_snapshot_current() {
            return Ok(Outcome::Refused(Refusal::OwnerChanged));
        }
        let mut state = self.state.lock();
        let outcome =
            state.delivery.control_native_editor(intent, cancellation).map_err(|error| {
                audio_render_error("native_audio_editor_control", error.to_string())
            })?;
        // Admission guards never span SDK/window work. If canonical publication
        // raced attach, clean up only the exact returned target on this worker.
        if opening && !self.native_author_snapshot_current() {
            if let Outcome::Opened { target, .. } = outcome {
                let cleanup = state
                    .delivery
                    .control_native_editor(Intent::Close { target }, cancellation)
                    .map_err(|error| {
                        audio_render_error("native_audio_editor_control", error.to_string())
                    })?;
                if matches!(cleanup, Outcome::ClosePending { .. }) {
                    return Ok(cleanup);
                }
            }
            return Ok(Outcome::Refused(Refusal::OwnerChanged));
        }
        Ok(outcome)
    }

    fn render(
        &self,
        request: AudioPcmRenderRequest,
        cancellation: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
        if request.sample_rate != self.sample_rate || request.channel_layout != self.channel_layout
        {
            return Err(audio_render_error(
                "timeline_audio_render_contract",
                format!(
                    "requested {} Hz/{:?} but Adapter is configured for {} Hz/{:?}",
                    request.sample_rate,
                    request.channel_layout,
                    self.sample_rate,
                    self.channel_layout
                ),
            ));
        }
        let mut state = self.state.lock();
        let max_block_frames = state.delivery.program_render_contract().max_block_frames;
        if request.frame_count > max_block_frames {
            return Err(audio_render_error(
                "timeline_audio_render_contract",
                format!(
                    "requested {} frames but the prepared maximum is {}",
                    request.frame_count, max_block_frames
                ),
            ));
        }
        let samples = request
            .frame_count
            .checked_mul(self.channel_layout.channel_count())
            .ok_or_else(|| {
                audio_render_error("timeline_audio_sample_range", "audio window is too large")
            })?;
        let _end_sample = request
            .start_sample
            .checked_add(i64::try_from(request.frame_count).map_err(|_| {
                audio_render_error("timeline_audio_sample_range", "audio window is too large")
            })?)
            .ok_or_else(|| {
                audio_render_error("timeline_audio_sample_range", "audio window is too large")
            })?;
        let mut output = vec![0.0; samples];
        let generation = request.continuity.generation();
        match request.continuity {
            AudioPcmContinuity::Enter(_) => {
                if state.generation == Some(generation) {
                    return Err(audio_render_error(
                        "timeline_audio_continuity",
                        format!(
                            "render generation {} attempted to enter twice",
                            generation.get()
                        ),
                    ));
                }
                if let Err(error) = state.delivery.enter_state_cancellable(
                    AudioContinuityEpoch::new(generation.get()),
                    request.start_sample,
                    cancellation,
                ) {
                    return match error {
                        mondrian_audio::AudioDeliveryError::ProgramExecution(
                            mondrian_audio::AudioExecutionError::ReprepareRequired(ticket),
                        ) => Ok(AudioPcmRenderOutcome::DeferredForReprepare(
                            ticket.product_notice(),
                        )),
                        mondrian_audio::AudioDeliveryError::ProgramExecution(
                            mondrian_audio::AudioExecutionError::NativeAuthorPending(hold),
                        ) => Ok(AudioPcmRenderOutcome::DeferredForNativeAuthor(Box::new(
                            hold.target,
                        ))),
                        mondrian_audio::AudioDeliveryError::ProgramExecution(
                            mondrian_audio::AudioExecutionError::EditorCleanupRequired(target),
                        ) => Ok(AudioPcmRenderOutcome::DeferredForEditorCleanup(*target)),
                        error => Err(audio_render_error(
                            "timeline_audio_state_entry",
                            error.to_string(),
                        )),
                    };
                }
                state.generation = Some(generation);
            }
            AudioPcmContinuity::Continue(_) if state.generation != Some(generation) => {
                return Err(audio_render_error(
                    "timeline_audio_continuity",
                    format!(
                        "render generation {} continued without a matching entry",
                        generation.get()
                    ),
                ));
            }
            AudioPcmContinuity::Continue(_) => {}
        }
        // Audio Delivery checks the submitted-window sequence after the SAME
        // whole-root native preflight, independently of private DSP history.
        // A queued later window must retain the typed native hold.
        if let Err(error) = state.delivery.render_into_cancellable(
            AudioRenderRequest {
                start_sample: request.start_sample,
                frames: request.frame_count,
            },
            &mut output,
            cancellation,
        ) {
            return match error {
                mondrian_audio::AudioDeliveryError::ProgramExecution(
                    mondrian_audio::AudioExecutionError::ReprepareRequired(ticket),
                ) => Ok(AudioPcmRenderOutcome::DeferredForReprepare(
                    ticket.product_notice(),
                )),
                mondrian_audio::AudioDeliveryError::ProgramExecution(
                    mondrian_audio::AudioExecutionError::NativeAuthorPending(hold),
                ) => Ok(AudioPcmRenderOutcome::DeferredForNativeAuthor(Box::new(
                    hold.target,
                ))),
                mondrian_audio::AudioDeliveryError::ProgramExecution(
                    mondrian_audio::AudioExecutionError::EditorCleanupRequired(target),
                ) => Ok(AudioPcmRenderOutcome::DeferredForEditorCleanup(*target)),
                error => Err(audio_render_error(
                    "timeline_audio_execute",
                    error.to_string(),
                )),
            };
        }
        Ok(AudioPcmRenderOutcome::Rendered(AudioBuffer {
            samples: output,
            sample_rate: self.sample_rate,
            channel_layout: self.channel_layout,
        }))
    }
}

struct PlaybackMediaResolver {
    library: Arc<AssetLibrary>,
    source_cache: Arc<AudioSourceCache>,
}

impl AudioMediaResolver for PlaybackMediaResolver {
    fn resolve(
        &self,
        asset_id: AssetId,
        component_id: AudioSourceComponentId,
        sample_rate: u32,
    ) -> Result<ResolvedAudioSource, String> {
        if self.source_cache.sample_rate() != sample_rate {
            return Err(format!(
                "audio source cache rate {} does not match render rate {}",
                self.source_cache.sample_rate(),
                sample_rate
            ));
        }
        let asset = self
            .library
            .get_asset(asset_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("Asset {asset_id} is unavailable"))?;
        let path = asset
            .file_path()
            .ok_or_else(|| format!("Asset {asset_id} is not a file-backed audio source"))?;
        let media_probe = asset.media_probe().ok_or_else(|| {
            format!("Asset {asset_id} has no coherent media probe for audio execution")
        })?;
        let source_fingerprint = asset.source_fingerprint().ok_or_else(|| {
            format!("Asset {asset_id} has no admitted source revision for audio execution")
        })?;
        let stream = asset
            .audio_components
            .resolve_current(component_id, media_probe, source_fingerprint)
            .map_err(|error| {
                format!(
                    "failed to bind audio Component {component_id} for Asset {asset_id}: {error}"
                )
            })?;
        let selection = AudioSourceSelection::from_stream(stream, source_fingerprint);
        let source = self.source_cache.open(path, selection).map_err(|error| {
            format!(
                "failed to open bounded audio source {} at {}: {error}",
                asset.id,
                path.display()
            )
        })?;
        Ok(ResolvedAudioSource::new(
            source.channel_layout(),
            Arc::new(PlaybackDecodedAudioSource(source)),
        ))
    }
}

struct PlaybackDecodedAudioSource(AudioSourceReader);

impl AudioDecodedSource for PlaybackDecodedAudioSource {
    fn read_interleaved(
        &self,
        start_frame: i64,
        frames: usize,
        destination: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), String> {
        self.0
            .read_interleaved_cancellable(start_frame, frames, destination, cancellation)
            .map_err(|error| error.to_string())
    }
}

fn audio_render_error(
    step_id: &'static str,
    reason: impl Into<String>,
) -> mondrian_core::MondrianError {
    mondrian_core::MondrianError::WorkflowStepFailed {
        step_id: step_id.to_owned(),
        reason: reason.into(),
    }
}

/// Immutable request on Media's one serial queue. The concrete and erased Arc
/// alias the SAME installed renderer; neither is a second native controller.
struct NativeAudioAuthorHandoffRequest {
    renderer: Arc<TimelineAudioPcmRenderer>,
    source: Arc<dyn AudioPcmRenderer>,
    proposal: mondrian_core::NativeAudioAuthorProposal,
    target: mondrian_core::NativeAudioEditorTarget,
    snapshot: Arc<mondrian_editor_state::AuthorExecutionSnapshot>,
    programs: Arc<[mondrian_audio::NativeAudioAuthorProgram]>,
    intent: mondrian_media::AudioPcmNativeAuthorHandoffIntent,
}

impl TimelineAudioPcmRenderer {
    pub(super) fn native_author_compile_request(
        &self,
        output_id: mondrian_core::ProgramOutputId,
    ) -> AudioCompileRequest {
        AudioCompileRequest { output_id, audition: self.audition.clone() }
    }

    /// Original producer owns the complete preparation across canonical publication.
    /// The shared host slot contains only the actual committed author snapshot.
    pub(super) fn native_author_terminal_batch_request(
        self: &Arc<Self>,
        anchor: mondrian_core::NativeAudioAuthorProposal,
        proposals: Arc<[mondrian_core::NativeAudioAuthorProposal]>,
        programs: Arc<[mondrian_audio::NativeAudioAuthorProgram]>,
        publication: Arc<Mutex<Option<Arc<mondrian_editor_state::AuthorExecutionSnapshot>>>>,
    ) -> Box<dyn mondrian_media::AudioPcmNativeAuthorHandoff> {
        native_author_terminal_batch::request(self, anchor, proposals, programs, publication)
    }

    pub(super) fn native_author_handoff_request(
        self: &Arc<Self>,
        proposal: mondrian_core::NativeAudioAuthorProposal,
        target: mondrian_core::NativeAudioEditorTarget,
        snapshot: Arc<mondrian_editor_state::AuthorExecutionSnapshot>,
        programs: Arc<[mondrian_audio::NativeAudioAuthorProgram]>,
        intent: mondrian_media::AudioPcmNativeAuthorHandoffIntent,
    ) -> Box<dyn mondrian_media::AudioPcmNativeAuthorHandoff> {
        let source: Arc<dyn AudioPcmRenderer> = self.clone();
        Box::new(NativeAudioAuthorHandoffRequest {
            renderer: self.clone(),
            source,
            proposal,
            target,
            snapshot,
            programs,
            intent,
        })
    }
}

fn native_handoff_refusal(
    error: &mondrian_audio::NativeAudioParameterHandoffError,
) -> mondrian_media::AudioPcmNativeAuthorHandoffRefusal {
    use mondrian_audio::NativeAudioParameterHandoffError as Error;
    use mondrian_media::AudioPcmNativeAuthorHandoffRefusal as Refusal;
    match error {
        Error::Resources(_) => Refusal::ResourceAdmission,
        Error::Admission(mondrian_audio::NativeAudioAuthorAdmissionError::Cancelled) => {
            Refusal::Cancelled
        }
        Error::Admission(mondrian_audio::NativeAudioAuthorAdmissionError::Pending { .. }) => {
            Refusal::AuthorStatePending
        }
        Error::Owner(_) | Error::Installation(_) => Refusal::Superseded,
        _ => Refusal::ProcessorRejected,
    }
}

impl mondrian_media::AudioPcmNativeAuthorHandoff for NativeAudioAuthorHandoffRequest {
    fn renderer(&self) -> &Arc<dyn AudioPcmRenderer> {
        &self.source
    }
    fn proposal(&self) -> &mondrian_core::NativeAudioAuthorProposal {
        &self.proposal
    }
    fn expected_target(&self) -> mondrian_core::NativeAudioEditorTarget {
        self.target
    }
    fn intent(&self) -> mondrian_media::AudioPcmNativeAuthorHandoffIntent {
        self.intent
    }

    fn continuity_policy(
        &self,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<
        mondrian_media::AudioPcmNativeAuthorHandoffContinuity,
        mondrian_media::AudioPcmNativeAuthorHandoffRefusal,
    > {
        use mondrian_media::{
            AudioPcmNativeAuthorHandoffContinuity as Continuity,
            AudioPcmNativeAuthorHandoffRefusal as Refusal,
        };
        if cancellation.is_canceled() {
            return Err(Refusal::Cancelled);
        }
        let required = self
            .renderer
            .state
            .lock()
            .delivery
            .native_author_state_entry_required(&self.proposal)
            .map_err(|_| Refusal::Superseded)?;
        Ok(if required {
            Continuity::StateEntryRequired
        } else {
            Continuity::Preserved
        })
    }

    fn apply(
        &self,
        destination: mondrian_core::AudioOwnerInstallationVerifier,
        authorization: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
        cancellation: &ExecutionCancellationToken,
    ) -> mondrian_media::AudioPcmNativeAuthorHandoffOutcome {
        use mondrian_media::{
            AudioPcmNativeAuthorHandoffOutcome as Outcome,
            AudioPcmNativeAuthorHandoffRefusal as Refusal,
        };
        if self.intent != mondrian_media::AudioPcmNativeAuthorHandoffIntent::Install {
            return Outcome::Refused(Refusal::Superseded);
        }
        let mut state = self.renderer.state.lock();
        let mut prepared = match state.delivery.prepare_native_author_parameter_handoff(
            &self.programs,
            &self.proposal,
            destination,
            cancellation,
        ) {
            Ok(prepared) => prepared,
            Err(error) => return Outcome::Refused(native_handoff_refusal(&error)),
        };
        // Prepare Arc owners before the short final guard. Arc payload and
        // allocation headers remain explicitly unqualified physical frontiers.
        let next_snapshot = Some(Arc::clone(&self.snapshot));
        let mut metadata = self.renderer.author_snapshot.write();
        let author_guard = match self.snapshot.enter_commit() {
            Ok(guard) => guard,
            Err(_) => {
                drop(metadata);
                drop(prepared);
                return Outcome::Refused(Refusal::Superseded);
            }
        };
        let installed = state.delivery.install_native_author_parameter_handoff(
            &mut prepared,
            authorization,
            cancellation,
        );
        let retired = if installed.is_ok() {
            std::mem::replace(&mut *metadata, next_snapshot)
        } else {
            None
        };
        drop(author_guard);
        drop(metadata);
        // Actual meter publisher changes with the new Session. This private
        // metadata slot is swapped outside all final guards and read by Main
        // without acquiring the native/DSP Runtime mutex.
        if installed.as_ref().is_ok_and(|receipt| {
            receipt.continuity
                == mondrian_audio::NativeAudioParameterHandoffContinuity::StateEntryRequired
        }) {
            state.generation = None;
            let next_meter = state.delivery.meter_observer();
            let retired_meter =
                std::mem::replace(&mut *self.renderer.meter_metadata.write(), next_meter);
            drop(retired_meter);
        }
        // No Plan, factory, payload or old Arc retirement occurs under either
        // canonical author or Source final guards. SDK Ack also runs outside.
        drop(retired);
        drop(prepared);
        let receipt = match installed {
            Ok(receipt) => receipt,
            Err(error) => return Outcome::Refused(native_handoff_refusal(&error)),
        };
        match state
            .delivery
            .acknowledge_native_author_parameter_handoff(&self.proposal, cancellation)
        {
            Ok(ack) => Outcome::Acknowledged {
                claim: ack.claim,
                target: Box::new(ack.target),
                continuity: handoff_continuity(ack.continuity),
            },
            Err(_) => Outcome::InstalledAwaitingAcknowledgement {
                claim: receipt.claim,
                target: Box::new(receipt.target),
                continuity: handoff_continuity(receipt.continuity),
            },
        }
    }

    fn acknowledge(
        &self,
        cancellation: &ExecutionCancellationToken,
    ) -> mondrian_media::AudioPcmNativeAuthorHandoffOutcome {
        use mondrian_media::{
            AudioPcmNativeAuthorHandoffOutcome as Outcome,
            AudioPcmNativeAuthorHandoffRefusal as Refusal,
        };
        if self.intent != mondrian_media::AudioPcmNativeAuthorHandoffIntent::Acknowledge
            || !self.renderer.native_author_snapshot_current()
        {
            return Outcome::Refused(Refusal::Superseded);
        }
        match self
            .renderer
            .state
            .lock()
            .delivery
            .acknowledge_native_author_parameter_handoff(&self.proposal, cancellation)
        {
            Ok(ack) => Outcome::Acknowledged {
                claim: ack.claim,
                target: Box::new(ack.target),
                continuity: handoff_continuity(ack.continuity),
            },
            Err(error) => Outcome::Refused(native_handoff_refusal(&error)),
        }
    }
}

fn handoff_continuity(
    value: mondrian_audio::NativeAudioParameterHandoffContinuity,
) -> mondrian_media::AudioPcmNativeAuthorHandoffContinuity {
    match value {
        mondrian_audio::NativeAudioParameterHandoffContinuity::Preserved => {
            mondrian_media::AudioPcmNativeAuthorHandoffContinuity::Preserved
        }
        mondrian_audio::NativeAudioParameterHandoffContinuity::StateEntryRequired => {
            mondrian_media::AudioPcmNativeAuthorHandoffContinuity::StateEntryRequired
        }
    }
}

// One cold carrier. The Media work owns the actual renderer; these Weak
// identities do not keep a second Runtime/controller alive.
struct NativeAudioReprepareHandoffRequest {
    renderer: std::sync::Weak<TimelineAudioPcmRenderer>,
    source: std::sync::Weak<dyn AudioPcmRenderer>,
    owner: NativeAudioReprepareOwner,
}

enum NativeAudioReprepareOwner {
    Empty,
    Installed(mondrian_audio::NativeAudioReprepareInstallReceipt),
    Retired(mondrian_audio::NativeAudioReprepareRetirement),
    Failed(mondrian_audio::NativeAudioReprepareError),
    Finished,
}

impl TimelineAudioPcmRenderer {
    pub(super) fn native_reprepare_request(
        self: &Arc<Self>,
    ) -> Box<dyn mondrian_media::AudioPcmNativeReprepareHandoff> {
        let source: Arc<dyn AudioPcmRenderer> = self.clone();
        Box::new(NativeAudioReprepareHandoffRequest {
            renderer: Arc::downgrade(self),
            source: Arc::downgrade(&source),
            owner: NativeAudioReprepareOwner::Empty,
        })
    }
}

fn reprepare_refusal(
    error: &mondrian_audio::NativeAudioReprepareError,
) -> mondrian_media::AudioPcmNativeReprepareRefusal {
    use mondrian_audio::NativeAudioReprepareError as E;
    use mondrian_media::AudioPcmNativeReprepareRefusal as R;
    match error {
        E::Cancelled => R::Cancelled,
        E::Resources(_) => R::ResourceAdmission,
        E::Owner(_) | E::Admission(_) => R::Superseded,
        _ => R::ProcessorRejected,
    }
}

impl NativeAudioReprepareHandoffRequest {
    fn failed(
        &mut self,
        error: mondrian_audio::NativeAudioReprepareError,
    ) -> mondrian_media::AudioPcmNativeReprepareRefusal {
        let refusal = reprepare_refusal(&error);
        // The original first error is retained until the SAME carrier retires.
        // This method is called after Runtime/Source/metadata guards are gone.
        if !matches!(self.owner, NativeAudioReprepareOwner::Failed(_)) {
            self.owner = NativeAudioReprepareOwner::Failed(error);
            if let NativeAudioReprepareOwner::Failed(error) = &self.owner {
                tracing::warn!(%error, "retained native restart refused; keeping its original owner hold");
            }
        }
        refusal
    }
}

impl mondrian_media::AudioPcmNativeReprepareHandoff for NativeAudioReprepareHandoffRequest {
    fn renderer(&self) -> &std::sync::Weak<dyn AudioPcmRenderer> {
        &self.source
    }

    fn install(
        &mut self,
        expected_notice: mondrian_core::NativeAudioReprepareNotice,
        destination: mondrian_core::AudioOwnerInstallationVerifier,
        authorization: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<
        mondrian_core::AudioOwnerInstallationClaim,
        mondrian_media::AudioPcmNativeReprepareRefusal,
    > {
        use mondrian_media::AudioPcmNativeReprepareRefusal as R;
        if !matches!(self.owner, NativeAudioReprepareOwner::Empty) {
            return Err(R::Superseded);
        }
        let Some(renderer) = self.renderer.upgrade() else {
            return Err(R::Superseded);
        };
        let mut state = renderer.state.lock();
        if !state
            .delivery
            .native_owner_installation_verifier()
            .is_some_and(|actual| actual.same_registration(&destination))
        {
            return Err(R::Superseded);
        }
        let result = (|| {
            // The notice is only an equality hint. Audio reads its actual pending
            // Session ticket and owns capture/prepare/seed/CAS and every failure.
            let operation =
                state.delivery.service_pending_native_reprepare(expected_notice, cancellation)?;
            state
                .delivery
                .prepare_native_reprepare_plan(&operation, destination, cancellation)?;
            state
                .delivery
                .install_native_reprepare_plan(&operation, authorization, cancellation)
        })();
        match result {
            Ok(receipt) => {
                let claim = receipt.claim().clone();
                // Adopt BEFORE any following action can fail or unwind.
                self.owner = NativeAudioReprepareOwner::Installed(receipt);
                state.generation = None;
                let next_meter = state.delivery.meter_observer();
                let retired_meter =
                    std::mem::replace(&mut *renderer.meter_metadata.write(), next_meter);
                drop(state);
                drop(retired_meter);
                Ok(claim)
            }
            Err(error) => {
                drop(state);
                Err(self.failed(error))
            }
        }
    }

    fn retire(&mut self) -> Result<(), mondrian_media::AudioPcmNativeReprepareRefusal> {
        let owner = std::mem::replace(&mut self.owner, NativeAudioReprepareOwner::Finished);
        match owner {
            NativeAudioReprepareOwner::Installed(receipt) => {
                // NO renderer upgrade, Runtime/Source lock or credit publication.
                self.owner = NativeAudioReprepareOwner::Retired(receipt.retire());
                Ok(())
            }
            owner => {
                self.owner = owner;
                Err(mondrian_media::AudioPcmNativeReprepareRefusal::RetirementUnconfirmed)
            }
        }
    }

    fn settle(&mut self) -> Result<(), mondrian_media::AudioPcmNativeReprepareRefusal> {
        use mondrian_media::AudioPcmNativeReprepareRefusal as R;
        let Some(renderer) = self.renderer.upgrade() else {
            return Err(R::Superseded);
        };
        let owner = std::mem::replace(&mut self.owner, NativeAudioReprepareOwner::Finished);
        let NativeAudioReprepareOwner::Retired(completion) = owner else {
            self.owner = owner;
            return Err(R::RetirementUnconfirmed);
        };
        let mut state = renderer.state.lock();
        // Pure resource settlement: old PCM cancellation never discards it.
        let result = state.delivery.finish_native_reprepare_retirement(completion);
        drop(state);
        result.map_err(|error| self.failed(error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod host_output_fader_publication_contracts;
    mod playing_host_fader_continuity;

    #[derive(Debug, Default, Clone, Copy)]
    struct SettlementSnapshot {
        scopes: u32,
        children: u32,
        unavailable: bool,
    }

    #[derive(Default)]
    struct SettlementCounts(Mutex<SettlementSnapshot>);

    impl mondrian_core::NativeChildSettlementReporter for SettlementCounts {
        fn begin_scope(&self) -> Result<(), mondrian_core::NativeChildSettlementError> {
            let mut state = self.0.lock();
            let Some(next) = state.scopes.checked_add(1) else {
                state.unavailable = true;
                return Err(mondrian_core::NativeChildSettlementError::Overflow);
            };
            state.scopes = next;
            Ok(())
        }

        fn end_scope(&self) {
            let mut state = self.0.lock();
            if let Some(next) = state.scopes.checked_sub(1) {
                state.scopes = next;
            } else {
                state.unavailable = true;
            }
        }

        fn begin_child(&self) -> Result<(), mondrian_core::NativeChildSettlementError> {
            let mut state = self.0.lock();
            if state.scopes == 0 {
                state.unavailable = true;
                return Err(mondrian_core::NativeChildSettlementError::Unavailable);
            }
            let Some(next) = state.children.checked_add(1) else {
                state.unavailable = true;
                return Err(mondrian_core::NativeChildSettlementError::Overflow);
            };
            state.children = next;
            Ok(())
        }

        fn end_child(&self, settlement: mondrian_core::NativeChildSettlement) {
            let mut state = self.0.lock();
            if let Some(next) = state.children.checked_sub(1) {
                state.children = next;
            } else {
                state.unavailable = true;
            }
            if settlement == mondrian_core::NativeChildSettlement::Unconfirmed {
                state.unavailable = true;
            }
        }

        fn unavailable(&self) {
            self.0.lock().unavailable = true;
        }
    }

    fn settlement_model_renderer() -> (tempfile::TempDir, Arc<TimelineAudioPcmRenderer>) {
        let root = tempfile::tempdir().expect("private library directory");
        let library = AssetLibrary::open(root.path().join("library")).expect("asset library");
        let renderer = TimelineAudioPcmRenderer::new_public_program(
            Sequence::new("settlement scope model"),
            Vec::new(),
            library,
            Arc::new(AudioSourceCache::new(48_000)),
            test_runtime_grant(),
        )
        .expect("actual Timeline/Delivery/Runtime with no native plugin");
        (root, Arc::new(renderer))
    }

    #[test]
    fn native_restart_provider_retains_original_no_pending_error_without_clearing_generation() {
        use mondrian_media::AudioPcmNativeReprepareHandoff;
        let (_directory, renderer) = settlement_model_renderer();
        let source = mondrian_core::AudioOwnerInstallationSource::new();
        let (registration, verifier) = source.register().unwrap();
        let previous = source
            .begin_commit(source.reserve_install(&registration, None).unwrap())
            .unwrap()
            .commit()
            .unwrap();
        renderer.bind_owner_installation(verifier.clone()).unwrap();
        let generation = AudioPcmRenderGeneration::new(9);
        renderer.state.lock().generation = Some(generation);
        let erased: Arc<dyn AudioPcmRenderer> = renderer.clone();
        let mut provider = NativeAudioReprepareHandoffRequest {
            renderer: Arc::downgrade(&renderer),
            source: Arc::downgrade(&erased),
            owner: NativeAudioReprepareOwner::Empty,
        };
        let mut authorization = source
            .authorize_runtime_installation(
                source.reserve_install(&registration, Some(&previous)).unwrap(),
            )
            .unwrap();
        let notice = mondrian_core::NativeAudioReprepareNotice {
            instance_id: mondrian_core::AudioProcessorInstanceId::new(),
            request: mondrian_core::NativeAudioReprepareRequest::ClapRestart,
        };
        assert!(matches!(
            provider.install(
                notice,
                verifier.clone(),
                &mut authorization,
                &ExecutionCancellationToken::new()
            ),
            Err(mondrian_media::AudioPcmNativeReprepareRefusal::Superseded)
        ));
        assert!(matches!(
            provider.owner,
            NativeAudioReprepareOwner::Failed(
                mondrian_audio::NativeAudioReprepareError::Admission(_)
            )
        ));
        assert_eq!(renderer.state.lock().generation, Some(generation));
        verifier.validate(&previous).unwrap();
        assert_eq!(
            provider.retire(),
            Err(mondrian_media::AudioPcmNativeReprepareRefusal::RetirementUnconfirmed)
        );
        assert!(matches!(
            provider.owner,
            NativeAudioReprepareOwner::Failed(_)
        ));
    }

    #[test]
    fn timeline_settlement_scope_survives_concrete_and_erased_renderer_aliases() {
        let (_directory, renderer) = settlement_model_renderer();
        let counts = Arc::new(SettlementCounts::default());
        let reporter: Arc<dyn mondrian_core::NativeChildSettlementReporter> = counts.clone();
        let root = mondrian_core::NativeChildSettlementRegistrar::try_new(reporter).unwrap();
        renderer.bind_native_child_settlement(&root).expect("real Delivery forwarding");
        let before = *counts.0.lock();
        assert!(before.scopes > 1);
        assert_eq!(
            before.children, 0,
            "this is a host model, not a native lifecycle gate"
        );
        renderer.bind_native_child_settlement(&root).expect("same-domain idempotence");
        assert_eq!(counts.0.lock().scopes, before.scopes);

        // These are the concrete/erased aliases retained by the existing
        // handoff request. Neither is a second Runtime or cleanup owner.
        let concrete = Arc::clone(&renderer);
        let erased: Arc<dyn AudioPcmRenderer> = renderer.clone();
        let weak = Arc::downgrade(&renderer);
        drop(renderer);
        drop(root);
        assert!(counts.0.lock().scopes > 0);
        drop(concrete);
        assert!(weak.upgrade().is_some());
        assert!(counts.0.lock().scopes > 0);
        drop(erased);
        assert!(weak.upgrade().is_none());
        let final_state = *counts.0.lock();
        assert_eq!((final_state.scopes, final_state.children), (0, 0));
        assert!(!final_state.unavailable);
    }

    #[test]
    fn timeline_settlement_foreign_rebind_keeps_both_domains_unavailable() {
        let (_directory, renderer) = settlement_model_renderer();
        let first = Arc::new(SettlementCounts::default());
        let second = Arc::new(SettlementCounts::default());
        let first_sink: Arc<dyn mondrian_core::NativeChildSettlementReporter> = first.clone();
        let second_sink: Arc<dyn mondrian_core::NativeChildSettlementReporter> = second.clone();
        let a = mondrian_core::NativeChildSettlementRegistrar::try_new(first_sink).unwrap();
        let b = mondrian_core::NativeChildSettlementRegistrar::try_new(second_sink).unwrap();
        renderer.bind_native_child_settlement(&a).unwrap();
        let first_scopes = first.0.lock().scopes;
        assert_eq!(
            renderer.bind_native_child_settlement(&b),
            Err(mondrian_core::NativeChildSettlementError::ForeignDomain)
        );
        assert_eq!(first.0.lock().scopes, first_scopes);
        assert_eq!(
            second.0.lock().scopes,
            1,
            "no partial migration into the second domain"
        );
        assert!(first.0.lock().unavailable && second.0.lock().unavailable);
        drop(renderer);
        drop(a);
        drop(b);
        assert_eq!(first.0.lock().scopes, 0);
        assert_eq!(second.0.lock().scopes, 0);
        assert!(first.0.lock().unavailable && second.0.lock().unavailable);
    }

    #[derive(Default)]
    struct QuantumRecordingResolver(Mutex<Vec<usize>>);

    impl AudioProcessorResolver for QuantumRecordingResolver {
        fn prepare(
            &self,
            request: mondrian_audio::AudioProcessorPrepareRequest<'_>,
        ) -> Result<
            Arc<dyn mondrian_audio::AudioProcessorFactory>,
            mondrian_audio::AudioProcessorHostError,
        > {
            self.0.lock().push(request.render_contract().max_block_frames);
            BuiltInAudioProcessorResolver.prepare(request)
        }
    }

    #[test]
    fn timeline_pcm_adapter_uses_the_prepared_quantum_before_entry() {
        let root = std::env::temp_dir().join(format!(
            "mondrian-audio-quantum-{}",
            mondrian_core::ProjectId::new()
        ));
        let library = AssetLibrary::open(root.clone()).expect("asset library");
        let mut sequence = Sequence::new("configured quantum");
        sequence.audio_program.outputs[0].strip.pre_fader.processors.push(
            mondrian_timeline::audio::AudioProcessorInstance::built_in(
                mondrian_timeline::audio::BUILTIN_GAIN_DEFINITION_ID,
                1,
            ),
        );
        let resolver = QuantumRecordingResolver::default();
        let prepare = |max_block_frames| {
            TimelineAudioPcmRenderer::new_with_processor_resolver(
                sequence.clone(),
                Vec::new(),
                Arc::clone(&library),
                Arc::new(AudioSourceCache::new(48_000)),
                test_runtime_grant(),
                AudioAuditionOverlay::default(),
                48_000,
                AudioChannelLayout::Stereo,
                max_block_frames,
                &resolver,
                None,
            )
        };
        assert!(
            matches!(prepare(0), Err(mondrian_core::MondrianError::WorkflowStepFailed {
            step_id, ..
        }) if step_id == "timeline_audio_render_contract")
        );
        assert!(
            resolver.0.lock().is_empty(),
            "zero rejects before processor preparation"
        );
        let renderer = prepare(7).expect("configured renderer");
        assert_eq!(*resolver.0.lock(), vec![7]);
        assert_eq!(
            renderer.state.lock().delivery.program_render_contract().max_block_frames,
            7
        );
        let before = renderer.meter_observer().latest();
        let generation = AudioPcmRenderGeneration::new(19);
        let cancellation = ExecutionCancellationToken::new();
        let request = |continuity, start_sample, frame_count| AudioPcmRenderRequest {
            continuity,
            start_sample,
            frame_count,
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
        };
        assert!(matches!(renderer.render(
            request(AudioPcmContinuity::Enter(generation), i64::MAX, 8), &cancellation,
        ), Err(mondrian_core::MondrianError::WorkflowStepFailed {
            step_id, reason,
        }) if step_id == "timeline_audio_render_contract" && reason.contains("prepared maximum is 7")));
        assert!(renderer.state.lock().generation.is_none());
        assert_eq!(renderer.meter_observer().latest(), before);
        for (continuity, start_sample) in [
            (AudioPcmContinuity::Enter(generation), 0),
            (AudioPcmContinuity::Continue(generation), 7),
        ] {
            let AudioPcmRenderOutcome::Rendered(output) = renderer
                .render(request(continuity, start_sample, 7), &cancellation)
                .expect("legal contiguous quantum after refusal")
            else {
                panic!("expected actual PCM");
            };
            assert_eq!(output.samples, vec![0.0; 14]);
        }
        drop(renderer);
        let reference = TimelineAudioPcmRenderer::new(
            Sequence::new("explicit reference contract"),
            Vec::new(),
            library,
            Arc::new(AudioSourceCache::new(48_000)),
            test_runtime_grant(),
            AudioAuditionOverlay::default(),
            48_000,
            AudioChannelLayout::Stereo,
        )
        .expect("reference renderer");
        assert_eq!(
            reference.state.lock().delivery.program_render_contract().max_block_frames,
            MAX_AUDIO_RENDER_BLOCK_FRAMES
        );
        drop(reference);
        let _ = std::fs::remove_dir_all(root);
    }

    fn test_runtime_grant() -> AudioRuntimeResourceGrant {
        AudioRuntimeResourceGrant::new(64, 512 * 1024 * 1024, 128 * 1024 * 1024)
    }

    #[test]
    fn note_pcm_adapter_publishes_its_actual_prepared_origin() {
        // Owned factory/score control only: no native plugin or PCM processing.
        struct Model;
        impl mondrian_audio::AudioProcessorResolver for Model {
            fn prepare(
                &self,
                request: mondrian_audio::AudioProcessorPrepareRequest<'_>,
            ) -> Result<
                Arc<dyn mondrian_audio::AudioProcessorFactory>,
                mondrian_audio::AudioProcessorHostError,
            > {
                assert!(request.note_input_requested());
                Ok(Arc::new(Model))
            }
        }
        impl mondrian_audio::AudioProcessorFactory for Model {
            fn execution_contract(&self) -> mondrian_audio::AudioProcessorExecutionContract {
                mondrian_audio::AudioProcessorExecutionContract::stateless()
            }
            fn note_input_contract(&self) -> mondrian_audio::AudioProcessorNoteInputContract {
                mondrian_audio::AudioProcessorNoteInputContract {
                    completion: mondrian_audio::AudioNoteCompletionPolicy::NativeEnd,
                    ports: vec![mondrian_audio::AudioProcessorNoteInputPort {
                        channel_count: 16,
                        port_key: "clap.note-input.0".to_owned(),
                        native_id: 0,
                        native_index: 0,
                    }],
                    limits: mondrian_audio::AudioNoteExecutionLimits {
                        input_events: 4,
                        output_ends: 4,
                        retained_records: 4,
                        live_voices: 4,
                    },
                }
            }
            fn create(
                &self,
                _: Option<&mondrian_core::NativeChildSettlementRegistrar>,
            ) -> Result<
                Box<dyn mondrian_audio::AudioProcessor>,
                mondrian_audio::AudioProcessorHostError,
            > {
                Ok(Box::new(Model))
            }
        }
        impl mondrian_audio::AudioProcessor for Model {
            fn enter_state(
                &mut self,
                _: i64,
            ) -> Result<(), mondrian_audio::AudioProcessorHostError> {
                panic!("the origin metadata query must not enter DSP")
            }

            fn process(
                &mut self,
                _: mondrian_audio::AudioProcessorProcessContext,
                _: &mut dyn mondrian_audio::AudioProcessorAudioIo,
                _: mondrian_audio::AudioParameterEventBatch<'_>,
            ) -> Result<(), mondrian_audio::AudioProcessorHostError> {
                panic!("the origin metadata query must not process PCM")
            }
        }
        use mondrian_core::{
            AudioNoteId, AudioNoteLaneId, AudioProcessorInstanceId, AuthoringList,
        };
        use mondrian_timeline::audio::{
            AudioNote, AudioNoteLane, AudioNoteTarget, AudioProcessorDefinitionRef,
            AudioProcessorInstance,
        };
        let mut sequence = Sequence::new("actual prepared origin");
        let track = sequence.audio_tracks[0].id;
        let processor = AudioProcessorInstance {
            id: AudioProcessorInstanceId::new(),
            definition: AudioProcessorDefinitionRef::Clap {
                plugin_id: "test.owned.origin".to_owned(),
                schema_version: 1,
                binary_sha256: Some([7; 32]),
            },
            bypassed: false,
            parameters: Default::default(),
            opaque_state: None,
        };
        let processor_id = processor.id;
        sequence
            .audio_program
            .track_channels
            .get_mut(&track)
            .unwrap()
            .strip
            .pre_fader
            .processors
            .push(processor);
        sequence.audio_program.note_lanes.push(AudioNoteLane {
            schema_version: 1,
            id: AudioNoteLaneId::new(),
            target: AudioNoteTarget {
                track_id: track,
                processor_id,
                input_port_key: "clap.note-input.0".to_owned(),
            },
            notes: AuthoringList::from(vec![AudioNote {
                id: AudioNoteId::new(),
                start: TimelineTime::ZERO,
                end: TimelineTime::new(1, 48_000).unwrap(),
                channel: 0,
                key: 60,
                on_velocity: 1.0,
                off_velocity: 0.0,
            }]),
        });
        let root = std::env::temp_dir().join(format!(
            "mondrian-origin-{}",
            mondrian_core::ProjectId::new()
        ));
        let renderer = TimelineAudioPcmRenderer::new_with_processor_resolver(
            sequence,
            Vec::new(),
            AssetLibrary::open(root.clone()).unwrap(),
            Arc::new(AudioSourceCache::new(48_000)),
            test_runtime_grant(),
            AudioAuditionOverlay::default(),
            48_000,
            AudioChannelLayout::Stereo,
            7,
            &Model,
            None,
        )
        .unwrap();
        assert_eq!(
            renderer.continuity_model(),
            AudioPcmContinuityModel::GenerationState
        );
        assert_eq!(
            renderer.state.lock().delivery.program_render_contract().max_block_frames,
            7
        );
        drop(renderer);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn timeline_pcm_adapter_requires_one_entry_and_exact_generation_continuation() {
        let root = std::env::temp_dir().join(format!(
            "mondrian-audio-continuity-{}",
            mondrian_core::ProjectId::new()
        ));
        let sequence = Sequence::new("continuity");
        let renderer = TimelineAudioPcmRenderer::new(
            sequence,
            Vec::new(),
            AssetLibrary::open(root.clone()).expect("asset library"),
            Arc::new(AudioSourceCache::new(48_000)),
            test_runtime_grant(),
            AudioAuditionOverlay::default(),
            48_000,
            AudioChannelLayout::Stereo,
        )
        .expect("stateless renderer");
        let cancellation = ExecutionCancellationToken::new();
        let generation = AudioPcmRenderGeneration::new(7);
        let request = |continuity, start_sample| AudioPcmRenderRequest {
            start_sample,
            frame_count: 4,
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            continuity,
        };

        assert!(renderer
            .render(
                request(AudioPcmContinuity::Continue(generation), 0),
                &cancellation,
            )
            .is_err());
        renderer
            .render(
                request(AudioPcmContinuity::Enter(generation), 0),
                &cancellation,
            )
            .expect("entry");
        renderer
            .render(
                request(AudioPcmContinuity::Continue(generation), 4),
                &cancellation,
            )
            .expect("continuation");
        assert!(renderer
            .render(
                request(AudioPcmContinuity::Continue(generation), 9),
                &cancellation,
            )
            .is_err());
        assert!(renderer
            .render(
                request(AudioPcmContinuity::Enter(generation), 8),
                &cancellation
            )
            .is_err());

        let next_generation = AudioPcmRenderGeneration::new(8);
        renderer
            .render(
                request(AudioPcmContinuity::Enter(next_generation), 9),
                &cancellation,
            )
            .expect("fresh generation entry");
        drop(renderer);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn timeline_pcm_adapter_invalid_extent_refuses_before_entry_and_same_generation_remains_unused()
    {
        let root = std::env::temp_dir().join(format!(
            "mondrian-audio-invalid-entry-{}",
            mondrian_core::ProjectId::new()
        ));
        let renderer = TimelineAudioPcmRenderer::new(
            Sequence::new("invalid initial audio window"),
            Vec::new(),
            AssetLibrary::open(root.clone()).expect("asset library"),
            Arc::new(AudioSourceCache::new(48_000)),
            test_runtime_grant(),
            AudioAuditionOverlay::default(),
            48_000,
            AudioChannelLayout::Stereo,
        )
        .expect("actual stateless renderer");
        let cancellation = ExecutionCancellationToken::new();
        let generation = AudioPcmRenderGeneration::new(7);
        let request = |continuity, start_sample| AudioPcmRenderRequest {
            start_sample,
            frame_count: 4,
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            continuity,
        };
        assert!(renderer
            .render(
                request(AudioPcmContinuity::Enter(generation), i64::MAX),
                &cancellation,
            )
            .is_err());
        assert!(matches!(
            renderer
                .render(
                    request(AudioPcmContinuity::Enter(generation), 0),
                    &cancellation
                )
                .expect("invalid extent consumed neither the generation nor actual entry"),
            AudioPcmRenderOutcome::Rendered(_)
        ));
        assert!(matches!(
            renderer
                .render(
                    request(AudioPcmContinuity::Continue(generation), 4),
                    &cancellation
                )
                .expect("exact first continuation after actual legal entry"),
            AudioPcmRenderOutcome::Rendered(_)
        ));
        drop(renderer);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn timeline_pcm_adapter_maps_sequence_program_layout_at_the_delivery_boundary() {
        let root = std::env::temp_dir().join(format!(
            "mondrian-audio-delivery-layout-{}",
            mondrian_core::ProjectId::new()
        ));
        let mut sequence = Sequence::new("mono program");
        sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
        let renderer = TimelineAudioPcmRenderer::new(
            sequence,
            Vec::new(),
            AssetLibrary::open(root.clone()).expect("asset library"),
            Arc::new(AudioSourceCache::new(48_000)),
            test_runtime_grant(),
            AudioAuditionOverlay::default(),
            48_000,
            AudioChannelLayout::Stereo,
        )
        .expect("mono program with stereo delivery");

        let evidence = renderer.delivery_evidence();
        assert_eq!(evidence.program_layout, AudioChannelLayout::Mono);
        assert_eq!(evidence.target_layout, AudioChannelLayout::Stereo);
        assert_eq!(
            evidence.mapping_kind,
            mondrian_audio::AudioDeliveryMappingKind::ProvenSilence
        );
        assert_eq!(evidence.coefficient_count, 0);

        let output = renderer
            .render(
                AudioPcmRenderRequest {
                    start_sample: 0,
                    frame_count: 4,
                    sample_rate: 48_000,
                    channel_layout: AudioChannelLayout::Stereo,
                    continuity: AudioPcmContinuity::Enter(AudioPcmRenderGeneration::new(1)),
                },
                &ExecutionCancellationToken::new(),
            )
            .expect("delivery block");
        let AudioPcmRenderOutcome::Rendered(output) = output else {
            panic!("expected complete PCM");
        };

        assert_eq!(output.channel_layout, AudioChannelLayout::Stereo);
        assert_eq!(output.samples, vec![0.0; 8]);
        drop(renderer);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn public_program_adapter_preserves_sequence_layout_without_audition_mapping() {
        let root = std::env::temp_dir().join(format!(
            "mondrian-public-program-audio-{}",
            mondrian_core::ProjectId::new()
        ));
        let mut sequence = Sequence::new("public Program");
        sequence.settings.audio_sample_rate = 48_000;
        sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
        let renderer = TimelineAudioPcmRenderer::new_public_program(
            sequence,
            Vec::new(),
            AssetLibrary::open(root.clone()).expect("asset library"),
            Arc::new(AudioSourceCache::new(48_000)),
            test_runtime_grant(),
        )
        .expect("public Program renderer");

        let evidence = renderer.delivery_evidence();
        assert_eq!(evidence.program_layout, AudioChannelLayout::Stereo);
        assert_eq!(evidence.target_layout, AudioChannelLayout::Stereo);
        assert!(matches!(
            evidence.mapping_kind,
            mondrian_audio::AudioDeliveryMappingKind::Identity
                | mondrian_audio::AudioDeliveryMappingKind::ProvenSilence
        ));
        drop(renderer);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "requires a built Mondrian executable and the Clack gain reference DLL"]
    fn preview_runtime_executes_installed_clap_through_selected_resolver() {
        use mondrian_audio::{
            DiscoveredClapAudioProcessorSpecResolver, IsolatedAudioProcessorResolver,
        };

        let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
            .map(std::path::PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_HELPER to the app executable");
        let plugin = std::env::var_os("MONDRIAN_CLAP_TEST_PLUGIN")
            .map(std::path::PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_PLUGIN to the Clack gain DLL");
        let installed = DiscoveredClapAudioProcessorSpecResolver::discover(helper, [plugin])
            .expect("discover reference plugin");
        let descriptor = installed.descriptors()[0].clone();
        let instance = installed
            .create_instance(
                &descriptor.plugin_id,
                reference_render_contract(),
                Some(0.5_f32.to_le_bytes().to_vec()),
            )
            .expect("capture pinned CLAP instance");
        let resolver = IsolatedAudioProcessorResolver::new(Arc::new(installed));
        assert_reference_preview_gain(instance, &resolver);
    }

    #[test]
    #[ignore = "requires MONDRIAN_VST3_TEST_HELPER and MONDRIAN_VST3_TEST_PLUGIN"]
    fn preview_runtime_executes_installed_vst3_through_selected_resolver() {
        use mondrian_audio::{
            DiscoveredVst3AudioProcessorSpecResolver, IsolatedAudioProcessorResolver,
        };

        let helper = std::env::var_os("MONDRIAN_VST3_TEST_HELPER")
            .map(std::path::PathBuf::from)
            .expect("built Mondrian helper executable");
        let plugin = std::env::var_os("MONDRIAN_VST3_TEST_PLUGIN")
            .map(std::path::PathBuf::from)
            .expect("VST3 Gain reference file or bundle");
        let installed = DiscoveredVst3AudioProcessorSpecResolver::discover(helper, [plugin])
            .expect("discover VST3 Gain");
        let class_id = installed.descriptors()[0].class_id.clone();
        let mut instance = installed
            .create_instance(&class_id, reference_render_contract(), None)
            .expect("capture pinned VST3 instance");
        let id = instance.parameters.keys().next().cloned().expect("Gain parameter ID");
        instance
            .parameters
            .get_mut(&id)
            .expect("Gain parameter")
            .set_automation(mondrian_core::ExactAutomationCurve::new(id, 0.5).expect("Gain curve"))
            .expect("set Gain curve");
        let resolver = IsolatedAudioProcessorResolver::new(Arc::new(installed));
        assert_reference_preview_gain(instance, &resolver);
    }

    fn reference_render_contract() -> AudioRenderContract {
        AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: MAX_AUDIO_RENDER_BLOCK_FRAMES,
            processing_mode: AudioProcessingMode::Realtime,
            processor_session_scratch_budget_bytes:
                AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
            public_output_lookahead_budget_frames:
                AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
            compensation_delay_scratch_budget_bytes:
                AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
        }
    }

    fn assert_reference_preview_gain(
        instance: mondrian_timeline::audio::AudioProcessorInstance,
        resolver: &dyn AudioProcessorResolver,
    ) {
        let (root, library, mut sequence) = native_gain_media_fixture();
        sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
        let renderer = TimelineAudioPcmRenderer::new_with_processor_resolver(
            sequence,
            Vec::new(),
            library,
            Arc::new(AudioSourceCache::new(48_000)),
            test_runtime_grant(),
            AudioAuditionOverlay::default(),
            48_000,
            AudioChannelLayout::Stereo,
            MAX_AUDIO_RENDER_BLOCK_FRAMES,
            resolver,
            None,
        )
        .expect("prepare native plugin preview");
        assert!(renderer.execution_demand().requires_execution());
        let output = renderer
            .render(
                AudioPcmRenderRequest {
                    start_sample: 0,
                    frame_count: 64,
                    sample_rate: 48_000,
                    channel_layout: AudioChannelLayout::Stereo,
                    continuity: AudioPcmContinuity::Enter(AudioPcmRenderGeneration::new(1)),
                },
                &ExecutionCancellationToken::new(),
            )
            .expect("render through native plugin worker");
        let AudioPcmRenderOutcome::Rendered(output) = output else {
            panic!("expected complete PCM");
        };
        assert_eq!(output.samples.len(), 128);
        for (frame, pair) in output.samples.chunks_exact(2).enumerate() {
            assert!(
                (pair[0] - 0.125).abs() <= 1e-6,
                "left frame {frame}: {}",
                pair[0]
            );
            assert!(
                (pair[1] + 0.125).abs() <= 1e-6,
                "right frame {frame}: {}",
                pair[1]
            );
        }
        drop(renderer);
        drop(root);
    }
    fn native_gain_media_fixture() -> (tempfile::TempDir, Arc<AssetLibrary>, Sequence) {
        native_gain_media_fixture_with_samples(48_000, |_| [0.25, -0.25])
    }

    fn native_gain_media_fixture_with_samples(
        frames: usize,
        sample: impl Fn(usize) -> [f32; 2],
    ) -> (tempfile::TempDir, Arc<AssetLibrary>, Sequence) {
        use std::io::Write;

        let root = tempfile::Builder::new()
            .prefix("mondrian-native-audio-音频🙂-")
            .tempdir()
            .expect("native audio fixture directory");
        let source_path = root.path().join("constant-stereo.wav");
        let sample_rate = 48_000_u32;
        let data_bytes = u32::try_from(frames * 2 * 4).expect("short WAV payload");
        let mut wave = std::io::BufWriter::new(
            std::fs::File::create(&source_path).expect("create nonzero WAV source"),
        );
        wave.write_all(b"RIFF").expect("RIFF");
        wave.write_all(&(36_u32 + data_bytes).to_le_bytes()).expect("RIFF extent");
        wave.write_all(b"WAVEfmt ").expect("format");
        wave.write_all(&16_u32.to_le_bytes()).expect("format extent");
        wave.write_all(&3_u16.to_le_bytes()).expect("IEEE Float");
        wave.write_all(&2_u16.to_le_bytes()).expect("stereo");
        wave.write_all(&sample_rate.to_le_bytes()).expect("sample rate");
        wave.write_all(&(sample_rate * 8).to_le_bytes()).expect("byte rate");
        wave.write_all(&8_u16.to_le_bytes()).expect("block alignment");
        wave.write_all(&32_u16.to_le_bytes()).expect("sample bits");
        wave.write_all(b"data").expect("data");
        wave.write_all(&data_bytes.to_le_bytes()).expect("data extent");
        for frame in 0..frames {
            let [left, right] = sample(frame);
            wave.write_all(&left.to_le_bytes()).expect("left sample");
            wave.write_all(&right.to_le_bytes()).expect("right sample");
        }
        wave.flush().expect("complete WAV source");
        drop(wave);
        let source_path = std::fs::canonicalize(source_path).expect("canonical WAV path");
        let library = AssetLibrary::open(root.path().join("library")).expect("asset library");
        let asset_id = library
            .commit_media_probe(
                mondrian_assets::AssetMediaProbeCandidate::new(
                    source_path.clone(),
                    mondrian_core::MediaFileFingerprint::capture(&source_path),
                    mondrian_media::probe_media_info(&source_path).expect("probe WAV"),
                )
                .expect("audio source candidate"),
                None,
            )
            .expect("register WAV source");
        let mut sequence = Sequence::new("native plugin preview");
        let track_id = sequence.audio_tracks[0].id;
        sequence
            .add_media_audio_clip(
                track_id,
                mondrian_timeline::Clip::new(
                    asset_id,
                    mondrian_core::TimelineTime::ZERO,
                    mondrian_core::TimelineTime::new(
                        i64::try_from(frames).expect("finite WAV frames"),
                        i64::from(sample_rate),
                    )
                    .expect("exact WAV Clip duration"),
                )
                .expect("audio Clip"),
                mondrian_core::AudioSourceComponentId::primary(),
            )
            .expect("add WAV Clip");
        (root, library, sequence)
    }

    #[test]
    #[ignore = "requires current product and owned sample-accurate CLAP gain binary"]
    fn native_clap_group_interpolation_undo_save_reopen_preserves_output() {
        use mondrian_audio::{
            DiscoveredClapAudioProcessorSpecResolver, IsolatedAudioProcessorResolver,
        };
        let helper = std::path::PathBuf::from(
            std::env::var_os("MONDRIAN_CLAP_TEST_HELPER").expect("built product"),
        );
        let plugin = std::path::PathBuf::from(
            std::env::var_os("MONDRIAN_CLAP_SAMPLE_TEST_PLUGIN").expect("sample-accurate fixture"),
        );
        let installed = DiscoveredClapAudioProcessorSpecResolver::discover(helper, [plugin])
            .expect("discover native CLAP");
        let instance = installed
            .create_instance(
                "org.mondrian.fixture.sample-gain",
                reference_render_contract(),
                None,
            )
            .expect("capture pinned native instance");
        let resolver = IsolatedAudioProcessorResolver::new(Arc::new(installed));
        qualify_native_audio_group_history(instance, &resolver);
    }

    #[test]
    #[ignore = "requires current product and owned sample-accurate stateful VST3 gain binary"]
    fn native_vst3_group_interpolation_undo_save_reopen_preserves_output() {
        use mondrian_audio::{
            DiscoveredVst3AudioProcessorSpecResolver, IsolatedAudioProcessorResolver,
        };
        let helper = std::path::PathBuf::from(
            std::env::var_os("MONDRIAN_VST3_TEST_HELPER").expect("built product"),
        );
        let plugin = std::path::PathBuf::from(
            std::env::var_os("MONDRIAN_VST3_STATEFUL_TEST_PLUGIN").expect("stateful fixture"),
        );
        let installed = DiscoveredVst3AudioProcessorSpecResolver::discover(helper, [plugin])
            .expect("discover native VST3");
        let class_id = installed.descriptors()[0].class_id.clone();
        let instance = installed
            .create_instance(&class_id, reference_render_contract(), None)
            .expect("capture pinned native instance");
        let resolver = IsolatedAudioProcessorResolver::new(Arc::new(installed));
        qualify_native_audio_group_history(instance, &resolver);
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum NativeSidechainCase {
        External,
        SameStrip,
        LateExternal,
        ProcessingScope,
        SharedProcessingScope,
    }

    #[test]
    #[ignore = "requires current product and owned CLAP sidechain DLL"]
    fn native_clap_shared_scope_sidechain_undo_save_reopen_preserves_output() {
        qualify_native_clap_sidechain(NativeSidechainCase::SharedProcessingScope);
    }

    #[test]
    #[ignore = "requires current product and owned VST3 sidechain DLL"]
    fn native_vst3_shared_scope_sidechain_undo_save_reopen_preserves_output() {
        qualify_native_vst3_sidechain(NativeSidechainCase::SharedProcessingScope);
    }

    #[test]
    #[ignore = "requires current product and owned CLAP sidechain DLL"]
    fn native_clap_scope_sidechain_undo_save_reopen_preserves_output() {
        qualify_native_clap_sidechain(NativeSidechainCase::ProcessingScope);
    }

    #[test]
    #[ignore = "requires current product and owned VST3 sidechain DLL"]
    fn native_vst3_scope_sidechain_undo_save_reopen_preserves_output() {
        qualify_native_vst3_sidechain(NativeSidechainCase::ProcessingScope);
    }

    #[test]
    #[ignore = "requires current product and owned CLAP sidechain DLL"]
    fn native_clap_late_detector_sidechain_undo_save_reopen_preserves_output() {
        qualify_native_clap_sidechain(NativeSidechainCase::LateExternal);
    }

    #[test]
    #[ignore = "requires current product and owned VST3 sidechain DLL"]
    fn native_vst3_late_detector_sidechain_undo_save_reopen_preserves_output() {
        qualify_native_vst3_sidechain(NativeSidechainCase::LateExternal);
    }

    #[test]
    #[ignore = "requires current product and owned CLAP sidechain DLL"]
    fn native_clap_sidechain_routing_undo_save_reopen_preserves_output() {
        qualify_native_clap_sidechain(NativeSidechainCase::External);
    }

    #[test]
    #[ignore = "requires current product and owned CLAP sidechain DLL"]
    fn native_clap_same_strip_prefader_sidechain_undo_save_reopen_preserves_output() {
        qualify_native_clap_sidechain(NativeSidechainCase::SameStrip);
    }

    fn qualify_native_clap_sidechain(case: NativeSidechainCase) {
        use mondrian_audio::{
            DiscoveredClapAudioProcessorSpecResolver, IsolatedAudioProcessorResolver,
        };
        let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
            .map(std::path::PathBuf::from)
            .expect("current product");
        let plugin = std::env::var_os("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")
            .map(std::path::PathBuf::from)
            .expect("owned sidechain DLL");
        let installed = DiscoveredClapAudioProcessorSpecResolver::discover(helper, [plugin])
            .expect("discover native sidechain");
        let mut opaque = vec![0_u8];
        opaque.extend(0.5_f32.to_le_bytes());
        let instance = installed
            .create_instance(
                "org.mondrian.fixture.sidechain",
                reference_render_contract(),
                Some(opaque),
            )
            .expect("capture stateful native definition");
        let resolver = Arc::new(IsolatedAudioProcessorResolver::new(Arc::new(installed)));
        assert_native_sidechain_history(instance, resolver, "clap.audio-input.11", case);
    }

    #[test]
    #[ignore = "requires current product and owned VST3 sidechain DLL"]
    fn native_vst3_sidechain_routing_undo_save_reopen_preserves_output() {
        qualify_native_vst3_sidechain(NativeSidechainCase::External);
    }

    #[test]
    #[ignore = "requires current product and owned VST3 sidechain DLL"]
    fn native_vst3_same_strip_prefader_sidechain_undo_save_reopen_preserves_output() {
        qualify_native_vst3_sidechain(NativeSidechainCase::SameStrip);
    }

    fn qualify_native_vst3_sidechain(case: NativeSidechainCase) {
        use mondrian_audio::{
            DiscoveredVst3AudioProcessorSpecResolver, IsolatedAudioProcessorResolver,
        };
        let helper = std::env::var_os("MONDRIAN_VST3_TEST_HELPER")
            .map(std::path::PathBuf::from)
            .expect("current product");
        let plugin = std::env::var_os("MONDRIAN_VST3_SIDECHAIN_TEST_PLUGIN")
            .map(std::path::PathBuf::from)
            .expect("owned VST3 sidechain DLL");
        let installed = DiscoveredVst3AudioProcessorSpecResolver::discover(helper, [plugin])
            .expect("discover native sidechain");
        assert_eq!(installed.descriptors().len(), 1);
        let instance = installed
            .create_instance(
                &installed.descriptors()[0].class_id,
                reference_render_contract(),
                None,
            )
            .expect("capture default native state");
        let resolver = Arc::new(IsolatedAudioProcessorResolver::new(Arc::new(installed)));
        assert_native_sidechain_history(instance, resolver, "vst3.audio-input.1", case);
    }

    fn assert_native_sidechain_history(
        mut instance: mondrian_timeline::audio::AudioProcessorInstance,
        resolver: Arc<dyn AudioProcessorResolver>,
        bus_key: &str,
        case: NativeSidechainCase,
    ) {
        use super::super::product_action::{AudioProductAction, ProductAction};
        use mondrian_timeline::audio::{AudioChannelStripOutputPort, AudioRouteSource};
        use mondrian_timeline::{AudioRoutingEdit, AudioRoutingEditRequest};
        let same_strip = case == NativeSidechainCase::SameStrip;
        let is_scope = matches!(
            case,
            NativeSidechainCase::ProcessingScope | NativeSidechainCase::SharedProcessingScope
        );
        // Shared Scope occurrences at times 5 and 7 have effective gains .5
        // and .25. VST3 uses Scope parameter automation; the state-only CLAP
        // fixture uses .5 native gain and .5 second-occurrence placement gain.
        let signal_scale = if case == NativeSidechainCase::SharedProcessingScope {
            1.5
        } else {
            1.0
        };
        let routed_lookahead = if case == NativeSidechainCase::LateExternal {
            6
        } else {
            3
        };
        let processor_id = instance.id;
        let (root, library, mut sequence) = native_gain_media_fixture();
        let asset_id = sequence.audio_tracks[0].clips[0].media_asset_id().expect("media asset");
        let detector = if same_strip {
            sequence.audio_tracks[0].id
        } else {
            let detector = sequence.add_audio_track();
            sequence
                .add_media_audio_clip(
                    detector,
                    mondrian_timeline::Clip::new(
                        asset_id,
                        mondrian_core::TimelineTime::ZERO,
                        mondrian_core::TimelineTime::ONE,
                    )
                    .expect("detector clip"),
                    AudioSourceComponentId::primary(),
                )
                .expect("authored detector source");
            sequence
                .audio_program
                .track_channels
                .get_mut(&detector)
                .expect("detector strip")
                .strip
                .fader_db = -6.020_599_913_279_624;
            sequence.audio_program.routes.retain(|route| {
                !matches!(route.source,
                AudioRouteSource::Track { track_id, .. } if track_id == detector)
            });
            detector
        };
        if case == NativeSidechainCase::LateExternal {
            // The owned native fixture has three frames of declared latency.
            // Its gain is 0.5 and disconnected auxiliary buses are silent, so
            // gain * (2 main) preserves the detector's independent amplitude.
            let mut detector_processor = instance.clone();
            detector_processor.id = mondrian_core::AudioProcessorInstanceId::new();
            sequence
                .audio_program
                .track_channels
                .get_mut(&detector)
                .expect("detector strip")
                .strip
                .pre_fader
                .processors
                .push(detector_processor);
        }
        if is_scope {
            let component = &mut sequence.audio_tracks[0].clips[0].audio_components[0];
            component.processing.scope_in =
                mondrian_core::TimelineTime::new(5, 1).expect("Scope binding trim");
            let scope_id = component.processing.scope_id;
            if case == NativeSidechainCase::SharedProcessingScope {
                let mut other = sequence.audio_tracks[0].clips[0].clone();
                other.id = mondrian_core::ClipId::new();
                for edit in &mut other.audio_components {
                    edit.id = mondrian_core::AudioComponentEditId::new();
                    edit.processing.scope_in =
                        mondrian_core::TimelineTime::new(7, 1).expect("second Scope binding");
                }
                match &instance.definition {
                    mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap { .. } => {
                        assert!(instance.parameters.is_empty(), "state-only CLAP contract");
                        other.audio_components[0].volume_db = -6.020_599_913_279_624;
                    }
                    mondrian_timeline::audio::AudioProcessorDefinitionRef::Vst3 { .. } => {
                        let parameter_id =
                            instance.parameters.keys().next().cloned().expect("native VST3 gain");
                        let mut curve =
                            mondrian_core::ExactAutomationCurve::new(parameter_id.clone(), 0.5)
                                .expect("Scope gain curve");
                        for (second, value) in [(5, 0.5), (7, 0.25)] {
                            let mut key = mondrian_core::ExactAutomationKeyframe::linear(
                                mondrian_core::TimelineTime::new(second, 1)
                                    .expect("Scope key time"),
                                value,
                            );
                            key.interpolation_to_next =
                                mondrian_core::AutomationSegmentInterpolation::Hold;
                            curve.set_keyframe(key).expect("Scope gain key");
                        }
                        instance
                            .parameters
                            .get_mut(&parameter_id)
                            .expect("native gain")
                            .set_automation(curve)
                            .expect("native Scope curve admitted");
                    }
                    mondrian_timeline::audio::AudioProcessorDefinitionRef::BuiltIn { .. } => {
                        panic!("native fixture required")
                    }
                }
                sequence.audio_tracks[0].clips.push(other);
            }
            sequence
                .audio_program
                .processing_scopes
                .iter_mut()
                .find(|scope| scope.id == scope_id)
                .expect("main Scope")
                .processors
                .processors
                .push(instance);
        } else if same_strip {
            sequence
                .audio_program
                .track_channels
                .get_mut(&detector)
                .expect("main strip")
                .strip
                .post_fader
                .processors
                .push(instance);
        } else {
            sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
        }
        let replacement_detector = sequence.add_audio_track();
        sequence
            .add_media_audio_clip(
                replacement_detector,
                mondrian_timeline::Clip::new(
                    asset_id,
                    mondrian_core::TimelineTime::ZERO,
                    mondrian_core::TimelineTime::ONE,
                )
                .expect("replacement clip"),
                AudioSourceComponentId::primary(),
            )
            .expect("replacement source");
        sequence
            .audio_program
            .track_channels
            .get_mut(&replacement_detector)
            .expect("replacement strip")
            .strip
            .fader_db = -18.061_799_739_838_87;
        sequence.audio_program.routes.retain(|route| {
            !matches!(route.source, AudioRouteSource::Track { track_id, .. } if track_id == replacement_detector)
        });
        let replacement_source = AudioRouteSource::Track {
            track_id: replacement_detector,
            port: AudioChannelStripOutputPort::PostMute,
        };
        let source = AudioRouteSource::Track {
            track_id: detector,
            port: if same_strip {
                AudioChannelStripOutputPort::PreFader
            } else {
                AudioChannelStripOutputPort::PostMute
            },
        };
        // Independent owned ABI fixture: gain * (2 main + 3 first auxiliary).
        let routed_expected = if same_strip { 0.625 } else { 0.4375 } * signal_scale;
        let action = |processor_id| {
            ProductAction::Audio(AudioProductAction::EditRouting(AudioRoutingEditRequest {
                edit: AudioRoutingEdit::CreateProcessorSidechain {
                    source: source.clone(),
                    processor_id,
                    bus_key: bus_key.to_owned(),
                },
            }))
            .into_external_action()
        };
        let mut state = AppState::with_audio_processor_resolver(Arc::clone(&resolver));
        state.test_set_sequence(Some(sequence.clone()));
        state.test_set_asset_library(Some(library));
        let before =
            assert_native_sidechain_delivery(&state, resolver.as_ref(), 0.25 * signal_scale, 3);
        let generation = state.project_author_generation();
        let history = state.authoring_history().expect("history").diagnostics();
        state
            .dispatch_action(action(mondrian_core::AudioProcessorInstanceId::new()))
            .expect_err("missing processor refuses route transaction");
        assert_eq!(state.project_author_generation(), generation);
        assert_eq!(
            state.authoring_history().expect("history").diagnostics(),
            history
        );
        assert!(state
            .active_sequence()
            .expect("unchanged")
            .author_state_eq_ignoring_revision(&sequence));
        let mut locked = sequence.clone();
        locked
            .audio_tracks
            .iter_mut()
            .find(|track| track.id == detector)
            .expect("detector")
            .is_locked = true;
        let mut refused = AppState::new();
        refused.test_set_sequence(Some(locked.clone()));
        let locked_generation = refused.project_author_generation();
        refused
            .dispatch_action(action(processor_id))
            .expect_err("locked detector refuses route");
        assert!(refused
            .active_sequence()
            .expect("locked")
            .author_state_eq_ignoring_revision(&locked));
        assert_eq!(refused.project_author_generation(), locked_generation);
        assert!(!refused.can_undo_action() && !refused.can_redo_action());
        drop(refused);
        use super::super::audio_input_observation::AudioInputObservation;
        use super::super::product_action::{
            AudioProcessorInputQueryOperation, AudioProcessorInputQueryPayload,
        };
        let (owner, rack) = if same_strip {
            (
                mondrian_timeline::AudioChannelStripOwner::Track { track_id: detector },
                mondrian_timeline::AudioChannelStripRack::PostFader,
            )
        } else {
            (
                mondrian_timeline::AudioChannelStripOwner::ProgramOutput {
                    output_id: sequence.audio_program.outputs[0].id,
                },
                mondrian_timeline::AudioChannelStripRack::PreFader,
            )
        };
        let query = AudioProcessorInputQueryPayload {
            session_id: state.authoring_session_id().expect("native editing session"),
            sequence_id: sequence.id,
            target: if is_scope {
                let clip = &sequence.audio_tracks[0].clips[0];
                let component = &clip.audio_components[0];
                mondrian_audio::AudioProcessorInputInspectionTarget::ProcessingScope {
                    scope_id: component.processing.scope_id,
                    occurrence: mondrian_timeline::AudioComponentAddress {
                        track_id: sequence.audio_tracks[0].id,
                        clip_id: clip.id,
                        edit_id: component.id,
                    },
                }
            } else {
                mondrian_audio::AudioProcessorInputInspectionTarget::ChannelStrip { owner, rack }
            },
            processor_id,
            operation: AudioProcessorInputQueryOperation::Query,
        };
        state
            .dispatch_action(
                ProductAction::Audio(AudioProductAction::QueryProcessorInputs(query))
                    .into_external_action(),
            )
            .expect("real native control inspection");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while state.has_native_audio_control_work() {
            state.poll_audio_processor_input_observations();
            assert!(
                std::time::Instant::now() < deadline,
                "native input observation deadline"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let AudioInputObservation::Ready(observed) = state.audio_processor_input_observation(query)
        else {
            panic!("Real native auxiliary observation must succeed");
        };
        assert!(observed
            .inputs
            .input_names
            .iter()
            .any(|name| name.bus_key == bus_key && name.name == "压缩探测 🙂"));
        let connection = crate::app_ui::test_utils::native_sidechain_source_action(
            &state, query, source, bus_key, None,
        );
        assert_eq!(
            state.project_author_generation(),
            generation,
            "Read-only SDK labels never edit author state"
        );
        assert_eq!(
            state.authoring_history().expect("history").diagnostics(),
            history
        );
        state
            .dispatch_action(connection)
            .expect("atomic observed native sidechain routing");
        assert_eq!(state.project_author_generation(), generation + 1);
        assert_eq!(
            state.authoring_history().expect("history").diagnostics().undo_entries,
            1
        );
        let edited = state.active_sequence().expect("edited").clone();
        assert_eq!(edited.audio_program.sidechain_routes.len(), 1);
        let after = assert_native_sidechain_delivery(
            &state,
            resolver.as_ref(),
            routed_expected,
            routed_lookahead,
        );
        assert_ne!(before, after);
        assert!(state.undo_timeline().expect("one route Undo"));
        assert!(state
            .active_sequence()
            .expect("Undo")
            .author_state_eq_ignoring_revision(&sequence));
        assert_eq!(
            assert_native_sidechain_delivery(&state, resolver.as_ref(), 0.25 * signal_scale, 3),
            before
        );
        assert!(!state.can_undo_action() && state.can_redo_action());
        let undone_path = root.path().join("原生侧链撤销🙂.mdp");
        state.save_project_file_as(undone_path.clone()).expect("save Undo archive");
        assert!(state.can_redo_action(), "saving retains live Redo");
        assert!(state.redo_timeline().expect("route Redo"));
        assert!(state
            .active_sequence()
            .expect("Redo")
            .author_state_eq_ignoring_revision(&edited));
        assert_eq!(
            assert_native_sidechain_delivery(
                &state,
                resolver.as_ref(),
                routed_expected,
                routed_lookahead
            ),
            after
        );
        let edited_path = root.path().join("原生侧链重做🙂.mdp");
        state.save_project_file_as(edited_path.clone()).expect("save routed archive");
        let route_id = edited.audio_program.sidechain_routes[0].id;
        let rewire = crate::app_ui::test_utils::native_sidechain_source_action(
            &state,
            query,
            replacement_source.clone(),
            bus_key,
            Some(route_id),
        );
        let generation = state.project_author_generation();
        let history = state.authoring_history().expect("history").diagnostics();
        state.dispatch_action(rewire).expect("shared Rack native source rewire");
        assert_eq!(state.project_author_generation(), generation + 1);
        assert_eq!(
            state.authoring_history().expect("history").diagnostics().undo_entries,
            history.undo_entries + 1
        );
        let rewired = state.active_sequence().expect("rewired").clone();
        let mut expected_rewire = edited.clone();
        expected_rewire.audio_program.sidechain_routes[0].source = replacement_source;
        assert!(rewired.author_state_eq_ignoring_revision(&expected_rewire));
        // Independent ABI oracle: 0.5 * (2 * 0.25 + 3 * (0.25 * 0.125)).
        let rewired_expected = 0.296875 * signal_scale;
        let rewired_pcm =
            assert_native_sidechain_delivery(&state, resolver.as_ref(), rewired_expected, 3);
        assert_ne!(rewired_pcm, after);
        assert!(state.undo_timeline().expect("one source rewire Undo"));
        assert!(state
            .active_sequence()
            .expect("rewire Undo")
            .author_state_eq_ignoring_revision(&edited));
        assert_eq!(
            assert_native_sidechain_delivery(
                &state,
                resolver.as_ref(),
                routed_expected,
                routed_lookahead
            ),
            after
        );
        let rewire_undo_path = root.path().join("原生侧链改线撤销🙂.mdp");
        state.save_project_file_as(rewire_undo_path.clone()).expect("save rewire Undo");
        assert!(
            state.can_redo_action(),
            "saving source Undo retains live Redo"
        );
        assert!(state.redo_timeline().expect("source rewire Redo"));
        assert_eq!(
            assert_native_sidechain_delivery(&state, resolver.as_ref(), rewired_expected, 3),
            rewired_pcm
        );
        let rewired_path = root.path().join("原生侧链改线🙂.mdp");
        state.save_project_file_as(rewired_path.clone()).expect("save rewritten route");
        assert_native_sidechain_gain_curve_history(
            &mut state,
            resolver.as_ref(),
            processor_id,
            route_id,
            root.path(),
            signal_scale,
        );
        drop(state);
        for (path, authored, expected, lookahead, output) in [
            (undone_path, &sequence, 0.25 * signal_scale, 3, before),
            (
                edited_path,
                &edited,
                routed_expected,
                routed_lookahead,
                after.clone(),
            ),
            (
                rewire_undo_path,
                &edited,
                routed_expected,
                routed_lookahead,
                after,
            ),
            (rewired_path, &rewired, rewired_expected, 3, rewired_pcm),
        ] {
            let mut reopened = AppState::new();
            reopened.open_project_file(path).expect("fresh Unicode sidechain reopen");
            assert!(reopened
                .active_sequence()
                .expect("reopened")
                .author_state_eq_ignoring_revision(authored));
            assert!(!reopened.can_undo_action() && !reopened.can_redo_action());
            assert!(reopened.active_clipboard_kind().is_none());
            assert_eq!(
                reopened.asset_library().expect("SQLite").list_assets().expect("assets").len(),
                1
            );
            assert_eq!(
                assert_native_sidechain_delivery(&reopened, resolver.as_ref(), expected, lookahead),
                output
            );
        }
    }

    fn assert_native_sidechain_delivery(
        state: &AppState,
        processor_resolver: &dyn AudioProcessorResolver,
        expected: f32,
        expected_lookahead: usize,
    ) -> Vec<f32> {
        assert_native_sidechain_delivery_samples(
            state,
            processor_resolver,
            &[expected; 257],
            expected_lookahead,
        )
    }

    fn assert_native_sidechain_gain_curve_history(
        state: &mut AppState,
        resolver: &dyn AudioProcessorResolver,
        processor_id: mondrian_core::AudioProcessorInstanceId,
        route_id: mondrian_core::AudioRouteId,
        root: &std::path::Path,
        signal_scale: f32,
    ) {
        let before = state.active_sequence().expect("before gain curve").clone();
        // Independent oracle: main .25, detector .03125; native weights 2 and 3,
        // plugin gain .5. Route dB rises linearly from START_DB to 0 over one
        // exact second. Evaluate delivered signal time, not physical PDC time.
        const START_DB: f64 = -6.020_599_913_279_624;
        for (x, value) in [(0.0, START_DB), (1.0, 0.0)] {
            let action = crate::app_ui::test_utils::native_sidechain_gain_key_action(
                state,
                processor_id,
                route_id,
                x,
                value,
            );
            state.dispatch_action(action).expect("shared native gain key");
        }
        state
            .dispatch_action(
                crate::app_ui::test_utils::native_sidechain_gain_linear_action(
                    state,
                    processor_id,
                    route_id,
                ),
            )
            .expect("shared native Linear menu");
        let authored = state.active_sequence().expect("gain authored").clone();
        let curve = authored.audio_program.sidechain_routes[0]
            .gain_automation
            .as_ref()
            .expect("exact route curve");
        assert_eq!(curve.keyframes.len(), 2);
        assert_eq!(curve.keyframes[0].time, mondrian_core::TimelineTime::ZERO);
        assert_eq!(curve.keyframes[1].time, mondrian_core::TimelineTime::ONE);
        assert_eq!(curve.keyframes[0].value, START_DB);
        assert_eq!(curve.keyframes[1].value, 0.0);
        let expected: Vec<f32> = (0..257)
            .map(|sample| {
                let db = START_DB * (1.0 - f64::from(sample) / 48_000.0);
                ((0.25 + 0.046875 * 10.0_f64.powf(db / 20.0)) * f64::from(signal_scale)) as f32
            })
            .collect();
        let output = assert_native_sidechain_delivery_samples(state, resolver, &expected, 3);
        assert!(state.undo_timeline().expect("Linear menu Undo"));
        assert!(state.undo_timeline().expect("end key Undo"));
        assert!(state.undo_timeline().expect("start key Undo"));
        assert!(state
            .active_sequence()
            .expect("gain Undo")
            .author_state_eq_ignoring_revision(&before));
        let undone_path = root.join("侧链曲线撤销保存🙂.mdp");
        state.save_project_file_as(undone_path.clone()).expect("save gain Undo");
        assert!(state.can_redo_action(), "save preserves live gain Redo");
        assert_native_sidechain_delivery(state, resolver, 0.296875 * signal_scale, 3);
        assert!(state.redo_timeline().expect("start key Redo"));
        assert!(state.redo_timeline().expect("end key Redo"));
        assert!(state.redo_timeline().expect("Linear menu Redo"));
        assert!(state
            .active_sequence()
            .expect("gain Redo")
            .author_state_eq_ignoring_revision(&authored));
        assert_eq!(
            assert_native_sidechain_delivery_samples(state, resolver, &expected, 3),
            output
        );
        let edited_path = root.join("侧链曲线输出🙂.mdp");
        state.save_project_file_as(edited_path.clone()).expect("save gain edited");
        for (path, author, samples) in [
            (undone_path, &before, vec![0.296875 * signal_scale; 257]),
            (edited_path, &authored, expected),
        ] {
            let mut reopened = AppState::new();
            reopened.open_project_file(path).expect("fresh Unicode gain reopen");
            assert!(reopened
                .active_sequence()
                .expect("reopened")
                .author_state_eq_ignoring_revision(author));
            assert!(!reopened.can_undo_action());
            assert!(!reopened.can_redo_action());
            assert_native_sidechain_delivery_samples(&reopened, resolver, &samples, 3);
        }
    }

    fn assert_native_sidechain_delivery_samples(
        state: &AppState,
        processor_resolver: &dyn AudioProcessorResolver,
        expected_samples: &[f32],
        expected_lookahead: usize,
    ) -> Vec<f32> {
        let sequence = state.active_sequence().expect("author sequence");
        let library = state.asset_library_handle().expect("source library");
        let mut reference = None;
        for (mode, partitions) in [
            (AudioProcessingMode::Realtime, [64, 64, 129]),
            (AudioProcessingMode::Offline, [37, 59, 161]),
        ] {
            let media = PlaybackMediaResolver {
                library: library.clone(),
                source_cache: Arc::new(AudioSourceCache::new(48_000)),
            };
            let runtime = AudioProgramRuntime::build_with_compile_request_processor_resolver_and_resource_grant(
                sequence, &[], &media, processor_resolver,
                AudioRenderContract { processing_mode: mode, ..reference_render_contract() },
                AudioCompileRequest { output_id: sequence.audio_program.outputs[0].id, audition: AudioAuditionOverlay::default() },
                test_runtime_grant(),
            ).expect("prepare shared native routing and PDC");
            assert_eq!(
                runtime.public_output_lookahead_frames(),
                expected_lookahead,
                "native latency remains in shared public output compensation"
            );
            let mut delivery =
                AudioProgramDeliveryRuntime::prepare_standard(runtime, AudioChannelLayout::Stereo)
                    .expect("public stereo delivery");
            delivery
                .enter_state(AudioContinuityEpoch::new(1), 0)
                .expect("native delivery continuity");
            let cancellation = ExecutionCancellationToken::new();
            let mut output = Vec::new();
            let mut start_sample = 0;
            for frames in partitions {
                let mut samples = vec![0.0; frames * 2];
                delivery
                    .render_into_cancellable(
                        AudioRenderRequest { start_sample, frames },
                        &mut samples,
                        &cancellation,
                    )
                    .expect("native public PCM");
                output.extend(samples);
                start_sample += frames as i64;
            }
            assert_eq!(output.len(), 514);
            for (frame, pair) in output.chunks_exact(2).enumerate() {
                let expected = expected_samples[frame];
                assert!(
                    (pair[0] - expected).abs() <= 1e-6,
                    "{mode:?} frame{frame} left{} != {expected}",
                    pair[0]
                );
                assert!(
                    (pair[1] + expected).abs() <= 1e-6,
                    "{mode:?} frame{frame} right{} != {}",
                    pair[1],
                    -expected
                );
            }
            delivery
                .enter_state(AudioContinuityEpoch::new(2), 93)
                .expect("native seek reset");
            let mut seek_samples = [0.0; 62];
            delivery
                .render_into_cancellable(
                    AudioRenderRequest { start_sample: 93, frames: 31 },
                    &mut seek_samples,
                    &cancellation,
                )
                .expect("native gain after seek");
            for (offset, pair) in seek_samples.chunks_exact(2).enumerate() {
                let expected = expected_samples[93 + offset];
                assert!(
                    (pair[0] - expected).abs() <= 1e-6,
                    "{mode:?} seek signal sample {}",
                    93 + offset
                );
                assert!(
                    (pair[1] + expected).abs() <= 1e-6,
                    "{mode:?} seek right signal sample {}",
                    93 + offset
                );
            }
            if let Some(reference) = &reference {
                assert_eq!(
                    &output, reference,
                    "public realtime/offline partition invariance"
                );
            } else {
                reference = Some(output);
            }
        }
        reference.expect("native output")
    }

    fn qualify_native_audio_group_history(
        instance: mondrian_timeline::audio::AudioProcessorInstance,
        resolver: &dyn AudioProcessorResolver,
    ) {
        use super::super::product_action::{AudioProductAction, ProductAction};
        use mondrian_core::{
            ExactAutomationCurve, ExactAutomationKeyframe, InterpolationType, KeyframeId,
            TimelineTime,
        };
        use mondrian_timeline::audio::AudioChannelStripOwner;
        use mondrian_timeline::{
            inspect_audio_automation, AudioAutomationEdit, AudioAutomationEditRequest,
            AudioAutomationTarget, AudioChannelStripRack, AudioProcessorRackAddress,
        };
        for mode in [
            InterpolationType::AutoBezier,
            InterpolationType::ContinuousBezier,
        ] {
            let (root, library, mut sequence) = native_gain_media_fixture();
            let mut instance = instance.clone();
            let parameter_id = instance.parameters.keys().next().cloned().expect("gain parameter");
            let mut source = ExactAutomationCurve::new(parameter_id.clone(), 0.2).expect("curve");
            for (sample, value) in [(0, 0.2), (128, 0.7), (256, 0.3)] {
                source
                    .set_keyframe(ExactAutomationKeyframe::linear(
                        TimelineTime::new(sample, 48_000).expect("exact sample time"),
                        value,
                    ))
                    .expect("seed curve");
            }
            let ids: Vec<_> = source.keyframes.iter().map(|key| key.id).collect();
            instance
                .parameters
                .get_mut(&parameter_id)
                .expect("gain")
                .set_automation(source)
                .expect("admitted source curve");
            let track_id = sequence.audio_tracks[0].id;
            let target = AudioAutomationTarget::ProcessorParameter {
                rack: AudioProcessorRackAddress::ChannelStrip {
                    owner: AudioChannelStripOwner::Track { track_id },
                    rack: AudioChannelStripRack::PreFader,
                },
                processor_id: instance.id,
                parameter_id,
            };
            sequence
                .audio_program
                .track_channels
                .get_mut(&track_id)
                .expect("track strip")
                .strip
                .pre_fader
                .processors
                .push(instance);
            let mut state = AppState::new();
            state.test_set_sequence(Some(sequence));
            state.test_set_asset_library(Some(library.clone()));
            let snapshot = |state: &AppState| {
                inspect_audio_automation(state.active_sequence().expect("sequence"), &target)
                    .expect("native parameter address")
                    .curve()
                    .expect("curve")
                    .clone()
            };
            let execute =
                |state: &AppState| assert_native_audio_group_signal(state, &target, resolver);
            let action = |keyframe_ids| {
                ProductAction::Audio(AudioProductAction::EditAutomation(
                    AudioAutomationEditRequest {
                        target: target.clone(),
                        edit: AudioAutomationEdit::SetInterpolations {
                            keyframe_ids,
                            interpolation: mode,
                        },
                    },
                ))
                .into_external_action()
            };
            let original_sequence = state.active_sequence().expect("original").clone();
            let original = snapshot(&state);
            let before = execute(&state);
            let generation = state.project_author_generation();
            state
                .dispatch_action(action(vec![ids[0], KeyframeId::new()]))
                .expect_err("missing key refuses complete selection");
            assert_eq!(snapshot(&state), original);
            assert_eq!(state.project_author_generation(), generation);
            assert!(!state.can_undo_action() && !state.can_redo_action());
            let mut locked = original_sequence.clone();
            locked.audio_tracks[0].is_locked = true;
            let mut refused = AppState::new();
            refused.test_set_sequence(Some(locked.clone()));
            let refused_generation = refused.project_author_generation();
            refused.dispatch_action(action(ids.clone())).expect_err("locked native owner");
            assert!(refused
                .active_sequence()
                .expect("locked")
                .author_state_eq_ignoring_revision(&locked));
            assert_eq!(refused.project_author_generation(), refused_generation);
            assert!(!refused.can_undo_action() && !refused.can_redo_action());
            drop(refused);
            state
                .dispatch_action(action(ids.clone()))
                .expect("one native group transaction");
            assert_eq!(state.project_author_generation(), generation + 1);
            let edited = snapshot(&state);
            for (old, new) in original.keyframes.iter().zip(edited.keyframes.iter()) {
                assert_eq!((new.id, new.time, new.value), (old.id, old.time, old.value));
                assert_eq!(
                    new.tangent_mode,
                    if mode == InterpolationType::AutoBezier {
                        mondrian_core::ExactAutomationTangentMode::Auto
                    } else {
                        mondrian_core::ExactAutomationTangentMode::Continuous
                    }
                );
            }
            let after = execute(&state);
            if mode == InterpolationType::AutoBezier {
                assert_ne!(after, before);
            }
            let history = state.authoring_history().expect("history").diagnostics();
            state.dispatch_action(action(ids.clone())).expect("same mode is a no-op");
            assert_eq!(state.project_author_generation(), generation + 1);
            assert_eq!(
                state.authoring_history().expect("history").diagnostics(),
                history
            );
            assert!(state.undo_timeline().expect("one Undo"));
            assert!(state
                .active_sequence()
                .expect("undone")
                .author_state_eq_ignoring_revision(&original_sequence));
            assert!(!state.can_undo_action() && state.can_redo_action());
            assert_eq!(execute(&state), before);
            let undone_path = root.path().join("原生音频分组撤销🙂.mdp");
            state.save_project_file_as(undone_path.clone()).expect("save undone");
            assert!(state.can_redo_action(), "saving retains live Redo");
            let mut reopened = AppState::new();
            reopened.open_project_file(undone_path).expect("fresh undone reopen");
            assert!(reopened
                .active_sequence()
                .expect("saved undone")
                .author_state_eq_ignoring_revision(&original_sequence));
            assert!(!reopened.can_undo_action() && !reopened.can_redo_action());
            assert_eq!(execute(&reopened), before);
            drop(reopened);
            assert!(state.redo_timeline().expect("Redo after save"));
            assert_eq!(snapshot(&state), edited);
            assert_eq!(execute(&state), after);
            let edited_sequence = state.active_sequence().expect("edited").clone();
            let edited_path = root.path().join("原生音频分组重做🙂.mdp");
            state.save_project_file_as(edited_path.clone()).expect("save edited");
            drop(state);
            let mut reopened = AppState::new();
            reopened.open_project_file(edited_path).expect("fresh edited reopen");
            assert!(reopened
                .active_sequence()
                .expect("saved edited")
                .author_state_eq_ignoring_revision(&edited_sequence));
            assert!(!reopened.can_undo_action() && !reopened.can_redo_action());
            assert_eq!(execute(&reopened), after);
            assert_eq!(
                reopened.asset_library().expect("SQLite").list_assets().expect("assets").len(),
                1
            );
            qualify_native_audio_clipboard_history(reopened, &target, resolver, root.path());
            drop(library);
            drop(root);
        }
    }

    fn qualify_native_audio_clipboard_history(
        mut state: AppState,
        target: &mondrian_timeline::AudioAutomationTarget,
        resolver: &dyn AudioProcessorResolver,
        directory: &std::path::Path,
    ) {
        use super::super::product_action::{
            AudioCurveClipboardPayload, AudioProductAction, CurveClipboardOperation, ProductAction,
        };
        use mondrian_core::KeyframeId;
        let original = state.active_sequence().expect("original native clipboard state").clone();
        let source = mondrian_timeline::inspect_audio_automation(&original, target)
            .expect("native target")
            .curve()
            .expect("native source")
            .clone();
        let ids: Vec<_> = source.keyframes.iter().map(|key| key.id).collect();
        let action = |operation| {
            ProductAction::Audio(AudioProductAction::CurveClipboard(Box::new(
                AudioCurveClipboardPayload {
                    sequence_id: original.id,
                    target: target.clone(),
                    scope_occurrence: None,
                    operation,
                },
            )))
            .into_external_action()
        };
        let generation = state.project_author_generation();
        let before = assert_native_audio_group_signal(&state, target, resolver);
        assert!(!state.can_undo_action() && !state.can_redo_action());
        state
            .dispatch_action(action(CurveClipboardOperation::Copy {
                keyframe_ids: ids.clone(),
            }))
            .expect("native Copy retains complete constrained group");
        assert_eq!(state.project_author_generation(), generation);
        assert!(!state.can_undo_action() && !state.can_redo_action());
        state
            .dispatch_action(action(CurveClipboardOperation::Cut {
                keyframe_ids: vec![ids[0], KeyframeId::new()],
            }))
            .expect_err("stale native Cut refuses the complete group");
        assert!(state
            .active_sequence()
            .expect("unchanged")
            .author_state_eq_ignoring_revision(&original));
        assert_eq!(state.project_author_generation(), generation);
        assert!(!state.can_undo_action() && !state.can_redo_action());
        state
            .dispatch_action(action(CurveClipboardOperation::Paste {}))
            .expect("native Paste commits one complete group");
        let pasted_sequence = state.active_sequence().expect("pasted").clone();
        let pasted = mondrian_timeline::inspect_audio_automation(&pasted_sequence, target)
            .expect("native target")
            .curve()
            .expect("pasted curve")
            .clone();
        assert_eq!(pasted.keyframes.len(), source.keyframes.len());
        for (old, new) in source.keyframes.iter().zip(&pasted.keyframes) {
            assert!(!ids.contains(&new.id));
            assert_eq!(new.time, old.time);
            assert_eq!(new.value.to_bits(), old.value.to_bits());
            assert_eq!(new.interpolation_to_next, old.interpolation_to_next);
            assert_eq!(new.tangent_mode, old.tangent_mode);
            assert_eq!(new.in_handle, old.in_handle);
            assert_eq!(new.out_handle, old.out_handle);
        }
        assert_eq!(state.project_author_generation(), generation + 1);
        assert_eq!(
            state.authoring_history().expect("history").diagnostics().undo_entries,
            1
        );
        assert_eq!(
            assert_native_audio_group_signal(&state, target, resolver),
            before
        );
        assert!(state.undo_timeline().expect("native Paste Undo"));
        assert!(state
            .active_sequence()
            .expect("undone")
            .author_state_eq_ignoring_revision(&original));
        assert!(!state.can_undo_action() && state.can_redo_action());
        assert_eq!(
            assert_native_audio_group_signal(&state, target, resolver),
            before
        );
        let undone_path = directory.join("原生音频剪贴板撤销🙂.mdp");
        state.save_project_file_as(undone_path.clone()).expect("save undone clipboard");
        assert!(
            state.can_redo_action(),
            "save preserves live native clipboard Redo"
        );
        assert!(state.redo_timeline().expect("native clipboard Redo after save"));
        assert!(state
            .active_sequence()
            .expect("redone")
            .author_state_eq_ignoring_revision(&pasted_sequence));
        assert_eq!(
            assert_native_audio_group_signal(&state, target, resolver),
            before
        );
        let pasted_path = directory.join("原生音频剪贴板重做🙂.mdp");
        state
            .save_project_file_as(pasted_path.clone())
            .expect("save native clipboard Redo");
        drop(state);
        let mut reopened = AppState::new();
        reopened.open_project_file(undone_path).expect("fresh clipboard Undo reopen");
        assert!(reopened
            .active_sequence()
            .expect("undone reopen")
            .author_state_eq_ignoring_revision(&original));
        assert!(!reopened.can_undo_action() && !reopened.can_redo_action());
        assert!(reopened.active_clipboard_kind().is_none());
        assert_eq!(
            assert_native_audio_group_signal(&reopened, target, resolver),
            before
        );
        drop(reopened);
        let mut reopened = AppState::new();
        reopened
            .open_project_file(pasted_path)
            .expect("fresh native clipboard Redo reopen");
        assert!(reopened
            .active_sequence()
            .expect("pasted reopen")
            .author_state_eq_ignoring_revision(&pasted_sequence));
        assert!(!reopened.can_undo_action() && !reopened.can_redo_action());
        assert!(reopened.active_clipboard_kind().is_none());
        assert_eq!(
            assert_native_audio_group_signal(&reopened, target, resolver),
            before
        );
        qualify_native_audio_transform_history(reopened, target, resolver, directory);
    }

    fn qualify_native_audio_transform_history(
        mut state: AppState,
        target: &mondrian_timeline::AudioAutomationTarget,
        resolver: &dyn AudioProcessorResolver,
        directory: &std::path::Path,
    ) {
        use super::super::product_action::{AudioProductAction, ProductAction};
        use mondrian_core::{KeyframeId, TimelineTime};
        use mondrian_timeline::{
            inspect_audio_automation, AudioAutomationEdit, AudioAutomationEditRequest,
        };
        let original_sequence = state.active_sequence().expect("native joint source").clone();
        let snapshot = |state: &AppState| {
            inspect_audio_automation(state.active_sequence().expect("sequence"), target)
                .expect("native target")
                .curve()
                .expect("native curve")
                .clone()
        };
        let original = snapshot(&state);
        let ids: Vec<_> = original.keyframes.iter().map(|key| key.id).collect();
        assert_eq!(ids.len(), 3);
        assert!(!state.can_undo_action() && !state.can_redo_action());
        let time_delta = TimelineTime::new(32, 48_000).expect("exact joint displacement");
        let action = |keyframe_ids, time_delta, value_delta| {
            ProductAction::Audio(AudioProductAction::EditAutomation(
                AudioAutomationEditRequest {
                    target: target.clone(),
                    edit: AudioAutomationEdit::TransformKeyframes {
                        keyframe_ids,
                        time_delta,
                        value_delta,
                        steps: 1,
                    },
                },
            ))
            .into_external_action()
        };
        let before = assert_native_audio_group_signal(&state, target, resolver);
        let generation = state.project_author_generation();
        for refused in [
            action(vec![ids[1], KeyframeId::new()], time_delta, 0.05),
            action(
                ids[1..].to_vec(),
                TimelineTime::new(-128, 48_000).expect("collision displacement"),
                0.05,
            ),
        ] {
            state
                .dispatch_action(refused)
                .expect_err("complete joint edit refuses atomically");
            assert!(state
                .active_sequence()
                .expect("unchanged")
                .author_state_eq_ignoring_revision(&original_sequence));
            assert_eq!(snapshot(&state), original);
            assert_eq!(state.project_author_generation(), generation);
            assert!(!state.can_undo_action() && !state.can_redo_action());
            assert_eq!(
                assert_native_audio_group_signal(&state, target, resolver),
                before
            );
        }
        let mut locked = original_sequence.clone();
        locked.audio_tracks[0].is_locked = true;
        let mut refused = AppState::new();
        refused.test_set_sequence(Some(locked.clone()));
        let locked_generation = refused.project_author_generation();
        refused
            .dispatch_action(action(ids[1..].to_vec(), time_delta, 0.05))
            .expect_err("locked native joint owner");
        assert!(refused
            .active_sequence()
            .expect("locked")
            .author_state_eq_ignoring_revision(&locked));
        assert_eq!(refused.project_author_generation(), locked_generation);
        assert!(!refused.can_undo_action() && !refused.can_redo_action());
        drop(refused);

        state
            .dispatch_action(action(ids[1..].to_vec(), time_delta, 0.05))
            .expect("one native joint transaction");
        assert_eq!(state.project_author_generation(), generation + 1);
        let edited = snapshot(&state);
        for (index, (old, new)) in original.keyframes.iter().zip(&edited.keyframes).enumerate() {
            assert_eq!(new.id, old.id);
            assert_eq!(new.tangent_mode, old.tangent_mode);
            assert_eq!(new.interpolation_to_next, old.interpolation_to_next);
            if index == 0 {
                assert_eq!(new.time, old.time);
                assert_eq!(
                    new.value.to_bits(),
                    old.value.to_bits(),
                    "unselected author value"
                );
            } else {
                assert_eq!(
                    new.time,
                    old.time.checked_add(time_delta).expect("translated time")
                );
                assert_eq!(new.value, old.value + 0.05);
            }
        }
        let after = assert_native_audio_group_signal(&state, target, resolver);
        assert_ne!(after, before, "joint native edit changes actual stereo PCM");
        let history = state.authoring_history().expect("history").diagnostics();
        assert_eq!(history.undo_entries, 1);
        state
            .dispatch_action(action(ids[1..].to_vec(), TimelineTime::ZERO, 0.0))
            .expect("zero joint displacement");
        assert_eq!(state.project_author_generation(), generation + 1);
        assert_eq!(
            state.authoring_history().expect("history").diagnostics(),
            history
        );
        let edited_sequence = state.active_sequence().expect("edited").clone();
        assert!(state.undo_timeline().expect("one joint Undo"));
        assert!(state
            .active_sequence()
            .expect("undone")
            .author_state_eq_ignoring_revision(&original_sequence));
        assert!(!state.can_undo_action() && state.can_redo_action());
        assert_eq!(
            assert_native_audio_group_signal(&state, target, resolver),
            before
        );
        let undone_path = directory.join("原生音频联合撤销🙂.mdp");
        state.save_project_file_as(undone_path.clone()).expect("save joint Undo");
        assert!(state.can_redo_action(), "save preserves live joint Redo");
        assert!(state.redo_timeline().expect("joint Redo after save"));
        assert_eq!(snapshot(&state), edited);
        assert_eq!(
            assert_native_audio_group_signal(&state, target, resolver),
            after
        );
        let edited_path = directory.join("原生音频联合重做🙂.mdp");
        state.save_project_file_as(edited_path.clone()).expect("save joint Redo");
        drop(state);
        for (path, sequence, curve, output) in [
            (undone_path, &original_sequence, &original, &before),
            (edited_path, &edited_sequence, &edited, &after),
        ] {
            let mut reopened = AppState::new();
            reopened.open_project_file(path).expect("fresh Unicode joint reopen");
            assert!(reopened
                .active_sequence()
                .expect("reopened")
                .author_state_eq_ignoring_revision(sequence));
            assert_eq!(&snapshot(&reopened), curve);
            assert!(!reopened.can_undo_action() && !reopened.can_redo_action());
            assert!(reopened.active_clipboard_kind().is_none());
            assert_eq!(
                &assert_native_audio_group_signal(&reopened, target, resolver),
                output
            );
            assert_eq!(
                reopened.asset_library().expect("SQLite").list_assets().expect("assets").len(),
                1
            );
            qualify_native_audio_reset_history(reopened, target, resolver, directory, output);
        }
    }

    fn qualify_native_audio_reset_history(
        mut state: AppState,
        target: &mondrian_timeline::AudioAutomationTarget,
        resolver: &dyn AudioProcessorResolver,
        directory: &std::path::Path,
        before: &[f32],
    ) {
        use super::super::product_action::{AudioProductAction, ProductAction};
        use mondrian_core::{automation::PropertyValue, AudioProcessorInstanceId};
        use mondrian_timeline::{
            inspect_audio_automation, AudioAutomationEdit, AudioAutomationEditRequest,
            AudioAutomationTarget, AudioChannelStripOwner, AudioChannelStripRack,
            AudioProcessorRackAddress,
        };
        let original = state.active_sequence().expect("reset origin").clone();
        let AudioAutomationTarget::ProcessorParameter {
            rack:
                AudioProcessorRackAddress::ChannelStrip {
                    owner: AudioChannelStripOwner::Track { track_id },
                    rack: AudioChannelStripRack::PreFader,
                },
            processor_id,
            parameter_id,
        } = target
        else {
            panic!("native gain fixture must address a track pre-fader parameter");
        };
        let original_parameter = original.audio_program.track_channels[track_id]
            .strip
            .pre_fader
            .processors
            .iter()
            .find(|processor| processor.id == *processor_id)
            .expect("native instance")
            .parameters[parameter_id]
            .clone();
        let PropertyValue::Double(declared_default) = original_parameter.schema.default_value
        else {
            panic!("native normalized gain fixture must declare a Double default");
        };
        assert!(!original_parameter
            .automation
            .as_ref()
            .expect("bound host curve")
            .keyframes
            .is_empty());
        assert!(!state.can_undo_action() && !state.can_redo_action());
        let action = |target| {
            ProductAction::Audio(AudioProductAction::EditAutomation(
                AudioAutomationEditRequest { target, edit: AudioAutomationEdit::ResetToDefault },
            ))
            .into_external_action()
        };
        let generation = state.project_author_generation();
        let initial_history = state.authoring_history().expect("history").diagnostics();
        let mut missing = target.clone();
        let AudioAutomationTarget::ProcessorParameter { processor_id: missing_instance_id, .. } =
            &mut missing
        else {
            panic!("native target");
        };
        *missing_instance_id = AudioProcessorInstanceId::new();
        state
            .dispatch_action(action(missing))
            .expect_err("missing native instance reset");
        assert!(state
            .active_sequence()
            .expect("unchanged")
            .author_state_eq_ignoring_revision(&original));
        assert_eq!(state.project_author_generation(), generation);
        assert_eq!(
            state.authoring_history().expect("history").diagnostics(),
            initial_history
        );
        let mut locked = original.clone();
        locked.audio_tracks[0].is_locked = true;
        let mut refused = AppState::new();
        refused.test_set_sequence(Some(locked.clone()));
        let locked_generation = refused.project_author_generation();
        refused
            .dispatch_action(action(target.clone()))
            .expect_err("locked native reset");
        assert!(refused
            .active_sequence()
            .expect("locked")
            .author_state_eq_ignoring_revision(&locked));
        assert_eq!(refused.project_author_generation(), locked_generation);
        assert!(!refused.can_undo_action() && !refused.can_redo_action());
        drop(refused);

        state
            .dispatch_action(action(target.clone()))
            .expect("native declared-default reset");
        assert_eq!(state.project_author_generation(), generation + 1);
        let reset_sequence = state.active_sequence().expect("reset").clone();
        let inspection = inspect_audio_automation(&reset_sequence, target).expect("reset target");
        assert_eq!(
            inspection.curve_contract().schema(),
            &original_parameter.schema
        );
        assert_eq!(inspection.default_value(), declared_default);
        let reset_curve = inspection.curve().expect("processor owns its unkeyed curve");
        assert!(reset_curve.keyframes.is_empty());
        assert_eq!(
            reset_curve.parameter_id,
            original_parameter.automation.as_ref().expect("bound host curve").parameter_id
        );
        let mut unrelated = reset_sequence.clone();
        unrelated
            .audio_program
            .track_channels
            .get_mut(track_id)
            .expect("track")
            .strip
            .pre_fader
            .processors
            .iter_mut()
            .find(|processor| processor.id == *processor_id)
            .expect("reset instance")
            .parameters
            .insert(parameter_id.clone(), original_parameter);
        assert!(unrelated.author_state_eq_ignoring_revision(&original), "only the addressed parameter changes; plugin state, bindings and other owners stay exact");
        let after = assert_native_audio_group_signal(&state, target, resolver);
        assert_ne!(after.as_slice(), before, "reset changes actual native PCM");
        for pair in after.chunks_exact(2) {
            let expected = 0.25 * declared_default as f32;
            assert!((pair[0] - expected).abs() <= 1e-6);
            assert!((pair[1] + expected).abs() <= 1e-6);
        }
        let history = state.authoring_history().expect("history").diagnostics();
        assert_eq!(history.undo_entries, 1);
        state
            .dispatch_action(action(target.clone()))
            .expect("already-default reset is a no-op");
        assert!(state
            .active_sequence()
            .expect("no-op")
            .author_state_eq_ignoring_revision(&reset_sequence));
        assert_eq!(state.project_author_generation(), generation + 1);
        assert_eq!(
            state.authoring_history().expect("history").diagnostics(),
            history
        );
        assert!(state.undo_timeline().expect("one reset Undo"));
        assert!(state
            .active_sequence()
            .expect("Undo")
            .author_state_eq_ignoring_revision(&original));
        assert!(!state.can_undo_action() && state.can_redo_action());
        assert_eq!(
            assert_native_audio_group_signal(&state, target, resolver).as_slice(),
            before
        );
        let undone_path = directory.join("原生音频默认重置撤销🙂.mdp");
        state.save_project_file_as(undone_path.clone()).expect("save reset Undo");
        assert!(state.can_redo_action(), "saving preserves live reset Redo");
        assert!(state.redo_timeline().expect("reset Redo after save"));
        assert!(state
            .active_sequence()
            .expect("Redo")
            .author_state_eq_ignoring_revision(&reset_sequence));
        let reset_path = directory.join("原生音频默认重置重做🙂.mdp");
        state.save_project_file_as(reset_path.clone()).expect("save reset Redo");
        drop(state);
        for (path, expected_sequence, expected_output) in [
            (undone_path, &original, before),
            (reset_path, &reset_sequence, after.as_slice()),
        ] {
            let mut reopened = AppState::new();
            reopened.open_project_file(path).expect("fresh native reset Unicode reopen");
            assert!(reopened
                .active_sequence()
                .expect("archive")
                .author_state_eq_ignoring_revision(expected_sequence));
            assert!(!reopened.can_undo_action() && !reopened.can_redo_action());
            assert!(reopened.active_clipboard_kind().is_none());
            assert_eq!(
                assert_native_audio_group_signal(&reopened, target, resolver).as_slice(),
                expected_output
            );
            assert_eq!(
                reopened.asset_library().expect("SQLite").list_assets().expect("assets").len(),
                1
            );
        }
    }

    fn assert_native_audio_group_signal(
        state: &AppState,
        target: &mondrian_timeline::AudioAutomationTarget,
        processor_resolver: &dyn AudioProcessorResolver,
    ) -> Vec<f32> {
        use mondrian_core::TimelineTime;
        let sequence = state.active_sequence().expect("author sequence");
        let curve = mondrian_timeline::inspect_audio_automation(sequence, target)
            .expect("native parameter")
            .curve()
            .expect("curve");
        let library = state.asset_library_handle().expect("source library");
        let mut reference_output = None;
        for (mode, partitions) in [
            (AudioProcessingMode::Realtime, [64, 64, 129]),
            (AudioProcessingMode::Offline, [37, 59, 161]),
        ] {
            let media_resolver = PlaybackMediaResolver {
                library: library.clone(),
                source_cache: Arc::new(AudioSourceCache::new(48_000)),
            };
            let mut runtime = AudioProgramRuntime::build_with_compile_request_processor_resolver_and_resource_grant(
                sequence, &[], &media_resolver, processor_resolver,
                AudioRenderContract { processing_mode: mode, ..reference_render_contract() },
                AudioCompileRequest {
                    output_id: sequence.audio_program.outputs[0].id,
                    audition: AudioAuditionOverlay::default(),
                }, test_runtime_grant(),
            ).expect("shared production audio program");
            runtime
                .enter_state(mondrian_audio::AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(1),
                    start_sample: 0,
                })
                .expect("native state entry");
            let mut output = Vec::new();
            let mut start_sample = 0_i64;
            for frames in partitions {
                let mut samples = vec![0.0; frames * 2];
                runtime
                    .render_into(AudioRenderRequest { start_sample, frames }, &mut samples)
                    .expect("native signal block");
                output.extend(samples);
                start_sample += i64::try_from(frames).expect("short block");
            }
            assert_eq!(output.len(), 257 * 2);
            for (sample, pair) in output.chunks_exact(2).enumerate() {
                let time = TimelineTime::new(sample as i64, 48_000).expect("sample time");
                let expected =
                    0.25 * curve.evaluate(time).expect("canonical author trajectory") as f32;
                assert!(
                    (pair[0] - expected).abs() <= 1e-6,
                    "{mode:?} left sample {sample}: {} vs {expected}",
                    pair[0]
                );
                assert!(
                    (pair[1] + expected).abs() <= 1e-6,
                    "{mode:?} right sample {sample}: {} vs {}",
                    pair[1],
                    -expected
                );
            }
            if let Some(reference) = &reference_output {
                assert_eq!(
                    &output, reference,
                    "native realtime/offline partitions preserve PCM"
                );
            } else {
                reference_output = Some(output);
            }
        }
        reference_output.expect("rendered native signal")
    }
}
