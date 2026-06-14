use super::AppState;
use mondrian_core::types::{ClipId, TrackId};
use mondrian_timeline::sequence::Sequence;

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

impl AppState {
    /// The primary selected clip, used by single-target panels such as the
    /// Inspector and Effects browser.
    pub fn primary_selected_clip(&self) -> Option<SelectedClipRef> {
        self.selection.selected_clips.first().copied()
    }

    /// All selected clips in app selection order.
    pub fn selected_clips(&self) -> &[SelectedClipRef] {
        &self.selection.selected_clips
    }

    /// Clear all app-level selections.
    ///
    /// Clip, mask, and animation selections represent nested targeting scopes.
    /// Clearing selection resets all of them together so subsequent commands do
    /// not accidentally target stale sub-selection state.
    pub fn clear_selection(&mut self) {
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

    /// Replace the selected clip set and clear narrower selection scopes.
    pub fn replace_clip_selection(&mut self, selections: Vec<SelectedClipRef>) {
        self.selection.selected_clips = selections;
        self.selection.selected_mask = None;
        self.clear_animation_selection();
    }
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
