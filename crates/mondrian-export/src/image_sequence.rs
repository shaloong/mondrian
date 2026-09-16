//! Image-sequence validation and manifest publication.
//!
//! FFmpeg is an encoding Adapter, not publication evidence. This Module
//! independently decodes every numbered frame, records its content identity,
//! and writes the manifest before the enclosing directory crosses the
//! namespace publication Seam.

use crate::artifact_identity::sha256_file;
use crate::frame_contract::ExportFrameContract;
use crate::preset::{ExportAlphaMode, ImageSequenceFormat};
use mondrian_core::{ColorSpace, ExecutionCancellationToken, Rational};
use mondrian_media::FfmpegCommand as Command;
use serde::{Deserialize, Serialize};
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

pub(crate) const MANIFEST_FILE_NAME: &str = "manifest.json";
pub(crate) const FRAME_FILE_PREFIX: &str = "frame-";
pub(crate) const FRAME_FILE_DIGITS: usize = 8;

/// Physical encoder family selected by the exact image representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageSequenceEncoderAdapter {
    /// One supervised FFmpeg image2 process consumes the rendered frame pipe.
    FfmpegImage2,
    /// The Rust TIFF encoder consumes exact Float32 frames one at a time.
    NativeTiffFloat,
}

/// Complete execution contract for one still-image representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ImageSequenceEncodingContract {
    pub frame: ExportFrameContract,
    pub output_pixel_format: &'static str,
    pub adapter: ImageSequenceEncoderAdapter,
}

/// Return the renderer boundary owned by an image representation.
///
/// Alpha changes channel presence at the encoder, but never changes scalar
/// precision or the renderer-side four-channel staging contract.
pub(crate) const fn image_sequence_frame_contract(
    format: ImageSequenceFormat,
) -> ExportFrameContract {
    match format {
        ImageSequenceFormat::Png8 => ExportFrameContract::EncodedRgba8Unorm,
        ImageSequenceFormat::Png16 | ImageSequenceFormat::Dpx16 | ImageSequenceFormat::Tiff16 => {
            ExportFrameContract::EncodedRgba16Unorm
        }
        ImageSequenceFormat::OpenExrHalf
        | ImageSequenceFormat::OpenExrFloat
        | ImageSequenceFormat::TiffFloat => ExportFrameContract::FloatMasterRgba32,
    }
}

/// Resolve file representation, renderer boundary and encoder Adapter once.
pub(crate) fn resolve_image_sequence_encoding(
    format: ImageSequenceFormat,
    alpha_mode: ExportAlphaMode,
) -> Result<ImageSequenceEncodingContract, &'static str> {
    let alpha = alpha_mode == ExportAlphaMode::Preserve;
    let contract = match format {
        ImageSequenceFormat::Png8 => ImageSequenceEncodingContract {
            frame: image_sequence_frame_contract(format),
            output_pixel_format: if alpha { "rgba" } else { "rgb24" },
            adapter: ImageSequenceEncoderAdapter::FfmpegImage2,
        },
        ImageSequenceFormat::Png16 => ImageSequenceEncodingContract {
            frame: image_sequence_frame_contract(format),
            output_pixel_format: if alpha { "rgba64be" } else { "rgb48be" },
            adapter: ImageSequenceEncoderAdapter::FfmpegImage2,
        },
        ImageSequenceFormat::OpenExrHalf => ImageSequenceEncodingContract {
            frame: image_sequence_frame_contract(format),
            output_pixel_format: if alpha { "gbrapf32le" } else { "gbrpf32le" },
            adapter: ImageSequenceEncoderAdapter::FfmpegImage2,
        },
        ImageSequenceFormat::OpenExrFloat => ImageSequenceEncodingContract {
            frame: image_sequence_frame_contract(format),
            output_pixel_format: if alpha { "gbrapf32le" } else { "gbrpf32le" },
            adapter: ImageSequenceEncoderAdapter::FfmpegImage2,
        },
        ImageSequenceFormat::Dpx16 if !alpha => ImageSequenceEncodingContract {
            frame: image_sequence_frame_contract(format),
            output_pixel_format: "rgb48be",
            adapter: ImageSequenceEncoderAdapter::FfmpegImage2,
        },
        ImageSequenceFormat::Dpx16 => {
            return Err("DPX 16-bit master does not preserve alpha in this product contract");
        }
        ImageSequenceFormat::Tiff16 => ImageSequenceEncodingContract {
            frame: image_sequence_frame_contract(format),
            output_pixel_format: if alpha { "rgba64le" } else { "rgb48le" },
            adapter: ImageSequenceEncoderAdapter::FfmpegImage2,
        },
        ImageSequenceFormat::TiffFloat => ImageSequenceEncodingContract {
            frame: image_sequence_frame_contract(format),
            output_pixel_format: if alpha {
                "rgbaf32-native"
            } else {
                "rgbf32-native"
            },
            adapter: ImageSequenceEncoderAdapter::NativeTiffFloat,
        },
    };
    Ok(contract)
}

/// Validate representation-specific numeric limits before encoder admission.
pub(crate) fn validate_image_sequence_frame_samples(
    format: ImageSequenceFormat,
    frame_contract: ExportFrameContract,
    frame_bytes: &[u8],
) -> Result<(), String> {
    if format != ImageSequenceFormat::OpenExrHalf {
        return Ok(());
    }
    let rgba = frame_contract
        .to_rgba_f32(frame_bytes)
        .map_err(|error| format!("cannot inspect OpenEXR Half source frame: {error}"))?;
    let half_max = half::f16::MAX.to_f32();
    if let Some((component_index, _)) = rgba
        .iter()
        .copied()
        .enumerate()
        .find(|(component_index, value)| component_index % 4 != 3 && value.abs() > half_max)
    {
        return Err(format!(
            "OpenEXR Half RGB component {component_index} exceeds the finite Half range"
        ));
    }
    Ok(())
}

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
    frame_contract: ExportFrameContract,
    output_pixel_format: String,
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
        ImageSequenceFormat::Png8 | ImageSequenceFormat::Png16 => "png",
        ImageSequenceFormat::OpenExrHalf | ImageSequenceFormat::OpenExrFloat => "exr",
        ImageSequenceFormat::Dpx16 => "dpx",
        ImageSequenceFormat::Tiff16 | ImageSequenceFormat::TiffFloat => "tiff",
    };
    format!("{FRAME_FILE_PREFIX}{index:0FRAME_FILE_DIGITS$}.{extension}")
}

pub(crate) fn ffmpeg_frame_pattern(directory: &Path, format: ImageSequenceFormat) -> PathBuf {
    let extension = match format {
        ImageSequenceFormat::Png8 | ImageSequenceFormat::Png16 => "png",
        ImageSequenceFormat::OpenExrHalf | ImageSequenceFormat::OpenExrFloat => "exr",
        ImageSequenceFormat::Dpx16 => "dpx",
        ImageSequenceFormat::Tiff16 | ImageSequenceFormat::TiffFloat => "tiff",
    };
    directory.join(format!(
        "{FRAME_FILE_PREFIX}%0{FRAME_FILE_DIGITS}d.{extension}"
    ))
}

/// Add only the format-owned FFmpeg encoder options.
pub(crate) fn apply_ffmpeg_image_encoder_args(
    command: &mut Command,
    format: ImageSequenceFormat,
    output_pixel_format: &str,
) -> Result<(), &'static str> {
    match format {
        ImageSequenceFormat::Png8 | ImageSequenceFormat::Png16 => {
            command.arg("-c:v").arg("png").arg("-compression_level").arg("6");
        }
        ImageSequenceFormat::OpenExrHalf => {
            command
                .arg("-c:v")
                .arg("exr")
                .arg("-compression")
                .arg("zip16")
                .arg("-format")
                .arg("half");
        }
        ImageSequenceFormat::OpenExrFloat => {
            command
                .arg("-c:v")
                .arg("exr")
                .arg("-compression")
                .arg("zip16")
                .arg("-format")
                .arg("float");
        }
        ImageSequenceFormat::Dpx16 => {
            command.arg("-c:v").arg("dpx");
        }
        ImageSequenceFormat::Tiff16 => {
            command.arg("-c:v").arg("tiff").arg("-compression_algo").arg("deflate");
        }
        ImageSequenceFormat::TiffFloat => {
            return Err("TIFF Float32 is owned by the native image encoder Adapter");
        }
    }
    command.arg("-pix_fmt").arg(output_pixel_format);
    Ok(())
}

/// Encode one exact Float32 TIFF frame without an integer or RGBA8 seam.
pub(crate) fn write_native_tiff_float_frame(
    path: &Path,
    width: u32,
    height: u32,
    alpha_mode: ExportAlphaMode,
    frame_contract: ExportFrameContract,
    frame_bytes: &[u8],
) -> Result<(), String> {
    if frame_contract != ExportFrameContract::FloatMasterRgba32 {
        return Err("native TIFF Float32 encoder received a non-Float32 frame contract".to_owned());
    }
    let rgba = frame_contract
        .to_rgba_f32(frame_bytes)
        .map_err(|error| format!("cannot unpack TIFF Float32 frame: {error}"))?;
    let file = std::fs::File::create(path).map_err(|error| {
        format!(
            "cannot create TIFF Float32 frame {}: {error}",
            path.display()
        )
    })?;
    let mut encoder = tiff::encoder::TiffEncoder::new(BufWriter::new(file))
        .map_err(|error| format!("cannot initialize TIFF Float32 encoder: {error}"))?;
    if alpha_mode == ExportAlphaMode::Preserve {
        encoder
            .write_image::<tiff::encoder::colortype::RGBA32Float>(width, height, &rgba)
            .map_err(|error| {
                format!(
                    "cannot encode TIFF Float32 frame {}: {error}",
                    path.display()
                )
            })
    } else {
        let rgb = rgba
            .chunks_exact(4)
            .flat_map(|pixel| pixel[..3].iter().copied())
            .collect::<Vec<_>>();
        encoder
            .write_image::<tiff::encoder::colortype::RGB32Float>(width, height, &rgb)
            .map_err(|error| {
                format!(
                    "cannot encode TIFF Float32 frame {}: {error}",
                    path.display()
                )
            })
    }
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

    let encoding = resolve_image_sequence_encoding(contract.format, contract.alpha_mode)
        .map_err(|error| format!("image-sequence manifest contract is invalid: {error}"))?;
    let manifest = ImageSequenceManifest {
        schema_version: 2,
        format: contract.format,
        frame_contract: encoding.frame,
        output_pixel_format: encoding.output_pixel_format.to_owned(),
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
    if contract.format == ImageSequenceFormat::Dpx16 {
        return validate_dpx16_frame(path, contract);
    }
    if matches!(
        contract.format,
        ImageSequenceFormat::OpenExrHalf | ImageSequenceFormat::OpenExrFloat
    ) {
        validate_exr_channel_storage(path, contract)?;
        exr::prelude::read_all_flat_layers_from_file(path)
            .map_err(|error| format!("cannot decode OpenEXR frame {}: {error}", path.display()))?;
        return Ok(());
    }
    if matches!(
        contract.format,
        ImageSequenceFormat::Tiff16 | ImageSequenceFormat::TiffFloat
    ) {
        return validate_tiff_frame(path, contract);
    }
    let reader = image::ImageReader::open(path)
        .map_err(|error| format!("cannot open encoded frame {}: {error}", path.display()))?
        .with_guessed_format()
        .map_err(|error| format!("cannot identify encoded frame {}: {error}", path.display()))?;
    let expected_format = match contract.format {
        ImageSequenceFormat::Png8 | ImageSequenceFormat::Png16 => image::ImageFormat::Png,
        ImageSequenceFormat::OpenExrHalf
        | ImageSequenceFormat::OpenExrFloat
        | ImageSequenceFormat::Dpx16
        | ImageSequenceFormat::Tiff16
        | ImageSequenceFormat::TiffFloat => {
            unreachable!("specialized validation returned above")
        }
    };
    if reader.format() != Some(expected_format) {
        return Err(format!(
            "encoded frame format does not match {:?}: {}",
            contract.format,
            path.display()
        ));
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
    let expected_color = match (contract.format, carries_alpha) {
        (ImageSequenceFormat::Png8, false) => image::ColorType::Rgb8,
        (ImageSequenceFormat::Png8, true) => image::ColorType::Rgba8,
        (ImageSequenceFormat::Png16, false) => image::ColorType::Rgb16,
        (ImageSequenceFormat::Png16, true) => image::ColorType::Rgba16,
        (
            ImageSequenceFormat::OpenExrHalf
            | ImageSequenceFormat::OpenExrFloat
            | ImageSequenceFormat::Dpx16
            | ImageSequenceFormat::Tiff16
            | ImageSequenceFormat::TiffFloat,
            _,
        ) => unreachable!("specialized validation returned above"),
    };
    if decoded.color() != expected_color {
        return Err(format!(
            "encoded frame sample/channel contract is {:?}, expected {:?}: {}",
            decoded.color(),
            expected_color,
            path.display()
        ));
    }
    Ok(())
}

fn validate_tiff_frame(
    path: &Path,
    contract: ImageSequenceValidationContract,
) -> Result<(), String> {
    use tiff::decoder::{Decoder, DecodingResult};

    let file = std::fs::File::open(path)
        .map_err(|error| format!("cannot open TIFF frame {}: {error}", path.display()))?;
    let mut decoder = Decoder::new(BufReader::new(file))
        .map_err(|error| format!("cannot read TIFF header {}: {error}", path.display()))?;
    let dimensions = decoder
        .dimensions()
        .map_err(|error| format!("cannot read TIFF dimensions {}: {error}", path.display()))?;
    if dimensions != (contract.width, contract.height) {
        return Err(format!(
            "TIFF dimensions are {}x{}, expected {}x{}: {}",
            dimensions.0,
            dimensions.1,
            contract.width,
            contract.height,
            path.display()
        ));
    }
    let expected_alpha = contract.alpha_mode == ExportAlphaMode::Preserve;
    let color = decoder
        .colortype()
        .map_err(|error| format!("cannot read TIFF color type {}: {error}", path.display()))?;
    let expected_color = match (contract.format, expected_alpha) {
        (ImageSequenceFormat::Tiff16, false) => tiff::ColorType::RGB(16),
        (ImageSequenceFormat::Tiff16, true) => tiff::ColorType::RGBA(16),
        (ImageSequenceFormat::TiffFloat, false) => tiff::ColorType::RGB(32),
        (ImageSequenceFormat::TiffFloat, true) => tiff::ColorType::RGBA(32),
        _ => return Err("non-TIFF format entered TIFF validation".to_owned()),
    };
    if color != expected_color {
        return Err(format!(
            "TIFF color type is {color:?}, expected {expected_color:?}: {}",
            path.display()
        ));
    }
    let decoded = decoder
        .read_image()
        .map_err(|error| format!("cannot decode TIFF frame {}: {error}", path.display()))?;
    let scalar_matches = matches!(
        (contract.format, decoded),
        (ImageSequenceFormat::Tiff16, DecodingResult::U16(_))
            | (ImageSequenceFormat::TiffFloat, DecodingResult::F32(_))
    );
    if !scalar_matches {
        return Err(format!(
            "TIFF scalar representation does not match {:?}: {}",
            contract.format,
            path.display()
        ));
    }
    Ok(())
}

fn validate_exr_channel_storage(
    path: &Path,
    contract: ImageSequenceValidationContract,
) -> Result<(), String> {
    use exr::prelude::{MetaData, SampleType};

    let metadata = MetaData::read_from_file(path, true)
        .map_err(|error| format!("cannot read OpenEXR metadata {}: {error}", path.display()))?;
    if metadata.headers.len() != 1 {
        return Err(format!(
            "OpenEXR master must contain exactly one layer: {}",
            path.display()
        ));
    }
    let header = &metadata.headers[0];
    if header.layer_size.width() != contract.width as usize
        || header.layer_size.height() != contract.height as usize
    {
        return Err(format!(
            "OpenEXR data window does not match export raster: {}",
            path.display()
        ));
    }
    let expected_sample = match contract.format {
        ImageSequenceFormat::OpenExrHalf => SampleType::F16,
        ImageSequenceFormat::OpenExrFloat => SampleType::F32,
        _ => return Err("non-OpenEXR format entered EXR validation".to_owned()),
    };
    let expected_channels = if contract.alpha_mode == ExportAlphaMode::Preserve {
        ["A", "B", "G", "R"].as_slice()
    } else {
        ["B", "G", "R"].as_slice()
    };
    let actual_channels = header
        .channels
        .list
        .iter()
        .map(|channel| channel.name.to_string())
        .collect::<Vec<_>>();
    if actual_channels.iter().map(String::as_str).collect::<Vec<_>>() != expected_channels {
        return Err(format!(
            "OpenEXR channels {:?} do not match {:?}: {}",
            actual_channels,
            expected_channels,
            path.display()
        ));
    }
    if header
        .channels
        .list
        .iter()
        .any(|channel| channel.sample_type != expected_sample)
    {
        return Err(format!(
            "OpenEXR channel sample type does not match {:?}: {}",
            expected_sample,
            path.display()
        ));
    }
    Ok(())
}

fn validate_dpx16_frame(
    path: &Path,
    contract: ImageSequenceValidationContract,
) -> Result<(), String> {
    let header = std::fs::read(path)
        .map_err(|error| format!("cannot read DPX frame {}: {error}", path.display()))?;
    if header.len() < 804 {
        return Err(format!("DPX header is truncated: {}", path.display()));
    }
    let big_endian = match &header[..4] {
        b"SDPX" => true,
        b"XPDS" => false,
        _ => return Err(format!("DPX magic is invalid: {}", path.display())),
    };
    let read_u32 = |offset: usize| {
        let bytes = [
            header[offset],
            header[offset + 1],
            header[offset + 2],
            header[offset + 3],
        ];
        if big_endian {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        }
    };
    let width = read_u32(772);
    let height = read_u32(776);
    if width != contract.width || height != contract.height {
        return Err(format!(
            "DPX dimensions are {width}x{height}, expected {}x{}: {}",
            contract.width,
            contract.height,
            path.display()
        ));
    }
    if header[800] != 50 || header[803] != 16 {
        return Err(format!(
            "DPX image element is not 16-bit RGB (descriptor={}, bits={}): {}",
            header[800],
            header[803],
            path.display()
        ));
    }
    let mut command = mondrian_media::ffmpeg_command().map_err(|error| error.to_string())?;
    let output = command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(path)
        .arg("-frames:v")
        .arg("1")
        .arg("-f")
        .arg("null")
        .arg("-")
        .output()
        .map_err(|error| format!("cannot launch independent DPX decode: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "DPX decode validation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::Stdio;

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

    fn assert_extended_float_samples_survive(
        path: &Path,
        format: ImageSequenceFormat,
        expected: [f32; 4],
    ) {
        let (actual_samples, tolerance) = match format {
            ImageSequenceFormat::OpenExrHalf | ImageSequenceFormat::OpenExrFloat => {
                let image = exr::prelude::read_all_flat_layers_from_file(path)
                    .expect("decode float OpenEXR proof frame");
                let layer = image.layer_data.first().expect("one OpenEXR layer");
                let sample = |name: &str| {
                    layer
                        .channel_data
                        .list
                        .iter()
                        .find(|channel| channel.name.to_string() == name)
                        .unwrap_or_else(|| panic!("missing OpenEXR {name} channel"))
                        .sample_data
                        .values_as_f32()
                        .next()
                        .unwrap_or_else(|| panic!("empty OpenEXR {name} channel"))
                };
                (
                    [sample("R"), sample("G"), sample("B"), sample("A")],
                    if format == ImageSequenceFormat::OpenExrHalf {
                        1.0e-3
                    } else {
                        1.0e-6
                    },
                )
            }
            ImageSequenceFormat::TiffFloat => {
                let file = std::fs::File::open(path).expect("open float TIFF proof frame");
                let mut decoder = tiff::decoder::Decoder::new(BufReader::new(file))
                    .expect("decode float TIFF proof frame");
                let tiff::decoder::DecodingResult::F32(samples) =
                    decoder.read_image().expect("read float TIFF samples")
                else {
                    panic!("float TIFF proof frame did not decode as Float32");
                };
                ([samples[0], samples[1], samples[2], samples[3]], 1.0e-6)
            }
            _ => return,
        };

        for (channel, (actual, expected)) in actual_samples.into_iter().zip(expected).enumerate() {
            assert!(
                (actual - expected).abs() <= tolerance,
                "{format:?} channel {channel} changed extended-range sample: actual={actual}, expected={expected}, decoded={actual_samples:?}"
            );
        }
    }

    #[test]
    fn high_precision_representation_matrix_encodes_and_validates_exact_storage() {
        let cases = [
            (ImageSequenceFormat::Png16, ExportAlphaMode::Preserve),
            (ImageSequenceFormat::OpenExrHalf, ExportAlphaMode::Preserve),
            (ImageSequenceFormat::OpenExrFloat, ExportAlphaMode::Preserve),
            (ImageSequenceFormat::Dpx16, ExportAlphaMode::FlattenBlack),
            (ImageSequenceFormat::Tiff16, ExportAlphaMode::Preserve),
            (ImageSequenceFormat::TiffFloat, ExportAlphaMode::Preserve),
        ];
        let rgba = [
            -0.25_f32, 0.5, 1.5, 0.5, // exercises float extended range / integer clamp
            0.125, 0.75, 1.0, 1.0,
        ];

        for (format, alpha_mode) in cases {
            let directory = tempfile::tempdir().expect("temporary image master directory");
            let path = directory.path().join(frame_file_name(0, format));
            let encoding = resolve_image_sequence_encoding(format, alpha_mode)
                .expect("representation should resolve");
            let bytes = encoding.frame.pack_rgba_f32(&rgba).expect("pack exact master frame");
            match encoding.adapter {
                ImageSequenceEncoderAdapter::FfmpegImage2 => {
                    let mut command =
                        mondrian_media::ffmpeg_command().expect("admit image fixture command");
                    command
                        .arg("-y")
                        .arg("-hide_banner")
                        .arg("-loglevel")
                        .arg("error")
                        .arg("-f")
                        .arg("rawvideo")
                        .arg("-pix_fmt")
                        .arg(encoding.frame.ffmpeg_pix_fmt())
                        .arg("-s")
                        .arg("2x1")
                        .arg("-i")
                        .arg("pipe:0");
                    apply_ffmpeg_image_encoder_args(
                        &mut command,
                        format,
                        encoding.output_pixel_format,
                    )
                    .expect("FFmpeg representation lowering");
                    command
                        .arg("-frames:v")
                        .arg("1")
                        .arg("-f")
                        .arg("image2")
                        .arg(&path)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::null())
                        .stderr(Stdio::piped());
                    let mut child = command.spawn().expect("launch bundled FFmpeg");
                    child
                        .stdin
                        .take()
                        .expect("FFmpeg stdin")
                        .write_all(&bytes)
                        .expect("write exact frame");
                    let output = child.wait_with_output().expect("wait for FFmpeg");
                    assert!(
                        output.status.success(),
                        "{format:?} encoding failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                ImageSequenceEncoderAdapter::NativeTiffFloat => {
                    write_native_tiff_float_frame(&path, 2, 1, alpha_mode, encoding.frame, &bytes)
                        .expect("native TIFF Float32 encoding");
                }
            }

            validate_frame_decode(
                &path,
                ImageSequenceValidationContract {
                    format,
                    frame_count: 1,
                    width: 2,
                    height: 1,
                    frame_rate: Rational::FPS_25,
                    color_space: if matches!(format, ImageSequenceFormat::Png16) {
                        ColorSpace::Srgb
                    } else {
                        ColorSpace::LinearRec709
                    },
                    alpha_mode,
                },
            )
            .unwrap_or_else(|error| panic!("{format:?} validation failed: {error}"));
            assert_extended_float_samples_survive(
                &path,
                format,
                [rgba[0], rgba[1], rgba[2], rgba[3]],
            );
        }
    }

    #[test]
    fn float_master_alpha_outside_coverage_domain_fails_closed() {
        let error = ExportFrameContract::FloatMasterRgba32
            .pack_rgba_f32(&[0.0, 0.0, 0.0, 1.01])
            .expect_err("float alpha above one must fail");
        assert!(matches!(
            error,
            crate::frame_contract::ExportFramePackingError::AlphaOutOfRange { component_index: 3 }
        ));
    }

    #[test]
    fn open_exr_half_rejects_finite_rgb_outside_half_range_before_encoding() {
        let encoding = resolve_image_sequence_encoding(
            ImageSequenceFormat::OpenExrHalf,
            ExportAlphaMode::Preserve,
        )
        .expect("OpenEXR Half contract");
        let bytes = encoding
            .frame
            .pack_rgba_f32(&[half::f16::MAX.to_f32() * 2.0, 0.0, 0.0, 1.0])
            .expect("Float32 seam accepts finite source");
        let error = validate_image_sequence_frame_samples(
            ImageSequenceFormat::OpenExrHalf,
            encoding.frame,
            &bytes,
        )
        .expect_err("Half encoder admission must reject unrepresentable RGB");
        assert!(error.contains("component 0"));
    }
}
