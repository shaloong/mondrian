//! Validation and manifest publication for Program Output stem packages.

use crate::artifact_identity::sha256_file;
use crate::delivery::ffmpeg_audio_channel_layout;
use crate::preset::AudioStemFormat;
use crate::validator::probe_export_output;
use mondrian_audio::AudioLoudnessReport;
use mondrian_core::{AudioChannelLayout, ExecutionCancellationToken, ProgramOutputId};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub(crate) const AUDIO_STEM_MANIFEST_FILE_NAME: &str = "manifest.json";

#[derive(Debug, Clone)]
pub(crate) struct AudioStemValidationContract {
    pub format: AudioStemFormat,
    pub sample_rate: u32,
    pub channel_layout: AudioChannelLayout,
    pub sample_frames: u64,
    pub stems: Vec<AudioStemExpectation>,
}

#[derive(Debug, Clone)]
pub(crate) struct AudioStemExpectation {
    pub output_id: ProgramOutputId,
    pub name: String,
    pub file_name: String,
    pub loudness: AudioLoudnessReport,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct AudioStemManifest {
    schema_version: u32,
    format: AudioStemFormat,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    sample_frames: u64,
    stems: Vec<AudioStemManifestEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct AudioStemManifestEntry {
    output_id: ProgramOutputId,
    name: String,
    file_name: String,
    byte_len: u64,
    sha256: String,
    loudness: AudioLoudnessReport,
}

pub(crate) fn stem_file_name(index: usize, output_id: ProgramOutputId) -> String {
    format!("stem-{index:03}-{output_id}.wav")
}

pub(crate) fn stem_path(directory: &Path, index: usize, output_id: ProgramOutputId) -> PathBuf {
    directory.join(stem_file_name(index, output_id))
}

pub(crate) fn validate_and_write_audio_stem_manifest(
    directory: &Path,
    contract: AudioStemValidationContract,
    cancel: &ExecutionCancellationToken,
) -> Result<(), String> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| format!("cannot enumerate audio-stem staging directory: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("cannot enumerate audio-stem staging entry: {error}"))?;
    if entries.len() != contract.stems.len() {
        return Err(format!(
            "audio-stem package contains {} objects; expected exactly {} WAV files",
            entries.len(),
            contract.stems.len()
        ));
    }
    let mut manifest_stems = Vec::with_capacity(contract.stems.len());
    for stem in contract.stems {
        if cancel.is_canceled() {
            return Err("audio-stem validation cancelled".to_owned());
        }
        let path = directory.join(&stem.file_name);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("missing audio stem {}: {error}", stem.file_name))?;
        if !metadata.file_type().is_file() || metadata.len() == 0 {
            return Err(format!(
                "audio-stem object is not a non-empty regular file: {}",
                stem.file_name
            ));
        }
        validate_wave_stem(
            &path,
            contract.sample_rate,
            contract.channel_layout,
            contract.sample_frames,
        )?;
        manifest_stems.push(AudioStemManifestEntry {
            output_id: stem.output_id,
            name: stem.name,
            file_name: stem.file_name,
            byte_len: metadata.len(),
            sha256: sha256_file(&path, cancel, "audio stem")?,
            loudness: stem.loudness,
        });
    }
    let manifest = AudioStemManifest {
        schema_version: 1,
        format: contract.format,
        sample_rate: contract.sample_rate,
        channel_layout: contract.channel_layout,
        sample_frames: contract.sample_frames,
        stems: manifest_stems,
    };
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("cannot serialize audio-stem manifest: {error}"))?;
    mondrian_storage::write_durable_file_atomically(
        &directory.join(AUDIO_STEM_MANIFEST_FILE_NAME),
        &bytes,
    )
    .map_err(|error| format!("cannot durably publish audio-stem manifest: {error}"))?;
    Ok(())
}

fn validate_wave_stem(
    path: &Path,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    sample_frames: u64,
) -> Result<(), String> {
    let probe = probe_export_output(path)?;
    let container = probe
        .container_format
        .as_deref()
        .ok_or_else(|| format!("audio stem has no container identity: {}", path.display()))?;
    if !container.split(',').any(|identity| identity.trim() == "wav") {
        return Err(format!("audio stem is not WAV: {}", path.display()));
    }
    if probe.video.is_some() {
        return Err(format!(
            "audio stem unexpectedly contains video: {}",
            path.display()
        ));
    }
    let audio = probe
        .audio
        .as_ref()
        .ok_or_else(|| format!("audio stem has no audio stream: {}", path.display()))?;
    if audio.codec_name.as_deref() != Some("pcm_s24le")
        || audio.sample_rate != Some(sample_rate)
        || audio.channels != Some(channel_layout.channel_count() as u32)
        || audio.channel_layout.as_deref() != ffmpeg_audio_channel_layout(channel_layout)
    {
        return Err(format!(
            "audio stem stream contract mismatch for {}: {audio:?}",
            path.display()
        ));
    }
    let timing = audio.timing;
    let (Some(duration), Some(time_num), Some(time_den)) = (
        timing.duration_ts,
        timing.time_base_num,
        timing.time_base_den,
    ) else {
        return Err(format!(
            "audio stem lacks exact stream duration/time-base evidence: {}",
            path.display()
        ));
    };
    if duration < 0 || time_num <= 0 || time_den <= 0 {
        return Err(format!("audio stem timing is invalid: {}", path.display()));
    }
    let actual = i128::from(duration)
        .checked_mul(i128::from(time_num))
        .and_then(|value| value.checked_mul(i128::from(sample_rate)))
        .ok_or_else(|| "audio stem duration validation overflowed".to_owned())?;
    let expected = i128::from(sample_frames)
        .checked_mul(i128::from(time_den))
        .ok_or_else(|| "audio stem expected duration validation overflowed".to_owned())?;
    if actual != expected {
        return Err(format!(
            "audio stem has {actual}/{time_den} sample-time units; expected {expected}/{time_den}: {}",
            path.display()
        ));
    }
    Ok(())
}
