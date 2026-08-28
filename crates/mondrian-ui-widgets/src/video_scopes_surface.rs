//! GPU-backed Program Output video scopes surface.

use mondrian_core::{
    Color, ColorSpace, ProgramScopeScale, ProgramScopesTap, ProgramSignalColorimetry,
    SignalMonitoringSettings, WaveformMode,
};
use mondrian_editor_state::Action;
use mondrian_ui_core::types::{LayoutConstraint, MouseButton, Point, Rect, Size, WidgetId};
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use serde::{Deserialize, Serialize};

const TOOLBAR_HEIGHT: f32 = 28.0;
const CONTROL_COUNT: usize = 9;

/// Independent scope-pane layout selected by the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum VideoScopesLayout {
    /// Large waveform above histogram and vectorscope.
    #[default]
    Overview,
    /// Equal three-column scope panes.
    Grid,
    /// Dedicated waveform pane.
    Waveform,
    /// Dedicated RGB histogram pane.
    Histogram,
    /// Dedicated vectorscope pane.
    Vectorscope,
}

impl VideoScopesLayout {
    /// Cycle through the bounded product layouts.
    pub const fn next(self) -> Self {
        match self {
            Self::Overview => Self::Grid,
            Self::Grid => Self::Waveform,
            Self::Waveform => Self::Histogram,
            Self::Histogram => Self::Vectorscope,
            Self::Vectorscope => Self::Overview,
        }
    }
}

/// Machine-local professional scopes controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VideoScopesSettings {
    pub waveform_mode: WaveformMode,
    pub scale: ProgramScopeScale,
    pub tap: ProgramScopesTap,
    pub layout: VideoScopesLayout,
    pub show_skin_tone_line: bool,
    pub show_color_targets: bool,
    /// Viewer-picture warnings configured beside the signal-analysis controls.
    #[serde(default)]
    pub monitoring: SignalMonitoringSettings,
}

impl Default for VideoScopesSettings {
    fn default() -> Self {
        Self {
            waveform_mode: WaveformMode::Luma,
            scale: ProgramScopeScale::Ire,
            tap: ProgramScopesTap::ProgramOutput,
            layout: VideoScopesLayout::Overview,
            show_skin_tone_line: true,
            show_color_targets: true,
            monitoring: SignalMonitoringSettings::default(),
        }
    }
}

impl VideoScopesSettings {
    /// Identity of analysis work; presentation-only layout/guides are excluded.
    pub const fn analysis_identity(self) -> (WaveformMode, ProgramScopeScale, ProgramScopesTap) {
        (self.waveform_mode, self.scale, self.tap)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
    Waveform,
    Histogram,
    Vectorscope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeControl {
    Waveform,
    Scale,
    Tap,
    Skin,
    Targets,
    FalseColor,
    Zebra,
    Gamut,
    Layout,
}

/// Stable renderer-registry keys for one coherent scope texture set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoScopesTextureSet {
    /// RGB histogram texture key.
    pub histogram: String,
    /// Luma/RGB waveform texture key.
    pub waveform: String,
    /// Vectorscope texture key.
    pub vectorscope: String,
}

impl VideoScopesTextureSet {
    /// Construct a texture set when every registry key is non-empty.
    pub fn new(
        histogram: impl Into<String>,
        waveform: impl Into<String>,
        vectorscope: impl Into<String>,
    ) -> Option<Self> {
        let set = Self {
            histogram: histogram.into(),
            waveform: waveform.into(),
            vectorscope: vectorscope.into(),
        };
        (!set.histogram.is_empty() && !set.waveform.is_empty() && !set.vectorscope.is_empty())
            .then_some(set)
    }
}

/// Responsive waveform, histogram, and vectorscope presentation surface.
pub struct VideoScopesSurface {
    id: WidgetId,
    bounds: Rect,
    textures: Option<VideoScopesTextureSet>,
    settings: VideoScopesSettings,
    signal_color_space: ColorSpace,
    on_settings_changed: Option<Box<dyn Fn(VideoScopesSettings) -> Action>>,
    pressed_control: Option<ScopeControl>,
}

impl VideoScopesSurface {
    /// Create an empty surface that shows a waiting state until GPU textures arrive.
    pub fn new() -> Self {
        Self {
            id: WidgetId::new(),
            bounds: Rect::ZERO,
            textures: None,
            settings: VideoScopesSettings::default(),
            signal_color_space: ColorSpace::Rec709,
            on_settings_changed: None,
            pressed_control: None,
        }
    }

    /// Attach the current coherent GPU texture set.
    pub fn with_textures(mut self, textures: VideoScopesTextureSet) -> Self {
        self.textures = Some(textures);
        self
    }

    /// Replace the current texture set during the narrow playback refresh path.
    pub fn set_textures(&mut self, textures: Option<VideoScopesTextureSet>) {
        self.textures = textures;
    }

    /// Apply the complete machine-local scope presentation contract.
    pub fn with_settings(
        mut self,
        settings: VideoScopesSettings,
        signal_color_space: ColorSpace,
    ) -> Self {
        self.settings = settings;
        self.signal_color_space = signal_color_space;
        self
    }

    /// Replace settings during the narrow shell refresh path.
    pub fn set_settings(&mut self, settings: VideoScopesSettings, signal_color_space: ColorSpace) {
        self.settings = settings;
        self.signal_color_space = signal_color_space;
    }

    /// Map toolbar changes into an app-owned action.
    pub fn on_settings_changed(
        mut self,
        mapper: impl Fn(VideoScopesSettings) -> Action + 'static,
    ) -> Self {
        self.on_settings_changed = Some(Box::new(mapper));
        self
    }

    fn toolbar_rect(&self) -> Rect {
        Rect::new(
            self.bounds.x,
            self.bounds.y,
            self.bounds.width,
            TOOLBAR_HEIGHT,
        )
    }

    fn control_at(&self, point: Point) -> Option<ScopeControl> {
        let toolbar = self.toolbar_rect();
        if !toolbar.contains(point) || toolbar.width <= 0.0 {
            return None;
        }
        let index = (((point.x - toolbar.x) / toolbar.width) * CONTROL_COUNT as f32)
            .floor()
            .clamp(0.0, (CONTROL_COUNT - 1) as f32) as usize;
        Some(
            [
                ScopeControl::Waveform,
                ScopeControl::Scale,
                ScopeControl::Tap,
                ScopeControl::Skin,
                ScopeControl::Targets,
                ScopeControl::FalseColor,
                ScopeControl::Zebra,
                ScopeControl::Gamut,
                ScopeControl::Layout,
            ][index],
        )
    }

    fn changed_settings(&self, control: ScopeControl) -> VideoScopesSettings {
        let mut settings = self.settings;
        match control {
            ScopeControl::Waveform => {
                settings.waveform_mode = match settings.waveform_mode {
                    WaveformMode::Luma => WaveformMode::RgbParade,
                    WaveformMode::RgbParade => WaveformMode::Luma,
                };
            }
            ScopeControl::Scale => settings.scale = settings.scale.next(),
            ScopeControl::Tap => {
                settings.tap = match settings.tap {
                    ProgramScopesTap::ProgramOutput => ProgramScopesTap::MonitorOutput,
                    ProgramScopesTap::MonitorOutput => ProgramScopesTap::ProgramOutput,
                };
            }
            ScopeControl::Skin => settings.show_skin_tone_line = !settings.show_skin_tone_line,
            ScopeControl::Targets => settings.show_color_targets = !settings.show_color_targets,
            ScopeControl::FalseColor => {
                settings.monitoring.false_color = !settings.monitoring.false_color;
            }
            ScopeControl::Zebra => {
                if !settings.monitoring.zebra {
                    settings.monitoring.zebra = true;
                    settings.monitoring.zebra_lower_per_mille = 900;
                    settings.monitoring.zebra_upper_per_mille = 1_000;
                } else {
                    match (
                        settings.monitoring.zebra_lower_per_mille,
                        settings.monitoring.zebra_upper_per_mille,
                    ) {
                        (900, 1_000) => {
                            settings.monitoring.zebra_lower_per_mille = 700;
                            settings.monitoring.zebra_upper_per_mille = 800;
                        }
                        (700, 800) => {
                            settings.monitoring.zebra_lower_per_mille = 800;
                            settings.monitoring.zebra_upper_per_mille = 900;
                        }
                        _ => settings.monitoring.zebra = false,
                    }
                }
            }
            ScopeControl::Gamut => {
                settings.monitoring.gamut_alarm = !settings.monitoring.gamut_alarm;
            }
            ScopeControl::Layout => settings.layout = settings.layout.next(),
        }
        settings
    }

    fn scope_rects(&self, gap: f32) -> Vec<(ScopeKind, Rect)> {
        let content = Rect::new(
            self.bounds.x + gap,
            self.bounds.y + TOOLBAR_HEIGHT + gap,
            (self.bounds.width - gap * 2.0).max(1.0),
            (self.bounds.height - TOOLBAR_HEIGHT - gap * 2.0).max(1.0),
        );
        if self.settings.layout == VideoScopesLayout::Waveform {
            return vec![(ScopeKind::Waveform, content)];
        }
        if self.settings.layout == VideoScopesLayout::Histogram {
            return vec![(ScopeKind::Histogram, content)];
        }
        if self.settings.layout == VideoScopesLayout::Vectorscope {
            return vec![(ScopeKind::Vectorscope, content)];
        }
        if self.settings.layout == VideoScopesLayout::Grid {
            let width = ((content.width - gap * 2.0) / 3.0).max(1.0);
            return [
                ScopeKind::Waveform,
                ScopeKind::Histogram,
                ScopeKind::Vectorscope,
            ]
            .into_iter()
            .enumerate()
            .map(|(index, kind)| {
                (
                    kind,
                    Rect::new(
                        content.x + index as f32 * (width + gap),
                        content.y,
                        width,
                        content.height,
                    ),
                )
            })
            .collect();
        }
        let waveform_height = (content.height * 0.6 - gap * 0.5).max(1.0);
        let lower_height = (content.height - waveform_height - gap).max(1.0);
        let lower_width = ((content.width - gap) * 0.5).max(1.0);
        let waveform = Rect::new(content.x, content.y, content.width, waveform_height);
        let histogram = Rect::new(
            content.x,
            content.y + waveform_height + gap,
            lower_width,
            lower_height,
        );
        let vectorscope = Rect::new(
            content.x + lower_width + gap,
            histogram.y,
            (content.width - lower_width - gap).max(1.0),
            lower_height,
        );
        vec![
            (ScopeKind::Waveform, waveform),
            (ScopeKind::Histogram, histogram),
            (ScopeKind::Vectorscope, vectorscope),
        ]
    }

    fn control_label(&self, control: ScopeControl) -> &'static str {
        match control {
            ScopeControl::Waveform => match self.settings.waveform_mode {
                WaveformMode::Luma => "Luma",
                WaveformMode::RgbParade => "RGB Parade",
            },
            ScopeControl::Scale => match self.settings.scale {
                ProgramScopeScale::Ire => "IRE",
                ProgramScopeScale::Nits100 => "100 nits",
                ProgramScopeScale::Nits1000 => "1K nits",
                ProgramScopeScale::Nits4000 => "4K nits",
                ProgramScopeScale::Nits10000 => "10K nits",
            },
            ScopeControl::Tap => match self.settings.tap {
                ProgramScopesTap::ProgramOutput => "Program",
                ProgramScopesTap::MonitorOutput => "Monitor",
            },
            ScopeControl::Skin => {
                if self.settings.show_skin_tone_line {
                    "Skin ✓"
                } else {
                    "Skin"
                }
            }
            ScopeControl::Targets => {
                if self.settings.show_color_targets {
                    "Targets ✓"
                } else {
                    "Targets"
                }
            }
            ScopeControl::FalseColor => {
                if self.settings.monitoring.false_color {
                    "False ✓"
                } else {
                    "False"
                }
            }
            ScopeControl::Zebra => {
                if !self.settings.monitoring.zebra {
                    "Zebra"
                } else {
                    match (
                        self.settings.monitoring.zebra_lower_per_mille,
                        self.settings.monitoring.zebra_upper_per_mille,
                    ) {
                        (700, 800) => "Z 70–80",
                        (800, 900) => "Z 80–90",
                        _ => "Z 90–100",
                    }
                }
            }
            ScopeControl::Gamut => {
                if self.settings.monitoring.gamut_alarm {
                    "Gamut ✓"
                } else {
                    "Gamut"
                }
            }
            ScopeControl::Layout => match self.settings.layout {
                VideoScopesLayout::Overview => "Overview",
                VideoScopesLayout::Grid => "Grid",
                VideoScopesLayout::Waveform => "Waveform",
                VideoScopesLayout::Histogram => "Histogram",
                VideoScopesLayout::Vectorscope => "Vector",
            },
        }
    }

    fn paint_waveform_guides(&self, ctx: &mut PaintContext<'_>, rect: Rect) {
        let labels = match self.settings.scale {
            ProgramScopeScale::Ire => ["100 IRE", "50", "0 IRE"],
            ProgramScopeScale::Nits100 => ["100 nits", "50", "0"],
            ProgramScopeScale::Nits1000 => ["1000 nits", "500", "0"],
            ProgramScopeScale::Nits4000 => ["4000 nits", "2000", "0"],
            ProgramScopeScale::Nits10000 => ["10000 nits", "5000", "0"],
        };
        for (index, label) in labels.into_iter().enumerate() {
            let y = rect.y + rect.height * index as f32 * 0.5;
            ctx.encoder.draw_line(
                Point::new(rect.x, y),
                Point::new(rect.x + rect.width, y),
                ctx.theme.spacing.border_standard,
                ctx.theme.colors.border,
            );
            ctx.encoder.draw_text_box(
                label,
                ctx.theme.typography.small.font_size,
                Point::new(rect.x + 4.0, (y + 2.0).min(rect.y + rect.height - 12.0)),
                (rect.width - 8.0).max(1.0),
                ctx.theme.colors.text_tertiary,
            );
        }
    }

    fn paint_vectorscope_guides(&self, ctx: &mut PaintContext<'_>, rect: Rect) {
        let center = Point::new(rect.x + rect.width * 0.5, rect.y + rect.height * 0.5);
        let radius_x = rect.width * 0.46;
        let radius_y = rect.height * 0.46;
        let Ok(colorimetry) = ProgramSignalColorimetry::for_color_space(self.signal_color_space)
        else {
            return;
        };
        let map = |rgb: [f32; 3]| {
            let (u, v) = colorimetry.vectorscope_uv(rgb);
            Point::new(center.x + u * radius_x * 2.0, center.y - v * radius_y * 2.0)
        };
        if self.settings.show_skin_tone_line {
            ctx.encoder.draw_line(
                center,
                map([0.75, 0.57, 0.48]),
                ctx.theme.spacing.border_standard,
                ctx.theme.colors.warning,
            );
        }
        if self.settings.show_color_targets {
            for rgb in [
                [0.75, 0.0, 0.0],
                [0.75, 0.75, 0.0],
                [0.0, 0.75, 0.0],
                [0.0, 0.75, 0.75],
                [0.0, 0.0, 0.75],
                [0.75, 0.0, 0.75],
            ] {
                let point = map(rgb);
                let size = 5.0;
                let color = Color { r: rgb[0], g: rgb[1], b: rgb[2], a: 1.0 };
                ctx.encoder.draw_rect(
                    Rect::new(point.x - size, point.y - size, size * 2.0, size * 2.0),
                    color,
                    1.0,
                );
            }
        }
    }
}

impl Default for VideoScopesSurface {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for VideoScopesSurface {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(320.0, 240.0))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext<'_>) -> EventResult {
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                self.pressed_control = self.control_at(*position);
                if self.pressed_control.is_some() {
                    ctx.request_repaint();
                    EventResult::Handled
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::MouseUp { position, button: MouseButton::Left, .. } => {
                let pressed = self.pressed_control.take();
                let released = self.control_at(*position);
                if let Some(control) = pressed {
                    if released == Some(control) {
                        let settings = self.changed_settings(control);
                        if let Some(mapper) = &self.on_settings_changed {
                            (ctx.dispatch)(mapper(settings));
                        }
                    }
                    ctx.request_repaint();
                    EventResult::Handled
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::FocusLost => {
                let handled = self.pressed_control.take().is_some();
                if handled {
                    ctx.request_repaint();
                    EventResult::Handled
                } else {
                    EventResult::Ignored
                }
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext<'_>) {
        let gap = ctx.theme.spacing.sm;
        ctx.encoder.draw_rect(self.bounds, ctx.theme.colors.card, 0.0);
        let toolbar = self.toolbar_rect();
        let control_width = toolbar.width / CONTROL_COUNT as f32;
        for (index, control) in [
            ScopeControl::Waveform,
            ScopeControl::Scale,
            ScopeControl::Tap,
            ScopeControl::Skin,
            ScopeControl::Targets,
            ScopeControl::Layout,
        ]
        .into_iter()
        .enumerate()
        {
            let rect = Rect::new(
                toolbar.x + index as f32 * control_width,
                toolbar.y,
                control_width,
                toolbar.height,
            );
            if self.pressed_control == Some(control) {
                ctx.encoder.draw_rect(
                    rect,
                    ctx.theme.colors.surface_2,
                    ctx.theme.spacing.radius_sm,
                );
            }
            ctx.encoder.draw_text_box(
                self.control_label(control),
                ctx.theme.typography.small.font_size,
                Point::new(rect.x + 4.0, rect.y + 6.0),
                (rect.width - 8.0).max(1.0),
                ctx.theme.colors.text_secondary,
            );
        }
        let rects = self.scope_rects(gap);
        for (_, rect) in &rects {
            ctx.encoder
                .draw_rect(*rect, ctx.theme.colors.canvas, ctx.theme.spacing.radius_sm);
        }
        if let Some(textures) = &self.textures {
            let uv = Rect::new(0.0, 0.0, 1.0, 1.0);
            for (kind, rect) in &rects {
                let key = match kind {
                    ScopeKind::Waveform => &textures.waveform,
                    ScopeKind::Histogram => &textures.histogram,
                    ScopeKind::Vectorscope => &textures.vectorscope,
                };
                ctx.encoder.draw_external_texture(key, *rect, uv, Color::WHITE);
            }
        } else {
            let style = &ctx.theme.typography.small;
            let first = rects.first().map(|(_, rect)| *rect).unwrap_or(self.bounds);
            ctx.encoder.draw_text_box(
                "等待 Scopes tap",
                style.font_size,
                Point::new(first.x + gap, first.y + gap),
                (first.width - gap * 2.0).max(1.0),
                ctx.theme.colors.text_tertiary,
            );
        }
        for (kind, rect) in rects {
            match kind {
                ScopeKind::Waveform => self.paint_waveform_guides(ctx, rect),
                ScopeKind::Vectorscope => self.paint_vectorscope_guides(ctx, rect),
                ScopeKind::Histogram => {}
            }
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_ui_core::types::Modifiers;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn texture_set_rejects_partial_registry_identity() {
        assert!(VideoScopesTextureSet::new("hist", "wave", "vector").is_some());
        assert!(VideoScopesTextureSet::new("hist", "", "vector").is_none());
    }

    #[test]
    fn responsive_layout_keeps_all_scope_regions_inside_bounds() {
        let mut surface = VideoScopesSurface::new();
        surface.layout(Rect::new(10.0, 20.0, 500.0, 300.0));
        for (_, rect) in surface.scope_rects(8.0) {
            assert!(surface.bounds.contains(Point::new(rect.x, rect.y)));
            assert!(surface.bounds.contains(Point::new(
                rect.x + rect.width - f32::EPSILON,
                rect.y + rect.height - f32::EPSILON,
            )));
        }
    }

    #[test]
    fn every_independent_layout_stays_inside_the_scopes_surface() {
        for layout in [
            VideoScopesLayout::Overview,
            VideoScopesLayout::Grid,
            VideoScopesLayout::Waveform,
            VideoScopesLayout::Histogram,
            VideoScopesLayout::Vectorscope,
        ] {
            let mut surface = VideoScopesSurface::new().with_settings(
                VideoScopesSettings { layout, ..VideoScopesSettings::default() },
                ColorSpace::Rec709,
            );
            surface.layout(Rect::new(10.0, 20.0, 500.0, 300.0));
            let rects = surface.scope_rects(8.0);
            let expected = if matches!(
                layout,
                VideoScopesLayout::Overview | VideoScopesLayout::Grid
            ) {
                3
            } else {
                1
            };
            assert_eq!(rects.len(), expected);
            for (_, rect) in rects {
                assert!(surface.bounds.contains(Point::new(rect.x, rect.y)));
                assert!(surface.bounds.contains(Point::new(
                    rect.x + rect.width - f32::EPSILON,
                    rect.y + rect.height - f32::EPSILON,
                )));
            }
        }
    }

    #[test]
    fn presentation_guides_do_not_invalidate_scope_analysis() {
        let baseline = VideoScopesSettings::default();
        let presentation_only = VideoScopesSettings {
            layout: VideoScopesLayout::Vectorscope,
            show_skin_tone_line: false,
            show_color_targets: false,
            ..baseline
        };
        assert_eq!(
            baseline.analysis_identity(),
            presentation_only.analysis_identity()
        );

        let surface = VideoScopesSurface::new();
        assert_ne!(
            surface.changed_settings(ScopeControl::Waveform).analysis_identity(),
            baseline.analysis_identity()
        );
        assert_ne!(
            surface.changed_settings(ScopeControl::Scale).analysis_identity(),
            baseline.analysis_identity()
        );
        assert_ne!(
            surface.changed_settings(ScopeControl::Tap).analysis_identity(),
            baseline.analysis_identity()
        );
    }

    #[test]
    fn toolbar_click_emits_the_complete_changed_settings_contract() {
        let observed = Rc::new(RefCell::new(None));
        let callback_observed = Rc::clone(&observed);
        let mut surface = VideoScopesSurface::new().on_settings_changed(move |settings| {
            *callback_observed.borrow_mut() = Some(settings);
            Action::Play
        });
        surface.layout(Rect::new(0.0, 0.0, 600.0, 300.0));
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let dispatched = RefCell::new(Vec::new());
        let dispatch = |action| dispatched.borrow_mut().push(action);
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let scale_control = Point::new(100.0, TOOLBAR_HEIGHT * 0.5);

        assert_eq!(
            surface.event(
                &UiEvent::MouseDown {
                    position: scale_control,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            surface.event(
                &UiEvent::MouseUp {
                    position: scale_control,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        let settings = observed.borrow().expect("settings callback");
        assert_eq!(settings.scale, ProgramScopeScale::Nits100);
        assert_eq!(settings.waveform_mode, WaveformMode::Luma);
        assert_eq!(settings.tap, ProgramScopesTap::ProgramOutput);
        assert_eq!(dispatched.borrow().as_slice(), &[Action::Play]);
    }

    #[test]
    fn monitoring_controls_are_machine_local_and_zebra_cycles_bounded_presets() {
        let surface = VideoScopesSurface::new();
        let false_color = surface.changed_settings(ScopeControl::FalseColor);
        assert!(false_color.monitoring.false_color);
        assert_eq!(
            false_color.analysis_identity(),
            VideoScopesSettings::default().analysis_identity()
        );

        let zebra_90 = surface.changed_settings(ScopeControl::Zebra);
        assert!(zebra_90.monitoring.zebra);
        assert_eq!(zebra_90.monitoring.zebra_lower_per_mille, 900);
        let surface = VideoScopesSurface::new().with_settings(zebra_90, ColorSpace::Rec709);
        let zebra_70 = surface.changed_settings(ScopeControl::Zebra);
        assert_eq!(zebra_70.monitoring.zebra_lower_per_mille, 700);
        assert_eq!(zebra_70.monitoring.zebra_upper_per_mille, 800);
        let surface = VideoScopesSurface::new().with_settings(zebra_70, ColorSpace::Rec709);
        let zebra_80 = surface.changed_settings(ScopeControl::Zebra);
        assert_eq!(zebra_80.monitoring.zebra_lower_per_mille, 800);
        assert_eq!(zebra_80.monitoring.zebra_upper_per_mille, 900);
        let surface = VideoScopesSurface::new().with_settings(zebra_80, ColorSpace::Rec709);
        let zebra_off = surface.changed_settings(ScopeControl::Zebra);
        assert!(!zebra_off.monitoring.zebra);
        let surface = VideoScopesSurface::new().with_settings(zebra_off, ColorSpace::Rec709);
        let zebra_restart = surface.changed_settings(ScopeControl::Zebra);
        assert_eq!(zebra_restart.monitoring.zebra_lower_per_mille, 900);
        assert_eq!(zebra_restart.monitoring.zebra_upper_per_mille, 1_000);
    }
}
