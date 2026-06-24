//! 事件路由系统
//!
//! 将用户输入事件从根 Widget 向下路由到目标 Widget，支持冒泡和捕获。
//! 包含 FocusManager、ShortcutManager 的真实实现。

mod capture;
pub mod focus_manager;
pub mod hit_test;
pub mod router;
pub mod shortcut_manager;

pub use focus_manager::FocusManagerImpl;
pub use router::{EventRouteDiagnostics, EventRouter};
pub use shortcut_manager::ShortcutManagerImpl;
