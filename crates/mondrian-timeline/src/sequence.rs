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
///
/// 一个项目可以有多个序列（对标 PR 的多个序列）。
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
    /// 创建新序列
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

    /// 时间基（= 1 / fps）
    pub fn time_base(&self) -> Rational {
        Rational::new(self.settings.frame_rate.den, self.settings.frame_rate.num)
    }

    /// 计算时间线总时长（最后一个 Clip 的 end_position）
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

    /// 获取指定时间码处所有活跃 Clip（从底层到顶层排列）
    pub fn active_clips_at(&self, time: TimeCode) -> Vec<ActiveClip> {
        let mut result = Vec::new();

        // 从底层视频轨开始（倒序 → 最后一个轨道在最底）
        for (i, track) in self.video_tracks.iter().enumerate().rev() {
            if !track.is_visible || track.is_muted {
                continue;
            }
            for clip in track.active_clips_at(time) {
                let source_time = clip.timeline_to_source_time(time);
                let transform_mat = clip.transform.evaluate_matrix(time);
                let opacity = clip.transform.opacity.evaluate(time);
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

    /// 全局吸附点（所有轨道的 snap points 合集 + 播放头）
    pub fn snap_points(&self) -> Vec<TimeCode> {
        let mut pts: Vec<TimeCode> = self
            .video_tracks
            .iter()
            .chain(self.audio_tracks.iter())
            .flat_map(|t| t.snap_points())
            .collect();
        pts.push(self.playhead);
        pts.push(TimeCode::new(0, self.time_base()));
        pts.sort_unstable();
        pts.dedup();
        pts
    }

    /// 按 ID 查找视频轨道（可变引用）
    pub fn video_track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.video_tracks.iter_mut().find(|t| t.id == id)
    }

    /// 按 ID 查找音频轨道（可变引用）
    pub fn audio_track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.audio_tracks.iter_mut().find(|t| t.id == id)
    }

    pub fn add_video_track(&mut self) -> TrackId {
        let name = format!("V{}", self.video_tracks.len() + 1);
        let track = Track::new_video(name);
        let id = track.id;
        self.video_tracks.push(track);
        id
    }

    pub fn add_audio_track(&mut self) -> TrackId {
        let name = format!("A{}", self.audio_tracks.len() + 1);
        let track = Track::new_audio(name);
        let id = track.id;
        self.audio_tracks.push(track);
        id
    }

    pub fn remove_video_track(&mut self, id: TrackId) -> mondrian_core::Result<()> {
        if self.video_tracks.len() <= 1 {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "remove_video_track".to_string(),
                reason: "至少保留 1 条视频轨道".to_string(),
            });
        }

        if let Some(index) = self.video_tracks.iter().position(|t| t.id == id) {
            self.video_tracks.remove(index);
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

        if let Some(index) = self.audio_tracks.iter().position(|t| t.id == id) {
            self.audio_tracks.remove(index);
            Ok(())
        } else {
            Err(mondrian_core::MondrianError::TrackNotFound { track_id: id.to_string() })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::Clip;

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
}
