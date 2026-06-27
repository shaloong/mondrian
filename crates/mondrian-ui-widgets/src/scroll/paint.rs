//! ScrollView paint: viewport clipping, scrollbar chrome.

use crate::paint::color_with_alpha;
use mondrian_ui_core::types::Rect;
use mondrian_ui_core::widget::PaintContext;

use super::model::{ScrollViewVisualTokens, ScrollbarAxis};

/// Paint the scroll view's child inside a viewport clip, then overlay
/// scrollbar chrome. Returns the restored clip rect.
pub(super) fn paint_scroll_view(
    ctx: &mut PaintContext,
    child: Option<&dyn mondrian_ui_core::Widget>,
    bounds: Rect,
    viewport_rect: Rect,
    has_vertical_scrollbar: bool,
    has_horizontal_scrollbar: bool,
    vertical_thumb_rect: Option<Rect>,
    horizontal_thumb_rect: Option<Rect>,
    vertical_track_rect: Rect,
    horizontal_track_rect: Rect,
    dragging_thumb: Option<ScrollbarAxis>,
    hovered_thumb: Option<ScrollbarAxis>,
) {
    let previous_clip = ctx.clip_rect;
    let viewport_clip = previous_clip.intersection(&viewport_rect);
    ctx.clip_rect = viewport_clip;
    ctx.push_clip(viewport_clip);
    if let Some(child) = child {
        child.paint(ctx);
    }
    ctx.pop_clip();

    let has_scrollbar = has_vertical_scrollbar || has_horizontal_scrollbar;
    if has_scrollbar {
        let chrome_clip = previous_clip.intersection(&bounds);
        ctx.clip_rect = chrome_clip;
        ctx.push_clip(chrome_clip);
    }

    let visual = ScrollViewVisualTokens::from_theme(ctx.theme);

    if let Some(sb_rect) = vertical_thumb_rect {
        paint_one_scrollbar(
            ctx,
            &visual,
            sb_rect,
            vertical_track_rect,
            dragging_thumb,
            hovered_thumb,
            ScrollbarAxis::Vertical,
            false,
        );
    }

    if let Some(sb_rect) = horizontal_thumb_rect {
        paint_one_scrollbar(
            ctx,
            &visual,
            sb_rect,
            horizontal_track_rect,
            dragging_thumb,
            hovered_thumb,
            ScrollbarAxis::Horizontal,
            true,
        );
    }

    if has_scrollbar {
        ctx.pop_clip();
    }
    ctx.clip_rect = previous_clip;
}

fn paint_one_scrollbar(
    ctx: &mut PaintContext,
    visual: &ScrollViewVisualTokens,
    mut sb_rect: Rect,
    track: Rect,
    dragging_thumb: Option<ScrollbarAxis>,
    hovered_thumb: Option<ScrollbarAxis>,
    axis: ScrollbarAxis,
    is_horizontal: bool,
) {
    let dragging = dragging_thumb == Some(axis);
    let hovered = hovered_thumb == Some(axis);
    let active = dragging || hovered;

    if active {
        if is_horizontal {
            sb_rect = Rect::new(
                sb_rect.x,
                sb_rect.y - visual.hover_expand,
                sb_rect.width,
                sb_rect.height + visual.hover_expand * 2.0,
            );
        } else {
            sb_rect = Rect::new(
                sb_rect.x - visual.hover_expand,
                sb_rect.y,
                sb_rect.width + visual.hover_expand * 2.0,
                sb_rect.height,
            );
        }
    }

    let track_color = color_with_alpha(
        ctx.theme.colors.scrollbar_thumb,
        if active {
            visual.track_active_alpha
        } else {
            visual.track_idle_alpha
        },
    );
    ctx.encoder.draw_rect(track, track_color, visual.scrollbar_radius);

    let thumb_alpha = if dragging {
        visual.thumb_drag_alpha
    } else if hovered {
        visual.thumb_hover_alpha
    } else {
        visual.thumb_idle_alpha
    };
    let thumb_color = color_with_alpha(ctx.theme.colors.scrollbar_thumb, thumb_alpha);
    ctx.encoder.draw_rect(sb_rect, thumb_color, visual.scrollbar_radius);
}
