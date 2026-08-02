//! 轨道定义

use crate::clip::Clip;
use mondrian_core::{
    automation::{
        AnimatedProperty, ParameterInvalidValuePolicy, ParameterNumericContract, ParameterUnit,
        PropertyBag, PropertyDescriptor, PropertyHost, PropertyMutation, PropertyValue,
    },
    types::*,
    AuthoringFootprint, AuthoringFootprintCollector, AuthoringFootprintError, AuthoringList,
    MondrianError, ParameterId, Result, TimelineTime,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackType {
    Video,
    Audio,
    Subtitle,
}

impl AuthoringFootprint for TrackType {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), AuthoringFootprintError> {
        match self {
            Self::Video | Self::Audio | Self::Subtitle => Ok(()),
        }
    }
}

/// 时间线轨道
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub track_type: TrackType,
    /// 轨道高度（UI 像素）
    pub height: f32,
    pub is_muted: bool,
    pub is_locked: bool,
    pub is_visible: bool,
    pub blend_mode: BlendMode,
    /// 轨道不透明度关键帧（仅视频轨有效）
    pub opacity: AnimatedProperty,
    /// 按位置排序的 Clip 列表
    pub clips: AuthoringList<Clip>,
}

impl AuthoringFootprint for Track {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), AuthoringFootprintError> {
        let Self {
            id: _,
            name,
            track_type,
            height: _,
            is_muted: _,
            is_locked: _,
            is_visible: _,
            blend_mode: _,
            opacity,
            clips,
        } = self;
        collector.collect(name)?;
        collector.collect(track_type)?;
        collector.collect(opacity)?;
        collector.collect(clips)
    }
}

impl Track {
    pub const OPACITY_PATH: &'static str = "track.opacity";

    pub fn new_video(name: impl Into<String>) -> Self {
        Self {
            id: TrackId::new(),
            name: name.into(),
            track_type: TrackType::Video,
            height: 80.0,
            is_muted: false,
            is_locked: false,
            is_visible: true,
            blend_mode: BlendMode::Normal,
            opacity: AnimatedProperty::from_descriptor(track_opacity_descriptor()),
            clips: AuthoringList::new(),
        }
    }

    pub fn new_audio(name: impl Into<String>) -> Self {
        Self {
            id: TrackId::new(),
            name: name.into(),
            track_type: TrackType::Audio,
            height: 50.0,
            is_muted: false,
            is_locked: false,
            is_visible: true,
            blend_mode: BlendMode::Normal,
            opacity: AnimatedProperty::from_descriptor(track_opacity_descriptor()),
            clips: AuthoringList::new(),
        }
    }

    /// Validate persisted Track-owned author state before publication.
    pub fn validate_author_state(&self) -> Result<()> {
        if !self.height.is_finite() || self.height <= 0.0 {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "track_author_state_validation".to_owned(),
                reason: format!(
                    "track {} height must be finite and greater than zero, got {}",
                    self.id, self.height
                ),
            });
        }
        if self.clips.windows(2).any(|pair| pair[0].position > pair[1].position) {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "track_author_state_validation".to_owned(),
                reason: format!(
                    "track {} Clip placements must be ordered by nondecreasing position",
                    self.id
                ),
            });
        }
        Ok(())
    }

    /// 添加 Clip（按位置插入，保持排序）
    pub fn add_clip(&mut self, clip: Clip) -> mondrian_core::Result<()> {
        if self.is_locked {
            return Err(mondrian_core::MondrianError::TrackLocked {
                track_id: self.id.to_string(),
            });
        }
        let pos = self.clips.partition_point(|c| c.position <= clip.position);
        self.clips.insert(pos, clip);
        Ok(())
    }

    /// 移除 Clip
    pub fn remove_clip(&mut self, clip_id: ClipId) -> Option<Clip> {
        if let Some(idx) = self.clips.iter().position(|c| c.id == clip_id) {
            Some(self.clips.remove(idx))
        } else {
            None
        }
    }

    /// 获取指定时间码处所有活跃 Clip
    pub fn active_clips_at(&self, time: TimelineTime) -> Result<Vec<&Clip>> {
        let mut active = Vec::new();
        for clip in &self.clips {
            if !clip.is_disabled && clip.contains(time)? {
                active.push(clip);
            }
        }
        Ok(active)
    }

    /// 吸附点列表（所有 Clip 的 in/out 点 + 每个 Clip 的关键帧时间）
    pub fn snap_points(&self) -> Result<Vec<TimelineTime>> {
        let mut pts = Vec::with_capacity(self.clips.len().saturating_mul(2));
        for clip in &self.clips {
            pts.push(clip.position);
            pts.push(clip.end_position()?);
        }
        pts.sort_unstable();
        pts.dedup();
        Ok(pts)
    }

    pub fn to_property_bag(&self) -> PropertyBag {
        let mut properties = PropertyBag::default();
        properties.upsert(self.opacity.clone());
        properties
    }
}

fn track_opacity_descriptor() -> PropertyDescriptor {
    let numeric =
        ParameterNumericContract::closed(0.0, 1.0, Some(0.01), ParameterInvalidValuePolicy::Reject)
            .expect("track opacity has a valid built-in numeric contract");
    PropertyDescriptor::new(
        Track::OPACITY_PATH,
        "轨道不透明度",
        PropertyValue::Float(1.0),
    )
    .with_parameter_id(ParameterId::new_static("mondrian.track.opacity"))
    .with_numeric_contract(ParameterUnit::Normalized, numeric)
}

impl PropertyHost for Track {
    fn property_bag(&self) -> Result<PropertyBag> {
        Ok(self.to_property_bag())
    }

    fn apply_property_mutation(&mut self, mutation: PropertyMutation) -> Result<()> {
        let path = mutation.path();

        if path != Self::OPACITY_PATH {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "track_apply_property_mutation".to_string(),
                reason: format!("Track 不支持属性路径: {path}"),
            });
        }
        if matches!(mutation, PropertyMutation::RemoveProperty { .. }) {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "track_apply_property_mutation".to_string(),
                reason: "内建 track.opacity 属性不可移除".to_string(),
            });
        }

        self.opacity.apply_mutation(mutation)
    }
}

impl Track {
    pub fn evaluate_opacity(&self, time: TimelineTime) -> f32 {
        self.opacity.evaluate(time).as_f32().unwrap_or(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_height_must_be_positive_and_finite() {
        for invalid_height in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut track = Track::new_video("V1");
            track.height = invalid_height;
            assert!(track.validate_author_state().is_err(), "{invalid_height:?}");
        }

        let track = Track::new_video("V1");
        assert!(track.validate_author_state().is_ok());
    }

    #[test]
    fn persisted_clip_placements_must_be_sorted_but_equal_positions_are_stable() {
        let mut track = Track::new_video("V1");
        let late = Clip::new(
            AssetId::new(),
            TimelineTime::new(2, 1).expect("late position"),
            TimelineTime::ONE,
        )
        .expect("valid late Clip");
        let early = Clip::new(
            AssetId::new(),
            TimelineTime::new(1, 1).expect("early position"),
            TimelineTime::ONE,
        )
        .expect("valid early Clip");
        track.clips = AuthoringList::from(vec![late, early]);
        assert!(track.validate_author_state().is_err());

        let first = Clip::new(AssetId::new(), TimelineTime::ONE, TimelineTime::ONE)
            .expect("valid first Clip");
        let second = Clip::new(AssetId::new(), TimelineTime::ONE, TimelineTime::ONE)
            .expect("valid second Clip");
        let expected_order = [first.id, second.id];
        track.clips = AuthoringList::from(vec![first, second]);
        track.validate_author_state().expect("equal positions remain valid");
        assert_eq!(
            track.clips.iter().map(|clip| clip.id).collect::<Vec<_>>(),
            expected_order
        );
    }
}
