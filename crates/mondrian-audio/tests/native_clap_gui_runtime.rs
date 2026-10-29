//! Real Ring Out GUI and PCM through the public Runtime/Graph and normal Product.

#[path = "support/native_gui_runtime.rs"]
mod gui;
#[path = "support/portable_native_runtime.rs"]
mod support;

use mondrian_audio::{
    probe_clap_plugin_registration, AudioProcessingMode, AudioRenderContract,
    DiscoveredClapAudioProcessorSpecResolver, IsolatedAudioProcessorResolver,
};
use mondrian_core::AudioChannelLayout;
use std::{path::PathBuf, sync::Arc};

#[test]
#[ignore = "requires matching normal Product, official Ring Out CLAP and actual -6dB state; Linux requires authenticated X11/GLX"]
fn ring_out_gui_retains_installed_object_state_and_pcm_through_resize_and_close() {
    let helper =
        PathBuf::from(std::env::var_os("MONDRIAN_CLAP_TEST_HELPER").expect("normal Product"));
    let plugin =
        PathBuf::from(std::env::var_os("MONDRIAN_CLAP_TEST_PLUGIN").expect("official Ring Out"));
    let state =
        std::fs::read(std::env::var_os("MONDRIAN_CLAP_TEST_STATE").expect("actual -6dB state"))
            .unwrap();
    assert!(!state.is_empty());
    const ID: &str = "com.duskaudio.ring-out";
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 64,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 32 * 1024 * 1024,
        public_output_lookahead_budget_frames: 4096,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    };
    let registration =
        probe_clap_plugin_registration(&helper, &plugin, ID, render, Some(&state)).unwrap();
    assert!(registration.auxiliary_inputs.buses.is_empty());
    assert!(registration.auxiliary_outputs.buses.is_empty());
    // Actual state getters, not parameter defaults. Empty filters and bypass off
    // make the saved output gain an independent full-signal PCM oracle.
    for (id, value) in [(7, -6.0), (8, 0.0)] {
        assert_eq!(
            registration.parameters.iter().find(|p| p.id == id).unwrap().current_value,
            value
        );
    }
    let catalog =
        Arc::new(DiscoveredClapAudioProcessorSpecResolver::discover(helper, [plugin]).unwrap());
    let author = catalog.create_instance(ID, render, Some(state.clone())).unwrap();
    assert_eq!(
        author.opaque_state.as_deref().map(Vec::as_slice),
        Some(state.as_slice())
    );
    assert!(author.parameters.values().all(|parameter| parameter.automation.is_none()));
    let (sequence, asset, locator) = gui::effect_sequence(author, render);
    let resolver = IsolatedAudioProcessorResolver::new(catalog);
    // Ring Out declares a resizable editor; verify the negotiated native size.
    gui::exercise_gui_runtime(
        &sequence,
        asset,
        &resolver,
        render,
        locator,
        10_f32.powf(-6.0 / 20.0),
        true,
    );
}
