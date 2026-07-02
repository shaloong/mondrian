use crate::{ColorFrameDomain, CpuColorFrame, CpuEncodedColorFrame};
use mondrian_core::{
    convert_rgba8_in_place,
    types::{ColorEngine, ColorSpace},
    ColorPipeline, RgbaF32Frame,
};

/// Backend used to execute a render color transform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderColorTransformBackend {
    /// CPU OCIO path via an explicit RGBA8 boundary.
    CpuOcioRgba8Boundary,
}

/// Color transform requested by a render graph boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenderColorTransform {
    /// Destination color space.
    pub output_color_space: ColorSpace,
    /// Destination frame domain.
    pub output_domain: ColorFrameDomain,
    /// Whether HDR/scene data should be tone-mapped for the destination.
    pub tone_map: bool,
    /// Color engine used to execute the transform.
    pub engine: ColorEngine,
    /// Execution backend selected for this transform.
    pub backend: RenderColorTransformBackend,
}

/// Input transform requested by a decode/source boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenderInputTransform {
    /// Timeline working color space.
    pub working_color_space: ColorSpace,
    /// Whether input HDR/scene data should be tone-mapped while entering working space.
    pub tone_map: bool,
    /// Color engine used to execute the input transform.
    pub engine: ColorEngine,
    /// Execution backend selected for this transform.
    pub backend: RenderColorTransformBackend,
}

impl RenderInputTransform {
    /// Build a source/import -> timeline working-space transform.
    pub fn to_working(
        working_color_space: ColorSpace,
        tone_map: bool,
        engine: ColorEngine,
    ) -> Self {
        Self {
            working_color_space,
            tone_map,
            engine,
            backend: RenderColorTransformBackend::CpuOcioRgba8Boundary,
        }
    }
}

impl RenderColorTransform {
    /// Build a display/presentation transform.
    pub fn display(output_color_space: ColorSpace, tone_map: bool, engine: ColorEngine) -> Self {
        Self {
            output_color_space,
            output_domain: ColorFrameDomain::Display,
            tone_map,
            engine,
            backend: RenderColorTransformBackend::CpuOcioRgba8Boundary,
        }
    }

    /// Build an export/delivery transform.
    pub fn export(output_color_space: ColorSpace, tone_map: bool, engine: ColorEngine) -> Self {
        Self {
            output_color_space,
            output_domain: ColorFrameDomain::Export,
            tone_map,
            engine,
            backend: RenderColorTransformBackend::CpuOcioRgba8Boundary,
        }
    }
}

/// Execute renderer color transforms for CPU-resident frames.
pub struct CpuColorTransformExecutor;

impl CpuColorTransformExecutor {
    /// Apply a source/import transform to a typed encoded CPU frame.
    pub fn input_to_working(
        frame: &CpuEncodedColorFrame,
        transform: &RenderInputTransform,
    ) -> Result<CpuColorFrame, RenderColorTransformError> {
        let descriptor = frame.descriptor();
        if descriptor.domain != ColorFrameDomain::Source {
            return Err(RenderColorTransformError::UnsupportedInputDomain {
                domain: descriptor.domain,
            });
        }

        let mut rgba = frame.rgba().to_vec();
        convert_rgba8_in_place(
            &mut rgba,
            ColorPipeline::new(
                descriptor.color_space,
                transform.working_color_space,
                transform.working_color_space,
                transform.tone_map,
            )
            .with_engine(transform.engine.clone()),
        )
        .map_err(RenderColorTransformError::ExecutionFailed)?;

        Ok(CpuColorFrame::working(RgbaF32Frame::from_rgba8(
            descriptor.width,
            descriptor.height,
            &rgba,
            transform.working_color_space,
            transform.working_color_space,
            false,
        )))
    }

    /// Apply a render color transform to a typed CPU working frame.
    pub fn transform(
        frame: &CpuColorFrame,
        transform: &RenderColorTransform,
    ) -> Result<CpuEncodedColorFrame, RenderColorTransformError> {
        let descriptor = frame.descriptor();
        if descriptor.domain != ColorFrameDomain::Working {
            return Err(RenderColorTransformError::UnsupportedInputDomain {
                domain: descriptor.domain,
            });
        }

        let mut rgba = frame.to_output_rgba8(descriptor.color_space, false);
        convert_rgba8_in_place(
            &mut rgba,
            ColorPipeline::new(
                descriptor.color_space,
                descriptor.color_space,
                transform.output_color_space,
                transform.tone_map,
            )
            .with_engine(transform.engine.clone()),
        )
        .map_err(RenderColorTransformError::ExecutionFailed)?;

        Ok(CpuEncodedColorFrame::rgba8(
            descriptor.width,
            descriptor.height,
            transform.output_color_space,
            transform.output_domain,
            rgba,
        ))
    }
}

/// Error returned by render color transform execution.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RenderColorTransformError {
    /// The executor was asked to transform a frame from an unsupported domain.
    #[error("unsupported render color transform input domain: {domain:?}")]
    UnsupportedInputDomain {
        /// Input frame domain.
        domain: ColorFrameDomain,
    },
    /// The selected color engine failed.
    #[error("render color transform failed: {0}")]
    ExecutionFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::RgbaF32Frame;

    #[test]
    fn cpu_transform_returns_typed_display_boundary_frame() {
        let source = CpuColorFrame::working(RgbaF32Frame {
            width: 1,
            height: 1,
            data: vec![[0.5, 0.25, 0.125, 1.0]],
            color_space: ColorSpace::Rec709,
        });
        let transform =
            RenderColorTransform::display(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);

        let output =
            CpuColorTransformExecutor::transform(&source, &transform).expect("display transform");

        let descriptor = output.descriptor();
        assert_eq!(descriptor.domain, ColorFrameDomain::Display);
        assert_eq!(descriptor.color_space, ColorSpace::Srgb);
        assert_eq!(descriptor.encoding, crate::ColorFrameEncoding::EncodedRgba8);
        assert_eq!(output.rgba().len(), 4);
    }

    #[test]
    fn input_transform_returns_typed_working_frame() {
        let source =
            CpuEncodedColorFrame::source_rgba8(1, 1, ColorSpace::Rec709, vec![128, 64, 32, 255]);
        let transform =
            RenderInputTransform::to_working(ColorSpace::Srgb, false, ColorEngine::MondrianSmart);

        let working = CpuColorTransformExecutor::input_to_working(&source, &transform)
            .expect("input transform");

        let descriptor = working.descriptor();
        assert_eq!(descriptor.domain, ColorFrameDomain::Working);
        assert_eq!(descriptor.color_space, ColorSpace::Srgb);
        assert_eq!(descriptor.encoding, crate::ColorFrameEncoding::LinearFloat);
        assert_eq!(working.rgba_f32().data.len(), 1);
    }
}
