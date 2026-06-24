use super::*;
use super::{new_project, preferences};

#[allow(dead_code)]
pub(in crate::app) struct MondrianApp {
    pub(in crate::app::legacy_egui) state: AppState,

    // UI 面板
    pub(in crate::app::legacy_egui) timeline_panel: TimelinePanel,
    pub(in crate::app::legacy_egui) effect_library_panel: EffectLibraryPanel,
    pub(in crate::app::legacy_egui) effect_controls_panel: EffectControlsPanel,
    pub(in crate::app::legacy_egui) viewer_panel: ViewerPanel,
    pub(in crate::app::legacy_egui) library_panel: LibraryPanel,
    pub(in crate::app::legacy_egui) export_panel: ExportPanel,
    pub(in crate::app::legacy_egui) node_graph_panel:
        crate::egui_ui::node_graph_panel::NodeGraphPanel,

    // 面板可见性
    pub(in crate::app::legacy_egui) show_effect_controls: bool,
    pub(in crate::app::legacy_egui) show_node_graph: bool,
    pub(in crate::app::legacy_egui) show_effect_library: bool,
    pub(in crate::app::legacy_egui) show_library: bool,
    pub(in crate::app::legacy_egui) show_export: bool,
    pub(in crate::app::legacy_egui) show_sequence_settings: bool,
    pub(in crate::app::legacy_egui) show_dev_metrics: bool,
    pub(in crate::app::legacy_egui) show_preferences_dialog: bool,
    pub(in crate::app::legacy_egui) theme: crate::egui_ui::theme::Theme,
    pub(in crate::app::legacy_egui) preferences_tab: PreferencesTab,
    pub(in crate::app::legacy_egui) capturing_shortcut: Option<ShortcutAction>,
    pub(in crate::app::legacy_egui) show_new_project_dialog: bool,
    pub(in crate::app::legacy_egui) new_project_active_tab: NewProjectTab,
    pub(in crate::app::legacy_egui) show_project_bootstrap_dialog: bool,
    pub(in crate::app::legacy_egui) startup_viewport_mode: bool,
    pub(in crate::app::legacy_egui) pending_close_action: Option<PendingCloseAction>,
    pub(in crate::app::legacy_egui) allow_next_viewport_close: bool,
    pub(in crate::app::legacy_egui) new_project_draft: NewProjectDraft,
    pub(in crate::app::legacy_egui) sequence_presets: Vec<SequencePreset>,
    pub(in crate::app::legacy_egui) sequence_settings_sequence_id: Option<SequenceId>,
    pub(in crate::app::legacy_egui) sequence_settings_name_buffer: String,
    pub(in crate::app::legacy_egui) sequence_settings_draft: Option<SequenceSettings>,
    pub(in crate::app::legacy_egui) sequence_settings_preset_name: String,
    pub(in crate::app::legacy_egui) playback_last_tick: Option<std::time::Instant>,
    pub(in crate::app::legacy_egui) playback_subframe_accum: f64,
    pub(in crate::app::legacy_egui) playback_buffering_last_frame: bool,
    pub(in crate::app::legacy_egui) app_config_path: PathBuf,
    pub(in crate::app::legacy_egui) shortcuts: ShortcutPreferences,
    pub(in crate::app::legacy_egui) media_cache_auto_cleanup: bool,
    pub(in crate::app::legacy_egui) media_cache_max_size_gb: u32,
    pub(in crate::app::legacy_egui) media_cache_max_age_days: u32,
    pub(in crate::app::legacy_egui) auto_save_enabled: bool,
    pub(in crate::app::legacy_egui) auto_save_interval_secs: u32,
    pub(in crate::app::legacy_egui) auto_save_max_recovery_points: u32,
    pub(in crate::app::legacy_egui) auto_save_retention_days: u32,
    pub(in crate::app::legacy_egui) last_auto_save_at: Option<std::time::Instant>,
    pub(in crate::app::legacy_egui) auto_save_error_reported: bool,
    pub(in crate::app::legacy_egui) recent_projects: Vec<PathBuf>,
    pub(in crate::app::legacy_egui) crash_recovery_candidates: Vec<CrashRecoveryCandidate>,
    pub(in crate::app::legacy_egui) lut_library_filter: String,
    pub(in crate::app::legacy_egui) show_video_metrics: bool,
    pub(in crate::app::legacy_egui) show_audio_metrics: bool,
    pub(in crate::app::legacy_egui) timeline_panel_height: f32,
    pub(in crate::app::legacy_egui) timeline_resize_drag: Option<(f32, f32)>,
    pub(in crate::app::legacy_egui) last_saved_preferences: Option<AppPreferences>,
    pub(in crate::app::legacy_egui) persist_error_reported: bool,
    pub(in crate::app::legacy_egui) last_cache_maintenance_at: Option<std::time::Instant>,
    pub(in crate::app::legacy_egui) cache_maintenance_in_flight: bool,
    pub(in crate::app::legacy_egui) cache_maintenance_rx:
        Option<mpsc::Receiver<anyhow::Result<MediaCacheCleanupStats>>>,
    /// Whether GPU compute acceleration is available (set at startup).
    pub(in crate::app::legacy_egui) gpu_available: bool,
}

#[allow(dead_code)]
impl MondrianApp {
    pub(in crate::app::legacy_egui) const MENU_POPUP_MIN_WIDTH: f32 = 176.0;
    pub(in crate::app::legacy_egui) const MENU_POPUP_WIDTH: f32 = 196.0;

    pub(in crate::app) fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::egui_ui::fonts::configure_fonts(&cc.egui_ctx);
        egui_extras::install_image_loaders(&cc.egui_ctx);

        // Record surface format for GPU callback pipeline creation.
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            crate::egui_ui::viewer::gpu_texture::set_surface_format(rs.target_format);
        }

        let state = AppState::new();

        let mut app = Self {
            state,
            timeline_panel: TimelinePanel::default(),
            effect_library_panel: EffectLibraryPanel::default(),
            effect_controls_panel: EffectControlsPanel::default(),
            viewer_panel: ViewerPanel::default(),
            library_panel: LibraryPanel::default(),
            export_panel: ExportPanel::default(),
            node_graph_panel: crate::egui_ui::node_graph_panel::NodeGraphPanel::default(),
            show_effect_controls: true,
            show_node_graph: false,
            show_effect_library: true,
            show_library: true,
            show_export: false,
            show_sequence_settings: false,
            show_dev_metrics: false,
            show_preferences_dialog: false,
            theme: default_app_theme(),
            preferences_tab: PreferencesTab::default(),
            capturing_shortcut: None,
            show_new_project_dialog: false,
            new_project_active_tab: NewProjectTab::default(),
            show_project_bootstrap_dialog: true,
            startup_viewport_mode: false,
            pending_close_action: None,
            allow_next_viewport_close: false,
            new_project_draft: NewProjectDraft::default(),
            sequence_presets: builtin_sequence_presets(),
            sequence_settings_sequence_id: None,
            sequence_settings_name_buffer: String::new(),
            sequence_settings_draft: None,
            sequence_settings_preset_name: String::new(),
            playback_last_tick: None,
            playback_subframe_accum: 0.0,
            playback_buffering_last_frame: false,
            app_config_path: app_preferences_path(),
            shortcuts: ShortcutPreferences::default(),
            media_cache_auto_cleanup: default_media_cache_auto_cleanup(),
            media_cache_max_size_gb: default_media_cache_max_size_gb(),
            media_cache_max_age_days: default_media_cache_max_age_days(),
            auto_save_enabled: default_auto_save_enabled(),
            auto_save_interval_secs: default_auto_save_interval_secs(),
            auto_save_max_recovery_points: default_auto_save_max_recovery_points(),
            auto_save_retention_days: default_auto_save_retention_days(),
            last_auto_save_at: None,
            auto_save_error_reported: false,
            recent_projects: Vec::new(),
            crash_recovery_candidates: discover_crash_recovery_candidates(),
            lut_library_filter: String::new(),
            show_video_metrics: default_show_video_metrics(),
            show_audio_metrics: default_show_audio_metrics(),
            timeline_panel_height: default_timeline_panel_height(),
            timeline_resize_drag: None,
            last_saved_preferences: None,
            persist_error_reported: false,
            last_cache_maintenance_at: None,
            cache_maintenance_in_flight: false,
            cache_maintenance_rx: None,
            gpu_available: false,
        };

        // Initialize GPU using eframe's wgpu device (shared, no separate adapter).
        app.try_init_gpu_with_device(cc.wgpu_render_state.as_ref());
        mondrian_renderer::profile::init_profiling();

        app.load_app_preferences();
        crate::egui_ui::theme::apply_theme(&cc.egui_ctx, app.theme);
        cc.egui_ctx
            .send_viewport_cmd(egui::ViewportCommand::SetTheme(app.theme.to_system_theme()));

        // 在首帧前就切到启动窗口模式，避免 clear_color 首帧走到不透明分支。
        let startup_mode = !app.state.has_open_project();
        app.sync_startup_viewport_mode(&cc.egui_ctx, startup_mode);

        app
    }
}

impl eframe::App for MondrianApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let update_started_at = std::time::Instant::now();

        self.advance_playback_clock();
        if ui_diag_enabled() {
            log_ui_stage_slow("advance_playback_clock", update_started_at.elapsed());
        }

        let is_playing = self.state.is_playing();

        let theme_started_at = std::time::Instant::now();
        crate::egui_ui::theme::apply_theme(&ctx, self.theme);
        ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(
            self.theme.to_system_theme(),
        ));
        if ui_diag_enabled() {
            log_ui_stage_slow("apply_theme", theme_started_at.elapsed());
        }

        let shortcuts_started_at = std::time::Instant::now();
        self.process_global_shortcuts(&ctx);
        if ui_diag_enabled() {
            log_ui_stage_slow("process_global_shortcuts", shortcuts_started_at.elapsed());
        }

        self.handle_viewport_close_requested(&ctx);

        let cache_maintenance_started_at = std::time::Instant::now();
        self.run_cache_maintenance_if_needed();
        if ui_diag_enabled() {
            log_ui_stage_slow(
                "run_cache_maintenance_if_needed",
                cache_maintenance_started_at.elapsed(),
            );
        }

        let auto_save_started_at = std::time::Instant::now();
        self.run_project_autosave_if_needed();
        if ui_diag_enabled() {
            log_ui_stage_slow(
                "run_project_autosave_if_needed",
                auto_save_started_at.elapsed(),
            );
        }

        if self.state.dragging_asset().is_some() {
            ctx.output_mut(|o| o.cursor_icon = egui::CursorIcon::Default);
        }

        // 播放中持续请求重绘（60 fps 上限）
        if is_playing {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }

        if !self.state.has_open_project() {
            self.show_project_bootstrap_dialog = true;
        }

        if self.show_new_project_dialog {
            new_project::draw_new_project_window(self, &ctx);
        }

        self.sync_startup_viewport_mode(&ctx, !self.state.has_open_project());

        if self.show_project_bootstrap_dialog {
            let recovery_items: Vec<BootstrapRecoveryItem> = self
                .crash_recovery_candidates
                .iter()
                .map(|candidate| BootstrapRecoveryItem {
                    project_name: candidate
                        .project_file
                        .file_name()
                        .and_then(|v| v.to_str())
                        .unwrap_or("未知项目")
                        .to_string(),
                    autosave_path: candidate.autosave_file.display().to_string(),
                    age_label: Self::bootstrap_recovery_age_label(candidate.saved_at_unix_ms),
                    total_snapshots: candidate.total_snapshots,
                })
                .collect();

            let recent_items = self
                .recent_projects
                .iter()
                .filter(|p| p.exists())
                .map(|project_path| {
                    let (last_edited_label, project_size_label) =
                        Self::bootstrap_recent_project_meta(project_path.as_path());
                    BootstrapRecentProjectItem {
                        project_name: project_path
                            .file_stem()
                            .or_else(|| project_path.file_name())
                            .and_then(|v| v.to_str())
                            .unwrap_or("未知项目")
                            .to_string(),
                        project_path: project_path.display().to_string(),
                        last_edited_label,
                        project_size_label,
                    }
                })
                .collect::<Vec<_>>();

            if let Some(action) = crate::egui_ui::startup::show_project_bootstrap_window(
                ui,
                PROJECT_EXTENSION,
                &recent_items,
                &recovery_items,
            ) {
                match action {
                    BootstrapAction::OpenProject => self.open_project_dialog(),
                    BootstrapAction::OpenRecent(path) => match self.open_project_by_path(path) {
                        Ok(()) => self.state.set_status_hint("项目已打开", false),
                        Err(err) => {
                            self.state.set_status_hint(format!("打开项目失败：{err}"), true);
                            tracing::error!("打开项目失败: {err}");
                        }
                    },
                    BootstrapAction::NewProject => self.show_new_project_dialog = true,
                    BootstrapAction::Quit => self.request_quit_app(&ctx),
                    BootstrapAction::Recover(idx) => self.recover_project_from_candidate(idx),
                }
            }

            if !self.state.has_open_project() {
                if self.show_preferences_dialog {
                    self.capture_shortcut_input(&ctx);
                    self.draw_preferences_window(&ctx);
                }

                self.draw_pending_close_action_dialog(&ctx);

                let persist_started_at = std::time::Instant::now();
                self.persist_preferences_if_needed();

                if ui_diag_enabled() {
                    log_ui_stage_slow(
                        "persist_preferences_if_needed",
                        persist_started_at.elapsed(),
                    );
                    log_ui_stage_slow("update_total", update_started_at.elapsed());
                }

                return;
            }
        }

        self.sync_startup_viewport_mode(&ctx, false);

        // ── 顶部菜单栏 ──
        let top_menu_started_at = std::time::Instant::now();
        egui::Panel::top("top_menu")
            .frame(
                egui::Frame::new()
                    .fill(crate::egui_ui::theme::palette::bg_surface())
                    .stroke(egui::Stroke::new(
                        1.0,
                        crate::egui_ui::theme::palette::panel_divider_strong(),
                    ))
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show_inside(ui, |ui| {
                self.draw_menu_bar(ui);
            });
        if ui_diag_enabled() {
            log_ui_stage_slow("top_menu", top_menu_started_at.elapsed());
        }

        // ── 底部状态栏 ──
        let status_bar_started_at = std::time::Instant::now();
        egui::Panel::bottom("status_bar")
            .exact_size(28.0)
            .frame(
                egui::Frame::new()
                    .fill(crate::egui_ui::theme::palette::bg_surface())
                    .stroke(egui::Stroke::new(
                        1.0,
                        crate::egui_ui::theme::palette::panel_divider_strong(),
                    ))
                    .inner_margin(egui::Margin::symmetric(10, 0)),
            )
            .show_inside(ui, |ui| {
                self.draw_status_bar(ui);
            });
        if ui_diag_enabled() {
            log_ui_stage_slow("status_bar", status_bar_started_at.elapsed());
        }

        // ── 底部：时间线（全宽） ──
        let timeline_started_at = std::time::Instant::now();
        egui::Panel::bottom("timeline_panel")
            .exact_size(self.timeline_panel_height)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(crate::egui_ui::theme::palette::bg_base())
                    .stroke(egui::Stroke::new(
                        1.0,
                        crate::egui_ui::theme::palette::panel_divider_strong(),
                    ))
                    .inner_margin(egui::Margin { left: 12, right: 12, top: 0, bottom: 8 }),
            )
            .show_inside(ui, |ui| {
                let (_resize_rect, resize_response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 8.0),
                    egui::Sense::click_and_drag(),
                );
                resize_response.clone().on_hover_cursor(egui::CursorIcon::ResizeVertical);
                if resize_response.drag_started() {
                    if let Some(pointer) = resize_response.interact_pointer_pos() {
                        self.timeline_resize_drag = Some((pointer.y, self.timeline_panel_height));
                    }
                }
                if let Some((start_y, start_height)) = self.timeline_resize_drag {
                    if ctx.input(|i| i.pointer.primary_down()) {
                        if let Some(pointer) = ctx.input(|i| i.pointer.interact_pos()) {
                            let delta = start_y - pointer.y;
                            self.timeline_panel_height = (start_height + delta).clamp(160.0, 640.0);
                        }
                    } else {
                        self.timeline_resize_drag = None;
                    }
                }

                ui.add_space(4.0);
                self.timeline_panel.show(ui, &mut self.state);
            });
        if ui_diag_enabled() {
            log_ui_stage_slow("timeline_panel", timeline_started_at.elapsed());
        }

        // ── 左侧：素材库 ──
        if self.show_library {
            let library_started_at = std::time::Instant::now();
            let viewer_min = 540.0;
            let right_reserved = crate::egui_ui::theme::tokens::inspector_panel_min_width() + 208.0; // effect_library min
            let library_max = (ctx.content_rect().width() - viewer_min - right_reserved).max(220.0);
            egui::Panel::left("library_panel")
                .default_size(296.0)
                .min_size(220.0)
                .max_size(library_max)
                .resizable(true)
                .frame(
                    egui::Frame::new()
                        .fill(crate::egui_ui::theme::palette::bg_base())
                        .stroke(egui::Stroke::NONE)
                        .inner_margin(egui::Margin::symmetric(12, 8)),
                )
                .show_inside(ui, |ui| {
                    self.library_panel.show(ui, &mut self.state);
                });
            if ui_diag_enabled() {
                log_ui_stage_slow("library_panel", library_started_at.elapsed());
            }
        }

        let selected_clip_ref = self.timeline_panel.selected_clip_ref();
        if let Some(selection) = selected_clip_ref {
            if !ctx.egui_wants_keyboard_input()
                && ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::C))
            {
                match self.state.copy_selected_animation_keyframes(selection) {
                    Ok(true) => self.state.set_status_hint("已复制关键帧", false),
                    Ok(false) => {}
                    Err(err) => {
                        self.state.set_status_hint(format!("复制关键帧失败：{err}"), true);
                    }
                }
            }

            if !ctx.egui_wants_keyboard_input()
                && ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::V))
            {
                let destination_time =
                    self.state.current_time_code().map(timecode_to_ticks).unwrap_or(0);
                match self.state.paste_animation_keyframes(selection, destination_time) {
                    Ok(true) => self.state.set_status_hint("已粘贴关键帧", false),
                    Ok(false) => {}
                    Err(err) => {
                        self.state.set_status_hint(format!("粘贴关键帧失败：{err}"), true);
                    }
                }
            }
        }
        if self.show_effect_controls {
            let effect_controls_started_at = std::time::Instant::now();
            let viewer_min = 540.0;
            let left_reserved = 220.0; // library min
            let other_right = if self.show_effect_library { 208.0 } else { 0.0 };
            let ec_max = (ctx.content_rect().width() - viewer_min - left_reserved - other_right)
                .max(crate::egui_ui::theme::tokens::inspector_panel_min_width());
            egui::Panel::right("effect_controls_panel")
                .default_size(crate::egui_ui::theme::tokens::inspector_panel_width())
                .min_size(crate::egui_ui::theme::tokens::inspector_panel_min_width())
                .max_size(ec_max)
                .resizable(true)
                .frame(
                    egui::Frame::new()
                        .fill(crate::egui_ui::theme::palette::bg_base())
                        .stroke(egui::Stroke::NONE)
                        .inner_margin(egui::Margin::symmetric(12, 8)),
                )
                .show_inside(ui, |ui| {
                    self.effect_controls_panel.show(ui, &mut self.state, selected_clip_ref);
                });
            if ui_diag_enabled() {
                log_ui_stage_slow(
                    "effect_controls_panel",
                    effect_controls_started_at.elapsed(),
                );
            }
        }
        if self.show_effect_library {
            let effect_library_started_at = std::time::Instant::now();
            let viewer_min = 540.0;
            let left_reserved = 220.0; // library min
            let other_right = if self.show_effect_controls {
                crate::egui_ui::theme::tokens::inspector_panel_min_width()
            } else {
                0.0
            };
            let el_max =
                (ctx.content_rect().width() - viewer_min - left_reserved - other_right).max(208.0);
            egui::Panel::right("effect_library_panel")
                .default_size(252.0)
                .min_size(208.0)
                .max_size(el_max)
                .resizable(true)
                .frame(
                    egui::Frame::new()
                        .fill(crate::egui_ui::theme::palette::bg_base())
                        .stroke(egui::Stroke::NONE)
                        .inner_margin(egui::Margin::symmetric(12, 8)),
                )
                .show_inside(ui, |ui| {
                    self.effect_library_panel.show(ui, &mut self.state, selected_clip_ref);
                });
            if ui_diag_enabled() {
                log_ui_stage_slow("effect_library_panel", effect_library_started_at.elapsed());
            }
        }

        // ── 中央：预览窗口 ──
        let viewer_started_at = std::time::Instant::now();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(crate::egui_ui::theme::palette::bg_base())
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show_inside(ui, |ui| {
                self.viewer_panel.show(
                    ui,
                    &mut self.state,
                    self.show_dev_metrics,
                    self.show_video_metrics,
                    self.show_audio_metrics,
                    true,
                );
            });
        // Sync canvas selection to timeline.
        if let Some(selection) = self.state.primary_selected_clip() {
            self.timeline_panel.apply_canvas_selection(Some(selection));
        }
        if ui_diag_enabled() {
            log_ui_stage_slow("viewer_panel", viewer_started_at.elapsed());
        }

        // ── 节点图面板 ──
        if self.show_node_graph {
            let mut open = self.show_node_graph;
            egui::Window::new("节点图编辑器")
                .open(&mut open)
                .default_size([800.0, 600.0])
                .show(&ctx, |ui| {
                    self.node_graph_panel.show(ui);
                });
            self.show_node_graph = open;
        }

        // ── 导出弹窗 ──
        if self.show_export {
            let export_started_at = std::time::Instant::now();
            let mut open = self.show_export;
            egui::Window::new("导出")
                .open(&mut open)
                .default_size([560.0, 680.0])
                .frame(crate::egui_ui::theme::dialog_frame())
                .show(&ctx, |ui| {
                    self.export_panel.show(ui, &mut self.state);
                });
            self.show_export = open;
            if ui_diag_enabled() {
                log_ui_stage_slow("export_panel", export_started_at.elapsed());
            }
        }

        if self.show_sequence_settings {
            let mut open = self.show_sequence_settings;
            egui::Window::new("序列设置")
                .open(&mut open)
                .default_size([520.0, 520.0])
                .frame(crate::egui_ui::theme::dialog_frame())
                .show(&ctx, |ui| {
                    self.draw_sequence_settings_window(ui);
                });
            self.show_sequence_settings = open;
        }

        if self.show_preferences_dialog {
            self.capture_shortcut_input(&ctx);
            self.draw_preferences_window(&ctx);
        }

        self.draw_pending_close_action_dialog(&ctx);

        let persist_started_at = std::time::Instant::now();
        self.persist_preferences_if_needed();

        if ui_diag_enabled() {
            log_ui_stage_slow(
                "persist_preferences_if_needed",
                persist_started_at.elapsed(),
            );
            log_ui_stage_slow("update_total", update_started_at.elapsed());
        }
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        if self.startup_viewport_mode {
            return [0.0, 0.0, 0.0, 0.0];
        }

        visuals.window_fill().to_normalized_gamma_f32()
    }
}

//  MondrianApp — 私有 UI helpers
// ─────────────────────────────────────────────

#[allow(dead_code)]
impl MondrianApp {
    /// Try to initialize GPU using eframe's shared wgpu device.
    /// Falls back to standalone device creation if eframe device is unavailable.
    pub(in crate::app::legacy_egui) fn try_init_gpu_with_device(
        &mut self,
        render_state: Option<&egui_wgpu::RenderState>,
    ) {
        let gpu = if let Some(rs) = render_state {
            tracing::info!("Using eframe wgpu device for GPU compositor & compute");
            mondrian_renderer::GpuContext::from_device_queue(
                std::sync::Arc::new(rs.device.clone()),
                std::sync::Arc::new(rs.queue.clone()),
                rs.adapter.clone(),
            )
        } else {
            tracing::info!("eframe wgpu device not available, creating standalone GPU context");
            let handle = match tokio::runtime::Handle::try_current() {
                Ok(h) => h,
                Err(_) => {
                    self.gpu_available = false;
                    return;
                }
            };
            match handle.block_on(mondrian_renderer::GpuContext::new()) {
                Ok(gpu) => gpu,
                Err(e) => {
                    tracing::warn!("Failed to create standalone GPU context: {e}");
                    self.gpu_available = false;
                    return;
                }
            }
        };

        // Initialize the GPU compositor (layer composition).
        crate::egui_ui::viewer::gpu_composite::init_gpu_compositor(gpu.clone());

        // Initialize GPU compute backend for effect processing.
        let backend = mondrian_renderer::GpuBackend::from_context(gpu);
        mondrian_effects::set_global_gpu_executor(Some(backend));
        self.gpu_available = true;
        tracing::info!("GPU 加速已启用");
        self.state.event_bus.publish(mondrian_core::AppEvent::GpuStatusChanged {
            available: true,
            reason: "GPU 加速已启用".into(),
        });
    }

    /// Legacy path kept for test compatibility.
    /// Try to initialize GPU acceleration for effect processing.
    /// On failure, GPU is silently unavailable — effects fall back to CPU.
    #[allow(dead_code)]
    pub(in crate::app::legacy_egui) fn try_init_gpu(&mut self) {
        let handle = match tokio::runtime::Handle::try_current() {
            Ok(h) => h,
            Err(_) => {
                tracing::info!("GPU 初始化跳过：无 Tokio 运行时");
                self.gpu_available = false;
                return;
            }
        };
        match handle.block_on(mondrian_renderer::GpuBackend::new()) {
            Some(backend) => {
                mondrian_effects::set_global_gpu_executor(Some(backend));
                self.gpu_available = true;
                tracing::info!("GPU 加速已启用");
                self.state.event_bus.publish(mondrian_core::AppEvent::GpuStatusChanged {
                    available: true,
                    reason: "GPU 加速已启用".into(),
                });
            }
            None => {
                self.gpu_available = false;
                tracing::info!("GPU 不可用，使用 CPU 渲染");
                self.state.event_bus.publish(mondrian_core::AppEvent::GpuStatusChanged {
                    available: false,
                    reason: "未检测到兼容 GPU，使用 CPU 渲染".into(),
                });
            }
        }
    }

    pub(in crate::app::legacy_egui) fn load_app_preferences(&mut self) {
        preferences::load_app_preferences(self);
    }

    pub(in crate::app::legacy_egui) fn process_global_shortcuts(&mut self, ctx: &egui::Context) {
        preferences::process_global_shortcuts(self, ctx);
    }

    pub(in crate::app::legacy_egui) fn run_cache_maintenance_if_needed(&mut self) {
        preferences::run_cache_maintenance_if_needed(self);
    }

    pub(in crate::app::legacy_egui) fn run_project_autosave_if_needed(&mut self) {
        preferences::run_project_autosave_if_needed(self);
    }

    pub(in crate::app::legacy_egui) fn trigger_import_media(&mut self) {
        preferences::trigger_import_media(self);
    }

    pub(in crate::app::legacy_egui) fn save_project_as_dialog(&mut self) {
        preferences::save_project_as_dialog(self);
    }

    pub(in crate::app::legacy_egui) fn draw_sequence_settings_window(&mut self, ui: &mut egui::Ui) {
        let Some(sequence) = self.state.sequence.as_ref() else {
            ui.label("当前无序列");
            return;
        };

        let sequence_id = sequence.id;
        let sequence_name = sequence.name.clone();
        let sequence_settings = sequence.settings.clone();
        if self.sequence_settings_sequence_id != Some(sequence_id) {
            self.sequence_settings_sequence_id = Some(sequence_id);
            self.sequence_settings_name_buffer = sequence_name.clone();
            self.sequence_settings_draft = Some(sequence_settings.clone());
            self.sequence_settings_preset_name = format!("{} 预设", sequence_name);
        }

        let mut settings = self.sequence_settings_draft.clone().unwrap_or(sequence_settings);
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);

        ui.horizontal(|ui| {
            ui.label("序列预设");
            egui::ComboBox::from_id_salt("sequence_settings_preset")
                .selected_text("选择预设")
                .show_ui(ui, |ui| {
                    for preset in self.sequence_presets.clone() {
                        if ui.button(&preset.name).clicked() {
                            settings = preset.settings.clone();
                            ui.close();
                        }
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.sequence_settings_preset_name)
                    .desired_width(150.0)
                    .hint_text("预设名称"),
            );
            if ui.button("保存当前为预设").clicked() {
                let preset_name = self.sequence_settings_preset_name.trim().to_string();
                match SequencePreset::new(preset_name, settings.clone()) {
                    Ok(preset) => {
                        self.sequence_presets.retain(|item| item.name != preset.name);
                        self.sequence_presets.push(preset);
                        self.state.set_status_hint("序列预设已保存", false);
                    }
                    Err(err) => {
                        self.state.set_status_hint(format!("保存序列预设失败：{err}"), true);
                    }
                }
            }
        });
        ui.add_space(8.0);

        egui::Grid::new("sequence_settings_grid")
            .num_columns(2)
            .spacing([14.0, 8.0])
            .show(ui, |ui| {
                ui.label("序列名称");
                ui.add(
                    egui::TextEdit::singleline(&mut self.sequence_settings_name_buffer)
                        .desired_width(220.0),
                );
                ui.end_row();

                ui.label("编辑模式");
                egui::ComboBox::from_id_salt("sequence_editing_mode")
                    .selected_text(editing_mode_label(settings.editing_mode))
                    .show_ui(ui, |ui| {
                        let mut selected_mode = settings.editing_mode;
                        for mode in [
                            EditingMode::Custom,
                            EditingMode::Dslr1080p,
                            EditingMode::Dslr720p,
                            EditingMode::Avchd1080p,
                            EditingMode::DigitalCinema4k,
                            EditingMode::SocialVertical1080p,
                        ] {
                            ui.selectable_value(&mut selected_mode, mode, editing_mode_label(mode));
                        }
                        if selected_mode != settings.editing_mode {
                            settings.apply_editing_mode_preset(selected_mode);
                        }
                    });
                ui.end_row();

                ui.label("时基");
                egui::ComboBox::from_id_salt("sequence_frame_rate")
                    .selected_text(frame_rate_label(settings.frame_rate))
                    .show_ui(ui, |ui| {
                        for frame_rate in Rational::SEQUENCE_FRAME_RATES {
                            ui.selectable_value(
                                &mut settings.frame_rate,
                                frame_rate,
                                frame_rate_label(frame_rate),
                            );
                        }
                    });
                ui.end_row();

                ui.label("帧大小");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut settings.resolution.width)
                            .range(SequenceSettings::MIN_WIDTH..=SequenceSettings::MAX_WIDTH)
                            .speed(8)
                            .suffix(" px"),
                    );
                    ui.label("x");
                    ui.add(
                        egui::DragValue::new(&mut settings.resolution.height)
                            .range(SequenceSettings::MIN_HEIGHT..=SequenceSettings::MAX_HEIGHT)
                            .speed(8)
                            .suffix(" px"),
                    );
                });
                ui.end_row();

                ui.label("像素长宽比");
                egui::ComboBox::from_id_salt("sequence_pixel_aspect_ratio")
                    .selected_text(pixel_aspect_ratio_label(settings.pixel_aspect_ratio))
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
                                &mut settings.pixel_aspect_ratio,
                                par,
                                pixel_aspect_ratio_label(par),
                            );
                        }
                    });
                ui.end_row();

                ui.label("场");
                egui::ComboBox::from_id_salt("sequence_field_order")
                    .selected_text(field_order_label(settings.field_order))
                    .show_ui(ui, |ui| {
                        for field_order in [
                            FieldOrder::Progressive,
                            FieldOrder::UpperFirst,
                            FieldOrder::LowerFirst,
                        ] {
                            ui.selectable_value(
                                &mut settings.field_order,
                                field_order,
                                field_order_label(field_order),
                            );
                        }
                    });
                ui.end_row();

                ui.label("显示格式");
                egui::ComboBox::from_id_salt("sequence_video_display_format")
                    .selected_text(video_display_format_label(settings.video_display_format))
                    .show_ui(ui, |ui| {
                        for display_format in [
                            VideoDisplayFormat::Timecode2997DropFrame,
                            VideoDisplayFormat::Timecode2997NonDropFrame,
                            VideoDisplayFormat::FeetAndFrames16mm,
                            VideoDisplayFormat::FeetAndFrames35mm,
                            VideoDisplayFormat::Frames,
                        ] {
                            ui.selectable_value(
                                &mut settings.video_display_format,
                                display_format,
                                video_display_format_label(display_format),
                            );
                        }
                    });
                ui.end_row();

                ui.label("起始时间码帧");
                ui.add(
                    egui::DragValue::new(&mut settings.start_timecode_frame)
                        .range(0..=24 * 60 * 60 * 240)
                        .speed(1)
                        .suffix(" f"),
                );
                ui.end_row();

                ui.label("工作色彩空间");
                egui::ComboBox::from_id_salt("sequence_color_space")
                    .selected_text(color_space_label(settings.color_space))
                    .show_ui(ui, |ui| {
                        for color_space in color_space_options() {
                            ui.selectable_value(
                                &mut settings.color_space,
                                color_space,
                                color_space_label(color_space),
                            );
                        }
                    });
                ui.end_row();

                ui.label("自动色调映射");
                ui.checkbox(&mut settings.auto_tone_map_media, "");
                ui.end_row();

                ui.label("色彩工作流");
                egui::ComboBox::from_id_salt("sequence_color_workflow")
                    .selected_text(color_workflow_label(settings.color_management.workflow))
                    .show_ui(ui, |ui| {
                        for workflow in [
                            ColorWorkflow::DisplayReferred,
                            ColorWorkflow::SceneReferred,
                            ColorWorkflow::Aces,
                        ] {
                            ui.selectable_value(
                                &mut settings.color_management.workflow,
                                workflow,
                                color_workflow_label(workflow),
                            );
                        }
                    });
                ui.end_row();

                ui.label("缺失色彩元数据");
                egui::ComboBox::from_id_salt("sequence_missing_color_metadata")
                    .selected_text(missing_color_metadata_policy_label(
                        settings.color_management.missing_metadata_policy,
                    ))
                    .show_ui(ui, |ui| {
                        for policy in [
                            MissingColorMetadataPolicy::AssumeRec709,
                            MissingColorMetadataPolicy::AssumeSequenceWorkingSpace,
                            MissingColorMetadataPolicy::RejectMedia,
                        ] {
                            ui.selectable_value(
                                &mut settings.color_management.missing_metadata_policy,
                                policy,
                                missing_color_metadata_policy_label(policy),
                            );
                        }
                    });
                ui.end_row();

                ui.label("嵌套序列色彩");
                egui::ComboBox::from_id_salt("sequence_nested_color_processing")
                    .selected_text(nested_color_processing_label(
                        settings.color_management.nested_processing,
                    ))
                    .show_ui(ui, |ui| {
                        for processing in [
                            NestedColorProcessing::PreserveChildWorkingSpace,
                            NestedColorProcessing::ForceParentWorkingSpace,
                            NestedColorProcessing::BakeChildOutputTransform,
                        ] {
                            ui.selectable_value(
                                &mut settings.color_management.nested_processing,
                                processing,
                                nested_color_processing_label(processing),
                            );
                        }
                    });
                ui.end_row();

                ui.label("输出色彩空间");
                egui::ComboBox::from_id_salt("sequence_output_color_space")
                    .selected_text(color_space_label(
                        settings.color_management.output_color_space,
                    ))
                    .show_ui(ui, |ui| {
                        for color_space in color_space_options() {
                            ui.selectable_value(
                                &mut settings.color_management.output_color_space,
                                color_space,
                                color_space_label(color_space),
                            );
                        }
                    });
                ui.end_row();

                ui.label("视频电平");
                egui::ComboBox::from_id_salt("sequence_video_range")
                    .selected_text(video_range_label(settings.color_management.video_range))
                    .show_ui(ui, |ui| {
                        for range in [VideoRange::Full, VideoRange::Legal] {
                            ui.selectable_value(
                                &mut settings.color_management.video_range,
                                range,
                                video_range_label(range),
                            );
                        }
                    });
                ui.end_row();

                ui.label("导出位深");
                egui::ComboBox::from_id_salt("sequence_export_bit_depth")
                    .selected_text(export_bit_depth_label(
                        settings.color_management.export_bit_depth,
                    ))
                    .show_ui(ui, |ui| {
                        for bit_depth in [
                            ExportBitDepth::Eight,
                            ExportBitDepth::Ten,
                            ExportBitDepth::SixteenFloat,
                        ] {
                            ui.selectable_value(
                                &mut settings.color_management.export_bit_depth,
                                bit_depth,
                                export_bit_depth_label(bit_depth),
                            );
                        }
                    });
                ui.end_row();

                ui.label("保留 HDR metadata");
                ui.checkbox(&mut settings.color_management.preserve_hdr_metadata, "");
                ui.end_row();

                ui.label("采样率");
                egui::ComboBox::from_id_salt("sequence_audio_sample_rate")
                    .selected_text(format!("{} Hz", settings.audio_sample_rate))
                    .show_ui(ui, |ui| {
                        for sample_rate in SequenceSettings::AUDIO_SAMPLE_RATES {
                            ui.selectable_value(
                                &mut settings.audio_sample_rate,
                                sample_rate,
                                format!("{sample_rate} Hz"),
                            );
                        }
                    });
                ui.end_row();

                ui.label("声道布局");
                egui::ComboBox::from_id_salt("sequence_audio_channel_layout")
                    .selected_text(audio_channel_layout_label(settings.audio_channel_layout))
                    .show_ui(ui, |ui| {
                        for layout in [
                            AudioChannelLayout::Mono,
                            AudioChannelLayout::Stereo,
                            AudioChannelLayout::Surround51,
                        ] {
                            if ui
                                .selectable_value(
                                    &mut settings.audio_channel_layout,
                                    layout,
                                    audio_channel_layout_label(layout),
                                )
                                .clicked()
                            {
                                settings.audio_channels = layout.channels();
                            }
                        }
                    });
                settings.audio_channels = settings.audio_channel_layout.channels();
                ui.end_row();

                ui.label("音频显示格式");
                egui::ComboBox::from_id_salt("sequence_audio_display_format")
                    .selected_text(audio_display_format_label(settings.audio_display_format))
                    .show_ui(ui, |ui| {
                        for display_format in [
                            AudioDisplayFormat::AudioSamples,
                            AudioDisplayFormat::Milliseconds,
                        ] {
                            ui.selectable_value(
                                &mut settings.audio_display_format,
                                display_format,
                                audio_display_format_label(display_format),
                            );
                        }
                    });
                ui.end_row();

                ui.label("预览格式");
                egui::ComboBox::from_id_salt("sequence_preview_format")
                    .selected_text(preview_render_format_label(settings.preview.format))
                    .show_ui(ui, |ui| {
                        for format in [
                            PreviewRenderFormat::IFrameOnly,
                            PreviewRenderFormat::ProResProxy,
                            PreviewRenderFormat::DnxHrLb,
                            PreviewRenderFormat::LosslessRgba,
                        ] {
                            ui.selectable_value(
                                &mut settings.preview.format,
                                format,
                                preview_render_format_label(format),
                            );
                        }
                    });
                ui.end_row();

                ui.label("预览分辨率");
                ui.add(
                    egui::Slider::new(&mut settings.preview.resolution_scale, 0.125..=1.0)
                        .text("")
                        .custom_formatter(|value, _| format!("{:.0}%", value * 100.0)),
                );
                ui.end_row();

                ui.label("预览缓存");
                ui.checkbox(&mut settings.preview.cache_enabled, "");
                ui.end_row();
            });

        ui.add_space(12.0);
        self.sequence_settings_draft = Some(settings.clone());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("应用").clicked() {
                let rename_result = self
                    .state
                    .rename_sequence(sequence_id, self.sequence_settings_name_buffer.clone());
                match rename_result
                    .and_then(|()| self.state.update_active_sequence_settings(settings))
                {
                    Ok(()) => {
                        self.state.set_status_hint("序列设置已更新", false);
                        self.show_sequence_settings = false;
                    }
                    Err(err) => {
                        self.state.set_status_hint(format!("更新序列设置失败：{err}"), true);
                    }
                }
            }
        });
    }

    pub(in crate::app::legacy_egui) fn capture_shortcut_input(&mut self, ctx: &egui::Context) {
        preferences::capture_shortcut_input(self, ctx);
    }

    pub(in crate::app::legacy_egui) fn draw_preferences_window(&mut self, ctx: &egui::Context) {
        preferences::draw_preferences_window(self, ctx);
    }

    pub(in crate::app::legacy_egui) fn persist_preferences_if_needed(&mut self) {
        preferences::persist_preferences_if_needed(self);
    }

    pub(in crate::app::legacy_egui) fn handle_viewport_close_requested(
        &mut self,
        ctx: &egui::Context,
    ) {
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if !close_requested {
            return;
        }

        if self.allow_next_viewport_close {
            self.allow_next_viewport_close = false;
            return;
        }

        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        self.request_quit_app(ctx);
    }

    pub(in crate::app::legacy_egui) fn close_project_and_refresh_state(&mut self) {
        self.state.close_project();
        self.last_auto_save_at = None;
        self.auto_save_error_reported = false;
        self.crash_recovery_candidates = discover_crash_recovery_candidates();
    }

    pub(in crate::app::legacy_egui) fn has_unsaved_project_changes(&self) -> bool {
        self.state.has_unsaved_project_changes()
    }

    pub(in crate::app::legacy_egui) fn request_close_project(&mut self) {
        if !self.state.has_open_project() {
            return;
        }

        if self.has_unsaved_project_changes() {
            self.pending_close_action = Some(PendingCloseAction::CloseProject);
        } else {
            self.close_project_and_refresh_state();
        }
    }

    pub(in crate::app::legacy_egui) fn request_quit_app(&mut self, ctx: &egui::Context) {
        if self.state.has_open_project() {
            if self.has_unsaved_project_changes() {
                self.pending_close_action = Some(PendingCloseAction::QuitApp);
                return;
            }

            self.close_project_and_refresh_state();
        }

        self.allow_next_viewport_close = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    pub(in crate::app::legacy_egui) fn execute_pending_close_action(
        &mut self,
        ctx: &egui::Context,
    ) {
        let Some(action) = self.pending_close_action.take() else {
            return;
        };

        match action {
            PendingCloseAction::CloseProject => {
                self.close_project_and_refresh_state();
            }
            PendingCloseAction::QuitApp => {
                self.close_project_and_refresh_state();
                self.allow_next_viewport_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    pub(in crate::app::legacy_egui) fn draw_pending_close_action_dialog(
        &mut self,
        ctx: &egui::Context,
    ) {
        let Some(action) = self.pending_close_action else {
            return;
        };

        let mut keep_open = true;
        egui::Window::new("关闭前保存项目")
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .default_size([460.0, 160.0])
            .open(&mut keep_open)
            .frame(crate::egui_ui::theme::dialog_frame())
            .show(ctx, |ui| {
                let action_text = match action {
                    PendingCloseAction::CloseProject => "关闭项目",
                    PendingCloseAction::QuitApp => "退出应用",
                };
                ui.label(format!("正在{action_text}，是否先保存当前项目？"));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("保存并继续").clicked() {
                        if let Err(err) = self.state.save_project() {
                            tracing::error!("关闭前保存失败: {err}");
                            self.state.set_status_hint(format!("保存项目失败：{err}"), true);
                        } else {
                            self.execute_pending_close_action(ctx);
                        }
                    }

                    if ui.button("不保存").clicked() {
                        self.execute_pending_close_action(ctx);
                    }

                    if ui.button("取消").clicked() {
                        self.pending_close_action = None;
                    }
                });
            });

        if !keep_open {
            self.pending_close_action = None;
        }
    }

    /// 播放自然到达终点时调用：将播放头停在 `end_frame`，并标记自然到达标志。
    /// 与 `state.seek()` 不同，此方法**不会清除** `playback_reached_end`，
    /// 因此下次 `play()` 将从 in_point 重新开始（经典循环行为）。
    pub(in crate::app::legacy_egui) fn end_playback_at(&mut self, end_frame: i64) {
        self.state.playback_reached_end = true;
        self.state.playback_buffering = false;
        self.state.playback = PlaybackState::Paused { timecode_frames: end_frame };
        self.state.sync_audio_clock_to_frame(end_frame);
        self.state
            .reset_audio_render_pipeline(self.state.audio_clock.now_seconds().max(0.0));
        if let Some(output) = &self.state.audio_output {
            output.set_muted(false);
            output.clear();
        }
        self.playback_last_tick = None;
        self.playback_subframe_accum = 0.0;
        self.playback_buffering_last_frame = false;
    }

    pub(in crate::app::legacy_egui) fn advance_playback_clock(&mut self) {
        if !self.state.is_playing() {
            if let Some(output) = &self.state.audio_output {
                output.set_muted(false);
            }
            self.playback_last_tick = None;
            self.playback_subframe_accum = 0.0;
            self.playback_buffering_last_frame = false;
            return;
        }

        if self.state.is_playback_buffering() {
            if !self.playback_buffering_last_frame {
                self.state.sync_audio_clock_to_frame(self.state.current_frame());
                self.state
                    .reset_audio_render_pipeline(self.state.audio_clock.now_seconds().max(0.0));
                if let Some(output) = &self.state.audio_output {
                    output.clear();
                }
            }
            if let Some(output) = &self.state.audio_output {
                output.set_muted(true);
            }
            self.state.pump_audio_output();

            self.playback_last_tick = None;
            self.playback_subframe_accum = 0.0;
            self.playback_buffering_last_frame = true;
            return;
        }

        if self.playback_buffering_last_frame {
            if let Some(output) = &self.state.audio_output {
                output.set_muted(false);
            }
            self.playback_last_tick = None;
            self.playback_subframe_accum = 0.0;
        }
        self.playback_buffering_last_frame = false;
        if let Some(output) = &self.state.audio_output {
            output.set_muted(false);
        }

        self.state.pump_audio_output();

        let now = std::time::Instant::now();
        let previous = self.playback_last_tick.replace(now).unwrap_or(now);
        let elapsed_secs = now.saturating_duration_since(previous).as_secs_f64();
        if elapsed_secs <= 0.0 {
            return;
        }

        let mut fps = self
            .state
            .sequence
            .as_ref()
            .map(|seq| seq.settings.frame_rate.to_f64())
            .unwrap_or(25.0)
            .max(1.0);

        let nominal_fps = fps;
        fps *= self.state.update_av_sync().clamp(0.97, 1.03);

        let frame_budget = elapsed_secs * fps + self.playback_subframe_accum;
        let mut advance_frames = frame_budget.floor() as i64;
        self.playback_subframe_accum = frame_budget - advance_frames as f64;

        let drift_secs = self.state.av_drift_ms / 1000.0;
        let frame_duration_secs = 1.0 / nominal_fps.max(1.0);
        let sync_threshold_secs = (frame_duration_secs * 0.5).clamp(
            self.state.audio_sync.max_soft_drift.as_secs_f64(),
            self.state.audio_sync.max_hard_drift.as_secs_f64(),
        );
        let hard_drift_secs = self.state.audio_sync.max_hard_drift.as_secs_f64();
        let no_sync_threshold_secs = self.state.audio_sync.no_sync_threshold.as_secs_f64();

        if drift_secs.abs() < no_sync_threshold_secs {
            if drift_secs > hard_drift_secs {
                self.playback_subframe_accum = 0.0;
                return;
            }

            if drift_secs < -sync_threshold_secs {
                let late_secs = (-drift_secs - sync_threshold_secs).max(0.0);
                let catch_up_frames = (late_secs * nominal_fps).ceil() as i64;
                let catch_up_frames = catch_up_frames.clamp(1, 8);
                advance_frames = (advance_frames + catch_up_frames).max(1);
            }
        }

        if advance_frames <= 0 {
            return;
        }

        let current = self.state.current_frame();
        // 入/出点不影响播放逻辑，仅影响导出。
        let playback_end = self.state.last_content_frame().max(0);

        if current >= playback_end {
            // 播放头已在/超过终点：标记「自然到达终点」，暂停于终点帧
            self.end_playback_at(playback_end);
            return;
        }

        let next = current + advance_frames;
        if next >= playback_end {
            // 本帧推进后越界：停在终点并设置自然到达标志
            self.end_playback_at(playback_end);
        } else {
            self.state.set_playback_frame_running(next.max(0));
        }
    }
}

#[allow(dead_code)]
fn app_preferences_path() -> PathBuf {
    app_data_dir().join("app_preferences.json")
}
