use super::frame_contract::{
    configure_preview_rgba_scaler, decoded_surface_format_from_pixel,
    decoded_video_sampling_from_frame, decoded_video_sampling_from_frame_and_surface,
    resolve_cpu_rgba_contract,
};
use super::native_frame::PreviewDecodeSessionOutputLease;
use super::{
    duration_us, preview_create_rgba_scaler, preview_hardware_frame_format, preview_trace,
    CpuYuvChromaPlaneLayout, CpuYuvChromaSubsampling, CpuYuvFrame, CpuYuvSampleFormat,
    DecodedRgbaFrameContract, FfmpegNativeDecodedFrameResource,
    FfmpegNativeDecodedFrameResourceError, FloatRgbaFrame, PreviewDecodeDiagnostics,
    PreviewDecodePath, PreviewDecodeRepresentation, PreviewDecodeStageDurations,
    PreviewDecodedFramePayload, PreviewHardwareDecodePlan, PreviewNativeDecodeFallback,
    PreviewNativeDecodedFrame, PreviewNativeDecodedFrameError, PreviewNativeDecodedFrameHandle,
    PreviewSourceColorContract, RgbaFrame, Unorm16RgbaFrame,
};
use crate::decoder::{
    DecodedVideoChromaLocation, DecodedVideoSurfaceChromaSubsampling, DecodedVideoSurfaceFormat,
};
use ffmpeg_next as ffmpeg;
use mondrian_core::{MondrianError, Result};
use std::path::Path;
use std::ptr::NonNull;
use std::time::Instant;

fn compact_yuv_layout(
    pixel: ffmpeg::util::format::pixel::Pixel,
) -> Option<(
    CpuYuvChromaSubsampling,
    CpuYuvSampleFormat,
    CpuYuvChromaPlaneLayout,
)> {
    use ffmpeg::util::format::pixel::Pixel;
    Some(match pixel {
        Pixel::YUV420P => (
            CpuYuvChromaSubsampling::Cs420,
            CpuYuvSampleFormat::Unorm8,
            CpuYuvChromaPlaneLayout::Planar,
        ),
        Pixel::YUV420P10LE => (
            CpuYuvChromaSubsampling::Cs420,
            CpuYuvSampleFormat::Unorm16Lsb10,
            CpuYuvChromaPlaneLayout::Planar,
        ),
        Pixel::YUV420P12LE => (
            CpuYuvChromaSubsampling::Cs420,
            CpuYuvSampleFormat::Unorm16Lsb12,
            CpuYuvChromaPlaneLayout::Planar,
        ),
        Pixel::YUV420P16LE => (
            CpuYuvChromaSubsampling::Cs420,
            CpuYuvSampleFormat::Unorm16,
            CpuYuvChromaPlaneLayout::Planar,
        ),
        Pixel::YUV422P10LE => (
            CpuYuvChromaSubsampling::Cs422,
            CpuYuvSampleFormat::Unorm16Lsb10,
            CpuYuvChromaPlaneLayout::Planar,
        ),
        Pixel::YUV422P12LE => (
            CpuYuvChromaSubsampling::Cs422,
            CpuYuvSampleFormat::Unorm16Lsb12,
            CpuYuvChromaPlaneLayout::Planar,
        ),
        Pixel::YUV422P16LE => (
            CpuYuvChromaSubsampling::Cs422,
            CpuYuvSampleFormat::Unorm16,
            CpuYuvChromaPlaneLayout::Planar,
        ),
        Pixel::YUV422P => (
            CpuYuvChromaSubsampling::Cs422,
            CpuYuvSampleFormat::Unorm8,
            CpuYuvChromaPlaneLayout::Planar,
        ),
        Pixel::YUV444P => (
            CpuYuvChromaSubsampling::Cs444,
            CpuYuvSampleFormat::Unorm8,
            CpuYuvChromaPlaneLayout::Planar,
        ),
        Pixel::YUV444P10LE => (
            CpuYuvChromaSubsampling::Cs444,
            CpuYuvSampleFormat::Unorm16Lsb10,
            CpuYuvChromaPlaneLayout::Planar,
        ),
        Pixel::YUV444P12LE | Pixel::YUVA444P12LE => (
            CpuYuvChromaSubsampling::Cs444,
            CpuYuvSampleFormat::Unorm16Lsb12,
            CpuYuvChromaPlaneLayout::Planar,
        ),
        Pixel::YUV444P16LE => (
            CpuYuvChromaSubsampling::Cs444,
            CpuYuvSampleFormat::Unorm16,
            CpuYuvChromaPlaneLayout::Planar,
        ),
        Pixel::NV12 => (
            CpuYuvChromaSubsampling::Cs420,
            CpuYuvSampleFormat::Unorm8,
            CpuYuvChromaPlaneLayout::Interleaved,
        ),
        Pixel::P010LE => (
            CpuYuvChromaSubsampling::Cs420,
            CpuYuvSampleFormat::Unorm16Msb10,
            CpuYuvChromaPlaneLayout::Interleaved,
        ),
        _ => return None,
    })
}

fn convert_decoded_to_compact_yuv(
    decoded: &ffmpeg::util::frame::video::Video,
    path: &Path,
    source_color: PreviewSourceColorContract,
) -> Result<Option<CpuYuvFrame>> {
    let Some((subsampling, sample_format, plane_layout)) = compact_yuv_layout(decoded.format())
    else {
        return Ok(None);
    };
    let color_contract = resolve_cpu_rgba_contract(decoded, source_color, path)?;
    let mut video_sampling = decoded_video_sampling_from_frame(decoded);
    video_sampling.matrix = color_contract.applied_matrix;
    video_sampling.range = color_contract.applied_range;
    if subsampling != CpuYuvChromaSubsampling::Cs444
        && video_sampling.chroma_location == DecodedVideoChromaLocation::Unknown
    {
        // FFmpeg's conventional MPEG-family planar layouts are horizontally
        // left-sited when stream metadata omits a more local fact. Bind that
        // fallback explicitly so the renderer never invents sampling policy.
        video_sampling.chroma_location = DecodedVideoChromaLocation::Left;
    }
    let width = decoded.width();
    let height = decoded.height();
    let chroma_width = match subsampling {
        CpuYuvChromaSubsampling::Cs444 => width,
        _ => width.div_ceil(2),
    };
    let chroma_height = match subsampling {
        CpuYuvChromaSubsampling::Cs420 => height.div_ceil(2),
        CpuYuvChromaSubsampling::Cs422 | CpuYuvChromaSubsampling::Cs444 => height,
    };
    if decoded.format() == ffmpeg::util::format::pixel::Pixel::YUVA444P12LE {
        let stride = decoded.stride(3);
        let row_bytes = width as usize * 2;
        let alpha = decoded.data(3);
        let required = stride
            .checked_mul(height.saturating_sub(1) as usize)
            .and_then(|prefix| prefix.checked_add(row_bytes));
        if stride < row_bytes || required.is_none_or(|required| alpha.len() < required) {
            return Err(MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: "YUVA12 alpha plane layout is invalid".to_owned(),
            });
        }
        // The immutable compact contract must carry normalized coverage,
        // just as CPU RGBA admission does. Padding is not a sample.
        for y in 0..height as usize {
            for (x, bytes) in alpha[y * stride..][..row_bytes].chunks_exact(2).enumerate() {
                let code = u16::from_le_bytes([bytes[0], bytes[1]]);
                if code > 4095 {
                    return Err(MondrianError::DecodeFailed {
                        asset_id: path.display().to_string(),
                        reason: format!(
                            "YUVA12 alpha code {code} at pixel {} exceeds 4095",
                            y * width as usize + x
                        ),
                    });
                }
            }
        }
    }
    // SAFETY: decoded is valid for this borrow. av_frame_clone creates an
    // independently owned AVFrame and retains each immutable AVBufferRef.
    // FFmpeg's frame wrapper is Send + Sync, so the resulting CPU plane view
    // can cross the decode/renderer boundary without copying or reordering
    // roughly 33 MiB for every UHD 4:2:2 10-bit frame.
    let retained = unsafe { ffmpeg::ffi::av_frame_clone(decoded.as_ptr()) };
    if retained.is_null() {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: "FFmpeg could not retain compact CPU YUV planes".to_owned(),
        });
    }
    // SAFETY: retained is a fresh av_frame_clone allocation. Video drop calls
    // av_frame_free exactly once for the owned pointer.
    let retained = unsafe { ffmpeg::util::frame::video::Video::wrap(retained) };
    Ok(Some(
        CpuYuvFrame::new_ffmpeg(
            width,
            height,
            chroma_width,
            chroma_height,
            subsampling,
            sample_format,
            plane_layout,
            source_color,
            video_sampling,
            retained,
            PreviewDecodePath::InProcessFfmpegCpuYuv,
        )
        .with_decoded_surface_format(decoded_surface_format_from_pixel(decoded.format()))
        .with_stage_durations(PreviewDecodeStageDurations::default()),
    ))
}

#[allow(clippy::too_many_arguments)]
fn materialize_decoded_to_compact_yuv(
    decoded: &ffmpeg::util::frame::video::Video,
    scaler: &mut Option<super::scaler::PreviewScaler>,
    scaler_format_contract: &mut Option<(
        ffmpeg::util::format::pixel::Pixel,
        ffmpeg::util::format::pixel::Pixel,
    )>,
    target_width: u32,
    target_height: u32,
    path: &Path,
    source_color: PreviewSourceColorContract,
) -> Result<Option<CpuYuvFrame>> {
    if compact_yuv_layout(decoded.format()).is_none() {
        return Ok(None);
    }
    if decoded.width() == target_width && decoded.height() == target_height {
        return convert_decoded_to_compact_yuv(decoded, path, source_color);
    }

    let scaler = ensure_preview_scaler(
        scaler,
        scaler_format_contract,
        decoded.format(),
        decoded.format(),
        decoded.width(),
        decoded.height(),
        target_width,
        target_height,
        path,
    )?;
    let mut scaled = ffmpeg::util::frame::video::Video::empty();
    let scale_started_at = Instant::now();
    scaler.run(decoded, &mut scaled).map_err(|error| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: error.to_string(),
    })?;
    let swscale_us = duration_us(scale_started_at.elapsed());
    // Preserve frame-local color and temporal facts so scaled and unscaled
    // compact payloads pass through the same fail-closed interpretation.
    unsafe {
        ffmpeg::ffi::av_frame_copy_props(scaled.as_mut_ptr(), decoded.as_ptr());
    }
    convert_decoded_to_compact_yuv(&scaled, path, source_color).map(|frame| {
        frame.map(|frame| {
            frame.with_stage_durations(PreviewDecodeStageDurations {
                swscale_us,
                ..PreviewDecodeStageDurations::default()
            })
        })
    })
}

#[allow(clippy::too_many_arguments)]
fn materialize_yuva12_to_encoded_float(
    decoded: &ffmpeg::util::frame::video::Video,
    scaler: &mut Option<super::scaler::PreviewScaler>,
    scaler_format_contract: &mut Option<(
        ffmpeg::util::format::pixel::Pixel,
        ffmpeg::util::format::pixel::Pixel,
    )>,
    width: u32,
    height: u32,
    path: &Path,
    source_color: PreviewSourceColorContract,
) -> Result<FloatRgbaFrame> {
    let frame = materialize_decoded_to_compact_yuv(
        decoded,
        scaler,
        scaler_format_contract,
        width,
        height,
        path,
        source_color,
    )?
    .expect("YUVA12 is a supported compact layout");
    let contract = resolve_cpu_rgba_contract(decoded, source_color, path)?;
    use crate::DecodedVideoMatrix;
    use mondrian_core::ColorMatrixCoefficients;
    let matrix = match contract.applied_matrix {
        DecodedVideoMatrix::Bt709 => ColorMatrixCoefficients::Bt709,
        DecodedVideoMatrix::Fcc => ColorMatrixCoefficients::Fcc,
        DecodedVideoMatrix::Bt470Bg => ColorMatrixCoefficients::Bt470Bg,
        DecodedVideoMatrix::Smpte170M => ColorMatrixCoefficients::Smpte170M,
        DecodedVideoMatrix::Smpte240M => ColorMatrixCoefficients::Smpte240M,
        DecodedVideoMatrix::Bt2020NonConstant => ColorMatrixCoefficients::Bt2020NonConstant,
        _ => {
            return Err(MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: "YUVA12 requires supported nonconstant YCbCr matrix coefficients"
                    .to_owned(),
            })
        }
    };
    let parameters =
        mondrian_core::EncodedYuvToRgbParameters::new(matrix, contract.applied_range, 12)
            .expect("resolved YUVA12 matrix/range is supported");
    let super::CpuYuvChromaPlanes::Planar { cb, cr } = frame.chroma_planes() else {
        unreachable!("YUVA12 retains planar chroma")
    };
    let planes = [
        frame.luma_plane(),
        cb,
        cr,
        frame.alpha_plane().expect("YUVA12 alpha"),
    ];
    let started = Instant::now();
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    let row_bytes = width as usize * 2;
    for y in 0..height as usize {
        let [luma, cb, cr, alpha] = planes.map(|plane| {
            let start = y * plane.bytes_per_row() as usize;
            plane.data()[start..start + row_bytes].chunks_exact(2)
        });
        for (((luma, cb), cr), alpha) in luma.zip(cb).zip(cr).zip(alpha) {
            let code =
                [luma, cb, cr].map(|bytes| f32::from(u16::from_le_bytes([bytes[0], bytes[1]])));
            let [r, g, b] = parameters.decode(code);
            let a = f32::from(u16::from_le_bytes([alpha[0], alpha[1]])) / 4095.0;
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }
    Ok(FloatRgbaFrame::new(
        width,
        height,
        rgba,
        contract,
        PreviewDecodePath::InProcessFfmpegCpuFloat,
    )
    .with_decoded_surface_format(frame.surface_format())
    .with_decoded_video_sampling(frame.video_sampling)
    .with_stage_durations(PreviewDecodeStageDurations {
        rgba_copy_us: duration_us(started.elapsed()),
        ..frame.diagnostics.stage_durations
    }))
}

pub(super) fn convert_decoded_to_rgba(
    decoded: &ffmpeg::util::frame::video::Video,
    scaler: &mut super::scaler::PreviewScaler,
    path: &Path,
    source_color: PreviewSourceColorContract,
    scaler_color_contract: &mut Option<DecodedRgbaFrameContract>,
) -> Result<RgbaFrame> {
    let decoded_surface_format = decoded_surface_format_from_pixel(decoded.format());
    let decoded_video_sampling = decoded_video_sampling_from_frame(decoded);
    let color_contract = resolve_cpu_rgba_contract(decoded, source_color, path)?;
    if scaler.prepare(decoded).map_err(|error| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: error.to_string(),
    })? {
        *scaler_color_contract = None;
    }
    if *scaler_color_contract != Some(color_contract) {
        configure_preview_rgba_scaler(scaler, color_contract, path)?;
        *scaler_color_contract = Some(color_contract);
    }
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

fn convert_decoded_to_encoded_unorm16_rgba(
    decoded: &ffmpeg::util::frame::video::Video,
    scaler: &mut super::scaler::PreviewScaler,
    path: &Path,
    source_color: PreviewSourceColorContract,
    scaler_color_contract: &mut Option<DecodedRgbaFrameContract>,
) -> Result<Unorm16RgbaFrame> {
    let decoded_surface_format = decoded_surface_format_from_pixel(decoded.format());
    let decoded_video_sampling = decoded_video_sampling_from_frame(decoded);
    let color_contract = resolve_cpu_rgba_contract(decoded, source_color, path)?;
    if scaler.prepare(decoded).map_err(|error| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: error.to_string(),
    })? {
        *scaler_color_contract = None;
    }
    if *scaler_color_contract != Some(color_contract) {
        configure_preview_rgba_scaler(scaler, color_contract, path)?;
        *scaler_color_contract = Some(color_contract);
    }

    let width = scaler.output().width;
    let height = scaler.output().height;
    let row_samples =
        (width as usize).checked_mul(4).ok_or_else(|| MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: "RGBA64 preview row exceeds addressable memory".to_owned(),
        })?;
    let row_bytes = row_samples.checked_mul(std::mem::size_of::<u16>()).ok_or_else(|| {
        MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: "RGBA64 preview row exceeds addressable memory".to_owned(),
        }
    })?;
    let stride = i32::try_from(row_bytes).map_err(|_| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: "RGBA64 preview row exceeds FFmpeg stride limit".to_owned(),
    })?;
    let sample_count =
        row_samples
            .checked_mul(height as usize)
            .ok_or_else(|| MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: "RGBA64 preview extent exceeds addressable memory".to_owned(),
            })?;
    let swscale_started_at = Instant::now();
    let mut output = vec![0_u16; sample_count];
    let destination_data = [
        output.as_mut_ptr().cast::<u8>(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    ];
    let destination_stride = [stride, 0, 0, 0];
    if scaler.input().format != decoded.format()
        || scaler.input().width != decoded.width()
        || scaler.input().height != decoded.height()
        || scaler.output().format != ffmpeg::util::format::pixel::Pixel::RGBA64LE
    {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: "RGBA64 preview scaler format changed before materialization".to_owned(),
        });
    }
    // The ffmpeg-next wrapper calls the same sws_scale function after allocating
    // an AVFrame. Write its RGBA64LE result into the final typed allocation so
    // no second full-resolution pixel copy is needed. The destination is fully
    // initialized and remains owned by Rust throughout the synchronous call.
    let scaled_rows = unsafe {
        let source = decoded.as_ptr();
        ffmpeg::ffi::sws_scale(
            scaler.as_mut_ptr(),
            (*source).data.as_ptr().cast(),
            (*source).linesize.as_ptr(),
            0,
            decoded.height() as i32,
            destination_data.as_ptr(),
            destination_stride.as_ptr(),
        )
    };
    if scaled_rows != height as i32 {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!("RGBA64 preview scaler produced {scaled_rows} of {height} rows"),
        });
    }
    for sample in &mut output {
        *sample = u16::from_le(*sample);
    }
    let swscale_us = duration_us(swscale_started_at.elapsed());

    Ok(Unorm16RgbaFrame::new(
        width,
        height,
        output,
        color_contract,
        PreviewDecodePath::InProcessFfmpegCpuUnorm16,
    )
    .with_decoded_surface_format(decoded_surface_format)
    .with_decoded_video_sampling(decoded_video_sampling)
    .with_stage_durations(PreviewDecodeStageDurations {
        swscale_us,
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
    let decoded_surface_format = decoded_surface_format_from_pixel(decoded.format());
    let decoded_video_sampling = decoded_video_sampling_from_frame(decoded);
    let copy_started_at = Instant::now();
    let rgba = unpack_ffmpeg_planar_float_rgba(decoded, path)?;
    // Float storage does not imply a linear transfer function. Preserve the
    // already resolved author interpretation while bypassing integer swscale
    // for encoded RGB and numeric data just as for scene-linear RGB.
    let color_contract = if source_color.is_scene_linear() {
        DecodedRgbaFrameContract::source_linear(source_color)
    } else {
        resolve_cpu_rgba_contract(decoded, source_color, path)?
    };
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
        color_contract,
        PreviewDecodePath::InProcessFfmpegCpuFloat,
    )
    .with_decoded_surface_format(decoded_surface_format)
    .with_decoded_video_sampling(decoded_video_sampling)
    .with_stage_durations(PreviewDecodeStageDurations {
        rgba_copy_us,
        ..PreviewDecodeStageDurations::default()
    }))
}

fn planar_float_layout(pixel_format: ffmpeg::util::format::pixel::Pixel) -> Option<(bool, bool)> {
    use ffmpeg::util::format::pixel::Pixel;
    match pixel_format {
        Pixel::GBRPF32LE => Some((true, false)),
        Pixel::GBRPF32BE => Some((false, false)),
        Pixel::GBRAPF32LE => Some((true, true)),
        Pixel::GBRAPF32BE => Some((false, true)),
        _ => None,
    }
}

fn unpack_ffmpeg_planar_float_rgba(
    decoded: &ffmpeg::util::frame::video::Video,
    path: &Path,
) -> Result<Vec<f32>> {
    let (little_endian, has_alpha) = planar_float_layout(decoded.format()).ok_or_else(|| {
        MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "float source decoded to unsupported non-planar-f32 format {:?}; refusing integer quantization",
                decoded.format()
            ),
        }
    })?;

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

pub(crate) fn resize_float_rgba(
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
    for (target_y, row) in output.chunks_mut(target_width * 4).enumerate() {
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
    }
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
    #[error(
        "decoded {codec_id:?} {surface_format:?} chroma location {actual:?} conflicts with the codec-defined 4:2:2 co-sited grid"
    )]
    ConflictingChromaLocation {
        codec_id: ffmpeg::codec::Id,
        surface_format: DecodedVideoSurfaceFormat,
        actual: DecodedVideoChromaLocation,
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
            Self::ConflictingChromaLocation { .. } => {
                PreviewNativeDecodeFallback::SamplingMetadataConflict
            }
            Self::Resource(_) => PreviewNativeDecodeFallback::ResourceRetentionFailed,
        }
    }
}

#[cfg(test)]
pub(super) fn materialize_decoded_frame(
    decoded: &ffmpeg::util::frame::video::Video,
    representation: PreviewDecodeRepresentation,
    hardware_decode_plan: &mut PreviewHardwareDecodePlan,
    scaler: &mut Option<super::scaler::PreviewScaler>,
    scaler_format_contract: &mut Option<(
        ffmpeg::util::format::pixel::Pixel,
        ffmpeg::util::format::pixel::Pixel,
    )>,
    scaler_color_contract: &mut Option<DecodedRgbaFrameContract>,
    target_width: u32,
    target_height: u32,
    path: &Path,
    source_color: PreviewSourceColorContract,
) -> Result<PreviewDecodedFramePayload> {
    materialize_decoded_frame_inner(
        decoded,
        representation,
        hardware_decode_plan,
        scaler,
        scaler_format_contract,
        scaler_color_contract,
        target_width,
        target_height,
        path,
        source_color,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn materialize_decoded_frame_with_session_output_lease(
    decoded: &ffmpeg::util::frame::video::Video,
    representation: PreviewDecodeRepresentation,
    hardware_decode_plan: &mut PreviewHardwareDecodePlan,
    scaler: &mut Option<super::scaler::PreviewScaler>,
    scaler_format_contract: &mut Option<(
        ffmpeg::util::format::pixel::Pixel,
        ffmpeg::util::format::pixel::Pixel,
    )>,
    scaler_color_contract: &mut Option<DecodedRgbaFrameContract>,
    target_width: u32,
    target_height: u32,
    path: &Path,
    source_color: PreviewSourceColorContract,
    session_output_lease: PreviewDecodeSessionOutputLease,
) -> Result<PreviewDecodedFramePayload> {
    materialize_decoded_frame_inner(
        decoded,
        representation,
        hardware_decode_plan,
        scaler,
        scaler_format_contract,
        scaler_color_contract,
        target_width,
        target_height,
        path,
        source_color,
        Some(session_output_lease),
    )
}

#[allow(clippy::too_many_arguments)]
fn materialize_decoded_frame_inner(
    decoded: &ffmpeg::util::frame::video::Video,
    representation: PreviewDecodeRepresentation,
    hardware_decode_plan: &mut PreviewHardwareDecodePlan,
    scaler: &mut Option<super::scaler::PreviewScaler>,
    scaler_format_contract: &mut Option<(
        ffmpeg::util::format::pixel::Pixel,
        ffmpeg::util::format::pixel::Pixel,
    )>,
    scaler_color_contract: &mut Option<DecodedRgbaFrameContract>,
    target_width: u32,
    target_height: u32,
    path: &Path,
    source_color: PreviewSourceColorContract,
    session_output_lease: Option<PreviewDecodeSessionOutputLease>,
) -> Result<PreviewDecodedFramePayload> {
    if decoded.is_interlaced() {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: "decoded frame is interlaced but no deinterlacing execution path was admitted"
                .to_owned(),
        });
    }
    if hardware_decode_plan.request.prefers_gpu_residency() {
        if preview_hardware_frame_format(decoded.format()) {
            match materialize_native_decoded_frame(
                decoded,
                source_color,
                hardware_decode_plan.codec_id,
                session_output_lease,
                hardware_decode_plan.native_decode_path,
            ) {
                Ok(frame) => {
                    hardware_decode_plan.mark_gpu_resident_native_observed(frame.handle_kind());
                    return Ok(PreviewDecodedFramePayload::NativeGpu(frame));
                }
                Err(
                    error @ PreviewNativeFrameMaterializationError::ConflictingChromaLocation {
                        ..
                    },
                ) => {
                    return Err(MondrianError::DecodeFailed {
                        asset_id: path.display().to_string(),
                        reason: error.to_string(),
                    });
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
                    "required GPU-resident decode returned software frame {:?}; backend {:?}; codec: {}; device: {}",
                    decoded.format(),
                    hardware_decode_plan.probe.candidate_backend,
                    hardware_decode_plan.ffmpeg_codec_config.reason,
                    hardware_decode_plan.ffmpeg_device_context.reason
                ),
            });
        } else {
            hardware_decode_plan
                .mark_native_decode_fallback(PreviewNativeDecodeFallback::SoftwareFrame);
        }
    }

    materialize_decoded_to_cpu(
        decoded,
        representation,
        hardware_decode_plan,
        scaler,
        scaler_format_contract,
        scaler_color_contract,
        target_width,
        target_height,
        path,
        source_color,
    )
}

pub(super) fn materialize_native_decoded_frame(
    decoded: &ffmpeg::util::frame::video::Video,
    source_color: PreviewSourceColorContract,
    codec_id: ffmpeg::codec::Id,
    session_output_lease: Option<PreviewDecodeSessionOutputLease>,
    decode_path: PreviewDecodePath,
) -> std::result::Result<PreviewNativeDecodedFrame, PreviewNativeFrameMaterializationError> {
    use ffmpeg::util::format::pixel::Pixel;

    let is_hardware_pixel = matches!(
        decoded.format(),
        Pixel::D3D11 | Pixel::VIDEOTOOLBOX | Pixel::VAAPI | Pixel::CUDA
    ) || {
        #[cfg(mondrian_ffmpeg_7_1)]
        {
            decoded.format() == Pixel::D3D12
        }
        #[cfg(not(mondrian_ffmpeg_7_1))]
        {
            false
        }
    };
    if !is_hardware_pixel {
        return Err(
            PreviewNativeFrameMaterializationError::UnsupportedHardwarePixelFormat {
                pixel_format: decoded.format(),
            },
        );
    }
    let surface_format = decoded_native_surface_format(decoded)?;
    let mut sampling = decoded_video_sampling_from_frame_and_surface(decoded, surface_format);
    sampling.range = source_color.range.resolve_for_frame(sampling.range);
    sampling.chroma_location = resolve_codec_defined_native_chroma_location(
        codec_id,
        surface_format,
        sampling.chroma_location,
    )?;
    let resource = FfmpegNativeDecodedFrameResource::retain_with_session_output_lease(
        decoded,
        session_output_lease,
    )?;
    match decoded.format() {
        #[cfg(mondrian_ffmpeg_7_1)]
        Pixel::D3D12 => {
            resource.d3d12_texture()?;
        }
        Pixel::D3D11 => {
            resource.d3d11_texture()?;
        }
        Pixel::VIDEOTOOLBOX => {
            resource.cv_pixel_buffer()?;
        }
        #[cfg(target_os = "linux")]
        Pixel::VAAPI => {
            resource.drm_prime_frame()?;
        }
        #[cfg(target_os = "linux")]
        Pixel::CUDA => {
            resource.cuda_frame()?;
        }
        #[cfg(not(target_os = "linux"))]
        Pixel::VAAPI | Pixel::CUDA => {
            return Err(
                PreviewNativeFrameMaterializationError::UnsupportedHardwarePixelFormat {
                    pixel_format: decoded.format(),
                },
            );
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
        PreviewDecodeDiagnostics::new(decode_path),
    )?)
}

fn resolve_codec_defined_native_chroma_location(
    codec_id: ffmpeg::codec::Id,
    surface_format: DecodedVideoSurfaceFormat,
    location: DecodedVideoChromaLocation,
) -> std::result::Result<DecodedVideoChromaLocation, PreviewNativeFrameMaterializationError> {
    // HEVC 4:2:2 chroma samples are co-sited with the corresponding luma
    // samples by H.265 clause 6.2. Its VUI chroma-location fields apply only
    // to 4:2:0, so FFmpeg can correctly report Unspecified for a 4:2:2 frame.
    // Apple ProRes White Paper (June 2014), p. 8, likewise defines its 4:2:2
    // grid with chroma co-sited on the left luma sample of each pair:
    // https://www.apple.com/support/assets/docs/products/finalcutpro/Apple_ProRes_June_2014.pdf
    // Keep the inference tied to these codecs and the actual sampling grid;
    // other formats still need explicit frame metadata before native import.
    if !matches!(
        codec_id,
        ffmpeg::codec::Id::HEVC | ffmpeg::codec::Id::PRORES
    ) || !surface_format.descriptor().is_some_and(|descriptor| {
        descriptor.chroma_subsampling == Some(DecodedVideoSurfaceChromaSubsampling::Cs422)
    }) {
        return Ok(location);
    }
    match location {
        DecodedVideoChromaLocation::Unknown
        | DecodedVideoChromaLocation::Left
        | DecodedVideoChromaLocation::TopLeft
        | DecodedVideoChromaLocation::BottomLeft => Ok(DecodedVideoChromaLocation::Left),
        actual => Err(
            PreviewNativeFrameMaterializationError::ConflictingChromaLocation {
                codec_id,
                surface_format,
                actual,
            },
        ),
    }
}

fn decoded_native_surface_format(
    decoded: &ffmpeg::util::frame::video::Video,
) -> std::result::Result<DecodedVideoSurfaceFormat, PreviewNativeFrameMaterializationError> {
    #[cfg(target_os = "macos")]
    if decoded.format() == ffmpeg::util::format::pixel::Pixel::VIDEOTOOLBOX {
        // The retained CoreVideo allocation supplies the real storage layout.
        // Direct native owners have no AVHWFramesContext; never fabricate one
        // or infer eight-bit storage from that context's absence.
        let pixel = super::native_frame::ffmpeg_cv_pixel_buffer_format(decoded)?;
        match pixel {
            value
                if value == u32::from_be_bytes(*b"420v")
                    || value == u32::from_be_bytes(*b"420f") =>
            {
                return Ok(DecodedVideoSurfaceFormat::Nv12);
            }
            value if value == u32::from_be_bytes(*b"s4as") => {
                return Ok(DecodedVideoSurfaceFormat::Yuva444sp16);
            }
            value if value == u32::from_be_bytes(*b"x420") => {
                return Ok(DecodedVideoSurfaceFormat::P010);
            }
            value if value == u32::from_be_bytes(*b"x422") => {
                return Ok(DecodedVideoSurfaceFormat::P210);
            }
            value if value == u32::from_be_bytes(*b"sv22") => {
                return Ok(DecodedVideoSurfaceFormat::P216);
            }
            value if value == u32::from_be_bytes(*b"x444") => {
                return Ok(DecodedVideoSurfaceFormat::P410);
            }
            _ => {}
        }
    }
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
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_P012LE => Ok(DecodedVideoSurfaceFormat::P012),
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_P016LE => Ok(DecodedVideoSurfaceFormat::P016),
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_P210LE => Ok(DecodedVideoSurfaceFormat::P210),
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_P212LE => Ok(DecodedVideoSurfaceFormat::P212),
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_P216LE => Ok(DecodedVideoSurfaceFormat::P216),
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_P410LE => Ok(DecodedVideoSurfaceFormat::P410),
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_P412LE => Ok(DecodedVideoSurfaceFormat::P412),
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_P416LE => Ok(DecodedVideoSurfaceFormat::P416),
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_Y210LE => Ok(DecodedVideoSurfaceFormat::Y210),
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_Y212LE => Ok(DecodedVideoSurfaceFormat::Y212),
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_XV30LE => Ok(DecodedVideoSurfaceFormat::Xv30),
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_XV36LE => Ok(DecodedVideoSurfaceFormat::Xv36),
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_RGBAF16LE => {
            Ok(DecodedVideoSurfaceFormat::Rgba16Float)
        }
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_RGBAF32LE => {
            Ok(DecodedVideoSurfaceFormat::Rgba32Float)
        }
        _ => Err(
            PreviewNativeFrameMaterializationError::UnsupportedHardwareSurfaceFormat {
                software_format,
            },
        ),
    }
}

fn materialize_decoded_to_cpu(
    decoded: &ffmpeg::util::frame::video::Video,
    representation: PreviewDecodeRepresentation,
    hardware_decode_plan: &mut PreviewHardwareDecodePlan,
    scaler: &mut Option<super::scaler::PreviewScaler>,
    scaler_format_contract: &mut Option<(
        ffmpeg::util::format::pixel::Pixel,
        ffmpeg::util::format::pixel::Pixel,
    )>,
    scaler_color_contract: &mut Option<DecodedRgbaFrameContract>,
    target_width: u32,
    target_height: u32,
    path: &Path,
    source_color: PreviewSourceColorContract,
) -> Result<PreviewDecodedFramePayload> {
    let compact_fallback = representation.is_native_surface()
        && !source_color.is_scene_linear()
        && !source_color.is_data_texture();
    if !preview_hardware_frame_format(decoded.format()) {
        if representation.is_compact_cpu_yuv()
            || (compact_fallback && compact_yuv_layout(decoded.format()).is_some())
        {
            return materialize_decoded_to_compact_yuv(
                decoded,
                scaler,
                scaler_format_contract,
                target_width,
                target_height,
                path,
                source_color,
            )?
            .map_or_else(
                || {
                    Err(MondrianError::DecodeFailed {
                        asset_id: path.display().to_string(),
                        reason: format!(
                            "compact CPU YUV representation expected supported compact YUV, decoder returned {:?}",
                            decoded.format()
                        ),
                    })
                },
                |frame| Ok(PreviewDecodedFramePayload::CpuYuv(frame)),
            );
        }
        if decoded.format() == ffmpeg::util::format::pixel::Pixel::YUVA444P12LE
            && !source_color.is_scene_linear()
            && !source_color.is_data_texture()
        {
            return materialize_yuva12_to_encoded_float(
                decoded,
                scaler,
                scaler_format_contract,
                target_width,
                target_height,
                path,
                source_color,
            )
            .map(PreviewDecodedFramePayload::CpuFloat);
        }
        if source_color.is_scene_linear() || planar_float_layout(decoded.format()).is_some() {
            return convert_decoded_to_float_rgba(
                decoded,
                target_width,
                target_height,
                path,
                source_color,
            )
            .map(PreviewDecodedFramePayload::CpuFloat);
        }
        let destination_format = cpu_preview_destination_format(decoded);
        let scaler = ensure_preview_scaler(
            scaler,
            scaler_format_contract,
            decoded.format(),
            destination_format,
            decoded.width(),
            decoded.height(),
            target_width,
            target_height,
            path,
        )?;
        return if destination_format == ffmpeg::util::format::pixel::Pixel::RGBA64LE {
            convert_decoded_to_encoded_unorm16_rgba(
                decoded,
                scaler,
                path,
                source_color,
                scaler_color_contract,
            )
            .map(PreviewDecodedFramePayload::CpuUnorm16)
        } else {
            convert_decoded_to_rgba(decoded, scaler, path, source_color, scaler_color_contract)
                .map(PreviewDecodedFramePayload::CpuRgba)
        };
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
    if representation.is_compact_cpu_yuv()
        || (compact_fallback && compact_yuv_layout(transferred.format()).is_some())
    {
        return materialize_decoded_to_compact_yuv(
            &transferred,
            scaler,
            scaler_format_contract,
            target_width,
            target_height,
            path,
            source_color,
        )?
        .map_or_else(
            || {
                Err(MondrianError::DecodeFailed {
                    asset_id: path.display().to_string(),
                    reason: format!(
                        "compact CPU YUV representation expected transferred supported compact YUV, decoder returned {:?}",
                        transferred.format()
                    ),
                })
            },
            |frame| {
                Ok(PreviewDecodedFramePayload::CpuYuv(frame.with_stage_durations(
                    PreviewDecodeStageDurations {
                        hardware_transfer_us,
                        ..PreviewDecodeStageDurations::default()
                    },
                )))
            },
        );
    }
    if transferred.format() == ffmpeg::util::format::pixel::Pixel::YUVA444P12LE
        && !source_color.is_scene_linear()
        && !source_color.is_data_texture()
    {
        return materialize_yuva12_to_encoded_float(
            &transferred,
            scaler,
            scaler_format_contract,
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
    if source_color.is_scene_linear() || planar_float_layout(transferred.format()).is_some() {
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
    let destination_format = cpu_preview_destination_format(&transferred);
    let scaler = ensure_preview_scaler(
        scaler,
        scaler_format_contract,
        transferred.format(),
        destination_format,
        transferred.width(),
        transferred.height(),
        target_width,
        target_height,
        path,
    )?;
    if destination_format == ffmpeg::util::format::pixel::Pixel::RGBA64LE {
        convert_decoded_to_encoded_unorm16_rgba(
            &transferred,
            scaler,
            path,
            source_color,
            scaler_color_contract,
        )
        .map(|frame| {
            PreviewDecodedFramePayload::CpuUnorm16(frame.with_stage_durations(
                PreviewDecodeStageDurations {
                    hardware_transfer_us,
                    ..PreviewDecodeStageDurations::default()
                },
            ))
        })
    } else {
        convert_decoded_to_rgba(
            &transferred,
            scaler,
            path,
            source_color,
            scaler_color_contract,
        )
        .map(|frame| {
            PreviewDecodedFramePayload::CpuRgba(frame.with_stage_durations(
                PreviewDecodeStageDurations {
                    hardware_transfer_us,
                    ..PreviewDecodeStageDurations::default()
                },
            ))
        })
    }
}

fn cpu_preview_destination_format(
    decoded: &ffmpeg::util::frame::video::Video,
) -> ffmpeg::util::format::pixel::Pixel {
    if super::decode_contract::cpu_preview_uses_float(
        decoded_video_sampling_from_frame(decoded).bit_depth,
    ) {
        ffmpeg::util::format::pixel::Pixel::RGBA64LE
    } else {
        ffmpeg::util::format::pixel::Pixel::RGBA
    }
}

fn ensure_preview_scaler<'a>(
    scaler: &'a mut Option<super::scaler::PreviewScaler>,
    scaler_format_contract: &mut Option<(
        ffmpeg::util::format::pixel::Pixel,
        ffmpeg::util::format::pixel::Pixel,
    )>,
    source_format: ffmpeg::util::format::pixel::Pixel,
    destination_format: ffmpeg::util::format::pixel::Pixel,
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
    path: &Path,
) -> Result<&'a mut super::scaler::PreviewScaler> {
    let requested_contract = (source_format, destination_format);
    if scaler.is_none() || *scaler_format_contract != Some(requested_contract) {
        *scaler = Some(preview_create_rgba_scaler(
            source_format,
            destination_format,
            source_width,
            source_height,
            target_width,
            target_height,
            path,
        )?);
        *scaler_format_contract = Some(requested_contract);
    }
    scaler.as_mut().ok_or_else(|| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: "preview scaler was not initialized".to_string(),
    })
}

#[cfg(test)]
mod codec_chroma_tests {
    #[test]
    fn prores_422_resolves_only_its_defined_chroma_grid() {
        use DecodedVideoChromaLocation::{Center, Left, Unknown};
        use DecodedVideoSurfaceFormat::{P010, P210, P410};
        let codec = ffmpeg::codec::Id::PRORES;
        assert_eq!(
            resolve_codec_defined_native_chroma_location(codec, P210, Unknown).unwrap(),
            Left
        );
        assert_eq!(
            resolve_codec_defined_native_chroma_location(codec, P210, Left).unwrap(),
            Left
        );
        assert_eq!(
            resolve_codec_defined_native_chroma_location(codec, P010, Unknown).unwrap(),
            Unknown
        );
        assert_eq!(
            resolve_codec_defined_native_chroma_location(codec, P410, Unknown).unwrap(),
            Unknown
        );
        assert!(matches!(
            resolve_codec_defined_native_chroma_location(codec, P210, Center),
            Err(PreviewNativeFrameMaterializationError::ConflictingChromaLocation { .. })
        ));
    }

    use super::*;

    #[test]
    fn hevc_422_uses_its_defined_grid_without_admitting_unknown_420_or_other_codecs() {
        use DecodedVideoChromaLocation::{Center, Left, Unknown};
        use DecodedVideoSurfaceFormat::{P010, P210};

        assert_eq!(
            resolve_codec_defined_native_chroma_location(ffmpeg::codec::Id::HEVC, P210, Unknown)
                .unwrap(),
            Left
        );
        assert_eq!(
            resolve_codec_defined_native_chroma_location(ffmpeg::codec::Id::HEVC, P010, Unknown)
                .unwrap(),
            Unknown
        );
        assert_eq!(
            resolve_codec_defined_native_chroma_location(ffmpeg::codec::Id::H264, P210, Unknown)
                .unwrap(),
            Unknown
        );
        assert!(matches!(
            resolve_codec_defined_native_chroma_location(ffmpeg::codec::Id::HEVC, P210, Center),
            Err(
                PreviewNativeFrameMaterializationError::ConflictingChromaLocation {
                    actual: Center,
                    ..
                }
            )
        ));
    }
}

#[cfg(test)]
mod rgba64_direct_scale_tests {
    use super::*;
    use crate::decoder::DecodedVideoRange;
    use mondrian_core::ColorSpace;

    #[test]
    fn rgba64_cpu_scale_honors_left_chroma_siting_at_color_edges() {
        use ffmpeg::util::format::pixel::Pixel;

        let mut decoded = ffmpeg::util::frame::video::Video::new(Pixel::YUV420P10LE, 16, 4);
        decoded.set_color_space(ffmpeg::util::color::Space::BT2020NCL);
        decoded.set_color_range(ffmpeg::util::color::Range::MPEG);
        for (plane, (width, height)) in [(16, 4), (8, 2), (8, 2)].into_iter().enumerate() {
            let stride = decoded.stride(plane);
            for row in decoded.data_mut(plane).chunks_exact_mut(stride).take(height) {
                for (x, sample) in row.chunks_exact_mut(2).take(width).enumerate() {
                    let left = [365_u16, 395, 778];
                    let right = [579_u16, 279, 274];
                    let value = if x < width / 2 {
                        left[plane]
                    } else {
                        right[plane]
                    };
                    sample.copy_from_slice(&value.to_le_bytes());
                }
            }
        }
        let path = Path::new("left-chroma-edge.mov");
        let mut scaler =
            preview_create_rgba_scaler(Pixel::YUV420P10LE, Pixel::RGBA64LE, 16, 4, 16, 4, path)
                .expect("production scaler");
        let mut color_contract = None;
        for (location, cb_code, cr_code) in [
            (ffmpeg::util::chroma::Location::Left, 279.0_f32, 274.0_f32),
            (ffmpeg::util::chroma::Location::Center, 308.0, 400.0),
            (ffmpeg::util::chroma::Location::Left, 279.0, 274.0),
        ] {
            // SAFETY: the test exclusively owns the same frame across layout changes.
            unsafe { (*decoded.as_mut_ptr()).chroma_location = location.into() };
            let output = convert_decoded_to_encoded_unorm16_rgba(
                &decoded,
                &mut scaler,
                path,
                PreviewSourceColorContract::automatic(
                    ColorSpace::Rec2100Hlg,
                    DecodedVideoRange::Limited,
                ),
                &mut color_contract,
            )
            .expect("typed RGBA64 output");
            // At x=8 Left samples the green chroma exactly; Center interpolates
            // one quarter of the red chroma. This oracle uses BT.2020 NCL math.
            let y = (579.0 - 64.0) / 876.0;
            let cb = (cb_code - 512.0) / 896.0;
            let cr = (cr_code - 512.0) / 896.0;
            let expected = [
                y + 2.0 * (1.0 - 0.2627) * cr,
                y - 2.0 * 0.0593 * (1.0 - 0.0593) / 0.6780 * cb
                    - 2.0 * 0.2627 * (1.0 - 0.2627) / 0.6780 * cr,
                y + 2.0 * (1.0 - 0.0593) * cb,
            ];
            let pixel = &output.rgba()[(2 * 16 + 8) * 4..(2 * 16 + 9) * 4];
            for (channel, expected) in expected.into_iter().enumerate() {
                let expected = (expected * 65535.0).round() as u16;
                assert!(
                    pixel[channel].abs_diff(expected) <= 512,
                    "{location:?}: {pixel:?}"
                );
            }
            assert_eq!(pixel[3], u16::MAX);
        }
    }

    #[test]
    fn full_chroma_rgba64_preserves_negative_bt2020_chroma() {
        use ffmpeg::util::format::pixel::Pixel;

        // Independent BT.2020 NCL limited-range 10-bit color patches. Negative
        // centered Cb/Cr must stay signed through vertical interpolation.
        for format in [Pixel::YUV420P10LE, Pixel::YUV422P10LE, Pixel::YUV444P10LE] {
            for (yuv, expected) in [
                ([119_u16, 512, 512], [64_u16, 64, 64]),
                ([365, 395, 778], [800, 200, 100]),
                ([579, 279, 274], [200, 800, 100]),
                ([243, 787, 447], [100, 200, 800]),
            ] {
                let mut decoded = ffmpeg::util::frame::video::Video::new(format, 16, 4);
                decoded.set_color_space(ffmpeg::util::color::Space::BT2020NCL);
                decoded.set_color_range(ffmpeg::util::color::Range::MPEG);
                for (plane, value) in yuv.into_iter().enumerate() {
                    for sample in decoded.data_mut(plane).chunks_exact_mut(2) {
                        sample.copy_from_slice(&value.to_le_bytes());
                    }
                }
                let path = Path::new("bt2020-negative-chroma.mov");
                let mut scaler =
                    preview_create_rgba_scaler(format, Pixel::RGBA64LE, 16, 4, 16, 4, path)
                        .expect("full-chroma production scaler");
                let output = convert_decoded_to_encoded_unorm16_rgba(
                    &decoded,
                    &mut scaler,
                    path,
                    PreviewSourceColorContract::automatic(
                        ColorSpace::Rec2100Hlg,
                        DecodedVideoRange::Limited,
                    ),
                    &mut None,
                )
                .expect("typed RGBA64 output");
                for pixel in output.rgba().chunks_exact(4) {
                    for channel in 0..3 {
                        let expected = ((u32::from(expected[channel]) * 65535 + 511) / 1023) as u16;
                        assert!(
                            pixel[channel].abs_diff(expected) <= 512,
                            "{format:?} {yuv:?}: actual={pixel:?}, channel={channel}, expected={expected}"
                        );
                    }
                    assert_eq!(pixel[3], u16::MAX);
                }
            }
        }
    }

    #[test]
    fn direct_rgba64_matches_ffmpeg_frame_for_odd_rows_and_straight_alpha() {
        use ffmpeg::util::format::pixel::Pixel;

        for (width, height) in [(5, 3), (64, 2), (192, 108), (3840, 2160)] {
            let mut decoded =
                ffmpeg::util::frame::video::Video::new(Pixel::YUVA444P12LE, width, height);
            decoded.set_color_space(ffmpeg::util::color::Space::BT709);
            decoded.set_color_range(ffmpeg::util::color::Range::MPEG);
            for plane in 0..4 {
                let stride = decoded.stride(plane);
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        let value = match plane {
                            0 => 256 + ((x * 191 + y * 127) % 3505) as u16,
                            1 => ((x * 761 + y * 103) % 4096) as u16,
                            2 => ((x * 547 + y * 331) % 4096) as u16,
                            _ => [0, 1024, 2048, 4095][(x + y) % 4],
                        };
                        let start = y * stride + x * 2;
                        decoded.data_mut(plane)[start..start + 2]
                            .copy_from_slice(&value.to_le_bytes());
                    }
                }
            }

            let mut scaler = preview_create_rgba_scaler(
                Pixel::YUVA444P12LE,
                Pixel::RGBA64LE,
                width,
                height,
                width,
                height,
                Path::new("synthetic-yuva444p12le.mov"),
            )
            .expect("create production scaler");
            let direct = convert_decoded_to_encoded_unorm16_rgba(
                &decoded,
                &mut scaler,
                Path::new("synthetic-yuva444p12le.mov"),
                PreviewSourceColorContract::automatic(
                    ColorSpace::Rec709,
                    DecodedVideoRange::Limited,
                ),
                &mut None,
            )
            .expect("scale directly into the final RGBA64 allocation");

            let mut reference = ffmpeg::util::frame::video::Video::empty();
            scaler.run(&decoded, &mut reference).expect("scale into an FFmpeg frame");
            let reference_stride = reference.stride(0);
            for y in 0..height as usize {
                let row = &reference.data(0)
                    [y * reference_stride..y * reference_stride + width as usize * 8];
                for (x, sample) in row.chunks_exact(2).enumerate() {
                    assert_eq!(
                        direct.rgba()[y * width as usize * 4 + x],
                        u16::from_le_bytes([sample[0], sample[1]]),
                        "pixel mismatch at {width}x{height} sample ({x},{y})"
                    );
                }
            }
        }
    }
}
