use super::*;

const WINDOW_WIDTH: f32 = 420.0;
const WINDOW_HEIGHT: f32 = 560.0;

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, u8::MAX as f32) as u8)
}

pub(super) fn draw_new_project_window(app: &mut MondrianApp, ctx: &egui::Context) {
    let viewport_id = egui::ViewportId::from_hash_of("new_project_viewport");
    let viewport_builder = egui::ViewportBuilder::default()
        .with_title("新建项目")
        .with_inner_size([WINDOW_WIDTH, WINDOW_HEIGHT])
        .with_resizable(true)
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
                    .resizable(true)
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

    egui::ScrollArea::vertical()
        .id_salt("new_project_sequence_settings_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Grid::new("new_project_grid").num_columns(2).spacing([12.0, 10.0]).show(
                ui,
                |ui| {
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
                    egui::ComboBox::from_id_salt("new_project_frame_rate")
                        .selected_text(frame_rate_label(Rational::new(
                            app.new_project_draft.fps_num.max(1),
                            app.new_project_draft.fps_den.max(1),
                        )))
                        .show_ui(ui, |ui| {
                            for frame_rate in Rational::SEQUENCE_FRAME_RATES {
                                let selected = app.new_project_draft.fps_num == frame_rate.num
                                    && app.new_project_draft.fps_den == frame_rate.den;
                                if ui
                                    .selectable_label(selected, frame_rate_label(frame_rate))
                                    .clicked()
                                {
                                    app.new_project_draft.fps_num = frame_rate.num;
                                    app.new_project_draft.fps_den = frame_rate.den;
                                }
                            }
                        });
                    ui.end_row();

                    ui.label(egui::RichText::new("编辑模式").color(text_primary).size(12.5));
                    egui::ComboBox::from_id_salt("new_project_editing_mode")
                        .selected_text(editing_mode_label(app.new_project_draft.editing_mode))
                        .show_ui(ui, |ui| {
                            for mode in [
                                EditingMode::Custom,
                                EditingMode::Dslr1080p,
                                EditingMode::Dslr720p,
                                EditingMode::Avchd1080p,
                                EditingMode::DigitalCinema4k,
                                EditingMode::SocialVertical1080p,
                            ] {
                                ui.selectable_value(
                                    &mut app.new_project_draft.editing_mode,
                                    mode,
                                    editing_mode_label(mode),
                                );
                            }
                        });
                    ui.end_row();

                    ui.label(egui::RichText::new("像素长宽比").color(text_primary).size(12.5));
                    egui::ComboBox::from_id_salt("new_project_pixel_aspect_ratio")
                        .selected_text(pixel_aspect_ratio_label(
                            app.new_project_draft.pixel_aspect_ratio,
                        ))
                        .show_ui(ui, |ui| {
                            for par in [
                                PixelAspectRatio::Square,
                                PixelAspectRatio::D1DvNtsc,
                                PixelAspectRatio::D1DvNtscWidescreen,
                                PixelAspectRatio::D1DvPal,
                                PixelAspectRatio::D1DvPalWidescreen,
                                PixelAspectRatio::Anamorphic2x,
                                PixelAspectRatio::HdAnamorphic1080,
                                PixelAspectRatio::DvcproHd,
                                PixelAspectRatio::Unknown,
                            ] {
                                ui.selectable_value(
                                    &mut app.new_project_draft.pixel_aspect_ratio,
                                    par,
                                    pixel_aspect_ratio_label(par),
                                );
                            }
                        });
                    ui.end_row();

                    ui.label(egui::RichText::new("场").color(text_primary).size(12.5));
                    egui::ComboBox::from_id_salt("new_project_field_order")
                        .selected_text(field_order_label(app.new_project_draft.field_order))
                        .show_ui(ui, |ui| {
                            for field_order in [
                                FieldOrder::Progressive,
                                FieldOrder::UpperFirst,
                                FieldOrder::LowerFirst,
                            ] {
                                ui.selectable_value(
                                    &mut app.new_project_draft.field_order,
                                    field_order,
                                    field_order_label(field_order),
                                );
                            }
                        });
                    ui.end_row();

                    ui.label(egui::RichText::new("显示格式").color(text_primary).size(12.5));
                    egui::ComboBox::from_id_salt("new_project_video_display_format")
                        .selected_text(video_display_format_label(
                            app.new_project_draft.video_display_format,
                        ))
                        .show_ui(ui, |ui| {
                            for display_format in [
                                VideoDisplayFormat::Timecode2997DropFrame,
                                VideoDisplayFormat::Timecode2997NonDropFrame,
                                VideoDisplayFormat::FeetAndFrames16mm,
                                VideoDisplayFormat::FeetAndFrames35mm,
                                VideoDisplayFormat::Frames,
                            ] {
                                ui.selectable_value(
                                    &mut app.new_project_draft.video_display_format,
                                    display_format,
                                    video_display_format_label(display_format),
                                );
                            }
                        });
                    ui.end_row();

                    ui.label(egui::RichText::new("工作色彩空间").color(text_primary).size(12.5));
                    color_space_combo(
                        ui,
                        "new_project_color_space",
                        &mut app.new_project_draft.color_space,
                    );
                    ui.end_row();

                    ui.label(egui::RichText::new("输出色彩空间").color(text_primary).size(12.5));
                    color_space_combo(
                        ui,
                        "new_project_output_color_space",
                        &mut app.new_project_draft.output_color_space,
                    );
                    ui.end_row();

                    ui.label(egui::RichText::new("色彩工作流").color(text_primary).size(12.5));
                    egui::ComboBox::from_id_salt("new_project_color_workflow")
                        .selected_text(color_workflow_label(app.new_project_draft.color_workflow))
                        .show_ui(ui, |ui| {
                            for workflow in [
                                ColorWorkflow::DisplayReferred,
                                ColorWorkflow::SceneReferred,
                                ColorWorkflow::Aces,
                            ] {
                                ui.selectable_value(
                                    &mut app.new_project_draft.color_workflow,
                                    workflow,
                                    color_workflow_label(workflow),
                                );
                            }
                        });
                    ui.end_row();

                    ui.label(egui::RichText::new("缺失色彩元数据").color(text_primary).size(12.5));
                    egui::ComboBox::from_id_salt("new_project_missing_metadata")
                        .selected_text(missing_color_metadata_policy_label(
                            app.new_project_draft.missing_color_metadata_policy,
                        ))
                        .show_ui(ui, |ui| {
                            for policy in [
                                MissingColorMetadataPolicy::AssumeRec709,
                                MissingColorMetadataPolicy::AssumeSequenceWorkingSpace,
                                MissingColorMetadataPolicy::RejectMedia,
                            ] {
                                ui.selectable_value(
                                    &mut app.new_project_draft.missing_color_metadata_policy,
                                    policy,
                                    missing_color_metadata_policy_label(policy),
                                );
                            }
                        });
                    ui.end_row();

                    ui.label(egui::RichText::new("嵌套序列色彩").color(text_primary).size(12.5));
                    egui::ComboBox::from_id_salt("new_project_nested_color")
                        .selected_text(nested_color_processing_label(
                            app.new_project_draft.nested_color_processing,
                        ))
                        .show_ui(ui, |ui| {
                            for processing in [
                                NestedColorProcessing::PreserveChildWorkingSpace,
                                NestedColorProcessing::ForceParentWorkingSpace,
                                NestedColorProcessing::BakeChildOutputTransform,
                            ] {
                                ui.selectable_value(
                                    &mut app.new_project_draft.nested_color_processing,
                                    processing,
                                    nested_color_processing_label(processing),
                                );
                            }
                        });
                    ui.end_row();

                    ui.label(egui::RichText::new("视频电平").color(text_primary).size(12.5));
                    egui::ComboBox::from_id_salt("new_project_video_range")
                        .selected_text(video_range_label(app.new_project_draft.video_range))
                        .show_ui(ui, |ui| {
                            for range in [VideoRange::Full, VideoRange::Legal] {
                                ui.selectable_value(
                                    &mut app.new_project_draft.video_range,
                                    range,
                                    video_range_label(range),
                                );
                            }
                        });
                    ui.end_row();

                    ui.label(egui::RichText::new("导出位深").color(text_primary).size(12.5));
                    egui::ComboBox::from_id_salt("new_project_export_bit_depth")
                        .selected_text(export_bit_depth_label(
                            app.new_project_draft.export_bit_depth,
                        ))
                        .show_ui(ui, |ui| {
                            for bit_depth in [
                                ExportBitDepth::Eight,
                                ExportBitDepth::Ten,
                                ExportBitDepth::SixteenFloat,
                            ] {
                                ui.selectable_value(
                                    &mut app.new_project_draft.export_bit_depth,
                                    bit_depth,
                                    export_bit_depth_label(bit_depth),
                                );
                            }
                        });
                    ui.end_row();

                    ui.label(egui::RichText::new("自动色调映射").color(text_primary).size(12.5));
                    ui.checkbox(&mut app.new_project_draft.auto_tone_map_media, "");
                    ui.end_row();

                    ui.label(
                        egui::RichText::new("保留 HDR metadata").color(text_primary).size(12.5),
                    );
                    ui.checkbox(&mut app.new_project_draft.preserve_hdr_metadata, "");
                    ui.end_row();

                    ui.label(egui::RichText::new("采样率").color(text_primary).size(12.5));
                    egui::ComboBox::from_id_salt("new_project_audio_sample_rate")
                        .selected_text(format!("{} Hz", app.new_project_draft.audio_sample_rate))
                        .show_ui(ui, |ui| {
                            for sample_rate in SequenceSettings::AUDIO_SAMPLE_RATES {
                                ui.selectable_value(
                                    &mut app.new_project_draft.audio_sample_rate,
                                    sample_rate,
                                    format!("{sample_rate} Hz"),
                                );
                            }
                        });
                    ui.end_row();

                    ui.label(egui::RichText::new("音频显示格式").color(text_primary).size(12.5));
                    egui::ComboBox::from_id_salt("new_project_audio_display_format")
                        .selected_text(audio_display_format_label(
                            app.new_project_draft.audio_display_format,
                        ))
                        .show_ui(ui, |ui| {
                            for display_format in [
                                AudioDisplayFormat::AudioSamples,
                                AudioDisplayFormat::Milliseconds,
                            ] {
                                ui.selectable_value(
                                    &mut app.new_project_draft.audio_display_format,
                                    display_format,
                                    audio_display_format_label(display_format),
                                );
                            }
                        });
                    ui.end_row();
                },
            );
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
        let mut settings = SequenceSettings::from_editing_mode(app.new_project_draft.editing_mode);
        settings.resolution.width = app.new_project_draft.width.max(1);
        settings.resolution.height = app.new_project_draft.height.max(1);
        settings.frame_rate = fps;
        settings.pixel_aspect_ratio = app.new_project_draft.pixel_aspect_ratio;
        settings.field_order = app.new_project_draft.field_order;
        settings.video_display_format = app.new_project_draft.video_display_format;
        settings.audio_sample_rate = app.new_project_draft.audio_sample_rate;
        settings.audio_display_format = app.new_project_draft.audio_display_format;
        settings.color_space = app.new_project_draft.color_space;
        settings.auto_tone_map_media = app.new_project_draft.auto_tone_map_media;
        settings.color_management.workflow = app.new_project_draft.color_workflow;
        settings.color_management.missing_metadata_policy =
            app.new_project_draft.missing_color_metadata_policy;
        settings.color_management.nested_processing = app.new_project_draft.nested_color_processing;
        settings.color_management.output_color_space = app.new_project_draft.output_color_space;
        settings.color_management.video_range = app.new_project_draft.video_range;
        settings.color_management.export_bit_depth = app.new_project_draft.export_bit_depth;
        settings.color_management.preserve_hdr_metadata =
            app.new_project_draft.preserve_hdr_metadata;

        if let Err(err) =
            app.state
                .create_new_project_with_settings_at(project_path.clone(), name, settings)
        {
            app.state.set_status_hint(format!("新建项目失败：{err}"), true);
            tracing::error!("新建项目失败: {err}");
        } else {
            app.finish_project_opened();
            app.record_recent_project(project_path);
            app.show_new_project_dialog = false;
        }
    }
}

fn color_space_combo(ui: &mut egui::Ui, id: impl std::hash::Hash, value: &mut ColorSpace) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(color_space_label(*value))
        .show_ui(ui, |ui| {
            for color_space in color_space_options() {
                ui.selectable_value(value, color_space, color_space_label(color_space));
            }
        });
}
