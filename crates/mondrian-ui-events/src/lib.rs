//! 事件路由系统
//!
//! 将用户输入事件从根 Widget 向下路由到目标 Widget，支持冒泡和捕获。

pub mod hit_test;
pub mod router;

pub use router::EventRouter;
