//! Linux factory/editor registration scopes and their shared UI-thread dispatcher.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use vst3::{ComPtr, Steinberg::*};

// --- Linux IRunLoop ------------------------------------------------------
// VSTGUI-based editors (and most non-JUCE plugin UIs) strictly require the
// host frame to also implement `Steinberg::Linux::IRunLoop`: the view
// registers file-descriptor event handlers (its X11 connection) and
// periodic timers with the host, and paints/responds ONLY when the host
// services them. Without this the editor attaches but stays black. The host
// must service the same UI-thread dispatcher even without an open editor.

/// One registration scope, owned either by a factory context or by an editor.
/// Both scopes use the same UI-thread dispatcher and cancellation leases.
pub struct RunLoopRegistry {
    pub handlers: Vec<Arc<RunLoopEventRegistration>>,
    pub timers: Vec<RunLoopTimer>,
    closed: bool,
}

// Each registration owns its cancellation state and native reference. A turn
// captures Arc leases, so unregister/re-register cannot revive an old callback,
// and snapshot capture never calls a plugin's AddRef while holding the mutex.
pub struct RunLoopEventRegistration {
    pub handler: ComPtr<vst3::Steinberg::Linux::IEventHandler>,
    pub fd: vst3::Steinberg::Linux::FileDescriptor,
    pub active: AtomicBool,
}

pub struct RunLoopTimerRegistration {
    pub handler: ComPtr<vst3::Steinberg::Linux::ITimerHandler>,
    pub active: AtomicBool,
}

pub struct RunLoopTimer {
    pub registration: Arc<RunLoopTimerRegistration>,
    pub interval_ms: u64,
    pub due: std::time::Instant,
}

impl RunLoopRegistry {
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
            timers: Vec::new(),
            closed: false,
        }
    }

    fn deactivate_all(&self) {
        for entry in &self.handlers {
            entry.active.store(false, Ordering::Release);
        }
        for timer in &self.timers {
            timer.registration.active.store(false, Ordering::Release);
        }
    }

    /// Return the retired native owners for release after unlocking the registry.
    #[must_use]
    pub fn take_retired(&mut self) -> (Vec<Arc<RunLoopEventRegistration>>, Vec<RunLoopTimer>) {
        self.deactivate_all();
        (
            std::mem::take(&mut self.handlers),
            std::mem::take(&mut self.timers),
        )
    }
}

impl Drop for RunLoopRegistry {
    fn drop(&mut self) {
        self.deactivate_all();
    }
}

impl RunLoopRegistry {
    pub(crate) unsafe fn register_event_handler(
        registry: &Mutex<Self>,
        handler: *mut vst3::Steinberg::Linux::IEventHandler,
        fd: vst3::Steinberg::Linux::FileDescriptor,
    ) -> tresult {
        let Some(handler) = vst3::ComRef::from_raw(handler) else {
            return kInvalidArgument;
        };
        if fd < 0 {
            return kInvalidArgument;
        }
        let registration = Arc::new(RunLoopEventRegistration {
            handler: handler.to_com_ptr(),
            fd,
            active: AtomicBool::new(true),
        });
        match registry.lock() {
            Ok(mut reg) if !reg.closed => {
                reg.handlers.push(registration);
                kResultOk
            }
            Ok(reg) => {
                drop(reg);
                drop(registration);
                kResultFalse
            }
            Err(poisoned) => {
                // A tail-expression temporary can otherwise retain this guard
                // until after the unused native registration is released.
                drop(poisoned);
                drop(registration);
                kInternalError
            }
        }
    }

    pub(crate) unsafe fn unregister_event_handler(
        registry: &Mutex<Self>,
        handler: *mut vst3::Steinberg::Linux::IEventHandler,
    ) -> tresult {
        let retired: Vec<_> = match registry.lock() {
            Ok(mut reg) => reg
                .handlers
                .extract_if(.., |entry| {
                    let matches = entry.handler.as_ptr() == handler;
                    if matches {
                        entry.active.store(false, Ordering::Release);
                    }
                    matches
                })
                .collect(),
            Err(_) => return kInternalError,
        };
        // A final native Release may reenter IRunLoop.
        drop(retired);
        kResultOk
    }

    pub(crate) unsafe fn register_timer(
        registry: &Mutex<Self>,
        handler: *mut vst3::Steinberg::Linux::ITimerHandler,
        milliseconds: vst3::Steinberg::Linux::TimerInterval,
    ) -> tresult {
        let Some(handler) = vst3::ComRef::from_raw(handler) else {
            return kInvalidArgument;
        };
        let interval_ms = milliseconds.max(1);
        let Some(due) =
            std::time::Instant::now().checked_add(std::time::Duration::from_millis(interval_ms))
        else {
            return kInvalidArgument;
        };
        let registration = Arc::new(RunLoopTimerRegistration {
            handler: handler.to_com_ptr(),
            active: AtomicBool::new(true),
        });
        match registry.lock() {
            Ok(mut reg) if !reg.closed => {
                reg.timers.push(RunLoopTimer { registration, interval_ms, due });
                kResultOk
            }
            Ok(reg) => {
                drop(reg);
                drop(registration);
                kResultFalse
            }
            Err(poisoned) => {
                drop(poisoned);
                drop(registration);
                kInternalError
            }
        }
    }

    pub(crate) unsafe fn unregister_timer(
        registry: &Mutex<Self>,
        handler: *mut vst3::Steinberg::Linux::ITimerHandler,
    ) -> tresult {
        let retired: Vec<_> = match registry.lock() {
            Ok(mut reg) => reg
                .timers
                .extract_if(.., |timer| {
                    let matches = timer.registration.handler.as_ptr() == handler;
                    if matches {
                        timer.registration.active.store(false, Ordering::Release);
                    }
                    matches
                })
                .collect(),
            Err(_) => return kInternalError,
        };
        drop(retired);
        kResultOk
    }

    /// Final factory retirement rejects reentrant registrations before native Release.
    pub(crate) fn shutdown(registry: &Mutex<Self>) {
        let retired = {
            let mut state = registry.lock().unwrap_or_else(|poison| poison.into_inner());
            state.closed = true;
            state.take_retired()
        };
        drop(retired);
    }
}

pub(crate) fn visit_owned_storage(
    registry: &Arc<Mutex<RunLoopRegistry>>,
    visitor: &mut dyn crate::host_storage::HostStorageVisitor,
) -> Result<(), crate::host_storage::HostStorageError> {
    use crate::host_storage::{HostStorageError, HostStorageRef};
    visitor.visit(HostStorageRef::arc(registry))?;
    let state = registry.lock().map_err(|_| HostStorageError::PoisonedOwner)?;
    visitor.visit(HostStorageRef::vector(&state.handlers)?)?;
    visitor.visit(HostStorageRef::vector(&state.timers)?)?;
    for entry in &state.handlers {
        visitor.visit(HostStorageRef::arc(entry))?;
    }
    for timer in &state.timers {
        visitor.visit(HostStorageRef::arc(&timer.registration))?;
    }
    Ok(())
}

pub(crate) fn service(registry: &Mutex<RunLoopRegistry>) -> crate::Result<()> {
    use std::sync::atomic::Ordering;
    use vst3::Steinberg::Linux::{IEventHandlerTrait, ITimerHandlerTrait};

    // Fire due timers. Snapshot the handlers, then invoke with the lock
    // RELEASED: a callback may re-enter registerTimer/unregisterTimer
    // (VSTGUI does), which takes the same lock.
    let now = std::time::Instant::now();
    let mut due = Vec::new();
    {
        let mut reg = registry
            .lock()
            .map_err(|_| crate::Error::Other("run-loop state is poisoned".to_string()))?;
        for timer in reg.timers.iter_mut() {
            if now >= timer.due {
                timer.due = now
                    .checked_add(std::time::Duration::from_millis(timer.interval_ms))
                    .ok_or_else(|| {
                        crate::Error::Other("run-loop timer deadline overflow".to_string())
                    })?;
                due.push(Arc::clone(&timer.registration));
            }
        }
    }
    for registration in due {
        if registration.active.load(Ordering::Acquire) {
            unsafe { registration.handler.onTimer() };
        }
    }

    // Poll registered fds (zero timeout - never blocks the UI thread)
    // and notify ready ones. Same snapshot-then-invoke pattern.
    let handlers = registry
        .lock()
        .map_err(|_| crate::Error::Other("run-loop state is poisoned".to_string()))?
        .handlers
        .clone();
    if handlers.is_empty() {
        return Ok(());
    }
    let mut fds: Vec<libc::pollfd> = handlers
        .iter()
        .map(|entry| libc::pollfd { fd: entry.fd, events: libc::POLLIN, revents: 0 })
        .collect();
    let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, 0) };
    if ready < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(crate::Error::IoError(error));
        }
    }
    if ready > 0 {
        for (pfd, entry) in fds.iter().zip(handlers.iter()) {
            if pfd.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0
                && entry.active.load(Ordering::Acquire)
            {
                unsafe { entry.handler.onFDIsSet(entry.fd) };
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poisoned_registration_scope_is_reported_instead_of_silently_skipping_callbacks() {
        let registry = Mutex::new(RunLoopRegistry::new());
        let _ = std::panic::catch_unwind(|| {
            let _guard = registry.lock().unwrap();
            panic!("controlled run-loop owner poison");
        });
        assert!(matches!(service(&registry), Err(crate::Error::Other(_))));
        RunLoopRegistry::shutdown(&registry);
    }
}
