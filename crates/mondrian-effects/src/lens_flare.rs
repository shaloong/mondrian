//! Lens flare: a screen-space light emission added on top of the source frame.
//!
//! The effect list specifies only the flare centre, its brightness, the lens
//! type (three presets, as the list's menu of three values implies) and the
//! blend with the original. The flare geometry itself is not specified, so the
//! model here is deliberately explicit and documented:
//!
//! * the light source sits at the author's centre, drawn as a bright core with
//!   a gaussian falloff plus a horizontal anamorphic streak;
//! * ghost discs are placed on the line through the light and the frame centre,
//!   at the signed fractions listed by the lens type, which is how real
//!   multi-element lenses draw internal reflections;
//! * every contribution is additive light in the premultiplied domain, so
//!   transparent pixels stay transparent and coverage never changes.
//!
//! The lens types differ only in ghost layout and tint: `zoom_50_300` draws the
//! widest set of ghosts (a zoom lens has the most elements), `prime_35` draws a
//! tighter, warmer set, and `prime_105` draws two large, faint ghosts with long
//! anamorphic streaks. All numbers are relative to `min(width, height)`, so the
//! flare keeps its proportions on any frame size.

use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Largest accepted brightness, in percent, mirroring the effect list.
pub const LENS_FLARE_MAX_BRIGHTNESS_PERCENT: f32 = 300.0;

/// A ghost disc of a lens type.
#[derive(Clone, Copy, Debug)]
struct Ghost {
    /// Position along the light-centre axis: `0` is the light, `1` the frame
    /// centre, values beyond `1` continue on the opposite side.
    position: f32,
    /// Radius as a fraction of the frame's smaller dimension.
    radius: f32,
    /// Linear-light tint of the reflection.
    tint: [f32; 3],
}

/// The lens presets the effect list exposes as a three-value menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LensFlareType {
    /// Menu value 1: the widest ghost train, cool highlights.
    Zoom50To300,
    /// Menu value 2: fewer ghosts, warmer reflections.
    Prime35,
    /// Menu value 3: two large faint ghosts and long streaks.
    Prime105,
}

impl LensFlareType {
    /// Menu value used by the effect list.
    #[must_use]
    pub const fn menu_index(self) -> i64 {
        match self {
            Self::Zoom50To300 => 1,
            Self::Prime35 => 2,
            Self::Prime105 => 3,
        }
    }

    /// Stable authoring key.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Zoom50To300 => "zoom_50_300",
            Self::Prime35 => "prime_35",
            Self::Prime105 => "prime_105",
        }
    }

    /// Parses an authoring key back into a lens type.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "zoom_50_300" => Some(Self::Zoom50To300),
            "prime_35" => Some(Self::Prime35),
            "prime_105" => Some(Self::Prime105),
            _ => None,
        }
    }

    /// Parses the effect list's menu value.
    #[must_use]
    pub const fn from_menu(menu: i64) -> Option<Self> {
        match menu {
            1 => Some(Self::Zoom50To300),
            2 => Some(Self::Prime35),
            3 => Some(Self::Prime105),
            _ => None,
        }
    }

    /// Compact code used by the GPU uniform block.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Zoom50To300 => 1,
            Self::Prime35 => 2,
            Self::Prime105 => 3,
        }
    }

    fn ghosts(self) -> &'static [Ghost] {
        match self {
            // A wide zoom has many weak elements, so the train is long and the
            // reflections are small and cool.
            Self::Zoom50To300 => &[
                Ghost {
                    position: 0.45,
                    radius: 0.030,
                    tint: [0.55, 0.70, 1.00],
                },
                Ghost {
                    position: 0.95,
                    radius: 0.016,
                    tint: [0.70, 0.85, 1.00],
                },
                Ghost {
                    position: 1.40,
                    radius: 0.055,
                    tint: [0.45, 0.75, 0.95],
                },
                Ghost {
                    position: 1.85,
                    radius: 0.022,
                    tint: [0.80, 0.90, 1.00],
                },
                Ghost {
                    position: 2.35,
                    radius: 0.070,
                    tint: [0.35, 0.55, 0.90],
                },
            ],
            // A fast prime has fewer, warmer elements.
            Self::Prime35 => &[
                Ghost {
                    position: 0.55,
                    radius: 0.045,
                    tint: [1.00, 0.75, 0.35],
                },
                Ghost {
                    position: 1.15,
                    radius: 0.024,
                    tint: [1.00, 0.60, 0.25],
                },
                Ghost {
                    position: 1.70,
                    radius: 0.080,
                    tint: [0.95, 0.55, 0.30],
                },
            ],
            // A long telephoto spreads a wide, faint ring far from the source.
            Self::Prime105 => &[
                Ghost {
                    position: 0.80,
                    radius: 0.120,
                    tint: [0.85, 0.85, 1.00],
                },
                Ghost {
                    position: 1.60,
                    radius: 0.180,
                    tint: [0.70, 0.80, 1.00],
                },
            ],
        }
    }

    /// Core radius as a fraction of the smaller frame dimension.
    fn core_radius(self) -> f32 {
        match self {
            Self::Zoom50To300 => 0.045,
            Self::Prime35 => 0.060,
            Self::Prime105 => 0.035,
        }
    }

    /// Anamorphic streak half-length as a fraction of the frame width.
    fn streak_length(self) -> f32 {
        match self {
            Self::Zoom50To300 => 0.35,
            Self::Prime35 => 0.25,
            Self::Prime105 => 0.55,
        }
    }

    /// Anamorphic streak thickness as a fraction of the smaller dimension.
    fn streak_thickness(self) -> f32 {
        match self {
            Self::Zoom50To300 => 0.012,
            Self::Prime35 => 0.008,
            Self::Prime105 => 0.010,
        }
    }

    /// Tint of the central core.
    fn core_tint(self) -> [f32; 3] {
        match self {
            Self::Zoom50To300 => [0.95, 0.97, 1.00],
            Self::Prime35 => [1.00, 0.92, 0.78],
            Self::Prime105 => [1.00, 0.96, 0.90],
        }
    }
}

/// Rejected author values for the lens flare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LensFlareError {
    /// The brightness is not a finite value inside `0..=300`.
    Brightness,
    /// The blend is not a finite value inside `0..=1`.
    Blend,
    /// The centre is not finite.
    Centre,
}

impl core::fmt::Display for LensFlareError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let message = match self {
            Self::Brightness => "lens flare brightness must be within 0..=300 percent",
            Self::Blend => "lens flare blend must be within 0..=1",
            Self::Centre => "lens flare centre must be finite",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for LensFlareError {}

/// Authored lens flare parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LensFlareGrade {
    brightness: f32,
    blend_with_original: f32,
    centre: [f32; 2],
    lens: LensFlareType,
}

impl LensFlareGrade {
    /// Builds a grade, rejecting values outside the declared ranges.
    pub fn new(
        brightness_percent: f32,
        blend_with_original: f32,
        centre: [f32; 2],
        lens: LensFlareType,
    ) -> Result<Self, LensFlareError> {
        if !brightness_percent.is_finite()
            || !(0.0..=LENS_FLARE_MAX_BRIGHTNESS_PERCENT).contains(&brightness_percent)
        {
            return Err(LensFlareError::Brightness);
        }
        if !blend_with_original.is_finite() || !(0.0..=1.0).contains(&blend_with_original) {
            return Err(LensFlareError::Blend);
        }
        if !centre[0].is_finite() || !centre[1].is_finite() {
            return Err(LensFlareError::Centre);
        }
        Ok(Self {
            brightness: brightness_percent / 100.0,
            blend_with_original,
            centre,
            lens,
        })
    }

    /// Light level as a multiplier where `1.0` is the list's 100 percent.
    #[must_use]
    pub const fn brightness(&self) -> f32 {
        self.brightness
    }

    /// How much of the source survives: `1.0` keeps the original frame.
    #[must_use]
    pub const fn blend_with_original(&self) -> f32 {
        self.blend_with_original
    }

    /// Flare centre in pixels.
    #[must_use]
    pub const fn centre(&self) -> [f32; 2] {
        self.centre
    }

    /// Selected lens preset.
    #[must_use]
    pub const fn lens(&self) -> LensFlareType {
        self.lens
    }

    /// A flare with no brightness or a full blend leaves the frame unchanged.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.brightness <= 1.0e-6 || self.blend_with_original >= 1.0 - 1.0e-6
    }

    /// Packs the uniform block: `[centre_x, centre_y, brightness, blend]` and
    /// `[lens, 0, 0, 0]`.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.centre[0],
                self.centre[1],
                self.brightness,
                self.blend_with_original,
            ],
            [self.lens.as_u32() as f32, 0.0, 0.0, 0.0],
        )
    }

    /// Light emitted at one pixel, before blending with the source.
    ///
    /// The result is linear light in `0..`, already scaled by the brightness.
    #[must_use]
    pub fn emission_at(&self, column: f32, row: f32, width: f32, height: f32) -> [f32; 3] {
        if width <= 0.0 || height <= 0.0 || self.brightness <= 0.0 {
            return [0.0; 3];
        }
        let scale = width.min(height);
        let centre_x = width * 0.5;
        let centre_y = height * 0.5;
        let light_x = self.centre[0];
        let light_y = self.centre[1];

        let mut emission = [0.0_f32; 3];

        // The bright core: a gaussian blob around the source.
        let core_radius = self.lens.core_radius() * scale;
        let distance = ((column - light_x).powi(2) + (row - light_y).powi(2)).sqrt();
        let core = (-0.5 * (distance / core_radius.max(1.0e-6)).powi(2)).exp();
        add_tinted(&mut emission, self.lens.core_tint(), core);

        // The anamorphic streak: a thin horizontal gaussian bar through the
        // light, which is what lens iris diffraction produces.
        let streak_thickness = self.lens.streak_thickness() * scale;
        let streak_half = self.lens.streak_length() * width;
        let across = (row - light_y).abs() / streak_thickness.max(1.0e-6);
        let along = (column - light_x).abs() / streak_half.max(1.0e-6);
        if across < 4.0 && along < 1.0 {
            let streak = (-0.5 * across * across).exp() * (1.0 - along).powi(2);
            add_tinted(&mut emission, self.lens.core_tint(), streak * 0.85);
        }

        // The ghost train: soft discs on the light-centre axis.
        let axis_x = light_x - centre_x;
        let axis_y = light_y - centre_y;
        for ghost in self.lens.ghosts() {
            let ghost_x = centre_x + axis_x * (ghost.position - 1.0);
            let ghost_y = centre_y + axis_y * (ghost.position - 1.0);
            let radius = ghost.radius * scale;
            let separation = ((column - ghost_x).powi(2) + (row - ghost_y).powi(2)).sqrt();
            if separation >= radius {
                continue;
            }
            // A soft disc: flat inside 60 percent of the radius, fading out.
            let normalized = separation / radius.max(1.0e-6);
            let disc = 1.0 - smoothstep(0.6, 1.0, normalized);
            add_tinted(&mut emission, ghost.tint, disc * 0.35);
        }

        [
            emission[0] * self.brightness,
            emission[1] * self.brightness,
            emission[2] * self.brightness,
        ]
    }

    /// Applies the flare to one straight-alpha pixel.
    #[must_use]
    pub fn apply(
        &self,
        pixel: [f32; 4],
        column: usize,
        row: usize,
        width: usize,
        height: usize,
    ) -> [f32; 4] {
        if self.is_identity() {
            return pixel;
        }
        let emission = self.emission_at(
            column as f32 + 0.5,
            row as f32 + 0.5,
            width as f32,
            height as f32,
        );
        if emission == [0.0; 3] {
            return pixel;
        }
        // Light is added in the premultiplied domain so transparent pixels do
        // not glow and coverage is never invented.
        let premultiplied = [
            pixel[0] * pixel[3] + emission[0],
            pixel[1] * pixel[3] + emission[1],
            pixel[2] * pixel[3] + emission[2],
            pixel[3],
        ];
        let flared = unpremultiplied(premultiplied);
        let keep = self.blend_with_original;
        [
            flared[0] * (1.0 - keep) + pixel[0] * keep,
            flared[1] * (1.0 - keep) + pixel[1] * keep,
            flared[2] * (1.0 - keep) + pixel[2] * keep,
            flared[3] * (1.0 - keep) + pixel[3] * keep,
        ]
    }
}

fn add_tinted(emission: &mut [f32; 3], tint: [f32; 3], weight: f32) {
    if weight <= 0.0 {
        return;
    }
    for (target, channel) in emission.iter_mut().zip(tint.iter()) {
        *target += channel * weight;
    }
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let span = edge1 - edge0;
    if span.abs() <= 1.0e-9 {
        return if value < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((value - edge0) / span).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn unpremultiplied(sample: [f32; 4]) -> [f32; 4] {
    let alpha = sample[3].clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return [0.0; 4];
    }
    [
        sample[0] / alpha,
        sample[1] / alpha,
        sample[2] / alpha,
        alpha,
    ]
}

/// Applies the flare to a whole frame, returning whether anything changed.
#[must_use]
pub fn lens_flare_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: LensFlareGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_lens_flare_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the flare to a frame, premultiplying exactly like the effect graph.
///
/// # Errors
///
/// Propagates whatever the checkpoint returns.
pub(crate) fn apply_lens_flare_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: LensFlareGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let expected = width.checked_mul(height).filter(|count| *count > 0 && *count == working.len());
    if expected.is_none() {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(false);
    }
    premultiply_rgba_controlled(working, checkpoint)?;
    for row in 0..height {
        checkpoint()?;
        for column in 0..width {
            let index = row * width + column;
            let pixel = unpremultiplied(working[index]);
            let flared = grade.apply(pixel, column, row, width, height);
            working[index] = [
                flared[0] * flared[3],
                flared[1] * flared[3],
                flared[2] * flared[3],
                flared[3],
            ];
        }
    }
    unpremultiply_rgba_controlled(working, checkpoint)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade() -> LensFlareGrade {
        LensFlareGrade::new(100.0, 0.0, [16.0, 9.0], LensFlareType::Prime35).expect("valid grade")
    }

    fn floor_frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        vec![[0.1, 0.1, 0.1, 1.0]; width * height]
    }

    #[test]
    fn rejects_values_outside_the_declared_ranges() {
        assert_eq!(
            LensFlareGrade::new(-1.0, 0.0, [0.0, 0.0], LensFlareType::Prime35).unwrap_err(),
            LensFlareError::Brightness
        );
        assert_eq!(
            LensFlareGrade::new(301.0, 0.0, [0.0, 0.0], LensFlareType::Prime35).unwrap_err(),
            LensFlareError::Brightness
        );
        assert_eq!(
            LensFlareGrade::new(100.0, 1.5, [0.0, 0.0], LensFlareType::Prime35).unwrap_err(),
            LensFlareError::Blend
        );
        assert_eq!(
            LensFlareGrade::new(100.0, 0.0, [f32::NAN, 0.0], LensFlareType::Prime35).unwrap_err(),
            LensFlareError::Centre
        );
    }

    #[test]
    fn menu_keys_and_codes_round_trip() {
        for lens in [
            LensFlareType::Zoom50To300,
            LensFlareType::Prime35,
            LensFlareType::Prime105,
        ] {
            assert_eq!(LensFlareType::from_key(lens.key()), Some(lens));
            assert_eq!(LensFlareType::from_menu(lens.menu_index()), Some(lens));
            assert_eq!(lens.as_u32() as i64, lens.menu_index());
        }
        assert_eq!(LensFlareType::from_menu(0), None);
        assert_eq!(LensFlareType::from_menu(4), None);
        assert_eq!(LensFlareType::from_key("wide"), None);
    }

    #[test]
    fn a_zero_brightness_or_full_blend_is_the_identity() {
        let dark = LensFlareGrade::new(0.0, 0.0, [16.0, 9.0], LensFlareType::Prime35).unwrap();
        assert!(dark.is_identity());
        let kept = LensFlareGrade::new(100.0, 1.0, [16.0, 9.0], LensFlareType::Prime35).unwrap();
        assert!(kept.is_identity());

        let mut frame = floor_frame(32, 18);
        let original = frame.clone();
        assert!(!lens_flare_frame_f32(&mut frame, 32, 18, dark));
        assert_eq!(frame, original);
        assert!(!lens_flare_frame_f32(&mut frame, 32, 18, kept));
        assert_eq!(frame, original);
    }

    #[test]
    fn the_core_brightens_the_light_centre() {
        let mut frame = floor_frame(32, 18);
        assert!(lens_flare_frame_f32(&mut frame, 32, 18, grade()));
        let centre = frame[9 * 32 + 16];
        assert!(centre[0] > 1.0, "core should be bright, got {centre:?}");
        assert!(centre[3] <= 1.0 + 1.0e-6);
    }

    #[test]
    fn emission_falls_off_away_from_the_light() {
        let grade = grade();
        let (width, height) = (64.0_f32, 36.0_f32);
        let near = grade.emission_at(16.5, 9.5, width, height)[0];
        let far = grade.emission_at(60.5, 30.5, width, height)[0];
        assert!(near > far, "near {near} should exceed far {far}");
        assert_eq!(far, 0.0, "a corner far from every element stays untouched");
    }

    #[test]
    fn brightness_scales_the_emission_linearly() {
        let dim = LensFlareGrade::new(50.0, 0.0, [16.0, 9.0], LensFlareType::Prime35).unwrap();
        let bright = LensFlareGrade::new(200.0, 0.0, [16.0, 9.0], LensFlareType::Prime35).unwrap();
        let dim_emission = dim.emission_at(20.0, 11.0, 64.0, 36.0);
        let bright_emission = bright.emission_at(20.0, 11.0, 64.0, 36.0);
        for channel in 0..3 {
            assert!(
                (bright_emission[channel] - 4.0 * dim_emission[channel]).abs() <= 1.0e-5,
                "channel {channel} should scale by four"
            );
        }
    }

    #[test]
    fn ghosts_appear_on_the_line_through_the_frame_centre() {
        // A light above the frame centre (32, 36) puts a ghost at 0.55 along the
        // axis, which lands below the centre: 36 + (-32) * (0.55 - 1) = 50.4.
        let grade = LensFlareGrade::new(100.0, 0.0, [32.0, 4.0], LensFlareType::Prime35).unwrap();
        let on_axis = grade.emission_at(32.0, 50.4, 64.0, 72.0);
        let off_axis = grade.emission_at(12.0, 50.4, 64.0, 72.0);
        assert!(
            on_axis[2] > 0.0,
            "the ghost disc should emit on the axis: {on_axis:?}"
        );
        assert!(
            off_axis[2] < 1.0e-6,
            "outside the disc only the gaussian core remains: {off_axis:?}"
        );
    }

    #[test]
    fn lens_types_draw_different_flares() {
        let zoom =
            LensFlareGrade::new(100.0, 0.0, [16.0, 9.0], LensFlareType::Zoom50To300).unwrap();
        let tele = LensFlareGrade::new(100.0, 0.0, [16.0, 9.0], LensFlareType::Prime105).unwrap();
        let mut zoom_frame = floor_frame(64, 36);
        let mut tele_frame = floor_frame(64, 36);
        assert!(lens_flare_frame_f32(&mut zoom_frame, 64, 36, zoom));
        assert!(lens_flare_frame_f32(&mut tele_frame, 64, 36, tele));
        assert_ne!(
            zoom_frame, tele_frame,
            "each lens preset must draw its own flare"
        );
    }

    #[test]
    fn transparent_pixels_stay_transparent() {
        let mut frame = vec![[0.0_f32, 0.0, 0.0, 0.0]; 32 * 18];
        assert!(lens_flare_frame_f32(&mut frame, 32, 18, grade()));
        for (index, pixel) in frame.iter().enumerate() {
            assert_eq!(pixel[3], 0.0, "pixel {index} invented coverage: {pixel:?}");
            assert_eq!(
                *pixel,
                [0.0, 0.0, 0.0, 0.0],
                "pixel {index} glowed: {pixel:?}"
            );
        }
    }

    #[test]
    fn blending_keeps_the_authored_share_of_the_source() {
        let kept = LensFlareGrade::new(100.0, 0.5, [16.0, 9.0], LensFlareType::Prime35).unwrap();
        let full = LensFlareGrade::new(100.0, 0.0, [16.0, 9.0], LensFlareType::Prime35).unwrap();
        let mut blended = floor_frame(32, 18);
        let mut flared = floor_frame(32, 18);
        assert!(lens_flare_frame_f32(&mut blended, 32, 18, kept));
        assert!(lens_flare_frame_f32(&mut flared, 32, 18, full));
        let index = 9 * 32 + 16;
        let expected = flared[index][0] * 0.5 + 0.1 * 0.5;
        assert!(
            (blended[index][0] - expected).abs() <= 1.0e-4,
            "got {} expected {expected}",
            blended[index][0]
        );
    }

    #[test]
    fn extended_range_sources_are_not_clamped() {
        let mut frame = vec![[0.0_f32, 0.0, 0.0, 1.0]; 32 * 18];
        frame[9 * 32 + 16] = [4.0, 0.0, 0.0, 1.0];
        assert!(lens_flare_frame_f32(&mut frame, 32, 18, grade()));
        assert!(
            frame[9 * 32 + 16][0] > 4.0,
            "flare light adds to HDR sources"
        );
    }

    #[test]
    fn the_gpu_controls_carry_every_author_value() {
        let grade = LensFlareGrade::new(250.0, 0.25, [3.5, 4.5], LensFlareType::Prime105).unwrap();
        let (params, extra) = grade.gpu_controls();
        assert!((params[0] - 3.5).abs() <= f32::EPSILON);
        assert!((params[1] - 4.5).abs() <= f32::EPSILON);
        assert!((params[2] - 2.5).abs() <= f32::EPSILON);
        assert!((params[3] - 0.25).abs() <= f32::EPSILON);
        assert!((extra[0] - 3.0).abs() <= f32::EPSILON);
    }

    #[test]
    fn a_mismatched_geometry_reports_no_change() {
        let mut frame = floor_frame(8, 8);
        assert!(!lens_flare_frame_f32(&mut frame, 8, 17, grade()));
    }

    #[test]
    fn the_flare_is_deterministic() {
        let mut first = floor_frame(32, 18);
        let mut second = floor_frame(32, 18);
        assert!(lens_flare_frame_f32(&mut first, 32, 18, grade()));
        assert!(lens_flare_frame_f32(&mut second, 32, 18, grade()));
        assert_eq!(first, second);
    }
}
