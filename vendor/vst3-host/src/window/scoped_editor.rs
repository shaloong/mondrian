//! One retained IPlugView attachment over the platform-owned native parent.

use super::{NativeEditorContainer, NativeEditorContainerEvents, NativeEditorMainThread};
use crate::error::{Error, Result};
use crate::plugin::{NativeEditorAttachmentOwner, PluginEditorControl};
use std::rc::Rc;

/// Same-instance editor parent retained through acknowledged view removal.
///
/// Temporary Main/DSP borrows do not close the view or replace its attachment
/// identity. The native container owns no controller; it remains live if foreign
/// removal fails. An isolated consumer must retire before unloading that module.
pub struct PluginEditorParent {
    container: NativeEditorContainer,
    owner: NativeEditorAttachmentOwner,
    view_size: (i32, i32),
}

impl PluginEditorParent {
    /// Attach the sole existing view on its original native control thread.
    pub fn open(
        main: Rc<NativeEditorMainThread>,
        editor: &mut PluginEditorControl<'_>,
        title: &str,
    ) -> Result<Self> {
        main.with_native_scope(|| {
            let owner = editor.attachment_owner()?;
            let view_size = editor.editor_size()?;
            let mut container =
                NativeEditorContainer::create(Rc::clone(&main), title, view_size.0, view_size.1)?;
            let attach = (|| {
                if let Some(scale) = container.scale()? {
                    editor.set_editor_scale_factor(scale as f32)?;
                }
                editor.open_editor(container.window_handle()?)
            })();
            if let Err(error) = attach {
                // A failed attached callback is not paired with removed. The
                // canonical attachment model retains any failed frame cleanup.
                container.destroy()?;
                return Err(error);
            }
            if let Err(error) = container.set_visible(true) {
                // Successful attachment must be removed before parent release,
                // including when showing the host window itself fails.
                editor.close_editor()?;
                container.destroy()?;
                return Err(error);
            }
            Ok(Self {
                container,
                owner,
                view_size,
            })
        })?
    }

    fn service_turn(
        &mut self,
        editor: &mut PluginEditorControl<'_>,
    ) -> Result<NativeEditorContainerEvents> {
        self.container.main.with_native_scope(|| {
            if editor.attachment_owner()? != self.owner {
                return Err(Error::EditorAttachmentOwnerMismatch);
            }
            let events = self.container.service()?;
            editor.service_controller()?;
            editor.service_run_loop()?;
            if let Some(scale) = events.scale {
                editor.set_editor_scale_factor(scale as f32)?;
            }
            if let Some(size) = events.client_size.filter(|size| *size != self.view_size) {
                let accepted = editor.resize_editor(size.0, size.1)?;
                self.view_size = accepted;
                if accepted != size {
                    self.container.resize(accepted.0, accepted.1)?;
                }
            }
            if let Some(size) = editor.take_editor_resize_request()? {
                self.container.resize(size.0, size.1)?;
                self.view_size = size;
            }
            Ok(events)
        })?
    }

    /// Service bounded native messages and this same instance's controller.
    /// The historical Windows queue result, including WM_QUIT, is preserved.
    #[cfg(target_os = "windows")]
    pub fn service(
        &mut self,
        editor: &mut PluginEditorControl<'_>,
    ) -> Result<super::WindowsMessageStep> {
        self.service_turn(editor).map(|events| events.step)
    }

    /// Service bounded platform work and this same instance's controller.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn service(&mut self, editor: &mut PluginEditorControl<'_>) -> Result<()> {
        self.service_turn(editor).map(|_| ())
    }

    /// Observe user close intent without destroying the attached native view.
    pub fn closed_by_user(&self) -> Result<bool> {
        self.container.closed_by_user()
    }

    /// Remove the same view, then destroy its exact native parent.
    /// Failed view or frame cleanup preserves the parent for isolated retirement.
    pub fn close(&mut self, editor: &mut PluginEditorControl<'_>) -> Result<()> {
        let main = Rc::clone(&self.container.main);
        main.with_native_scope(|| {
            if editor.attachment_owner()? != self.owner {
                return Err(Error::EditorAttachmentOwnerMismatch);
            }
            if self.container.is_live()? {
                editor.close_editor()?;
                self.container.destroy()?;
            }
            Ok(())
        })?
    }
}
