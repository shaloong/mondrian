//! Shared widget painting helpers.

use mondrian_core::Color;
use mondrian_ui_core::types::Rect;
use mondrian_ui_core::widget::PaintContext;
use mondrian_ui_theme::spacing::ShadowToken;

pub(crate) fn paint_shadow(ctx: &mut PaintContext, bounds: Rect, radius: f32) {
    let shadow = &ctx.theme.spacing.shadow_md;
    ctx.encoder.draw_rect(shadow_rect(bounds, shadow), shadow_color(shadow), radius);
}

pub(crate) fn shadow_rect(bounds: Rect, shadow: &ShadowToken) -> Rect {
    Rect::new(
        bounds.x + shadow.offset_x - shadow.spread,
        bounds.y + shadow.offset_y - shadow.spread,
        bounds.width + shadow.spread * 2.0,
        bounds.height + shadow.spread * 2.0,
    )
}

pub(crate) fn shadow_color(shadow: &ShadowToken) -> Color {
    Color {
        r: shadow.color[0],
        g: shadow.color[1],
        b: shadow.color[2],
        a: shadow.color[3],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_rect_applies_offset_and_spread() {
        let shadow = ShadowToken {
            offset_x: 2.0,
            offset_y: 4.0,
            blur: 12.0,
            spread: 3.0,
            color: [0.0, 0.0, 0.0, 0.5],
        };

        assert_eq!(
            shadow_rect(Rect::new(10.0, 20.0, 100.0, 40.0), &shadow),
            Rect::new(9.0, 21.0, 106.0, 46.0)
        );
    }

    #[test]
    fn shadow_color_reads_rgba_channels() {
        let shadow = ShadowToken {
            offset_x: 0.0,
            offset_y: 0.0,
            blur: 0.0,
            spread: 0.0,
            color: [0.1, 0.2, 0.3, 0.4],
        };

        assert_eq!(
            shadow_color(&shadow),
            Color { r: 0.1, g: 0.2, b: 0.3, a: 0.4 }
        );
    }
}
