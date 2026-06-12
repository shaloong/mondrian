//! Mondrian UI 控件库
//!
//! 提供基于 Widget trait 的可复用交互控件。

pub mod button;
pub mod checkbox;
pub mod color_picker;
pub mod context_menu;
pub mod curve_editor;
pub mod dock_panel;
pub mod dock_splitter;
pub mod dock_tab_bar;
pub mod flex_container;
pub mod label;
pub mod list;
pub mod menu;
pub mod panel_list;
pub mod panel_slot;
pub mod property_panel;
pub mod scroll;
pub mod slider;
pub mod text_input;

#[cfg(test)]
mod test_utils;

pub use button::Button;
pub use checkbox::Checkbox;
pub use color_picker::{
    ColorPicker, ColorPickerAreaMode, ColorPickerMode, ColorPickerTrigger,
    ColorPickerTriggerOptions,
};
pub use context_menu::ContextMenu;
pub use curve_editor::{CurveEditor, CurvePoint};
pub use dock_panel::DockPanel;
pub use dock_splitter::DockSplitter;
pub use dock_tab_bar::{DockTabBar, TabInfo};
pub use flex_container::{FlexChild, FlexContainer};
pub use label::Label;
pub use list::{List, ListItem};
pub use menu::{Dropdown, MenuItem};
pub use panel_list::{PanelList, PanelListAction, PanelListItem};
pub use panel_slot::{PanelSlot, SlotKind};
pub use property_panel::{PropertyPanel, PropertyPanelOptions, PropertyRow, PropertySection};
pub use scroll::ScrollView;
pub use slider::Slider;
pub use text_input::TextInput;
