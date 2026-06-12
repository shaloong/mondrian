use crate::app::AppState;
use crate::egui_ui::theme::{self, palette, tokens, typography};
use egui::Ui;
use mondrian_assets::AssetKind;
use mondrian_core::types::SequenceId;
use mondrian_export::{
    preset::{
        ExportConfig, ExportInput, ExportPreset, TimelineExportInput, TimelineExportRange,
        VideoCodecConfig,
    },
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
    selected_sequence_id: Option<SequenceId>,
    selected_range: TimelineExportRange,
}

impl ExportPanel {
    pub fn show(&mut self, ui: &mut Ui, state: &mut AppState) {
        egui::ScrollArea::vertical().id_salt("export_panel_scroll").show(ui, |ui| {
            ui.vertical(|ui| {
                let presets = builtin_presets();
                let preset = &presets[self.selected_preset_idx].1;
                let pending = state.render_queue.list_jobs().len();
                let combo_id = ui.make_persistent_id("export_preset");
                let combo_open = egui::Popup::is_id_open(ui.ctx(), combo_id)
                    || egui::Popup::is_id_open(ui.ctx(), combo_id.with("popup"));

                ui.label(
                    egui::RichText::new(format!("队列中 {} 个任务", pending))
                        .font(typography::body_small())
                        .color(palette::text_muted()),
                );
                ui.add_space(tokens::panel_gap());

                ui.label("导出预设");
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

                ui.add_space(tokens::spacing_md());
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

                        ui.label("分辨率");
                        ui.label(resolution_text);
                        ui.end_row();

                        ui.label("视频编码");
                        ui.label(codec_name);
                        ui.end_row();

                        ui.label("视频码率");
                        ui.label(bitrate_text);
                        ui.end_row();

                        ui.label("容器格式");
                        ui.label(format!("{:?}", preset.container));
                        ui.end_row();
                    });

                ui.add_space(tokens::panel_gap());
                ui.separator();
                ui.add_space(tokens::panel_gap());

                ui.label("输入源");
                let sequences = state.export_sequences_snapshot();
                if !sequences.is_empty() {
                    if !self
                        .selected_sequence_id
                        .map(|id| sequences.iter().any(|sequence| sequence.id == id))
                        .unwrap_or(false)
                    {
                        self.selected_sequence_id = state
                            .active_sequence_id
                            .or(state.default_sequence_id)
                            .filter(|id| sequences.iter().any(|sequence| sequence.id == *id))
                            .or_else(|| sequences.first().map(|sequence| sequence.id));
                    }
                    let selected_sequence_id = self.selected_sequence_id;
                    let selected_sequence = selected_sequence_id
                        .and_then(|id| sequences.iter().find(|sequence| sequence.id == id))
                        .or_else(|| sequences.first())
                        .expect("non-empty sequences should have first sequence");
                    egui::ComboBox::from_id_salt("export_sequence")
                        .selected_text(&selected_sequence.name)
                        .show_ui(ui, |ui| {
                            for sequence in &sequences {
                                theme::checkmark_selectable_value(
                                    ui,
                                    &mut self.selected_sequence_id,
                                    Some(sequence.id),
                                    &sequence.name,
                                );
                            }
                        });
                    ui.add_space(tokens::spacing_sm());
                    let sequence = selected_sequence;
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
                    ui.add_space(tokens::spacing_sm());
                    egui::ComboBox::from_id_salt("export_range")
                        .selected_text(export_range_label(self.selected_range))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.selected_range,
                                TimelineExportRange::SequenceInOut,
                                export_range_label(TimelineExportRange::SequenceInOut),
                            );
                            ui.selectable_value(
                                &mut self.selected_range,
                                TimelineExportRange::EntireSequence,
                                export_range_label(TimelineExportRange::EntireSequence),
                            );
                        });
                } else {
                    ui.horizontal(|ui| {
                        let _ = theme::icon(ui, theme::UiIcon::Warning, palette::status_warning());
                        ui.colored_label(palette::status_warning(), "当前无可导出的序列");
                    });
                }

                ui.add_space(tokens::panel_gap());
                ui.separator();
                ui.add_space(tokens::panel_gap());

                ui.label("输出文件");
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [ui.available_width() - 88.0, ui.spacing().interact_size.y],
                        egui::TextEdit::singleline(&mut self.output_path).hint_text("选择导出路径"),
                    );
                    if ui.button("浏览…").clicked() {
                        let default_name = default_output_filename(preset);
                        if let Some(path) =
                            FileDialog::new().set_file_name(&default_name).save_file()
                        {
                            self.output_path = path.display().to_string();
                        }
                    }
                });

                if self.output_path.is_empty() {
                    ui.add_space(tokens::spacing_xs());
                    ui.horizontal(|ui| {
                        let _ = theme::icon(ui, theme::UiIcon::Warning, palette::status_warning());
                        ui.colored_label(palette::status_warning(), "请指定输出路径");
                    });
                }

                ui.add_space(tokens::panel_gap());

                if let Some((msg, is_err)) = &self.status_msg {
                    let color = if *is_err {
                        palette::status_error()
                    } else {
                        palette::status_success()
                    };
                    ui.colored_label(color, msg);
                    ui.add_space(tokens::panel_gap());
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let can_export = !self.output_path.is_empty() && !sequences.is_empty();
                    let export_button = egui::Button::new(
                        egui::RichText::new("加入导出队列").color(palette::text_primary()).strong(),
                    )
                    .fill(palette::interaction_highlight())
                    .stroke(egui::Stroke::NONE);
                    if ui.add_enabled(can_export, export_button).clicked() {
                        self.enqueue(state, preset.clone());
                    }
                });
            });
        });
    }

    fn enqueue(&mut self, state: &mut AppState, preset: ExportPreset) {
        let sequences = state.export_sequences_snapshot();
        let Some(sequence) = self
            .selected_sequence_id
            .and_then(|id| sequences.iter().find(|sequence| sequence.id == id))
            .cloned()
            .or_else(|| state.sequence.clone())
        else {
            self.status_msg = Some(("导出失败：当前无序列".to_owned(), true));
            return;
        };

        let (asset_paths, asset_color_spaces) =
            match collect_timeline_asset_paths(state, &sequence, &sequences) {
                Ok(result) => result,
                Err(err) => {
                    if err == "素材库未连接" {
                        self.status_msg = None;
                        return;
                    }
                    self.status_msg = Some((format!("导出失败：{err}"), true));
                    return;
                }
            };

        let path = PathBuf::from(&self.output_path);
        let config = ExportConfig {
            preset,
            input: ExportInput::Timeline(Box::new(TimelineExportInput {
                sequence,
                sequences,
                asset_paths,
                asset_color_spaces,
                range: self.selected_range,
                project_color_management: state.project_settings.color_management.clone(),
            })),
            output_path: path,
        };
        let job = RenderJob::new(config);
        state.render_queue.enqueue(job);
        self.status_msg = Some(("已加入导出队列".to_owned(), false));
        tracing::info!("导出任务已加入队列: {}", self.output_path);
    }
}

fn export_range_label(range: TimelineExportRange) -> &'static str {
    match range {
        TimelineExportRange::SequenceInOut => "序列入点/出点",
        TimelineExportRange::EntireSequence => "整个序列",
        TimelineExportRange::WorkArea { .. } => "工作区",
    }
}

fn collect_timeline_asset_paths(
    state: &AppState,
    sequence: &mondrian_timeline::sequence::Sequence,
    sequences: &[mondrian_timeline::sequence::Sequence],
) -> Result<TimelineAssetPaths, String> {
    let library = state.asset_library.as_ref().ok_or_else(|| "素材库未连接".to_string())?;

    let mut asset_ids = HashSet::new();
    let mut visited_sequences = HashSet::new();
    collect_sequence_asset_ids(sequence, sequences, &mut visited_sequences, &mut asset_ids)?;

    let mut paths = HashMap::new();
    let mut color_spaces = HashMap::new();
    for asset_id in asset_ids {
        let asset = library
            .get_asset(asset_id)
            .map_err(|err| format!("读取素材 {} 失败: {}", asset_id, err))?
            .ok_or_else(|| format!("素材不存在: {}", asset_id))?;

        if matches!(asset.kind, AssetKind::AdjustmentLayer) {
            continue;
        }

        if !asset.path.exists() {
            return Err(format!("素材离线: {}", asset.path.display()));
        }
        let color_space = asset
            .media_info
            .primary_video()
            .map(|video| video.color_space)
            .unwrap_or(mondrian_core::types::ColorSpace::Rec709);
        paths.insert(asset_id, asset.path);
        color_spaces.insert(asset_id, color_space);
    }

    Ok((paths, color_spaces))
}

type TimelineAssetPaths = (
    HashMap<mondrian_core::types::AssetId, PathBuf>,
    HashMap<mondrian_core::types::AssetId, mondrian_core::types::ColorSpace>,
);

fn collect_sequence_asset_ids(
    sequence: &mondrian_timeline::sequence::Sequence,
    sequences: &[mondrian_timeline::sequence::Sequence],
    visited_sequences: &mut HashSet<SequenceId>,
    asset_ids: &mut HashSet<mondrian_core::types::AssetId>,
) -> Result<(), String> {
    if !visited_sequences.insert(sequence.id) {
        return Ok(());
    }

    for track in sequence.video_tracks.iter().chain(sequence.audio_tracks.iter()) {
        for clip in &track.clips {
            if clip.is_disabled {
                continue;
            }
            if clip.is_nested_sequence() {
                let Some(nested_sequence_id) = clip.nested_sequence_id else {
                    return Err(format!("嵌套序列片段缺少序列引用: {}", clip.id));
                };
                let nested_sequence = sequences
                    .iter()
                    .find(|sequence| sequence.id == nested_sequence_id)
                    .ok_or_else(|| format!("嵌套序列不存在: {nested_sequence_id}"))?;
                collect_sequence_asset_ids(
                    nested_sequence,
                    sequences,
                    visited_sequences,
                    asset_ids,
                )?;
                continue;
            }
            asset_ids.insert(clip.asset_id);
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_assets::AssetLibrary;
    use mondrian_core::types::TimeCode;
    use mondrian_timeline::{clip::Clip, sequence::Sequence};

    #[test]
    fn build_asset_paths_skips_synthetic_adjustment_assets() {
        let mut state = AppState::default();
        state.sequence = Some(Sequence::new("export-adjustment"));
        let temp_root = std::env::temp_dir().join(format!(
            "mondrian-export-adjustment-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        state.asset_library = Some(AssetLibrary::open(temp_root.clone()).expect("open library"));

        let asset_id = state.create_adjustment_layer_asset(None).expect("create adjustment asset");
        {
            let seq = state.sequence.as_mut().expect("sequence should exist");
            let tb = seq.time_base();
            seq.video_tracks[0]
                .add_clip(Clip::new_adjustment_layer(
                    asset_id,
                    TimeCode::new(0, tb),
                    TimeCode::new(20, tb),
                ))
                .expect("add adjustment clip");
        }

        let seq = state.sequence.as_ref().expect("sequence should exist");
        let (paths, color_spaces) =
            collect_timeline_asset_paths(&state, seq, std::slice::from_ref(seq))
                .expect("collect asset paths");
        assert!(!paths.contains_key(&asset_id));
        assert!(!color_spaces.contains_key(&asset_id));

        let _ = std::fs::remove_dir_all(temp_root);
    }

    #[test]
    fn collect_sequence_asset_ids_recurses_into_nested_sequences() {
        let mut parent = Sequence::new("parent");
        let mut child = Sequence::new("child");
        let tb = parent.time_base();
        let asset_id = mondrian_core::types::AssetId::new();

        child.video_tracks[0]
            .add_clip(Clip::new(
                asset_id,
                TimeCode::new(0, tb),
                TimeCode::new(12, tb),
            ))
            .expect("add media");
        parent.video_tracks[0]
            .add_clip(Clip::new_nested_sequence(
                child.id,
                TimeCode::new(0, tb),
                TimeCode::new(12, tb),
                Some("child".to_string()),
            ))
            .expect("add nested");

        let sequences = vec![parent.clone(), child];
        let mut visited = HashSet::new();
        let mut assets = HashSet::new();
        collect_sequence_asset_ids(&parent, &sequences, &mut visited, &mut assets)
            .expect("collect nested assets");

        assert!(assets.contains(&asset_id));
    }
}
