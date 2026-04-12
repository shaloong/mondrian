use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Theme {
    #[default]
    System,
    Dark,
    Light,
}

impl Theme {
    pub const ALL: [Self; 3] = [Self::System, Self::Dark, Self::Light];

    pub fn display_name(self) -> &'static str {
        match self {
            Self::System => "跟随系统",
            Self::Dark => "深色",
            Self::Light => "浅色",
        }
    }

    pub fn to_system_theme(self) -> egui::SystemTheme {
        match self {
            Self::System => egui::SystemTheme::SystemDefault,
            Self::Dark => egui::SystemTheme::Dark,
            Self::Light => egui::SystemTheme::Light,
        }
    }

    fn to_egui_preference(self) -> egui::ThemePreference {
        match self {
            Self::System => egui::ThemePreference::System,
            Self::Dark => egui::ThemePreference::Dark,
            Self::Light => egui::ThemePreference::Light,
        }
    }

    fn resolve(self, ctx: &egui::Context) -> egui::Theme {
        resolve_egui_theme(self, ctx.system_theme())
    }
}

fn resolve_egui_theme(preference: Theme, system_theme: Option<egui::Theme>) -> egui::Theme {
    match preference {
        Theme::Dark => egui::Theme::Dark,
        Theme::Light => egui::Theme::Light,
        Theme::System => system_theme.unwrap_or(egui::Theme::Dark),
    }
}

#[derive(Debug, Clone)]
pub struct ThemeTokens {
    pub palette: PaletteTokens,
    pub metrics: MetricsTokens,
}

impl ThemeTokens {
    fn for_theme(theme: egui::Theme) -> Self {
        match theme {
            egui::Theme::Dark => Self {
                palette: PaletteTokens::dark(),
                metrics: MetricsTokens::default(),
            },
            egui::Theme::Light => Self {
                palette: PaletteTokens::light(),
                metrics: MetricsTokens::default(),
            },
        }
    }

    pub fn palette_mut(&mut self) -> &mut PaletteTokens {
        &mut self.palette
    }

    pub fn metrics_mut(&mut self) -> &mut MetricsTokens {
        &mut self.metrics
    }
}

#[derive(Debug, Clone)]
pub struct PaletteTokens {
    pub bg_base: egui::Color32,
    pub bg_surface: egui::Color32,
    pub bg_surface_hover: egui::Color32,
    pub bg_surface_active: egui::Color32,
    pub border_subtle: egui::Color32,
    pub border_emphasis: egui::Color32,
    pub text_primary: egui::Color32,
    pub text_muted: egui::Color32,
    pub status_warning: egui::Color32,
    pub status_success: egui::Color32,
    pub interaction_highlight: egui::Color32,
    pub status_error: egui::Color32,
    pub timeline_clip_video: egui::Color32,
    pub timeline_clip_audio: egui::Color32,
    pub timeline_playhead: egui::Color32,
    pub canvas_bg: egui::Color32,
    pub image_tint: egui::Color32,
}

impl PaletteTokens {
    fn dark() -> Self {
        Self {
            bg_base: egui::Color32::from_rgb(0x12, 0x12, 0x12),
            bg_surface: egui::Color32::from_rgb(0x2A, 0x2A, 0x2C),
            bg_surface_hover: egui::Color32::from_rgb(0x33, 0x33, 0x36),
            bg_surface_active: egui::Color32::from_rgb(0x3A, 0x3A, 0x3C),
            border_subtle: egui::Color32::from_rgb(0x76, 0x76, 0x80),
            border_emphasis: egui::Color32::from_rgb(0x76, 0x76, 0x80),
            text_primary: egui::Color32::from_rgb(0xF2, 0xF2, 0xF2),
            text_muted: egui::Color32::from_rgb(0x76, 0x76, 0x80),
            status_warning: egui::Color32::from_rgb(0xD6, 0xAA, 0x43),
            status_success: egui::Color32::from_rgb(0x73, 0xD1, 0x8F),
            interaction_highlight: egui::Color32::from_rgb(0x00, 0x6E, 0xFF),
            status_error: egui::Color32::from_rgb(0xE3, 0x6D, 0x6D),
            timeline_clip_video: egui::Color32::from_rgb(0x4D, 0x72, 0x9D),
            timeline_clip_audio: egui::Color32::from_rgb(0x4E, 0x8A, 0x6A),
            timeline_playhead: egui::Color32::from_rgb(0xE0, 0x67, 0x67),
            canvas_bg: egui::Color32::BLACK,
            image_tint: egui::Color32::WHITE,
        }
    }

    fn light() -> Self {
        Self {
            bg_base: egui::Color32::from_rgb(0xF5, 0xF5, 0xF7),
            bg_surface: egui::Color32::from_rgb(0xFF, 0xFF, 0xFF),
            bg_surface_hover: egui::Color32::from_rgb(0xF0, 0xF2, 0xF5),
            bg_surface_active: egui::Color32::from_rgb(0xE8, 0xEC, 0xF2),
            border_subtle: egui::Color32::from_rgb(0xC9, 0xCF, 0xD8),
            border_emphasis: egui::Color32::from_rgb(0xA6, 0xB2, 0xC2),
            text_primary: egui::Color32::from_rgb(0x1E, 0x23, 0x2C),
            text_muted: egui::Color32::from_rgb(0x5A, 0x67, 0x7A),
            status_warning: egui::Color32::from_rgb(0xB5, 0x78, 0x08),
            status_success: egui::Color32::from_rgb(0x1D, 0x89, 0x48),
            interaction_highlight: egui::Color32::from_rgb(0x00, 0x5F, 0xD9),
            status_error: egui::Color32::from_rgb(0xC1, 0x3C, 0x3C),
            timeline_clip_video: egui::Color32::from_rgb(0x80, 0x9F, 0xC4),
            timeline_clip_audio: egui::Color32::from_rgb(0x86, 0xB2, 0x95),
            timeline_playhead: egui::Color32::from_rgb(0xC0, 0x54, 0x54),
            canvas_bg: egui::Color32::from_rgb(0x14, 0x14, 0x14),
            image_tint: egui::Color32::WHITE,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MetricsTokens {
    pub item_spacing: egui::Vec2,
    pub button_padding: egui::Vec2,
    pub button_rounding: f32,
    pub interact_height: f32,
    pub menu_rounding: f32,
    pub window_rounding: f32,
    pub icon_size: f32,
    pub icon_raster_min_size: f32,
    pub icon_text_inset_x: f32,
    pub icon_text_inset_y: f32,
    pub checkmark_stroke_width: f32,
    pub checkmark_start_x: f32,
    pub drop_overlay_radius: f32,
    pub drop_overlay_stroke_width: f32,
    pub font_small: f32,
    pub font_body: f32,
    pub font_large: f32,
    pub font_mono_small: f32,
    pub font_mono_large: f32,
    pub list_row_radius: f32,
    pub list_compact_spacing_x: f32,
    pub list_row_height: f32,
    pub list_min_height: f32,
    pub list_empty_height: f32,
    pub list_icon_column_width: f32,
    pub list_row_content_height: f32,
    pub list_row_edit_min_width: f32,
    pub list_row_edit_padding: f32,
    pub list_row_name_min_width: f32,
    pub list_proxy_tag_width: f32,
    pub list_proxy_tag_font_size: f32,
    pub export_grid_spacing: [f32; 2],
    pub ai_workflow_editor_max_height: f32,
    pub ai_workflow_log_max_height: f32,
    pub ai_workflow_editor_rows: usize,
    pub timeline_toolbar_button_size: [f32; 2],
    pub timeline_clip_radius: f32,
    pub timeline_track_height: f32,
    pub timeline_ruler_height: f32,
    pub timeline_track_label_width: f32,
    pub timeline_min_pixels_per_frame: f32,
    pub timeline_max_pixels_per_frame: f32,
    pub timeline_drag_snap_pixels: f32,
    pub timeline_default_pixels_per_frame: f32,
    pub timeline_right_padding_frames_min: i64,
    pub timeline_right_padding_frames_multiplier: i64,
    pub timeline_ruler_minor_tick_height: f32,
    pub timeline_ruler_major_tick_height: f32,
    pub timeline_ruler_label_inset_x: f32,
    pub timeline_ruler_label_inset_y: f32,
    pub timeline_track_label_text_inset_x: f32,
    pub timeline_track_icon_size: f32,
    pub timeline_track_lock_offset_x: f32,
    pub timeline_track_mode_offset_x: f32,
    pub timeline_clip_top_inset: f32,
    pub timeline_clip_bottom_inset: f32,
    pub timeline_clip_label_padding_x: f32,
    pub timeline_clip_label_min_width: f32,
    pub timeline_clip_ghost_min_width: f32,
    pub timeline_clip_ghost_padding_x: f32,
    pub timeline_clip_ghost_padding_y: f32,
    pub timeline_selection_stroke_width: f32,
    pub timeline_drop_stroke_width: f32,
    pub timeline_insert_guide_width: f32,
    pub timeline_linked_audio_highlight_width: f32,
    pub timeline_playhead_stroke_width: f32,
    pub timeline_playhead_secondary_stroke_width: f32,
}

impl Default for MetricsTokens {
    fn default() -> Self {
        Self {
            item_spacing: egui::vec2(7.0, 7.0),
            button_padding: egui::vec2(10.0, 5.0),
            button_rounding: 4.0,
            interact_height: 23.0,
            menu_rounding: 3.0,
            window_rounding: 4.0,
            icon_size: 14.0,
            icon_raster_min_size: 12.0,
            icon_text_inset_x: 6.0,
            icon_text_inset_y: 7.0,
            checkmark_stroke_width: 1.35,
            checkmark_start_x: 7.0,
            drop_overlay_radius: 4.0,
            drop_overlay_stroke_width: 1.5,
            font_small: 10.0,
            font_body: 12.0,
            font_large: 14.0,
            font_mono_small: 10.0,
            font_mono_large: 24.0,
            list_row_radius: 3.0,
            list_compact_spacing_x: 4.0,
            list_row_height: 36.0,
            list_min_height: 120.0,
            list_empty_height: 160.0,
            list_icon_column_width: 20.0,
            list_row_content_height: 18.0,
            list_row_edit_min_width: 80.0,
            list_row_edit_padding: 12.0,
            list_row_name_min_width: 32.0,
            list_proxy_tag_width: 34.0,
            list_proxy_tag_font_size: 11.0,
            export_grid_spacing: [12.0, 4.0],
            ai_workflow_editor_max_height: 240.0,
            ai_workflow_log_max_height: 180.0,
            ai_workflow_editor_rows: 12,
            timeline_toolbar_button_size: [24.0, 22.0],
            timeline_clip_radius: 3.0,
            timeline_track_height: 40.0,
            timeline_ruler_height: 24.0,
            timeline_track_label_width: 80.0,
            timeline_min_pixels_per_frame: 0.02,
            timeline_max_pixels_per_frame: 64.0,
            timeline_drag_snap_pixels: 10.0,
            timeline_default_pixels_per_frame: 4.0,
            timeline_right_padding_frames_min: 240,
            timeline_right_padding_frames_multiplier: 20,
            timeline_ruler_minor_tick_height: 6.0,
            timeline_ruler_major_tick_height: 4.0,
            timeline_ruler_label_inset_x: 2.0,
            timeline_ruler_label_inset_y: 4.0,
            timeline_track_label_text_inset_x: 6.0,
            timeline_track_icon_size: 14.0,
            timeline_track_lock_offset_x: 10.0,
            timeline_track_mode_offset_x: 28.0,
            timeline_clip_top_inset: 2.0,
            timeline_clip_bottom_inset: 4.0,
            timeline_clip_label_padding_x: 4.0,
            timeline_clip_label_min_width: 24.0,
            timeline_clip_ghost_min_width: 8.0,
            timeline_clip_ghost_padding_x: 4.0,
            timeline_clip_ghost_padding_y: 4.0,
            timeline_selection_stroke_width: 2.0,
            timeline_drop_stroke_width: 1.5,
            timeline_insert_guide_width: 2.0,
            timeline_linked_audio_highlight_width: 1.5,
            timeline_playhead_stroke_width: 2.0,
            timeline_playhead_secondary_stroke_width: 1.8,
        }
    }
}

pub trait ThemeTokenOverride: Send + Sync {
    fn override_tokens(&self, preference: Theme, resolved: egui::Theme, tokens: &mut ThemeTokens);
}

fn token_overrides() -> &'static RwLock<Vec<Arc<dyn ThemeTokenOverride>>> {
    static TOKEN_OVERRIDES: OnceLock<RwLock<Vec<Arc<dyn ThemeTokenOverride>>>> = OnceLock::new();
    TOKEN_OVERRIDES.get_or_init(|| RwLock::new(Vec::new()))
}

fn active_tokens() -> &'static RwLock<ThemeTokens> {
    static ACTIVE_TOKENS: OnceLock<RwLock<ThemeTokens>> = OnceLock::new();
    ACTIVE_TOKENS.get_or_init(|| RwLock::new(ThemeTokens::for_theme(egui::Theme::Dark)))
}

fn with_active_tokens<R>(f: impl FnOnce(&ThemeTokens) -> R) -> R {
    let lock = active_tokens();
    match lock.read() {
        Ok(tokens) => f(&tokens),
        Err(_) => f(&ThemeTokens::for_theme(egui::Theme::Dark)),
    }
}

fn set_active_tokens(tokens: ThemeTokens) {
    let lock = active_tokens();
    if let Ok(mut guard) = lock.write() {
        *guard = tokens;
    }
}

fn resolve_tokens(preference: Theme, resolved: egui::Theme) -> ThemeTokens {
    let mut tokens = ThemeTokens::for_theme(resolved);
    if let Ok(overrides) = token_overrides().read() {
        for override_provider in overrides.iter() {
            override_provider.override_tokens(preference, resolved, &mut tokens);
        }
    }
    tokens
}

fn build_visuals(theme: egui::Theme, tokens: &ThemeTokens) -> egui::Visuals {
    let mut visuals = match theme {
        egui::Theme::Dark => egui::Visuals::dark(),
        egui::Theme::Light => egui::Visuals::light(),
    };
    let p = &tokens.palette;
    let m = &tokens.metrics;

    visuals.dark_mode = matches!(theme, egui::Theme::Dark);
    visuals.override_text_color = Some(p.text_primary);
    visuals.panel_fill = p.bg_base;
    visuals.window_fill = p.bg_surface;
    visuals.faint_bg_color = p.bg_surface;
    visuals.extreme_bg_color = p.bg_base;
    visuals.code_bg_color = p.bg_surface;

    visuals.widgets.noninteractive.bg_fill = p.bg_base;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, p.border_subtle);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, p.text_muted);
    visuals.widgets.noninteractive.rounding = egui::Rounding::same(m.button_rounding);

    visuals.widgets.inactive.bg_fill = p.bg_surface;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, p.border_subtle);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, p.text_primary);
    visuals.widgets.inactive.rounding = egui::Rounding::same(m.button_rounding);

    visuals.widgets.hovered.bg_fill = p.bg_surface;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, p.border_emphasis);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, p.text_primary);
    visuals.widgets.hovered.rounding = egui::Rounding::same(m.button_rounding);

    visuals.widgets.active.bg_fill = p.bg_surface_active;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, p.text_primary);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, p.text_primary);
    visuals.widgets.active.rounding = egui::Rounding::same(m.button_rounding);

    visuals.widgets.open.bg_fill = p.bg_surface;
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, p.border_emphasis);
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, p.text_primary);
    visuals.widgets.open.rounding = egui::Rounding::same(m.button_rounding);

    visuals.selection.bg_fill = p.bg_surface_active;
    visuals.selection.stroke = egui::Stroke::new(1.0, p.text_primary);
    visuals.window_stroke = egui::Stroke::new(1.0, p.border_subtle);
    visuals.hyperlink_color = p.interaction_highlight;
    visuals.menu_rounding = m.menu_rounding.into();
    visuals.window_rounding = m.window_rounding.into();
    visuals
}

fn apply_style(ctx: &egui::Context, tokens: &ThemeTokens) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = tokens.metrics.item_spacing;
    style.spacing.button_padding = tokens.metrics.button_padding;
    style.spacing.interact_size.y = tokens.metrics.interact_height;
    style.visuals.window_fill = tokens.palette.bg_surface;
    style.visuals.panel_fill = tokens.palette.bg_base;
    ctx.set_style(style);
}

pub fn register_theme_override(override_provider: Arc<dyn ThemeTokenOverride>) {
    if let Ok(mut providers) = token_overrides().write() {
        providers.push(override_provider);
    }
}

pub fn clear_theme_overrides() {
    if let Ok(mut providers) = token_overrides().write() {
        providers.clear();
    }
}

pub fn apply_theme(ctx: &egui::Context, theme: Theme) {
    ctx.set_theme(theme.to_egui_preference());

    let dark_tokens = resolve_tokens(theme, egui::Theme::Dark);
    let light_tokens = resolve_tokens(theme, egui::Theme::Light);
    ctx.set_visuals_of(
        egui::Theme::Dark,
        build_visuals(egui::Theme::Dark, &dark_tokens),
    );
    ctx.set_visuals_of(
        egui::Theme::Light,
        build_visuals(egui::Theme::Light, &light_tokens),
    );

    let resolved = theme.resolve(ctx);
    let active = match resolved {
        egui::Theme::Dark => dark_tokens,
        egui::Theme::Light => light_tokens,
    };
    apply_style(ctx, &active);
    set_active_tokens(active);
}

pub fn with_minimal_dropdown<R>(
    ui: &mut egui::Ui,
    is_open: bool,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.scope(|ui| {
        let mut style: egui::Style = ui.style().as_ref().clone();

        for w in [
            &mut style.visuals.widgets.noninteractive,
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.open,
        ] {
            w.bg_fill = egui::Color32::TRANSPARENT;
            w.weak_bg_fill = egui::Color32::TRANSPARENT;
            w.bg_stroke = egui::Stroke::NONE;
            w.fg_stroke = if is_open {
                egui::Stroke::new(1.0, palette::text_primary())
            } else {
                egui::Stroke::new(1.0, palette::text_muted())
            };
        }

        ui.set_style(style);
        add_contents(ui)
    })
    .inner
}

/// 带勾选标记的选择项
pub fn checkmark_selectable_value<V: PartialEq>(
    ui: &mut egui::Ui,
    current: &mut V,
    value: V,
    text: impl Into<String>,
) -> egui::Response {
    let selected = *current == value;
    let label = format!("    {}", text.into());
    let response = ui.selectable_label(false, label);

    if selected {
        draw_checkmark_glyph(ui.painter(), response.rect);
    }

    if response.clicked() {
        *current = value;
    }
    response
}

pub fn checkmark_toggle(
    ui: &mut egui::Ui,
    current: &mut bool,
    text: impl Into<String>,
) -> egui::Response {
    let label = format!("      {}", text.into());
    let response = ui.selectable_label(false, label);

    if *current {
        draw_checkmark_glyph(ui.painter(), response.rect);
    }

    if response.clicked() {
        *current = !*current;
    }

    response
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UiIcon {
    Search,
    Video,
    Audio,
    Warning,
    Info,
    Play,
    Pause,
    StepBack,
    StepForward,
    JumpStart,
    JumpEnd,
    Cursor,
    Scissors,
    Magnet,
    Eye,
    EyeOff,
    Lock,
    Unlock,
    Speaker,
    Mute,
}

pub fn icon(ui: &mut egui::Ui, kind: UiIcon, color: egui::Color32) -> egui::Response {
    let icon_size = tokens::icon_size();
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(icon_size, icon_size), egui::Sense::hover());
    draw_icon(ui.painter(), rect, kind, color);
    response
}

pub fn icon_button(ui: &mut egui::Ui, size: [f32; 2], kind: UiIcon) -> egui::Response {
    let response = ui.add_sized(size, egui::Button::new(""));
    let icon_size = tokens::icon_size();
    let icon_rect =
        egui::Rect::from_center_size(response.rect.center(), egui::vec2(icon_size, icon_size));
    draw_icon(ui.painter(), icon_rect, kind, ui.visuals().text_color());
    response
}

pub fn icon_toggle_button(
    ui: &mut egui::Ui,
    size: [f32; 2],
    kind: UiIcon,
    selected: bool,
) -> egui::Response {
    let response = ui.add_sized(size, egui::Button::new("").selected(selected));
    let icon_size = tokens::icon_size();
    let icon_rect =
        egui::Rect::from_center_size(response.rect.center(), egui::vec2(icon_size, icon_size));
    let icon_color = ui.visuals().text_color();
    draw_icon(ui.painter(), icon_rect, kind, icon_color);
    response
}

pub fn icon_text_button(
    ui: &mut egui::Ui,
    kind: UiIcon,
    text: impl Into<String>,
) -> egui::Response {
    let response = ui.button(format!("    {}", text.into()));
    let icon_size = tokens::icon_size();
    let inset_x = tokens::icon_text_inset_x();
    let inset_y = tokens::icon_text_inset_y();
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(
            response.rect.left() + inset_x,
            response.rect.center().y - inset_y,
        ),
        egui::vec2(icon_size, icon_size),
    );
    draw_icon(ui.painter(), icon_rect, kind, ui.visuals().text_color());
    response
}

pub fn draw_icon(painter: &egui::Painter, rect: egui::Rect, kind: UiIcon, color: egui::Color32) {
    let side = rect.width().max(rect.height()).round().max(tokens::icon_raster_min_size()) as u32;
    let texture = icon_texture(painter.ctx(), kind, side);
    painter.image(
        texture.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        color,
    );
}

fn icon_texture(ctx: &egui::Context, kind: UiIcon, side: u32) -> egui::TextureHandle {
    static ICON_CACHE: OnceLock<Mutex<HashMap<(UiIcon, u32), egui::TextureHandle>>> =
        OnceLock::new();
    let cache = ICON_CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    if let Some(found) = cache.lock().expect("icon cache lock poisoned").get(&(kind, side)).cloned()
    {
        return found;
    }

    let image = rasterize_svg(icon_svg_bytes(kind), side, side);
    let texture = ctx.load_texture(
        format!("svg_icon_{kind:?}_{side}"),
        image,
        egui::TextureOptions::LINEAR,
    );
    cache
        .lock()
        .expect("icon cache lock poisoned")
        .insert((kind, side), texture.clone());
    texture
}

fn rasterize_svg(svg_bytes: &[u8], width: u32, height: u32) -> egui::ColorImage {
    let options = usvg::Options::default();
    let tree = usvg::Tree::from_data(svg_bytes, &options).expect("invalid svg icon data");
    let mut pixmap =
        tiny_skia::Pixmap::new(width, height).expect("failed to create icon raster surface");

    let svg_size = tree.size();
    let scale_x = width as f32 / svg_size.width();
    let scale_y = height as f32 / svg_size.height();
    let transform = tiny_skia::Transform::from_scale(scale_x, scale_y);

    resvg::render(&tree, transform, &mut pixmap.as_mut());

    for rgba in pixmap.data_mut().chunks_exact_mut(4) {
        if rgba[3] > 0 {
            rgba[0] = 255;
            rgba[1] = 255;
            rgba[2] = 255;
        }
    }

    egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], pixmap.data())
}

fn icon_svg_bytes(kind: UiIcon) -> &'static [u8] {
    match kind {
        UiIcon::Search => include_bytes!("../../assets/icons/search.svg"),
        UiIcon::Video => include_bytes!("../../assets/icons/film.svg"),
        UiIcon::Audio => include_bytes!("../../assets/icons/music.svg"),
        UiIcon::Warning => include_bytes!("../../assets/icons/info.svg"),
        UiIcon::Info => include_bytes!("../../assets/icons/info.svg"),
        UiIcon::Play => include_bytes!("../../assets/icons/play_fill.svg"),
        UiIcon::Pause => include_bytes!("../../assets/icons/pause_fill.svg"),
        UiIcon::StepBack => include_bytes!("../../assets/icons/left_frame_fill.svg"),
        UiIcon::StepForward => include_bytes!("../../assets/icons/right_frame_fill.svg"),
        UiIcon::JumpStart => include_bytes!("../../assets/icons/home_frame_fill.svg"),
        UiIcon::JumpEnd => include_bytes!("../../assets/icons/end_frame_fill.svg"),
        UiIcon::Cursor => include_bytes!("../../assets/icons/cursor.svg"),
        UiIcon::Scissors => include_bytes!("../../assets/icons/cut.svg"),
        UiIcon::Magnet => include_bytes!("../../assets/icons/magnet.svg"),
        UiIcon::Eye => include_bytes!("../../assets/icons/eye_visiable.svg"),
        UiIcon::EyeOff => include_bytes!("../../assets/icons/eye_invisiable.svg"),
        UiIcon::Lock => include_bytes!("../../assets/icons/lock.svg"),
        UiIcon::Unlock => include_bytes!("../../assets/icons/unlock.svg"),
        UiIcon::Speaker => include_bytes!("../../assets/icons/speaker.svg"),
        UiIcon::Mute => include_bytes!("../../assets/icons/speaker_muted.svg"),
    }
}

fn draw_checkmark_glyph(painter: &egui::Painter, rect: egui::Rect) {
    let center_y = rect.center().y - 0.5;
    let start_x = rect.left() + tokens::checkmark_start_x();
    let stroke = egui::Stroke::new(tokens::checkmark_stroke_width(), palette::text_primary());

    painter.line_segment(
        [
            egui::pos2(start_x, center_y + 1.5),
            egui::pos2(start_x + 3.0, center_y + 4.2),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(start_x + 3.0, center_y + 4.2),
            egui::pos2(start_x + 9.2, center_y - 3.0),
        ],
        stroke,
    );
}

pub fn draw_drop_overlay(painter: &egui::Painter, rect: egui::Rect, message: &str) {
    let radius = tokens::drop_overlay_radius();
    painter.rect_filled(rect, radius, palette::drop_overlay_fill());
    painter.rect_stroke(
        rect.shrink(2.0),
        radius,
        egui::Stroke::new(
            tokens::drop_overlay_stroke_width(),
            palette::drop_overlay_stroke(),
        ),
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        message,
        typography::body_large(),
        palette::drop_overlay_text(),
    );
}

pub mod typography {
    pub fn body() -> egui::FontId {
        super::with_active_tokens(|tokens| egui::FontId::proportional(tokens.metrics.font_body))
    }

    pub fn body_small() -> egui::FontId {
        super::with_active_tokens(|tokens| egui::FontId::proportional(tokens.metrics.font_small))
    }

    pub fn body_large() -> egui::FontId {
        super::with_active_tokens(|tokens| egui::FontId::proportional(tokens.metrics.font_large))
    }

    pub fn mono_small() -> egui::FontId {
        super::with_active_tokens(|tokens| egui::FontId::monospace(tokens.metrics.font_mono_small))
    }

    pub fn mono_large() -> egui::FontId {
        super::with_active_tokens(|tokens| egui::FontId::monospace(tokens.metrics.font_mono_large))
    }
}

pub mod tokens {
    pub fn icon_size() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.icon_size)
    }

    pub fn icon_raster_min_size() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.icon_raster_min_size)
    }

    pub fn icon_text_inset_x() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.icon_text_inset_x)
    }

    pub fn icon_text_inset_y() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.icon_text_inset_y)
    }

    pub fn checkmark_stroke_width() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.checkmark_stroke_width)
    }

    pub fn checkmark_start_x() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.checkmark_start_x)
    }

    pub fn drop_overlay_radius() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.drop_overlay_radius)
    }

    pub fn drop_overlay_stroke_width() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.drop_overlay_stroke_width)
    }

    pub fn button_rounding() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.button_rounding)
    }

    pub fn list_row_radius() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.list_row_radius)
    }

    pub fn list_compact_spacing_x() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.list_compact_spacing_x)
    }

    pub fn list_row_height() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.list_row_height)
    }

    pub fn list_min_height() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.list_min_height)
    }

    pub fn list_empty_height() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.list_empty_height)
    }

    pub fn list_icon_column_width() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.list_icon_column_width)
    }

    pub fn list_row_content_height() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.list_row_content_height)
    }

    pub fn list_row_edit_min_width() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.list_row_edit_min_width)
    }

    pub fn list_row_edit_padding() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.list_row_edit_padding)
    }

    pub fn list_proxy_tag_width() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.list_proxy_tag_width)
    }

    pub fn list_proxy_tag_font_size() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.list_proxy_tag_font_size)
    }

    pub fn list_row_name_min_width() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.list_row_name_min_width)
    }

    pub fn ai_workflow_editor_max_height() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.ai_workflow_editor_max_height)
    }

    pub fn ai_workflow_log_max_height() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.ai_workflow_log_max_height)
    }

    pub fn ai_workflow_editor_rows() -> usize {
        super::with_active_tokens(|tokens| tokens.metrics.ai_workflow_editor_rows)
    }

    pub fn export_grid_spacing() -> [f32; 2] {
        super::with_active_tokens(|tokens| tokens.metrics.export_grid_spacing)
    }

    pub fn timeline_toolbar_button_size() -> [f32; 2] {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_toolbar_button_size)
    }

    pub fn timeline_clip_radius() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_clip_radius)
    }

    pub fn timeline_track_height() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_track_height)
    }

    pub fn timeline_ruler_height() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_ruler_height)
    }

    pub fn timeline_track_label_width() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_track_label_width)
    }

    pub fn timeline_min_pixels_per_frame() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_min_pixels_per_frame)
    }

    pub fn timeline_max_pixels_per_frame() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_max_pixels_per_frame)
    }

    pub fn timeline_drag_snap_pixels() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_drag_snap_pixels)
    }

    pub fn timeline_default_pixels_per_frame() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_default_pixels_per_frame)
    }

    pub fn timeline_right_padding_frames_min() -> i64 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_right_padding_frames_min)
    }

    pub fn timeline_right_padding_frames_multiplier() -> i64 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_right_padding_frames_multiplier)
    }

    pub fn timeline_ruler_minor_tick_height() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_ruler_minor_tick_height)
    }

    pub fn timeline_ruler_major_tick_height() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_ruler_major_tick_height)
    }

    pub fn timeline_ruler_label_inset_x() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_ruler_label_inset_x)
    }

    pub fn timeline_ruler_label_inset_y() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_ruler_label_inset_y)
    }

    pub fn timeline_track_label_text_inset_x() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_track_label_text_inset_x)
    }

    pub fn timeline_track_icon_size() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_track_icon_size)
    }

    pub fn timeline_track_lock_offset_x() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_track_lock_offset_x)
    }

    pub fn timeline_track_mode_offset_x() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_track_mode_offset_x)
    }

    pub fn timeline_clip_top_inset() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_clip_top_inset)
    }

    pub fn timeline_clip_bottom_inset() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_clip_bottom_inset)
    }

    pub fn timeline_clip_label_padding_x() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_clip_label_padding_x)
    }

    pub fn timeline_clip_label_min_width() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_clip_label_min_width)
    }

    pub fn timeline_clip_ghost_min_width() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_clip_ghost_min_width)
    }

    pub fn timeline_clip_ghost_padding_x() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_clip_ghost_padding_x)
    }

    pub fn timeline_clip_ghost_padding_y() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_clip_ghost_padding_y)
    }

    pub fn timeline_selection_stroke_width() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_selection_stroke_width)
    }

    pub fn timeline_drop_stroke_width() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_drop_stroke_width)
    }

    pub fn timeline_insert_guide_width() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_insert_guide_width)
    }

    pub fn timeline_linked_audio_highlight_width() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_linked_audio_highlight_width)
    }

    pub fn timeline_playhead_stroke_width() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_playhead_stroke_width)
    }

    pub fn timeline_playhead_secondary_stroke_width() -> f32 {
        super::with_active_tokens(|tokens| tokens.metrics.timeline_playhead_secondary_stroke_width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().expect("test lock poisoned")
    }

    #[test]
    fn theme_labels_and_system_mapping_are_stable() {
        let _guard = test_lock();

        assert_eq!(Theme::System.display_name(), "跟随系统");
        assert_eq!(Theme::Dark.display_name(), "深色");
        assert_eq!(Theme::Light.display_name(), "浅色");
        assert_eq!(
            Theme::System.to_system_theme(),
            egui::SystemTheme::SystemDefault
        );
        assert_eq!(Theme::Dark.to_system_theme(), egui::SystemTheme::Dark);
        assert_eq!(Theme::Light.to_system_theme(), egui::SystemTheme::Light);
    }

    #[test]
    fn system_theme_falls_back_to_dark_when_system_theme_is_missing() {
        let _guard = test_lock();

        assert_eq!(resolve_egui_theme(Theme::System, None), egui::Theme::Dark);
        assert_eq!(
            resolve_egui_theme(Theme::System, Some(egui::Theme::Light)),
            egui::Theme::Light
        );
        assert_eq!(
            resolve_egui_theme(Theme::System, Some(egui::Theme::Dark)),
            egui::Theme::Dark
        );
    }

    #[test]
    fn token_override_updates_active_tokens_after_apply_theme() {
        let _guard = test_lock();

        clear_theme_overrides();

        struct TestOverride;

        impl ThemeTokenOverride for TestOverride {
            fn override_tokens(
                &self,
                _preference: Theme,
                _resolved: egui::Theme,
                tokens: &mut ThemeTokens,
            ) {
                tokens.palette.bg_base = egui::Color32::from_rgb(1, 2, 3);
                tokens.metrics.font_body = 19.0;
            }
        }

        register_theme_override(Arc::new(TestOverride));

        let ctx = egui::Context::default();
        apply_theme(&ctx, Theme::Dark);

        assert_eq!(palette::bg_base(), egui::Color32::from_rgb(1, 2, 3));
        assert_eq!(typography::body().size, 19.0);

        clear_theme_overrides();
    }

    #[test]
    fn visuals_use_button_rounding_token_for_all_widget_states() {
        let _guard = test_lock();

        let mut tokens = ThemeTokens::for_theme(egui::Theme::Dark);
        tokens.metrics.button_rounding = 4.0;

        let visuals = build_visuals(egui::Theme::Dark, &tokens);
        let expected = egui::Rounding::same(4.0);

        assert_eq!(visuals.widgets.noninteractive.rounding, expected);
        assert_eq!(visuals.widgets.inactive.rounding, expected);
        assert_eq!(visuals.widgets.hovered.rounding, expected);
        assert_eq!(visuals.widgets.active.rounding, expected);
        assert_eq!(visuals.widgets.open.rounding, expected);
    }
}

pub mod palette {
    pub fn bg_base() -> egui::Color32 {
        super::with_active_tokens(|tokens| tokens.palette.bg_base)
    }

    pub fn bg_surface() -> egui::Color32 {
        super::with_active_tokens(|tokens| tokens.palette.bg_surface)
    }

    pub fn bg_surface_hover() -> egui::Color32 {
        super::with_active_tokens(|tokens| tokens.palette.bg_surface_hover)
    }

    pub fn bg_surface_active() -> egui::Color32 {
        super::with_active_tokens(|tokens| tokens.palette.bg_surface_active)
    }

    pub fn border_subtle() -> egui::Color32 {
        super::with_active_tokens(|tokens| tokens.palette.border_subtle)
    }

    pub fn border_emphasis() -> egui::Color32 {
        super::with_active_tokens(|tokens| tokens.palette.border_emphasis)
    }

    pub fn text_primary() -> egui::Color32 {
        super::with_active_tokens(|tokens| tokens.palette.text_primary)
    }

    pub fn text_muted() -> egui::Color32 {
        super::with_active_tokens(|tokens| tokens.palette.text_muted)
    }

    pub fn status_warning() -> egui::Color32 {
        super::with_active_tokens(|tokens| tokens.palette.status_warning)
    }

    pub fn status_success() -> egui::Color32 {
        super::with_active_tokens(|tokens| tokens.palette.status_success)
    }

    pub fn interaction_highlight() -> egui::Color32 {
        super::with_active_tokens(|tokens| tokens.palette.interaction_highlight)
    }

    pub fn status_error() -> egui::Color32 {
        super::with_active_tokens(|tokens| tokens.palette.status_error)
    }

    pub fn drop_overlay_fill() -> egui::Color32 {
        bg_surface().gamma_multiply(0.78)
    }

    pub fn drop_overlay_stroke() -> egui::Color32 {
        interaction_highlight()
    }

    pub fn drop_overlay_text() -> egui::Color32 {
        interaction_highlight()
    }

    pub fn timeline_clip_video() -> egui::Color32 {
        super::with_active_tokens(|tokens| tokens.palette.timeline_clip_video)
    }

    pub fn timeline_clip_audio() -> egui::Color32 {
        super::with_active_tokens(|tokens| tokens.palette.timeline_clip_audio)
    }

    pub fn timeline_playhead() -> egui::Color32 {
        super::with_active_tokens(|tokens| tokens.palette.timeline_playhead)
    }

    pub fn canvas_bg() -> egui::Color32 {
        super::with_active_tokens(|tokens| tokens.palette.canvas_bg)
    }

    pub fn image_tint() -> egui::Color32 {
        super::with_active_tokens(|tokens| tokens.palette.image_tint)
    }
}
