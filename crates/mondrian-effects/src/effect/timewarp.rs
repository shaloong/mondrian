//! Author time mapping for whole-frame and blended retiming.

use super::{
    builtin_parameter_id, define_builtin_enum_property, define_builtin_property, EffectEvalContext,
    EffectGraphBuildError, PreparedEffectEvaluator,
};
use crate::{EffectGraphBuilderState, EffectNode, EffectRenderOp, EffectType};
use mondrian_core::{
    automation::{
        AutomationQueryTime, ParameterEnumOption, PreparedPropertyQuery, PropertyBag, PropertyValue,
    },
    FramePosition, FrameRounding, Rational, TimeScale, TimelineTime,
};
use std::sync::Arc;

const GROUP: &str = "时间扭曲";
const DEFAULT_SPEED: f32 = 50.0;
const RANGE: f64 = 5000.0;

pub(super) fn define_properties(properties: &mut PropertyBag) {
    for (key, label, default, options) in [
        (
            "method",
            "方法",
            "frame_mix",
            [("whole_frames", "整帧保持"), ("frame_mix", "帧混合")],
        ),
        (
            "adjust_by",
            "调整时间方式",
            "speed",
            [("speed", "速度"), ("source_frame", "源帧")],
        ),
    ] {
        define_builtin_enum_property(
            properties,
            &EffectType::Timewarp,
            key,
            GROUP,
            label,
            default,
            options
                .into_iter()
                .map(|(option, name)| {
                    ParameterEnumOption::new(
                        option,
                        name,
                        format!("mondrian.effect.timewarp.{key}.{option}"),
                    )
                })
                .collect(),
        );
    }
    for (key, label, default) in [
        ("speed", "速度", DEFAULT_SPEED),
        ("source_frame", "源帧", 0.0),
    ] {
        define_builtin_property(
            properties,
            &EffectType::Timewarp,
            key,
            GROUP,
            label,
            PropertyValue::Float(default),
            Some(-RANGE),
            Some(RANGE),
            Some(0.01),
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
    let option = |key| {
        let id = builtin_parameter_id(&EffectType::Timewarp, key);
        effect.evaluate_enum_parameter(&id, TimelineTime::ZERO)
    };
    let mix = match option("method").as_deref() {
        Some("whole_frames") => false,
        Some("frame_mix") | None => true,
        _ => return Err(invalid(effect, "invalid Timewarp interpolation method")),
    };
    let use_speed = match option("adjust_by").as_deref() {
        Some("speed") | None => true,
        Some("source_frame") => false,
        _ => return Err(invalid(effect, "invalid Timewarp time adjustment")),
    };
    let speed = builtin_parameter_id(&EffectType::Timewarp, "speed");
    let curve = effect
        .properties
        .iter()
        .find_map(|(_, property)| {
            (use_speed
                && property.descriptor.parameter_id() == &speed
                && property.is_enabled()
                && property.is_animated())
            .then_some(property)
        })
        .map(|property| PreparedPropertyQuery::prepare(property, TimeScale::ONE))
        .transpose()
        .map_err(|error| invalid(effect, error))?;
    let retained_bytes = curve.as_ref().map_or(0, PreparedPropertyQuery::retained_bytes_estimate);
    let source_frame = builtin_parameter_id(&EffectType::Timewarp, "source_frame");
    Ok(
        PreparedEffectEvaluator::new(Arc::new(move |effect, context, graph| {
            let source_position = if use_speed {
                let constant = effect.evaluate_f32_parameter(&speed, context.time, DEFAULT_SPEED);
                if curve.is_none() && constant == 100.0 {
                    return Ok(());
                }
                source_position_from_speed(effect, context, curve.as_ref(), constant)?
            } else {
                f64::from(effect.evaluate_f32_parameter(&source_frame, context.time, 0.0))
            };
            append_sample(effect, context, graph, source_position, mix)
        }))
        .with_retained_resource_bytes(retained_bytes),
    )
}

fn source_position_from_speed(
    effect: &EffectNode,
    context: EffectEvalContext,
    curve: Option<&PreparedPropertyQuery>,
    speed: f32,
) -> Result<f64, EffectGraphBuildError> {
    let frame = context
        .frame_context
        .ok_or_else(|| invalid(effect, "Timewarp requires an owner frame grid"))?;
    let range = frame.available_range();
    let fps = frame.frame_rate();
    let elapsed = context.time.distance_from_f64(range.start);
    let half_frame = 0.5 / fps.to_f64();
    let (initial_speed, integral) = if let Some(curve) = curve {
        let at = |time| AutomationQueryTime::new(time).map_err(|error| invalid(effect, error));
        let start = at(range.start.to_f64())?;
        let center = at(context.time.to_f64() + half_frame)?;
        let initial_speed = curve
            .evaluate(start)
            .map_err(|error| invalid(effect, error))?
            .as_f32()
            .ok_or_else(|| invalid(effect, "Timewarp speed is not numeric"))?;
        let integral = curve.integrals(start, center).map_err(|error| invalid(effect, error))?;
        let [integral] = integral.as_slice() else {
            return Err(invalid(effect, "Timewarp speed must have one channel"));
        };
        (initial_speed, *integral)
    } else {
        (speed, (elapsed + half_frame) * f64::from(speed))
    };
    // Picture centers sit half a frame from the interval boundary. Reverse
    // motion starts at the right edge of the last available picture.
    let origin = if initial_speed < 0.0 {
        last_frame(range.duration, fps).map_err(|error| invalid(effect, error))? as f64 + 1.0
    } else {
        0.0
    };
    Ok(origin + integral * (fps.to_f64() / 100.0) - 0.5)
}

fn last_frame(
    duration: TimelineTime,
    fps: Rational,
) -> Result<i64, mondrian_core::TimelineTimeError> {
    Ok(duration.to_frame_position(fps, FrameRounding::Ceil)?.frame.saturating_sub(1))
}

fn append_sample(
    effect: &EffectNode,
    context: EffectEvalContext,
    graph: &mut EffectGraphBuilderState,
    position: f64,
    mix: bool,
) -> Result<(), EffectGraphBuildError> {
    if !position.is_finite() {
        return Err(invalid(effect, "Timewarp source position must be finite"));
    }
    let frame = context
        .frame_context
        .ok_or_else(|| invalid(effect, "Timewarp requires an owner frame grid"))?;
    let range = frame.available_range();
    let fps = frame.frame_rate();
    let last = last_frame(range.duration, fps).map_err(|error| invalid(effect, error))?;
    let rounded = position.round();
    let tolerance = (16.0 * f64::EPSILON * position.abs().max(1.0)).min(1.0e-6);
    let position = if (position - rounded).abs() <= tolerance {
        rounded
    } else {
        position
    };
    let position = position.clamp(0.0, last as f64);
    let frame_index = (position.floor() as i64).min(last);
    let weight = (position - frame_index as f64) as f32;
    let offset = |index| {
        TimelineTime::from_frame_position(FramePosition::new(
            index,
            Rational::new(fps.den, fps.num),
        ))
        .and_then(|time| range.start.checked_add(time))
        .and_then(|time| time.checked_sub(context.time))
        .map_err(|error| invalid(effect, error))
    };
    let (first, following, weight) = if mix && frame_index < last && weight > 0.0 {
        if weight == 1.0 {
            (offset(frame_index + 1)?, None, 0.0)
        } else {
            (offset(frame_index)?, Some(offset(frame_index + 1)?), weight)
        }
    } else {
        (offset(frame_index)?, None, 0.0)
    };
    if !first.is_zero() || following.is_some() {
        graph.append_unary(EffectRenderOp::TemporalFrameResample { first, following, weight });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        instantiate_effect_node, prepare_temporal_frame_execution, EffectExecutionContinuity,
        EffectFrameContext, EffectFrameExtent, EffectProjectGeometry,
        EffectTemporalExecutionRequest, LutPreparationCache, PreparedEffectProgram,
        TemporalGpuStepOperation,
    };
    use mondrian_core::{
        ExecutionCancellationToken, ImageSampling, Keyframe, SampleAspectRatio, TimelineTimeRange,
        WorkingColorSpace,
    };

    fn authored(values: &[(&str, PropertyValue)]) -> EffectNode {
        let mut effect = instantiate_effect_node(EffectType::Timewarp).unwrap();
        for (key, value) in values {
            effect
                .set_static_value_by_parameter(
                    &EffectType::Timewarp.parameter_id(key).unwrap(),
                    value.clone(),
                )
                .unwrap();
        }
        effect
    }

    fn program(
        effects: &[EffectNode],
        fps: Rational,
        start: TimelineTime,
    ) -> PreparedEffectProgram {
        let context = EffectFrameContext::new(
            fps,
            EffectProjectGeometry::new(6, 4, SampleAspectRatio::SQUARE).unwrap(),
            TimelineTimeRange::new(start, TimelineTime::new(2, 1).unwrap()).unwrap(),
        )
        .unwrap();
        PreparedEffectProgram::prepare_hierarchical_with_frame_context(
            effects,
            &[],
            &[],
            &[],
            WorkingColorSpace::LinearRec709,
            context,
            &LutPreparationCache::uncached(),
        )
        .unwrap()
    }

    fn frozen(
        program: &PreparedEffectProgram,
        now: TimelineTime,
    ) -> crate::PreparedEffectTemporalExecution {
        let extent = EffectFrameExtent::new(6, 4);
        let request = EffectTemporalExecutionRequest::new(
            1,
            EffectExecutionContinuity::Discontinuous,
            now,
            extent,
            ImageSampling::UNIT_SQUARE,
            extent.full_frame_roi(),
            ExecutionCancellationToken::new(),
        );
        prepare_temporal_frame_execution(program, &request, |_| Ok(1)).unwrap()
    }

    #[test]
    fn timewarp_mixes_two_samples_of_the_entering_stage_on_an_exact_fractional_grid() {
        let fps = Rational::new(30_000, 1001);
        let start = TimelineTime::new(1, 120).unwrap();
        let at = |frame: i64| {
            start.checked_add(TimelineTime::new(frame * 1001, 30_000).unwrap()).unwrap()
        };
        let prefix = instantiate_effect_node(EffectType::Invert).unwrap();
        let effect = authored(&[
            ("adjust_by", PropertyValue::Enum("source_frame".into())),
            ("method", PropertyValue::Enum("frame_mix".into())),
            ("source_frame", PropertyValue::Float(1.25)),
        ]);
        let execution = frozen(&program(&[prefix, effect], fps, start), at(8));
        let mut times = execution
            .demands()
            .requests()
            .iter()
            .map(|request| request.time())
            .collect::<Vec<_>>();
        times.sort();
        assert_eq!(times, [at(1), at(2)]);
        let gpu = execution.plan_gpu_execution().unwrap();
        assert_eq!(gpu.contexts().len(), 3);
        assert_eq!(
            gpu.step_count(),
            3,
            "each sample evaluates its own upstream prefix"
        );
        assert!(matches!(
            gpu.steps().last().unwrap().operation(),
            TemporalGpuStepOperation::TemporalBlend { input, sample, mix }
                if input != sample && *mix == 0.25
        ));
    }

    #[test]
    fn timewarp_speed_curve_uses_accumulated_distance_at_the_output_center() {
        let mut effect = authored(&[("method", PropertyValue::Enum("frame_mix".into()))]);
        for (time, speed) in [
            (TimelineTime::ZERO, 0.0),
            (TimelineTime::new(2, 1).unwrap(), 100.0),
        ] {
            effect
                .properties
                .set_keyframe(
                    &EffectType::Timewarp.property_path("speed"),
                    Keyframe::linear(time, PropertyValue::Float(speed)),
                )
                .unwrap();
        }
        let execution = frozen(
            &program(&[effect], Rational::new(4, 1), TimelineTime::ZERO),
            TimelineTime::ONE,
        );
        let mut times = execution
            .demands()
            .requests()
            .iter()
            .map(|request| request.time())
            .collect::<Vec<_>>();
        times.sort();
        assert_eq!(
            times,
            [TimelineTime::ZERO, TimelineTime::new(1, 4).unwrap()]
        );
        let gpu = execution.plan_gpu_execution().unwrap();
        assert!(matches!(
            gpu.steps().last().unwrap().operation(),
            TemporalGpuStepOperation::TemporalBlend { mix, .. } if *mix == 0.765625
        ));
    }

    #[test]
    fn timewarp_hold_and_reverse_request_only_the_selected_frame() {
        for (speed, time, expected) in [
            (-100.0, TimelineTime::ZERO, TimelineTime::new(7, 4).unwrap()),
            (0.0, TimelineTime::ONE, TimelineTime::ZERO),
            (1000.0, TimelineTime::ONE, TimelineTime::new(7, 4).unwrap()),
        ] {
            let effect = authored(&[("speed", PropertyValue::Float(speed))]);
            let program = program(&[effect], Rational::new(4, 1), TimelineTime::ZERO);
            let execution = frozen(&program, time);
            let [sample] = execution.demands().requests() else {
                panic!("whole frames require one exact sample");
            };
            assert_eq!(sample.time(), expected);
            let gpu = execution.plan_gpu_execution().unwrap();
            assert!(matches!(
                gpu.steps().first().unwrap().operation(),
                TemporalGpuStepOperation::TemporalHold { .. }
            ));
        }
        let effect = authored(&[("speed", PropertyValue::Float(100.0))]);
        let program = program(&[effect], Rational::new(4, 1), TimelineTime::ZERO);
        assert!(program
            .evaluate(TimelineTime::new(1, 8).unwrap())
            .unwrap()
            .graph()
            .is_identity());
    }
}
