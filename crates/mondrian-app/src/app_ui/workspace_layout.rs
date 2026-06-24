//! Persistable workspace layout model for the app UI shell.
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

/// Where a docked panel should be inserted relative to another panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockDropArea {
    /// Add as another tab in the target panel group.
    Center,
    /// Split to the left of the target group.
    Left,
    /// Split to the right of the target group.
    Right,
    /// Split above the target group.
    Top,
    /// Split below the target group.
    Bottom,
}

/// Persistable app UI workspace dock tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AppUiWorkspaceLayout {
    /// A binary dock split.
    Split {
        direction: SplitDirection,
        ratio: f32,
        first: Box<AppUiWorkspaceLayout>,
        second: Box<AppUiWorkspaceLayout>,
    },
    /// A dock panel slot and its active tab.
    Panel {
        kind: PanelKind,
        active_index: usize,
        #[serde(default)]
        hidden_tabs: Vec<PanelKind>,
        #[serde(default)]
        tabs: Vec<PanelKind>,
    },
}

impl AppUiWorkspaceLayout {
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
            Self::Panel { kind, active_index, hidden_tabs, tabs } => {
                let tabs = sanitize_panel_tabs(kind, hidden_tabs.as_slice(), tabs);
                let hidden_tabs = sanitize_hidden_tabs(kind, hidden_tabs);
                Some(Self::Panel {
                    kind: tabs.first().copied().unwrap_or(kind),
                    active_index: sanitize_active_index_for_tabs(active_index, &tabs),
                    hidden_tabs,
                    tabs,
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
            Self::Panel { kind, hidden_tabs, tabs, .. } => {
                panel_tabs_for_leaf(*kind, hidden_tabs, tabs).contains(&panel)
            }
        }
    }

    /// Remove a direct dock panel leaf and collapse now-empty split branches.
    ///
    /// Returns `None` when the requested panel was the only remaining leaf.
    pub fn without_panel(self, panel: PanelKind) -> Option<Self> {
        match self {
            Self::Panel { kind, active_index, hidden_tabs, tabs } => {
                let mut tabs = panel_tabs_for_leaf(kind, &hidden_tabs, &tabs);
                let Some(removed_index) = tabs.iter().position(|tab| *tab == panel) else {
                    return Some(Self::Panel { kind, active_index, hidden_tabs, tabs });
                };
                tabs.remove(removed_index);
                if tabs.is_empty() {
                    return None;
                }
                let next_active = if removed_index < active_index {
                    active_index.saturating_sub(1)
                } else {
                    active_index.min(tabs.len().saturating_sub(1))
                };
                Some(Self::Panel {
                    kind: tabs[0],
                    active_index: next_active,
                    hidden_tabs: Vec::new(),
                    tabs,
                })
            }
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

    /// Move an existing panel to another dock target.
    ///
    /// This is the persistable layout mutation used by future pointer DnD:
    /// dropping in the center creates a tab group; dropping on an edge creates
    /// a split around the target group.
    pub fn relocate_panel(
        self,
        panel: PanelKind,
        target: PanelKind,
        area: DockDropArea,
    ) -> Option<Self> {
        let target = self.resolve_drop_target(panel, target, area).unwrap_or(target);
        if panel == target {
            return self.sanitized();
        }
        let without_panel = self.without_panel(panel)?;
        without_panel.insert_panel(panel, target, area)?.sanitized()
    }

    /// Move an existing panel tab into a target tab group at an explicit tab index.
    ///
    /// This is used by tab-bar drag/drop. It keeps same-group reordering as a
    /// pure tab order mutation and only removes/collapses source leaves when
    /// the target group is different.
    pub fn relocate_panel_to_tab_index(
        self,
        panel: PanelKind,
        target: PanelKind,
        insert_index: usize,
    ) -> Option<Self> {
        if !self.contains_panel(panel) || !self.contains_panel(target) {
            return None;
        }
        if self.panel_group_contains_all(&[panel, target]) {
            return self.reorder_panel_in_tab_group(panel, insert_index)?.sanitized();
        }
        let without_panel = self.without_panel(panel)?;
        without_panel
            .insert_panel_at_tab_index(panel, target, insert_index)?
            .sanitized()
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
                active_index: sanitize_active_index_for_tabs(
                    panel.active_index(),
                    &panel.tab_kinds(),
                ),
                hidden_tabs: Vec::new(),
                tabs: panel.tab_kinds(),
            })
    }

    fn merge_panel_metadata_from(&mut self, previous: &Self) {
        match self {
            Self::Split { first, second, .. } => {
                first.merge_panel_metadata_from(previous);
                second.merge_panel_metadata_from(previous);
            }
            Self::Panel { kind, active_index, hidden_tabs, tabs } => {
                let current_tabs = panel_tabs_for_leaf(*kind, hidden_tabs, tabs);
                if let Some(previous_tabs) = previous.tabs_for_panel_group(current_tabs.as_slice())
                {
                    *tabs = previous_tabs;
                    *kind = tabs.first().copied().unwrap_or(*kind);
                    *active_index = sanitize_active_index_for_tabs(*active_index, tabs);
                    *hidden_tabs = Vec::new();
                } else if let Some(previous_hidden) = previous.hidden_tabs_for_panel(*kind) {
                    *hidden_tabs = sanitize_hidden_tabs(*kind, previous_hidden.to_vec());
                    *tabs = sanitize_panel_tabs(*kind, hidden_tabs, tabs.clone());
                    *active_index = sanitize_active_index_for_tabs(*active_index, tabs);
                }
            }
        }
    }

    fn insert_panel(self, panel: PanelKind, target: PanelKind, area: DockDropArea) -> Option<Self> {
        match self {
            Self::Panel { kind, active_index, hidden_tabs, tabs } => {
                let mut tabs = panel_tabs_for_leaf(kind, &hidden_tabs, &tabs);
                if !tabs.contains(&target) {
                    return None;
                }
                if area == DockDropArea::Center {
                    if !tabs.contains(&panel) {
                        let target_index = tabs.iter().position(|tab| *tab == target).unwrap_or(0);
                        tabs.insert(target_index + 1, panel);
                    }
                    let active_index = tabs.iter().position(|tab| *tab == panel).unwrap_or(0);
                    return Some(Self::Panel {
                        kind: tabs[0],
                        active_index,
                        hidden_tabs: Vec::new(),
                        tabs,
                    });
                }

                let target_leaf = Self::Panel {
                    kind,
                    active_index: sanitize_active_index_for_tabs(active_index, &tabs),
                    hidden_tabs: Vec::new(),
                    tabs,
                };
                Some(split_for_drop_area(
                    Self::Panel {
                        kind: panel,
                        active_index: 0,
                        hidden_tabs: Vec::new(),
                        tabs: vec![panel],
                    },
                    target_leaf,
                    area,
                ))
            }
            Self::Split { direction, ratio, first, second } => {
                match first.clone().insert_panel(panel, target, area) {
                    Some(first) => {
                        Some(Self::Split { direction, ratio, first: Box::new(first), second })
                    }
                    None => second.insert_panel(panel, target, area).map(|second| Self::Split {
                        direction,
                        ratio,
                        first,
                        second: Box::new(second),
                    }),
                }
            }
        }
    }

    fn insert_panel_at_tab_index(
        self,
        panel: PanelKind,
        target: PanelKind,
        insert_index: usize,
    ) -> Option<Self> {
        match self {
            Self::Panel { kind, hidden_tabs, tabs, .. } => {
                let mut tabs = panel_tabs_for_leaf(kind, &hidden_tabs, &tabs);
                if !tabs.contains(&target) {
                    return None;
                }
                if !tabs.contains(&panel) {
                    let index = insert_index.min(tabs.len());
                    tabs.insert(index, panel);
                }
                let active_index = tabs.iter().position(|tab| *tab == panel).unwrap_or(0);
                Some(Self::Panel {
                    kind: tabs[0],
                    active_index,
                    hidden_tabs: Vec::new(),
                    tabs,
                })
            }
            Self::Split { direction, ratio, first, second } => {
                match first.clone().insert_panel_at_tab_index(panel, target, insert_index) {
                    Some(first) => {
                        Some(Self::Split { direction, ratio, first: Box::new(first), second })
                    }
                    None => second.insert_panel_at_tab_index(panel, target, insert_index).map(
                        |second| Self::Split { direction, ratio, first, second: Box::new(second) },
                    ),
                }
            }
        }
    }

    fn reorder_panel_in_tab_group(self, panel: PanelKind, insert_index: usize) -> Option<Self> {
        match self {
            Self::Panel { kind, active_index: _, hidden_tabs, tabs } => {
                let tabs = panel_tabs_for_leaf(kind, &hidden_tabs, &tabs);
                if !tabs.contains(&panel) {
                    return None;
                }
                let tabs = reorder_panel_tabs(tabs, panel, insert_index);
                let active_index = tabs.iter().position(|tab| *tab == panel).unwrap_or(0);
                Some(Self::Panel {
                    kind: tabs[0],
                    active_index,
                    hidden_tabs: Vec::new(),
                    tabs,
                })
            }
            Self::Split { direction, ratio, first, second } => {
                if first.contains_panel(panel) {
                    return first.reorder_panel_in_tab_group(panel, insert_index).map(|first| {
                        Self::Split { direction, ratio, first: Box::new(first), second }
                    });
                }
                second
                    .reorder_panel_in_tab_group(panel, insert_index)
                    .map(|second| Self::Split { direction, ratio, first, second: Box::new(second) })
            }
        }
    }

    fn panel_group_contains_all(&self, panels: &[PanelKind]) -> bool {
        match self {
            Self::Split { first, second, .. } => {
                first.panel_group_contains_all(panels) || second.panel_group_contains_all(panels)
            }
            Self::Panel { kind, hidden_tabs, tabs, .. } => {
                let tabs = panel_tabs_for_leaf(*kind, hidden_tabs, tabs);
                panels.iter().all(|panel| tabs.contains(panel))
            }
        }
    }

    fn resolve_drop_target(
        &self,
        panel: PanelKind,
        target: PanelKind,
        area: DockDropArea,
    ) -> Option<PanelKind> {
        if panel != target {
            return Some(target);
        }
        if area == DockDropArea::Center {
            return Some(target);
        }
        self.tabs_for_panel_group(&[panel])?
            .into_iter()
            .find(|candidate| *candidate != panel)
            .or(Some(target))
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

    fn tabs_for_panel_group(&self, candidates: &[PanelKind]) -> Option<Vec<PanelKind>> {
        match self {
            Self::Split { first, second, .. } => first
                .tabs_for_panel_group(candidates)
                .or_else(|| second.tabs_for_panel_group(candidates)),
            Self::Panel { kind, hidden_tabs, tabs, .. } => {
                let existing = panel_tabs_for_leaf(*kind, hidden_tabs, tabs);
                existing.iter().any(|tab| candidates.contains(tab)).then_some(existing)
            }
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

fn sanitize_active_index_for_tabs(active_index: usize, tabs: &[PanelKind]) -> usize {
    active_index.min(tabs.len().saturating_sub(1))
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

fn panel_tabs_for_leaf(
    kind: PanelKind,
    hidden_tabs: &[PanelKind],
    tabs: &[PanelKind],
) -> Vec<PanelKind> {
    sanitize_panel_tabs(kind, hidden_tabs, tabs.to_vec())
}

fn sanitize_panel_tabs(
    kind: PanelKind,
    hidden_tabs: &[PanelKind],
    tabs: Vec<PanelKind>,
) -> Vec<PanelKind> {
    let source = if tabs.is_empty() {
        visible_tabs_for_panel(kind, hidden_tabs)
    } else {
        tabs
    };
    let mut sanitized = Vec::new();
    for tab in source {
        if !sanitized.contains(&tab) {
            sanitized.push(tab);
        }
    }
    if sanitized.is_empty() {
        sanitized.push(kind);
    }
    sanitized
}

fn reorder_panel_tabs(
    mut tabs: Vec<PanelKind>,
    panel: PanelKind,
    insert_index: usize,
) -> Vec<PanelKind> {
    let Some(old_index) = tabs.iter().position(|tab| *tab == panel) else {
        return tabs;
    };
    tabs.remove(old_index);
    let mut adjusted = insert_index;
    if old_index < insert_index {
        adjusted = adjusted.saturating_sub(1);
    }
    tabs.insert(adjusted.min(tabs.len()), panel);
    tabs
}

fn split_for_drop_area(
    moving: AppUiWorkspaceLayout,
    target: AppUiWorkspaceLayout,
    area: DockDropArea,
) -> AppUiWorkspaceLayout {
    let direction = match area {
        DockDropArea::Left | DockDropArea::Right => SplitDirection::Horizontal,
        DockDropArea::Top | DockDropArea::Bottom => SplitDirection::Vertical,
        DockDropArea::Center => return target,
    };
    let (first, second) = match area {
        DockDropArea::Left | DockDropArea::Top => (moving, target),
        DockDropArea::Right | DockDropArea::Bottom => (target, moving),
        DockDropArea::Center => unreachable!("center handled above"),
    };
    AppUiWorkspaceLayout::Split {
        direction,
        ratio: 0.5,
        first: Box::new(first),
        second: Box::new(second),
    }
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
    use mondrian_ui_widgets::dock_tab_bar::TabInfo;

    fn empty_panel(kind: PanelKind) -> Box<dyn Widget> {
        Box::new(DockPanel::new(
            kind,
            vec![TabInfo {
                label: kind.display_name().to_owned(),
                active: true,
                panel_kind: Some(kind),
            }],
            move |_kind, _active| Box::new(EmptyWidget::default()),
        ))
    }

    fn layout_panel(kind: PanelKind, active_index: usize) -> AppUiWorkspaceLayout {
        layout_tabs(vec![kind], active_index)
    }

    fn layout_tabs(tabs: Vec<PanelKind>, active_index: usize) -> AppUiWorkspaceLayout {
        AppUiWorkspaceLayout::Panel {
            kind: tabs.first().copied().unwrap_or(PanelKind::Viewer),
            active_index,
            hidden_tabs: Vec::new(),
            tabs,
        }
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

        let layout = AppUiWorkspaceLayout::from_dock(&dock).expect("layout");

        assert_eq!(
            layout,
            AppUiWorkspaceLayout::Split {
                direction: SplitDirection::Horizontal,
                ratio: 0.42,
                first: Box::new(layout_panel(PanelKind::Assets, 0)),
                second: Box::new(layout_panel(PanelKind::Viewer, 0)),
            }
        );
    }

    #[test]
    fn sanitizes_ratios_and_panel_tab_indices() {
        let layout = AppUiWorkspaceLayout::Split {
            direction: SplitDirection::Vertical,
            ratio: f32::NAN,
            first: Box::new(layout_panel(PanelKind::Assets, 12)),
            second: Box::new(layout_panel(PanelKind::Timeline, 8)),
        }
        .sanitized()
        .expect("sanitized");

        assert_eq!(
            layout,
            AppUiWorkspaceLayout::Split {
                direction: SplitDirection::Vertical,
                ratio: 0.5,
                first: Box::new(layout_panel(PanelKind::Assets, 0)),
                second: Box::new(layout_panel(PanelKind::Timeline, 0)),
            }
        );
    }

    #[test]
    fn removing_panel_collapses_empty_split_branches() {
        let layout = AppUiWorkspaceLayout::Split {
            direction: SplitDirection::Vertical,
            ratio: 0.66,
            first: Box::new(AppUiWorkspaceLayout::Split {
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
    fn removing_grouped_tab_prunes_tab_group_without_removing_owner_panel() {
        let layout = layout_tabs(vec![PanelKind::Assets, PanelKind::Effects], 1);

        let without_effects = layout.without_panel(PanelKind::Effects).expect("assets remain");

        assert!(without_effects.contains_panel(PanelKind::Assets));
        assert!(!without_effects.contains_panel(PanelKind::Effects));
        assert_eq!(
            without_effects,
            AppUiWorkspaceLayout::Panel {
                kind: PanelKind::Assets,
                active_index: 0,
                hidden_tabs: Vec::new(),
                tabs: vec![PanelKind::Assets],
            }
        );
    }

    #[test]
    fn preserves_hidden_grouped_tabs_when_merging_live_panel_metadata() {
        let live = layout_panel(PanelKind::Assets, 1);
        let previous = AppUiWorkspaceLayout::Panel {
            kind: PanelKind::Assets,
            active_index: 0,
            hidden_tabs: vec![PanelKind::Effects],
            tabs: Vec::new(),
        };

        let merged = live.with_panel_metadata_from(&previous);

        assert_eq!(
            merged,
            AppUiWorkspaceLayout::Panel {
                kind: PanelKind::Assets,
                active_index: 0,
                hidden_tabs: Vec::new(),
                tabs: vec![PanelKind::Assets],
            }
        );
    }

    #[test]
    fn relocate_panel_to_center_creates_active_tab_group() {
        let layout = AppUiWorkspaceLayout::Split {
            direction: SplitDirection::Horizontal,
            ratio: 0.5,
            first: Box::new(layout_panel(PanelKind::Assets, 0)),
            second: Box::new(layout_panel(PanelKind::Inspector, 0)),
        };

        let moved = layout
            .relocate_panel(
                PanelKind::Inspector,
                PanelKind::Assets,
                DockDropArea::Center,
            )
            .expect("relocated");

        assert_eq!(
            moved,
            AppUiWorkspaceLayout::Panel {
                kind: PanelKind::Assets,
                active_index: 1,
                hidden_tabs: Vec::new(),
                tabs: vec![PanelKind::Assets, PanelKind::Inspector],
            }
        );
    }

    #[test]
    fn relocate_panel_to_tab_index_reorders_tabs_inside_existing_group() {
        let layout = layout_tabs(
            vec![PanelKind::Assets, PanelKind::Effects, PanelKind::Inspector],
            2,
        );

        let moved = layout
            .relocate_panel_to_tab_index(PanelKind::Inspector, PanelKind::Assets, 0)
            .expect("reordered");

        assert_eq!(
            moved,
            AppUiWorkspaceLayout::Panel {
                kind: PanelKind::Inspector,
                active_index: 0,
                hidden_tabs: Vec::new(),
                tabs: vec![PanelKind::Inspector, PanelKind::Assets, PanelKind::Effects],
            }
        );
    }

    #[test]
    fn relocate_panel_to_tab_index_inserts_into_target_group_and_collapses_source() {
        let layout = AppUiWorkspaceLayout::Split {
            direction: SplitDirection::Horizontal,
            ratio: 0.5,
            first: Box::new(layout_panel(PanelKind::Assets, 0)),
            second: Box::new(layout_tabs(
                vec![PanelKind::Viewer, PanelKind::Inspector],
                0,
            )),
        };

        let moved = layout
            .relocate_panel_to_tab_index(PanelKind::Assets, PanelKind::Viewer, 1)
            .expect("inserted");

        assert_eq!(
            moved,
            AppUiWorkspaceLayout::Panel {
                kind: PanelKind::Viewer,
                active_index: 1,
                hidden_tabs: Vec::new(),
                tabs: vec![PanelKind::Viewer, PanelKind::Assets, PanelKind::Inspector],
            }
        );
    }

    #[test]
    fn relocate_panel_to_edge_creates_split_around_target_group() {
        let layout = AppUiWorkspaceLayout::Split {
            direction: SplitDirection::Horizontal,
            ratio: 0.5,
            first: Box::new(layout_tabs(vec![PanelKind::Assets, PanelKind::Effects], 0)),
            second: Box::new(layout_panel(PanelKind::Inspector, 0)),
        };

        let moved = layout
            .relocate_panel(PanelKind::Inspector, PanelKind::Assets, DockDropArea::Right)
            .expect("relocated");

        assert_eq!(
            moved,
            AppUiWorkspaceLayout::Split {
                direction: SplitDirection::Horizontal,
                ratio: 0.5,
                first: Box::new(layout_tabs(vec![PanelKind::Assets, PanelKind::Effects], 0)),
                second: Box::new(layout_panel(PanelKind::Inspector, 0)),
            }
        );
    }

    #[test]
    fn relocate_panel_to_own_edge_splits_tab_out_of_same_group() {
        let layout = layout_tabs(vec![PanelKind::Assets, PanelKind::Effects], 0);

        let moved = layout
            .relocate_panel(PanelKind::Assets, PanelKind::Assets, DockDropArea::Right)
            .expect("relocated");

        assert_eq!(
            moved,
            AppUiWorkspaceLayout::Split {
                direction: SplitDirection::Horizontal,
                ratio: 0.5,
                first: Box::new(layout_panel(PanelKind::Effects, 0)),
                second: Box::new(layout_panel(PanelKind::Assets, 0)),
            }
        );
    }

    #[test]
    fn removing_only_panel_returns_none() {
        let layout = layout_panel(PanelKind::Viewer, 0);

        assert_eq!(layout.without_panel(PanelKind::Viewer), None);
    }
}
