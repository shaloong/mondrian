//! Menu data model: items, tokens, constants, trigger style.

use mondrian_editor_state::Action;
use mondrian_ui_theme::{current_theme, Theme};

use crate::text_metrics::measure_single_line;
use crate::vector_icon::VectorIcon;

// ── Constants ────────────────────────────────────────────────────────────────────

// ── MenuMetrics ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MenuMetrics {
    pub measure_font_size: f32,
    pub min_width: f32,
    pub item_height: f32,
    pub trigger_height: f32,
    pub menu_bar_trigger_height: f32,
    pub trigger_padding_x: f32,
    pub menu_bar_trigger_padding_x: f32,
    pub arrow_space: f32,
    pub popup_padding: f32,
    pub popup_gap: f32,
    pub row_padding_x: f32,
    pub row_icon_size: f32,
    pub row_icon_gap: f32,
    pub row_shortcut_gap: f32,
    pub scrollbar_space: f32,
    pub viewport_margin: f32,
    pub popup_max_height: f32,
    pub popup_viewport_pad: f32,
}

impl MenuMetrics {
    pub fn from_theme(theme: &Theme) -> Self {
        let spacing = &theme.spacing;
        Self {
            measure_font_size: theme.typography.small.font_size,
            min_width: spacing.menu_min_width,
            item_height: spacing.menu_item_height,
            trigger_height: spacing.menu_trigger_height,
            menu_bar_trigger_height: spacing.menu_bar_trigger_height,
            trigger_padding_x: spacing.menu_trigger_padding_x,
            menu_bar_trigger_padding_x: spacing.menu_bar_trigger_padding_x,
            arrow_space: spacing.menu_arrow_space,
            popup_padding: spacing.menu_popup_padding,
            popup_gap: spacing.menu_popup_gap,
            row_padding_x: spacing.menu_row_padding_x,
            row_icon_size: spacing.menu_row_icon_size,
            row_icon_gap: spacing.menu_row_icon_gap,
            row_shortcut_gap: spacing.menu_row_shortcut_gap,
            scrollbar_space: spacing.menu_scrollbar_space,
            viewport_margin: spacing.menu_viewport_margin,
            popup_max_height: spacing.menu_popup_max_height,
            popup_viewport_pad: spacing.menu_popup_viewport_pad,
        }
    }

    pub fn current() -> Self {
        let theme = current_theme();
        Self::from_theme(&theme)
    }

    pub fn trigger_height(self, style: DropdownTriggerStyle) -> f32 {
        match style {
            DropdownTriggerStyle::Filled => self.trigger_height,
            DropdownTriggerStyle::MenuBar => self.menu_bar_trigger_height,
        }
    }

    pub fn trigger_padding_x(self, style: DropdownTriggerStyle) -> f32 {
        match style {
            DropdownTriggerStyle::Filled => self.trigger_padding_x,
            DropdownTriggerStyle::MenuBar => self.menu_bar_trigger_padding_x,
        }
    }
}

// ── MenuRowPaint ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct MenuRowPaint {
    pub enabled: bool,
    pub active: bool,
    pub hovered: bool,
}

// ── MenuVisualTokens ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MenuVisualTokens {
    pub filled_trigger_radius: f32,
    pub menu_bar_trigger_radius: f32,
    pub filled_trigger_padding_x: f32,
    pub menu_bar_trigger_padding_x: f32,
    pub menu_bar_font_size: f32,
    pub popup_radius: f32,
    pub popup_border_inset: f32,
    pub row_radius: f32,
    pub row_font_size: f32,
    pub row_hover_alpha: f32,
    pub row_padding_x: f32,
    pub row_icon_size: f32,
    pub row_icon_gap: f32,
    pub row_shortcut_gap: f32,
    pub shortcut_font_size: f32,
    pub separator_inset_x: f32,
    pub separator_width: f32,
    pub separator_alpha: f32,
    pub scrollbar_right_inset: f32,
    pub scrollbar_top_inset: f32,
    pub scrollbar_width: f32,
    pub scrollbar_min_thumb_height: f32,
    pub arrow_right_inset: f32,
    pub arrow_half_width: f32,
    pub arrow_top_offset: f32,
    pub arrow_bottom_offset: f32,
    pub check_start_offset_x: f32,
    pub check_width: f32,
}

impl MenuVisualTokens {
    pub fn from_theme(theme: &Theme) -> Self {
        let spacing = &theme.spacing;
        Self {
            filled_trigger_radius: spacing.radius_sm,
            menu_bar_trigger_radius: spacing.radius_sm,
            filled_trigger_padding_x: spacing.sm + spacing.border_emphasis,
            menu_bar_trigger_padding_x: spacing.sm + spacing.border_standard,
            menu_bar_font_size: theme.typography.button.font_size,
            popup_radius: spacing.radius_lg.min(spacing.radius_md),
            popup_border_inset: spacing.border_standard,
            row_radius: spacing.radius_sm,
            row_font_size: theme.typography.small.font_size,
            row_hover_alpha: 0.72,
            row_padding_x: spacing.menu_row_padding_x,
            row_icon_size: spacing.menu_row_icon_size,
            row_icon_gap: spacing.menu_row_icon_gap,
            row_shortcut_gap: spacing.menu_row_shortcut_gap,
            shortcut_font_size: theme.typography.metadata.font_size,
            separator_inset_x: spacing.sm,
            separator_width: spacing.border_standard,
            separator_alpha: 0.86,
            scrollbar_right_inset: spacing.xs + spacing.border_standard,
            scrollbar_top_inset: spacing.border_emphasis + spacing.border_standard,
            scrollbar_width: spacing.border_emphasis + spacing.border_standard,
            scrollbar_min_thumb_height: spacing.icon_size + spacing.border_emphasis,
            arrow_right_inset: spacing.icon_size + spacing.border_emphasis,
            arrow_half_width: spacing.xs,
            arrow_top_offset: spacing.border_emphasis,
            arrow_bottom_offset: spacing.border_emphasis + spacing.border_standard,
            check_start_offset_x: spacing.border_standard,
            check_width: spacing.md - spacing.border_standard * 0.5,
        }
    }

    pub fn trigger_padding_x(self, style: DropdownTriggerStyle) -> f32 {
        match style {
            DropdownTriggerStyle::Filled => self.filled_trigger_padding_x,
            DropdownTriggerStyle::MenuBar => self.menu_bar_trigger_padding_x,
        }
    }

    pub fn trigger_radius(self, style: DropdownTriggerStyle) -> f32 {
        match style {
            DropdownTriggerStyle::Filled => self.filled_trigger_radius,
            DropdownTriggerStyle::MenuBar => self.menu_bar_trigger_radius,
        }
    }
}

// ── DropdownTriggerStyle ──────────────────────────────────────────────────────────

/// Visual treatment for a dropdown trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropdownTriggerStyle {
    /// Filled rounded rectangle suitable for form and toolbar dropdowns.
    Filled,
    /// Lightweight transparent trigger suitable for native-style menu bars.
    MenuBar,
}

// ── MenuItemKind ──────────────────────────────────────────────────────────────────

/// Menu row behavior.
#[derive(Debug, Clone)]
pub enum MenuItemKind {
    /// Clickable menu row that may dispatch an action when enabled.
    Action,
    /// Visual divider row that never dispatches an action.
    Separator,
    /// Submenu trigger that opens a child menu to the right when hovered.
    Submenu { children: Vec<MenuItem> },
}

/// Command carried by an activatable menu row.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuItemCommand {
    /// Dispatch an editor/app action.
    Action(Action),
    /// Report a widget-local command to the owning control.
    Local(String),
    /// No executable command. Used by separators and submenu parents.
    None,
}

// ── MenuItem ─────────────────────────────────────────────────────────────────────

/// Menu item with label, command, optional icon, shortcut, and checked state.
#[derive(Debug, Clone)]
pub struct MenuItem {
    pub label: String,
    pub command: MenuItemCommand,
    pub enabled: bool,
    pub kind: MenuItemKind,
    pub icon: Option<VectorIcon>,
    pub shortcut: Option<String>,
    pub checked: bool,
}

impl MenuItem {
    pub fn new(label: impl Into<String>, action: Action) -> Self {
        Self {
            label: label.into(),
            command: MenuItemCommand::Action(action),
            enabled: true,
            kind: MenuItemKind::Action,
            icon: None,
            shortcut: None,
            checked: false,
        }
    }

    /// Create a menu row that reports a component-local command instead of
    /// dispatching an app action.
    pub fn local(label: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            command: MenuItemCommand::Local(command.into()),
            enabled: true,
            kind: MenuItemKind::Action,
            icon: None,
            shortcut: None,
            checked: false,
        }
    }

    /// Create a submenu trigger row.
    pub fn submenu(label: impl Into<String>, children: Vec<MenuItem>) -> Self {
        Self {
            label: label.into(),
            command: MenuItemCommand::None,
            enabled: true,
            kind: MenuItemKind::Submenu { children },
            icon: None,
            shortcut: None,
            checked: false,
        }
    }

    /// Create a disabled action row without an editor command.
    ///
    /// Use this for not-yet-implemented product entries instead of routing a
    /// placeholder action through the command system.
    pub fn inert(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            command: MenuItemCommand::None,
            enabled: false,
            kind: MenuItemKind::Action,
            icon: None,
            shortcut: None,
            checked: false,
        }
    }

    /// Create a visual divider row.
    pub fn separator() -> Self {
        Self {
            label: String::new(),
            command: MenuItemCommand::None,
            enabled: false,
            kind: MenuItemKind::Separator,
            icon: None,
            shortcut: None,
            checked: false,
        }
    }

    /// Paint a vector icon before this item label.
    pub fn with_icon(mut self, icon: VectorIcon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Paint a right-aligned keyboard shortcut hint for this item.
    pub fn with_shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Make this item a submenu trigger with child items.
    pub fn with_submenu(mut self, children: Vec<MenuItem>) -> Self {
        self.kind = MenuItemKind::Submenu { children };
        self.command = MenuItemCommand::None;
        self
    }

    /// Mark this item as representing the current checked/selected state.
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    pub fn is_separator(&self) -> bool {
        matches!(self.kind, MenuItemKind::Separator)
    }

    pub fn is_activatable(&self) -> bool {
        self.enabled && !self.is_separator()
    }

    pub fn is_submenu(&self) -> bool {
        matches!(self.kind, MenuItemKind::Submenu { .. })
    }

    pub fn action(&self) -> Option<&Action> {
        match &self.command {
            MenuItemCommand::Action(action) => Some(action),
            MenuItemCommand::Local(_) | MenuItemCommand::None => None,
        }
    }

    pub fn local_command(&self) -> Option<&str> {
        match &self.command {
            MenuItemCommand::Local(command) => Some(command),
            MenuItemCommand::Action(_) | MenuItemCommand::None => None,
        }
    }

    pub fn has_command(&self) -> bool {
        !matches!(self.command, MenuItemCommand::None)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────────

pub(crate) fn menu_item_text_width_with_metrics(item: &MenuItem, metrics: MenuMetrics) -> f32 {
    let label_width = measure_single_line(&item.label, metrics.measure_font_size).0;
    let shortcut_width = item
        .shortcut
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| metrics.row_shortcut_gap + measure_single_line(s, metrics.measure_font_size).0)
        .unwrap_or(0.0);
    let submenu_arrow = if item.is_submenu() {
        metrics.arrow_space
    } else {
        0.0
    };
    label_width + shortcut_width + submenu_arrow
}

pub(crate) fn menu_item_activation(item: &MenuItem) -> Option<MenuItemCommand> {
    if !item.is_activatable() {
        return None;
    }
    match &item.kind {
        MenuItemKind::Action => item.has_command().then(|| item.command.clone()),
        MenuItemKind::Submenu { children } => children.iter().find_map(menu_item_activation),
        MenuItemKind::Separator => None,
    }
}
