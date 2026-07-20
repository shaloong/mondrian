use super::frame_contract::{
    configure_preview_rgba_scaler, decoded_surface_format_from_pixel,
    decoded_video_sampling_from_frame, decoded_video_sampling_from_frame_and_surface,
    resolve_cpu_rgba_contract,
};
use super::{
    duration_us, preview_create_rgba_scaler, preview_hardware_frame_format, preview_trace,
    DecodedRgbaFrameContract, FfmpegNativeDecodedFrameResource,
    FfmpegNativeDecodedFrameResourceError, FloatRgbaFrame, PreviewDecodeDiagnostics,
    PreviewDecodePath, PreviewDecodeStageDurations, PreviewDecodedFramePayload,
    PreviewHardwareDecodePlan, PreviewNativeDecodeFallback, PreviewNativeDecodedFrame,
    PreviewNativeDecodedFrameError, PreviewNativeDecodedFrameHandle, PreviewSourceColorContract,
    RgbaFrame,
};
use crate::decoder::DecodedVideoSurfaceFormat;
use ffmpeg_next as ffmpeg;
use mondrian_core::{MondrianError, Result};
use rayon::prelude::*;
use std::path::Path;
use std::ptr::NonNull;
use std::time::Instant;

pub(super) fn convert_decoded_to_rgba(
    decoded: &ffmpeg::util::frame::video::Video,
    scaler: &mut ffmpeg::software::scaling::Context,
    path: &Path,
    source_color: PreviewSourceColorContract,
) -> Result<RgbaFrame> {
    let decoded_surface_format = decoded_surface_format_from_pixel(decoded.format());
    let decoded_video_sampling = decoded_video_sampling_from_frame(decoded);
    let color_contract = resolve_cpu_rgba_contract(decoded, source_color, path)?;
    configure_preview_rgba_scaler(scaler, color_contract, path)?;
    let mut rgba = ffmpeg::util::frame::video::Video::empty();
    let swscale_started_at = Instant::now();
    scaler.run(decoded, &mut rgba).map_err(|error| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: error.to_string(),
    })?;
    let swscale_us = duration_us(swscale_started_at.elapsed());

    let width = rgba.width();
    let height = rgba.height();
    let stride = rgba.stride(0);
    let row_bytes = width as usize * 4;
    let source = rgba.data(0);
    let copy_started_at = Instant::now();
    let output = if stride == row_bytes {
        source[..row_bytes * height as usize].to_vec()
    } else {
        let mut output = vec![0_u8; row_bytes * height as usize];
        for y in 0..height as usize {
            let source_start = y * stride;
            let source_end = source_start + row_bytes;
            let target_start = y * row_bytes;
            let target_end = target_start + row_bytes;
            output[target_start..target_end].copy_from_slice(&source[source_start..source_end]);
        }
        output
    };
    let rgba_copy_us = duration_us(copy_started_at.elapsed());

    Ok(RgbaFrame::new(
        width,
        height,
        output,
        color_contract,
        PreviewDecodePath::InProcessFfmpegCpuRgba,
    )
    .with_decoded_surface_format(decoded_surface_format)
    .with_decoded_video_sampling(decoded_video_sampling)
    .with_stage_durations(PreviewDecodeStageDurations {
        swscale_us,
        rgba_copy_us,
        ..PreviewDecodeStageDurations::default()
    }))
}

fn convert_decoded_to_float_rgba(
    decoded: &ffmpeg::util::frame::video::Video,
    target_width: u32,
    target_height: u32,
    path: &Path,
    source_color: PreviewSourceColorContract,
) -> Result<FloatRgbaFrame> {
    if !source_color.color_space.is_scene_linear() {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "float preview materialization requires a scene-linear source identity, got {:?}",
                source_color.color_space
            ),
        });
    }

    let decoded_surface_format = decoded_surface_format_from_pixel(decoded.format());
    let decoded_video_sampling = decoded_video_sampling_from_frame(decoded);
    let copy_started_at = Instant::now();
    let rgba = unpack_ffmpeg_planar_float_rgba(decoded, path)?;
    let rgba = resize_float_rgba(
        &rgba,
        decoded.width(),
        decoded.height(),
        target_width,
        target_height,
    );
    let rgba_copy_us = duration_us(copy_started_at.elapsed());

    Ok(FloatRgbaFrame::new(
        target_width,
        target_height,
        rgba,
        DecodedRgbaFrameContract::source_linear(source_color),
        PreviewDecodePath::InProcessFfmpegCpuFloat,
    )
    .with_decoded_surface_format(decoded_surface_format)
    .with_decoded_video_sampling(decoded_video_sampling)
    .with_stage_durations(PreviewDecodeStageDurations {
        rgba_copy_us,
        ..PreviewDecodeStageDurations::default()
    }))
}

fn unpack_ffmpeg_planar_float_rgba(
    decoded: &ffmpeg::util::frame::video::Video,
    path: &Path,
) -> Result<Vec<f32>> {
    let (little_endian, has_alpha) = match decoded.format() {
        ffmpeg::util::format::pixel::Pixel::GBRPF32LE => (true, false),
        ffmpeg::util::format::pixel::Pixel::GBRPF32BE => (false, false),
        ffmpeg::util::format::pixel::Pixel::GBRAPF32LE => (true, true),
        ffmpeg::util::format::pixel::Pixel::GBRAPF32BE => (false, true),
        format => {
            return Err(MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: format!(
                    "scene-linear source decoded to unsupported non-planar-f32 format {format:?}; refusing RGBA8 quantization"
                ),
            });
        }
    };

    let width = decoded.width() as usize;
    let height = decoded.height() as usize;
    let mut rgba = vec![0.0_f32; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let pixel = (y * width + x) * 4;
            for (channel, plane) in [(0, 2), (1, 0), (2, 1)] {
                rgba[pixel + channel] =
                    read_ffmpeg_f32_plane_sample(decoded, plane, x, y, little_endian, path)?;
            }
            rgba[pixel + 3] = if has_alpha {
                read_ffmpeg_f32_plane_sample(decoded, 3, x, y, little_endian, path)?
            } else {
                1.0
            };
        }
    }
    Ok(rgba)
}

fn read_ffmpeg_f32_plane_sample(
    decoded: &ffmpeg::util::frame::video::Video,
    plane: usize,
    x: usize,
    y: usize,
    little_endian: bool,
    path: &Path,
) -> Result<f32> {
    let offset = y * decoded.stride(plane) + x * std::mem::size_of::<f32>();
    let end = offset + std::mem::size_of::<f32>();
    let bytes: [u8; 4] = decoded
        .data(plane)
        .get(offset..end)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "FFmpeg float plane {plane} row {y} is shorter than the declared stride"
            ),
        })?;
    Ok(if little_endian {
        f32::from_le_bytes(bytes)
    } else {
        f32::from_be_bytes(bytes)
    })
}

pub(super) fn resize_float_rgba(
    source: &[f32],
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> Vec<f32> {
    if source_width == target_width && source_height == target_height {
        return source.to_vec();
    }

    let source_width = source_width as usize;
    let source_height = source_height as usize;
    let target_width = target_width as usize;
    let target_height = target_height as usize;
    let scale_x = source_width as f32 / target_width as f32;
    let scale_y = source_height as f32 / target_height as f32;
    let mut output = vec![0.0_f32; target_width * target_height * 4];
    output.par_chunks_mut(target_width * 4).enumerate().for_each(|(target_y, row)| {
        let source_y = ((target_y as f32 + 0.5) * scale_y - 0.5)
            .clamp(0.0, source_height.saturating_sub(1) as f32);
        let y0 = source_y.floor() as usize;
        let y1 = (y0 + 1).min(source_height.saturating_sub(1));
        let fy = source_y - y0 as f32;
        for target_x in 0..target_width {
            let source_x = ((target_x as f32 + 0.5) * scale_x - 0.5)
                .clamp(0.0, source_width.saturating_sub(1) as f32);
            let x0 = source_x.floor() as usize;
            let x1 = (x0 + 1).min(source_width.saturating_sub(1));
            let fx = source_x - x0 as f32;
            for channel in 0..4 {
                let top_left = source[(y0 * source_width + x0) * 4 + channel];
                let top_right = source[(y0 * source_width + x1) * 4 + channel];
                let bottom_left = source[(y1 * source_width + x0) * 4 + channel];
                let bottom_right = source[(y1 * source_width + x1) * 4 + channel];
                let top = top_left + (top_right - top_left) * fx;
                let bottom = bottom_left + (bottom_right - bottom_left) * fx;
                row[target_x * 4 + channel] = top + (bottom - top) * fy;
            }
        }
    });
    output
}

#[derive(Debug, thiserror::Error)]
pub(super) enum PreviewNativeFrameMaterializationError {
    #[error("FFmpeg hardware pixel format {pixel_format:?} has no native media adapter")]
    UnsupportedHardwarePixelFormat {
        pixel_format: ffmpeg::util::format::pixel::Pixel,
    },
    #[error("FFmpeg hardware frame is missing AVFrame::hw_frames_ctx")]
    MissingHardwareFramesContext,
    #[error("FFmpeg hardware frame has an empty AVHWFramesContext payload")]
    MissingHardwareFramesContextData,
    #[error(
        "FFmpeg hardware frame software layout {software_format:?} is not explicitly NV12/P010"
    )]
    UnsupportedHardwareSurfaceFormat {
        software_format: ffmpeg::ffi::AVPixelFormat,
    },
    #[error(transparent)]
    Resource(#[from] FfmpegNativeDecodedFrameResourceError),
    #[error(transparent)]
    Payload(#[from] PreviewNativeDecodedFrameError),
}

impl PreviewNativeFrameMaterializationError {
    fn fallback_reason(&self) -> PreviewNativeDecodeFallback {
        match self {
            Self::UnsupportedHardwarePixelFormat { .. } => {
                PreviewNativeDecodeFallback::ResourceAdapterUnavailable
            }
            Self::MissingHardwareFramesContext
            | Self::MissingHardwareFramesContextData
            | Self::UnsupportedHardwareSurfaceFormat { .. } => {
                PreviewNativeDecodeFallback::SurfaceFormatUnavailable
            }
            Self::Payload(_) => PreviewNativeDecodeFallback::SamplingMetadataIncomplete,
            Self::Resource(_) => PreviewNativeDecodeFallback::ResourceRetentionFailed,
        }
    }
}

pub(super) fn materialize_decoded_frame(
    decoded: &ffmpeg::util::frame::video::Video,
    hardware_decode_plan: &mut PreviewHardwareDecodePlan,
    scaler: &mut Option<ffmpeg::software::scaling::Context>,
    scaler_source_format: &mut Option<ffmpeg::util::format::pixel::Pixel>,
    target_width: u32,
    target_height: u32,
    path: &Path,
    source_color: PreviewSourceColorContract,
) -> Result<PreviewDecodedFramePayload> {
    if hardware_decode_plan.request.prefers_gpu_residency() {
        if preview_hardware_frame_format(decoded.format()) {
            match materialize_native_decoded_frame(decoded, source_color) {
                Ok(frame) => {
                    hardware_decode_plan.mark_gpu_resident_native_observed(frame.handle_kind());
                    return Ok(PreviewDecodedFramePayload::NativeGpu(frame));
                }
                Err(error) if hardware_decode_plan.request.requires_gpu_residency() => {
                    return Err(MondrianError::DecodeFailed {
                        asset_id: path.display().to_string(),
                        reason: format!(
                            "required GPU-resident decoded frame could not be materialized: {error}"
                        ),
                    });
                }
                Err(error) => {
                    hardware_decode_plan.mark_native_decode_fallback(error.fallback_reason());
                    preview_trace(format!(
                        "[preview] native FFmpeg frame materialization failed, fallback CPU transfer: {error}"
                    ));
                }
            }
        } else if hardware_decode_plan.request.requires_gpu_residency() {
            return Err(MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: format!(
                    "required GPU-resident decode returned software frame {:?}",
                    decoded.format()
                ),
            });
        } else {
            hardware_decode_plan
                .mark_native_decode_fallback(PreviewNativeDecodeFallback::SoftwareFrame);
        }
    }

    materialize_decoded_to_cpu(
        decoded,
        hardware_decode_plan,
        scaler,
        scaler_source_format,
        target_width,
        target_height,
        path,
        source_color,
    )
}

fn materialize_native_decoded_frame(
    decoded: &ffmpeg::util::frame::video::Video,
    source_color: PreviewSourceColorContract,
) -> std::result::Result<PreviewNativeDecodedFrame, PreviewNativeFrameMaterializationError> {
    use ffmpeg::util::format::pixel::Pixel;

    if !matches!(decoded.format(), Pixel::D3D12 | Pixel::D3D11) {
        return Err(
            PreviewNativeFrameMaterializationError::UnsupportedHardwarePixelFormat {
                pixel_format: decoded.format(),
            },
        );
    }
    let surface_format = decoded_native_surface_format(decoded)?;
    let mut sampling = decoded_video_sampling_from_frame_and_surface(decoded, surface_format);
    sampling.range = source_color.range.resolve_for_frame(sampling.range);
    let resource = FfmpegNativeDecodedFrameResource::retain(decoded)?;
    match decoded.format() {
        Pixel::D3D12 => {
            resource.d3d12_texture()?;
        }
        Pixel::D3D11 => {
            resource.d3d11_texture()?;
        }
        _ => unreachable!("native frame pixel format was validated above"),
    }
    let handle = PreviewNativeDecodedFrameHandle::new(resource);
    Ok(PreviewNativeDecodedFrame::new(
        decoded.width(),
        decoded.height(),
        handle,
        surface_format,
        sampling,
        PreviewDecodeDiagnostics::new(PreviewDecodePath::InProcessFfmpegNative),
    )?)
}

fn decoded_native_surface_format(
    decoded: &ffmpeg::util::frame::video::Video,
) -> std::result::Result<DecodedVideoSurfaceFormat, PreviewNativeFrameMaterializationError> {
    // SAFETY: decoded owns the AVFrame and its hw_frames_ctx for this borrow.
    let hardware_frames_context = unsafe { (*decoded.as_ptr()).hw_frames_ctx };
    if hardware_frames_context.is_null() {
        return Err(PreviewNativeFrameMaterializationError::MissingHardwareFramesContext);
    }
    // SAFETY: hardware_frames_context is non-null and owned by decoded.
    let context_data = unsafe { (*hardware_frames_context).data };
    let context = NonNull::new(context_data.cast::<ffmpeg::ffi::AVHWFramesContext>())
        .ok_or(PreviewNativeFrameMaterializationError::MissingHardwareFramesContextData)?;
    // SAFETY: FFmpeg defines AVBufferRef::data as AVHWFramesContext here.
    let software_format = unsafe { context.as_ref().sw_format };
    decoded_native_surface_format_from_software_format(software_format)
}

pub(super) fn decoded_native_surface_format_from_software_format(
    software_format: ffmpeg::ffi::AVPixelFormat,
) -> std::result::Result<DecodedVideoSurfaceFormat, PreviewNativeFrameMaterializationError> {
    match software_format {
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NV12 => Ok(DecodedVideoSurfaceFormat::Nv12),
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_P010LE => Ok(DecodedVideoSurfaceFormat::P010),
        _ => Err(
            PreviewNativeFrameMaterializationError::UnsupportedHardwareSurfaceFormat {
                software_format,
            },
        ),
    }
}

fn materialize_decoded_to_cpu(
    decoded: &ffmpeg::util::frame::video::Video,
    hardware_decode_plan: &mut PreviewHardwareDecodePlan,
    scaler: &mut Option<ffmpeg::software::scaling::Context>,
    scaler_source_format: &mut Option<ffmpeg::util::format::pixel::Pixel>,
    target_width: u32,
    target_height: u32,
    path: &Path,
    source_color: PreviewSourceColorContract,
) -> Result<PreviewDecodedFramePayload> {
    if !preview_hardware_frame_format(decoded.format()) {
        if source_color.color_space.is_scene_linear() {
            return convert_decoded_to_float_rgba(
                decoded,
                target_width,
                target_height,
                path,
                source_color,
            )
            .map(PreviewDecodedFramePayload::CpuFloat);
        }
        let scaler = ensure_preview_rgba_scaler(
            scaler,
            scaler_source_format,
            decoded.format(),
            decoded.width(),
            decoded.height(),
            target_width,
            target_height,
            path,
        )?;
        return convert_decoded_to_rgba(decoded, scaler, path, source_color)
            .map(PreviewDecodedFramePayload::CpuRgba);
    }

    let mut transferred = ffmpeg::util::frame::video::Video::empty();
    let transfer_started_at = Instant::now();
    let result = unsafe {
        ffmpeg::ffi::av_hwframe_transfer_data(transferred.as_mut_ptr(), decoded.as_ptr(), 0)
    };
    let hardware_transfer_us = duration_us(transfer_started_at.elapsed());
    if result < 0 {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "hardware frame transfer to CPU failed: {}",
                ffmpeg::Error::from(result)
            ),
        });
    }
    unsafe {
        ffmpeg::ffi::av_frame_copy_props(transferred.as_mut_ptr(), decoded.as_ptr());
    }
    hardware_decode_plan.mark_hardware_cpu_transfer_observed();
    if source_color.color_space.is_scene_linear() {
        return convert_decoded_to_float_rgba(
            &transferred,
            target_width,
            target_height,
            path,
            source_color,
        )
        .map(|frame| {
            PreviewDecodedFramePayload::CpuFloat(frame.with_stage_durations(
                PreviewDecodeStageDurations {
                    hardware_transfer_us,
                    ..PreviewDecodeStageDurations::default()
                },
            ))
        });
    }
    let scaler = ensure_preview_rgba_scaler(
        scaler,
        scaler_source_format,
        transferred.format(),
        transferred.width(),
        transferred.height(),
        target_width,
        target_height,
        path,
    )?;
    convert_decoded_to_rgba(&transferred, scaler, path, source_color).map(|frame| {
        PreviewDecodedFramePayload::CpuRgba(frame.with_stage_durations(
            PreviewDecodeStageDurations {
                hardware_transfer_us,
                ..PreviewDecodeStageDurations::default()
            },
        ))
    })
}

fn ensure_preview_rgba_scaler<'a>(
    scaler: &'a mut Option<ffmpeg::software::scaling::Context>,
    scaler_source_format: &mut Option<ffmpeg::util::format::pixel::Pixel>,
    source_format: ffmpeg::util::format::pixel::Pixel,
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
    path: &Path,
) -> Result<&'a mut ffmpeg::software::scaling::Context> {
    if scaler.is_none() || *scaler_source_format != Some(source_format) {
        *scaler = Some(preview_create_rgba_scaler(
            source_format,
            source_width,
            source_height,
            target_width,
            target_height,
            path,
        )?);
        *scaler_source_format = Some(source_format);
    }
    scaler.as_mut().ok_or_else(|| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: "preview RGBA scaler was not initialized".to_string(),
    })
}
