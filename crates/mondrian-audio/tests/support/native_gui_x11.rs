//! Read-only native GUI evidence and ordinary window-manager requests for the lab.

use xcb::{x, Xid};

pub struct Probe {
    connection: xcb::Connection,
    root: x::Window,
    title: x::Atom,
    utf8: x::Atom,
    protocols: x::Atom,
    delete: x::Atom,
}

impl Probe {
    pub fn connect() -> Self {
        let (connection, screen) = xcb::Connection::connect(None).expect("explicit X11 display");
        let root = connection.get_setup().roots().nth(screen as usize).unwrap().root();
        let atom = |name: &[u8]| {
            connection
                .wait_for_reply(
                    connection.send_request(&x::InternAtom { only_if_exists: false, name }),
                )
                .unwrap()
                .atom()
        };
        let title = atom(b"_NET_WM_NAME");
        let utf8 = atom(b"UTF8_STRING");
        let protocols = atom(b"WM_PROTOCOLS");
        let delete = atom(b"WM_DELETE_WINDOW");
        Self { connection, root, title, utf8, protocols, delete }
    }

    pub fn find(&self, title: &str) -> Option<x::Window> {
        self.connection
            .wait_for_reply(self.connection.send_request(&x::QueryTree { window: self.root }))
            .unwrap()
            .children()
            .iter()
            .copied()
            .find(|window| {
                let property =
                    self.connection.wait_for_reply(self.connection.send_request(&x::GetProperty {
                        delete: false,
                        window: *window,
                        property: self.title,
                        r#type: self.utf8,
                        long_offset: 0,
                        long_length: 1024,
                    }));
                match property {
                    Ok(reply) => reply.format() == 8 && reply.value::<u8>() == title.as_bytes(),
                    Err(error) => {
                        // Confirm the close race through a fresh server reply;
                        // a failed connection is never evidence of retirement.
                        let current = self
                            .connection
                            .wait_for_reply(
                                self.connection.send_request(&x::QueryTree { window: self.root }),
                            )
                            .expect("working X11 server after property race");
                        assert!(
                            !current.children().contains(window),
                            "live window property request failed: {error}"
                        );
                        false
                    }
                }
            })
    }

    pub fn size(&self, window: x::Window) -> (u32, u32) {
        let geometry = self
            .connection
            .wait_for_reply(
                self.connection
                    .send_request(&x::GetGeometry { drawable: x::Drawable::Window(window) }),
            )
            .expect("live native parent geometry");
        (u32::from(geometry.width()), u32::from(geometry.height()))
    }

    pub fn describe_windows(&self) -> Vec<String> {
        let mut pending = vec![self.root];
        let mut descriptions = Vec::new();
        while let Some(window) = pending.pop() {
            assert!(descriptions.len() < 128, "bounded native lab window tree");
            let tree = self
                .connection
                .wait_for_reply(self.connection.send_request(&x::QueryTree { window }))
                .unwrap();
            let attributes = self
                .connection
                .wait_for_reply(self.connection.send_request(&x::GetWindowAttributes { window }))
                .unwrap();
            descriptions.push(format!(
                "{:#x}: {:?}, {:?}, children {:?}",
                window.resource_id(),
                self.size(window),
                attributes.map_state(),
                tree.children()
            ));
            pending.extend_from_slice(tree.children());
        }
        descriptions
    }

    pub fn foreign_view_is_painted(&self, window: x::Window) -> bool {
        let tree = self
            .connection
            .wait_for_reply(self.connection.send_request(&x::QueryTree { window }))
            .expect("live actual parent");
        if tree.children().is_empty() {
            return false;
        }
        let attributes = self
            .connection
            .wait_for_reply(self.connection.send_request(&x::GetWindowAttributes { window }))
            .unwrap();
        if attributes.map_state() != x::MapState::Viewable {
            return false;
        }
        let (width, height) = self.size(window);
        // Bound diagnostic readback; this never changes the plugin's viewport.
        assert!(u64::from(width) * u64::from(height) <= 4 * 1024 * 1024);
        let image = self
            .connection
            .wait_for_reply(self.connection.send_request(&x::GetImage {
                format: x::ImageFormat::ZPixmap,
                drawable: x::Drawable::Window(window),
                x: 0,
                y: 0,
                width: width.try_into().unwrap(),
                height: height.try_into().unwrap(),
                plane_mask: u32::MAX,
            }))
            .expect("native client readback");
        let setup = self.connection.get_setup();
        let format = setup.pixmap_formats().iter().find(|f| f.depth() == image.depth()).unwrap();
        let bits = usize::from(format.bits_per_pixel());
        assert!(bits > 0 && bits <= 32 && bits.is_multiple_of(8));
        let pad = usize::from(format.scanline_pad());
        assert!(pad > 0 && pad.is_multiple_of(8) && usize::from(image.depth()) <= bits);
        let stride = (width as usize * bits).div_ceil(pad) * pad / 8;
        assert_eq!(image.data().len(), stride * height as usize);
        let little = setup.image_byte_order() == x::ImageOrder::LsbFirst;
        let mask = (1_u64 << image.depth()) - 1;
        let mut first = None;
        for row in image.data().chunks_exact(stride) {
            for pixel in row[..width as usize * bits / 8].chunks_exact(bits / 8) {
                let mut value = 0_u64;
                for (index, byte) in pixel.iter().enumerate() {
                    let shift = if little {
                        index
                    } else {
                        pixel.len() - index - 1
                    };
                    value |= u64::from(*byte) << (shift * 8);
                }
                let value = value & mask;
                if *first.get_or_insert(value) != value {
                    return true;
                }
            }
        }
        false
    }

    pub fn request_resize(&self, window: x::Window, width: u32, height: u32) {
        self.connection
            .send_and_check_request(&x::ConfigureWindow {
                window,
                value_list: &[
                    x::ConfigWindow::Width(width),
                    x::ConfigWindow::Height(height),
                ],
            })
            .unwrap();
    }

    pub fn request_close(&self, window: x::Window) {
        let event = x::ClientMessageEvent::new(
            window,
            self.protocols,
            x::ClientMessageData::Data32([self.delete.resource_id(), 0, 0, 0, 0]),
        );
        self.connection
            .send_and_check_request(&x::SendEvent {
                propagate: false,
                destination: x::SendEventDest::Window(window),
                event_mask: x::EventMask::NO_EVENT,
                event: &event,
            })
            .unwrap();
    }
}
