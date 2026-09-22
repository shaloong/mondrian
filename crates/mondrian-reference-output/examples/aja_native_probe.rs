//! Read-only packaged constructor and real AJA NTV2 discovery. Never opens output.
#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use mondrian_reference_output::{
        AjaReferenceOutputAdapter, ReferenceOutputAdapter, ReferenceOutputAdapterError,
    };
    let mut adapter = match AjaReferenceOutputAdapter::load_packaged() {
        Ok(adapter) => adapter,
        Err(ReferenceOutputAdapterError::DriverMissing { detail }) => {
            println!(
                "{}",
                serde_json::json!({
                    "schema_version": 1, "qualifying": false,
                    "constructor": "AjaReferenceOutputAdapter::load_packaged",
                    "discovery_status": "NativeBridgeOrDriverMissing", "devices": 0,
                    "detail": detail, "provider_evidence": null,
                    "physical_output": "NotRun",
                })
            );
            return Ok(());
        }
        Err(ReferenceOutputAdapterError::VersionMismatch { detail }) => {
            println!(
                "{}",
                serde_json::json!({
                    "schema_version": 1, "qualifying": false,
                    "constructor": "AjaReferenceOutputAdapter::load_packaged",
                    "discovery_status": "DriverInterfaceMismatch", "devices": 0,
                    "detail": detail, "provider_evidence": null,
                    "physical_output": "NotRun",
                })
            );
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let (status, devices, detail) = match adapter.discover() {
        Ok(devices) => ("DevicesDiscovered", devices.len(), None),
        Err(ReferenceOutputAdapterError::DriverMissing { detail }) => {
            ("NativeBridgeOrDriverMissing", 0, Some(detail))
        }
        Err(ReferenceOutputAdapterError::VersionMismatch { detail }) => {
            ("DriverInterfaceMismatch", 0, Some(detail))
        }
        Err(ReferenceOutputAdapterError::NoDevices) => ("NoAjaDevices", 0, None),
        Err(error) => return Err(error.into()),
    };
    println!(
        "{}",
        serde_json::json!({
            "schema_version": 1, "qualifying": false,
            "constructor": "AjaReferenceOutputAdapter::load_packaged",
            "discovery_status": status, "devices": devices, "detail": detail,
            "provider_evidence": adapter.evidence(), "physical_output": "NotRun",
        })
    );
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    println!(
        "{{\"qualifying\":false,\"physical_output\":\"NotRun\",\"reason\":\"WindowsNativeBridgeRequired\"}}"
    );
}
