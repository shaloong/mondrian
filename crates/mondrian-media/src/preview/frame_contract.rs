use super::{DecodedRgbaFrameContract, PreviewSourceColorContract};
use crate::decoder::{
    decoded_video_range_from_ffmpeg, DecodedVideoChromaLocation, DecodedVideoMatrix,
    DecodedVideoRange, DecodedVideoSampling, DecodedVideoSurfaceFormat,
};
use ffmpeg_next as ffmpeg;
use mondrian_core::{MondrianError, Result};
use std::path::Path;

pub(super) fn decoded_surface_format_from_pixel(
    pixel: ffmpeg::util::format::pixel::Pixel,
) -> DecodedVideoSurfaceFormat {
    match pixel {
        ffmpeg::util::format::pixel::Pixel::NV12 => DecodedVideoSurfaceFormat::Nv12,
        ffmpeg::util::format::pixel::Pixel::P010LE => DecodedVideoSurfaceFormat::P010,
        ffmpeg::util::format::pixel::Pixel::P012LE => DecodedVideoSurfaceFormat::P012,
        ffmpeg::util::format::pixel::Pixel::P016LE => DecodedVideoSurfaceFormat::P016,
        ffmpeg::util::format::pixel::Pixel::P210LE => DecodedVideoSurfaceFormat::P210,
        ffmpeg::util::format::pixel::Pixel::P212LE => DecodedVideoSurfaceFormat::P212,
        ffmpeg::util::format::pixel::Pixel::P216LE => DecodedVideoSurfaceFormat::P216,
        ffmpeg::util::format::pixel::Pixel::P410LE => DecodedVideoSurfaceFormat::P410,
        ffmpeg::util::format::pixel::Pixel::P412LE => DecodedVideoSurfaceFormat::P412,
        ffmpeg::util::format::pixel::Pixel::P416LE => DecodedVideoSurfaceFormat::P416,
        ffmpeg::util::format::pixel::Pixel::Y210LE => DecodedVideoSurfaceFormat::Y210,
        ffmpeg::util::format::pixel::Pixel::Y212LE => DecodedVideoSurfaceFormat::Y212,
        ffmpeg::util::format::pixel::Pixel::XV30LE => DecodedVideoSurfaceFormat::Xv30,
        ffmpeg::util::format::pixel::Pixel::XV36LE => DecodedVideoSurfaceFormat::Xv36,
        ffmpeg::util::format::pixel::Pixel::YUV420P => DecodedVideoSurfaceFormat::Yuv420p,
        ffmpeg::util::format::pixel::Pixel::YUV420P10LE => DecodedVideoSurfaceFormat::Yuv420p10le,
        ffmpeg::util::format::pixel::Pixel::YUV422P => DecodedVideoSurfaceFormat::Yuv422p,
        ffmpeg::util::format::pixel::Pixel::YUV422P10LE => DecodedVideoSurfaceFormat::Yuv422p10le,
        ffmpeg::util::format::pixel::Pixel::RGBA => DecodedVideoSurfaceFormat::Rgba8,
        ffmpeg::util::format::pixel::Pixel::BGRA => DecodedVideoSurfaceFormat::Bgra8,
        ffmpeg::util::format::pixel::Pixel::RGBAF16LE => DecodedVideoSurfaceFormat::Rgba16Float,
        ffmpeg::util::format::pixel::Pixel::RGBAF32LE => DecodedVideoSurfaceFormat::Rgba32Float,
        _ => DecodedVideoSurfaceFormat::Other,
    }
}

pub(super) fn decoded_video_sampling_from_frame(
    frame: &ffmpeg::util::frame::video::Video,
) -> DecodedVideoSampling {
    let surface_format = decoded_surface_format_from_pixel(frame.format());
    decoded_video_sampling_from_frame_and_surface(frame, surface_format)
}

pub(super) fn decoded_video_sampling_from_frame_and_surface(
    frame: &ffmpeg::util::frame::video::Video,
    surface_format: DecodedVideoSurfaceFormat,
) -> DecodedVideoSampling {
    DecodedVideoSampling {
        matrix: match decoded_video_matrix_from_ffmpeg(frame.color_space()) {
            Ok(Some(matrix)) => matrix,
            Ok(None) => DecodedVideoMatrix::Unknown,
            Err(_) => DecodedVideoMatrix::Unsupported,
        },
        range: decoded_video_range_from_ffmpeg(frame.color_range()),
        chroma_location: decoded_chroma_location_from_ffmpeg(frame.chroma_location()),
        bit_depth: decoded_bit_depth_from_pixel(frame.format())
            .or_else(|| surface_format.fixed_bit_depth())
            .unwrap_or(0),
    }
}

fn decoded_bit_depth_from_pixel(pixel: ffmpeg::util::format::pixel::Pixel) -> Option<u8> {
    let descriptor = pixel.descriptor()?;
    let component_count = usize::from(descriptor.nb_components()).min(4);
    if component_count == 0 {
        return None;
    }
    // SAFETY: FFmpeg owns this static descriptor and `component_count` is
    // bounded to the four entries in AVPixFmtDescriptor::comp.
    let depth = unsafe {
        (&(*descriptor.as_ptr()).comp)[..component_count]
            .iter()
            .map(|component| component.depth)
            .max()
    }?;
    u8::try_from(depth).ok().filter(|depth| *depth > 0)
}

fn decoded_chroma_location_from_ffmpeg(
    location: ffmpeg::util::chroma::Location,
) -> DecodedVideoChromaLocation {
    match location {
        ffmpeg::util::chroma::Location::Left => DecodedVideoChromaLocation::Left,
        ffmpeg::util::chroma::Location::Center => DecodedVideoChromaLocation::Center,
        ffmpeg::util::chroma::Location::TopLeft => DecodedVideoChromaLocation::TopLeft,
        ffmpeg::util::chroma::Location::Top => DecodedVideoChromaLocation::Top,
        ffmpeg::util::chroma::Location::BottomLeft => DecodedVideoChromaLocation::BottomLeft,
        ffmpeg::util::chroma::Location::Bottom => DecodedVideoChromaLocation::Bottom,
        ffmpeg::util::chroma::Location::Unspecified => DecodedVideoChromaLocation::Unknown,
    }
}

fn pixel_format_is_rgb(pixel: ffmpeg::util::format::pixel::Pixel) -> bool {
    pixel.descriptor().is_some_and(|descriptor| unsafe {
        ((*descriptor.as_ptr()).flags & ffmpeg::ffi::AV_PIX_FMT_FLAG_RGB as u64) != 0
    })
}

fn decoded_video_matrix_from_ffmpeg(
    space: ffmpeg::util::color::Space,
) -> std::result::Result<Option<DecodedVideoMatrix>, String> {
    let matrix = match space {
        ffmpeg::util::color::Space::RGB => Some(DecodedVideoMatrix::Rgb),
        ffmpeg::util::color::Space::BT709 => Some(DecodedVideoMatrix::Bt709),
        ffmpeg::util::color::Space::FCC => Some(DecodedVideoMatrix::Fcc),
        ffmpeg::util::color::Space::BT470BG => Some(DecodedVideoMatrix::Bt470Bg),
        ffmpeg::util::color::Space::SMPTE170M => Some(DecodedVideoMatrix::Smpte170M),
        ffmpeg::util::color::Space::SMPTE240M => Some(DecodedVideoMatrix::Smpte240M),
        ffmpeg::util::color::Space::BT2020NCL => Some(DecodedVideoMatrix::Bt2020NonConstant),
        ffmpeg::util::color::Space::Unspecified => None,
        unsupported => {
            return Err(format!(
                "unsupported FFmpeg YUV matrix {unsupported:?}; constant-luminance and derived matrices require a dedicated conversion"
            ));
        }
    };
    Ok(matrix)
}

pub(super) fn resolve_cpu_rgba_contract(
    decoded: &ffmpeg::util::frame::video::Video,
    source: PreviewSourceColorContract,
    path: &Path,
) -> Result<DecodedRgbaFrameContract> {
    resolve_cpu_rgba_contract_from_metadata(
        decoded.format(),
        decoded.color_space(),
        decoded.color_range(),
        source,
        path,
    )
}

pub(super) fn resolve_cpu_rgba_contract_from_metadata(
    pixel_format: ffmpeg::util::format::pixel::Pixel,
    decoded_color_space: ffmpeg::util::color::Space,
    decoded_color_range: ffmpeg::util::color::Range,
    source: PreviewSourceColorContract,
    path: &Path,
) -> Result<DecodedRgbaFrameContract> {
    if pixel_format_is_rgb(pixel_format) {
        return Ok(DecodedRgbaFrameContract::source_encoded(
            source,
            DecodedVideoMatrix::Rgb,
            DecodedVideoRange::Full,
        ));
    }
    if source.is_data_texture() {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "data-texture materialization requires RGB/GBR decoder samples, got {pixel_format:?}"
            ),
        });
    }

    let decoded_matrix =
        decoded_video_matrix_from_ffmpeg(decoded_color_space).map_err(|reason| {
            MondrianError::DecodeFailed { asset_id: path.display().to_string(), reason }
        })?;
    // CICP RGB colorimetry and YCbCr sampling matrices are independent facts.
    // A missing frame fact can only use a fallback already bound into the
    // immutable source contract by an authored/project policy.
    let matrix = decoded_matrix.or(source.yuv_matrix_fallback).ok_or_else(|| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: format!(
            "YUV matrix is unspecified for resolved source color space {:?}; refusing implicit swscale defaults",
            source.color_space()
        ),
    })?;
    if matrix == DecodedVideoMatrix::Rgb {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason:
                "YUV pixel sampling cannot use RGB/GBR matrix metadata; refusing implicit swscale defaults"
                    .to_owned(),
        });
    }
    // Auto accepts a more local frame fact; an authored range override remains
    // authoritative over incorrect source tags.
    let range = source
        .range
        .resolve_for_frame(decoded_video_range_from_ffmpeg(decoded_color_range));
    if range == DecodedVideoRange::Unknown {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: "YUV quantization range is unspecified; refusing implicit swscale defaults"
                .to_owned(),
        });
    }
    Ok(DecodedRgbaFrameContract::source_encoded(
        source, matrix, range,
    ))
}

pub(super) fn configure_preview_rgba_scaler(
    scaler: &mut ffmpeg::software::scaling::Context,
    contract: DecodedRgbaFrameContract,
    path: &Path,
) -> Result<()> {
    let coefficient_id = match contract.applied_matrix {
        DecodedVideoMatrix::Bt709 => Some(ffmpeg::ffi::SWS_CS_ITU709),
        DecodedVideoMatrix::Bt2020NonConstant => Some(ffmpeg::ffi::SWS_CS_BT2020),
        DecodedVideoMatrix::Fcc => Some(ffmpeg::ffi::SWS_CS_FCC),
        DecodedVideoMatrix::Bt470Bg | DecodedVideoMatrix::Smpte170M => {
            Some(ffmpeg::ffi::SWS_CS_ITU601)
        }
        DecodedVideoMatrix::Smpte240M => Some(ffmpeg::ffi::SWS_CS_SMPTE240M),
        DecodedVideoMatrix::Rgb => None,
        DecodedVideoMatrix::Unknown | DecodedVideoMatrix::Unsupported => {
            return Err(MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: format!(
                    "resolved CPU color contract contains invalid matrix {:?}",
                    contract.applied_matrix
                ),
            });
        }
    };
    let Some(coefficient_id) = coefficient_id else {
        return Ok(());
    };
    let source_full_range = i32::from(contract.applied_range == DecodedVideoRange::Full);
    let result = unsafe {
        let coefficients = ffmpeg::ffi::sws_getCoefficients(coefficient_id);
        ffmpeg::ffi::sws_setColorspaceDetails(
            scaler.as_mut_ptr(),
            coefficients,
            source_full_range,
            coefficients,
            1,
            0,
            1 << 16,
            1 << 16,
        )
    };
    if result < 0 {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "failed to configure swscale matrix {:?} and range {:?}: {}",
                contract.applied_matrix,
                contract.applied_range,
                ffmpeg::Error::from(result)
            ),
        });
    }
    Ok(())
}
