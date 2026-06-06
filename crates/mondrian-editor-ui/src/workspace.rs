//! 工作区布局系统
//!
//! [`WorkspaceLayout`] 描述 Dock 面板的布局树。
//! 支持保存/恢复、预设布局切换。
//!
//! ## 布局树结构
//!
//! ```text
//! WorkspaceLayout
//!   └─ DockNode::Split(Horizontal)
//!        ├─ DockNode::Split(Vertical)
//!        │    ├─ DockNode::Panel(Viewer)
//!        │    └─ DockNode::Tabs([Timeline])
//!        └─ DockNode::Split(Vertical)
//!             ├─ DockNode::Panel(Inspector)
//!             └─ DockNode::Panel(Assets)
//! ```

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::panel::PanelKind;

/// 工作区布局
///
/// 包含主 Dock 树和浮窗列表。
/// 可序列化为 `workspace.json` 保存/恢复。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceLayout {
    /// 版本号（用于向前兼容）
    pub version: u32,
    /// 根 Dock 节点
    pub root: DockNode,
    /// 浮窗列表
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub floating: Vec<FloatingWindow>,
}

impl Default for WorkspaceLayout {
    fn default() -> Self {
        Self::editing()
    }
}

impl WorkspaceLayout {
    pub const CURRENT_VERSION: u32 = 1;

    // ═══════════════════════════════════════════════════════════════════
    // 预设布局
    // ═══════════════════════════════════════════════════════════════════

    /// 编辑工作区：Viewer + Timeline + Inspector + Assets
    pub fn editing() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            root: DockNode::Split {
                direction: SplitDirection::Horizontal,
                ratio: 0.25,
                children: vec![
                    // 左侧：Assets
                    DockNode::Panel { kind: PanelKind::Assets },
                    // 右侧：垂直分割
                    DockNode::Split {
                        direction: SplitDirection::Vertical,
                        ratio: 0.65,
                        children: vec![
                            // 上方：Viewer
                            DockNode::Panel { kind: PanelKind::Viewer },
                            // 下方：水平分割
                            DockNode::Split {
                                direction: SplitDirection::Horizontal,
                                ratio: 0.75,
                                children: vec![
                                    // 左下：Timeline
                                    DockNode::Panel { kind: PanelKind::Timeline },
                                    // 右下：Inspector
                                    DockNode::Panel { kind: PanelKind::Inspector },
                                ],
                            },
                        ],
                    },
                ],
            },
            floating: vec![],
        }
    }

    /// 调色工作区：Viewer（大） + Scopes + Inspector
    pub fn color() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            root: DockNode::Split {
                direction: SplitDirection::Horizontal,
                ratio: 0.7,
                children: vec![
                    DockNode::Panel { kind: PanelKind::Viewer },
                    DockNode::Split {
                        direction: SplitDirection::Vertical,
                        ratio: 0.5,
                        children: vec![
                            DockNode::Panel { kind: PanelKind::Inspector },
                            DockNode::Panel { kind: PanelKind::Effects },
                        ],
                    },
                ],
            },
            floating: vec![],
        }
    }

    /// 音频工作区：Viewer + Mixer + Timeline（音频轨道优先）
    pub fn audio() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            root: DockNode::Split {
                direction: SplitDirection::Vertical,
                ratio: 0.4,
                children: vec![
                    DockNode::Panel { kind: PanelKind::Viewer },
                    DockNode::Split {
                        direction: SplitDirection::Horizontal,
                        ratio: 0.3,
                        children: vec![
                            DockNode::Panel { kind: PanelKind::Timeline },
                            DockNode::Panel { kind: PanelKind::Inspector },
                        ],
                    },
                ],
            },
            floating: vec![],
        }
    }

    /// 合成工作区：NodeGraph + Viewer + Effects
    pub fn compositing() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            root: DockNode::Split {
                direction: SplitDirection::Horizontal,
                ratio: 0.35,
                children: vec![
                    DockNode::Panel { kind: PanelKind::NodeGraph },
                    DockNode::Split {
                        direction: SplitDirection::Vertical,
                        ratio: 0.6,
                        children: vec![
                            DockNode::Panel { kind: PanelKind::Viewer },
                            DockNode::Panel { kind: PanelKind::Effects },
                        ],
                    },
                ],
            },
            floating: vec![],
        }
    }

    /// 导出工作区：Export + Viewer（小）
    pub fn export() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            root: DockNode::Split {
                direction: SplitDirection::Horizontal,
                ratio: 0.55,
                children: vec![
                    DockNode::Panel { kind: PanelKind::Export },
                    DockNode::Panel { kind: PanelKind::Viewer },
                ],
            },
            floating: vec![],
        }
    }

    // ═══════════════════════════════════════════════════════════════════
    // 持久化
    // ═══════════════════════════════════════════════════════════════════

    /// 序列化为 JSON 字符串
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// 从 JSON 字符串反序列化
    pub fn from_json(json: &str) -> serde_json::Result<Self> {
        serde_json::from_str(json)
    }

    /// 保存到文件
    pub fn save_to_file(&self, path: &Path) -> std::io::Result<()> {
        let json = self
            .to_json()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        std::fs::write(path, json)
    }

    /// 从文件加载
    pub fn load_from_file(path: &Path) -> std::io::Result<Self> {
        let json = std::fs::read_to_string(path)?;
        Self::from_json(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
    }

    /// 收集布局中所有使用的 PanelKind
    pub fn collect_panels(&self) -> Vec<PanelKind> {
        let mut panels = Vec::new();
        self.root.collect_panels(&mut panels);
        for floating in &self.floating {
            panels.push(floating.panel);
        }
        panels
    }
}

/// Dock 布局节点
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DockNode {
    /// 分割容器
    Split {
        direction: SplitDirection,
        /// 分割比例 [0.0, 1.0]（第一个 child 占的比例）
        ratio: f32,
        children: Vec<DockNode>,
    },
    /// Tab 容器
    Tabs {
        /// 当前活跃的 Tab 索引
        active: usize,
        children: Vec<TabContent>,
    },
    /// 单个面板
    Panel { kind: PanelKind },
}

impl DockNode {
    fn collect_panels(&self, panels: &mut Vec<PanelKind>) {
        match self {
            Self::Split { children, .. } => {
                for child in children {
                    child.collect_panels(panels);
                }
            }
            Self::Tabs { children, .. } => {
                for tab in children {
                    panels.push(tab.panel);
                }
            }
            Self::Panel { kind } => panels.push(*kind),
        }
    }
}

pub use mondrian_ui_core::types::SplitDirection;

/// Tab 标签内容
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabContent {
    /// Tab 显示标签
    pub label: String,
    /// 包含的面板类型
    pub panel: PanelKind,
}

/// 浮窗
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FloatingWindow {
    /// 浮窗位置和大小（屏幕坐标）
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// 包含的面板类型
    pub panel: PanelKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_layout_round_trip() {
        let layout = WorkspaceLayout::editing();
        let json = layout.to_json().expect("serialize");
        let back = WorkspaceLayout::from_json(&json).expect("deserialize");
        assert_eq!(layout.version, back.version);
    }

    #[test]
    fn collect_panels_from_editing_layout() {
        let layout = WorkspaceLayout::editing();
        let panels = layout.collect_panels();
        assert!(panels.contains(&PanelKind::Viewer));
        assert!(panels.contains(&PanelKind::Timeline));
        assert!(panels.contains(&PanelKind::Inspector));
        assert!(panels.contains(&PanelKind::Assets));
    }

    #[test]
    fn all_presets_serialize() {
        let layouts = [
            WorkspaceLayout::editing(),
            WorkspaceLayout::color(),
            WorkspaceLayout::audio(),
            WorkspaceLayout::compositing(),
            WorkspaceLayout::export(),
        ];
        for layout in &layouts {
            let json = layout.to_json().expect("serialize");
            let back = WorkspaceLayout::from_json(&json).expect("deserialize");
            assert_eq!(layout.version, back.version);
        }
    }
}
