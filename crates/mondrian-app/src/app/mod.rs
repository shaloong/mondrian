use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use std::{fs, path::Path, path::PathBuf};

use mondrian_assets::{AssetKind, AssetLibrary};
use mondrian_core::{
    automation::{
        interpolation_mode_from_keyframe, AnimationParameterAddress, InterpolationType, Keyframe,
        PropertyHost, PropertyMutation, PropertyValue,
    },
    events::{AppEvent, EventBus},
    types::{
        AssetId, AudioSourceComponentId, ClipId, ClipLinkGroupId, Color, EffectId, FramePosition,
        KeyframeId, Rational, Resolution, SequenceId, TrackId,
    },
    AudioChannelLayout, AudioSamplePosition, AudioSampleRate, AudioSampleRounding,
    DisplayManagementPolicy, FrameRounding, ProjectId, ProjectSettings, TimelineTime,
};
use mondrian_editor_state::{AuthoringSession, AuthoringSessionId, SequenceNavigationIntent};
use mondrian_effects::{EffectType, MaskId};
use mondrian_export::queue::RenderQueue;
use mondrian_media::audio::{AudioBuffer, RealtimeAudioOutputSnapshot};
use mondrian_media::{
    AudioPcmContinuity, AudioPcmContinuityModel, AudioPcmRenderGeneration, AudioPcmRenderRequest,
    AudioPcmRenderer, AudioPlayback, AudioPlaybackError, AudioPlaybackEvent, AudioPlaybackMode,
    AudioPlaybackPoll, AudioPlaybackSnapshot, AudioSourceCache, AudioSourceCacheDiagnostics,
};
use mondrian_playback::{
    AudioClockObservationGrade, AudioDeviceClockObservation, AudioDeviceClockState, ClockMaster,
    FrameDelivery, FrameDeliveryCandidate, FrameDeliveryKind, FrameDemandIdentity,
    FramePresentationQuality, FramePresentationTicket, MonotonicTimestamp, PlaybackEngine,
    PlaybackEvidenceCollector, PlaybackEvidenceReport, PlaybackRate, PlaybackSeekKind,
    PlaybackShuttleDirection, PreviewResolutionScale, TransportState, VideoPrerollObservation,
};
use mondrian_timeline::clip::Clip;
use mondrian_timeline::sequence::{
    ProgramColorContext, Sequence, SequenceCollection, SequenceSettings,
};
pub(crate) use project_persistence::ProjectPersistenceRequestId;
use project_persistence::{
    AutosaveArchiveDestination, ManualProjectFileDestination, ProjectPersistencePurpose,
    ProjectPersistenceService,
};
pub(crate) use project_recovery::discover_crash_recovery_candidates;
pub use project_recovery::{CrashRecoveryCandidate, RecoveryCanonicalTargetEvidence};
pub use reference_output::{
    AppReferenceOutputError, AppReferenceOutputTeardownStatus, ReferenceOutputBinding,
};

const PROJECT_EXTENSION: &str = "mdp";
const DEFAULT_VISUAL_PLACEMENT_DURATION_SECS: f64 = 5.0;
const MAX_STATUS_LOG_ENTRIES: usize = 64;
const AUDIO_OUTPUT_LAYOUT: AudioChannelLayout = AudioChannelLayout::Stereo;

/// One exact left/right identity mapping created by a Clip split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitClipMemberOutcome {
    /// Existing left-hand Clip whose identity is retained.
    pub left_clip_id: ClipId,
    /// Newly authored right-hand Clip.
    pub right_clip_id: ClipId,
}

/// Complete stable result of one targeted Clip split author transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitClipOutcome {
    primary: SplitClipMemberOutcome,
    linked_members: Vec<SplitClipMemberOutcome>,
}

impl SplitClipOutcome {
    /// Return the requested Clip's exact left/right identity mapping.
    pub const fn primary(&self) -> SplitClipMemberOutcome {
        self.primary
    }

    /// Return every additional synchronized link-group member mapping.
    pub fn linked_members(&self) -> &[SplitClipMemberOutcome] {
        &self.linked_members
    }

    /// Return the complete number of placements changed by the transaction.
    pub fn split_member_count(&self) -> usize {
        1 + self.linked_members.len()
    }
}

#[cfg(test)]
pub(crate) fn tt(frame: i64, time_base: Rational) -> TimelineTime {
    let numerator = frame.checked_mul(time_base.num).expect("test time fits i64");
    TimelineTime::new(numerator, time_base.den).expect("valid test time")
}

mod action_handler;
mod animation_authoring;
mod animation_state;
mod audio_authoring;
mod audio_idle_warmup;
mod audio_monitoring;
mod clip_authoring;
pub use audio_monitoring::ActiveAudioMonitoringPathEvidence;
#[cfg(test)]
mod audio_playback_acceptance;
mod audio_rendering;
mod basic_titles;
mod clip_clipboard;
mod clip_retime;
mod dynamic_hdr_authoring;
#[cfg(any(test, feature = "validation"))]
pub mod endurance_campaign;
#[cfg(any(test, feature = "validation"))]
pub mod endurance_export;
#[cfg(any(test, feature = "validation"))]
pub mod endurance_machine_plan;
#[cfg(any(test, feature = "validation"))]
pub mod endurance_playback;
#[cfg(any(test, feature = "validation"))]
pub mod endurance_product_runtime;
#[cfg(any(test, feature = "validation"))]
pub mod endurance_qualification;
#[cfg(any(test, feature = "validation"))]
pub mod endurance_recovery;
#[cfg(any(test, feature = "validation"))]
pub mod endurance_reference_output;
#[cfg(any(test, feature = "validation"))]
pub mod endurance_run_request;
mod endurance_shutdown;
#[cfg(any(test, feature = "validation"))]
pub mod endurance_source_inventory;
#[cfg(any(test, feature = "validation"))]
pub mod endurance_workload;
pub(crate) mod execution_resource_coordination;
pub(crate) mod execution_resource_slots;
pub(crate) mod exporting;
mod gallery_authoring;
#[cfg(any(test, feature = "validation"))]
pub mod golden_project_acceptance;
mod grade_authoring;
#[cfg(any(test, feature = "validation"))]
pub(crate) mod headless_preview_presentation;
#[cfg(any(test, feature = "validation"))]
pub(crate) mod headless_realtime_playback;
#[cfg(any(test, feature = "validation"))]
pub(crate) mod headless_viewer_gpu;
mod interchange;
pub mod media_asset_mutation;
mod media_import;
pub(crate) mod native_video_import;
mod packaged_worker;
mod playback;
pub(crate) mod viewer_gpu_device_progress;
pub(crate) mod viewer_gpu_publication;
pub(crate) mod viewer_gpu_submission;
pub(crate) use playback::{
    FramePresentationDisposition, FramePresentationPreflight, FramePresentationPublication,
};
#[cfg(test)]
mod playback_acceptance;
pub(crate) mod playback_preview;
pub(crate) mod preview_access_mode;
pub(crate) mod preview_cpu_execution;
pub(crate) mod preview_cpu_fallback_task;
pub(crate) mod preview_decode_residency;
pub(crate) mod preview_display_contract;
pub(crate) mod preview_execution;
mod preview_execution_input;
pub(crate) mod preview_frame_store;
pub(crate) mod preview_gpu_output_blocker;
pub(crate) mod preview_hardware_admission;
pub(crate) mod preview_media_frame;
pub(crate) mod preview_media_source;
pub(crate) mod preview_media_task;
pub(crate) mod preview_quality;
pub(crate) mod preview_raster_frame;
pub(crate) mod preview_render_cache;
mod preview_render_cache_identity;
pub mod preview_runtime;
pub(crate) mod preview_scheduler_policy;
pub(crate) mod preview_timeline_execution;
pub(crate) mod preview_title_task;
pub mod preview_unavailability;
pub(crate) mod preview_viewer_plan;
pub(crate) mod preview_visual_dependencies;
pub(crate) mod preview_visual_execution_task;
pub(crate) mod preview_work_notification;
pub mod product_action;
mod project_library_generation;
mod project_lifecycle;
#[cfg(any(test, feature = "validation"))]
pub use project_lifecycle::PreparedEnduranceProjectFixture;
pub(crate) use project_lifecycle::ProjectClosePoll;
mod project_persistence;
mod project_recovery;
pub(crate) mod project_runtime;
pub mod proxy_generation;
mod reference_output;
mod selection;
mod single_worker_activity;
pub mod thumbnail_service;
mod timeline_asset_placement;
mod timeline_clip_gesture;
mod timeline_commands;
mod timeline_insert;
mod timeline_position;
mod timeline_precompose;
mod timeline_range_edit;
mod timeline_selection_edit;
mod timeline_targeting;
pub mod ui_actions;
mod video_transitions;
pub mod viewer_gpu_output_health;
pub(crate) mod viewer_gpu_output_residency;
mod visual_mask_authoring;
pub mod visual_tracking;
pub mod waveform_service;

use self::ui_actions::TimelineSeekSource;
use audio_idle_warmup::{
    AudioIdleWarmupAuthorBinding, AudioIdleWarmupDemand, AudioIdleWarmupService,
    AudioIdleWarmupSubmitOutcome, AUDIO_IDLE_WARMUP_CHUNK_MILLIS,
};
pub use audio_idle_warmup::{
    AudioIdleWarmupDiagnostics, AudioIdleWarmupRequestIdentity, AudioIdleWarmupTerminal,
};
use audio_rendering::*;
use execution_resource_coordination::ExecutionResourceCoordinator;
pub use execution_resource_coordination::ExecutionResourcePressure;
use exporting::TimelineExportDraft;
pub use media_import::{
    MediaImportBatchId, MediaImportDiagnostics, MediaImportFailureReason, MediaImportTerminalRecord,
};
use media_import::{MediaImportExecution, PendingMediaImportBatch};
use proxy_generation::{
    ProxyGenerationDiagnostics, ProxyGenerationOrigin, ProxyGenerationRequestOutcome,
    ProxyGenerationService,
};
pub(crate) use selection::{find_clip_by_selection, find_clip_mut_by_selection};
pub use selection::{
    ClipSelectionMode, SelectedClipRef, SelectedEffectRef, SelectedTrackRef,
    SelectedVideoTransitionRef,
};
// Timeline edit algorithms live in mondrian-timeline; the App Adapter keeps
// gesture, selection, and frame-grid policy on top of them.
pub(crate) use mondrian_timeline::{
    apply_roll_edit, apply_sequence_track_conflicts_for_focus_group, apply_slide_edit,
    apply_slip_edit, apply_split_edit, apply_track_conflicts_for_focus_group, clip_selection_unit,
    expand_clip_selection_units, prepare_trimmed_clip_at_time, resolve_track_conflicts,
    resolve_track_overlaps, CutEditError, RollEditRequest, SlideEditRequest, SlipEditRequest,
    SplitEditRequest,
};
pub use video_transitions::VideoTransitionHandleState;

#[derive(Debug, Clone)]
pub struct DraggingAsset {
    pub asset_id: AssetId,
    pub name: String,
    pub kind: AssetKind,
    pub duration: Duration,
    pub has_linked_audio: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AnimationPropertySelection {
    pub clip_id: ClipId,
    pub property: AnimationParameterAddress,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AnimationKeyframeSelection {
    pub property: AnimationPropertySelection,
    pub keyframe_id: KeyframeId,
}

/// Unified timeline, clip, mask, and effect selection — single source of truth.
///
/// All panels read from and write to this struct. No panel maintains its
/// own copy of selection state.
#[derive(Debug, Clone, Default)]
pub struct SelectionState {
    /// Selected timeline tracks (supports multi-select from track headers).
    pub selected_track_ids: Vec<TrackId>,
    /// Selected clips (supports multi-select from timeline).
    pub selected_clips: Vec<SelectedClipRef>,
    /// Selected visual Transition in the active sequence.
    ///
    /// Transition identity is sufficient: Track membership is derived from
    /// its strong Clip endpoints and must never be mirrored here.
    pub selected_video_transition: Option<SelectedVideoTransitionRef>,
    /// Selected effect inside the primary clip, shared by Inspector and graph views.
    pub selected_effect: Option<SelectedEffectRef>,
    /// Currently selected mask (canvas → effect controls).
    pub selected_mask: Option<(MaskId, ClipId, TrackId)>,
}

#[derive(Debug, Clone, Default)]
pub struct AnimationSelectionState {
    pub active_property: Option<AnimationPropertySelection>,
    pub remembered_active_properties: HashMap<ClipId, AnimationParameterAddress>,
    pub selected_keyframes: HashSet<AnimationKeyframeSelection>,
    pub bubble_host: Option<AnimationBubbleHost>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnimationBubbleHost {
    Timeline,
    Graph,
}

#[derive(Debug, Clone)]
pub struct AnimationClipboardEntry {
    pub property: AnimationParameterAddress,
    pub relative_time: TimelineTime,
    pub keyframe: Keyframe<PropertyValue>,
}

#[derive(Debug, Clone, Default)]
pub struct AnimationClipboard {
    pub entries: Vec<AnimationClipboardEntry>,
}

/// Clip clipboard content used by app-level Copy/Cut/Paste actions.
#[derive(Debug, Clone, Default)]
pub struct ClipClipboard {
    entries: Vec<ClipClipboardEntry>,
    audio_transitions: Vec<mondrian_timeline::audio::AudioTransition>,
    video_transitions: Vec<mondrian_timeline::VideoTransition>,
}

/// One copied clip plus enough context to paste it back into the active sequence.
#[derive(Debug, Clone)]
struct ClipClipboardEntry {
    original_clip_id: ClipId,
    track_id: TrackId,
    is_video_track: bool,
    relative_start: TimelineTime,
    clip: Clip,
    audio_processing_scopes: Vec<mondrian_timeline::audio::AudioProcessingScope>,
}

/// Active app clipboard payload kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppClipboardKind {
    /// Animation keyframes copied from the selected clip.
    AnimationKeyframes,
    /// Timeline clips copied from the active sequence.
    Clips,
}

/// One user-visible runtime status message retained for diagnostics panels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLogEntry {
    /// Human-readable status message.
    pub message: String,
    /// Whether the message represents an error.
    pub is_error: bool,
}

pub use mondrian_timeline::ClipOverlapMode;

// ─────────────────────────────────────────────
//  AppState — 单向数据流中心
// ─────────────────────────────────────────────

pub struct AppState {
    // 全局事件总线
    pub event_bus: Arc<EventBus>,

    /// Sole mutable authority for the open Project document, asset library,
    /// navigation, author generations, and project-wide Undo/Redo.
    pub(crate) authoring: Option<AuthoringSession>,
    /// Kernel-backed exclusive authority for the open Project runtime.
    ///
    /// Background persistence requests clone this same lease; Project
    /// identity and runtime mutation authority never travel as a bare path.
    project_runtime_lease: Option<Arc<project_runtime::ProjectRuntimeLease>>,
    /// Immutable library directories awaiting the final external
    /// `Arc<AssetLibrary>` release before owner-authorized collection.
    retired_project_libraries: Vec<project_library_generation::RetiredProjectLibraryGeneration>,
    /// UI-independent single-writer durable archive publisher.
    project_persistence: ProjectPersistenceService,
    /// Non-blocking Project-close handoff owned by the application lifecycle.
    ///
    /// While present, the Authoring Session remains readable for projection,
    /// but no new product Action may mutate or enqueue work against it.
    pending_project_close: Option<project_lifecycle::PendingProjectClose>,
    /// Fail-closed lifecycle fault after a quiescence protocol violation.
    /// The in-memory Project is retained for diagnosis; mutation and further
    /// persistence remain disabled because worker ownership is unproven.
    project_close_fault: Option<project_lifecycle::ProjectCloseFault>,
    /// Latest admitted canonical manual-save destination for the open Session.
    ///
    /// This may lead `AuthoringSession::project_file` while Save As is in
    /// flight. Completions must match it before changing the durable baseline,
    /// canonical path, or Recovery Authority.
    manual_project_file_destination: Option<ManualProjectFileDestination>,
    /// Latest manual completion applied for its exact destination binding.
    ///
    /// This is delivery-order evidence, not a second authoring baseline. The
    /// Session remains sole authority for generation and Asset Library
    /// coverage; this receipt only proves that equal baseline state came from
    /// this worker lifetime rather than from opening an already-saved file.
    manual_project_file_applied_request: Option<(
        ManualProjectFileDestination,
        project_persistence::ProjectPersistenceRequestId,
    )>,
    /// Event-loop time of the latest admitted autosave request.
    autosave_last_requested_at: Instant,
    /// Snapshot identity currently being written as a recovery point.
    autosave_in_flight_request: Option<project_persistence::ProjectPersistenceRequestId>,
    /// Machine-local Viewer policy. It is runtime state, never Project or
    /// Sequence author data.
    viewer_display_management: DisplayManagementPolicy,
    /// Viewer-only frozen-still comparison; never Project author data.
    gallery_comparison: Option<gallery_authoring::GalleryComparisonState>,
    /// Machine-local professional clean-feed output; never Project author data.
    reference_output: reference_output::AppReferenceOutputService,

    // 播放状态
    /// Sole authority for transport position, epoch, and Clock Master.
    playback_engine: PlaybackEngine,
    /// Bounded production Adapter for versioned Playback Evidence.
    playback_evidence: PlaybackEvidenceCollector,
    /// High-water mark preventing evidence from regressing between event-loop ticks.
    playback_evidence_now: MonotonicTimestamp,
    /// Process-monotonic observation instant corresponding exactly to
    /// `playback_observation_time_anchor`.
    playback_observation_instant_anchor: Instant,
    /// Playback Engine timestamp paired with the observation instant anchor.
    playback_observation_time_anchor: MonotonicTimestamp,
    /// Most recent timeline seek interaction source used by preview access-mode selection.
    pub last_timeline_seek_source: TimelineSeekSource,

    // 正在拖拽的素材（从素材库拖向时间线）
    pub dragging_asset: Option<DraggingAsset>,
    /// Unified selection state — single source of truth for all panels.
    pub selection: SelectionState,
    /// UI-independent per-Sequence Track Targeting and Sync-Lock policy.
    timeline_targeting: timeline_targeting::TimelineTargetingState,
    /// Immutable, revision-bound visual-Transition handle observations.
    video_transition_handle_diagnostics: video_transitions::VideoTransitionHandleDiagnosticsCache,

    // 渲染导出队列
    pub(crate) render_queue: Arc<RenderQueue>,
    /// Product-level immutable execution resource decisions. Domain Modules
    /// retain their own queues, workers, cancellation, and terminal evidence.
    execution_resources: Arc<ExecutionResourceCoordinator>,
    /// Last export job-snapshot revision consumed by the app event-loop Adapter.
    export_jobs_observed_revision: u64,
    /// UI-stable timeline export draft shared by app UI export panels.
    pub export_draft: TimelineExportDraft,

    // 底部状态栏提示（message, is_error）
    pub status_hint: Option<(String, bool)>,
    /// Bounded history of user-visible status messages for diagnostics panels.
    pub status_log: Vec<StatusLogEntry>,

    // 动画选择状态（timeline / inspector / future graph 共用）
    pub animation_selection: AnimationSelectionState,
    pub animation_clipboard: Option<AnimationClipboard>,
    pub clip_clipboard: Option<ClipClipboard>,
    pub active_clipboard_kind: Option<AppClipboardKind>,

    // 代理策略
    pub auto_proxy_enabled: bool,
    proxy_generation: ProxyGenerationService,
    /// Last terminal publication consumed by the serialized App Adapter.
    ///
    /// Attempt IDs cannot serve as this cursor because concurrent attempts may
    /// publish out of admission order.
    proxy_terminal_observed_sequence: u64,

    // 音频时钟与 A/V 同步
    pub audio_sample_rate: u32,
    audio_playback: playback::AppAudioPlayback,
    /// Cumulative Audio failure facts retained when a terminal owner is replaced.
    #[cfg(any(test, feature = "validation"))]
    audio_endurance_failure_ledger: playback::AudioEnduranceFailureLedger,
    /// Open-Session audition intent and observations from the exact prepared Runtime.
    audio_monitoring: audio_monitoring::AudioMonitoringState,
    pub audio_source_cache: Arc<AudioSourceCache>,
    audio_idle_warmup: AudioIdleWarmupService,
    audio_idle_warmup_terminal_cursor: u64,

    media_import: MediaImportExecution,
    media_import_batches: HashMap<u64, PendingMediaImportBatch>,
    /// Ordered two-phase execution for relink and audio Component mutations.
    media_asset_mutations: media_asset_mutation::MediaAssetMutationExecution,
    /// Instance-owned, bounded Mask tracking execution and result cache.
    visual_tracking: visual_tracking::VisualTrackingService,
}

impl AppState {
    pub fn new() -> Self {
        let audio_sample_rate = 48_000;
        let audio_source_cache = Arc::new(AudioSourceCache::new(audio_sample_rate));
        let playback_observation_instant_anchor = Instant::now();

        Self {
            event_bus: EventBus::new(),
            authoring: None,
            project_runtime_lease: None,
            retired_project_libraries: Vec::new(),
            project_persistence: ProjectPersistenceService::new(),
            pending_project_close: None,
            project_close_fault: None,
            manual_project_file_destination: None,
            manual_project_file_applied_request: None,
            autosave_last_requested_at: Instant::now(),
            autosave_in_flight_request: None,
            viewer_display_management: DisplayManagementPolicy::default(),
            gallery_comparison: None,
            reference_output: reference_output::AppReferenceOutputService::default(),
            playback_engine: PlaybackEngine::default(),
            playback_evidence: PlaybackEvidenceCollector::default(),
            playback_evidence_now: MonotonicTimestamp::ZERO,
            playback_observation_instant_anchor,
            playback_observation_time_anchor: MonotonicTimestamp::ZERO,
            last_timeline_seek_source: TimelineSeekSource::Settled,
            dragging_asset: None,
            selection: SelectionState::default(),
            timeline_targeting: timeline_targeting::TimelineTargetingState::default(),
            video_transition_handle_diagnostics:
                video_transitions::VideoTransitionHandleDiagnosticsCache::default(),
            render_queue: RenderQueue::new(),
            execution_resources: ExecutionResourceCoordinator::new(Default::default()),
            export_jobs_observed_revision: 0,
            export_draft: TimelineExportDraft::default(),
            status_hint: None,
            status_log: Vec::new(),
            animation_selection: AnimationSelectionState::default(),
            animation_clipboard: None,
            clip_clipboard: None,
            active_clipboard_kind: None,
            auto_proxy_enabled: false,
            proxy_generation: ProxyGenerationService::new(),
            proxy_terminal_observed_sequence: 0,
            audio_sample_rate,
            audio_playback: playback::AppAudioPlayback::product_default(audio_sample_rate),
            #[cfg(any(test, feature = "validation"))]
            audio_endurance_failure_ledger: playback::AudioEnduranceFailureLedger::default(),
            audio_monitoring: audio_monitoring::AudioMonitoringState::default(),
            audio_source_cache,
            audio_idle_warmup: AudioIdleWarmupService::new(),
            audio_idle_warmup_terminal_cursor: 0,
            media_import: MediaImportExecution::new(),
            media_import_batches: HashMap::new(),
            media_asset_mutations: media_asset_mutation::MediaAssetMutationExecution::new(),
            visual_tracking: visual_tracking::VisualTrackingService::new(),
        }
    }

    pub fn set_status_hint(&mut self, message: impl Into<String>, is_error: bool) {
        let message = message.into();
        self.status_hint = Some((message.clone(), is_error));
        self.push_status_log(message, is_error);
    }

    pub fn clear_status_hint(&mut self) {
        self.status_hint = None;
    }

    fn push_status_log(&mut self, message: String, is_error: bool) {
        if message.trim().is_empty() {
            return;
        }
        if self
            .status_log
            .last()
            .is_some_and(|entry| entry.message == message && entry.is_error == is_error)
        {
            return;
        }
        self.status_log.push(StatusLogEntry { message, is_error });
        let overflow = self.status_log.len().saturating_sub(MAX_STATUS_LOG_ENTRIES);
        if overflow > 0 {
            self.status_log.drain(0..overflow);
        }
    }

    pub fn set_auto_proxy_enabled(&mut self, enabled: bool) {
        self.auto_proxy_enabled = enabled;
    }

    /// Canonical active Sequence. Execution and UI callers receive no mutable clone.
    pub fn active_sequence(&self) -> Option<&Sequence> {
        self.authoring.as_ref().and_then(AuthoringSession::active_sequence)
    }

    /// Canonical Project Gallery, if a Project is open.
    pub fn project_gallery(&self) -> Option<&mondrian_core::ProjectGallery> {
        self.authoring.as_ref().map(|session| &session.document().gallery)
    }

    /// Direct fixture access for tests that need to construct otherwise
    /// unreachable author states. Production edits must use an authoring
    /// transaction and can never borrow the canonical document mutably.
    #[cfg(test)]
    pub(crate) fn active_sequence_mut_uncommitted(&mut self) -> Option<&mut Sequence> {
        self.video_transition_handle_diagnostics.clear();
        self.authoring
            .as_mut()
            .and_then(|session| session.document_mut_for_test_fixture().sequences.active_mut())
    }

    /// Canonical Sequence collection.
    pub fn sequences(&self) -> &[Sequence] {
        self.authoring
            .as_ref()
            .map(|session| session.document().sequences.sequences.as_slice())
            .unwrap_or_default()
    }

    /// Active Sequence identity.
    pub fn active_sequence_id(&self) -> Option<SequenceId> {
        self.authoring
            .as_ref()
            .map(|session| session.document().sequences.active_sequence_id)
    }

    /// Default delivery Sequence identity.
    pub fn default_sequence_id(&self) -> Option<SequenceId> {
        self.authoring
            .as_ref()
            .map(|session| session.document().sequences.default_sequence_id)
    }

    /// Canonical project settings, or closed-project defaults for startup UI.
    pub fn project_settings(&self) -> &ProjectSettings {
        static CLOSED_PROJECT_SETTINGS: OnceLock<ProjectSettings> = OnceLock::new();
        self.authoring
            .as_ref()
            .map(|session| &session.document().settings)
            .unwrap_or_else(|| CLOSED_PROJECT_SETTINGS.get_or_init(ProjectSettings::default))
    }

    /// Project-wide color engine used by every Sequence execution contract.
    pub fn project_color_environment(&self) -> &mondrian_core::ProjectColorEnvironment {
        static CLOSED_PROJECT_COLOR_ENVIRONMENT: OnceLock<mondrian_core::ProjectColorEnvironment> =
            OnceLock::new();
        self.authoring
            .as_ref()
            .map(|session| &session.document().color_environment)
            .unwrap_or_else(|| {
                CLOSED_PROJECT_COLOR_ENVIRONMENT
                    .get_or_init(mondrian_core::ProjectColorEnvironment::default)
            })
    }

    /// Template copied into newly created Sequences.
    ///
    /// Existing Sequences never consult this value during execution.
    pub fn new_sequence_defaults(&self) -> &SequenceSettings {
        static CLOSED_PROJECT_DEFAULTS: OnceLock<SequenceSettings> = OnceLock::new();
        self.authoring
            .as_ref()
            .map(|session| &session.document().new_sequence_defaults)
            .unwrap_or_else(|| CLOSED_PROJECT_DEFAULTS.get_or_init(SequenceSettings::default))
    }

    /// Resolve the Project-owned color policy for source-library thumbnails.
    ///
    /// Thumbnails use the future-Sequence template for source interpretation,
    /// but always publish an sRGB display raster. Window Adapters consume this
    /// value and never interpret the Project color engine themselves.
    pub(crate) fn thumbnail_color_context(
        &self,
    ) -> Result<ProgramColorContext, mondrian_timeline::sequence::ProgramColorContextError> {
        let environment = self.project_color_environment();
        self.new_sequence_defaults()
            .root_program_color_context(environment)?
            .for_rendering_view_output(mondrian_core::types::ColorSpace::Srgb)
    }

    /// Machine-local Viewer display policy used by Window and Headless Adapters.
    pub fn viewer_display_management(&self) -> &DisplayManagementPolicy {
        &self.viewer_display_management
    }

    /// Install the validated machine-local Viewer display policy.
    ///
    /// This is runtime/user preference state. It never mutates Project or
    /// Sequence authoring data; Window observes the changed value and rebuilds
    /// the display-dependent output contract before publishing more pixels.
    pub fn set_viewer_display_management(&mut self, policy: DisplayManagementPolicy) -> bool {
        if self.viewer_display_management == policy {
            return false;
        }
        self.viewer_display_management = policy;
        true
    }

    /// Current author generation used by execution snapshots and cache identity.
    pub fn project_author_generation(&self) -> u64 {
        self.authoring
            .as_ref()
            .map(|session| session.author_generation().get())
            .unwrap_or(0)
    }

    /// Process-local identity of the currently open Authoring Session.
    pub fn authoring_session_id(&self) -> Option<AuthoringSessionId> {
        self.authoring.as_ref().map(AuthoringSession::session_id)
    }

    /// Current project identity.
    pub fn project_id(&self) -> Option<ProjectId> {
        self.authoring.as_ref().map(AuthoringSession::project_id)
    }

    /// Current project file path.
    pub fn current_project_path(&self) -> Option<&Path> {
        self.authoring.as_ref().map(AuthoringSession::project_file)
    }

    /// Current project runtime root.
    pub fn project_runtime_dir(&self) -> Option<&Path> {
        self.authoring.as_ref().map(AuthoringSession::runtime_root)
    }

    /// Project asset-library authority.
    pub fn asset_library(&self) -> Option<&AssetLibrary> {
        self.authoring.as_ref().map(AuthoringSession::asset_library).map(Arc::as_ref)
    }

    /// Cloneable project asset-library handle for background Adapters.
    pub fn asset_library_handle(&self) -> Option<Arc<AssetLibrary>> {
        self.authoring.as_ref().map(AuthoringSession::asset_library).cloned()
    }

    /// Canonical proxy-mode asset identities.
    pub fn proxy_mode_assets(&self) -> &BTreeSet<AssetId> {
        static EMPTY: OnceLock<BTreeSet<AssetId>> = OnceLock::new();
        self.authoring
            .as_ref()
            .map(|session| session.document().proxy_mode_assets.as_set())
            .unwrap_or_else(|| EMPTY.get_or_init(BTreeSet::new))
    }

    /// Project-wide bounded authoring history.
    pub fn authoring_history(&self) -> Option<&mondrian_editor_state::AuthoringHistory> {
        self.authoring.as_ref().map(AuthoringSession::history)
    }

    pub(crate) fn request_proxy_generation(
        &self,
        asset_id: AssetId,
        source_path: PathBuf,
        config: mondrian_media::ProxyConfig,
        color: mondrian_media::ProxyColorContract,
        origin: ProxyGenerationOrigin,
    ) -> ProxyGenerationRequestOutcome {
        let outcome = self.proxy_generation.request(asset_id, source_path, config, color, origin);
        let _ = self.refresh_internal_execution_resource_decision();
        outcome
    }

    #[cfg(test)]
    fn test_fixture_root() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!(
            "mondrian-app-test-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("create test fixture root");
        root
    }

    #[cfg(test)]
    fn test_default_authoring_session(
    ) -> (AuthoringSession, Arc<project_runtime::ProjectRuntimeLease>) {
        let root = Self::test_fixture_root();
        let sequence = Sequence::new("__mondrian_test_fixture__");
        let document = mondrian_project::ProjectDocument::new(
            "Test Project",
            SequenceCollection::new(sequence),
            mondrian_core::ProjectColorEnvironment::default(),
            SequenceSettings::default(),
            ProjectSettings::default(),
        );
        let project_file = root.join("project.mdp");
        let runtime_lease = project_runtime::claim_project_runtime_lease_for_test(
            &root.join("runtime-roots"),
            &project_file,
            document.project_id,
        )
        .expect("claim test Project runtime owner");
        let runtime_root = runtime_lease.runtime_root().to_path_buf();
        let library =
            AssetLibrary::open(runtime_root.join("library")).expect("open test asset library");
        (
            AuthoringSession::new_unsaved(document, project_file, runtime_root, library)
                .expect("create test authoring session"),
            runtime_lease,
        )
    }

    #[cfg(test)]
    fn test_ensure_authoring(&mut self) -> &mut AuthoringSession {
        if self.authoring.is_none() {
            let (session, runtime_lease) = Self::test_default_authoring_session();
            self.authoring = Some(session);
            self.project_runtime_lease = Some(runtime_lease);
            self.manual_project_file_destination = None;
            self.manual_project_file_applied_request = None;
        }
        self.authoring.as_mut().expect("test authoring session")
    }

    #[cfg(test)]
    fn test_normalize_sequence(mut sequence: Sequence) -> Sequence {
        let mut scopes = Vec::new();
        for track in &mut sequence.audio_tracks {
            for clip in &mut track.clips {
                if clip.audio_components.is_empty() {
                    let scope = mondrian_timeline::audio::AudioProcessingScope::identity();
                    clip.audio_components.push(
                        mondrian_timeline::audio::AudioComponentEdit::media(
                            AudioSourceComponentId::primary(),
                            scope.id,
                        ),
                    );
                    scopes.push(scope);
                }
            }
        }
        for scope in scopes {
            sequence.audio_program.add_processing_scope(scope);
        }
        sequence
    }
    /// Install or replace the active Sequence through the canonical Project document.
    #[cfg(test)]
    pub(crate) fn test_set_sequence(&mut self, sequence: Option<Sequence>) {
        let Some(sequence) = sequence else {
            self.visual_tracking.cancel_all();
            self.media_import.bind_project(None);
            self.media_import_batches.clear();
            self.media_asset_mutations.bind_project(None);
            self.authoring = None;
            self.project_runtime_lease = None;
            self.manual_project_file_destination = None;
            self.manual_project_file_applied_request = None;
            self.synchronize_audio_idle_warmup_binding();
            return;
        };
        let sequence = Self::test_normalize_sequence(sequence);
        let session = self.test_ensure_authoring();
        let document = session.document_mut_for_test_fixture();
        let previous_active = document.sequences.active_sequence_id;
        document.sequences.sequences.retain(|candidate| {
            candidate.id != sequence.id
                && candidate.id != previous_active
                && candidate.name != "__mondrian_test_fixture__"
        });
        let sequence_id = sequence.id;
        document.sequences.sequences.push(sequence);
        if document.sequences.default_sequence_id == previous_active
            || document.sequences.sequence(document.sequences.default_sequence_id).is_none()
        {
            document.sequences.default_sequence_id = sequence_id;
        }
        document.sequences.active_sequence_id = sequence_id;
    }

    #[cfg(test)]
    pub(crate) fn test_add_sequence(&mut self, sequence: Sequence) {
        let sequence = Self::test_normalize_sequence(sequence);
        let session = self.test_ensure_authoring();
        let document = session.document_mut_for_test_fixture();
        if document.sequences.sequence(sequence.id).is_none() {
            document.sequences.sequences.push(sequence);
        }
    }

    #[cfg(test)]
    pub(crate) fn test_set_sequences(&mut self, sequences: Vec<Sequence>) {
        let mut sequences =
            sequences.into_iter().map(Self::test_normalize_sequence).collect::<Vec<_>>();
        let session = self.test_ensure_authoring();
        let document = session.document_mut_for_test_fixture();
        if let Some(active) = document.sequences.active().cloned()
            && active.name != "__mondrian_test_fixture__"
            && sequences.iter().all(|sequence| sequence.id != active.id)
        {
            sequences.push(active);
        }
        assert!(
            !sequences.is_empty(),
            "test Sequence collection cannot be empty"
        );
        let fallback = sequences[0].id;
        document.sequences.sequences = sequences.into();
        if document.sequences.sequence(document.sequences.active_sequence_id).is_none() {
            document.sequences.active_sequence_id = fallback;
        }
        if document.sequences.sequence(document.sequences.default_sequence_id).is_none() {
            document.sequences.default_sequence_id = fallback;
        }
    }

    #[cfg(test)]
    pub(crate) fn test_set_active_sequence(&mut self, sequence_id: SequenceId) {
        let session = self.test_ensure_authoring();
        if session.document().sequences.sequence(sequence_id).is_some() {
            session.document_mut_for_test_fixture().sequences.active_sequence_id = sequence_id;
        }
    }

    #[cfg(test)]
    pub(crate) fn test_set_default_sequence(&mut self, sequence_id: SequenceId) {
        let session = self.test_ensure_authoring();
        if session.document().sequences.sequence(sequence_id).is_some() {
            session.document_mut_for_test_fixture().sequences.default_sequence_id = sequence_id;
        }
    }

    #[cfg(test)]
    pub(crate) fn test_set_navigation_stack(&mut self, stack: Vec<SequenceId>) {
        let session = self.test_ensure_authoring();
        let target = session.document().sequences.active_sequence_id;
        if let Some(first) = stack.first().copied() {
            session
                .switch_active_sequence(first, SequenceNavigationIntent::ReplaceRoot)
                .expect("valid test navigation Sequence");
            for sequence_id in stack.iter().copied().skip(1) {
                session
                    .switch_active_sequence(sequence_id, SequenceNavigationIntent::EnterNested)
                    .expect("valid test navigation Sequence");
            }
            session
                .switch_active_sequence(target, SequenceNavigationIntent::EnterNested)
                .expect("restore test active Sequence");
        }
    }

    #[cfg(test)]
    pub(crate) fn test_navigation_stack(&self) -> &[SequenceId] {
        self.authoring
            .as_ref()
            .map(AuthoringSession::navigation_stack)
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn test_set_asset_library(&mut self, library: Option<Arc<AssetLibrary>>) {
        let Some(library) = library else {
            self.visual_tracking.cancel_all();
            self.media_import.bind_project(None);
            self.media_import_batches.clear();
            self.media_asset_mutations.bind_project(None);
            self.authoring = None;
            self.project_runtime_lease = None;
            self.manual_project_file_destination = None;
            self.manual_project_file_applied_request = None;
            self.synchronize_audio_idle_warmup_binding();
            return;
        };
        let existing = self.authoring.take().unwrap_or_else(|| {
            let (session, runtime_lease) = Self::test_default_authoring_session();
            self.project_runtime_lease = Some(runtime_lease);
            session
        });
        let (document, project_file, runtime_root) = (
            existing.document().clone(),
            existing.project_file().to_path_buf(),
            existing.runtime_root().to_path_buf(),
        );
        self.authoring = Some(
            AuthoringSession::open_saved(document, project_file, runtime_root, library)
                .expect("replace test asset library"),
        );
        self.visual_tracking.cancel_all();
        self.manual_project_file_destination = None;
        self.manual_project_file_applied_request = None;
        self.synchronize_audio_idle_warmup_binding();
        self.media_import.bind_project(self.project_id());
        self.media_import_batches.clear();
        self.media_asset_mutations.bind_project(self.project_id());
    }

    #[cfg(test)]
    pub(crate) fn test_set_project_path(&mut self, project_file: PathBuf) {
        let existing = self.authoring.take().unwrap_or_else(|| {
            let (session, runtime_lease) = Self::test_default_authoring_session();
            self.project_runtime_lease = Some(runtime_lease);
            session
        });
        let document = existing.document().clone();
        let library = existing.asset_library().clone();
        let runtime_base = Self::test_fixture_root();
        let runtime_lease = project_runtime::claim_project_runtime_lease_for_test(
            &runtime_base.join("runtime-roots"),
            &project_file,
            document.project_id,
        )
        .expect("claim replacement Project runtime owner");
        let runtime_root = runtime_lease.runtime_root().to_path_buf();
        self.authoring = Some(
            AuthoringSession::open_saved(document, project_file, runtime_root, library)
                .expect("replace test project path"),
        );
        self.project_runtime_lease = Some(runtime_lease);
        self.manual_project_file_destination = None;
        self.manual_project_file_applied_request = None;
        self.synchronize_audio_idle_warmup_binding();
    }

    #[cfg(test)]
    pub(crate) fn test_advance_project_generation(&mut self) {
        {
            let session = self.test_ensure_authoring();
            let before = session.document().clone();
            let mut after = before.clone();
            after.meta.description.push('x');
            session
                .commit_project_snapshot("advance test generation", before, after)
                .expect("advance test author generation")
                .expect("generation-advancing commit");
        }
        self.synchronize_audio_idle_warmup_binding();
    }
    #[cfg(test)]
    pub(crate) fn test_project_settings_mut(&mut self) -> &mut ProjectSettings {
        &mut self.test_ensure_authoring().document_mut_for_test_fixture().settings
    }
    #[cfg(test)]
    pub(crate) fn test_new_sequence_defaults_mut(&mut self) -> &mut SequenceSettings {
        &mut self
            .test_ensure_authoring()
            .document_mut_for_test_fixture()
            .new_sequence_defaults
    }
    #[cfg(test)]
    pub(crate) fn test_project_color_environment_mut(
        &mut self,
    ) -> &mut mondrian_core::ProjectColorEnvironment {
        &mut self.test_ensure_authoring().document_mut_for_test_fixture().color_environment
    }
    #[cfg(test)]
    pub(crate) fn test_viewer_display_management_mut(&mut self) -> &mut DisplayManagementPolicy {
        &mut self.viewer_display_management
    }
    /// Observe completed/canceled proxy work for background UI refresh.
    pub fn poll_proxy_generation(&mut self) -> bool {
        if !self.proxy_generation.poll_finished() {
            return false;
        }
        let terminal_delta = self
            .proxy_generation
            .terminal_delta_after(self.proxy_terminal_observed_sequence);
        if terminal_delta.retention_gap {
            tracing::warn!(
                target: "mondrian::proxy",
                observed_terminal_sequence = self.proxy_terminal_observed_sequence,
                next_terminal_sequence = terminal_delta.next_cursor,
                retained_terminal_records = terminal_delta.records.len(),
                "proxy terminal consumer crossed the bounded evidence retention window"
            );
        }
        self.proxy_terminal_observed_sequence = terminal_delta.next_cursor;
        let current_generation = terminal_delta.generation;
        for terminal in terminal_delta.records {
            if terminal.evidence.disposition == mondrian_core::ExecutionTerminalDisposition::Failed
                && terminal.executed
                && terminal.evidence.generation == current_generation
                && let Some(detail) = terminal.failure_detail
            {
                self.set_status_hint(format!("代理生成失败：{detail}"), true);
            }
        }
        let _ = self.refresh_internal_execution_resource_decision();
        true
    }

    /// Snapshot bounded proxy-generation execution evidence.
    pub fn proxy_generation_diagnostics(&self) -> ProxyGenerationDiagnostics {
        self.proxy_generation.diagnostics()
    }

    /// Whether newly imported video media should enter proxy playback and start proxy generation.
    pub fn should_auto_generate_proxy_for_import(&self) -> bool {
        self.project_settings().proxy_enabled
    }

    /// Resolve project proxy settings into the media-layer proxy generator config.
    pub fn proxy_config(&self) -> mondrian_media::ProxyConfig {
        let mut config = mondrian_media::ProxyConfig {
            resolution: proxy_resolution_from_project(self.project_settings().proxy_resolution),
            ..mondrian_media::ProxyConfig::default()
        };
        if let Some(cache_dir) = self.project_settings().cache_dir.as_ref() {
            config.cache_dir = cache_dir.join("proxy");
        }
        config
    }

    pub fn is_asset_proxy_mode(&self, asset_id: AssetId) -> bool {
        self.authoring
            .as_ref()
            .is_some_and(|session| session.is_asset_proxy_mode(asset_id))
    }

    pub fn set_asset_proxy_mode(&mut self, asset_id: AssetId, enabled: bool) {
        if let Some(result) = self
            .authoring
            .as_mut()
            .map(|session| session.set_asset_proxy_mode(asset_id, enabled))
        {
            match result {
                Ok(Some(commit)) => self.consume_authoring_commit(commit),
                Ok(None) => {}
                Err(error) => {
                    self.set_status_hint(format!("切换代理模式失败：{error}"), true);
                }
            }
        }
    }
}

fn proxy_resolution_from_project(resolution: Resolution) -> mondrian_media::ProxyResolution {
    match resolution.height {
        0..=360 => mondrian_media::ProxyResolution::P360,
        361..=480 => mondrian_media::ProxyResolution::P480,
        481..=720 => mondrian_media::ProxyResolution::P720,
        _ => mondrian_media::ProxyResolution::P1080,
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod status_log_tests {
    use super::*;

    #[test]
    fn set_status_hint_records_bounded_status_log_without_duplicate_tail() {
        let mut state = AppState::new();

        state.set_status_hint("Ready", false);
        state.set_status_hint("Ready", false);
        state.set_status_hint("Failed", true);

        assert_eq!(
            state.status_log,
            vec![
                StatusLogEntry { message: "Ready".to_owned(), is_error: false },
                StatusLogEntry { message: "Failed".to_owned(), is_error: true },
            ]
        );

        for index in 0..(MAX_STATUS_LOG_ENTRIES + 4) {
            state.set_status_hint(format!("Message {index}"), false);
        }

        assert_eq!(state.status_log.len(), MAX_STATUS_LOG_ENTRIES);
        assert_eq!(
            state.status_log.first().expect("first status").message,
            "Message 4"
        );
        assert_eq!(
            state.status_log.last().expect("last status").message,
            format!("Message {}", MAX_STATUS_LOG_ENTRIES + 3)
        );
    }

    #[test]
    fn clear_status_hint_preserves_status_log_history() {
        let mut state = AppState::new();

        state.set_status_hint("Saved", false);
        state.clear_status_hint();

        assert!(state.status_hint.is_none());
        assert_eq!(state.status_log.len(), 1);
        assert_eq!(state.status_log[0].message, "Saved");
    }
}

#[cfg(test)]
mod color_policy_tests {
    use super::*;

    #[test]
    fn thumbnail_color_policy_is_resolved_by_app_state_for_srgb_publication() {
        let state = AppState::new();

        let context = state.thumbnail_color_context().expect("valid thumbnail context");

        assert_eq!(
            context.output_color_space().color(),
            Some(mondrian_core::types::ColorSpace::Srgb)
        );
        assert!(context.output_tone_map());
        assert_eq!(
            context.output_transform(),
            &mondrian_core::OutputTransformIntent::mondrian_standard()
        );
    }
}

fn audio_idle_warmup_enabled() -> bool {
    static AUDIO_IDLE_WARMUP: OnceLock<bool> = OnceLock::new();
    *AUDIO_IDLE_WARMUP.get_or_init(|| {
        std::env::var("MONDRIAN_AUDIO_IDLE_WARMUP")
            .map(|v| {
                let value = v.trim().to_ascii_lowercase();
                matches!(value.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(true)
    })
}

// ─────────────────────────────────────────────

pub(crate) fn app_data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    base.join("mondrian")
}

fn ensure_project_extension(path: PathBuf) -> PathBuf {
    let has_expected_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case(PROJECT_EXTENSION))
        .unwrap_or(false);

    if has_expected_ext {
        path
    } else {
        path.with_extension(PROJECT_EXTENSION)
    }
}

fn unix_now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as u64,
        Err(_) => 0,
    }
}

#[cfg(test)]
mod animation_selection_tests;
#[cfg(test)]
mod perf_tests;
#[cfg(test)]
mod timeline_edit_tests;
