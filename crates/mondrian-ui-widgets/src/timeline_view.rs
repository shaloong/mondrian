//! Timeline surface primitive.
//!
//! `TimelineView` is intentionally domain-light: it renders tracks, clips,
//! playhead, scrolling, zooming, and selection in frame space. Editor crates can
//! map real `Sequence` / `Track` / `Clip` data into these view models without
//! pulling timeline command logic into the widget layer.

use mondrian_core::types::AssetId;
use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

use crate::{ContextMenu, MenuItem};

const SCROLLBAR_THICKNESS: f32 = 8.0;
const SCROLLBAR_MIN_THUMB: f32 = 28.0;
const TIMELINE_TOOL_BUTTON_SIZE: f32 = 18.0;
const TIMELINE_TOOL_BUTTON_GAP: f32 = 4.0;

/// Action factory for clip selection.
pub type TimelineClipAction = dyn Fn(TimelineClipRef, &TimelineClip) -> Action;

/// Action factory for track selection.
pub type TimelineTrackAction = dyn Fn(TimelineTrackRef, &TimelineTrack) -> Action;

/// Action factory for track reorder commits.
pub type TimelineTrackMoveAction = dyn Fn(TimelineTrackMove, &TimelineTrack) -> Action;

/// Action factory for track header control commits.
pub type TimelineTrackControlAction =
    dyn Fn(TimelineTrackControl, TimelineTrackRef, &TimelineTrack) -> Action;

/// Action factory for adding a track from the timeline corner controls.
pub type TimelineTrackAddAction = dyn Fn(TimelineTrackKind) -> Action;

/// Action factory for timeline-scoped editing commands.
pub type TimelineEditCommandAction = dyn Fn(TimelineEditCommand) -> Action;

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
    ruler_rect: Rect,
    header_rect: Rect,
    body_rect: Rect,
    selected_track: Option<TimelineTrackRef>,
    selected_clip: Option<TimelineClipRef>,
    hovered_clip: Option<TimelineClipRef>,
    hovered_track_control: Option<(TimelineTrackRef, TimelineTrackControl)>,
    hovered_tool: Option<TimelineTool>,
    hovered_track_add: Option<TimelineTrackKind>,
    active_tool: TimelineTool,
    playhead_frame: i64,
    in_point_frame: i64,
    out_point_frame: Option<i64>,
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
    context_menu: Option<ContextMenu>,
    horizontal_scrollbar_hovered: bool,
    vertical_scrollbar_hovered: bool,
    on_clip_select: Option<Box<TimelineClipAction>>,
    on_track_select: Option<Box<TimelineTrackAction>>,
    on_track_move: Option<Box<TimelineTrackMoveAction>>,
    on_track_control: Option<Box<TimelineTrackControlAction>>,
    on_track_add: Option<Box<TimelineTrackAddAction>>,
    on_edit_command: Option<Box<TimelineEditCommandAction>>,
    on_edit_command_shortcut: Option<Box<TimelineEditCommandShortcut>>,
    on_seek: Option<Box<TimelineSeekAction>>,
    on_asset_drop: Option<Box<TimelineAssetDropAction>>,
    on_clip_move: Option<Box<TimelineClipMoveAction>>,
    on_clip_trim: Option<Box<TimelineClipTrimAction>>,
    on_in_out_point: Option<Box<TimelineInOutPointAction>>,
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

#[derive(Debug, Clone, Copy)]
struct TimelineScrollbarDrag {
    axis: TimelineScrollbarAxis,
    start_pointer: f32,
    start_scroll: f32,
}

impl TimelineView {
    /// Create a timeline view with the provided tracks.
    pub fn new(tracks: Vec<TimelineTrack>) -> Self {
        Self {
            id: WidgetId::new(),
            tracks,
            bounds: Rect::ZERO,
            ruler_rect: Rect::ZERO,
            header_rect: Rect::ZERO,
            body_rect: Rect::ZERO,
            selected_track: None,
            selected_clip: None,
            hovered_clip: None,
            hovered_track_control: None,
            hovered_tool: None,
            hovered_track_add: None,
            active_tool: TimelineTool::Select,
            playhead_frame: 0,
            in_point_frame: 0,
            out_point_frame: None,
            pixels_per_frame: 4.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            focused: false,
            focus_visible: false,
            enabled: true,
            track_height: 50.0,
            header_width: 96.0,
            ruler_height: 30.0,
            playhead_dragging: false,
            in_out_drag: None,
            asset_drop_hover: None,
            track_drag: None,
            clip_drag: None,
            trim_drag: None,
            scrollbar_drag: None,
            context_menu: None,
            horizontal_scrollbar_hovered: false,
            vertical_scrollbar_hovered: false,
            on_clip_select: None,
            on_track_select: None,
            on_track_move: None,
            on_track_control: None,
            on_track_add: None,
            on_edit_command: None,
            on_edit_command_shortcut: None,
            on_seek: None,
            on_asset_drop: None,
            on_clip_move: None,
            on_clip_trim: None,
            on_in_out_point: None,
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

    /// Set initial zoom in pixels per frame.
    pub fn with_pixels_per_frame(mut self, pixels_per_frame: f32) -> Self {
        self.pixels_per_frame = pixels_per_frame.clamp(0.25, 64.0);
        self
    }

    /// Set the track header width.
    pub fn with_header_width(mut self, width: f32) -> Self {
        self.header_width = width.max(96.0);
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
            self.hovered_tool = None;
            self.hovered_track_add = None;
            self.active_tool = TimelineTool::Select;
            self.selected_track = None;
            self.asset_drop_hover = None;
            self.track_drag = None;
            self.clip_drag = None;
            self.trim_drag = None;
            self.scrollbar_drag = None;
            self.context_menu = None;
            self.horizontal_scrollbar_hovered = false;
            self.vertical_scrollbar_hovered = false;
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

    /// Set a dynamic add-track action factory for the timeline corner buttons.
    pub fn on_track_add(mut self, action: impl Fn(TimelineTrackKind) -> Action + 'static) -> Self {
        self.on_track_add = Some(Box::new(action));
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

    /// Active pointer tool for the timeline surface.
    pub fn active_tool(&self) -> TimelineTool {
        self.active_tool
    }

    /// Snapshot local timeline UI state for rebuild preservation.
    pub fn state(&self) -> TimelineViewState {
        TimelineViewState {
            active_tool: self.active_tool,
            scroll_x: self.scroll_x,
            scroll_y: self.scroll_y,
            pixels_per_frame: self.pixels_per_frame,
        }
    }

    /// Restore local timeline UI state after rebuilding from fresh models.
    pub fn restore_state(&mut self, state: &TimelineViewState) {
        self.active_tool = state.active_tool;
        self.pixels_per_frame = state.pixels_per_frame.clamp(0.25, 64.0);
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
        (self.max_scroll_x() > 0.0 && self.body_rect.width > SCROLLBAR_MIN_THUMB).then_some(
            Rect::new(
                self.body_rect.x + 4.0,
                self.body_rect.y + self.body_rect.height - SCROLLBAR_THICKNESS + 2.0,
                (self.body_rect.width - SCROLLBAR_THICKNESS - 8.0).max(0.0),
                (SCROLLBAR_THICKNESS - 4.0).max(1.0),
            ),
        )
    }

    fn vertical_scrollbar_track_rect(&self) -> Option<Rect> {
        (self.max_scroll_y() > 0.0 && self.body_rect.height > SCROLLBAR_MIN_THUMB).then_some(
            Rect::new(
                self.body_rect.x + self.body_rect.width - SCROLLBAR_THICKNESS + 2.0,
                self.body_rect.y + 4.0,
                (SCROLLBAR_THICKNESS - 4.0).max(1.0),
                (self.body_rect.height - SCROLLBAR_THICKNESS - 8.0).max(0.0),
            ),
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

    fn set_scrollbar_hovered(&mut self, point: Point) -> bool {
        let horizontal = self
            .horizontal_scrollbar_thumb_rect()
            .is_some_and(|thumb| thumb.contains(point));
        let vertical =
            self.vertical_scrollbar_thumb_rect().is_some_and(|thumb| thumb.contains(point));
        let changed = horizontal != self.horizontal_scrollbar_hovered
            || vertical != self.vertical_scrollbar_hovered;
        self.horizontal_scrollbar_hovered = horizontal;
        self.vertical_scrollbar_hovered = vertical;
        changed
    }

    fn frame_to_x(&self, frame: i64) -> f32 {
        self.body_rect.x + frame.max(0) as f32 * self.pixels_per_frame - self.scroll_x
    }

    fn x_to_frame(&self, x: f32) -> i64 {
        ((x - self.body_rect.x + self.scroll_x) / self.pixels_per_frame)
            .round()
            .max(0.0) as i64
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
        let gap = 5.0;
        let right_padding = 8.0;
        let group_width = size * 3.0 + gap * 2.0;
        let start_x = header.x + header.width - right_padding - group_width;
        let index = match control {
            TimelineTrackControl::Visibility => 0.0,
            TimelineTrackControl::Mute => 1.0,
            TimelineTrackControl::Lock => 2.0,
        };
        Rect::new(
            start_x + index * (size + gap),
            header.y + (header.height - size) * 0.5,
            size,
            size,
        )
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
            self.bounds.y,
            self.header_width,
            self.ruler_height,
        )
    }

    fn track_add_button_rect(&self, kind: TimelineTrackKind) -> Rect {
        let size = TIMELINE_TOOL_BUTTON_SIZE;
        let gap = TIMELINE_TOOL_BUTTON_GAP;
        let x = self.bounds.x
            + match kind {
                TimelineTrackKind::Video => 8.0 + (size + gap) * 2.0 + 2.0,
                TimelineTrackKind::Audio => 8.0 + (size + gap) * 3.0 + 2.0,
            };
        Rect::new(
            x,
            self.bounds.y + (self.ruler_height - size) * 0.5,
            size,
            size,
        )
    }

    fn track_add_at(&self, point: Point) -> Option<TimelineTrackKind> {
        if !self.timeline_corner_rect().contains(point) {
            return None;
        }
        [TimelineTrackKind::Video, TimelineTrackKind::Audio]
            .into_iter()
            .find(|kind| self.track_add_button_rect(*kind).contains(point))
    }

    fn tool_button_rect(&self, tool: TimelineTool) -> Rect {
        let size = TIMELINE_TOOL_BUTTON_SIZE;
        let gap = TIMELINE_TOOL_BUTTON_GAP;
        let x = self.bounds.x
            + match tool {
                TimelineTool::Select => 8.0,
                TimelineTool::Blade => 8.0 + size + gap,
            };
        Rect::new(
            x,
            self.bounds.y + (self.ruler_height - size) * 0.5,
            size,
            size,
        )
    }

    fn tool_at(&self, point: Point) -> Option<TimelineTool> {
        if !self.timeline_corner_rect().contains(point) {
            return None;
        }
        [TimelineTool::Select, TimelineTool::Blade]
            .into_iter()
            .find(|tool| self.tool_button_rect(*tool).contains(point))
    }

    fn clip_rect(&self, track_index: usize, clip: &TimelineClip) -> Rect {
        self.clip_rect_at(track_index, clip.start_frame, clip)
    }

    fn clip_rect_at(&self, track_index: usize, start_frame: i64, clip: &TimelineClip) -> Rect {
        let x = self.frame_to_x(start_frame);
        let y = self.track_y(track_index) + 6.0;
        let width = (clip.duration_frames.max(1) as f32 * self.pixels_per_frame).max(8.0);
        Rect::new(x, y, width, self.track_height - 12.0)
    }

    fn clip_rect_for_preview(
        &self,
        track_index: usize,
        start_frame: i64,
        duration_frames: i64,
    ) -> Rect {
        let x = self.frame_to_x(start_frame);
        let y = self.track_y(track_index) + 6.0;
        let width = (duration_frames.max(1) as f32 * self.pixels_per_frame).max(8.0);
        Rect::new(x, y, width, self.track_height - 12.0)
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

    fn activate_track_add_from_input(
        &mut self,
        kind: TimelineTrackKind,
        ctx: &mut EventContext,
    ) -> EventResult {
        if let Some(factory) = &self.on_track_add {
            (ctx.dispatch)(factory(kind));
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
        let new_start_frame = (self.x_to_frame(position.x) - drag.pointer_offset_frames).max(0);
        if self.clip(drag.clip_ref).is_none() {
            self.clip_drag = None;
            return false;
        };
        let target_track_index = self
            .track_index_at(position)
            .map(|index| self.compatible_drag_track(drag.clip_ref.track_index, index))
            .unwrap_or(drag.current_track_index);
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
            return false;
        };
        let pointer_frame = self.x_to_frame(position.x);
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
        let item = Self::menu_item(label, self.edit_command_action(command));
        if let Some(shortcut) = self.edit_command_shortcut(command) {
            item.with_shortcut(shortcut)
        } else {
            item
        }
    }

    fn clip_context_menu_items(&self) -> Vec<MenuItem> {
        let mut items = vec![
            self.edit_menu_item("Cut Clip", TimelineEditCommand::CutSelection),
            self.edit_menu_item("Copy Clip", TimelineEditCommand::CopySelection),
            self.edit_menu_item("Paste", TimelineEditCommand::PasteAtPlayhead),
            self.edit_menu_item("Duplicate Clip", TimelineEditCommand::DuplicateSelection),
            MenuItem::separator(),
            self.edit_menu_item("Delete Clip", TimelineEditCommand::DeleteSelection),
            self.edit_menu_item(
                "Ripple Delete Clip",
                TimelineEditCommand::RippleDeleteSelection,
            ),
            self.edit_menu_item("Split at Playhead", TimelineEditCommand::SplitAtPlayhead),
            MenuItem::separator(),
            self.edit_menu_item(
                "Trim In to Playhead",
                TimelineEditCommand::TrimSelectionInToPlayhead,
            ),
            self.edit_menu_item(
                "Trim Out to Playhead",
                TimelineEditCommand::TrimSelectionOutToPlayhead,
            ),
            MenuItem::separator(),
            self.edit_menu_item("Enable Clip", TimelineEditCommand::EnableSelection),
            self.edit_menu_item("Disable Clip", TimelineEditCommand::DisableSelection),
        ];
        if let Some(clip_ref) = self
            .selected_clip
            .filter(|clip_ref| self.clip(*clip_ref).is_some_and(|clip| clip.nested))
        {
            items.push(MenuItem::separator());
            items.push(self.edit_menu_item(
                "Open Nested Sequence",
                TimelineEditCommand::OpenNestedSequence(clip_ref),
            ));
        }
        items.extend([
            MenuItem::separator(),
            self.edit_menu_item("Mark In", TimelineEditCommand::MarkInAtPlayhead),
            self.edit_menu_item("Mark Out", TimelineEditCommand::MarkOutAtPlayhead),
        ]);
        items
    }

    fn timeline_context_menu_items(&self) -> Vec<MenuItem> {
        vec![
            Self::menu_item(
                "Add Video Track",
                self.track_add_action(TimelineTrackKind::Video),
            ),
            Self::menu_item(
                "Add Audio Track",
                self.track_add_action(TimelineTrackKind::Audio),
            ),
            MenuItem::separator(),
            self.edit_menu_item("Paste at Playhead", TimelineEditCommand::PasteAtPlayhead),
            MenuItem::separator(),
            self.edit_menu_item("Cut Selection", TimelineEditCommand::CutSelection),
            self.edit_menu_item("Copy Selection", TimelineEditCommand::CopySelection),
            self.edit_menu_item(
                "Duplicate Selection",
                TimelineEditCommand::DuplicateSelection,
            ),
            MenuItem::separator(),
            self.edit_menu_item("Split at Playhead", TimelineEditCommand::SplitAtPlayhead),
            self.edit_menu_item(
                "Trim Selection In to Playhead",
                TimelineEditCommand::TrimSelectionInToPlayhead,
            ),
            self.edit_menu_item(
                "Trim Selection Out to Playhead",
                TimelineEditCommand::TrimSelectionOutToPlayhead,
            ),
            MenuItem::separator(),
            self.edit_menu_item("Enable Selection", TimelineEditCommand::EnableSelection),
            self.edit_menu_item("Disable Selection", TimelineEditCommand::DisableSelection),
            MenuItem::separator(),
            self.edit_menu_item("Mark In", TimelineEditCommand::MarkInAtPlayhead),
            self.edit_menu_item("Mark Out", TimelineEditCommand::MarkOutAtPlayhead),
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
            KeyCode::B if !modifiers.shift && (modifiers.ctrl || modifiers.meta) => {
                TimelineEditCommand::SplitAtPlayhead
            }
            KeyCode::I if !modifiers.ctrl && !modifiers.meta && !modifiers.shift => {
                TimelineEditCommand::MarkInAtPlayhead
            }
            KeyCode::O if !modifiers.ctrl && !modifiers.meta && !modifiers.shift => {
                TimelineEditCommand::MarkOutAtPlayhead
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

    fn zoom_at(&mut self, anchor_x: f32, factor: f32) {
        let frame_at_anchor = self.x_to_frame(anchor_x) as f32;
        let old = self.pixels_per_frame;
        self.pixels_per_frame = (self.pixels_per_frame * factor).clamp(0.25, 64.0);
        if (old - self.pixels_per_frame).abs() > f32::EPSILON {
            self.scroll_x = frame_at_anchor * self.pixels_per_frame - (anchor_x - self.body_rect.x);
            self.clamp_scroll();
        }
    }

    fn tick_step_frames(&self) -> i64 {
        let target_px = 88.0;
        let raw = (target_px / self.pixels_per_frame).max(1.0);
        for step in [1, 2, 5, 10, 15, 30, 60, 120, 240, 600, 1200] {
            if raw <= step as f32 {
                return step;
            }
        }
        2400
    }

    fn paint_ruler(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        ctx.encoder.draw_rect(self.ruler_rect, colors.card, 0.0);
        self.paint_in_out_ruler_region(ctx);
        let step = self.tick_step_frames();
        let start_frame = (self.scroll_x / self.pixels_per_frame).floor().max(0.0) as i64;
        let first_tick = start_frame - start_frame % step;
        let end_frame = self.x_to_frame(self.body_rect.x + self.body_rect.width) + step;
        let mut frame = first_tick;
        while frame <= end_frame {
            let x = self.frame_to_x(frame);
            let major = frame % (step * 4) == 0;
            let height = if major { 16.0 } else { 8.0 };
            ctx.encoder.draw_line(
                Point::new(x, self.ruler_rect.y + self.ruler_rect.height - height),
                Point::new(x, self.ruler_rect.y + self.ruler_rect.height - 1.0),
                1.0,
                colors.border,
            );
            if major {
                ctx.encoder.draw_text(
                    &frame.to_string(),
                    ctx.theme.typography.metadata.font_size,
                    snap_point(Point::new(x + 4.0, self.ruler_rect.y + 6.0)),
                    colors.muted_foreground,
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
            colors.border,
        );
    }

    fn paint_timeline_corner(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        let corner = self.timeline_corner_rect();
        ctx.encoder.draw_rect(corner, colors.card, 0.0);
        ctx.encoder.draw_line(
            Point::new(corner.x, corner.y + corner.height - 1.0),
            Point::new(corner.x + corner.width, corner.y + corner.height - 1.0),
            1.0,
            colors.border,
        );
        ctx.encoder.draw_line(
            Point::new(corner.x + corner.width - 1.0, corner.y),
            Point::new(corner.x + corner.width - 1.0, corner.y + corner.height),
            1.0,
            colors.border,
        );
        self.paint_tool_button(ctx, TimelineTool::Select);
        self.paint_tool_button(ctx, TimelineTool::Blade);
        self.paint_track_add_button(ctx, TimelineTrackKind::Video);
        self.paint_track_add_button(ctx, TimelineTrackKind::Audio);
    }

    fn paint_tool_button(&self, ctx: &mut PaintContext, tool: TimelineTool) {
        let colors = &ctx.theme.colors;
        let rect = self.tool_button_rect(tool);
        let active = self.active_tool == tool;
        let hovered = self.hovered_tool == Some(tool);
        let mut bg = if active {
            colors.accent
        } else if hovered {
            colors.secondary
        } else {
            colors.card
        };
        bg.a = if active || hovered { 0.92 } else { 0.32 };
        ctx.encoder.draw_rect(rect, bg, ctx.theme.spacing.radius_sm);

        let icon = if active {
            colors.accent_foreground
        } else {
            colors.muted_foreground
        };
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

    fn paint_track_add_button(&self, ctx: &mut PaintContext, kind: TimelineTrackKind) {
        let colors = &ctx.theme.colors;
        let rect = self.track_add_button_rect(kind);
        let hovered = self.hovered_track_add == Some(kind);
        let mut bg = if hovered {
            colors.secondary
        } else {
            colors.card
        };
        bg.a = if hovered { 0.9 } else { 0.32 };
        ctx.encoder.draw_rect(rect, bg, ctx.theme.spacing.radius_sm);

        let icon = if hovered {
            colors.foreground
        } else {
            colors.muted_foreground
        };
        match kind {
            TimelineTrackKind::Video => {
                let frame = Rect::new(rect.x + 4.0, rect.y + 6.0, 8.0, 6.0);
                ctx.encoder.draw_rect(frame, icon, 1.5);
                ctx.encoder.draw_triangles(
                    &[
                        Point::new(frame.x + frame.width, frame.y + 1.2),
                        Point::new(frame.x + frame.width + 4.0, frame.y - 0.6),
                        Point::new(frame.x + frame.width + 4.0, frame.y + frame.height + 0.6),
                    ],
                    icon,
                );
            }
            TimelineTrackKind::Audio => {
                let cy = rect.center().y;
                ctx.encoder.draw_line(
                    Point::new(rect.x + 4.0, cy + 2.0),
                    Point::new(rect.x + 7.0, cy - 3.0),
                    1.4,
                    icon,
                );
                ctx.encoder.draw_line(
                    Point::new(rect.x + 7.0, cy - 3.0),
                    Point::new(rect.x + 10.0, cy + 3.0),
                    1.4,
                    icon,
                );
                ctx.encoder.draw_line(
                    Point::new(rect.x + 10.0, cy + 3.0),
                    Point::new(rect.x + 13.0, cy - 2.0),
                    1.4,
                    icon,
                );
            }
        }
        let plus_center = Point::new(rect.x + rect.width - 5.0, rect.y + 5.0);
        ctx.encoder.draw_line(
            Point::new(plus_center.x - 3.0, plus_center.y),
            Point::new(plus_center.x + 3.0, plus_center.y),
            1.5,
            icon,
        );
        ctx.encoder.draw_line(
            Point::new(plus_center.x, plus_center.y - 3.0),
            Point::new(plus_center.x, plus_center.y + 3.0),
            1.5,
            icon,
        );
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
            ctx.encoder.draw_rect(
                header,
                if track_selected {
                    colors.accent
                } else {
                    colors.card
                },
                0.0,
            );
            let control_group_x =
                self.track_control_rect(header, TimelineTrackControl::Visibility).x;
            ctx.encoder.draw_text_box(
                &track.label,
                ctx.theme.typography.tab_label.font_size,
                snap_point(Point::new(header.x + 10.0, header.y + 16.0)),
                (control_group_x - header.x - 18.0).max(0.0),
                if track_selected {
                    colors.accent_foreground
                } else if track.locked {
                    colors.muted_foreground
                } else {
                    colors.foreground
                },
            );
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
                colors.background
            } else {
                colors.card
            };
            ctx.encoder.draw_rect(row, row_fill, 0.0);
            self.paint_in_out_row_region(ctx, row);
            if self.asset_drop_hover.is_some_and(|hover| hover.track_index == track_index) {
                let mut target = colors.accent;
                target.a = 0.14;
                ctx.encoder.draw_rect(row, target, 0.0);
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
            let mut fill = colors.ring;
            fill.a = 0.10;
            ctx.encoder.draw_rect(
                Rect::new(x0, self.ruler_rect.y, x1 - x0, self.ruler_rect.height),
                fill,
                0.0,
            );
        }
        self.paint_in_out_marker_lines(ctx, self.ruler_rect);
    }

    fn paint_in_out_row_region(&self, ctx: &mut PaintContext, row: Rect) {
        let colors = &ctx.theme.colors;
        if let Some((x0, x1)) = self.in_out_visible_range() {
            let mut fill = colors.ring;
            fill.a = 0.055;
            ctx.encoder.draw_rect(Rect::new(x0, row.y, x1 - x0, row.height), fill, 0.0);
        }
        self.paint_in_out_marker_lines(ctx, row);
    }

    fn paint_in_out_marker_lines(&self, ctx: &mut PaintContext, rect: Rect) {
        let colors = &ctx.theme.colors;
        let mut color = colors.ring;
        color.a = 0.55;
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
        let active = match control {
            TimelineTrackControl::Visibility => track.visible,
            TimelineTrackControl::Mute => track.muted,
            TimelineTrackControl::Lock => track.locked,
        };
        let mut bg = if hovered {
            colors.secondary
        } else {
            colors.card
        };
        bg.a = if hovered || active { 0.85 } else { 0.18 };
        ctx.encoder.draw_rect(rect, bg, ctx.theme.spacing.radius_sm);

        let mut icon = if active {
            colors.foreground
        } else {
            colors.muted_foreground
        };
        if !track.visible && control != TimelineTrackControl::Visibility {
            icon.a *= 0.52;
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
        let spacing = &ctx.theme.spacing;
        let track_kind = self
            .tracks
            .get(clip_ref.track_index)
            .map(|track| track.kind)
            .unwrap_or(TimelineTrackKind::Video);
        let selected = clip.selected || self.selected_clip == Some(clip_ref) || dragging;
        let hovered = self.hovered_clip == Some(clip_ref);
        let mut fill = clip.color.unwrap_or(match track_kind {
            TimelineTrackKind::Video => colors.timeline_clip_video,
            TimelineTrackKind::Audio => colors.timeline_clip_audio,
        });
        if clip.disabled {
            fill.a *= 0.45;
        } else if dragging {
            fill = fill.lerp(colors.primary, 0.24);
            fill.a *= 0.92;
        } else if hovered {
            fill = fill.lerp(colors.primary, 0.18);
        }
        if selected {
            let mut ring = colors.ring;
            ring.a = if dragging { 0.72 } else { 0.55 };
            ctx.encoder.draw_rect(rect.inset(-1.5, -1.5), ring, spacing.radius_sm + 1.5);
        }
        ctx.encoder.draw_rect(rect, fill, spacing.radius_sm);
        ctx.push_clip(rect.inset(6.0, 2.0));
        ctx.encoder.draw_text_box(
            &clip.label,
            ctx.theme.typography.body.font_size,
            snap_point(Point::new(rect.x + 8.0, rect.y + 10.0)),
            (rect.width - 16.0).max(0.0),
            colors.foreground,
        );
        ctx.pop_clip();
    }

    fn paint_playhead(&self, ctx: &mut PaintContext) {
        let x = self.frame_to_x(self.playhead_frame);
        if x < self.body_rect.x - 1.0 || x > self.body_rect.x + self.body_rect.width + 1.0 {
            return;
        }
        let colors = &ctx.theme.colors;
        ctx.encoder.draw_line(
            Point::new(x, self.ruler_rect.y),
            Point::new(x, self.body_rect.y + self.body_rect.height),
            2.0,
            colors.timeline_playhead,
        );
        let marker = [
            Point::new(x, self.ruler_rect.y + self.ruler_rect.height - 1.0),
            Point::new(x - 5.0, self.ruler_rect.y + self.ruler_rect.height - 9.0),
            Point::new(x + 5.0, self.ruler_rect.y + self.ruler_rect.height - 9.0),
        ];
        ctx.encoder.draw_triangles(&marker, colors.timeline_playhead);
    }

    fn paint_scrollbars(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        let radius = ctx.theme.spacing.radius_full;
        if let (Some(track), Some(mut thumb)) = (
            self.horizontal_scrollbar_track_rect(),
            self.horizontal_scrollbar_thumb_rect(),
        ) {
            let dragging = self
                .scrollbar_drag
                .is_some_and(|drag| drag.axis == TimelineScrollbarAxis::Horizontal);
            let active = dragging || self.horizontal_scrollbar_hovered;
            if active {
                thumb = Rect::new(thumb.x, thumb.y - 1.0, thumb.width, thumb.height + 2.0);
            }
            let mut track_color = colors.scrollbar_thumb;
            track_color.a *= if active { 0.22 } else { 0.12 };
            ctx.encoder.draw_rect(track, track_color, radius);

            let mut thumb_color = colors.scrollbar_thumb;
            thumb_color.a *= if dragging {
                1.0
            } else if active {
                0.82
            } else {
                0.62
            };
            ctx.encoder.draw_rect(thumb, thumb_color, radius);
        }

        if let (Some(track), Some(mut thumb)) = (
            self.vertical_scrollbar_track_rect(),
            self.vertical_scrollbar_thumb_rect(),
        ) {
            let dragging = self
                .scrollbar_drag
                .is_some_and(|drag| drag.axis == TimelineScrollbarAxis::Vertical);
            let active = dragging || self.vertical_scrollbar_hovered;
            if active {
                thumb = Rect::new(thumb.x - 1.0, thumb.y, thumb.width + 2.0, thumb.height);
            }
            let mut track_color = colors.scrollbar_thumb;
            track_color.a *= if active { 0.22 } else { 0.12 };
            ctx.encoder.draw_rect(track, track_color, radius);

            let mut thumb_color = colors.scrollbar_thumb;
            thumb_color.a *= if dragging {
                1.0
            } else if active {
                0.82
            } else {
                0.62
            };
            ctx.encoder.draw_rect(thumb, thumb_color, radius);
        }
    }
}

impl Widget for TimelineView {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(560.0, self.ruler_height + self.content_height()))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.header_rect = Rect::new(
            bounds.x,
            bounds.y + self.ruler_height,
            self.header_width,
            (bounds.height - self.ruler_height).max(0.0),
        );
        self.ruler_rect = Rect::new(
            bounds.x + self.header_width,
            bounds.y,
            (bounds.width - self.header_width).max(0.0),
            self.ruler_height,
        );
        self.body_rect = Rect::new(
            bounds.x + self.header_width,
            bounds.y + self.ruler_height,
            (bounds.width - self.header_width).max(0.0),
            (bounds.height - self.ruler_height).max(0.0),
        );
        self.clamp_scroll();
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.enabled {
            if self.playhead_dragging
                || self.in_out_drag.is_some()
                || self.track_drag.is_some()
                || self.clip_drag.is_some()
                || self.trim_drag.is_some()
                || self.scrollbar_drag.is_some()
                || self.focused
            {
                ctx.release_pointer_capture(self.id);
            }
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
            self.hovered_tool = None;
            self.hovered_track_add = None;
            self.horizontal_scrollbar_hovered = false;
            self.vertical_scrollbar_hovered = false;
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
                if let Some(thumb) = self.horizontal_scrollbar_thumb_rect() {
                    if thumb.contains(*position) {
                        self.scrollbar_drag = Some(TimelineScrollbarDrag {
                            axis: TimelineScrollbarAxis::Horizontal,
                            start_pointer: position.x,
                            start_scroll: self.scroll_x,
                        });
                        self.horizontal_scrollbar_hovered = true;
                        ctx.request_pointer_capture(self.id);
                        ctx.request_repaint();
                        return EventResult::Handled;
                    }
                }
                if let Some(thumb) = self.vertical_scrollbar_thumb_rect() {
                    if thumb.contains(*position) {
                        self.scrollbar_drag = Some(TimelineScrollbarDrag {
                            axis: TimelineScrollbarAxis::Vertical,
                            start_pointer: position.y,
                            start_scroll: self.scroll_y,
                        });
                        self.vertical_scrollbar_hovered = true;
                        ctx.request_pointer_capture(self.id);
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
                if let Some(tool) = self.tool_at(*position) {
                    return self.set_active_tool(tool, ctx);
                }
                if let Some(kind) = self.track_add_at(*position) {
                    return self.activate_track_add_from_input(kind, ctx);
                }
                if self.ruler_rect.contains(*position) {
                    if let Some(point) = self.in_out_marker_at(*position) {
                        self.start_in_out_drag(point);
                        ctx.request_pointer_capture(self.id);
                        ctx.request_repaint();
                        return EventResult::Handled;
                    }
                    self.playhead_dragging = true;
                    ctx.request_pointer_capture(self.id);
                    self.seek_from_input(self.x_to_frame(position.x), ctx);
                    return EventResult::Handled;
                }
                if let Some((track_ref, control)) = self.track_control_at(*position) {
                    return self.activate_track_control_from_input(track_ref, control, ctx);
                }
                if let Some(track_ref) = self.track_header_at(*position) {
                    let result = self.select_track_from_input(track_ref, ctx);
                    self.start_track_drag(track_ref);
                    ctx.request_pointer_capture(self.id);
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
                        ctx.request_pointer_capture(self.id);
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
                if self.playhead_dragging {
                    self.seek_from_input(self.x_to_frame(position.x), ctx);
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
                    let changed = match drag.axis {
                        TimelineScrollbarAxis::Horizontal => self.set_scroll_x(
                            self.scroll_x_for_thumb_delta(position.x - drag.start_pointer, drag),
                        ),
                        TimelineScrollbarAxis::Vertical => self.set_scroll_y(
                            self.scroll_y_for_thumb_delta(position.y - drag.start_pointer, drag),
                        ),
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
                let hovered_tool = self.tool_at(*position);
                if hovered_tool != self.hovered_tool {
                    self.hovered_tool = hovered_tool;
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                let hovered_track_add = self.track_add_at(*position);
                if hovered_track_add != self.hovered_track_add {
                    self.hovered_track_add = hovered_track_add;
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                let hovered_track_control = self.track_control_at(*position);
                if hovered_track_control != self.hovered_track_control {
                    self.hovered_track_control = hovered_track_control;
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                let hovered = self.hit_clip(*position);
                if hovered != self.hovered_clip {
                    self.hovered_clip = hovered;
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.playhead_dragging => {
                self.playhead_dragging = false;
                ctx.release_pointer_capture(self.id);
                ctx.request_repaint();
                return EventResult::Handled;
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.in_out_drag.is_some() => {
                self.finish_in_out_drag(ctx);
                ctx.release_pointer_capture(self.id);
                return EventResult::Handled;
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.track_drag.is_some() => {
                self.finish_track_drag(ctx);
                ctx.release_pointer_capture(self.id);
                return EventResult::Handled;
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.clip_drag.is_some() => {
                self.finish_clip_drag(ctx);
                ctx.release_pointer_capture(self.id);
                return EventResult::Handled;
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.trim_drag.is_some() => {
                self.finish_trim_drag(ctx);
                ctx.release_pointer_capture(self.id);
                return EventResult::Handled;
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.scrollbar_drag.is_some() => {
                self.scrollbar_drag = None;
                ctx.release_pointer_capture(self.id);
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
                self.in_out_drag = None;
                self.asset_drop_hover = None;
                self.track_drag = None;
                self.clip_drag = None;
                self.trim_drag = None;
                self.scrollbar_drag = None;
                self.context_menu = None;
                self.hovered_tool = None;
                ctx.release_pointer_capture(self.id);
                return EventResult::Handled;
            }
            UiEvent::KeyDown { key, modifiers } if self.focused => {
                if !modifiers.ctrl && !modifiers.meta && !modifiers.alt && !modifiers.shift {
                    match key {
                        KeyCode::V => return self.set_active_tool(TimelineTool::Select, ctx),
                        KeyCode::B => return self.set_active_tool(TimelineTool::Blade, ctx),
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
                if modifiers.ctrl || modifiers.meta {
                    let factor = if *delta < 0.0 { 1.12 } else { 1.0 / 1.12 };
                    let old = self.pixels_per_frame;
                    self.zoom_at(position.x, factor);
                    if (self.pixels_per_frame - old).abs() > 0.001 {
                        ctx.request_repaint();
                    }
                } else if modifiers.shift {
                    if self.set_scroll_x(self.scroll_x + *delta) {
                        ctx.request_repaint();
                    }
                } else if self.set_scroll_y(self.scroll_y + *delta) {
                    ctx.request_repaint();
                }
                return EventResult::Handled;
            }
            _ => {}
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        ctx.encoder.draw_rect(self.bounds, colors.background, 0.0);
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

        ctx.push_clip(self.ruler_rect);
        self.paint_ruler(ctx);
        ctx.pop_clip();

        ctx.push_clip(self.body_rect);
        self.paint_tracks(ctx);
        ctx.pop_clip();

        ctx.push_clip(Rect::new(
            self.ruler_rect.x,
            self.ruler_rect.y,
            self.ruler_rect.width,
            self.ruler_rect.height + self.body_rect.height,
        ));
        self.paint_playhead(ctx);
        ctx.pop_clip();

        ctx.push_clip(self.body_rect);
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

    fn dispatching_ctx<'a>(
        focus: &'a mut DummyFocus,
        shortcut: &'a mut DummyShortcut,
        tooltip: &'a mut DummyTooltip,
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
    struct RecordingEncoder {
        rects: usize,
        lines: usize,
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

        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {
            self.lines += 1;
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
                position: Point::new(108.0, 42.0),
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
                position: Point::new(20.0, 42.0),
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
                position: Point::new(12.0, 105.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseMove {
                position: Point::new(12.0, 55.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        let result = view.event(
            &UiEvent::MouseUp {
                position: Point::new(12.0, 55.0),
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
                position: Point::new(12.0, 55.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseMove {
                position: Point::new(12.0, 105.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseUp {
                position: Point::new(12.0, 105.0),
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
                    position: Point::new(168.0, 105.0),
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
                    position: Point::new(168.0, 105.0),
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
                position: Point::new(64.0, 48.0),
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
                position: Point::new(88.0, 55.0),
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
    fn clicking_timeline_corner_add_buttons_dispatches_track_kind_without_seeking() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut view = timeline().on_track_add(|kind| match kind {
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
                position: Point::new(63.0, 15.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(view.playhead_frame(), 12);
        assert_eq!(actions.borrow().as_slice(), &[Action::Play]);
        assert!(ctx.requests.pointer_capture.is_none());
        assert!(ctx.requests.repaint);

        ctx.requests.repaint = false;
        let result = view.event(
            &UiEvent::MouseDown {
                position: Point::new(85.0, 15.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(view.playhead_frame(), 12);
        assert_eq!(actions.borrow().as_slice(), &[Action::Play, Action::Pause]);
        assert!(ctx.requests.pointer_capture.is_none());
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
                position: Point::new(192.0, 12.0),
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
                position: Point::new(192.0, 12.0),
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
                    position: Point::new(192.0, 12.0),
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
                    position: Point::new(192.0, 12.0),
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
                    position: Point::new(216.0, 12.0),
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
                position: Point::new(108.0, 42.0),
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
                position: Point::new(148.0, 42.0),
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
                position: Point::new(148.0, 42.0),
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
                position: Point::new(108.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseMove {
                position: Point::new(148.0, 92.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(view
            .clip_drag
            .is_some_and(|drag| drag.current_start_frame == 10 && drag.current_track_index == 1));

        view.event(
            &UiEvent::MouseUp {
                position: Point::new(148.0, 92.0),
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
                position: Point::new(108.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseMove {
                position: Point::new(148.0, 92.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseUp {
                position: Point::new(148.0, 92.0),
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
                position: Point::new(108.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseUp {
                position: Point::new(108.0, 42.0),
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
                position: Point::new(258.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(view.trim_drag.is_some());
        view.event(
            &UiEvent::MouseMove {
                position: Point::new(278.0, 42.0),
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
                position: Point::new(278.0, 42.0),
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
                position: Point::new(374.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseMove {
                position: Point::new(394.0, 42.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseUp {
                position: Point::new(394.0, 42.0),
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
                position: Point::new(258.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseUp {
                position: Point::new(258.0, 42.0),
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
                position: Point::new(108.0, 42.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        view.event(
            &UiEvent::MouseMove {
                position: Point::new(148.0, 42.0),
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
                    position: Point::new(108.0, 42.0),
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

        view.event(
            &UiEvent::MouseWheel {
                delta: 60.0,
                position: Point::new(180.0, 90.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(view.scroll_y() > 0.0);

        view.event(
            &UiEvent::MouseWheel {
                delta: 80.0,
                position: Point::new(180.0, 90.0),
                modifiers: Modifiers::shift(),
            },
            &mut ctx,
        );
        assert!(view.scroll_x() > 0.0);
    }

    #[test]
    fn horizontal_scrollbar_thumb_drag_updates_offset_and_releases_capture() {
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Long", 0, 1000)],
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
                position: Point::new(260.0, 90.0),
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
                TimelineEditCommand::EnableSelection => Action::Play,
                TimelineEditCommand::DisableSelection => Action::Pause,
                TimelineEditCommand::OpenNestedSequence(_) => Action::NoOp,
                TimelineEditCommand::MarkInAtPlayhead => Action::MarkInAtPlayhead,
                TimelineEditCommand::MarkOutAtPlayhead => Action::MarkOutAtPlayhead,
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
            TimelineEditCommand::EnableSelection => Action::Play,
            TimelineEditCommand::DisableSelection => Action::Pause,
            TimelineEditCommand::OpenNestedSequence(_) => Action::NoOp,
            TimelineEditCommand::MarkInAtPlayhead => Action::MarkInAtPlayhead,
            TimelineEditCommand::MarkOutAtPlayhead => Action::MarkOutAtPlayhead,
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
                position: Point::new(108.0, 42.0),
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

        let result = view.event(
            &UiEvent::MouseDown {
                position: Point::new(120.0, 189.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
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
            .find(|item| item.label == "Open Nested Sequence")
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
            items.iter().all(|item| item.label != "Open Nested Sequence"),
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
                TimelineEditCommand::EnableSelection => Action::Play,
                TimelineEditCommand::DisableSelection => Action::Pause,
                TimelineEditCommand::OpenNestedSequence(_) => Action::NoOp,
                TimelineEditCommand::MarkInAtPlayhead => Action::MarkInAtPlayhead,
                TimelineEditCommand::MarkOutAtPlayhead => Action::MarkOutAtPlayhead,
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
                position: Point::new(104.0, 42.0),
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
                    position: Point::new(136.0, 12.0),
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
                    position: Point::new(176.0, 12.0),
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
                    position: Point::new(176.0, 12.0),
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
    }
}
