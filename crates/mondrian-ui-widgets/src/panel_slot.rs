//! 面板槽位控件
//!
//! 包装面板标识和面板内容 Widget 的容器。

use mondrian_editor_state::state::PanelKind;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// PanelSlot —— 包装面板内容 Widget
pub struct PanelSlot {
    id: WidgetId,
    kind: PanelKind,
    content: Option<Box<dyn Widget>>,
    bounds: Rect,
}

impl PanelSlot {
    pub fn new(kind: PanelKind, content: Box<dyn Widget>) -> Self {
        Self {
            id: WidgetId::new(),
            kind,
            content: Some(content),
            bounds: Rect::ZERO,
        }
    }

    pub fn kind(&self) -> PanelKind {
        self.kind
    }
}

impl Widget for PanelSlot {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn panel_kind(&self) -> Option<PanelKind> {
        Some(self.kind)
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        if let Some(content) = &self.content {
            content.measure(constraint)
        } else {
            Size::new(100.0, 100.0)
        }
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        if let Some(content) = &mut self.content {
            content.layout(bounds);
        }
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if let Some(content) = &mut self.content {
            content.event(event, ctx)
        } else {
            EventResult::Ignored
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        if let Some(content) = &self.content {
            let previous_clip = ctx.clip_rect;
            let slot_clip = previous_clip.intersection(&self.bounds);
            ctx.clip_rect = slot_clip;
            ctx.push_clip(slot_clip);
            content.paint(ctx);
            ctx.pop_clip();
            ctx.clip_rect = previous_clip;
        }
    }

    fn paint_overlay(&self, ctx: &mut PaintContext) {
        if let Some(content) = &self.content {
            content.paint_overlay(ctx);
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_hit_test_clip(&self) -> Option<Rect> {
        Some(self.bounds)
    }

    fn children(&self) -> &[Box<dyn Widget>] {
        match &self.content {
            Some(c) => std::slice::from_ref(c),
            None => &[],
        }
    }

    fn children_mut(&mut self) -> &mut [Box<dyn Widget>] {
        match &mut self.content {
            Some(c) => std::slice::from_mut(c),
            None => &mut [],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::Color;
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;
    use std::cell::Cell;
    use std::rc::Rc;

    struct OverlayProbe {
        id: WidgetId,
        bounds: Rect,
        overlay_painted: Rc<Cell<bool>>,
        paint_clip: Option<Rc<Cell<Option<Rect>>>>,
    }

    impl OverlayProbe {
        fn new(overlay_painted: Rc<Cell<bool>>) -> Self {
            Self {
                id: WidgetId::new(),
                bounds: Rect::ZERO,
                overlay_painted,
                paint_clip: None,
            }
        }

        fn with_paint_clip(mut self, paint_clip: Rc<Cell<Option<Rect>>>) -> Self {
            self.paint_clip = Some(paint_clip);
            self
        }
    }

    impl Widget for OverlayProbe {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, constraint: LayoutConstraint) -> Size {
            constraint.constrain(Size::new(80.0, 24.0))
        }

        fn layout(&mut self, bounds: Rect) {
            self.bounds = bounds;
        }

        fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            EventResult::Ignored
        }

        fn paint(&self, ctx: &mut PaintContext) {
            if let Some(paint_clip) = &self.paint_clip {
                paint_clip.set(Some(ctx.clip_rect));
            }
        }

        fn paint_overlay(&self, _ctx: &mut PaintContext) {
            self.overlay_painted.set(true);
        }

        fn hit_test(&self, point: Point) -> bool {
            self.bounds.contains(point)
        }
    }

    #[derive(Default)]
    struct RecordingEncoder {
        clips: Vec<Rect>,
        clip_pops: usize,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, bounds: Rect) {
            self.clips.push(bounds);
        }

        fn pop_clip(&mut self) {
            self.clip_pops += 1;
        }

        fn draw_rect(&mut self, _bounds: Rect, _color: Color, _corner_radius: f32) {}

        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {}

        fn draw_text(&mut self, _text: &str, _font_size: f32, _position: Point, _color: Color) {}

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    #[test]
    fn panel_slot_paint_overlay_forwards_to_content() {
        let overlay_painted = Rc::new(Cell::new(false));
        let slot = PanelSlot::new(
            PanelKind::Inspector,
            Box::new(OverlayProbe::new(Rc::clone(&overlay_painted))),
        );
        let mut encoder = RecordingEncoder::default();
        let theme = ThemePreset::Dark.build();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 100.0, 100.0),
        };

        slot.paint_overlay(&mut ctx);

        assert!(overlay_painted.get());
    }

    #[test]
    fn panel_slot_paint_clips_normal_content_to_slot_bounds() {
        let overlay_painted = Rc::new(Cell::new(false));
        let observed_clip = Rc::new(Cell::new(None));
        let mut slot = PanelSlot::new(
            PanelKind::Inspector,
            Box::new(
                OverlayProbe::new(Rc::clone(&overlay_painted))
                    .with_paint_clip(Rc::clone(&observed_clip)),
            ),
        );
        slot.layout(Rect::new(20.0, 30.0, 80.0, 50.0));

        let mut encoder = RecordingEncoder::default();
        let theme = ThemePreset::Dark.build();
        let final_clip = {
            let mut ctx = PaintContext {
                encoder: &mut encoder,
                theme: &theme,
                clip_rect: Rect::new(0.0, 0.0, 64.0, 64.0),
            };
            slot.paint(&mut ctx);
            ctx.clip_rect
        };

        let expected = Rect::new(20.0, 30.0, 44.0, 34.0);
        assert_eq!(observed_clip.get(), Some(expected));
        assert_eq!(encoder.clips, vec![expected]);
        assert_eq!(encoder.clip_pops, 1);
        assert_eq!(final_clip, Rect::new(0.0, 0.0, 64.0, 64.0));
    }

    #[test]
    fn panel_slot_reports_child_hit_test_clip() {
        let overlay_painted = Rc::new(Cell::new(false));
        let mut slot = PanelSlot::new(
            PanelKind::Inspector,
            Box::new(OverlayProbe::new(Rc::clone(&overlay_painted))),
        );
        slot.layout(Rect::new(20.0, 30.0, 80.0, 50.0));

        assert_eq!(
            slot.child_hit_test_clip(),
            Some(Rect::new(20.0, 30.0, 80.0, 50.0))
        );
    }
}
