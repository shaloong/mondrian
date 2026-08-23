//! GPU-backed Program Output video scopes surface.

use mondrian_core::Color;
use mondrian_ui_core::types::{LayoutConstraint, Point, Rect, Size, WidgetId};
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// Stable renderer-registry keys for one coherent scope texture set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoScopesTextureSet {
    /// RGB histogram texture key.
    pub histogram: String,
    /// Luma/RGB waveform texture key.
    pub waveform: String,
    /// Vectorscope texture key.
    pub vectorscope: String,
}

impl VideoScopesTextureSet {
    /// Construct a texture set when every registry key is non-empty.
    pub fn new(
        histogram: impl Into<String>,
        waveform: impl Into<String>,
        vectorscope: impl Into<String>,
    ) -> Option<Self> {
        let set = Self {
            histogram: histogram.into(),
            waveform: waveform.into(),
            vectorscope: vectorscope.into(),
        };
        (!set.histogram.is_empty() && !set.waveform.is_empty() && !set.vectorscope.is_empty())
            .then_some(set)
    }
}

/// Responsive waveform, histogram, and vectorscope presentation surface.
pub struct VideoScopesSurface {
    id: WidgetId,
    bounds: Rect,
    textures: Option<VideoScopesTextureSet>,
}

impl VideoScopesSurface {
    /// Create an empty surface that shows a waiting state until GPU textures arrive.
    pub fn new() -> Self {
        Self {
            id: WidgetId::new(),
            bounds: Rect::ZERO,
            textures: None,
        }
    }

    /// Attach the current coherent GPU texture set.
    pub fn with_textures(mut self, textures: VideoScopesTextureSet) -> Self {
        self.textures = Some(textures);
        self
    }

    /// Replace the current texture set during the narrow playback refresh path.
    pub fn set_textures(&mut self, textures: Option<VideoScopesTextureSet>) {
        self.textures = textures;
    }

    fn scope_rects(&self, gap: f32) -> (Rect, Rect, Rect) {
        let content = self.bounds.inset(gap, gap);
        let waveform_height = (content.height * 0.6 - gap * 0.5).max(1.0);
        let lower_height = (content.height - waveform_height - gap).max(1.0);
        let lower_width = ((content.width - gap) * 0.5).max(1.0);
        let waveform = Rect::new(content.x, content.y, content.width, waveform_height);
        let histogram = Rect::new(
            content.x,
            content.y + waveform_height + gap,
            lower_width,
            lower_height,
        );
        let vectorscope = Rect::new(
            content.x + lower_width + gap,
            histogram.y,
            (content.width - lower_width - gap).max(1.0),
            lower_height,
        );
        (waveform, histogram, vectorscope)
    }
}

impl Default for VideoScopesSurface {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for VideoScopesSurface {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(320.0, 240.0))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext<'_>) -> EventResult {
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext<'_>) {
        let gap = ctx.theme.spacing.sm;
        let (waveform, histogram, vectorscope) = self.scope_rects(gap);
        ctx.encoder.draw_rect(self.bounds, ctx.theme.colors.card, 0.0);
        for rect in [waveform, histogram, vectorscope] {
            ctx.encoder
                .draw_rect(rect, ctx.theme.colors.canvas, ctx.theme.spacing.radius_sm);
            ctx.encoder.draw_line(
                Point::new(rect.x, rect.y + rect.height * 0.5),
                Point::new(rect.x + rect.width, rect.y + rect.height * 0.5),
                ctx.theme.spacing.border_standard,
                ctx.theme.colors.border,
            );
        }
        if let Some(textures) = &self.textures {
            let uv = Rect::new(0.0, 0.0, 1.0, 1.0);
            ctx.encoder
                .draw_external_texture(&textures.waveform, waveform, uv, Color::WHITE);
            ctx.encoder
                .draw_external_texture(&textures.histogram, histogram, uv, Color::WHITE);
            ctx.encoder
                .draw_external_texture(&textures.vectorscope, vectorscope, uv, Color::WHITE);
        } else {
            let style = &ctx.theme.typography.small;
            ctx.encoder.draw_text_box(
                "等待 Program Output",
                style.font_size,
                Point::new(waveform.x + gap, waveform.y + gap),
                (waveform.width - gap * 2.0).max(1.0),
                ctx.theme.colors.text_tertiary,
            );
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_set_rejects_partial_registry_identity() {
        assert!(VideoScopesTextureSet::new("hist", "wave", "vector").is_some());
        assert!(VideoScopesTextureSet::new("hist", "", "vector").is_none());
    }

    #[test]
    fn responsive_layout_keeps_all_scope_regions_inside_bounds() {
        let mut surface = VideoScopesSurface::new();
        surface.layout(Rect::new(10.0, 20.0, 500.0, 300.0));
        let (waveform, histogram, vectorscope) = surface.scope_rects(8.0);
        for rect in [waveform, histogram, vectorscope] {
            assert!(surface.bounds.contains(Point::new(rect.x, rect.y)));
            assert!(surface.bounds.contains(Point::new(
                rect.x + rect.width - f32::EPSILON,
                rect.y + rect.height - f32::EPSILON,
            )));
        }
    }
}
