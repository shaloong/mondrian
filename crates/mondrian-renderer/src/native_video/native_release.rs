//! Session-owned foreign destruction, never executed under wgpu resource locks.
use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;

#[derive(Debug, thiserror::Error)]
pub(super) enum ReleaseError {
    #[error("native release worker could not start: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("native release admission is closed")]
    Closed,
    #[error("previous native destruction is still pending")]
    Pending,
    #[error("native release worker failed; closure is unproven")]
    Failed,
    #[error("native release owner counter exhausted")]
    Exhausted,
}

#[derive(Default)]
struct State {
    live: AtomicUsize,
    pending: AtomicUsize,
    failed: AtomicBool,
}

type Payload = Box<dyn Send + 'static>;

pub(super) struct NativeReleaseOwner {
    sender: Option<mpsc::Sender<Payload>>,
    worker: Option<JoinHandle<()>>,
    state: Arc<State>,
}

impl NativeReleaseOwner {
    pub(super) fn new() -> Result<Self, ReleaseError> {
        let (sender, receiver) = mpsc::channel::<Payload>();
        let state = Arc::new(State::default());
        let observed = Arc::clone(&state);
        let worker = std::thread::Builder::new()
            .name("native-video-release".to_owned())
            .spawn(move || {
                while let Ok(payload) = receiver.recv() {
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(payload)))
                        .is_err()
                    {
                        observed.failed.store(true, Ordering::Release);
                    } else {
                        observed.pending.fetch_sub(1, Ordering::AcqRel);
                        observed.live.fetch_sub(1, Ordering::AcqRel);
                    }
                }
            })
            .map_err(ReleaseError::Spawn)?;
        Ok(Self { sender: Some(sender), worker: Some(worker), state })
    }

    pub(super) fn admit(&self) -> Result<ReleaseAdmission, ReleaseError> {
        if self.state.failed.load(Ordering::Acquire) {
            return Err(ReleaseError::Failed);
        }
        let sender = self.sender.as_ref().ok_or(ReleaseError::Closed)?;
        // Do not replace an allocation whose destruction is merely queued.
        // Existing frame admission bounds the set of live transfers; this gate
        // prevents the executor from hiding an additional retired allocation set.
        if self.state.pending.load(Ordering::Acquire) != 0 {
            return Err(ReleaseError::Pending);
        }
        self.state
            .live
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .map_err(|_| ReleaseError::Exhausted)?;
        let admission = ReleaseAdmission {
            sender: sender.clone(),
            state: Arc::clone(&self.state),
            transferred: false,
        };
        if self.state.pending.load(Ordering::Acquire) != 0 {
            return Err(ReleaseError::Pending);
        }
        Ok(admission)
    }

    pub(super) fn retained_owners(&self) -> usize {
        self.state.live.load(Ordering::Acquire)
    }

    pub(super) fn poll_retirement(&mut self) -> Result<bool, ReleaseError> {
        self.sender.take();
        if self.worker.as_ref().is_some_and(|worker| !worker.is_finished()) {
            return Ok(false);
        }
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            self.state.failed.store(true, Ordering::Release);
        }
        if self.state.failed.load(Ordering::Acquire) || self.state.live.load(Ordering::Acquire) != 0
        {
            return Err(ReleaseError::Failed);
        }
        Ok(true)
    }
}

impl Drop for NativeReleaseOwner {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            if self.state.live.load(Ordering::Acquire) == 0 || worker.is_finished() {
                if worker.join().is_err() {
                    self.state.failed.store(true, Ordering::Release);
                }
            } else {
                // An abandoned owner is not a successful retirement. Do not
                // block a queue callback or release parents to force closure.
                self.state.failed.store(true, Ordering::Release);
                tracing::error!("native release owner abandoned before consuming retirement");
                std::mem::forget(worker);
            }
        }
    }
}

pub(super) struct ReleaseAdmission {
    sender: mpsc::Sender<Payload>,
    state: Arc<State>,
    transferred: bool,
}
impl ReleaseAdmission {
    pub(super) fn retain<T: Send + 'static>(self, payload: T) -> DeferredNativeOwner<T> {
        DeferredNativeOwner {
            payload: ManuallyDrop::new(payload),
            admission: Some(self),
        }
    }
}
impl Drop for ReleaseAdmission {
    fn drop(&mut self) {
        if !self.transferred {
            self.state.live.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

pub(super) struct DeferredNativeOwner<T: Send + 'static> {
    payload: ManuallyDrop<T>,
    admission: Option<ReleaseAdmission>,
}
impl<T: Send + 'static> Deref for DeferredNativeOwner<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.payload
    }
}
impl<T: Send + 'static> DerefMut for DeferredNativeOwner<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.payload
    }
}
impl<T: Send + 'static> Drop for DeferredNativeOwner<T> {
    fn drop(&mut self) {
        if let Some(mut admission) = self.admission.take() {
            admission.state.pending.fetch_add(1, Ordering::AcqRel);
            // SAFETY: only Drop takes the payload, exactly once. ManuallyDrop
            // prevents automatic foreign destruction on the callback stack.
            let payload = unsafe { ManuallyDrop::take(&mut self.payload) };
            if let Err(error) = admission.sender.send(Box::new(payload)) {
                admission.state.failed.store(true, Ordering::Release);
                // A dead executor gives no native release proof. Preserve the
                // payload and its parents rather than destroy under caller locks.
                std::mem::forget(error.0);
            }
            // Transfer the live count to the worker while closing this sender.
            admission.transferred = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn finish(owner: &mut NativeReleaseOwner) -> Result<bool, ReleaseError> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match owner.poll_retirement() {
                Ok(false) => {
                    assert!(
                        Instant::now() < deadline,
                        "native release worker did not return"
                    );
                    std::thread::yield_now();
                }
                result => return result,
            }
        }
    }

    #[test]
    fn late_release_is_consumed_after_admission_closes() {
        let mut owner = NativeReleaseOwner::new().expect("worker");
        let lease = owner.admit().expect("admission").retain(vec![1_u8; 32]);
        assert!(!owner.poll_retirement().expect("still owns late sender"));
        assert!(matches!(owner.admit(), Err(ReleaseError::Closed)));
        drop(lease);
        assert!(finish(&mut owner).expect("joined"));
        assert!(owner.worker.is_none());
        assert!(owner.poll_retirement().expect("stable closure"));
    }

    #[test]
    fn unused_admission_does_not_prevent_worker_closure() {
        let mut owner = NativeReleaseOwner::new().expect("worker");
        drop(owner.admit().expect("admission"));
        assert_eq!(owner.state.live.load(Ordering::Acquire), 0);
        assert!(finish(&mut owner).expect("joined"));
    }

    #[test]
    fn foreign_panic_cannot_publish_successful_closure() {
        struct Panics;
        impl Drop for Panics {
            fn drop(&mut self) {
                panic!("injected foreign destructor panic");
            }
        }
        let mut owner = NativeReleaseOwner::new().expect("worker");
        drop(owner.admit().expect("admission").retain(Panics));
        assert!(matches!(finish(&mut owner), Err(ReleaseError::Failed)));
        assert!(owner.worker.is_none(), "failed worker must still be joined");
        assert!(matches!(owner.admit(), Err(ReleaseError::Failed)));
        assert_eq!(owner.state.live.load(Ordering::Acquire), 1);
    }
}
