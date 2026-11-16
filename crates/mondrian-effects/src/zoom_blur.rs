//! Zoom blur (effect list BLR-05).
//!
//! Stretches each pixel along its ray away from an authored centre, so the
//! middle of the frame stays readable while the edges streak outwards. The
//! nine authoring controls follow the Resolve FX zoom blur parameter set; the
//! remaining group toggles are UI collapsers that the effect accepts without
//! changing the result.
//!
//! Conventions:
//! - `zoom` is the fraction of the pixel-to-centre distance the sweep may
//!   travel, so `0` is the identity and `1` reaches the centre. Every tap stays
//!   on the pixel's own ray, so no colour crosses the centre.
//! - `realistic` spaces the taps by `t * t` and damps them with
//!   `1 - smoothness * t`, which keeps the samples dense near the source and
//!   thins them towards the centre the way a real zoom streak does. `linear`
//!   spaces and weights the taps evenly.
//! - `centre_exclusion` keeps a disc around the centre sharp; its radius is a
//!   fraction of half the frame diagonal.
//! - Colour is filtered through the premultiplied frame and un-premultiplied
//!   per tap, which matches the GPU pass. `border_type` decides what an
//!   out-of-frame tap reads: `replicate` clamps, `mirror` reflects, `black` and
//!   `white` return a constant.
//! - `quality` picks 24 or 8 taps; both paths use the same count, so the CPU
//!   reference and the shader agree tap for tap.

/// Default smoothing of the tap weights.
pub const ZOOM_BLUR_DEFAULT_SMOOTHNESS: f32 = 0.4;

/// Default fraction of the pixel-to-centre distance the sweep travels.
pub const ZOOM_BLUR_DEFAULT_ZOOM: f32 = 0.4;

/// Default sharp radius around the centre, as a fraction of half the diagonal.
pub const ZOOM_BLUR_DEFAULT_CENTRE_EXCLUSION: f32 = 0.0;

/// Default per-channel blur strength.
pub const ZOOM_BLUR_DEFAULT_CHANNEL_GAIN: f32 = 1.0;

/// Default centre in normalised frame coordinates.
pub const ZOOM_BLUR_DEFAULT_CENTRE: [f32; 2] = [0.5, 0.5];

/// Largest smoothing value the schema accepts.
pub const ZOOM_BLUR_MAX_SMOOTHNESS: f32 = 1.0;

/// Largest zoom the schema accepts.
pub const ZOOM_BLUR_MAX_ZOOM: f32 = 1.0;

/// Largest centre exclusion the schema accepts.
pub const ZOOM_BLUR_MAX_CENTRE_EXCLUSION: f32 = 1.0;

/// Largest per-channel blur strength the schema accepts.
pub const ZOOM_BLUR_MAX_CHANNEL_GAIN: f32 = 1.0;

/// Taps used by the `better` quality menu.
pub const ZOOM_BLUR_BETTER_TAPS: u32 = 24;

/// Taps used by the `faster` quality menu.
pub const ZOOM_BLUR_FASTER_TAPS: u32 = 8;

/// A value below this reads as "no zoom".
const ZOOM_EPSILON: f32 = 1.0e-6;

/// Smallest authored centre coordinate, so the centre may sit outside the frame.
pub const ZOOM_BLUR_MIN_CENTRE: f32 = -1.0;

/// Largest authored centre coordinate, so the centre may sit outside the frame.
pub const ZOOM_BLUR_MAX_CENTRE: f32 = 2.0;

/// Strength of the realistic tap distribution before normalisation.
/// Blur distribution along the ray.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoomBlurType {
    /// Taps pile towards the far end and fade with `smoothness`.
    Realistic,
    /// Taps sit at even distances with equal weight.
    Linear,
}

impl ZoomBlurType {
    /// Every menu entry, in schema order.
    pub const ALL: [Self; 2] = [Self::Realistic, Self::Linear];

    /// Default menu entry.
    pub const DEFAULT: Self = Self::Realistic;

    /// Stable key used by the authoring schema and by cached identities.
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Realistic => "realistic",
            Self::Linear => "linear",
        }
    }

    /// Menu caption shown next to the key.
    pub fn label(self) -> &'static str {
        match self {
            Self::Realistic => "真实",
            Self::Linear => "线性",
        }
    }

    /// Maps an authored enumeration key onto a mode.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_key() == key)
    }

    /// Stable tag used by the op hash and by the GPU plan.
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Realistic => 1,
            Self::Linear => 2,
        }
    }
}

/// Sample budget used by one blurred pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoomBlurQuality {
    /// Twenty-four taps.
    Better,
    /// Eight taps.
    Faster,
}

impl ZoomBlurQuality {
    /// Every menu entry, in schema order.
    pub const ALL: [Self; 2] = [Self::Better, Self::Faster];

    /// Default menu entry.
    pub const DEFAULT: Self = Self::Better;

    /// Stable key used by the authoring schema and by cached identities.
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Better => "better",
            Self::Faster => "faster",
        }
    }

    /// Menu caption shown next to the key.
    pub fn label(self) -> &'static str {
        match self {
            Self::Better => "更好",
            Self::Faster => "更快",
        }
    }

    /// Maps an authored enumeration key onto a quality.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|quality| quality.as_key() == key)
    }

    /// Stable tag used by the op hash and by the GPU plan.
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Better => 1,
            Self::Faster => 2,
        }
    }

    /// Number of taps this quality uses.
    pub fn taps(self) -> u32 {
        match self {
            Self::Better => ZOOM_BLUR_BETTER_TAPS,
            Self::Faster => ZOOM_BLUR_FASTER_TAPS,
        }
    }
}

/// What a tap that leaves the frame reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoomBlurBorder {
    /// Repeat the nearest edge pixel.
    Replicate,
    /// Reflect at the edge.
    Mirror,
    /// Read transparent black.
    Black,
    /// Read opaque white.
    White,
}

impl ZoomBlurBorder {
    /// Every menu entry, in schema order.
    pub const ALL: [Self; 4] = [Self::Replicate, Self::Mirror, Self::Black, Self::White];

    /// Default menu entry.
    pub const DEFAULT: Self = Self::Replicate;

    /// Stable key used by the authoring schema and by cached identities.
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Replicate => "replicate",
            Self::Mirror => "mirror",
            Self::Black => "black",
            Self::White => "white",
        }
    }

    /// Menu caption shown next to the key.
    pub fn label(self) -> &'static str {
        match self {
            Self::Replicate => "复制边缘",
            Self::Mirror => "镜像",
            Self::Black => "黑色",
            Self::White => "白色",
        }
    }

    /// Maps an authored enumeration key onto a border rule.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|border| border.as_key() == key)
    }

    /// Stable tag used by the op hash and by the GPU plan.
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Replicate => 1,
            Self::Mirror => 2,
            Self::Black => 3,
            Self::White => 4,
        }
    }
}

/// Authored zoom blur parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoomBlurParameters {
    /// Damping applied to the far taps.
    pub smoothness: f32,
    /// Fraction of the ray the sweep travels.
    pub zoom: f32,
    /// Blur distribution along the ray.
    pub blur_type: ZoomBlurType,
    /// Sharp disc around the centre, as a fraction of half the diagonal.
    pub centre_exclusion: f32,
    /// UI collapser for the per-channel strengths.
    pub channel_adjust: bool,
    /// Red channel blur strength.
    pub relative_red: f32,
    /// Green channel blur strength.
    pub relative_green: f32,
    /// Blue channel blur strength.
    pub relative_blue: f32,
    /// Alpha blur strength, used while `blur_alpha` is on.
    pub relative_alpha: f32,
    /// When set, the legacy `x_position`/`y_position` pair drives the centre.
    pub centre_group: bool,
    /// Legacy normalised X centre.
    pub x_position: f32,
    /// Legacy normalised Y centre.
    pub y_position: f32,
    /// Current normalised centre.
    pub position: [f32; 2],
    /// UI collapser for the advanced controls.
    pub advanced_controls: bool,
    /// Sample budget.
    pub quality: ZoomBlurQuality,
    /// Out-of-frame tap rule.
    pub border_type: ZoomBlurBorder,
    /// UI collapser; the centre is always normalised, so this never moves it.
    pub follow_sizing: bool,
    /// Whether the alpha channel is blurred too.
    pub blur_alpha: bool,
}

impl Default for ZoomBlurParameters {
    fn default() -> Self {
        Self {
            smoothness: ZOOM_BLUR_DEFAULT_SMOOTHNESS,
            zoom: ZOOM_BLUR_DEFAULT_ZOOM,
            blur_type: ZoomBlurType::DEFAULT,
            centre_exclusion: ZOOM_BLUR_DEFAULT_CENTRE_EXCLUSION,
            channel_adjust: false,
            relative_red: ZOOM_BLUR_DEFAULT_CHANNEL_GAIN,
            relative_green: ZOOM_BLUR_DEFAULT_CHANNEL_GAIN,
            relative_blue: ZOOM_BLUR_DEFAULT_CHANNEL_GAIN,
            relative_alpha: ZOOM_BLUR_DEFAULT_CHANNEL_GAIN,
            centre_group: false,
            x_position: ZOOM_BLUR_DEFAULT_CENTRE[0],
            y_position: ZOOM_BLUR_DEFAULT_CENTRE[1],
            position: ZOOM_BLUR_DEFAULT_CENTRE,
            advanced_controls: false,
            quality: ZoomBlurQuality::DEFAULT,
            border_type: ZoomBlurBorder::DEFAULT,
            follow_sizing: true,
            blur_alpha: true,
        }
    }
}

/// Reasons a zoom blur cannot be built from its authored values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("zoom_blur.{parameter} must be finite and within its documented range")]
pub struct ZoomBlurError {
    /// Name of the parameter that could not be accepted.
    pub parameter: &'static str,
}

impl ZoomBlurError {
    /// Name of the offending parameter.
    pub fn parameter(&self) -> &'static str {
        self.parameter
    }
}

/// Uniform cells the GPU pass reads.
pub type ZoomBlurGpuControls = ([f32; 4], [f32; 4], [f32; 4], [f32; 4]);

/// Validated zoom blur parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoomBlurGrade {
    parameters: ZoomBlurParameters,
}

impl ZoomBlurGrade {
    /// Builds a grade, rejecting values the authoring schema cannot produce.
    pub fn new(parameters: ZoomBlurParameters) -> Result<Self, ZoomBlurError> {
        fn check(
            value: f32,
            lower: f32,
            upper: f32,
            parameter: &'static str,
        ) -> Result<(), ZoomBlurError> {
            if value.is_finite() && (lower..=upper).contains(&value) {
                Ok(())
            } else {
                Err(ZoomBlurError { parameter })
            }
        }

        check(
            parameters.smoothness,
            0.0,
            ZOOM_BLUR_MAX_SMOOTHNESS,
            "smoothness",
        )?;
        check(parameters.zoom, 0.0, ZOOM_BLUR_MAX_ZOOM, "zoom")?;
        check(
            parameters.centre_exclusion,
            0.0,
            ZOOM_BLUR_MAX_CENTRE_EXCLUSION,
            "centre_exclusion",
        )?;
        check(
            parameters.relative_red,
            0.0,
            ZOOM_BLUR_MAX_CHANNEL_GAIN,
            "relative_red",
        )?;
        check(
            parameters.relative_green,
            0.0,
            ZOOM_BLUR_MAX_CHANNEL_GAIN,
            "relative_green",
        )?;
        check(
            parameters.relative_blue,
            0.0,
            ZOOM_BLUR_MAX_CHANNEL_GAIN,
            "relative_blue",
        )?;
        check(
            parameters.relative_alpha,
            0.0,
            ZOOM_BLUR_MAX_CHANNEL_GAIN,
            "relative_alpha",
        )?;
        check(
            parameters.x_position,
            ZOOM_BLUR_MIN_CENTRE,
            ZOOM_BLUR_MAX_CENTRE,
            "x_position",
        )?;
        check(
            parameters.y_position,
            ZOOM_BLUR_MIN_CENTRE,
            ZOOM_BLUR_MAX_CENTRE,
            "y_position",
        )?;
        check(
            parameters.position[0],
            ZOOM_BLUR_MIN_CENTRE,
            ZOOM_BLUR_MAX_CENTRE,
            "position",
        )?;
        check(
            parameters.position[1],
            ZOOM_BLUR_MIN_CENTRE,
            ZOOM_BLUR_MAX_CENTRE,
            "position",
        )?;
        Ok(Self { parameters })
    }

    /// The documented defaults, already validated.
    pub fn documented_defaults() -> Self {
        Self { parameters: ZoomBlurParameters::default() }
    }

    /// Authored parameters.
    pub fn parameters(self) -> ZoomBlurParameters {
        self.parameters
    }

    /// Normalised centre this grade reads.
    pub fn centre(self) -> [f32; 2] {
        if self.parameters.centre_group {
            [self.parameters.x_position, self.parameters.y_position]
        } else {
            self.parameters.position
        }
    }

    /// Whether the grade leaves the frame untouched.
    pub fn is_identity(self) -> bool {
        if self.parameters.zoom <= ZOOM_EPSILON {
            return true;
        }
        let colours_off = self.parameters.relative_red <= ZOOM_EPSILON
            && self.parameters.relative_green <= ZOOM_EPSILON
            && self.parameters.relative_blue <= ZOOM_EPSILON;
        let alpha_off =
            !self.parameters.blur_alpha || self.parameters.relative_alpha <= ZOOM_EPSILON;
        colours_off && alpha_off
    }

    /// Estimated relative cost, on the same scale as the other effects.
    pub fn cost(self) -> u8 {
        3
    }

    /// Taps one blurred pixel reads.
    pub fn taps(self) -> u32 {
        if self.is_identity() {
            return 1;
        }
        self.parameters.quality.taps()
    }

    /// Reach of the pass, in pixels. The sweep grows with the distance to the
    /// centre, so the furthest tap can land anywhere in the frame.
    pub fn input_halo(self) -> (u32, u32) {
        (u32::MAX, u32::MAX)
    }

    /// Packs the values the GPU pass needs.
    ///
    /// `params` carries the smoothing, the zoom and the centre exclusion;
    /// `extra` carries the centre and the blur type and quality tags; `aces`
    /// carries the per-channel strengths; `extra1` carries the border tag and
    /// the alpha toggle. The recorder prepends the frame size.
    pub fn gpu_controls(self) -> ZoomBlurGpuControls {
        let centre = self.centre();
        let parameters = self.parameters;
        (
            [
                parameters.smoothness,
                parameters.zoom,
                parameters.centre_exclusion,
                0.0,
            ],
            [
                centre[0],
                centre[1],
                parameters.blur_type.as_u8() as f32,
                parameters.quality.as_u8() as f32,
            ],
            [
                parameters.relative_red,
                parameters.relative_green,
                parameters.relative_blue,
                parameters.relative_alpha,
            ],
            [
                parameters.border_type.as_u8() as f32,
                f32::from(u8::from(parameters.blur_alpha)),
                0.0,
                0.0,
            ],
        )
    }

    /// Folds every authored value into an operation hash.
    pub fn hash_state<H: std::hash::Hasher>(self, state: &mut H) {
        use std::hash::Hash;
        let parameters = self.parameters;
        parameters.smoothness.to_bits().hash(state);
        parameters.zoom.to_bits().hash(state);
        parameters.blur_type.as_u8().hash(state);
        parameters.centre_exclusion.to_bits().hash(state);
        u8::from(parameters.channel_adjust).hash(state);
        parameters.relative_red.to_bits().hash(state);
        parameters.relative_green.to_bits().hash(state);
        parameters.relative_blue.to_bits().hash(state);
        parameters.relative_alpha.to_bits().hash(state);
        u8::from(parameters.centre_group).hash(state);
        parameters.x_position.to_bits().hash(state);
        parameters.y_position.to_bits().hash(state);
        parameters.position[0].to_bits().hash(state);
        parameters.position[1].to_bits().hash(state);
        u8::from(parameters.advanced_controls).hash(state);
        parameters.quality.as_u8().hash(state);
        parameters.border_type.as_u8().hash(state);
        u8::from(parameters.follow_sizing).hash(state);
        u8::from(parameters.blur_alpha).hash(state);
    }
}

/// Position of tap `index` along the ray, as a fraction of the full zoom.
fn tap_shape(index: u32, taps: u32, blur_type: ZoomBlurType) -> f32 {
    if taps <= 1 {
        return 0.0;
    }
    let t = index as f32 / (taps - 1) as f32;
    match blur_type {
        ZoomBlurType::Linear => t,
        ZoomBlurType::Realistic => t * t,
    }
}

/// Weight of tap `index` in the average.
fn tap_weight(index: u32, taps: u32, blur_type: ZoomBlurType, smoothness: f32) -> f32 {
    match blur_type {
        ZoomBlurType::Linear => 1.0,
        ZoomBlurType::Realistic => {
            if taps <= 1 {
                1.0
            } else {
                1.0 - smoothness * (index as f32 / (taps - 1) as f32)
            }
        }
    }
}

/// Blurs one frame in place, returning `false` when the grade is unusable.
pub fn zoom_blur_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: ZoomBlurGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_zoom_blur_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Blurs one frame in place with an explicit cancellation checkpoint.
///
/// Returns `Ok(false)` when the frame cannot be executed so the caller can
/// report an explicit unsupported-op error.
pub(crate) fn apply_zoom_blur_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: ZoomBlurGrade,
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
    let source = frame.to_vec();
    let mut premultiplied = source.clone();
    crate::straight_alpha::premultiply_rgba_controlled(&mut premultiplied, checkpoint)?;

    let parameters = grade.parameters();
    let taps = grade.taps();
    let centre = grade.centre();
    // Taps live in pixel-index space, where integer coordinates address pixel
    // centres; the authored centre is a fraction of the frame, so it shifts by
    // half a pixel to name the same point.
    let centre = [
        centre[0] * width as f32 - 0.5,
        centre[1] * height as f32 - 0.5,
    ];
    let diagonal = (width as f32 * width as f32 + height as f32 * height as f32).sqrt();
    let exclusion = parameters.centre_exclusion * 0.5 * diagonal;
    let zoom = parameters.zoom;
    let smoothness = parameters.smoothness;
    let blur_type = parameters.blur_type;
    let border = parameters.border_type;
    let gains = [
        parameters.relative_red,
        parameters.relative_green,
        parameters.relative_blue,
    ];
    let alpha_gain = if parameters.blur_alpha {
        parameters.relative_alpha
    } else {
        0.0
    };
    let keep_alpha = !parameters.blur_alpha;

    let mut weights = [0.0_f32; ZOOM_BLUR_BETTER_TAPS as usize];
    let mut weight_sum = 0.0_f32;
    for (index, weight) in weights.iter_mut().take(taps as usize).enumerate() {
        *weight = tap_weight(index as u32, taps, blur_type, smoothness);
        weight_sum += *weight;
    }
    let inverse_weight = if weight_sum > 0.0 {
        1.0 / weight_sum
    } else {
        0.0
    };
    checkpoint()?;

    let rows = pixel_count / width;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + offset;
            let pixel_y = row as f32;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let pixel_x = column as f32;
                let dx = pixel_x - centre[0];
                let dy = pixel_y - centre[1];
                let distance = (dx * dx + dy * dy).sqrt();
                if distance <= exclusion {
                    continue;
                }
                let cell = row * width + column;
                let straight_source = source[cell];
                let mut blurred = [0.0_f32; 4];
                for (index, &weight) in weights.iter().take(taps as usize).enumerate() {
                    if weight <= 0.0 {
                        continue;
                    }
                    let scale = 1.0 - zoom * tap_shape(index as u32, taps, blur_type);
                    let sample_x = centre[0] + dx * scale;
                    let sample_y = centre[1] + dy * scale;
                    let sample =
                        sample_straight(&premultiplied, width, height, sample_x, sample_y, border);
                    for channel in 0..4 {
                        blurred[channel] += sample[channel] * weight;
                    }
                }
                for value in &mut blurred {
                    *value *= inverse_weight;
                }
                let mut out = straight_source;
                for (channel, gain) in gains.iter().enumerate() {
                    out[channel] = straight_source[channel]
                        + (blurred[channel] - straight_source[channel]) * gain;
                }
                out[3] = if keep_alpha {
                    straight_source[3]
                } else {
                    straight_source[3] + (blurred[3] - straight_source[3]) * alpha_gain
                };
                *pixel = out;
            }
        }
    });

    checkpoint()?;
    Ok(true)
}

/// Bilinear tap in straight alpha, honouring the authored border rule.
///
/// The frame is filtered through its premultiplied copy so a tap never pulls
/// colour out of a transparent neighbour.
fn sample_straight(
    premultiplied: &[[f32; 4]],
    width: usize,
    height: usize,
    x: f32,
    y: f32,
    border: ZoomBlurBorder,
) -> [f32; 4] {
    let max_x = (width - 1) as f32;
    let max_y = (height - 1) as f32;
    let (x, y) = match border {
        ZoomBlurBorder::Replicate => (x.clamp(0.0, max_x), y.clamp(0.0, max_y)),
        ZoomBlurBorder::Mirror => (mirror_coordinate(x, max_x), mirror_coordinate(y, max_y)),
        ZoomBlurBorder::Black | ZoomBlurBorder::White => {
            if x < -0.5 || x > max_x + 0.5 || y < -0.5 || y > max_y + 0.5 {
                return if matches!(border, ZoomBlurBorder::Black) {
                    [0.0, 0.0, 0.0, 0.0]
                } else {
                    [1.0, 1.0, 1.0, 1.0]
                };
            }
            (x.clamp(0.0, max_x), y.clamp(0.0, max_y))
        }
    };

    let x0 = x.floor();
    let y0 = y.floor();
    let frac_x = x - x0;
    let frac_y = y - y0;
    let x0 = (x0 as usize).min(width - 1);
    let y0 = (y0 as usize).min(height - 1);
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let top_left = premultiplied[y0 * width + x0];
    let top_right = premultiplied[y0 * width + x1];
    let bottom_left = premultiplied[y1 * width + x0];
    let bottom_right = premultiplied[y1 * width + x1];
    let mut sample = [0.0_f32; 4];
    for (channel, value) in sample.iter_mut().enumerate() {
        let top = top_left[channel] + (top_right[channel] - top_left[channel]) * frac_x;
        let bottom = bottom_left[channel] + (bottom_right[channel] - bottom_left[channel]) * frac_x;
        *value = top + (bottom - top) * frac_y;
    }
    crate::coverage::straight_rgba_from_premultiplied(sample)
}

/// Reflects a coordinate back into `0..=max` with edge duplication.
fn mirror_coordinate(value: f32, max: f32) -> f32 {
    if max <= 0.0 {
        return 0.0;
    }
    let period = 2.0 * max;
    let mut folded = value.abs() % period;
    if folded > max {
        folded = period - folded;
    }
    folded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(parameters: ZoomBlurParameters) -> ZoomBlurGrade {
        ZoomBlurGrade::new(parameters).expect("the parameters must be accepted")
    }

    fn rejected(parameters: ZoomBlurParameters) -> ZoomBlurError {
        ZoomBlurGrade::new(parameters).expect_err("the parameters must be rejected")
    }

    fn frame(width: usize, height: usize, background: [f32; 4]) -> Vec<[f32; 4]> {
        vec![background; width * height]
    }

    fn ramp(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = Vec::with_capacity(width * height);
        for row in 0..height {
            for column in 0..width {
                let value = (column + row * width) as f32 / (width * height) as f32;
                pixels.push([value, value, value, 1.0]);
            }
        }
        pixels
    }

    #[test]
    fn defaults_match_the_documented_values() {
        let parameters = ZoomBlurParameters::default();
        assert_eq!(parameters.smoothness, 0.4);
        assert_eq!(parameters.zoom, 0.4);
        assert_eq!(parameters.blur_type, ZoomBlurType::Realistic);
        assert_eq!(parameters.centre_exclusion, 0.0);
        assert_eq!(parameters.relative_red, 1.0);
        assert_eq!(parameters.relative_alpha, 1.0);
        assert_eq!(parameters.position, [0.5, 0.5]);
        assert_eq!(parameters.quality, ZoomBlurQuality::Better);
        assert_eq!(parameters.border_type, ZoomBlurBorder::Replicate);
        assert!(parameters.blur_alpha);
        assert_eq!(valid(parameters), ZoomBlurGrade::documented_defaults());
    }

    #[test]
    fn new_rejects_values_the_schema_cannot_author() {
        let cases = [
            (
                ZoomBlurParameters { smoothness: -0.1, ..Default::default() },
                "smoothness",
            ),
            (
                ZoomBlurParameters { zoom: 1.5, ..Default::default() },
                "zoom",
            ),
            (
                ZoomBlurParameters { centre_exclusion: -0.01, ..Default::default() },
                "centre_exclusion",
            ),
            (
                ZoomBlurParameters { relative_red: 1.2, ..Default::default() },
                "relative_red",
            ),
            (
                ZoomBlurParameters { relative_alpha: f32::NAN, ..Default::default() },
                "relative_alpha",
            ),
            (
                ZoomBlurParameters { position: [2.5, 0.5], ..Default::default() },
                "position",
            ),
            (
                ZoomBlurParameters { x_position: -1.5, ..Default::default() },
                "x_position",
            ),
        ];
        for (parameters, expected) in cases {
            assert_eq!(rejected(parameters).parameter(), expected);
        }
    }

    #[test]
    fn menu_keys_round_trip_for_every_entry() {
        for blur_type in ZoomBlurType::ALL {
            assert_eq!(ZoomBlurType::from_key(blur_type.as_key()), Some(blur_type));
        }
        for quality in ZoomBlurQuality::ALL {
            assert_eq!(ZoomBlurQuality::from_key(quality.as_key()), Some(quality));
        }
        for border in ZoomBlurBorder::ALL {
            assert_eq!(ZoomBlurBorder::from_key(border.as_key()), Some(border));
        }
        assert_eq!(ZoomBlurType::from_key("nearest"), None);
        assert_eq!(ZoomBlurQuality::from_key(""), None);
        assert_eq!(ZoomBlurBorder::from_key("clamp"), None);
    }

    #[test]
    fn menu_tags_are_stable() {
        assert_eq!(ZoomBlurType::Realistic.as_u8(), 1);
        assert_eq!(ZoomBlurType::Linear.as_u8(), 2);
        assert_eq!(ZoomBlurQuality::Better.as_u8(), 1);
        assert_eq!(ZoomBlurQuality::Faster.as_u8(), 2);
        assert_eq!(ZoomBlurBorder::Replicate.as_u8(), 1);
        assert_eq!(ZoomBlurBorder::Mirror.as_u8(), 2);
        assert_eq!(ZoomBlurBorder::Black.as_u8(), 3);
        assert_eq!(ZoomBlurBorder::White.as_u8(), 4);
    }

    #[test]
    fn quality_selects_the_documented_tap_counts() {
        let better = valid(ZoomBlurParameters::default());
        assert_eq!(better.taps(), 24);
        let faster = valid(ZoomBlurParameters {
            quality: ZoomBlurQuality::Faster,
            ..Default::default()
        });
        assert_eq!(faster.taps(), 8);
    }

    #[test]
    fn zero_zoom_is_the_identity() {
        let mut pixels = ramp(7, 5);
        let before = pixels.clone();
        let grade = valid(ZoomBlurParameters { zoom: 0.0, ..Default::default() });
        assert!(grade.is_identity());
        assert_eq!(grade.taps(), 1);
        assert!(zoom_blur_frame_f32(&mut pixels, 7, 5, grade));
        assert_eq!(pixels, before);
    }

    #[test]
    fn every_channel_disabled_with_alpha_off_is_the_identity() {
        let grade = valid(ZoomBlurParameters {
            relative_red: 0.0,
            relative_green: 0.0,
            relative_blue: 0.0,
            blur_alpha: false,
            ..Default::default()
        });
        assert!(grade.is_identity());
        let alpha_still_on = valid(ZoomBlurParameters {
            relative_red: 0.0,
            relative_green: 0.0,
            relative_blue: 0.0,
            blur_alpha: true,
            relative_alpha: 1.0,
            ..Default::default()
        });
        assert!(!alpha_still_on.is_identity());
    }

    #[test]
    fn exclusion_keeps_the_centre_disc_sharp() {
        let mut pixels = ramp(9, 9);
        let before = pixels.clone();
        let grade = valid(ZoomBlurParameters { centre_exclusion: 1.0, ..Default::default() });
        assert!(zoom_blur_frame_f32(&mut pixels, 9, 9, grade));
        assert_eq!(pixels, before);
    }

    #[test]
    fn linear_zoom_averages_evenly_along_the_ray() {
        // A one-pixel-high frame keeps the ray horizontal: the centre sits at
        // x = 0 and the probe at x = 8, so taps land at 8, 6, 4, 2 when zoom is
        // 0.5 and the linear mode spaces them evenly.
        let width = 9;
        let mut pixels = frame(width, 1, [0.0, 0.0, 0.0, 1.0]);
        for (column, pixel) in pixels.iter_mut().enumerate() {
            let value = column as f32 / 8.0;
            *pixel = [value, value, value, 1.0];
        }
        let grade = valid(ZoomBlurParameters {
            zoom: 0.5,
            blur_type: ZoomBlurType::Linear,
            quality: ZoomBlurQuality::Faster,
            position: [0.0, 0.5],
            ..Default::default()
        });
        assert!(zoom_blur_frame_f32(&mut pixels, width, 1, grade));
        // The taps run from x = 8 down to x = 3.75 and average 5.875, so the ramp
        // reads 5.875 / 8 = 0.734375.
        assert!((pixels[8][0] - 0.734375).abs() < 1e-5, "{:?}", pixels[8]);
        assert!((pixels[8][1] - 0.734375).abs() < 1e-5);
    }

    #[test]
    fn realistic_mode_differs_from_the_flat_mean() {
        let width = 9;
        let mut linear_pixels = frame(width, 1, [0.0, 0.0, 0.0, 1.0]);
        let mut realistic_pixels = linear_pixels.clone();
        for (column, pixel) in linear_pixels.iter_mut().enumerate() {
            let value = column as f32 / 8.0;
            *pixel = [value, value, value, 1.0];
        }
        realistic_pixels.copy_from_slice(&linear_pixels);
        let common = ZoomBlurParameters {
            zoom: 0.5,
            quality: ZoomBlurQuality::Faster,
            position: [0.0, 0.5],
            ..Default::default()
        };
        assert!(zoom_blur_frame_f32(
            &mut linear_pixels,
            width,
            1,
            valid(ZoomBlurParameters { blur_type: ZoomBlurType::Linear, ..common })
        ));
        assert!(zoom_blur_frame_f32(
            &mut realistic_pixels,
            width,
            1,
            valid(ZoomBlurParameters { smoothness: 0.4, ..common })
        ));
        // The realistic curve pushes the taps towards the centre and damps
        // them, so it cannot coincide with the flat mean.
        assert!((realistic_pixels[8][0] - linear_pixels[8][0]).abs() > 1e-3);
        assert!((0.0..=1.0).contains(&realistic_pixels[8][0]));
    }

    #[test]
    fn smoothness_pulls_the_result_towards_the_source() {
        let width = 9;
        let mut smooth = ramp(width, 1);
        let mut flat = smooth.clone();
        let common = ZoomBlurParameters {
            zoom: 0.5,
            quality: ZoomBlurQuality::Faster,
            position: [0.0, 0.5],
            ..Default::default()
        };
        assert!(zoom_blur_frame_f32(
            &mut smooth,
            width,
            1,
            valid(ZoomBlurParameters { smoothness: 1.0, ..common })
        ));
        assert!(zoom_blur_frame_f32(
            &mut flat,
            width,
            1,
            valid(ZoomBlurParameters { smoothness: 0.0, ..common })
        ));
        // The ramp source at the probe is 1, so the damped weights must land the
        // streak closer to it than the undamped ones.
        assert!(smooth[8][0] > flat[8][0]);
        assert!(smooth[8][0] <= 1.0);
    }

    #[test]
    fn a_channel_gain_of_zero_keeps_that_channel() {
        let mut pixels = ramp(9, 1);
        let before = pixels.clone();
        let grade = valid(ZoomBlurParameters {
            zoom: 0.5,
            relative_red: 0.0,
            quality: ZoomBlurQuality::Faster,
            position: [0.0, 0.5],
            ..Default::default()
        });
        assert!(zoom_blur_frame_f32(&mut pixels, 9, 1, grade));
        assert!((pixels[8][0] - before[8][0]).abs() < 1e-6);
        assert!((pixels[8][1] - before[8][1]).abs() > 1e-3);
    }

    #[test]
    fn alpha_keeps_its_source_value_when_the_toggle_is_off() {
        let mut pixels: Vec<[f32; 4]> =
            (0..9).map(|column| [0.5, 0.5, 0.5, column as f32 / 8.0]).collect();
        let before = pixels.clone();
        let grade = valid(ZoomBlurParameters {
            zoom: 0.5,
            blur_alpha: false,
            quality: ZoomBlurQuality::Faster,
            position: [0.0, 0.5],
            ..Default::default()
        });
        assert!(zoom_blur_frame_f32(&mut pixels, 9, 1, grade));
        for (after, original) in pixels.iter().zip(before.iter()) {
            assert_eq!(after[3], original[3]);
        }
    }

    #[test]
    fn alpha_gain_controls_how_much_the_alpha_blurs() {
        let mut full =
            (0..9).map(|column| [0.0, 0.0, 0.0, column as f32 / 8.0]).collect::<Vec<_>>();
        let mut none = full.clone();
        let common = ZoomBlurParameters {
            zoom: 0.5,
            quality: ZoomBlurQuality::Faster,
            position: [0.0, 0.5],
            ..Default::default()
        };
        assert!(zoom_blur_frame_f32(
            &mut full,
            9,
            1,
            valid(ZoomBlurParameters { relative_alpha: 1.0, ..common })
        ));
        assert!(zoom_blur_frame_f32(
            &mut none,
            9,
            1,
            valid(ZoomBlurParameters { relative_alpha: 0.0, ..common })
        ));
        // The alpha ramp is smooth, so the streak keeps most of the probe's
        // alpha while still blending in its lower-alpha neighbours.
        assert!((0.7..1.0).contains(&full[8][3]), "{:?}", full[8]);
        assert!(full[8][3] < 0.95);
        assert!((none[8][3] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn border_rules_decide_what_a_tap_outside_the_frame_reads() {
        // The centre sits past the right edge, so the outer taps of the probe
        // pixel leave the frame. Every pixel is mid grey, which makes the four
        // rules distinguishable: replicate and mirror keep the grey, black pulls
        // the average down, white pulls it up.
        let width = 4;
        let common = ZoomBlurParameters {
            zoom: 1.0,
            quality: ZoomBlurQuality::Faster,
            position: [2.0, 0.5],
            ..Default::default()
        };
        let run = |border| {
            let mut pixels = frame(width, 1, [0.5, 0.5, 0.5, 1.0]);
            assert!(zoom_blur_frame_f32(
                &mut pixels,
                width,
                1,
                valid(ZoomBlurParameters { border_type: border, ..common })
            ));
            pixels[0][0]
        };
        let replicate = run(ZoomBlurBorder::Replicate);
        let mirror = run(ZoomBlurBorder::Mirror);
        let black = run(ZoomBlurBorder::Black);
        let white = run(ZoomBlurBorder::White);
        assert!((replicate - 0.5).abs() < 1e-5, "{replicate}");
        assert!((mirror - 0.5).abs() < 1e-5, "{mirror}");
        assert!(black < 0.5, "{black}");
        assert!(white > 0.5, "{white}");
    }

    #[test]
    fn mirror_reflects_a_coordinate_back_into_the_frame() {
        assert_eq!(mirror_coordinate(10.0, 8.0), 6.0);
        assert_eq!(mirror_coordinate(-2.0, 8.0), 2.0);
        assert_eq!(mirror_coordinate(8.0, 8.0), 8.0);
        assert_eq!(mirror_coordinate(0.0, 0.0), 0.0);
        assert_eq!(mirror_coordinate(6.0, 8.0), 6.0);
    }

    #[test]
    fn the_centre_group_switches_between_the_point_and_the_xy_pair() {
        let point = valid(ZoomBlurParameters {
            position: [0.25, 0.75],
            x_position: 0.5,
            y_position: 0.5,
            ..Default::default()
        });
        assert_eq!(point.centre(), [0.25, 0.75]);
        let legacy = valid(ZoomBlurParameters {
            centre_group: true,
            position: [0.25, 0.75],
            x_position: 0.1,
            y_position: 0.9,
            ..Default::default()
        });
        assert_eq!(legacy.centre(), [0.1, 0.9]);
        assert_eq!(legacy.gpu_controls().1[0], 0.1);
        assert_eq!(legacy.gpu_controls().1[1], 0.9);
    }

    #[test]
    fn gpu_controls_pack_the_authored_values() {
        let grade = valid(ZoomBlurParameters {
            smoothness: 0.25,
            zoom: 0.75,
            centre_exclusion: 0.5,
            blur_type: ZoomBlurType::Linear,
            quality: ZoomBlurQuality::Faster,
            border_type: ZoomBlurBorder::Mirror,
            relative_red: 0.1,
            relative_green: 0.2,
            relative_blue: 0.3,
            relative_alpha: 0.4,
            blur_alpha: true,
            position: [0.3, 0.6],
            ..Default::default()
        });
        let (params, extra, aces, extra1) = grade.gpu_controls();
        assert_eq!(params, [0.25, 0.75, 0.5, 0.0]);
        assert_eq!(extra, [0.3, 0.6, 2.0, 2.0]);
        assert_eq!(aces, [0.1, 0.2, 0.3, 0.4]);
        assert_eq!(extra1, [2.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn hash_state_changes_with_every_authored_field() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::Hasher;

        fn digest(parameters: ZoomBlurParameters) -> u64 {
            let mut hasher = DefaultHasher::new();
            valid(parameters).hash_state(&mut hasher);
            hasher.finish()
        }

        let baseline = digest(ZoomBlurParameters::default());
        let variants = [
            ZoomBlurParameters { smoothness: 0.2, ..Default::default() },
            ZoomBlurParameters { zoom: 0.8, ..Default::default() },
            ZoomBlurParameters {
                blur_type: ZoomBlurType::Linear,
                ..Default::default()
            },
            ZoomBlurParameters { centre_exclusion: 0.3, ..Default::default() },
            ZoomBlurParameters { channel_adjust: true, ..Default::default() },
            ZoomBlurParameters { relative_red: 0.5, ..Default::default() },
            ZoomBlurParameters { relative_green: 0.5, ..Default::default() },
            ZoomBlurParameters { relative_blue: 0.5, ..Default::default() },
            ZoomBlurParameters { relative_alpha: 0.5, ..Default::default() },
            ZoomBlurParameters { centre_group: true, ..Default::default() },
            ZoomBlurParameters { x_position: 0.25, ..Default::default() },
            ZoomBlurParameters { y_position: 0.25, ..Default::default() },
            ZoomBlurParameters { position: [0.25, 0.5], ..Default::default() },
            ZoomBlurParameters { advanced_controls: true, ..Default::default() },
            ZoomBlurParameters {
                quality: ZoomBlurQuality::Faster,
                ..Default::default()
            },
            ZoomBlurParameters {
                border_type: ZoomBlurBorder::White,
                ..Default::default()
            },
            ZoomBlurParameters { follow_sizing: false, ..Default::default() },
            ZoomBlurParameters { blur_alpha: false, ..Default::default() },
        ];
        let mut seen = std::collections::HashSet::new();
        seen.insert(baseline);
        for parameters in variants {
            assert!(seen.insert(digest(parameters)));
        }
    }

    #[test]
    fn halo_covers_the_whole_frame() {
        let grade = valid(ZoomBlurParameters::default());
        assert_eq!(grade.input_halo(), (u32::MAX, u32::MAX));
        assert_eq!(grade.cost(), 3);
    }

    #[test]
    fn tiny_and_mismatched_frames_are_rejected_without_panicking() {
        let grade = valid(ZoomBlurParameters::default());
        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!zoom_blur_frame_f32(&mut empty, 0, 0, grade));
        let mut short = frame(2, 2, [0.0, 0.0, 0.0, 1.0]);
        short.truncate(3);
        assert!(!zoom_blur_frame_f32(&mut short, 2, 2, grade));
        let mut single = vec![[0.2, 0.4, 0.6, 1.0]];
        assert!(zoom_blur_frame_f32(&mut single, 1, 1, grade));
        assert_eq!(single[0], [0.2, 0.4, 0.6, 1.0]);
    }

    #[test]
    fn blurred_pixels_stay_transparent_where_the_source_was_empty() {
        let width = 9;
        let mut pixels = frame(width, 1, [0.0, 0.0, 0.0, 0.0]);
        pixels[8] = [1.0, 0.5, 0.25, 1.0];
        let grade = valid(ZoomBlurParameters {
            zoom: 0.5,
            quality: ZoomBlurQuality::Faster,
            position: [0.0, 0.5],
            ..Default::default()
        });
        assert!(zoom_blur_frame_f32(&mut pixels, width, 1, grade));
        assert_eq!(pixels[0][3], 0.0);
        // The sweep smears the single opaque column across its transparent
        // neighbours, so the probe keeps some but not all of its alpha.
        assert!(pixels[8][3] > 0.2 && pixels[8][3] < 0.5, "{:?}", pixels[8]);
        assert!(pixels[8][0] > 0.0);
    }
}
