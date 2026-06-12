//! Tooltip 管理器 trait
//!
//! 统一管理所有控件的 Hover Tooltip 显示。
//! 支持延迟显示、自动截断检测。

use crate::types::Point;

/// Tooltip 状态
#[derive(Debug, Clone)]
pub struct TooltipState {
    /// Tooltip 文本内容
    pub text: String,
    /// 显示位置（锚点）
    pub position: Point,
    /// 是否可见
    pub visible: bool,
}

/// Tooltip 管理器
///
/// ## 职责
///
/// * 接收 Widget 的 tooltip 请求
/// * 延迟显示（hover 后 N ms 才出现）
/// * 同一时间只显示一个 tooltip
/// * 文字未截断时不显示（自动截断检测）
pub trait TooltipManager {
    /// 请求显示 tooltip
    fn show(&mut self, text: String, position: Point);

    /// 隐藏当前 tooltip
    fn hide(&mut self);

    /// 当前 tooltip 状态
    fn current(&self) -> Option<&TooltipState>;

    /// 每帧更新（处理延迟显示计时器）
    fn update(&mut self, delta_ms: u64);

    /// Milliseconds until the manager needs another timer update.
    ///
    /// Returns `None` when no delayed tooltip or timed transition is pending.
    fn next_update_in_ms(&self) -> Option<u64> {
        None
    }
}
