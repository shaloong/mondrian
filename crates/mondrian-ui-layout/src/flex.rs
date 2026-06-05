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

use glam::Vec2;
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
        Self { direction: FlexDirection::Row, ..Default::default() }
    }

    pub fn column() -> Self {
        Self { direction: FlexDirection::Column, ..Default::default() }
    }

    /// 给定父 bounds 和子 widget 列表，计算每个子的 Rect
    pub fn compute(&self, parent: Rect, children: &[&dyn Widget]) -> Vec<Rect> {
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
        let n = children.len() as f32;

        // ── Step 1: Measure all children ──
        let constraint = LayoutConstraint::LOOSE;
        let measured: Vec<Size> = children.iter().map(|c| c.measure(constraint)).collect();

        // ── Step 2: Main axis allocation ──
        let main_size = if is_row { inner.width } else { inner.height };
        let total_gap = self.gap * (n - 1.0).max(0.0);
        let total_preferred: f32 = measured.iter().map(|s| if is_row { s.width } else { s.height }).sum();
        let remaining = (main_size - total_preferred - total_gap).max(0.0);

        let total_flex: f32 = 1.0; // currently all flex equally

        let mut main_sizes: Vec<f32> = Vec::with_capacity(children.len());
        for (i, m) in measured.iter().enumerate() {
            let preferred = if is_row { m.width } else { m.height };
            let flex_share = if total_flex > 0.0 && i == children.len() - 1 {
                remaining
            } else if total_flex > 0.0 {
                remaining / n
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
            JustifyContent::SpaceBetween if n > 1.0 => {
                (main_size - total_final_main).max(0.0) / (n - 1.0)
            }
            _ => 0.0,
        };

        let mut rects = Vec::with_capacity(children.len());
        let mut cursor = start_offset;

        for i in 0..children.len() {
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
    use mondrian_ui_core::types::{LayoutConstraint, Point, Size, WidgetId};
    use mondrian_ui_core::widget::{EventContext, PaintContext};
    use mondrian_ui_core::{EventResult, UiEvent};

    struct TestWidget {
        id: WidgetId,
        preferred: Size,
    }
    impl Widget for TestWidget {
        fn id(&self) -> WidgetId { self.id }
        fn measure(&self, _c: LayoutConstraint) -> Size { self.preferred }
        fn layout(&mut self, _b: Rect) {}
        fn event(&mut self, _e: &UiEvent, _ctx: &mut EventContext) -> EventResult { EventResult::Ignored }
        fn paint(&self, _ctx: &PaintContext) {}
    }

    #[test]
    fn column_layout_stacks_vertically() {
        let w1 = TestWidget { id: WidgetId::new(), preferred: Size::new(100.0, 30.0) };
        let w2 = TestWidget { id: WidgetId::new(), preferred: Size::new(100.0, 40.0) };
        let children: Vec<&dyn Widget> = vec![&w1, &w2];

        let layout = FlexLayout::column();
        let parent = Rect::new(0.0, 0.0, 200.0, 200.0);
        let rects = layout.compute(parent, &children);

        assert_eq!(rects.len(), 2);
        assert!(rects[1].y > rects[0].y, "second child should be below first");
    }

    #[test]
    fn row_layout_aligns_horizontally() {
        let w1 = TestWidget { id: WidgetId::new(), preferred: Size::new(50.0, 30.0) };
        let w2 = TestWidget { id: WidgetId::new(), preferred: Size::new(60.0, 30.0) };
        let children: Vec<&dyn Widget> = vec![&w1, &w2];

        let layout = FlexLayout::row();
        let parent = Rect::new(0.0, 0.0, 200.0, 100.0);
        let rects = layout.compute(parent, &children);

        assert_eq!(rects.len(), 2);
        assert!(rects[1].x > rects[0].x, "second child should be right of first");
    }

    #[test]
    fn center_alignment() {
        let w1 = TestWidget { id: WidgetId::new(), preferred: Size::new(50.0, 30.0) };
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
}
