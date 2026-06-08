//! 面板槽位控件
//!
//! 包装面板标识和面板内容 Widget 的容器。

use mondrian_editor_state::state::PanelKind;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

pub use mondrian_editor_state::state::PanelKind as SlotKind;

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
            content.paint(ctx);
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
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
