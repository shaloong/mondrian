//! Channel combiner (通道合成器): rewrites the RGBA slots of every pixel from a
//! template of source channels.
//!
//! `自` (`from`) names 19 source channels — the four channels, four derived
//! scalars, and eleven multi-channel templates. `至` (`to`) names the 17
//! templates that can be written as output, which is the same closed set minus
//! saturation and hue (they are not channels and cannot be written directly).
//! Pairing is positional: the *i*-th source member is written into the *i*-th
//! destination slot, a shorter source is broadcast through its first member,
//! and a longer source is truncated. Slots that the destination does not name
//! are kept bit-for-bit.
//! The second-layer form reads these same slots from the sampled auxiliary
//! picture and keeps the primary picture as the destination.
//!
//! Alpha contract:
//!
//! 1. Alpha only changes when the A slot is written (`至` names A) or
//!    `solid_alpha > 0`; every other configuration leaves alpha bit-identical.
//! 2. Pixels with `alpha = 0` are processed like any other pixel — there is no
//!    early return. A template that writes A, or a derived target that rewrites
//!    RGB, can make a fully transparent pixel visible. The GPU pass therefore
//!    has to be registered in the author-coverage query.
//! 3. No premultiply, no unpremultiply, no clamping: input and output are both
//!    straight coverage, and channel values stay unbounded (negative HDR values
//!    survive a pass-through slot).
//! 4. A NaN alpha propagates when the A slot is written (inverting NaN yields
//!    NaN again); an alpha of NaN never triggers a special branch. A NaN
//!    `solid_alpha` is rejected while building the grade.
//! 5. `apply_pixel` never reads pixel coordinates: this is a pure point effect,
//!    so there is no texel-centre offset to reproduce.
//!
//! Identity is exact only at the graph-builder level. `is_identity()` reports
//! `true` for a self-mapping template (`自 = 至`, e.g. luminance → luminance)
//! so the single-input builder can elide the node; the kernel itself has no identity branch.
//! Running such a grade by hand is *not* bit-identical: the luminance target
//! multiplies RGB by `target / base`, and `base <= 0` (pure black, or a
//! negative-luminance HDR pixel) is defined as a scale of `0.0`, which maps
//! those pixels to black. The same caveat applies to a manually constructed
//! grade that is never elided.

use std::hash::{Hash, Hasher};

use crate::hsl_space::extremes;
use crate::shift_channels::{hue_sample, saturation_sample};

/// Number of pixels between cooperative cancellation checkpoints.
const CHECKPOINT_STRIDE: usize = 4_096;

/// Channel numbers shared by the source and slot encodings.
const SLOT_RED: u8 = 0;
const SLOT_GREEN: u8 = 1;
const SLOT_BLUE: u8 = 2;
const SLOT_ALPHA: u8 = 3;
const SLOT_LUMINANCE: u8 = 4;
const SLOT_LIGHTNESS: u8 = 5;
const SLOT_SATURATION: u8 = 6;
const SLOT_HUE: u8 = 7;
const SLOT_KEEP: u8 = 15;

/// One entry of the closed channel set.
///
/// The first eight variants are single values (four channels and four derived
/// scalars); the rest are fixed multi-channel templates. Variant order is the
/// persisted 1-based index: sources use `1..=19`, destinations use the same set
/// without [`Self::Saturation`] and [`Self::Hue`], i.e. `1..=17`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChannelCombinerChannel {
    /// Alpha (`p.a`).
    Alpha,
    /// Red (`p.r`).
    Red,
    /// Green (`p.g`).
    Green,
    /// Blue (`p.b`).
    Blue,
    /// CIE Y of the working space: `dot(rgb, coefficients)`.
    Luminance,
    /// HSL lightness: `0.5 * (max + min)`.
    Lightness,
    /// HSL saturation: `chroma / (1 - |2 * L - 1|)`.
    Saturation,
    /// HSL hue in turns.
    Hue,
    /// `[R, G]`.
    RedGreen,
    /// `[R, B]`.
    RedBlue,
    /// `[A, R]`.
    AlphaRed,
    /// `[G, B]`.
    GreenBlue,
    /// `[A, G]`.
    AlphaGreen,
    /// `[A, B]`.
    AlphaBlue,
    /// `[R, G, B]`.
    RedGreenBlue,
    /// `[A, R, G]`.
    AlphaRedGreen,
    /// `[A, R, B]`.
    AlphaRedBlue,
    /// `[A, G, B]`.
    AlphaGreenBlue,
    /// `[R, G, B, A]`.
    RedGreenBlueAlpha,
}

impl ChannelCombinerChannel {
    /// Every source channel, in index order.
    pub const SOURCE_ALL: [Self; 19] = [
        Self::Alpha,
        Self::Red,
        Self::Green,
        Self::Blue,
        Self::Luminance,
        Self::Lightness,
        Self::Saturation,
        Self::Hue,
        Self::RedGreen,
        Self::RedBlue,
        Self::AlphaRed,
        Self::GreenBlue,
        Self::AlphaGreen,
        Self::AlphaBlue,
        Self::RedGreenBlue,
        Self::AlphaRedGreen,
        Self::AlphaRedBlue,
        Self::AlphaGreenBlue,
        Self::RedGreenBlueAlpha,
    ];

    /// Every writable destination channel, in index order.
    pub const DESTINATION_ALL: [Self; 17] = [
        Self::Alpha,
        Self::Red,
        Self::Green,
        Self::Blue,
        Self::Luminance,
        Self::Lightness,
        Self::RedGreen,
        Self::RedBlue,
        Self::AlphaRed,
        Self::GreenBlue,
        Self::AlphaGreen,
        Self::AlphaBlue,
        Self::RedGreenBlue,
        Self::AlphaRedGreen,
        Self::AlphaRedBlue,
        Self::AlphaGreenBlue,
        Self::RedGreenBlueAlpha,
    ];

    /// Persisted 1-based source index (`1..=19`).
    pub const fn source_index(self) -> u8 {
        match self {
            Self::Alpha => 1,
            Self::Red => 2,
            Self::Green => 3,
            Self::Blue => 4,
            Self::Luminance => 5,
            Self::Lightness => 6,
            Self::Saturation => 7,
            Self::Hue => 8,
            Self::RedGreen => 9,
            Self::RedBlue => 10,
            Self::AlphaRed => 11,
            Self::GreenBlue => 12,
            Self::AlphaGreen => 13,
            Self::AlphaBlue => 14,
            Self::RedGreenBlue => 15,
            Self::AlphaRedGreen => 16,
            Self::AlphaRedBlue => 17,
            Self::AlphaGreenBlue => 18,
            Self::RedGreenBlueAlpha => 19,
        }
    }

    /// Persisted 1-based destination index (`1..=17`), or `0` for the two
    /// source-only channels that cannot be written.
    pub const fn destination_index(self) -> u8 {
        match self {
            Self::Saturation | Self::Hue => 0,
            // The first six destinations are the first six sources; every
            // template after the two dropped scalars sits two places lower.
            Self::Alpha
            | Self::Red
            | Self::Green
            | Self::Blue
            | Self::Luminance
            | Self::Lightness => self.source_index(),
            _ => self.source_index() - 2,
        }
    }

    /// Source channel for a 1-based index.
    pub fn source_from_index(index: u8) -> Option<Self> {
        Self::SOURCE_ALL.into_iter().find(|channel| channel.source_index() == index)
    }

    /// Destination channel for a 1-based index.
    pub fn destination_from_index(index: u8) -> Option<Self> {
        Self::DESTINATION_ALL
            .into_iter()
            .find(|channel| channel.destination_index() == index)
    }

    /// Whether this channel can be used as `至`.
    pub fn is_destination(self) -> bool {
        !matches!(self, Self::Saturation | Self::Hue)
    }

    /// Stable key used by the enum parameter and the localization catalogs.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Alpha => "alpha",
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Luminance => "luminance",
            Self::Lightness => "lightness",
            Self::Saturation => "saturation",
            Self::Hue => "hue",
            Self::RedGreen => "red_green",
            Self::RedBlue => "red_blue",
            Self::AlphaRed => "alpha_red",
            Self::GreenBlue => "green_blue",
            Self::AlphaGreen => "alpha_green",
            Self::AlphaBlue => "alpha_blue",
            Self::RedGreenBlue => "red_green_blue",
            Self::AlphaRedGreen => "alpha_red_green",
            Self::AlphaRedBlue => "alpha_red_blue",
            Self::AlphaGreenBlue => "alpha_green_blue",
            Self::RedGreenBlueAlpha => "red_green_blue_alpha",
        }
    }

    /// Display label of this channel.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Alpha => "Alpha",
            Self::Red => "红色",
            Self::Green => "绿色",
            Self::Blue => "蓝色",
            Self::Luminance => "亮度",
            Self::Lightness => "明度",
            Self::Saturation => "饱和度",
            Self::Hue => "色相",
            Self::RedGreen => "红绿",
            Self::RedBlue => "红蓝",
            Self::AlphaRed => "Alpha 红",
            Self::GreenBlue => "绿蓝",
            Self::AlphaGreen => "Alpha 绿",
            Self::AlphaBlue => "Alpha 蓝",
            Self::RedGreenBlue => "红绿蓝",
            Self::AlphaRedGreen => "Alpha 红绿",
            Self::AlphaRedBlue => "Alpha 红蓝",
            Self::AlphaGreenBlue => "Alpha 绿蓝",
            Self::RedGreenBlueAlpha => "红绿蓝 Alpha",
        }
    }

    /// Derived scalar this channel samples, if any.
    pub(crate) fn derived(self) -> Option<ChannelCombinerDerived> {
        match self {
            Self::Luminance => Some(ChannelCombinerDerived::Luminance),
            Self::Lightness => Some(ChannelCombinerDerived::Lightness),
            Self::Saturation => Some(ChannelCombinerDerived::Saturation),
            Self::Hue => Some(ChannelCombinerDerived::Hue),
            _ => None,
        }
    }

    /// Member slot codes in template order.
    ///
    /// Codes `0..=3` are the RGBA channels and `4..=7` the derived scalars; the
    /// encoding is shared with the GPU slot nibbles, so a member list can be
    /// copied into a grade slot verbatim. The first element is the broadcast
    /// value for a shorter source.
    pub(crate) fn slots(self) -> &'static [u8] {
        match self {
            Self::Alpha => &[SLOT_ALPHA],
            Self::Red => &[SLOT_RED],
            Self::Green => &[SLOT_GREEN],
            Self::Blue => &[SLOT_BLUE],
            Self::Luminance => &[SLOT_LUMINANCE],
            Self::Lightness => &[SLOT_LIGHTNESS],
            Self::Saturation => &[SLOT_SATURATION],
            Self::Hue => &[SLOT_HUE],
            Self::RedGreen => &[SLOT_RED, SLOT_GREEN],
            Self::RedBlue => &[SLOT_RED, SLOT_BLUE],
            Self::AlphaRed => &[SLOT_ALPHA, SLOT_RED],
            Self::GreenBlue => &[SLOT_GREEN, SLOT_BLUE],
            Self::AlphaGreen => &[SLOT_ALPHA, SLOT_GREEN],
            Self::AlphaBlue => &[SLOT_ALPHA, SLOT_BLUE],
            Self::RedGreenBlue => &[SLOT_RED, SLOT_GREEN, SLOT_BLUE],
            Self::AlphaRedGreen => &[SLOT_ALPHA, SLOT_RED, SLOT_GREEN],
            Self::AlphaRedBlue => &[SLOT_ALPHA, SLOT_RED, SLOT_BLUE],
            Self::AlphaGreenBlue => &[SLOT_ALPHA, SLOT_GREEN, SLOT_BLUE],
            Self::RedGreenBlueAlpha => &[SLOT_RED, SLOT_GREEN, SLOT_BLUE, SLOT_ALPHA],
        }
    }
}

/// Derived scalar that can be sampled from one pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChannelCombinerDerived {
    /// CIE Y of the working space.
    Luminance,
    /// HSL lightness.
    Lightness,
    /// HSL saturation.
    Saturation,
    /// HSL hue in turns.
    Hue,
}

/// One of the four RGBA output slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChannelCombinerSlot {
    /// Leave the input slot untouched.
    Keep,
    /// Copy a channel of the input pixel (`0..=3` for R, G, B, A).
    Channel(u8),
    /// Write a derived scalar of the input pixel.
    Derived(ChannelCombinerDerived),
}

impl ChannelCombinerSlot {
    /// GPU slot code (`15` for [`Self::Keep`]).
    const fn code(self) -> u8 {
        match self {
            Self::Keep => SLOT_KEEP,
            Self::Channel(index) => index,
            Self::Derived(derived) => derived.slot_code(),
        }
    }
}

impl ChannelCombinerDerived {
    /// GPU source code of this scalar.
    const fn slot_code(self) -> u8 {
        match self {
            Self::Luminance => SLOT_LUMINANCE,
            Self::Lightness => SLOT_LIGHTNESS,
            Self::Saturation => SLOT_SATURATION,
            Self::Hue => SLOT_HUE,
        }
    }

    /// GPU `header.z` target code (only the two writable scalars have one).
    const fn target_code(self) -> u32 {
        match self {
            Self::Luminance => 1,
            Self::Lightness => 2,
            Self::Saturation | Self::Hue => 0,
        }
    }
}

/// A normalized channel-combiner grade.
///
/// The four slots are already resolved: broadcasting, truncation and `KEEP`
/// are baked in, so neither the CPU kernel nor the shader needs the source or
/// destination sets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelCombinerGrade {
    source: ChannelCombinerChannel,
    destination: ChannelCombinerChannel,
    slots: [ChannelCombinerSlot; 4],
    derived_target: Option<ChannelCombinerDerived>,
    derived_source: Option<ChannelCombinerDerived>,
    invert: bool,
    solid_alpha: f32,
    luminance_coefficients: [f32; 3],
}

impl Hash for ChannelCombinerGrade {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.slots.hash(state);
        self.derived_target.hash(state);
        self.derived_source.hash(state);
        self.invert.hash(state);
        self.solid_alpha.to_bits().hash(state);
        for coefficient in self.luminance_coefficients {
            coefficient.to_bits().hash(state);
        }
    }
}

/// Invalid channel-combiner parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelCombinerError {
    /// `自` is outside `1..=19`.
    SourceChannel,
    /// `至` is outside `1..=17`.
    DestinationChannel,
    /// The second-layer toggle is malformed.
    SecondLayer,
    /// `源图层` is outside `1..=19`.
    SourceLayer,
    /// `纯色 Alpha` is not a finite number in `[0, 1]`.
    SolidAlpha,
    /// A luminance coefficient is not finite.
    Coefficients,
    /// The source set cannot be paired with the destination set.
    Incompatible,
}

impl std::fmt::Display for ChannelCombinerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::SourceChannel => "源通道编号必须落在 1..=19",
            Self::DestinationChannel => "目标通道编号必须落在 1..=17",
            Self::SecondLayer => "「使用第二图层」必须是布尔值",
            Self::SourceLayer => "源图层必须落在 1..=19",
            Self::SolidAlpha => "纯色 Alpha 必须是 [0,1] 内的有限数",
            Self::Coefficients => "亮度系数必须是有限数",
            Self::Incompatible => "源集合与目标集合无法配对（派生目标要求单一源）",
        })
    }
}

impl std::error::Error for ChannelCombinerError {}

impl ChannelCombinerGrade {
    /// Build a normalized grade from channel values.
    ///
    /// Validation order is fixed: channel indices, derived pairing, solid
    /// alpha, then coefficient finiteness.
    pub fn new(
        source: ChannelCombinerChannel,
        destination: ChannelCombinerChannel,
        invert: bool,
        solid_alpha: f32,
        luminance_coefficients: [f32; 3],
    ) -> Result<Self, ChannelCombinerError> {
        if !destination.is_destination() {
            return Err(ChannelCombinerError::DestinationChannel);
        }
        let mut slots = [ChannelCombinerSlot::Keep; 4];
        let mut derived_target = None;
        let mut derived_source = None;
        if let Some(target) = destination.derived() {
            // A derived target rewrites the whole RGB triple, so it can only
            // consume a single source value.
            let members = source.slots();
            if members.len() != 1 {
                return Err(ChannelCombinerError::Incompatible);
            }
            derived_target = Some(target);
            derived_source = source.derived();
            slots[0] = slot_from_code(members[0]);
        } else {
            let members = source.slots();
            let destinations = destination.slots();
            let paired = members.len().min(destinations.len());
            for (position, destination_code) in destinations.iter().enumerate() {
                let source_code = if position < paired {
                    members[position]
                } else {
                    members[0]
                };
                slots[usize::from(*destination_code)] = slot_from_code(source_code);
            }
        }
        if solid_alpha.is_nan() || !(0.0..=1.0).contains(&solid_alpha) {
            return Err(ChannelCombinerError::SolidAlpha);
        }
        if luminance_coefficients.iter().any(|coefficient| !coefficient.is_finite()) {
            return Err(ChannelCombinerError::Coefficients);
        }
        Ok(Self {
            source,
            destination,
            slots,
            derived_target,
            derived_source,
            invert,
            solid_alpha,
            luminance_coefficients,
        })
    }

    /// Build a normalized grade from persisted 1-based indices.
    pub fn from_indices(
        source_index: u8,
        destination_index: u8,
        invert: bool,
        solid_alpha: f32,
        luminance_coefficients: [f32; 3],
    ) -> Result<Self, ChannelCombinerError> {
        let source = ChannelCombinerChannel::source_from_index(source_index)
            .ok_or(ChannelCombinerError::SourceChannel)?;
        let destination = ChannelCombinerChannel::destination_from_index(destination_index)
            .ok_or(ChannelCombinerError::DestinationChannel)?;
        Self::new(
            source,
            destination,
            invert,
            solid_alpha,
            luminance_coefficients,
        )
    }

    /// Source channel (`自`).
    pub const fn source(&self) -> ChannelCombinerChannel {
        self.source
    }

    /// Destination channel (`至`).
    pub const fn destination(&self) -> ChannelCombinerChannel {
        self.destination
    }

    /// Normalized output slots, in R, G, B, A order.
    pub const fn slots(&self) -> &[ChannelCombinerSlot; 4] {
        &self.slots
    }

    /// Whether every written value is complemented.
    pub const fn invert(&self) -> bool {
        self.invert
    }

    /// Alpha override; `0.0` means "leave alpha to the template".
    pub const fn solid_alpha(&self) -> f32 {
        self.solid_alpha
    }

    /// Working-space CIE Y coefficients.
    pub const fn luminance_coefficients(&self) -> [f32; 3] {
        self.luminance_coefficients
    }

    /// Whether the grade leaves every pixel unchanged.
    ///
    /// This is a graph-builder hint: a `true` grade may be elided. See the
    /// module docs for the derived-target edge case where running the grade by
    /// hand is not bit-identical.
    pub fn is_identity(&self) -> bool {
        if self.invert || self.solid_alpha > 0.0 {
            return false;
        }
        if let Some(target) = self.derived_target {
            return self.derived_source == Some(target);
        }
        self.slots.iter().enumerate().all(|(index, slot)| match slot {
            ChannelCombinerSlot::Keep => true,
            ChannelCombinerSlot::Channel(channel) => usize::from(*channel) == index,
            ChannelCombinerSlot::Derived(_) => false,
        })
    }

    /// Apply the grade to one pixel.
    pub fn apply_pixel(&self, pixel: [f32; 4]) -> [f32; 4] {
        self.apply_pixel_from(pixel, pixel)
    }

    /// Read the selected channels from another pixel and write into the primary pixel.
    pub(crate) fn apply_pixel_from(&self, pixel: [f32; 4], source_pixel: [f32; 4]) -> [f32; 4] {
        if let Some(target) = self.derived_target {
            let value = sample_slot(source_pixel, self.slots[0], self.luminance_coefficients);
            let value = if self.invert { 1.0 - value } else { value };
            let mut out = pixel;
            match target {
                ChannelCombinerDerived::Luminance => {
                    let rgb = [pixel[0], pixel[1], pixel[2]];
                    let base = luminance(rgb, self.luminance_coefficients);
                    let scale = if base > 0.0 { value / base } else { 0.0 };
                    out[0] = rgb[0] * scale;
                    out[1] = rgb[1] * scale;
                    out[2] = rgb[2] * scale;
                }
                ChannelCombinerDerived::Lightness => {
                    let (high, low) = extremes([pixel[0], pixel[1], pixel[2]]);
                    let base = 0.5 * (high + low);
                    let shift = if base.is_nan() { 0.0 } else { value - base };
                    out[0] += shift;
                    out[1] += shift;
                    out[2] += shift;
                }
                ChannelCombinerDerived::Saturation | ChannelCombinerDerived::Hue => {
                    unreachable!("only luminance and lightness are writable targets")
                }
            }
            if self.solid_alpha > 0.0 {
                out[3] = self.solid_alpha;
            }
            return out;
        }
        // Sample every written slot from the input pixel first: a source may
        // coincide with a slot that a later member writes.
        let mut values = [0.0f32; 4];
        let mut written = [false; 4];
        for (index, slot) in self.slots.iter().enumerate() {
            if matches!(slot, ChannelCombinerSlot::Keep) {
                continue;
            }
            let value = sample_slot(source_pixel, *slot, self.luminance_coefficients);
            values[index] = if self.invert { 1.0 - value } else { value };
            written[index] = true;
        }
        let mut out = pixel;
        for (index, is_written) in written.iter().enumerate() {
            if *is_written {
                out[index] = values[index];
            }
        }
        if self.solid_alpha > 0.0 {
            out[3] = self.solid_alpha;
        }
        out
    }

    /// Apply the grade to every pixel of a frame.
    pub fn apply_to_frame(&self, pixels: &mut [[f32; 4]]) {
        for pixel in pixels.iter_mut() {
            *pixel = self.apply_pixel(*pixel);
        }
    }

    /// GPU uniform cells, in the shared `GpuEffectUniform` layout.
    pub fn gpu_controls(&self) -> ([u32; 4], [f32; 4]) {
        let mut slots = 0u32;
        for (index, slot) in self.slots.iter().enumerate() {
            slots |= u32::from(slot.code()) << (4 * index);
        }
        let mut flags = 0u32;
        if let Some(target) = self.derived_target {
            slots = 0xFFFF;
            flags |= target.target_code();
            flags |= u32::from(self.slots[0].code()) << 4;
        }
        if self.invert {
            flags |= 256;
        }
        let coefficients = self.luminance_coefficients;
        (
            [47, slots, flags, 0],
            [
                self.solid_alpha,
                coefficients[0],
                coefficients[1],
                coefficients[2],
            ],
        )
    }
}

/// Convert a member slot code into a grade slot.
fn slot_from_code(code: u8) -> ChannelCombinerSlot {
    match code {
        SLOT_RED | SLOT_GREEN | SLOT_BLUE | SLOT_ALPHA => ChannelCombinerSlot::Channel(code),
        SLOT_LUMINANCE => ChannelCombinerSlot::Derived(ChannelCombinerDerived::Luminance),
        SLOT_LIGHTNESS => ChannelCombinerSlot::Derived(ChannelCombinerDerived::Lightness),
        SLOT_SATURATION => ChannelCombinerSlot::Derived(ChannelCombinerDerived::Saturation),
        SLOT_HUE => ChannelCombinerSlot::Derived(ChannelCombinerDerived::Hue),
        _ => ChannelCombinerSlot::Keep,
    }
}

/// CIE Y of an unclamped RGB triple.
fn luminance(rgb: [f32; 3], coefficients: [f32; 3]) -> f32 {
    rgb[0] * coefficients[0] + rgb[1] * coefficients[1] + rgb[2] * coefficients[2]
}

/// Sample one grade slot from a pixel.
fn sample_slot(pixel: [f32; 4], slot: ChannelCombinerSlot, coefficients: [f32; 3]) -> f32 {
    match slot {
        ChannelCombinerSlot::Keep => 0.0,
        ChannelCombinerSlot::Channel(index) => pixel[usize::from(index)],
        ChannelCombinerSlot::Derived(derived) => {
            let rgb = [pixel[0], pixel[1], pixel[2]];
            match derived {
                ChannelCombinerDerived::Luminance => luminance(rgb, coefficients),
                ChannelCombinerDerived::Lightness => {
                    let (high, low) = extremes(rgb);
                    0.5 * (high + low)
                }
                ChannelCombinerDerived::Saturation => saturation_sample(rgb),
                ChannelCombinerDerived::Hue => hue_sample(rgb),
            }
        }
    }
}

/// Apply the grade to a frame, calling `checkpoint` between strides.
pub(crate) fn apply_channel_combiner_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    grade: &ChannelCombinerGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    for chunk in pixels.chunks_mut(CHECKPOINT_STRIDE) {
        checkpoint()?;
        for pixel in chunk.iter_mut() {
            *pixel = grade.apply_pixel(*pixel);
        }
    }
    checkpoint()?;
    Ok(true)
}

/// Apply the grade to a frame.
pub fn channel_combiner_frame_f32(pixels: &mut [[f32; 4]], grade: &ChannelCombinerGrade) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_channel_combiner_f32_controlled(pixels, grade, &mut checkpoint).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coefficients() -> [f32; 3] {
        [0.2126, 0.7152, 0.0722]
    }

    fn close(actual: f32, expected: f32, tolerance: f32) -> bool {
        (actual - expected).abs() <= tolerance
    }

    fn close_pixel(actual: [f32; 4], expected: [f32; 4], tolerance: f32) -> bool {
        actual
            .iter()
            .zip(expected.iter())
            .all(|(actual, expected)| close(*actual, *expected, tolerance))
    }

    fn bits(pixel: [f32; 4]) -> [u32; 4] {
        pixel.map(f32::to_bits)
    }

    fn pair(source: u8, destination: u8) -> ChannelCombinerGrade {
        ChannelCombinerGrade::from_indices(source, destination, false, 0.0, coefficients())
            .expect("valid grade")
    }

    fn hash_of(grade: &ChannelCombinerGrade) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        grade.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn is_identity_only_for_self_mapping() {
        // 自 = 9 红绿 → 至 = 7 红绿.
        let identity = pair(9, 7);
        assert!(identity.is_identity());
        assert!(
            !ChannelCombinerGrade::from_indices(9, 7, true, 0.0, coefficients())
                .expect("valid")
                .is_identity()
        );
        assert!(
            !ChannelCombinerGrade::from_indices(9, 7, false, 0.25, coefficients())
                .expect("valid")
                .is_identity()
        );
        assert!(pair(5, 5).is_identity());
        assert!(pair(6, 6).is_identity());
        assert!(!pair(5, 6).is_identity());
        assert!(pair(11, 9).is_identity());
        assert!(pair(19, 17).is_identity());
        assert!(!pair(19, 14).is_identity());
        // The default template rewrites two slots and is not elided.
        assert!(!pair(1, 7).is_identity());
    }

    #[test]
    fn default_rewrites_red_and_green_from_alpha_bitwise() {
        let grade = pair(1, 7);
        assert_eq!(
            grade.gpu_controls(),
            ([47, 0xFF33, 0, 0], [0.0, 0.2126, 0.7152, 0.0722])
        );
        let pixel = [0.25, 0.5, 0.75, 1.0];
        let out = grade.apply_pixel(pixel);
        assert_eq!(bits(out), bits([1.0, 1.0, 0.75, 1.0]));
        let pixel = [0.05, 0.07, 0.9, 0.6];
        let out = grade.apply_pixel(pixel);
        assert_eq!(bits(out), bits([0.6, 0.6, 0.9, 0.6]));
        assert_eq!(out[0].to_bits(), pixel[3].to_bits());
    }

    #[test]
    fn alpha_changes_only_when_the_template_writes_it() {
        let pixel = [0.2, 0.4, 0.6, 0.6];
        for destination in ChannelCombinerChannel::DESTINATION_ALL {
            let grade = ChannelCombinerGrade::new(
                ChannelCombinerChannel::Red,
                destination,
                false,
                0.0,
                coefficients(),
            )
            .expect("valid");
            let out = grade.apply_pixel(pixel);
            if destination.slots().contains(&SLOT_ALPHA) {
                assert_eq!(out[3].to_bits(), pixel[0].to_bits(), "{destination:?}");
            } else {
                assert_eq!(out[3].to_bits(), pixel[3].to_bits(), "{destination:?}");
            }
        }
        let with_alpha = ChannelCombinerGrade::new(
            ChannelCombinerChannel::Red,
            ChannelCombinerChannel::RedGreenBlueAlpha,
            false,
            0.0,
            coefficients(),
        )
        .expect("valid");
        assert_eq!(
            with_alpha.apply_pixel(pixel)[3].to_bits(),
            pixel[0].to_bits()
        );
    }

    #[test]
    fn truncation_keeps_prefix_positions() {
        // 自 = 15 [R, G, B] → 至 = 10 [G, B]: G ← R and B ← G, not B ← B.
        let out = pair(15, 10).apply_pixel([0.1, 0.2, 0.3, 0.5]);
        assert_eq!(bits(out), bits([0.1, 0.1, 0.2, 0.5]));
    }

    #[test]
    fn broadcast_repeats_the_first_source() {
        // 自 = 2 红色 → 至 = 17 [R, G, B, A].
        let out = pair(2, 17).apply_pixel([0.2, 0.4, 0.6, 1.0]);
        assert_eq!(bits(out), bits([0.2, 0.2, 0.2, 0.2]));
    }

    #[test]
    fn hue_and_saturation_reuse_the_hsl_helpers() {
        // Pure red has hue 0, so the written slots become 0.
        let out = pair(8, 9).apply_pixel([1.0, 0.0, 0.0, 0.5]);
        assert_eq!(bits(out), bits([0.0, 0.0, 0.0, 0.0]));
        // Grey carries neither hue nor saturation; both report 0 without NaN.
        let grey = [0.5, 0.5, 0.5, 1.0];
        assert_eq!(pair(8, 9).apply_pixel(grey)[0], 0.0);
        assert_eq!(pair(7, 8).apply_pixel(grey)[0], 0.0);
        let hue_of_red = pair(8, 2).apply_pixel([1.0, 0.0, 0.0, 1.0])[0];
        assert!(close(hue_of_red, 0.0, 1.0e-6));
    }

    #[test]
    fn invert_complements_every_written_value() {
        // 自 = 11 [A, R] → 至 = 13 [R, G, B]: R ← A, G ← R, B ← A.
        let grade =
            ChannelCombinerGrade::from_indices(11, 13, true, 0.0, coefficients()).expect("valid");
        let out = grade.apply_pixel([0.2, 0.4, 0.6, 0.8]);
        assert!(close_pixel(out, [0.2, 0.8, 0.2, 0.8], 1.0e-6));
        assert_eq!(out[3].to_bits(), 0.8f32.to_bits());
    }

    #[test]
    fn solid_alpha_overrides_the_template() {
        // 自 = 2 红色 → 至 = 13 [R, G, B] with solid 0.75.
        let grade =
            ChannelCombinerGrade::from_indices(2, 13, false, 0.75, coefficients()).expect("valid");
        let out = grade.apply_pixel([0.2, 0.4, 0.6, 1.0]);
        assert_eq!(bits(out), bits([0.2, 0.2, 0.2, 0.75]));
        assert_eq!(pair(2, 13).apply_pixel([0.2, 0.4, 0.6, 0.6])[3], 0.6);
    }

    #[test]
    fn zero_alpha_pixels_are_still_mapped() {
        // 自 = 5 亮度 → 至 = 9 [A, R]: the invisible pixel becomes visible.
        let out = pair(5, 9).apply_pixel([0.3, 0.0, 0.2, 0.0]);
        assert!(close_pixel(out, [0.07822, 0.0, 0.2, 0.07822], 1.0e-6));
        assert!(out[3] > 0.0);
    }

    #[test]
    fn luminance_target_scales_rgb_uniformly() {
        // 自 = 1 Alpha → 至 = 5 亮度.
        let out = pair(1, 5).apply_pixel([0.25, 0.5, 0.75, 0.5]);
        assert!(close_pixel(
            out,
            [0.268875, 0.53775, 0.8066251, 0.5],
            1.0e-6
        ));
        let ratios = [out[0] / 0.25, out[1] / 0.5, out[2] / 0.75];
        assert!(close(ratios[0], ratios[1], 1.0e-6));
        assert!(close(ratios[1], ratios[2], 1.0e-6));
    }

    #[test]
    fn luminance_target_on_black_outputs_black() {
        let out = pair(1, 5).apply_pixel([0.0, 0.0, 0.0, 1.0]);
        assert_eq!(out, [0.0, 0.0, 0.0, 1.0]);
        // Negative-luminance HDR is not identity through the luminance target:
        // the builder elides `自 = 5 → 至 = 5`, but running that grade by hand
        // scales by zero because the base is not positive.
        let hdr = [-0.2, -0.3, -0.1, 1.0];
        let out = pair(5, 5).apply_pixel(hdr);
        assert_eq!([out[0], out[1], out[2]], [0.0, 0.0, 0.0]);
        assert_eq!(out[3], 1.0);
        assert!(pair(5, 5).is_identity());
    }

    #[test]
    fn lightness_target_shifts_rgb_additively() {
        // 自 = 3 绿色 → 至 = 6 明度; the shift is one scalar for all channels.
        let out = pair(3, 6).apply_pixel([0.2, 0.3, 0.6, 1.0]);
        assert!(close_pixel(out, [0.1, 0.2, 0.5, 1.0], 1.0e-6));
    }

    #[test]
    fn nan_inputs_do_not_produce_new_nan_channels() {
        // `自 = 3 绿色 → 至 = 6 明度` on a pixel whose red is already NaN: the
        // lightness base is finite here (extremes ignores the NaN comparison),
        // so the additive shift is exactly 0 and the finite channels survive.
        let pixel = [f32::NAN, 0.5, 0.5, 1.0];
        let out = pair(3, 6).apply_pixel(pixel);
        assert!(out[0].is_nan());
        assert_eq!(out[1].to_bits(), 0.5f32.to_bits());
        assert_eq!(out[2].to_bits(), 0.5f32.to_bits());
        assert_eq!(out[3], 1.0);
        // A finite pixel never gains a NaN from the same grade.
        let finite = pair(3, 6).apply_pixel([0.2, 0.5, 0.5, 1.0]);
        assert!(finite.iter().all(|value| value.is_finite()));
        // Black through the luminance target stays black.
        assert_eq!(pair(1, 5).apply_pixel([0.0, 0.0, 0.0, 1.0])[0], 0.0);
    }

    #[test]
    fn invalid_parameters_are_rejected() {
        for source in [0, 20, 99] {
            assert_eq!(
                ChannelCombinerGrade::from_indices(source, 7, false, 0.0, coefficients())
                    .unwrap_err(),
                ChannelCombinerError::SourceChannel
            );
        }
        for destination in [0, 18, 99] {
            assert_eq!(
                ChannelCombinerGrade::from_indices(1, destination, false, 0.0, coefficients())
                    .unwrap_err(),
                ChannelCombinerError::DestinationChannel
            );
        }
        for solid in [1.5, -0.1, f32::NAN] {
            assert_eq!(
                ChannelCombinerGrade::new(
                    ChannelCombinerChannel::Alpha,
                    ChannelCombinerChannel::RedGreen,
                    false,
                    solid,
                    coefficients(),
                )
                .unwrap_err(),
                ChannelCombinerError::SolidAlpha
            );
        }
        assert_eq!(
            ChannelCombinerGrade::new(
                ChannelCombinerChannel::RedGreen,
                ChannelCombinerChannel::Luminance,
                false,
                0.0,
                coefficients(),
            )
            .unwrap_err(),
            ChannelCombinerError::Incompatible
        );
        assert_eq!(
            ChannelCombinerGrade::new(
                ChannelCombinerChannel::Alpha,
                ChannelCombinerChannel::RedGreen,
                false,
                0.0,
                [f32::NAN, 1.0, 0.0],
            )
            .unwrap_err(),
            ChannelCombinerError::Coefficients
        );
        for error in [
            ChannelCombinerError::SourceChannel,
            ChannelCombinerError::DestinationChannel,
            ChannelCombinerError::SecondLayer,
            ChannelCombinerError::SourceLayer,
            ChannelCombinerError::SolidAlpha,
            ChannelCombinerError::Coefficients,
            ChannelCombinerError::Incompatible,
        ] {
            assert!(!error.to_string().is_empty());
        }
    }

    #[test]
    fn nibble_encoding_round_trips() {
        // 槽序 R, G, B, A = 色相, 绿, KEEP, 红.
        let grade = ChannelCombinerGrade {
            source: ChannelCombinerChannel::Hue,
            destination: ChannelCombinerChannel::AlphaRed,
            slots: [
                ChannelCombinerSlot::Derived(ChannelCombinerDerived::Hue),
                ChannelCombinerSlot::Channel(SLOT_GREEN),
                ChannelCombinerSlot::Keep,
                ChannelCombinerSlot::Channel(SLOT_RED),
            ],
            derived_target: None,
            derived_source: None,
            invert: false,
            solid_alpha: 0.0,
            luminance_coefficients: coefficients(),
        };
        assert_eq!(grade.gpu_controls().0[1], 0xF17);
        // 自 = 1 Alpha → 至 = 7 红绿 rebuilds an equal, equally-hashed grade.
        let built = pair(1, 7);
        let rebuilt = ChannelCombinerGrade::new(
            ChannelCombinerChannel::Alpha,
            ChannelCombinerChannel::RedGreen,
            false,
            0.0,
            coefficients(),
        )
        .expect("valid");
        assert_eq!(built, rebuilt);
        assert_eq!(hash_of(&built), hash_of(&rebuilt));
    }

    #[test]
    fn hash_covers_every_field() {
        let baseline = pair(1, 7);
        let baseline_hash = hash_of(&baseline);
        let variants = [
            pair(2, 7),
            pair(5, 5),
            ChannelCombinerGrade::from_indices(1, 7, true, 0.0, coefficients()).expect("valid"),
            ChannelCombinerGrade::from_indices(1, 7, false, 0.5, coefficients()).expect("valid"),
            ChannelCombinerGrade::from_indices(1, 7, false, 0.0, [0.1, 0.2, 0.3]).expect("valid"),
        ];
        for variant in variants {
            assert_ne!(hash_of(&variant), baseline_hash);
        }
        // The derived source is part of the hash even though the slots match.
        assert_ne!(hash_of(&pair(5, 5)), hash_of(&pair(6, 5)));
        assert_eq!(hash_of(&baseline), hash_of(&pair(1, 7)));
    }

    #[test]
    fn checkpoint_runs_per_stride_plus_one() {
        let grade = pair(1, 7);
        let mut pixels = vec![[0.1, 0.2, 0.3, 1.0]; 10_000];
        let mut calls = 0usize;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), ()>(())
        };
        assert!(
            apply_channel_combiner_f32_controlled(&mut pixels, &grade, &mut checkpoint).is_ok()
        );
        assert_eq!(calls, 4);
        assert!(channel_combiner_frame_f32(&mut pixels, &grade));
    }
}
