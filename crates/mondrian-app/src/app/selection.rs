use super::AppState;
use mondrian_core::types::{ClipId, TrackId};
use mondrian_timeline::sequence::Sequence;
use std::collections::{HashMap, HashSet};

/// UI-agnostic reference to a selected clip in the active sequence.
///
/// This lives in the application state layer so legacy egui panels and
/// self-hosted UI adapters can share selection semantics without depending on
/// each other's widget modules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SelectedClipRef {
    pub track_id: TrackId,
    pub is_video_track: bool,
    pub clip_id: ClipId,
}

/// UI-agnostic reference to a selected timeline track in the active sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SelectedTrackRef {
    pub track_id: TrackId,
    pub is_video_track: bool,
}

impl AppState {
    /// The primary selected clip, used by single-target panels such as the
    /// Inspector and Effects browser.
    pub fn primary_selected_clip(&self) -> Option<SelectedClipRef> {
        let selection = self.selection.selected_clips.first().copied()?;
        let sequence = self.sequence.as_ref()?;
        resolve_clip_selection(sequence, selection.clip_id)
    }

    /// All selected clips in app selection order.
    pub fn selected_clips(&self) -> &[SelectedClipRef] {
        &self.selection.selected_clips
    }

    /// All selected timeline tracks in visible track order.
    pub fn selected_tracks(&self) -> &[TrackId] {
        &self.selection.selected_track_ids
    }

    /// Clear all app-level selections.
    ///
    /// Track, clip, mask, and animation selections represent nested targeting scopes.
    /// Clearing selection resets all of them together so subsequent commands do
    /// not accidentally target stale sub-selection state.
    pub fn clear_selection(&mut self) {
        self.selection.selected_track_ids.clear();
        self.selection.selected_clips.clear();
        self.selection.selected_mask = None;
        self.clear_animation_selection();
    }

    /// Select a clip by stable id in the active sequence.
    ///
    /// Clip ids are the authoritative selection input. Track ids stored in UI
    /// snapshots can be stale after moves, undo/redo, or model refresh lag, so
    /// selection resolves the current track from the sequence before mutating
    /// app state. Selecting a clip clears narrower mask and animation
    /// selections, matching desktop editor expectations.
    pub fn select_clip_by_id(&mut self, clip_id: ClipId) -> Option<SelectedClipRef> {
        let selection = self
            .sequence
            .as_ref()
            .and_then(|sequence| resolve_clip_selection(sequence, clip_id))?;
        self.replace_clip_selection(vec![selection]);
        Some(selection)
    }

    /// Select every clip in the active sequence in visible track order.
    pub fn select_all_clips(&mut self) {
        let Some(sequence) = self.sequence.as_ref() else {
            self.clear_selection();
            return;
        };

        let selections = all_clip_selections(sequence);
        self.replace_clip_selection(selections);
    }

    /// Select a timeline track by stable id in the active sequence.
    ///
    /// Track selection is a higher-level target than clip/mask/keyframe
    /// selection. Selecting a track clears narrower scopes so commands and
    /// panels cannot accidentally continue targeting a previously selected clip.
    pub fn select_track_by_id(&mut self, track_id: TrackId) -> Option<TrackId> {
        let sequence = self.sequence.as_ref()?;
        track_exists(sequence, track_id).then(|| {
            self.replace_track_selection(vec![track_id]);
            track_id
        })
    }

    /// Select every track in the active sequence in visible track order.
    pub fn select_all_tracks(&mut self) {
        let Some(sequence) = self.sequence.as_ref() else {
            self.clear_selection();
            return;
        };

        let tracks = all_track_ids(sequence);
        self.replace_track_selection(tracks);
    }

    /// Replace the selected clip set and clear narrower selection scopes.
    pub fn replace_clip_selection(&mut self, selections: Vec<SelectedClipRef>) {
        self.selection.selected_track_ids.clear();
        self.selection.selected_clips = selections;
        self.selection.selected_mask = None;
        self.clear_animation_selection();
    }

    /// Replace the selected track set and clear narrower selection scopes.
    pub fn replace_track_selection(&mut self, track_ids: Vec<TrackId>) {
        self.selection.selected_track_ids = track_ids;
        self.selection.selected_clips.clear();
        self.selection.selected_mask = None;
        self.clear_animation_selection();
    }

    /// Refresh selected clip track metadata after timeline mutations.
    ///
    /// Selection identity is the clip id. Track ids and clip kind are cached so
    /// UI and command code can target the active sequence quickly, but clip
    /// moves can make those cached fields stale.
    pub fn refresh_selected_clip_locations(&mut self, clip_ids: &[ClipId]) {
        let Some(sequence) = self.sequence.as_ref() else {
            return;
        };
        let updates = clip_ids
            .iter()
            .filter_map(|clip_id| resolve_clip_selection(sequence, *clip_id))
            .collect::<Vec<_>>();

        for selection in &mut self.selection.selected_clips {
            if let Some(updated) =
                updates.iter().find(|updated| updated.clip_id == selection.clip_id)
            {
                selection.track_id = updated.track_id;
                selection.is_video_track = updated.is_video_track;
            }
        }
    }

    /// Drop or refresh selection references that no longer exist in the active sequence.
    ///
    /// Timeline structure edits such as track removal can invalidate selected
    /// tracks, selected clips, masks, and animation keyframes. This method keeps
    /// selection state aligned with the authoritative sequence after those
    /// mutations, regardless of whether they came from legacy egui, self-hosted
    /// widgets, shortcuts, or scripts.
    pub fn prune_selection_to_active_sequence(&mut self) {
        let Some(sequence) = self.sequence.as_ref() else {
            self.clear_selection();
            return;
        };

        let valid_track_ids = all_track_ids(sequence).into_iter().collect::<HashSet<_>>();
        let clip_updates = all_clip_selections(sequence)
            .into_iter()
            .map(|selection| (selection.clip_id, selection))
            .collect::<HashMap<_, _>>();
        let valid_clip_ids = clip_updates.keys().copied().collect::<HashSet<_>>();

        self.selection
            .selected_track_ids
            .retain(|track_id| valid_track_ids.contains(track_id));
        self.selection.selected_clips = self
            .selection
            .selected_clips
            .iter()
            .filter_map(|selection| clip_updates.get(&selection.clip_id).copied())
            .collect();

        self.selection.selected_mask =
            self.selection.selected_mask.and_then(|(mask_id, clip_id, _track_id)| {
                clip_updates
                    .get(&clip_id)
                    .map(|selection| (mask_id, clip_id, selection.track_id))
            });

        if self
            .animation_selection
            .active_property
            .as_ref()
            .is_some_and(|selection| !valid_clip_ids.contains(&selection.clip_id))
        {
            self.animation_selection.active_property = None;
        }
        self.animation_selection
            .selected_keyframes
            .retain(|selection| valid_clip_ids.contains(&selection.clip_id));
        self.animation_selection
            .remembered_active_properties
            .retain(|clip_id, _| valid_clip_ids.contains(clip_id));
        if self.animation_selection.active_property.is_none()
            && self.animation_selection.selected_keyframes.is_empty()
        {
            self.animation_selection.bubble_host = None;
        }
    }
}

/// Resolve a track id to its current timeline-track selection reference.
pub fn resolve_track_selection(sequence: &Sequence, track_id: TrackId) -> Option<SelectedTrackRef> {
    if sequence.video_tracks.iter().any(|track| track.id == track_id) {
        return Some(SelectedTrackRef { track_id, is_video_track: true });
    }

    if sequence.audio_tracks.iter().any(|track| track.id == track_id) {
        return Some(SelectedTrackRef { track_id, is_video_track: false });
    }

    None
}

/// Return all track ids in video-track then audio-track order.
pub fn all_track_ids(sequence: &Sequence) -> Vec<TrackId> {
    sequence
        .video_tracks
        .iter()
        .map(|track| track.id)
        .chain(sequence.audio_tracks.iter().map(|track| track.id))
        .collect()
}

/// Return all clip selections in video-track then audio-track order.
pub fn all_clip_selections(sequence: &Sequence) -> Vec<SelectedClipRef> {
    sequence
        .video_tracks
        .iter()
        .flat_map(|track| {
            track.clips.iter().map(move |clip| SelectedClipRef {
                track_id: track.id,
                is_video_track: true,
                clip_id: clip.id,
            })
        })
        .chain(sequence.audio_tracks.iter().flat_map(|track| {
            track.clips.iter().map(move |clip| SelectedClipRef {
                track_id: track.id,
                is_video_track: false,
                clip_id: clip.id,
            })
        }))
        .collect()
}

/// Return whether a track id exists in the active sequence.
pub fn track_exists(sequence: &Sequence, track_id: TrackId) -> bool {
    resolve_track_selection(sequence, track_id).is_some()
}

/// Resolve a clip id to its current track-backed selection reference.
pub fn resolve_clip_selection(sequence: &Sequence, clip_id: ClipId) -> Option<SelectedClipRef> {
    for track in &sequence.video_tracks {
        if track.clips.iter().any(|clip| clip.id == clip_id) {
            return Some(SelectedClipRef { track_id: track.id, is_video_track: true, clip_id });
        }
    }

    for track in &sequence.audio_tracks {
        if track.clips.iter().any(|clip| clip.id == clip_id) {
            return Some(SelectedClipRef { track_id: track.id, is_video_track: false, clip_id });
        }
    }

    None
}
