//! 布局引擎
//!
//! 提供 Flex 和 Stack 两种布局算法。
//! 所有布局算法都是纯函数：输入父 bounds + 子 Widget 列表，输出每个子的最终 Rect。

pub mod constraint;
pub mod flex;
pub mod stack;

pub use constraint::ExtendedConstraint;
pub use flex::{AlignItems, FlexDirection, FlexLayout, JustifyContent};
pub use stack::StackLayout;
