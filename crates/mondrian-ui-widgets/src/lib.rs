//! Mondrian UI 控件库
//!
//! 提供基于 Widget trait 的可复用交互控件。

pub mod button;
pub mod checkbox;
pub mod dock_splitter;
pub mod dock_tab_bar;
pub mod label;
pub mod panel_slot;
pub mod scroll;
pub mod slider;
pub mod text_input;

#[cfg(test)]
mod test_utils;

pub use button::Button;
pub use checkbox::Checkbox;
pub use dock_splitter::DockSplitter;
pub use dock_tab_bar::{DockTabBar, TabInfo};
pub use label::Label;
pub use panel_slot::{PanelSlot, SlotKind};
pub use scroll::ScrollView;
pub use slider::Slider;
pub use text_input::TextInput;
