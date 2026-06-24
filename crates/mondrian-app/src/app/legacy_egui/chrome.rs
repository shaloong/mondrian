use super::*;

impl MondrianApp {
    pub(in crate::app) fn draw_menu_bar(&mut self, ui: &mut egui::Ui) {
        ui.ctx().global_style_mut(|style| {
            style.spacing.menu_width = Self::MENU_POPUP_WIDTH;
        });
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("文件", |ui| {
                ui.set_min_width(Self::MENU_POPUP_MIN_WIDTH);

                if Self::menu_action(ui, "新建项目...", None).clicked() {
                    self.show_new_project_dialog = true;
                    ui.close();
                }
                if Self::menu_action(
                    ui,
                    "打开项目...",
                    Some(self.shortcuts.open_project_label().as_str()),
                )
                .clicked()
                {
                    self.open_project_dialog();
                    ui.close();
                }
                if Self::menu_action(
                    ui,
                    "保存",
                    Some(self.shortcuts.save_project_label().as_str()),
                )
                .clicked()
                {
                    if let Err(err) = self.state.save_project() {
                        tracing::error!("保存项目失败: {err}");
                        self.state.set_status_hint(format!("保存项目失败：{err}"), true);
                    } else {
                        self.state.set_status_hint("项目已保存", false);
                    }
                    ui.close();
                }
                if Self::menu_action(
                    ui,
                    "另存为...",
                    Some(self.shortcuts.save_project_as_label().as_str()),
                )
                .clicked()
                {
                    self.save_project_as_dialog();
                    ui.close();
                }
                if Self::menu_action(
                    ui,
                    "关闭项目",
                    Some(self.shortcuts.close_project_label().as_str()),
                )
                .clicked()
                {
                    self.request_close_project();
                    ui.close();
                }
                ui.separator();
                if Self::menu_action(
                    ui,
                    "导入媒体",
                    Some(self.shortcuts.import_media_label().as_str()),
                )
                .clicked()
                {
                    self.trigger_import_media();
                    ui.close();
                }
                ui.separator();
                if Self::menu_action(ui, "退出", Some(self.shortcuts.quit_app_label().as_str()))
                    .clicked()
                {
                    self.request_quit_app(ui.ctx());
                }
            });

            ui.menu_button("编辑", |ui| {
                ui.set_min_width(Self::MENU_POPUP_MIN_WIDTH);
                let can_undo = self.state.cmd_history.can_undo();
                let can_redo = self.state.cmd_history.can_redo();

                if Self::menu_action_enabled(ui, "撤销", Some("Ctrl+Z"), can_undo).clicked() {
                    if let Err(err) = self.state.undo_timeline() {
                        self.state.set_status_hint(format!("撤销失败：{err}"), true);
                    }
                    ui.close();
                }
                if Self::menu_action_enabled(ui, "重做", Some("Ctrl+Shift+Z"), can_redo).clicked()
                {
                    if let Err(err) = self.state.redo_timeline() {
                        self.state.set_status_hint(format!("重做失败：{err}"), true);
                    }
                    ui.close();
                }

                ui.separator();
                if Self::menu_action(ui, "首选项...", None).clicked() {
                    self.show_preferences_dialog = true;
                    ui.close();
                }
            });

            ui.menu_button("视图", |ui| {
                ui.set_min_width(Self::MENU_POPUP_MIN_WIDTH);
                let _ = crate::egui_ui::theme::checkmark_menu_toggle(
                    ui,
                    &mut self.show_effect_controls,
                    "属性面板",
                );
                let _ = crate::egui_ui::theme::checkmark_menu_toggle(
                    ui,
                    &mut self.show_effect_library,
                    "特效库",
                );
                let _ = crate::egui_ui::theme::checkmark_menu_toggle(
                    ui,
                    &mut self.show_library,
                    "素材库",
                );
                if cfg!(debug_assertions) {
                    let _ = crate::egui_ui::theme::checkmark_menu_toggle(
                        ui,
                        &mut self.show_dev_metrics,
                        "开发指标",
                    );
                }
            });

            ui.menu_button("序列", |ui| {
                ui.set_min_width(Self::MENU_POPUP_MIN_WIDTH);
                if Self::menu_action(ui, "新建序列", None).clicked() {
                    let next = self.state.sequences.len() + 1;
                    self.state.new_sequence(&format!("序列 {next}"));
                    ui.close();
                }
                if Self::menu_action_enabled(
                    ui,
                    "返回父序列",
                    None,
                    !self.state.sequence_navigation_stack.is_empty(),
                )
                .clicked()
                {
                    if let Err(err) = self.state.return_to_parent_sequence() {
                        self.state.set_status_hint(format!("返回父序列失败：{err}"), true);
                    }
                    ui.close();
                }
                if let Some(active_id) = self.state.active_sequence_id {
                    if Self::menu_action(ui, "设为默认序列", None).clicked() {
                        if let Err(err) = self.state.set_default_sequence(active_id) {
                            self.state.set_status_hint(format!("设置默认序列失败：{err}"), true);
                        } else {
                            self.state.set_status_hint("已设置默认序列", false);
                        }
                        ui.close();
                    }
                }
                if Self::menu_action_enabled(ui, "序列设置...", None, self.state.sequence.is_some())
                    .clicked()
                {
                    self.show_sequence_settings = true;
                    ui.close();
                }
                ui.separator();
                for sequence in self.state.export_sequences_snapshot() {
                    let is_active =
                        self.state.active_sequence_id.is_some_and(|id| id == sequence.id);
                    let is_default =
                        self.state.default_sequence_id.is_some_and(|id| id == sequence.id);
                    let label = if is_default {
                        format!("{}  默认", sequence.name)
                    } else {
                        sequence.name.clone()
                    };
                    let response =
                        crate::egui_ui::theme::checkmark_menu_action(ui, is_active, label);
                    response.context_menu(|ui| {
                        if Self::menu_action(ui, "序列设置...", None).clicked() {
                            if let Err(err) = self.state.switch_active_sequence(sequence.id) {
                                self.state.set_status_hint(format!("切换序列失败：{err}"), true);
                            } else {
                                self.show_sequence_settings = true;
                            }
                            ui.close();
                        }
                        if Self::menu_action(ui, "设为默认序列", None).clicked() {
                            if let Err(err) = self.state.set_default_sequence(sequence.id) {
                                self.state
                                    .set_status_hint(format!("设置默认序列失败：{err}"), true);
                            } else {
                                self.state.set_status_hint("已设置默认序列", false);
                            }
                            ui.close();
                        }
                        if Self::menu_action(ui, "复制序列", None).clicked() {
                            let name = format!("{} 副本", sequence.name);
                            match self.state.duplicate_sequence(sequence.id, name) {
                                Ok(_) => self.state.set_status_hint("已复制序列", false),
                                Err(err) => {
                                    self.state.set_status_hint(format!("复制序列失败：{err}"), true)
                                }
                            }
                            ui.close();
                        }
                        if Self::menu_action_enabled(
                            ui,
                            "删除序列",
                            None,
                            self.state.export_sequences_snapshot().len() > 1,
                        )
                        .clicked()
                        {
                            match self.state.delete_sequence(sequence.id) {
                                Ok(()) => self.state.set_status_hint("已删除序列", false),
                                Err(err) => {
                                    self.state.set_status_hint(format!("删除序列失败：{err}"), true)
                                }
                            }
                            ui.close();
                        }
                    });
                    if response.clicked() {
                        if let Err(err) = self.state.switch_active_sequence(sequence.id) {
                            self.state.set_status_hint(format!("切换序列失败：{err}"), true);
                        } else {
                            self.state.set_status_hint("已切换序列", false);
                        }
                        ui.close();
                    }
                }
            });

            ui.menu_button("导出", |ui| {
                ui.set_min_width(Self::MENU_POPUP_MIN_WIDTH);
                if Self::menu_action(ui, "导出视频…", None).clicked() {
                    self.show_export = true;
                    ui.close();
                }
            });

            ui.menu_button("帮助", |ui| {
                ui.set_min_width(Self::MENU_POPUP_MIN_WIDTH);
                let _ = Self::menu_action(ui, "关于Mondrian", None);
            });
        });
    }

    fn menu_action(ui: &mut egui::Ui, label: &str, shortcut: Option<&str>) -> egui::Response {
        Self::menu_action_enabled(ui, label, shortcut, true)
    }

    fn menu_action_enabled(
        ui: &mut egui::Ui,
        label: &str,
        shortcut: Option<&str>,
        enabled: bool,
    ) -> egui::Response {
        let label_width = ui
            .painter()
            .layout_no_wrap(
                label.to_owned(),
                crate::egui_ui::theme::typography::body_small(),
                crate::egui_ui::theme::palette::text_primary(),
            )
            .size()
            .x;
        let shortcut_width = shortcut
            .map(|shortcut| {
                ui.painter()
                    .layout_no_wrap(
                        shortcut.to_owned(),
                        crate::egui_ui::theme::typography::body_small(),
                        crate::egui_ui::theme::palette::text_muted(),
                    )
                    .size()
                    .x
            })
            .unwrap_or(0.0);
        let content_width = 10.0
            + label_width
            + if shortcut.is_some() {
                28.0 + shortcut_width
            } else {
                0.0
            }
            + 10.0;
        let desired_size = egui::vec2(
            ui.spacing().menu_width.max(content_width),
            ui.spacing().interact_size.y,
        );
        let sense = if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        };
        let (rect, response) = ui.allocate_exact_size(desired_size, sense);

        if ui.is_rect_visible(rect) {
            let visuals = ui.visuals();
            let fill = if enabled && response.hovered() {
                visuals.widgets.hovered.weak_bg_fill
            } else {
                egui::Color32::TRANSPARENT
            };
            let rounding = visuals.menu_corner_radius;
            ui.painter().rect_filled(rect, rounding, fill);

            let label_color = if enabled {
                crate::egui_ui::theme::palette::text_primary()
            } else {
                crate::egui_ui::theme::palette::text_muted()
            };
            let shortcut_color = crate::egui_ui::theme::palette::text_muted().gamma_multiply(0.78);

            ui.painter().text(
                rect.left_center() + egui::vec2(10.0, 0.0),
                egui::Align2::LEFT_CENTER,
                label,
                crate::egui_ui::theme::typography::body_small(),
                label_color,
            );

            if let Some(shortcut) = shortcut {
                ui.painter().text(
                    rect.right_center() - egui::vec2(10.0, 0.0),
                    egui::Align2::RIGHT_CENTER,
                    shortcut,
                    crate::egui_ui::theme::typography::body_small(),
                    shortcut_color,
                );
            }
        }

        response
    }

    pub(in crate::app) fn draw_status_bar(&self, ui: &mut egui::Ui) {
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), ui.available_height()),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (status_text, is_error, is_busy) = self.status_bar_text();
                let status_color = if is_error {
                    crate::egui_ui::theme::palette::status_error()
                } else if is_busy {
                    crate::egui_ui::theme::palette::interaction_highlight()
                } else {
                    crate::egui_ui::theme::palette::text_muted()
                };

                let _ = crate::egui_ui::theme::icon(
                    ui,
                    crate::egui_ui::theme::UiIcon::Info,
                    crate::egui_ui::theme::palette::text_muted(),
                );
                ui.add(
                    egui::Label::new(egui::RichText::new(status_text).color(status_color))
                        .truncate(),
                );

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let project_name = self
                        .state
                        .sequence
                        .as_ref()
                        .map(|seq| seq.name.as_str())
                        .unwrap_or("未命名项目");
                    ui.label(
                        egui::RichText::new(project_name)
                            .color(crate::egui_ui::theme::palette::text_muted()),
                    );
                });
            },
        );
    }

    fn status_bar_text(&self) -> (String, bool, bool) {
        let jobs = self.state.render_queue.list_jobs();
        let active_jobs: Vec<_> = jobs
            .into_iter()
            .filter(|job| {
                matches!(
                    job.status,
                    JobStatus::Pending | JobStatus::Rendering { .. } | JobStatus::Encoding
                )
            })
            .collect();

        if let Some(job) = active_jobs.first() {
            let label = match &job.status {
                JobStatus::Pending => format!("导出队列处理中（{}）", active_jobs.len()),
                JobStatus::Rendering { frame, total_frames } => {
                    format!(
                        "正在导出帧 {}/{}（队列 {}）",
                        frame,
                        total_frames,
                        active_jobs.len()
                    )
                }
                JobStatus::Encoding => format!("正在编码（队列 {}）", active_jobs.len()),
                _ => "导出处理中".to_string(),
            };
            return (label, false, true);
        }

        if self.state.is_playing() && self.state.is_playback_buffering() {
            return ("预览缓冲中…".to_string(), false, true);
        }

        if let Some((message, is_error)) = &self.state.status_hint {
            return (message.clone(), *is_error, false);
        }

        ("就绪".to_string(), false, false)
    }
}
