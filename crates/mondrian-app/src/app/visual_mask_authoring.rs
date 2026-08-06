//! Transactional App Adapter for Clip-local visual Mask authoring.
//!
//! Product Actions carry stable identities only. This Module re-derives Track
//! placement and exact Clip-local time from the current Authoring Session,
//! enforces Track and Mask locks, and commits each mutation atomically.

use mondrian_core::mask_data::{MaskComponent, MaskEvaluation};
use mondrian_core::{ClipId, MaskId, MondrianError, SequenceId, TimelineTime, TrackId};
use mondrian_timeline::clip::Clip;
use mondrian_timeline::sequence::Sequence;

use super::product_action::*;
use super::AppState;

impl AppState {
    pub(super) fn dispatch_visual_mask_product_action(
        &mut self,
        action: VisualMaskProductAction,
    ) -> mondrian_core::Result<()> {
        match action {
            VisualMaskProductAction::AddToClip(payload) => self.add_visual_mask(payload),
            VisualMaskProductAction::Select(payload) => self.select_visual_mask(payload),
            VisualMaskProductAction::SetEnabled(payload) => {
                let context = self.visual_mask_context(
                    "visual_mask_set_enabled",
                    payload.clip_id,
                    Some(payload.mask_id),
                )?;
                ensure_track_unlocked("visual_mask_set_enabled", context)?;
                self.commit_sequence_edit(
                    context.sequence_id,
                    "修改蒙版启用状态",
                    |sequence| {
                        require_changed(
                            video_clip_mut(sequence, payload.clip_id)?
                                .set_mask_enabled(payload.mask_id, payload.enabled)?,
                            "visual_mask_set_enabled",
                            "Mask already has the requested enabled state",
                        )
                    },
                )
            }
            VisualMaskProductAction::SetLocked(payload) => {
                let context = self.visual_mask_context(
                    "visual_mask_set_locked",
                    payload.clip_id,
                    Some(payload.mask_id),
                )?;
                ensure_track_unlocked("visual_mask_set_locked", context)?;
                self.commit_sequence_edit(context.sequence_id, "修改蒙版编辑锁", |sequence| {
                    require_changed(
                        video_clip_mut(sequence, payload.clip_id)?
                            .set_mask_locked(payload.mask_id, payload.locked)?,
                        "visual_mask_set_locked",
                        "Mask already has the requested lock state",
                    )
                })
            }
            VisualMaskProductAction::Remove(payload) => {
                let context = self.visual_mask_context(
                    "visual_mask_remove",
                    payload.clip_id,
                    Some(payload.mask_id),
                )?;
                ensure_track_unlocked("visual_mask_remove", context)?;
                self.commit_sequence_edit(context.sequence_id, "删除蒙版", |sequence| {
                    video_clip_mut(sequence, payload.clip_id)?.remove_mask(payload.mask_id)
                })?;
                self.prune_selection_to_active_sequence();
                Ok(())
            }
            VisualMaskProductAction::Reorder(payload) => {
                let context = self.visual_mask_context(
                    "visual_mask_reorder",
                    payload.clip_id,
                    Some(payload.mask_id),
                )?;
                ensure_track_unlocked("visual_mask_reorder", context)?;
                self.commit_sequence_edit(context.sequence_id, "调整蒙版顺序", |sequence| {
                    require_changed(
                        video_clip_mut(sequence, payload.clip_id)?
                            .reorder_mask_relative(payload.mask_id, payload.placement)?,
                        "visual_mask_reorder",
                        "Mask stack already has the requested relative order",
                    )
                })
            }
            VisualMaskProductAction::SetShapeAnimationEnabled(payload) => {
                let context = self.visual_mask_context(
                    "visual_mask_set_shape_animation_enabled",
                    payload.clip_id,
                    Some(payload.mask_id),
                )?;
                ensure_track_unlocked("visual_mask_set_shape_animation_enabled", context)?;
                self.commit_sequence_edit(
                    context.sequence_id,
                    "修改蒙版形状动画",
                    |sequence| {
                        require_changed(
                            video_clip_mut(sequence, payload.clip_id)?
                                .set_mask_shape_animation_enabled(
                                    payload.mask_id,
                                    payload.enabled,
                                    context.clip_time,
                                )?,
                            "visual_mask_set_shape_animation_enabled",
                            "Mask shape animation already has the requested state",
                        )
                    },
                )
            }
            VisualMaskProductAction::WriteShape(payload) => {
                let context = self.visual_mask_context(
                    "visual_mask_write_shape",
                    payload.clip_id,
                    Some(payload.mask_id),
                )?;
                ensure_track_unlocked("visual_mask_write_shape", context)?;
                self.commit_sequence_edit(context.sequence_id, "修改蒙版形状", |sequence| {
                    let changed = video_clip_mut(sequence, payload.clip_id)?
                        .write_mask_shape(
                            payload.mask_id,
                            context.clip_time,
                            payload.shape,
                            payload.interpolation,
                        )?
                        .is_some();
                    require_changed(
                        changed,
                        "visual_mask_write_shape",
                        "Mask already has the requested shape at the current author time",
                    )
                })
            }
            VisualMaskProductAction::SetParameterValue(payload) => {
                let VisualMaskSetParameterValuePayload { clip_id, mask_id, parameter, value } =
                    *payload;
                let context = self.visual_mask_context(
                    "visual_mask_set_parameter_value",
                    clip_id,
                    Some(mask_id),
                )?;
                ensure_track_unlocked("visual_mask_set_parameter_value", context)?;
                let mutation = {
                    let sequence = self.active_sequence().ok_or_else(|| {
                        mask_action_error(
                            "visual_mask_set_parameter_value",
                            "there is no active Sequence",
                        )
                    })?;
                    video_clip(sequence, clip_id)?.prepare_mask_parameter_value(
                        mask_id,
                        &parameter,
                        context.clip_time,
                        value,
                    )?
                }
                .ok_or_else(|| {
                    mask_action_error(
                        "visual_mask_set_parameter_value",
                        "Mask parameter already has the requested value at the current author time",
                    )
                })?;
                self.commit_sequence_edit(context.sequence_id, "修改蒙版属性", |sequence| {
                    video_clip_mut(sequence, clip_id)?
                        .apply_mask_parameter_mutation(mask_id, mutation)
                })
            }
        }
    }

    fn add_visual_mask(
        &mut self,
        payload: VisualMaskAddToClipPayload,
    ) -> mondrian_core::Result<()> {
        let context = self.visual_mask_context("visual_mask_add_to_clip", payload.clip_id, None)?;
        ensure_track_unlocked("visual_mask_add_to_clip", context)?;
        let ordinal = self
            .active_sequence()
            .and_then(|sequence| video_clip(sequence, payload.clip_id).ok())
            .map_or(1, |clip| clip.masks.len() + 1);
        let mask = MaskComponent::new(
            format!("Mask {ordinal}"),
            MaskEvaluation { shape: payload.shape, ..Default::default() },
        );
        mask.validate_author_state()?;
        let mask_id = mask.id;
        self.commit_sequence_edit(context.sequence_id, "添加蒙版", |sequence| {
            video_clip_mut(sequence, payload.clip_id)?.add_mask_component(mask);
            Ok(())
        })?;
        self.select_mask_by_id(payload.clip_id, mask_id).ok_or_else(|| {
            mask_action_error(
                "visual_mask_add_to_clip",
                "the committed Mask could not be selected",
            )
        })?;
        Ok(())
    }

    fn select_visual_mask(
        &mut self,
        payload: VisualMaskTargetPayload,
    ) -> mondrian_core::Result<()> {
        self.visual_mask_context("visual_mask_select", payload.clip_id, Some(payload.mask_id))?;
        if self.selection.selected_mask.is_some_and(|(mask_id, clip_id, _)| {
            mask_id == payload.mask_id && clip_id == payload.clip_id
        }) {
            return Err(mask_action_error(
                "visual_mask_select",
                "Mask is already the active Inspector selection",
            ));
        }
        self.select_mask_by_id(payload.clip_id, payload.mask_id)
            .map(|_| ())
            .ok_or_else(|| {
                mask_action_error("visual_mask_select", "Mask no longer belongs to the Clip")
            })
    }

    fn visual_mask_context(
        &self,
        step_id: &'static str,
        clip_id: ClipId,
        mask_id: Option<MaskId>,
    ) -> mondrian_core::Result<VisualMaskContext> {
        let sequence = self
            .active_sequence()
            .ok_or_else(|| mask_action_error(step_id, "there is no active Sequence"))?;
        let sequence_time = self.current_timeline_time()?.unwrap_or(sequence.playhead);
        let (track_id, track_unlocked, clip) = sequence
            .video_tracks
            .iter()
            .find_map(|track| {
                track
                    .clips
                    .iter()
                    .find(|clip| clip.id == clip_id)
                    .map(|clip| (track.id, !track.is_locked, clip))
            })
            .ok_or_else(|| {
                mask_action_error(step_id, format!("video Clip does not exist: {clip_id}"))
            })?;
        if let Some(mask_id) = mask_id
            && clip.mask(mask_id).is_none()
        {
            return Err(mask_action_error(
                step_id,
                format!("Mask {mask_id} does not belong to Clip {clip_id}"),
            ));
        }
        let end = clip.end_position()?;
        let clip_time = clip.timeline_to_clip_time(sequence_time.clamp(clip.position, end))?;
        Ok(VisualMaskContext {
            sequence_id: sequence.id,
            track_id,
            track_unlocked,
            clip_time,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct VisualMaskContext {
    sequence_id: SequenceId,
    track_id: TrackId,
    track_unlocked: bool,
    clip_time: TimelineTime,
}

fn ensure_track_unlocked(
    step_id: &'static str,
    context: VisualMaskContext,
) -> mondrian_core::Result<()> {
    if context.track_unlocked {
        Ok(())
    } else {
        Err(mask_action_error(
            step_id,
            format!("Track is locked: {}", context.track_id),
        ))
    }
}

fn require_changed(
    changed: bool,
    step_id: &'static str,
    reason: &'static str,
) -> mondrian_core::Result<()> {
    if changed {
        Ok(())
    } else {
        Err(mask_action_error(step_id, reason))
    }
}

fn video_clip(sequence: &Sequence, clip_id: ClipId) -> mondrian_core::Result<&Clip> {
    sequence
        .video_tracks
        .iter()
        .flat_map(|track| &track.clips)
        .find(|clip| clip.id == clip_id)
        .ok_or_else(|| {
            mask_action_error(
                "visual_mask_authoring",
                format!("video Clip does not exist: {clip_id}"),
            )
        })
}

fn video_clip_mut(sequence: &mut Sequence, clip_id: ClipId) -> mondrian_core::Result<&mut Clip> {
    sequence
        .video_tracks
        .iter_mut()
        .flat_map(|track| &mut track.clips)
        .find(|clip| clip.id == clip_id)
        .ok_or_else(|| {
            mask_action_error(
                "visual_mask_authoring",
                format!("video Clip does not exist: {clip_id}"),
            )
        })
}

fn mask_action_error(step_id: &'static str, reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed { step_id: step_id.to_owned(), reason: reason.into() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::automation::PropertyValue;
    use mondrian_core::mask_data::{MaskShape, MaskShapeInterpolation, MASK_PROP_OPACITY};
    use mondrian_core::{AssetId, Rational};

    fn tt(frame: i64) -> TimelineTime {
        TimelineTime::new(frame, 25).expect("valid test time")
    }

    fn mask_test_state() -> (AppState, ClipId, TrackId) {
        let mut sequence = Sequence::new("Mask authoring");
        sequence.settings.frame_rate = Rational::new(25, 1);
        sequence.playhead = tt(15);
        sequence.add_video_track();
        let track_id = sequence.video_tracks[0].id;
        let clip = Clip::new(AssetId::new(), tt(10), tt(20)).expect("valid Clip");
        let clip_id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("add Clip");
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        state.seek(15).expect("seek to author time");
        (state, clip_id, track_id)
    }

    fn dispatch(
        state: &mut AppState,
        action: VisualMaskProductAction,
    ) -> mondrian_core::Result<()> {
        state.dispatch_action(ProductAction::VisualMask(action).into_external_action())
    }

    fn mask(state: &AppState, clip_id: ClipId, mask_id: MaskId) -> &MaskComponent {
        state
            .active_sequence()
            .and_then(|sequence| video_clip(sequence, clip_id).ok())
            .and_then(|clip| clip.mask(mask_id))
            .expect("Mask")
    }

    #[test]
    fn mask_product_slice_preserves_shape_key_identity_across_undo_redo() {
        let (mut state, clip_id, _) = mask_test_state();
        dispatch(
            &mut state,
            VisualMaskProductAction::AddToClip(VisualMaskAddToClipPayload {
                clip_id,
                shape: MaskShape::default(),
            }),
        )
        .expect("add Mask");
        let mask_id = state.selection.selected_mask.expect("selected Mask").0;
        let initial_key_id = mask(&state, clip_id, mask_id).shape_keyframes[0].id;

        dispatch(
            &mut state,
            VisualMaskProductAction::SetShapeAnimationEnabled(
                VisualMaskSetShapeAnimationEnabledPayload { clip_id, mask_id, enabled: true },
            ),
        )
        .expect("enable shape animation");
        dispatch(
            &mut state,
            VisualMaskProductAction::WriteShape(VisualMaskWriteShapePayload {
                clip_id,
                mask_id,
                shape: MaskShape::Ellipse {
                    center: glam::Vec2::new(0.5, 0.5),
                    radii: glam::Vec2::new(0.2, 0.3),
                },
                interpolation: MaskShapeInterpolation::Hold,
            }),
        )
        .expect("write exact-time shape");
        let written_key_id = mask(&state, clip_id, mask_id)
            .shape_keyframes
            .iter()
            .find(|key| key.time == tt(5))
            .expect("exact Clip-local shape key")
            .id;
        assert_ne!(written_key_id, initial_key_id);

        assert!(state.undo_timeline().expect("undo shape write"));
        assert_eq!(mask(&state, clip_id, mask_id).shape_keyframes.len(), 1);
        assert!(state.redo_timeline().expect("redo shape write"));
        assert_eq!(
            mask(&state, clip_id, mask_id)
                .shape_keyframes
                .iter()
                .find(|key| key.time == tt(5))
                .expect("restored key")
                .id,
            written_key_id
        );
    }

    #[test]
    fn mask_product_slice_enforces_track_and_mask_locks_and_stable_parameters() {
        let (mut state, clip_id, track_id) = mask_test_state();
        dispatch(
            &mut state,
            VisualMaskProductAction::AddToClip(VisualMaskAddToClipPayload {
                clip_id,
                shape: MaskShape::default(),
            }),
        )
        .expect("add Mask");
        let mask_id = state.selection.selected_mask.expect("selected Mask").0;
        let parameter = mask(&state, clip_id, mask_id)
            .properties
            .property(MASK_PROP_OPACITY)
            .map(|property| property.address())
            .expect("stable opacity address");
        dispatch(
            &mut state,
            VisualMaskProductAction::SetParameterValue(Box::new(
                VisualMaskSetParameterValuePayload {
                    clip_id,
                    mask_id,
                    parameter: parameter.clone(),
                    value: PropertyValue::Float(0.35),
                },
            )),
        )
        .expect("write Mask opacity");
        dispatch(
            &mut state,
            VisualMaskProductAction::SetLocked(VisualMaskSetLockedPayload {
                clip_id,
                mask_id,
                locked: true,
            }),
        )
        .expect("lock Mask");
        assert!(dispatch(
            &mut state,
            VisualMaskProductAction::SetParameterValue(Box::new(
                VisualMaskSetParameterValuePayload {
                    clip_id,
                    mask_id,
                    parameter,
                    value: PropertyValue::Float(0.5),
                },
            )),
        )
        .is_err());

        state
            .commit_active_sequence_edit("lock Track", |sequence| {
                sequence.video_tracks[0].is_locked = true;
                Ok(())
            })
            .expect("lock Track");
        assert!(dispatch(
            &mut state,
            VisualMaskProductAction::SetLocked(VisualMaskSetLockedPayload {
                clip_id,
                mask_id,
                locked: false,
            }),
        )
        .is_err());
        assert_eq!(
            state.selection.selected_mask,
            Some((mask_id, clip_id, track_id))
        );
    }
}
