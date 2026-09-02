use super::*;
use mondrian_audio::{
    AudioAuditionOverlay, AudioCompileRequest, AudioContinuityEpoch, AudioDecodedSource,
    AudioDeliveryEvidence, AudioMediaResolver, AudioMeterObserver, AudioProcessingMode,
    AudioProgramDeliveryRuntime, AudioProgramExecutionDemand, AudioProgramRuntime,
    AudioRenderContract, AudioRenderRequest, AudioRuntimeResourceGrant, ResolvedAudioSource,
};
use mondrian_core::{AudioChannelLayout, AudioSourceComponentId, ExecutionCancellationToken};
use mondrian_media::{AudioSourceReader, AudioSourceSelection};
use parking_lot::Mutex;

const MAX_AUDIO_RENDER_BLOCK_FRAMES: usize = 16_384;

pub(super) struct TimelineAudioPcmRenderer {
    state: Mutex<TimelineAudioRenderState>,
    continuity_model: AudioPcmContinuityModel,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
}

struct TimelineAudioRenderState {
    delivery: AudioProgramDeliveryRuntime,
    generation: Option<AudioPcmRenderGeneration>,
    next_sample: Option<i64>,
}

impl TimelineAudioPcmRenderer {
    /// Prepare the canonical public Program without audition or channel remapping.
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
        let program_channel_layout = sequence.settings.audio_channel_layout;
        let contract = AudioRenderContract {
            sample_rate,
            channel_layout: program_channel_layout,
            max_block_frames: MAX_AUDIO_RENDER_BLOCK_FRAMES,
            processing_mode: AudioProcessingMode::Realtime,
            processor_session_scratch_budget_bytes:
                AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
            public_output_lookahead_budget_frames:
                AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
            compensation_delay_scratch_budget_bytes:
                AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
        };
        let resolver = PlaybackMediaResolver { library, source_cache };
        let output_id =
            sequence.audio_program.outputs.first().map(|output| output.id).ok_or_else(|| {
                audio_render_error("timeline_audio_prepare", "Sequence has no Program Output")
            })?;
        let runtime = AudioProgramRuntime::build_with_compile_request_and_resource_grant(
            &sequence,
            &sequences,
            &resolver,
            contract,
            AudioCompileRequest { output_id, audition },
            runtime_grant,
        )
        .map_err(|error| audio_render_error("timeline_audio_prepare", error.to_string()))?;
        let continuity_model = if runtime.requires_state_entry() {
            AudioPcmContinuityModel::GenerationState
        } else {
            AudioPcmContinuityModel::IndependentWindows
        };
        let delivery = AudioProgramDeliveryRuntime::prepare_standard(runtime, channel_layout)
            .map_err(|error| {
                audio_render_error("playback_audio_delivery_mapping", error.to_string())
            })?;
        Ok(Self {
            state: Mutex::new(TimelineAudioRenderState {
                delivery,
                generation: None,
                next_sample: None,
            }),
            continuity_model,
            sample_rate,
            channel_layout,
        })
    }

    /// Compiler-owned evidence for replacing this Program Output with silence.
    pub(super) fn execution_demand(&self) -> AudioProgramExecutionDemand {
        self.state.lock().delivery.execution_demand()
    }

    /// Lock-free meter observation bound to this exact prepared Runtime.
    pub(super) fn meter_observer(&self) -> AudioMeterObserver {
        self.state.lock().delivery.meter_observer()
    }

    /// Exact Sequence Program Output to monitoring-target delivery evidence.
    pub(super) fn delivery_evidence(&self) -> AudioDeliveryEvidence {
        self.state.lock().delivery.evidence()
    }

    /// Immutable cross-window state contract captured before trait erasure.
    pub(super) const fn continuity_model(&self) -> AudioPcmContinuityModel {
        self.continuity_model
    }
}

impl AudioPcmRenderer for TimelineAudioPcmRenderer {
    fn render(
        &self,
        request: AudioPcmRenderRequest,
        cancellation: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<AudioBuffer> {
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
        if request.frame_count > MAX_AUDIO_RENDER_BLOCK_FRAMES {
            return Err(audio_render_error(
                "timeline_audio_render_contract",
                format!(
                    "requested {} frames but the prepared maximum is {}",
                    request.frame_count, MAX_AUDIO_RENDER_BLOCK_FRAMES
                ),
            ));
        }
        let samples = request
            .frame_count
            .checked_mul(self.channel_layout.channel_count())
            .ok_or_else(|| {
                audio_render_error("timeline_audio_sample_range", "audio window is too large")
            })?;
        let next_sample = request
            .start_sample
            .checked_add(i64::try_from(request.frame_count).map_err(|_| {
                audio_render_error("timeline_audio_sample_range", "audio window is too large")
            })?)
            .ok_or_else(|| {
                audio_render_error("timeline_audio_sample_range", "audio window is too large")
            })?;
        let mut output = vec![0.0; samples];
        let mut state = self.state.lock();
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
                state
                    .delivery
                    .enter_state(
                        AudioContinuityEpoch::new(generation.get()),
                        request.start_sample,
                    )
                    .map_err(|error| {
                        audio_render_error("timeline_audio_state_entry", error.to_string())
                    })?;
                state.generation = Some(generation);
                state.next_sample = Some(request.start_sample);
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
        if state.next_sample != Some(request.start_sample) {
            return Err(audio_render_error(
                "timeline_audio_continuity",
                format!(
                    "render generation {} expected sample {:?}, got {}",
                    generation.get(),
                    state.next_sample,
                    request.start_sample
                ),
            ));
        }
        state
            .delivery
            .render_into_cancellable(
                AudioRenderRequest {
                    start_sample: request.start_sample,
                    frames: request.frame_count,
                },
                &mut output,
                cancellation,
            )
            .map_err(|error| audio_render_error("timeline_audio_execute", error.to_string()))?;
        state.next_sample = Some(next_sample);
        Ok(AudioBuffer {
            samples: output,
            sample_rate: self.sample_rate,
            channel_layout: self.channel_layout,
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_runtime_grant() -> AudioRuntimeResourceGrant {
        AudioRuntimeResourceGrant::new(64, 512 * 1024 * 1024, 128 * 1024 * 1024)
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
}
