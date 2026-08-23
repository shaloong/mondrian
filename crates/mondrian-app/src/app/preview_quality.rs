//! UI-independent preview quality normalization.
//!
//! Author mutations, Window presentation, and Headless execution must resolve
//! the same legal runtime scale instead of maintaining parallel clamp rules.

use mondrian_core::Resolution;
use mondrian_media::PreviewRepresentationQuality;
use mondrian_playback::PreviewResolutionScale;

/// Normalize an authored preview resolution scale for execution.
pub(crate) fn normalize_preview_resolution_scale(scale: f32) -> f32 {
    if scale.is_finite() {
        scale.clamp(0.125, 1.0)
    } else {
        0.5
    }
}

/// Resolve one Sequence's execution resolution from authored and runtime quality.
///
/// Every Sequence keeps its own logical canvas. Nested execution applies the
/// same runtime divisor to that canvas before the child is projected into its
/// parent; inheriting the parent's raster size would change sampling semantics.
pub(crate) fn preview_execution_resolution(
    logical_resolution: Resolution,
    authored_scale: f32,
    runtime_scale: PreviewResolutionScale,
) -> Resolution {
    let authored_scale = normalize_preview_resolution_scale(authored_scale);
    let divisor = runtime_scale.dimension_divisor();
    Resolution {
        width: ((logical_resolution.width as f32 * authored_scale).round() as u32)
            .max(1)
            .div_ceil(divisor),
        height: ((logical_resolution.height as f32 * authored_scale).round() as u32)
            .max(1)
            .div_ceil(divisor),
    }
}

/// Project Playback's runtime-only spatial recovery choice into the media
/// representation requested for CPU-addressable Preview decode.
///
/// This mapping is deliberately independent of the authored Viewer scale and
/// output extent. Playback owns the temporary quality revision; media owns the
/// resulting source-raster representation identity.
pub(crate) fn preview_representation_quality(
    runtime_scale: PreviewResolutionScale,
) -> PreviewRepresentationQuality {
    let divisor = runtime_scale.dimension_divisor();
    if divisor == 1 {
        PreviewRepresentationQuality::Full
    } else {
        std::num::NonZeroU32::new(divisor).map_or(PreviewRepresentationQuality::Full, |divisor| {
            PreviewRepresentationQuality::Reduced { divisor }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_has_one_bounded_fail_safe_contract() {
        assert_eq!(normalize_preview_resolution_scale(0.0), 0.125);
        assert_eq!(normalize_preview_resolution_scale(0.25), 0.25);
        assert_eq!(normalize_preview_resolution_scale(2.0), 1.0);
        assert_eq!(normalize_preview_resolution_scale(f32::NAN), 0.5);
        assert_eq!(normalize_preview_resolution_scale(f32::INFINITY), 0.5);
    }

    #[test]
    fn execution_resolution_combines_authored_and_runtime_quality() {
        let logical = Resolution { width: 1920, height: 1080 };
        assert_eq!(
            preview_execution_resolution(logical, 0.5, PreviewResolutionScale::Full),
            Resolution { width: 960, height: 540 }
        );
        assert_eq!(
            preview_execution_resolution(logical, 0.5, PreviewResolutionScale::Half),
            Resolution { width: 480, height: 270 }
        );
        assert_eq!(
            preview_execution_resolution(logical, 0.5, PreviewResolutionScale::Quarter),
            Resolution { width: 240, height: 135 }
        );
    }

    #[test]
    fn runtime_scale_maps_to_media_representation_without_output_extent() {
        assert_eq!(
            preview_representation_quality(PreviewResolutionScale::Full),
            PreviewRepresentationQuality::Full
        );
        assert_eq!(
            preview_representation_quality(PreviewResolutionScale::Half),
            PreviewRepresentationQuality::Reduced {
                divisor: std::num::NonZeroU32::new(2).expect("test divisor"),
            }
        );
        assert_eq!(
            preview_representation_quality(PreviewResolutionScale::Quarter),
            PreviewRepresentationQuality::Reduced {
                divisor: std::num::NonZeroU32::new(4).expect("test divisor"),
            }
        );
    }
}
