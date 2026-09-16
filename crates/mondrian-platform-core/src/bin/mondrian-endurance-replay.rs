use mondrian_platform_core::{
    EnduranceQualificationProfile, EnduranceQualificationStatus, EnduranceRunManifest,
    PreparedEnduranceQualification,
};
use serde::de::DeserializeOwned;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAXIMUM_PROFILE_BYTES: u64 = 1024 * 1024;
const MAXIMUM_MANIFEST_BYTES: u64 = 2 * 1024 * 1024;
const MAXIMUM_CHUNK_BYTES: u64 = 4 * 1024 * 1024;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut arguments = std::env::args_os().skip(1);
    let profile_path = PathBuf::from(arguments.next().ok_or("missing profile path")?);
    let manifest_path = PathBuf::from(arguments.next().ok_or("missing run manifest path")?);
    let chunk_directory = PathBuf::from(arguments.next().ok_or("missing chunk directory")?);
    let report_path = PathBuf::from(arguments.next().ok_or("missing report output path")?);
    if arguments.next().is_some() {
        return Err("endurance replay accepts exactly four paths".to_owned());
    }
    let profile: EnduranceQualificationProfile =
        read_bounded_regular_json(&profile_path, MAXIMUM_PROFILE_BYTES)?;
    let run: EnduranceRunManifest =
        read_bounded_regular_json(&manifest_path, MAXIMUM_MANIFEST_BYTES)?;
    let chunk_metadata = std::fs::symlink_metadata(&chunk_directory)
        .map_err(|error| format!("inspect chunk directory: {error}"))?;
    if !chunk_metadata.is_dir() || chunk_metadata.file_type().is_symlink() {
        return Err("chunk directory must be a real directory".to_owned());
    }
    let prepared =
        PreparedEnduranceQualification::compile(profile).map_err(|error| error.to_string())?;
    let report = prepared
        .evaluate(run, |receipt| {
            let path = chunk_directory.join(&receipt.file_name);
            read_bounded_regular_json(&path, MAXIMUM_CHUNK_BYTES).map_err(|_| {
                mondrian_platform_core::EnduranceQualificationError::ChunkMismatch {
                    phase_id: "chunk-load".to_owned(),
                }
            })
        })
        .map_err(|error| error.to_string())?;
    if report.status != EnduranceQualificationStatus::Qualified
        || !report.missing_phases.is_empty()
        || !report.verify_evidence()
    {
        return Err("sealed endurance run is not complete and qualified".to_owned());
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

fn read_bounded_regular_json<T: DeserializeOwned>(
    path: &Path,
    maximum_bytes: u64,
) -> Result<T, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > maximum_bytes
    {
        return Err("sealed JSON input is outside its file/type/size bound".to_owned());
    }
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let opened = file.metadata().map_err(|error| error.to_string())?;
    if !opened.is_file() || opened.len() != metadata.len() {
        return Err("sealed JSON input changed before it was opened".to_owned());
    }
    let capacity = usize::try_from(opened.len()).map_err(|error| error.to_string())?;
    let mut bytes = Vec::with_capacity(capacity);
    Read::by_ref(&mut file)
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() != capacity {
        return Err("sealed JSON input changed while it was read".to_owned());
    }
    let after = file.metadata().map_err(|error| error.to_string())?;
    let path_after = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if after.len() != opened.len()
        || !path_after.is_file()
        || path_after.file_type().is_symlink()
        || path_after.len() != opened.len()
    {
        return Err("sealed JSON input changed while it was read".to_owned());
    }
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}
