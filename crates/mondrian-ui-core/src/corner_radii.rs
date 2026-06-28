//! Per-corner border radii for rounded rectangles.
//!
//! CSS-style independent corner control. The renderer's fragment shader uses
//! the quadrant of the pixel coordinate to select the appropriate radius when
//! computing the rounded-rect SDF.

/// Four independent corner radii in CSS order.
///
/// Field order: top-left, top-right, bottom-right, bottom-left.
/// The fragment shader receives this as `vec4<f32>` with the same layout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CornerRadii {
    pub top_left: f32,
    pub top_right: f32,
    pub bottom_right: f32,
    pub bottom_left: f32,
}

impl CornerRadii {
    /// All four corners set to zero (sharp rectangle).
    pub const ZERO: Self = Self {
        top_left: 0.0,
        top_right: 0.0,
        bottom_right: 0.0,
        bottom_left: 0.0,
    };

    /// All four corners set to the same radius.
    pub fn all(radius: f32) -> Self {
        Self {
            top_left: radius,
            top_right: radius,
            bottom_right: radius,
            bottom_left: radius,
        }
    }

    /// Pack into `[top_left, top_right, bottom_right, bottom_left]` for the GPU.
    pub fn as_array(self) -> [f32; 4] {
        [
            self.top_left,
            self.top_right,
            self.bottom_right,
            self.bottom_left,
        ]
    }

    /// Whether every corner is zero (no rounding at all). Used for the
    /// fragment shader fast-path skip.
    pub fn is_zero(self) -> bool {
        self.top_left == 0.0
            && self.top_right == 0.0
            && self.bottom_right == 0.0
            && self.bottom_left == 0.0
    }

    /// CSS-like normalization: scales all radii proportionally so that no
    /// pair of adjacent radii exceeds the relevant side length.
    ///
    /// Returns `None` if the dimensions are non-positive, signalling that the
    /// caller should skip the shape.
    pub fn normalize(self, width: f32, height: f32) -> Option<Self> {
        if !(width > 0.0 && height > 0.0) {
            return None;
        }

        let sum_horiz_top = self.top_left + self.top_right;
        let sum_horiz_bot = self.bottom_left + self.bottom_right;
        let sum_vert_left = self.top_left + self.bottom_left;
        let sum_vert_right = self.top_right + self.bottom_right;

        let mut scale = 1.0f32;
        if sum_horiz_top > 0.0 {
            scale = scale.min(width / sum_horiz_top);
        }
        if sum_horiz_bot > 0.0 {
            scale = scale.min(width / sum_horiz_bot);
        }
        if sum_vert_left > 0.0 {
            scale = scale.min(height / sum_vert_left);
        }
        if sum_vert_right > 0.0 {
            scale = scale.min(height / sum_vert_right);
        }
        scale = scale.min(1.0);

        Some(Self {
            top_left: self.top_left * scale,
            top_right: self.top_right * scale,
            bottom_right: self.bottom_right * scale,
            bottom_left: self.bottom_left * scale,
        })
    }

    /// Maximum individual corner value.
    pub fn max_radius(self) -> f32 {
        self.top_left.max(self.top_right).max(self.bottom_right).max(self.bottom_left)
    }
}

impl From<f32> for CornerRadii {
    fn from(radius: f32) -> Self {
        Self::all(radius)
    }
}

impl Default for CornerRadii {
    fn default() -> Self {
        Self::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_sets_every_corner() {
        let r = CornerRadii::all(8.0);
        assert_eq!(r.top_left, 8.0);
        assert_eq!(r.top_right, 8.0);
        assert_eq!(r.bottom_right, 8.0);
        assert_eq!(r.bottom_left, 8.0);
    }

    #[test]
    fn zero_is_all_zero() {
        assert!(CornerRadii::ZERO.is_zero());
    }

    #[test]
    fn is_zero_detects_non_zero_corner() {
        let r = CornerRadii { top_left: 1.0, ..CornerRadii::ZERO };
        assert!(!r.is_zero());
    }

    #[test]
    fn from_f32_converts_to_all() {
        let r: CornerRadii = 4.0.into();
        assert_eq!(r, CornerRadii::all(4.0));
    }

    #[test]
    fn normalize_scales_proportionally_when_sum_exceeds_width() {
        let r = CornerRadii {
            top_left: 60.0,
            top_right: 60.0,
            bottom_right: 0.0,
            bottom_left: 0.0,
        };
        let n = r.normalize(100.0, 200.0).unwrap();
        let expected = 100.0 / 120.0 * 60.0; // = 50.0
        assert!((n.top_left - expected).abs() < 0.001);
        assert!((n.top_right - expected).abs() < 0.001);
        assert_eq!(n.bottom_right, 0.0);
    }

    #[test]
    fn normalize_does_nothing_when_radii_fit() {
        let r = CornerRadii::all(10.0);
        let n = r.normalize(100.0, 100.0).unwrap();
        assert_eq!(n, r);
    }

    #[test]
    fn normalize_clamps_to_1_when_smaller() {
        let r = CornerRadii { top_left: 200.0, ..CornerRadii::ZERO };
        let n = r.normalize(100.0, 100.0).unwrap();
        assert_eq!(n.top_left, 100.0);
    }
}
