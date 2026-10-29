//! One bounded service field inside the existing native invocation slot.
//!
//! This type owns no mutex, thread, channel, request queue or native instance.
//! The sole pump's Shared gate/Condvar remains the lifetime/correlation owner.
//! Only its current executing interactive request may reserve this field.

use super::super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QuestionIdentity {
    pub(super) native_owner: u64,
    pub(super) invocation: u64,
    pub(super) service: u64,
}

/// Yes/No are genuine admitted user actions. Closing/loss is a service failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum QuestionReply {
    Yes,
    No,
    Dismissed,
}
impl QuestionReply {
    pub(super) const fn native_status(self) -> c_int {
        match self {
            Self::Yes => 12,
            Self::No => 13,
            Self::Dismissed => 1,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct PendingQuestion {
    pub(super) identity: QuestionIdentity,
    pub(super) value: NativeLiveQuestion,
    published: bool,
    answer: Option<QuestionReply>,
}

#[derive(Default)]
pub(super) struct QuestionSlot {
    next_service: u64,
    pending: Option<PendingQuestion>,
}

impl QuestionSlot {
    /// Called only after Shared admits the same current executing request.
    /// Fixed POD copy; callbacks allocate no text, receipt or frame owner.
    pub(super) fn reserve(&mut self, value: NativeLiveQuestion) -> Option<QuestionIdentity> {
        if self.pending.is_some() || !valid_native_question(&value) {
            return None;
        }
        let service = self.next_service.checked_add(1)?;
        let identity = QuestionIdentity {
            native_owner: value.owner,
            invocation: value.invocation,
            service,
        };
        self.next_service = service;
        self.pending = Some(PendingQuestion { identity, value, published: false, answer: None });
        Some(identity)
    }

    /// Borrow complete POD for the sole pump's bounded packet encoder.
    pub(super) fn unpublished(&self) -> Option<PendingQuestion> {
        self.pending.filter(|pending| !pending.published && pending.answer.is_none())
    }

    /// Publication means the entire original packet committed on that pump.
    pub(super) fn mark_published(&mut self, identity: QuestionIdentity) -> bool {
        let Some(pending) = self.pending.as_mut().filter(|value| value.identity == identity) else {
            return false;
        };
        if pending.published {
            return false;
        }
        pending.published = true;
        true
    }

    /// The caller additionally checks parent namespace and work sequence.
    /// An uncommitted, duplicate or stale answer cannot consume a later service.
    pub(super) fn answer(&mut self, identity: QuestionIdentity, answer: QuestionReply) -> bool {
        let Some(pending) = self.pending.as_mut().filter(|value| value.identity == identity) else {
            return false;
        };
        if !pending.published || pending.answer.is_some() {
            return false;
        }
        pending.answer = Some(answer);
        true
    }

    /// The original callback alone consumes its terminal answer.
    pub(super) fn take_answer(&mut self, identity: QuestionIdentity) -> Option<QuestionReply> {
        let answer = self.pending.filter(|value| value.identity == identity)?.answer?;
        self.pending = None;
        Some(answer)
    }

    /// Cancel/shutdown releases only this original callback's borrowed service.
    pub(super) fn abandon(&mut self, identity: QuestionIdentity) -> bool {
        if self.pending.is_some_and(|value| value.identity == identity) {
            self.pending = None;
            true
        } else {
            false
        }
    }

    /// The original callback must stop if its exact reservation disappeared.
    /// Absence is failure, never a reason to wait for a different service.
    pub(super) fn contains(&self, identity: QuestionIdentity) -> bool {
        self.pending.is_some_and(|value| value.identity == identity)
    }

    pub(super) fn is_active(&self) -> bool {
        self.pending.is_some()
    }
}

fn valid_native_question(value: &NativeLiveQuestion) -> bool {
    if value.owner == 0 || value.invocation == 0 || value.text.kind != 1 {
        return false;
    }
    valid_text(&value.text.text, value.text.text_length)
        && valid_text(&value.text.id, value.text.id_length)
}

fn valid_text<const N: usize>(value: &[c_char; N], length: usize) -> bool {
    if length >= N || value[length] != 0 {
        return false;
    }
    // SAFETY: c_char is one byte. The fixed POD is live and immutable during
    // this callback, so a temporary borrowed byte view needs no heap owner.
    let bytes = unsafe { std::slice::from_raw_parts(value.as_ptr().cast::<u8>(), length) };
    !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(invocation: u64) -> NativeLiveQuestion {
        NativeLiveQuestion {
            owner: 7,
            invocation,
            text: NativeNotice {
                kind: 1,
                text_length: 1,
                id_length: 0,
                text: {
                    let mut bytes = [0; NATIVE_NOTICE_TEXT_BYTES + 1];
                    bytes[0] = b'?' as c_char;
                    bytes
                },
                id: [0; NATIVE_NOTICE_ID_BYTES + 1],
            },
        }
    }

    #[test]
    fn one_slot_requires_real_packet_commit_then_one_exact_answer() {
        let mut slot = QuestionSlot::default();
        let identity = slot.reserve(value(3)).expect("one current service");
        assert!(
            slot.reserve(value(3)).is_none(),
            "concurrent second Question must fail, never queue"
        );
        assert!(
            !slot.answer(identity, QuestionReply::Yes),
            "a pre-publication answer is not user delivery proof"
        );
        assert!(slot.mark_published(identity));
        assert!(slot.answer(identity, QuestionReply::No));
        assert!(!slot.answer(identity, QuestionReply::Yes));
        assert_eq!(slot.take_answer(identity), Some(QuestionReply::No));
        assert!(!slot.is_active());
        assert!(slot.take_answer(identity).is_none());
    }

    #[test]
    fn late_cancel_or_answer_cannot_consume_the_next_work_service() {
        let mut slot = QuestionSlot::default();
        let old = slot.reserve(value(3)).expect("original service");
        assert!(slot.abandon(old));
        let next = slot.reserve(value(4)).expect("later exact work");
        assert!(next.service > old.service);
        assert!(!slot.abandon(old));
        assert!(!slot.mark_published(old));
        assert!(!slot.answer(old, QuestionReply::Yes));
        assert!(slot.mark_published(next));
        assert!(slot.answer(next, QuestionReply::Dismissed));
        assert_eq!(slot.take_answer(next), Some(QuestionReply::Dismissed));
    }

    #[test]
    fn counter_exhaustion_refuses_service_without_wrapping_or_replacing_owner() {
        let mut slot = QuestionSlot { next_service: u64::MAX - 1, pending: None };
        let final_service = slot.reserve(value(1)).expect("last nonwrapping identity");
        assert_eq!(final_service.service, u64::MAX);
        assert!(slot.abandon(final_service));
        assert!(slot.reserve(value(2)).is_none());
        assert!(!slot.is_active());
    }

    #[test]
    fn complete_utf8_and_bounds_are_admitted_before_any_service_identity() {
        let mut slot = QuestionSlot::default();
        for malformed in [0, 1, 2, 3, 4] {
            let mut input = value(1);
            match malformed {
                0 => input.owner = 0,
                1 => input.invocation = 0,
                2 => input.text.text_length = NATIVE_NOTICE_TEXT_BYTES + 1,
                3 => input.text.text[0] = 0xff_u8 as c_char,
                4 => input.text.text[0] = 0,
                _ => unreachable!(),
            }
            assert!(slot.reserve(input).is_none());
        }
        assert_eq!(slot.reserve(value(1)).expect("valid service").service, 1);
    }
}
