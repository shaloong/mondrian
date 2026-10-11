//! The current GUI invocation's active-time budget, with one exact Question.
//!
//! Decoding/publishing a native Question does not pause a fault deadline. Only
//! the real current product modal may acknowledge presentation. An independent
//! scheduler deadline always remains absolute, including while a human waits.
//! This owns no timer thread, queue, author state, processor or reply policy.

use super::question::QuestionIdentity;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QuestionBudgetError {
    Expired,
    WrongQuestion,
    AlreadyWaiting,
    ClockOrder,
    DeadlineOverflow,
}

#[derive(Debug, Clone, Copy)]
enum Phase {
    Active {
        deadline: Instant,
    },
    HumanWaiting {
        identity: QuestionIdentity,
        since: Instant,
        remaining: Duration,
    },
}

pub(super) struct QuestionExecutionBudget {
    phase: Phase,
    scheduler_deadline: Option<Instant>,
}
impl QuestionExecutionBudget {
    pub(super) fn new(active_deadline: Instant, scheduler_deadline: Option<Instant>) -> Self {
        Self {
            phase: Phase::Active { deadline: active_deadline },
            scheduler_deadline,
        }
    }

    pub(super) fn check(&self, now: Instant) -> Result<(), QuestionBudgetError> {
        if self.scheduler_deadline.is_some_and(|deadline| now >= deadline) {
            return Err(QuestionBudgetError::Expired);
        }
        match self.phase {
            Phase::Active { deadline } if now >= deadline => Err(QuestionBudgetError::Expired),
            Phase::HumanWaiting { since, .. } if now < since => {
                Err(QuestionBudgetError::ClockOrder)
            }
            _ => Ok(()),
        }
    }

    /// Called only for the admitted, actually visible modal's exact service.
    /// Native packet receipt, queue publication and authored address alone must
    /// never call this method. The caller retains the real product proof.
    pub(super) fn presented(
        &mut self,
        identity: QuestionIdentity,
        now: Instant,
    ) -> Result<(), QuestionBudgetError> {
        self.check(now)?;
        let Phase::Active { deadline } = self.phase else {
            return Err(QuestionBudgetError::AlreadyWaiting);
        };
        self.phase = Phase::HumanWaiting {
            identity,
            since: now,
            remaining: deadline.saturating_duration_since(now),
        };
        Ok(())
    }

    /// Resume the exact original active remainder before submitting a real
    /// answer. No new full budget is created, and a late reply cannot release
    /// the current Question or consume another service's remaining duration.
    pub(super) fn answered(
        &mut self,
        identity: QuestionIdentity,
        now: Instant,
    ) -> Result<(), QuestionBudgetError> {
        self.check(now)?;
        let Phase::HumanWaiting { identity: current, remaining, .. } = self.phase else {
            return Err(QuestionBudgetError::WrongQuestion);
        };
        if current != identity {
            return Err(QuestionBudgetError::WrongQuestion);
        }
        let deadline = now.checked_add(remaining).ok_or(QuestionBudgetError::DeadlineOverflow)?;
        self.phase = Phase::Active { deadline };
        Ok(())
    }

    /// Existing writes/polls use this deadline. None is legal only during a
    /// proved human wait with no external scheduler deadline; owner cancellation
    /// and actual process exit still require polling on that path.
    pub(super) fn deadline(&self) -> Option<Instant> {
        let active = match self.phase {
            Phase::Active { deadline } => Some(deadline),
            Phase::HumanWaiting { .. } => None,
        };
        match (active, self.scheduler_deadline) {
            (Some(first), Some(second)) => Some(first.min(second)),
            (first, second) => first.or(second),
        }
    }

    /// Cooperative cancel grace is always bounded. A paused GUI budget contributes
    /// its remaining active duration from now; an absolute owner deadline clips
    /// it exactly. Cancellation never waits for a human response.
    pub(super) fn cancellation_deadline(
        &self,
        now: Instant,
        grace: Duration,
    ) -> Result<Instant, QuestionBudgetError> {
        let grace_deadline = now.checked_add(grace).ok_or(QuestionBudgetError::DeadlineOverflow)?;
        let active = match self.phase {
            Phase::Active { deadline } => deadline,
            Phase::HumanWaiting { since, remaining, .. } => {
                if now < since {
                    return Err(QuestionBudgetError::ClockOrder);
                }
                now.checked_add(remaining).ok_or(QuestionBudgetError::DeadlineOverflow)?
            }
        };
        Ok(
            self.scheduler_deadline.map_or(active.min(grace_deadline), |owner| {
                owner.min(active).min(grace_deadline)
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity(service: u64) -> QuestionIdentity {
        QuestionIdentity { native_owner: 11, invocation: 7, service }
    }
    fn plus(start: Instant, seconds: u64) -> Instant {
        start + Duration::from_secs(seconds)
    }

    #[test]
    fn decoded_or_unconsumed_question_never_pauses_the_current_active_budget() {
        let start = Instant::now();
        let budget = QuestionExecutionBudget::new(plus(start, 10), None);
        assert_eq!(budget.deadline(), Some(plus(start, 10)));
        assert_eq!(
            budget.check(plus(start, 10)),
            Err(QuestionBudgetError::Expired)
        );
    }
    #[test]
    fn real_original_presentation_pauses_only_its_remaining_active_duration() {
        let start = Instant::now();
        let mut budget = QuestionExecutionBudget::new(plus(start, 10), None);
        budget.presented(identity(1), plus(start, 3)).expect("real current modal proof");
        assert_eq!(budget.deadline(), None);
        budget.check(plus(start, 100)).expect("only actual human waiting is excluded");
        budget
            .answered(identity(1), plus(start, 100))
            .expect("real exact button response");
        assert_eq!(budget.deadline(), Some(plus(start, 107)));
        assert_eq!(
            budget.check(plus(start, 107)),
            Err(QuestionBudgetError::Expired)
        );
    }
    #[test]
    fn external_preview_deadline_stays_absolute_during_human_wait() {
        let start = Instant::now();
        let mut budget = QuestionExecutionBudget::new(plus(start, 10), Some(plus(start, 5)));
        budget.presented(identity(1), plus(start, 3)).expect("current presentation");
        assert_eq!(budget.deadline(), Some(plus(start, 5)));
        assert_eq!(
            budget.check(plus(start, 5)),
            Err(QuestionBudgetError::Expired)
        );
        assert_eq!(
            budget.answered(identity(1), plus(start, 6)),
            Err(QuestionBudgetError::Expired)
        );
    }
    #[test]
    fn stale_answer_does_not_release_or_refresh_the_next_question() {
        let start = Instant::now();
        let mut budget = QuestionExecutionBudget::new(plus(start, 10), None);
        budget.presented(identity(1), plus(start, 2)).expect("first visible question");
        budget.answered(identity(1), plus(start, 20)).expect("first genuine reply");
        budget.presented(identity(2), plus(start, 23)).expect("next visible question");
        assert_eq!(
            budget.answered(identity(1), plus(start, 40)),
            Err(QuestionBudgetError::WrongQuestion)
        );
        assert_eq!(budget.deadline(), None);
        budget.answered(identity(2), plus(start, 41)).expect("next genuine reply");
        assert_eq!(
            budget.deadline(),
            Some(plus(start, 46)),
            "only five active seconds remained"
        );
    }
    #[test]
    fn cancel_while_waiting_keeps_original_remainder_and_clipped_grace() {
        let start = Instant::now();
        let mut budget = QuestionExecutionBudget::new(plus(start, 10), None);
        budget
            .presented(identity(1), plus(start, 9))
            .expect("visible original question");
        assert_eq!(
            budget
                .cancellation_deadline(plus(start, 100), Duration::from_secs(2))
                .expect("bounded original grace"),
            plus(start, 101)
        );
        let mut bounded = QuestionExecutionBudget::new(plus(start, 10), Some(plus(start, 5)));
        bounded.presented(identity(1), plus(start, 3)).expect("absolute owner remains");
        assert_eq!(
            bounded
                .cancellation_deadline(plus(start, 4), Duration::from_secs(2))
                .expect("owner-clipped grace"),
            plus(start, 5)
        );
    }
}
