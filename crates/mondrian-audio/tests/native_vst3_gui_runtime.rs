//! Official AGain GUI through the public Runtime/Graph and normal Product.
#[path = "support/native_gui_runtime.rs"]
mod gui;
#[path = "support/portable_native_runtime.rs"]
mod support;
use mondrian_audio::{
    probe_vst3_plugin_registration, AudioProcessingMode, AudioRenderContract,
    DiscoveredVst3AudioProcessorSpecResolver, IsolatedAudioProcessorResolver,
};
use mondrian_core::{AudioChannelLayout, ExactAutomationCurve};
use std::{path::PathBuf, sync::Arc};

#[test]
#[ignore = "requires matching normal Product and unmodified SDK3.8.0 AGain GUI; Linux requires authenticated X11 display"]
fn again_gui_retains_installed_object_pcm_and_native_parent_through_repeated_close() {
    let helper =
        PathBuf::from(std::env::var_os("MONDRIAN_VST3_TEST_HELPER").expect("normal Product"));
    let plugin = PathBuf::from(
        std::env::var_os("MONDRIAN_VST3_GUI_TEST_PLUGIN").expect("official GUI reference"),
    );
    let catalog = Arc::new(
        DiscoveredVst3AudioProcessorSpecResolver::discover(helper.clone(), [plugin.clone()])
            .unwrap(),
    );
    // Explicit official reference class, independently of discovery ordering or captions.
    let class = &catalog
        .descriptors()
        .iter()
        .find(|d| d.class_id.eq_ignore_ascii_case("84E8DE5F92554F5396FAE4133C935A18"))
        .expect("official non-sidechain AGain class")
        .class_id;
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
        probe_vst3_plugin_registration(&helper, &plugin, class, render, None).unwrap();
    assert!(registration.auxiliary_inputs.buses.is_empty());
    let mut author = catalog.create_instance(class, render, None).unwrap();
    assert!(author.opaque_state.as_ref().is_some_and(|state| !state.is_empty()));
    // Explicit gain and bypass stimuli, not assumptions about plugin defaults.
    let gain = 0.5_f32;
    for (id, value) in [(0, f64::from(gain)), (2, 0.0)] {
        let parameter = registration.parameters.iter().find(|p| p.id == id).unwrap();
        assert!(parameter.can_automate && !parameter.read_only);
        author
            .set_parameter_automation(
                ExactAutomationCurve::new(parameter.parameter_id().unwrap(), value).unwrap(),
            )
            .unwrap();
    }
    let (sequence, asset, locator) = gui::effect_sequence(author, render);
    let resolver = IsolatedAudioProcessorResolver::new(catalog);
    // The official reference declares a fixed editor viewport.
    gui::exercise_gui_runtime(&sequence, asset, &resolver, render, locator, gain, false);
}
