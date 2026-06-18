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
use crate::self_hosted::preferences_dialog::{
    PreferencesDialog, PreferencesDialogTab, SelfHostedPreferencesModel,
};

/// Shell-local modal dialog.
pub enum ShellModal {
    About(Box<AboutDialog>),
    NewProject(Box<NewProjectDialog>),
    Preferences(Box<PreferencesDialog>),
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

    /// Build the product preferences modal.
    pub fn preferences(model: SelfHostedPreferencesModel) -> Self {
        Self::Preferences(Box::new(PreferencesDialog::with_model(model)))
    }

    /// Build the product preferences modal with one selected section.
    pub fn preferences_with_tab(
        model: SelfHostedPreferencesModel,
        tab: PreferencesDialogTab,
    ) -> Self {
        Self::Preferences(Box::new(PreferencesDialog::with_model_and_tab(model, tab)))
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

    /// Access the preferences modal when it is active.
    pub fn as_preferences(&self) -> Option<&PreferencesDialog> {
        match self {
            Self::Preferences(dialog) => Some(dialog.as_ref()),
            _ => None,
        }
    }

    /// Mutably access the preferences modal when it is active.
    pub fn as_preferences_mut(&mut self) -> Option<&mut PreferencesDialog> {
        match self {
            Self::Preferences(dialog) => Some(dialog.as_mut()),
            _ => None,
        }
    }
}

impl Widget for ShellModal {
    fn id(&self) -> WidgetId {
        match self {
            Self::About(dialog) => dialog.id(),
            Self::NewProject(dialog) => dialog.id(),
            Self::Preferences(dialog) => dialog.id(),
        }
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        match self {
            Self::About(dialog) => dialog.measure(constraint),
            Self::NewProject(dialog) => dialog.measure(constraint),
            Self::Preferences(dialog) => dialog.measure(constraint),
        }
    }

    fn layout(&mut self, bounds: Rect) {
        match self {
            Self::About(dialog) => dialog.layout(bounds),
            Self::NewProject(dialog) => dialog.layout(bounds),
            Self::Preferences(dialog) => dialog.layout(bounds),
        }
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match self {
            Self::About(dialog) => dialog.event(event, ctx),
            Self::NewProject(dialog) => dialog.event(event, ctx),
            Self::Preferences(dialog) => dialog.event(event, ctx),
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        match self {
            Self::About(dialog) => dialog.paint(ctx),
            Self::NewProject(dialog) => dialog.paint(ctx),
            Self::Preferences(dialog) => dialog.paint(ctx),
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        match self {
            Self::About(dialog) => dialog.hit_test(point),
            Self::NewProject(dialog) => dialog.hit_test(point),
            Self::Preferences(dialog) => dialog.hit_test(point),
        }
    }

    fn overlay_hit_test(&self, point: Point) -> bool {
        self.hit_test(point)
    }

    fn child_count(&self) -> usize {
        match self {
            Self::About(dialog) => dialog.child_count(),
            Self::NewProject(dialog) => dialog.child_count(),
            Self::Preferences(dialog) => dialog.child_count(),
        }
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match self {
            Self::About(dialog) => dialog.child(index),
            Self::NewProject(dialog) => dialog.child(index),
            Self::Preferences(dialog) => dialog.child(index),
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match self {
            Self::About(dialog) => dialog.child_mut(index),
            Self::NewProject(dialog) => dialog.child_mut(index),
            Self::Preferences(dialog) => dialog.child_mut(index),
        }
    }
}
