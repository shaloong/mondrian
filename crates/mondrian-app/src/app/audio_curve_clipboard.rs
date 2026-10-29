//! Focus-qualified audio curve commands over the sole transient clipboard.

use mondrian_core::automation::{ParameterUnit, PropertyValueType};
use mondrian_core::{
    AuthoringTimeDomain, DomainTime, ExactAutomationKeyframe, KeyframeId, TimelineTime,
};
use mondrian_timeline::{
    audio_automation_author_time, inspect_audio_automation, AudioAutomationEdit,
    AudioAutomationEditRequest, AudioAutomationTarget,
};

use super::curve_clipboard::CurveClipboardError;
use super::product_action::{AudioCurveClipboardPayload, CurveClipboardOperation};
use super::{AppClipboard, AppState, AudioCurveSelection};

/// Complete authored scalar keys in relative owner time, independent of UI projection.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct AudioAutomationClipboard {
    unit: ParameterUnit,
    value_type: PropertyValueType,
    keyframes: Vec<ExactAutomationKeyframe>,
}

struct PreparedAudioCurveClipboard {
    capture: Option<AudioAutomationClipboard>,
    edit: Option<AudioAutomationEditRequest>,
    selection: Option<AudioCurveSelection>,
}

impl AppState {
    fn prepare_audio_curve_clipboard(
        &self,
        payload: &AudioCurveClipboardPayload,
    ) -> mondrian_core::Result<PreparedAudioCurveClipboard> {
        let sequence = self
            .active_sequence()
            .filter(|sequence| sequence.id == payload.sequence_id)
            .ok_or_else(|| CurveClipboardError::OwnerChanged.into_core())?;
        let inspection = inspect_audio_automation(sequence, &payload.target)
            .map_err(|_| CurveClipboardError::ParameterMissing.into_core())?;
        match inspection.time_domain() {
            AuthoringTimeDomain::AudioComponentEdit(_) => {
                if payload.scope_occurrence.is_some() {
                    return Err(CurveClipboardError::OwnerChanged.into_core());
                }
                let clip_id = match &payload.target {
                    AudioAutomationTarget::ComponentVolume { clip_id, .. }
                    | AudioAutomationTarget::ComponentPan { clip_id, .. } => *clip_id,
                    _ => return Err(CurveClipboardError::OwnerChanged.into_core()),
                };
                if self.primary_selected_clip().is_none_or(|selected| selected.clip_id != clip_id) {
                    return Err(CurveClipboardError::OwnerChanged.into_core());
                }
            }
            AuthoringTimeDomain::AudioProcessingScope(scope_id) => {
                let occurrence = payload
                    .scope_occurrence
                    .ok_or_else(|| CurveClipboardError::OwnerChanged.into_core())?;
                if self
                    .primary_selected_clip()
                    .is_none_or(|selected| selected.clip_id != occurrence.clip_id)
                    || self.selection.audio_scope_occurrences.get(&(sequence.id, scope_id))
                        != Some(&occurrence)
                {
                    return Err(CurveClipboardError::OwnerChanged.into_core());
                }
            }
            AuthoringTimeDomain::Sequence(_) => {
                if payload.scope_occurrence.is_some() {
                    return Err(CurveClipboardError::OwnerChanged.into_core());
                }
            }
            _ => return Err(CurveClipboardError::OwnerChanged.into_core()),
        }
        let schema = inspection.curve_contract().schema();
        if !schema.is_animatable {
            return Err(CurveClipboardError::NotAnimated.into_core());
        }
        if !matches!(payload.operation, CurveClipboardOperation::Copy { .. })
            && !inspection.is_editable()
        {
            return Err(CurveClipboardError::OwnerChanged.into_core());
        }
        let owner_time = || {
            let time = self
                .current_timeline_time()?
                .ok_or_else(|| CurveClipboardError::MissingPlayhead.into_core())?;
            audio_automation_author_time(
                sequence,
                &payload.target,
                DomainTime::new(AuthoringTimeDomain::Sequence(sequence.id), time),
                payload.scope_occurrence,
            )
            .map_err(|_| CurveClipboardError::OwnerChanged.into_core())
        };
        match &payload.operation {
            CurveClipboardOperation::Copy { keyframe_ids }
            | CurveClipboardOperation::Cut { keyframe_ids } => {
                let source = inspection
                    .curve()
                    .ok_or_else(|| CurveClipboardError::InvalidSelection.into_core())?;
                let mut keys = source
                    .capture_keyframes(keyframe_ids)
                    .map_err(|_| CurveClipboardError::InvalidSelection.into_core())?;
                let origin = keys
                    .first()
                    .ok_or_else(|| CurveClipboardError::InvalidSelection.into_core())?
                    .time;
                for key in &mut keys {
                    key.time = key.time.checked_sub(origin)?;
                }
                let edit = if matches!(payload.operation, CurveClipboardOperation::Cut { .. }) {
                    Some(AudioAutomationEditRequest {
                        target: payload.target.clone(),
                        edit: AudioAutomationEdit::RemoveKeyframes {
                            keyframe_ids: keyframe_ids.clone(),
                            retain_time: owner_time()?.time,
                        },
                    })
                } else {
                    None
                };
                Ok(PreparedAudioCurveClipboard {
                    capture: Some(AudioAutomationClipboard {
                        unit: schema.unit,
                        value_type: schema.value_type,
                        keyframes: keys,
                    }),
                    edit,
                    selection: None,
                })
            }
            CurveClipboardOperation::Paste {} => {
                let Some(AppClipboard::AudioAutomation(clipboard)) = &self.clipboard else {
                    return Err(CurveClipboardError::IncompatibleClipboard.into_core());
                };
                if clipboard.keyframes.is_empty() {
                    return Err(CurveClipboardError::EmptyClipboard.into_core());
                }
                if clipboard.unit != schema.unit || clipboard.value_type != schema.value_type {
                    return Err(CurveClipboardError::IncompatibleClipboard.into_core());
                }
                let destination = owner_time()?.time;
                let component_range = match &payload.target {
                    AudioAutomationTarget::ComponentVolume { clip_id, .. }
                    | AudioAutomationTarget::ComponentPan { clip_id, .. } => {
                        let clip = sequence
                            .find_clip(*clip_id)
                            .ok_or_else(|| CurveClipboardError::OwnerChanged.into_core())?;
                        let end = clip.position.checked_add(clip.duration)?;
                        let resolve = |time| {
                            audio_automation_author_time(
                                sequence,
                                &payload.target,
                                DomainTime::new(AuthoringTimeDomain::Sequence(sequence.id), time),
                                payload.scope_occurrence,
                            )
                            .map(|time| time.time)
                            .map_err(|_| CurveClipboardError::OwnerChanged.into_core())
                        };
                        Some((resolve(clip.position)?, resolve(end)?))
                    }
                    _ => None,
                };
                let mut keys = clipboard.keyframes.clone();
                for key in &mut keys {
                    key.id = KeyframeId::new();
                    key.time = destination.checked_add(key.time)?;
                    if key.time < TimelineTime::ZERO
                        || component_range
                            .is_some_and(|(start, end)| key.time < start || key.time > end)
                    {
                        return Err(CurveClipboardError::OutOfBounds.into_core());
                    }
                }
                let selection = AudioCurveSelection {
                    sequence_id: sequence.id,
                    target: payload.target.clone(),
                    keyframe_ids: keys.iter().map(|key| key.id).collect(),
                };
                let edit = AudioAutomationEdit::PasteKeyframes { keyframes: keys };
                let source = match inspection.curve() {
                    Some(curve) => curve.clone(),
                    None => mondrian_core::ExactAutomationCurve::new(
                        inspection.parameter_id().clone(),
                        inspection.default_value(),
                    )
                    .map_err(|_| CurveClipboardError::NotAnimated.into_core())?,
                };
                inspection
                    .curve_contract()
                    .preview_edit(&source, &edit)
                    .map_err(|_| CurveClipboardError::IncompatibleClipboard.into_core())?;
                Ok(PreparedAudioCurveClipboard {
                    capture: None,
                    edit: Some(AudioAutomationEditRequest { target: payload.target.clone(), edit }),
                    selection: Some(selection),
                })
            }
        }
    }

    pub(super) fn audio_curve_clipboard_available(
        &self,
        payload: &AudioCurveClipboardPayload,
    ) -> bool {
        self.prepare_audio_curve_clipboard(payload).is_ok()
    }

    pub(super) fn dispatch_audio_curve_clipboard(
        &mut self,
        payload: AudioCurveClipboardPayload,
    ) -> mondrian_core::Result<()> {
        let prepared = self.prepare_audio_curve_clipboard(&payload)?;
        if let Some(edit) = prepared.edit {
            self.edit_audio_automation(edit)?;
        }
        if let Some(selection) = prepared.selection {
            self.animation_selection.audio_curve = Some(selection);
        }
        if let Some(capture) = prepared.capture {
            self.clipboard = Some(AppClipboard::AudioAutomation(capture));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::product_action::{AudioProductAction, ProductAction};
    use mondrian_core::{ExactAutomationCurve, InterpolationType, SequenceId};
    use mondrian_timeline::{apply_audio_automation_edit, AudioChannelStripOwner, Sequence};

    fn fixture(preset: InterpolationType) -> (AppState, AudioCurveClipboardPayload) {
        let mut sequence = Sequence::new("Audio clipboard");
        let target = AudioAutomationTarget::ChannelFader {
            owner: AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id },
        };
        let mut ids = Vec::new();
        for (time, value) in [(0, -24.125), (1, -12.2), (2, -18.3)] {
            let key =
                ExactAutomationKeyframe::linear(TimelineTime::new(time, 3).expect("time"), value);
            ids.push(key.id);
            apply_audio_automation_edit(
                &mut sequence,
                &AudioAutomationEditRequest {
                    target: target.clone(),
                    edit: AudioAutomationEdit::UpsertKeyframe { keyframe: key },
                },
            )
            .expect("seed");
        }
        apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest {
                target: target.clone(),
                edit: AudioAutomationEdit::SetInterpolations {
                    keyframe_ids: ids.clone(),
                    interpolation: preset,
                },
            },
        )
        .expect("mode");
        let payload = AudioCurveClipboardPayload {
            sequence_id: sequence.id,
            target,
            scope_occurrence: None,
            operation: CurveClipboardOperation::Copy { keyframe_ids: ids },
        };
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        (state, payload)
    }

    fn curve(state: &AppState, payload: &AudioCurveClipboardPayload) -> ExactAutomationCurve {
        inspect_audio_automation(state.active_sequence().expect("sequence"), &payload.target)
            .expect("target")
            .curve()
            .expect("authored curve")
            .clone()
    }

    fn clipboard(state: &AppState) -> &AudioAutomationClipboard {
        let Some(AppClipboard::AudioAutomation(clipboard)) = &state.clipboard else {
            panic!("sole audio clipboard");
        };
        clipboard
    }

    #[test]
    fn audio_curve_clipboard_copy_cut_paste_preserves_exact_group_and_saved_history() {
        for preset in [
            InterpolationType::Linear,
            InterpolationType::AutoBezier,
            InterpolationType::ContinuousBezier,
        ] {
            let (mut state, mut payload) = fixture(preset);
            let original = curve(&state, &payload);
            let generation = state.project_author_generation();
            let history = state.authoring_history().expect("history").diagnostics();
            let command = ProductAction::Audio(AudioProductAction::CurveClipboard(Box::new(
                payload.clone(),
            )));
            let encoded = command.clone().into_external_action();
            assert_eq!(
                ProductAction::decode_external(&encoded).expect("decode"),
                Some(command)
            );
            state.dispatch_action(encoded).expect("real Copy command");
            assert_eq!(
                clipboard(&state).keyframes,
                original.keyframes.iter().cloned().collect::<Vec<_>>()
            );
            assert_eq!(
                clipboard(&state).keyframes[1].value.to_bits(),
                (-12.2_f64).to_bits()
            );
            assert_eq!(state.project_author_generation(), generation);
            assert_eq!(
                state.authoring_history().expect("history").diagnostics(),
                history
            );
            assert!(
                !state.can_paste_from_app_clipboard(),
                "global Paste requires explicit audio focus"
            );
            let captured = clipboard(&state).clone();
            let ids = original.keyframes.iter().map(|key| key.id).collect::<Vec<_>>();
            payload.operation = CurveClipboardOperation::Cut { keyframe_ids: ids.clone() };
            state
                .dispatch_action(
                    ProductAction::Audio(AudioProductAction::CurveClipboard(Box::new(
                        payload.clone(),
                    )))
                    .into_external_action(),
                )
                .expect("Cut once");
            let cut_state = state.active_sequence().expect("cut").clone();
            assert_eq!(state.project_author_generation(), generation + 1);
            assert_eq!(clipboard(&state), &captured);
            state.set_playback_frame_running(48);
            let destination = state.current_timeline_time().expect("playhead").expect("time");
            assert!(
                destination > TimelineTime::ONE,
                "fixture shifts the exact playhead"
            );
            payload.operation = CurveClipboardOperation::Paste {};
            state
                .dispatch_action(
                    ProductAction::Audio(AudioProductAction::CurveClipboard(Box::new(
                        payload.clone(),
                    )))
                    .into_external_action(),
                )
                .expect("Paste once");
            let pasted = curve(&state, &payload);
            assert_eq!(pasted.keyframes.len(), original.keyframes.len());
            for (new, old) in pasted.keyframes.iter().zip(&original.keyframes) {
                assert!(!ids.contains(&new.id));
                assert_eq!(
                    new.time,
                    destination.checked_add(old.time).expect("exact relative time")
                );
                assert_eq!(new.value.to_bits(), old.value.to_bits());
                assert_eq!(new.tangent_mode, old.tangent_mode);
                assert_eq!(new.interpolation_to_next, old.interpolation_to_next);
                assert_eq!(new.in_handle, old.in_handle);
                assert_eq!(new.out_handle, old.out_handle);
            }
            assert_eq!(state.project_author_generation(), generation + 2);
            assert_eq!(
                state.authoring_history().expect("history").diagnostics().undo_entries,
                history.undo_entries + 2
            );
            assert_eq!(clipboard(&state), &captured);
            let directory =
                std::env::temp_dir().join(format!("audio-clipboard-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&directory).expect("directory");
            let undone = directory.join("撤销 音频🙂.mdp");
            let redone = directory.join("重做 音频🙂.mdp");
            assert!(state.undo_timeline().expect("one Paste Undo"));
            assert!(state
                .active_sequence()
                .expect("undone")
                .author_state_eq_ignoring_revision(&cut_state));
            state.save_project_file_as(undone.clone()).expect("save undone");
            assert!(state.redo_timeline().expect("Redo after save"));
            assert_eq!(curve(&state, &payload), pasted);
            state.save_project_file_as(redone.clone()).expect("save redone");
            drop(state);
            for (path, expected) in [(undone, None), (redone, Some(&pasted))] {
                let mut reopened = AppState::new();
                reopened.open_project_file(path).expect("fresh Unicode reopen");
                let restored = inspect_audio_automation(
                    reopened.active_sequence().expect("sequence"),
                    &payload.target,
                )
                .expect("target")
                .curve()
                .cloned();
                assert_eq!(restored.as_ref(), expected);
                assert!(reopened.active_clipboard_kind().is_none());
                assert_eq!(
                    reopened.authoring_history().expect("fresh history").diagnostics().undo_entries,
                    0
                );
                drop(reopened);
            }
            std::fs::remove_dir_all(directory).expect("owned fixture cleanup");
        }
    }

    #[test]
    fn audio_curve_clipboard_invalid_intent_preserves_old_clipboard_author_and_history() {
        let (mut state, payload) = fixture(InterpolationType::AutoBezier);
        state.dispatch_audio_curve_clipboard(payload.clone()).expect("initial Copy");
        let old = clipboard(&state).clone();
        let original = state.active_sequence().expect("sequence").clone();
        let generation = state.project_author_generation();
        let history = state.authoring_history().expect("history").diagnostics();
        let id = old.keyframes[0].id;
        let mut wrong_sequence = payload.clone();
        wrong_sequence.sequence_id = SequenceId::new();
        let mut wrong_target = payload.clone();
        wrong_target.target = AudioAutomationTarget::ChannelFader {
            owner: AudioChannelStripOwner::Track { track_id: mondrian_core::TrackId::new() },
        };
        let mut unexpected_occurrence = payload.clone();
        unexpected_occurrence.scope_occurrence = Some(mondrian_timeline::AudioComponentAddress {
            track_id: mondrian_core::TrackId::new(),
            clip_id: mondrian_core::ClipId::new(),
            edit_id: mondrian_core::AudioComponentEditId::new(),
        });
        let mut invalid = vec![wrong_sequence, wrong_target, unexpected_occurrence];
        for ids in [vec![], vec![id, id], vec![id, KeyframeId::new()]] {
            for cut in [false, true] {
                let mut request = payload.clone();
                request.operation = if cut {
                    CurveClipboardOperation::Cut { keyframe_ids: ids.clone() }
                } else {
                    CurveClipboardOperation::Copy { keyframe_ids: ids.clone() }
                };
                invalid.push(request);
            }
        }
        for request in invalid {
            assert!(!state.audio_curve_clipboard_available(&request));
            assert!(state.dispatch_audio_curve_clipboard(request).is_err());
            assert!(state
                .active_sequence()
                .expect("unchanged")
                .author_state_eq_ignoring_revision(&original));
            assert_eq!(state.project_author_generation(), generation);
            assert_eq!(
                state.authoring_history().expect("history").diagnostics(),
                history
            );
            assert_eq!(clipboard(&state), &old);
        }
        state.active_sequence_mut_uncommitted().expect("lock fixture").audio_tracks[0].is_locked =
            true;
        assert!(
            state.audio_curve_clipboard_available(&payload),
            "locked target still admits Copy"
        );
        state.dispatch_audio_curve_clipboard(payload.clone()).expect("locked Copy");
        for operation in [
            CurveClipboardOperation::Cut {
                keyframe_ids: old.keyframes.iter().map(|key| key.id).collect(),
            },
            CurveClipboardOperation::Paste {},
        ] {
            let mut request = payload.clone();
            request.operation = operation;
            let locked = state.active_sequence().expect("locked").clone();
            assert!(!state.audio_curve_clipboard_available(&request));
            assert!(state.dispatch_audio_curve_clipboard(request).is_err());
            assert!(state
                .active_sequence()
                .expect("unchanged")
                .author_state_eq_ignoring_revision(&locked));
            assert_eq!(clipboard(&state), &old);
            assert_eq!(state.project_author_generation(), generation);
            assert_eq!(
                state.authoring_history().expect("history").diagnostics(),
                history
            );
        }
    }
}
