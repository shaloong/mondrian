//! 面板槽位控件
//!
//! 包装面板标识和面板内容 Widget 的容器。

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// 面板类型标识（精简版，用于 Demo）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    Viewer,
    Timeline,
    Assets,
    Inspector,
    Effects,
    Project,
    Console,
}

impl SlotKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Viewer => "预览",
            Self::Timeline => "时间线",
            Self::Assets => "素材",
            Self::Inspector => "检查器",
            Self::Effects => "效果",
            Self::Project => "项目",
            Self::Console => "控制台",
        }
    }
}

/// PanelSlot —— 包装面板内容 Widget
pub struct PanelSlot {
    id: WidgetId,
    kind: SlotKind,
    content: Option<Box<dyn Widget>>,
    bounds: Rect,
}

impl PanelSlot {
    pub fn new(kind: SlotKind, content: Box<dyn Widget>) -> Self {
        Self {
            id: WidgetId::new(),
            kind,
            content: Some(content),
            bounds: Rect::ZERO,
        }
    }

    pub fn kind(&self) -> SlotKind {
        self.kind
    }
}

impl Widget for PanelSlot {
    fn id(&self) -> WidgetId { self.id }

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

    fn children(&self) -> &[Box<dyn Widget>] { &[] }
    fn children_mut(&mut self) -> &mut [Box<dyn Widget>] { &mut [] }
}
