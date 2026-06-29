//! Mondrian UI 控件库
//!
//! 提供基于 Widget trait 的可复用交互控件。

pub mod asset_grid;
pub mod button;
pub mod checkbox;
pub mod color_picker;
pub mod context_menu;
pub mod curve_editor;
pub mod dialog_surface;
pub mod dock_panel;
pub mod dock_splitter;
pub mod dock_tab_bar;
pub mod flex_container;
pub mod form_layout;
pub mod icon_button;
pub mod label;
pub mod list;
pub mod menu;
pub mod node_graph_view;
pub mod number_input;
mod paint;
pub mod panel_header;
pub mod panel_list;
pub mod panel_slot;
pub mod property_panel;
pub mod raster_image;
pub mod scroll;
pub mod segmented_button_group;
pub mod slider;
pub mod text_input;
mod text_metrics;
pub mod timeline_view;
pub mod vector_icon;
pub mod viewer_surface;

#[cfg(test)]
mod component_extreme_tests;
#[cfg(test)]
mod component_visual_tests;
#[cfg(test)]
mod test_utils;

pub use asset_grid::{
    AssetGrid, AssetGridAction, AssetGridBadge, AssetGridBadgeTone, AssetGridItem, AssetGridState,
    AssetGridThumbnailStatus,
};
pub use button::Button;
pub use checkbox::Checkbox;
pub use color_picker::{
    ColorPicker, ColorPickerAreaMode, ColorPickerMode, ColorPickerTrigger,
    ColorPickerTriggerOptions,
};
pub use context_menu::ContextMenu;
pub use curve_editor::{CurveEditor, CurvePoint};
pub use dialog_surface::DialogSurface;
pub use dock_panel::{DockPanel, DockPanelDropArea};
pub use dock_splitter::DockSplitter;
pub use dock_tab_bar::{DockTabBar, TabInfo};
pub use flex_container::{FlexChild, FlexContainer};
pub use form_layout::{FormLayout, FormRowOptions, FormRowRects};
pub use icon_button::IconButton;
pub use label::Label;
pub use list::{List, ListItem};
pub use menu::{Dropdown, MenuItem};
pub use node_graph_view::{NodeGraphEdge, NodeGraphNode, NodeGraphView};
pub use number_input::{NumberInput, NumberInputChangeAction};
pub use panel_list::{
    PanelList, PanelListAction, PanelListBadge, PanelListBadgeTone, PanelListItem, PanelListState,
};
pub use panel_slot::PanelSlot;
pub use property_panel::{PropertyPanel, PropertyPanelOptions, PropertyRow, PropertySection};
pub use raster_image::RasterImage;
pub use scroll::{ScrollView, ScrollViewState};
pub use segmented_button_group::{SegmentedButtonGroup, SegmentedButtonItem};
pub use slider::Slider;
pub use text_input::{MultilineTextInput, TabBehavior, TextInput, TextInputChangeAction};
pub use timeline_view::{
    TimelineAssetDrop, TimelineAssetDropAction, TimelineClip, TimelineClipAction, TimelineClipKind,
    TimelineClipMove, TimelineClipMoveAction, TimelineClipRef, TimelineClipTrim,
    TimelineClipTrimAction, TimelineEditCommand, TimelineEditCommandAction, TimelineInOutPoint,
    TimelineInOutPointAction, TimelineSeekAction, TimelineTool, TimelineToolbarIconSlot,
    TimelineTrack, TimelineTrackAction, TimelineTrackAddAction, TimelineTrackControl,
    TimelineTrackControlAction, TimelineTrackControlIconSlot, TimelineTrackKind, TimelineTrackMove,
    TimelineTrackMoveAction, TimelineTrackRef, TimelineTrimEdge, TimelineView, TimelineViewState,
    WaveformDisplay,
};
pub use vector_icon::{VectorIcon, VectorIconError};
pub use viewer_surface::{
    ViewerControl, ViewerControlAction, ViewerFrameImage, ViewerStatusTone, ViewerSurface,
};
