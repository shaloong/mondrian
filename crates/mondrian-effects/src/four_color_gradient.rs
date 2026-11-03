//! GEN-02 四色渐变 Four-Color Gradient: a pure generator that blends four
//! authored colours with inverse-distance weights.
//!
//! # Documented reading
//!
//! The generator is a fused point operation: every output pixel is a function
//! of its own position only, with no source read and no spatial pass. For the
//! pixel centre `p = (column + 0.5, row + 0.5)` and the four control points
//! `p_i`:
//!
//! - `d2_i = (p.x - p_i.x)^2 + (p.y - p_i.y)^2`,
//! - `d4_i = d2_i * d2_i` and `lambda4 = (blend * blend) * (blend * blend)`
//!   (no `powf`, so the CPU and the shader round the same way),
//! - `w_i = 1 / (d4_i + lambda4)`, always strictly positive,
//! - `w_hat_i = w_i / (w_1 + w_2 + w_3 + w_4)`, summed in that fixed order,
//! - `gradient = w_hat_1 * c_1 + w_hat_2 * c_2 + w_hat_3 * c_3 + w_hat_4 * c_4`.
//!
//! `blend` is a soft radius: small values approach a nearest-point painting,
//! large values approach the plain mean of the four colours, and points outside
//! the frame are never clamped — their weight simply falls off.
//!
//! # Dither
//!
//! `dither` is an 8-bit amplitude: the deterministic integer hash the deband and
//! ramp kernels use, salted with [`FOUR_COLOR_GRADIENT_DITHER_SALT`], is
//! sampled at the pixel index and scaled by `dither / 255`, then added to the
//! blended colour before the clamp. The salt is a compile-time constant and no
//! frame seed enters it, so a re-render is bit-identical and the pattern does
//! not flicker during playback. `dither = 0` never samples the hash at all.
//!
//! # Colour and coverage
//!
//! The blend pixel is `[clamp(gradient + noise, 0, 1), opacity]` in straight
//! alpha, with `opacity` authored as a percentage and stored normalised. It is
//! composited over the accumulated stack with the shared straight-alpha blend,
//! so `opacity` is the generator's own coverage rather than a multiplier on the
//! frame alpha. A fully transparent pixel is therefore painted instead of being
//! skipped: the generator authors coverage, which is exactly why the shader's
//! `effects_author_coverage` registers this kind.
//!
//! `opacity <= 1e-6` is the identity: the blend returns the base pixel and the
//! graph builder elides the operation.

use mondrian_core::BlendMode;

use crate::noise::noise_sample;

/// Number of control points the generator blends.
pub const FOUR_COLOR_GRADIENT_POINT_COUNT: usize = 4;
/// Smallest accepted blend radius, in pixels.
pub const FOUR_COLOR_GRADIENT_MIN_BLEND: f32 = 5.0;
/// Largest accepted blend radius, in pixels.
pub const FOUR_COLOR_GRADIENT_MAX_BLEND: f32 = 10_000.0;
/// Default blend radius, in pixels.
pub const FOUR_COLOR_GRADIENT_DEFAULT_BLEND: f32 = 100.0;
/// Smallest accepted dither amplitude, in 8-bit code values.
pub const FOUR_COLOR_GRADIENT_MIN_DITHER: f32 = 0.0;
/// Largest accepted dither amplitude, in 8-bit code values.
pub const FOUR_COLOR_GRADIENT_MAX_DITHER: f32 = 500.0;
/// Smallest accepted opacity, in percent.
pub const FOUR_COLOR_GRADIENT_MIN_OPACITY: f32 = 0.0;
/// Largest accepted opacity, in percent.
pub const FOUR_COLOR_GRADIENT_MAX_OPACITY: f32 = 100.0;
/// Default opacity, in percent.
pub const FOUR_COLOR_GRADIENT_DEFAULT_OPACITY: f32 = 100.0;
/// Largest accepted absolute control-point coordinate, in pixels.
pub const FOUR_COLOR_GRADIENT_MAX_POINT: f32 = 1_000_000.0;
/// Salt of the deterministic dither hash; shared with the GPU shader.
pub const FOUR_COLOR_GRADIENT_DITHER_SALT: u32 = 0x2545_F491;

/// Pixels visited between two cancellation checkpoints.
const CHECKPOINT_STRIDE: usize = 4_096;

/// Why a [`FourColorGradientGrade`] could not be authored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FourColorGradientError {
    /// A control point is not a finite number.
    NonFinitePoint {
        /// Zero-based index of the rejected point.
        index: usize,
    },
    /// A control point leaves the authorable pixel square.
    PointOutOfRange {
        /// Zero-based index of the rejected point.
        index: usize,
    },
    /// A colour has a non-finite component.
    NonFiniteColor {
        /// Zero-based index of the rejected colour.
        index: usize,
    },
    /// A colour leaves `0..=1`.
    ColorOutOfRange {
        /// Zero-based index of the rejected colour.
        index: usize,
    },
    /// The blend radius is not a finite number.
    NonFiniteBlend,
    /// The blend radius leaves `5..=10000`.
    BlendOutOfRange,
    /// The dither amplitude is not a finite number.
    NonFiniteDither,
    /// The dither amplitude leaves `0..=500`.
    DitherOutOfRange,
    /// The opacity is not a finite number.
    NonFiniteOpacity,
    /// The opacity leaves `0..=100`.
    OpacityOutOfRange,
    /// `Dissolve` needs a frame seed the generator kernel does not receive.
    DissolveUnsupported,
}

impl std::fmt::Display for FourColorGradientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonFinitePoint { index } => {
                write!(
                    formatter,
                    "four color gradient point {} must be finite",
                    index + 1
                )
            }
            Self::PointOutOfRange { index } => write!(
                formatter,
                "four color gradient point {} must be inside [-1000000, 1000000]",
                index + 1
            ),
            Self::NonFiniteColor { index } => {
                write!(
                    formatter,
                    "four color gradient color {} must be finite",
                    index + 1
                )
            }
            Self::ColorOutOfRange { index } => write!(
                formatter,
                "four color gradient color {} must be inside [0, 1]",
                index + 1
            ),
            Self::NonFiniteBlend => formatter.write_str("four color gradient blend must be finite"),
            Self::BlendOutOfRange => {
                formatter.write_str("four color gradient blend must be within 5..=10000")
            }
            Self::NonFiniteDither => {
                formatter.write_str("four color gradient dither must be finite")
            }
            Self::DitherOutOfRange => {
                formatter.write_str("four color gradient dither must be within 0..=500")
            }
            Self::NonFiniteOpacity => {
                formatter.write_str("four color gradient opacity must be finite")
            }
            Self::OpacityOutOfRange => {
                formatter.write_str("four color gradient opacity must be within 0..=100")
            }
            Self::DissolveUnsupported => {
                formatter.write_str("four color gradient blend mode dissolve is not supported")
            }
        }
    }
}

impl std::error::Error for FourColorGradientError {}

/// One grade's seven-cell uniform payload.
///
/// The cells are `(params, color, extra0, extra1, extra2, extra3)`, in exactly
/// the order the fused GPU pass reads them.
pub type FourColorGradientControls = ([f32; 4], [f32; 4], [f32; 4], [f32; 4], [f32; 4], [f32; 4]);

/// A validated four-point colour gradient.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FourColorGradientGrade {
    points: [[f32; 2]; FOUR_COLOR_GRADIENT_POINT_COUNT],
    colors: [[f32; 3]; FOUR_COLOR_GRADIENT_POINT_COUNT],
    blend: f32,
    dither: f32,
    opacity: f32,
    blend_mode: BlendMode,
}

impl FourColorGradientGrade {
    /// Validate and build a four-colour gradient grade.
    ///
    /// `opacity` is authored in percent and stored normalised.
    ///
    /// # Errors
    ///
    /// Returns [`FourColorGradientError`] when a point, colour, or scalar leaves
    /// its authorable range, or when the blend mode is `Dissolve`.
    pub fn new(
        points: [[f32; 2]; FOUR_COLOR_GRADIENT_POINT_COUNT],
        colors: [[f32; 3]; FOUR_COLOR_GRADIENT_POINT_COUNT],
        blend: f32,
        dither: f32,
        opacity: f32,
        blend_mode: BlendMode,
    ) -> Result<Self, FourColorGradientError> {
        for (index, point) in points.iter().enumerate() {
            if !point.iter().all(|coordinate| coordinate.is_finite()) {
                return Err(FourColorGradientError::NonFinitePoint { index });
            }
            if !point.iter().all(|coordinate| {
                (-FOUR_COLOR_GRADIENT_MAX_POINT..=FOUR_COLOR_GRADIENT_MAX_POINT)
                    .contains(coordinate)
            }) {
                return Err(FourColorGradientError::PointOutOfRange { index });
            }
        }
        for (index, colour) in colors.iter().enumerate() {
            if !colour.iter().all(|component| component.is_finite()) {
                return Err(FourColorGradientError::NonFiniteColor { index });
            }
            if !colour.iter().all(|component| (0.0..=1.0).contains(component)) {
                return Err(FourColorGradientError::ColorOutOfRange { index });
            }
        }
        if !blend.is_finite() {
            return Err(FourColorGradientError::NonFiniteBlend);
        }
        if !(FOUR_COLOR_GRADIENT_MIN_BLEND..=FOUR_COLOR_GRADIENT_MAX_BLEND).contains(&blend) {
            return Err(FourColorGradientError::BlendOutOfRange);
        }
        if !dither.is_finite() {
            return Err(FourColorGradientError::NonFiniteDither);
        }
        if !(FOUR_COLOR_GRADIENT_MIN_DITHER..=FOUR_COLOR_GRADIENT_MAX_DITHER).contains(&dither) {
            return Err(FourColorGradientError::DitherOutOfRange);
        }
        if !opacity.is_finite() {
            return Err(FourColorGradientError::NonFiniteOpacity);
        }
        if !(FOUR_COLOR_GRADIENT_MIN_OPACITY..=FOUR_COLOR_GRADIENT_MAX_OPACITY).contains(&opacity) {
            return Err(FourColorGradientError::OpacityOutOfRange);
        }
        if blend_mode == BlendMode::Dissolve {
            return Err(FourColorGradientError::DissolveUnsupported);
        }
        Ok(Self {
            points,
            colors,
            blend,
            dither,
            opacity: opacity / FOUR_COLOR_GRADIENT_MAX_OPACITY,
            blend_mode,
        })
    }

    /// The four control points, in pixels.
    #[must_use]
    pub const fn points(&self) -> [[f32; 2]; FOUR_COLOR_GRADIENT_POINT_COUNT] {
        self.points
    }

    /// The four straight colours.
    #[must_use]
    pub const fn colors(&self) -> [[f32; 3]; FOUR_COLOR_GRADIENT_POINT_COUNT] {
        self.colors
    }

    /// The authored blend radius, in pixels.
    #[must_use]
    pub const fn blend(&self) -> f32 {
        self.blend
    }

    /// The authored dither amplitude, in 8-bit code values.
    #[must_use]
    pub const fn dither(&self) -> f32 {
        self.dither
    }

    /// The authored opacity, normalised to `0..=1`.
    #[must_use]
    pub const fn opacity(&self) -> f32 {
        self.opacity
    }

    /// The authored blend mode.
    #[must_use]
    pub const fn blend_mode(&self) -> BlendMode {
        self.blend_mode
    }

    /// Whether this generator contributes nothing to the image.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.opacity <= 1.0e-6
    }

    /// The six uniform cells this grade packs for the fused point pass.
    ///
    /// The cells are exactly:
    /// - `params`: `[p1.x, p1.y, p2.x, p2.y]`,
    /// - `color`: `[c1.rgb, blend]`,
    /// - `extra0`: `[p3.x, p3.y, p4.x, p4.y]`,
    /// - `extra1`: `[c2.rgb, dither]`,
    /// - `extra2`: `[c3.rgb, opacity]`,
    /// - `extra3`: `[c4.rgb, mode_index]`.
    #[must_use]
    pub fn gpu_controls(self) -> FourColorGradientControls {
        (
            [
                self.points[0][0],
                self.points[0][1],
                self.points[1][0],
                self.points[1][1],
            ],
            [
                self.colors[0][0],
                self.colors[0][1],
                self.colors[0][2],
                self.blend,
            ],
            [
                self.points[2][0],
                self.points[2][1],
                self.points[3][0],
                self.points[3][1],
            ],
            [
                self.colors[1][0],
                self.colors[1][1],
                self.colors[1][2],
                self.dither,
            ],
            [
                self.colors[2][0],
                self.colors[2][1],
                self.colors[2][2],
                self.opacity,
            ],
            [
                self.colors[3][0],
                self.colors[3][1],
                self.colors[3][2],
                crate::glow::blend_mode_index(self.blend_mode) as f32,
            ],
        )
    }

    /// The normalised inverse-distance weights at one pixel centre.
    fn weights(&self, centre: [f32; 2]) -> [f32; FOUR_COLOR_GRADIENT_POINT_COUNT] {
        let lambda_squared = self.blend * self.blend;
        let lambda4 = lambda_squared * lambda_squared;
        let mut weights = [0.0_f32; FOUR_COLOR_GRADIENT_POINT_COUNT];
        for (index, point) in self.points.iter().enumerate() {
            let delta_x = centre[0] - point[0];
            let delta_y = centre[1] - point[1];
            let distance_squared = delta_x * delta_x + delta_y * delta_y;
            let distance4 = distance_squared * distance_squared;
            weights[index] = 1.0 / (distance4 + lambda4);
        }
        let total = weights[0] + weights[1] + weights[2] + weights[3];
        [
            weights[0] / total,
            weights[1] / total,
            weights[2] / total,
            weights[3] / total,
        ]
    }

    /// The straight, unclamped and undithered blended colour at one centre.
    fn gradient(&self, centre: [f32; 2]) -> [f32; 3] {
        let weights = self.weights(centre);
        let mut gradient = [0.0_f32; 3];
        for (weight, colour) in weights.iter().zip(self.colors.iter()) {
            gradient[0] += weight * colour[0];
            gradient[1] += weight * colour[1];
            gradient[2] += weight * colour[2];
        }
        gradient
    }

    /// The signed dither offset at one pixel index.
    fn dither_offset(&self, column: u32, row: u32) -> f32 {
        if self.dither <= 0.0 {
            return 0.0;
        }
        noise_sample(column, row, FOUR_COLOR_GRADIENT_DITHER_SALT) * (self.dither / 255.0)
    }

    /// The dithered, clamped generator colour at one pixel index.
    fn shaded_colour(&self, column: u32, row: u32) -> [f32; 3] {
        let centre = [column as f32 + 0.5, row as f32 + 0.5];
        let gradient = self.gradient(centre);
        let dither = self.dither_offset(column, row);
        [
            (gradient[0] + dither).clamp(0.0, 1.0),
            (gradient[1] + dither).clamp(0.0, 1.0),
            (gradient[2] + dither).clamp(0.0, 1.0),
        ]
    }

    /// Apply the generator to one straight-alpha pixel at a frame-absolute
    /// index.
    ///
    /// A transparent base pixel is painted rather than skipped: the generator
    /// authors coverage equal to its own opacity.
    #[must_use]
    pub fn apply(&self, pixel: [f32; 4], column: u32, row: u32) -> [f32; 4] {
        if self.is_identity() {
            return pixel;
        }
        let colour = self.shaded_colour(column, row);
        crate::adjustment::blend_rgba_f32_pixel_seeded(
            pixel,
            [colour[0], colour[1], colour[2], self.opacity],
            1.0,
            self.blend_mode,
            0,
        )
    }
}

/// Generates one pixel of the gradient without any base pixel.
///
/// The returned alpha is the generator's own full coverage `1.0`; the authored
/// opacity is applied by [`FourColorGradientGrade::apply`].
#[must_use]
pub fn four_color_gradient_pixel(
    column: u32,
    row: u32,
    grade: &FourColorGradientGrade,
) -> [f32; 4] {
    let colour = grade.shaded_colour(column, row);
    [colour[0], colour[1], colour[2], 1.0]
}

/// Apply a four-colour gradient grade to a whole straight-alpha f32 frame.
///
/// Returns `false` when the geometry does not describe the frame exactly.
pub fn four_color_gradient_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &FourColorGradientGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_four_color_gradient_f32_region_controlled(
        pixels,
        0,
        0,
        width,
        height,
        grade,
        &mut checkpoint,
    )
    .unwrap_or(false)
}

/// Apply a four-colour gradient grade to a retained frame region.
///
/// `region_x`/`region_y` are the region's offset inside the complete frame, so
/// the pixel geometry stays frame absolute when only a sub-rectangle is
/// evaluated. Rows are processed in chunks of at most [`CHECKPOINT_STRIDE`]
/// pixels with a cooperative checkpoint before each chunk and once after the
/// last one; an error aborts immediately and leaves the remaining pixels
/// untouched.
///
/// The returned `bool` reports whether the buffer described the extent; the
/// identity also reports `true` (there is simply nothing to write).
///
/// # Errors
///
/// Propagates the error returned by `checkpoint`.
pub(crate) fn apply_four_color_gradient_f32_region_controlled<E>(
    pixels: &mut [[f32; 4]],
    region_x: usize,
    region_y: usize,
    width: usize,
    height: usize,
    grade: &FourColorGradientGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if width == 0 || height == 0 || pixels.len() < pixel_count {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    let frame = &mut pixels[..pixel_count];
    let rows_per_chunk = (CHECKPOINT_STRIDE / width).max(1);
    let mut start = 0usize;
    while start < height {
        checkpoint()?;
        let end = (start + rows_per_chunk).min(height);
        let rows = end - start;
        crate::parallel::for_each_row_band(
            &mut frame[start * width..end * width],
            width,
            0..rows,
            |first_row, band| {
                for (offset, chunk) in band.chunks_mut(width).enumerate() {
                    let row = (region_y + start + first_row + offset) as u32;
                    for (column, pixel) in chunk.iter_mut().enumerate() {
                        let column = (region_x + column) as u32;
                        *pixel = grade.apply(*pixel, column, row);
                    }
                }
            },
        );
        start = end;
    }
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    use mondrian_core::display_labels::blend_mode_options;

    const DEFAULT_POINTS: [[f32; 2]; 4] =
        [[25.6, 25.6], [230.4, 25.6], [25.6, 230.4], [230.4, 230.4]];
    const DEFAULT_COLORS: [[f32; 3]; 4] = [
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [1.0, 0.0, 1.0],
        [0.0, 0.0, 1.0],
    ];

    fn new_grade(
        points: [[f32; 2]; 4],
        colors: [[f32; 3]; 4],
        blend: f32,
        dither: f32,
        opacity: f32,
        blend_mode: BlendMode,
    ) -> FourColorGradientGrade {
        FourColorGradientGrade::new(points, colors, blend, dither, opacity, blend_mode)
            .expect("valid four color gradient grade")
    }

    fn default_grade() -> FourColorGradientGrade {
        new_grade(
            DEFAULT_POINTS,
            DEFAULT_COLORS,
            FOUR_COLOR_GRADIENT_DEFAULT_BLEND,
            0.0,
            FOUR_COLOR_GRADIENT_DEFAULT_OPACITY,
            BlendMode::Normal,
        )
    }

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() <= 1.0e-6
    }

    fn close_pixel(left: [f32; 4], right: [f32; 4]) -> bool {
        left.iter().zip(right.iter()).all(|(left, right)| close(*left, *right))
    }

    fn mean_of(colors: [[f32; 3]; 4]) -> [f32; 3] {
        [0, 1, 2].map(|channel| colors.iter().map(|colour| colour[channel]).sum::<f32>() / 4.0)
    }

    #[test]
    fn four_color_gradient_weights_are_a_partition_of_unity() {
        let coincident = [[25.6, 25.6]; 4];
        let mut distant = DEFAULT_POINTS;
        distant[0] = [-1_000_000.0, -1_000_000.0];
        for points in [DEFAULT_POINTS, coincident, distant] {
            for blend in [
                FOUR_COLOR_GRADIENT_MIN_BLEND,
                FOUR_COLOR_GRADIENT_DEFAULT_BLEND,
                FOUR_COLOR_GRADIENT_MAX_BLEND,
            ] {
                let grade = new_grade(points, DEFAULT_COLORS, blend, 0.0, 100.0, BlendMode::Normal);
                for row in 0..256u32 {
                    for column in 0..256u32 {
                        let centre = [column as f32 + 0.5, row as f32 + 0.5];
                        let weights = grade.weights(centre);
                        let total: f32 = weights.iter().sum();
                        assert!(
                            (total - 1.0).abs() <= 1.0e-6,
                            "weights {weights:?} sum to {total} at ({column}, {row})"
                        );
                        for weight in weights {
                            assert!(weight.is_finite(), "{weight} at ({column}, {row})");
                            assert!(weight >= 0.0, "{weight} at ({column}, {row})");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn four_color_gradient_corner_reaches_its_own_colour_at_minimum_blend() {
        let grade = new_grade(
            DEFAULT_POINTS,
            DEFAULT_COLORS,
            FOUR_COLOR_GRADIENT_MIN_BLEND,
            0.0,
            100.0,
            BlendMode::Normal,
        );
        let samples = [(25u32, 25u32), (230, 25), (25, 230), (230, 230)];
        let mut rendered = [[0.0_f32; 3]; 4];
        for (index, (column, row)) in samples.iter().enumerate() {
            let pixel = four_color_gradient_pixel(*column, *row, &grade);
            for channel in 0..3 {
                assert!(
                    (pixel[channel] - DEFAULT_COLORS[index][channel]).abs() <= 1.0e-3,
                    "corner {index} channel {channel} is {} rather than {}",
                    pixel[channel],
                    DEFAULT_COLORS[index][channel]
                );
            }
            rendered[index] = [pixel[0], pixel[1], pixel[2]];
        }
        for left in 0..4 {
            for right in (left + 1)..4 {
                assert_ne!(
                    rendered[left], rendered[right],
                    "corners {left} and {right} rendered the same colour"
                );
            }
        }
    }

    #[test]
    fn four_color_gradient_frame_centre_is_the_four_colour_mean() {
        // The four points are symmetric about the sampled pixel centre
        // (127.5, 127.5), so the weights are exactly equal there.
        let points = [[0.0, 0.0], [255.0, 0.0], [0.0, 255.0], [255.0, 255.0]];
        let colors = [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 1.0, 0.0],
        ];
        let grade = new_grade(points, colors, 100.0, 0.0, 100.0, BlendMode::Normal);
        let pixel = four_color_gradient_pixel(127, 127, &grade);
        let expected = mean_of(colors);
        assert!(close_pixel(
            pixel,
            [expected[0], expected[1], expected[2], 1.0]
        ));
    }

    #[test]
    fn four_color_gradient_dither_is_deterministic_and_salted() {
        let grade = new_grade(
            DEFAULT_POINTS,
            DEFAULT_COLORS,
            100.0,
            200.0,
            100.0,
            BlendMode::Normal,
        );
        let first = grade.apply([0.0, 0.0, 0.0, 1.0], 3, 4);
        let second = grade.apply([0.0, 0.0, 0.0, 1.0], 3, 4);
        assert_eq!(first.map(f32::to_bits), second.map(f32::to_bits));

        let plain = new_grade(
            DEFAULT_POINTS,
            DEFAULT_COLORS,
            100.0,
            0.0,
            100.0,
            BlendMode::Normal,
        );
        assert_eq!(plain.dither_offset(3, 4), 0.0);
        let gradient = plain.gradient([3.5, 4.5]);
        let expected = [
            gradient[0].clamp(0.0, 1.0),
            gradient[1].clamp(0.0, 1.0),
            gradient[2].clamp(0.0, 1.0),
            1.0,
        ];
        assert_eq!(
            plain.apply([0.0, 0.0, 0.0, 1.0], 3, 4).map(f32::to_bits),
            expected.map(f32::to_bits)
        );

        // The offset is our own salted sample, not the ramp or noise salt.
        let amplitude = 200.0 / 255.0;
        assert_eq!(
            grade.dither_offset(3, 4),
            noise_sample(3, 4, FOUR_COLOR_GRADIENT_DITHER_SALT) * amplitude
        );
        assert_ne!(
            grade.dither_offset(3, 4).to_bits(),
            (noise_sample(3, 4, crate::ramp::RAMP_DITHER_SALT) * amplitude).to_bits()
        );
    }

    #[test]
    fn four_color_gradient_opacity_scales_the_authored_coverage() {
        let clear = [0.0, 0.0, 0.0, 0.0];
        for (opacity, expected_alpha) in [(100.0, 1.0), (50.0, 0.5), (0.0, 0.0)] {
            let grade = new_grade(
                DEFAULT_POINTS,
                DEFAULT_COLORS,
                100.0,
                0.0,
                opacity,
                BlendMode::Normal,
            );
            let pixel = grade.apply(clear, 3, 2);
            assert_eq!(pixel[3], expected_alpha);
            if opacity > 0.0 {
                let gradient = four_color_gradient_pixel(3, 2, &grade);
                assert!(close(pixel[0], gradient[0]));
                assert!(close(pixel[1], gradient[1]));
                assert!(close(pixel[2], gradient[2]));
            } else {
                assert_eq!(pixel, clear);
            }
        }

        // Over an opaque base the result is a straight mix of the gradient and
        // the base; the coverage stays opaque.
        let base = [0.2, 0.3, 0.4, 1.0];
        for opacity in [100.0, 50.0, 25.0] {
            let grade = new_grade(
                DEFAULT_POINTS,
                DEFAULT_COLORS,
                100.0,
                0.0,
                opacity,
                BlendMode::Normal,
            );
            let pixel = grade.apply(base, 3, 2);
            assert_eq!(pixel[3], 1.0);
            let gradient = four_color_gradient_pixel(3, 2, &grade);
            let weight = opacity / 100.0;
            for channel in 0..3 {
                let expected = weight * gradient[channel] + (1.0 - weight) * base[channel];
                assert!(
                    close(pixel[channel], expected),
                    "channel {channel} is {} rather than {expected} at opacity {opacity}",
                    pixel[channel]
                );
            }
        }
    }

    #[test]
    fn four_color_gradient_each_blend_mode_matches_the_shared_blend_helper() {
        let modes: Vec<BlendMode> = blend_mode_options()
            .iter()
            .filter_map(|option| option.mode)
            .filter(|mode| *mode != BlendMode::Dissolve)
            .collect();
        assert_eq!(modes.len(), 26);

        let base = [0.2, 0.4, 0.6, 1.0];
        let reference = new_grade(
            DEFAULT_POINTS,
            DEFAULT_COLORS,
            100.0,
            0.0,
            80.0,
            BlendMode::Normal,
        );
        let gradient = four_color_gradient_pixel(3, 2, &reference);
        let mut rendered = Vec::new();
        for mode in modes {
            let grade = new_grade(DEFAULT_POINTS, DEFAULT_COLORS, 100.0, 0.0, 80.0, mode);
            let pixel = grade.apply(base, 3, 2);
            let expected = crate::adjustment::blend_rgba_f32_pixel(
                base,
                [gradient[0], gradient[1], gradient[2], 1.0],
                0.8,
                mode,
            );
            assert!(
                close_pixel(pixel, expected),
                "{mode:?} produced {pixel:?} rather than {expected:?}"
            );
            rendered.push(pixel);
        }
        assert_ne!(
            rendered[0], rendered[2],
            "Normal and Multiply produced the same pixel"
        );
    }

    #[test]
    fn four_color_gradient_coincident_points_are_the_mean() {
        let point = [8.5, 4.5];
        let grade = new_grade(
            [point; 4],
            DEFAULT_COLORS,
            100.0,
            0.0,
            100.0,
            BlendMode::Normal,
        );
        let mean = mean_of(DEFAULT_COLORS);
        for row in 0..6u32 {
            for column in 0..9u32 {
                let pixel = four_color_gradient_pixel(column, row, &grade);
                for channel in 0..3 {
                    assert!(
                        close(pixel[channel], mean[channel]),
                        "({column}, {row}) channel {channel} is {} rather than {}",
                        pixel[channel],
                        mean[channel]
                    );
                }
                assert!(pixel.iter().all(|value| value.is_finite()));
            }
        }

        // Collapsing a second point onto the first changes the result
        // continuously.
        let mut near = [point; 4];
        near[1] = [point[0] + 1.0e-3, point[1]];
        let near_grade = new_grade(near, DEFAULT_COLORS, 100.0, 0.0, 100.0, BlendMode::Normal);
        for (column, row) in [(0u32, 0u32), (4, 2), (8, 5)] {
            let before = four_color_gradient_pixel(column, row, &near_grade);
            let after = four_color_gradient_pixel(column, row, &grade);
            for channel in 0..3 {
                assert!(
                    (before[channel] - after[channel]).abs() <= 1.0e-4,
                    "({column}, {row}) channel {channel} jumped from {} to {}",
                    before[channel],
                    after[channel]
                );
            }
        }
    }

    #[test]
    fn four_color_gradient_points_outside_the_frame_still_render() {
        let mut distant = DEFAULT_POINTS;
        distant[0] = [-4_096.0, -4_096.0];
        let grade = new_grade(
            distant,
            DEFAULT_COLORS,
            100.0,
            0.0,
            100.0,
            BlendMode::Normal,
        );
        let inside = default_grade();
        let remainder = mean_of([
            [0.0; 3],
            DEFAULT_COLORS[1],
            DEFAULT_COLORS[2],
            DEFAULT_COLORS[3],
        ]);
        for row in 0..8u32 {
            for column in 0..8u32 {
                let centre = [column as f32 + 0.5, row as f32 + 0.5];
                let pixel = four_color_gradient_pixel(column, row, &grade);
                assert!(pixel.iter().all(|value| value.is_finite()));
                for value in pixel.iter().take(3) {
                    assert!((0.0..=1.0).contains(value));
                }
                assert!(
                    grade.weights(centre)[0] < inside.weights(centre)[0],
                    "the distant point kept its weight at ({column}, {row})"
                );
                // Dropping the first point can only move the colour towards the
                // mean of the remaining three.
                let kept = (pixel[0] - remainder[0]).abs()
                    + (pixel[1] - remainder[1]).abs()
                    + (pixel[2] - remainder[2]).abs();
                let owned = (pixel[0] - inside.apply([0.0, 0.0, 0.0, 1.0], column, row)[0]).abs();
                assert!(kept <= owned + 3.0);
            }
        }
    }

    #[test]
    fn four_color_gradient_rejects_invalid_grades() {
        let points = DEFAULT_POINTS;
        let colors = DEFAULT_COLORS;
        let build = |points, colors, blend, dither, opacity, blend_mode| {
            FourColorGradientGrade::new(points, colors, blend, dither, opacity, blend_mode)
        };
        let mut nan_point = points;
        nan_point[0][0] = f32::NAN;
        assert_eq!(
            build(nan_point, colors, 100.0, 0.0, 100.0, BlendMode::Normal),
            Err(FourColorGradientError::NonFinitePoint { index: 0 })
        );
        let mut far_point = points;
        far_point[1][1] = FOUR_COLOR_GRADIENT_MAX_POINT + 1.0;
        assert_eq!(
            build(far_point, colors, 100.0, 0.0, 100.0, BlendMode::Normal),
            Err(FourColorGradientError::PointOutOfRange { index: 1 })
        );
        let mut nan_color = colors;
        nan_color[2][0] = f32::NAN;
        assert_eq!(
            build(points, nan_color, 100.0, 0.0, 100.0, BlendMode::Normal),
            Err(FourColorGradientError::NonFiniteColor { index: 2 })
        );
        let mut bright_color = colors;
        bright_color[3][2] = 1.5;
        assert_eq!(
            build(points, bright_color, 100.0, 0.0, 100.0, BlendMode::Normal),
            Err(FourColorGradientError::ColorOutOfRange { index: 3 })
        );
        assert_eq!(
            build(points, colors, f32::NAN, 0.0, 100.0, BlendMode::Normal),
            Err(FourColorGradientError::NonFiniteBlend)
        );
        assert_eq!(
            build(points, colors, 4.9, 0.0, 100.0, BlendMode::Normal),
            Err(FourColorGradientError::BlendOutOfRange)
        );
        assert_eq!(
            build(points, colors, 10_000.1, 0.0, 100.0, BlendMode::Normal),
            Err(FourColorGradientError::BlendOutOfRange)
        );
        assert_eq!(
            build(points, colors, 100.0, f32::NAN, 100.0, BlendMode::Normal),
            Err(FourColorGradientError::NonFiniteDither)
        );
        assert_eq!(
            build(points, colors, 100.0, -1.0, 100.0, BlendMode::Normal),
            Err(FourColorGradientError::DitherOutOfRange)
        );
        assert_eq!(
            build(points, colors, 100.0, 501.0, 100.0, BlendMode::Normal),
            Err(FourColorGradientError::DitherOutOfRange)
        );
        assert_eq!(
            build(points, colors, 100.0, 0.0, f32::NAN, BlendMode::Normal),
            Err(FourColorGradientError::NonFiniteOpacity)
        );
        assert_eq!(
            build(points, colors, 100.0, 0.0, -1.0, BlendMode::Normal),
            Err(FourColorGradientError::OpacityOutOfRange)
        );
        assert_eq!(
            build(points, colors, 100.0, 0.0, 101.0, BlendMode::Normal),
            Err(FourColorGradientError::OpacityOutOfRange)
        );
        assert_eq!(
            build(points, colors, 100.0, 0.0, 100.0, BlendMode::Dissolve),
            Err(FourColorGradientError::DissolveUnsupported)
        );
        assert!(build(points, colors, 5.0, 0.0, 0.0, BlendMode::Luminosity).is_ok());
        assert!(build(points, colors, 10_000.0, 500.0, 100.0, BlendMode::Normal).is_ok());
    }

    #[test]
    fn four_color_gradient_kernel_writes_every_pixel_in_a_region() {
        let grade = default_grade();
        let (region_x, region_y, width, height) = (7usize, 3usize, 5usize, 4usize);
        let base = [0.0, 0.0, 0.0, 1.0];
        let mut region = vec![base; width * height];
        let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
        assert!(apply_four_color_gradient_f32_region_controlled(
            &mut region,
            region_x,
            region_y,
            width,
            height,
            &grade,
            &mut checkpoint,
        )
        .expect("infallible checkpoint"));
        for row in 0..height {
            for column in 0..width {
                let expected =
                    grade.apply(base, (region_x + column) as u32, (region_y + row) as u32);
                assert_eq!(
                    region[row * width + column].map(f32::to_bits),
                    expected.map(f32::to_bits),
                    "({column}, {row}) is not the frame-absolute pixel"
                );
            }
        }

        // Ignoring the region offset would produce a different frame.
        let mut origin = vec![base; width * height];
        let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
        assert!(apply_four_color_gradient_f32_region_controlled(
            &mut origin,
            0,
            0,
            width,
            height,
            &grade,
            &mut checkpoint,
        )
        .expect("infallible checkpoint"));
        assert_ne!(region, origin);
    }

    #[test]
    fn four_color_gradient_checkpoint_is_observed() {
        let grade = default_grade();
        let mut pixels = vec![[0.25, 0.5, 0.75, 1.0]; 20];
        let mut calls = 0usize;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), &'static str>(())
        };
        assert!(apply_four_color_gradient_f32_region_controlled(
            &mut pixels,
            0,
            0,
            5,
            4,
            &grade,
            &mut checkpoint,
        )
        .expect("infallible checkpoint"));
        assert_eq!(calls, 2);

        // A single column of 5000 pixels straddles two row chunks.
        let mut tall = vec![[0.25, 0.5, 0.75, 1.0]; 5_000];
        let mut calls = 0usize;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), &'static str>(())
        };
        assert!(apply_four_color_gradient_f32_region_controlled(
            &mut tall,
            0,
            0,
            1,
            5_000,
            &grade,
            &mut checkpoint,
        )
        .expect("infallible checkpoint"));
        assert_eq!(calls, 3);

        // An error on the second checkpoint aborts and is propagated verbatim:
        // the first 4096-row chunk was already written, the tail is untouched.
        let base = [0.25, 0.5, 0.75, 1.0];
        let mut pixels = vec![base; 5_000];
        let mut calls = 0usize;
        let mut failing = || {
            calls += 1;
            if calls == 2 {
                Err("cancelled")
            } else {
                Ok(())
            }
        };
        assert_eq!(
            apply_four_color_gradient_f32_region_controlled(
                &mut pixels,
                0,
                0,
                1,
                5_000,
                &grade,
                &mut failing
            ),
            Err("cancelled")
        );
        assert_eq!(calls, 2);
        assert!(pixels[..4_096].iter().all(|pixel| *pixel != base));
        assert!(pixels[4_096..].iter().all(|pixel| *pixel == base));
    }

    #[test]
    fn four_color_gradient_zero_coverage_frame_becomes_the_gradient() {
        let grade = default_grade();
        let (width, height) = (8usize, 6usize);
        let mut pixels = vec![[0.0; 4]; width * height];
        assert!(four_color_gradient_frame_f32(
            &mut pixels,
            width,
            height,
            &grade
        ));
        for (index, pixel) in pixels.iter().enumerate() {
            let column = (index % width) as u32;
            let row = (index / width) as u32;
            let gradient = four_color_gradient_pixel(column, row, &grade);
            assert_eq!(pixel[3], 1.0, "({column}, {row}) did not author coverage");
            for channel in 0..3 {
                assert!(
                    close(pixel[channel], gradient[channel]),
                    "({column}, {row}) channel {channel} is {} rather than {}",
                    pixel[channel],
                    gradient[channel]
                );
            }
            assert_ne!(*pixel, [0.0; 4]);
        }
    }
}
