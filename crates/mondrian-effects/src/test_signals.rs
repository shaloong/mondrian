//! Test signals: a generated calibration frame.
//!
//! This effect is a pure generator. Its output depends only on the pixel
//! coordinate and the frame extent, so it reads no input pixel and needs no
//! halo: the whole frame is replaced by the pattern. That is why it is a fused
//! point op rather than a spatial pass.
//!
//! # Documented reading
//!
//! * `pattern` selects one of [`TestSignalsPattern::ALL`]; the menu is
//!   one-based, the uniform ordinal zero-based.
//! * `intensity` (0..=100) multiplies the linear rgb of the pattern only. It
//!   never scales the calibration marks or the safe frames: those are
//!   measurement overlays and must stay at full white so the operator can
//!   still see them on a black (intensity 0) frame.
//! * `display_labels` overlays the calibration marks: a 10% grid, a centre
//!   cross, and four corner ticks. These are our own geometry, not AE text
//!   labels; text layout stays out of scope for a point kernel.
//! * `safe_frames` traces the action safe (93%) and title safe (90%)
//!   rectangles with a 1.5 px white line. It never crops.
//! * Alpha is always 1: the generated frame covers every pixel.
//!
//! # Uniform packing
//!
//! [`TestSignalsGrade::gpu_controls`] returns
//! `([pattern, intensity / 100, labels, safety], [0.0; 4])`. The frame size
//! travels in the point uniform's `extra0` as `[width, height, 0, 0]`.
//!
//! The host stamps that size, not the grade: `effect_uniforms` in
//! `mondrian-renderer` has no frame extent in scope, so
//! `record_layer_pass_scheduled` rewrites `extra0` for
//! [`crate::EffectGpuPointOp::TestSignals`] after assembling the uniforms,
//! using the source extent from `uniforms.geometry[2..4]`. Keeping the
//! `gpu_controls` contract at `[0.0; 4]` leaves the uniform assembly and its
//! existing call sites untouched while every production path (composite and
//! `record_point_effect_pass`) still funnels through that one function.

use crate::parallel::for_each_row_band;

/// Number of patterns in the `pattern` menu.
pub const TEST_SIGNALS_PATTERN_COUNT: usize = 6;

/// Lowest authored `intensity`, in percent.
pub const TEST_SIGNALS_MIN_INTENSITY: f32 = 0.0;

/// Highest authored `intensity`, in percent.
pub const TEST_SIGNALS_MAX_INTENSITY: f32 = 100.0;

/// Default authored `intensity`, in percent.
pub const TEST_SIGNALS_DEFAULT_INTENSITY: f32 = 100.0;

/// Amplitude of the SMPTE bars: the bars sit at 75%, not 100%.
const BAR_LEVEL: f32 = 0.75;

/// The pluge steps sit 4% below and above black.
const PLUGE_LEVEL: f32 = 0.04;

/// Checkerboard cell edge, in pixels.
const CHECKER_CELL_PIXELS: u32 = 32;

/// Concentric ring width, in pixels.
const RING_WIDTH_PIXELS: u32 = 16;

/// The zone plate's outermost cycle is eight times the shorter frame side.
const ZONE_PLATE_EXTENT_DIVISOR: f32 = 8.0;

/// Calibration grid steps across each axis (a 10% grid).
const GRID_STEPS: u32 = 10;

/// Action safe rectangle, as a fraction of the frame.
const SAFE_ACTION_SCALE: f32 = 0.93;

/// Title safe rectangle, as a fraction of the frame.
const SAFE_TITLE_SCALE: f32 = 0.90;

/// Half of the 1.5 px safe-frame line.
const SAFE_LINE_HALF_PIXELS: f32 = 0.75;

/// One pattern of the `pattern` menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TestSignalsPattern {
    /// SMPTE-ish 75% colour bars with a reverse-blue and pluge lower third.
    Bars,
    /// Eleven equal-width linear grey steps.
    Grayscale,
    /// Sixteen equal-width linear steps that may exceed 1.0.
    HdrStaircase,
    /// 32 px black and white checkerboard.
    Checkerboard,
    /// 16 px concentric rings about the frame centre.
    ConcentricRings,
    /// Radial zone plate that resolves toward the frame centre.
    ZonePlate,
}

impl TestSignalsPattern {
    /// Every pattern, in menu order.
    pub const ALL: [Self; TEST_SIGNALS_PATTERN_COUNT] = [
        Self::Bars,
        Self::Grayscale,
        Self::HdrStaircase,
        Self::Checkerboard,
        Self::ConcentricRings,
        Self::ZonePlate,
    ];

    /// Stable key used by the authoring property.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Bars => "bars",
            Self::Grayscale => "grayscale",
            Self::HdrStaircase => "hdr_staircase",
            Self::Checkerboard => "checkerboard",
            Self::ConcentricRings => "concentric_rings",
            Self::ZonePlate => "zone_plate",
        }
    }

    /// Resolves an authored key back to a pattern.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|pattern| pattern.as_key() == key)
    }

    /// One-based menu index, matching the authoring metadata.
    #[must_use]
    pub const fn as_menu(self) -> i64 {
        self.as_u32() as i64 + 1
    }

    /// Resolves a one-based menu index.
    #[must_use]
    pub const fn from_menu(menu: i64) -> Option<Self> {
        if menu < 1 || menu > TEST_SIGNALS_PATTERN_COUNT as i64 {
            return None;
        }
        Some(Self::ALL[(menu - 1) as usize])
    }

    /// Zero-based uniform ordinal.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Bars => 0,
            Self::Grayscale => 1,
            Self::HdrStaircase => 2,
            Self::Checkerboard => 3,
            Self::ConcentricRings => 4,
            Self::ZonePlate => 5,
        }
    }

    /// Chinese display label used by the authoring metadata.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Bars => "彩条",
            Self::Grayscale => "灰阶",
            Self::HdrStaircase => "HDR 阶梯",
            Self::Checkerboard => "棋盘",
            Self::ConcentricRings => "同心圆",
            Self::ZonePlate => "分辨率图",
        }
    }
}

/// Reason a [`TestSignalsGrade`] cannot be authored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TestSignalsError {
    /// The intensity is NaN or infinite.
    NonFiniteIntensity,
    /// The intensity is outside 0..=100.
    IntensityOutOfRange,
}

impl std::fmt::Display for TestSignalsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NonFiniteIntensity => "test signals intensity must be finite",
            Self::IntensityOutOfRange => "test signals intensity must be within 0..=100",
        })
    }
}

impl std::error::Error for TestSignalsError {}

/// Authored state of one test-signal generator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TestSignalsGrade {
    pattern: TestSignalsPattern,
    intensity: f32,
    display_labels: bool,
    safe_frames: bool,
}

impl TestSignalsGrade {
    /// Builds a grade, rejecting intensities the kernel cannot honour.
    pub fn new(
        pattern: TestSignalsPattern,
        intensity: f32,
        display_labels: bool,
        safe_frames: bool,
    ) -> Result<Self, TestSignalsError> {
        if !intensity.is_finite() {
            return Err(TestSignalsError::NonFiniteIntensity);
        }
        if !(TEST_SIGNALS_MIN_INTENSITY..=TEST_SIGNALS_MAX_INTENSITY).contains(&intensity) {
            return Err(TestSignalsError::IntensityOutOfRange);
        }
        Ok(Self { pattern, intensity, display_labels, safe_frames })
    }

    /// Selected pattern.
    #[must_use]
    pub const fn pattern(self) -> TestSignalsPattern {
        self.pattern
    }

    /// Authored intensity, in percent.
    #[must_use]
    pub const fn intensity(self) -> f32 {
        self.intensity
    }

    /// Whether the calibration marks are drawn.
    #[must_use]
    pub const fn display_labels(self) -> bool {
        self.display_labels
    }

    /// Whether the safe frames are drawn.
    #[must_use]
    pub const fn safe_frames(self) -> bool {
        self.safe_frames
    }

    /// A generator always replaces the frame, so it is never an identity.
    #[must_use]
    pub const fn is_identity(self) -> bool {
        false
    }

    /// Uniform cells for the fused point pass.
    ///
    /// `params` is `[pattern, intensity / 100, labels, safety]`; `extra0` is
    /// reserved for the frame size and is stamped by the host (see the module
    /// documentation).
    #[must_use]
    pub fn gpu_controls(self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.pattern.as_u32() as f32,
                self.intensity / TEST_SIGNALS_MAX_INTENSITY,
                if self.display_labels { 1.0 } else { 0.0 },
                if self.safe_frames { 1.0 } else { 0.0 },
            ],
            [0.0; 4],
        )
    }
}

/// Generates one pixel of the requested pattern.
#[must_use]
pub fn test_signals_pixel(
    column: u32,
    row: u32,
    width: u32,
    height: u32,
    grade: &TestSignalsGrade,
) -> [f32; 4] {
    if width == 0 || height == 0 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    let pattern = pattern_rgb(grade.pattern, column, row, width, height);
    let intensity = grade.intensity / TEST_SIGNALS_MAX_INTENSITY;
    let mut rgb = [
        pattern[0] * intensity,
        pattern[1] * intensity,
        pattern[2] * intensity,
    ];
    if grade.display_labels && mark_pixel(column, row, width, height) {
        rgb = [1.0; 3];
    }
    if grade.safe_frames && safe_frame_pixel(column, row, width, height) {
        rgb = [1.0; 3];
    }
    [rgb[0], rgb[1], rgb[2], 1.0]
}

/// Generates a whole frame, row-banded across the available cores.
#[must_use]
pub fn test_signals_frame_f32(
    width: usize,
    height: usize,
    grade: &TestSignalsGrade,
) -> Vec<[f32; 4]> {
    let Some(expected) = width.checked_mul(height) else {
        return Vec::new();
    };
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let mut pixels = vec![[0.0; 4]; expected];
    fill_test_signals(&mut pixels, width, height, grade);
    pixels
}

/// Overwrites an entire float frame with the generated pattern.
///
/// Returns `Ok(false)` when the buffer length does not match the extent; a
/// generator is never an identity, so a valid frame always returns `Ok(true)`.
pub(crate) fn apply_test_signals_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &TestSignalsGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(expected) = width.checked_mul(height) else {
        return Ok(false);
    };
    if width == 0 || height == 0 || pixels.len() != expected {
        return Ok(false);
    }
    // The band closure is `Fn`, so the cooperative checkpoint brackets the
    // parallel section instead of landing inside it.
    checkpoint()?;
    fill_test_signals(pixels, width, height, grade);
    checkpoint()?;
    Ok(true)
}

/// Writes the pattern into a buffer whose length already matches the extent.
fn fill_test_signals(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &TestSignalsGrade,
) {
    let width_u32 = width as u32;
    let height_u32 = height as u32;
    for_each_row_band(pixels, width, 0..height, |first_row, band| {
        for (offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = (first_row + offset) as u32;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                *pixel = test_signals_pixel(column as u32, row, width_u32, height_u32, grade);
            }
        }
    });
}

/// Linear rgb of the selected pattern, before intensity and overlays.
fn pattern_rgb(
    pattern: TestSignalsPattern,
    column: u32,
    row: u32,
    width: u32,
    height: u32,
) -> [f32; 3] {
    match pattern {
        TestSignalsPattern::Bars => bars_rgb(column, row, width, height),
        TestSignalsPattern::Grayscale => {
            let level = column_index(column, width, 11) as f32 / 10.0;
            [level; 3]
        }
        TestSignalsPattern::HdrStaircase => {
            // The only pattern allowed above 1.0: 16 linear steps, no clamp.
            let level = column_index(column, width, 16) as f32 / 15.0;
            [level; 3]
        }
        TestSignalsPattern::Checkerboard => {
            let cell = (column / CHECKER_CELL_PIXELS + row / CHECKER_CELL_PIXELS) % 2;
            if cell == 0 {
                [1.0; 3]
            } else {
                [0.0; 3]
            }
        }
        TestSignalsPattern::ConcentricRings => {
            let radius = pixel_radius(column, row, width, height);
            let ring = (radius / RING_WIDTH_PIXELS as f32) as u32;
            if ring.is_multiple_of(2) {
                [1.0; 3]
            } else {
                [0.0; 3]
            }
        }
        TestSignalsPattern::ZonePlate => {
            let extent = width.min(height) as f32;
            let wave_number = std::f32::consts::TAU / (ZONE_PLATE_EXTENT_DIVISOR * extent);
            let radius_squared = pixel_radius_squared(column, row, width, height);
            // The design calls for a peak at the centre, so the sine carries a
            // quarter-turn phase: `sin(k * r^2 + pi / 2)` is 1.0 at r = 0.
            let value =
                0.5 + 0.5 * (wave_number * radius_squared + std::f32::consts::FRAC_PI_2).sin();
            [value; 3]
        }
    }
}

/// SMPTE-ish bars: 75% colour bars over a reverse-blue and pluge lower third.
fn bars_rgb(column: u32, row: u32, width: u32, height: u32) -> [f32; 3] {
    let band = (row * 3) / height;
    if band < 2 {
        return smpte_bar(column_index(column, width, 7));
    }
    if column * 2 < width {
        return reverse_bar(column_index(column * 2, width, 7));
    }
    let pluge_start = (width * 7) / 8;
    if width > pluge_start && column >= pluge_start {
        return match column_index(column - pluge_start, width - pluge_start, 3) {
            0 => [-PLUGE_LEVEL; 3],
            1 => [0.0; 3],
            _ => [PLUGE_LEVEL; 3],
        };
    }
    [0.0; 3]
}

/// One of the seven 75% bars: white, yellow, cyan, green, magenta, red, blue.
fn smpte_bar(index: u32) -> [f32; 3] {
    match index {
        0 => [BAR_LEVEL; 3],
        1 => [BAR_LEVEL, BAR_LEVEL, 0.0],
        2 => [0.0, BAR_LEVEL, BAR_LEVEL],
        3 => [0.0, BAR_LEVEL, 0.0],
        4 => [BAR_LEVEL, 0.0, BAR_LEVEL],
        5 => [BAR_LEVEL, 0.0, 0.0],
        _ => [0.0, 0.0, BAR_LEVEL],
    }
}

/// The reverse bars: blue, black, magenta, black, cyan, black, white.
fn reverse_bar(index: u32) -> [f32; 3] {
    match index {
        0 => [0.0, 0.0, BAR_LEVEL],
        2 => [BAR_LEVEL, 0.0, BAR_LEVEL],
        4 => [0.0, BAR_LEVEL, BAR_LEVEL],
        6 => [BAR_LEVEL; 3],
        _ => [0.0; 3],
    }
}

/// Segment index for an evenly split axis: the last segment eats the remainder.
fn column_index(column: u32, width: u32, count: u32) -> u32 {
    if width == 0 {
        return 0;
    }
    ((column * count) / width).min(count - 1)
}

/// Distance from the pixel centre to the frame centre.
fn pixel_radius(column: u32, row: u32, width: u32, height: u32) -> f32 {
    pixel_radius_squared(column, row, width, height).sqrt()
}

/// Squared distance from the pixel centre to the frame centre.
fn pixel_radius_squared(column: u32, row: u32, width: u32, height: u32) -> f32 {
    let centre_x = (width as f32 - 1.0) * 0.5;
    let centre_y = (height as f32 - 1.0) * 0.5;
    let dx = column as f32 - centre_x;
    let dy = row as f32 - centre_y;
    dx * dx + dy * dy
}

/// Calibration marks: a 10% grid, a centre cross, and four corner ticks.
fn mark_pixel(column: u32, row: u32, width: u32, height: u32) -> bool {
    if width == 0 || height == 0 {
        return false;
    }
    let step_x = (width.div_ceil(GRID_STEPS)).max(1);
    let step_y = (height.div_ceil(GRID_STEPS)).max(1);
    if column.is_multiple_of(step_x) || row.is_multiple_of(step_y) {
        return true;
    }
    let centre_x = (width - 1) / 2;
    let centre_y = (height - 1) / 2;
    let arm = (width.min(height) / GRID_STEPS).max(1);
    if column == centre_x && row.abs_diff(centre_y) <= arm {
        return true;
    }
    if row == centre_y && column.abs_diff(centre_x) <= arm {
        return true;
    }
    let near_left = column < arm;
    let near_right = column + arm >= width;
    let near_top = row < arm;
    let near_bottom = row + arm >= height;
    (near_top || near_bottom) && (near_left || near_right)
}

/// Action safe and title safe outlines.
fn safe_frame_pixel(column: u32, row: u32, width: u32, height: u32) -> bool {
    safe_frame_rect(column, row, width, height, SAFE_ACTION_SCALE)
        || safe_frame_rect(column, row, width, height, SAFE_TITLE_SCALE)
}

/// One safe rectangle outline, 1.5 px wide, measured in pixel centres.
fn safe_frame_rect(column: u32, row: u32, width: u32, height: u32, scale: f32) -> bool {
    if width < 2 || height < 2 {
        return false;
    }
    let inset_x = width as f32 * (1.0 - scale) * 0.5;
    let inset_y = height as f32 * (1.0 - scale) * 0.5;
    let centre_x = column as f32 + 0.5;
    let centre_y = row as f32 + 0.5;
    let left = inset_x;
    let right = width as f32 - inset_x;
    let top = inset_y;
    let bottom = height as f32 - inset_y;
    let on_vertical = (centre_x - left).abs() <= SAFE_LINE_HALF_PIXELS
        || (centre_x - right).abs() <= SAFE_LINE_HALF_PIXELS;
    let on_horizontal = (centre_y - top).abs() <= SAFE_LINE_HALF_PIXELS
        || (centre_y - bottom).abs() <= SAFE_LINE_HALF_PIXELS;
    let inside_y =
        centre_y >= top - SAFE_LINE_HALF_PIXELS && centre_y <= bottom + SAFE_LINE_HALF_PIXELS;
    let inside_x =
        centre_x >= left - SAFE_LINE_HALF_PIXELS && centre_x <= right + SAFE_LINE_HALF_PIXELS;
    (on_vertical && inside_y) || (on_horizontal && inside_x)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(pattern: TestSignalsPattern, intensity: f32) -> TestSignalsGrade {
        TestSignalsGrade::new(pattern, intensity, false, false).expect("grade is in range")
    }

    fn close(actual: f32, expected: f32) -> bool {
        (actual - expected).abs() <= 1.0e-6
    }

    #[test]
    fn menu_round_trips_through_one_based_indices() {
        for (index, pattern) in TestSignalsPattern::ALL.into_iter().enumerate() {
            assert_eq!(pattern.as_menu(), index as i64 + 1);
            assert_eq!(
                TestSignalsPattern::from_menu(pattern.as_menu()),
                Some(pattern)
            );
            assert_eq!(
                TestSignalsPattern::from_key(pattern.as_key()),
                Some(pattern)
            );
            assert_eq!(pattern.as_u32(), index as u32);
            assert!(!pattern.label().is_empty());
        }
        assert_eq!(TestSignalsPattern::ALL.len(), TEST_SIGNALS_PATTERN_COUNT);
        assert_eq!(TestSignalsPattern::from_menu(0), None);
        assert_eq!(
            TestSignalsPattern::from_menu(TEST_SIGNALS_PATTERN_COUNT as i64 + 1),
            None
        );
        assert_eq!(TestSignalsPattern::from_key("not_a_pattern"), None);
    }

    #[test]
    fn intensity_is_validated_before_it_reaches_the_kernel() {
        assert_eq!(
            TestSignalsGrade::new(TestSignalsPattern::Bars, f32::NAN, false, false),
            Err(TestSignalsError::NonFiniteIntensity)
        );
        assert_eq!(
            TestSignalsGrade::new(TestSignalsPattern::Bars, f32::INFINITY, false, false),
            Err(TestSignalsError::NonFiniteIntensity)
        );
        assert_eq!(
            TestSignalsGrade::new(TestSignalsPattern::Bars, -1.0, false, false),
            Err(TestSignalsError::IntensityOutOfRange)
        );
        assert_eq!(
            TestSignalsGrade::new(TestSignalsPattern::Bars, 100.5, false, false),
            Err(TestSignalsError::IntensityOutOfRange)
        );
        assert!(TestSignalsGrade::new(TestSignalsPattern::Bars, 0.0, false, false).is_ok());
        assert!(TestSignalsGrade::new(TestSignalsPattern::Bars, 100.0, false, false).is_ok());
    }

    #[test]
    fn bars_draw_smpte_seventy_five_percent_top_two_thirds() {
        let grade = grade(TestSignalsPattern::Bars, 100.0);
        let expected = [
            [BAR_LEVEL; 3],
            [BAR_LEVEL, BAR_LEVEL, 0.0],
            [0.0, BAR_LEVEL, BAR_LEVEL],
            [0.0, BAR_LEVEL, 0.0],
            [BAR_LEVEL, 0.0, BAR_LEVEL],
            [BAR_LEVEL, 0.0, 0.0],
            [0.0, 0.0, BAR_LEVEL],
        ];
        for row in 0..2 {
            for (column, expected) in expected.iter().enumerate() {
                let pixel = test_signals_pixel(column as u32, row, 7, 3, &grade);
                assert_eq!(&pixel[..3], expected, "column {column} row {row}");
                assert_eq!(pixel[3], 1.0);
            }
        }
    }

    #[test]
    fn bars_reverse_their_order_and_carry_pluge_in_the_lower_third() {
        let grade = grade(TestSignalsPattern::Bars, 100.0);
        // Fourteen columns: the left half of the lower third holds all seven
        // reverse bars, the bottom-right eighth starts at 14 * 7 / 8 = 12.
        let reverse = [
            [0.0, 0.0, BAR_LEVEL],
            [0.0; 3],
            [BAR_LEVEL, 0.0, BAR_LEVEL],
            [0.0; 3],
            [0.0, BAR_LEVEL, BAR_LEVEL],
            [0.0; 3],
            [BAR_LEVEL; 3],
        ];
        for (column, expected) in reverse.iter().enumerate() {
            let pixel = test_signals_pixel(column as u32, 2, 14, 3, &grade);
            assert_eq!(&pixel[..3], expected, "column {column}");
        }
        for column in 7..12 {
            assert_eq!(
                &test_signals_pixel(column, 2, 14, 3, &grade)[..3],
                &[0.0; 3],
                "the black field after the reverse bars, column {column}"
            );
        }
        // The two-column pluge strip starts at its darkest step.
        let pluge = test_signals_pixel(12, 2, 14, 3, &grade);
        assert!(close(pluge[0], -PLUGE_LEVEL));
        assert!(close(pluge[1], -PLUGE_LEVEL));
        assert!(close(pluge[2], -PLUGE_LEVEL));
        let neutral = test_signals_pixel(13, 2, 14, 3, &grade);
        assert!(close(neutral[0], 0.0));
    }

    #[test]
    fn pluge_steps_run_from_minus_four_percent_to_plus_four_percent() {
        let grade = grade(TestSignalsPattern::Bars, 100.0);
        // 24 columns put the pluge start at 24 * 7 / 8 = 21 with three strips.
        let steps: Vec<f32> =
            (21..24).map(|column| test_signals_pixel(column, 2, 24, 3, &grade)[0]).collect();
        assert!(close(steps[0], -PLUGE_LEVEL), "{steps:?}");
        assert!(close(steps[1], 0.0), "{steps:?}");
        assert!(close(steps[2], PLUGE_LEVEL), "{steps:?}");
    }

    #[test]
    fn grayscale_steps_through_eleven_linear_levels() {
        let grade = grade(TestSignalsPattern::Grayscale, 100.0);
        for column in 0..11 {
            let pixel = test_signals_pixel(column, 4, 11, 9, &grade);
            assert_eq!(pixel[0], column as f32 / 10.0, "column {column}");
            assert_eq!(pixel[0], pixel[1]);
            assert_eq!(pixel[1], pixel[2]);
        }
    }

    #[test]
    fn hdr_staircase_is_linear_and_unclamped() {
        let grade = grade(TestSignalsPattern::HdrStaircase, 100.0);
        for column in 0..16 {
            let pixel = test_signals_pixel(column, 4, 16, 9, &grade);
            assert!(
                close(pixel[0], column as f32 / 15.0),
                "column {column} produced {}",
                pixel[0]
            );
        }
        assert_eq!(test_signals_pixel(15, 0, 16, 1, &grade)[0], 1.0);
        assert_eq!(test_signals_pixel(7, 0, 16, 1, &grade)[0], 7.0 / 15.0);
    }

    #[test]
    fn checkerboard_inverts_at_every_cell_edge() {
        let grade = grade(TestSignalsPattern::Checkerboard, 100.0);
        assert_eq!(test_signals_pixel(0, 0, 64, 64, &grade)[0], 1.0);
        assert_eq!(test_signals_pixel(31, 31, 64, 64, &grade)[0], 1.0);
        assert_eq!(test_signals_pixel(32, 0, 64, 64, &grade)[0], 0.0);
        assert_eq!(test_signals_pixel(0, 32, 64, 64, &grade)[0], 0.0);
        assert_eq!(test_signals_pixel(32, 32, 64, 64, &grade)[0], 1.0);
    }

    #[test]
    fn concentric_rings_invert_at_every_ring_width() {
        let grade = grade(TestSignalsPattern::ConcentricRings, 100.0);
        // 129 px frame: the centre is exactly pixel 64 and every measured
        // offset is an exact floating point distance.
        assert_eq!(test_signals_pixel(64, 64, 129, 129, &grade)[0], 1.0);
        assert_eq!(test_signals_pixel(64, 79, 129, 129, &grade)[0], 1.0);
        assert_eq!(test_signals_pixel(64, 80, 129, 129, &grade)[0], 0.0);
        assert_eq!(test_signals_pixel(64, 96, 129, 129, &grade)[0], 1.0);
        assert_eq!(test_signals_pixel(48, 64, 129, 129, &grade)[0], 0.0);
    }

    #[test]
    fn zone_plate_peaks_at_the_centre_and_oscillates_outward() {
        let grade = grade(TestSignalsPattern::ZonePlate, 100.0);
        assert!(
            close(test_signals_pixel(64, 64, 129, 129, &grade)[0], 1.0),
            "centre must be the plate's peak"
        );
        let wave_number = std::f64::consts::TAU / (8.0 * 129.0);
        for offset in [1_u32, 7, 10, 33] {
            let expected = 0.5
                + 0.5
                    * (wave_number * f64::from(offset * offset) + std::f64::consts::FRAC_PI_2)
                        .sin();
            let actual = f64::from(test_signals_pixel(64 + offset, 64, 129, 129, &grade)[0]);
            assert!(
                (actual - expected).abs() <= 1.0e-5,
                "offset {offset}: {actual} vs {expected}"
            );
        }
    }

    #[test]
    fn intensity_scales_the_pattern_but_never_the_overlays() {
        let half = grade(TestSignalsPattern::Bars, 50.0);
        assert!(close(
            test_signals_pixel(0, 0, 7, 3, &half)[0],
            BAR_LEVEL * 0.5
        ));
        let dark = grade(TestSignalsPattern::Bars, 0.0);
        assert_eq!(test_signals_pixel(0, 0, 7, 3, &dark)[..3], [0.0; 3]);
        assert_eq!(test_signals_pixel(0, 0, 7, 3, &dark)[3], 1.0);
        let dark_marks = TestSignalsGrade::new(TestSignalsPattern::Grayscale, 0.0, true, true)
            .expect("grade is in range");
        // Column 0 is a grid line, and the safe-frame outline crosses the
        // centre row: both stay white on a black frame.
        assert_eq!(test_signals_pixel(0, 1, 100, 80, &dark_marks)[0], 1.0);
        assert_eq!(test_signals_pixel(3, 40, 100, 80, &dark_marks)[0], 1.0);
        assert_eq!(test_signals_pixel(51, 41, 100, 80, &dark_marks)[0], 0.0);
    }

    #[test]
    fn calibration_marks_appear_only_when_the_switch_is_on() {
        let off = TestSignalsGrade::new(TestSignalsPattern::Grayscale, 100.0, false, false)
            .expect("grade is in range");
        let on = TestSignalsGrade::new(TestSignalsPattern::Grayscale, 100.0, true, false)
            .expect("grade is in range");
        // 100x80: steps are 10 and 8 px; the cross is centred at (49, 39)
        // with an eight pixel arm, and the corner ticks are eight px long.
        for (column, row) in [(10, 3), (49, 47), (3, 2), (0, 0)] {
            let marked = test_signals_pixel(column, row, 100, 80, &on);
            let plain = test_signals_pixel(column, row, 100, 80, &off);
            assert_eq!(marked[0], 1.0, "({column}, {row}) should be marked");
            assert!(plain[0] < 1.0, "({column}, {row}) should stay pattern-only");
        }
        for (column, row) in [(11, 3), (51, 41), (23, 17)] {
            assert_eq!(
                test_signals_pixel(column, row, 100, 80, &on),
                test_signals_pixel(column, row, 100, 80, &off),
                "({column}, {row}) is not a mark"
            );
        }
    }

    #[test]
    fn safe_frames_trace_the_action_and_title_rectangles() {
        let off = TestSignalsGrade::new(TestSignalsPattern::Grayscale, 100.0, false, false)
            .expect("grade is in range");
        let on = TestSignalsGrade::new(TestSignalsPattern::Grayscale, 100.0, false, true)
            .expect("grade is in range");
        // 100x80: 93% insets the frame by 3.5 x 2.8 px, 90% by 5.0 x 4.0 px.
        for (column, row) in [(3, 40), (4, 40), (50, 2), (95, 40)] {
            assert_eq!(
                test_signals_pixel(column, row, 100, 80, &on)[0],
                1.0,
                "({column}, {row}) lies on a safe frame"
            );
        }
        assert!(test_signals_pixel(50, 40, 100, 80, &on)[0] < 1.0);
        assert_eq!(
            test_signals_pixel(50, 40, 100, 80, &on),
            test_signals_pixel(50, 40, 100, 80, &off)
        );
    }

    #[test]
    fn degenerate_frame_sizes_stay_total() {
        let bars = grade(TestSignalsPattern::Bars, 100.0);
        for pattern in TestSignalsPattern::ALL {
            let grade = grade(pattern, 100.0);
            let _ = test_signals_pixel(0, 0, 1, 1, &grade);
            let _ = test_signals_pixel(2, 1, 3, 2, &grade);
            assert!(test_signals_frame_f32(1, 1, &grade).len() == 1);
        }
        assert!(test_signals_frame_f32(0, 0, &bars).is_empty());
        assert!(test_signals_frame_f32(0, 5, &bars).is_empty());
        assert!(test_signals_frame_f32(5, 0, &bars).is_empty());
        assert_eq!(test_signals_pixel(0, 0, 0, 0, &bars), [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn frame_kernel_matches_the_per_pixel_kernel() {
        for pattern in TestSignalsPattern::ALL {
            for (labels, safe) in [(false, false), (true, true)] {
                let grade =
                    TestSignalsGrade::new(pattern, 63.0, labels, safe).expect("grade is in range");
                let frame = test_signals_frame_f32(13, 9, &grade);
                assert_eq!(frame.len(), 13 * 9);
                for row in 0..9_u32 {
                    for column in 0..13_u32 {
                        let expected = test_signals_pixel(column, row, 13, 9, &grade);
                        let actual = frame[(row * 13 + column) as usize];
                        assert_eq!(
                            actual.map(f32::to_bits),
                            expected.map(f32::to_bits),
                            "{pattern:?} at ({column}, {row})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn frame_kernel_overwrites_authored_input() {
        let grade = grade(TestSignalsPattern::Checkerboard, 100.0);
        let mut pixels = vec![[9.0, 9.0, 9.0, 9.0]; 20];
        let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
        assert!(
            apply_test_signals_f32_controlled(&mut pixels, 5, 4, &grade, &mut checkpoint)
                .expect("infallible")
        );
        assert_eq!(pixels, test_signals_frame_f32(5, 4, &grade));
    }

    #[test]
    fn frame_kernel_rejects_a_mismatched_extent() {
        let grade = grade(TestSignalsPattern::Bars, 100.0);
        let mut pixels = vec![[0.0; 4]; 4];
        let mut count = 0_u32;
        let mut checkpoint = || {
            count += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(
            !apply_test_signals_f32_controlled(&mut pixels, 3, 2, &grade, &mut checkpoint)
                .expect("infallible")
        );
        assert_eq!(count, 0, "invalid geometry must not run the checkpoints");
    }

    #[test]
    fn frame_kernel_runs_two_cooperative_checkpoints() {
        let grade = grade(TestSignalsPattern::Checkerboard, 100.0);
        let mut pixels = vec![[0.0; 4]; 4];
        let mut count = 0_u32;
        let mut checkpoint = || {
            count += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(
            apply_test_signals_f32_controlled(&mut pixels, 4, 1, &grade, &mut checkpoint)
                .expect("infallible")
        );
        assert_eq!(count, 2);
    }

    #[test]
    fn gpu_controls_pack_the_grade() {
        let grade = TestSignalsGrade::new(TestSignalsPattern::ZonePlate, 50.0, true, true)
            .expect("grade is in range");
        let (params, extra0) = grade.gpu_controls();
        assert_eq!(params, [5.0, 0.5, 1.0, 1.0]);
        assert_eq!(extra0, [0.0; 4]);
        let plain = TestSignalsGrade::new(TestSignalsPattern::Bars, 100.0, false, false)
            .expect("grade is in range");
        assert_eq!(plain.gpu_controls(), ([0.0, 1.0, 0.0, 0.0], [0.0; 4]));
    }

    #[test]
    fn a_generator_is_never_an_identity() {
        let dark = grade(TestSignalsPattern::Bars, 0.0);
        assert!(!dark.is_identity());
        assert!(!grade(TestSignalsPattern::Bars, 100.0).is_identity());
    }
}
