//! Domain-light viewer surface for editor preview panels.
//!
//! The widget paints preview chrome, aspect-ratio fitting, an optional raster
//! preview frame, safe-area guides, and status metadata. App/runtime layers own
//! preview decoding and pass already-renderable frame images across this
//! domain-light boundary.

use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use std::sync::Arc;

use crate::paint::{
    centered_text_origin_y, color_with_alpha, horizontal_stroke_rect, mix_color, paint_focus_ring,
    soft_border, vertical_stroke_rect,
};
use crate::text_metrics::measure_single_line;
use crate::{RasterImage, VectorIcon};

const DEFAULT_WIDTH: f32 = 480.0;
const DEFAULT_HEIGHT: f32 = 270.0;
const TRANSPORT_BUTTON_SIZE: f32 = 28.0;
const TRANSPORT_BUTTON_GAP: f32 = 6.0;
const CHECKER_TILE_SIZE: f32 = 16.0;
const VIEWER_DROPDOWN_ROW_HEIGHT: f32 = 24.0;
const VIEWER_DROPDOWN_PAD_X: f32 = 5.0;
const VIEWER_DROPDOWN_PAD_Y: f32 = 5.0;

/// RGBA preview image presented by [`ViewerSurface`].
pub type ViewerFrameImage = RasterImage;

/// Semantic tone for the viewer status badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerStatusTone {
    /// Neutral idle/ready state.
    Neutral,
    /// Active playback or focused preview state.
    Accent,
    /// Successful/healthy state.
    Success,
    /// Attention-needed preview state.
    Warning,
    /// Error/unavailable preview state.
    Error,
}

/// Transport or mark control rendered by [`ViewerSurface`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerControl {
    /// Mark an in point at the current playhead.
    MarkIn,
    /// Mark an out point at the current playhead.
    MarkOut,
    /// Jump to the first frame of the active sequence.
    JumpStart,
    /// Step one frame backward.
    StepBack,
    /// Toggle preview playback.
    PlayPause,
    /// Step one frame forward.
    StepForward,
    /// Jump to the last content frame of the active sequence.
    JumpEnd,
}

/// Maps a viewer chrome control to an editor action.
pub type ViewerControlAction = dyn Fn(ViewerControl) -> Action;

/// Maps a viewer preview-quality chip activation to an editor action.
pub type ViewerPreviewQualityAction = dyn Fn(f32) -> Action;

/// Maps a viewer zoom chip activation to an editor action.
pub type ViewerZoomAction = dyn Fn(Option<f32>) -> Action;

/// One selectable viewer zoom mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewerZoomOption {
    /// User-facing label.
    pub label: &'static str,
    /// Fixed canvas scale. `None` means fit to available viewer space.
    pub scale: Option<f32>,
}

/// One selectable preview resolution mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewerPreviewQualityOption {
    /// User-facing label.
    pub label: &'static str,
    /// Preview render resolution scale.
    pub scale: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewerDropdown {
    Zoom,
    PreviewQuality,
}

/// Preview viewer surface.
pub struct ViewerSurface {
    id: WidgetId,
    bounds: Rect,
    status: String,
    status_tone: ViewerStatusTone,
    resolution_label: String,
    timecode_label: String,
    frame_label: String,
    duration_label: String,
    zoom_label: String,
    zoom_scale: Option<f32>,
    preview_quality_label: String,
    source_width: u32,
    source_height: u32,
    playing: bool,
    enabled: bool,
    frame_image: Option<ViewerFrameImage>,
    empty_message: Option<String>,
    hovered_control: Option<ViewerControl>,
    pressed_control: Option<ViewerControl>,
    hovered_zoom: bool,
    pressed_zoom: bool,
    hovered_preview_quality: bool,
    pressed_preview_quality: bool,
    open_dropdown: Option<ViewerDropdown>,
    hovered_dropdown_index: Option<usize>,
    pressed_dropdown_index: Option<usize>,
    focused: bool,
    focus_visible: bool,
    on_control: Option<Box<ViewerControlAction>>,
    on_zoom: Option<Box<ViewerZoomAction>>,
    on_preview_quality: Option<Box<ViewerPreviewQualityAction>>,
    control_icons: Vec<(ViewerControl, VectorIcon)>,
    play_pause_icon: Option<VectorIcon>,
    playing_pause_icon: Option<VectorIcon>,
}

impl ViewerSurface {
    /// Create a viewer surface with a title and source dimensions.
    pub fn new(title: impl Into<String>, source_width: u32, source_height: u32) -> Self {
        let _ = title.into();
        Self {
            id: WidgetId::new(),
            bounds: Rect::ZERO,
            status: "无信号".into(),
            status_tone: ViewerStatusTone::Neutral,
            resolution_label: String::new(),
            timecode_label: "00:00:00:00".into(),
            frame_label: "F0".into(),
            duration_label: String::new(),
            zoom_label: "适合".into(),
            zoom_scale: None,
            preview_quality_label: "1/1".into(),
            source_width: source_width.max(1),
            source_height: source_height.max(1),
            playing: false,
            enabled: true,
            frame_image: None,
            empty_message: None,
            hovered_control: None,
            pressed_control: None,
            hovered_zoom: false,
            pressed_zoom: false,
            hovered_preview_quality: false,
            pressed_preview_quality: false,
            open_dropdown: None,
            hovered_dropdown_index: None,
            pressed_dropdown_index: None,
            focused: false,
            focus_visible: false,
            on_control: None,
            on_zoom: None,
            on_preview_quality: None,
            control_icons: Vec::new(),
            play_pause_icon: None,
            playing_pause_icon: None,
        }
    }

    /// Set the viewer status label.
    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = status.into();
        self
    }

    /// Set the semantic tone used for the viewer status badge.
    pub fn with_status_tone(mut self, tone: ViewerStatusTone) -> Self {
        self.status_tone = tone;
        self
    }

    /// Set the formatted source resolution label.
    pub fn with_resolution_label(mut self, label: impl Into<String>) -> Self {
        self.resolution_label = label.into();
        self
    }

    /// Set the SMPTE-style current timecode label.
    pub fn with_timecode_label(mut self, label: impl Into<String>) -> Self {
        self.timecode_label = label.into();
        self
    }

    /// Set the formatted current frame label.
    pub fn with_frame_label(mut self, label: impl Into<String>) -> Self {
        self.frame_label = label.into();
        self
    }

    /// Set the formatted duration label.
    pub fn with_duration_label(mut self, label: impl Into<String>) -> Self {
        self.duration_label = label.into();
        self
    }

    /// Set the displayed canvas zoom mode label.
    pub fn with_zoom_label(mut self, label: impl Into<String>) -> Self {
        self.zoom_label = label.into();
        self
    }

    /// Set a fixed canvas zoom scale, or `None` to fit the source in view.
    pub fn with_zoom_scale(mut self, scale: Option<f32>) -> Self {
        self.zoom_scale = scale
            .filter(|scale| scale.is_finite() && *scale > 0.0)
            .map(|scale| scale.clamp(0.01, 32.0));
        self
    }

    /// Set the displayed preview quality / resolution mode label.
    pub fn with_preview_quality_label(mut self, label: impl Into<String>) -> Self {
        self.preview_quality_label = label.into();
        self
    }

    /// Set whether playback is active.
    pub fn playing(mut self, playing: bool) -> Self {
        self.playing = playing;
        if playing && self.status_tone == ViewerStatusTone::Neutral {
            self.status_tone = ViewerStatusTone::Accent;
        }
        self
    }

    /// Set whether the surface represents an available preview target.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        if !enabled {
            self.clear_interaction_state();
        }
        self
    }

    /// Mark the surface as unavailable.
    pub fn disabled(self) -> Self {
        self.enabled(false)
    }

    /// Set the rendered preview image shown inside the fitted canvas.
    pub fn with_frame_image(mut self, frame_image: ViewerFrameImage) -> Self {
        self.frame_image = Some(frame_image);
        self
    }

    /// Set a short message painted inside the canvas when no frame is shown.
    pub fn with_empty_message(mut self, message: impl Into<String>) -> Self {
        self.empty_message = Some(message.into());
        self
    }

    /// Set a custom action mapper for viewer controls.
    pub fn on_control(mut self, action: impl Fn(ViewerControl) -> Action + 'static) -> Self {
        self.on_control = Some(Box::new(action));
        self
    }

    /// Set a custom action for selecting a preview-quality option.
    pub fn on_preview_quality(mut self, action: impl Fn(f32) -> Action + 'static) -> Self {
        self.on_preview_quality = Some(Box::new(action));
        self
    }

    /// Set a custom action for selecting a viewer zoom option.
    pub fn on_zoom(mut self, action: impl Fn(Option<f32>) -> Action + 'static) -> Self {
        self.on_zoom = Some(Box::new(action));
        self
    }

    /// Set a vector icon used to paint one transport control.
    pub fn with_control_icon(mut self, control: ViewerControl, icon: VectorIcon) -> Self {
        if let Some((_, existing)) =
            self.control_icons.iter_mut().find(|(candidate, _)| *candidate == control)
        {
            *existing = icon;
        } else {
            self.control_icons.push((control, icon));
        }
        self
    }

    /// Set the vector icons used by the playback toggle in paused/playing states.
    pub fn with_play_pause_icons(mut self, play: VectorIcon, pause: VectorIcon) -> Self {
        self.play_pause_icon = Some(play);
        self.playing_pause_icon = Some(pause);
        self
    }

    /// Whether the surface represents an available preview target.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn aspect_ratio(&self) -> f32 {
        (self.source_width as f32 / self.source_height as f32).clamp(0.1, 10.0)
    }

    fn canvas_viewport_rect(&self) -> Rect {
        let chrome_top = 14.0;
        let chrome_bottom = 44.0;
        let padding = 16.0;
        Rect::new(
            self.bounds.x + padding,
            self.bounds.y + chrome_top,
            (self.bounds.width - padding * 2.0).max(0.0),
            (self.bounds.height - chrome_top - chrome_bottom).max(0.0),
        )
    }

    fn canvas_rect(&self) -> Rect {
        let available = self.canvas_viewport_rect();
        if let Some(scale) = self.zoom_scale {
            let width = self.source_width as f32 * scale;
            let height = self.source_height as f32 * scale;
            return Rect::new(
                available.x + (available.width - width) * 0.5,
                available.y + (available.height - height) * 0.5,
                width.max(0.0),
                height.max(0.0),
            );
        }
        fit_aspect(available, self.aspect_ratio())
    }

    fn metadata_text(&self) -> String {
        self.timecode_label.clone()
    }

    fn should_paint_status_badge(&self) -> bool {
        matches!(
            self.status_tone,
            ViewerStatusTone::Warning | ViewerStatusTone::Error
        ) && !self.status.is_empty()
    }

    fn control_strip_rect(&self) -> Rect {
        let controls = self.visible_controls();
        let width = controls.iter().map(|control| self.control_width(*control)).sum::<f32>()
            + TRANSPORT_BUTTON_GAP * (controls.len().saturating_sub(1) as f32);
        Rect::new(
            self.bounds.x + (self.bounds.width - width) * 0.5,
            self.bounds.y + self.bounds.height - 34.0,
            width,
            TRANSPORT_BUTTON_SIZE,
        )
    }

    fn visible_controls(&self) -> &'static [ViewerControl] {
        if self.bounds.width >= 236.0 {
            &FULL_VIEWER_CONTROLS
        } else if self.bounds.width >= 180.0 {
            &JUMP_VIEWER_CONTROLS
        } else if self.bounds.width >= 116.0 {
            &BASIC_VIEWER_CONTROLS
        } else {
            &MINIMAL_VIEWER_CONTROLS
        }
    }

    fn control_width(&self, control: ViewerControl) -> f32 {
        match control {
            ViewerControl::MarkIn
            | ViewerControl::MarkOut
            | ViewerControl::JumpStart
            | ViewerControl::StepBack
            | ViewerControl::PlayPause
            | ViewerControl::StepForward
            | ViewerControl::JumpEnd => TRANSPORT_BUTTON_SIZE,
        }
    }

    fn control_rect(&self, control: ViewerControl) -> Rect {
        let strip = self.control_strip_rect();
        let mut x = strip.x;
        for &candidate in self.visible_controls() {
            let width = self.control_width(candidate);
            if candidate == control {
                return Rect::new(x, strip.y, width, TRANSPORT_BUTTON_SIZE);
            }
            x += width + TRANSPORT_BUTTON_GAP;
        }
        Rect::ZERO
    }

    fn control_at(&self, point: Point) -> Option<ViewerControl> {
        self.visible_controls()
            .iter()
            .copied()
            .find(|control| self.control_rect(*control).contains(point))
    }

    fn preview_quality_rect(&self) -> Rect {
        let control_strip = self.control_strip_rect();
        let left = control_strip.x + control_strip.width + 12.0;
        let right = self.bounds.x + self.bounds.width - 14.0;
        let available = right - left;
        if available < 48.0 {
            return Rect::ZERO;
        }
        let wanted =
            (self.preview_quality_label.chars().count() as f32 * 7.0 + 28.0).clamp(50.0, 86.0);
        let width = wanted.min(available);
        Rect::new(
            right - width,
            self.bounds.y + self.bounds.height - 29.0,
            width,
            22.0,
        )
    }

    fn zoom_rect(&self) -> Rect {
        let quality = self.preview_quality_rect();
        if quality.width <= 0.0 {
            return Rect::ZERO;
        }
        let control_strip = self.control_strip_rect();
        let left = control_strip.x + control_strip.width + 12.0;
        let right = quality.x - 6.0;
        let available = right - left;
        if available < 42.0 {
            return Rect::ZERO;
        }
        let wanted = (self.zoom_label.chars().count() as f32 * 7.0 + 28.0).clamp(50.0, 78.0);
        let width = wanted.min(available);
        Rect::new(right - width, quality.y, width, quality.height)
    }

    fn preview_quality_at(&self, point: Point) -> bool {
        let rect = self.preview_quality_rect();
        rect.width > 0.0 && rect.height > 0.0 && rect.contains(point)
    }

    fn zoom_at(&self, point: Point) -> bool {
        let rect = self.zoom_rect();
        rect.width > 0.0 && rect.height > 0.0 && rect.contains(point)
    }

    fn dropdown_anchor_rect(&self, dropdown: ViewerDropdown) -> Rect {
        match dropdown {
            ViewerDropdown::Zoom => self.zoom_rect(),
            ViewerDropdown::PreviewQuality => self.preview_quality_rect(),
        }
    }

    fn dropdown_options_len(dropdown: ViewerDropdown) -> usize {
        match dropdown {
            ViewerDropdown::Zoom => VIEWER_ZOOM_OPTIONS.len(),
            ViewerDropdown::PreviewQuality => VIEWER_PREVIEW_QUALITY_OPTIONS.len(),
        }
    }

    fn dropdown_rect(&self, dropdown: ViewerDropdown) -> Rect {
        let anchor = self.dropdown_anchor_rect(dropdown);
        if anchor.width <= 0.0 || anchor.height <= 0.0 {
            return Rect::ZERO;
        }
        let row_count = Self::dropdown_options_len(dropdown) as f32;
        let width: f32 = match dropdown {
            ViewerDropdown::Zoom => 92.0,
            ViewerDropdown::PreviewQuality => 74.0,
        };
        let height = row_count * VIEWER_DROPDOWN_ROW_HEIGHT + VIEWER_DROPDOWN_PAD_Y * 2.0;
        let min_x = self.bounds.x + 8.0;
        let max_x = (self.bounds.x + self.bounds.width - width - 8.0).max(min_x);
        let x = (anchor.x + anchor.width - width).clamp(min_x, max_x);
        let y = (anchor.y - height - 6.0).max(self.bounds.y + 8.0);
        Rect::new(x, y, width, height)
    }

    fn dropdown_row_rect(&self, dropdown: ViewerDropdown, index: usize) -> Rect {
        let menu = self.dropdown_rect(dropdown);
        Rect::new(
            menu.x + VIEWER_DROPDOWN_PAD_X,
            menu.y + VIEWER_DROPDOWN_PAD_Y + index as f32 * VIEWER_DROPDOWN_ROW_HEIGHT,
            (menu.width - VIEWER_DROPDOWN_PAD_X * 2.0).max(0.0),
            VIEWER_DROPDOWN_ROW_HEIGHT,
        )
    }

    fn dropdown_item_at(&self, point: Point) -> Option<(ViewerDropdown, usize)> {
        let dropdown = self.open_dropdown?;
        let menu = self.dropdown_rect(dropdown);
        if !menu.contains(point) {
            return None;
        }
        (0..Self::dropdown_options_len(dropdown))
            .find(|index| self.dropdown_row_rect(dropdown, *index).contains(point))
            .map(|index| (dropdown, index))
    }

    fn dispatch_control(&self, control: ViewerControl, ctx: &mut EventContext) {
        let action = self
            .on_control
            .as_ref()
            .map(|mapper| mapper(control))
            .unwrap_or_else(|| default_viewer_control_action(control));
        (ctx.dispatch)(action);
    }

    fn dispatch_preview_quality(&self, scale: f32, ctx: &mut EventContext) {
        if let Some(mapper) = &self.on_preview_quality {
            (ctx.dispatch)(mapper(scale));
        }
    }

    fn dispatch_zoom(&self, scale: Option<f32>, ctx: &mut EventContext) {
        if let Some(mapper) = &self.on_zoom {
            (ctx.dispatch)(mapper(scale));
        }
    }

    fn control_icon(&self, control: ViewerControl) -> Option<&VectorIcon> {
        self.control_icons
            .iter()
            .find_map(|(candidate, icon)| (*candidate == control).then_some(icon))
    }

    fn focus_from_pointer(&mut self, ctx: &mut EventContext) {
        self.focused = true;
        self.focus_visible = false;
        ctx.focus.request_focus(self.id);
    }

    fn keyboard_control(&self, key: KeyCode, modifiers: Modifiers) -> Option<ViewerControl> {
        if modifiers != Modifiers::none() {
            return None;
        }
        match key {
            KeyCode::Space => Some(ViewerControl::PlayPause),
            KeyCode::Left => Some(ViewerControl::StepBack),
            KeyCode::Right => Some(ViewerControl::StepForward),
            KeyCode::Home => Some(ViewerControl::JumpStart),
            KeyCode::End => Some(ViewerControl::JumpEnd),
            KeyCode::I => Some(ViewerControl::MarkIn),
            KeyCode::O => Some(ViewerControl::MarkOut),
            _ => None,
        }
    }

    fn clear_interaction_state(&mut self) -> bool {
        let changed = self.hovered_control.is_some()
            || self.pressed_control.is_some()
            || self.hovered_zoom
            || self.pressed_zoom
            || self.hovered_preview_quality
            || self.pressed_preview_quality
            || self.open_dropdown.is_some()
            || self.hovered_dropdown_index.is_some()
            || self.pressed_dropdown_index.is_some()
            || self.focused
            || self.focus_visible;
        self.hovered_control = None;
        self.pressed_control = None;
        self.hovered_zoom = false;
        self.pressed_zoom = false;
        self.hovered_preview_quality = false;
        self.pressed_preview_quality = false;
        self.open_dropdown = None;
        self.hovered_dropdown_index = None;
        self.pressed_dropdown_index = None;
        self.focused = false;
        self.focus_visible = false;
        changed
    }
}

impl Widget for ViewerSurface {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(DEFAULT_WIDTH, DEFAULT_HEIGHT))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.enabled {
            if self.clear_interaction_state() {
                ctx.request_repaint();
            }
            return EventResult::Ignored;
        }

        match event {
            UiEvent::MouseMove { position, .. } => {
                let hovered = self.control_at(*position);
                let hovered_zoom = self.zoom_at(*position);
                let hovered_preview_quality = self.preview_quality_at(*position);
                let hovered_dropdown_index =
                    self.dropdown_item_at(*position).map(|(_, index)| index);
                if hovered != self.hovered_control
                    || hovered_zoom != self.hovered_zoom
                    || hovered_preview_quality != self.hovered_preview_quality
                    || hovered_dropdown_index != self.hovered_dropdown_index
                {
                    self.hovered_control = hovered;
                    self.hovered_zoom = hovered_zoom;
                    self.hovered_preview_quality = hovered_preview_quality;
                    self.hovered_dropdown_index = hovered_dropdown_index;
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                if hovered.is_some()
                    || hovered_zoom
                    || hovered_preview_quality
                    || hovered_dropdown_index.is_some()
                {
                    EventResult::Handled
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                if !self.bounds.contains(*position) {
                    if self.open_dropdown.is_some() {
                        self.open_dropdown = None;
                        self.hovered_dropdown_index = None;
                        self.pressed_dropdown_index = None;
                        ctx.request_repaint();
                    }
                    return EventResult::Ignored;
                }
                self.focus_from_pointer(ctx);
                if let Some((dropdown, index)) = self.dropdown_item_at(*position) {
                    self.open_dropdown = Some(dropdown);
                    self.hovered_dropdown_index = Some(index);
                    self.pressed_dropdown_index = Some(index);
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                if let Some(control) = self.control_at(*position) {
                    self.open_dropdown = None;
                    self.hovered_dropdown_index = None;
                    self.pressed_dropdown_index = None;
                    self.pressed_control = Some(control);
                    self.hovered_control = Some(control);
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                if self.zoom_at(*position) {
                    self.pressed_zoom = true;
                    self.hovered_zoom = true;
                    self.open_dropdown = None;
                    self.hovered_dropdown_index = None;
                    self.pressed_dropdown_index = None;
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                if self.preview_quality_at(*position) {
                    self.pressed_preview_quality = true;
                    self.hovered_preview_quality = true;
                    self.open_dropdown = None;
                    self.hovered_dropdown_index = None;
                    self.pressed_dropdown_index = None;
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                if self.open_dropdown.is_some() {
                    self.open_dropdown = None;
                    self.hovered_dropdown_index = None;
                    self.pressed_dropdown_index = None;
                    ctx.request_repaint();
                }
                EventResult::Handled
            }
            UiEvent::FocusGained => {
                self.focused = true;
                self.focus_visible = true;
                EventResult::Handled
            }
            UiEvent::MouseUp { position, button: MouseButton::Left, .. } => {
                let pressed_dropdown_index = self.pressed_dropdown_index.take();
                if let Some((dropdown, hovered_index)) = self.dropdown_item_at(*position) {
                    if pressed_dropdown_index == Some(hovered_index)
                        && self.open_dropdown == Some(dropdown)
                    {
                        match dropdown {
                            ViewerDropdown::Zoom => {
                                let option = VIEWER_ZOOM_OPTIONS[hovered_index];
                                self.dispatch_zoom(option.scale, ctx);
                            }
                            ViewerDropdown::PreviewQuality => {
                                let option = VIEWER_PREVIEW_QUALITY_OPTIONS[hovered_index];
                                self.dispatch_preview_quality(option.scale, ctx);
                            }
                        }
                        self.open_dropdown = None;
                        self.hovered_dropdown_index = None;
                        ctx.request_repaint();
                        return EventResult::Handled;
                    }
                }
                let pressed = self.pressed_control.take();
                let hovered = self.control_at(*position);
                self.hovered_control = hovered;
                if let Some(control) = pressed {
                    if hovered == Some(control) {
                        self.dispatch_control(control, ctx);
                    }
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                let pressed_zoom = self.pressed_zoom;
                self.pressed_zoom = false;
                self.hovered_zoom = self.zoom_at(*position);
                if pressed_zoom {
                    if self.hovered_zoom {
                        self.open_dropdown = Some(ViewerDropdown::Zoom);
                        self.hovered_dropdown_index = None;
                    }
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                let pressed_preview_quality = self.pressed_preview_quality;
                self.pressed_preview_quality = false;
                self.hovered_preview_quality = self.preview_quality_at(*position);
                if pressed_preview_quality {
                    if self.hovered_preview_quality {
                        self.open_dropdown = Some(ViewerDropdown::PreviewQuality);
                        self.hovered_dropdown_index = None;
                    }
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            UiEvent::FocusLost => {
                if self.clear_interaction_state() {
                    ctx.request_repaint();
                }
                EventResult::Handled
            }
            UiEvent::KeyDown { key, modifiers } if self.focused => {
                if let Some(control) = self.keyboard_control(*key, *modifiers) {
                    self.dispatch_control(control, ctx);
                    EventResult::Handled
                } else {
                    EventResult::Ignored
                }
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let typography = &ctx.theme.typography;
        let viewport = self.canvas_viewport_rect();
        let canvas = self.canvas_rect();

        ctx.encoder.draw_rect(self.bounds, colors.viewer_panel, 0.0);
        if self.focus_visible {
            paint_focus_ring(ctx, self.bounds.inset(2.0, 2.0), spacing.radius_lg);
        }
        if self.should_paint_status_badge() {
            let status_width = (measure_single_line(&self.status, typography.small.font_size).0
                + 24.0)
                .clamp(64.0, (self.bounds.width - 32.0).max(64.0));
            let badge = Rect::new(
                self.bounds.x + self.bounds.width - status_width - 16.0,
                self.bounds.y + 9.0,
                status_width,
                24.0,
            );
            let (badge_fill, badge_text) = status_badge_colors(self, ctx);
            ctx.encoder.draw_rect(badge, soft_border(colors.border), spacing.radius_sm);
            ctx.encoder
                .draw_rect(badge.inset(1.0, 1.0), badge_fill, spacing.radius_sm - 1.0);
            ctx.push_clip(badge.inset(4.0, 0.0));
            ctx.encoder.draw_text(
                &self.status,
                typography.small.font_size,
                Point::new(badge.x + 8.0, badge.y + 5.0),
                badge_text,
            );
            ctx.pop_clip();
        }

        ctx.push_clip(viewport);
        ctx.encoder.draw_rect(viewport, colors.viewer_stage, 0.0);
        ctx.encoder.draw_rect(
            canvas.inset(-1.0, -1.0),
            color_with_alpha(colors.foreground, 0.08),
            0.0,
        );
        ctx.push_clip(canvas);
        ctx.encoder.draw_rect(canvas, colors.canvas, 0.0);
        if self.enabled {
            if let Some(frame) = &self.frame_image {
                paint_checkerboard(ctx, canvas);
                ctx.encoder.draw_raster_image(
                    &frame.key,
                    canvas,
                    frame.width,
                    frame.height,
                    Arc::clone(&frame.rgba),
                    Color::WHITE,
                );
            }
        }
        if self.frame_image.is_none() {
            if let Some(message) =
                self.empty_message.as_deref().filter(|message| !message.is_empty())
            {
                let message_rect = Rect::new(
                    canvas.x + 12.0,
                    canvas.y + (canvas.height - 22.0) * 0.5,
                    (canvas.width - 24.0).max(1.0),
                    22.0,
                );
                let mut muted = colors.muted_foreground;
                muted.a *= 0.72;
                ctx.encoder.draw_text_box(
                    message,
                    typography.body.font_size,
                    Point::new(message_rect.x, message_rect.y),
                    message_rect.width,
                    muted,
                );
            }
        }
        paint_safe_guides(ctx, canvas, self.enabled);
        ctx.pop_clip();
        ctx.pop_clip();

        self.paint_transport_controls(ctx);

        let metadata = self.metadata_text();
        let control_strip = self.control_strip_rect();
        ctx.encoder.draw_text_box(
            &metadata,
            typography.small.font_size,
            Point::new(
                self.bounds.x + 14.0,
                self.bounds.y + self.bounds.height - 24.0,
            ),
            (control_strip.x - self.bounds.x - 28.0).max(1.0),
            colors.muted_foreground,
        );

        self.paint_chrome_chip(
            ctx,
            self.zoom_rect(),
            &self.zoom_label,
            self.hovered_zoom,
            self.pressed_zoom,
            self.open_dropdown == Some(ViewerDropdown::Zoom),
        );
        self.paint_chrome_chip(
            ctx,
            self.preview_quality_rect(),
            &self.preview_quality_label,
            self.hovered_preview_quality,
            self.pressed_preview_quality,
            self.open_dropdown == Some(ViewerDropdown::PreviewQuality),
        );
        self.paint_open_dropdown(ctx);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn can_focus(&self) -> bool {
        self.enabled
    }
}

impl ViewerSurface {
    fn paint_transport_controls(&self, ctx: &mut PaintContext) {
        for &control in self.visible_controls() {
            self.paint_transport_control(ctx, control);
        }
    }

    fn paint_transport_control(&self, ctx: &mut PaintContext, control: ViewerControl) {
        let colors = &ctx.theme.colors;
        let radius = ctx.theme.spacing.radius_sm;
        let rect = self.control_rect(control);
        let pressed = self.pressed_control == Some(control);
        let hovered = self.hovered_control == Some(control);
        let active = control == ViewerControl::PlayPause && self.playing;
        let bg = if !self.enabled {
            color_with_alpha(colors.surface, 0.30)
        } else if pressed {
            color_with_alpha(colors.foreground, 0.11)
        } else if active {
            color_with_alpha(colors.primary, 0.16)
        } else if hovered {
            color_with_alpha(colors.foreground, 0.07)
        } else {
            Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }
        };
        let icon = if self.enabled {
            if hovered || active || control == ViewerControl::PlayPause {
                colors.foreground
            } else {
                colors.text_secondary
            }
        } else {
            colors.text_disabled
        };

        ctx.encoder.draw_rect(rect, bg, radius.max(7.0));
        let vector_icon = if control == ViewerControl::PlayPause && self.playing {
            self.playing_pause_icon.as_ref()
        } else if control == ViewerControl::PlayPause {
            self.play_pause_icon.as_ref().or_else(|| self.control_icon(control))
        } else {
            self.control_icon(control)
        };
        if let Some(vector_icon) = vector_icon {
            let icon_size = 14.0_f32.min(rect.width - 6.0).min(rect.height - 6.0).max(1.0);
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
            ViewerControl::MarkIn => {
                paint_mark_in_icon(ctx, rect, icon);
            }
            ViewerControl::MarkOut => {
                paint_mark_out_icon(ctx, rect, icon);
            }
            ViewerControl::JumpStart => {
                ctx.encoder.draw_rect(
                    Rect::new(rect.x + 7.0, rect.y + 7.0, 2.0, 12.0),
                    color_with_alpha(icon, 0.9),
                    1.0,
                );
                paint_left_triangle(ctx, rect, 10.0, icon);
            }
            ViewerControl::StepBack => {
                paint_left_triangle(ctx, rect, 8.0, icon);
            }
            ViewerControl::PlayPause if self.playing => {
                ctx.encoder
                    .draw_rect(Rect::new(rect.x + 8.0, rect.y + 7.0, 3.0, 12.0), icon, 1.0);
                ctx.encoder
                    .draw_rect(Rect::new(rect.x + 15.0, rect.y + 7.0, 3.0, 12.0), icon, 1.0);
            }
            ViewerControl::PlayPause => {
                ctx.encoder.draw_triangles(
                    &[
                        Point::new(rect.x + 10.0, rect.y + 6.0),
                        Point::new(rect.x + 10.0, rect.y + 20.0),
                        Point::new(rect.x + 19.0, rect.y + 13.0),
                    ],
                    icon,
                );
            }
            ViewerControl::StepForward => {
                paint_right_triangle(ctx, rect, 8.0, icon);
            }
            ViewerControl::JumpEnd => {
                paint_right_triangle(ctx, rect, 8.0, icon);
                ctx.encoder.draw_rect(
                    Rect::new(rect.x + 17.0, rect.y + 7.0, 2.0, 12.0),
                    color_with_alpha(icon, 0.9),
                    1.0,
                );
            }
        }
    }

    fn paint_chrome_chip(
        &self,
        ctx: &mut PaintContext,
        rect: Rect,
        label: &str,
        hovered: bool,
        pressed: bool,
        open: bool,
    ) {
        if rect.width <= 0.0 || rect.height <= 0.0 || label.is_empty() {
            return;
        }
        let colors = &ctx.theme.colors;
        let fill = if pressed {
            colors.muted
        } else if hovered || open {
            colors.surface_2
        } else {
            colors.surface
        };
        ctx.encoder.draw_rect(
            rect,
            soft_border(colors.border),
            ctx.theme.spacing.radius_sm,
        );
        ctx.encoder.draw_rect(
            rect.inset(1.0, 1.0),
            fill,
            ctx.theme.spacing.radius_sm - 1.0,
        );
        let style = &ctx.theme.typography.small;
        let label_width = measure_single_line(label, style.font_size).0;
        let caret_width = 6.0;
        let caret_gap = 6.0;
        let content_width =
            (label_width + caret_gap + caret_width).min((rect.width - 12.0).max(1.0));
        let content_x = rect.x + (rect.width - content_width).max(0.0) * 0.5;
        let text_color = if hovered || open {
            colors.foreground
        } else {
            colors.muted_foreground
        };
        ctx.push_clip(rect.inset(6.0, 0.0));
        ctx.encoder.draw_text(
            label,
            style.font_size,
            Point::new(content_x, centered_text_origin_y(rect, style.line_height)),
            text_color,
        );
        let caret_x = content_x + label_width + caret_gap;
        let caret_y = rect.y + rect.height * 0.5 + if open { -1.0 } else { 1.0 };
        let caret = if open {
            [
                Point::new(caret_x, caret_y + 2.0),
                Point::new(caret_x + caret_width, caret_y + 2.0),
                Point::new(caret_x + caret_width * 0.5, caret_y - 2.0),
            ]
        } else {
            [
                Point::new(caret_x, caret_y - 2.0),
                Point::new(caret_x + caret_width, caret_y - 2.0),
                Point::new(caret_x + caret_width * 0.5, caret_y + 2.0),
            ]
        };
        ctx.encoder.draw_triangles(&caret, color_with_alpha(text_color, 0.86));
        ctx.pop_clip();
    }

    fn paint_open_dropdown(&self, ctx: &mut PaintContext) {
        let Some(dropdown) = self.open_dropdown else {
            return;
        };
        let menu = self.dropdown_rect(dropdown);
        if menu.width <= 0.0 || menu.height <= 0.0 {
            return;
        }
        let colors = &ctx.theme.colors;
        let radius = ctx.theme.spacing.radius_md;
        ctx.encoder.draw_rect(menu, soft_border(colors.border), radius);
        ctx.encoder.draw_rect(menu.inset(1.0, 1.0), colors.popover, radius - 1.0);
        for index in 0..Self::dropdown_options_len(dropdown) {
            let row = self.dropdown_row_rect(dropdown, index);
            let hovered = self.hovered_dropdown_index == Some(index);
            if hovered {
                ctx.encoder.draw_rect(
                    row,
                    color_with_alpha(colors.foreground, 0.07),
                    ctx.theme.spacing.radius_sm,
                );
            }
            let (label, selected) = match dropdown {
                ViewerDropdown::Zoom => {
                    let option = VIEWER_ZOOM_OPTIONS[index];
                    (
                        option.label,
                        viewer_zoom_option_selected(option, self.zoom_scale),
                    )
                }
                ViewerDropdown::PreviewQuality => {
                    let option = VIEWER_PREVIEW_QUALITY_OPTIONS[index];
                    (
                        option.label,
                        (option.label == self.preview_quality_label)
                            || (preview_quality_label_for_scale(option.scale)
                                == self.preview_quality_label),
                    )
                }
            };
            let style = &ctx.theme.typography.small;
            let text_color = if hovered {
                colors.foreground
            } else {
                colors.popover_foreground
            };
            let check_lane = 16.0;
            if selected {
                ctx.encoder.draw_text(
                    "✓",
                    style.font_size,
                    Point::new(row.x + 5.0, centered_text_origin_y(row, style.line_height)),
                    text_color,
                );
            }
            ctx.encoder.draw_text(
                label,
                style.font_size,
                Point::new(
                    row.x + check_lane,
                    centered_text_origin_y(row, style.line_height),
                ),
                text_color,
            );
        }
    }
}

const FULL_VIEWER_CONTROLS: [ViewerControl; 5] = [
    ViewerControl::JumpStart,
    ViewerControl::StepBack,
    ViewerControl::PlayPause,
    ViewerControl::StepForward,
    ViewerControl::JumpEnd,
];

const JUMP_VIEWER_CONTROLS: [ViewerControl; 5] = [
    ViewerControl::JumpStart,
    ViewerControl::StepBack,
    ViewerControl::PlayPause,
    ViewerControl::StepForward,
    ViewerControl::JumpEnd,
];

const BASIC_VIEWER_CONTROLS: [ViewerControl; 3] = [
    ViewerControl::StepBack,
    ViewerControl::PlayPause,
    ViewerControl::StepForward,
];

const MINIMAL_VIEWER_CONTROLS: [ViewerControl; 1] = [ViewerControl::PlayPause];

const VIEWER_ZOOM_OPTIONS: [ViewerZoomOption; 9] = [
    ViewerZoomOption { label: "适合", scale: None },
    ViewerZoomOption { label: "10%", scale: Some(0.10) },
    ViewerZoomOption { label: "25%", scale: Some(0.25) },
    ViewerZoomOption { label: "50%", scale: Some(0.50) },
    ViewerZoomOption { label: "75%", scale: Some(0.75) },
    ViewerZoomOption { label: "100%", scale: Some(1.0) },
    ViewerZoomOption { label: "150%", scale: Some(1.5) },
    ViewerZoomOption { label: "200%", scale: Some(2.0) },
    ViewerZoomOption { label: "400%", scale: Some(4.0) },
];

const VIEWER_PREVIEW_QUALITY_OPTIONS: [ViewerPreviewQualityOption; 4] = [
    ViewerPreviewQualityOption { label: "1/1", scale: 1.0 },
    ViewerPreviewQualityOption { label: "1/2", scale: 0.5 },
    ViewerPreviewQualityOption { label: "1/4", scale: 0.25 },
    ViewerPreviewQualityOption { label: "1/8", scale: 0.125 },
];

fn viewer_zoom_option_selected(option: ViewerZoomOption, current: Option<f32>) -> bool {
    match (option.scale, current) {
        (None, None) => true,
        (Some(a), Some(b)) => (a - b).abs() <= 0.001,
        _ => false,
    }
}

fn preview_quality_label_for_scale(scale: f32) -> &'static str {
    if (scale - 1.0).abs() <= 0.001 {
        "1/1"
    } else if (scale - 0.5).abs() <= 0.001 {
        "1/2"
    } else if (scale - 0.25).abs() <= 0.001 {
        "1/4"
    } else {
        "1/8"
    }
}

fn default_viewer_control_action(control: ViewerControl) -> Action {
    match control {
        ViewerControl::MarkIn => Action::MarkInAtPlayhead,
        ViewerControl::MarkOut => Action::MarkOutAtPlayhead,
        ViewerControl::JumpStart => Action::GoToStart,
        ViewerControl::StepBack => Action::StepBack,
        ViewerControl::PlayPause => Action::TogglePlay,
        ViewerControl::StepForward => Action::StepForward,
        ViewerControl::JumpEnd => Action::GoToEnd,
    }
}

fn paint_left_triangle(ctx: &mut PaintContext, rect: Rect, left: f32, color: Color) {
    ctx.encoder.draw_triangles(
        &[
            Point::new(rect.x + left + 10.0, rect.y + 6.0),
            Point::new(rect.x + left + 10.0, rect.y + 20.0),
            Point::new(rect.x + left, rect.y + 13.0),
        ],
        color,
    );
}

fn paint_right_triangle(ctx: &mut PaintContext, rect: Rect, left: f32, color: Color) {
    ctx.encoder.draw_triangles(
        &[
            Point::new(rect.x + left, rect.y + 6.0),
            Point::new(rect.x + left, rect.y + 20.0),
            Point::new(rect.x + left + 10.0, rect.y + 13.0),
        ],
        color,
    );
}

fn paint_checkerboard(ctx: &mut PaintContext, canvas: Rect) {
    if canvas.width <= 0.0 || canvas.height <= 0.0 {
        return;
    }
    let colors = &ctx.theme.colors;
    let mut dark = colors.checkerboard_dark;
    let mut light = colors.checkerboard_light;
    dark.a *= 0.28;
    light.a *= 0.28;
    ctx.encoder.draw_rect(canvas, dark, 0.0);

    let columns = (canvas.width / CHECKER_TILE_SIZE).ceil().max(1.0) as usize;
    let rows = (canvas.height / CHECKER_TILE_SIZE).ceil().max(1.0) as usize;
    for row in 0..rows {
        for column in 0..columns {
            if (row + column) % 2 == 0 {
                let x = canvas.x + column as f32 * CHECKER_TILE_SIZE;
                let y = canvas.y + row as f32 * CHECKER_TILE_SIZE;
                ctx.encoder.draw_rect(
                    Rect::new(
                        x,
                        y,
                        (canvas.x + canvas.width - x).clamp(0.0, CHECKER_TILE_SIZE),
                        (canvas.y + canvas.height - y).clamp(0.0, CHECKER_TILE_SIZE),
                    ),
                    light,
                    0.0,
                );
            }
        }
    }
}

fn paint_mark_in_icon(ctx: &mut PaintContext, rect: Rect, color: Color) {
    let x = rect.x.round();
    let y = rect.y.round();
    ctx.encoder.draw_rect(Rect::new(x + 7.0, y + 7.0, 2.0, 12.0), color, 1.0);
    ctx.encoder.draw_triangles(
        &[
            Point::new(x + 12.0, y + 8.0),
            Point::new(x + 12.0, y + 18.0),
            Point::new(x + 18.0, y + 13.0),
        ],
        color,
    );
}

fn paint_mark_out_icon(ctx: &mut PaintContext, rect: Rect, color: Color) {
    let x = rect.x.round();
    let y = rect.y.round();
    ctx.encoder.draw_triangles(
        &[
            Point::new(x + 12.0, y + 13.0),
            Point::new(x + 18.0, y + 8.0),
            Point::new(x + 18.0, y + 18.0),
        ],
        color,
    );
    ctx.encoder.draw_rect(Rect::new(x + 20.0, y + 7.0, 2.0, 12.0), color, 1.0);
}

fn fit_aspect(bounds: Rect, aspect: f32) -> Rect {
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return Rect::new(bounds.x, bounds.y, 0.0, 0.0);
    }
    let available_aspect = bounds.width / bounds.height;
    if available_aspect > aspect {
        let width = bounds.height * aspect;
        Rect::new(
            bounds.x + (bounds.width - width) * 0.5,
            bounds.y,
            width,
            bounds.height,
        )
    } else {
        let height = bounds.width / aspect;
        Rect::new(
            bounds.x,
            bounds.y + (bounds.height - height) * 0.5,
            bounds.width,
            height,
        )
    }
}

fn status_badge_colors(surface: &ViewerSurface, ctx: &PaintContext) -> (Color, Color) {
    let colors = &ctx.theme.colors;
    if !surface.enabled {
        return (
            mix_color(colors.popover, colors.muted, 0.28),
            colors.muted_foreground,
        );
    }
    match surface.status_tone {
        ViewerStatusTone::Neutral => (
            mix_color(colors.popover, colors.foreground, 0.045),
            colors.popover_foreground,
        ),
        ViewerStatusTone::Accent => (
            mix_color(colors.popover, colors.primary, 0.18),
            colors.primary,
        ),
        ViewerStatusTone::Success => (color_with_alpha(colors.success, 0.20), colors.success),
        ViewerStatusTone::Warning => (color_with_alpha(colors.warning, 0.20), colors.warning),
        ViewerStatusTone::Error => (color_with_alpha(colors.error, 0.20), colors.error),
    }
}

fn paint_safe_guides(ctx: &mut PaintContext, canvas: Rect, enabled: bool) {
    if canvas.width <= 0.0 || canvas.height <= 0.0 {
        return;
    }
    let colors = &ctx.theme.colors;
    let mut guide = colors.safe_guide;
    let mut inner_guide = colors.safe_guide_inner;
    if !enabled {
        guide.a *= 0.56;
        inner_guide.a *= 0.56;
    }
    let action = canvas.inset(canvas.width * 0.05, canvas.height * 0.05);
    let title = canvas.inset(canvas.width * 0.10, canvas.height * 0.10);
    draw_rect_outline(ctx, action, guide);
    draw_rect_outline(ctx, title, inner_guide);
}

fn draw_rect_outline(ctx: &mut PaintContext, rect: Rect, color: Color) {
    ctx.encoder.draw_rect(
        horizontal_stroke_rect(rect.y, rect.x, rect.width, 1.0),
        color,
        0.0,
    );
    ctx.encoder.draw_rect(
        horizontal_stroke_rect(rect.y + rect.height, rect.x, rect.width, 1.0),
        color,
        0.0,
    );
    ctx.encoder.draw_rect(
        vertical_stroke_rect(rect.x, rect.y, rect.height, 1.0),
        color,
        0.0,
    );
    ctx.encoder.draw_rect(
        vertical_stroke_rect(rect.x + rect.width, rect.y, rect.height, 1.0),
        color,
        0.0,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingEncoder {
        rects: Vec<Rect>,
        rect_colors: Vec<Color>,
        rect_radii: Vec<f32>,
        lines: usize,
        triangles: usize,
        texts: Vec<String>,
        raster_images: Vec<(String, Rect, u32, u32)>,
        clips: Vec<Rect>,
        clip_pops: usize,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, bounds: Rect) {
            self.clips.push(bounds);
        }
        fn pop_clip(&mut self) {
            self.clip_pops += 1;
        }
        fn draw_rect(&mut self, bounds: Rect, color: Color, corner_radius: f32) {
            self.rects.push(bounds);
            self.rect_colors.push(color);
            self.rect_radii.push(corner_radius);
        }
        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {
            self.lines += 1;
        }
        fn draw_triangles(&mut self, vertices: &[Point], _color: Color) {
            self.triangles += vertices.len();
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
        fn draw_raster_image(
            &mut self,
            key: &str,
            bounds: Rect,
            width: u32,
            height: u32,
            _rgba: Arc<[u8]>,
            _tint: Color,
        ) {
            self.raster_images.push((key.to_owned(), bounds, width, height));
        }
        fn push_translate(&mut self, _offset: glam::Vec2) {}
        fn pop_transform(&mut self) {}
    }

    #[test]
    fn canvas_preserves_source_aspect_ratio() {
        let mut viewer = ViewerSurface::new("Demo", 1920, 1080);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));

        let canvas = viewer.canvas_rect();

        assert!((canvas.width / canvas.height - 16.0 / 9.0).abs() < 0.001);
        assert!(canvas.x >= 16.0);
        assert!(canvas.y >= 14.0);
    }

    #[test]
    fn fixed_zoom_uses_source_pixel_scale_inside_viewport() {
        let mut viewer = ViewerSurface::new("Demo", 1920, 1080).with_zoom_scale(Some(0.25));
        viewer.layout(Rect::new(0.0, 0.0, 800.0, 500.0));

        let canvas = viewer.canvas_rect();
        let viewport = viewer.canvas_viewport_rect();

        assert!((canvas.width - 480.0).abs() < 0.001);
        assert!((canvas.height - 270.0).abs() < 0.001);
        assert!((canvas.center().x - viewport.center().x).abs() < 0.001);
        assert!((canvas.center().y - viewport.center().y).abs() < 0.001);
    }

    #[test]
    fn paint_prioritizes_canvas_and_omits_normal_status_chrome() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080)
            .with_status("Playing")
            .with_resolution_label("1920x1080")
            .with_timecode_label("00:00:01:18")
            .with_frame_label("F42")
            .with_duration_label("240 frames")
            .with_zoom_label("适合")
            .with_preview_quality_label("1/1")
            .playing(true);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 500.0, 320.0),
        };

        viewer.paint(&mut ctx);

        assert!(!encoder.texts.iter().any(|text| text == "Scene 01"));
        assert!(!encoder.texts.iter().any(|text| text == "Playing"));
        assert!(encoder.texts.iter().any(|text| text.contains("00:00:01:18")));
        assert!(!encoder.texts.iter().any(|text| text.contains("1920x1080")));
        assert!(!encoder.texts.iter().any(|text| text.contains("F42")));
        assert!(!encoder.texts.iter().any(|text| text.contains("240 frames")));
        assert!(encoder.texts.iter().any(|text| text.contains("适合")));
        assert!(encoder.texts.iter().any(|text| text.contains("1/1")));
        assert!(!encoder
            .rect_colors
            .iter()
            .any(|color| *color == mix_color(theme.colors.popover, theme.colors.primary, 0.18)));
        assert_eq!(encoder.lines, 0);
        assert!(encoder.rects.len() >= 12);
    }

    #[test]
    fn warning_status_badge_can_surface_preview_problems() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080)
            .with_status("Offline")
            .with_status_tone(ViewerStatusTone::Warning);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 500.0, 320.0),
        };

        viewer.paint(&mut ctx);

        assert!(encoder.texts.iter().any(|text| text == "Offline"));
    }

    #[test]
    fn disabled_empty_viewer_paints_empty_message_without_frame() {
        let mut viewer = ViewerSurface::new("Viewer", 16, 9)
            .with_status("No sequence")
            .with_empty_message("未载入序列")
            .disabled();
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 500.0, 320.0),
        };

        viewer.paint(&mut ctx);

        assert!(encoder.raster_images.is_empty());
        assert!(!encoder.texts.iter().any(|text| text == "No sequence"));
        assert!(encoder.texts.iter().any(|text| text == "未载入序列"));
        assert_eq!(encoder.clip_pops, encoder.clips.len());
    }

    #[test]
    fn disabled_builder_marks_surface_unavailable() {
        let viewer = ViewerSurface::new("Offline", 1920, 1080).disabled();

        assert!(!viewer.is_enabled());
    }

    #[test]
    fn transport_controls_dispatch_playback_actions() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        for control in [
            ViewerControl::JumpStart,
            ViewerControl::StepBack,
            ViewerControl::PlayPause,
            ViewerControl::StepForward,
            ViewerControl::JumpEnd,
        ] {
            let position = viewer.control_rect(control).center();
            assert_eq!(
                viewer.event(
                    &UiEvent::MouseDown {
                        position,
                        button: MouseButton::Left,
                        modifiers: Modifiers::none(),
                    },
                    &mut ctx,
                ),
                EventResult::Handled
            );
            assert_eq!(
                viewer.event(
                    &UiEvent::MouseUp {
                        position,
                        button: MouseButton::Left,
                        modifiers: Modifiers::none(),
                    },
                    &mut ctx,
                ),
                EventResult::Handled
            );
        }

        assert_eq!(
            actions.borrow().as_slice(),
            &[
                Action::GoToStart,
                Action::StepBack,
                Action::TogglePlay,
                Action::StepForward,
                Action::GoToEnd
            ]
        );
    }

    #[test]
    fn custom_control_mapper_overrides_default_actions() {
        let mut viewer =
            ViewerSurface::new("Scene 01", 1920, 1080).on_control(|control| match control {
                ViewerControl::PlayPause => Action::SaveProject,
                _ => Action::NoOp,
            });
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let position = viewer.control_rect(ViewerControl::PlayPause).center();
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        assert_eq!(
            viewer.event(
                &UiEvent::MouseDown {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            viewer.event(
                &UiEvent::MouseUp {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(actions.borrow().as_slice(), &[Action::SaveProject]);
    }

    #[test]
    fn preview_quality_chip_opens_dropdown_and_option_dispatches_custom_action() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080)
            .with_preview_quality_label("1/2")
            .on_preview_quality(|scale| {
                if (scale - 0.25).abs() <= f32::EPSILON {
                    Action::SaveProject
                } else {
                    Action::DeselectAll
                }
            });
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let position = viewer.preview_quality_rect().center();
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        assert_eq!(
            viewer.event(
                &UiEvent::MouseDown {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            viewer.event(
                &UiEvent::MouseUp {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(actions.borrow().as_slice(), &[]);
        assert_eq!(viewer.open_dropdown, Some(ViewerDropdown::PreviewQuality));
        let option = viewer.dropdown_row_rect(ViewerDropdown::PreviewQuality, 2).center();
        assert_eq!(
            viewer.event(
                &UiEvent::MouseDown {
                    position: option,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            viewer.event(
                &UiEvent::MouseUp {
                    position: option,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(actions.borrow().as_slice(), &[Action::SaveProject]);
    }

    #[test]
    fn zoom_chip_opens_dropdown_and_option_dispatches_custom_action() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080)
            .with_zoom_label("适合")
            .on_zoom(|scale| {
                if scale == Some(1.0) {
                    Action::SaveProject
                } else {
                    Action::DeselectAll
                }
            });
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let position = viewer.zoom_rect().center();
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        assert_eq!(
            viewer.event(
                &UiEvent::MouseDown {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            viewer.event(
                &UiEvent::MouseUp {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(actions.borrow().as_slice(), &[]);
        assert_eq!(viewer.open_dropdown, Some(ViewerDropdown::Zoom));
        let option = viewer.dropdown_row_rect(ViewerDropdown::Zoom, 5).center();
        assert_eq!(
            viewer.event(
                &UiEvent::MouseDown {
                    position: option,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            viewer.event(
                &UiEvent::MouseUp {
                    position: option,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(actions.borrow().as_slice(), &[Action::SaveProject]);
    }

    #[test]
    fn optional_viewer_chips_without_actions_do_not_dispatch_noop() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080)
            .with_zoom_label("适合")
            .with_preview_quality_label("1/2");
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        for position in [
            viewer.zoom_rect().center(),
            viewer.preview_quality_rect().center(),
        ] {
            assert_eq!(
                viewer.event(
                    &UiEvent::MouseDown {
                        position,
                        button: MouseButton::Left,
                        modifiers: Modifiers::none(),
                    },
                    &mut ctx,
                ),
                EventResult::Handled
            );
            assert_eq!(
                viewer.event(
                    &UiEvent::MouseUp {
                        position,
                        button: MouseButton::Left,
                        modifiers: Modifiers::none(),
                    },
                    &mut ctx,
                ),
                EventResult::Handled
            );
        }

        assert!(actions.borrow().is_empty());
        assert!(viewer.open_dropdown.is_some());
    }

    #[test]
    fn narrow_viewer_collapses_transport_controls() {
        let mut viewer = ViewerSurface::new("Narrow", 1920, 1080);
        viewer.layout(Rect::new(0.0, 0.0, 90.0, 120.0));

        assert_eq!(viewer.visible_controls(), &[ViewerControl::PlayPause]);
        assert_eq!(
            viewer.control_at(viewer.control_rect(ViewerControl::MarkIn).center()),
            None
        );
        assert_eq!(
            viewer.control_at(viewer.control_rect(ViewerControl::PlayPause).center()),
            Some(ViewerControl::PlayPause)
        );
        assert!(viewer.control_strip_rect().x >= 0.0);
        assert!(
            viewer.control_strip_rect().x + viewer.control_strip_rect().width
                <= viewer.bounds.width
        );
    }

    #[test]
    fn disabled_viewer_transport_controls_ignore_input() {
        let mut viewer = ViewerSurface::new("Offline", 1920, 1080).disabled();
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let position = viewer.control_rect(ViewerControl::PlayPause).center();
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        assert_eq!(
            viewer.event(
                &UiEvent::MouseDown {
                    position,
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
    fn viewer_keyboard_controls_dispatch_when_focused() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        assert_eq!(
            viewer.event(&UiEvent::FocusGained, &mut ctx),
            EventResult::Handled
        );
        for key in [
            KeyCode::I,
            KeyCode::O,
            KeyCode::Home,
            KeyCode::Left,
            KeyCode::Space,
            KeyCode::Right,
            KeyCode::End,
        ] {
            assert_eq!(
                viewer.event(
                    &UiEvent::KeyDown { key, modifiers: Modifiers::none() },
                    &mut ctx,
                ),
                EventResult::Handled
            );
        }

        assert_eq!(
            actions.borrow().as_slice(),
            &[
                Action::MarkInAtPlayhead,
                Action::MarkOutAtPlayhead,
                Action::GoToStart,
                Action::StepBack,
                Action::TogglePlay,
                Action::StepForward,
                Action::GoToEnd
            ]
        );
    }

    #[test]
    fn viewer_keyboard_ignores_without_focus_or_with_modifiers() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        assert_eq!(
            viewer.event(
                &UiEvent::KeyDown { key: KeyCode::Space, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Ignored
        );
        assert_eq!(
            viewer.event(&UiEvent::FocusGained, &mut ctx),
            EventResult::Handled
        );
        assert_eq!(
            viewer.event(
                &UiEvent::KeyDown { key: KeyCode::Space, modifiers: Modifiers::ctrl() },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn viewer_canvas_click_focuses_without_dispatching_transport_action() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let position = viewer.canvas_rect().center();

        assert_eq!(
            viewer.event(
                &UiEvent::MouseDown {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            viewer.event(
                &UiEvent::KeyDown { key: KeyCode::Space, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(actions.borrow().as_slice(), &[Action::TogglePlay]);
    }

    #[test]
    fn viewer_focus_lost_clears_pressed_chrome_and_repaints() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let position = viewer.control_rect(ViewerControl::PlayPause).center();
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        assert_eq!(
            viewer.event(
                &UiEvent::MouseDown {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        ctx.requests.repaint = false;

        assert_eq!(
            viewer.event(&UiEvent::FocusLost, &mut ctx),
            EventResult::Handled
        );

        assert!(ctx.requests.repaint);
        assert_eq!(viewer.pressed_control, None);
        assert_eq!(viewer.hovered_control, None);
        assert!(!viewer.focused);
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn disabled_viewer_next_event_clears_pressed_chrome_and_repaints() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let position = viewer.control_rect(ViewerControl::PlayPause).center();
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        assert_eq!(
            viewer.event(
                &UiEvent::MouseDown {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        viewer.enabled = false;
        ctx.requests.repaint = false;

        assert_eq!(
            viewer.event(
                &UiEvent::MouseMove { position, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert!(ctx.requests.repaint);
        assert_eq!(viewer.pressed_control, None);
        assert_eq!(viewer.hovered_control, None);
        assert!(!viewer.focused);
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn disabling_viewer_builder_cancels_pending_control_press() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let position = viewer.control_rect(ViewerControl::PlayPause).center();
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        assert_eq!(
            viewer.event(
                &UiEvent::MouseDown {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(viewer.pressed_control, Some(ViewerControl::PlayPause));

        viewer = viewer.enabled(false);
        assert_eq!(viewer.pressed_control, None);
        assert_eq!(viewer.hovered_control, None);
        assert!(!viewer.focused);

        viewer = viewer.enabled(true);
        assert_eq!(
            viewer.event(
                &UiEvent::MouseUp {
                    position,
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
    fn disabled_viewer_does_not_participate_in_focus() {
        let mut viewer = ViewerSurface::new("Offline", 1920, 1080).disabled();
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        assert!(!viewer.can_focus());
        assert_eq!(
            viewer.event(&UiEvent::FocusGained, &mut ctx),
            EventResult::Ignored
        );

        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn playing_viewer_paints_pause_transport_icon() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080).playing(true);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 500.0, 320.0),
        };

        viewer.paint(&mut ctx);

        assert!(
            encoder.triangles >= 12,
            "jump and step buttons should paint geometric triangles"
        );
        assert!(
            encoder.rects.len() >= 18,
            "playing transport should add pause-bar geometry"
        );
    }

    #[test]
    fn viewer_transport_omits_mark_in_out_buttons() {
        let mut viewer = ViewerSurface::new("Viewer", 1920, 1080);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));

        assert!(!viewer.visible_controls().contains(&ViewerControl::MarkIn));
        assert!(!viewer.visible_controls().contains(&ViewerControl::MarkOut));
        assert_eq!(
            viewer.control_at(viewer.control_rect(ViewerControl::MarkIn).center()),
            None
        );
        assert_eq!(
            viewer.control_at(viewer.control_rect(ViewerControl::MarkOut).center()),
            None
        );
    }

    #[test]
    fn frame_image_rejects_invalid_rgba_payloads() {
        assert!(ViewerFrameImage::new("bad", 2, 2, vec![255; 15]).is_none());
        assert!(ViewerFrameImage::new("empty", 0, 2, Vec::<u8>::new()).is_none());
    }

    #[test]
    fn paint_draws_preview_frame_inside_canvas_clip() {
        let image =
            ViewerFrameImage::new("preview:42", 2, 2, vec![255; 16]).expect("valid preview image");
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080).with_frame_image(image);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let canvas = viewer.canvas_rect();
        let viewport = viewer.canvas_viewport_rect();
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 500.0, 320.0),
        };

        viewer.paint(&mut ctx);

        assert_eq!(
            encoder.raster_images,
            vec![("preview:42".to_owned(), canvas, 2, 2)]
        );
        let mut checker_dark = theme.colors.checkerboard_dark;
        checker_dark.a *= 0.28;
        assert!(encoder.rect_colors.iter().any(|color| *color == theme.colors.viewer_stage));
        assert!(encoder.rect_colors.iter().any(|color| *color == checker_dark));
        assert!(encoder
            .rect_colors
            .iter()
            .any(|color| *color == color_with_alpha(theme.colors.foreground, 0.08)));
        assert!(encoder.rect_colors.iter().any(|color| *color == theme.colors.canvas));
        assert!(
            encoder.clips.contains(&viewport),
            "preview image must be clipped to the viewer viewport"
        );
        assert!(
            encoder.clips.contains(&canvas),
            "preview image must also be clipped to the sequence canvas"
        );
        assert!(
            encoder
                .rects
                .iter()
                .zip(encoder.rect_radii.iter())
                .any(|(rect, radius)| *rect == canvas && *radius == 0.0),
            "sequence canvas should be painted as a straight-edged rectangle"
        );
        assert_eq!(encoder.clip_pops, encoder.clips.len());
    }

    #[test]
    fn disabled_viewer_does_not_draw_preview_frame() {
        let image = ViewerFrameImage::new("preview:disabled", 2, 2, vec![255; 16])
            .expect("valid preview image");
        let mut viewer =
            ViewerSurface::new("Scene 01", 1920, 1080).with_frame_image(image).disabled();
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 500.0, 320.0),
        };

        viewer.paint(&mut ctx);

        assert!(encoder.raster_images.is_empty());
    }
}
