//! Read-only packaged constructor and real COM discovery. Never opens output.
#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use mondrian_reference_output::{
        DeckLinkReferenceOutputAdapter, ReferenceOutputAdapter, ReferenceOutputAdapterError,
    };
    let mut adapter = match DeckLinkReferenceOutputAdapter::load_packaged() {
        Ok(adapter) => adapter,
        Err(ReferenceOutputAdapterError::DriverMissing { detail }) => {
            println!(
                "{}",
                serde_json::json!({
                    "schema_version": 1, "qualifying": false,
                    "constructor": "DeckLinkReferenceOutputAdapter::load_packaged",
                    "discovery_status": "DesktopVideoDriverMissing", "devices": 0,
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
                    "constructor": "DeckLinkReferenceOutputAdapter::load_packaged",
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
            ("DesktopVideoDriverMissing", 0, Some(detail))
        }
        Err(ReferenceOutputAdapterError::VersionMismatch { detail }) => {
            ("DriverInterfaceMismatch", 0, Some(detail))
        }
        Err(ReferenceOutputAdapterError::NoDevices) => ("NoDeckLinkDevices", 0, None),
        Err(error) => return Err(error.into()),
    };
    println!(
        "{}",
        serde_json::json!({
            "schema_version": 1, "qualifying": false,
            "constructor": "DeckLinkReferenceOutputAdapter::load_packaged",
            "discovery_status": status, "devices": devices, "detail": detail,
            "provider_evidence": adapter.evidence(), "physical_output": "NotRun",
        })
    );
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    println!(
        "{{\"qualifying\":false,\"physical_output\":\"NotRun\",\"reason\":\"WindowsCOMRequired\"}}"
    );
}
