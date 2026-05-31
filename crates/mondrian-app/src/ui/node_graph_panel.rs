//! Node Graph panel — skeleton for future DAG visualization.
//!
//! Phase 5: Placeholder panel. The Node Graph will visualize the
//! underlying Effect DAG as an interactive node editor, providing
//! an alternative projection of the same data that drives the
//! linear Timeline view.

use egui::Ui;

/// Canvas display mode — toggles between linear Timeline and Node Graph views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanvasMode {
    /// Standard linear timeline view (current default).
    Timeline,
    /// Node graph view — effects, masks, and blends as a DAG.
    NodeGraph,
}

impl CanvasMode {
    pub fn label(&self) -> &str {
        match self {
            Self::Timeline => "时间线",
            Self::NodeGraph => "节点图",
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::Timeline => Self::NodeGraph,
            Self::NodeGraph => Self::Timeline,
        }
    }
}

/// Placeholder panel for the Node Graph view.
///
/// Will eventually render the Effect DAG as interactive nodes with
/// input/output ports, parameter widgets, and real-time preview.
#[derive(Default)]
pub struct NodeGraphPanel {}

impl NodeGraphPanel {
    pub fn show(&mut self, ui: &mut Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(80.0);
            ui.heading("节点图编辑器");
            ui.add_space(20.0);
            ui.label("Node Graph — 即将推出");
            ui.add_space(10.0);
            ui.label("在此视图中，您将可以：");
            ui.add_space(8.0);
            ui.label("• 以节点图方式查看和编辑特效 DAG");
            ui.label("• 拖拽连接输入/输出端口");
            ui.label("• 可视化 Blend / Mask 的数据流");
            ui.label("• 实时预览节点输出");
        });
    }
}
