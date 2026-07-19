//! UI-independent final raster contract for Preview presentation.
//!
//! Preview execution and residency retain this value. A concrete presentation
//! Adapter may translate it into a Widget image, a headless readback, or another
//! frontend payload without changing cache or stale-reuse semantics.

use std::sync::Arc;

use mondrian_core::types::{ColorSpace, SequenceId};
use mondrian_renderer::RenderMonitorAdaptation;
use mondrian_timeline::sequence::ColorContext;

use super::preview_execution::PreviewOutputKey;

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

/// Resolve the final CPU raster contract from the authored Program Output.
pub(crate) fn preview_raster_presentation_contract(
    requested: &ColorContext,
) -> Result<PreviewRasterPresentationContract, String> {
    let program_output = requested.output_color_space.color().ok_or_else(|| {
        format!(
            "Program Output {:?} is not an encoded color identity",
            requested.output_color_space
        )
    })?;
    RenderMonitorAdaptation::new(program_output, ColorSpace::Srgb, requested.engine.clone())
        .map_err(|error| error.to_string())?;
    Ok(PreviewRasterPresentationContract { color_space: PreviewRasterColorSpace::Srgb })
}

/// Stable presentation-resource identity for a cacheable Preview raster.
pub(crate) fn preview_raster_resource_key(output: &PreviewOutputKey) -> String {
    format!(
        "preview.raster:{}:{}x{}:{:016x}",
        output.sequence_id, output.width, output.height, output.plan_signature
    )
}

/// Stable presentation-resource identity for an uncached Preview raster.
pub(crate) fn uncached_preview_raster_resource_key(
    sequence_id: SequenceId,
    frame: i64,
    width: u32,
    height: u32,
) -> String {
    format!(
        "preview.raster-uncached:{sequence_id}:{width}x{height}:f{}",
        frame.max(0)
    )
}

impl PreviewRasterFrame {
    /// Validate and construct one final Preview raster.
    pub(crate) fn new(
        resource_key: impl Into<String>,
        width: u32,
        height: u32,
        color_space: PreviewRasterColorSpace,
        rgba: impl Into<Arc<[u8]>>,
    ) -> Option<Self> {
        let rgba = rgba.into();
        let expected = width.checked_mul(height)?.checked_mul(4)? as usize;
        (width > 0 && height > 0 && rgba.len() == expected).then(|| Self {
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
        assert!(PreviewRasterFrame::new(
            "zero",
            0,
            1,
            PreviewRasterColorSpace::Srgb,
            Vec::<u8>::new(),
        )
        .is_none());
        assert!(
            PreviewRasterFrame::new("short", 2, 1, PreviewRasterColorSpace::Srgb, vec![0; 7],)
                .is_none()
        );
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
