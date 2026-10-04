//! Immutable continuous-time execution over canonical author curves.

mod integral;
use integral::IntegralAccumulator;

use super::{
    interpolate_numeric_value, numeric_segment_control, AnimatedProperty, Keyframe,
    KeyframeInterpolation, PropertyValue, PropertyValueType,
};
use crate::numeric_bezier;

/// Finite owner-local seconds at a floating-point execution boundary.
/// This coordinate is never persisted or quantized onto an author grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutomationQueryTime(f64);

impl AutomationQueryTime {
    /// Admit a finite execution coordinate in owner-local seconds.
    pub fn new(seconds: f64) -> Result<Self, AutomationQueryError> {
        if seconds.is_finite() {
            Ok(Self(seconds))
        } else {
            Err(AutomationQueryError::NonFiniteTime)
        }
    }

    /// Owner-local seconds supplied by the execution adapter.
    pub const fn seconds(self) -> f64 {
        self.0
    }
}

/// A canonical curve cannot answer the requested continuous-time query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AutomationQueryError {
    /// The execution coordinate contains NaN or infinity.
    #[error("automation query time must be finite")]
    NonFiniteTime,
    /// The author property failed canonical validation.
    #[error("automation query source is invalid")]
    InvalidSource,
    /// Distinct author keys collapse onto the same floating coordinate.
    #[error("automation keys cannot be distinguished at the execution time precision")]
    TimeResolution,
    /// Calculus is defined only for continuous floating numeric values.
    #[error("automation calculus requires a floating numeric property")]
    UnsupportedCalculus,
    /// A value, slope or integral cannot be represented as a finite number.
    #[error("automation query result is not finite")]
    NonFiniteResult,
}

/// Prepared execution snapshot using the same segment lowering and solver as
/// exact-time author evaluation. Preparation validates once; queries are read-only.
#[derive(Debug, Clone)]
pub struct PreparedPropertyQuery {
    property: AnimatedProperty,
    channels: Vec<PreparedChannel>,
    key_times: Vec<f64>,
}

impl PreparedPropertyQuery {
    /// Validate and freeze one canonical property without changing author state.
    pub fn prepare(property: &AnimatedProperty) -> Result<Self, AutomationQueryError> {
        property.validate().map_err(|_| AutomationQueryError::InvalidSource)?;
        let animated = property.is_enabled() && property.is_animated();
        let fallback = property
            .value_to_channel_values(property.static_value())
            .map_err(|_| AutomationQueryError::InvalidSource)?;
        let channels = if animated {
            property
                .channels()
                .iter()
                .enumerate()
                .map(|(index, channel)| {
                    let times =
                        channel.keyframes().iter().map(|key| key.time.to_f64()).collect::<Vec<_>>();
                    admit_projected_times(&times)?;
                    Ok(PreparedChannel {
                        times,
                        values: channel.keyframes().iter().map(|key| key.value).collect(),
                        segments: channel
                            .keyframes()
                            .windows(2)
                            .map(|keys| NumericSegment::between(&keys[0], &keys[1]))
                            .collect(),
                        fallback: fallback.get(index).copied().unwrap_or(0.0),
                    })
                })
                .collect::<Result<Vec<_>, AutomationQueryError>>()?
        } else {
            Vec::new()
        };
        let key_times = if animated {
            let times = property
                .keyframe_times()
                .into_iter()
                .map(|time| time.to_f64())
                .collect::<Vec<_>>();
            admit_projected_times(&times)?;
            times
        } else {
            Vec::new()
        };
        Ok(Self { property: property.clone(), channels, key_times })
    }

    /// Ordered union of all enabled channel key times, in owner-local seconds.
    pub fn key_times(&self) -> &[f64] {
        &self.key_times
    }

    /// Evaluate the complete scalar or compound value without author-time rounding.
    pub fn evaluate(
        &self,
        time: AutomationQueryTime,
    ) -> Result<PropertyValue, AutomationQueryError> {
        if self.channels.is_empty() {
            return Ok(self.property.static_value().clone());
        }
        let values = self
            .channels
            .iter()
            .map(|channel| finite(channel.evaluate(time.0)))
            .collect::<Result<Vec<_>, _>>()?;
        if self.property.value_type().uses_f32_channels()
            && values.iter().any(|value| value.abs() > f64::from(f32::MAX))
        {
            return Err(AutomationQueryError::NonFiniteResult);
        }
        Ok(self.property.value_from_channel_values(&values, self.property.static_value()))
    }

    /// Right-hand owner-time derivatives. Before the first key and at/after the
    /// last key, endpoint extension has slope zero. Hold jumps are not impulses.
    /// A vertical slope returns an error rather than an invented finite value.
    pub fn derivatives(&self, time: AutomationQueryTime) -> Result<Vec<f64>, AutomationQueryError> {
        self.admit_calculus()?;
        if self.channels.is_empty() {
            return Ok(vec![0.0; self.property.channel_count()]);
        }
        self.channels.iter().map(|channel| channel.derivative(time.0)).collect()
    }

    /// Signed owner-time integrals, including endpoint extension and exact empty
    /// intervals. Cubic integration uses the shared parametric curve geometry.
    pub fn integrals(
        &self,
        start: AutomationQueryTime,
        end: AutomationQueryTime,
    ) -> Result<Vec<f64>, AutomationQueryError> {
        self.admit_calculus()?;
        if self.channels.is_empty() {
            return self
                .property
                .static_value()
                .to_channel_values()
                .into_iter()
                .map(|value| constant_integral(value, start.0, end.0))
                .collect();
        }
        self.channels.iter().map(|channel| channel.integral(start.0, end.0)).collect()
    }

    fn admit_calculus(&self) -> Result<(), AutomationQueryError> {
        match self.property.value_type() {
            PropertyValueType::Float
            | PropertyValueType::Double
            | PropertyValueType::DoubleVec2
            | PropertyValueType::DoubleVec3
            | PropertyValueType::DoubleRgb
            | PropertyValueType::DoubleColor
            | PropertyValueType::Vec2
            | PropertyValueType::Vec3
            | PropertyValueType::Color
            | PropertyValueType::Vec4 => Ok(()),
            _ => Err(AutomationQueryError::UnsupportedCalculus),
        }
    }
}

fn admit_projected_times(times: &[f64]) -> Result<(), AutomationQueryError> {
    if times.iter().any(|time| !time.is_finite()) || times.windows(2).any(|pair| pair[0] >= pair[1])
    {
        Err(AutomationQueryError::TimeResolution)
    } else {
        Ok(())
    }
}

fn finite(value: f64) -> Result<f64, AutomationQueryError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(AutomationQueryError::NonFiniteResult)
    }
}

fn constant_integral(value: f64, start: f64, end: f64) -> Result<f64, AutomationQueryError> {
    if value == 0.0 || start == end {
        return Ok(0.0);
    }
    let mut sum = IntegralAccumulator::empty();
    sum.add_product(value, end, false);
    sum.add_product(value, start, true);
    sum.finish().ok_or(AutomationQueryError::NonFiniteResult)
}

#[derive(Debug, Clone)]
struct PreparedChannel {
    times: Vec<f64>,
    values: Vec<f64>,
    segments: Vec<NumericSegment>,
    fallback: f64,
}

impl PreparedChannel {
    fn evaluate(&self, time: f64) -> f64 {
        let Some(&first) = self.times.first() else {
            return self.fallback;
        };
        let right = self.times.partition_point(|key| *key <= time);
        if time <= first {
            return self.values[0];
        }
        if right == self.times.len() {
            return self.values[right - 1];
        }
        self.segments[right - 1]
            .evaluate((time - self.times[right - 1]) / (self.times[right] - self.times[right - 1]))
    }

    fn derivative(&self, time: f64) -> Result<f64, AutomationQueryError> {
        let right = self.times.partition_point(|key| *key <= time);
        if right == 0 || right == self.times.len() {
            return Ok(0.0);
        }
        let span = self.times[right] - self.times[right - 1];
        self.segments[right - 1].derivative((time - self.times[right - 1]) / span, span)
    }

    fn integral(&self, start: f64, end: f64) -> Result<f64, AutomationQueryError> {
        if start == end {
            return Ok(0.0);
        }
        if start > end {
            return self.integral(end, start).map(|value| -value);
        }
        if self.times.is_empty() {
            return constant_integral(self.fallback, start, end);
        }
        let mut sum = IntegralAccumulator::empty();
        let mut left = start;
        while left < end {
            let right_index = self.times.partition_point(|key| *key <= left);
            let right = self.times.get(right_index).copied().unwrap_or(end).min(end);
            let mean = if right_index == 0 {
                self.values[0]
            } else if right_index == self.times.len() {
                self.values[right_index - 1]
            } else {
                let origin = self.times[right_index - 1];
                let span = self.times[right_index] - origin;
                self.segments[right_index - 1].mean((left - origin) / span, (right - origin) / span)
            };
            sum.add_product(mean, right, false);
            sum.add_product(mean, left, true);
            left = right;
        }
        sum.finish().ok_or(AutomationQueryError::NonFiniteResult)
    }
}

#[derive(Debug, Clone)]
pub(super) enum NumericSegment {
    Hold(f64),
    Linear([f64; 2]),
    Bezier {
        times: [f64; 2],
        values: [f64; 4],
        uniform_time: bool,
    },
}

impl NumericSegment {
    pub(super) fn between(a: &Keyframe<f64>, b: &Keyframe<f64>) -> Self {
        match (a.interp_out, b.interp_in) {
            (KeyframeInterpolation::Hold, _) | (_, KeyframeInterpolation::Hold) => {
                Self::Hold(a.value)
            }
            (KeyframeInterpolation::Linear, KeyframeInterpolation::Linear) => {
                Self::Linear([a.value, b.value])
            }
            _ => {
                let span = b.time.distance_from_f64(a.time);
                let exact_span = b.time.checked_sub(a.time).ok();
                let out = numeric_segment_control(
                    a.interp_out,
                    a.value,
                    b.value,
                    span,
                    exact_span,
                    false,
                );
                let incoming =
                    numeric_segment_control(b.interp_in, a.value, b.value, span, exact_span, true);
                Self::Bezier {
                    times: [out.0, incoming.0],
                    values: [a.value, out.1, incoming.1, b.value],
                    uniform_time: out.2 && incoming.2,
                }
            }
        }
    }

    pub(super) fn evaluate(&self, progress: f64) -> f64 {
        match *self {
            Self::Hold(value) => value,
            Self::Linear([a, b]) => interpolate_numeric_value(a, b, progress),
            Self::Bezier { times, values, uniform_time } => {
                numeric_bezier::evaluate_numeric_bezier(progress, times, values, uniform_time)
            }
        }
    }

    fn derivative(&self, progress: f64, span: f64) -> Result<f64, AutomationQueryError> {
        match *self {
            Self::Hold(_) => Ok(0.0),
            Self::Linear([a, b]) => {
                let difference = b - a;
                finite(if difference.is_finite() {
                    difference / span
                } else {
                    b / span - a / span
                })
            }
            Self::Bezier { times, values, uniform_time } => {
                numeric_bezier::numeric_bezier_derivative(
                    progress,
                    times,
                    values,
                    uniform_time,
                    span,
                )
                .ok_or(AutomationQueryError::NonFiniteResult)
            }
        }
    }

    fn mean(&self, start: f64, end: f64) -> f64 {
        match *self {
            Self::Hold(value) => value,
            Self::Linear([a, b]) => interpolate_numeric_value(a, b, start * 0.5 + end * 0.5),
            Self::Bezier { times, values, uniform_time } => {
                numeric_bezier::numeric_bezier_mean(start, end, times, values, uniform_time)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::InterpolationType;
    use crate::automation::{
        ExactBezierHandle, ParameterInvalidValuePolicy, ParameterNumericContract, ParameterUnit,
        PropertyDescriptor,
    };
    use crate::TimelineTime;

    fn time(value: f64) -> AutomationQueryTime {
        AutomationQueryTime::new(value).expect("finite query")
    }
    fn close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 1e-12 * expected.abs().max(1.0),
            "{actual} != {expected}"
        );
    }
    fn property(default: PropertyValue, keys: Vec<Keyframe<PropertyValue>>) -> AnimatedProperty {
        let descriptor = PropertyDescriptor::new("query.value", "Query", default)
            .with_numeric_contract(
                ParameterUnit::Unitless,
                ParameterNumericContract::closed(
                    -f64::MAX,
                    f64::MAX,
                    None,
                    ParameterInvalidValuePolicy::Reject,
                )
                .expect("bounds"),
            );
        let mut property = AnimatedProperty::from_descriptor(descriptor);
        for key in keys {
            property.set_exact_keyframe(key).expect("admitted key");
        }
        property.set_animation_enabled(true);
        property
    }
    fn scalar(query: &PreparedPropertyQuery, at: f64) -> f64 {
        let PropertyValue::Double(value) = query.evaluate(time(at)).expect("value") else {
            panic!("double");
        };
        value
    }
    fn cubic(
        times: [TimelineTime; 2],
        controls: [(TimelineTime, f64); 2],
        values: [f64; 2],
    ) -> AnimatedProperty {
        let mut left = Keyframe::linear(times[0], PropertyValue::Double(values[0]));
        let mut right = Keyframe::linear(times[1], PropertyValue::Double(values[1]));
        left.interp_out = KeyframeInterpolation::Bezier(ExactBezierHandle {
            time_offset: controls[0].0,
            value_offset: controls[0].1,
        });
        right.interp_in = KeyframeInterpolation::Bezier(ExactBezierHandle {
            time_offset: controls[1].0,
            value_offset: controls[1].1,
        });
        property(PropertyValue::Double(0.0), vec![left, right])
    }

    #[test]
    fn uniform_cubic_queries_match_independent_polynomial() {
        let property = cubic(
            [TimelineTime::ZERO, TimelineTime::new(2, 1).expect("two")],
            [
                (TimelineTime::new(2, 3).expect("out"), 0.0),
                (TimelineTime::new(-2, 3).expect("in"), 0.0),
            ],
            [0.0, 1.0],
        );
        let query = PreparedPropertyQuery::prepare(&property).expect("query");
        for step in 0..64 {
            let u = f64::from(step) / 64.0;
            close(scalar(&query, 2.0 * u), 3.0 * u * u - 2.0 * u * u * u);
            close(
                query.derivatives(time(2.0 * u)).expect("slope")[0],
                3.0 * u - 3.0 * u * u,
            );
            close(
                query.integrals(time(0.0), time(2.0 * u)).expect("integral")[0],
                2.0 * (u.powi(3) - 0.5 * u.powi(4)),
            );
            assert_eq!(
                query.evaluate(time(2.0 * u)).expect("query"),
                property.evaluate(TimelineTime::new(i64::from(step), 32).expect("exact"))
            );
        }
        close(query.integrals(time(0.0), time(2.0)).expect("full")[0], 1.0);
        close(
            query.integrals(time(3.0), time(-1.0)).expect("reverse")[0],
            -2.0,
        );
        assert_eq!(query.derivatives(time(2.0)).expect("extension"), vec![0.0]);
    }

    #[test]
    fn nonuniform_cubic_uses_parametric_calculus() {
        // x(u)=u², y(u)=u³: y(x)=x^(3/2), independently of the host solver.
        let property = cubic(
            [TimelineTime::ZERO, TimelineTime::ONE],
            [
                (TimelineTime::ZERO, 0.0),
                (TimelineTime::new(-2, 3).expect("in"), -1.0),
            ],
            [0.0, 1.0],
        );
        let query = PreparedPropertyQuery::prepare(&property).expect("query");
        for x in [0.0_f64, 0.015625, 0.0625, 0.25, 0.5625, 0.9] {
            close(scalar(&query, x), x.powf(1.5));
            close(
                query.derivatives(time(x)).expect("derivative")[0],
                1.5 * x.sqrt(),
            );
            close(
                query.integrals(time(0.0), time(x)).expect("integral")[0],
                0.4 * x.powf(2.5),
            );
        }
        close(
            query.integrals(time(1.0), time(0.0)).expect("reverse")[0],
            -0.4,
        );
    }

    #[test]
    fn cubic_stationary_time_controls_distinguish_finite_and_vertical_slopes() {
        // x(u)=u³, y(u)=u³ has a finite derivative even at its stationary endpoint.
        let same = cubic(
            [TimelineTime::ZERO, TimelineTime::ONE],
            [
                (TimelineTime::ZERO, 0.0),
                (TimelineTime::NEGATIVE_ONE, -1.0),
            ],
            [0.0, 1.0],
        );
        close(
            PreparedPropertyQuery::prepare(&same)
                .expect("query")
                .derivatives(time(0.0))
                .expect("finite limit")[0],
            1.0,
        );
        // x(u)=u², y(u)=u gives a vertical slope at the first key.
        let vertical = cubic(
            [TimelineTime::ZERO, TimelineTime::ONE],
            [
                (TimelineTime::ZERO, 1.0 / 3.0),
                (TimelineTime::new(-2, 3).expect("in"), -1.0 / 3.0),
            ],
            [0.0, 1.0],
        );
        let query = PreparedPropertyQuery::prepare(&vertical).expect("query");
        assert_eq!(
            query.derivatives(time(0.0)),
            Err(AutomationQueryError::NonFiniteResult)
        );
        close(query.derivatives(time(0.25)).expect("finite slope")[0], 1.0);
    }

    #[test]
    fn equal_endpoints_retain_value_excursion_and_integral() {
        let property = cubic(
            [TimelineTime::ZERO, TimelineTime::ONE],
            [
                (TimelineTime::ONE_THIRD, 2.0),
                (TimelineTime::NEGATIVE_ONE_THIRD, 2.0),
            ],
            [0.0, 0.0],
        );
        let query = PreparedPropertyQuery::prepare(&property).expect("query");
        close(scalar(&query, 0.5), 1.5);
        close(query.derivatives(time(0.5)).expect("peak")[0], 0.0);
        close(query.integrals(time(0.0), time(1.0)).expect("area")[0], 1.0);
    }

    #[test]
    fn hold_queries_are_right_continuous_with_signed_area() {
        let mut keys = vec![
            Keyframe::linear(TimelineTime::ZERO, PropertyValue::Double(2.0)),
            Keyframe::linear(TimelineTime::ONE, PropertyValue::Double(5.0)),
            Keyframe::linear(
                TimelineTime::new(2, 1).expect("two"),
                PropertyValue::Double(7.0),
            ),
        ];
        for key in &mut keys {
            key.interp_in = KeyframeInterpolation::Hold;
            key.interp_out = KeyframeInterpolation::Hold;
        }
        let query = PreparedPropertyQuery::prepare(&property(PropertyValue::Double(0.0), keys))
            .expect("query");
        assert_eq!(scalar(&query, 0.999), 2.0);
        assert_eq!(scalar(&query, 1.0), 5.0);
        assert_eq!(query.derivatives(time(1.0)).expect("hold"), vec![0.0]);
        close(
            query.integrals(time(-1.0), time(3.0)).expect("area")[0],
            16.0,
        );
        close(
            query.integrals(time(3.0), time(-1.0)).expect("signed")[0],
            -16.0,
        );
    }

    #[test]
    fn compound_values_calculus_and_union_keys_are_complete() {
        let mut property = property(PropertyValue::DoubleVec3([1.0, 2.0, 3.0]), vec![]);
        property
            .write_channels(
                TimelineTime::ZERO,
                &[(0, 1.0), (2, 3.0)],
                InterpolationType::Linear,
            )
            .expect("sparse start");
        property
            .write_channels(TimelineTime::ONE, &[(0, 5.0)], InterpolationType::Linear)
            .expect("first channel");
        property
            .write_channels(
                TimelineTime::new(2, 1).expect("two"),
                &[(2, 7.0)],
                InterpolationType::Linear,
            )
            .expect("third channel");
        let query = PreparedPropertyQuery::prepare(&property).expect("query");
        assert_eq!(query.key_times(), [0.0, 1.0, 2.0]);
        assert_eq!(
            query.evaluate(time(0.5)).expect("vector"),
            PropertyValue::DoubleVec3([3.0, 2.0, 4.0])
        );
        assert_eq!(
            query.derivatives(time(0.5)).expect("slopes"),
            [4.0, 0.0, 2.0]
        );
        assert_eq!(
            query.integrals(time(0.0), time(2.0)).expect("areas"),
            [8.0, 4.0, 10.0]
        );
    }

    #[test]
    fn frozen_snapshot_and_disabled_animation_preserve_author_state() {
        let mut property = property(
            PropertyValue::Double(9.0),
            vec![
                Keyframe::linear(TimelineTime::ZERO, PropertyValue::Double(1.0)),
                Keyframe::linear(TimelineTime::ONE, PropertyValue::Double(3.0)),
            ],
        );
        let original = property.clone();
        let query = PreparedPropertyQuery::prepare(&property).expect("query");
        assert_eq!(property, original);
        property.set_animation_enabled(false);
        property.set_static_value(PropertyValue::Double(8.0)).expect("edit");
        assert_eq!(scalar(&query, 0.5), 2.0);
        let disabled = PreparedPropertyQuery::prepare(&property).expect("disabled");
        assert!(disabled.key_times().is_empty());
        assert_eq!(scalar(&disabled, -500.0), 8.0);
        assert_eq!(disabled.derivatives(time(0.5)).expect("constant"), [0.0]);
        assert_eq!(
            disabled.integrals(time(2.0), time(0.0)).expect("signed"),
            [-16.0]
        );
    }

    #[test]
    fn extreme_constant_intervals_and_subnormals_do_not_overflow_intermediates() {
        let query = PreparedPropertyQuery::prepare(&property(PropertyValue::Double(0.25), vec![]))
            .expect("query");
        assert_eq!(
            query.integrals(time(-f64::MAX), time(f64::MAX)).expect("representable"),
            [f64::MAX * 0.5]
        );
        assert_eq!(
            query.integrals(time(f64::MAX), time(f64::MAX)).expect("empty"),
            [0.0]
        );
        let one = PreparedPropertyQuery::prepare(&property(PropertyValue::Double(1.0), vec![]))
            .expect("one");
        assert_eq!(
            one.integrals(time(0.0), time(f64::from_bits(1))).expect("subnormal"),
            [f64::from_bits(1)]
        );
        assert_eq!(
            one.integrals(time(-f64::MAX), time(f64::MAX)),
            Err(AutomationQueryError::NonFiniteResult)
        );
    }

    #[test]
    fn large_linear_slopes_and_signed_piece_cancellation_remain_finite() {
        let query = PreparedPropertyQuery::prepare(&property(
            PropertyValue::Double(0.0),
            vec![
                Keyframe::linear(TimelineTime::ZERO, PropertyValue::Double(f64::MAX)),
                Keyframe::linear(
                    TimelineTime::new(2, 1).expect("two"),
                    PropertyValue::Double(-f64::MAX),
                ),
            ],
        ))
        .expect("query");
        assert_eq!(
            query.derivatives(time(1.0)).expect("finite slope"),
            [-f64::MAX]
        );
        assert_eq!(
            query.integrals(time(0.0), time(2.0)).expect("cancellation"),
            [0.0]
        );
        let mut keys = vec![
            Keyframe::linear(TimelineTime::ZERO, PropertyValue::Double(f64::MAX)),
            Keyframe::linear(TimelineTime::ONE, PropertyValue::Double(-f64::MAX)),
            Keyframe::linear(
                TimelineTime::new(2, 1).expect("two"),
                PropertyValue::Double(0.0),
            ),
        ];
        for key in &mut keys {
            key.interp_in = KeyframeInterpolation::Hold;
            key.interp_out = KeyframeInterpolation::Hold;
        }
        assert_eq!(
            PreparedPropertyQuery::prepare(&property(PropertyValue::Double(0.0), keys))
                .expect("query")
                .integrals(time(0.0), time(2.0))
                .expect("piece cancellation"),
            [0.0]
        );
    }

    #[test]
    fn query_admission_rejects_nonfinite_time_and_collapsed_exact_keys() {
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                AutomationQueryTime::new(invalid),
                Err(AutomationQueryError::NonFiniteTime)
            );
        }
        let property = property(
            PropertyValue::Double(0.0),
            vec![
                Keyframe::linear(
                    TimelineTime::new(1_i64 << 53, 1).expect("large"),
                    PropertyValue::Double(1.0),
                ),
                Keyframe::linear(
                    TimelineTime::new((1_i64 << 53) + 1, 1).expect("next"),
                    PropertyValue::Double(2.0),
                ),
            ],
        );
        assert!(matches!(
            PreparedPropertyQuery::prepare(&property),
            Err(AutomationQueryError::TimeResolution)
        ));
    }

    #[test]
    fn categorical_and_integer_queries_preserve_type_and_reject_calculus() {
        // Integer defaults admit Hold. This explicit bounded schema models an
        // interpolated native Integer, whose conversion rounds canonical channels.
        let mut descriptor =
            PropertyDescriptor::new("query.integer", "Integer", PropertyValue::Int(0))
                .with_numeric_contract(
                    ParameterUnit::Unitless,
                    ParameterNumericContract::closed(
                        -10.0,
                        10.0,
                        None,
                        ParameterInvalidValuePolicy::Reject,
                    )
                    .expect("bounds"),
                );
        descriptor.schema.allowed_interpolations =
            vec![super::super::ParameterInterpolation::Linear];
        let mut property = AnimatedProperty::from_descriptor(descriptor);
        for key in [
            Keyframe::linear(TimelineTime::ZERO, PropertyValue::Int(0)),
            Keyframe::linear(TimelineTime::ONE, PropertyValue::Int(3)),
        ] {
            property.set_exact_keyframe(key).expect("interpolated Integer key");
        }
        property.set_animation_enabled(true);
        let query = PreparedPropertyQuery::prepare(&property).expect("query");
        assert_eq!(
            query.evaluate(time(0.5)).expect("integer"),
            PropertyValue::Int(2)
        );
        assert_eq!(
            query.derivatives(time(0.5)),
            Err(AutomationQueryError::UnsupportedCalculus)
        );
        assert_eq!(
            query.integrals(time(0.0), time(1.0)),
            Err(AutomationQueryError::UnsupportedCalculus)
        );
        let mut boolean = AnimatedProperty::from_descriptor(PropertyDescriptor::new(
            "query.boolean",
            "Boolean",
            PropertyValue::Bool(false),
        ));
        for (at, value) in [(TimelineTime::ZERO, false), (TimelineTime::ONE, true)] {
            boolean
                .set_exact_keyframe(Keyframe::from_preset(
                    at,
                    PropertyValue::Bool(value),
                    InterpolationType::Hold,
                ))
                .expect("held Boolean");
        }
        boolean.set_animation_enabled(true);
        let held = PreparedPropertyQuery::prepare(&boolean).expect("Boolean query");
        assert_eq!(
            held.evaluate(time(0.5)).expect("held"),
            PropertyValue::Bool(false)
        );
        assert_eq!(
            held.evaluate(time(1.0)).expect("key"),
            PropertyValue::Bool(true)
        );
        assert_eq!(
            held.derivatives(time(0.5)),
            Err(AutomationQueryError::UnsupportedCalculus)
        );
    }

    #[test]
    fn tiny_nonuniform_times_preserve_relative_query_precision() {
        let property = cubic(
            [TimelineTime::ZERO, TimelineTime::ONE],
            [
                (TimelineTime::ZERO, 0.0),
                (TimelineTime::new(-2, 3).expect("in"), -1.0),
            ],
            [0.0, 1.0],
        );
        let query = PreparedPropertyQuery::prepare(&property).expect("query");
        for x in [1e-20_f64, 1e-100] {
            let expected = x.powf(1.5);
            let value = scalar(&query, x);
            assert!(
                (value / expected - 1.0).abs() < 1e-12,
                "value at {x}: {value} != {expected}"
            );
            let slope = query.derivatives(time(x)).expect("slope")[0];
            assert!(
                (slope / (1.5 * x.sqrt()) - 1.0).abs() < 1e-12,
                "slope at {x}: {slope}"
            );
            let area = query.integrals(time(0.0), time(x)).expect("tiny area")[0];
            assert!(
                (area / (0.4 * x.powf(2.5)) - 1.0).abs() < 1e-12,
                "area at {x}: {area}"
            );
        }
    }

    #[test]
    fn wide_integral_retains_tiny_pulses_after_large_piece_cancellation() {
        for large in [0.0, f64::MAX] {
            let mut keys = [(-5, 0.0), (-4, large), (-2, -large), (0, 1e-300), (1, 0.0)]
                .into_iter()
                .map(|(at, value)| {
                    Keyframe::from_preset(
                        TimelineTime::new(at, 1).expect("key time"),
                        PropertyValue::Double(value),
                        InterpolationType::Hold,
                    )
                })
                .collect::<Vec<_>>();
            // The two +/- large pieces each have width two; their areas cancel
            // even when each area alone exceeds the Float64 result range.
            let query = PreparedPropertyQuery::prepare(&property(
                PropertyValue::Double(0.0),
                std::mem::take(&mut keys),
            ))
            .expect("query");
            let area = query.integrals(time(-f64::MAX), time(f64::MAX)).expect("finite area")[0];
            assert_eq!(area, 1e-300, "pulse after cancellation, large={large}");
        }
    }
}
