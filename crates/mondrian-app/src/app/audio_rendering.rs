use super::*;
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
use mondrian_media::{AudioSourceReader, AudioSourceSelection};
use parking_lot::Mutex;

pub(super) const MAX_AUDIO_RENDER_BLOCK_FRAMES: usize = 16_384;

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
            &BuiltInAudioProcessorResolver,
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
        processor_resolver: &dyn AudioProcessorResolver,
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
        let runtime =
            AudioProgramRuntime::build_with_compile_request_processor_resolver_and_resource_grant(
                &sequence,
                &sequences,
                &resolver,
                processor_resolver,
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
            resolver,
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
        use std::io::Write;

        let root = tempfile::Builder::new()
            .prefix("mondrian-native-audio-音频🙂-")
            .tempdir()
            .expect("native audio fixture directory");
        let source_path = root.path().join("constant-stereo.wav");
        let sample_rate = 48_000_u32;
        let frames = sample_rate as usize;
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
        for _ in 0..frames {
            wave.write_all(&0.25_f32.to_le_bytes()).expect("left sample");
            wave.write_all(&(-0.25_f32).to_le_bytes()).expect("right sample");
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
                    mondrian_core::TimelineTime::ONE,
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
        assert!(!original_parameter.automation.keyframes.is_empty());
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
            original_parameter.automation.parameter_id
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
