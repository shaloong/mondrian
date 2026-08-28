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
pub mod audio_automation_edit;
pub mod audio_channel_strip_edit;
pub mod audio_component_edit;
pub mod audio_processor_edit;
pub mod audio_routing_edit;
pub mod clip;
mod clip_fragment;
pub mod clip_linking;
mod cut_edit;
pub mod grade;
pub mod insert_edit;
pub mod keyframe;
pub mod overwrite_edit;
pub mod range_edit;
pub mod retime;
pub mod sequence;
mod sequence_dependency;
mod sequence_time_edit;
pub mod track;
pub mod video_transition;
pub mod visual_schedule;

pub use audio::*;
pub use audio_automation_edit::{
    apply_audio_automation_edit, inspect_audio_automation, AudioAutomationAddressError,
    AudioAutomationEdit, AudioAutomationEditBlocker, AudioAutomationEditError,
    AudioAutomationEditOutcome, AudioAutomationEditRequest, AudioAutomationInspection,
    AudioAutomationTarget, AudioAutomationValueContract,
};
pub use audio_channel_strip_edit::{
    apply_audio_channel_strip_edit, audio_channel_strip, inspect_audio_channel_strip,
    AudioChannelStripAddressError, AudioChannelStripEdit, AudioChannelStripEditBlocker,
    AudioChannelStripEditError, AudioChannelStripEditOutcome, AudioChannelStripEditRequest,
    AudioChannelStripInspection,
};
pub use audio_component_edit::{
    apply_audio_component_edit, inspect_audio_component, AudioComponentAddress,
    AudioComponentAddressError, AudioComponentEditBlocker, AudioComponentEditError,
    AudioComponentEditOutcome, AudioComponentEditRequest, AudioComponentInspection,
    AudioComponentMutation,
};
pub use audio_processor_edit::{
    apply_audio_processor_rack_edit, audio_processor_rack, inspect_audio_processor_rack,
    AudioChannelStripRack, AudioProcessorRackAddress, AudioProcessorRackEdit,
    AudioProcessorRackEditError, AudioProcessorRackEditOutcome, AudioProcessorRackEditRequest,
    AudioProcessorRackInspection, AudioProcessorRackPlacement,
};
pub use audio_routing_edit::{
    apply_audio_routing_edit, inspect_audio_mix_bus, inspect_audio_route,
    inspect_audio_route_candidates, AudioBusRemovalPolicy, AudioMixBusInspection,
    AudioRouteCandidateInspection, AudioRouteInspection, AudioRoutingAddressError,
    AudioRoutingEdit, AudioRoutingEditBlocker, AudioRoutingEditError, AudioRoutingEditOutcome,
    AudioRoutingEditRequest,
};
pub use clip::{
    ActiveClip, Clip, ClipKind, ClipSourceTimeMap, EffectRelativePlacement, MaskRelativePlacement,
    TrimEdge,
};
pub use clip_linking::{
    apply_clip_link_edit, assess_clip_link_edit, clip_selection_unit, expand_clip_selection_units,
    ClipLinkEditAssessment, ClipLinkEditError, ClipLinkEditKind, ClipLinkEditOutcome,
    ClipLinkEditRequest,
};
pub use cut_edit::{
    apply_roll_edit, apply_slide_edit, apply_slip_edit, apply_split_edit, assess_roll_edit,
    assess_slide_edit, assess_slip_edit, assess_split_edit, prepare_trimmed_clip_at_time,
    CutEditError, RollEditOutcome, RollEditRequest, SlideEditOutcome, SlideEditRequest,
    SlipEditOutcome, SlipEditRequest, SplitEditOutcome, SplitEditRequest,
};
pub use grade::{GradeGroup, GradeScope};
pub use insert_edit::{
    apply_insert_edit, InsertAutomationPolicy, InsertEditError, InsertEditOutcome,
    InsertEditPlacement, InsertEditRequest, InsertSplitOutcome, InsertTimelineStatePolicy,
    InsertTransitionPolicy,
};
pub use keyframe::{InterpolationType, Keyframe, KeyframeTrack};
pub use overwrite_edit::{
    apply_overwrite_conflicts, apply_sequence_track_conflicts_for_focus_group,
    apply_track_conflicts_for_focus_group, resolve_track_conflicts, resolve_track_overlaps,
    subtract_overwrite_range_from_clip, ClipOverlapMode,
};
pub use range_edit::{
    apply_range_edit, assess_range_edit, RangeEditAutomationPolicy, RangeEditError, RangeEditKind,
    RangeEditOutcome, RangeEditRequest, RangeEditSplitOutcome, RangeEditTimelineStatePolicy,
    RangeEditTransitionPolicy,
};
pub use retime::{
    apply_clip_constant_retime, ClipConstantRetime, ClipConstantRetimeOutcome,
    ClipConstantRetimeRequest,
};
pub use sequence::{
    AudioChannelLayout, AudioDisplayFormat, ClipTrackLocation, EditingMode, FieldOrder,
    PixelAspectRatio, PreviewRenderFormat, Sequence, SequenceAuthorContractCertificate,
    SequenceCollection, SequencePreset, SequenceRole, SequenceSettings,
};
pub use sequence_dependency::SequenceDependencyCertificate;
pub use track::TrackRelativePlacement;
pub use track::{Track, TrackType};
pub use video_transition::{
    validate_selected_video_transition_source_handles, PictureSourceExtent, PictureSourceRef,
    VideoTransition, VideoTransitionEndpointSide, VideoTransitionSourceDemand,
    VideoTransitionSourceHandleValidationError, VideoTransitionType,
};
pub use visual_schedule::{
    PreparedVisualSchedule, PreparedVisualScheduleDiagnostics,
    PreparedVisualScheduleQueryDiagnostics, PreparedVisualScheduleRangeClip,
    PreparedVisualSourceIdentity,
};
