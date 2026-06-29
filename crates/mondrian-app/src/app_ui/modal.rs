//! Modal host for app UI shell-local dialogs.
//!
//! `AppUiAppRoot` owns at most one modal at a time. Concrete dialogs stay
//! in their own modules; this enum is the narrow top-layer boundary that root
//! layout, event routing, painting, and tests can depend on.

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};

use crate::app_ui::about_dialog::AboutDialog;
use crate::app_ui::new_project_dialog::{AppUiNewProjectDraft, NewProjectDialog};
use crate::app_ui::pending_close_dialog::{PendingCloseDialog, PendingCloseDialogAction};
use crate::app_ui::preferences_dialog::{
    AppUiPreferencesModel, PreferencesDialog, PreferencesDialogTab,
};
use crate::app_ui::sequence_settings_dialog::{AppUiSequenceSettingsDraft, SequenceSettingsDialog};

/// Shell-local modal dialog.
pub enum ShellModal {
    About(Box<AboutDialog>),
    NewProject(Box<NewProjectDialog>),
    PendingClose(Box<PendingCloseDialog>),
    Preferences(Box<PreferencesDialog>),
    SequenceSettings(Box<SequenceSettingsDialog>),
}

impl ShellModal {
    /// Build the product about modal.
    pub fn about() -> Self {
        Self::About(Box::default())
    }

    /// Build the new-project modal from an initial draft.
    pub fn new_project(draft: AppUiNewProjectDraft) -> Self {
        Self::NewProject(Box::new(NewProjectDialog::new(draft)))
    }

    /// Build the pending-close confirmation modal.
    pub fn pending_close(action: PendingCloseDialogAction) -> Self {
        Self::PendingClose(Box::new(PendingCloseDialog::new(action)))
    }

    /// Build the product preferences modal.
    pub fn preferences(model: AppUiPreferencesModel) -> Self {
        Self::Preferences(Box::new(PreferencesDialog::with_model(model)))
    }

    /// Build the product preferences modal with one selected section.
    pub fn preferences_with_tab(model: AppUiPreferencesModel, tab: PreferencesDialogTab) -> Self {
        Self::Preferences(Box::new(PreferencesDialog::with_model_and_tab(model, tab)))
    }

    /// Build the active-sequence settings modal.
    pub fn sequence_settings(draft: AppUiSequenceSettingsDraft) -> Self {
        Self::SequenceSettings(Box::new(SequenceSettingsDialog::new(draft)))
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

    /// Access the pending-close modal when it is active.
    pub fn as_pending_close(&self) -> Option<&PendingCloseDialog> {
        match self {
            Self::PendingClose(dialog) => Some(dialog.as_ref()),
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

    /// Access the sequence-settings modal when it is active.
    pub fn as_sequence_settings(&self) -> Option<&SequenceSettingsDialog> {
        match self {
            Self::SequenceSettings(dialog) => Some(dialog.as_ref()),
            _ => None,
        }
    }

    /// Mutably access the sequence-settings modal when it is active.
    pub fn as_sequence_settings_mut(&mut self) -> Option<&mut SequenceSettingsDialog> {
        match self {
            Self::SequenceSettings(dialog) => Some(dialog.as_mut()),
            _ => None,
        }
    }
}

impl Widget for ShellModal {
    fn id(&self) -> WidgetId {
        match self {
            Self::About(dialog) => dialog.id(),
            Self::NewProject(dialog) => dialog.id(),
            Self::PendingClose(dialog) => dialog.id(),
            Self::Preferences(dialog) => dialog.id(),
            Self::SequenceSettings(dialog) => dialog.id(),
        }
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        match self {
            Self::About(dialog) => dialog.measure(constraint),
            Self::NewProject(dialog) => dialog.measure(constraint),
            Self::PendingClose(dialog) => dialog.measure(constraint),
            Self::Preferences(dialog) => dialog.measure(constraint),
            Self::SequenceSettings(dialog) => dialog.measure(constraint),
        }
    }

    fn layout(&mut self, bounds: Rect) {
        match self {
            Self::About(dialog) => dialog.layout(bounds),
            Self::NewProject(dialog) => dialog.layout(bounds),
            Self::PendingClose(dialog) => dialog.layout(bounds),
            Self::Preferences(dialog) => dialog.layout(bounds),
            Self::SequenceSettings(dialog) => dialog.layout(bounds),
        }
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match self {
            Self::About(dialog) => dialog.event(event, ctx),
            Self::NewProject(dialog) => dialog.event(event, ctx),
            Self::PendingClose(dialog) => dialog.event(event, ctx),
            Self::Preferences(dialog) => dialog.event(event, ctx),
            Self::SequenceSettings(dialog) => dialog.event(event, ctx),
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        match self {
            Self::About(dialog) => dialog.paint(ctx),
            Self::NewProject(dialog) => dialog.paint(ctx),
            Self::PendingClose(dialog) => dialog.paint(ctx),
            Self::Preferences(dialog) => dialog.paint(ctx),
            Self::SequenceSettings(dialog) => dialog.paint(ctx),
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        match self {
            Self::About(dialog) => dialog.hit_test(point),
            Self::NewProject(dialog) => dialog.hit_test(point),
            Self::PendingClose(dialog) => dialog.hit_test(point),
            Self::Preferences(dialog) => dialog.hit_test(point),
            Self::SequenceSettings(dialog) => dialog.hit_test(point),
        }
    }

    fn overlay_hit_test(&self, point: Point) -> bool {
        self.hit_test(point)
    }

    fn can_focus(&self) -> bool {
        match self {
            Self::About(dialog) => dialog.can_focus(),
            Self::NewProject(dialog) => dialog.can_focus(),
            Self::PendingClose(dialog) => dialog.can_focus(),
            Self::Preferences(dialog) => dialog.can_focus(),
            Self::SequenceSettings(dialog) => dialog.can_focus(),
        }
    }

    fn accepts_text_input(&self) -> bool {
        match self {
            Self::About(dialog) => dialog.accepts_text_input(),
            Self::NewProject(dialog) => dialog.accepts_text_input(),
            Self::PendingClose(dialog) => dialog.accepts_text_input(),
            Self::Preferences(dialog) => dialog.accepts_text_input(),
            Self::SequenceSettings(dialog) => dialog.accepts_text_input(),
        }
    }

    fn child_count(&self) -> usize {
        match self {
            Self::About(dialog) => dialog.child_count(),
            Self::NewProject(dialog) => dialog.child_count(),
            Self::PendingClose(dialog) => dialog.child_count(),
            Self::Preferences(dialog) => dialog.child_count(),
            Self::SequenceSettings(dialog) => dialog.child_count(),
        }
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match self {
            Self::About(dialog) => dialog.child(index),
            Self::NewProject(dialog) => dialog.child(index),
            Self::PendingClose(dialog) => dialog.child(index),
            Self::Preferences(dialog) => dialog.child(index),
            Self::SequenceSettings(dialog) => dialog.child(index),
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match self {
            Self::About(dialog) => dialog.child_mut(index),
            Self::NewProject(dialog) => dialog.child_mut(index),
            Self::PendingClose(dialog) => dialog.child_mut(index),
            Self::Preferences(dialog) => dialog.child_mut(index),
            Self::SequenceSettings(dialog) => dialog.child_mut(index),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_modal_exposes_dialog_keyboard_focus() {
        let modal = ShellModal::preferences_with_tab(
            AppUiPreferencesModel::default(),
            PreferencesDialogTab::Shortcuts,
        );

        assert!(modal.can_focus());
    }
}
