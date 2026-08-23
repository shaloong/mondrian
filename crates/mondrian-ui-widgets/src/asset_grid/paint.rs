//! Asset grid paint: thumbnail states.

use mondrian_core::Color;
use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_core::widget::PaintContext;

use crate::paint::color_with_alpha;

use super::AssetGridVisualTokens;

pub(super) fn paint_thumbnail_loading(ctx: &mut PaintContext, preview: Rect, color: Color) {
    let dot_size = 4.0;
    let gap = 5.0;
    let total_width = dot_size * 3.0 + gap * 2.0;
    let y = preview.y + preview.height - 13.0;
    let start_x = preview.x + (preview.width - total_width) * 0.5;
    for index in 0..3 {
        let alpha = 0.30 + index as f32 * 0.18;
        ctx.encoder.draw_rect(
            Rect::new(
                start_x + index as f32 * (dot_size + gap),
                y,
                dot_size,
                dot_size,
            ),
            color_with_alpha(color, alpha),
            dot_size * 0.5,
        );
    }
}

pub(super) fn paint_thumbnail_failed(
    ctx: &mut PaintContext,
    preview: Rect,
    color: Color,
    visual: &AssetGridVisualTokens,
) {
    let chip = Rect::new(
        preview.x + 8.0,
        preview.y + preview.height - 24.0,
        22.0,
        16.0,
    );
    ctx.encoder.draw_rect(chip, color_with_alpha(color, 0.18), 5.0);
    let center = chip.center();
    let triangle = [
        Point::new(center.x, chip.y + 4.0),
        Point::new(chip.x + 6.0, chip.y + chip.height - 4.0),
        Point::new(chip.x + chip.width - 6.0, chip.y + chip.height - 4.0),
    ];
    ctx.encoder.draw_triangles(&triangle, color_with_alpha(color, 0.70));
    ctx.encoder.draw_rect(
        Rect::new(center.x - 0.75, chip.y + 7.0, 1.5, 4.5),
        visual.failed_thumbnail_mark,
        0.75,
    );
    ctx.encoder.draw_rect(
        Rect::new(center.x - 0.75, chip.y + 12.4, 1.5, 1.5),
        visual.failed_thumbnail_mark,
        0.75,
    );
}
