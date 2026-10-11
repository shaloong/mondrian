//! Private actual Widget presence for one generic effect parameter section.

use crate::app::preview_native_gui::{NativeVisualGuiSectionPresence, PreviewNativeGuiController};
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use std::sync::Arc;

/// Only constructing the actual control subtree acquires presence. Model
/// cloning, labels, selection and an author-address view cannot acquire it.
pub(crate) fn wrap_section(
    controller: Option<&Arc<PreviewNativeGuiController>>,
    control: Box<dyn Widget>,
) -> Box<dyn Widget> {
    let Some(presence) = controller.and_then(|controller| controller.section_presence()) else {
        return control;
    };
    Box::new(SectionControl { id: WidgetId::new(), control, _presence: presence })
}

struct SectionControl {
    id: WidgetId,
    control: Box<dyn Widget>,
    _presence: NativeVisualGuiSectionPresence,
}
impl Widget for SectionControl {
    fn id(&self) -> WidgetId {
        self.id
    }
    fn measure(&self, constraint: LayoutConstraint) -> Size {
        self.control.measure(constraint)
    }
    fn layout(&mut self, bounds: Rect) {
        self.control.layout(bounds);
    }
    fn event(&mut self, event: &UiEvent, context: &mut EventContext) -> EventResult {
        self.control.event(event, context)
    }
    fn paint(&self, context: &mut PaintContext) {
        self.control.paint(context);
    }
    fn hit_test(&self, point: Point) -> bool {
        self.control.hit_test(point)
    }
    fn child_count(&self) -> usize {
        1
    }
    fn child(&self, index: usize) -> Option<&dyn Widget> {
        (index == 0).then_some(self.control.as_ref())
    }
    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        if index == 0 {
            Some(self.control.as_mut())
        } else {
            None
        }
    }
}
