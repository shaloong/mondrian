//! Product authoring operations for generated Basic Title Clips.

use super::*;
use mondrian_core::default_basic_title_font_family;

const DEFAULT_BASIC_TITLE_DURATION_SECS: f64 = 5.0;

impl AppState {
    /// Create one Basic Title at the current edit range as a single author transaction.
    ///
    /// Placement prefers an explicitly selected, unlocked video Track when it
    /// has room, then the highest unlocked non-overlapping Track. If every
    /// existing Track is occupied, the new Track and Clip are committed
    /// together so one Undo removes both.
    pub fn create_basic_title_at_playhead(&mut self) -> mondrian_core::Result<ClipId> {
        let sequence = self.active_sequence().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "create_basic_title".to_owned(),
                reason: "当前没有活动序列".to_owned(),
            }
        })?;
        let preferred_track = self
            .selected_tracks()
            .iter()
            .copied()
            .find(|track_id| sequence.video_tracks.iter().any(|track| track.id == *track_id))
            .or_else(|| {
                self.primary_selected_clip()
                    .filter(|selection| selection.is_video_track)
                    .map(|selection| selection.track_id)
            });
        let in_frame = self.in_point_frame()?;
        let selected_range = self.out_point_frame()?.filter(|out| *out > in_frame);
        let start_frame =
            selected_range.map(|_| in_frame).unwrap_or_else(|| self.current_frame().max(0));
        let duration_frames = selected_range
            .map(|out| out.saturating_sub(in_frame))
            .unwrap_or_else(|| {
                let fps = sequence.settings.frame_rate.to_f64();
                (DEFAULT_BASIC_TITLE_DURATION_SECS * fps).round() as i64
            })
            .max(1);
        let font_family = default_basic_title_font_family().to_owned();

        let (sequence_id, clip_id) =
            self.commit_active_sequence_edit("创建基础标题", move |sequence| {
                let time_base = sequence.time_base();
                let position =
                    TimelineTime::from_frame_position(FramePosition::new(start_frame, time_base))?;
                let duration = TimelineTime::from_frame_position(FramePosition::new(
                    duration_frames,
                    time_base,
                ))?;
                let end = position.checked_add(duration)?;

                let preferred = preferred_track.and_then(|track_id| {
                    sequence
                        .video_tracks
                        .iter()
                        .find(|track| {
                            track.id == track_id
                                && !track.is_locked
                                && title_range_is_free(track, position, end)
                        })
                        .map(|track| track.id)
                });
                let track_id = preferred
                    .or_else(|| {
                        sequence
                            .video_tracks
                            .iter()
                            .rev()
                            .find(|track| {
                                !track.is_locked && title_range_is_free(track, position, end)
                            })
                            .map(|track| track.id)
                    })
                    .unwrap_or_else(|| sequence.add_video_track());

                let clip = Clip::new_basic_title("标题", font_family, position, duration)?;
                let clip_id = clip.id;
                sequence
                    .video_track_mut(track_id)
                    .ok_or_else(|| mondrian_core::MondrianError::TrackNotFound {
                        track_id: track_id.to_string(),
                    })?
                    .add_clip(clip)?;
                Ok((sequence.id, clip_id))
            })?;
        self.select_clip_by_id(clip_id);
        self.event_bus.publish(AppEvent::TimelineModified { sequence_id });
        self.set_status_hint("已创建基础标题", false);
        Ok(clip_id)
    }
}

fn title_range_is_free(
    track: &mondrian_timeline::track::Track,
    start: TimelineTime,
    end: TimelineTime,
) -> bool {
    track.clips.iter().all(|clip| {
        clip.end_position()
            .map(|clip_end| clip_end <= start || clip.position >= end)
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occupied_tracks_create_title_and_track_in_one_undo_step() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("Title");
        let time_base = sequence.time_base();
        for track in &mut sequence.video_tracks {
            track
                .add_clip(
                    Clip::new_solid_color(
                        AssetId::new(),
                        Color::BLACK,
                        TimelineTime::ZERO,
                        TimelineTime::from_frame_position(FramePosition::new(200, time_base))
                            .expect("duration"),
                    )
                    .expect("solid"),
                )
                .expect("placement");
        }
        let original_track_count = sequence.video_tracks.len();
        state.test_set_sequence(Some(sequence));

        let title_id = state.create_basic_title_at_playhead().expect("title");
        let sequence = state.active_sequence().expect("sequence");
        assert_eq!(sequence.video_tracks.len(), original_track_count + 1);
        assert!(sequence.video_tracks[original_track_count]
            .clips
            .iter()
            .any(|clip| clip.id == title_id));
        assert!(state.undo_timeline().expect("undo"));
        let sequence = state.active_sequence().expect("sequence");
        assert_eq!(sequence.video_tracks.len(), original_track_count);
        assert!(sequence
            .video_tracks
            .iter()
            .flat_map(|track| &track.clips)
            .all(|clip| clip.id != title_id));
    }

    #[test]
    fn selected_free_video_track_is_preferred() {
        let mut state = AppState::new();
        let sequence = Sequence::new("Title");
        let selected_track = sequence.video_tracks[0].id;
        state.test_set_sequence(Some(sequence));
        state.select_track_by_id(selected_track);

        let title_id = state.create_basic_title_at_playhead().expect("title");
        let sequence = state.active_sequence().expect("sequence");
        assert_eq!(sequence.video_tracks.len(), 3);
        assert!(sequence.video_tracks[0].clips.iter().any(|clip| clip.id == title_id));
    }
}
