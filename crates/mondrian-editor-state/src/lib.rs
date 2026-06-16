//! Mondrian 编辑器状态管理
//!
//! [`EditorState`] 是编辑器的唯一真相源，所有数据集中在这里。
//! [`Action`] 是所有状态变更的唯一入口。
//!
//! ## 设计原则
//!
//! * **单入口** — 修改 [`EditorState`] 只能通过 `dispatch(Action)`
//! * **可序列化** — [`Action`] 支持 serde，可录制/重放/AI 生成
//! * **UI 无关** — 此 crate 不依赖任何 UI 框架（egui/wgpu/winit）

pub mod action;
pub mod animation_groups;
pub mod dispatch;
pub mod state;

pub use action::Action;
pub use animation_groups::{
    property_display_name, property_group_meta, property_order, qualified_property_display_name,
    AnimationGroupKind, AnimationGroupMeta,
};
pub use dispatch::EditorDispatch;
pub use state::EditorState;
