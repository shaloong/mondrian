//! Stable UI vocabulary shared by the application shell and widget crates.
//!
//! This Module deliberately contains no authored Project, transport, selection,
//! or Undo state. Canonical project authoring belongs to `AuthoringSession`;
//! UI adapters consume purpose-built immutable view models.

use serde::{Deserialize, Serialize};

/// Stable panel identity used by docking, focus, shortcuts, and persisted UI layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PanelKind {
    Viewer,
    Scopes,
    Timeline,
    Assets,
    Inspector,
    Effects,
    NodeGraph,
    Export,
}

impl PanelKind {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Viewer => "预览",
            Self::Scopes => "示波器",
            Self::Timeline => "时间线",
            Self::Assets => "素材",
            Self::Inspector => "检查器",
            Self::Effects => "效果",
            Self::NodeGraph => "节点图",
            Self::Export => "导出",
        }
    }

    pub fn icon_name(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Scopes => "scopes",
            Self::Timeline => "timeline",
            Self::Assets => "assets",
            Self::Inspector => "inspector",
            Self::Effects => "effects",
            Self::NodeGraph => "node_graph",
            Self::Export => "export",
        }
    }

    pub const ALL: [Self; 8] = [
        Self::Viewer,
        Self::Scopes,
        Self::Timeline,
        Self::Assets,
        Self::Inspector,
        Self::Effects,
        Self::NodeGraph,
        Self::Export,
    ];
}

/// Stable workspace-layout preset identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WorkspacePreset {
    #[default]
    Editing,
    Color,
    Audio,
    Compositing,
    Export,
    Custom,
}

impl WorkspacePreset {
    pub const ALL: [Self; 6] = [
        Self::Editing,
        Self::Color,
        Self::Audio,
        Self::Compositing,
        Self::Export,
        Self::Custom,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Editing => "编辑",
            Self::Color => "调色",
            Self::Audio => "音频",
            Self::Compositing => "合成",
            Self::Export => "导出",
            Self::Custom => "自定义",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn panel_kind_contract_is_unique_and_serializable() {
        let mut identities = HashSet::new();
        let mut labels = HashSet::new();
        for kind in PanelKind::ALL {
            assert!(identities.insert(kind));
            assert!(labels.insert(kind.display_name()));
            assert!(!kind.icon_name().is_empty());
            let encoded = serde_json::to_string(&kind).expect("serialize panel kind");
            assert_eq!(
                serde_json::from_str::<PanelKind>(&encoded).expect("deserialize panel kind"),
                kind
            );
        }
    }

    #[test]
    fn workspace_preset_contract_is_unique_and_serializable() {
        assert_eq!(WorkspacePreset::default(), WorkspacePreset::Editing);
        for preset in WorkspacePreset::ALL {
            assert!(!preset.display_name().is_empty());
            let encoded = serde_json::to_string(&preset).expect("serialize workspace preset");
            assert_eq!(
                serde_json::from_str::<WorkspacePreset>(&encoded)
                    .expect("deserialize workspace preset"),
                preset
            );
        }
    }
}
