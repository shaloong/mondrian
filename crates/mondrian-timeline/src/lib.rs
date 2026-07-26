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
mod clip_fragment;
pub mod clip_linking;
pub mod insert_edit;
pub mod keyframe;
pub mod range_edit;
pub mod sequence;
mod sequence_time_edit;
pub mod track;
pub mod video_transition;

pub use audio::*;
pub use clip::{ActiveClip, Clip, ClipKind, ClipSourceTimeMap};
pub use clip_linking::{
    apply_clip_link_edit, assess_clip_link_edit, clip_selection_unit, ClipLinkEditAssessment,
    ClipLinkEditError, ClipLinkEditKind, ClipLinkEditOutcome, ClipLinkEditRequest,
};
pub use insert_edit::{
    apply_insert_edit, InsertAutomationPolicy, InsertEditError, InsertEditOutcome,
    InsertEditPlacement, InsertEditRequest, InsertSplitOutcome, InsertTimelineStatePolicy,
    InsertTransitionPolicy,
};
pub use keyframe::{InterpolationType, Keyframe, KeyframeTrack};
pub use range_edit::{
    apply_range_edit, assess_range_edit, RangeEditAutomationPolicy, RangeEditError, RangeEditKind,
    RangeEditOutcome, RangeEditRequest, RangeEditSplitOutcome, RangeEditTimelineStatePolicy,
    RangeEditTransitionPolicy,
};
pub use sequence::{
    AudioChannelLayout, AudioDisplayFormat, EditingMode, FieldOrder, PixelAspectRatio,
    PreviewRenderFormat, Sequence, SequenceCollection, SequencePreset, SequencePreviewSettings,
    SequenceRole, SequenceSettings,
};
pub use track::{Track, TrackType};
pub use video_transition::{VideoTransition, VideoTransitionSourceDemand, VideoTransitionType};
