//! UI 核心抽象
//!
//! 定义所有 UI 控件必须遵循的基础接口：
//! * [`Widget`] — 所有控件的统一 trait（measure / layout / event / paint）
//! * [`UiEvent`] — 用户输入事件类型
//! * 管理器 trait — FocusManager / ShortcutManager / TooltipManager
//!
//! ## 设计原则
//!
//! * **Retained Mode** — Widget 树驻留内存，变化时局部更新
//! * **事件冒泡** — 未处理的事件沿 Widget 树向上传播
//! * **绘制分离** — `paint()` 是纯读操作，不产生副作用
//! * **无具体实现** — 此 crate 只定义 trait，不实现具体 Widget

pub mod focus;
pub mod shortcut;
pub mod tooltip;
pub mod tree;
pub mod types;
pub mod widget;
pub mod widgets;

pub use focus::FocusManager;
pub use shortcut::{ShortcutBinding, ShortcutManager, ShortcutScope};
pub use tooltip::{TooltipManager, TooltipState};
pub use tree::{TreeWalker, WidgetTree};
pub use types::*;
pub use widget::{DrawCommandEncoder, EventContext, PaintContext, Widget, WidgetContext};
