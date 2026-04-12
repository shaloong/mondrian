use crate::app::AppState;
use crate::ui::theme::{self, palette, tokens};
use egui::Ui;
use mondrian_export::{
    preset::{ExportConfig, ExportInput, ExportPreset, TimelineExportInput, VideoCodecConfig},
    queue::RenderJob,
};
use rfd::FileDialog;
use std::collections::{HashMap, HashSet};
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

            // ── 输入源（时间线逐帧渲染） ──
            ui.label("输入源:");
            if let Some(sequence) = state.sequence.as_ref() {
                let video_clips =
                    sequence.video_tracks.iter().map(|t| t.clips.len()).sum::<usize>();
                let audio_clips =
                    sequence.audio_tracks.iter().map(|t| t.clips.len()).sum::<usize>();
                let frame_range = match state.out_point_frame() {
                    Some(out) => format!(
                        "{} - {}",
                        state.in_point_frame(),
                        out.max(state.in_point_frame())
                    ),
                    None => format!("{} - End", state.in_point_frame()),
                };
                ui.label(format!(
                    "时间线渲染（V{} / A{}，范围 {}）",
                    video_clips, audio_clips, frame_range
                ));
            } else {
                ui.horizontal(|ui| {
                    let _ = theme::icon(ui, theme::UiIcon::Warning, palette::status_warning());
                    ui.colored_label(palette::status_warning(), "当前无可导出的序列");
                });
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
                let can_export = !self.output_path.is_empty() && state.sequence.is_some();
                if ui.add_enabled(can_export, egui::Button::new("加入导出队列")).clicked() {
                    self.enqueue(state, preset.clone());
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

    fn enqueue(&mut self, state: &mut AppState, preset: ExportPreset) {
        let Some(sequence) = state.sequence.clone() else {
            self.status_msg = Some(("导出失败：当前无序列".to_owned(), true));
            return;
        };

        let asset_paths = match collect_timeline_asset_paths(state, &sequence) {
            Ok(paths) => paths,
            Err(err) => {
                self.status_msg = Some((format!("导出失败：{err}"), true));
                return;
            }
        };

        let path = PathBuf::from(&self.output_path);
        let config = ExportConfig {
            preset,
            input: ExportInput::Timeline(TimelineExportInput {
                sequence,
                asset_paths,
                in_point_frame: state.project_in_point,
                out_point_frame: state.project_out_point,
            }),
            output_path: path,
        };
        let job = RenderJob::new(config);
        state.render_queue.enqueue(job);
        self.status_msg = Some(("已加入导出队列".to_owned(), false));
        tracing::info!("导出任务已加入队列: {}", self.output_path);
    }
}

fn collect_timeline_asset_paths(
    state: &AppState,
    sequence: &mondrian_timeline::sequence::Sequence,
) -> Result<HashMap<mondrian_core::types::AssetId, PathBuf>, String> {
    let library = state.asset_library.as_ref().ok_or_else(|| "素材库未连接".to_string())?;

    let mut asset_ids = HashSet::new();
    for track in sequence.video_tracks.iter().chain(sequence.audio_tracks.iter()) {
        for clip in &track.clips {
            if clip.is_disabled {
                continue;
            }
            asset_ids.insert(clip.asset_id);
        }
    }

    let mut paths = HashMap::new();
    for asset_id in asset_ids {
        let asset = library
            .get_asset(asset_id)
            .map_err(|err| format!("读取素材 {} 失败: {}", asset_id, err))?
            .ok_or_else(|| format!("素材不存在: {}", asset_id))?;

        if !asset.path.exists() {
            return Err(format!("素材离线: {}", asset.path.display()));
        }
        paths.insert(asset_id, asset.path);
    }

    Ok(paths)
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
