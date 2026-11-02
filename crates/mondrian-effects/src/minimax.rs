//! Minimax: a separable min/max morphology over the selected frame channels.
//!
//! The kernel is the whole effect; the render op only carries a validated
//! [`MinimaxGrade`]. Unlike [`crate::choker`], which is a matte-only morphology,
//! this filter can run on the colour channels, on the alpha channel, or on all
//! four, and it can chain a minimum with a maximum to make an opening or a
//! closing. Every pixel is read and written in **straight alpha**: only the
//! selected channels change, the others stay bit-identical, and the RGB of a
//! fully transparent pixel still takes part in the window exactly like any
//! other sample.
//!
//! # The filter
//!
//! One rounded radius in pixels and one operation:
//!
//! ```text
//! radius == 0  identity, and the graph builder elides the op
//! radius  > 0  window = (2 * radius + 1)² square taps around the pixel
//! ```
//!
//! The square window is separable into one horizontal pass and one vertical
//! pass, so a single-axis direction costs `2 * radius + 1` taps per pixel and a
//! two-axis direction costs twice that. The two axes commute, and the builder
//! always runs the horizontal axis first inside every operation step.
//!
//! The four operations (menus are 1-based, the uniform index is 0-based):
//!
//! | menu | key | label | behaviour |
//! |---|---|---|---|
//! | 1 | `minimum` | 最小 | horizontal then vertical minimum |
//! | 2 | `maximum` | 最大 | horizontal then vertical maximum |
//! | 3 | `minimum_then_maximum` | 先最小后最大 | a full minimum, then a full maximum |
//! | 4 | `maximum_then_minimum` | 先最大后最小 | a full maximum, then a full minimum |
//!
//! Six channel selections: `red` (红), `green` (绿), `blue` (蓝), `alpha`
//! (Alpha), `color` (红绿蓝) and `all` (全部). They are packed as a bitmask
//! (`1 = r`, `2 = g`, `4 = b`, `8 = a`) so a single `select` in the shader covers
//! all six.
//!
//! Three directions: `horizontal` (水平), `vertical` (垂直) and `both` (双向,
//! horizontal then vertical).
//!
//! # Don't shrink edges
//!
//! The authored `dont_shrink_edges` switch (0-1, thresholded at 0.5) removes
//! Alpha from **minimum** passes only: the pass channel mask becomes
//! `mask & 0b0111`. It never affects a maximum pass, never affects the second
//! step of `minimum_then_maximum`, and never touches the RGB selection. When the
//! selection is Alpha alone the protected minimum pass is a no-op, so the effect
//! reduces to a pure alpha dilation.
//!
//! # GPU packing
//!
//! [`MinimaxGrade::gpu_controls`] returns the stage-level pair for the
//! `SpatialPassUniform` cells. `params` is
//! `[radius, axis, extreme, channel_mask]` — the axis and extreme of the first
//! pass of [`MinimaxGrade::passes`] — and `extra` is `[dont_shrink_edges, 0, 0, 0]`.
//! The renderer replaces `axis`/`extreme` per pass and keeps the same radius,
//! mask and flags, so the shader and [`apply_minimax_f32_controlled`] evaluate
//! exactly the same window.
//!
//! # Performance
//!
//! Every pass costs `2 * radius + 1` taps per pixel and per selected channel.
//! There is no pyramid acceleration: a radius of [`MINIMAX_MAX_RADIUS`] scans a
//! whole row or column per tap, which is the same order of cost as the
//! reference implementation and is documented here on purpose. The CPU kernel
//! splits the destination into row bands ([`crate::parallel::for_each_row_band`])
//! and only stages the frame once per pass.
//!
//! # Documented deviations
//!
//! 1. The checklist only numbered the menus; the keys and labels above are
//!    defined by this implementation's design note and are the contract.
//! 2. `dont_shrink_edges` protects Alpha only. The reference application was not
//!    disassembled; the rule here is "a minimum pass must not erode the matte".
//! 3. The largest radius is a brute-force scan, as documented under Performance.
//!
//! # Tests
//!
//! The unit tests cover the documented matrix: identity at radius zero, a
//! minimum eating a bright pixel, a maximum growing a square, per-axis
//! isolation, the four operations against manual two-step chains, every channel
//! option, the `dont_shrink_edges` rule on minimum and maximum passes, edge
//! clamping, the largest radius, straight-alpha RGB, the packed uniform, the
//! pass-count table and the checkpoint cadence.

/// Number of channel menu options.
pub const MINIMAX_CHANNEL_COUNT: usize = 6;
/// Largest authored radius in pixels.
pub const MINIMAX_MAX_RADIUS: u32 = 32_000;
/// Documented default radius: the identity.
pub const MINIMAX_DEFAULT_RADIUS: f32 = 0.0;
/// Documented default `dont_shrink_edges` switch.
pub const MINIMAX_DEFAULT_DONT_SHRINK_EDGES: f32 = 0.0;

/// The horizontal scan axis in the packed pass chain.
const AXIS_HORIZONTAL: u32 = 0;
/// The vertical scan axis in the packed pass chain.
const AXIS_VERTICAL: u32 = 1;
/// The minimum extreme in the packed pass chain.
const EXTREME_MINIMUM: u32 = 0;
/// The maximum extreme in the packed pass chain.
const EXTREME_MAXIMUM: u32 = 1;
/// Alpha bit of the packed channel mask.
const CHANNEL_ALPHA_BIT: u32 = 8;
/// The RGB-only mask a protected minimum pass falls back to.
const COLOR_CHANNEL_MASK: u32 = 0b0111;
/// Checkpoints every this many staged pixels in the sequential staging loop.
const CHECKPOINT_STRIDE: usize = 4096;

/// The four minimax operations, in menu order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MinimaxOperation {
    /// Horizontal then vertical minimum.
    Minimum,
    /// Horizontal then vertical maximum.
    Maximum,
    /// A full minimum followed by a full maximum.
    MinimumThenMaximum,
    /// A full maximum followed by a full minimum.
    MaximumThenMinimum,
}

impl MinimaxOperation {
    /// Every operation, in menu order.
    pub const ALL: [Self; 4] = [
        Self::Minimum,
        Self::Maximum,
        Self::MinimumThenMaximum,
        Self::MaximumThenMinimum,
    ];

    /// The stable serialized key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Minimum => "minimum",
            Self::Maximum => "maximum",
            Self::MinimumThenMaximum => "minimum_then_maximum",
            Self::MaximumThenMinimum => "maximum_then_minimum",
        }
    }

    /// Parses a serialized key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "minimum" => Some(Self::Minimum),
            "maximum" => Some(Self::Maximum),
            "minimum_then_maximum" => Some(Self::MinimumThenMaximum),
            "maximum_then_minimum" => Some(Self::MaximumThenMinimum),
            _ => None,
        }
    }

    /// The 1-based menu index this operation is authored with.
    #[must_use]
    pub const fn as_menu(self) -> i64 {
        match self {
            Self::Minimum => 1,
            Self::Maximum => 2,
            Self::MinimumThenMaximum => 3,
            Self::MaximumThenMinimum => 4,
        }
    }

    /// Resolves a 1-based menu index.
    #[must_use]
    pub const fn from_menu(menu: i64) -> Option<Self> {
        match menu {
            1 => Some(Self::Minimum),
            2 => Some(Self::Maximum),
            3 => Some(Self::MinimumThenMaximum),
            4 => Some(Self::MaximumThenMinimum),
            _ => None,
        }
    }

    /// The 0-based uniform index used by the renderer and the shader.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Minimum => 0,
            Self::Maximum => 1,
            Self::MinimumThenMaximum => 2,
            Self::MaximumThenMinimum => 3,
        }
    }

    /// The localized display name defined by the design note.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Minimum => "最小",
            Self::Maximum => "最大",
            Self::MinimumThenMaximum => "先最小后最大",
            Self::MaximumThenMinimum => "先最大后最小",
        }
    }
}

/// The six channel selections, in menu order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MinimaxChannel {
    /// Red only.
    Red,
    /// Green only.
    Green,
    /// Blue only.
    Blue,
    /// Alpha only.
    Alpha,
    /// Red, green and blue.
    Color,
    /// All four channels.
    All,
}

impl MinimaxChannel {
    /// Every channel selection, in menu order.
    pub const ALL: [Self; MINIMAX_CHANNEL_COUNT] = [
        Self::Red,
        Self::Green,
        Self::Blue,
        Self::Alpha,
        Self::Color,
        Self::All,
    ];

    /// The stable serialized key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Alpha => "alpha",
            Self::Color => "color",
            Self::All => "all",
        }
    }

    /// Parses a serialized key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "red" => Some(Self::Red),
            "green" => Some(Self::Green),
            "blue" => Some(Self::Blue),
            "alpha" => Some(Self::Alpha),
            "color" => Some(Self::Color),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    /// The 1-based menu index this channel is authored with.
    #[must_use]
    pub const fn as_menu(self) -> i64 {
        match self {
            Self::Red => 1,
            Self::Green => 2,
            Self::Blue => 3,
            Self::Alpha => 4,
            Self::Color => 5,
            Self::All => 6,
        }
    }

    /// Resolves a 1-based menu index.
    #[must_use]
    pub const fn from_menu(menu: i64) -> Option<Self> {
        match menu {
            1 => Some(Self::Red),
            2 => Some(Self::Green),
            3 => Some(Self::Blue),
            4 => Some(Self::Alpha),
            5 => Some(Self::Color),
            6 => Some(Self::All),
            _ => None,
        }
    }

    /// The 0-based uniform ordinal.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Red => 0,
            Self::Green => 1,
            Self::Blue => 2,
            Self::Alpha => 3,
            Self::Color => 4,
            Self::All => 5,
        }
    }

    /// The localized display name defined by the design note.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Red => "红",
            Self::Green => "绿",
            Self::Blue => "蓝",
            Self::Alpha => "Alpha",
            Self::Color => "红绿蓝",
            Self::All => "全部",
        }
    }

    /// The bitmask packed into `params.w`.
    #[must_use]
    pub const fn mask(self) -> u32 {
        match self {
            Self::Red => 0b0001,
            Self::Green => 0b0010,
            Self::Blue => 0b0100,
            Self::Alpha => CHANNEL_ALPHA_BIT,
            Self::Color => COLOR_CHANNEL_MASK,
            Self::All => 0b1111,
        }
    }
}

/// The three scan directions, in menu order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MinimaxDirection {
    /// Horizontal pass only.
    Horizontal,
    /// Vertical pass only.
    Vertical,
    /// Horizontal, then vertical.
    Both,
}

impl MinimaxDirection {
    /// Every direction, in menu order.
    pub const ALL: [Self; 3] = [Self::Horizontal, Self::Vertical, Self::Both];

    /// The stable serialized key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
            Self::Both => "both",
        }
    }

    /// Parses a serialized key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "horizontal" => Some(Self::Horizontal),
            "vertical" => Some(Self::Vertical),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    /// The 1-based menu index this direction is authored with.
    #[must_use]
    pub const fn as_menu(self) -> i64 {
        match self {
            Self::Horizontal => 1,
            Self::Vertical => 2,
            Self::Both => 3,
        }
    }

    /// Resolves a 1-based menu index.
    #[must_use]
    pub const fn from_menu(menu: i64) -> Option<Self> {
        match menu {
            1 => Some(Self::Horizontal),
            2 => Some(Self::Vertical),
            3 => Some(Self::Both),
            _ => None,
        }
    }

    /// The 0-based uniform ordinal.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Horizontal => 0,
            Self::Vertical => 1,
            Self::Both => 2,
        }
    }

    /// The localized display name defined by the design note.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Horizontal => "水平",
            Self::Vertical => "垂直",
            Self::Both => "双向",
        }
    }
}

/// The rejection reason of a [`MinimaxGrade`] field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MinimaxError {
    /// The radius is not a finite number.
    NonFiniteRadius,
    /// The radius is outside `[0, 32000]`.
    RadiusOutOfRange,
    /// The `dont_shrink_edges` switch is not a finite number.
    NonFiniteDontShrinkEdges,
    /// The `dont_shrink_edges` switch is outside `[0, 1]`.
    DontShrinkEdgesOutOfRange,
}

impl std::fmt::Display for MinimaxError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::NonFiniteRadius => "minimax radius must be a finite number",
            Self::RadiusOutOfRange => "minimax radius must be inside [0, 32000] pixels",
            Self::NonFiniteDontShrinkEdges => "minimax don't shrink edges must be a finite number",
            Self::DontShrinkEdgesOutOfRange => "minimax don't shrink edges must be inside [0, 1]",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for MinimaxError {}

/// One pass of the renderer chain: one axis and one extreme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MinimaxPass {
    /// Scan axis: `0` horizontal, `1` vertical.
    pub axis: u32,
    /// Extreme: `0` minimum, `1` maximum.
    pub extreme: u32,
}

impl MinimaxPass {
    /// Whether this pass keeps the minimum of the window.
    #[must_use]
    pub const fn minimum(self) -> bool {
        self.extreme == EXTREME_MINIMUM
    }

    /// The channel mask this pass actually filters.
    ///
    /// A minimum pass guarded by `dont_shrink_edges` drops the alpha bit; every
    /// other pass keeps the authored mask. The shader applies the same rule.
    #[must_use]
    pub const fn channel_mask(self, channel: MinimaxChannel, dont_shrink_edges: bool) -> u32 {
        if self.minimum() && dont_shrink_edges {
            channel.mask() & COLOR_CHANNEL_MASK
        } else {
            channel.mask()
        }
    }
}

/// The four passes of the fixed chain table.
const PASS_HORIZONTAL_MINIMUM: MinimaxPass =
    MinimaxPass { axis: AXIS_HORIZONTAL, extreme: EXTREME_MINIMUM };
const PASS_VERTICAL_MINIMUM: MinimaxPass =
    MinimaxPass { axis: AXIS_VERTICAL, extreme: EXTREME_MINIMUM };
const PASS_HORIZONTAL_MAXIMUM: MinimaxPass =
    MinimaxPass { axis: AXIS_HORIZONTAL, extreme: EXTREME_MAXIMUM };
const PASS_VERTICAL_MAXIMUM: MinimaxPass =
    MinimaxPass { axis: AXIS_VERTICAL, extreme: EXTREME_MAXIMUM };

/// The authored parameters of one minimax operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MinimaxGrade {
    radius: u32,
    operation: MinimaxOperation,
    channel: MinimaxChannel,
    direction: MinimaxDirection,
    dont_shrink_edges: bool,
}

impl MinimaxGrade {
    /// Builds a grade from the authored radius in pixels and the menu choices.
    ///
    /// The radius is rounded to the nearest whole pixel, like the shader's
    /// `i32(round(x))`.
    ///
    /// # Errors
    ///
    /// Returns [`MinimaxError::NonFiniteRadius`] or
    /// [`MinimaxError::RadiusOutOfRange`] for a radius outside `[0, 32000]`, and
    /// [`MinimaxError::NonFiniteDontShrinkEdges`] or
    /// [`MinimaxError::DontShrinkEdgesOutOfRange`] for a switch outside `[0, 1]`.
    pub fn new(
        radius: f32,
        operation: MinimaxOperation,
        channel: MinimaxChannel,
        direction: MinimaxDirection,
        dont_shrink_edges: f32,
    ) -> Result<Self, MinimaxError> {
        if !radius.is_finite() {
            return Err(MinimaxError::NonFiniteRadius);
        }
        let rounded = radius.round();
        if !(0.0..=MINIMAX_MAX_RADIUS as f32).contains(&rounded) {
            return Err(MinimaxError::RadiusOutOfRange);
        }
        if !dont_shrink_edges.is_finite() {
            return Err(MinimaxError::NonFiniteDontShrinkEdges);
        }
        if !(0.0..=1.0).contains(&dont_shrink_edges) {
            return Err(MinimaxError::DontShrinkEdgesOutOfRange);
        }
        Ok(Self {
            radius: rounded as u32,
            operation,
            channel,
            direction,
            dont_shrink_edges: dont_shrink_edges >= 0.5,
        })
    }

    /// The documented default grade.
    ///
    /// # Errors
    ///
    /// Never fails: the documented defaults are inside their ranges. The result
    /// is propagated as a `Result` so callers can share one parsing path.
    pub fn documented_defaults() -> Result<Self, MinimaxError> {
        Self::new(
            MINIMAX_DEFAULT_RADIUS,
            MinimaxOperation::Minimum,
            MinimaxChannel::Red,
            MinimaxDirection::Horizontal,
            MINIMAX_DEFAULT_DONT_SHRINK_EDGES,
        )
    }

    /// The integer radius in pixels.
    #[must_use]
    pub const fn radius(self) -> u32 {
        self.radius
    }

    /// The authored operation.
    #[must_use]
    pub const fn operation(self) -> MinimaxOperation {
        self.operation
    }

    /// The authored channel selection.
    #[must_use]
    pub const fn channel(self) -> MinimaxChannel {
        self.channel
    }

    /// The authored direction.
    #[must_use]
    pub const fn direction(self) -> MinimaxDirection {
        self.direction
    }

    /// Whether a minimum pass must leave Alpha untouched.
    #[must_use]
    pub const fn dont_shrink_edges(self) -> bool {
        self.dont_shrink_edges
    }

    /// The authored channel mask, before any per-pass protection.
    #[must_use]
    pub const fn channel_mask(self) -> u32 {
        self.channel.mask()
    }

    /// Whether the grade is a no-op and the graph builder may elide it.
    #[must_use]
    pub const fn is_identity(self) -> bool {
        self.radius == 0
    }

    /// The ordered pass chain the renderer executes.
    ///
    /// The table is the design contract: one operation step over one axis is a
    /// single pass, a two-axis direction doubles it, a two-operation chain
    /// doubles it again, so the chain is always 1, 2 or 4 passes.
    #[must_use]
    pub fn passes(self) -> &'static [MinimaxPass] {
        use MinimaxDirection::{Both, Horizontal, Vertical};
        use MinimaxOperation::{Maximum, MaximumThenMinimum, Minimum, MinimumThenMaximum};
        match (self.operation, self.direction) {
            (Minimum, Horizontal) => &[PASS_HORIZONTAL_MINIMUM],
            (Minimum, Vertical) => &[PASS_VERTICAL_MINIMUM],
            (Minimum, Both) => &[PASS_HORIZONTAL_MINIMUM, PASS_VERTICAL_MINIMUM],
            (Maximum, Horizontal) => &[PASS_HORIZONTAL_MAXIMUM],
            (Maximum, Vertical) => &[PASS_VERTICAL_MAXIMUM],
            (Maximum, Both) => &[PASS_HORIZONTAL_MAXIMUM, PASS_VERTICAL_MAXIMUM],
            (MinimumThenMaximum, Horizontal) => &[PASS_HORIZONTAL_MINIMUM, PASS_HORIZONTAL_MAXIMUM],
            (MinimumThenMaximum, Vertical) => &[PASS_VERTICAL_MINIMUM, PASS_VERTICAL_MAXIMUM],
            (MinimumThenMaximum, Both) => &[
                PASS_HORIZONTAL_MINIMUM,
                PASS_VERTICAL_MINIMUM,
                PASS_HORIZONTAL_MAXIMUM,
                PASS_VERTICAL_MAXIMUM,
            ],
            (MaximumThenMinimum, Horizontal) => &[PASS_HORIZONTAL_MAXIMUM, PASS_HORIZONTAL_MINIMUM],
            (MaximumThenMinimum, Vertical) => &[PASS_VERTICAL_MAXIMUM, PASS_VERTICAL_MINIMUM],
            (MaximumThenMinimum, Both) => &[
                PASS_HORIZONTAL_MAXIMUM,
                PASS_VERTICAL_MAXIMUM,
                PASS_HORIZONTAL_MINIMUM,
                PASS_VERTICAL_MINIMUM,
            ],
        }
    }

    /// The number of renderer passes in [`Self::passes`].
    #[must_use]
    pub fn pass_count(self) -> u32 {
        self.passes().len() as u32
    }

    /// The channel mask of one pass, with the `dont_shrink_edges` rule applied.
    #[must_use]
    pub const fn pass_channel_mask(self, pass: MinimaxPass) -> u32 {
        pass.channel_mask(self.channel, self.dont_shrink_edges)
    }

    /// The stage-level uniform payload: the first pass plus the shared flags.
    ///
    /// `params` is `[radius, axis, extreme, channel_mask]` for
    /// `passes()[0]`; `extra` is `[dont_shrink_edges, 0, 0, 0]`. The renderer
    /// keeps `radius`, the mask and `extra` and replaces the axis and extreme
    /// for every later pass.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        let first = self.passes()[0];
        (
            [
                self.radius as f32,
                first.axis as f32,
                first.extreme as f32,
                self.channel.mask() as f32,
            ],
            [f32::from(u8::from(self.dont_shrink_edges)), 0.0, 0.0, 0.0],
        )
    }

    /// Hashes every authored field so a cache key cannot alias two grades.
    ///
    /// The packed uniform alone would alias `minimum` with
    /// `minimum_then_maximum` (their first pass is identical), so the operation
    /// and direction are hashed explicitly.
    pub(crate) fn hash_state<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash as _;
        self.radius.hash(state);
        self.operation.as_u32().hash(state);
        self.channel.as_u32().hash(state);
        self.direction.as_u32().hash(state);
        self.dont_shrink_edges.hash(state);
    }
}

/// Apply minimax to one straight-alpha rgba f32 frame.
///
/// Returns `false` only when the frame is empty or cannot be addressed; the
/// grade is assumed to be validated already.
pub fn minimax_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &MinimaxGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_minimax_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// The checkpointed kernel behind [`minimax_frame_f32`].
///
/// The per-pass work runs over row bands; the frame is staged into the ping-pong
/// buffers with a checkpoint every [`CHECKPOINT_STRIDE`] pixels so a cancelled
/// render stops early even when the row bands run on this thread only.
///
/// # Errors
///
/// Propagates the first error returned by `checkpoint`.
pub fn apply_minimax_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &MinimaxGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || pixels.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    let frame = &mut pixels[..pixel_count];
    checkpoint()?;

    // Stage 1: copy the frame into the ping-pong source buffer. This is the only
    // sequential per-pixel loop in the kernel, so it carries the checkpoint
    // cadence; the row-band passes below are cancellation-free by construction.
    let mut source = Vec::with_capacity(pixel_count);
    for (index, pixel) in frame.iter().enumerate() {
        if index % CHECKPOINT_STRIDE == 0 {
            checkpoint()?;
        }
        source.push(*pixel);
    }
    checkpoint()?;

    // Stage 2: run the documented pass chain. `source` and `destination` swap so
    // every pass reads a complete previous result and writes a fresh buffer; the
    // radius is converted once because the tap indices are signed.
    let radius = i64::from(grade.radius());
    let mut destination = vec![[0.0_f32; 4]; pixel_count];
    for pass in grade.passes() {
        let context = PassContext {
            axis: pass.axis,
            minimum: pass.minimum(),
            mask: grade.pass_channel_mask(*pass),
            radius,
            width,
            height,
        };
        let current: &[[f32; 4]] = &source;
        crate::parallel::for_each_row_band(
            &mut destination,
            width,
            0..height,
            |row_start, band| {
                context.apply(current, band, row_start);
            },
        );
        std::mem::swap(&mut source, &mut destination);
        checkpoint()?;
    }

    // Stage 3: write the last pass back over row bands, like every other spatial
    // kernel. The copy is exact, so untouched channels keep their bits.
    crate::parallel::for_each_row_band(frame, width, 0..height, |row_start, band| {
        let offset = row_start * width;
        band.copy_from_slice(&source[offset..offset + band.len()]);
    });
    checkpoint()?;
    Ok(true)
}

/// The fixed parameters of one row-band pass.
struct PassContext {
    axis: u32,
    minimum: bool,
    mask: u32,
    radius: i64,
    width: usize,
    height: usize,
}

impl PassContext {
    /// Applies the pass to the rows of `band`, starting at `row_start`.
    fn apply(&self, source: &[[f32; 4]], band: &mut [[f32; 4]], row_start: usize) {
        for (row_offset, row_pixels) in band.chunks_mut(self.width).enumerate() {
            let row = row_start + row_offset;
            for (column, pixel) in row_pixels.iter_mut().enumerate() {
                let mut result = source[row * self.width + column];
                for offset in -self.radius..=self.radius {
                    let (sample_row, sample_column) = if self.axis == AXIS_HORIZONTAL {
                        (row, clamp_index(column as i64 + offset, self.width))
                    } else {
                        (clamp_index(row as i64 + offset, self.height), column)
                    };
                    let candidate = source[sample_row * self.width + sample_column];
                    for (channel, value) in result.iter_mut().enumerate() {
                        if self.mask & (1 << channel) == 0 {
                            continue;
                        }
                        let current = *value;
                        *value = if self.minimum {
                            current.min(candidate[channel])
                        } else {
                            current.max(candidate[channel])
                        };
                    }
                }
                *pixel = result;
            }
        }
    }
}

/// Clamps a signed sample index to `[0, limit - 1]`, repeating the edge pixel.
fn clamp_index(value: i64, limit: usize) -> usize {
    // Every caller has already rejected empty frames, so `limit >= 1`.
    value.clamp(0, limit as i64 - 1) as usize
}

#[cfg(test)]
mod tests {
    use super::{
        apply_minimax_f32_controlled, minimax_frame_f32, MinimaxChannel, MinimaxDirection,
        MinimaxError, MinimaxGrade, MinimaxOperation, MinimaxPass, MINIMAX_CHANNEL_COUNT,
        MINIMAX_DEFAULT_DONT_SHRINK_EDGES, MINIMAX_DEFAULT_RADIUS, MINIMAX_MAX_RADIUS,
    };

    fn grade(radius: f32) -> MinimaxGrade {
        build(
            radius,
            MinimaxOperation::Minimum,
            MinimaxChannel::All,
            MinimaxDirection::Both,
            0.0,
        )
    }

    fn build(
        radius: f32,
        operation: MinimaxOperation,
        channel: MinimaxChannel,
        direction: MinimaxDirection,
        dont_shrink_edges: f32,
    ) -> MinimaxGrade {
        MinimaxGrade::new(radius, operation, channel, direction, dont_shrink_edges)
            .expect("test grades stay inside the authored ranges")
    }

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        vec![[0.0, 0.0, 0.0, 1.0]; width * height]
    }

    fn patterned(width: usize, height: usize) -> Vec<[f32; 4]> {
        (0..width * height)
            .map(|index| {
                [
                    (index % 7) as f32 * 0.25,
                    (index % 5) as f32 * 0.5,
                    (index % 3) as f32 * 0.75,
                    0.25 + (index % 4) as f32 * 0.25,
                ]
            })
            .collect()
    }

    fn alphas(pixels: &[[f32; 4]]) -> Vec<f32> {
        pixels.iter().map(|pixel| pixel[3]).collect()
    }

    #[test]
    fn defaults_are_the_documented_identity() {
        let defaults = MinimaxGrade::documented_defaults().expect("documented defaults are valid");
        assert_eq!(defaults.radius(), 0);
        assert_eq!(defaults.radius(), MINIMAX_DEFAULT_RADIUS as u32);
        assert_eq!(defaults.operation(), MinimaxOperation::Minimum);
        assert_eq!(defaults.channel(), MinimaxChannel::Red);
        assert_eq!(defaults.direction(), MinimaxDirection::Horizontal);
        assert!(!defaults.dont_shrink_edges());
        assert!(defaults.is_identity());
        assert_eq!(defaults.pass_count(), 1);
        assert_eq!(
            defaults.gpu_controls(),
            ([0.0, 0.0, 0.0, 1.0], [0.0; 4]),
            "the default mask is red only"
        );
        assert_eq!(
            defaults.gpu_controls().1[0],
            MINIMAX_DEFAULT_DONT_SHRINK_EDGES
        );
    }

    #[test]
    fn authored_values_outside_the_ranges_are_rejected() {
        let cases = [
            (f32::NAN, 0.0, MinimaxError::NonFiniteRadius),
            (f32::INFINITY, 0.0, MinimaxError::NonFiniteRadius),
            (-1.0, 0.0, MinimaxError::RadiusOutOfRange),
            (
                MINIMAX_MAX_RADIUS as f32 + 1.0,
                0.0,
                MinimaxError::RadiusOutOfRange,
            ),
            (1.0, f32::NAN, MinimaxError::NonFiniteDontShrinkEdges),
            (1.0, -0.01, MinimaxError::DontShrinkEdgesOutOfRange),
            (1.0, 1.01, MinimaxError::DontShrinkEdgesOutOfRange),
        ];
        for (radius, dont_shrink_edges, expected) in cases {
            assert_eq!(
                MinimaxGrade::new(
                    radius,
                    MinimaxOperation::Minimum,
                    MinimaxChannel::All,
                    MinimaxDirection::Both,
                    dont_shrink_edges,
                ),
                Err(expected),
                "radius {radius} switch {dont_shrink_edges}"
            );
        }
        assert!(MinimaxGrade::new(
            MINIMAX_MAX_RADIUS as f32,
            MinimaxOperation::Minimum,
            MinimaxChannel::All,
            MinimaxDirection::Both,
            0.0,
        )
        .is_ok());
    }

    #[test]
    fn error_messages_name_the_documented_ranges() {
        assert_eq!(
            MinimaxError::NonFiniteRadius.to_string(),
            "minimax radius must be a finite number"
        );
        assert_eq!(
            MinimaxError::RadiusOutOfRange.to_string(),
            "minimax radius must be inside [0, 32000] pixels"
        );
        assert_eq!(
            MinimaxError::DontShrinkEdgesOutOfRange.to_string(),
            "minimax don't shrink edges must be inside [0, 1]"
        );
        assert_eq!(
            MinimaxError::NonFiniteDontShrinkEdges.to_string(),
            "minimax don't shrink edges must be a finite number"
        );
    }

    #[test]
    fn radius_rounds_like_the_shader_and_the_switch_thresholds_at_half() {
        assert_eq!(
            build(
                2.4,
                MinimaxOperation::Minimum,
                MinimaxChannel::Red,
                MinimaxDirection::Both,
                0.0
            )
            .radius(),
            2
        );
        assert_eq!(
            build(
                2.5,
                MinimaxOperation::Minimum,
                MinimaxChannel::Red,
                MinimaxDirection::Both,
                0.0
            )
            .radius(),
            3
        );
        assert!(build(
            0.4,
            MinimaxOperation::Minimum,
            MinimaxChannel::Red,
            MinimaxDirection::Both,
            0.0
        )
        .is_identity());
        assert!(!build(
            0.5,
            MinimaxOperation::Minimum,
            MinimaxChannel::Red,
            MinimaxDirection::Both,
            0.0
        )
        .is_identity());
        assert_eq!(
            build(
                0.5,
                MinimaxOperation::Minimum,
                MinimaxChannel::Red,
                MinimaxDirection::Both,
                0.0
            )
            .pass_count(),
            2
        );
        assert!(!build(
            1.0,
            MinimaxOperation::Minimum,
            MinimaxChannel::Red,
            MinimaxDirection::Both,
            0.4
        )
        .dont_shrink_edges());
        assert!(build(
            1.0,
            MinimaxOperation::Minimum,
            MinimaxChannel::Red,
            MinimaxDirection::Both,
            0.5
        )
        .dont_shrink_edges());
        assert!(build(
            1.0,
            MinimaxOperation::Minimum,
            MinimaxChannel::Red,
            MinimaxDirection::Both,
            1.0
        )
        .dont_shrink_edges());
    }

    #[test]
    fn menus_are_one_based_and_uniforms_are_zero_based() {
        for (index, operation) in MinimaxOperation::ALL.into_iter().enumerate() {
            let menu = index as i64 + 1;
            assert_eq!(operation.as_menu(), menu);
            assert_eq!(operation.as_u32(), index as u32);
            assert_eq!(MinimaxOperation::from_menu(menu), Some(operation));
            assert_eq!(
                MinimaxOperation::from_key(operation.as_key()),
                Some(operation)
            );
        }
        assert_eq!(MinimaxOperation::from_menu(0), None);
        assert_eq!(MinimaxOperation::from_menu(5), None);

        for (index, channel) in MinimaxChannel::ALL.into_iter().enumerate() {
            let menu = index as i64 + 1;
            assert_eq!(channel.as_menu(), menu);
            assert_eq!(channel.as_u32(), index as u32);
            assert_eq!(MinimaxChannel::from_menu(menu), Some(channel));
            assert_eq!(MinimaxChannel::from_key(channel.as_key()), Some(channel));
        }
        assert_eq!(MinimaxChannel::ALL.len(), MINIMAX_CHANNEL_COUNT);
        assert_eq!(MinimaxChannel::from_menu(7), None);

        for (index, direction) in MinimaxDirection::ALL.into_iter().enumerate() {
            let menu = index as i64 + 1;
            assert_eq!(direction.as_menu(), menu);
            assert_eq!(direction.as_u32(), index as u32);
            assert_eq!(MinimaxDirection::from_menu(menu), Some(direction));
            assert_eq!(
                MinimaxDirection::from_key(direction.as_key()),
                Some(direction)
            );
        }
        assert_eq!(MinimaxDirection::from_menu(4), None);
    }

    #[test]
    fn channel_masks_match_the_packed_bits() {
        assert_eq!(MinimaxChannel::Red.mask(), 0b0001);
        assert_eq!(MinimaxChannel::Green.mask(), 0b0010);
        assert_eq!(MinimaxChannel::Blue.mask(), 0b0100);
        assert_eq!(MinimaxChannel::Alpha.mask(), 0b1000);
        assert_eq!(MinimaxChannel::Color.mask(), 0b0111);
        assert_eq!(MinimaxChannel::All.mask(), 0b1111);
    }

    #[test]
    fn the_pass_chain_follows_the_design_table() {
        let horizontal = MinimaxDirection::Horizontal;
        let vertical = MinimaxDirection::Vertical;
        let both = MinimaxDirection::Both;
        let pass = |axis, extreme| MinimaxPass { axis, extreme };
        let cases = [
            (MinimaxOperation::Minimum, horizontal, vec![pass(0, 0)]),
            (MinimaxOperation::Minimum, vertical, vec![pass(1, 0)]),
            (
                MinimaxOperation::Minimum,
                both,
                vec![pass(0, 0), pass(1, 0)],
            ),
            (MinimaxOperation::Maximum, horizontal, vec![pass(0, 1)]),
            (MinimaxOperation::Maximum, vertical, vec![pass(1, 1)]),
            (
                MinimaxOperation::Maximum,
                both,
                vec![pass(0, 1), pass(1, 1)],
            ),
            (
                MinimaxOperation::MinimumThenMaximum,
                horizontal,
                vec![pass(0, 0), pass(0, 1)],
            ),
            (
                MinimaxOperation::MinimumThenMaximum,
                vertical,
                vec![pass(1, 0), pass(1, 1)],
            ),
            (
                MinimaxOperation::MinimumThenMaximum,
                both,
                vec![pass(0, 0), pass(1, 0), pass(0, 1), pass(1, 1)],
            ),
            (
                MinimaxOperation::MaximumThenMinimum,
                horizontal,
                vec![pass(0, 1), pass(0, 0)],
            ),
            (
                MinimaxOperation::MaximumThenMinimum,
                vertical,
                vec![pass(1, 1), pass(1, 0)],
            ),
            (
                MinimaxOperation::MaximumThenMinimum,
                both,
                vec![pass(0, 1), pass(1, 1), pass(0, 0), pass(1, 0)],
            ),
        ];
        for (operation, direction, expected) in cases {
            let grade = build(2.0, operation, MinimaxChannel::All, direction, 0.0);
            assert_eq!(
                grade.passes(),
                expected.as_slice(),
                "{operation:?} {direction:?}"
            );
            assert_eq!(grade.pass_count(), expected.len() as u32);
            assert!(matches!(grade.pass_count(), 1 | 2 | 4));
            let (params, _) = grade.gpu_controls();
            assert_eq!(params[1] as u32, expected[0].axis);
            assert_eq!(params[2] as u32, expected[0].extreme);
        }
    }

    #[test]
    fn zero_radius_is_the_identity() {
        let mut pixels = patterned(4, 3);
        let source = pixels.clone();
        assert!(minimax_frame_f32(&mut pixels, 4, 3, &grade(0.0)));
        assert_eq!(pixels, source);
    }

    #[test]
    fn minimum_eats_a_single_bright_pixel() {
        let mut pixels = frame(5, 5);
        pixels[12] = [1.0, 1.0, 1.0, 1.0];
        assert!(minimax_frame_f32(&mut pixels, 5, 5, &grade(1.0)));
        for (index, pixel) in pixels.iter().enumerate() {
            assert_eq!(&pixel[..3], &[0.0, 0.0, 0.0], "pixel {index}");
        }
    }

    #[test]
    fn maximum_grows_a_single_bright_pixel_into_a_square() {
        let mut pixels = frame(5, 5);
        pixels[12] = [1.0, 0.5, 0.25, 1.0];
        assert!(
            minimax_frame_f32(
                &mut pixels,
                5,
                5,
                &build(
                    1.0,
                    MinimaxOperation::Maximum,
                    MinimaxChannel::All,
                    MinimaxDirection::Both,
                    0.0,
                ),
            ),
            "the maximum must grow"
        );
        for row in 0..5 {
            for column in 0..5 {
                let inside = (1..=3).contains(&row) && (1..=3).contains(&column);
                let pixel = pixels[row * 5 + column];
                if inside {
                    assert_eq!(&pixel[..3], &[1.0, 0.5, 0.25], "row {row} column {column}");
                } else {
                    assert_eq!(&pixel[..3], &[0.0, 0.0, 0.0], "row {row} column {column}");
                }
            }
        }
    }

    #[test]
    fn horizontal_spreads_along_the_row_only() {
        let mut pixels = frame(5, 5);
        pixels[12] = [1.0, 1.0, 1.0, 1.0];
        let maximum = build(
            1.0,
            MinimaxOperation::Maximum,
            MinimaxChannel::All,
            MinimaxDirection::Horizontal,
            0.0,
        );
        assert!(minimax_frame_f32(&mut pixels, 5, 5, &maximum));
        let mut expected = frame(5, 5);
        expected[11] = [1.0, 1.0, 1.0, 1.0];
        expected[12] = [1.0, 1.0, 1.0, 1.0];
        expected[13] = [1.0, 1.0, 1.0, 1.0];
        assert_eq!(pixels, expected);
    }

    #[test]
    fn vertical_spreads_along_the_column_only() {
        let mut pixels = frame(5, 5);
        pixels[12] = [1.0, 1.0, 1.0, 1.0];
        let maximum = build(
            1.0,
            MinimaxOperation::Maximum,
            MinimaxChannel::All,
            MinimaxDirection::Vertical,
            0.0,
        );
        assert!(minimax_frame_f32(&mut pixels, 5, 5, &maximum));
        let mut expected = frame(5, 5);
        expected[7] = [1.0, 1.0, 1.0, 1.0];
        expected[12] = [1.0, 1.0, 1.0, 1.0];
        expected[17] = [1.0, 1.0, 1.0, 1.0];
        assert_eq!(pixels, expected);
    }

    #[test]
    fn minimum_then_maximum_matches_two_manual_steps() {
        let source = patterned(6, 5);
        let mut chained = source.clone();
        let combined = build(
            2.0,
            MinimaxOperation::MinimumThenMaximum,
            MinimaxChannel::All,
            MinimaxDirection::Both,
            0.0,
        );
        assert!(minimax_frame_f32(&mut chained, 6, 5, &combined));

        let mut manual = source.clone();
        let minimum = build(
            2.0,
            MinimaxOperation::Minimum,
            MinimaxChannel::All,
            MinimaxDirection::Both,
            0.0,
        );
        let maximum = build(
            2.0,
            MinimaxOperation::Maximum,
            MinimaxChannel::All,
            MinimaxDirection::Both,
            0.0,
        );
        assert!(minimax_frame_f32(&mut manual, 6, 5, &minimum));
        assert!(minimax_frame_f32(&mut manual, 6, 5, &maximum));
        assert_eq!(chained, manual);
    }

    #[test]
    fn maximum_then_minimum_matches_two_manual_steps() {
        let source = patterned(6, 5);
        let mut chained = source.clone();
        let combined = build(
            2.0,
            MinimaxOperation::MaximumThenMinimum,
            MinimaxChannel::All,
            MinimaxDirection::Both,
            0.0,
        );
        assert!(minimax_frame_f32(&mut chained, 6, 5, &combined));

        let mut manual = source.clone();
        let maximum = build(
            2.0,
            MinimaxOperation::Maximum,
            MinimaxChannel::All,
            MinimaxDirection::Both,
            0.0,
        );
        let minimum = build(
            2.0,
            MinimaxOperation::Minimum,
            MinimaxChannel::All,
            MinimaxDirection::Both,
            0.0,
        );
        assert!(minimax_frame_f32(&mut manual, 6, 5, &maximum));
        assert!(minimax_frame_f32(&mut manual, 6, 5, &minimum));
        assert_eq!(chained, manual);
    }

    #[test]
    fn every_channel_option_touches_only_its_selection() {
        let source = patterned(6, 5);
        for channel in MinimaxChannel::ALL {
            for minimum in [true, false] {
                let operation = if minimum {
                    MinimaxOperation::Minimum
                } else {
                    MinimaxOperation::Maximum
                };
                let mut pixels = source.clone();
                let grade = build(1.0, operation, channel, MinimaxDirection::Both, 0.0);
                assert!(minimax_frame_f32(&mut pixels, 6, 5, &grade));
                let mut changed = false;
                for (index, pixel) in pixels.iter().enumerate() {
                    for selected in 0..4 {
                        let bit = 1 << selected;
                        if channel.mask() & bit == 0 {
                            assert_eq!(
                                pixel[selected].to_bits(),
                                source[index][selected].to_bits(),
                                "{channel:?} minimum {minimum} pixel {index} channel {selected}"
                            );
                        } else if pixel[selected].to_bits() != source[index][selected].to_bits() {
                            changed = true;
                        }
                    }
                }
                assert!(changed, "{channel:?} minimum {minimum} changed nothing");
            }
        }
    }

    #[test]
    fn dont_shrink_edges_protects_alpha_in_minimum_passes() {
        let mut source = patterned(5, 5);
        source[12][3] = 0.0;
        let mut pixels = source.clone();
        let grade = build(
            1.0,
            MinimaxOperation::Minimum,
            MinimaxChannel::Alpha,
            MinimaxDirection::Both,
            1.0,
        );
        assert!(minimax_frame_f32(&mut pixels, 5, 5, &grade));
        assert_eq!(
            alphas(&pixels),
            alphas(&source),
            "the minimum must not erode alpha"
        );

        // The rgb channels still erode when they are selected.
        let mut rgb = source.clone();
        let rgb_grade = build(
            1.0,
            MinimaxOperation::Minimum,
            MinimaxChannel::All,
            MinimaxDirection::Both,
            1.0,
        );
        assert!(minimax_frame_f32(&mut rgb, 5, 5, &rgb_grade));
        assert_eq!(alphas(&rgb), alphas(&source));
        assert_ne!(&rgb[..3], &source[..3]);
    }

    #[test]
    fn dont_shrink_edges_does_not_protect_the_maximum_step() {
        let mut pixels = frame(5, 5);
        for row in 0..5 {
            for column in 0..5 {
                let index = row * 5 + column;
                pixels[index][0] = index as f32 * 0.01;
                pixels[index][3] = 0.0;
            }
        }
        pixels[12][3] = 1.0;
        let grade = build(
            2.0,
            MinimaxOperation::MinimumThenMaximum,
            MinimaxChannel::Alpha,
            MinimaxDirection::Vertical,
            1.0,
        );
        assert!(minimax_frame_f32(&mut pixels, 5, 5, &grade));
        let mut expected = vec![0.0_f32; 25];
        // The protected minimum pass changes nothing; the maximum pass dilates
        // the single lit alpha along the whole column at radius 2.
        for row in 0..5 {
            expected[row * 5 + 2] = 1.0;
        }
        assert_eq!(alphas(&pixels), expected);
        for (index, pixel) in pixels.iter().enumerate() {
            assert_eq!(pixel[0].to_bits(), (index as f32 * 0.01).to_bits());
        }
    }

    #[test]
    fn frame_edges_clamp_instead_of_shrinking_the_window() {
        let mut pixels = vec![
            [0.2, 0.0, 0.0, 1.0],
            [0.6, 0.0, 0.0, 1.0],
            [0.4, 0.0, 0.0, 1.0],
        ];
        let grade = build(
            1.0,
            MinimaxOperation::Minimum,
            MinimaxChannel::Red,
            MinimaxDirection::Horizontal,
            0.0,
        );
        assert!(minimax_frame_f32(&mut pixels, 3, 1, &grade));
        assert_eq!(
            pixels.iter().map(|pixel| pixel[0]).collect::<Vec<_>>(),
            vec![0.2, 0.2, 0.4]
        );
    }

    #[test]
    fn straight_alpha_keeps_rgb_and_lets_transparent_pixels_vote() {
        let mut pixels = vec![
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let grade = build(
            1.0,
            MinimaxOperation::Maximum,
            MinimaxChannel::Red,
            MinimaxDirection::Horizontal,
            0.0,
        );
        assert!(minimax_frame_f32(&mut pixels, 3, 1, &grade));
        // The zero-alpha pixel keeps its rgb and its alpha, and its red still
        // reaches its neighbours.
        assert_eq!(pixels[0], [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(pixels[1][0], 1.0);
        assert_eq!(pixels[1][3], 1.0);
        assert_eq!(pixels[1][1], 0.0);
    }

    #[test]
    fn the_largest_radius_collapses_the_frame() {
        let mut pixels = frame(2, 2);
        pixels[0] = [1.0, 1.0, 1.0, 1.0];
        assert!(minimax_frame_f32(
            &mut pixels,
            2,
            2,
            &grade(MINIMAX_MAX_RADIUS as f32)
        ));
        for pixel in &pixels {
            assert_eq!(&pixel[..3], &[0.0, 0.0, 0.0]);
        }
        assert_eq!(alphas(&pixels), vec![1.0; 4]);
    }

    #[test]
    fn unaddressable_frames_are_rejected() {
        let mut pixels = frame(2, 2);
        assert!(!minimax_frame_f32(&mut pixels, 0, 2, &grade(1.0)));
        assert!(!minimax_frame_f32(&mut pixels, 2, 0, &grade(1.0)));
        assert!(!minimax_frame_f32(&mut pixels, 3, 2, &grade(1.0)));
        assert!(!minimax_frame_f32(&mut pixels, usize::MAX, 2, &grade(1.0)));
    }

    #[test]
    fn checkpoint_cadence_is_bounded() {
        let mut pixels = frame(4, 2);
        let mut calls = 0_usize;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        let grade = build(
            1.0,
            MinimaxOperation::Minimum,
            MinimaxChannel::All,
            MinimaxDirection::Horizontal,
            0.0,
        );
        assert_eq!(
            apply_minimax_f32_controlled(&mut pixels, 4, 2, &grade, &mut checkpoint),
            Ok(true)
        );
        // One before staging, one at the first staged pixel, one after staging,
        // one after the single pass, and one after the write-back.
        assert_eq!(calls, 5);
    }

    #[test]
    fn the_checkpoint_error_cancels_the_frame() {
        let mut pixels = frame(4, 2);
        let mut calls = 0_usize;
        let mut checkpoint = || {
            calls += 1;
            if calls == 3 {
                return Err("cancelled");
            }
            Ok(())
        };
        assert_eq!(
            apply_minimax_f32_controlled(&mut pixels, 4, 2, &grade(1.0), &mut checkpoint),
            Err("cancelled")
        );
    }

    #[test]
    fn gpu_controls_pack_the_first_pass_and_the_mask() {
        let grade = build(
            2.0,
            MinimaxOperation::MaximumThenMinimum,
            MinimaxChannel::Alpha,
            MinimaxDirection::Both,
            1.0,
        );
        assert_eq!(
            grade.gpu_controls(),
            ([2.0, 0.0, 1.0, 8.0], [1.0, 0.0, 0.0, 0.0])
        );
        let grade = build(
            3.0,
            MinimaxOperation::Minimum,
            MinimaxChannel::Color,
            MinimaxDirection::Vertical,
            0.0,
        );
        assert_eq!(grade.gpu_controls(), ([3.0, 1.0, 0.0, 7.0], [0.0; 4]));
    }

    #[test]
    fn protected_minimum_passes_drop_only_the_alpha_bit() {
        let grade = build(
            1.0,
            MinimaxOperation::MinimumThenMaximum,
            MinimaxChannel::All,
            MinimaxDirection::Horizontal,
            1.0,
        );
        let minimum = grade.passes()[0];
        let maximum = grade.passes()[1];
        assert_eq!(grade.pass_channel_mask(minimum), 0b0111);
        assert_eq!(grade.pass_channel_mask(maximum), 0b1111);
        let alpha = build(
            1.0,
            MinimaxOperation::MinimumThenMaximum,
            MinimaxChannel::Alpha,
            MinimaxDirection::Horizontal,
            1.0,
        );
        assert_eq!(alpha.pass_channel_mask(alpha.passes()[0]), 0);
        assert_eq!(alpha.pass_channel_mask(alpha.passes()[1]), 8);
    }

    #[test]
    fn hashes_differ_per_authored_state() {
        use std::hash::{Hash as _, Hasher as _};
        let hash = |grade: &MinimaxGrade| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            grade.hash_state(&mut hasher);
            "minimax".hash(&mut hasher);
            hasher.finish()
        };
        let minimum = build(
            2.0,
            MinimaxOperation::Minimum,
            MinimaxChannel::All,
            MinimaxDirection::Both,
            0.0,
        );
        let chained = build(
            2.0,
            MinimaxOperation::MinimumThenMaximum,
            MinimaxChannel::All,
            MinimaxDirection::Both,
            0.0,
        );
        assert_eq!(hash(&minimum), hash(&minimum));
        assert_ne!(
            hash(&minimum),
            hash(&chained),
            "the first pass cannot alias the chain"
        );
        assert_ne!(
            hash(&minimum),
            hash(&build(
                2.0,
                MinimaxOperation::Minimum,
                MinimaxChannel::All,
                MinimaxDirection::Both,
                1.0
            ))
        );
        assert_ne!(
            hash(&minimum),
            hash(&build(
                2.0,
                MinimaxOperation::Minimum,
                MinimaxChannel::Alpha,
                MinimaxDirection::Both,
                0.0
            ))
        );
    }
}
