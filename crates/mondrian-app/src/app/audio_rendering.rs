use super::*;
use mondrian_audio::{
    AudioDecodedSource, AudioMediaResolver, AudioProcessingMode, AudioProgramRuntime,
    AudioRenderContract, AudioRenderRequest,
};
use mondrian_core::{AudioSourceComponentId, ProgramOutputId};
use parking_lot::Mutex;

const MAX_AUDIO_RENDER_BLOCK_FRAMES: usize = 16_384;

pub(super) struct TimelineAudioPcmRenderer {
    runtime: Mutex<AudioProgramRuntime>,
    sample_rate: u32,
    channels: u8,
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
        Ok(Self {
            runtime: Mutex::new(runtime),
            sample_rate,
            channels,
        })
    }
}

impl AudioPcmRenderer for TimelineAudioPcmRenderer {
    fn render(&self, request: AudioPcmRenderRequest) -> mondrian_core::Result<AudioBuffer> {
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
        let mut output = vec![0.0; samples];
        self.runtime
            .lock()
            .render_into(
                AudioRenderRequest {
                    start_sample: request.start_sample,
                    frames: request.frame_count,
                },
                &mut output,
            )
            .map_err(|error| audio_render_error("timeline_audio_execute", error.to_string()))?;
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
        let buffer = self.source_cache.get_or_decode(asset.path.as_path()).map_err(|error| {
            format!(
                "failed to decode {} at {}: {error}",
                asset.id,
                asset.path.display()
            )
        })?;
        Ok(Arc::new(DecodedAudioBuffer(buffer)))
    }
}

struct DecodedAudioBuffer(Arc<AudioBuffer>);

impl AudioDecodedSource for DecodedAudioBuffer {
    fn sample(&self, frame: i64, channel: usize) -> f32 {
        let Ok(frame) = usize::try_from(frame) else {
            return 0.0;
        };
        let channels = usize::from(self.0.channels);
        let source_channel = channel.min(channels.saturating_sub(1));
        self.0
            .samples
            .get(frame.saturating_mul(channels).saturating_add(source_channel))
            .copied()
            .unwrap_or(0.0)
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
