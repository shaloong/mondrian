//! Timeline surface primitive.
//!
//! `TimelineView` is intentionally domain-light: it renders tracks, clips,
//! playhead, scrolling, zooming, and selection in frame space. Editor crates can
//! map real `Sequence` / `Track` / `Clip` data into these view models without
//! pulling timeline command logic into the widget layer.

use mondrian_core::types::{AssetId, Rational, TimeCode};
use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{CursorRequest, EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_theme::colors::ColorTokens;

use crate::paint::{centered_text_origin_y, color_with_alpha};
use crate::text_metrics::measure_single_line;
use crate::{ContextMenu, MenuItem, VectorIcon};

const SCROLLBAR_THICKNESS: f32 = 12.0;
const SCROLLBAR_MIN_THUMB: f32 = 28.0;
const SCROLLBAR_HANDLE_SIZE: f32 = 12.0;
const SCROLLBAR_HANDLE_VISUAL_SIZE: f32 = 8.0;
const SCROLLBAR_TRACK_VISUAL_THICKNESS: f32 = 6.0;
const SCROLLBAR_BODY_VISUAL_THICKNESS: f32 = 5.0;
const TIMELINE_SCROLLBAR_GUTTER: f32 = SCROLLBAR_THICKNESS;
const TIMELINE_TOOL_BUTTON_SIZE: f32 = 26.0;
const TIMELINE_TOOL_BUTTON_GAP: f32 = 4.0;
const TIMELINE_TOOLBAR_HEIGHT: f32 = 34.0;
const TIMELINE_TOOLBAR_GROUP_GAP: f32 = 10.0;
const TIMELINE_SNAP_THRESHOLD_PX: f32 = 8.0;
const TIMELINE_MIN_PIXELS_PER_FRAME: f32 = 0.25;
const TIMELINE_MAX_PIXELS_PER_FRAME: f32 = 64.0;
const TIMELINE_MIN_TRACK_HEIGHT: f32 = 30.0;
const TIMELINE_MAX_TRACK_HEIGHT: f32 = 96.0;
const TIMELINE_TRACK_HEADER_MIN_WIDTH: f32 = 132.0;

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
pub type TimelineSeekAction = dyn Fn(i64) -> Action;

/// Action factory for dropping an asset onto a timeline track.
pub type TimelineAssetDropAction = dyn Fn(TimelineAssetDrop, &TimelineTrack) -> Action;

/// Action factory for clip move commits.
pub type TimelineClipMoveAction = dyn Fn(TimelineClipMove, &TimelineClip) -> Action;

/// Action factory for clip trim commits.
pub type TimelineClipTrimAction = dyn Fn(TimelineClipTrim, &TimelineClip) -> Action;

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

/// Clip view model rendered by [`TimelineView`].
#[derive(Debug, Clone)]
pub struct TimelineClip {
    pub label: String,
    pub start_frame: i64,
    pub duration_frames: i64,
    pub color: Option<Color>,
    pub selected: bool,
    pub disabled: bool,
    pub nested: bool,
    pub waveform_peaks: Vec<f32>,
    pub select_action: Option<Action>,
}

impl TimelineClip {
    /// Create a clip view model in frame space.
    pub fn new(label: impl Into<String>, start_frame: i64, duration_frames: i64) -> Self {
        Self {
            label: label.into(),
            start_frame,
            duration_frames: duration_frames.max(1),
            color: None,
            selected: false,
            disabled: false,
            nested: false,
            waveform_peaks: Vec::new(),
            select_action: None,
        }
    }

    /// Set the clip tint.
    pub fn with_color(mut self, color: Color) -> Self {
        self.color = Some(color);
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

    /// Dispatch a static action when this clip is selected by input.
    pub fn with_select_action(mut self, action: Action) -> Self {
        self.select_action = Some(action);
        self
    }

    fn end_frame(&self) -> i64 {
        self.start_frame + self.duration_frames.max(1)
    }
}

/// Track view model rendered by [`TimelineView`].
#[derive(Debug, Clone)]
pub struct TimelineTrack {
    pub label: String,
    pub kind: TimelineTrackKind,
    pub clips: Vec<TimelineClip>,
    pub selected: bool,
    pub visible: bool,
    pub muted: bool,
    pub locked: bool,
    pub select_action: Option<Action>,
}

impl TimelineTrack {
    /// Create a video track.
    pub fn video(label: impl Into<String>, clips: Vec<TimelineClip>) -> Self {
        Self::new(label, TimelineTrackKind::Video, clips)
    }

    /// Create an audio track.
    pub fn audio(label: impl Into<String>, clips: Vec<TimelineClip>) -> Self {
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
            selected: false,
            visible: true,
            muted: false,
            locked: false,
            select_action: None,
        }
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
    hovered_clip: Option<TimelineClipRef>,
    hovered_track_control: Option<(TimelineTrackRef, TimelineTrackControl)>,
    hovered_toolbar_button: Option<TimelineToolbarButton>,
    active_tool: TimelineTool,
    playhead_frame: i64,
    in_point_frame: i64,
    out_point_frame: Option<i64>,
    frame_rate: Rational,
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
    on_in_out_point: Option<Box<TimelineInOutPointAction>>,
    toolbar_icons: Vec<(TimelineToolbarIconSlot, VectorIcon)>,
    track_control_icons: Vec<(TimelineTrackControlIconSlot, VectorIcon)>,
    empty_message: Option<String>,
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
            hovered_clip: None,
            hovered_track_control: None,
            hovered_toolbar_button: None,
            active_tool: TimelineTool::Select,
            playhead_frame: 0,
            in_point_frame: 0,
            out_point_frame: None,
            frame_rate: Rational::FPS_30,
            snapping_enabled: true,
            active_snap: None,
            pixels_per_frame: 4.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            focused: false,
            focus_visible: false,
            enabled: true,
            track_height: 42.0,
            header_width: TIMELINE_TRACK_HEADER_MIN_WIDTH,
            ruler_height: 28.0,
            playhead_dragging: false,
            in_out_drag: None,
            asset_drop_hover: None,
            track_drag: None,
            clip_drag: None,
            trim_drag: None,
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
            on_in_out_point: None,
            toolbar_icons: Vec::new(),
            track_control_icons: Vec::new(),
            empty_message: None,
        }
    }

    /// Set the current playhead frame.
    pub fn with_playhead(mut self, frame: i64) -> Self {
        self.playhead_frame = frame.max(0);
        self
    }

    /// Set optional sequence in/out points in frame space.
    pub fn with_in_out_points(mut self, in_point_frame: i64, out_point_frame: Option<i64>) -> Self {
        self.in_point_frame = in_point_frame.max(0);
        self.out_point_frame = out_point_frame.map(|frame| frame.max(self.in_point_frame));
        self
    }

    /// Set the frame rate used for ruler labels and SMPTE display.
    pub fn with_frame_rate(mut self, frame_rate: Rational) -> Self {
        self.frame_rate = if frame_rate.num <= 0 || frame_rate.den <= 0 {
            Rational::FPS_30
        } else {
            frame_rate
        };
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
        self.track_height =
            track_height.clamp(TIMELINE_MIN_TRACK_HEIGHT, TIMELINE_MAX_TRACK_HEIGHT);
        self
    }

    /// Set the track header width.
    pub fn with_header_width(mut self, width: f32) -> Self {
        self.header_width = width.max(TIMELINE_TRACK_HEADER_MIN_WIDTH);
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
    pub fn on_seek(mut self, action: impl Fn(i64) -> Action + 'static) -> Self {
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
        self.track_height =
            state.track_height.clamp(TIMELINE_MIN_TRACK_HEIGHT, TIMELINE_MAX_TRACK_HEIGHT);
        self.scroll_x = state.scroll_x.max(0.0);
        self.scroll_y = state.scroll_y.max(0.0);
        if self.body_rect.width > 0.0 || self.body_rect.height > 0.0 {
            self.clamp_scroll();
        }
    }

    fn content_width(&self) -> f32 {
        self.max_content_frame() as f32 * self.pixels_per_frame + 160.0
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
        (self.body_rect.width > SCROLLBAR_MIN_THUMB).then_some(Rect::new(
            self.body_rect.x + SCROLLBAR_HANDLE_SIZE * 0.5,
            self.body_rect.y + self.body_rect.height,
            (self.body_rect.width - SCROLLBAR_HANDLE_SIZE).max(0.0),
            SCROLLBAR_THICKNESS,
        ))
    }

    fn vertical_scrollbar_track_rect(&self) -> Option<Rect> {
        (self.body_rect.height > SCROLLBAR_MIN_THUMB).then_some(Rect::new(
            self.body_rect.x + self.body_rect.width,
            self.body_rect.y + SCROLLBAR_HANDLE_SIZE * 0.5,
            SCROLLBAR_THICKNESS,
            (self.body_rect.height - SCROLLBAR_HANDLE_SIZE).max(0.0),
        ))
    }

    fn scrollbar_clip_rect(&self) -> Rect {
        Rect::new(
            self.body_rect.x,
            self.body_rect.y,
            self.body_rect.width + TIMELINE_SCROLLBAR_GUTTER,
            self.body_rect.height + TIMELINE_SCROLLBAR_GUTTER,
        )
    }

    fn horizontal_scrollbar_thumb_rect(&self) -> Option<Rect> {
        let track = self.horizontal_scrollbar_track_rect()?;
        let content_width = self.content_width();
        let thumb_width = (self.body_rect.width / content_width * track.width)
            .max(SCROLLBAR_MIN_THUMB)
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
        Some(Rect::new(
            center_x - SCROLLBAR_HANDLE_SIZE * 0.5,
            thumb.center().y - SCROLLBAR_HANDLE_SIZE * 0.5,
            SCROLLBAR_HANDLE_SIZE,
            SCROLLBAR_HANDLE_SIZE,
        ))
    }

    fn horizontal_scrollbar_body_rect(&self) -> Option<Rect> {
        let thumb = self.horizontal_scrollbar_thumb_rect()?;
        Some(Rect::new(
            thumb.x,
            thumb.center().y - SCROLLBAR_BODY_VISUAL_THICKNESS * 0.5,
            thumb.width,
            SCROLLBAR_BODY_VISUAL_THICKNESS,
        ))
    }

    fn vertical_scrollbar_body_rect(&self) -> Option<Rect> {
        let thumb = self.vertical_scrollbar_thumb_rect()?;
        Some(Rect::new(
            thumb.center().x - SCROLLBAR_BODY_VISUAL_THICKNESS * 0.5,
            thumb.y,
            SCROLLBAR_BODY_VISUAL_THICKNESS,
            thumb.height,
        ))
    }

    fn vertical_scrollbar_thumb_rect(&self) -> Option<Rect> {
        let track = self.vertical_scrollbar_track_rect()?;
        let content_height = self.content_height();
        let thumb_height = (self.body_rect.height / content_height * track.height)
            .max(SCROLLBAR_MIN_THUMB)
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
        Some(Rect::new(
            thumb.center().x - SCROLLBAR_HANDLE_SIZE * 0.5,
            center_y - SCROLLBAR_HANDLE_SIZE * 0.5,
            SCROLLBAR_HANDLE_SIZE,
            SCROLLBAR_HANDLE_SIZE,
        ))
    }

    fn scroll_x_for_thumb_delta(&self, delta_x: f32, drag: TimelineScrollbarDrag) -> f32 {
        let Some(track) = self.horizontal_scrollbar_track_rect() else {
            return self.scroll_x;
        };
        let Some(thumb) = self.horizontal_scrollbar_thumb_rect() else {
            return self.scroll_x;
        };
        let travel = (track.width - thumb.width).max(1.0);
        drag.start_scroll + (delta_x / travel) * self.max_scroll_x()
    }

    fn scroll_y_for_thumb_delta(&self, delta_y: f32, drag: TimelineScrollbarDrag) -> f32 {
        let Some(track) = self.vertical_scrollbar_track_rect() else {
            return self.scroll_y;
        };
        let Some(thumb) = self.vertical_scrollbar_thumb_rect() else {
            return self.scroll_y;
        };
        let travel = (track.height - thumb.height).max(1.0);
        drag.start_scroll + (delta_y / travel) * self.max_scroll_y()
    }

    fn zoom_x_for_handle_delta(&mut self, delta_x: f32, drag: TimelineScrollbarDrag) -> bool {
        if self.body_rect.width <= 1.0 {
            return false;
        }
        let Some(track) = self.horizontal_scrollbar_track_rect() else {
            return false;
        };
        let start_pixels = drag
            .start_pixels_per_frame
            .clamp(TIMELINE_MIN_PIXELS_PER_FRAME, TIMELINE_MAX_PIXELS_PER_FRAME);
        let content_frames = self.max_content_frame() as f32;
        let content_padding = 160.0;
        let total_frames = (content_frames + content_padding / start_pixels).max(1.0);
        let start_left = (drag.start_scroll / start_pixels).clamp(0.0, total_frames);
        let start_visible = (self.body_rect.width / start_pixels).max(1.0);
        let start_right = (start_left + start_visible).clamp(start_left, total_frames);
        let delta_frames = delta_x / track.width.max(1.0) * total_frames;
        let min_visible_frames = self.body_rect.width / TIMELINE_MAX_PIXELS_PER_FRAME;
        let max_visible_frames = self.body_rect.width / TIMELINE_MIN_PIXELS_PER_FRAME;
        let (left_frame, proposed_visible_frames) = match drag.kind {
            TimelineScrollbarDragKind::LeadingHandle => {
                let left = (start_left + delta_frames)
                    .clamp(0.0, start_right - min_visible_frames.max(1.0));
                (left, start_right - left)
            }
            TimelineScrollbarDragKind::TrailingHandle => {
                let right = (start_right + delta_frames)
                    .clamp(start_left + min_visible_frames.max(1.0), total_frames);
                (start_left, right - start_left)
            }
            TimelineScrollbarDragKind::Thumb => return false,
        };
        let visible_frames =
            proposed_visible_frames.clamp(min_visible_frames.max(1.0), max_visible_frames.max(1.0));
        let old_pixels = self.pixels_per_frame;
        let old_scroll = self.scroll_x;
        self.pixels_per_frame = (self.body_rect.width / visible_frames)
            .clamp(TIMELINE_MIN_PIXELS_PER_FRAME, TIMELINE_MAX_PIXELS_PER_FRAME);
        self.scroll_x = left_frame.max(0.0) * self.pixels_per_frame;
        self.clamp_scroll();
        (self.pixels_per_frame - old_pixels).abs() > 0.001
            || (self.scroll_x - old_scroll).abs() > 0.01
    }

    fn resize_tracks_for_handle_delta(
        &mut self,
        delta_y: f32,
        drag: TimelineScrollbarDrag,
    ) -> bool {
        if self.body_rect.height <= 1.0 {
            return false;
        }
        let Some(track) = self.vertical_scrollbar_track_rect() else {
            return false;
        };
        let start_height = drag
            .start_track_height
            .clamp(TIMELINE_MIN_TRACK_HEIGHT, TIMELINE_MAX_TRACK_HEIGHT);
        let total_rows = (self.tracks.len() as f32).max(1.0);
        let start_top = (drag.start_scroll / start_height).clamp(0.0, total_rows);
        let start_visible = (self.body_rect.height / start_height).max(1.0);
        let start_bottom = (start_top + start_visible).clamp(start_top, total_rows);
        let delta_rows = delta_y / track.height.max(1.0) * total_rows;
        let min_visible_rows = self.body_rect.height / TIMELINE_MAX_TRACK_HEIGHT;
        let max_visible_rows = self.body_rect.height / TIMELINE_MIN_TRACK_HEIGHT;
        let (top_row, proposed_visible_rows) = match drag.kind {
            TimelineScrollbarDragKind::LeadingHandle => {
                let top =
                    (start_top + delta_rows).clamp(0.0, start_bottom - min_visible_rows.max(1.0));
                (top, start_bottom - top)
            }
            TimelineScrollbarDragKind::TrailingHandle => {
                let bottom = (start_bottom + delta_rows)
                    .clamp(start_top + min_visible_rows.max(1.0), total_rows);
                (start_top, bottom - start_top)
            }
            TimelineScrollbarDragKind::Thumb => return false,
        };
        let visible_rows =
            proposed_visible_rows.clamp(min_visible_rows.max(1.0), max_visible_rows.max(1.0));
        let old_height = self.track_height;
        let old_scroll = self.scroll_y;
        self.track_height = (self.body_rect.height / visible_rows)
            .clamp(TIMELINE_MIN_TRACK_HEIGHT, TIMELINE_MAX_TRACK_HEIGHT);
        self.scroll_y = top_row.max(0.0) * self.track_height;
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
        self.body_rect.x + frame.max(0) as f32 * self.pixels_per_frame - self.scroll_x
    }

    fn x_to_frame(&self, x: f32) -> i64 {
        ((x - self.body_rect.x + self.scroll_x) / self.pixels_per_frame)
            .round()
            .max(0.0) as i64
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
        self.body_rect.y + track_index as f32 * self.track_height - self.scroll_y
    }

    fn track_index_at(&self, point: Point) -> Option<usize> {
        if !self.body_rect.contains(point) {
            return None;
        }
        self.track_index_from_y(point.y)
    }

    fn track_index_from_y(&self, y: f32) -> Option<usize> {
        let index = ((y - self.body_rect.y + self.scroll_y) / self.track_height).floor();
        (index >= 0.0)
            .then_some(index as usize)
            .filter(|index| *index < self.tracks.len())
    }

    fn track_header_at(&self, point: Point) -> Option<TimelineTrackRef> {
        if !self.header_rect.contains(point) {
            return None;
        }
        let index = ((point.y - self.body_rect.y + self.scroll_y) / self.track_height).floor();
        (index >= 0.0)
            .then_some(TimelineTrackRef { track_index: index as usize })
            .filter(|track_ref| track_ref.track_index < self.tracks.len())
    }

    fn track_control_rect(&self, header: Rect, control: TimelineTrackControl) -> Rect {
        let size = 18.0;
        let gap = 8.0;
        let right_padding = 8.0;
        let group_width = size * 3.0 + gap * 2.0;
        let start_x = header.x + header.width - right_padding - group_width;
        let y = header.y + (header.height - size) * 0.5;
        let index = match control {
            TimelineTrackControl::Visibility => 0.0,
            TimelineTrackControl::Mute => 1.0,
            TimelineTrackControl::Lock => 2.0,
        };
        Rect::new(start_x + index * (size + gap), y, size, size)
    }

    fn track_control_at(&self, point: Point) -> Option<(TimelineTrackRef, TimelineTrackControl)> {
        let track_ref = self.track_header_at(point)?;
        let header = Rect::new(
            self.header_rect.x,
            self.track_y(track_ref.track_index),
            self.header_rect.width,
            self.track_height,
        );
        [
            TimelineTrackControl::Visibility,
            TimelineTrackControl::Mute,
            TimelineTrackControl::Lock,
        ]
        .into_iter()
        .find(|control| self.track_control_rect(header, *control).contains(point))
        .map(|control| (track_ref, control))
    }

    fn in_out_marker_at(&self, point: Point) -> Option<TimelineInOutPoint> {
        if !self.ruler_rect.contains(point) {
            return None;
        }
        let hit_radius = 5.0;
        if (point.x - self.frame_to_x(self.in_point_frame)).abs() <= hit_radius {
            return Some(TimelineInOutPoint::In);
        }
        let out = self.out_point_frame?;
        if (point.x - self.frame_to_x(out.saturating_add(1))).abs() <= hit_radius {
            return Some(TimelineInOutPoint::Out);
        }
        None
    }

    fn timeline_corner_rect(&self) -> Rect {
        Rect::new(
            self.bounds.x,
            self.bounds.y + TIMELINE_TOOLBAR_HEIGHT,
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
        let size = TIMELINE_TOOL_BUTTON_SIZE;
        let y = self.toolbar_rect.y + (self.toolbar_rect.height - size) * 0.5;
        let limit = self.toolbar_left_limit();
        let mut x = self.toolbar_rect.x + 8.0;
        for (index, candidate) in Self::toolbar_left_buttons().into_iter().enumerate() {
            if index == 3 {
                x += TIMELINE_TOOLBAR_GROUP_GAP;
            }
            let rect = Rect::new(x, y, size, size);
            if candidate == button {
                return (rect.x + rect.width <= limit).then_some(rect);
            }
            x += size + TIMELINE_TOOL_BUTTON_GAP;
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

    fn update_clip_hover(&mut self, position: Point, ctx: &mut EventContext) -> bool {
        let hovered = self.hit_clip(position);
        if hovered == self.hovered_clip {
            return false;
        }

        let had_tooltip = self
            .hovered_clip
            .and_then(|clip_ref| self.clip_label_tooltip(clip_ref))
            .is_some();
        self.hovered_clip = hovered;
        if let Some((text, rect)) =
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
        let x = self.frame_to_x(start_frame);
        let y = self.track_y(track_index) + 4.0;
        let width = (clip.duration_frames.max(1) as f32 * self.pixels_per_frame).max(8.0);
        Rect::new(x, y, width, self.track_height - 8.0)
    }

    fn clip_rect_for_preview(
        &self,
        track_index: usize,
        start_frame: i64,
        duration_frames: i64,
    ) -> Rect {
        let x = self.frame_to_x(start_frame);
        let y = self.track_y(track_index) + 4.0;
        let width = (duration_frames.max(1) as f32 * self.pixels_per_frame).max(8.0);
        Rect::new(x, y, width, self.track_height - 8.0)
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
        if !rect.contains(point) {
            return None;
        }
        let edge_width = 6.0_f32.min((rect.width * 0.35).max(3.0));
        if point.x <= rect.x + edge_width {
            Some(TimelineTrimEdge::In)
        } else if point.x >= rect.x + rect.width - edge_width {
            Some(TimelineTrimEdge::Out)
        } else {
            None
        }
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

    fn compatible_track_move_target(
        &self,
        source_track_index: usize,
        target_track_index: usize,
    ) -> usize {
        let Some(source) = self.tracks.get(source_track_index) else {
            return source_track_index;
        };
        let Some(target) = self.tracks.get(target_track_index) else {
            return source_track_index;
        };
        if source.kind == target.kind {
            target_track_index
        } else {
            source_track_index
        }
    }

    fn select_clip_from_input(
        &mut self,
        clip_ref: TimelineClipRef,
        ctx: &mut EventContext,
    ) -> EventResult {
        self.selected_track = None;
        self.selected_clip = Some(clip_ref);
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

    fn select_track_from_input(
        &mut self,
        track_ref: TimelineTrackRef,
        ctx: &mut EventContext,
    ) -> EventResult {
        self.selected_track = Some(track_ref);
        self.selected_clip = None;
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
        let Some(target_index) = self.track_index_from_y(position.y) else {
            return true;
        };
        let target_index =
            self.compatible_track_move_target(drag.track_ref.track_index, target_index);
        if drag.current_track_index == target_index {
            return true;
        }
        drag.current_track_index = target_index;
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
        let proposed_start_frame =
            (self.x_to_frame(position.x) - drag.pointer_offset_frames).max(0);
        let snap = self.snap_clip_start(drag.clip_ref, proposed_start_frame, clip_duration);
        let new_start_frame = snap.map_or(proposed_start_frame, |snap| snap.frame.max(0));
        let target_track_index = self
            .track_index_at(position)
            .map(|index| self.compatible_drag_track(drag.clip_ref.track_index, index))
            .unwrap_or(drag.current_track_index);
        self.set_active_snap(snap, ctx);
        if drag.current_start_frame == new_start_frame
            && drag.current_track_index == target_track_index
        {
            return true;
        }
        drag.current_start_frame = new_start_frame;
        drag.current_track_index = target_track_index;
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
        let pointer_frame = snap.map_or(pointer_frame, |snap| snap.frame);
        let old_end = drag.old_start_frame + drag.old_duration_frames.max(1);
        let (new_start, new_duration) = match drag.edge {
            TimelineTrimEdge::In => {
                let new_start = pointer_frame.clamp(0, old_end - 1);
                (new_start, old_end - new_start)
            }
            TimelineTrimEdge::Out => {
                let new_end = pointer_frame.max(drag.old_start_frame + 1);
                (drag.old_start_frame, new_end - drag.old_start_frame)
            }
        };
        self.set_active_snap(snap, ctx);
        if drag.current_start_frame == new_start && drag.current_duration_frames == new_duration {
            return true;
        }
        drag.current_start_frame = new_start;
        drag.current_duration_frames = new_duration;
        drag.moved = true;
        self.trim_drag = Some(drag);
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
        let frame = self.x_to_frame(position.x).max(0);
        let frame = match drag.point {
            TimelineInOutPoint::In => frame,
            TimelineInOutPoint::Out => frame.max(self.in_point_frame),
        };
        if drag.current_frame == frame {
            return true;
        }
        drag.current_frame = frame;
        drag.moved = true;
        match drag.point {
            TimelineInOutPoint::In => {
                self.in_point_frame = frame;
                if self.out_point_frame.is_some_and(|out| out < frame) {
                    self.out_point_frame = Some(frame);
                }
            }
            TimelineInOutPoint::Out => {
                self.out_point_frame = Some(frame);
            }
        }
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

    fn seek_from_input(&mut self, frame: i64, ctx: &mut EventContext) {
        let frame = frame.max(0);
        if self.playhead_frame != frame {
            self.playhead_frame = frame;
            if let Some(factory) = &self.on_seek {
                (ctx.dispatch)(factory(frame));
            }
            ctx.request_repaint();
        }
    }

    fn seek_from_drag_input(&mut self, frame: i64, ctx: &mut EventContext) {
        let proposed = frame.max(0);
        let snap = self.snap_frame(proposed, None, false);
        self.set_active_snap(snap, ctx);
        self.seek_from_input(snap.map_or(proposed, |snap| snap.frame), ctx);
    }

    fn asset_drop_target_at(&self, position: Point) -> Option<(usize, i64)> {
        let track_index = self.track_index_at(position)?;
        Some((track_index, self.x_to_frame(position.x).max(0)))
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
        match command {
            TimelineEditCommand::PasteAtPlayhead
            | TimelineEditCommand::MarkInAtPlayhead
            | TimelineEditCommand::MarkOutAtPlayhead
            | TimelineEditCommand::TogglePlayback => true,
            TimelineEditCommand::ClearInOutPoints => {
                self.in_point_frame > 0 || self.out_point_frame.is_some()
            }
            TimelineEditCommand::SplitAtPlayhead => self.clip_intersects_playhead(),
            TimelineEditCommand::DeleteSelection | TimelineEditCommand::RippleDeleteSelection => {
                self.selected_clip.is_some() || self.selected_track.is_some()
            }
            TimelineEditCommand::CutSelection
            | TimelineEditCommand::CopySelection
            | TimelineEditCommand::DuplicateSelection
            | TimelineEditCommand::TrimSelectionInToPlayhead
            | TimelineEditCommand::EnableSelection
            | TimelineEditCommand::DisableSelection => self.selected_clip.is_some(),
            TimelineEditCommand::TrimSelectionOutToPlayhead
            | TimelineEditCommand::RollSelectedCutToPlayhead => self.selected_clip.is_some(),
            TimelineEditCommand::OpenNestedSequence(clip_ref) => {
                self.selected_clip == Some(clip_ref)
                    && self.clip(clip_ref).is_some_and(|clip| clip.nested)
            }
        }
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

    fn timeline_context_menu_items(&self) -> Vec<MenuItem> {
        vec![
            Self::menu_item(
                "添加视频轨道",
                self.track_add_action(TimelineTrackKind::Video),
            ),
            Self::menu_item(
                "添加音频轨道",
                self.track_add_action(TimelineTrackKind::Audio),
            ),
            MenuItem::separator(),
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
        ]
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
        self.seek_from_input(self.x_to_frame(point.x), ctx);
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
        if modifiers.ctrl || modifiers.meta || modifiers.alt {
            return false;
        }
        let step = if modifiers.shift { 10 } else { 1 };
        let target = match key {
            KeyCode::Left => self.playhead_frame.saturating_sub(step),
            KeyCode::Right => self.playhead_frame.saturating_add(step),
            KeyCode::Home => 0,
            KeyCode::End => self.max_content_frame(),
            _ => return false,
        };
        self.seek_from_input(target, ctx);
        true
    }

    fn keyboard_edit_command(
        &mut self,
        key: KeyCode,
        modifiers: Modifiers,
        ctx: &mut EventContext,
    ) -> bool {
        if modifiers.alt {
            return false;
        }
        let command = match key {
            KeyCode::X if !modifiers.shift && (modifiers.ctrl || modifiers.meta) => {
                TimelineEditCommand::CutSelection
            }
            KeyCode::C if !modifiers.shift && (modifiers.ctrl || modifiers.meta) => {
                TimelineEditCommand::CopySelection
            }
            KeyCode::V if !modifiers.shift && (modifiers.ctrl || modifiers.meta) => {
                TimelineEditCommand::PasteAtPlayhead
            }
            KeyCode::D if !modifiers.shift && (modifiers.ctrl || modifiers.meta) => {
                TimelineEditCommand::DuplicateSelection
            }
            KeyCode::K if !modifiers.shift && (modifiers.ctrl || modifiers.meta) => {
                TimelineEditCommand::SplitAtPlayhead
            }
            KeyCode::B if !modifiers.shift && (modifiers.ctrl || modifiers.meta) => {
                TimelineEditCommand::SplitAtPlayhead
            }
            KeyCode::I if !modifiers.ctrl && !modifiers.meta && !modifiers.shift => {
                TimelineEditCommand::MarkInAtPlayhead
            }
            KeyCode::O if !modifiers.ctrl && !modifiers.meta && !modifiers.shift => {
                TimelineEditCommand::MarkOutAtPlayhead
            }
            KeyCode::Space if !modifiers.ctrl && !modifiers.meta && !modifiers.shift => {
                TimelineEditCommand::TogglePlayback
            }
            KeyCode::Delete | KeyCode::Backspace if !modifiers.ctrl && !modifiers.meta => {
                if modifiers.shift {
                    TimelineEditCommand::RippleDeleteSelection
                } else {
                    TimelineEditCommand::DeleteSelection
                }
            }
            _ => return false,
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

    fn pick_ruler_step_frames(&self, target_px: f32, min_step: i64) -> i64 {
        let raw = (target_px / self.pixels_per_frame).max(min_step.max(1) as f32);
        let fps = self.ruler_fps().max(1);
        for step in [
            1,
            2,
            5,
            10,
            15,
            fps,
            fps * 2,
            fps * 5,
            fps * 10,
            fps * 15,
            fps * 30,
            fps * 60,
            fps * 120,
            fps * 240,
            fps * 300,
            fps * 600,
            fps * 900,
            fps * 1800,
            fps * 3600,
        ] {
            if step < min_step || step % min_step != 0 {
                continue;
            }
            if raw <= step as f32 {
                return step;
            }
        }
        let fallback = fps * 3600;
        if fallback >= min_step && fallback % min_step == 0 {
            fallback
        } else {
            min_step.max(1)
        }
    }

    fn tick_step_frames(&self) -> i64 {
        self.pick_ruler_step_frames(20.0, 1)
    }

    fn major_tick_step_frames(&self, minor_step: i64) -> i64 {
        self.pick_ruler_step_frames(96.0, minor_step.max(1))
    }

    fn ruler_label_for_frame(&self, frame: i64, major_step: i64) -> String {
        let frame = frame.max(0);
        let fps = self.ruler_fps().max(1);
        let smpte = TimeCode::new(frame, self.frame_time_base()).to_smpte();
        let total_seconds = (frame as f64 / self.frame_rate.to_f64()).floor().max(0.0) as i64;
        let hours = total_seconds / 3600;
        let parts = smpte.split(':').collect::<Vec<_>>();

        if major_step < fps * 2 {
            smpte
        } else if hours > 0 || major_step >= fps * 60 * 10 {
            format!("{}:{}:{}", parts[0], parts[1], parts[2])
        } else {
            format!("{}:{}", parts[1], parts[2])
        }
    }

    fn ruler_fps(&self) -> i64 {
        self.frame_rate.to_f64().round().max(1.0) as i64
    }

    fn frame_time_base(&self) -> Rational {
        Rational::new(self.frame_rate.den, self.frame_rate.num)
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
            self.paint_track_control(
                ctx,
                header,
                track_ref,
                track,
                TimelineTrackControl::Visibility,
            );
            self.paint_track_control(ctx, header, track_ref, track, TimelineTrackControl::Mute);
            self.paint_track_control(ctx, header, track_ref, track, TimelineTrackControl::Lock);

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
        let out = self.out_point_frame?;
        if out < self.in_point_frame {
            return None;
        }
        let start = self.frame_to_x(self.in_point_frame);
        let end = self.frame_to_x(out.saturating_add(1));
        let x0 = start.max(self.body_rect.x);
        let x1 = end.min(self.body_rect.x + self.body_rect.width);
        (x1 > x0).then_some((x0, x1))
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
        let in_x = self.frame_to_x(self.in_point_frame);
        if in_x >= rect.x && in_x <= rect.x + rect.width {
            ctx.encoder.draw_line(
                Point::new(in_x, rect.y),
                Point::new(in_x, rect.y + rect.height),
                1.0,
                color,
            );
        }
        if let Some(out) = self.out_point_frame {
            let out_x = self.frame_to_x(out.saturating_add(1));
            if out_x >= rect.x && out_x <= rect.x + rect.width {
                ctx.encoder.draw_line(
                    Point::new(out_x, rect.y),
                    Point::new(out_x, rect.y + rect.height),
                    1.0,
                    color,
                );
            }
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
        let rect = self.track_control_rect(header, control);
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
            Color::from_hex(0xDDEEFF),
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

    fn paint_clip(
        &self,
        ctx: &mut PaintContext,
        clip_ref: TimelineClipRef,
        clip: &TimelineClip,
        rect: Rect,
        dragging: bool,
    ) {
        let colors = &ctx.theme.colors;
        let track_kind = self
            .tracks
            .get(clip_ref.track_index)
            .map(|track| track.kind)
            .unwrap_or(TimelineTrackKind::Video);
        let selected = clip.selected || self.selected_clip == Some(clip_ref) || dragging;
        let hovered = self.hovered_clip == Some(clip_ref);
        let base_fill = clip.color.unwrap_or(match track_kind {
            TimelineTrackKind::Video => colors.timeline_clip_video,
            TimelineTrackKind::Audio => colors.timeline_clip_audio,
        });
        let hover_fill = match (clip.color, track_kind) {
            (None, TimelineTrackKind::Video) => colors.timeline_clip_video_hover,
            (None, TimelineTrackKind::Audio) => colors.timeline_clip_audio_hover,
            (Some(color), _) => color.lerp(colors.foreground, 0.10),
        };
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
            let mut ring = match track_kind {
                TimelineTrackKind::Video => colors.timeline_clip_selected_border,
                TimelineTrackKind::Audio => colors.timeline_clip_audio_selected_border,
            };
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
        let accent_height = rect.height.clamp(2.0, 4.0);
        ctx.encoder.draw_rect(
            Rect::new(
                rect.x + 1.0,
                rect.y + 1.0,
                (rect.width - 2.0).max(1.0),
                accent_height,
            ),
            color_with_alpha(colors.foreground, 0.08),
            4.0,
        );
        ctx.encoder.draw_line(
            Point::new(rect.x + 1.0, rect.y + rect.height - 1.0),
            Point::new(rect.x + rect.width - 1.0, rect.y + rect.height - 1.0),
            1.0,
            color_with_alpha(Color::BLACK, 0.10),
        );
        if track_kind == TimelineTrackKind::Audio && !clip.waveform_peaks.is_empty() {
            self.paint_audio_waveform(ctx, rect, clip, selected || hovered || dragging);
        }
        if selected || hovered {
            let handle = color_with_alpha(colors.foreground, if hovered { 0.35 } else { 0.22 });
            ctx.encoder.draw_rect(
                Rect::new(
                    rect.x + 2.0,
                    rect.y + 4.0,
                    2.0,
                    (rect.height - 8.0).max(4.0),
                ),
                handle,
                1.0,
            );
            ctx.encoder.draw_rect(
                Rect::new(
                    rect.x + rect.width - 4.0,
                    rect.y + 4.0,
                    2.0,
                    (rect.height - 8.0).max(4.0),
                ),
                handle,
                1.0,
            );
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

    fn paint_audio_waveform(
        &self,
        ctx: &mut PaintContext,
        rect: Rect,
        clip: &TimelineClip,
        emphasized: bool,
    ) {
        let inner = rect.inset(6.0, (rect.height * 0.24).min(10.0));
        if inner.width <= 2.0 || inner.height <= 4.0 {
            return;
        }
        let mut color = ctx.theme.colors.foreground;
        color.a = if emphasized { 0.46 } else { 0.30 };
        let mid_y = inner.y + inner.height * 0.5;
        let column_count = inner.width.floor().max(1.0) as usize;
        for column in 0..column_count {
            let sample_index =
                ((column as f32 / column_count as f32) * clip.waveform_peaks.len() as f32)
                    .floor()
                    .min((clip.waveform_peaks.len() - 1) as f32) as usize;
            let peak = clip.waveform_peaks[sample_index].clamp(0.0, 1.0);
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
        let track_color = colors.timeline_navigator_track;
        let corner = Rect::new(
            self.body_rect.x + self.body_rect.width,
            self.body_rect.y + self.body_rect.height,
            TIMELINE_SCROLLBAR_GUTTER,
            TIMELINE_SCROLLBAR_GUTTER,
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
                track.center().y - SCROLLBAR_TRACK_VISUAL_THICKNESS * 0.5,
                track.width,
                SCROLLBAR_TRACK_VISUAL_THICKNESS,
            );
            ctx.encoder.draw_rect(
                track_visual,
                track_color,
                SCROLLBAR_TRACK_VISUAL_THICKNESS * 0.5,
            );

            if let Some(body) =
                self.horizontal_scrollbar_body_rect().filter(|body| body.width > 0.0)
            {
                ctx.encoder.draw_rect(
                    body,
                    navigator_body_color(colors, body_hovered, body_dragging),
                    SCROLLBAR_BODY_VISUAL_THICKNESS * 0.5,
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
                track.center().x - SCROLLBAR_TRACK_VISUAL_THICKNESS * 0.5,
                track.y,
                SCROLLBAR_TRACK_VISUAL_THICKNESS,
                track.height,
            );
            ctx.encoder.draw_rect(
                track_visual,
                track_color,
                SCROLLBAR_TRACK_VISUAL_THICKNESS * 0.5,
            );

            if let Some(body) = self.vertical_scrollbar_body_rect().filter(|body| body.height > 0.0)
            {
                ctx.encoder.draw_rect(
                    body,
                    navigator_body_color(colors, body_hovered, body_dragging),
                    SCROLLBAR_BODY_VISUAL_THICKNESS * 0.5,
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
        let visual = Rect::new(
            center.x - SCROLLBAR_HANDLE_VISUAL_SIZE * 0.5,
            center.y - SCROLLBAR_HANDLE_VISUAL_SIZE * 0.5,
            SCROLLBAR_HANDLE_VISUAL_SIZE,
            SCROLLBAR_HANDLE_VISUAL_SIZE,
        );
        ctx.encoder.draw_rect(
            visual.inset(-1.0, -1.0),
            ctx.theme.colors.timeline_navigator_handle_border,
            (SCROLLBAR_HANDLE_VISUAL_SIZE + 2.0) * 0.5,
        );
        ctx.encoder.draw_rect(
            visual,
            navigator_handle_color(&ctx.theme.colors, hovered, dragging),
            SCROLLBAR_HANDLE_VISUAL_SIZE * 0.5,
        );
    }
}

fn navigator_body_color(colors: &ColorTokens, hovered: bool, active: bool) -> Color {
    if active {
        colors.timeline_navigator_body_active
    } else if hovered {
        colors.timeline_navigator_body_hover
    } else {
        colors.timeline_navigator_body
    }
}

fn navigator_handle_color(colors: &ColorTokens, hovered: bool, active: bool) -> Color {
    if active {
        colors.timeline_navigator_handle_active
    } else if hovered {
        colors.timeline_navigator_handle_hover
    } else {
        colors.timeline_navigator_handle
    }
}

impl Widget for TimelineView {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(
            560.0,
            TIMELINE_TOOLBAR_HEIGHT + self.ruler_height + self.content_height(),
        ))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        let top_chrome = TIMELINE_TOOLBAR_HEIGHT + self.ruler_height;
        let available_content_width = (bounds.width - self.header_width).max(0.0);
        let available_content_height = (bounds.height - top_chrome).max(0.0);
        let vertical_gutter = TIMELINE_SCROLLBAR_GUTTER.min(available_content_width);
        let horizontal_gutter = TIMELINE_SCROLLBAR_GUTTER.min(available_content_height);
        let viewport_width = (available_content_width - vertical_gutter).max(0.0);
        let viewport_height = (available_content_height - horizontal_gutter).max(0.0);
        self.toolbar_rect = Rect::new(bounds.x, bounds.y, bounds.width, TIMELINE_TOOLBAR_HEIGHT);
        self.header_rect = Rect::new(
            bounds.x,
            bounds.y + top_chrome,
            self.header_width,
            viewport_height,
        );
        self.ruler_rect = Rect::new(
            bounds.x + self.header_width,
            bounds.y + TIMELINE_TOOLBAR_HEIGHT,
            viewport_width,
            self.ruler_height,
        );
        self.body_rect = Rect::new(
            bounds.x + self.header_width,
            bounds.y + top_chrome,
            viewport_width,
            viewport_height,
        );
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
            self.scrollbar_drag = None;
            self.hovered_clip = None;
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
                self.scrollbar_drag = None;
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
                        ctx.set_cursor(CursorRequest::EwResize);
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
                    self.seek_from_input(self.x_to_frame(position.x), ctx);
                    return EventResult::Handled;
                }
            }
            UiEvent::MouseMove { position, .. } => {
                let active_drag = self.playhead_dragging
                    || self.in_out_drag.is_some()
                    || self.track_drag.is_some()
                    || self.clip_drag.is_some()
                    || self.trim_drag.is_some()
                    || self.scrollbar_drag.is_some();
                if !self.bounds.contains(*position) && !active_drag {
                    if self.chrome_tooltip().is_some() {
                        ctx.tooltip.hide();
                    }
                    self.hovered_toolbar_button = None;
                    self.hovered_track_control = None;
                    self.hovered_clip = None;
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
                if let Some(drag) = self.scrollbar_drag {
                    match (drag.axis, drag.kind) {
                        (TimelineScrollbarAxis::Horizontal, TimelineScrollbarDragKind::Thumb) => {
                            ctx.set_cursor(CursorRequest::Grabbing);
                        }
                        (TimelineScrollbarAxis::Horizontal, _) => {
                            ctx.set_cursor(CursorRequest::EwResize);
                        }
                        (TimelineScrollbarAxis::Vertical, TimelineScrollbarDragKind::Thumb) => {
                            ctx.set_cursor(CursorRequest::Grabbing);
                        }
                        (TimelineScrollbarAxis::Vertical, _) => {
                            ctx.set_cursor(CursorRequest::NsResize);
                        }
                    }
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
                if let Some(kind) =
                    self.scrollbar_hover_kind(TimelineScrollbarAxis::Horizontal, *position)
                {
                    match kind {
                        TimelineScrollbarDragKind::Thumb => {
                            ctx.set_cursor(CursorRequest::Grab);
                        }
                        TimelineScrollbarDragKind::LeadingHandle
                        | TimelineScrollbarDragKind::TrailingHandle => {
                            ctx.set_cursor(CursorRequest::EwResize);
                        }
                    }
                } else if let Some(kind) =
                    self.scrollbar_hover_kind(TimelineScrollbarAxis::Vertical, *position)
                {
                    match kind {
                        TimelineScrollbarDragKind::Thumb => {
                            ctx.set_cursor(CursorRequest::Grab);
                        }
                        TimelineScrollbarDragKind::LeadingHandle
                        | TimelineScrollbarDragKind::TrailingHandle => {
                            ctx.set_cursor(CursorRequest::NsResize);
                        }
                    }
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
                if self.update_clip_hover(*position, ctx) {
                    return EventResult::Handled;
                }
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.playhead_dragging => {
                self.playhead_dragging = false;
                self.active_snap = None;
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
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.scrollbar_drag.is_some() => {
                self.scrollbar_drag = None;
                self.release_timeline_pointer_capture(ctx);
                ctx.request_repaint();
                return EventResult::Handled;
            }
            UiEvent::FocusGained => {
                self.focused = true;
                self.focus_visible = true;
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

    use mondrian_platform::NoopPlatformService;
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

    fn old_timeline_point(x: f32, y: f32) -> Point {
        Point::new(x, y + TIMELINE_TOOLBAR_HEIGHT)
    }

    fn timeline_content_point(x: f32, y: f32) -> Point {
        const LEGACY_TIMELINE_HEADER_WIDTH: f32 = 96.0;
        old_timeline_point(
            x + TIMELINE_TRACK_HEADER_MIN_WIDTH - LEGACY_TIMELINE_HEADER_WIDTH,
            y,
        )
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
        lines: usize,
        line_colors: Vec<Color>,
        triangles: usize,
        texts: Vec<String>,
        clips: usize,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, _bounds: Rect) {
            self.clips += 1;
        }

        fn pop_clip(&mut self) {}

        fn draw_rect(&mut self, _bounds: Rect, _color: Color, _corner_radius: f32) {
            self.rects += 1;
        }

        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, color: Color) {
            self.lines += 1;
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
            _position: Point,
            _max_width: f32,
            _color: Color,
        ) {
            self.texts.push(text.into());
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}
        fn pop_transform(&mut self) {}
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
                    if control == TimelineTrackControl::Mute && track_ref.track_index == 0 {
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

        let result = view.event(
            &UiEvent::MouseDown {
                position: old_timeline_point(88.0, 55.0),
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
    fn ruler_drag_seeks_and_uses_pointer_capture() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline().on_seek(|frame| {
            if frame == 24 {
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
    }

    #[test]
    fn ruler_drag_snaps_playhead_to_clip_edge() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline().on_seek(|frame| Action::Custom {
            namespace: "timeline.seek".into(),
            name: frame.to_string(),
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
            view.event(&UiEvent::FocusGained, &mut ctx),
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
        assert_eq!(ctx.requests.cursor, Some(CursorRequest::EwResize));
        assert!(ctx.requests.repaint);
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
        assert_eq!(ctx.requests.cursor, Some(CursorRequest::Grabbing));
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
        let timeline_labels: Vec<&str> = timeline_menu_items
            .iter()
            .filter(|item| !item.is_separator())
            .map(|item| item.label.as_str())
            .collect();
        for label in ["添加视频轨道", "添加音频轨道", "在播放头处分割"] {
            assert!(
                timeline_labels.contains(&label),
                "{label} should remain available from the timeline context menu"
            );
        }
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

        view.event(&UiEvent::FocusGained, &mut ctx);
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

        view.event(&UiEvent::FocusGained, &mut ctx);
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

        view.event(&UiEvent::FocusGained, &mut ctx);
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

        assert_eq!(clear.action, Action::SaveProject);
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
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline()
            .on_edit_command(|command| match command {
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
            })
            .on_track_add(|kind| match kind {
                TimelineTrackKind::Video => Action::Play,
                TimelineTrackKind::Audio => Action::Pause,
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
                position: Point::new(500.0, 42.0),
                button: MouseButton::Right,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(result, EventResult::Handled);
        assert!(view.overlay_hit_test(Point::new(900.0, 900.0)));

        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut paint_ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 520.0, 320.0),
        };
        view.paint_overlay(&mut paint_ctx);

        let result = view.event(
            &UiEvent::MouseDown {
                position: Point::new(510.0, 21.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(actions.borrow().as_slice(), &[Action::Play]);
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

        view.event(&UiEvent::FocusGained, &mut ctx);
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

        view.event(&UiEvent::FocusGained, &mut ctx);
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

        view.event(&UiEvent::FocusGained, &mut ctx);
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
            .on_seek(|frame| Action::Custom {
                namespace: "timeline.seek".into(),
                name: frame.to_string(),
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

        view.event(&UiEvent::FocusGained, &mut ctx);
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

        view.event(&UiEvent::FocusGained, &mut ctx);
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
        let mut view = timeline().on_seek(|frame| Action::Custom {
            namespace: "timeline.seek".into(),
            name: frame.to_string(),
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
        let view = timeline().with_frame_rate(Rational::FPS_25);

        assert_eq!(view.ruler_label_for_frame(250, 25), "00:00:10:00");
        assert_eq!(view.ruler_label_for_frame(250, 125), "00:10");
    }

    #[test]
    fn ruler_tick_steps_use_dense_minor_marks_and_meaningful_major_labels() {
        let view = timeline().with_frame_rate(Rational::FPS_25).with_pixels_per_frame(0.5);

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
        let mut view = timeline().on_seek(|frame| Action::Custom {
            namespace: "timeline.seek".into(),
            name: frame.to_string(),
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

        view.event(&UiEvent::FocusGained, &mut ctx);
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
        let mut view = timeline().with_playhead(12).on_seek(|frame| Action::Custom {
            namespace: "timeline.seek".into(),
            name: frame.to_string(),
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

        view.event(&UiEvent::FocusGained, &mut ctx);
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
}
