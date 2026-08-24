//! Exact picture scan and display-geometry contracts.
//!
//! Encoded raster extent, sample aspect ratio, source orientation, and scan
//! order are one interpretation problem.  Keeping them in one deep Module
//! prevents Preview, Proxy, Viewer, and Export Adapters from independently
//! guessing how stored samples become authored display geometry.

use crate::timeline_data::{FieldOrder, PixelAspectRatio};
use crate::Resolution;
use serde::{Deserialize, Serialize};

/// Exact horizontal-to-vertical size ratio of one encoded picture sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleAspectRatio {
    numerator: u32,
    denominator: u32,
}

impl SampleAspectRatio {
    /// Square pixels.
    pub const SQUARE: Self = Self { numerator: 1, denominator: 1 };

    /// Construct a positive, reduced sample aspect ratio.
    pub fn new(numerator: u32, denominator: u32) -> Option<Self> {
        if numerator == 0 || denominator == 0 {
            return None;
        }
        let divisor = gcd_u32(numerator, denominator);
        Some(Self {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        })
    }

    /// Horizontal sample-size numerator.
    pub const fn numerator(self) -> u32 {
        self.numerator
    }

    /// Vertical sample-size denominator.
    pub const fn denominator(self) -> u32 {
        self.denominator
    }

    /// Exact ratio projected to floating point only at a spatial execution seam.
    pub fn to_f64(self) -> f64 {
        f64::from(self.numerator) / f64::from(self.denominator)
    }
}

impl Default for SampleAspectRatio {
    fn default() -> Self {
        Self::SQUARE
    }
}

const fn gcd_u32(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

/// Cardinal source-display transform represented by common media metadata.
///
/// The two diagonal reflections use the conventional EXIF names. Arbitrary
/// affine display matrices remain explicitly unsupported rather than being
/// approximated as a nearby rotation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PictureOrientation {
    /// Encoded rows and columns already have display orientation.
    #[default]
    Identity,
    /// Rotate the encoded picture 90 degrees clockwise for display.
    RotateClockwise90,
    /// Rotate the encoded picture 180 degrees for display.
    Rotate180,
    /// Rotate the encoded picture 270 degrees clockwise for display.
    RotateClockwise270,
    /// Reflect the displayed picture across its vertical axis.
    MirrorHorizontal,
    /// Reflect the displayed picture across its horizontal axis.
    MirrorVertical,
    /// Reflect across the top-left to bottom-right diagonal.
    Transpose,
    /// Reflect across the top-right to bottom-left diagonal.
    Transverse,
    /// The source carries a non-cardinal, singular, or otherwise unsupported matrix.
    Unsupported,
}

impl PictureOrientation {
    /// Whether display width and height exchange axes.
    pub const fn swaps_axes(self) -> bool {
        matches!(
            self,
            Self::RotateClockwise90 | Self::RotateClockwise270 | Self::Transpose | Self::Transverse
        )
    }

    fn orientation_affine(self, width: f64, height: f64) -> Option<[f64; 6]> {
        let affine = match self {
            Self::Identity => [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            Self::RotateClockwise90 => [0.0, -1.0, height, 1.0, 0.0, 0.0],
            Self::Rotate180 => [-1.0, 0.0, width, 0.0, -1.0, height],
            Self::RotateClockwise270 => [0.0, 1.0, 0.0, -1.0, 0.0, width],
            Self::MirrorHorizontal => [-1.0, 0.0, width, 0.0, 1.0, 0.0],
            Self::MirrorVertical => [1.0, 0.0, 0.0, 0.0, -1.0, height],
            Self::Transpose => [0.0, 1.0, 0.0, 1.0, 0.0, 0.0],
            Self::Transverse => [0.0, -1.0, height, -1.0, 0.0, width],
            Self::Unsupported => return None,
        };
        Some(affine)
    }
}

/// Immutable picture interpretation facts captured by one media probe.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PictureStreamMetadata {
    /// Stream-declared sample aspect ratio; absence means unspecified.
    #[serde(default)]
    pub sample_aspect_ratio: Option<SampleAspectRatio>,
    /// Stream-declared scan order; absence means unspecified.
    #[serde(default)]
    pub field_order: Option<FieldOrder>,
    /// Source display orientation derived from the stream display matrix.
    #[serde(default)]
    pub orientation: PictureOrientation,
}

/// Placement-local picture interpretation overrides authored on one media Clip.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PictureInterpretationOverrides {
    /// Explicit sample aspect preset, or `None` to follow source metadata.
    pub pixel_aspect_ratio: Option<PixelAspectRatio>,
    /// Explicit scan order, or `None` to follow source metadata.
    pub field_order: Option<FieldOrder>,
}

/// One fully resolved progressive picture geometry used by execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResolvedPictureGeometry {
    encoded_resolution: Resolution,
    sample_aspect_ratio: SampleAspectRatio,
    orientation: PictureOrientation,
}

impl ResolvedPictureGeometry {
    /// Resolve probe facts plus placement-local author overrides.
    ///
    /// Unknown PAR falls back to square pixels, matching ordinary media
    /// interpretation. Non-progressive scan and unsupported display matrices
    /// fail closed because the current pixel pipeline cannot execute them
    /// without changing temporal or spatial semantics.
    pub fn resolve(
        encoded_resolution: Resolution,
        metadata: PictureStreamMetadata,
        pixel_aspect_ratio_override: Option<PixelAspectRatio>,
        field_order_override: Option<FieldOrder>,
    ) -> Result<Self, PictureInterpretationError> {
        if encoded_resolution.width == 0 || encoded_resolution.height == 0 {
            return Err(PictureInterpretationError::EmptyRaster(encoded_resolution));
        }
        let sample_aspect_ratio = match pixel_aspect_ratio_override {
            Some(override_value) => override_value
                .exact_ratio()
                .ok_or(PictureInterpretationError::UnknownPixelAspectRatio)?,
            None => metadata.sample_aspect_ratio.unwrap_or(SampleAspectRatio::SQUARE),
        };
        let field_order =
            field_order_override.or(metadata.field_order).unwrap_or(FieldOrder::Progressive);
        if field_order != FieldOrder::Progressive {
            return Err(PictureInterpretationError::InterlacedUnsupported(
                field_order,
            ));
        }
        if metadata.orientation == PictureOrientation::Unsupported {
            return Err(PictureInterpretationError::UnsupportedOrientation);
        }
        Ok(Self {
            encoded_resolution,
            sample_aspect_ratio,
            orientation: metadata.orientation,
        })
    }

    /// Resolve probe facts plus one closed placement-local override value.
    pub fn resolve_with_overrides(
        encoded_resolution: Resolution,
        metadata: PictureStreamMetadata,
        overrides: PictureInterpretationOverrides,
    ) -> Result<Self, PictureInterpretationError> {
        Self::resolve(
            encoded_resolution,
            metadata,
            overrides.pixel_aspect_ratio,
            overrides.field_order,
        )
    }

    /// Identity geometry for generated or already materialized square-pixel pictures.
    pub fn square(encoded_resolution: Resolution) -> Result<Self, PictureInterpretationError> {
        Self::resolve(
            encoded_resolution,
            PictureStreamMetadata::default(),
            None,
            None,
        )
    }

    /// Encoded source raster extent.
    pub const fn encoded_resolution(self) -> Resolution {
        self.encoded_resolution
    }

    /// Exact effective sample aspect ratio.
    pub const fn sample_aspect_ratio(self) -> SampleAspectRatio {
        self.sample_aspect_ratio
    }

    /// Effective cardinal source orientation.
    pub const fn orientation(self) -> PictureOrientation {
        self.orientation
    }

    /// Continuous display-authoring extent after SAR and source orientation.
    pub fn display_extent(self) -> [f64; 2] {
        let width = f64::from(self.encoded_resolution.width) * self.sample_aspect_ratio.to_f64();
        let height = f64::from(self.encoded_resolution.height);
        if self.orientation.swaps_axes() {
            [height, width]
        } else {
            [width, height]
        }
    }

    /// Display aspect ratio used by Viewer layout and delivery validation.
    pub fn display_aspect_ratio(self) -> f64 {
        let [width, height] = self.display_extent();
        width / height
    }

    /// Affine conversion from encoded pixel coordinates to display-authoring coordinates.
    pub fn source_to_display_affine(self) -> [f32; 6] {
        let sample_width =
            f64::from(self.encoded_resolution.width) * self.sample_aspect_ratio.to_f64();
        let sample_height = f64::from(self.encoded_resolution.height);
        let pixel_shape = [self.sample_aspect_ratio.to_f64(), 0.0, 0.0, 0.0, 1.0, 0.0];
        let orientation = self
            .orientation
            .orientation_affine(sample_width, sample_height)
            .expect("resolved picture geometry excludes unsupported orientation");
        compose_affine_f64(orientation, pixel_shape).map(|value| value as f32)
    }
}

/// Typed rejection for picture interpretation that current execution cannot honor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PictureInterpretationError {
    /// Encoded raster has no pixels.
    #[error("picture raster must be non-empty, got {0}")]
    EmptyRaster(Resolution),
    /// `Unknown` is diagnostic state and cannot be an explicit author override.
    #[error("explicit pixel aspect ratio cannot be Unknown")]
    UnknownPixelAspectRatio,
    /// Interlaced sources require a real deinterlacing execution path.
    #[error("{0:?} scan is unsupported until a deinterlacing execution path is admitted")]
    InterlacedUnsupported(FieldOrder),
    /// Arbitrary source display matrices cannot be approximated safely.
    #[error("source display matrix is not a supported cardinal orientation")]
    UnsupportedOrientation,
}

/// Compose two row-major 2D affines, returning `outer(inner(point))`.
pub fn compose_picture_affine(outer: [f32; 6], inner: [f32; 6]) -> Option<[f32; 6]> {
    if outer.iter().chain(inner.iter()).any(|value| !value.is_finite()) {
        return None;
    }
    let composed = [
        outer[0] * inner[0] + outer[1] * inner[3],
        outer[0] * inner[1] + outer[1] * inner[4],
        outer[0] * inner[2] + outer[1] * inner[5] + outer[2],
        outer[3] * inner[0] + outer[4] * inner[3],
        outer[3] * inner[1] + outer[4] * inner[4],
        outer[3] * inner[2] + outer[4] * inner[5] + outer[5],
    ];
    composed.iter().all(|value| value.is_finite()).then_some(composed)
}

fn compose_affine_f64(outer: [f64; 6], inner: [f64; 6]) -> [f64; 6] {
    [
        outer[0] * inner[0] + outer[1] * inner[3],
        outer[0] * inner[1] + outer[1] * inner[4],
        outer[0] * inner[2] + outer[1] * inner[5] + outer[2],
        outer[3] * inner[0] + outer[4] * inner[3],
        outer[3] * inner[1] + outer[4] * inner[4],
        outer[3] * inner[2] + outer[4] * inner[5] + outer[5],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_aspect_ratio_is_positive_and_reduced() {
        assert_eq!(
            SampleAspectRatio::new(40, 44),
            SampleAspectRatio::new(10, 11)
        );
        assert_eq!(SampleAspectRatio::new(0, 1), None);
        assert_eq!(SampleAspectRatio::new(1, 0), None);
    }

    #[test]
    fn rotated_anamorphic_geometry_swaps_display_axes() {
        let geometry = ResolvedPictureGeometry::resolve(
            Resolution { width: 720, height: 480 },
            PictureStreamMetadata {
                sample_aspect_ratio: SampleAspectRatio::new(40, 33),
                field_order: Some(FieldOrder::Progressive),
                orientation: PictureOrientation::RotateClockwise90,
            },
            None,
            None,
        )
        .expect("supported geometry");

        let [width, height] = geometry.display_extent();
        assert!((width - 480.0).abs() < 1.0e-9);
        assert!((height - 720.0 * 40.0 / 33.0).abs() < 1.0e-9);
        let affine = geometry.source_to_display_affine();
        assert!((affine[1] + 1.0).abs() < 1.0e-6);
        assert!((affine[2] - 480.0).abs() < 1.0e-6);
        assert!((affine[3] - 40.0 / 33.0).abs() < 1.0e-6);
    }

    #[test]
    fn interlaced_and_unknown_override_contracts_fail_closed() {
        let resolution = Resolution::FHD;
        assert_eq!(
            ResolvedPictureGeometry::resolve(
                resolution,
                PictureStreamMetadata {
                    field_order: Some(FieldOrder::UpperFirst),
                    ..PictureStreamMetadata::default()
                },
                None,
                None,
            ),
            Err(PictureInterpretationError::InterlacedUnsupported(
                FieldOrder::UpperFirst
            ))
        );
        assert_eq!(
            ResolvedPictureGeometry::resolve(
                resolution,
                PictureStreamMetadata::default(),
                Some(PixelAspectRatio::Unknown),
                None,
            ),
            Err(PictureInterpretationError::UnknownPixelAspectRatio)
        );
    }

    #[test]
    fn affine_composition_applies_picture_interpretation_before_clip_transform() {
        let clip = [2.0, 0.0, 10.0, 0.0, 3.0, 20.0];
        let picture = [0.0, -1.0, 480.0, 1.0, 0.0, 0.0];
        assert_eq!(
            compose_picture_affine(clip, picture),
            Some([0.0, -2.0, 970.0, 3.0, 0.0, 20.0])
        );
    }
}
