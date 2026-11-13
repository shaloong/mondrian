//! Float32 bump-map refraction and direct lighting for Glass.

use crate::auxiliary::{sample_auxiliary_associated_f32, BILINEAR_TAPS, PIXEL_CENTRE};
use crate::{
    AuxiliaryFloatFrame, AuxiliarySampling, EffectFloatExecutionError, EffectFloatFrame,
    EffectFloatFrameError, EffectFrameExtent, EffectProcessorControl, EffectRasterBounds,
};
use mondrian_core::{ImageSampling, SceneLightFalloff, SceneLightSample, SceneLightSource};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Last checked host hard limits for the material's roughness.
pub const GLASS_ROUGHNESS_MIN: f32 = 0.000_991_821_3;
pub const GLASS_ROUGHNESS_DEFAULT: f32 = 0.024_993_896;
const MAX_BLUR_RADIUS: f32 = 12.0;
const MAX_HEIGHT_REDUCTION: u32 = 64;
const MAX_LIGHT_COMPONENT: f32 = 65_536.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum GlassChannel {
    Red,
    Green,
    Blue,
    Alpha,
    Luminance,
    Lightness,
}

impl GlassChannel {
    pub const OPTIONS: [(&'static str, &'static str, Self); 6] = [
        ("red", "红色", Self::Red),
        ("green", "绿色", Self::Green),
        ("blue", "蓝色", Self::Blue),
        ("alpha", "Alpha", Self::Alpha),
        ("luminance", "亮度", Self::Luminance),
        ("lightness", "明度", Self::Lightness),
    ];

    pub fn from_key(key: &str) -> Option<Self> {
        Self::OPTIONS
            .into_iter()
            .find(|(id, _, _)| *id == key)
            .map(|(_, _, channel)| channel)
    }

    fn height(self, pixel: [f32; 4]) -> f32 {
        let alpha = pixel[3].clamp(0.0, 1.0);
        if self == Self::Alpha {
            return alpha;
        }
        if alpha == 0.0 {
            return 0.0;
        }
        let [r, g, b, _] = pixel.map(|value| (value / alpha).clamp(0.0, 1.0));
        alpha
            * match self {
                Self::Red => r,
                Self::Green => g,
                Self::Blue => b,
                Self::Alpha => unreachable!("alpha is handled before color extraction"),
                Self::Luminance => (r * 0.2126 + g * 0.7152) + b * 0.0722,
                Self::Lightness => 0.5 * (r.min(g).min(b) + r.max(g).max(b)),
            }
    }
}

/// The effect's independent light. Its point position is normalized in the
/// original layer so the default remains the center for every source size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GlassEffectLight {
    Distant { direction_degrees: f32, height: f32 },
    Point { position: [f32; 2], height: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlassMaterial {
    pub ambient: f32,
    pub diffuse: f32,
    pub specular: f32,
    pub roughness: f32,
    pub metal: f32,
}

/// Author controls evaluated at the effect's owner-local time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlassGrade {
    channel: GlassChannel,
    softness: f32,
    height: f32,
    displacement: f32,
    intensity: f32,
    color: [f32; 3],
    effect_light: GlassEffectLight,
    material: GlassMaterial,
    use_composition_lights: bool,
}

impl GlassGrade {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        channel: GlassChannel,
        softness: f32,
        height: f32,
        displacement: f32,
        intensity: f32,
        color: [f32; 3],
        effect_light: GlassEffectLight,
        material: GlassMaterial,
        use_composition_lights: bool,
    ) -> Result<Self, GlassError> {
        let bounded = [
            ("softness", softness, 0.0, 500.0),
            ("height", height, -100.0, 100.0),
            ("displacement", displacement, -500.0, 500.0),
            ("light_intensity", intensity, 0.0, 1000.0),
            ("ambient", material.ambient, 0.0, 200.0),
            ("diffuse", material.diffuse, 0.0, 100.0),
            ("specular", material.specular, 0.0, 100.0),
            ("roughness", material.roughness, GLASS_ROUGHNESS_MIN, 0.5),
            ("metal", material.metal, 0.0, 100.0),
        ];
        for (name, value, min, max) in bounded {
            if !(min..=max).contains(&value) {
                return Err(GlassError::InvalidControl(name));
            }
        }
        let (light_height, valid_position) = match effect_light {
            GlassEffectLight::Distant { height, direction_degrees } => {
                (height, direction_degrees.is_finite())
            }
            GlassEffectLight::Point { height, position } => {
                (height, position.into_iter().all(f32::is_finite))
            }
        };
        if !valid_position || !(-100.0..=100.0).contains(&light_height) {
            return Err(GlassError::InvalidControl("light_geometry"));
        }
        if !color.into_iter().all(f32::is_finite) {
            return Err(GlassError::InvalidControl("light_color"));
        }
        Ok(Self {
            channel,
            softness,
            height,
            displacement,
            intensity,
            color,
            effect_light,
            material,
            use_composition_lights,
        })
    }

    pub const fn uses_composition_lights(self) -> bool {
        self.use_composition_lights
    }

    pub(crate) fn hash_state(self, state: &mut impl Hasher) {
        self.channel.hash(state);
        self.use_composition_lights.hash(state);
        [
            self.softness,
            self.height,
            self.displacement,
            self.material.ambient,
            self.material.diffuse,
            self.material.specular,
            self.material.roughness,
            self.material.metal,
        ]
        .map(f32::to_bits)
        .hash(state);
        if !self.use_composition_lights {
            self.intensity.to_bits().hash(state);
            self.color.map(f32::to_bits).hash(state);
            match self.effect_light {
                GlassEffectLight::Distant { direction_degrees, height } => {
                    false.hash(state);
                    [direction_degrees, height].map(f32::to_bits).hash(state);
                }
                GlassEffectLight::Point { position, height } => {
                    true.hash(state);
                    [position[0], position[1], height].map(f32::to_bits).hash(state);
                }
            }
        }
    }
}

impl Default for GlassGrade {
    fn default() -> Self {
        Self {
            channel: GlassChannel::Lightness,
            softness: 20.0,
            height: 25.0,
            displacement: 100.0,
            intensity: 100.0,
            color: [1.0; 3],
            effect_light: GlassEffectLight::Distant { direction_degrees: -45.0, height: 65.0 },
            material: GlassMaterial {
                ambient: 50.0,
                diffuse: 50.0,
                specular: 50.0,
                roughness: GLASS_ROUGHNESS_DEFAULT,
                metal: 100.0,
            },
            use_composition_lights: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GlassError {
    #[error("invalid Glass control {0}")]
    InvalidControl(&'static str),
    #[error("Glass composition lighting requires a frozen Sequence sample")]
    MissingSceneLights,
    #[error("Glass composition lighting requires the actual layer projection")]
    MissingProjection,
    #[error("Glass layer projection must be finite and invertible")]
    InvalidProjection,
    #[error("Glass light contains invalid geometry or values")]
    InvalidLight,
    #[error("Glass height pyramid exceeds its raster dimensions")]
    Extent,
}

/// One actual source raster's affine map into the owning Sequence's physical
/// pixels. X includes the Sequence pixel aspect and both axes include the
/// output sampling density. The map already includes picture interpretation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlassProjection([f32; 6]);

impl GlassProjection {
    pub fn new(transform: [f32; 6], output_sampling: ImageSampling) -> Result<Self, GlassError> {
        let [sx, sy] = output_sampling.render_scale();
        let factors = [
            output_sampling.sample_aspect_ratio().to_f64() / sx.to_f64(),
            1.0 / sy.to_f64(),
        ];
        let affine =
            std::array::from_fn(|index| (f64::from(transform[index]) * factors[index / 3]) as f32);
        Self::from_physical(affine)
    }

    fn from_physical(affine: [f32; 6]) -> Result<Self, GlassError> {
        let [a, b, _, c, d, _] = affine.map(f64::from);
        let determinant = a * d - b * c;
        if !affine.into_iter().all(f32::is_finite)
            || determinant == 0.0
            || ![d, -c, -b, a]
                .map(|value| (value / determinant) as f32)
                .into_iter()
                .all(f32::is_finite)
        {
            return Err(GlassError::InvalidProjection);
        }
        Ok(Self(affine))
    }

    fn normal_gradient(self) -> [f32; 4] {
        let [a, b, _, c, d, _] = self.0.map(f64::from);
        let determinant = a * d - b * c;
        [d, -c, -b, a].map(|value| (value / determinant) as f32)
    }

    pub(crate) fn hash_state(self, state: &mut impl Hasher) {
        self.0.map(f32::to_bits).hash(state);
    }
}

/// Packed, immutable render lighting in the order supplied by the Sequence.
/// Each light is four vec4s shared by the CPU kernel and the GPU storage ABI.
#[derive(Debug, Clone, PartialEq)]
pub struct GlassSceneLighting {
    lights: Arc<[[f32; 16]]>,
    ambient: [f32; 3],
}

impl GlassSceneLighting {
    pub fn from_samples(samples: &[SceneLightSample]) -> Result<Self, GlassError> {
        let mut lights = Vec::new();
        let mut ambient = [0.0; 3];
        for sample in samples {
            if !sample.color.into_iter().all(f32::is_finite)
                || !sample.intensity_percent.is_finite()
            {
                return Err(GlassError::InvalidLight);
            }
            let power = sample.intensity_percent * 0.01;
            let color = sample.color.map(|value| bounded_light(value * power));
            let pack = match sample.source {
                SceneLightSource::Ambient => {
                    for (value, component) in ambient.iter_mut().zip(color) {
                        *value = bounded_light(*value + component);
                    }
                    continue;
                }
                SceneLightSource::Parallel { position, target, falloff } => {
                    pack_light(color, position, Some(target), None, falloff)?
                }
                SceneLightSource::Point { position, falloff } => {
                    pack_light(color, position, None, None, falloff)?
                }
                SceneLightSource::Spot {
                    position,
                    target,
                    cone_angle_degrees,
                    cone_feather_percent,
                    falloff,
                } => pack_light(
                    color,
                    position,
                    Some(target),
                    Some([cone_angle_degrees, cone_feather_percent]),
                    falloff,
                )?,
            };
            if color != [0.0; 3] {
                lights.push(pack);
            }
        }
        Ok(Self { lights: lights.into(), ambient })
    }

    pub fn is_empty(&self) -> bool {
        self.lights.is_empty() && self.ambient == [0.0; 3]
    }

    pub(crate) fn hash_state(&self, state: &mut impl Hasher) {
        self.ambient.map(f32::to_bits).hash(state);
        self.lights.len().hash(state);
        for light in self.lights.iter() {
            light.map(f32::to_bits).hash(state);
        }
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.lights.len().saturating_mul(std::mem::size_of::<[f32; 16]>())
    }

    /// Number of directional or positional lights, excluding the ambient sum.
    pub fn light_count(&self) -> usize {
        self.lights.len()
    }
}

fn bounded_light(value: f32) -> f32 {
    value.clamp(-MAX_LIGHT_COMPONENT, MAX_LIGHT_COMPONENT)
}

fn pack_light(
    color: [f32; 3],
    position: [f32; 3],
    target: Option<[f32; 3]>,
    cone: Option<[f32; 2]>,
    falloff: SceneLightFalloff,
) -> Result<[f32; 16], GlassError> {
    if !position.into_iter().all(f32::is_finite) {
        return Err(GlassError::InvalidLight);
    }
    let direction = if let Some(target) = target {
        let delta: [f64; 3] =
            std::array::from_fn(|axis| f64::from(target[axis]) - f64::from(position[axis]));
        let length = delta.into_iter().map(|x| x * x).sum::<f64>().sqrt();
        if !length.is_finite() || length == 0.0 {
            return Err(GlassError::InvalidLight);
        }
        delta.map(|x| (x / length) as f32)
    } else {
        [0.0; 3]
    };
    let [inner, outer] = if let Some([angle, feather]) = cone {
        if !(0.0..=180.0).contains(&angle) || !(0.0..=100.0).contains(&feather) {
            return Err(GlassError::InvalidLight);
        }
        let outer = angle.to_radians() * 0.5;
        [(outer * (1.0 - 0.01 * feather)).cos(), outer.cos()]
    } else {
        [0.0; 2]
    };
    let (falloff_kind, radius, range) = match falloff {
        SceneLightFalloff::None => (0.0, 0.0, 0.0),
        SceneLightFalloff::Smooth { radius, distance } => (1.0, radius, distance),
        SceneLightFalloff::InverseSquareClamped { radius } => (2.0, radius, 0.0),
    };
    if ![radius, range].into_iter().all(|value| value.is_finite() && value >= 0.0) {
        return Err(GlassError::InvalidLight);
    }
    let kind = if cone.is_some() {
        2.0
    } else {
        f32::from(target.is_none())
    };
    Ok([
        color[0],
        color[1],
        color[2],
        kind,
        position[0],
        position[1],
        position[2],
        falloff_kind,
        direction[0],
        direction[1],
        direction[2],
        radius,
        range,
        inner,
        outer,
        0.0,
    ])
}

/// Extent-bound height layout. Each reduction averages up to four adjacent
/// samples. The final radius is bounded while the authored footprint scales.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlassHeightPlan {
    pub factor: u32,
    pub extent: EffectFrameExtent,
    pub radius: [f32; 2],
}

impl GlassHeightPlan {
    /// Maximum number of scalar GPU planes, including the two blur outputs.
    pub const MAX_PLANES: u32 = 3 + MAX_HEIGHT_REDUCTION.ilog2();

    pub fn new(grade: GlassGrade, extent: EffectFrameExtent, sampling: ImageSampling) -> Self {
        let [x, y] = sampling.render_scale().map(|value| value.to_f64() as f32);
        let par = sampling.sample_aspect_ratio().to_f64() as f32;
        let radii = [
            (grade.softness * x / par).min(extent.width() as f32),
            (grade.softness * y).min(extent.height() as f32),
        ];
        let mut factor = 1;
        while radii[0].max(radii[1]) / factor as f32 > MAX_BLUR_RADIUS
            && factor < MAX_HEIGHT_REDUCTION
        {
            factor *= 2;
        }
        let extent = EffectFrameExtent::new(
            extent.width().div_ceil(factor),
            extent.height().div_ceil(factor),
        );
        let radius = radii.map(|value| (value / factor as f32).min(MAX_BLUR_RADIUS));
        Self { factor, extent, radius }
    }

    pub fn scratch_bytes(extent: EffectFrameExtent) -> usize {
        // Full height plus two scalar planes covers all pyramid and blur peaks.
        (extent.width() as usize)
            .saturating_mul(extent.height() as usize)
            .saturating_mul(12)
    }
}

/// Runtime values shared with the dedicated GPU draw. Four vec4 headers plus
/// two affine rows, the inverse-transposed linear part, and ambient light.
#[derive(Debug, Clone)]
pub struct GlassGpuControls {
    height: GlassHeightPlan,
    words: [[f32; 4]; 8],
    lights: Arc<[[f32; 16]]>,
}

impl GlassGpuControls {
    pub fn height_plan(&self) -> GlassHeightPlan {
        self.height
    }

    pub fn uniform_words(&self) -> &[[f32; 4]; 8] {
        &self.words
    }

    pub fn packed_lights(&self) -> &[[f32; 16]] {
        &self.lights
    }
}

impl GlassGrade {
    pub fn gpu_controls(
        self,
        sampling: ImageSampling,
        primary: EffectFrameExtent,
        stage: EffectRasterBounds,
        scene: Option<&GlassSceneLighting>,
        projection: Option<GlassProjection>,
    ) -> Result<GlassGpuControls, GlassError> {
        if primary.is_empty() || stage.extent().is_empty() {
            return Err(GlassError::Extent);
        }
        let [scale_x, scale_y] = sampling.render_scale().map(|value| value.to_f64() as f32);
        let par = sampling.sample_aspect_ratio().to_f64() as f32;
        let steps = [par / scale_x, 1.0 / scale_y];
        let material = self.material;
        let (ambient, lights, affine) = if self.use_composition_lights {
            let scene = scene.ok_or(GlassError::MissingSceneLights)?;
            let projection = if scene.lights.is_empty() {
                GlassProjection::from_physical(
                    stage.project_affine([steps[0], 0.0, 0.0, 0.0, steps[1], 0.0]),
                )?
            } else {
                let projection = projection.ok_or(GlassError::MissingProjection)?;
                GlassProjection::from_physical(stage.project_affine(projection.0))?
            };
            (scene.ambient, Arc::clone(&scene.lights), projection)
        } else {
            let (position, target) = match self.effect_light {
                GlassEffectLight::Distant { direction_degrees, height } => {
                    let (sin, cos) = direction_degrees.rem_euclid(360.0).to_radians().sin_cos();
                    ([cos, sin, height * 0.01], Some([0.0; 3]))
                }
                GlassEffectLight::Point { position, height } => (
                    [
                        bounded_light(position[0] * primary.width() as f32 * steps[0]),
                        bounded_light(position[1] * primary.height() as f32 * steps[1]),
                        height,
                    ],
                    None,
                ),
            };
            let color = self.color.map(|value| bounded_light(value * (self.intensity * 0.01)));
            let light = pack_light(color, position, target, None, SceneLightFalloff::None)?;
            let projection = GlassProjection::from_physical(
                stage.project_affine([steps[0], 0.0, 0.0, 0.0, steps[1], 0.0]),
            )?;
            ([1.0; 3], Arc::from([light]), projection)
        };
        let [a, b, tx, c, d, ty] = affine.0;
        let [ox, oy] = stage.origin_half_pixels().map(|value| value as f32 * 0.5);
        let height = GlassHeightPlan::new(self, stage.extent(), sampling);
        let words = [
            [
                self.channel as u32 as f32,
                self.height,
                self.displacement,
                height.factor as f32,
            ],
            [
                material.ambient * 0.01,
                material.diffuse * 0.01,
                material.specular * 0.01,
                material.metal * 0.01,
            ],
            [material.roughness, steps[0], steps[1], 0.0],
            [a, b, tx, 0.0],
            [c, d, ty, 0.0],
            affine.normal_gradient(),
            [ambient[0], ambient[1], ambient[2], 0.0],
            [ox, oy, primary.width() as f32, primary.height() as f32],
        ];
        Ok(GlassGpuControls { height, words, lights })
    }
}

/// Execute a complete Glass stage with the graph's actual input geometry.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_glass_node(
    primary: EffectFloatFrame,
    grade: GlassGrade,
    lighting: Option<&GlassSceneLighting>,
    projection: Option<GlassProjection>,
    original: EffectFrameExtent,
    auxiliary: Option<(AuxiliaryFloatFrame<'_>, AuxiliarySampling)>,
    node_id: crate::EffectGraphNodeId,
    control: &mut EffectProcessorControl<'_>,
) -> Result<EffectFloatFrame, EffectFloatExecutionError> {
    let bounds = EffectRasterBounds::centered_on(original, primary.extent())
        .ok_or(EffectFloatFrameError::ExtentOverflow)?;
    let controls = grade
        .gpu_controls(primary.sampling(), original, bounds, lighting, projection)
        .map_err(|source| EffectFloatExecutionError::GlassFailed { node_id, source })?;
    let primary = crate::materialize_finite_coverage_controlled(
        primary,
        crate::EffectAlphaConsumer::StraightCoverageOperation,
        &mut || {
            control
                .checkpoint()
                .map_err(|reason| EffectFloatExecutionError::Stopped { reason })
        },
    )?;
    apply_glass_controlled(primary, auxiliary, &controls, control)
}

fn apply_glass_controlled(
    mut primary: EffectFloatFrame,
    auxiliary: Option<(AuxiliaryFloatFrame<'_>, AuxiliarySampling)>,
    controls: &GlassGpuControls,
    control: &mut EffectProcessorControl<'_>,
) -> Result<EffectFloatFrame, EffectFloatExecutionError> {
    let mut checkpoint = || {
        control
            .checkpoint()
            .map_err(|reason| EffectFloatExecutionError::Stopped { reason })
    };
    checkpoint()?;
    if primary.is_empty() {
        return Ok(primary);
    }
    let extent = primary.extent();
    let width = extent.width() as usize;
    let height = extent.height() as usize;
    let [ox, oy, original_width, original_height] = controls.words[7];
    let [channel, depth, displacement, _] = controls.words[0];
    let (_, _, channel) = GlassChannel::OPTIONS[channel as usize];
    let [roughness, step_x, step_y, _] = controls.words[2];
    let distance = displacement * (depth * 0.01);
    let [na, nb, nc, nd] = controls.words[5];
    let [a, b, tx, _] = controls.words[3];
    let [c, d, ty, _] = controls.words[4];
    let mut bump = reserve::<f32>(primary.len())?;
    for (index, pixel) in primary.pixels().iter().copied().enumerate() {
        if index.is_multiple_of(4096) {
            checkpoint()?;
        }
        let sample = if let Some((map, sampling)) = auxiliary {
            let uv = [
                (index.rem_euclid(width) as f32 + PIXEL_CENTRE + ox) / original_width,
                ((index / width) as f32 + PIXEL_CENTRE + oy) / original_height,
            ];
            sample_auxiliary_associated_f32(map, uv, sampling)
        } else {
            [
                pixel[0] * pixel[3],
                pixel[1] * pixel[3],
                pixel[2] * pixel[3],
                pixel[3],
            ]
        };
        bump.push(channel.height(sample));
    }
    let plan = controls.height;
    let mut dimensions = [width, height];
    let mut factor = 1;
    while factor < plan.factor {
        let [w, h] = dimensions;
        let [next_w, next_h] = [w.div_ceil(2), h.div_ceil(2)];
        let mut next = reserve::<f32>(next_w * next_h)?;
        for y in 0..next_h {
            checkpoint()?;
            for x in 0..next_w {
                let p = [x * 2, y * 2];
                let end = [(p[0] + 1).min(w - 1), (p[1] + 1).min(h - 1)];
                next.push(
                    ((bump[p[1] * w + p[0]] + bump[p[1] * w + end[0]])
                        + (bump[end[1] * w + p[0]] + bump[end[1] * w + end[0]]))
                        * 0.25,
                );
            }
        }
        bump = next;
        dimensions = [next_w, next_h];
        factor *= 2;
    }
    for (axis, radius) in plan.radius.into_iter().enumerate() {
        if radius > 0.0 {
            bump = blur_height(&bump, dimensions, axis, radius, &mut checkpoint)?;
        }
    }

    let mut output = reserve::<[f32; 4]>(primary.len())?;
    for y in 0..height {
        checkpoint()?;
        for x in 0..width {
            if x != 0 && x.is_multiple_of(4096) {
                checkpoint()?;
            }
            let x = x as f32;
            let y = y as f32;
            let p = [
                (x + PIXEL_CENTRE) / factor as f32 - PIXEL_CENTRE,
                (y + PIXEL_CENTRE) / factor as f32 - PIXEL_CENTRE,
            ];
            let gx = (height_sample(&bump, dimensions, [p[0] + 1.0, p[1]])
                - height_sample(&bump, dimensions, [p[0] - 1.0, p[1]]))
                * (0.5 / factor as f32);
            let gy = (height_sample(&bump, dimensions, [p[0], p[1] + 1.0])
                - height_sample(&bump, dimensions, [p[0], p[1] - 1.0]))
                * (0.5 / factor as f32);
            let uv = [
                x + distance * gx / (step_x * step_x).max(f32::MIN_POSITIVE),
                y + distance * gy / (step_y * step_y).max(f32::MIN_POSITIVE),
            ];
            let sample = sample_picture(primary.pixels(), [width, height], uv);
            if sample[3] == 0.0 {
                output.push([0.0; 4]);
                continue;
            }
            let normal = unit([
                -(na * gx + nb * gy) * depth,
                -(nc * gx + nd * gy) * depth,
                1.0,
            ])
            .0;
            let [px, py] = [x + PIXEL_CENTRE, y + PIXEL_CENTRE];
            let position = [a * px + b * py + tx, c * px + d * py + ty, 0.0];
            output.push(shade(sample, normal, position, roughness, controls));
        }
    }
    checkpoint()?;
    *primary.allocation_mut() = output;
    primary.set_format_after_write(
        primary.format().with_alpha(mondrian_core::ColorFrameAlpha::StraightCoverage),
    );
    primary.view().validate_controlled(&mut checkpoint)?;
    Ok(primary)
}

fn reserve<T>(length: usize) -> Result<Vec<T>, EffectFloatFrameError> {
    let required_bytes = length
        .checked_mul(std::mem::size_of::<T>())
        .ok_or(EffectFloatFrameError::ExtentOverflow)?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| EffectFloatFrameError::AllocationUnavailable { required_bytes })?;
    Ok(values)
}

fn blur_height(
    plane: &[f32],
    [width, height]: [usize; 2],
    axis: usize,
    radius: f32,
    checkpoint: &mut impl FnMut() -> Result<(), EffectFloatExecutionError>,
) -> Result<Vec<f32>, EffectFloatExecutionError> {
    let mut output = reserve::<f32>(plane.len())?;
    let bound = radius.ceil() as i32;
    let size = [width as i32, height as i32];
    for y in 0..height {
        checkpoint()?;
        for x in 0..width {
            let center = [x as i32, y as i32];
            let mut sum = 0.0;
            let mut total = 0.0;
            for offset in -bound..=bound {
                let weight = (radius + 1.0 - offset.abs() as f32).clamp(0.0, radius + 1.0);
                let mut tap = center;
                tap[axis] = (tap[axis] + offset).clamp(0, size[axis] - 1);
                sum += plane[tap[1] as usize * width + tap[0] as usize] * weight;
                total += weight;
            }
            output.push(sum / total);
        }
    }
    Ok(output)
}

fn taps(size: [usize; 2], position: [f32; 2]) -> impl Iterator<Item = (usize, f32)> {
    let p = std::array::from_fn::<_, 2, _>(|i| position[i].clamp(0.0, (size[i] - 1) as f32));
    let base = p.map(f32::floor);
    let [x, y] = [p[0] - base[0], p[1] - base[1]];
    BILINEAR_TAPS
        .into_iter()
        .zip([(1.0 - x) * (1.0 - y), x * (1.0 - y), (1.0 - x) * y, x * y])
        .map(move |([dx, dy], weight)| {
            let ix = (base[0] as usize + dx as usize).min(size[0] - 1);
            let iy = (base[1] as usize + dy as usize).min(size[1] - 1);
            (iy * size[0] + ix, weight)
        })
}

fn height_sample(plane: &[f32], size: [usize; 2], position: [f32; 2]) -> f32 {
    taps(size, position).fold(0.0, |total, (i, w)| total + plane[i] * w)
}

fn sample_picture(pixels: &[[f32; 4]], size: [usize; 2], position: [f32; 2]) -> [f32; 4] {
    let mut rgb = [0.0; 3];
    let mut alpha = 0.0;
    for (index, weight) in taps(size, position) {
        let pixel = pixels[index];
        let weight = weight * pixel[3];
        alpha += weight;
        for (component, value) in rgb.iter_mut().zip(pixel) {
            *component += weight * value;
        }
    }
    if alpha > 0.0 {
        [
            (rgb[0] / alpha).clamp(-f32::MAX, f32::MAX),
            (rgb[1] / alpha).clamp(-f32::MAX, f32::MAX),
            (rgb[2] / alpha).clamp(-f32::MAX, f32::MAX),
            alpha.min(1.0),
        ]
    } else {
        [0.0; 4]
    }
}

fn unit(vector: [f32; 3]) -> ([f32; 3], f32) {
    let scale = vector[0].abs().max(vector[1].abs()).max(vector[2].abs());
    if !scale.is_finite() || scale == 0.0 {
        return ([0.0; 3], 0.0);
    }
    let reduced = vector.map(|value| value / scale);
    let length = dot(reduced, reduced).sqrt();
    (reduced.map(|value| value / length), length * scale)
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    (a[0] * b[0] + a[1] * b[1]) + a[2] * b[2]
}

fn shade(
    sample: [f32; 4],
    normal: [f32; 3],
    position: [f32; 3],
    roughness: f32,
    controls: &GlassGpuControls,
) -> [f32; 4] {
    let [ambient, diffuse, specular, metal] = controls.words[1];
    let exponent = (2.0 / roughness - 2.0).clamp(2.0, 256.0);
    let base = [sample[0], sample[1], sample[2]];
    let mut diffuse_sum = std::array::from_fn::<_, 3, _>(|i| ambient * controls.words[6][i]);
    let mut specular_sum = [0.0; 3];
    for light in controls.lights.iter() {
        let (direction, distance) = if light[3] == 0.0 && light[7] == 0.0 {
            ([0.0; 3], 0.0)
        } else {
            unit(std::array::from_fn(|i| light[i + 4] - position[i]))
        };
        let to_light = if light[3] == 0.0 {
            [-light[8], -light[9], -light[10]]
        } else {
            direction
        };
        let outside = (distance - light[11]).max(0.0);
        let mut falloff = if light[7] == 1.0 && outside > 0.0 {
            if light[12] == 0.0 {
                0.0
            } else {
                (1.0 - outside / light[12]).max(0.0)
            }
        } else if light[7] == 2.0 && outside > 0.0 {
            let ratio = light[11] / distance;
            ratio * ratio
        } else {
            1.0
        };
        if light[3] == 2.0 {
            let angle = dot(to_light.map(|x| -x), [light[8], light[9], light[10]]);
            let cone = if light[13] > light[14] {
                ((angle - light[14]) / (light[13] - light[14])).clamp(0.0, 1.0)
            } else {
                f32::from(angle >= light[14])
            };
            falloff *= cone;
        }
        let facing = dot(normal, to_light).max(0.0);
        if facing == 0.0 || falloff == 0.0 {
            continue;
        }
        let half = unit([to_light[0], to_light[1], to_light[2] + 1.0]).0;
        let glint = dot(normal, half).clamp(0.0, 1.0).powf(exponent);
        for i in 0..3 {
            let light_color = light[i] * falloff;
            diffuse_sum[i] = bounded_light(diffuse_sum[i] + light_color * (facing * diffuse));
            specular_sum[i] = bounded_light(specular_sum[i] + light_color * (glint * specular));
        }
    }
    let rgb = std::array::from_fn::<_, 3, _>(|i| {
        let tint = (1.0 - metal) + base[i].clamp(0.0, 1.0) * metal;
        let scale = base[i].abs().max(1.0);
        let value = (base[i] / scale) * diffuse_sum[i] + tint * (specular_sum[i] / scale);
        (value.clamp(-f32::MAX / scale, f32::MAX / scale) * scale).clamp(-f32::MAX, f32::MAX)
    });
    [rgb[0], rgb[1], rgb[2], sample[3]]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        compile_clip_effect_graph, compile_reference_render_graph, instantiate_effect_node,
        lower_effect_graph_to_gpu_pipeline, AuxiliaryFloatInput, AuxiliaryPolicy,
        AuxiliaryResolution, AuxiliaryRole, EffectColorDomain, EffectExecutionSession,
        EffectFloatFrameView, EffectGpuSpatialPass, EffectGpuStage, EffectGraphBuilderState,
        EffectRenderOp, EffectType, EffectValueFormat, EffectWorkingPrecision,
    };
    use mondrian_core::automation::PropertyValue;
    use mondrian_core::{
        ColorFrameAlpha, Rational, SampleAspectRatio, TimelineTime, WorkingColorSpace,
    };

    const FORMAT: EffectValueFormat = EffectValueFormat::new(
        EffectWorkingPrecision::Float32,
        EffectColorDomain::SceneLinearRgb,
        ColorFrameAlpha::StraightCoverage,
    );

    #[test]
    fn glass_projection_is_invariant_to_output_density_and_retains_reflection() {
        let aspect = SampleAspectRatio::new(2, 1).unwrap();
        let full = ImageSampling::new(aspect, [Rational::new(1, 1); 2]).unwrap();
        let proxy = ImageSampling::new(aspect, [Rational::new(1, 2), Rational::new(1, 4)]).unwrap();
        let a = GlassProjection::new([-2.0, 0.0, 16.0, 0.0, 3.0, 4.0], full).unwrap();
        let b = GlassProjection::new([-1.0, 0.0, 8.0, 0.0, 0.75, 1.0], proxy).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.normal_gradient(), [-0.25, 0.0, 0.0, 1.0 / 3.0]);
        assert!(GlassProjection::new([1.0, 2.0, 0.0, 2.0, 4.0, 0.0], full).is_err());
    }

    #[test]
    fn glass_bump_channels_use_color_and_alpha_independently() {
        let sample = [0.3, 0.1, 0.0, 0.5];
        let values = GlassChannel::OPTIONS.map(|(_, _, channel)| channel.height(sample));
        for (actual, expected) in
            values.into_iter().zip([0.3, 0.1, 0.0, 0.5, 0.3 * 0.2126 + 0.1 * 0.7152, 0.15])
        {
            assert!((actual - expected).abs() < 1e-6);
        }
        assert!(GlassChannel::OPTIONS
            .into_iter()
            .all(|(_, _, channel)| { channel.height([2.0, 5.0, -10.0, 0.0]) == 0.0 }));
    }

    #[test]
    fn glass_default_source_and_external_actual_grid_lower_to_the_same_operation() {
        let mut effect = instantiate_effect_node(EffectType::Glass).unwrap();
        let evaluate = |effect: &mondrian_core::effect_data::EffectNode| {
            compile_clip_effect_graph(
                std::slice::from_ref(effect),
                &[],
                TimelineTime::ZERO,
                WorkingColorSpace::LinearRec709,
            )
            .unwrap()
        };
        let self_graph = evaluate(&effect);
        let source = EffectType::Glass.parameter_id("bump_map").unwrap();
        effect
            .set_static_value_by_parameter(
                &source,
                PropertyValue::VisualSource(mondrian_core::VisualSourceReference::Asset {
                    asset_id: mondrian_core::AssetId::new(),
                    retime: mondrian_core::VisualSourceRetime::default(),
                }),
            )
            .unwrap();
        let graph = evaluate(&effect);
        let slot = graph.auxiliary_inputs()[0].slot();
        assert_eq!(graph.auxiliary_inputs()[0].role(), AuxiliaryRole::Map);
        for (graph, expected) in [(&self_graph, None), (&graph, Some(Some(slot)))] {
            let lowered = lower_effect_graph_to_gpu_pipeline(graph).unwrap();
            let [EffectGpuStage::Spatial { pass, .. }] = lowered.stages() else {
                panic!("expected Glass draw")
            };
            assert!(matches!(pass, EffectGpuSpatialPass::Glass { .. }));
            assert_eq!(pass.auxiliary_slot(), expected);
        }
        let extent = EffectFrameExtent::new(11, 5);
        let pixels = (0..55)
            .map(|index| {
                [
                    2.0 * (index % 11) as f32 / 10.0,
                    -0.3,
                    0.25,
                    (index % 7) as f32 / 6.0,
                ]
            })
            .collect::<Vec<_>>();
        let bump = [
            [0.0, 0.0, 0.0, 1.0],
            [1.0, 1.0, 1.0, 0.5],
            [0.2, 0.3, 0.4, 1.0],
        ];
        let input = EffectFloatFrameView::new(extent, FORMAT, ImageSampling::UNIT_SQUARE, &pixels);
        let map = EffectFloatFrameView::new(
            EffectFrameExtent::new(3, 1),
            FORMAT,
            ImageSampling::UNIT_SQUARE,
            &bump,
        );
        let mut session = EffectExecutionSession::default();
        let mut control = EffectProcessorControl::uncancelled();
        let base = session.apply_compiled_rgba_f32(input, &self_graph, 0, &mut control).unwrap();
        let mapped = session
            .apply_compiled_rgba_f32_with_auxiliary(
                input,
                &graph,
                0,
                &[AuxiliaryFloatInput {
                    slot,
                    raster: AuxiliaryFloatFrame::new(
                        map,
                        AuxiliaryPolicy::new(AuxiliaryResolution::Authored),
                    ),
                }],
                &mut control,
            )
            .unwrap();
        assert_eq!(mapped.extent(), extent);
        assert_ne!(base.pixels(), mapped.pixels());
        assert!(mapped.pixels().iter().any(|pixel| pixel[3] > 0.0));
    }

    #[test]
    fn glass_composition_binding_changes_pixel_identity_and_distinguishes_empty_from_missing() {
        let grade = GlassGrade {
            use_composition_lights: true,
            ..GlassGrade::default()
        };
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::Glass { grade, lighting: None, projection: None });
        let graph = compile_reference_render_graph(builder.finish()).unwrap();
        let dark = Arc::new(GlassSceneLighting::from_samples(&[]).unwrap());
        let lit =
            Arc::new(GlassSceneLighting::from_samples(&[SceneLightSample::default()]).unwrap());
        let dark_graph = graph.bind_glass_lighting(Some(&dark), None);
        let lit_graph = graph.bind_glass_lighting(Some(&lit), None);
        assert_ne!(
            dark_graph.semantic_fingerprint(),
            lit_graph.semantic_fingerprint()
        );
        let pixel = [[0.25, 2.0, -0.5, 1.0]];
        let input = EffectFloatFrameView::new(
            EffectFrameExtent::new(1, 1),
            FORMAT,
            ImageSampling::UNIT_SQUARE,
            &pixel,
        );
        let mut session = EffectExecutionSession::default();
        let mut control = EffectProcessorControl::uncancelled();
        assert!(matches!(
            session.apply_compiled_rgba_f32(input, &graph, 0, &mut control),
            Err(EffectFloatExecutionError::GlassFailed {
                source: GlassError::MissingSceneLights,
                ..
            })
        ));
        for (graph, expected) in [
            (dark_graph, [0.0, 0.0, 0.0, 1.0]),
            (lit_graph, [0.125, 1.0, -0.25, 1.0]),
        ] {
            let actual = session.apply_compiled_rgba_f32(input, &graph, 0, &mut control).unwrap();
            assert_eq!(actual.pixels(), &[expected]);
        }
    }
}
