//! Real Mono/stereo native configuration through the public immutable Export queue.
#![cfg(windows)]

#[path = "support/native_gain_export.rs"]
mod native_gain_export;

use mondrian_audio::{
    probe_clap_plugin_registration, AudioProcessingMode, AudioRenderContract, ClapMainAudioInput,
    DiscoveredClapAudioProcessorSpecResolver,
};
use mondrian_core::{AudioChannelLayout, ParameterId, Rational};
use std::{path::PathBuf, sync::Arc};

#[test]
#[ignore = "real Ring Out state/config inputs, matching Product, FFmpeg and original native Job"]
fn production_queue_ring_out_mono_config_preserves_native_state_and_pcm() {
    let path = |key| PathBuf::from(std::env::var_os(key).expect("real config input"));
    let helper = path("MONDRIAN_CLAP_TEST_HELPER");
    let plugin = path("MONDRIAN_RING_OUT_TEST_PLUGIN");
    let catalog = Arc::new(
        DiscoveredClapAudioProcessorSpecResolver::discover(helper.clone(), [plugin.clone()])
            .unwrap(),
    );
    assert_eq!(catalog.descriptors().len(), 1);
    const ID: &str = "com.duskaudio.ring-out";
    assert_eq!(catalog.descriptors()[0].plugin_id, ID);
    for (state_env, gain) in [
        ("MONDRIAN_RING_OUT_FLAT_STATE", 0.0_f64),
        ("MONDRIAN_RING_OUT_GAIN_STATE", -6.0_f64),
    ] {
        let state_path = path(state_env);
        assert!(std::fs::metadata(&state_path).unwrap().len() <= 64 * 1024);
        let state = std::fs::read(state_path).unwrap();
        assert!(!state.is_empty());
        for (layout, rate) in [
            (AudioChannelLayout::Mono, 44_100),
            (AudioChannelLayout::Mono, 48_000),
            (AudioChannelLayout::Stereo, 48_000),
        ] {
            let render = AudioRenderContract {
                sample_rate: rate,
                channel_layout: layout,
                max_block_frames: 16_384,
                processing_mode: AudioProcessingMode::Offline,
                processor_session_scratch_budget_bytes:
                    AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
                public_output_lookahead_budget_frames:
                    AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
                compensation_delay_scratch_budget_bytes:
                    AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
            };
            let registration =
                probe_clap_plugin_registration(&helper, &plugin, ID, render, Some(&state)).unwrap();
            assert_eq!(registration.main_audio_input, ClapMainAudioInput::Present);
            assert!(registration.auxiliary_inputs.buses.is_empty());
            assert!(registration.auxiliary_outputs.buses.is_empty());
            for (id, value) in [(7, gain), (8, 0.0)] {
                assert_eq!(
                    registration
                        .parameters
                        .iter()
                        .find(|p| p.id == id)
                        .unwrap()
                        .current_value
                        .to_bits(),
                    value.to_bits()
                );
            }
            let instance = catalog.create_instance(ID, render, Some(state.clone())).unwrap();
            assert!(!instance.bypassed);
            assert!(instance.parameters.contains_key(&ParameterId::new("clap.param.7").unwrap()));
            assert!(instance.parameters.values().all(|p| p.automation.is_none()));
            assert_eq!(
                instance.opaque_state.as_ref().unwrap().as_slice(),
                state.as_slice()
            );
            native_gain_export::export_gain_case(
                catalog.clone(),
                instance,
                render,
                Rational::FPS_24,
                24,
                |frame, channel| (((frame * 29 + channel * 11) % 113) as f32 - 56.0) / 256.0,
                gain,
            );
        }
    }
}
