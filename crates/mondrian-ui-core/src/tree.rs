//! Widget 树遍历器
//!
//! 提供递归遍历 Widget 树的工具函数：layout、paint、事件冒泡、焦点遍历。

use crate::types::{LayoutConstraint, Rect, WidgetId};
use crate::widget::{DrawCommandEncoder, PaintContext};
use crate::Widget;
use mondrian_ui_theme::Theme;

/// Widget 树接口 —— 随机访问 Widget 节点
///
/// 这是事件路由和焦点系统访问 Widget 树的方式。
/// 实现者通常是应用的根 Widget 容器。
pub trait WidgetTree {
    fn get(&self, id: WidgetId) -> Option<&dyn Widget>;
    fn get_mut(&mut self, id: WidgetId) -> Option<&mut dyn Widget>;
    fn root_id(&self) -> WidgetId;
    fn parent_id(&self, id: WidgetId) -> Option<WidgetId>;
    fn children_ids(&self, id: WidgetId) -> Vec<WidgetId>;
}

/// Widget 树遍历工具
pub struct TreeWalker;

impl TreeWalker {
    /// 触发整棵 Widget 树的布局。
    ///
    /// 对根 Widget 执行 `measure` + `layout`。
    /// 每个父 Widget 的 `layout()` 负责递归布局其所有子 Widget。
    pub fn layout(root: &mut dyn Widget, bounds: Rect) {
        let constraint = LayoutConstraint::loose(bounds.width, bounds.height);
        let _measured = root.measure(constraint);
        root.layout(bounds);
    }

    /// 递归执行 paint：前序遍历
    ///
    /// 按深度优先顺序收集绘制命令到 encoder。
    pub fn paint(root: &dyn Widget, encoder: &mut dyn DrawCommandEncoder, theme: &Theme) {
        let clip_rect = Rect::new(0.0, 0.0, f32::MAX, f32::MAX);
        {
            let mut ctx = PaintContext { encoder, theme, clip_rect };
            root.paint(&mut ctx);
        }

        for child in root.children() {
            Self::paint(child.as_ref(), encoder, theme);
        }
    }

    /// 在 Widget 树中查找下一个可聚焦的 Widget（Tab 顺序）
    pub fn focus_next(_tree: &mut dyn WidgetTree, _from: WidgetId) -> Option<WidgetId> {
        None
    }

    /// 在 Widget 树中查找上一个可聚焦的 Widget（Shift+Tab 顺序）
    pub fn focus_prev(_tree: &mut dyn WidgetTree, _from: WidgetId) -> Option<WidgetId> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EventResult, LayoutConstraint, Point, Size};
    use crate::widget::EventContext;
    use crate::UiEvent;
    use crate::WidgetId;
    use mondrian_core::Color;
    use std::collections::HashMap;

    // ═══════════════════════════════════════════════════════════════════════
    // Simple Widget with child support for tree-walking tests
    // ═══════════════════════════════════════════════════════════════════════

    /// A widget that can hold children (as Vec<Box<dyn Widget>>)
    struct ParentWidget {
        id: WidgetId,
        children: Vec<Box<dyn Widget>>,
        bounds: Rect,
    }

    impl ParentWidget {
        fn new(children: Vec<Box<dyn Widget>>) -> Self {
            Self { id: WidgetId::new(), children, bounds: Rect::ZERO }
        }
    }

    impl Widget for ParentWidget {
        fn id(&self) -> WidgetId { self.id }

        fn measure(&self, _constraint: LayoutConstraint) -> Size {
            Size::new(200.0, 200.0)
        }

        fn layout(&mut self, bounds: Rect) {
            self.bounds = bounds;
            for child in &mut self.children {
                child.layout(bounds);
            }
        }

        fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            EventResult::Ignored
        }

        fn paint(&self, ctx: &mut PaintContext) {
            ctx.encoder.draw_rect(self.bounds, Color::from_hex(0x0000FF), 0.0);
        }

        fn children(&self) -> &[Box<dyn Widget>] {
            &self.children
        }

        fn children_mut(&mut self) -> &mut [Box<dyn Widget>] {
            &mut self.children
        }
    }

    struct LeafWidget {
        id: WidgetId,
        bounds: Rect,
        painted_count: std::cell::Cell<u32>,
    }

    impl LeafWidget {
        fn new() -> Self {
            Self { id: WidgetId::new(), bounds: Rect::ZERO, painted_count: std::cell::Cell::new(0) }
        }
    }

    impl Widget for LeafWidget {
        fn id(&self) -> WidgetId { self.id }

        fn measure(&self, _constraint: LayoutConstraint) -> Size {
            Size::new(100.0, 100.0)
        }

        fn layout(&mut self, bounds: Rect) {
            self.bounds = bounds;
        }

        fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            EventResult::Ignored
        }

        fn paint(&self, ctx: &mut PaintContext) {
            self.painted_count.set(self.painted_count.get() + 1);
            ctx.encoder.draw_rect(self.bounds, Color::from_hex(0xFF0000), 0.0);
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Mock encoder
    // ═══════════════════════════════════════════════════════════════════════

    struct MockEncoder {
        rect_count: usize,
    }

    impl MockEncoder {
        fn new() -> Self { Self { rect_count: 0 } }
    }

    impl DrawCommandEncoder for MockEncoder {
        fn push_clip(&mut self, _bounds: Rect) {}
        fn pop_clip(&mut self) {}
        fn draw_rect(&mut self, _bounds: Rect, _color: Color, _corner_radius: f32) {
            self.rect_count += 1;
        }
        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {}
        fn draw_text(&mut self, _text: &str, _font_size: f32, _position: Point, _color: Color) {}
        fn push_translate(&mut self, _offset: glam::Vec2) {}
        fn pop_transform(&mut self) {}
    }

    // ═══════════════════════════════════════════════════════════════════════
    // TreeWalker tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn tree_walker_layout_calls_root_layout() {
        let leaf = LeafWidget::new();
        let mut parent = ParentWidget::new(vec![Box::new(leaf)]);
        let bounds = Rect::new(0.0, 0.0, 800.0, 600.0);

        TreeWalker::layout(&mut parent, bounds);
        assert_eq!(parent.bounds, bounds);
    }

    #[test]
    fn tree_walker_paint_visits_root() {
        let leaf = LeafWidget::new();
        let parent = ParentWidget::new(vec![Box::new(leaf)]);
        let mut encoder = MockEncoder::new();
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();

        TreeWalker::paint(&parent, &mut encoder, &theme);
        // Root is painted once
        assert!(encoder.rect_count >= 1);
    }

    #[test]
    fn tree_walker_paint_recurse_into_children() {
        let leaf1 = LeafWidget::new();
        let leaf2 = LeafWidget::new();
        let parent = ParentWidget::new(vec![Box::new(leaf1), Box::new(leaf2)]);
        let children = parent.children();

        assert_eq!(children.len(), 2);
        // TreeWalker will iterate children and call paint on each
        let mut encoder = MockEncoder::new();
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();

        TreeWalker::paint(&parent, &mut encoder, &theme);
        // root (1) + child1 (1) + child2 (1) = 3 rects
        assert_eq!(encoder.rect_count, 3);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // WidgetTree trait
    // ═══════════════════════════════════════════════════════════════════════

    struct TestTree {
        nodes: HashMap<WidgetId, Box<dyn Widget>>,
        parents: HashMap<WidgetId, WidgetId>,
        children: HashMap<WidgetId, Vec<WidgetId>>,
        root: WidgetId,
    }

    impl WidgetTree for TestTree {
        fn get(&self, id: WidgetId) -> Option<&dyn Widget> {
            self.nodes.get(&id).map(|w| w.as_ref())
        }

        fn get_mut(&mut self, id: WidgetId) -> Option<&mut dyn Widget> {
            match self.nodes.get_mut(&id) {
                Some(w) => Some(w.as_mut()),
                None => None,
            }
        }

        fn root_id(&self) -> WidgetId {
            self.root
        }

        fn parent_id(&self, id: WidgetId) -> Option<WidgetId> {
            self.parents.get(&id).copied()
        }

        fn children_ids(&self, id: WidgetId) -> Vec<WidgetId> {
            self.children.get(&id).cloned().unwrap_or_default()
        }
    }

    #[test]
    fn widget_tree_get_returns_root() {
        let root_id = WidgetId::new();
        let mut nodes = HashMap::new();
        nodes.insert(root_id, Box::new(LeafWidget::new()) as Box<dyn Widget>);

        let tree = TestTree {
            nodes,
            parents: HashMap::new(),
            children: HashMap::new(),
            root: root_id,
        };

        assert!(tree.get(root_id).is_some());
        assert_eq!(tree.root_id(), root_id);
        assert!(tree.parent_id(root_id).is_none());
    }

    #[test]
    fn widget_tree_parent_child_relationship() {
        let root_id = WidgetId::new();
        let child_id = WidgetId::new();
        let mut nodes = HashMap::new();
        nodes.insert(root_id, Box::new(LeafWidget::new()) as Box<dyn Widget>);
        nodes.insert(child_id, Box::new(LeafWidget::new()) as Box<dyn Widget>);

        let mut parents = HashMap::new();
        parents.insert(child_id, root_id);

        let mut children_map = HashMap::new();
        children_map.insert(root_id, vec![child_id]);

        let tree = TestTree {
            nodes,
            parents,
            children: children_map,
            root: root_id,
        };

        assert_eq!(tree.parent_id(child_id), Some(root_id));
        assert_eq!(tree.children_ids(root_id), vec![child_id]);
        assert!(tree.children_ids(child_id).is_empty());
    }

    #[test]
    fn tree_walker_focus_next_returns_none() {
        let root_id = WidgetId::new();
        let mut nodes = HashMap::new();
        nodes.insert(root_id, Box::new(LeafWidget::new()) as Box<dyn Widget>);

        let mut tree = TestTree {
            nodes,
            parents: HashMap::new(),
            children: HashMap::new(),
            root: root_id,
        };

        // Stage B placeholder — returns None
        assert_eq!(TreeWalker::focus_next(&mut tree, root_id), None);
        assert_eq!(TreeWalker::focus_prev(&mut tree, root_id), None);
    }
}
