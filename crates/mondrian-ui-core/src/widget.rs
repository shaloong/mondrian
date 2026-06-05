//! Widget 基础 trait
//!
//! 所有 UI 控件的统一接口。
//!
//! ## 生命周期
//!
//! 1. `measure(constraint)` → 返回期望尺寸（纯计算，无副作用）
//! 2. `layout(bounds)` → 给定最终区域，计算子节点位置
//! 3. `event(event, ctx)` → 处理输入（可产生副作用如 dispatch Action）
//! 4. `paint(ctx)` → 发出绘制命令（纯读操作）
//!
//! 步骤 1-2 在布局阶段执行；步骤 3-4 在每帧执行。

use crate::focus::FocusManager;
use crate::shortcut::ShortcutManager;
use crate::tooltip::TooltipManager;
use crate::types::*;
use mondrian_editor_state::Action;
use mondrian_ui_theme::Theme;

use mondrian_platform::PlatformService;

/// Widget 布局/事件上下文（event 方法使用）
pub struct WidgetContext<'a> {
    /// 当前 Widget 的 bounds（布局后的）
    pub bounds: Rect,
    /// 焦点管理器
    pub focus: &'a mut dyn FocusManager,
    /// 快捷键管理器
    pub shortcut: &'a mut dyn ShortcutManager,
    /// Tooltip 管理器
    pub tooltip: &'a mut dyn TooltipManager,
    /// 平台服务
    pub platform: &'a dyn PlatformService,
}

/// 事件上下文（event 方法使用，可 dispatch Action）
pub struct EventContext<'a> {
    /// 焦点管理器
    pub focus: &'a mut dyn FocusManager,
    /// 快捷键管理器
    pub shortcut: &'a mut dyn ShortcutManager,
    /// Tooltip 管理器
    pub tooltip: &'a mut dyn TooltipManager,
    /// 派发 Action（闭包，由框架注入）
    pub dispatch: &'a dyn Fn(Action),
    /// 平台服务
    pub platform: &'a dyn PlatformService,
}

/// 绘制上下文（paint 方法使用）
///
/// 注意：此结构在后续 Stage 实现 wgpu 渲染时会被扩展，
/// 加入 `DrawEncoder` 等真正的绘制接口。
/// Stage A 阶段仅为 trait 骨架，PaintContext 暂为占位。
pub struct PaintContext<'a> {
    /// 当前主题
    pub theme: &'a Theme,
    /// 当前裁剪区域
    pub clip_rect: Rect,
}

/// 所有 UI 控件的统一接口
///
/// ## 实现要求
///
/// * `measure` 必须是纯函数（不修改自身状态）
/// * `paint` 必须是纯读操作（不产生副作用）
/// * `event` 中可以 dispatch Action
/// * `layout` 递归调用子 Widget 的 layout
pub trait Widget {
    /// 返回 Widget 的唯一标识
    fn id(&self) -> WidgetId;

    /// 给定约束，返回期望尺寸。纯函数。
    fn measure(&self, constraint: LayoutConstraint) -> Size;

    /// 给定最终 bounds，计算自身及子 Widget 的布局。
    fn layout(&mut self, bounds: Rect);

    /// 处理输入事件。
    /// 返回 `Handled` 则停止冒泡，`Ignored` 则继续向父级传递。
    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult;

    /// 发出绘制命令。纯读操作。
    fn paint(&self, ctx: &PaintContext);

    /// 判断点是否命中此 Widget（用于 HitTest）
    fn hit_test(&self, _point: Point) -> bool {
        // 默认实现：判断点是否在自身 bounds 内
        // 子类型应重写以支持更精确的命中判定
        true // 占位：实际应由 layout 设置的 bounds 决定
    }

    /// 子 Widget 遍历（用于事件冒泡和焦点遍历）
    fn children(&self) -> &[Box<dyn Widget>] {
        &[]
    }

    /// 可变子 Widget 访问
    fn children_mut(&mut self) -> &mut [Box<dyn Widget>] {
        &mut []
    }
}
