//! Bounded asynchronous lifecycle for Viewer GPU queue submissions.
//!
//! The lifecycle is deliberately presentation-Adapter agnostic. It owns one
//! submitted resource envelope from queue submission through an exact
//! completion callback, including timeout quarantine. Window and Headless
//! Adapters may attach different physical publication artifacts while sharing
//! the same completion, deadline, and resource-retirement semantics.

use std::sync::mpsc;
use std::time::Instant;

/// Exact process-local identity of one Viewer GPU queue submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ViewerGpuSubmissionId(u64);

impl ViewerGpuSubmissionId {
    /// Stable numeric evidence value for diagnostics and validation reports.
    pub(crate) const fn get(self) -> u64 {
        self.0
    }

    /// Construct an isolated identity for sibling Module unit tests.
    #[cfg(test)]
    pub(crate) const fn for_test(value: u64) -> Self {
        Self(value)
    }
}

/// Why publication authority was revoked while GPU ownership remains retained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ViewerGpuSubmissionQuarantineReason {
    /// The exact submission did not produce a completion callback before its
    /// non-renewing safety deadline.
    CompletionDeadlineExceeded,
    /// Driving the concrete GPU device reported a terminal poll failure.
    DevicePollFailed(String),
    /// The semantic consumer revoked publication authority while physical GPU
    /// ownership still awaited its exact completion callback.
    PublicationAuthorityRevoked(String),
}

/// First transition into quarantine for one exact submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ViewerGpuSubmissionQuarantine {
    /// Exact submitted work whose publication authority was revoked.
    pub(crate) submission_id: ViewerGpuSubmissionId,
    /// Typed reason for retaining rather than releasing its owners.
    pub(crate) reason: ViewerGpuSubmissionQuarantineReason,
}

/// One exact completion callback correlated to its retained submission owner.
pub(crate) struct ViewerGpuCompletedSubmission<O, C> {
    /// Exact physical submission identity.
    pub(crate) submission_id: ViewerGpuSubmissionId,
    /// Adapter-owned resource and semantic authority envelope.
    pub(crate) owner: O,
    /// Renderer completion evidence for the exact submitted batch.
    pub(crate) completion: C,
    /// Monotonic time at which the callback became observable.
    #[cfg_attr(not(any(test, feature = "validation")), allow(dead_code))]
    pub(crate) completion_observed_at: Instant,
    /// Why publication authority was revoked before resource retirement.
    pub(crate) quarantine_reason: Option<ViewerGpuSubmissionQuarantineReason>,
}

/// Result of one non-blocking lifecycle observation.
pub(crate) enum ViewerGpuSubmissionPoll<O, C> {
    /// No reservation or submitted work exists.
    Idle,
    /// One exact submission remains retained.
    Pending {
        /// Exact physical submission identity.
        submission_id: ViewerGpuSubmissionId,
        /// Whether only resource retirement remains authoritative.
        #[cfg_attr(not(any(test, feature = "validation")), allow(dead_code))]
        quarantined: bool,
    },
    /// One exact callback proved that retained GPU owners may retire.
    Completed(ViewerGpuCompletedSubmission<O, C>),
    /// A deadline or device failure revoked publication authority while keeping
    /// all submitted owners resident.
    QuarantineStarted(ViewerGpuSubmissionQuarantine),
}

struct ViewerGpuCompletionNotice<C> {
    submission_id: ViewerGpuSubmissionId,
    completion: C,
    observed_at: Instant,
}

struct ViewerGpuInFlight<O> {
    submission_id: ViewerGpuSubmissionId,
    owner: O,
    completion_deadline: Instant,
    quarantine: Option<ViewerGpuSubmissionQuarantineReason>,
}

/// Single-slot asynchronous Viewer GPU submission owner.
///
/// Capacity is intentionally one until the renderer exposes move-only
/// per-frame resource slots. A quarantined slot remains occupied until its
/// exact callback arrives or the owning Adapter/device is dropped.
pub(crate) struct ViewerGpuSubmissionLifecycle<O, C> {
    next_submission_id: u64,
    in_flight: Option<ViewerGpuInFlight<O>>,
    completion_sender: mpsc::Sender<ViewerGpuCompletionNotice<C>>,
    completion_receiver: mpsc::Receiver<ViewerGpuCompletionNotice<C>>,
    orphaned_completion_count: u64,
}

/// Move-only admission authority for one not-yet-submitted Viewer batch.
///
/// The reservation exclusively borrows its lifecycle, so committing the
/// already-submitted owner cannot race another reservation or fail because the
/// slot changed underneath it.
pub(crate) struct ViewerGpuSubmissionReservation<'a, O, C> {
    lifecycle: &'a mut ViewerGpuSubmissionLifecycle<O, C>,
    submission_id: ViewerGpuSubmissionId,
}

impl<O, C> ViewerGpuSubmissionLifecycle<O, C>
where
    O: 'static,
    C: Send + 'static,
{
    /// Construct an empty single-slot owner.
    pub(crate) fn new() -> Self {
        let (completion_sender, completion_receiver) = mpsc::channel();
        Self {
            next_submission_id: 1,
            in_flight: None,
            completion_sender,
            completion_receiver,
            orphaned_completion_count: 0,
        }
    }

    /// Reserve a physical identity before any fallible recording or submission.
    pub(crate) fn reserve(
        &mut self,
    ) -> Result<ViewerGpuSubmissionReservation<'_, O, C>, ViewerGpuSubmissionAdmissionError> {
        if self.in_flight.is_some() {
            return Err(ViewerGpuSubmissionAdmissionError::Backpressured);
        }
        let submission_id = ViewerGpuSubmissionId(self.next_submission_id);
        self.next_submission_id = self
            .next_submission_id
            .checked_add(1)
            .ok_or(ViewerGpuSubmissionAdmissionError::IdentityExhausted)?;
        Ok(ViewerGpuSubmissionReservation { lifecycle: self, submission_id })
    }

    /// Whether recording a new candidate would violate the single-slot grant.
    pub(crate) fn is_occupied(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Exact submitted identity currently retaining the capacity-one slot.
    pub(crate) fn current_submission_id(&self) -> Option<ViewerGpuSubmissionId> {
        self.in_flight.as_ref().map(|in_flight| in_flight.submission_id)
    }

    /// Inspect the retained owner for exact queue-ordered publication.
    pub(crate) fn owner(&self, submission_id: ViewerGpuSubmissionId) -> Option<&O> {
        self.in_flight
            .as_ref()
            .filter(|in_flight| in_flight.submission_id == submission_id)
            .map(|in_flight| &in_flight.owner)
    }

    /// Mutate Adapter-local evidence attached to one exact retained owner.
    pub(crate) fn owner_mut(&mut self, submission_id: ViewerGpuSubmissionId) -> Option<&mut O> {
        self.in_flight
            .as_mut()
            .filter(|in_flight| in_flight.submission_id == submission_id)
            .map(|in_flight| &mut in_flight.owner)
    }

    /// Observe callback, deadline, and quarantine state without blocking.
    pub(crate) fn poll(&mut self, now: Instant) -> ViewerGpuSubmissionPoll<O, C> {
        loop {
            match self.completion_receiver.try_recv() {
                Ok(notice) => {
                    let exact = self
                        .in_flight
                        .as_ref()
                        .is_some_and(|in_flight| in_flight.submission_id == notice.submission_id);
                    if !exact {
                        self.orphaned_completion_count =
                            self.orphaned_completion_count.saturating_add(1);
                        continue;
                    }
                    let Some(in_flight) = self.in_flight.take() else {
                        continue;
                    };
                    let quarantine_reason = in_flight.quarantine.or_else(|| {
                        (notice.observed_at >= in_flight.completion_deadline).then_some(
                            ViewerGpuSubmissionQuarantineReason::CompletionDeadlineExceeded,
                        )
                    });
                    return ViewerGpuSubmissionPoll::Completed(ViewerGpuCompletedSubmission {
                        submission_id: in_flight.submission_id,
                        owner: in_flight.owner,
                        completion: notice.completion,
                        completion_observed_at: notice.observed_at,
                        quarantine_reason,
                    });
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }

        self.poll_deadline_state(now)
    }

    /// Observe only deadline/quarantine state while an exact callback remains
    /// staged behind the device progress domain's post-poll barrier.
    ///
    /// A wgpu work-done callback can be invoked before device-lost in the same
    /// native poll. Adapters use this form until the progress worker publishes
    /// that the poll returned with a healthy generation.
    pub(crate) fn poll_deadline_only(&mut self, now: Instant) -> ViewerGpuSubmissionPoll<O, C> {
        self.poll_deadline_state(now)
    }

    fn poll_deadline_state(&mut self, now: Instant) -> ViewerGpuSubmissionPoll<O, C> {
        let Some(in_flight) = self.in_flight.as_ref() else {
            return ViewerGpuSubmissionPoll::Idle;
        };
        let submission_id = in_flight.submission_id;
        let completion_deadline = in_flight.completion_deadline;
        let quarantined = in_flight.quarantine.is_some();
        if !quarantined && now >= completion_deadline {
            if let Some(quarantine) = self
                .begin_quarantine(ViewerGpuSubmissionQuarantineReason::CompletionDeadlineExceeded)
            {
                return ViewerGpuSubmissionPoll::QuarantineStarted(quarantine);
            }
        }
        ViewerGpuSubmissionPoll::Pending { submission_id, quarantined }
    }

    /// Revoke publication authority after a concrete device failure.
    pub(crate) fn quarantine_after_device_failure(
        &mut self,
        reason: String,
    ) -> Option<ViewerGpuSubmissionQuarantine> {
        self.begin_quarantine(ViewerGpuSubmissionQuarantineReason::DevicePollFailed(
            reason,
        ))
    }

    /// Revoke publication authority without releasing submitted owners.
    pub(crate) fn quarantine_after_authority_revocation(
        &mut self,
        reason: String,
    ) -> Option<ViewerGpuSubmissionQuarantine> {
        self.begin_quarantine(
            ViewerGpuSubmissionQuarantineReason::PublicationAuthorityRevoked(reason),
        )
    }

    /// Retire the retained owner after the concrete wgpu device generation is
    /// known lost.
    ///
    /// This is teardown-only authority: callers must first prove an actual
    /// wgpu device-lost terminal and independently drain any native decoder
    /// copy fences owned outside wgpu. A generic progress failure is not
    /// sufficient release evidence.
    pub(crate) fn retire_owner_after_wgpu_device_loss(&mut self) -> Option<O> {
        self.in_flight.take().map(|in_flight| in_flight.owner)
    }

    fn begin_quarantine(
        &mut self,
        reason: ViewerGpuSubmissionQuarantineReason,
    ) -> Option<ViewerGpuSubmissionQuarantine> {
        let in_flight = self.in_flight.as_mut()?;
        if in_flight.quarantine.is_some() {
            return None;
        }
        in_flight.quarantine = Some(reason.clone());
        Some(ViewerGpuSubmissionQuarantine { submission_id: in_flight.submission_id, reason })
    }

    /// Earliest useful monotonic wake for the retained submission.
    pub(crate) fn next_wake(&self) -> Option<Instant> {
        let in_flight = self.in_flight.as_ref()?;
        if in_flight.quarantine.is_some() {
            None
        } else {
            Some(in_flight.completion_deadline)
        }
    }

    /// Number of callbacks discarded because no exact owner remained.
    ///
    /// This is production diagnostics: an orphan callback is not publication
    /// authority and must remain observable outside unit-test builds.
    #[cfg(any(test, feature = "validation"))]
    pub(crate) const fn orphaned_completion_count(&self) -> u64 {
        self.orphaned_completion_count
    }
}

impl<O, C> ViewerGpuSubmissionReservation<'_, O, C>
where
    O: 'static,
    C: Send + 'static,
{
    /// Exact identity to embed in submission and presentation evidence.
    pub(crate) const fn submission_id(&self) -> ViewerGpuSubmissionId {
        self.submission_id
    }

    /// Commit already-submitted ownership and register its short callback.
    ///
    /// The owner is installed before callback registration, so an immediately
    /// observable callback cannot become orphaned through ordering. Exclusive
    /// lifecycle ownership makes this transition infallible.
    pub(crate) fn commit(
        self,
        owner: O,
        completion_deadline: Instant,
        register_completion: impl FnOnce(Box<dyn FnOnce(C) + Send + 'static>),
        wake: impl Fn() + Send + Sync + 'static,
    ) -> ViewerGpuSubmissionId {
        let submission_id = self.submission_id;
        let lifecycle = self.lifecycle;
        lifecycle.in_flight = Some(ViewerGpuInFlight {
            submission_id,
            owner,
            completion_deadline,
            quarantine: None,
        });
        let completion_sender = lifecycle.completion_sender.clone();
        register_completion(Box::new(move |completion| {
            let notice = ViewerGpuCompletionNotice {
                submission_id,
                completion,
                observed_at: Instant::now(),
            };
            if completion_sender.send(notice).is_ok() {
                wake();
            }
        }));
        submission_id
    }
}

/// Admission failure before any GPU work was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ViewerGpuSubmissionAdmissionError {
    /// The single renderer frame-resource slot remains owned.
    #[error("the Viewer GPU submission slot is occupied")]
    Backpressured,
    /// The process-local submission identity space was exhausted.
    #[error("Viewer GPU submission identity space is exhausted")]
    IdentityExhausted,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    type TestCallback = Box<dyn FnOnce(u64) + Send + 'static>;

    fn callback_slot() -> Arc<Mutex<Option<TestCallback>>> {
        Arc::new(Mutex::new(None))
    }

    fn commit_test_submission(
        lifecycle: &mut ViewerGpuSubmissionLifecycle<String, u64>,
        deadline: Instant,
        callback: &Arc<Mutex<Option<TestCallback>>>,
    ) -> ViewerGpuSubmissionId {
        let reservation = lifecycle.reserve().expect("reserve submission");
        let submission_id = reservation.submission_id();
        let callback = Arc::clone(callback);
        reservation.commit(
            "retained-owner".to_owned(),
            deadline,
            move |registered| {
                *callback.lock().expect("callback slot") = Some(registered);
            },
            || {},
        );
        submission_id
    }

    #[test]
    fn delayed_completion_retains_the_owner_and_frees_exactly_one_slot() {
        let now = Instant::now();
        let callback = callback_slot();
        let mut lifecycle = ViewerGpuSubmissionLifecycle::new();
        let submission_id =
            commit_test_submission(&mut lifecycle, now + Duration::from_secs(1), &callback);

        assert!(lifecycle.is_occupied());
        assert_eq!(
            lifecycle.owner(submission_id).map(String::as_str),
            Some("retained-owner")
        );
        assert!(matches!(
            lifecycle.reserve(),
            Err(ViewerGpuSubmissionAdmissionError::Backpressured)
        ));
        callback.lock().expect("callback slot").take().expect("registered callback")(7);

        let completed = match lifecycle.poll(now) {
            ViewerGpuSubmissionPoll::Completed(completed) => completed,
            _ => panic!("expected exact completion"),
        };
        assert_eq!(completed.submission_id, submission_id);
        assert_eq!(completed.owner, "retained-owner");
        assert_eq!(completed.completion, 7);
        assert_eq!(completed.quarantine_reason, None);
        assert!(!lifecycle.is_occupied());
    }

    #[test]
    fn callback_remains_staged_until_post_poll_progress_barrier() {
        let now = Instant::now();
        let callback = callback_slot();
        let mut lifecycle = ViewerGpuSubmissionLifecycle::new();
        let submission_id =
            commit_test_submission(&mut lifecycle, now + Duration::from_secs(1), &callback);
        callback.lock().expect("callback slot").take().expect("registered callback")(9);

        assert!(matches!(
            lifecycle.poll_deadline_only(now),
            ViewerGpuSubmissionPoll::Pending {
                submission_id: pending,
                quarantined: false,
            } if pending == submission_id
        ));
        assert!(lifecycle.is_occupied());
        assert!(matches!(
            lifecycle.poll(now),
            ViewerGpuSubmissionPoll::Completed(completed)
                if completed.submission_id == submission_id && completed.completion == 9
        ));
    }

    #[test]
    fn timeout_quarantine_keeps_owner_until_a_late_callback_retires_it() {
        let now = Instant::now();
        let callback = callback_slot();
        let mut lifecycle = ViewerGpuSubmissionLifecycle::new();
        let submission_id =
            commit_test_submission(&mut lifecycle, now + Duration::from_millis(1), &callback);

        let quarantine = match lifecycle.poll(now + Duration::from_millis(2)) {
            ViewerGpuSubmissionPoll::QuarantineStarted(quarantine) => quarantine,
            _ => panic!("expected timeout quarantine"),
        };
        assert_eq!(quarantine.submission_id, submission_id);
        assert!(lifecycle.is_occupied());
        assert!(lifecycle.owner(submission_id).is_some());
        callback.lock().expect("callback slot").take().expect("registered callback")(11);

        let completed = match lifecycle.poll(now + Duration::from_millis(3)) {
            ViewerGpuSubmissionPoll::Completed(completed) => completed,
            _ => panic!("expected late completion"),
        };
        assert_eq!(
            completed.quarantine_reason,
            Some(ViewerGpuSubmissionQuarantineReason::CompletionDeadlineExceeded)
        );
        assert_eq!(completed.completion, 11);
        assert!(!lifecycle.is_occupied());
    }

    #[test]
    fn callback_observed_at_the_exact_deadline_is_retirement_only() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        let callback = callback_slot();
        let mut lifecycle = ViewerGpuSubmissionLifecycle::new();
        let submission_id = commit_test_submission(&mut lifecycle, deadline, &callback);
        lifecycle
            .completion_sender
            .send(ViewerGpuCompletionNotice {
                submission_id,
                completion: 13,
                observed_at: deadline,
            })
            .expect("queue boundary completion");

        let completed = match lifecycle.poll(now) {
            ViewerGpuSubmissionPoll::Completed(completed) => completed,
            _ => panic!("expected boundary completion"),
        };
        assert_eq!(
            completed.quarantine_reason,
            Some(ViewerGpuSubmissionQuarantineReason::CompletionDeadlineExceeded)
        );
    }

    #[test]
    fn callback_observed_after_deadline_is_retirement_only_even_before_timeout_poll() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        let callback = callback_slot();
        let mut lifecycle = ViewerGpuSubmissionLifecycle::new();
        let submission_id = commit_test_submission(&mut lifecycle, deadline, &callback);
        lifecycle
            .completion_sender
            .send(ViewerGpuCompletionNotice {
                submission_id,
                completion: 17,
                observed_at: deadline + Duration::from_nanos(1),
            })
            .expect("queue late completion");

        let completed = match lifecycle.poll(now) {
            ViewerGpuSubmissionPoll::Completed(completed) => completed,
            _ => panic!("expected late completion"),
        };
        assert_eq!(
            completed.quarantine_reason,
            Some(ViewerGpuSubmissionQuarantineReason::CompletionDeadlineExceeded)
        );
    }

    #[test]
    fn quarantine_has_no_timer_wake_and_retains_callback_owner() {
        let now = Instant::now();
        let callback = callback_slot();
        let mut lifecycle = ViewerGpuSubmissionLifecycle::new();
        let submission_id =
            commit_test_submission(&mut lifecycle, now + Duration::from_millis(1), &callback);
        assert!(matches!(
            lifecycle.poll(now + Duration::from_millis(2)),
            ViewerGpuSubmissionPoll::QuarantineStarted(_)
        ));

        assert!(matches!(
            lifecycle.poll(now + Duration::from_millis(7)),
            ViewerGpuSubmissionPoll::Pending {
                submission_id: current,
                quarantined: true,
            } if current == submission_id
        ));
        assert_eq!(lifecycle.next_wake(), None);
        assert!(lifecycle.owner(submission_id).is_some());
    }

    #[test]
    fn orphan_completion_never_consumes_the_current_submission() {
        let now = Instant::now();
        let callback = callback_slot();
        let mut lifecycle = ViewerGpuSubmissionLifecycle::new();
        let submission_id =
            commit_test_submission(&mut lifecycle, now + Duration::from_secs(1), &callback);
        lifecycle
            .completion_sender
            .send(ViewerGpuCompletionNotice {
                submission_id: ViewerGpuSubmissionId(submission_id.get() + 1),
                completion: 3,
                observed_at: now,
            })
            .expect("queue orphan");

        assert!(matches!(
            lifecycle.poll(now),
            ViewerGpuSubmissionPoll::Pending { submission_id: current, .. }
                if current == submission_id
        ));
        assert_eq!(lifecycle.orphaned_completion_count(), 1);
        assert!(lifecycle.owner(submission_id).is_some());
    }
}
