//! Mondrian 编辑器 UI 框架
//!
//! 提供 Panel 注册/创建、Workspace 布局管理。
//! 此 crate 是编辑器特有的 UI 逻辑，与通用 UI 引擎（mondrian-ui-*）分离。
//!
//! ## 核心抽象
//!
//! * [`Panel`] — 所有面板的统一接口
//! * [`PanelRegistry`] — 面板工厂注册表
//! * [`WorkspaceLayout`] — Dock 布局树 + 序列化

pub mod panel;
pub mod registry;
pub mod workspace;

pub use panel::{Panel, PanelKind};
pub use registry::PanelRegistry;
pub use workspace::{DockNode, FloatingWindow, SplitDirection, TabContent, WorkspaceLayout};
