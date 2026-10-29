//! Real nondefault opaque state through public Project save/reopen and the
//! same App-created Timeline/Delivery source, with native-owned values and a
//! conflicting authored constant. No plugin GUI or alternate DSP executor.
use super::native_gain_project::{self, ProjectCase};
use super::project_state_roundtrip::initialize_nonzero_audio;
use super::*;
use mondrian_core::ParameterId;

const PLUGIN: &str = "com.zamaudio.ZamCompX2";

fn observe_restored_cold_state(
    state: &mut AppState,
    weak: &std::sync::Weak<TimelineAudioPcmRenderer>,
    instance: mondrian_core::AudioProcessorInstanceId,
) {
    native_gain_project::observe_cold_values(
        state,
        weak,
        instance,
        11,
        &[(ParameterId::new("clap.param.5").unwrap(), 12.0)],
    );
}

#[test]
#[ignore = "official Zam4.5 CLAP, actual captured Makeup12dB state and matching normal Product; public Project and same-source software PCM"]
fn app_zam_clap_nondefault_opaque_project_reload_preserves_native_and_host_authority() {
    for host_override in [None, Some(6.0)] {
        project_roundtrip(host_override);
    }
}

fn project_roundtrip(host_override: Option<f64>) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let path = |key| std::path::PathBuf::from(std::env::var_os(key).expect("native test input"));
    let helper = path("MONDRIAN_CLAP_TEST_HELPER");
    let seed = std::fs::read(path("MONDRIAN_ZAM_NONDEFAULT_STATE")).unwrap();
    assert!(!seed.is_empty() && seed.len() <= 64 * 1024);
    let clap = Arc::new(
        mondrian_audio::InstalledClapAudioProcessorSpecResolver::new(helper.clone()).unwrap(),
    );
    let vst3 =
        Arc::new(mondrian_audio::InstalledVst3AudioProcessorSpecResolver::new(helper).unwrap());
    let installed = clap.install_library(path("MONDRIAN_CLAP_MIXED_VENDOR_TEST_PLUGIN")).unwrap();
    assert!(installed.iter().any(|descriptor| descriptor.plugin_id == PLUGIN));
    let config = mondrian_media::AudioPlaybackConfig {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        chunk_frames: 4,
        preroll_frames: 4,
        high_watermark_frames: 8,
        max_in_flight: 1,
        ..mondrian_media::AudioPlaybackConfig::product_default()
    };
    let contract = mondrian_audio::AudioRenderContract {
        sample_rate: config.sample_rate,
        channel_layout: config.channel_layout,
        max_block_frames: config.chunk_frames,
        processing_mode: mondrian_audio::AudioProcessingMode::Realtime,
        processor_session_scratch_budget_bytes:
            mondrian_audio::AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            mondrian_audio::AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            mondrian_audio::AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    };
    let factory = clap.create_instance(PLUGIN, contract, None).unwrap();
    assert_ne!(
        factory.opaque_state.as_ref().map(|state| state.as_slice()),
        Some(seed.as_slice()),
        "actual nondefault discriminator"
    );
    let mut instance = clap.create_instance(PLUGIN, contract, Some(seed.clone())).unwrap();
    assert_eq!(
        instance.opaque_state.as_ref().map(|state| state.as_slice()),
        Some(seed.as_slice())
    );
    // Native meters9/10 are presentation only; the existing author factory
    // intentionally excludes readonly descriptors from the canonical map.
    assert_eq!(instance.parameters.len(), 9);
    for id in 0..9 {
        assert!(instance
            .parameters
            .contains_key(&ParameterId::new(format!("clap.param.{id}")).unwrap()));
    }
    assert!(instance.parameters.values().all(|parameter| parameter.automation.is_none()));
    if let Some(value) = host_override {
        let id = ParameterId::new("clap.param.5").unwrap();
        instance
            .parameters
            .get_mut(&id)
            .unwrap()
            .set_automation(mondrian_core::ExactAutomationCurve::new(id, value).unwrap())
            .unwrap();
    }
    let control = prepare_app_native_control_with_instance(
        clap.clone(),
        vst3.clone(),
        config,
        instance.clone(),
        initialize_nonzero_audio,
    );
    let gain = 10.0_f64.powf(host_override.unwrap_or(12.0) / 20.0);
    native_gain_project::run(
        ProjectCase { clap, vst3, config, instance, deadline },
        control,
        observe_restored_cold_state,
        |_, channel| {
            if channel == 0 {
                0.25 * gain
            } else {
                -0.25 * gain
            }
        },
    );
}
