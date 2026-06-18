#![allow(deprecated)]
//! UI Element Gallery — 独立 wgpu 窗口，全面测试所有 UI 控件
//!
//! 运行: cargo run --bin ui_demo
//!
//! 包含的 Widget 类型：
//!   Interactive: Button, Checkbox, TextInput, Slider, List, Dropdown, ContextMenu,
//!     Tooltip, ColorPicker, CurveEditor, TimelineView, NodeGraphView
//!   Layout: DockSplitter, DockPanel, DockTabBar, PanelSlot, ScrollView, PropertyPanel

use std::cell::Cell;
use std::cell::RefCell;
use std::sync::Arc;

use mondrian_app::self_hosted::icons::AppIcon;
use mondrian_app::self_hosted::rendering::{
    SelfHostedFrameRenderer, SelfHostedRenderDiagnosticReporter,
};
use mondrian_app::self_hosted::runtime::{
    winit_cursor_icon_for_ui_state, winit_modifiers_to_ui_modifiers,
    winit_mouse_button_to_ui_button, winit_scroll_delta_to_ui_delta, WinitUiRuntime,
};
use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_ui_core::tooltip::TooltipState;
use mondrian_ui_core::types::{self as ui_types, *};
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, TreeWalker, Widget};
use mondrian_ui_events::EventRouter;
use mondrian_ui_renderer::command::DrawEncoder;
use mondrian_ui_tooltip::TooltipManagerImpl;
use mondrian_ui_tooltip::TooltipWidget;
use mondrian_ui_widgets::button::Button;
use mondrian_ui_widgets::checkbox::Checkbox;
use mondrian_ui_widgets::color_picker::{ColorPicker, ColorPickerAreaMode, ColorPickerTrigger};
use mondrian_ui_widgets::context_menu::ContextMenu;
use mondrian_ui_widgets::curve_editor::{CurveEditor, CurvePoint};
use mondrian_ui_widgets::dock_panel::DockPanel;
use mondrian_ui_widgets::dock_splitter::DockSplitter;
use mondrian_ui_widgets::dock_tab_bar::TabInfo;
use mondrian_ui_widgets::label::Label;
use mondrian_ui_widgets::list::{List, ListItem};
use mondrian_ui_widgets::menu::{Dropdown, MenuItem};
use mondrian_ui_widgets::panel_list::{PanelList, PanelListItem};
use mondrian_ui_widgets::panel_slot::SlotKind;
use mondrian_ui_widgets::property_panel::{PropertyPanel, PropertyRow, PropertySection};
use mondrian_ui_widgets::scroll::ScrollView;
use mondrian_ui_widgets::slider::Slider;
use mondrian_ui_widgets::text_input::TextInput;
use mondrian_ui_widgets::{
    AssetGrid, AssetGridItem, NodeGraphEdge, NodeGraphNode, NodeGraphView, RasterImage,
    TimelineClip, TimelineClipRef, TimelineTrack, TimelineView, ViewerSurface,
};

fn demo_action(name: &str) -> Action {
    Action::Custom {
        namespace: "demo".into(),
        name: name.into(),
        payload: serde_json::Value::Null,
    }
}

thread_local! {
    static TEXT_INPUT_BOUNDS: Cell<Option<Rect>> = const { Cell::new(None) };
    static TEXT_INPUT_ID: Cell<Option<WidgetId>> = const { Cell::new(None) };
    static DEMO_LAST_ACTION: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn record_demo_action(action: Action) {
    DEMO_LAST_ACTION.with(|last| {
        *last.borrow_mut() = Some(format!("{action:?}"));
    });
}

fn take_demo_action() -> Option<String> {
    DEMO_LAST_ACTION.with(|last| last.borrow_mut().take())
}

fn with_demo_icon(item: PanelListItem, icon: AppIcon) -> PanelListItem {
    item.with_icon(icon.vector_icon().expect("bundled ui_demo icon asset should parse"))
}

fn with_demo_menu_icon(item: MenuItem, icon: AppIcon) -> MenuItem {
    item.with_icon(icon.vector_icon().expect("bundled ui_demo menu icon asset should parse"))
}

// ═══════════════════════════════════════════════════════════════════════════
// Gallery Widget
// ═══════════════════════════════════════════════════════════════════════════

const GALLERY_CHILD_COUNT: usize = 12;
const GALLERY_FOCUSABLE_CHILDREN: [usize; 10] = [0, 1, 2, 3, 4, 5, 6, 9, 10, 11];

struct GalleryWidget {
    id: WidgetId,
    bounds: Rect,
    button_click: Button,
    button_no_action: Button,
    checkbox_a: Checkbox,
    checkbox_b: Checkbox,
    text_input: TextInput,
    slider: Slider,
    dropdown: Dropdown,
    list: List,
    scroll_area: ScrollView,
    color_picker: ColorPicker,
    color_trigger: ColorPickerTrigger,
    curve_editor: CurveEditor,
    tooltip_trigger: Rect,
    tooltip: TooltipWidget,
    tooltip_active: bool,
    context_menu: Option<ContextMenu>,
    last_action: String,
    last_slider_value: f32,
    last_color_value: Color,
    last_trigger_color_value: Color,
    last_curve_points: Vec<CurvePoint>,
    /// Index of the child that captured the mouse (during drag/selection)
    captured: Option<usize>,
    /// Demo-local keyboard focus for hand-written gallery routing.
    focused: Option<usize>,
}

impl GalleryWidget {
    fn new() -> Self {
        let dropdown_items = vec![
            with_demo_menu_icon(
                MenuItem::new("选项 Alpha", demo_action("alpha")),
                AppIcon::Info,
            ),
            with_demo_menu_icon(
                MenuItem::new("选项 Beta", demo_action("beta")),
                AppIcon::Effect,
            ),
            with_demo_menu_icon(
                MenuItem::new("选项 Gamma", demo_action("gamma")),
                AppIcon::Clock,
            ),
            MenuItem::new("选项 Delta", demo_action("delta")),
            MenuItem::new("选项 Epsilon", demo_action("epsilon")),
            MenuItem::new("选项 Zeta", demo_action("zeta")),
            MenuItem::new("选项 Eta", demo_action("eta")),
            MenuItem::new("选项 Theta", demo_action("theta")),
            with_demo_menu_icon(
                MenuItem::new("选项 Iota (禁用)", demo_action("iota")).disabled(),
                AppIcon::Warning,
            ),
            MenuItem::new("选项 Kappa", demo_action("kappa")),
        ];
        let list_items = vec![
            ListItem::new("列表项 1").with_action(demo_action("item1")),
            ListItem::new("列表项 2").with_action(demo_action("item2")),
            ListItem::new("列表项 3"),
            ListItem::new("列表项 4").with_action(demo_action("item4")),
            ListItem::new("列表项 5"),
            ListItem::new("列表项 6"),
            ListItem::new("列表项 7"),
        ];

        let scroll_content = Label::new(
            "ScrollView 内容区域：这段长文本用于检查裁剪、滚轮滚动、滚动条拖动以及子控件坐标转换。\
            \n\n继续滚动可以看到更多文字，确保内容不会溢出面板，也不会破坏 clip/transform 栈。\
            \n\nA deliberately long Latin sentence lives here as well, so mixed CJK and Latin layout can be checked inside the same scroll surface.",
        )
        .wrapped()
        .with_padding(10.0, 10.0);

        let initial_color = Color::from_hex(0x336699);

        let mut color_trigger = ColorPickerTrigger::new(Color::from_hex(0xD946EF));
        color_trigger.picker_mut().set_area_mode(ColorPickerAreaMode::Wheel);
        color_trigger.picker_mut().set_show_swatch(false);
        let curve_editor = CurveEditor::new();
        let last_curve_points = curve_editor.points().to_vec();

        Self {
            id: WidgetId::new(),
            bounds: Rect::ZERO,
            button_click: Button::new("点击派发 Play").on_click(Action::Play),
            button_no_action: Button::new("无动作按钮"),
            checkbox_a: Checkbox::new("启用特性 A", false).on_toggle(demo_action("toggle_a")),
            checkbox_b: Checkbox::new("启用特性 B", true),
            text_input: TextInput::new("输入文本..."),
            slider: Slider::new(50.0, 0.0, 100.0),
            dropdown: Dropdown::new("选择选项", dropdown_items).with_max_visible_items(5),
            list: List::new(list_items),
            scroll_area: ScrollView::new(Some(Box::new(scroll_content))),
            color_picker: ColorPicker::new(initial_color),
            color_trigger,
            curve_editor,
            tooltip_trigger: Rect::ZERO,
            tooltip: TooltipWidget::new(),
            tooltip_active: false,
            context_menu: None,
            last_action: String::new(),
            last_slider_value: 50.0,
            last_color_value: initial_color,
            last_trigger_color_value: Color::from_hex(0xD946EF),
            last_curve_points,
            captured: None,
            focused: None,
        }
    }

    fn is_focusable_child(index: usize) -> bool {
        GALLERY_FOCUSABLE_CHILDREN.contains(&index)
    }

    fn child_dispatches_action(index: usize) -> bool {
        matches!(index, 0 | 2 | 3 | 6 | 7)
    }

    fn event_targets_focus(event: &UiEvent) -> bool {
        matches!(
            event,
            UiEvent::KeyDown { .. }
                | UiEvent::KeyUp { .. }
                | UiEvent::TextInput(_)
                | UiEvent::ImePreedit(_)
                | UiEvent::ImeCommit(_)
        )
    }

    fn child_event(
        &mut self,
        index: usize,
        event: &UiEvent,
        ctx: &mut EventContext,
    ) -> EventResult {
        match index {
            0 => self.button_click.event(event, ctx),
            1 => self.button_no_action.event(event, ctx),
            2 => self.checkbox_a.event(event, ctx),
            3 => self.checkbox_b.event(event, ctx),
            4 => self.text_input.event(event, ctx),
            5 => self.slider.event(event, ctx),
            6 => self.dropdown.event(event, ctx),
            7 => self.list.event(event, ctx),
            8 => self.scroll_area.event(event, ctx),
            9 => self.color_picker.event(event, ctx),
            10 => self.color_trigger.event(event, ctx),
            11 => self.curve_editor.event(event, ctx),
            _ => EventResult::Ignored,
        }
    }

    fn child_hit_test(&self, index: usize, point: Point) -> bool {
        match index {
            0 => self.button_click.hit_test(point),
            1 => self.button_no_action.hit_test(point),
            2 => self.checkbox_a.hit_test(point),
            3 => self.checkbox_b.hit_test(point),
            4 => self.text_input.hit_test(point),
            5 => self.slider.hit_test(point),
            6 => self.dropdown.hit_test(point),
            7 => self.list.hit_test(point),
            8 => self.scroll_area.hit_test(point),
            9 => self.color_picker.hit_test(point),
            10 => self.color_trigger.hit_test(point),
            11 => self.curve_editor.hit_test(point),
            _ => false,
        }
    }

    fn child_overlay_hit_test(&self, index: usize, point: Point) -> bool {
        match index {
            0 => self.button_click.overlay_hit_test(point),
            1 => self.button_no_action.overlay_hit_test(point),
            2 => self.checkbox_a.overlay_hit_test(point),
            3 => self.checkbox_b.overlay_hit_test(point),
            4 => self.text_input.overlay_hit_test(point),
            5 => self.slider.overlay_hit_test(point),
            6 => self.dropdown.overlay_hit_test(point),
            7 => self.list.overlay_hit_test(point),
            8 => self.scroll_area.overlay_hit_test(point),
            9 => self.color_picker.overlay_hit_test(point),
            10 => self.color_trigger.overlay_hit_test(point),
            11 => self.curve_editor.overlay_hit_test(point),
            _ => false,
        }
    }

    fn event_position(event: &UiEvent) -> Option<Point> {
        match event {
            UiEvent::MouseDown { position, .. }
            | UiEvent::MouseUp { position, .. }
            | UiEvent::MouseMove { position, .. }
            | UiEvent::MouseWheel { position, .. }
            | UiEvent::DragEnter { position, .. }
            | UiEvent::DragOver { position, .. }
            | UiEvent::Drop { position, .. } => Some(*position),
            _ => None,
        }
    }

    fn event_target_children(&self, event: &UiEvent) -> Vec<usize> {
        let Some(position) = Self::event_position(event) else {
            return Vec::new();
        };

        let overlay_targets = (0..GALLERY_CHILD_COUNT)
            .rev()
            .filter(|index| self.child_overlay_hit_test(*index, position))
            .collect::<Vec<_>>();
        if !overlay_targets.is_empty() {
            return overlay_targets;
        }

        (0..GALLERY_CHILD_COUNT)
            .rev()
            .filter(|index| self.child_hit_test(*index, position))
            .collect()
    }

    fn set_focused_child(&mut self, next: Option<usize>, ctx: &mut EventContext) {
        let next = next.filter(|index| Self::is_focusable_child(*index));
        if self.focused == next {
            return;
        }

        if let Some(previous) = self.focused.take() {
            let _ = self.child_event(previous, &UiEvent::FocusLost, ctx);
        }

        self.focused = next;
        if let Some(current) = self.focused {
            let _ = self.child_event(current, &UiEvent::FocusGained, ctx);
        }
    }

    fn focus_next_child(&mut self, reverse: bool, ctx: &mut EventContext) {
        let focusables = &GALLERY_FOCUSABLE_CHILDREN;
        let current_position = self
            .focused
            .and_then(|current| focusables.iter().position(|index| *index == current));

        let next_position = match (current_position, reverse) {
            (Some(0), true) | (None, true) => focusables.len() - 1,
            (Some(position), true) => position - 1,
            (Some(position), false) => (position + 1) % focusables.len(),
            (None, false) => 0,
        };
        self.set_focused_child(Some(focusables[next_position]), ctx);
    }

    fn apply_child_feedback(&mut self, index: usize, local_action: &RefCell<String>) {
        if Self::child_dispatches_action(index) {
            let action = local_action.borrow();
            if !action.is_empty() {
                self.last_action = action.clone();
            }
        }
        if index == 5 {
            self.last_slider_value = self.slider.value();
            self.last_action = format!("slider={:.1}", self.last_slider_value);
        }
        self.sync_child_feedback();
    }

    fn update_hover_tooltip(&mut self, position: Point) {
        if self.tooltip_trigger.contains(position) {
            if !self.tooltip_active {
                self.tooltip_active = true;
                self.tooltip.update_state(TooltipState {
                    text: "Tooltip 示例：用于检查悬停提示、边界夹紧和文字绘制。".into(),
                    position: Point::new(
                        self.tooltip_trigger.x,
                        self.tooltip_trigger.y + self.tooltip_trigger.height,
                    ),
                    visible: true,
                });
            }
        } else {
            self.tooltip_active = false;
            self.tooltip.clear();
        }
    }

    fn sync_child_feedback(&mut self) {
        if let Some(action) = take_demo_action() {
            self.last_action = action;
        }
        let slider_value = self.slider.value();
        if (slider_value - self.last_slider_value).abs() > 0.05 {
            self.last_slider_value = slider_value;
            self.last_action = format!("slider={slider_value:.1}");
        }
        let color_value = self.color_picker.color();
        if color_value != self.last_color_value {
            self.last_color_value = color_value;
            self.last_action = format!("color={}", color_value.to_hex_rgba());
        }
        let trigger_color = self.color_trigger.color();
        if trigger_color != self.last_trigger_color_value {
            self.last_trigger_color_value = trigger_color;
            self.last_action = format!("trigger_color={}", trigger_color.to_hex_rgba());
        }
        if self.curve_editor.points() != self.last_curve_points.as_slice() {
            self.last_curve_points = self.curve_editor.points().to_vec();
            self.last_action = "curve edited".into();
        }
    }
}

impl Widget for GalleryWidget {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, _c: LayoutConstraint) -> Size {
        Size::new(400.0, 600.0)
    }

    fn layout(&mut self, b: Rect) {
        self.bounds = b;
        let x0 = b.x + 12.0;
        let col_w = b.width - 24.0;
        let row_h = 30.0;
        let gap = 6.0;
        let mut y = b.y + 48.0;

        let half_w = (col_w - 8.0).max(1.0) * 0.5;
        self.button_click.layout(Rect::new(x0, y, half_w, row_h));
        self.button_no_action.layout(Rect::new(x0 + half_w + 8.0, y, half_w, row_h));
        y += row_h + gap;

        self.checkbox_a.layout(Rect::new(x0, y, col_w * 0.5, row_h));
        self.checkbox_b.layout(Rect::new(x0 + col_w * 0.5, y, col_w * 0.5, row_h));
        y += row_h + gap;

        self.text_input.layout(Rect::new(x0, y, col_w, row_h));
        TEXT_INPUT_BOUNDS.with(|b| b.set(Some(Rect::new(x0, y, col_w, row_h))));
        TEXT_INPUT_ID.with(|id| id.set(Some(self.text_input.id())));
        y += row_h + gap;

        self.slider.layout(Rect::new(x0, y, col_w, row_h));
        y += row_h + gap;

        let dropdown_w = col_w.clamp(1.0, 160.0);
        self.dropdown.layout(Rect::new(x0, y, dropdown_w, row_h));
        if col_w >= 260.0 {
            self.tooltip_trigger = Rect::new(x0 + 172.0, y, (col_w - 172.0).max(0.0), row_h);
            y += row_h + 12.0;
        } else {
            y += row_h + gap;
            self.tooltip_trigger = Rect::new(x0, y, col_w.max(1.0), row_h);
            y += row_h + 12.0;
        }

        let list_h = 5.0 * 28.0;
        self.list.layout(Rect::new(x0, y, col_w * 0.55, list_h));
        self.scroll_area.layout(Rect::new(
            x0 + col_w * 0.55 + 8.0,
            y,
            col_w * 0.45 - 8.0,
            list_h,
        ));
        y += list_h + 12.0;

        self.color_picker.layout(Rect::new(x0, y, col_w, 292.0));
        y += 292.0 + 12.0;

        self.color_trigger.layout(Rect::new(x0, y, 32.0, 32.0));
        self.curve_editor.layout(Rect::new(x0 + 44.0, y, (col_w - 44.0).max(1.0), 96.0));

        if let Some(ref mut cm) = &mut self.context_menu {
            let cm_size = cm.measure(LayoutConstraint::LOOSE);
            cm.layout(Rect::new(0.0, 0.0, cm_size.width, cm_size.height));
        }
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if let UiEvent::MouseMove { position, .. } = event {
            self.update_hover_tooltip(*position);
        }

        // Context menu always gets first dibs
        if let Some(ref mut cm) = &mut self.context_menu {
            if cm.event(event, ctx) == EventResult::Handled {
                if !cm.is_visible() {
                    self.context_menu = None;
                }
                return EventResult::Handled;
            }
        }

        let last_action: std::cell::RefCell<String> = std::cell::RefCell::new(String::new());
        let inner_ctx = &mut EventContext {
            focus: ctx.focus,
            shortcut: ctx.shortcut,
            tooltip: ctx.tooltip,
            dispatch: &|a: Action| {
                *last_action.borrow_mut() = format!("{a:?}");
            },
            platform: ctx.platform,
            requests: ctx.requests,
        };

        if let UiEvent::KeyDown { key: KeyCode::Tab, modifiers } = event {
            self.focus_next_child(modifiers.shift, inner_ctx);
            return EventResult::Handled;
        }

        if Self::event_targets_focus(event) {
            if let Some(index) = self.focused {
                let result = self.child_event(index, event, inner_ctx);
                if result == EventResult::Handled {
                    self.apply_child_feedback(index, &last_action);
                    return EventResult::Handled;
                }
            }
            return EventResult::Ignored;
        }

        // If a child captured the mouse (drag/selection in progress), route
        // MouseMove and MouseUp to it first. MouseUp clears the capture.
        // On MouseDown: if captured child ignores it, fall through to normal
        // dispatch so other widgets can receive the click.
        if let Some(idx) = self.captured {
            let handled = self.child_event(idx, event, inner_ctx);
            if matches!(event, UiEvent::MouseUp { .. }) {
                self.captured = None;
            }
            // MouseDown during capture: if captured child doesn't handle it,
            // let the click go to the actual target.
            if matches!(event, UiEvent::MouseDown { .. }) && handled == EventResult::Ignored {
                self.captured = None;
                // fall through to normal dispatch below
            } else {
                if handled == EventResult::Handled {
                    self.apply_child_feedback(idx, &last_action);
                    return EventResult::Handled;
                }
                return EventResult::Ignored;
            }
        }

        // Normal pointer dispatch mirrors the framework router: open overlays
        // get first priority, then the topmost normal hit-test target. This
        // keeps scroll/dropdown/color-picker state isolated inside ui_demo.
        let mut handled = EventResult::Ignored;
        for idx in self.event_target_children(event) {
            if self.child_event(idx, event, inner_ctx) == EventResult::Handled {
                if matches!(event, UiEvent::MouseDown { .. }) {
                    self.captured = Some(idx);
                    self.set_focused_child(Some(idx), inner_ctx);
                }
                self.apply_child_feedback(idx, &last_action);
                handled = EventResult::Handled;
                break;
            }
        }
        if handled == EventResult::Handled {
            return EventResult::Handled;
        }

        if let UiEvent::MouseDown { position, button: MouseButton::Left, .. } = event {
            if self.bounds.contains(*position) {
                self.set_focused_child(None, inner_ctx);
            }
        }

        // Right-click opens context menu
        if let UiEvent::MouseDown { position, button: MouseButton::Right, .. } = event {
            if self.bounds.contains(*position) {
                let items = vec![
                    with_demo_menu_icon(
                        MenuItem::new("剪切", Action::Cut).with_shortcut("Ctrl+X"),
                        AppIcon::Cut,
                    ),
                    with_demo_menu_icon(
                        MenuItem::new("复制", Action::Copy).with_shortcut("Ctrl+C"),
                        AppIcon::Copy,
                    ),
                    with_demo_menu_icon(
                        MenuItem::new("粘贴", Action::Paste).with_shortcut("Ctrl+V"),
                        AppIcon::ClipboardText,
                    ),
                    MenuItem::separator(),
                    with_demo_menu_icon(
                        MenuItem::new("删除", demo_action("delete")),
                        AppIcon::Trash,
                    ),
                ];
                self.context_menu = Some(ContextMenu::new(*position, items));
                return EventResult::Handled;
            }
        }

        EventResult::Ignored
    }

    fn after_child_event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        self.sync_child_feedback();
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;

        ctx.encoder.draw_rect(self.bounds, tokens.card, ctx.theme.spacing.radius_md);

        self.button_click.paint(ctx);
        self.button_no_action.paint(ctx);
        self.checkbox_a.paint(ctx);
        self.checkbox_b.paint(ctx);
        self.text_input.paint(ctx);
        self.slider.paint(ctx);
        self.dropdown.paint(ctx);
        ctx.encoder.draw_rect(
            self.tooltip_trigger,
            tokens.secondary,
            ctx.theme.spacing.radius_sm,
        );
        ctx.encoder.draw_text(
            "悬停 Tooltip",
            12.0,
            Point::new(self.tooltip_trigger.x + 8.0, self.tooltip_trigger.y + 7.0),
            tokens.secondary_foreground,
        );
        self.list.paint(ctx);
        self.scroll_area.paint(ctx);
        self.color_picker.paint(ctx);
        self.color_trigger.paint(ctx);
        self.curve_editor.paint(ctx);

        let p = ui_types::snap_point(Point::new(self.bounds.x + 12.0, self.bounds.y + 6.0));
        ctx.encoder
            .draw_text("UI 控件画廊 — 右键可打开菜单", 15.0, p, tokens.foreground);

        if !self.last_action.is_empty() {
            let fb = format!("最后操作: {}", self.last_action);
            let p = ui_types::snap_point(Point::new(self.bounds.x + 12.0, self.bounds.y + 24.0));
            ctx.encoder.draw_text(&fb, 12.0, p, tokens.primary);
        }
    }

    fn paint_overlay(&self, ctx: &mut PaintContext) {
        self.dropdown.paint_overlay(ctx);
        self.color_picker.paint_overlay(ctx);
        self.color_trigger.paint_overlay(ctx);
        if let Some(ref cm) = &self.context_menu {
            cm.paint_overlay(ctx);
        }
        self.tooltip.paint_overlay(ctx);
    }

    fn hit_test(&self, p: Point) -> bool {
        self.bounds.contains(p)
    }

    fn child_count(&self) -> usize {
        12 + usize::from(self.context_menu.is_some())
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.button_click),
            1 => Some(&self.button_no_action),
            2 => Some(&self.checkbox_a),
            3 => Some(&self.checkbox_b),
            4 => Some(&self.text_input),
            5 => Some(&self.slider),
            6 => Some(&self.dropdown),
            7 => Some(&self.list),
            8 => Some(&self.scroll_area),
            9 => Some(&self.color_picker),
            10 => Some(&self.color_trigger),
            11 => Some(&self.curve_editor),
            12 => self.context_menu.as_ref().map(|menu| menu as &dyn Widget),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.button_click),
            1 => Some(&mut self.button_no_action),
            2 => Some(&mut self.checkbox_a),
            3 => Some(&mut self.checkbox_b),
            4 => Some(&mut self.text_input),
            5 => Some(&mut self.slider),
            6 => Some(&mut self.dropdown),
            7 => Some(&mut self.list),
            8 => Some(&mut self.scroll_area),
            9 => Some(&mut self.color_picker),
            10 => Some(&mut self.color_trigger),
            11 => Some(&mut self.curve_editor),
            12 => self.context_menu.as_mut().map(|menu| menu as &mut dyn Widget),
            _ => None,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Text diagnostic widget
// ═══════════════════════════════════════════════════════════════════════════

struct TextDiagnosticWidget {
    id: WidgetId,
    bounds: Rect,
}

impl TextDiagnosticWidget {
    fn new() -> Self {
        Self { id: WidgetId::new(), bounds: Rect::ZERO }
    }
}

impl Widget for TextDiagnosticWidget {
    fn id(&self) -> WidgetId {
        self.id
    }
    fn measure(&self, _c: LayoutConstraint) -> Size {
        Size::new(800.0, 400.0)
    }
    fn layout(&mut self, b: Rect) {
        self.bounds = b;
    }
    fn event(&mut self, _e: &UiEvent, _c: &mut EventContext) -> EventResult {
        EventResult::Ignored
    }
    fn paint(&self, ctx: &mut PaintContext) {
        let bg = Color::from_hex(0x1A1A2E);
        ctx.encoder.draw_rect(self.bounds, bg, 0.0);
        let c = Color::from_hex(0xEBEBF0);

        let text = "Mondrian 自研 UI 框架 — 文本渲染测试";
        let sizes = [11.0, 12.0, 13.0, 14.0, 16.0, 20.0, 24.0];
        let base_x = (self.bounds.x + 10.0).round();
        let mut y = (self.bounds.y + 10.0).round();
        for &fs in &sizes {
            ctx.encoder.draw_text(text, fs, Point::new(base_x, y), c);
            y += (fs * 1.5 + 4.0).round();
        }

        y += 20.0;
        ctx.encoder.draw_text(
            "子像素定位测试 (l 字符):",
            13.0,
            Point::new(base_x, y),
            Color::from_hex(0x888899),
        );
        y += 20.0;
        // Subpixel test: intentionally fractional x positions
        for &fs in &[13.0, 14.0, 16.0] {
            for i in 0..5 {
                let px = self.bounds.x + 10.0 + i as f32 * 0.33;
                ctx.encoder.draw_text("l", fs, Point::new(px, y), c);
            }
            y += (fs * 1.5 + 8.0).round();
        }

        y += 10.0;
        ctx.encoder.draw_text(
            "中文字符测试：你好世界！これは日本語です。",
            14.0,
            Point::new(base_x, y),
            Color::from_hex(0xAABBCC),
        );
    }
    fn hit_test(&self, _p: Point) -> bool {
        false
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tab and panel content factories
// ═══════════════════════════════════════════════════════════════════════════

fn dock_panel(kind: SlotKind) -> Box<dyn Widget> {
    Box::new(DockPanel::new(kind, tab_infos(kind), slot_content_for_tab))
}

fn tab_infos(kind: SlotKind) -> Vec<TabInfo> {
    match kind {
        SlotKind::Viewer => vec![
            TabInfo { label: "查看器".into(), active: true },
            TabInfo { label: "节点图".into(), active: false },
        ],
        SlotKind::Timeline => vec![
            TabInfo { label: "时间线".into(), active: true },
            TabInfo { label: "音频".into(), active: false },
            TabInfo { label: "效果".into(), active: false },
        ],

        SlotKind::Inspector => vec![
            TabInfo { label: "检查器".into(), active: true },
            TabInfo { label: "属性".into(), active: false },
        ],
        SlotKind::Effects => vec![
            TabInfo { label: "控件".into(), active: true },
            TabInfo { label: "文本".into(), active: false },
            TabInfo { label: "形状".into(), active: false },
        ],
        SlotKind::Assets => vec![
            TabInfo { label: "资源".into(), active: true },
            TabInfo { label: "库".into(), active: false },
        ],
        _ => vec![TabInfo { label: kind.display_name().into(), active: true }],
    }
}

fn slot_content_for_tab(kind: SlotKind, tab_index: usize) -> Box<dyn Widget> {
    match kind {
        SlotKind::Viewer => match tab_index {
            0 => Box::new(demo_viewer_surface()),
            _ => Box::new(demo_node_graph_panel()),
        },
        SlotKind::Effects => match tab_index {
            0 => Box::new(GalleryWidget::new()),
            1 => Box::new(TextDiagnosticWidget::new()),
            _ => Box::new(ShapePanelWidget::new()),
        },
        SlotKind::Assets => match tab_index {
            0 => Box::new(demo_asset_panel()),
            _ => Box::new(demo_library_panel()),
        },
        SlotKind::Inspector => match tab_index {
            0 => inspector_demo_panel(),
            _ => Box::new(demo_property_browser_panel()),
        },
        SlotKind::Timeline => match tab_index {
            0 => Box::new(demo_timeline_panel()),
            1 => Box::new(demo_audio_panel()),
            _ => Box::new(demo_effect_panel()),
        },
        _ => Box::new(demo_unsupported_panel(kind)),
    }
}

fn demo_unsupported_panel(kind: SlotKind) -> PanelList {
    PanelList::new(
        kind.display_name(),
        vec![with_demo_icon(
            PanelListItem::new("Demo panel not configured")
                .with_subtitle(format!(
                    "{} has no dedicated ui_demo tab content yet",
                    kind.display_name()
                ))
                .with_badge("Pending")
                .disabled(true),
            AppIcon::Info,
        )],
    )
    .with_subtitle("UI demo coverage")
}

fn demo_viewer_surface() -> ViewerSurface {
    ViewerSurface::new("Demo edit", 3840, 2160)
        .with_status("Ready")
        .with_resolution_label("3840x2160 @ 29.97 fps")
        .with_frame_label("F68")
        .with_duration_label("224 frames")
}

fn demo_node_graph_panel() -> NodeGraphView {
    NodeGraphView::new(
        vec![
            NodeGraphNode::new("source", "Source")
                .with_subtitle("B-roll")
                .with_accent(Color::from_hex(0x4B7BE5)),
            NodeGraphNode::new("blur", "Gaussian Blur")
                .with_subtitle("GPU 1")
                .with_accent(Color::from_hex(0x3B82F6)),
            NodeGraphNode::new("lut", "LUT 3D")
                .with_subtitle("3D 2")
                .with_accent(Color::from_hex(0x22C55E)),
            NodeGraphNode::new("key", "Chroma Key")
                .with_subtitle("KEY 3")
                .with_accent(Color::from_hex(0xF59E0B))
                .disabled(true),
            NodeGraphNode::new("output", "Output")
                .with_subtitle("Composite")
                .with_accent(Color::from_hex(0x22C55E)),
        ],
        vec![
            NodeGraphEdge::new("source", "blur"),
            NodeGraphEdge::new("blur", "lut"),
            NodeGraphEdge::new("lut", "key"),
            NodeGraphEdge::new("key", "output"),
        ],
    )
    .with_title("Node Graph")
    .with_subtitle("Viewer tab demo / 3 effect(s)")
    .with_selected_node("blur")
    .on_select(|id| Action::Custom {
        namespace: "demo.node_graph".into(),
        name: format!("select:{id}"),
        payload: serde_json::Value::Null,
    })
}

fn demo_asset_panel() -> AssetGrid {
    AssetGrid::new(
        "Assets",
        vec![
            demo_asset_card(
                "demo-camera-main",
                "A001_Camera_Main.mov",
                "00:01:24:12 - Rec.709 - 4K",
                "Video",
                Color::from_hex(0x89B4FA),
                AppIcon::Film,
            )
            .with_thumbnail(demo_asset_thumbnail(
                "demo-thumb:camera-main",
                Color::from_hex(0x89B4FA),
                Color::from_hex(0xF6C177),
            )),
            demo_asset_card(
                "demo-vo-take",
                "VO_Take_03.wav",
                "48 kHz stereo - normalized",
                "Audio",
                Color::from_hex(0xA6E3A1),
                AppIcon::Music,
            ),
            demo_asset_card(
                "demo-brand-pack",
                "Brand_Pack",
                "Logos, colors, and lower thirds",
                "Folder",
                Color::from_hex(0xF9E2AF),
                AppIcon::Folder,
            ),
            demo_asset_card(
                "demo-missing-reference",
                "Missing_Reference.psd",
                "Offline media placeholder",
                "Offline",
                Color::from_hex(0xF38BA8),
                AppIcon::Warning,
            )
            .disabled(true),
        ],
    )
    .with_subtitle("Project media")
    .with_filter("Search assets")
    .on_activate(|index, item| demo_action(&format!("assets.activate.{index}.{}", item.title)))
}

fn demo_asset_card(
    id: &str,
    title: &str,
    subtitle: &str,
    badge: &str,
    accent: Color,
    icon: AppIcon,
) -> AssetGridItem {
    AssetGridItem::new(id, title, accent)
        .with_subtitle(subtitle)
        .with_badge(badge)
        .with_icon(icon.vector_icon().expect("bundled ui_demo icon asset should parse"))
        .with_select_action(demo_action(&format!("assets.select.{id}")))
}

fn demo_asset_thumbnail(key: &str, primary: Color, secondary: Color) -> RasterImage {
    const WIDTH: u32 = 96;
    const HEIGHT: u32 = 54;
    let mut rgba = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let u = x as f32 / (WIDTH - 1) as f32;
            let v = y as f32 / (HEIGHT - 1) as f32;
            let stripe = if (x / 12 + y / 9) % 2 == 0 { 0.10 } else { 0.0 };
            let vignette = ((u - 0.5).abs() + (v - 0.5).abs()).min(1.0) * 0.22;
            let color = primary.lerp(secondary, (u * 0.72 + v * 0.28).clamp(0.0, 1.0));
            rgba.push(((color.r + stripe - vignette).clamp(0.0, 1.0) * 255.0) as u8);
            rgba.push(((color.g + stripe - vignette).clamp(0.0, 1.0) * 255.0) as u8);
            rgba.push(((color.b + stripe - vignette).clamp(0.0, 1.0) * 255.0) as u8);
            rgba.push(255);
        }
    }
    RasterImage::new(key, WIDTH, HEIGHT, rgba).expect("demo thumbnail dimensions are fixed")
}

fn demo_library_panel() -> PanelList {
    PanelList::new(
        "Library",
        vec![
            with_demo_icon(
                PanelListItem::new("Stock transitions")
                    .with_subtitle("Cross dissolve, dip to color, push")
                    .with_badge("12")
                    .with_select_action(demo_action("library.select.transitions")),
                AppIcon::Effect,
            ),
            with_demo_icon(
                PanelListItem::new("Motion presets")
                    .with_subtitle("Position and scale keyframe templates")
                    .with_badge("9")
                    .with_select_action(demo_action("library.select.motion")),
                AppIcon::Anchor,
            ),
            with_demo_icon(
                PanelListItem::new("Team shared bins")
                    .with_subtitle("Network-backed media collection")
                    .with_badge("Beta")
                    .disabled(true),
                AppIcon::Folder,
            ),
        ],
    )
    .with_subtitle("Reusable resources")
    .with_filter("Search library")
}

fn demo_audio_panel() -> PanelList {
    PanelList::new(
        "Audio",
        vec![
            with_demo_icon(
                PanelListItem::new("Dialogue")
                    .with_subtitle("Voice cleanup, EQ, dynamics")
                    .with_badge("A1")
                    .with_select_action(demo_action("audio.select.dialogue")),
                AppIcon::Speaker,
            ),
            with_demo_icon(
                PanelListItem::new("Music")
                    .with_subtitle("Ducking and stem balance")
                    .with_badge("A2")
                    .with_select_action(demo_action("audio.select.music")),
                AppIcon::Music,
            ),
            with_demo_icon(
                PanelListItem::new("Ambience")
                    .with_subtitle("Room tone and location beds")
                    .with_badge("A3")
                    .with_select_action(demo_action("audio.select.ambience")),
                AppIcon::SpeakerMuted,
            ),
        ],
    )
    .with_subtitle("Track lanes")
    .with_filter("Search audio")
}

fn demo_timeline_panel() -> TimelineView {
    TimelineView::new(vec![
        TimelineTrack::video(
            "V3",
            vec![
                TimelineClip::new("Color Grade", 28, 80)
                    .with_color(Color::from_hex(0x6D5DD3))
                    .with_select_action(demo_action("timeline.select.grade")),
                TimelineClip::new("Lower Third", 122, 42)
                    .with_color(Color::from_hex(0x4B7BE5))
                    .with_select_action(demo_action("timeline.select.lower_third")),
            ],
        ),
        TimelineTrack::video(
            "V2",
            vec![
                TimelineClip::new("B-roll: Hands", 10, 64)
                    .with_color(Color::from_hex(0x2C7A7B))
                    .with_select_action(demo_action("timeline.select.hands")),
                TimelineClip::new("Screen Insert", 92, 70)
                    .with_color(Color::from_hex(0x805AD5))
                    .selected(true)
                    .with_select_action(demo_action("timeline.select.screen")),
                TimelineClip::new("Offline Ref", 178, 40)
                    .with_color(Color::from_hex(0x744210))
                    .disabled(true),
            ],
        ),
        TimelineTrack::video(
            "V1",
            vec![
                TimelineClip::new("A Cam", 0, 96)
                    .with_color(Color::from_hex(0x1E3A5F))
                    .with_select_action(demo_action("timeline.select.acam")),
                TimelineClip::new("Reaction", 104, 58)
                    .with_color(Color::from_hex(0x2F855A))
                    .with_select_action(demo_action("timeline.select.reaction")),
                TimelineClip::new("Outro", 170, 54)
                    .with_color(Color::from_hex(0x975A16))
                    .with_select_action(demo_action("timeline.select.outro")),
            ],
        ),
        TimelineTrack::audio(
            "A1",
            vec![TimelineClip::new("Dialogue Mix", 0, 168)
                .with_color(Color::from_hex(0x1D587B))
                .with_select_action(demo_action("timeline.select.dialogue"))],
        ),
        TimelineTrack::audio(
            "A2",
            vec![TimelineClip::new("Music Bed", 20, 204)
                .with_color(Color::from_hex(0x2B6CB0))
                .with_select_action(demo_action("timeline.select.music"))],
        ),
        TimelineTrack::audio(
            "A3",
            vec![TimelineClip::new("Room Tone", 0, 224)
                .with_color(Color::from_hex(0x2F6F73))
                .with_select_action(demo_action("timeline.select.room_tone"))],
        ),
    ])
    .with_playhead(68)
    .on_clip_select(|clip_ref, clip| demo_timeline_clip_action(clip_ref, &clip.label))
    .on_clip_move(|movement, clip| {
        demo_action(&format!(
            "timeline.move.{}.{}.track{}->track{}.{}->{}.{}",
            movement.clip_ref.track_index,
            movement.clip_ref.clip_index,
            movement.clip_ref.track_index,
            movement.new_track_index,
            movement.old_start_frame,
            movement.new_start_frame,
            clip.label
        ))
    })
    .on_clip_trim(|trim, clip| {
        demo_action(&format!(
            "timeline.trim.{}.{}.{:?}.{}+{}->{}+{}.{}",
            trim.clip_ref.track_index,
            trim.clip_ref.clip_index,
            trim.edge,
            trim.old_start_frame,
            trim.old_duration_frames,
            trim.new_start_frame,
            trim.new_duration_frames,
            clip.label
        ))
    })
    .on_seek(|frame| demo_action(&format!("timeline.seek.{frame}")))
}

fn demo_timeline_clip_action(clip_ref: TimelineClipRef, label: &str) -> Action {
    demo_action(&format!(
        "timeline.select.{}.{}.{}",
        clip_ref.track_index, clip_ref.clip_index, label
    ))
}

fn demo_effect_panel() -> PanelList {
    PanelList::new(
        "Effects",
        vec![
            with_demo_icon(
                PanelListItem::new("Color Balance")
                    .with_subtitle("Lift, gamma, gain")
                    .with_badge("GPU")
                    .with_select_action(demo_action("effects.select.color_balance")),
                AppIcon::Effect,
            ),
            with_demo_icon(
                PanelListItem::new("Gaussian Blur")
                    .with_subtitle("Separable blur preview")
                    .with_badge("GPU")
                    .with_select_action(demo_action("effects.select.blur")),
                AppIcon::Effect,
            ),
            with_demo_icon(
                PanelListItem::new("Transform")
                    .with_subtitle("Position, scale, rotation")
                    .with_badge("Core")
                    .with_select_action(demo_action("effects.select.transform")),
                AppIcon::Anchor,
            ),
            with_demo_icon(
                PanelListItem::new("Optical Flow")
                    .with_subtitle("Disabled row smoke test")
                    .with_badge("Soon")
                    .disabled(true),
                AppIcon::Warning,
            ),
        ],
    )
    .with_subtitle("Apply to selected clip")
    .with_filter("Search effects")
    .on_activate(|index, item| demo_action(&format!("effects.apply.{index}.{}", item.title)))
}

fn demo_property_browser_panel() -> PanelList {
    PanelList::new(
        "Properties",
        vec![
            with_demo_icon(
                PanelListItem::new("Clip metadata")
                    .with_subtitle("Name, labels, source path")
                    .with_select_action(demo_action("properties.select.metadata")),
                AppIcon::Info,
            ),
            with_demo_icon(
                PanelListItem::new("Playback")
                    .with_subtitle("Speed, reverse, frame sampling")
                    .with_select_action(demo_action("properties.select.playback")),
                AppIcon::PlayFilled,
            ),
            with_demo_icon(
                PanelListItem::new("Render cache")
                    .with_subtitle("Cache policy and invalidation")
                    .with_select_action(demo_action("properties.select.cache")),
                AppIcon::Clock,
            ),
        ],
    )
    .with_subtitle("Inspector categories")
    .with_filter("Search properties")
}

fn inspector_demo_panel() -> Box<dyn Widget> {
    let mut tint = ColorPickerTrigger::new(Color::from_rgba8(190, 156, 255, 220));
    tint.picker_mut().set_area_mode(ColorPickerAreaMode::Wheel);

    let panel = PropertyPanel::new("Inspector")
        .with_subtitle("Selected clip")
        .with_section(
            PropertySection::new("Clip")
                .with_row(PropertyRow::new(
                    "Enabled",
                    Box::new(Checkbox::new("启用效果", true).on_change(inspector_bool_action)),
                ))
                .with_row(PropertyRow::new(
                    "Opacity",
                    Box::new(
                        Slider::new(72.0, 0.0, 100.0)
                            .on_change(|value| inspector_value_action("opacity", value)),
                    ),
                ))
                .with_row(PropertyRow::new(
                    "Tint",
                    Box::new(tint.on_change(inspector_color_action)),
                )),
        )
        .with_section(
            PropertySection::new("Transform")
                .with_row(PropertyRow::new(
                    "Position X",
                    Box::new(
                        Slider::new(12.0, -100.0, 100.0)
                            .on_change(|value| inspector_value_action("position_x", value)),
                    ),
                ))
                .with_row(PropertyRow::new(
                    "Position Y",
                    Box::new(
                        Slider::new(-8.0, -100.0, 100.0)
                            .on_change(|value| inspector_value_action("position_y", value)),
                    ),
                ))
                .with_row(PropertyRow::new(
                    "Scale",
                    Box::new(
                        Slider::new(100.0, 25.0, 400.0)
                            .on_change(|value| inspector_value_action("scale", value)),
                    ),
                ))
                .with_row(PropertyRow::new(
                    "Rotation",
                    Box::new(
                        Slider::new(0.0, -180.0, 180.0)
                            .on_change(|value| inspector_value_action("rotation", value)),
                    ),
                )),
        )
        .with_section(
            PropertySection::new("Timing")
                .with_row(PropertyRow::new(
                    "In",
                    Box::new(
                        Slider::new(0.0, 0.0, 240.0)
                            .on_change(|value| inspector_value_action("in", value)),
                    ),
                ))
                .with_row(PropertyRow::new(
                    "Out",
                    Box::new(
                        Slider::new(96.0, 0.0, 240.0)
                            .on_change(|value| inspector_value_action("out", value)),
                    ),
                )),
        )
        .with_section(
            PropertySection::new("Animation").with_row(
                PropertyRow::new(
                    "Curve",
                    Box::new(
                        CurveEditor::with_points(vec![
                            CurvePoint::new(0.0, 0.0),
                            CurvePoint::new(0.35, 0.68),
                            CurvePoint::new(0.72, 0.42),
                            CurvePoint::new(1.0, 1.0),
                        ])
                        .on_change(inspector_curve_action),
                    ),
                )
                .with_height(118.0),
            ),
        );

    Box::new(ScrollView::new(Some(Box::new(panel))))
}

fn inspector_value_action(name: &'static str, value: f32) -> Action {
    Action::Custom {
        namespace: "demo.inspector".into(),
        name: format!("{name}:{value:.3}"),
        payload: serde_json::Value::Null,
    }
}

fn inspector_bool_action(value: bool) -> Action {
    Action::Custom {
        namespace: "demo.inspector".into(),
        name: format!("enabled:{value}"),
        payload: serde_json::Value::Null,
    }
}

fn inspector_color_action(color: Color) -> Action {
    let [r, g, b, a] = color.to_rgba8();
    Action::Custom {
        namespace: "demo.inspector".into(),
        name: format!("tint:{r},{g},{b},{a}"),
        payload: serde_json::Value::Null,
    }
}

fn inspector_curve_action(points: &[CurvePoint]) -> Action {
    let mut name = String::from("curve");
    for point in points {
        name.push_str(&format!(":{:.3},{:.3}", point.x, point.y));
    }
    Action::Custom {
        namespace: "demo.inspector".into(),
        name,
        payload: serde_json::Value::Null,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Dock tree
// ═══════════════════════════════════════════════════════════════════════════

fn build_dock_tree() -> DockSplitter {
    // Top-left: Assets=red (35%) / Bottom-left: widget gallery/effects (65%)
    let left = DockSplitter::new(
        SplitDirection::Vertical,
        0.35,
        dock_panel(SlotKind::Assets),
        dock_panel(SlotKind::Effects),
    );

    let right_bottom = DockSplitter::new(
        SplitDirection::Horizontal,
        0.65,
        dock_panel(SlotKind::Timeline),
        dock_panel(SlotKind::Inspector),
    );

    let right = DockSplitter::new(
        SplitDirection::Vertical,
        0.5,
        dock_panel(SlotKind::Viewer),
        Box::new(right_bottom),
    );

    DockSplitter::new(
        SplitDirection::Horizontal,
        0.3,
        Box::new(left),
        Box::new(right),
    )
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

fn route_demo_window_event(
    window: &winit::window::Window,
    router: &mut EventRouter,
    root: &mut dyn Widget,
    event: UiEvent,
    runtime: &mut WinitUiRuntime,
) -> EventResult {
    runtime.route_window_event(window, router, root, event, &record_demo_action)
}

// ═══════════════════════════════════════════════════════════════════════════
// Main
// ═══════════════════════════════════════════════════════════════════════════

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use winit::event_loop::EventLoop;

    let event_loop = EventLoop::new()?;
    let window_attrs = winit::window::Window::default_attributes()
        .with_title("Mondrian UI — 控件画廊 (Gallery)")
        .with_inner_size(winit::dpi::LogicalSize::new(1440, 860))
        .with_visible(false);

    let window = Arc::new(event_loop.create_window(window_attrs)?);

    let instance_desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    let instance = wgpu::Instance::new(instance_desc);
    let surface = instance.create_surface(window.clone())?;

    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: Some(&surface),
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    })) {
        Ok(a) => a,
        Err(_) => return Err("No suitable GPU adapter".into()),
    };

    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;

    let size = window.inner_size();
    let mut config = surface
        .get_default_config(&adapter, size.width, size.height)
        .ok_or("Failed surface config")?;
    surface.configure(&device, &config);

    let mut frame_renderer = SelfHostedFrameRenderer::new(&device, config.format);
    let mut render_diagnostic_reporter = SelfHostedRenderDiagnosticReporter::default();

    let mut root = build_dock_tree();
    let bounds = Rect::new(0.0, 0.0, size.width as f32, size.height as f32);
    TreeWalker::layout(&mut root, bounds);
    let mut router = EventRouter::with_platform_and_tooltip(
        root.id(),
        Box::new(mondrian_platform::SystemPlatformService),
        Box::new(TooltipManagerImpl::new(450)),
    );

    let mut last_cursor = Point::new(0.0, 0.0);
    let current_bounds = std::cell::Cell::new(bounds);
    let mut modifiers_state = Modifiers::none();
    let mut ui_runtime = WinitUiRuntime::new();
    let mut pending_initial_redraw = true;
    window.set_visible(true);
    window.request_redraw();

    event_loop.run(move |event, elwt| {
        use winit::event::ElementState;
        use winit::event::Event;
        use winit::event::WindowEvent;
        use winit::event_loop::ControlFlow;
        elwt.set_control_flow(ControlFlow::Wait);

        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => elwt.exit(),

            Event::WindowEvent {
                event: WindowEvent::ModifiersChanged(modifiers), ..
            } => {
                modifiers_state = winit_modifiers_to_ui_modifiers(modifiers);
            }

            Event::WindowEvent { event: WindowEvent::Focused(false), .. } => {
                modifiers_state = Modifiers::none();
                let _ = route_demo_window_event(
                    &window,
                    &mut router,
                    &mut root,
                    UiEvent::FocusLost,
                    &mut ui_runtime,
                );
                window.request_redraw();
            }

            // Keyboard input → dispatch KeyDown / TextInput to widget tree
            Event::WindowEvent {
                event: WindowEvent::KeyboardInput { event: key_event, .. },
                ..
            } => {
                let is_escape = matches!(
                    key_event.logical_key,
                    winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
                );
                let pressed = key_event.state == ElementState::Pressed;
                let result = ui_runtime.route_keyboard_input(
                    &window,
                    &mut router,
                    &mut root,
                    &key_event,
                    &mut modifiers_state,
                    &record_demo_action,
                );
                if pressed && is_escape && result == EventResult::Ignored {
                    elwt.exit();
                }
                window.request_redraw();
            }

            // IME composition events
            Event::WindowEvent { event: WindowEvent::Ime(ime), .. } => {
                let _ = ui_runtime.route_ime_event(
                    &window,
                    &mut router,
                    &mut root,
                    ime,
                    &record_demo_action,
                );
                window.request_redraw();
            }

            Event::WindowEvent { event: WindowEvent::RedrawRequested, .. } => {
                let mut encoder = DrawEncoder::new();
                let theme = mondrian_ui_theme::current_theme();
                let b = current_bounds.get();
                encoder.draw_rect(b, theme.colors.background, 0.0);
                TreeWalker::paint_clipped(&root, &mut encoder, &theme, b);

                ui_runtime.paint_shell_overlays(&mut encoder, &theme, b, last_cursor, &router);

                let size = window.inner_size();
                let frame_result = frame_renderer.render_draw_commands(
                    &device,
                    &queue,
                    &surface,
                    &config,
                    (size.width, size.height),
                    encoder.finish(),
                );
                if let Some(diagnostics) = render_diagnostic_reporter.changed_failure(frame_result)
                {
                    tracing::warn!(
                        "ui_demo render resource failures: missing_glyphs={}, raster_image_failures={}",
                        diagnostics.text_missing_glyphs,
                        diagnostics.raster_image_failures
                    );
                }
                if frame_result.needs_follow_up_redraw() {
                    window.request_redraw();
                }
                pending_initial_redraw = false;
            }

            Event::WindowEvent { event: WindowEvent::Resized(new_size), .. } => {
                if new_size.width > 0 && new_size.height > 0 {
                    config.width = new_size.width;
                    config.height = new_size.height;
                    surface.configure(&device, &config);
                    let new_bounds =
                        Rect::new(0.0, 0.0, new_size.width as f32, new_size.height as f32);
                    current_bounds.set(new_bounds);
                    TreeWalker::layout(&mut root, new_bounds);
                    window.request_redraw();
                }
            }

            Event::WindowEvent { event: WindowEvent::HoveredFile(path), .. } => {
                let _ = ui_runtime.route_hovered_file(
                    &window,
                    &mut router,
                    &mut root,
                    path,
                    last_cursor,
                    &record_demo_action,
                );
                window.request_redraw();
            }

            Event::WindowEvent { event: WindowEvent::HoveredFileCancelled, .. } => {
                let _ = ui_runtime.route_hovered_file_cancelled(
                    &window,
                    &mut router,
                    &mut root,
                    &record_demo_action,
                );
                window.request_redraw();
            }

            Event::WindowEvent { event: WindowEvent::DroppedFile(path), .. } => {
                let _ = ui_runtime.route_dropped_file(
                    &window,
                    &mut router,
                    &mut root,
                    path,
                    last_cursor,
                    &record_demo_action,
                );
                window.request_redraw();
            }

            Event::WindowEvent {
                event: WindowEvent::CursorMoved { position, .. }, ..
            } => {
                last_cursor = Point::new(position.x as f32, position.y as f32);

                ui_runtime.update_eyedropper_preview_at_window_point(&window, last_cursor);

                let _ = route_demo_window_event(
                    &window,
                    &mut router,
                    &mut root,
                    UiEvent::MouseMove { position: last_cursor, modifiers: modifiers_state },
                    &mut ui_runtime,
                );
                let grab_zones = root.collect_grab_zones();
                let direction =
                    grab_zones.iter().find(|(z, _)| z.contains(last_cursor)).map(|(_, d)| *d);
                let is_text = TEXT_INPUT_BOUNDS
                    .with(|b| b.get().is_some_and(|r| r.contains(last_cursor)))
                    && TEXT_INPUT_ID.with(|id| id.get()) == router.focused();
                window.set_cursor_icon(winit_cursor_icon_for_ui_state(
                    ui_runtime.is_eyedropper_active(),
                    direction,
                    is_text,
                ));
                window.request_redraw();
            }

            Event::WindowEvent {
                event: WindowEvent::MouseInput { state, button, .. },
                ..
            } => {
                let event = match state {
                    ElementState::Pressed => UiEvent::MouseDown {
                        position: last_cursor,
                        button: winit_mouse_button_to_ui_button(button),
                        modifiers: modifiers_state,
                    },
                    ElementState::Released => UiEvent::MouseUp {
                        position: last_cursor,
                        button: winit_mouse_button_to_ui_button(button),
                        modifiers: modifiers_state,
                    },
                };
                let is_press =
                    matches!(event, UiEvent::MouseDown { button: MouseButton::Left, .. });

                if is_press && ui_runtime.is_eyedropper_active() {
                    ui_runtime.finish_eyedropper_at_window_point(
                        &window,
                        &mut router,
                        &mut root,
                        last_cursor,
                        &record_demo_action,
                    );
                } else {
                    let _ = route_demo_window_event(
                        &window,
                        &mut router,
                        &mut root,
                        event,
                        &mut ui_runtime,
                    );
                }
                window.request_redraw();
            }

            Event::WindowEvent { event: WindowEvent::MouseWheel { delta, .. }, .. } => {
                let _ = route_demo_window_event(
                    &window,
                    &mut router,
                    &mut root,
                    UiEvent::MouseWheel {
                        delta: winit_scroll_delta_to_ui_delta(delta),
                        position: last_cursor,
                        modifiers: modifiers_state,
                    },
                    &mut ui_runtime,
                );
                window.request_redraw();
            }

            Event::AboutToWait => {
                ui_runtime.drive_timers(&window, &mut router, elwt);
                if pending_initial_redraw {
                    window.request_redraw();
                    elwt.set_control_flow(ControlFlow::Poll);
                }
                if ui_runtime.is_eyedropper_active() {
                    ui_runtime.poll_eyedropper(
                        &window,
                        &mut router,
                        &mut root,
                        &mut last_cursor,
                        modifiers_state,
                        &record_demo_action,
                    );
                    window.request_redraw();
                    elwt.set_control_flow(ControlFlow::Poll);
                }
            }
            _ => {}
        }
    })?;

    Ok(())
}

// ── Shape Panel Widget ────────────────────────────────────────────────────────
/// 形状面板：在效果/组件画廊的“形状”标签页中绘制 3 个圆形（大/中/小），
/// 中号圆形不绘制背景矩形。
struct ShapePanelWidget {
    id: WidgetId,
    bounds: Rect,
}

impl ShapePanelWidget {
    fn new() -> Self {
        Self { id: WidgetId::new(), bounds: Rect::ZERO }
    }
}

impl Widget for ShapePanelWidget {
    fn id(&self) -> WidgetId {
        self.id
    }
    fn measure(&self, _c: LayoutConstraint) -> Size {
        Size::new(500.0, 300.0)
    }
    fn layout(&mut self, b: Rect) {
        self.bounds = b;
    }
    fn event(&mut self, _e: &UiEvent, _c: &mut EventContext) -> EventResult {
        EventResult::Ignored
    }
    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let anchor_bg = Color::from_hex(0x3A3A5A);
        // Snap base origin to integer pixel to eliminate subpixel jitter from
        // dock-splitter fractional layout.
        let bx = self.bounds.x.round();
        let by = self.bounds.y.round();

        // 大圆 — 200x200，有背景矩形 + 四边定位标记 + 圆心
        let large_size = 200.0;
        let large_x = bx + 30.0;
        let large_y = by + 40.0;
        let large_color = Color { r: 0.94, g: 0.27, b: 0.27, a: 0.65 };
        ctx.encoder.draw_rect(
            Rect::new(large_x, large_y, large_size, large_size),
            anchor_bg,
            0.0,
        );
        ctx.encoder.draw_rect(
            Rect::new(large_x, large_y, large_size, large_size),
            large_color,
            large_size * 0.5,
        );
        // 圆心 + 四边中点标记
        let cx = large_x + large_size * 0.5;
        let cy = large_y + large_size * 0.5;
        let marker = Color::WHITE;
        let ms = 4.0; // 标记半宽
        ctx.encoder
            .draw_rect(Rect::new(cx - ms, cy - ms, ms * 2.0, ms * 2.0), marker, 0.0);
        ctx.encoder.draw_rect(
            Rect::new(cx - ms, large_y - 8.0, ms * 2.0, 8.0),
            marker,
            0.0,
        );
        ctx.encoder.draw_rect(
            Rect::new(cx - ms, large_y + large_size, ms * 2.0, 8.0),
            marker,
            0.0,
        );
        ctx.encoder.draw_rect(
            Rect::new(large_x - 8.0, cy - ms, 8.0, ms * 2.0),
            marker,
            0.0,
        );
        ctx.encoder.draw_rect(
            Rect::new(large_x + large_size, cy - ms, 8.0, ms * 2.0),
            marker,
            0.0,
        );
        ctx.encoder.draw_text(
            "r=100 大圆",
            11.0,
            ui_types::snap_point(Point::new(large_x, large_y + large_size + 14.0)),
            tokens.foreground,
        );

        // 中圆 — 80x80，无背景矩形
        let med_size = 80.0;
        let med_x = large_x + large_size + 40.0;
        let med_y = large_y + 100.0;
        let med_color = Color { r: 0.13, g: 0.77, b: 0.37, a: 0.65 };
        ctx.encoder.draw_rect(
            Rect::new(med_x, med_y, med_size, med_size),
            med_color,
            med_size * 0.5,
        );
        let mcx = med_x + med_size * 0.5;
        let mcy = med_y + med_size * 0.5;
        ctx.encoder.draw_rect(
            Rect::new(mcx - ms, mcy - ms, ms * 2.0, ms * 2.0),
            marker,
            0.0,
        );
        ctx.encoder.draw_text(
            "r=40 中圆",
            11.0,
            ui_types::snap_point(Point::new(med_x, med_y + med_size + 14.0)),
            tokens.foreground,
        );

        // 小圆 — 24x24，有背景矩形
        let sm_size = 24.0;
        let sm_x = med_x + med_size + 40.0;
        let sm_y = med_y + 40.0;
        let sm_color = Color { r: 0.23, g: 0.51, b: 0.96, a: 0.75 };
        ctx.encoder.draw_rect(Rect::new(sm_x, sm_y, sm_size, sm_size), anchor_bg, 0.0);
        ctx.encoder.draw_rect(
            Rect::new(sm_x, sm_y, sm_size, sm_size),
            sm_color,
            sm_size * 0.5,
        );
        let scx = sm_x + sm_size * 0.5;
        let scy = sm_y + sm_size * 0.5;
        ctx.encoder.draw_rect(
            Rect::new(scx - ms, scy - ms, ms * 2.0, ms * 2.0),
            marker,
            0.0,
        );
        ctx.encoder.draw_text(
            "r=12 小圆",
            11.0,
            ui_types::snap_point(Point::new(sm_x, sm_y + sm_size + 14.0)),
            tokens.foreground,
        );

        // 胶囊形 — 60x120, r=30，有背景矩形
        let cap_w = 60.0;
        let cap_h = 120.0;
        let cap_r = cap_w * 0.5;
        let cap_x = large_x;
        let cap_y = large_y + large_size + 40.0;
        let cap_color = Color { r: 0.70, g: 0.30, b: 1.00, a: 0.75 };
        ctx.encoder.draw_rect(Rect::new(cap_x, cap_y, cap_w, cap_h), anchor_bg, 0.0);
        ctx.encoder.draw_rect(Rect::new(cap_x, cap_y, cap_w, cap_h), cap_color, cap_r);
        ctx.encoder.draw_text(
            "r=30 胶囊",
            11.0,
            ui_types::snap_point(Point::new(cap_x, cap_y + cap_h + 14.0)),
            tokens.foreground,
        );
    }
    fn hit_test(&self, _p: Point) -> bool {
        false
    }
}

#[cfg(test)]
mod gallery_tests {
    use super::*;

    #[test]
    fn gallery_pointer_dispatch_targets_only_hit_child() {
        let mut gallery = GalleryWidget::new();
        gallery.layout(Rect::new(0.0, 0.0, 500.0, 760.0));

        let scroll_targets = gallery.event_target_children(&UiEvent::MouseWheel {
            delta: 24.0,
            position: Point::new(310.0, 260.0),
            modifiers: Modifiers::none(),
        });
        let list_targets = gallery.event_target_children(&UiEvent::MouseDown {
            position: Point::new(36.0, 260.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        });

        assert_eq!(scroll_targets, vec![8]);
        assert_eq!(list_targets, vec![7]);
    }
}
