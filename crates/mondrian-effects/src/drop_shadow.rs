//! Drop shadow built-in effect.
//!
//! The drop shadow offsets the source coverage, blurs it, tints it with the authored
//! color, and composites the result underneath the source. Coverage outside the frame is
//! treated as transparent, so an offset shadow leaves the frame cleanly instead of
//! repeating the border.
//!
//! Every value in this module is straight-alpha scene-linear RGBA, matching the working
//! seam of the effect graph. The shadow color follows the same convention as the other
//! color parameters in this crate (the authored `Color` components are working-domain
//! values, not display-encoded sRGB).

use mondrian_core::Color;

/// The largest distance accepted from authoring, in pixels.
pub const DROP_SHADOW_AUTHOR_MAX_DISTANCE_PIXELS: f32 = 4_000.0;
/// The largest softness accepted from authoring, in pixels.
pub const DROP_SHADOW_AUTHOR_MAX_SOFTNESS_PIXELS: f32 = 30_000.0;
/// The largest opacity accepted from authoring, in 0..=255 units.
pub const DROP_SHADOW_AUTHOR_MAX_OPACITY: f32 = 255.0;
/// Number of box blur passes used to approximate the shadow gaussian.
const DROP_SHADOW_BLUR_PASSES: u32 = 3;
/// Radius below this value skips the blur entirely.
const SOFTNESS_EPSILON: f32 = 0.5;
/// Halo beyond this many pixels degrades to a full-frame read.
const DROP_SHADOW_MAX_HALO_PIXELS: u32 = 4_096;

/// A validated drop shadow configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DropShadowGrade {
    color: [f32; 3],
    opacity: f32,
    direction_degrees: f32,
    distance: f32,
    softness: f32,
    shadow_only: bool,
}

impl DropShadowGrade {
    /// Validate authored controls.
    ///
    /// * `opacity` is authored in 0..=255 units and stored as a 0..=1 fraction.
    /// * `direction_degrees` is a compass bearing: 0 points up, 90 points right, and 135
    ///   points down-right.
    /// * `distance` and `softness` are pixels.
    pub fn new(
        color: Color,
        opacity: f32,
        direction_degrees: f32,
        distance: f32,
        softness: f32,
        shadow_only: bool,
    ) -> Result<Self, DropShadowError> {
        let rgb = [color.r, color.g, color.b];
        if rgb.iter().any(|channel| !channel.is_finite()) {
            return Err(DropShadowError::Color);
        }
        if !opacity.is_finite() || !(0.0..=DROP_SHADOW_AUTHOR_MAX_OPACITY).contains(&opacity) {
            return Err(DropShadowError::Opacity);
        }
        if !direction_degrees.is_finite() {
            return Err(DropShadowError::Direction);
        }
        if !distance.is_finite()
            || !(0.0..=DROP_SHADOW_AUTHOR_MAX_DISTANCE_PIXELS).contains(&distance)
        {
            return Err(DropShadowError::Distance);
        }
        if !softness.is_finite()
            || !(0.0..=DROP_SHADOW_AUTHOR_MAX_SOFTNESS_PIXELS).contains(&softness)
        {
            return Err(DropShadowError::Softness);
        }
        Ok(Self {
            color: rgb,
            opacity: opacity / DROP_SHADOW_AUTHOR_MAX_OPACITY,
            direction_degrees: direction_degrees.rem_euclid(360.0),
            distance,
            softness,
            shadow_only,
        })
    }

    /// The validated shadow color as working-domain RGB.
    pub const fn color(self) -> [f32; 3] {
        self.color
    }

    /// The shadow opacity as a 0..=1 fraction.
    pub const fn opacity(self) -> f32 {
        self.opacity
    }

    /// The shadow offset direction in degrees.
    pub const fn direction_degrees(self) -> f32 {
        self.direction_degrees
    }

    /// The shadow offset distance in pixels.
    pub const fn distance(self) -> f32 {
        self.distance
    }

    /// The shadow softness in pixels.
    pub const fn softness(self) -> f32 {
        self.softness
    }

    /// Whether only the shadow is kept.
    pub const fn shadow_only(self) -> bool {
        self.shadow_only
    }

    /// The pixel offset applied to the shadow, as `(dx, dy)` in frame coordinates.
    pub fn offset_pixels(self) -> (f32, f32) {
        let radians = self.direction_degrees.to_radians();
        (
            radians.sin() * self.distance,
            -radians.cos() * self.distance,
        )
    }

    /// The box blur radius used to soften the shadow.
    pub fn blur_radius(self) -> f32 {
        if self.softness <= SOFTNESS_EPSILON {
            0.0
        } else {
            self.softness * 0.5
        }
    }

    /// Whether the grade leaves the frame untouched.
    pub fn is_identity(self) -> bool {
        !self.shadow_only && self.opacity <= f32::EPSILON
    }

    /// The ROI halo the shadow reads beyond the frame, or `None` when it is unbounded.
    pub fn input_halo(self) -> Option<(u32, u32)> {
        let distance = self.distance.ceil() as u32;
        let radius = self.blur_radius();
        let (horizontal, vertical) = crate::box_blur::box_blur_axis_halo(
            radius,
            DROP_SHADOW_BLUR_PASSES,
            crate::BoxBlurDirection::HorizontalAndVertical,
        )?;
        let horizontal = horizontal.checked_add(distance)?;
        let vertical = vertical.checked_add(distance)?;
        if horizontal > DROP_SHADOW_MAX_HALO_PIXELS || vertical > DROP_SHADOW_MAX_HALO_PIXELS {
            return None;
        }
        Some((horizontal, vertical))
    }
}

/// Drop shadow authoring errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DropShadowError {
    /// The shadow color had a non-finite component.
    #[error("drop shadow color must be finite")]
    Color,
    /// The opacity was outside 0..=255.
    #[error("drop shadow opacity must be finite and within [0, 255]")]
    Opacity,
    /// The direction was not finite.
    #[error("drop shadow direction must be finite")]
    Direction,
    /// The distance was outside 0..=4000.
    #[error("drop shadow distance must be finite and within [0, 4000] pixels")]
    Distance,
    /// The softness was outside 0..=30000.
    #[error("drop shadow softness must be finite and within [0, 30000] pixels")]
    Softness,
}

/// Composite a drop shadow underneath (or instead of) the source frame.
///
/// Returns `false` when the frame cannot be processed (invalid geometry) so the caller can
/// report a typed execution error.
pub(crate) fn apply_drop_shadow_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: DropShadowGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if width == 0 || height == 0 || working.len() < width * height {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }

    let (dx, dy) = grade.offset_pixels();
    let mut shadow = vec![[0.0_f32; 4]; width * height];
    for y in 0..height {
        checkpoint()?;
        for x in 0..width {
            let coverage = sample_coverage(working, width, height, x as f32 - dx, y as f32 - dy);
            let alpha = coverage * grade.opacity();
            shadow[y * width + x] = [grade.color()[0], grade.color()[1], grade.color()[2], alpha];
        }
    }

    let radius = grade.blur_radius();
    if radius > 0.0 {
        crate::box_blur::apply_box_blur_f32_controlled(
            &mut shadow,
            width,
            height,
            radius,
            DROP_SHADOW_BLUR_PASSES,
            crate::BoxBlurDirection::HorizontalAndVertical,
            false,
            checkpoint,
        )?;
    }

    for (pixel, shade) in working.iter_mut().zip(shadow.iter()).take(width * height) {
        let source_alpha = pixel[3].clamp(0.0, 1.0);
        let shadow_alpha = shade[3].clamp(0.0, 1.0);
        if grade.shadow_only() {
            *pixel = if shade[3] > 0.0 { *shade } else { [0.0; 4] };
            continue;
        }
        // `over`: the source sits on top of the shadow.
        let out_alpha = source_alpha + shadow_alpha * (1.0 - source_alpha);
        if out_alpha <= f32::EPSILON {
            *pixel = [0.0, 0.0, 0.0, 0.0];
            continue;
        }
        for channel in 0..3 {
            let source = pixel[channel] * source_alpha;
            let shade_value = shade[channel] * shadow_alpha * (1.0 - source_alpha);
            pixel[channel] = (source + shade_value) / out_alpha;
        }
        pixel[3] = out_alpha;
    }
    checkpoint()?;
    Ok(true)
}

/// Bilinear coverage sample with transparent surroundings.
fn sample_coverage(pixels: &[[f32; 4]], width: usize, height: usize, x: f32, y: f32) -> f32 {
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let sample = |column: f32, row: f32| -> f32 {
        if column < 0.0 || row < 0.0 {
            return 0.0;
        }
        let column = column as usize;
        let row = row as usize;
        if column >= width || row >= height {
            return 0.0;
        }
        pixels[row * width + column][3].clamp(0.0, 1.0)
    };
    let top = sample(x0, y0) * (1.0 - fx) + sample(x0 + 1.0, y0) * fx;
    let bottom = sample(x0, y0 + 1.0) * (1.0 - fx) + sample(x0 + 1.0, y0 + 1.0) * fx;
    top * (1.0 - fy) + bottom * fy
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color(r: f32, g: f32, b: f32) -> Color {
        Color { r, g, b, a: 1.0 }
    }

    fn opaque_pixel(r: f32, g: f32, b: f32) -> [f32; 4] {
        [r, g, b, 1.0]
    }

    /// Compass bearings run through `sin`/`cos`, so a ninety degree offset lands within a
    /// single f32 ulp of an integer pixel; compare with a tolerance instead of exactly.
    fn assert_close(actual: [f32; 4], expected: [f32; 4]) {
        for channel in 0..4 {
            assert!(
                (actual[channel] - expected[channel]).abs() < 1.0e-5,
                "channel {channel}: actual {actual:?} expected {expected:?}"
            );
        }
    }

    #[test]
    fn direction_uses_a_compass_bearing() {
        let right = DropShadowGrade::new(color(0.0, 0.0, 0.0), 255.0, 90.0, 4.0, 0.0, false)
            .expect("valid grade");
        let (dx, dy) = right.offset_pixels();
        assert!(dx > 3.9 && dx <= 4.0, "90 degrees points right, got {dx}");
        assert!(
            dy.abs() < 1.0e-5,
            "90 degrees has no vertical offset, got {dy}"
        );

        let down_right = DropShadowGrade::new(color(0.0, 0.0, 0.0), 255.0, 135.0, 4.0, 0.0, false)
            .expect("valid grade");
        let (dx, dy) = down_right.offset_pixels();
        assert!(
            dx > 2.0 && dy > 2.0,
            "135 degrees points down-right, got {dx}, {dy}"
        );
    }

    #[test]
    fn zero_opacity_is_identity() {
        let grade = DropShadowGrade::new(color(0.0, 0.0, 0.0), 0.0, 135.0, 10.0, 4.0, false)
            .expect("valid grade");
        assert!(grade.is_identity());

        let mut frame = vec![opaque_pixel(0.25, 0.5, 0.75)];
        assert!(
            apply_drop_shadow_f32_controlled(&mut frame, 1, 1, grade, &mut || Ok::<_, ()>(()))
                .expect("apply")
        );
        assert_eq!(frame[0], [0.25, 0.5, 0.75, 1.0]);
    }

    #[test]
    fn shadow_only_returns_the_offset_shadow() {
        let grade = DropShadowGrade::new(color(0.5, 0.25, 0.0), 255.0, 90.0, 2.0, 0.0, true)
            .expect("valid grade");
        let mut frame = vec![
            opaque_pixel(0.0, 0.0, 1.0),
            [0.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 0.0],
        ];
        assert!(
            apply_drop_shadow_f32_controlled(&mut frame, 3, 1, grade, &mut || Ok::<_, ()>(()))
                .expect("apply")
        );

        // The source coverage moves two pixels to the right, tinted by the shadow color.
        assert_eq!(frame[0], [0.0, 0.0, 0.0, 0.0]);
        assert_close(frame[2], [0.5, 0.25, 0.0, 1.0]);
    }

    #[test]
    fn shadow_composites_under_the_source() {
        let grade = DropShadowGrade::new(color(0.0, 0.0, 0.0), 255.0, 90.0, 1.0, 0.0, false)
            .expect("valid grade");
        let mut frame = vec![opaque_pixel(1.0, 1.0, 1.0), [0.0, 0.0, 0.0, 0.0]];
        assert!(
            apply_drop_shadow_f32_controlled(&mut frame, 2, 1, grade, &mut || Ok::<_, ()>(()))
                .expect("apply")
        );

        // The opaque source keeps its color; the empty pixel receives the solid shadow.
        assert_eq!(frame[0], [1.0, 1.0, 1.0, 1.0]);
        assert_close(frame[1], [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn half_opacity_shadow_keeps_partial_coverage() {
        let grade = DropShadowGrade::new(color(0.0, 0.0, 0.0), 127.5, 90.0, 1.0, 0.0, true)
            .expect("valid grade");
        let mut frame = vec![opaque_pixel(1.0, 1.0, 1.0), [0.0, 0.0, 0.0, 0.0]];
        assert!(
            apply_drop_shadow_f32_controlled(&mut frame, 2, 1, grade, &mut || Ok::<_, ()>(()))
                .expect("apply")
        );
        assert!(
            (frame[1][3] - 0.5).abs() < 1.0e-6,
            "opacity 127.5 is 0.5, got {}",
            frame[1][3]
        );
    }

    #[test]
    fn softness_spreads_coverage_without_darkening_it() {
        let hard = DropShadowGrade::new(color(0.0, 0.0, 0.0), 255.0, 90.0, 0.0, 0.0, true)
            .expect("valid grade");
        let soft = DropShadowGrade::new(color(0.0, 0.0, 0.0), 255.0, 90.0, 0.0, 4.0, true)
            .expect("valid grade");

        let mut frame = vec![
            [0.0, 0.0, 0.0, 0.0],
            opaque_pixel(1.0, 1.0, 1.0),
            [0.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 0.0],
        ];
        let mut blurred = frame.clone();
        assert!(
            apply_drop_shadow_f32_controlled(&mut frame, 5, 1, hard, &mut || Ok::<_, ()>(()))
                .expect("apply hard")
        );
        assert!(apply_drop_shadow_f32_controlled(
            &mut blurred,
            5,
            1,
            soft,
            &mut || Ok::<_, ()>(())
        )
        .expect("apply soft"));

        assert_eq!(frame[0][3], 0.0);
        assert!(
            blurred[0][3] > 0.0,
            "softness must spread coverage to the left"
        );
        assert!(
            blurred[4][3] > 0.0,
            "softness must spread coverage to the right"
        );
    }

    #[test]
    fn rejects_values_outside_the_declared_ranges() {
        assert_eq!(
            DropShadowGrade::new(color(0.0, 0.0, 0.0), 256.0, 0.0, 0.0, 0.0, false).unwrap_err(),
            DropShadowError::Opacity
        );
        assert_eq!(
            DropShadowGrade::new(color(0.0, 0.0, 0.0), 0.0, 0.0, 4_001.0, 0.0, false).unwrap_err(),
            DropShadowError::Distance
        );
        assert_eq!(
            DropShadowGrade::new(color(0.0, 0.0, 0.0), 0.0, 0.0, 0.0, 30_001.0, false).unwrap_err(),
            DropShadowError::Softness
        );
        assert_eq!(
            DropShadowGrade::new(color(f32::NAN, 0.0, 0.0), 0.0, 0.0, 0.0, 0.0, false).unwrap_err(),
            DropShadowError::Color
        );
    }

    #[test]
    fn input_halo_covers_the_offset_and_the_blur() {
        let grade = DropShadowGrade::new(color(0.0, 0.0, 0.0), 255.0, 90.0, 8.0, 4.0, false)
            .expect("valid grade");
        let (horizontal, vertical) = grade.input_halo().expect("bounded halo");
        assert!(
            horizontal >= 9,
            "halo covers the eight pixel offset, got {horizontal}"
        );
        assert!(vertical > 0, "softness adds vertical halo, got {vertical}");
    }
}
