//! Diagnostic fixture for exercising the native bootstrap without linking FFmpeg.
//! This executable is not a campaign provider and cannot produce a qualifying run.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(not(windows))]
    return Err("Windows native bootstrap fixture only".into());
    #[cfg(windows)]
    {
        use mondrian_validation_launcher::{AttestationExpectation, FileBinding};
        use sha2::{Digest, Sha256};
        use std::io::{Read, Write};
        let path = std::env::args_os().nth(1).ok_or("missing fixture request")?;
        if path == "--sleep" {
            std::thread::sleep(std::time::Duration::from_secs(60));
            return Ok(());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&path)?.take(1_048_577).read_to_end(&mut bytes)?;
        if bytes.len() > 1_048_576 {
            return Err("fixture request too large".into());
        }
        let request: serde_json::Value = serde_json::from_slice(&bytes)?;
        if request["fixture_mode"] == "refuse" {
            return Err("fixture refuses bootstrap".into());
        }
        let machine: FileBinding = serde_json::from_value(request["machine_plan"].clone())?;
        let machine_json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&machine.path)?)?;
        let launcher: FileBinding =
            serde_json::from_value(machine_json["verifier_tools"]["preloader"].clone())?;
        let runtime_files = serde_json::from_value::<Vec<FileBinding>>(
            machine_json["verifier_tools"]["runtime_files"].clone(),
        )?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let authority = mondrian_validation_launcher::attest(AttestationExpectation {
            launcher: &launcher,
            application_sha256: request["identity"]["runtime_image_sha256"]
                .as_str()
                .ok_or("missing image identity")?,
            request_sha256: &digest,
            machine_plan_sha256: &machine.sha256,
            runtime_files: &runtime_files,
        })?
        .ok_or("native bootstrap absent")?;
        if request["fixture_mode"] == "descendant" {
            use std::os::windows::process::CommandExt;
            let _child = std::process::Command::new(std::env::current_exe()?)
                .creation_flags(0x0800_0000)
                .arg("--sleep")
                .spawn()?;
            // Deliberately leave this native child for the outer Job owner to detect.
        }
        let output = request["output_manifest_path"].as_str().ok_or("missing fixture output")?;
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(output)?;
        file.write_all(&serde_json::to_vec(&serde_json::json!({ "qualifying": false, "scope": "native-bootstrap-fixture", "attestation": authority.receipt() }))?)?;
        file.sync_all()?;
        Ok(())
    }
}
