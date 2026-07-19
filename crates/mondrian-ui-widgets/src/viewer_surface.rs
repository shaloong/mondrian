//! Domain-light viewer surface for editor preview panels.
//!
//! The widget paints preview chrome, aspect-ratio fitting, an optional preview
//! frame, safe-area guides, and status metadata. App/runtime layers own preview
//! decoding/rendering and pass already-renderable frame references across this
//! domain-light boundary.

mod model;
mod paint;

use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{
    AccessibilityNode, AccessibilityRole, AccessibilityState, AccessibilityValue, EventContext,
    PaintContext,
};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_theme::{current_theme, Theme};
use std::cell::Cell;
use std::sync::Arc;

use crate::menu::{paint_menu_checkmark, MenuVisualTokens};
#[allow(unused_imports)]
use crate::paint::{
    centered_text_origin_y, color_with_alpha, horizontal_stroke_rect, mix_color, paint_focus_ring,
    soft_border, vertical_stroke_rect,
};
use crate::text_metrics::measure_single_line;
use crate::{RasterImage, VectorIcon};

use self::model as viewer_model;

#[derive(Debug, Clone, Copy, PartialEq)]
struct ViewerMetrics {
    default_width: f32,
    default_height: f32,
    transport_button_size: f32,
    transport_button_gap: f32,
    dropdown_row_height: f32,
    dropdown_padding_x: f32,
    dropdown_padding_y: f32,
}

impl ViewerMetrics {
    fn from_theme(theme: &Theme) -> Self {
        let spacing = &theme.spacing;
        Self {
            default_width: spacing.viewer_default_width,
            default_height: spacing.viewer_default_height,
            transport_button_size: spacing.viewer_transport_button_size,
            transport_button_gap: spacing.viewer_transport_button_gap,
            dropdown_row_height: spacing.viewer_dropdown_row_height,
            dropdown_padding_x: spacing.viewer_dropdown_padding_x,
            dropdown_padding_y: spacing.viewer_dropdown_padding_y,
        }
    }

    fn current() -> Self {
        let theme = current_theme();
        Self::from_theme(&theme)
    }
}

/// RGBA preview image presented by [`ViewerSurface`].
pub type ViewerFrameImage = RasterImage;

/// Renderer-registered GPU preview texture presented by [`ViewerSurface`].
#[derive(Debug, Clone, PartialEq)]
pub struct ViewerExternalTextureFrame {
    /// Stable key registered with the active UI renderer.
    pub key: String,
    /// Source frame width in pixels.
    pub width: u32,
    /// Source frame height in pixels.
    pub height: u32,
    /// Exact spatial presentation contract, when the texture was prefiltered
    /// for the current visible Viewer region.
    pub presentation: Option<ViewerExternalTexturePresentation>,
}

impl ViewerExternalTextureFrame {
    /// Create an external GPU texture frame reference.
    ///
    /// Empty keys or invalid dimensions are rejected so missing producer state
    /// becomes a renderer diagnostic instead of a malformed widget command.
    pub fn new(key: impl Into<String>, width: u32, height: u32) -> Option<Self> {
        let key = key.into();
        (!key.is_empty() && width > 0 && height > 0).then_some(Self {
            key,
            width,
            height,
            presentation: None,
        })
    }

    /// Create a prefiltered external texture for an exact Viewer presentation.
    pub fn new_spatial(
        key: impl Into<String>,
        presentation: ViewerExternalTexturePresentation,
    ) -> Option<Self> {
        let key = key.into();
        (!key.is_empty()).then_some(Self {
            key,
            width: presentation.output_width,
            height: presentation.output_height,
            presentation: Some(presentation),
        })
    }
}

const VIEWER_SOURCE_COORDINATE_SCALE: u32 = 1_000_000_000;

/// Stable spatial identity shared by Viewer layout and the GPU producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ViewerExternalTexturePresentation {
    /// Visible output width in physical pixels.
    pub output_width: u32,
    /// Visible output height in physical pixels.
    pub output_height: u32,
    source_rect: [u32; 4],
}

impl ViewerExternalTexturePresentation {
    /// Create a full-frame spatial presentation at an explicit output extent.
    pub fn full_frame(output_width: u32, output_height: u32) -> Option<Self> {
        Self::new(output_width, output_height, Rect::new(0.0, 0.0, 1.0, 1.0))
    }

    fn new(output_width: u32, output_height: u32, source_rect: Rect) -> Option<Self> {
        if output_width == 0
            || output_height == 0
            || source_rect.width <= 0.0
            || source_rect.height <= 0.0
        {
            return None;
        }
        let quantize = |value: f32| {
            (value.clamp(0.0, 1.0) * VIEWER_SOURCE_COORDINATE_SCALE as f32)
                .round()
                .clamp(0.0, VIEWER_SOURCE_COORDINATE_SCALE as f32) as u32
        };
        let left = quantize(source_rect.x);
        let top = quantize(source_rect.y);
        let right = quantize(source_rect.x + source_rect.width);
        let bottom = quantize(source_rect.y + source_rect.height);
        let source_rect = [
            left,
            top,
            right.saturating_sub(left),
            bottom.saturating_sub(top),
        ];
        (source_rect[2] > 0
            && source_rect[3] > 0
            && source_rect[0].saturating_add(source_rect[2]) <= VIEWER_SOURCE_COORDINATE_SCALE
            && source_rect[1].saturating_add(source_rect[3]) <= VIEWER_SOURCE_COORDINATE_SCALE)
            .then_some(Self { output_width, output_height, source_rect })
    }

    /// Return the normalized source region represented by this texture.
    pub fn normalized_source_rect(self) -> Rect {
        let scale = VIEWER_SOURCE_COORDINATE_SCALE as f32;
        Rect::new(
            self.source_rect[0] as f32 / scale,
            self.source_rect[1] as f32 / scale,
            self.source_rect[2] as f32 / scale,
            self.source_rect[3] as f32 / scale,
        )
    }

    /// Stable suffix for external texture/cache identities.
    pub fn key_suffix(self) -> String {
        format!(
            "{}x{}-{:08x}{:08x}{:08x}{:08x}",
            self.output_width,
            self.output_height,
            self.source_rect[0],
            self.source_rect[1],
            self.source_rect[2],
            self.source_rect[3]
        )
    }
}

/// Current pixel-aligned Viewer presentation geometry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewerPresentationGeometry {
    /// Complete sequence canvas rectangle.
    pub canvas_rect: Rect,
    /// Visible canvas intersection receiving the prefiltered texture.
    pub visible_rect: Rect,
    /// Stable producer/consumer spatial identity.
    pub presentation: ViewerExternalTexturePresentation,
}

/// Preview frame content presented by [`ViewerSurface`].
#[derive(Debug, Clone)]
pub enum ViewerFrameContent {
    /// CPU RGBA frame uploaded through the renderer-owned raster atlas.
    Raster(ViewerFrameImage),
    /// GPU frame already registered with the renderer texture registry.
    ExternalTexture(ViewerExternalTextureFrame),
}

impl ViewerFrameContent {
    /// Source frame dimensions.
    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Raster(frame) => (frame.width, frame.height),
            Self::ExternalTexture(frame) => (frame.width, frame.height),
        }
    }
}

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
    title: String,
    status: String,
    status_tone: ViewerStatusTone,
    resolution_label: String,
    position_label: String,
    duration_label: String,
    zoom_label: String,
    zoom_scale: Option<f32>,
    preview_quality_label: String,
    source_width: u32,
    source_height: u32,
    playing: bool,
    enabled: bool,
    frame_content: Option<ViewerFrameContent>,
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
    overlay_viewport: Cell<Option<Rect>>,
    control_icons: Vec<(ViewerControl, VectorIcon)>,
    play_pause_icon: Option<VectorIcon>,
    playing_pause_icon: Option<VectorIcon>,
}

impl ViewerSurface {
    /// Create a viewer surface with a title and source dimensions.
    pub fn new(title: impl Into<String>, source_width: u32, source_height: u32) -> Self {
        Self {
            id: WidgetId::new(),
            bounds: Rect::ZERO,
            title: title.into(),
            status: "无信号".into(),
            status_tone: ViewerStatusTone::Neutral,
            resolution_label: String::new(),
            position_label: "00:00:00:00".into(),
            duration_label: String::new(),
            zoom_label: "适合".into(),
            zoom_scale: None,
            preview_quality_label: "1/1".into(),
            source_width: source_width.max(1),
            source_height: source_height.max(1),
            playing: false,
            enabled: true,
            frame_content: None,
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
            overlay_viewport: Cell::new(None),
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

    /// Set the current position label resolved by the Sequence display contract.
    pub fn with_position_label(mut self, label: impl Into<String>) -> Self {
        self.position_label = label.into();
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
        self.frame_content = Some(ViewerFrameContent::Raster(frame_image));
        self
    }

    /// Set the rendered preview content shown inside the fitted canvas.
    pub fn with_frame_content(mut self, frame_content: ViewerFrameContent) -> Self {
        self.frame_content = Some(frame_content);
        self
    }

    /// Update only playback-frame dependent viewer state.
    pub fn set_playback_frame_state(
        &mut self,
        status: impl Into<String>,
        status_tone: ViewerStatusTone,
        position_label: impl Into<String>,
        playing: bool,
        frame_image: Option<ViewerFrameImage>,
        empty_message: Option<String>,
    ) {
        self.set_playback_frame_content_state(
            status,
            status_tone,
            position_label,
            playing,
            frame_image.map(ViewerFrameContent::Raster),
            empty_message,
        );
    }

    /// Update only playback-frame dependent viewer state with generic frame content.
    pub fn set_playback_frame_content_state(
        &mut self,
        status: impl Into<String>,
        status_tone: ViewerStatusTone,
        position_label: impl Into<String>,
        playing: bool,
        frame_content: Option<ViewerFrameContent>,
        empty_message: Option<String>,
    ) {
        self.status = status.into();
        self.status_tone = status_tone;
        self.position_label = position_label.into();
        self.playing = playing;
        self.frame_content = frame_content;
        self.empty_message = empty_message;
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

    fn canvas_viewport_rect(&self) -> Rect {
        viewer_model::canvas_viewport_rect(self.bounds)
    }

    fn canvas_rect(&self) -> Rect {
        viewer_model::canvas_rect(
            self.bounds,
            self.source_width,
            self.source_height,
            self.zoom_scale,
        )
    }

    /// Resolve the visible, pixel-aligned spatial contract for GPU presentation.
    pub fn presentation_geometry(&self) -> Option<ViewerPresentationGeometry> {
        let canvas_rect = self.canvas_rect();
        let visible_rect = canvas_rect.intersection(&self.canvas_viewport_rect());
        if visible_rect.width <= 0.0 || visible_rect.height <= 0.0 {
            return None;
        }
        let source_rect = Rect::new(
            (visible_rect.x - canvas_rect.x) / canvas_rect.width,
            (visible_rect.y - canvas_rect.y) / canvas_rect.height,
            visible_rect.width / canvas_rect.width,
            visible_rect.height / canvas_rect.height,
        );
        let output_width = visible_rect.width.round().max(1.0) as u32;
        let output_height = visible_rect.height.round().max(1.0) as u32;
        let presentation =
            ViewerExternalTexturePresentation::new(output_width, output_height, source_rect)?;
        Some(ViewerPresentationGeometry { canvas_rect, visible_rect, presentation })
    }

    fn metadata_text(&self) -> String {
        self.position_label.clone()
    }

    fn should_paint_status_badge(&self) -> bool {
        matches!(
            self.status_tone,
            ViewerStatusTone::Warning | ViewerStatusTone::Error
        ) && !self.status.is_empty()
    }

    fn control_strip_rect(&self) -> Rect {
        viewer_model::control_strip_rect(self.bounds)
    }

    fn visible_controls(&self) -> &'static [ViewerControl] {
        viewer_model::visible_controls(self.bounds.width)
    }

    fn control_rect(&self, control: ViewerControl) -> Rect {
        viewer_model::control_rect(self.bounds, control)
    }

    fn control_at(&self, point: Point) -> Option<ViewerControl> {
        viewer_model::control_at(self.bounds, point)
    }

    fn preview_quality_rect(&self) -> Rect {
        viewer_model::preview_quality_rect(self.bounds, &self.preview_quality_label)
    }

    fn zoom_rect(&self) -> Rect {
        viewer_model::zoom_rect(self.bounds, &self.zoom_label, &self.preview_quality_label)
    }

    fn preview_quality_at(&self, point: Point) -> bool {
        viewer_model::point_in_rect(self.preview_quality_rect(), point)
    }

    fn zoom_at(&self, point: Point) -> bool {
        viewer_model::point_in_rect(self.zoom_rect(), point)
    }

    fn dropdown_options_len(dropdown: ViewerDropdown) -> usize {
        viewer_model::dropdown_options_len(dropdown)
    }

    fn dropdown_rect(&self, dropdown: ViewerDropdown) -> Rect {
        viewer_model::dropdown_rect(
            self.bounds,
            self.overlay_viewport.get(),
            dropdown,
            &self.zoom_label,
            &self.preview_quality_label,
        )
    }

    fn dropdown_row_rect(&self, dropdown: ViewerDropdown, index: usize) -> Rect {
        viewer_model::dropdown_row_rect(
            self.bounds,
            self.overlay_viewport.get(),
            dropdown,
            &self.zoom_label,
            &self.preview_quality_label,
            index,
        )
    }

    fn dropdown_item_at(&self, point: Point) -> Option<(ViewerDropdown, usize)> {
        viewer_model::dropdown_item_at(
            self.bounds,
            self.overlay_viewport.get(),
            self.open_dropdown,
            &self.zoom_label,
            &self.preview_quality_label,
            point,
        )
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
        viewer_model::keyboard_control(key, modifiers)
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
        let metrics = ViewerMetrics::current();
        constraint.constrain(Size::new(metrics.default_width, metrics.default_height))
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
                if let Some((dropdown, index)) = self.dropdown_item_at(*position) {
                    self.open_dropdown = Some(dropdown);
                    self.hovered_dropdown_index = Some(index);
                    self.pressed_dropdown_index = Some(index);
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
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
            UiEvent::FocusGained { source } => {
                self.focused = true;
                self.focus_visible = source.is_focus_visible();
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
        ctx.push_clip(canvas);
        ctx.encoder.draw_rect(canvas, colors.canvas, 0.0);
        if self.enabled {
            if let Some(frame) = &self.frame_content {
                paint::paint_checkerboard(ctx, canvas);
                match frame {
                    ViewerFrameContent::Raster(frame) => {
                        ctx.encoder.draw_raster_image(
                            &frame.key,
                            canvas,
                            frame.width,
                            frame.height,
                            frame.color_space,
                            Arc::clone(&frame.rgba),
                            Color::WHITE,
                        );
                    }
                    ViewerFrameContent::ExternalTexture(frame) => match frame.presentation {
                        Some(presentation) => {
                            if let Some(geometry) = self
                                .presentation_geometry()
                                .filter(|geometry| geometry.presentation == presentation)
                            {
                                ctx.encoder.draw_external_texture(
                                    &frame.key,
                                    geometry.visible_rect,
                                    Rect::new(0.0, 0.0, 1.0, 1.0),
                                    Color::WHITE,
                                );
                            }
                        }
                        None => ctx.encoder.draw_external_texture(
                            &frame.key,
                            canvas,
                            Rect::new(0.0, 0.0, 1.0, 1.0),
                            Color::WHITE,
                        ),
                    },
                }
            }
        }
        if self.frame_content.is_none() {
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
        paint::paint_safe_guides(ctx, canvas, self.enabled);
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
    }

    fn paint_overlay(&self, ctx: &mut PaintContext) {
        if self.open_dropdown.is_none() {
            self.overlay_viewport.set(None);
            return;
        }
        self.overlay_viewport.set(Some(ctx.clip_rect));
        self.paint_open_dropdown(ctx);
    }

    fn overlay_hit_test(&self, _point: Point) -> bool {
        self.enabled && self.open_dropdown.is_some()
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

    fn accessibility(&self) -> Option<AccessibilityNode> {
        Some(
            AccessibilityNode::new(self.id, AccessibilityRole::Canvas)
                .with_name(self.title.clone())
                .with_state(AccessibilityState {
                    focusable: self.enabled,
                    focused: self.focused,
                    disabled: !self.enabled,
                    pressed: Some(
                        self.pressed_control.is_some()
                            || self.pressed_zoom
                            || self.pressed_preview_quality
                            || self.pressed_dropdown_index.is_some(),
                    ),
                    expanded: Some(self.open_dropdown.is_some()),
                    ..AccessibilityState::default()
                })
                .with_value(AccessibilityValue::Viewer {
                    source_width: self.source_width,
                    source_height: self.source_height,
                    has_frame: self.frame_content.is_some(),
                    zoom_scale: self.zoom_scale,
                }),
        )
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
        let hovered = self.hovered_control == Some(control);
        let active = control == ViewerControl::PlayPause && self.playing;
        // Match timeline tool button style
        let bg = if !self.enabled {
            Color::TRANSPARENT
        } else if active {
            color_with_alpha(colors.foreground, 0.085)
        } else if hovered {
            colors.surface_2
        } else {
            Color::TRANSPARENT
        };
        let icon = if !self.enabled {
            color_with_alpha(colors.muted_foreground, 0.44)
        } else if active || hovered {
            colors.foreground
        } else {
            colors.muted_foreground
        };

        ctx.encoder.draw_rect(rect, bg, radius);
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
                paint::paint_mark_in_icon(ctx, rect, icon);
            }
            ViewerControl::MarkOut => {
                paint::paint_mark_out_icon(ctx, rect, icon);
            }
            ViewerControl::JumpStart => {
                ctx.encoder.draw_rect(
                    Rect::new(rect.x + 7.0, rect.y + 7.0, 2.0, 12.0),
                    color_with_alpha(icon, 0.9),
                    1.0,
                );
                paint::paint_left_triangle(ctx, rect, 10.0, icon);
            }
            ViewerControl::StepBack => {
                paint::paint_left_triangle(ctx, rect, 8.0, icon);
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
                paint::paint_right_triangle(ctx, rect, 8.0, icon);
            }
            ViewerControl::JumpEnd => {
                paint::paint_right_triangle(ctx, rect, 8.0, icon);
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
        let fill = if pressed || open {
            colors.surface_2
        } else if hovered {
            color_with_alpha(colors.foreground, 0.06)
        } else {
            Color::TRANSPARENT
        };
        ctx.encoder.draw_rect(rect, fill, ctx.theme.spacing.radius_sm);
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
            let visual = MenuVisualTokens::from_theme(ctx.theme);
            let check_lane = visual.row_padding_x + visual.row_icon_size + visual.row_icon_gap;
            if selected {
                paint_menu_checkmark(ctx, row, text_color);
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

fn status_badge_colors(surface: &ViewerSurface, ctx: &PaintContext) -> (Color, Color) {
    paint::status_badge_colors(surface.enabled, surface.status_tone, ctx)
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
        text_positions: Vec<Point>,
        raster_images: Vec<(String, Rect, u32, u32)>,
        external_textures: Vec<(String, Rect, Rect)>,
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
        fn draw_text(&mut self, text: &str, _font_size: f32, position: Point, _color: Color) {
            self.texts.push(text.into());
            self.text_positions.push(position);
        }
        fn draw_text_box(
            &mut self,
            text: &str,
            _font_size: f32,
            position: Point,
            _max_width: f32,
            _color: Color,
        ) {
            self.texts.push(text.into());
            self.text_positions.push(position);
        }
        fn draw_raster_image(
            &mut self,
            key: &str,
            bounds: Rect,
            width: u32,
            height: u32,
            _color_space: mondrian_ui_core::RasterImageColorSpace,
            _rgba: Arc<[u8]>,
            _tint: Color,
        ) {
            self.raster_images.push((key.to_owned(), bounds, width, height));
        }
        fn draw_external_texture(&mut self, key: &str, bounds: Rect, uv_rect: Rect, _tint: Color) {
            self.external_textures.push((key.to_owned(), bounds, uv_rect));
        }
        fn push_translate(&mut self, _offset: glam::Vec2) {}
        fn pop_transform(&mut self) {}
    }

    fn rect_approx_eq(left: Rect, right: Rect) -> bool {
        (left.x - right.x).abs() < 0.001
            && (left.y - right.y).abs() < 0.001
            && (left.width - right.width).abs() < 0.001
            && (left.height - right.height).abs() < 0.001
    }

    fn has_rect(rects: &[Rect], wanted: Rect) -> bool {
        rects.iter().copied().any(|candidate| rect_approx_eq(candidate, wanted))
    }

    #[test]
    fn canvas_preserves_source_aspect_ratio() {
        let mut viewer = ViewerSurface::new("Demo", 1920, 1080);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));

        let canvas = viewer.canvas_rect();

        assert!((canvas.width - canvas.height * 16.0 / 9.0).abs() <= 1.0);
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
    fn presentation_geometry_is_pixel_aligned_and_crops_fixed_zoom() {
        let mut viewer = ViewerSurface::new("Demo", 1920, 1080).with_zoom_scale(Some(1.0));
        viewer.layout(Rect::new(0.25, 0.5, 500.0, 320.0));

        let geometry = viewer.presentation_geometry().expect("visible presentation");
        let source = geometry.presentation.normalized_source_rect();

        assert_eq!(geometry.visible_rect.x.fract(), 0.0);
        assert_eq!(geometry.visible_rect.y.fract(), 0.0);
        assert_eq!(geometry.visible_rect.width.fract(), 0.0);
        assert_eq!(geometry.visible_rect.height.fract(), 0.0);
        assert_eq!(
            geometry.presentation.output_width,
            geometry.visible_rect.width as u32
        );
        assert_eq!(
            geometry.presentation.output_height,
            geometry.visible_rect.height as u32
        );
        assert!(source.x > 0.0 && source.y > 0.0);
        assert!(source.width < 1.0 && source.height < 1.0);
    }

    #[test]
    fn paint_prioritizes_canvas_and_omits_normal_status_chrome() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080)
            .with_status("Playing")
            .with_resolution_label("1920x1080")
            .with_position_label("00:00:01:18")
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
    fn playback_frame_state_updates_empty_message() {
        let mut viewer = ViewerSurface::new("Viewer", 16, 9).with_empty_message("旧状态");

        viewer.set_playback_frame_state(
            "预览准备中",
            ViewerStatusTone::Warning,
            "00:00:00:00",
            true,
            None,
            Some("预览准备中".into()),
        );

        assert_eq!(viewer.empty_message.as_deref(), Some("预览准备中"));

        viewer.set_playback_frame_state(
            "就绪",
            ViewerStatusTone::Neutral,
            "00:00:00:00",
            false,
            None,
            None,
        );

        assert_eq!(viewer.empty_message, None);
    }

    #[test]
    fn safe_guides_are_drawn_inside_the_fitted_canvas() {
        let mut viewer = ViewerSurface::new("Viewer", 1920, 1080);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let canvas = viewer.canvas_rect();
        let action = canvas.inset(canvas.width * 0.05, canvas.height * 0.05);
        let title = canvas.inset(canvas.width * 0.10, canvas.height * 0.10);
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 500.0, 320.0),
        };

        viewer.paint(&mut ctx);

        for guide in [action, title] {
            assert!(guide.x > canvas.x);
            assert!(guide.y > canvas.y);
            assert!(guide.x + guide.width < canvas.x + canvas.width);
            assert!(guide.y + guide.height < canvas.y + canvas.height);
            assert!(has_rect(
                &encoder.rects,
                horizontal_stroke_rect(guide.y, guide.x, guide.width, 1.0)
            ));
            assert!(has_rect(
                &encoder.rects,
                horizontal_stroke_rect(guide.y + guide.height, guide.x, guide.width, 1.0)
            ));
            assert!(has_rect(
                &encoder.rects,
                vertical_stroke_rect(guide.x, guide.y, guide.height, 1.0)
            ));
            assert!(has_rect(
                &encoder.rects,
                vertical_stroke_rect(guide.x + guide.width, guide.y, guide.height, 1.0)
            ));
        }
    }

    #[test]
    fn disabled_builder_marks_surface_unavailable() {
        let viewer = ViewerSurface::new("Offline", 1920, 1080).disabled();

        assert!(!viewer.is_enabled());
    }

    #[test]
    fn viewer_accessibility_exposes_canvas_state_and_source_metadata() {
        let image = ViewerFrameImage::new(
            "frame:a11y",
            1,
            1,
            mondrian_ui_core::RasterImageColorSpace::Srgb,
            vec![255, 0, 0, 255],
        )
        .expect("valid frame");
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080)
            .with_frame_image(image)
            .with_zoom_scale(Some(0.5));
        viewer.focused = true;
        viewer.open_dropdown = Some(ViewerDropdown::Zoom);

        let node = viewer.accessibility().expect("viewer should expose accessibility metadata");

        assert_eq!(node.role, AccessibilityRole::Canvas);
        assert_eq!(node.name.as_deref(), Some("Scene 01"));
        assert!(node.state.focusable);
        assert!(node.state.focused);
        assert!(!node.state.disabled);
        assert_eq!(node.state.pressed, Some(false));
        assert_eq!(node.state.expanded, Some(true));
        assert_eq!(
            node.value,
            Some(AccessibilityValue::Viewer {
                source_width: 1920,
                source_height: 1080,
                has_frame: true,
                zoom_scale: Some(0.5),
            })
        );
    }

    #[test]
    fn disabled_viewer_accessibility_is_not_focusable() {
        let viewer = ViewerSurface::new("Offline", 1920, 1080).disabled();
        let node = viewer.accessibility().expect("viewer should expose accessibility metadata");

        assert_eq!(node.role, AccessibilityRole::Canvas);
        assert!(!node.state.focusable);
        assert!(!node.state.focused);
        assert!(node.state.disabled);
        assert_eq!(
            node.value,
            Some(AccessibilityValue::Viewer {
                source_width: 1920,
                source_height: 1080,
                has_frame: false,
                zoom_scale: None,
            })
        );
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
    fn viewer_dropdown_uses_menu_checkmark_geometry() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080).with_zoom_label("适合");
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        viewer.open_dropdown = Some(ViewerDropdown::Zoom);
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 800.0, 600.0),
        };

        viewer.paint_overlay(&mut ctx);

        assert!(
            encoder.lines >= 2,
            "selected viewer dropdown option should paint the shared menu checkmark lines"
        );
        assert!(
            !encoder.texts.iter().any(|text| text == "✓"),
            "viewer dropdown should not use a text glyph checkmark"
        );
    }

    #[test]
    fn viewer_dropdown_label_starts_after_reserved_check_lane() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080).with_zoom_label("适合");
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        viewer.open_dropdown = Some(ViewerDropdown::Zoom);
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 800.0, 600.0),
        };

        viewer.paint_overlay(&mut ctx);

        let visual = MenuVisualTokens::from_theme(&theme);
        let row = viewer.dropdown_row_rect(ViewerDropdown::Zoom, 0);
        let expected_x = row.x + visual.row_padding_x + visual.row_icon_size + visual.row_icon_gap;
        let index = encoder
            .texts
            .iter()
            .position(|text| text == VIEWER_ZOOM_OPTIONS[0].label)
            .expect("zoom option label should be painted");
        assert!(encoder.text_positions[index].x >= expected_x);
    }

    #[test]
    fn focus_lost_closes_open_viewer_dropdown_without_dispatching() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080)
            .with_zoom_label("适合")
            .on_zoom(|_| Action::SaveProject);
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
        assert_eq!(viewer.open_dropdown, Some(ViewerDropdown::Zoom));
        assert!(viewer.hovered_zoom);
        ctx.requests.repaint = false;

        assert_eq!(
            viewer.event(&UiEvent::FocusLost, &mut ctx),
            EventResult::Handled
        );

        assert!(ctx.requests.repaint);
        assert_eq!(viewer.open_dropdown, None);
        assert_eq!(viewer.hovered_dropdown_index, None);
        assert_eq!(viewer.pressed_dropdown_index, None);
        assert!(!viewer.hovered_zoom);
        assert!(!viewer.focused);
        assert!(actions.borrow().is_empty());
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
            viewer.event(&UiEvent::focus_gained_keyboard(), &mut ctx),
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
            viewer.event(&UiEvent::focus_gained_keyboard(), &mut ctx),
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
            viewer.event(&UiEvent::focus_gained_keyboard(), &mut ctx),
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
        assert!(ViewerFrameImage::new(
            "bad",
            2,
            2,
            mondrian_ui_core::RasterImageColorSpace::Srgb,
            vec![255; 15],
        )
        .is_none());
        assert!(ViewerFrameImage::new(
            "empty",
            0,
            2,
            mondrian_ui_core::RasterImageColorSpace::Srgb,
            Vec::<u8>::new(),
        )
        .is_none());
    }

    #[test]
    fn external_texture_frame_rejects_invalid_identity() {
        assert!(ViewerExternalTextureFrame::new("", 1920, 1080).is_none());
        assert!(ViewerExternalTextureFrame::new("viewer.preview", 0, 1080).is_none());
        assert!(ViewerExternalTextureFrame::new("viewer.preview", 1920, 0).is_none());
    }

    #[test]
    fn paint_draws_preview_frame_inside_canvas_clip() {
        let image = ViewerFrameImage::new(
            "preview:42",
            2,
            2,
            mondrian_ui_core::RasterImageColorSpace::Srgb,
            vec![255; 16],
        )
        .expect("valid preview image");
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
        assert!(encoder.rect_colors.contains(&theme.colors.viewer_stage));
        assert!(encoder.rect_colors.contains(&checker_dark));
        assert!(encoder.rect_colors.contains(&theme.colors.canvas));
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
    fn paint_draws_external_texture_frame_inside_canvas_clip() {
        let frame = ViewerExternalTextureFrame::new("viewer.preview.gpu", 1920, 1080)
            .expect("valid external texture frame");
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080)
            .with_frame_content(ViewerFrameContent::ExternalTexture(frame));
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

        assert!(encoder.raster_images.is_empty());
        assert_eq!(
            encoder.external_textures,
            vec![(
                "viewer.preview.gpu".to_owned(),
                canvas,
                Rect::new(0.0, 0.0, 1.0, 1.0)
            )]
        );
        assert!(
            encoder.clips.contains(&viewport),
            "external texture must be clipped to the viewer viewport"
        );
        assert!(
            encoder.clips.contains(&canvas),
            "external texture must also be clipped to the sequence canvas"
        );
    }

    #[test]
    fn spatial_external_texture_requires_exact_current_geometry() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080).with_zoom_scale(Some(1.0));
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let geometry = viewer.presentation_geometry().expect("visible presentation");
        let frame = ViewerExternalTextureFrame::new_spatial(
            "viewer.preview.spatial",
            geometry.presentation,
        )
        .expect("spatial frame");
        viewer.frame_content = Some(ViewerFrameContent::ExternalTexture(frame));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 500.0, 320.0),
        };

        viewer.paint(&mut ctx);
        assert_eq!(
            encoder.external_textures,
            vec![(
                "viewer.preview.spatial".to_owned(),
                geometry.visible_rect,
                Rect::new(0.0, 0.0, 1.0, 1.0),
            )]
        );

        viewer.layout(Rect::new(0.0, 0.0, 600.0, 360.0));
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 600.0, 360.0),
        };
        viewer.paint(&mut ctx);
        assert!(encoder.external_textures.is_empty());
    }

    #[test]
    fn disabled_viewer_does_not_draw_preview_frame() {
        let image = ViewerFrameImage::new(
            "preview:disabled",
            2,
            2,
            mondrian_ui_core::RasterImageColorSpace::Srgb,
            vec![255; 16],
        )
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
        assert!(encoder.external_textures.is_empty());
    }
}
