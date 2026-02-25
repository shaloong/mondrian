//! Clip（时间线剪辑片段）

use crate::keyframe::KeyframeTrack;
use glam::Vec2;
use mondrian_core::types::*;
use serde::{Deserialize, Serialize};

/// 裁剪边缘
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimEdge {
    /// 裁剪入点（左侧）
    In,
    /// 裁剪出点（右侧）
    Out,
}

/// 2D 变换（位置 / 缩放 / 旋转 / 锚点 / 不透明度），所有属性可关键帧动画
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transform2D {
    pub position: KeyframeTrack<Vec2>,
    pub scale: KeyframeTrack<Vec2>,
    pub rotation: KeyframeTrack<f32>,
    pub anchor_point: KeyframeTrack<Vec2>,
    pub opacity: KeyframeTrack<f32>,
}

impl Transform2D {
    pub fn identity() -> Self {
        Self {
            position: KeyframeTrack::constant(Vec2::ZERO),
            scale: KeyframeTrack::constant(Vec2::ONE),
            rotation: KeyframeTrack::constant(0.0),
            anchor_point: KeyframeTrack::constant(Vec2::ZERO),
            opacity: KeyframeTrack::constant(1.0),
        }
    }

    /// 求值为 3x3 仿射变换矩阵（用于 GPU 渲染）
    pub fn evaluate_matrix(&self, time: TimeCode) -> glam::Mat3 {
        let pos = self.position.evaluate(time);
        let scale = self.scale.evaluate(time);
        let rot = self.rotation.evaluate(time).to_radians();

        let cos_r = rot.cos();
        let sin_r = rot.sin();

        // 缩放 + 旋转 + 位移
        glam::Mat3::from_cols(
            glam::Vec3::new(scale.x * cos_r, scale.x * sin_r, 0.0),
            glam::Vec3::new(-scale.y * sin_r, scale.y * cos_r, 0.0),
            glam::Vec3::new(pos.x, pos.y, 1.0),
        )
    }
}

/// 变速模式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SpeedMode {
    /// 恒定速度（1.0 = 正常速度）
    Constant(f64),
    /// 变速曲线（时间重映射）
    Keyframed(KeyframeTrack<f64>),
    /// 倒放
    Reverse,
}

impl SpeedMode {
    /// 给定时间线本地时间 → 素材源时间（帧偏移）
    pub fn map_time(&self, local_time: TimeCode) -> TimeCode {
        match self {
            Self::Constant(speed) => TimeCode::new(
                (local_time.frame as f64 * speed) as i64,
                local_time.time_base,
            ),
            Self::Keyframed(track) => {
                let speed_at = track.evaluate(local_time);
                TimeCode::new(
                    (local_time.frame as f64 * speed_at) as i64,
                    local_time.time_base,
                )
            }
            Self::Reverse => {
                // 倒放由外层处理
                local_time
            }
        }
    }
}

/// 效果引用（指向效果系统中的节点）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectRef {
    pub effect_id: EffectId,
    pub is_enabled: bool,
}

/// 时间线上的一个剪辑片段
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    /// 关联的素材资产
    pub asset_id: AssetId,
    /// 在时间线上的起始位置
    pub position: TimeCode,
    /// 在时间线上的持续时长
    pub duration: TimeCode,
    /// 素材内入点
    pub source_in: TimeCode,
    /// 素材内出点（= source_in + duration / speed）
    pub source_out: TimeCode,
    /// 2D 变换（关键帧）
    pub transform: Transform2D,
    /// 变速模式
    pub speed: SpeedMode,
    /// 效果链
    pub effects: Vec<EffectRef>,
    /// 关联的音频/视频 Clip（保持同步）
    pub linked_clip: Option<ClipId>,
    /// 是否禁用
    pub is_disabled: bool,
    /// 混合模式（覆盖轨道设置）
    pub blend_mode: Option<BlendMode>,
    /// 显示标签（可选）
    pub label: Option<String>,
}

impl Clip {
    pub fn new(asset_id: AssetId, position: TimeCode, duration: TimeCode) -> Self {
        let tb = position.time_base;
        Self {
            id: ClipId::new(),
            asset_id,
            position,
            duration,
            source_in: TimeCode::new(0, tb),
            source_out: duration,
            transform: Transform2D::identity(),
            speed: SpeedMode::Constant(1.0),
            effects: vec![],
            linked_clip: None,
            is_disabled: false,
            blend_mode: None,
            label: None,
        }
    }

    /// Clip 在时间线上的结束位置
    pub fn end_position(&self) -> TimeCode {
        self.position + self.duration
    }

    /// 判断给定时间码是否在此 Clip 范围内
    pub fn contains(&self, time: TimeCode) -> bool {
        time >= self.position && time < self.end_position()
    }

    /// 将时间线时间 → Clip 内本地时间 → 素材源时间
    pub fn timeline_to_source_time(&self, timeline_time: TimeCode) -> TimeCode {
        let local = timeline_time - self.position;
        let source_local = self.speed.map_time(local);
        self.source_in + source_local
    }
}

/// 某时刻激活的 Clip（用于渲染请求）
#[derive(Debug, Clone)]
pub struct ActiveClip {
    pub clip: Clip,
    pub track_index: usize,
    /// 此时刻对应的素材源时间（用于解码）
    pub source_time: TimeCode,
    /// Transform 矩阵（已在此时刻求值）
    pub transform_matrix: glam::Mat3,
    /// 不透明度（已在此时刻求值）
    pub opacity: f32,
}
