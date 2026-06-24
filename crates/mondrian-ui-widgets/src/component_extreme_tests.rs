use std::cell::RefCell;

use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext, PointerCaptureRequest};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

use crate::test_utils::{
    custom_action, make_event_ctx, paint_widget, test_icon, DummyFocus, DummyShortcut, DummyTooltip,
};
use crate::{
    Checkbox, ColorPickerTrigger, ColorPickerTriggerOptions, ContextMenu, CurveEditor, CurvePoint,
    Dropdown, Label, MenuItem, PanelList, PanelListItem, ScrollView, Slider, TextInput,
    TimelineClip, TimelineTrack, TimelineView, ViewerSurface,
};

struct NestedOverlayWidget {
    id: WidgetId,
    bounds: Rect,
    open: bool,
}

impl NestedOverlayWidget {
    fn new() -> Self {
        Self {
            id: WidgetId::new(),
            bounds: Rect::ZERO,
            open: false,
        }
    }

    fn trigger_rect(&self) -> Rect {
        Rect::new(self.bounds.x, self.bounds.y, 64.0, 24.0)
    }

    fn overlay_rect(&self) -> Rect {
        Rect::new(self.bounds.x, self.bounds.y + 28.0, 96.0, 44.0)
    }
}

impl Widget for NestedOverlayWidget {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(72.0, 28.0))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. }
                if self.trigger_rect().contains(*position) =>
            {
                self.open = true;
                ctx.request_repaint();
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        ctx.encoder.draw_rect(self.trigger_rect(), ctx.theme.colors.surface, 4.0);
        ctx.encoder.draw_text(
            "Nested trigger",
            ctx.theme.typography.body.font_size,
            Point::new(self.bounds.x + 6.0, self.bounds.y + 6.0),
            ctx.theme.colors.foreground,
        );
    }

    fn paint_overlay(&self, ctx: &mut PaintContext) {
        if !self.open {
            return;
        }
        let rect = self.overlay_rect();
        ctx.encoder.draw_rect(rect, ctx.theme.colors.popover, 6.0);
        ctx.encoder.draw_text(
            "Nested overlay item",
            ctx.theme.typography.body.font_size,
            Point::new(rect.x + 6.0, rect.y + 8.0),
            ctx.theme.colors.foreground,
        );
    }

    fn overlay_hit_test(&self, point: Point) -> bool {
        self.open && self.overlay_rect().contains(point)
    }

    fn hit_test(&self, point: Point) -> bool {
        self.trigger_rect().contains(point)
    }
}

#[test]
fn extreme_sized_controls_paint_without_leaking_clip_or_nan_geometry() {
    let clip_rect = Rect::new(0.0, 0.0, 96.0, 64.0);

    let mut checkbox = Checkbox::new("A very long label that must not destabilize paint", true);
    checkbox.layout(Rect::new(2.25, 2.75, 12.0, 6.0));
    let checkbox_paint = paint_widget(&checkbox, clip_rect);
    assert!(
        checkbox_paint.triangle_batches.iter().any(|batch| batch.len() >= 12),
        "checked checkbox should draw a vector checkmark"
    );

    let mut slider = Slider::new(50.0, 0.0, 100.0);
    slider.layout(Rect::new(4.5, 12.25, 8.0, 4.0));
    let slider_paint = paint_widget(&slider, clip_rect);
    assert!(
        slider_paint.rects.iter().any(|rect| rect.width <= 8.0 && rect.height <= 4.0),
        "tiny slider should clamp its thumb into the available height"
    );

    let mut input = TextInput::new("placeholder")
        .with_text("abcdefghijklmnopqrstuvwxyz中文输入法预编辑🙂🙂🙂abcdefghijklmnopqrstuvwxyz");
    input.layout(Rect::new(0.0, 20.0, 34.0, 18.0));
    let input_paint = paint_widget(&input, clip_rect);
    assert!(
        input_paint
            .texts
            .iter()
            .any(|text| text.text.contains("abcdefghijklmnopqrstuvwxyz")),
        "long text input should still issue a clipped text draw"
    );

    let mut curve = CurveEditor::with_points(vec![
        CurvePoint::new(-1.0, 2.0),
        CurvePoint::new(0.5, 0.5),
        CurvePoint::new(2.0, -1.0),
    ]);
    curve.layout(Rect::new(40.0, 2.0, 18.0, 18.0));
    let curve_paint = paint_widget(&curve, clip_rect);
    assert!(
        !curve_paint.rects.is_empty() || !curve_paint.lines.is_empty(),
        "curve editor should paint a usable frame even when very small"
    );
}

#[test]
fn slider_extreme_drag_clamps_value_and_releases_pointer_capture() {
    let actions = RefCell::new(Vec::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut slider = Slider::new(50.0, 0.0, 100.0).on_change(|_| Action::ToggleFullscreen);
    slider.layout(Rect::new(10.0, 10.0, 8.0, 4.0));
    let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

    assert_eq!(
        slider.event(
            &UiEvent::MouseDown {
                position: Point::new(14.0, 12.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert!(matches!(
        ctx.requests.pointer_capture,
        Some(PointerCaptureRequest::Capture(_))
    ));

    slider.event(
        &UiEvent::MouseMove {
            position: Point::new(10_000.0, 12.0),
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );
    assert_eq!(slider.value(), 100.0);

    slider.event(
        &UiEvent::MouseUp {
            position: Point::new(10_000.0, 12.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );
    assert!(matches!(
        ctx.requests.pointer_capture,
        Some(PointerCaptureRequest::Release(_))
    ));
    assert!(!actions.borrow().is_empty());
}

#[test]
fn dropdown_overlay_scrolls_in_pc_direction_and_keeps_overlay_balanced() {
    let dispatch = |_| {};
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut items = (0..8)
        .map(|index| MenuItem::new(format!("Item {index}"), custom_action(&format!("i{index}"))))
        .collect::<Vec<_>>();
    items[0] = MenuItem::new("Item 0", custom_action("i0"))
        .with_icon(test_icon())
        .with_shortcut("Ctrl+0");
    let mut dropdown = Dropdown::new("Menu", items).with_max_visible_items(3);
    dropdown.layout(Rect::new(8.0, 8.0, 60.0, 28.0));
    let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

    dropdown.event(
        &UiEvent::MouseDown {
            position: Point::new(20.0, 18.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );
    dropdown.event(
        &UiEvent::MouseUp {
            position: Point::new(20.0, 18.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );

    let before = paint_widget(&dropdown, Rect::new(0.0, 0.0, 180.0, 180.0));
    assert!(
        before.triangle_batches.iter().any(|batch| batch.len() >= 6)
            || !before.raster_images.is_empty(),
        "iconized dropdown rows should paint SVG icon geometry"
    );
    assert!(
        before.texts.iter().any(|text| text.text == "Ctrl+0"),
        "iconized dropdown rows should paint shortcut hints"
    );
    let before_y = before
        .texts
        .iter()
        .find(|text| text.text == "Item 0")
        .expect("item text before scroll")
        .position
        .y;

    dropdown.event(
        &UiEvent::MouseWheel {
            delta: 24.0,
            position: Point::new(20.0, 54.0),
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );
    let after = paint_widget(&dropdown, Rect::new(0.0, 0.0, 180.0, 180.0));
    let after_y = after
        .texts
        .iter()
        .find(|text| text.text == "Item 0")
        .expect("item text after scroll")
        .position
        .y;

    assert!(
        after_y < before_y,
        "positive wheel delta should scroll PC-style content downward, moving rows upward"
    );
}

#[test]
fn color_picker_trigger_outside_click_closes_overlay_after_open() {
    let dispatch = |_| {};
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut trigger = ColorPickerTrigger::with_options(
        Color::from_rgba8(180, 120, 255, 180),
        ColorPickerTriggerOptions {
            trigger_width: 18.0,
            trigger_height: 18.0,
            popup_width: 160.0,
            popup_height: 180.0,
        },
    );
    trigger.layout(Rect::new(4.0, 4.0, 18.0, 18.0));
    let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

    trigger.event(
        &UiEvent::MouseDown {
            position: Point::new(10.0, 10.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );
    trigger.event(
        &UiEvent::MouseUp {
            position: Point::new(10.0, 10.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );
    assert!(trigger.is_open());
    let open_paint = paint_widget(&trigger, Rect::new(0.0, 0.0, 240.0, 240.0));
    assert!(
        open_paint.colored_triangle_batches.len() >= 2,
        "open color picker should paint checkerboard and color area geometry"
    );

    trigger.event(
        &UiEvent::MouseDown {
            position: Point::new(230.0, 230.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );
    assert!(!trigger.is_open());
}

#[test]
fn panel_surfaces_extreme_scroll_keyboard_and_paint_remain_stable() {
    let actions = RefCell::new(Vec::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let items = (0..18)
        .map(|index| {
            PanelListItem::new(format!("Item {index} with a very long production label"))
                .with_subtitle("Nested metadata should wrap or clip without corrupting paint")
                .with_badge(format!("{index:02}"))
                .disabled(index % 7 == 0)
        })
        .collect::<Vec<_>>();
    let mut list = PanelList::new("Assets", items)
        .with_subtitle("Long list stress")
        .with_row_height(40.0)
        .on_select(|_, _| Action::ToggleFullscreen);
    list.layout(Rect::new(0.25, 0.5, 118.0, 124.0));
    let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

    list.event(
        &UiEvent::MouseWheel {
            delta: 240.0,
            position: Point::new(24.0, 86.0),
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );
    assert!(list.scroll_offset_y() > 0.0);

    list.event(&UiEvent::FocusGained, &mut ctx);
    assert_eq!(
        list.event(
            &UiEvent::KeyDown { key: KeyCode::End, modifiers: Modifiers::none() },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert_eq!(list.selected_index(), Some(17));
    assert!(!actions.borrow().is_empty());

    let list_paint = paint_widget(&list, Rect::new(0.0, 0.0, 128.0, 128.0));
    assert!(
        list_paint.texts.iter().any(|text| text.text == "Assets"),
        "panel list should keep painting its header in constrained layouts"
    );

    let mut viewer = ViewerSurface::new("Viewer with extremely narrow chrome", 1, 10_000)
        .with_status("No signal")
        .with_frame_label("F999999")
        .disabled();
    viewer.layout(Rect::new(0.0, 0.0, 20.0, 36.0));
    let viewer_paint = paint_widget(&viewer, Rect::new(0.0, 0.0, 32.0, 48.0));
    assert!(
        !viewer_paint.texts.iter().any(|text| text.text.contains("Viewer")),
        "viewer should not reintroduce title chrome when the canvas collapses"
    );
    assert!(
        viewer_paint.texts.iter().any(|text| text.text.contains("00:00:00:00")),
        "viewer should still issue bounded timecode text when chrome collapses"
    );
}

#[test]
fn overlay_and_scroll_container_extremes_keep_paint_and_event_state_stable() {
    let actions = RefCell::new(Vec::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let mut menu = ContextMenu::new(
        Point::new(-4.5, 3.25),
        vec![
            MenuItem::new("Open with a very long menu label", custom_action("open"))
                .with_icon(test_icon())
                .with_shortcut("Ctrl+O"),
            MenuItem::separator(),
            MenuItem::new("Disabled", custom_action("disabled")).disabled(),
            MenuItem::new("Reveal", custom_action("reveal")),
        ],
    );
    let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

    let menu_paint = paint_widget(&menu, Rect::new(0.0, 0.0, 96.0, 96.0));
    assert!(
        menu_paint.texts.iter().any(|text| text.text.contains("Open")),
        "context menu overlay should paint row labels through the shared menu helpers"
    );
    assert!(
        menu_paint.texts.iter().any(|text| text.text == "Ctrl+O"),
        "context menu overlay should paint shortcut hints"
    );
    assert!(
        menu_paint.triangle_batches.iter().any(|batch| batch.len() >= 6)
            || !menu_paint.raster_images.is_empty(),
        "context menu overlay should paint icon geometry"
    );
    assert!(
        menu.overlay_hit_test(Point::new(90.0, 90.0)),
        "visible overlays should retain a broad close-hit region"
    );

    menu.event(
        &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
        &mut ctx,
    );
    assert_eq!(
        menu.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert!(!menu.is_visible());
    assert_eq!(actions.borrow().len(), 1);

    let child = Label::new(
        "Scrollable child text that is intentionally much taller than its parent viewport",
    )
    .wrapped()
    .with_max_width(42.0);
    let mut scroll = ScrollView::new(Some(Box::new(child)));
    scroll.layout(Rect::new(0.0, 0.0, 48.0, 24.0));
    scroll.event(
        &UiEvent::MouseWheel {
            delta: 64.0,
            position: Point::new(10.0, 10.0),
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );
    assert!(scroll.scroll_offset().y >= 0.0);
    let scroll_paint = paint_widget(&scroll, Rect::new(0.0, 0.0, 48.0, 24.0));
    assert_eq!(
        scroll_paint.clips.first().copied(),
        Some(Rect::new(0.0, 0.0, 40.0, 24.0)),
        "scroll view must establish its viewport clip before painting child content"
    );
    assert!(
        scroll_paint.texts.iter().any(|text| text.text.contains("Scrollable")),
        "scroll view should paint translated child content through a balanced clip"
    );
}

#[test]
fn nested_scroll_views_do_not_clip_child_overlays() {
    let actions = RefCell::new(Vec::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let inner_scroll = ScrollView::new(Some(Box::new(NestedOverlayWidget::new())));
    let mut outer_scroll = ScrollView::new(Some(Box::new(inner_scroll)));
    outer_scroll.layout(Rect::new(0.0, 0.0, 72.0, 32.0));
    let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

    assert_eq!(
        outer_scroll.event(
            &UiEvent::MouseDown {
                position: Point::new(16.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        ),
        EventResult::Handled
    );
    let _ = outer_scroll.event(
        &UiEvent::MouseUp {
            position: Point::new(16.0, 14.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );

    assert!(
        outer_scroll.overlay_hit_test(Point::new(16.0, 50.0)),
        "nested child overlay should remain hit-testable outside the outer scroll viewport"
    );
    let paint = paint_widget(&outer_scroll, Rect::new(0.0, 0.0, 120.0, 120.0));
    assert!(
        paint.texts.iter().any(|text| text.text == "Nested overlay item"),
        "nested child overlay should paint outside ancestor scroll clips"
    );
    assert!(
        paint.clips.iter().any(|clip| clip.height <= 32.0),
        "outer scroll viewport should still establish a bounded content clip"
    );
}

#[test]
fn timeline_extreme_scroll_zoom_and_paint_remain_stable() {
    let actions = RefCell::new(Vec::new());
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let tracks = vec![
        TimelineTrack::video(
            "V1",
            vec![
                TimelineClip::new("Very long clip name that should stay clipped", 0, 30),
                TimelineClip::new("Far clip", 5_000, 25),
            ],
        ),
        TimelineTrack::audio("A1", vec![TimelineClip::new("Audio", 12, 180)]),
    ];
    let mut timeline = TimelineView::new(tracks)
        .with_pixels_per_frame(64.0)
        .on_seek(|_| Action::ToggleFullscreen);
    timeline.layout(Rect::new(0.0, 0.0, 220.0, 118.0));
    let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

    timeline.event(
        &UiEvent::MouseWheel {
            delta: 120.0,
            position: Point::new(180.0, 90.0),
            modifiers: Modifiers::shift(),
        },
        &mut ctx,
    );
    timeline.event(
        &UiEvent::MouseWheel {
            delta: -120.0,
            position: Point::new(130.0, 15.0),
            modifiers: Modifiers { ctrl: true, ..Modifiers::none() },
        },
        &mut ctx,
    );

    let paint = paint_widget(&timeline, Rect::new(0.0, 0.0, 220.0, 118.0));
    assert!(!paint.rects.is_empty());
    assert!(timeline.scroll_x() >= 0.0);
    assert!(timeline.pixels_per_frame().is_finite());
}
