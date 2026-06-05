//! Action 派发接口
//!
//! [`EditorDispatch`] 定义了将 [`Action`] 转化为 [`EditorState`] 修改的标准接口。
//! 实现者（当前为 `mondrian-app` 的 `AppState`）负责具体的状态变更逻辑。

use mondrian_core::error::Result;

use crate::action::Action;
use crate::state::EditorState;

/// Action 派发器 —— 连接 UI 输入与状态变更的桥梁
///
/// ## 实现要求
///
/// * `dispatch` 必须是同步的（不返回 Future）
/// * 每个 Action 的处理应该是原子操作
/// * 处理失败时返回 `Err(MondrianError)`，调用方可以显示错误提示
/// * 如果 Action 需要记录到撤销历史，由实现者内部处理
pub trait EditorDispatch {
    /// 派发一个 Action，修改内部状态
    fn dispatch(&mut self, action: Action) -> Result<()>;

    /// 是否可以撤销
    fn can_undo(&self) -> bool;

    /// 是否可以重做
    fn can_redo(&self) -> bool;

    /// 获取只读的状态引用
    fn state(&self) -> &EditorState;
}
