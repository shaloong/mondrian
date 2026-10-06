//! Focused visual curve clipboard commands over the sole transient clipboard.

use super::product_action::{ClipCurveClipboardOperation, ClipCurveClipboardPayload};
use super::*;

/// Typed product refusal; diagnostic text is never used as a UI message key.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CurveClipboardError {
    #[error("Focused curve owner is no longer selected")]
    OwnerChanged,
    #[error("Focused curve parameter instance is missing")]
    ParameterMissing,
    #[error("Focused curve parameter does not admit animation")]
    NotAnimated,
    #[error("Capture requires a complete unique nonempty stable key selection")]
    InvalidSelection,
    #[error("Focused curve requires compatible single-parameter keyframe clipboard content")]
    IncompatibleClipboard,
    #[error("Keyframe clipboard is empty")]
    EmptyClipboard,
    #[error("Active playhead is missing")]
    MissingPlayhead,
    #[error("The complete pasted group exceeds the curve owner extent")]
    OutOfBounds,
}

impl CurveClipboardError {
    pub(crate) fn message_id(&self) -> &'static str {
        match self {
            Self::OwnerChanged => "error-curve-clipboard-owner-changed",
            Self::ParameterMissing => "error-curve-clipboard-parameter-missing",
            Self::NotAnimated => "error-curve-clipboard-not-animated",
            Self::InvalidSelection => "error-curve-clipboard-invalid-selection",
            Self::IncompatibleClipboard => "error-curve-clipboard-incompatible",
            Self::EmptyClipboard => "error-curve-clipboard-empty",
            Self::MissingPlayhead => "error-curve-clipboard-missing-playhead",
            Self::OutOfBounds => "error-curve-clipboard-out-of-bounds",
        }
    }
    pub(super) fn into_core(self) -> mondrian_core::MondrianError {
        mondrian_core::MondrianError::Other(anyhow::Error::new(self))
    }
}

impl AppState {
    fn focused_curve_clipboard_selection(
        &self,
        payload: &ClipCurveClipboardPayload,
    ) -> mondrian_core::Result<SelectedClipRef> {
        self.primary_selected_clip()
            .filter(|selection| selection.clip_id == payload.clip_id)
            .ok_or_else(|| CurveClipboardError::OwnerChanged.into_core())
    }

    pub(super) fn curve_clipboard_available(&self, payload: &ClipCurveClipboardPayload) -> bool {
        let Ok(selection) = self.focused_curve_clipboard_selection(payload) else {
            return false;
        };
        let Some(clip) = self.clip_snapshot(selection) else {
            return false;
        };
        let Ok(properties) = clip.property_bag() else {
            return false;
        };
        let Some((_, property)) = properties.property_by_address(&payload.parameter) else {
            return false;
        };
        if !property.descriptor.schema.is_animatable {
            return false;
        }
        match &payload.operation {
            ClipCurveClipboardOperation::Copy { keyframe_ids } => {
                property.capture_complete_keyframes(keyframe_ids).is_ok()
            }
            ClipCurveClipboardOperation::Cut { keyframe_ids } => {
                self.active_sequence()
                    .and_then(|sequence| sequence.clip_track_location(payload.clip_id))
                    .is_some_and(|location| !location.is_locked)
                    && property.capture_complete_keyframes(keyframe_ids).is_ok()
            }
            ClipCurveClipboardOperation::Paste {} => {
                self.can_paste_from_app_clipboard() && self.has_animation_clipboard()
            }
        }
    }

    pub(super) fn dispatch_curve_clipboard(
        &mut self,
        payload: ClipCurveClipboardPayload,
    ) -> mondrian_core::Result<()> {
        let selection = self.focused_curve_clipboard_selection(&payload)?;
        let clip = self
            .clip_snapshot(selection)
            .ok_or_else(|| CurveClipboardError::OwnerChanged.into_core())?;
        let properties = clip.property_bag()?;
        let (path, property) = properties
            .property_by_address(&payload.parameter)
            .ok_or_else(|| CurveClipboardError::ParameterMissing.into_core())?;
        if !property.descriptor.schema.is_animatable {
            return Err(CurveClipboardError::NotAnimated.into_core());
        }
        let cut = matches!(&payload.operation, ClipCurveClipboardOperation::Cut { .. });
        match payload.operation {
            ClipCurveClipboardOperation::Copy { keyframe_ids }
            | ClipCurveClipboardOperation::Cut { keyframe_ids } => {
                if keyframe_ids.is_empty()
                    || keyframe_ids.iter().copied().collect::<HashSet<_>>().len()
                        != keyframe_ids.len()
                {
                    return Err(CurveClipboardError::InvalidSelection.into_core());
                }
                let selected = keyframe_ids
                    .iter()
                    .copied()
                    .map(|keyframe_id| AnimationKeyframeSelection {
                        property: AnimationPropertySelection {
                            clip_id: payload.clip_id,
                            property: payload.parameter.clone(),
                        },
                        keyframe_id,
                    })
                    .collect();
                let clipboard = Self::prepare_animation_keyframes(&properties, selected)?
                    .ok_or_else(|| CurveClipboardError::InvalidSelection.into_core())?;
                if cut {
                    let time = self
                        .current_timeline_time()?
                        .ok_or_else(|| CurveClipboardError::MissingPlayhead.into_core())?;
                    let retain_time = clip.clamped_visual_author_time(time)?;
                    self.mutate_clip_property(
                        selection,
                        PropertyMutation::RemoveKeyframes {
                            path: path.to_owned(),
                            keyframe_ids: keyframe_ids.clone(),
                            retain_time,
                        },
                        "Cut curve keyframes",
                    )?;
                    let removed: HashSet<_> = keyframe_ids.into_iter().collect();
                    self.animation_selection.selected_keyframes.retain(|key| {
                        key.property.clip_id != payload.clip_id
                            || key.property.property != payload.parameter
                            || !removed.contains(&key.keyframe_id)
                    });
                }
                self.clipboard = Some(AppClipboard::AnimationKeyframes(clipboard));
            }
            ClipCurveClipboardOperation::Paste {} => {
                let Some(AppClipboard::AnimationKeyframes(clipboard)) = &self.clipboard else {
                    return Err(CurveClipboardError::IncompatibleClipboard.into_core());
                };
                let Some(first) = clipboard.entries.first() else {
                    return Err(CurveClipboardError::EmptyClipboard.into_core());
                };
                if clipboard.entries.iter().any(|entry| entry.property != first.property) {
                    return Err(CurveClipboardError::IncompatibleClipboard.into_core());
                }
                let time = self
                    .current_timeline_time()?
                    .ok_or_else(|| CurveClipboardError::MissingPlayhead.into_core())?;
                let destination = clip.clamped_visual_author_time(time)?;
                let end = clip.clip_time_out()?;
                let mut groups = Vec::with_capacity(clipboard.entries.len());
                let mut selected = Vec::with_capacity(clipboard.entries.len());
                for entry in &clipboard.entries {
                    let id = KeyframeId::new();
                    let time = destination.checked_add(entry.relative_time)?;
                    if time < clip.clip_time_in || time > end {
                        return Err(CurveClipboardError::OutOfBounds.into_core());
                    }
                    let mut keys = entry.channel_keyframes.clone();
                    for key in &mut keys {
                        key.id = id;
                        key.time = time;
                    }
                    selected.push(AnimationKeyframeSelection {
                        property: AnimationPropertySelection {
                            clip_id: payload.clip_id,
                            property: payload.parameter.clone(),
                        },
                        keyframe_id: id,
                    });
                    groups.push(keys);
                }
                let mutations = vec![PropertyMutation::PasteCompleteKeyframes {
                    path: path.to_owned(),
                    keyframes: groups,
                }];
                self.mutate_clip_properties(selection, mutations, "Paste curve keyframes")?;
                self.set_animation_keyframe_selection(selected);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppMessage;
    use crate::app_ui::localization::{AppUiLocale, Localizer};

    #[test]
    fn focused_curve_clipboard_preserves_raw_channel_number_bits() {
        use crate::app::product_action::{ClipProductAction, ProductAction};
        use mondrian_core::automation::AnimationChannelValue;
        use mondrian_core::PropertyHost;
        let (mut state, mut request) = clipboard_fixture();
        let clip = state
            .active_sequence_mut_uncommitted()
            .expect("fixture")
            .find_clip_mut(request.clip_id)
            .expect("clip");
        let bag = clip.intrinsic_parameter_bag();
        let mut raw = bag
            .property(mondrian_timeline::clip::Transform2D::OPACITY_PATH)
            .expect("opacity")
            .channel(0)
            .expect("canonical channel")
            .keyframes()[0]
            .clone();
        raw.value = AnimationChannelValue::Number(0.2);
        clip.apply_property_mutation(PropertyMutation::SetChannelKeyframe {
            path: mondrian_timeline::clip::Transform2D::OPACITY_PATH.into(),
            channel_index: 0,
            keyframe: raw,
        })
        .expect("seed exact raw author value");
        let before = state.active_sequence().expect("original").clone();
        let source = before.find_clip(request.clip_id).expect("source").intrinsic_parameter_bag();
        assert_eq!(
            source
                .property(mondrian_timeline::clip::Transform2D::OPACITY_PATH)
                .expect("source property")
                .channel(0)
                .expect("source channel")
                .keyframes()[0]
                .value,
            AnimationChannelValue::Number(0.2)
        );
        state
            .dispatch_action(
                ProductAction::Clip(ClipProductAction::CurveClipboard(Box::new(request.clone())))
                    .into_external_action(),
            )
            .expect("focused Copy");
        request.operation = ClipCurveClipboardOperation::Paste {};
        state
            .dispatch_action(
                ProductAction::Clip(ClipProductAction::CurveClipboard(Box::new(request.clone())))
                    .into_external_action(),
            )
            .expect("focused Paste");
        let pasted = state
            .active_sequence()
            .expect("pasted")
            .find_clip(request.clip_id)
            .expect("pasted clip")
            .intrinsic_parameter_bag();
        let value = pasted
            .property(mondrian_timeline::clip::Transform2D::OPACITY_PATH)
            .expect("pasted property")
            .channel(0)
            .expect("pasted channel")
            .keyframes()[0]
            .value;
        let AnimationChannelValue::Number(value) = value else {
            panic!("numeric author key")
        };
        assert_eq!(
            value.to_bits(),
            0.2_f64.to_bits(),
            "Copy/Paste must retain canonical author bits before native projection"
        );
    }

    #[test]
    fn focused_curve_canonical_clipboard_survives_undo_saved_redo_and_fresh_reopen() {
        use crate::app::product_action::{ClipProductAction, ProductAction};
        use mondrian_core::automation::{AnimationChannelValue, InterpolationType};
        use mondrian_core::PropertyHost;
        for mode in [
            InterpolationType::Linear,
            InterpolationType::AutoBezier,
            InterpolationType::ContinuousBezier,
        ] {
            let (mut state, mut request) = clipboard_fixture();
            let clip = state
                .active_sequence_mut_uncommitted()
                .expect("fixture")
                .find_clip_mut(request.clip_id)
                .expect("clip");
            let keys = clip
                .intrinsic_parameter_bag()
                .property(mondrian_timeline::clip::Transform2D::OPACITY_PATH)
                .expect("opacity")
                .channel(0)
                .expect("channel")
                .keyframes()
                .to_vec();
            for (key, value) in keys.iter().zip([0.1, 0.2]) {
                let mut raw =
                    Keyframe::from_preset(key.time, AnimationChannelValue::Number(value), mode);
                raw.id = key.id;
                clip.apply_property_mutation(PropertyMutation::SetChannelKeyframe {
                    path: mondrian_timeline::clip::Transform2D::OPACITY_PATH.into(),
                    channel_index: 0,
                    keyframe: raw,
                })
                .expect("exact constrained source");
            }
            let original = state.active_sequence().expect("original").clone();
            let original_keys = original
                .find_clip(request.clip_id)
                .expect("clip")
                .intrinsic_parameter_bag()
                .property(mondrian_timeline::clip::Transform2D::OPACITY_PATH)
                .expect("opacity")
                .channel(0)
                .expect("channel")
                .keyframes()
                .to_vec();
            let generation = state.project_author_generation();
            state
                .dispatch_action(
                    ProductAction::Clip(ClipProductAction::CurveClipboard(Box::new(
                        request.clone(),
                    )))
                    .into_external_action(),
                )
                .expect("focused Copy");
            assert_eq!(state.project_author_generation(), generation);
            request.operation = ClipCurveClipboardOperation::Paste {};
            state
                .dispatch_action(
                    ProductAction::Clip(ClipProductAction::CurveClipboard(Box::new(
                        request.clone(),
                    )))
                    .into_external_action(),
                )
                .expect("focused Paste");
            assert_eq!(state.project_author_generation(), generation + 1);
            let pasted = state.active_sequence().expect("pasted").clone();
            let pasted_keys = pasted
                .find_clip(request.clip_id)
                .expect("clip")
                .intrinsic_parameter_bag()
                .property(mondrian_timeline::clip::Transform2D::OPACITY_PATH)
                .expect("opacity")
                .channel(0)
                .expect("channel")
                .keyframes()
                .to_vec();
            assert_eq!(original_keys.len(), pasted_keys.len());
            for (old, new) in original_keys.iter().zip(&pasted_keys) {
                assert_ne!(old.id, new.id);
                let mut expected = old.clone();
                expected.id = new.id;
                assert_eq!(
                    &expected, new,
                    "all author metadata and raw values survive Copy/Paste"
                );
            }
            assert!(state.undo_timeline().expect("single atomic Undo"));
            assert!(state
                .active_sequence()
                .expect("undone")
                .author_state_eq_ignoring_revision(&original));
            let directory = tempfile::tempdir().expect("archives");
            let undone_path = directory.path().join("精确曲线撤销🙂.mdp");
            state.save_project_file_as(undone_path.clone()).expect("save undone");
            assert!(state.can_redo_action());
            let mut reopened = AppState::new();
            reopened.open_project_file(undone_path).expect("fresh undone reopen");
            assert!(reopened
                .active_sequence()
                .expect("original reopened")
                .author_state_eq_ignoring_revision(&original));
            assert_eq!(reopened.active_clipboard_kind(), None);
            assert!(!reopened.can_undo_action() && !reopened.can_redo_action());
            drop(reopened);
            assert!(state.redo_timeline().expect("Redo after save"));
            assert!(state
                .active_sequence()
                .expect("redone")
                .author_state_eq_ignoring_revision(&pasted));
            let edited_path = directory.path().join("精确曲线重做🙂.mdp");
            state.save_project_file_as(edited_path.clone()).expect("save redone");
            drop(state);
            let mut reopened = AppState::new();
            reopened.open_project_file(edited_path).expect("fresh redone reopen");
            assert!(reopened
                .active_sequence()
                .expect("pasted reopened")
                .author_state_eq_ignoring_revision(&pasted));
            assert_eq!(reopened.active_clipboard_kind(), None);
            assert!(!reopened.can_undo_action() && !reopened.can_redo_action());
        }
    }

    #[test]
    fn focused_curve_large_canonical_group_has_complete_availability_and_atomic_paste() {
        use crate::app::product_action::{ClipProductAction, ProductAction};
        use mondrian_core::automation::AnimationChannelValue;
        use mondrian_core::PropertyHost;
        let (mut state, mut request) = clipboard_fixture();
        let groups: Vec<_> = (0..4096)
            .map(|index| {
                vec![Keyframe::linear(
                    TimelineTime::new(index, 4096).expect("exact time"),
                    AnimationChannelValue::Number(0.2),
                )]
            })
            .collect();
        let ids: Vec<_> = groups.iter().map(|keys| keys[0].id).collect();
        let clip = state
            .active_sequence_mut_uncommitted()
            .expect("fixture")
            .find_clip_mut(request.clip_id)
            .expect("clip");
        clip.apply_property_mutation(PropertyMutation::PasteCompleteKeyframes {
            path: mondrian_timeline::clip::Transform2D::OPACITY_PATH.into(),
            keyframes: groups,
        })
        .expect("seed canonical group as one batch");
        request.operation = ClipCurveClipboardOperation::Copy { keyframe_ids: ids.clone() };
        let original = state.active_sequence().expect("original").clone();
        let generation = state.project_author_generation();
        assert!(state.curve_clipboard_available(&request));
        state
            .dispatch_action(
                ProductAction::Clip(ClipProductAction::CurveClipboard(Box::new(request.clone())))
                    .into_external_action(),
            )
            .expect("complete Copy");
        assert_eq!(state.project_author_generation(), generation);
        request.operation = ClipCurveClipboardOperation::Paste {};
        assert!(state.curve_clipboard_available(&request));
        state
            .dispatch_action(
                ProductAction::Clip(ClipProductAction::CurveClipboard(Box::new(request.clone())))
                    .into_external_action(),
            )
            .expect("complete Paste");
        assert_eq!(state.project_author_generation(), generation + 1);
        let bag = state
            .active_sequence()
            .expect("pasted")
            .find_clip(request.clip_id)
            .expect("clip")
            .intrinsic_parameter_bag();
        let keys = bag
            .property(mondrian_timeline::clip::Transform2D::OPACITY_PATH)
            .expect("opacity")
            .channel(0)
            .expect("channel")
            .keyframes();
        assert_eq!(keys.len(), 4096);
        let old_ids: HashSet<_> = ids.into_iter().collect();
        for key in keys {
            assert!(!old_ids.contains(&key.id));
            let AnimationChannelValue::Number(value) = key.value else {
                panic!("numeric author value")
            };
            assert_eq!(value.to_bits(), 0.2_f64.to_bits());
        }
        assert_eq!(state.animation_selection.selected_keyframes.len(), 4096);
        assert!(state.undo_timeline().expect("one Undo"));
        assert!(state
            .active_sequence()
            .expect("undone")
            .author_state_eq_ignoring_revision(&original));
    }

    #[test]
    fn focused_curve_cut_refuses_without_publishing_captured_data_or_author_changes() {
        use crate::app::product_action::{ClipProductAction, ProductAction};
        for old_key_clipboard in [false, true] {
            for case in 0..7 {
                let (mut state, mut request) = clipboard_fixture();
                if old_key_clipboard {
                    state
                        .dispatch_action(
                            ProductAction::Clip(ClipProductAction::CurveClipboard(Box::new(
                                request.clone(),
                            )))
                            .into_external_action(),
                        )
                        .expect("previous key clipboard");
                } else {
                    state.copy_selected_clips_to_clipboard().expect("previous Clip clipboard");
                }
                let ClipCurveClipboardOperation::Copy { mut keyframe_ids } = request.operation
                else {
                    panic!("fixture Copy")
                };
                match case {
                    0 => request.clip_id = ClipId::new(),
                    1 => {
                        request.parameter.animation_track_id =
                            mondrian_core::AnimationTrackId::new()
                    }
                    2 => keyframe_ids.push(KeyframeId::new()),
                    3 => keyframe_ids.push(keyframe_ids[0]),
                    4 => keyframe_ids.clear(),
                    5 => {
                        state.active_sequence_mut_uncommitted().expect("late lock").video_tracks
                            [0]
                        .is_locked = true
                    }
                    _ => state.selection.selected_clips.clear(),
                }
                request.operation = ClipCurveClipboardOperation::Cut { keyframe_ids };
                let before = state.active_sequence().expect("before").clone();
                let clipboard = state.clipboard.clone();
                let selected = state.animation_selection.selected_keyframes.clone();
                let active = state.active_animation_property_address(request.clip_id).cloned();
                let generation = state.project_author_generation();
                let history = state.authoring_history().expect("history").diagnostics();
                assert!(!state.curve_clipboard_available(&request));
                state
                    .dispatch_action(
                        ProductAction::Clip(ClipProductAction::CurveClipboard(Box::new(
                            request.clone(),
                        )))
                        .into_external_action(),
                    )
                    .expect_err("complete Cut refusal");
                assert_clipboard_unchanged(&clipboard, &state.clipboard);
                assert_eq!(state.active_sequence().expect("after"), &before);
                assert_eq!(state.animation_selection.selected_keyframes, selected);
                assert_eq!(
                    state.active_animation_property_address(request.clip_id),
                    active.as_ref()
                );
                assert_eq!(state.project_author_generation(), generation);
                assert_eq!(
                    state.authoring_history().expect("history").diagnostics(),
                    history
                );
            }
        }
    }

    #[test]
    fn focused_curve_cut_preserves_other_parameters_and_durable_saved_redo() {
        use crate::app::product_action::{ClipProductAction, ProductAction};
        use mondrian_timeline::clip::Transform2D;
        let (mut state, mut request) = clipboard_fixture();
        let ClipCurveClipboardOperation::Copy { keyframe_ids } = request.operation.clone() else {
            panic!("Copy")
        };
        let bag = state
            .active_sequence()
            .expect("sequence")
            .find_clip(request.clip_id)
            .expect("clip")
            .intrinsic_parameter_bag();
        let rotation = bag.property(Transform2D::ROTATION_PATH).expect("rotation");
        let other = AnimationKeyframeSelection {
            property: AnimationPropertySelection {
                clip_id: request.clip_id,
                property: rotation.address(),
            },
            keyframe_id: rotation.channel(0).expect("channel").keyframes()[0].id,
        };
        let mut selected: Vec<_> = keyframe_ids
            .iter()
            .map(|id| AnimationKeyframeSelection {
                property: AnimationPropertySelection {
                    clip_id: request.clip_id,
                    property: request.parameter.clone(),
                },
                keyframe_id: *id,
            })
            .collect();
        selected.push(other.clone());
        state.set_animation_keyframe_selection(selected);
        let original = state.active_sequence().expect("original").clone();
        let generation = state.project_author_generation();
        request.operation = ClipCurveClipboardOperation::Cut { keyframe_ids: keyframe_ids.clone() };
        assert!(state.curve_clipboard_available(&request));
        state
            .dispatch_action(
                ProductAction::Clip(ClipProductAction::CurveClipboard(Box::new(request.clone())))
                    .into_external_action(),
            )
            .expect("targeted Cut");
        assert_eq!(state.project_author_generation(), generation + 1);
        let cut = state.active_sequence().expect("cut").clone();
        let after = cut
            .find_clip(request.clip_id)
            .expect("owning Clip retained")
            .intrinsic_parameter_bag();
        assert!(after
            .property(Transform2D::OPACITY_PATH)
            .expect("opacity")
            .channel(0)
            .expect("channel")
            .keyframes()
            .is_empty());
        assert_eq!(
            after.property(Transform2D::ROTATION_PATH),
            bag.property(Transform2D::ROTATION_PATH)
        );
        assert_eq!(
            state.animation_selection.selected_keyframes,
            HashSet::from([other])
        );
        let Some(AppClipboard::AnimationKeyframes(clipboard)) = &state.clipboard else {
            panic!("key clipboard")
        };
        assert_eq!(clipboard.entries.len(), 2);
        for (entry, id) in clipboard.entries.iter().zip(&keyframe_ids) {
            assert_eq!(entry.property, request.parameter);
            assert_eq!(
                entry.channel_keyframes,
                bag.property(Transform2D::OPACITY_PATH)
                    .expect("source")
                    .capture_complete_keyframes(&[*id])
                    .expect("raw source")
                    .remove(0)
            );
        }
        let retained_clipboard = state.clipboard.clone();
        assert!(state.undo_timeline().expect("one Cut Undo"));
        assert!(state
            .active_sequence()
            .expect("undone")
            .author_state_eq_ignoring_revision(&original));
        assert_clipboard_unchanged(&retained_clipboard, &state.clipboard);
        let directory = tempfile::tempdir().expect("archives");
        let original_path = directory.path().join("曲线剪切撤销🙂.mdp");
        state.save_project_file_as(original_path.clone()).expect("save undone");
        assert!(state.can_redo_action());
        let mut reopened = AppState::new();
        reopened.open_project_file(original_path).expect("fresh original reopen");
        assert!(reopened
            .active_sequence()
            .expect("original")
            .author_state_eq_ignoring_revision(&original));
        assert_eq!(reopened.active_clipboard_kind(), None);
        assert!(!reopened.can_undo_action() && !reopened.can_redo_action());
        drop(reopened);
        assert!(state.redo_timeline().expect("Redo after saving"));
        assert!(state.active_sequence().expect("redone").author_state_eq_ignoring_revision(&cut));
        assert_clipboard_unchanged(&retained_clipboard, &state.clipboard);
        let cut_path = directory.path().join("曲线剪切重做🙂.mdp");
        state.save_project_file_as(cut_path.clone()).expect("save Cut");
        drop(state);
        let mut reopened = AppState::new();
        reopened.open_project_file(cut_path).expect("fresh Cut reopen");
        assert!(reopened.active_sequence().expect("cut").author_state_eq_ignoring_revision(&cut));
        assert_eq!(reopened.active_clipboard_kind(), None);
        assert!(!reopened.can_undo_action() && !reopened.can_redo_action());
    }

    fn clipboard_fixture() -> (AppState, ClipCurveClipboardPayload) {
        use mondrian_timeline::clip::Transform2D;
        let mut clip =
            Clip::new_basic_title("Clipboard", "Arial", TimelineTime::ZERO, TimelineTime::ONE)
                .expect("title");
        let mut ids = Vec::new();
        for (time, value) in [
            (TimelineTime::ZERO, 0.25),
            (TimelineTime::new(1, 2).expect("time"), 0.75),
        ] {
            let keyframe = Keyframe::linear(time, PropertyValue::Float(value));
            ids.push(keyframe.id);
            clip.apply_property_mutation(PropertyMutation::SetKeyframe {
                path: Transform2D::OPACITY_PATH.into(),
                keyframe,
            })
            .expect("seed opacity");
        }
        clip.apply_property_mutation(PropertyMutation::SetKeyframe {
            path: Transform2D::ROTATION_PATH.into(),
            keyframe: Keyframe::linear(TimelineTime::ZERO, PropertyValue::Float(45.0)),
        })
        .expect("seed rotation");
        let parameter = clip
            .intrinsic_parameter_bag()
            .address_for_path(Transform2D::OPACITY_PATH)
            .expect("address");
        let clip_id = clip.id;
        let mut sequence = mondrian_timeline::Sequence::new("Focused clipboard");
        sequence.video_tracks[0].add_clip(clip).expect("place");
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        state.select_clip_by_id(clip_id).expect("select");
        (
            state,
            ClipCurveClipboardPayload {
                clip_id,
                parameter,
                operation: ClipCurveClipboardOperation::Copy { keyframe_ids: ids },
            },
        )
    }

    fn assert_clipboard_unchanged(before: &Option<AppClipboard>, after: &Option<AppClipboard>) {
        match (before, after) {
            (
                Some(AppClipboard::AnimationKeyframes(a)),
                Some(AppClipboard::AnimationKeyframes(b)),
            ) => {
                let entries = |clipboard: &AnimationClipboard| {
                    clipboard
                        .entries
                        .iter()
                        .map(|entry| {
                            (
                                entry.property.clone(),
                                entry.relative_time,
                                entry.channel_keyframes.clone(),
                            )
                        })
                        .collect::<Vec<_>>()
                };
                assert_eq!(entries(a), entries(b));
            }
            (Some(AppClipboard::Clips(a)), Some(AppClipboard::Clips(b))) => {
                assert_eq!(a.entries.len(), b.entries.len());
                for (a, b) in a.entries.iter().zip(&b.entries) {
                    assert_eq!(a.original_clip_id, b.original_clip_id);
                    assert_eq!(a.track_id, b.track_id);
                    assert_eq!(a.is_video_track, b.is_video_track);
                    assert_eq!(a.relative_start, b.relative_start);
                    assert_eq!(a.clip, b.clip);
                    assert_eq!(a.audio_processing_scopes, b.audio_processing_scopes);
                }
                assert_eq!(a.audio_transitions, b.audio_transitions);
                assert_eq!(a.video_transitions, b.video_transitions);
            }
            (None, None) => {}
            _ => panic!("clipboard intent changed on refusal"),
        }
    }

    #[test]
    fn focused_curve_clipboard_refuses_complete_request_without_transient_or_author_changes() {
        use crate::app::product_action::{ClipProductAction, ProductAction};
        use mondrian_timeline::clip::Transform2D;
        for case in 0..10 {
            let (mut state, mut request) = clipboard_fixture();
            let copy_action =
                ProductAction::Clip(ClipProductAction::CurveClipboard(Box::new(request.clone())))
                    .into_external_action();
            state.dispatch_action(copy_action).expect("capture actual keys");
            match case {
                0 => request.clip_id = ClipId::new(),
                1 => request.parameter.animation_track_id = mondrian_core::AnimationTrackId::new(),
                2 => {
                    request.operation =
                        ClipCurveClipboardOperation::Copy { keyframe_ids: vec![KeyframeId::new()] }
                }
                3 => {
                    let ClipCurveClipboardOperation::Copy { keyframe_ids } = &mut request.operation
                    else {
                        panic!("copy");
                    };
                    keyframe_ids.push(keyframe_ids[0]);
                }
                4 => {
                    request.operation =
                        ClipCurveClipboardOperation::Copy { keyframe_ids: Vec::new() }
                }
                5 => {
                    state.copy_selected_clips_to_clipboard().expect("legitimate Clip clipboard");
                    request.operation = ClipCurveClipboardOperation::Paste {};
                }
                6 => {
                    let time = TimelineTime::new(3, 4).expect("time");
                    let position = time
                        .to_frame_position(
                            state.active_sequence().expect("sequence").time_base(),
                            mondrian_core::FrameRounding::Ceil,
                        )
                        .expect("grid");
                    state
                        .dispatch_action(mondrian_editor_state::Action::Seek(position))
                        .expect("real Playback seek");
                    assert!(state
                        .current_timeline_time()
                        .expect("clock")
                        .is_some_and(|current| current >= time));
                    request.operation = ClipCurveClipboardOperation::Paste {};
                }
                7 => {
                    state.active_sequence_mut_uncommitted().expect("late lock").video_tracks[0]
                        .is_locked = true;
                    request.operation = ClipCurveClipboardOperation::Paste {};
                }
                8 => {
                    let clip = state
                        .active_sequence()
                        .expect("sequence")
                        .find_clip(request.clip_id)
                        .expect("clip");
                    let bag = clip.intrinsic_parameter_bag();
                    let rotation = bag.property(Transform2D::ROTATION_PATH).expect("rotation");
                    let ClipCurveClipboardOperation::Copy { keyframe_ids } = &request.operation
                    else {
                        panic!("copy");
                    };
                    let mut selected = keyframe_ids
                        .iter()
                        .map(|id| AnimationKeyframeSelection {
                            property: AnimationPropertySelection {
                                clip_id: request.clip_id,
                                property: request.parameter.clone(),
                            },
                            keyframe_id: *id,
                        })
                        .collect::<Vec<_>>();
                    selected.push(AnimationKeyframeSelection {
                        property: AnimationPropertySelection {
                            clip_id: request.clip_id,
                            property: rotation.address(),
                        },
                        keyframe_id: rotation.channels()[0].keyframes()[0].id,
                    });
                    state.set_animation_keyframe_selection(selected);
                    state
                        .dispatch_action(mondrian_editor_state::Action::Copy)
                        .expect("legitimate multi-parameter clipboard");
                    request.operation = ClipCurveClipboardOperation::Paste {};
                }
                _ => {
                    state.clipboard = None;
                    request.operation = ClipCurveClipboardOperation::Paste {};
                }
            }
            let sequence = state.active_sequence().expect("before").clone();
            let clipboard = state.clipboard.clone();
            let selected = state.animation_selection.selected_keyframes.clone();
            let generation = state.project_author_generation();
            let history = state.authoring_history().expect("history").diagnostics();
            let action = ProductAction::Clip(ClipProductAction::CurveClipboard(Box::new(request)))
                .into_external_action();
            assert!(
                state.dispatch_action(action).is_err(),
                "refusal case {case}"
            );
            assert_clipboard_unchanged(&clipboard, &state.clipboard);
            assert_eq!(
                state.active_sequence().expect("after"),
                &sequence,
                "case {case}"
            );
            assert_eq!(
                state.animation_selection.selected_keyframes, selected,
                "case {case}"
            );
            assert_eq!(state.project_author_generation(), generation, "case {case}");
            assert_eq!(
                state.authoring_history().expect("history").diagnostics(),
                history,
                "case {case}"
            );
        }
    }

    #[test]
    fn focused_curve_clipboard_refusals_localize_by_type_through_error_chains() {
        let english = Localizer::new(AppUiLocale::EnUs).expect("English");
        let chinese = Localizer::new(AppUiLocale::ZhCn).expect("Chinese");
        for failure in [
            CurveClipboardError::OwnerChanged,
            CurveClipboardError::ParameterMissing,
            CurveClipboardError::NotAnimated,
            CurveClipboardError::InvalidSelection,
            CurveClipboardError::IncompatibleClipboard,
            CurveClipboardError::EmptyClipboard,
            CurveClipboardError::MissingPlayhead,
            CurveClipboardError::OutOfBounds,
        ] {
            let expected_id = failure.message_id();
            let core = failure.into_core();
            let message = AppMessage::from_error(&core);
            assert_eq!(message.id, expected_id);
            assert!(
                message.args.is_empty(),
                "internal diagnostic text is not a display argument"
            );
            let en = english.message(&message);
            let zh = chinese.message(&message);
            assert_ne!(en, expected_id, "English catalog entry must exist");
            assert_ne!(zh, expected_id, "Chinese catalog entry must exist");
            assert_ne!(en, zh, "locale switch reformats the same retained message");
            let wrapped = anyhow::Error::new(core).context("opaque diagnostic context");
            assert_eq!(
                AppMessage::from_anyhow(&wrapped),
                message,
                "typed cause remains authoritative through context"
            );
        }
    }
}
