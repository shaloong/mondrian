//! Capacity-one native drawable acquisition, independent of the UI clock.
//!
//! This owner has no frame, Timeline, Preview or color authority. It transfers
//! a drawable only for the latest surface configuration and excludes native
//! configure/acquire while the caller owns the previous drawable. Configuration
//! is applied on the caller's queue-submission thread, never by this worker.

use crate::app::owned_worker_lifecycle::OwnedWorkerShutdown;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SurfaceAcquisitionShutdown {
    pub(crate) surface_generation: u64,
    pub(crate) worker_shutdown: OwnedWorkerShutdown,
    pub(crate) ready_outputs: usize,
    pub(crate) in_use_outputs: usize,
    pub(crate) acquisition_in_flight: bool,
    pub(crate) wake_failures: u64,
}

impl SurfaceAcquisitionShutdown {
    pub(crate) const fn not_started(surface_generation: u64) -> Self {
        Self {
            surface_generation,
            worker_shutdown: OwnedWorkerShutdown::NotStarted,
            ready_outputs: 0,
            in_use_outputs: 0,
            acquisition_in_flight: false,
            wake_failures: 0,
        }
    }

    pub(crate) fn qualifies(&self) -> bool {
        self.surface_generation != 0
            && matches!(
                self.worker_shutdown,
                OwnedWorkerShutdown::Terminated | OwnedWorkerShutdown::NotStarted
            )
            && self.ready_outputs == 0
            && self.in_use_outputs == 0
            && !self.acquisition_in_flight
            && self.wake_failures == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SurfaceAcquisitionsShutdown {
    pub(crate) active: SurfaceAcquisitionShutdown,
    pub(crate) retired: Vec<SurfaceAcquisitionShutdown>,
}

impl SurfaceAcquisitionsShutdown {
    pub(crate) fn qualifies_for(&self, generation: u64, require_active_worker: bool) -> bool {
        self.active.surface_generation == generation
            && self.active.qualifies()
            && (!require_active_worker
                || self.active.worker_shutdown == OwnedWorkerShutdown::Terminated)
            && self.retired.iter().enumerate().all(|(index, receipt)| {
                receipt.qualifies()
                    && receipt.surface_generation != generation
                    && !self.retired[..index]
                        .iter()
                        .any(|previous| previous.surface_generation == receipt.surface_generation)
            })
    }
}

struct State<C, T> {
    configuration: C,
    revision: u64,
    applied_revision: u64,
    requested: bool,
    acquiring: bool,
    in_use: bool,
    ready: Option<T>,
    closed: bool,
    wake_failures: u64,
}

struct Shared<C, T> {
    state: Mutex<State<C, T>>,
    changed: Condvar,
}

impl<C, T> Shared<C, T> {
    fn lock(&self) -> MutexGuard<'_, State<C, T>> {
        self.state.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn close(&self) {
        let ready = {
            let mut state = self.lock();
            state.closed = true;
            state.requested = false;
            state.ready.take()
        };
        // A native drawable destructor must never run with the protocol lock.
        drop(ready);
        self.changed.notify_all();
    }
}

struct WorkerExitHint<'a, C, T> {
    shared: &'a Shared<C, T>,
    wake: &'a dyn Fn() -> bool,
}

impl<C, T> Drop for WorkerExitHint<'_, C, T> {
    fn drop(&mut self) {
        if !self.shared.lock().closed {
            // An unexpected return/unwind must wake an idle Window too.
            // The real join result remains the failure authority.
            self.shared.close();
            if !std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.wake)()))
                .unwrap_or(false)
            {
                self.shared.lock().wake_failures += 1;
            }
        }
    }
}

/// The drawable is released before another configure/acquire may begin.
struct Acquired<C, T> {
    output: Option<T>,
    shared: Arc<Shared<C, T>>,
}

impl<C, T> Acquired<C, T> {
    fn consume<R>(mut self, paint: impl FnOnce(T) -> R) -> R {
        // The guard remains alive across recording, submission and present,
        // including unwinding. Only the caller's completed paint releases it.
        paint(self.output.take().expect("owned surface output"))
    }
}

impl<C, T> Drop for Acquired<C, T> {
    fn drop(&mut self) {
        drop(self.output.take());
        self.shared.lock().in_use = false;
        self.shared.changed.notify_all();
    }
}

struct Acquisition<C, T> {
    shared: Arc<Shared<C, T>>,
    worker: Option<JoinHandle<()>>,
    shutdown: Option<SurfaceAcquisitionShutdown>,
    surface_generation: u64,
}

impl<C: Clone + Send + 'static, T: Send + 'static> Acquisition<C, T> {
    fn start(
        surface_generation: u64,
        configuration: C,
        mut acquire: impl FnMut(&C, bool) -> T + Send + 'static,
        wake: impl Fn() -> bool + Send + 'static,
    ) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                configuration,
                revision: 0,
                applied_revision: 0,
                requested: false,
                acquiring: false,
                in_use: false,
                ready: None,
                closed: false,
                wake_failures: 0,
            }),
            changed: Condvar::new(),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new().name("mondrian-surface".into()).spawn(move || {
            let _exit_hint = WorkerExitHint { shared: &worker_shared, wake: &wake };
            // Initial native configuration was applied during Window creation.
            let mut applied_revision = 0;
            loop {
                let (configuration, revision) = {
                    let mut state = worker_shared.lock();
                    while !state.closed
                        && (!state.requested
                            || state.in_use
                            || state.ready.is_some()
                            || state.revision != state.applied_revision)
                    {
                        state = worker_shared
                            .changed
                            .wait(state)
                            .unwrap_or_else(|poison| poison.into_inner());
                    }
                    if state.closed {
                        break;
                    }
                    state.requested = false;
                    state.acquiring = true;
                    (state.configuration.clone(), state.revision)
                };
                // The potentially blocking native call owns no UI/protocol lock.
                let started = Instant::now();
                let output = acquire(&configuration, revision != applied_revision);
                applied_revision = revision;
                if started.elapsed() >= Duration::from_millis(5) {
                    tracing::debug!(
                        elapsed_us = started.elapsed().as_micros(),
                        revision,
                        "slow native surface acquisition"
                    );
                }
                let discarded = {
                    let mut state = worker_shared.lock();
                    if state.closed || state.revision != revision {
                        Some(output)
                    } else {
                        state.acquiring = false;
                        debug_assert!(state.ready.is_none());
                        state.ready = Some(output);
                        None
                    }
                };
                if discarded.is_some() {
                    drop(discarded);
                    // Destruction of the obsolete native drawable is part of
                    // acquisition. Only now may the caller reconfigure.
                    worker_shared.lock().acquiring = false;
                    if worker_shared.lock().closed {
                        continue;
                    }
                }
                if !wake() {
                    worker_shared.lock().wake_failures += 1;
                    worker_shared.close();
                }
            }
        })?;
        Ok(Self {
            shared,
            worker: Some(worker),
            shutdown: None,
            surface_generation,
        })
    }

    fn health(&self) -> Result<(), &'static str> {
        if self.shared.lock().closed || self.worker.as_ref().is_none_or(JoinHandle::is_finished) {
            Err("native surface acquisition worker is closed or exited")
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    fn poll(&self) -> Result<Option<Acquired<C, T>>, &'static str> {
        self.poll_with_configuration(|_| {})
    }

    fn poll_with_configuration(
        &self,
        configure: impl FnOnce(&C),
    ) -> Result<Option<Acquired<C, T>>, &'static str> {
        self.health()?;
        let configuration = {
            let mut state = self.shared.lock();
            if state.closed {
                return Err("native surface owner closed before configuration");
            }
            if state.revision != state.applied_revision
                && !state.acquiring
                && !state.in_use
                && state.ready.is_none()
            {
                state.in_use = true;
                Some((state.configuration.clone(), state.revision))
            } else {
                None
            }
        };
        if let Some((configuration, revision)) = configuration {
            // The same exclusion guard spans native configuration, including
            // unwind. Queue submission and configure remain on the caller's
            // thread; the worker can only wait for the next drawable.
            let exclusion = Acquired::<C, T> { output: None, shared: Arc::clone(&self.shared) };
            configure(&configuration);
            self.shared.lock().applied_revision = revision;
            drop(exclusion);
        }
        let mut state = self.shared.lock();
        if state.closed {
            return Err("native surface acquisition owner closed before drawable transfer");
        }
        if let Some(output) = state.ready.take() {
            debug_assert!(!state.in_use && !state.acquiring);
            state.in_use = true;
            return Ok(Some(Acquired {
                output: Some(output),
                shared: Arc::clone(&self.shared),
            }));
        }
        if !state.acquiring && !state.in_use {
            state.requested = true;
            self.shared.changed.notify_one();
        }
        Ok(None)
    }

    fn configure(&self, configuration: C) -> Result<(), &'static str> {
        let stale = {
            let mut state = self.shared.lock();
            if state.closed {
                return Err("cannot configure a closed surface acquisition owner");
            }
            state.revision = state
                .revision
                .checked_add(1)
                .ok_or("surface configuration revision exhausted")?;
            state.configuration = configuration;
            state.requested = true;
            let stale = state.ready.take();
            if stale.is_some() {
                // Even a spurious wake must not configure the native surface
                // before its old ready drawable's destructor has returned.
                debug_assert!(!state.in_use);
                state.in_use = true;
            }
            stale
        };
        if stale.is_some() {
            drop(stale);
            self.shared.lock().in_use = false;
        }
        self.shared.changed.notify_one();
        Ok(())
    }

    fn is_pending(&self) -> bool {
        let state = self.shared.lock();
        state.requested || state.acquiring || state.in_use
    }

    fn close_admission(&self) {
        self.shared.close();
    }

    fn try_finish(&mut self) -> Option<SurfaceAcquisitionShutdown> {
        if self.shutdown.is_some() || self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            Some(self.shutdown_until(Instant::now()))
        } else {
            None
        }
    }

    fn shutdown_until(&mut self, deadline: Instant) -> SurfaceAcquisitionShutdown {
        if let Some(receipt) = &self.shutdown {
            return receipt.clone();
        }
        self.close_admission();
        let worker_shutdown =
            self.worker.take().map_or(OwnedWorkerShutdown::NotStarted, |worker| {
                OwnedWorkerShutdown::join_until(worker, deadline)
            });
        let state = self.shared.lock();
        let receipt = SurfaceAcquisitionShutdown {
            surface_generation: self.surface_generation,
            worker_shutdown,
            ready_outputs: usize::from(state.ready.is_some()),
            in_use_outputs: usize::from(state.in_use),
            acquisition_in_flight: state.acquiring,
            wake_failures: state.wake_failures,
        };
        self.shutdown = Some(receipt.clone());
        receipt
    }
}

impl<C, T> Drop for Acquisition<C, T> {
    fn drop(&mut self) {
        // Fallback only. A detached native call retains its own real surface;
        // normal closure must consume the thread and retain its raw receipt.
        self.shared.close();
    }
}

pub(super) struct SurfaceAcquisition {
    inner: Acquisition<wgpu::SurfaceConfiguration, wgpu::CurrentSurfaceTexture>,
    surface: Arc<wgpu::Surface<'static>>,
    device: wgpu::Device,
}

pub(super) struct SurfaceFrame {
    inner: Acquired<wgpu::SurfaceConfiguration, wgpu::CurrentSurfaceTexture>,
}

impl SurfaceFrame {
    pub(super) fn paint(
        self,
        paint: impl FnOnce(wgpu::CurrentSurfaceTexture) -> super::rendering::AppUiFrameResult,
    ) -> super::rendering::AppUiFrameResult {
        self.inner.consume(paint)
    }
}

impl SurfaceAcquisition {
    pub(super) fn start(
        surface_generation: u64,
        surface: Arc<wgpu::Surface<'static>>,
        device: wgpu::Device,
        configuration: wgpu::SurfaceConfiguration,
        wake: impl Fn() -> bool + Send + 'static,
    ) -> std::io::Result<Self> {
        let acquired_surface = Arc::clone(&surface);
        Ok(Self {
            inner: Acquisition::start(
                surface_generation,
                configuration,
                move |_, _| acquired_surface.get_current_texture(),
                wake,
            )?,
            surface,
            device,
        })
    }

    pub(super) fn poll(&self) -> Result<Option<SurfaceFrame>, &'static str> {
        self.inner
            .poll_with_configuration(|configuration| {
                self.surface.configure(&self.device, configuration);
            })
            .map(|frame| frame.map(|inner| SurfaceFrame { inner }))
    }

    pub(super) fn configure(
        &self,
        configuration: wgpu::SurfaceConfiguration,
    ) -> Result<(), &'static str> {
        self.inner.configure(configuration)
    }

    pub(super) fn health(&self) -> Result<(), &'static str> {
        self.inner.health()
    }
    pub(super) fn is_pending(&self) -> bool {
        self.inner.is_pending()
    }
    pub(super) fn close_admission(&self) {
        self.inner.close_admission();
    }
    pub(super) fn try_finish(&mut self) -> Option<SurfaceAcquisitionShutdown> {
        self.inner.try_finish()
    }
    pub(super) fn shutdown_until(&mut self, deadline: Instant) -> SurfaceAcquisitionShutdown {
        self.inner.shutdown_until(deadline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn wait_frame<C: Clone + Send + 'static, T: Send + 'static>(
        owner: &Acquisition<C, T>,
    ) -> Acquired<C, T> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(frame) = owner.poll().expect("live owner") {
                return frame;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    }

    #[test]
    fn blocked_acquisition_does_not_block_poll_or_publish_old_configuration() {
        let (entered, rx) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let mut first = true;
        let mut owner = Acquisition::start(
            1,
            10,
            move |config, changed| {
                if first {
                    first = false;
                    entered.send(()).unwrap();
                    wait.recv().unwrap();
                }
                (*config, changed)
            },
            || true,
        )
        .unwrap();
        assert!(owner.poll().unwrap().is_none());
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        for _ in 0..100 {
            assert!(owner.poll().unwrap().is_none());
        }
        owner.configure(20).unwrap();
        release.send(()).unwrap();
        assert_eq!(wait_frame(&owner).consume(|output| output), (20, true));
        assert!(owner.shutdown_until(Instant::now() + Duration::from_secs(2)).qualifies());
    }

    #[test]
    fn paint_guard_excludes_reconfigure_until_native_output_is_released() {
        let (configured, rx) = mpsc::channel();
        let mut owner = Acquisition::start(
            2,
            10,
            move |config, changed| {
                if changed {
                    configured.send(*config).unwrap();
                }
                *config
            },
            || true,
        )
        .unwrap();
        let frame = wait_frame(&owner);
        owner.configure(20).unwrap();
        assert!(rx.try_recv().is_err());
        frame.consume(|output| {
            assert_eq!(output, 10);
            assert!(rx.try_recv().is_err());
        });
        assert_eq!(wait_frame(&owner).consume(|output| output), 20);
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), 20);
        assert!(owner.shutdown_until(Instant::now() + Duration::from_secs(2)).qualifies());
    }

    struct DropProbe {
        value: u32,
        gate: Option<DropGate>,
    }

    #[test]
    fn native_configuration_runs_on_caller_before_worker_acquires_new_revision() {
        let (acquired, rx) = mpsc::channel();
        let mut owner = Acquisition::start(
            7,
            10,
            move |config, _| {
                acquired.send((thread::current().id(), *config)).unwrap();
                *config
            },
            || true,
        )
        .unwrap();
        assert_eq!(wait_frame(&owner).consume(|output| output), 10);
        let (native_worker, _) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        owner.configure(20).unwrap();
        let caller = thread::current().id();
        drop(
            owner
                .poll_with_configuration(|configuration| {
                    assert_eq!(*configuration, 20);
                    assert_eq!(thread::current().id(), caller);
                    assert_ne!(caller, native_worker);
                    owner.shared.changed.notify_all();
                    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
                })
                .unwrap(),
        );
        assert_eq!(wait_frame(&owner).consume(|output| output), 20);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            (native_worker, 20)
        );
        assert!(owner.shutdown_until(Instant::now() + Duration::from_secs(2)).qualifies());
    }

    #[test]
    fn obsolete_inflight_drawable_destructor_excludes_caller_configuration() {
        let (entered, entered_rx) = mpsc::channel();
        let (acquire_release, acquire_wait) = mpsc::channel();
        let (drop_entered, drop_rx) = mpsc::channel();
        let (drop_release, drop_wait) = mpsc::channel();
        let drop_wait = Arc::new(Mutex::new(drop_wait));
        let mut owner = Acquisition::start(
            8,
            10,
            move |config, _| {
                if *config == 10 {
                    entered.send(()).unwrap();
                    acquire_wait.recv().unwrap();
                }
                DropProbe {
                    value: *config,
                    gate: (*config == 10).then(|| DropGate {
                        entered: drop_entered.clone(),
                        release: Arc::clone(&drop_wait),
                    }),
                }
            },
            || true,
        )
        .unwrap();
        assert!(owner.poll().unwrap().is_none());
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        owner.configure(20).unwrap();
        acquire_release.send(()).unwrap();
        drop_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        for _ in 0..100 {
            assert!(owner
                .poll_with_configuration(|_| panic!("old native drawable still alive"))
                .unwrap()
                .is_none());
        }
        drop_release.send(()).unwrap();
        assert_eq!(wait_frame(&owner).consume(|output| output.value), 20);
        assert!(owner.shutdown_until(Instant::now() + Duration::from_secs(2)).qualifies());
    }

    struct DropGate {
        entered: mpsc::Sender<()>,
        release: Arc<Mutex<mpsc::Receiver<()>>>,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            if let Some(gate) = self.gate.take() {
                gate.entered.send(()).unwrap();
                gate.release.lock().unwrap().recv().unwrap();
            }
        }
    }

    #[test]
    fn stale_ready_drawable_is_destroyed_before_configure_can_begin() {
        let (ready, ready_rx) = mpsc::channel();
        let (drop_entered, drop_rx) = mpsc::channel();
        let (drop_release, release_rx) = mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let (configured, configured_rx) = mpsc::channel();
        let owner = Arc::new(
            Acquisition::start(
                5,
                10,
                move |config, changed| {
                    if changed {
                        configured.send(()).unwrap();
                    }
                    DropProbe {
                        value: *config,
                        gate: (*config == 10).then(|| DropGate {
                            entered: drop_entered.clone(),
                            release: Arc::clone(&release_rx),
                        }),
                    }
                },
                move || ready.send(()).is_ok(),
            )
            .unwrap(),
        );
        assert!(owner.poll().unwrap().is_none());
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let configuring = Arc::clone(&owner);
        let worker = thread::spawn(move || configuring.configure(20).unwrap());
        drop_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        // Condvars allow spurious wakeups; the predicate must protect native
        // destruction, not depend on the timing of the explicit notification.
        owner.shared.changed.notify_all();
        let configured_before_drop = configured_rx.recv_timeout(Duration::from_millis(100)).is_ok();
        drop_release.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(wait_frame(&owner).consume(|output| output.value), 20);
        let mut owner = Arc::try_unwrap(owner).ok().expect("sole owner");
        assert!(owner.shutdown_until(Instant::now() + Duration::from_secs(2)).qualifies());
        assert!(
            !configured_before_drop,
            "configure raced the old live drawable destructor"
        );
    }

    #[test]
    fn real_blocked_worker_cannot_be_reported_terminated_at_expired_deadline() {
        let (entered, rx) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let (returned, done) = mpsc::channel();
        let mut owner = Acquisition::start(
            3,
            (),
            move |_, _| {
                entered.send(()).unwrap();
                wait.recv().unwrap();
                returned.send(()).unwrap();
            },
            || true,
        )
        .unwrap();
        assert!(owner.poll().unwrap().is_none());
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let receipt = owner.shutdown_until(Instant::now());
        assert_eq!(
            receipt.worker_shutdown,
            OwnedWorkerShutdown::TimedOutDetached
        );
        assert!(!receipt.qualifies());
        assert_eq!(
            owner.shutdown_until(Instant::now() + Duration::from_secs(2)),
            receipt
        );
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn acquire_panic_wakes_idle_window_and_cannot_qualify_as_closed() {
        let (called, rx) = mpsc::channel();
        let mut owner = Acquisition::<(), ()>::start(
            6,
            (),
            |_, _| {
                panic!("injected native acquisition panic");
            },
            move || called.send(()).is_ok(),
        )
        .unwrap();
        assert!(owner.poll().unwrap().is_none());
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(owner.poll().is_err());
        let receipt = owner.shutdown_until(Instant::now() + Duration::from_secs(2));
        assert_eq!(receipt.worker_shutdown, OwnedWorkerShutdown::Panicked);
        assert!(!receipt.qualifies());
    }

    #[test]
    fn rejected_wake_closes_owner_and_remains_failed_in_consuming_receipt() {
        let (called, rx) = mpsc::channel();
        let mut owner = Acquisition::start(
            4,
            (),
            |_, _| (),
            move || {
                called.send(()).unwrap();
                false
            },
        )
        .unwrap();
        assert!(owner.poll().unwrap().is_none());
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let receipt = owner.shutdown_until(Instant::now() + Duration::from_secs(2));
        assert_eq!(receipt.worker_shutdown, OwnedWorkerShutdown::Terminated);
        assert_eq!(receipt.wake_failures, 1);
        assert!(!receipt.qualifies());
    }
}
