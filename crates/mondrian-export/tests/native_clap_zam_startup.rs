//! Real public Export consumer: preserve native state and authored overrides.
#![cfg(windows)]

use mondrian_audio::{
    AudioProcessingMode, AudioRenderContract, DiscoveredClapAudioProcessorSpecResolver,
};
use mondrian_core::{AudioChannelLayout, ExactAutomationCurve, ParameterId, Rational};
use std::{path::PathBuf, sync::Arc};
#[path = "support/native_gain_export.rs"]
mod native_gain_export;
use native_gain_export::export_gain_case;

#[test]
#[ignore = "official ZamCompX2 4.5 CLAP, matching Product, FFmpeg PCM24 and supervised native Job"]
fn production_queue_zam_clap_preserves_authored_makeup_on_state_load() {
    export_makeup_case(None, Some(12.0), 12.0);
}

#[test]
#[ignore = "official Zam4.5 CLAP, actual captured Makeup12dB state, matching Product and supervised public Export"]
fn production_queue_zam_clap_nondefault_state_preserves_native_and_host_authority() {
    let state = std::fs::read(
        std::env::var_os("MONDRIAN_ZAM_NONDEFAULT_STATE").expect("actual native state capture"),
    )
    .unwrap();
    assert_eq!(
        state.len(),
        137,
        "current actual capture; no synthetic state"
    );
    for host_override in [None, Some(6.0)] {
        export_makeup_case(
            Some(state.clone()),
            host_override,
            host_override.unwrap_or(12.0),
        );
    }
}

fn export_makeup_case(state: Option<Vec<u8>>, host_override: Option<f64>, expected_db: f64) {
    let path = |key| PathBuf::from(std::env::var_os(key).expect("native qualification input"));
    let catalog = Arc::new(
        DiscoveredClapAudioProcessorSpecResolver::discover(
            path("MONDRIAN_CLAP_TEST_HELPER"),
            [path("MONDRIAN_CLAP_MIXED_VENDOR_TEST_PLUGIN")],
        )
        .unwrap(),
    );
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 16_384,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    };
    let mut instance = catalog
        .create_instance("com.zamaudio.ZamCompX2", render, state.clone())
        .unwrap();
    if let Some(expected) = &state {
        assert_eq!(
            instance.opaque_state.as_ref().unwrap().as_slice(),
            expected.as_slice()
        );
    }
    assert!(!instance.opaque_state.as_ref().unwrap().is_empty());
    assert!(instance.parameters.values().all(|parameter| parameter.automation.is_none()));
    let makeup = ParameterId::new("clap.param.5").unwrap();
    if let Some(value) = host_override {
        instance
            .parameters
            .get_mut(&makeup)
            .unwrap()
            .set_automation(ExactAutomationCurve::new(makeup, value).unwrap())
            .unwrap();
    }
    export_gain_case(
        catalog,
        instance,
        render,
        Rational::FPS_25,
        25,
        |_, channel| [0.03125_f32, -0.0625][channel],
        expected_db,
    );
}
