//! Authoring and graph assembly for native, source-calibrated grain reduction.

use super::{
    builtin_parameter_id, define_builtin_enum_property, define_builtin_property, EffectEvalContext,
    EffectFrameContext, EffectGraphBuildError,
};
use crate::grain_calibration::{GrainCalibrationRequest, GrainCalibrationSettings};
use crate::grain_spatial::{GrainSpatialGrade, GrainSpatialShaping};
use crate::grain_temporal::GrainTemporalGrade;
use crate::{
    EffectColorDomain, EffectColorDomainContract, EffectGraphBuilderState, EffectGraphNodeId,
    EffectNode, EffectRenderOp, EffectTemporalInputExtent, EffectType, EffectValueFormat,
    EffectWorkingPrecision, GrainColorMatchMode, MaskCombineMode, PreparedQualifier,
    SetMatteChannel,
};
use mondrian_core::{
    automation::{ParameterEnumOption, PropertyBag, PropertyValue},
    Color, ColorFrameAlpha, TimelineTime, VisualSourceReference,
};
use std::sync::Arc;

const REDUCTION: &str = "降噪";
const TEMPORAL: &str = "降噪时序滤波";
const DETAIL: &str = "钝化蒙版";
const CALIBRATION: &str = "降噪采样";
const PROTECTION: &str = "降噪混合";
const PREVIEW: &str = "降噪预览区域";
const MATCH_MODES: [&str; 8] = [
    "关闭", "RGB", "色相", "色度", "亮度", "红色", "绿色", "蓝色",
];
pub(super) const SAMPLE_POINTS: [&str; 10] = [
    "sample_1",
    "sample_2",
    "sample_3",
    "sample_4",
    "sample_5",
    "sample_6",
    "sample_7",
    "sample_8",
    "sample_9",
    "sample_10",
];

pub(crate) fn author_time_extent(
    effect: &EffectNode,
    context: Option<EffectFrameContext>,
) -> EffectTemporalInputExtent {
    use crate::graph::temporal_input_extent_for_sample_offset as extent_for_offset;
    let parameter = |key| builtin_parameter_id(&EffectType::Denoise, key);
    let may_use = |key, predicate: fn(&PropertyValue) -> bool| {
        let id = parameter(key);
        effect.properties.iter().any(|(_, property)| {
            property.descriptor.parameter_id() == &id
                && (predicate(property.static_value())
                    || (property.is_enabled() && property.is_animated()))
        })
    };
    let positive = |value: &PropertyValue| value.as_f32().is_some_and(|value| value > 0.0);
    let view = effect.evaluate_enum_parameter(&parameter("view"), TimelineTime::ZERO);
    if view.as_deref() == Some("samples") {
        return EffectTemporalInputExtent::CURRENT_FRAME;
    }
    let matte_view = view.as_deref() == Some("matte");
    let mut extent = EffectTemporalInputExtent::CURRENT_FRAME;
    if !matte_view
        && may_use("temporal_enabled", |value| {
            matches!(value, PropertyValue::Bool(true))
        })
        && may_use("temporal_amount", positive)
    {
        let Some([past, future]) = context.and_then(|context| frame_offsets(context).ok()) else {
            return EffectTemporalInputExtent::UNBOUNDED;
        };
        extent = extent_for_offset(past).merge(extent_for_offset(future));
    }
    if (matte_view || may_use("blend_amount", positive))
        && let Some(retime) = effect
            .evaluate_visual_source_parameter(&parameter("matte_source"))
            .and_then(VisualSourceReference::retime)
    {
        extent = extent.merge(extent_for_offset(retime.time_offset));
    }
    extent
}

fn frame_offsets(
    context: EffectFrameContext,
) -> Result<[TimelineTime; 2], mondrian_core::TimelineTimeError> {
    let fps = context.frame_rate();
    let step = TimelineTime::new(fps.den, fps.num)?;
    Ok([TimelineTime::ZERO.checked_sub(step)?, step])
}

pub(super) fn define_properties(properties: &mut PropertyBag, kind: &EffectType) {
    let define_menu =
        |properties: &mut PropertyBag, key, group, label, default, options: &[(&str, &str)]| {
            define_builtin_enum_property(
                properties,
                kind,
                key,
                group,
                label,
                default,
                options
                    .iter()
                    .map(|&(option, label)| {
                        ParameterEnumOption::new(
                            option,
                            label,
                            format!("mondrian.effect.denoise.{key}.{option}"),
                        )
                    })
                    .collect(),
            );
        };
    define_menu(
        properties,
        "view",
        REDUCTION,
        "查看模式",
        "final",
        &[
            ("final", "最终输出"),
            ("preview", "预览"),
            ("matte", "混合蒙版"),
            ("samples", "噪声样本"),
        ],
    );
    for (key, label) in [("preview_width", "宽度"), ("preview_height", "高度")] {
        define_builtin_property(
            properties,
            kind,
            key,
            PREVIEW,
            label,
            PropertyValue::Float(200.0),
            Some(50.0),
            Some(1024.0),
            Some(1.0),
        );
    }
    for (key, label, value) in [
        (
            "preview_center",
            "中心",
            PropertyValue::Vec2(glam::Vec2::splat(128.0)),
        ),
        ("preview_border", "显示方框", PropertyValue::Bool(true)),
        (
            "preview_border_color",
            "方框颜色",
            PropertyValue::Color(Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }),
        ),
    ] {
        define_builtin_property(
            properties, kind, key, PREVIEW, label, value, None, None, None,
        );
    }
    define_menu(
        properties,
        "reduction_mode",
        REDUCTION,
        "降噪模式",
        "linked",
        &[("linked", "多通道"), ("per_channel", "按通道")],
    );
    for (key, group, label, value, min, max, step) in [
        ("amount", REDUCTION, "降噪量", 1.0, 0.0, 100.0, 0.01),
        ("red_amount", REDUCTION, "红色降噪量", 1.0, 0.0, 100.0, 0.01),
        (
            "green_amount",
            REDUCTION,
            "绿色降噪量",
            1.0,
            0.0,
            100.0,
            0.01,
        ),
        (
            "blue_amount",
            REDUCTION,
            "蓝色降噪量",
            1.0,
            0.0,
            100.0,
            0.01,
        ),
        ("chroma", REDUCTION, "色度抑制", 1.0, 0.0, 10.0, 0.01),
        ("texture", REDUCTION, "纹理", 0.1, 0.0, 1.0, 0.01),
        ("size_bias", REDUCTION, "颗粒大小偏差", 0.0, -1.0, 1.0, 0.01),
        (
            "solid_cleanup",
            REDUCTION,
            "纯色区域清理",
            0.0,
            0.0,
            1.0,
            0.01,
        ),
        (
            "temporal_amount",
            TEMPORAL,
            "时序滤波量",
            100.0,
            0.0,
            100.0,
            0.01,
        ),
        (
            "motion_sensitivity",
            TEMPORAL,
            "运动敏感度",
            0.9,
            0.0,
            1.0,
            0.01,
        ),
        ("detail_amount", DETAIL, "细节恢复量", 0.0, 0.0, 5.0, 0.01),
        ("detail_radius", DETAIL, "半径", 1.0, 0.0, 24.0, 0.01),
        ("detail_threshold", DETAIL, "阈值", 0.0, 0.0, 1.0, 0.01),
        (
            "blend_amount",
            PROTECTION,
            "与原图混合量",
            0.0,
            0.0,
            100.0,
            0.01,
        ),
        ("matte_blur", PROTECTION, "模糊蒙版", 0.0, 0.0, 100.0, 0.01),
        (
            "match_tolerance",
            PROTECTION,
            "匹配容差",
            0.1,
            0.0,
            1.0,
            0.01,
        ),
        (
            "match_softness",
            PROTECTION,
            "匹配柔度",
            0.1,
            0.0,
            1.0,
            0.01,
        ),
    ] {
        define_builtin_property(
            properties,
            kind,
            key,
            group,
            label,
            PropertyValue::Float(value),
            Some(min),
            Some(max),
            Some(step),
        );
    }
    for (key, label, value, min, max) in [
        ("sample_frame", "源帧", 0, 0.0, 100_000.0),
        ("sample_count", "样本数量", 8, 1.0, 10.0),
        ("sample_size", "样本大小", 24, 16.0, 256.0),
    ] {
        define_builtin_property(
            properties,
            kind,
            key,
            CALIBRATION,
            label,
            PropertyValue::Int(value),
            Some(min),
            Some(max),
            Some(1.0),
        );
    }
    define_menu(
        properties,
        "sample_selection",
        CALIBRATION,
        "样本选择",
        "automatic",
        &[("automatic", "自动"), ("manual", "手动")],
    );
    define_builtin_property(
        properties,
        kind,
        "sample_box_color",
        CALIBRATION,
        "采样框颜色",
        PropertyValue::Color(Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }),
        None,
        None,
        None,
    );
    for (index, key) in SAMPLE_POINTS.into_iter().enumerate() {
        define_builtin_property(
            properties,
            kind,
            key,
            CALIBRATION,
            &format!("样本点 {}", index + 1),
            PropertyValue::Vec2(glam::Vec2::splat(128.0)),
            None,
            None,
            None,
        );
    }
    define_menu(
        properties,
        "matte_combination",
        PROTECTION,
        "组合匹配和图层蒙版",
        "multiply",
        &[("multiply", "正片叠底"), ("screen", "滤色")],
    );
    let matching = GrainColorMatchMode::ALL.map(GrainColorMatchMode::key);
    let match_options = std::array::from_fn::<_, 8, _>(|i| (matching[i], MATCH_MODES[i]));
    define_menu(
        properties,
        "match_mode",
        PROTECTION,
        "颜色匹配模式",
        "rgb",
        &match_options,
    );
    define_menu(
        properties,
        "matte_channel",
        PROTECTION,
        "蒙版通道",
        "intensity",
        &[
            ("intensity", "强度"),
            ("alpha", "Alpha"),
            ("inverted_intensity", "反相强度"),
            ("inverted_alpha", "反相 Alpha"),
        ],
    );
    define_menu(
        properties,
        "matte_size",
        PROTECTION,
        "图层尺寸不同时",
        "center",
        &[("center", "居中"), ("stretch", "拉伸")],
    );
    for (key, group, label, value) in [
        (
            "temporal_enabled",
            TEMPORAL,
            "启用时序滤波",
            PropertyValue::Bool(false),
        ),
        (
            "match_invert",
            PROTECTION,
            "反转颜色匹配",
            PropertyValue::Bool(false),
        ),
        (
            "match_color",
            PROTECTION,
            "匹配颜色",
            PropertyValue::Color(Color { r: 0.5, g: 0.5, b: 0.5, a: 1.0 }),
        ),
        (
            "matte_source",
            PROTECTION,
            "蒙版图层",
            PropertyValue::VisualSource(VisualSourceReference::Unbound),
        ),
    ] {
        define_builtin_property(properties, kind, key, group, label, value, None, None, None);
    }
}

/// All reads address the Definition's schema rather than Clip-namespaced paths.
struct Controls<'a> {
    effect: &'a EffectNode,
    context: EffectEvalContext,
}

impl Controls<'_> {
    fn read(&self, key: &str) -> PropertyValue {
        self.effect
            .evaluate_parameter(
                &builtin_parameter_id(&EffectType::Denoise, key),
                self.context.time,
            )
            .expect("prepared denoise schema supplies every declared parameter")
    }

    fn number(&self, key: &str) -> f32 {
        self.read(key).as_f32().expect("denoise numeric parameter")
    }

    fn integer(&self, key: &str) -> i64 {
        let PropertyValue::Int(value) = self.read(key) else {
            unreachable!("denoise integer parameter");
        };
        value
    }

    fn menu(&self, key: &str) -> String {
        let PropertyValue::Enum(selected) = self.read(key) else {
            unreachable!("denoise enum parameter");
        };
        selected
    }

    fn enabled(&self, key: &str) -> bool {
        let PropertyValue::Bool(value) = self.read(key) else {
            unreachable!("denoise boolean parameter");
        };
        value
    }

    fn invalid(&self, error: impl ToString) -> EffectGraphBuildError {
        EffectGraphBuildError::InvalidAuthorState {
            effect_key: self.effect.effect_type.key(),
            effect_id: self.effect.id,
            reason: error.to_string(),
        }
    }

    fn calibration(&self) -> Result<GrainCalibrationRequest, EffectGraphBuildError> {
        let count = self.integer("sample_count") as u8;
        let size = self.integer("sample_size") as u16;
        let settings = if self.menu("sample_selection") == "manual" {
            let mut centers = [[0; 2]; SAMPLE_POINTS.len()];
            for (center, key) in centers.iter_mut().zip(SAMPLE_POINTS).take(usize::from(count)) {
                let PropertyValue::Vec2(point) = self.read(key) else {
                    unreachable!("denoise sample point");
                };
                *center = point.to_array().map(|value| value.round() as u32);
            }
            GrainCalibrationSettings::manual(size, &centers[..usize::from(count)])
        } else {
            GrainCalibrationSettings::new(size, count)
        }
        .map_err(|error| self.invalid(error))?;
        GrainCalibrationRequest::new(self.integer("sample_frame") as u32, settings)
            .ok_or_else(|| self.invalid("invalid fixed calibration frame"))
    }

    fn append_filter(
        &self,
        graph: &mut EffectGraphBuilderState,
    ) -> Result<(), EffectGraphBuildError> {
        let amounts = if self.menu("reduction_mode") == "per_channel" {
            ["red_amount", "green_amount", "blue_amount"].map(|key| self.number(key))
        } else {
            [self.number("amount"); 3]
        };
        let temporal_mix = if self.enabled("temporal_enabled") {
            self.number("temporal_amount") / 100.0
        } else {
            0.0
        };
        if temporal_mix > 0.0 || amounts.iter().any(|&amount| amount > 0.0) {
            let calibration = self.calibration()?;
            if temporal_mix > 0.0 {
                let context = self.context.frame_context.ok_or_else(|| {
                    self.invalid("temporal denoise requires the sequence frame grid")
                })?;
                let offsets = frame_offsets(context).map_err(|error| self.invalid(error))?;
                let grade = GrainTemporalGrade::awaiting_source_calibration(
                    offsets.map(Some),
                    calibration,
                    temporal_mix,
                    self.number("motion_sensitivity"),
                )
                .map_err(|error| self.invalid(error))?;
                graph.append_unary(EffectRenderOp::TemporalGrain { grade });
            }
            if amounts.iter().any(|&amount| amount > 0.0) {
                let shaping = GrainSpatialShaping {
                    texture: self.number("texture"),
                    size_bias: self.number("size_bias"),
                    solid_cleanup: self.number("solid_cleanup"),
                    chroma: self.number("chroma"),
                };
                let grade =
                    GrainSpatialGrade::awaiting_source_calibration(calibration, amounts, shaping)
                        .map_err(|error| self.invalid(error))?;
                graph.append_unary(EffectRenderOp::SpatialGrain { grade });
            }
        }
        let gain = self.number("detail_amount");
        let radius = self.number("detail_radius");
        if gain > crate::unsharp_mask::MIN_EFFECTIVE_GAIN && radius > 0.0 {
            graph.append_unary(EffectRenderOp::UnsharpMask {
                gain,
                radius,
                threshold: self.number("detail_threshold"),
            });
        }
        Ok(())
    }

    fn append_protection(
        &self,
        graph: &mut EffectGraphBuilderState,
        original: EffectGraphNodeId,
    ) -> Result<EffectGraphNodeId, EffectGraphBuildError> {
        let mode = GrainColorMatchMode::from_key(&self.menu("match_mode"))
            .ok_or_else(|| self.invalid("invalid color matching mode"))?;
        let PropertyValue::Color(color) = self.read("match_color") else {
            unreachable!("denoise reference color");
        };
        let match_color = PreparedQualifier::new_color_match(
            mode,
            [color.r, color.g, color.b],
            self.number("match_tolerance"),
            self.number("match_softness"),
            self.enabled("match_invert"),
            self.context.working_color_space,
        )
        .map_err(|error| self.invalid(error))?;
        let color_mask =
            match_color.map(|qualifier| append_color_matte(graph, original, qualifier));
        let source = builtin_parameter_id(&EffectType::Denoise, "matte_source");
        let layer_mask = match self.effect.evaluate_visual_source_parameter(&source) {
            Some(VisualSourceReference::Unbound) => None,
            Some(_) => {
                let (channel, invert) = match self.menu("matte_channel").as_str() {
                    "intensity" => (SetMatteChannel::Luminance, false),
                    "alpha" => (SetMatteChannel::Alpha, false),
                    "inverted_intensity" => (SetMatteChannel::Luminance, true),
                    "inverted_alpha" => (SetMatteChannel::Alpha, true),
                    _ => return Err(self.invalid("invalid denoise layer matte channel")),
                };
                let auxiliary = graph.append_visual_source_input(
                    self.effect,
                    &source,
                    EffectValueFormat::new(
                        EffectWorkingPrecision::Float32,
                        EffectColorDomain::SceneLinearRgb,
                        ColorFrameAlpha::StraightCoverage,
                    ),
                    EffectColorDomainContract::SCENE_LINEAR,
                )?;
                Some(graph.add_layer_matte(
                    original,
                    auxiliary,
                    channel,
                    invert,
                    self.menu("matte_size") == "stretch",
                ))
            }
            None => return Err(self.invalid("invalid denoise layer source")),
        };
        let matte = match (color_mask, layer_mask) {
            (Some(color), Some(layer)) => {
                let mode = if self.menu("matte_combination") == "screen" {
                    MaskCombineMode::Screen
                } else {
                    MaskCombineMode::Multiply
                };
                graph.add_mask_combine_with_mode(color, layer, mode)
            }
            (Some(mask), None) | (None, Some(mask)) => mask,
            (None, None) => {
                // RGB distances are bounded by one. At this tolerance every
                // source value yields exact full coverage for uniform mixing.
                let uniform = PreparedQualifier::new_color_match(
                    GrainColorMatchMode::Rgb,
                    [0.0; 3],
                    1.0,
                    0.0,
                    false,
                    self.context.working_color_space,
                )
                .map_err(|error| self.invalid(error))?
                .expect("constant full matte");
                return Ok(append_color_matte(graph, original, uniform));
            }
        };
        graph.set_current_output(matte);
        graph.append_matte_feather(self.number("matte_blur"))
    }
}

fn append_color_matte(
    graph: &mut EffectGraphBuilderState,
    original: EffectGraphNodeId,
    qualifier: PreparedQualifier,
) -> EffectGraphNodeId {
    graph.set_current_output(original);
    graph.append_unary_in_domain(
        EffectRenderOp::Qualifier { qualifier: Arc::new(qualifier) },
        EffectColorDomainContract {
            input: EffectColorDomain::SceneLinearRgb,
            output: EffectColorDomain::AlphaMask,
        },
    )
}

pub(super) fn build_graph(
    effect: &EffectNode,
    context: EffectEvalContext,
    graph: &mut EffectGraphBuilderState,
) -> Result<(), EffectGraphBuildError> {
    let controls = Controls { effect, context };
    let original = graph.current_output();
    let view = controls.menu("view");
    if view == "samples" {
        let PropertyValue::Color(color) = controls.read("sample_box_color") else {
            unreachable!("denoise sample box color");
        };
        let display = graph.add_fixed_grain_sample_display(
            controls.calibration()?,
            [color.r, color.g, color.b, color.a],
        )?;
        graph.set_current_output(display);
        return Ok(());
    }
    if view == "matte" {
        let matte = controls.append_protection(graph, original)?;
        graph.set_current_output(matte);
        graph.append_unary_in_domain(
            EffectRenderOp::MattePreview { invert: false },
            EffectColorDomainContract {
                input: EffectColorDomain::AlphaMask,
                output: EffectColorDomain::SceneLinearRgb,
            },
        );
        return Ok(());
    }

    controls.append_filter(graph)?;
    let filtered = graph.current_output();
    let amount = controls.number("blend_amount") / 100.0;
    if amount > 0.0 && filtered != original {
        let matte = controls.append_protection(graph, original)?;
        let mixed = graph.add_matte_mix_with_amount(filtered, original, matte, amount);
        graph.set_current_output(mixed);
    }
    if view == "preview" {
        let PropertyValue::Vec2(center) = controls.read("preview_center") else {
            unreachable!("denoise preview center");
        };
        let PropertyValue::Color(border) = controls.read("preview_border_color") else {
            unreachable!("denoise preview border color");
        };
        let border = if controls.enabled("preview_border") {
            [border.r, border.g, border.b, border.a]
        } else {
            [0.0; 4]
        };
        let window = crate::EffectPreviewWindow::new(
            center.to_array(),
            [
                controls.number("preview_width"),
                controls.number("preview_height"),
            ],
            border,
        )
        .ok_or_else(|| controls.invalid("invalid denoise preview window"))?;
        if graph.current_output() != original || border[3] > 0.0 {
            let window = graph.add_preview_window(original, graph.current_output(), window);
            graph.set_current_output(window);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{compile_clip_effect_graph, instantiate_effect_node, EffectGraphNodeKind};
    use mondrian_core::WorkingColorSpace;

    fn set(effect: &mut EffectNode, key: &str, value: PropertyValue) {
        effect
            .set_static_value_by_parameter(&EffectType::Denoise.parameter_id(key).unwrap(), value)
            .unwrap();
    }

    fn compile(effect: EffectNode) -> Arc<crate::CompiledEffectGraph> {
        compile_clip_effect_graph(
            &[effect],
            &[],
            TimelineTime::ZERO,
            WorkingColorSpace::LinearRec709,
        )
        .unwrap()
    }

    #[test]
    fn denoise_only_requests_primary_history_at_instants_with_a_temporal_kernel() {
        use mondrian_core::{Keyframe, Rational, SampleAspectRatio, TimelineTimeRange};

        let mut effect = instantiate_effect_node(EffectType::Denoise).unwrap();
        set(&mut effect, "temporal_enabled", PropertyValue::Bool(true));
        let end = TimelineTime::new(5, 25).unwrap();
        for (time, amount) in [(TimelineTime::ZERO, 100.0), (end, 0.0)] {
            effect
                .properties
                .set_keyframe(
                    &EffectType::Denoise.property_path("temporal_amount"),
                    Keyframe::linear(time, PropertyValue::Float(amount)),
                )
                .unwrap();
        }
        let range = TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::ONE).unwrap();
        let geometry =
            crate::EffectProjectGeometry::new(64, 64, SampleAspectRatio::SQUARE).unwrap();
        let context = EffectFrameContext::new(Rational::FPS_25, geometry, range).unwrap();
        let program = crate::PreparedEffectProgram::prepare_hierarchical_with_frame_context(
            &[effect],
            &[],
            &[],
            &[],
            WorkingColorSpace::LinearRec709,
            context,
            &crate::LutPreparationCache::uncached(),
        )
        .unwrap();
        let temporal = program.evaluate(TimelineTime::ZERO).unwrap();
        let spatial = program.evaluate(end).unwrap();
        assert!(temporal.requires_primary_temporal_execution());
        assert!(!spatial.requires_primary_temporal_execution());
        assert_ne!(
            temporal.primary_temporal_input_extent().unwrap(),
            EffectTemporalInputExtent::CURRENT_FRAME
        );
        assert_eq!(
            spatial.primary_temporal_input_extent().unwrap(),
            EffectTemporalInputExtent::CURRENT_FRAME
        );
    }

    #[test]
    fn denoise_manual_calibration_uses_authored_source_frame_and_only_active_points() {
        let mut effect = instantiate_effect_node(EffectType::Denoise).unwrap();
        let default_graph = compile(effect.clone());
        let default_request = default_graph.grain_calibration_requests().next().unwrap();
        assert_eq!(default_request.frame_index(), 0);
        assert_eq!(
            default_request.settings(),
            GrainCalibrationSettings::DEFAULT
        );

        set(
            &mut effect,
            "sample_selection",
            PropertyValue::Enum("manual".into()),
        );
        set(&mut effect, "sample_count", PropertyValue::Int(1));
        set(&mut effect, "sample_size", PropertyValue::Int(16));
        set(&mut effect, "sample_frame", PropertyValue::Int(4));
        set(
            &mut effect,
            "sample_1",
            PropertyValue::Vec2(glam::vec2(27.8, 19.4)),
        );
        let graph = compile(effect.clone());
        let requests = graph.grain_calibration_requests().collect::<Vec<_>>();
        assert_eq!(
            requests,
            [GrainCalibrationRequest::new(
                4,
                GrainCalibrationSettings::manual(16, &[[28, 19]]).unwrap(),
            )
            .unwrap()]
        );
        set(
            &mut effect,
            "sample_2",
            PropertyValue::Vec2(glam::Vec2::ZERO),
        );
        assert_eq!(graph.identity(), compile(effect).identity());
    }

    #[test]
    fn denoise_detail_and_weighted_matte_graph_use_the_author_values() {
        let mut effect = instantiate_effect_node(EffectType::Denoise).unwrap();
        set(&mut effect, "amount", PropertyValue::Float(0.0));
        set(&mut effect, "detail_amount", PropertyValue::Float(2.5));
        set(&mut effect, "detail_radius", PropertyValue::Float(3.0));
        set(&mut effect, "detail_threshold", PropertyValue::Float(0.02));
        set(&mut effect, "blend_amount", PropertyValue::Float(65.0));
        set(&mut effect, "match_mode", PropertyValue::Enum("off".into()));
        let graph = compile(effect.clone());
        assert_eq!(graph.grain_calibration_requests().count(), 0);
        assert!(graph.graph().nodes.iter().any(|node| matches!(
            node.kind,
            EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::UnsharpMask { gain: 2.5, radius: 3.0, threshold: 0.02 },
                ..
            }
        )));
        assert!(graph.graph().nodes.iter().any(|node| matches!(
            node.kind,
            EffectGraphNodeKind::MatteMix { amount: 0.65, .. }
        )));
        set(&mut effect, "detail_radius", PropertyValue::Float(0.0));
        assert!(compile(effect).graph().is_identity());
    }

    #[test]
    fn denoise_matte_observation_reads_the_entering_color_without_sampling_grain() {
        let mut effect = instantiate_effect_node(EffectType::Denoise).unwrap();
        set(&mut effect, "view", PropertyValue::Enum("matte".into()));
        set(&mut effect, "match_mode", PropertyValue::Enum("red".into()));
        set(&mut effect, "match_tolerance", PropertyValue::Float(0.0));
        set(&mut effect, "match_softness", PropertyValue::Float(0.0));
        let graph = compile(effect);
        assert_eq!(graph.grain_calibration_requests().count(), 0);
        let pixels = [[0.5, 0.25, 0.8, 0.5], [0.25, 0.5, 0.5, 0.25]];
        let frame = crate::EffectFloatFrameView::new(
            crate::EffectFrameExtent::new(2, 1),
            EffectValueFormat::new(
                EffectWorkingPrecision::Float32,
                EffectColorDomain::SceneLinearRgb,
                ColorFrameAlpha::StraightCoverage,
            ),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            &pixels,
        );
        let output = crate::apply_compiled_effect_graph_rgba_f32(frame, &graph, 0).unwrap();
        assert_eq!(output.pixels(), &[[1.0; 4], [0.0, 0.0, 0.0, 1.0]]);
    }

    #[test]
    fn denoise_preview_compares_on_the_sampled_source_grid_after_output_mixing() {
        use mondrian_core::{ImageSampling, Rational, SampleAspectRatio};

        let mut effect = instantiate_effect_node(EffectType::Denoise).unwrap();
        for (key, value) in [
            ("amount", PropertyValue::Float(0.0)),
            ("detail_amount", PropertyValue::Float(1.0)),
            ("detail_radius", PropertyValue::Float(2.0)),
            ("blend_amount", PropertyValue::Float(25.0)),
            ("match_mode", PropertyValue::Enum("off".into())),
            (
                "preview_center",
                PropertyValue::Vec2(glam::vec2(64.0, 32.0)),
            ),
            ("preview_width", PropertyValue::Float(50.0)),
            ("preview_height", PropertyValue::Float(50.0)),
            ("preview_border", PropertyValue::Bool(false)),
        ] {
            set(&mut effect, key, value);
        }
        let final_graph = compile(effect.clone());
        assert!(!final_graph.uses_source_grid_observation());
        let mut moved = effect.clone();
        set(
            &mut moved,
            "preview_center",
            PropertyValue::Vec2(glam::vec2(0.0, 0.0)),
        );
        assert_eq!(
            compile(moved).semantic_fingerprint(),
            final_graph.semantic_fingerprint()
        );

        let pixels = (0..64 * 64)
            .map(|i| [if i % 2 == 0 { 0.7 } else { 0.4 }, 0.3, -0.1, 0.35])
            .collect::<Vec<_>>();
        let frame = crate::EffectFloatFrameView::new(
            crate::EffectFrameExtent::new(64, 64),
            EffectValueFormat::new(
                EffectWorkingPrecision::Float32,
                EffectColorDomain::SceneLinearRgb,
                ColorFrameAlpha::StraightCoverage,
            ),
            ImageSampling::new(
                SampleAspectRatio::new(4, 3).unwrap(),
                [Rational::new(1, 2), Rational::new(1, 1)],
            )
            .unwrap(),
            &pixels,
        );
        let render = |graph| crate::apply_compiled_effect_graph_rgba_f32(frame, graph, 0).unwrap();
        let final_output = render(&final_graph);
        set(&mut effect, "view", PropertyValue::Enum("preview".into()));
        let preview_graph = compile(effect.clone());
        let preview = render(&preview_graph);
        let inside = 32 * 64 + 30;
        let fractional = 32 * 64 + 19;
        assert!(preview_graph.uses_source_grid_observation());
        assert_eq!(preview.pixels()[0], pixels[0]);
        assert_ne!(final_output.pixels()[inside], pixels[inside]);
        assert_eq!(preview.pixels()[inside], final_output.pixels()[inside]);
        assert_eq!(
            preview.pixels()[fractional][0],
            (pixels[fractional][0] + final_output.pixels()[fractional][0]) * 0.5
        );

        set(&mut effect, "preview_border", PropertyValue::Bool(true));
        let outlined_graph = compile(effect);
        assert_ne!(
            outlined_graph.semantic_fingerprint(),
            preview_graph.semantic_fingerprint()
        );
        let outlined = render(&outlined_graph);
        assert_eq!(outlined.pixels()[inside], final_output.pixels()[inside]);
        assert_eq!(outlined.pixels()[0], pixels[0]);
        assert_eq!(
            outlined.pixels()[fractional][0],
            (1.0 + preview.pixels()[fractional][0]) * 0.5
        );
        assert!(outlined.pixels().iter().zip(&pixels).all(|(a, b)| a[3] == b[3]));
    }

    #[test]
    fn denoise_detail_accepts_positive_subpixel_radii_in_both_backends() {
        let mut effect = instantiate_effect_node(EffectType::Denoise).unwrap();
        set(&mut effect, "amount", PropertyValue::Float(0.0));
        set(&mut effect, "detail_amount", PropertyValue::Float(5.0));
        set(&mut effect, "detail_radius", PropertyValue::Float(0.05));
        let graph = compile(effect);
        let pixels = [
            [0.0, 0.0, 0.0, 1.0],
            [0.5, 0.5, 0.5, 1.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let frame = crate::EffectFloatFrameView::new(
            crate::EffectFrameExtent::new(3, 1),
            EffectValueFormat::new(
                EffectWorkingPrecision::Float32,
                EffectColorDomain::SceneLinearRgb,
                ColorFrameAlpha::StraightCoverage,
            ),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            &pixels,
        );
        let output = crate::apply_compiled_effect_graph_rgba_f32(frame, &graph, 0).unwrap();
        assert!(output.pixels()[1][0] > 0.5);
        let gpu = crate::lower_effect_graph_to_gpu_pipeline(&graph).unwrap();
        assert!(matches!(
            gpu.stages(),
            [crate::EffectGpuStage::Spatial {
                pass: crate::EffectGpuSpatialPass::UnsharpMask { radius: 0.05, .. },
                ..
            }]
        ));
    }
}
