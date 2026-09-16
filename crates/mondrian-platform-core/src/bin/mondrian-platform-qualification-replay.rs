use mondrian_platform_core::{
    PlatformDriverDisplayQualificationProfile, PlatformQualificationCampaign,
    PlatformQualificationStatus, PreparedPlatformDriverDisplayQualification,
};
use serde::de::DeserializeOwned;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

const MAXIMUM_PROFILE_BYTES: u64 = 1024 * 1024;
const MAXIMUM_CAMPAIGN_BYTES: u64 = 8 * 1024 * 1024;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut arguments = std::env::args_os().skip(1);
    let profile_path = PathBuf::from(arguments.next().ok_or("missing profile path")?);
    let campaign_path = PathBuf::from(arguments.next().ok_or("missing campaign path")?);
    let report_path = PathBuf::from(arguments.next().ok_or("missing report output path")?);
    if arguments.next().is_some() {
        return Err("qualification replay accepts exactly three paths".to_owned());
    }
    let profile: PlatformDriverDisplayQualificationProfile =
        read_bounded_json(&profile_path, MAXIMUM_PROFILE_BYTES)?;
    let campaign: PlatformQualificationCampaign =
        read_bounded_json(&campaign_path, MAXIMUM_CAMPAIGN_BYTES)?;
    let prepared = PreparedPlatformDriverDisplayQualification::compile(profile)
        .map_err(|error| error.to_string())?;
    let report = prepared.evaluate(campaign).map_err(|error| error.to_string())?;
    if report.status != PlatformQualificationStatus::Qualified
        || !report.missing_cells.is_empty()
        || !report.verify_evidence()
    {
        return Err("sealed campaign is not complete and qualified".to_owned());
    }
    let bytes = serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(report_path)
        .map_err(|error| error.to_string())?;
    output.write_all(&bytes).map_err(|error| error.to_string())?;
    output.sync_all().map_err(|error| error.to_string())
}

fn read_bounded_json<T: DeserializeOwned>(path: &Path, maximum_bytes: u64) -> Result<T, String> {
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > maximum_bytes {
        return Err("sealed JSON input is outside its size bound".to_owned());
    }
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}
