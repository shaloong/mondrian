//! Dust & Scratches kernel for the Dust & Scratches built-in effect.
//!
//! The effect repairs dust specks, hair and small blemishes with a gated
//! selective median: the per-channel median of the `(2r + 1)²` window is
//! computed exactly the way the Median effect computes it, but it replaces the
//! centre pixel only when the centre disagrees with that median by more than the
//! authored threshold. Both endpoints are meaningful, which is why the semantics
//! are defensible: a threshold of zero degenerates to the plain median (and is
//! bit-for-bit identical to `builtin.median` on an eight-bit source), while a
//! threshold of one or more is the identity.
//!
//! # Quantised domain
//!
//! The rank selection reuses the eight-bit histogram machinery of
//! [`crate::median`] instead of duplicating it: the quantisation is a shared
//! bit-level contract between two effects, and a copy would drift. Every sample
//! maps to `round(clamp01(value) * 255)` and every repaired sample comes back as
//! `f32(bin) / 255.0`. `NaN` takes the bottom bin, values above one take the top
//! bin.
//!
//! # Gate
//!
//! The gate compares the centre against the median on the *clamped* centre value,
//! never on the raw sample:
//!
//! ```text
//! gate(v) = if v != v { 0.0 } else { clamp(v, 0.0, 1.0) }
//! delta   = max(|gate(centre.r) - med.r|, |gate(centre.g) - med.g|, |gate(centre.b) - med.b|)
//! if operate_on_alpha { delta = max(delta, |gate(centre.a) - med.a|) }
//! repair  = delta > threshold
//! ```
//!
//! The `NaN` guard is required, not defensive: Rust's `f32::clamp` propagates
//! `NaN` while WGSL's `clamp` returns the upper bound on some drivers, so the two
//! backends would diverge. Mapping `NaN` to zero is the one value both can agree
//! on, and it is also the bin `median_bin` already assigns to `NaN`.
//!
//! Only `repair` writes the median. Every other pixel is copied bit for bit, so
//! an out-of-range sample survives the pass untouched: a 1.5 highlight stays 1.5
//! because `gate(1.5) = 1.0` equals the median `1.0`, no repair happens, and the
//! kept branch does not clamp.
//!
//! # Alpha handling
//!
//! Straight alpha, no premultiplication — same ruling as [`crate::median`],
//! because a rank filter selects one sample and weighting the samples by coverage
//! first would reorder them. When the toggle is off the alpha channel is copied
//! from the centre pixel bit for bit, whether or not the pixel was repaired. When
//! the toggle is on alpha joins the same window and the same rank, and a repair
//! takes the median alpha.
//!
//! # Identity
//!
//! [`DustScratchesGrade::is_identity`] is `radius < 0.5 || threshold >= 1.0`.
//! Both arms have to short-circuit the whole op in the graph builder: a single
//! tap window is `median_bin(centre) / 255` and is *not* the centre for a sample
//! between grid points, so leaving the node in the graph with radius zero would
//! re-quantise the frame and change it.
//!
//! # GPU packing
//!
//! [`DustScratchesGrade::gpu_controls`] returns `params = [radius, threshold,
//! operate_on_alpha, 0]` and `extra = [0; 4]`. The renderer replaces `params[0]`
//! with the frame-clamped pixel radius, so the shader sees the same integer as
//! [`DustScratchesGrade::radius_pixels`]. The GPU pass lowers radii up to
//! [`DUST_SCRATCHES_GPU_MAX_RADIUS`]; larger windows stay on the CPU with an
//! identical picture, and the bound lives in the renderer's admission step so a
//! chain is rejected before it records a single pass.
//!
//! # Performance
//!
//! Cost per pixel is `O(r)` taps plus `O(1)` selection: the initial window per
//! row is `(2r + 1)²` taps, each remaining column is `2(2r + 1)` taps. Row bands
//! run in parallel through [`crate::parallel::for_each_row_band`]; each band owns
//! its histograms, so the result does not depend on the thread count.
//!
//! # Documented deviations
//!
//! 1. Samples are quantised to eight bits before the rank selection, so a
//!    repaired pixel moves to the nearest grid point (at most `1/510` away).
//! 2. Out-of-frame taps repeat the boundary pixel; there is no other mode.
//! 3. The alpha toggle is stored as a float in `[0, 1]` and read as `>= 0.5`
//!    rather than as a boolean property.
//! 4. The GPU pass lowers radii up to three pixels. Larger radii fall back to the
//!    CPU kernel, which produces the same frame.
//! 5. The authored radius defaults to one pixel, not zero, so adding the effect
//!    already filters. The Median effect defaults to zero.
//!
//! # Tests
//!
//! The kernel tests pin the two degenerate endpoints (the median at threshold
//! zero, the identity at radius zero and at threshold one), the strict `>`
//! comparison, both alpha modes, the verbatim survival of out-of-range samples,
//! the frame clamp of an oversized radius, the checkpoint cadence, and a
//! brute-force sort of the same window on random eight-bit data.

use crate::median::{bin_pixel, clamp_coord, median_value, window_rank, WindowHistograms};

/// Largest authored radius in pixels.
pub const DUST_SCRATCHES_MAX_RADIUS: u32 = 255;
/// Default authored radius: one pixel, so adding the effect already filters.
pub const DUST_SCRATCHES_DEFAULT_RADIUS: f32 = 1.0;
/// Default gate: a threshold of zero keeps every median repair.
pub const DUST_SCRATCHES_DEFAULT_THRESHOLD: f32 = 0.0;
/// Default alpha handling: copy the centre alpha.
pub const DUST_SCRATCHES_DEFAULT_OPERATE_ON_ALPHA: f32 = 0.0;
/// Largest radius the GPU pass lowers; larger radii stay on the CPU.
pub const DUST_SCRATCHES_GPU_MAX_RADIUS: u32 = 3;

/// Pixels between two sequential checkpoint calls while staging.
const CHECKPOINT_STRIDE: usize = 4096;

/// Rejected authored Dust & Scratches state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DustScratchesError {
    /// The radius is not a finite number.
    NonFiniteRadius,
    /// The radius is outside the authored range.
    RadiusOutOfRange,
    /// The threshold is not a finite number.
    NonFiniteThreshold,
    /// The threshold is outside the authored range.
    ThresholdOutOfRange,
    /// The alpha toggle is not a finite number.
    NonFiniteOperateOnAlpha,
    /// The alpha toggle is outside the authored range.
    OperateOnAlphaOutOfRange,
}

impl std::fmt::Display for DustScratchesError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::NonFiniteRadius => "dust & scratches radius must be a finite number",
            Self::RadiusOutOfRange => "dust & scratches radius must be inside [0, 255] pixels",
            Self::NonFiniteThreshold => "dust & scratches threshold must be a finite number",
            Self::ThresholdOutOfRange => "dust & scratches threshold must be inside [0, 1]",
            Self::NonFiniteOperateOnAlpha => {
                "dust & scratches operate on alpha must be a finite number"
            }
            Self::OperateOnAlphaOutOfRange => {
                "dust & scratches operate on alpha must be inside [0, 1]"
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for DustScratchesError {}

/// The authored parameters of one Dust & Scratches instance.
///
/// The radius stays authored (`f32`) so the hash sees exactly what the project
/// stored; the pixel radius is derived per frame by [`Self::radius_pixels`],
/// which clamps it to the frame extent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DustScratchesGrade {
    radius: f32,
    threshold: f32,
    operate_on_alpha: f32,
}

impl DustScratchesGrade {
    /// Validates one instance against the documented parameter ranges.
    ///
    /// # Errors
    ///
    /// Returns [`DustScratchesError`] when the radius is not finite or rounds
    /// outside `[0, 255]`, when the threshold is not finite or lies outside
    /// `[0, 1]`, and when the alpha toggle is not finite or lies outside
    /// `[0, 1]`.
    pub fn new(
        radius: f32,
        threshold: f32,
        operate_on_alpha: f32,
    ) -> Result<Self, DustScratchesError> {
        if !radius.is_finite() {
            return Err(DustScratchesError::NonFiniteRadius);
        }
        let rounded = radius.round();
        if !(0.0..=DUST_SCRATCHES_MAX_RADIUS as f32).contains(&rounded) {
            return Err(DustScratchesError::RadiusOutOfRange);
        }
        if !threshold.is_finite() {
            return Err(DustScratchesError::NonFiniteThreshold);
        }
        if !(0.0..=1.0).contains(&threshold) {
            return Err(DustScratchesError::ThresholdOutOfRange);
        }
        if !operate_on_alpha.is_finite() {
            return Err(DustScratchesError::NonFiniteOperateOnAlpha);
        }
        if !(0.0..=1.0).contains(&operate_on_alpha) {
            return Err(DustScratchesError::OperateOnAlphaOutOfRange);
        }
        Ok(Self { radius, threshold, operate_on_alpha })
    }

    /// The documented defaults: a one-pixel window at threshold zero.
    #[must_use]
    pub const fn documented_defaults() -> Self {
        Self {
            radius: DUST_SCRATCHES_DEFAULT_RADIUS,
            threshold: DUST_SCRATCHES_DEFAULT_THRESHOLD,
            operate_on_alpha: DUST_SCRATCHES_DEFAULT_OPERATE_ON_ALPHA,
        }
    }

    /// The authored radius, in pixels.
    #[must_use]
    pub const fn radius(self) -> f32 {
        self.radius
    }

    /// The authored maximum channel deviation that repairs a pixel.
    #[must_use]
    pub const fn threshold(self) -> f32 {
        self.threshold
    }

    /// The authored alpha toggle, in `[0, 1]`.
    #[must_use]
    pub const fn operate_on_alpha(self) -> f32 {
        self.operate_on_alpha
    }

    /// The pixel radius this instance applies to a frame of the given extent.
    ///
    /// The authored radius is rounded to the nearest pixel and clamped to
    /// `max(width, height)`, the largest window the frame can hold. A frame with
    /// a zero extent therefore yields a zero radius.
    #[must_use]
    pub fn radius_pixels(self, width: u32, height: u32) -> u32 {
        let limit = width.max(height);
        let rounded = self.radius.round();
        let radius = if rounded > 0.0 { rounded as u32 } else { 0 };
        radius.min(limit)
    }

    /// Whether the alpha channel joins the rank filter and the gate.
    ///
    /// `operate_on_alpha >= 0.5` filters alpha with the colour channels;
    /// anything below copies the centre alpha instead.
    #[must_use]
    pub const fn operates_on_alpha(self) -> bool {
        self.operate_on_alpha >= 0.5
    }

    /// Whether this instance leaves every pixel untouched.
    #[must_use]
    pub const fn is_identity(self) -> bool {
        // `f32::round` is not const, so compare the authored radius directly:
        // every radius that rounds to zero is below 0.5.
        self.radius < 0.5 || self.threshold >= 1.0
    }

    /// Whether the GPU pass can lower this instance.
    ///
    /// Admission happens before any frame extent is known, so the authored
    /// radius is rounded and compared against [`DUST_SCRATCHES_GPU_MAX_RADIUS`]
    /// rather than the frame-clamped pixel radius.
    #[must_use]
    pub fn gpu_supported(self) -> bool {
        let rounded = self.radius.round();
        let radius = if rounded > 0.0 { rounded as u32 } else { 0 };
        radius <= DUST_SCRATCHES_GPU_MAX_RADIUS
    }

    /// The `(params, extra)` pair the renderer uploads.
    ///
    /// `params = [radius, threshold, operate_on_alpha, 0]` carries the authored
    /// values; the renderer overwrites `params[0]` with the frame-clamped pixel
    /// radius and the fragment shader reads the toggle as `>= 0.5`. `extra` is
    /// unused and stays zero.
    #[must_use]
    pub fn gpu_controls(self) -> ([f32; 4], [f32; 4]) {
        (
            [self.radius, self.threshold, self.operate_on_alpha, 0.0],
            [0.0; 4],
        )
    }

    /// Feeds every authored value into a semantic hash.
    pub(crate) fn hash_state<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash as _;
        self.radius.to_bits().hash(state);
        self.threshold.to_bits().hash(state);
        self.operate_on_alpha.to_bits().hash(state);
    }
}

/// Applies one authored Dust & Scratches instance to a straight-alpha frame.
///
/// Returns `false` for a frame whose geometry cannot be addressed by `pixels`
/// (`width * height` must fit, be non-zero, and be covered by the slice).
#[must_use]
pub fn dust_scratches_frame_f32(
    pixels: &mut [[f32; 4]],
    width: u32,
    height: u32,
    grade: &DustScratchesGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_dust_scratches_f32_controlled(pixels, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

/// Applies one Dust & Scratches instance while polling `checkpoint`.
///
/// `checkpoint` runs before the frame is staged, every [`CHECKPOINT_STRIDE`]
/// pixels while staging, once after staging and once after the window pass. A
/// checkpoint error aborts the frame immediately; because the pass writes into
/// the frame only after staging completes, an error raised up to and including
/// the staging boundary leaves the frame untouched.
///
/// # Errors
///
/// Propagates the error returned by `checkpoint`.
pub(crate) fn apply_dust_scratches_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: u32,
    height: u32,
    grade: &DustScratchesGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let width_extent = width as usize;
    let height_extent = height as usize;
    let Some(pixel_count) = width_extent.checked_mul(height_extent) else {
        return Ok(false);
    };
    if pixel_count == 0 || width == 0 || height == 0 || pixels.len() < pixel_count {
        return Ok(false);
    }
    let radius = grade.radius_pixels(width, height);
    if grade.is_identity() || radius == 0 {
        return Ok(true);
    }
    let frame = &mut pixels[..pixel_count];
    // The sliding window reads the frame while the output overwrites it, so the
    // original samples are staged first: the pass may not read a sample it has
    // already replaced.
    let mut source = Vec::with_capacity(pixel_count);
    for (index, pixel) in frame.iter().enumerate() {
        if index % CHECKPOINT_STRIDE == 0 {
            checkpoint()?;
        }
        source.push(*pixel);
    }
    checkpoint()?;
    let context = BandContext {
        source: &source,
        width: width_extent,
        height: height_extent,
        radius: radius as usize,
        threshold: grade.threshold(),
        operate_on_alpha: grade.operates_on_alpha(),
        rank: window_rank(radius as usize),
    };
    crate::parallel::for_each_row_band(frame, width_extent, 0..height_extent, |row_start, band| {
        context.fill_band(band, row_start);
    });
    checkpoint()?;
    Ok(true)
}

/// Clamps one centre sample into the gate's comparable range.
///
/// `NaN` compares as zero: `f32::clamp` propagates `NaN`, while WGSL's
/// `select(0.0, clamp(value, 0.0, 1.0), value == value)` yields zero, and the two
/// backends have to agree bit for bit.
#[inline]
fn gate(value: f32) -> f32 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

/// One row-band pass of the sliding window.
///
/// This is the same loop shape as `median::BandContext`, deliberately not shared
/// with it: that context hard-wires "write the median", while this pass writes
/// the median only when the gate fires and the centre sample otherwise.
struct BandContext<'a> {
    source: &'a [[f32; 4]],
    width: usize,
    height: usize,
    radius: usize,
    threshold: f32,
    operate_on_alpha: bool,
    rank: u32,
}

impl BandContext<'_> {
    /// Fills every row of one band from the staged frame.
    fn fill_band(&self, band: &mut [[f32; 4]], row_start: usize) {
        let mut histograms = WindowHistograms::new();
        for (row_offset, row_pixels) in band.chunks_mut(self.width).enumerate() {
            let row = row_start + row_offset;
            histograms.clear();
            // Build the window for the first column of the row, then slide it.
            for offset_y in -(self.radius as i64)..=(self.radius as i64) {
                let source_row = clamp_coord(row as i64 + offset_y, self.height);
                let row_offset_in_source = source_row * self.width;
                for offset_x in -(self.radius as i64)..=(self.radius as i64) {
                    let source_column = clamp_coord(offset_x, self.width);
                    histograms.add(
                        bin_pixel(self.source[row_offset_in_source + source_column]),
                        self.operate_on_alpha,
                    );
                }
            }
            for (column, pixel) in row_pixels.iter_mut().enumerate() {
                let centre = self.source[row * self.width + column];
                let mut medianed = [0.0_f32; 4];
                let mut delta = 0.0_f32;
                for (channel, value) in medianed.iter_mut().take(3).enumerate() {
                    let median = median_value(histograms.select(channel, self.rank));
                    *value = median;
                    delta = delta.max((gate(centre[channel]) - median).abs());
                }
                medianed[3] = if self.operate_on_alpha {
                    let median = median_value(histograms.select(3, self.rank));
                    delta = delta.max((gate(centre[3]) - median).abs());
                    median
                } else {
                    // Bit-for-bit copy of the sample the window was centred on.
                    centre[3]
                };
                // The kept branch copies the centre verbatim: no clamp, no
                // quantisation, no Alpha rewrite.
                *pixel = if delta > self.threshold {
                    medianed
                } else {
                    centre
                };
                if column + 1 < self.width {
                    // The window at `column + 1` is the window at `column` with
                    // the leftmost sample removed and the rightmost added. The
                    // clamped indices keep that true at the frame edges, where
                    // the same physical column is sampled more than once.
                    let remove_column = clamp_coord(column as i64 - self.radius as i64, self.width);
                    let add_column =
                        clamp_coord(column as i64 + self.radius as i64 + 1, self.width);
                    for offset_y in -(self.radius as i64)..=(self.radius as i64) {
                        let source_row = clamp_coord(row as i64 + offset_y, self.height);
                        let row_offset_in_source = source_row * self.width;
                        histograms.remove(
                            bin_pixel(self.source[row_offset_in_source + remove_column]),
                            self.operate_on_alpha,
                        );
                        histograms.add(
                            bin_pixel(self.source[row_offset_in_source + add_column]),
                            self.operate_on_alpha,
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::Hasher as _;

    fn grade(radius: f32, threshold: f32, operate_on_alpha: f32) -> DustScratchesGrade {
        DustScratchesGrade::new(radius, threshold, operate_on_alpha)
            .expect("valid dust & scratches grade")
    }

    /// Deterministic pseudo-random source, so a failure is reproducible.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }

        /// One eight-bit code value, so the sample sits on the quantisation grid.
        fn code(&mut self) -> f32 {
            (self.next() % 256) as f32 / 255.0
        }
    }

    fn assert_bit_identical(actual: &[[f32; 4]], expected: &[[f32; 4]], context: &str) {
        assert_eq!(actual.len(), expected.len(), "{context}: pixel count");
        for (index, (ours, theirs)) in actual.iter().zip(expected.iter()).enumerate() {
            for channel in 0..4 {
                assert_eq!(
                    ours[channel].to_bits(),
                    theirs[channel].to_bits(),
                    "{context}: pixel {index} channel {channel}: {ours:?} vs {theirs:?}"
                );
            }
        }
    }

    /// Sorts the clipped window per channel and applies the same gate.
    fn brute_force_dust(
        pixels: &[[f32; 4]],
        width: usize,
        height: usize,
        radius: usize,
        threshold: f32,
        operate_on_alpha: bool,
    ) -> Vec<[f32; 4]> {
        let rank = window_rank(radius) as usize;
        let mut output = vec![[0.0_f32; 4]; width * height];
        for row in 0..height {
            for column in 0..width {
                let mut taps: [Vec<u32>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
                for offset_y in -(radius as i64)..=(radius as i64) {
                    let source_row = clamp_coord(row as i64 + offset_y, height);
                    for offset_x in -(radius as i64)..=(radius as i64) {
                        let source_column = clamp_coord(column as i64 + offset_x, width);
                        let bins = bin_pixel(pixels[source_row * width + source_column]);
                        for (channel, tap) in taps.iter_mut().enumerate() {
                            tap.push(bins[channel]);
                        }
                    }
                }
                let centre = pixels[row * width + column];
                let mut medianed = [0.0_f32; 4];
                for (channel, tap) in taps.iter_mut().take(3).enumerate() {
                    tap.sort_unstable();
                    medianed[channel] = median_value(tap[rank]);
                }
                medianed[3] = if operate_on_alpha {
                    taps[3].sort_unstable();
                    median_value(taps[3][rank])
                } else {
                    centre[3]
                };
                let rgb_delta = (gate(centre[0]) - medianed[0])
                    .abs()
                    .max((gate(centre[1]) - medianed[1]).abs())
                    .max((gate(centre[2]) - medianed[2]).abs());
                let delta = if operate_on_alpha {
                    rgb_delta.max((gate(centre[3]) - medianed[3]).abs())
                } else {
                    rgb_delta
                };
                output[row * width + column] = if delta > threshold { medianed } else { centre };
            }
        }
        output
    }

    /// T2: threshold zero degenerates to the plain median bit for bit.
    #[test]
    fn threshold_zero_equals_median() {
        let mut random = Lcg(0x0d05_5c27);
        let width = 7_u32;
        let height = 5_u32;
        let mut dust: Vec<[f32; 4]> = (0..width * height)
            .map(|_| [random.code(), random.code(), random.code(), random.code()])
            .collect();
        let mut medianed = dust.clone();
        let dust_grade = grade(2.0, 0.0, 0.0);
        let median_grade = crate::MedianGrade::new(2.0, 0.0).expect("valid median grade");
        assert!(dust_scratches_frame_f32(
            &mut dust,
            width,
            height,
            &dust_grade
        ));
        assert!(crate::median_frame_f32(
            &mut medianed,
            width,
            height,
            &median_grade
        ));
        assert_bit_identical(&dust, &medianed, "threshold zero against median");
    }

    /// T3: one bright speck disappears and every other pixel is untouched.
    #[test]
    fn single_blob_is_removed() {
        let width = 5_u32;
        let height = 5_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        pixels[12] = [1.0, 1.0, 1.0, 1.0];
        let original = pixels.clone();
        assert!(dust_scratches_frame_f32(
            &mut pixels,
            width,
            height,
            &grade(2.0, 0.0, 0.0)
        ));
        assert_eq!(pixels[12], [0.0, 0.0, 0.0, 1.0]);
        for (index, (ours, theirs)) in pixels.iter().zip(original.iter()).enumerate() {
            if index == 12 {
                continue;
            }
            for channel in 0..4 {
                assert_eq!(
                    ours[channel].to_bits(),
                    theirs[channel].to_bits(),
                    "untouched pixel {index} channel {channel}"
                );
            }
        }

        // The mirror fixture: a dark speck on a bright field becomes bright.
        let mut mirrored = vec![[1.0_f32, 1.0, 1.0, 1.0]; (width * height) as usize];
        mirrored[12] = [0.0, 0.0, 0.0, 1.0];
        assert!(dust_scratches_frame_f32(
            &mut mirrored,
            width,
            height,
            &grade(2.0, 0.0, 0.0)
        ));
        assert_eq!(mirrored[12], [1.0, 1.0, 1.0, 1.0]);
    }

    /// T4: the gate is `>`, not `>=`, and compares exact values.
    #[test]
    fn threshold_is_strict_and_exact() {
        let width = 9_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; width as usize];
        pixels[4] = [1.0, 1.0, 1.0, 1.0];

        // delta is exactly 1.0 here, so a threshold of 1.0 keeps the centre.
        let mut at_one = pixels.clone();
        assert!(dust_scratches_frame_f32(
            &mut at_one,
            width,
            1,
            &grade(1.0, 1.0, 0.0)
        ));
        assert_eq!(at_one[4][0].to_bits(), 1.0_f32.to_bits());

        // The largest float below one is strictly smaller than delta.
        let just_below_one = f32::from_bits(1.0_f32.to_bits() - 1);
        let mut below_one = pixels.clone();
        assert!(dust_scratches_frame_f32(
            &mut below_one,
            width,
            1,
            &grade(1.0, just_below_one, 0.0)
        ));
        assert_eq!(below_one[4], [0.0, 0.0, 0.0, 1.0]);

        // delta is exactly 0.5 here, so the gate runs at threshold 0.5 and the
        // strict comparison is observable without the identity short circuit.
        let mut half = vec![[0.0_f32, 0.0, 0.0, 1.0]; 5];
        half[2] = [0.5, 0.5, 0.5, 1.0];
        let mut kept = half.clone();
        assert!(dust_scratches_frame_f32(
            &mut kept,
            5,
            1,
            &grade(1.0, 0.5, 0.0)
        ));
        assert_eq!(kept[2], [0.5, 0.5, 0.5, 1.0]);
        let just_below_half = f32::from_bits(0.5_f32.to_bits() - 1);
        let mut repaired = half;
        assert!(dust_scratches_frame_f32(
            &mut repaired,
            5,
            1,
            &grade(1.0, just_below_half, 0.0)
        ));
        assert_eq!(repaired[2], [0.0, 0.0, 0.0, 1.0]);
    }

    /// T5: with the alpha toggle off the centre alpha survives bit for bit.
    #[test]
    fn alpha_off_preserves_alpha_bits() {
        for threshold in [0.0_f32, 0.5, 0.9] {
            let width = 5_u32;
            let height = 5_u32;
            let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
            pixels[12][3] = 0.25;
            assert!(dust_scratches_frame_f32(
                &mut pixels,
                width,
                height,
                &grade(1.0, threshold, 0.0)
            ));
            assert_eq!(pixels[12][3].to_bits(), 0.25_f32.to_bits());
        }
    }

    /// T6: with the alpha toggle on the alpha difference can trigger the repair.
    #[test]
    fn alpha_on_participates() {
        let build = || {
            let width = 5_u32;
            let height = 5_u32;
            let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
            pixels[12][3] = 0.25;
            pixels
        };

        // delta = |0.25 - 1.0| = 0.75, above 0.3: the repair takes the median
        // alpha, which is 1.0.
        let mut repaired = build();
        assert!(dust_scratches_frame_f32(
            &mut repaired,
            5,
            5,
            &grade(1.0, 0.3, 1.0)
        ));
        assert_eq!(repaired[12], [0.0, 0.0, 0.0, 1.0]);

        // The same delta is not above 0.8: the centre alpha is kept.
        let mut kept = build();
        assert!(dust_scratches_frame_f32(
            &mut kept,
            5,
            5,
            &grade(1.0, 0.8, 1.0)
        ));
        assert_eq!(kept[12][3].to_bits(), 0.25_f32.to_bits());
    }

    /// T7: out-of-range samples survive the kept branch verbatim.
    #[test]
    fn out_of_range_is_kept_verbatim() {
        let mut pixels = vec![[1.5_f32, 1.5, 1.5, 1.0]; 9];
        assert!(dust_scratches_frame_f32(
            &mut pixels,
            3,
            3,
            &grade(1.0, 0.0, 0.0)
        ));
        for pixel in &pixels {
            assert_eq!(pixel[0].to_bits(), 1.5_f32.to_bits());
            assert_eq!(pixel[1].to_bits(), 1.5_f32.to_bits());
            assert_eq!(pixel[2].to_bits(), 1.5_f32.to_bits());
        }

        // A NaN centre must not panic and must stay deterministic and finite.
        let mut nan_frame = vec![[1.5_f32, 1.5, 1.5, 1.0]; 9];
        nan_frame[4] = [f32::NAN, f32::NAN, f32::NAN, 1.0];
        let mut first = nan_frame.clone();
        let mut second = nan_frame;
        assert!(dust_scratches_frame_f32(
            &mut first,
            3,
            3,
            &grade(1.0, 0.0, 0.0)
        ));
        assert!(dust_scratches_frame_f32(
            &mut second,
            3,
            3,
            &grade(1.0, 0.0, 0.0)
        ));
        assert_bit_identical(&first, &second, "deterministic NaN handling");
        for pixel in &first {
            for channel in pixel {
                assert!(channel.is_finite(), "non-finite output sample {pixel:?}");
            }
        }
    }

    /// T8: the sliding window matches a brute-force sort of the same window.
    #[test]
    fn matches_brute_force() {
        let mut random = Lcg(0x5ca7_7c15);
        let width = 13_u32;
        let height = 9_u32;
        let mut pixels: Vec<[f32; 4]> = (0..width * height)
            .map(|_| [random.code(), random.code(), random.code(), random.code()])
            .collect();
        let expected = brute_force_dust(&pixels, width as usize, height as usize, 3, 0.05, true);
        assert!(dust_scratches_frame_f32(
            &mut pixels,
            width,
            height,
            &grade(3.0, 0.05, 1.0)
        ));
        assert_bit_identical(&pixels, &expected, "brute force radius 3");
    }

    /// T9: an oversized authored radius clamps to the frame extent.
    #[test]
    fn radius_clamps_to_frame() {
        let mut random = Lcg(0x9c11_7a3d);
        let width = 3_u32;
        let height = 3_u32;
        let pixels: Vec<[f32; 4]> = (0..width * height)
            .map(|_| [random.code(), random.code(), random.code(), random.code()])
            .collect();
        let huge = grade(255.0, 0.0, 0.0);
        assert_eq!(huge.radius_pixels(3, 3), 3);
        let expected = brute_force_dust(&pixels, width as usize, height as usize, 3, 0.0, false);
        let mut oversized = pixels;
        assert!(dust_scratches_frame_f32(
            &mut oversized,
            width,
            height,
            &huge
        ));
        assert_bit_identical(&oversized, &expected, "radius clamped to frame");
        assert_eq!(huge.radius_pixels(0, 0), 0);
        assert!(grade(0.0, 0.0, 0.0).is_identity());
        assert!(!huge.is_identity());
    }

    /// T10: both identity arms short-circuit the pass.
    #[test]
    fn identity_short_circuits() {
        let mut pixels = vec![
            [0.0_f32, 0.0, 0.0, 1.0],
            [0.25, 0.5, 0.75, 0.5],
            [f32::NAN, -0.25, 2.5, 0.25],
            [-0.0, 1.0, 0.0, 1.0],
        ];
        let original = pixels.clone();

        // 0.4 rounds to zero but is below the documented 0.5 boundary; the kernel
        // must not rely on `radius_pixels == 0` alone, because 0.6 rounds to one.
        let tiny = grade(0.4, 0.0, 0.0);
        assert!(tiny.is_identity());
        assert!(dust_scratches_frame_f32(&mut pixels, 2, 2, &tiny));
        assert_bit_identical(&pixels, &original, "radius identity");
        assert!(!grade(0.6, 0.0, 0.0).is_identity());
        assert_eq!(grade(0.6, 0.0, 0.0).radius_pixels(9, 9), 1);

        let at_one = grade(3.0, 1.0, 0.0);
        assert!(at_one.is_identity());
        let mut threshold_identity = original.clone();
        assert!(dust_scratches_frame_f32(
            &mut threshold_identity,
            2,
            2,
            &at_one
        ));
        assert_bit_identical(&threshold_identity, &original, "threshold identity");

        // Invalid authored state never reaches a frame, and each rejection
        // carries the message the graph builder reports as the author reason.
        assert_eq!(
            DustScratchesGrade::new(f32::NAN, 0.0, 0.0),
            Err(DustScratchesError::NonFiniteRadius)
        );
        assert_eq!(
            DustScratchesGrade::new(f32::INFINITY, 0.0, 0.0),
            Err(DustScratchesError::NonFiniteRadius)
        );
        assert_eq!(
            DustScratchesGrade::new(-1.0, 0.0, 0.0),
            Err(DustScratchesError::RadiusOutOfRange)
        );
        assert_eq!(
            DustScratchesGrade::new(255.5, 0.0, 0.0),
            Err(DustScratchesError::RadiusOutOfRange)
        );
        assert_eq!(
            DustScratchesGrade::new(1.0, f32::NAN, 0.0),
            Err(DustScratchesError::NonFiniteThreshold)
        );
        assert_eq!(
            DustScratchesGrade::new(1.0, 1.1, 0.0),
            Err(DustScratchesError::ThresholdOutOfRange)
        );
        assert_eq!(
            DustScratchesGrade::new(1.0, -0.1, 0.0),
            Err(DustScratchesError::ThresholdOutOfRange)
        );
        assert_eq!(
            DustScratchesGrade::new(1.0, 0.0, f32::NAN),
            Err(DustScratchesError::NonFiniteOperateOnAlpha)
        );
        assert_eq!(
            DustScratchesGrade::new(1.0, 0.0, 1.5),
            Err(DustScratchesError::OperateOnAlphaOutOfRange)
        );
        assert_eq!(
            DustScratchesGrade::new(1.0, 0.0, -0.1),
            Err(DustScratchesError::OperateOnAlphaOutOfRange)
        );
        assert_eq!(
            DustScratchesError::NonFiniteRadius.to_string(),
            "dust & scratches radius must be a finite number"
        );
        assert_eq!(
            DustScratchesError::RadiusOutOfRange.to_string(),
            "dust & scratches radius must be inside [0, 255] pixels"
        );
        assert_eq!(
            DustScratchesError::NonFiniteThreshold.to_string(),
            "dust & scratches threshold must be a finite number"
        );
        assert_eq!(
            DustScratchesError::ThresholdOutOfRange.to_string(),
            "dust & scratches threshold must be inside [0, 1]"
        );
        assert_eq!(
            DustScratchesError::NonFiniteOperateOnAlpha.to_string(),
            "dust & scratches operate on alpha must be a finite number"
        );
        assert_eq!(
            DustScratchesError::OperateOnAlphaOutOfRange.to_string(),
            "dust & scratches operate on alpha must be inside [0, 1]"
        );
    }

    /// T11: a checkpoint error leaves the frame bit for bit unchanged.
    #[test]
    fn staging_leaves_frame_untouched_on_error() {
        let width = 4096_u32;
        let height = 2_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for (index, pixel) in pixels.iter_mut().enumerate() {
            pixel[0] = (index % 251) as f32 / 255.0;
        }
        let original = pixels.clone();
        let mut checkpoints = 0_usize;
        let mut checkpoint = || {
            checkpoints += 1;
            if checkpoints == 2 {
                Err("injected")
            } else {
                Ok(())
            }
        };
        let result = apply_dust_scratches_f32_controlled(
            &mut pixels,
            width,
            height,
            &grade(1.0, 0.0, 0.0),
            &mut checkpoint,
        );
        assert_eq!(result, Err("injected"));
        assert!(pixels.len() > 4096);
        assert_bit_identical(&pixels, &original, "aborted staging");
    }

    /// T12: there are no lazy parameters — the alpha toggle is part of the grade.
    #[test]
    fn lazy_none_keeps_operate_on_alpha_in_the_grade() {
        fn hash_of(grade: DustScratchesGrade) -> u64 {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            grade.hash_state(&mut hasher);
            hasher.finish()
        }

        let off = grade(2.0, 0.25, 0.0);
        let on = grade(2.0, 0.25, 1.0);
        assert_ne!(off, on);
        assert_ne!(off.gpu_controls(), on.gpu_controls());
        assert_ne!(hash_of(off), hash_of(on));
        // Radius and threshold also reach the hash.
        assert_ne!(
            hash_of(grade(2.0, 0.25, 0.0)),
            hash_of(grade(3.0, 0.25, 0.0))
        );
        assert_ne!(
            hash_of(grade(2.0, 0.25, 0.0)),
            hash_of(grade(2.0, 0.5, 0.0))
        );
        assert_eq!(off.gpu_controls().0[2], 0.0);
        assert_eq!(on.gpu_controls().0[2], 1.0);
    }
}
