//! # mondrian-timeline
//!
//! 非线编时间线核心引擎。
//!
//! 提供：
//! - 多轨时间线（Sequence / Track / Clip）
//! - 关键帧系统（贝塞尔曲线插值）
//! - 变速曲线
//! - Command 模式撤销/重做
//! - 时间线查询（活跃 Clip、吸附点）

pub mod audio;
pub mod clip;
pub mod keyframe;
pub mod sequence;
pub mod track;
pub mod video_transition;

pub use audio::*;
pub use clip::{ActiveClip, Clip, ClipKind};
pub use keyframe::{InterpolationType, Keyframe, KeyframeTrack};
pub use sequence::{
    AudioChannelLayout, AudioDisplayFormat, EditingMode, FieldOrder, PixelAspectRatio,
    PreviewRenderFormat, Sequence, SequenceCollection, SequencePreset, SequencePreviewSettings,
    SequenceRole, SequenceSettings,
};
pub use track::{Track, TrackType};
pub use video_transition::{VideoTransition, VideoTransitionSourceDemand, VideoTransitionType};
