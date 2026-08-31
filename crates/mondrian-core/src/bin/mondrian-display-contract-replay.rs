use anyhow::{bail, Context, Result};
use mondrian_core::display_contract::{
    DisplayOutputSnapshot, DisplayValidationStatus, HdrStatus, MonitorProfileStatus,
};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

const MAXIMUM_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAXIMUM_CONTRACTS: usize = 4_096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayInput {
    schema_version: u32,
    contracts: Vec<DisplayOutputSnapshot>,
}

#[derive(Serialize)]
struct ReplayOutput {
    schema_version: u32,
    contracts: Vec<ReplayContract>,
}

#[derive(Serialize)]
struct ReplayContract {
    contract_sha256: String,
    display_id: mondrian_core::display_contract::DisplayId,
    platform: mondrian_core::display_contract::DisplayPlatform,
    scale_factor_ppm: u32,
    surface_format: String,
    surface_color_space: String,
    surface_hdr_mode: String,
    requested_viewer_mode: String,
    requested_output_color_space: String,
    resolved_output_color_space: String,
    monitor_profile_managed: bool,
    monitor_profile_fingerprint_sha256: Option<String>,
    hdr_requested_supported: bool,
    validation_passed: bool,
    warning_count: usize,
    blocker_count: usize,
}

fn main() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let input_path = PathBuf::from(arguments.next().context("missing replay input path")?);
    let output_path = PathBuf::from(arguments.next().context("missing replay output path")?);
    if arguments.next().is_some() {
        bail!("display contract replay accepts exactly two paths");
    }
    let metadata = std::fs::metadata(&input_path).context("inspect replay input")?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAXIMUM_INPUT_BYTES {
        bail!("display contract replay input is empty, non-file, or oversized");
    }
    let bytes = std::fs::read(&input_path).context("read replay input")?;
    let input: ReplayInput = serde_json::from_slice(&bytes).context("parse replay input")?;
    if input.schema_version != 1
        || input.contracts.is_empty()
        || input.contracts.len() > MAXIMUM_CONTRACTS
    {
        bail!("display contract replay input has an invalid schema or contract count");
    }
    let contracts = input.contracts.into_iter().map(replay_contract).collect();
    let payload = serde_json::to_vec(&ReplayOutput { schema_version: 1, contracts })
        .context("serialize replay output")?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)
        .context("create replay output")?;
    output.write_all(&payload).context("write replay output")?;
    output.sync_all().context("flush replay output")?;
    Ok(())
}

fn replay_contract(snapshot: DisplayOutputSnapshot) -> ReplayContract {
    let (monitor_profile_managed, monitor_profile_fingerprint_sha256) =
        match &snapshot.monitor_profile_status {
            MonitorProfileStatus::ManagedColorSpace { .. } => (true, None),
            MonitorProfileStatus::ManagedIccCalibration { profile_fingerprint, .. } => {
                (true, Some(bytes_to_hex(profile_fingerprint.as_bytes())))
            }
            _ => (false, None),
        };
    ReplayContract {
        contract_sha256: snapshot.contract_identity().to_hex(),
        display_id: snapshot.display_id.clone(),
        platform: snapshot.platform,
        scale_factor_ppm: snapshot.scale_factor.0,
        surface_format: snapshot.surface_format.clone(),
        surface_color_space: snapshot.surface_color_space.clone(),
        surface_hdr_mode: snapshot.surface_hdr_mode.clone(),
        requested_viewer_mode: snapshot.requested_viewer_mode.clone(),
        requested_output_color_space: snapshot.requested_output_color_space.clone(),
        resolved_output_color_space: snapshot.resolved_output_color_space.clone(),
        monitor_profile_managed,
        monitor_profile_fingerprint_sha256,
        hdr_requested_supported: matches!(
            snapshot.hdr_status,
            HdrStatus::RequestedSupported { .. }
        ),
        validation_passed: snapshot.validation_status == DisplayValidationStatus::Pass,
        warning_count: snapshot.warnings.len(),
        blocker_count: snapshot.blockers.len(),
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to String is infallible");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_recomputes_the_complete_contract_identity() {
        let snapshot = mondrian_core::display_probe::FakeDisplayProbe::sdr_pass().snapshot;
        let expected = snapshot.contract_identity().to_hex();
        let replayed = replay_contract(snapshot);
        assert_eq!(replayed.contract_sha256, expected);
        assert!(replayed.validation_passed);
        assert!(!replayed.hdr_requested_supported);
        assert_eq!(replayed.blocker_count, 0);
    }
}
