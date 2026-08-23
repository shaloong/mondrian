use std::cell::RefCell;

use mondrian_core::Color;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

use crate::test_utils::{
    custom_action, make_event_ctx, paint_widget, test_icon, DummyFocus, DummyShortcut, DummyTooltip,
};
use crate::{
    Checkbox, ColorPickerTrigger, ColorPickerTriggerOptions, ContextMenu, DockSplitter, Dropdown,
    Label, MenuItem, ScrollView, Slider, TextInput,
};

struct VisualBlock {
    id: WidgetId,
    label: &'static str,
    fill: Color,
    bounds: Rect,
}

impl VisualBlock {
    fn new(label: &'static str, fill: Color) -> Self {
        Self {
            id: WidgetId::new(),
            label,
            fill,
            bounds: Rect::ZERO,
        }
    }
}

impl Widget for VisualBlock {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(80.0, 48.0))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        ctx.encoder.draw_rect(self.bounds, self.fill, ctx.theme.spacing.radius_sm);
        ctx.encoder.draw_text(
            self.label,
            ctx.theme.typography.body.font_size,
            Point::new(self.bounds.x + 8.0, self.bounds.y + 8.0),
            ctx.theme.colors.foreground,
        );
    }
}

#[test]
fn form_control_visual_scenarios_emit_stable_layers_text_and_vectors() {
    let clip = Rect::new(0.0, 0.0, 260.0, 120.0);

    let mut text_input = TextInput::new("Search assets").with_text("Camera_Main_01.mov");
    text_input.layout(Rect::new(12.0, 12.0, 220.0, 30.0));
    let text_paint = paint_widget(&text_input, clip);
    assert!(
        text_paint.rects.len() >= 2,
        "text input should paint field background and border/focus chrome"
    );
    assert!(
        text_paint.clips.iter().any(|clip| clip.width < 220.0),
        "text input text must be clipped to its padded content lane"
    );
    assert!(
        text_paint.texts.iter().any(|text| text.text == "Camera_Main_01.mov"),
        "text input should paint committed text in the visual scenario"
    );

    let mut checkbox = Checkbox::new("Enable effect", true);
    checkbox.layout(Rect::new(12.0, 52.0, 160.0, 24.0));
    let checkbox_paint = paint_widget(&checkbox, clip);
    assert!(
        checkbox_paint.texts.iter().any(|text| text.text == "Enable effect"),
        "checkbox should paint its visible label"
    );
    assert!(
        checkbox_paint.triangle_batches.iter().any(|batch| batch.len() >= 12),
        "checked checkbox should paint a vector checkmark instead of text chrome"
    );

    let mut slider = Slider::new(0.65, 0.0, 1.0).with_step(0.05);
    slider.layout(Rect::new(12.0, 84.0, 180.0, 28.0));
    let slider_paint = paint_widget(&slider, clip);
    assert!(
        slider_paint.rects.len() >= 3,
        "slider should paint track, filled range, and thumb layers"
    );
    assert!(
        slider_paint.rects.iter().all(|rect| rect.height <= 28.0),
        "slider visual layers should remain bounded by the control height"
    );
}

#[test]
fn text_input_visual_scenarios_cover_small_bounds_selection_caret_and_ime() {
    let actions = RefCell::new(Vec::new());
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

    let mut mixed_input = TextInput::new("Search media").with_text("A你🙂B");
    mixed_input.layout(Rect::new(8.0, 8.0, 128.0, 24.0));
    assert_eq!(
        mixed_input.event(&UiEvent::focus_gained_keyboard(), &mut ctx),
        EventResult::Handled
    );
    assert_eq!(
        mixed_input.event(
            &UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: Modifiers { ctrl: true, ..Modifiers::none() },
            },
            &mut ctx,
        ),
        EventResult::Handled
    );
    assert_eq!(
        mixed_input.event(&UiEvent::ImePreedit("pin yin".to_owned()), &mut ctx),
        EventResult::Handled
    );

    let mixed_paint = paint_widget(&mixed_input, Rect::new(0.0, 0.0, 160.0, 48.0));
    assert!(
        mixed_paint.texts.iter().any(|text| text.text == "A你🙂B"),
        "committed mixed Latin/CJK/emoji text should remain visible"
    );
    assert!(
        mixed_paint.texts.iter().any(|text| text.text == "pin yin"),
        "IME preedit text should paint beside the committed text"
    );
    assert!(
        mixed_paint.lines.iter().any(|(_, _, width)| *width > 0.0),
        "IME preedit should emit an underline stroke"
    );
    assert!(
        mixed_paint.rects.len() >= 4,
        "focused selected text input should paint background, selection, and caret chrome"
    );
    assert!(
        mixed_paint
            .clips
            .iter()
            .any(|clip| clip.x > 8.0 && clip.width < 128.0 && clip.height <= 24.0),
        "mixed-script text must stay clipped to the padded content lane"
    );

    let mut tiny_input = TextInput::new("Tiny search").with_text("abcdef 你好");
    tiny_input.layout(Rect::new(8.0, 36.0, 56.0, 16.0));
    let tiny_paint = paint_widget(&tiny_input, Rect::new(0.0, 0.0, 96.0, 64.0));
    assert!(
        tiny_paint.texts.iter().any(|text| text.text == "abcdef 你好"),
        "small text fields should still emit the committed text command"
    );
    assert!(
        tiny_paint.clips.iter().any(|clip| clip.width <= 40.0 && clip.height <= 16.0),
        "small text fields should clip text to a positive, bounded lane"
    );
}

#[test]
fn popup_component_visual_scenarios_emit_overlay_text_icons_and_color_geometry() {
    let actions = RefCell::new(Vec::new());
    let mut focus = DummyFocus;
    let mut shortcut = DummyShortcut;
    let mut tooltip = DummyTooltip;
    let dispatch = |action| actions.borrow_mut().push(action);
    let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

    let mut dropdown = Dropdown::new(
        "Quality",
        vec![
            MenuItem::new("Full", custom_action("full"))
                .with_icon(test_icon())
                .with_shortcut("Ctrl+F"),
            MenuItem::new("Half", custom_action("half")),
        ],
    );
    dropdown.layout(Rect::new(8.0, 8.0, 120.0, 28.0));
    dropdown.event(
        &UiEvent::MouseDown {
            position: Point::new(20.0, 20.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );
    dropdown.event(
        &UiEvent::MouseUp {
            position: Point::new(20.0, 20.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );
    let dropdown_paint = paint_widget(&dropdown, Rect::new(0.0, 0.0, 260.0, 220.0));
    assert!(
        dropdown_paint.texts.iter().any(|text| text.text == "Quality"),
        "dropdown trigger label should remain visible while menu is open"
    );
    assert!(
        dropdown_paint.texts.iter().any(|text| text.text == "Full")
            && dropdown_paint.texts.iter().any(|text| text.text == "Ctrl+F"),
        "open dropdown should paint row labels and shortcuts in the overlay pass"
    );
    assert!(
        dropdown_paint.triangle_batches.iter().any(|batch| batch.len() >= 6)
            || !dropdown_paint.raster_images.is_empty(),
        "dropdown icon rows should emit vector or raster icon geometry"
    );

    let menu = ContextMenu::new(
        Point::new(44.0, 44.0),
        vec![
            MenuItem::new("Cut", custom_action("cut")).with_shortcut("Ctrl+X"),
            MenuItem::separator(),
            MenuItem::new("Disabled", custom_action("disabled")).disabled(),
        ],
    );
    let menu_paint = paint_widget(&menu, Rect::new(0.0, 0.0, 260.0, 220.0));
    assert!(
        menu_paint.texts.iter().any(|text| text.text == "Cut")
            && menu_paint.texts.iter().any(|text| text.text == "Ctrl+X"),
        "context menu visual scenario should include command labels and shortcuts"
    );
    assert!(
        menu_paint.rects.len() >= 5,
        "context menu should paint popover chrome, separator, and row states"
    );

    let mut trigger = ColorPickerTrigger::with_options(
        Color::from_rgba8(52, 118, 204, 220),
        ColorPickerTriggerOptions {
            trigger_width: 24.0,
            trigger_height: 24.0,
            popup_width: 220.0,
            popup_height: 260.0,
        },
    );
    trigger.layout(Rect::new(8.0, 150.0, 24.0, 24.0));
    trigger.event(
        &UiEvent::MouseDown {
            position: Point::new(16.0, 158.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );
    trigger.event(
        &UiEvent::MouseUp {
            position: Point::new(16.0, 158.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        },
        &mut ctx,
    );
    let picker_paint = paint_widget(&trigger, Rect::new(0.0, 0.0, 280.0, 460.0));
    assert!(
        picker_paint.colored_triangle_batches.len() >= 2,
        "open color picker should paint color field/wheel gradient geometry"
    );
    assert!(
        picker_paint.texts.iter().any(|text| text.text == "HEX"),
        "open color picker should paint channel labels through normal text commands"
    );
}

#[test]
fn container_visual_scenarios_keep_scroll_clips_and_splitter_handle_order() {
    let mut scroll = ScrollView::new(Some(Box::new(
        Label::new("Scrollable visual regression content with a deliberately long wrapped row")
            .wrapped()
            .with_max_width(96.0),
    )));
    scroll.layout(Rect::new(0.0, 0.0, 120.0, 48.0));
    let scroll_paint = paint_widget(&scroll, Rect::new(0.0, 0.0, 140.0, 80.0));
    assert!(
        scroll_paint.clips.iter().any(|clip| clip.width <= 112.0 && clip.height <= 48.0),
        "scroll view should establish a bounded viewport clip"
    );
    assert!(
        scroll_paint
            .texts
            .iter()
            .any(|text| text.text.contains("Scrollable visual regression")),
        "scroll view visual scenario should paint child content inside the clip"
    );

    let mut splitter = DockSplitter::new(
        SplitDirection::Horizontal,
        0.45,
        Box::new(VisualBlock::new("Left", Color::from_rgba8(64, 74, 88, 255))),
        Box::new(VisualBlock::new(
            "Right",
            Color::from_rgba8(44, 50, 62, 255),
        )),
    );
    splitter.layout(Rect::new(0.0, 0.0, 240.0, 96.0));
    let splitter_paint = paint_widget(&splitter, Rect::new(0.0, 0.0, 260.0, 120.0));
    assert!(
        splitter_paint.texts.iter().any(|text| text.text == "Left")
            && splitter_paint.texts.iter().any(|text| text.text == "Right"),
        "splitter should paint both child panes before handle chrome"
    );
    let handle_rect = splitter_paint
        .rects
        .last()
        .copied()
        .expect("splitter should paint handle after children");
    assert!(
        handle_rect.width <= 2.0 && handle_rect.height >= 90.0,
        "splitter handle should be the final narrow vertical chrome layer"
    );
}
