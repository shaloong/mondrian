use crate::app::AppState;
use crate::ui::theme::{self, palette, tokens};
use egui::Ui;
use mondrian_export::{
    preset::{ExportConfig, ExportPreset, VideoCodecConfig},
    queue::RenderJob,
};
use rfd::FileDialog;
use std::path::PathBuf;

/// 导出弹窗面板
#[derive(Default)]
pub struct ExportPanel {
    /// 当前选中的预设
    selected_preset_idx: usize,
    /// 自定义输出路径
    output_path: String,
    /// 状态消息
    status_msg: Option<(String, bool)>, // (消息, 是否错误)
}

impl ExportPanel {
    pub fn show(&mut self, ui: &mut Ui, state: &mut AppState) {
        ui.vertical(|ui| {
            ui.heading("导出视频");
            ui.separator();

            // ── 预设选择 ──────────────────────
            ui.label("导出预设:");
            let presets = builtin_presets();
            let combo_id = ui.make_persistent_id("export_preset");
            let combo_open =
                ui.memory(|m| m.is_popup_open(combo_id) || m.is_popup_open(combo_id.with("popup")));

            theme::with_minimal_dropdown(ui, combo_open, |ui| {
                egui::ComboBox::from_id_salt("export_preset")
                    .selected_text(
                        egui::RichText::new(&presets[self.selected_preset_idx].0).color(
                            if combo_open {
                                palette::text_primary()
                            } else {
                                palette::text_muted()
                            },
                        ),
                    )
                    .show_ui(ui, |ui| {
                        for (i, (name, _)) in presets.iter().enumerate() {
                            theme::checkmark_selectable_value(
                                ui,
                                &mut self.selected_preset_idx,
                                i,
                                name,
                            );
                        }
                    });
            });

            let preset = &presets[self.selected_preset_idx].1;

            // 预设详情只读展示
            ui.separator();
            egui::Grid::new("preset_details")
                .num_columns(2)
                .spacing(tokens::export_grid_spacing())
                .show(ui, |ui| {
                    let vc = &preset.video;
                    let resolution_text = preset
                        .resolution
                        .as_ref()
                        .map(|r| format!("{}×{}", r.width, r.height))
                        .unwrap_or_else(|| "跟随序列".to_owned());

                    let (codec_name, bitrate_text) = match vc {
                        VideoCodecConfig::H264 { bitrate_kbps, .. } => (
                            "H.264",
                            bitrate_kbps
                                .map(|v| format!("{v} kbps"))
                                .unwrap_or_else(|| "自动".to_owned()),
                        ),
                        VideoCodecConfig::H265 { bitrate_kbps, .. } => (
                            "H.265",
                            bitrate_kbps
                                .map(|v| format!("{v} kbps"))
                                .unwrap_or_else(|| "自动".to_owned()),
                        ),
                        VideoCodecConfig::Av1 { .. } => ("AV1", "自动".to_owned()),
                        VideoCodecConfig::ProRes { .. } => ("ProRes", "N/A".to_owned()),
                        VideoCodecConfig::Gif { .. } => ("GIF", "N/A".to_owned()),
                    };

                    ui.label("分辨率:");
                    ui.label(resolution_text);
                    ui.end_row();

                    ui.label("视频编码:");
                    ui.label(codec_name);
                    ui.end_row();

                    ui.label("视频码率:");
                    ui.label(bitrate_text);
                    ui.end_row();

                    ui.label("容器格式:");
                    ui.label(format!("{:?}", preset.container));
                    ui.end_row();
                });

            ui.separator();

            // ── 输入源（首版：自动选择时间线首个可用视频素材） ──
            let export_input = state.default_export_input_path();
            ui.label("输入源:");
            match &export_input {
                Some(path) => {
                    ui.monospace(path.display().to_string());
                }
                None => {
                    ui.horizontal(|ui| {
                        let _ = theme::icon(ui, theme::UiIcon::Warning, palette::status_warning());
                        ui.colored_label(
                            palette::status_warning(),
                            "未找到可导出的视频素材（请先将视频放入时间线）",
                        );
                    });
                }
            }

            ui.separator();

            // ── 输出路径 ──────────────────────
            ui.label("输出文件:");
            ui.horizontal(|ui| {
                ui.text_edit_singleline(&mut self.output_path);
                if ui.button("浏览…").clicked() {
                    let default_name = default_output_filename(preset);
                    if let Some(path) = FileDialog::new().set_file_name(&default_name).save_file() {
                        self.output_path = path.display().to_string();
                    }
                }
            });

            if self.output_path.is_empty() {
                ui.horizontal(|ui| {
                    let _ = theme::icon(ui, theme::UiIcon::Warning, palette::status_warning());
                    ui.colored_label(palette::status_warning(), "请指定输出路径");
                });
            }

            ui.separator();

            // ── 渲染队列状态 ──────────────────
            let pending = state.render_queue.list_jobs().len();
            ui.label(format!("队列中: {pending} 个任务"));

            // ── 操作按钮 ──────────────────────
            ui.separator();
            ui.horizontal(|ui| {
                let can_export = !self.output_path.is_empty()
                    && state.sequence.is_some()
                    && export_input.is_some();
                if ui.add_enabled(can_export, egui::Button::new("加入导出队列")).clicked() {
                    if let Some(input_path) = export_input.clone() {
                        self.enqueue(state, preset.clone(), input_path);
                    } else {
                        self.status_msg = Some(("导出失败：未找到可用输入源".to_owned(), true));
                    }
                }
            });

            // ── 状态消息 ──────────────────────
            if let Some((msg, is_err)) = &self.status_msg {
                let color = if *is_err {
                    palette::status_error()
                } else {
                    palette::status_success()
                };
                ui.colored_label(color, msg);
            }
        });
    }

    fn enqueue(&mut self, state: &mut AppState, preset: ExportPreset, input_path: PathBuf) {
        let path = PathBuf::from(&self.output_path);
        let config = ExportConfig {
            preset,
            input_path,
            output_path: path,
            in_point: None,
            out_point: None,
        };
        let job = RenderJob::new(config);
        state.render_queue.enqueue(job);
        self.status_msg = Some(("已加入导出队列".to_owned(), false));
        tracing::info!("导出任务已加入队列: {}", self.output_path);
    }
}

/// 内置预设列表（名称 + ExportPreset）
fn builtin_presets() -> Vec<(String, ExportPreset)> {
    vec![
        (
            "YouTube 1080p H.264".to_owned(),
            ExportPreset::youtube_1080p(),
        ),
        (
            "TikTok 竖屏 9:16".to_owned(),
            ExportPreset::tiktok_vertical(),
        ),
        ("代理文件 720p".to_owned(), ExportPreset::proxy_720p()),
    ]
}

fn default_output_filename(preset: &ExportPreset) -> String {
    let ext = match preset.container {
        mondrian_export::preset::Container::Mp4 => "mp4",
        mondrian_export::preset::Container::Mov => "mov",
        mondrian_export::preset::Container::Mkv => "mkv",
        mondrian_export::preset::Container::Gif => "gif",
        mondrian_export::preset::Container::Mxf => "mxf",
        mondrian_export::preset::Container::Webm => "webm",
    };
    format!("mondrian-export.{}", ext)
}
