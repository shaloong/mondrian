//! Median kernel for the Median built-in effect.
//!
//! The effect replaces every pixel with the per-channel median of the
//! `(2r + 1)²` window centred on it. A median is a rank filter: every output
//! sample is one of the input samples, so isolated outliers disappear while an
//! edge that covers more than half of the window survives with its full
//! contrast. That is the property a blur cannot offer, because averaging turns
//! one bad sample into a halo instead of deleting it.
//!
//! # Quantised domain
//!
//! An exact 32-bit median needs the window samples counted per distinct value.
//! Counting them in the authored `f32` domain would need a 2³²-entry histogram,
//! and approximating the rank with a running sum of float values is not a median
//! at all. The kernel therefore quantises every sample to the eight-bit code
//! value it would occupy:
//!
//! ```text
//! bin   = round(clamp01(value) * 255)
//! value = f32(bin) / 255.0
//! ```
//!
//! A 256-bin histogram is four KiB per channel and fits the cache, the rank
//! selection is an integer walk, and the output frame is bit-for-bit
//! reproducible because every tap maps to one small integer. Values that already
//! sit on the eight-bit grid (`k / 255`) round-trip exactly, so a project built
//! from eight-bit sources sees no quantisation at all. Values between grid points
//! move to the nearest one, at most `1/510` away.
//!
//! # Sliding window
//!
//! Rebuilding the histogram for every pixel costs `O(r²)` per pixel. The kernel
//! instead builds the window once per row and then slides it: moving one column
//! right removes the column that left the window and adds the column that
//! entered it, `2r + 1` taps each. Selection uses two levels, a sixteen-bucket
//! coarse histogram over the high nibble and the fine 256-bin histogram, so the
//! rank walk touches at most 32 counters instead of 256.
//!
//! # Clamp-only edges
//!
//! Out-of-frame taps repeat the boundary pixel. There is no authored edge
//! behaviour: the effect is exact, and treating out-of-frame samples as anything
//! but the nearest real sample would change the rank of the window. The window
//! therefore always holds `(2r + 1)²` samples, which is what makes the rank
//! independent of where the pixel sits in the frame.
//!
//! # Straight alpha, no premultiplication
//!
//! RGB is always medianed on the stored straight-alpha values. When
//! `operate_on_alpha` is off the alpha channel is copied from the centre pixel
//! bit for bit, so a matte survives the filter untouched while the colour
//! channels are cleaned. When the toggle is on, alpha joins the rank filter with
//! the same window and the same rank.
//!
//! Premultiplying is not an option here: a median selects one sample, and
//! weighting the samples by coverage before selecting reorders them, so the
//! selected sample would no longer be the median of the stored frame. The
//! consequence is the documented one: a colour in a semi-transparent region can
//! disagree with its coverage, exactly as the eight-bit reference implementation
//! behaves.
//!
//! # GPU packing
//!
//! [`MedianGrade::gpu_controls`] returns the pair the renderer uploads:
//! `params = [radius, operate_on_alpha, 0, 0]` and `extra = [0; 4]`. The
//! fragment shader rounds `params.x` and reads `params.y >= 0.5` as the toggle.
//! The GPU pass lowers radii up to [`MEDIAN_GPU_MAX_RADIUS`]; larger windows stay
//! on the CPU with an identical picture. The bound lives in the renderer's
//! admission step, not in this module, so a chain is rejected before it records
//! a single pass.
//!
//! # Performance
//!
//! Cost per pixel is `O(r)` taps plus `O(1)` selection: the initial window per
//! row is `(2r + 1)²` taps, each of the remaining columns is `2(2r + 1)` taps.
//! Row bands run in parallel through [`crate::parallel::for_each_row_band`]; each
//! band owns its histograms, so the result does not depend on the thread count.
//!
//! # Documented deviations
//!
//! 1. Samples are quantised to eight bits before the rank selection. See above.
//! 2. Values above one clamp to the top bin, so HDR highlights are medianed as
//!    `1.0`. Values below zero and `NaN` clamp to the bottom bin.
//! 3. The alpha toggle is stored as a float in `[0, 1]` and read as `>= 0.5`
//!    rather than as a boolean property.
//! 4. Out-of-frame taps repeat the boundary pixel; there is no other mode.
//! 5. An even-sized window would select the lower of the two middle samples;
//!    every window here has an odd sample count, so the case is unreachable.
//! 6. The GPU pass lowers radii up to three pixels. Larger radii fall back to
//!    the CPU kernel, which produces the same frame.
//!
//! # Tests
//!
//! The kernel tests cross-check the sliding window against a brute-force sort of
//! the same window on eight-bit data, pin the eight-bit grid round trip, cover
//! the clamp-only edges, both alpha modes, the documented clamping of HDR,
//! negative and `NaN` samples, unaddressable frames, the checkpoint cadence and
//! the two denoising properties (a flat field stays flat, a monotone gradient
//! stays monotone).

/// Largest authored radius in pixels.
pub const MEDIAN_MAX_RADIUS: u32 = 255;
/// Authored radius that leaves the frame untouched.
pub const MEDIAN_DEFAULT_RADIUS: f32 = 0.0;
/// Default alpha handling: copy the centre alpha.
pub const MEDIAN_DEFAULT_OPERATE_ON_ALPHA: f32 = 0.0;
/// Number of histogram bins: one per eight-bit code value.
pub const MEDIAN_BIN_COUNT: usize = 256;
/// Number of coarse buckets used by the two-level rank selection.
pub const MEDIAN_COARSE_BINS: usize = 16;
/// Largest radius the GPU pass lowers; larger radii stay on the CPU.
pub const MEDIAN_GPU_MAX_RADIUS: u32 = 3;

/// Fine bins covered by one coarse bucket.
const MEDIAN_COARSE_WIDTH: usize = MEDIAN_BIN_COUNT / MEDIAN_COARSE_BINS;
/// Pixels between two sequential checkpoint calls while staging.
const CHECKPOINT_STRIDE: usize = 4096;

/// Rejected authored median state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MedianError {
    /// The radius is not a finite number.
    NonFiniteRadius,
    /// The radius is outside the authored range.
    RadiusOutOfRange,
    /// The alpha toggle is not a finite number.
    NonFiniteOperateOnAlpha,
    /// The alpha toggle is outside the authored range.
    OperateOnAlphaOutOfRange,
}

impl std::fmt::Display for MedianError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::NonFiniteRadius => "median radius must be a finite number",
            Self::RadiusOutOfRange => "median radius must be inside [0, 255] pixels",
            Self::NonFiniteOperateOnAlpha => "median operate on alpha must be a finite number",
            Self::OperateOnAlphaOutOfRange => "median operate on alpha must be inside [0, 1]",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for MedianError {}

/// The authored parameters of one Median instance.
///
/// The radius stays authored (`f32`) so the hash sees exactly what the project
/// stored; the pixel radius is derived per frame by [`Self::radius_pixels`],
/// which clamps it to the frame extent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MedianGrade {
    radius: f32,
    operate_on_alpha: f32,
}

impl MedianGrade {
    /// Validates one instance against the documented parameter ranges.
    ///
    /// # Errors
    ///
    /// Returns [`MedianError`] when the radius is not finite or rounds outside
    /// `[0, 255]`, and when the alpha toggle is not finite or lies outside
    /// `[0, 1]`.
    pub fn new(radius: f32, operate_on_alpha: f32) -> Result<Self, MedianError> {
        if !radius.is_finite() {
            return Err(MedianError::NonFiniteRadius);
        }
        let rounded = radius.round();
        if !(0.0..=MEDIAN_MAX_RADIUS as f32).contains(&rounded) {
            return Err(MedianError::RadiusOutOfRange);
        }
        if !operate_on_alpha.is_finite() {
            return Err(MedianError::NonFiniteOperateOnAlpha);
        }
        if !(0.0..=1.0).contains(&operate_on_alpha) {
            return Err(MedianError::OperateOnAlphaOutOfRange);
        }
        Ok(Self { radius, operate_on_alpha })
    }

    /// The documented default: the identity.
    #[must_use]
    pub const fn documented_defaults() -> Self {
        Self {
            radius: MEDIAN_DEFAULT_RADIUS,
            operate_on_alpha: MEDIAN_DEFAULT_OPERATE_ON_ALPHA,
        }
    }

    /// The authored radius, in pixels.
    #[must_use]
    pub const fn radius(self) -> f32 {
        self.radius
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

    /// Whether the alpha channel joins the rank filter.
    ///
    /// `operate_on_alpha >= 0.5` medianes alpha with the colour channels;
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
        self.radius < 0.5
    }

    /// Whether the GPU pass can lower this instance.
    ///
    /// Admission happens before any frame extent is known, so the authored
    /// radius is rounded and compared against [`MEDIAN_GPU_MAX_RADIUS`] rather
    /// than the frame-clamped pixel radius.
    #[must_use]
    pub fn gpu_supported(self) -> bool {
        let rounded = self.radius.round();
        let radius = if rounded > 0.0 { rounded as u32 } else { 0 };
        radius <= MEDIAN_GPU_MAX_RADIUS
    }

    /// The `(params, extra)` pair the renderer uploads.
    ///
    /// `params = [radius, operate_on_alpha, 0, 0]` carries the authored values;
    /// the fragment shader rounds the radius and reads the toggle as
    /// `>= 0.5`. `extra` is unused and stays zero.
    #[must_use]
    pub fn gpu_controls(self) -> ([f32; 4], [f32; 4]) {
        ([self.radius, self.operate_on_alpha, 0.0, 0.0], [0.0; 4])
    }

    /// Feeds every authored value into a semantic hash.
    pub(crate) fn hash_state<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash as _;
        self.radius.to_bits().hash(state);
        self.operate_on_alpha.to_bits().hash(state);
    }
}

/// Applies one authored Median instance to a straight-alpha frame.
///
/// Returns `false` for a frame whose geometry cannot be addressed by `pixels`
/// (`width * height` must fit, be non-zero, and be covered by the slice).
#[must_use]
pub fn median_frame_f32(
    pixels: &mut [[f32; 4]],
    width: u32,
    height: u32,
    grade: &MedianGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_median_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies one Median instance while polling `checkpoint`.
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
pub(crate) fn apply_median_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: u32,
    height: u32,
    grade: &MedianGrade,
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
        operate_on_alpha: grade.operates_on_alpha(),
        rank: window_rank(radius as usize),
    };
    crate::parallel::for_each_row_band(frame, width_extent, 0..height_extent, |row_start, band| {
        context.fill_band(band, row_start);
    });
    checkpoint()?;
    Ok(true)
}

/// Quantises one straight-alpha sample to its eight-bit histogram bin.
///
/// `NaN` has no place in an ordered histogram, so it takes the bottom bin, the
/// same bin as every value at or below zero. Values above one clamp to the top
/// bin.
#[inline]
pub(crate) fn median_bin(value: f32) -> u32 {
    if value.is_nan() {
        return 0;
    }
    let clamped = value.clamp(0.0, 1.0);
    (clamped * 255.0).round() as u32
}

/// Expands one eight-bit histogram bin back to a straight-alpha sample.
#[inline]
pub(crate) fn median_value(bin: u32) -> f32 {
    bin as f32 / 255.0
}

/// The zero-based rank of the lower median for a window of `taps` samples.
pub(crate) fn window_rank(radius: usize) -> u32 {
    let taps = 2 * radius + 1;
    let count = u32::try_from(taps * taps).unwrap_or(u32::MAX);
    count.div_ceil(2) - 1
}

/// Clamps one window offset to a valid frame index.
#[inline]
pub(crate) fn clamp_coord(value: i64, extent: usize) -> usize {
    value.clamp(0, extent as i64 - 1) as usize
}

/// Quantises the four channels of one sample once per tap.
#[inline]
pub(crate) fn bin_pixel(pixel: [f32; 4]) -> [u32; 4] {
    [
        median_bin(pixel[0]),
        median_bin(pixel[1]),
        median_bin(pixel[2]),
        median_bin(pixel[3]),
    ]
}

/// Per-band fine and coarse histograms for the four channels.
///
/// The arrays are band-private, so two bands never write the same counter and
/// the result is independent of how the frame is split across threads.
pub(crate) struct WindowHistograms {
    fine: [[u32; MEDIAN_BIN_COUNT]; 4],
    coarse: [[u32; MEDIAN_COARSE_BINS]; 4],
}

impl WindowHistograms {
    /// Builds an empty histogram set.
    pub(crate) fn new() -> Self {
        Self {
            fine: [[0; MEDIAN_BIN_COUNT]; 4],
            coarse: [[0; MEDIAN_COARSE_BINS]; 4],
        }
    }

    /// Empties every counter before a new row window is built.
    pub(crate) fn clear(&mut self) {
        self.fine = [[0; MEDIAN_BIN_COUNT]; 4];
        self.coarse = [[0; MEDIAN_COARSE_BINS]; 4];
    }

    /// Counts one window sample.
    pub(crate) fn add(&mut self, bins: [u32; 4], operate_on_alpha: bool) {
        let channels = if operate_on_alpha { 4 } else { 3 };
        for ((bin, fine), coarse) in
            bins.iter().zip(self.fine.iter_mut()).zip(self.coarse.iter_mut()).take(channels)
        {
            let bin = *bin as usize;
            fine[bin] += 1;
            coarse[bin / MEDIAN_COARSE_WIDTH] += 1;
        }
    }

    /// Uncounts one sample that left the window.
    pub(crate) fn remove(&mut self, bins: [u32; 4], operate_on_alpha: bool) {
        let channels = if operate_on_alpha { 4 } else { 3 };
        for ((bin, fine), coarse) in
            bins.iter().zip(self.fine.iter_mut()).zip(self.coarse.iter_mut()).take(channels)
        {
            let bin = *bin as usize;
            fine[bin] -= 1;
            coarse[bin / MEDIAN_COARSE_WIDTH] -= 1;
        }
    }

    /// Returns the bin that holds the sample at `rank`.
    ///
    /// `rank` is zero-based and always below the window count, so the walk finds
    /// a bucket; the trailing value only keeps the return type total.
    pub(crate) fn select(&self, channel: usize, rank: u32) -> u32 {
        let mut remaining = rank;
        let mut coarse_index = 0;
        while coarse_index + 1 < MEDIAN_COARSE_BINS
            && remaining >= self.coarse[channel][coarse_index]
        {
            remaining -= self.coarse[channel][coarse_index];
            coarse_index += 1;
        }
        let start = coarse_index * MEDIAN_COARSE_WIDTH;
        for bin in start..start + MEDIAN_COARSE_WIDTH {
            let count = self.fine[channel][bin];
            if remaining < count {
                return bin as u32;
            }
            remaining -= count;
        }
        (MEDIAN_BIN_COUNT - 1) as u32
    }
}

/// One row-band pass of the sliding window.
struct BandContext<'a> {
    source: &'a [[f32; 4]],
    width: usize,
    height: usize,
    radius: usize,
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
                let mut output = [0.0_f32; 4];
                for (channel, value) in output.iter_mut().take(3).enumerate() {
                    *value = median_value(histograms.select(channel, self.rank));
                }
                output[3] = if self.operate_on_alpha {
                    median_value(histograms.select(3, self.rank))
                } else {
                    // Bit-for-bit copy of the sample the window was centred on.
                    self.source[row * self.width + column][3]
                };
                *pixel = output;
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

    fn grade(radius: f32, operate_on_alpha: f32) -> MedianGrade {
        MedianGrade::new(radius, operate_on_alpha).expect("valid median grade")
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

    /// Sorts the clipped window per channel and picks the same rank.
    fn brute_force(
        pixels: &[[f32; 4]],
        width: usize,
        height: usize,
        radius: usize,
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
                let mut pixel = [0.0_f32; 4];
                for (channel, tap) in taps.iter_mut().take(3).enumerate() {
                    tap.sort_unstable();
                    pixel[channel] = median_value(tap[rank]);
                }
                pixel[3] = if operate_on_alpha {
                    taps[3].sort_unstable();
                    median_value(taps[3][rank])
                } else {
                    pixels[row * width + column][3]
                };
                output[row * width + column] = pixel;
            }
        }
        output
    }

    #[test]
    fn defaults_are_the_documented_identity() {
        let grade = MedianGrade::documented_defaults();
        assert!(grade.is_identity());
        assert_eq!(grade.radius(), MEDIAN_DEFAULT_RADIUS);
        assert!(!grade.operates_on_alpha());
        assert_eq!(grade.radius_pixels(1920, 1080), 0);
        assert_eq!(grade.gpu_controls(), ([0.0; 4], [0.0; 4]));
        assert!(grade.gpu_supported());
    }

    #[test]
    fn invalid_authored_state_is_rejected() {
        assert_eq!(
            MedianGrade::new(f32::NAN, 0.0),
            Err(MedianError::NonFiniteRadius)
        );
        assert_eq!(
            MedianGrade::new(f32::INFINITY, 0.0),
            Err(MedianError::NonFiniteRadius)
        );
        assert_eq!(
            MedianGrade::new(-1.0, 0.0),
            Err(MedianError::RadiusOutOfRange)
        );
        assert_eq!(
            MedianGrade::new(255.5, 0.0),
            Err(MedianError::RadiusOutOfRange)
        );
        assert_eq!(
            MedianGrade::new(0.0, f32::NAN),
            Err(MedianError::NonFiniteOperateOnAlpha)
        );
        assert_eq!(
            MedianGrade::new(0.0, 1.5),
            Err(MedianError::OperateOnAlphaOutOfRange)
        );
        assert_eq!(
            MedianGrade::new(0.0, -0.1),
            Err(MedianError::OperateOnAlphaOutOfRange)
        );
        assert_eq!(
            MedianError::NonFiniteRadius.to_string(),
            "median radius must be a finite number"
        );
        assert_eq!(
            MedianError::RadiusOutOfRange.to_string(),
            "median radius must be inside [0, 255] pixels"
        );
        assert_eq!(
            MedianError::NonFiniteOperateOnAlpha.to_string(),
            "median operate on alpha must be a finite number"
        );
        assert_eq!(
            MedianError::OperateOnAlphaOutOfRange.to_string(),
            "median operate on alpha must be inside [0, 1]"
        );
    }

    #[test]
    fn zero_radius_leaves_the_frame_bit_identical() {
        let mut pixels = vec![
            [0.0_f32, 0.0, 0.0, 1.0],
            [0.25, 0.5, 0.75, 0.0],
            [f32::NAN, -0.25, 2.5, 0.5],
            [-0.0, 1.0, 0.0, 0.25],
        ];
        let original = pixels.clone();
        let grade = MedianGrade::documented_defaults();
        assert!(grade.is_identity());
        assert!(median_frame_f32(&mut pixels, 2, 2, &grade));
        assert_bit_identical(&pixels, &original, "identity");
    }

    #[test]
    fn radius_larger_than_the_frame_clamps() {
        let mut random = Lcg(0x5eed);
        let width = 8_u32;
        let height = 6_u32;
        let mut oversized: Vec<[f32; 4]> = (0..width * height)
            .map(|_| [random.code(), random.code(), random.code(), 1.0])
            .collect();
        let clamped = oversized.clone();
        let mut explicit = oversized.clone();
        let huge = grade(255.0, 0.0);
        assert_eq!(huge.radius_pixels(width, height), 8);
        assert_eq!(grade(8.0, 0.0).radius_pixels(width, height), 8);
        assert_eq!(huge.radius_pixels(0, 0), 0);
        assert!(median_frame_f32(&mut oversized, width, height, &huge));
        assert!(median_frame_f32(
            &mut explicit,
            width,
            height,
            &grade(8.0, 0.0)
        ));
        assert_bit_identical(&oversized, &explicit, "oversized radius");
        assert_ne!(oversized, clamped);
    }

    #[test]
    fn matches_a_brute_force_sort_on_random_eight_by_eight_data() {
        for operate_on_alpha in [false, true] {
            for radius in [1_u32, 2, 3] {
                let mut random = Lcg(0x1234_5678 ^ u64::from(radius));
                let width = 8_u32;
                let height = 8_u32;
                let mut pixels: Vec<[f32; 4]> = (0..width * height)
                    .map(|_| [random.code(), random.code(), random.code(), random.code()])
                    .collect();
                let expected = brute_force(
                    &pixels,
                    width as usize,
                    height as usize,
                    radius as usize,
                    operate_on_alpha,
                );
                let grade = grade(radius as f32, if operate_on_alpha { 1.0 } else { 0.0 });
                assert!(median_frame_f32(&mut pixels, width, height, &grade));
                assert_bit_identical(
                    &pixels,
                    &expected,
                    &format!("radius {radius} alpha {operate_on_alpha}"),
                );
            }
        }
    }

    #[test]
    fn eight_bit_grid_values_round_trip_exactly() {
        // Every code value survives `bin -> value -> bin` untouched, and a flat
        // frame of that code value comes out of the filter bit-identical.
        for bin in 0..MEDIAN_BIN_COUNT as u32 {
            let value = median_value(bin);
            assert_eq!(median_bin(value), bin, "bin {bin}");
            let mut pixels = vec![[value, value, value, 1.0]; 9];
            let grade = grade(1.0, 0.0);
            assert!(median_frame_f32(&mut pixels, 3, 3, &grade));
            for pixel in &pixels {
                assert_eq!(pixel[0].to_bits(), value.to_bits(), "bin {bin}");
            }
        }
    }

    #[test]
    fn a_thin_line_survives_only_when_it_covers_more_than_half_the_window() {
        let width = 6_u32;
        let height = 6_u32;
        let mut thin = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for column in 0..width as usize {
            thin[2 * width as usize + column] = [1.0, 1.0, 1.0, 1.0];
        }
        let grade = grade(1.0, 0.0);
        assert!(median_frame_f32(&mut thin, width, height, &grade));
        // Three of nine samples cover the one-pixel line, so the median removes it.
        for column in 0..width as usize {
            assert_eq!(
                thin[2 * width as usize + column][0],
                0.0,
                "thin line column {column}"
            );
        }

        let mut thick = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for row in [2_usize, 3] {
            for column in 0..width as usize {
                thick[row * width as usize + column] = [1.0, 1.0, 1.0, 1.0];
            }
        }
        assert!(median_frame_f32(&mut thick, width, height, &grade));
        // Six of nine samples cover the two-pixel line, so the median keeps it.
        for row in [2_usize, 3] {
            for column in 0..width as usize {
                assert_eq!(
                    thick[row * width as usize + column][0],
                    1.0,
                    "thick line row {row} column {column}"
                );
            }
        }
    }

    #[test]
    fn an_isolated_alpha_hole_is_filled() {
        let width = 5_u32;
        let height = 5_u32;
        let mut pixels = vec![[0.25_f32, 0.5, 0.75, 1.0]; (width * height) as usize];
        pixels[2 * width as usize + 2] = [0.25, 0.5, 0.75, 0.0];
        let grade = grade(1.0, 1.0);
        assert!(grade.operates_on_alpha());
        assert!(median_frame_f32(&mut pixels, width, height, &grade));
        for (index, pixel) in pixels.iter().enumerate() {
            assert_eq!(pixel[3], 1.0, "pixel {index}");
        }
        assert_eq!(
            pixels[2 * width as usize + 2][0],
            median_value(median_bin(0.25))
        );
    }

    #[test]
    fn opaque_windows_stay_opaque_when_alpha_is_medianed() {
        let width = 6_u32;
        let height = 6_u32;
        let mut pixels = vec![[0.5_f32, 0.5, 0.5, 1.0]; (width * height) as usize];
        pixels[0] = [0.5, 0.5, 0.5, 0.0];
        pixels[(width * height) as usize - 1] = [0.5, 0.5, 0.5, 0.0];
        let grade = grade(1.0, 1.0);
        assert!(median_frame_f32(&mut pixels, width, height, &grade));
        // No window can hold both holes, so every window keeps a majority of
        // opaque samples and the median stays fully opaque.
        for (index, pixel) in pixels.iter().enumerate() {
            assert_eq!(pixel[3], 1.0, "pixel {index}");
        }
    }

    #[test]
    fn alpha_is_copied_bit_exactly_when_the_toggle_is_off() {
        let width = 4_u32;
        let height = 3_u32;
        let mut pixels = vec![
            [0.0_f32, 0.0, 0.0, f32::NAN],
            [1.0, 1.0, 1.0, -0.0],
            [0.25, 0.5, 0.75, 0.3],
            [0.5, 0.5, 0.5, 1.0],
            [0.125, 0.25, 0.5, 0.0],
            [0.75, 0.25, 0.0, 0.5],
            [0.0, 1.0, 0.0, 0.25],
            [0.5, 0.0, 0.5, 1.0],
            [0.25, 0.25, 0.25, 0.75],
            [1.0, 0.0, 1.0, 0.1],
            [0.5, 0.5, 0.5, 0.9],
            [0.25, 0.75, 0.5, 1.0],
        ];
        let alpha: Vec<u32> = pixels.iter().map(|pixel| pixel[3].to_bits()).collect();
        let grade = grade(2.0, 0.0);
        assert!(!grade.operates_on_alpha());
        assert!(median_frame_f32(&mut pixels, width, height, &grade));
        for (index, pixel) in pixels.iter().enumerate() {
            assert_eq!(pixel[3].to_bits(), alpha[index], "pixel {index}");
        }
        assert!(pixels
            .iter()
            .any(|pixel| pixel[3].is_nan() || pixel[3].to_bits() == (-0.0_f32).to_bits()));
    }

    #[test]
    fn hdr_values_above_one_clamp_to_the_top_bin() {
        let mut pixels = vec![[2.5_f32, 4.0, 1.5, 1.0]; 25];
        let grade = grade(1.0, 0.0);
        assert!(median_frame_f32(&mut pixels, 5, 5, &grade));
        for pixel in &pixels {
            assert_eq!(pixel[0], 1.0);
            assert_eq!(pixel[1], 1.0);
            assert_eq!(pixel[2], 1.0);
        }
        assert_eq!(median_bin(2.5), 255);
        assert_eq!(median_bin(f32::INFINITY), 255);
    }

    #[test]
    fn negative_values_clamp_to_zero_and_nan_is_ordered_as_zero() {
        assert_eq!(median_bin(-0.25), 0);
        assert_eq!(median_bin(f32::NAN), 0);
        assert_eq!(median_bin(-0.0), 0);
        assert_eq!(median_bin(-f32::INFINITY), 0);
        let mut negative = vec![[-0.25_f32, -1.0, -0.5, 1.0]; 25];
        let mut not_a_number = vec![[f32::NAN, f32::NAN, f32::NAN, 1.0]; 25];
        let grade = grade(1.0, 0.0);
        assert!(median_frame_f32(&mut negative, 5, 5, &grade));
        assert!(median_frame_f32(&mut not_a_number, 5, 5, &grade));
        for pixel in &negative {
            assert_eq!(pixel[0], 0.0);
            assert_eq!(pixel[1], 0.0);
            assert_eq!(pixel[2], 0.0);
        }
        for pixel in &not_a_number {
            assert_eq!(pixel[0], 0.0);
            assert_eq!(pixel[1], 0.0);
        }
    }

    #[test]
    fn channels_do_not_bleed_into_each_other() {
        let width = 7_u32;
        let height = 5_u32;
        // One red impulse in a flat green/blue field: the impulse is removed and
        // neither colour channel moves.
        let mut pixels = vec![[0.0_f32, 0.4, 0.8, 1.0]; (width * height) as usize];
        pixels[(height / 2 * width + width / 2) as usize] = [1.0, 0.4, 0.8, 1.0];
        let grade = grade(1.0, 0.0);
        assert!(median_frame_f32(&mut pixels, width, height, &grade));
        let green = median_value(median_bin(0.4));
        let blue = median_value(median_bin(0.8));
        for (index, pixel) in pixels.iter().enumerate() {
            assert_eq!(pixel[0], 0.0, "pixel {index}");
            assert_eq!(pixel[1], green, "pixel {index}");
            assert_eq!(pixel[2], blue, "pixel {index}");
        }
    }

    #[test]
    fn edge_pixels_equal_the_clamped_padded_frame() {
        let width = 7_u32;
        let height = 5_u32;
        let radius = 2_usize;
        let mut random = Lcg(0xabc);
        let pixels: Vec<[f32; 4]> = (0..width * height)
            .map(|_| [random.code(), random.code(), random.code(), 1.0])
            .collect();
        // Replicating the border by `radius` turns every clipped tap of the
        // original frame into an ordinary tap of the padded frame.
        let padded_width = width as usize + 2 * radius;
        let padded_height = height as usize + 2 * radius;
        let mut padded = vec![[0.0_f32; 4]; padded_width * padded_height];
        for row in 0..padded_height {
            for column in 0..padded_width {
                let source_row = clamp_coord(row as i64 - radius as i64, height as usize);
                let source_column = clamp_coord(column as i64 - radius as i64, width as usize);
                padded[row * padded_width + column] =
                    pixels[source_row * width as usize + source_column];
            }
        }
        let expected = brute_force(&padded, padded_width, padded_height, radius, false);
        let mut actual = pixels.clone();
        let grade = grade(radius as f32, 0.0);
        assert!(median_frame_f32(&mut actual, width, height, &grade));
        for row in 0..height as usize {
            for column in 0..width as usize {
                let ours = actual[row * width as usize + column];
                let theirs = expected[(row + radius) * padded_width + column + radius];
                for channel in 0..4 {
                    assert_eq!(
                        ours[channel].to_bits(),
                        theirs[channel].to_bits(),
                        "pixel ({column}, {row}) channel {channel}"
                    );
                }
            }
        }
    }

    #[test]
    fn gpu_controls_pack_the_radius_and_the_toggle() {
        let packed = grade(3.0, 1.0);
        assert_eq!(packed.gpu_controls(), ([3.0, 1.0, 0.0, 0.0], [0.0; 4]));
        assert!(packed.gpu_supported());
        assert!(!grade(4.0, 0.0).gpu_supported());
        // The admission gate rounds the authored radius exactly like the kernel.
        assert!(grade(3.4, 0.0).gpu_supported());
        assert!(!grade(3.5, 0.0).gpu_supported());
        assert!(grade(1.0, 0.5).operates_on_alpha());
        assert!(!grade(1.0, 0.49).operates_on_alpha());
    }

    #[test]
    fn authored_state_reaches_the_grade_and_the_hash() {
        let authored = grade(2.4, 0.0);
        assert_eq!(authored.radius(), 2.4);
        assert_eq!(authored.radius_pixels(16, 9), 2);
        assert_eq!(grade(2.6, 0.0).radius_pixels(16, 9), 3);
        assert!(!authored.is_identity());

        use std::hash::{DefaultHasher, Hash as _, Hasher as _};
        let hash = |grade: &MedianGrade| {
            let mut hasher = DefaultHasher::new();
            "median".hash(&mut hasher);
            grade.hash_state(&mut hasher);
            hasher.finish()
        };
        assert_eq!(hash(&authored), hash(&grade(2.4, 0.0)));
        assert_ne!(hash(&authored), hash(&grade(2.4, 1.0)));
        assert_ne!(hash(&authored), hash(&grade(2.6, 0.0)));
    }

    #[test]
    fn unaddressable_frames_and_checkpoints_are_handled() {
        let grade = grade(1.0, 0.0);
        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!median_frame_f32(&mut empty, 0, 0, &grade));
        assert!(!median_frame_f32(&mut empty, 4, 0, &grade));
        assert!(!median_frame_f32(&mut empty, 0, 4, &grade));
        let mut short = vec![[0.0_f32; 4]; 3];
        assert!(!median_frame_f32(&mut short, 4, 2, &grade));
        assert!(!median_frame_f32(&mut short, u32::MAX, u32::MAX, &grade));
        assert_eq!(short, vec![[0.0_f32; 4]; 3]);

        // Sixty-four rows of sixty-four pixels stage exactly one checkpoint per
        // stride plus the staging and pass boundaries.
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; 4096];
        let mut calls = 0_u32;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert_eq!(
            apply_median_f32_controlled(&mut pixels, 64, 64, &grade, &mut checkpoint),
            Ok(true)
        );
        assert!(calls <= 4096 / 4096 + 2, "checkpoint calls: {calls}");
        assert_eq!(calls, 3);

        // An error raised while staging still leaves the frame unwritten.
        let mut pixels = vec![[0.75_f32, 0.25, 0.5, 1.0]; 8];
        let original = pixels.clone();
        let mut calls = 0;
        let mut checkpoint = || {
            calls += 1;
            if calls == 2 {
                return Err("cancelled");
            }
            Ok(())
        };
        assert_eq!(
            apply_median_f32_controlled(&mut pixels, 4, 2, &grade, &mut checkpoint),
            Err("cancelled")
        );
        assert_bit_identical(&pixels, &original, "cancelled frame");
    }

    #[test]
    fn a_flat_field_stays_flat_and_a_gradient_stays_monotone() {
        let mut flat = vec![[0.37_f32, 0.37, 0.37, 1.0]; 49];
        let grade = grade(2.0, 0.0);
        assert!(median_frame_f32(&mut flat, 7, 7, &grade));
        let expected = median_value(median_bin(0.37));
        for (index, pixel) in flat.iter().enumerate() {
            assert_eq!(pixel[0].to_bits(), expected.to_bits(), "pixel {index}");
        }

        let width = 16_u32;
        let mut gradient = Vec::with_capacity(width as usize);
        for column in 0..width {
            let value = column as f32 / (width - 1) as f32;
            gradient.push([value, value, value, 1.0]);
        }
        assert!(median_frame_f32(&mut gradient, width, 1, &grade));
        for pair in gradient.windows(2) {
            assert!(
                pair[0][0] <= pair[1][0],
                "gradient is not monotone: {} then {}",
                pair[0][0],
                pair[1][0]
            );
        }
    }

    #[test]
    fn a_radius_of_half_a_pixel_rounds_up() {
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; 5];
        pixels[2] = [1.0, 1.0, 1.0, 1.0];
        let mut rounded = pixels.clone();
        let half = grade(0.5, 0.0);
        assert_eq!(half.radius_pixels(5, 1), 1);
        assert_eq!(
            half.radius_pixels(5, 1),
            grade(1.0, 0.0).radius_pixels(5, 1)
        );
        assert!(!half.is_identity());
        assert!(median_frame_f32(&mut pixels, 5, 1, &half));
        assert!(median_frame_f32(&mut rounded, 5, 1, &grade(1.0, 0.0)));
        assert_bit_identical(&pixels, &rounded, "half pixel radius");
    }
}
