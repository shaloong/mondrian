//! Image-sequence validation and manifest publication.
//!
//! FFmpeg is an encoding Adapter, not publication evidence. This Module
//! independently decodes every numbered frame, records its content identity,
//! and writes the manifest before the enclosing directory crosses the
//! namespace publication Seam.

use crate::artifact_identity::sha256_file;
use crate::preset::{ExportAlphaMode, ImageSequenceFormat};
use mondrian_core::{ColorSpace, ExecutionCancellationToken, Rational};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub(crate) const MANIFEST_FILE_NAME: &str = "manifest.json";
pub(crate) const FRAME_FILE_PREFIX: &str = "frame-";
pub(crate) const FRAME_FILE_DIGITS: usize = 8;

/// Immutable expectations for one complete image-sequence deliverable.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ImageSequenceValidationContract {
    pub format: ImageSequenceFormat,
    pub frame_count: u64,
    pub width: u32,
    pub height: u32,
    pub frame_rate: Rational,
    pub color_space: ColorSpace,
    pub alpha_mode: ExportAlphaMode,
}

/// Durable inventory for one atomically published image sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ImageSequenceManifest {
    schema_version: u32,
    format: ImageSequenceFormat,
    frame_count: u64,
    width: u32,
    height: u32,
    frame_rate: Rational,
    color_space: ColorSpace,
    alpha_mode: ExportAlphaMode,
    frames: Vec<ImageSequenceFrameManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ImageSequenceFrameManifest {
    index: u64,
    file_name: String,
    byte_len: u64,
    sha256: String,
}

pub(crate) fn frame_file_name(index: u64, format: ImageSequenceFormat) -> String {
    let extension = match format {
        ImageSequenceFormat::Png8 => "png",
    };
    format!("{FRAME_FILE_PREFIX}{index:0FRAME_FILE_DIGITS$}.{extension}")
}

pub(crate) fn ffmpeg_frame_pattern(directory: &Path, format: ImageSequenceFormat) -> PathBuf {
    let extension = match format {
        ImageSequenceFormat::Png8 => "png",
    };
    directory.join(format!(
        "{FRAME_FILE_PREFIX}%0{FRAME_FILE_DIGITS}d.{extension}"
    ))
}

/// Independently validate every frame and durably add the complete manifest.
pub(crate) fn validate_and_write_manifest(
    directory: &Path,
    contract: ImageSequenceValidationContract,
    cancel: &ExecutionCancellationToken,
) -> Result<(), String> {
    let expected_count = usize::try_from(contract.frame_count)
        .map_err(|_| "image-sequence frame count exceeds addressable memory".to_owned())?;
    let actual_entries = std::fs::read_dir(directory)
        .map_err(|error| format!("cannot enumerate image-sequence staging directory: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("cannot enumerate image-sequence staging entry: {error}"))?;
    if actual_entries.len() != expected_count {
        return Err(format!(
            "image sequence contains {} objects; expected exactly {} frames",
            actual_entries.len(),
            expected_count
        ));
    }

    let mut frames = Vec::with_capacity(expected_count);
    for index in 0..contract.frame_count {
        if cancel.is_canceled() {
            return Err("image-sequence validation cancelled".to_owned());
        }
        let file_name = frame_file_name(index, contract.format);
        let path = directory.join(&file_name);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("missing image-sequence frame {file_name}: {error}"))?;
        if !metadata.file_type().is_file() {
            return Err(format!(
                "image-sequence object is not a regular file: {file_name}"
            ));
        }
        validate_frame_decode(&path, contract)?;
        frames.push(ImageSequenceFrameManifest {
            index,
            file_name,
            byte_len: metadata.len(),
            sha256: sha256_file(&path, cancel, "image-sequence frame")?,
        });
    }

    let manifest = ImageSequenceManifest {
        schema_version: 1,
        format: contract.format,
        frame_count: contract.frame_count,
        width: contract.width,
        height: contract.height,
        frame_rate: contract.frame_rate,
        color_space: contract.color_space,
        alpha_mode: contract.alpha_mode,
        frames,
    };
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("cannot serialize image-sequence manifest: {error}"))?;
    mondrian_storage::write_durable_file_atomically(&directory.join(MANIFEST_FILE_NAME), &bytes)
        .map_err(|error| format!("cannot durably publish image-sequence manifest: {error}"))?;
    Ok(())
}

fn validate_frame_decode(
    path: &Path,
    contract: ImageSequenceValidationContract,
) -> Result<(), String> {
    let reader = image::ImageReader::open(path)
        .map_err(|error| format!("cannot open encoded frame {}: {error}", path.display()))?
        .with_guessed_format()
        .map_err(|error| format!("cannot identify encoded frame {}: {error}", path.display()))?;
    if reader.format() != Some(image::ImageFormat::Png) {
        return Err(format!("encoded frame is not PNG: {}", path.display()));
    }
    let decoded = reader
        .decode()
        .map_err(|error| format!("cannot decode encoded frame {}: {error}", path.display()))?;
    if decoded.width() != contract.width || decoded.height() != contract.height {
        return Err(format!(
            "encoded frame dimensions are {}x{}, expected {}x{}: {}",
            decoded.width(),
            decoded.height(),
            contract.width,
            contract.height,
            path.display()
        ));
    }
    let carries_alpha = decoded.color().has_alpha();
    if carries_alpha != (contract.alpha_mode == ExportAlphaMode::Preserve) {
        return Err(format!(
            "encoded frame alpha contract mismatch (has_alpha={carries_alpha}): {}",
            path.display()
        ));
    }
    if decoded.color().bits_per_pixel() != if carries_alpha { 32 } else { 24 } {
        return Err(format!(
            "encoded frame is not 8-bit RGB/RGBA: {}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract(frame_count: u64) -> ImageSequenceValidationContract {
        ImageSequenceValidationContract {
            format: ImageSequenceFormat::Png8,
            frame_count,
            width: 4,
            height: 3,
            frame_rate: Rational::FPS_25,
            color_space: ColorSpace::Srgb,
            alpha_mode: ExportAlphaMode::Preserve,
        }
    }

    #[test]
    fn complete_decodable_sequence_receives_content_identity_manifest() {
        let directory = tempfile::tempdir().expect("temporary sequence directory");
        for index in 0..2 {
            image::RgbaImage::from_pixel(4, 3, image::Rgba([index as u8, 20, 30, 128]))
                .save(directory.path().join(frame_file_name(index, ImageSequenceFormat::Png8)))
                .expect("write PNG fixture");
        }

        validate_and_write_manifest(
            directory.path(),
            contract(2),
            &ExecutionCancellationToken::new(),
        )
        .expect("complete sequence should validate");

        let manifest: ImageSequenceManifest = serde_json::from_slice(
            &std::fs::read(directory.path().join(MANIFEST_FILE_NAME)).expect("read manifest"),
        )
        .expect("parse manifest");
        assert_eq!(manifest.frame_count, 2);
        assert_eq!(manifest.frames.len(), 2);
        assert!(manifest.frames.iter().all(|frame| frame.sha256.len() == 64));
    }

    #[test]
    fn missing_or_extra_namespace_objects_fail_closed_before_manifest() {
        let directory = tempfile::tempdir().expect("temporary sequence directory");
        image::RgbaImage::new(4, 3)
            .save(directory.path().join(frame_file_name(0, ImageSequenceFormat::Png8)))
            .expect("write PNG fixture");
        std::fs::write(directory.path().join("unexpected.txt"), b"not a frame")
            .expect("write unexpected object");

        let error = validate_and_write_manifest(
            directory.path(),
            contract(1),
            &ExecutionCancellationToken::new(),
        )
        .expect_err("extra objects must invalidate sequence completeness");
        assert!(error.contains("expected exactly 1 frames"));
        assert!(!directory.path().join(MANIFEST_FILE_NAME).exists());
    }

    #[test]
    fn corrupt_png_fails_independent_decode_validation() {
        let directory = tempfile::tempdir().expect("temporary sequence directory");
        std::fs::write(
            directory.path().join(frame_file_name(0, ImageSequenceFormat::Png8)),
            b"not a PNG",
        )
        .expect("write corrupt fixture");

        let error = validate_and_write_manifest(
            directory.path(),
            contract(1),
            &ExecutionCancellationToken::new(),
        )
        .expect_err("corrupt frame must fail independent decode");
        assert!(error.contains("not PNG") || error.contains("decode"));
    }
}
