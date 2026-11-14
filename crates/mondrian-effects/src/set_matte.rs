//! Set Matte reads one channel from the current input or an authored matte.
//! It writes straight coverage and preserves the primary RGB.

use crate::auxiliary::{sample_auxiliary_associated_f32, AuxiliaryFloatFrame, PIXEL_CENTRE};
use crate::coverage::straight_rgba_from_premultiplied;
use crate::{AuxiliaryCoordinateMapping, AuxiliaryFilter, AuxiliaryOutOfBounds, AuxiliarySampling};

/// Set Matte's source channels in host menu order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SetMatteChannel {
    Red,
    Green,
    Blue,
    Alpha,
    Luminance,
    Hue,
    Lightness,
    Saturation,
    FullOn,
    FullOff,
}

impl SetMatteChannel {
    /// Stable author keys and their corresponding runtime channels.
    pub const OPTIONS: [(&'static str, &'static str, Self); 10] = [
        ("red", "红色", Self::Red),
        ("green", "绿色", Self::Green),
        ("blue", "蓝色", Self::Blue),
        ("alpha", "Alpha", Self::Alpha),
        ("luminance", "亮度", Self::Luminance),
        ("hue", "色相", Self::Hue),
        ("lightness", "明度", Self::Lightness),
        ("saturation", "饱和度", Self::Saturation),
        ("full_on", "全开", Self::FullOn),
        ("full_off", "全关", Self::FullOff),
    ];

    /// Resolve a persisted menu key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::OPTIONS
            .iter()
            .find(|(candidate, _, _)| *candidate == key)
            .map(|entry| entry.2)
    }

    fn coverage(self, sample: [f32; 4]) -> f32 {
        match self {
            Self::Red => sample[0],
            Self::Green => sample[1],
            Self::Blue => sample[2],
            Self::Alpha => sample[3],
            Self::Luminance => 0.299 * sample[0] + 0.587 * sample[1] + 0.114 * sample[2],
            Self::Hue | Self::Lightness | Self::Saturation => {
                let rgb = [sample[0], sample[1], sample[2]];
                let (high, low) = crate::hsl_space::extremes(rgb);
                let chroma = high - low;
                let sum = high + low;
                match self {
                    Self::Hue if chroma.is_infinite() => {
                        let scale = high.abs().max(low.abs());
                        let high = high / scale;
                        let low = low / scale;
                        crate::hsl_space::hue_turns(
                            rgb.map(|channel| channel / scale),
                            high,
                            high - low,
                        )
                    }
                    Self::Hue if chroma > 0.0 => crate::hsl_space::hue_turns(rgb, high, chroma),
                    Self::Hue => 0.0,
                    Self::Lightness => sum * 0.5,
                    Self::Saturation if sum > 0.0 && sum < 2.0 => {
                        chroma * (1.0 - (1.0 - sum).abs())
                    }
                    Self::Saturation => 0.0,
                    _ => unreachable!("only HSL components enter this branch"),
                }
            }
            Self::FullOn => 1.0,
            Self::FullOff => 0.0,
        }
        .clamp(0.0, 1.0)
    }
}

/// Validated Set Matte parameters. All fields are discrete and have no invalid combinations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SetMatteGrade {
    pub channel: SetMatteChannel,
    pub invert: bool,
    pub stretch: bool,
    pub combine: bool,
    pub premultiply: bool,
}

impl Default for SetMatteGrade {
    fn default() -> Self {
        Self {
            channel: SetMatteChannel::Alpha,
            invert: false,
            stretch: true,
            combine: true,
            premultiply: true,
        }
    }
}

impl SetMatteGrade {
    /// Layout for the authored auxiliary source. The centered path uses exact integer texels.
    pub const fn sampling(self) -> AuxiliarySampling {
        AuxiliarySampling {
            mapping: AuxiliaryCoordinateMapping::PrimaryGrid,
            filter: if self.stretch {
                AuxiliaryFilter::Linear
            } else {
                AuxiliaryFilter::Nearest
            },
            out_of_bounds: AuxiliaryOutOfBounds::Transparent,
        }
    }

    /// Parameters and layout flags consumed by the dedicated spatial pass.
    pub const fn gpu_controls(self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.channel as u8 as f32,
                self.invert as u8 as f32,
                self.combine as u8 as f32,
                0.0,
            ],
            [
                self.stretch as u8 as f32,
                self.premultiply as u8 as f32,
                0.0,
                0.0,
            ],
        )
    }

    fn apply(self, mut primary: [f32; 4], sample: [f32; 4]) -> [f32; 4] {
        let mut coverage = self.channel.coverage(sample);
        if self.invert {
            coverage = 1.0 - coverage;
        }
        if self.combine {
            coverage *= primary[3].clamp(0.0, 1.0);
        }
        primary[3] = coverage;
        primary
    }

    fn associate(self, mut sample: [f32; 4]) -> [f32; 4] {
        if self.premultiply {
            let alpha = sample[3].clamp(0.0, 1.0);
            for channel in &mut sample[..3] {
                *channel *= alpha;
            }
        }
        sample
    }
}

/// Set coverage on a straight-alpha frame, using its own pixels when the matte is absent.
pub fn set_matte_frame_f32(
    primary: &mut [[f32; 4]],
    width: u32,
    height: u32,
    matte: Option<AuxiliaryFloatFrame<'_>>,
    grade: SetMatteGrade,
) -> bool {
    let Some(len) = (width as usize).checked_mul(height as usize) else {
        return false;
    };
    let Some(primary) = primary.get_mut(..len).filter(|pixels| !pixels.is_empty()) else {
        return false;
    };
    let mut checkpoint = || Ok::<_, std::convert::Infallible>(());
    match matte {
        Some(matte) => apply_set_matte_auxiliary_controlled(
            primary,
            width,
            height,
            matte,
            grade,
            grade.sampling(),
            &mut checkpoint,
        ),
        None => apply_set_matte_self_controlled(primary, grade, &mut checkpoint),
    }
    .unwrap_or(false)
}

pub(crate) fn apply_set_matte_self_controlled<E>(
    frame: &mut [[f32; 4]],
    grade: SetMatteGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    for band in frame.chunks_mut(4096) {
        checkpoint()?;
        for pixel in band {
            *pixel = grade.apply(*pixel, grade.associate(*pixel));
        }
    }
    Ok(true)
}

pub(crate) fn apply_set_matte_auxiliary_controlled<E>(
    primary: &mut [[f32; 4]],
    width: u32,
    height: u32,
    matte: AuxiliaryFloatFrame<'_>,
    grade: SetMatteGrade,
    sampling: AuxiliarySampling,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    apply_set_matte_auxiliary_region_controlled(
        primary,
        crate::adjustment::EffectRasterRegion::new(width, height, 0, 0, width, height),
        matte,
        grade,
        sampling,
        checkpoint,
    )
}

/// Apply Set Matte over one retained frame region.
///
/// The buffer holds only `region`'s pixels while sampling keeps complete-frame
/// coordinates, so a tiled temporal run matches a full-frame run.
pub(crate) fn apply_set_matte_auxiliary_region_controlled<E>(
    primary: &mut [[f32; 4]],
    region: crate::adjustment::EffectRasterRegion,
    matte: AuxiliaryFloatFrame<'_>,
    grade: SetMatteGrade,
    sampling: AuxiliarySampling,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if !region.is_valid_for(primary.len()) {
        return Ok(false);
    }
    let (width, height) = (region.width(), region.height());
    if width == 0 || height == 0 {
        return Ok(false);
    }
    let matte_extent = matte.frame.extent();
    if (matte_extent.width() as usize).checked_mul(matte_extent.height() as usize)
        != Some(matte.frame.pixels().len())
    {
        return Ok(false);
    }
    let frame_width = region.frame_width();
    let frame_height = region.frame_height();
    let matte_width = i64::from(matte_extent.width());
    let matte_height = i64::from(matte_extent.height());
    let offset_x = (i64::from(frame_width) - matte_width + 1).div_euclid(2);
    let offset_y = (i64::from(frame_height) - matte_height + 1).div_euclid(2);
    for (index, pixel) in primary.iter_mut().enumerate() {
        if index % 4096 == 0 {
            checkpoint()?;
        }
        let x = i64::from(region.x()) + (index % width as usize) as i64;
        let y = i64::from(region.y()) + (index / width as usize) as i64;
        let sample = if grade.stretch {
            let uv = [
                (x as f32 + PIXEL_CENTRE) / frame_width as f32,
                (y as f32 + PIXEL_CENTRE) / frame_height as f32,
            ];
            let associated = sample_auxiliary_associated_f32(matte, uv, sampling);
            if grade.premultiply {
                associated
            } else {
                straight_rgba_from_premultiplied(associated)
            }
        } else {
            let matte_x = x - offset_x;
            let matte_y = y - offset_y;
            let straight =
                if (0..matte_width).contains(&matte_x) && (0..matte_height).contains(&matte_y) {
                    matte.frame.pixels()[matte_y as usize * matte_width as usize + matte_x as usize]
                } else {
                    [0.0; 4]
                };
            grade.associate(straight)
        };
        *pixel = grade.apply(*pixel, sample);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EffectExecutionSession, EffectFloatFrameView, EffectFrameExtent, EffectGpuSpatialPass,
        EffectGpuStage, EffectProcessorControl, EffectType, EffectValueFormat,
        PreparedEffectProgram,
    };
    use mondrian_core::{
        AssetId, ClipId, ColorFrameAlpha, ImageSampling, PropertyValue, SequenceId, TimelineTime,
        VisualSourceReference, WorkingColorSpace,
    };

    fn view(width: u32, height: u32, pixels: &[[f32; 4]]) -> EffectFloatFrameView<'_> {
        EffectFloatFrameView::new(
            EffectFrameExtent::new(width, height),
            EffectValueFormat::new(
                crate::EffectWorkingPrecision::Float32,
                crate::EffectColorDomain::SceneLinearRgb,
                ColorFrameAlpha::StraightCoverage,
            ),
            ImageSampling::UNIT_SQUARE,
            pixels,
        )
    }

    fn authored(values: &[(&str, PropertyValue)]) -> crate::EffectNode {
        let kind = EffectType::SetMatte;
        let mut effect = crate::instantiate_effect_node(kind.clone()).unwrap();
        for (key, value) in values {
            effect
                .set_static_value_by_parameter(&kind.parameter_id(key).unwrap(), value.clone())
                .unwrap();
        }
        effect
    }

    fn compile(effect: &crate::EffectNode) -> std::sync::Arc<crate::CompiledEffectGraph> {
        PreparedEffectProgram::prepare(
            std::slice::from_ref(effect),
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .unwrap()
        .evaluate(TimelineTime::ZERO)
        .unwrap()
    }

    #[test]
    fn set_matte_default_reads_its_primary_and_squares_alpha() {
        let effect = authored(&[]);
        assert_eq!(effect.properties.iter().count(), 6);
        let compiled = compile(&effect);
        assert!(compiled.auxiliary_inputs().is_empty());
        assert!(matches!(
            crate::lower_effect_graph_to_gpu_pipeline(&compiled).unwrap().stages(),
            [EffectGpuStage::Spatial {
                pass: EffectGpuSpatialPass::SetMatte { sampling: None, external_slot: None, .. },
                ..
            }]
        ));
        let primary = [[0.1, 1.25, -0.5, 0.5], [4.0, -3.0, 1.0, 0.0]];
        let result =
            crate::apply_compiled_effect_graph_rgba_f32(view(2, 1, &primary), &compiled, 0)
                .unwrap();
        assert_eq!(result.pixels(), &[[0.1, 1.25, -0.5, 0.25], primary[1]]);
    }

    #[test]
    fn set_matte_bound_sources_compile_as_actual_grid_layer_inputs() {
        for reference in [
            VisualSourceReference::Asset {
                asset_id: AssetId::new(),
                retime: Default::default(),
            },
            VisualSourceReference::Clip { clip_id: ClipId::new(), retime: Default::default() },
            VisualSourceReference::Sequence {
                sequence_id: SequenceId::new(),
                retime: Default::default(),
            },
        ] {
            let compiled = compile(&authored(&[(
                "matte_source",
                PropertyValue::VisualSource(reference),
            )]));
            assert_eq!(
                compiled.auxiliary_source_binding(0).unwrap().reference(),
                reference
            );
            assert_eq!(
                compiled.auxiliary_inputs()[0].role(),
                crate::AuxiliaryRole::Layer
            );
            assert!(matches!(
                crate::lower_effect_graph_to_gpu_pipeline(&compiled).unwrap().stages(),
                [EffectGpuStage::Spatial {
                    pass: EffectGpuSpatialPass::SetMatte {
                        sampling: Some(_),
                        external_slot: Some(0),
                        ..
                    },
                    ..
                }]
            ));
        }
    }

    #[test]
    fn set_matte_freezes_auxiliary_retime_without_requesting_primary_history() {
        use crate::{EffectProcessingBackend, EffectWorkingPrecision};
        use mondrian_core::{VisualSourceBoundary, VisualSourceRetime};

        for ticks in [-1001, 1001] {
            let time_offset = TimelineTime::new(ticks, 24_000).unwrap();
            let reference = VisualSourceReference::Clip {
                clip_id: ClipId::new(),
                retime: VisualSourceRetime {
                    time_offset,
                    mode: VisualSourceBoundary::Transparent,
                },
            };
            let effect = authored(&[("matte_source", PropertyValue::VisualSource(reference))]);
            let graph = compile(&effect);
            assert_eq!(graph.auxiliary_inputs()[0].time_offset(), time_offset);
            assert_eq!(
                graph.auxiliary_source_binding(0).unwrap().reference(),
                reference
            );
            assert!(!graph.requires_primary_temporal_execution());
            for backend in [EffectProcessingBackend::Cpu, EffectProcessingBackend::Gpu] {
                graph
                    .admit_single_frame_backend_with_resolved_auxiliary(
                        backend,
                        EffectWorkingPrecision::Float32,
                    )
                    .unwrap();
            }
            assert!(crate::lower_effect_graph_to_gpu_pipeline(&graph).is_ok());
        }
    }

    #[test]
    fn set_matte_centers_the_odd_raster_and_reveals_hidden_primary_rgb() {
        let compiled = compile(&authored(&[
            (
                "matte_source",
                PropertyValue::VisualSource(VisualSourceReference::Asset {
                    asset_id: AssetId::new(),
                    retime: Default::default(),
                }),
            ),
            ("stretch", PropertyValue::Bool(false)),
            ("combine", PropertyValue::Bool(false)),
        ]));
        let source = [[1.25, -0.125, 0.75, 0.0]; 4];
        let binding = [crate::AuxiliaryFloatInput {
            slot: 0,
            raster: AuxiliaryFloatFrame::new(
                view(1, 1, &[[0.2, 0.4, 0.6, 0.375]]),
                crate::AuxiliaryPolicy::new(crate::AuxiliaryResolution::Authored),
            ),
        }];
        let result = EffectExecutionSession::default()
            .apply_compiled_rgba_f32_with_auxiliary(
                view(4, 1, &source),
                &compiled,
                0,
                &binding,
                &mut EffectProcessorControl::uncancelled(),
            )
            .unwrap();
        assert_eq!(result.extent(), EffectFrameExtent::new(4, 1));
        assert_eq!(
            result.pixels(),
            &[source[0], source[0], [1.25, -0.125, 0.75, 0.375], source[0]]
        );
    }

    #[test]
    fn set_matte_stretch_filters_associated_color_through_the_transparent_border() {
        let raster = AuxiliaryFloatFrame::new(
            view(1, 1, &[[0.8, 0.25, 0.1, 0.5]]),
            crate::AuxiliaryPolicy::new(crate::AuxiliaryResolution::Authored),
        );
        for (premultiply, expected) in [(true, 0.3), (false, 0.8)] {
            let mut frame = [[0.0; 4]; 2];
            let grade = SetMatteGrade {
                channel: SetMatteChannel::Red,
                combine: false,
                premultiply,
                ..Default::default()
            };
            assert!(set_matte_frame_f32(&mut frame, 2, 1, Some(raster), grade));
            for pixel in frame {
                assert!((pixel[3] - expected).abs() < 1.0e-6);
            }
        }
    }

    #[test]
    fn set_matte_replays_the_ten_host_channel_values() {
        let texel = [50.0 / 255.0, 115.0 / 255.0, 195.0 / 255.0, 95.0 / 255.0];
        let expected = [
            0.19607845,
            0.45098042,
            0.76470596,
            0.37254903,
            0.41052943,
            0.59195405,
            0.48039222,
            0.546_328_4,
            1.0,
            0.0,
        ];
        for ((key, _, channel), expected) in SetMatteChannel::OPTIONS.into_iter().zip(expected) {
            assert_eq!(SetMatteChannel::from_key(key), Some(channel));
            assert!((channel.coverage(texel) - expected).abs() < 2.0e-7, "{key}");
        }
        let grade = SetMatteGrade {
            channel: SetMatteChannel::Red,
            invert: true,
            ..Default::default()
        };
        let primary = [0.4, 0.5, 0.6, 0.5];
        let output = grade.apply(primary, grade.associate(texel));
        assert!((output[3] - 0.46347558).abs() < 2.0e-7);
    }

    #[test]
    fn set_matte_derived_channels_keep_finite_hdr_coverage() {
        let primary = [f32::MAX, -f32::MAX, 0.0, 1.0];
        let hue = SetMatteChannel::Hue.coverage(primary);
        assert!((hue - 11.0 / 12.0).abs() < 1.0e-6);
        assert_eq!(SetMatteChannel::Saturation.coverage([f32::MAX; 4]), 0.0);
    }
}
