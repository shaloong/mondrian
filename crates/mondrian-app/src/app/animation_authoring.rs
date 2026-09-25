//! Stable-identity authoring for Clip-owned numeric automation.
//!
//! UI Adapters submit one normalized control-point intent. This module resolves
//! the current property path and keyframe time from stable identities, prepares
//! the smallest exact mutation set, and commits it through one Author
//! Transaction. Widgets never replace an entire curve or infer author identity
//! from point order.

use super::{AppState, SelectedClipRef};
use mondrian_core::automation::{
    AnimatedProperty, AnimationParameterAddress, InterpolationType, Keyframe,
    ParameterInterpolation, PropertyBag, PropertyHost, PropertyMutation, PropertyValue,
    PropertyValueType,
};
use mondrian_core::{EffectId, KeyframeId, MondrianError, Result, TimeScale, TimelineTime};

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
    /// Change one complete key's interpolation using its stable identity.
    SetInterpolation {
        keyframe_id: KeyframeId,
        interpolation: InterpolationType,
    },
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
        clip_id: mondrian_core::ClipId,
        address: AnimationParameterAddress,
        edit: ClipNumericCurveEdit,
    ) -> Result<ClipNumericCurveEditOutcome> {
        let (selection, clip) = numeric_curve_target(self, clip_id)?;
        let prepared = prepare_curve_edit(&clip.intrinsic_parameter_bag(), clip, &address, edit)?;
        if prepared.mutations.is_empty() {
            return Ok(ClipNumericCurveEditOutcome {
                changed: false,
                keyframe_id: prepared.keyframe_id,
            });
        }

        self.mutate_clip_properties(selection, prepared.mutations, "调整关键帧")?;
        Ok(ClipNumericCurveEditOutcome { changed: true, keyframe_id: prepared.keyframe_id })
    }

    /// Return whether one stable-identity numeric curve edit would commit.
    pub fn clip_numeric_curve_edit_would_change(
        &self,
        clip_id: mondrian_core::ClipId,
        address: &AnimationParameterAddress,
        edit: ClipNumericCurveEdit,
    ) -> Result<bool> {
        let (_, clip) = numeric_curve_target(self, clip_id)?;
        Ok(
            !prepare_curve_edit(&clip.intrinsic_parameter_bag(), clip, address, edit)?
                .mutations
                .is_empty(),
        )
    }

    /// Edit one floating-point visual Effect curve through its stable instance address.
    pub fn edit_effect_numeric_curve(
        &mut self,
        clip_id: mondrian_core::ClipId,
        effect_id: EffectId,
        address: AnimationParameterAddress,
        edit: ClipNumericCurveEdit,
    ) -> Result<ClipNumericCurveEditOutcome> {
        let (selection, clip) = numeric_curve_target(self, clip_id)?;
        let effect =
            clip.effects.iter().find(|effect| effect.id == effect_id).ok_or_else(|| {
                curve_error(format!(
                    "Effect {effect_id} does not belong to Clip {clip_id}"
                ))
            })?;
        let prepared = prepare_curve_edit(&effect.properties, clip, &address, edit)?;
        if prepared.mutations.is_empty() {
            return Ok(ClipNumericCurveEditOutcome {
                changed: false,
                keyframe_id: prepared.keyframe_id,
            });
        }
        let sequence_id =
            self.active_sequence_id().ok_or_else(|| curve_error("no active Sequence"))?;
        self.commit_sequence_edit(sequence_id, "调整特效关键帧", |sequence| {
            let clip = sequence
                .find_clip_mut(selection.clip_id)
                .ok_or_else(|| MondrianError::ClipNotFound { clip_id: clip_id.to_string() })?;
            let effect =
                clip.effects.iter_mut().find(|effect| effect.id == effect_id).ok_or_else(|| {
                    curve_error(format!(
                        "Effect {effect_id} does not belong to Clip {clip_id}"
                    ))
                })?;
            for mutation in prepared.mutations {
                effect.apply_property_mutation(mutation)?;
            }
            Ok(sequence.id)
        })?;
        Ok(ClipNumericCurveEditOutcome { changed: true, keyframe_id: prepared.keyframe_id })
    }

    /// Return whether an Effect curve edit would commit, including lock and stale-ID checks.
    pub fn effect_numeric_curve_edit_would_change(
        &self,
        clip_id: mondrian_core::ClipId,
        effect_id: EffectId,
        address: &AnimationParameterAddress,
        edit: ClipNumericCurveEdit,
    ) -> Result<bool> {
        let (_, clip) = numeric_curve_target(self, clip_id)?;
        let effect =
            clip.effects.iter().find(|effect| effect.id == effect_id).ok_or_else(|| {
                curve_error(format!(
                    "Effect {effect_id} does not belong to Clip {clip_id}"
                ))
            })?;
        Ok(
            !prepare_curve_edit(&effect.properties, clip, address, edit)?
                .mutations
                .is_empty(),
        )
    }

    /// Whether the current exact Effect key can be toggled without changing author state.
    pub fn effect_current_key_toggle_available(
        &self,
        clip_id: mondrian_core::ClipId,
        effect_id: EffectId,
        address: &AnimationParameterAddress,
    ) -> bool {
        let Ok((_, clip)) = numeric_curve_target(self, clip_id) else {
            return false;
        };
        let Some(effect) = clip.effects.iter().find(|effect| effect.id == effect_id) else {
            return false;
        };
        let Some((_, property)) = effect.properties.property_by_address(address) else {
            return false;
        };
        property.descriptor.schema.is_animatable
            && property.channel_count() == 1
            && matches!(
                property.value_type(),
                PropertyValueType::Float | PropertyValueType::Double
            )
            && property.descriptor.schema.numeric.is_some()
            && !property.descriptor.schema.allowed_interpolations.is_empty()
    }

    /// Toggle a key at the exact current Clip-local time, preserving its evaluated value.
    pub fn toggle_effect_current_key(
        &mut self,
        clip_id: mondrian_core::ClipId,
        effect_id: EffectId,
        address: AnimationParameterAddress,
    ) -> Result<bool> {
        let (selection, clip) = numeric_curve_target(self, clip_id)?;
        let sequence = self.active_sequence().ok_or_else(|| curve_error("no active Sequence"))?;
        let sequence_id = sequence.id;
        let time = clip.clamped_visual_author_time(
            self.current_timeline_time()?.unwrap_or(sequence.playhead),
        )?;
        let effect =
            clip.effects.iter().find(|effect| effect.id == effect_id).ok_or_else(|| {
                curve_error(format!(
                    "Effect {effect_id} does not belong to Clip {clip_id}"
                ))
            })?;
        let (path, property) =
            effect.properties.property_by_address(&address).ok_or_else(|| {
                curve_error(format!(
                    "parameter {} is stale or not owned by Effect {effect_id}",
                    address.parameter_id
                ))
            })?;
        if !property.descriptor.schema.is_animatable
            || property.channel_count() != 1
            || !matches!(
                property.value_type(),
                PropertyValueType::Float | PropertyValueType::Double
            )
            || property.descriptor.schema.numeric.is_none()
        {
            return Err(curve_error(
                "Effect parameter is not an animatable numeric scalar",
            ));
        }
        let mutation = if property.keyframe_at(time).is_some() {
            if property.keyframe_times().len() == 1 {
                PropertyMutation::ClearAnimation { path: path.to_owned(), time }
            } else {
                PropertyMutation::RemoveKeyframe { path: path.to_owned(), time }
            }
        } else {
            PropertyMutation::SetKeyframe {
                path: path.to_owned(),
                keyframe: Keyframe::from_preset(
                    time,
                    property.evaluate(time),
                    default_curve_interpolation(property)?,
                ),
            }
        };
        self.commit_sequence_edit(sequence_id, "切换特效关键帧", |sequence| {
            let clip = sequence
                .find_clip_mut(selection.clip_id)
                .ok_or_else(|| MondrianError::ClipNotFound { clip_id: clip_id.to_string() })?;
            let effect =
                clip.effects.iter_mut().find(|effect| effect.id == effect_id).ok_or_else(|| {
                    curve_error(format!(
                        "Effect {effect_id} does not belong to Clip {clip_id}"
                    ))
                })?;
            effect.apply_property_mutation(mutation)?;
            Ok(sequence.id)
        })?;
        self.set_active_animation_property(clip_id, address);
        Ok(true)
    }
}

fn numeric_curve_target(
    state: &AppState,
    clip_id: mondrian_core::ClipId,
) -> Result<(SelectedClipRef, &mondrian_timeline::Clip)> {
    let sequence = state.active_sequence().ok_or_else(|| curve_error("no active Sequence"))?;
    let Some(location) = sequence.clip_track_location(clip_id) else {
        return Err(MondrianError::ClipNotFound { clip_id: clip_id.to_string() });
    };
    if !location.is_video_track {
        return Err(curve_error("visual Clip automation requires a video Clip"));
    }
    if location.is_locked {
        return Err(MondrianError::TrackLocked { track_id: location.track_id.to_string() });
    }
    let clip = sequence
        .find_clip(clip_id)
        .ok_or_else(|| MondrianError::ClipNotFound { clip_id: clip_id.to_string() })?;
    Ok((
        SelectedClipRef {
            track_id: location.track_id,
            is_video_track: location.is_video_track,
            clip_id,
        },
        clip,
    ))
}

fn prepare_curve_edit(
    properties: &PropertyBag,
    clip: &mondrian_timeline::Clip,
    address: &AnimationParameterAddress,
    edit: ClipNumericCurveEdit,
) -> Result<PreparedCurveEdit> {
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
        ClipNumericCurveEdit::SetInterpolation { keyframe_id, interpolation } => {
            if !property
                .descriptor
                .schema
                .allowed_interpolations
                .contains(&schema_interpolation(interpolation))
            {
                return Err(curve_error(format!(
                    "parameter {} does not allow {interpolation:?}",
                    address.parameter_id
                )));
            }
            let keyframe = property.keyframe_by_id(keyframe_id).ok_or_else(|| {
                curve_error(format!(
                    "keyframe {keyframe_id} is stale or is not a complete key of parameter {}",
                    address.parameter_id
                ))
            })?;
            let unchanged = match interpolation {
                InterpolationType::Hold => {
                    matches!(
                        keyframe.interp_in,
                        mondrian_core::automation::KeyframeInterpolation::Hold
                    ) && matches!(
                        keyframe.interp_out,
                        mondrian_core::automation::KeyframeInterpolation::Hold
                    )
                }
                InterpolationType::Linear => {
                    matches!(
                        keyframe.interp_in,
                        mondrian_core::automation::KeyframeInterpolation::Linear
                    ) && matches!(
                        keyframe.interp_out,
                        mondrian_core::automation::KeyframeInterpolation::Linear
                    )
                }
                InterpolationType::AutoBezier => keyframe.temporal_flags.auto_bezier,
                InterpolationType::ContinuousBezier => {
                    keyframe.temporal_flags.continuous
                        && !keyframe.temporal_flags.auto_bezier
                        && !keyframe.temporal_flags.broken_handles
                }
                InterpolationType::Bezier => {
                    !keyframe.temporal_flags.auto_bezier
                        && !keyframe.temporal_flags.continuous
                        && matches!(
                            keyframe.interp_in,
                            mondrian_core::automation::KeyframeInterpolation::Bezier(_)
                        )
                        && matches!(
                            keyframe.interp_out,
                            mondrian_core::automation::KeyframeInterpolation::Bezier(_)
                        )
                }
                InterpolationType::EaseIn => matches!(
                    (keyframe.interp_in, keyframe.interp_out),
                    (
                        mondrian_core::automation::KeyframeInterpolation::Linear,
                        mondrian_core::automation::KeyframeInterpolation::Bezier(_)
                    )
                ),
                InterpolationType::EaseOut => matches!(
                    (keyframe.interp_in, keyframe.interp_out),
                    (
                        mondrian_core::automation::KeyframeInterpolation::Bezier(_),
                        mondrian_core::automation::KeyframeInterpolation::Linear
                    )
                ),
            };
            Ok(PreparedCurveEdit {
                mutations: if unchanged {
                    Vec::new()
                } else {
                    vec![PropertyMutation::UpdateKeyframeInterpolation {
                        path: path.to_owned(),
                        time: keyframe.time,
                        interpolation,
                    }]
                },
                keyframe_id,
            })
        }
        ClipNumericCurveEdit::Remove { keyframe_id } => {
            let keyframe = property.keyframe_by_id(keyframe_id).ok_or_else(|| {
                curve_error(format!(
                    "keyframe {keyframe_id} is stale or is not a complete key of parameter {}",
                    address.parameter_id
                ))
            })?;
            let mutation = if property.keyframe_times().len() == 1 {
                PropertyMutation::ClearAnimation { path: path.to_owned(), time: keyframe.time }
            } else {
                PropertyMutation::RemoveKeyframe { path: path.to_owned(), time: keyframe.time }
            };
            Ok(PreparedCurveEdit { mutations: vec![mutation], keyframe_id })
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

            let keyframe = Keyframe::from_preset(
                target_time,
                target_value,
                default_curve_interpolation(property)?,
            );
            let keyframe_id = keyframe.id;
            Ok(PreparedCurveEdit {
                mutations: vec![PropertyMutation::SetKeyframe { path: path.to_owned(), keyframe }],
                keyframe_id,
            })
        }
    }
}

fn schema_interpolation(interpolation: InterpolationType) -> ParameterInterpolation {
    match interpolation {
        InterpolationType::Hold => ParameterInterpolation::Hold,
        InterpolationType::Linear => ParameterInterpolation::Linear,
        InterpolationType::Bezier
        | InterpolationType::AutoBezier
        | InterpolationType::ContinuousBezier
        | InterpolationType::EaseIn
        | InterpolationType::EaseOut => ParameterInterpolation::Bezier,
    }
}

fn default_curve_interpolation(property: &AnimatedProperty) -> Result<InterpolationType> {
    let allowed = &property.descriptor.schema.allowed_interpolations;
    if allowed.contains(&ParameterInterpolation::Linear) {
        Ok(InterpolationType::Linear)
    } else {
        allowed
            .first()
            .map(|interpolation| match interpolation {
                ParameterInterpolation::Hold => InterpolationType::Hold,
                ParameterInterpolation::Linear => InterpolationType::Linear,
                ParameterInterpolation::Bezier => InterpolationType::Bezier,
            })
            .ok_or_else(|| curve_error("parameter admits no interpolation"))
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
    use mondrian_core::automation::{
        BezierHandle, KeyframeInterpolation, KeyframeTemporalFlags, PropertyHost,
    };
    use mondrian_core::{AssetId, Rational};
    use mondrian_effects::{EffectNodeExt, EffectType};
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

    fn effect_curve_target(
        state: &mut AppState,
        selection: SelectedClipRef,
    ) -> (EffectId, AnimationParameterAddress) {
        let effect: mondrian_effects::EffectNode =
            EffectNodeExt::with_defaults(EffectType::GaussianBlur);
        let effect_id = effect.id;
        let property = effect
            .properties
            .iter()
            .map(|(_, property)| property)
            .find(|property| {
                property.descriptor.schema.is_animatable
                    && property.channel_count() == 1
                    && matches!(
                        property.value_type(),
                        PropertyValueType::Float | PropertyValueType::Double
                    )
            })
            .expect("scalar effect parameter");
        let address = property.address();
        state
            .active_sequence_mut_uncommitted()
            .expect("sequence")
            .find_clip_mut(selection.clip_id)
            .expect("clip")
            .add_effect_node(effect);
        (effect_id, address)
    }

    fn effect_property(
        state: &AppState,
        selection: SelectedClipRef,
        effect_id: EffectId,
        address: &AnimationParameterAddress,
    ) -> mondrian_core::automation::AnimatedProperty {
        state
            .active_sequence()
            .and_then(|sequence| sequence.find_clip(selection.clip_id))
            .and_then(|clip| clip.effects.iter().find(|effect| effect.id == effect_id))
            .and_then(|effect| effect.properties.property_by_address(address))
            .map(|(_, property)| property.clone())
            .expect("effect property")
    }

    #[test]
    fn effect_curve_supports_stable_keys_constrained_bezier_and_undo() {
        let (mut state, selection, _) = state_with_clip();
        let (effect_id, address) = effect_curve_target(&mut state, selection);
        let before = effect_property(&state, selection, effect_id, &address);
        let inserted = state
            .edit_effect_numeric_curve(
                selection.clip_id,
                effect_id,
                address.clone(),
                ClipNumericCurveEdit::Upsert {
                    keyframe_id: None,
                    point: NormalizedCurvePoint::new(0.5, 0.5).expect("point"),
                },
            )
            .expect("insert key");
        assert!(inserted.changed);
        let id = inserted.keyframe_id;
        assert!(effect_property(&state, selection, effect_id, &address)
            .keyframe_by_id(id)
            .is_some());

        state
            .edit_effect_numeric_curve(
                selection.clip_id,
                effect_id,
                address.clone(),
                ClipNumericCurveEdit::SetInterpolation {
                    keyframe_id: id,
                    interpolation: InterpolationType::ContinuousBezier,
                },
            )
            .expect("continuous bezier");
        let key = effect_property(&state, selection, effect_id, &address)
            .keyframe_by_id(id)
            .expect("key");
        assert!(key.temporal_flags.continuous);
        assert!(!key.temporal_flags.broken_handles);

        state
            .edit_effect_numeric_curve(
                selection.clip_id,
                effect_id,
                address.clone(),
                ClipNumericCurveEdit::SetInterpolation {
                    keyframe_id: id,
                    interpolation: InterpolationType::AutoBezier,
                },
            )
            .expect("auto bezier");
        assert!(
            effect_property(&state, selection, effect_id, &address)
                .keyframe_by_id(id)
                .expect("key")
                .temporal_flags
                .auto_bezier
        );

        assert!(state.undo_timeline().expect("undo auto"));
        assert!(
            effect_property(&state, selection, effect_id, &address)
                .keyframe_by_id(id)
                .expect("key")
                .temporal_flags
                .continuous
        );
        assert!(state.undo_timeline().expect("undo continuous"));
        assert!(state.undo_timeline().expect("undo insertion"));
        assert_eq!(
            effect_property(&state, selection, effect_id, &address),
            before
        );
    }

    #[test]
    fn effect_curve_rejects_stale_and_locked_edits_without_commit() {
        let (mut state, selection, _) = state_with_clip();
        let (effect_id, address) = effect_curve_target(&mut state, selection);
        let before = state.project_author_generation();
        state
            .edit_effect_numeric_curve(
                selection.clip_id,
                effect_id,
                address.clone(),
                ClipNumericCurveEdit::Remove { keyframe_id: KeyframeId::new() },
            )
            .expect_err("stale key");
        state
            .edit_effect_numeric_curve(
                selection.clip_id,
                EffectId::new(),
                address.clone(),
                ClipNumericCurveEdit::Upsert {
                    keyframe_id: None,
                    point: NormalizedCurvePoint::new(0.5, 0.5).expect("point"),
                },
            )
            .expect_err("stale effect");
        assert_eq!(state.project_author_generation(), before);
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].is_locked = true;
        assert!(!state.effect_current_key_toggle_available(selection.clip_id, effect_id, &address));
        state
            .toggle_effect_current_key(selection.clip_id, effect_id, address)
            .expect_err("locked track");
        assert_eq!(state.project_author_generation(), before);
    }

    #[test]
    fn effect_key_toggle_uses_exact_playhead_time_and_restores_original_property() {
        let (mut state, selection, time_base) = state_with_clip();
        let (effect_id, address) = effect_curve_target(&mut state, selection);
        let before = effect_property(&state, selection, effect_id, &address);
        let key_time = tt(7, time_base);
        state.seek(7).expect("seek to author frame");
        assert!(state.effect_current_key_toggle_available(selection.clip_id, effect_id, &address));
        assert!(state
            .toggle_effect_current_key(selection.clip_id, effect_id, address.clone())
            .expect("insert key"));
        let inserted = effect_property(&state, selection, effect_id, &address);
        let key = inserted.keyframe_at(key_time).expect("key at exact playhead");
        assert_eq!(key.value, before.evaluate(key_time));
        assert!(state
            .toggle_effect_current_key(selection.clip_id, effect_id, address.clone())
            .expect("remove key"));
        let after = effect_property(&state, selection, effect_id, &address);
        assert_eq!(after.static_value(), before.static_value());
        assert!(!after.is_enabled());
        assert!(!after.is_animated());
    }

    #[test]
    fn interpolation_preset_edit_uses_stable_key_and_skips_noop() {
        let (mut state, selection, time_base) = state_with_clip();
        let middle = Keyframe::linear(tt(10, time_base), PropertyValue::Float(0.5));
        let keyframe_id = middle.id;
        state
            .mutate_clip_property(
                selection,
                PropertyMutation::SetKeyframe {
                    path: Transform2D::OPACITY_PATH.to_owned(),
                    keyframe: middle,
                },
                "seed curve",
            )
            .expect("seed curve");
        let address = opacity_address(&state, selection);
        let before = state.project_author_generation();
        let outcome = state
            .edit_clip_numeric_curve(
                selection.clip_id,
                address.clone(),
                ClipNumericCurveEdit::SetInterpolation {
                    keyframe_id,
                    interpolation: InterpolationType::AutoBezier,
                },
            )
            .expect("set auto bezier");
        assert!(outcome.changed);
        assert_eq!(state.project_author_generation(), before + 1);
        assert!(opacity_key(&state, selection, keyframe_id).temporal_flags.auto_bezier);

        let outcome = state
            .edit_clip_numeric_curve(
                selection.clip_id,
                address,
                ClipNumericCurveEdit::SetInterpolation {
                    keyframe_id,
                    interpolation: InterpolationType::AutoBezier,
                },
            )
            .expect("repeat auto bezier");
        assert!(!outcome.changed);
        assert_eq!(state.project_author_generation(), before + 1);
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
                selection.clip_id,
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
                selection.clip_id,
                address.clone(),
                ClipNumericCurveEdit::Upsert {
                    keyframe_id: Some(first_id),
                    point: NormalizedCurvePoint::new(0.5, 0.5).expect("point"),
                },
            )
            .expect_err("occupied time rejects");
        state
            .edit_clip_numeric_curve(
                selection.clip_id,
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
                selection.clip_id,
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
                selection.clip_id,
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
