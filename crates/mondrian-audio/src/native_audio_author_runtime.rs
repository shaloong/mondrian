//! Current installed native author source admission on the sole Runtime.
//!
//! A closed source is a current observation, not a persistent authorization,
//! author snapshot or DSP checkpoint. It must be rechecked after foreign work
//! and immediately before a lifecycle operation can erase that source.

/// Why the exact installed native source cannot yet be reseeded or captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioAuthorHoldReason {
    /// This adapter has no actual normative author source.
    UnsupportedSource,
    /// An actual parameter edit or timestamp scope remains open.
    OpenBoundary,
    /// Ordered callbacks have not reached the canonical Session.
    PendingCallbacks,
    /// Accepted readonly/nonrecorded output is retained for its real observer.
    /// This does not block ordinary PCM or authorize an author transaction.
    PendingParameterObservations,
    /// Native editor values have not reached the same DSP processor.
    DspFeedbackPending,
    /// Callback loss or an invalid native boundary is lifetime-sticky.
    SourceFailure(crate::NativeAudioAuthorSourceFailure),
    /// A child or Runtime delivery awaits Session/plan/acknowledgement.
    OutstandingDelivery,
    /// This coherent opaque state still needs actual Scope/peer propagation.
    /// Installing only its future seed cannot update another live DSP object.
    CoherentPeerStateRequired,
    /// The actual producer explicitly refused the complete author batch.
    DeliveryRefused(crate::NativeAudioAuthorRefusal),
}

/// Recoverable native author admission refusal, separate from PCM failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NativeAudioAuthorAdmissionError {
    /// Superseding work cancelled before or during native observation.
    #[error("native author admission was cancelled")]
    Cancelled,
    /// The current installation claim or actual Hosted object changed.
    #[error(transparent)]
    Owner(#[from] crate::AudioNativeOwnerError),
    /// A native observation operation failed without an author commit.
    #[error(transparent)]
    Processor(#[from] crate::AudioProcessorHostError),
    /// Keep this exact installed object until its source becomes coherent.
    #[error("native author source is pending at {target:?}: {reason:?}")]
    Pending {
        /// Read-only exact address of the currently installed native owner.
        target: Box<mondrian_core::NativeAudioEditorTarget>,
        /// Actual source/delivery refusal; no synthetic empty proof.
        reason: NativeAudioAuthorHoldReason,
    },
}

pub(crate) fn closed_source(
    author: Option<&crate::NativeAudioAuthorObservation>,
    outstanding: bool,
) -> Result<(), NativeAudioAuthorHoldReason> {
    use crate::NativeAudioAuthorDeliveryState as Delivery;
    use NativeAudioAuthorHoldReason as Reason;
    let author = author.ok_or(Reason::UnsupportedSource)?;
    author.source.validate().map_err(Reason::DeliveryRefused)?;
    if let Some(failure) = author.source.failure() {
        return Err(Reason::SourceFailure(failure));
    }
    if author.source.publishing() {
        return Err(Reason::PendingCallbacks);
    }
    if author.source.parameter_flush_pending() {
        return Err(Reason::DspFeedbackPending);
    }
    match author.delivery {
        Delivery::Preserved => {}
        Delivery::Refused(reason) => return Err(Reason::DeliveryRefused(reason)),
        Delivery::Delivered(_)
        | Delivery::AwaitingAcknowledgement { .. }
        | Delivery::Acknowledged { .. }
        | Delivery::ReenteredAcknowledged(_) => {
            return Err(Reason::OutstandingDelivery);
        }
    }
    if outstanding {
        return Err(Reason::OutstandingDelivery);
    }
    if author.source.open_parameter_edits() != 0 || author.source.group_depth() != 0 {
        return Err(Reason::OpenBoundary);
    }
    if author.source.pending_events() != 0 {
        return Err(Reason::PendingCallbacks);
    }
    if author.source.pending_parameter_observations() != 0 {
        return Err(Reason::PendingParameterObservations);
    }
    if author.source.pending_dsp_feedback_values() != 0 {
        return Err(Reason::DspFeedbackPending);
    }
    Ok(())
}

/// Actual same-object root preflight deferred before any PCM or DSP mutation.
/// This read-only projection is not an author permission or persistent lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAudioAuthorExecutionHold {
    /// Exact currently installed owner whose pending source blocks this window.
    pub target: mondrian_core::NativeAudioEditorTarget,
    /// Real source/delivery cause. Open gestures alone never block audition.
    pub reason: NativeAudioAuthorHoldReason,
}

/// A parameter-only window may proceed during open Begin/group scopes after
/// each accepted batch is installed and acknowledged. Lifecycle/reseed callers
/// still use `closed_source`, which additionally requires all boundaries closed.
pub(crate) fn author_pcm_source(
    author: Option<&crate::NativeAudioAuthorObservation>,
    outstanding: bool,
) -> Result<(), NativeAudioAuthorHoldReason> {
    use crate::NativeAudioAuthorDeliveryState as Delivery;
    use NativeAudioAuthorHoldReason as Reason;
    if outstanding {
        return Err(Reason::OutstandingDelivery);
    }
    let Some(author) = author else {
        return Ok(());
    };
    author.source.validate().map_err(Reason::DeliveryRefused)?;
    if let Some(failure) = author.source.failure() {
        return Err(Reason::SourceFailure(failure));
    }
    if author.source.publishing() {
        return Err(Reason::PendingCallbacks);
    }
    if author.source.parameter_flush_pending() {
        return Err(Reason::DspFeedbackPending);
    }
    match &author.delivery {
        Delivery::Preserved => {}
        Delivery::Refused(reason) => return Err(Reason::DeliveryRefused(*reason)),
        Delivery::Delivered(_)
        | Delivery::AwaitingAcknowledgement { .. }
        | Delivery::Acknowledged { .. }
        | Delivery::ReenteredAcknowledged(_) => return Err(Reason::OutstandingDelivery),
    }
    if author.source.pending_events() != 0 {
        return Err(Reason::PendingCallbacks);
    }
    if author.source.pending_dsp_feedback_values() != 0 {
        return Err(Reason::DspFeedbackPending);
    }
    Ok(())
}
