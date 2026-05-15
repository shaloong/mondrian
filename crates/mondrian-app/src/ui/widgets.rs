//! Reusable UI widget components.
//!
//! These sit one level above the theme primitives and provide semantic
//! building blocks for panels: empty states, search bars, dialogs,
//! segmented controls, and collapsible sections.

use crate::ui::theme::{self, corner_radius, palette, tokens, typography, UiIcon};

// ── EmptyState ────────────────────────────────────────────────────────────

/// A centered placeholder shown when a panel has no content.
///
/// Renders an optional icon, title, subtitle, and optional action button.
pub fn empty_state(ui: &mut egui::Ui, icon: Option<UiIcon>, title: &str, subtitle: &str) {
    let available = ui.available_size();
    let min_h = tokens::list_empty_height();
    ui.allocate_space(egui::vec2(
        available.x,
        (available.y - min_h).max(0.0) * 0.4,
    ));

    ui.vertical_centered(|ui| {
        if let Some(icon_kind) = icon {
            theme::icon(ui, icon_kind, palette::text_muted());
            ui.add_space(tokens::spacing_sm());
        }
        if !title.is_empty() {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(title)
                        .font(typography::body())
                        .color(palette::text_muted()),
                )
                .selectable(false),
            );
        }
        if !subtitle.is_empty() {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(subtitle)
                        .font(typography::body_small())
                        .color(palette::text_muted().gamma_multiply(0.7)),
                )
                .selectable(false),
            );
        }
    });
}

// ── SearchBar ─────────────────────────────────────────────────────────────

/// A search input with icon, styled consistently.
///
/// Returns the `TextEdit` response so callers can check `.changed()`.
/// The caller is responsible for constraining the available width
/// (e.g. via `ui.allocate_ui_with_layout` or a horizontal layout).
pub fn search_bar(ui: &mut egui::Ui, query: &mut String, hint: &str) -> egui::Response {
    egui::Frame::new()
        .fill(palette::bg_surface_raised())
        .stroke(egui::Stroke::new(
            tokens::border_standard(),
            palette::border_subtle(),
        ))
        .corner_radius(corner_radius(tokens::button_rounding()))
        .inner_margin(egui::Margin::symmetric(
            theme::margin_px(tokens::search_bar_margin_x()),
            theme::margin_px(tokens::search_bar_margin_y().min(2.0)),
        ))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                theme::icon(ui, UiIcon::Search, palette::text_muted());
                ui.add_space(tokens::spacing_sm());
                let input_width = ui.available_width().max(24.0);
                ui.add_sized(
                    [input_width, ui.spacing().interact_size.y],
                    egui::TextEdit::singleline(query)
                        .hint_text(hint)
                        .frame(false)
                        .margin(egui::Margin::ZERO)
                        .vertical_align(egui::Align::Center),
                )
            })
            .inner
        })
        .inner
}

// ── ConfirmationDialog ────────────────────────────────────────────────────

/// Buttons available in a confirmation dialog.
pub enum ConfirmButton {
    /// Single "OK" / dismiss button.
    Ok,
    /// "Cancel" + "Confirm" pair.
    CancelConfirm,
}

/// Show a modal confirmation dialog.
///
/// Returns `true` when the confirm action is clicked.
/// `cancel_label` is used when `buttons` is `CancelConfirm`.
pub fn confirmation_dialog(
    ctx: &egui::Context,
    title: &str,
    message: &str,
    confirm_label: &str,
    cancel_label: &str,
    buttons: ConfirmButton,
    open: &mut bool,
) -> bool {
    if !*open {
        return false;
    }

    let mut confirmed = false;

    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_min_width(280.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(message)
                        .font(typography::body())
                        .color(palette::text_primary()),
                )
                .wrap(),
            );
            ui.add_space(tokens::spacing_md());

            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| match buttons {
                    ConfirmButton::Ok => {
                        if ui
                            .add_sized(
                                [80.0, ui.spacing().interact_size.y],
                                egui::Button::new(
                                    egui::RichText::new(confirm_label).font(typography::button()),
                                ),
                            )
                            .clicked()
                        {
                            confirmed = true;
                            *open = false;
                        }
                    }
                    ConfirmButton::CancelConfirm => {
                        if ui
                            .add_sized(
                                [80.0, ui.spacing().interact_size.y],
                                egui::Button::new(
                                    egui::RichText::new(confirm_label).font(typography::button()),
                                ),
                            )
                            .clicked()
                        {
                            confirmed = true;
                            *open = false;
                        }
                        if ui
                            .add_sized(
                                [80.0, ui.spacing().interact_size.y],
                                egui::Button::new(
                                    egui::RichText::new(cancel_label).font(typography::button()),
                                ),
                            )
                            .clicked()
                        {
                            *open = false;
                        }
                    }
                },
            );
        });

    confirmed
}

// ── SegmentedControl ──────────────────────────────────────────────────────

/// An option in a segmented control.
pub struct SegmentedOption<V> {
    pub value: V,
    pub label: String,
}

impl<V> SegmentedOption<V> {
    pub fn new(value: V, label: impl Into<String>) -> Self {
        Self { value, label: label.into() }
    }
}

/// Horizontal segmented control (like iOS/macOS segmented picker).
///
/// Renders a row of adjacent selectable buttons with connected corners.
/// Returns `true` if the selection changed.
pub fn segmented_control<V: PartialEq + Clone>(
    ui: &mut egui::Ui,
    current: &mut V,
    options: &[SegmentedOption<V>],
) -> bool {
    let mut changed = false;
    let rounding = corner_radius(tokens::button_rounding());

    let total_width: f32 = options
        .iter()
        .map(|o| {
            let galley = ui.painter().layout_no_wrap(
                o.label.clone(),
                typography::body_small(),
                palette::text_primary(),
            );
            galley.size().x + tokens::panel_inner_margin_x() * 2.0
        })
        .sum();

    let desired = egui::vec2(total_width, ui.spacing().interact_size.y);
    let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());

    if !ui.is_rect_visible(rect) {
        return false;
    }

    let n = options.len();
    let mut x = rect.left();

    for (i, opt) in options.iter().enumerate() {
        let galley = ui.painter().layout_no_wrap(
            opt.label.clone(),
            typography::body_small(),
            palette::text_primary(),
        );
        let w = galley.size().x + tokens::panel_inner_margin_x() * 2.0;
        let seg_rect =
            egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(w, rect.height()));

        let selected = *current == opt.value;
        let seg_id = ui.id().with(i);
        let seg_response = ui.interact(seg_rect, seg_id, egui::Sense::click());

        let seg_rounding = if n == 1 {
            rounding
        } else if i == 0 {
            egui::CornerRadius { nw: rounding.nw, sw: rounding.sw, ne: 0, se: 0 }
        } else if i == n - 1 {
            egui::CornerRadius { nw: 0, sw: 0, ne: rounding.ne, se: rounding.se }
        } else {
            egui::CornerRadius::same(0)
        };

        let fill = if selected {
            palette::interaction_highlight()
        } else if seg_response.hovered() {
            palette::bg_surface_hover()
        } else {
            palette::bg_surface_raised()
        };

        ui.painter().rect_filled(seg_rect, seg_rounding, fill);
        ui.painter().rect_stroke(
            seg_rect,
            seg_rounding,
            egui::Stroke::new(tokens::border_standard(), palette::border_subtle()),
            egui::StrokeKind::Inside,
        );

        let text_color = if selected {
            palette::bg_base()
        } else {
            palette::text_primary()
        };

        ui.painter().text(
            seg_rect.center(),
            egui::Align2::CENTER_CENTER,
            &opt.label,
            typography::body_small(),
            text_color,
        );

        if seg_response.clicked() && !selected {
            *current = opt.value.clone();
            changed = true;
        }

        x += w;
    }

    changed
}

// ── CollapsibleSection ────────────────────────────────────────────────────

/// Header state for a collapsible section.
pub struct CollapsibleHeader {
    /// The egui Id under which collapsed state is persisted.
    pub id: egui::Id,
    pub title: String,
    pub subtitle: Option<String>,
    /// Whether the section starts collapsed.
    pub default_collapsed: bool,
}

/// Renders a collapsible section header + content.
///
/// Returns `Some(result)` when expanded (content was rendered), `None` when collapsed.
/// The collapsed state is stored in egui's memory (survives frame).
pub fn collapsible_section<R>(
    ui: &mut egui::Ui,
    header: &CollapsibleHeader,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    let collapsed_id = header.id.with("collapsed");
    let mut collapsed = ui
        .memory_mut(|mem| mem.data.get_temp::<bool>(collapsed_id))
        .unwrap_or(header.default_collapsed);

    let header_h = tokens::inspector_group_header_height();
    let header_rect = egui::Rect::from_min_size(
        ui.next_widget_position(),
        egui::vec2(ui.available_width() - tokens::spacing_xs(), header_h),
    );

    let sense = egui::Sense::click();
    let header_resp = ui.interact(header_rect, header.id, sense);

    // Hover background
    if header_resp.hovered() {
        ui.painter().rect_filled(
            header_rect,
            corner_radius(tokens::section_rounding()),
            palette::bg_surface_hover(),
        );
    }

    // Caret icon
    let caret = if collapsed {
        UiIcon::ArrowRight
    } else {
        UiIcon::ArrowDown
    };
    let icon_size = tokens::icon_size();
    let caret_rect = egui::Rect::from_center_size(
        egui::pos2(
            header_rect.left() + icon_size * 0.5 + 4.0,
            header_rect.center().y,
        ),
        egui::vec2(icon_size, icon_size),
    );
    theme::draw_icon(ui.painter(), caret_rect, caret, palette::text_muted());

    // Title
    let text_x = caret_rect.right() + tokens::spacing_sm();
    ui.painter().text(
        egui::pos2(text_x, header_rect.center().y),
        egui::Align2::LEFT_CENTER,
        &header.title,
        typography::body_small(),
        palette::text_primary(),
    );

    // Subtitle
    if let Some(ref subtitle) = header.subtitle {
        let subtitle_galley = ui.painter().layout_no_wrap(
            subtitle.clone(),
            typography::body_small(),
            palette::text_muted(),
        );
        ui.painter().text(
            egui::pos2(
                header_rect.right() - subtitle_galley.size().x - 4.0,
                header_rect.center().y,
            ),
            egui::Align2::LEFT_CENTER,
            subtitle,
            typography::body_small(),
            palette::text_muted(),
        );
    }

    // Allocate the header space
    ui.allocate_rect(header_rect, egui::Sense::hover());

    if header_resp.clicked() {
        collapsed = !collapsed;
        ui.memory_mut(|mem| mem.data.insert_temp(collapsed_id, collapsed));
    }

    let content_indent = tokens::inspector_group_indent();
    let inner = if collapsed {
        let (_, resp) =
            ui.allocate_exact_size(egui::vec2(0.0, tokens::spacing_sm()), egui::Sense::hover());
        egui::InnerResponse::new(None, resp)
    } else {
        ui.add_space(tokens::spacing_xs());
        ui.horizontal(|ui| {
            ui.add_space(content_indent);
            let content = ui.vertical(|ui| add_contents(ui));
            egui::InnerResponse::new(Some(content.inner), content.response)
        })
        .inner
    };

    egui::InnerResponse::new(inner.inner, header_resp)
}
