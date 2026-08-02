//! Exact physical phase accounting for one sequential background worker.
//!
//! A domain remains responsible for its own request identity, queue, results,
//! cancellation, and terminal evidence. This small Module only prevents
//! product resource policy from guessing physical execution from lifecycle
//! pending counts.

use std::collections::HashSet;
use std::hash::Hash;
use std::sync::Arc;

use parking_lot::Mutex;

/// Physical phase of the one request currently owned by a sequential worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SingleWorkerPhase {
    /// The worker dequeued the request but is waiting at its dispatch gate.
    WaitingForDispatch,
    /// The worker crossed the dispatch gate and is executing domain work.
    Running,
}

/// Point-in-time physical worker facts.
#[derive(Debug, Clone)]
pub(crate) struct SingleWorkerActivitySnapshot<Identity> {
    /// Exact current physical identity and phase, when the worker owns one.
    pub(crate) current: Option<(Identity, SingleWorkerPhase)>,
    /// Completed identities whose results have not been consumed yet.
    pub(crate) awaiting_publication: Vec<Identity>,
}

struct SingleWorkerActivityState<Identity> {
    current: Option<(Identity, SingleWorkerPhase)>,
    awaiting_publication: HashSet<Identity>,
}

impl<Identity> Default for SingleWorkerActivityState<Identity> {
    fn default() -> Self {
        Self {
            current: None,
            awaiting_publication: HashSet::new(),
        }
    }
}

/// Thread-safe physical activity owner for a single sequential worker.
pub(crate) struct SingleWorkerActivity<Identity> {
    state: Mutex<SingleWorkerActivityState<Identity>>,
}

impl<Identity> Default for SingleWorkerActivity<Identity> {
    fn default() -> Self {
        Self {
            state: Mutex::new(SingleWorkerActivityState::default()),
        }
    }
}

impl<Identity> SingleWorkerActivity<Identity>
where
    Identity: Clone + Eq + Hash,
{
    /// Begin physical ownership while the worker is still at its dispatch gate.
    pub(crate) fn begin(
        self: &Arc<Self>,
        identity: Identity,
    ) -> SingleWorkerActivityLease<Identity> {
        let mut state = self.state.lock();
        debug_assert!(
            state.current.is_none(),
            "a sequential worker cannot own two current requests"
        );
        state.current = Some((identity.clone(), SingleWorkerPhase::WaitingForDispatch));
        drop(state);
        SingleWorkerActivityLease {
            activity: Arc::clone(self),
            identity,
            awaiting_publication: false,
            publication_committed: false,
        }
    }

    /// Acknowledge that the domain owner consumed one worker result.
    pub(crate) fn acknowledge_publication(&self, identity: &Identity) {
        self.state.lock().awaiting_publication.remove(identity);
    }

    /// Snapshot physical phase without exposing domain lifecycle state.
    pub(crate) fn snapshot(&self) -> SingleWorkerActivitySnapshot<Identity> {
        let state = self.state.lock();
        SingleWorkerActivitySnapshot {
            current: state.current.clone(),
            awaiting_publication: state.awaiting_publication.iter().cloned().collect(),
        }
    }

    fn mark_running(&self, identity: &Identity) {
        let mut state = self.state.lock();
        if state.current.as_ref().is_some_and(|(current, _)| current == identity) {
            state.current = Some((identity.clone(), SingleWorkerPhase::Running));
        }
    }

    fn finish_for_publication(&self, identity: &Identity) {
        let mut state = self.state.lock();
        if state.current.as_ref().is_some_and(|(current, _)| current == identity) {
            state.current = None;
        }
        state.awaiting_publication.insert(identity.clone());
    }

    fn abandon(&self, identity: &Identity) {
        let mut state = self.state.lock();
        if state.current.as_ref().is_some_and(|(current, _)| current == identity) {
            state.current = None;
        }
    }
}

/// RAII ownership of one physical worker request.
pub(crate) struct SingleWorkerActivityLease<Identity>
where
    Identity: Clone + Eq + Hash,
{
    activity: Arc<SingleWorkerActivity<Identity>>,
    identity: Identity,
    awaiting_publication: bool,
    publication_committed: bool,
}

impl<Identity> SingleWorkerActivityLease<Identity>
where
    Identity: Clone + Eq + Hash,
{
    /// Record that the request crossed its dispatch gate.
    pub(crate) fn mark_running(&mut self) {
        self.activity.mark_running(&self.identity);
    }

    /// Transfer the physical request into the result-publication channel.
    pub(crate) fn finish_for_publication(&mut self) {
        if self.awaiting_publication {
            return;
        }
        self.activity.finish_for_publication(&self.identity);
        self.awaiting_publication = true;
    }

    /// Confirm that the result transport now owns publication.
    pub(crate) fn commit_publication(&mut self) {
        debug_assert!(self.awaiting_publication);
        self.publication_committed = true;
    }
}

impl<Identity> Drop for SingleWorkerActivityLease<Identity>
where
    Identity: Clone + Eq + Hash,
{
    fn drop(&mut self) {
        if self.awaiting_publication && !self.publication_committed {
            self.activity.acknowledge_publication(&self.identity);
        } else if !self.awaiting_publication {
            self.activity.abandon(&self.identity);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_and_publication_are_distinct_physical_states() {
        let activity = Arc::new(SingleWorkerActivity::default());
        let mut lease = activity.begin(7_u64);
        assert_eq!(
            activity.snapshot().current,
            Some((7, SingleWorkerPhase::WaitingForDispatch))
        );

        lease.mark_running();
        assert_eq!(
            activity.snapshot().current,
            Some((7, SingleWorkerPhase::Running))
        );

        lease.finish_for_publication();
        lease.commit_publication();
        let finished = activity.snapshot();
        assert!(finished.current.is_none());
        assert_eq!(finished.awaiting_publication, vec![7]);

        activity.acknowledge_publication(&7);
        assert!(activity.snapshot().awaiting_publication.is_empty());
    }

    #[test]
    fn dropped_execution_lease_cannot_leave_false_running_evidence() {
        let activity = Arc::new(SingleWorkerActivity::default());
        {
            let mut lease = activity.begin(9_u64);
            lease.mark_running();
        }
        let snapshot = activity.snapshot();
        assert!(snapshot.current.is_none());
        assert!(snapshot.awaiting_publication.is_empty());
    }

    #[test]
    fn failed_result_transport_cannot_leave_false_publication_backlog() {
        let activity = Arc::new(SingleWorkerActivity::default());
        {
            let mut lease = activity.begin(11_u64);
            lease.mark_running();
            lease.finish_for_publication();
        }
        let snapshot = activity.snapshot();
        assert!(snapshot.current.is_none());
        assert!(snapshot.awaiting_publication.is_empty());
    }
}
