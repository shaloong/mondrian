use super::*;
use mondrian_audio::{
    AudioContinuityEpoch, AudioDecodedSource, AudioMediaResolver, AudioProcessingMode,
    AudioProgramRuntime, AudioRenderContract, AudioRenderRequest, AudioStateEntry,
};
use mondrian_core::{AudioSourceComponentId, ExecutionCancellationToken, ProgramOutputId};
use mondrian_media::AudioSourceReader;
use parking_lot::Mutex;

const MAX_AUDIO_RENDER_BLOCK_FRAMES: usize = 16_384;

pub(super) struct TimelineAudioPcmRenderer {
    state: Mutex<TimelineAudioRenderState>,
    continuity_model: AudioPcmContinuityModel,
    sample_rate: u32,
    channels: u8,
}

struct TimelineAudioRenderState {
    runtime: AudioProgramRuntime,
    generation: Option<AudioPcmRenderGeneration>,
    next_sample: Option<i64>,
}

impl TimelineAudioPcmRenderer {
    pub(super) fn new(
        sequence: Sequence,
        sequences: Vec<Sequence>,
        library: Arc<AssetLibrary>,
        source_cache: Arc<AudioSourceCache>,
        sample_rate: u32,
        channels: u8,
    ) -> mondrian_core::Result<Self> {
        let contract = AudioRenderContract {
            sample_rate,
            channels: usize::from(channels),
            max_block_frames: MAX_AUDIO_RENDER_BLOCK_FRAMES,
            processing_mode: AudioProcessingMode::Realtime,
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
        if runtime.requires_state_entry() && !runtime.supports_state_entry() {
            return Err(audio_render_error(
                "timeline_audio_state_entry",
                "stateful nested audio requires direction/time-map-aware child replay",
            ));
        }
        let continuity_model = if runtime.requires_state_entry() {
            AudioPcmContinuityModel::GenerationState
        } else {
            AudioPcmContinuityModel::IndependentWindows
        };
        Ok(Self {
            state: Mutex::new(TimelineAudioRenderState {
                runtime,
                generation: None,
                next_sample: None,
            }),
            continuity_model,
            sample_rate,
            channels,
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
        if request.sample_rate != self.sample_rate || request.channels != self.channels {
            return Err(audio_render_error(
                "timeline_audio_render_contract",
                format!(
                    "requested {} Hz/{} ch but Adapter is configured for {} Hz/{} ch",
                    request.sample_rate, request.channels, self.sample_rate, self.channels
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
        let samples =
            request.frame_count.checked_mul(usize::from(self.channels)).ok_or_else(|| {
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
        state
            .runtime
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
            channels: self.channels,
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
        _contract: AudioRenderContract,
    ) -> Result<Arc<dyn AudioDecodedSource>, String> {
        if component_id != AudioSourceComponentId::primary() {
            return Err(format!(
                "audio component {component_id} is not bound to a decoded media stream"
            ));
        }
        let asset = self
            .library
            .get_asset(asset_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("Asset {asset_id} is unavailable"))?;
        let source = self.source_cache.open(asset.path.as_path()).map_err(|error| {
            format!(
                "failed to open bounded audio source {} at {}: {error}",
                asset.id,
                asset.path.display()
            )
        })?;
        Ok(Arc::new(PlaybackDecodedAudioSource(source)))
    }
}

struct PlaybackDecodedAudioSource(AudioSourceReader);

impl AudioDecodedSource for PlaybackDecodedAudioSource {
    fn read_interleaved(
        &self,
        start_frame: i64,
        frames: usize,
        channels: usize,
        destination: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), String> {
        self.0
            .read_interleaved_cancellable(start_frame, frames, channels, destination, cancellation)
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
            Arc::new(AudioSourceCache::new(48_000, 2)),
            48_000,
            2,
        )
        .expect("stateless renderer");
        let cancellation = ExecutionCancellationToken::new();
        let generation = AudioPcmRenderGeneration::new(7);
        let request = |continuity, start_sample| AudioPcmRenderRequest {
            start_sample,
            frame_count: 4,
            sample_rate: 48_000,
            channels: 2,
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
}
