//! Persistable workspace layout model for the self-hosted shell.
//!
//! Widgets own live event handling and paint state. This module owns the
//! app-shell schema used to save and restore dock structure across sessions.

use mondrian_editor_state::state::PanelKind;
use mondrian_ui_core::types::SplitDirection;
use mondrian_ui_core::Widget;
use mondrian_ui_widgets::dock_panel::DockPanel;
use mondrian_ui_widgets::dock_splitter::DockSplitter;
use serde::{Deserialize, Serialize};

const MIN_SPLIT_RATIO: f32 = 0.1;
const MAX_SPLIT_RATIO: f32 = 0.9;

/// Persistable self-hosted workspace dock tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SelfHostedWorkspaceLayout {
    /// A binary dock split.
    Split {
        direction: SplitDirection,
        ratio: f32,
        first: Box<SelfHostedWorkspaceLayout>,
        second: Box<SelfHostedWorkspaceLayout>,
    },
    /// A dock panel slot and its active tab.
    Panel {
        kind: PanelKind,
        active_index: usize,
        #[serde(default)]
        hidden_tabs: Vec<PanelKind>,
    },
}

impl SelfHostedWorkspaceLayout {
    /// Capture the current dock tree as a persistable layout model.
    pub fn from_dock(dock: &DockSplitter) -> Option<Self> {
        Self::from_widget(dock)
    }

    /// Return a sanitized layout, clamping ratios and tab indices.
    pub fn sanitized(self) -> Option<Self> {
        match self {
            Self::Split { direction, ratio, first, second } => Some(Self::Split {
                direction,
                ratio: sanitize_ratio(ratio),
                first: Box::new(first.sanitized()?),
                second: Box::new(second.sanitized()?),
            }),
            Self::Panel { kind, active_index, hidden_tabs } => {
                let hidden_tabs = sanitize_hidden_tabs(kind, hidden_tabs);
                Some(Self::Panel {
                    kind,
                    active_index: sanitize_active_index(kind, active_index, &hidden_tabs),
                    hidden_tabs,
                })
            }
        }
    }

    /// Whether this layout contains a visible direct dock panel leaf or grouped
    /// tab.
    pub fn contains_panel(&self, panel: PanelKind) -> bool {
        match self {
            Self::Split { first, second, .. } => {
                first.contains_panel(panel) || second.contains_panel(panel)
            }
            Self::Panel { kind, hidden_tabs, .. } => {
                *kind == panel
                    || grouped_tab_owner(panel) == Some(*kind) && !hidden_tabs.contains(&panel)
            }
        }
    }

    /// Remove a direct dock panel leaf and collapse now-empty split branches.
    ///
    /// Returns `None` when the requested panel was the only remaining leaf.
    pub fn without_panel(self, panel: PanelKind) -> Option<Self> {
        match self {
            Self::Panel { kind, .. } if kind == panel => None,
            Self::Panel { kind, active_index, mut hidden_tabs }
                if grouped_tab_owner(panel) == Some(kind) =>
            {
                if !hidden_tabs.contains(&panel) {
                    hidden_tabs.push(panel);
                }
                let hidden_tabs = sanitize_hidden_tabs(kind, hidden_tabs);
                Some(Self::Panel {
                    kind,
                    active_index: sanitize_active_index(kind, active_index, &hidden_tabs),
                    hidden_tabs,
                })
            }
            Self::Panel { .. } => Some(self),
            Self::Split { direction, ratio, first, second } => {
                let first = first.without_panel(panel);
                let second = second.without_panel(panel);
                match (first, second) {
                    (Some(first), Some(second)) => Some(Self::Split {
                        direction,
                        ratio: sanitize_ratio(ratio),
                        first: Box::new(first),
                        second: Box::new(second),
                    }),
                    (Some(only), None) | (None, Some(only)) => Some(only),
                    (None, None) => None,
                }
            }
        }
    }

    /// Whether this layout can be materialized as the root dock splitter.
    pub fn is_split_root(&self) -> bool {
        matches!(self, Self::Split { .. })
    }

    /// Preserve panel-level metadata that cannot be inferred from the live
    /// widget tree, such as hidden grouped tabs.
    pub fn with_panel_metadata_from(mut self, previous: &Self) -> Self {
        self.merge_panel_metadata_from(previous);
        self
    }

    fn from_widget(widget: &dyn Widget) -> Option<Self> {
        if let Some(splitter) = widget.as_any().and_then(|any| any.downcast_ref::<DockSplitter>()) {
            let first = splitter.child(0).and_then(Self::from_widget)?;
            let second = splitter.child(1).and_then(Self::from_widget)?;
            return Some(Self::Split {
                direction: splitter.direction(),
                ratio: sanitize_ratio(splitter.ratio()),
                first: Box::new(first),
                second: Box::new(second),
            });
        }

        widget
            .as_any()
            .and_then(|any| any.downcast_ref::<DockPanel>())
            .map(|panel| Self::Panel {
                kind: panel.kind(),
                active_index: sanitize_active_index(panel.kind(), panel.active_index(), &[]),
                hidden_tabs: Vec::new(),
            })
    }

    fn merge_panel_metadata_from(&mut self, previous: &Self) {
        match self {
            Self::Split { first, second, .. } => {
                first.merge_panel_metadata_from(previous);
                second.merge_panel_metadata_from(previous);
            }
            Self::Panel { kind, active_index, hidden_tabs } => {
                if let Some(previous_hidden) = previous.hidden_tabs_for_panel(*kind) {
                    *hidden_tabs = sanitize_hidden_tabs(*kind, previous_hidden.to_vec());
                    *active_index = sanitize_active_index(*kind, *active_index, hidden_tabs);
                }
            }
        }
    }

    fn hidden_tabs_for_panel(&self, panel: PanelKind) -> Option<&[PanelKind]> {
        match self {
            Self::Split { first, second, .. } => first
                .hidden_tabs_for_panel(panel)
                .or_else(|| second.hidden_tabs_for_panel(panel)),
            Self::Panel { kind, hidden_tabs, .. } if *kind == panel => Some(hidden_tabs),
            Self::Panel { .. } => None,
        }
    }
}

fn sanitize_ratio(ratio: f32) -> f32 {
    if ratio.is_finite() {
        ratio.clamp(MIN_SPLIT_RATIO, MAX_SPLIT_RATIO)
    } else {
        0.5
    }
}

fn sanitize_active_index(kind: PanelKind, active_index: usize, hidden_tabs: &[PanelKind]) -> usize {
    let visible_tabs = visible_tabs_for_panel(kind, hidden_tabs);
    active_index.min(visible_tabs.len().saturating_sub(1))
}

fn sanitize_hidden_tabs(kind: PanelKind, hidden_tabs: Vec<PanelKind>) -> Vec<PanelKind> {
    let mut sanitized = Vec::new();
    for tab in hidden_tabs {
        if grouped_tab_owner(tab) == Some(kind) && !sanitized.contains(&tab) {
            sanitized.push(tab);
        }
    }
    sanitized
}

fn visible_tabs_for_panel(kind: PanelKind, hidden_tabs: &[PanelKind]) -> Vec<PanelKind> {
    panel_tabs(kind)
        .iter()
        .copied()
        .filter(|tab| *tab == kind || !hidden_tabs.contains(tab))
        .collect()
}

fn grouped_tab_owner(panel: PanelKind) -> Option<PanelKind> {
    match panel {
        PanelKind::Effects => Some(PanelKind::Assets),
        _ => None,
    }
}

fn panel_tabs(kind: PanelKind) -> &'static [PanelKind] {
    match kind {
        PanelKind::Assets => &[PanelKind::Assets, PanelKind::Effects],
        PanelKind::Viewer => &[PanelKind::Viewer],
        PanelKind::Timeline => &[PanelKind::Timeline],
        PanelKind::Inspector => &[PanelKind::Inspector],
        PanelKind::Effects => &[PanelKind::Effects],
        PanelKind::NodeGraph => &[PanelKind::NodeGraph],
        PanelKind::Export => &[PanelKind::Export],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_ui_widgets::dock_splitter::DockSplitter;

    fn empty_panel(kind: PanelKind) -> Box<dyn Widget> {
        Box::new(DockPanel::new(kind, vec![], move |_kind, _active| {
            Box::new(EmptyWidget::default())
        }))
    }

    fn layout_panel(kind: PanelKind, active_index: usize) -> SelfHostedWorkspaceLayout {
        SelfHostedWorkspaceLayout::Panel { kind, active_index, hidden_tabs: Vec::new() }
    }

    #[derive(Default)]
    struct EmptyWidget {
        id: mondrian_ui_core::types::WidgetId,
    }

    impl Widget for EmptyWidget {
        fn id(&self) -> mondrian_ui_core::types::WidgetId {
            self.id
        }

        fn measure(
            &self,
            constraint: mondrian_ui_core::types::LayoutConstraint,
        ) -> mondrian_ui_core::types::Size {
            constraint.constrain(mondrian_ui_core::types::Size::ZERO)
        }

        fn layout(&mut self, _bounds: mondrian_ui_core::types::Rect) {}

        fn event(
            &mut self,
            _event: &mondrian_ui_core::UiEvent,
            _ctx: &mut mondrian_ui_core::widget::EventContext,
        ) -> mondrian_ui_core::EventResult {
            mondrian_ui_core::EventResult::Ignored
        }

        fn paint(&self, _ctx: &mut mondrian_ui_core::widget::PaintContext) {}
    }

    #[test]
    fn captures_nested_dock_splitters_and_panel_tabs() {
        let dock = DockSplitter::new(
            SplitDirection::Horizontal,
            0.42,
            empty_panel(PanelKind::Assets),
            empty_panel(PanelKind::Viewer),
        );

        let layout = SelfHostedWorkspaceLayout::from_dock(&dock).expect("layout");

        assert_eq!(
            layout,
            SelfHostedWorkspaceLayout::Split {
                direction: SplitDirection::Horizontal,
                ratio: 0.42,
                first: Box::new(layout_panel(PanelKind::Assets, 0)),
                second: Box::new(layout_panel(PanelKind::Viewer, 0)),
            }
        );
    }

    #[test]
    fn sanitizes_ratios_and_panel_tab_indices() {
        let layout = SelfHostedWorkspaceLayout::Split {
            direction: SplitDirection::Vertical,
            ratio: f32::NAN,
            first: Box::new(layout_panel(PanelKind::Assets, 12)),
            second: Box::new(layout_panel(PanelKind::Timeline, 8)),
        }
        .sanitized()
        .expect("sanitized");

        assert_eq!(
            layout,
            SelfHostedWorkspaceLayout::Split {
                direction: SplitDirection::Vertical,
                ratio: 0.5,
                first: Box::new(layout_panel(PanelKind::Assets, 1)),
                second: Box::new(layout_panel(PanelKind::Timeline, 0)),
            }
        );
    }

    #[test]
    fn removing_panel_collapses_empty_split_branches() {
        let layout = SelfHostedWorkspaceLayout::Split {
            direction: SplitDirection::Vertical,
            ratio: 0.66,
            first: Box::new(SelfHostedWorkspaceLayout::Split {
                direction: SplitDirection::Horizontal,
                ratio: 0.3,
                first: Box::new(layout_panel(PanelKind::Assets, 0)),
                second: Box::new(layout_panel(PanelKind::Viewer, 0)),
            }),
            second: Box::new(layout_panel(PanelKind::Timeline, 0)),
        };

        let without_viewer = layout.without_panel(PanelKind::Viewer).expect("remaining layout");

        assert!(without_viewer.contains_panel(PanelKind::Assets));
        assert!(without_viewer.contains_panel(PanelKind::Timeline));
        assert!(!without_viewer.contains_panel(PanelKind::Viewer));
        assert!(without_viewer.is_split_root());
    }

    #[test]
    fn removing_grouped_tab_hides_it_without_removing_owner_panel() {
        let layout = layout_panel(PanelKind::Assets, 1);

        let without_effects = layout.without_panel(PanelKind::Effects).expect("assets remain");

        assert!(without_effects.contains_panel(PanelKind::Assets));
        assert!(!without_effects.contains_panel(PanelKind::Effects));
        assert_eq!(
            without_effects,
            SelfHostedWorkspaceLayout::Panel {
                kind: PanelKind::Assets,
                active_index: 0,
                hidden_tabs: vec![PanelKind::Effects],
            }
        );
    }

    #[test]
    fn preserves_hidden_grouped_tabs_when_merging_live_panel_metadata() {
        let live = layout_panel(PanelKind::Assets, 1);
        let previous = SelfHostedWorkspaceLayout::Panel {
            kind: PanelKind::Assets,
            active_index: 0,
            hidden_tabs: vec![PanelKind::Effects],
        };

        let merged = live.with_panel_metadata_from(&previous);

        assert_eq!(
            merged,
            SelfHostedWorkspaceLayout::Panel {
                kind: PanelKind::Assets,
                active_index: 0,
                hidden_tabs: vec![PanelKind::Effects],
            }
        );
    }

    #[test]
    fn removing_only_panel_returns_none() {
        let layout = layout_panel(PanelKind::Viewer, 0);

        assert_eq!(layout.without_panel(PanelKind::Viewer), None);
    }
}
