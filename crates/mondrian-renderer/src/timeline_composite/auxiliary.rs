//! Shared Float32 registration for Timeline auxiliary picture sources.

use super::{
    timeline_processor_checkpoint, working_float_view, TimelineCompositeError,
    TimelineCpuWorkingSetError,
};
use crate::CpuColorFrame;
use mondrian_core::{ColorFrameAlpha, ImageSampling, WorkingColorSpace, WorkingRgbaF32Frame};
use mondrian_effects::{
    AuxiliaryPolicy, AuxiliaryResolution, AuxiliaryRole, EffectFloatExecutionError,
    EffectFloatFrameError, EffectFloatFrameView, EffectFrameExtent, EffectProcessorControl,
};

type ImageDomain = (EffectFrameExtent, ImageSampling);

/// Maps retain the source grid. Mattes and layers align to the actual primary grid.
pub fn timeline_auxiliary_registration_geometry(
    role: AuxiliaryRole,
    primary: ImageDomain,
    source: ImageDomain,
) -> (ImageDomain, AuxiliaryPolicy) {
    if role == AuxiliaryRole::Map {
        (source, AuxiliaryPolicy::new(AuxiliaryResolution::Authored))
    } else {
        (primary, AuxiliaryPolicy::MATCH_PRIMARY)
    }
}

/// View a working picture as the renderer's linear Float32 effect format.
pub fn timeline_auxiliary_frame_view(frame: &CpuColorFrame) -> EffectFloatFrameView<'_> {
    working_float_view(frame.rgba_f32())
}

fn pixel_count(extent: EffectFrameExtent) -> Result<usize, TimelineCompositeError> {
    let overflow = TimelineCpuWorkingSetError::ArithmeticOverflow;
    (extent.width() as usize)
        .checked_mul(extent.height() as usize)
        .filter(|count| count.checked_mul(16).is_some())
        .ok_or_else(|| overflow.into())
}

fn allocate(extent: EffectFrameExtent) -> Result<Vec<[f32; 4]>, TimelineCompositeError> {
    let count = pixel_count(extent)?;
    let mut data = Vec::new();
    data.try_reserve_exact(count).map_err(|_| {
        frame_error(EffectFloatFrameError::AllocationUnavailable { required_bytes: count * 16 })
    })?;
    Ok(data)
}

fn frame_error(reason: EffectFloatFrameError) -> TimelineCompositeError {
    EffectFloatExecutionError::FrameContract(reason).into()
}

fn empty_grid(actual: EffectFrameExtent) -> TimelineCompositeError {
    frame_error(EffectFloatFrameError::GridExtentMismatch {
        expected: EffectFrameExtent::new(1, 1),
        actual,
    })
}

/// Align an admitted working picture to its requested grid. An absent picture
/// represents the explicit transparent boundary. Equal straight grids borrow
/// the original immutable storage.
pub fn register_timeline_auxiliary_raster(
    source: Option<&CpuColorFrame>,
    extent: EffectFrameExtent,
    sampling: ImageSampling,
    working_color: WorkingColorSpace,
    control: &mut EffectProcessorControl<'_>,
) -> Result<CpuColorFrame, TimelineCompositeError> {
    if extent.is_empty() {
        return Err(empty_grid(extent));
    }
    let Some(source) = source else {
        let mut data = allocate(extent)?;
        for index in 0..pixel_count(extent)? {
            if index % 65_536 == 0 {
                timeline_processor_checkpoint(control)?;
            }
            data.push([0.0; 4]);
        }
        return Ok(CpuColorFrame::working(WorkingRgbaF32Frame {
            width: extent.width(),
            height: extent.height(),
            sampling,
            alpha: ColorFrameAlpha::StraightCoverage,
            color_space: working_color,
            data,
        }));
    };
    let descriptor = source.descriptor();
    if source.rgba_f32().color_space != working_color {
        return Err(TimelineCompositeError::MediaFrameContractMismatch {
            expected: crate::ColorFrameDescriptor {
                color_space: crate::ColorFrameSpace::Working(working_color),
                ..descriptor
            },
            actual: descriptor,
        });
    }
    let view = timeline_auxiliary_frame_view(source);
    if view.extent().is_empty() {
        return Err(empty_grid(view.extent()));
    }
    view.validate_controlled(&mut || timeline_processor_checkpoint(control))?;
    if view.extent() == extent
        && view.sampling() == sampling
        && !view.format().alpha().is_premultiplied()
    {
        return Ok(source.clone());
    }
    let alpha = view.format().alpha();
    let output_alpha = if alpha.is_premultiplied() {
        ColorFrameAlpha::StraightFloat
    } else {
        alpha
    };
    let mut data = allocate(extent)?;
    let source_width = view.extent().width();
    let source_height = view.extent().height();
    for y in 0..extent.height() {
        timeline_processor_checkpoint(control)?;
        let py = ((y as f32 + mondrian_effects::PIXEL_CENTRE) * source_height as f32
            / extent.height() as f32
            - mondrian_effects::PIXEL_CENTRE)
            .clamp(0.0, (source_height - 1) as f32);
        for x in 0..extent.width() {
            if x % 65_536 == 0 {
                timeline_processor_checkpoint(control)?;
            }
            let px = ((x as f32 + mondrian_effects::PIXEL_CENTRE) * source_width as f32
                / extent.width() as f32
                - mondrian_effects::PIXEL_CENTRE)
                .clamp(0.0, (source_width - 1) as f32);
            let ix = px.floor() as u32;
            let iy = py.floor() as u32;
            let weights = [
                (1.0 - px.fract()) * (1.0 - py.fract()),
                px.fract() * (1.0 - py.fract()),
                (1.0 - px.fract()) * py.fract(),
                px.fract() * py.fract(),
            ];
            let mut associated = [0.0; 4];
            for (tap, weight) in mondrian_effects::BILINEAR_TAPS.iter().zip(weights) {
                let sx = (ix + tap[0] as u32).min(source_width - 1);
                let sy = (iy + tap[1] as u32).min(source_height - 1);
                let pixel = mondrian_effects::convert_alpha_sample(
                    view.pixels()[sy as usize * source_width as usize + sx as usize],
                    alpha,
                    ColorFrameAlpha::PremultipliedFloat,
                )
                .map_err(frame_error)?;
                for lane in 0..4 {
                    associated[lane] += pixel[lane] * weight;
                }
            }
            if output_alpha == ColorFrameAlpha::Opaque {
                associated[3] = 1.0;
            }
            data.push(
                mondrian_effects::convert_alpha_sample(
                    associated,
                    ColorFrameAlpha::PremultipliedFloat,
                    output_alpha,
                )
                .map_err(frame_error)?,
            );
        }
    }
    Ok(CpuColorFrame::working(WorkingRgbaF32Frame {
        width: extent.width(),
        height: extent.height(),
        sampling,
        alpha: output_alpha,
        color_space: source.rgba_f32().color_space,
        data,
    }))
}
