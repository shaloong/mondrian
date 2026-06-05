//! Panel 统一接口
//!
//! 所有面板都必须实现 [`Panel`] trait。
//! Panel 之间**禁止直接引用**，只能通过 `EventBus` 通信。

use std::borrow::Cow;

use mondrian_core::events::AppEvent;
use mondrian_ui_core::Widget;

/// 面板类型标识
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum PanelKind {
    Viewer,
    Timeline,
    Assets,
    Inspector,
    Effects,
    Project,
    Console,
    NodeGraph,
    Export,
}

impl PanelKind {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Viewer => "预览",
            Self::Timeline => "时间线",
            Self::Assets => "素材",
            Self::Inspector => "检查器",
            Self::Effects => "效果",
            Self::Project => "项目",
            Self::Console => "控制台",
            Self::NodeGraph => "节点图",
            Self::Export => "导出",
        }
    }

    pub fn icon_name(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Timeline => "timeline",
            Self::Assets => "assets",
            Self::Inspector => "inspector",
            Self::Effects => "effects",
            Self::Project => "project",
            Self::Console => "console",
            Self::NodeGraph => "node_graph",
            Self::Export => "export",
        }
    }

    /// 所有面板类型
    pub const ALL: [Self; 9] = [
        Self::Viewer,
        Self::Timeline,
        Self::Assets,
        Self::Inspector,
        Self::Effects,
        Self::Project,
        Self::Console,
        Self::NodeGraph,
        Self::Export,
    ];
}

/// 面板上下文（构建 Panel 时传入的服务依赖）
pub struct PanelContext {
    /// 事件总线（用于发送/接收 AppEvent）
    pub event_bus: std::sync::Arc<mondrian_core::events::EventBus>,
}

/// 所有面板的统一接口
///
/// ## 设计约束
///
/// * Panel 之间**禁止**直接持有对方引用
/// * Panel 通信通过 `EventBus`（发布/订阅）
/// * Panel 读取 `EditorState`（只读），修改通过 `dispatch(Action)`
/// * 每个 Panel 构建自己的 Widget 树
pub trait Panel: Send + Sync {
    /// 面板类型标识
    fn kind(&self) -> PanelKind;

    /// 面板标题（显示在 Tab 标签上）
    fn title(&self) -> Cow<'static, str>;

    /// 构建 Widget 树。工作区布局变化时调用。
    fn build_widget_tree(&mut self) -> Box<dyn Widget>;

    /// 接收来自 EventBus 的事件通知
    fn on_event(&mut self, event: &AppEvent) {
        let _ = event; // 默认忽略
    }

    /// 面板是否可关闭
    fn is_closable(&self) -> bool {
        true
    }

    /// 面板是否可拖拽移动
    fn is_draggable(&self) -> bool {
        true
    }
}
