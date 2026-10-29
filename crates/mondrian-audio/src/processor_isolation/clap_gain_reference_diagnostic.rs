//! Test-only direct SDK control, independent of Worker, automation lowering and DAG.
use super::super::clap_worker::{ClapHost, ClapHostShared};
use super::clap_deactivated_parameter_diagnostic::{
    lease_binding, no_reparse, require_actual_job, Binding,
};
use super::*;
use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::audio_ports_config::{AudioPortsConfigBuffer, PluginAudioPortsConfig};
use clack_host::events::event_types::ParamValueEvent;
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::events::Pckn;
use clack_host::prelude::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel, PluginAudioConfiguration,
    PluginAudioProcessor,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferenceCase {
    schema_version: u32,
    plugin: Binding,
    plugin_id: String,
    flat_state: Binding,
    gain_state: Binding,
    output: PathBuf,
}

#[derive(Clone, Copy)]
enum ParameterPhase {
    ProcessEveryBlock,
    ProcessOnce,
    InactiveMainFlush,
}

impl ParameterPhase {
    fn name(self) -> &'static str {
        match self {
            Self::ProcessEveryBlock => "process",
            Self::ProcessOnce => "process_once",
            Self::InactiveMainFlush => "inactive_flush",
        }
    }
}

fn direct_pcm(
    entry: &PluginEntry,
    plugin_id: &str,
    seed: &[u8],
    native_db: f64,
    host_db: Option<f64>,
    phase: ParameterPhase,
) -> Vec<f32> {
    let shared = ClapHostShared::new();
    let native_shared = shared.clone().with_native_logs().unwrap();
    let info = HostInfo::new(
        "Mondrian",
        "Mondrian",
        "https://github.com/shaloong/mondrian",
        env!("CARGO_PKG_VERSION"),
    )
    .unwrap();
    let id = CString::new(plugin_id).unwrap();
    let mut instance = PluginInstance::<ClapHost>::new(
        |_| native_shared,
        |shared| super::super::clap_main_services::ClapHostMainThread::new(shared),
        entry,
        &id,
        &info,
    )
    .unwrap();
    shared.service_callbacks(&mut instance).unwrap();
    // This control chooses the actual native Mono preset directly. It does not
    // invoke Mondrian's cold negotiation or use authored port metadata.
    {
        let handle = instance.inactive_plugin_handle().unwrap();
        let configs = handle.get_extension::<PluginAudioPortsConfig>().unwrap();
        let count = configs.count(&handle);
        assert!(count <= 128);
        let mut mono = None;
        for index in 0..count {
            let mut buffer = AudioPortsConfigBuffer::new();
            let config = configs.get(&handle, index, &mut buffer).unwrap();
            if config.input_port_count == 1
                && config.output_port_count == 1
                && config.main_input.is_some_and(|p| p.channel_count == 1)
                && config.main_output.is_some_and(|p| p.channel_count == 1)
            {
                assert!(mono.replace(config.id).is_none());
            }
        }
        configs.select(&handle, mono.expect("one native Mono preset")).unwrap();
    }
    {
        let handle = instance.plugin_handle();
        let mut reader = seed;
        handle
            .get_extension::<PluginState>()
            .unwrap()
            .load(&handle, &mut reader)
            .unwrap();
    }
    shared.service_callbacks(&mut instance).unwrap();
    configure_render_mode(&instance.plugin_handle(), AudioProcessingMode::Realtime).unwrap();
    {
        let handle = instance.plugin_handle();
        let ports = handle.get_extension::<PluginAudioPorts>().unwrap();
        for is_input in [true, false] {
            assert_eq!(ports.count(&handle, is_input), 1);
            let mut buffer = AudioPortInfoBuffer::new();
            let port = ports.get(&handle, 0, is_input, &mut buffer).unwrap();
            assert_eq!(port.channel_count, 1);
            assert!(port.flags.contains(AudioPortFlags::IS_MAIN));
        }
        let params = handle.get_extension::<PluginParams>().unwrap();
        assert_eq!(
            params.get_value(&handle, ClapId::new(7)).unwrap().to_bits(),
            native_db.to_bits()
        );
        assert_eq!(
            params.get_value(&handle, ClapId::new(8)).unwrap().to_bits(),
            0.0_f64.to_bits()
        );
    }
    let events = [ParamValueEvent::new(
        0,
        ClapId::new(7),
        Pckn::match_all(),
        host_db.unwrap_or(native_db),
    )];
    if matches!(phase, ParameterPhase::InactiveMainFlush) && host_db.is_some() {
        let mut handle = instance.inactive_plugin_handle().unwrap();
        handle.get_extension::<PluginParams>().unwrap().flush(
            &mut handle,
            &InputEvents::from_buffer(&events),
            &mut OutputEvents::void(),
        );
    }
    let mut processor: PluginAudioProcessor<ClapHost> = instance
        .activate(
            |_, _| (),
            PluginAudioConfiguration {
                sample_rate: 48_000.0,
                min_frames_count: 1,
                max_frames_count: 4,
            },
        )
        .unwrap()
        .into();
    instance.access_shared_handler(|host| {
        host.services.active.store(true, std::sync::atomic::Ordering::Release)
    });
    let mut input_ports = AudioPorts::with_capacity(1, 1);
    let mut output_ports = AudioPorts::with_capacity(1, 1);
    let mut input = [0.25_f32; 4];
    let mut output = [f32::NAN; 4];
    let mut output_events = EventBuffer::with_capacity(64);
    let mut pcm = Vec::with_capacity(4096);
    for block in 0..1024 {
        output.fill(f32::NAN);
        output_events.clear();
        let input_audio = input_ports.with_input_buffers([AudioPortBuffer {
            latency: 0,
            channels: AudioPortBufferType::f32_input_only([InputChannel::variable(&mut input)]),
        }]);
        let mut output_audio = output_ports.with_output_buffers([AudioPortBuffer {
            latency: 0,
            channels: AudioPortBufferType::f32_output_only([&mut output[..]]),
        }]);
        let send = match phase {
            ParameterPhase::ProcessEveryBlock => true,
            ParameterPhase::ProcessOnce => block == 0,
            ParameterPhase::InactiveMainFlush => false,
        };
        let input_events = if host_db.is_some() && send {
            InputEvents::from_buffer(&events)
        } else {
            InputEvents::empty()
        };
        {
            let _audio_role = shared.enter_audio_thread();
            processor
                .ensure_processing_started()
                .unwrap()
                .process(
                    &input_audio,
                    &mut output_audio,
                    &input_events,
                    &mut output_events.as_output(),
                    None,
                    None,
                )
                .unwrap();
        }
        drop(output_audio);
        drop(input_audio);
        assert!(output_events.len() <= 64);
        assert!(output.iter().all(|v| v.is_finite()));
        pcm.extend_from_slice(&output);
    }
    if let Some(target) = host_db {
        let handle = instance.plugin_handle();
        assert_eq!(
            handle
                .get_extension::<PluginParams>()
                .unwrap()
                .get_value(&handle, ClapId::new(7))
                .unwrap()
                .to_bits(),
            target.to_bits()
        );
    } else {
        let expected = 0.25 * 10.0_f64.powf(native_db / 20.0);
        assert!(pcm
            .iter()
            .all(|v| (f64::from(*v) - expected).abs() <= 2.0 * f64::from(f32::EPSILON)));
    }
    {
        let _audio_role = shared.enter_audio_thread();
        processor.ensure_processing_stopped();
    }
    drop(processor);
    instance.access_shared_handler(|host| {
        host.services.active.store(false, std::sync::atomic::Ordering::Release)
    });
    instance.try_deactivate().unwrap();
    drop(instance);
    pcm
}

#[test]
#[ignore = "actual Ring Out DLL/states and original native Job; direct SDK reference only"]
fn native_clap_ring_out_direct_sdk_gain_reference() {
    require_actual_job();
    let path = PathBuf::from(std::env::var_os("MONDRIAN_CLAP_GAIN_REFERENCE_CASE").unwrap());
    no_reparse(&path);
    assert!(fs::metadata(&path).unwrap().len() <= 64 * 1024);
    let case: ReferenceCase = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(case.schema_version, 1);
    assert_eq!(case.plugin_id, "com.duskaudio.ring-out");
    no_reparse(case.output.parent().unwrap());
    assert!(!case.output.exists());
    let plugin_lease = lease_binding(&case.plugin, MAX_CLAP_BINARY_BYTES);
    // SAFETY: exact read-only DLL lease spans the entry and every native instance.
    // The original Job contains native faults; all main/audio roles are exclusive.
    let entry = unsafe { PluginEntry::load(case.plugin.path.as_os_str()) }.unwrap();
    let mut rows = Vec::new();
    for (binding, native_db) in [(&case.flat_state, 0.0_f64), (&case.gain_state, -6.0_f64)] {
        let mut seed_lease = lease_binding(binding, 64 * 1024);
        let mut seed = Vec::new();
        seed_lease.read_to_end(&mut seed).unwrap();
        for (host, phase) in [
            (None, ParameterPhase::ProcessEveryBlock),
            (Some(-3.0), ParameterPhase::ProcessEveryBlock),
            (Some(-3.0), ParameterPhase::ProcessOnce),
            (Some(-3.0), ParameterPhase::InactiveMainFlush),
        ] {
            let pcm = direct_pcm(&entry, &case.plugin_id, &seed, native_db, host, phase);
            rows.push(serde_json::json!({"state_bytes":seed,"native_gain_db":native_db,
                "host_override_db":host,"phase":phase.name(),"pcm":pcm[..32],
                "observed_frames":pcm.len(),"tail_pcm":pcm[pcm.len()-32..],
                "range":[pcm.iter().copied().fold(f32::INFINITY,f32::min),pcm.iter().copied().fold(f32::NEG_INFINITY,f32::max)]}));
        }
    }
    drop(entry);
    drop(plugin_lease);
    let reference = serde_json::json!({"schema_version":1,"producer":"direct CLAP SDK; no Worker/Runtime/DAG",
        "plugin_id":case.plugin_id,"plugin_sha256":case.plugin.sha256,"sample_rate":48000,
        "channel_layout":"Mono","block_frames":4,"frame_count":32,"rows":rows});
    let bytes = serde_json::to_vec(&reference).unwrap();
    assert!(bytes.len() <= 64 * 1024);
    let mut file = fs::OpenOptions::new().create_new(true).write(true).open(case.output).unwrap();
    file.write_all(&bytes).unwrap();
    file.sync_all().unwrap();
}
