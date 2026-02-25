use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Dark,
}

pub fn apply_theme(ctx: &egui::Context, theme: Theme) {
    match theme {
        Theme::Dark => apply_dark_theme(ctx),
    }
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
    let (rect, response) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    draw_icon(ui.painter(), rect, kind, color);
    response
}

pub fn icon_button(ui: &mut egui::Ui, size: [f32; 2], kind: UiIcon) -> egui::Response {
    let response = ui.add_sized(size, egui::Button::new(""));
    let icon_rect = egui::Rect::from_center_size(response.rect.center(), egui::vec2(14.0, 14.0));
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
    let icon_rect = egui::Rect::from_center_size(response.rect.center(), egui::vec2(14.0, 14.0));
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
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(response.rect.left() + 6.0, response.rect.center().y - 7.0),
        egui::vec2(14.0, 14.0),
    );
    draw_icon(ui.painter(), icon_rect, kind, ui.visuals().text_color());
    response
}

pub fn draw_icon(painter: &egui::Painter, rect: egui::Rect, kind: UiIcon, color: egui::Color32) {
    let side = rect.width().max(rect.height()).round().max(12.0) as u32;
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
        UiIcon::Play => include_bytes!("../../assets/icons/play.svg"),
        UiIcon::Pause => include_bytes!("../../assets/icons/pause.svg"),
        UiIcon::StepBack => include_bytes!("../../assets/icons/left_frame.svg"),
        UiIcon::StepForward => include_bytes!("../../assets/icons/right_frame.svg"),
        UiIcon::JumpStart => include_bytes!("../../assets/icons/home_Frame.svg"),
        UiIcon::JumpEnd => include_bytes!("../../assets/icons/end_frame.svg"),
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
    let start_x = rect.left() + 7.0;
    let stroke = egui::Stroke::new(1.35, palette::text_primary());

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
    painter.rect_filled(rect, 4.0, palette::drop_overlay_fill());
    painter.rect_stroke(
        rect.shrink(2.0),
        4.0,
        egui::Stroke::new(1.5, palette::drop_overlay_stroke()),
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        message,
        egui::FontId::proportional(14.0),
        palette::drop_overlay_text(),
    );
}

fn apply_dark_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.dark_mode = true;

    visuals.override_text_color = Some(palette::text_primary());
    visuals.panel_fill = palette::bg_base();
    visuals.window_fill = palette::bg_surface();
    visuals.faint_bg_color = palette::bg_surface();
    visuals.extreme_bg_color = palette::bg_base();
    visuals.code_bg_color = palette::bg_surface();

    visuals.widgets.noninteractive.bg_fill = palette::bg_base();
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, palette::border_subtle());
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, palette::text_muted());

    visuals.widgets.inactive.bg_fill = palette::bg_surface();
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, palette::border_subtle());
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, palette::text_primary());

    visuals.widgets.hovered.bg_fill = palette::bg_surface();
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, palette::border_emphasis());
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, palette::text_primary());

    visuals.widgets.active.bg_fill = palette::bg_surface_active();
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, palette::text_primary());
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, palette::text_primary());

    visuals.widgets.open.bg_fill = palette::bg_surface();
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, palette::border_emphasis());
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, palette::text_primary());

    visuals.selection.bg_fill = palette::bg_surface_active();
    visuals.selection.stroke = egui::Stroke::new(1.0, palette::text_primary());
    visuals.window_stroke = egui::Stroke::new(1.0, palette::border_subtle());
    visuals.hyperlink_color = palette::text_primary();
    visuals.menu_rounding = 3.0.into();
    visuals.window_rounding = 4.0.into();

    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(7.0, 7.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.interact_size.y = 23.0;
    style.visuals.window_fill = palette::bg_surface();
    style.visuals.panel_fill = palette::bg_base();
    ctx.set_style(style);
}

pub mod palette {
    use egui::Color32;

    pub fn bg_base() -> Color32 {
        Color32::from_rgb(0x12, 0x12, 0x12)
    }

    pub fn bg_surface() -> Color32 {
        Color32::from_rgb(0x2A, 0x2A, 0x2C)
    }

    pub fn bg_surface_hover() -> Color32 {
        Color32::from_rgb(0x33, 0x33, 0x36)
    }

    pub fn bg_surface_active() -> Color32 {
        Color32::from_rgb(0x3A, 0x3A, 0x3C)
    }

    pub fn border_subtle() -> Color32 {
        Color32::from_rgb(0x76, 0x76, 0x80)
    }

    pub fn border_emphasis() -> Color32 {
        Color32::from_rgb(0x76, 0x76, 0x80)
    }

    pub fn text_primary() -> Color32 {
        Color32::from_rgb(0xF2, 0xF2, 0xF2)
    }

    pub fn text_muted() -> Color32 {
        Color32::from_rgb(0x76, 0x76, 0x80)
    }

    pub fn status_warning() -> Color32 {
        Color32::from_rgb(0xD6, 0xAA, 0x43)
    }

    pub fn status_success() -> Color32 {
        Color32::from_rgb(0x73, 0xD1, 0x8F)
    }

    pub fn interaction_highlight() -> Color32 {
        Color32::from_rgb(0x00, 0x6E, 0xFF)
    }

    pub fn status_error() -> Color32 {
        Color32::from_rgb(0xE3, 0x6D, 0x6D)
    }

    pub fn drop_overlay_fill() -> Color32 {
        bg_surface().gamma_multiply(0.78)
    }

    pub fn drop_overlay_stroke() -> Color32 {
        interaction_highlight()
    }

    pub fn drop_overlay_text() -> Color32 {
        interaction_highlight()
    }

    pub fn timeline_clip_video() -> Color32 {
        Color32::from_rgb(0x4D, 0x72, 0x9D)
    }

    pub fn timeline_clip_audio() -> Color32 {
        Color32::from_rgb(0x4E, 0x8A, 0x6A)
    }

    pub fn timeline_playhead() -> Color32 {
        Color32::from_rgb(0xE0, 0x67, 0x67)
    }
}
