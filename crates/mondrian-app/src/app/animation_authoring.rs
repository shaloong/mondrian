//! Stable-identity authoring for Clip-owned numeric automation.
//!
//! UI Adapters submit one normalized control-point intent. This module resolves
//! the current property path and keyframe time from stable identities, prepares
//! the smallest exact mutation set, and commits it through one Author
//! Transaction. Widgets never replace an entire curve or infer author identity
//! from point order.

use super::{AppState, SelectedClipRef};
use mondrian_core::automation::{
    AnimationParameterAddress, InterpolationType, Keyframe, PropertyHost, PropertyMutation,
    PropertyValue, PropertyValueType,
};
use mondrian_core::{KeyframeId, MondrianError, Result, TimeScale, TimelineTime};

const CURVE_QUANTIZATION_DENOMINATOR: u32 = 1_000_000;

/// One normalized point in an owner-local curve viewport.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormalizedCurvePoint {
    /// Relative position across the visible Clip author span.
    pub time_ratio: f64,
    /// Relative value across the Parameter Schema soft range.
    pub value_ratio: f64,
}

impl NormalizedCurvePoint {
    /// Construct a finite point inside the inclusive unit square.
    pub fn new(time_ratio: f64, value_ratio: f64) -> Result<Self> {
        if !time_ratio.is_finite()
            || !value_ratio.is_finite()
            || !(0.0..=1.0).contains(&time_ratio)
            || !(0.0..=1.0).contains(&value_ratio)
        {
            return Err(curve_error(
                "normalized curve coordinates must be finite values inside 0..=1",
            ));
        }
        Ok(Self { time_ratio, value_ratio })
    }
}

/// One stable-identity curve edit requested by a product Adapter.
#[derive(Debug, Clone, PartialEq)]
pub enum ClipNumericCurveEdit {
    /// Insert a key, or edit the key with the supplied stable identity.
    Upsert {
        /// Existing identity for a move/value edit; `None` inserts at the
        /// requested time or updates the existing key already at that time.
        keyframe_id: Option<KeyframeId>,
        /// New normalized time and value.
        point: NormalizedCurvePoint,
    },
    /// Remove one existing key by stable identity.
    Remove { keyframe_id: KeyframeId },
}

/// Result of one curve authoring request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipNumericCurveEditOutcome {
    /// Whether an Author Transaction committed.
    pub changed: bool,
    /// Stable identity created, edited, or removed.
    pub keyframe_id: KeyframeId,
}

#[derive(Debug)]
struct PreparedCurveEdit {
    mutations: Vec<PropertyMutation>,
    keyframe_id: KeyframeId,
}

impl AppState {
    /// Apply one normalized numeric curve edit through stable author identities.
    ///
    /// The current property path and key time are resolved from the canonical
    /// Sequence snapshot. A stale or ambiguous identity fails before the Author
    /// Transaction begins. Existing interpolation, temporal flags, and
    /// Keyframe ID survive time/value edits.
    pub fn edit_clip_numeric_curve(
        &mut self,
        selection: SelectedClipRef,
        address: AnimationParameterAddress,
        edit: ClipNumericCurveEdit,
    ) -> Result<ClipNumericCurveEditOutcome> {
        let clip = self.clip_snapshot(selection).ok_or_else(|| MondrianError::ClipNotFound {
            clip_id: selection.clip_id.to_string(),
        })?;
        let prepared = prepare_curve_edit(&clip, &address, edit)?;
        if prepared.mutations.is_empty() {
            return Ok(ClipNumericCurveEditOutcome {
                changed: false,
                keyframe_id: prepared.keyframe_id,
            });
        }

        self.mutate_clip_properties(selection, prepared.mutations, "调整关键帧")?;
        Ok(ClipNumericCurveEditOutcome { changed: true, keyframe_id: prepared.keyframe_id })
    }
}

fn prepare_curve_edit(
    clip: &mondrian_timeline::Clip,
    address: &AnimationParameterAddress,
    edit: ClipNumericCurveEdit,
) -> Result<PreparedCurveEdit> {
    let properties = clip.property_bag()?;
    let (path, property) = properties.property_by_address(address).ok_or_else(|| {
        curve_error(format!(
            "animation parameter {} / {} is not owned by Clip {}",
            address.animation_track_id, address.parameter_id, clip.id
        ))
    })?;
    if !property.descriptor.schema.is_animatable || property.channel_count() != 1 {
        return Err(curve_error(format!(
            "parameter {} is not a single-channel animatable curve",
            address.parameter_id
        )));
    }
    let value_type = property.value_type();
    if !matches!(
        value_type,
        PropertyValueType::Float | PropertyValueType::Double
    ) {
        return Err(curve_error(format!(
            "parameter {} is not a floating-point curve",
            address.parameter_id
        )));
    }
    let numeric = property.descriptor.schema.numeric.ok_or_else(|| {
        curve_error(format!(
            "parameter {} has no finite numeric editor range",
            address.parameter_id
        ))
    })?;
    let value_span = numeric.soft_range.max - numeric.soft_range.min;
    if !value_span.is_finite() || value_span <= 0.0 {
        return Err(curve_error(format!(
            "parameter {} has an unusable numeric editor range",
            address.parameter_id
        )));
    }

    match edit {
        ClipNumericCurveEdit::Remove { keyframe_id } => {
            let keyframe = property.keyframe_by_id(keyframe_id).ok_or_else(|| {
                curve_error(format!(
                    "keyframe {keyframe_id} is stale or is not a complete key of parameter {}",
                    address.parameter_id
                ))
            })?;
            Ok(PreparedCurveEdit {
                mutations: vec![PropertyMutation::RemoveKeyframe {
                    path: path.to_owned(),
                    time: keyframe.time,
                }],
                keyframe_id,
            })
        }
        ClipNumericCurveEdit::Upsert { keyframe_id, point } => {
            let target_time = curve_time(clip, point.time_ratio)?;
            let target_value = curve_value(
                value_type,
                numeric.soft_range.min + value_span * point.value_ratio,
            )?;

            let existing = if let Some(keyframe_id) = keyframe_id {
                let keyframe = property.keyframe_by_id(keyframe_id).ok_or_else(|| {
                    curve_error(format!(
                        "keyframe {keyframe_id} is stale or is not a complete key of parameter {}",
                        address.parameter_id
                    ))
                })?;
                if property
                    .keyframe_at(target_time)
                    .is_some_and(|candidate| candidate.id != keyframe_id)
                {
                    return Err(curve_error(format!(
                        "keyframe {keyframe_id} cannot move onto an occupied author time"
                    )));
                }
                Some(keyframe)
            } else {
                property.keyframe_at(target_time)
            };

            if let Some(keyframe) = existing {
                if keyframe.time == target_time && keyframe.value == target_value {
                    return Ok(PreparedCurveEdit {
                        mutations: Vec::new(),
                        keyframe_id: keyframe.id,
                    });
                }
                return Ok(PreparedCurveEdit {
                    mutations: vec![PropertyMutation::EditKeyframe {
                        path: path.to_owned(),
                        keyframe_id: keyframe.id,
                        time: target_time,
                        value: target_value,
                    }],
                    keyframe_id: keyframe.id,
                });
            }

            let keyframe =
                Keyframe::from_preset(target_time, target_value, InterpolationType::Linear);
            let keyframe_id = keyframe.id;
            Ok(PreparedCurveEdit {
                mutations: vec![PropertyMutation::SetKeyframe { path: path.to_owned(), keyframe }],
                keyframe_id,
            })
        }
    }
}

fn curve_time(clip: &mondrian_timeline::Clip, ratio: f64) -> Result<TimelineTime> {
    let ratio = TimelineTime::from_f64_quantized(ratio, CURVE_QUANTIZATION_DENOMINATOR)?;
    let scale = TimeScale::try_from(ratio)?;
    let duration = clip.clip_time_out()?.checked_sub(clip.clip_time_in)?;
    clip.clip_time_in
        .checked_add(duration.checked_scale(scale)?)
        .map_err(Into::into)
}

fn curve_value(value_type: PropertyValueType, value: f64) -> Result<PropertyValue> {
    if !value.is_finite() {
        return Err(curve_error(
            "curve value mapping produced a non-finite number",
        ));
    }
    match value_type {
        PropertyValueType::Float => Ok(PropertyValue::Float(value as f32)),
        PropertyValueType::Double => Ok(PropertyValue::Double(value)),
        _ => Err(curve_error("curve value mapping requires Float or Double")),
    }
}

fn curve_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "edit_clip_numeric_curve".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::automation::{BezierHandle, KeyframeInterpolation, KeyframeTemporalFlags};
    use mondrian_core::{AssetId, Rational};
    use mondrian_timeline::clip::Transform2D;
    use mondrian_timeline::{Clip, Sequence};

    fn tt(frame: i64, time_base: Rational) -> TimelineTime {
        TimelineTime::new(
            frame.checked_mul(time_base.num).expect("test time fits"),
            time_base.den,
        )
        .expect("valid test time")
    }

    fn state_with_clip() -> (AppState, SelectedClipRef, Rational) {
        let mut sequence = Sequence::new("curve authoring");
        let time_base = sequence.time_base();
        let track_id = sequence.video_tracks[0].id;
        let clip = Clip::new(AssetId::new(), TimelineTime::ZERO, tt(20, time_base)).expect("clip");
        let clip_id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("add clip");
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        (
            state,
            SelectedClipRef { track_id, is_video_track: true, clip_id },
            time_base,
        )
    }

    fn opacity_address(state: &AppState, selection: SelectedClipRef) -> AnimationParameterAddress {
        state
            .clip_snapshot(selection)
            .and_then(|clip| clip.property_bag().ok())
            .and_then(|bag| bag.address_for_path(Transform2D::OPACITY_PATH))
            .expect("opacity address")
    }

    fn opacity_key(
        state: &AppState,
        selection: SelectedClipRef,
        keyframe_id: KeyframeId,
    ) -> Keyframe<PropertyValue> {
        state
            .clip_snapshot(selection)
            .and_then(|clip| clip.property_bag().ok())
            .and_then(|bag| {
                bag.property(Transform2D::OPACITY_PATH)
                    .and_then(|property| property.keyframe_by_id(keyframe_id))
            })
            .expect("opacity key")
    }

    #[test]
    fn curve_edit_commits_once_and_preserves_manual_bezier_author_identity() {
        let (mut state, selection, time_base) = state_with_clip();
        let start = Keyframe::linear(tt(0, time_base), PropertyValue::Float(0.0));
        let middle_id = KeyframeId::new();
        let middle = Keyframe {
            id: middle_id,
            time: tt(10, time_base),
            value: PropertyValue::Float(0.5),
            interp_in: KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: TimelineTime::from_f64_quantized(-0.2, 1_000_000).expect("handle"),
                value_offset: -0.1,
            }),
            interp_out: KeyframeInterpolation::Bezier(BezierHandle {
                time_offset: TimelineTime::from_f64_quantized(0.3, 1_000_000).expect("handle"),
                value_offset: 0.2,
            }),
            temporal_flags: KeyframeTemporalFlags {
                auto_bezier: false,
                continuous: false,
                broken_handles: true,
            },
        };
        let end = Keyframe::linear(tt(20, time_base), PropertyValue::Float(1.0));
        for keyframe in [start, middle.clone(), end] {
            state
                .mutate_clip_property(
                    selection,
                    PropertyMutation::SetKeyframe {
                        path: Transform2D::OPACITY_PATH.to_owned(),
                        keyframe,
                    },
                    "seed curve",
                )
                .expect("seed curve");
        }
        let address = opacity_address(&state, selection);
        let generation_before = state.project_author_generation();
        let revision_before = state.active_sequence().expect("sequence").revision.get();

        let outcome = state
            .edit_clip_numeric_curve(
                selection,
                address,
                ClipNumericCurveEdit::Upsert {
                    keyframe_id: Some(middle_id),
                    point: NormalizedCurvePoint::new(0.6, 0.75).expect("point"),
                },
            )
            .expect("edit curve");

        assert!(outcome.changed);
        assert_eq!(outcome.keyframe_id, middle_id);
        assert_eq!(state.project_author_generation(), generation_before + 1);
        assert_eq!(
            state.active_sequence().expect("sequence").revision.get(),
            revision_before + 1
        );
        let edited = opacity_key(&state, selection, middle_id);
        assert_eq!(edited.time, tt(12, time_base));
        assert_eq!(edited.value, PropertyValue::Float(0.75));
        assert_eq!(edited.interp_in, middle.interp_in);
        assert_eq!(edited.interp_out, middle.interp_out);
        assert_eq!(edited.temporal_flags, middle.temporal_flags);

        assert!(state.undo_timeline().expect("undo"));
        let restored = opacity_key(&state, selection, middle_id);
        assert_eq!(restored.time, middle.time);
        assert_eq!(restored.value, middle.value);
        assert_eq!(restored.interp_in, middle.interp_in);
        assert_eq!(restored.interp_out, middle.interp_out);
    }

    #[test]
    fn stale_or_colliding_curve_identity_rejects_without_author_commit() {
        let (mut state, selection, time_base) = state_with_clip();
        let first = Keyframe::linear(tt(5, time_base), PropertyValue::Float(0.25));
        let first_id = first.id;
        let second = Keyframe::linear(tt(10, time_base), PropertyValue::Float(0.75));
        for keyframe in [first, second] {
            state
                .mutate_clip_property(
                    selection,
                    PropertyMutation::SetKeyframe {
                        path: Transform2D::OPACITY_PATH.to_owned(),
                        keyframe,
                    },
                    "seed curve",
                )
                .expect("seed curve");
        }
        let address = opacity_address(&state, selection);
        let generation_before = state.project_author_generation();
        let revision_before = state.active_sequence().expect("sequence").revision;
        let before = state
            .clip_snapshot(selection)
            .and_then(|clip| clip.property_bag().ok())
            .and_then(|bag| bag.property(Transform2D::OPACITY_PATH).cloned())
            .expect("opacity before");

        state
            .edit_clip_numeric_curve(
                selection,
                address.clone(),
                ClipNumericCurveEdit::Upsert {
                    keyframe_id: Some(first_id),
                    point: NormalizedCurvePoint::new(0.5, 0.5).expect("point"),
                },
            )
            .expect_err("occupied time rejects");
        state
            .edit_clip_numeric_curve(
                selection,
                address,
                ClipNumericCurveEdit::Remove { keyframe_id: KeyframeId::new() },
            )
            .expect_err("stale identity rejects");

        assert_eq!(state.project_author_generation(), generation_before);
        assert_eq!(
            state.active_sequence().expect("sequence").revision,
            revision_before
        );
        assert_eq!(
            state
                .clip_snapshot(selection)
                .and_then(|clip| clip.property_bag().ok())
                .and_then(|bag| bag.property(Transform2D::OPACITY_PATH).cloned())
                .expect("opacity after"),
            before
        );
    }

    #[test]
    fn curve_insert_and_remove_use_the_same_stable_key_identity() {
        let (mut state, selection, _time_base) = state_with_clip();
        let address = opacity_address(&state, selection);

        let inserted = state
            .edit_clip_numeric_curve(
                selection,
                address.clone(),
                ClipNumericCurveEdit::Upsert {
                    keyframe_id: None,
                    point: NormalizedCurvePoint::new(0.25, 0.4).expect("point"),
                },
            )
            .expect("insert");
        assert!(inserted.changed);
        let key = opacity_key(&state, selection, inserted.keyframe_id);
        assert_eq!(key.value, PropertyValue::Float(0.4));

        let removed = state
            .edit_clip_numeric_curve(
                selection,
                address,
                ClipNumericCurveEdit::Remove { keyframe_id: inserted.keyframe_id },
            )
            .expect("remove");
        assert!(removed.changed);
        assert_eq!(removed.keyframe_id, inserted.keyframe_id);
        assert!(state
            .clip_snapshot(selection)
            .and_then(|clip| clip.property_bag().ok())
            .and_then(|bag| bag.property(Transform2D::OPACITY_PATH).cloned())
            .and_then(|property| property.keyframe_by_id(inserted.keyframe_id))
            .is_none());
    }
}
