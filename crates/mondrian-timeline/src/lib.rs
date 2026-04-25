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

pub mod clip;
pub mod command;
pub mod keyframe;
pub mod sequence;
pub mod track;

pub use clip::{ActiveClip, Clip, ClipKind};
pub use command::{Command, CommandHistory};
pub use keyframe::{InterpolationType, Keyframe, KeyframeTrack};
pub use sequence::{
    AudioDisplayFormat, EditingMode, FieldOrder, PixelAspectRatio, Sequence, SequenceCollection,
    SequenceRole, SequenceSettings, VideoDisplayFormat,
};
pub use track::{Track, TrackType};
