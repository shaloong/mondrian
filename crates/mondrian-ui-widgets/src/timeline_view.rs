//! Timeline surface primitive.
//!
//! `TimelineView` is intentionally domain-light: it renders tracks, clips,
//! playhead, scrolling, zooming, and selection in frame space. Editor crates can
//! map real `Sequence` / `Track` / `Clip` data into these view models without
//! pulling timeline command logic into the widget layer.

use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// Action factory for clip selection.
pub type TimelineClipAction = dyn Fn(TimelineClipRef, &TimelineClip) -> Action;

/// Action factory for playhead seeking.
pub type TimelineSeekAction = dyn Fn(i64) -> Action;

/// Action factory for clip move commits.
pub type TimelineClipMoveAction = dyn Fn(TimelineClipMove, &TimelineClip) -> Action;

/// Stable view reference to a clip inside the timeline surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineClipRef {
    pub track_index: usize,
    pub clip_index: usize,
}

/// Domain-light clip move proposal emitted when a drag commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineClipMove {
    pub clip_ref: TimelineClipRef,
    pub old_start_frame: i64,
    pub new_start_frame: i64,
}

/// Track category used only for styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineTrackKind {
    Video,
    Audio,
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
    pub muted: bool,
    pub locked: bool,
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
            muted: false,
            locked: false,
        }
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
}

/// Scrollable, zoomable timeline surface.
pub struct TimelineView {
    id: WidgetId,
    tracks: Vec<TimelineTrack>,
    bounds: Rect,
    ruler_rect: Rect,
    header_rect: Rect,
    body_rect: Rect,
    selected_clip: Option<TimelineClipRef>,
    hovered_clip: Option<TimelineClipRef>,
    playhead_frame: i64,
    pixels_per_frame: f32,
    scroll_x: f32,
    scroll_y: f32,
    track_height: f32,
    header_width: f32,
    ruler_height: f32,
    playhead_dragging: bool,
    clip_drag: Option<TimelineClipDrag>,
    on_clip_select: Option<Box<TimelineClipAction>>,
    on_seek: Option<Box<TimelineSeekAction>>,
    on_clip_move: Option<Box<TimelineClipMoveAction>>,
}

#[derive(Debug, Clone, Copy)]
struct TimelineClipDrag {
    clip_ref: TimelineClipRef,
    old_start_frame: i64,
    pointer_offset_frames: i64,
    moved: bool,
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
            selected_clip: None,
            hovered_clip: None,
            playhead_frame: 0,
            pixels_per_frame: 4.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            track_height: 50.0,
            header_width: 96.0,
            ruler_height: 30.0,
            playhead_dragging: false,
            clip_drag: None,
            on_clip_select: None,
            on_seek: None,
            on_clip_move: None,
        }
    }

    /// Set the current playhead frame.
    pub fn with_playhead(mut self, frame: i64) -> Self {
        self.playhead_frame = frame.max(0);
        self
    }

    /// Set initial zoom in pixels per frame.
    pub fn with_pixels_per_frame(mut self, pixels_per_frame: f32) -> Self {
        self.pixels_per_frame = pixels_per_frame.clamp(0.25, 64.0);
        self
    }

    /// Set a dynamic clip-selection action factory.
    pub fn on_clip_select(
        mut self,
        action: impl Fn(TimelineClipRef, &TimelineClip) -> Action + 'static,
    ) -> Self {
        self.on_clip_select = Some(Box::new(action));
        self
    }

    /// Set a dynamic seek action factory.
    pub fn on_seek(mut self, action: impl Fn(i64) -> Action + 'static) -> Self {
        self.on_seek = Some(Box::new(action));
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

    /// Current selected clip reference.
    pub fn selected_clip(&self) -> Option<TimelineClipRef> {
        self.selected_clip
    }

    /// Current playhead frame.
    pub fn playhead_frame(&self) -> i64 {
        self.playhead_frame
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

    fn content_width(&self) -> f32 {
        let max_frame = self
            .tracks
            .iter()
            .flat_map(|track| track.clips.iter().map(TimelineClip::end_frame))
            .max()
            .unwrap_or(240)
            .max(240);
        max_frame as f32 * self.pixels_per_frame + 160.0
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

    fn clip_rect(&self, track_index: usize, clip: &TimelineClip) -> Rect {
        let x = self.frame_to_x(clip.start_frame);
        let y = self.track_y(track_index) + 6.0;
        let width = (clip.duration_frames.max(1) as f32 * self.pixels_per_frame).max(8.0);
        Rect::new(x, y, width, self.track_height - 12.0)
    }

    fn hit_clip(&self, point: Point) -> Option<TimelineClipRef> {
        if !self.body_rect.contains(point) {
            return None;
        }
        for (track_index, track) in self.tracks.iter().enumerate() {
            for (clip_index, clip) in track.clips.iter().enumerate().rev() {
                if clip.disabled {
                    continue;
                }
                if self.clip_rect(track_index, clip).contains(point) {
                    return Some(TimelineClipRef { track_index, clip_index });
                }
            }
        }
        None
    }

    fn clip(&self, clip_ref: TimelineClipRef) -> Option<&TimelineClip> {
        self.tracks
            .get(clip_ref.track_index)
            .and_then(|track| track.clips.get(clip_ref.clip_index))
    }

    fn clip_mut(&mut self, clip_ref: TimelineClipRef) -> Option<&mut TimelineClip> {
        self.tracks
            .get_mut(clip_ref.track_index)
            .and_then(|track| track.clips.get_mut(clip_ref.clip_index))
    }

    fn track_locked(&self, track_index: usize) -> bool {
        self.tracks.get(track_index).is_some_and(|track| track.locked)
    }

    fn select_clip_from_input(
        &mut self,
        clip_ref: TimelineClipRef,
        ctx: &mut EventContext,
    ) -> EventResult {
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
            moved: false,
        });
    }

    fn drag_clip_to(&mut self, position: Point, ctx: &mut EventContext) -> bool {
        let Some(mut drag) = self.clip_drag else {
            return false;
        };
        let new_start_frame = (self.x_to_frame(position.x) - drag.pointer_offset_frames).max(0);
        let Some(clip) = self.clip_mut(drag.clip_ref) else {
            self.clip_drag = None;
            return false;
        };
        if clip.start_frame == new_start_frame {
            return true;
        }
        clip.start_frame = new_start_frame;
        drag.moved = true;
        self.clip_drag = Some(drag);
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
            new_start_frame: clip.start_frame,
        };
        if movement.old_start_frame != movement.new_start_frame {
            if let Some(factory) = &self.on_clip_move {
                (ctx.dispatch)(factory(movement, clip));
            }
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

    fn paint_tracks(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
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
            ctx.encoder.draw_rect(header, colors.card, 0.0);
            ctx.encoder.draw_text(
                &track.label,
                ctx.theme.typography.tab_label.font_size,
                snap_point(Point::new(header.x + 10.0, header.y + 16.0)),
                if track.locked {
                    colors.muted_foreground
                } else {
                    colors.foreground
                },
            );

            let row = Rect::new(self.body_rect.x, y, self.body_rect.width, self.track_height);
            let row_fill = if track_index % 2 == 0 {
                colors.background
            } else {
                colors.card
            };
            ctx.encoder.draw_rect(row, row_fill, 0.0);
            ctx.encoder.draw_line(
                Point::new(self.bounds.x, y + self.track_height),
                Point::new(self.bounds.x + self.bounds.width, y + self.track_height),
                1.0,
                colors.border,
            );

            for (clip_index, clip) in track.clips.iter().enumerate() {
                let rect = self.clip_rect(track_index, clip);
                if rect.x > self.body_rect.x + self.body_rect.width
                    || rect.x + rect.width < self.body_rect.x
                {
                    continue;
                }
                let clip_ref = TimelineClipRef { track_index, clip_index };
                let selected = clip.selected || self.selected_clip == Some(clip_ref);
                let hovered = self.hovered_clip == Some(clip_ref);
                let mut fill = clip.color.unwrap_or(match track.kind {
                    TimelineTrackKind::Video => colors.timeline_clip_video,
                    TimelineTrackKind::Audio => colors.timeline_clip_audio,
                });
                if clip.disabled {
                    fill.a *= 0.45;
                } else if hovered {
                    fill = fill.lerp(colors.primary, 0.18);
                }
                if selected {
                    let mut ring = colors.ring;
                    ring.a = 0.55;
                    ctx.encoder.draw_rect(rect.inset(-1.5, -1.5), ring, spacing.radius_sm + 1.5);
                }
                ctx.encoder.draw_rect(rect, fill, spacing.radius_sm);
                ctx.encoder.push_clip(rect.inset(6.0, 2.0));
                ctx.encoder.draw_text_box(
                    &clip.label,
                    ctx.theme.typography.body.font_size,
                    snap_point(Point::new(rect.x + 8.0, rect.y + 10.0)),
                    (rect.width - 16.0).max(0.0),
                    colors.foreground,
                );
                ctx.encoder.pop_clip();
            }
        }
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
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                if !self.bounds.contains(*position) {
                    return EventResult::Ignored;
                }
                if self.ruler_rect.contains(*position) {
                    self.playhead_dragging = true;
                    ctx.request_pointer_capture(self.id);
                    self.seek_from_input(self.x_to_frame(position.x), ctx);
                    return EventResult::Handled;
                }
                if let Some(clip_ref) = self.hit_clip(*position) {
                    let result = self.select_clip_from_input(clip_ref, ctx);
                    self.start_clip_drag(clip_ref, *position);
                    if self.clip_drag.is_some() {
                        ctx.request_pointer_capture(self.id);
                    }
                    return result;
                }
                if self.body_rect.contains(*position) {
                    self.seek_from_input(self.x_to_frame(position.x), ctx);
                    return EventResult::Handled;
                }
            }
            UiEvent::MouseMove { position, .. } => {
                if self.playhead_dragging {
                    self.seek_from_input(self.x_to_frame(position.x), ctx);
                    return EventResult::Handled;
                }
                if self.clip_drag.is_some() {
                    self.drag_clip_to(*position, ctx);
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
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.clip_drag.is_some() => {
                self.finish_clip_drag(ctx);
                ctx.release_pointer_capture(self.id);
                return EventResult::Handled;
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
        ctx.encoder.draw_rect(
            Rect::new(
                self.bounds.x,
                self.bounds.y,
                self.header_width,
                self.ruler_height,
            ),
            colors.card,
            0.0,
        );

        ctx.encoder.push_clip(self.ruler_rect);
        self.paint_ruler(ctx);
        ctx.encoder.pop_clip();

        ctx.encoder.push_clip(self.body_rect);
        self.paint_tracks(ctx);
        ctx.encoder.pop_clip();

        ctx.encoder.push_clip(Rect::new(
            self.ruler_rect.x,
            self.ruler_rect.y,
            self.ruler_rect.width,
            self.ruler_rect.height + self.body_rect.height,
        ));
        self.paint_playhead(ctx);
        ctx.encoder.pop_clip();
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;

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
    fn dragging_clip_moves_view_model_and_commits_move_on_release() {
        let moves = RefCell::new(Vec::new());
        let dispatch = |action| moves.borrow_mut().push(action);
        let mut view = TimelineView::new(vec![TimelineTrack::video(
            "V1",
            vec![TimelineClip::new("Intro", 0, 24)],
        )])
        .on_clip_move(|movement, _clip| Action::Custom {
            namespace: "timeline.move".into(),
            name: format!(
                "{}:{}->{}",
                movement.clip_ref.track_index, movement.old_start_frame, movement.new_start_frame
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
        assert_eq!(view.tracks[0].clips[0].start_frame, 10);
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
                name: "0:0->10".into(),
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
        assert_eq!(encoder.triangles, 1);
        assert!(encoder.clips >= 3);
        assert!(encoder.texts.iter().any(|text| text == "V1"));
        assert!(encoder.texts.iter().any(|text| text == "Intro"));
    }
}
