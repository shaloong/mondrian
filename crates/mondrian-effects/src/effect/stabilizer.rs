//! Built-in Clip stabilization controls and prepared evaluation.

use super::{
    builtin_parameter_id, define_builtin_enum_property, define_builtin_property,
    EffectGraphBuildError, EffectResourceRecovery, PreparedEffectEvaluator,
};
use crate::stabilization::{
    StabilizationFraming, StabilizationMotion, StabilizationRecord, StabilizationWarp,
};
use crate::{EffectNode, EffectRenderOp, EffectType};
use mondrian_core::{
    automation::{ParameterEnumOption, PropertyBag, PropertyValue},
    TimelineTime,
};
use std::sync::Arc;

const GROUP: &str = "画面稳定器";
const DEFAULT_SMOOTHNESS: f32 = 50.0;
const DEFAULT_MAX_ZOOM: f32 = 150.0;
const DEFAULT_ADDITIONAL_ZOOM: f32 = 100.0;

pub(super) fn define_properties(properties: &mut PropertyBag) {
    for (key, label, options) in [
        (
            "result",
            "结果",
            [("smooth", "平滑运动"), ("lock", "锁定画面")],
        ),
        (
            "framing",
            "取景",
            [("auto_zoom", "自动缩放"), ("show_edges", "显示空边")],
        ),
    ] {
        define_builtin_enum_property(
            properties,
            &EffectType::Stabilizer,
            key,
            GROUP,
            label,
            options[0].0,
            options
                .into_iter()
                .map(|(option, name)| {
                    ParameterEnumOption::new(
                        option,
                        name,
                        format!("mondrian.effect.stabilizer.{key}.{option}"),
                    )
                })
                .collect(),
        );
    }
    for (key, label, default, min, max) in [
        ("smoothness", "平滑度", DEFAULT_SMOOTHNESS, 0.0, 1000.0),
        ("max_zoom", "最大缩放", DEFAULT_MAX_ZOOM, 100.0, 10_000.0),
        (
            "additional_zoom",
            "其他缩放",
            DEFAULT_ADDITIONAL_ZOOM,
            0.0,
            10_000.0,
        ),
    ] {
        define_builtin_property(
            properties,
            &EffectType::Stabilizer,
            key,
            GROUP,
            label,
            PropertyValue::Float(default),
            Some(min),
            Some(max),
            Some(0.1),
        );
    }
}

fn invalid(effect: &EffectNode, reason: impl ToString) -> EffectGraphBuildError {
    EffectGraphBuildError::InvalidAuthorState {
        effect_key: effect.effect_type.key(),
        effect_id: effect.id,
        reason: reason.to_string(),
    }
}

pub(super) fn prepare(
    effect: &EffectNode,
) -> Result<PreparedEffectEvaluator, EffectGraphBuildError> {
    let record = StabilizationRecord::from_effect(effect)
        .map_err(|error| invalid(effect, error))?
        .ok_or_else(|| EffectGraphBuildError::ResourceUnavailable {
            effect_key: effect.effect_type.key(),
            effect_id: effect.id,
            parameter_id: builtin_parameter_id(&EffectType::Stabilizer, "result"),
            recovery: EffectResourceRecovery::AuthorEdit,
            reason: "this Clip needs stabilization analysis".to_owned(),
        })?;
    let option = |key| {
        let id = builtin_parameter_id(&EffectType::Stabilizer, key);
        effect.evaluate_enum_parameter(&id, TimelineTime::ZERO)
    };
    let percent = |key, default| {
        let id = builtin_parameter_id(&EffectType::Stabilizer, key);
        f64::from(effect.evaluate_f32_parameter(&id, TimelineTime::ZERO, default)) / 100.0
    };
    let motion = match option("result").as_deref() {
        Some("lock") => StabilizationMotion::Lock,
        Some("smooth") | None => StabilizationMotion::Smooth {
            radius_seconds: percent("smoothness", DEFAULT_SMOOTHNESS),
        },
        _ => return Err(invalid(effect, "invalid stabilization result")),
    };
    let framing = match option("framing").as_deref() {
        Some("show_edges") => StabilizationFraming::ExposeEdges,
        Some("auto_zoom") | None => {
            StabilizationFraming::AutoZoom { max_scale: percent("max_zoom", DEFAULT_MAX_ZOOM) }
        }
        _ => return Err(invalid(effect, "invalid stabilization framing")),
    };
    let path = record
        .analysis()
        .prepare_path(
            motion,
            framing,
            percent("additional_zoom", DEFAULT_ADDITIONAL_ZOOM),
            || false,
        )
        .map_err(|error| invalid(effect, error))?;
    let retained = path.retained_bytes_estimate();
    let source = record.source();
    let (start, end) = record.analysis().analysis_interval();
    Ok(
        PreparedEffectEvaluator::new(Arc::new(move |effect, context, graph| {
            // Prepared graph topology is probed at zero, even for a trimmed
            // Clip whose first author time is later. The host checks coverage
            // before using any frame of its actual visible interval.
            let time =
                if context.time == TimelineTime::ZERO && !(start..end).contains(&context.time) {
                    start
                } else {
                    context.time
                };
            let warp = path
                .view_at(time)
                .and_then(StabilizationWarp::new)
                .map_err(|error| invalid(effect, error))?;
            graph
                .append_unary(EffectRenderOp::StabilizationWarp { warp: warp.with_source(source) });
            Ok(())
        }))
        .with_retained_resource_bytes(retained),
    )
}
