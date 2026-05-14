use super::*;

impl MondrianApp {
    pub(super) fn sync_startup_viewport_mode(&mut self, ctx: &egui::Context, startup_mode: bool) {
        if self.startup_viewport_mode == startup_mode {
            return;
        }
        self.startup_viewport_mode = startup_mode;

        if startup_mode {
            let size = crate::ui::theme::tokens::startup_viewport_size();
            ctx.send_viewport_cmd(egui::ViewportCommand::Transparent(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Resizable(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::EnableButtons {
                close: false,
                minimized: false,
                maximize: false,
            });
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(size));
            ctx.send_viewport_cmd(egui::ViewportCommand::MaxInnerSize(size));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
            return;
        }

        ctx.send_viewport_cmd(egui::ViewportCommand::Transparent(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Resizable(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::EnableButtons {
            close: true,
            minimized: true,
            maximize: true,
        });
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::vec2(
            1024.0, 600.0,
        )));
        ctx.send_viewport_cmd(egui::ViewportCommand::MaxInnerSize(egui::vec2(
            4096.0, 2160.0,
        )));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1600.0, 900.0)));
    }

    pub(super) fn bootstrap_recovery_age_label(saved_at_unix_ms: u64) -> String {
        let age_secs = (unix_now_ms().saturating_sub(saved_at_unix_ms)) / 1000;
        if age_secs < 60 {
            format!("{age_secs} 秒前")
        } else if age_secs < 3600 {
            format!("{} 分钟前", age_secs / 60)
        } else if age_secs < 86_400 {
            format!("{} 小时前", age_secs / 3600)
        } else {
            format!("{} 天前", age_secs / 86_400)
        }
    }

    pub(super) fn bootstrap_recent_project_meta(project_path: &Path) -> (String, String) {
        let metadata = fs::metadata(project_path).ok();

        let last_edited_label = metadata
            .as_ref()
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .map(Self::format_elapsed_label)
            .unwrap_or_else(|| "未知时间".to_string());

        let project_size_label = metadata
            .as_ref()
            .map(|meta| Self::format_file_size_label(meta.len()))
            .unwrap_or_else(|| "未知大小".to_string());

        (last_edited_label, project_size_label)
    }

    fn format_elapsed_label(elapsed: std::time::Duration) -> String {
        let secs = elapsed.as_secs();
        if secs < 60 {
            "刚刚".to_string()
        } else if secs < 3_600 {
            format!("{} 分钟前", secs / 60)
        } else if secs < 86_400 {
            format!("{} 小时前", secs / 3_600)
        } else {
            format!("{} 天前", secs / 86_400)
        }
    }

    fn format_file_size_label(bytes: u64) -> String {
        const KB: u64 = 1024;
        const MB: u64 = KB * 1024;
        const GB: u64 = MB * 1024;

        if bytes < KB {
            format!("{} B", bytes)
        } else if bytes < MB {
            format!("{:.1} KB", bytes as f64 / KB as f64)
        } else if bytes < GB {
            format!("{:.1} MB", bytes as f64 / MB as f64)
        } else {
            format!("{:.2} GB", bytes as f64 / GB as f64)
        }
    }

    pub(super) fn open_project_dialog(&mut self) {
        let picked = FileDialog::new()
            .add_filter("Mondrian Project", &[PROJECT_EXTENSION])
            .pick_file();

        let Some(path) = picked else {
            return;
        };

        match self.open_project_by_path(path) {
            Ok(()) => {
                self.state.set_status_hint("项目已打开", false);
            }
            Err(err) => {
                self.state.set_status_hint(format!("打开项目失败：{err}"), true);
                tracing::error!("打开项目失败: {err}");
            }
        }
    }

    pub(super) fn open_project_by_path(&mut self, project_file: PathBuf) -> anyhow::Result<()> {
        if !project_file.exists() {
            let msg = format!(
                "项目文件不存在，已从最近记录中移除：{}",
                project_file.display()
            );
            self.recent_projects.retain(|p| p != &project_file);
            return Err(anyhow::anyhow!(msg));
        }
        self.state.open_project_file(project_file.clone())?;
        self.finish_project_opened();
        self.record_recent_project(project_file);
        Ok(())
    }

    pub(super) fn recover_project_from_candidate(&mut self, index: usize) {
        let Some(candidate) = self.crash_recovery_candidates.get(index).cloned() else {
            return;
        };

        match self.state.open_project_from_autosave_snapshot(
            candidate.project_file.clone(),
            candidate.autosave_file.clone(),
        ) {
            Ok(()) => {
                self.finish_project_opened();
                self.record_recent_project(candidate.project_file.clone());
                self.state.set_status_hint(
                    format!("已从自动保存恢复：{}", candidate.project_file.display()),
                    false,
                );
            }
            Err(err) => {
                self.state.set_status_hint(format!("恢复自动保存失败：{err}"), true);
                tracing::error!("恢复自动保存失败: {err}");
                self.crash_recovery_candidates = discover_crash_recovery_candidates();
            }
        }
    }

    pub(super) fn finish_project_opened(&mut self) {
        self.show_effect_controls = true;
        self.show_effect_library = true;
        self.show_library = true;
        self.show_project_bootstrap_dialog = false;
        self.last_auto_save_at = None;
        self.auto_save_error_reported = false;
        self.crash_recovery_candidates = discover_crash_recovery_candidates();
    }

    pub(super) fn record_recent_project(&mut self, project_file: PathBuf) {
        self.recent_projects.retain(|existing| existing != &project_file);
        self.recent_projects.insert(0, project_file);
        self.recent_projects.truncate(12);
    }
}
