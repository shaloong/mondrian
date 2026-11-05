//! JPEG damage: block quantisation, seam darkening and mosaic noise.
//!
//! [`JpegDamageGrade`] implements `builtin.jpeg_damage`, the point-channel
//! approximation of a badly encoded JPEG. The fused point pass sees one sample
//! and its absolute position, so the effect cannot run a block DCT: instead it
//! derives a block grid from the sample position and expresses the compression
//! artefacts geometrically.
//!
//! ```text
//! t  = (100 - q) / 100
//! dY = 0.125 * t²            dC = 2 * dY
//! A  = 0.0625 * t²           M  = 0.03125 * t²
//! ```
//!
//! The luma/chroma split is the integer Rec.709 variant already used elsewhere
//! in this crate (`cb = B - y`, `cr = R - y`). Both components are quantised to
//! the step `d`, the residual is attenuated by a block-local phase, and the
//! result is reconstructed with two multiply-adds. On top of that the first
//! row/column of every block is darkened (`edge`), the second is brightened
//! slightly (`ring`), and each block carries one constant signed mosaic offset.
//!
//! Everything is `+ - * /`, `floor` and `u32` bit operations: there is no
//! `pow`/`exp`/`log`, so the CPU kernel and the fused point shader can spell the
//! same expression. The grid is addressed from the absolute sample position, so
//! splitting a layer into tiles does not move the block boundaries.
//!
//! # Exact identity and endpoints
//!
//! Quantisation is skipped when the luma step is zero (`q = 100`); that branch
//! is read from the same value the shader branches on, so neither side ever
//! divides by zero. The strict four-tuple `q = 100, r = 1, a = 1, f = 1` is the
//! exact identity and returns the sample untouched, bit for bit, without
//! entering the formula at all. Any other configuration still runs the
//! reconstruction, which rounds even when it is mathematically neutral.
//!
//! # Deviations from JPEG
//!
//! * No DCT, no block average, no chroma subsampling: the artefacts are
//!   geometric and depend only on the sample, its position and the parameters.
//! * The quality curve is the quadratic above, not the standard quantisation
//!   tables; it is monotonic and pins both endpoints.
//! * Nothing is clamped. Extended-range samples stay extended, and the tests
//!   pin that: a bright input really does produce a channel above one.
//! * A sample without coverage (`a <= 0`, or `a` NaN) is returned unchanged.

/// How many pixels sit between two cooperative checkpoints.
///
/// The region kernel checkpoints per row chunk rather than per pixel, so a
/// cancellation is observed at roughly this cadence.
const CHECKPOINT_STRIDE: usize = 4_096;

/// The fused point kind the compositor dispatches on.
pub const JPEG_DAMAGE_KIND: u32 = 45;

/// The block edge at `resolution = 1`, in pixels.
pub const JPEG_DAMAGE_BLOCK_SIZE: f32 = 8.0;

/// The largest accepted block-size multiplier.
pub const JPEG_DAMAGE_MAX_RESOLUTION: f32 = 8.0;

/// The narrowest accepted macroblock aspect.
pub const JPEG_DAMAGE_MIN_MACROBLOCK_ASPECT: f32 = 0.25;

/// The widest accepted macroblock aspect.
pub const JPEG_DAMAGE_MAX_MACROBLOCK_ASPECT: f32 = 4.0;

/// The default quality, in percent.
pub const JPEG_DAMAGE_DEFAULT_QUALITY: f32 = 10.0;

/// The default block-size multiplier.
pub const JPEG_DAMAGE_DEFAULT_RESOLUTION: f32 = 1.0;

/// The default macroblock aspect.
pub const JPEG_DAMAGE_DEFAULT_MACROBLOCK_ASPECT: f32 = 1.0;

/// The default residual scale.
pub const JPEG_DAMAGE_DEFAULT_FREQUENCY_SCALE: f32 = 1.0;

/// The default scaling component.
pub const JPEG_DAMAGE_DEFAULT_COMPONENT: JpegDamageScalingComponent =
    JpegDamageScalingComponent::Chroma;

/// The luma step at full damage, as a fraction of `t²`.
const JPEG_DAMAGE_STEP_K: f32 = 0.125;

/// The chroma step relative to the luma step.
const JPEG_DAMAGE_CHROMA_RATIO: f32 = 2.0;

/// The seam-darkening amplitude at full damage, as a fraction of `t²`.
const JPEG_DAMAGE_EDGE_K: f32 = 0.0625;

/// The ring highlight relative to the seam amplitude.
const JPEG_DAMAGE_RING_RATIO: f32 = 0.5;

/// The mosaic amplitude at full damage, as a fraction of `t²`.
const JPEG_DAMAGE_MOSAIC_K: f32 = 0.03125;

/// The salt of the per-block mosaic hash.
const JPEG_DAMAGE_SALT_MOSAIC: u32 = 0x1F83_D9AB;

/// Rec.709 red luma coefficient, matching `noise` and `photo_filter`.
const JPEG_DAMAGE_LUMA_R: f32 = 0.2126;
/// Rec.709 green luma coefficient.
const JPEG_DAMAGE_LUMA_G: f32 = 0.7152;
/// Rec.709 blue luma coefficient.
const JPEG_DAMAGE_LUMA_B: f32 = 0.0722;

/// The green-channel coefficient of `cr`.
///
/// Written as the decimal literal rather than `0.2126 / 0.7152`: the two f32
/// bit patterns differ, and the shader uses this literal too.
const JPEG_DAMAGE_G_CR: f32 = 0.297_259_5;
/// The green-channel coefficient of `cb`, for the same reason.
const JPEG_DAMAGE_G_CB: f32 = 0.100_951_9;

/// Which component carries the residual attenuation.
///
/// Quantisation always applies to both luma and chroma; this only selects where
/// the *additional* frequency-scale attenuation is applied after quantisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JpegDamageScalingComponent {
    /// Attenuate the luma residual; chroma stays at its quantised value.
    Luma,
    /// Attenuate the chroma residual; luma stays at its quantised value.
    Chroma,
    /// Attenuate both residuals.
    Both,
}

impl JpegDamageScalingComponent {
    /// Every component, in menu order.
    pub const ALL: [Self; 3] = [Self::Luma, Self::Chroma, Self::Both];

    /// The value packed into `header.y`.
    #[must_use]
    pub const fn index(self) -> u32 {
        match self {
            Self::Luma => 0,
            Self::Chroma => 1,
            Self::Both => 2,
        }
    }

    /// The authoring key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Luma => "luma",
            Self::Chroma => "chroma",
            Self::Both => "both",
        }
    }

    /// The menu label, matching the zh-CN catalogue.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Luma => "亮度",
            Self::Chroma => "色度",
            Self::Both => "两者",
        }
    }

    /// Parses an authoring key; unknown keys are not coerced.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "luma" => Some(Self::Luma),
            "chroma" => Some(Self::Chroma),
            "both" => Some(Self::Both),
            _ => None,
        }
    }
}

/// Why a [`JpegDamageGrade`] could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JpegDamageError {
    /// The quality is not finite or sits outside `0..=100`.
    Quality,
    /// The resolution multiplier is not finite or sits outside `1..=8`.
    Resolution,
    /// The macroblock aspect is not finite or sits outside `0.25..=4`.
    MacroblockAspect,
    /// The residual scale is not finite or sits outside `0..=1`.
    FrequencyScale,
}

impl std::fmt::Display for JpegDamageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Quality => write!(
                formatter,
                "JPEG damage quality must be finite and within 0..=100"
            ),
            Self::Resolution => write!(
                formatter,
                "JPEG damage resolution must be finite and within 1..=8"
            ),
            Self::MacroblockAspect => write!(
                formatter,
                "JPEG damage macroblock aspect must be finite and within 0.25..=4"
            ),
            Self::FrequencyScale => write!(
                formatter,
                "JPEG damage frequency scale must be finite and within 0..=1"
            ),
        }
    }
}

impl std::error::Error for JpegDamageError {}

/// The parameters of one `builtin.jpeg_damage` node.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JpegDamageGrade {
    quality: f32,
    resolution: f32,
    macroblock_aspect: f32,
    frequency_scale: f32,
    scaling_component: JpegDamageScalingComponent,
}

impl Default for JpegDamageGrade {
    fn default() -> Self {
        Self {
            quality: JPEG_DAMAGE_DEFAULT_QUALITY,
            resolution: JPEG_DAMAGE_DEFAULT_RESOLUTION,
            macroblock_aspect: JPEG_DAMAGE_DEFAULT_MACROBLOCK_ASPECT,
            frequency_scale: JPEG_DAMAGE_DEFAULT_FREQUENCY_SCALE,
            scaling_component: JPEG_DAMAGE_DEFAULT_COMPONENT,
        }
    }
}

impl JpegDamageGrade {
    /// Builds a validated grade.
    ///
    /// # Errors
    ///
    /// Returns the variant matching the first parameter that is not finite or
    /// lies outside its documented range. Values are never silently clamped.
    pub fn new(
        quality: f32,
        resolution: f32,
        macroblock_aspect: f32,
        frequency_scale: f32,
        scaling_component: JpegDamageScalingComponent,
    ) -> Result<Self, JpegDamageError> {
        if !quality.is_finite() || !(0.0..=100.0).contains(&quality) {
            return Err(JpegDamageError::Quality);
        }
        if !resolution.is_finite() || !(1.0..=JPEG_DAMAGE_MAX_RESOLUTION).contains(&resolution) {
            return Err(JpegDamageError::Resolution);
        }
        if !macroblock_aspect.is_finite()
            || !(JPEG_DAMAGE_MIN_MACROBLOCK_ASPECT..=JPEG_DAMAGE_MAX_MACROBLOCK_ASPECT)
                .contains(&macroblock_aspect)
        {
            return Err(JpegDamageError::MacroblockAspect);
        }
        if !frequency_scale.is_finite() || !(0.0..=1.0).contains(&frequency_scale) {
            return Err(JpegDamageError::FrequencyScale);
        }
        Ok(Self {
            quality,
            resolution,
            macroblock_aspect,
            frequency_scale,
            scaling_component,
        })
    }

    /// The authored quality, in percent.
    #[must_use]
    pub const fn quality(&self) -> f32 {
        self.quality
    }

    /// The authored block-size multiplier.
    #[must_use]
    pub const fn resolution(&self) -> f32 {
        self.resolution
    }

    /// The authored macroblock aspect.
    #[must_use]
    pub const fn macroblock_aspect(&self) -> f32 {
        self.macroblock_aspect
    }

    /// The authored residual scale.
    #[must_use]
    pub const fn frequency_scale(&self) -> f32 {
        self.frequency_scale
    }

    /// The authored scaling component.
    #[must_use]
    pub const fn scaling_component(&self) -> JpegDamageScalingComponent {
        self.scaling_component
    }

    /// Whether this grade leaves every frame untouched.
    ///
    /// Only the strict four-tuple counts; a mathematically neutral grade such as
    /// `q = 100, r = 2` still runs the reconstruction and is not the identity.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.quality == 100.0
            && self.resolution == 1.0
            && self.macroblock_aspect == 1.0
            && self.frequency_scale == 1.0
    }

    /// The block extent in pixels, `(block_x, block_y)`.
    ///
    /// The base edge is `8 * resolution`; the aspect multiplies one axis and
    /// divides the other before both are rounded up, so a multiplier of `1.01`
    /// is visible immediately instead of collapsing onto the integer grid.
    #[must_use]
    pub fn block_extent(&self) -> (u32, u32) {
        let base = JPEG_DAMAGE_BLOCK_SIZE * self.resolution;
        let block_x = (base * self.macroblock_aspect).ceil().max(1.0);
        let block_y = (base / self.macroblock_aspect).ceil().max(1.0);
        (block_x as u32, block_y as u32)
    }

    /// The luma quantisation step; exactly zero at `q = 100`.
    fn luma_step(&self) -> f32 {
        let t = (100.0 - self.quality) / 100.0;
        JPEG_DAMAGE_STEP_K * (t * t)
    }

    /// The chroma quantisation step, twice the luma step.
    fn chroma_step(&self) -> f32 {
        self.luma_step() * JPEG_DAMAGE_CHROMA_RATIO
    }

    /// The seam-darkening amplitude.
    fn edge_amplitude(&self) -> f32 {
        let t = (100.0 - self.quality) / 100.0;
        JPEG_DAMAGE_EDGE_K * (t * t)
    }

    /// The per-block mosaic amplitude.
    fn mosaic_amplitude(&self) -> f32 {
        let t = (100.0 - self.quality) / 100.0;
        JPEG_DAMAGE_MOSAIC_K * (t * t)
    }

    /// The uniform payload for the fused point shader.
    ///
    /// `header = [45, component, block_x, block_y]`, `params = [dY, dC, s, A]`
    /// and `color = [M, 0, 0, 0]`, where `s = 1 - frequency_scale`. The block
    /// extent is computed here once so the shader never re-derives it and the
    /// two grids cannot drift apart.
    #[must_use]
    pub fn gpu_controls(&self) -> ([u32; 4], [f32; 4], [f32; 4]) {
        let (block_x, block_y) = self.block_extent();
        (
            [
                JPEG_DAMAGE_KIND,
                self.scaling_component.index(),
                block_x,
                block_y,
            ],
            [
                self.luma_step(),
                self.chroma_step(),
                1.0 - self.frequency_scale,
                self.edge_amplitude(),
            ],
            [self.mosaic_amplitude(), 0.0, 0.0, 0.0],
        )
    }

    /// Applies the grade to the sample at the frame origin.
    #[must_use]
    pub fn apply_pixel(&self, pixel: [f32; 4]) -> [f32; 4] {
        self.apply_pixel_at(0.0, 0.0, pixel)
    }

    /// Applies the grade to one straight-alpha sample at an absolute position.
    ///
    /// Negative coordinates are clamped to zero and the result is floored, so
    /// the whole negative quadrant addresses block zero.
    #[must_use]
    pub fn apply_pixel_at(&self, position_x: f32, position_y: f32, pixel: [f32; 4]) -> [f32; 4] {
        let x = position_x.max(0.0).floor() as u32;
        let y = position_y.max(0.0).floor() as u32;
        self.apply_sample(x, y, pixel)
    }

    /// Applies the grade to every sample of a `width x height` frame.
    pub fn apply_to_frame(&self, pixels: &mut [[f32; 4]], width: usize, height: usize) {
        let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
        let _ = apply_jpeg_damage_f32_region_controlled(
            pixels,
            0,
            0,
            width,
            height,
            self,
            &mut checkpoint,
        );
    }

    /// The per-sample kernel.
    ///
    /// The order of operations is the contract with the WGSL mirror: the same
    /// operands, in the same order, with the same branches.
    fn apply_sample(&self, x: u32, y: u32, pixel: [f32; 4]) -> [f32; 4] {
        if pixel[3].is_nan() || pixel[3] <= 0.0 {
            return pixel;
        }
        if self.is_identity() {
            return pixel;
        }
        let (block_x, block_y) = self.block_extent();
        let cell_x = x / block_x;
        let cell_y = y / block_y;
        let local_x = x - cell_x * block_x;
        let local_y = y - cell_y * block_y;

        let luma = JPEG_DAMAGE_LUMA_R * pixel[0]
            + JPEG_DAMAGE_LUMA_G * pixel[1]
            + JPEG_DAMAGE_LUMA_B * pixel[2];
        let cb = pixel[2] - luma;
        let cr = pixel[0] - luma;

        // `q = 100` makes the luma step exactly zero; the shader branches on the
        // same value (`params.x > 0`) instead of duplicating the quality check.
        let (y_quantized, cb_quantized, cr_quantized) = if self.luma_step() > 0.0 {
            (
                quantize(luma, self.luma_step()),
                quantize(cb, self.chroma_step()),
                quantize(cr, self.chroma_step()),
            )
        } else {
            (luma, cb, cr)
        };

        let phase_x = (local_x * 2 >= block_x) as u32;
        let phase_y = (local_y * 2 >= block_y) as u32;
        let attenuation =
            1.0 - (1.0 - self.frequency_scale) * (0.5 + 0.5 * (phase_x ^ phase_y) as f32);

        let mut y_out = y_quantized;
        let mut cb_out = cb_quantized;
        let mut cr_out = cr_quantized;
        if self.scaling_component != JpegDamageScalingComponent::Chroma {
            y_out = y_quantized + (luma - y_quantized) * attenuation;
        }
        if self.scaling_component != JpegDamageScalingComponent::Luma {
            cb_out = cb_quantized * attenuation;
            cr_out = cr_quantized * attenuation;
        }

        let red = y_out + cr_out;
        let blue = y_out + cb_out;
        let green = y_out - (JPEG_DAMAGE_G_CR * cr_out + JPEG_DAMAGE_G_CB * cb_out);

        let mut edge = 0.0;
        let mut ring = 0.0;
        if local_x == 0 || local_y == 0 {
            edge = 1.0;
        }
        if (local_x == 1 && block_x > 1) || (local_y == 1 && block_y > 1) {
            ring = 1.0;
        }
        let gain = (1.0 - self.edge_amplitude() * edge)
            + (JPEG_DAMAGE_RING_RATIO * self.edge_amplitude() * ring);
        let mosaic = crate::noise::noise_sample(cell_x, cell_y, JPEG_DAMAGE_SALT_MOSAIC)
            * self.mosaic_amplitude();
        [
            red * gain + mosaic,
            green * gain + mosaic,
            blue * gain + mosaic,
            pixel[3],
        ]
    }
}

/// `Q(v, d) = floor(v / d + 0.5) * d`, rounding ties up.
fn quantize(value: f32, step: f32) -> f32 {
    (value / step + 0.5).floor() * step
}

/// Applies a grade to a region, yielding to `checkpoint` between row chunks.
///
/// `region_x` and `region_y` are the *absolute* pixel origin of `pixels`, not an
/// offset inside the region: the block grid is addressed from frame coordinates
/// so a tiled render cannot shift it. The error type is the caller's
/// cancellation error, so a stop unwinds with its own type.
pub(crate) fn apply_jpeg_damage_f32_region_controlled<E>(
    pixels: &mut [[f32; 4]],
    region_x: usize,
    region_y: usize,
    width: usize,
    height: usize,
    grade: &JpegDamageGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if grade.is_identity() {
        return Ok(true);
    }
    if width == 0 || height == 0 {
        return Ok(true);
    }
    debug_assert_eq!(pixels.len(), width * height);
    let rows_per_chunk = (CHECKPOINT_STRIDE / width).max(1);
    let mut start = 0usize;
    while start < height {
        checkpoint()?;
        let end = (start + rows_per_chunk).min(height);
        for row in start..end {
            let base = row * width;
            for (column, pixel) in pixels[base..base + width].iter_mut().enumerate() {
                let x = region_x as u32 + column as u32;
                let y = region_y as u32 + row as u32;
                *pixel = grade.apply_sample(x, y, *pixel);
            }
        }
        start = end;
    }
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: f32, expected: f32, tolerance: f32) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {expected}, got {actual}"
        );
    }

    fn bits(pixel: [f32; 4]) -> [u32; 4] {
        pixel.map(f32::to_bits)
    }

    fn grade(
        quality: f32,
        resolution: f32,
        aspect: f32,
        frequency: f32,
        component: JpegDamageScalingComponent,
    ) -> JpegDamageGrade {
        JpegDamageGrade::new(quality, resolution, aspect, frequency, component)
            .expect("valid grade")
    }

    /// K1: the strict four-tuple is a bit-exact identity for every sample.
    #[test]
    fn identity_grade_returns_every_sample_bit_for_bit() {
        let grade = grade(100.0, 1.0, 1.0, 1.0, JpegDamageScalingComponent::Both);
        assert!(grade.is_identity());
        let frame = [
            [-0.0, 0.0, 0.0, 1.0],
            [-1.5, 2.0, 0.25, 0.5],
            [0.0, 0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0, 1.0],
            [0.125, 0.5, 0.875, 0.25],
            [-0.0, -0.0, -0.0, -0.0],
            [3.0, -2.0, 0.5, 2.0],
            [0.5, 0.5, 0.5, 1.0e-12],
        ];
        for (index, source) in frame.iter().enumerate() {
            let mapped = grade.apply_pixel_at(7.0, 11.0, *source);
            assert_eq!(bits(mapped), bits(*source), "pixel {index}");
        }
        let mut copied = frame;
        grade.apply_to_frame(&mut copied, 4, 2);
        for (index, pixel) in copied.iter().enumerate() {
            assert_eq!(bits(*pixel), bits(frame[index]), "pixel {index}");
        }
    }

    /// K2: the default grade darkens the block corner by the example amount.
    #[test]
    fn default_grade_darkens_the_block_corner() {
        let grade = JpegDamageGrade::default();
        let mapped = grade.apply_pixel_at(0.0, 0.0, [0.5, 0.5, 0.5, 1.0]);
        for (index, channel) in mapped.iter().take(3).enumerate() {
            assert_eq!(
                channel.to_bits(),
                0x3eee_e1d8,
                "channel {index} = {channel}"
            );
        }
        assert_eq!(mapped[3], 1.0);
    }

    /// K3: the block extent rounds up and scales with the aspect.
    #[test]
    fn block_extent_rounds_up_and_scales_with_aspect() {
        let cases = [
            ((1.0, 1.0), (8, 8)),
            ((1.01, 1.0), (9, 9)),
            ((2.0, 0.5), (8, 32)),
            ((2.0, 4.0), (64, 4)),
            ((8.0, 4.0), (256, 16)),
        ];
        for ((resolution, aspect), expected) in cases {
            let grade = grade(
                50.0,
                resolution,
                aspect,
                1.0,
                JpegDamageScalingComponent::Chroma,
            );
            assert_eq!(
                grade.block_extent(),
                expected,
                "resolution {resolution} aspect {aspect}"
            );
        }
    }

    /// K4: quantisation rounds ties up, so 1.5 steps lands on two.
    #[test]
    fn luma_quantization_rounds_ties_up() {
        let grade = grade(50.0, 1.0, 1.0, 1.0, JpegDamageScalingComponent::Chroma);
        let source = [0.046_875, 0.046_875, 0.046_875, 1.0];
        assert_eq!(grade.luma_step(), 0.03125);
        let mapped = grade.apply_pixel_at(3.0, 3.0, source);
        assert_eq!(mapped[0].to_bits(), 0x3d6e_3b8e);
        assert_eq!(mapped[1].to_bits(), 0x3d6e_3b8e);
        assert_eq!(mapped[2].to_bits(), 0x3d6e_3b8e);
    }

    /// K5: the pinned chroma example reconstructs to the documented triple.
    #[test]
    fn chroma_quantization_reconstructs_the_pinned_pixel() {
        let grade = grade(50.0, 1.0, 1.0, 0.5, JpegDamageScalingComponent::Both);
        let source = [0.75, 0.5, 0.25, 0.7];
        let mapped = grade.apply_pixel_at(3.0, 3.0, source);
        close(mapped[0], 0.670_424_8, 1.0e-6);
        close(mapped[1], 0.511_658_3, 1.0e-6);
        close(mapped[2], 0.295_424_8, 1.0e-6);
        assert_eq!(mapped[3].to_bits(), source[3].to_bits());
    }

    /// K6: chroma steps twice as fast, and a zero step zeroes all amplitudes.
    #[test]
    fn chroma_step_is_twice_the_luma_step() {
        let stepped = grade(50.0, 1.0, 1.0, 0.5, JpegDamageScalingComponent::Chroma);
        assert_eq!(stepped.gpu_controls().1, [0.03125, 0.0625, 0.5, 0.015625]);
        let neutral = grade(100.0, 1.0, 1.0, 1.0, JpegDamageScalingComponent::Chroma);
        assert_eq!(neutral.gpu_controls().1, [0.0, 0.0, 0.0, 0.0]);
    }

    /// K7: with no residual scale the high-phase quadrant collapses to luma.
    #[test]
    fn zero_frequency_scale_flattens_the_high_phase_quadrant() {
        let grade = grade(50.0, 1.0, 1.0, 0.0, JpegDamageScalingComponent::Chroma);
        let source = [0.75, 0.5, 0.25, 1.0];
        let flat = grade.apply_pixel_at(5.0, 3.0, source);
        for (index, channel) in flat.iter().take(3).enumerate() {
            close(*channel, 0.526_912_3, 1.0e-6);
            assert_eq!(flat[index], flat[0], "channel {index} must match red");
        }
        let uneven = grade.apply_pixel_at(3.0, 3.0, source);
        assert!(
            !(uneven[0] == uneven[1] && uneven[1] == uneven[2]),
            "the low-phase quadrant must keep its chroma"
        );
    }

    /// K8: the component menu selects three distinct, documented results.
    #[test]
    fn scaling_component_selects_luma_chroma_or_both() {
        let source = [0.75, 0.5, 0.25, 1.0];
        let luma = grade(50.0, 1.0, 1.0, 0.0, JpegDamageScalingComponent::Luma)
            .apply_pixel_at(3.0, 3.0, source);
        let chroma = grade(50.0, 1.0, 1.0, 0.0, JpegDamageScalingComponent::Chroma)
            .apply_pixel_at(3.0, 3.0, source);
        let both = grade(50.0, 1.0, 1.0, 0.0, JpegDamageScalingComponent::Both)
            .apply_pixel_at(3.0, 3.0, source);
        close(luma[0], 0.716_337_26, 1.0e-6);
        close(luma[1], 0.504_648_57, 1.0e-6);
        close(luma[2], 0.216_337_26, 1.0e-6);
        close(chroma[0], 0.620_662_3, 1.0e-6);
        close(chroma[1], 0.514_817_95, 1.0e-6);
        close(chroma[2], 0.370_662_27, 1.0e-6);
        close(both[0], 0.622_587_26, 1.0e-6);
        close(both[1], 0.516_742_94, 1.0e-6);
        close(both[2], 0.372_587_26, 1.0e-6);
        assert!((luma[0] - chroma[0]).abs() > 0.09);
        assert!((both[0] - chroma[0]).abs() > 1.0e-3);
        assert!((both[0] - luma[0]).abs() > 1.0e-3);
    }

    /// K9: the seam darkens the first row and the ring brightens the second.
    #[test]
    fn block_grid_darkens_the_first_row_and_ring_highlights_the_second() {
        let grade = grade(50.0, 1.0, 1.0, 1.0, JpegDamageScalingComponent::Chroma);
        let source = [0.5, 0.5, 0.5, 1.0];
        let seam = grade.apply_pixel_at(0.0, 3.0, source);
        let ring = grade.apply_pixel_at(1.0, 3.0, source);
        let middle = grade.apply_pixel_at(3.0, 3.0, source);
        for channel in seam.iter().take(3) {
            close(*channel, 0.487_849_77, 1.0e-6);
        }
        for channel in ring.iter().take(3) {
            close(*channel, 0.499_568_52, 1.0e-6);
        }
        for channel in middle.iter().take(3) {
            close(*channel, 0.495_662_27, 1.0e-6);
        }
        assert!(seam[0] < middle[0]);
        assert!(ring[0] > middle[0]);
    }

    /// K10: the mosaic offset is constant inside a block and jumps across.
    #[test]
    fn mosaic_offset_is_constant_inside_a_block() {
        let grade = grade(0.0, 1.0, 1.0, 1.0, JpegDamageScalingComponent::Chroma);
        let source = [0.5, 0.5, 0.5, 1.0];
        let first = grade.apply_pixel_at(2.0, 2.0, source);
        let same_block = grade.apply_pixel_at(6.0, 6.0, source);
        let next_block = grade.apply_pixel_at(2.0, 10.0, source);
        assert_eq!(bits(first), bits(same_block));
        assert_ne!(bits(first), bits(next_block));
        close((next_block[0] - first[0]).abs(), 0.002_863_927, 1.0e-6);
    }

    /// K11: the region origin addresses the absolute block grid.
    #[test]
    fn region_origin_is_absolute_for_block_addressing() {
        let grade = grade(0.0, 1.0, 1.0, 1.0, JpegDamageScalingComponent::Chroma);
        let source = [0.5, 0.5, 0.5, 1.0];
        let mut region = [[0.0f32; 4]; 16];
        region.iter_mut().for_each(|pixel| *pixel = source);
        let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
        assert!(apply_jpeg_damage_f32_region_controlled(
            &mut region,
            8,
            0,
            16,
            1,
            &grade,
            &mut checkpoint,
        )
        .is_ok());
        assert_eq!(
            bits(region[0]),
            bits(grade.apply_pixel_at(8.0, 0.0, source))
        );
        assert_ne!(
            bits(region[0]),
            bits(grade.apply_pixel_at(0.0, 0.0, source))
        );
    }

    /// K12: alpha is passed through and uncovered samples are skipped.
    #[test]
    fn alpha_is_never_modified_and_zero_coverage_is_skipped() {
        let grade = grade(0.0, 1.0, 1.0, 1.0, JpegDamageScalingComponent::Both);
        for alpha in [0.0, 0.5, 1.0] {
            let source = [0.25, 0.75, 0.125, alpha];
            let mapped = grade.apply_pixel_at(4.0, 4.0, source);
            assert_eq!(mapped[3].to_bits(), alpha.to_bits());
        }
        for alpha in [0.0, -0.0, f32::NAN] {
            let source = [1.0, 0.0, 0.0, alpha];
            let mapped = grade.apply_pixel_at(0.0, 0.0, source);
            assert_eq!(bits(mapped), bits(source), "alpha {alpha}");
        }
    }

    /// K13: extended-range results are not clamped.
    #[test]
    fn out_of_range_colours_do_not_clamp() {
        let grade = grade(50.0, 1.0, 1.0, 1.0, JpegDamageScalingComponent::Chroma);
        let mapped = grade.apply_pixel_at(1.0, 1.0, [1.5, -0.25, 0.5, 1.0]);
        assert!(mapped[0] > 1.0, "red must stay above one: {}", mapped[0]);
        assert!(mapped[1] < 0.0, "green must stay below zero: {}", mapped[1]);
    }

    /// K14: construction validates every numeric parameter and pins the text.
    #[test]
    fn grade_rejects_non_finite_and_out_of_range_parameters() {
        use JpegDamageScalingComponent::Chroma;

        let invalid = [
            (-0.5, JpegDamageError::Quality),
            (100.5, JpegDamageError::Quality),
            (f32::NAN, JpegDamageError::Quality),
            (f32::INFINITY, JpegDamageError::Quality),
        ];
        for (quality, expected) in invalid {
            assert_eq!(
                JpegDamageGrade::new(quality, 1.0, 1.0, 1.0, Chroma).unwrap_err(),
                expected
            );
        }
        let invalid = [
            (0.5, JpegDamageError::Resolution),
            (8.5, JpegDamageError::Resolution),
            (f32::NEG_INFINITY, JpegDamageError::Resolution),
        ];
        for (resolution, expected) in invalid {
            assert_eq!(
                JpegDamageGrade::new(50.0, resolution, 1.0, 1.0, Chroma).unwrap_err(),
                expected
            );
        }
        let invalid = [
            (0.2, JpegDamageError::MacroblockAspect),
            (4.5, JpegDamageError::MacroblockAspect),
            (f32::NAN, JpegDamageError::MacroblockAspect),
        ];
        for (aspect, expected) in invalid {
            assert_eq!(
                JpegDamageGrade::new(50.0, 1.0, aspect, 1.0, Chroma).unwrap_err(),
                expected
            );
        }
        let invalid = [
            (-0.1, JpegDamageError::FrequencyScale),
            (1.1, JpegDamageError::FrequencyScale),
            (f32::INFINITY, JpegDamageError::FrequencyScale),
        ];
        for (frequency, expected) in invalid {
            assert_eq!(
                JpegDamageGrade::new(50.0, 1.0, 1.0, frequency, Chroma).unwrap_err(),
                expected
            );
        }
        assert!(JpegDamageGrade::new(0.0, 1.0, 0.25, 0.0, Chroma).is_ok());
        assert!(JpegDamageGrade::new(100.0, 8.0, 4.0, 1.0, Chroma).is_ok());
        assert_eq!(
            JpegDamageError::Quality.to_string(),
            "JPEG damage quality must be finite and within 0..=100"
        );
        assert_eq!(
            JpegDamageError::Resolution.to_string(),
            "JPEG damage resolution must be finite and within 1..=8"
        );
        assert_eq!(
            JpegDamageError::MacroblockAspect.to_string(),
            "JPEG damage macroblock aspect must be finite and within 0.25..=4"
        );
        assert_eq!(
            JpegDamageError::FrequencyScale.to_string(),
            "JPEG damage frequency scale must be finite and within 0..=1"
        );
    }

    /// K15: the checkpoint cadence is one per row chunk plus a trailing one.
    #[test]
    fn checkpoint_runs_once_per_chunk_and_propagates_errors() {
        let width = 4096;
        let height = 2;
        let mut identity_pixels = vec![[0.5f32, 0.5, 0.5, 1.0]; width * height];
        let identity = grade(100.0, 1.0, 1.0, 1.0, JpegDamageScalingComponent::Chroma);
        let mut calls = 0usize;
        {
            let mut checkpoint = || {
                calls += 1;
                Ok::<(), &'static str>(())
            };
            assert!(apply_jpeg_damage_f32_region_controlled(
                &mut identity_pixels,
                0,
                0,
                width,
                height,
                &identity,
                &mut checkpoint,
            )
            .is_ok());
        }
        assert_eq!(calls, 0, "identity must not reach a checkpoint");

        let mut pixels = vec![[0.5f32, 0.5, 0.5, 1.0]; width * height];
        let damaged = grade(50.0, 1.0, 1.0, 1.0, JpegDamageScalingComponent::Chroma);
        let mut calls = 0usize;
        {
            let mut checkpoint = || {
                calls += 1;
                Ok::<(), &'static str>(())
            };
            assert!(apply_jpeg_damage_f32_region_controlled(
                &mut pixels,
                0,
                0,
                width,
                height,
                &damaged,
                &mut checkpoint,
            )
            .is_ok());
        }
        assert_eq!(calls, 3);

        let mut pixels = vec![[0.5f32, 0.5, 0.5, 1.0]; width * height];
        let mut calls = 0usize;
        {
            let mut checkpoint = || {
                calls += 1;
                if calls == 2 {
                    Err("abort")
                } else {
                    Ok(())
                }
            };
            assert_eq!(
                apply_jpeg_damage_f32_region_controlled(
                    &mut pixels,
                    0,
                    0,
                    width,
                    height,
                    &damaged,
                    &mut checkpoint,
                ),
                Err("abort")
            );
        }
        assert_eq!(calls, 2);
        for pixel in pixels.iter().take(width) {
            assert_ne!(bits(*pixel), bits([0.5, 0.5, 0.5, 1.0]));
        }
        for pixel in pixels.iter().skip(width) {
            assert_eq!(bits(*pixel), bits([0.5, 0.5, 0.5, 1.0]));
        }
    }

    /// K16: the uniform carries the documented cells.
    #[test]
    fn gpu_controls_pack_the_documented_uniform() {
        let grade = grade(50.0, 2.0, 0.5, 0.25, JpegDamageScalingComponent::Both);
        let (header, params, color) = grade.gpu_controls();
        assert_eq!(header, [45, 2, 8, 32]);
        assert_eq!(params, [0.03125, 0.0625, 0.75, 0.015625]);
        assert_eq!(color, [0.0078125, 0.0, 0.0, 0.0]);
    }
}
