//! GPU 2D UI 渲染后端
//!
//! 基于 wgpu 的 Retained Mode 绘制系统。与 egui 完全独立。
//!
//! ## 核心类型
//!
//! * [`DrawCommand`] — 绘制命令枚举
//! * [`DrawEncoder`] — 收集绘制命令的编码器
//! * [`RetainedDrawCommands`] — 可复用的结构化命令片段
//! * [`UiRenderer`] — wgpu 渲染器（管线 + 已解析绘制命令批次提交）

pub mod atlas;
pub mod batch;
pub mod command;
pub mod context;
pub mod pipeline;
pub mod shape;

pub use atlas::TextureAtlas;
pub use command::{
    DrawCommand, DrawEncoder, ExternalTextureKey, RetainedDrawCommandError, RetainedDrawCommands,
};
pub use context::{
    ExternalTextureRegistrationError, ExternalTextureTransfer, GlyphUpload, GlyphUploadStats,
    UiRenderFrameStats, UiRenderer,
};
pub use mondrian_ui_core::CornerRadii;
pub use shape::{generate_rect_vertices, RectVertex};
