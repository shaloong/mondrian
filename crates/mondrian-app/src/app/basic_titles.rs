//! Product authoring operations for generated Basic Title Clips.

use super::*;
use crate::app::AppMessage;
use mondrian_core::{default_basic_title_font_family, BasicTitleContentKind};

const DEFAULT_BASIC_TITLE_DURATION_SECS: f64 = 5.0;

impl AppState {
    /// Create one Basic Title at the current edit range as a single author transaction.
    ///
    /// Placement prefers an explicitly selected, unlocked video Track when it
    /// has room, then the highest unlocked non-overlapping Track. If every
    /// existing Track is occupied, the new Track and Clip are committed
    /// together so one Undo removes both.
    pub fn create_basic_title_at_playhead(&mut self) -> mondrian_core::Result<ClipId> {
        self.create_title_at_playhead(BasicTitleContentKind::Text)
    }

    /// Place a generated Timecode Title using the same undoable placement rules.
    pub fn create_timecode_title_at_playhead(&mut self) -> mondrian_core::Result<ClipId> {
        self.create_title_at_playhead(BasicTitleContentKind::Timecode)
    }

    /// Place a generated Numbers Title using the same undoable placement rules.
    pub fn create_numbers_title_at_playhead(&mut self) -> mondrian_core::Result<ClipId> {
        self.create_title_at_playhead(BasicTitleContentKind::Numbers)
    }

    /// Place a Clip Info overlay using the normal undoable title placement rules.
    pub fn create_clip_info_title_at_playhead(&mut self) -> mondrian_core::Result<ClipId> {
        self.create_title_at_playhead(BasicTitleContentKind::ClipInfo)
    }

    /// Place a five-line metadata overlay as one undoable edit.
    pub fn create_metadata_burn_in_at_playhead(&mut self) -> mondrian_core::Result<ClipId> {
        self.create_title_at_playhead(BasicTitleContentKind::MetadataBurnIn)
    }

    fn create_title_at_playhead(
        &mut self,
        kind: BasicTitleContentKind,
    ) -> mondrian_core::Result<ClipId> {
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

        let (description, status) = match kind {
            BasicTitleContentKind::Text => ("创建基础标题", "status-title-created"),
            BasicTitleContentKind::Timecode => ("创建时间码标题", "status-timecode-title-created"),
            BasicTitleContentKind::Numbers => ("创建编号标题", "status-numbers-title-created"),
            BasicTitleContentKind::ClipInfo => {
                ("创建片段信息标题", "status-clip-info-title-created")
            }
            BasicTitleContentKind::MetadataBurnIn => {
                ("创建元数据刻录", "status-metadata-burn-in-created")
            }
        };
        let (_sequence_id, clip_id) =
            self.commit_active_sequence_edit(description, move |sequence| {
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

                let clip = match kind {
                    BasicTitleContentKind::Text => {
                        Clip::new_basic_title("标题", font_family, position, duration)?
                    }
                    BasicTitleContentKind::Timecode => Clip::new_timecode_title(
                        mondrian_core::TitleTimecode::default(),
                        font_family,
                        position,
                        duration,
                    )?,
                    BasicTitleContentKind::Numbers => {
                        Clip::new_numbers_title(font_family, position, duration)?
                    }
                    BasicTitleContentKind::ClipInfo => {
                        Clip::new_clip_info_title(font_family, position, duration)?
                    }
                    BasicTitleContentKind::MetadataBurnIn => {
                        Clip::new_metadata_burn_in_title(font_family, position, duration)?
                    }
                };
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
        self.set_status_hint(AppMessage::new(status), false);
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
    fn timecode_title_menu_action_creates_an_undoable_clock_clip() {
        let mut state = AppState::new();
        state.test_set_sequence(Some(Sequence::new("Timecode")));
        state
            .dispatch_action(crate::app::ui_actions::timeline_create_timecode_title_action())
            .unwrap();

        let clip = state
            .active_sequence()
            .unwrap()
            .video_tracks
            .iter()
            .find_map(|track| track.clips.first())
            .unwrap();
        assert_eq!(
            clip.content.basic_title().unwrap().timecode().unwrap(),
            Some(mondrian_core::TitleTimecode::default())
        );
        assert!(state.undo_timeline().unwrap());
        assert!(state
            .active_sequence()
            .unwrap()
            .video_tracks
            .iter()
            .all(|track| track.clips.is_empty()));
    }

    #[test]
    fn metadata_burn_in_action_creates_visible_defaults_and_undoes_as_one_edit() {
        let mut state = AppState::new();
        state.test_set_sequence(Some(Sequence::new("Metadata")));
        state
            .dispatch_action(crate::app::ui_actions::timeline_create_metadata_burn_in_action())
            .unwrap();
        let clip = state
            .active_sequence()
            .unwrap()
            .video_tracks
            .iter()
            .find_map(|track| track.clips.first())
            .unwrap();
        let title = clip.content.basic_title().unwrap();
        assert_eq!(title.content_kind(), BasicTitleContentKind::MetadataBurnIn);
        let lines = title.metadata_lines().unwrap().unwrap();
        assert!(lines[0].enabled);
        assert_eq!(
            lines[0].field,
            mondrian_core::TitleMetadataField::SequenceTimecode
        );
        assert!(lines[1..].iter().all(|line| !line.enabled));
        assert!(state.undo_timeline().unwrap());
        assert!(state
            .active_sequence()
            .unwrap()
            .video_tracks
            .iter()
            .all(|track| track.clips.is_empty()));
    }

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
