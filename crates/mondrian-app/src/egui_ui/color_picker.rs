//! Color picker components for property editors, palettes, and general UX.
//!
//! All components work with `mondrian_core::types::Color` (linear f32 0..1) internally
//! and convert to/from `egui::Color32` (sRGBA u8) at the UI boundary.

use mondrian_core::types::Color;

use crate::egui_ui::theme::{corner_radius, palette, tokens, typography};

// ── Linear ↔ sRGBA conversion ────────────────────────────────────────────

pub fn color_to_egui(c: &Color) -> egui::Color32 {
    let r = (c.r.clamp(0.0, 1.0) * 255.0).round() as u8;
    let g = (c.g.clamp(0.0, 1.0) * 255.0).round() as u8;
    let b = (c.b.clamp(0.0, 1.0) * 255.0).round() as u8;
    let a = (c.a.clamp(0.0, 1.0) * 255.0).round() as u8;
    egui::Color32::from_rgba_unmultiplied(r, g, b, a)
}

pub fn egui_to_color(c: egui::Color32) -> Color {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    Color {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: a as f32 / 255.0,
    }
}

// ── Color swatch ─────────────────────────────────────────────────────────

/// A filled rectangle showing a color.
pub fn color_swatch(ui: &mut egui::Ui, color: egui::Color32, size: f32) -> egui::Response {
    let desired = egui::vec2(size, size);
    let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click());
    let rounding = corner_radius(tokens::button_rounding());

    if ui.is_rect_visible(rect) {
        if color.a() < 255 {
            draw_checkerboard(ui.painter(), rect, rounding);
        }
        ui.painter().rect_filled(rect, rounding, color);

        if response.hovered() {
            let highlight = palette::interaction_highlight().gamma_multiply(0.35);
            ui.painter().rect_stroke(
                rect.shrink(1.0),
                rounding,
                egui::Stroke::new(tokens::border_standard() * 1.5, highlight),
                egui::StrokeKind::Inside,
            );
        }
    }

    response
}

/// Swatch with a selection indicator (checkmark + accent ring).
pub fn color_swatch_selected(
    ui: &mut egui::Ui,
    color: egui::Color32,
    size: f32,
    selected: bool,
) -> egui::Response {
    let desired = egui::vec2(size, size);
    let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click());
    let rounding = corner_radius(tokens::button_rounding());

    if ui.is_rect_visible(rect) {
        if color.a() < 255 {
            draw_checkerboard(ui.painter(), rect, rounding);
        }
        ui.painter().rect_filled(rect, rounding, color);

        if selected {
            let accent = palette::interaction_highlight();
            ui.painter().rect_stroke(
                rect,
                rounding,
                egui::Stroke::new(tokens::border_standard() * 2.0, accent),
                egui::StrokeKind::Inside,
            );
            draw_swatch_checkmark(ui.painter(), rect, accent);
        } else if response.hovered() {
            let highlight = palette::interaction_highlight().gamma_multiply(0.35);
            ui.painter().rect_stroke(
                rect.shrink(1.0),
                rounding,
                egui::Stroke::new(tokens::border_standard() * 1.5, highlight),
                egui::StrokeKind::Inside,
            );
        } else {
            ui.painter().rect_stroke(
                rect,
                rounding,
                egui::Stroke::new(tokens::border_standard(), palette::border_subtle()),
                egui::StrokeKind::Inside,
            );
        }
    }

    response
}

fn draw_swatch_checkmark(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let side = rect.width().min(rect.height()) * 0.5;
    let center = rect.center();
    let start = egui::pos2(center.x - side * 0.35, center.y);
    let mid = egui::pos2(center.x - side * 0.08, center.y + side * 0.28);
    let end = egui::pos2(center.x + side * 0.38, center.y - side * 0.32);
    let stroke = egui::Stroke::new(1.8, color);
    painter.line_segment([start, mid], stroke);
    painter.line_segment([mid, end], stroke);
}

fn draw_checkerboard(painter: &egui::Painter, rect: egui::Rect, rounding: egui::CornerRadius) {
    let cell = 8.0;
    let dark = egui::Color32::from_gray(160);
    let light = egui::Color32::from_gray(210);
    let mut x = rect.left();
    let mut row = 0;
    while x < rect.right() {
        let mut y = rect.top();
        let mut col = row % 2;
        while y < rect.bottom() {
            let cx = x.min(rect.right());
            let cy = y.min(rect.bottom());
            let cw = (rect.right() - x).min(cell);
            let ch = (rect.bottom() - y).min(cell);
            let cell_rect = egui::Rect::from_min_size(egui::pos2(cx, cy), egui::vec2(cw, ch));
            let color = if col % 2 == 0 { dark } else { light };
            painter.rect_filled(cell_rect, rounding, color);
            y += cell;
            col += 1;
        }
        x += cell;
        row += 1;
    }
}

// ── Color picker button (popup-based) ─────────────────────────────────────

/// Which editing surface to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorPickerVariant {
    /// Just a swatch that opens a popup on click.
    Inline,
    /// Swatch + hex input side by side.
    Compact,
    /// Swatch + hex + R/G/B sliders in a group.
    Full,
}

/// The result returned after a color picker interaction.
pub struct ColorPickerResponse {
    pub response: egui::Response,
    pub changed: bool,
}

fn show_color_edit_content(ui: &mut egui::Ui, color: &mut egui::Color32) {
    let mut r = color.r() as f32 / 255.0;
    let mut g = color.g() as f32 / 255.0;
    let mut b = color.b() as f32 / 255.0;

    ui.spacing_mut().item_spacing.y = tokens::spacing_xs();
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("R")
                .font(typography::body_small())
                .color(palette::text_muted()),
        );
        ui.add(egui::Slider::new(&mut r, 0.0..=1.0).step_by(0.01).text(""));
    });
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("G")
                .font(typography::body_small())
                .color(palette::text_muted()),
        );
        ui.add(egui::Slider::new(&mut g, 0.0..=1.0).step_by(0.01).text(""));
    });
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("B")
                .font(typography::body_small())
                .color(palette::text_muted()),
        );
        ui.add(egui::Slider::new(&mut b, 0.0..=1.0).step_by(0.01).text(""));
    });

    // Hex input
    ui.add_space(tokens::spacing_sm());
    let hex = format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b());
    let mut hex_buf = hex;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Hex")
                .font(typography::body_small())
                .color(palette::text_muted()),
        );
        let resp = ui.add_sized(
            [100.0, ui.spacing().interact_size.y],
            egui::TextEdit::singleline(&mut hex_buf)
                .font(typography::body_small())
                .hint_text("#RRGGBB"),
        );
        if resp.changed() {
            if let Some(parsed) = parse_hex(&hex_buf) {
                r = parsed.r() as f32 / 255.0;
                g = parsed.g() as f32 / 255.0;
                b = parsed.b() as f32 / 255.0;
            }
        }
    });

    // Apply slider changes
    let new_r = (r * 255.0).round() as u8;
    let new_g = (g * 255.0).round() as u8;
    let new_b = (b * 255.0).round() as u8;
    if new_r != color.r() || new_g != color.g() || new_b != color.b() {
        *color = egui::Color32::from_rgb(new_r, new_g, new_b);
    }
}

/// An interactive color picker with configurable display variant.
pub fn color_picker_button(
    ui: &mut egui::Ui,
    color: &mut Color,
    variant: ColorPickerVariant,
) -> ColorPickerResponse {
    let original = color_to_egui(color);
    let id = ui.next_auto_id();
    let mut popup_open = egui::Popup::is_id_open(ui.ctx(), id);

    let (swatch_response, inline_color) = match variant {
        ColorPickerVariant::Inline => {
            let resp = color_swatch(ui, original, 24.0);
            (resp, original)
        }
        ColorPickerVariant::Compact => {
            let mut cur = original;
            let resp = ui.horizontal(|ui| {
                let sr = color_swatch(ui, cur, 24.0);
                ui.spacing_mut().item_spacing.x = tokens::spacing_sm();
                let hex = format!("#{:02X}{:02X}{:02X}", cur.r(), cur.g(), cur.b());
                let mut hex_buf = hex;
                let hr = ui.add_sized(
                    [80.0, ui.spacing().interact_size.y],
                    egui::TextEdit::singleline(&mut hex_buf).font(typography::button()),
                );
                if hr.changed() {
                    if let Some(parsed) = parse_hex(&hex_buf) {
                        cur = parsed;
                    }
                }
                sr
            });
            (resp.inner, cur)
        }
        ColorPickerVariant::Full => {
            let mut cur = original;
            let resp = ui.vertical(|ui| {
                let sr = color_swatch(ui, cur, 28.0);
                show_color_rgb_drags(ui, &mut cur);
                sr
            });
            (resp.inner, cur)
        }
    };

    if swatch_response.clicked() {
        popup_open = !popup_open;
    }

    let mut egui_color = inline_color;

    // Show popup with full editor
    egui::Popup::from_response(&swatch_response)
        .id(id)
        .open_bool(&mut popup_open)
        .show(|ui| {
            show_color_edit_content(ui, &mut egui_color);
        });

    let changed = egui_color != original;
    if changed {
        *color = egui_to_color(egui_color);
    }

    ColorPickerResponse { response: swatch_response, changed }
}

fn show_color_rgb_drags(ui: &mut egui::Ui, color: &mut egui::Color32) {
    let mut r = color.r() as f32 / 255.0;
    let mut g = color.g() as f32 / 255.0;
    let mut b = color.b() as f32 / 255.0;

    ui.spacing_mut().item_spacing.y = tokens::spacing_xs();
    ui.horizontal(|ui| {
        ui.label("R");
        ui.add(egui::DragValue::new(&mut r).range(0.0..=1.0).speed(0.005).fixed_decimals(3));
    });
    ui.horizontal(|ui| {
        ui.label("G");
        ui.add(egui::DragValue::new(&mut g).range(0.0..=1.0).speed(0.005).fixed_decimals(3));
    });
    ui.horizontal(|ui| {
        ui.label("B");
        ui.add(egui::DragValue::new(&mut b).range(0.0..=1.0).speed(0.005).fixed_decimals(3));
    });

    if (r - color.r() as f32 / 255.0).abs() > f32::EPSILON
        || (g - color.g() as f32 / 255.0).abs() > f32::EPSILON
        || (b - color.b() as f32 / 255.0).abs() > f32::EPSILON
    {
        *color = egui::Color32::from_rgb(
            (r * 255.0).round() as u8,
            (g * 255.0).round() as u8,
            (b * 255.0).round() as u8,
        );
    }
}

// ── Color palette (single-select) ────────────────────────────────────────

/// A predefined palette of colors.
pub struct ColorPalette {
    pub colors: Vec<egui::Color32>,
    pub columns: usize,
}

impl ColorPalette {
    pub fn new(colors: Vec<egui::Color32>) -> Self {
        Self { colors, columns: 8 }
    }

    pub fn with_columns(mut self, columns: usize) -> Self {
        self.columns = columns.max(2);
        self
    }
}

/// Single-select color palette grid. Returns the index of the clicked color, or None.
pub fn color_palette_single(
    ui: &mut egui::Ui,
    current: &mut usize,
    palette: &ColorPalette,
) -> Option<usize> {
    let mut clicked = None;
    let swatch_size = 22.0;
    let gap = tokens::spacing_sm();

    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
        egui::Grid::new(ui.next_auto_id()).spacing(egui::vec2(gap, gap)).show(ui, |ui| {
            for (i, &swatch_color) in palette.colors.iter().enumerate() {
                let response = color_swatch_selected(ui, swatch_color, swatch_size, i == *current);
                if response.clicked() {
                    *current = i;
                    clicked = Some(i);
                }
                response.on_hover_ui(|ui| {
                    ui.label(format!(
                        "#{:02X}{:02X}{:02X}",
                        swatch_color.r(),
                        swatch_color.g(),
                        swatch_color.b()
                    ));
                });

                if i % palette.columns == palette.columns - 1 {
                    ui.end_row();
                }
            }
            if palette.colors.len() % palette.columns != 0 {
                ui.end_row();
            }
        });
    });

    clicked
}

/// Multi-select color palette grid. Returns the index of the last clicked color, or None.
/// `selected` is a mutable bitmask over `palette.colors`.
pub fn color_palette_multi(
    ui: &mut egui::Ui,
    selected: &mut [bool],
    palette: &ColorPalette,
) -> Option<usize> {
    assert_eq!(
        selected.len(),
        palette.colors.len(),
        "selected mask must match palette size"
    );
    let mut clicked = None;
    let swatch_size = 22.0;
    let gap = tokens::spacing_sm();

    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
        egui::Grid::new(ui.next_auto_id()).spacing(egui::vec2(gap, gap)).show(ui, |ui| {
            for (i, &swatch_color) in palette.colors.iter().enumerate() {
                let response = color_swatch_selected(ui, swatch_color, swatch_size, selected[i]);
                if response.clicked() {
                    selected[i] = !selected[i];
                    clicked = Some(i);
                }
                response.on_hover_ui(|ui| {
                    ui.label(format!(
                        "#{:02X}{:02X}{:02X}",
                        swatch_color.r(),
                        swatch_color.g(),
                        swatch_color.b()
                    ));
                });

                if i % palette.columns == palette.columns - 1 {
                    ui.end_row();
                }
            }
            if palette.colors.len() % palette.columns != 0 {
                ui.end_row();
            }
        });
    });

    clicked
}

// ── Hex input ────────────────────────────────────────────────────────────

fn parse_hex(hex: &str) -> Option<egui::Color32> {
    let hex = hex.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(egui::Color32::from_rgb(r, g, b))
}

// ── Convenience: predefined palettes ─────────────────────────────────────

pub fn chroma_key_palette() -> ColorPalette {
    ColorPalette::new(vec![
        egui::Color32::from_rgb(0x00, 0xFF, 0x00),
        egui::Color32::from_rgb(0x00, 0xB0, 0x00),
        egui::Color32::from_rgb(0x00, 0x00, 0xFF),
        egui::Color32::from_rgb(0x00, 0x00, 0xC0),
        egui::Color32::from_rgb(0xFF, 0x00, 0x00),
        egui::Color32::from_rgb(0xC0, 0x00, 0x00),
        egui::Color32::from_rgb(0xFF, 0xFF, 0x00),
        egui::Color32::from_rgb(0xC0, 0xC0, 0x00),
        egui::Color32::from_rgb(0xFF, 0x00, 0xFF),
        egui::Color32::from_rgb(0xC0, 0x00, 0xC0),
        egui::Color32::from_rgb(0x00, 0xFF, 0xFF),
        egui::Color32::from_rgb(0x00, 0xC0, 0xC0),
        egui::Color32::from_rgb(0xFF, 0xFF, 0xFF),
        egui::Color32::from_rgb(0x80, 0x80, 0x80),
        egui::Color32::from_rgb(0x40, 0x40, 0x40),
        egui::Color32::from_rgb(0x00, 0x00, 0x00),
    ])
    .with_columns(8)
}
