use super::*;

impl AppState {
    pub fn active_animation_property_address(
        &self,
        clip_id: ClipId,
    ) -> Option<&AnimationParameterAddress> {
        self.animation_selection
            .active_property
            .as_ref()
            .filter(|selection| selection.clip_id == clip_id)
            .map(|selection| &selection.property)
            .or_else(|| self.animation_selection.remembered_active_properties.get(&clip_id))
    }

    pub fn active_animation_property_path(&self, clip_id: ClipId) -> Option<String> {
        let address = self.active_animation_property_address(clip_id)?;
        let sequence = self.active_sequence()?;
        let clip = sequence.find_clip(clip_id)?;
        clip.property_bag()
            .ok()?
            .property_by_address(address)
            .map(|(path, _)| path.to_owned())
    }

    pub fn set_active_animation_property(
        &mut self,
        clip_id: ClipId,
        property: AnimationParameterAddress,
    ) {
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
            .insert(clip_id, property.clone());
        self.animation_selection.active_property =
            Some(AnimationPropertySelection { clip_id, property });
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
            .retain(|selection| selection.property.clip_id != clip_id);
    }

    pub fn selected_animation_keyframes_for_clip(
        &self,
        clip_id: ClipId,
    ) -> Vec<AnimationKeyframeSelection> {
        self.animation_selection
            .selected_keyframes
            .iter()
            .filter(|selection| selection.property.clip_id == clip_id)
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
            .filter_map(|selection| {
                property_bag
                    .property_by_address(&selection.property.property)
                    .and_then(|(_, property)| property.keyframe_by_id(selection.keyframe_id))
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
            .filter_map(|selection| {
                property_bag
                    .property_by_address(&selection.property.property)
                    .and_then(|(_, property)| property.keyframe_by_id(selection.keyframe_id))
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
        self.set_active_animation_property(
            selection.property.clip_id,
            selection.property.property.clone(),
        );
        self.animation_selection.selected_keyframes.clear();
        self.animation_selection.selected_keyframes.insert(selection);
    }

    pub fn toggle_animation_keyframe_selection(&mut self, selection: AnimationKeyframeSelection) {
        self.set_active_animation_property(
            selection.property.clip_id,
            selection.property.property.clone(),
        );
        if !self.animation_selection.selected_keyframes.insert(selection.clone()) {
            self.animation_selection.selected_keyframes.remove(&selection);
        }
    }

    pub fn set_animation_keyframe_selection(
        &mut self,
        selections: Vec<AnimationKeyframeSelection>,
    ) {
        if let Some(first) = selections.first() {
            self.set_active_animation_property(
                first.property.clip_id,
                first.property.property.clone(),
            );
        }
        self.animation_selection.selected_keyframes = selections.into_iter().collect();
    }

    pub fn retain_animation_keyframe_selection_for_clip(
        &mut self,
        clip_id: ClipId,
        valid_keys: &HashSet<KeyframeId>,
    ) {
        self.animation_selection.selected_keyframes.retain(|selection| {
            selection.property.clip_id != clip_id || valid_keys.contains(&selection.keyframe_id)
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
        let mut resolved = Vec::new();
        for selection in selected {
            let Some((_, property)) =
                property_bag.property_by_address(&selection.property.property)
            else {
                continue;
            };
            let Some(keyframe) = property.keyframe_by_id(selection.keyframe_id) else {
                continue;
            };
            resolved.push((selection, keyframe));
        }
        let anchor_time = resolved
            .iter()
            .map(|(_, keyframe)| keyframe.time)
            .min()
            .unwrap_or(TimelineTime::ZERO);

        let mut entries = Vec::new();
        for (selection, mut keyframe) in resolved {
            keyframe.id = KeyframeId::new();
            entries.push(AnimationClipboardEntry {
                property: selection.property.property,
                relative_time: keyframe.time.checked_sub(anchor_time)?,
                keyframe,
            });
        }

        entries.sort_by(|a, b| {
            a.relative_time.cmp(&b.relative_time).then_with(|| a.property.cmp(&b.property))
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
        destination_clip_time: TimelineTime,
    ) -> mondrian_core::Result<bool> {
        let Some(clipboard) = self.animation_clipboard.clone() else {
            return Ok(false);
        };
        if clipboard.entries.is_empty() {
            return Ok(false);
        }

        let clip = self.clip_snapshot(selection).ok_or_else(|| {
            mondrian_core::MondrianError::ClipNotFound { clip_id: selection.clip_id.to_string() }
        })?;
        let property_bag = clip.property_bag()?;
        let active_property = self.active_animation_property_address(selection.clip_id).cloned();
        let mut mutations = Vec::with_capacity(clipboard.entries.len());
        let mut selections = Vec::with_capacity(clipboard.entries.len());

        for entry in clipboard.entries {
            let destination_property =
                if property_bag.property_by_address(&entry.property).is_some() {
                    entry.property
                } else if let Some(active) = active_property.as_ref().filter(|active| {
                    active.parameter_id == entry.property.parameter_id
                        && property_bag.property_by_address(active).is_some()
                }) {
                    active.clone()
                } else {
                    property_bag
                        .unique_address_for_parameter_id(&entry.property.parameter_id)
                        .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
                            step_id: "paste_animation_keyframes".to_owned(),
                            reason: format!(
                                "参数 {} 在目标 Clip 中缺失或存在多个实例",
                                entry.property.parameter_id
                            ),
                        })?
                };
            let (path, _) =
                property_bag.property_by_address(&destination_property).ok_or_else(|| {
                    mondrian_core::MondrianError::WorkflowStepFailed {
                        step_id: "paste_animation_keyframes".to_owned(),
                        reason: format!(
                            "目标参数实例 {} / {} 已失效",
                            destination_property.animation_track_id,
                            destination_property.parameter_id
                        ),
                    }
                })?;
            let mut keyframe = entry.keyframe;
            keyframe.id = KeyframeId::new();
            keyframe.time = destination_clip_time.checked_add(entry.relative_time)?;
            mutations.push(PropertyMutation::SetKeyframe {
                path: path.to_owned(),
                keyframe: keyframe.clone(),
            });
            selections.push(AnimationKeyframeSelection {
                property: AnimationPropertySelection {
                    clip_id: selection.clip_id,
                    property: destination_property,
                },
                keyframe_id: keyframe.id,
            });
        }

        self.mutate_clip_properties(selection, mutations, "粘贴关键帧")?;
        self.set_animation_keyframe_selection(selections);
        Ok(true)
    }

    pub fn clip_snapshot(&self, selection: SelectedClipRef) -> Option<Clip> {
        let seq = self.active_sequence()?;
        find_clip_by_selection(seq, selection).cloned()
    }

    pub fn set_clip_media_interpretation(
        &mut self,
        selection: SelectedClipRef,
        interpretation: mondrian_timeline::clip::MediaInterpretation,
    ) -> mondrian_core::Result<bool> {
        let changed = self.commit_active_sequence_edit("解释素材", |seq| {
            let Some(location) = seq.clip_track_location(selection.clip_id) else {
                return Err(mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                });
            };
            if location.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: location.track_id.to_string(),
                });
            }

            let clip = find_clip_mut_by_selection(seq, selection).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                }
            })?;
            let current = clip.media_interpretation_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "set_clip_media_interpretation".to_string(),
                    reason: "只有媒体 Clip 可以设置素材解释".to_string(),
                }
            })?;
            if *current == interpretation {
                return Ok(false);
            }
            *current = interpretation;
            Ok(true)
        })?;
        if !changed {
            return Ok(false);
        }
        Ok(true)
    }

    /// Directly set clip position for canvas drag.
    pub fn set_clip_position_direct(
        &mut self,
        selection: SelectedClipRef,
        pos: glam::Vec2,
    ) -> mondrian_core::Result<bool> {
        let _sequence_id = self.commit_active_sequence_edit("move clip", |seq| {
            let clip = find_clip_mut_by_selection(seq, selection).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                }
            })?;
            clip.transform.set_position(pos);
            Ok(seq.id)
        })?;
        Ok(true)
    }

    /// Directly set clip anchor and adjust position to keep visual unchanged.
    pub fn set_clip_anchor_direct(
        &mut self,
        selection: SelectedClipRef,
        new_anchor: glam::Vec2,
    ) -> mondrian_core::Result<bool> {
        let _sequence_id = self.commit_active_sequence_edit("move anchor", |seq| {
            let clip = find_clip_mut_by_selection(seq, selection).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                }
            })?;
            let old_anchor = clip.transform.get_anchor_point(TimelineTime::ZERO);
            let scale = clip.transform.get_scale(TimelineTime::ZERO);
            let old_pos = clip.transform.get_position(TimelineTime::ZERO);
            // Adjust position to keep visual position unchanged:
            // pos_new + S*(-anchor_new) = pos_old + S*(-anchor_old)
            // pos_new = pos_old + S*(anchor_new - anchor_old)
            let new_pos = old_pos + scale * (new_anchor - old_anchor);
            clip.transform.set_anchor_point(new_anchor);
            clip.transform.set_position(new_pos);
            Ok(seq.id)
        })?;
        Ok(true)
    }

    /// Directly set clip scale for canvas drag.
    pub fn set_clip_scale_direct(
        &mut self,
        selection: SelectedClipRef,
        scale: glam::Vec2,
    ) -> mondrian_core::Result<bool> {
        let _sequence_id = self.commit_active_sequence_edit("scale clip", |seq| {
            let clip = find_clip_mut_by_selection(seq, selection).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                }
            })?;
            clip.transform.set_scale(scale);
            Ok(seq.id)
        })?;
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
        let _sequence_id = self.commit_active_sequence_edit(description, |seq| {
            let Some(location) = seq.clip_track_location(selection.clip_id) else {
                return Err(mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                });
            };
            if location.is_locked {
                return Err(mondrian_core::MondrianError::TrackLocked {
                    track_id: location.track_id.to_string(),
                });
            }

            let clip = find_clip_mut_by_selection(seq, selection).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound {
                    clip_id: selection.clip_id.to_string(),
                }
            })?;
            for mutation in mutations {
                clip.apply_property_mutation(mutation)?;
            }
            Ok(seq.id)
        })?;
        Ok(true)
    }

    pub fn add_effect_to_clip(
        &mut self,
        clip_id: ClipId,
        effect_type: EffectType,
    ) -> mondrian_core::Result<EffectId> {
        let sequence = active_sequence_for_visual_effect(self, "add_effect_to_clip")?;
        authorable_visual_effect_clip(sequence, clip_id, "add_effect_to_clip")?;
        let effect =
            mondrian_effects::instantiate_effect_node(effect_type.clone()).map_err(|error| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "add_effect_to_clip".to_owned(),
                    reason: error.to_string(),
                }
            })?;
        let description = format!("添加{}", effect_type.display_name());
        let (_sequence_id, effect_id) =
            self.commit_active_sequence_edit(description, move |seq| {
                let clip = seq.find_clip_mut(clip_id).ok_or_else(|| {
                    mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() }
                })?;
                let effect_id = clip.add_effect_node(effect);
                Ok((seq.id, effect_id))
            })?;
        Ok(effect_id)
    }

    pub fn set_clip_effect_enabled(
        &mut self,
        clip_id: ClipId,
        effect_id: EffectId,
        enabled: bool,
    ) -> mondrian_core::Result<bool> {
        let sequence = active_sequence_for_visual_effect(self, "set_clip_effect_enabled")?;
        let clip = authorable_visual_effect_clip(sequence, clip_id, "set_clip_effect_enabled")?;
        let current = clip.effect_enabled(effect_id).ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "set_clip_effect_enabled".to_owned(),
                reason: format!("Effect {effect_id} does not belong to Clip {clip_id}"),
            }
        })?;
        if current == enabled {
            return Ok(false);
        }
        let _sequence_id = self.commit_active_sequence_edit("切换特效启用状态", |seq| {
            let clip = seq.find_clip_mut(clip_id).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() }
            })?;
            clip.set_effect_enabled(effect_id, enabled)?;
            Ok(seq.id)
        })?;
        Ok(true)
    }

    pub fn remove_effect_from_clip(
        &mut self,
        clip_id: ClipId,
        effect_id: EffectId,
    ) -> mondrian_core::Result<bool> {
        let sequence = active_sequence_for_visual_effect(self, "remove_effect_from_clip")?;
        let clip = authorable_visual_effect_clip(sequence, clip_id, "remove_effect_from_clip")?;
        if !clip.effects.iter().any(|effect| effect.id == effect_id) {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "remove_effect_from_clip".to_owned(),
                reason: format!("Effect {effect_id} does not belong to Clip {clip_id}"),
            });
        }
        let _sequence_id = self.commit_active_sequence_edit("删除特效", |seq| {
            let clip = seq.find_clip_mut(clip_id).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() }
            })?;
            clip.remove_effect(effect_id)?;
            Ok(seq.id)
        })?;
        Ok(true)
    }

    pub fn reorder_effect_for_clip(
        &mut self,
        clip_id: ClipId,
        effect_id: EffectId,
        placement: mondrian_timeline::EffectRelativePlacement,
    ) -> mondrian_core::Result<bool> {
        let sequence = active_sequence_for_visual_effect(self, "reorder_effect_for_clip")?;
        let clip = authorable_visual_effect_clip(sequence, clip_id, "reorder_effect_for_clip")?;
        if !clip.effect_relative_placement_would_change(effect_id, placement)? {
            return Ok(false);
        }
        let _sequence_id = self.commit_active_sequence_edit("调整特效顺序", |seq| {
            let clip = seq.find_clip_mut(clip_id).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() }
            })?;
            clip.reorder_effect_relative(effect_id, placement)?;
            Ok(seq.id)
        })?;
        Ok(true)
    }
}

fn active_sequence_for_visual_effect<'a>(
    state: &'a AppState,
    step_id: &str,
) -> mondrian_core::Result<&'a Sequence> {
    state
        .active_sequence()
        .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: step_id.to_owned(),
            reason: "no active Sequence".to_owned(),
        })
}

fn authorable_visual_effect_clip<'a>(
    sequence: &'a Sequence,
    clip_id: ClipId,
    step_id: &str,
) -> mondrian_core::Result<&'a Clip> {
    let location = sequence.clip_track_location(clip_id).ok_or_else(|| {
        mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() }
    })?;
    if !location.is_video_track {
        return Err(mondrian_core::MondrianError::WorkflowStepFailed {
            step_id: step_id.to_owned(),
            reason: "visual Effects require a video Clip".to_owned(),
        });
    }
    if location.is_locked {
        return Err(mondrian_core::MondrianError::TrackLocked {
            track_id: location.track_id.to_string(),
        });
    }
    sequence
        .find_clip(clip_id)
        .ok_or_else(|| mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() })
}
