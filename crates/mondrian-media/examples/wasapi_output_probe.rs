#[cfg(target_os = "windows")]
fn main() {
    use mondrian_core::AudioChannelLayout;
    use mondrian_media::{
        discover_realtime_audio_output_devices, probe_realtime_audio_output_contract,
        RealtimeAudioOutputDeviceSelection,
    };

    let catalog = discover_realtime_audio_output_devices()
        .expect("WASAPI output-device discovery must complete");
    println!(
        "host={:?} devices={}",
        catalog.host_name,
        catalog.devices.len()
    );
    for device in catalog.devices {
        println!(
            "device name={:?} default={} id_error={:?} description_error={:?}",
            device.display_name,
            device.is_system_default,
            device.device_id_error,
            device.description_error
        );
        let Some(device_id) = device.device_id else {
            println!("  status=not_run reason=device_identity_unavailable");
            continue;
        };
        let cases = [
            (
                "shared",
                RealtimeAudioOutputDeviceSelection::Specific { device_id: device_id.clone() },
            ),
            (
                "prefer_exclusive",
                RealtimeAudioOutputDeviceSelection::SpecificPreferExclusive {
                    device_id: device_id.clone(),
                },
            ),
            (
                "require_exclusive",
                RealtimeAudioOutputDeviceSelection::SpecificExclusive { device_id },
            ),
        ];
        for (policy, selection) in cases {
            match probe_realtime_audio_output_contract(
                &selection,
                48_000,
                AudioChannelLayout::Stereo,
            ) {
                Ok(evidence) => println!("  policy={policy} status=opened evidence={evidence:#?}"),
                Err(error) => println!("  policy={policy} status=rejected error={error:#?}"),
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("wasapi_output_probe is available only on Windows");
    std::process::exit(2);
}
