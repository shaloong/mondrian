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
    /// 递归执行 layout：从根开始，深度优先
    ///
    /// 先调用根的 `measure` + `layout`，然后对每个子递归。
    pub fn layout(root: &mut dyn Widget, bounds: Rect) {
        // Use constraint to let widget know max space
        let constraint = LayoutConstraint::loose(bounds.width, bounds.height);
        let _measured = root.measure(constraint);
        root.layout(bounds);

        // Recurse into children
        for child in root.children_mut() {
            // Children layout is handled by parent's layout() call
            // which should set child bounds via child.layout(child_bounds)
            _ = child;
        }
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
        // Stage B: simple depth-first traversal placeholder
        None
    }

    /// 在 Widget 树中查找上一个可聚焦的 Widget（Shift+Tab 顺序）
    pub fn focus_prev(_tree: &mut dyn WidgetTree, _from: WidgetId) -> Option<WidgetId> {
        None
    }
}
