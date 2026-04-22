//! 序列（时间线）

use crate::{clip::ActiveClip, track::Track};
use mondrian_core::types::*;
use serde::{Deserialize, Serialize};

/// 序列设置（帧率/分辨率/音频配置）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceSettings {
    pub resolution: Resolution,
    pub frame_rate: Rational,
    pub audio_sample_rate: u32,
    pub audio_channels: u8,
    pub color_space: ColorSpace,
}

impl Default for SequenceSettings {
    fn default() -> Self {
        Self {
            resolution: Resolution::FHD,
            frame_rate: Rational::FPS_25,
            audio_sample_rate: 48000,
            audio_channels: 2,
            color_space: ColorSpace::Rec709,
        }
    }
}

/// Mondrian 时间线序列
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sequence {
    pub id: SequenceId,
    pub name: String,
    pub settings: SequenceSettings,
    pub video_tracks: Vec<Track>,
    pub audio_tracks: Vec<Track>,
    pub playhead: TimeCode,
}

impl Sequence {
    pub fn new(name: impl Into<String>) -> Self {
        let settings = SequenceSettings::default();
        let tb = Rational::new(settings.frame_rate.den, settings.frame_rate.num);
        Self {
            id: SequenceId::new(),
            name: name.into(),
            video_tracks: vec![
                Track::new_video("V1"),
                Track::new_video("V2"),
                Track::new_video("V3"),
            ],
            audio_tracks: vec![
                Track::new_audio("A1"),
                Track::new_audio("A2"),
                Track::new_audio("A3"),
            ],
            playhead: TimeCode::new(0, tb),
            settings,
        }
    }

    pub fn time_base(&self) -> Rational {
        Rational::new(self.settings.frame_rate.den, self.settings.frame_rate.num)
    }

    pub fn total_duration(&self) -> TimeCode {
        let tb = self.time_base();
        let mut max_frame = 0i64;

        for track in self.video_tracks.iter().chain(self.audio_tracks.iter()) {
            if let Some(last) = track.clips.last() {
                max_frame = max_frame.max(last.end_position().frame);
            }
        }
        TimeCode::new(max_frame, tb)
    }

    pub fn active_clips_at(&self, time: TimeCode) -> Vec<ActiveClip> {
        let mut result = Vec::new();

        for (i, track) in self.video_tracks.iter().enumerate() {
            if !track.is_visible || track.is_muted {
                continue;
            }

            let track_opacity = track.evaluate_opacity(time).clamp(0.0, 1.0);
            for clip in track.active_clips_at(time) {
                let source_time = clip.timeline_to_source_time(time);
                let transform_mat = clip.transform.evaluate_matrix(time);
                let opacity =
                    (clip.transform.evaluate_opacity(time) * track_opacity).clamp(0.0, 1.0);
                result.push(ActiveClip {
                    clip: clip.clone(),
                    track_index: i,
                    source_time,
                    transform_matrix: transform_mat,
                    opacity,
                });
            }
        }
        result
    }

    pub fn snap_points(&self) -> Vec<TimeCode> {
        let mut pts: Vec<TimeCode> = self
            .video_tracks
            .iter()
            .chain(self.audio_tracks.iter())
            .flat_map(|track| track.snap_points())
            .collect();
        pts.push(self.playhead);
        pts.push(TimeCode::new(0, self.time_base()));
        pts.sort_unstable();
        pts.dedup();
        pts
    }

    pub fn video_track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.video_tracks.iter_mut().find(|track| track.id == id)
    }

    pub fn audio_track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.audio_tracks.iter_mut().find(|track| track.id == id)
    }

    pub fn add_video_track(&mut self) -> TrackId {
        let track = Track::new_video("");
        let id = track.id;
        self.video_tracks.push(track);
        self.normalize_track_names();
        id
    }

    pub fn add_audio_track(&mut self) -> TrackId {
        let track = Track::new_audio("");
        let id = track.id;
        self.audio_tracks.push(track);
        self.normalize_track_names();
        id
    }

    pub fn remove_video_track(&mut self, id: TrackId) -> mondrian_core::Result<()> {
        if self.video_tracks.len() <= 1 {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "remove_video_track".to_string(),
                reason: "至少保留 1 条视频轨道".to_string(),
            });
        }

        if let Some(index) = self.video_tracks.iter().position(|track| track.id == id) {
            self.video_tracks.remove(index);
            self.normalize_track_names();
            Ok(())
        } else {
            Err(mondrian_core::MondrianError::TrackNotFound { track_id: id.to_string() })
        }
    }

    pub fn remove_audio_track(&mut self, id: TrackId) -> mondrian_core::Result<()> {
        if self.audio_tracks.len() <= 1 {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "remove_audio_track".to_string(),
                reason: "至少保留 1 条音频轨道".to_string(),
            });
        }

        if let Some(index) = self.audio_tracks.iter().position(|track| track.id == id) {
            self.audio_tracks.remove(index);
            self.normalize_track_names();
            Ok(())
        } else {
            Err(mondrian_core::MondrianError::TrackNotFound { track_id: id.to_string() })
        }
    }

    pub fn move_video_track(&mut self, id: TrackId, new_index: usize) -> mondrian_core::Result<()> {
        move_track_in_list(&mut self.video_tracks, id, new_index)?;
        self.normalize_track_names();
        Ok(())
    }

    pub fn move_audio_track(&mut self, id: TrackId, new_index: usize) -> mondrian_core::Result<()> {
        move_track_in_list(&mut self.audio_tracks, id, new_index)?;
        self.normalize_track_names();
        Ok(())
    }

    pub fn normalize_track_names(&mut self) {
        renumber_tracks(&mut self.video_tracks, "V");
        renumber_tracks(&mut self.audio_tracks, "A");
    }
}

fn move_track_in_list(
    tracks: &mut Vec<Track>,
    id: TrackId,
    new_index: usize,
) -> mondrian_core::Result<()> {
    let current_index = tracks
        .iter()
        .position(|track| track.id == id)
        .ok_or_else(|| mondrian_core::MondrianError::TrackNotFound { track_id: id.to_string() })?;

    let clamped_index = new_index.min(tracks.len().saturating_sub(1));
    if current_index == clamped_index {
        return Ok(());
    }

    let track = tracks.remove(current_index);
    tracks.insert(clamped_index, track);
    Ok(())
}

fn renumber_tracks(tracks: &mut [Track], prefix: &str) {
    for (index, track) in tracks.iter_mut().enumerate() {
        track.name = format!("{prefix}{}", index + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::Clip;
    use mondrian_core::automation::{
        timecode_to_ticks, Keyframe, PropertyHost, PropertyMutation, PropertyValue,
    };

    #[test]
    fn sequence_active_clips() {
        let mut seq = Sequence::new("Test");
        let asset_id = AssetId::new();
        let tb = seq.time_base();

        let clip = Clip::new(asset_id, TimeCode::new(0, tb), TimeCode::new(50, tb));
        seq.video_tracks[0].add_clip(clip).unwrap();

        let active = seq.active_clips_at(TimeCode::new(25, tb));
        assert_eq!(active.len(), 1);

        let outside = seq.active_clips_at(TimeCode::new(100, tb));
        assert_eq!(outside.len(), 0);
    }

    #[test]
    fn track_opacity_automation_affects_active_clip_opacity() {
        let mut seq = Sequence::new("Opacity Test");
        let tb = seq.time_base();
        let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
        seq.video_tracks[0].add_clip(clip).expect("add clip");
        seq.video_tracks[0]
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: Track::OPACITY_PATH.to_string(),
                keyframe: Keyframe::linear(
                    timecode_to_ticks(TimeCode::new(0, tb)),
                    PropertyValue::Float(1.0),
                ),
            })
            .expect("set start opacity");
        seq.video_tracks[0]
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: Track::OPACITY_PATH.to_string(),
                keyframe: Keyframe::linear(
                    timecode_to_ticks(TimeCode::new(20, tb)),
                    PropertyValue::Float(0.4),
                ),
            })
            .expect("set end opacity");

        let active = seq.active_clips_at(TimeCode::new(10, tb));
        assert_eq!(active.len(), 1);
        assert!((active[0].opacity - 0.7).abs() < 0.01);
    }

    #[test]
    fn active_clips_follow_bottom_to_top_track_order() {
        let mut seq = Sequence::new("Track Order");
        let tb = seq.time_base();
        let bottom = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
        let top = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(20, tb));
        seq.video_tracks[0].add_clip(bottom).expect("add bottom clip");
        seq.video_tracks[2].add_clip(top).expect("add top clip");

        let active = seq.active_clips_at(TimeCode::new(5, tb));
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].track_index, 0);
        assert_eq!(active[1].track_index, 2);
    }

    #[test]
    fn adjustment_layer_is_active_only_within_its_time_range() {
        let mut seq = Sequence::new("Adjustment Range");
        let tb = seq.time_base();
        let media = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(30, tb));
        let adjustment =
            Clip::new_adjustment_layer(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(5, tb));
        seq.video_tracks[0].add_clip(media).expect("add media clip");
        seq.video_tracks[1].add_clip(adjustment).expect("add adjustment clip");

        let before = seq.active_clips_at(TimeCode::new(9, tb));
        assert_eq!(before.len(), 1);
        assert!(!before.iter().any(|clip| clip.clip.is_adjustment_layer()));

        let overlapping = seq.active_clips_at(TimeCode::new(12, tb));
        assert_eq!(overlapping.len(), 2);
        assert!(overlapping.iter().any(|clip| clip.clip.is_adjustment_layer()));

        let after = seq.active_clips_at(TimeCode::new(15, tb));
        assert_eq!(after.len(), 1);
        assert!(!after.iter().any(|clip| clip.clip.is_adjustment_layer()));
    }

    #[test]
    fn removing_track_renumbers_remaining_tracks() {
        let mut seq = Sequence::new("Track Names");
        let removed_id = seq.video_tracks[1].id;
        let last_id = seq.video_tracks[2].id;

        seq.remove_video_track(removed_id).expect("remove middle video track");

        assert_eq!(seq.video_tracks.len(), 2);
        assert_eq!(seq.video_tracks[0].name, "V1");
        assert_eq!(seq.video_tracks[1].name, "V2");
        assert_eq!(seq.video_tracks[1].id, last_id);
    }

    #[test]
    fn moving_track_preserves_track_identity_and_clips() {
        let mut seq = Sequence::new("Track Move");
        let tb = seq.time_base();
        let moved_id = seq.video_tracks[2].id;
        let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(10, tb));
        let clip_id = clip.id;
        seq.video_tracks[2].add_clip(clip).expect("add clip to track");

        seq.move_video_track(moved_id, 0).expect("move track to top");

        assert_eq!(seq.video_tracks[0].id, moved_id);
        assert_eq!(seq.video_tracks[0].name, "V1");
        assert_eq!(seq.video_tracks[0].clips[0].id, clip_id);
        assert_eq!(seq.video_tracks[1].name, "V2");
        assert_eq!(seq.video_tracks[2].name, "V3");
    }
}
