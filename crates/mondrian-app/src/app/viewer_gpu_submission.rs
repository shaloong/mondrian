//! Bounded asynchronous lifecycle for Viewer GPU queue submissions.
//!
//! The lifecycle is deliberately presentation-Adapter agnostic. It owns a
//! bounded set of submitted resource envelopes from queue submission through exact
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

/// One retained submission force-retired after quarantine without its callback.
///
/// The exact completion callback never arrived within the bounded grace after
/// quarantine. Keeping the single submission slot occupied forever would stall
/// the whole presentation pipeline on one lost callback, so the owner is
/// retired and the slot released; a callback that arrives later is counted as
/// orphaned by the lifecycle.
pub(crate) struct ViewerGpuRetiredSubmission<O> {
    /// Exact physical submission identity.
    pub(crate) submission_id: ViewerGpuSubmissionId,
    /// Adapter-owned resource and semantic authority envelope.
    pub(crate) owner: O,
    /// Quarantine reason that revoked publication authority.
    pub(crate) reason: ViewerGpuSubmissionQuarantineReason,
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
    /// A quarantined submission's completion callback never arrived within the
    /// bounded grace; the slot is force-released and the owner retired.
    RetiredAfterQuarantine(ViewerGpuRetiredSubmission<O>),
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

/// Bounded asynchronous Viewer GPU submission owner.
///
/// Capacity matches the bounded CPU staging horizon, so already queue-published
/// frames may retain callback cleanup while an exact staged current frame still
/// enters the same GPU queue. These additional slots are cleanup owners only:
/// physical publication remains one current plus one prepared output. A
/// quarantined slot remains occupied until its
/// exact callback arrives, the owning Adapter/device is dropped, or the
/// bounded [`QUARANTINE_RELEASE_GRACE`] after the completion deadline elapses.
pub(crate) struct ViewerGpuSubmissionLifecycle<O, C> {
    next_submission_id: u64,
    in_flight: Vec<ViewerGpuInFlight<O>>,
    completion_sender: mpsc::Sender<ViewerGpuCompletionNotice<C>>,
    completion_receiver: mpsc::Receiver<ViewerGpuCompletionNotice<C>>,
    orphaned_completion_count: u64,
}

/// Submitted cleanup-owner horizon, aligned with bounded CPU frame staging.
pub(crate) const VIEWER_GPU_SUBMISSION_CAPACITY: usize =
    super::preview_execution::PREVIEW_GPU_CPU_STAGING_CAPACITY;

/// Bounded additional wait after quarantine for the exact completion callback
/// before its bounded slot is force-released.
///
/// A lost wgpu work-done callback must stall only its exact Viewer submission slot
/// for at most this grace; after it, the owner is retired with its quarantine
/// reason and a late callback is counted as orphaned.
pub(crate) const QUARANTINE_RELEASE_GRACE: std::time::Duration =
    std::time::Duration::from_millis(500);

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
    /// Construct an empty bounded owner.
    pub(crate) fn new() -> Self {
        let (completion_sender, completion_receiver) = mpsc::channel();
        Self {
            next_submission_id: 1,
            in_flight: Vec::with_capacity(VIEWER_GPU_SUBMISSION_CAPACITY),
            completion_sender,
            completion_receiver,
            orphaned_completion_count: 0,
        }
    }

    /// Reserve a physical identity before any fallible recording or submission.
    pub(crate) fn reserve(
        &mut self,
    ) -> Result<ViewerGpuSubmissionReservation<'_, O, C>, ViewerGpuSubmissionAdmissionError> {
        if self.in_flight.len() >= VIEWER_GPU_SUBMISSION_CAPACITY {
            return Err(ViewerGpuSubmissionAdmissionError::Backpressured);
        }
        let submission_id = ViewerGpuSubmissionId(self.next_submission_id);
        self.next_submission_id = self
            .next_submission_id
            .checked_add(1)
            .ok_or(ViewerGpuSubmissionAdmissionError::IdentityExhausted)?;
        Ok(ViewerGpuSubmissionReservation { lifecycle: self, submission_id })
    }

    /// Whether any submitted owner remains active.
    pub(crate) fn is_occupied(&self) -> bool {
        !self.in_flight.is_empty()
    }

    /// Whether another exact submitted owner may be admitted.
    pub(crate) fn is_at_capacity(&self) -> bool {
        self.in_flight.len() >= VIEWER_GPU_SUBMISSION_CAPACITY
    }

    /// Number of exact submitted owners still retained by this lifecycle.
    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn active_count(&self) -> usize {
        self.in_flight.len()
    }

    /// Whether this exact submitted identity is still retained.
    pub(crate) fn contains(&self, submission_id: ViewerGpuSubmissionId) -> bool {
        self.in_flight.iter().any(|in_flight| in_flight.submission_id == submission_id)
    }

    /// Inspect the retained owner for exact queue-ordered publication.
    pub(crate) fn owner(&self, submission_id: ViewerGpuSubmissionId) -> Option<&O> {
        self.in_flight
            .iter()
            .find(|in_flight| in_flight.submission_id == submission_id)
            .map(|in_flight| &in_flight.owner)
    }

    /// Mutate Adapter-local evidence attached to one exact retained owner.
    pub(crate) fn owner_mut(&mut self, submission_id: ViewerGpuSubmissionId) -> Option<&mut O> {
        self.in_flight
            .iter_mut()
            .find(|in_flight| in_flight.submission_id == submission_id)
            .map(|in_flight| &mut in_flight.owner)
    }

    /// Whether any bounded submitted owner satisfies an Adapter-local query.
    #[cfg(test)]
    pub(crate) fn any_owner(&self, mut predicate: impl FnMut(&O) -> bool) -> bool {
        self.in_flight.iter().any(|in_flight| predicate(&in_flight.owner))
    }

    /// Observe callback, deadline, and quarantine state without blocking.
    pub(crate) fn poll(&mut self, now: Instant) -> ViewerGpuSubmissionPoll<O, C> {
        if let Some(completed) = self.consume_completion_notice() {
            return completed;
        }
        self.poll_deadline_state(now)
    }

    /// Observe deadline/quarantine state and any already-arrived completion.
    ///
    /// The wgpu work-done callback is the authoritative GPU completion
    /// evidence; the device progress worker's post-poll barrier is only an
    /// auxiliary queue-observation. A barrier that races or lags the callback
    /// must not defer the exact completion, otherwise the retained output is
    /// revoked by the quarantine deadline and the presentation pipeline
    /// re-submits the same frame forever.
    pub(crate) fn poll_deadline_only(&mut self, now: Instant) -> ViewerGpuSubmissionPoll<O, C> {
        if let Some(completed) = self.consume_completion_notice() {
            return completed;
        }
        self.poll_deadline_state(now)
    }

    /// Consume one already-arrived authoritative completion notice.
    ///
    /// `None` means no exact notice is pending; the caller then observes
    /// deadline/quarantine state. Stale notices whose owner was already
    /// retired are counted as orphaned and never manufacture completion.
    fn consume_completion_notice(&mut self) -> Option<ViewerGpuSubmissionPoll<O, C>> {
        loop {
            match self.completion_receiver.try_recv() {
                Ok(notice) => {
                    let exact = self
                        .in_flight
                        .iter()
                        .position(|in_flight| in_flight.submission_id == notice.submission_id);
                    let Some(exact) = exact else {
                        self.orphaned_completion_count =
                            self.orphaned_completion_count.saturating_add(1);
                        continue;
                    };
                    let in_flight = self.in_flight.remove(exact);
                    let quarantine_reason = in_flight.quarantine.or_else(|| {
                        (notice.observed_at >= in_flight.completion_deadline).then_some(
                            ViewerGpuSubmissionQuarantineReason::CompletionDeadlineExceeded,
                        )
                    });
                    return Some(ViewerGpuSubmissionPoll::Completed(
                        ViewerGpuCompletedSubmission {
                            submission_id: in_flight.submission_id,
                            owner: in_flight.owner,
                            completion: notice.completion,
                            completion_observed_at: notice.observed_at,
                            quarantine_reason,
                        },
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => return None,
            }
        }
    }

    fn poll_deadline_state(&mut self, now: Instant) -> ViewerGpuSubmissionPoll<O, C> {
        let Some(first) = self.in_flight.first() else {
            return ViewerGpuSubmissionPoll::Idle;
        };
        if let Some(index) = self.in_flight.iter().position(|in_flight| {
            in_flight.quarantine.is_none() && now >= in_flight.completion_deadline
        }) {
            let submission_id = self.in_flight[index].submission_id;
            let reason = ViewerGpuSubmissionQuarantineReason::CompletionDeadlineExceeded;
            self.in_flight[index].quarantine = Some(reason.clone());
            let quarantine = ViewerGpuSubmissionQuarantine { submission_id, reason };
            return ViewerGpuSubmissionPoll::QuarantineStarted(quarantine);
        }
        if let Some(index) = self.in_flight.iter().position(|in_flight| {
            in_flight.quarantine.is_some()
                && now >= in_flight.completion_deadline + QUARANTINE_RELEASE_GRACE
        }) {
            // The exact completion callback never arrived. Release the
            // bounded slot so the presentation pipeline can continue;
            // the retired owner carries the quarantine reason for cleanup and
            // a late callback is counted as orphaned.
            let in_flight = self.in_flight.remove(index);
            let submission_id = in_flight.submission_id;
            let reason = in_flight
                .quarantine
                .unwrap_or(ViewerGpuSubmissionQuarantineReason::CompletionDeadlineExceeded);
            return ViewerGpuSubmissionPoll::RetiredAfterQuarantine(ViewerGpuRetiredSubmission {
                submission_id,
                owner: in_flight.owner,
                reason,
            });
        }
        ViewerGpuSubmissionPoll::Pending {
            submission_id: first.submission_id,
            quarantined: first.quarantine.is_some(),
        }
    }

    /// Complete one submission on fence-barrier authority without its callback
    /// batch.
    ///
    /// The device progress worker publishes its bounded wait only after wgpu
    /// reported `WaitSucceeded` for the exact submission, which is the
    /// authoritative GPU completion evidence. The `on_submitted_work_done`
    /// callback batch is a supplementary carrier that may race or lag that
    /// fence (wgpu 30 defers callback delivery), so an absent batch must not
    /// strand the completion behind a quarantine that revokes the retained
    /// output and forces the pipeline to re-submit the same frame forever.
    /// Complete one exact submission on its correlated fence barrier.
    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn retire_submission_after_fence_barrier(
        &mut self,
        submission_id: ViewerGpuSubmissionId,
        now: Instant,
    ) -> Option<ViewerGpuSubmissionPoll<O, C>>
    where
        C: Default,
    {
        let index = self
            .in_flight
            .iter()
            .position(|in_flight| in_flight.submission_id == submission_id)?;
        let in_flight = self.in_flight.remove(index);
        Some(ViewerGpuSubmissionPoll::Completed(
            ViewerGpuCompletedSubmission {
                submission_id: in_flight.submission_id,
                owner: in_flight.owner,
                completion: C::default(),
                completion_observed_at: now,
                quarantine_reason: in_flight.quarantine,
            },
        ))
    }

    /// Revoke publication authority for every owner after a concrete device
    /// generation failure.
    pub(crate) fn quarantine_all_after_device_failure(
        &mut self,
        reason: String,
    ) -> Vec<ViewerGpuSubmissionQuarantine> {
        self.quarantine_all(ViewerGpuSubmissionQuarantineReason::DevicePollFailed(
            reason,
        ))
    }

    /// Revoke publication authority from every submitted owner without
    /// releasing any callback-owned resources.
    pub(crate) fn quarantine_all_after_authority_revocation(
        &mut self,
        reason: String,
    ) -> Vec<ViewerGpuSubmissionQuarantine> {
        self.quarantine_all(
            ViewerGpuSubmissionQuarantineReason::PublicationAuthorityRevoked(reason),
        )
    }

    /// Revoke one exact submitted owner's publication authority.
    pub(crate) fn quarantine_submission_after_authority_revocation(
        &mut self,
        submission_id: ViewerGpuSubmissionId,
        reason: String,
    ) -> Option<ViewerGpuSubmissionQuarantine> {
        self.begin_quarantine(
            submission_id,
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
    pub(crate) fn retire_owners_after_wgpu_device_loss(&mut self) -> Vec<O> {
        self.in_flight.drain(..).map(|in_flight| in_flight.owner).collect()
    }

    fn begin_quarantine(
        &mut self,
        submission_id: ViewerGpuSubmissionId,
        reason: ViewerGpuSubmissionQuarantineReason,
    ) -> Option<ViewerGpuSubmissionQuarantine> {
        let in_flight = self
            .in_flight
            .iter_mut()
            .find(|in_flight| in_flight.submission_id == submission_id)?;
        if in_flight.quarantine.is_some() {
            return None;
        }
        in_flight.quarantine = Some(reason.clone());
        Some(ViewerGpuSubmissionQuarantine { submission_id: in_flight.submission_id, reason })
    }

    fn quarantine_all(
        &mut self,
        reason: ViewerGpuSubmissionQuarantineReason,
    ) -> Vec<ViewerGpuSubmissionQuarantine> {
        let mut quarantines = Vec::with_capacity(self.in_flight.len());
        for in_flight in &mut self.in_flight {
            if in_flight.quarantine.is_some() {
                continue;
            }
            in_flight.quarantine = Some(reason.clone());
            quarantines.push(ViewerGpuSubmissionQuarantine {
                submission_id: in_flight.submission_id,
                reason: reason.clone(),
            });
        }
        quarantines
    }

    /// Earliest useful monotonic wake for the retained submission.
    pub(crate) fn next_wake(&self) -> Option<Instant> {
        self.in_flight
            .iter()
            .filter(|in_flight| in_flight.quarantine.is_none())
            .map(|in_flight| in_flight.completion_deadline)
            .min()
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
        lifecycle.in_flight.push(ViewerGpuInFlight {
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
    /// Every bounded renderer submission slot remains owned.
    #[error("the Viewer GPU submission capacity is full")]
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
        let first_callback = callback_slot();
        let mut lifecycle = ViewerGpuSubmissionLifecycle::new();
        let submission_id = commit_test_submission(
            &mut lifecycle,
            now + Duration::from_secs(1),
            &first_callback,
        );
        let mut retained_callbacks = Vec::new();
        let mut retained_submission_ids = Vec::new();
        for _ in 1..VIEWER_GPU_SUBMISSION_CAPACITY {
            let callback = callback_slot();
            retained_submission_ids.push(commit_test_submission(
                &mut lifecycle,
                now + Duration::from_secs(1),
                &callback,
            ));
            retained_callbacks.push(callback);
        }

        assert!(lifecycle.is_occupied());
        assert_eq!(
            lifecycle.owner(submission_id).map(String::as_str),
            Some("retained-owner")
        );
        assert!(matches!(
            lifecycle.reserve(),
            Err(ViewerGpuSubmissionAdmissionError::Backpressured)
        ));
        first_callback
            .lock()
            .expect("callback slot")
            .take()
            .expect("registered callback")(7);

        let completed = match lifecycle.poll(now) {
            ViewerGpuSubmissionPoll::Completed(completed) => completed,
            _ => panic!("expected exact completion"),
        };
        assert_eq!(completed.submission_id, submission_id);
        assert_eq!(completed.owner, "retained-owner");
        assert_eq!(completed.completion, 7);
        assert_eq!(completed.quarantine_reason, None);
        assert!(lifecycle.is_occupied());
        assert!(retained_submission_ids
            .iter()
            .all(|submission_id| lifecycle.owner(*submission_id).is_some()));
        assert_eq!(retained_callbacks.len(), VIEWER_GPU_SUBMISSION_CAPACITY - 1);
        assert!(lifecycle.reserve().is_ok());
    }

    #[test]
    fn generation_failure_quarantines_every_pipelined_owner() {
        let now = Instant::now();
        let mut lifecycle = ViewerGpuSubmissionLifecycle::new();
        let mut callbacks = Vec::new();
        let mut expected_submission_ids = Vec::new();
        for _ in 0..VIEWER_GPU_SUBMISSION_CAPACITY {
            let callback = callback_slot();
            expected_submission_ids.push(commit_test_submission(
                &mut lifecycle,
                now + Duration::from_secs(1),
                &callback,
            ));
            callbacks.push(callback);
        }

        let quarantines = lifecycle.quarantine_all_after_device_failure("device lost".to_owned());

        assert_eq!(
            quarantines
                .iter()
                .map(|quarantine| quarantine.submission_id)
                .collect::<Vec<_>>(),
            expected_submission_ids
        );
        assert_eq!(callbacks.len(), VIEWER_GPU_SUBMISSION_CAPACITY);
        assert!(matches!(
            lifecycle.poll(now),
            ViewerGpuSubmissionPoll::Pending { quarantined: true, .. }
        ));
        assert!(matches!(
            lifecycle.reserve(),
            Err(ViewerGpuSubmissionAdmissionError::Backpressured)
        ));
    }

    #[test]
    fn callback_completion_is_consumed_without_requiring_the_progress_barrier() {
        let now = Instant::now();
        let callback = callback_slot();
        let mut lifecycle = ViewerGpuSubmissionLifecycle::new();
        let submission_id =
            commit_test_submission(&mut lifecycle, now + Duration::from_secs(1), &callback);
        callback.lock().expect("callback slot").take().expect("registered callback")(9);

        // The wgpu work-done callback is the authoritative GPU completion
        // evidence. It must complete the submission even when the progress
        // worker's post-poll barrier races or lags the callback; otherwise the
        // retained output is revoked by the quarantine deadline and the
        // presentation pipeline re-submits the same frame forever.
        assert!(matches!(
            lifecycle.poll_deadline_only(now),
            ViewerGpuSubmissionPoll::Completed(completed)
                if completed.submission_id == submission_id && completed.completion == 9
        ));
        assert!(!lifecycle.is_occupied());
        assert!(matches!(lifecycle.poll(now), ViewerGpuSubmissionPoll::Idle));
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
