//! Flex 布局算法
//!
//! 类似 CSS Flexbox 的约束求解布局。
//!
//! ## 算法流程
//!
//! 1. Measure: 对每个子调用 `measure(constraint)`
//! 2. 主轴分配: flex_grow=0 取期望尺寸，剩余空间按 flex_grow 比例分配
//! 3. 交叉轴: 根据 align_items 决定每个子的位置/尺寸
//! 4. 主轴排列: 根据 justify_content 决定起始偏移

use mondrian_ui_core::types::{LayoutConstraint, Rect, Size};
use mondrian_ui_core::Widget;

use crate::constraint::RectInsets;

/// Flex 排版方向
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlexDirection {
    Row,
    Column,
}

/// 交叉轴对齐
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignItems {
    Start,
    Center,
    End,
    Stretch,
}

/// 主轴分布
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JustifyContent {
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
}

/// Flex 布局配置
#[derive(Debug, Clone)]
pub struct FlexLayout {
    pub direction: FlexDirection,
    pub align_items: AlignItems,
    pub justify_content: JustifyContent,
    pub gap: f32,
    pub padding: RectInsets,
}

impl Default for FlexLayout {
    fn default() -> Self {
        Self {
            direction: FlexDirection::Column,
            align_items: AlignItems::Start,
            justify_content: JustifyContent::Start,
            gap: 0.0,
            padding: RectInsets::default(),
        }
    }
}

impl FlexLayout {
    pub fn row() -> Self {
        Self {
            direction: FlexDirection::Row,
            ..Default::default()
        }
    }

    pub fn column() -> Self {
        Self {
            direction: FlexDirection::Column,
            ..Default::default()
        }
    }

    /// 给定父 bounds 和子 widget 列表，计算每个子的 Rect。
    /// 所有子等权重分配剩余空间（flex_grow = 1.0）。
    pub fn compute(&self, parent: Rect, children: &[&dyn Widget]) -> Vec<Rect> {
        let flex_grows = vec![1.0; children.len()];
        self.compute_with_flex(parent, children, &flex_grows)
    }

    /// 给定父 bounds、子 widget 列表和每个子的 flex_grow 权重，计算每个子的 Rect。
    ///
    /// `flex_grows` 长度必须等于 `children` 长度。
    /// flex_grow = 0 表示子节点只占期望尺寸，不参与剩余空间分配。
    /// 剩余空间按 flex_grow 的比例分配。
    pub fn compute_with_flex(
        &self,
        parent: Rect,
        children: &[&dyn Widget],
        flex_grows: &[f32],
    ) -> Vec<Rect> {
        assert_eq!(
            children.len(),
            flex_grows.len(),
            "flex_grows must match children count"
        );

        if children.is_empty() {
            return vec![];
        }

        let inner = Rect::new(
            parent.x + self.padding.left,
            parent.y + self.padding.top,
            parent.width - self.padding.left - self.padding.right,
            parent.height - self.padding.top - self.padding.bottom,
        );

        let is_row = matches!(self.direction, FlexDirection::Row);
        let n = children.len();

        // ── Step 1: Measure all children ──
        //
        // The parent bounds are already known during layout, so children must
        // see the real cross-axis limit. Wrapped text, scroll views, and form
        // controls derive their preferred main-axis size from that width/height;
        // measuring them with an unbounded constraint makes the first layout
        // disagree with the eventual child rects.
        let child_constraint = LayoutConstraint {
            min: Size::ZERO,
            max: Size::new(inner.width.max(0.0), inner.height.max(0.0)),
        };
        let measured: Vec<Size> = children.iter().map(|c| c.measure(child_constraint)).collect();

        // ── Step 2: Main axis allocation ──
        let main_size = if is_row { inner.width } else { inner.height };
        let total_gap = self.gap * (n as f32 - 1.0).max(0.0);
        let total_preferred: f32 =
            measured.iter().map(|s| if is_row { s.width } else { s.height }).sum();
        let remaining = (main_size - total_preferred - total_gap).max(0.0);
        let total_flex: f32 = flex_grows.iter().sum();

        let mut main_sizes: Vec<f32> = Vec::with_capacity(n);
        for (i, m) in measured.iter().enumerate() {
            let preferred = if is_row { m.width } else { m.height };
            let flex_share = if total_flex > 0.0 {
                remaining * flex_grows[i] / total_flex
            } else {
                0.0
            };
            main_sizes.push(preferred + flex_share);
        }

        // ── Step 3: Cross axis ──
        let cross_size = if is_row { inner.height } else { inner.width };
        let cross_sizes: Vec<f32> = measured
            .iter()
            .map(|m| match self.align_items {
                AlignItems::Stretch => cross_size,
                _ => (if is_row { m.height } else { m.width }).min(cross_size),
            })
            .collect();

        // ── Step 4: Main axis positioning ──
        let total_final_main: f32 = main_sizes.iter().sum::<f32>() + total_gap;
        let start_offset = match self.justify_content {
            JustifyContent::Start => 0.0,
            JustifyContent::Center => (main_size - total_final_main).max(0.0) * 0.5,
            JustifyContent::End => (main_size - total_final_main).max(0.0),
            JustifyContent::SpaceBetween => 0.0,
            JustifyContent::SpaceAround => (main_size - total_final_main).max(0.0) * 0.5,
        };

        let space_between_gap = match self.justify_content {
            JustifyContent::SpaceBetween if n > 1 => {
                (main_size - total_final_main).max(0.0) / (n as f32 - 1.0)
            }
            _ => 0.0,
        };

        let mut rects = Vec::with_capacity(n);
        let mut cursor = start_offset;

        for i in 0..n {
            let cross_pos = match self.align_items {
                AlignItems::Start => 0.0,
                AlignItems::Center => (cross_size - cross_sizes[i]).max(0.0) * 0.5,
                AlignItems::End => (cross_size - cross_sizes[i]).max(0.0),
                AlignItems::Stretch => 0.0,
            };

            let rect = if is_row {
                Rect::new(
                    inner.x + cursor,
                    inner.y + cross_pos,
                    main_sizes[i],
                    cross_sizes[i],
                )
            } else {
                Rect::new(
                    inner.x + cross_pos,
                    inner.y + cursor,
                    cross_sizes[i],
                    main_sizes[i],
                )
            };

            rects.push(rect);
            cursor += main_sizes[i] + self.gap + space_between_gap;
        }

        rects
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_ui_core::types::{LayoutConstraint, Size, WidgetId};
    use mondrian_ui_core::widget::{EventContext, PaintContext};
    use mondrian_ui_core::{EventResult, UiEvent};
    use std::cell::RefCell;
    use std::rc::Rc;

    struct TestWidget {
        id: WidgetId,
        preferred: Size,
    }
    impl Widget for TestWidget {
        fn id(&self) -> WidgetId {
            self.id
        }
        fn measure(&self, _c: LayoutConstraint) -> Size {
            self.preferred
        }
        fn layout(&mut self, _b: Rect) {}
        fn event(&mut self, _e: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            EventResult::Ignored
        }
        fn paint(&self, _ctx: &mut PaintContext) {}
    }

    struct ConstraintRecordingWidget {
        id: WidgetId,
        seen: Rc<RefCell<Vec<LayoutConstraint>>>,
    }

    impl Widget for ConstraintRecordingWidget {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, constraint: LayoutConstraint) -> Size {
            self.seen.borrow_mut().push(constraint);
            Size::new(
                constraint.max.width.min(40.0),
                constraint.max.height.min(30.0),
            )
        }

        fn layout(&mut self, _b: Rect) {}

        fn event(&mut self, _e: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            EventResult::Ignored
        }

        fn paint(&self, _ctx: &mut PaintContext) {}
    }

    #[test]
    fn column_layout_stacks_vertically() {
        let w1 = TestWidget {
            id: WidgetId::new(),
            preferred: Size::new(100.0, 30.0),
        };
        let w2 = TestWidget {
            id: WidgetId::new(),
            preferred: Size::new(100.0, 40.0),
        };
        let children: Vec<&dyn Widget> = vec![&w1, &w2];

        let layout = FlexLayout::column();
        let parent = Rect::new(0.0, 0.0, 200.0, 200.0);
        let rects = layout.compute(parent, &children);

        assert_eq!(rects.len(), 2);
        assert!(
            rects[1].y > rects[0].y,
            "second child should be below first"
        );
    }

    #[test]
    fn row_layout_aligns_horizontally() {
        let w1 = TestWidget {
            id: WidgetId::new(),
            preferred: Size::new(50.0, 30.0),
        };
        let w2 = TestWidget {
            id: WidgetId::new(),
            preferred: Size::new(60.0, 30.0),
        };
        let children: Vec<&dyn Widget> = vec![&w1, &w2];

        let layout = FlexLayout::row();
        let parent = Rect::new(0.0, 0.0, 200.0, 100.0);
        let rects = layout.compute(parent, &children);

        assert_eq!(rects.len(), 2);
        assert!(
            rects[1].x > rects[0].x,
            "second child should be right of first"
        );
    }

    #[test]
    fn center_alignment() {
        let w1 = TestWidget {
            id: WidgetId::new(),
            preferred: Size::new(50.0, 30.0),
        };
        let children: Vec<&dyn Widget> = vec![&w1];

        let layout = FlexLayout {
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..FlexLayout::column()
        };
        let parent = Rect::new(0.0, 0.0, 200.0, 200.0);
        let rects = layout.compute(parent, &children);

        assert_eq!(rects.len(), 1);
        assert!(rects[0].x > 0.0, "should be centered horizontally");
    }

    #[test]
    fn space_between_justify() {
        let w1 = TestWidget {
            id: WidgetId::new(),
            preferred: Size::new(30.0, 20.0),
        };
        let w2 = TestWidget {
            id: WidgetId::new(),
            preferred: Size::new(30.0, 20.0),
        };
        let w3 = TestWidget {
            id: WidgetId::new(),
            preferred: Size::new(30.0, 20.0),
        };
        let children: Vec<&dyn Widget> = vec![&w1, &w2, &w3];

        let layout = FlexLayout {
            justify_content: JustifyContent::SpaceBetween,
            ..FlexLayout::column()
        };
        let parent = Rect::new(0.0, 0.0, 200.0, 200.0);
        let rects = layout.compute(parent, &children);

        assert_eq!(rects.len(), 3);
        assert!((rects[0].y - 0.0).abs() < 0.01, "first should be at top");
        assert!(rects[1].y > rects[0].y, "middle between first and last");
        assert!(rects[2].y > rects[1].y, "last below middle");
    }

    #[test]
    fn end_alignment() {
        let w1 = TestWidget {
            id: WidgetId::new(),
            preferred: Size::new(50.0, 30.0),
        };
        let children: Vec<&dyn Widget> = vec![&w1];

        let layout = FlexLayout { align_items: AlignItems::End, ..FlexLayout::row() };
        let parent = Rect::new(0.0, 0.0, 200.0, 100.0);
        let rects = layout.compute(parent, &children);

        assert_eq!(rects.len(), 1);
        assert!(rects[0].y > 0.0, "should be at bottom");
    }

    #[test]
    fn flex_with_gap() {
        let w1 = TestWidget {
            id: WidgetId::new(),
            preferred: Size::new(100.0, 20.0),
        };
        let w2 = TestWidget {
            id: WidgetId::new(),
            preferred: Size::new(100.0, 20.0),
        };
        let children: Vec<&dyn Widget> = vec![&w1, &w2];

        let no_gap = FlexLayout::column();
        let with_gap = FlexLayout { gap: 8.0, ..FlexLayout::column() };

        let parent = Rect::new(0.0, 0.0, 200.0, 200.0);
        let rects_no_gap = no_gap.compute(parent, &children);
        let rects_with_gap = with_gap.compute(parent, &children);

        assert!(rects_with_gap[1].y > rects_no_gap[1].y);
    }

    #[test]
    fn flex_empty_children() {
        let children: Vec<&dyn Widget> = vec![];
        let layout = FlexLayout::column();
        let parent = Rect::new(0.0, 0.0, 200.0, 200.0);
        let rects = layout.compute(parent, &children);
        assert!(rects.is_empty());
    }

    #[test]
    fn flex_with_padding() {
        let w1 = TestWidget {
            id: WidgetId::new(),
            preferred: Size::new(100.0, 30.0),
        };
        let children: Vec<&dyn Widget> = vec![&w1];

        let layout = FlexLayout {
            padding: crate::constraint::RectInsets::all(10.0),
            ..FlexLayout::column()
        };
        let parent = Rect::new(0.0, 0.0, 200.0, 200.0);
        let rects = layout.compute(parent, &children);

        assert_eq!(rects.len(), 1);
        assert!(rects[0].x >= 10.0);
        assert!(rects[0].y >= 10.0);
    }

    #[test]
    fn flex_layout_measures_children_with_parent_bounds() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let child = ConstraintRecordingWidget { id: WidgetId::new(), seen: Rc::clone(&seen) };
        let children: Vec<&dyn Widget> = vec![&child];
        let layout = FlexLayout {
            padding: crate::constraint::RectInsets {
                left: 8.0,
                right: 12.0,
                top: 4.0,
                bottom: 6.0,
            },
            ..FlexLayout::column()
        };

        let rects = layout.compute(Rect::new(0.0, 0.0, 100.0, 80.0), &children);

        assert_eq!(
            seen.borrow().as_slice(),
            &[LayoutConstraint { min: Size::ZERO, max: Size::new(80.0, 70.0) }]
        );
        assert_eq!(rects.len(), 1);
    }

    #[test]
    fn stretch_cross_axis() {
        let w1 = TestWidget {
            id: WidgetId::new(),
            preferred: Size::new(50.0, 30.0),
        };
        let children: Vec<&dyn Widget> = vec![&w1];

        let layout = FlexLayout {
            align_items: AlignItems::Stretch,
            ..FlexLayout::row()
        };
        let parent = Rect::new(0.0, 0.0, 200.0, 100.0);
        let rects = layout.compute(parent, &children);

        assert_eq!(rects.len(), 1);
        assert!(rects[0].height > 30.0, "should stretch to fill cross-axis");
    }

    #[test]
    fn flex_grow_distributes_remaining_space() {
        let w1 = TestWidget {
            id: WidgetId::new(),
            preferred: Size::new(50.0, 20.0),
        };
        let w2 = TestWidget {
            id: WidgetId::new(),
            preferred: Size::new(50.0, 20.0),
        };
        let children: Vec<&dyn Widget> = vec![&w1, &w2];

        let layout = FlexLayout::column();
        let parent = Rect::new(0.0, 0.0, 200.0, 200.0);
        // w1 flex_grow=2, w2 flex_grow=1 → w1 gets 2/3 of remaining, w2 gets 1/3
        let rects = layout.compute_with_flex(parent, &children, &[2.0, 1.0]);

        assert_eq!(rects.len(), 2);
        // remaining = 200 - 20 - 20 = 160 → w1 gets 2/3, w2 gets 1/3
        assert!(
            rects[0].height > rects[1].height,
            "flex_grow=2 should get more space than flex_grow=1"
        );
    }

    #[test]
    fn flex_grow_zero_uses_preferred_only() {
        let w1 = TestWidget {
            id: WidgetId::new(),
            preferred: Size::new(50.0, 30.0),
        };
        let w2 = TestWidget {
            id: WidgetId::new(),
            preferred: Size::new(50.0, 30.0),
        };
        let children: Vec<&dyn Widget> = vec![&w1, &w2];

        let layout = FlexLayout::column();
        let parent = Rect::new(0.0, 0.0, 200.0, 200.0);
        let rects = layout.compute_with_flex(parent, &children, &[0.0, 1.0]);

        assert_eq!(rects.len(), 2);
        assert_eq!(
            rects[0].height, 30.0,
            "flex_grow=0 should use preferred height only"
        );
    }
}
