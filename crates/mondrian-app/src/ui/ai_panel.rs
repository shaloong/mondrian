use crate::{
    app::AppState,
    ui::theme::{self, palette},
};
use egui::Ui;

/// 右侧 AI 工作流面板
#[derive(Default)]
pub struct AiPanel {
    /// 当前输入的工作流 YAML
    workflow_yaml: String,
    /// 执行状态日志
    log_lines: Vec<String>,
    /// 是否正在运行
    running: bool,
}

impl AiPanel {
    pub fn show(&mut self, ui: &mut Ui, _state: &mut AppState) {
        ui.vertical(|ui| {
            ui.heading("AI 工作流");
            ui.separator();

            // ── YAML 编辑器 ─────────────────
            ui.label("工作流 DSL (YAML):");
            egui::ScrollArea::vertical()
                .id_salt("workflow_yaml_scroll")
                .max_height(240.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.workflow_yaml)
                            .font(egui::FontId::monospace(12.0))
                            .desired_rows(12)
                            .desired_width(f32::INFINITY)
                            .code_editor(),
                    );
                });

            ui.horizontal(|ui| {
                // 加载示例
                if ui.button("加载示例").clicked() {
                    self.workflow_yaml = EXAMPLE_WORKFLOW_YAML.to_owned();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let run_clicked = if self.running {
                        ui.button("停止").clicked()
                    } else {
                        theme::icon_text_button(ui, theme::UiIcon::Play, "运行").clicked()
                    };
                    if run_clicked {
                        if self.running {
                            self.running = false;
                            self.log_lines.push("[用户] 工作流已停止".to_owned());
                        } else {
                            self.start_workflow();
                        }
                    }
                });
            });

            ui.separator();

            // ── 执行日志 ────────────────────
            ui.label("执行日志:");
            egui::ScrollArea::vertical()
                .id_salt("workflow_log_scroll")
                .stick_to_bottom(true)
                .max_height(180.0)
                .show(ui, |ui| {
                    for line in &self.log_lines {
                        let color = if line.starts_with("[错误]") {
                            palette::status_error()
                        } else if line.starts_with("[完成]") {
                            palette::status_success()
                        } else {
                            palette::text_muted()
                        };
                        ui.colored_label(color, line);
                    }
                });
        });
    }

    fn start_workflow(&mut self) {
        if self.workflow_yaml.trim().is_empty() {
            self.log_lines.push("[错误] 工作流 YAML 为空".to_owned());
            return;
        }
        // TODO: 解析 YAML → WorkflowDef → AgentOrchestrator::run_workflow()
        self.running = true;
        self.log_lines.clear();
        self.log_lines.push("[开始] 解析工作流…".to_owned());
        self.log_lines.push("[步骤 1/3] generate_image — 排队中…".to_owned());
        // 实际调用将通过 tokio::spawn + EventBus 推送进度
    }
}

/// 内置 AI 导演模式示例 YAML
const EXAMPLE_WORKFLOW_YAML: &str = r#"name: "AI 导演模式示例"
version: "1.0"

inputs:
  character: "时尚博主小林"
  product: "新款运动鞋"

steps:
  - id: gen_script
    action: llm_chat
    provider: openai
    params:
      model: gpt-4o
      prompt: "为 {character} 写一段 30 秒的 {product} 带货脚本"

  - id: gen_image
    action: generate_image
    provider: kling
    depends_on: [gen_script]
    params:
      prompt: "{character} 展示 {product}，时尚街拍风格"
      aspect_ratio: "9:16"

  - id: gen_voice
    action: text_to_speech
    provider: openai
    depends_on: [gen_script]
    params:
      text: "{{gen_script.output}}"
      voice: alloy
"#;
