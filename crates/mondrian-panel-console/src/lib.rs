//! Mondrian Console Panel
//!
//! 首个用新 Widget 体系构建的 Panel。
//! 显示日志输出，与 egui 并行运行。

pub mod panel;
pub mod tracing_layer;

pub use panel::ConsolePanel;
pub use tracing_layer::ConsoleLogLayer;
