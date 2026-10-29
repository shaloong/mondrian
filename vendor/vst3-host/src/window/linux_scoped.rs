//! Same-control-thread X11 parents for the scoped native editor owner.

use crate::error::{Error, Result};
use crate::plugin::WindowHandle;
use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::thread::{self, ThreadId};
use xcb::{x, Xid};

const MAX_EVENTS_PER_TURN: usize = 64;

/// Affinity of the Linux thread that initializes and services the native plugin.
///
/// This !Send token does not open a display. Headless processing needs no X11
/// connection; an actual editor parent opens and owns its own connection.
pub struct LinuxEditorMainThread {
    thread: ThreadId,
    _main_only: PhantomData<Rc<()>>,
}

impl LinuxEditorMainThread {
    /// Retain the current control thread before loading native plugin code.
    pub fn initialize() -> Result<Self> {
        Ok(Self {
            thread: thread::current().id(),
            _main_only: PhantomData,
        })
    }

    fn ensure_current(&self) -> Result<()> {
        if thread::current().id() != self.thread {
            return Err(Error::Other(
                "native X11 editor changed control thread".to_owned(),
            ));
        }
        Ok(())
    }
}

struct X11Parent {
    connection: xcb::Connection,
    window: x::Window,
    protocols: x::Atom,
    delete_window: x::Atom,
}

/// An X11 parent that owns no plugin, callback context or authoring authority.
///
/// The format adapter must remove its actual view before `destroy`. Drop cannot
/// infer foreign view removal and retains an unresolved parent until process exit.
pub struct LinuxNativeEditorContainer {
    main: Rc<LinuxEditorMainThread>,
    parent: Option<X11Parent>,
    close_requested: Cell<bool>,
}

impl LinuxNativeEditorContainer {
    /// Create an initially hidden parent on the original native control thread.
    pub fn create(
        main: Rc<LinuxEditorMainThread>,
        title: &str,
        width: i32,
        height: i32,
    ) -> Result<Self> {
        main.ensure_current()?;
        let (width, height) = dimensions(width, height)?;
        let (connection, screen) = xcb::Connection::connect(None).map_err(native_error)?;
        let screen = connection
            .get_setup()
            .roots()
            .nth(screen as usize)
            .ok_or_else(|| Error::Other("X11 display has no selected screen".to_owned()))?;
        let window = connection.generate_id();
        connection
            .send_and_check_request(&x::CreateWindow {
                depth: x::COPY_FROM_PARENT as u8,
                wid: window,
                parent: screen.root(),
                x: 0,
                y: 0,
                width,
                height,
                border_width: 0,
                class: x::WindowClass::InputOutput,
                visual: screen.root_visual(),
                value_list: &[
                    x::Cw::BackPixel(screen.black_pixel()),
                    x::Cw::EventMask(x::EventMask::STRUCTURE_NOTIFY),
                ],
            })
            .map_err(native_error)?;
        // Until return, connection Drop also removes this unattached window on
        // every failure. No foreign view has received its id yet.
        let atom = |name: &[u8]| -> Result<x::Atom> {
            connection
                .wait_for_reply(connection.send_request(&x::InternAtom {
                    only_if_exists: false,
                    name,
                }))
                .map(|reply| reply.atom())
                .map_err(native_error)
        };
        let protocols = atom(b"WM_PROTOCOLS")?;
        let delete_window = atom(b"WM_DELETE_WINDOW")?;
        let utf8 = atom(b"UTF8_STRING")?;
        let net_name = atom(b"_NET_WM_NAME")?;
        connection
            .send_and_check_request(&x::ChangeProperty {
                mode: x::PropMode::Replace,
                window,
                property: protocols,
                r#type: x::ATOM_ATOM,
                data: &[delete_window],
            })
            .map_err(native_error)?;
        for property in [x::ATOM_WM_NAME, net_name] {
            connection
                .send_and_check_request(&x::ChangeProperty {
                    mode: x::PropMode::Replace,
                    window,
                    property,
                    r#type: utf8,
                    data: title.as_bytes(),
                })
                .map_err(native_error)?;
        }
        Ok(Self {
            main,
            parent: Some(X11Parent {
                connection,
                window,
                protocols,
                delete_window,
            }),
            close_requested: Cell::new(false),
        })
    }

    fn parent(&self) -> Result<&X11Parent> {
        self.main.ensure_current()?;
        self.parent
            .as_ref()
            .ok_or_else(|| Error::Other("native X11 editor parent is closed".to_owned()))
    }

    /// Whether this owner still retains its native parent.
    pub fn is_live(&self) -> Result<bool> {
        self.main.ensure_current()?;
        Ok(self.parent.is_some())
    }

    /// Borrow the live X11 id; it must survive the attached foreign view.
    pub fn x11_window_id(&self) -> Result<u32> {
        Ok(self.parent()?.window.resource_id())
    }

    /// Borrow the existing VST3 X11 embedding contract, never a Wayland surface.
    pub fn window_handle(&self) -> Result<WindowHandle> {
        Ok(WindowHandle::from_x11(self.x11_window_id()?))
    }

    /// X11 dimensions are pixels; this adapter does not infer monitor scaling.
    pub fn scale(&self) -> Result<Option<f64>> {
        self.parent()?;
        Ok(None)
    }

    /// Map or unmap this parent after the format's own show/hide operation.
    pub fn set_visible(&self, visible: bool) -> Result<()> {
        let parent = self.parent()?;
        if visible {
            parent.connection.send_and_check_request(&x::MapWindow {
                window: parent.window,
            })
        } else {
            parent.connection.send_and_check_request(&x::UnmapWindow {
                window: parent.window,
            })
        }
        .map_err(native_error)
    }

    /// Resize to exact protocol-representable client dimensions, without clamping.
    pub fn resize(&self, width: i32, height: i32) -> Result<()> {
        let (width, height) = dimensions(width, height)?;
        let parent = self.parent()?;
        parent
            .connection
            .send_and_check_request(&x::ConfigureWindow {
                window: parent.window,
                value_list: &[
                    x::ConfigWindow::Width(u32::from(width)),
                    x::ConfigWindow::Height(u32::from(height)),
                ],
            })
            .map_err(native_error)
    }

    /// Consume a bounded batch from this connection without calling any plugin.
    /// WM_DELETE_WINDOW records intent; it never destroys an attached parent.
    pub fn service(&self) -> Result<super::NativeEditorContainerEvents> {
        let parent = self.parent()?;
        let mut client_size = None;
        for _ in 0..MAX_EVENTS_PER_TURN {
            let Some(event) = parent.connection.poll_for_event().map_err(native_error)? else {
                break;
            };
            match event {
                xcb::Event::X(x::Event::ClientMessage(event))
                    if event.window() == parent.window && event.r#type() == parent.protocols =>
                {
                    if let x::ClientMessageData::Data32(data) = event.data() {
                        if data[0] == parent.delete_window.resource_id() {
                            self.close_requested.set(true);
                        }
                    }
                }
                xcb::Event::X(x::Event::ConfigureNotify(event))
                    if event.window() == parent.window =>
                {
                    client_size = Some((i32::from(event.width()), i32::from(event.height())));
                }
                xcb::Event::X(x::Event::DestroyNotify(event))
                    if event.window() == parent.window =>
                {
                    return Err(Error::Other(
                        "native X11 parent destroyed before confirmed view removal".to_owned(),
                    ));
                }
                _ => {}
            }
        }
        Ok(super::NativeEditorContainerEvents {
            scale: None,
            client_size,
        })
    }

    /// Observe the recorded close request without inferring view removal.
    pub fn closed_by_user(&self) -> Result<bool> {
        self.main.ensure_current()?;
        Ok(self.parent.is_some() && self.close_requested.get())
    }

    /// Destroy only after the actual format adapter confirms GUI/view removal.
    /// An X11 failure retains the native owner for explicit isolated retirement.
    pub fn destroy(&mut self) -> Result<()> {
        self.main.ensure_current()?;
        if let Some(parent) = &self.parent {
            parent
                .connection
                .send_and_check_request(&x::DestroyWindow {
                    window: parent.window,
                })
                .map_err(native_error)?;
            self.parent = None;
        }
        Ok(())
    }
}

impl Drop for LinuxNativeEditorContainer {
    fn drop(&mut self) {
        if let Some(parent) = self.parent.take() {
            // Closing the XCB connection would destroy a still-attached parent.
            // Only the original isolated process owner may retire this failure.
            log::error!("native editor parent dropped before confirmed GUI destruction; X11 connection retained until isolated process exit");
            std::mem::forget(parent);
        }
    }
}

fn dimensions(width: i32, height: i32) -> Result<(u16, u16)> {
    let (Ok(width), Ok(height)) = (u16::try_from(width), u16::try_from(height)) else {
        return Err(Error::Other(
            "native X11 editor dimensions exceed the protocol".to_owned(),
        ));
    };
    if width == 0 || height == 0 {
        return Err(Error::Other(
            "native X11 editor dimensions must be positive".to_owned(),
        ));
    }
    Ok((width, height))
}

fn native_error(error: impl std::fmt::Display) -> Error {
    Error::Other(format!("native X11 editor: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_dimensions_are_refused_before_opening_a_display() {
        let main = Rc::new(LinuxEditorMainThread::initialize().unwrap());
        for size in [
            (0, 1),
            (1, 0),
            (-1, 1),
            (1, -1),
            (i32::from(u16::MAX) + 1, 1),
        ] {
            assert!(
                LinuxNativeEditorContainer::create(Rc::clone(&main), "", size.0, size.1).is_err()
            );
        }
        assert_eq!(dimensions(1, i32::from(u16::MAX)).unwrap(), (1, u16::MAX));
    }

    #[test]
    #[ignore = "explicit authenticated X11 display; real native parent lifecycle"]
    fn real_parent_preserves_close_intent_until_explicit_retirement() {
        let main = Rc::new(LinuxEditorMainThread::initialize().unwrap());
        for _ in 0..3 {
            let mut container =
                LinuxNativeEditorContainer::create(Rc::clone(&main), "Native parent", 240, 120)
                    .unwrap();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                container.set_visible(true).unwrap();
                container.resize(317, 193).unwrap();
                let parent = container.parent().unwrap();
                let geometry = parent
                    .connection
                    .wait_for_reply(parent.connection.send_request(&x::GetGeometry {
                        drawable: x::Drawable::Window(parent.window),
                    }))
                    .unwrap();
                assert_eq!((geometry.width(), geometry.height()), (317, 193));
                let close = x::ClientMessageEvent::new(
                    parent.window,
                    parent.protocols,
                    x::ClientMessageData::Data32([parent.delete_window.resource_id(), 0, 0, 0, 0]),
                );
                parent
                    .connection
                    .send_and_check_request(&x::SendEvent {
                        propagate: false,
                        destination: x::SendEventDest::Window(parent.window),
                        event_mask: x::EventMask::NO_EVENT,
                        event: &close,
                    })
                    .unwrap();
                container.service().unwrap();
                assert!(container.closed_by_user().unwrap());
                assert!(container.is_live().unwrap());
                assert_eq!(
                    container.x11_window_id().unwrap(),
                    parent.window.resource_id()
                );
                container.set_visible(false).unwrap();
            }));
            let retired = container.destroy();
            if let Err(panic) = result {
                std::panic::resume_unwind(panic);
            }
            retired.unwrap();
            assert!(!container.is_live().unwrap());
            assert!(!container.closed_by_user().unwrap());
            assert!(container.x11_window_id().is_err());
            container.destroy().unwrap();
        }
    }
}
