//! Fixed thread-safe CLAP GUI requests, owned by one live host wrapper.
//!
//! A GUI incarnation is a lifecycle observation, never a Core installation or
//! author permission. The main owner issues a new incarnation for every create
//! attempt and revokes it before native destruction. Requests admitted to an
//! older incarnation cannot replace a newer tagged request. CLAP callbacks carry
//! no GUI token: a plugin must settle its old callbacks before a new create.
//! No callback waits, allocates or calls native window APIs. Plugin timers have
//! their own registration lifetime and are never guessed to belong to a GUI.

use clack_extensions::gui::GuiSize;
use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const REQUEST_ATTEMPTS: usize = 16;
const STATE_BITS: u32 = 2;
const MAX_SERIAL: u64 = u64::MAX >> STATE_BITS;
const BUSY: u64 = 1;
const READY: u64 = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct GuiIncarnation(u64);

/// Main-only nonwrapping issuer within this host wrapper's native GUI lifetime.
pub(super) struct GuiIncarnationIssuer {
    next: Cell<u64>,
    owner_thread: PhantomData<Rc<()>>,
}

impl GuiIncarnationIssuer {
    pub(super) const fn new() -> Self {
        Self { next: Cell::new(0), owner_thread: PhantomData }
    }

    pub(super) fn issue(&self) -> Option<GuiIncarnation> {
        let value = self.next.get().checked_add(1).filter(|value| *value <= MAX_SERIAL)?;
        self.next.set(value);
        Some(GuiIncarnation(value))
    }
}

/// The shared owner contains only fixed-width scalar atomics.
pub(super) struct GuiRequests {
    active: AtomicU64,
    embedded: AtomicBool,
    visibility: AtomicU64,
    hints: AtomicU64,
    closed: AtomicU64,
    // State encodes a nonwrapping publication serial plus Idle/Busy/Ready.
    // Idle preserves its last serial, so a consumer cannot suffer Ready ABA.
    resize_state: AtomicU64,
    resize_incarnation: AtomicU64,
    resize_dimensions: AtomicU64,
}

impl GuiRequests {
    pub(super) const fn new() -> Self {
        Self {
            active: AtomicU64::new(0),
            embedded: AtomicBool::new(false),
            visibility: AtomicU64::new(0),
            hints: AtomicU64::new(0),
            closed: AtomicU64::new(0),
            resize_state: AtomicU64::new(0),
            resize_incarnation: AtomicU64::new(0),
            resize_dimensions: AtomicU64::new(0),
        }
    }

    /// Publish after actual GUI create/parent succeeds, before it may be shown.
    pub(super) fn activate(&self, incarnation: GuiIncarnation, embedded: bool) {
        self.embedded.store(embedded, Ordering::Release);
        self.active.store(incarnation.0, Ordering::Release);
    }

    /// Revoke before hide/destroy or parent retirement. Do not reset mailboxes:
    /// a callback already admitted to an old incarnation may publish later.
    pub(super) fn revoke(&self) {
        self.active.store(0, Ordering::Release);
    }

    fn current(&self) -> Option<GuiIncarnation> {
        let value = self.active.load(Ordering::Acquire);
        (value != 0).then_some(GuiIncarnation(value))
    }

    fn embedded_current(&self) -> Option<GuiIncarnation> {
        let current = self.current()?;
        self.embedded.load(Ordering::Acquire).then_some(current)
    }

    // Coalesce one field while preventing an old incarnation from replacing
    // a record already published for a newer GUI. Contention is a legal refusal.
    fn publish_stamped(&self, field: &AtomicU64, current: GuiIncarnation, flags: u64) -> bool {
        let mut previous = field.load(Ordering::Acquire);
        for _ in 0..REQUEST_ATTEMPTS {
            if self.active.load(Ordering::Acquire) != current.0
                || previous >> STATE_BITS > current.0
            {
                return false;
            }
            let next = (current.0 << STATE_BITS) | flags;
            match field.compare_exchange_weak(previous, next, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return true,
                Err(observed) => previous = observed,
            }
        }
        false
    }

    pub(super) fn request_visibility(&self, visible: bool) -> bool {
        let Some(current) = self.current() else {
            return false;
        };
        self.publish_stamped(&self.visibility, current, if visible { 1 } else { 2 })
    }

    pub(super) fn request_hints(&self) {
        if let Some(current) = self.embedded_current() {
            // The ABI has no rejection return. A single atomic coalesces every
            // valid request without dropping it because another producer won.
            self.hints.fetch_max(current.0, Ordering::AcqRel);
        }
    }

    pub(super) fn report_closed(&self, destroyed: bool) {
        if let Some(current) = self.current() {
            // Publish the incarnation and strongest destruction observation in
            // ONE atomic operation. A reader can never interpret closed(true)
            // as closed(false) between separate publications. This terminal
            // fact remains sticky until the incarnation is revoked; no callback
            // allocation, bounded-CAS refusal or clearing ABA is introduced.
            self.closed.fetch_max(
                (current.0 << STATE_BITS) | if destroyed { 2 } else { 1 },
                Ordering::AcqRel,
            );
        }
    }

    pub(super) fn close_requested(&self) -> bool {
        let Some(current) = self.current() else {
            return false;
        };
        self.closed.load(Ordering::Acquire) >> STATE_BITS == current.0
    }

    pub(super) fn request_resize(&self, size: GuiSize) -> bool {
        if !valid_parent_size(&size) {
            return false;
        }
        let Some(current) = self.embedded_current() else {
            return false;
        };
        self.publish_resize(current, size)
    }

    fn publish_resize(&self, current: GuiIncarnation, size: GuiSize) -> bool {
        let mut previous = self.resize_state.load(Ordering::Acquire);
        for _ in 0..REQUEST_ATTEMPTS {
            if previous & 3 == BUSY || self.active.load(Ordering::Acquire) != current.0 {
                return false;
            }
            let Some(serial) =
                (previous >> STATE_BITS).checked_add(1).filter(|serial| *serial <= MAX_SERIAL)
            else {
                return false;
            };
            let busy = (serial << STATE_BITS) | BUSY;
            match self.resize_state.compare_exchange_weak(
                previous,
                busy,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    // No other publisher owns the payload while Busy. All payload
                    // fields are atomic; stale consumers must win the unique
                    // Ready CAS before using the copied payload. A late old GUI
                    // callback remains tagged old, even after a newer create.
                    self.resize_incarnation.store(current.0, Ordering::Relaxed);
                    self.resize_dimensions.store(
                        (u64::from(size.width) << 32) | u64::from(size.height),
                        Ordering::Relaxed,
                    );
                    self.resize_state.store((serial << STATE_BITS) | READY, Ordering::Release);
                    return true;
                }
                Err(observed) => previous = observed,
            }
        }
        false
    }

    fn take_resize(&self, current: GuiIncarnation) -> Option<GuiSize> {
        let ready = self.resize_state.load(Ordering::Acquire);
        if ready & 3 != READY {
            return None;
        }
        let incarnation = self.resize_incarnation.load(Ordering::Relaxed);
        let packed = self.resize_dimensions.load(Ordering::Relaxed);
        if self
            .resize_state
            .compare_exchange(ready, ready & !3, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        (incarnation == current.0).then_some(GuiSize {
            width: (packed >> 32) as u32,
            height: packed as u32,
        })
    }

    fn take_stamped(&self, field: &AtomicU64, current: GuiIncarnation) -> u64 {
        let value = field.swap(0, Ordering::AcqRel);
        if value >> STATE_BITS == current.0 {
            value & 3
        } else {
            0
        }
    }

    /// Main-only consumption. Native calls occur after these atomic borrows end.
    pub(super) fn take(&self) -> GuiIntents {
        let Some(current) = self.current() else {
            return GuiIntents::default();
        };
        GuiIntents {
            visibility: match self.take_stamped(&self.visibility, current) {
                1 => Some(true),
                2 => Some(false),
                _ => None,
            },
            resize: self.take_resize(current),
            hints_changed: self.hints.swap(0, Ordering::AcqRel) == current.0,
            closed: {
                let terminal = self.closed.load(Ordering::Acquire);
                (terminal >> STATE_BITS == current.0).then_some(terminal & 3 == 2)
            },
        }
    }
}

#[derive(Default)]
pub(super) struct GuiIntents {
    pub(super) visibility: Option<bool>,
    pub(super) resize: Option<GuiSize>,
    pub(super) hints_changed: bool,
    /// True requires actual PluginGui.destroy acknowledgement on main.
    pub(super) closed: Option<bool>,
}

/// Refuse known protocol limits before accepting a plugin resize request.
/// X11 dimensions occupy u16 fields; the other native parents accept i32.
pub(super) fn valid_parent_size(size: &GuiSize) -> bool {
    let maximum = if cfg!(target_os = "linux") {
        u32::from(u16::MAX)
    } else {
        i32::MAX as u32
    };
    size.width != 0 && size.height != 0 && size.width <= maximum && size.height <= maximum
}

#[cfg(test)]
mod terminal_observation_contract_tests {
    use super::*;
    #[test]
    fn gui_destroyed_observation_is_atomic_sticky_and_exact_incarnation() {
        let requests = GuiRequests::new();
        let issuer = GuiIncarnationIssuer::new();
        requests.activate(issuer.issue().expect("first incarnation"), true);
        requests.report_closed(true);
        assert_eq!(requests.take().closed, Some(true));
        // A later weaker notification cannot erase actual destruction, and
        // taking other mailboxes cannot consume this lifecycle fact.
        requests.report_closed(false);
        assert_eq!(requests.take().closed, Some(true));
        assert!(requests.close_requested());
        requests.revoke();
        assert_eq!(requests.take().closed, None);
        requests.activate(issuer.issue().expect("new incarnation"), true);
        assert_eq!(requests.take().closed, None);
        assert!(!requests.close_requested());
        requests.report_closed(false);
        assert_eq!(requests.take().closed, Some(false));
        requests.report_closed(true);
        assert_eq!(requests.take().closed, Some(true));
    }

    #[test]
    fn resize_admission_respects_the_actual_native_protocol() {
        let requests = GuiRequests::new();
        requests.activate(GuiIncarnationIssuer::new().issue().unwrap(), true);
        let maximum = if cfg!(target_os = "linux") {
            u32::from(u16::MAX)
        } else {
            i32::MAX as u32
        };
        for size in [
            GuiSize { width: 0, height: 1 },
            GuiSize { width: 1, height: 0 },
            GuiSize { width: maximum + 1, height: 1 },
            GuiSize { width: 1, height: maximum + 1 },
        ] {
            assert!(!requests.request_resize(size));
            assert!(requests.take().resize.is_none());
        }
        let accepted = GuiSize { width: maximum, height: 1 };
        assert!(requests.request_resize(accepted));
        assert_eq!(requests.take().resize, Some(accepted));
        assert!(requests.take().resize.is_none());
    }
}
