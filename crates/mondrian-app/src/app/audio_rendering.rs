use super::*;
use mondrian_audio::{
    AudioContinuityEpoch, AudioDecodedSource, AudioKernelBackend, AudioMediaResolver,
    AudioProcessingMode, AudioProgramRuntime, AudioRenderContract, AudioRenderRequest,
    AudioStateEntry, PreparedAudioChannelMixer, ResolvedAudioSource,
};
use mondrian_core::{
    AudioChannelLayout, AudioChannelMixMatrix, AudioSourceComponentId, ExecutionCancellationToken,
    ProgramOutputId,
};
use mondrian_media::AudioSourceReader;
use parking_lot::Mutex;

const MAX_AUDIO_RENDER_BLOCK_FRAMES: usize = 16_384;

pub(super) struct TimelineAudioPcmRenderer {
    state: Mutex<TimelineAudioRenderState>,
    continuity_model: AudioPcmContinuityModel,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    program_channel_layout: AudioChannelLayout,
    delivery_mixer: PreparedAudioChannelMixer,
}

struct TimelineAudioRenderState {
    runtime: AudioProgramRuntime,
    generation: Option<AudioPcmRenderGeneration>,
    next_sample: Option<i64>,
    program_pcm: Vec<f32>,
}

impl TimelineAudioPcmRenderer {
    pub(super) fn new(
        sequence: Sequence,
        sequences: Vec<Sequence>,
        library: Arc<AssetLibrary>,
        source_cache: Arc<AudioSourceCache>,
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
        let runtime = AudioProgramRuntime::build(
            &sequence,
            &sequences,
            &resolver,
            contract,
            None::<ProgramOutputId>,
        )
        .map_err(|error| audio_render_error("timeline_audio_prepare", error.to_string()))?;
        let continuity_model = if runtime.requires_state_entry() {
            AudioPcmContinuityModel::GenerationState
        } else {
            AudioPcmContinuityModel::IndependentWindows
        };
        let delivery_mixer = PreparedAudioChannelMixer::new(
            AudioChannelMixMatrix::standard(program_channel_layout, channel_layout).map_err(
                |error| audio_render_error("playback_audio_delivery_mapping", error.to_string()),
            )?,
        );
        let program_samples = MAX_AUDIO_RENDER_BLOCK_FRAMES
            .checked_mul(program_channel_layout.channel_count())
            .ok_or_else(|| {
                audio_render_error(
                    "playback_audio_delivery_mapping",
                    "program audio scratch extent is too large",
                )
            })?;
        Ok(Self {
            state: Mutex::new(TimelineAudioRenderState {
                runtime,
                generation: None,
                next_sample: None,
                program_pcm: vec![0.0; program_samples],
            }),
            continuity_model,
            sample_rate,
            channel_layout,
            program_channel_layout,
            delivery_mixer,
        })
    }
}

impl AudioPcmRenderer for TimelineAudioPcmRenderer {
    fn continuity_model(&self) -> AudioPcmContinuityModel {
        self.continuity_model
    }

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
                    .runtime
                    .enter_state(AudioStateEntry {
                        epoch: AudioContinuityEpoch::new(generation.get()),
                        start_sample: request.start_sample,
                    })
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
        let program_samples = request
            .frame_count
            .checked_mul(self.program_channel_layout.channel_count())
            .ok_or_else(|| {
                audio_render_error("timeline_audio_sample_range", "audio window is too large")
            })?;
        let TimelineAudioRenderState { runtime, program_pcm, .. } = &mut *state;
        runtime
            .render_into_cancellable(
                AudioRenderRequest {
                    start_sample: request.start_sample,
                    frames: request.frame_count,
                },
                &mut program_pcm[..program_samples],
                cancellation,
            )
            .map_err(|error| audio_render_error("timeline_audio_execute", error.to_string()))?;
        self.delivery_mixer
            .mix_into(
                AudioKernelBackend::RuntimeVectorized,
                request.frame_count,
                &program_pcm[..program_samples],
                &mut output,
            )
            .map_err(|error| {
                audio_render_error("playback_audio_delivery_mapping", error.to_string())
            })?;
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
        let current_fingerprint = mondrian_media::MediaFileFingerprint::capture(&asset.path);
        let selection = asset
            .audio_components
            .resolve_current_selection(component_id, &asset.media_info, current_fingerprint)
            .map_err(|error| {
                format!(
                    "failed to bind audio Component {component_id} for Asset {asset_id}: {error}"
                )
            })?;
        let source = self.source_cache.open(asset.path.as_path(), selection).map_err(|error| {
            format!(
                "failed to open bounded audio source {} at {}: {error}",
                asset.id,
                asset.path.display()
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
            48_000,
            AudioChannelLayout::Stereo,
        )
        .expect("mono program with stereo delivery");

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
}
