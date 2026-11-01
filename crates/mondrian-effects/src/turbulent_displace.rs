//! DST-01 湍流置换 · turbulent displace.
//!
//! A fractal value-noise displacement: the field either pushes pixels along the
//! noise gradient, along the frame radius, along the tangent, or along a single
//! axis. Heat haze, water, waving flags and melting all come out of the same
//! field, so one kernel serves every menu entry.
//!
//! # Coordinates
//!
//! Every coordinate in this module is a **pixel centre** coordinate: `(0, 0)` is
//! the centre of the top-left pixel and `(width - 1, height - 1)` is the centre
//! of the bottom-right one.
//!
//! # Noise field
//!
//! The field is sampled in the fractional domain
//!
//! ```text
//! u = (x - offset.x) / size
//! v = (y - offset.y) / size
//! ```
//!
//! so `size` is the noise cell size in pixels. Octave `i` in `0..complexity`
//! samples `value_noise(u * 2^i, v * 2^i, salt_i)` with
//!
//! ```text
//! salt_i = seed_hash ^ (i * 0x9E37_79B9)
//! seed_hash = 0x85EB_CA6B ^ random_seed
//! ```
//!
//! so seed `0` still produces a stable pattern. The octaves are summed with the
//! weights `0.5^(i+1)` and divided by their sum, which keeps the field inside
//! `[-1, 1]`.
//!
//! `value_noise` interpolates the four integer-cell corner hashes from
//! [`crate::noise::noise_hash`], each mapped to `[-1, 1]` exactly like
//! [`crate::noise::noise_sample`]: `bits = (hash >> 8) & 0xFFFF` and
//! `value = bits / 65535 * 2 - 1`. The three `*_smoother` variants swap the
//! bicubic weights `t*t*(3-2t)` for the quintic `t*t*t*(t*(t*6-15)+10)` and halve
//! the octave count (`max(1, complexity / 2)`), which visibly smooths the warp.
//!
//! # Evolution
//!
//! A domain offset is added to `(u, v)`. Without `cycle_evolution` it is
//! `(evolution * 2, evolution * 0.618)`. With `cycle_evolution` it is
//! `(cos(theta), sin(theta)) * size_of_evolution` with
//! `theta = TAU * evolution * cycle`, so the pattern loops every `1 / cycle` of
//! evolution.
//!
//! # Displacement
//!
//! With `c` the frame centre, `r = p - c`,
//! `radial = r / max(|r|, 1e-6)` and `tangent = (-radial.y, radial.x)`:
//!
//! * `Turbulent` — `amount * 0.5 * (f(u+h,v) - f(u-h,v), f(u,v+h) - f(u,v-h)) / (2h)`
//!   with `h = 0.001`, i.e. the gradient of the field in domain units, so the
//!   warp follows the noise contour (four field evaluations). The `0.5` keeps a
//!   full swing of the noise (`df/du ≈ 2`) at `amount` pixels; the quotient is
//!   deliberately *not* multiplied by `size`, because one domain unit is `size`
//!   pixels wide and the default grade would then smear the frame by thousands
//!   of pixels instead of the tens of pixels `amount` promises.
//! * `Bulge` — `radial * amount * f(u, v)`.
//! * `Twist` — `tangent * amount * f(u, v)`.
//! * `TurbulentSmoother` / `BulgeSmoother` / `TwistSmoother` — the same three
//!   formulas evaluated with the quintic field.
//! * `Vertical` — `(0, amount * f(u, v))`.
//! * `Horizontal` — `(amount * f(u, v), 0)`.
//! * `Cross` — `amount * (f(u, v), f(u + 5.2, v + 1.3))`, two decorrelated
//!   fields, one per axis.
//!
//! `pin` damps the offset near the edges with the same geometry as
//! [`crate::wave_warp`] (normalized coordinates, a `0.2` margin and
//! `smoothstep01` ramps). `resize_layer` blends the clamped sample with a
//! mirrored one in premultiplied space. `antialias` takes one sample (`Low`) or
//! four samples per axis (`High`) at `(i + 0.5) / 4 - 0.5`.
//!
//! The identity is `amount == 0.0`: the graph builder elides the node, while the
//! kernel still reports the frame as supported and returns without touching it.
//!
//! # Precision
//!
//! The whole field and the whole displacement are computed in `f32` on both the
//! CPU and the GPU. WGSL has no `f64`, so CPU `f64` maths would round differently
//! from the shader and drift past the `1e-4` parity budget. The coordinate is
//! widened to `f64` only when it is handed to
//! [`crate::resample::sample_bilinear_clamped`], which owns the bilinear taps.

use crate::noise::noise_sample;
use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Largest supported displacement, in pixels.
pub const TURBULENT_MAX_AMOUNT: f32 = 10_000.0;
/// Smallest supported displacement, in pixels.
pub const TURBULENT_MIN_AMOUNT: f32 = -10_000.0;
/// Smallest supported noise cell size, in pixels.
pub const TURBULENT_MIN_SIZE: f32 = 2.0;
/// Largest supported noise cell size, in pixels.
pub const TURBULENT_MAX_SIZE: f32 = 1_000.0;
/// Smallest supported octave count.
pub const TURBULENT_MIN_COMPLEXITY: u32 = 1;
/// Largest supported octave count.
pub const TURBULENT_MAX_COMPLEXITY: u32 = 10;
/// Smallest supported cycle count.
pub const TURBULENT_MIN_CYCLE: u32 = 1;
/// Largest supported cycle count.
pub const TURBULENT_MAX_CYCLE: u32 = 88;
/// Largest supported random seed.
pub const TURBULENT_MAX_RANDOM_SEED: u32 = 100_000;
/// Largest supported domain offset, in pixels.
pub const TURBULENT_MAX_OFFSET_PIXELS: f32 = 1_000_000.0;
/// Number of displacement types.
pub const TURBULENT_TYPE_COUNT: usize = 9;
/// Number of pinning modes.
pub const TURBULENT_PIN_COUNT: usize = 9;
/// Number of antialias levels.
pub const TURBULENT_ANTIALIAS_COUNT: usize = 2;

/// Salt applied to the random seed so seed `0` is still a stable pattern.
const TURBULENT_SEED_BASE: u32 = 0x85EB_CA6B;
/// Salt step between octaves.
const TURBULENT_OCTAVE_SALT_STEP: u32 = 0x9E37_79B9;
/// Finite-difference step used by the gradient displacement, in domain units.
const TURBULENT_GRADIENT_STEP: f32 = 0.001;
/// Normalized evolution drift of a non-cycling field.
const TURBULENT_EVOLUTION_DRIFT: [f32; 2] = [2.0, 0.618];
/// Decorrelating domain offset of the second `Cross` field.
const TURBULENT_CROSS_OFFSET: [f32; 2] = [5.2, 1.3];
/// Width of the pinning ramp, in normalized frame units.
const TURBULENT_PIN_MARGIN: f32 = 0.2;
/// Centre-pin radius, as a fraction of the shorter frame side.
const TURBULENT_CENTRE_PIN_FRACTION: f32 = 0.25;

/// Which family of displacement the field feeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DisplacementType {
    /// Push pixels along the noise gradient.
    Turbulent,
    /// Push pixels along the frame radius.
    Bulge,
    /// Push pixels along the frame tangent.
    Twist,
    /// [`DisplacementType::Turbulent`] with the quintic field.
    TurbulentSmoother,
    /// [`DisplacementType::Bulge`] with the quintic field.
    BulgeSmoother,
    /// [`DisplacementType::Twist`] with the quintic field.
    TwistSmoother,
    /// Push pixels vertically only.
    Vertical,
    /// Push pixels horizontally only.
    Horizontal,
    /// Push each axis with its own decorrelated field.
    Cross,
}

impl DisplacementType {
    /// Every displacement type, in menu order.
    pub const ALL: [DisplacementType; TURBULENT_TYPE_COUNT] = [
        Self::Turbulent,
        Self::Bulge,
        Self::Twist,
        Self::TurbulentSmoother,
        Self::BulgeSmoother,
        Self::TwistSmoother,
        Self::Vertical,
        Self::Horizontal,
        Self::Cross,
    ];

    /// Stable serialization key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Turbulent => "turbulent",
            Self::Bulge => "bulge",
            Self::Twist => "twist",
            Self::TurbulentSmoother => "turbulent_smoother",
            Self::BulgeSmoother => "bulge_smoother",
            Self::TwistSmoother => "twist_smoother",
            Self::Vertical => "vertical",
            Self::Horizontal => "horizontal",
            Self::Cross => "cross",
        }
    }

    /// Resolve a stable serialization key.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "turbulent" => Some(Self::Turbulent),
            "bulge" => Some(Self::Bulge),
            "twist" => Some(Self::Twist),
            "turbulent_smoother" => Some(Self::TurbulentSmoother),
            "bulge_smoother" => Some(Self::BulgeSmoother),
            "twist_smoother" => Some(Self::TwistSmoother),
            "vertical" => Some(Self::Vertical),
            "horizontal" => Some(Self::Horizontal),
            "cross" => Some(Self::Cross),
            _ => None,
        }
    }

    /// Zero-based index handed to the shader.
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Turbulent => 0,
            Self::Bulge => 1,
            Self::Twist => 2,
            Self::TurbulentSmoother => 3,
            Self::BulgeSmoother => 4,
            Self::TwistSmoother => 5,
            Self::Vertical => 6,
            Self::Horizontal => 7,
            Self::Cross => 8,
        }
    }

    /// One-based index shown in the UI menu.
    pub fn as_menu(self) -> u32 {
        self.as_u32() + 1
    }

    /// Chinese menu label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Turbulent => "湍流",
            Self::Bulge => "凸出",
            Self::Twist => "扭转",
            Self::TurbulentSmoother => "湍流较平滑",
            Self::BulgeSmoother => "凸出较平滑",
            Self::TwistSmoother => "扭转较平滑",
            Self::Vertical => "垂直置换",
            Self::Horizontal => "水平置换",
            Self::Cross => "交叉置换",
        }
    }
}

/// Which part of the frame the displacement is pinned to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DisplacePin {
    /// No damping anywhere.
    None,
    /// Damp on every edge.
    AllEdges,
    /// Damp away from the frame centre.
    Center,
    /// Damp on the left edge only.
    LeftEdge,
    /// Damp on the top edge only.
    TopEdge,
    /// Damp on the right edge only.
    RightEdge,
    /// Damp on the bottom edge only.
    BottomEdge,
    /// Damp on the top and bottom edges.
    HorizontalEdges,
    /// Damp on the left and right edges.
    VerticalEdges,
}

impl DisplacePin {
    /// Every pinning mode, in menu order.
    pub const ALL: [DisplacePin; TURBULENT_PIN_COUNT] = [
        Self::None,
        Self::AllEdges,
        Self::Center,
        Self::LeftEdge,
        Self::TopEdge,
        Self::RightEdge,
        Self::BottomEdge,
        Self::HorizontalEdges,
        Self::VerticalEdges,
    ];

    /// Stable serialization key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::AllEdges => "all_edges",
            Self::Center => "center",
            Self::LeftEdge => "left_edge",
            Self::TopEdge => "top_edge",
            Self::RightEdge => "right_edge",
            Self::BottomEdge => "bottom_edge",
            Self::HorizontalEdges => "horizontal_edges",
            Self::VerticalEdges => "vertical_edges",
        }
    }

    /// Resolve a stable serialization key.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "none" => Some(Self::None),
            "all_edges" => Some(Self::AllEdges),
            "center" => Some(Self::Center),
            "left_edge" => Some(Self::LeftEdge),
            "top_edge" => Some(Self::TopEdge),
            "right_edge" => Some(Self::RightEdge),
            "bottom_edge" => Some(Self::BottomEdge),
            "horizontal_edges" => Some(Self::HorizontalEdges),
            "vertical_edges" => Some(Self::VerticalEdges),
            _ => None,
        }
    }

    /// Zero-based index handed to the shader.
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::None => 0,
            Self::AllEdges => 1,
            Self::Center => 2,
            Self::LeftEdge => 3,
            Self::TopEdge => 4,
            Self::RightEdge => 5,
            Self::BottomEdge => 6,
            Self::HorizontalEdges => 7,
            Self::VerticalEdges => 8,
        }
    }

    /// One-based index shown in the UI menu.
    pub fn as_menu(self) -> u32 {
        self.as_u32() + 1
    }

    /// Chinese menu label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "无",
            Self::AllEdges => "所有边缘",
            Self::Center => "中心",
            Self::LeftEdge => "左侧边缘",
            Self::TopEdge => "顶部边缘",
            Self::RightEdge => "右侧边缘",
            Self::BottomEdge => "底部边缘",
            Self::HorizontalEdges => "水平边缘",
            Self::VerticalEdges => "垂直边缘",
        }
    }
}

/// How many samples the kernel takes per axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DisplaceAntialias {
    /// One sample per axis.
    Low,
    /// Four samples per axis.
    High,
}

impl DisplaceAntialias {
    /// Every antialias level, in menu order.
    pub const ALL: [DisplaceAntialias; TURBULENT_ANTIALIAS_COUNT] = [Self::Low, Self::High];

    /// Stable serialization key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::High => "high",
        }
    }

    /// Resolve a stable serialization key.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "low" => Some(Self::Low),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    /// Zero-based index handed to the shader.
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Low => 0,
            Self::High => 1,
        }
    }

    /// One-based index shown in the UI menu.
    pub fn as_menu(self) -> u32 {
        self.as_u32() + 1
    }

    /// Number of samples taken along each axis.
    pub const fn samples_per_axis(self) -> u32 {
        match self {
            Self::Low => 1,
            Self::High => 4,
        }
    }

    /// Chinese menu label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Low => "低",
            Self::High => "高",
        }
    }
}

/// A rejected turbulent displace authoring value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TurbulentDisplaceError {
    /// The displacement type is not a declared menu entry.
    Displacement,
    /// The displacement amount is not finite or outside its range.
    Amount,
    /// The noise cell size is not finite or outside its range.
    Size,
    /// A domain offset is not finite or outside its range.
    Offset,
    /// The octave count is outside its range.
    Complexity,
    /// The evolution is not finite.
    Evolution,
    /// The cycle count is outside its range.
    Cycle,
    /// The random seed is outside its range.
    RandomSeed,
    /// The resize layer is not finite or outside its range.
    ResizeLayer,
}

impl std::fmt::Display for TurbulentDisplaceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Displacement => "turbulent displace displacement must be a declared type",
            Self::Amount => {
                "turbulent displace amount must be a finite value inside [-10000, 10000]"
            }
            Self::Size => "turbulent displace size must be a finite value inside [2, 1000]",
            Self::Offset => {
                "turbulent displace offset must be finite values inside [-1000000, 1000000]"
            }
            Self::Complexity => "turbulent displace complexity must be inside [1, 10]",
            Self::Evolution => "turbulent displace evolution must be a finite value",
            Self::Cycle => "turbulent displace cycle must be inside [1, 88]",
            Self::RandomSeed => "turbulent displace random seed must be inside [0, 100000]",
            Self::ResizeLayer => {
                "turbulent displace resize layer must be a finite value inside [0, 1]"
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for TurbulentDisplaceError {}

/// A validated turbulent displace configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TurbulentDisplaceGrade {
    displacement: DisplacementType,
    amount: f32,
    size: f32,
    offset: [f32; 2],
    complexity: u32,
    evolution: f32,
    cycle_evolution: bool,
    cycle: u32,
    random_seed: u32,
    pin: DisplacePin,
    resize_layer: f32,
    antialias: DisplaceAntialias,
}

impl TurbulentDisplaceGrade {
    /// Validate every authored cell.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        displacement: DisplacementType,
        amount: f32,
        size: f32,
        offset: [f32; 2],
        complexity: u32,
        evolution: f32,
        cycle_evolution: bool,
        cycle: u32,
        random_seed: u32,
        pin: DisplacePin,
        resize_layer: f32,
        antialias: DisplaceAntialias,
    ) -> Result<Self, TurbulentDisplaceError> {
        validate_displacement(displacement.as_u32())?;
        if !amount.is_finite() || !(TURBULENT_MIN_AMOUNT..=TURBULENT_MAX_AMOUNT).contains(&amount) {
            return Err(TurbulentDisplaceError::Amount);
        }
        if !size.is_finite() || !(TURBULENT_MIN_SIZE..=TURBULENT_MAX_SIZE).contains(&size) {
            return Err(TurbulentDisplaceError::Size);
        }
        for component in offset {
            if !component.is_finite()
                || !(-TURBULENT_MAX_OFFSET_PIXELS..=TURBULENT_MAX_OFFSET_PIXELS)
                    .contains(&component)
            {
                return Err(TurbulentDisplaceError::Offset);
            }
        }
        if !(TURBULENT_MIN_COMPLEXITY..=TURBULENT_MAX_COMPLEXITY).contains(&complexity) {
            return Err(TurbulentDisplaceError::Complexity);
        }
        if !evolution.is_finite() {
            return Err(TurbulentDisplaceError::Evolution);
        }
        if !(TURBULENT_MIN_CYCLE..=TURBULENT_MAX_CYCLE).contains(&cycle) {
            return Err(TurbulentDisplaceError::Cycle);
        }
        if random_seed > TURBULENT_MAX_RANDOM_SEED {
            return Err(TurbulentDisplaceError::RandomSeed);
        }
        if !resize_layer.is_finite() || !(0.0..=1.0).contains(&resize_layer) {
            return Err(TurbulentDisplaceError::ResizeLayer);
        }
        Ok(Self {
            displacement,
            amount,
            size,
            offset,
            complexity,
            evolution,
            cycle_evolution,
            cycle,
            random_seed,
            pin,
            resize_layer,
            antialias,
        })
    }

    /// The displacement family.
    pub const fn displacement(&self) -> DisplacementType {
        self.displacement
    }

    /// The displacement amount, in pixels.
    pub const fn amount(&self) -> f32 {
        self.amount
    }

    /// The noise cell size, in pixels.
    pub const fn size(&self) -> f32 {
        self.size
    }

    /// The domain offset, in pixels.
    pub const fn offset(&self) -> [f32; 2] {
        self.offset
    }

    /// The octave count.
    pub const fn complexity(&self) -> u32 {
        self.complexity
    }

    /// The evolution phase.
    pub const fn evolution(&self) -> f32 {
        self.evolution
    }

    /// Whether evolution cycles the field instead of drifting it.
    pub const fn cycle_evolution(&self) -> bool {
        self.cycle_evolution
    }

    /// The number of cycles per unit of evolution.
    pub const fn cycle(&self) -> u32 {
        self.cycle
    }

    /// The random seed.
    pub const fn random_seed(&self) -> u32 {
        self.random_seed
    }

    /// The pinning mode.
    pub const fn pin(&self) -> DisplacePin {
        self.pin
    }

    /// The clamp-to-mirror blend, from `0.0` to `1.0`.
    pub const fn resize_layer(&self) -> f32 {
        self.resize_layer
    }

    /// The antialias level.
    pub const fn antialias(&self) -> DisplaceAntialias {
        self.antialias
    }

    /// Whether the effect leaves the frame untouched.
    pub fn is_identity(&self) -> bool {
        self.amount == 0.0
    }

    /// Pack the three uniform cells the shader reads.
    ///
    /// `params` carries `[amount, size, offset.x, offset.y]`, `extra` carries
    /// `[evolution, complexity, cycle * cycle_evolution, pin]` and `aces` carries
    /// `[displacement, random_seed, resize_layer, antialias samples]`.
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4], [f32; 4]) {
        (
            [self.amount, self.size, self.offset[0], self.offset[1]],
            [
                self.evolution,
                f32::from(self.complexity as u16),
                self.cycle as f32 * f32::from(u8::from(self.cycle_evolution)),
                self.pin.as_u32() as f32,
            ],
            [
                self.displacement.as_u32() as f32,
                self.random_seed as f32,
                self.resize_layer,
                f32::from(self.antialias.samples_per_axis() as u16),
            ],
        )
    }
}

/// Apply turbulent displace to one straight-alpha rgba f32 frame.
///
/// Returns `false` when the frame is empty or cannot be addressed; the grade is
/// assumed to be validated already, so an unsupported frame is the only
/// failure.
pub fn turbulent_displace_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &TurbulentDisplaceGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_turbulent_displace_f32_controlled(pixels, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

/// The checkpointed kernel behind [`turbulent_displace_frame_f32`].
pub(crate) fn apply_turbulent_displace_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &TurbulentDisplaceGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    let frame = &mut working[..pixel_count];
    premultiply_rgba_controlled(frame, checkpoint)?;
    let source = frame.to_vec();
    checkpoint()?;
    let rows = frame.len() / width;
    let setup = FieldSetup::new(grade, width, height);
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (row_offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + row_offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let base = [column as f32, row as f32];
                let mut accumulated = [0.0_f32; 4];
                let samples = setup.samples;
                let step = setup.step;
                for sample_row in 0..samples {
                    let offset_y = (sample_row as f32 + 0.5) * step - 0.5;
                    for sample_column in 0..samples {
                        let offset_x = (sample_column as f32 + 0.5) * step - 0.5;
                        let coord = [base[0] + offset_x, base[1] + offset_y];
                        let damping = setup.pin_damping(coord);
                        let displacement = setup.displacement_at(coord);
                        let sample = sample_source(
                            &source,
                            width,
                            height,
                            [
                                coord[0] + displacement[0] * damping,
                                coord[1] + displacement[1] * damping,
                            ],
                            setup.resize_layer,
                        );
                        for channel in 0..4 {
                            accumulated[channel] += sample[channel];
                        }
                    }
                }
                let scale = step * step;
                *pixel = [
                    accumulated[0] * scale,
                    accumulated[1] * scale,
                    accumulated[2] * scale,
                    accumulated[3] * scale,
                ];
            }
        }
    });
    unpremultiply_rgba_controlled(frame, checkpoint)?;
    checkpoint()?;
    Ok(true)
}

/// Reject a displacement discriminant that is not a declared menu entry.
fn validate_displacement(bits: u32) -> Result<(), TurbulentDisplaceError> {
    if bits < TURBULENT_TYPE_COUNT as u32 {
        Ok(())
    } else {
        Err(TurbulentDisplaceError::Displacement)
    }
}

/// Everything the per-pixel kernel needs, precomputed once per frame.
#[derive(Clone, Copy)]
struct FieldSetup {
    displacement: DisplacementType,
    amount: f32,
    size: f32,
    offset: [f32; 2],
    evolution: [f32; 2],
    seed_hash: u32,
    complexity: u32,
    smooth: bool,
    centre: [f32; 2],
    dims: [f32; 2],
    pin: DisplacePin,
    resize_layer: f32,
    samples: u32,
    step: f32,
}

impl FieldSetup {
    fn new(grade: &TurbulentDisplaceGrade, width: usize, height: usize) -> Self {
        let dims = [width as f32, height as f32];
        let evolution = if grade.cycle_evolution() {
            let theta = std::f32::consts::TAU * grade.evolution() * grade.cycle() as f32;
            let size_of_evolution = 1.0_f32;
            [
                size_of_evolution * theta.cos(),
                size_of_evolution * theta.sin(),
            ]
        } else {
            [
                grade.evolution() * TURBULENT_EVOLUTION_DRIFT[0],
                grade.evolution() * TURBULENT_EVOLUTION_DRIFT[1],
            ]
        };
        let samples = grade.antialias().samples_per_axis();
        Self {
            displacement: grade.displacement(),
            amount: grade.amount(),
            size: grade.size(),
            offset: grade.offset(),
            evolution,
            seed_hash: TURBULENT_SEED_BASE ^ grade.random_seed(),
            complexity: grade.complexity(),
            smooth: matches!(
                grade.displacement(),
                DisplacementType::TurbulentSmoother
                    | DisplacementType::BulgeSmoother
                    | DisplacementType::TwistSmoother
            ),
            centre: [0.5 * (dims[0] - 1.0), 0.5 * (dims[1] - 1.0)],
            dims,
            pin: grade.pin(),
            resize_layer: grade.resize_layer(),
            samples,
            step: 1.0 / samples as f32,
        }
    }

    /// The fractional field domain of one pixel-centre coordinate.
    fn domain(&self, coord: [f32; 2]) -> [f32; 2] {
        [
            (coord[0] - self.offset[0]) / self.size + self.evolution[0],
            (coord[1] - self.offset[1]) / self.size + self.evolution[1],
        ]
    }

    fn field(&self, u: f32, v: f32) -> f32 {
        turbulent_field(u, v, self.seed_hash, self.complexity, self.smooth)
    }

    /// The normalized radius of one coordinate, with the origin at the centre.
    fn radial(&self, coord: [f32; 2]) -> [f32; 2] {
        let dx = coord[0] - self.centre[0];
        let dy = coord[1] - self.centre[1];
        let length = (dx * dx + dy * dy).sqrt();
        let denominator = length.max(1.0e-6);
        [dx / denominator, dy / denominator]
    }

    /// The displacement, in pixels, before pinning damps it.
    fn displacement_at(&self, coord: [f32; 2]) -> [f32; 2] {
        let [u, v] = self.domain(coord);
        match self.displacement {
            DisplacementType::Turbulent | DisplacementType::TurbulentSmoother => {
                let h = TURBULENT_GRADIENT_STEP;
                let difference_x = self.field(u + h, v) - self.field(u - h, v);
                let difference_y = self.field(u, v + h) - self.field(u, v - h);
                // `amount` is pixels of displacement, so the difference
                // quotient stays in domain units: multiplying by `size` would
                // scale it by the noise cell size as well, and the default
                // grade (50 / 100) would push every pixel off the frame.
                [
                    self.amount * 0.5 * difference_x / (2.0 * h),
                    self.amount * 0.5 * difference_y / (2.0 * h),
                ]
            }
            DisplacementType::Bulge | DisplacementType::BulgeSmoother => {
                let radial = self.radial(coord);
                let value = self.field(u, v);
                [
                    radial[0] * self.amount * value,
                    radial[1] * self.amount * value,
                ]
            }
            DisplacementType::Twist | DisplacementType::TwistSmoother => {
                let radial = self.radial(coord);
                let value = self.field(u, v);
                [
                    -radial[1] * self.amount * value,
                    radial[0] * self.amount * value,
                ]
            }
            DisplacementType::Vertical => [0.0, self.amount * self.field(u, v)],
            DisplacementType::Horizontal => [self.amount * self.field(u, v), 0.0],
            DisplacementType::Cross => [
                self.amount * self.field(u, v),
                self.amount
                    * self.field(u + TURBULENT_CROSS_OFFSET[0], v + TURBULENT_CROSS_OFFSET[1]),
            ],
        }
    }

    /// The pinning damping at one continuous coordinate.
    fn pin_damping(&self, coord: [f32; 2]) -> f32 {
        pin_damping(self.pin, coord, self.dims)
    }
}

/// The normalized pin ramp, mirroring `wave_warp`'s geometry in f32.
fn pin_damping(pin: DisplacePin, coord: [f32; 2], dims: [f32; 2]) -> f32 {
    let normalized_x = (coord[0] + 0.5) / dims[0];
    let normalized_y = (coord[1] + 0.5) / dims[1];
    let margin = TURBULENT_PIN_MARGIN;
    let left = smoothstep01(normalized_x / margin);
    let right = smoothstep01((1.0 - normalized_x) / margin);
    let top = smoothstep01(normalized_y / margin);
    let bottom = smoothstep01((1.0 - normalized_y) / margin);
    match pin {
        DisplacePin::None => 1.0,
        DisplacePin::AllEdges => left.min(right).min(top).min(bottom),
        DisplacePin::Center => {
            let centre = [0.5 * (dims[0] - 1.0), 0.5 * (dims[1] - 1.0)];
            let radius = TURBULENT_CENTRE_PIN_FRACTION * dims[0].min(dims[1]);
            let dx = coord[0] - centre[0];
            let dy = coord[1] - centre[1];
            smoothstep01((dx * dx + dy * dy).sqrt() / radius)
        }
        DisplacePin::LeftEdge => left,
        DisplacePin::TopEdge => top,
        DisplacePin::RightEdge => right,
        DisplacePin::BottomEdge => bottom,
        DisplacePin::HorizontalEdges => top.min(bottom),
        DisplacePin::VerticalEdges => left.min(right),
    }
}

/// Hermite smoothstep clamped to `[0, 1]`.
fn smoothstep01(value: f32) -> f32 {
    let t = value.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Bicubic (cubic Hermite) interpolation weight.
fn cubic_weight(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Quintic interpolation weight used by the `*_smoother` variants.
fn quintic_weight(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// The normalized value noise of one octave.
fn value_noise(x: f32, y: f32, salt: u32, smooth: bool) -> f32 {
    let cell_x = x.floor();
    let cell_y = y.floor();
    let fraction_x = x - cell_x;
    let fraction_y = y - cell_y;
    let weight_x = if smooth {
        quintic_weight(fraction_x)
    } else {
        cubic_weight(fraction_x)
    };
    let weight_y = if smooth {
        quintic_weight(fraction_y)
    } else {
        cubic_weight(fraction_y)
    };
    let x0 = cell_x as i32 as u32;
    let y0 = cell_y as i32 as u32;
    let x1 = (cell_x as i32).wrapping_add(1) as u32;
    let y1 = (cell_y as i32).wrapping_add(1) as u32;
    let top_left = noise_sample(x0, y0, salt);
    let top_right = noise_sample(x1, y0, salt);
    let bottom_left = noise_sample(x0, y1, salt);
    let bottom_right = noise_sample(x1, y1, salt);
    let top = top_left + (top_right - top_left) * weight_x;
    let bottom = bottom_left + (bottom_right - bottom_left) * weight_x;
    top + (bottom - top) * weight_y
}

/// The normalized fractal field, in `[-1, 1]`.
fn turbulent_field(u: f32, v: f32, seed_hash: u32, complexity: u32, smooth: bool) -> f32 {
    let octaves = if smooth {
        (complexity / 2).max(1)
    } else {
        complexity.max(1)
    };
    let mut total = 0.0_f32;
    let mut total_weight = 0.0_f32;
    let mut amplitude = 0.5_f32;
    let mut frequency = 1.0_f32;
    for octave in 0..octaves {
        let salt = seed_hash ^ octave.wrapping_mul(TURBULENT_OCTAVE_SALT_STEP);
        total += amplitude * value_noise(u * frequency, v * frequency, salt, smooth);
        total_weight += amplitude;
        frequency *= 2.0;
        amplitude *= 0.5;
    }
    total / total_weight
}

/// Reflect one coordinate about the frame edges, applied once.
fn mirror_coordinate(value: f32, extent: f32) -> f32 {
    if value < 0.0 {
        -value
    } else if value > extent {
        2.0 * extent - value
    } else {
        value
    }
}

/// Sample the premultiplied source, blending a clamped and a mirrored sample.
///
/// `resize_layer == 0.0` reproduces edge clamping bit for bit; `1.0` takes the
/// mirrored sample anywhere the coordinate leaves the frame.
fn sample_source(
    source: &[[f32; 4]],
    width: usize,
    height: usize,
    coord: [f32; 2],
    resize_layer: f32,
) -> [f32; 4] {
    let clamped = crate::resample::sample_bilinear_clamped(
        source,
        width,
        height,
        coord[0] as f64,
        coord[1] as f64,
    );
    let mirrored = crate::resample::sample_bilinear_clamped(
        source,
        width,
        height,
        mirror_coordinate(coord[0], (width - 1) as f32) as f64,
        mirror_coordinate(coord[1], (height - 1) as f32) as f64,
    );
    let weight = resize_layer.clamp(0.0, 1.0);
    if weight <= 0.0 {
        return clamped;
    }
    if weight >= 1.0 {
        return mirrored;
    }
    let mut sample = [0.0_f32; 4];
    for channel in 0..4 {
        sample[channel] = clamped[channel] + (mirrored[channel] - clamped[channel]) * weight;
    }
    sample
}

#[cfg(test)]
mod tests {
    use super::{
        apply_turbulent_displace_f32_controlled, mirror_coordinate, pin_damping, sample_source,
        smoothstep01, turbulent_displace_frame_f32, validate_displacement, DisplaceAntialias,
        DisplacePin, DisplacementType, TurbulentDisplaceError, TurbulentDisplaceGrade,
        TURBULENT_GRADIENT_STEP, TURBULENT_MAX_CYCLE, TURBULENT_OCTAVE_SALT_STEP,
        TURBULENT_SEED_BASE, TURBULENT_TYPE_COUNT,
    };
    use crate::noise::noise_sample;

    fn grade(displacement: DisplacementType, amount: f32, size: f32) -> TurbulentDisplaceGrade {
        TurbulentDisplaceGrade::new(
            displacement,
            amount,
            size,
            [0.0, 0.0],
            1,
            0.0,
            false,
            1,
            0,
            DisplacePin::None,
            0.0,
            DisplaceAntialias::Low,
        )
        .expect("valid turbulent displace grade")
    }

    fn gradient_frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut frame = vec![[0.0_f32; 4]; width * height];
        for row in 0..height {
            for column in 0..width {
                let value = 0.011 * (column + 3 * row) as f32;
                frame[row * width + column] = [value, 0.5 - 0.3 * value, 0.2 + 0.7 * value, 1.0];
            }
        }
        frame
    }

    fn coordinate_frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut frame = vec![[0.0_f32; 4]; width * height];
        for row in 0..height {
            for column in 0..width {
                frame[row * width + column] = [column as f32 * 0.01, row as f32 * 0.01, 0.25, 1.0];
            }
        }
        frame
    }

    fn at(frame: &[[f32; 4]], width: usize, column: usize, row: usize) -> [f32; 4] {
        frame[row * width + column]
    }

    fn assert_close(actual: [f32; 4], expected: [f32; 4], label: &str) {
        for channel in 0..4 {
            assert!(
                (actual[channel] - expected[channel]).abs() <= 1.0e-5,
                "{label} channel {channel}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// A second, independent transcription of the documented field.
    fn reference_weight(t: f32, smooth: bool) -> f32 {
        if smooth {
            t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
        } else {
            t * t * (3.0 - 2.0 * t)
        }
    }

    fn reference_noise(x: f32, y: f32, salt: u32, smooth: bool) -> f32 {
        let cell_x = x.floor();
        let cell_y = y.floor();
        let fraction_x = x - cell_x;
        let fraction_y = y - cell_y;
        let weight_x = reference_weight(fraction_x, smooth);
        let weight_y = reference_weight(fraction_y, smooth);
        let x0 = cell_x as i32;
        let y0 = cell_y as i32;
        let corner = |x: i32, y: i32| noise_sample(x as u32, y as u32, salt);
        let top = corner(x0, y0) + (corner(x0 + 1, y0) - corner(x0, y0)) * weight_x;
        let bottom = corner(x0, y0 + 1) + (corner(x0 + 1, y0 + 1) - corner(x0, y0 + 1)) * weight_x;
        top + (bottom - top) * weight_y
    }

    fn reference_field(u: f32, v: f32, seed_hash: u32, complexity: u32, smooth: bool) -> f32 {
        let octaves = if smooth {
            (complexity / 2).max(1)
        } else {
            complexity
        };
        let mut total = 0.0_f32;
        let mut total_weight = 0.0_f32;
        let mut amplitude = 0.5_f32;
        let mut frequency = 1.0_f32;
        for octave in 0..octaves {
            let salt = seed_hash ^ octave.wrapping_mul(TURBULENT_OCTAVE_SALT_STEP);
            total += amplitude * reference_noise(u * frequency, v * frequency, salt, smooth);
            total_weight += amplitude;
            frequency *= 2.0;
            amplitude *= 0.5;
        }
        total / total_weight
    }

    #[test]
    fn identity_grade_is_bit_identical() {
        let width = 17;
        let height = 13;
        let mut frame = gradient_frame(width, height);
        frame[21] = [2.5, -0.75, 1.25, 0.5];
        let original = frame.clone();
        let grade = TurbulentDisplaceGrade::new(
            DisplacementType::Turbulent,
            0.0,
            100.0,
            [128.0, 128.0],
            4,
            0.4,
            true,
            7,
            42,
            DisplacePin::AllEdges,
            0.75,
            DisplaceAntialias::High,
        )
        .expect("identity grade");
        assert!(grade.is_identity());
        assert!(turbulent_displace_frame_f32(
            &mut frame, width, height, &grade
        ));
        assert_eq!(frame, original);
    }

    #[test]
    fn zero_amount_is_identity_and_supported() {
        let width = 9;
        let height = 7;
        let mut frame = gradient_frame(width, height);
        let original = frame.clone();
        let grade = grade(DisplacementType::Twist, 0.0, 6.0);
        assert_eq!(grade.amount(), 0.0);
        assert!(grade.is_identity());
        let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
        assert_eq!(
            apply_turbulent_displace_f32_controlled(
                &mut frame,
                width,
                height,
                &grade,
                &mut checkpoint
            ),
            Ok(true)
        );
        assert_eq!(frame, original);
    }

    #[test]
    fn positive_amount_moves_a_high_contrast_edge() {
        let width = 21;
        let height = 17;
        let mut frame = vec![[0.0_f32; 4]; width * height];
        for row in 0..height {
            for column in 0..width {
                let value = if column < 10 { 0.05_f32 } else { 0.85 };
                frame[row * width + column] = [value, value, value, 1.0];
            }
        }
        let size = 1_000.0_f32;
        let amount = 0.05_f32;
        let grade = grade(DisplacementType::Turbulent, amount, size);
        let source = frame.clone();
        assert!(turbulent_displace_frame_f32(
            &mut frame, width, height, &grade
        ));
        let seed_hash = TURBULENT_SEED_BASE;
        let h = TURBULENT_GRADIENT_STEP;
        for (column, row) in [(4_usize, 3_usize), (9, 8), (10, 12), (15, 6), (0, 0)] {
            let u = column as f32 / size;
            let v = row as f32 / size;
            let difference_x = reference_field(u + h, v, seed_hash, 1, false)
                - reference_field(u - h, v, seed_hash, 1, false);
            let difference_y = reference_field(u, v + h, seed_hash, 1, false)
                - reference_field(u, v - h, seed_hash, 1, false);
            let offset_x = amount * 0.5 * difference_x / (2.0 * h);
            let offset_y = amount * 0.5 * difference_y / (2.0 * h);
            let expected = crate::resample::sample_bilinear_clamped(
                &source,
                width,
                height,
                (column as f32 + offset_x) as f64,
                (row as f32 + offset_y) as f64,
            );
            let actual = at(&frame, width, column, row);
            assert_close(
                actual,
                expected,
                &format!("hand-computed displacement at ({column}, {row})"),
            );
        }
        assert_ne!(
            frame, source,
            "a positive amount must move the high-contrast frame"
        );
    }

    #[test]
    fn every_displacement_type_differs_from_turbulent() {
        let width = 40;
        let height = 40;
        let mut frame = gradient_frame(width, height);
        for row in 0..height {
            for column in 0..width {
                let value = 0.5 + 0.35 * ((column as f32 * 0.21).sin() * (row as f32 * 0.17).cos());
                frame[row * width + column] = [value, 1.0 - value, 0.5 + 0.25 * value, 1.0];
            }
        }
        let base = grade(DisplacementType::Turbulent, 0.5, 4.0);
        let mut reference = frame.clone();
        assert!(turbulent_displace_frame_f32(
            &mut reference,
            width,
            height,
            &base
        ));
        for displacement in DisplacementType::ALL {
            if displacement == DisplacementType::Turbulent {
                continue;
            }
            let mut candidate = frame.clone();
            assert!(turbulent_displace_frame_f32(
                &mut candidate,
                width,
                height,
                &grade(displacement, 0.5, 4.0)
            ));
            assert_ne!(
                candidate,
                reference,
                "{} must not match the default turbulent warp",
                displacement.as_key()
            );
        }
    }

    #[test]
    fn smoother_variants_reduce_second_differences() {
        let width = 40;
        let height = 40;
        let mut frame = vec![[0.0_f32; 4]; width * height];
        for row in 0..height {
            for column in 0..width {
                let value = 0.5 + 0.35 * ((column as f32 * 0.13).sin() * (row as f32 * 0.11).cos());
                frame[row * width + column] = [value, value, value, 1.0];
            }
        }
        let roughness = |frame: &[[f32; 4]]| {
            let mut total = 0.0_f64;
            for row in 1..height - 1 {
                for column in 1..width - 1 {
                    for channel in [0_usize, 1, 2] {
                        let centre = frame[row * width + column][channel] as f64;
                        let left = frame[row * width + column - 1][channel] as f64;
                        let right = frame[row * width + column + 1][channel] as f64;
                        let up = frame[(row - 1) * width + column][channel] as f64;
                        let down = frame[(row + 1) * width + column][channel] as f64;
                        total += (left - 2.0 * centre + right).abs();
                        total += (up - 2.0 * centre + down).abs();
                    }
                }
            }
            total
        };
        for (sharp, smooth) in [
            (
                DisplacementType::Turbulent,
                DisplacementType::TurbulentSmoother,
            ),
            (DisplacementType::Bulge, DisplacementType::BulgeSmoother),
            (DisplacementType::Twist, DisplacementType::TwistSmoother),
        ] {
            let build = |displacement: DisplacementType| {
                TurbulentDisplaceGrade::new(
                    displacement,
                    8.0,
                    16.0,
                    [0.0, 0.0],
                    6,
                    0.0,
                    false,
                    1,
                    0,
                    DisplacePin::None,
                    0.0,
                    DisplaceAntialias::Low,
                )
                .expect("valid grade")
            };
            let mut sharp_frame = frame.clone();
            assert!(turbulent_displace_frame_f32(
                &mut sharp_frame,
                width,
                height,
                &build(sharp)
            ));
            let mut smooth_frame = frame.clone();
            assert!(turbulent_displace_frame_f32(
                &mut smooth_frame,
                width,
                height,
                &build(smooth)
            ));
            assert!(
                roughness(&smooth_frame) < roughness(&sharp_frame),
                "{} must be smoother than {}: {} vs {}",
                smooth.as_key(),
                sharp.as_key(),
                roughness(&smooth_frame),
                roughness(&sharp_frame)
            );
        }
    }

    #[test]
    fn vertical_and_horizontal_axes_stay_fixed() {
        let width = 24;
        let height = 20;
        let original = coordinate_frame(width, height);
        let mut vertical = original.clone();
        assert!(turbulent_displace_frame_f32(
            &mut vertical,
            width,
            height,
            &grade(DisplacementType::Vertical, 0.4, 6.0)
        ));
        for row in 0..height {
            for column in 0..width {
                assert_eq!(
                    at(&vertical, width, column, row)[0],
                    at(&original, width, column, row)[0],
                    "vertical displacement moved x at ({column}, {row})"
                );
            }
        }
        let mut horizontal = original.clone();
        assert!(turbulent_displace_frame_f32(
            &mut horizontal,
            width,
            height,
            &grade(DisplacementType::Horizontal, 0.4, 6.0)
        ));
        for row in 0..height {
            for column in 0..width {
                assert_eq!(
                    at(&horizontal, width, column, row)[1],
                    at(&original, width, column, row)[1],
                    "horizontal displacement moved y at ({column}, {row})"
                );
            }
        }
    }

    #[test]
    fn cross_displaces_both_axes_independently() {
        let width = 24;
        let height = 20;
        let original = coordinate_frame(width, height);
        let mut frame = original.clone();
        assert!(turbulent_displace_frame_f32(
            &mut frame,
            width,
            height,
            &grade(DisplacementType::Cross, 0.4, 6.0)
        ));
        let mut independent = false;
        let mut moved_both = false;
        for row in 2..height - 2 {
            for column in 2..width - 2 {
                let shift_x =
                    at(&frame, width, column, row)[0] - at(&original, width, column, row)[0];
                let shift_y =
                    at(&frame, width, column, row)[1] - at(&original, width, column, row)[1];
                if shift_x.abs() > 1.0e-4 && shift_y.abs() > 1.0e-4 {
                    moved_both = true;
                }
                if (shift_x - shift_y).abs() > 1.0e-4 {
                    independent = true;
                }
            }
        }
        assert!(moved_both, "cross displacement must move both axes");
        assert!(
            independent,
            "cross displacement must use one field per axis"
        );
    }

    #[test]
    fn field_is_deterministic_for_a_fixed_seed() {
        let width = 23;
        let height = 19;
        let build = || {
            TurbulentDisplaceGrade::new(
                DisplacementType::Cross,
                0.8,
                5.0,
                [3.0, 4.0],
                3,
                0.25,
                false,
                4,
                1234,
                DisplacePin::Center,
                0.4,
                DisplaceAntialias::High,
            )
            .expect("valid grade")
        };
        let mut first = gradient_frame(width, height);
        let mut second = first.clone();
        assert!(turbulent_displace_frame_f32(
            &mut first,
            width,
            height,
            &build()
        ));
        assert!(turbulent_displace_frame_f32(
            &mut second,
            width,
            height,
            &build()
        ));
        assert_eq!(first, second);
    }

    #[test]
    fn random_seed_changes_the_field() {
        let width = 23;
        let height = 19;
        let build = |seed: u32| {
            TurbulentDisplaceGrade::new(
                DisplacementType::Bulge,
                2.0,
                6.0,
                [0.0, 0.0],
                2,
                0.0,
                false,
                1,
                seed,
                DisplacePin::None,
                0.0,
                DisplaceAntialias::Low,
            )
            .expect("valid grade")
        };
        let mut first = gradient_frame(width, height);
        let mut second = first.clone();
        assert!(turbulent_displace_frame_f32(
            &mut first,
            width,
            height,
            &build(0)
        ));
        assert!(turbulent_displace_frame_f32(
            &mut second,
            width,
            height,
            &build(9)
        ));
        assert_ne!(first, second);
    }

    #[test]
    fn complexity_changes_the_field() {
        let width = 23;
        let height = 19;
        let build = |complexity: u32| {
            TurbulentDisplaceGrade::new(
                DisplacementType::Turbulent,
                2.0,
                8.0,
                [0.0, 0.0],
                complexity,
                0.0,
                false,
                1,
                0,
                DisplacePin::None,
                0.0,
                DisplaceAntialias::Low,
            )
            .expect("valid grade")
        };
        let mut first = gradient_frame(width, height);
        let mut second = first.clone();
        assert!(turbulent_displace_frame_f32(
            &mut first,
            width,
            height,
            &build(1)
        ));
        assert!(turbulent_displace_frame_f32(
            &mut second,
            width,
            height,
            &build(3)
        ));
        assert_ne!(first, second);
    }

    #[test]
    fn cycle_evolution_loops_after_one_cycle() {
        let width = 24;
        let height = 20;
        let cycle = 5_u32;
        let build = |evolution: f32| {
            TurbulentDisplaceGrade::new(
                DisplacementType::Vertical,
                0.6,
                5.0,
                [0.0, 0.0],
                2,
                evolution,
                true,
                cycle,
                7,
                DisplacePin::None,
                0.0,
                DisplaceAntialias::Low,
            )
            .expect("valid grade")
        };
        let mut start = gradient_frame(width, height);
        let mut looped = start.clone();
        assert!(turbulent_displace_frame_f32(
            &mut start,
            width,
            height,
            &build(0.0)
        ));
        assert!(turbulent_displace_frame_f32(
            &mut looped,
            width,
            height,
            &build(1.0 / cycle as f32)
        ));
        for (index, (left, right)) in start.iter().zip(looped.iter()).enumerate() {
            for channel in 0..4 {
                assert!(
                    (left[channel] - right[channel]).abs() <= 1.0e-4,
                    "pixel {index} channel {channel}: {} vs {}",
                    left[channel],
                    right[channel]
                );
            }
        }
    }

    #[test]
    fn mirror_sampling_differs_from_clamped_sampling() {
        let width = 24;
        let height = 20;
        let frame = gradient_frame(width, height);
        let clamped = grade(DisplacementType::Horizontal, 2.0, 4.0);
        let mirrored = TurbulentDisplaceGrade::new(
            DisplacementType::Horizontal,
            2.0,
            4.0,
            [0.0, 0.0],
            1,
            0.0,
            false,
            1,
            0,
            DisplacePin::None,
            1.0,
            DisplaceAntialias::Low,
        )
        .expect("valid grade");
        let mut first = frame.clone();
        let mut second = frame.clone();
        assert!(turbulent_displace_frame_f32(
            &mut first, width, height, &clamped
        ));
        assert!(turbulent_displace_frame_f32(
            &mut second,
            width,
            height,
            &mirrored
        ));
        assert_ne!(first, second);
    }

    #[test]
    fn resize_layer_zero_equals_edge_clamping() {
        let width = 11;
        let height = 9;
        let frame = gradient_frame(width, height);
        for coord in [[-4.5_f32, 2.25], [3.75, -2.5], [20.5, 30.25], [-9.0, -9.0]] {
            let expected = crate::resample::sample_bilinear_clamped(
                &frame,
                width,
                height,
                coord[0] as f64,
                coord[1] as f64,
            );
            assert_eq!(
                sample_source(&frame, width, height, coord, 0.0),
                expected,
                "resize layer zero must clamp at {coord:?}"
            );
            let mirrored_expected = crate::resample::sample_bilinear_clamped(
                &frame,
                width,
                height,
                mirror_coordinate(coord[0], (width - 1) as f32) as f64,
                mirror_coordinate(coord[1], (height - 1) as f32) as f64,
            );
            assert_eq!(
                sample_source(&frame, width, height, coord, 1.0),
                mirrored_expected,
                "resize layer one must mirror at {coord:?}"
            );
        }
    }

    #[test]
    fn all_edges_pinning_damps_the_border() {
        let width = 40;
        let height = 40;
        let original = coordinate_frame(width, height);
        let build = |pin: DisplacePin| {
            TurbulentDisplaceGrade::new(
                DisplacementType::Vertical,
                1.5,
                8.0,
                [0.0, 0.0],
                2,
                0.0,
                false,
                1,
                0,
                pin,
                0.0,
                DisplaceAntialias::Low,
            )
            .expect("valid grade")
        };
        let mut free = original.clone();
        let mut pinned = original.clone();
        assert!(turbulent_displace_frame_f32(
            &mut free,
            width,
            height,
            &build(DisplacePin::None)
        ));
        assert!(turbulent_displace_frame_f32(
            &mut pinned,
            width,
            height,
            &build(DisplacePin::AllEdges)
        ));
        let band = (width as f32 * 0.2) as usize;
        let movement = |frame: &[[f32; 4]], column: usize, row: usize| {
            (at(frame, width, column, row)[1] - at(&original, width, column, row)[1]).abs()
        };
        let mut free_total = 0.0_f64;
        let mut pinned_total = 0.0_f64;
        for row in 0..height {
            for column in 0..width {
                let on_border =
                    column < band || row < band || column >= width - band || row >= height - band;
                if on_border {
                    free_total += movement(&free, column, row) as f64;
                    pinned_total += movement(&pinned, column, row) as f64;
                }
            }
        }
        assert!(
            pinned_total < free_total,
            "all-edge pinning must damp the border band: {pinned_total} vs {free_total}"
        );
        for (column, row) in [
            (0_usize, 0_usize),
            (width - 1, 0),
            (0, height - 1),
            (width - 1, height - 1),
        ] {
            assert!(
                movement(&pinned, column, row) <= movement(&free, column, row) + 1.0e-6,
                "pinned corner ({column}, {row}) must never move more than the free one"
            );
        }
    }

    #[test]
    fn premultiplied_sampling_does_not_bleed_hidden_colour() {
        let width = 9;
        let height = 9;
        let mut visible = gradient_frame(width, height);
        for row in 0..height {
            for column in 0..width {
                visible[row * width + column][3] = 1.0;
            }
        }
        let centre = 4 * width + 4;
        visible[centre][3] = 0.0;
        let mut hidden = visible.clone();
        hidden[centre] = [1.0, 0.0, 0.0, 0.0];
        let grade = grade(DisplacementType::Horizontal, 1.5, 4.0);
        let mut first = visible.clone();
        let mut second = hidden;
        assert!(turbulent_displace_frame_f32(
            &mut first, width, height, &grade
        ));
        assert!(turbulent_displace_frame_f32(
            &mut second,
            width,
            height,
            &grade
        ));
        assert_eq!(first, second);
        for pixel in &first {
            if pixel[3] <= 1.0e-6 {
                assert_eq!(&pixel[..3], &[0.0, 0.0, 0.0]);
            }
        }
    }

    #[test]
    fn one_pixel_frames_pass_through() {
        let mut frame = [[0.25_f32, 0.5, 0.75, 1.0]];
        let original = frame;
        let grade = grade(DisplacementType::Turbulent, 80.0, 3.0);
        assert!(turbulent_displace_frame_f32(&mut frame, 1, 1, &grade));
        assert_eq!(frame, original);
    }

    #[test]
    fn empty_frames_report_unsupported() {
        let grade = grade(DisplacementType::Turbulent, 10.0, 5.0);
        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!turbulent_displace_frame_f32(&mut empty, 0, 0, &grade));
        assert!(!turbulent_displace_frame_f32(&mut empty, 0, 4, &grade));
        let mut short = [[0.0_f32; 4]; 2];
        assert!(!turbulent_displace_frame_f32(&mut short, 3, 3, &grade));
    }

    #[test]
    fn every_validation_error_triggers() {
        let valid = grade(DisplacementType::Turbulent, 10.0, 100.0);
        assert_eq!(valid.displacement(), DisplacementType::Turbulent);
        let mut cases: Vec<(TurbulentDisplaceError, TurbulentDisplaceError)> = Vec::new();
        let build = |amount: f32,
                     size: f32,
                     offset: [f32; 2],
                     complexity: u32,
                     evolution: f32,
                     cycle: u32,
                     seed: u32,
                     resize: f32| {
            TurbulentDisplaceGrade::new(
                DisplacementType::Turbulent,
                amount,
                size,
                offset,
                complexity,
                evolution,
                false,
                cycle,
                seed,
                DisplacePin::None,
                resize,
                DisplaceAntialias::Low,
            )
        };
        cases.push((
            build(10_001.0, 100.0, [0.0, 0.0], 1, 0.0, 1, 0, 0.0).unwrap_err(),
            TurbulentDisplaceError::Amount,
        ));
        cases.push((
            build(f32::NAN, 100.0, [0.0, 0.0], 1, 0.0, 1, 0, 0.0).unwrap_err(),
            TurbulentDisplaceError::Amount,
        ));
        cases.push((
            build(10.0, 1.9, [0.0, 0.0], 1, 0.0, 1, 0, 0.0).unwrap_err(),
            TurbulentDisplaceError::Size,
        ));
        cases.push((
            build(10.0, 1000.1, [0.0, 0.0], 1, 0.0, 1, 0, 0.0).unwrap_err(),
            TurbulentDisplaceError::Size,
        ));
        cases.push((
            build(10.0, 100.0, [1.0e7, 0.0], 1, 0.0, 1, 0, 0.0).unwrap_err(),
            TurbulentDisplaceError::Offset,
        ));
        cases.push((
            build(10.0, 100.0, [0.0, f32::INFINITY], 1, 0.0, 1, 0, 0.0).unwrap_err(),
            TurbulentDisplaceError::Offset,
        ));
        cases.push((
            build(10.0, 100.0, [0.0, 0.0], 0, 0.0, 1, 0, 0.0).unwrap_err(),
            TurbulentDisplaceError::Complexity,
        ));
        cases.push((
            build(10.0, 100.0, [0.0, 0.0], 11, 0.0, 1, 0, 0.0).unwrap_err(),
            TurbulentDisplaceError::Complexity,
        ));
        cases.push((
            build(10.0, 100.0, [0.0, 0.0], 1, f32::NAN, 1, 0, 0.0).unwrap_err(),
            TurbulentDisplaceError::Evolution,
        ));
        cases.push((
            build(10.0, 100.0, [0.0, 0.0], 1, 0.0, 0, 0, 0.0).unwrap_err(),
            TurbulentDisplaceError::Cycle,
        ));
        cases.push((
            build(10.0, 100.0, [0.0, 0.0], 1, 0.0, 89, 0, 0.0).unwrap_err(),
            TurbulentDisplaceError::Cycle,
        ));
        cases.push((
            build(10.0, 100.0, [0.0, 0.0], 1, 0.0, 1, 100_001, 0.0).unwrap_err(),
            TurbulentDisplaceError::RandomSeed,
        ));
        cases.push((
            build(10.0, 100.0, [0.0, 0.0], 1, 0.0, 1, 0, -0.1).unwrap_err(),
            TurbulentDisplaceError::ResizeLayer,
        ));
        cases.push((
            build(10.0, 100.0, [0.0, 0.0], 1, 0.0, 1, 0, 1.1).unwrap_err(),
            TurbulentDisplaceError::ResizeLayer,
        ));
        for (error, expected) in cases {
            assert_eq!(error, expected);
            assert_eq!(error.to_string(), expected.to_string());
        }
        assert_eq!(
            validate_displacement(TURBULENT_TYPE_COUNT as u32),
            Err(TurbulentDisplaceError::Displacement)
        );
        assert_eq!(
            TurbulentDisplaceError::Displacement.to_string(),
            "turbulent displace displacement must be a declared type"
        );
        assert_eq!(
            TurbulentDisplaceError::Amount.to_string(),
            "turbulent displace amount must be a finite value inside [-10000, 10000]"
        );
    }

    #[test]
    fn gpu_controls_packs_the_documented_cells() {
        let grade = TurbulentDisplaceGrade::new(
            DisplacementType::Cross,
            50.0,
            100.0,
            [7.0, 9.0],
            3,
            0.25,
            true,
            12,
            4321,
            DisplacePin::VerticalEdges,
            0.5,
            DisplaceAntialias::High,
        )
        .expect("valid grade");
        let (params, extra, aces) = grade.gpu_controls();
        assert_eq!(params, [50.0, 100.0, 7.0, 9.0]);
        assert_eq!(extra, [0.25, 3.0, 12.0, 8.0]);
        assert_eq!(aces, [8.0, 4321.0, 0.5, 4.0]);
        let static_grade = TurbulentDisplaceGrade::new(
            DisplacementType::BulgeSmoother,
            0.0,
            2.0,
            [0.0, 0.0],
            1,
            0.0,
            false,
            1,
            0,
            DisplacePin::None,
            0.0,
            DisplaceAntialias::Low,
        )
        .expect("valid grade");
        let (_, extra, aces) = static_grade.gpu_controls();
        assert_eq!(extra[2], 0.0);
        assert_eq!(aces[3], 1.0);
        assert_eq!(
            TurbulentDisplaceGrade::new(
                DisplacementType::Turbulent,
                0.0,
                2.0,
                [0.0, 0.0],
                1,
                0.0,
                false,
                TURBULENT_MAX_CYCLE,
                0,
                DisplacePin::None,
                0.0,
                DisplaceAntialias::Low,
            )
            .expect("max cycle grade")
            .cycle(),
            TURBULENT_MAX_CYCLE
        );
    }

    #[test]
    fn grade_accessors_round_trip() {
        let grade = TurbulentDisplaceGrade::new(
            DisplacementType::TwistSmoother,
            -12.5,
            42.0,
            [-3.0, 8.5],
            6,
            -0.75,
            true,
            9,
            77,
            DisplacePin::Center,
            0.25,
            DisplaceAntialias::High,
        )
        .expect("valid grade");
        assert_eq!(grade.displacement(), DisplacementType::TwistSmoother);
        assert_eq!(grade.amount(), -12.5);
        assert_eq!(grade.size(), 42.0);
        assert_eq!(grade.offset(), [-3.0, 8.5]);
        assert_eq!(grade.complexity(), 6);
        assert_eq!(grade.evolution(), -0.75);
        assert!(grade.cycle_evolution());
        assert_eq!(grade.cycle(), 9);
        assert_eq!(grade.random_seed(), 77);
        assert_eq!(grade.pin(), DisplacePin::Center);
        assert_eq!(grade.resize_layer(), 0.25);
        assert_eq!(grade.antialias(), DisplaceAntialias::High);
        assert!(!grade.is_identity());
    }

    #[test]
    fn menus_and_keys_cover_every_variant() {
        for (index, displacement) in DisplacementType::ALL.into_iter().enumerate() {
            assert_eq!(displacement.as_u32() as usize, index);
            assert_eq!(displacement.as_menu(), index as u32 + 1);
            assert_eq!(
                DisplacementType::from_key(displacement.as_key()),
                Some(displacement)
            );
            assert!(!displacement.label().is_empty());
        }
        for (index, pin) in DisplacePin::ALL.into_iter().enumerate() {
            assert_eq!(pin.as_u32() as usize, index);
            assert_eq!(pin.as_menu(), index as u32 + 1);
            assert_eq!(DisplacePin::from_key(pin.as_key()), Some(pin));
            assert!(!pin.label().is_empty());
        }
        assert_eq!(DisplacementType::from_key("nope"), None);
        assert_eq!(DisplacePin::from_key("nope"), None);
        assert_eq!(DisplaceAntialias::from_key("nope"), None);
        assert_eq!(DisplaceAntialias::ALL.len(), 2);
        assert_eq!(DisplaceAntialias::Low.samples_per_axis(), 1);
        assert_eq!(DisplaceAntialias::High.samples_per_axis(), 4);
    }

    #[test]
    fn pin_damping_ramps_match_the_wave_warp_geometry() {
        let dims = [40.0_f32, 20.0_f32];
        assert_eq!(pin_damping(DisplacePin::None, [0.0, 0.0], dims), 1.0);
        assert!(
            (pin_damping(DisplacePin::AllEdges, [0.0, 0.0], dims) - smoothstep01(0.0625)).abs()
                < 1.0e-6
        );
        assert!((pin_damping(DisplacePin::AllEdges, [20.0, 10.0], dims) - 1.0).abs() < 1.0e-6);
        assert_eq!(
            pin_damping(DisplacePin::VerticalEdges, [0.0, 10.0], dims),
            pin_damping(DisplacePin::LeftEdge, [0.0, 10.0], dims)
        );
        assert!(
            pin_damping(DisplacePin::HorizontalEdges, [20.0, 0.0], dims)
                < pin_damping(DisplacePin::None, [20.0, 0.0], dims)
        );
    }

    #[test]
    fn checkpoint_propagates_from_the_controlled_kernel() {
        #[derive(Debug, PartialEq)]
        struct Cancelled;
        let grade = grade(DisplacementType::Turbulent, 5.0, 6.0);
        let mut frame = gradient_frame(16, 16);
        let mut calls = 0_usize;
        let mut checkpoint = || {
            calls += 1;
            if calls > 2 {
                Err(Cancelled)
            } else {
                Ok(())
            }
        };
        assert_eq!(
            apply_turbulent_displace_f32_controlled(&mut frame, 16, 16, &grade, &mut checkpoint),
            Err(Cancelled)
        );
        assert!(calls > 2, "the kernel must poll the checkpoint");
        assert_eq!(frame.len(), 16 * 16);
    }
}
