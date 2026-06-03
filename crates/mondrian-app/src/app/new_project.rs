use super::*;

const WINDOW_WIDTH: f32 = 440.0;
const WINDOW_HEIGHT: f32 = 540.0;

const TAB_LABELS: &[(NewProjectTab, &str)] = &[
    (NewProjectTab::Basic, "基础"),
    (NewProjectTab::Timeline, "时间线"),
    (NewProjectTab::Color, "色彩"),
    (NewProjectTab::Audio, "音频"),
    (NewProjectTab::Advanced, "高级"),
];

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
            egui::ViewportClass::EmbeddedWindow => {
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
                    .show_inside(viewport_ctx, |ui| {
                        draw_new_project_panel(app, ui);
                    });
            }
        }
    });
}

fn draw_new_project_panel(app: &mut MondrianApp, ui: &mut egui::Ui) {
    let text_primary = crate::ui::theme::palette::text_primary();
    let text_muted = crate::ui::theme::palette::text_muted();
    let highlight = crate::ui::theme::palette::interaction_highlight();
    let bg_card = crate::ui::theme::palette::bg_surface_raised();

    ui.spacing_mut().item_spacing = egui::vec2(6.0, 8.0);

    // ── Tab bar ──
    let mut active_tab = app.new_project_active_tab;
    ui.horizontal(|ui| {
        for &(tab, label) in TAB_LABELS {
            let selected = active_tab == tab;
            let response = ui.selectable_label(selected, label);
            if response.clicked() {
                active_tab = tab;
            }
            if selected {
                response.highlight();
            }
        }
    });
    app.new_project_active_tab = active_tab;
    ui.separator();

    let content_height = ui.available_height() - 70.0;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(content_height)
        .show(ui, |ui| match active_tab {
            NewProjectTab::Basic => draw_basic_tab(app, ui, text_primary, text_muted),
            NewProjectTab::Timeline => draw_timeline_tab(app, ui, text_primary, text_muted),
            NewProjectTab::Color => draw_color_tab(app, ui, text_primary, bg_card, highlight),
            NewProjectTab::Audio => draw_audio_tab(app, ui, text_primary),
            NewProjectTab::Advanced => draw_advanced_tab(app, ui, text_primary, text_muted),
        });

    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui.button("取消").clicked() {
            app.show_new_project_dialog = false;
        }
        let can_create = !app.new_project_draft.name.trim().is_empty();
        if ui
            .add_enabled(can_create, egui::Button::new("创建项目").fill(highlight))
            .clicked()
        {
            commit_new_project(app);
        }
    });
}

// ── Basic tab ──────────────────────────────────────────────────────────────────

fn draw_basic_tab(
    app: &mut MondrianApp,
    ui: &mut egui::Ui,
    text_primary: egui::Color32,
    text_muted: egui::Color32,
) {
    egui::Grid::new("basic_grid")
        .num_columns(2)
        .spacing([10.0, 8.0])
        .show(ui, |ui| {
            ui.label(egui::RichText::new("项目名称").color(text_primary));
            ui.add(
                egui::TextEdit::singleline(&mut app.new_project_draft.name)
                    .hint_text("未命名项目")
                    .desired_width(f32::INFINITY),
            );
            ui.end_row();

            ui.label(egui::RichText::new("保存位置").color(text_primary));
            ui.label(
                egui::RichText::new(default_project_path(&app.new_project_draft.name))
                    .color(text_muted),
            );
            ui.end_row();
        });

    ui.add_space(4.0);
    ui.label(egui::RichText::new("分辨率").color(text_primary));
    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(&mut app.new_project_draft.width)
                .range(320..=8192)
                .speed(8),
        );
        ui.label(egui::RichText::new("×").color(text_muted));
        ui.add(
            egui::DragValue::new(&mut app.new_project_draft.height)
                .range(240..=4320)
                .speed(8),
        );
        ui.menu_button("预设", |ui| {
            for &(label, w, h) in RESOLUTION_PRESETS {
                if ui.button(label).clicked() {
                    app.new_project_draft.width = w;
                    app.new_project_draft.height = h;
                    ui.close();
                }
            }
        });
    });

    ui.add_space(2.0);
    ui.label(egui::RichText::new("帧率").color(text_primary));
    fps_combo(ui, app);

    ui.add_space(2.0);
    ui.label(egui::RichText::new("色彩模式").color(text_primary));
    draw_color_mode_cards(ui, app);
}

const RESOLUTION_PRESETS: &[(&str, u32, u32)] = &[
    ("720p", 1280, 720),
    ("1080p", 1920, 1080),
    ("1440p", 2560, 1440),
    ("4K UHD", 3840, 2160),
    ("DCI 4K", 4096, 2160),
    ("8K", 7680, 4320),
    ("竖屏 1080×1920", 1080, 1920),
    ("方形 1080×1080", 1080, 1080),
];

// ── Timeline tab ───────────────────────────────────────────────────────────────

fn draw_timeline_tab(
    app: &mut MondrianApp,
    ui: &mut egui::Ui,
    text_primary: egui::Color32,
    text_muted: egui::Color32,
) {
    egui::Grid::new("timeline_grid")
        .num_columns(2)
        .spacing([10.0, 8.0])
        .show(ui, |ui| {
            ui.label(egui::RichText::new("起始时间码").color(text_primary));
            ui.add(
                egui::DragValue::new(&mut app.new_project_draft.start_timecode_frame)
                    .range(0..=24 * 60 * 60 * 240)
                    .suffix(" f"),
            );
            ui.end_row();

            let is_ntsc =
                is_ntsc_frame_rate(app.new_project_draft.fps_num, app.new_project_draft.fps_den);
            ui.label(egui::RichText::new("Drop Frame").color(text_primary));
            ui.add_enabled_ui(is_ntsc, |ui| {
                let mut df = matches!(
                    app.new_project_draft.video_display_format,
                    VideoDisplayFormat::Timecode2997DropFrame
                );
                if ui.checkbox(&mut df, "29.97 DF").changed() {
                    app.new_project_draft.video_display_format = if df {
                        VideoDisplayFormat::Timecode2997DropFrame
                    } else {
                        VideoDisplayFormat::Timecode2997NonDropFrame
                    };
                }
            });
            if !is_ntsc {
                ui.label(egui::RichText::new("（仅 NTSC 帧率）").color(text_muted));
            }
            ui.end_row();

            ui.label(egui::RichText::new("像素长宽比").color(text_primary));
            egui::ComboBox::from_id_salt("timeline_par")
                .selected_text(pixel_aspect_ratio_label(
                    app.new_project_draft.pixel_aspect_ratio,
                ))
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut app.new_project_draft.pixel_aspect_ratio,
                        PixelAspectRatio::Square,
                        pixel_aspect_ratio_label(PixelAspectRatio::Square),
                    );
                    ui.selectable_value(
                        &mut app.new_project_draft.pixel_aspect_ratio,
                        PixelAspectRatio::D1DvNtsc,
                        pixel_aspect_ratio_label(PixelAspectRatio::D1DvNtsc),
                    );
                    ui.selectable_value(
                        &mut app.new_project_draft.pixel_aspect_ratio,
                        PixelAspectRatio::D1DvPal,
                        pixel_aspect_ratio_label(PixelAspectRatio::D1DvPal),
                    );
                    ui.selectable_value(
                        &mut app.new_project_draft.pixel_aspect_ratio,
                        PixelAspectRatio::Anamorphic2x,
                        pixel_aspect_ratio_label(PixelAspectRatio::Anamorphic2x),
                    );
                });
            ui.end_row();

            ui.label(egui::RichText::new("扫描模式").color(text_primary));
            egui::ComboBox::from_id_salt("timeline_field_order")
                .selected_text(field_order_label(app.new_project_draft.field_order))
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut app.new_project_draft.field_order,
                        FieldOrder::Progressive,
                        field_order_label(FieldOrder::Progressive),
                    );
                    ui.selectable_value(
                        &mut app.new_project_draft.field_order,
                        FieldOrder::UpperFirst,
                        field_order_label(FieldOrder::UpperFirst),
                    );
                    ui.selectable_value(
                        &mut app.new_project_draft.field_order,
                        FieldOrder::LowerFirst,
                        field_order_label(FieldOrder::LowerFirst),
                    );
                });
            ui.end_row();
        });
}

// ── Color tab ──────────────────────────────────────────────────────────────────

fn draw_color_tab(
    app: &mut MondrianApp,
    ui: &mut egui::Ui,
    text_primary: egui::Color32,
    _bg_card: egui::Color32,
    _highlight: egui::Color32,
) {
    ui.label(egui::RichText::new("色彩模式").color(text_primary));
    ui.add_space(2.0);
    draw_color_mode_cards(ui, app);

    ui.add_space(6.0);
    ui.separator();
    ui.add_space(4.0);

    let info = color_mode_info(app.new_project_draft.color_mode);
    egui::Grid::new("color_info_grid")
        .num_columns(2)
        .spacing([10.0, 6.0])
        .show(ui, |ui| {
            ui.label(egui::RichText::new("工作空间").color(text_primary));
            ui.label(egui::RichText::new(info.working_label).color(text_primary));
            ui.end_row();
            ui.label(egui::RichText::new("位深").color(text_primary));
            ui.label(egui::RichText::new(info.bit_depth_label).color(text_primary));
            ui.end_row();
            ui.label(egui::RichText::new("引擎").color(text_primary));
            ui.label(egui::RichText::new(info.engine_label).color(text_primary));
            ui.end_row();
            ui.label(egui::RichText::new("色调映射").color(text_primary));
            ui.label(egui::RichText::new(info.tone_map_label).color(text_primary));
            ui.end_row();
        });
}

struct ColorModeInfo {
    working_label: &'static str,
    bit_depth_label: &'static str,
    engine_label: &'static str,
    tone_map_label: &'static str,
}

fn color_mode_info(mode: ColorMode) -> ColorModeInfo {
    match mode {
        ColorMode::Sdr => ColorModeInfo {
            working_label: "Rec.709",
            bit_depth_label: "8-bit",
            engine_label: "Mondrian Smart",
            tone_map_label: "关闭",
        },
        ColorMode::HdrPq => ColorModeInfo {
            working_label: "Rec.2100 PQ",
            bit_depth_label: "16-bit Float",
            engine_label: "Mondrian Smart",
            tone_map_label: "开启",
        },
        ColorMode::HdrHlg => ColorModeInfo {
            working_label: "Rec.2100 HLG",
            bit_depth_label: "16-bit Float",
            engine_label: "Mondrian Smart",
            tone_map_label: "开启",
        },
        ColorMode::Aces => ColorModeInfo {
            working_label: "ACEScg (via Rec.2020)",
            bit_depth_label: "16-bit Float",
            engine_label: "OpenColorIO (ACES 1.2)",
            tone_map_label: "开启（ACES ODT）",
        },
    }
}

// ── Audio tab ──────────────────────────────────────────────────────────────────

fn draw_audio_tab(app: &mut MondrianApp, ui: &mut egui::Ui, text_primary: egui::Color32) {
    egui::Grid::new("audio_grid")
        .num_columns(2)
        .spacing([10.0, 8.0])
        .show(ui, |ui| {
            ui.label(egui::RichText::new("采样率").color(text_primary));
            egui::ComboBox::from_id_salt("audio_sample_rate")
                .selected_text(format!("{} Hz", app.new_project_draft.audio_sample_rate))
                .show_ui(ui, |ui| {
                    for rate in [44100u32, 48000, 96000] {
                        ui.selectable_value(
                            &mut app.new_project_draft.audio_sample_rate,
                            rate,
                            format!("{rate} Hz"),
                        );
                    }
                });
            ui.end_row();

            ui.label(egui::RichText::new("声道布局").color(text_primary));
            egui::ComboBox::from_id_salt("audio_channel_layout")
                .selected_text(audio_channel_layout_label(
                    app.new_project_draft.audio_channel_layout,
                ))
                .show_ui(ui, |ui| {
                    for layout in [
                        AudioChannelLayout::Mono,
                        AudioChannelLayout::Stereo,
                        AudioChannelLayout::Surround51,
                    ] {
                        ui.selectable_value(
                            &mut app.new_project_draft.audio_channel_layout,
                            layout,
                            audio_channel_layout_label(layout),
                        );
                    }
                });
            ui.end_row();
        });
}

// ── Advanced tab ───────────────────────────────────────────────────────────────

fn draw_advanced_tab(
    app: &mut MondrianApp,
    ui: &mut egui::Ui,
    text_primary: egui::Color32,
    _text_muted: egui::Color32,
) {
    // ── 色彩高级 ──
    ui.collapsing("色彩高级", |ui| {
        egui::Grid::new("adv_color_grid")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                ui.label(egui::RichText::new("色彩工作流").color(text_primary));
                egui::ComboBox::from_id_salt("adv_color_workflow")
                    .selected_text(color_workflow_label(app.new_project_draft.color_workflow))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut app.new_project_draft.color_workflow,
                            ColorWorkflow::DisplayReferred,
                            color_workflow_label(ColorWorkflow::DisplayReferred),
                        );
                        ui.selectable_value(
                            &mut app.new_project_draft.color_workflow,
                            ColorWorkflow::SceneReferred,
                            color_workflow_label(ColorWorkflow::SceneReferred),
                        );
                        ui.selectable_value(
                            &mut app.new_project_draft.color_workflow,
                            ColorWorkflow::Aces,
                            color_workflow_label(ColorWorkflow::Aces),
                        );
                    });
                ui.end_row();

                ui.label(egui::RichText::new("色彩引擎").color(text_primary));
                egui::ComboBox::from_id_salt("adv_color_engine")
                    .selected_text(color_engine_label(&app.new_project_draft.engine))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut app.new_project_draft.engine,
                            ColorEngine::MondrianSmart,
                            "Mondrian Smart",
                        );
                        ui.selectable_value(
                            &mut app.new_project_draft.engine,
                            ColorEngine::Ocio { source: OcioConfigSource::Environment },
                            "OpenColorIO ($OCIO)",
                        );
                        for (name, ui_name) in &mondrian_core::builtin_config_entries() {
                            ui.selectable_value(
                                &mut app.new_project_draft.engine,
                                ColorEngine::Ocio {
                                    source: OcioConfigSource::Builtin { name: name.clone() },
                                },
                                format!("OCIO: {ui_name}"),
                            );
                        }
                    });
                ui.end_row();

                // OCIO path input
                if let ColorEngine::Ocio { source: OcioConfigSource::Path { path: ref mut p } } =
                    &mut app.new_project_draft.engine
                {
                    ui.label(egui::RichText::new("OCIO 路径").color(text_primary));
                    let mut s = p.to_string_lossy().into_owned();
                    if ui.text_edit_singleline(&mut s).changed() {
                        *p = PathBuf::from(s);
                    }
                    ui.end_row();
                }

                ui.label(egui::RichText::new("缺失元数据").color(text_primary));
                egui::ComboBox::from_id_salt("adv_missing_metadata")
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

                ui.label(egui::RichText::new("HDR 元数据").color(text_primary));
                ui.checkbox(&mut app.new_project_draft.preserve_hdr_metadata, "");
                ui.end_row();
            });

        ui.label(egui::RichText::new("视频电平").color(text_primary));
        egui::ComboBox::from_id_salt("adv_video_range")
            .selected_text(video_range_label(app.new_project_draft.video_range))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut app.new_project_draft.video_range,
                    VideoRange::Full,
                    video_range_label(VideoRange::Full),
                );
                ui.selectable_value(
                    &mut app.new_project_draft.video_range,
                    VideoRange::Legal,
                    video_range_label(VideoRange::Legal),
                );
            });
    });

    // ── 性能 ──
    ui.collapsing("性能", |ui| {
        egui::Grid::new("adv_perf_grid")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                ui.label(egui::RichText::new("预览格式").color(text_primary));
                egui::ComboBox::from_id_salt("adv_preview_format")
                    .selected_text(preview_render_format_label(
                        app.new_project_draft.preview_format,
                    ))
                    .show_ui(ui, |ui| {
                        for fmt in [
                            PreviewRenderFormat::IFrameOnly,
                            PreviewRenderFormat::ProResProxy,
                            PreviewRenderFormat::DnxHrLb,
                        ] {
                            ui.selectable_value(
                                &mut app.new_project_draft.preview_format,
                                fmt,
                                preview_render_format_label(fmt),
                            );
                        }
                    });
                ui.end_row();

                ui.label(egui::RichText::new("预览分辨率").color(text_primary));
                ui.add(
                    egui::Slider::new(
                        &mut app.new_project_draft.preview_resolution_scale,
                        0.125..=1.0,
                    )
                    .text("")
                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                );
                ui.end_row();

                ui.label(egui::RichText::new("预览缓存").color(text_primary));
                ui.checkbox(&mut app.new_project_draft.preview_cache_enabled, "");
                ui.end_row();
            });
    });
}

// ── Color mode cards ───────────────────────────────────────────────────────────

fn draw_color_mode_cards(ui: &mut egui::Ui, app: &mut MondrianApp) {
    let card_w = 88.0;
    let card_h = 48.0;
    let modes = [
        (ColorMode::Sdr, "SDR"),
        (ColorMode::HdrPq, "HDR PQ"),
        (ColorMode::HdrHlg, "HDR HLG"),
        (ColorMode::Aces, "ACES"),
    ];

    ui.horizontal(|ui| {
        for (mode, label) in modes {
            let selected = app.new_project_draft.color_mode == mode;
            let fill = if selected {
                crate::ui::theme::palette::interaction_highlight()
            } else {
                crate::ui::theme::palette::bg_surface_raised()
            };
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(card_w, card_h), egui::Sense::click());
            ui.painter().rect_filled(rect, 4.0, fill);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(13.0),
                if selected {
                    crate::ui::theme::palette::bg_surface()
                } else {
                    crate::ui::theme::palette::text_primary()
                },
            );
            if response.clicked() {
                app.new_project_draft.color_mode = mode;
                app.new_project_draft.apply_color_mode_defaults();
            }
        }
    });
}

// ── Helpers ────────────────────────────────────────────────────────────────────

fn fps_combo(ui: &mut egui::Ui, app: &mut MondrianApp) {
    let current = Rational::new(
        app.new_project_draft.fps_num.max(1),
        app.new_project_draft.fps_den.max(1),
    );
    egui::ComboBox::from_id_salt("basic_fps")
        .selected_text(frame_rate_label(current))
        .show_ui(ui, |ui| {
            for frame_rate in Rational::SEQUENCE_FRAME_RATES {
                let selected = app.new_project_draft.fps_num == frame_rate.num
                    && app.new_project_draft.fps_den == frame_rate.den;
                if ui.selectable_label(selected, frame_rate_label(frame_rate)).clicked() {
                    app.new_project_draft.fps_num = frame_rate.num;
                    app.new_project_draft.fps_den = frame_rate.den;
                }
            }
        });
}

fn is_ntsc_frame_rate(num: i64, den: i64) -> bool {
    let r = Rational::new(num.max(1), den.max(1));
    r.den == 1001 && (r.num == 30000 || r.num == 60000)
}

fn color_engine_label(engine: &ColorEngine) -> &str {
    match engine {
        ColorEngine::MondrianSmart => "Mondrian Smart",
        ColorEngine::Ocio { .. } => "OpenColorIO",
    }
}

fn default_project_path(name: &str) -> String {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    let base = PathBuf::from(home).join("Documents").join("Mondrian Projects");
    let dir_name = if name.trim().is_empty() {
        "未命名项目"
    } else {
        name.trim()
    };
    base.join(dir_name).to_string_lossy().into_owned()
}

// ── Commit ─────────────────────────────────────────────────────────────────────

fn commit_new_project(app: &mut MondrianApp) {
    // Apply colour-mode defaults before reading name (avoids borrow conflict).
    app.new_project_draft.apply_color_mode_defaults();

    let fps = Rational::new(
        app.new_project_draft.fps_num.max(1),
        app.new_project_draft.fps_den.max(1),
    );
    let project_name = if app.new_project_draft.name.trim().is_empty() {
        "未命名项目".to_string()
    } else {
        app.new_project_draft.name.trim().to_string()
    };
    let name: &str = &project_name;

    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    let default_dir = PathBuf::from(&home).join("Documents").join("Mondrian Projects").join(name);
    if let Err(e) = std::fs::create_dir_all(&default_dir) {
        tracing::warn!("无法创建默认项目目录 {}: {e}", default_dir.display());
    }

    let default_name = format!("{}.{}", sanitize_filename(name), PROJECT_EXTENSION);
    let initial_dir = std::path::PathBuf::from(&home).join("Documents").join("Mondrian Projects");
    let _ = std::fs::create_dir_all(&initial_dir);
    let picked = FileDialog::new()
        .add_filter("Mondrian Project", &[PROJECT_EXTENSION])
        .set_directory(initial_dir)
        .set_file_name(&default_name)
        .save_file();

    if let Some(path) = picked {
        let project_path = ensure_project_extension(path);

        let mut settings = SequenceSettings::default();
        settings.resolution.width = app.new_project_draft.width.max(1);
        settings.resolution.height = app.new_project_draft.height.max(1);
        settings.frame_rate = fps;
        settings.start_timecode_frame = app.new_project_draft.start_timecode_frame.max(0);
        settings.pixel_aspect_ratio = app.new_project_draft.pixel_aspect_ratio;
        settings.field_order = app.new_project_draft.field_order;
        settings.video_display_format = app.new_project_draft.video_display_format;
        settings.audio_sample_rate = app.new_project_draft.audio_sample_rate;
        settings.audio_channel_layout = app.new_project_draft.audio_channel_layout;
        settings.audio_channels = app.new_project_draft.audio_channel_layout.channels();
        settings.preview.format = app.new_project_draft.preview_format;
        settings.preview.resolution_scale = app.new_project_draft.preview_resolution_scale;
        settings.preview.cache_enabled = app.new_project_draft.preview_cache_enabled;

        // Color: derive from color_mode.
        settings.color_space = app.new_project_draft.working_color_space();
        settings.color_management.output_color_space = app.new_project_draft.output_color_space();
        settings.color_management.export_bit_depth = app.new_project_draft.export_bit_depth();
        settings.color_management.workflow = app.new_project_draft.color_workflow;
        settings.color_management.engine = app.new_project_draft.engine.clone();
        settings.color_management.missing_metadata_policy =
            app.new_project_draft.missing_color_metadata_policy;
        settings.color_management.nested_processing = app.new_project_draft.nested_color_processing;
        settings.color_management.video_range = app.new_project_draft.video_range;
        settings.color_management.preserve_hdr_metadata =
            app.new_project_draft.preserve_hdr_metadata;
        // Auto tone map for HDR and ACES workflows.
        settings.auto_tone_map_media = matches!(
            app.new_project_draft.color_mode,
            ColorMode::HdrPq | ColorMode::HdrHlg | ColorMode::Aces
        );

        let project_settings = ProjectSettings {
            color_management: ProjectColorManagement {
                engine: app.new_project_draft.engine.clone(),
            },
            ..ProjectSettings::default()
        };

        if let Err(err) = app.state.create_new_project_with_settings_at(
            project_path.clone(),
            name,
            settings,
            project_settings,
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
