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

/// Borrowed adapter that exposes any `Widget` subtree through `WidgetTree`.
///
/// This is useful for demos and simple app shells that already own a root
/// widget tree and do not maintain a parallel retained node map.
pub struct WidgetTreeView<'a> {
    root: &'a mut dyn Widget,
    root_id: WidgetId,
}

impl<'a> WidgetTreeView<'a> {
    pub fn new(root: &'a mut dyn Widget) -> Self {
        let root_id = root.id();
        Self { root, root_id }
    }
}

impl WidgetTree for WidgetTreeView<'_> {
    fn get(&self, id: WidgetId) -> Option<&dyn Widget> {
        find_widget(self.root, id)
    }

    fn get_mut(&mut self, id: WidgetId) -> Option<&mut dyn Widget> {
        let path = find_widget_path(self.root, id)?;
        widget_mut_at_path(self.root, &path)
    }

    fn root_id(&self) -> WidgetId {
        self.root_id
    }

    fn parent_id(&self, id: WidgetId) -> Option<WidgetId> {
        find_parent_id(self.root, id, None)
    }

    fn children_ids(&self, id: WidgetId) -> Vec<WidgetId> {
        find_widget(self.root, id).map(child_ids).unwrap_or_default()
    }
}

fn find_widget(widget: &dyn Widget, id: WidgetId) -> Option<&dyn Widget> {
    if widget.id() == id {
        return Some(widget);
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child(index)
            && let Some(found) = find_widget(child, id)
        {
            return Some(found);
        }
    }
    None
}

fn child_ids(widget: &dyn Widget) -> Vec<WidgetId> {
    let mut ids = Vec::with_capacity(widget.child_count());
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child(index) {
            ids.push(child.id());
        }
    }
    ids
}

fn find_widget_path(widget: &dyn Widget, id: WidgetId) -> Option<Vec<usize>> {
    if widget.id() == id {
        return Some(Vec::new());
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child(index)
            && let Some(mut path) = find_widget_path(child, id)
        {
            path.insert(0, index);
            return Some(path);
        }
    }
    None
}

fn widget_mut_at_path<'a>(
    widget: &'a mut dyn Widget,
    path: &[usize],
) -> Option<&'a mut dyn Widget> {
    let Some((&index, rest)) = path.split_first() else {
        return Some(widget);
    };
    let child = widget.child_mut(index)?;
    widget_mut_at_path(child, rest)
}

fn find_parent_id(
    widget: &dyn Widget,
    target: WidgetId,
    parent: Option<WidgetId>,
) -> Option<WidgetId> {
    if widget.id() == target {
        return parent;
    }
    let current = Some(widget.id());
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child(index)
            && let Some(found) = find_parent_id(child, target, current)
        {
            return Some(found);
        }
    }
    None
}

fn collect_focusable(tree: &dyn WidgetTree) -> Vec<WidgetId> {
    let mut order = Vec::new();
    let root = tree.root_id();
    collect_focusable_recursive(tree, root, &mut order);
    order
}

fn collect_focusable_recursive(tree: &dyn WidgetTree, node: WidgetId, order: &mut Vec<WidgetId>) {
    if let Some(w) = tree.get(node)
        && w.can_focus()
    {
        order.push(node);
    }
    for child_id in tree.children_ids(node) {
        collect_focusable_recursive(tree, child_id, order);
    }
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

    /// 递归执行 paint。
    ///
    /// 只调用 root.paint()。每个 Widget 的 paint() 负责递归绘制其子节点。
    /// 这样 ScrollView 等容器可以在绘制子节点前设置 clip/transform。
    pub fn paint(root: &dyn Widget, encoder: &mut dyn DrawCommandEncoder, theme: &Theme) {
        let clip_rect = Rect::new(0.0, 0.0, f32::MAX, f32::MAX);
        Self::paint_clipped(root, encoder, theme, clip_rect);
    }

    /// Paint a widget tree with an explicit root clip rectangle.
    ///
    /// Application shells should pass the window or surface bounds here so
    /// overlay widgets can place popups, tooltips, and menus against the real
    /// visible viewport instead of an unbounded test canvas.
    pub fn paint_clipped(
        root: &dyn Widget,
        encoder: &mut dyn DrawCommandEncoder,
        theme: &Theme,
        clip_rect: Rect,
    ) {
        let mut ctx = PaintContext { encoder, theme, clip_rect };
        root.paint(&mut ctx);
        root.paint_overlay(&mut ctx);
    }

    /// 在 Widget 树中查找下一个可聚焦的 Widget（Tab 顺序）
    pub fn focus_next(tree: &dyn WidgetTree, from: WidgetId) -> Option<WidgetId> {
        let order = collect_focusable(tree);
        if order.is_empty() {
            return None;
        }
        let pos = order.iter().position(|id| *id == from);
        match pos {
            Some(i) => Some(order[(i + 1) % order.len()]),
            None => Some(order[0]),
        }
    }

    /// 在 Widget 树中查找上一个可聚焦的 Widget（Shift+Tab 顺序）
    pub fn focus_prev(tree: &dyn WidgetTree, from: WidgetId) -> Option<WidgetId> {
        let order = collect_focusable(tree);
        if order.is_empty() {
            return None;
        }
        let pos = order.iter().position(|id| *id == from);
        let next = match pos {
            Some(0) | None => order.len() - 1,
            Some(i) => i - 1,
        };
        Some(order[next])
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
        fn id(&self) -> WidgetId {
            self.id
        }

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
            Self {
                id: WidgetId::new(),
                bounds: Rect::ZERO,
                painted_count: std::cell::Cell::new(0),
            }
        }
    }

    impl Widget for LeafWidget {
        fn id(&self) -> WidgetId {
            self.id
        }

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
        rects: Vec<Rect>,
        rect_colors: Vec<Color>,
    }

    impl MockEncoder {
        fn new() -> Self {
            Self {
                rect_count: 0,
                rects: Vec::new(),
                rect_colors: Vec::new(),
            }
        }
    }

    impl DrawCommandEncoder for MockEncoder {
        fn push_clip(&mut self, _bounds: Rect) {}
        fn pop_clip(&mut self) {}
        fn draw_rect(&mut self, bounds: Rect, color: Color, _corner_radius: f32) {
            self.rect_count += 1;
            self.rects.push(bounds);
            self.rect_colors.push(color);
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
    fn tree_walker_paint_calls_root_paint_only() {
        let leaf1 = LeafWidget::new();
        let leaf2 = LeafWidget::new();
        let parent = ParentWidget::new(vec![Box::new(leaf1), Box::new(leaf2)]);

        let mut encoder = MockEncoder::new();
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();

        TreeWalker::paint(&parent, &mut encoder, &theme);
        // Only root.paint() is called by TreeWalker.
        // Children are painted by the parent widget's own paint() method.
        assert_eq!(encoder.rect_count, 1);
    }

    struct OverlayWidget {
        id: WidgetId,
    }

    impl OverlayWidget {
        fn new() -> Self {
            Self { id: WidgetId::new() }
        }
    }

    impl Widget for OverlayWidget {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, _constraint: LayoutConstraint) -> Size {
            Size::new(100.0, 100.0)
        }

        fn layout(&mut self, _bounds: Rect) {}

        fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            EventResult::Ignored
        }

        fn paint(&self, ctx: &mut PaintContext) {
            ctx.encoder.draw_rect(Rect::ZERO, Color::from_hex(0x111111), 0.0);
        }

        fn paint_overlay(&self, ctx: &mut PaintContext) {
            ctx.encoder.draw_rect(Rect::ZERO, Color::from_hex(0xEEEEEE), 0.0);
        }
    }

    struct PaintOrderChild {
        id: WidgetId,
        normal: Color,
        overlay: Option<Color>,
    }

    impl PaintOrderChild {
        fn new(normal: Color, overlay: Option<Color>) -> Self {
            Self { id: WidgetId::new(), normal, overlay }
        }
    }

    impl Widget for PaintOrderChild {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, _constraint: LayoutConstraint) -> Size {
            Size::new(100.0, 100.0)
        }

        fn layout(&mut self, _bounds: Rect) {}

        fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            EventResult::Ignored
        }

        fn paint(&self, ctx: &mut PaintContext) {
            ctx.encoder.draw_rect(Rect::ZERO, self.normal, 0.0);
        }

        fn paint_overlay(&self, ctx: &mut PaintContext) {
            if let Some(color) = self.overlay {
                ctx.encoder.draw_rect(Rect::ZERO, color, 0.0);
            }
        }
    }

    struct PaintOrderRoot {
        id: WidgetId,
        children: Vec<Box<dyn Widget>>,
    }

    impl PaintOrderRoot {
        fn new(children: Vec<Box<dyn Widget>>) -> Self {
            Self { id: WidgetId::new(), children }
        }
    }

    impl Widget for PaintOrderRoot {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, _constraint: LayoutConstraint) -> Size {
            Size::new(200.0, 100.0)
        }

        fn layout(&mut self, _bounds: Rect) {}

        fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            EventResult::Ignored
        }

        fn paint(&self, ctx: &mut PaintContext) {
            for child in self.children() {
                child.paint(ctx);
            }
        }

        fn children(&self) -> &[Box<dyn Widget>] {
            &self.children
        }

        fn children_mut(&mut self) -> &mut [Box<dyn Widget>] {
            &mut self.children
        }
    }

    #[test]
    fn tree_walker_paint_draws_overlay_after_normal_content() {
        let root = OverlayWidget::new();
        let mut encoder = MockEncoder::new();
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();

        TreeWalker::paint(&root, &mut encoder, &theme);

        assert_eq!(
            encoder.rect_colors,
            vec![Color::from_hex(0x111111), Color::from_hex(0xEEEEEE)]
        );
    }

    #[test]
    fn tree_walker_paint_draws_child_overlays_after_later_sibling_content() {
        let first_normal = Color::from_hex(0x111111);
        let second_normal = Color::from_hex(0x222222);
        let first_overlay = Color::from_hex(0xEEEEEE);
        let root = PaintOrderRoot::new(vec![
            Box::new(PaintOrderChild::new(first_normal, Some(first_overlay))),
            Box::new(PaintOrderChild::new(second_normal, None)),
        ]);
        let mut encoder = MockEncoder::new();
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();

        TreeWalker::paint(&root, &mut encoder, &theme);

        assert_eq!(
            encoder.rect_colors,
            vec![first_normal, second_normal, first_overlay],
            "all overlay chrome must paint after the complete normal content pass"
        );
    }

    struct ClipEchoWidget {
        id: WidgetId,
    }

    impl ClipEchoWidget {
        fn new() -> Self {
            Self { id: WidgetId::new() }
        }
    }

    impl Widget for ClipEchoWidget {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, _constraint: LayoutConstraint) -> Size {
            Size::new(100.0, 100.0)
        }

        fn layout(&mut self, _bounds: Rect) {}

        fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            EventResult::Ignored
        }

        fn paint(&self, _ctx: &mut PaintContext) {}

        fn paint_overlay(&self, ctx: &mut PaintContext) {
            ctx.encoder.draw_rect(ctx.clip_rect, Color::from_hex(0xEEEEEE), 0.0);
        }
    }

    #[test]
    fn tree_walker_paint_clipped_passes_root_clip_to_overlay() {
        let root = ClipEchoWidget::new();
        let mut encoder = MockEncoder::new();
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip = Rect::new(10.0, 20.0, 320.0, 240.0);

        TreeWalker::paint_clipped(&root, &mut encoder, &theme, clip);

        assert_eq!(encoder.rects, vec![clip]);
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
    fn tree_walker_focus_next_returns_none_for_empty_tree() {
        let root_id = WidgetId::new();
        let mut nodes = HashMap::new();
        nodes.insert(root_id, Box::new(LeafWidget::new()) as Box<dyn Widget>);

        let tree = TestTree {
            nodes,
            parents: HashMap::new(),
            children: HashMap::new(),
            root: root_id,
        };

        // LeafWidget doesn't override can_focus (defaults to false), so focus_next returns None
        assert_eq!(TreeWalker::focus_next(&tree, root_id), None);
        assert_eq!(TreeWalker::focus_prev(&tree, root_id), None);
    }

    struct FocusableWidget {
        id: WidgetId,
    }

    impl FocusableWidget {
        fn new(id: WidgetId) -> Self {
            Self { id }
        }
    }

    impl Widget for FocusableWidget {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, _constraint: LayoutConstraint) -> Size {
            Size::new(100.0, 100.0)
        }

        fn layout(&mut self, _bounds: Rect) {}

        fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            EventResult::Ignored
        }

        fn paint(&self, _ctx: &mut PaintContext) {}

        fn can_focus(&self) -> bool {
            true
        }
    }

    #[test]
    fn tree_walker_focus_next_cycles_through_focusable() {
        let a = WidgetId::new();
        let b = WidgetId::new();
        let c = WidgetId::new();
        let mut nodes: HashMap<WidgetId, Box<dyn Widget>> = HashMap::new();
        nodes.insert(a, Box::new(FocusableWidget::new(a)));
        nodes.insert(b, Box::new(FocusableWidget::new(b)));
        nodes.insert(c, Box::new(FocusableWidget::new(c)));

        let mut children = HashMap::new();
        children.insert(a, vec![b]);
        children.insert(b, vec![c]);
        children.insert(c, vec![]);

        let mut parents = HashMap::new();
        parents.insert(b, a);
        parents.insert(c, b);

        let tree = TestTree { nodes, parents, children, root: a };

        // Forward cycle: a -> b -> c -> a
        assert_eq!(TreeWalker::focus_next(&tree, a), Some(b));
        assert_eq!(TreeWalker::focus_next(&tree, b), Some(c));
        assert_eq!(TreeWalker::focus_next(&tree, c), Some(a));

        // Backward cycle: a -> c -> b -> a
        assert_eq!(TreeWalker::focus_prev(&tree, a), Some(c));
        assert_eq!(TreeWalker::focus_prev(&tree, c), Some(b));
        assert_eq!(TreeWalker::focus_prev(&tree, b), Some(a));
    }

    #[test]
    fn tree_walker_focus_next_unknown_id_starts_from_first() {
        let a = WidgetId::new();
        let b = WidgetId::new();
        let mut nodes: HashMap<WidgetId, Box<dyn Widget>> = HashMap::new();
        nodes.insert(a, Box::new(FocusableWidget::new(a)));
        nodes.insert(b, Box::new(FocusableWidget::new(b)));

        let children = HashMap::from([(a, vec![b]), (b, vec![])]);
        let tree = TestTree { nodes, parents: HashMap::new(), children, root: a };

        let unknown = WidgetId::new();
        assert_eq!(TreeWalker::focus_next(&tree, unknown), Some(a));
        assert_eq!(TreeWalker::focus_prev(&tree, unknown), Some(b));
    }

    #[test]
    fn widget_tree_view_finds_nested_children() {
        let child = LeafWidget::new();
        let child_id = child.id();
        let mut root = ParentWidget::new(vec![Box::new(child)]);
        let root_id = root.id();

        let view = WidgetTreeView::new(&mut root);

        assert_eq!(view.root_id(), root_id);
        assert!(view.get(root_id).is_some());
        assert!(view.get(child_id).is_some());
        assert_eq!(view.parent_id(child_id), Some(root_id));
        assert_eq!(view.children_ids(root_id), vec![child_id]);
    }

    #[test]
    fn widget_tree_view_get_mut_updates_nested_child() {
        let child = HitLeafWidget::new();
        let child_id = child.id();
        let mut root = ParentWidget::new(vec![Box::new(child)]);

        let mut view = WidgetTreeView::new(&mut root);
        let child = view.get_mut(child_id).expect("child should exist");
        child.layout(Rect::new(1.0, 2.0, 3.0, 4.0));

        let child = view.get(child_id).expect("child should still exist");
        assert!(child.hit_test(Point::new(2.0, 3.0)));
    }

    struct HitLeafWidget {
        id: WidgetId,
        bounds: Rect,
    }

    impl HitLeafWidget {
        fn new() -> Self {
            Self { id: WidgetId::new(), bounds: Rect::ZERO }
        }
    }

    impl Widget for HitLeafWidget {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, _constraint: LayoutConstraint) -> Size {
            Size::new(10.0, 10.0)
        }

        fn layout(&mut self, bounds: Rect) {
            self.bounds = bounds;
        }

        fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            EventResult::Ignored
        }

        fn paint(&self, _ctx: &mut PaintContext) {}

        fn hit_test(&self, point: Point) -> bool {
            self.bounds.contains(point)
        }
    }
}
