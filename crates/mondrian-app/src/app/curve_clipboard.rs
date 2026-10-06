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
    #[error("Copy requires a complete unique nonempty stable key selection")]
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
                !keyframe_ids.is_empty()
                    && keyframe_ids.iter().copied().collect::<HashSet<_>>().len()
                        == keyframe_ids.len()
                    && keyframe_ids.iter().all(|id| property.keyframe_by_id(*id).is_some())
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
        match payload.operation {
            ClipCurveClipboardOperation::Copy { keyframe_ids } => {
                if keyframe_ids.is_empty()
                    || keyframe_ids.iter().copied().collect::<HashSet<_>>().len()
                        != keyframe_ids.len()
                {
                    return Err(CurveClipboardError::InvalidSelection.into_core());
                }
                let selected = keyframe_ids
                    .into_iter()
                    .map(|keyframe_id| AnimationKeyframeSelection {
                        property: AnimationPropertySelection {
                            clip_id: payload.clip_id,
                            property: payload.parameter.clone(),
                        },
                        keyframe_id,
                    })
                    .collect();
                if !self.capture_animation_keyframes(&properties, selected)? {
                    return Err(CurveClipboardError::InvalidSelection.into_core());
                }
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
                let mut mutations = Vec::with_capacity(clipboard.entries.len());
                let mut selected = Vec::with_capacity(clipboard.entries.len());
                for entry in &clipboard.entries {
                    let mut keyframe = entry.keyframe.clone();
                    keyframe.id = KeyframeId::new();
                    keyframe.time = destination.checked_add(entry.relative_time)?;
                    if keyframe.time < clip.clip_time_in || keyframe.time > end {
                        return Err(CurveClipboardError::OutOfBounds.into_core());
                    }
                    selected.push(AnimationKeyframeSelection {
                        property: AnimationPropertySelection {
                            clip_id: payload.clip_id,
                            property: payload.parameter.clone(),
                        },
                        keyframe_id: keyframe.id,
                    });
                    mutations
                        .push(PropertyMutation::SetKeyframe { path: path.to_owned(), keyframe });
                }
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
                                entry.keyframe.clone(),
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
