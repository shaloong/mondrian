//! Modal host for self-hosted shell-local dialogs.
//!
//! `SelfHostedAppRoot` owns at most one modal at a time. Concrete dialogs stay
//! in their own modules; this enum is the narrow top-layer boundary that root
//! layout, event routing, painting, and tests can depend on.

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};

use crate::self_hosted::about_dialog::AboutDialog;
use crate::self_hosted::new_project_dialog::{NewProjectDialog, SelfHostedNewProjectDraft};

/// Shell-local modal dialog.
pub enum ShellModal {
    About(Box<AboutDialog>),
    NewProject(Box<NewProjectDialog>),
}

impl ShellModal {
    /// Build the product about modal.
    pub fn about() -> Self {
        Self::About(Box::default())
    }

    /// Build the new-project modal from an initial draft.
    pub fn new_project(draft: SelfHostedNewProjectDraft) -> Self {
        Self::NewProject(Box::new(NewProjectDialog::new(draft)))
    }

    /// Access the product about modal when it is active.
    pub fn as_about(&self) -> Option<&AboutDialog> {
        match self {
            Self::About(dialog) => Some(dialog.as_ref()),
            _ => None,
        }
    }

    /// Access the new-project modal when it is active.
    pub fn as_new_project(&self) -> Option<&NewProjectDialog> {
        match self {
            Self::NewProject(dialog) => Some(dialog.as_ref()),
            _ => None,
        }
    }

    /// Mutably access the new-project modal when it is active.
    pub fn as_new_project_mut(&mut self) -> Option<&mut NewProjectDialog> {
        match self {
            Self::NewProject(dialog) => Some(dialog.as_mut()),
            _ => None,
        }
    }
}

impl Widget for ShellModal {
    fn id(&self) -> WidgetId {
        match self {
            Self::About(dialog) => dialog.id(),
            Self::NewProject(dialog) => dialog.id(),
        }
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        match self {
            Self::About(dialog) => dialog.measure(constraint),
            Self::NewProject(dialog) => dialog.measure(constraint),
        }
    }

    fn layout(&mut self, bounds: Rect) {
        match self {
            Self::About(dialog) => dialog.layout(bounds),
            Self::NewProject(dialog) => dialog.layout(bounds),
        }
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match self {
            Self::About(dialog) => dialog.event(event, ctx),
            Self::NewProject(dialog) => dialog.event(event, ctx),
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        match self {
            Self::About(dialog) => dialog.paint(ctx),
            Self::NewProject(dialog) => dialog.paint(ctx),
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        match self {
            Self::About(dialog) => dialog.hit_test(point),
            Self::NewProject(dialog) => dialog.hit_test(point),
        }
    }

    fn child_count(&self) -> usize {
        match self {
            Self::About(dialog) => dialog.child_count(),
            Self::NewProject(dialog) => dialog.child_count(),
        }
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match self {
            Self::About(dialog) => dialog.child(index),
            Self::NewProject(dialog) => dialog.child(index),
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match self {
            Self::About(dialog) => dialog.child_mut(index),
            Self::NewProject(dialog) => dialog.child_mut(index),
        }
    }
}
