//! Compound arithmetic (CHN-03): combines the frame with a second layer.
//!
//! The authored controls are the auxiliary layer slot, one of the fifteen
//! Adobe documented operators, the channel set the operator runs on, how an
//! out-of-range result is folded back into the picture, how a differently sized
//! layer is fitted onto the primary frame, and how strongly the result is mixed
//! back with the original image.
//!
//! Both inputs are clamped to the 8-bpc coverage range before the operator runs
//! (the AE compatibility contract). The primary working buffer is premultiplied,
//! the auxiliary raster is straight coverage. The kernel unpremultiplies the
//! primary pixel, clamps, runs the operator, applies the overflow rule, mixes
//! the result back with the original image in premultiplied space, and
//! premultiplies the output.

use std::hash::{Hash, Hasher};

use crate::auxiliary::{sample_auxiliary_f32, AuxiliaryFloatFrame};
use crate::graph::{AuxiliaryCoordinateMapping, AuxiliarySampling};

/// Lowest accepted auxiliary layer slot; `0` means no second layer.
pub const COMPOUND_MIN_LAYER: u32 = 0;
/// Highest accepted auxiliary layer slot.
pub const COMPOUND_MAX_LAYER: u32 = 19;
/// Documented default auxiliary layer slot.
pub const COMPOUND_DEFAULT_LAYER: u32 = 6;
/// Documented default mix with the original image, in percent.
pub const COMPOUND_DEFAULT_BLEND_WITH_ORIGINAL: f32 = 0.0;
/// Range below which a scaled component counts as constant.
pub const COMPOUND_EPSILON: f32 = 1e-6;
/// Eight-bit quantization denominator used by the bitwise operators.
pub const QUANTIZE: f32 = 255.0;
/// Scene-linear Rec. 709 luma weights.
pub const COMPOUND_LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// One of the fifteen documented compound operators, in AE menu order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompoundOperator {
    /// Take the second layer.
    Copy,
    /// Sum of both inputs.
    Add,
    /// Source minus the second layer.
    Subtract,
    /// Product of both inputs.
    Multiply,
    /// Absolute difference of both inputs.
    Difference,
    /// Eight-bit bitwise and.
    And,
    /// Eight-bit bitwise or.
    Or,
    /// Eight-bit bitwise exclusive or.
    Xor,
    /// Take the whole colour of the brighter input.
    Lighter,
    /// Take the whole colour of the darker input.
    Darker,
    /// Per-channel minimum.
    Minimum,
    /// Per-channel maximum.
    Maximum,
    /// `1 - (1 - a)(1 - b)`.
    Screen,
    /// Multiply below half, screen above it.
    Overlay,
    /// Overlay driven by the second layer.
    HardLight,
}

impl CompoundOperator {
    /// Every authored operator, in menu order.
    pub const ALL: [Self; 15] = [
        Self::Copy,
        Self::Add,
        Self::Subtract,
        Self::Multiply,
        Self::Difference,
        Self::And,
        Self::Or,
        Self::Xor,
        Self::Lighter,
        Self::Darker,
        Self::Minimum,
        Self::Maximum,
        Self::Screen,
        Self::Overlay,
        Self::HardLight,
    ];

    /// Stable menu code carried in the GPU uniform block.
    pub const fn code(self) -> u32 {
        match self {
            Self::Copy => 1,
            Self::Add => 2,
            Self::Subtract => 3,
            Self::Multiply => 4,
            Self::Difference => 5,
            Self::And => 6,
            Self::Or => 7,
            Self::Xor => 8,
            Self::Lighter => 9,
            Self::Darker => 10,
            Self::Minimum => 11,
            Self::Maximum => 12,
            Self::Screen => 13,
            Self::Overlay => 14,
            Self::HardLight => 15,
        }
    }

    /// Inverse of [`Self::code`].
    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::Copy),
            2 => Some(Self::Add),
            3 => Some(Self::Subtract),
            4 => Some(Self::Multiply),
            5 => Some(Self::Difference),
            6 => Some(Self::And),
            7 => Some(Self::Or),
            8 => Some(Self::Xor),
            9 => Some(Self::Lighter),
            10 => Some(Self::Darker),
            11 => Some(Self::Minimum),
            12 => Some(Self::Maximum),
            13 => Some(Self::Screen),
            14 => Some(Self::Overlay),
            15 => Some(Self::HardLight),
            _ => None,
        }
    }

    /// Stable schema key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Multiply => "multiply",
            Self::Difference => "difference",
            Self::And => "and",
            Self::Or => "or",
            Self::Xor => "xor",
            Self::Lighter => "lighter",
            Self::Darker => "darker",
            Self::Minimum => "minimum",
            Self::Maximum => "maximum",
            Self::Screen => "screen",
            Self::Overlay => "overlay",
            Self::HardLight => "hard_light",
        }
    }

    /// Inverse of [`Self::as_key`].
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "copy" => Some(Self::Copy),
            "add" => Some(Self::Add),
            "subtract" => Some(Self::Subtract),
            "multiply" => Some(Self::Multiply),
            "difference" => Some(Self::Difference),
            "and" => Some(Self::And),
            "or" => Some(Self::Or),
            "xor" => Some(Self::Xor),
            "lighter" => Some(Self::Lighter),
            "darker" => Some(Self::Darker),
            "minimum" => Some(Self::Minimum),
            "maximum" => Some(Self::Maximum),
            "screen" => Some(Self::Screen),
            "overlay" => Some(Self::Overlay),
            "hard_light" => Some(Self::HardLight),
            _ => None,
        }
    }

    /// Localized menu label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Copy => "复制",
            Self::Add => "相加",
            Self::Subtract => "相减",
            Self::Multiply => "相乘",
            Self::Difference => "差值",
            Self::And => "与",
            Self::Or => "或",
            Self::Xor => "异或",
            Self::Lighter => "变亮",
            Self::Darker => "变暗",
            Self::Minimum => "最小值",
            Self::Maximum => "最大值",
            Self::Screen => "滤色",
            Self::Overlay => "叠加",
            Self::HardLight => "强光",
        }
    }
}

/// Which channels the operator runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompoundChannels {
    /// Operate on R/G/B; the output keeps the source alpha.
    Rgb,
    /// Operate on R/G/B and alpha.
    Argb,
    /// Operate on alpha only; the colour keeps the source.
    Alpha,
}

impl CompoundChannels {
    /// Every authored channel set, in menu order.
    pub const ALL: [Self; 3] = [Self::Rgb, Self::Argb, Self::Alpha];

    /// Stable menu code carried in the GPU uniform block.
    pub const fn code(self) -> u32 {
        match self {
            Self::Rgb => 1,
            Self::Argb => 2,
            Self::Alpha => 3,
        }
    }

    /// Inverse of [`Self::code`].
    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::Rgb),
            2 => Some(Self::Argb),
            3 => Some(Self::Alpha),
            _ => None,
        }
    }

    /// Stable schema key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Rgb => "rgb",
            Self::Argb => "argb",
            Self::Alpha => "alpha",
        }
    }

    /// Inverse of [`Self::as_key`].
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "rgb" => Some(Self::Rgb),
            "argb" => Some(Self::Argb),
            "alpha" => Some(Self::Alpha),
            _ => None,
        }
    }

    /// Localized menu label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Rgb => "RGB",
            Self::Argb => "ARGB",
            Self::Alpha => "Alpha",
        }
    }
}

/// How an out-of-range result is folded back into the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompoundOverflow {
    /// Clamp to the 0..1 coverage range.
    Clip,
    /// Wrap modulo one.
    Wrap,
    /// Stretch this frame's minimum/maximum onto 0..1, per channel.
    Scale,
}

impl CompoundOverflow {
    /// Every authored overflow rule, in menu order.
    pub const ALL: [Self; 3] = [Self::Clip, Self::Wrap, Self::Scale];

    /// Stable menu code carried in the GPU uniform block.
    pub const fn code(self) -> u32 {
        match self {
            Self::Clip => 1,
            Self::Wrap => 2,
            Self::Scale => 3,
        }
    }

    /// Inverse of [`Self::code`].
    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::Clip),
            2 => Some(Self::Wrap),
            3 => Some(Self::Scale),
            _ => None,
        }
    }

    /// Stable schema key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Clip => "clip",
            Self::Wrap => "wrap",
            Self::Scale => "scale",
        }
    }

    /// Inverse of [`Self::as_key`].
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "clip" => Some(Self::Clip),
            "wrap" => Some(Self::Wrap),
            "scale" => Some(Self::Scale),
            _ => None,
        }
    }

    /// Localized menu label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Clip => "剪切",
            Self::Wrap => "回绕",
            Self::Scale => "缩放",
        }
    }
}

/// How a differently sized layer is fitted onto the primary frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompoundStretch {
    /// Stretch the layer over the whole primary frame.
    Stretch,
    /// Keep the layer's own pixel size, aligned to the top-left corner.
    Original,
}

impl CompoundStretch {
    /// Every authored stretch mode, in menu order.
    pub const ALL: [Self; 2] = [Self::Stretch, Self::Original];

    /// Stable menu code carried in the GPU uniform block.
    pub const fn code(self) -> u32 {
        match self {
            Self::Stretch => 1,
            Self::Original => 2,
        }
    }

    /// Inverse of [`Self::code`].
    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::Stretch),
            2 => Some(Self::Original),
            _ => None,
        }
    }

    /// Stable schema key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Stretch => "stretch",
            Self::Original => "original",
        }
    }

    /// Inverse of [`Self::as_key`].
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "stretch" => Some(Self::Stretch),
            "original" => Some(Self::Original),
            _ => None,
        }
    }

    /// Localized menu label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Stretch => "伸缩适配",
            Self::Original => "保持原始尺寸",
        }
    }
}

/// Why a set of authored compound-arithmetic controls was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompoundArithmeticError {
    parameter: &'static str,
}

impl CompoundArithmeticError {
    /// Authored parameter that produced this rejection.
    pub const fn parameter(self) -> &'static str {
        self.parameter
    }
}

impl std::fmt::Display for CompoundArithmeticError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "compound_arithmetic.{} must be finite and within its documented range",
            self.parameter
        )
    }
}

impl std::error::Error for CompoundArithmeticError {}

/// Authored compound-arithmetic controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompoundArithmeticGrade {
    layer: u32,
    operator: CompoundOperator,
    channels: CompoundChannels,
    overflow: CompoundOverflow,
    stretch: CompoundStretch,
    blend_with_original: f32,
}

impl CompoundArithmeticGrade {
    /// Validate authored controls.
    pub fn new(
        layer: u32,
        operator: CompoundOperator,
        channels: CompoundChannels,
        overflow: CompoundOverflow,
        stretch: CompoundStretch,
        blend_with_original: f32,
    ) -> Result<Self, CompoundArithmeticError> {
        if !(COMPOUND_MIN_LAYER..=COMPOUND_MAX_LAYER).contains(&layer) {
            return Err(CompoundArithmeticError { parameter: "layer" });
        }
        if !blend_with_original.is_finite() || !(0.0..=100.0).contains(&blend_with_original) {
            return Err(CompoundArithmeticError { parameter: "blend_with_original" });
        }
        Ok(Self {
            layer,
            operator,
            channels,
            overflow,
            stretch,
            blend_with_original,
        })
    }

    /// Controls a freshly authored effect starts with.
    pub const fn documented_defaults() -> Self {
        Self {
            layer: COMPOUND_DEFAULT_LAYER,
            operator: CompoundOperator::Copy,
            channels: CompoundChannels::Rgb,
            overflow: CompoundOverflow::Clip,
            stretch: CompoundStretch::Stretch,
            blend_with_original: COMPOUND_DEFAULT_BLEND_WITH_ORIGINAL,
        }
    }

    /// Auxiliary layer slot that supplies the second layer.
    pub const fn layer(self) -> u32 {
        self.layer
    }

    /// Documented operator applied to the two inputs.
    pub const fn operator(self) -> CompoundOperator {
        self.operator
    }

    /// Channel set the operator runs on.
    pub const fn channels(self) -> CompoundChannels {
        self.channels
    }

    /// Rule that folds an out-of-range result back into the picture.
    pub const fn overflow(self) -> CompoundOverflow {
        self.overflow
    }

    /// How the layer is fitted onto the primary frame.
    pub const fn stretch(self) -> CompoundStretch {
        self.stretch
    }

    /// Mix with the original image, in the authored `0..=100` percent domain.
    pub const fn blend_with_original(self) -> f32 {
        self.blend_with_original
    }

    /// Mix with the original image in the normalized `0..=1` kernel domain.
    pub const fn blend_with_original_normalized(self) -> f32 {
        self.blend_with_original / 100.0
    }

    /// Packed controls shared with the GPU spatial pass.
    ///
    /// `params = [operator.code(), channels.code(), overflow.code(), 0]` — the
    /// fourth lane is reserved: the renderer overwrites it with the linear
    /// filter flag.
    ///
    /// `extra = [stretch.code(), blend_with_original / 100, layer, 0]`.
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.operator.code() as f32,
                self.channels.code() as f32,
                self.overflow.code() as f32,
                0.0,
            ],
            [
                self.stretch.code() as f32,
                self.blend_with_original_normalized(),
                self.layer as f32,
                0.0,
            ],
        )
    }

    /// Hash every authored control into the render operation's signature.
    pub fn hash_state<H: Hasher>(&self, state: &mut H) {
        self.layer.hash(state);
        self.operator.hash(state);
        self.channels.hash(state);
        self.overflow.hash(state);
        self.stretch.hash(state);
        self.blend_with_original.to_bits().hash(state);
    }

    /// Whether the GPU spatial pass can evaluate these controls.
    ///
    /// The `Scale` overflow rule needs a whole-frame minimum/maximum reduction,
    /// which the single full-screen pass cannot perform.
    pub fn gpu_supported(&self) -> bool {
        self.overflow != CompoundOverflow::Scale
    }

    /// Combine one premultiplied source pixel with one straight layer sample.
    ///
    /// The result is premultiplied, matching the working buffer. `Scale` cannot
    /// be resolved from a single pixel, so this entry point leaves the operated
    /// components un-remapped; the frame kernel performs the whole-frame
    /// reduction.
    pub fn apply_pixel_from(
        &self,
        source_premultiplied: [f32; 4],
        auxiliary_straight: [f32; 4],
    ) -> [f32; 4] {
        let (components, computed) =
            self.operated_components(source_premultiplied, auxiliary_straight);
        self.finish_pixel(source_premultiplied, components, computed, None)
    }

    /// Operated straight components plus the mask naming the operated channels.
    fn operated_components(
        &self,
        source_premultiplied: [f32; 4],
        auxiliary_straight: [f32; 4],
    ) -> ([f32; 4], [bool; 4]) {
        let source = straight_from_premultiplied(source_premultiplied);
        let auxiliary = [
            auxiliary_straight[0].clamp(0.0, 1.0),
            auxiliary_straight[1].clamp(0.0, 1.0),
            auxiliary_straight[2].clamp(0.0, 1.0),
            auxiliary_straight[3].clamp(0.0, 1.0),
        ];
        let luma_source = luma([source[0], source[1], source[2]]);
        let luma_auxiliary = luma([auxiliary[0], auxiliary[1], auxiliary[2]]);
        // Lighter/Darker pick one whole colour, never a per-channel blend.
        let select_layer = match self.operator {
            CompoundOperator::Lighter => luma_auxiliary > luma_source,
            CompoundOperator::Darker => luma_auxiliary < luma_source,
            _ => false,
        };
        let operate = |first: f32, second: f32| {
            apply_channel_operator(self.operator, first, second, select_layer)
        };
        match self.channels {
            CompoundChannels::Rgb => (
                [
                    operate(source[0], auxiliary[0]),
                    operate(source[1], auxiliary[1]),
                    operate(source[2], auxiliary[2]),
                    source[3],
                ],
                [true, true, true, false],
            ),
            CompoundChannels::Argb => (
                [
                    operate(source[0], auxiliary[0]),
                    operate(source[1], auxiliary[1]),
                    operate(source[2], auxiliary[2]),
                    operate(source[3], auxiliary[3]),
                ],
                [true, true, true, true],
            ),
            CompoundChannels::Alpha => (
                [
                    source[0],
                    source[1],
                    source[2],
                    operate(source[3], auxiliary[3]),
                ],
                [false, false, false, true],
            ),
        }
    }

    /// Apply the overflow rule, mix with the original image, premultiply.
    fn finish_pixel(
        &self,
        source_premultiplied: [f32; 4],
        components: [f32; 4],
        computed: [bool; 4],
        scale_ranges: Option<&[[f32; 2]; 4]>,
    ) -> [f32; 4] {
        let mut overflowed = components;
        for (channel, value) in overflowed.iter_mut().enumerate() {
            if !computed[channel] {
                continue;
            }
            let scale_range = scale_ranges.map(|ranges| ranges[channel]);
            *value = self.overflow_component(*value, scale_range);
        }
        let alpha = if computed[3] {
            overflowed[3]
        } else {
            source_premultiplied[3].clamp(0.0, 1.0)
        };
        let color = if self.channels == CompoundChannels::Alpha {
            // Alpha-only keeps the source's straight colour and re-premultiplies
            // it by the newly computed coverage.
            let source = straight_from_premultiplied(source_premultiplied);
            [source[0], source[1], source[2]]
        } else {
            [overflowed[0], overflowed[1], overflowed[2]]
        };
        let effect = [color[0] * alpha, color[1] * alpha, color[2] * alpha, alpha];
        let mix = self.blend_with_original_normalized();
        let final_pixel = [
            effect[0] * (1.0 - mix) + source_premultiplied[0] * mix,
            effect[1] * (1.0 - mix) + source_premultiplied[1] * mix,
            effect[2] * (1.0 - mix) + source_premultiplied[2] * mix,
            effect[3] * (1.0 - mix) + source_premultiplied[3] * mix,
        ];
        if final_pixel[3] <= 0.0 {
            return [0.0; 4];
        }
        final_pixel
    }

    /// Fold one operated component back into the coverage range.
    fn overflow_component(&self, value: f32, scale_range: Option<[f32; 2]>) -> f32 {
        match self.overflow {
            CompoundOverflow::Clip => value.clamp(0.0, 1.0),
            CompoundOverflow::Wrap => value - value.floor(),
            CompoundOverflow::Scale => match scale_range {
                Some([low, high]) if high - low >= COMPOUND_EPSILON => (value - low) / (high - low),
                _ => value,
            },
        }
    }
}

impl Hash for CompoundArithmeticGrade {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash_state(state);
    }
}

/// Straight colour and coverage of one premultiplied working pixel.
#[inline]
fn straight_from_premultiplied(pixel: [f32; 4]) -> [f32; 4] {
    let alpha = pixel[3].clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return [0.0; 4];
    }
    [
        (pixel[0] / alpha).clamp(0.0, 1.0),
        (pixel[1] / alpha).clamp(0.0, 1.0),
        (pixel[2] / alpha).clamp(0.0, 1.0),
        alpha,
    ]
}

/// Scene-linear Rec. 709 luma of a straight colour.
#[inline]
fn luma(color: [f32; 3]) -> f32 {
    COMPOUND_LUMA[0] * color[0] + COMPOUND_LUMA[1] * color[1] + COMPOUND_LUMA[2] * color[2]
}

/// Quantize one straight component to the eight-bit operator domain.
#[inline]
fn quantize(value: f32) -> u32 {
    (value.clamp(0.0, 1.0) * QUANTIZE).round() as u32
}

/// Restore one eight-bit result to the straight component domain.
#[inline]
fn dequantize(value: u32) -> f32 {
    value as f32 / QUANTIZE
}

/// Run one operator on one pair of straight components.
///
/// `select_layer` is the whole-colour decision of the Lighter/Darker pair; it
/// is ignored by every other operator.
#[inline]
fn apply_channel_operator(
    operator: CompoundOperator,
    first: f32,
    second: f32,
    select_layer: bool,
) -> f32 {
    match operator {
        CompoundOperator::Copy => second,
        CompoundOperator::Add => first + second,
        CompoundOperator::Subtract => first - second,
        CompoundOperator::Multiply => first * second,
        CompoundOperator::Difference => (first - second).abs(),
        CompoundOperator::And => dequantize(quantize(first) & quantize(second)),
        CompoundOperator::Or => dequantize(quantize(first) | quantize(second)),
        CompoundOperator::Xor => dequantize(quantize(first) ^ quantize(second)),
        CompoundOperator::Lighter | CompoundOperator::Darker => {
            if select_layer {
                second
            } else {
                first
            }
        }
        CompoundOperator::Minimum => first.min(second),
        CompoundOperator::Maximum => first.max(second),
        CompoundOperator::Screen => 1.0 - (1.0 - first) * (1.0 - second),
        CompoundOperator::Overlay => {
            if first < 0.5 {
                2.0 * first * second
            } else {
                1.0 - 2.0 * (1.0 - first) * (1.0 - second)
            }
        }
        CompoundOperator::HardLight => {
            if second < 0.5 {
                2.0 * first * second
            } else {
                1.0 - 2.0 * (1.0 - first) * (1.0 - second)
            }
        }
    }
}

/// Reference uv for one primary pixel centre under the authored fit.
///
/// `Stretch` maps the whole layer onto the primary frame; `Original` keeps the
/// layer's own pixel size and aligns it to the top-left corner, so coordinates
/// past the layer read as transparent through the declared out-of-bounds rule.
/// The delivery affine is deliberately ignored: the conversion is the extent
/// ratio alone, matching CHN-02's fixed grid mapping.
pub(crate) fn compound_arithmetic_reference_uv(
    x: u32,
    y: u32,
    frame_width: u32,
    frame_height: u32,
    extent: crate::EffectFrameExtent,
    stretch: CompoundStretch,
) -> [f32; 2] {
    let uv = [
        (x as f32 + crate::auxiliary::PIXEL_CENTRE) / frame_width as f32,
        (y as f32 + crate::auxiliary::PIXEL_CENTRE) / frame_height as f32,
    ];
    if stretch == CompoundStretch::Original {
        return [
            uv[0] * frame_width as f32 / extent.width().max(1) as f32,
            uv[1] * frame_height as f32 / extent.height().max(1) as f32,
        ];
    }
    uv
}

/// Merge one component into a band's running `[low, high]` bounds.
#[inline]
fn merge_component(bounds: &mut [f32; 2], value: f32) {
    if value < bounds[0] {
        bounds[0] = value;
    }
    if value > bounds[1] {
        bounds[1] = value;
    }
}

/// Merge one band's `[low, high]` bounds into the shared frame bounds.
#[inline]
fn merge_bounds(bounds: &mut [f32; 2], band: [f32; 2]) {
    if band[0] < bounds[0] {
        bounds[0] = band[0];
    }
    if band[1] > bounds[1] {
        bounds[1] = band[1];
    }
}

/// First `Scale` pass: the whole-frame `[low, high]` bounds of every operated
/// component. Channels the channel set leaves alone keep an empty bound.
fn scale_ranges(
    primary: &mut [[f32; 4]],
    width: u32,
    height: u32,
    reference: AuxiliaryFloatFrame<'_>,
    grid_sampling: AuxiliarySampling,
    grade: &CompoundArithmeticGrade,
) -> [[f32; 2]; 4] {
    let collected = std::sync::Mutex::new([
        [f32::INFINITY, f32::NEG_INFINITY],
        [f32::INFINITY, f32::NEG_INFINITY],
        [f32::INFINITY, f32::NEG_INFINITY],
        [f32::INFINITY, f32::NEG_INFINITY],
    ]);
    let extent = reference.frame.extent();
    crate::parallel::for_each_row_band(primary, width as usize, 0..height as usize, |row, band| {
        let mut local = [
            [f32::INFINITY, f32::NEG_INFINITY],
            [f32::INFINITY, f32::NEG_INFINITY],
            [f32::INFINITY, f32::NEG_INFINITY],
            [f32::INFINITY, f32::NEG_INFINITY],
        ];
        for (offset, row_pixels) in band.chunks(width as usize).enumerate() {
            let y = row as u32 + offset as u32;
            for (x, source) in row_pixels.iter().enumerate() {
                let uv = compound_arithmetic_reference_uv(
                    x as u32,
                    y,
                    width,
                    height,
                    extent,
                    grade.stretch(),
                );
                let auxiliary = sample_auxiliary_f32(reference, uv, grid_sampling);
                let (components, computed) = grade.operated_components(*source, auxiliary);
                for (channel, bounds) in local.iter_mut().enumerate() {
                    if computed[channel] {
                        merge_component(bounds, components[channel]);
                    }
                }
            }
        }
        let mut shared = collected.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for (bounds, band) in shared.iter_mut().zip(local) {
            merge_bounds(bounds, band);
        }
    });
    collected.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Second pass: operate, fold the overflow rule, mix and write back in place.
fn write_pass(
    primary: &mut [[f32; 4]],
    width: u32,
    height: u32,
    reference: AuxiliaryFloatFrame<'_>,
    grid_sampling: AuxiliarySampling,
    grade: &CompoundArithmeticGrade,
    scale_ranges: Option<&[[f32; 2]; 4]>,
) {
    let extent = reference.frame.extent();
    crate::parallel::for_each_row_band(primary, width as usize, 0..height as usize, |row, band| {
        for (offset, row_pixels) in band.chunks_mut(width as usize).enumerate() {
            let y = row as u32 + offset as u32;
            for (x, pixel) in row_pixels.iter_mut().enumerate() {
                let uv = compound_arithmetic_reference_uv(
                    x as u32,
                    y,
                    width,
                    height,
                    extent,
                    grade.stretch(),
                );
                let auxiliary = sample_auxiliary_f32(reference, uv, grid_sampling);
                let (components, computed) = grade.operated_components(*pixel, auxiliary);
                *pixel = grade.finish_pixel(*pixel, components, computed, scale_ranges);
            }
        }
    });
}

/// Compound the Float32 frame with a second layer, with a cooperative checkpoint.
///
/// Returns `Ok(false)` when the buffers are inconsistent, and `Ok(true)` after
/// writing the whole frame.
pub fn apply_compound_arithmetic_controlled<E>(
    primary: &mut [[f32; 4]],
    width: u32,
    height: u32,
    reference: AuxiliaryFloatFrame<'_>,
    sampling: AuxiliarySampling,
    grade: &CompoundArithmeticGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    checkpoint()?;
    if (width as usize).checked_mul(height as usize) != Some(primary.len()) {
        return Ok(false);
    }
    if width == 0 || height == 0 {
        return Ok(false);
    }
    let extent = reference.frame.extent();
    if extent.is_empty()
        || (extent.width() as usize).checked_mul(extent.height() as usize)
            != Some(reference.frame.pixels().len())
    {
        return Ok(false);
    }

    // The reference uv already contains the authored fit, so the sampler must
    // read it on the primary grid.
    let grid_sampling = AuxiliarySampling::new(
        AuxiliaryCoordinateMapping::PrimaryGrid,
        sampling.filter,
        sampling.out_of_bounds,
    );
    let ranges = match grade.overflow() {
        CompoundOverflow::Scale => {
            let ranges = scale_ranges(primary, width, height, reference, grid_sampling, grade);
            checkpoint()?;
            Some(ranges)
        }
        CompoundOverflow::Clip | CompoundOverflow::Wrap => None,
    };
    write_pass(
        primary,
        width,
        height,
        reference,
        grid_sampling,
        grade,
        ranges.as_ref(),
    );
    checkpoint()?;
    Ok(true)
}

/// Compound one Float32 frame with a second layer.
///
/// Returns `false` when the buffers are inconsistent.
pub fn apply_compound_arithmetic_f32(
    primary: &mut [[f32; 4]],
    width: u32,
    height: u32,
    reference: AuxiliaryFloatFrame<'_>,
    sampling: AuxiliarySampling,
    grade: &CompoundArithmeticGrade,
) -> bool {
    apply_compound_arithmetic_controlled(
        primary,
        width,
        height,
        reference,
        sampling,
        grade,
        &mut || Ok::<(), std::convert::Infallible>(()),
    )
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AuxiliaryPolicy, EffectColorDomain, EffectFloatFrameView, EffectFrameExtent,
        EffectValueFormat, EffectWorkingPrecision,
    };

    fn format() -> EffectValueFormat {
        EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightCoverage,
        )
    }

    fn reference(pixels: &[[f32; 4]], width: u32, height: u32) -> AuxiliaryFloatFrame<'_> {
        assert_eq!(pixels.len(), (width as usize) * (height as usize));
        AuxiliaryFloatFrame::new(
            EffectFloatFrameView::new(
                EffectFrameExtent::new(width, height),
                format(),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                pixels,
            ),
            AuxiliaryPolicy::new(crate::AuxiliaryResolution::Authored),
        )
    }

    fn sampling() -> AuxiliarySampling {
        AuxiliarySampling::new(
            AuxiliaryCoordinateMapping::PrimaryGrid,
            crate::AuxiliaryFilter::Linear,
            crate::AuxiliaryOutOfBounds::Transparent,
        )
    }

    fn grade(
        operator: CompoundOperator,
        channels: CompoundChannels,
        overflow: CompoundOverflow,
        blend_with_original: f32,
    ) -> CompoundArithmeticGrade {
        CompoundArithmeticGrade::new(
            6,
            operator,
            channels,
            overflow,
            CompoundStretch::Stretch,
            blend_with_original,
        )
        .unwrap()
    }

    fn run_operator(operator: CompoundOperator, source: [f32; 3], layer: [f32; 3]) -> [f32; 4] {
        grade(operator, CompoundChannels::Rgb, CompoundOverflow::Clip, 0.0).apply_pixel_from(
            [source[0], source[1], source[2], 1.0],
            [layer[0], layer[1], layer[2], 1.0],
        )
    }

    fn run(
        primary: &mut [[f32; 4]],
        width: u32,
        height: u32,
        pixels: &[[f32; 4]],
        aux_width: u32,
        aux_height: u32,
        settings: CompoundArithmeticGrade,
    ) -> bool {
        apply_compound_arithmetic_f32(
            primary,
            width,
            height,
            reference(pixels, aux_width, aux_height),
            sampling(),
            &settings,
        )
    }

    fn solid(width: u32, height: u32, pixel: [f32; 4]) -> Vec<[f32; 4]> {
        vec![pixel; (width as usize) * (height as usize)]
    }

    fn assert_pixel(actual: [f32; 4], expected: [f32; 4]) {
        for (channel, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            assert!(
                (actual - expected).abs() < 1e-5,
                "channel {channel}: {actual} != {expected}"
            );
        }
    }

    #[test]
    fn operators_round_trip_through_their_codes_and_keys() {
        for (index, operator) in CompoundOperator::ALL.into_iter().enumerate() {
            assert_eq!(operator.code(), index as u32 + 1);
            assert_eq!(CompoundOperator::from_code(operator.code()), Some(operator));
            assert_eq!(
                CompoundOperator::from_key(operator.as_key()),
                Some(operator)
            );
            assert!(!operator.label().is_empty());
        }
        assert_eq!(CompoundOperator::Copy.as_key(), "copy");
        assert_eq!(CompoundOperator::HardLight.as_key(), "hard_light");
        assert_eq!(CompoundOperator::HardLight.label(), "强光");
        assert!(CompoundOperator::from_code(0).is_none());
        assert!(CompoundOperator::from_key("blend").is_none());
    }

    #[test]
    fn channels_round_trip_through_their_codes_and_keys() {
        for (index, channels) in CompoundChannels::ALL.into_iter().enumerate() {
            assert_eq!(channels.code(), index as u32 + 1);
            assert_eq!(CompoundChannels::from_code(channels.code()), Some(channels));
            assert_eq!(
                CompoundChannels::from_key(channels.as_key()),
                Some(channels)
            );
            assert!(!channels.label().is_empty());
        }
        assert_eq!(CompoundChannels::Rgb.as_key(), "rgb");
        assert_eq!(CompoundChannels::Argb.as_key(), "argb");
        assert_eq!(CompoundChannels::Alpha.as_key(), "alpha");
    }

    #[test]
    fn overflow_round_trip_through_their_codes_and_keys() {
        for (index, overflow) in CompoundOverflow::ALL.into_iter().enumerate() {
            assert_eq!(overflow.code(), index as u32 + 1);
            assert_eq!(CompoundOverflow::from_code(overflow.code()), Some(overflow));
            assert_eq!(
                CompoundOverflow::from_key(overflow.as_key()),
                Some(overflow)
            );
            assert!(!overflow.label().is_empty());
        }
        assert_eq!(CompoundOverflow::Clip.label(), "剪切");
        assert_eq!(CompoundOverflow::Wrap.label(), "回绕");
        assert_eq!(CompoundOverflow::Scale.label(), "缩放");
    }

    #[test]
    fn stretch_modes_round_trip_through_their_codes_and_keys() {
        for (index, stretch) in CompoundStretch::ALL.into_iter().enumerate() {
            assert_eq!(stretch.code(), index as u32 + 1);
            assert_eq!(CompoundStretch::from_code(stretch.code()), Some(stretch));
            assert_eq!(CompoundStretch::from_key(stretch.as_key()), Some(stretch));
            assert!(!stretch.label().is_empty());
        }
        assert_eq!(CompoundStretch::Stretch.as_key(), "stretch");
        assert_eq!(CompoundStretch::Original.as_key(), "original");
        assert_eq!(CompoundStretch::Stretch.label(), "伸缩适配");
    }

    #[test]
    fn copy_takes_the_second_layer() {
        assert_pixel(
            run_operator(CompoundOperator::Copy, [0.2, 0.4, 0.6], [0.5, 0.25, 0.3]),
            [0.5, 0.25, 0.3, 1.0],
        );
    }

    #[test]
    fn add_sums_the_two_inputs() {
        assert_pixel(
            run_operator(CompoundOperator::Add, [0.2, 0.4, 0.6], [0.5, 0.25, 0.3]),
            [0.7, 0.65, 0.9, 1.0],
        );
    }

    #[test]
    fn subtract_subtracts_the_second_layer() {
        assert_pixel(
            run_operator(
                CompoundOperator::Subtract,
                [0.2, 0.4, 0.6],
                [0.1, 0.25, 0.3],
            ),
            [0.1, 0.15, 0.3, 1.0],
        );
    }

    #[test]
    fn multiply_multiplies_the_two_inputs() {
        assert_pixel(
            run_operator(CompoundOperator::Multiply, [0.5, 0.5, 0.5], [0.4, 0.2, 1.0]),
            [0.2, 0.1, 0.5, 1.0],
        );
    }

    #[test]
    fn difference_takes_the_absolute_difference() {
        assert_pixel(
            run_operator(
                CompoundOperator::Difference,
                [0.2, 0.4, 0.6],
                [0.5, 0.25, 0.3],
            ),
            [0.3, 0.15, 0.3, 1.0],
        );
    }

    #[test]
    fn and_runs_on_eight_bit_quantized_components() {
        assert_pixel(
            run_operator(CompoundOperator::And, [1.0, 0.5, 0.2], [0.5, 0.5, 0.2]),
            [128.0 / 255.0, 128.0 / 255.0, 51.0 / 255.0, 1.0],
        );
    }

    #[test]
    fn or_runs_on_eight_bit_quantized_components() {
        assert_pixel(
            run_operator(CompoundOperator::Or, [1.0, 0.5, 0.2], [0.0, 0.25, 0.2]),
            [1.0, 192.0 / 255.0, 51.0 / 255.0, 1.0],
        );
    }

    #[test]
    fn xor_runs_on_eight_bit_quantized_components() {
        assert_pixel(
            run_operator(CompoundOperator::Xor, [1.0, 0.5, 0.2], [0.5, 0.25, 0.2]),
            [127.0 / 255.0, 192.0 / 255.0, 0.0, 1.0],
        );
    }

    #[test]
    fn bitwise_operators_round_half_to_eight_bit_128() {
        assert_eq!(quantize(0.5), 128);
        assert_pixel(
            run_operator(CompoundOperator::And, [0.5, 0.5, 0.5], [1.0, 1.0, 1.0]),
            [128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0, 1.0],
        );
    }

    #[test]
    fn lighter_takes_the_whole_brighter_colour() {
        // luma([0.1, 0.1, 0.1]) = 0.1 < luma([0.6, 0.3, 0.4]) = 0.371, so the
        // layer is picked as one colour instead of per channel.
        assert_pixel(
            run_operator(CompoundOperator::Lighter, [0.1, 0.1, 0.1], [0.6, 0.3, 0.4]),
            [0.6, 0.3, 0.4, 1.0],
        );
        assert_pixel(
            run_operator(CompoundOperator::Lighter, [0.7, 0.6, 0.5], [0.1, 0.1, 0.1]),
            [0.7, 0.6, 0.5, 1.0],
        );
    }

    #[test]
    fn lighter_keeps_the_source_on_equal_luminance() {
        // Equal RGB luma: the tie goes to the source, proved by its coverage.
        let output = grade(
            CompoundOperator::Lighter,
            CompoundChannels::Argb,
            CompoundOverflow::Clip,
            0.0,
        )
        .apply_pixel_from([0.2, 0.2, 0.2, 0.5], [0.4, 0.4, 0.4, 1.0]);
        assert_pixel(output, [0.2, 0.2, 0.2, 0.5]);
    }

    #[test]
    fn darker_takes_the_whole_darker_colour() {
        assert_pixel(
            run_operator(CompoundOperator::Darker, [0.7, 0.6, 0.5], [0.1, 0.1, 0.1]),
            [0.1, 0.1, 0.1, 1.0],
        );
        assert_pixel(
            run_operator(CompoundOperator::Darker, [0.1, 0.1, 0.1], [0.6, 0.3, 0.4]),
            [0.1, 0.1, 0.1, 1.0],
        );
    }

    #[test]
    fn minimum_takes_the_per_channel_minimum() {
        assert_pixel(
            run_operator(CompoundOperator::Minimum, [0.2, 0.4, 0.6], [0.5, 0.25, 0.3]),
            [0.2, 0.25, 0.3, 1.0],
        );
    }

    #[test]
    fn maximum_takes_the_per_channel_maximum() {
        assert_pixel(
            run_operator(CompoundOperator::Maximum, [0.2, 0.4, 0.6], [0.5, 0.25, 0.3]),
            [0.5, 0.4, 0.6, 1.0],
        );
    }

    #[test]
    fn screen_inverts_the_product_of_the_inverted_inputs() {
        assert_pixel(
            run_operator(CompoundOperator::Screen, [0.2, 0.4, 0.6], [0.5, 0.25, 0.3]),
            [0.6, 0.55, 0.72, 1.0],
        );
    }

    #[test]
    fn overlay_multiplies_below_half_and_screens_above_it() {
        assert_pixel(
            run_operator(CompoundOperator::Overlay, [0.2, 0.6, 0.4], [0.5, 0.5, 0.25]),
            [0.2, 0.6, 0.2, 1.0],
        );
    }

    #[test]
    fn hard_light_is_driven_by_the_second_layer() {
        assert_pixel(
            run_operator(
                CompoundOperator::HardLight,
                [0.2, 0.6, 0.4],
                [0.4, 0.7, 0.2],
            ),
            [0.16, 0.76, 0.16, 1.0],
        );
    }

    #[test]
    fn rgb_mode_keeps_the_source_coverage() {
        let output = grade(
            CompoundOperator::Copy,
            CompoundChannels::Rgb,
            CompoundOverflow::Clip,
            0.0,
        )
        .apply_pixel_from([0.1, 0.2, 0.3, 0.5], [0.6, 0.4, 0.2, 0.0]);
        assert_pixel(output, [0.3, 0.2, 0.1, 0.5]);
    }

    #[test]
    fn argb_mode_operates_on_coverage_too() {
        let output = grade(
            CompoundOperator::Copy,
            CompoundChannels::Argb,
            CompoundOverflow::Clip,
            0.0,
        )
        .apply_pixel_from([0.1, 0.2, 0.3, 0.5], [0.6, 0.4, 0.2, 0.25]);
        assert_pixel(output, [0.15, 0.1, 0.05, 0.25]);
    }

    #[test]
    fn alpha_mode_repremultiplies_the_source_colour() {
        let output = grade(
            CompoundOperator::Copy,
            CompoundChannels::Alpha,
            CompoundOverflow::Clip,
            0.0,
        )
        .apply_pixel_from([0.2, 0.4, 0.6, 1.0], [0.0, 0.0, 0.0, 0.5]);
        assert_pixel(output, [0.1, 0.2, 0.3, 0.5]);
    }

    #[test]
    fn clip_overflow_clamps_both_ends() {
        let settings = grade(
            CompoundOperator::Add,
            CompoundChannels::Rgb,
            CompoundOverflow::Clip,
            0.0,
        );
        assert_pixel(
            settings.apply_pixel_from([0.8, 0.5, 0.8, 1.0], [0.8, 0.5, 0.8, 1.0]),
            [1.0, 1.0, 1.0, 1.0],
        );
        let settings = grade(
            CompoundOperator::Subtract,
            CompoundChannels::Rgb,
            CompoundOverflow::Clip,
            0.0,
        );
        assert_pixel(
            settings.apply_pixel_from([0.2, 0.5, 0.2, 1.0], [0.8, 0.5, 0.8, 1.0]),
            [0.0, 0.0, 0.0, 1.0],
        );
    }

    #[test]
    fn wrap_overflow_wraps_one_to_zero_and_negatives_up() {
        let settings = grade(
            CompoundOperator::Add,
            CompoundChannels::Rgb,
            CompoundOverflow::Wrap,
            0.0,
        );
        assert_pixel(
            settings.apply_pixel_from([0.5, 0.5, 0.5, 1.0], [0.5, 0.5, 0.5, 1.0]),
            [0.0, 0.0, 0.0, 1.0],
        );
        let settings = grade(
            CompoundOperator::Subtract,
            CompoundChannels::Rgb,
            CompoundOverflow::Wrap,
            0.0,
        );
        assert_pixel(
            settings.apply_pixel_from([0.2, 0.2, 0.2, 1.0], [0.7, 0.7, 0.7, 1.0]),
            [0.5, 0.5, 0.5, 1.0],
        );
    }

    #[test]
    fn scale_overflow_stretches_the_frame_range() {
        let pixels = solid(1, 1, [0.0, 0.0, 0.0, 0.0]);
        let settings = grade(
            CompoundOperator::Add,
            CompoundChannels::Rgb,
            CompoundOverflow::Scale,
            0.0,
        );
        let mut primary = vec![[0.25, 0.25, 0.25, 1.0], [0.75, 0.75, 0.75, 1.0]];
        assert!(run(&mut primary, 2, 1, &pixels, 1, 1, settings));
        assert_pixel(primary[0], [0.0, 0.0, 0.0, 1.0]);
        assert_pixel(primary[1], [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn scale_overflow_keeps_a_degenerate_channel_untouched() {
        let pixels = solid(1, 1, [0.0, 0.0, 0.0, 0.0]);
        let settings = grade(
            CompoundOperator::Add,
            CompoundChannels::Rgb,
            CompoundOverflow::Scale,
            0.0,
        );
        let mut primary = vec![[0.5, 0.5, 0.5, 1.0], [0.5, 0.5, 0.5, 1.0]];
        assert!(run(&mut primary, 2, 1, &pixels, 1, 1, settings));
        assert_pixel(primary[0], [0.5, 0.5, 0.5, 1.0]);
        assert_pixel(primary[1], [0.5, 0.5, 0.5, 1.0]);
    }

    #[test]
    fn stretch_overflow_maps_the_whole_layer_onto_the_frame() {
        let layer = solid(2, 2, [0.2, 0.4, 0.6, 1.0]);
        let settings = grade(
            CompoundOperator::Copy,
            CompoundChannels::Rgb,
            CompoundOverflow::Clip,
            0.0,
        );
        let mut primary = solid(2, 2, [0.5, 0.5, 0.5, 1.0]);
        assert!(run(&mut primary, 2, 2, &layer, 2, 2, settings));
        for pixel in primary {
            assert_pixel(pixel, [0.2, 0.4, 0.6, 1.0]);
        }
    }

    #[test]
    fn original_stretch_aligns_the_layer_to_the_top_left() {
        // The left half of the layer is red, the right half blue: only the
        // top-left quadrant survives an unscaled fit.
        let mut layer = solid(4, 4, [0.8, 0.1, 0.1, 1.0]);
        for y in 0..4usize {
            for x in 2..4usize {
                layer[y * 4 + x] = [0.1, 0.1, 0.8, 1.0];
            }
        }
        let settings = CompoundArithmeticGrade::new(
            6,
            CompoundOperator::Copy,
            CompoundChannels::Rgb,
            CompoundOverflow::Clip,
            CompoundStretch::Original,
            0.0,
        )
        .unwrap();
        let mut primary = solid(2, 2, [0.5, 0.5, 0.5, 1.0]);
        assert!(run(&mut primary, 2, 2, &layer, 4, 4, settings));
        for pixel in primary {
            assert_pixel(pixel, [0.8, 0.1, 0.1, 1.0]);
        }
    }

    #[test]
    fn original_stretch_reads_past_the_layer_as_transparent() {
        let mut layer = solid(2, 2, [0.0, 0.0, 0.0, 1.0]);
        layer[0] = [0.2, 0.4, 0.6, 1.0];
        layer[2] = [0.2, 0.4, 0.6, 1.0];
        let settings = CompoundArithmeticGrade::new(
            6,
            CompoundOperator::Copy,
            CompoundChannels::Rgb,
            CompoundOverflow::Clip,
            CompoundStretch::Original,
            0.0,
        )
        .unwrap();
        let mut primary = solid(4, 1, [0.5, 0.5, 0.5, 1.0]);
        assert!(run(&mut primary, 4, 1, &layer, 2, 2, settings));
        assert_pixel(primary[0], [0.2, 0.4, 0.6, 1.0]);
        // Columns past the layer read transparent, so Copy writes zero.
        assert_pixel(primary[2], [0.0, 0.0, 0.0, 1.0]);
        assert_pixel(primary[3], [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn blend_with_original_mixes_at_zero_fifty_and_one_hundred_percent() {
        let source = [0.2, 0.4, 0.6, 1.0];
        let layer = [0.8, 0.8, 0.8, 1.0];
        let at_zero = grade(
            CompoundOperator::Copy,
            CompoundChannels::Rgb,
            CompoundOverflow::Clip,
            0.0,
        );
        assert_pixel(
            at_zero.apply_pixel_from(source, layer),
            [0.8, 0.8, 0.8, 1.0],
        );
        let halfway = grade(
            CompoundOperator::Copy,
            CompoundChannels::Rgb,
            CompoundOverflow::Clip,
            50.0,
        );
        assert_pixel(
            halfway.apply_pixel_from(source, layer),
            [0.5, 0.6, 0.7, 1.0],
        );
        let at_full = grade(
            CompoundOperator::Copy,
            CompoundChannels::Rgb,
            CompoundOverflow::Clip,
            100.0,
        );
        assert_pixel(at_full.apply_pixel_from(source, layer), source);
    }

    #[test]
    fn zero_source_coverage_normalizes_to_transparent() {
        let settings = grade(
            CompoundOperator::Copy,
            CompoundChannels::Rgb,
            CompoundOverflow::Clip,
            0.0,
        );
        assert_pixel(
            settings.apply_pixel_from([0.5, 0.5, 0.5, 0.0], [1.0, 1.0, 1.0, 1.0]),
            [0.0, 0.0, 0.0, 0.0],
        );
    }

    #[test]
    fn gpu_supported_rejects_the_scale_overflow_rule() {
        for overflow in CompoundOverflow::ALL {
            let settings = grade(CompoundOperator::Copy, CompoundChannels::Rgb, overflow, 0.0);
            assert_eq!(
                settings.gpu_supported(),
                overflow != CompoundOverflow::Scale
            );
        }
    }

    #[test]
    fn gpu_controls_pack_the_documented_layout() {
        let settings = CompoundArithmeticGrade::new(
            9,
            CompoundOperator::Overlay,
            CompoundChannels::Argb,
            CompoundOverflow::Clip,
            CompoundStretch::Original,
            50.0,
        )
        .unwrap();
        let (params, extra) = settings.gpu_controls();
        assert_eq!(params, [14.0, 2.0, 1.0, 0.0]);
        assert_eq!(extra, [2.0, 0.5, 9.0, 0.0]);
    }

    #[test]
    fn hash_covers_every_control() {
        use std::collections::hash_map::DefaultHasher;

        fn hash_of(grade: CompoundArithmeticGrade) -> u64 {
            let mut hasher = DefaultHasher::new();
            grade.hash(&mut hasher);
            hasher.finish()
        }

        let base = CompoundArithmeticGrade::documented_defaults();
        assert_eq!(hash_of(base), hash_of(base));
        let variants = [
            CompoundArithmeticGrade::new(
                7,
                CompoundOperator::Copy,
                CompoundChannels::Rgb,
                CompoundOverflow::Clip,
                CompoundStretch::Stretch,
                0.0,
            )
            .unwrap(),
            CompoundArithmeticGrade::new(
                6,
                CompoundOperator::Add,
                CompoundChannels::Rgb,
                CompoundOverflow::Clip,
                CompoundStretch::Stretch,
                0.0,
            )
            .unwrap(),
            CompoundArithmeticGrade::new(
                6,
                CompoundOperator::Copy,
                CompoundChannels::Argb,
                CompoundOverflow::Clip,
                CompoundStretch::Stretch,
                0.0,
            )
            .unwrap(),
            CompoundArithmeticGrade::new(
                6,
                CompoundOperator::Copy,
                CompoundChannels::Rgb,
                CompoundOverflow::Wrap,
                CompoundStretch::Stretch,
                0.0,
            )
            .unwrap(),
            CompoundArithmeticGrade::new(
                6,
                CompoundOperator::Copy,
                CompoundChannels::Rgb,
                CompoundOverflow::Clip,
                CompoundStretch::Original,
                0.0,
            )
            .unwrap(),
            CompoundArithmeticGrade::new(
                6,
                CompoundOperator::Copy,
                CompoundChannels::Rgb,
                CompoundOverflow::Clip,
                CompoundStretch::Stretch,
                25.0,
            )
            .unwrap(),
        ];
        for variant in variants {
            assert_ne!(hash_of(base), hash_of(variant));
        }
    }

    #[test]
    fn documented_defaults_are_the_documented_controls() {
        let defaults = CompoundArithmeticGrade::documented_defaults();
        assert_eq!(defaults.layer(), 6);
        assert_eq!(defaults.operator(), CompoundOperator::Copy);
        assert_eq!(defaults.channels(), CompoundChannels::Rgb);
        assert_eq!(defaults.overflow(), CompoundOverflow::Clip);
        assert_eq!(defaults.stretch(), CompoundStretch::Stretch);
        assert_eq!(defaults.blend_with_original(), 0.0);
        assert_eq!(defaults.blend_with_original_normalized(), 0.0);
        assert_eq!(
            defaults.gpu_controls(),
            ([1.0, 1.0, 1.0, 0.0], [1.0, 0.0, 6.0, 0.0])
        );
    }

    #[test]
    fn out_of_range_controls_are_rejected() {
        for layer in [20, 21, u32::MAX] {
            let error = CompoundArithmeticGrade::new(
                layer,
                CompoundOperator::Copy,
                CompoundChannels::Rgb,
                CompoundOverflow::Clip,
                CompoundStretch::Stretch,
                0.0,
            )
            .unwrap_err();
            assert_eq!(error.parameter(), "layer");
            assert_eq!(
                error.to_string(),
                "compound_arithmetic.layer must be finite and within its documented range"
            );
        }
        for blend in [-1.0, 101.0, f32::NAN, f32::INFINITY] {
            let error = CompoundArithmeticGrade::new(
                6,
                CompoundOperator::Copy,
                CompoundChannels::Rgb,
                CompoundOverflow::Clip,
                CompoundStretch::Stretch,
                blend,
            )
            .unwrap_err();
            assert_eq!(error.parameter(), "blend_with_original");
        }
        assert!(CompoundArithmeticGrade::new(
            0,
            CompoundOperator::Copy,
            CompoundChannels::Rgb,
            CompoundOverflow::Clip,
            CompoundStretch::Stretch,
            0.0,
        )
        .is_ok());
        assert!(CompoundArithmeticGrade::new(
            19,
            CompoundOperator::Copy,
            CompoundChannels::Rgb,
            CompoundOverflow::Clip,
            CompoundStretch::Stretch,
            100.0,
        )
        .is_ok());
    }

    #[test]
    fn inconsistent_buffers_and_an_empty_reference_fail_closed() {
        let layer = solid(1, 1, [0.2, 0.4, 0.6, 1.0]);
        let settings = grade(
            CompoundOperator::Copy,
            CompoundChannels::Rgb,
            CompoundOverflow::Clip,
            0.0,
        );
        let mut primary = vec![[0.5, 0.5, 0.5, 1.0]; 2];
        assert!(!run(&mut primary, 1, 1, &layer, 1, 1, settings));

        let empty: Vec<[f32; 4]> = Vec::new();
        let mut primary = vec![[0.5, 0.5, 0.5, 1.0]];
        assert!(!apply_compound_arithmetic_f32(
            &mut primary,
            1,
            1,
            reference(&empty, 0, 0),
            sampling(),
            &settings,
        ));

        let mut empty_primary: Vec<[f32; 4]> = Vec::new();
        assert!(!apply_compound_arithmetic_f32(
            &mut empty_primary,
            0,
            0,
            reference(&layer, 1, 1),
            sampling(),
            &settings,
        ));
    }

    #[test]
    fn region_execution_matches_the_full_frame() {
        let width = 4u32;
        let height = 2u32;
        let primary: Vec<[f32; 4]> = (0..(width * height) as usize)
            .map(|index| {
                let value = index as f32 * 0.1;
                [value, 1.0 - value, value * 0.5, 0.5 + value * 0.05]
            })
            .collect();
        let layer: Vec<[f32; 4]> = (0..(width * height) as usize)
            .map(|index| {
                let value = index as f32 * 0.07;
                [1.0 - value, value, 0.25, 0.75]
            })
            .collect();
        let settings = CompoundArithmeticGrade::new(
            6,
            CompoundOperator::Screen,
            CompoundChannels::Argb,
            CompoundOverflow::Clip,
            CompoundStretch::Stretch,
            25.0,
        )
        .unwrap();

        let mut full = primary.clone();
        assert!(apply_compound_arithmetic_f32(
            &mut full,
            width,
            height,
            reference(&layer, width, height),
            sampling(),
            &settings,
        ));

        // Retain the right-hand column of both rows.
        let region = crate::adjustment::EffectRasterRegion::new(width, height, 2, 0, 2, 2);
        let mut retained = vec![primary[2], primary[3], primary[6], primary[7]];
        let op = crate::EffectRenderOp::CompoundArithmetic { grade: settings };
        assert!(crate::auxiliary::apply_auxiliary_effect_f32_region(
            &mut retained,
            region,
            &op,
            reference(&layer, width, height),
            sampling(),
            &mut || Ok::<(), std::convert::Infallible>(()),
        )
        .unwrap());
        assert_pixel(retained[0], full[2]);
        assert_pixel(retained[1], full[3]);
        assert_pixel(retained[2], full[6]);
        assert_pixel(retained[3], full[7]);
    }

    #[test]
    fn region_execution_refuses_the_scale_overflow_rule() {
        let settings = grade(
            CompoundOperator::Copy,
            CompoundChannels::Rgb,
            CompoundOverflow::Scale,
            0.0,
        );
        let op = crate::EffectRenderOp::CompoundArithmetic { grade: settings };
        let layer = solid(4, 2, [0.2, 0.4, 0.6, 1.0]);
        let mut retained = vec![[0.5, 0.5, 0.5, 1.0]; 4];
        let region = crate::adjustment::EffectRasterRegion::new(4, 2, 2, 0, 2, 2);
        assert!(!crate::auxiliary::apply_auxiliary_effect_f32_region(
            &mut retained,
            region,
            &op,
            reference(&layer, 4, 2),
            sampling(),
            &mut || Ok::<(), std::convert::Infallible>(()),
        )
        .unwrap());
    }

    /// The checkpoint is called at least once even for a frame smaller than the
    /// stride, and the whole frame is still written.
    #[test]
    fn the_kernel_honours_the_checkpoint() {
        // The layer must be wider than the primary frame: a one-texel layer
        // under `PrimaryGrid` sampling lands half a texel outside its own
        // centre, and the transparent out-of-bounds policy would weigh the
        // sample down instead of exercising the checkpoint path.
        let layer = solid(4, 4, [0.2, 0.4, 0.6, 1.0]);
        let settings = grade(
            CompoundOperator::Copy,
            CompoundChannels::Rgb,
            CompoundOverflow::Clip,
            0.0,
        );
        let mut primary = vec![[0.5, 0.5, 0.5, 1.0]; 4];
        let mut calls = 0usize;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(apply_compound_arithmetic_controlled(
            &mut primary,
            2,
            2,
            reference(&layer, 4, 4),
            sampling(),
            &settings,
            &mut checkpoint,
        )
        .unwrap());
        assert!(calls >= 2);
        for pixel in primary {
            assert_pixel(pixel, [0.2, 0.4, 0.6, 1.0]);
        }
    }
}
