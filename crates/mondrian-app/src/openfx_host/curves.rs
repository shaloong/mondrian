//! Worker-owned canonical snapshots behind the native read-only query ABI.

use super::*;
use mondrian_core::{
    AutomationQueryError, AutomationQueryTime, PreparedPropertyQuery, PropertyValue,
};

pub(super) type NativeQuery = unsafe extern "C" fn(
    *const c_void,
    c_int,
    f64,
    f64,
    c_int,
    c_int,
    *mut NativeQueryResult,
) -> c_int;

#[repr(C)]
pub(super) struct NativeQueryResult {
    pub value: [f64; 4],
    pub string_value: *const c_char,
    pub string_length: usize,
    pub key_count: u32,
    pub key_time: f64,
    pub key_index: c_int,
}

impl NativeQueryResult {
    pub const fn empty() -> Self {
        Self {
            value: [0.0; 4],
            string_value: std::ptr::null(),
            string_length: 0,
            key_count: 0,
            key_time: 0.0,
            key_index: -1,
        }
    }
}

pub(super) struct CurveContext {
    pub source: OpenFxParameterQueryMode,
    pub kind: c_int,
    pub revision: u64,
    prepared: PreparedPropertyQuery,
    // SDK StringInstance copies the returned text. These immutable allocations
    // survive every callback and stay alive through native instance destruction.
    strings: BTreeMap<String, CString>,
}

impl CurveContext {
    pub fn prepare(
        binding: &OpenFxParameterBinding,
        revision: u64,
    ) -> Result<Self, OpenFxHostError> {
        let OpenFxParameterQueryMode::Curve { property, time_scale } = &binding.query_mode else {
            return Err(invalid("curve snapshot is required"));
        };
        if !property.is_enabled() || !property.is_animated() {
            return Err(invalid("curve snapshot has no enabled animation"));
        }
        let kind = binding.value.native_fields().0;
        let prepared = PreparedPropertyQuery::prepare(property, *time_scale)
            .map_err(|error| invalid(error.to_string()))?;
        convert(property.static_value().clone(), kind)
            .map_err(|_| invalid("curve property does not match the SDK parameter kind"))?;
        let strings = if kind == 5 {
            property
                .descriptor
                .schema
                .enum_options
                .iter()
                .map(|option| {
                    if option.key.is_empty()
                        || option.key.len() > 256
                        || option.key.chars().any(char::is_control)
                    {
                        return Err(invalid("curve StrChoice key is invalid"));
                    }
                    Ok((
                        option.key.clone(),
                        CString::new(option.key.as_str())
                            .map_err(|_| invalid("curve StrChoice key contains null"))?,
                    ))
                })
                .collect::<Result<BTreeMap<_, _>, _>>()?
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            source: binding.query_mode.clone(),
            kind,
            revision,
            prepared,
            strings,
        })
    }

    pub fn value_at(&self, frame: f64) -> Result<OpenFxParameterValue, OpenFxHostError> {
        self.value(frame)
            .map_err(|status| invalid(format!("curve query failed with SDK status {status}")))
    }

    fn value(&self, frame: f64) -> Result<OpenFxParameterValue, c_int> {
        let time = AutomationQueryTime::new(frame).map_err(query_error)?;
        convert(
            self.prepared.evaluate(time).map_err(query_error)?,
            self.kind,
        )
    }

    fn answer(
        &self,
        operation: c_int,
        start: f64,
        end: f64,
        index: c_int,
        direction: c_int,
    ) -> Result<(c_int, NativeQueryResult), c_int> {
        let mut result = NativeQueryResult::empty();
        match operation {
            0 => {
                let value = self.value(start)?;
                result.value = value.native_fields().1;
                if let OpenFxParameterValue::StrChoice(key) = value {
                    let text = self.strings.get(&key).ok_or(11)?;
                    result.string_value = text.as_ptr();
                    result.string_length = text.as_bytes().len();
                }
            }
            1 | 2 => {
                let start = AutomationQueryTime::new(start).map_err(query_error)?;
                let values = if operation == 1 {
                    self.prepared.derivatives(start)
                } else {
                    self.prepared
                        .integrals(start, AutomationQueryTime::new(end).map_err(query_error)?)
                }
                .map_err(query_error)?;
                if values.len() > 4 {
                    return Err(11);
                }
                result.value[..values.len()].copy_from_slice(&values);
            }
            3 => result.key_count = self.prepared.key_times().len().try_into().map_err(|_| 11)?,
            4 => {
                let index = usize::try_from(index).map_err(|_| 10)?;
                result.key_time = *self.prepared.key_times().get(index).ok_or(10)?;
            }
            5 => {
                AutomationQueryTime::new(start).map_err(query_error)?;
                let keys = self.prepared.key_times();
                let found = if direction < 0 {
                    keys.partition_point(|key| *key < start).checked_sub(1)
                } else if direction > 0 {
                    let position = keys.partition_point(|key| *key <= start);
                    (position < keys.len()).then_some(position)
                } else {
                    keys.binary_search_by(|key| {
                        if *key == start {
                            std::cmp::Ordering::Equal
                        } else {
                            key.total_cmp(&start)
                        }
                    })
                    .ok()
                };
                let Some(found) = found else {
                    return Ok((1, result));
                };
                result.key_index = found.try_into().map_err(|_| 11)?;
            }
            _ => return Err(5),
        }
        Ok((0, result))
    }
}

fn query_error(error: AutomationQueryError) -> c_int {
    if error == AutomationQueryError::UnsupportedCalculus {
        5
    } else {
        11
    }
}

fn convert(value: PropertyValue, kind: c_int) -> Result<OpenFxParameterValue, c_int> {
    let value = match (kind, value) {
        (1, PropertyValue::Double(value)) => OpenFxParameterValue::Double(value),
        (2, PropertyValue::Bool(value)) => OpenFxParameterValue::Boolean(value),
        (3, PropertyValue::Int(value)) => {
            OpenFxParameterValue::Integer(value.try_into().map_err(|_| 11)?)
        }
        (4, PropertyValue::Enum(key)) => OpenFxParameterValue::Choice(key.parse().map_err(|_| 11)?),
        (5, PropertyValue::Enum(key)) => OpenFxParameterValue::StrChoice(key),
        (7, PropertyValue::DoubleVec2(value)) => OpenFxParameterValue::Double2(value),
        (8, PropertyValue::DoubleVec3(value)) => OpenFxParameterValue::Double3(value),
        (9, PropertyValue::DoubleRgb(value)) => OpenFxParameterValue::Rgb(value),
        (10, PropertyValue::DoubleColor(value)) => OpenFxParameterValue::Rgba(value),
        (11, PropertyValue::IntVec2(value)) => OpenFxParameterValue::Integer2(value),
        (12, PropertyValue::IntVec3(value)) => OpenFxParameterValue::Integer3(value),
        _ => return Err(11),
    };
    if value.native_fields().1.iter().any(|channel| !channel.is_finite()) {
        return Err(11);
    }
    Ok(value)
}

/// Native context pointers are valid while the worker owns their Arc snapshot.
/// A panic cannot unwind into C++, and failed queries leave the output untouched.
pub(super) unsafe extern "C" fn query(
    context: *const c_void,
    operation: c_int,
    start: f64,
    end: f64,
    index: c_int,
    direction: c_int,
    output: *mut NativeQueryResult,
) -> c_int {
    if context.is_null() || output.is_null() {
        return 9;
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: the worker retains this immutable context through teardown.
        let context = unsafe { &*context.cast::<CurveContext>() };
        context.answer(operation, start, end, index, direction)
    }));
    match result {
        Ok(Ok((status, result))) => {
            // SAFETY: native callers supply a valid local result buffer.
            unsafe {
                output.write(result);
            }
            status
        }
        Ok(Err(status)) => status,
        Err(_) => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{
        AnimatedProperty, ExactBezierHandle, InterpolationType, KeyframeInterpolation,
        PropertyDescriptor, TimeScale, TimelineTime,
    };

    fn cubic() -> CurveContext {
        let mut property = AnimatedProperty::from_descriptor(PropertyDescriptor::new(
            "curve",
            "Curve",
            PropertyValue::Double(0.0),
        ));
        property.set_animation_enabled(true);
        for (time, value) in [(TimelineTime::ZERO, 0.0), (TimelineTime::ONE, 1.0)] {
            property
                .write_channels(time, &[(0, value)], InterpolationType::Linear)
                .expect("key");
        }
        property
            .update_channel_keyframe_handles(
                TimelineTime::ZERO,
                0,
                KeyframeInterpolation::Linear,
                KeyframeInterpolation::Bezier(ExactBezierHandle {
                    time_offset: TimelineTime::new(1, 3).expect("out"),
                    value_offset: 0.0,
                }),
            )
            .expect("outgoing");
        property
            .update_channel_keyframe_handles(
                TimelineTime::ONE,
                0,
                KeyframeInterpolation::Bezier(ExactBezierHandle {
                    time_offset: TimelineTime::new(-1, 3).expect("in"),
                    value_offset: 0.0,
                }),
                KeyframeInterpolation::Linear,
            )
            .expect("incoming");
        CurveContext::prepare(
            &OpenFxParameterBinding::curve(
                OpenFxParameterValue::Double(900.0),
                property,
                TimeScale::new(24, 1).expect("frame grid"),
            ),
            1,
        )
        .expect("snapshot")
    }

    #[test]
    fn native_callback_uses_canonical_curve_on_the_frame_grid() {
        let context = cubic();
        assert_eq!(
            context.value_at(6.0).expect("current value"),
            OpenFxParameterValue::Double(0.15625)
        );
        for (operation, start, end, expected) in [
            (0, 6.0, 0.0, 0.15625),
            (1, 6.0, 0.0, 1.125 / 24.0),
            (2, 0.0, 24.0, 12.0),
            (2, 24.0, 0.0, -12.0),
        ] {
            let mut result = NativeQueryResult::empty();
            // SAFETY: local immutable context and result live for the entire call.
            assert_eq!(
                unsafe {
                    query(
                        (&context as *const CurveContext).cast(),
                        operation,
                        start,
                        end,
                        0,
                        0,
                        &mut result,
                    )
                },
                0
            );
            if operation == 2 {
                assert!(
                    (result.value[0] - expected).abs()
                        <= 4.0 * f64::EPSILON * expected.abs().max(1.0)
                );
            } else {
                assert_eq!(result.value[0], expected);
            }
            assert_eq!(&result.value[1..], &[0.0; 3]);
        }
    }

    #[test]
    fn native_key_queries_preserve_signed_zero_and_failure_outputs() {
        let context = cubic();
        assert_eq!(
            context.answer(3, 0.0, 0.0, 0, 0).expect("count").1.key_count,
            2
        );
        for at in [0.0, -0.0] {
            assert_eq!(
                context.answer(5, at, 0.0, 0, 0).expect("first").1.key_index,
                0
            );
        }
        assert_eq!(
            context.answer(5, 0.0, 0.0, 0, 1).expect("after").1.key_index,
            1
        );
        let (status, result) = context.answer(5, 0.0, 0.0, 0, -1).expect("no earlier key");
        assert_eq!((status, result.key_index), (1, -1));
        for (operation, time, index, expected) in [
            (0, f64::NAN, 0, 11),
            (1, f64::INFINITY, 0, 11),
            (4, 0.0, -1, 10),
            (4, 0.0, 2, 10),
        ] {
            let mut result = NativeQueryResult::empty();
            result.value = [-999.0; 4];
            result.key_time = -999.0;
            // SAFETY: local immutable context and result live for the entire call.
            assert_eq!(
                unsafe {
                    query(
                        (&context as *const CurveContext).cast(),
                        operation,
                        time,
                        0.0,
                        index,
                        0,
                        &mut result,
                    )
                },
                expected
            );
            assert_eq!(result.value, [-999.0; 4]);
            assert_eq!(result.key_time, -999.0);
        }
    }

    #[test]
    fn binding_wire_rejects_unknown_mode_fields() {
        let binding = serde_json::json!({"value":{"kind":"double","value":1.0},
            "query_mode":{"mode":"constant","extra":0}});
        assert!(serde_json::from_value::<OpenFxParameterBinding>(binding).is_err());
    }
}
