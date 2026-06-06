//! Mondrian UI 控件库
//!
//! 提供基于 Widget trait 的可复用交互控件。

pub mod button;
pub mod dock_splitter;
pub mod dock_tab_bar;
pub mod label;
pub mod panel_slot;
pub mod scroll;
pub mod slider;

pub use button::Button;
pub use dock_splitter::{DockSplitter, SplitDirection};
pub use dock_tab_bar::{DockTabBar, TabInfo};
pub use label::Label;
pub use panel_slot::{PanelSlot, SlotKind};
pub use scroll::ScrollView;
pub use slider::Slider;
