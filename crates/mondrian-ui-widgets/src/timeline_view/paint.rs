//! Timeline paint helpers: navigator colors.

use mondrian_core::Color;
use mondrian_ui_theme::colors::ColorTokens;

pub(super) fn navigator_body_color(colors: &ColorTokens, hovered: bool, active: bool) -> Color {
    if active {
        colors.timeline_navigator_body_active
    } else if hovered {
        colors.timeline_navigator_body_hover
    } else {
        colors.timeline_navigator_body
    }
}

pub(super) fn navigator_handle_color(colors: &ColorTokens, hovered: bool, active: bool) -> Color {
    if active {
        colors.timeline_navigator_handle_active
    } else if hovered {
        colors.timeline_navigator_handle_hover
    } else {
        colors.timeline_navigator_handle
    }
}
