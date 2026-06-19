use super::*;

impl AppState {
    pub fn active_animation_property_path(&self, clip_id: ClipId) -> Option<&str> {
        self.animation_selection
            .active_property
            .as_ref()
            .filter(|selection| selection.clip_id == clip_id)
            .map(|selection| selection.path.as_str())
            .or_else(|| {
                self.animation_selection
                    .remembered_active_properties
                    .get(&clip_id)
                    .map(|path| path.as_str())
            })
    }

    pub fn set_active_animation_property(&mut self, clip_id: ClipId, path: impl Into<String>) {
        let path = path.into();
        let changed_clip = self
            .animation_selection
            .active_property
            .as_ref()
            .map(|selection| selection.clip_id != clip_id)
            .unwrap_or(false);
        if changed_clip {
            self.animation_selection.selected_keyframes.clear();
            self.animation_selection.bubble_host = None;
        }
        self.animation_selection
            .remembered_active_properties
            .insert(clip_id, path.clone());
        self.animation_selection.active_property =
            Some(AnimationPropertySelection { clip_id, path });
    }

    pub fn clear_animation_selection(&mut self) {
        self.animation_selection.active_property = None;
        self.animation_selection.selected_keyframes.clear();
        self.animation_selection.bubble_host = None;
    }

    pub fn animation_bubble_host(&self) -> Option<AnimationBubbleHost> {
        self.animation_selection.bubble_host
    }

    pub fn set_animation_bubble_host(&mut self, host: AnimationBubbleHost) {
        self.animation_selection.bubble_host = Some(host);
    }

    pub fn clear_animation_keyframe_selection_for_clip(&mut self, clip_id: ClipId) {
        self.animation_selection
            .selected_keyframes
            .retain(|selection| selection.clip_id != clip_id);
    }

    pub fn selected_animation_keyframes_for_clip(
        &self,
        clip_id: ClipId,
    ) -> Vec<AnimationKeyframeSelection> {
        self.animation_selection
            .selected_keyframes
            .iter()
            .filter(|selection| selection.clip_id == clip_id)
            .cloned()
            .collect()
    }

    pub fn available_animation_interpolation_presets(
        &self,
        selection: SelectedClipRef,
        presets: &[InterpolationType],
    ) -> Vec<InterpolationType> {
        let selected = self.selected_animation_keyframes_for_clip(selection.clip_id);
        if selected.is_empty() {
            return presets.to_vec();
        }

        let Some(clip) = self.clip_snapshot(selection) else {
            return presets.to_vec();
        };
        let Ok(property_bag) = clip.property_bag() else {
            return presets.to_vec();
        };

        let current = selected
            .iter()
            .filter_map(|item| {
                property_bag
                    .property(&item.path)
                    .and_then(|property| property.keyframe_at(item.time))
                    .map(|keyframe| {
                        interpolation_mode_from_keyframe(
                            keyframe.interp_in,
                            keyframe.interp_out,
                            keyframe.temporal_flags,
                        )
                    })
            })
            .collect::<Vec<_>>();

        if current.is_empty() {
            return presets.to_vec();
        }

        presets
            .iter()
            .copied()
            .filter(|preset| !current.iter().all(|current| current == preset))
            .collect()
    }

    pub fn selected_animation_interpolation_mode(
        &self,
        selection: SelectedClipRef,
    ) -> Option<InterpolationType> {
        let selected = self.selected_animation_keyframes_for_clip(selection.clip_id);
        if selected.is_empty() {
            return None;
        }

        let clip = self.clip_snapshot(selection)?;
        let property_bag = clip.property_bag().ok()?;
        let mut modes = selected
            .iter()
            .filter_map(|item| {
                property_bag
                    .property(&item.path)
                    .and_then(|property| property.keyframe_at(item.time))
                    .map(|keyframe| {
                        interpolation_mode_from_keyframe(
                            keyframe.interp_in,
                            keyframe.interp_out,
                            keyframe.temporal_flags,
                        )
                    })
            })
            .collect::<Vec<_>>();
        let first = modes.pop()?;
        if modes.into_iter().all(|mode| mode == first) {
            Some(first)
        } else {
            None
        }
    }

    pub fn is_animation_keyframe_selected(&self, selection: &AnimationKeyframeSelection) -> bool {
        self.animation_selection.selected_keyframes.contains(selection)
    }

    pub fn select_animation_keyframe_only(&mut self, selection: AnimationKeyframeSelection) {
        self.set_active_animation_property(selection.clip_id, selection.path.clone());
        self.animation_selection.selected_keyframes.clear();
        self.animation_selection.selected_keyframes.insert(selection);
    }

    pub fn toggle_animation_keyframe_selection(&mut self, selection: AnimationKeyframeSelection) {
        self.set_active_animation_property(selection.clip_id, selection.path.clone());
        if !self.animation_selection.selected_keyframes.insert(selection.clone()) {
            self.animation_selection.selected_keyframes.remove(&selection);
        }
    }

    pub fn set_animation_keyframe_selection(
        &mut self,
        selections: Vec<AnimationKeyframeSelection>,
    ) {
        if let Some(first) = selections.first() {
            self.set_active_animation_property(first.clip_id, first.path.clone());
        }
        self.animation_selection.selected_keyframes = selections.into_iter().collect();
    }

    pub fn retain_animation_keyframe_selection_for_clip(
        &mut self,
        clip_id: ClipId,
        valid_keys: &HashSet<(String, mondrian_core::automation::TimeTicks)>,
    ) {
        self.animation_selection.selected_keyframes.retain(|selection| {
            selection.clip_id != clip_id
                || valid_keys.contains(&(selection.path.clone(), selection.time))
        });
    }

    pub fn has_animation_clipboard(&self) -> bool {
        self.animation_clipboard
            .as_ref()
            .map(|clipboard| !clipboard.entries.is_empty())
            .unwrap_or(false)
    }

    pub fn copy_selected_animation_keyframes(
        &mut self,
        selection: SelectedClipRef,
    ) -> mondrian_core::Result<bool> {
        let selected = self.selected_animation_keyframes_for_clip(selection.clip_id);
        if selected.is_empty() {
            return Ok(false);
        }

        let clip = self.clip_snapshot(selection).ok_or_else(|| {
            mondrian_core::MondrianError::ClipNotFound { clip_id: selection.clip_id.to_string() }
        })?;
        let property_bag = clip.property_bag()?;
        let anchor_time = selected.iter().map(|item| item.time).min().unwrap_or(0);

        let mut entries = Vec::new();
        for item in selected {
            let Some(property) = property_bag.property(&item.path) else {
                continue;
            };
            let Some(mut keyframe) = property.keyframe_at(item.time) else {
                continue;
            };
            keyframe.id = KeyframeId::new();
            entries.push(AnimationClipboardEntry {
                path: item.path,
                relative_time: item.time - anchor_time,
                keyframe,
            });
        }

        entries.sort_by(|a, b| {
            a.relative_time.cmp(&b.relative_time).then_with(|| a.path.cmp(&b.path))
        });
        self.animation_clipboard = if entries.is_empty() {
            None
        } else {
            Some(AnimationClipboard { entries })
        };
        if self.has_animation_clipboard() {
            self.active_clipboard_kind = Some(AppClipboardKind::AnimationKeyframes);
        }
        Ok(self.has_animation_clipboard())
    }

    pub fn paste_animation_keyframes(
        &mut self,
        selection: SelectedClipRef,
        destination_time: TimeTicks,
    ) -> mondrian_core::Result<bool> {
        let Some(clipboard) = self.animation_clipboard.clone() else {
            return Ok(false);
        };
        if clipboard.entries.is_empty() {
            return Ok(false);
        }

        let mut mutations = Vec::with_capacity(clipboard.entries.len());
        let mut selections = Vec::with_capacity(clipboard.entries.len());

        for entry in clipboard.entries {
            let mut keyframe = entry.keyframe;
            keyframe.id = KeyframeId::new();
            keyframe.time = (destination_time + entry.relative_time).max(0);
            mutations.push(PropertyMutation::SetKeyframe {
                path: entry.path.clone(),
                keyframe: keyframe.clone(),
            });
            selections.push(AnimationKeyframeSelection {
                clip_id: selection.clip_id,
                path: entry.path,
                time: keyframe.time,
            });
        }

        self.mutate_clip_properties(selection, mutations, "粘贴关键帧")?;
        self.set_animation_keyframe_selection(selections);
        Ok(true)
    }

    pub fn clip_snapshot(&self, selection: SelectedClipRef) -> Option<Clip> {
        let seq = self.sequence.as_ref()?;
        find_clip_by_selection(seq, selection).cloned()
    }

    pub fn set_clip_media_interpretation(
        &mut self,
        selection: SelectedClipRef,
        interpretation: mondrian_timeline::clip::MediaInterpretation,
    ) -> mondrian_core::Result<bool> {
        let (sequence_id, before, after) = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "set_clip_media_interpretation".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;
            let Some((track_id, _is_video, is_locked)) =
                find_clip_track_lock(seq, selection.clip_id)
            else {
                return Err(mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                });
            };
            if is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track_id.to_string(),
                });
            }

            let before = seq.clone();
            let clip = find_clip_mut_by_selection(seq, selection).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                }
            })?;
            if clip.interpretation == interpretation {
                return Ok(false);
            }
            clip.interpretation = interpretation;
            (seq.id, before, seq.clone())
        };

        self.record_sequence_snapshot_command("解释素材", before, after);
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        let _ = self.save_project_file();
        Ok(true)
    }

    /// Directly set clip position for canvas drag.
    pub fn set_clip_position_direct(
        &mut self,
        selection: SelectedClipRef,
        pos: glam::Vec2,
    ) -> mondrian_core::Result<bool> {
        let (sequence_id, before, after) = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "set_clip_position".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;
            let before = seq.clone();
            let clip = find_clip_mut_by_selection(seq, selection).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                }
            })?;
            clip.transform.set_position(pos);
            (seq.id, before, seq.clone())
        };
        self.record_sequence_snapshot_command("move clip", before, after);
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        let _ = self.save_project_file();
        Ok(true)
    }

    /// Directly set clip anchor and adjust position to keep visual unchanged.
    pub fn set_clip_anchor_direct(
        &mut self,
        selection: SelectedClipRef,
        new_anchor: glam::Vec2,
    ) -> mondrian_core::Result<bool> {
        let (sequence_id, before, after) = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "set_clip_anchor".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;
            let before = seq.clone();
            let clip = find_clip_mut_by_selection(seq, selection).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                }
            })?;
            let old_anchor = clip.transform.get_anchor_point(TimeCode::ZERO);
            let scale = clip.transform.get_scale(TimeCode::ZERO);
            let old_pos = clip.transform.get_position(TimeCode::ZERO);
            // Adjust position to keep visual position unchanged:
            // pos_new + S*(-anchor_new) = pos_old + S*(-anchor_old)
            // pos_new = pos_old + S*(anchor_new - anchor_old)
            let new_pos = old_pos + scale * (new_anchor - old_anchor);
            clip.transform.set_anchor_point(new_anchor);
            clip.transform.set_position(new_pos);
            (seq.id, before, seq.clone())
        };
        self.record_sequence_snapshot_command("move anchor", before, after);
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        let _ = self.save_project_file();
        Ok(true)
    }

    /// Directly set clip scale for canvas drag.
    pub fn set_clip_scale_direct(
        &mut self,
        selection: SelectedClipRef,
        scale: glam::Vec2,
    ) -> mondrian_core::Result<bool> {
        let (sequence_id, before, after) = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "set_clip_scale".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;
            let before = seq.clone();
            let clip = find_clip_mut_by_selection(seq, selection).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                }
            })?;
            clip.transform.set_scale(scale);
            (seq.id, before, seq.clone())
        };
        self.record_sequence_snapshot_command("scale clip", before, after);
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        let _ = self.save_project_file();
        Ok(true)
    }

    pub fn mutate_clip_property(
        &mut self,
        selection: SelectedClipRef,
        mutation: PropertyMutation,
        description: impl Into<String>,
    ) -> mondrian_core::Result<bool> {
        self.mutate_clip_properties(selection, vec![mutation], description)
    }

    pub fn mutate_clip_properties(
        &mut self,
        selection: SelectedClipRef,
        mutations: Vec<PropertyMutation>,
        description: impl Into<String>,
    ) -> mondrian_core::Result<bool> {
        let description = description.into();
        if mutations.is_empty() {
            return Ok(false);
        }
        let (sequence_id, before, after) = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "mutate_clip_properties".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;
            let Some((track_id, _is_video, is_locked)) =
                find_clip_track_lock(seq, selection.clip_id)
            else {
                return Err(mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                });
            };
            if is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track_id.to_string(),
                });
            }

            let before = seq.clone();
            let clip = find_clip_mut_by_selection(seq, selection).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                }
            })?;
            for mutation in mutations {
                clip.apply_property_mutation(mutation)?;
            }
            (seq.id, before, seq.clone())
        };

        self.record_sequence_snapshot_command(description, before, after);
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        let _ = self.save_project_file();
        Ok(true)
    }

    pub fn add_effect_to_clip(
        &mut self,
        selection: SelectedClipRef,
        effect_type: EffectType,
    ) -> mondrian_core::Result<EffectId> {
        self.insert_effect_at_index(selection, effect_type, usize::MAX)
    }

    pub fn insert_effect_at_index(
        &mut self,
        selection: SelectedClipRef,
        effect_type: EffectType,
        index: usize,
    ) -> mondrian_core::Result<EffectId> {
        if !selection.is_video_track {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "add_effect_to_clip".to_string(),
                reason: "当前仅支持给视频类片段添加特效".to_string(),
            });
        }

        let (sequence_id, effect_id, before, after) = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "add_effect_to_clip".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;
            let before = seq.clone();
            let Some((track_id, _is_video, is_locked)) =
                find_clip_track_lock(seq, selection.clip_id)
            else {
                return Err(mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                });
            };
            if is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track_id.to_string(),
                });
            }

            let clip = find_clip_mut_by_selection(seq, selection).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                }
            })?;
            let effect = EffectNode::with_defaults(effect_type.clone());
            let effect_id = clip.insert_effect_node_at(index, effect);
            (seq.id, effect_id, before, seq.clone())
        };

        self.record_sequence_snapshot_command(
            format!("添加{}", effect_type.display_name()),
            before,
            after,
        );
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        let _ = self.save_project_file();
        Ok(effect_id)
    }

    pub fn set_clip_effect_enabled(
        &mut self,
        selection: SelectedClipRef,
        effect_id: EffectId,
        enabled: bool,
    ) -> mondrian_core::Result<bool> {
        let (sequence_id, before, after) = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "set_clip_effect_enabled".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;
            let Some((track_id, _is_video, is_locked)) =
                find_clip_track_lock(seq, selection.clip_id)
            else {
                return Err(mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                });
            };
            if is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track_id.to_string(),
                });
            }

            let clip = find_clip_by_selection(seq, selection).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                }
            })?;
            let effect =
                clip.effects.iter().find(|effect| effect.id == effect_id).ok_or_else(|| {
                    mondrian_core::MondrianError::WorkflowStepFailed {
                        step_id: "set_clip_effect_enabled".to_string(),
                        reason: format!("effect {effect_id} not found on clip"),
                    }
                })?;
            if effect.is_enabled == enabled {
                return Ok(false);
            }

            let before = seq.clone();
            let clip = find_clip_mut_by_selection(seq, selection).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                }
            })?;
            clip.set_effect_enabled(effect_id, enabled)?;
            (seq.id, before, seq.clone())
        };

        self.record_sequence_snapshot_command("切换特效启用状态", before, after);
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        let _ = self.save_project_file();
        Ok(true)
    }

    pub fn remove_effect_from_clip(
        &mut self,
        selection: SelectedClipRef,
        effect_id: EffectId,
    ) -> mondrian_core::Result<bool> {
        let (sequence_id, before, after) = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "remove_effect_from_clip".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;
            let before = seq.clone();
            let Some((track_id, _is_video, is_locked)) =
                find_clip_track_lock(seq, selection.clip_id)
            else {
                return Err(mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                });
            };
            if is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track_id.to_string(),
                });
            }

            let clip = find_clip_mut_by_selection(seq, selection).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                }
            })?;
            clip.remove_effect(effect_id)?;
            (seq.id, before, seq.clone())
        };

        self.record_sequence_snapshot_command("删除特效", before, after);
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        let _ = self.save_project_file();
        Ok(true)
    }

    pub fn reorder_effects_for_clip(
        &mut self,
        selection: SelectedClipRef,
        from: usize,
        to: usize,
    ) -> mondrian_core::Result<bool> {
        let (sequence_id, before, after) = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "reorder_effects_for_clip".to_string(),
                    reason: "当前无项目".to_string(),
                }
            })?;
            let Some((track_id, _is_video, is_locked)) =
                find_clip_track_lock(seq, selection.clip_id)
            else {
                return Err(mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                });
            };
            if is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: track_id.to_string(),
                });
            }

            let before = seq.clone();
            let clip = find_clip_mut_by_selection(seq, selection).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                }
            })?;
            if from >= clip.effects.len() {
                return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "reorder_effects_for_clip".to_string(),
                    reason: format!(
                        "effect index {from} is out of range for {} effects",
                        clip.effects.len()
                    ),
                });
            }
            let target_index = to.min(clip.effects.len().saturating_sub(1));
            if from == target_index {
                return Ok(false);
            }

            let effect = clip.effects.remove(from);
            clip.effects.insert(target_index, effect);
            (seq.id, before, seq.clone())
        };

        self.record_sequence_snapshot_command("调整特效顺序", before, after);
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        let _ = self.save_project_file();
        Ok(true)
    }
}
