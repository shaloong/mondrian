//! Roughen edges: grows the layer silhouette outward along a fractal field.
//!
//! The effect list names this entry `粗糙边缘` (roughen edges) and gives it a mode,
//! an edge colour and a set of field controls. The kernel reads the alpha gradient
//! of the source, grows coverage outward by up to `border` pixels, hardens or
//! softens the resulting ramp through `sharpness`, and then styles the growth:
//! `roughen` keeps it raw, `rusty` pits it with a second noise field, `photocopy`
//! quantises it into four steps and `cutout` binarises it around half coverage.
//! The four coloured variants additionally fill the grown band with `edge_color`.
//!
//! # Shape
//!
//! For a pixel `p = (x, y)` in pixel-index space the alpha gradient is the central
//! difference of the four clamped neighbours, `g = (gx, gy)`. A flat region has
//! `|g| <= 1e-6` and is returned bit for bit unchanged. Otherwise the unit normal
//! `n = g / |g|` points *into* the shape (towards growing coverage) and the sample
//! is taken at `q = p + s * n` with `s ∈ [0, border]`. The sampled alpha replaces
//! the pixel's own coverage where it is larger, so the silhouette moves outward by
//! at most `border` pixels without ever eroding the interior.
//!
//! # Field
//!
//! The fractal field is value noise in `[-1, 1]` with up to ten octaves, stretched
//! anisotropically by `stretch` and offset in pixels. `evolution` animates the
//! third axis of the noise; with `cycle_evolution` the evolution wraps every
//! `cycle` revolutions instead of walking away. All arithmetic is `f32` so the GPU
//! pass reproduces the CPU kernel bit-comparably within float rounding.

use crate::fractal_noise::FRACTAL_FEATURE_PIXELS;
use crate::noise::noise_sample;
use crate::parallel::for_each_row_band;
use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Smallest border the author surface can express, in pixels.
pub const ROUGHEN_EDGES_MIN_BORDER: f32 = 0.0;
/// Largest border the author surface can express, in pixels.
pub const ROUGHEN_EDGES_MAX_BORDER: f32 = 500.0;
/// Smallest sharpness the author surface can express.
pub const ROUGHEN_EDGES_MIN_SHARPNESS: f32 = 0.0;
/// Largest sharpness the author surface can express.
pub const ROUGHEN_EDGES_MAX_SHARPNESS: f32 = 10.0;
/// Smallest fractal influence the author surface can express.
pub const ROUGHEN_EDGES_MIN_INFLUENCE: f32 = 0.0;
/// Largest fractal influence the author surface can express.
pub const ROUGHEN_EDGES_MAX_INFLUENCE: f32 = 1.0;
/// Smallest scale the author surface can express, in percent.
pub const ROUGHEN_EDGES_MIN_SCALE: f32 = 10.0;
/// Largest scale the author surface can express, in percent.
pub const ROUGHEN_EDGES_MAX_SCALE: f32 = 1000.0;
/// Smallest stretch the author surface can express, in percent.
pub const ROUGHEN_EDGES_MIN_STRETCH: f32 = -100.0;
/// Largest stretch the author surface can express, in percent.
pub const ROUGHEN_EDGES_MAX_STRETCH: f32 = 100.0;
/// Smallest fractal complexity the author surface can express.
pub const ROUGHEN_EDGES_MIN_COMPLEXITY: u32 = 1;
/// Largest fractal complexity the author surface can express.
pub const ROUGHEN_EDGES_MAX_COMPLEXITY: u32 = 10;
/// Smallest evolution cycle the author surface can express, in revolutions.
pub const ROUGHEN_EDGES_MIN_CYCLE: u32 = 1;
/// Largest evolution cycle the author surface can express, in revolutions.
pub const ROUGHEN_EDGES_MAX_CYCLE: u32 = 88;
/// Largest random seed the author surface can express.
pub const ROUGHEN_EDGES_MAX_RANDOM_SEED: u32 = 100_000;
/// Largest absolute evolution the author surface can express, in degrees.
pub const ROUGHEN_EDGES_MAX_EVOLUTION_DEGREES: f32 = 36_000.0;
/// Largest absolute field offset the author surface can express, in pixels.
pub const ROUGHEN_EDGES_MAX_OFFSET_PIXELS: f32 = 1_000.0;
/// Gradients at or below this magnitude are treated as flat.
pub const ROUGHEN_EDGES_GRADIENT_EPSILON: f32 = 1.0e-6;
/// The anisotropic stretch never divides the field by less than this factor.
pub const ROUGHEN_EDGES_MIN_STRETCH_FACTOR: f32 = 0.05;
/// Smallest alpha used when dividing a premultiplied sample back to straight colour.
pub const ROUGHEN_EDGES_MIN_SAMPLE_ALPHA: f32 = 1.0e-6;
/// Salt mixed into the fractal field.
pub const ROUGHEN_EDGES_FIELD_SALT: u32 = 0x8F3A_2C17;
/// Salt mixed into the pitting field of the rusty modes.
pub const ROUGHEN_EDGES_RUST_SALT: u32 = 0x51ED_2701;
/// Pixels per field unit at `scale = 1000`.
pub const ROUGHEN_EDGES_FEATURE_PIXELS: f32 = FRACTAL_FEATURE_PIXELS;

/// Pixels copied per cancellation checkpoint.
const CHECKPOINT_STRIDE: usize = 4_096;
/// Rotation applied to the integer octave before it is folded into the salt.
const OCTAVE_SALT_MULTIPLIER: u32 = 0x27D4_EB2F;
/// Rotation applied to the octave index inside the fractal sum.
const FBM_SALT_MULTIPLIER: u32 = 0x85EB_CA6B;
/// Rotation applied to the random seed before it is folded into the salt.
const SEED_SALT_MULTIPLIER: u32 = 0x9E37_79B9;

/// The eight authoring modes, in menu order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoughenEdgesType {
    /// Grow the silhouette without styling.
    Roughen,
    /// Grow the silhouette and fill it with the edge colour.
    RoughenColor,
    /// Dim the growth with a second noise field.
    Rusty,
    /// Dim the growth and fill it with the edge colour.
    RustyColor,
    /// Quantise the growth into four coverage steps.
    Photocopy,
    /// Quantise the growth and fill it with the edge colour.
    PhotocopyColor,
    /// Binarise the growth around half coverage.
    Cutout,
    /// Binarise the growth and fill it with the edge colour.
    CutoutColor,
}

impl RoughenEdgesType {
    /// Every mode in authoring menu order.
    pub const ALL: [Self; 8] = [
        Self::Roughen,
        Self::RoughenColor,
        Self::Rusty,
        Self::RustyColor,
        Self::Photocopy,
        Self::PhotocopyColor,
        Self::Cutout,
        Self::CutoutColor,
    ];

    /// Stable persistence key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Roughen => "roughen",
            Self::RoughenColor => "roughen-color",
            Self::Rusty => "rusty",
            Self::RustyColor => "rusty-color",
            Self::Photocopy => "photocopy",
            Self::PhotocopyColor => "photocopy-color",
            Self::Cutout => "cutout",
            Self::CutoutColor => "cutout-color",
        }
    }

    /// Parses a persistence key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "roughen" => Some(Self::Roughen),
            "roughen-color" => Some(Self::RoughenColor),
            "rusty" => Some(Self::Rusty),
            "rusty-color" => Some(Self::RustyColor),
            "photocopy" => Some(Self::Photocopy),
            "photocopy-color" => Some(Self::PhotocopyColor),
            "cutout" => Some(Self::Cutout),
            "cutout-color" => Some(Self::CutoutColor),
            _ => None,
        }
    }

    /// Zero-based value used by uniforms and hashes.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Roughen => 0,
            Self::RoughenColor => 1,
            Self::Rusty => 2,
            Self::RustyColor => 3,
            Self::Photocopy => 4,
            Self::PhotocopyColor => 5,
            Self::Cutout => 6,
            Self::CutoutColor => 7,
        }
    }

    /// One-based authoring menu value.
    #[must_use]
    pub const fn as_menu(self) -> u32 {
        self.as_u32() + 1
    }

    /// Parses a one-based authoring menu value.
    #[must_use]
    pub fn from_menu(menu: u32) -> Option<Self> {
        match menu {
            1 => Some(Self::Roughen),
            2 => Some(Self::RoughenColor),
            3 => Some(Self::Rusty),
            4 => Some(Self::RustyColor),
            5 => Some(Self::Photocopy),
            6 => Some(Self::PhotocopyColor),
            7 => Some(Self::Cutout),
            8 => Some(Self::CutoutColor),
            _ => None,
        }
    }

    /// Zero-based styling family: raw, rusty, photocopy, cutout.
    #[must_use]
    pub const fn base(self) -> u32 {
        self.as_u32() / 2
    }

    /// Whether the mode fills the grown band with the authored edge colour.
    #[must_use]
    pub const fn uses_edge_color(self) -> bool {
        self.as_u32() % 2 == 1
    }

    /// Menu label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Roughen => "粗糙",
            Self::RoughenColor => "粗糙（着色）",
            Self::Rusty => "锈蚀",
            Self::RustyColor => "锈蚀（着色）",
            Self::Photocopy => "影印",
            Self::PhotocopyColor => "影印（着色）",
            Self::Cutout => "剪切",
            Self::CutoutColor => "剪切（着色）",
        }
    }
}

/// Failures raised when validating a roughen edges grade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoughenEdgesError {
    /// The edge type is not one of the eight menu values.
    EdgeType,
    /// The edge colour has a non-finite component or lies outside `[0, 1]`.
    EdgeColor,
    /// The border is not finite or lies outside the declared range.
    Border,
    /// The sharpness is not finite or lies outside the declared range.
    Sharpness,
    /// The fractal influence is not finite or lies outside the declared range.
    Influence,
    /// The scale is not finite or lies outside the declared range.
    Scale,
    /// The stretch is not finite or lies outside the declared range.
    Stretch,
    /// One field offset is not finite or lies outside the declared range.
    Offset,
    /// The fractal complexity lies outside the declared range.
    Complexity,
    /// The evolution is not finite or lies outside the declared range.
    Evolution,
    /// The evolution cycle lies outside the declared range.
    Cycle,
    /// The random seed lies outside the declared range.
    RandomSeed,
}

impl std::fmt::Display for RoughenEdgesError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EdgeType => formatter.write_str("roughen edges type is not a menu value"),
            Self::EdgeColor => {
                formatter.write_str("roughen edges edge colour is outside the declared range")
            }
            Self::Border => {
                formatter.write_str("roughen edges border is outside the declared range")
            }
            Self::Sharpness => {
                formatter.write_str("roughen edges sharpness is outside the declared range")
            }
            Self::Influence => {
                formatter.write_str("roughen edges influence is outside the declared range")
            }
            Self::Scale => formatter.write_str("roughen edges scale is outside the declared range"),
            Self::Stretch => {
                formatter.write_str("roughen edges stretch is outside the declared range")
            }
            Self::Offset => {
                formatter.write_str("roughen edges offset is outside the declared range")
            }
            Self::Complexity => {
                formatter.write_str("roughen edges complexity is outside the declared range")
            }
            Self::Evolution => {
                formatter.write_str("roughen edges evolution is outside the declared range")
            }
            Self::Cycle => formatter.write_str("roughen edges cycle is outside the declared range"),
            Self::RandomSeed => {
                formatter.write_str("roughen edges random seed is outside the declared range")
            }
        }
    }
}

impl std::error::Error for RoughenEdgesError {}

/// A validated roughen edges grade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoughenEdgesGrade {
    edge_type: RoughenEdgesType,
    edge_color: [f32; 3],
    border: f32,
    sharpness: f32,
    influence: f32,
    scale: f32,
    stretch: f32,
    offset: [f32; 2],
    complexity: u32,
    evolution: f32,
    cycle_evolution: bool,
    cycle: u32,
    random_seed: u32,
}

impl RoughenEdgesGrade {
    /// Validates every authored control.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        edge_type: u32,
        edge_color: [f32; 3],
        border: f32,
        sharpness: f32,
        influence: f32,
        scale: f32,
        stretch: f32,
        offset: [f32; 2],
        complexity: u32,
        evolution: f32,
        cycle_evolution: bool,
        cycle: u32,
        random_seed: u32,
    ) -> Result<Self, RoughenEdgesError> {
        let Some(edge_type) = RoughenEdgesType::from_menu(edge_type) else {
            return Err(RoughenEdgesError::EdgeType);
        };
        if edge_color
            .iter()
            .any(|channel| !channel.is_finite() || !(0.0..=1.0).contains(channel))
        {
            return Err(RoughenEdgesError::EdgeColor);
        }
        if !border.is_finite()
            || !(ROUGHEN_EDGES_MIN_BORDER..=ROUGHEN_EDGES_MAX_BORDER).contains(&border)
        {
            return Err(RoughenEdgesError::Border);
        }
        if !sharpness.is_finite()
            || !(ROUGHEN_EDGES_MIN_SHARPNESS..=ROUGHEN_EDGES_MAX_SHARPNESS).contains(&sharpness)
        {
            return Err(RoughenEdgesError::Sharpness);
        }
        if !influence.is_finite()
            || !(ROUGHEN_EDGES_MIN_INFLUENCE..=ROUGHEN_EDGES_MAX_INFLUENCE).contains(&influence)
        {
            return Err(RoughenEdgesError::Influence);
        }
        if !scale.is_finite()
            || !(ROUGHEN_EDGES_MIN_SCALE..=ROUGHEN_EDGES_MAX_SCALE).contains(&scale)
        {
            return Err(RoughenEdgesError::Scale);
        }
        if !stretch.is_finite()
            || !(ROUGHEN_EDGES_MIN_STRETCH..=ROUGHEN_EDGES_MAX_STRETCH).contains(&stretch)
        {
            return Err(RoughenEdgesError::Stretch);
        }
        if offset.iter().any(|coordinate| {
            !coordinate.is_finite() || coordinate.abs() > ROUGHEN_EDGES_MAX_OFFSET_PIXELS
        }) {
            return Err(RoughenEdgesError::Offset);
        }
        if !(ROUGHEN_EDGES_MIN_COMPLEXITY..=ROUGHEN_EDGES_MAX_COMPLEXITY).contains(&complexity) {
            return Err(RoughenEdgesError::Complexity);
        }
        if !evolution.is_finite() || evolution.abs() > ROUGHEN_EDGES_MAX_EVOLUTION_DEGREES {
            return Err(RoughenEdgesError::Evolution);
        }
        if !(ROUGHEN_EDGES_MIN_CYCLE..=ROUGHEN_EDGES_MAX_CYCLE).contains(&cycle) {
            return Err(RoughenEdgesError::Cycle);
        }
        if random_seed > ROUGHEN_EDGES_MAX_RANDOM_SEED {
            return Err(RoughenEdgesError::RandomSeed);
        }
        Ok(Self {
            edge_type,
            edge_color,
            border,
            sharpness,
            influence,
            scale,
            stretch,
            offset,
            complexity,
            evolution,
            cycle_evolution,
            cycle,
            random_seed,
        })
    }

    /// Authoring mode.
    #[must_use]
    pub fn edge_type(&self) -> RoughenEdgesType {
        self.edge_type
    }

    /// Straight-alpha colour used by the coloured modes.
    #[must_use]
    pub fn edge_color(&self) -> [f32; 3] {
        self.edge_color
    }

    /// Maximum outward growth, in pixels.
    #[must_use]
    pub fn border(&self) -> f32 {
        self.border
    }

    /// Coverage hardening applied to the sampled alpha.
    #[must_use]
    pub fn sharpness(&self) -> f32 {
        self.sharpness
    }

    /// How much the fractal field modulates the displacement.
    #[must_use]
    pub fn influence(&self) -> f32 {
        self.influence
    }

    /// Field scale, in percent.
    #[must_use]
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Horizontal field stretch, in percent.
    #[must_use]
    pub fn stretch(&self) -> f32 {
        self.stretch
    }

    /// Field offset, in pixels.
    #[must_use]
    pub fn offset(&self) -> [f32; 2] {
        self.offset
    }

    /// Number of noise octaves.
    #[must_use]
    pub fn complexity(&self) -> u32 {
        self.complexity
    }

    /// Field evolution, in degrees.
    #[must_use]
    pub fn evolution(&self) -> f32 {
        self.evolution
    }

    /// Whether evolution wraps every cycle.
    #[must_use]
    pub fn cycle_evolution(&self) -> bool {
        self.cycle_evolution
    }

    /// Evolution cycle, in revolutions.
    #[must_use]
    pub fn cycle(&self) -> u32 {
        self.cycle
    }

    /// Field random seed.
    #[must_use]
    pub fn random_seed(&self) -> u32 {
        self.random_seed
    }

    /// Reach of the kernel in source pixels for the authored border.
    #[must_use]
    pub fn halo_pixels(&self) -> u32 {
        roughen_edges_halo_pixels(self.border)
    }

    /// A zero border leaves every pixel untouched.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.border == 0.0
    }

    /// Fractal field in `[-1, 1]` at a pixel-index position.
    #[must_use]
    pub fn field(&self, x: f32, y: f32) -> f32 {
        let (u, v, salt, t) = self.coordinates(x, y);
        self.field_at(u, v, salt, t)
    }

    /// Signed outward displacement for a field sample.
    #[must_use]
    pub fn displacement(&self, field: f32) -> f32 {
        let modulated = 1.0 + (0.5 * (field + 1.0) - 1.0) * self.influence;
        self.border * modulated
    }

    /// Border, sharpness, influence and scale packed for the GPU pass.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4], [f32; 4]) {
        (
            [self.border, self.sharpness, self.influence, self.scale],
            [
                self.stretch,
                self.offset[0],
                self.offset[1],
                self.complexity as f32,
            ],
            [
                self.edge_type.as_menu() as f32,
                self.edge_color[0],
                self.edge_color[1],
                self.edge_color[2],
            ],
        )
    }

    /// Evolution controls packed for the GPU pass.
    #[must_use]
    pub fn aces_controls(&self) -> [f32; 4] {
        [
            self.evolution,
            f32::from(u8::from(self.cycle_evolution)),
            self.cycle as f32,
            self.random_seed as f32,
        ]
    }

    /// Field coordinates and animation terms for one pixel.
    fn coordinates(&self, x: f32, y: f32) -> (f32, f32, u32, f32) {
        let feature = ROUGHEN_EDGES_FEATURE_PIXELS * self.scale / 1000.0;
        let stretch_x = (1.0 + self.stretch / 100.0).max(ROUGHEN_EDGES_MIN_STRETCH_FACTOR);
        let u = (x + self.offset[0]) / (feature * stretch_x);
        let v = (y + self.offset[1]) / feature;
        let salt = ROUGHEN_EDGES_FIELD_SALT ^ self.random_seed.wrapping_mul(SEED_SALT_MULTIPLIER);
        let mut t = self.evolution / 360.0;
        if self.cycle_evolution {
            let cycle = self.cycle as f32;
            t -= cycle * (t / cycle).floor();
        }
        (u, v, salt, t)
    }

    /// Blends the two adjacent integer octaves of the fractal field.
    fn field_at(&self, u: f32, v: f32, salt: u32, t: f32) -> f32 {
        let z = t.floor();
        let weight = t - z;
        let blend = weight * weight * (3.0 - 2.0 * weight);
        let base = z as i32;
        let first = fbm(u, v, base, self.complexity, salt);
        let second = fbm(u, v, base + 1, self.complexity, salt);
        (1.0 - blend) * first + blend * second
    }

    /// Clamped central-difference alpha gradient at one pixel.
    fn gradient(
        &self,
        source: &[[f32; 4]],
        width: usize,
        height: usize,
        column: usize,
        row: usize,
    ) -> (f32, f32) {
        let left = column.saturating_sub(1);
        let right = (column + 1).min(width - 1);
        let up = row.saturating_sub(1);
        let down = (row + 1).min(height - 1);
        let row_start = row * width;
        let gradient_x = (source[row_start + right][3] - source[row_start + left][3]) * 0.5;
        let gradient_y = (source[down * width + column][3] - source[up * width + column][3]) * 0.5;
        (gradient_x, gradient_y)
    }

    /// Computes one premultiplied output pixel and its final alpha.
    fn apply_alpha(
        &self,
        source: &[[f32; 4]],
        width: usize,
        height: usize,
        column: usize,
        row: usize,
    ) -> (f32, [f32; 3]) {
        let [red, green, blue, alpha] = source[row * width + column];
        if self.is_identity() {
            return (alpha, [red, green, blue]);
        }
        let (gradient_x, gradient_y) = self.gradient(source, width, height, column, row);
        let gradient = (gradient_x * gradient_x + gradient_y * gradient_y).sqrt();
        if gradient <= ROUGHEN_EDGES_GRADIENT_EPSILON {
            return (alpha, [red, green, blue]);
        }

        let x = column as f32;
        let y = row as f32;
        let (u, v, salt, t) = self.coordinates(x, y);
        let field = self.field_at(u, v, salt, t);
        let displacement = self.displacement(field);
        let aim_x = x + displacement * (gradient_x / gradient);
        let aim_y = y + displacement * (gradient_y / gradient);
        let sampled =
            sample_bilinear_clamped(source, width, height, f64::from(aim_x), f64::from(aim_y));
        let sampled_alpha = sampled[3];

        let sharpening = (self.sharpness - 1.0) / 9.0;
        let strength = displacement / self.border;
        let hardened =
            sampled_alpha + sharpening * strength * (smoothstep01(sampled_alpha) - sampled_alpha);
        let grown_alpha = alpha.max(hardened.clamp(0.0, 1.0));
        let grown = (grown_alpha - alpha).clamp(0.0, 1.0);

        let amount = match self.edge_type.base() {
            0 => grown,
            1 => {
                let rust = value_noise(
                    u * 3.0 + 5.0,
                    v * 3.0 + 11.0,
                    t.floor() as i32,
                    salt ^ ROUGHEN_EDGES_RUST_SALT,
                );
                grown * (0.35 + 0.65 * (0.5 * (rust + 1.0)))
            }
            2 => (grown * 4.0 + 0.5).floor() / 4.0,
            _ => smoothstep01((grown - 0.35) / 0.30),
        };
        let final_alpha = (alpha + amount).clamp(0.0, 1.0);

        let inverse_alpha = sampled_alpha.max(ROUGHEN_EDGES_MIN_SAMPLE_ALPHA);
        let own = [
            sampled[0] / inverse_alpha,
            sampled[1] / inverse_alpha,
            sampled[2] / inverse_alpha,
        ];
        let fill = amount / final_alpha.max(ROUGHEN_EDGES_MIN_SAMPLE_ALPHA);
        let aim = if self.edge_type.uses_edge_color() {
            self.edge_color
        } else {
            own
        };
        let blended = [
            final_alpha * (own[0] + (aim[0] - own[0]) * fill),
            final_alpha * (own[1] + (aim[1] - own[1]) * fill),
            final_alpha * (own[2] + (aim[2] - own[2]) * fill),
        ];
        (final_alpha, blended)
    }

    /// Runs the band pass against an immutable premultiplied snapshot.
    fn apply_snapshot(
        &self,
        frame: &mut [[f32; 4]],
        source: &[[f32; 4]],
        width: usize,
        height: usize,
    ) {
        for_each_row_band(frame, width, 0..height, |first_row, band| {
            for (offset, row_pixels) in band.chunks_mut(width).enumerate() {
                let row = first_row + offset;
                for (column, destination) in row_pixels.iter_mut().enumerate() {
                    let (alpha, rgb) = self.apply_alpha(source, width, height, column, row);
                    *destination = [rgb[0], rgb[1], rgb[2], alpha];
                }
            }
        });
    }
}

/// Reach of the kernel in source pixels for a border, including the bilinear tap.
#[must_use]
pub fn roughen_edges_halo_pixels(border: f32) -> u32 {
    border.ceil() as u32 + 1
}

/// Applies a roughen edges grade to a whole straight-alpha frame.
pub fn roughen_edges_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &RoughenEdgesGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_roughen_edges_f32_controlled(pixels, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

/// Applies a roughen edges grade to a frame slice with checkpointing.
pub(crate) fn apply_roughen_edges_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &RoughenEdgesGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(expected) = width.checked_mul(height) else {
        return Ok(false);
    };
    if width == 0 || height == 0 || expected != pixels.len() {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }

    premultiply_rgba_controlled(pixels, checkpoint)?;
    let mut source = Vec::with_capacity(pixels.len());
    for chunk in pixels.chunks(CHECKPOINT_STRIDE) {
        source.extend_from_slice(chunk);
        checkpoint()?;
    }
    grade.apply_snapshot(pixels, &source, width, height);
    checkpoint()?;
    unpremultiply_rgba_controlled(pixels, checkpoint)?;
    Ok(true)
}

/// Smoothstep over `[0, 1]` with clamping outside the interval.
fn smoothstep01(value: f32) -> f32 {
    let clamped = value.clamp(0.0, 1.0);
    clamped * clamped * (3.0 - 2.0 * clamped)
}

/// One octave of value noise in `[-1, 1]`.
fn value_noise(x: f32, y: f32, z: i32, salt: u32) -> f32 {
    let cell_x = x.floor();
    let cell_y = y.floor();
    let weight_x = smoothstep01(x - cell_x);
    let weight_y = smoothstep01(y - cell_y);
    let x0 = cell_x as i32;
    let y0 = cell_y as i32;
    let top_left = lattice(x0, y0, z, salt);
    let top_right = lattice(x0 + 1, y0, z, salt);
    let bottom_left = lattice(x0, y0 + 1, z, salt);
    let bottom_right = lattice(x0 + 1, y0 + 1, z, salt);
    let top = top_left + (top_right - top_left) * weight_x;
    let bottom = bottom_left + (bottom_right - bottom_left) * weight_x;
    top + (bottom - top) * weight_y
}

/// One lattice sample, folded with the integer octave.
fn lattice(x: i32, y: i32, z: i32, salt: u32) -> f32 {
    noise_sample(
        x as u32,
        y as u32,
        salt ^ (z as u32).wrapping_mul(OCTAVE_SALT_MULTIPLIER),
    )
}

/// Fractal sum of `complexity` value-noise octaves, normalised to `[-1, 1]`.
fn fbm(u: f32, v: f32, z: i32, complexity: u32, salt: u32) -> f32 {
    let mut sum = 0.0_f32;
    let mut norm = 0.0_f32;
    let mut amplitude = 1.0_f32;
    let mut frequency = 1.0_f32;
    for octave in 0..complexity {
        let index = octave as f32;
        sum += amplitude
            * value_noise(
                u * frequency + 17.0 * index,
                v * frequency + 31.0 * index,
                z,
                salt ^ octave.wrapping_mul(FBM_SALT_MULTIPLIER),
            );
        norm += amplitude;
        amplitude *= 0.5;
        frequency *= 2.0;
    }
    sum / norm
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
    const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 0.0];
    const EDGE_COLOR: [f32; 3] = [0.6, 0.2, 0.0];

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        vec![CLEAR; width * height]
    }

    fn set(pixels: &mut [[f32; 4]], width: usize, column: usize, row: usize, pixel: [f32; 4]) {
        pixels[row * width + column] = pixel;
    }

    fn alpha(pixels: &[[f32; 4]], width: usize, column: usize, row: usize) -> f32 {
        pixels[row * width + column][3]
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= 1.0e-6,
            "{actual} is not close to {expected}"
        );
    }

    fn default_grade() -> RoughenEdgesGrade {
        RoughenEdgesGrade::new(
            1,
            EDGE_COLOR,
            2.0,
            1.0,
            0.0,
            100.0,
            0.0,
            [0.0, 0.0],
            2,
            0.0,
            false,
            1,
            0,
        )
        .expect("grade")
    }

    fn grade_with_border(border: f32) -> RoughenEdgesGrade {
        RoughenEdgesGrade::new(
            1,
            EDGE_COLOR,
            border,
            1.0,
            0.0,
            100.0,
            0.0,
            [0.0, 0.0],
            2,
            0.0,
            false,
            1,
            0,
        )
        .expect("grade")
    }

    fn opaque_block() -> (Vec<[f32; 4]>, usize, usize) {
        let width = 12;
        let height = 12;
        let mut pixels = frame(width, height);
        for row in 4..8 {
            for column in 4..8 {
                set(&mut pixels, width, column, row, WHITE);
            }
        }
        (pixels, width, height)
    }

    #[test]
    fn zero_border_is_an_identity() {
        let (mut pixels, width, height) = opaque_block();
        let original = pixels.clone();
        let grade = grade_with_border(0.0);
        assert!(grade.is_identity());
        assert!(roughen_edges_frame_f32(&mut pixels, width, height, &grade));
        assert_eq!(pixels, original);
    }

    #[test]
    fn a_straight_edge_grows_outward_by_the_border() {
        let width = 9;
        let height = 1;
        let mut pixels = frame(width, height);
        for column in 0..width {
            let coverage = 0.125 * column as f32;
            set(&mut pixels, width, column, 0, [0.3, 0.4, 0.5, coverage]);
        }
        let grade = grade_with_border(2.0);
        assert!(roughen_edges_frame_f32(&mut pixels, width, height, &grade));
        for column in 0..=6 {
            assert_close(
                alpha(&pixels, width, column, 0),
                0.125 * (column + 2) as f32,
            );
        }
        assert_close(alpha(&pixels, width, 2, 0), 0.5);
        assert_close(alpha(&pixels, width, 4, 0), 0.75);
    }

    #[test]
    fn flat_regions_are_left_untouched() {
        let (mut pixels, width, height) = opaque_block();
        let original = pixels.clone();
        let grade = default_grade();
        assert!(roughen_edges_frame_f32(&mut pixels, width, height, &grade));
        // The centre of the block and the far corner are flat alpha regions, so
        // the premultiply round trip must hand them back bit for bit.
        assert_eq!(pixels[5 * width + 5], original[5 * width + 5]);
        assert_eq!(pixels[0], original[0]);
        assert!(alpha(&pixels, width, 3, 5) > alpha(&original, width, 3, 5));
    }

    #[test]
    fn the_gradient_points_into_the_shape() {
        let width = 9;
        let height = 5;
        let mut pixels = frame(width, height);
        for row in 0..height {
            for column in 3..=5 {
                set(&mut pixels, width, column, row, WHITE);
            }
        }
        let grade = grade_with_border(2.0);
        assert!(roughen_edges_frame_f32(&mut pixels, width, height, &grade));
        // Both sides sample towards the opaque centre, so coverage grows outward.
        assert_eq!(alpha(&pixels, width, 2, 2), 1.0);
        assert_eq!(alpha(&pixels, width, 6, 2), 1.0);
        // One pixel further the gradient is already flat, so nothing changes.
        assert_eq!(alpha(&pixels, width, 1, 2), 0.0);
        assert_eq!(alpha(&pixels, width, 7, 2), 0.0);
    }

    #[test]
    fn the_displacement_never_exceeds_the_border() {
        let grade = grade_with_border(3.0);
        for field in [-1.0, -0.5, 0.0, 0.5, 1.0] {
            let displacement = grade.displacement(field);
            assert!((0.0..=3.0).contains(&displacement));
        }

        let mut modulated = RoughenEdgesGrade::new(
            1,
            EDGE_COLOR,
            3.0,
            1.0,
            1.0,
            100.0,
            0.0,
            [0.0, 0.0],
            2,
            0.0,
            false,
            1,
            0,
        )
        .expect("grade");
        for field in [-1.0, 0.0, 1.0] {
            let displacement = modulated.displacement(field);
            assert!((0.0..=3.0).contains(&displacement));
        }
        modulated = grade;
        assert_close(modulated.displacement(-1.0), 3.0);

        // The kernel itself never rewrites a pixel outside the halo.
        let (mut pixels, width, height) = opaque_block();
        let original = pixels.clone();
        let grade = grade_with_border(2.0);
        assert!(roughen_edges_frame_f32(&mut pixels, width, height, &grade));
        for row in 0..height {
            for column in 0..width {
                let inside = (4..8).contains(&column) || (4..8).contains(&row);
                if !inside {
                    assert_eq!(pixels[row * width + column], original[row * width + column]);
                }
            }
        }
    }

    #[test]
    fn the_halo_bound_covers_the_bilinear_footprint() {
        assert_eq!(roughen_edges_halo_pixels(0.0), 1);
        assert_eq!(roughen_edges_halo_pixels(0.5), 2);
        assert_eq!(roughen_edges_halo_pixels(8.0), 9);
        assert_eq!(roughen_edges_halo_pixels(500.0), 501);
        for border in [1.5_f32, 2.0, 7.25] {
            let halo = roughen_edges_halo_pixels(border);
            assert!(halo as f32 >= border + 1.0);
        }
    }

    #[test]
    fn sharpness_one_is_a_no_op() {
        let (pixels, width, _height) = sharpness_ramp();
        assert_close(alpha(&pixels, width, 1, 0), 0.25);
    }

    #[test]
    fn sharpness_ten_hardens_the_edge() {
        let mut grade = sharpness_grade();
        grade = with_sharpness(grade, 10.0);
        let (pixels, width, _height) = sharpness_ramp_with(grade);
        assert_close(alpha(&pixels, width, 1, 0), 0.15625);
    }

    #[test]
    fn sharpness_zero_softens_the_edge() {
        let grade = with_sharpness(sharpness_grade(), 0.0);
        let (pixels, width, _height) = sharpness_ramp_with(grade);
        assert_close(alpha(&pixels, width, 1, 0), 0.25 + 0.09375 / 9.0);
    }

    fn sharpness_grade() -> RoughenEdgesGrade {
        RoughenEdgesGrade::new(
            1,
            EDGE_COLOR,
            2.0,
            1.0,
            0.0,
            100.0,
            0.0,
            [0.0, 0.0],
            2,
            0.0,
            false,
            1,
            0,
        )
        .expect("grade")
    }

    fn with_sharpness(grade: RoughenEdgesGrade, sharpness: f32) -> RoughenEdgesGrade {
        RoughenEdgesGrade::new(
            grade.edge_type().as_menu(),
            grade.edge_color(),
            grade.border(),
            sharpness,
            grade.influence(),
            grade.scale(),
            grade.stretch(),
            grade.offset(),
            grade.complexity(),
            grade.evolution(),
            grade.cycle_evolution(),
            grade.cycle(),
            grade.random_seed(),
        )
        .expect("grade")
    }

    fn sharpness_ramp() -> (Vec<[f32; 4]>, usize, usize) {
        sharpness_ramp_with(sharpness_grade())
    }

    fn sharpness_ramp_with(grade: RoughenEdgesGrade) -> (Vec<[f32; 4]>, usize, usize) {
        let width = 9;
        let height = 1;
        let mut pixels = frame(width, height);
        for column in 0..width {
            let coverage = 0.1 + 0.05 * column as f32;
            set(&mut pixels, width, column, 0, [0.3, 0.4, 0.5, coverage]);
        }
        assert!(roughen_edges_frame_f32(&mut pixels, width, height, &grade));
        (pixels, width, height)
    }

    #[test]
    fn the_rusty_mode_pits_the_grown_edge() {
        let (plain, width, height) = grown_square(1);
        let (rusty, _, _) = grown_square(3);
        let mut strictly_pitted = false;
        for index in 0..width * height {
            assert!(rusty[index][3] <= plain[index][3] + 1.0e-6);
            assert!(rusty[index][3] >= 0.35 * plain[index][3] - 1.0e-6);
            if rusty[index][3] < plain[index][3] - 1.0e-4 {
                strictly_pitted = true;
            }
        }
        assert!(strictly_pitted, "the pitting field must dim some pixels");
    }

    #[test]
    fn the_photocopy_mode_quantises_the_grown_edge() {
        let width = 16;
        let height = 16;
        let mut pixels = frame(width, height);
        set(&mut pixels, width, 7, 7, WHITE);
        let grade = RoughenEdgesGrade::new(
            5,
            EDGE_COLOR,
            1.5,
            1.0,
            0.0,
            100.0,
            0.0,
            [0.0, 0.0],
            2,
            0.0,
            false,
            1,
            0,
        )
        .expect("grade");
        assert!(roughen_edges_frame_f32(&mut pixels, width, height, &grade));
        let mut partial = false;
        for pixel in &pixels {
            let steps = pixel[3] * 4.0;
            assert!((steps - steps.round()).abs() <= 1.0e-5);
            if pixel[3] > 0.0 && pixel[3] < 1.0 {
                assert!((pixel[3] - 0.5).abs() <= 1.0e-5);
                partial = true;
            }
        }
        assert!(
            partial,
            "the quantised edge must keep some intermediate steps"
        );
    }

    #[test]
    fn the_cutout_mode_binarises_the_grown_edge() {
        let width = 9;
        let height = 1;
        let mut pixels = frame(width, height);
        for (column, coverage) in
            [0.0, 0.0, 0.8, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0].into_iter().enumerate()
        {
            set(&mut pixels, width, column, 0, [0.3, 0.4, 0.5, coverage]);
        }
        let grade = RoughenEdgesGrade::new(
            7,
            EDGE_COLOR,
            1.0,
            1.0,
            0.0,
            100.0,
            0.0,
            [0.0, 0.0],
            2,
            0.0,
            false,
            1,
            0,
        )
        .expect("grade");
        assert!(roughen_edges_frame_f32(&mut pixels, width, height, &grade));
        // A half-covered edge crosses the cutout threshold and snaps to full coverage.
        assert_eq!(alpha(&pixels, width, 1, 0), 1.0);
        assert_eq!(alpha(&pixels, width, 0, 0), 0.0);
    }

    #[test]
    fn the_coloured_variants_fill_with_the_edge_colour() {
        let (plain, width) = coloured_square(1);
        let (coloured, _) = coloured_square(2);
        let grown = plain[2 * width + 2];
        assert_eq!(grown, WHITE);
        let pixel = coloured[2 * width + 2];
        assert!((pixel[0] - 0.6).abs() <= 1.0e-6);
        assert!((pixel[1] - 0.2).abs() <= 1.0e-6);
        assert_eq!(pixel[2], 0.0);
        assert_eq!(pixel[3], 1.0);
    }

    #[test]
    fn the_monochrome_variants_ignore_the_edge_colour() {
        let (plain, width) = coloured_square(1);
        let pixel = plain[2 * width + 2];
        assert_eq!(pixel[3], 1.0);
        assert_eq!([pixel[0], pixel[1], pixel[2]], [1.0, 1.0, 1.0]);
        assert_ne!([pixel[0], pixel[1], pixel[2]], EDGE_COLOR);
    }

    fn coloured_square(edge_type: u32) -> (Vec<[f32; 4]>, usize) {
        let width = 9;
        let height = 5;
        let mut pixels = frame(width, height);
        for row in 0..height {
            for column in 3..=5 {
                set(&mut pixels, width, column, row, WHITE);
            }
        }
        let grade = RoughenEdgesGrade::new(
            edge_type,
            EDGE_COLOR,
            2.0,
            1.0,
            0.0,
            100.0,
            0.0,
            [0.0, 0.0],
            2,
            0.0,
            false,
            1,
            0,
        )
        .expect("grade");
        assert!(roughen_edges_frame_f32(&mut pixels, width, height, &grade));
        (pixels, width)
    }

    fn grown_square(edge_type: u32) -> (Vec<[f32; 4]>, usize, usize) {
        let width = 16;
        let height = 16;
        let mut pixels = frame(width, height);
        for row in 5..11 {
            for column in 5..11 {
                set(&mut pixels, width, column, row, WHITE);
            }
        }
        let grade = RoughenEdgesGrade::new(
            edge_type,
            EDGE_COLOR,
            3.0,
            1.0,
            1.0,
            100.0,
            0.0,
            [0.0, 0.0],
            2,
            0.0,
            false,
            1,
            0,
        )
        .expect("grade");
        assert!(roughen_edges_frame_f32(&mut pixels, width, height, &grade));
        (pixels, width, height)
    }

    #[test]
    fn evolution_rotates_the_field() {
        let first = field_grade(0.0);
        let second = field_grade(90.0);
        assert_ne!(
            first.field(3.0, 4.0).to_bits(),
            second.field(3.0, 4.0).to_bits()
        );
        let ramp = |evolution: f32| {
            let width = 41;
            let height = 1;
            let mut pixels = frame(width, height);
            for (column, pixel) in pixels.iter_mut().enumerate() {
                *pixel = [0.2, 0.3, 0.4, column as f32 / 40.0];
            }
            let grade = RoughenEdgesGrade::new(
                1,
                EDGE_COLOR,
                6.0,
                1.0,
                1.0,
                100.0,
                0.0,
                [0.0, 0.0],
                3,
                evolution,
                false,
                1,
                0,
            )
            .expect("grade");
            assert!(roughen_edges_frame_f32(&mut pixels, width, height, &grade));
            pixels
        };
        assert_ne!(ramp(0.0), ramp(90.0));
    }

    #[test]
    fn cycle_evolution_wraps_after_one_cycle() {
        let zero = field_grade(0.0);
        let wrapped = RoughenEdgesGrade::new(
            1,
            EDGE_COLOR,
            6.0,
            1.0,
            1.0,
            100.0,
            0.0,
            [0.0, 0.0],
            3,
            360.0,
            true,
            1,
            0,
        )
        .expect("grade");
        assert_close(wrapped.field(3.0, 4.0), zero.field(3.0, 4.0));
        assert_close(wrapped.field(7.0, 1.0), zero.field(7.0, 1.0));
        let two_cycles = RoughenEdgesGrade::new(
            1,
            EDGE_COLOR,
            6.0,
            1.0,
            1.0,
            100.0,
            0.0,
            [0.0, 0.0],
            3,
            720.0,
            true,
            2,
            0,
        )
        .expect("grade");
        let one_cycle = RoughenEdgesGrade::new(
            1,
            EDGE_COLOR,
            6.0,
            1.0,
            1.0,
            100.0,
            0.0,
            [0.0, 0.0],
            3,
            360.0,
            true,
            1,
            0,
        )
        .expect("grade");
        assert_close(two_cycles.field(3.0, 4.0), zero.field(3.0, 4.0));
        assert_close(one_cycle.field(3.0, 4.0), zero.field(3.0, 4.0));
        let unwrapped = RoughenEdgesGrade::new(
            1,
            EDGE_COLOR,
            6.0,
            1.0,
            1.0,
            100.0,
            0.0,
            [0.0, 0.0],
            3,
            360.0,
            false,
            1,
            0,
        )
        .expect("grade");
        assert_ne!(unwrapped.field(3.0, 4.0), zero.field(3.0, 4.0));
    }

    #[test]
    fn the_random_seed_changes_the_field() {
        let first = RoughenEdgesGrade::new(
            1,
            EDGE_COLOR,
            6.0,
            1.0,
            1.0,
            100.0,
            0.0,
            [0.0, 0.0],
            3,
            0.0,
            false,
            1,
            0,
        )
        .expect("grade");
        let second = RoughenEdgesGrade::new(
            1,
            EDGE_COLOR,
            6.0,
            1.0,
            1.0,
            100.0,
            0.0,
            [0.0, 0.0],
            3,
            0.0,
            false,
            1,
            1,
        )
        .expect("grade");
        assert_ne!(first.field(3.0, 4.0), second.field(3.0, 4.0));
    }

    #[test]
    fn the_field_stays_within_the_unit_range() {
        for scale in [10.0_f32, 100.0, 1000.0] {
            for stretch in [-100.0_f32, 0.0, 100.0] {
                for complexity in [1_u32, 2, 10] {
                    for evolution in [-36_000.0_f32, 0.0, 36_000.0] {
                        for random_seed in [0_u32, 100_000] {
                            for offset in [[-1000.0_f32, -1000.0], [1000.0, 1000.0]] {
                                let grade = RoughenEdgesGrade::new(
                                    1,
                                    EDGE_COLOR,
                                    6.0,
                                    1.0,
                                    1.0,
                                    scale,
                                    stretch,
                                    offset,
                                    complexity,
                                    evolution,
                                    true,
                                    3,
                                    random_seed,
                                )
                                .expect("grade");
                                for (x, y) in
                                    [(-5.0_f32, 0.0_f32), (0.0, 0.0), (7.5, 2.25), (100.0, -3.0)]
                                {
                                    let field = grade.field(x, y);
                                    assert!(field.is_finite());
                                    assert!(field.abs() <= 1.0 + 1.0e-6, "{field}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_kernel_is_deterministic() {
        let grade = field_grade(0.0);
        let (first, _, _) = grown_square_with(&grade);
        let (second, _, _) = grown_square_with(&grade);
        assert_eq!(first, second);
    }

    fn field_grade(evolution: f32) -> RoughenEdgesGrade {
        RoughenEdgesGrade::new(
            1,
            EDGE_COLOR,
            6.0,
            1.0,
            1.0,
            100.0,
            0.0,
            [0.0, 0.0],
            3,
            evolution,
            false,
            1,
            0,
        )
        .expect("grade")
    }

    fn grown_square_with(grade: &RoughenEdgesGrade) -> (Vec<[f32; 4]>, usize, usize) {
        let width = 24;
        let height = 24;
        let mut pixels = frame(width, height);
        for row in 7..17 {
            for column in 7..17 {
                set(&mut pixels, width, column, row, WHITE);
            }
        }
        assert!(roughen_edges_frame_f32(&mut pixels, width, height, grade));
        (pixels, width, height)
    }

    #[test]
    fn gpu_controls_packs_the_documented_cells() {
        let grade = RoughenEdgesGrade::new(
            2,
            EDGE_COLOR,
            8.0,
            1.5,
            0.25,
            200.0,
            -40.0,
            [3.0, -4.0],
            4,
            90.0,
            true,
            3,
            7,
        )
        .expect("grade");
        assert_eq!(grade.gpu_controls().0, [8.0, 1.5, 0.25, 200.0]);
        assert_eq!(grade.gpu_controls().1, [-40.0, 3.0, -4.0, 4.0]);
        assert_eq!(grade.gpu_controls().2, [2.0, 0.6, 0.2, 0.0]);
        assert_eq!(grade.aces_controls(), [90.0, 1.0, 3.0, 7.0]);
    }

    #[test]
    fn menu_and_key_round_trips() {
        for (index, edge_type) in RoughenEdgesType::ALL.into_iter().enumerate() {
            assert_eq!(edge_type.as_u32(), index as u32);
            assert_eq!(edge_type.as_menu(), index as u32 + 1);
            assert_eq!(
                RoughenEdgesType::from_key(edge_type.as_key()),
                Some(edge_type)
            );
            assert_eq!(
                RoughenEdgesType::from_menu(edge_type.as_menu()),
                Some(edge_type)
            );
            assert!(!edge_type.label().is_empty());
        }
        assert_eq!(RoughenEdgesType::from_menu(0), None);
        assert_eq!(RoughenEdgesType::from_menu(9), None);
        assert_eq!(RoughenEdgesType::from_key("missing"), None);
        assert_eq!(RoughenEdgesType::Roughen.base(), 0);
        assert_eq!(RoughenEdgesType::RustyColor.base(), 1);
        assert_eq!(RoughenEdgesType::PhotocopyColor.base(), 2);
        assert_eq!(RoughenEdgesType::CutoutColor.base(), 3);
        assert!(RoughenEdgesType::CutoutColor.uses_edge_color());
        assert!(!RoughenEdgesType::Cutout.uses_edge_color());
    }

    #[derive(Clone, Copy)]
    struct Controls {
        edge_type: u32,
        edge_color: [f32; 3],
        border: f32,
        sharpness: f32,
        influence: f32,
        scale: f32,
        stretch: f32,
        offset: [f32; 2],
        complexity: u32,
        evolution: f32,
        cycle_evolution: bool,
        cycle: u32,
        random_seed: u32,
    }

    impl Controls {
        fn valid() -> Self {
            Self {
                edge_type: 1,
                edge_color: EDGE_COLOR,
                border: 8.0,
                sharpness: 1.0,
                influence: 1.0,
                scale: 100.0,
                stretch: 0.0,
                offset: [0.0, 0.0],
                complexity: 2,
                evolution: 0.0,
                cycle_evolution: false,
                cycle: 1,
                random_seed: 0,
            }
        }

        fn build(self) -> Result<RoughenEdgesGrade, RoughenEdgesError> {
            RoughenEdgesGrade::new(
                self.edge_type,
                self.edge_color,
                self.border,
                self.sharpness,
                self.influence,
                self.scale,
                self.stretch,
                self.offset,
                self.complexity,
                self.evolution,
                self.cycle_evolution,
                self.cycle,
                self.random_seed,
            )
        }
    }

    #[test]
    fn out_of_range_parameters_are_rejected_per_parameter() {
        assert_eq!(Controls::valid().build().map(|_| ()), Ok(()));

        let mut controls = Controls::valid();
        controls.edge_type = 0;
        assert_eq!(controls.build(), Err(RoughenEdgesError::EdgeType));
        controls.edge_type = 9;
        assert_eq!(controls.build(), Err(RoughenEdgesError::EdgeType));

        let mut controls = Controls::valid();
        controls.edge_color = [1.5, 0.0, 0.0];
        assert_eq!(controls.build(), Err(RoughenEdgesError::EdgeColor));
        controls.edge_color = [f32::NAN, 0.0, 0.0];
        assert_eq!(controls.build(), Err(RoughenEdgesError::EdgeColor));

        let mut controls = Controls::valid();
        controls.border = -1.0;
        assert_eq!(controls.build(), Err(RoughenEdgesError::Border));
        controls.border = ROUGHEN_EDGES_MAX_BORDER + 1.0;
        assert_eq!(controls.build(), Err(RoughenEdgesError::Border));

        let mut controls = Controls::valid();
        controls.sharpness = ROUGHEN_EDGES_MAX_SHARPNESS + 0.5;
        assert_eq!(controls.build(), Err(RoughenEdgesError::Sharpness));

        let mut controls = Controls::valid();
        controls.influence = -0.1;
        assert_eq!(controls.build(), Err(RoughenEdgesError::Influence));
        controls.influence = 1.1;
        assert_eq!(controls.build(), Err(RoughenEdgesError::Influence));

        let mut controls = Controls::valid();
        controls.scale = 9.0;
        assert_eq!(controls.build(), Err(RoughenEdgesError::Scale));
        controls.scale = 1001.0;
        assert_eq!(controls.build(), Err(RoughenEdgesError::Scale));

        let mut controls = Controls::valid();
        controls.stretch = -101.0;
        assert_eq!(controls.build(), Err(RoughenEdgesError::Stretch));
        controls.stretch = 101.0;
        assert_eq!(controls.build(), Err(RoughenEdgesError::Stretch));

        let mut controls = Controls::valid();
        controls.offset = [1001.0, 0.0];
        assert_eq!(controls.build(), Err(RoughenEdgesError::Offset));

        let mut controls = Controls::valid();
        controls.complexity = 0;
        assert_eq!(controls.build(), Err(RoughenEdgesError::Complexity));
        controls.complexity = 11;
        assert_eq!(controls.build(), Err(RoughenEdgesError::Complexity));

        let mut controls = Controls::valid();
        controls.evolution = 36_001.0;
        assert_eq!(controls.build(), Err(RoughenEdgesError::Evolution));

        let mut controls = Controls::valid();
        controls.cycle = 0;
        assert_eq!(controls.build(), Err(RoughenEdgesError::Cycle));
        controls.cycle = 89;
        assert_eq!(controls.build(), Err(RoughenEdgesError::Cycle));

        let mut controls = Controls::valid();
        controls.random_seed = 100_001;
        assert_eq!(controls.build(), Err(RoughenEdgesError::RandomSeed));
    }

    #[test]
    fn mismatched_frames_are_reported_as_unsupported() {
        let grade = default_grade();
        let mut pixels = frame(3, 3);
        assert!(!roughen_edges_frame_f32(&mut pixels, 2, 3, &grade));
        assert!(!roughen_edges_frame_f32(&mut pixels, 0, 3, &grade));
        assert!(!roughen_edges_frame_f32(&mut pixels, 3, 0, &grade));
        assert!(!roughen_edges_frame_f32(&mut pixels, usize::MAX, 2, &grade));
    }
}
