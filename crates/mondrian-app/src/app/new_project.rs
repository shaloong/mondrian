use super::*;

const WINDOW_WIDTH: f32 = 420.0;
const WINDOW_HEIGHT: f32 = 240.0;

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, u8::MAX as f32) as u8)
}

pub(super) fn draw_new_project_window(app: &mut MondrianApp, ctx: &egui::Context) {
    let viewport_id = egui::ViewportId::from_hash_of("new_project_viewport");
    let viewport_builder = egui::ViewportBuilder::default()
        .with_title("新建项目")
        .with_inner_size([WINDOW_WIDTH, WINDOW_HEIGHT])
        .with_resizable(false)
        .with_active(true);

    ctx.show_viewport_immediate(viewport_id, viewport_builder, |viewport_ctx, class| {
        crate::ui::theme::apply_theme(viewport_ctx, app.theme);
        viewport_ctx
            .send_viewport_cmd(egui::ViewportCommand::SetTheme(app.theme.to_system_theme()));

        if viewport_ctx.input(|i| i.viewport().close_requested()) {
            app.show_new_project_dialog = false;
            viewport_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        match class {
            egui::ViewportClass::Embedded => {
                let mut open = app.show_new_project_dialog;
                egui::Window::new("新建项目")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .default_size([WINDOW_WIDTH, WINDOW_HEIGHT])
                    .frame(crate::ui::theme::dialog_frame())
                    .show(viewport_ctx, |ui| {
                        draw_new_project_panel(app, ui);
                    });
                app.show_new_project_dialog = open;
            }
            _ => {
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::new()
                            .fill(crate::ui::theme::palette::bg_surface())
                            .inner_margin(egui::Margin::symmetric(18, 16)),
                    )
                    .show(viewport_ctx, |ui| {
                        draw_new_project_panel(app, ui);
                    });
            }
        }
    });
}

fn draw_new_project_panel(app: &mut MondrianApp, ui: &mut egui::Ui) {
    let text_primary = crate::ui::theme::palette::text_primary();
    let text_muted = crate::ui::theme::palette::text_muted();

    ui.spacing_mut().item_spacing = egui::vec2(8.0, 10.0);

    egui::Grid::new("new_project_grid")
        .num_columns(2)
        .spacing([12.0, 10.0])
        .show(ui, |ui| {
            ui.label(egui::RichText::new("项目名称").color(text_primary).size(12.5));
            ui.add(
                egui::TextEdit::singleline(&mut app.new_project_draft.name)
                    .desired_width(f32::INFINITY),
            );
            ui.end_row();

            ui.label(egui::RichText::new("分辨率").color(text_primary).size(12.5));
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut app.new_project_draft.width)
                        .range(320..=8192)
                        .suffix(" px"),
                );
                ui.label(egui::RichText::new("×").color(text_muted));
                ui.add(
                    egui::DragValue::new(&mut app.new_project_draft.height)
                        .range(240..=4320)
                        .suffix(" px"),
                );
            });
            ui.end_row();

            ui.label(egui::RichText::new("帧率").color(text_primary).size(12.5));
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut app.new_project_draft.fps_num).range(1..=240));
                ui.label(egui::RichText::new("/").color(text_muted));
                ui.add(egui::DragValue::new(&mut app.new_project_draft.fps_den).range(1..=1001));
                ui.label(egui::RichText::new("fps").color(text_muted).size(11.5));
            });
            ui.end_row();
        });

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(4.0);

    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let create_btn = ui.add(
            egui::Button::new(
                egui::RichText::new("创建项目").color(egui::Color32::WHITE).size(12.5),
            )
            .fill(crate::ui::theme::palette::interaction_highlight())
            .corner_radius(corner_radius(crate::ui::theme::tokens::button_rounding())),
        );

        if create_btn.clicked() {
            commit_new_project(app);
        }

        let cancel_btn = ui.button("取消");
        if cancel_btn.clicked() {
            app.show_new_project_dialog = false;
        }
    });
}

fn commit_new_project(app: &mut MondrianApp) {
    let fps = Rational::new(
        app.new_project_draft.fps_num.max(1),
        app.new_project_draft.fps_den.max(1),
    );
    let name = if app.new_project_draft.name.trim().is_empty() {
        "未命名项目"
    } else {
        app.new_project_draft.name.trim()
    };

    let default_name = format!("{}.{}", sanitize_filename(name), PROJECT_EXTENSION);
    let picked = FileDialog::new()
        .add_filter("Mondrian Project", &[PROJECT_EXTENSION])
        .set_file_name(&default_name)
        .save_file();

    if let Some(path) = picked {
        let project_path = ensure_project_extension(path);
        if let Err(err) = app.state.create_new_project_at(
            project_path.clone(),
            name,
            app.new_project_draft.width.max(1),
            app.new_project_draft.height.max(1),
            fps,
        ) {
            app.state.set_status_hint(format!("新建项目失败：{err}"), true);
            tracing::error!("新建项目失败: {err}");
        } else {
            app.finish_project_opened();
            app.record_recent_project(project_path);
            app.show_new_project_dialog = false;
        }
    }
}
