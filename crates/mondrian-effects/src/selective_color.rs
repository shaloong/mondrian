//! The selective colour effect from the built-in effect list.
//!
//! The list authors nine colour ranges — reds, yellows, greens, cyans, blues
//! and magentas, plus whites, neutrals and blacks — each with a cyan, magenta,
//! yellow and black amount in `-100..=100`, one colour range that also receives
//! four unprefixed trim amounts, and a `method` switch that chooses relative or
//! absolute ink application. That is forty sliders and two menus; forty-two
//! parameters do not fit the shared seven-cell spatial uniform, so the renderer
//! runs this effect through a dedicated 160-byte uniform and a dedicated
//! pipeline.
//!
//! # Documented reading
//!
//! The list names the effect but does not define the maths, so this module fixes
//! one reading and documents it here. Straight alpha, float32:
//!
//! 1. With `m = max(r, g, b)`, `mn = min(r, g, b)` and `chroma = m - mn`, the
//!    six hue ranges carry the tent weights `gate * max(0, 1 - |wrap180(h - c)|
//!    / 60)` for centres `0°, 60°, 120°, 180°, 240°, 300°`; the tent weights sum
//!    to one for every hue, so a hue between two centres splits between the two
//!    neighbouring ranges and nowhere else.
//! 2. `gate = smoothstep((chroma - 0.02) / 0.08)` fades the hue ranges out on
//!    near-grey samples. The three tone ranges share what the gate leaves:
//!    `white = smoothstep((luma - 0.5) / 0.5)`,
//!    `black = smoothstep((0.5 - luma) / 0.5)`, `neutral = 1 - white - black`,
//!    so `Σw = 1` for every sample and a sample always reaches exactly the
//!    ranges whose ink it should move.
//! 3. Every authored amount becomes `value / 100`, and the four trim amounts are
//!    folded into the selected range only:
//!    `folded[r][ch] = clamp(d[r][ch] + (r == selected ? trim[ch] : 0), -1, 1)`.
//!    The clamp is what keeps a trim and a colour amount from stacking past a
//!    full ink unit.
//! 4. `D[ch] = Σ w[r] * folded[r][ch]`, the weighted ink shift for cyan,
//!    magenta, yellow and black.
//! 5. The source sample resolves to ink through `safe = max(m, 1e-6)`,
//!    `c = (safe - r) / safe`, `mg = (safe - g) / safe`, `yl = (safe - b) /
//!    safe`, `k = 1 - safe`. In relative mode a channel moves a fraction of its
//!    remaining distance — `x + Δ(1 - x)` for `Δ >= 0` and `x(1 + Δ)` for
//!    `Δ < 0` — and in absolute mode it moves by `x + Δ`. The black channel
//!    follows the same rule, so the black slider darkens through `k`.
//! 6. The graded ink rebuilds as `shade = 1 - k'`, `(shade(1 - c'), shade(1 -
//!    mg'), shade(1 - yl'))`, clamped once per output channel. The clamp happens
//!    once, on the final value; the ink values themselves are never clamped, so
//!    an over-range ink amount cannot distort a neighbouring channel through an
//!    early clamp.
//! 7. Alpha is passed through, and a pixel with `alpha <= 0` is returned
//!    unchanged, because the compositor short circuits zero coverage and
//!    grading such a pixel would only make the CPU and the GPU disagree about a
//!    pixel that is never visible.
//!
//! A grade counts as an identity while all forty sliders are exactly zero; the
//! method and the selected colour range only choose where a non-zero slider
//! applies, so they do not make an unconfigured effect non-identity, and the
//! graph builder elides the operation in that case.
//!
//! # Uniform packing
//!
//! [`SelectiveColorGrade::gpu_controls`] returns the folded ink table as nine
//! `vec4`s plus the method menu value, so the shader can weight the very same
//! numbers the CPU kernel uses. The shader repeats the weight and rebuild
//! expressions in the same order, so the two sides agree on the final sample.

/// Lowest authored selective-colour amount, in percent.
pub const SELECTIVE_COLOR_SLIDER_MIN: f32 = -100.0;

/// Highest authored selective-colour amount, in percent.
pub const SELECTIVE_COLOR_SLIDER_MAX: f32 = 100.0;

/// Chroma at which the hue ranges start to carry weight.
pub const CHROMA_GATE_START: f32 = 0.02;

/// Chroma at which the hue ranges carry full weight.
pub const CHROMA_GATE_END: f32 = 0.10;

/// Half width of one hue range's tent, in degrees.
pub const TENT_HALF_WIDTH_DEGREES: f32 = 60.0;

/// Luma that separates the white and the black tone ranges.
pub const LUMA_PIVOT: f32 = 0.5;

/// Largest channel value the ink round trip may divide by.
pub const MIN_MAX_CHANNEL: f32 = 1.0e-6;

/// Number of colour ranges the effect authors.
pub const RANGE_COUNT: usize = 9;

/// Number of ink channels per range.
pub const CHANNEL_COUNT: usize = 4;

/// Number of pixels between two execution checkpoints.
pub(crate) const CHECKPOINT_STRIDE: usize = 4_096;

/// Rec.709 luma coefficients, shared verbatim with the spatial WGSL shader.
const REC_709_LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// Why a selective-colour grade could not be resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectiveColorError {
    /// The method menu value is not 1 (relative) or 2 (absolute).
    Method,
    /// The colour range menu value is not within `1..=9`.
    Range,
    /// A cyan amount is not finite or falls outside `-100..=100`.
    Cyan,
    /// A magenta amount is not finite or falls outside `-100..=100`.
    Magenta,
    /// A yellow amount is not finite or falls outside `-100..=100`.
    Yellow,
    /// A black amount is not finite or falls outside `-100..=100`.
    Black,
}

impl std::fmt::Display for SelectiveColorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Method => write!(
                formatter,
                "selective color method must be 1 (relative) or 2 (absolute)"
            ),
            Self::Range => write!(formatter, "selective color range must be within 1..=9"),
            Self::Cyan => write!(formatter, "selective color cyan must be within -100..=100"),
            Self::Magenta => write!(
                formatter,
                "selective color magenta must be within -100..=100"
            ),
            Self::Yellow => write!(
                formatter,
                "selective color yellow must be within -100..=100"
            ),
            Self::Black => write!(formatter, "selective color black must be within -100..=100"),
        }
    }
}

impl std::error::Error for SelectiveColorError {}

/// How a selective-colour amount moves an ink channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectiveColorMethod {
    /// Move a fraction of the distance that remains to the channel limit.
    Relative,
    /// Add the amount to the channel without regard to what remains.
    Absolute,
}

impl SelectiveColorMethod {
    /// Every method, in menu order.
    pub const ALL: [Self; 2] = [Self::Relative, Self::Absolute];

    /// The stable key used by the effect list and the project file.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Relative => "relative",
            Self::Absolute => "absolute",
        }
    }

    /// Resolves the stable key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|method| method.as_key() == key)
    }

    /// The one-based menu value used by the uniform payload.
    #[must_use]
    pub const fn as_menu(self) -> u32 {
        match self {
            Self::Relative => 1,
            Self::Absolute => 2,
        }
    }

    /// Resolves the one-based menu value.
    #[must_use]
    pub const fn from_menu(menu: u32) -> Option<Self> {
        match menu {
            1 => Some(Self::Relative),
            2 => Some(Self::Absolute),
            _ => None,
        }
    }

    /// The zero-based value used by the parameter menu.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Relative => 0,
            Self::Absolute => 1,
        }
    }

    /// The Chinese label shown by the effect list.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Relative => "相对",
            Self::Absolute => "绝对",
        }
    }
}

/// One of the nine colour ranges the effect authors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectiveColorRange {
    /// The red hue range.
    Reds,
    /// The yellow hue range.
    Yellows,
    /// The green hue range.
    Greens,
    /// The cyan hue range.
    Cyans,
    /// The blue hue range.
    Blues,
    /// The magenta hue range.
    Magentas,
    /// The white tone range.
    Whites,
    /// The neutral tone range.
    Neutrals,
    /// The black tone range.
    Blacks,
}

impl SelectiveColorRange {
    /// Every range, in menu order.
    pub const ALL: [Self; RANGE_COUNT] = [
        Self::Reds,
        Self::Yellows,
        Self::Greens,
        Self::Cyans,
        Self::Blues,
        Self::Magentas,
        Self::Whites,
        Self::Neutrals,
        Self::Blacks,
    ];

    /// The stable key used by the effect list and the project file.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Reds => "reds",
            Self::Yellows => "yellows",
            Self::Greens => "greens",
            Self::Cyans => "cyans",
            Self::Blues => "blues",
            Self::Magentas => "magentas",
            Self::Whites => "whites",
            Self::Neutrals => "neutrals",
            Self::Blacks => "blacks",
        }
    }

    /// Resolves the stable key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|range| range.as_key() == key)
    }

    /// The one-based menu value used by the uniform payload.
    #[must_use]
    pub const fn as_menu(self) -> u32 {
        self.index() as u32 + 1
    }

    /// Resolves the one-based menu value.
    #[must_use]
    pub const fn from_menu(menu: u32) -> Option<Self> {
        match menu {
            1 => Some(Self::Reds),
            2 => Some(Self::Yellows),
            3 => Some(Self::Greens),
            4 => Some(Self::Cyans),
            5 => Some(Self::Blues),
            6 => Some(Self::Magentas),
            7 => Some(Self::Whites),
            8 => Some(Self::Neutrals),
            9 => Some(Self::Blacks),
            _ => None,
        }
    }

    /// The zero-based index into the weight and ink tables.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Reds => 0,
            Self::Yellows => 1,
            Self::Greens => 2,
            Self::Cyans => 3,
            Self::Blues => 4,
            Self::Magentas => 5,
            Self::Whites => 6,
            Self::Neutrals => 7,
            Self::Blacks => 8,
        }
    }

    /// The Chinese label shown by the effect list.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Reds => "红色",
            Self::Yellows => "黄色",
            Self::Greens => "绿色",
            Self::Cyans => "青色",
            Self::Blues => "蓝色",
            Self::Magentas => "洋红",
            Self::Whites => "白色",
            Self::Neutrals => "中性色",
            Self::Blacks => "黑色",
        }
    }
}

/// A resolved selective-colour grade.
///
/// The authored amounts stay in their authored `-100..=100` domain in the
/// effect list; this type stores them divided by one hundred, which is the ink
/// domain the kernel and the uniform payload use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectiveColorGrade {
    method: SelectiveColorMethod,
    range: SelectiveColorRange,
    trim: [f32; CHANNEL_COUNT],
    deltas: [[f32; CHANNEL_COUNT]; RANGE_COUNT],
}

impl SelectiveColorGrade {
    /// Resolves the authored controls, rejecting menu values and amounts that
    /// fall outside their declared domain.
    ///
    /// # Errors
    ///
    /// * [`SelectiveColorError::Method`] when `method` is not 1 or 2.
    /// * [`SelectiveColorError::Range`] when `range` is not within `1..=9`.
    /// * [`SelectiveColorError::Cyan`], [`SelectiveColorError::Magenta`],
    ///   [`SelectiveColorError::Yellow`] or [`SelectiveColorError::Black`] when
    ///   one of the forty amounts is not finite or falls outside `-100..=100`.
    pub fn new(
        method: u32,
        range: u32,
        trim: [f32; CHANNEL_COUNT],
        deltas: [[f32; CHANNEL_COUNT]; RANGE_COUNT],
    ) -> Result<Self, SelectiveColorError> {
        let method = SelectiveColorMethod::from_menu(method).ok_or(SelectiveColorError::Method)?;
        let range = SelectiveColorRange::from_menu(range).ok_or(SelectiveColorError::Range)?;
        for (value, error) in trim.into_iter().zip(CHANNEL_ERRORS) {
            validate_slider(value, error)?;
        }
        for row in &deltas {
            for (value, error) in row.iter().zip(CHANNEL_ERRORS) {
                validate_slider(*value, error)?;
            }
        }
        let mut deltas = deltas;
        for row in &mut deltas {
            for value in row {
                *value /= 100.0;
            }
        }
        let mut trim = trim;
        for value in &mut trim {
            *value /= 100.0;
        }
        Ok(Self { method, range, trim, deltas })
    }

    /// The resolved application method.
    #[must_use]
    pub const fn method(self) -> SelectiveColorMethod {
        self.method
    }

    /// The colour range the trim amounts apply to.
    #[must_use]
    pub const fn range(self) -> SelectiveColorRange {
        self.range
    }

    /// The four trim amounts, already divided by one hundred.
    #[must_use]
    pub const fn trim(&self) -> [f32; CHANNEL_COUNT] {
        self.trim
    }

    /// The nine authored ink rows, already divided by one hundred.
    #[must_use]
    pub const fn deltas(&self) -> [[f32; CHANNEL_COUNT]; RANGE_COUNT] {
        self.deltas
    }

    /// Whether all forty sliders are exactly zero.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.trim.iter().all(|value| *value == 0.0)
            && self.deltas.iter().flatten().all(|value| *value == 0.0)
    }

    /// The ink table with the trim amounts folded into the selected range.
    ///
    /// Only the selected row can leave the authored range, so the clamp keeps a
    /// trim plus a colour amount from stacking past one full ink unit.
    #[must_use]
    pub fn folded_deltas(&self) -> [[f32; CHANNEL_COUNT]; RANGE_COUNT] {
        let mut folded = self.deltas;
        let selected = self.range.index();
        for (channel, value) in folded[selected].iter_mut().enumerate() {
            *value = (*value + self.trim[channel]).clamp(-1.0, 1.0);
        }
        folded
    }

    /// The uniform payload for the dedicated spatial shader.
    ///
    /// Returns the folded ink table and the one-based method menu value. The
    /// shader weights the same table with the same expressions, so the two
    /// sides resolve the same ink shift.
    #[must_use]
    pub fn gpu_controls(&self) -> ([[f32; CHANNEL_COUNT]; RANGE_COUNT], u32) {
        (self.folded_deltas(), self.method.as_menu())
    }

    /// Applies the grade to one straight-alpha pixel.
    ///
    /// An identity grade returns the pixel untouched: the CMYK round trip is
    /// exact in the mathematical model but not in float32, and a grade that
    /// authors nothing must not perturb the frame.
    #[must_use]
    pub fn apply(&self, pixel: [f32; 4]) -> [f32; 4] {
        if self.is_identity() {
            return pixel;
        }
        self.apply_with(&self.folded_deltas(), pixel)
    }

    /// Applies a pre-folded ink table to one straight-alpha pixel.
    ///
    /// The kernel hoists the fold out of the pixel loop and calls this; the
    /// expression order matches the spatial shader statement for statement.
    fn apply_with(
        &self,
        folded: &[[f32; CHANNEL_COUNT]; RANGE_COUNT],
        pixel: [f32; 4],
    ) -> [f32; 4] {
        if pixel[3] <= 0.0 {
            return pixel;
        }
        let rgb = [pixel[0], pixel[1], pixel[2]];
        let weights = range_weights(rgb);
        let mut delta = [0.0f32; CHANNEL_COUNT];
        for (range, weight) in weights.iter().enumerate() {
            for (channel, delta) in delta.iter_mut().enumerate() {
                *delta += weight * folded[range][channel];
            }
        }
        let max_channel = rgb[0].max(rgb[1]).max(rgb[2]);
        let safe = max_channel.max(MIN_MAX_CHANNEL);
        let cyan = (safe - rgb[0]) / safe;
        let magenta = (safe - rgb[1]) / safe;
        let yellow = (safe - rgb[2]) / safe;
        let black = 1.0 - safe;
        let (cyan, magenta, yellow, black) = match self.method {
            SelectiveColorMethod::Relative => (
                relative(cyan, delta[0]),
                relative(magenta, delta[1]),
                relative(yellow, delta[2]),
                relative(black, delta[3]),
            ),
            SelectiveColorMethod::Absolute => (
                cyan + delta[0],
                magenta + delta[1],
                yellow + delta[2],
                black + delta[3],
            ),
        };
        let shade = 1.0 - black;
        [
            (shade * (1.0 - cyan)).clamp(0.0, 1.0),
            (shade * (1.0 - magenta)).clamp(0.0, 1.0),
            (shade * (1.0 - yellow)).clamp(0.0, 1.0),
            pixel[3],
        ]
    }
}

/// The channel error variants, in ink channel order.
const CHANNEL_ERRORS: [SelectiveColorError; CHANNEL_COUNT] = [
    SelectiveColorError::Cyan,
    SelectiveColorError::Magenta,
    SelectiveColorError::Yellow,
    SelectiveColorError::Black,
];

/// Rejects one authored amount that is not finite or outside `-100..=100`.
fn validate_slider(value: f32, error: SelectiveColorError) -> Result<(), SelectiveColorError> {
    if !value.is_finite()
        || !(SELECTIVE_COLOR_SLIDER_MIN..=SELECTIVE_COLOR_SLIDER_MAX).contains(&value)
    {
        return Err(error);
    }
    Ok(())
}

/// Smoothstep over `0..=1`, shared verbatim with the spatial shader.
fn smoothstep01(t: f32) -> f32 {
    let c = t.clamp(0.0, 1.0);
    c * c * (3.0 - 2.0 * c)
}

/// Hue in degrees for a sample whose maximum channel and chroma are known.
///
/// The three branches use the same expression order as the shader; a grey
/// sample has no hue and resolves to zero.
fn hue_degrees(rgb: [f32; 3], max_channel: f32, chroma: f32) -> f32 {
    if chroma <= 0.0 {
        return 0.0;
    }
    let mut hue = if max_channel == rgb[0] {
        60.0 * ((rgb[1] - rgb[2]) / chroma)
    } else if max_channel == rgb[1] {
        60.0 * ((rgb[2] - rgb[0]) / chroma + 2.0)
    } else {
        60.0 * ((rgb[0] - rgb[1]) / chroma + 4.0)
    };
    if hue < 0.0 {
        hue += 360.0;
    }
    hue
}

/// One hue range's tent weight for a hue and a centre.
fn tent(hue: f32, centre: f32) -> f32 {
    let mut distance = hue - centre;
    distance -= 360.0 * (distance / 360.0).round();
    (1.0 - distance.abs() / TENT_HALF_WIDTH_DEGREES).max(0.0)
}

/// Moves `value` by a fraction of its remaining distance, or toward zero.
fn relative(value: f32, delta: f32) -> f32 {
    if delta >= 0.0 {
        value + delta * (1.0 - value)
    } else {
        value * (1.0 + delta)
    }
}

/// The nine range weights for one straight-alpha RGB sample.
///
/// The expression order matches the shader so the CPU and the GPU agree on the
/// final sample.
fn range_weights(rgb: [f32; 3]) -> [f32; RANGE_COUNT] {
    let max_channel = rgb[0].max(rgb[1]).max(rgb[2]);
    let min_channel = rgb[0].min(rgb[1]).min(rgb[2]);
    let chroma = max_channel - min_channel;
    let luma = REC_709_LUMA[0] * rgb[0] + REC_709_LUMA[1] * rgb[1] + REC_709_LUMA[2] * rgb[2];
    let gate = smoothstep01((chroma - CHROMA_GATE_START) / (CHROMA_GATE_END - CHROMA_GATE_START));
    let hue = hue_degrees(rgb, max_channel, chroma);
    let white = smoothstep01((luma - LUMA_PIVOT) / LUMA_PIVOT);
    let black = smoothstep01((LUMA_PIVOT - luma) / LUMA_PIVOT);
    let mut weights = [0.0f32; RANGE_COUNT];
    for (index, centre) in [0.0, 60.0, 120.0, 180.0, 240.0, 300.0].into_iter().enumerate() {
        weights[index] = gate * tent(hue, centre);
    }
    weights[6] = (1.0 - gate) * white;
    weights[7] = (1.0 - gate) * (1.0 - white - black);
    weights[8] = (1.0 - gate) * black;
    weights
}

/// Applies the effect to every pixel of one frame, in place.
///
/// Returns whether the grade ran. The graph builder elides an identity grade, so
/// a compiled graph that reaches this kernel always maps the frame.
pub fn selective_color_frame_f32(pixels: &mut [[f32; 4]], grade: &SelectiveColorGrade) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_selective_color_f32_controlled(pixels, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the effect to a flat frame, honouring the execution checkpoint.
///
/// # Errors
///
/// Propagates whatever the checkpoint returns.
pub(crate) fn apply_selective_color_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    grade: &SelectiveColorGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    // The flat entry point has no geometry, so it cannot split the frame into
    // row bands; it processes 4096-pixel chunks purely for cancellation
    // checkpoints, like the other point kernels.
    if grade.is_identity() {
        return Ok(true);
    }
    let folded = grade.folded_deltas();
    for chunk in pixels.chunks_mut(CHECKPOINT_STRIDE) {
        checkpoint()?;
        for pixel in chunk.iter_mut() {
            *pixel = grade.apply_with(&folded, *pixel);
        }
    }
    checkpoint()?;
    Ok(true)
}

/// Applies the effect to a frame region, honouring the execution checkpoint.
///
/// Rows are processed in chunks of at most [`CHECKPOINT_STRIDE`] pixels with a
/// cooperative checkpoint before each chunk and once after the last one; an
/// error aborts immediately and leaves the remaining pixels untouched.
///
/// The returned `bool` reports whether the buffer described the extent; the
/// identity also reports `true` (there is simply nothing to write).
///
/// # Errors
///
/// Propagates whatever the checkpoint returns.
pub(crate) fn apply_selective_color_f32_region_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &SelectiveColorGrade,
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
    let folded = grade.folded_deltas();
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
            |_, band| {
                for pixel in band.iter_mut() {
                    *pixel = grade.apply_with(&folded, *pixel);
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

    const ZERO_TRIM: [f32; CHANNEL_COUNT] = [0.0; CHANNEL_COUNT];
    const ZERO_DELTAS: [[f32; CHANNEL_COUNT]; RANGE_COUNT] = [[0.0; CHANNEL_COUNT]; RANGE_COUNT];

    fn build_grade(
        method: SelectiveColorMethod,
        range: SelectiveColorRange,
        trim: [f32; CHANNEL_COUNT],
        deltas: [[f32; CHANNEL_COUNT]; RANGE_COUNT],
    ) -> SelectiveColorGrade {
        SelectiveColorGrade::new(method.as_menu(), range.as_menu(), trim, deltas).expect("grade")
    }

    /// A grade that moves one ink channel of one range.
    fn single(
        method: SelectiveColorMethod,
        range: SelectiveColorRange,
        channel: usize,
        amount: f32,
    ) -> SelectiveColorGrade {
        let mut deltas = ZERO_DELTAS;
        deltas[range.index()][channel] = amount;
        build_grade(method, range, ZERO_TRIM, deltas)
    }

    fn close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= 1.0e-6,
            "expected {expected}, got {actual}"
        );
    }

    fn close_pixel(actual: [f32; 4], expected: [f32; 4]) {
        for (index, expected) in expected.into_iter().enumerate() {
            close(actual[index], expected);
        }
    }

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        (0..width * height)
            .map(|index| {
                let channel = |offset: usize| ((index * (offset + 3)) % 17) as f32 / 16.0;
                [
                    channel(0),
                    channel(1),
                    channel(2),
                    if index % 5 == 0 { 0.0 } else { 1.0 },
                ]
            })
            .collect()
    }

    #[test]
    fn zero_deltas_are_an_identity() {
        let zero = build_grade(
            SelectiveColorMethod::Relative,
            SelectiveColorRange::Reds,
            ZERO_TRIM,
            ZERO_DELTAS,
        );
        assert!(zero.is_identity());
        // The method and the colour range only choose where a non-zero slider
        // applies, so another pair with the same zero table is an identity too.
        assert!(build_grade(
            SelectiveColorMethod::Absolute,
            SelectiveColorRange::Blacks,
            ZERO_TRIM,
            ZERO_DELTAS,
        )
        .is_identity());
        for pixel in [
            [0.0, 0.0, 0.0, 1.0],
            [0.25, 0.5, 0.75, 1.0],
            [1.0, 1.0, 1.0, 0.5],
        ] {
            for (actual, expected) in zero.apply(pixel).into_iter().zip(pixel) {
                assert_eq!(actual.to_bits(), expected.to_bits());
            }
        }
        let mut pixels = frame(5, 4);
        let untouched = pixels.clone();
        assert!(selective_color_frame_f32(&mut pixels, &zero));
        assert_eq!(pixels, untouched);
    }

    #[test]
    fn relative_mode_moves_a_fraction_of_the_remaining_distance() {
        // c = 0.5 and delta = 0.4 leaves 0.4 * (1 - 0.5) = 0.2 behind, so the
        // channel lands on 0.7.
        close(relative(0.5, 0.4), 0.7);
        // A sample with c = (1 - 0.5) / 1 = 0.5 reaches the cyan range through
        // its hue of 180 degrees, so the pixel-level expectation matches.
        let grade = single(
            SelectiveColorMethod::Relative,
            SelectiveColorRange::Cyans,
            0,
            40.0,
        );
        close_pixel(grade.apply([0.5, 1.0, 1.0, 1.0]), [0.3, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn a_negative_relative_delta_scales_toward_zero() {
        // A negative delta has no remainder to travel, so it scales the ink
        // toward zero: 0.8 * (1 - 0.5) = 0.4.
        close(relative(0.8, -0.5), 0.4);
        let grade = single(
            SelectiveColorMethod::Relative,
            SelectiveColorRange::Cyans,
            0,
            -50.0,
        );
        close_pixel(grade.apply([0.2, 1.0, 1.0, 1.0]), [0.6, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn absolute_mode_adds_a_fixed_ink_amount() {
        let grade = single(
            SelectiveColorMethod::Absolute,
            SelectiveColorRange::Cyans,
            0,
            30.0,
        );
        // The source cyan is (1 - 0.8) / 1 = 0.2; absolute mode adds a fixed
        // 0.3, so the channel lands on 0.5.
        close_pixel(grade.apply([0.8, 1.0, 1.0, 1.0]), [0.5, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn the_black_slider_darkens_through_the_k_channel() {
        // White sits entirely in the white tone range, whose black slider adds
        // to k: k = 0 becomes 0.5, so the rebuilt colour is 0.5 * 1.
        let grade = single(
            SelectiveColorMethod::Absolute,
            SelectiveColorRange::Whites,
            3,
            50.0,
        );
        close_pixel(grade.apply([1.0, 1.0, 1.0, 1.0]), [0.5, 0.5, 0.5, 1.0]);
    }

    #[test]
    fn a_pure_red_pixel_gets_all_weight_on_the_reds_range() {
        let weights = range_weights([1.0, 0.0, 0.0]);
        close(weights[0], 1.0);
        for (index, weight) in weights.into_iter().enumerate().skip(1) {
            assert_eq!(weight, 0.0, "weight {index} must be zero for pure red");
        }
        // A red-only amount moves pure red and leaves a primary green alone.
        let grade = single(
            SelectiveColorMethod::Absolute,
            SelectiveColorRange::Reds,
            0,
            100.0,
        );
        close_pixel(grade.apply([1.0, 0.0, 0.0, 1.0]), [0.0, 0.0, 0.0, 1.0]);
        close_pixel(grade.apply([0.0, 1.0, 0.0, 1.0]), [0.0, 1.0, 0.0, 1.0]);
    }

    #[test]
    fn an_orange_pixel_splits_between_reds_and_yellows() {
        // (1, 0.5, 0) has a hue of 30 degrees, exactly halfway between the red
        // and the yellow centres.
        let weights = range_weights([1.0, 0.5, 0.0]);
        close(weights[0], 0.5);
        close(weights[1], 0.5);
        for (index, weight) in weights.into_iter().enumerate().skip(2) {
            assert_eq!(weight, 0.0, "weight {index} must be zero for orange");
        }
    }

    #[test]
    fn a_grey_pixel_reaches_only_the_tone_ranges() {
        let weights = range_weights([0.5, 0.5, 0.5]);
        for (index, weight) in weights.into_iter().enumerate().take(6) {
            assert_eq!(weight, 0.0, "hue weight {index} must be zero for grey");
        }
        close(weights[6], 0.0);
        close(weights[7], 1.0);
        close(weights[8], 0.0);
    }

    #[test]
    fn the_nine_weights_sum_to_one() {
        for rgb in [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 1.0, 0.0],
            [0.5, 0.5, 0.5],
            [1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0],
            [0.2, 0.6, 0.9],
            [1.0, 0.5, 0.0],
        ] {
            let total: f32 = range_weights(rgb).into_iter().sum();
            close(total, 1.0);
        }
    }

    #[test]
    fn a_uniform_delta_across_all_ranges_is_a_uniform_shift() {
        // Mid-grey reaches only the neutral range, but the same delta sits on
        // every row, so the weighted sum is the delta itself on each channel.
        let mut deltas = ZERO_DELTAS;
        for row in &mut deltas {
            row[0] = 30.0;
            row[1] = 30.0;
            row[2] = 30.0;
        }
        let grade = build_grade(
            SelectiveColorMethod::Absolute,
            SelectiveColorRange::Reds,
            ZERO_TRIM,
            deltas,
        );
        // k = 0.5 and c = mg = yl = 0, so the rebuilt colour is
        // 0.5 * (1 - 0.3).
        close_pixel(grade.apply([0.5, 0.5, 0.5, 1.0]), [0.35, 0.35, 0.35, 1.0]);
    }

    #[test]
    fn the_trim_lands_on_the_selected_range() {
        let grade = build_grade(
            SelectiveColorMethod::Relative,
            SelectiveColorRange::Reds,
            [50.0, 0.0, 0.0, 0.0],
            ZERO_DELTAS,
        );
        // Pure red carries the whole trim, so its cyan ink moves from 0 to 0.5.
        close_pixel(grade.apply([1.0, 0.0, 0.0, 1.0]), [0.5, 0.0, 0.0, 1.0]);
        // Pure blue reaches the blue row, which carries no trim.
        close_pixel(grade.apply([0.0, 0.0, 1.0, 1.0]), [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn the_folded_delta_is_clamped() {
        // A full colour amount plus a full trim is two ink units, which the
        // fold clamps to one.
        let mut deltas = ZERO_DELTAS;
        deltas[SelectiveColorRange::Reds.index()] = [100.0; CHANNEL_COUNT];
        let grade = build_grade(
            SelectiveColorMethod::Absolute,
            SelectiveColorRange::Reds,
            [100.0; CHANNEL_COUNT],
            deltas,
        );
        for channel in 0..CHANNEL_COUNT {
            close(
                grade.folded_deltas()[SelectiveColorRange::Reds.index()][channel],
                1.0,
            );
        }
        let mut deltas = ZERO_DELTAS;
        deltas[SelectiveColorRange::Reds.index()] = [-100.0; CHANNEL_COUNT];
        let grade = build_grade(
            SelectiveColorMethod::Absolute,
            SelectiveColorRange::Reds,
            [-100.0; CHANNEL_COUNT],
            deltas,
        );
        for channel in 0..CHANNEL_COUNT {
            close(
                grade.folded_deltas()[SelectiveColorRange::Reds.index()][channel],
                -1.0,
            );
        }
    }

    #[test]
    fn relative_mode_never_clips() {
        // A relative delta of one reaches the limit but cannot pass it, and a
        // delta of minus one reaches zero but cannot pass it.
        close(relative(0.9, 1.0), 1.0);
        close(relative(0.9, -1.0), 0.0);
        close(relative(0.0, 1.0), 1.0);
        close(relative(1.0, -1.0), 0.0);
        let grade = single(
            SelectiveColorMethod::Relative,
            SelectiveColorRange::Reds,
            0,
            100.0,
        );
        for step in 0..=100 {
            let value = step as f32 / 100.0;
            let output = grade.apply([value, 0.0, 0.0, 1.0]);
            for channel in &output[..3] {
                assert!(
                    (0.0..=1.0).contains(channel),
                    "channel {channel} left 0..=1"
                );
            }
        }
    }

    #[test]
    fn zero_coverage_pixels_are_left_untouched() {
        let grade = single(
            SelectiveColorMethod::Absolute,
            SelectiveColorRange::Reds,
            0,
            100.0,
        );
        for alpha in [0.0, -0.5] {
            let pixel = [1.0, 0.0, 0.0, alpha];
            for (actual, expected) in grade.apply(pixel).into_iter().zip(pixel) {
                assert_eq!(actual.to_bits(), expected.to_bits());
            }
        }
        // A saturated sample carries no tone weight, so a tone-only amount
        // cannot move it.
        let grade = single(
            SelectiveColorMethod::Absolute,
            SelectiveColorRange::Whites,
            3,
            100.0,
        );
        close_pixel(grade.apply([1.0, 0.0, 0.0, 1.0]), [1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn the_kernel_is_deterministic() {
        let grade = build_grade(
            SelectiveColorMethod::Relative,
            SelectiveColorRange::Cyans,
            [10.0, -20.0, 30.0, -40.0],
            {
                let mut deltas = ZERO_DELTAS;
                deltas[0] = [10.0, 20.0, -30.0, 40.0];
                deltas[4] = [-50.0, 25.0, 0.0, 10.0];
                deltas[7] = [12.0, 0.0, -12.0, 5.0];
                deltas
            },
        );
        let source = frame(7, 3);
        let mut first = source.clone();
        let mut second = source.clone();
        assert!(selective_color_frame_f32(&mut first, &grade));
        assert!(selective_color_frame_f32(&mut second, &grade));
        for (left, right) in first.iter().zip(&second) {
            for channel in 0..4 {
                assert_eq!(left[channel].to_bits(), right[channel].to_bits());
            }
        }
        // The frame kernel and the per-pixel application agree bit for bit.
        for (pixel, source) in first.iter().zip(&source) {
            let expected = grade.apply(*source);
            for channel in 0..4 {
                assert_eq!(pixel[channel].to_bits(), expected[channel].to_bits());
            }
        }
    }

    #[test]
    fn gpu_controls_packs_the_folded_table() {
        let mut deltas = ZERO_DELTAS;
        deltas[2][1] = 50.0;
        let grade = build_grade(
            SelectiveColorMethod::Absolute,
            SelectiveColorRange::Magentas,
            [10.0, -20.0, 30.0, -40.0],
            deltas,
        );
        let (folded, method) = grade.gpu_controls();
        assert_eq!(method, 2);
        assert_eq!(folded, grade.folded_deltas());
        close(folded[2][1], 0.5);
        close(folded[5][0], 0.1);
        close(folded[5][1], -0.2);
        close(folded[5][2], 0.3);
        close(folded[5][3], -0.4);
        close(folded[0][0], 0.0);
    }

    #[test]
    fn menu_and_key_round_trips() {
        for method in SelectiveColorMethod::ALL {
            assert_eq!(
                SelectiveColorMethod::from_key(method.as_key()),
                Some(method)
            );
            assert_eq!(
                SelectiveColorMethod::from_menu(method.as_menu()),
                Some(method)
            );
            assert_eq!(method.as_menu(), method.as_u32() + 1);
            assert!(!method.label().is_empty());
        }
        assert_eq!(SelectiveColorMethod::from_menu(0), None);
        assert_eq!(SelectiveColorMethod::from_menu(3), None);
        for range in SelectiveColorRange::ALL {
            assert_eq!(SelectiveColorRange::from_key(range.as_key()), Some(range));
            assert_eq!(SelectiveColorRange::from_menu(range.as_menu()), Some(range));
            assert_eq!(range.as_menu(), range.index() as u32 + 1);
            assert!(!range.label().is_empty());
        }
        assert_eq!(SelectiveColorRange::from_menu(0), None);
        assert_eq!(SelectiveColorRange::from_menu(10), None);
    }

    #[test]
    fn out_of_range_deltas_are_rejected_per_parameter() {
        let channels = CHANNEL_ERRORS;
        for parameter in 0..40usize {
            let (error, channel) = if parameter < CHANNEL_COUNT {
                (channels[parameter], parameter)
            } else {
                let index = parameter - CHANNEL_COUNT;
                (channels[index % CHANNEL_COUNT], index % CHANNEL_COUNT)
            };
            for value in [
                SELECTIVE_COLOR_SLIDER_MAX + 0.5,
                SELECTIVE_COLOR_SLIDER_MIN - 0.5,
            ] {
                let mut trim = ZERO_TRIM;
                let mut deltas = ZERO_DELTAS;
                if parameter < CHANNEL_COUNT {
                    trim[channel] = value;
                } else {
                    let index = parameter - CHANNEL_COUNT;
                    deltas[index / CHANNEL_COUNT][channel] = value;
                }
                let result = SelectiveColorGrade::new(1, 1, trim, deltas);
                assert_eq!(result, Err(error), "parameter {parameter} value {value}");
            }
            // A non-finite amount is rejected by the same check.
            let mut trim = ZERO_TRIM;
            let mut deltas = ZERO_DELTAS;
            if parameter < CHANNEL_COUNT {
                trim[channel] = f32::NAN;
            } else {
                let index = parameter - CHANNEL_COUNT;
                deltas[index / CHANNEL_COUNT][channel] = f32::INFINITY;
            }
            assert_eq!(SelectiveColorGrade::new(1, 1, trim, deltas), Err(error));
        }
        assert_eq!(
            SelectiveColorGrade::new(1, 1, [100.5, 0.0, 0.0, 0.0], ZERO_DELTAS)
                .unwrap_err()
                .to_string(),
            "selective color cyan must be within -100..=100"
        );
    }

    #[test]
    fn mismatched_menu_values_are_rejected() {
        assert_eq!(
            SelectiveColorGrade::new(0, 1, ZERO_TRIM, ZERO_DELTAS),
            Err(SelectiveColorError::Method)
        );
        assert_eq!(
            SelectiveColorGrade::new(3, 1, ZERO_TRIM, ZERO_DELTAS),
            Err(SelectiveColorError::Method)
        );
        assert_eq!(
            SelectiveColorGrade::new(1, 0, ZERO_TRIM, ZERO_DELTAS),
            Err(SelectiveColorError::Range)
        );
        assert_eq!(
            SelectiveColorGrade::new(1, 10, ZERO_TRIM, ZERO_DELTAS),
            Err(SelectiveColorError::Range)
        );
        // The method is validated before the range, so a bad method wins.
        assert_eq!(
            SelectiveColorGrade::new(9, 99, ZERO_TRIM, ZERO_DELTAS),
            Err(SelectiveColorError::Method)
        );
        assert_eq!(
            SelectiveColorError::Range.to_string(),
            "selective color range must be within 1..=9"
        );
        assert_eq!(
            SelectiveColorError::Method.to_string(),
            "selective color method must be 1 (relative) or 2 (absolute)"
        );
    }

    #[test]
    fn a_cancelled_checkpoint_aborts_the_frame() {
        let grade = single(
            SelectiveColorMethod::Relative,
            SelectiveColorRange::Reds,
            0,
            50.0,
        );
        let mut pixels = frame(64, 64);
        let untouched = pixels.clone();
        let mut calls = 0usize;
        let mut checkpoint = || {
            calls += 1;
            if calls == 2 {
                return Err("cancelled");
            }
            Ok(())
        };
        assert_eq!(
            apply_selective_color_f32_controlled(&mut pixels, &grade, &mut checkpoint),
            Err("cancelled")
        );
        // The first chunk ran, the second was rejected before touching pixels.
        assert_ne!(pixels[..CHECKPOINT_STRIDE], untouched[..CHECKPOINT_STRIDE]);
        assert_eq!(pixels[CHECKPOINT_STRIDE..], untouched[CHECKPOINT_STRIDE..]);
    }

    #[test]
    fn the_region_kernel_checkpoints_once_per_chunk() {
        let grade = single(
            SelectiveColorMethod::Relative,
            SelectiveColorRange::Reds,
            0,
            25.0,
        );
        let width = 64;
        let height = 256;
        let mut pixels = frame(width, height);
        let calls = std::cell::Cell::new(0usize);
        let mut checkpoint = || {
            calls.set(calls.get() + 1);
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(apply_selective_color_f32_region_controlled(
            &mut pixels,
            width,
            height,
            &grade,
            &mut checkpoint,
        )
        .unwrap());
        // 16384 pixels in 4096-pixel chunks: four chunk checkpoints plus the
        // trailing one.
        assert_eq!(calls.get(), 5);
        // A mismatched extent reports `false` and writes nothing.
        let mut short = vec![[0.0; 4]; 10];
        assert!(!apply_selective_color_f32_region_controlled(
            &mut short,
            64,
            64,
            &grade,
            &mut checkpoint,
        )
        .unwrap());
        assert!(short.iter().all(|pixel| *pixel == [0.0; 4]));
    }
}
