//! Graph-local displacement map reference, with explicit auxiliary input.
//!
//! The grade describes pixel sampling only. Expanding a layer's output bounds
//! requires the separate ROI-growth facility and is refused at construction.

use crate::auxiliary::sample_auxiliary_color_f32;
use crate::resample::sample_bilinear_wrapped;
use crate::{AuxiliaryFloatFrame, AuxiliarySampling};

/// Maximum absolute authored displacement, in pixels.
pub const DISPLACEMENT_MAP_MAX_PIXELS: f32 = 32_000.0;

/// Channel selectors in the source effect's one-based menu order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum DisplacementMapChannel {
    Red = 1,
    Green,
    Blue,
    Alpha,
    Luminance,
    Hue,
    Lightness,
    Saturation,
    Full,
    Half,
    Off,
}

impl DisplacementMapChannel {
    pub const ALL: [Self; 11] = [
        Self::Red,
        Self::Green,
        Self::Blue,
        Self::Alpha,
        Self::Luminance,
        Self::Hue,
        Self::Lightness,
        Self::Saturation,
        Self::Full,
        Self::Half,
        Self::Off,
    ];
    pub const fn as_menu(self) -> u32 {
        self as u32
    }
    pub fn from_menu(value: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_menu() == value)
    }
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Alpha => "alpha",
            Self::Luminance => "luminance",
            Self::Hue => "hue",
            Self::Lightness => "lightness",
            Self::Saturation => "saturation",
            Self::Full => "full",
            Self::Half => "half",
            Self::Off => "off",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_key() == key)
    }
    pub const fn is_constant(self) -> bool {
        matches!(self, Self::Full | Self::Half | Self::Off)
    }

    fn offset(self, pixel: [f32; 4]) -> f32 {
        let rgb = [pixel[0], pixel[1], pixel[2]].map(|v| v.clamp(0.0, 1.0));
        let value = match self {
            Self::Red => rgb[0],
            Self::Green => rgb[1],
            Self::Blue => rgb[2],
            Self::Alpha => pixel[3],
            Self::Luminance => rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722,
            Self::Hue => crate::shift_channels::hue_sample(rgb),
            Self::Lightness => {
                (rgb[0].max(rgb[1]).max(rgb[2]) + rgb[0].min(rgb[1]).min(rgb[2])) * 0.5
            }
            Self::Saturation => crate::shift_channels::saturation_sample(rgb),
            Self::Full => 1.0,
            Self::Half => 0.5,
            Self::Off => 0.0,
        };
        let coverage = if matches!(self, Self::Alpha | Self::Full | Self::Half | Self::Off) {
            1.0
        } else {
            pixel[3].clamp(0.0, 1.0)
        };
        (2.0 * value.clamp(0.0, 1.0) - 1.0) * coverage
    }
}

/// Layout of an auxiliary map relative to the primary pixel grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum DisplacementMapBehavior {
    Center = 1,
    Stretch,
    Tile,
}

impl DisplacementMapBehavior {
    pub const ALL: [Self; 3] = [Self::Center, Self::Stretch, Self::Tile];
    pub const fn as_menu(self) -> u32 {
        self as u32
    }
    pub fn from_menu(value: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_menu() == value)
    }
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Center => "center",
            Self::Stretch => "stretch",
            Self::Tile => "tile",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_key() == key)
    }
}

/// Rejected displacement authoring state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DisplacementMapError {
    #[error("displacement amounts must be finite")]
    NonFiniteAmount,
    #[error("displacement amounts must be within [-32000, 32000] pixels")]
    AmountOutOfRange,
    #[error("expanded displacement output requires ROI-growth support")]
    RequiresRoiGrowth,
}

/// Validated displacement controls, independent of the auxiliary raster.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplacementMapGrade {
    amounts: [f32; 2],
    channels: [DisplacementMapChannel; 2],
    behavior: DisplacementMapBehavior,
    wrap: bool,
}

impl DisplacementMapGrade {
    pub fn new(
        amounts: [f32; 2],
        channels: [DisplacementMapChannel; 2],
        behavior: DisplacementMapBehavior,
        wrap: bool,
        expand_output: bool,
    ) -> Result<Self, DisplacementMapError> {
        if amounts.iter().any(|v| !v.is_finite()) {
            return Err(DisplacementMapError::NonFiniteAmount);
        }
        if amounts.iter().any(|v| v.abs() > DISPLACEMENT_MAP_MAX_PIXELS) {
            return Err(DisplacementMapError::AmountOutOfRange);
        }
        if expand_output {
            return Err(DisplacementMapError::RequiresRoiGrowth);
        }
        Ok(Self { amounts, channels, behavior, wrap })
    }
    pub const fn amounts(self) -> [f32; 2] {
        self.amounts
    }
    pub const fn channels(self) -> [DisplacementMapChannel; 2] {
        self.channels
    }
    pub const fn behavior(self) -> DisplacementMapBehavior {
        self.behavior
    }
    pub const fn wrap(self) -> bool {
        self.wrap
    }
    pub fn is_identity(self) -> bool {
        self.amounts.iter().all(|v| v.abs() <= 1e-6)
            || self.channels.iter().all(|v| v.is_constant())
    }
    pub fn gpu_controls(self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.amounts[0],
                self.amounts[1],
                self.channels[0].as_menu() as f32,
                self.channels[1].as_menu() as f32,
            ],
            [
                self.behavior.as_menu() as f32,
                u32::from(self.wrap) as f32,
                0.0,
                0.0,
            ],
        )
    }
}

/// Apply a displacement map on a fixed output grid with bounded cancellation.
/// Returns false if either raster's storage/extent is inconsistent.
pub fn apply_displacement_map_controlled<E>(
    primary: &mut [[f32; 4]],
    width: u32,
    height: u32,
    map: AuxiliaryFloatFrame<'_>,
    sampling: AuxiliarySampling,
    grade: &DisplacementMapGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    checkpoint()?;
    let extent = map.frame.extent();
    if width == 0
        || height == 0
        || extent.is_empty()
        || (width as usize).checked_mul(height as usize) != Some(primary.len())
        || (extent.width() as usize).checked_mul(extent.height() as usize)
            != Some(map.frame.pixels().len())
    {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    let mut source = Vec::new();
    if source.try_reserve_exact(primary.len()).is_err() {
        return Ok(false);
    }
    for chunk in primary.chunks(4096) {
        checkpoint()?;
        source.extend_from_slice(chunk);
    }
    crate::straight_alpha::premultiply_rgba_controlled(&mut source, checkpoint)?;
    for (index, pixel) in primary.iter_mut().enumerate() {
        if index % 4096 == 0 {
            checkpoint()?;
        }
        let x = (index % width as usize) as f32;
        let y = (index / width as usize) as f32;
        let uv = match grade.behavior {
            DisplacementMapBehavior::Center => [
                (x + 0.5 + (extent.width() as f32 - width as f32) * 0.5) / extent.width() as f32,
                (y + 0.5 + (extent.height() as f32 - height as f32) * 0.5) / extent.height() as f32,
            ],
            DisplacementMapBehavior::Stretch => {
                [(x + 0.5) / width as f32, (y + 0.5) / height as f32]
            }
            DisplacementMapBehavior::Tile => [
                (x + 0.5) / extent.width() as f32,
                (y + 0.5) / extent.height() as f32,
            ],
        };
        let sampled = sample_auxiliary_color_f32(
            map,
            uv,
            sampling,
            grade.behavior == DisplacementMapBehavior::Tile,
        );
        let sx = x + grade.channels[0].offset(sampled) * grade.amounts[0];
        let sy = y + grade.channels[1].offset(sampled) * grade.amounts[1];
        let sample = if grade.wrap {
            sample_bilinear_wrapped(
                &source,
                width as usize,
                height as usize,
                sx as f64,
                sy as f64,
            )
        } else {
            sample_transparent(&source, width, height, sx, sy)
        };
        *pixel = crate::coverage::straight_rgba_from_premultiplied(sample);
    }
    checkpoint()?;
    Ok(true)
}

/// Infallible convenience entry for the complete-frame reference kernel.
pub fn displacement_map_frame_f32(
    primary: &mut [[f32; 4]],
    width: u32,
    height: u32,
    map: AuxiliaryFloatFrame<'_>,
    sampling: AuxiliarySampling,
    grade: &DisplacementMapGrade,
) -> bool {
    apply_displacement_map_controlled(primary, width, height, map, sampling, grade, &mut || {
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

fn sample_transparent(source: &[[f32; 4]], width: u32, height: u32, x: f32, y: f32) -> [f32; 4] {
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let mut result = [0.0; 4];
    for [dx, dy] in crate::BILINEAR_TAPS {
        let ix = x0 as i64 + dx as i64;
        let iy = y0 as i64 + dy as i64;
        if ix < 0 || iy < 0 || ix >= width as i64 || iy >= height as i64 {
            continue;
        }
        let weight = (if dx == 0 { 1.0 - fx } else { fx }) * (if dy == 0 { 1.0 - fy } else { fy });
        let pixel = source[iy as usize * width as usize + ix as usize];
        for channel in 0..4 {
            result[channel] += pixel[channel] * weight;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AuxiliaryPolicy, AuxiliaryRole, EffectColorDomain, EffectFloatFrameView, EffectFrameExtent,
        EffectValueFormat, EffectWorkingPrecision,
    };
    fn map(pixels: &[[f32; 4]], width: u32) -> AuxiliaryFloatFrame<'_> {
        AuxiliaryFloatFrame::new(
            EffectFloatFrameView::new(
                EffectFrameExtent::new(width, 1),
                EffectValueFormat::new(
                    EffectWorkingPrecision::Float32,
                    EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                mondrian_core::ImageSampling::UNIT_SQUARE,
                pixels,
            ),
            AuxiliaryPolicy::new(crate::AuxiliaryResolution::Authored),
        )
    }
    fn grade(
        channels: [DisplacementMapChannel; 2],
        amounts: [f32; 2],
        wrap: bool,
    ) -> DisplacementMapGrade {
        DisplacementMapGrade::new(
            amounts,
            channels,
            DisplacementMapBehavior::Stretch,
            wrap,
            false,
        )
        .unwrap()
    }
    fn ramp() -> Vec<[f32; 4]> {
        (0..4).map(|x| [x as f32, -(x as f32), 2.0, 1.0]).collect()
    }
    #[test]
    fn selectors_and_behaviors_round_trip_in_the_documented_order() {
        for (index, channel) in DisplacementMapChannel::ALL.into_iter().enumerate() {
            assert_eq!(channel.as_menu(), index as u32 + 1);
            assert_eq!(
                DisplacementMapChannel::from_menu(channel.as_menu()),
                Some(channel)
            );
            assert_eq!(
                DisplacementMapChannel::from_key(channel.as_key()),
                Some(channel)
            );
        }
        for behavior in DisplacementMapBehavior::ALL {
            assert_eq!(
                DisplacementMapBehavior::from_menu(behavior.as_menu()),
                Some(behavior)
            );
            assert_eq!(
                DisplacementMapBehavior::from_key(behavior.as_key()),
                Some(behavior)
            );
        }
        assert!(DisplacementMapChannel::from_menu(0).is_none());
        assert!(DisplacementMapChannel::from_menu(12).is_none());
    }
    #[test]
    fn invalid_amounts_and_expanded_output_fail_closed() {
        let channels = [DisplacementMapChannel::Red; 2];
        for amount in [f32::NAN, f32::INFINITY, 32001.0, -32001.0] {
            assert!(DisplacementMapGrade::new(
                [amount, 0.0],
                channels,
                DisplacementMapBehavior::Center,
                false,
                false
            )
            .is_err());
        }
        assert_eq!(
            DisplacementMapGrade::new(
                [5.0; 2],
                channels,
                DisplacementMapBehavior::Center,
                false,
                true
            ),
            Err(DisplacementMapError::RequiresRoiGrowth)
        );
    }
    #[test]
    fn black_white_and_middle_gray_produce_signed_full_displacement() {
        let original = ramp();
        for (value, expected) in [(0.0, [0.0; 4]), (0.5, original[0]), (1.0, original[1])] {
            let pixels = [[value, 0.5, 0.0, 1.0]];
            let mut output = original.clone();
            assert!(displacement_map_frame_f32(
                &mut output,
                4,
                1,
                map(&pixels, 1),
                AuxiliaryRole::Map.default_sampling(),
                &grade(
                    [DisplacementMapChannel::Red, DisplacementMapChannel::Green],
                    [1.0, 0.0],
                    false
                )
            ));
            assert_eq!(output[0], expected);
        }
    }
    #[test]
    fn off_is_negative_half_is_neutral_and_two_constant_axes_are_identity() {
        assert_eq!(DisplacementMapChannel::Off.offset([0.0; 4]), -1.0);
        assert_eq!(DisplacementMapChannel::Half.offset([0.0; 4]), 0.0);
        assert_eq!(DisplacementMapChannel::Full.offset([0.0; 4]), 1.0);
        assert!(grade(
            [DisplacementMapChannel::Full, DisplacementMapChannel::Off],
            [4.0; 2],
            true
        )
        .is_identity());
    }
    #[test]
    fn picture_channels_use_coverage_but_alpha_is_a_direct_signal() {
        let pixel = [1.0, 0.0, 0.0, 0.25];
        assert_eq!(DisplacementMapChannel::Red.offset(pixel), 0.25);
        assert_eq!(DisplacementMapChannel::Alpha.offset(pixel), -0.5);
        assert_eq!(
            DisplacementMapChannel::Red.offset([10.0, -10.0, 0.0, 0.0]),
            0.0
        );
    }
    #[test]
    fn wrapped_displacement_keeps_hdr_and_negative_color() {
        let mut pixels = ramp();
        let original = pixels.clone();
        let aux = [[1.0, 0.5, 0.0, 1.0]];
        assert!(displacement_map_frame_f32(
            &mut pixels,
            4,
            1,
            map(&aux, 1),
            AuxiliaryRole::Map.default_sampling(),
            &grade(
                [DisplacementMapChannel::Red, DisplacementMapChannel::Green],
                [1.0, 0.0],
                true
            )
        ));
        assert_eq!(pixels, [original[1], original[2], original[3], original[0]]);
    }

    #[test]
    fn primary_filtering_is_premultiplied_and_preserves_transparent_edges() {
        let mut pixels = [[0.0, 0.0, 1000.0, 0.0], [2.0, -1.0, 0.0, 0.5]];
        let aux = [[1.0, 0.5, 0.0, 1.0]];
        assert!(displacement_map_frame_f32(
            &mut pixels,
            2,
            1,
            map(&aux, 1),
            AuxiliaryRole::Map.default_sampling(),
            &grade(
                [DisplacementMapChannel::Red, DisplacementMapChannel::Green],
                [0.5, 0.0],
                false
            )
        ));
        assert_eq!(pixels, [[2.0, -1.0, 0.0, 0.25]; 2]);
    }

    #[test]
    fn hidden_map_color_cannot_pollute_a_neutral_control() {
        let original = ramp();
        let mut output = original.clone();
        let aux = [[0.5, 0.5, 0.5, 1.0], [1000.0, -10.0, 50.0, 0.0]];
        let sampling = AuxiliarySampling {
            filter: crate::AuxiliaryFilter::Linear,
            ..AuxiliaryRole::Map.default_sampling()
        };
        assert!(displacement_map_frame_f32(
            &mut output,
            4,
            1,
            map(&aux, 2),
            sampling,
            &grade(
                [DisplacementMapChannel::Red, DisplacementMapChannel::Green],
                [2.0; 2],
                false
            )
        ));
        assert_eq!(output, original);
    }

    #[test]
    fn different_map_sizes_respect_center_stretch_and_tile_layouts() {
        let original = (0..7).map(|x| [x as f32, 0.0, 2.0, 1.0]).collect::<Vec<_>>();
        let aux = [
            [0.0, 0.5, 0.0, 1.0],
            [0.5, 0.5, 0.0, 1.0],
            [1.0, 0.5, 0.0, 1.0],
        ];
        for (behavior, indices) in [
            (DisplacementMapBehavior::Center, [-1, 0, 1, 3, 5, 6, -1]),
            (DisplacementMapBehavior::Stretch, [-1, 0, 2, 3, 4, 6, -1]),
            (DisplacementMapBehavior::Tile, [-1, 1, 3, 2, 4, 6, 5]),
        ] {
            let mut output = original.clone();
            let grade = DisplacementMapGrade::new(
                [1.0, 0.0],
                [DisplacementMapChannel::Red, DisplacementMapChannel::Green],
                behavior,
                false,
                false,
            )
            .unwrap();
            assert!(displacement_map_frame_f32(
                &mut output,
                7,
                1,
                map(&aux, 3),
                AuxiliaryRole::Map.default_sampling(),
                &grade
            ));
            for (actual, index) in output.iter().zip(indices) {
                assert_eq!(
                    *actual,
                    if index < 0 {
                        [0.0; 4]
                    } else {
                        original[index as usize]
                    },
                    "{behavior:?}"
                );
            }
        }
    }
    #[test]
    fn checkpoints_can_cancel_before_any_pixel_is_written() {
        let mut pixels = ramp();
        let original = pixels.clone();
        let aux = [[1.0; 4]];
        let result = apply_displacement_map_controlled(
            &mut pixels,
            4,
            1,
            map(&aux, 1),
            AuxiliaryRole::Map.default_sampling(),
            &grade([DisplacementMapChannel::Red; 2], [1.0; 2], false),
            &mut || Err("cancel"),
        );
        assert_eq!(result, Err("cancel"));
        assert_eq!(pixels, original);
    }

    fn procedural_grade() -> crate::FractalNoiseGrade {
        crate::FractalNoiseGrade::new(
            crate::FractalType::Basic,
            crate::FractalNoiseType::SoftLinear,
            false,
            100.0,
            0.0,
            crate::FractalOverflow::Clip,
            0.0,
            true,
            100.0,
            100.0,
            100.0,
            [0.0; 2],
            false,
            6,
            50.0,
            50.0,
            0.0,
            [0.0; 2],
            0.0,
            0.0,
            false,
            1,
            0,
            100.0,
            mondrian_core::BlendMode::Normal,
        )
        .unwrap()
    }

    fn procedural_graph() -> (
        std::sync::Arc<crate::CompiledEffectGraph>,
        crate::EffectGraphNodeId,
    ) {
        let mut builder = crate::EffectGraphBuilderState::new();
        let input = builder.source();
        let auxiliary = builder.append_custom_source(crate::EffectRenderOp::FractalNoise {
            grade: procedural_grade(),
        });
        let output = builder.add_auxiliary_effect_with_role(
            input,
            auxiliary,
            crate::EffectRenderOp::DisplacementMap {
                grade: grade(
                    [DisplacementMapChannel::Red, DisplacementMapChannel::Green],
                    [0.25, -0.375],
                    false,
                ),
            },
            AuxiliaryRole::Map,
        );
        builder.set_current_output(output);
        (
            crate::compile_reference_render_graph(builder.finish())
                .expect("procedural displacement DAG"),
            output,
        )
    }

    #[test]
    fn a_displacement_map_with_a_procedural_map_matches_the_cpu_reference() {
        let (width, height) = (16, 10);
        let input = (0..width * height)
            .map(|index| {
                let x = (index % width) as f32;
                let y = (index / width) as f32;
                [x + 2.0 * y, -x + y, 2.0, 1.0]
            })
            .collect::<Vec<_>>();
        let mut noise = vec![[0.0, 0.0, 0.0, 1.0]; input.len()];
        assert!(crate::fractal_noise_frame_f32(
            &mut noise,
            width,
            height,
            &procedural_grade()
        ));
        let (compiled, _) = procedural_graph();
        assert!(compiled.auxiliary_inputs().is_empty());
        assert!(compiled.has_auxiliary_effects());
        let view = EffectFloatFrameView::new(
            EffectFrameExtent::new(width as u32, height as u32),
            EffectValueFormat::new(
                EffectWorkingPrecision::Float32,
                EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightCoverage,
            ),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            &input,
        );
        // The primary, generated map and displacement snapshot coexist.
        let peak = 3 * input.len() * std::mem::size_of::<[f32; 4]>();
        assert_eq!(
            crate::estimate_compiled_float_raster_working_bytes(&compiled, view.extent(), &[])
                .unwrap(),
            peak
        );
        let mut refused = crate::EffectExecutionSession::new(
            crate::EffectExecutionSessionConfig::uncached(peak - 1),
        );
        let mut control = crate::EffectProcessorControl::uncancelled();
        assert!(matches!(
            refused.apply_compiled_rgba_f32(view, &compiled, 0, &mut control),
            Err(crate::EffectFloatExecutionError::AuxiliaryInput(
                crate::EffectExecutionError::AuxiliaryWorkingSetBudgetExceeded {
                    required_bytes,
                    budget_bytes,
                }
            )) if required_bytes == peak && budget_bytes == peak - 1
        ));
        let mut admitted =
            crate::EffectExecutionSession::new(crate::EffectExecutionSessionConfig::uncached(peak));
        let output = admitted.apply_compiled_rgba_f32(view, &compiled, 0, &mut control).unwrap();
        assert!(
            output.pixels() != input,
            "the procedural map must produce a nonidentity result"
        );
        // Bilinear sampling reproduces an affine color field exactly. This
        // reference has no sampler implementation shared with the kernel.
        for y in 1..height - 1 {
            for x in 1..width - 1 {
                let index = y * width + x;
                let signal = 2.0 * noise[index][0] - 1.0;
                let dx = signal * 0.25;
                let dy = signal * (-0.375);
                let expected = [
                    input[index][0] + dx + 2.0 * dy,
                    input[index][1] - dx + dy,
                    2.0,
                    1.0,
                ];
                for (actual, expected) in output[index].iter().zip(expected) {
                    assert!(
                        (actual - expected).abs() < 1e-5,
                        "({x},{y}): {actual} vs {expected}"
                    );
                }
            }
        }
    }

    #[test]
    fn an_auxiliary_effect_refuses_the_fused_gpu_point_path() {
        let (compiled, output) = procedural_graph();
        assert!(matches!(
            crate::lower_effect_graph_node_to_gpu_point_plan(&compiled, output),
            Err(crate::EffectGpuPlanBlocker::NodeExecutionModeNotAdmitted { .. })
        ));
    }

    #[test]
    fn a_displacement_map_without_an_auxiliary_edge_is_not_executable() {
        let op = crate::EffectRenderOp::DisplacementMap {
            grade: grade([DisplacementMapChannel::Red; 2], [1.0; 2], false),
        };
        let compiled =
            crate::compile_reference_effect_graph(&crate::EffectRenderPlan { ops: vec![op] })
                .unwrap();
        assert!(!crate::compiled_effect_graph_supports_rgba_f32(&compiled));
    }
}
