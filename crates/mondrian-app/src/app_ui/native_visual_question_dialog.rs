//! An opaque native Question, answered only by actual local Widget input.

use mondrian_effects::EffectProcessorQuestionReply;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_widgets::{Button, DialogSurface, Label, ScrollView};
use std::cell::Cell;

use crate::app::preview_native_gui::NativeVisualQuestionPresentation;
use crate::app_ui::localization::{AppUiLocale, Localizer};

/// The existing shell modal/focus scope holds this exact service delivery.
pub struct NativeVisualQuestionDialog {
    id: WidgetId,
    presentation: NativeVisualQuestionPresentation,
    surface: DialogSurface,
    bounds: Rect,
    card: Rect,
    painted: Cell<bool>,
    #[cfg(feature = "validation")]
    validation_button_rects: [Rect; 3],
    title: Label,
    text: ScrollView,
    yes: Button,
    no: Button,
    dismiss: Button,
}

impl NativeVisualQuestionDialog {
    pub(crate) fn new(presentation: NativeVisualQuestionPresentation, locale: AppUiLocale) -> Self {
        let localizer = Localizer::new(locale).expect("bundled UI catalogs must be valid");
        let theme = mondrian_ui_theme::current_theme();
        let spacing = &theme.spacing;
        let typography = &theme.typography;
        let yes = presentation.clone();
        let no = presentation.clone();
        let dismiss = presentation.clone();
        let body = Label::new(presentation.text())
            .popover_foreground()
            .wrapped()
            .with_font_size(typography.body.font_size)
            .with_padding(0.0, 0.0);
        Self {
            id: WidgetId::new(),
            presentation,
            surface: DialogSurface::new(
                Size::new(spacing.tooltip_max_width, spacing.interact_height * 5.0),
                Size::new(
                    spacing.tooltip_max_width * 2.0,
                    spacing.interact_height * 12.0,
                ),
            )
            .with_content_padding(spacing.lg),
            bounds: Rect::ZERO,
            card: Rect::ZERO,
            painted: Cell::new(false),
            #[cfg(feature = "validation")]
            validation_button_rects: [Rect::ZERO; 3],
            title: Label::new(localizer.text("native-visual-question-title"))
                .popover_foreground()
                .with_font_size(typography.large.font_size)
                .with_padding(0.0, 0.0),
            text: ScrollView::new(Some(Box::new(body))),
            yes: Button::new(localizer.text("native-visual-question-yes")).on_local_activate(
                move || {
                    yes.answer(EffectProcessorQuestionReply::Yes);
                },
            ),
            no: Button::new(localizer.text("native-visual-question-no")).on_local_activate(
                move || {
                    no.answer(EffectProcessorQuestionReply::No);
                },
            ),
            dismiss: Button::new(localizer.text("native-visual-question-dismiss"))
                .on_local_activate(move || {
                    dismiss.dismiss();
                }),
        }
    }

    #[cfg(feature = "validation")]
    pub(crate) fn validation_controls(
        &self,
    ) -> Option<super::native_question_validation::NativeQuestionValidationControls> {
        self.is_current().then(|| {
            super::native_question_validation::NativeQuestionValidationControls {
                presentation: self.presentation.clone(),
                modal: self.id,
                ids: [self.yes.id(), self.no.id(), self.dismiss.id()],
                rects: self.validation_button_rects,
                card: self.card,
            }
        })
    }
    pub(crate) fn is_current(&self) -> bool {
        self.presentation.is_current()
    }

    /// Capture only this original visible paint; marking Presented remains an
    /// operation after the Window reports real surface presentation. A skipped
    /// or reconfigured frame must not suspend the native fault budget.
    pub(crate) fn painted_delivery(&self) -> Option<NativeVisualQuestionPresentation> {
        (self.painted.replace(false) && self.presentation.is_current())
            .then(|| self.presentation.clone())
    }
}

impl Drop for NativeVisualQuestionDialog {
    fn drop(&mut self) {
        self.presentation.dismiss();
    }
}

impl Widget for NativeVisualQuestionDialog {
    fn id(&self) -> WidgetId {
        self.id
    }
    fn measure(&self, _constraint: LayoutConstraint) -> Size {
        self.surface.preferred_size()
    }
    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.card = self.surface.card_rect(bounds);
        self.painted.set(false);
        let theme = mondrian_ui_theme::current_theme();
        let spacing = &theme.spacing;
        let content = self.surface.content_rect(self.card);
        let title_height = theme.typography.large.line_height;
        let button_height = spacing.interact_height;
        let button_y = (content.y + content.height - button_height).max(content.y);
        let text_y = content.y + title_height + spacing.md;
        self.title.layout(Rect::new(content.x, content.y, content.width, title_height));
        self.text.layout(Rect::new(
            content.x,
            text_y,
            content.width,
            (button_y - spacing.md - text_y).max(0.0),
        ));
        let widths = [&self.yes, &self.no, &self.dismiss]
            .map(|button| button.measure(LayoutConstraint::loose(0.0, 0.0)).width);
        let total = widths.iter().sum::<f32>() + spacing.sm * 2.0;
        let mut x = (content.x + content.width - total).max(content.x);
        for (index, (button, width)) in [&mut self.yes, &mut self.no, &mut self.dismiss]
            .into_iter()
            .zip(widths)
            .enumerate()
        {
            let bounds = Rect::new(x, button_y, width, button_height);
            button.layout(bounds);
            #[cfg(feature = "validation")]
            {
                self.validation_button_rects[index] = bounds;
            }
            #[cfg(not(feature = "validation"))]
            let _ = index;
            x += width + spacing.sm;
        }
    }
    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                self.presentation.dismiss();
                ctx.request_repaint();
                EventResult::Handled
            }
            UiEvent::MouseDown { position, .. }
                if self.surface.is_outside_card(self.card, *position) =>
            {
                EventResult::Handled
            }
            _ => {
                for widget in [
                    &mut self.text as &mut dyn Widget,
                    &mut self.yes,
                    &mut self.no,
                    &mut self.dismiss,
                ] {
                    if widget.event(event, ctx) == EventResult::Handled {
                        return EventResult::Handled;
                    }
                }
                EventResult::Ignored
            }
        }
    }
    fn paint(&self, ctx: &mut PaintContext) {
        self.surface.paint(self.bounds, self.card, ctx);
        self.title.paint(ctx);
        self.text.paint(ctx);
        self.yes.paint(ctx);
        self.no.paint(ctx);
        self.dismiss.paint(ctx);
        let visible = self.card.intersection(&ctx.clip_rect);
        self.painted
            .set(visible.width > 0.0 && visible.height > 0.0 && self.presentation.is_current());
    }
    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
    fn child_count(&self) -> usize {
        5
    }
    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.title),
            1 => Some(&self.text),
            2 => Some(&self.yes),
            3 => Some(&self.no),
            4 => Some(&self.dismiss),
            _ => None,
        }
    }
    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.title),
            1 => Some(&mut self.text),
            2 => Some(&mut self.yes),
            3 => Some(&mut self.no),
            4 => Some(&mut self.dismiss),
            _ => None,
        }
    }
}

#[cfg(test)]
mod question_dialog_contract_tests {
    use super::*;
    use crate::app::preview_native_gui::question_contract_tests::fixture;
    use mondrian_ui_core::tree::WidgetTreeView;
    use mondrian_ui_events::EventRouter;

    fn click_target(modal: &crate::app_ui::modal::ShellModal, index: usize) -> Point {
        let widget = modal.child(index).expect("actual Button subtree");
        // Find a real hit point from the independently laid-out Widget. The
        // test never calls Button::activate/Dialog::event to bypass routing.
        for y in 0..768 {
            for x in 0..1024 {
                let point = Point::new(x as f32 + 0.5, y as f32 + 0.5);
                if widget.hit_test(point) {
                    return point;
                }
            }
        }
        panic!("laid-out Button must have a hit region");
    }
    fn pointer(router: &mut EventRouter, modal: &mut dyn Widget, point: Point) {
        for event in [
            UiEvent::MouseDown {
                position: point,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            UiEvent::MouseUp {
                position: point,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
        ] {
            router.route(event, &mut WidgetTreeView::new(modal), &|_| {
                panic!("local reply must not dispatch an author Action")
            });
        }
    }

    #[test]
    fn real_modal_router_mouse_answers_original_no_without_parent_dispatch_interception() {
        let fixture = fixture();
        assert!(fixture.presentation.presented()); // Value-layer precondition; not a real-surface claim.
        let mut modal = crate::app_ui::modal::ShellModal::native_visual_question(
            fixture.presentation.clone(),
            AppUiLocale::EnUs,
        );
        modal.layout(Rect::new(0.0, 0.0, 1024.0, 768.0));
        let target = click_target(&modal, 3);
        let mut router = EventRouter::new(modal.id());
        pointer(&mut router, &mut modal, target);
        assert_eq!(
            fixture.sample(),
            mondrian_effects::EffectProcessorQuestionState::Answered(
                EffectProcessorQuestionReply::No
            )
        );
        assert!(!fixture.session.history().can_undo());
    }

    #[test]
    fn real_modal_router_keyboard_requires_actual_activation_key_and_single_reply() {
        let fixture = fixture();
        let mut modal = crate::app_ui::modal::ShellModal::native_visual_question(
            fixture.presentation.clone(),
            AppUiLocale::ZhCn,
        );
        modal.layout(Rect::new(0.0, 0.0, 1024.0, 768.0));
        let target = click_target(&modal, 2);
        let mut router = EventRouter::new(modal.id());
        pointer(&mut router, &mut modal, target); // Click before surface proof focuses but refuses Yes.
        assert_eq!(
            fixture.sample(),
            mondrian_effects::EffectProcessorQuestionState::Pending
        );
        assert!(fixture.presentation.presented());
        let mut tree = WidgetTreeView::new(&mut modal);
        router.route(
            UiEvent::KeyUp { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut tree,
            &|_| panic!("local reply"),
        );
        assert_eq!(
            fixture.sample(),
            mondrian_effects::EffectProcessorQuestionState::Presented
        );
        router.route(
            UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut tree,
            &|_| panic!("local reply"),
        );
        assert_eq!(
            fixture.sample(),
            mondrian_effects::EffectProcessorQuestionState::Answered(
                EffectProcessorQuestionReply::Yes
            )
        );
        router.route(
            UiEvent::KeyUp { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut tree,
            &|_| panic!("local reply"),
        );
        assert_eq!(
            fixture.sample(),
            mondrian_effects::EffectProcessorQuestionState::Answered(
                EffectProcessorQuestionReply::Yes
            )
        );
        assert!(!fixture.presentation.answer(EffectProcessorQuestionReply::No));
    }

    #[test]
    fn actual_modal_drop_or_escape_returns_failed_without_synthetic_no() {
        for escape in [false, true] {
            let fixture = fixture();
            let mut modal = crate::app_ui::modal::ShellModal::native_visual_question(
                fixture.presentation.clone(),
                AppUiLocale::EnUs,
            );
            modal.layout(Rect::new(0.0, 0.0, 1024.0, 768.0));
            if escape {
                let mut router = EventRouter::new(modal.id());
                router.route(
                    UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
                    &mut WidgetTreeView::new(&mut modal),
                    &|_| panic!("local dismissal"),
                );
            }
            drop(modal);
            assert_eq!(
                fixture.sample(),
                mondrian_effects::EffectProcessorQuestionState::Answered(
                    EffectProcessorQuestionReply::Dismissed
                )
            );
        }
    }
}
