use super::*;
use mondrian_core::DisplayColorProfile;

fn corner_radius(radius: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(radius.round().clamp(0.0, u8::MAX as f32) as u8)
}

fn margin_i8(value: f32) -> i8 {
    value.round().clamp(i8::MIN as f32, i8::MAX as f32) as i8
}

pub(super) fn load_app_preferences(app: &mut MondrianApp) {
    let Ok(bytes) = fs::read(&app.app_config_path) else {
        app.last_saved_preferences = Some(capture_preferences(app));
        return;
    };

    let Ok(preferences) = serde_json::from_slice::<AppPreferences>(&bytes) else {
        app.last_saved_preferences = Some(capture_preferences(app));
        return;
    };

    app.show_library = preferences.show_library;
    app.show_effect_controls = preferences.show_effect_controls;
    app.show_effect_library = preferences.show_effect_library;
    app.theme = preferences.theme;
    app.state.auto_proxy_enabled = preferences.auto_proxy_enabled;
    app.show_dev_metrics = if cfg!(debug_assertions) {
        preferences.show_dev_metrics
    } else {
        false
    };
    let _ = preferences.av_clock_role;
    app.state.audio_sync.role = ClockRole::AudioMaster;
    app.new_project_draft = preferences.new_project_draft.clone();
    app.shortcuts = preferences.shortcuts.clone();
    app.media_cache_auto_cleanup = preferences.media_cache_auto_cleanup;
    app.media_cache_max_size_gb = preferences.media_cache_max_size_gb.max(1);
    app.media_cache_max_age_days = preferences.media_cache_max_age_days.max(1);
    app.auto_save_enabled = preferences.auto_save_enabled;
    app.auto_save_interval_secs = preferences.auto_save_interval_secs.max(10);
    app.auto_save_max_recovery_points = preferences.auto_save_max_recovery_points.max(1);
    app.auto_save_retention_days = preferences.auto_save_retention_days.max(1);
    app.recent_projects = preferences.recent_projects.clone();
    app.show_video_metrics = preferences.show_video_metrics;
    app.show_audio_metrics = preferences.show_audio_metrics;
    app.timeline_panel_height = preferences.timeline_panel_height.max(160.0);
    app.viewer_panel.apply_preferences(&preferences.viewer);
    app.last_saved_preferences = Some(preferences);
}

pub(super) fn capture_preferences(app: &MondrianApp) -> AppPreferences {
    AppPreferences {
        version: 1,
        theme: app.theme,
        show_effect_controls: app.show_effect_controls,
        show_effect_library: app.show_effect_library,
        show_library: app.show_library,
        auto_proxy_enabled: app.state.auto_proxy_enabled,
        show_dev_metrics: if cfg!(debug_assertions) {
            app.show_dev_metrics
        } else {
            false
        },
        av_clock_role: ClockRole::AudioMaster,
        new_project_draft: app.new_project_draft.clone(),
        shortcuts: app.shortcuts.clone(),
        media_cache_auto_cleanup: app.media_cache_auto_cleanup,
        media_cache_max_size_gb: app.media_cache_max_size_gb.max(1),
        media_cache_max_age_days: app.media_cache_max_age_days.max(1),
        auto_save_enabled: app.auto_save_enabled,
        auto_save_interval_secs: app.auto_save_interval_secs.max(10),
        auto_save_max_recovery_points: app.auto_save_max_recovery_points.max(1),
        auto_save_retention_days: app.auto_save_retention_days.max(1),
        recent_projects: app.recent_projects.clone(),
        show_video_metrics: app.show_video_metrics,
        show_audio_metrics: app.show_audio_metrics,
        timeline_panel_height: app.timeline_panel_height.max(160.0),
        viewer: app.viewer_panel.preferences_snapshot(),
    }
}

pub(super) fn run_project_autosave_if_needed(app: &mut MondrianApp) {
    if !app.auto_save_enabled {
        app.last_auto_save_at = None;
        app.auto_save_error_reported = false;
        return;
    }

    if !app.state.has_open_project() {
        app.last_auto_save_at = None;
        app.auto_save_error_reported = false;
        return;
    }

    let interval = Duration::from_secs(app.auto_save_interval_secs.max(10) as u64);
    let now = std::time::Instant::now();
    if let Some(last) = app.last_auto_save_at {
        if now.saturating_duration_since(last) < interval {
            return;
        }
    }

    app.last_auto_save_at = Some(now);
    match app.state.write_autosave_snapshot(
        app.auto_save_max_recovery_points.max(1) as usize,
        app.auto_save_retention_days.max(1),
    ) {
        Ok(path) => {
            app.auto_save_error_reported = false;
            tracing::debug!("自动保存完成: {}", path.display());
        }
        Err(err) => {
            tracing::error!("自动保存失败: {err}");
            if !app.auto_save_error_reported {
                app.state.set_status_hint(format!("自动保存失败：{err}"), true);
                app.auto_save_error_reported = true;
            }
        }
    }
}

pub(super) fn run_cache_maintenance_if_needed(app: &mut MondrianApp) {
    if let Some(rx) = app.cache_maintenance_rx.as_ref() {
        match rx.try_recv() {
            Ok(Ok(stats)) => {
                if stats.deleted_files > 0 {
                    tracing::debug!(
                        "自动清理媒体缓存完成: 删除 {} 文件, 释放 {} bytes",
                        stats.deleted_files,
                        stats.deleted_bytes
                    );
                }
                app.cache_maintenance_in_flight = false;
                app.cache_maintenance_rx = None;
            }
            Ok(Err(err)) => {
                tracing::debug!("自动清理媒体缓存失败: {}", err);
                app.cache_maintenance_in_flight = false;
                app.cache_maintenance_rx = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                app.cache_maintenance_in_flight = false;
                app.cache_maintenance_rx = None;
            }
        }
    }

    if !app.media_cache_auto_cleanup {
        app.last_cache_maintenance_at = None;
        return;
    }

    if app.cache_maintenance_in_flight {
        return;
    }

    let now = std::time::Instant::now();
    if let Some(last) = app.last_cache_maintenance_at {
        if now.saturating_duration_since(last) < Duration::from_secs(60) {
            return;
        }
    }

    let policy = crate::ui::viewer_panel::MediaCachePolicy {
        max_size_bytes: (app.media_cache_max_size_gb.max(1) as u64)
            .saturating_mul(1024)
            .saturating_mul(1024)
            .saturating_mul(1024),
        max_age_days: app.media_cache_max_age_days.max(1) as u64,
    };

    let cache_dir = app.viewer_panel.media_cache_dir();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result =
            crate::ui::viewer_panel::run_media_cache_maintenance_for_dir(cache_dir, policy);
        let _ = tx.send(result);
    });
    app.cache_maintenance_in_flight = true;
    app.cache_maintenance_rx = Some(rx);

    app.last_cache_maintenance_at = Some(now);
}

pub(super) fn process_global_shortcuts(app: &mut MondrianApp, ctx: &egui::Context) {
    if app.show_preferences_dialog || ctx.wants_keyboard_input() {
        return;
    }

    let action = ctx.input(|i| {
        [
            ShortcutAction::ImportMedia,
            ShortcutAction::OpenProject,
            ShortcutAction::SaveProject,
            ShortcutAction::SaveProjectAs,
            ShortcutAction::CloseProject,
            ShortcutAction::QuitApp,
        ]
        .into_iter()
        .find(|action| {
            shortcut_binding(app, *action)
                .map(|binding| binding.matches_input(i))
                .unwrap_or(false)
        })
    });

    match action {
        Some(ShortcutAction::ImportMedia) => trigger_import_media(app),
        Some(ShortcutAction::OpenProject) => app.open_project_dialog(),
        Some(ShortcutAction::SaveProject) => {
            if let Err(err) = app.state.save_project() {
                tracing::error!("保存项目失败: {err}");
                app.state.set_status_hint(format!("保存项目失败：{err}"), true);
            } else {
                app.state.set_status_hint("项目已保存", false);
            }
        }
        Some(ShortcutAction::SaveProjectAs) => save_project_as_dialog(app),
        Some(ShortcutAction::CloseProject) => {
            app.request_close_project();
        }
        Some(ShortcutAction::QuitApp) => {
            app.request_quit_app(ctx);
        }
        None => {}
    }
}

pub(super) fn shortcut_binding(
    app: &MondrianApp,
    action: ShortcutAction,
) -> Option<&ShortcutBinding> {
    match action {
        ShortcutAction::ImportMedia => app.shortcuts.import_media.as_ref(),
        ShortcutAction::OpenProject => app.shortcuts.open_project.as_ref(),
        ShortcutAction::SaveProject => app.shortcuts.save_project.as_ref(),
        ShortcutAction::SaveProjectAs => app.shortcuts.save_project_as.as_ref(),
        ShortcutAction::CloseProject => app.shortcuts.close_project.as_ref(),
        ShortcutAction::QuitApp => app.shortcuts.quit_app.as_ref(),
    }
}

pub(super) fn shortcut_binding_mut(
    app: &mut MondrianApp,
    action: ShortcutAction,
) -> &mut Option<ShortcutBinding> {
    match action {
        ShortcutAction::ImportMedia => &mut app.shortcuts.import_media,
        ShortcutAction::OpenProject => &mut app.shortcuts.open_project,
        ShortcutAction::SaveProject => &mut app.shortcuts.save_project,
        ShortcutAction::SaveProjectAs => &mut app.shortcuts.save_project_as,
        ShortcutAction::CloseProject => &mut app.shortcuts.close_project,
        ShortcutAction::QuitApp => &mut app.shortcuts.quit_app,
    }
}

pub(super) fn shortcut_action_title(action: ShortcutAction) -> &'static str {
    match action {
        ShortcutAction::ImportMedia => "导入媒体",
        ShortcutAction::OpenProject => "打开项目",
        ShortcutAction::SaveProject => "保存项目",
        ShortcutAction::SaveProjectAs => "另存为",
        ShortcutAction::CloseProject => "关闭项目",
        ShortcutAction::QuitApp => "退出",
    }
}

pub(super) fn shortcut_action_label(app: &MondrianApp, action: ShortcutAction) -> String {
    shortcut_binding(app, action)
        .map(|s| s.display_text())
        .unwrap_or_else(|| "未设置".to_string())
}

pub(super) fn trigger_import_media(app: &mut MondrianApp) {
    if !app.state.has_open_project() {
        return;
    }
    app.show_library = true;
    app.library_panel.open_import_dialog(&mut app.state);
}

pub(super) fn save_project_as_dialog(app: &mut MondrianApp) {
    if !app.state.has_open_project() {
        app.state.set_status_hint("当前无可另存项目", true);
        return;
    }

    let default_name = app
        .state
        .current_project_path
        .as_ref()
        .and_then(|p| p.file_name().and_then(|n| n.to_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| format!("未命名项目.{}", PROJECT_EXTENSION));

    let picked = FileDialog::new()
        .add_filter("Mondrian Project", &[PROJECT_EXTENSION])
        .set_file_name(&default_name)
        .save_file();
    let Some(path) = picked else {
        return;
    };

    let target_path = ensure_project_extension(path);
    let previous = app.state.current_project_path.clone();
    app.state.current_project_path = Some(target_path.clone());
    if let Err(err) = app.state.save_project() {
        app.state.current_project_path = previous;
        app.state.set_status_hint(format!("另存为失败：{err}"), true);
        return;
    }

    app.state
        .set_status_hint(format!("项目已另存为：{}", target_path.display()), false);
}

pub(super) fn capture_shortcut_input(app: &mut MondrianApp, ctx: &egui::Context) {
    let Some(action) = app.capturing_shortcut else {
        return;
    };

    let maybe_key = ctx.input(|i| {
        i.events.iter().find_map(|event| {
            if let egui::Event::Key { key, pressed, repeat, modifiers, .. } = event {
                if *pressed && !*repeat {
                    return Some((*key, *modifiers));
                }
            }
            None
        })
    });

    let Some((key, modifiers)) = maybe_key else {
        return;
    };

    if key == egui::Key::Escape {
        app.capturing_shortcut = None;
        return;
    }

    let Some(shortcut_key) = ShortcutKey::from_egui(key) else {
        return;
    };

    let binding = ShortcutBinding {
        command: modifiers.command,
        shift: modifiers.shift,
        alt: modifiers.alt,
        key: shortcut_key,
    };

    *shortcut_binding_mut(app, action) = Some(binding);
    app.state.set_status_hint(
        format!(
            "已更新快捷键：{} = {}",
            shortcut_action_title(action),
            shortcut_action_label(app, action)
        ),
        false,
    );

    app.capturing_shortcut = None;
}

pub(super) fn draw_preferences_window(app: &mut MondrianApp, ctx: &egui::Context) {
    const WINDOW_DEFAULT_WIDTH: f32 = 800.0;
    const WINDOW_DEFAULT_HEIGHT: f32 = 568.0;
    const WINDOW_MIN_WIDTH: f32 = 760.0;
    const WINDOW_MIN_HEIGHT: f32 = 568.0;
    let viewport_id = egui::ViewportId::from_hash_of("preferences_viewport");
    let viewport_builder = egui::ViewportBuilder::default()
        .with_title("首选项")
        .with_inner_size([WINDOW_DEFAULT_WIDTH, WINDOW_DEFAULT_HEIGHT])
        .with_min_inner_size([WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT])
        .with_resizable(true)
        .with_active(true);

    ctx.show_viewport_immediate(viewport_id, viewport_builder, |viewport_ctx, class| {
        crate::ui::theme::apply_theme(viewport_ctx, app.theme);
        viewport_ctx
            .send_viewport_cmd(egui::ViewportCommand::SetTheme(app.theme.to_system_theme()));

        if viewport_ctx.input(|i| i.viewport().close_requested()) {
            app.show_preferences_dialog = false;
            app.capturing_shortcut = None;
            viewport_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        match class {
            egui::ViewportClass::Embedded => {
                let mut open = app.show_preferences_dialog;
                egui::Window::new("首选项")
                    .open(&mut open)
                    .collapsible(false)
                    .default_size([WINDOW_DEFAULT_WIDTH, WINDOW_DEFAULT_HEIGHT])
                    .min_size([WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT])
                    .resizable(true)
                    .show(viewport_ctx, |ui| {
                        draw_preferences_panel(app, ui);
                    });
                app.show_preferences_dialog = open;
            }
            _ => {
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::new()
                            .fill(crate::ui::theme::palette::bg_surface())
                            .inner_margin(egui::Margin::symmetric(18, 18)),
                    )
                    .show(viewport_ctx, |ui| {
                        draw_preferences_panel(app, ui);
                    });
            }
        }
    });

    if !app.show_preferences_dialog {
        app.capturing_shortcut = None;
    }
}

fn draw_preferences_panel(app: &mut MondrianApp, ui: &mut egui::Ui) {
    const BODY_MIN_HEIGHT: f32 = 492.0;
    const NAV_WIDTH: f32 = 132.0;
    const DIVIDER_WIDTH: f32 = 14.0;
    const CONTENT_PADDING_X: f32 = 24.0;
    const CONTENT_PADDING_Y: f32 = 8.0;

    let body_height = ui.available_height().max(BODY_MIN_HEIGHT);
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), body_height),
        egui::Layout::left_to_right(egui::Align::Min),
        |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;

            ui.allocate_ui_with_layout(
                egui::vec2(NAV_WIDTH, body_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    draw_preferences_nav(ui, &mut app.preferences_tab);
                },
            );

            let (divider_rect, _) = ui
                .allocate_exact_size(egui::vec2(DIVIDER_WIDTH, body_height), egui::Sense::hover());
            ui.painter().line_segment(
                [
                    egui::pos2(divider_rect.center().x, divider_rect.top()),
                    egui::pos2(divider_rect.center().x, divider_rect.bottom()),
                ],
                egui::Stroke::new(1.0, crate::ui::theme::palette::panel_divider_strong()),
            );

            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), body_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(
                            margin_i8(CONTENT_PADDING_X),
                            margin_i8(CONTENT_PADDING_Y),
                        ))
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("preferences_content_scroll")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    draw_preferences_tab_content(app, ui);
                                    ui.add_space(8.0);
                                });
                        });
                },
            );
        },
    );
}

fn draw_preferences_nav(ui: &mut egui::Ui, current_tab: &mut PreferencesTab) {
    ui.spacing_mut().item_spacing.y = 6.0;
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("设置")
            .font(crate::ui::theme::typography::body_large())
            .strong()
            .color(crate::ui::theme::palette::text_primary()),
    );
    ui.add_space(14.0);

    draw_preferences_nav_button(ui, current_tab, PreferencesTab::General, "常规");
    draw_preferences_nav_button(ui, current_tab, PreferencesTab::Media, "媒体");
    draw_preferences_nav_button(ui, current_tab, PreferencesTab::Shortcuts, "快捷键");
    if cfg!(debug_assertions) {
        draw_preferences_nav_button(ui, current_tab, PreferencesTab::Developer, "开发");
    }
}

fn draw_preferences_nav_button(
    ui: &mut egui::Ui,
    current_tab: &mut PreferencesTab,
    value: PreferencesTab,
    label: &str,
) {
    let selected = *current_tab == value;
    let desired_size = egui::vec2(ui.available_width(), 30.0);
    let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if selected {
            crate::ui::theme::palette::accent_secondary().gamma_multiply(0.55)
        } else if response.hovered() {
            crate::ui::theme::palette::bg_surface_hover()
        } else {
            egui::Color32::TRANSPARENT
        };
        let stroke = if selected {
            egui::Stroke::new(1.0, crate::ui::theme::palette::interaction_highlight())
        } else {
            egui::Stroke::NONE
        };
        ui.painter().rect(
            rect,
            corner_radius(crate::ui::theme::tokens::button_rounding()),
            fill,
            stroke,
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            rect.left_center() + egui::vec2(14.0, 0.0),
            egui::Align2::LEFT_CENTER,
            label,
            crate::ui::theme::typography::body(),
            if selected {
                crate::ui::theme::palette::text_primary()
            } else {
                crate::ui::theme::palette::text_muted()
            },
        );
    }
    if response.clicked() {
        *current_tab = value;
    }
}

fn draw_preferences_tab_content(app: &mut MondrianApp, ui: &mut egui::Ui) {
    match app.preferences_tab {
        PreferencesTab::General => draw_general_preferences(app, ui),
        PreferencesTab::Media => draw_media_preferences(app, ui),
        PreferencesTab::Shortcuts => draw_shortcut_preferences(app, ui),
        PreferencesTab::Developer => draw_developer_preferences(app, ui),
    }
}

fn draw_general_preferences(app: &mut MondrianApp, ui: &mut egui::Ui) {
    draw_preferences_header(ui, "常规", None);
    draw_preferences_section(ui, "外观", |ui| {
        preference_labeled_row(ui, "主题", None, |ui| {
            let previous_theme = app.theme;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 18.0;
                for theme_option in crate::ui::theme::Theme::ALL {
                    ui.radio_value(
                        &mut app.theme,
                        theme_option,
                        egui::RichText::new(theme_option.display_name())
                            .color(crate::ui::theme::palette::text_primary()),
                    );
                }
            });
            if app.theme != previous_theme {
                let label = app.theme.display_name();
                app.state.set_status_hint(format!("应用主题已切换为 {label}"), false);
            }
        });
    });

    draw_preferences_section(ui, "项目安全", |ui| {
        preference_toggle_row(
            ui,
            &mut app.auto_save_enabled,
            "启用自动保存",
            Some("定时写入恢复点；异常退出后可在启动界面恢复。"),
        );
        ui.add_space(10.0);
        ui.add_enabled_ui(app.auto_save_enabled, |ui| {
            preference_labeled_row(
                ui,
                "自动保存间隔",
                Some("推荐 30-120 秒。"),
                |ui| {
                    ui.add(
                        egui::DragValue::new(&mut app.auto_save_interval_secs)
                            .range(10..=3600)
                            .speed(1)
                            .suffix(" 秒"),
                    );
                },
            );
            preference_labeled_row(
                ui,
                "恢复点数量",
                Some("达到上限后将覆盖较旧恢复点。"),
                |ui| {
                    ui.add(
                        egui::DragValue::new(&mut app.auto_save_max_recovery_points)
                            .range(1..=100)
                            .speed(1),
                    );
                },
            );
            preference_labeled_row(
                ui,
                "保留时长",
                Some("过期恢复点会自动清理。"),
                |ui| {
                    ui.add(
                        egui::DragValue::new(&mut app.auto_save_retention_days)
                            .range(1..=365)
                            .speed(1)
                            .suffix(" 天"),
                    );
                },
            );
        });
        ui.add_space(8.0);
        preference_status_line(
            ui,
            &format!("当前可恢复点：{}", app.crash_recovery_candidates.len()),
            Some("恢复点文件保存在本地配置目录；清理后不可恢复。"),
        );
        ui.add_space(10.0);

        if ui.button("清理所有恢复点").clicked() {
            match clear_all_crash_recovery_points() {
                Ok(removed) => {
                    app.crash_recovery_candidates = discover_crash_recovery_candidates();
                    app.state.set_status_hint(format!("已清理恢复点文件：{} 个", removed), false);
                }
                Err(err) => {
                    app.state.set_status_hint(format!("清理恢复点失败：{err}"), true);
                }
            }
        }
    });
}

fn draw_media_preferences(app: &mut MondrianApp, ui: &mut egui::Ui) {
    draw_preferences_header(ui, "媒体", None);

    draw_preferences_section(ui, "预览", |ui| {
        preference_labeled_row(
            ui,
            "解码后端",
            Some("自动为默认稳定策略；GPU 辅助属于实验能力，失败会回退到软件解码。"),
            |ui| {
                let mut backend = app.viewer_panel.preview_decode_backend();
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 18.0;
                    ui.radio_value(
                        &mut backend,
                        mondrian_media::PreviewDecodeBackend::Auto,
                        egui::RichText::new("自动")
                            .color(crate::ui::theme::palette::text_primary()),
                    );
                    ui.radio_value(
                        &mut backend,
                        mondrian_media::PreviewDecodeBackend::Software,
                        egui::RichText::new("软件")
                            .color(crate::ui::theme::palette::text_primary()),
                    );
                    ui.radio_value(
                        &mut backend,
                        mondrian_media::PreviewDecodeBackend::GpuAssist,
                        egui::RichText::new("GPU辅助")
                            .color(crate::ui::theme::palette::text_primary()),
                    );
                });
                if backend != app.viewer_panel.preview_decode_backend() {
                    app.viewer_panel.set_preview_decode_backend(backend);
                }
            },
        );

        preference_toggle_row(
            ui,
            &mut app.state.auto_proxy_enabled,
            "自动代理媒体文件",
            Some("导入视频素材时自动启用代理模式，并在后台生成代理。"),
        );
    });

    draw_preferences_section(ui, "显示器 profile", |ui| {
        let mut display_profile = app.viewer_panel.display_profile_snapshot();
        let mut changed = false;
        let mut profile_kind = if display_profile == DisplayColorProfile::rec709_reference() {
            0usize
        } else if display_profile == DisplayColorProfile::display_p3_reference() {
            1usize
        } else {
            2usize
        };

        preference_labeled_row(
            ui,
            "预置",
            Some("Rec.709 / Display P3 是内置参考 profile；自定义模式保留手动编辑参数。"),
            |ui| {
                egui::ComboBox::from_id_salt("display_profile_preset")
                    .selected_text(match profile_kind {
                        0 => "Rec.709 参考",
                        1 => "Display P3 参考",
                        _ => "自定义",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut profile_kind, 0, "Rec.709 参考");
                        ui.selectable_value(&mut profile_kind, 1, "Display P3 参考");
                        ui.selectable_value(&mut profile_kind, 2, "自定义");
                    });
            },
        );

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if ui.button("导入 ICC/ICM").clicked() {
                if let Some(path) =
                    FileDialog::new().add_filter("ICC Profile", &["icc", "icm"]).pick_file()
                {
                    match DisplayColorProfile::from_icc_file(&path) {
                        Ok(profile) => {
                            let profile_name = profile.name.clone();
                            app.viewer_panel.set_display_profile(profile);
                            app.state.set_status_hint(
                                format!("ICC 已导入：{}（{}）", path.display(), profile_name),
                                false,
                            );
                        }
                        Err(err) => {
                            app.state.set_status_hint(format!("ICC 解析失败：{err}"), true);
                        }
                    }
                }
            }
        });

        if profile_kind == 0 {
            let preset_profile = DisplayColorProfile::rec709_reference();
            if display_profile != preset_profile {
                display_profile = preset_profile;
                changed = true;
            }
        } else if profile_kind == 1 {
            let preset_profile = DisplayColorProfile::display_p3_reference();
            if display_profile != preset_profile {
                display_profile = preset_profile;
                changed = true;
            }
        } else {
            preference_labeled_row(ui, "名称", None, |ui| {
                changed |= ui.text_edit_singleline(&mut display_profile.name).changed();
            });

            preference_labeled_row(ui, "显示色彩空间", None, |ui| {
                let mut selected_space = display_profile.color_space;
                egui::ComboBox::from_id_salt("display_profile_color_space")
                    .selected_text(color_space_label(selected_space))
                    .show_ui(ui, |ui| {
                        for color_space in color_space_options() {
                            ui.selectable_value(
                                &mut selected_space,
                                color_space,
                                color_space_label(color_space),
                            );
                        }
                    });
                if selected_space != display_profile.color_space {
                    display_profile.color_space = selected_space;
                    changed = true;
                }
            });

            preference_labeled_row(ui, "Gamma", None, |ui| {
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut display_profile.gamma)
                            .range(0.1..=10.0)
                            .speed(0.01),
                    )
                    .changed();
            });

            preference_labeled_row(ui, "黑位亮度", None, |ui| {
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut display_profile.black_luminance_nits)
                            .range(0.0..=1000.0)
                            .speed(0.1)
                            .suffix(" nits"),
                    )
                    .changed();
            });

            preference_labeled_row(ui, "白位亮度", None, |ui| {
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut display_profile.white_luminance_nits)
                            .range(0.1..=10_000.0)
                            .speed(0.5)
                            .suffix(" nits"),
                    )
                    .changed();
            });

            preference_labeled_row(ui, "校正矩阵", None, |ui| {
                ui.vertical(|ui| {
                    egui::Grid::new("display_profile_matrix").spacing([8.0, 4.0]).show(ui, |ui| {
                        for row in 0..3 {
                            for col in 0..3 {
                                changed |= ui
                                    .add(
                                        egui::DragValue::new(
                                            &mut display_profile.linear_matrix[row][col],
                                        )
                                        .speed(0.01)
                                        .range(-4.0..=4.0),
                                    )
                                    .changed();
                            }
                            ui.end_row();
                        }
                    });
                });
            });
        }

        if changed {
            app.viewer_panel.set_display_profile(display_profile);
            app.state.set_status_hint("显示器 profile 已更新", false);
        }
    });

    draw_preferences_section(ui, "LUT 库", |ui| {
        let library = mondrian_effects::LutLibrary::new(app_lut_library_dir());
        preference_status_line(
            ui,
            &format!("目录：{}", library.root().display()),
            Some("导入的 .cube 文件会被复制到应用数据目录，项目中的 LUT 效果可以直接引用这里的文件。"),
        );
        ui.add_space(10.0);

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.label("搜索");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut app.lut_library_filter)
                        .hint_text("按名称、路径或尺寸过滤"),
                )
                .changed()
            {
                app.state.set_status_hint("LUT 列表已更新过滤条件", false);
            }
            if ui.button("清空").clicked() {
                app.lut_library_filter.clear();
            }
            if ui.button("刷新目录").clicked() {
                app.state.set_status_hint("已刷新 LUT 库目录", false);
            }
        });

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if ui.button("创建目录").clicked() {
                match library.ensure_root() {
                    Ok(()) => app.state.set_status_hint("LUT 库目录已就绪", false),
                    Err(err) => app.state.set_status_hint(format!("创建 LUT 库失败：{err}"), true),
                }
            }
            if ui.button("导入 .cube LUT").clicked() {
                if let Some(path) = FileDialog::new().add_filter("Cube LUT", &["cube"]).pick_file()
                {
                    match library.import_cube_file(&path) {
                        Ok(imported) => app
                            .state
                            .set_status_hint(format!("LUT 已导入：{}", imported.display()), false),
                        Err(err) => {
                            app.state.set_status_hint(format!("导入 LUT 失败：{err}"), true)
                        }
                    }
                }
            }
        });

        match library.search_luts(&app.lut_library_filter) {
            Ok(entries) if entries.is_empty() => {
                preference_status_line(ui, "当前没有已导入的 LUT", None);
            }
            Ok(entries) => {
                preference_status_line(ui, &format!("已导入 {} 个 LUT", entries.len()), None);
                for entry in entries.iter().take(16) {
                    let path_text = entry.path.display().to_string();
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            preference_status_line(
                                ui,
                                &format!("{} · {}³", entry.name, entry.size),
                                Some(&path_text),
                            );
                        });
                        if ui.button("删除").clicked() {
                            match library.remove_lut(&entry.path) {
                                Ok(()) => app.state.set_status_hint(
                                    format!("已删除 LUT：{}", entry.path.display()),
                                    false,
                                ),
                                Err(err) => {
                                    app.state.set_status_hint(format!("删除 LUT 失败：{err}"), true)
                                }
                            }
                        }
                    });
                }
            }
            Err(err) => {
                preference_status_line(ui, &format!("读取 LUT 库失败：{err}"), None);
            }
        }
    });

    draw_preferences_section(ui, "缓存", |ui| {
        preference_toggle_row(
            ui,
            &mut app.media_cache_auto_cleanup,
            "自动清理媒体缓存",
            Some("按照容量和保留天数执行清理，不会影响原始媒体文件。"),
        );

        let cache_status = match app.viewer_panel.media_cache_usage_stats() {
            Ok(usage) => format!(
                "当前缓存：{} 个文件，约 {:.1} MB",
                usage.file_count,
                usage.total_bytes as f64 / 1024.0 / 1024.0
            ),
            Err(err) => format!("当前缓存统计失败：{}", err),
        };
        preference_status_line(ui, &cache_status, None);
        ui.add_space(10.0);

        ui.add_enabled_ui(app.media_cache_auto_cleanup, |ui| {
            preference_labeled_row(ui, "缓存上限", None, |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.media_cache_max_size_gb)
                        .range(1..=4096)
                        .speed(1)
                        .suffix(" GB"),
                );
            });
            preference_labeled_row(ui, "保留天数", None, |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.media_cache_max_age_days)
                        .range(1..=3650)
                        .speed(1)
                        .suffix(" 天"),
                );
            });
        });

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if ui.button("清空媒体缓存").clicked() {
                app.state.audio_source_cache.clear();
                match app.viewer_panel.clear_all_media_cache() {
                    Ok(stats) => {
                        app.state.set_status_hint(
                            format!(
                                "媒体缓存已清空（{} 个文件，约 {:.1} MB）",
                                stats.deleted_files,
                                stats.deleted_bytes as f64 / 1024.0 / 1024.0,
                            ),
                            false,
                        );
                    }
                    Err(err) => {
                        app.state.set_status_hint(format!("清空媒体缓存失败：{err}"), true);
                    }
                }
            }

            if ui.button("立即执行自动清理").clicked() {
                let policy = crate::ui::viewer_panel::MediaCachePolicy {
                    max_size_bytes: (app.media_cache_max_size_gb.max(1) as u64)
                        .saturating_mul(1024)
                        .saturating_mul(1024)
                        .saturating_mul(1024),
                    max_age_days: app.media_cache_max_age_days.max(1) as u64,
                };
                match app.viewer_panel.run_media_cache_maintenance(policy) {
                    Ok(stats) => {
                        app.state.set_status_hint(
                            format!(
                                "媒体缓存已清理（{} 个文件，约 {:.1} MB）",
                                stats.deleted_files,
                                stats.deleted_bytes as f64 / 1024.0 / 1024.0,
                            ),
                            false,
                        );
                        app.last_cache_maintenance_at = Some(std::time::Instant::now());
                    }
                    Err(err) => {
                        app.state.set_status_hint(format!("自动清理媒体缓存失败：{err}"), true);
                    }
                }
            }
        });
    });
}

fn draw_shortcut_preferences(app: &mut MondrianApp, ui: &mut egui::Ui) {
    draw_preferences_header(ui, "快捷键", None);

    let actions = [
        ShortcutAction::ImportMedia,
        ShortcutAction::OpenProject,
        ShortcutAction::SaveProject,
        ShortcutAction::SaveProjectAs,
        ShortcutAction::CloseProject,
        ShortcutAction::QuitApp,
    ];
    let action_count = actions.len();

    ui.add_space(4.0);
    for (index, action) in actions.into_iter().enumerate() {
        draw_shortcut_row(app, ui, action);
        if index + 1 != action_count {
            ui.add_space(6.0);
            let y = ui.cursor().top();
            ui.painter().line_segment(
                [
                    egui::pos2(ui.min_rect().left(), y),
                    egui::pos2(ui.max_rect().right(), y),
                ],
                egui::Stroke::new(1.0, crate::ui::theme::palette::border_subtle()),
            );
            ui.add_space(6.0);
        }
    }

    ui.add_space(16.0);
    if ui.button("恢复默认快捷键").clicked() {
        app.shortcuts = ShortcutPreferences::default();
        app.capturing_shortcut = None;
        app.state.set_status_hint("已恢复默认快捷键", false);
    }
}

fn draw_developer_preferences(app: &mut MondrianApp, ui: &mut egui::Ui) {
    draw_preferences_header(ui, "开发", None);

    draw_preferences_section(ui, "指标显示", |ui| {
        preference_toggle_row(
            ui,
            &mut app.show_dev_metrics,
            "显示开发指标",
            Some("Perf1s 分段指标可帮助判断瓶颈位于解码、合成还是上传。"),
        );

        ui.add_space(10.0);
        ui.add_enabled_ui(app.show_dev_metrics, |ui| {
            preference_toggle_row(ui, &mut app.show_video_metrics, "显示视频指标", None);
            preference_toggle_row(ui, &mut app.show_audio_metrics, "显示音频指标", None);
        });
    });

    draw_preferences_section(ui, "缓存控制", |ui| {
        let mut prefetch_enabled = app.viewer_panel.prefetch_enabled();
        if preference_toggle_row(
            ui,
            &mut prefetch_enabled,
            "启用预取缓冲",
            Some("提前请求相邻帧，适合顺播与拖拽预览。"),
        )
        .clicked()
        {
            app.viewer_panel.set_prefetch_enabled(prefetch_enabled);
            app.state.set_status_hint(
                if prefetch_enabled {
                    "已启用预取缓冲"
                } else {
                    "已禁用预取缓冲"
                },
                false,
            );
        }

        let mut layer_cache_enabled = app.viewer_panel.layer_cache_enabled();
        if preference_toggle_row(
            ui,
            &mut layer_cache_enabled,
            "启用图层缓存",
            Some("缓存合成后的中间结果，复杂时间线会更受益。"),
        )
        .clicked()
        {
            app.viewer_panel.set_layer_cache_enabled(layer_cache_enabled);
            app.state.set_status_hint(
                if layer_cache_enabled {
                    "已启用图层缓存"
                } else {
                    "已禁用图层缓存"
                },
                false,
            );
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if ui.button("清空图层缓存").clicked() {
                app.viewer_panel.clear_layer_cache();
                app.state.set_status_hint("图层缓存已清空", false);
            }
        });
    });
}

fn draw_preferences_header(ui: &mut egui::Ui, title: &str, info: Option<&str>) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(title)
                .font(crate::ui::theme::typography::body_large())
                .strong()
                .color(crate::ui::theme::palette::text_primary()),
        );
        if let Some(text) = info {
            draw_info_icon(ui, text);
        }
    });
    ui.add_space(14.0);
}

fn draw_preferences_section(
    ui: &mut egui::Ui,
    title: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    ui.label(
        egui::RichText::new(title)
            .font(crate::ui::theme::typography::body())
            .strong()
            .color(crate::ui::theme::palette::text_primary()),
    );
    ui.add_space(10.0);
    add_contents(ui);
    ui.add_space(18.0);
}

fn preference_labeled_row<R>(
    ui: &mut egui::Ui,
    label: &str,
    info: Option<&str>,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    const LABEL_WIDTH: f32 = 140.0;
    const INFO_GAP: f32 = 6.0;
    let row_height = ui.spacing().interact_size.y.max(28.0);
    let label_color = crate::ui::theme::palette::text_primary().gamma_multiply(0.9);

    ui.horizontal(|ui| {
        ui.set_min_height(row_height);
        ui.spacing_mut().item_spacing.x = 0.0;
        let (label_rect, _) =
            ui.allocate_exact_size(egui::vec2(LABEL_WIDTH, row_height), egui::Sense::hover());
        let label_galley = ui.painter().layout_no_wrap(
            label.to_owned(),
            crate::ui::theme::typography::body(),
            label_color,
        );
        ui.painter().text(
            label_rect.left_center(),
            egui::Align2::LEFT_CENTER,
            label,
            crate::ui::theme::typography::body(),
            label_color,
        );
        if let Some(text) = info {
            let icon_center_x = (label_rect.left() + label_galley.size().x + INFO_GAP + 7.0)
                .min(label_rect.right() - 7.0);
            draw_info_icon_at(
                ui,
                egui::Rect::from_center_size(
                    egui::pos2(icon_center_x, label_rect.center().y),
                    egui::vec2(18.0, 18.0),
                ),
                text,
            );
        }
        ui.add_space(14.0);
        add_contents(ui)
    })
    .inner
}

fn preference_toggle_row(
    ui: &mut egui::Ui,
    current: &mut bool,
    label: &str,
    info: Option<&str>,
) -> egui::Response {
    ui.horizontal(|ui| {
        ui.set_min_height(ui.spacing().interact_size.y.max(28.0));
        ui.spacing_mut().item_spacing.x = 0.0;
        let response = crate::ui::theme::checkmark_toggle(ui, current, label);
        if let Some(text) = info {
            ui.add_space(6.0);
            draw_info_icon(ui, text);
        }
        response
    })
    .inner
}

fn preference_status_line(ui: &mut egui::Ui, text: &str, info: Option<&str>) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(text)
                .font(crate::ui::theme::typography::body_small())
                .color(crate::ui::theme::palette::text_muted()),
        );
        if let Some(text) = info {
            draw_info_icon(ui, text);
        }
    });
}

fn draw_shortcut_row(app: &mut MondrianApp, ui: &mut egui::Ui, action: ShortcutAction) {
    const ACTION_WIDTH: f32 = 146.0;
    const VALUE_WIDTH: f32 = 156.0;
    const ACTION_GAP: f32 = 18.0;
    const BUTTON_GAP: f32 = 10.0;
    const BUTTON_GROUP_WIDTH: f32 = 130.0;

    let row_height = 34.0;
    let row_width = ui.available_width().max(0.0);
    let (row_rect, _) =
        ui.allocate_exact_size(egui::vec2(row_width, row_height), egui::Sense::hover());

    ui.scope_builder(egui::UiBuilder::new().max_rect(row_rect), |ui| {
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let (action_rect, _) =
                ui.allocate_exact_size(egui::vec2(ACTION_WIDTH, row_height), egui::Sense::hover());
            ui.painter().text(
                action_rect.left_center(),
                egui::Align2::LEFT_CENTER,
                shortcut_action_title(action),
                crate::ui::theme::typography::body(),
                crate::ui::theme::palette::text_primary(),
            );
            ui.add_space(ACTION_GAP);

            let capturing = app.capturing_shortcut == Some(action);
            let shortcut_label = shortcut_action_label(app, action);
            ui.allocate_ui_with_layout(
                egui::vec2(VALUE_WIDTH, row_height),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    draw_shortcut_value_chip(
                        ui,
                        VALUE_WIDTH,
                        if capturing {
                            "按下快捷键"
                        } else {
                            shortcut_label.as_str()
                        },
                        capturing,
                    );
                },
            );

            let spacer = (ui.available_width() - BUTTON_GROUP_WIDTH).max(0.0);
            ui.add_space(spacer);
            ui.allocate_ui_with_layout(
                egui::vec2(BUTTON_GROUP_WIDTH, row_height),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = BUTTON_GAP;
                    ui.spacing_mut().button_padding.x = 12.0;
                    ui.spacing_mut().button_padding.y = 6.0;
                    if capturing {
                        if ui.add_sized([60.0, 26.0], egui::Button::new("取消")).clicked() {
                            app.capturing_shortcut = None;
                        }
                    } else {
                        if ui.add_sized([60.0, 26.0], egui::Button::new("设置")).clicked() {
                            app.capturing_shortcut = Some(action);
                        }
                        if ui.add_sized([60.0, 26.0], egui::Button::new("清空")).clicked() {
                            *shortcut_binding_mut(app, action) = None;
                        }
                    }
                },
            );
        });
    });
}

fn draw_shortcut_value_chip(ui: &mut egui::Ui, width: f32, text: &str, highlighted: bool) {
    let fill = if highlighted {
        crate::ui::theme::palette::interaction_highlight().gamma_multiply(0.14)
    } else {
        crate::ui::theme::palette::bg_surface_raised()
    };
    let stroke = if highlighted {
        egui::Stroke::new(1.0, crate::ui::theme::palette::interaction_highlight())
    } else {
        egui::Stroke::new(1.0, crate::ui::theme::palette::border_subtle())
    };

    egui::Frame::new()
        .fill(fill)
        .stroke(stroke)
        .corner_radius(corner_radius(crate::ui::theme::tokens::button_rounding()))
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.add_sized(
                [
                    width - 20.0,
                    crate::ui::theme::tokens::list_row_content_height(),
                ],
                egui::Label::new(
                    egui::RichText::new(text)
                        .font(crate::ui::theme::typography::mono_small())
                        .color(if highlighted {
                            crate::ui::theme::palette::interaction_highlight()
                        } else {
                            crate::ui::theme::palette::text_primary()
                        }),
                )
                .truncate()
                .halign(egui::Align::LEFT),
            );
        });
}

fn draw_info_icon(ui: &mut egui::Ui, text: &str) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
    paint_info_icon(ui, rect, response, text);
}

fn draw_info_icon_at(ui: &mut egui::Ui, rect: egui::Rect, text: &str) {
    let response = ui.interact(
        rect,
        ui.id().with(("info", rect.min.x as i32, rect.min.y as i32)),
        egui::Sense::hover(),
    );
    paint_info_icon(ui, rect, response, text);
}

fn paint_info_icon(ui: &mut egui::Ui, rect: egui::Rect, response: egui::Response, text: &str) {
    let icon_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(14.0, 14.0));
    crate::ui::theme::draw_icon(
        ui.painter(),
        icon_rect,
        crate::ui::theme::UiIcon::Info,
        crate::ui::theme::palette::text_muted(),
    );
    response.on_hover_text(text);
}

pub(super) fn persist_preferences_if_needed(app: &mut MondrianApp) {
    let snapshot = capture_preferences(app);
    if app.last_saved_preferences.as_ref() == Some(&snapshot) {
        return;
    }

    if let Some(parent) = app.app_config_path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            report_persist_error(app, &format!("创建配置目录失败: {err}"));
            return;
        }
    }

    let serialized = match serde_json::to_vec_pretty(&snapshot) {
        Ok(v) => v,
        Err(err) => {
            report_persist_error(app, &format!("序列化配置失败: {err}"));
            return;
        }
    };

    let tmp = app.app_config_path.with_extension("json.tmp");
    if let Err(err) = fs::write(&tmp, &serialized) {
        report_persist_error(app, &format!("写入配置临时文件失败: {err}"));
        return;
    }
    if let Err(err) = fs::rename(&tmp, &app.app_config_path) {
        let _ = fs::remove_file(&tmp);
        report_persist_error(app, &format!("保存配置失败: {err}"));
        return;
    }

    app.last_saved_preferences = Some(snapshot);
    app.persist_error_reported = false;
}

pub(super) fn report_persist_error(app: &mut MondrianApp, message: &str) {
    if !app.persist_error_reported {
        tracing::error!("{message}");
        app.state.set_status_hint(message.to_string(), true);
        app.persist_error_reported = true;
    }
}
