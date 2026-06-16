//! Product icon assets for the self-hosted UI shell.
//!
//! This module is the app-layer registry for bundled designer-authored SVG
//! assets. Reusable widgets stay asset-agnostic; panels ask this registry for
//! vector geometry or icon buttons when they need Mondrian product icons.

use mondrian_ui_widgets::{Button, IconButton, VectorIcon, VectorIconError};

/// Built-in product icons available to the self-hosted UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppIcon {
    /// Add item outline icon.
    Add,
    /// Anchor point icon.
    Anchor,
    /// Down arrow icon.
    ArrowDown,
    /// Right arrow icon.
    ArrowRight,
    /// Up arrow icon.
    ArrowUp,
    /// Circle shape icon.
    Circle,
    /// Clipboard text icon.
    ClipboardText,
    /// Clock icon.
    Clock,
    /// Copy icon.
    Copy,
    /// Cursor outline icon.
    Cursor,
    /// Filled cursor icon.
    CursorFilled,
    /// Cut/scissors icon.
    Cut,
    /// Jump to sequence end icon.
    EndFrameFilled,
    /// Effect browser icon.
    Effect,
    /// Export icon.
    Export,
    /// Hidden eye icon.
    EyeHidden,
    /// Visible eye icon.
    EyeVisible,
    /// Film/video icon.
    Film,
    /// Folder outline icon.
    Folder,
    /// Filled open-folder icon.
    FolderOpenFilled,
    /// Full-screen icon.
    FullScreen,
    /// Grid icon.
    Grid,
    /// Jump to sequence start icon.
    HomeFrameFilled,
    /// Import icon.
    Import,
    /// Info icon.
    Info,
    /// Previous frame icon.
    LeftFrameFilled,
    /// List icon.
    List,
    /// Lock icon.
    Lock,
    /// Magnet/snap icon.
    Magnet,
    /// Music/audio icon.
    Music,
    /// Filled pause icon.
    PauseFilled,
    /// Pen icon.
    Pen,
    /// Filled play icon.
    PlayFilled,
    /// Filled plus icon.
    PlusFilled,
    /// Rectangle shape icon.
    Rectangle,
    /// Next frame icon.
    RightFrameFilled,
    /// Search icon.
    Search,
    /// Save/floppy icon.
    Save,
    /// Speaker icon.
    Speaker,
    /// Muted speaker icon.
    SpeakerMuted,
    /// Stopwatch/timer icon.
    Stopwatch,
    /// Trash/delete icon.
    Trash,
    /// Unlock icon.
    Unlock,
    /// Warning triangle icon.
    Warning,
    /// Zoom in icon.
    ZoomIn,
    /// Zoom out icon.
    ZoomOut,
}

impl AppIcon {
    /// Every bundled product icon that should remain parseable by the custom UI.
    pub const ALL: [Self; 46] = [
        Self::Add,
        Self::Anchor,
        Self::ArrowDown,
        Self::ArrowRight,
        Self::ArrowUp,
        Self::Circle,
        Self::ClipboardText,
        Self::Clock,
        Self::Copy,
        Self::Cursor,
        Self::CursorFilled,
        Self::Cut,
        Self::EndFrameFilled,
        Self::Effect,
        Self::Export,
        Self::EyeHidden,
        Self::EyeVisible,
        Self::Film,
        Self::Folder,
        Self::FolderOpenFilled,
        Self::FullScreen,
        Self::Grid,
        Self::HomeFrameFilled,
        Self::Import,
        Self::Info,
        Self::LeftFrameFilled,
        Self::List,
        Self::Lock,
        Self::Magnet,
        Self::Music,
        Self::PauseFilled,
        Self::Pen,
        Self::PlayFilled,
        Self::PlusFilled,
        Self::Rectangle,
        Self::RightFrameFilled,
        Self::Search,
        Self::Save,
        Self::Speaker,
        Self::SpeakerMuted,
        Self::Stopwatch,
        Self::Trash,
        Self::Unlock,
        Self::Warning,
        Self::ZoomIn,
        Self::ZoomOut,
    ];

    /// Stable cache id for the icon.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Add => "app.add",
            Self::Anchor => "app.anchor",
            Self::ArrowDown => "app.arrow-down",
            Self::ArrowRight => "app.arrow-right",
            Self::ArrowUp => "app.arrow-up",
            Self::Circle => "app.circle",
            Self::ClipboardText => "app.clipboard-text",
            Self::Clock => "app.clock",
            Self::Copy => "app.copy",
            Self::Cursor => "app.cursor",
            Self::CursorFilled => "app.cursor-filled",
            Self::Cut => "app.cut",
            Self::EndFrameFilled => "app.end-frame-filled",
            Self::Effect => "app.effect",
            Self::Export => "app.export",
            Self::EyeHidden => "app.eye-hidden",
            Self::EyeVisible => "app.eye-visible",
            Self::Film => "app.film",
            Self::Folder => "app.folder",
            Self::FolderOpenFilled => "app.folder-open-filled",
            Self::FullScreen => "app.full-screen",
            Self::Grid => "app.grid",
            Self::HomeFrameFilled => "app.home-frame-filled",
            Self::Import => "app.import",
            Self::Info => "app.info",
            Self::LeftFrameFilled => "app.left-frame-filled",
            Self::List => "app.list",
            Self::Lock => "app.lock",
            Self::Magnet => "app.magnet",
            Self::Music => "app.music",
            Self::PauseFilled => "app.pause-filled",
            Self::Pen => "app.pen",
            Self::PlayFilled => "app.play-filled",
            Self::PlusFilled => "app.plus-filled",
            Self::Rectangle => "app.rectangle",
            Self::RightFrameFilled => "app.right-frame-filled",
            Self::Search => "app.search",
            Self::Save => "app.save",
            Self::Speaker => "app.speaker",
            Self::SpeakerMuted => "app.speaker-muted",
            Self::Stopwatch => "app.stopwatch",
            Self::Trash => "app.trash",
            Self::Unlock => "app.unlock",
            Self::Warning => "app.warning",
            Self::ZoomIn => "app.zoom-in",
            Self::ZoomOut => "app.zoom-out",
        }
    }

    /// Raw bundled SVG source.
    pub const fn svg(self) -> &'static str {
        match self {
            Self::Add => include_str!("../../assets/icons/add.svg"),
            Self::Anchor => include_str!("../../assets/icons/anchor.svg"),
            Self::ArrowDown => include_str!("../../assets/icons/arrow_down.svg"),
            Self::ArrowRight => include_str!("../../assets/icons/arrow_right.svg"),
            Self::ArrowUp => include_str!("../../assets/icons/arrow_up.svg"),
            Self::Circle => include_str!("../../assets/icons/circle.svg"),
            Self::ClipboardText => include_str!("../../assets/icons/clipboard_text.svg"),
            Self::Clock => include_str!("../../assets/icons/clock.svg"),
            Self::Copy => include_str!("../../assets/icons/copy.svg"),
            Self::Cursor => include_str!("../../assets/icons/cursor.svg"),
            Self::CursorFilled => include_str!("../../assets/icons/cursor_fill.svg"),
            Self::Cut => include_str!("../../assets/icons/cut.svg"),
            Self::EndFrameFilled => include_str!("../../assets/icons/end_frame_fill.svg"),
            Self::Effect => include_str!("../../assets/icons/effect.svg"),
            Self::Export => include_str!("../../assets/icons/export.svg"),
            Self::EyeHidden => include_str!("../../assets/icons/eye_invisiable.svg"),
            Self::EyeVisible => include_str!("../../assets/icons/eye_visiable.svg"),
            Self::Film => include_str!("../../assets/icons/film.svg"),
            Self::Folder => include_str!("../../assets/icons/folder.svg"),
            Self::FolderOpenFilled => include_str!("../../assets/icons/folder_open_fill.svg"),
            Self::FullScreen => include_str!("../../assets/icons/full_Screen.svg"),
            Self::Grid => include_str!("../../assets/icons/grid.svg"),
            Self::HomeFrameFilled => include_str!("../../assets/icons/home_frame_fill.svg"),
            Self::Import => include_str!("../../assets/icons/import.svg"),
            Self::Info => include_str!("../../assets/icons/info.svg"),
            Self::LeftFrameFilled => include_str!("../../assets/icons/left_frame_fill.svg"),
            Self::List => include_str!("../../assets/icons/list.svg"),
            Self::Lock => include_str!("../../assets/icons/lock.svg"),
            Self::Magnet => include_str!("../../assets/icons/magnet.svg"),
            Self::Music => include_str!("../../assets/icons/music.svg"),
            Self::PauseFilled => include_str!("../../assets/icons/pause_fill.svg"),
            Self::Pen => include_str!("../../assets/icons/pen.svg"),
            Self::PlayFilled => include_str!("../../assets/icons/play_fill.svg"),
            Self::PlusFilled => include_str!("../../assets/icons/plus_fill.svg"),
            Self::Rectangle => include_str!("../../assets/icons/rectangle.svg"),
            Self::RightFrameFilled => include_str!("../../assets/icons/right_frame_fill.svg"),
            Self::Search => include_str!("../../assets/icons/search.svg"),
            Self::Save => include_str!("../../assets/icons/save.svg"),
            Self::Speaker => include_str!("../../assets/icons/speaker.svg"),
            Self::SpeakerMuted => include_str!("../../assets/icons/speaker_muted.svg"),
            Self::Stopwatch => include_str!("../../assets/icons/stopwatch.svg"),
            Self::Trash => include_str!("../../assets/icons/trash.svg"),
            Self::Unlock => include_str!("../../assets/icons/unlock.svg"),
            Self::Warning => include_str!("../../assets/icons/warning.svg"),
            Self::ZoomIn => include_str!("../../assets/icons/zoom_in.svg"),
            Self::ZoomOut => include_str!("../../assets/icons/zoom_out.svg"),
        }
    }

    /// Parse the icon into cached vector geometry for custom UI painting.
    pub fn vector_icon(self) -> Result<VectorIcon, VectorIconError> {
        VectorIcon::from_static_svg(self.id(), self.svg())
    }

    /// Build a theme-tinted icon button for this asset.
    pub fn icon_button(self) -> Result<IconButton, VectorIconError> {
        Ok(IconButton::from_vector_icon(self.vector_icon()?))
    }

    /// Build a text button with this asset painted before the label.
    pub fn text_button(self, label: impl Into<String>) -> Result<Button, VectorIconError> {
        Ok(Button::new(label).with_leading_icon(self.vector_icon()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_ui_core::Widget;
    use std::collections::HashSet;

    #[test]
    fn all_self_hosted_icon_assets_parse_as_vector_icons() {
        for icon in AppIcon::ALL {
            let vector = icon
                .vector_icon()
                .unwrap_or_else(|err| panic!("{icon:?} failed to parse: {err}"));

            assert!(vector.triangle_count() > 0, "{icon:?} has no triangles");
        }
    }

    #[test]
    fn self_hosted_icon_ids_are_unique() {
        let mut ids = HashSet::new();

        for icon in AppIcon::ALL {
            assert!(ids.insert(icon.id()), "duplicate icon id {}", icon.id());
        }
    }

    #[test]
    fn self_hosted_icons_build_text_buttons() {
        let button = AppIcon::Trash.text_button("Remove").expect("trash text button");

        assert!(button.measure(mondrian_ui_core::types::LayoutConstraint::LOOSE).width > 0.0);
    }
}
