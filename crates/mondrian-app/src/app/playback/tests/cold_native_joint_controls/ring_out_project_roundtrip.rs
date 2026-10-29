//! Real native Mono state survives public Project reopen and Stereo delivery.
use super::native_gain_project::{self, ProjectCase};
use super::*;
use mondrian_audio::{AudioProcessingMode, AudioRenderContract, ClapMainAudioInput};
use mondrian_core::ParameterId;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeReference {
    schema_version: u32,
    producer: String,
    plugin_id: String,
    plugin_sha256: String,
    sample_rate: u32,
    channel_layout: String,
    block_frames: usize,
    frame_count: usize,
    rows: Vec<ReferenceRow>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferenceRow {
    state_bytes: Vec<u8>,
    native_gain_db: f64,
    host_override_db: Option<f64>,
    phase: String,
    pcm: Vec<f32>,
    observed_frames: usize,
    tail_pcm: Vec<f32>,
    range: [f32; 2],
}

fn matching_native_reference(
    seed: &[u8],
    native_gain: f64,
    host_override: Option<f64>,
    instance: &mondrian_timeline::audio::AudioProcessorInstance,
    config: mondrian_media::AudioPlaybackConfig,
) -> Vec<f32> {
    let path = std::path::PathBuf::from(
        std::env::var_os("MONDRIAN_RING_OUT_PROJECT_REFERENCE")
            .expect("current direct SDK reference"),
    );
    assert!(std::fs::metadata(&path).unwrap().len() <= 64 * 1024);
    let reference: NativeReference = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(reference.schema_version, 1);
    assert_eq!(reference.producer, "direct CLAP SDK; no Worker/Runtime/DAG");
    let mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap {
        plugin_id,
        binary_sha256,
        ..
    } = &instance.definition
    else {
        panic!("actual CLAP instance")
    };
    assert_eq!(&reference.plugin_id, plugin_id);
    let binary_hex = binary_sha256
        .as_ref()
        .expect("installed native binary fingerprint")
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(reference.plugin_sha256, binary_hex);
    assert_eq!(reference.sample_rate, config.sample_rate);
    assert_eq!(reference.channel_layout, "Mono");
    assert_eq!(reference.block_frames, config.chunk_frames);
    assert_eq!(
        reference.frame_count,
        usize::try_from(super::project_state_roundtrip::GAIN_SOURCE_FRAMES).unwrap()
    );
    assert!(reference.rows.len() <= 16);
    // One constant author curve has its initial point at sample0. Native-owned
    // cases still use independent gain math; this is the same DLL's response
    // to a changed target, including its own processing transition.
    let phase = if host_override.is_some() {
        "process_once"
    } else {
        "process"
    };
    let mut matches = reference.rows.into_iter().filter(|row| {
        row.state_bytes == seed
            && row.native_gain_db.to_bits() == native_gain.to_bits()
            && row.host_override_db.map(f64::to_bits) == host_override.map(f64::to_bits)
            && row.phase == phase
    });
    let row = matches.next().expect("exact native state/parameter/phase reference");
    assert!(matches.next().is_none());
    assert_eq!(row.pcm.len(), reference.frame_count);
    assert!(row.observed_frames >= row.pcm.len());
    assert_eq!(row.tail_pcm.len(), row.pcm.len().min(row.observed_frames));
    assert!(row.range.iter().all(|v| v.is_finite()) && row.range[0] <= row.range[1]);
    assert!(row
        .pcm
        .iter()
        .chain(&row.tail_pcm)
        .all(|v| v.is_finite() && *v >= row.range[0] && *v <= row.range[1]));
    row.pcm
}

#[test]
#[ignore = "real Ring Out captured states, matching normal Product; public Mono Project and same-source software Stereo PCM"]
fn app_ring_out_mono_project_reload_preserves_config_state_and_host_authority() {
    for (state_env, native_gain) in [
        ("MONDRIAN_RING_OUT_FLAT_STATE", 0.0_f64),
        ("MONDRIAN_RING_OUT_GAIN_STATE", -6.0_f64),
    ] {
        for host_override in [None, Some(-3.0)] {
            project_roundtrip(state_env, native_gain, host_override);
        }
    }
}

fn project_roundtrip(state_env: &str, native_gain: f64, host_override: Option<f64>) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let path =
        |key| std::path::PathBuf::from(std::env::var_os(key).expect("real Mono state input"));
    let helper = path("MONDRIAN_CLAP_TEST_HELPER");
    let seed = std::fs::read(path(state_env)).unwrap();
    assert!(!seed.is_empty() && seed.len() <= 64 * 1024);
    let clap = Arc::new(
        mondrian_audio::InstalledClapAudioProcessorSpecResolver::new(helper.clone()).unwrap(),
    );
    let vst3 = Arc::new(
        mondrian_audio::InstalledVst3AudioProcessorSpecResolver::new(helper.clone()).unwrap(),
    );
    const PLUGIN: &str = "com.duskaudio.ring-out";
    let plugin = path("MONDRIAN_RING_OUT_TEST_PLUGIN");
    assert!(clap
        .install_library(plugin.clone())
        .unwrap()
        .iter()
        .any(|d| d.plugin_id == PLUGIN));
    // A Stereo software device does not define the plugin's Program layout.
    let config = mondrian_media::AudioPlaybackConfig {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        chunk_frames: 4,
        preroll_frames: 4,
        high_watermark_frames: 8,
        max_in_flight: 1,
        ..mondrian_media::AudioPlaybackConfig::product_default()
    };
    let contract = AudioRenderContract {
        sample_rate: config.sample_rate,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: config.chunk_frames,
        processing_mode: AudioProcessingMode::Realtime,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    };
    let registration = mondrian_audio::probe_clap_plugin_registration(
        &helper,
        &plugin,
        PLUGIN,
        contract,
        Some(&seed),
    )
    .unwrap();
    assert_eq!(registration.main_audio_input, ClapMainAudioInput::Present);
    assert!(registration.auxiliary_inputs.buses.is_empty());
    assert!(registration.auxiliary_outputs.buses.is_empty());
    let mut instance = clap.create_instance(PLUGIN, contract, Some(seed.clone())).unwrap();
    assert!(!instance.bypassed);
    assert_eq!(
        instance.opaque_state.as_ref().unwrap().as_slice(),
        seed.as_slice()
    );
    assert!(instance.parameters.values().all(|p| p.automation.is_none()));
    let gain_id = ParameterId::new("clap.param.7").unwrap();
    assert!(instance.parameters.contains_key(&gain_id));
    if let Some(value) = host_override {
        instance
            .parameters
            .get_mut(&gain_id)
            .unwrap()
            .set_automation(
                mondrian_core::ExactAutomationCurve::new(gain_id.clone(), value).unwrap(),
            )
            .unwrap();
    }
    let reference = matching_native_reference(&seed, native_gain, host_override, &instance, config);
    let control = prepare_app_native_control_with_instance(
        clap.clone(),
        vst3.clone(),
        config,
        instance.clone(),
        |sequence, library, root| {
            sequence.settings.frame_rate = mondrian_core::Rational::FPS_24;
            sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
            super::project_state_roundtrip::initialize_nonzero_audio(sequence, library, root);
        },
    );
    native_gain_project::run(
        ProjectCase { clap, vst3, config, instance, deadline },
        control,
        |state, weak, id| {
            native_gain_project::observe_cold_values(
                state,
                weak,
                id,
                registration.parameters.len(),
                &[
                    (gain_id.clone(), native_gain),
                    (ParameterId::new("clap.param.8").unwrap(), 0.0),
                ],
            )
        },
        // Standard Mono->Stereo delivery duplicates the Program at unity.
        // Compare every sample from0, with no mixer call or transition skipping.
        |frame, _| {
            if host_override.is_some() {
                f64::from(reference[frame])
            } else {
                0.25_f64 * 10.0_f64.powf(native_gain / 20.0)
            }
        },
    );
}
