//! Finite third-party VST3 sidechain qualification; no vendor DSP implementation.
use super::*;
use crate::{
    compile_audio_program, AudioCompileRequest, AudioContinuityEpoch, AudioPcmSource,
    AudioRenderSession, AudioStateEntry, PreparedAudioPlan,
};
use mondrian_core::{
    AssetId, AudioComponentEditId, AudioSourceComponentId, ExactAutomationCurve, TimelineTime,
};
use mondrian_timeline::audio::{
    AudioChannelStripOutputPort, AudioProcessorSidechainRoute, AudioRouteSource,
};
use mondrian_timeline::{Clip, Sequence};

struct Source {
    main: AudioComponentEditId,
    detector: f32,
}
impl AudioPcmSource for Source {
    fn read_indexed_interleaved(
        &mut self,
        edit: AudioComponentEditId,
        positions: &[i64],
        layout: AudioChannelLayout,
        pcm: &mut [f32],
    ) -> Result<(), crate::AudioExecutionError> {
        assert_eq!(layout, AudioChannelLayout::Stereo);
        for (frame, position) in pcm.chunks_exact_mut(2).zip(positions) {
            frame.copy_from_slice(&if *position < 0 {
                [0.0; 2]
            } else if edit == self.main {
                [0.03125, -0.0625]
            } else {
                [self.detector; 2]
            });
        }
        Ok(())
    }
}

#[test]
#[ignore = "requires official ZamCompX2 4.5 VST3, matching Product and original native Job"]
fn installed_zam_compx2_vst3_mono_sidechain_controls_canonical_stereo_pcm() {
    let helper = PathBuf::from(std::env::var_os("MONDRIAN_VST3_TEST_HELPER").unwrap());
    let plugin = PathBuf::from(std::env::var_os("MONDRIAN_VST3_MIXED_VENDOR_TEST_PLUGIN").unwrap());
    let catalog = Arc::new(
        DiscoveredVst3AudioProcessorSpecResolver::discover(helper.clone(), [plugin.clone()])
            .unwrap(),
    );
    const ID: &str = "445046206173636C3250435A00000000";
    assert_eq!(catalog.descriptors().len(), 1);
    assert_eq!(catalog.descriptors()[0].class_id, ID);
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 64,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 16 * 1024 * 1024,
        public_output_lookahead_budget_frames: 4096,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    };
    let registration = probe_vst3_plugin_registration(&helper, &plugin, ID, render, None).unwrap();
    assert_eq!(registration.main_audio_input, Vst3MainAudioInput::Present);
    assert_eq!(registration.auxiliary_inputs.buses.len(), 1);
    let bus = &registration.auxiliary_inputs.buses[0];
    assert_eq!(bus.channel_layout, AudioChannelLayout::Mono);
    assert_eq!(registration.audio_output_count, 1);
    assert_eq!(registration.main_audio_output_count, 1);
    assert_eq!(registration.parameters.len(), 14);
    assert_eq!(
        registration.parameters.iter().filter(|p| p.read_only).count(),
        4
    );
    let initial = catalog.create_instance(ID, render, None).unwrap();
    assert_eq!(initial.parameters.len(), 10);
    assert!(initial.parameters.values().all(|p| p.automation.is_none()));
    let state = initial.opaque_state.as_ref().expect("real native default state");
    assert!(!state.is_empty());
    let mut results = Vec::new();
    // Same low Main signal: only external detector and the public sidechain
    // parameter differ. No test-specific behavior exists in the host adapter.
    for (enabled, detector) in [(1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
        let mut instance = catalog.create_instance(ID, render, Some(state.to_vec())).unwrap();
        assert_eq!(instance.opaque_state.as_ref(), Some(state));
        // Native normalized controls: fastest Attack, hard knee, Ratio20,
        // Threshold-18dB in [-80,0], Makeup0dB and explicit sidechain toggle.
        for (id, value) in [
            (3, 0.0),
            (5, 0.0),
            (6, 1.0),
            (7, 0.775),
            (8, 0.0),
            (11, enabled),
        ] {
            let descriptor = registration.parameters.iter().find(|p| p.id == id).unwrap();
            let parameter_id = descriptor.parameter_id().unwrap();
            instance
                .parameters
                .get_mut(&parameter_id)
                .unwrap()
                .set_automation(ExactAutomationCurve::new(parameter_id.clone(), value).unwrap())
                .unwrap();
        }
        let mut sequence = Sequence::new("ZamCompX2 finite mixed-input qualification");
        let main = sequence.audio_tracks[0].id;
        let detector_track = sequence.add_audio_track();
        for track in [main, detector_track] {
            sequence
                .add_media_audio_clip(
                    track,
                    Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
                    AudioSourceComponentId::primary(),
                )
                .unwrap();
        }
        let main_edit = sequence.audio_tracks.iter().find(|t| t.id == main).unwrap().clips[0]
            .audio_components[0]
            .id;
        let processor = instance.id;
        sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
        sequence.audio_program.routes.retain(|route|
            matches!(route.source, AudioRouteSource::Track { track_id, .. } if track_id == main));
        sequence.audio_program.sidechain_routes.push(AudioProcessorSidechainRoute::new(
            AudioRouteSource::Track {
                track_id: detector_track,
                port: AudioChannelStripOutputPort::PostMute,
            },
            processor,
            bus.bus_key.clone(),
        ));
        let before = serde_json::to_value(&sequence).unwrap();
        let program = Arc::new(
            compile_audio_program(
                &sequence,
                AudioCompileRequest::program(sequence.audio_program.outputs[0].id),
            )
            .unwrap(),
        );
        let resolver = IsolatedAudioProcessorResolver::new(catalog.clone());
        let plan = Arc::new(
            PreparedAudioPlan::prepare_with_processor_resolver(
                program,
                render,
                AudioKernelBackend::ScalarReference,
                &resolver,
            )
            .unwrap(),
        );
        assert_eq!(plan.public_output_lookahead_frames(), 0);
        let mut session = AudioRenderSession::new(plan).unwrap();
        let mut reference = None;
        for (epoch, start, partitions) in [(1, 0, [64, 64]), (2, 0, [17, 47]), (3, 97, [47, 17])] {
            session
                .enter_state(AudioStateEntry {
                    epoch: AudioContinuityEpoch::new(epoch),
                    start_sample: start,
                })
                .unwrap();
            let mut source = Source { main: main_edit, detector };
            let mut output = Vec::new();
            let mut block_index = 0;
            while output.len() < 2048 {
                let frames = partitions[block_index % 2].min((2048 - output.len()) / 2);
                let mut pcm = vec![f32::NAN; frames * 2];
                session
                    .render_into(
                        &mut source,
                        AudioRenderRequest {
                            transport_activity: None,
                            start_sample: start + output.len() as i64 / 2,
                            frames,
                        },
                        &mut pcm,
                    )
                    .unwrap();
                assert!(pcm.iter().all(|x| x.is_finite()));
                output.extend(pcm);
                block_index += 1;
            }
            if let Some(expected) = &reference {
                assert_eq!(&output, expected, "partition/seek invariance");
            } else {
                reference = Some(output);
            }
        }
        assert_eq!(serde_json::to_value(&sequence).unwrap(), before);
        results.push(reference.unwrap());
    }
    for dry in &results[..2] {
        for frame in dry.chunks_exact(2) {
            assert_eq!(frame, [0.03125, -0.0625]);
        }
    }
    let compressed = &results[2];
    assert_ne!(compressed, &results[0]);
    for frame in compressed.chunks_exact(2).skip(512) {
        let gain = frame[0] / 0.03125;
        assert!(
            gain > 0.0 && gain < 0.5,
            "external detector must attenuate low Main"
        );
        assert_eq!(frame[1], -2.0 * frame[0], "stereo relationship is retained");
    }
}
