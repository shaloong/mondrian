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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A test mock that records dispatched actions.
    struct MockDispatch {
        state: EditorState,
        dispatched: VecDeque<Action>,
        undo_count: usize,
        redo_count: usize,
    }

    impl MockDispatch {
        fn new() -> Self {
            Self {
                state: EditorState::new(),
                dispatched: VecDeque::new(),
                undo_count: 0,
                redo_count: 0,
            }
        }

        fn with_undo(mut self, count: usize) -> Self {
            self.undo_count = count;
            self
        }

        fn with_redo(mut self, count: usize) -> Self {
            self.redo_count = count;
            self
        }
    }

    impl EditorDispatch for MockDispatch {
        fn dispatch(&mut self, action: Action) -> Result<()> {
            self.dispatched.push_back(action);
            Ok(())
        }

        fn can_undo(&self) -> bool {
            self.undo_count > 0
        }

        fn can_redo(&self) -> bool {
            self.redo_count > 0
        }

        fn state(&self) -> &EditorState {
            &self.state
        }
    }

    #[test]
    fn mock_dispatch_records_actions() {
        let mut dispatch = MockDispatch::new();
        dispatch.dispatch(Action::Play).unwrap();
        dispatch.dispatch(Action::Pause).unwrap();
        assert_eq!(dispatch.dispatched.len(), 2);
        assert_eq!(dispatch.dispatched[0], Action::Play);
        assert_eq!(dispatch.dispatched[1], Action::Pause);
    }

    #[test]
    fn mock_dispatch_can_undo_is_false_by_default() {
        let dispatch = MockDispatch::new();
        assert!(!dispatch.can_undo());
    }

    #[test]
    fn mock_dispatch_can_undo_reflects_count() {
        let dispatch = MockDispatch::new().with_undo(5);
        assert!(dispatch.can_undo());
    }

    #[test]
    fn mock_dispatch_can_redo_reflects_count() {
        let dispatch = MockDispatch::new().with_redo(3);
        assert!(dispatch.can_redo());
    }

    #[test]
    fn mock_dispatch_state_is_accessible() {
        let dispatch = MockDispatch::new();
        assert!(!dispatch.state().has_open_project());
    }

    #[test]
    fn mock_dispatch_dispatches_all_action_variants() {
        let mut dispatch = MockDispatch::new();
        // Dispatch one of each major category
        dispatch.dispatch(Action::NewProject).unwrap();
        dispatch.dispatch(Action::Play).unwrap();
        dispatch.dispatch(Action::Undo).unwrap();
        dispatch.dispatch(Action::SelectAll).unwrap();
        dispatch.dispatch(Action::Copy).unwrap();
        dispatch.dispatch(Action::ToggleFullscreen).unwrap();
        assert_eq!(dispatch.dispatched.len(), 6);
    }

    #[test]
    fn dispatch_is_object_safe() {
        let mut dispatch: Box<dyn EditorDispatch> = Box::new(MockDispatch::new());
        dispatch.dispatch(Action::Play).unwrap();
        assert!(!dispatch.can_undo());
        assert!(!dispatch.state().has_open_project());
    }
}
