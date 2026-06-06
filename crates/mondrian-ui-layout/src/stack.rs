//! Stack 布局 —— Z 轴层叠
//!
//! 所有子 Widget 占据同一空间，以最大子 Widget 的尺寸为参考。

use mondrian_ui_core::types::{LayoutConstraint, Rect};
use mondrian_ui_core::Widget;

/// Stack（层叠）布局
///
/// 所有子 Widget 占据相同的父空间。
/// 常用于在背景上叠加前景内容（如 Canvas 上的 Overlay）。
#[derive(Debug, Clone, Default)]
pub struct StackLayout;

impl StackLayout {
    pub fn new() -> Self {
        Self
    }

    /// 所有子都在同一个 bounds 内
    pub fn compute(&self, parent: Rect, children: &[&dyn Widget]) -> Vec<Rect> {
        if children.is_empty() {
            return vec![];
        }

        // Stack: 所有子都分配 parent 区域
        vec![parent; children.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_ui_core::types::{Point, Size, WidgetId};
    use mondrian_ui_core::widget::{EventContext, PaintContext};
    use mondrian_ui_core::{EventResult, UiEvent};

    struct StackTestWidget(WidgetId);
    impl Widget for StackTestWidget {
        fn id(&self) -> WidgetId { self.0 }
        fn measure(&self, _c: LayoutConstraint) -> Size { Size::new(100.0, 100.0) }
        fn layout(&mut self, _b: Rect) {}
        fn event(&mut self, _e: &UiEvent, _ctx: &mut EventContext) -> EventResult { EventResult::Ignored }
        fn paint(&self, _ctx: &mut PaintContext) {}
    }

    #[test]
    fn stack_all_children_same_bounds() {
        let w1 = StackTestWidget(WidgetId::new());
        let w2 = StackTestWidget(WidgetId::new());
        let children: Vec<&dyn Widget> = vec![&w1, &w2];

        let layout = StackLayout::new();
        let parent = Rect::new(10.0, 20.0, 200.0, 100.0);
        let rects = layout.compute(parent, &children);

        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0], parent);
        assert_eq!(rects[1], parent);
    }
}
