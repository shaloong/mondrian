//! # mondrian-core
//!
//! Mondrian 核心基础库。提供所有模块共享的基础类型、错误体系、
//! 事件总线和项目数据模型。

pub mod automation;
pub mod color;
pub mod color_models;
pub mod effect_data;
pub mod error;
pub mod events;
pub mod icc;
pub mod mask_data;
pub mod ocio;
pub mod project;
pub mod render_graph;
pub mod timeline_data;
pub mod types;

pub use automation::*;
pub use color::*;
pub use color_models::*;
pub use error::{MondrianError, Result};
pub use events::{AppEvent, EventBus};
pub use ocio::*;
pub use project::*;
pub use types::*;
