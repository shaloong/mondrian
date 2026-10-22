//! Current-work projection of bounded native facts; this owns no processor.

use super::notifications::{AppNotificationFeed, AppNotificationSeverity};
use super::preview_visual_execution_task::VisualExecutionTaskKey;
use super::AppMessage;
use mondrian_effects::{
    EffectProcessorFeedbackReceipt, EffectProcessorNotice, EffectProcessorNoticeKind,
};
use mondrian_playback::PlaybackEpoch;

/// Complete identities issued by the existing heterogeneous/scalar routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewProcessorWorkKey {
    Visual(VisualExecutionTaskKey),
    Scalar {
        sequence_id: mondrian_core::SequenceId,
        width: u32,
        height: u32,
        plan_identity: super::preview_execution::PreviewSemanticIdentity,
    },
}

/// Exact current Viewer attempt selected by the production Preview adapter.
/// Sampling this identity is not successful frame/presentation evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewProcessorFeedbackScope {
    /// None retires a previous view; it is not a fabricated executed frame.
    pub(crate) key: Option<PreviewProcessorWorkKey>,
    pub(crate) generation: u64,
    pub(crate) epoch: PlaybackEpoch,
}

impl PreviewProcessorFeedbackScope {
    pub(crate) fn scalar(
        key: &super::preview_execution::PreviewOutputKey,
        generation: u64,
        epoch: PlaybackEpoch,
    ) -> Self {
        Self {
            key: Some(PreviewProcessorWorkKey::Scalar {
                sequence_id: key.sequence_id,
                width: key.width,
                height: key.height,
                plan_identity: key.plan_identity,
            }),
            generation,
            epoch,
        }
    }
}

/// An atomic bounded metadata handoff from the actual current invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreviewProcessorFeedbackUpdate {
    pub(crate) scope: PreviewProcessorFeedbackScope,
    pub(crate) feedback: mondrian_effects::EffectProcessorFeedbackBatch,
}

/// Same existing work/result transport, without frame completion evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreviewProcessorProgressUpdate {
    pub(crate) scope: PreviewProcessorFeedbackScope,
    /// None means this exact native preparation task ended, not that its frame
    /// or GPU suffix was published. A stale task cannot clear current progress.
    pub(crate) receipt: Option<mondrian_effects::EffectProcessorProgressReceipt>,
}

type ProgressPublication = dyn Fn(&mondrian_effects::EffectProcessorProgressReceipt) + Send + Sync;

/// An observer publishes into the task's EXISTING bounded result channel.
/// No second queue, reader, render session or authoring authority is created.
pub(crate) struct PreviewProcessorProgressPublisher {
    publication: Box<ProgressPublication>,
    capacity_bytes: usize,

    question_service: Option<std::sync::Arc<dyn mondrian_effects::EffectProcessorQuestionService>>,
}
impl std::fmt::Debug for PreviewProcessorProgressPublisher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreviewProcessorProgressPublisher")
            .field("capacity_bytes", &self.capacity_bytes)
            .finish_non_exhaustive()
    }
}
impl PreviewProcessorProgressPublisher {
    pub(crate) fn new_observer(
        result_slots: usize,
        publication: impl Fn(&mondrian_effects::EffectProcessorProgressReceipt) + Send + Sync + 'static,
    ) -> std::sync::Arc<dyn mondrian_effects::EffectProcessorObserver> {
        // Queue slots, producer temporary and current UI observation can all
        // coexist. Existing raster queue storage is owned by the task already;
        // this adds only complete bounded progress metadata to route admission.
        let capacity_bytes = result_slots
            .saturating_add(2)
            .saturating_mul(
                mondrian_effects::EffectProcessorProgressReceipt::maximum_retained_bytes(),
            )
            .saturating_add(std::mem::size_of::<Self>())
            .saturating_add(std::mem::size_of_val(&publication));
        std::sync::Arc::new(Self {
            publication: Box::new(publication),
            capacity_bytes,
            question_service: None,
        })
    }
}
impl PreviewProcessorProgressPublisher {
    pub(crate) fn with_question_service(
        result_slots: usize,
        question_service: std::sync::Arc<dyn mondrian_effects::EffectProcessorQuestionService>,
        publication: impl Fn(&mondrian_effects::EffectProcessorProgressReceipt) + Send + Sync + 'static,
    ) -> std::sync::Arc<dyn mondrian_effects::EffectProcessorObserver> {
        let capacity_bytes = result_slots
            .saturating_add(2)
            .saturating_mul(
                mondrian_effects::EffectProcessorProgressReceipt::maximum_retained_bytes(),
            )
            .saturating_add(std::mem::size_of::<Self>())
            .saturating_add(std::mem::size_of_val(&publication))
            .saturating_add(question_service.maximum_storage_bytes());
        std::sync::Arc::new(Self {
            publication: Box::new(publication),
            capacity_bytes,
            question_service: Some(question_service),
        })
    }
}
impl mondrian_effects::EffectProcessorObserver for PreviewProcessorProgressPublisher {
    fn maximum_storage_bytes(&self) -> usize {
        self.capacity_bytes
    }
    fn observe(&self, receipt: &mondrian_effects::EffectProcessorProgressReceipt) {
        (self.publication)(receipt);
    }
    fn question_service(&self) -> Option<&dyn mondrian_effects::EffectProcessorQuestionService> {
        self.question_service.as_deref()
    }
}

/// In-session observational state. Canonical Effects retention is carried as
/// evidence; no UI reference keeps an evicted private instance alive.
#[derive(Debug, Default)]
pub(crate) struct PreviewProcessorFeedbackState {
    scope: Option<PreviewProcessorFeedbackScope>,
    feedback: mondrian_effects::EffectProcessorFeedbackBatch,
    gui_action_serials: [(Option<mondrian_core::EffectProcessorOwnerId>, u64);
        mondrian_effects::EFFECT_PROCESSOR_FEEDBACK_SLOTS],
    live: Option<PreviewProcessorProgressUpdate>,
    ended: Option<(
        PreviewProcessorFeedbackScope,
        mondrian_core::EffectProcessorOwnerId,
        u64,
    )>,
}

impl PreviewProcessorFeedbackState {
    /// Observe the existing current result of the exact original native call.
    /// No raster copy, frame interpretation, alternate task or completion store.
    #[cfg(feature = "validation")]
    pub(crate) fn validation_question_completion(
        &self,
        scope: PreviewProcessorFeedbackScope,
        question: &mondrian_effects::EffectProcessorQuestionReceipt,
        effect: mondrian_core::EffectId,
    ) -> Option<&EffectProcessorFeedbackReceipt> {
        if self.scope != Some(scope) {
            return None;
        }
        self.feedback.iter().find(|receipt| {
            receipt.effect_id == effect
                && receipt.preparation == question.preparation
                && receipt.provider_revision == question.provider_revision
                && receipt.feedback.owner == question.question.owner
                && receipt.feedback.origin == question.question.origin
                && receipt.feedback.snapshot.native_owner() == question.question.id.native_owner()
                && receipt.feedback.snapshot.invocation() == question.question.id.invocation()
        })
    }

    /// Observe a bank-bound idle GUI action only for an already observed exact
    /// parent/native owner, recipe, input format and extent. No old frame scope
    /// is promoted to current execution or presentation evidence.
    pub(crate) fn observe_gui_action(
        &mut self,
        action: mondrian_effects::EffectProcessorGuiActionReceipt,
        feed: &mut AppNotificationFeed,
    ) -> bool {
        let receipt = &action.receipt;
        let Some((index, previous)) = self.feedback.iter().enumerate().find(|(_, old)| {
            old.preparation == receipt.preparation
                && old.provider_revision == receipt.provider_revision
                && old.feedback.owner == receipt.feedback.owner
                && old.feedback.origin == receipt.feedback.origin
        }) else {
            return false;
        };
        if action.action_serial == 0
            || receipt.retention != mondrian_effects::EffectProcessorRetention::Retained
            || previous.retention != mondrian_effects::EffectProcessorRetention::Retained
            || previous.feedback.snapshot.native_owner() != receipt.feedback.snapshot.native_owner()
            || previous.feedback.snapshot.invocation() != receipt.feedback.snapshot.invocation()
            || previous.feedback.snapshot.persistent_revision()
                > receipt.feedback.snapshot.persistent_revision()
            || (self.gui_action_serials[index].0 == Some(receipt.feedback.owner)
                && self.gui_action_serials[index].1 >= action.action_serial)
        {
            return false;
        }
        for notice in receipt.feedback.snapshot.notices() {
            publish_notice(feed, receipt, notice, "gui_action");
        }
        if let Some(notice) = receipt.feedback.snapshot.persistent()
            && previous.feedback.snapshot.persistent_revision()
                < receipt.feedback.snapshot.persistent_revision()
        {
            publish_notice(feed, receipt, notice, "gui_action_persistent");
        }
        self.gui_action_serials[index] = (Some(receipt.feedback.owner), action.action_serial);
        self.feedback.replace_current(action.receipt)
    }

    /// Only exact current Runtime work may expose an executing observation.
    /// Author/epoch/provider/actual-owner/request/revision checks precede UI.
    pub(crate) fn observe_progress(
        &mut self,
        update: PreviewProcessorProgressUpdate,
        active_epoch: PlaybackEpoch,
    ) -> bool {
        if update.scope.epoch != active_epoch
            || update.scope.key.is_none()
            || self.scope.is_some_and(|old| {
                old.epoch == active_epoch && old.generation > update.scope.generation
            })
            || self.live.as_ref().is_some_and(|old| {
                old.scope.epoch == active_epoch && old.scope.generation > update.scope.generation
            })
        {
            return false;
        }
        let Some(receipt) = &update.receipt else {
            if self.live.as_ref().is_some_and(|live| live.scope == update.scope) {
                self.ended =
                    self.live.as_ref().and_then(|live| live.receipt.as_ref()).map(|receipt| {
                        (
                            update.scope,
                            receipt.progress.owner,
                            receipt.progress.invocation,
                        )
                    });
                self.live = None;
                return true;
            }
            return false;
        };
        if receipt.progress.owner.0.is_nil()
            || receipt.progress.native_owner == 0
            || receipt.progress.invocation == 0
            || receipt.progress.revision == 0
            || self.ended.is_some_and(|(scope, owner, invocation)| {
                scope == update.scope
                    && owner == receipt.progress.owner
                    && invocation >= receipt.progress.invocation
            })
            || self.feedback.iter().any(|old| {
                old.effect_id == receipt.preparation.effect_id
                    && (old.provider_revision > receipt.provider_revision
                        || (old.provider_revision == receipt.provider_revision
                            && old.feedback.owner == receipt.progress.owner
                            && old.feedback.snapshot.invocation() >= receipt.progress.invocation))
            })
        {
            return false;
        }
        if let Some(old) = self.live.as_ref().and_then(|live| live.receipt.as_ref())
            && old.preparation.effect_id == receipt.preparation.effect_id
            && (old.provider_revision > receipt.provider_revision
                || (old.progress.owner == receipt.progress.owner
                    && (old.progress.native_owner != receipt.progress.native_owner
                        || old.progress.invocation > receipt.progress.invocation
                        || (old.progress.invocation == receipt.progress.invocation
                            && old.progress.revision >= receipt.progress.revision))))
        {
            return false;
        }
        self.live = Some(update);
        true
    }

    pub(crate) fn active_progress(
        &self,
    ) -> Option<&mondrian_effects::EffectProcessorProgressReceipt> {
        self.live
            .as_ref()?
            .receipt
            .as_ref()
            .filter(|receipt| receipt.progress.progress.active())
    }

    /// Project only the adapter's exact current scope. Stale native generations
    /// never overwrite current state or publish new notifications.
    pub(crate) fn observe(
        &mut self,
        update: PreviewProcessorFeedbackUpdate,
        active_epoch: PlaybackEpoch,
        feed: &mut AppNotificationFeed,
    ) -> bool {
        if update.scope.epoch != active_epoch
            || (update.scope.key.is_none() && !update.feedback.is_empty())
            || self.scope.is_some_and(|old| {
                old.epoch == active_epoch && old.generation > update.scope.generation
            })
            || update.feedback.iter().any(|receipt| {
                self.feedback.iter().any(|old| {
                    old.effect_id == receipt.effect_id
                        && old.provider_revision == receipt.provider_revision
                        && old.feedback.owner == receipt.feedback.owner
                        && (old.feedback.snapshot.invocation()
                            > receipt.feedback.snapshot.invocation()
                            || old.feedback.snapshot.persistent_revision()
                                > receipt.feedback.snapshot.persistent_revision()
                            || old.feedback.snapshot.progress().is_some_and(|old_progress| {
                                receipt.feedback.snapshot.progress().is_some_and(|progress| {
                                    old_progress.generation() > progress.generation()
                                })
                            }))
                })
            })
        {
            return false;
        }
        // An exact current plan change replaces the view, including removal of
        // effects. It is not a claim that a previously evicted owner recovered.
        let same_scope = self.scope == Some(update.scope);
        let mut changed = !same_scope;
        for receipt in update.feedback.iter() {
            let previous = self.feedback.iter().find(|old| {
                old.effect_id == receipt.effect_id
                    && old.provider_revision == receipt.provider_revision
                    && old.feedback.owner == receipt.feedback.owner
            });
            if previous.is_some_and(|old| {
                old.feedback.snapshot.invocation() >= receipt.feedback.snapshot.invocation()
            }) {
                continue;
            }
            changed = true;
            for notice in receipt.feedback.snapshot.notices() {
                publish_notice(feed, receipt, notice, "transient");
            }
            // Persistent errors remain a typed Viewer-unavailable state. Their
            // informational toast is secondary and never retires the owner.
            if let Some(notice) = receipt.feedback.snapshot.persistent()
                && previous.is_none_or(|old| {
                    old.feedback.snapshot.persistent_revision()
                        < receipt.feedback.snapshot.persistent_revision()
                })
            {
                publish_notice(feed, receipt, notice, "persistent");
            }
        }
        if changed {
            if self.live.as_ref().is_some_and(|live| {
                live.scope != update.scope
                    || live.receipt.as_ref().is_some_and(|progress| {
                        update.feedback.iter().any(|receipt| {
                            receipt.feedback.owner == progress.progress.owner
                                && receipt.feedback.snapshot.invocation()
                                    >= progress.progress.invocation
                        })
                    })
            }) {
                self.live = None;
            }
            self.scope = Some(update.scope);
            let old_actions = self.gui_action_serials;
            self.gui_action_serials = std::array::from_fn(|index| {
                update.feedback.iter().nth(index).map_or((None, 0), |receipt| {
                    old_actions
                        .iter()
                        .find(|(owner, _)| *owner == Some(receipt.feedback.owner))
                        .copied()
                        .unwrap_or((Some(receipt.feedback.owner), 0))
                })
            });
            self.feedback = update.feedback;
        }
        changed
    }
}

fn publish_notice(
    feed: &mut AppNotificationFeed,
    receipt: &EffectProcessorFeedbackReceipt,
    notice: &EffectProcessorNotice,
    source: &str,
) {
    let severity = match notice.kind() {
        EffectProcessorNoticeKind::Information => AppNotificationSeverity::Information,
        EffectProcessorNoticeKind::Warning => AppNotificationSeverity::Warning,
        EffectProcessorNoticeKind::Error => AppNotificationSeverity::Error,
        EffectProcessorNoticeKind::Log | EffectProcessorNoticeKind::Fatal => return,
    };
    // Stable occurrence/owner/notice identity coalesces repeated plugin text;
    // these diagnostic IDs are never shown to the user or authoring authority.
    let key = format!(
        "processor:{}:{}:{source}:{:?}:{}",
        receipt.effect_id,
        receipt.feedback.owner,
        notice.kind(),
        notice.id()
    );
    feed.publish(
        key,
        severity,
        AppMessage::new("notification-native-processor").with_text("message", notice.text()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{EffectId, EffectProcessorOwnerId};
    use mondrian_effects::{
        EffectFloatProcessorPreparation, EffectProcessorExecutionOrigin, EffectProcessorFeedback,
        EffectProcessorFeedbackBatch, EffectProcessorFeedbackSnapshot, EffectProcessorRetention,
    };

    fn epochs() -> (PlaybackEpoch, PlaybackEpoch) {
        let sequence = mondrian_timeline::Sequence::new("native feedback observation");
        let mut engine =
            mondrian_playback::PlaybackEngine::new(sequence.time_base(), Default::default())
                .expect("actual transport owner");
        let first = engine.snapshot().epoch;
        let binding = mondrian_playback::PlaybackTimelineBinding::new(
            Some(sequence.id),
            1,
            sequence.time_base(),
            100,
        )
        .expect("canonical timeline binding");
        let next = engine
            .seek_timeline(
                binding,
                mondrian_core::FramePosition::new(0, sequence.time_base()),
                mondrian_playback::MonotonicTimestamp::ZERO,
            )
            .expect("actual epoch transition")
            .epoch;
        assert_ne!(first, next);
        (first, next)
    }
    fn scope(epoch: PlaybackEpoch, generation: u64) -> PreviewProcessorFeedbackScope {
        PreviewProcessorFeedbackScope {
            key: Some(PreviewProcessorWorkKey::Visual(
                VisualExecutionTaskKey::from_complete_semantic_fingerprint([7; 32]),
            )),
            generation,
            epoch,
        }
    }
    fn receipt(
        effect_id: EffectId,
        owner: EffectProcessorOwnerId,
        invocation: u64,
        revision: u64,
        persistent: Option<EffectProcessorNotice>,
        notices: Vec<EffectProcessorNotice>,
        retention: EffectProcessorRetention,
    ) -> EffectProcessorFeedbackReceipt {
        EffectProcessorFeedbackReceipt {
            effect_id,
            preparation: EffectFloatProcessorPreparation {
                input_role: mondrian_effects::EffectFloatProcessorInputRole::SingleImage,
                effect_id,
                width: 1,
                height: 1,
                format: mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),

                project_geometry: None,
            },
            provider_revision: 3,
            retention,
            feedback: EffectProcessorFeedback {
                owner,
                origin: EffectProcessorExecutionOrigin::InProcess,
                snapshot: EffectProcessorFeedbackSnapshot::new(
                    1, invocation, revision, persistent, None, notices,
                )
                .expect("actual bounded lifecycle"),
            },
        }
    }
    fn update(
        scope: PreviewProcessorFeedbackScope,
        receipt: EffectProcessorFeedbackReceipt,
    ) -> PreviewProcessorFeedbackUpdate {
        let mut feedback = EffectProcessorFeedbackBatch::new();
        feedback.push(receipt).expect("admitted slot");
        PreviewProcessorFeedbackUpdate { scope, feedback }
    }
    fn notice(kind: EffectProcessorNoticeKind, text: &str) -> EffectProcessorNotice {
        EffectProcessorNotice::new(kind, text.to_owned(), "native-operation".to_owned())
            .expect("complete authored UTF-8")
    }

    #[test]
    fn idle_action_notices_require_current_retained_owner_and_cannot_be_replayed_as_frames() {
        let (epoch, _) = epochs();
        let effect = EffectId::new();
        let owner = EffectProcessorOwnerId::new();
        let mut state = PreviewProcessorFeedbackState::default();
        let mut feed = AppNotificationFeed::default();
        let current_scope = scope(epoch, 10);
        let original = receipt(
            effect,
            owner,
            1,
            0,
            None,
            vec![],
            EffectProcessorRetention::Retained,
        );
        assert!(state.observe(update(current_scope, original.clone()), epoch, &mut feed));
        let ended = receipt(
            effect,
            owner,
            1,
            0,
            None,
            vec![notice(EffectProcessorNoticeKind::Warning, "Actual End 1")],
            EffectProcessorRetention::Retained,
        );
        let action =
            mondrian_effects::EffectProcessorGuiActionReceipt { action_serial: 2, receipt: ended };
        assert!(state.observe_gui_action(action.clone(), &mut feed));
        assert_eq!(
            state.scope,
            Some(current_scope),
            "no new frame scope was manufactured"
        );
        assert_eq!(
            state
                .feedback
                .iter()
                .next()
                .expect("same actual owner")
                .feedback
                .snapshot
                .invocation(),
            1
        );
        assert_eq!(feed.iter().count(), 1);
        assert!(
            !state.observe_gui_action(action.clone(), &mut feed),
            "duplicate action cannot notify twice"
        );
        let mut invalid = action.clone();
        invalid.receipt.feedback.owner = EffectProcessorOwnerId::new();
        assert!(!state.observe_gui_action(invalid, &mut feed));
        let mut invalid = action.clone();
        invalid.action_serial = 3;
        invalid.receipt.provider_revision += 1;
        assert!(!state.observe_gui_action(invalid, &mut feed));
        let mut invalid = action.clone();
        invalid.action_serial = 3;
        invalid.receipt.retention = EffectProcessorRetention::Evicted;
        assert!(!state.observe_gui_action(invalid, &mut feed));
        assert!(state.observe(
            update(
                scope(epoch, 11),
                receipt(
                    effect,
                    owner,
                    2,
                    0,
                    None,
                    vec![],
                    EffectProcessorRetention::Retained
                )
            ),
            epoch,
            &mut feed
        ));
        let mut late = action;
        late.action_serial = 4;
        assert!(
            !state.observe_gui_action(late, &mut feed),
            "old native Render cannot overwrite current native state"
        );
        assert_eq!(feed.iter().count(), 1);
    }

    #[test]
    fn stale_native_revision_or_author_generation_publishes_nothing() {
        let (epoch, other_epoch) = epochs();
        let effect = EffectId::new();
        let owner = EffectProcessorOwnerId::new();
        let mut state = PreviewProcessorFeedbackState::default();
        let mut feed = AppNotificationFeed::default();
        assert!(state.observe(
            update(
                scope(epoch, 10),
                receipt(
                    effect,
                    owner,
                    4,
                    4,
                    Some(notice(EffectProcessorNoticeKind::Error, "当前错误 α")),
                    vec![],
                    EffectProcessorRetention::Retained
                )
            ),
            epoch,
            &mut feed
        ));
        let count = feed.iter().count();
        for (generation, invocation, revision) in [(9, 5, 5), (10, 3, 5), (10, 5, 3)] {
            assert!(!state.observe(
                update(
                    scope(epoch, generation),
                    receipt(
                        effect,
                        owner,
                        invocation,
                        revision,
                        Some(notice(EffectProcessorNoticeKind::Error, "stale error")),
                        vec![],
                        EffectProcessorRetention::Retained
                    )
                ),
                epoch,
                &mut feed
            ));
            assert_eq!(
                feed.iter().count(),
                count,
                "stale facts have no notification side effects"
            );
            assert_eq!(
                state
                    .feedback
                    .iter()
                    .next()
                    .expect("current state")
                    .feedback
                    .snapshot
                    .invocation(),
                4
            );
        }
        assert!(!state.observe(
            update(
                scope(other_epoch, 11),
                receipt(
                    effect,
                    owner,
                    5,
                    5,
                    None,
                    vec![],
                    EffectProcessorRetention::Retained
                )
            ),
            epoch,
            &mut feed
        ));
        assert_eq!(feed.iter().count(), count);
    }

    #[test]
    fn newer_clear_replaces_blocked_state_without_fabricating_private_retention() {
        let (epoch, _) = epochs();
        let effect = EffectId::new();
        let old_owner = EffectProcessorOwnerId::new();
        let mut state = PreviewProcessorFeedbackState::default();
        let mut feed = AppNotificationFeed::default();
        state.observe(
            update(
                scope(epoch, 1),
                receipt(
                    effect,
                    old_owner,
                    1,
                    1,
                    Some(notice(EffectProcessorNoticeKind::Error, "repair")),
                    vec![],
                    EffectProcessorRetention::Retained,
                ),
            ),
            epoch,
            &mut feed,
        );
        assert!(state.observe(
            update(
                scope(epoch, 2),
                receipt(
                    effect,
                    old_owner,
                    2,
                    2,
                    None,
                    vec![],
                    EffectProcessorRetention::Retained
                )
            ),
            epoch,
            &mut feed
        ));
        assert!(!state
            .feedback
            .iter()
            .next()
            .expect("clear receipt")
            .feedback
            .snapshot
            .is_blocked());
        let new_owner = EffectProcessorOwnerId::new();
        assert!(state.observe(
            update(
                scope(epoch, 3),
                receipt(
                    effect,
                    new_owner,
                    1,
                    0,
                    None,
                    vec![],
                    EffectProcessorRetention::Evicted
                )
            ),
            epoch,
            &mut feed
        ));
        let actual = state.feedback.iter().next().expect("new lifetime fact");
        assert_eq!(actual.feedback.owner, new_owner);
        assert_eq!(actual.retention, EffectProcessorRetention::Evicted);
        assert!(!state.observe(
            update(
                scope(epoch, 2),
                receipt(
                    effect,
                    old_owner,
                    3,
                    3,
                    Some(notice(EffectProcessorNoticeKind::Error, "stale old owner")),
                    vec![],
                    EffectProcessorRetention::Retained
                )
            ),
            epoch,
            &mut feed
        ));
        assert_eq!(
            state.feedback.iter().next().expect("current lifetime").feedback.owner,
            new_owner
        );
    }

    #[test]
    fn information_is_neutral_and_log_fatal_are_not_popup_success() {
        let (epoch, _) = epochs();
        let mut state = PreviewProcessorFeedbackState::default();
        let mut feed = AppNotificationFeed::default();
        assert!(state.observe(
            update(
                scope(epoch, 1),
                receipt(
                    EffectId::new(),
                    EffectProcessorOwnerId::new(),
                    1,
                    0,
                    None,
                    vec![
                        notice(EffectProcessorNoticeKind::Information, "插件完整消息 α"),
                        notice(EffectProcessorNoticeKind::Log, "diagnostic"),
                        notice(EffectProcessorNoticeKind::Fatal, "not a success popup")
                    ],
                    EffectProcessorRetention::Evicted
                )
            ),
            epoch,
            &mut feed
        ));
        let entries: Vec<_> = feed.iter().collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].severity, AppNotificationSeverity::Information);
        assert_eq!(
            entries[0].message,
            AppMessage::new("notification-native-processor").with_text("message", "插件完整消息 α")
        );
    }

    #[test]
    fn scope_retirement_requires_empty_facts_and_clears_removed_effects() {
        let (epoch, _) = epochs();
        let mut state = PreviewProcessorFeedbackState::default();
        let mut feed = AppNotificationFeed::default();
        let actual = receipt(
            EffectId::new(),
            EffectProcessorOwnerId::new(),
            1,
            0,
            None,
            vec![],
            EffectProcessorRetention::Retained,
        );
        state.observe(update(scope(epoch, 1), actual.clone()), epoch, &mut feed);
        let retirement = PreviewProcessorFeedbackScope { key: None, generation: 2, epoch };
        assert!(!state.observe(update(retirement, actual), epoch, &mut feed));
        assert_eq!(state.feedback.len(), 1);
        assert!(state.observe(
            PreviewProcessorFeedbackUpdate {
                scope: retirement,
                feedback: EffectProcessorFeedbackBatch::new()
            },
            epoch,
            &mut feed
        ));
        assert!(state.feedback.is_empty());
        assert_eq!(feed.iter().count(), 0);
    }
    fn live_update(
        scope: PreviewProcessorFeedbackScope,
        effect: EffectId,
        owner: EffectProcessorOwnerId,
        invocation: u64,
        revision: u64,
    ) -> PreviewProcessorProgressUpdate {
        let terminal = receipt(
            effect,
            owner,
            invocation,
            0,
            None,
            vec![],
            EffectProcessorRetention::Retained,
        );
        PreviewProcessorProgressUpdate {
            scope,
            receipt: Some(mondrian_effects::EffectProcessorProgressReceipt {
                preparation: terminal.preparation,
                provider_revision: terminal.provider_revision,
                progress: mondrian_effects::EffectProcessorLiveProgress {
                    owner,
                    origin: terminal.feedback.origin,
                    native_owner: 1,
                    invocation,
                    revision,
                    progress: mondrian_effects::EffectProcessorProgress::new(
                        1,
                        true,
                        0.375,
                        notice(EffectProcessorNoticeKind::Information, "Live native render"),
                    )
                    .expect("bounded live fact"),
                },
            }),
        }
    }

    #[test]
    fn executing_progress_refuses_stale_epoch_generation_owner_and_revision() {
        let (epoch, other_epoch) = epochs();
        let effect = EffectId::new();
        let owner = EffectProcessorOwnerId::new();
        let mut state = PreviewProcessorFeedbackState::default();
        let current = live_update(scope(epoch, 10), effect, owner, 3, 4);
        assert!(state.observe_progress(current.clone(), epoch));
        for stale in [
            live_update(scope(other_epoch, 10), effect, owner, 3, 5),
            live_update(scope(epoch, 9), effect, owner, 4, 5),
            live_update(scope(epoch, 10), effect, owner, 2, 5),
            live_update(scope(epoch, 10), effect, owner, 3, 4),
        ] {
            assert!(!state.observe_progress(stale, epoch));
            assert_eq!(state.active_progress(), current.receipt.as_ref());
        }
        let mut foreign_native = live_update(scope(epoch, 10), effect, owner, 3, 5);
        foreign_native.receipt.as_mut().expect("receipt").progress.native_owner = 2;
        assert!(!state.observe_progress(foreign_native, epoch));
        assert_eq!(state.active_progress(), current.receipt.as_ref());
    }

    #[test]
    fn live_progress_has_no_popup_or_retention_and_exact_end_prevents_resurrection() {
        let (epoch, other_epoch) = epochs();
        let effect = EffectId::new();
        let owner = EffectProcessorOwnerId::new();
        let mut state = PreviewProcessorFeedbackState::default();
        let feed = AppNotificationFeed::default();
        let current_scope = scope(epoch, 10);
        assert!(state.observe_progress(live_update(current_scope, effect, owner, 3, 4), epoch));
        assert_eq!(feed.iter().count(), 0);
        assert!(
            state.feedback.is_empty(),
            "executing metadata owns no terminal retention fact"
        );
        assert!(!state.observe_progress(
            PreviewProcessorProgressUpdate { scope: scope(other_epoch, 10), receipt: None },
            epoch
        ));
        assert!(
            state.active_progress().is_some(),
            "stale end cannot clear current work"
        );
        assert!(state.observe_progress(
            PreviewProcessorProgressUpdate { scope: current_scope, receipt: None },
            epoch
        ));
        assert!(state.active_progress().is_none());
        assert!(!state.observe_progress(live_update(current_scope, effect, owner, 3, 99), epoch));
        assert!(state.observe_progress(live_update(current_scope, effect, owner, 4, 1), epoch));
        assert_eq!(
            state.active_progress().expect("new actual invocation").progress.invocation,
            4
        );
        assert_eq!(feed.iter().count(), 0);
    }

    #[test]
    fn terminal_user_blocked_clears_live_observation_without_fake_frame_success() {
        let (epoch, _) = epochs();
        let effect = EffectId::new();
        let owner = EffectProcessorOwnerId::new();
        let mut state = PreviewProcessorFeedbackState::default();
        let mut feed = AppNotificationFeed::default();
        let current = scope(epoch, 10);
        assert!(state.observe_progress(live_update(current, effect, owner, 3, 4), epoch));
        assert!(state.observe(
            update(
                current,
                receipt(
                    effect,
                    owner,
                    3,
                    1,
                    Some(notice(
                        EffectProcessorNoticeKind::Error,
                        "Repair current parameters"
                    )),
                    vec![],
                    EffectProcessorRetention::Retained
                )
            ),
            epoch,
            &mut feed
        ));
        assert!(state.active_progress().is_none());
        assert!(state
            .feedback
            .iter()
            .next()
            .expect("real terminal fact")
            .feedback
            .snapshot
            .is_blocked());
        assert!(!state.observe_progress(live_update(current, effect, owner, 3, 99), epoch));
        assert_eq!(
            state.feedback.iter().next().expect("canonical fact").retention,
            EffectProcessorRetention::Retained
        );
    }

    #[test]
    fn existing_task_result_backpressure_drops_only_observational_progress() {
        let (epoch, _) = epochs();
        let update = live_update(
            scope(epoch, 1),
            EffectId::new(),
            EffectProcessorOwnerId::new(),
            1,
            1,
        );
        let receipt = update.receipt.expect("bounded fact");
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let sink = PreviewProcessorProgressPublisher::new_observer(1, move |receipt| {
            let _ = sender.try_send(receipt.clone());
        });
        sink.observe(&receipt);
        let mut newer = receipt.clone();
        newer.progress.revision = 2;
        sink.observe(&newer);
        assert_eq!(receiver.try_recv().expect("original queued fact"), receipt);
        assert!(
            receiver.try_recv().is_err(),
            "no alternate queue or blocked producer"
        );
        sink.observe(&newer);
        assert_eq!(receiver.try_recv().expect("later admitted fact"), newer);
        assert!(
            sink.maximum_storage_bytes()
                >= 3 * mondrian_effects::EffectProcessorProgressReceipt::maximum_retained_bytes()
        );
    }
}
