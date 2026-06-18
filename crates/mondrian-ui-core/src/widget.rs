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
use mondrian_editor_state::state::PanelKind;
use mondrian_editor_state::Action;
use mondrian_platform::PlatformService;
use mondrian_ui_theme::Theme;
use std::sync::Arc;

/// Pointer capture requested by a widget during event handling.
///
/// Capture keeps pointer move/up events flowing to the requesting widget until
/// it explicitly releases capture or the router clears it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerCaptureRequest {
    Capture(WidgetId),
    Release(WidgetId),
    Clear,
}

/// Internal drag lifecycle requested by a widget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DragRequest {
    Begin(DragPayload),
    Cancel,
}

/// Input method state requested by a focused text widget.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImeRequest {
    pub enabled: bool,
    pub cursor_area: Option<Rect>,
}

/// Cursor icon requested by a widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorRequest {
    Crosshair,
    Default,
}

/// Eyedropper mode requested by a widget (for screen color sampling).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EyedropperRequest {
    pub active: bool,
    /// Position of the eyedropper button or magnifier hotspot in widget-local coords.
    pub hotspot: Option<Point>,
}

/// Side-effect requests emitted by widgets while handling an input event.
///
/// Widgets stay platform independent: they record intent here, and the app or
/// event router decides how to apply it to winit/the OS.
#[derive(Debug, Clone, Default)]
pub struct EventRequests {
    pub pointer_capture: Option<PointerCaptureRequest>,
    pub ime: Option<ImeRequest>,
    pub cursor: Option<CursorRequest>,
    pub eyedropper: Option<EyedropperRequest>,
    pub drag: Option<DragRequest>,
    pub repaint: bool,
}

impl EventRequests {
    pub fn request_pointer_capture(&mut self, widget: WidgetId) {
        self.pointer_capture = Some(PointerCaptureRequest::Capture(widget));
    }

    pub fn release_pointer_capture(&mut self, widget: WidgetId) {
        self.pointer_capture = Some(PointerCaptureRequest::Release(widget));
    }

    pub fn clear_pointer_capture(&mut self) {
        self.pointer_capture = Some(PointerCaptureRequest::Clear);
    }

    pub fn set_ime_enabled(&mut self, enabled: bool, cursor_area: Option<Rect>) {
        self.ime = Some(ImeRequest { enabled, cursor_area });
    }

    pub fn set_cursor(&mut self, cursor: CursorRequest) {
        self.cursor = Some(cursor);
    }

    pub fn set_eyedropper(&mut self, active: bool, hotspot: Option<Point>) {
        self.eyedropper = Some(EyedropperRequest { active, hotspot });
    }

    pub fn begin_drag(&mut self, payload: DragPayload) {
        self.drag = Some(DragRequest::Begin(payload));
    }

    pub fn cancel_drag(&mut self) {
        self.drag = Some(DragRequest::Cancel);
    }

    pub fn request_repaint(&mut self) {
        self.repaint = true;
    }
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
    /// Widget-to-router/app side-effect requests for this event.
    pub requests: &'a mut EventRequests,
}

impl EventContext<'_> {
    pub fn request_pointer_capture(&mut self, widget: WidgetId) {
        self.requests.request_pointer_capture(widget);
    }

    pub fn release_pointer_capture(&mut self, widget: WidgetId) {
        self.requests.release_pointer_capture(widget);
    }

    pub fn clear_pointer_capture(&mut self) {
        self.requests.clear_pointer_capture();
    }

    pub fn set_ime_enabled(&mut self, enabled: bool, cursor_area: Option<Rect>) {
        self.requests.set_ime_enabled(enabled, cursor_area);
    }

    pub fn set_cursor(&mut self, cursor: CursorRequest) {
        self.requests.set_cursor(cursor);
    }

    pub fn set_eyedropper(&mut self, active: bool, hotspot: Option<Point>) {
        self.requests.set_eyedropper(active, hotspot);
    }

    pub fn begin_drag(&mut self, payload: DragPayload) {
        self.requests.begin_drag(payload);
    }

    pub fn cancel_drag(&mut self) {
        self.requests.cancel_drag();
    }

    pub fn request_repaint(&mut self) {
        self.requests.request_repaint();
    }
}

/// 绘制上下文（paint 方法使用）
///
/// Widget::paint() 通过此上下文发出绘制命令。
/// `encoder` 字段的实际类型在 `mondrian-ui-renderer` 中定义。
/// 这里使用 trait object 打破循环依赖。
pub struct PaintContext<'a> {
    /// 绘制命令编码器 —— Widget::paint() 向它写入 DrawCommand
    pub encoder: &'a mut dyn DrawCommandEncoder,
    /// 当前主题
    pub theme: &'a Theme,
    /// 当前裁剪区域
    pub clip_rect: Rect,
}

/// 绘制命令编码器 trait —— 打破 mondrian-ui-core ↔ mondrian-ui-renderer 循环依赖
///
/// `mondrian-ui-renderer::command::DrawEncoder` 实现此 trait。
pub trait DrawCommandEncoder {
    fn push_clip(&mut self, bounds: Rect);
    fn pop_clip(&mut self);
    fn draw_rect(&mut self, bounds: Rect, color: mondrian_core::Color, corner_radius: f32);
    /// Draw a GPU-interpolated rectangle gradient.
    ///
    /// Color order is top-left, top-right, bottom-left, bottom-right.
    fn draw_gradient_rect(
        &mut self,
        bounds: Rect,
        colors: [mondrian_core::Color; 4],
        corner_radius: f32,
    ) {
        self.draw_rect(bounds, colors[0], corner_radius);
    }
    fn draw_line(&mut self, start: Point, end: Point, width: f32, color: mondrian_core::Color);
    fn draw_triangles(&mut self, _vertices: &[Point], _color: mondrian_core::Color) {}
    fn draw_colored_triangles(&mut self, _vertices: &[(Point, mondrian_core::Color)]) {}
    fn draw_colored_triangles_in_rect(
        &mut self,
        vertices: &[(Point, mondrian_core::Color)],
        _mask_bounds: Rect,
        _corner_radius: f32,
    ) {
        self.draw_colored_triangles(vertices);
    }
    /// Draw a cached RGBA raster image from the renderer-owned image atlas.
    ///
    /// The `key` must be stable for a given image payload. Production encoders
    /// upload the bytes once and then reuse the cached atlas slot.
    fn draw_raster_image(
        &mut self,
        _key: &str,
        _bounds: Rect,
        _width: u32,
        _height: u32,
        _rgba: Arc<[u8]>,
        _tint: mondrian_core::Color,
    ) {
    }
    fn draw_text(
        &mut self,
        text: &str,
        font_size: f32,
        position: Point,
        color: mondrian_core::Color,
    );

    /// Draw wrapped text constrained to `max_width`.
    ///
    /// Implementations that do not support paragraph layout may fall back to
    /// single-line text; the production renderer resolves this through
    /// `mondrian-ui-text` and cosmic-text.
    fn draw_text_box(
        &mut self,
        text: &str,
        font_size: f32,
        position: Point,
        max_width: f32,
        color: mondrian_core::Color,
    ) {
        let _ = max_width;
        self.draw_text(text, font_size, position, color);
    }
    fn push_translate(&mut self, offset: glam::Vec2);
    fn pop_transform(&mut self);
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

    /// Return the editor panel kind represented by this widget, if it is a
    /// panel boundary.
    ///
    /// Leaf widgets normally return `None`; containers such as `PanelSlot`
    /// expose this so the event router can derive focused-panel context without
    /// individual controls hardcoding panel identities.
    fn panel_kind(&self) -> Option<PanelKind> {
        None
    }

    /// 给定约束，返回期望尺寸。纯函数。
    fn measure(&self, constraint: LayoutConstraint) -> Size;

    /// 给定最终 bounds，计算自身及子 Widget 的布局。
    fn layout(&mut self, bounds: Rect);

    /// 处理输入事件。
    /// 返回 `Handled` 则停止冒泡，`Ignored` 则继续向父级传递。
    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult;

    /// Called on ancestors before a descendant receives an event.
    ///
    /// This is for parent-owned chrome that must win over child hit targets,
    /// such as dock splitter handles. It must not re-dispatch the event to
    /// children.
    fn before_child_event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        EventResult::Ignored
    }

    /// Called on ancestors after a descendant handled an event.
    ///
    /// This is for parent-owned state that depends on a child interaction, such
    /// as a tab container rebuilding content after its tab bar changes active
    /// index. It must not re-dispatch the event to children.
    fn after_child_event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        EventResult::Ignored
    }

    /// 发出绘制命令。纯读操作。encoder 通过 &mut 访问。
    fn paint(&self, ctx: &mut PaintContext);

    /// 发出顶层覆盖物绘制命令。默认递归绘制子树 overlay。
    ///
    /// Popover、dropdown、context menu、tooltip 等不应被后绘制的普通
    /// 内容压住的 UI chrome 应在这里绘制。需要外部点击关闭的顶层弹层
    /// 在打开时应让 `overlay_hit_test()` 覆盖窗口，再在 `event()` 内
    /// 区分内部/外部命中并关闭或处理事件。
    fn paint_overlay(&self, ctx: &mut PaintContext) {
        for index in 0..self.child_count() {
            if let Some(child) = self.child(index) {
                child.paint_overlay(ctx);
            }
        }
    }

    /// 判断点是否命中此 Widget 拥有的顶层覆盖物。
    ///
    /// 这个命中层和 `paint_overlay()` 对齐，优先于普通内容命中。打开的
    /// dropdown、context menu、popover、取色器弹窗等需要覆盖 sibling
    /// panel 普通内容的组件应重写此方法。默认返回 `false`。
    fn overlay_hit_test(&self, _point: Point) -> bool {
        false
    }

    /// 判断点是否命中此 Widget（用于 HitTest）
    ///
    /// 默认返回 `false`（不拦截任何点击）。
    /// 所有具体 Widget 实现 **必须** 重写此方法以使用自身的 bounds 判定。
    fn hit_test(&self, _point: Point) -> bool {
        false
    }

    /// 子 Widget 遍历（用于事件冒泡和焦点遍历）
    fn children(&self) -> &[Box<dyn Widget>] {
        &[]
    }

    /// 可变子 Widget 访问
    fn children_mut(&mut self) -> &mut [Box<dyn Widget>] {
        &mut []
    }

    /// Number of logical children.
    ///
    /// Containers with children stored as named fields can override this and
    /// `child()` / `child_mut()` without reshaping their storage into a Vec.
    fn child_count(&self) -> usize {
        self.children().len()
    }

    /// Immutable child access by logical z/order index.
    fn child(&self, index: usize) -> Option<&dyn Widget> {
        self.children().get(index).map(|child| child.as_ref())
    }

    /// Mutable child access by logical z/order index.
    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match self.children_mut().get_mut(index) {
            Some(child) => Some(child.as_mut()),
            None => None,
        }
    }

    /// 向下转型支持。默认返回 None。具体 Widget 可重写以支持类型检测。
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }

    /// Mutable downcast support. Defaults to `None`.
    ///
    /// Stateful containers can override this to migrate child state when a
    /// model refresh rebuilds part of the widget tree.
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }

    /// Whether this widget can receive keyboard focus (Tab/Shift+Tab traversal).
    ///
    /// Defaults to `false`. Interactive widgets (Button, Checkbox, Slider,
    /// TextInput, ColorPicker, Dropdown, CurveEditor, List) override to return
    /// `true`.
    fn can_focus(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec2;
    use mondrian_core::Color;

    // ═══════════════════════════════════════════════════════════════════════
    // Test mocks
    // ═══════════════════════════════════════════════════════════════════════

    struct MockEncoder {
        rects: Vec<(Rect, Color, f32)>,
        clips_pushed: usize,
        clips_popped: usize,
    }

    impl MockEncoder {
        fn new() -> Self {
            Self { rects: vec![], clips_pushed: 0, clips_popped: 0 }
        }
    }

    impl DrawCommandEncoder for MockEncoder {
        fn push_clip(&mut self, _bounds: Rect) {
            self.clips_pushed += 1;
        }
        fn pop_clip(&mut self) {
            self.clips_popped += 1;
        }
        fn draw_rect(&mut self, bounds: Rect, color: Color, corner_radius: f32) {
            self.rects.push((bounds, color, corner_radius));
        }
        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {}
        fn draw_text(&mut self, _text: &str, _font_size: f32, _position: Point, _color: Color) {}
        fn push_translate(&mut self, _offset: Vec2) {}
        fn pop_transform(&mut self) {}
    }

    fn mock_paint_ctx<'a>(encoder: &'a mut MockEncoder) -> PaintContext<'a> {
        // Use a leaked dark theme to satisfy lifetime — safe in tests
        let theme: &'static Theme =
            Box::leak(Box::new(mondrian_ui_theme::ThemePreset::Dark.build()));
        PaintContext {
            encoder,
            theme,
            clip_rect: Rect::new(0.0, 0.0, 1920.0, 1080.0),
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Minimal widget for testing widget contract
    // ═══════════════════════════════════════════════════════════════════════

    /// Minimal stateless widget that just draws a rect at its bounds.
    struct TestWidget {
        id: WidgetId,
        color: Color,
        bounds: Rect,
    }

    impl TestWidget {
        fn new(color: Color) -> Self {
            Self { id: WidgetId::new(), color, bounds: Rect::ZERO }
        }
    }

    impl Widget for TestWidget {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, _constraint: LayoutConstraint) -> Size {
            Size::new(100.0, 50.0)
        }

        fn layout(&mut self, bounds: Rect) {
            self.bounds = bounds;
        }

        fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            EventResult::Ignored
        }

        fn paint(&self, ctx: &mut PaintContext) {
            ctx.encoder.draw_rect(self.bounds, self.color, 0.0);
        }

        fn hit_test(&self, point: Point) -> bool {
            self.bounds.contains(point)
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Widget contract tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn widget_measure_is_pure() {
        let w = TestWidget::new(Color::from_hex(0xFF0000));
        // measure should return the same result each call
        let s1 = w.measure(LayoutConstraint::LOOSE);
        let s2 = w.measure(LayoutConstraint::LOOSE);
        assert_eq!(s1, s2);
    }

    #[test]
    fn widget_layout_updates_bounds() {
        let mut w = TestWidget::new(Color::from_hex(0xFF0000));
        let bounds = Rect::new(10.0, 20.0, 100.0, 50.0);
        w.layout(bounds);
        assert_eq!(w.bounds, bounds);
    }

    #[test]
    fn widget_paint_is_pure_read() {
        let mut w = TestWidget::new(Color::from_hex(0xFF0000));
        w.layout(Rect::new(0.0, 0.0, 100.0, 50.0));

        let mut encoder = MockEncoder::new();
        {
            let mut ctx = mock_paint_ctx(&mut encoder);
            w.paint(&mut ctx);
        }
        // paint again — should produce same result
        {
            let mut ctx = mock_paint_ctx(&mut encoder);
            w.paint(&mut ctx);
        }
        assert_eq!(encoder.rects.len(), 2);
        assert_eq!(encoder.rects[0], encoder.rects[1]);
    }

    #[test]
    fn widget_hit_test_after_layout() {
        let mut w = TestWidget::new(Color::from_hex(0xFF0000));
        w.layout(Rect::new(10.0, 10.0, 100.0, 100.0));

        assert!(w.hit_test(Point::new(60.0, 60.0)));
        assert!(!w.hit_test(Point::new(0.0, 0.0)));
    }

    #[test]
    fn widget_default_hit_test_returns_false() {
        // A widget that doesn't override hit_test should reject all clicks.
        struct NoHitTestWidget;
        impl Widget for NoHitTestWidget {
            fn id(&self) -> WidgetId {
                WidgetId::new()
            }
            fn measure(&self, _c: LayoutConstraint) -> Size {
                Size::ZERO
            }
            fn layout(&mut self, _b: Rect) {}
            fn event(&mut self, _e: &UiEvent, _c: &mut EventContext) -> EventResult {
                EventResult::Ignored
            }
            fn paint(&self, _c: &mut PaintContext) {}
        }

        let w = NoHitTestWidget;
        assert!(!w.hit_test(Point::new(0.0, 0.0)));
        assert!(!w.hit_test(Point::new(f32::MAX, f32::MAX)));
    }

    #[test]
    fn widget_default_children_is_empty() {
        let mut w = TestWidget::new(Color::from_hex(0xFF0000));
        assert!(w.children().is_empty());
        assert!(w.children_mut().is_empty());
    }

    #[test]
    fn widget_id_is_accessible() {
        let w = TestWidget::new(Color::from_hex(0xFF0000));
        assert_eq!(w.id(), w.id); // idempotent
    }

    // ═══════════════════════════════════════════════════════════════════════
    // DrawCommandEncoder trait object safety
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn draw_command_encoder_is_object_safe() {
        let encoder: &mut dyn DrawCommandEncoder = &mut MockEncoder::new();
        encoder.draw_rect(Rect::ZERO, Color::from_hex(0xFF0000), 4.0);
        encoder.push_clip(Rect::new(0.0, 0.0, 100.0, 100.0));
        encoder.pop_clip();
    }
}
