//! Physical Viewer GPU publication ownership shared by presentation Adapters.
//!
//! Preview production state retains cloneable semantic output metadata. This
//! Module separately owns the move-only physical lease for the visible output
//! and for at most one prepared successor. Window and Headless Adapters attach
//! different artifact payloads while sharing exact promotion and retirement
//! semantics.

use crate::app::viewer_gpu_submission::ViewerGpuSubmissionId;

/// One physical Viewer publication and its move-only renderer lease.
pub(crate) struct ViewerGpuPhysicalPublication<K, O, L> {
    source_submission_id: ViewerGpuSubmissionId,
    output_key: K,
    artifact: O,
    _lease: L,
}

impl<K, O, L> ViewerGpuPhysicalPublication<K, O, L> {
    /// Submission that created this exact physical allocation.
    pub(crate) const fn source_submission_id(&self) -> ViewerGpuSubmissionId {
        self.source_submission_id
    }

    /// Semantic output identity bound to the physical allocation.
    pub(crate) const fn output_key(&self) -> &K {
        &self.output_key
    }

    /// Adapter-owned artifact kept usable by the retained lease.
    pub(crate) const fn artifact(&self) -> &O {
        &self.artifact
    }

    /// Consume the publication and return its semantic identity and artifact.
    ///
    /// The move-only lease is retired before this method returns.
    pub(crate) fn into_key_and_artifact(self) -> (K, O) {
        let Self {
            output_key,
            artifact,
            _lease: _,
            source_submission_id: _,
        } = self;
        (output_key, artifact)
    }

    /// Consume the publication and return only its Adapter artifact.
    ///
    /// The move-only lease is retired before this method returns.
    pub(crate) fn into_artifact(self) -> O {
        self.into_key_and_artifact().1
    }
}

/// Result of asking the physical owner to promote one exact prepared output.
pub(crate) struct ViewerGpuPreparedPromotion<K, O, L> {
    exact_output_available: bool,
    retired: Option<ViewerGpuPhysicalPublication<K, O, L>>,
}

impl<K, O, L> ViewerGpuPreparedPromotion<K, O, L> {
    /// Whether current already matched or the exact prepared output was promoted.
    pub(crate) const fn exact_output_available(&self) -> bool {
        self.exact_output_available
    }

    /// Publication made redundant by the promotion, if any.
    pub(crate) fn into_retired(self) -> Option<ViewerGpuPhysicalPublication<K, O, L>> {
        self.retired
    }
}

/// Capacity-one visible publication plus one capacity-one prepared successor.
///
/// Submission capacity is independent: the last accepted output may remain
/// visible while one replacement is in flight and one already-completed exact
/// successor is retained without becoming visible.
pub(crate) struct ViewerGpuPublicationSlots<K, O, L> {
    current: Option<ViewerGpuPhysicalPublication<K, O, L>>,
    prepared: Option<ViewerGpuPhysicalPublication<K, O, L>>,
}

impl<K, O, L> Default for ViewerGpuPublicationSlots<K, O, L> {
    fn default() -> Self {
        Self { current: None, prepared: None }
    }
}

impl<K: PartialEq, O, L> ViewerGpuPublicationSlots<K, O, L> {
    fn publication(
        source_submission_id: ViewerGpuSubmissionId,
        output_key: K,
        artifact: O,
        lease: L,
    ) -> ViewerGpuPhysicalPublication<K, O, L> {
        ViewerGpuPhysicalPublication {
            source_submission_id,
            output_key,
            artifact,
            _lease: lease,
        }
    }

    /// Publish a visible output and return the previous visible owner.
    pub(crate) fn publish_current(
        &mut self,
        source_submission_id: ViewerGpuSubmissionId,
        output_key: K,
        artifact: O,
        lease: L,
    ) -> Option<ViewerGpuPhysicalPublication<K, O, L>> {
        self.current.replace(Self::publication(
            source_submission_id,
            output_key,
            artifact,
            lease,
        ))
    }

    /// Retain a ticketless successor and return the prior prepared owner.
    pub(crate) fn publish_prepared(
        &mut self,
        source_submission_id: ViewerGpuSubmissionId,
        output_key: K,
        artifact: O,
        lease: L,
    ) -> Option<ViewerGpuPhysicalPublication<K, O, L>> {
        self.prepared.replace(Self::publication(
            source_submission_id,
            output_key,
            artifact,
            lease,
        ))
    }

    /// Visible physical owner, irrespective of semantic identity.
    pub(crate) const fn current(&self) -> Option<&ViewerGpuPhysicalPublication<K, O, L>> {
        self.current.as_ref()
    }

    /// Visible artifact only when it has the exact semantic output identity.
    pub(crate) fn current_artifact_for_key(&self, output_key: &K) -> Option<&O> {
        self.current
            .as_ref()
            .filter(|current| current.output_key() == output_key)
            .map(ViewerGpuPhysicalPublication::artifact)
    }

    /// Promote only the exact prepared identity into the visible slot.
    ///
    /// A matching prepared owner takes precedence even when current has the
    /// same semantic key: the semantic coordinator may already have promoted
    /// that prepared artifact, so keeping an older same-key physical resource
    /// would split semantic and physical identity. With no matching prepared
    /// owner, an already-exact current output remains valid.
    pub(crate) fn promote_prepared_exact(
        &mut self,
        output_key: &K,
    ) -> ViewerGpuPreparedPromotion<K, O, L> {
        if let Some(prepared) =
            self.prepared.take_if(|prepared| prepared.output_key() == output_key)
        {
            let retired = self.current.replace(prepared);
            return ViewerGpuPreparedPromotion { exact_output_available: true, retired };
        }
        ViewerGpuPreparedPromotion {
            exact_output_available: self
                .current
                .as_ref()
                .is_some_and(|current| current.output_key() == output_key),
            retired: None,
        }
    }

    /// Remove only the owner created by one exact submission.
    pub(crate) fn take_for_submission(
        &mut self,
        submission_id: ViewerGpuSubmissionId,
    ) -> Option<ViewerGpuPhysicalPublication<K, O, L>> {
        if self
            .current
            .as_ref()
            .is_some_and(|current| current.source_submission_id() == submission_id)
        {
            return self.current.take();
        }
        if self
            .prepared
            .as_ref()
            .is_some_and(|prepared| prepared.source_submission_id() == submission_id)
        {
            self.prepared.take()
        } else {
            None
        }
    }

    /// Revoke both physical owners and return them for Adapter cleanup.
    pub(crate) fn drain(&mut self) -> [Option<ViewerGpuPhysicalPublication<K, O, L>>; 2] {
        [self.current.take(), self.prepared.take()]
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use super::*;

    struct TestLease(Arc<AtomicUsize>);

    impl Drop for TestLease {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn publish(
        slots: &mut ViewerGpuPublicationSlots<String, &'static str, TestLease>,
        prepared: bool,
        submission: u64,
        key: &str,
        artifact: &'static str,
        drops: &Arc<AtomicUsize>,
    ) {
        let lease = TestLease(Arc::clone(drops));
        if prepared {
            let _ = slots.publish_prepared(
                ViewerGpuSubmissionId::for_test(submission),
                key.to_owned(),
                artifact,
                lease,
            );
        } else {
            let _ = slots.publish_current(
                ViewerGpuSubmissionId::for_test(submission),
                key.to_owned(),
                artifact,
                lease,
            );
        }
    }

    #[test]
    fn exact_prepared_promotion_retires_previous_current() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut slots = ViewerGpuPublicationSlots::default();
        publish(&mut slots, false, 1, "current", "a", &drops);
        publish(&mut slots, true, 2, "next", "b", &drops);

        let promotion = slots.promote_prepared_exact(&"next".to_owned());
        assert!(promotion.exact_output_available());
        assert_eq!(
            promotion.into_retired().map(|retired| retired.into_artifact()),
            Some("a")
        );
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(
            slots.current_artifact_for_key(&"next".to_owned()),
            Some(&"b")
        );
    }

    #[test]
    fn already_current_keeps_a_distinct_successor() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut slots = ViewerGpuPublicationSlots::default();
        publish(&mut slots, false, 1, "current", "a", &drops);
        publish(&mut slots, true, 2, "next", "b", &drops);

        let promotion = slots.promote_prepared_exact(&"current".to_owned());
        assert!(promotion.exact_output_available());
        assert!(promotion.into_retired().is_none());
        assert_eq!(
            slots
                .take_for_submission(ViewerGpuSubmissionId::for_test(2))
                .map(ViewerGpuPhysicalPublication::into_artifact),
            Some("b")
        );
    }

    #[test]
    fn matching_prepared_artifact_replaces_an_older_same_key_current() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut slots = ViewerGpuPublicationSlots::default();
        publish(&mut slots, false, 1, "same", "old", &drops);
        publish(&mut slots, true, 2, "same", "prepared", &drops);

        let promotion = slots.promote_prepared_exact(&"same".to_owned());
        assert!(promotion.exact_output_available());
        assert_eq!(
            promotion.into_retired().map(ViewerGpuPhysicalPublication::into_artifact),
            Some("old")
        );
        assert_eq!(
            slots.current_artifact_for_key(&"same".to_owned()),
            Some(&"prepared")
        );
    }

    #[test]
    fn exact_submission_retirement_cannot_clear_a_replacement() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut slots = ViewerGpuPublicationSlots::default();
        publish(&mut slots, false, 3, "same", "old", &drops);
        publish(&mut slots, false, 4, "same", "new", &drops);

        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(slots.take_for_submission(ViewerGpuSubmissionId::for_test(3)).is_none());
        assert_eq!(
            slots.current_artifact_for_key(&"same".to_owned()),
            Some(&"new")
        );
    }

    #[test]
    fn generation_retirement_returns_current_and_prepared() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut slots = ViewerGpuPublicationSlots::default();
        publish(&mut slots, false, 5, "current", "a", &drops);
        publish(&mut slots, true, 6, "prepared", "b", &drops);

        let retired = slots.drain();
        assert_eq!(
            retired
                .into_iter()
                .flatten()
                .map(ViewerGpuPhysicalPublication::into_artifact)
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        assert!(slots.current().is_none());
    }
}
