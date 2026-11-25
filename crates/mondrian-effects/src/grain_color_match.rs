//! Pointwise color protection for the grain effects' final AlphaMask.

use mondrian_core::WorkingColorSpace;

/// Matching criterion, in the order exposed by the grain effects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum GrainColorMatchMode {
    /// Omit the color mask. A layer mask can still protect pixels.
    Off = 1,
    /// Compare all three working RGB channels.
    Rgb = 3,
    /// Compare circular hue, independent of intensity.
    Hue = 4,
    /// Compare channel spread.
    Chroma = 5,
    /// Compare working-space luminance.
    Luminance = 6,
    /// Compare only red.
    Red = 7,
    /// Compare only green.
    Green = 8,
    /// Compare only blue.
    Blue = 9,
}

impl GrainColorMatchMode {
    /// Stable persisted menu keys in authoring order. Index 2 is a host separator.
    pub const ALL: [Self; 8] = [
        Self::Off,
        Self::Rgb,
        Self::Hue,
        Self::Chroma,
        Self::Luminance,
        Self::Red,
        Self::Green,
        Self::Blue,
    ];

    /// Stable authoring key.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Rgb => "rgb",
            Self::Hue => "hue",
            Self::Chroma => "chroma",
            Self::Luminance => "luminance",
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
        }
    }

    /// Decode a persisted criterion.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.key() == key)
    }
}

/// Immutable coordinates used by the scalar matcher and device uniform packer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GrainColorMatch {
    mode: GrainColorMatchMode,
    reference: [f32; 3],
    coefficients: [f32; 3],
    tolerance: f32,
    softness: f32,
    invert: bool,
}

impl GrainColorMatch {
    pub(crate) fn new(
        mode: GrainColorMatchMode,
        reference: [f32; 3],
        tolerance: f32,
        softness: f32,
        invert: bool,
        working_space: WorkingColorSpace,
    ) -> Result<Option<Self>, crate::QualifierError> {
        if mode == GrainColorMatchMode::Off {
            return Ok(None);
        }
        if reference.into_iter().any(|value| !value.is_finite())
            || !(0.0..=1.0).contains(&tolerance)
            || !(0.0..=1.0).contains(&softness)
        {
            return Err(crate::QualifierError::InvalidColorMatch);
        }
        let coefficients = working_space.luminance_coefficients();
        Ok(Some(Self {
            mode,
            reference: project(mode, reference, coefficients),
            coefficients,
            tolerance,
            softness,
            invert,
        }))
    }

    pub(crate) fn gpu_controls(self) -> ([u32; 2], [f32; 4], f32) {
        let [r, g, b] = self.reference;
        (
            [self.mode as u32, u32::from(self.invert)],
            [r, g, b, self.tolerance],
            self.softness,
        )
    }

    pub(crate) fn coverage(self, rgb: [f32; 3]) -> f32 {
        let sample = project(self.mode, rgb, self.coefficients);
        let difference = (sample[0] - self.reference[0]).abs();
        let distance = match self.mode {
            GrainColorMatchMode::Rgb => {
                let [r, g, b] =
                    std::array::from_fn(|i| (sample[i] - self.reference[i]).abs().min(2.0));
                ((r * r + g * g + b * b) / 3.0).sqrt()
            }
            GrainColorMatchMode::Hue => {
                if sample[1] != self.reference[1] {
                    1.0
                } else {
                    2.0 * difference.min(1.0 - difference)
                }
            }
            _ => difference,
        }
        .min(1.0);
        let weight = if distance <= self.tolerance {
            1.0
        } else if self.softness == 0.0 {
            0.0
        } else {
            let t = ((distance - self.tolerance) / self.softness).min(1.0);
            1.0 - t * t * (3.0 - 2.0 * t)
        };
        if self.invert {
            1.0 - weight
        } else {
            weight
        }
    }
}

fn project(mode: GrainColorMatchMode, rgb: [f32; 3], coefficients: [f32; 3]) -> [f32; 3] {
    use GrainColorMatchMode::{Blue, Chroma, Green, Hue, Luminance, Off, Red, Rgb};
    let rgb = rgb.map(|value| value.max(0.0));
    let maximum = rgb[0].max(rgb[1]).max(rgb[2]);
    let chroma = maximum - rgb[0].min(rgb[1]).min(rgb[2]);
    match mode {
        Rgb => rgb,
        Hue => {
            if chroma <= f32::EPSILON {
                [0.0; 3]
            } else {
                [crate::hsl_space::hue_turns(rgb, maximum, chroma), 1.0, 0.0]
            }
        }
        Chroma => [chroma, 0.0, 0.0],
        Luminance => {
            let luminance = if maximum > 0.0 {
                let [r, g, b] = rgb.map(|v| v / maximum);
                let [kr, kg, kb] = coefficients;
                (r * kr + g * kg + b * kb).min(1.0) * maximum
            } else {
                0.0
            };
            [luminance, 0.0, 0.0]
        }
        Red => [rgb[0], 0.0, 0.0],
        Green => [rgb[1], 0.0, 0.0],
        Blue => [rgb[2], 0.0, 0.0],
        Off => [0.0; 3],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grain_color_match_uses_each_distinct_projection() {
        use GrainColorMatchMode::{Blue, Chroma, Green, Hue, Luminance, Off, Red, Rgb};
        let reference = [0.25, 0.5, 0.75];
        let space = WorkingColorSpace::LinearRec709;
        let [kr, kg, _] = space.luminance_coefficients();
        assert!(GrainColorMatch::new(Off, reference, 0.01, 0.0, true, space).unwrap().is_none());
        assert_eq!(
            GrainColorMatchMode::ALL.map(|mode| mode as u32),
            [1, 3, 4, 5, 6, 7, 8, 9]
        );
        for mode in GrainColorMatchMode::ALL {
            assert_eq!(GrainColorMatchMode::from_key(mode.key()), Some(mode));
        }
        for (mode, matching, missing) in [
            (Rgb, reference, [0.5, 0.5, 0.75]),
            (Hue, [0.125, 0.25, 0.375], [0.5; 3]),
            (Chroma, [0.75, 0.625, 0.25], [0.25, 0.5, 0.6]),
            (
                Luminance,
                [0.5, 0.5 - 0.25 * kr / kg, 0.75],
                [0.5, 0.5, 0.75],
            ),
            (Red, [0.25, 1.0, 0.0], [0.5, 0.5, 0.75]),
            (Green, [1.0, 0.5, 0.0], [0.25, 0.75, 0.75]),
            (Blue, [1.0, 0.0, 0.75], [0.25, 0.5, 1.0]),
        ] {
            let matcher =
                GrainColorMatch::new(mode, reference, 0.01, 0.0, false, space).unwrap().unwrap();
            assert_eq!(matcher.coverage(matching), 1.0, "{mode:?}");
            assert_eq!(matcher.coverage(missing), 0.0, "{mode:?}");
            assert!(matcher.coverage([f32::MAX, -f32::MAX, f32::MAX]).is_finite());
        }
        let red_wrap = GrainColorMatch::new(Hue, [1.0, 0.0, 0.02], 0.02, 0.0, false, space)
            .unwrap()
            .unwrap();
        assert_eq!(red_wrap.coverage([1.0, 0.02, 0.0]), 1.0);
        assert!(GrainColorMatch::new(Rgb, [f32::NAN; 3], 0.01, 0.0, false, space).is_err());
        assert!(GrainColorMatch::new(Rgb, reference, -0.01, 0.0, false, space).is_err());
    }

    #[test]
    fn grain_color_match_graph_keeps_landed_softness_and_inverted_endpoints() {
        use crate::{
            compile_reference_render_graph, EffectColorDomain, EffectColorDomainContract,
            EffectFloatFrameView, EffectFrameExtent, EffectGraphBuilderState, EffectRenderOp,
            EffectValueFormat, EffectWorkingPrecision, PreparedQualifier,
        };
        use mondrian_core::{ColorFrameAlpha, ImageSampling};
        use std::sync::Arc;

        let input = [
            [0.25, 0.1, 0.2, 0.0],
            [0.375, 0.1, 0.2, 1.0],
            [0.5, 0.1, 0.2, 0.25],
            [0.625, 0.1, 0.2, 0.75],
        ];
        let prepare = |invert| {
            let qualifier = PreparedQualifier::new_color_match(
                GrainColorMatchMode::Red,
                [0.25, 0.5, 0.75],
                0.125,
                0.25,
                invert,
                WorkingColorSpace::LinearRec709,
            )
            .unwrap()
            .unwrap();
            assert_eq!(qualifier.input_halo(), 0);
            assert_eq!(qualifier.gpu_pass_count(), 1);
            let mut graph = EffectGraphBuilderState::new();
            graph.append_unary_in_domain(
                EffectRenderOp::Qualifier { qualifier: Arc::new(qualifier) },
                EffectColorDomainContract {
                    input: EffectColorDomain::SceneLinearRgb,
                    output: EffectColorDomain::AlphaMask,
                },
            );
            graph.append_unary_in_domain(
                EffectRenderOp::MattePreview { invert: false },
                EffectColorDomainContract {
                    input: EffectColorDomain::AlphaMask,
                    output: EffectColorDomain::SceneLinearRgb,
                },
            );
            compile_reference_render_graph(graph.finish()).unwrap()
        };
        let normal = prepare(false);
        let inverted = prepare(true);
        assert_ne!(normal.identity(), inverted.identity());
        for (graph, coverage) in [
            (normal, [1.0, 1.0, 0.5, 0.0]),
            (inverted, [0.0, 0.0, 0.5, 1.0]),
        ] {
            let source = EffectFloatFrameView::new(
                EffectFrameExtent::new(4, 1),
                EffectValueFormat::new(
                    EffectWorkingPrecision::Float32,
                    EffectColorDomain::SceneLinearRgb,
                    ColorFrameAlpha::StraightCoverage,
                ),
                ImageSampling::UNIT_SQUARE,
                &input,
            );
            let result = crate::apply_compiled_effect_graph_rgba_f32(source, &graph, 0).unwrap();
            assert_eq!(result.pixels(), &coverage.map(|v| [v, v, v, 1.0]));
        }
    }
}
