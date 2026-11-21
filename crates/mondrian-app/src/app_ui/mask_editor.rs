//! Session-local Viewer mask controls and stale gesture validation.

use std::collections::HashMap;

use mondrian_core::mask_data::MaskShape;
use mondrian_core::{ClipId, Color, MaskId, TimelineTime};
use mondrian_editor_state::{Action, AuthoringSessionId};
use mondrian_ui_widgets::{ViewerMaskOverlayStyle, ViewerMaskSelectionMode};
use serde::{Deserialize, Serialize};

use crate::app::AppState;

use crate::app::ui_actions::APP_SHELL_NAMESPACE as NAMESPACE;
const NAME: &str = "viewer_mask_editor";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MaskEditorTarget {
    pub session: AuthoringSessionId,
    pub clip_id: ClipId,
    pub mask_id: MaskId,
}

impl MaskEditorTarget {
    pub fn selected(state: &AppState) -> Option<Self> {
        let (mask_id, clip_id, _) = state.primary_selected_mask()?;
        let clip = state.primary_selected_clip()?;
        (clip.is_video_track && clip.clip_id == clip_id).then_some(Self {
            session: state.authoring_session_id()?,
            clip_id,
            mask_id,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AnchorSelection {
    #[default]
    Rectangle,
    Lasso,
}

impl AnchorSelection {
    pub const ALL: [Self; 2] = [Self::Rectangle, Self::Lasso];

    pub const fn message_id(self) -> &'static str {
        match self {
            Self::Rectangle => "mask-editor-selection-rectangle",
            Self::Lasso => "mask-editor-selection-lasso",
        }
    }

    pub const fn viewer_mode(self) -> ViewerMaskSelectionMode {
        match self {
            Self::Rectangle => ViewerMaskSelectionMode::Rectangle,
            Self::Lasso => ViewerMaskSelectionMode::Lasso,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum OverlayColor {
    #[default]
    Theme,
    Blue,
    Green,
    Chartreuse,
    Yellow,
    Orange,
    Red,
    Magenta,
    Purple,
    Indigo,
    Fuchsia,
    Seafoam,
}

impl OverlayColor {
    pub const ALL: [Self; 12] = [
        Self::Theme,
        Self::Blue,
        Self::Green,
        Self::Chartreuse,
        Self::Yellow,
        Self::Orange,
        Self::Red,
        Self::Magenta,
        Self::Purple,
        Self::Indigo,
        Self::Fuchsia,
        Self::Seafoam,
    ];

    const fn metadata(self) -> (&'static str, Option<u32>) {
        let (id, rgb) = match self {
            Self::Theme => return ("mask-editor-color-theme", None),
            Self::Blue => ("mask-editor-color-blue", 0x60A5FA),
            Self::Green => ("mask-editor-color-green", 0x4ADE80),
            Self::Chartreuse => ("mask-editor-color-chartreuse", 0xBEF264),
            Self::Yellow => ("mask-editor-color-yellow", 0xFDE047),
            Self::Orange => ("mask-editor-color-orange", 0xFB923C),
            Self::Red => ("mask-editor-color-red", 0xF87171),
            Self::Magenta => ("mask-editor-color-magenta", 0xDB2777),
            Self::Purple => ("mask-editor-color-purple", 0xA855F7),
            Self::Indigo => ("mask-editor-color-indigo", 0x818CF8),
            Self::Fuchsia => ("mask-editor-color-fuchsia", 0xE879F9),
            Self::Seafoam => ("mask-editor-color-seafoam", 0x5EEAD4),
        };
        (id, Some(rgb))
    }

    pub fn message_id(self) -> &'static str {
        self.metadata().0
    }

    fn color(self) -> Option<Color> {
        self.metadata().1.map(Color::from_hex)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MaskEditorSettings {
    pub selection: AnchorSelection,
    pub color: OverlayColor,
    pub fill_opacity: u8,
    pub outline: bool,
}

impl Default for MaskEditorSettings {
    fn default() -> Self {
        Self {
            selection: AnchorSelection::Rectangle,
            color: OverlayColor::Theme,
            fill_opacity: 0,
            outline: true,
        }
    }
}

impl MaskEditorSettings {
    pub fn viewer_style(self) -> ViewerMaskOverlayStyle {
        ViewerMaskOverlayStyle {
            color: self.color.color(),
            fill_opacity_percent: self.fill_opacity,
            show_outline: self.outline,
        }
    }

    fn apply(&mut self, setting: MaskEditorSetting) {
        match setting {
            MaskEditorSetting::Selection(selection) => self.selection = selection,
            MaskEditorSetting::Color(color) => self.color = color,
            MaskEditorSetting::FillOpacity(opacity) if opacity <= 100 => {
                self.fill_opacity = opacity
            }
            MaskEditorSetting::FillOpacity(_) => {}
            MaskEditorSetting::Outline(outline) => self.outline = outline,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum MaskEditorSetting {
    Selection(AnchorSelection),
    Color(OverlayColor),
    FillOpacity(u8),
    Outline(bool),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum MaskEditorEdit {
    Presentation(MaskEditorSetting),
    WriteShape {
        author_time: TimelineTime,
        original: MaskShape,
        shape: MaskShape,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MaskEditorAction {
    pub target: MaskEditorTarget,
    pub edit: MaskEditorEdit,
}

impl MaskEditorAction {
    pub fn action(target: MaskEditorTarget, edit: MaskEditorEdit) -> Action {
        Action::Custom {
            namespace: NAMESPACE.into(),
            name: NAME.into(),
            payload: serde_json::to_value(Self { target, edit })
                .expect("Viewer mask action is serializable"),
        }
    }

    /// Outer `Some` consumes this protocol even when its payload is invalid.
    pub fn decode(action: &Action) -> Option<Option<Self>> {
        let Action::Custom { namespace, name, payload } = action else {
            return None;
        };
        (namespace == NAMESPACE && name == NAME)
            .then(|| serde_json::from_value(payload.clone()).ok())
    }
}

#[derive(Default)]
pub(super) struct MaskEditorSession {
    session: Option<AuthoringSessionId>,
    settings: HashMap<(ClipId, MaskId), MaskEditorSettings>,
}

impl MaskEditorSession {
    pub fn sync(&mut self, session: Option<AuthoringSessionId>) {
        if self.session != session {
            self.session = session;
            self.settings.clear();
        }
    }

    pub fn settings(&self, clip_id: ClipId, mask_id: MaskId) -> MaskEditorSettings {
        self.settings.get(&(clip_id, mask_id)).copied().unwrap_or_default()
    }

    pub fn update(&mut self, target: MaskEditorTarget, setting: MaskEditorSetting) -> bool {
        self.sync(Some(target.session));
        let key = (target.clip_id, target.mask_id);
        let before = self.settings(key.0, key.1);
        let mut after = before;
        after.apply(setting);
        if before == after {
            return false;
        }
        if after == MaskEditorSettings::default() {
            self.settings.remove(&key);
        } else {
            self.settings.insert(key, after);
        }
        true
    }
}
