//! Stopped external VST3 audio-bus observation through the existing SDK loader.
//! This ignored Windows gate supplies no restore blob, requests no state capture,
//! and processes no PCM.
//! It does not qualify Product admission or the plug-in's audio behavior.
#![cfg(target_os = "windows")]

use std::path::PathBuf;
use vst3_host::audio::{AudioBusQueryFrame, AudioBusRole, MAX_OBSERVED_AUDIO_BUSES};
use vst3_host::host::PluginLoadPreparation;
use vst3_host::plugin::ProcessMode;
use vst3_host::{PluginInterface, PluginInterfaceSupport, Vst3Host};

#[test]
#[ignore = "requires an explicit external VST3 DLL and class ID in the native Job"]
fn external_vst3_stopped_bounded_audio_bus_inventory() {
    let path = PathBuf::from(
        std::env::var_os("MONDRIAN_VST3_INVENTORY_PLUGIN")
            .expect("set MONDRIAN_VST3_INVENTORY_PLUGIN to an existing absolute VST3 DLL"),
    );
    assert!(
        path.is_absolute() && path.is_file(),
        "external VST3 DLL must exist"
    );
    let class_id = std::env::var("MONDRIAN_VST3_INVENTORY_CLASS_ID")
        .expect("set MONDRIAN_VST3_INVENTORY_CLASS_ID to the exact native class ID")
        .to_ascii_uppercase();
    assert!(
        class_id.len() == 32 && class_id.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "external class ID must contain exactly 32 hexadecimal digits"
    );

    // Match the Windows Product cold probe's interface/profile and no-state
    // sequence, without importing Audio/Timeline or applying its shape policy.
    let interfaces = PluginInterfaceSupport::none()
        .with(PluginInterface::Component)
        .with(PluginInterface::AudioProcessor)
        .with(PluginInterface::EditController)
        .with(PluginInterface::ConnectionPoint)
        .with(PluginInterface::ProcessContextRequirements)
        .with(PluginInterface::PlugView)
        .with(PluginInterface::PlugViewContentScaleSupport)
        .with(PluginInterface::DataExchangeReceiver);
    let mut host = Vst3Host::builder()
        .plugin_interfaces(interfaces)
        .initial_process_mode(ProcessMode::Offline)
        .sample_rate(48_000.0)
        .block_size(256)
        .input_channels(2)
        .output_channels(2)
        .load_preparation(PluginLoadPreparation::Eager)
        .with_process_isolation(false)
        .build()
        .expect("SDK host matching the Product cold probe profile");
    // Eager SDK load itself activates declared default buses, negotiates their
    // advertised arrangements, calls setupProcessing and attempts component
    // activation. A separate controller may receive initial component-state
    // synchronization too. Stopped does not mean inactive or no native actions.
    let mut plugin = host
        .load_plugin_class(&path, &class_id)
        .expect("load exact external class");
    assert_eq!(
        plugin.info().uid,
        class_id,
        "a replacement class is not the selected class"
    );
    // The Product probe also repeats this setter after load. It reruns setup
    // while stopped, including component deactivation/reactivation when needed.
    // No state restore, explicit bus negotiation/activation, start or DSP follows.
    plugin
        .set_process_mode(ProcessMode::Offline)
        .expect("same stopped Offline setup as probe");
    assert_eq!(plugin.sample_rate(), 48_000.0);
    assert_eq!(plugin.block_size(), 256);
    assert!(!plugin.is_processing());

    let mut frame = AudioBusQueryFrame::default();
    let observed = plugin
        .observe_audio_buses_bounded(&mut frame)
        .expect("complete stopped metadata/layout/arrangement observation");
    let directions = [
        (
            "input",
            observed.native_inputs,
            observed.layout_inputs,
            observed.input_arrangements,
        ),
        (
            "output",
            observed.native_outputs,
            observed.layout_outputs,
            observed.output_arrangements,
        ),
    ];
    // Validate both complete extents before materializing or publishing any row.
    // The SDK bounds each stage to 32; matching extents preserves native indexes.
    // These census limits do not bound eager loading, startup or the plug-in heap.
    for (direction, native, prepared, arrangements) in directions {
        assert!(
            native.len() <= MAX_OBSERVED_AUDIO_BUSES,
            "{direction} native bound"
        );
        assert_eq!(
            native.len(),
            prepared.len(),
            "{direction} native/prepared extent"
        );
        assert_eq!(
            native.len(),
            arrangements.len(),
            "{direction} native/arrangement extent"
        );
    }
    let mut rows = Vec::with_capacity(observed.native_inputs.len() + observed.native_outputs.len());
    for (direction, native, prepared, arrangements) in directions {
        for (index, ((native, prepared), arrangement)) in
            native.iter().zip(prepared).zip(arrangements).enumerate()
        {
            rows.push(serde_json::json!({
                "direction": direction,
                "index": index,
                "native_role": match native.role {
                    AudioBusRole::Main => "main",
                    AudioBusRole::Auxiliary => "auxiliary",
                },
                "native_channels": native.channel_count,
                "prepared_channels": prepared.channel_count,
                "host_active": prepared.active,
                "raw_arrangement": arrangement.raw(),
            }));
        }
    }
    assert!(!plugin.is_processing());
    let report = serde_json::json!({
        "scope": "sdk_stopped_audio_bus_inventory",
        "class_id": class_id,
        "phase": "after_eager_load_and_stopped_offline_setup",
        "sample_rate": 48000,
        "requested_input_channels": 2,
        "requested_output_channels": 2,
        "max_block_frames": 256,
        "process_mode": "offline",
        "state": null,
        "input_count": observed.native_inputs.len(),
        "output_count": observed.native_outputs.len(),
        // Each fresh arrangement is published only after kResultOk. A failed
        // query rejects the complete census before this report can be emitted.
        "arrangement_query_status": "checked_success",
        "buses": rows,
    });
    // All native owners are destroyed on this same test thread before stdout.
    // host_active above is the retained host ACK, not a native GetActive query.
    drop(plugin);
    drop(host);
    println!("{report}");
}
