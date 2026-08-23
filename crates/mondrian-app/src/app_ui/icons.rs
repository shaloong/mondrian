//! Product icon assets for the app UI shell.
//!
//! This module is the app-layer registry for bundled designer-authored SVG
//! assets. Reusable widgets stay asset-agnostic; panels ask this registry for
//! vector geometry or icon buttons when they need Mondrian product icons.

use mondrian_ui_widgets::{Button, IconButton, VectorIcon, VectorIconError};

/// Built-in product icons available to the app UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppIcon {
    /// Add item outline icon.
    Add,
    /// Anchor point icon.
    Anchor,
    /// Left bracket in/out range icon.
    BracketsLeft,
    /// Right bracket in/out range icon.
    BracketsRight,
    /// Down caret icon.
    CaretDown,
    /// Left caret icon.
    CaretLeft,
    /// Right caret icon.
    CaretRight,
    /// Up caret icon.
    CaretUp,
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
    /// Eyedropper color sampler icon.
    Eyedropper,
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
    /// Redo icon.
    Redo,
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
    /// Undo icon.
    Undo,
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
    pub const ALL: [Self; 52] = [
        Self::Add,
        Self::Anchor,
        Self::BracketsLeft,
        Self::BracketsRight,
        Self::CaretDown,
        Self::CaretLeft,
        Self::CaretRight,
        Self::CaretUp,
        Self::Circle,
        Self::ClipboardText,
        Self::Clock,
        Self::Copy,
        Self::Cursor,
        Self::CursorFilled,
        Self::Cut,
        Self::Eyedropper,
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
        Self::Redo,
        Self::RightFrameFilled,
        Self::Search,
        Self::Save,
        Self::Speaker,
        Self::SpeakerMuted,
        Self::Stopwatch,
        Self::Trash,
        Self::Undo,
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
            Self::BracketsLeft => "app.brackets-left",
            Self::BracketsRight => "app.brackets-right",
            Self::CaretDown => "app.caret-down",
            Self::CaretLeft => "app.caret-left",
            Self::CaretRight => "app.caret-right",
            Self::CaretUp => "app.caret-up",
            Self::Circle => "app.circle",
            Self::ClipboardText => "app.clipboard-text",
            Self::Clock => "app.clock",
            Self::Copy => "app.copy",
            Self::Cursor => "app.cursor",
            Self::CursorFilled => "app.cursor-filled",
            Self::Cut => "app.cut",
            Self::Eyedropper => "app.eyedropper",
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
            Self::Redo => "app.redo",
            Self::RightFrameFilled => "app.right-frame-filled",
            Self::Search => "app.search",
            Self::Save => "app.save",
            Self::Speaker => "app.speaker",
            Self::SpeakerMuted => "app.speaker-muted",
            Self::Stopwatch => "app.stopwatch",
            Self::Trash => "app.trash",
            Self::Undo => "app.undo",
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
            Self::BracketsLeft => include_str!("../../assets/icons/brackets_left.svg"),
            Self::BracketsRight => include_str!("../../assets/icons/brackets_right.svg"),
            Self::CaretDown => include_str!("../../assets/icons/caret_down.svg"),
            Self::CaretLeft => include_str!("../../assets/icons/caret_left.svg"),
            Self::CaretRight => include_str!("../../assets/icons/caret_right.svg"),
            Self::CaretUp => include_str!("../../assets/icons/caret_up.svg"),
            Self::Circle => include_str!("../../assets/icons/circle.svg"),
            Self::ClipboardText => include_str!("../../assets/icons/clipboard_text.svg"),
            Self::Clock => include_str!("../../assets/icons/clock.svg"),
            Self::Copy => include_str!("../../assets/icons/copy.svg"),
            Self::Cursor => include_str!("../../assets/icons/cursor.svg"),
            Self::CursorFilled => include_str!("../../assets/icons/cursor_fill.svg"),
            Self::Cut => include_str!("../../assets/icons/cut.svg"),
            Self::Eyedropper => include_str!("../../assets/icons/eyedropper.svg"),
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
            Self::Redo => include_str!("../../assets/icons/redo.svg"),
            Self::RightFrameFilled => include_str!("../../assets/icons/right_frame_fill.svg"),
            Self::Search => include_str!("../../assets/icons/search.svg"),
            Self::Save => include_str!("../../assets/icons/save.svg"),
            Self::Speaker => include_str!("../../assets/icons/speaker.svg"),
            Self::SpeakerMuted => include_str!("../../assets/icons/speaker_muted.svg"),
            Self::Stopwatch => include_str!("../../assets/icons/stopwatch.svg"),
            Self::Trash => include_str!("../../assets/icons/trash.svg"),
            Self::Undo => include_str!("../../assets/icons/undo.svg"),
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

    /// Build a text button and omit the icon if the bundled SVG cannot parse.
    pub fn text_button_or_label(self, label: impl Into<String>) -> Button {
        let label = label.into();
        match self.text_button(label.clone()) {
            Ok(button) => button,
            Err(_) => Button::new(label),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_ui_core::Widget;
    use std::collections::HashSet;

    #[test]
    fn all_app_ui_icon_assets_parse_as_vector_icons() {
        for icon in AppIcon::ALL {
            let vector = icon
                .vector_icon()
                .unwrap_or_else(|err| panic!("{icon:?} failed to parse: {err}"));

            assert!(vector.triangle_count() > 0, "{icon:?} has no triangles");
        }
    }

    #[test]
    fn app_ui_icon_ids_are_unique() {
        let mut ids = HashSet::new();

        for icon in AppIcon::ALL {
            assert!(ids.insert(icon.id()), "duplicate icon id {}", icon.id());
        }
    }

    #[test]
    fn eyedropper_uses_canonical_bundled_svg_filename() {
        let icons_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets").join("icons");
        let mut eyedropper_files = std::fs::read_dir(&icons_dir)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", icons_dir.display()))
            .map(|entry| {
                entry
                    .unwrap_or_else(|err| panic!("failed to read icon entry: {err}"))
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.starts_with("eyedropper"))
            .collect::<Vec<_>>();
        eyedropper_files.sort();

        assert_eq!(eyedropper_files, ["eyedropper.svg"]);
        assert!(AppIcon::Eyedropper.vector_icon().is_ok());
    }

    #[test]
    fn app_ui_icons_build_text_buttons() {
        let button = AppIcon::Trash.text_button("Remove").expect("trash text button");

        assert!(button.measure(mondrian_ui_core::types::LayoutConstraint::LOOSE).width > 0.0);
    }

    #[test]
    fn app_ui_icons_build_lossy_text_buttons() {
        let button = AppIcon::Trash.text_button_or_label("Remove");

        assert!(button.measure(mondrian_ui_core::types::LayoutConstraint::LOOSE).width > 0.0);
    }
}
