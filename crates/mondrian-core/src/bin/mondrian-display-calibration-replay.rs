use anyhow::{bail, Context, Result};
use mondrian_core::display_calibration::{DisplayCalibrationLut3d, IccProfileFingerprint};
use mondrian_core::{ColorSpace, IccRenderingIntent};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

const MAXIMUM_ICC_BYTES: u64 = 64 * 1024 * 1024;
const MAXIMUM_REQUEST_BYTES: u64 = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayRequest {
    schema_version: u32,
    source_color_space: ColorSpace,
    rendering_intent: IccRenderingIntent,
}

#[derive(Serialize)]
struct ReplayOutput {
    schema_version: u32,
    source_color_space: ColorSpace,
    rendering_intent: IccRenderingIntent,
    profile_fingerprint_sha256: String,
    calibration_identity_sha256: String,
}

fn main() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let profile_path = PathBuf::from(arguments.next().context("missing ICC profile path")?);
    let request_path = PathBuf::from(arguments.next().context("missing replay request path")?);
    let output_path = PathBuf::from(arguments.next().context("missing replay output path")?);
    if arguments.next().is_some() {
        bail!("display calibration replay accepts exactly three paths");
    }
    let profile_bytes = read_bounded(&profile_path, MAXIMUM_ICC_BYTES, "ICC profile")?;
    let request_bytes = read_bounded(&request_path, MAXIMUM_REQUEST_BYTES, "replay request")?;
    let request: ReplayRequest =
        serde_json::from_slice(&request_bytes).context("parse calibration replay request")?;
    if request.schema_version != 1 {
        bail!("display calibration replay request has an invalid schema");
    }
    let fingerprint = IccProfileFingerprint::from_bytes(&profile_bytes);
    let calibration = DisplayCalibrationLut3d::from_icc_bytes_with_intent(
        request.source_color_space,
        &profile_bytes,
        request.rendering_intent,
    )
    .context("build display calibration LUT")?;
    let payload = serde_json::to_vec(&ReplayOutput {
        schema_version: 1,
        source_color_space: request.source_color_space,
        rendering_intent: request.rendering_intent,
        profile_fingerprint_sha256: bytes_to_hex(fingerprint.as_bytes()),
        calibration_identity_sha256: bytes_to_hex(calibration.identity().as_bytes()),
    })
    .context("serialize calibration replay output")?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)
        .context("create calibration replay output")?;
    output.write_all(&payload).context("write calibration replay output")?;
    output.sync_all().context("flush calibration replay output")?;
    Ok(())
}

fn read_bounded(path: &PathBuf, maximum_bytes: u64, label: &str) -> Result<Vec<u8>> {
    let metadata = std::fs::metadata(path).with_context(|| format!("inspect {label}"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > maximum_bytes {
        bail!("{label} is empty, non-file, or oversized");
    }
    std::fs::read(path).with_context(|| format!("read {label}"))
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to String is infallible");
    }
    output
}
