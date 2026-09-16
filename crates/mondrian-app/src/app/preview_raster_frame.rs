//! UI-independent final raster contract for Preview presentation.
//!
//! Preview execution and residency retain this value. A concrete presentation
//! Adapter may translate it into a Widget image, a headless readback, or another
//! frontend payload without changing cache or stale-reuse semantics.

use std::sync::Arc;

use mondrian_core::types::ColorSpace;
use mondrian_core::OcioColorSpaceIdentity;
use mondrian_renderer::{RenderMonitorAdaptation, RenderMonitorAdaptationError};
use mondrian_timeline::sequence::ProgramColorContext;

use super::preview_execution::PreviewOutputKey;
use super::preview_unavailability::{PreviewOutputStage, PreviewUnavailability};

/// Encoded color identity of a final Preview RGBA8 raster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PreviewRasterColorSpace {
    /// IEC 61966-2-1 sRGB encoded RGB values.
    Srgb,
}

/// Validated, UI-independent final Preview raster.
#[derive(Debug, Clone)]
pub(crate) struct PreviewRasterFrame {
    /// Stable identity for presentation-resource reuse.
    pub(crate) resource_key: String,
    /// Raster width in pixels.
    pub(crate) width: u32,
    /// Raster height in pixels.
    pub(crate) height: u32,
    /// Color space in which the RGBA8 bytes are encoded.
    pub(crate) color_space: PreviewRasterColorSpace,
    /// Encoded RGBA8 pixels, row-major, `width * height * 4` bytes.
    pub(crate) rgba: Arc<[u8]>,
}

/// UI-independent encoding contract for the final CPU Preview raster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreviewRasterPresentationContract {
    /// Encoded color identity required at the raster boundary.
    pub(crate) color_space: PreviewRasterColorSpace,
}

/// Failure to resolve the final CPU raster presentation contract.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PreviewRasterPresentationContractError {
    #[error("Program Output {identity:?} is not an encoded color identity")]
    ProgramOutputIdentity { identity: OcioColorSpaceIdentity },
    #[error("CPU raster monitor adaptation is blocked: {0}")]
    MonitorAdaptation(#[from] RenderMonitorAdaptationError),
}

impl PreviewRasterPresentationContractError {
    /// Project this contract failure into the shared terminal Preview contract.
    pub(crate) fn unavailability(&self) -> PreviewUnavailability {
        let stage = match self {
            Self::ProgramOutputIdentity { .. } => PreviewOutputStage::ProgramOutput,
            Self::MonitorAdaptation(_) => PreviewOutputStage::MonitorAdaptation,
        };
        PreviewUnavailability::blocked(stage, self.to_string())
    }
}

/// Failure to construct a validated final Preview raster.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PreviewRasterFrameError {
    #[error("Preview raster extent must be non-zero, got {width}x{height}")]
    EmptyExtent { width: u32, height: u32 },
    #[error("Preview raster byte extent overflow for {width}x{height}")]
    ByteExtentOverflow { width: u32, height: u32 },
    #[error("Preview raster payload length mismatch: expected {expected}, actual {actual}")]
    PayloadLengthMismatch { expected: usize, actual: usize },
}

/// Resolve the final CPU raster contract from the authored Program Output.
pub(crate) fn preview_raster_presentation_contract(
    requested: &ProgramColorContext,
) -> Result<PreviewRasterPresentationContract, PreviewRasterPresentationContractError> {
    let program_output = requested.output_color_space().color().ok_or({
        PreviewRasterPresentationContractError::ProgramOutputIdentity {
            identity: requested.output_color_space(),
        }
    })?;
    RenderMonitorAdaptation::new(program_output, ColorSpace::Srgb, requested.engine().clone())
        .map_err(PreviewRasterPresentationContractError::from)?;
    Ok(PreviewRasterPresentationContract { color_space: PreviewRasterColorSpace::Srgb })
}

/// Stable presentation-resource identity for a cacheable Preview raster.
pub(crate) fn preview_raster_resource_key(output: &PreviewOutputKey) -> String {
    format!("preview.raster:{output}")
}

impl PreviewRasterFrame {
    /// Validate and construct one final Preview raster.
    pub(crate) fn new(
        resource_key: impl Into<String>,
        width: u32,
        height: u32,
        color_space: PreviewRasterColorSpace,
        rgba: impl Into<Arc<[u8]>>,
    ) -> Result<Self, PreviewRasterFrameError> {
        let rgba = rgba.into();
        if width == 0 || height == 0 {
            return Err(PreviewRasterFrameError::EmptyExtent { width, height });
        }
        let expected = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height).ok().and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(PreviewRasterFrameError::ByteExtentOverflow { width, height })?;
        if rgba.len() != expected {
            return Err(PreviewRasterFrameError::PayloadLengthMismatch {
                expected,
                actual: rgba.len(),
            });
        }
        Ok(Self {
            resource_key: resource_key.into(),
            width,
            height,
            color_space,
            rgba,
        })
    }

    /// Exact host-memory reservation used by the Preview Frame Store.
    pub(crate) fn reserved_bytes(&self) -> usize {
        self.rgba.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raster_contract_rejects_invalid_extent_or_payload_length() {
        assert!(matches!(
            PreviewRasterFrame::new(
                "zero",
                0,
                1,
                PreviewRasterColorSpace::Srgb,
                Vec::<u8>::new(),
            ),
            Err(PreviewRasterFrameError::EmptyExtent { width: 0, height: 1 })
        ));
        assert!(matches!(
            PreviewRasterFrame::new("short", 2, 1, PreviewRasterColorSpace::Srgb, vec![0; 7],),
            Err(PreviewRasterFrameError::PayloadLengthMismatch { expected: 8, actual: 7 })
        ));
    }

    #[test]
    fn raster_contract_preserves_identity_color_and_exact_reservation() {
        let frame =
            PreviewRasterFrame::new("preview:1", 2, 1, PreviewRasterColorSpace::Srgb, vec![0; 8])
                .expect("valid raster");

        assert_eq!(frame.resource_key, "preview:1");
        assert_eq!(frame.color_space, PreviewRasterColorSpace::Srgb);
        assert_eq!(frame.reserved_bytes(), 8);
    }
}
