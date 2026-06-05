//! HitTest —— 确定鼠标位置命中的最深层 Widget

use mondrian_ui_core::types::{Point, WidgetId};
use mondrian_ui_core::WidgetTree;

/// 从根开始递归，找到鼠标位置下的最深层 Widget
///
/// 返回 None 表示没有命中任何 Widget。
pub fn hit_test_deepest(tree: &dyn WidgetTree, position: Point) -> Option<WidgetId> {
    hit_test_recursive(tree, tree.root_id(), position).last().copied()
}

/// 返回从根到最深命中 Widget 的完整路径
pub fn hit_test_path(tree: &dyn WidgetTree, position: Point) -> Vec<WidgetId> {
    hit_test_recursive(tree, tree.root_id(), position)
}

fn hit_test_recursive(tree: &dyn WidgetTree, node_id: WidgetId, position: Point) -> Vec<WidgetId> {
    let Some(widget) = tree.get(node_id) else {
        return vec![];
    };

    if widget.hit_test(position) {
        let mut path = vec![node_id];

        // 递归搜索子节点（后添加的在上层）
        let children = tree.children_ids(node_id);
        for child_id in children.iter().rev() {
            let child_path = hit_test_recursive(tree, *child_id, position);
            if !child_path.is_empty() {
                path.extend(child_path);
                break;
            }
        }

        path
    } else {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_ui_core::types::{LayoutConstraint, Rect, Size};
    use mondrian_ui_core::widget::{EventContext, PaintContext};
    use mondrian_ui_core::{EventResult, UiEvent, Widget};

    struct HitWidget {
        id: WidgetId,
        bounds: Rect,
        children: Vec<Box<dyn Widget>>,
    }
    impl Widget for HitWidget {
        fn id(&self) -> WidgetId { self.id }
        fn measure(&self, _c: LayoutConstraint) -> Size { Size::new(self.bounds.width, self.bounds.height) }
        fn layout(&mut self, _b: Rect) {}
        fn event(&mut self, _e: &UiEvent, _ctx: &mut EventContext) -> EventResult { EventResult::Ignored }
        fn paint(&self, _ctx: &PaintContext) {}
        fn hit_test(&self, point: Point) -> bool { self.bounds.contains(point) }
        fn children(&self) -> &[Box<dyn Widget>] { &self.children }
        fn children_mut(&mut self) -> &mut [Box<dyn Widget>] { &mut self.children }
    }

    struct TestTree {
        widgets: std::collections::HashMap<WidgetId, HitWidget>,
        parents: std::collections::HashMap<WidgetId, WidgetId>,
        root: WidgetId,
    }
    impl WidgetTree for TestTree {
        fn get(&self, id: WidgetId) -> Option<&dyn Widget> {
            self.widgets.get(&id).map(|w| w as &dyn Widget)
        }
        fn get_mut(&mut self, id: WidgetId) -> Option<&mut dyn Widget> {
            self.widgets.get_mut(&id).map(|w| w as &mut dyn Widget)
        }
        fn root_id(&self) -> WidgetId { self.root }
        fn parent_id(&self, id: WidgetId) -> Option<WidgetId> {
            self.parents.get(&id).copied()
        }
        fn children_ids(&self, id: WidgetId) -> Vec<WidgetId> {
            self.widgets.get(&id).map(|w| w.children.iter().map(|c| c.id()).collect()).unwrap_or_default()
        }
    }

    #[test]
    fn hit_test_finds_deepest() {
        let parent_id = WidgetId::new();
        let child_id = WidgetId::new();

        let child = HitWidget {
            id: child_id,
            bounds: Rect::new(10.0, 10.0, 80.0, 80.0),
            children: vec![],
        };
        let parent = HitWidget {
            id: parent_id,
            bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
            children: vec![Box::new(child)],
        };

        let mut widgets = std::collections::HashMap::new();
        widgets.insert(parent_id, parent);
        widgets.insert(child_id, HitWidget {
            id: child_id, bounds: Rect::new(10.0, 10.0, 80.0, 80.0), children: vec![]
        });

        let mut parents = std::collections::HashMap::new();
        parents.insert(child_id, parent_id);

        let tree = TestTree { widgets, parents, root: parent_id };

        let hit = hit_test_deepest(&tree, Point::new(50.0, 50.0));
        assert_eq!(hit, Some(child_id), "should hit child at center");

        let miss = hit_test_deepest(&tree, Point::new(200.0, 200.0));
        assert_eq!(miss, None, "should miss outside");
    }
}
