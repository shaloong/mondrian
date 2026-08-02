//! Observation-only application event broadcast.
//!
//! This synchronous bus carries low-frequency facts after their owning
//! transaction or execution boundary has committed. It is not a request
//! transport, state authority, realtime queue, or Undo/Redo implementation.

use crate::types::*;
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use parking_lot::RwLock;
use std::sync::Arc;

// ─── 应用事件枚举 ─────────────────────────────────────────────────────────────

const SUBSCRIBER_CAPACITY: usize = 256;

/// Low-frequency facts that independent application observers may consume.
///
/// A variant must describe something that already happened. Commands belong in
/// typed request interfaces such as the editor `Action` adapter, never in this
/// enum.
#[derive(Debug, Clone)]
pub enum AppEvent {
    // ── 时间线编辑 ────────────────────────────────────────────────────────────
    /// One committed edit changed a Sequence.
    TimelineModified {
        /// Sequence whose committed author state changed.
        sequence_id: SequenceId,
    },
    /// One Clip was committed into a Sequence.
    ClipAdded {
        /// Sequence that now owns the Clip.
        sequence_id: SequenceId,
        /// Newly committed Clip.
        clip_id: ClipId,
    },
    /// One Clip was removed by a committed Sequence edit.
    ClipRemoved {
        /// Sequence from which the Clip was removed.
        sequence_id: SequenceId,
        /// Removed Clip identity.
        clip_id: ClipId,
    },

    // ── 素材库 ────────────────────────────────────────────────────────────────
    /// One Asset became an ordinary member of the Project Asset Library.
    AssetImported {
        /// Imported Asset identity.
        asset_id: AssetId,
    },
    /// Ordinary Asset Library membership was retired while the strong Project
    /// record and every author reference remain valid.
    AssetRetired {
        /// Retired Asset identity.
        asset_id: AssetId,
    },
    /// The owning Asset Library published a new committed view.
    AssetLibraryReloaded,

    // ── AI 工作流 ─────────────────────────────────────────────────────────────
    /// An admitted AI workflow started executing.
    WorkflowStarted {
        /// Stable user-facing workflow name.
        workflow_name: String,
    },
    /// One admitted workflow step started executing.
    WorkflowStepStarted {
        /// Workflow-local step identity.
        step_id: String,
        /// Stable user-facing step name.
        step_name: String,
    },
    /// One workflow step completed successfully.
    WorkflowStepCompleted {
        /// Workflow-local step identity.
        step_id: String,
    },
    /// One workflow step terminated with an explicit failure.
    WorkflowStepFailed {
        /// Workflow-local step identity.
        step_id: String,
        /// Diagnostic failure text; this is observation, not control flow.
        error: String,
    },
    /// Every admitted step in an AI workflow completed successfully.
    WorkflowCompleted {
        /// Stable user-facing workflow name.
        workflow_name: String,
    },
}

// ─── 事件总线 ─────────────────────────────────────────────────────────────────

type Subscriber = Sender<AppEvent>;

/// Lightweight observation-only broadcast bus.
///
/// Subscribers receive facts independently through bounded queues. Delivery is
/// intentionally best-effort: a disconnected observer is removed, a saturated
/// observer drops the new notification, and no producer may depend on a
/// subscriber to execute work or acknowledge success. Observers must reconcile
/// from the owning typed state when they need an exact current view.
#[derive(Default)]
pub struct EventBus {
    subscribers: RwLock<Vec<Subscriber>>,
}

impl EventBus {
    /// Create an empty application-fact bus.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Subscribe to subsequently published facts.
    pub fn subscribe(&self) -> Receiver<AppEvent> {
        let (tx, rx) = bounded(SUBSCRIBER_CAPACITY);
        self.subscribers.write().push(tx);
        rx
    }

    /// Non-blockingly broadcast one committed fact.
    ///
    /// Saturated observers remain subscribed but must reconcile from authority;
    /// disconnected observers are retired.
    pub fn publish(&self, event: AppEvent) {
        let mut subs = self.subscribers.write();
        subs.retain(|subscriber| match subscriber.try_send(event.clone()) {
            Ok(()) | Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        });
    }

    /// Return the number of currently connected observers.
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.read().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_bus_broadcast() {
        let bus = EventBus::new();
        let rx1 = bus.subscribe();
        let rx2 = bus.subscribe();

        bus.publish(AppEvent::AssetLibraryReloaded);

        assert!(matches!(
            rx1.try_recv().unwrap(),
            AppEvent::AssetLibraryReloaded
        ));
        assert!(matches!(
            rx2.try_recv().unwrap(),
            AppEvent::AssetLibraryReloaded
        ));
    }

    #[test]
    fn event_bus_cleanup_disconnected() {
        let bus = EventBus::new();
        {
            let _rx = bus.subscribe(); // 离开作用域后 rx 被 drop
        }
        // Broadcasting retires disconnected observers.
        bus.publish(AppEvent::AssetLibraryReloaded);
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[test]
    fn saturated_observer_never_blocks_or_allocates_an_unbounded_backlog() {
        let bus = EventBus::new();
        let rx = bus.subscribe();

        for _ in 0..(SUBSCRIBER_CAPACITY * 2) {
            bus.publish(AppEvent::AssetLibraryReloaded);
        }

        assert_eq!(rx.len(), SUBSCRIBER_CAPACITY);
        assert_eq!(bus.subscriber_count(), 1);
    }
}
