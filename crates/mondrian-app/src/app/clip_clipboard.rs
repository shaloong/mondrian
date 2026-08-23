use std::collections::{HashMap, HashSet};

use super::*;
use super::{apply_track_conflicts_for_focus_group, clip_selection_unit};

impl AppState {
    /// Whether `Copy` can place animation keyframes or timeline clips on the app clipboard.
    pub fn can_copy_to_app_clipboard(&self) -> bool {
        self.primary_selected_clip().is_some_and(|selection| {
            !self.selected_animation_keyframes_for_clip(selection.clip_id).is_empty()
        }) || self.selected_clip_clipboard_entries(false).is_some()
    }

    /// Whether `Cut` can remove the selected timeline clips through the app clipboard path.
    pub fn can_cut_to_app_clipboard(&self) -> bool {
        self.selected_clip_clipboard_entries(true).is_some()
    }

    /// Whether `Paste` can apply the currently active app clipboard to the active target.
    pub fn can_paste_from_app_clipboard(&self) -> bool {
        let can_paste_animation = self.has_animation_clipboard()
            && self.primary_selected_clip().is_some_and(|selection| {
                self.active_sequence()
                    .and_then(|seq| seq.clip_track_location(selection.clip_id))
                    .is_some_and(|location| !location.is_locked)
            });
        let can_paste_clips = self.active_sequence().is_some_and(|seq| {
            self.clip_clipboard.as_ref().is_some_and(|clipboard| {
                !clipboard.entries.is_empty()
                    && validate_clip_clipboard_targets(seq, &clipboard.entries).is_ok()
            })
        });
        match self.active_clipboard_kind {
            Some(AppClipboardKind::AnimationKeyframes) => can_paste_animation || can_paste_clips,
            Some(AppClipboardKind::Clips) => can_paste_clips,
            None => can_paste_animation || can_paste_clips,
        }
    }

    pub fn has_clip_clipboard(&self) -> bool {
        self.clip_clipboard
            .as_ref()
            .map(|clipboard| !clipboard.entries.is_empty())
            .unwrap_or(false)
    }

    pub fn copy_selected_clips_to_clipboard(&mut self) -> mondrian_core::Result<bool> {
        let Some(seq) = self.active_sequence() else {
            self.clip_clipboard = None;
            return Ok(false);
        };

        let selected_ids = self
            .selection
            .selected_clips
            .iter()
            .map(|selection| selection.clip_id)
            .collect::<Vec<_>>();
        let entries = collect_clip_clipboard_entries(seq, &selected_ids)?;
        let audio_transitions = collect_clipboard_audio_transitions(seq, &entries)?;
        let video_transitions = collect_clipboard_video_transitions(seq, &entries)?;
        self.clip_clipboard = if entries.is_empty() {
            None
        } else {
            Some(ClipClipboard { entries, audio_transitions, video_transitions })
        };
        if self.has_clip_clipboard() {
            self.active_clipboard_kind = Some(AppClipboardKind::Clips);
        }
        Ok(self.has_clip_clipboard())
    }

    fn selected_clip_clipboard_entries(
        &self,
        require_unlocked_targets: bool,
    ) -> Option<Vec<ClipClipboardEntry>> {
        let seq = self.active_sequence()?;
        let selected_ids = self
            .selection
            .selected_clips
            .iter()
            .map(|selection| selection.clip_id)
            .collect::<Vec<_>>();
        let entries = collect_clip_clipboard_entries(seq, &selected_ids).ok()?;
        if entries.is_empty() {
            return None;
        }
        if require_unlocked_targets && validate_clip_clipboard_targets(seq, &entries).is_err() {
            return None;
        }
        Some(entries)
    }

    pub fn cut_selected_clips_to_clipboard(&mut self) -> mondrian_core::Result<usize> {
        let Some(seq) = self.active_sequence() else {
            return Ok(0);
        };
        let selected_ids = self
            .selection
            .selected_clips
            .iter()
            .map(|selection| selection.clip_id)
            .collect::<Vec<_>>();
        let entries = collect_clip_clipboard_entries(seq, &selected_ids)?;
        if entries.is_empty() {
            return Ok(0);
        }
        validate_clip_clipboard_targets(seq, &entries)?;

        let selections = self
            .selection
            .selected_clips
            .iter()
            .map(|selection| {
                (
                    selection.track_id,
                    selection.is_video_track,
                    selection.clip_id,
                )
            })
            .collect::<Vec<_>>();
        let audio_transitions = collect_clipboard_audio_transitions(seq, &entries)?;
        let video_transitions = collect_clipboard_video_transitions(seq, &entries)?;
        self.clip_clipboard = Some(ClipClipboard { entries, audio_transitions, video_transitions });
        self.active_clipboard_kind = Some(AppClipboardKind::Clips);

        let removed = self.remove_clips_bulk(&selections, false)?;
        if removed > 0 {
            self.clear_selection();
        }
        Ok(removed)
    }

    pub fn paste_clip_clipboard_at_playhead(&mut self) -> mondrian_core::Result<usize> {
        let frame = self.current_frame().max(0);
        self.paste_clip_clipboard_at_frame(frame)
    }

    pub fn paste_clip_clipboard_at_frame(
        &mut self,
        timeline_frame: i64,
    ) -> mondrian_core::Result<usize> {
        let Some(clipboard) = self.clip_clipboard.clone() else {
            return Ok(0);
        };
        let sequence = self.active_sequence().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "paste_clip_clipboard".to_string(),
                reason: "当前无序列".to_string(),
            }
        })?;
        let destination = TimelineTime::from_frame_position(FramePosition::new(
            timeline_frame.max(0),
            sequence.time_base(),
        ))?;
        self.paste_clip_entries_at_time(
            clipboard.entries,
            clipboard.audio_transitions,
            clipboard.video_transitions,
            destination,
            "粘贴片段",
        )
    }

    pub fn duplicate_selected_clips_after_selection(&mut self) -> mondrian_core::Result<usize> {
        let Some(seq) = self.active_sequence() else {
            return Ok(0);
        };
        let selected_ids = self
            .selection
            .selected_clips
            .iter()
            .map(|selection| selection.clip_id)
            .collect::<Vec<_>>();
        let entries = collect_clip_clipboard_entries(seq, &selected_ids)?;
        if entries.is_empty() {
            return Ok(0);
        }
        validate_clip_clipboard_targets(seq, &entries)?;
        let destination = entries
            .iter()
            .map(|entry| entry.clip.end_position())
            .collect::<mondrian_core::Result<Vec<_>>>()?
            .into_iter()
            .max()
            .unwrap_or(seq.playhead);
        let audio_transitions = collect_clipboard_audio_transitions(seq, &entries)?;
        let video_transitions = collect_clipboard_video_transitions(seq, &entries)?;
        self.paste_clip_entries_at_time(
            entries,
            audio_transitions,
            video_transitions,
            destination,
            "复制片段",
        )
    }

    fn paste_clip_entries_at_time(
        &mut self,
        entries: Vec<ClipClipboardEntry>,
        audio_transitions: Vec<mondrian_timeline::audio::AudioTransition>,
        video_transitions: Vec<mondrian_timeline::VideoTransition>,
        destination: TimelineTime,
        description: &'static str,
    ) -> mondrian_core::Result<usize> {
        if entries.is_empty() {
            return Ok(0);
        }

        let mut pasted_selection = Vec::<SelectedClipRef>::with_capacity(entries.len());
        let sequence = self.active_sequence().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "paste_clip_clipboard".to_string(),
                reason: "当前无序列".to_string(),
            }
        })?;
        let sequence_id = sequence.id;
        let frame_rate = sequence.settings.frame_rate;
        let pasted_count = self.commit_sequence_edit(sequence_id, description, |seq| {
            validate_clip_clipboard_targets(seq, &entries)?;
            let destination = destination.max(TimelineTime::ZERO);
            let mut id_map = HashMap::<ClipId, ClipId>::new();
            let mut link_groups = HashMap::<ClipLinkGroupId, ClipLinkGroupId>::new();

            let mut focus_by_track = HashMap::<(TrackId, bool), HashSet<ClipId>>::new();
            let mut audio_edit_ids = HashMap::new();
            for entry in entries {
                let mut clip = entry.clip;
                clip.fork_visual_placement_identities();
                let new_id = clip.id;
                id_map.insert(entry.original_clip_id, new_id);
                clip.position =
                    destination.checked_add(entry.relative_start)?.max(TimelineTime::ZERO);
                clip.link_group =
                    clip.link_group.map(|group| *link_groups.entry(group).or_default());
                if !entry.is_video_track {
                    audio_edit_ids.extend(
                        seq.fork_audio_clip_authoring(&mut clip, &entry.audio_processing_scopes)?,
                    );
                }

                if entry.is_video_track {
                    let track = seq.video_track_mut(entry.track_id).ok_or_else(|| {
                        mondrian_core::MondrianError::TrackNotFound {
                            track_id: entry.track_id.to_string(),
                        }
                    })?;
                    track.add_clip(clip)?;
                } else {
                    let track = seq.audio_track_mut(entry.track_id).ok_or_else(|| {
                        mondrian_core::MondrianError::TrackNotFound {
                            track_id: entry.track_id.to_string(),
                        }
                    })?;
                    track.add_clip(clip)?;
                }
                focus_by_track
                    .entry((entry.track_id, entry.is_video_track))
                    .or_default()
                    .insert(new_id);
                pasted_selection.push(SelectedClipRef {
                    track_id: entry.track_id,
                    is_video_track: entry.is_video_track,
                    clip_id: new_id,
                });
            }

            for mut transition in audio_transitions {
                let (Some(left), Some(right)) = (
                    audio_edit_ids.get(&transition.left).copied(),
                    audio_edit_ids.get(&transition.right).copied(),
                ) else {
                    continue;
                };
                transition.id = mondrian_core::AudioTransitionId::new();
                transition.left = left;
                transition.right = right;
                transition.sequence_range = mondrian_core::TimelineTimeRange::new(
                    destination.checked_add(transition.sequence_range.start)?,
                    transition.sequence_range.duration,
                )?;
                seq.audio_program.transitions.push(transition);
            }

            for mut transition in video_transitions {
                let (Some(left), Some(right)) = (
                    id_map.get(&transition.left).copied(),
                    id_map.get(&transition.right).copied(),
                ) else {
                    continue;
                };
                transition.id = mondrian_core::VideoTransitionId::new();
                transition.left = left;
                transition.right = right;
                transition.properties.fork_author_identities();
                transition.sequence_range = mondrian_core::TimelineTimeRange::new(
                    destination.checked_add(transition.sequence_range.start)?,
                    transition.sequence_range.duration,
                )?;
                seq.video_transitions.push(transition);
            }

            for ((track_id, is_video_track), focus_ids) in &focus_by_track {
                let track = if *is_video_track {
                    seq.video_track_mut(*track_id)
                } else {
                    seq.audio_track_mut(*track_id)
                }
                .ok_or_else(|| mondrian_core::MondrianError::TrackNotFound {
                    track_id: track_id.to_string(),
                })?;
                apply_track_conflicts_for_focus_group(
                    track,
                    focus_ids,
                    ClipOverlapMode::Overwrite,
                )?;
            }
            seq.compact_structural_references();
            let pasted_count = pasted_selection.len();
            Ok(pasted_count)
        })?;

        if pasted_count > 0 {
            let seek_frame =
                destination.to_frame_position(frame_rate, FrameRounding::Nearest)?.frame.max(0);
            self.replace_clip_selection(pasted_selection);
            self.reconcile_playhead_after_committed_authoring_change(seek_frame, "paste_clipboard");
        }
        Ok(pasted_count)
    }
}

fn collect_clip_clipboard_entries(
    seq: &Sequence,
    selected_clip_ids: &[ClipId],
) -> mondrian_core::Result<Vec<ClipClipboardEntry>> {
    if selected_clip_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut ids = Vec::<ClipId>::new();
    let mut seen = HashSet::<ClipId>::new();
    for clip_id in selected_clip_ids {
        if seen.insert(*clip_id) {
            ids.push(*clip_id);
        }
        for member in clip_selection_unit(seq, *clip_id).unwrap_or_default() {
            if seen.insert(member) {
                ids.push(member);
            }
        }
    }

    let anchor = ids
        .iter()
        .filter_map(|clip_id| seq.find_clip(*clip_id).map(|clip| clip.position))
        .min()
        .unwrap_or(TimelineTime::ZERO);
    let mut entries = Vec::with_capacity(ids.len());
    for clip_id in ids {
        let location = seq.clip_track_location(clip_id).ok_or_else(|| {
            mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() }
        })?;
        let track_id = location.track_id;
        let is_video_track = location.is_video_track;
        let clip = seq
            .find_clip(clip_id)
            .ok_or_else(|| mondrian_core::MondrianError::ClipNotFound {
                clip_id: clip_id.to_string(),
            })?
            .clone();
        let audio_processing_scopes = if is_video_track {
            Vec::new()
        } else {
            let scope_ids = clip
                .audio_components
                .iter()
                .map(|edit| edit.processing.scope_id)
                .collect::<HashSet<_>>();
            seq.audio_program
                .processing_scopes
                .iter()
                .filter(|scope| scope_ids.contains(&scope.id))
                .cloned()
                .collect()
        };
        entries.push(ClipClipboardEntry {
            original_clip_id: clip_id,
            track_id,
            is_video_track,
            relative_start: clip.position.checked_sub(anchor)?,
            clip,
            audio_processing_scopes,
        });
    }

    entries.sort_by_key(|entry| {
        (
            entry.relative_start,
            !entry.is_video_track,
            entry.track_id.to_string(),
            entry.original_clip_id.to_string(),
        )
    });
    Ok(entries)
}

fn collect_clipboard_audio_transitions(
    seq: &Sequence,
    entries: &[ClipClipboardEntry],
) -> mondrian_core::Result<Vec<mondrian_timeline::audio::AudioTransition>> {
    let edit_ids = entries
        .iter()
        .flat_map(|entry| &entry.clip.audio_components)
        .map(|edit| edit.id)
        .collect::<HashSet<_>>();
    let anchor = entries
        .iter()
        .map(|entry| entry.clip.position)
        .min()
        .unwrap_or(TimelineTime::ZERO);
    seq.audio_program
        .transitions
        .iter()
        .filter(|transition| {
            edit_ids.contains(&transition.left) && edit_ids.contains(&transition.right)
        })
        .cloned()
        .map(|mut transition| {
            transition.sequence_range = mondrian_core::TimelineTimeRange::new(
                transition.sequence_range.start.checked_sub(anchor)?,
                transition.sequence_range.duration,
            )?;
            Ok(transition)
        })
        .collect()
}

fn collect_clipboard_video_transitions(
    seq: &Sequence,
    entries: &[ClipClipboardEntry],
) -> mondrian_core::Result<Vec<mondrian_timeline::VideoTransition>> {
    let clip_ids = entries.iter().map(|entry| entry.original_clip_id).collect::<HashSet<_>>();
    let anchor = entries
        .iter()
        .map(|entry| entry.clip.position)
        .min()
        .unwrap_or(TimelineTime::ZERO);
    seq.video_transitions
        .iter()
        .filter(|transition| {
            clip_ids.contains(&transition.left) && clip_ids.contains(&transition.right)
        })
        .cloned()
        .map(|mut transition| {
            transition.sequence_range = mondrian_core::TimelineTimeRange::new(
                transition.sequence_range.start.checked_sub(anchor)?,
                transition.sequence_range.duration,
            )?;
            Ok(transition)
        })
        .collect()
}

fn validate_clip_clipboard_targets(
    seq: &Sequence,
    entries: &[ClipClipboardEntry],
) -> mondrian_core::Result<()> {
    for entry in entries {
        let track = if entry.is_video_track {
            seq.video_tracks.iter().find(|track| track.id == entry.track_id)
        } else {
            seq.audio_tracks.iter().find(|track| track.id == entry.track_id)
        }
        .ok_or_else(|| mondrian_core::MondrianError::TrackNotFound {
            track_id: entry.track_id.to_string(),
        })?;
        if track.is_locked {
            return Err(mondrian_core::MondrianError::TrackLocked {
                track_id: entry.track_id.to_string(),
            });
        }
    }
    Ok(())
}
