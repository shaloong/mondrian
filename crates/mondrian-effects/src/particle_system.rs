//! Stateless particle births, closed-form motion, and analytic Float32 coverage.

use mondrian_core::{BlendMode, ImageSampling, TimelineTime};
use std::f64::consts::TAU;

use crate::{
    adjustment::blend_rgba_f32_pixel, coverage::has_positive_coverage, EffectFrameExtent,
    EffectPixelRoi,
};

const EMISSION_HZ: i128 = 120;
const SUBTICKS: i128 = 64;
const TIME_SCALE: i128 = EMISSION_HZ * SUBTICKS;
const MAX_LIFETIME: i128 = 30;
const RATE_SCALE: i128 = 1_000_000;
/// Ratio of the native ellipse's minor to major radius.
pub const PARTICLE_ELLIPSE_ASPECT: f32 = 0.75;

pub(crate) const PARTICLE_GROUP: &str = "粒子系统";
pub(crate) const PARTICLE_SHAPE_OPTIONS: &[(&str, &str)] = &[
    ("point", "点"),
    ("line", "线"),
    ("circle", "圆"),
    ("rectangle", "矩形"),
];
pub(crate) const PARTICLE_BLEND_OPTIONS: &[(&str, &str)] = &[
    ("add", "相加"),
    ("normal", "正常"),
    ("screen", "滤色"),
    ("overlay", "叠加"),
];

pub(crate) struct ParticleScalar {
    pub key: &'static str,
    pub label: &'static str,
    pub bounds: [f32; 2],
    pub step: f64,
    pub scale: f32,
    pub read: fn(&ParticleEmitter) -> f32,
    write: fn(&mut ParticleEmitter, f32),
}

macro_rules! scalar {
    ($key:literal, $label:literal, $min:expr, $max:expr, $step:expr, $scale:expr, $field:ident) => {
        ParticleScalar {
            key: $key,
            label: $label,
            bounds: [$min, $max],
            step: $step,
            scale: $scale,
            read: |emitter| emitter.$field,
            write: |emitter, value| emitter.$field = value / $scale,
        }
    };
}

pub(crate) const PARTICLE_SCALARS: &[ParticleScalar] = &[
    scalar!(
        "emitter_size",
        "发射区域大小",
        0.0,
        500.0,
        1.0,
        1.0,
        emitter_size
    ),
    scalar!("rate", "每秒粒子数", 0.0, 2000.0, 1.0, 1.0, rate),
    scalar!("lifetime", "粒子寿命", 0.1, 30.0, 0.01, 1.0, lifetime),
    scalar!(
        "lifetime_random",
        "寿命随机",
        0.0,
        100.0,
        1.0,
        100.0,
        lifetime_random
    ),
    scalar!("speed", "初始速度", 0.0, 2000.0, 1.0, 1.0, speed),
    scalar!(
        "speed_random",
        "速度随机",
        0.0,
        100.0,
        1.0,
        100.0,
        speed_random
    ),
    scalar!("direction", "发射方向", -180.0, 180.0, 1.0, 1.0, direction),
    scalar!("spread", "方向扩散", 0.0, 360.0, 1.0, 1.0, spread),
    scalar!("gravity", "重力", -1000.0, 1000.0, 1.0, 1.0, gravity),
    scalar!("drag", "空气阻力", 0.0, 10.0, 0.01, 1.0, drag),
    scalar!("size", "粒子大小", 0.1, 500.0, 0.1, 1.0, size),
    scalar!(
        "size_random",
        "大小随机",
        0.0,
        100.0,
        1.0,
        100.0,
        size_random
    ),
    scalar!("rotation", "粒子旋转", -720.0, 720.0, 1.0, 1.0, rotation),
    scalar!(
        "rotation_random",
        "旋转随机",
        0.0,
        360.0,
        1.0,
        1.0,
        rotation_random
    ),
    scalar!(
        "color_variation",
        "颜色变化",
        0.0,
        100.0,
        1.0,
        100.0,
        color_variation
    ),
    scalar!(
        "opacity_variation",
        "不透明度变化",
        0.0,
        100.0,
        1.0,
        100.0,
        opacity_variation
    ),
];

fn effect_property<'a>(
    effect: &'a crate::EffectNode,
    key: &str,
) -> Option<&'a mondrian_core::AnimatedProperty> {
    let id = effect.effect_type.parameter_id(key).ok()?;
    effect
        .properties
        .iter()
        .find_map(|(_, property)| (property.descriptor.parameter_id() == &id).then_some(property))
}

fn is_animated(property: &mondrian_core::AnimatedProperty) -> bool {
    property.is_enabled() && property.is_animated()
}

pub(crate) fn graph_builder() -> crate::EffectGraphBuilder {
    std::sync::Arc::new(|effect, context, graph| {
        use mondrian_core::PropertyValue;
        let invalid = |reason: String| crate::EffectGraphBuildError::InvalidAuthorState {
            effect_key: effect.effect_type.key(),
            effect_id: effect.id,
            reason,
        };
        let property = |key| {
            effect_property(effect, key)
                .ok_or_else(|| invalid(format!("missing particle parameter {key}")))
        };
        let shape = property("emitter_shape")?;
        let position = property("emitter_position")?;
        let color = property("color")?;
        let seed = property("seed")?;
        let shape = match shape.evaluate(context.time) {
            PropertyValue::Enum(key) => match key.as_str() {
                "point" => ParticleEmitterShape::Point,
                "line" => ParticleEmitterShape::Line,
                "circle" => ParticleEmitterShape::Circle,
                "rectangle" => ParticleEmitterShape::Rectangle,
                _ => return Err(invalid(format!("invalid particle emitter shape {key}"))),
            },
            _ => return Err(invalid("particle emitter shape must be an enum".to_owned())),
        };
        let blend = match property("blend")?.evaluate(context.time) {
            PropertyValue::Enum(key) => match key.as_str() {
                "add" => ParticleBlend::Add,
                "normal" => ParticleBlend::Normal,
                "screen" => ParticleBlend::Screen,
                "overlay" => ParticleBlend::Overlay,
                _ => return Err(invalid(format!("invalid particle blend {key}"))),
            },
            _ => return Err(invalid("particle blend must be an enum".to_owned())),
        };
        let read_position = |time| match position.evaluate(time) {
            PropertyValue::Vec2(value) => Ok(value.to_array()),
            _ => Err(invalid(
                "particle emitter position must be a Vec2".to_owned(),
            )),
        };
        let read_color = |time| match color.evaluate(time) {
            PropertyValue::Color(value) => Ok([value.r, value.g, value.b, value.a.clamp(0.0, 1.0)]),
            _ => Err(invalid("particle color must be RGBA".to_owned())),
        };
        let read_seed = |time| match seed.evaluate(time) {
            PropertyValue::Int(value) => Ok(value.clamp(0, 9999) as u32),
            _ => Err(invalid("particle seed must be an integer".to_owned())),
        };
        let mut base = ParticleEmitter {
            shape,
            position: read_position(context.time)?,
            color: read_color(context.time)?,
            seed: read_seed(context.time)?,
            ..ParticleEmitter::default()
        };
        let animated_position = is_animated(position);
        let animated_color = is_animated(color);
        let animated_seed = is_animated(seed);
        let mut curves = Vec::new();
        let mut constant_rate = Some(base.rate);
        let mut lifetime_window = i128::from(base.lifetime.ceil() as u32);
        for scalar in PARTICLE_SCALARS {
            let prop = property(scalar.key)?;
            if is_animated(prop) {
                curves.push((scalar, prop));
                if scalar.key == "rate" {
                    constant_rate = None;
                }
                if scalar.key == "lifetime" {
                    lifetime_window = MAX_LIFETIME;
                }
            } else {
                let value = prop.evaluate(context.time).as_f32().ok_or_else(|| {
                    invalid(format!("particle parameter {} must be numeric", scalar.key))
                })?;
                (scalar.write)(&mut base, value);
                if scalar.key == "rate" {
                    constant_rate = Some(base.rate);
                }
                if scalar.key == "lifetime" {
                    lifetime_window = i128::from(base.lifetime.ceil() as u32);
                }
            }
        }
        let sample = |birth| {
            let mut emitter = base;
            for (scalar, prop) in &curves {
                let value = prop.evaluate(birth).as_f32().ok_or_else(|| {
                    invalid(format!("particle parameter {} must be numeric", scalar.key))
                })?;
                (scalar.write)(
                    &mut emitter,
                    value.clamp(scalar.bounds[0], scalar.bounds[1]),
                );
            }
            if animated_position {
                emitter.position = read_position(birth)?;
            }
            if animated_color {
                emitter.color = read_color(birth)?;
            }
            if animated_seed {
                emitter.seed = read_seed(birth)?;
            }
            Ok(emitter)
        };
        let particles =
            simulate_window(context.time, constant_rate, lifetime_window, sample, || {
                Ok(())
            })
            .map_err(|error| match error {
                ControlledParticleError::Particle(error) => invalid(error.to_string()),
                ControlledParticleError::Caller(error) => error,
            })?;
        if !particles.is_empty() {
            graph.append_unary(crate::EffectRenderOp::ParticleSystem {
                particles: particles.into(),
                blend,
            });
        }
        Ok(())
    })
}
/// Edge length of the actual-grid tiles used by the device candidate index.
pub const PARTICLE_TILE_SIZE: u32 = 16;

/// Native emission geometry in the layer's physical-pixel space.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ParticleEmitterShape {
    /// All births originate at the emitter position.
    #[default]
    Point,
    /// Uniform distribution along a horizontal line.
    Line,
    /// Uniform distribution over the interior of a disk.
    Circle,
    /// Uniform distribution inside a centered square.
    Rectangle,
}

/// Birth parameters sampled at a time in the visual Clip author domain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParticleEmitter {
    /// Emitter center relative to the complete layer, in normalized coordinates.
    pub position: [f32; 2],
    /// Distribution of birth positions.
    pub shape: ParticleEmitterShape,
    /// Physical-pixel diameter or side length of the emission region.
    pub emitter_size: f32,
    /// Particles per second.
    pub rate: f32,
    /// Maximum particle age, in seconds.
    pub lifetime: f32,
    /// Fraction by which lifetime can decrease.
    pub lifetime_random: f32,
    /// Launch speed in physical pixels per second.
    pub speed: f32,
    /// Fraction by which speed can vary in either direction.
    pub speed_random: f32,
    /// Launch direction in degrees. Negative 90 points up.
    pub direction: f32,
    /// Total launch cone angle in degrees.
    pub spread: f32,
    /// Downward acceleration in physical pixels per second squared.
    pub gravity: f32,
    /// Linear velocity resistance in inverse seconds.
    pub drag: f32,
    /// Particle major-axis diameter in physical pixels.
    pub size: f32,
    /// Fraction by which diameter can vary in either direction.
    pub size_random: f32,
    /// Particle orientation in degrees.
    pub rotation: f32,
    /// Total random orientation span in degrees.
    pub rotation_random: f32,
    /// Straight working-space RGBA, with alpha in the unit interval.
    pub color: [f32; 4],
    /// Fractional RGB variation for each channel.
    pub color_variation: f32,
    /// Fraction by which alpha can decrease.
    pub opacity_variation: f32,
    /// Authored random seed, independent of render request or frame index.
    pub seed: u32,
}

impl Default for ParticleEmitter {
    fn default() -> Self {
        Self {
            position: [0.5; 2],
            shape: ParticleEmitterShape::Point,
            emitter_size: 20.0,
            rate: 30.0,
            lifetime: 2.0,
            lifetime_random: 0.0,
            speed: 200.0,
            speed_random: 0.2,
            direction: -90.0,
            spread: 30.0,
            gravity: 0.0,
            drag: 0.0,
            size: 6.0,
            size_random: 0.3,
            rotation: 0.0,
            rotation_random: 0.0,
            color: [1.0; 4],
            color_variation: 0.0,
            opacity_variation: 0.0,
            seed: 0,
        }
    }
}

/// Native compositing applied in ascending birth order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ParticleBlend {
    /// Add light in the overlap without clipping HDR values.
    #[default]
    Add,
    /// Normal source-over.
    Normal,
    /// Screen the particle against the backdrop.
    Screen,
    /// Overlay the particle over the backdrop.
    Overlay,
}

impl ParticleBlend {
    /// Stable spatial-pass control code.
    pub const fn gpu_code(self) -> u32 {
        match self {
            Self::Add => 0,
            Self::Normal => 1,
            Self::Screen => 2,
            Self::Overlay => 3,
        }
    }
}

/// Failure to admit author parameters or lay out the actual raster.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParticleError {
    /// One emitter parameter was nonfinite or exceeded its authored range.
    #[error("invalid particle emitter control {0}")]
    InvalidControl(&'static str),
    /// A birth time exceeded the exact timeline carrier.
    #[error(transparent)]
    Time(#[from] mondrian_core::TimelineTimeError),
    /// The actual raster or its pixel buffer was invalid.
    #[error("invalid particle raster or Float32 pixel buffer")]
    InvalidRaster,
    /// There is insufficient space to store the coverage index for this layout.
    #[error("particle tile index exceeds its storage budget")]
    IndexBudget,
    /// The admitted scene payload could not be allocated.
    #[error("could not allocate particle raster storage")]
    Allocation,
}

/// A simulation or its caller's time query/cancellation failed.
#[derive(Debug, thiserror::Error)]
pub enum ControlledParticleError<E> {
    /// The effect rejected its controls or raster.
    #[error(transparent)]
    Particle(#[from] ParticleError),
    /// The caller's query or checkpoint returned this error.
    #[error("particle request cancelled or author-time query failed")]
    Caller(E),
}

impl ParticleEmitter {
    fn validate(self) -> Result<(), ParticleError> {
        for (key, value, range) in [
            ("emitter_size", self.emitter_size, 0.0..=500.0),
            ("rate", self.rate, 0.0..=2_000.0),
            ("lifetime", self.lifetime, 0.1..=30.0),
            ("lifetime_random", self.lifetime_random, 0.0..=1.0),
            ("speed", self.speed, 0.0..=2_000.0),
            ("speed_random", self.speed_random, 0.0..=1.0),
            ("direction", self.direction, -180.0..=180.0),
            ("spread", self.spread, 0.0..=360.0),
            ("gravity", self.gravity, -1_000.0..=1_000.0),
            ("drag", self.drag, 0.0..=10.0),
            ("size", self.size, 0.1..=500.0),
            ("size_random", self.size_random, 0.0..=1.0),
            ("rotation", self.rotation, -720.0..=720.0),
            ("rotation_random", self.rotation_random, 0.0..=360.0),
            ("color_variation", self.color_variation, 0.0..=1.0),
            ("opacity_variation", self.opacity_variation, 0.0..=1.0),
            ("color_alpha", self.color[3], 0.0..=1.0),
        ] {
            if !range.contains(&value) {
                return Err(ParticleError::InvalidControl(key));
            }
        }
        if !self.position.iter().all(|value| value.is_finite()) {
            return Err(ParticleError::InvalidControl("position"));
        }
        if !self.color[..3].iter().all(|value| value.is_finite()) {
            return Err(ParticleError::InvalidControl("color"));
        }
        if self.seed > 9_999 {
            return Err(ParticleError::InvalidControl("seed"));
        }
        Ok(())
    }
}

/// A living particle at the requested author time, before raster sampling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Particle {
    /// Stable birth time, also the particle's ordering key.
    pub birth: TimelineTime,
    /// Normalized anchor retained from birth.
    pub anchor: [f32; 2],
    /// Displacement from the anchor in physical pixels.
    pub offset: [f32; 2],
    /// Major-axis radius in physical pixels.
    pub radius: f32,
    /// Cosine and sine of the orientation.
    pub orientation: [f32; 2],
    /// Straight working-space RGBA, frozen at birth.
    pub color: [f32; 4],
}

impl Particle {
    pub(crate) fn hash_signature(&self, state: &mut impl std::hash::Hasher) {
        use std::hash::Hash as _;
        self.anchor.map(f32::to_bits).hash(state);
        self.offset.map(f32::to_bits).hash(state);
        self.radius.to_bits().hash(state);
        self.orientation.map(f32::to_bits).hash(state);
        self.color.map(f32::to_bits).hash(state);
    }
}

/// Reconstruct the live set in birth order on the exact visual Clip timeline.
///
/// With `constant_rate`, each slot contains the exact difference of two fixed-point
/// emission counts. Otherwise rate is sampled at the start of each 120 Hz slot.
/// In both cases the other controls are sampled at each distinct birth time.
/// Evaluation only visits emission slots within the preceding 30 seconds.
pub fn simulate_particles<E>(
    time: TimelineTime,
    constant_rate: Option<f32>,
    sample: impl FnMut(TimelineTime) -> Result<ParticleEmitter, E>,
    checkpoint: impl FnMut() -> Result<(), E>,
) -> Result<Vec<Particle>, ControlledParticleError<E>> {
    simulate_window(time, constant_rate, MAX_LIFETIME, sample, checkpoint)
}

fn simulate_window<E>(
    time: TimelineTime,
    constant_rate: Option<f32>,
    lifetime_window: i128,
    mut sample: impl FnMut(TimelineTime) -> Result<ParticleEmitter, E>,
    mut checkpoint: impl FnMut() -> Result<(), E>,
) -> Result<Vec<Particle>, ControlledParticleError<E>> {
    let denominator = i128::from(time.denominator());
    let numerator = i128::from(time.numerator());
    let end_tick = numerator * EMISSION_HZ / denominator;
    let start_tick = (end_tick - lifetime_window * EMISSION_HZ).max(0);
    let rate = constant_rate.map(rate_units).transpose()?;
    let mut particles = Vec::new();
    if numerator <= 0 || rate == Some(0) {
        return Ok(particles);
    }

    for tick in start_tick..=end_tick {
        checkpoint().map_err(ControlledParticleError::Caller)?;
        let count = if let Some(rate) = rate {
            let unit = EMISSION_HZ * RATE_SCALE;
            (rate * (tick + 1) / unit - rate * tick / unit) as u32
        } else {
            let tick_time = timeline_time(tick, EMISSION_HZ)?;
            let rate =
                rate_units(sample(tick_time).map_err(ControlledParticleError::Caller)?.rate)?;
            let unit = EMISSION_HZ * RATE_SCALE;
            // Digit reversal provides a deterministic low-discrepancy phase in [0, 1).
            let phase = (tick as u64).reverse_bits();
            let fractional_birth = (i128::from(phase) * unit) >> 64;
            ((rate + fractional_birth) / unit) as u32
        };
        for slot in 0..count {
            let center = i128::from(2 * slot + 1) * SUBTICKS / i128::from(2 * count);
            let event = tick * SUBTICKS + center;
            let age_numerator = numerator * TIME_SCALE - event * denominator;
            let age_denominator = denominator * TIME_SCALE;
            if age_numerator < 0 || age_numerator >= MAX_LIFETIME * age_denominator {
                continue;
            }
            let birth = timeline_time(event, TIME_SCALE)?;
            let emitter = sample(birth).map_err(ControlledParticleError::Caller)?;
            emitter.validate()?;
            let age = age_numerator as f64 / age_denominator as f64;
            if let Some(particle) = particle_at(birth, event, emitter, age) {
                particles.push(particle);
            }
        }
    }
    Ok(particles)
}

fn timeline_time(numerator: i128, denominator: i128) -> Result<TimelineTime, ParticleError> {
    // Reduce in the wider domain before entering the exact i64 carrier.
    let mut divisor = denominator;
    let mut remainder = numerator % denominator;
    while remainder != 0 {
        (divisor, remainder) = (remainder, divisor % remainder);
    }
    let ticks = i64::try_from(numerator / divisor)
        .map_err(|_| mondrian_core::TimelineTimeError::Overflow)?;
    Ok(TimelineTime::new(ticks, (denominator / divisor) as i64)?)
}

fn rate_units(rate: f32) -> Result<i128, ParticleError> {
    if !(0.0..=2_000.0).contains(&rate) {
        return Err(ParticleError::InvalidControl("rate"));
    }
    Ok((f64::from(rate) * RATE_SCALE as f64).round() as i128)
}

fn particle_at(
    birth: TimelineTime,
    event: i128,
    grade: ParticleEmitter,
    age: f64,
) -> Option<Particle> {
    let unit = |channel: u64| {
        let mut bits = (event as u64) ^ ((event >> 64) as u64).rotate_left(29);
        bits ^= u64::from(grade.seed) << 32;
        bits = bits.wrapping_add(channel.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        bits = (bits ^ (bits >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        bits = (bits ^ (bits >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((bits ^ (bits >> 31)) >> 40) as f64 / 16_777_216.0
    };
    let signed = |channel| 2.0 * unit(channel) - 1.0;
    let lifetime = f64::from(grade.lifetime) * (1.0 - f64::from(grade.lifetime_random) * unit(1));
    if age >= lifetime {
        return None;
    }

    let launch_radius = f64::from(grade.emitter_size) * 0.5;
    let origin = match grade.shape {
        ParticleEmitterShape::Point => [0.0, 0.0],
        ParticleEmitterShape::Line => [launch_radius * signed(2), 0.0],
        ParticleEmitterShape::Circle => {
            let (sine, cosine) = (TAU * unit(2)).sin_cos();
            let radius = launch_radius * unit(3).sqrt();
            [cosine * radius, sine * radius]
        }
        ParticleEmitterShape::Rectangle => [launch_radius * signed(2), launch_radius * signed(3)],
    };
    let direction = f64::from(grade.direction) + f64::from(grade.spread) * (unit(4) - 0.5);
    let (dy, dx) = direction.to_radians().sin_cos();
    let speed = f64::from(grade.speed) * (1.0 + f64::from(grade.speed_random) * signed(5));
    let (travel, fall) = motion_factors(f64::from(grade.drag), age);
    let offset = [
        (origin[0] + dx * speed * travel) as f32,
        (origin[1] + dy * speed * travel + f64::from(grade.gravity) * fall) as f32,
    ];
    let radius = f64::from(grade.size) * (1.0 + f64::from(grade.size_random) * signed(6)) * 0.5;
    let rotation = f64::from(grade.rotation) + f64::from(grade.rotation_random) * (unit(7) - 0.5);
    let (sine, cosine) = rotation.to_radians().sin_cos();
    let mut color = grade.color;
    for (index, component) in color[..3].iter_mut().enumerate() {
        *component *= (1.0 + f64::from(grade.color_variation) * signed(8 + index as u64)) as f32;
    }
    color[3] *= (1.0 - f64::from(grade.opacity_variation) * unit(11)) as f32;
    Some(Particle {
        birth,
        anchor: grade.position,
        offset,
        radius: radius as f32,
        orientation: [cosine as f32, sine as f32],
        color,
    })
}

fn motion_factors(drag: f64, age: f64) -> (f64, f64) {
    let x = drag * age;
    if x < 1e-3 {
        let travel = age * (1.0 - x * (0.5 - x * (1.0 / 6.0 - x / 24.0)));
        let fall = age * age * (0.5 - x * (1.0 / 6.0 - x * (1.0 / 24.0 - x / 120.0)));
        return (travel, fall);
    }
    let travel = -(-x).exp_m1() / drag;
    (travel, (age - travel) / drag)
}

/// A visible particle expressed in actual sample coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterParticle {
    /// Edge-origin center in output texels.
    pub center: [f32; 2],
    /// Physical major-axis radius.
    pub radius: f32,
    /// Cosine and sine used to rotate the physical sample offset.
    pub orientation: [f32; 2],
    /// Straight working-space coverage color.
    pub color: [f32; 4],
    /// Clipped conservative footprint, in complete-frame coordinates.
    pub bounds: EffectPixelRoi,
}

/// The actual-grid geometry used for both clipped CPU draws and device indexing.
#[derive(Clone, Debug, PartialEq)]
pub struct ParticleRaster {
    extent: EffectFrameExtent,
    physical_per_sample: [f32; 2],
    antialias_radius: f32,
    particles: Vec<RasterParticle>,
}

/// Packed offsets followed by birth-ordered particle indices for each tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParticleTileIndex {
    columns: u32,
    words: Vec<u32>,
}

impl ParticleTileIndex {
    /// Number of tiles in each full-frame row.
    pub const fn columns(&self) -> u32 {
        self.columns
    }

    /// For tile `t`, the entries occupy `words[words[t] .. words[t + 1]]`.
    pub fn words(&self) -> &[u32] {
        &self.words
    }
}

impl ParticleRaster {
    /// Full extent to which the particle payload was bound.
    pub const fn extent(&self) -> EffectFrameExtent {
        self.extent
    }

    /// Project from normalized anchors and physical offsets onto a complete raster.
    pub fn prepare(
        particles: &[Particle],
        extent: EffectFrameExtent,
        sampling: ImageSampling,
    ) -> Result<Self, ParticleError> {
        let scale = sampling.render_scale().map(|ratio| ratio.num as f64 / ratio.den as f64);
        let par = sampling.sample_aspect_ratio();
        let par = f64::from(par.numerator()) / f64::from(par.denominator());
        let physical = [par / scale[0], 1.0 / scale[1]];
        let physical_per_sample = physical.map(|distance| distance as f32);
        let antialias_radius = (physical[0].hypot(physical[1]) * 0.5) as f32;
        if !antialias_radius.is_finite() || antialias_radius <= 0.0 {
            return Err(ParticleError::InvalidRaster);
        }
        let limits = [extent.width(), extent.height()];
        let mut output = Self {
            extent,
            physical_per_sample,
            antialias_radius,
            particles: Vec::new(),
        };
        if extent.is_empty() {
            return Ok(output);
        }
        output
            .particles
            .try_reserve_exact(particles.len())
            .map_err(|_| ParticleError::Allocation)?;
        for p in particles {
            if !p
                .anchor
                .iter()
                .chain(&p.offset)
                .chain(&p.orientation)
                .chain(&p.color)
                .chain(std::iter::once(&p.radius))
                .all(|value| value.is_finite())
            {
                return Err(ParticleError::InvalidRaster);
            }
            if p.color[3] <= 0.0 || p.radius <= 0.0 {
                continue;
            }
            let center = std::array::from_fn::<_, 2, _>(|i| {
                f64::from(p.anchor[i]) * f64::from(limits[i]) + f64::from(p.offset[i]) / physical[i]
            });
            let support =
                f64::from(p.radius) + f64::from(antialias_radius / PARTICLE_ELLIPSE_ASPECT);
            let limits = std::array::from_fn::<_, 2, _>(|i| {
                let r = support / physical[i];
                let clip = |value: f64| value.clamp(0.0, f64::from(limits[i])) as u32;
                [clip((center[i] - r).floor()), clip((center[i] + r).ceil())]
            });
            let bounds = EffectPixelRoi::new(
                limits[0][0],
                limits[1][0],
                limits[0][1] - limits[0][0],
                limits[1][1] - limits[1][0],
            );
            if bounds.width() != 0 && bounds.height() != 0 {
                output.particles.push(RasterParticle {
                    center: center.map(|n| n as f32),
                    radius: p.radius,
                    orientation: p.orientation,
                    color: p.color,
                    bounds,
                });
            }
        }
        Ok(output)
    }

    /// Construct a bounded actual-grid candidate index without sorting or changing draw order.
    pub fn tile_index<E>(
        &self,
        max_bytes: u64,
        mut checkpoint: impl FnMut() -> Result<(), E>,
    ) -> Result<ParticleTileIndex, ControlledParticleError<E>> {
        let columns = self.extent.width().div_ceil(PARTICLE_TILE_SIZE);
        let rows = self.extent.height().div_ceil(PARTICLE_TILE_SIZE);
        let tiles = columns.checked_mul(rows).ok_or(ParticleError::IndexBudget)?;
        let header = tiles.checked_add(1).ok_or(ParticleError::IndexBudget)?;
        let max_words = (max_bytes / u64::from(u32::BITS / 8)).min(u64::from(u32::MAX));
        if u64::from(header) > max_words {
            return Err(ParticleError::IndexBudget.into());
        }
        if self.particles.is_empty() {
            return Ok(ParticleTileIndex { columns, words: Vec::new() });
        }

        let tile_span = |bounds: EffectPixelRoi| {
            let left = bounds.x() / PARTICLE_TILE_SIZE;
            let top = bounds.y() / PARTICLE_TILE_SIZE;
            let right = (bounds.x() + bounds.width()).div_ceil(PARTICLE_TILE_SIZE);
            let bottom = (bounds.y() + bounds.height()).div_ceil(PARTICLE_TILE_SIZE);
            (left..right, top..bottom)
        };
        let mut capacity = u64::from(header);
        let mut offsets = vec![0_u32; header as usize];
        for p in &self.particles {
            checkpoint().map_err(ControlledParticleError::Caller)?;
            let (xs, ys) = tile_span(p.bounds);
            capacity += (xs.len() as u64) * (ys.len() as u64);
            if capacity > max_words {
                return Err(ParticleError::IndexBudget.into());
            }
            for y in ys {
                for x in xs.clone() {
                    offsets[(y * columns + x) as usize + 1] += 1;
                }
            }
        }

        offsets[0] = header;
        for tile in 1..offsets.len() {
            offsets[tile] += offsets[tile - 1];
        }
        let mut words = vec![0; capacity as usize];
        words[..offsets.len()].copy_from_slice(&offsets);
        for (index, p) in self.particles.iter().enumerate() {
            checkpoint().map_err(ControlledParticleError::Caller)?;
            let (xs, ys) = tile_span(p.bounds);
            for y in ys {
                for x in xs.clone() {
                    let cursor = &mut offsets[(y * columns + x) as usize];
                    words[*cursor as usize] = index as u32;
                    *cursor += 1;
                }
            }
        }
        Ok(ParticleTileIndex { columns, words })
    }

    /// Visible particles in the same order used for per-pixel blending.
    pub fn particles(&self) -> &[RasterParticle] {
        &self.particles
    }

    /// Physical-pixel distances between neighboring samples.
    pub const fn physical_per_sample(&self) -> [f32; 2] {
        self.physical_per_sample
    }

    /// Half of the physical sample diagonal used for matte coverage.
    pub const fn antialias_radius(&self) -> f32 {
        self.antialias_radius
    }

    /// Composite over a tightly packed straight-RGBA ROI, preserving invisible input exactly.
    pub fn apply_region_f32<E>(
        &self,
        pixels: &mut [[f32; 4]],
        roi: EffectPixelRoi,
        mode: ParticleBlend,
        mut checkpoint: impl FnMut() -> Result<(), E>,
    ) -> Result<(), ControlledParticleError<E>> {
        let Some(right) = roi.x().checked_add(roi.width()) else {
            return Err(ParticleError::InvalidRaster.into());
        };
        let Some(bottom) = roi.y().checked_add(roi.height()) else {
            return Err(ParticleError::InvalidRaster.into());
        };
        let width = roi.width() as usize;
        if right > self.extent.width()
            || bottom > self.extent.height()
            || width.checked_mul(roi.height() as usize) != Some(pixels.len())
        {
            return Err(ParticleError::InvalidRaster.into());
        }
        if pixels.is_empty() {
            return Ok(());
        }
        for p in &self.particles {
            checkpoint().map_err(ControlledParticleError::Caller)?;
            let x0 = roi.x().max(p.bounds.x());
            let x1 = right.min(p.bounds.x() + p.bounds.width());
            if x0 >= x1 {
                continue;
            }
            let y0 = roi.y().max(p.bounds.y());
            let y1 = bottom.min(p.bounds.y() + p.bounds.height());
            for y in y0..y1 {
                let row = (y - roi.y()) as usize * width;
                for (chunk, columns) in (x0..x1).step_by(4096).zip(
                    pixels[row + (x0 - roi.x()) as usize..row + (x1 - roi.x()) as usize]
                        .chunks_mut(4096),
                ) {
                    checkpoint().map_err(ControlledParticleError::Caller)?;
                    for (x, pixel) in (chunk..).zip(columns) {
                        let alpha = self.coverage(p, [x as f32 + 0.5, y as f32 + 0.5]);
                        if alpha > 0.0 {
                            *pixel = blend_particle(*pixel, p.color, alpha, mode);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn coverage(&self, particle: &RasterParticle, position: [f32; 2]) -> f32 {
        let dx = (position[0] - particle.center[0]) * self.physical_per_sample[0];
        let dy = (position[1] - particle.center[1]) * self.physical_per_sample[1];
        let [cosine, sine] = particle.orientation;
        let x = cosine * dx + sine * dy;
        let y = cosine * dy - sine * dx;
        let scaled_x = x * PARTICLE_ELLIPSE_ASPECT;
        let distance =
            (scaled_x * scaled_x + y * y).sqrt() - particle.radius * PARTICLE_ELLIPSE_ASPECT;
        // Subpixel ellipses fade proportionally to their physical area.
        let weight = (particle.radius * particle.radius * PARTICLE_ELLIPSE_ASPECT
            / (self.antialias_radius * self.antialias_radius))
            .min(1.0);
        (0.5 - 0.5 * distance / self.antialias_radius).clamp(0.0, 1.0) * weight
    }
}

fn blend_particle(base: [f32; 4], color: [f32; 4], coverage: f32, mode: ParticleBlend) -> [f32; 4] {
    let mode = match mode {
        ParticleBlend::Add => {
            let alpha = color[3] * coverage;
            if !has_positive_coverage(alpha) {
                return base;
            }
            let base_alpha = base[3].clamp(0.0, 1.0);
            if !has_positive_coverage(base_alpha) {
                return [color[0], color[1], color[2], alpha];
            }
            let output_alpha = alpha + base_alpha * (1.0 - alpha);
            return [
                (base[0] * base_alpha + color[0] * alpha) / output_alpha,
                (base[1] * base_alpha + color[1] * alpha) / output_alpha,
                (base[2] * base_alpha + color[2] * alpha) / output_alpha,
                output_alpha,
            ];
        }
        ParticleBlend::Normal => BlendMode::Normal,
        ParticleBlend::Screen => BlendMode::Screen,
        ParticleBlend::Overlay => BlendMode::Overlay,
    };
    blend_rgba_f32_pixel(base, color, coverage, mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{Rational, SampleAspectRatio};

    fn time(numerator: i64, denominator: i64) -> TimelineTime {
        TimelineTime::new(numerator, denominator).unwrap()
    }

    fn constant_at(time: TimelineTime, emitter: ParticleEmitter) -> Vec<Particle> {
        simulate_particles(
            time,
            Some(emitter.rate),
            |_| Ok(emitter),
            || Ok::<_, ()>(()),
        )
        .unwrap()
    }

    #[test]
    fn authored_particle_graph_replays_after_a_seek_and_lowers_to_the_actual_grid() {
        use crate::{
            apply_compiled_effect_graph_rgba_f32, instantiate_effect_node,
            lower_effect_graph_to_gpu_pipeline, EffectColorDomain, EffectFloatFrameView,
            EffectGpuSpatialPass, EffectGpuStage, EffectType, EffectValueFormat,
            EffectWorkingPrecision, PreparedEffectProgram,
        };
        use mondrian_core::{
            ColorFrameAlpha, PropertyHost, PropertyMutation, PropertyValue, WorkingColorSpace,
        };

        let kind = EffectType::ParticleSystem;
        let mut effect = instantiate_effect_node(kind.clone()).unwrap();
        for (parameter, value) in [
            ("rate", PropertyValue::Float(48.0)),
            ("speed", PropertyValue::Float(18.0)),
            ("emitter_shape", PropertyValue::Enum("circle".into())),
        ] {
            effect
                .apply_property_mutation(PropertyMutation::SetStaticValue {
                    path: kind.property_path(parameter),
                    value,
                })
                .unwrap();
        }
        let space = WorkingColorSpace::LinearRec709;
        let prepared = PreparedEffectProgram::prepare(&[effect], &[], space).unwrap();
        let request = time(3, 2);
        let initial = prepared.evaluate(request).unwrap();
        let later = prepared.evaluate(time(5, 2)).unwrap();
        let replayed = prepared.evaluate(time(90_000, 60_000)).unwrap();
        let empty = prepared.evaluate(TimelineTime::ZERO).unwrap();
        let extent = EffectFrameExtent::new(72, 49);
        let pixels = vec![[0.0; 4]; 72 * 49];
        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            ColorFrameAlpha::StraightCoverage,
        );
        let sampling = ImageSampling::UNIT_SQUARE;
        let render = |graph| {
            apply_compiled_effect_graph_rgba_f32(
                EffectFloatFrameView::new(extent, format, sampling, &pixels),
                graph,
                0,
            )
            .unwrap()
        };
        let frame = render(&initial);
        assert!(frame.pixels().iter().any(|pixel| pixel[3] > 0.01));
        assert_eq!(frame.pixels(), render(&replayed).pixels());
        assert_ne!(frame.pixels(), render(&later).pixels());
        assert_eq!(render(&empty).pixels(), pixels);
        let pipeline = lower_effect_graph_to_gpu_pipeline(&initial).unwrap();
        assert!(pipeline.requires_image_sampling());
        assert!(matches!(pipeline.with_image_sampling(sampling).stages(),
            [EffectGpuStage::Spatial { pass: EffectGpuSpatialPass::ParticleSystem {
                sampling: Some(bound), ..
            }, .. }] if *bound == sampling));
    }

    #[test]
    fn constant_rate_rebuilds_only_the_exact_live_window_at_all_request_orders() {
        let emitter = ParticleEmitter { rate: 30.5, ..Default::default() };
        let t = time(75, 1);
        let initial = constant_at(t, emitter);
        assert_eq!(initial.len(), 61);
        assert!(initial.iter().all(|p| p.birth > time(73, 1) && p.birth <= t));
        assert!(initial.windows(2).all(|w| w[0].birth < w[1].birth));
        assert!(constant_at(TimelineTime::ZERO, emitter).is_empty());
        assert!(constant_at(TimelineTime::NEGATIVE_ONE_THIRD, emitter).is_empty());
        let _ = constant_at(time(8, 3), emitter);
        assert_eq!(initial, constant_at(t, emitter));
        assert_eq!(
            constant_at(time(3, 2), emitter),
            constant_at(time(90_000, 60_000), emitter)
        );
    }

    #[test]
    fn sampled_rate_and_launch_controls_respect_birth_time_across_seeks() {
        let frame = |t| {
            simulate_particles(
                t,
                None,
                |birth| {
                    let early = birth < TimelineTime::ONE;
                    Ok(ParticleEmitter {
                        rate: if early { 120.0 } else { 240.0 },
                        position: if early { [0.25, 0.5] } else { [0.75, 0.5] },
                        speed: 0.0,
                        ..Default::default()
                    })
                },
                || Ok::<_, ()>(()),
            )
            .unwrap()
        };
        let middle = frame(time(3, 2));
        assert_eq!(middle.len(), 240);
        assert_eq!(&middle[..120], frame(TimelineTime::ONE));
        assert!(middle[..120].iter().all(|p| p.anchor[0] == 0.25));
        assert!(middle[120..].iter().all(|p| p.anchor[0] == 0.75));
        assert_eq!(middle, frame(time(3, 2)));
    }

    #[test]
    fn saturated_emission_and_clock_errors_are_bounded() {
        let emitter = ParticleEmitter { rate: 2000.0, lifetime: 30.0, ..Default::default() };
        let late = time(1_000_000_001, 1);
        assert_eq!(constant_at(late, emitter).len(), 60_000);
        let mut query_count = 0;
        let mut checkpoints = 0;
        let error = simulate_particles(
            late,
            None,
            |_| {
                query_count += 1;
                Ok(emitter)
            },
            || {
                checkpoints += 1;
                if checkpoints > 2 {
                    Err("cancel")
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        assert!(matches!(error, ControlledParticleError::Caller("cancel")));
        assert!(query_count <= 36);
        let error = simulate_particles(late, Some(f32::NAN), |_| Ok(emitter), || Ok::<_, ()>(()))
            .unwrap_err();
        assert!(matches!(
            error,
            ControlledParticleError::Particle(ParticleError::InvalidControl("rate"))
        ));
    }

    #[test]
    fn closed_form_flight_matches_zero_drag_limit_and_exact_terminal_motion() {
        assert_eq!(motion_factors(0.0, 2.0), (2.0, 2.0));
        let (travel, fall) = motion_factors(1e-12, 2.0);
        assert!((travel - 2.0).abs() < 3e-12);
        assert!((fall - 2.0).abs() < 2e-12);
        let (travel, fall) = motion_factors(2.0, 1.0);
        assert!((travel - 0.432_332_358_381_693_65).abs() < 1e-15);
        assert!((fall - 0.283_833_820_809_153_2).abs() < 1e-15);
    }

    #[test]
    fn seed_and_shape_are_stable_and_distribute_in_declared_physical_bounds() {
        let emitter = ParticleEmitter {
            rate: 120.0,
            speed: 0.0,
            shape: ParticleEmitterShape::Circle,
            ..Default::default()
        };
        let frame = constant_at(TimelineTime::ONE, emitter);
        assert!(frame.iter().all(|p| p.offset[0].hypot(p.offset[1]) <= 10.0));
        assert!(frame.iter().any(|p| p.offset[0].hypot(p.offset[1]) > 9.0));
        let changed = constant_at(TimelineTime::ONE, ParticleEmitter { seed: 1, ..emitter });
        assert_ne!(frame, changed);
        assert!(frame.iter().zip(changed).all(|(a, b)| a.birth == b.birth));
    }

    #[test]
    fn raster_tiles_match_full_frame_with_hdr_and_fractional_anisotropic_sampling() {
        let extent = EffectFrameExtent::new(47, 31);
        let sampling = ImageSampling::new(
            SampleAspectRatio::new(4, 3).unwrap(),
            [Rational::new(1, 2), Rational::new(2, 3)],
        )
        .unwrap();
        let emitter = ParticleEmitter {
            speed: 0.0,
            emitter_size: 500.0,
            shape: ParticleEmitterShape::Circle,
            size: 30.0,
            rotation: 37.0,
            color: [1.8, 0.3, -0.2, 0.35],
            ..Default::default()
        };
        let particles = constant_at(time(3, 2), emitter);
        let layout = ParticleRaster::prepare(&particles, extent, sampling).unwrap();
        assert_eq!(layout.physical_per_sample(), [8.0 / 3.0, 1.5]);
        assert!(!layout.particles().is_empty());
        let background = [0.25, 0.4, 0.1, 0.3];
        let roi = EffectPixelRoi::new(17, 11, 23, 17);
        for mode in [
            ParticleBlend::Normal,
            ParticleBlend::Add,
            ParticleBlend::Screen,
            ParticleBlend::Overlay,
        ] {
            let mut full = vec![background; 47 * 31];
            let mut tile = vec![background; 23 * 17];
            layout
                .apply_region_f32(&mut full, extent.full_frame_roi(), mode, || Ok::<_, ()>(()))
                .unwrap();
            layout.apply_region_f32(&mut tile, roi, mode, || Ok::<_, ()>(())).unwrap();
            assert!(full.iter().any(|&px| px != background));
            for (y, row) in tile.chunks_exact(23).enumerate() {
                let start = (y + 11) * 47 + 17;
                assert_eq!(row, &full[start..start + 23]);
            }
        }
    }

    #[test]
    fn adding_light_preserves_hdr_and_zero_coverage_preserves_hidden_rgb() {
        let base = [1.5, -0.5, 3.0, 1.0];
        let color = [2.0, 0.5, 0.2, 0.5];
        assert_eq!(
            blend_particle(base, color, 0.5, ParticleBlend::Add),
            [2.0, -0.375, 3.05, 1.0]
        );
        let hidden = [7.0, 1.0, -8.0, 0.0];
        assert_eq!(
            blend_particle(hidden, color, 0.0, ParticleBlend::Add),
            hidden
        );
        assert_eq!(
            blend_particle(hidden, color, 0.5, ParticleBlend::Add),
            [2.0, 0.5, 0.2, 0.25]
        );
    }

    #[test]
    fn tile_index_matches_a_brute_force_bound_scan_in_birth_order() {
        let frame = constant_at(
            time(1, 1),
            ParticleEmitter {
                shape: ParticleEmitterShape::Rectangle,
                emitter_size: 125.0,
                speed: 0.0,
                rate: 220.0,
                size: 30.0,
                ..Default::default()
            },
        );
        let extent = EffectFrameExtent::new(97, 71);
        let layout = ParticleRaster::prepare(&frame, extent, ImageSampling::UNIT_SQUARE).unwrap();
        let tiled = layout.tile_index(128 * 1024, || Ok::<_, ()>(())).unwrap();
        let words = tiled.words();
        let columns = tiled.columns();
        let rows = extent.height().div_ceil(PARTICLE_TILE_SIZE);
        assert_eq!(columns, 7);
        assert_eq!(rows, 5);
        assert_eq!(words[0], columns * rows + 1);
        assert_eq!(words[(columns * rows) as usize] as usize, words.len());
        for y in 0..rows {
            for x in 0..columns {
                let left = x * PARTICLE_TILE_SIZE;
                let top = y * PARTICLE_TILE_SIZE;
                let expected: Vec<u32> = layout
                    .particles()
                    .iter()
                    .enumerate()
                    .filter_map(|(i, p)| {
                        (p.bounds.x() < left + PARTICLE_TILE_SIZE
                            && p.bounds.y() < top + PARTICLE_TILE_SIZE
                            && p.bounds.x() + p.bounds.width() > left
                            && p.bounds.y() + p.bounds.height() > top)
                            .then_some(i as u32)
                    })
                    .collect();
                let slot = (y * columns + x) as usize;
                assert_eq!(
                    &words[words[slot] as usize..words[slot + 1] as usize],
                    expected
                );
            }
        }
        let just_under = words.len() as u64 * 4 - 1;
        assert!(matches!(
            layout.tile_index(just_under, || Ok::<_, ()>(())),
            Err(ControlledParticleError::Particle(
                ParticleError::IndexBudget
            ))
        ));
    }
}
