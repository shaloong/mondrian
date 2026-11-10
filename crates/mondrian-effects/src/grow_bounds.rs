//! Transparent expansion of a layer's raster without resampling its pixels.

use mondrian_core::ColorFrameAlpha;

use crate::{
    EffectFloatExecutionError, EffectFloatFrame, EffectFloatFrameError, EffectProcessorControl,
    EffectRasterBounds,
};

/// Maximum authored padding in pixels on each side.
pub const GROW_BOUNDS_MAX_PIXELS: f32 = 10_000.0;

/// Canonical integer padding for one Grow Bounds operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GrowBoundsGrade {
    padding: u32,
}

/// An invalid Grow Bounds control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GrowBoundsError {
    /// Pixels must be finite and within the authoring interval.
    #[error("grow bounds pixels must be finite and in [0, 10000]")]
    InvalidPixels,
}

impl GrowBoundsGrade {
    /// Round positive fractional padding outward to a whole raster pixel.
    pub fn new(pixels: f32) -> Result<Self, GrowBoundsError> {
        if !(0.0..=GROW_BOUNDS_MAX_PIXELS).contains(&pixels) {
            return Err(GrowBoundsError::InvalidPixels);
        }
        Ok(Self { padding: pixels.ceil() as u32 })
    }

    /// Symmetric padding along the two raster axes.
    pub const fn output_outset(self) -> [u32; 2] {
        [self.padding; 2]
    }

    /// Padding packed exactly in a Float32 uniform lane.
    pub fn gpu_controls(self) -> [f32; 4] {
        [self.padding as f32, 0.0, 0.0, 0.0]
    }
}

/// Expand a Float32 frame without filtering its existing texels.
pub fn grow_bounds_frame_f32(
    source: &EffectFloatFrame,
    grade: GrowBoundsGrade,
) -> Result<EffectFloatFrame, EffectFloatExecutionError> {
    apply_grow_bounds_controlled(source, grade, &mut EffectProcessorControl::uncancelled())
}

/// Expand a Float32 frame under the owner's cancellation and deadline checks.
pub fn apply_grow_bounds_controlled(
    source: &EffectFloatFrame,
    grade: GrowBoundsGrade,
    control: &mut EffectProcessorControl<'_>,
) -> Result<EffectFloatFrame, EffectFloatExecutionError> {
    let mut checkpoint = || {
        control
            .checkpoint()
            .map_err(|reason| EffectFloatExecutionError::Stopped { reason })
    };
    checkpoint()?;
    let original = source.extent();
    let grows = grade.padding != 0 && !original.is_empty();
    let bounds = EffectRasterBounds::from_extent(original);
    let extent = if grows {
        bounds
            .checked_outset(grade.output_outset())
            .ok_or(EffectFloatFrameError::ExtentOverflow)?
            .extent()
    } else {
        original
    };
    let len = (extent.width() as usize)
        .checked_mul(extent.height() as usize)
        .ok_or(EffectFloatFrameError::ExtentOverflow)?;
    let required_bytes = len
        .checked_mul(std::mem::size_of::<[f32; 4]>())
        .ok_or(EffectFloatFrameError::ExtentOverflow)?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(len)
        .map_err(|_| EffectFloatFrameError::AllocationUnavailable { required_bytes })?;
    let mut format = source.format();
    if grows {
        if format.alpha() == ColorFrameAlpha::Opaque {
            format = format.with_alpha(ColorFrameAlpha::StraightCoverage);
        }
        let out_width = extent.width() as usize;
        let padding = grade.padding as usize;
        let width = original.width() as usize;
        while pixels.len() < len {
            checkpoint()?;
            pixels.resize(len.min(pixels.len() + 4096), [0.0; 4]);
        }
        for (y, row) in source.pixels().chunks_exact(width).enumerate() {
            checkpoint()?;
            let start = (y + padding) * out_width + padding;
            pixels[start..start + width].copy_from_slice(row);
        }
    } else {
        for chunk in source.pixels().chunks(4096) {
            checkpoint()?;
            pixels.extend_from_slice(chunk);
        }
    }
    checkpoint()?;
    Ok(EffectFloatFrame::from_validated_parts(
        extent,
        format,
        source.sampling(),
        pixels,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectColorDomain, EffectFrameExtent, EffectValueFormat, EffectWorkingPrecision};
    use mondrian_core::ImageSampling;

    fn frame(alpha: ColorFrameAlpha, pixels: Vec<[f32; 4]>) -> EffectFloatFrame {
        EffectFloatFrame::from_pixels(
            EffectFrameExtent::new(2, 1),
            EffectValueFormat::new(
                EffectWorkingPrecision::Float32,
                EffectColorDomain::SceneLinearRgb,
                alpha,
            ),
            ImageSampling::UNIT_SQUARE,
            pixels,
        )
        .unwrap()
    }

    #[test]
    fn controls_round_outward_and_validate_authoring_interval() {
        for (pixels, expected) in [(0.0, 0), (0.25, 1), (10.0, 10), (10_000.0, 10_000)] {
            let grade = GrowBoundsGrade::new(pixels).unwrap();
            assert_eq!(grade.output_outset(), [expected; 2]);
            assert_eq!(grade.gpu_controls()[0], expected as f32);
        }
        for invalid in [-0.1, 10_000.1, f32::NAN, f32::INFINITY] {
            assert_eq!(
                GrowBoundsGrade::new(invalid),
                Err(GrowBoundsError::InvalidPixels)
            );
        }
    }

    #[test]
    fn growth_preserves_source_texels_and_association() {
        let pixels = [[-0.5, 3.0, 0.25, 0.5], [0.1, 0.2, 0.3, 0.25]];
        let source = frame(ColorFrameAlpha::PremultipliedCoverage, pixels.to_vec());
        let output = grow_bounds_frame_f32(&source, GrowBoundsGrade::new(0.5).unwrap()).unwrap();
        assert_eq!(output.extent(), EffectFrameExtent::new(4, 3));
        assert_eq!(output.format(), source.format());
        assert_eq!(output.sampling(), source.sampling());
        assert_eq!(&output.pixels()[5..7], &pixels);
        assert!(output
            .pixels()
            .iter()
            .enumerate()
            .all(|(index, pixel)| { (5..7).contains(&index) || *pixel == [0.0; 4] }));
    }

    #[test]
    fn only_effective_growth_opens_opaque_coverage() {
        let source = frame(ColorFrameAlpha::Opaque, vec![[0.2, 0.4, 0.8, 1.0]; 2]);
        let identity = grow_bounds_frame_f32(&source, GrowBoundsGrade::new(0.0).unwrap()).unwrap();
        assert_eq!(identity.format(), source.format());
        assert_eq!(identity.pixels(), source.pixels());
        let grown = grow_bounds_frame_f32(&source, GrowBoundsGrade::new(1.0).unwrap()).unwrap();
        assert_eq!(grown.format().alpha(), ColorFrameAlpha::StraightCoverage);
        assert_eq!(&grown.pixels()[5..7], source.pixels());
    }

    #[test]
    fn authored_grow_bounds_advertises_geometry_and_zero_is_identity() {
        use mondrian_core::{PropertyValue, TimelineTime, WorkingColorSpace};

        let kind = crate::EffectType::GrowBounds;
        let mut effect = crate::instantiate_effect_node(kind.clone()).unwrap();
        assert_eq!(effect.properties.iter().count(), 1);
        let evaluate = |effect: &crate::EffectNode| {
            crate::PreparedEffectProgram::prepare(
                std::slice::from_ref(effect),
                &[],
                WorkingColorSpace::LinearRec709,
            )
            .unwrap()
            .evaluate(TimelineTime::ZERO)
            .unwrap()
        };
        let source_extent = EffectFrameExtent::new(7, 5);
        let expanded = evaluate(&effect);
        let bounds = expanded.output_raster_bounds(source_extent).unwrap();
        assert_eq!(bounds.extent(), EffectFrameExtent::new(27, 25));
        assert_eq!(bounds.origin(), [-10; 2]);
        let pipeline = crate::lower_effect_graph_to_gpu_pipeline(&expanded).unwrap();
        assert!(matches!(
            pipeline.stages(),
            [crate::EffectGpuStage::Spatial { pass: crate::EffectGpuSpatialPass::GrowBounds { grade }, .. }]
                if grade.output_outset() == [10; 2]
        ));
        effect
            .set_static_value_by_parameter(
                &kind.parameter_id("pixels").unwrap(),
                PropertyValue::Float(0.0),
            )
            .unwrap();
        let identity = evaluate(&effect);
        assert!(identity.graph().is_identity());
        assert_eq!(
            identity.output_raster_bounds(source_extent).unwrap(),
            EffectRasterBounds::from_extent(source_extent)
        );
        assert_ne!(
            identity.semantic_fingerprint(),
            expanded.semantic_fingerprint()
        );
    }

    #[test]
    fn grow_bounds_chains_compose_origins_and_budget_live_allocations() {
        use crate::{
            EffectExecutionError, EffectExecutionSession, EffectExecutionSessionConfig,
            EffectGraphBuilderState, EffectRenderOp,
        };

        let mut builder = EffectGraphBuilderState::new();
        for padding in [1.0, 1.25] {
            builder.append_unary(EffectRenderOp::GrowBounds {
                grade: GrowBoundsGrade::new(padding).unwrap(),
            });
        }
        let compiled = crate::compile_reference_render_graph(builder.finish()).unwrap();
        let source = frame(
            ColorFrameAlpha::StraightCoverage,
            vec![[1.0, 0.0, 0.0, 0.5]; 2],
        );
        let bounds = compiled.output_raster_bounds(source.extent()).unwrap();
        assert_eq!(bounds.extent(), EffectFrameExtent::new(8, 7));
        assert_eq!(bounds.origin(), [-3; 2]);
        let peak =
            crate::estimate_compiled_float_raster_working_bytes(&compiled, source.extent(), &[])
                .unwrap();
        assert!(peak >= (4 * 3 + 8 * 7) * 16);
        let execute = |budget| {
            EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(budget))
                .apply_compiled_rgba_f32(
                    source.view(),
                    &compiled,
                    0,
                    &mut EffectProcessorControl::uncancelled(),
                )
        };
        assert!(
            matches!(execute(peak - 1), Err(EffectFloatExecutionError::AuxiliaryInput(
            EffectExecutionError::AuxiliaryWorkingSetBudgetExceeded { required_bytes, .. },
        )) if required_bytes == peak)
        );
        let output = execute(peak).unwrap();
        assert_eq!(output.extent(), bounds.extent());
        assert_eq!(&output.pixels()[27..29], source.pixels());
        assert!(output
            .pixels()
            .iter()
            .enumerate()
            .all(|(index, pixel)| { (27..29).contains(&index) || *pixel == [0.0; 4] }));
    }
}
