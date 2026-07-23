//! Timeline surface primitive.
//!
//! `TimelineView` is intentionally domain-light: it renders tracks, clips,
//! playhead, scrolling, zooming, and selection in frame space. Editor crates can
//! map real `Sequence` / `Track` / `Clip` data into these view models without
//! pulling timeline command logic into the widget layer.

mod model;
mod paint;
mod transition;

use mondrian_core::types::AssetId;
#[cfg(test)]
use mondrian_core::types::Rational;
use mondrian_core::{Color, TimelineDisplayContract};
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{
    AccessibilityNode, AccessibilityRole, AccessibilityState, AccessibilityValue, CursorRequest,
    EventContext, PaintContext,
};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_theme::{colors::ColorTokens, current_theme, Theme};

#[cfg(test)]
use crate::menu::MenuItemKind;
use crate::paint::{centered_text_origin_y, color_with_alpha};
use crate::text_metrics::measure_single_line;
use crate::{ContextMenu, MenuItem, VectorIcon};

use self::model as timeline_model;

const TIMELINE_SNAP_THRESHOLD_PX: f32 = 8.0;
const TIMELINE_IN_OUT_MARKER_HIT_RADIUS: f32 = 5.0;
const TIMELINE_MIN_PIXELS_PER_FRAME: f32 = 0.25;
const TIMELINE_MAX_PIXELS_PER_FRAME: f32 = 64.0;

#[derive(Debug, Clone, Copy, PartialEq)]
struct TimelineMetrics {
    scrollbar_thickness: f32,
    scrollbar_min_thumb: f32,
    scrollbar_handle_size: f32,
    scrollbar_handle_visual_size: f32,
    scrollbar_track_visual_thickness: f32,
    scrollbar_body_visual_thickness: f32,
    scrollbar_gutter: f32,
    tool_button_size: f32,
    tool_button_gap: f32,
    toolbar_height: f32,
    toolbar_group_gap: f32,
    content_trailing_padding: f32,
    min_track_height: f32,
    max_track_height: f32,
    track_header_min_width: f32,
    default_track_height: f32,
    default_header_width: f32,
    default_ruler_height: f32,
    default_pixels_per_frame: f32,
}

impl TimelineMetrics {
    fn from_theme(theme: &Theme) -> Self {
        let spacing = &theme.spacing;
        Self {
            scrollbar_thickness: spacing.timeline_scrollbar_size,
            scrollbar_min_thumb: spacing.timeline_scrollbar_min_thumb,
            scrollbar_handle_size: spacing.timeline_scrollbar_handle_size,
            scrollbar_handle_visual_size: spacing.timeline_scrollbar_handle_visual_size,
            scrollbar_track_visual_thickness: spacing.timeline_scrollbar_track_visual_thickness,
            scrollbar_body_visual_thickness: spacing.timeline_scrollbar_body_visual_thickness,
            scrollbar_gutter: spacing.timeline_scrollbar_size,
            tool_button_size: spacing.timeline_tool_button_size,
            tool_button_gap: spacing.timeline_tool_button_gap,
            toolbar_height: spacing.timeline_toolbar_height,
            toolbar_group_gap: spacing.timeline_toolbar_group_gap,
            content_trailing_padding: spacing.timeline_content_trailing_padding,
            min_track_height: spacing.timeline_min_track_height,
            max_track_height: spacing.timeline_max_track_height,
            track_header_min_width: spacing.timeline_track_header_min_width,
            default_track_height: spacing.timeline_track_height,
            default_header_width: spacing
                .timeline_track_label_width
                .max(spacing.timeline_track_header_min_width),
            default_ruler_height: spacing.timeline_ruler_height,
            default_pixels_per_frame: spacing.timeline_default_pixels_per_frame,
        }
    }

    fn current() -> Self {
        let theme = current_theme();
        Self::from_theme(&theme)
    }
}

/// Action factory for clip selection.
pub type TimelineClipAction = dyn Fn(TimelineClipRef, &TimelineClip) -> Action;

/// Action factory for track selection.
pub type TimelineTrackAction = dyn Fn(TimelineTrackRef, &TimelineTrack) -> Action;

/// Action factory for track reorder commits.
pub type TimelineTrackMoveAction = dyn Fn(TimelineTrackMove, &TimelineTrack) -> Action;

/// Action factory for track header control commits.
pub type TimelineTrackControlAction =
    dyn Fn(TimelineTrackControl, TimelineTrackRef, &TimelineTrack) -> Action;

/// Action factory for adding a track from timeline menu entries.
pub type TimelineTrackAddAction = dyn Fn(TimelineTrackKind) -> Action;

/// Action factory for timeline-scoped editing commands.
pub type TimelineEditCommandAction = dyn Fn(TimelineEditCommand) -> Action;

/// Availability factory for timeline-scoped editing commands.
pub type TimelineEditCommandAvailability = dyn Fn(TimelineEditCommand) -> bool;

/// Shortcut-label factory for timeline-scoped editing commands.
pub type TimelineEditCommandShortcut = dyn Fn(TimelineEditCommand) -> Option<String>;

/// Action factory for playhead seeking.
pub type TimelineSeekAction = dyn Fn(TimelineSeek) -> Action;

/// User interaction source for a timeline seek.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineSeekSource {
    /// Continuous pointer drag on the ruler or playhead.
    PointerDrag,
    /// Stable pointer click, keyboard command, or drag release.
    Settled,
}

/// Timeline seek event emitted by [`TimelineView`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineSeek {
    /// Target timeline frame.
    pub frame: i64,
    /// Interaction source that produced the seek.
    pub source: TimelineSeekSource,
}

/// Action factory for dropping an asset onto a timeline track.
pub type TimelineAssetDropAction = dyn Fn(TimelineAssetDrop, &TimelineTrack) -> Action;

/// Action factory for clip move commits.
pub type TimelineClipMoveAction = dyn Fn(TimelineClipMove, &TimelineClip) -> Action;

/// Action factory for clip trim commits.
pub type TimelineClipTrimAction = dyn Fn(TimelineClipTrim, &TimelineClip) -> Action;

/// Action factory for visual-Transition selection.
pub type TimelineTransitionAction = dyn Fn(TimelineTransitionRef, &TimelineTransition) -> Action;

/// Action factory for visual-Transition range commits.
pub type TimelineTransitionResizeAction =
    dyn Fn(TimelineTransitionResize, &TimelineTransition) -> Action;

/// Action factory for creating a visual Transition at an adjacent edit.
pub type TimelineCutAction = dyn Fn(TimelineCutRef) -> Action;

/// Action factory for in/out point changes.
pub type TimelineInOutPointAction = dyn Fn(TimelineInOutPoint, i64) -> Action;

/// Stable view reference to a clip inside the timeline surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineClipRef {
    pub track_index: usize,
    pub clip_index: usize,
}

/// Stable view reference to a track inside the timeline surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineTrackRef {
    pub track_index: usize,
}

/// Stable view reference to a visual Transition inside one track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineTransitionRef {
    /// Display-order index of the containing track.
    pub track_index: usize,
    /// Display-order index of the Transition inside the track.
    pub transition_index: usize,
}

/// Stable view reference to one adjacent Clip edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineCutRef {
    /// Display-order index of the containing track.
    pub track_index: usize,
    /// Display-order index of the Clip ending at the cut.
    pub left_clip_index: usize,
    /// Display-order index of the Clip starting at the cut.
    pub right_clip_index: usize,
}

/// Track header control rendered by [`TimelineView`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineTrackControl {
    Visibility,
    Mute,
    Lock,
}

/// Domain-light clip move proposal emitted when a drag commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineClipMove {
    pub clip_ref: TimelineClipRef,
    pub old_start_frame: i64,
    pub new_start_frame: i64,
    pub new_track_index: usize,
}

/// Domain-light track reorder proposal emitted when a header drag commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineTrackMove {
    pub track_ref: TimelineTrackRef,
    pub old_track_index: usize,
    pub new_track_index: usize,
}

/// Domain-light asset drop proposal emitted for timeline drop targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineAssetDrop {
    pub asset_id: AssetId,
    pub track_ref: TimelineTrackRef,
    pub frame: i64,
}

/// Clip edge being trimmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineTrimEdge {
    In,
    Out,
}

/// Visual-Transition range edge being resized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineTransitionEdge {
    In,
    Out,
}

/// Sequence range marker edited from the timeline ruler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineInOutPoint {
    In,
    Out,
}

/// Domain-light clip trim proposal emitted when an edge drag commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineClipTrim {
    pub clip_ref: TimelineClipRef,
    pub edge: TimelineTrimEdge,
    pub old_start_frame: i64,
    pub old_duration_frames: i64,
    pub new_start_frame: i64,
    pub new_duration_frames: i64,
}

/// Domain-light visual-Transition resize proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineTransitionResize {
    /// Transition being resized.
    pub transition_ref: TimelineTransitionRef,
    /// Range edge controlled by the gesture.
    pub edge: TimelineTransitionEdge,
    /// Authored range start before the gesture.
    pub old_start_frame: i64,
    /// Authored range duration before the gesture.
    pub old_duration_frames: i64,
    /// Proposed range start after the gesture.
    pub new_start_frame: i64,
    /// Proposed range duration after the gesture.
    pub new_duration_frames: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimelineTransitionResizePosition {
    start_frame: i64,
    duration_frames: i64,
}

/// Track category used only for styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineTrackKind {
    Video,
    Audio,
}

/// Domain-light edit command emitted by timeline-focused keyboard input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineEditCommand {
    /// Cut the current timeline selection.
    CutSelection,
    /// Copy the current timeline selection.
    CopySelection,
    /// Paste clipboard content at the current timeline target.
    PasteAtPlayhead,
    /// Duplicate the current timeline selection.
    DuplicateSelection,
    /// Delete the current timeline selection.
    DeleteSelection,
    /// Ripple-delete the current timeline clip selection.
    RippleDeleteSelection,
    /// Split clips intersecting the playhead.
    SplitAtPlayhead,
    /// Trim selected clip starts to the playhead frame.
    TrimSelectionInToPlayhead,
    /// Trim selected clip ends to the playhead frame.
    TrimSelectionOutToPlayhead,
    /// Roll the selected edit point to the playhead frame.
    RollSelectedCutToPlayhead,
    /// Enable the current timeline clip selection.
    EnableSelection,
    /// Disable the current timeline clip selection.
    DisableSelection,
    /// Open one nested sequence clip.
    OpenNestedSequence(TimelineClipRef),
    /// Mark the current playhead frame as the sequence in point.
    MarkInAtPlayhead,
    /// Mark the current playhead frame as the sequence out point.
    MarkOutAtPlayhead,
    /// Clear the sequence in/out range.
    ClearInOutPoints,
    /// Toggle timeline playback from focused timeline keyboard input.
    TogglePlayback,
}

/// Pointer tool currently active inside the timeline surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineTool {
    /// Standard selection, trim, move, and seek behavior.
    Select,
    /// Split clips at the clicked frame by seeking there and dispatching
    /// [`TimelineEditCommand::SplitAtPlayhead`].
    Blade,
}

/// Icon slot for timeline toolbar controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineToolbarIconSlot {
    SelectTool,
    BladeTool,
    Snapping,
    MarkInAtPlayhead,
    MarkOutAtPlayhead,
}

/// Icon slot for timeline track-header controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineTrackControlIconSlot {
    VisibilityOn,
    VisibilityOff,
    MuteOff,
    MuteOn,
    LockOff,
    LockOn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimelineToolbarButton {
    Tool(TimelineTool),
    Snapping,
    Edit(TimelineEditCommand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimelineSnapKind {
    TimelineStart,
    Playhead,
    ClipStart,
    ClipEnd,
    InPoint,
    OutPoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimelineSnapTarget {
    frame: i64,
    kind: TimelineSnapKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimelineSnapResult {
    frame: i64,
    target: TimelineSnapTarget,
    delta_frames: i64,
}

/// Local UI state preserved across [`TimelineView`] model rebuilds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimelineViewState {
    /// Active pointer tool.
    pub active_tool: TimelineTool,
    /// Horizontal scroll offset in logical pixels.
    pub scroll_x: f32,
    /// Vertical scroll offset in logical pixels.
    pub scroll_y: f32,
    /// Current zoom in pixels per frame.
    pub pixels_per_frame: f32,
    /// Whether timeline snapping is enabled.
    pub snapping_enabled: bool,
    /// Track row height in logical pixels.
    pub track_height: f32,
}

/// Which semantic kind of timeline content a clip represents.
///
/// Used to select the correct theme token for body fill, hover, and
/// selected border without guessing from the track kind alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineClipKind {
    Video,
    Audio,
    Adjustment,
    NestedSequence,
    SolidColor,
}

/// Callback for paint-time waveform peak lookup.
///
/// The app wires this to its waveform source Adapter so the widget
/// layer stays decoupled from the audio infrastructure.
pub type WaveformLookupFn = dyn Fn(
    mondrian_core::AssetId,
    u64, // source_revision
    f64, // source_start_secs
    f64, // source_end_secs
    u32, // pixel_width
) -> Option<Vec<f32>>;

/// How audio waveforms are rendered on timeline clips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum WaveformDisplay {
    /// Top-half, bottom-aligned (Premiere-style). Only the positive
    /// amplitude envelope is drawn, rising from the clip's bottom edge.
    #[default]
    BottomAligned,
    /// Full waveform from the clip's vertical centre line, mirrored
    /// symmetrically above and below (DAW-style).
    Centered,
}

/// Clip view model rendered by [`TimelineView`].
#[derive(Debug, Clone)]
pub struct TimelineClip {
    pub kind: TimelineClipKind,
    pub label: String,
    pub start_frame: i64,
    pub duration_frames: i64,
    pub color: Option<Color>,
    pub selected: bool,
    pub disabled: bool,
    pub nested: bool,
    /// Audio source identity for paint-time waveform lookup.
    /// `None` for non-audio or unlinked clips.
    pub asset_id: Option<AssetId>,
    /// App-resolved media revision used to reject stale waveform artifacts.
    pub source_revision: u64,
    /// Source time range (seconds) covered by this clip.
    /// Used to extract the correct portion of the waveform envelope.
    pub source_start_secs: f64,
    pub source_end_secs: f64,
    /// Pre-computed waveform peaks (populated at paint time from the cache).
    /// Kept for backward-compatible test paths.
    pub waveform_peaks: Vec<f32>,
    pub select_action: Option<Action>,
}

impl TimelineClip {
    /// Create a clip view model in frame space.
    pub fn new(label: impl Into<String>, start_frame: i64, duration_frames: i64) -> Self {
        Self {
            kind: TimelineClipKind::Video,
            label: label.into(),
            start_frame,
            duration_frames: duration_frames.max(1),
            color: None,
            selected: false,
            disabled: false,
            nested: false,
            asset_id: None,
            source_revision: 0,
            source_start_secs: 0.0,
            source_end_secs: 1.0,
            waveform_peaks: Vec::new(),
            select_action: None,
        }
    }

    /// Set the clip tint.
    pub fn with_color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    /// Set the semantic clip kind for theme-driven color selection.
    pub fn kind(mut self, kind: TimelineClipKind) -> Self {
        self.kind = kind;
        self
    }

    /// Mark this clip as selected.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Mark this clip as disabled.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Mark this clip as a nested sequence.
    pub fn nested(mut self, nested: bool) -> Self {
        self.nested = nested;
        self
    }

    /// Attach precomputed normalized audio waveform peaks to this clip.
    pub fn with_waveform_peaks(mut self, peaks: impl Into<Vec<f32>>) -> Self {
        self.waveform_peaks = peaks.into().into_iter().map(|peak| peak.clamp(0.0, 1.0)).collect();
        self
    }

    /// Set audio source identity for paint-time waveform lookup.
    pub fn with_source_identity(
        mut self,
        asset_id: AssetId,
        source_revision: u64,
        source_start_secs: f64,
        source_end_secs: f64,
    ) -> Self {
        self.asset_id = Some(asset_id);
        self.source_revision = source_revision;
        self.source_start_secs = source_start_secs;
        self.source_end_secs = source_end_secs;
        self
    }

    /// Dispatch a static action when this clip is selected by input.
    pub fn with_select_action(mut self, action: Action) -> Self {
        self.select_action = Some(action);
        self
    }

    fn end_frame(&self) -> i64 {
        self.start_frame + self.duration_frames.max(1)
    }
}

/// Visual-Transition view model rendered above its two endpoint Clips.
#[derive(Debug, Clone)]
pub struct TimelineTransition {
    /// Display label for the Transition definition.
    pub label: String,
    /// Authored range start in Sequence frame space.
    pub start_frame: i64,
    /// Authored range duration in Sequence frame space.
    pub duration_frames: i64,
    /// Endpoint edit position in Sequence frame space.
    pub cut_frame: i64,
    /// Earliest range start allowed by endpoint placement geometry.
    pub minimum_start_frame: i64,
    /// Latest range end allowed by endpoint placement geometry.
    pub maximum_end_frame: i64,
    /// Whether this Transition is the current author selection.
    pub selected: bool,
    /// Whether the authored Transition participates in execution.
    pub enabled: bool,
    /// Human-readable fail-closed source-handle issue, when current external
    /// dependencies can no longer satisfy the authored Transition.
    pub handle_issue: Option<String>,
}

impl TimelineTransition {
    /// Create a visual-Transition view model in Sequence frame space.
    pub fn new(
        label: impl Into<String>,
        start_frame: i64,
        duration_frames: i64,
        cut_frame: i64,
        minimum_start_frame: i64,
        maximum_end_frame: i64,
    ) -> Self {
        let start_frame = start_frame.max(minimum_start_frame);
        let maximum_end_frame = maximum_end_frame.max(start_frame.saturating_add(1));
        let end_frame = start_frame.saturating_add(duration_frames.max(1)).min(maximum_end_frame);
        let duration_frames = end_frame.saturating_sub(start_frame).max(1);
        Self {
            label: label.into(),
            start_frame,
            duration_frames,
            cut_frame,
            minimum_start_frame,
            maximum_end_frame,
            selected: false,
            enabled: true,
            handle_issue: None,
        }
    }

    /// Mark this Transition as selected.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Mark this Transition as disabled in author state.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Attach a fail-closed source-handle diagnostic.
    pub fn with_handle_issue(mut self, issue: impl Into<String>) -> Self {
        self.handle_issue = Some(issue.into());
        self
    }
}

/// Track view model rendered by [`TimelineView`].
#[derive(Debug, Clone)]
pub struct TimelineTrack {
    pub label: String,
    pub kind: TimelineTrackKind,
    pub clips: Vec<TimelineClip>,
    pub transitions: Vec<TimelineTransition>,
    pub selected: bool,
    pub visible: bool,
    pub muted: bool,
    pub locked: bool,
    pub select_action: Option<Action>,
}

impl TimelineTrack {
    /// Create a video track.
    pub fn video(label: impl Into<String>, clips: Vec<TimelineClip>) -> Self {
        let clips = clips.into_iter().map(|c| c.kind(TimelineClipKind::Video)).collect();
        Self::new(label, TimelineTrackKind::Video, clips)
    }

    /// Create an audio track.
    pub fn audio(label: impl Into<String>, clips: Vec<TimelineClip>) -> Self {
        let clips = clips.into_iter().map(|c| c.kind(TimelineClipKind::Audio)).collect();
        Self::new(label, TimelineTrackKind::Audio, clips)
    }

    /// Create a track with an explicit kind.
    pub fn new(
        label: impl Into<String>,
        kind: TimelineTrackKind,
        clips: Vec<TimelineClip>,
    ) -> Self {
        Self {
            label: label.into(),
            kind,
            clips,
            transitions: Vec::new(),
            selected: false,
            visible: true,
            muted: false,
            locked: false,
            select_action: None,
        }
    }

    /// Attach visual Transitions projected for this video Track.
    pub fn with_transitions(mut self, transitions: Vec<TimelineTransition>) -> Self {
        self.transitions = transitions;
        self
    }

    /// Mark this track as selected.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Mark this track as visible in video/compositing output.
    pub fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Mark this track as muted.
    pub fn muted(mut self, muted: bool) -> Self {
        self.muted = muted;
        self
    }

    /// Mark this track as locked.
    pub fn locked(mut self, locked: bool) -> Self {
        self.locked = locked;
        self
    }

    /// Dispatch a static action when this track is selected by input.
    pub fn with_select_action(mut self, action: Action) -> Self {
        self.select_action = Some(action);
        self
    }
}

/// Scrollable, zoomable timeline surface.
pub struct TimelineView {
    id: WidgetId,
    tracks: Vec<TimelineTrack>,
    bounds: Rect,
    toolbar_rect: Rect,
    ruler_rect: Rect,
    header_rect: Rect,
    body_rect: Rect,
    selected_track: Option<TimelineTrackRef>,
    selected_clip: Option<TimelineClipRef>,
    selected_transition: Option<TimelineTransitionRef>,
    hovered_clip: Option<TimelineClipRef>,
    hovered_transition: Option<TimelineTransitionRef>,
    hovered_track_control: Option<(TimelineTrackRef, TimelineTrackControl)>,
    hovered_toolbar_button: Option<TimelineToolbarButton>,
    active_tool: TimelineTool,
    playhead_frame: i64,
    in_point_frame: i64,
    out_point_frame: Option<i64>,
    timeline_display: TimelineDisplayContract,
    snapping_enabled: bool,
    active_snap: Option<TimelineSnapResult>,
    pixels_per_frame: f32,
    scroll_x: f32,
    scroll_y: f32,
    focused: bool,
    focus_visible: bool,
    enabled: bool,
    track_height: f32,
    header_width: f32,
    ruler_height: f32,
    playhead_dragging: bool,
    in_out_drag: Option<TimelineInOutDrag>,
    asset_drop_hover: Option<TimelineAssetDropHover>,
    track_drag: Option<TimelineTrackDrag>,
    clip_drag: Option<TimelineClipDrag>,
    trim_drag: Option<TimelineTrimDrag>,
    transition_resize_drag: Option<TimelineTransitionResizeDrag>,
    scrollbar_drag: Option<TimelineScrollbarDrag>,
    pointer_capture_active: bool,
    context_menu: Option<ContextMenu>,
    horizontal_scrollbar_hovered: bool,
    vertical_scrollbar_hovered: bool,
    horizontal_scrollbar_hover_kind: Option<TimelineScrollbarDragKind>,
    vertical_scrollbar_hover_kind: Option<TimelineScrollbarDragKind>,
    on_clip_select: Option<Box<TimelineClipAction>>,
    on_track_select: Option<Box<TimelineTrackAction>>,
    on_track_move: Option<Box<TimelineTrackMoveAction>>,
    on_track_control: Option<Box<TimelineTrackControlAction>>,
    on_track_add: Option<Box<TimelineTrackAddAction>>,
    on_edit_command: Option<Box<TimelineEditCommandAction>>,
    on_edit_command_available: Option<Box<TimelineEditCommandAvailability>>,
    on_edit_command_shortcut: Option<Box<TimelineEditCommandShortcut>>,
    on_seek: Option<Box<TimelineSeekAction>>,
    on_asset_drop: Option<Box<TimelineAssetDropAction>>,
    on_clip_move: Option<Box<TimelineClipMoveAction>>,
    on_clip_trim: Option<Box<TimelineClipTrimAction>>,
    on_transition_select: Option<Box<TimelineTransitionAction>>,
    on_transition_resize: Option<Box<TimelineTransitionResizeAction>>,
    on_cut_transition_create: Option<Box<TimelineCutAction>>,
    on_in_out_point: Option<Box<TimelineInOutPointAction>>,
    toolbar_icons: Vec<(TimelineToolbarIconSlot, VectorIcon)>,
    track_control_icons: Vec<(TimelineTrackControlIconSlot, VectorIcon)>,
    empty_message: Option<String>,
    waveform_lookup: Option<Box<WaveformLookupFn>>,
    waveform_display: WaveformDisplay,
}

#[derive(Debug, Clone, Copy)]
struct TimelineAssetDropHover {
    asset_id: AssetId,
    track_index: usize,
    frame: i64,
}

#[derive(Debug, Clone, Copy)]
struct TimelineClipDrag {
    clip_ref: TimelineClipRef,
    old_start_frame: i64,
    pointer_offset_frames: i64,
    current_start_frame: i64,
    current_track_index: usize,
    moved: bool,
}

#[derive(Debug, Clone, Copy)]
struct TimelineTrackDrag {
    track_ref: TimelineTrackRef,
    current_track_index: usize,
    moved: bool,
}

#[derive(Debug, Clone, Copy)]
struct TimelineTrimDrag {
    clip_ref: TimelineClipRef,
    edge: TimelineTrimEdge,
    old_start_frame: i64,
    old_duration_frames: i64,
    current_start_frame: i64,
    current_duration_frames: i64,
    moved: bool,
}

#[derive(Debug, Clone, Copy)]
struct TimelineTransitionResizeDrag {
    transition_ref: TimelineTransitionRef,
    edge: TimelineTransitionEdge,
    old_start_frame: i64,
    old_duration_frames: i64,
    current_start_frame: i64,
    current_duration_frames: i64,
    moved: bool,
}

#[derive(Debug, Clone, Copy)]
struct TimelineInOutDrag {
    point: TimelineInOutPoint,
    start_frame: i64,
    current_frame: i64,
    moved: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimelineScrollbarAxis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimelineScrollbarDragKind {
    Thumb,
    LeadingHandle,
    TrailingHandle,
}

#[derive(Debug, Clone, Copy)]
struct TimelineScrollbarDrag {
    axis: TimelineScrollbarAxis,
    kind: TimelineScrollbarDragKind,
    start_pointer: f32,
    start_scroll: f32,
    start_pixels_per_frame: f32,
    start_track_height: f32,
}

impl TimelineView {
    /// Create a timeline view with the provided tracks.
    pub fn new(tracks: Vec<TimelineTrack>) -> Self {
        let metrics = TimelineMetrics::current();
        let selected_transition = tracks.iter().enumerate().find_map(|(track_index, track)| {
            track
                .transitions
                .iter()
                .position(|transition| transition.selected)
                .map(|transition_index| TimelineTransitionRef { track_index, transition_index })
        });
        Self {
            id: WidgetId::new(),
            tracks,
            bounds: Rect::ZERO,
            toolbar_rect: Rect::ZERO,
            ruler_rect: Rect::ZERO,
            header_rect: Rect::ZERO,
            body_rect: Rect::ZERO,
            selected_track: None,
            selected_clip: None,
            selected_transition,
            hovered_clip: None,
            hovered_transition: None,
            hovered_track_control: None,
            hovered_toolbar_button: None,
            active_tool: TimelineTool::Select,
            playhead_frame: 0,
            in_point_frame: 0,
            out_point_frame: None,
            timeline_display: TimelineDisplayContract::default(),
            snapping_enabled: true,
            active_snap: None,
            pixels_per_frame: metrics.default_pixels_per_frame,
            scroll_x: 0.0,
            scroll_y: 0.0,
            focused: false,
            focus_visible: false,
            enabled: true,
            track_height: metrics.default_track_height,
            header_width: metrics.default_header_width,
            ruler_height: metrics.default_ruler_height,
            playhead_dragging: false,
            in_out_drag: None,
            asset_drop_hover: None,
            track_drag: None,
            clip_drag: None,
            trim_drag: None,
            transition_resize_drag: None,
            scrollbar_drag: None,
            pointer_capture_active: false,
            context_menu: None,
            horizontal_scrollbar_hovered: false,
            vertical_scrollbar_hovered: false,
            horizontal_scrollbar_hover_kind: None,
            vertical_scrollbar_hover_kind: None,
            on_clip_select: None,
            on_track_select: None,
            on_track_move: None,
            on_track_control: None,
            on_track_add: None,
            on_edit_command: None,
            on_edit_command_available: None,
            on_edit_command_shortcut: None,
            on_seek: None,
            on_asset_drop: None,
            on_clip_move: None,
            on_clip_trim: None,
            on_transition_select: None,
            on_transition_resize: None,
            on_cut_transition_create: None,
            on_in_out_point: None,
            toolbar_icons: Vec::new(),
            track_control_icons: Vec::new(),
            empty_message: None,
            waveform_lookup: None,
            waveform_display: WaveformDisplay::BottomAligned,
        }
    }

    /// Set the current playhead frame.
    pub fn with_playhead(mut self, frame: i64) -> Self {
        self.playhead_frame = frame.max(0);
        self
    }

    /// Update the current playhead frame without rebuilding the timeline model.
    pub fn set_playhead_frame(&mut self, frame: i64) {
        self.playhead_frame = frame.max(0);
    }

    /// Set optional sequence in/out points in frame space.
    pub fn with_in_out_points(mut self, in_point_frame: i64, out_point_frame: Option<i64>) -> Self {
        self.in_point_frame = in_point_frame.max(0);
        self.out_point_frame = out_point_frame.map(|frame| frame.max(self.in_point_frame));
        self
    }

    /// Set the validated frame-grid and position-label display contract.
    pub fn with_timeline_display(mut self, display: TimelineDisplayContract) -> Self {
        self.timeline_display = display;
        self
    }

    /// Set initial zoom in pixels per frame.
    pub fn with_pixels_per_frame(mut self, pixels_per_frame: f32) -> Self {
        self.pixels_per_frame =
            pixels_per_frame.clamp(TIMELINE_MIN_PIXELS_PER_FRAME, TIMELINE_MAX_PIXELS_PER_FRAME);
        self
    }

    /// Set initial track height in logical pixels.
    pub fn with_track_height(mut self, track_height: f32) -> Self {
        let metrics = TimelineMetrics::current();
        self.track_height = track_height.clamp(metrics.min_track_height, metrics.max_track_height);
        self
    }

    /// Set the track header width.
    pub fn with_header_width(mut self, width: f32) -> Self {
        self.header_width = width.max(TimelineMetrics::current().track_header_min_width);
        self
    }

    /// Set whether the timeline accepts pointer, keyboard, wheel, and focus input.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.set_enabled(enabled);
        self
    }

    /// Disable pointer, keyboard, wheel, and focus input.
    pub fn disabled(self) -> Self {
        self.enabled(false)
    }

    /// Whether the timeline accepts user input.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Set whether the timeline accepts pointer, keyboard, wheel, and focus input.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.focused = false;
            self.focus_visible = false;
            self.hovered_clip = None;
            self.hovered_transition = None;
            self.hovered_track_control = None;
            self.hovered_toolbar_button = None;
            self.active_tool = TimelineTool::Select;
            self.selected_track = None;
            self.playhead_dragging = false;
            self.active_snap = None;
            self.in_out_drag = None;
            self.asset_drop_hover = None;
            self.track_drag = None;
            self.clip_drag = None;
            self.trim_drag = None;
            self.transition_resize_drag = None;
            self.scrollbar_drag = None;
            self.context_menu = None;
            self.horizontal_scrollbar_hovered = false;
            self.vertical_scrollbar_hovered = false;
            self.horizontal_scrollbar_hover_kind = None;
            self.vertical_scrollbar_hover_kind = None;
        }
    }

    fn request_timeline_pointer_capture(&mut self, ctx: &mut EventContext) {
        self.pointer_capture_active = true;
        ctx.request_pointer_capture(self.id);
    }

    fn release_timeline_pointer_capture(&mut self, ctx: &mut EventContext) {
        if self.pointer_capture_active {
            self.pointer_capture_active = false;
            ctx.release_pointer_capture(self.id);
        }
    }

    /// Set a dynamic clip-selection action factory.
    pub fn on_clip_select(
        mut self,
        action: impl Fn(TimelineClipRef, &TimelineClip) -> Action + 'static,
    ) -> Self {
        self.on_clip_select = Some(Box::new(action));
        self
    }

    /// Set a dynamic track-selection action factory.
    pub fn on_track_select(
        mut self,
        action: impl Fn(TimelineTrackRef, &TimelineTrack) -> Action + 'static,
    ) -> Self {
        self.on_track_select = Some(Box::new(action));
        self
    }

    /// Set a dynamic track-reorder action factory.
    pub fn on_track_move(
        mut self,
        action: impl Fn(TimelineTrackMove, &TimelineTrack) -> Action + 'static,
    ) -> Self {
        self.on_track_move = Some(Box::new(action));
        self
    }

    /// Set a dynamic track-control action factory.
    pub fn on_track_control(
        mut self,
        action: impl Fn(TimelineTrackControl, TimelineTrackRef, &TimelineTrack) -> Action + 'static,
    ) -> Self {
        self.on_track_control = Some(Box::new(action));
        self
    }

    /// Set a dynamic add-track action factory for timeline menu entries.
    pub fn on_track_add(mut self, action: impl Fn(TimelineTrackKind) -> Action + 'static) -> Self {
        self.on_track_add = Some(Box::new(action));
        self
    }

    /// Set a vector icon used to paint one timeline toolbar button.
    pub fn with_toolbar_icon(mut self, slot: TimelineToolbarIconSlot, icon: VectorIcon) -> Self {
        if let Some((_, existing)) =
            self.toolbar_icons.iter_mut().find(|(candidate, _)| *candidate == slot)
        {
            *existing = icon;
        } else {
            self.toolbar_icons.push((slot, icon));
        }
        self
    }

    /// Set a vector icon used to paint one track-header control state.
    pub fn with_track_control_icon(
        mut self,
        slot: TimelineTrackControlIconSlot,
        icon: VectorIcon,
    ) -> Self {
        if let Some((_, existing)) =
            self.track_control_icons.iter_mut().find(|(candidate, _)| *candidate == slot)
        {
            *existing = icon;
        } else {
            self.track_control_icons.push((slot, icon));
        }
        self
    }

    /// Set the message shown in the timeline body when there are no tracks.
    pub fn with_empty_message(mut self, message: impl Into<String>) -> Self {
        self.empty_message = Some(message.into());
        self
    }

    /// Set a paint-time waveform lookup callback for rendering audio clip peaks.
    pub fn with_waveform_lookup(
        mut self,
        lookup: impl Fn(mondrian_core::AssetId, u64, f64, f64, u32) -> Option<Vec<f32>> + 'static,
    ) -> Self {
        self.waveform_lookup = Some(Box::new(lookup));
        self
    }

    /// Set the waveform display mode.
    /// [`WaveformDisplay::BottomAligned`] is the default (Premiere-style).
    pub fn with_waveform_display(mut self, mode: WaveformDisplay) -> Self {
        self.waveform_display = mode;
        self
    }

    /// Set a dynamic edit-command action factory.
    pub fn on_edit_command(
        mut self,
        action: impl Fn(TimelineEditCommand) -> Action + 'static,
    ) -> Self {
        self.on_edit_command = Some(Box::new(action));
        self
    }

    /// Set a dynamic edit-command availability factory.
    pub fn on_edit_command_available(
        mut self,
        available: impl Fn(TimelineEditCommand) -> bool + 'static,
    ) -> Self {
        self.on_edit_command_available = Some(Box::new(available));
        self
    }

    /// Set a shortcut label factory for edit-command context menu rows.
    pub fn on_edit_command_shortcut(
        mut self,
        shortcut: impl Fn(TimelineEditCommand) -> Option<String> + 'static,
    ) -> Self {
        self.on_edit_command_shortcut = Some(Box::new(shortcut));
        self
    }

    /// Set a dynamic seek action factory.
    pub fn on_seek(mut self, action: impl Fn(TimelineSeek) -> Action + 'static) -> Self {
        self.on_seek = Some(Box::new(action));
        self
    }

    /// Set a dynamic action factory for asset drops onto timeline tracks.
    pub fn on_asset_drop(
        mut self,
        action: impl Fn(TimelineAssetDrop, &TimelineTrack) -> Action + 'static,
    ) -> Self {
        self.on_asset_drop = Some(Box::new(action));
        self
    }

    /// Set a dynamic clip-move action factory.
    pub fn on_clip_move(
        mut self,
        action: impl Fn(TimelineClipMove, &TimelineClip) -> Action + 'static,
    ) -> Self {
        self.on_clip_move = Some(Box::new(action));
        self
    }

    /// Set a dynamic clip-trim action factory.
    pub fn on_clip_trim(
        mut self,
        action: impl Fn(TimelineClipTrim, &TimelineClip) -> Action + 'static,
    ) -> Self {
        self.on_clip_trim = Some(Box::new(action));
        self
    }

    /// Set a dynamic visual-Transition selection action factory.
    pub fn on_transition_select(
        mut self,
        action: impl Fn(TimelineTransitionRef, &TimelineTransition) -> Action + 'static,
    ) -> Self {
        self.on_transition_select = Some(Box::new(action));
        self
    }

    /// Set a dynamic visual-Transition resize action factory.
    pub fn on_transition_resize(
        mut self,
        action: impl Fn(TimelineTransitionResize, &TimelineTransition) -> Action + 'static,
    ) -> Self {
        self.on_transition_resize = Some(Box::new(action));
        self
    }

    /// Set an action factory for creating a Transition at an adjacent edit.
    pub fn on_cut_transition_create(
        mut self,
        action: impl Fn(TimelineCutRef) -> Action + 'static,
    ) -> Self {
        self.on_cut_transition_create = Some(Box::new(action));
        self
    }

    /// Set an action factory for ruler in/out point edits.
    pub fn on_in_out_point(
        mut self,
        action: impl Fn(TimelineInOutPoint, i64) -> Action + 'static,
    ) -> Self {
        self.on_in_out_point = Some(Box::new(action));
        self
    }

    /// Current selected clip reference.
    pub fn selected_clip(&self) -> Option<TimelineClipRef> {
        self.selected_clip
    }

    /// Current locally selected visual Transition reference.
    pub fn selected_transition(&self) -> Option<TimelineTransitionRef> {
        self.selected_transition
    }

    /// Current locally selected track reference.
    pub fn selected_track(&self) -> Option<TimelineTrackRef> {
        self.selected_track
    }

    /// Current playhead frame.
    pub fn playhead_frame(&self) -> i64 {
        self.playhead_frame
    }

    /// Current timeline in point frame.
    pub fn in_point_frame(&self) -> i64 {
        self.in_point_frame
    }

    /// Current timeline out point frame.
    pub fn out_point_frame(&self) -> Option<i64> {
        self.out_point_frame
    }

    /// Current horizontal scroll offset in pixels.
    pub fn scroll_x(&self) -> f32 {
        self.scroll_x
    }

    /// Current vertical scroll offset in pixels.
    pub fn scroll_y(&self) -> f32 {
        self.scroll_y
    }

    /// Current zoom in pixels per frame.
    pub fn pixels_per_frame(&self) -> f32 {
        self.pixels_per_frame
    }

    /// Current track row height in logical pixels.
    pub fn track_height(&self) -> f32 {
        self.track_height
    }

    /// Active pointer tool for the timeline surface.
    pub fn active_tool(&self) -> TimelineTool {
        self.active_tool
    }

    /// Whether timeline snapping is enabled.
    pub fn snapping_enabled(&self) -> bool {
        self.snapping_enabled
    }

    /// Snapshot local timeline UI state for rebuild preservation.
    pub fn state(&self) -> TimelineViewState {
        TimelineViewState {
            active_tool: self.active_tool,
            scroll_x: self.scroll_x,
            scroll_y: self.scroll_y,
            pixels_per_frame: self.pixels_per_frame,
            snapping_enabled: self.snapping_enabled,
            track_height: self.track_height,
        }
    }

    /// Restore local timeline UI state after rebuilding from fresh models.
    pub fn restore_state(&mut self, state: &TimelineViewState) {
        self.active_tool = state.active_tool;
        self.pixels_per_frame = state
            .pixels_per_frame
            .clamp(TIMELINE_MIN_PIXELS_PER_FRAME, TIMELINE_MAX_PIXELS_PER_FRAME);
        self.snapping_enabled = state.snapping_enabled;
        let metrics = TimelineMetrics::current();
        self.track_height =
            state.track_height.clamp(metrics.min_track_height, metrics.max_track_height);
        self.scroll_x = state.scroll_x.max(0.0);
        self.scroll_y = state.scroll_y.max(0.0);
        if self.body_rect.width > 0.0 || self.body_rect.height > 0.0 {
            self.clamp_scroll();
        }
    }

    fn content_width(&self) -> f32 {
        self.max_content_frame() as f32 * self.pixels_per_frame
            + TimelineMetrics::current().content_trailing_padding
    }

    fn max_content_frame(&self) -> i64 {
        let max_frame = self
            .tracks
            .iter()
            .flat_map(|track| track.clips.iter().map(TimelineClip::end_frame))
            .max()
            .unwrap_or(240)
            .max(240);
        max_frame
    }

    fn content_height(&self) -> f32 {
        self.tracks.len() as f32 * self.track_height
    }

    fn max_scroll_x(&self) -> f32 {
        (self.content_width() - self.body_rect.width).max(0.0)
    }

    fn max_scroll_y(&self) -> f32 {
        (self.content_height() - self.body_rect.height).max(0.0)
    }

    fn clamp_scroll(&mut self) {
        self.scroll_x = self.scroll_x.clamp(0.0, self.max_scroll_x());
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll_y());
    }

    fn set_scroll_x(&mut self, scroll_x: f32) -> bool {
        let old = self.scroll_x;
        self.scroll_x = scroll_x.clamp(0.0, self.max_scroll_x());
        (self.scroll_x - old).abs() > 0.01
    }

    fn set_scroll_y(&mut self, scroll_y: f32) -> bool {
        let old = self.scroll_y;
        self.scroll_y = scroll_y.clamp(0.0, self.max_scroll_y());
        (self.scroll_y - old).abs() > 0.01
    }

    fn horizontal_scrollbar_track_rect(&self) -> Option<Rect> {
        timeline_model::horizontal_scrollbar_track_rect(self.body_rect)
    }

    fn vertical_scrollbar_track_rect(&self) -> Option<Rect> {
        timeline_model::vertical_scrollbar_track_rect(self.body_rect)
    }

    fn scrollbar_clip_rect(&self) -> Rect {
        timeline_model::scrollbar_clip_rect(self.body_rect)
    }

    fn horizontal_scrollbar_thumb_rect(&self) -> Option<Rect> {
        let track = self.horizontal_scrollbar_track_rect()?;
        let content_width = self.content_width();
        let metrics = TimelineMetrics::current();
        let thumb_width = (self.body_rect.width / content_width * track.width)
            .max(metrics.scrollbar_min_thumb)
            .min(track.width);
        let travel = (track.width - thumb_width).max(0.0);
        let offset_ratio = if self.max_scroll_x() > 0.0 {
            self.scroll_x / self.max_scroll_x()
        } else {
            0.0
        };
        Some(Rect::new(
            track.x + travel * offset_ratio,
            track.y,
            thumb_width.min(track.width),
            track.height,
        ))
    }

    fn horizontal_scrollbar_handle_rect(&self, kind: TimelineScrollbarDragKind) -> Option<Rect> {
        let thumb = self.horizontal_scrollbar_thumb_rect()?;
        let center_x = match kind {
            TimelineScrollbarDragKind::LeadingHandle => thumb.x,
            TimelineScrollbarDragKind::TrailingHandle => thumb.x + thumb.width,
            TimelineScrollbarDragKind::Thumb => return None,
        };
        let metrics = TimelineMetrics::current();
        Some(Rect::new(
            center_x - metrics.scrollbar_handle_size * 0.5,
            thumb.center().y - metrics.scrollbar_handle_size * 0.5,
            metrics.scrollbar_handle_size,
            metrics.scrollbar_handle_size,
        ))
    }

    fn horizontal_scrollbar_body_rect(&self) -> Option<Rect> {
        let thumb = self.horizontal_scrollbar_thumb_rect()?;
        let metrics = TimelineMetrics::current();
        Some(Rect::new(
            thumb.x,
            thumb.center().y - metrics.scrollbar_body_visual_thickness * 0.5,
            thumb.width,
            metrics.scrollbar_body_visual_thickness,
        ))
    }

    fn vertical_scrollbar_body_rect(&self) -> Option<Rect> {
        let thumb = self.vertical_scrollbar_thumb_rect()?;
        let metrics = TimelineMetrics::current();
        Some(Rect::new(
            thumb.center().x - metrics.scrollbar_body_visual_thickness * 0.5,
            thumb.y,
            metrics.scrollbar_body_visual_thickness,
            thumb.height,
        ))
    }

    fn vertical_scrollbar_thumb_rect(&self) -> Option<Rect> {
        let track = self.vertical_scrollbar_track_rect()?;
        let content_height = self.content_height();
        let metrics = TimelineMetrics::current();
        let thumb_height = (self.body_rect.height / content_height * track.height)
            .max(metrics.scrollbar_min_thumb)
            .min(track.height);
        let travel = (track.height - thumb_height).max(0.0);
        let offset_ratio = if self.max_scroll_y() > 0.0 {
            self.scroll_y / self.max_scroll_y()
        } else {
            0.0
        };
        Some(Rect::new(
            track.x,
            track.y + travel * offset_ratio,
            track.width,
            thumb_height.min(track.height),
        ))
    }

    fn vertical_scrollbar_handle_rect(&self, kind: TimelineScrollbarDragKind) -> Option<Rect> {
        let thumb = self.vertical_scrollbar_thumb_rect()?;
        let center_y = match kind {
            TimelineScrollbarDragKind::LeadingHandle => thumb.y,
            TimelineScrollbarDragKind::TrailingHandle => thumb.y + thumb.height,
            TimelineScrollbarDragKind::Thumb => return None,
        };
        let metrics = TimelineMetrics::current();
        Some(Rect::new(
            thumb.center().x - metrics.scrollbar_handle_size * 0.5,
            center_y - metrics.scrollbar_handle_size * 0.5,
            metrics.scrollbar_handle_size,
            metrics.scrollbar_handle_size,
        ))
    }

    fn scroll_x_for_thumb_delta(&self, delta_x: f32, drag: TimelineScrollbarDrag) -> f32 {
        let Some(track) = self.horizontal_scrollbar_track_rect() else {
            return self.scroll_x;
        };
        let Some(thumb) = self.horizontal_scrollbar_thumb_rect() else {
            return self.scroll_x;
        };
        timeline_model::scrollbar_scroll_for_thumb_delta(
            track.width,
            thumb.width,
            self.max_scroll_x(),
            drag.start_scroll,
            delta_x,
        )
    }

    fn scroll_y_for_thumb_delta(&self, delta_y: f32, drag: TimelineScrollbarDrag) -> f32 {
        let Some(track) = self.vertical_scrollbar_track_rect() else {
            return self.scroll_y;
        };
        let Some(thumb) = self.vertical_scrollbar_thumb_rect() else {
            return self.scroll_y;
        };
        timeline_model::scrollbar_scroll_for_thumb_delta(
            track.height,
            thumb.height,
            self.max_scroll_y(),
            drag.start_scroll,
            delta_y,
        )
    }

    fn zoom_x_for_handle_delta(&mut self, delta_x: f32, drag: TimelineScrollbarDrag) -> bool {
        let Some(track) = self.horizontal_scrollbar_track_rect() else {
            return false;
        };
        let Some(update) = timeline_model::horizontal_zoom_for_handle_delta(
            self.body_rect.width,
            track.width,
            self.max_content_frame() as f32,
            delta_x,
            drag.kind,
            drag.start_scroll,
            drag.start_pixels_per_frame,
        ) else {
            return false;
        };
        let old_pixels = self.pixels_per_frame;
        let old_scroll = self.scroll_x;
        self.pixels_per_frame = update.pixels_per_frame;
        self.scroll_x = update.scroll_x;
        self.clamp_scroll();
        (self.pixels_per_frame - old_pixels).abs() > 0.001
            || (self.scroll_x - old_scroll).abs() > 0.01
    }

    fn resize_tracks_for_handle_delta(
        &mut self,
        delta_y: f32,
        drag: TimelineScrollbarDrag,
    ) -> bool {
        let Some(track) = self.vertical_scrollbar_track_rect() else {
            return false;
        };
        let Some(update) = timeline_model::track_resize_for_handle_delta(
            self.body_rect.height,
            track.height,
            self.tracks.len(),
            delta_y,
            drag.kind,
            drag.start_scroll,
            drag.start_track_height,
        ) else {
            return false;
        };
        let old_height = self.track_height;
        let old_scroll = self.scroll_y;
        self.track_height = update.track_height;
        self.scroll_y = update.scroll_y;
        self.clamp_scroll();
        (self.track_height - old_height).abs() > 0.01 || (self.scroll_y - old_scroll).abs() > 0.01
    }

    fn set_scrollbar_hovered(&mut self, point: Point) -> bool {
        let horizontal_kind = self.scrollbar_hover_kind(TimelineScrollbarAxis::Horizontal, point);
        let vertical_kind = self.scrollbar_hover_kind(TimelineScrollbarAxis::Vertical, point);
        let horizontal = horizontal_kind.is_some();
        let vertical = vertical_kind.is_some();
        let changed = horizontal != self.horizontal_scrollbar_hovered
            || vertical != self.vertical_scrollbar_hovered
            || horizontal_kind != self.horizontal_scrollbar_hover_kind
            || vertical_kind != self.vertical_scrollbar_hover_kind;
        self.horizontal_scrollbar_hovered = horizontal;
        self.vertical_scrollbar_hovered = vertical;
        self.horizontal_scrollbar_hover_kind = horizontal_kind;
        self.vertical_scrollbar_hover_kind = vertical_kind;
        changed
    }

    fn scrollbar_hover_kind(
        &self,
        axis: TimelineScrollbarAxis,
        point: Point,
    ) -> Option<TimelineScrollbarDragKind> {
        let handle_rect = |kind| match axis {
            TimelineScrollbarAxis::Horizontal => self.horizontal_scrollbar_handle_rect(kind),
            TimelineScrollbarAxis::Vertical => self.vertical_scrollbar_handle_rect(kind),
        };
        for kind in [
            TimelineScrollbarDragKind::LeadingHandle,
            TimelineScrollbarDragKind::TrailingHandle,
        ] {
            if handle_rect(kind).is_some_and(|handle| handle.contains(point)) {
                return Some(kind);
            }
        }
        let thumb = match axis {
            TimelineScrollbarAxis::Horizontal => self.horizontal_scrollbar_thumb_rect(),
            TimelineScrollbarAxis::Vertical => self.vertical_scrollbar_thumb_rect(),
        };
        thumb
            .is_some_and(|thumb| thumb.contains(point))
            .then_some(TimelineScrollbarDragKind::Thumb)
    }

    fn frame_to_x(&self, frame: i64) -> f32 {
        timeline_model::frame_to_x(self.body_rect, self.pixels_per_frame, self.scroll_x, frame)
    }

    fn x_to_frame(&self, x: f32) -> i64 {
        timeline_model::x_to_frame(self.body_rect, self.pixels_per_frame, self.scroll_x, x)
    }

    fn snap_threshold_frames(&self) -> i64 {
        (TIMELINE_SNAP_THRESHOLD_PX / self.pixels_per_frame).ceil().max(1.0) as i64
    }

    fn snap_targets(
        &self,
        excluded_clip: Option<TimelineClipRef>,
        include_playhead: bool,
    ) -> Vec<TimelineSnapTarget> {
        let mut targets = Vec::new();
        targets.push(TimelineSnapTarget { frame: 0, kind: TimelineSnapKind::TimelineStart });
        if include_playhead {
            targets.push(TimelineSnapTarget {
                frame: self.playhead_frame.max(0),
                kind: TimelineSnapKind::Playhead,
            });
        }
        targets.push(TimelineSnapTarget {
            frame: self.in_point_frame.max(0),
            kind: TimelineSnapKind::InPoint,
        });
        if let Some(out) = self.out_point_frame {
            targets.push(TimelineSnapTarget {
                frame: out.saturating_add(1).max(0),
                kind: TimelineSnapKind::OutPoint,
            });
        }
        for (track_index, track) in self.tracks.iter().enumerate() {
            for (clip_index, clip) in track.clips.iter().enumerate() {
                let clip_ref = TimelineClipRef { track_index, clip_index };
                if excluded_clip == Some(clip_ref) {
                    continue;
                }
                targets.push(TimelineSnapTarget {
                    frame: clip.start_frame.max(0),
                    kind: TimelineSnapKind::ClipStart,
                });
                targets.push(TimelineSnapTarget {
                    frame: clip.end_frame().max(0),
                    kind: TimelineSnapKind::ClipEnd,
                });
            }
        }
        targets
    }

    fn snap_frame(
        &self,
        frame: i64,
        excluded_clip: Option<TimelineClipRef>,
        include_playhead: bool,
    ) -> Option<TimelineSnapResult> {
        if !self.snapping_enabled {
            return None;
        }
        let threshold = self.snap_threshold_frames();
        self.snap_targets(excluded_clip, include_playhead)
            .into_iter()
            .filter_map(|target| {
                let delta_frames = target.frame - frame;
                (delta_frames.abs() <= threshold).then_some(TimelineSnapResult {
                    frame: target.frame,
                    target,
                    delta_frames,
                })
            })
            .min_by_key(|result| (result.delta_frames.abs(), result.target.frame))
    }

    fn snap_clip_start(
        &self,
        clip_ref: TimelineClipRef,
        start_frame: i64,
        duration_frames: i64,
    ) -> Option<TimelineSnapResult> {
        if !self.snapping_enabled {
            return None;
        }
        let start_snap = self.snap_frame(start_frame, Some(clip_ref), true);
        let end_frame = start_frame.saturating_add(duration_frames.max(1));
        let end_snap =
            self.snap_frame(end_frame, Some(clip_ref), true).map(|snap| TimelineSnapResult {
                frame: snap.frame.saturating_sub(duration_frames.max(1)).max(0),
                target: snap.target,
                delta_frames: snap.delta_frames,
            });
        [start_snap, end_snap]
            .into_iter()
            .flatten()
            .min_by_key(|result| (result.delta_frames.abs(), result.target.frame))
    }

    fn set_active_snap(&mut self, snap: Option<TimelineSnapResult>, ctx: &mut EventContext) {
        if self.active_snap != snap {
            self.active_snap = snap;
            ctx.request_repaint();
        }
    }

    fn track_y(&self, track_index: usize) -> f32 {
        timeline_model::track_y(
            self.body_rect,
            self.track_height,
            self.scroll_y,
            track_index,
        )
    }

    fn track_index_at(&self, point: Point) -> Option<usize> {
        timeline_model::track_index_at(
            self.body_rect,
            self.track_height,
            self.scroll_y,
            self.tracks.len(),
            point,
        )
    }

    fn track_index_from_y(&self, y: f32) -> Option<usize> {
        timeline_model::track_index_from_y(
            self.body_rect,
            self.track_height,
            self.scroll_y,
            self.tracks.len(),
            y,
        )
    }

    fn track_header_at(&self, point: Point) -> Option<TimelineTrackRef> {
        timeline_model::track_header_at(
            self.header_rect,
            self.body_rect,
            self.track_height,
            self.scroll_y,
            self.tracks.len(),
            point,
        )
    }

    fn track_control_rect(
        &self,
        header: Rect,
        kind: TimelineTrackKind,
        control: TimelineTrackControl,
    ) -> Option<Rect> {
        timeline_model::track_control_rect(
            header,
            timeline_model::track_controls_for_kind(kind),
            control,
        )
    }

    fn track_control_at(&self, point: Point) -> Option<(TimelineTrackRef, TimelineTrackControl)> {
        let track_ref = timeline_model::track_header_at(
            self.header_rect,
            self.body_rect,
            self.track_height,
            self.scroll_y,
            self.tracks.len(),
            point,
        )?;
        let track = self.tracks.get(track_ref.track_index)?;
        let header = timeline_model::track_header_rect(
            self.header_rect,
            self.body_rect,
            self.track_height,
            self.scroll_y,
            track_ref.track_index,
        );
        timeline_model::track_controls_for_kind(track.kind)
            .iter()
            .copied()
            .find(|control| {
                self.track_control_rect(header, track.kind, *control)
                    .is_some_and(|rect| rect.contains(point))
            })
            .map(|control| (track_ref, control))
    }

    fn in_out_marker_at(&self, point: Point) -> Option<TimelineInOutPoint> {
        timeline_model::in_out_marker_at(
            self.ruler_rect,
            self.body_rect,
            self.pixels_per_frame,
            self.scroll_x,
            self.in_point_frame,
            self.out_point_frame,
            point,
        )
    }

    fn timeline_corner_rect(&self) -> Rect {
        Rect::new(
            self.bounds.x,
            self.bounds.y + TimelineMetrics::current().toolbar_height,
            self.header_width,
            self.ruler_height,
        )
    }

    fn tool_button_rect(&self, tool: TimelineTool) -> Rect {
        self.toolbar_button_rect(TimelineToolbarButton::Tool(tool))
            .unwrap_or(Rect::ZERO)
    }

    fn toolbar_left_buttons() -> [TimelineToolbarButton; 5] {
        [
            TimelineToolbarButton::Tool(TimelineTool::Select),
            TimelineToolbarButton::Tool(TimelineTool::Blade),
            TimelineToolbarButton::Snapping,
            TimelineToolbarButton::Edit(TimelineEditCommand::MarkInAtPlayhead),
            TimelineToolbarButton::Edit(TimelineEditCommand::MarkOutAtPlayhead),
        ]
    }

    fn toolbar_left_limit(&self) -> f32 {
        let right_padding = 8.0;
        self.toolbar_rect.x + self.toolbar_rect.width - right_padding
    }

    fn toolbar_button_rect(&self, button: TimelineToolbarButton) -> Option<Rect> {
        let metrics = TimelineMetrics::current();
        let size = metrics.tool_button_size;
        let y = self.toolbar_rect.y + (self.toolbar_rect.height - size) * 0.5;
        let limit = self.toolbar_left_limit();
        let mut x = self.toolbar_rect.x + 8.0;
        for (index, candidate) in Self::toolbar_left_buttons().into_iter().enumerate() {
            if index == 3 {
                x += metrics.toolbar_group_gap;
            }
            let rect = Rect::new(x, y, size, size);
            if candidate == button {
                return (rect.x + rect.width <= limit).then_some(rect);
            }
            x += size + metrics.tool_button_gap;
        }
        None
    }

    fn toolbar_button_at(&self, point: Point) -> Option<TimelineToolbarButton> {
        if !self.toolbar_rect.contains(point) {
            return None;
        }
        Self::toolbar_left_buttons().into_iter().find(|button| {
            self.toolbar_button_rect(*button).is_some_and(|rect| rect.contains(point))
        })
    }

    fn toolbar_icon(&self, slot: TimelineToolbarIconSlot) -> Option<&VectorIcon> {
        self.toolbar_icons
            .iter()
            .find_map(|(candidate, icon)| (*candidate == slot).then_some(icon))
    }

    fn track_control_icon(&self, slot: TimelineTrackControlIconSlot) -> Option<&VectorIcon> {
        self.track_control_icons
            .iter()
            .find_map(|(candidate, icon)| (*candidate == slot).then_some(icon))
    }

    fn toolbar_icon_slot(button: TimelineToolbarButton) -> Option<TimelineToolbarIconSlot> {
        match button {
            TimelineToolbarButton::Tool(TimelineTool::Select) => {
                Some(TimelineToolbarIconSlot::SelectTool)
            }
            TimelineToolbarButton::Tool(TimelineTool::Blade) => {
                Some(TimelineToolbarIconSlot::BladeTool)
            }
            TimelineToolbarButton::Snapping => Some(TimelineToolbarIconSlot::Snapping),
            TimelineToolbarButton::Edit(TimelineEditCommand::MarkInAtPlayhead) => {
                Some(TimelineToolbarIconSlot::MarkInAtPlayhead)
            }
            TimelineToolbarButton::Edit(TimelineEditCommand::MarkOutAtPlayhead) => {
                Some(TimelineToolbarIconSlot::MarkOutAtPlayhead)
            }
            _ => None,
        }
    }

    fn chrome_tooltip(&self) -> Option<(String, Rect)> {
        let button = self.hovered_toolbar_button?;
        let label = match button {
            TimelineToolbarButton::Tool(TimelineTool::Select) => "选择工具 (V)",
            TimelineToolbarButton::Tool(TimelineTool::Blade) => "剃刀工具 (B)",
            TimelineToolbarButton::Snapping => "吸附 (S)",
            TimelineToolbarButton::Edit(TimelineEditCommand::MarkInAtPlayhead) => "标记入点",
            TimelineToolbarButton::Edit(TimelineEditCommand::MarkOutAtPlayhead) => "标记出点",
            _ => return None,
        };
        let label = if let TimelineToolbarButton::Edit(command) = button {
            self.edit_command_shortcut(command).map_or_else(
                || label.to_owned(),
                |shortcut| format!("{label} ({shortcut})"),
            )
        } else {
            label.to_owned()
        };
        self.toolbar_button_rect(button).map(|rect| (label, rect))
    }

    fn clip_label_tooltip(&self, clip_ref: TimelineClipRef) -> Option<(String, Rect)> {
        let clip = self.clip(clip_ref)?;
        let rect = self.clip_rect(clip_ref.track_index, clip);
        let text_width = (rect.width - 20.0).max(0.0);
        let label_width = measure_single_line(&clip.label, self.theme_small_font_size()).0;
        (label_width > text_width).then(|| (clip.label.clone(), rect))
    }

    fn theme_small_font_size(&self) -> f32 {
        12.0
    }

    fn update_content_hover(&mut self, position: Point, ctx: &mut EventContext) -> bool {
        let hovered_transition = self.hit_transition(position);
        let hovered_clip = hovered_transition.is_none().then(|| self.hit_clip(position)).flatten();
        if hovered_transition
            .and_then(|transition_ref| self.hit_transition_edge(transition_ref, position))
            .is_some()
        {
            ctx.set_cursor(CursorRequest::EwResize);
        }
        if hovered_transition == self.hovered_transition && hovered_clip == self.hovered_clip {
            return false;
        }

        let had_tooltip = self.hovered_transition.is_some()
            || self
                .hovered_clip
                .and_then(|clip_ref| self.clip_label_tooltip(clip_ref))
                .is_some();
        self.hovered_transition = hovered_transition;
        self.hovered_clip = hovered_clip;
        if let Some(transition_ref) = self.hovered_transition {
            if let Some((text, rect)) = self.transition_tooltip(transition_ref) {
                ctx.tooltip.show(text, Point::new(rect.x + 8.0, rect.y + rect.height + 4.0));
            }
        } else if let Some((text, rect)) =
            self.hovered_clip.and_then(|clip_ref| self.clip_label_tooltip(clip_ref))
        {
            ctx.tooltip.show(text, Point::new(rect.x + 8.0, rect.y + rect.height + 4.0));
        } else if had_tooltip {
            ctx.tooltip.hide();
        }
        ctx.request_repaint();
        true
    }

    fn update_chrome_hover(&mut self, position: Point, ctx: &mut EventContext) -> bool {
        let hovered_toolbar_button = self.toolbar_button_at(position);
        let changed = hovered_toolbar_button != self.hovered_toolbar_button;
        if !changed {
            return false;
        }

        let had_tooltip = self.chrome_tooltip().is_some();
        self.hovered_toolbar_button = hovered_toolbar_button;

        if let Some((text, rect)) = self.chrome_tooltip() {
            ctx.tooltip.show(text, Point::new(rect.x, rect.y + rect.height));
        } else if had_tooltip {
            ctx.tooltip.hide();
        }
        ctx.request_repaint();
        true
    }

    fn clip_rect(&self, track_index: usize, clip: &TimelineClip) -> Rect {
        self.clip_rect_at(track_index, clip.start_frame, clip)
    }

    fn clip_rect_at(&self, track_index: usize, start_frame: i64, clip: &TimelineClip) -> Rect {
        timeline_model::clip_rect(
            self.body_rect,
            self.pixels_per_frame,
            self.scroll_x,
            self.track_height,
            self.scroll_y,
            track_index,
            start_frame,
            clip.duration_frames,
        )
    }

    fn clip_rect_for_preview(
        &self,
        track_index: usize,
        start_frame: i64,
        duration_frames: i64,
    ) -> Rect {
        timeline_model::clip_rect(
            self.body_rect,
            self.pixels_per_frame,
            self.scroll_x,
            self.track_height,
            self.scroll_y,
            track_index,
            start_frame,
            duration_frames,
        )
    }

    fn transition_rect(
        &self,
        transition_ref: TimelineTransitionRef,
        transition: &TimelineTransition,
    ) -> Rect {
        transition::rect(
            self.body_rect,
            self.pixels_per_frame,
            self.scroll_x,
            self.track_height,
            self.scroll_y,
            transition_ref.track_index,
            transition.start_frame,
            transition.duration_frames,
        )
    }

    fn transition_rect_for_preview(
        &self,
        transition_ref: TimelineTransitionRef,
        start_frame: i64,
        duration_frames: i64,
    ) -> Rect {
        transition::rect(
            self.body_rect,
            self.pixels_per_frame,
            self.scroll_x,
            self.track_height,
            self.scroll_y,
            transition_ref.track_index,
            start_frame,
            duration_frames,
        )
    }

    fn hit_transition(&self, point: Point) -> Option<TimelineTransitionRef> {
        if !self.body_rect.contains(point) {
            return None;
        }
        for (track_index, track) in self.tracks.iter().enumerate() {
            for (transition_index, transition) in track.transitions.iter().enumerate().rev() {
                let transition_ref = TimelineTransitionRef { track_index, transition_index };
                if self.transition_rect(transition_ref, transition).contains(point) {
                    return Some(transition_ref);
                }
            }
        }
        None
    }

    fn hit_transition_edge(
        &self,
        transition_ref: TimelineTransitionRef,
        point: Point,
    ) -> Option<TimelineTransitionEdge> {
        let transition = self.transition(transition_ref)?;
        transition::edge_at(self.transition_rect(transition_ref, transition), point)
    }

    fn transition(&self, transition_ref: TimelineTransitionRef) -> Option<&TimelineTransition> {
        self.tracks
            .get(transition_ref.track_index)
            .and_then(|track| track.transitions.get(transition_ref.transition_index))
    }

    fn transition_tooltip(&self, transition_ref: TimelineTransitionRef) -> Option<(String, Rect)> {
        let transition = self.transition(transition_ref)?;
        let text = transition.handle_issue.as_ref().map_or_else(
            || transition.label.clone(),
            |issue| format!("{}\n{}", transition.label, issue),
        );
        Some((text, self.transition_rect(transition_ref, transition)))
    }

    fn hit_cut(&self, point: Point) -> Option<TimelineCutRef> {
        let track_index = self.track_index_at(point)?;
        let track = self.tracks.get(track_index)?;
        if track.kind != TimelineTrackKind::Video || track.locked {
            return None;
        }
        track
            .clips
            .windows(2)
            .enumerate()
            .filter_map(|(left_clip_index, pair)| {
                let cut_frame = pair[0].end_frame();
                let exact_edit = cut_frame == pair[1].start_frame;
                let already_has_transition =
                    track.transitions.iter().any(|transition| transition.cut_frame == cut_frame);
                let distance = (point.x - self.frame_to_x(cut_frame)).abs();
                (exact_edit && !already_has_transition && distance <= 6.0).then_some((
                    distance,
                    TimelineCutRef {
                        track_index,
                        left_clip_index,
                        right_clip_index: left_clip_index + 1,
                    },
                ))
            })
            .min_by(|(left_distance, left), (right_distance, right)| {
                left_distance
                    .total_cmp(right_distance)
                    .then(left.left_clip_index.cmp(&right.left_clip_index))
            })
            .map(|(_, cut_ref)| cut_ref)
    }

    fn hit_clip(&self, point: Point) -> Option<TimelineClipRef> {
        if !self.body_rect.contains(point) {
            return None;
        }
        for (track_index, track) in self.tracks.iter().enumerate() {
            for (clip_index, clip) in track.clips.iter().enumerate().rev() {
                if self.clip_rect(track_index, clip).contains(point) {
                    return Some(TimelineClipRef { track_index, clip_index });
                }
            }
        }
        None
    }

    fn hit_clip_edge(&self, clip_ref: TimelineClipRef, point: Point) -> Option<TimelineTrimEdge> {
        let clip = self.clip(clip_ref)?;
        let rect = self.clip_rect(clip_ref.track_index, clip);
        timeline_model::hit_clip_edge(rect, point)
    }

    fn clip(&self, clip_ref: TimelineClipRef) -> Option<&TimelineClip> {
        self.tracks
            .get(clip_ref.track_index)
            .and_then(|track| track.clips.get(clip_ref.clip_index))
    }

    fn track(&self, track_ref: TimelineTrackRef) -> Option<&TimelineTrack> {
        self.tracks.get(track_ref.track_index)
    }

    fn track_locked(&self, track_index: usize) -> bool {
        self.tracks.get(track_index).is_some_and(|track| track.locked)
    }

    fn compatible_drag_track(&self, source_track_index: usize, target_track_index: usize) -> usize {
        let Some(source) = self.tracks.get(source_track_index) else {
            return source_track_index;
        };
        let Some(target) = self.tracks.get(target_track_index) else {
            return source_track_index;
        };
        if target.locked || source.kind != target.kind {
            source_track_index
        } else {
            target_track_index
        }
    }

    fn select_clip_from_input(
        &mut self,
        clip_ref: TimelineClipRef,
        ctx: &mut EventContext,
    ) -> EventResult {
        self.selected_track = None;
        self.selected_clip = Some(clip_ref);
        self.selected_transition = None;
        if let Some(clip) = self.clip(clip_ref) {
            if let Some(action) = clip.select_action.clone() {
                (ctx.dispatch)(action);
            }
            if let Some(factory) = &self.on_clip_select {
                (ctx.dispatch)(factory(clip_ref, clip));
            }
        }
        ctx.request_repaint();
        EventResult::Handled
    }

    fn select_transition_from_input(
        &mut self,
        transition_ref: TimelineTransitionRef,
        ctx: &mut EventContext,
    ) -> EventResult {
        self.selected_track = None;
        self.selected_clip = None;
        self.selected_transition = Some(transition_ref);
        if let Some(transition) = self.transition(transition_ref) {
            if let Some(factory) = &self.on_transition_select {
                (ctx.dispatch)(factory(transition_ref, transition));
            }
        }
        ctx.request_repaint();
        EventResult::Handled
    }

    fn select_track_from_input(
        &mut self,
        track_ref: TimelineTrackRef,
        ctx: &mut EventContext,
    ) -> EventResult {
        self.selected_track = Some(track_ref);
        self.selected_clip = None;
        self.selected_transition = None;
        if let Some(track) = self.track(track_ref) {
            if let Some(action) = track.select_action.clone() {
                (ctx.dispatch)(action);
            }
            if let Some(factory) = &self.on_track_select {
                (ctx.dispatch)(factory(track_ref, track));
            }
        }
        ctx.request_repaint();
        EventResult::Handled
    }

    fn activate_track_control_from_input(
        &mut self,
        track_ref: TimelineTrackRef,
        control: TimelineTrackControl,
        ctx: &mut EventContext,
    ) -> EventResult {
        if let Some(track) = self.track(track_ref) {
            if let Some(factory) = &self.on_track_control {
                (ctx.dispatch)(factory(control, track_ref, track));
            }
        }
        ctx.request_repaint();
        EventResult::Handled
    }

    fn start_track_drag(&mut self, track_ref: TimelineTrackRef) {
        if self.track(track_ref).is_none() {
            return;
        }
        self.track_drag = Some(TimelineTrackDrag {
            track_ref,
            current_track_index: track_ref.track_index,
            moved: false,
        });
    }

    fn drag_track_to(&mut self, position: Point, ctx: &mut EventContext) -> bool {
        let Some(mut drag) = self.track_drag else {
            return false;
        };
        if self.track(drag.track_ref).is_none() {
            self.track_drag = None;
            return false;
        }
        let proposed_track_index = self.track_index_from_y(position.y);
        let update = timeline_model::track_drag_position(
            drag.track_ref.track_index,
            drag.current_track_index,
            proposed_track_index,
            self.tracks.get(drag.track_ref.track_index).map(|track| track.kind),
            proposed_track_index.and_then(|index| self.tracks.get(index).map(|track| track.kind)),
        );
        if !update.changed {
            return true;
        }
        drag.current_track_index = update.track_index;
        drag.moved = true;
        self.track_drag = Some(drag);
        ctx.request_repaint();
        true
    }

    fn finish_track_drag(&mut self, ctx: &mut EventContext) -> bool {
        let Some(drag) = self.track_drag.take() else {
            return false;
        };
        if !drag.moved || drag.current_track_index == drag.track_ref.track_index {
            ctx.request_repaint();
            return true;
        }
        let Some(track) = self.track(drag.track_ref) else {
            return true;
        };
        let movement = TimelineTrackMove {
            track_ref: drag.track_ref,
            old_track_index: drag.track_ref.track_index,
            new_track_index: drag.current_track_index,
        };
        if let Some(factory) = &self.on_track_move {
            (ctx.dispatch)(factory(movement, track));
        }
        ctx.request_repaint();
        true
    }

    fn start_clip_drag(&mut self, clip_ref: TimelineClipRef, position: Point) {
        let Some(clip) = self.clip(clip_ref) else {
            return;
        };
        if self.track_locked(clip_ref.track_index) {
            return;
        }
        let pointer_frame = self.x_to_frame(position.x);
        self.clip_drag = Some(TimelineClipDrag {
            clip_ref,
            old_start_frame: clip.start_frame,
            pointer_offset_frames: pointer_frame - clip.start_frame,
            current_start_frame: clip.start_frame,
            current_track_index: clip_ref.track_index,
            moved: false,
        });
    }

    fn start_trim_drag(&mut self, clip_ref: TimelineClipRef, edge: TimelineTrimEdge) {
        let Some(clip) = self.clip(clip_ref) else {
            return;
        };
        if self.track_locked(clip_ref.track_index) {
            return;
        }
        self.trim_drag = Some(TimelineTrimDrag {
            clip_ref,
            edge,
            old_start_frame: clip.start_frame,
            old_duration_frames: clip.duration_frames.max(1),
            current_start_frame: clip.start_frame,
            current_duration_frames: clip.duration_frames.max(1),
            moved: false,
        });
    }

    fn start_transition_resize(
        &mut self,
        transition_ref: TimelineTransitionRef,
        edge: TimelineTransitionEdge,
    ) {
        let Some(transition) = self.transition(transition_ref) else {
            return;
        };
        if self.track_locked(transition_ref.track_index) {
            return;
        }
        self.transition_resize_drag = Some(TimelineTransitionResizeDrag {
            transition_ref,
            edge,
            old_start_frame: transition.start_frame,
            old_duration_frames: transition.duration_frames.max(1),
            current_start_frame: transition.start_frame,
            current_duration_frames: transition.duration_frames.max(1),
            moved: false,
        });
    }

    fn drag_clip_to(&mut self, position: Point, ctx: &mut EventContext) -> bool {
        let Some(mut drag) = self.clip_drag else {
            return false;
        };
        if self.clip(drag.clip_ref).is_none() {
            self.clip_drag = None;
            self.active_snap = None;
            return false;
        };
        let clip_duration =
            self.clip(drag.clip_ref).map(|clip| clip.duration_frames.max(1)).unwrap_or(1);
        let pointer_frame = self.x_to_frame(position.x);
        let proposed_start_frame = timeline_model::clip_drag_proposed_start_frame(
            pointer_frame,
            drag.pointer_offset_frames,
        );
        let snap = self.snap_clip_start(drag.clip_ref, proposed_start_frame, clip_duration);
        let target_track_index = self
            .track_index_at(position)
            .map(|index| self.compatible_drag_track(drag.clip_ref.track_index, index))
            .unwrap_or(drag.current_track_index);
        let drag_position = timeline_model::clip_drag_position(
            pointer_frame,
            drag.pointer_offset_frames,
            drag.current_start_frame,
            drag.current_track_index,
            target_track_index,
            snap.map(|snap| snap.frame),
        );
        self.set_active_snap(snap, ctx);
        if !drag_position.changed {
            return true;
        }
        drag.current_start_frame = drag_position.start_frame;
        drag.current_track_index = drag_position.track_index;
        drag.moved = true;
        self.clip_drag = Some(drag);
        ctx.request_repaint();
        true
    }

    fn drag_trim_to(&mut self, position: Point, ctx: &mut EventContext) -> bool {
        let Some(mut drag) = self.trim_drag else {
            return false;
        };
        let Some(_clip) = self.clip(drag.clip_ref) else {
            self.trim_drag = None;
            self.active_snap = None;
            return false;
        };
        let pointer_frame = self.x_to_frame(position.x);
        let snap = self.snap_frame(pointer_frame, Some(drag.clip_ref), true);
        let trim_position = timeline_model::trim_drag_position(
            drag.edge,
            pointer_frame,
            snap.map(|snap| snap.frame),
            drag.old_start_frame,
            drag.old_duration_frames,
            drag.current_start_frame,
            drag.current_duration_frames,
        );
        self.set_active_snap(snap, ctx);
        if !trim_position.changed {
            return true;
        }
        drag.current_start_frame = trim_position.start_frame;
        drag.current_duration_frames = trim_position.duration_frames;
        drag.moved = true;
        self.trim_drag = Some(drag);
        ctx.request_repaint();
        true
    }

    fn drag_transition_resize_to(&mut self, position: Point, ctx: &mut EventContext) -> bool {
        let Some(mut drag) = self.transition_resize_drag else {
            return false;
        };
        let Some(transition) = self.transition(drag.transition_ref) else {
            self.transition_resize_drag = None;
            return false;
        };
        let resized = transition::resize_position(
            drag.edge,
            self.x_to_frame(position.x),
            drag.old_start_frame,
            drag.old_duration_frames,
            transition.cut_frame,
            transition.minimum_start_frame,
            transition.maximum_end_frame,
        );
        if resized.start_frame == drag.current_start_frame
            && resized.duration_frames == drag.current_duration_frames
        {
            return true;
        }
        drag.current_start_frame = resized.start_frame;
        drag.current_duration_frames = resized.duration_frames;
        drag.moved = true;
        self.transition_resize_drag = Some(drag);
        ctx.request_repaint();
        true
    }

    fn finish_clip_drag(&mut self, ctx: &mut EventContext) -> bool {
        let Some(drag) = self.clip_drag.take() else {
            return false;
        };
        self.active_snap = None;
        if !drag.moved {
            return true;
        }
        let Some(clip) = self.clip(drag.clip_ref) else {
            return true;
        };
        let movement = TimelineClipMove {
            clip_ref: drag.clip_ref,
            old_start_frame: drag.old_start_frame,
            new_start_frame: drag.current_start_frame,
            new_track_index: drag.current_track_index,
        };
        if movement.old_start_frame != movement.new_start_frame
            || movement.clip_ref.track_index != movement.new_track_index
        {
            if let Some(factory) = &self.on_clip_move {
                (ctx.dispatch)(factory(movement, clip));
            }
        }
        ctx.request_repaint();
        true
    }

    fn finish_trim_drag(&mut self, ctx: &mut EventContext) -> bool {
        let Some(drag) = self.trim_drag.take() else {
            return false;
        };
        self.active_snap = None;
        if !drag.moved {
            return true;
        }
        let Some(clip) = self.clip(drag.clip_ref) else {
            return true;
        };
        let trim = TimelineClipTrim {
            clip_ref: drag.clip_ref,
            edge: drag.edge,
            old_start_frame: drag.old_start_frame,
            old_duration_frames: drag.old_duration_frames,
            new_start_frame: drag.current_start_frame,
            new_duration_frames: drag.current_duration_frames,
        };
        if trim.old_start_frame != trim.new_start_frame
            || trim.old_duration_frames != trim.new_duration_frames
        {
            if let Some(factory) = &self.on_clip_trim {
                (ctx.dispatch)(factory(trim, clip));
            }
        }
        ctx.request_repaint();
        true
    }

    fn finish_transition_resize(&mut self, ctx: &mut EventContext) -> bool {
        let Some(drag) = self.transition_resize_drag.take() else {
            return false;
        };
        if !drag.moved {
            return true;
        }
        let Some(transition) = self.transition(drag.transition_ref) else {
            return true;
        };
        let resize = TimelineTransitionResize {
            transition_ref: drag.transition_ref,
            edge: drag.edge,
            old_start_frame: drag.old_start_frame,
            old_duration_frames: drag.old_duration_frames,
            new_start_frame: drag.current_start_frame,
            new_duration_frames: drag.current_duration_frames,
        };
        if let Some(factory) = &self.on_transition_resize {
            (ctx.dispatch)(factory(resize, transition));
        }
        ctx.request_repaint();
        true
    }

    fn start_in_out_drag(&mut self, point: TimelineInOutPoint) {
        let start_frame = match point {
            TimelineInOutPoint::In => self.in_point_frame,
            TimelineInOutPoint::Out => self.out_point_frame.unwrap_or(self.in_point_frame),
        };
        self.in_out_drag = Some(TimelineInOutDrag {
            point,
            start_frame,
            current_frame: start_frame,
            moved: false,
        });
    }

    fn drag_in_out_to(&mut self, position: Point, ctx: &mut EventContext) -> bool {
        let Some(mut drag) = self.in_out_drag else {
            return false;
        };
        let update = timeline_model::in_out_drag_update(
            drag.point,
            self.x_to_frame(position.x),
            drag.current_frame,
            self.in_point_frame,
            self.out_point_frame,
        );
        if !update.changed {
            return true;
        }
        drag.current_frame = update.current_frame;
        drag.moved = true;
        self.in_point_frame = update.in_point_frame;
        self.out_point_frame = update.out_point_frame;
        self.in_out_drag = Some(drag);
        ctx.request_repaint();
        true
    }

    fn finish_in_out_drag(&mut self, ctx: &mut EventContext) -> bool {
        let Some(drag) = self.in_out_drag.take() else {
            return false;
        };
        if !drag.moved || drag.current_frame == drag.start_frame {
            ctx.request_repaint();
            return true;
        }
        if let Some(factory) = &self.on_in_out_point {
            (ctx.dispatch)(factory(drag.point, drag.current_frame));
        }
        ctx.request_repaint();
        true
    }

    fn dispatch_seek(&self, frame: i64, source: TimelineSeekSource, ctx: &mut EventContext) {
        if let Some(factory) = &self.on_seek {
            (ctx.dispatch)(factory(TimelineSeek { frame: frame.max(0), source }));
        }
    }

    fn seek_from_input(&mut self, frame: i64, source: TimelineSeekSource, ctx: &mut EventContext) {
        let frame = frame.max(0);
        if self.playhead_frame != frame {
            self.playhead_frame = frame;
            self.dispatch_seek(frame, source, ctx);
            ctx.request_repaint();
        }
    }

    fn seek_from_drag_input(&mut self, frame: i64, ctx: &mut EventContext) {
        let proposed = frame.max(0);
        let snap = self.snap_frame(proposed, None, false);
        self.set_active_snap(snap, ctx);
        self.seek_from_input(
            snap.map_or(proposed, |snap| snap.frame),
            TimelineSeekSource::PointerDrag,
            ctx,
        );
    }

    fn asset_drop_target_at(&self, position: Point) -> Option<(usize, i64)> {
        let target = timeline_model::asset_drop_target_at(
            self.body_rect,
            self.track_height,
            self.scroll_y,
            self.tracks.len(),
            self.pixels_per_frame,
            self.scroll_x,
            position,
        )?;
        Some((target.track_index, target.frame))
    }

    fn hover_asset_drop(&mut self, asset_id: AssetId, position: Point, ctx: &mut EventContext) {
        let next = self
            .asset_drop_target_at(position)
            .map(|(track_index, frame)| TimelineAssetDropHover { asset_id, track_index, frame });
        if self
            .asset_drop_hover
            .map(|hover| (hover.asset_id, hover.track_index, hover.frame))
            != next.map(|hover| (hover.asset_id, hover.track_index, hover.frame))
        {
            self.asset_drop_hover = next;
            ctx.request_repaint();
        }
    }

    fn finish_asset_drop(
        &mut self,
        asset_id: AssetId,
        position: Point,
        ctx: &mut EventContext,
    ) -> EventResult {
        self.asset_drop_hover = None;
        let Some((track_index, frame)) = self.asset_drop_target_at(position) else {
            ctx.request_repaint();
            return EventResult::Handled;
        };
        let Some(track) = self.tracks.get(track_index) else {
            ctx.request_repaint();
            return EventResult::Handled;
        };
        if let Some(factory) = &self.on_asset_drop {
            (ctx.dispatch)(factory(
                TimelineAssetDrop {
                    asset_id,
                    track_ref: TimelineTrackRef { track_index },
                    frame,
                },
                track,
            ));
        }
        ctx.request_repaint();
        EventResult::Handled
    }

    fn set_active_tool(&mut self, tool: TimelineTool, ctx: &mut EventContext) -> EventResult {
        if self.active_tool != tool {
            self.active_tool = tool;
            self.clip_drag = None;
            self.trim_drag = None;
            ctx.request_repaint();
        }
        EventResult::Handled
    }

    fn dispatch_edit_command(
        &mut self,
        command: TimelineEditCommand,
        ctx: &mut EventContext,
    ) -> bool {
        if !self.enabled || !self.edit_command_available_from_host(command) {
            return false;
        }
        let Some(factory) = &self.on_edit_command else {
            return false;
        };
        (ctx.dispatch)(factory(command));
        ctx.request_repaint();
        true
    }

    fn edit_command_action(&self, command: TimelineEditCommand) -> Action {
        self.on_edit_command.as_ref().map_or(Action::NoOp, |factory| factory(command))
    }

    fn edit_command_shortcut(&self, command: TimelineEditCommand) -> Option<String> {
        self.on_edit_command_shortcut.as_ref().and_then(|factory| factory(command))
    }

    fn edit_command_enabled(&self, command: TimelineEditCommand) -> bool {
        self.enabled
            && self.on_edit_command.is_some()
            && self.edit_command_has_local_target(command)
            && self.edit_command_available_from_host(command)
    }

    fn edit_command_available_from_host(&self, command: TimelineEditCommand) -> bool {
        self.on_edit_command_available.as_ref().is_none_or(|factory| factory(command))
    }

    fn edit_command_context(&self) -> timeline_model::TimelineEditCommandContext {
        timeline_model::TimelineEditCommandContext {
            selected_clip: self.selected_clip,
            selected_track: self.selected_track,
            selected_transition: self.selected_transition.is_some(),
            in_point_frame: self.in_point_frame,
            has_out_point: self.out_point_frame.is_some(),
            clip_intersects_playhead: self.clip_intersects_playhead(),
            selected_clip_is_nested: self
                .selected_clip
                .and_then(|clip_ref| self.clip(clip_ref))
                .is_some_and(|clip| clip.nested),
        }
    }

    fn track_add_action(&self, kind: TimelineTrackKind) -> Action {
        self.on_track_add.as_ref().map_or(Action::NoOp, |factory| factory(kind))
    }

    fn menu_item(label: &str, action: Action) -> MenuItem {
        let item = MenuItem::new(label, action.clone());
        if matches!(action, Action::NoOp) {
            item.disabled()
        } else {
            item
        }
    }

    fn edit_menu_item(&self, label: &str, command: TimelineEditCommand) -> MenuItem {
        let mut item = Self::menu_item(label, self.edit_command_action(command));
        if !self.edit_command_enabled(command) {
            item = item.disabled();
        }
        if let Some(shortcut) = self.edit_command_shortcut(command) {
            item.with_shortcut(shortcut)
        } else {
            item
        }
    }

    fn edit_command_has_local_target(&self, command: TimelineEditCommand) -> bool {
        timeline_model::edit_command_has_local_target(command, self.edit_command_context())
    }

    fn clip_intersects_playhead(&self) -> bool {
        let frame = self.playhead_frame;
        self.tracks.iter().any(|track| {
            track
                .clips
                .iter()
                .any(|clip| frame > clip.start_frame && frame < clip.end_frame())
        })
    }

    fn clip_context_menu_items(&self) -> Vec<MenuItem> {
        let mut items = vec![
            self.edit_menu_item("剪切剪辑", TimelineEditCommand::CutSelection),
            self.edit_menu_item("复制剪辑", TimelineEditCommand::CopySelection),
            self.edit_menu_item("粘贴", TimelineEditCommand::PasteAtPlayhead),
            self.edit_menu_item("创建剪辑副本", TimelineEditCommand::DuplicateSelection),
            MenuItem::separator(),
            self.edit_menu_item("删除剪辑", TimelineEditCommand::DeleteSelection),
            self.edit_menu_item("波纹删除剪辑", TimelineEditCommand::RippleDeleteSelection),
            self.edit_menu_item("在播放头处分割", TimelineEditCommand::SplitAtPlayhead),
            MenuItem::separator(),
            self.edit_menu_item(
                "修剪入点到播放头",
                TimelineEditCommand::TrimSelectionInToPlayhead,
            ),
            self.edit_menu_item(
                "修剪出点到播放头",
                TimelineEditCommand::TrimSelectionOutToPlayhead,
            ),
            self.edit_menu_item(
                "滚动剪辑点到播放头",
                TimelineEditCommand::RollSelectedCutToPlayhead,
            ),
            MenuItem::separator(),
            self.edit_menu_item("启用剪辑", TimelineEditCommand::EnableSelection),
            self.edit_menu_item("禁用剪辑", TimelineEditCommand::DisableSelection),
        ];
        if let Some(clip_ref) = self
            .selected_clip
            .filter(|clip_ref| self.clip(*clip_ref).is_some_and(|clip| clip.nested))
        {
            items.push(MenuItem::separator());
            items.push(self.edit_menu_item(
                "打开嵌套序列",
                TimelineEditCommand::OpenNestedSequence(clip_ref),
            ));
        }
        items.extend([
            MenuItem::separator(),
            self.edit_menu_item("标记入点", TimelineEditCommand::MarkInAtPlayhead),
            self.edit_menu_item("标记出点", TimelineEditCommand::MarkOutAtPlayhead),
            self.edit_menu_item("清除入点/出点", TimelineEditCommand::ClearInOutPoints),
        ]);
        items
    }

    fn transition_context_menu_items(&self) -> Vec<MenuItem> {
        vec![
            self.edit_menu_item("删除视频转场", TimelineEditCommand::DeleteSelection),
            MenuItem::separator(),
            self.edit_menu_item("标记入点", TimelineEditCommand::MarkInAtPlayhead),
            self.edit_menu_item("标记出点", TimelineEditCommand::MarkOutAtPlayhead),
            self.edit_menu_item("清除入点/出点", TimelineEditCommand::ClearInOutPoints),
        ]
    }

    fn cut_context_menu_items(&self, cut_ref: TimelineCutRef) -> Vec<MenuItem> {
        let action = self
            .on_cut_transition_create
            .as_ref()
            .map_or(Action::NoOp, |factory| factory(cut_ref));
        vec![
            Self::menu_item("添加交叉溶解", action),
            MenuItem::separator(),
            self.edit_menu_item("在播放头处分割", TimelineEditCommand::SplitAtPlayhead),
            self.edit_menu_item("标记入点", TimelineEditCommand::MarkInAtPlayhead),
            self.edit_menu_item("标记出点", TimelineEditCommand::MarkOutAtPlayhead),
        ]
    }

    fn timeline_context_menu_items(&self) -> Vec<MenuItem> {
        let mut items = Vec::new();

        // Only include "新建" submenu when track-add callback is registered.
        if self.on_track_add.is_some() {
            items.push(MenuItem::submenu(
                "新建",
                vec![
                    Self::menu_item("视频轨道", self.track_add_action(TimelineTrackKind::Video)),
                    Self::menu_item("音频轨道", self.track_add_action(TimelineTrackKind::Audio)),
                ],
            ));
            items.push(MenuItem::separator());
        }

        items.extend_from_slice(&[
            self.edit_menu_item("粘贴到播放头", TimelineEditCommand::PasteAtPlayhead),
            MenuItem::separator(),
            self.edit_menu_item("剪切所选", TimelineEditCommand::CutSelection),
            self.edit_menu_item("复制所选", TimelineEditCommand::CopySelection),
            self.edit_menu_item("创建所选副本", TimelineEditCommand::DuplicateSelection),
            MenuItem::separator(),
            self.edit_menu_item("在播放头处分割", TimelineEditCommand::SplitAtPlayhead),
            self.edit_menu_item(
                "修剪所选入点到播放头",
                TimelineEditCommand::TrimSelectionInToPlayhead,
            ),
            self.edit_menu_item(
                "修剪所选出点到播放头",
                TimelineEditCommand::TrimSelectionOutToPlayhead,
            ),
            self.edit_menu_item(
                "滚动所选剪辑点到播放头",
                TimelineEditCommand::RollSelectedCutToPlayhead,
            ),
            MenuItem::separator(),
            self.edit_menu_item("启用所选", TimelineEditCommand::EnableSelection),
            self.edit_menu_item("禁用所选", TimelineEditCommand::DisableSelection),
            MenuItem::separator(),
            self.edit_menu_item("标记入点", TimelineEditCommand::MarkInAtPlayhead),
            self.edit_menu_item("标记出点", TimelineEditCommand::MarkOutAtPlayhead),
            self.edit_menu_item("清除入点/出点", TimelineEditCommand::ClearInOutPoints),
        ]);
        items
    }

    fn open_context_menu(
        &mut self,
        position: Point,
        items: Vec<MenuItem>,
        ctx: &mut EventContext,
    ) -> EventResult {
        if items.iter().all(|item| !item.is_activatable()) {
            return EventResult::Ignored;
        }
        let mut menu = ContextMenu::new(position, items);
        menu.layout(self.bounds);
        self.context_menu = Some(menu);
        ctx.request_repaint();
        EventResult::Handled
    }

    fn route_context_menu_event(
        &mut self,
        event: &UiEvent,
        ctx: &mut EventContext,
    ) -> Option<EventResult> {
        let replace_with_new_menu = matches!(
            event,
            UiEvent::MouseDown {
                position,
                button: MouseButton::Right,
                ..
            } if self.bounds.contains(*position)
        );
        if replace_with_new_menu {
            self.context_menu = None;
            return None;
        }

        let menu = self.context_menu.as_mut()?;
        let result = menu.event(event, ctx);
        if !menu.is_visible() {
            self.context_menu = None;
            ctx.request_repaint();
        }
        (result == EventResult::Handled).then_some(result)
    }

    fn toolbar_button_enabled(&self, button: TimelineToolbarButton) -> bool {
        if !self.enabled {
            return false;
        }
        match button {
            TimelineToolbarButton::Tool(_) | TimelineToolbarButton::Snapping => true,
            TimelineToolbarButton::Edit(command) => self.edit_command_enabled(command),
        }
    }

    fn activate_toolbar_button(
        &mut self,
        button: TimelineToolbarButton,
        ctx: &mut EventContext,
    ) -> EventResult {
        if !self.toolbar_button_enabled(button) {
            return EventResult::Handled;
        }
        match button {
            TimelineToolbarButton::Tool(tool) => self.set_active_tool(tool, ctx),
            TimelineToolbarButton::Snapping => self.toggle_snapping(ctx),
            TimelineToolbarButton::Edit(command) => {
                let _ = self.dispatch_edit_command(command, ctx);
                EventResult::Handled
            }
        }
    }

    fn toggle_snapping(&mut self, ctx: &mut EventContext) -> EventResult {
        self.snapping_enabled = !self.snapping_enabled;
        self.active_snap = None;
        ctx.request_repaint();
        EventResult::Handled
    }

    fn split_at_pointer_frame(&mut self, point: Point, ctx: &mut EventContext) -> EventResult {
        self.seek_from_input(self.x_to_frame(point.x), TimelineSeekSource::Settled, ctx);
        if self.dispatch_edit_command(TimelineEditCommand::SplitAtPlayhead, ctx) {
            EventResult::Handled
        } else {
            EventResult::Ignored
        }
    }

    fn keyboard_seek(
        &mut self,
        key: KeyCode,
        modifiers: Modifiers,
        ctx: &mut EventContext,
    ) -> bool {
        let Some(target) = timeline_model::keyboard_seek_frame(
            key,
            modifiers,
            self.playhead_frame,
            self.max_content_frame(),
        ) else {
            return false;
        };
        self.seek_from_input(target, TimelineSeekSource::Settled, ctx);
        true
    }

    fn keyboard_edit_command(
        &mut self,
        key: KeyCode,
        modifiers: Modifiers,
        ctx: &mut EventContext,
    ) -> bool {
        let Some(command) = timeline_model::keyboard_edit_command(key, modifiers) else {
            return false;
        };
        self.dispatch_edit_command(command, ctx)
    }

    fn zoom_at(&mut self, anchor_x: f32, factor: f32) -> bool {
        let frame_at_anchor = self.x_to_frame(anchor_x) as f32;
        let old = self.pixels_per_frame;
        self.pixels_per_frame = (self.pixels_per_frame * factor)
            .clamp(TIMELINE_MIN_PIXELS_PER_FRAME, TIMELINE_MAX_PIXELS_PER_FRAME);
        if (old - self.pixels_per_frame).abs() > f32::EPSILON {
            self.scroll_x = frame_at_anchor * self.pixels_per_frame - (anchor_x - self.body_rect.x);
            self.clamp_scroll();
            return true;
        }
        false
    }

    fn tick_step_frames(&self) -> i64 {
        timeline_model::tick_step_frames(self.pixels_per_frame, self.timeline_display.frame_rate())
    }

    fn major_tick_step_frames(&self, minor_step: i64) -> i64 {
        timeline_model::major_tick_step_frames(
            self.pixels_per_frame,
            self.timeline_display.frame_rate(),
            minor_step,
        )
    }

    fn ruler_label_for_frame(&self, frame: i64, major_step: i64) -> String {
        timeline_model::ruler_label_for_frame(frame, major_step, self.timeline_display)
    }

    fn paint_ruler(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        ctx.encoder.draw_rect(self.ruler_rect, colors.timeline_ruler, 0.0);
        self.paint_in_out_ruler_region(ctx);
        let step = self.tick_step_frames();
        let major_step = self.major_tick_step_frames(step);
        let start_frame = (self.scroll_x / self.pixels_per_frame).floor().max(0.0) as i64;
        let first_tick = start_frame - start_frame % step;
        let end_frame = self.x_to_frame(self.body_rect.x + self.body_rect.width) + step;
        let mut frame = first_tick;
        while frame <= end_frame {
            let x = self.frame_to_x(frame);
            let major = frame % major_step == 0;
            let height = if major { 9.0 } else { 4.0 };
            let tick_color = if major {
                colors.timeline_tick_major
            } else {
                colors.timeline_tick_minor
            };
            ctx.encoder.draw_line(
                Point::new(x, self.ruler_rect.y + self.ruler_rect.height - height),
                Point::new(x, self.ruler_rect.y + self.ruler_rect.height - 1.0),
                1.0,
                tick_color,
            );
            if major {
                let label = self.ruler_label_for_frame(frame, major_step);
                ctx.encoder.draw_text(
                    &label,
                    ctx.theme.typography.metadata.font_size,
                    snap_point(Point::new(x + 4.0, self.ruler_rect.y + 6.0)),
                    colors.text_secondary,
                );
            }
            frame += step;
        }
        ctx.encoder.draw_line(
            Point::new(
                self.ruler_rect.x,
                self.ruler_rect.y + self.ruler_rect.height - 1.0,
            ),
            Point::new(
                self.ruler_rect.x + self.ruler_rect.width,
                self.ruler_rect.y + self.ruler_rect.height - 1.0,
            ),
            1.0,
            color_with_alpha(colors.border, 0.72),
        );
    }

    fn paint_timeline_toolbar(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        ctx.encoder.draw_rect(self.toolbar_rect, colors.panel_alt, 0.0);
        ctx.encoder.draw_line(
            Point::new(
                self.toolbar_rect.x,
                self.toolbar_rect.y + self.toolbar_rect.height - 1.0,
            ),
            Point::new(
                self.toolbar_rect.x + self.toolbar_rect.width,
                self.toolbar_rect.y + self.toolbar_rect.height - 1.0,
            ),
            1.0,
            color_with_alpha(colors.border, 0.72),
        );
        for button in Self::toolbar_left_buttons() {
            self.paint_toolbar_button(ctx, button);
        }
    }

    fn paint_timeline_corner(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        let corner = self.timeline_corner_rect();
        ctx.encoder.draw_rect(corner, colors.timeline_ruler, 0.0);
        ctx.encoder.draw_line(
            Point::new(corner.x, corner.y + corner.height - 1.0),
            Point::new(corner.x + corner.width, corner.y + corner.height - 1.0),
            1.0,
            color_with_alpha(colors.border, 0.72),
        );
        ctx.encoder.draw_line(
            Point::new(corner.x + corner.width - 1.0, corner.y),
            Point::new(corner.x + corner.width - 1.0, corner.y + corner.height),
            1.0,
            color_with_alpha(colors.border, 0.72),
        );
    }

    fn paint_toolbar_button(&self, ctx: &mut PaintContext, button: TimelineToolbarButton) {
        match button {
            TimelineToolbarButton::Tool(tool) => self.paint_tool_button(ctx, tool),
            TimelineToolbarButton::Snapping => self.paint_snapping_button(ctx),
            TimelineToolbarButton::Edit(_) => {
                let Some(rect) = self.toolbar_button_rect(button) else {
                    return;
                };
                let colors = &ctx.theme.colors;
                let enabled = self.toolbar_button_enabled(button);
                let hovered = self.hovered_toolbar_button == Some(button);
                let mut bg = if hovered && enabled {
                    colors.surface_2
                } else {
                    Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }
                };
                bg.a = if enabled {
                    if hovered {
                        0.72
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };
                ctx.encoder.draw_rect(rect, bg, ctx.theme.spacing.radius_sm);

                let mut icon = colors.muted_foreground;
                if !enabled {
                    icon = color_with_alpha(icon, 0.44);
                }
                if let Some(slot) = Self::toolbar_icon_slot(button) {
                    if let Some(vector_icon) = self.toolbar_icon(slot) {
                        self.paint_toolbar_vector_icon(ctx, rect, vector_icon, icon);
                        return;
                    }
                }
                self.paint_toolbar_fallback_icon(ctx, rect, button, icon);
            }
        }
    }

    fn paint_toolbar_vector_icon(
        &self,
        ctx: &mut PaintContext,
        rect: Rect,
        vector_icon: &VectorIcon,
        color: Color,
    ) {
        let icon_size = 14.0_f32.min(rect.width - 6.0).min(rect.height - 6.0).max(1.0);
        let icon_rect = Rect::new(
            rect.x + (rect.width - icon_size) * 0.5,
            rect.y + (rect.height - icon_size) * 0.5,
            icon_size,
            icon_size,
        );
        vector_icon.paint(ctx, icon_rect, color);
    }

    fn paint_toolbar_fallback_icon(
        &self,
        ctx: &mut PaintContext,
        rect: Rect,
        button: TimelineToolbarButton,
        color: Color,
    ) {
        match button {
            TimelineToolbarButton::Edit(TimelineEditCommand::MarkInAtPlayhead) => {
                self.paint_toolbar_marker(ctx, rect, true, color);
            }
            TimelineToolbarButton::Edit(TimelineEditCommand::MarkOutAtPlayhead) => {
                self.paint_toolbar_marker(ctx, rect, false, color);
            }
            TimelineToolbarButton::Snapping => {
                self.paint_toolbar_magnet(ctx, rect, color);
            }
            _ => {}
        }
    }

    fn paint_snapping_button(&self, ctx: &mut PaintContext) {
        let Some(rect) = self.toolbar_button_rect(TimelineToolbarButton::Snapping) else {
            return;
        };
        let colors = &ctx.theme.colors;
        let enabled = self.toolbar_button_enabled(TimelineToolbarButton::Snapping);
        let active = enabled && self.snapping_enabled;
        let hovered =
            enabled && self.hovered_toolbar_button == Some(TimelineToolbarButton::Snapping);
        let mut bg = if active {
            color_with_alpha(colors.foreground, 0.085)
        } else if hovered {
            colors.surface_2
        } else {
            Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }
        };
        bg.a = if enabled {
            if active {
                0.18
            } else if hovered {
                0.72
            } else {
                0.0
            }
        } else {
            0.0
        };
        ctx.encoder.draw_rect(rect, bg, ctx.theme.spacing.radius_sm);

        let icon = if !enabled {
            color_with_alpha(colors.muted_foreground, 0.44)
        } else if active {
            colors.foreground
        } else {
            colors.muted_foreground
        };
        if let Some(vector_icon) = self.toolbar_icon(TimelineToolbarIconSlot::Snapping) {
            self.paint_toolbar_vector_icon(ctx, rect, vector_icon, icon);
        } else {
            self.paint_toolbar_magnet(ctx, rect, icon);
        }
    }

    fn paint_toolbar_magnet(&self, ctx: &mut PaintContext, rect: Rect, color: Color) {
        let left = rect.x + 5.0;
        let right = rect.x + rect.width - 5.0;
        let top = rect.y + 5.5;
        let bottom = rect.y + rect.height - 5.0;
        let mid_y = rect.y + rect.height * 0.54;
        ctx.encoder
            .draw_line(Point::new(left, top), Point::new(left, mid_y), 1.6, color);
        ctx.encoder
            .draw_line(Point::new(right, top), Point::new(right, mid_y), 1.6, color);
        ctx.encoder.draw_line(
            Point::new(left, mid_y),
            Point::new(left + 2.5, bottom),
            1.6,
            color,
        );
        ctx.encoder.draw_line(
            Point::new(right, mid_y),
            Point::new(right - 2.5, bottom),
            1.6,
            color,
        );
        ctx.encoder.draw_line(
            Point::new(left + 2.5, bottom),
            Point::new(right - 2.5, bottom),
            1.6,
            color,
        );
        ctx.encoder.draw_rect(Rect::new(left - 1.5, top - 0.5, 3.0, 3.0), color, 0.8);
        ctx.encoder.draw_rect(Rect::new(right - 1.5, top - 0.5, 3.0, 3.0), color, 0.8);
    }

    fn paint_toolbar_marker(
        &self,
        ctx: &mut PaintContext,
        rect: Rect,
        mark_in: bool,
        color: Color,
    ) {
        let stem_x = if mark_in {
            rect.x + 6.0
        } else {
            rect.x + rect.width - 6.0
        };
        ctx.encoder.draw_line(
            Point::new(stem_x, rect.y + 4.5),
            Point::new(stem_x, rect.y + rect.height - 4.5),
            1.5,
            color,
        );
        let direction = if mark_in { 1.0 } else { -1.0 };
        ctx.encoder.draw_triangles(
            &[
                Point::new(stem_x, rect.center().y),
                Point::new(stem_x + direction * 7.0, rect.y + 6.0),
                Point::new(stem_x + direction * 7.0, rect.y + rect.height - 6.0),
            ],
            color,
        );
    }

    fn paint_tool_button(&self, ctx: &mut PaintContext, tool: TimelineTool) {
        let colors = &ctx.theme.colors;
        let rect = self.tool_button_rect(tool);
        if rect == Rect::ZERO {
            return;
        }
        let enabled = self.toolbar_button_enabled(TimelineToolbarButton::Tool(tool));
        let active = enabled && self.active_tool == tool;
        let hovered =
            enabled && self.hovered_toolbar_button == Some(TimelineToolbarButton::Tool(tool));
        let mut bg = if active {
            color_with_alpha(colors.foreground, 0.085)
        } else if hovered {
            colors.surface_2
        } else {
            Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }
        };
        bg.a = if enabled {
            if active {
                0.18
            } else if hovered {
                0.72
            } else {
                0.0
            }
        } else {
            0.0
        };
        ctx.encoder.draw_rect(rect, bg, ctx.theme.spacing.radius_sm);

        let icon = if !enabled {
            color_with_alpha(colors.muted_foreground, 0.44)
        } else if active {
            colors.foreground
        } else {
            colors.muted_foreground
        };
        if let Some(slot) = Self::toolbar_icon_slot(TimelineToolbarButton::Tool(tool)) {
            if let Some(vector_icon) = self.toolbar_icon(slot) {
                self.paint_toolbar_vector_icon(ctx, rect, vector_icon, icon);
                return;
            }
        }
        match tool {
            TimelineTool::Select => {
                let p0 = Point::new(rect.x + 5.0, rect.y + 4.0);
                let p1 = Point::new(rect.x + 5.0, rect.y + 14.0);
                let p2 = Point::new(rect.x + 12.5, rect.y + 10.5);
                ctx.encoder.draw_triangles(&[p0, p1, p2], icon);
                ctx.encoder.draw_line(
                    Point::new(rect.x + 9.0, rect.y + 10.5),
                    Point::new(rect.x + 12.0, rect.y + 15.0),
                    1.2,
                    icon,
                );
            }
            TimelineTool::Blade => {
                ctx.encoder.draw_line(
                    Point::new(rect.x + 5.0, rect.y + 13.0),
                    Point::new(rect.x + 13.0, rect.y + 5.0),
                    1.8,
                    icon,
                );
                ctx.encoder.draw_line(
                    Point::new(rect.x + 6.0, rect.y + 5.5),
                    Point::new(rect.x + 13.5, rect.y + 13.0),
                    1.2,
                    icon,
                );
            }
        }
    }

    fn paint_tracks(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        for (track_index, track) in self.tracks.iter().enumerate() {
            let y = self.track_y(track_index);
            if y > self.body_rect.y + self.body_rect.height
                || y + self.track_height < self.body_rect.y
            {
                continue;
            }

            let header = Rect::new(
                self.header_rect.x,
                y,
                self.header_rect.width,
                self.track_height,
            );
            let track_ref = TimelineTrackRef { track_index };
            let track_selected = track.selected || self.selected_track == Some(track_ref);
            let drop_target =
                self.asset_drop_hover.is_some_and(|hover| hover.track_index == track_index);
            ctx.encoder.draw_rect(
                header,
                if track_selected {
                    color_with_alpha(colors.foreground, 0.075)
                } else if drop_target {
                    color_with_alpha(colors.primary, 0.08)
                } else if track.locked {
                    color_with_alpha(colors.surface, 0.72)
                } else {
                    colors.panel_alt
                },
                0.0,
            );
            if !track.visible {
                ctx.encoder.draw_rect(header, color_with_alpha(colors.background, 0.20), 0.0);
            }
            ctx.encoder.draw_line(
                Point::new(header.x + header.width - 1.0, header.y),
                Point::new(header.x + header.width - 1.0, header.y + header.height),
                1.0,
                color_with_alpha(colors.border, 0.72),
            );
            self.paint_track_kind_badge(ctx, header, track, track_selected);
            for control in timeline_model::track_controls_for_kind(track.kind) {
                self.paint_track_control(ctx, header, track_ref, track, *control);
            }

            let row = Rect::new(self.body_rect.x, y, self.body_rect.width, self.track_height);
            let row_fill = if track_index % 2 == 0 {
                colors.timeline_track_even
            } else {
                colors.timeline_track_odd
            };
            ctx.encoder.draw_rect(row, row_fill, 0.0);
            if track_selected {
                ctx.encoder.draw_rect(row, color_with_alpha(colors.foreground, 0.035), 0.0);
            }
            if track.locked {
                ctx.encoder.draw_rect(row, color_with_alpha(colors.foreground, 0.035), 0.0);
                ctx.encoder.draw_line(
                    Point::new(row.x, row.y),
                    Point::new(row.x, row.y + row.height),
                    2.0,
                    color_with_alpha(colors.text_tertiary, 0.32),
                );
            }
            if track.muted {
                ctx.encoder.draw_rect(row, color_with_alpha(colors.media_audio, 0.055), 0.0);
            }
            if !track.visible {
                ctx.encoder.draw_rect(row, color_with_alpha(colors.background, 0.30), 0.0);
            }
            self.paint_in_out_row_region(ctx, row);
            if drop_target {
                ctx.encoder.draw_rect(row, color_with_alpha(colors.primary, 0.11), 0.0);
                let ring = color_with_alpha(colors.ring, 0.48);
                ctx.encoder.draw_line(
                    Point::new(row.x, row.y + 1.0),
                    Point::new(row.x + row.width, row.y + 1.0),
                    1.0,
                    ring,
                );
                ctx.encoder.draw_line(
                    Point::new(row.x, row.y + row.height - 1.0),
                    Point::new(row.x + row.width, row.y + row.height - 1.0),
                    1.0,
                    ring,
                );
            }
            if self.track_drag.is_some_and(|drag| drag.current_track_index == track_index) {
                let mut target = colors.ring;
                target.a = 0.10;
                ctx.encoder.draw_rect(row, target, 0.0);
            }
            ctx.encoder.draw_line(
                Point::new(self.bounds.x, y + self.track_height),
                Point::new(self.bounds.x + self.bounds.width, y + self.track_height),
                1.0,
                colors.border,
            );
        }

        for (track_index, track) in self.tracks.iter().enumerate() {
            let y = self.track_y(track_index);
            if y > self.body_rect.y + self.body_rect.height
                || y + self.track_height < self.body_rect.y
            {
                continue;
            }
            for (clip_index, clip) in track.clips.iter().enumerate() {
                let clip_ref = TimelineClipRef { track_index, clip_index };
                if self.clip_drag.is_some_and(|drag| drag.clip_ref == clip_ref) {
                    continue;
                }
                if self.trim_drag.is_some_and(|drag| drag.clip_ref == clip_ref) {
                    continue;
                }
                let rect = self.clip_rect(track_index, clip);
                if rect.x > self.body_rect.x + self.body_rect.width
                    || rect.x + rect.width < self.body_rect.x
                {
                    continue;
                }
                self.paint_clip(ctx, clip_ref, clip, rect, false);
            }
        }

        for (track_index, track) in self.tracks.iter().enumerate() {
            for (transition_index, transition) in track.transitions.iter().enumerate() {
                let transition_ref = TimelineTransitionRef { track_index, transition_index };
                if self
                    .transition_resize_drag
                    .is_some_and(|drag| drag.transition_ref == transition_ref)
                {
                    continue;
                }
                let rect = self.transition_rect(transition_ref, transition);
                if rect.x <= self.body_rect.x + self.body_rect.width
                    && rect.x + rect.width >= self.body_rect.x
                {
                    self.paint_transition(ctx, transition_ref, transition, rect, false);
                }
            }
        }

        if let Some(drag) = self.clip_drag {
            if let Some(clip) = self.clip(drag.clip_ref) {
                let rect =
                    self.clip_rect_at(drag.current_track_index, drag.current_start_frame, clip);
                if rect.x <= self.body_rect.x + self.body_rect.width
                    && rect.x + rect.width >= self.body_rect.x
                    && rect.y <= self.body_rect.y + self.body_rect.height
                    && rect.y + rect.height >= self.body_rect.y
                {
                    self.paint_clip(ctx, drag.clip_ref, clip, rect, true);
                }
            }
        }

        if let Some(drag) = self.trim_drag {
            if let Some(clip) = self.clip(drag.clip_ref) {
                let rect = self.clip_rect_for_preview(
                    drag.clip_ref.track_index,
                    drag.current_start_frame,
                    drag.current_duration_frames,
                );
                if rect.x <= self.body_rect.x + self.body_rect.width
                    && rect.x + rect.width >= self.body_rect.x
                    && rect.y <= self.body_rect.y + self.body_rect.height
                    && rect.y + rect.height >= self.body_rect.y
                {
                    self.paint_clip(ctx, drag.clip_ref, clip, rect, true);
                }
            }
        }

        if let Some(drag) = self.transition_resize_drag {
            if let Some(transition) = self.transition(drag.transition_ref) {
                let rect = self.transition_rect_for_preview(
                    drag.transition_ref,
                    drag.current_start_frame,
                    drag.current_duration_frames,
                );
                if rect.x <= self.body_rect.x + self.body_rect.width
                    && rect.x + rect.width >= self.body_rect.x
                {
                    self.paint_transition(ctx, drag.transition_ref, transition, rect, true);
                }
            }
        }

        self.paint_track_drag_indicator(ctx);
        self.paint_asset_drop_indicator(ctx);
    }

    fn paint_empty_state(&self, ctx: &mut PaintContext) {
        if !self.tracks.is_empty() {
            return;
        }
        let Some(message) = self
            .empty_message
            .as_deref()
            .map(str::trim)
            .filter(|message| !message.is_empty())
        else {
            return;
        };
        if self.body_rect.width <= 0.0 || self.body_rect.height <= 0.0 {
            return;
        }

        let max_width = (self.body_rect.width - 32.0).max(0.0);
        if max_width <= 0.0 {
            return;
        }
        let (title, description) =
            message.split_once('\n').map_or((message, ""), |(title, description)| {
                (title.trim(), description.trim())
            });
        let x = self.body_rect.x + 16.0;
        let y = self.body_rect.y + (self.body_rect.height * 0.28).max(20.0);
        ctx.encoder.draw_text_box(
            title,
            ctx.theme.typography.small.font_size,
            snap_point(Point::new(x, y)),
            max_width,
            ctx.theme.colors.text_secondary,
        );
        if !description.is_empty() {
            ctx.encoder.draw_text_box(
                description,
                ctx.theme.typography.metadata.font_size,
                snap_point(Point::new(x, y + 20.0)),
                max_width.min(260.0),
                ctx.theme.colors.text_tertiary,
            );
        }
    }

    fn paint_asset_drop_indicator(&self, ctx: &mut PaintContext) {
        let Some(hover) = self.asset_drop_hover else {
            return;
        };
        if self.tracks.get(hover.track_index).is_none() {
            return;
        }
        let colors = &ctx.theme.colors;
        let mut color = colors.accent;
        color.a = 0.85;
        let x = self.frame_to_x(hover.frame).round();
        let row_top = self.track_y(hover.track_index).round();
        let row_bottom =
            (row_top + self.track_height).min(self.body_rect.y + self.body_rect.height);
        if x < self.body_rect.x || x > self.body_rect.x + self.body_rect.width {
            return;
        }
        ctx.encoder.draw_line(
            Point::new(x, row_top),
            Point::new(x, row_bottom),
            2.0,
            color,
        );
    }

    fn paint_track_drag_indicator(&self, ctx: &mut PaintContext) {
        let Some(drag) = self.track_drag else {
            return;
        };
        let Some(target) = self.tracks.get(drag.current_track_index) else {
            return;
        };
        if !self
            .tracks
            .get(drag.track_ref.track_index)
            .is_some_and(|source| source.kind == target.kind)
        {
            return;
        }
        let colors = &ctx.theme.colors;
        let mut color = colors.ring;
        color.a = 0.80;
        let y = self.track_y(drag.current_track_index).round();
        if y < self.body_rect.y || y > self.body_rect.y + self.body_rect.height {
            return;
        }
        ctx.encoder.draw_line(
            Point::new(self.bounds.x, y),
            Point::new(self.bounds.x + self.bounds.width, y),
            2.0,
            color,
        );
    }

    fn in_out_visible_range(&self) -> Option<(f32, f32)> {
        timeline_model::in_out_visible_range(
            self.body_rect,
            self.pixels_per_frame,
            self.scroll_x,
            self.in_point_frame,
            self.out_point_frame,
        )
    }

    fn paint_in_out_ruler_region(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        if let Some((x0, x1)) = self.in_out_visible_range() {
            let bar_height = 3.0_f32.min(self.ruler_rect.height.max(0.0));
            ctx.encoder.draw_rect(
                Rect::new(x0, self.ruler_rect.y, x1 - x0, bar_height),
                colors.timeline_range_edge,
                1.5,
            );
        }
        self.paint_in_out_marker_lines(ctx, self.ruler_rect);
    }

    fn paint_in_out_row_region(&self, ctx: &mut PaintContext, row: Rect) {
        let colors = &ctx.theme.colors;
        if let Some((x0, x1)) = self.in_out_visible_range() {
            ctx.encoder.draw_rect(
                Rect::new(x0, row.y, x1 - x0, row.height),
                colors.timeline_range_fill,
                0.0,
            );
        }
        self.paint_in_out_marker_lines(ctx, row);
    }

    fn paint_in_out_marker_lines(&self, ctx: &mut PaintContext, rect: Rect) {
        let colors = &ctx.theme.colors;
        let color = colors.timeline_range_edge;
        if let Some(in_x) = timeline_model::in_out_marker_x(
            self.body_rect,
            self.pixels_per_frame,
            self.scroll_x,
            TimelineInOutPoint::In,
            self.in_point_frame,
            self.out_point_frame,
        )
        .filter(|in_x| *in_x >= rect.x && *in_x <= rect.x + rect.width)
        {
            ctx.encoder.draw_line(
                Point::new(in_x, rect.y),
                Point::new(in_x, rect.y + rect.height),
                1.0,
                color,
            );
        }
        if let Some(out_x) = timeline_model::in_out_marker_x(
            self.body_rect,
            self.pixels_per_frame,
            self.scroll_x,
            TimelineInOutPoint::Out,
            self.in_point_frame,
            self.out_point_frame,
        )
        .filter(|out_x| *out_x >= rect.x && *out_x <= rect.x + rect.width)
        {
            ctx.encoder.draw_line(
                Point::new(out_x, rect.y),
                Point::new(out_x, rect.y + rect.height),
                1.0,
                color,
            );
        }
    }

    fn paint_track_control(
        &self,
        ctx: &mut PaintContext,
        header: Rect,
        track_ref: TimelineTrackRef,
        track: &TimelineTrack,
        control: TimelineTrackControl,
    ) {
        let colors = &ctx.theme.colors;
        let Some(rect) = self.track_control_rect(header, track.kind, control) else {
            return;
        };
        let hovered = self.hovered_track_control == Some((track_ref, control));
        let toggled = match control {
            TimelineTrackControl::Visibility => !track.visible,
            TimelineTrackControl::Mute => track.muted,
            TimelineTrackControl::Lock => track.locked,
        };
        let mut bg = if hovered {
            colors.surface_2
        } else if toggled {
            colors.surface
        } else {
            Color::TRANSPARENT
        };
        bg.a = if hovered {
            0.82
        } else if toggled {
            0.92
        } else {
            0.0
        };
        ctx.encoder.draw_rect(rect, bg, ctx.theme.spacing.radius_sm);

        let mut icon = if toggled {
            colors.foreground
        } else {
            colors.muted_foreground
        };
        if !track.visible && control != TimelineTrackControl::Visibility {
            icon.a *= 0.52;
        }
        let slot = match control {
            TimelineTrackControl::Visibility if track.visible => {
                TimelineTrackControlIconSlot::VisibilityOn
            }
            TimelineTrackControl::Visibility => TimelineTrackControlIconSlot::VisibilityOff,
            TimelineTrackControl::Mute if track.muted => TimelineTrackControlIconSlot::MuteOn,
            TimelineTrackControl::Mute => TimelineTrackControlIconSlot::MuteOff,
            TimelineTrackControl::Lock if track.locked => TimelineTrackControlIconSlot::LockOn,
            TimelineTrackControl::Lock => TimelineTrackControlIconSlot::LockOff,
        };
        if let Some(vector_icon) = self.track_control_icon(slot) {
            let icon_size = 13.0_f32.min(rect.width - 6.0).min(rect.height - 6.0).max(1.0);
            let icon_rect = Rect::new(
                rect.x + (rect.width - icon_size) * 0.5,
                rect.y + (rect.height - icon_size) * 0.5,
                icon_size,
                icon_size,
            );
            vector_icon.paint(ctx, icon_rect, icon);
            return;
        }
        match control {
            TimelineTrackControl::Visibility => {
                self.paint_visibility_icon(ctx, rect, icon, !track.visible);
            }
            TimelineTrackControl::Mute => {
                self.paint_mute_icon(ctx, rect, icon, track.muted);
            }
            TimelineTrackControl::Lock => {
                self.paint_lock_icon(ctx, rect, icon, track.locked);
            }
        }
    }

    fn paint_track_kind_badge(
        &self,
        ctx: &mut PaintContext,
        header: Rect,
        track: &TimelineTrack,
        selected: bool,
    ) -> Rect {
        let colors = &ctx.theme.colors;
        let font_size = ctx.theme.typography.metadata.font_size;
        let text_width = measure_single_line(&track.label, font_size).0;
        let horizontal_padding = 6.0;
        let width = (text_width + horizontal_padding * 2.0).max(22.0);
        let height = 18.0;
        let rect = Rect::new(
            header.x + 8.0,
            header.y + (header.height - height) * 0.5,
            width,
            height,
        );
        let base = match track.kind {
            TimelineTrackKind::Video => colors.media_video,
            TimelineTrackKind::Audio => colors.media_audio,
        };
        let mut fill = base;
        fill.a = match track.kind {
            TimelineTrackKind::Video => {
                if selected {
                    0.36
                } else {
                    0.25
                }
            }
            TimelineTrackKind::Audio => {
                if selected {
                    0.32
                } else {
                    0.22
                }
            }
        };
        ctx.encoder.draw_rect(rect, fill, ctx.theme.spacing.radius_sm);
        let text_y = centered_text_origin_y(rect, ctx.theme.typography.metadata.line_height);
        ctx.encoder.draw_text(
            &track.label,
            font_size,
            snap_point(Point::new(rect.x + horizontal_padding, text_y)),
            colors.foreground,
        );
        rect
    }

    fn paint_visibility_icon(
        &self,
        ctx: &mut PaintContext,
        rect: Rect,
        color: Color,
        hidden: bool,
    ) {
        let c = rect.center();
        ctx.encoder.draw_line(
            Point::new(c.x - 6.0, c.y),
            Point::new(c.x - 2.0, c.y - 4.0),
            1.3,
            color,
        );
        ctx.encoder.draw_line(
            Point::new(c.x - 2.0, c.y - 4.0),
            Point::new(c.x + 2.0, c.y - 4.0),
            1.3,
            color,
        );
        ctx.encoder.draw_line(
            Point::new(c.x + 2.0, c.y - 4.0),
            Point::new(c.x + 6.0, c.y),
            1.3,
            color,
        );
        ctx.encoder.draw_line(
            Point::new(c.x + 6.0, c.y),
            Point::new(c.x + 2.0, c.y + 4.0),
            1.3,
            color,
        );
        ctx.encoder.draw_line(
            Point::new(c.x + 2.0, c.y + 4.0),
            Point::new(c.x - 2.0, c.y + 4.0),
            1.3,
            color,
        );
        ctx.encoder.draw_line(
            Point::new(c.x - 2.0, c.y + 4.0),
            Point::new(c.x - 6.0, c.y),
            1.3,
            color,
        );
        ctx.encoder.draw_rect(Rect::new(c.x - 1.6, c.y - 1.6, 3.2, 3.2), color, 2.0);
        if hidden {
            ctx.encoder.draw_line(
                Point::new(rect.x + 4.0, rect.y + rect.height - 4.0),
                Point::new(rect.x + rect.width - 4.0, rect.y + 4.0),
                1.5,
                color,
            );
        }
    }

    fn paint_mute_icon(&self, ctx: &mut PaintContext, rect: Rect, color: Color, muted: bool) {
        let c = rect.center();
        ctx.encoder.draw_rect(Rect::new(rect.x + 4.0, c.y - 3.0, 3.0, 6.0), color, 1.0);
        ctx.encoder.draw_triangles(
            &[
                Point::new(rect.x + 7.0, c.y - 4.0),
                Point::new(rect.x + 12.0, c.y - 7.0),
                Point::new(rect.x + 12.0, c.y + 7.0),
            ],
            color,
        );
        if muted {
            ctx.encoder.draw_line(
                Point::new(rect.x + rect.width - 5.0, rect.y + 5.0),
                Point::new(rect.x + rect.width - 2.5, rect.y + 7.5),
                1.4,
                color,
            );
            ctx.encoder.draw_line(
                Point::new(rect.x + rect.width - 2.5, rect.y + 5.0),
                Point::new(rect.x + rect.width - 5.0, rect.y + 7.5),
                1.4,
                color,
            );
        } else {
            ctx.encoder.draw_line(
                Point::new(rect.x + 14.0, c.y - 4.0),
                Point::new(rect.x + 14.0, c.y + 4.0),
                1.3,
                color,
            );
        }
    }

    fn paint_lock_icon(&self, ctx: &mut PaintContext, rect: Rect, color: Color, locked: bool) {
        let c = rect.center();
        ctx.encoder.draw_rect(Rect::new(c.x - 5.0, c.y - 1.0, 10.0, 7.0), color, 2.0);
        let shackle_top = if locked { c.y - 7.0 } else { c.y - 8.0 };
        ctx.encoder.draw_line(
            Point::new(c.x - 4.0, c.y - 1.0),
            Point::new(c.x - 4.0, shackle_top + 4.0),
            1.5,
            color,
        );
        ctx.encoder.draw_line(
            Point::new(c.x - 4.0, shackle_top + 4.0),
            Point::new(c.x, shackle_top),
            1.5,
            color,
        );
        ctx.encoder.draw_line(
            Point::new(c.x, shackle_top),
            Point::new(c.x + 4.0, shackle_top + 4.0),
            1.5,
            color,
        );
        if locked {
            ctx.encoder.draw_line(
                Point::new(c.x + 4.0, shackle_top + 4.0),
                Point::new(c.x + 4.0, c.y - 1.0),
                1.5,
                color,
            );
        } else {
            ctx.encoder.draw_line(
                Point::new(c.x + 4.0, shackle_top + 4.0),
                Point::new(c.x + 7.0, c.y - 3.0),
                1.5,
                color,
            );
        }
    }

    /// Resolve base fill, hover fill, and selected-border colors for a
    /// [`TimelineClip`] from its [`TimelineClipKind`] and the active theme.
    fn clip_color_tokens(
        colors: &mondrian_ui_theme::colors::ColorTokens,
        clip: &TimelineClip,
    ) -> (Color, Color, Color) {
        match clip.kind {
            TimelineClipKind::Video => (
                clip.color.unwrap_or(colors.timeline_clip_video),
                colors.timeline_clip_video_hover,
                colors.timeline_clip_selected_border,
            ),
            TimelineClipKind::Audio => (
                clip.color.unwrap_or(colors.timeline_clip_audio),
                colors.timeline_clip_audio_hover,
                colors.timeline_clip_audio_selected_border,
            ),
            TimelineClipKind::Adjustment => (
                clip.color.unwrap_or(colors.timeline_clip_adjustment),
                colors.timeline_clip_adjustment_hover,
                colors.timeline_clip_adjustment_selected_border,
            ),
            TimelineClipKind::NestedSequence => (
                clip.color.unwrap_or(colors.timeline_clip_nested),
                colors.timeline_clip_nested_hover,
                colors.timeline_clip_nested_selected_border,
            ),
            TimelineClipKind::SolidColor => (
                clip.color.unwrap_or(colors.timeline_clip_solid),
                colors.timeline_clip_solid_hover,
                colors.timeline_clip_selected_border,
            ),
        }
    }

    fn paint_clip(
        &self,
        ctx: &mut PaintContext,
        clip_ref: TimelineClipRef,
        clip: &TimelineClip,
        rect: Rect,
        dragging: bool,
    ) {
        let colors = &ctx.theme.colors;
        let selected = clip.selected || self.selected_clip == Some(clip_ref) || dragging;
        let hovered = self.hovered_clip == Some(clip_ref);
        let (base_fill, hover_fill, selected_border) = Self::clip_color_tokens(colors, clip);
        let mut fill = base_fill;
        if clip.disabled {
            fill.a *= 0.45;
        } else if dragging {
            fill = fill.lerp(colors.foreground, 0.12);
            fill.a *= 0.92;
        } else if hovered {
            fill = hover_fill;
        }
        if selected {
            let mut ring = selected_border;
            ring.a = if dragging { 0.72 } else { 0.62 };
            ctx.encoder.draw_rect(rect.inset(-1.0, -1.0), ring, 6.0);
        } else {
            ctx.encoder.draw_rect(
                rect.inset(-1.0, -1.0),
                color_with_alpha(colors.foreground, if hovered { 0.20 } else { 0.14 }),
                6.0,
            );
        }
        ctx.encoder.draw_rect(rect, fill, 5.0);

        if clip.kind == TimelineClipKind::Audio {
            let peaks: Option<Vec<f32>> = if !clip.waveform_peaks.is_empty() {
                Some(clip.waveform_peaks.clone())
            } else if let (Some(asset_id), Some(lookup)) =
                (clip.asset_id, self.waveform_lookup.as_ref())
            {
                lookup(
                    asset_id,
                    clip.source_revision,
                    clip.source_start_secs,
                    clip.source_end_secs,
                    rect.width as u32,
                )
            } else {
                None
            };
            if let Some(ref peaks) = peaks {
                self.paint_audio_waveform(ctx, rect, peaks, selected || hovered || dragging);
            }
        }
        if clip.nested && rect.width >= 28.0 && rect.height >= 18.0 {
            let nested_color = color_with_alpha(colors.foreground, 0.64);
            let outer = Rect::new(rect.x + rect.width - 14.0, rect.y + 6.0, 7.0, 6.0);
            let inner = Rect::new(rect.x + rect.width - 11.0, rect.y + 4.0, 7.0, 6.0);
            ctx.encoder.draw_rect(outer, color_with_alpha(nested_color, 0.22), 1.5);
            ctx.encoder.draw_rect(inner, color_with_alpha(nested_color, 0.36), 1.5);
        }
        let text_width = rect.width - 20.0;
        let text_clip = rect.inset(6.0, 2.0);
        if text_width > 1.0 && text_clip.width > 1.0 && text_clip.height > 1.0 {
            ctx.push_clip(text_clip);
            ctx.encoder.draw_text_box(
                &clip.label,
                ctx.theme.typography.small.font_size,
                snap_point(Point::new(rect.x + 10.0, rect.y + 9.0)),
                text_width,
                if clip.disabled {
                    color_with_alpha(colors.foreground, 0.74)
                } else {
                    colors.foreground
                },
            );
            ctx.pop_clip();
        }
    }

    fn paint_transition(
        &self,
        ctx: &mut PaintContext,
        transition_ref: TimelineTransitionRef,
        transition: &TimelineTransition,
        rect: Rect,
        dragging: bool,
    ) {
        let colors = &ctx.theme.colors;
        let selected =
            transition.selected || self.selected_transition == Some(transition_ref) || dragging;
        let hovered = self.hovered_transition == Some(transition_ref);
        let blocked = transition.handle_issue.is_some();
        let mut fill = if blocked {
            colors.timeline_transition_blocked
        } else if hovered {
            colors.timeline_transition_hover
        } else {
            colors.timeline_transition
        };
        if !transition.enabled {
            fill.a *= 0.42;
        } else if dragging {
            fill.a *= 0.92;
        } else {
            fill.a *= 0.84;
        }
        let border = if selected {
            colors.timeline_transition_selected
        } else if blocked {
            colors.timeline_transition_blocked
        } else {
            color_with_alpha(colors.foreground, 0.42)
        };
        ctx.encoder.draw_rect(rect.inset(-1.0, -1.0), border, 4.0);
        ctx.encoder.draw_rect(rect, fill, 3.0);

        let cut_x = self.frame_to_x(transition.cut_frame).clamp(rect.x, rect.x + rect.width);
        let middle_y = rect.y + rect.height * 0.5;
        let line = color_with_alpha(colors.foreground, if selected { 0.86 } else { 0.60 });
        ctx.encoder.draw_line(
            Point::new(rect.x, rect.y),
            Point::new(cut_x, middle_y),
            1.0,
            line,
        );
        ctx.encoder.draw_line(
            Point::new(rect.x, rect.y + rect.height),
            Point::new(cut_x, middle_y),
            1.0,
            line,
        );
        ctx.encoder.draw_line(
            Point::new(cut_x, middle_y),
            Point::new(rect.x + rect.width, rect.y),
            1.0,
            line,
        );
        ctx.encoder.draw_line(
            Point::new(cut_x, middle_y),
            Point::new(rect.x + rect.width, rect.y + rect.height),
            1.0,
            line,
        );
        ctx.encoder.draw_line(
            Point::new(cut_x, rect.y),
            Point::new(cut_x, rect.y + rect.height),
            1.0,
            border,
        );
        if selected || hovered {
            ctx.encoder.draw_rect(
                Rect::new(rect.x, rect.y, 3.0_f32.min(rect.width), rect.height),
                border,
                1.0,
            );
            ctx.encoder.draw_rect(
                Rect::new(
                    (rect.x + rect.width - 3.0).max(rect.x),
                    rect.y,
                    3.0_f32.min(rect.width),
                    rect.height,
                ),
                border,
                1.0,
            );
        }
    }

    fn paint_audio_waveform(
        &self,
        ctx: &mut PaintContext,
        rect: Rect,
        peaks: &[f32],
        emphasized: bool,
    ) {
        let mode = self.waveform_display;
        let vpad = match mode {
            WaveformDisplay::BottomAligned => 2.0,
            WaveformDisplay::Centered => (rect.height * 0.24).min(10.0),
        };
        let inner = rect.inset(6.0, vpad);
        if inner.width <= 2.0 || inner.height <= 4.0 || peaks.is_empty() {
            return;
        }
        let mut color = ctx.theme.colors.foreground;
        color.a = if emphasized { 0.46 } else { 0.30 };
        let column_count = inner.width.floor().max(1.0) as usize;

        match mode {
            WaveformDisplay::BottomAligned => {
                let baseline = inner.y + inner.height;
                for column in 0..column_count {
                    let sample_index = ((column as f32 / column_count as f32) * peaks.len() as f32)
                        .floor()
                        .min((peaks.len() - 1) as f32)
                        as usize;
                    let peak = peaks[sample_index].clamp(0.0, 1.0);
                    let amplitude = (peak * inner.height).max(1.0);
                    let x = inner.x + column as f32 + 0.5;
                    ctx.encoder.draw_line(
                        Point::new(x, baseline - amplitude),
                        Point::new(x, baseline),
                        1.0,
                        color,
                    );
                }
            }
            WaveformDisplay::Centered => {
                let mid_y = inner.y + inner.height * 0.5;
                for column in 0..column_count {
                    let sample_index = ((column as f32 / column_count as f32) * peaks.len() as f32)
                        .floor()
                        .min((peaks.len() - 1) as f32)
                        as usize;
                    let peak = peaks[sample_index].clamp(0.0, 1.0);
                    let half_height = (peak * inner.height * 0.5).max(1.0);
                    let x = inner.x + column as f32 + 0.5;
                    ctx.encoder.draw_line(
                        Point::new(x, mid_y - half_height),
                        Point::new(x, mid_y + half_height),
                        1.0,
                        color,
                    );
                }
            }
        }
    }

    fn paint_playhead(&self, ctx: &mut PaintContext) {
        if !self.enabled {
            return;
        }
        let x = self.frame_to_x(self.playhead_frame);
        if x < self.body_rect.x - 1.0 || x > self.body_rect.x + self.body_rect.width + 1.0 {
            return;
        }
        let colors = &ctx.theme.colors;
        let top_y = self.ruler_rect.y + 2.0;
        let shoulder_y = self.ruler_rect.y + 8.0;
        let marker_base_y = self.ruler_rect.y + 13.0;
        let p0 = Point::new(x - 5.0, top_y);
        let p1 = Point::new(x + 5.0, top_y);
        let p2 = Point::new(x + 5.0, shoulder_y);
        let p3 = Point::new(x, marker_base_y);
        let p4 = Point::new(x - 5.0, shoulder_y);
        let marker = [p0, p1, p2, p0, p2, p4, p4, p2, p3];
        ctx.encoder.draw_triangles(&marker, colors.timeline_playhead);
        ctx.encoder.draw_line(
            Point::new(x, marker_base_y),
            Point::new(x, self.body_rect.y + self.body_rect.height),
            1.0,
            colors.timeline_playhead,
        );
    }

    fn paint_snap_guide(&self, ctx: &mut PaintContext) {
        let Some(snap) = self.active_snap else {
            return;
        };
        let x = self.frame_to_x(snap.target.frame).round();
        if x < self.body_rect.x - 1.0 || x > self.body_rect.x + self.body_rect.width + 1.0 {
            return;
        }
        let mut color = ctx.theme.colors.ring;
        color.a = 0.72;
        ctx.encoder.draw_line(
            Point::new(x, self.ruler_rect.y),
            Point::new(x, self.body_rect.y + self.body_rect.height),
            1.0,
            color,
        );
    }

    fn paint_scrollbars(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        let metrics = TimelineMetrics::from_theme(ctx.theme);
        let track_color = colors.timeline_navigator_track;
        let corner = Rect::new(
            self.body_rect.x + self.body_rect.width,
            self.body_rect.y + self.body_rect.height,
            metrics.scrollbar_gutter,
            metrics.scrollbar_gutter,
        );
        ctx.encoder.draw_rect(corner, track_color, 0.0);
        if let (Some(track), Some(_thumb)) = (
            self.horizontal_scrollbar_track_rect(),
            self.horizontal_scrollbar_thumb_rect(),
        ) {
            let body_dragging = self.scrollbar_drag.is_some_and(|drag| {
                drag.axis == TimelineScrollbarAxis::Horizontal
                    && drag.kind == TimelineScrollbarDragKind::Thumb
            });
            let body_hovered =
                self.horizontal_scrollbar_hover_kind == Some(TimelineScrollbarDragKind::Thumb);
            let track_visual = Rect::new(
                track.x,
                track.center().y - metrics.scrollbar_track_visual_thickness * 0.5,
                track.width,
                metrics.scrollbar_track_visual_thickness,
            );
            ctx.encoder.draw_rect(
                track_visual,
                track_color,
                metrics.scrollbar_track_visual_thickness * 0.5,
            );

            if let Some(body) =
                self.horizontal_scrollbar_body_rect().filter(|body| body.width > 0.0)
            {
                ctx.encoder.draw_rect(
                    body,
                    navigator_body_color(colors, body_hovered, body_dragging),
                    metrics.scrollbar_body_visual_thickness * 0.5,
                );
            }
            self.paint_scrollbar_handle(
                ctx,
                TimelineScrollbarAxis::Horizontal,
                TimelineScrollbarDragKind::LeadingHandle,
                self.horizontal_scrollbar_hover_kind
                    == Some(TimelineScrollbarDragKind::LeadingHandle),
            );
            self.paint_scrollbar_handle(
                ctx,
                TimelineScrollbarAxis::Horizontal,
                TimelineScrollbarDragKind::TrailingHandle,
                self.horizontal_scrollbar_hover_kind
                    == Some(TimelineScrollbarDragKind::TrailingHandle),
            );
        }

        if let (Some(track), Some(_thumb)) = (
            self.vertical_scrollbar_track_rect(),
            self.vertical_scrollbar_thumb_rect(),
        ) {
            let body_dragging = self.scrollbar_drag.is_some_and(|drag| {
                drag.axis == TimelineScrollbarAxis::Vertical
                    && drag.kind == TimelineScrollbarDragKind::Thumb
            });
            let body_hovered =
                self.vertical_scrollbar_hover_kind == Some(TimelineScrollbarDragKind::Thumb);
            let track_visual = Rect::new(
                track.center().x - metrics.scrollbar_track_visual_thickness * 0.5,
                track.y,
                metrics.scrollbar_track_visual_thickness,
                track.height,
            );
            ctx.encoder.draw_rect(
                track_visual,
                track_color,
                metrics.scrollbar_track_visual_thickness * 0.5,
            );

            if let Some(body) = self.vertical_scrollbar_body_rect().filter(|body| body.height > 0.0)
            {
                ctx.encoder.draw_rect(
                    body,
                    navigator_body_color(colors, body_hovered, body_dragging),
                    metrics.scrollbar_body_visual_thickness * 0.5,
                );
            }
            self.paint_scrollbar_handle(
                ctx,
                TimelineScrollbarAxis::Vertical,
                TimelineScrollbarDragKind::LeadingHandle,
                self.vertical_scrollbar_hover_kind
                    == Some(TimelineScrollbarDragKind::LeadingHandle),
            );
            self.paint_scrollbar_handle(
                ctx,
                TimelineScrollbarAxis::Vertical,
                TimelineScrollbarDragKind::TrailingHandle,
                self.vertical_scrollbar_hover_kind
                    == Some(TimelineScrollbarDragKind::TrailingHandle),
            );
        }
    }

    fn paint_scrollbar_handle(
        &self,
        ctx: &mut PaintContext,
        axis: TimelineScrollbarAxis,
        kind: TimelineScrollbarDragKind,
        hovered: bool,
    ) {
        let rect = match axis {
            TimelineScrollbarAxis::Horizontal => self.horizontal_scrollbar_handle_rect(kind),
            TimelineScrollbarAxis::Vertical => self.vertical_scrollbar_handle_rect(kind),
        };
        let Some(rect) = rect else {
            return;
        };
        let dragging =
            self.scrollbar_drag.is_some_and(|drag| drag.axis == axis && drag.kind == kind);
        let center = rect.center();
        let metrics = TimelineMetrics::from_theme(ctx.theme);
        let visual = Rect::new(
            center.x - metrics.scrollbar_handle_visual_size * 0.5,
            center.y - metrics.scrollbar_handle_visual_size * 0.5,
            metrics.scrollbar_handle_visual_size,
            metrics.scrollbar_handle_visual_size,
        );
        ctx.encoder.draw_rect(
            visual.inset(-1.0, -1.0),
            ctx.theme.colors.timeline_navigator_handle_border,
            (metrics.scrollbar_handle_visual_size + 2.0) * 0.5,
        );
        ctx.encoder.draw_rect(
            visual,
            navigator_handle_color(&ctx.theme.colors, hovered, dragging),
            metrics.scrollbar_handle_visual_size * 0.5,
        );
    }
}

fn navigator_body_color(colors: &ColorTokens, hovered: bool, active: bool) -> Color {
    paint::navigator_body_color(colors, hovered, active)
}

fn navigator_handle_color(colors: &ColorTokens, hovered: bool, active: bool) -> Color {
    paint::navigator_handle_color(colors, hovered, active)
}

impl Widget for TimelineView {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(
            560.0,
            TimelineMetrics::current().toolbar_height + self.ruler_height + self.content_height(),
        ))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        let rects = timeline_model::layout_rects(bounds, self.header_width, self.ruler_height);
        self.toolbar_rect = rects.toolbar;
        self.header_rect = rects.header;
        self.ruler_rect = rects.ruler;
        self.body_rect = rects.body;
        self.clamp_scroll();
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.enabled {
            if self.chrome_tooltip().is_some() {
                ctx.tooltip.hide();
            }
            self.release_timeline_pointer_capture(ctx);
            self.focused = false;
            self.focus_visible = false;
            self.playhead_dragging = false;
            self.in_out_drag = None;
            self.asset_drop_hover = None;
            self.track_drag = None;
            self.clip_drag = None;
            self.trim_drag = None;
            self.transition_resize_drag = None;
            self.scrollbar_drag = None;
            self.hovered_clip = None;
            self.hovered_transition = None;
            self.hovered_track_control = None;
            self.hovered_toolbar_button = None;
            self.horizontal_scrollbar_hovered = false;
            self.vertical_scrollbar_hovered = false;
            self.horizontal_scrollbar_hover_kind = None;
            self.vertical_scrollbar_hover_kind = None;
            return EventResult::Ignored;
        }

        if let Some(result) = self.route_context_menu_event(event, ctx) {
            return result;
        }

        match event {
            UiEvent::DragEnter { payload: DragPayload::Asset(asset_id), position } => {
                self.hover_asset_drop(*asset_id, *position, ctx);
                return EventResult::Handled;
            }
            UiEvent::DragOver { position } if self.asset_drop_hover.is_some() => {
                if let Some(hover) = self.asset_drop_hover {
                    self.hover_asset_drop(hover.asset_id, *position, ctx);
                }
                return EventResult::Handled;
            }
            UiEvent::DragLeave if self.asset_drop_hover.is_some() => {
                self.asset_drop_hover = None;
                ctx.request_repaint();
                return EventResult::Handled;
            }
            UiEvent::Drop { payload: DragPayload::Asset(asset_id), position } => {
                return self.finish_asset_drop(*asset_id, *position, ctx);
            }
            UiEvent::MouseDown { position, button: MouseButton::Right, .. } => {
                if !self.bounds.contains(*position) {
                    return EventResult::Ignored;
                }
                self.focused = true;
                self.focus_visible = false;
                self.playhead_dragging = false;
                self.active_snap = None;
                self.in_out_drag = None;
                self.track_drag = None;
                self.clip_drag = None;
                self.trim_drag = None;
                self.transition_resize_drag = None;
                self.scrollbar_drag = None;
                if let Some(transition_ref) = self.hit_transition(*position) {
                    let _ = self.select_transition_from_input(transition_ref, ctx);
                    return self.open_context_menu(
                        *position,
                        self.transition_context_menu_items(),
                        ctx,
                    );
                }
                if let Some(cut_ref) = self.hit_cut(*position) {
                    return self.open_context_menu(
                        *position,
                        self.cut_context_menu_items(cut_ref),
                        ctx,
                    );
                }
                if let Some(clip_ref) = self.hit_clip(*position) {
                    let _ = self.select_clip_from_input(clip_ref, ctx);
                    return self.open_context_menu(*position, self.clip_context_menu_items(), ctx);
                }
                if let Some(track_ref) = self.track_header_at(*position) {
                    let _ = self.select_track_from_input(track_ref, ctx);
                }
                if self.ruler_rect.contains(*position)
                    || self.body_rect.contains(*position)
                    || self.header_rect.contains(*position)
                {
                    return self.open_context_menu(
                        *position,
                        self.timeline_context_menu_items(),
                        ctx,
                    );
                }
            }
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                if !self.bounds.contains(*position) {
                    return EventResult::Ignored;
                }
                self.context_menu = None;
                self.focused = true;
                self.focus_visible = false;
                if let Some(button) = self.toolbar_button_at(*position) {
                    return self.activate_toolbar_button(button, ctx);
                }
                for kind in [
                    TimelineScrollbarDragKind::LeadingHandle,
                    TimelineScrollbarDragKind::TrailingHandle,
                ] {
                    if self
                        .horizontal_scrollbar_handle_rect(kind)
                        .is_some_and(|handle| handle.contains(*position))
                    {
                        self.scrollbar_drag = Some(TimelineScrollbarDrag {
                            axis: TimelineScrollbarAxis::Horizontal,
                            kind,
                            start_pointer: position.x,
                            start_scroll: self.scroll_x,
                            start_pixels_per_frame: self.pixels_per_frame,
                            start_track_height: self.track_height,
                        });
                        self.horizontal_scrollbar_hovered = true;
                        self.request_timeline_pointer_capture(ctx);
                        ctx.request_repaint();
                        return EventResult::Handled;
                    }
                    if self
                        .vertical_scrollbar_handle_rect(kind)
                        .is_some_and(|handle| handle.contains(*position))
                    {
                        self.scrollbar_drag = Some(TimelineScrollbarDrag {
                            axis: TimelineScrollbarAxis::Vertical,
                            kind,
                            start_pointer: position.y,
                            start_scroll: self.scroll_y,
                            start_pixels_per_frame: self.pixels_per_frame,
                            start_track_height: self.track_height,
                        });
                        self.vertical_scrollbar_hovered = true;
                        self.request_timeline_pointer_capture(ctx);
                        ctx.request_repaint();
                        return EventResult::Handled;
                    }
                }
                if let Some(thumb) = self.horizontal_scrollbar_thumb_rect() {
                    if thumb.contains(*position) {
                        self.scrollbar_drag = Some(TimelineScrollbarDrag {
                            axis: TimelineScrollbarAxis::Horizontal,
                            kind: TimelineScrollbarDragKind::Thumb,
                            start_pointer: position.x,
                            start_scroll: self.scroll_x,
                            start_pixels_per_frame: self.pixels_per_frame,
                            start_track_height: self.track_height,
                        });
                        self.horizontal_scrollbar_hovered = true;
                        self.request_timeline_pointer_capture(ctx);
                        ctx.set_cursor(CursorRequest::Grabbing);
                        ctx.request_repaint();
                        return EventResult::Handled;
                    }
                }
                if let Some(thumb) = self.vertical_scrollbar_thumb_rect() {
                    if thumb.contains(*position) {
                        self.scrollbar_drag = Some(TimelineScrollbarDrag {
                            axis: TimelineScrollbarAxis::Vertical,
                            kind: TimelineScrollbarDragKind::Thumb,
                            start_pointer: position.y,
                            start_scroll: self.scroll_y,
                            start_pixels_per_frame: self.pixels_per_frame,
                            start_track_height: self.track_height,
                        });
                        self.vertical_scrollbar_hovered = true;
                        self.request_timeline_pointer_capture(ctx);
                        ctx.request_repaint();
                        return EventResult::Handled;
                    }
                }
                if let Some(track) = self.horizontal_scrollbar_track_rect() {
                    if track.contains(*position) {
                        let thumb = self.horizontal_scrollbar_thumb_rect();
                        let page = self.body_rect.width.max(1.0);
                        let changed = if thumb.is_some_and(|thumb| position.x < thumb.x) {
                            self.set_scroll_x(self.scroll_x - page)
                        } else if thumb.is_some_and(|thumb| position.x > thumb.x + thumb.width) {
                            self.set_scroll_x(self.scroll_x + page)
                        } else {
                            false
                        };
                        if changed {
                            ctx.request_repaint();
                        }
                        return EventResult::Handled;
                    }
                }
                if let Some(track) = self.vertical_scrollbar_track_rect() {
                    if track.contains(*position) {
                        let thumb = self.vertical_scrollbar_thumb_rect();
                        let page = self.body_rect.height.max(1.0);
                        let changed = if thumb.is_some_and(|thumb| position.y < thumb.y) {
                            self.set_scroll_y(self.scroll_y - page)
                        } else if thumb.is_some_and(|thumb| position.y > thumb.y + thumb.height) {
                            self.set_scroll_y(self.scroll_y + page)
                        } else {
                            false
                        };
                        if changed {
                            ctx.request_repaint();
                        }
                        return EventResult::Handled;
                    }
                }
                if self.ruler_rect.contains(*position) {
                    if let Some(point) = self.in_out_marker_at(*position) {
                        self.start_in_out_drag(point);
                        self.request_timeline_pointer_capture(ctx);
                        ctx.request_repaint();
                        return EventResult::Handled;
                    }
                    self.playhead_dragging = true;
                    self.request_timeline_pointer_capture(ctx);
                    self.seek_from_drag_input(self.x_to_frame(position.x), ctx);
                    return EventResult::Handled;
                }
                if let Some((track_ref, control)) = self.track_control_at(*position) {
                    return self.activate_track_control_from_input(track_ref, control, ctx);
                }
                if let Some(track_ref) = self.track_header_at(*position) {
                    let result = self.select_track_from_input(track_ref, ctx);
                    self.start_track_drag(track_ref);
                    self.request_timeline_pointer_capture(ctx);
                    return result;
                }
                if let Some(transition_ref) = self.hit_transition(*position) {
                    let result = self.select_transition_from_input(transition_ref, ctx);
                    if let Some(edge) = self.hit_transition_edge(transition_ref, *position) {
                        self.start_transition_resize(transition_ref, edge);
                        if self.transition_resize_drag.is_some() {
                            self.request_timeline_pointer_capture(ctx);
                        }
                    }
                    return result;
                }
                if let Some(clip_ref) = self.hit_clip(*position) {
                    if self.active_tool == TimelineTool::Blade {
                        return self.split_at_pointer_frame(*position, ctx);
                    }
                    let result = self.select_clip_from_input(clip_ref, ctx);
                    if let Some(edge) = self.hit_clip_edge(clip_ref, *position) {
                        self.start_trim_drag(clip_ref, edge);
                    } else {
                        self.start_clip_drag(clip_ref, *position);
                    }
                    if self.clip_drag.is_some() || self.trim_drag.is_some() {
                        self.request_timeline_pointer_capture(ctx);
                    }
                    return result;
                }
                if self.body_rect.contains(*position) {
                    if self.active_tool == TimelineTool::Blade {
                        return self.split_at_pointer_frame(*position, ctx);
                    }
                    // If the click is on the playhead line itself, start
                    // a playhead drag instead of a body seek.
                    let playhead_x = self.frame_to_x(self.playhead_frame);
                    if (position.x - playhead_x).abs() <= 4.0 {
                        self.playhead_dragging = true;
                        self.request_timeline_pointer_capture(ctx);
                        self.seek_from_drag_input(self.x_to_frame(position.x), ctx);
                        return EventResult::Handled;
                    }
                    self.seek_from_input(
                        self.x_to_frame(position.x),
                        TimelineSeekSource::Settled,
                        ctx,
                    );
                    return EventResult::Handled;
                }
            }
            UiEvent::MouseMove { position, .. } => {
                let active_drag = self.playhead_dragging
                    || self.in_out_drag.is_some()
                    || self.track_drag.is_some()
                    || self.clip_drag.is_some()
                    || self.trim_drag.is_some()
                    || self.transition_resize_drag.is_some()
                    || self.scrollbar_drag.is_some();
                if !self.bounds.contains(*position) && !active_drag {
                    if self.chrome_tooltip().is_some() {
                        ctx.tooltip.hide();
                    }
                    self.hovered_toolbar_button = None;
                    self.hovered_track_control = None;
                    self.hovered_clip = None;
                    self.hovered_transition = None;
                    if self.horizontal_scrollbar_hovered || self.vertical_scrollbar_hovered {
                        self.horizontal_scrollbar_hovered = false;
                        self.vertical_scrollbar_hovered = false;
                        self.horizontal_scrollbar_hover_kind = None;
                        self.vertical_scrollbar_hover_kind = None;
                        ctx.request_repaint();
                        return EventResult::Handled;
                    }
                    return EventResult::Ignored;
                }
                if self.playhead_dragging {
                    self.seek_from_drag_input(self.x_to_frame(position.x), ctx);
                    return EventResult::Handled;
                }
                if self.in_out_drag.is_some() {
                    self.drag_in_out_to(*position, ctx);
                    return EventResult::Handled;
                }
                if self.track_drag.is_some() {
                    self.drag_track_to(*position, ctx);
                    return EventResult::Handled;
                }
                if self.clip_drag.is_some() {
                    self.drag_clip_to(*position, ctx);
                    return EventResult::Handled;
                }
                if self.trim_drag.is_some() {
                    self.drag_trim_to(*position, ctx);
                    return EventResult::Handled;
                }
                if self.transition_resize_drag.is_some() {
                    self.drag_transition_resize_to(*position, ctx);
                    return EventResult::Handled;
                }
                if let Some(drag) = self.scrollbar_drag {
                    let changed = match (drag.axis, drag.kind) {
                        (TimelineScrollbarAxis::Horizontal, TimelineScrollbarDragKind::Thumb) => {
                            self.set_scroll_x(
                                self.scroll_x_for_thumb_delta(
                                    position.x - drag.start_pointer,
                                    drag,
                                ),
                            )
                        }
                        (TimelineScrollbarAxis::Vertical, TimelineScrollbarDragKind::Thumb) => self
                            .set_scroll_y(
                                self.scroll_y_for_thumb_delta(
                                    position.y - drag.start_pointer,
                                    drag,
                                ),
                            ),
                        (TimelineScrollbarAxis::Horizontal, _) => {
                            self.zoom_x_for_handle_delta(position.x - drag.start_pointer, drag)
                        }
                        (TimelineScrollbarAxis::Vertical, _) => self
                            .resize_tracks_for_handle_delta(position.y - drag.start_pointer, drag),
                    };
                    if changed {
                        ctx.request_repaint();
                    }
                    return EventResult::Handled;
                }
                if self.set_scrollbar_hovered(*position) {
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                if self.update_chrome_hover(*position, ctx) {
                    return EventResult::Handled;
                }
                let hovered_track_control = self.track_control_at(*position);
                if hovered_track_control != self.hovered_track_control {
                    self.hovered_track_control = hovered_track_control;
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                if self.update_content_hover(*position, ctx) {
                    return EventResult::Handled;
                }
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.playhead_dragging => {
                self.playhead_dragging = false;
                self.active_snap = None;
                self.dispatch_seek(self.playhead_frame, TimelineSeekSource::Settled, ctx);
                self.release_timeline_pointer_capture(ctx);
                ctx.request_repaint();
                return EventResult::Handled;
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.in_out_drag.is_some() => {
                self.active_snap = None;
                self.finish_in_out_drag(ctx);
                self.release_timeline_pointer_capture(ctx);
                return EventResult::Handled;
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.track_drag.is_some() => {
                self.active_snap = None;
                self.finish_track_drag(ctx);
                self.release_timeline_pointer_capture(ctx);
                return EventResult::Handled;
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.clip_drag.is_some() => {
                self.finish_clip_drag(ctx);
                self.release_timeline_pointer_capture(ctx);
                return EventResult::Handled;
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.trim_drag.is_some() => {
                self.finish_trim_drag(ctx);
                self.release_timeline_pointer_capture(ctx);
                return EventResult::Handled;
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. }
                if self.transition_resize_drag.is_some() =>
            {
                self.finish_transition_resize(ctx);
                self.release_timeline_pointer_capture(ctx);
                return EventResult::Handled;
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.scrollbar_drag.is_some() => {
                self.scrollbar_drag = None;
                self.release_timeline_pointer_capture(ctx);
                ctx.request_repaint();
                return EventResult::Handled;
            }
            UiEvent::FocusGained { source } => {
                self.focused = true;
                self.focus_visible = source.is_focus_visible();
                return EventResult::Handled;
            }
            UiEvent::FocusLost => {
                self.focused = false;
                self.focus_visible = false;
                self.playhead_dragging = false;
                self.active_snap = None;
                self.in_out_drag = None;
                self.asset_drop_hover = None;
                self.track_drag = None;
                self.clip_drag = None;
                self.trim_drag = None;
                self.transition_resize_drag = None;
                self.scrollbar_drag = None;
                self.context_menu = None;
                if self.chrome_tooltip().is_some() {
                    ctx.tooltip.hide();
                }
                self.hovered_toolbar_button = None;
                self.release_timeline_pointer_capture(ctx);
                return EventResult::Handled;
            }
            UiEvent::KeyDown { key, modifiers } if self.focused => {
                if !modifiers.ctrl && !modifiers.meta && !modifiers.alt && !modifiers.shift {
                    match key {
                        KeyCode::V => return self.set_active_tool(TimelineTool::Select, ctx),
                        KeyCode::B => return self.set_active_tool(TimelineTool::Blade, ctx),
                        KeyCode::S => return self.toggle_snapping(ctx),
                        _ => {}
                    }
                }
                if self.keyboard_edit_command(*key, *modifiers, ctx) {
                    return EventResult::Handled;
                }
                if self.keyboard_seek(*key, *modifiers, ctx) {
                    return EventResult::Handled;
                }
            }
            UiEvent::MouseWheel { delta, position, modifiers } => {
                if !self.bounds.contains(*position) {
                    return EventResult::Ignored;
                }
                let changed = if modifiers.ctrl || modifiers.meta {
                    let factor = if *delta < 0.0 { 1.12 } else { 1.0 / 1.12 };
                    self.zoom_at(position.x, factor)
                } else if modifiers.shift {
                    self.set_scroll_x(self.scroll_x + *delta)
                } else {
                    self.set_scroll_y(self.scroll_y + *delta)
                };
                if changed {
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                return EventResult::Ignored;
            }
            _ => {}
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        ctx.encoder.draw_rect(self.bounds, colors.card, 0.0);
        if self.focus_visible {
            let mut ring = colors.ring;
            ring.a = 0.38;
            ctx.encoder.draw_rect(
                self.bounds.inset(1.0, 1.0),
                ring,
                ctx.theme.spacing.radius_sm,
            );
        }
        self.paint_timeline_corner(ctx);
        self.paint_timeline_toolbar(ctx);

        ctx.push_clip(self.ruler_rect);
        self.paint_ruler(ctx);
        ctx.pop_clip();

        ctx.push_clip(Rect::new(
            self.header_rect.x,
            self.body_rect.y,
            self.header_rect.width + self.body_rect.width,
            self.body_rect.height,
        ));
        self.paint_tracks(ctx);
        self.paint_empty_state(ctx);
        ctx.pop_clip();

        ctx.push_clip(Rect::new(
            self.ruler_rect.x,
            self.ruler_rect.y,
            self.ruler_rect.width,
            self.ruler_rect.height + self.body_rect.height,
        ));
        self.paint_snap_guide(ctx);
        self.paint_playhead(ctx);
        ctx.pop_clip();

        ctx.push_clip(self.scrollbar_clip_rect());
        self.paint_scrollbars(ctx);
        ctx.pop_clip();
    }

    fn paint_overlay(&self, ctx: &mut PaintContext) {
        if let Some(menu) = &self.context_menu {
            menu.paint_overlay(ctx);
        }
    }

    fn overlay_hit_test(&self, point: Point) -> bool {
        self.context_menu.as_ref().is_some_and(|menu| menu.overlay_hit_test(point))
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn can_focus(&self) -> bool {
        self.enabled
    }

    fn accessibility(&self) -> Option<AccessibilityNode> {
        let track_count = self.tracks.len();
        let clip_count = self.tracks.iter().map(|track| track.clips.len()).sum();
        let selected_track_count = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(track_index, track)| {
                track.selected
                    || self.selected_track == Some(TimelineTrackRef { track_index: *track_index })
            })
            .count();
        let selected_clip_count = self
            .tracks
            .iter()
            .enumerate()
            .map(|(track_index, track)| {
                track
                    .clips
                    .iter()
                    .enumerate()
                    .filter(|(clip_index, clip)| {
                        clip.selected
                            || self.selected_clip
                                == Some(TimelineClipRef { track_index, clip_index: *clip_index })
                    })
                    .count()
            })
            .sum();

        Some(
            AccessibilityNode::new(self.id, AccessibilityRole::Timeline)
                .with_name("Timeline")
                .with_state(AccessibilityState {
                    focusable: self.enabled,
                    focused: self.focused,
                    disabled: !self.enabled,
                    ..AccessibilityState::default()
                })
                .with_value(AccessibilityValue::Timeline {
                    playhead_frame: self.playhead_frame,
                    max_frame: self.max_content_frame(),
                    track_count,
                    clip_count,
                    selected_track_count,
                    selected_clip_count,
                }),
        )
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::rc::Rc;

    use mondrian_platform_core::NoopPlatformService;
    use mondrian_ui_core::tooltip::{TooltipManager, TooltipState};
    use mondrian_ui_core::widget::{DrawCommandEncoder, EventRequests, PointerCaptureRequest};
    use mondrian_ui_theme::ThemePreset;

    use crate::test_utils::{DummyFocus, DummyShortcut, DummyTooltip};

    fn timeline() -> TimelineView {
        TimelineView::new(vec![
            TimelineTrack::video(
                "V1",
                vec![
                    TimelineClip::new("Intro", 0, 24).with_select_action(Action::SaveProject),
                    TimelineClip::new("Shot", 40, 30),
                ],
            ),
            TimelineTrack::audio("A1", vec![TimelineClip::new("Music", 12, 90)]),
        ])
        .with_playhead(12)
    }

    fn timecode_display(frame_rate: Rational) -> TimelineDisplayContract {
        mondrian_core::TimelineDisplaySettings::timecode(
            mondrian_core::SmpteCountingMode::NonDropFrame,
            0,
        )
        .resolve(frame_rate)
        .expect("valid test timecode display")
    }

    fn old_timeline_point(x: f32, y: f32) -> Point {
        Point::new(x, y + TimelineMetrics::current().toolbar_height)
    }

    fn timeline_content_point(x: f32, y: f32) -> Point {
        const LEGACY_TIMELINE_HEADER_WIDTH: f32 = 96.0;
        old_timeline_point(
            x + TimelineMetrics::current().track_header_min_width - LEGACY_TIMELINE_HEADER_WIDTH,
            y,
        )
    }

    #[test]
    fn timeline_accessibility_exposes_playhead_counts_selection_and_state() {
        let mut view = timeline().with_playhead(12);
        view.focused = true;
        view.selected_clip = Some(TimelineClipRef { track_index: 0, clip_index: 1 });

        let node = view.accessibility().expect("timeline should expose accessibility metadata");

        assert_eq!(node.role, AccessibilityRole::Timeline);
        assert_eq!(node.name.as_deref(), Some("Timeline"));
        assert!(node.state.focusable);
        assert!(node.state.focused);
        assert!(!node.state.disabled);
        assert_eq!(
            node.value,
            Some(AccessibilityValue::Timeline {
                playhead_frame: 12,
                max_frame: view.max_content_frame(),
                track_count: 2,
                clip_count: 3,
                selected_track_count: 0,
                selected_clip_count: 1,
            })
        );
    }

    #[test]
    fn disabled_timeline_accessibility_is_not_focusable() {
        let view = timeline().disabled();
        let node = view.accessibility().expect("timeline should expose accessibility metadata");

        assert_eq!(node.role, AccessibilityRole::Timeline);
        assert!(!node.state.focusable);
        assert!(!node.state.focused);
        assert!(node.state.disabled);
    }

    fn dispatching_ctx<'a>(
        focus: &'a mut DummyFocus,
        shortcut: &'a mut DummyShortcut,
        tooltip: &'a mut dyn TooltipManager,
        requests: &'a mut EventRequests,
        dispatch: &'a dyn Fn(Action),
    ) -> EventContext<'a> {
        EventContext {
            focus,
            shortcut,
            tooltip,
            dispatch,
            platform: &NoopPlatformService,
            requests,
        }
    }

    #[derive(Default)]
    struct TooltipRecorder {
        current: Option<TooltipState>,
        hide_count: usize,
    }

    impl TooltipManager for TooltipRecorder {
        fn show(&mut self, text: String, position: Point) {
            self.current = Some(TooltipState { text, position, visible: true });
        }

        fn hide(&mut self) {
            self.current = None;
            self.hide_count += 1;
        }

        fn current(&self) -> Option<&TooltipState> {
            self.current.as_ref()
        }

        fn update(&mut self, _delta_ms: u64) {}
    }

    #[derive(Default)]
    struct RecordingEncoder {
        rects: usize,
        rect_commands: Vec<(Rect, Color, f32)>,
        lines: usize,
        line_commands: Vec<(Point, Point, f32, Color)>,
        line_colors: Vec<Color>,
        triangles: usize,
        texts: Vec<String>,
        text_boxes: Vec<(String, Point, f32, Color)>,
        clips: usize,
        clip_bounds: Vec<Rect>,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, bounds: Rect) {
            self.clips += 1;
            self.clip_bounds.push(bounds);
        }

        fn pop_clip(&mut self) {}

        fn draw_rect(&mut self, bounds: Rect, color: Color, corner_radius: f32) {
            self.rects += 1;
            self.rect_commands.push((bounds, color, corner_radius));
        }

        fn draw_line(&mut self, start: Point, end: Point, width: f32, color: Color) {
            self.lines += 1;
            self.line_commands.push((start, end, width, color));
            self.line_colors.push(color);
        }

        fn draw_triangles(&mut self, vertices: &[Point], _color: Color) {
            self.triangles += vertices.len() / 3;
        }

        fn draw_text(&mut self, text: &str, _font_size: f32, _position: Point, _color: Color) {
            self.texts.push(text.into());
        }

        fn draw_text_box(
            &mut self,
            text: &str,
            _font_size: f32,
            position: Point,
            max_width: f32,
            color: Color,
        ) {
            self.texts.push(text.into());
            self.text_boxes.push((text.into(), position, max_width, color));
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}
        fn pop_transform(&mut self) {}
    }

    fn color_close(actual: Color, expected: Color) -> bool {
        (actual.r - expected.r).abs() <= 0.001
            && (actual.g - expected.g).abs() <= 0.001
            && (actual.b - expected.b).abs() <= 0.001
            && (actual.a - expected.a).abs() <= 0.001
    }

    #[test]
    fn clicking_clip_selects_and_dispatches_static_and_dynamic_actions() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline().on_clip_select(|clip_ref, _clip| {
            if clip_ref.track_index == 0 && clip_ref.clip_index == 0 {
                Action::CloseProject
            } else {
                Action::Pause
            }
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let result = view.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(108.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            view.selected_clip(),
            Some(TimelineClipRef { track_index: 0, clip_index: 0 })
        );
        assert_eq!(
            actions.borrow().as_slice(),
            &[Action::SaveProject, Action::CloseProject]
        );
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn clicking_track_header_selects_track_without_selecting_clip() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline().on_track_select(|track_ref, _track| {
            if track_ref.track_index == 0 {
                Action::FocusPanel(mondrian_editor_state::state::PanelKind::Timeline)
            } else {
                Action::Pause
            }
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let result = view.event(
            &UiEvent::MouseDown {
                position: old_timeline_point(20.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            view.selected_track(),
            Some(TimelineTrackRef { track_index: 0 })
        );
        assert_eq!(view.selected_clip(), None);
        assert_eq!(
            actions.borrow().as_slice(),
            &[Action::FocusPanel(
                mondrian_editor_state::state::PanelKind::Timeline
            )]
        );
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn dragging_track_header_commits_same_kind_reorder() {
        let actions = RefCell::new(Vec::new());
        let moves = Rc::new(RefCell::new(Vec::new()));
        let dispatch = |action| actions.borrow_mut().push(action);
        let move_log = Rc::clone(&moves);
        let mut view = TimelineView::new(vec![
            TimelineTrack::video("V1", vec![]),
            TimelineTrack::video("V2", vec![]),
            TimelineTrack::audio("A1", vec![]),
        ])
        .on_track_move(move |movement, _track| {
            move_log.borrow_mut().push(movement);
            Action::Duplicate
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 220.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: old_timeline_point(12.0, 105.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseMove {
                position: old_timeline_point(12.0, 55.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        let result = view.event(
            &UiEvent::MouseUp {
                position: old_timeline_point(12.0, 55.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            moves.borrow().as_slice(),
            &[TimelineTrackMove {
                track_ref: TimelineTrackRef { track_index: 1 },
                old_track_index: 1,
                new_track_index: 0,
            }]
        );
        assert_eq!(actions.borrow().as_slice(), &[Action::Duplicate]);
    }

    #[test]
    fn dragging_track_header_to_other_kind_does_not_commit_reorder() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = TimelineView::new(vec![
            TimelineTrack::video("V1", vec![]),
            TimelineTrack::audio("A1", vec![]),
        ])
        .on_track_move(|_, _| Action::Duplicate);
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: old_timeline_point(12.0, 55.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseMove {
                position: old_timeline_point(12.0, 105.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseUp {
                position: old_timeline_point(12.0, 105.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn dropping_asset_on_track_dispatches_drop_proposal() {
        let actions = RefCell::new(Vec::new());
        let drops = Rc::new(RefCell::new(Vec::new()));
        let dispatch = |action| actions.borrow_mut().push(action);
        let drop_log = Rc::clone(&drops);
        let asset_id = AssetId::new();
        let mut view = TimelineView::new(vec![
            TimelineTrack::video("V1", vec![]),
            TimelineTrack::audio("A1", vec![]),
        ])
        .with_header_width(128.0)
        .on_asset_drop(move |drop, _track| {
            drop_log.borrow_mut().push(drop);
            Action::Paste
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));
        let drop_position = Point::new(
            view.body_rect.x + 10.0 * view.pixels_per_frame,
            view.body_rect.y + view.track_height * 1.5,
        );

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            view.event(
                &UiEvent::DragEnter {
                    payload: DragPayload::Asset(asset_id),
                    position: drop_position,
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert!(view.asset_drop_hover.is_some());
        assert_eq!(
            view.event(
                &UiEvent::Drop {
                    payload: DragPayload::Asset(asset_id),
                    position: drop_position,
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(actions.borrow().as_slice(), &[Action::Paste]);
        assert_eq!(
            drops.borrow().as_slice(),
            &[TimelineAssetDrop {
                asset_id,
                track_ref: TimelineTrackRef { track_index: 1 },
                frame: 10,
            }]
        );
        assert!(view.asset_drop_hover.is_none());
    }

    #[test]
    fn dropping_asset_outside_timeline_body_does_not_dispatch() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let asset_id = AssetId::new();
        let mut view = TimelineView::new(vec![TimelineTrack::video("V1", vec![])])
            .with_header_width(128.0)
            .on_asset_drop(|_, _| Action::Paste);
        view.layout(Rect::new(0.0, 0.0, 520.0, 140.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::Drop {
                payload: DragPayload::Asset(asset_id),
                position: old_timeline_point(64.0, 48.0),
            },
            &mut ctx,
        );

        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn clicking_track_control_dispatches_without_selecting_track() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view =
            timeline()
                .with_header_width(128.0)
                .on_track_control(|control, track_ref, _track| {
                    if control == TimelineTrackControl::Mute && track_ref.track_index == 1 {
                        Action::Pause
                    } else {
                        Action::NoOp
                    }
                });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let audio_header = timeline_model::track_header_rect(
            view.header_rect,
            view.body_rect,
            view.track_height,
            view.scroll_y,
            1,
        );
        let audio_mute = view
            .track_control_rect(
                audio_header,
                view.tracks[1].kind,
                TimelineTrackControl::Mute,
            )
            .expect("audio tracks expose mute");
        let result = view.event(
            &UiEvent::MouseDown {
                position: audio_mute.center(),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(view.selected_track(), None);
        assert_eq!(view.selected_clip(), None);
        assert_eq!(actions.borrow().as_slice(), &[Action::Pause]);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn track_controls_are_kind_specific() {
        let mut view = timeline().with_header_width(128.0);
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let video_header = timeline_model::track_header_rect(
            view.header_rect,
            view.body_rect,
            view.track_height,
            view.scroll_y,
            0,
        );
        let audio_header = timeline_model::track_header_rect(
            view.header_rect,
            view.body_rect,
            view.track_height,
            view.scroll_y,
            1,
        );

        assert!(view
            .track_control_rect(
                video_header,
                view.tracks[0].kind,
                TimelineTrackControl::Mute
            )
            .is_none());
        assert!(view
            .track_control_rect(
                audio_header,
                view.tracks[1].kind,
                TimelineTrackControl::Visibility
            )
            .is_none());
        let video_visibility = view
            .track_control_rect(
                video_header,
                view.tracks[0].kind,
                TimelineTrackControl::Visibility,
            )
            .expect("video tracks expose visibility");
        let audio_mute = view
            .track_control_rect(
                audio_header,
                view.tracks[1].kind,
                TimelineTrackControl::Mute,
            )
            .expect("audio tracks expose mute");

        assert_eq!(
            view.track_control_at(video_visibility.center()),
            Some((
                TimelineTrackRef { track_index: 0 },
                TimelineTrackControl::Visibility
            ))
        );
        assert_eq!(
            view.track_control_at(audio_mute.center()),
            Some((
                TimelineTrackRef { track_index: 1 },
                TimelineTrackControl::Mute
            ))
        );
    }

    #[test]
    fn ruler_drag_seeks_and_uses_pointer_capture() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let seek_events = Rc::new(RefCell::new(Vec::new()));
        let seek_log = Rc::clone(&seek_events);
        let mut view = timeline().on_seek(move |seek| {
            seek_log.borrow_mut().push(seek);
            if seek.frame == 24 && seek.source == TimelineSeekSource::PointerDrag {
                Action::Play
            } else {
                Action::Pause
            }
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(192.0, 12.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(view.playhead_frame(), 24);
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(view.id()))
        );
        assert_eq!(actions.borrow().last(), Some(&Action::Play));
        assert_eq!(
            seek_events.borrow().as_slice(),
            &[TimelineSeek { frame: 24, source: TimelineSeekSource::PointerDrag }]
        );

        view.event(
            &UiEvent::MouseUp {
                position: timeline_content_point(192.0, 12.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Release(view.id()))
        );
        assert_eq!(
            seek_events.borrow().as_slice(),
            &[
                TimelineSeek { frame: 24, source: TimelineSeekSource::PointerDrag },
                TimelineSeek { frame: 24, source: TimelineSeekSource::Settled }
            ]
        );
    }

    #[test]
    fn ruler_drag_snaps_playhead_to_clip_edge() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline().on_seek(|seek| Action::Custom {
            namespace: "timeline.seek".into(),
            name: seek.frame.to_string(),
            payload: Default::default(),
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(252.0, 12.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(view.playhead_frame(), 40);
        assert!(view
            .active_snap
            .is_some_and(|snap| snap.target.kind == TimelineSnapKind::ClipStart));
        assert_eq!(
            actions.borrow().as_slice(),
            &[Action::Custom {
                namespace: "timeline.seek".into(),
                name: "40".into(),
                payload: Default::default(),
            }]
        );

        view.event(
            &UiEvent::MouseUp {
                position: timeline_content_point(252.0, 12.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(view.active_snap.is_none());
    }

    #[test]
    fn focus_lost_without_active_drag_preserves_pointer_capture_owner() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline();
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            view.event(&UiEvent::focus_gained_keyboard(), &mut ctx),
            EventResult::Handled
        );
        assert!(ctx.requests.pointer_capture.is_none());

        assert_eq!(
            view.event(&UiEvent::FocusLost, &mut ctx),
            EventResult::Handled
        );
        assert!(ctx.requests.pointer_capture.is_none());
        assert!(!view.focused);
        assert!(!view.pointer_capture_active);
    }

    #[test]
    fn focus_lost_during_ruler_drag_releases_pointer_capture() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline();
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(192.0, 12.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(view.playhead_dragging);
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(view.id()))
        );

        ctx.requests.pointer_capture = None;
        assert_eq!(
            view.event(&UiEvent::FocusLost, &mut ctx),
            EventResult::Handled
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Release(view.id()))
        );
        assert!(!view.playhead_dragging);
        assert!(!view.pointer_capture_active);
    }

    #[test]
    fn disabled_timeline_ignores_seek_and_focus() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline().on_seek(|_| Action::Play).disabled();
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert!(!view.is_enabled());
        assert!(!view.can_focus());
        assert_eq!(
            view.event(
                &UiEvent::MouseDown {
                    position: timeline_content_point(192.0, 12.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert_eq!(view.playhead_frame(), 12);
        assert!(actions.borrow().is_empty());
        assert!(ctx.requests.pointer_capture.is_none());
    }

    #[test]
    fn disabled_timeline_ignores_clip_drag_and_trim_proposals() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Intro", 40, 30)],
        )])
        .on_clip_move(|_, _| Action::DeleteSelection)
        .on_clip_trim(|_, _| Action::SaveProject)
        .disabled();
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            view.event(
                &UiEvent::MouseDown {
                    position: timeline_content_point(312.0, 42.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );
        view.event(
            &UiEvent::MouseMove {
                position: timeline_content_point(352.0, 42.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseUp {
                position: timeline_content_point(352.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(
            view.event(
                &UiEvent::MouseDown {
                    position: timeline_content_point(258.0, 42.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );
        view.event(
            &UiEvent::MouseMove {
                position: timeline_content_point(278.0, 42.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseUp {
                position: timeline_content_point(278.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(view.clip_drag.is_none());
        assert!(view.trim_drag.is_none());
        assert!(actions.borrow().is_empty());
        assert!(ctx.requests.pointer_capture.is_none());
    }

    #[test]
    fn disabling_while_dragging_playhead_releases_capture_on_next_event() {
        let mut view = timeline();
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            view.event(
                &UiEvent::MouseDown {
                    position: timeline_content_point(192.0, 12.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        view.set_enabled(false);
        assert_eq!(
            view.event(
                &UiEvent::MouseMove {
                    position: timeline_content_point(216.0, 12.0),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Release(view.id()))
        );
    }

    #[test]
    fn disabling_while_dragging_clip_releases_preserved_capture_on_next_event() {
        let mut view = timeline();
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            view.event(
                &UiEvent::MouseDown {
                    position: timeline_content_point(108.0, 42.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert!(view.clip_drag.is_some());
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(view.id()))
        );

        view.set_enabled(false);
        assert!(view.clip_drag.is_none());
        assert!(view.pointer_capture_active);
        assert_eq!(
            view.event(
                &UiEvent::MouseMove {
                    position: timeline_content_point(148.0, 42.0),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Release(view.id()))
        );
        assert!(!view.pointer_capture_active);
    }

    #[test]
    fn dragging_clip_previews_without_mutating_model_and_commits_move_on_release() {
        let moves = RefCell::new(Vec::new());
        let dispatch = |action| moves.borrow_mut().push(action);
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Intro", 0, 24)],
        )])
        .on_clip_move(|movement, _clip| Action::Custom {
            namespace: "timeline.move".into(),
            name: format!(
                "{}->{}:{}->{}",
                movement.clip_ref.track_index,
                movement.new_track_index,
                movement.old_start_frame,
                movement.new_start_frame
            ),
            payload: Default::default(),
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(108.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(view.id()))
        );

        view.event(
            &UiEvent::MouseMove {
                position: timeline_content_point(148.0, 42.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(view.tracks[0].clips[0].start_frame, 0);
        assert!(view
            .clip_drag
            .is_some_and(|drag| drag.current_start_frame == 10 && drag.current_track_index == 0));
        assert!(moves.borrow().is_empty());

        view.event(
            &UiEvent::MouseUp {
                position: timeline_content_point(148.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Release(view.id()))
        );
        assert_eq!(
            moves.borrow().as_slice(),
            &[Action::Custom {
                namespace: "timeline.move".into(),
                name: "0->0:0->10".into(),
                payload: Default::default(),
            }]
        );
    }

    #[test]
    fn dragging_clip_snaps_to_neighbor_edge_when_enabled() {
        let moves = RefCell::new(Vec::new());
        let dispatch = |action| moves.borrow_mut().push(action);
        let mut view = timeline().on_clip_move(|movement, _clip| Action::Custom {
            namespace: "timeline.move".into(),
            name: movement.new_start_frame.to_string(),
            payload: Default::default(),
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(108.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseMove {
                position: timeline_content_point(264.0, 42.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(view.clip_drag.is_some_and(|drag| drag.current_start_frame == 40));
        assert!(view
            .active_snap
            .is_some_and(|snap| snap.target.kind == TimelineSnapKind::ClipStart));

        view.event(
            &UiEvent::MouseUp {
                position: timeline_content_point(264.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            moves.borrow().as_slice(),
            &[
                Action::SaveProject,
                Action::Custom {
                    namespace: "timeline.move".into(),
                    name: "40".into(),
                    payload: Default::default(),
                },
            ]
        );
        assert!(view.active_snap.is_none());
    }

    #[test]
    fn dragging_clip_does_not_snap_when_snapping_is_disabled() {
        let moves = RefCell::new(Vec::new());
        let dispatch = |action| moves.borrow_mut().push(action);
        let mut view = timeline().on_clip_move(|movement, _clip| Action::Custom {
            namespace: "timeline.move".into(),
            name: movement.new_start_frame.to_string(),
            payload: Default::default(),
        });
        view.snapping_enabled = false;
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(108.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseMove {
                position: timeline_content_point(264.0, 42.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseUp {
                position: timeline_content_point(264.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            moves.borrow().as_slice(),
            &[
                Action::SaveProject,
                Action::Custom {
                    namespace: "timeline.move".into(),
                    name: "39".into(),
                    payload: Default::default(),
                },
            ]
        );
    }

    #[test]
    fn dragging_clip_to_compatible_track_commits_target_track() {
        let moves = RefCell::new(Vec::new());
        let dispatch = |action| moves.borrow_mut().push(action);
        let mut view = TimelineView::new(vec![
            TimelineTrack::video("V1", vec![TimelineClip::new("Intro", 0, 24)]),
            TimelineTrack::video("V2", vec![]),
        ])
        .on_clip_move(|movement, _clip| Action::Custom {
            namespace: "timeline.move".into(),
            name: format!(
                "{}->{}:{}->{}",
                movement.clip_ref.track_index,
                movement.new_track_index,
                movement.old_start_frame,
                movement.new_start_frame
            ),
            payload: Default::default(),
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(108.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseMove {
                position: timeline_content_point(148.0, 92.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(view
            .clip_drag
            .is_some_and(|drag| drag.current_start_frame == 10 && drag.current_track_index == 1));

        view.event(
            &UiEvent::MouseUp {
                position: timeline_content_point(148.0, 92.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            moves.borrow().as_slice(),
            &[Action::Custom {
                namespace: "timeline.move".into(),
                name: "0->1:0->10".into(),
                payload: Default::default(),
            }]
        );
    }

    #[test]
    fn dragging_clip_to_incompatible_track_keeps_source_track() {
        let moves = RefCell::new(Vec::new());
        let dispatch = |action| moves.borrow_mut().push(action);
        let mut view = TimelineView::new(vec![
            TimelineTrack::video("V1", vec![TimelineClip::new("Intro", 0, 24)]),
            TimelineTrack::audio("A1", vec![]),
        ])
        .on_clip_move(|movement, _clip| Action::Custom {
            namespace: "timeline.move".into(),
            name: format!(
                "{}->{}:{}->{}",
                movement.clip_ref.track_index,
                movement.new_track_index,
                movement.old_start_frame,
                movement.new_start_frame
            ),
            payload: Default::default(),
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(108.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseMove {
                position: timeline_content_point(148.0, 92.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseUp {
                position: timeline_content_point(148.0, 92.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            moves.borrow().as_slice(),
            &[Action::Custom {
                namespace: "timeline.move".into(),
                name: "0->0:0->10".into(),
                payload: Default::default(),
            }]
        );
    }

    #[test]
    fn dragging_clip_to_locked_target_track_keeps_source_track() {
        let moves = RefCell::new(Vec::new());
        let dispatch = |action| moves.borrow_mut().push(action);
        let mut view = TimelineView::new(vec![
            TimelineTrack::video("V1", vec![TimelineClip::new("Intro", 0, 24)]),
            TimelineTrack::video("V2", vec![]).locked(true),
        ])
        .on_clip_move(|movement, _clip| Action::Custom {
            namespace: "timeline.move".into(),
            name: format!(
                "{}->{}:{}->{}",
                movement.clip_ref.track_index,
                movement.new_track_index,
                movement.old_start_frame,
                movement.new_start_frame
            ),
            payload: Default::default(),
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(108.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseMove {
                position: timeline_content_point(148.0, 92.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(view
            .clip_drag
            .is_some_and(|drag| drag.current_start_frame == 10 && drag.current_track_index == 0));

        view.event(
            &UiEvent::MouseUp {
                position: timeline_content_point(148.0, 92.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            moves.borrow().as_slice(),
            &[Action::Custom {
                namespace: "timeline.move".into(),
                name: "0->0:0->10".into(),
                payload: Default::default(),
            }]
        );
    }

    #[test]
    fn clicking_clip_without_motion_does_not_commit_move() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline().on_clip_move(|_, _| Action::DeleteSelection);
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(108.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseUp {
                position: timeline_content_point(108.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(actions.borrow().as_slice(), &[Action::SaveProject]);
    }

    #[test]
    fn dragging_left_clip_edge_previews_trim_and_commits_on_release() {
        let trims = RefCell::new(Vec::new());
        let dispatch = |action| trims.borrow_mut().push(action);
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Intro", 40, 30)],
        )])
        .on_clip_trim(|trim, _clip| Action::Custom {
            namespace: "timeline.trim".into(),
            name: format!(
                "{:?}:{}+{}->{}+{}",
                trim.edge,
                trim.old_start_frame,
                trim.old_duration_frames,
                trim.new_start_frame,
                trim.new_duration_frames
            ),
            payload: Default::default(),
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(258.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(view.trim_drag.is_some());
        view.event(
            &UiEvent::MouseMove {
                position: timeline_content_point(278.0, 42.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(view.tracks[0].clips[0].start_frame, 40);
        assert!(view.trim_drag.is_some_and(|drag| {
            drag.current_start_frame == 46 && drag.current_duration_frames == 24
        }));
        assert!(trims.borrow().is_empty());

        view.event(
            &UiEvent::MouseUp {
                position: timeline_content_point(278.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            trims.borrow().as_slice(),
            &[Action::Custom {
                namespace: "timeline.trim".into(),
                name: "In:40+30->46+24".into(),
                payload: Default::default(),
            }]
        );
    }

    #[test]
    fn dragging_right_clip_edge_commits_out_trim() {
        let trims = RefCell::new(Vec::new());
        let dispatch = |action| trims.borrow_mut().push(action);
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Intro", 40, 30)],
        )])
        .on_clip_trim(|trim, _clip| Action::Custom {
            namespace: "timeline.trim".into(),
            name: format!(
                "{:?}:{}+{}->{}+{}",
                trim.edge,
                trim.old_start_frame,
                trim.old_duration_frames,
                trim.new_start_frame,
                trim.new_duration_frames
            ),
            payload: Default::default(),
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(374.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseMove {
                position: timeline_content_point(394.0, 42.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseUp {
                position: timeline_content_point(394.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            trims.borrow().as_slice(),
            &[Action::Custom {
                namespace: "timeline.trim".into(),
                name: "Out:40+30->40+35".into(),
                payload: Default::default(),
            }]
        );
    }

    #[test]
    fn dragging_clip_edge_snaps_trim_to_neighbor_edge() {
        let trims = RefCell::new(Vec::new());
        let dispatch = |action| trims.borrow_mut().push(action);
        let mut view = timeline().on_clip_trim(|trim, _clip| Action::Custom {
            namespace: "timeline.trim".into(),
            name: format!("{}+{}", trim.new_start_frame, trim.new_duration_frames),
            payload: Default::default(),
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(192.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseMove {
                position: timeline_content_point(252.0, 42.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(view.trim_drag.is_some_and(|drag| {
            drag.current_start_frame == 0 && drag.current_duration_frames == 40
        }));
        assert!(view
            .active_snap
            .is_some_and(|snap| snap.target.kind == TimelineSnapKind::ClipStart));

        view.event(
            &UiEvent::MouseUp {
                position: timeline_content_point(252.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            trims.borrow().as_slice(),
            &[
                Action::SaveProject,
                Action::Custom {
                    namespace: "timeline.trim".into(),
                    name: "0+40".into(),
                    payload: Default::default(),
                },
            ]
        );
    }

    #[test]
    fn clicking_edge_without_motion_does_not_commit_trim() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Intro", 40, 30)],
        )])
        .on_clip_trim(|_, _| Action::DeleteSelection);
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(258.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseUp {
                position: timeline_content_point(258.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn locked_track_clip_selects_but_does_not_drag() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Locked", 0, 24).with_select_action(Action::SaveProject)],
        )
        .locked(true)])
        .on_clip_move(|_, _| Action::DeleteSelection);
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(108.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseMove {
                position: timeline_content_point(148.0, 42.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(view.tracks[0].clips[0].start_frame, 0);
        assert_eq!(actions.borrow().as_slice(), &[Action::SaveProject]);
        assert_ne!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(view.id()))
        );
    }

    #[test]
    fn disabled_clip_remains_selectable_for_inspection() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Disabled", 0, 24)
                .disabled(true)
                .with_select_action(Action::SaveProject)],
        )]);
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            view.event(
                &UiEvent::MouseDown {
                    position: timeline_content_point(108.0, 42.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(
            view.selected_clip(),
            Some(TimelineClipRef { track_index: 0, clip_index: 0 })
        );
        assert_eq!(actions.borrow().as_slice(), &[Action::SaveProject]);
    }

    #[test]
    fn wheel_scrolls_vertically_and_shift_wheel_scrolls_horizontally() {
        let mut view = TimelineView::new(
            (0..8)
                .map(|track| {
                    TimelineTrack::video(
                        format!("V{track}"),
                        vec![TimelineClip::new("Clip", 0, 240)],
                    )
                })
                .collect(),
        );
        view.layout(Rect::new(0.0, 0.0, 320.0, 140.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let vertical_result = view.event(
            &UiEvent::MouseWheel {
                delta: 60.0,
                position: old_timeline_point(180.0, 90.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(vertical_result, EventResult::Handled);
        assert!(view.scroll_y() > 0.0);

        let horizontal_result = view.event(
            &UiEvent::MouseWheel {
                delta: 80.0,
                position: old_timeline_point(180.0, 90.0),
                modifiers: Modifiers::shift(),
            },
            &mut ctx,
        );
        assert_eq!(horizontal_result, EventResult::Handled);
        assert!(view.scroll_x() > 0.0);
    }

    #[test]
    fn wheel_at_timeline_scroll_boundary_is_ignored_for_parent_bubbling() {
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Clip", 0, 60)],
        )]);
        view.layout(Rect::new(0.0, 0.0, 640.0, 240.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let result = view.event(
            &UiEvent::MouseWheel {
                delta: -60.0,
                position: old_timeline_point(180.0, 90.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(view.scroll_y(), 0.0);
        assert!(!requests.repaint);
    }

    #[test]
    fn ctrl_wheel_at_zoom_limit_is_ignored_for_parent_bubbling() {
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Clip", 0, 240)],
        )]);
        view.layout(Rect::new(0.0, 0.0, 640.0, 240.0));
        view.pixels_per_frame = 64.0;

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let result = view.event(
            &UiEvent::MouseWheel {
                delta: -60.0,
                position: old_timeline_point(180.0, 90.0),
                modifiers: Modifiers::ctrl(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(view.pixels_per_frame(), 64.0);
        assert!(!requests.repaint);
    }

    #[test]
    fn horizontal_scrollbar_thumb_drag_updates_offset_and_releases_capture() {
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Long", 0, 320)],
        )]);
        view.layout(Rect::new(0.0, 0.0, 320.0, 140.0));
        let thumb = view
            .horizontal_scrollbar_thumb_rect()
            .expect("wide timeline should show horizontal scrollbar");
        let start = thumb.center();

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(view.id()))
        );
        view.event(
            &UiEvent::MouseMove {
                position: Point::new(start.x + 30.0, start.y),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(view.scroll_x() > 0.0);
        view.event(
            &UiEvent::MouseUp {
                position: Point::new(start.x + 30.0, start.y),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Release(view.id()))
        );
    }

    #[test]
    fn vertical_scrollbar_thumb_drag_updates_offset() {
        let mut view = TimelineView::new(
            (0..10).map(|track| TimelineTrack::video(format!("V{track}"), vec![])).collect(),
        );
        view.layout(Rect::new(0.0, 0.0, 320.0, 140.0));
        let thumb = view
            .vertical_scrollbar_thumb_rect()
            .expect("many tracks should show vertical scrollbar");
        let start = thumb.center();

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseMove {
                position: Point::new(start.x, start.y + 24.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(view.scroll_y() > 0.0);
    }

    #[test]
    fn horizontal_scrollbar_track_click_pages_without_seeking() {
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Long", 0, 1000)],
        )])
        .with_playhead(12);
        view.layout(Rect::new(0.0, 0.0, 320.0, 140.0));
        let track = view
            .horizontal_scrollbar_track_rect()
            .expect("wide timeline should show horizontal scrollbar");
        let thumb = view.horizontal_scrollbar_thumb_rect().expect("thumb");
        let click = Point::new(
            (thumb.x + thumb.width + 24.0).min(track.x + track.width - 1.0),
            track.center().y,
        );

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: click,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(view.scroll_x() > 0.0);
        assert_eq!(view.playhead_frame(), 12);
    }

    #[test]
    fn horizontal_scrollbar_track_click_pages_backward_and_clamps() {
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Long", 0, 1000)],
        )])
        .with_playhead(12);
        view.layout(Rect::new(0.0, 0.0, 320.0, 140.0));
        assert!(view.set_scroll_x(view.max_scroll_x()));
        let initial_scroll = view.scroll_x();
        let track = view
            .horizontal_scrollbar_track_rect()
            .expect("wide timeline should show horizontal scrollbar");
        let thumb = view.horizontal_scrollbar_thumb_rect().expect("thumb");
        let click = Point::new((thumb.x - 24.0).max(track.x + 1.0), track.center().y);

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            view.event(
                &UiEvent::MouseDown {
                    position: click,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert!(view.scroll_x() < initial_scroll);
        assert!(view.scroll_x() >= 0.0);
        assert_eq!(view.playhead_frame(), 12);

        assert!(view.set_scroll_x(view.body_rect.width));
        let before_forward_page = view.scroll_x();
        let thumb = view.horizontal_scrollbar_thumb_rect().expect("thumb after scroll");
        let click = Point::new(
            (thumb.x + thumb.width + 24.0).min(track.x + track.width - 1.0),
            track.center().y,
        );
        assert_eq!(
            view.event(
                &UiEvent::MouseDown {
                    position: click,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert!(view.scroll_x() > before_forward_page);
        assert!(view.scroll_x() <= view.max_scroll_x());
        assert_eq!(view.playhead_frame(), 12);
    }

    #[test]
    fn horizontal_scrollbar_handles_adjust_zoom_without_seeking() {
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Long", 0, 1000)],
        )])
        .with_playhead(12);
        view.layout(Rect::new(0.0, 0.0, 420.0, 160.0));
        let handle = view
            .horizontal_scrollbar_handle_rect(TimelineScrollbarDragKind::TrailingHandle)
            .expect("wide timeline should show trailing zoom handle")
            .center();
        let initial_zoom = view.pixels_per_frame();
        let initial_playhead = view.playhead_frame();

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            view.event(
                &UiEvent::MouseDown {
                    position: handle,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(view.id()))
        );
        assert_eq!(
            view.event(
                &UiEvent::MouseMove {
                    position: Point::new(handle.x - 40.0, handle.y),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert!(view.pixels_per_frame() > initial_zoom);
        assert_eq!(view.playhead_frame(), initial_playhead);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn horizontal_scrollbar_handle_drag_keeps_handle_under_pointer() {
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Long", 0, 320)],
        )]);
        view.layout(Rect::new(0.0, 0.0, 420.0, 160.0));
        let start = view
            .horizontal_scrollbar_handle_rect(TimelineScrollbarDragKind::TrailingHandle)
            .expect("wide timeline should show trailing zoom handle")
            .center();
        let target = Point::new(start.x - 18.0, start.y);

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            view.event(
                &UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            view.event(
                &UiEvent::MouseMove { position: target, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );

        let next = view
            .horizontal_scrollbar_handle_rect(TimelineScrollbarDragKind::TrailingHandle)
            .expect("handle should remain visible")
            .center();
        assert!(
            (next.x - target.x).abs() <= 2.0,
            "handle {next:?} target {target:?}"
        );
    }

    #[test]
    fn horizontal_scrollbar_leading_handle_keeps_resizing_after_intermediate_narrow_drag() {
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Long", 0, 320)],
        )]);
        view.layout(Rect::new(0.0, 0.0, 420.0, 160.0));
        let start = view
            .horizontal_scrollbar_handle_rect(TimelineScrollbarDragKind::LeadingHandle)
            .expect("wide timeline should show leading zoom handle")
            .center();

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            view.event(
                &UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            view.event(
                &UiEvent::MouseMove {
                    position: Point::new(start.x + 18.0, start.y),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        let intermediate_zoom = view.pixels_per_frame();
        ctx.requests.cursor = None;
        ctx.requests.repaint = false;

        assert_eq!(
            view.event(
                &UiEvent::MouseMove {
                    position: Point::new(start.x + 36.0, start.y),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert!(view.pixels_per_frame() > intermediate_zoom);
        assert!(view.scrollbar_drag.is_some());
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn horizontal_scrollbar_handle_zoom_clamps_to_min_and_max() {
        let mut max_zoom = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Long", 0, 1000)],
        )])
        .with_pixels_per_frame(TIMELINE_MAX_PIXELS_PER_FRAME);
        max_zoom.layout(Rect::new(0.0, 0.0, 420.0, 160.0));
        let max_drag = TimelineScrollbarDrag {
            axis: TimelineScrollbarAxis::Horizontal,
            kind: TimelineScrollbarDragKind::TrailingHandle,
            start_pointer: 0.0,
            start_scroll: max_zoom.scroll_x(),
            start_pixels_per_frame: max_zoom.pixels_per_frame(),
            start_track_height: max_zoom.track_height(),
        };

        max_zoom.zoom_x_for_handle_delta(-10_000.0, max_drag);
        assert_eq!(max_zoom.pixels_per_frame(), TIMELINE_MAX_PIXELS_PER_FRAME);
        assert!(max_zoom.scroll_x() <= max_zoom.max_scroll_x());

        let mut min_zoom = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Long", 0, 1000)],
        )])
        .with_pixels_per_frame(TIMELINE_MIN_PIXELS_PER_FRAME);
        min_zoom.layout(Rect::new(0.0, 0.0, 420.0, 160.0));
        let min_drag = TimelineScrollbarDrag {
            axis: TimelineScrollbarAxis::Horizontal,
            kind: TimelineScrollbarDragKind::TrailingHandle,
            start_pointer: 0.0,
            start_scroll: min_zoom.scroll_x(),
            start_pixels_per_frame: min_zoom.pixels_per_frame(),
            start_track_height: min_zoom.track_height(),
        };

        min_zoom.zoom_x_for_handle_delta(10_000.0, min_drag);
        assert_eq!(min_zoom.pixels_per_frame(), TIMELINE_MIN_PIXELS_PER_FRAME);
        assert!(min_zoom.scroll_x() <= min_zoom.max_scroll_x());
    }

    #[test]
    fn restoring_timeline_state_clamps_zoom_track_height_and_offsets() {
        let mut view = TimelineView::new(
            (0..12)
                .map(|track| {
                    TimelineTrack::video(
                        format!("V{track}"),
                        vec![TimelineClip::new("Long", 0, 1000)],
                    )
                })
                .collect(),
        );
        view.layout(Rect::new(0.0, 0.0, 360.0, 180.0));

        view.restore_state(&TimelineViewState {
            active_tool: TimelineTool::Blade,
            scroll_x: f32::MAX,
            scroll_y: f32::MAX,
            pixels_per_frame: f32::MAX,
            snapping_enabled: false,
            track_height: f32::MAX,
        });

        assert_eq!(view.active_tool(), TimelineTool::Blade);
        assert_eq!(view.pixels_per_frame(), TIMELINE_MAX_PIXELS_PER_FRAME);
        assert_eq!(
            view.track_height(),
            TimelineMetrics::current().max_track_height
        );
        assert_eq!(view.scroll_x(), view.max_scroll_x());
        assert_eq!(view.scroll_y(), view.max_scroll_y());
        assert!(!view.snapping_enabled());
    }

    #[test]
    fn horizontal_scrollbar_thumb_drag_continues_outside_bounds_with_grabbing_cursor() {
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Long", 0, 1000)],
        )]);
        view.layout(Rect::new(0.0, 0.0, 320.0, 140.0));
        let start = view
            .horizontal_scrollbar_thumb_rect()
            .expect("wide timeline should show horizontal scrollbar")
            .center();
        let target = Point::new(start.x + 34.0, view.bounds.y + view.bounds.height + 10.0);

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            view.event(
                &UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        ctx.requests.cursor = None;
        assert_eq!(
            view.event(
                &UiEvent::MouseMove { position: target, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert!(view.scroll_x() > 0.0);
    }

    #[test]
    fn vertical_scrollbar_handles_adjust_track_height() {
        let mut view = TimelineView::new(
            (0..12).map(|track| TimelineTrack::video(format!("V{track}"), vec![])).collect(),
        );
        view.layout(Rect::new(0.0, 0.0, 360.0, 180.0));
        let handle = view
            .vertical_scrollbar_handle_rect(TimelineScrollbarDragKind::TrailingHandle)
            .expect("many tracks should show trailing height handle")
            .center();
        let initial_height = view.track_height();

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: handle,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseMove {
                position: Point::new(handle.x, handle.y - 24.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(view.track_height() > initial_height);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn structural_timeline_commands_stay_in_context_menu_not_toolbar() {
        let view = timeline().on_track_add(|kind| match kind {
            TimelineTrackKind::Video => Action::Play,
            TimelineTrackKind::Audio => Action::Pause,
        });

        assert_eq!(
            TimelineView::toolbar_left_buttons(),
            [
                TimelineToolbarButton::Tool(TimelineTool::Select),
                TimelineToolbarButton::Tool(TimelineTool::Blade),
                TimelineToolbarButton::Snapping,
                TimelineToolbarButton::Edit(TimelineEditCommand::MarkInAtPlayhead),
                TimelineToolbarButton::Edit(TimelineEditCommand::MarkOutAtPlayhead),
            ]
        );
        let timeline_menu_items = view.timeline_context_menu_items();
        let submenu_labels: Vec<&str> = timeline_menu_items
            .iter()
            .flat_map(|item| {
                if let MenuItemKind::Submenu { ref children } = item.kind {
                    children.iter().map(|c| c.label.as_str()).collect::<Vec<_>>()
                } else {
                    vec![item.label.as_str()]
                }
            })
            .filter(|l: &&str| !l.is_empty())
            .collect();
        assert!(
            submenu_labels.contains(&"视频轨道"),
            "视频轨道 should be in the 新建 submenu"
        );
        assert!(
            submenu_labels.contains(&"音频轨道"),
            "音频轨道 should be in the 新建 submenu"
        );
        assert!(
            submenu_labels.contains(&"在播放头处分割"),
            "在播放头处分割 should remain available"
        );
        let clip_menu_items = view.clip_context_menu_items();
        let clip_labels: Vec<&str> = clip_menu_items
            .iter()
            .filter(|item| !item.is_separator())
            .map(|item| item.label.as_str())
            .collect();
        assert!(
            clip_labels.contains(&"删除剪辑"),
            "delete should remain available from the clip context menu"
        );
    }

    #[test]
    fn timeline_toolbar_mark_buttons_dispatch_commands() {
        let commands = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&commands);
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline().on_edit_command(move |command| {
            recorded.borrow_mut().push(command);
            match command {
                TimelineEditCommand::SplitAtPlayhead => Action::SplitClipAtPlayhead,
                TimelineEditCommand::MarkInAtPlayhead => Action::MarkInAtPlayhead,
                TimelineEditCommand::MarkOutAtPlayhead => Action::MarkOutAtPlayhead,
                _ => Action::NoOp,
            }
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));
        let initial_playhead = view.playhead_frame();
        assert!(view
            .toolbar_button_rect(TimelineToolbarButton::Edit(
                TimelineEditCommand::SplitAtPlayhead,
            ))
            .is_none());
        assert!(view
            .toolbar_button_rect(TimelineToolbarButton::Edit(
                TimelineEditCommand::DeleteSelection,
            ))
            .is_none());
        let mark_in = view
            .toolbar_button_rect(TimelineToolbarButton::Edit(
                TimelineEditCommand::MarkInAtPlayhead,
            ))
            .expect("wide toolbar should show mark-in control")
            .center();
        let mark_out = view
            .toolbar_button_rect(TimelineToolbarButton::Edit(
                TimelineEditCommand::MarkOutAtPlayhead,
            ))
            .expect("wide toolbar should show mark-out control")
            .center();

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        for position in [mark_in, mark_out] {
            assert_eq!(
                view.event(
                    &UiEvent::MouseDown {
                        position,
                        button: MouseButton::Left,
                        modifiers: Modifiers::none(),
                    },
                    &mut ctx,
                ),
                EventResult::Handled
            );
        }

        assert_eq!(view.playhead_frame(), initial_playhead);
        assert_eq!(
            commands.borrow().as_slice(),
            &[
                TimelineEditCommand::MarkInAtPlayhead,
                TimelineEditCommand::MarkOutAtPlayhead,
            ]
        );
        assert_eq!(
            actions.borrow().as_slice(),
            &[Action::MarkInAtPlayhead, Action::MarkOutAtPlayhead]
        );
    }

    #[test]
    fn timeline_edit_command_availability_disables_toolbar_menu_and_keyboard() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline()
            .on_edit_command(|command| match command {
                TimelineEditCommand::SplitAtPlayhead => Action::SplitClipAtPlayhead,
                TimelineEditCommand::DeleteSelection => Action::DeleteSelection,
                _ => Action::NoOp,
            })
            .on_edit_command_available(|command| {
                !matches!(
                    command,
                    TimelineEditCommand::SplitAtPlayhead | TimelineEditCommand::DeleteSelection
                )
            });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        assert!(view
            .toolbar_button_rect(TimelineToolbarButton::Edit(
                TimelineEditCommand::SplitAtPlayhead,
            ))
            .is_none());
        assert!(view
            .toolbar_button_rect(TimelineToolbarButton::Edit(
                TimelineEditCommand::DeleteSelection,
            ))
            .is_none());
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            view.event(
                &UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        let items = view.timeline_context_menu_items();
        assert!(!items.iter().find(|item| item.label == "在播放头处分割").expect("split").enabled);
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn timeline_chrome_buttons_update_and_hide_tooltips() {
        let mut view = timeline().on_edit_command_shortcut(|command| match command {
            TimelineEditCommand::MarkInAtPlayhead => Some("I".to_owned()),
            _ => None,
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let select = view.tool_button_rect(TimelineTool::Select).center();
        let mark_in = view
            .toolbar_button_rect(TimelineToolbarButton::Edit(
                TimelineEditCommand::MarkInAtPlayhead,
            ))
            .expect("wide toolbar should show mark-in control")
            .center();
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = TooltipRecorder::default();
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            view.event(
                &UiEvent::MouseMove { position: select, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            ctx.tooltip.current().map(|state| state.text.as_str()),
            Some("选择工具 (V)")
        );

        assert_eq!(
            view.event(
                &UiEvent::MouseMove { position: mark_in, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            ctx.tooltip.current().map(|state| state.text.as_str()),
            Some("标记入点 (I)")
        );

        assert_eq!(
            view.event(
                &UiEvent::MouseMove {
                    position: Point::new(-10.0, -10.0),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );
        assert!(ctx.tooltip.current().is_none());
    }

    #[test]
    fn hovering_truncated_clip_label_shows_full_name_tooltip() {
        let label = "广东-03-Sony-59.940 DF-Slog3-灰片.MP4";
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new(label, 0, 8)],
        )]);
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));
        let clip_rect = view.clip_rect(0, &view.tracks[0].clips[0]);

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = TooltipRecorder::default();
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            view.event(
                &UiEvent::MouseMove {
                    position: clip_rect.center(),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            ctx.tooltip.current().map(|state| state.text.as_str()),
            Some(label)
        );
    }

    #[test]
    fn ctrl_wheel_zooms_around_cursor() {
        let mut view = timeline();
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));
        let before = view.pixels_per_frame();
        let frame_before = view.x_to_frame(260.0);

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseWheel {
                delta: -20.0,
                position: old_timeline_point(260.0, 90.0),
                modifiers: Modifiers::ctrl(),
            },
            &mut ctx,
        );

        assert!(view.pixels_per_frame() > before);
        assert_eq!(view.x_to_frame(260.0), frame_before);
    }

    #[test]
    fn keyboard_seek_ignores_events_without_focus() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline().on_seek(|_| Action::Play);
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let result = view.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(view.playhead_frame(), 12);
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn focused_delete_dispatches_timeline_edit_command() {
        let actions = RefCell::new(Vec::new());
        let commands = Rc::new(RefCell::new(Vec::new()));
        let dispatch = |action| actions.borrow_mut().push(action);
        let command_log = Rc::clone(&commands);
        let mut view = timeline().on_edit_command(move |command| {
            command_log.borrow_mut().push(command);
            Action::DeleteSelection
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        let result = view.event(
            &UiEvent::KeyDown { key: KeyCode::Delete, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            commands.borrow().as_slice(),
            &[TimelineEditCommand::DeleteSelection]
        );
        assert_eq!(actions.borrow().as_slice(), &[Action::DeleteSelection]);
    }

    #[test]
    fn shifted_delete_dispatches_ripple_delete_command() {
        let actions = RefCell::new(Vec::new());
        let commands = Rc::new(RefCell::new(Vec::new()));
        let dispatch = |action| actions.borrow_mut().push(action);
        let command_log = Rc::clone(&commands);
        let mut view = timeline().on_edit_command(move |command| {
            command_log.borrow_mut().push(command);
            match command {
                TimelineEditCommand::CutSelection => Action::Cut,
                TimelineEditCommand::CopySelection => Action::Copy,
                TimelineEditCommand::PasteAtPlayhead => Action::Paste,
                TimelineEditCommand::DuplicateSelection => Action::Duplicate,
                TimelineEditCommand::DeleteSelection => Action::DeleteSelection,
                TimelineEditCommand::RippleDeleteSelection => Action::RippleDeleteSelection,
                TimelineEditCommand::SplitAtPlayhead => Action::SplitClipAtPlayhead,
                TimelineEditCommand::TrimSelectionInToPlayhead => Action::Cut,
                TimelineEditCommand::TrimSelectionOutToPlayhead => Action::Copy,
                TimelineEditCommand::RollSelectedCutToPlayhead => Action::SaveProject,
                TimelineEditCommand::EnableSelection => Action::Play,
                TimelineEditCommand::DisableSelection => Action::Pause,
                TimelineEditCommand::OpenNestedSequence(_) => Action::NoOp,
                TimelineEditCommand::MarkInAtPlayhead => Action::MarkInAtPlayhead,
                TimelineEditCommand::MarkOutAtPlayhead => Action::MarkOutAtPlayhead,
                TimelineEditCommand::ClearInOutPoints => Action::NoOp,
                TimelineEditCommand::TogglePlayback => Action::TogglePlay,
            }
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        let result = view.event(
            &UiEvent::KeyDown {
                key: KeyCode::Delete,
                modifiers: Modifiers::shift(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            commands.borrow().as_slice(),
            &[TimelineEditCommand::RippleDeleteSelection]
        );
        assert_eq!(
            actions.borrow().as_slice(),
            &[Action::RippleDeleteSelection]
        );
    }

    #[test]
    fn focused_clipboard_shortcuts_dispatch_timeline_edit_commands() {
        let actions = RefCell::new(Vec::new());
        let commands = Rc::new(RefCell::new(Vec::new()));
        let dispatch = |action| actions.borrow_mut().push(action);
        let command_log = Rc::clone(&commands);
        let mut view = timeline().on_edit_command(move |command| {
            command_log.borrow_mut().push(command);
            match command {
                TimelineEditCommand::CutSelection => Action::Cut,
                TimelineEditCommand::CopySelection => Action::Copy,
                TimelineEditCommand::PasteAtPlayhead => Action::Paste,
                TimelineEditCommand::DuplicateSelection => Action::Duplicate,
                TimelineEditCommand::SplitAtPlayhead => Action::SplitClipAtPlayhead,
                _ => Action::NoOp,
            }
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        for key in [KeyCode::X, KeyCode::C, KeyCode::V, KeyCode::D, KeyCode::K] {
            assert_eq!(
                view.event(
                    &UiEvent::KeyDown { key, modifiers: Modifiers::ctrl() },
                    &mut ctx,
                ),
                EventResult::Handled
            );
        }

        assert_eq!(
            commands.borrow().as_slice(),
            &[
                TimelineEditCommand::CutSelection,
                TimelineEditCommand::CopySelection,
                TimelineEditCommand::PasteAtPlayhead,
                TimelineEditCommand::DuplicateSelection,
                TimelineEditCommand::SplitAtPlayhead,
            ]
        );
        assert_eq!(
            actions.borrow().as_slice(),
            &[
                Action::Cut,
                Action::Copy,
                Action::Paste,
                Action::Duplicate,
                Action::SplitClipAtPlayhead,
            ]
        );
    }

    #[test]
    fn right_click_clip_opens_context_menu_and_dispatches_delete() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline().on_edit_command(|command| match command {
            TimelineEditCommand::CutSelection => Action::Cut,
            TimelineEditCommand::CopySelection => Action::Copy,
            TimelineEditCommand::PasteAtPlayhead => Action::Paste,
            TimelineEditCommand::DuplicateSelection => Action::Duplicate,
            TimelineEditCommand::DeleteSelection => Action::DeleteSelection,
            TimelineEditCommand::RippleDeleteSelection => Action::RippleDeleteSelection,
            TimelineEditCommand::SplitAtPlayhead => Action::SplitClipAtPlayhead,
            TimelineEditCommand::TrimSelectionInToPlayhead => Action::Cut,
            TimelineEditCommand::TrimSelectionOutToPlayhead => Action::Copy,
            TimelineEditCommand::RollSelectedCutToPlayhead => Action::SaveProject,
            TimelineEditCommand::EnableSelection => Action::Play,
            TimelineEditCommand::DisableSelection => Action::Pause,
            TimelineEditCommand::OpenNestedSequence(_) => Action::NoOp,
            TimelineEditCommand::MarkInAtPlayhead => Action::MarkInAtPlayhead,
            TimelineEditCommand::MarkOutAtPlayhead => Action::MarkOutAtPlayhead,
            TimelineEditCommand::ClearInOutPoints => Action::NoOp,
            TimelineEditCommand::TogglePlayback => Action::TogglePlay,
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let result = view.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(108.0, 42.0),
                button: MouseButton::Right,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            view.selected_clip(),
            Some(TimelineClipRef { track_index: 0, clip_index: 0 })
        );
        assert!(view.overlay_hit_test(Point::new(900.0, 900.0)));

        for _ in 0..5 {
            assert_eq!(
                view.event(
                    &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
                    &mut ctx,
                ),
                EventResult::Handled
            );
        }
        assert_eq!(
            view.event(
                &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            actions.borrow().as_slice(),
            &[Action::SaveProject, Action::DeleteSelection]
        );
        assert!(!view.overlay_hit_test(Point::new(900.0, 900.0)));
    }

    #[test]
    fn context_menu_shortcuts_come_from_host_callback() {
        let view =
            timeline()
                .on_edit_command(|_| Action::Copy)
                .on_edit_command_shortcut(|command| match command {
                    TimelineEditCommand::CopySelection => Some("Host+Copy".to_owned()),
                    _ => None,
                });

        let items = view.clip_context_menu_items();

        assert_eq!(items[0].shortcut, None);
        assert_eq!(items[1].shortcut.as_deref(), Some("Host+Copy"));
        assert_eq!(items[2].shortcut, None);
    }

    #[test]
    fn empty_timeline_context_menu_disables_selection_only_commands() {
        let view = timeline().on_edit_command(|command| match command {
            TimelineEditCommand::CutSelection => Action::Cut,
            TimelineEditCommand::CopySelection => Action::Copy,
            TimelineEditCommand::PasteAtPlayhead => Action::Paste,
            TimelineEditCommand::DuplicateSelection => Action::Duplicate,
            TimelineEditCommand::DeleteSelection => Action::DeleteSelection,
            TimelineEditCommand::RippleDeleteSelection => Action::RippleDeleteSelection,
            TimelineEditCommand::SplitAtPlayhead => Action::SplitClipAtPlayhead,
            TimelineEditCommand::TrimSelectionInToPlayhead => Action::Cut,
            TimelineEditCommand::TrimSelectionOutToPlayhead => Action::Copy,
            TimelineEditCommand::RollSelectedCutToPlayhead => Action::SaveProject,
            TimelineEditCommand::EnableSelection => Action::Play,
            TimelineEditCommand::DisableSelection => Action::Pause,
            TimelineEditCommand::OpenNestedSequence(_) => Action::NoOp,
            TimelineEditCommand::MarkInAtPlayhead => Action::MarkInAtPlayhead,
            TimelineEditCommand::MarkOutAtPlayhead => Action::MarkOutAtPlayhead,
            TimelineEditCommand::ClearInOutPoints => Action::CloseProject,
            TimelineEditCommand::TogglePlayback => Action::TogglePlay,
        });

        let items = view.timeline_context_menu_items();

        for label in [
            "剪切所选",
            "复制所选",
            "创建所选副本",
            "修剪所选入点到播放头",
            "修剪所选出点到播放头",
            "滚动所选剪辑点到播放头",
            "启用所选",
            "禁用所选",
            "清除入点/出点",
        ] {
            let item = items.iter().find(|item| item.label == label).expect(label);
            assert!(!item.enabled, "{label} should require a local target");
        }
        assert!(items.iter().find(|item| item.label == "粘贴到播放头").expect("paste").enabled);
        assert!(items.iter().find(|item| item.label == "在播放头处分割").expect("split").enabled);
        assert!(items.iter().find(|item| item.label == "标记入点").expect("mark in").enabled);
        assert!(items.iter().find(|item| item.label == "标记出点").expect("mark out").enabled);
    }

    #[test]
    fn timeline_context_menu_disables_split_when_playhead_misses_clips() {
        let view = timeline().with_playhead(200).on_edit_command(|command| match command {
            TimelineEditCommand::SplitAtPlayhead => Action::SplitClipAtPlayhead,
            _ => Action::NoOp,
        });

        let items = view.timeline_context_menu_items();

        let split = items.iter().find(|item| item.label == "在播放头处分割").expect("split");
        assert!(!split.enabled);
    }

    #[test]
    fn clip_context_menu_keeps_clip_edit_commands_enabled_for_selected_clip() {
        let mut view = timeline().on_edit_command(|command| match command {
            TimelineEditCommand::CutSelection => Action::Cut,
            TimelineEditCommand::CopySelection => Action::Copy,
            TimelineEditCommand::DuplicateSelection => Action::Duplicate,
            TimelineEditCommand::DeleteSelection => Action::DeleteSelection,
            TimelineEditCommand::RippleDeleteSelection => Action::RippleDeleteSelection,
            TimelineEditCommand::TrimSelectionInToPlayhead => Action::Cut,
            TimelineEditCommand::TrimSelectionOutToPlayhead => Action::Copy,
            TimelineEditCommand::RollSelectedCutToPlayhead => Action::SaveProject,
            TimelineEditCommand::EnableSelection => Action::Play,
            TimelineEditCommand::DisableSelection => Action::Pause,
            _ => Action::NoOp,
        });
        view.selected_clip = Some(TimelineClipRef { track_index: 0, clip_index: 0 });

        let items = view.clip_context_menu_items();

        for label in [
            "剪切剪辑",
            "复制剪辑",
            "创建剪辑副本",
            "删除剪辑",
            "波纹删除剪辑",
            "修剪入点到播放头",
            "修剪出点到播放头",
            "滚动剪辑点到播放头",
            "启用剪辑",
            "禁用剪辑",
        ] {
            let item = items.iter().find(|item| item.label == label).expect(label);
            assert!(
                item.enabled,
                "{label} should be available for a selected clip"
            );
        }
    }

    #[test]
    fn context_menu_can_dispatch_clear_in_out_command() {
        let commands = Rc::new(RefCell::new(Vec::new()));
        let command_log = Rc::clone(&commands);
        let view = timeline().with_in_out_points(0, Some(30)).on_edit_command(move |command| {
            command_log.borrow_mut().push(command);
            match command {
                TimelineEditCommand::ClearInOutPoints => Action::SaveProject,
                TimelineEditCommand::TogglePlayback => Action::TogglePlay,
                _ => Action::NoOp,
            }
        });

        let items = view.timeline_context_menu_items();
        let clear = items
            .iter()
            .find(|item| item.label == "清除入点/出点")
            .expect("clear in/out item");

        assert_eq!(clear.action(), Some(&Action::SaveProject));
        assert!(commands.borrow().contains(&TimelineEditCommand::ClearInOutPoints));
    }

    #[test]
    fn nested_clip_context_menu_includes_open_nested_sequence_command() {
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Nested", 0, 24).nested(true)],
        )])
        .on_edit_command(|command| match command {
            TimelineEditCommand::OpenNestedSequence(_) => Action::OpenProject("nested".into()),
            _ => Action::NoOp,
        });
        view.selected_clip = Some(TimelineClipRef { track_index: 0, clip_index: 0 });

        let items = view.clip_context_menu_items();

        let open = items
            .iter()
            .find(|item| item.label == "打开嵌套序列")
            .expect("open nested menu item");
        assert!(open.enabled);
    }

    #[test]
    fn normal_clip_context_menu_omits_open_nested_sequence_command() {
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Clip", 0, 24)],
        )]);
        view.selected_clip = Some(TimelineClipRef { track_index: 0, clip_index: 0 });

        let items = view.clip_context_menu_items();

        assert!(
            items.iter().all(|item| item.label != "打开嵌套序列"),
            "normal clips must not expose nested navigation"
        );
    }

    #[test]
    fn right_click_timeline_empty_space_context_menu_can_add_track() {
        let view = timeline().on_track_add(|kind| match kind {
            TimelineTrackKind::Video => Action::Play,
            TimelineTrackKind::Audio => Action::Pause,
        });
        let items = view.timeline_context_menu_items();
        // "新建" should be a submenu with video and audio track children.
        assert!(!items.is_empty());
        assert_eq!(items[0].label, "新建");
        match &items[0].kind {
            MenuItemKind::Submenu { children } => {
                assert_eq!(children.len(), 2);
                assert_eq!(children[0].label, "视频轨道");
                assert_eq!(children[1].label, "音频轨道");
            }
            _ => panic!("expected 新建 submenu"),
        }
    }

    #[test]
    fn right_click_without_command_factories_does_not_open_empty_context_menu() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline();
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let result = view.event(
            &UiEvent::MouseDown {
                position: Point::new(500.0, 42.0),
                button: MouseButton::Right,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert!(!view.overlay_hit_test(Point::new(900.0, 900.0)));
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn ctrl_b_dispatches_split_at_playhead_command() {
        let actions = RefCell::new(Vec::new());
        let commands = Rc::new(RefCell::new(Vec::new()));
        let dispatch = |action| actions.borrow_mut().push(action);
        let command_log = Rc::clone(&commands);
        let mut view = timeline().on_edit_command(move |command| {
            command_log.borrow_mut().push(command);
            Action::SplitClipAtPlayhead
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        let result = view.event(
            &UiEvent::KeyDown { key: KeyCode::B, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            commands.borrow().as_slice(),
            &[TimelineEditCommand::SplitAtPlayhead]
        );
        assert_eq!(actions.borrow().as_slice(), &[Action::SplitClipAtPlayhead]);
    }

    #[test]
    fn timeline_tool_buttons_and_shortcuts_switch_active_tool() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline();
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(view.active_tool(), TimelineTool::Select);
        assert_eq!(
            view.event(
                &UiEvent::MouseDown {
                    position: Point::new(39.0, 15.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(view.active_tool(), TimelineTool::Blade);

        view.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        assert_eq!(
            view.event(
                &UiEvent::KeyDown { key: KeyCode::V, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(view.active_tool(), TimelineTool::Select);
        assert_eq!(
            view.event(
                &UiEvent::KeyDown { key: KeyCode::B, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(view.active_tool(), TimelineTool::Blade);
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn snapping_is_enabled_by_default_and_persisted_in_view_state() {
        let mut view = timeline();
        assert!(view.snapping_enabled());

        view.snapping_enabled = false;
        let state = view.state();
        let mut rebuilt = timeline();
        rebuilt.restore_state(&state);

        assert!(!rebuilt.snapping_enabled());
    }

    #[test]
    fn snapping_toolbar_button_and_s_shortcut_toggle_local_state() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline();
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));
        let snap_button = view
            .toolbar_button_rect(TimelineToolbarButton::Snapping)
            .expect("wide toolbar should show snap control")
            .center();

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            view.event(
                &UiEvent::MouseDown {
                    position: snap_button,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert!(!view.snapping_enabled());

        view.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        assert_eq!(
            view.event(
                &UiEvent::KeyDown { key: KeyCode::S, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert!(view.snapping_enabled());
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn blade_tool_click_seeks_and_dispatches_split_without_dragging_clip() {
        let actions = RefCell::new(Vec::new());
        let commands = Rc::new(RefCell::new(Vec::new()));
        let dispatch = |action| actions.borrow_mut().push(action);
        let command_log = Rc::clone(&commands);
        let mut view = timeline()
            .on_seek(|seek| Action::Custom {
                namespace: "timeline.seek".into(),
                name: seek.frame.to_string(),
                payload: Default::default(),
            })
            .on_edit_command(move |command| {
                command_log.borrow_mut().push(command);
                Action::SplitClipAtPlayhead
            });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(
            &UiEvent::MouseDown {
                position: Point::new(39.0, 15.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        let result = view.event(
            &UiEvent::MouseDown {
                position: timeline_content_point(104.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(view.active_tool(), TimelineTool::Blade);
        assert_eq!(view.playhead_frame(), 2);
        assert_eq!(
            commands.borrow().as_slice(),
            &[TimelineEditCommand::SplitAtPlayhead]
        );
        assert_eq!(
            actions.borrow().as_slice(),
            &[
                Action::Custom {
                    namespace: "timeline.seek".into(),
                    name: "2".into(),
                    payload: Default::default(),
                },
                Action::SplitClipAtPlayhead,
            ]
        );
        assert!(ctx.requests.pointer_capture.is_none());
    }

    #[test]
    fn focused_i_and_o_dispatch_mark_in_out_commands() {
        let actions = RefCell::new(Vec::new());
        let commands = Rc::new(RefCell::new(Vec::new()));
        let dispatch = |action| actions.borrow_mut().push(action);
        let command_log = Rc::clone(&commands);
        let mut view = timeline().on_edit_command(move |command| {
            command_log.borrow_mut().push(command);
            match command {
                TimelineEditCommand::MarkInAtPlayhead => Action::MarkInAtPlayhead,
                TimelineEditCommand::MarkOutAtPlayhead => Action::MarkOutAtPlayhead,
                _ => Action::NoOp,
            }
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        assert_eq!(
            view.event(
                &UiEvent::KeyDown { key: KeyCode::I, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            view.event(
                &UiEvent::KeyDown { key: KeyCode::O, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(
            commands.borrow().as_slice(),
            &[
                TimelineEditCommand::MarkInAtPlayhead,
                TimelineEditCommand::MarkOutAtPlayhead,
            ]
        );
        assert_eq!(
            actions.borrow().as_slice(),
            &[Action::MarkInAtPlayhead, Action::MarkOutAtPlayhead]
        );
    }

    #[test]
    fn focused_space_dispatches_toggle_playback_command() {
        let actions = RefCell::new(Vec::new());
        let commands = Rc::new(RefCell::new(Vec::new()));
        let dispatch = |action| actions.borrow_mut().push(action);
        let command_log = Rc::clone(&commands);
        let mut view = timeline().on_edit_command(move |command| {
            command_log.borrow_mut().push(command);
            match command {
                TimelineEditCommand::TogglePlayback => Action::TogglePlay,
                _ => Action::NoOp,
            }
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        assert_eq!(
            view.event(
                &UiEvent::KeyDown { key: KeyCode::Space, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            view.event(
                &UiEvent::KeyDown { key: KeyCode::Space, modifiers: Modifiers::ctrl() },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert_eq!(
            commands.borrow().as_slice(),
            &[TimelineEditCommand::TogglePlayback]
        );
        assert_eq!(actions.borrow().as_slice(), &[Action::TogglePlay]);
    }

    #[test]
    fn dragging_in_out_marker_previews_and_commits_on_release() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view =
            timeline().with_in_out_points(10, Some(30)).on_in_out_point(|point, frame| {
                Action::Custom {
                    namespace: "timeline.range".into(),
                    name: format!("{point:?}:{frame}"),
                    payload: Default::default(),
                }
            });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            view.event(
                &UiEvent::MouseDown {
                    position: timeline_content_point(136.0, 12.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            view.event(
                &UiEvent::MouseMove {
                    position: timeline_content_point(176.0, 12.0),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(view.in_point_frame(), 20);
        assert_eq!(
            view.event(
                &UiEvent::MouseUp {
                    position: timeline_content_point(176.0, 12.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            actions.borrow().as_slice(),
            &[Action::Custom {
                namespace: "timeline.range".into(),
                name: "In:20".into(),
                payload: Default::default(),
            }]
        );
    }

    #[test]
    fn disabling_timeline_cancels_pending_in_out_drag() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view =
            timeline().with_in_out_points(10, Some(30)).on_in_out_point(|point, frame| {
                Action::Custom {
                    namespace: "timeline.range".into(),
                    name: format!("{point:?}:{frame}"),
                    payload: Default::default(),
                }
            });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            view.event(
                &UiEvent::MouseDown {
                    position: timeline_content_point(136.0, 12.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert!(view.in_out_drag.is_some());

        view.set_enabled(false);
        assert!(view.in_out_drag.is_none());

        view.set_enabled(true);
        assert_eq!(
            view.event(
                &UiEvent::MouseUp {
                    position: timeline_content_point(176.0, 12.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn disabling_timeline_cancels_pending_playhead_drag() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline().on_seek(|seek| Action::Custom {
            namespace: "timeline.seek".into(),
            name: seek.frame.to_string(),
            payload: Default::default(),
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            view.event(
                &UiEvent::MouseDown {
                    position: timeline_content_point(136.0, 12.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert!(view.playhead_dragging);
        actions.borrow_mut().clear();

        view.set_enabled(false);
        assert!(!view.playhead_dragging);

        view.set_enabled(true);
        assert_eq!(
            view.event(
                &UiEvent::MouseMove {
                    position: timeline_content_point(176.0, 12.0),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn in_out_points_are_normalized_and_painted() {
        let mut view = timeline().with_in_out_points(40, Some(12));
        assert_eq!(view.in_point_frame(), 40);
        assert_eq!(view.out_point_frame(), Some(40));
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 520.0, 180.0),
        };

        view.paint(&mut ctx);

        assert!(encoder.rects >= 8);
        assert!(encoder.lines >= 7);
    }

    #[test]
    fn ruler_labels_follow_configured_frame_rate_instead_of_defaulting_to_thirty() {
        let view = timeline().with_timeline_display(timecode_display(Rational::FPS_25));

        assert_eq!(view.ruler_label_for_frame(250, 25), "00:00:10:00");
        assert_eq!(view.ruler_label_for_frame(250, 125), "00:10");
    }

    #[test]
    fn ruler_labels_use_the_resolved_drop_frame_origin_contract() {
        let display = mondrian_core::TimelineDisplaySettings::timecode(
            mondrian_core::SmpteCountingMode::DropFrame,
            107_892,
        )
        .resolve(Rational::FPS_2997)
        .expect("drop-frame display");
        let view = timeline().with_timeline_display(display);

        assert_eq!(view.ruler_label_for_frame(0, 1), "01:00:00;00");
        assert_eq!(view.ruler_label_for_frame(1_800, 1), "01:01:00;02");
    }

    #[test]
    fn ruler_tick_steps_use_dense_minor_marks_and_meaningful_major_labels() {
        let view = timeline()
            .with_timeline_display(timecode_display(Rational::FPS_25))
            .with_pixels_per_frame(0.5);

        let minor_step = view.tick_step_frames();
        let major_step = view.major_tick_step_frames(minor_step);

        assert_eq!(minor_step, 50);
        assert_eq!(major_step, 250);
        assert!(major_step > minor_step);
        assert_eq!(major_step % minor_step, 0);
    }

    #[test]
    fn focused_keyboard_seek_moves_playhead_and_dispatches() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let seek_events = Rc::new(RefCell::new(Vec::new()));
        let seek_log = Rc::clone(&seek_events);
        let mut view = timeline().on_seek(move |seek| {
            seek_log.borrow_mut().push(seek);
            Action::Custom {
                namespace: "timeline.seek".into(),
                name: seek.frame.to_string(),
                payload: Default::default(),
            }
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        let result = view.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::shift() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(view.playhead_frame(), 22);
        assert_eq!(
            actions.borrow().as_slice(),
            &[Action::Custom {
                namespace: "timeline.seek".into(),
                name: "22".into(),
                payload: Default::default(),
            }]
        );
        assert_eq!(
            seek_events.borrow().first(),
            Some(&TimelineSeek { frame: 22, source: TimelineSeekSource::Settled })
        );

        view.event(
            &UiEvent::KeyDown { key: KeyCode::Home, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(view.playhead_frame(), 0);

        view.event(
            &UiEvent::KeyDown { key: KeyCode::End, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(view.playhead_frame(), view.max_content_frame());
    }

    #[test]
    fn focused_keyboard_seek_ignores_unowned_modified_keys() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline().with_playhead(12).on_seek(|seek| Action::Custom {
            namespace: "timeline.seek".into(),
            name: seek.frame.to_string(),
            payload: Default::default(),
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        view.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        for (key, modifiers) in [
            (KeyCode::Left, Modifiers::ctrl()),
            (
                KeyCode::Right,
                Modifiers { alt: true, ..Default::default() },
            ),
            (
                KeyCode::Home,
                Modifiers { meta: true, ..Default::default() },
            ),
            (KeyCode::End, Modifiers::ctrl()),
        ] {
            assert_eq!(
                view.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Ignored
            );
            assert_eq!(view.playhead_frame(), 12);
        }
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn track_kind_badge_measures_label_with_equal_padding() {
        let view = TimelineView::new(vec![TimelineTrack::video("V12", Vec::new())]);
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 220.0, 80.0),
        };
        let header = Rect::new(0.0, 0.0, 132.0, 42.0);
        let rect = view.paint_track_kind_badge(&mut ctx, header, &view.tracks[0], false);
        let text_width = measure_single_line("V12", theme.typography.metadata.font_size).0;

        assert_eq!(rect.height, 18.0);
        assert_eq!(rect.width, (text_width + 12.0).max(22.0));
        assert!(encoder.texts.iter().any(|text| text == "V12"));
    }

    #[test]
    fn selected_clip_paints_tokenized_border() {
        let view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Selected", 0, 20).selected(true)],
        )]);
        let rect = Rect::new(24.0, 36.0, 96.0, 34.0);
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 180.0, 100.0),
        };

        view.paint_clip(
            &mut ctx,
            TimelineClipRef { track_index: 0, clip_index: 0 },
            &view.tracks[0].clips[0],
            rect,
            false,
        );

        let mut selected_border = theme.colors.timeline_clip_selected_border;
        selected_border.a = 0.62;
        assert!(encoder
            .rect_commands
            .iter()
            .any(|(bounds, color, _)| *bounds == rect.inset(-1.0, -1.0)
                && color_close(*color, selected_border)));
    }

    #[test]
    fn hovered_clip_paints_hover_fill() {
        let mut view = TimelineView::new(vec![TimelineTrack::audio(
            "A1",
            vec![TimelineClip::new("Hover", 0, 20)],
        )]);
        view.hovered_clip = Some(TimelineClipRef { track_index: 0, clip_index: 0 });
        let rect = Rect::new(24.0, 36.0, 96.0, 34.0);
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 180.0, 100.0),
        };

        view.paint_clip(
            &mut ctx,
            TimelineClipRef { track_index: 0, clip_index: 0 },
            &view.tracks[0].clips[0],
            rect,
            false,
        );

        assert!(
            encoder.rect_commands.iter().any(|(bounds, color, _)| *bounds == rect
                && color_close(*color, theme.colors.timeline_clip_audio_hover))
        );
    }

    #[test]
    fn disabled_clip_uses_muted_fill_and_clipped_label_color() {
        let view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Disabled", 0, 20).disabled(true)],
        )]);
        let rect = Rect::new(24.0, 36.0, 96.0, 34.0);
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 180.0, 100.0),
        };

        view.paint_clip(
            &mut ctx,
            TimelineClipRef { track_index: 0, clip_index: 0 },
            &view.tracks[0].clips[0],
            rect,
            false,
        );

        let mut disabled_fill = theme.colors.timeline_clip_video;
        disabled_fill.a *= 0.45;
        assert!(encoder
            .rect_commands
            .iter()
            .any(|(bounds, color, _)| *bounds == rect && color_close(*color, disabled_fill)));
        assert!(encoder.clip_bounds.contains(&rect.inset(6.0, 2.0)));

        let text = encoder
            .text_boxes
            .iter()
            .find(|(text, _, _, _)| text == "Disabled")
            .expect("disabled label text box");
        assert!(color_close(
            text.3,
            color_with_alpha(theme.colors.foreground, 0.74)
        ));
    }

    #[test]
    fn narrow_clip_skips_label_text_box_without_overflowing() {
        let view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Tiny", 0, 1)],
        )]);
        let rect = Rect::new(24.0, 36.0, 8.0, 24.0);
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 80.0, 80.0),
        };

        view.paint_clip(
            &mut ctx,
            TimelineClipRef { track_index: 0, clip_index: 0 },
            &view.tracks[0].clips[0],
            rect,
            false,
        );

        assert!(encoder.text_boxes.iter().all(|(text, _, _, _)| text != "Tiny"));
        assert!(encoder
            .rect_commands
            .iter()
            .all(|(bounds, _, _)| bounds.width.is_finite() && bounds.height.is_finite()));
    }

    #[test]
    fn waveform_peaks_are_clamped_and_painted_inside_clip() {
        let view = TimelineView::new(vec![TimelineTrack::audio("A1", Vec::new())]);
        let clip = TimelineClip::new("Wave", 0, 20).with_waveform_peaks(vec![-1.0, 0.5, 2.0]);
        let rect = Rect::new(20.0, 30.0, 26.0, 32.0);
        let inner = rect.inset(6.0, 2.0);
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 80.0, 80.0),
        };

        view.paint_audio_waveform(&mut ctx, rect, &clip.waveform_peaks, false);

        assert_eq!(encoder.line_commands.len(), inner.width.floor() as usize);
        assert!(
            encoder.line_commands.iter().all(|(start, end, width, color)| {
                *width == 1.0
                    && color_close(*color, color_with_alpha(theme.colors.foreground, 0.30))
                    && start.x >= inner.x
                    && start.x <= inner.x + inner.width
                    && end.x == start.x
                    && start.y >= inner.y
                    && end.y <= inner.y + inner.height
            })
        );
    }

    #[test]
    fn paint_draws_ruler_tracks_clips_and_playhead() {
        let mut view = timeline();
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 520.0, 180.0),
        };
        view.paint(&mut ctx);

        assert!(encoder.rects >= 6);
        assert!(encoder.lines >= 3);
        assert!(encoder.triangles >= 3);
        assert!(encoder.clips >= 3);
        assert!(encoder.texts.iter().any(|text| text == "V1"));
        assert!(encoder.texts.iter().any(|text| text == "Intro"));
        assert!(!encoder.texts.iter().any(|text| text == "视频轨道"));
        assert!(!encoder.texts.iter().any(|text| text == "音频轨道"));
        assert!(!encoder.texts.iter().any(|text| text == "静音"));
    }

    #[test]
    fn paint_skips_playhead_when_it_is_outside_visible_range() {
        let mut view = timeline().with_playhead(12).with_pixels_per_frame(8.0);
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));
        assert!(view.set_scroll_x(view.max_scroll_x()));

        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 520.0, 180.0),
        };
        view.paint(&mut ctx);

        assert!(!encoder.line_colors.contains(&theme.colors.timeline_playhead));
    }

    #[test]
    fn audio_clip_waveform_peaks_paint_as_clip_lines() {
        let mut without_waveform = TimelineView::new(vec![TimelineTrack::audio(
            "A1",
            vec![TimelineClip::new("Music", 0, 120)],
        )]);
        without_waveform.layout(Rect::new(0.0, 0.0, 520.0, 180.0));
        let mut with_waveform = TimelineView::new(vec![TimelineTrack::audio(
            "A1",
            vec![TimelineClip::new("Music", 0, 120)
                .with_waveform_peaks(vec![0.1, 0.7, 0.3, 1.0, 0.45, 0.8])],
        )]);
        with_waveform.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let theme = ThemePreset::Dark.build();
        let mut base_encoder = RecordingEncoder::default();
        let mut base_ctx = PaintContext {
            encoder: &mut base_encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 520.0, 180.0),
        };
        without_waveform.paint(&mut base_ctx);

        let mut waveform_encoder = RecordingEncoder::default();
        let mut waveform_ctx = PaintContext {
            encoder: &mut waveform_encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 520.0, 180.0),
        };
        with_waveform.paint(&mut waveform_ctx);

        assert!(waveform_encoder.lines > base_encoder.lines);
    }

    #[test]
    fn paint_draws_snap_guide_for_active_snap() {
        let mut view = timeline();
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));
        view.active_snap = Some(TimelineSnapResult {
            frame: 40,
            target: TimelineSnapTarget { frame: 40, kind: TimelineSnapKind::ClipStart },
            delta_frames: 1,
        });

        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 520.0, 180.0),
        };
        view.paint(&mut ctx);

        let mut guide_color = theme.colors.ring;
        guide_color.a = 0.72;
        assert!(encoder.line_colors.contains(&guide_color));
    }

    #[test]
    fn empty_timeline_paints_supplied_empty_message() {
        let mut view = TimelineView::new(Vec::new())
            .with_empty_message("未载入序列\n打开项目或创建序列以开始编辑")
            .disabled();
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));

        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 520.0, 180.0),
        };
        view.paint(&mut ctx);

        assert!(encoder.texts.iter().any(|text| text == "未载入序列"));
        assert!(encoder.texts.iter().any(|text| text == "打开项目或创建序列以开始编辑"));
        assert!(!encoder.line_colors.contains(&theme.colors.timeline_playhead));
    }

    #[test]
    fn adjacent_video_cut_exposes_a_domain_light_transition_action() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![
                TimelineClip::new("Left", 0, 10),
                TimelineClip::new("Right", 10, 10),
            ],
        )])
        .on_cut_transition_create(|_| Action::DeleteSelection);
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));
        let point = Point::new(
            view.frame_to_x(10),
            view.track_y(0) + view.track_height * 0.5,
        );

        let cut = view.hit_cut(point).expect("adjacent cut");
        assert_eq!(cut.left_clip_index, 0);
        assert_eq!(cut.right_clip_index, 1);
        let items = view.cut_context_menu_items(cut);
        assert_eq!(items[0].action(), Some(&Action::DeleteSelection));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );
        assert_eq!(
            view.event(
                &UiEvent::MouseDown {
                    position: point,
                    button: MouseButton::Right,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert!(view.selected_clip().is_none());
        assert_eq!(
            view.event(
                &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(actions.borrow().as_slice(), &[Action::DeleteSelection]);
    }

    #[test]
    fn cut_hit_testing_chooses_the_nearest_valid_edit_at_low_zoom() {
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![
                TimelineClip::new("One", 0, 10),
                TimelineClip::new("Two", 10, 10),
                TimelineClip::new("Three", 20, 10),
            ],
        )])
        .with_pixels_per_frame(0.5);
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));
        let point = Point::new(
            view.frame_to_x(18),
            view.track_y(0) + view.track_height * 0.5,
        );

        assert_eq!(
            view.hit_cut(point),
            Some(TimelineCutRef {
                track_index: 0,
                left_clip_index: 1,
                right_clip_index: 2,
            })
        );
    }

    #[test]
    fn transition_overlay_has_priority_over_its_endpoint_clips() {
        let transition = TimelineTransition::new("Cross Dissolve", 8, 4, 10, 0, 20).selected(true);
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![
                TimelineClip::new("Left", 0, 10),
                TimelineClip::new("Right", 10, 10),
            ],
        )
        .with_transitions(vec![transition])]);
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));
        let point = Point::new(
            view.frame_to_x(10),
            view.track_y(0) + view.track_height * 0.5,
        );

        assert_eq!(
            view.hit_transition(point),
            Some(TimelineTransitionRef { track_index: 0, transition_index: 0 })
        );
        assert_eq!(
            view.selected_transition(),
            Some(TimelineTransitionRef { track_index: 0, transition_index: 0 })
        );
        assert!(view.hit_cut(point).is_none());
    }

    #[test]
    fn transition_view_range_is_clamped_to_endpoint_geometry() {
        let transition = TimelineTransition::new("Cross Dissolve", 8, 100, 10, 0, 12);

        assert_eq!(transition.start_frame, 8);
        assert_eq!(transition.duration_frames, 4);
        assert_eq!(transition.maximum_end_frame, 12);
    }

    #[test]
    fn transition_handle_drag_previews_without_mutating_and_commits_once() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let transition = TimelineTransition::new("Cross Dissolve", 8, 4, 10, 0, 20);
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![
                TimelineClip::new("Left", 0, 10),
                TimelineClip::new("Right", 10, 10),
            ],
        )
        .with_transitions(vec![transition])])
        .on_transition_resize(|resize, _| Action::Custom {
            namespace: "timeline.transition".into(),
            name: format!(
                "{:?}:{}+{}->{}+{}",
                resize.edge,
                resize.old_start_frame,
                resize.old_duration_frames,
                resize.new_start_frame,
                resize.new_duration_frames
            ),
            payload: Default::default(),
        });
        view.layout(Rect::new(0.0, 0.0, 520.0, 180.0));
        let transition_ref = TimelineTransitionRef { track_index: 0, transition_index: 0 };
        let rect = view.transition_rect(transition_ref, &view.tracks[0].transitions[0]);

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );
        view.event(
            &UiEvent::MouseDown {
                position: Point::new(rect.x + 1.0, rect.center().y),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(view.transition_resize_drag.is_some());
        view.event(
            &UiEvent::MouseMove {
                position: Point::new(view.frame_to_x(6), rect.center().y),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(view.tracks[0].transitions[0].start_frame, 8);
        assert!(view.transition_resize_drag.is_some_and(|drag| {
            drag.current_start_frame == 6 && drag.current_duration_frames == 6
        }));
        assert!(actions.borrow().is_empty());

        view.event(
            &UiEvent::MouseUp {
                position: Point::new(view.frame_to_x(6), rect.center().y),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            actions.borrow().as_slice(),
            &[Action::Custom {
                namespace: "timeline.transition".into(),
                name: "In:8+4->6+6".into(),
                payload: Default::default(),
            }]
        );
    }
}
