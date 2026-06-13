//! Modal host for self-hosted shell-local dialogs.
//!
//! `SelfHostedAppRoot` owns at most one modal at a time. Concrete dialogs stay
//! in their own modules; this enum is the narrow top-layer boundary that root
//! layout, event routing, painting, and tests can depend on.

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};

use crate::self_hosted::new_project_dialog::{NewProjectDialog, SelfHostedNewProjectDraft};

/// Shell-local modal dialog.
pub enum ShellModal {
    NewProject(NewProjectDialog),
}

impl ShellModal {
    /// Build the new-project modal from an initial draft.
    pub fn new_project(draft: SelfHostedNewProjectDraft) -> Self {
        Self::NewProject(NewProjectDialog::new(draft))
    }

    /// Access the new-project modal when it is active.
    pub fn as_new_project(&self) -> Option<&NewProjectDialog> {
        match self {
            Self::NewProject(dialog) => Some(dialog),
        }
    }

    /// Mutably access the new-project modal when it is active.
    pub fn as_new_project_mut(&mut self) -> Option<&mut NewProjectDialog> {
        match self {
            Self::NewProject(dialog) => Some(dialog),
        }
    }
}

impl Widget for ShellModal {
    fn id(&self) -> WidgetId {
        match self {
            Self::NewProject(dialog) => dialog.id(),
        }
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        match self {
            Self::NewProject(dialog) => dialog.measure(constraint),
        }
    }

    fn layout(&mut self, bounds: Rect) {
        match self {
            Self::NewProject(dialog) => dialog.layout(bounds),
        }
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match self {
            Self::NewProject(dialog) => dialog.event(event, ctx),
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        match self {
            Self::NewProject(dialog) => dialog.paint(ctx),
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        match self {
            Self::NewProject(dialog) => dialog.hit_test(point),
        }
    }

    fn child_count(&self) -> usize {
        match self {
            Self::NewProject(dialog) => dialog.child_count(),
        }
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match self {
            Self::NewProject(dialog) => dialog.child(index),
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match self {
            Self::NewProject(dialog) => dialog.child_mut(index),
        }
    }
}
