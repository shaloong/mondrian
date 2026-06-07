//! Mondrian UI Tooltip 系统
//!
//! 提供 hover 检测、延迟显示、自动截断检测的 Tooltip 实现。

pub mod manager;
pub mod widget;

pub use manager::TooltipManagerImpl;
pub use widget::TooltipWidget;
