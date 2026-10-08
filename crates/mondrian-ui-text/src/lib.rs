//! Mondrian UI 文本渲染引擎
//!
//! 基于 cosmic-text 0.19 的文本排版和 GPU 字形渲染。

pub mod atlas;
pub mod font;
pub mod layout;
pub mod render;
pub mod single_line;

pub use render::{
    resolve_text_commands, ResolvedTextCommands, SingleLineTextGeometry,
    SingleLineTextSelectionGeometry, SingleLineTextSelectionSpan, TextRenderer, TextResolveStats,
};

pub use single_line::{
    single_line_request_admitted, SingleLineTextCaret, SingleLineTextNavigation,
    SingleLineTextProjection,
};
