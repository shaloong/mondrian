use super::*;

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
    app.show_ai = preferences.show_ai;
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
    app.show_video_metrics = preferences.show_video_metrics;
    app.show_audio_metrics = preferences.show_audio_metrics;
    app.show_preview_perf_metrics = preferences.show_preview_perf_metrics;
    app.viewer_panel.apply_preferences(&preferences.viewer);
    app.last_saved_preferences = Some(preferences);
}

pub(super) fn capture_preferences(app: &MondrianApp) -> AppPreferences {
    AppPreferences {
        version: 1,
        show_library: app.show_library,
        show_ai: app.show_ai,
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
        show_video_metrics: app.show_video_metrics,
        show_audio_metrics: app.show_audio_metrics,
        show_preview_perf_metrics: app.show_preview_perf_metrics,
        viewer: app.viewer_panel.preferences_snapshot(),
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
        Some(ShortcutAction::CloseProject) => app.state.close_project(),
        Some(ShortcutAction::QuitApp) => {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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
    let mut open = app.show_preferences_dialog;
    egui::Window::new("首选项")
        .open(&mut open)
        .default_size([760.0, 460.0])
        .resizable(true)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.set_min_width(180.0);
                ui.vertical(|ui| {
                    ui.selectable_value(&mut app.preferences_tab, PreferencesTab::General, "常规");
                    ui.selectable_value(&mut app.preferences_tab, PreferencesTab::Media, "媒体");
                    ui.selectable_value(
                        &mut app.preferences_tab,
                        PreferencesTab::Shortcuts,
                        "快捷键",
                    );
                    if cfg!(debug_assertions) {
                        ui.selectable_value(
                            &mut app.preferences_tab,
                            PreferencesTab::Developer,
                            "开发",
                        );
                    }
                });

                ui.separator();

                ui.vertical(|ui| {
                    ui.set_min_width(520.0);
                    match app.preferences_tab {
                        PreferencesTab::General => {
                            ui.heading("常规");
                            ui.label("软件级配置入口，后续会持续扩展。\n当前可在“媒体”和“快捷键”分页进行设置。\n");
                        }
                        PreferencesTab::Media => {
                            ui.heading("媒体");
                            ui.add_space(8.0);
                            ui.label("预览解码后端");
                            ui.horizontal(|ui| {
                                let mut backend = app.viewer_panel.preview_decode_backend();
                                ui.radio_value(
                                    &mut backend,
                                    mondrian_media::PreviewDecodeBackend::Auto,
                                    "自动",
                                );
                                ui.radio_value(
                                    &mut backend,
                                    mondrian_media::PreviewDecodeBackend::Software,
                                    "软件",
                                );
                                ui.radio_value(
                                    &mut backend,
                                    mondrian_media::PreviewDecodeBackend::GpuAssist,
                                    "GPU辅助",
                                );
                                if backend != app.viewer_panel.preview_decode_backend() {
                                    app.viewer_panel.set_preview_decode_backend(backend);
                                }
                            });
                            ui.label("自动=默认稳定策略；GPU辅助为实验能力，失败会自动回退到软件解码，部分机器可能更慢。");
                            ui.add_space(12.0);

                            let _ = crate::ui::theme::checkmark_toggle(
                                ui,
                                &mut app.state.auto_proxy_enabled,
                                "自动代理媒体文件",
                            );
                            ui.label("导入视频素材时自动启用代理模式并后台生成代理。");
                            ui.add_space(12.0);

                            let _ = crate::ui::theme::checkmark_toggle(
                                ui,
                                &mut app.media_cache_auto_cleanup,
                                "自动清理媒体缓存",
                            );

                            match app.viewer_panel.media_cache_usage_stats() {
                                Ok(usage) => {
                                    ui.label(format!(
                                        "当前缓存：{} 个文件，约 {:.1} MB",
                                        usage.file_count,
                                        usage.total_bytes as f64 / 1024.0 / 1024.0
                                    ));
                                }
                                Err(err) => {
                                    ui.label(format!("当前缓存统计失败：{}", err));
                                }
                            }

                            ui.add_enabled_ui(app.media_cache_auto_cleanup, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label("缓存上限（GB）");
                                    ui.add(
                                        egui::DragValue::new(&mut app.media_cache_max_size_gb)
                                            .range(1..=4096)
                                            .speed(1),
                                    );
                                });
                                ui.horizontal(|ui| {
                                    ui.label("文件保留天数");
                                    ui.add(
                                        egui::DragValue::new(&mut app.media_cache_max_age_days)
                                            .range(1..=3650)
                                            .speed(1),
                                    );
                                });
                            });

                            ui.horizontal(|ui| {
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
                                            app.state
                                                .set_status_hint(format!("清空媒体缓存失败：{err}"), true);
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
                                            app.last_cache_maintenance_at =
                                                Some(std::time::Instant::now());
                                        }
                                        Err(err) => {
                                            app.state.set_status_hint(
                                                format!("自动清理媒体缓存失败：{err}"),
                                                true,
                                            );
                                        }
                                    }
                                }
                            });
                        }
                        PreferencesTab::Shortcuts => {
                            ui.heading("快捷键");
                            ui.add_space(8.0);

                            let actions = [
                                ShortcutAction::ImportMedia,
                                ShortcutAction::OpenProject,
                                ShortcutAction::SaveProject,
                                ShortcutAction::SaveProjectAs,
                                ShortcutAction::CloseProject,
                                ShortcutAction::QuitApp,
                            ];

                            for action in actions {
                                ui.horizontal(|ui| {
                                    ui.label(shortcut_action_title(action));
                                    ui.add_space(16.0);
                                    ui.monospace(shortcut_action_label(app, action));
                                    ui.add_space(12.0);

                                    if app.capturing_shortcut == Some(action) {
                                        ui.colored_label(
                                            crate::ui::theme::palette::interaction_highlight(),
                                            "按下快捷键（Esc 取消）...",
                                        );
                                        if ui.button("取消").clicked() {
                                            app.capturing_shortcut = None;
                                        }
                                    } else {
                                        if ui.button("设置").clicked() {
                                            app.capturing_shortcut = Some(action);
                                        }
                                        if ui.button("清空").clicked() {
                                            *shortcut_binding_mut(app, action) = None;
                                        }
                                    }
                                });
                            }

                            ui.add_space(12.0);
                            if ui.button("恢复默认快捷键").clicked() {
                                app.shortcuts = ShortcutPreferences::default();
                                app.capturing_shortcut = None;
                                app.state.set_status_hint("已恢复默认快捷键", false);
                            }
                        }
                        PreferencesTab::Developer => {
                            ui.heading("开发");
                            ui.add_space(8.0);

                            let _ = crate::ui::theme::checkmark_toggle(
                                ui,
                                &mut app.show_dev_metrics,
                                "显示开发指标",
                            );

                            ui.add_enabled_ui(app.show_dev_metrics, |ui| {
                                let _ = crate::ui::theme::checkmark_toggle(
                                    ui,
                                    &mut app.show_video_metrics,
                                    "显示视频指标",
                                );
                                let _ = crate::ui::theme::checkmark_toggle(
                                    ui,
                                    &mut app.show_preview_perf_metrics,
                                    "显示 Perf1s 分段指标",
                                );
                                let _ = crate::ui::theme::checkmark_toggle(
                                    ui,
                                    &mut app.show_audio_metrics,
                                    "显示音频指标",
                                );
                            });

                            ui.add_space(8.0);
                            ui.label("提示：Perf1s 可帮助判断瓶颈在解码、合成还是上传。建议仅在调优时开启。");

                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(8.0);
                            ui.label("开发缓冲控制");

                            let mut prefetch_enabled = app.viewer_panel.prefetch_enabled();
                            if crate::ui::theme::checkmark_toggle(
                                ui,
                                &mut prefetch_enabled,
                                "启用预取缓冲",
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
                            if crate::ui::theme::checkmark_toggle(
                                ui,
                                &mut layer_cache_enabled,
                                "启用图层缓存",
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

                            if ui.button("清空图层缓存").clicked() {
                                app.viewer_panel.clear_layer_cache();
                                app.state.set_status_hint("图层缓存已清空", false);
                            }
                        }
                    }
                });
            });
        });

    app.show_preferences_dialog = open;
    if !open {
        app.capturing_shortcut = None;
    }
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
