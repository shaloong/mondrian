//! Channel blur kernel for the Channel Blur built-in effect.
//!
//! Channel blur convolves the red, green, blue and alpha channels with four
//! independent box filters. Each channel owns its own authored radius, so the
//! colour channels and the coverage channel can be smeared by unrelated amounts:
//! a large red radius with a zero alpha radius blurs the image while the matte
//! stays razor sharp, and the opposite keeps the matte soft and the image crisp.
//!
//! # Straight alpha, no premultiplication
//!
//! The kernel blurs the stored straight-alpha values and never premultiplies.
//! That is a definition of the effect, not an oversight: with one radius per
//! channel there is no single coverage value a premultiplied blur could divide
//! by. Premultiplying would weight each colour sample by the coverage it started
//! with and then divide the sum by the *blurred* coverage, which is not the
//! colour the effect promises. Evaluating every channel on the raw stored values
//! also matches the eight-bit behaviour of the reference implementation, where
//! each channel is filtered in place.
//!
//! The visible consequence is that a semi-transparent edge can end up with a
//! colour that no longer agrees with its coverage. That disagreement is the
//! point of the effect, and callers who want a premultiplied result can route
//! the frame through the Box Blur effect, which applies one radius to all four
//! channels and therefore has a coverage value to divide by.
//!
//! # Separable axes
//!
//! Both axes apply the same one-dimensional box window, so a two-axis blur is a
//! horizontal pass followed by a vertical pass. Passing either axis alone is the
//! same as authoring the two-axis blur and giving the other axis a zero radius,
//! except that the two-axis pass pays for one intermediate frame.
//!
//! # GPU packing
//!
//! [`ChannelBlurGrade::gpu_controls`] returns the pair the renderer uploads:
//! `params = [red, green, blue, alpha]` (the rounded radii, in pixels) and
//! `extra = [repeat_edge, axis, 0, 0]`, where `axis` is `0` for the horizontal
//! pass and `1` for the vertical pass. A two-axis blur records two passes with
//! the same `params`; the host supplies `axis = 0` for the first and `axis = 1`
//! for the second.
//!
//! # Performance
//!
//! One traversal of the window serves all four channels: the offset loop runs to
//! the largest channel radius and each channel adds the tap only when the offset
//! falls inside its own window. Cost is therefore `O(max_radius)` per pixel and
//! per axis, not the sum of the four radii. Horizontal passes fill row bands in
//! parallel through [`crate::parallel::for_each_row_band`]; a vertical pass reads
//! one column at a time and stays on its own row band because the columns
//! interleave in memory.
//!
//! Radii are clamped to the larger frame extent, so a frame that authors more
//! radius than it has pixels collapses to a single full-frame window instead of
//! walking offsets that can only repeat the frame. There is no pyramid or
//! summed-area acceleration: the clamped radius still bounds the work.
//!
//! # Documented deviations
//!
//! 1. Radii are integral. A fractional amount is rounded to the nearest pixel
//!    (`0.5` rounds up), so there is no fractional tail weighting like the Box
//!    Blur kernel exposes.
//! 2. The authored range reaches 32767 pixels, but the effective radius is
//!    clamped to `max(width, height)`. The clamp keeps the window inside the
//!    frame the effect is currently rendering and makes oversized values cheap.
//! 3. The blur is computed on straight-alpha values. See the section above.
//!
//! # Tests
//!
//! The kernel tests cover the four-channel matrix, both edge behaviours, the
//! radius clamp, identity and invalid authored state, plus a cross-check against
//! [`crate::box_blur::box_blur_frame_f32`] on an opaque frame where its
//! premultiplication is a no-op and the two kernels must agree exactly.

/// Largest authored radius in pixels.
pub const CHANNEL_BLUR_MAX_RADIUS: u32 = 32_767;
/// Authored amount that leaves a channel untouched.
pub const CHANNEL_BLUR_DEFAULT_AMOUNT: f32 = 0.0;
/// Default edge behaviour: repeat the boundary pixel.
pub const CHANNEL_BLUR_DEFAULT_EDGE_BEHAVIOR: f32 = 1.0;

/// Uniform `axis` value of the horizontal pass.
const AXIS_HORIZONTAL: f32 = 0.0;
/// Uniform `axis` value of the vertical pass.
const AXIS_VERTICAL: f32 = 1.0;
/// Pixels between two sequential checkpoint calls.
const CHECKPOINT_STRIDE: usize = 4096;

/// Axis set one Channel Blur instance repeats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChannelBlurDirection {
    /// Horizontal pass only.
    Horizontal,
    /// Vertical pass only.
    Vertical,
    /// Horizontal, then vertical.
    Both,
}

impl ChannelBlurDirection {
    /// Every direction in authoring menu order.
    pub const ALL: [Self; 3] = [Self::Horizontal, Self::Vertical, Self::Both];

    /// Stable persistence key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
            Self::Both => "both",
        }
    }

    /// Parses a persistence key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "horizontal" => Some(Self::Horizontal),
            "vertical" => Some(Self::Vertical),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    /// One-based authoring menu value.
    #[must_use]
    pub const fn as_menu(self) -> i64 {
        match self {
            Self::Horizontal => 1,
            Self::Vertical => 2,
            Self::Both => 3,
        }
    }

    /// Parses a one-based authoring menu value.
    #[must_use]
    pub const fn from_menu(menu: i64) -> Option<Self> {
        match menu {
            1 => Some(Self::Horizontal),
            2 => Some(Self::Vertical),
            3 => Some(Self::Both),
            _ => None,
        }
    }

    /// Zero-based value used by uniforms and hashes.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Horizontal => 0,
            Self::Vertical => 1,
            Self::Both => 2,
        }
    }

    /// Menu label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Horizontal => "水平",
            Self::Vertical => "垂直",
            Self::Both => "水平和垂直",
        }
    }

    /// Whether this direction records a horizontal pass.
    #[must_use]
    pub const fn blurs_horizontal(self) -> bool {
        matches!(self, Self::Horizontal | Self::Both)
    }

    /// Whether this direction records a vertical pass.
    #[must_use]
    pub const fn blurs_vertical(self) -> bool {
        matches!(self, Self::Vertical | Self::Both)
    }
}

/// Rejected authored channel-blur state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChannelBlurError {
    /// One channel amount is not a finite number.
    NonFiniteAmount,
    /// One channel amount is outside the authored range.
    AmountOutOfRange,
    /// The edge behaviour is not a finite number.
    NonFiniteEdgeBehavior,
    /// The edge behaviour is outside the authored range.
    EdgeBehaviorOutOfRange,
}

impl std::fmt::Display for ChannelBlurError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::NonFiniteAmount => "channel blur amount must be a finite number",
            Self::AmountOutOfRange => "channel blur amount must be inside [0, 32767] pixels",
            Self::NonFiniteEdgeBehavior => "channel blur edge behavior must be a finite number",
            Self::EdgeBehaviorOutOfRange => "channel blur edge behavior must be inside [0, 1]",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ChannelBlurError {}

/// The authored parameters of one Channel Blur instance.
///
/// Amounts stay authored (`f32`) so the hash sees exactly what the project
/// stored; the pixel radii are derived per frame by [`Self::radii`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelBlurGrade {
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
    edge_behavior: f32,
    direction: ChannelBlurDirection,
}

impl ChannelBlurGrade {
    /// Validates one instance against the documented parameter ranges.
    ///
    /// # Errors
    ///
    /// Returns [`ChannelBlurError`] when an amount is not finite or rounds
    /// outside `[0, 32767]`, and when the edge behaviour is not finite or lies
    /// outside `[0, 1]`.
    pub fn new(
        red: f32,
        green: f32,
        blue: f32,
        alpha: f32,
        edge_behavior: f32,
        direction: ChannelBlurDirection,
    ) -> Result<Self, ChannelBlurError> {
        for amount in [red, green, blue, alpha] {
            if !amount.is_finite() {
                return Err(ChannelBlurError::NonFiniteAmount);
            }
            let rounded = amount.round();
            if !(0.0..=CHANNEL_BLUR_MAX_RADIUS as f32).contains(&rounded) {
                return Err(ChannelBlurError::AmountOutOfRange);
            }
        }
        if !edge_behavior.is_finite() {
            return Err(ChannelBlurError::NonFiniteEdgeBehavior);
        }
        if !(0.0..=1.0).contains(&edge_behavior) {
            return Err(ChannelBlurError::EdgeBehaviorOutOfRange);
        }
        Ok(Self { red, green, blue, alpha, edge_behavior, direction })
    }

    /// The documented default: every channel untouched.
    #[must_use]
    pub const fn documented_defaults() -> Self {
        Self {
            red: CHANNEL_BLUR_DEFAULT_AMOUNT,
            green: CHANNEL_BLUR_DEFAULT_AMOUNT,
            blue: CHANNEL_BLUR_DEFAULT_AMOUNT,
            alpha: CHANNEL_BLUR_DEFAULT_AMOUNT,
            edge_behavior: CHANNEL_BLUR_DEFAULT_EDGE_BEHAVIOR,
            direction: ChannelBlurDirection::Both,
        }
    }

    /// The authored direction.
    #[must_use]
    pub const fn direction(self) -> ChannelBlurDirection {
        self.direction
    }

    /// Whether out-of-frame taps repeat the boundary pixel.
    ///
    /// `edge_behavior >= 0.5` repeats the edge; anything below counts
    /// out-of-frame taps as transparent black while keeping the `2r + 1`
    /// denominator, so the frame edges fade instead of brightening.
    #[must_use]
    pub const fn repeat_edge_pixels(self) -> bool {
        self.edge_behavior >= 0.5
    }

    /// The authored per-channel amounts, in red/green/blue/alpha order.
    #[must_use]
    pub fn amounts(self) -> [f32; 4] {
        [self.red, self.green, self.blue, self.alpha]
    }

    /// The pixel radii this instance applies to a frame of the given extent.
    ///
    /// Each amount is rounded to the nearest pixel and clamped to
    /// `max(width, height)`, the largest window the frame can hold.
    #[must_use]
    pub fn radii(self, width: usize, height: usize) -> [u32; 4] {
        let limit = u32::try_from(width.max(height)).unwrap_or(u32::MAX);
        self.amounts().map(|amount| {
            let rounded = amount.round();
            let radius = if rounded > 0.0 { rounded as u32 } else { 0 };
            radius.min(limit)
        })
    }

    /// Whether this instance leaves every pixel untouched.
    #[must_use]
    pub const fn is_identity(self) -> bool {
        // `f32::round` is not const, so compare the authored amounts directly:
        // every amount that rounds to zero is below 0.5.
        self.red < 0.5 && self.green < 0.5 && self.blue < 0.5 && self.alpha < 0.5
    }

    /// The `(params, extra)` pair the renderer uploads.
    ///
    /// `params = [red, green, blue, alpha]` carries the authored amounts, which
    /// the fragment shader rounds; `extra = [repeat_edge, axis, 0, 0]` selects
    /// the edge behaviour and one axis. A two-axis blur records two passes: this
    /// pair describes the first (horizontal) pass.
    #[must_use]
    pub fn gpu_controls(self) -> ([f32; 4], [f32; 4]) {
        let axis = match self.direction {
            ChannelBlurDirection::Vertical => AXIS_VERTICAL,
            ChannelBlurDirection::Horizontal | ChannelBlurDirection::Both => AXIS_HORIZONTAL,
        };
        let repeat_edge = if self.repeat_edge_pixels() { 1.0 } else { 0.0 };
        (self.amounts(), [repeat_edge, axis, 0.0, 0.0])
    }

    /// Feeds every authored value into a semantic hash.
    pub(crate) fn hash_state<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash as _;
        self.amounts().map(f32::to_bits).hash(state);
        self.edge_behavior.to_bits().hash(state);
        self.direction.as_u32().hash(state);
    }
}

/// Applies one authored Channel Blur instance to a straight-alpha frame.
///
/// Returns `false` for a frame whose geometry cannot be addressed by `pixels`
/// (`width * height` must fit, be non-zero, and be covered by the slice).
#[must_use]
pub fn channel_blur_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &ChannelBlurGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_channel_blur_f32_controlled(pixels, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

/// Applies one Channel Blur instance while polling `checkpoint`.
///
/// `checkpoint` runs before the frame is staged, every [`CHECKPOINT_STRIDE`]
/// pixels while staging, after each axis pass, and once after the result is
/// written back. A checkpoint error aborts the frame immediately.
///
/// # Errors
///
/// Propagates the error returned by `checkpoint`.
pub(crate) fn apply_channel_blur_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &ChannelBlurGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || width == 0 || height == 0 || pixels.len() < pixel_count {
        return Ok(false);
    }
    let radii = grade.radii(width, height);
    let max_radius = radii.iter().copied().max().unwrap_or(0);
    if max_radius == 0 {
        return Ok(true);
    }
    let frame = &mut pixels[..pixel_count];
    checkpoint()?;
    // The blur reads a stable source while writing another buffer, so the frame
    // is staged once and the two buffers swap roles on every axis pass.
    let mut source = Vec::with_capacity(pixel_count);
    for (index, pixel) in frame.iter().enumerate() {
        if index % CHECKPOINT_STRIDE == 0 {
            checkpoint()?;
        }
        source.push(*pixel);
    }
    checkpoint()?;
    let mut destination = vec![[0.0_f32; 4]; pixel_count];
    let context = AxisContext {
        radii,
        max_radius: i64::from(max_radius),
        horizontal: false,
        width,
        height,
        repeat_edge_pixels: grade.repeat_edge_pixels(),
    };
    for horizontal in [true, false] {
        let blurs_axis = if horizontal {
            grade.direction().blurs_horizontal()
        } else {
            grade.direction().blurs_vertical()
        };
        if !blurs_axis {
            continue;
        }
        let current: &[[f32; 4]] = &source;
        let axis = AxisContext { horizontal, ..context };
        crate::parallel::for_each_row_band(
            &mut destination,
            width,
            0..height,
            |row_start, band| {
                axis.apply(current, band, row_start);
            },
        );
        std::mem::swap(&mut source, &mut destination);
        checkpoint()?;
    }
    crate::parallel::for_each_row_band(frame, width, 0..height, |row_start, band| {
        let offset = row_start * width;
        band.copy_from_slice(&source[offset..offset + band.len()]);
    });
    checkpoint()?;
    Ok(true)
}

/// One axis pass over the four independent channel windows.
#[derive(Clone, Copy)]
struct AxisContext {
    radii: [u32; 4],
    max_radius: i64,
    horizontal: bool,
    width: usize,
    height: usize,
    repeat_edge_pixels: bool,
}

impl AxisContext {
    /// Fills one band of destination rows from the whole source frame.
    fn apply(&self, source: &[[f32; 4]], band: &mut [[f32; 4]], row_start: usize) {
        for (row_offset, row_pixels) in band.chunks_mut(self.width).enumerate() {
            let row = row_start + row_offset;
            for (column, pixel) in row_pixels.iter_mut().enumerate() {
                *pixel = self.sample(source, row, column);
            }
        }
    }

    /// Evaluates the four channel windows around one pixel.
    ///
    /// The offset loop runs to the largest radius; each channel includes a tap
    /// only when the offset is inside its own window. Out-of-frame taps either
    /// repeat the nearest boundary pixel or contribute nothing, and either way
    /// the divisor stays `2 * radius + 1`.
    fn sample(&self, source: &[[f32; 4]], row: usize, column: usize) -> [f32; 4] {
        let last_column = self.width as i64 - 1;
        let last_row = self.height as i64 - 1;
        let mut sums = [0.0_f64; 4];
        for offset in -self.max_radius..=self.max_radius {
            let distance = offset.abs();
            let tap = if self.horizontal {
                let target = column as i64 + offset;
                if target < 0 || target > last_column {
                    if !self.repeat_edge_pixels {
                        continue;
                    }
                    source[row * self.width + target.clamp(0, last_column) as usize]
                } else {
                    source[row * self.width + target as usize]
                }
            } else {
                let target = row as i64 + offset;
                if target < 0 || target > last_row {
                    if !self.repeat_edge_pixels {
                        continue;
                    }
                    source[target.clamp(0, last_row) as usize * self.width + column]
                } else {
                    source[target as usize * self.width + column]
                }
            };
            for (channel, sum) in sums.iter_mut().enumerate() {
                if distance <= i64::from(self.radii[channel]) {
                    *sum += f64::from(tap[channel]);
                }
            }
        }
        let mut output = [0.0_f32; 4];
        for (channel, value) in output.iter_mut().enumerate() {
            let divider = 2.0 * f64::from(self.radii[channel]) + 1.0;
            *value = (sums[channel] / divider) as f32;
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(red: f32, green: f32, blue: f32, alpha: f32) -> ChannelBlurGrade {
        ChannelBlurGrade::new(
            red,
            green,
            blue,
            alpha,
            CHANNEL_BLUR_DEFAULT_EDGE_BEHAVIOR,
            ChannelBlurDirection::Both,
        )
        .expect("valid channel blur grade")
    }

    fn opaque(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = Vec::with_capacity(width * height);
        for index in 0..width * height {
            let value = (index % 5) as f32 * 0.25;
            pixels.push([value, 1.0 - value, 0.5, 1.0]);
        }
        pixels
    }

    #[test]
    fn menus_are_one_based_and_uniforms_are_zero_based() {
        assert_eq!(ChannelBlurDirection::ALL.len(), 3);
        for (index, direction) in ChannelBlurDirection::ALL.iter().copied().enumerate() {
            assert_eq!(direction.as_menu(), index as i64 + 1);
            assert_eq!(
                ChannelBlurDirection::from_menu(direction.as_menu()),
                Some(direction)
            );
            assert_eq!(
                ChannelBlurDirection::from_key(direction.as_key()),
                Some(direction)
            );
            assert_eq!(direction.as_u32(), index as u32);
        }
        assert_eq!(ChannelBlurDirection::from_menu(0), None);
        assert_eq!(ChannelBlurDirection::from_menu(4), None);
        assert_eq!(ChannelBlurDirection::from_key("diagonal"), None);
        let horizontal =
            ChannelBlurGrade::new(0.0, 0.0, 0.0, 0.0, 1.0, ChannelBlurDirection::Horizontal)
                .expect("valid grade");
        assert_eq!(horizontal.gpu_controls().1, [1.0, 0.0, 0.0, 0.0]);
        let vertical =
            ChannelBlurGrade::new(0.0, 0.0, 0.0, 0.0, 0.0, ChannelBlurDirection::Vertical)
                .expect("valid grade");
        assert_eq!(vertical.gpu_controls().1, [0.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn defaults_are_the_documented_identity() {
        let grade = ChannelBlurGrade::documented_defaults();
        assert!(grade.is_identity());
        assert_eq!(grade.amounts(), [0.0; 4]);
        assert!(grade.repeat_edge_pixels());
        assert_eq!(grade.direction(), ChannelBlurDirection::Both);
    }

    #[test]
    fn invalid_authored_state_is_rejected() {
        assert_eq!(
            ChannelBlurGrade::new(f32::NAN, 0.0, 0.0, 0.0, 1.0, ChannelBlurDirection::Both),
            Err(ChannelBlurError::NonFiniteAmount)
        );
        assert_eq!(
            ChannelBlurGrade::new(
                0.0,
                f32::INFINITY,
                0.0,
                0.0,
                1.0,
                ChannelBlurDirection::Both
            ),
            Err(ChannelBlurError::NonFiniteAmount)
        );
        assert_eq!(
            ChannelBlurGrade::new(0.0, 0.0, -1.0, 0.0, 1.0, ChannelBlurDirection::Both),
            Err(ChannelBlurError::AmountOutOfRange)
        );
        assert_eq!(
            ChannelBlurGrade::new(0.0, 0.0, 0.0, 32_768.0, 1.0, ChannelBlurDirection::Both),
            Err(ChannelBlurError::AmountOutOfRange)
        );
        assert_eq!(
            ChannelBlurGrade::new(0.0, 0.0, 0.0, 0.0, f32::NAN, ChannelBlurDirection::Both),
            Err(ChannelBlurError::NonFiniteEdgeBehavior)
        );
        assert_eq!(
            ChannelBlurGrade::new(0.0, 0.0, 0.0, 0.0, 1.5, ChannelBlurDirection::Both),
            Err(ChannelBlurError::EdgeBehaviorOutOfRange)
        );
        assert_eq!(
            ChannelBlurError::NonFiniteAmount.to_string(),
            "channel blur amount must be a finite number"
        );
        assert_eq!(
            ChannelBlurError::AmountOutOfRange.to_string(),
            "channel blur amount must be inside [0, 32767] pixels"
        );
        assert_eq!(
            ChannelBlurError::NonFiniteEdgeBehavior.to_string(),
            "channel blur edge behavior must be a finite number"
        );
        assert_eq!(
            ChannelBlurError::EdgeBehaviorOutOfRange.to_string(),
            "channel blur edge behavior must be inside [0, 1]"
        );
    }

    #[test]
    fn zero_amounts_are_the_identity() {
        let mut pixels = opaque(7, 5);
        let original = pixels.clone();
        let grade = ChannelBlurGrade::documented_defaults();
        assert!(grade.is_identity());
        assert!(channel_blur_frame_f32(&mut pixels, 7, 5, &grade));
        assert_eq!(pixels, original);
    }

    #[test]
    fn equal_radii_on_an_opaque_frame_match_the_box_blur_kernel() {
        // Both kernels are exact on these values: the frame only carries
        // quarter-pixel steps, so every f64 partial sum is representable and the
        // premultiplication inside the box blur is an exact no-op (alpha = 1).
        let width = 9;
        let height = 7;
        let mut channel = opaque(width, height);
        let mut reference = channel.clone();
        let grade = grade(2.0, 2.0, 2.0, 2.0);
        assert!(channel_blur_frame_f32(&mut channel, width, height, &grade));
        assert!(crate::box_blur::box_blur_frame_f32(
            &mut reference,
            width,
            height,
            2.0,
            1,
            crate::BoxBlurDirection::HorizontalAndVertical,
            true,
        ));
        for (index, (ours, theirs)) in channel.iter().zip(reference.iter()).enumerate() {
            for channel_index in 0..4 {
                assert_eq!(
                    ours[channel_index], theirs[channel_index],
                    "pixel {index} channel {channel_index}"
                );
            }
        }
    }

    #[test]
    fn alpha_only_blur_leaves_the_colour_channels_bit_identical() {
        let width = 6;
        let height = 4;
        let mut pixels: Vec<[f32; 4]> = (0..width * height)
            .map(|index| {
                let coverage = if index % 3 == 0 { 0.0 } else { 1.0 };
                [0.2, 0.4, 0.8, coverage]
            })
            .collect();
        let colours: Vec<[f32; 3]> =
            pixels.iter().map(|pixel| [pixel[0], pixel[1], pixel[2]]).collect();
        let grade = grade(0.0, 0.0, 0.0, 2.0);
        assert!(channel_blur_frame_f32(&mut pixels, width, height, &grade));
        for (pixel, colour) in pixels.iter().zip(colours.iter()) {
            assert_eq!([pixel[0], pixel[1], pixel[2]], *colour);
        }
        assert!(pixels.iter().any(|pixel| pixel[3] != 1.0 && pixel[3] != 0.0));
    }

    #[test]
    fn channels_do_not_bleed_into_each_other() {
        let width = 7;
        let height = 5;
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; width * height];
        let centre = (height / 2) * width + width / 2;
        pixels[centre] = [1.0, 0.0, 0.0, 1.0];
        let grade = grade(1.0, 0.0, 0.0, 0.0);
        assert!(channel_blur_frame_f32(&mut pixels, width, height, &grade));
        let total: f32 = pixels.iter().map(|pixel| pixel[0]).sum();
        assert!(
            (total - 1.0).abs() < 1.0e-5,
            "red energy must be conserved: {total}"
        );
        for pixel in &pixels {
            assert_eq!(pixel[1], 0.0);
            assert_eq!(pixel[2], 0.0);
            assert_eq!(pixel[3], 1.0);
        }
        let corner = &pixels[centre - 1];
        assert!(corner[0] > 0.0);
    }

    #[test]
    fn each_direction_only_touches_its_own_axis() {
        let width = 5;
        let height = 5;
        let mut horizontal = vec![[0.0, 0.0, 0.0, 1.0]; width * height];
        let centre = 2 * width + 2;
        horizontal[centre] = [1.0, 0.0, 0.0, 1.0];
        let horizontal_grade =
            ChannelBlurGrade::new(1.0, 0.0, 0.0, 0.0, 1.0, ChannelBlurDirection::Horizontal)
                .expect("valid grade");
        assert!(channel_blur_frame_f32(
            &mut horizontal,
            width,
            height,
            &horizontal_grade
        ));
        assert!(horizontal[centre - 1][0] > 0.0);
        assert!(horizontal[centre + 1][0] > 0.0);
        assert_eq!(horizontal[centre - width][0], 0.0);
        assert_eq!(horizontal[centre + width][0], 0.0);

        let mut vertical = vec![[0.0, 0.0, 0.0, 1.0]; width * height];
        vertical[centre] = [1.0, 0.0, 0.0, 1.0];
        let vertical_grade =
            ChannelBlurGrade::new(1.0, 0.0, 0.0, 0.0, 1.0, ChannelBlurDirection::Vertical)
                .expect("valid grade");
        assert!(channel_blur_frame_f32(
            &mut vertical,
            width,
            height,
            &vertical_grade
        ));
        assert!(vertical[centre - width][0] > 0.0);
        assert!(vertical[centre + width][0] > 0.0);
        assert_eq!(vertical[centre - 1][0], 0.0);
        assert_eq!(vertical[centre + 1][0], 0.0);

        let mut both = vec![[0.0, 0.0, 0.0, 1.0]; width * height];
        both[centre] = [1.0, 0.0, 0.0, 1.0];
        let both_grade = grade(1.0, 0.0, 0.0, 0.0);
        assert!(channel_blur_frame_f32(
            &mut both,
            width,
            height,
            &both_grade
        ));
        assert!(both[centre - width - 1][0] > 0.0);
        assert!(both[centre + width + 1][0] > 0.0);
        assert_eq!(both[centre - width - 2][0], 0.0);
        assert_eq!(both[centre + width + 2][0], 0.0);
    }

    #[test]
    fn repeating_edges_and_transparent_edges_are_the_documented_modes() {
        let mut repeated = vec![[0.0_f32; 4]; 5];
        repeated[0] = [1.0, 1.0, 1.0, 1.0];
        let mut transparent = repeated.clone();
        let grade =
            ChannelBlurGrade::new(2.0, 2.0, 2.0, 2.0, 1.0, ChannelBlurDirection::Horizontal)
                .expect("valid grade");
        assert!(channel_blur_frame_f32(&mut repeated, 5, 1, &grade));
        // Three taps of the window repeat the boundary pixel, two are in frame.
        assert!(
            (repeated[0][0] - 0.6).abs() < 1.0e-6,
            "repeated edge: {:?}",
            repeated[0]
        );
        assert!((repeated[0][3] - 0.6).abs() < 1.0e-6);
        let grade =
            ChannelBlurGrade::new(2.0, 2.0, 2.0, 2.0, 0.0, ChannelBlurDirection::Horizontal)
                .expect("valid grade");
        assert!(channel_blur_frame_f32(&mut transparent, 5, 1, &grade));
        // Out-of-frame taps count as transparent black but stay in the divisor.
        assert!(
            (transparent[0][0] - 0.2).abs() < 1.0e-6,
            "transparent edge: {:?}",
            transparent[0]
        );
        assert!((transparent[0][3] - 0.2).abs() < 1.0e-6);
        assert_eq!(transparent[1][0], 0.2);
    }

    #[test]
    fn oversized_amounts_clamp_to_the_largest_frame_extent() {
        let width = 8;
        let height = 6;
        let mut clamped = opaque(width, height);
        let mut explicit = clamped.clone();
        let huge = grade(32_767.0, 32_767.0, 32_767.0, 32_767.0);
        assert_eq!(huge.radii(width, height), [8, 8, 8, 8]);
        assert!(channel_blur_frame_f32(&mut clamped, width, height, &huge));
        let largest = grade(8.0, 8.0, 8.0, 8.0);
        assert!(channel_blur_frame_f32(
            &mut explicit,
            width,
            height,
            &largest
        ));
        assert_eq!(clamped, explicit);
    }

    #[test]
    fn unaddressable_frames_are_rejected() {
        let grade = grade(2.0, 2.0, 2.0, 2.0);
        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!channel_blur_frame_f32(&mut empty, 0, 0, &grade));
        assert!(!channel_blur_frame_f32(&mut empty, 4, 0, &grade));
        assert!(!channel_blur_frame_f32(&mut empty, 0, 4, &grade));
        let mut short = vec![[0.0_f32; 4]; 3];
        assert!(!channel_blur_frame_f32(&mut short, 4, 2, &grade));
        assert!(!channel_blur_frame_f32(
            &mut short,
            usize::MAX,
            usize::MAX,
            &grade
        ));
        assert_eq!(short, vec![[0.0_f32; 4]; 3]);
    }

    #[test]
    fn checkpoint_cadence_is_bounded() {
        let mut pixels = opaque(4, 2);
        let mut calls = 0;
        let grade =
            ChannelBlurGrade::new(1.0, 1.0, 1.0, 1.0, 1.0, ChannelBlurDirection::Horizontal)
                .expect("valid grade");
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert_eq!(
            apply_channel_blur_f32_controlled(&mut pixels, 4, 2, &grade, &mut checkpoint),
            Ok(true)
        );
        // Staging start, first staged pixel, staging end, pass end, write-back end.
        assert_eq!(calls, 5);
    }

    #[test]
    fn the_checkpoint_error_cancels_the_frame() {
        let mut pixels = opaque(4, 2);
        let original = pixels.clone();
        let grade = grade(1.0, 0.0, 0.0, 0.0);
        let mut calls = 0;
        let mut checkpoint = || {
            calls += 1;
            if calls == 3 {
                return Err("cancelled");
            }
            Ok(())
        };
        assert_eq!(
            apply_channel_blur_f32_controlled(&mut pixels, 4, 2, &grade, &mut checkpoint),
            Err("cancelled")
        );
        assert_eq!(pixels, original);
    }

    #[test]
    fn authored_amounts_reach_the_radii_and_the_hash() {
        let grade = ChannelBlurGrade::new(2.4, 2.6, 0.4, 7.5, 0.4, ChannelBlurDirection::Vertical)
            .expect("valid grade");
        assert_eq!(grade.radii(16, 9), [2, 3, 0, 8]);
        assert!(!grade.is_identity());
        assert!(!grade.repeat_edge_pixels());
        assert_eq!(grade.gpu_controls().0, [2.4, 2.6, 0.4, 7.5]);
        assert_eq!(grade.gpu_controls().1, [0.0, 1.0, 0.0, 0.0]);

        use std::hash::{DefaultHasher, Hash as _, Hasher as _};
        let hash = |grade: &ChannelBlurGrade| {
            let mut hasher = DefaultHasher::new();
            "channel_blur".hash(&mut hasher);
            grade.hash_state(&mut hasher);
            hasher.finish()
        };
        let other = ChannelBlurGrade::new(2.4, 2.6, 0.4, 7.5, 1.0, ChannelBlurDirection::Vertical)
            .expect("valid grade");
        assert_ne!(hash(&grade), hash(&other));
    }

    #[test]
    fn a_radius_of_half_a_pixel_rounds_up() {
        let mut pixels = vec![[0.0_f32; 4]; 5];
        pixels[2] = [1.0, 1.0, 1.0, 1.0];
        let mut rounded = pixels.clone();
        let half = grade(0.5, 0.5, 0.5, 0.5);
        assert_eq!(half.radii(5, 1), [1, 1, 1, 1]);
        let one = grade(1.0, 1.0, 1.0, 1.0);
        assert!(channel_blur_frame_f32(&mut pixels, 5, 1, &half));
        assert!(channel_blur_frame_f32(&mut rounded, 5, 1, &one));
        assert_eq!(pixels, rounded);
    }
}
