//! Bounded native user services bound by the sole executing processor bank.
//!
//! These are runtime observations and real GUI replies, not an authoring or
//! processor owner. The product separately owns its current author/GUI proof.

use crate::{
    EffectFloatProcessorPreparation, EffectProcessorExecutionOrigin, EffectProcessorNotice,
};
use mondrian_core::EffectProcessorOwnerId;

/// Current host GUI facts. Presence and permission to wait for a user differ.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EffectProcessorGuiState {
    /// The actual effect GUI is closed, absent, or no longer current.
    #[default]
    Closed,
    /// A real effect GUI is open on this occurrence, with no human-wait grant.
    Open,
    /// Explicit nonrealtime GUI work may ask a real user without suspending an
    /// absolute scheduler deadline. Ordinary playback/scrub cannot issue this.
    QuestionWork,
    /// A real user edit caused this render, but it may not wait for a user.
    UserEdited,
    /// Explicit GUI work caused by a real user edit may ask a question.
    UserEditedQuestionWork,
}

/// Bank-admitted GUI facts for one exact currently executing occurrence.
/// Private fields prevent treating a display selection as this admission.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EffectProcessorGuiAdmission {
    state: EffectProcessorGuiState,
}
impl EffectProcessorGuiAdmission {
    /// A caller without a real current GUI gets no interaction privileges.
    pub const CLOSED: Self = Self { state: EffectProcessorGuiState::Closed };
    pub(crate) const fn new(state: EffectProcessorGuiState) -> Self {
        Self { state }
    }
    /// Courtesy interactivity: a real corresponding effect GUI is open.
    pub const fn is_open(self) -> bool {
        !matches!(self.state, EffectProcessorGuiState::Closed)
    }
    /// Whether this render was actually caused by a current user parameter
    /// edit, independently of GUI presence and human-wait permission.
    pub const fn is_user_edited(self) -> bool {
        matches!(
            self.state,
            EffectProcessorGuiState::UserEdited | EffectProcessorGuiState::UserEditedQuestionWork
        )
    }
    /// Explicit GUI work may wait for a real user but grants no extension of
    /// an absolute deadline, author mutation or replacement instance.
    pub const fn allows_question(self) -> bool {
        matches!(
            self.state,
            EffectProcessorGuiState::QuestionWork | EffectProcessorGuiState::UserEditedQuestionWork
        )
    }
}

/// Nonwrapping native service coordinates inside one actual parent namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectProcessorQuestionId {
    native_owner: u64,
    invocation: u64,
    service: u64,
}
impl EffectProcessorQuestionId {
    /// Admit exact nonzero coordinates from the supervised native service slot.
    pub const fn new(native_owner: u64, invocation: u64, service: u64) -> Option<Self> {
        if native_owner == 0 || invocation == 0 || service == 0 {
            None
        } else {
            Some(Self { native_owner, invocation, service })
        }
    }
    /// Child-local callback owner, meaningful only with the parent owner below.
    pub const fn native_owner(self) -> u64 {
        self.native_owner
    }
    /// Exact native Render invocation count, independent of TCP action serial.
    pub const fn invocation(self) -> u64 {
        self.invocation
    }
    /// Nonwrapping service serial, independent of progress generations.
    pub const fn service(self) -> u64 {
        self.service
    }
}

/// Actual complete Question packet from one supervised request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectProcessorLiveQuestion {
    /// Parent-issued processor namespace, not a plugin-local or author UUID.
    pub owner: EffectProcessorOwnerId,
    /// Supervisor-issued execution origin.
    pub origin: EffectProcessorExecutionOrigin,
    /// Exact accepted work/service coordinates.
    pub id: EffectProcessorQuestionId,
    /// Complete bounded plugin-authored UTF-8 question; no truncation/translation.
    pub text: EffectProcessorNotice,
}

/// A question bound only after the actual bank installs this occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectProcessorQuestionReceipt {
    /// Exact prepared occurrence, input format and extent.
    pub preparation: EffectFloatProcessorPreparation,
    /// Immutable provider revision from the sole actual residency key.
    pub provider_revision: u64,
    /// Actual parent namespace and native work/service, without retention claims.
    pub question: EffectProcessorLiveQuestion,
}
impl EffectProcessorQuestionReceipt {
    /// Bounded complete text/inline storage for one product service slot.
    /// Arc/allocator/runtime bookkeeping remains a separately declared frontier.
    pub const fn maximum_retained_bytes() -> usize {
        std::mem::size_of::<Self>()
            + crate::EFFECT_PROCESSOR_NOTICE_TEXT_BYTES
            + crate::EFFECT_PROCESSOR_NOTICE_ID_BYTES
    }
}

/// Allocation-free borrowed bank binding used for each owner-thread poll.
/// Only an actual first publication acquires the bounded owned receipt above.
#[derive(Debug, Clone, Copy)]
pub struct EffectProcessorQuestionView<'a> {
    /// Exact prepared occurrence, input format and extent.
    pub preparation: EffectFloatProcessorPreparation,
    /// Immutable provider revision from the actual bank invocation.
    pub provider_revision: u64,
    /// Complete immutable question owned by the current parent request.
    pub question: &'a EffectProcessorLiveQuestion,
}
impl EffectProcessorQuestionView<'_> {
    /// Acquire one bounded receipt for actual GUI publication. Consumers do
    /// this once per admitted service, never once per Pending poll.
    pub fn to_owned(self) -> EffectProcessorQuestionReceipt {
        EffectProcessorQuestionReceipt {
            preparation: self.preparation,
            provider_revision: self.provider_revision,
            question: self.question.clone(),
        }
    }
    /// Match original ownership without cloning any text or receipt.
    pub fn matches(self, receipt: &EffectProcessorQuestionReceipt) -> bool {
        self.preparation == receipt.preparation
            && self.provider_revision == receipt.provider_revision
            && self.question == &receipt.question
    }
}

/// A genuine real GUI button action. Dismissal is a failed service, never No.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectProcessorQuestionReply {
    /// The current user explicitly chose Yes.
    Yes,
    /// The current user explicitly chose No.
    No,
    /// The question/GUI lost its current lifetime or was explicitly dismissed.
    Dismissed,
}

/// Current product service state sampled only on the owning execution thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectProcessorQuestionState {
    /// No real current GUI can consume this service; native gets Failed.
    Unavailable,
    /// Complete bounded question awaits admission/display. This consumes the
    /// native active fault budget, and never pauses any absolute deadline.
    Pending,
    /// The exact current modal was actually admitted and presented by the Window.
    /// This is only human-wait evidence, not completed render/frame authority.
    Presented,
    /// A real exact GUI response, accepted once for this service.
    Answered(EffectProcessorQuestionReply),
}

/// An actual generic GUI lifetime, independent of a single render's scope.
/// The native bank retains only a weak reference; this is neither authoring
/// authority nor ownership of an instance or execution request.
pub trait EffectProcessorGuiPresence: std::fmt::Debug + Send + Sync {
    /// Whether at least one real current generic editor remains visible.
    fn is_open(&self) -> bool;
    /// Publish an idle action after the sole bank has bound its actual retained
    /// owner. False means the existing bounded product channel was unavailable;
    /// it does not invalidate a legal SDK End reply or processor output.
    fn observe_action(&self, receipt: EffectProcessorGuiActionReceipt) -> bool;
}

/// Native action facts awaiting the bank's exact occurrence/retention binding.
/// This contains no raster or frame-delivery evidence.
#[derive(Debug)]
pub struct EffectProcessorGuiActionFeedback {
    /// Nonzero TCP action serial, separate from native Render invocation.
    pub action_serial: u64,
    /// Supervisor-issued facts from the existing owner.
    pub feedback: crate::EffectProcessorFeedback,
    /// A weak actual product lifetime, never a private instance owner.
    pub presence: std::sync::Weak<dyn EffectProcessorGuiPresence>,
}

/// Bank-bound actual idle GUI action. It never claims another FrameWork.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectProcessorGuiActionReceipt {
    /// Nonzero action serial within this actual parent processor namespace.
    pub action_serial: u64,
    /// The same canonical feedback value and sole retention authority.
    pub receipt: crate::EffectProcessorFeedbackReceipt,
}

/// The product's bounded actual-GUI service, held by the existing observation.
/// Calls stay on the executing owner; no borrowed Rust callback runs on foreign
/// native threads. Implementations own at most one pending service, not a queue,
/// native instance or second graph. The owning bank binds occurrence facts.
pub trait EffectProcessorQuestionService: std::fmt::Debug + Send + Sync {
    /// Known logical shared capacity, charged once in the existing observation.
    fn maximum_storage_bytes(&self) -> usize;
    /// Actual GUI/work admission for the current bank-bound occurrence.
    fn gui_state(
        &self,
        preparation: EffectFloatProcessorPreparation,
        provider_revision: u64,
    ) -> EffectProcessorGuiState;
    /// Borrow factual GUI lifetime for this exact executing occurrence. The
    /// bank keeps a weak reference only and cannot keep a closed view alive.
    /// The owner must be the actual parent-issued identity of this prepared
    /// instance, not an authored effect ID or a previous native incarnation.
    fn gui_presence(
        &self,
        _preparation: EffectFloatProcessorPreparation,
        _provider_revision: u64,
        _owner: EffectProcessorOwnerId,
    ) -> Option<std::sync::Weak<dyn EffectProcessorGuiPresence>> {
        None
    }
    /// Publish or sample the same exact bounded service; repeated Pending polls
    /// must not allocate another owner or publish another question.
    fn question(&self, receipt: EffectProcessorQuestionView<'_>) -> EffectProcessorQuestionState;
    /// Release only this original service on reply, stop, fault or teardown.
    /// A stale completion cannot clear a replacement work's pending question.
    fn finish_question(&self, receipt: EffectProcessorQuestionView<'_>);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_zero_coordinate_can_issue_a_question_service() {
        for coordinates in [(0, 1, 1), (1, 0, 1), (1, 1, 0)] {
            assert!(
                EffectProcessorQuestionId::new(coordinates.0, coordinates.1, coordinates.2)
                    .is_none()
            );
        }
        let id = EffectProcessorQuestionId::new(u64::MAX, u64::MAX, u64::MAX)
            .expect("nonwrapping final service");
        assert_eq!(id.service(), u64::MAX);
    }
    #[test]
    fn gui_presence_does_not_implicitly_grant_human_wait() {
        let closed = EffectProcessorGuiAdmission::CLOSED;
        let open = EffectProcessorGuiAdmission::new(EffectProcessorGuiState::Open);
        let question = EffectProcessorGuiAdmission::new(EffectProcessorGuiState::QuestionWork);
        assert!(!closed.is_open());
        assert!(!closed.allows_question());
        assert!(open.is_open());
        assert!(!open.allows_question());
        assert!(question.is_open());
        assert!(question.allows_question());
    }
}
