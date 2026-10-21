//! Stopped external VST3 observations through the existing SDK loader.
//! The original inventory gate does not capture state. The separate bounded
//! capture gate supplies no restore blob and processes no PCM. Neither gate
//! qualifies Product admission or the plug-in's audio behavior.
#![cfg(target_os = "windows")]

use std::path::PathBuf;
use vst3_host::audio::{AudioBusQueryFrame, AudioBusRole, MAX_OBSERVED_AUDIO_BUSES};
use vst3_host::host::PluginLoadPreparation;
use vst3_host::plugin::ProcessMode;
use vst3_host::{PluginInterface, PluginInterfaceSupport, Vst3Host};

fn load_external_stopped_plugin() -> (Vst3Host, vst3_host::Plugin, String) {
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

    (host, plugin, class_id)
}

#[test]
#[ignore = "requires an explicit external VST3 DLL and class ID in the native Job"]
fn external_vst3_stopped_bounded_audio_bus_inventory() {
    let (host, plugin, class_id) = load_external_stopped_plugin();
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

#[test]
#[ignore = "requires an explicit external VST3 DLL/class and the original 10s Job with 2s cleanup"]
fn external_vst3_stopped_bounded_default_state_capture() {
    use std::io::Write as _;

    const STATE_LIMIT: usize = 128 * 1024;
    const BUFFER_GRANT: usize = 3 * STATE_LIMIT;
    const HEADER_BYTES: usize = 28;
    // The grant covers capture buffers, not Eager loading or the native heap.
    // Only the caller may authorize a raw output path. No default path or
    // directory is created, and an existing artifact is never overwritten.
    let output = std::env::var_os("MONDRIAN_VST3_INVENTORY_STATE_OUTPUT").map(PathBuf::from);
    if let Some(path) = &output {
        assert!(
            path.is_absolute(),
            "explicit absolute state output required"
        );
        assert!(path.file_name().is_some(), "state output must name a file");
        assert!(
            path.parent().is_some_and(|parent| parent.is_dir()),
            "state output parent must already exist"
        );
        assert!(!path.exists(), "state output must be fresh");
    }
    let (host, mut plugin, class_id) = load_external_stopped_plugin();
    // This is the Eager/stopped SDK phase, before Product's complete bus and
    // parameter admission. Bounded streams expose IBStream only; Product's
    // legacy save_state stream additionally supplies Project attributes.
    let captured = plugin
        .save_state_bounded(STATE_LIMIT, BUFFER_GRANT)
        .expect("actual stopped bounded native state capture; pending input is not bypassed");
    assert!(!plugin.is_processing(), "capture must not start processing");
    let receipt = captured.buffer_receipt();
    let bytes = captured.bytes();
    assert!(
        (HEADER_BYTES..=STATE_LIMIT).contains(&bytes.len()),
        "complete canonical state extent"
    );
    assert_eq!(&bytes[..16], b"VST3HOST_STATE\0\0");
    let word = |offset: usize| {
        u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("bounded state header"),
        )
    };
    assert_eq!(
        word(16),
        1,
        "canonical component/controller envelope version"
    );
    let component_bytes = word(20) as usize;
    let controller_bytes = match word(24) {
        u32::MAX => None,
        length => Some(length as usize),
    };
    assert_eq!(
        HEADER_BYTES
            .checked_add(component_bytes)
            .and_then(|n| n.checked_add(controller_bytes.unwrap_or(0))),
        Some(bytes.len()),
        "complete native state payload, without truncation"
    );
    assert!(receipt.envelope_capacity_bytes >= bytes.len());
    let stream_extent = STATE_LIMIT - HEADER_BYTES;
    assert!(
        receipt.stream_payload_bytes == stream_extent
            || receipt.stream_payload_bytes == 2 * stream_extent,
        "one component stream and at most one separate controller stream"
    );
    let peak = receipt
        .peak_buffer_bytes()
        .expect("checked capture buffer receipt");
    assert!(
        peak <= BUFFER_GRANT,
        "capture buffers remain within the original SDK grant"
    );
    let bytes = captured.into_bytes();
    let raw_bytes = bytes.len();
    // No native owner remains while writing the optional bounded raw artifact
    // or publishing the successful observation. All failures above stay FAIL.
    drop(plugin);
    drop(host);
    if let Some(path) = &output {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("fresh explicitly authorized state output");
        file.write_all(&bytes)
            .expect("write the exact bounded raw state");
        file.flush().expect("complete bounded state output");
        assert_eq!(
            file.metadata().expect("state output metadata").len(),
            raw_bytes as u64
        );
    }
    let report = serde_json::json!({
        "scope": "sdk_stopped_bounded_default_state_capture",
        "class_id": class_id,
        "phase": "after_eager_load_and_stopped_offline_setup",
        "sample_rate": 48000,
        "max_block_frames": 256,
        "process_mode": "offline",
        "stream_attributes": "unavailable",
        "raw_state_bytes": raw_bytes,
        "component_bytes": component_bytes,
        "controller_bytes": controller_bytes,
        "state_limit_bytes": STATE_LIMIT,
        "stream_payload_allocation_bytes": receipt.stream_payload_bytes,
        "envelope_capacity_bytes": receipt.envelope_capacity_bytes,
        "peak_capture_buffer_bytes": peak,
        "capture_buffer_grant_bytes": BUFFER_GRANT,
        "raw_output_written": output.is_some(),
    });
    println!("{report}");
}
