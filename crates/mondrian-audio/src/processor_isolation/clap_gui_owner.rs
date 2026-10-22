//! Same-instance Win32 CLAP GUI lifetime on the actual initializing main thread.
//! The existing native editor parent/WndProc is shared with VST; no second UI
//! controller, native instance, author lease or request dispatcher is created.

use super::clap_gui_requests::GuiIncarnationIssuer;
use super::clap_worker::ClapHost;
use crate::AudioProcessorHostError;
use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiSize, PluginGui, Window};
use clack_host::prelude::PluginInstance;
use std::rc::Rc;
use vst3_host::window::{
    WindowsEditorMainThread, WindowsMessageStep, WindowsNativeEditorContainer,
};

struct CreatedGui {
    gui: PluginGui,
    parent: WindowsNativeEditorContainer,
    size: GuiSize,
    can_resize: bool,
    close_requested: bool,
    externally_destroyed: bool,
    gui_destroyed: bool,
}

pub(super) struct ClapGuiOwner {
    main: Rc<WindowsEditorMainThread>,
    incarnation: GuiIncarnationIssuer,
    created: Option<CreatedGui>,
}

/// A same-owner, synchronous capability result, never a persistent attachment.
/// Actual create can still fail after a parameter exchange has mutated native state.
pub(super) enum ClapGuiOpenPreflight {
    Unavailable,
    Attached(GuiSize),
    Create(PluginGui),
}

impl ClapGuiOwner {
    pub(super) fn new(main: Rc<WindowsEditorMainThread>) -> Self {
        Self {
            main,
            incarnation: GuiIncarnationIssuer::new(),
            created: None,
        }
    }

    /// Check the original same-instance GUI capability before parameter mutation.
    /// A live attachment retains the old idempotent Open contract.
    pub(super) fn prepare_open(
        &self,
        instance: &mut PluginInstance<ClapHost>,
    ) -> ClapGuiOpenPreflight {
        if let Some(created) = &self.created {
            return if !created.gui_destroyed && !created.close_requested {
                ClapGuiOpenPreflight::Attached(created.size)
            } else {
                ClapGuiOpenPreflight::Unavailable
            };
        }
        let handle = instance.plugin_handle();
        let Some(gui) = handle.get_extension::<PluginGui>() else {
            return ClapGuiOpenPreflight::Unavailable;
        };
        let configuration = GuiConfiguration { api_type: GuiApiType::WIN32, is_floating: false };
        if !gui.is_api_supported(&handle, configuration) {
            return ClapGuiOpenPreflight::Unavailable;
        }
        ClapGuiOpenPreflight::Create(gui)
    }

    /// Consume only the immediately preceding same-owner capability result.
    /// GUI calls remain outside the exclusive active parameter audio role.
    pub(super) fn open_prepared(
        &mut self,
        instance: &mut PluginInstance<ClapHost>,
        title: &str,
        preflight: ClapGuiOpenPreflight,
    ) -> Result<Option<GuiSize>, AudioProcessorHostError> {
        let gui = match preflight {
            ClapGuiOpenPreflight::Unavailable => return Ok(None),
            ClapGuiOpenPreflight::Attached(size) => return Ok(Some(size)),
            ClapGuiOpenPreflight::Create(gui) => gui,
        };
        let handle = instance.plugin_handle();
        let configuration = GuiConfiguration { api_type: GuiApiType::WIN32, is_floating: false };
        let incarnation =
            self.incarnation.issue().ok_or_else(|| failed("GUI incarnation exhausted"))?;
        if gui.create(&handle, configuration).is_err() {
            return Ok(None);
        }
        // Every error after create acknowledges GUI destroy before destroying
        // a borrowed HWND. No attachment callback is paired with a fake success.
        let Some(size) = gui.get_size(&handle).filter(valid_size) else {
            gui.destroy(&handle);
            return Err(failed("native GUI returned an invalid initial size"));
        };
        let mut parent = match WindowsNativeEditorContainer::create(
            Rc::clone(&self.main),
            title,
            size.width as i32,
            size.height as i32,
        ) {
            Ok(parent) => parent,
            Err(error) => {
                gui.destroy(&handle);
                return Err(failed(&format!("native parent create failed: {error}")));
            }
        };
        let attached = (|| {
            if let Some(scale) = parent.scale().map_err(|error| failed(&error.to_string()))? {
                // The specification permits declining or ignoring host scaling.
                let _ = gui.set_scale(&handle, scale);
            }
            // Scale may change the plugin's physical dimensions. Query the
            // actual size after that optional call, before embedding or show.
            let scaled_size = gui
                .get_size(&handle)
                .filter(valid_size)
                .ok_or_else(|| failed("native GUI returned invalid scaled size"))?;
            parent
                .resize(scaled_size.width as i32, scaled_size.height as i32)
                .map_err(|error| failed(&error.to_string()))?;
            let hwnd = parent.hwnd().map_err(|error| failed(&error.to_string()))?;
            // SAFETY: parent is held by this same main owner until actual
            // PluginGui.destroy returns, including failure cleanup below.
            let window = unsafe { Window::from_win32_hwnd(hwnd) };
            // SAFETY: the live parent has the exact lifetime described above.
            unsafe { gui.set_parent(&handle, window) }
                .map_err(|error| failed(&error.to_string()))?;
            Ok::<GuiSize, AudioProcessorHostError>(scaled_size)
        })();
        let size = match attached {
            Ok(size) => size,
            Err(error) => {
                gui.destroy(&handle);
                parent
                    .destroy()
                    .map_err(|cleanup| failed(&format!("{error}; parent cleanup: {cleanup}")))?;
                return Err(error);
            }
        };
        let can_resize = gui.can_resize(&handle);
        instance.access_shared_handler(|shared| shared.services.gui.activate(incarnation, true));
        let shown = gui
            .show(&instance.plugin_handle())
            .map_err(|error| failed(&error.to_string()))
            .and_then(|_| parent.set_visible(true).map_err(|error| failed(&error.to_string())));
        if let Err(error) = shown {
            instance.access_shared_handler(|shared| shared.services.gui.revoke());
            gui.destroy(&instance.plugin_handle());
            parent
                .destroy()
                .map_err(|cleanup| failed(&format!("{error}; parent cleanup: {cleanup}")))?;
            return Err(error);
        }
        self.created = Some(CreatedGui {
            gui,
            parent,
            size,
            can_resize,
            close_requested: false,
            externally_destroyed: false,
            gui_destroyed: false,
        });
        Ok(Some(size))
    }

    /// Main service is bounded by the shared Windows dispatcher and fixed
    /// request owners. Native callbacks execute without a host request lock.
    pub(super) fn service(
        &mut self,
        instance: &mut PluginInstance<ClapHost>,
    ) -> Result<(), AudioProcessorHostError> {
        let Some(created) = self.created.as_mut() else {
            return check_step(
                self.main.dispatch_messages().map_err(|error| failed(&error.to_string()))?,
            );
        };
        let events = created.parent.service().map_err(|error| failed(&error.to_string()))?;
        check_step(events.step)?;
        let intents = instance.access_shared_handler(|shared| shared.services.gui.take());
        created.externally_destroyed |= intents.closed == Some(true);
        created.close_requested |= intents.closed.is_some()
            || created.parent.closed_by_user().map_err(|error| failed(&error.to_string()))?;
        if created.gui_destroyed || created.close_requested {
            return Ok(());
        }
        let handle = instance.plugin_handle();
        if let Some(scale) = events.scale {
            let _ = created.gui.set_scale(&handle, scale);
            let size = created
                .gui
                .get_size(&handle)
                .filter(valid_size)
                .ok_or_else(|| failed("native GUI returned invalid DPI-adjusted size"))?;
            created
                .parent
                .resize(size.width as i32, size.height as i32)
                .map_err(|error| failed(&error.to_string()))?;
            created.size = size;
        }
        if intents.hints_changed {
            // The host does not infer an author edit or arbitrary size from
            // hints; actual host drag uses adjust_size below for every request.
            let _ = created.gui.get_resize_hints(&handle);
            created.can_resize = created.gui.can_resize(&handle);
        }
        if let Some((width, height)) = events
            .client_size
            .filter(|_| events.scale.is_none())
            .filter(|size| *size != (created.size.width as i32, created.size.height as i32))
        {
            let requested = GuiSize {
                width: u32::try_from(width).unwrap_or(0),
                height: u32::try_from(height).unwrap_or(0),
            };
            let accepted = if created.can_resize && valid_size(&requested) {
                created
                    .gui
                    .adjust_size(&handle, requested)
                    .filter(valid_size)
                    .filter(|size| created.gui.set_size(&handle, *size).is_ok())
            } else {
                None
            };
            if let Some(size) = accepted {
                created.size = size;
            }
            if accepted != Some(requested) {
                created
                    .parent
                    .resize(created.size.width as i32, created.size.height as i32)
                    .map_err(|error| failed(&error.to_string()))?;
            }
        }
        if let Some(size) = intents.resize {
            if let Err(error) = created.parent.resize(size.width as i32, size.height as i32) {
                // An asynchronous accepted host resize must be reverted in the
                // native GUI when the OS parent cannot satisfy that size.
                created.gui.set_size(&handle, created.size).map_err(|revert| {
                    failed(&format!(
                        "native resize failed: {error}; native size revert failed: {revert}"
                    ))
                })?;
            } else {
                created.size = size;
            }
        }
        if let Some(visible) = intents.visibility {
            let changed = if visible {
                created.gui.show(&handle)
            } else {
                created.gui.hide(&handle)
            };
            if changed.is_ok() {
                created
                    .parent
                    .set_visible(visible)
                    .map_err(|error| failed(&error.to_string()))?;
            }
        }
        Ok(())
    }

    pub(super) fn close_requested(&self) -> Result<bool, AudioProcessorHostError> {
        self.created.as_ref().map_or(Ok(false), |created| {
            created
                .parent
                .closed_by_user()
                .map(|closed| closed || created.close_requested)
                .map_err(|error| failed(&error.to_string()))
        })
    }

    /// Called only after the same scoped DSP role has actually joined.
    /// PluginGui.destroy is the standard acknowledgement even after closed(true).
    /// Parent destruction failure never repeats a successfully acknowledged GUI
    /// destroy and must be surfaced before module/instance retirement.
    pub(super) fn close(
        &mut self,
        instance: &mut PluginInstance<ClapHost>,
    ) -> Result<(), AudioProcessorHostError> {
        let Some(created) = self.created.as_mut() else {
            return Ok(());
        };
        // A plugin may report closed(true) between the last idle main tick and
        // this full-owner Close command. Consume that exact incarnation before
        // revoke so hide is never called against an already destroyed GUI.
        let final_intents = instance.access_shared_handler(|shared| shared.services.gui.take());
        created.externally_destroyed |= final_intents.closed == Some(true);
        instance.access_shared_handler(|shared| shared.services.gui.revoke());
        if !created.gui_destroyed {
            let handle = instance.plugin_handle();
            // closed(true) means the native GUI is already gone; do not call
            // hide against that destroyed GUI before its destroy acknowledgement.
            if !created.externally_destroyed {
                let _ = created.gui.hide(&handle);
            }
            created.gui.destroy(&handle);
            created.gui_destroyed = true;
        }
        created.parent.destroy().map_err(|error| failed(&error.to_string()))?;
        self.created = None;
        Ok(())
    }
}

fn valid_size(size: &GuiSize) -> bool {
    size.width != 0
        && size.height != 0
        && size.width <= i32::MAX as u32
        && size.height <= i32::MAX as u32
}

fn check_step(step: WindowsMessageStep) -> Result<(), AudioProcessorHostError> {
    match step {
        WindowsMessageStep::Quit { exit_code } => {
            Err(failed(&format!("actual WM_QUIT: {exit_code}")))
        }
        _ => Ok(()),
    }
}

fn failed(detail: &str) -> AudioProcessorHostError {
    AudioProcessorHostError::WorkerFailed {
        operation: "CLAP native GUI main",
        detail: detail.to_owned(),
    }
}
