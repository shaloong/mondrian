use std::collections::{HashMap, HashSet};

use super::*;
use crate::app::timeline_editing::{
    apply_track_conflicts_for_focus_group, clear_broken_links, find_clip, find_clip_track_lock,
};

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
        let can_paste_animation =
            self.has_animation_clipboard() && self.primary_selected_clip().is_some();
        let can_paste_clips = self.has_clip_clipboard() && self.sequence.is_some();
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
        let Some(seq) = self.sequence.as_ref() else {
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
        self.clip_clipboard = if entries.is_empty() {
            None
        } else {
            Some(ClipClipboard { entries })
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
        let seq = self.sequence.as_ref()?;
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
        let Some(seq) = self.sequence.as_ref() else {
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
        self.clip_clipboard = Some(ClipClipboard { entries });
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
        self.paste_clip_entries_at_frame(clipboard.entries, timeline_frame, "粘贴片段")
    }

    pub fn duplicate_selected_clips_after_selection(&mut self) -> mondrian_core::Result<usize> {
        let Some(seq) = self.sequence.as_ref() else {
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
            .map(|entry| entry.clip.position.frame.saturating_add(entry.clip.duration.frame.max(0)))
            .max()
            .unwrap_or_else(|| self.current_frame().max(0));
        self.paste_clip_entries_at_frame(entries, destination, "复制片段")
    }

    fn paste_clip_entries_at_frame(
        &mut self,
        entries: Vec<ClipClipboardEntry>,
        timeline_frame: i64,
        description: &'static str,
    ) -> mondrian_core::Result<usize> {
        if entries.is_empty() {
            return Ok(0);
        }

        let mut pasted_selection = Vec::<SelectedClipRef>::with_capacity(entries.len());
        let (pasted_count, sequence_id, before, after) = {
            let seq = self.sequence.as_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "paste_clip_clipboard".to_string(),
                    reason: "当前无序列".to_string(),
                }
            })?;

            validate_clip_clipboard_targets(seq, &entries)?;
            let before = seq.clone();
            let destination = timeline_frame.max(0);
            let mut id_map = HashMap::<ClipId, ClipId>::new();
            for entry in &entries {
                id_map.insert(entry.original_clip_id, ClipId::new());
            }

            let mut focus_by_track = HashMap::<(TrackId, bool), HashSet<ClipId>>::new();
            for entry in entries {
                let new_id = id_map
                    .get(&entry.original_clip_id)
                    .copied()
                    .expect("new id should be assigned for every clipboard entry");
                let mut clip = entry.clip;
                clip.id = new_id;
                clip.position = TimeCode::new(
                    destination.saturating_add(entry.relative_start_frame).max(0),
                    clip.position.time_base,
                );
                clip.linked_clip = clip.linked_clip.and_then(|linked| id_map.get(&linked).copied());

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

            for ((track_id, is_video_track), focus_ids) in &focus_by_track {
                let track = if *is_video_track {
                    seq.video_track_mut(*track_id)
                } else {
                    seq.audio_track_mut(*track_id)
                }
                .ok_or_else(|| mondrian_core::MondrianError::TrackNotFound {
                    track_id: track_id.to_string(),
                })?;
                apply_track_conflicts_for_focus_group(track, focus_ids, ClipOverlapMode::Overwrite);
            }
            clear_broken_links(seq);
            let sequence_id = seq.id;
            let after = seq.clone();
            let pasted_count = pasted_selection.len();
            (pasted_count, sequence_id, before, after)
        };

        if pasted_count > 0 {
            self.record_sequence_snapshot_command(description, before, after);
            self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
            self.replace_clip_selection(pasted_selection);
            self.seek(timeline_frame.max(0));
            let _ = self.save_project_file();
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
        if let Some(linked_id) = find_clip(seq, *clip_id).and_then(|clip| clip.linked_clip) {
            if seen.insert(linked_id) {
                ids.push(linked_id);
            }
        }
    }

    let anchor_frame = ids
        .iter()
        .filter_map(|clip_id| find_clip(seq, *clip_id).map(|clip| clip.position.frame))
        .min()
        .unwrap_or(0);
    let mut entries = Vec::with_capacity(ids.len());
    for clip_id in ids {
        let (track_id, is_video_track, _) =
            find_clip_track_lock(seq, clip_id).ok_or_else(|| {
                mondrian_core::MondrianError::ClipNotFound { clip_id: clip_id.to_string() }
            })?;
        let clip = find_clip(seq, clip_id)
            .ok_or_else(|| mondrian_core::MondrianError::ClipNotFound {
                clip_id: clip_id.to_string(),
            })?
            .clone();
        entries.push(ClipClipboardEntry {
            original_clip_id: clip_id,
            track_id,
            is_video_track,
            relative_start_frame: clip.position.frame - anchor_frame,
            clip,
        });
    }

    entries.sort_by_key(|entry| {
        (
            entry.relative_start_frame,
            !entry.is_video_track,
            entry.track_id.to_string(),
            entry.original_clip_id.to_string(),
        )
    });
    Ok(entries)
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
