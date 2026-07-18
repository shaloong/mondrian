//! UI-independent coordination between Playback and preview execution.
//!
//! A Preview Adapter owns media work and reports facts. This module owns the
//! ordered application of those facts to [`AppState`], so Window and Headless
//! consumers cannot drift into different Frame Delivery or preroll policies.

use mondrian_playback::{FrameDelivery, FrameDemandIdentity};

use super::AppState;

/// Completed preview-work facts returned by one bounded Adapter poll.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct PreviewWorkPoll {
    /// A decoded frame or terminal decode failure changed visible Viewer state.
    pub(crate) visible_change: bool,
    /// Pending execution state changed without requiring Viewer reconstruction.
    pub(crate) transport_change: bool,
    /// More completions should be drained on a follow-up runtime turn.
    pub(crate) needs_follow_up_poll: bool,
    /// Exact terminal deliveries produced before usable presentation.
    ///
    /// Successful readiness remains nonterminal and is completed only by a
    /// Presentation Adapter using its exact Frame Presentation Ticket.
    pub(crate) frame_deliveries: Vec<FrameDelivery>,
}

impl PreviewWorkPoll {
    /// Merge facts sampled by multiple bounded queues in the same runtime turn.
    pub(crate) fn merge(&mut self, mut other: Self) {
        self.visible_change |= other.visible_change;
        self.transport_change |= other.transport_change;
        self.needs_follow_up_poll |= other.needs_follow_up_poll;
        self.frame_deliveries.append(&mut other.frame_deliveries);
    }
}

/// Immediate media lookahead available to the current Playback Epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewVideoPreroll {
    pub(crate) ready_media_frames: usize,
    pub(crate) available_media_frames: usize,
}

/// Preview execution facts required by the Playback coordination loop.
///
/// Implementations may schedule and cache differently, but cannot apply Frame
/// Deliveries or decide whether preroll changes Transport State themselves.
pub(crate) trait PlaybackPreviewAdapter {
    /// Poll completed and expired work against one sampled pending demand.
    fn poll_playback_work(&self, pending_demand: Option<FrameDemandIdentity>) -> PreviewWorkPoll;

    /// Observe current-epoch media lookahead without changing Playback state.
    fn video_preroll(&self, state: &AppState) -> Option<PreviewVideoPreroll>;
}

/// Observable result of one production preview-execution pump.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaybackPreviewPumpOutcome {
    pub(crate) visible_change: bool,
    pub(crate) transport_change: bool,
    pub(crate) needs_follow_up_poll: bool,
}

/// Apply one ordered production preview-execution cycle to Playback state.
///
/// The pending Frame Demand identity is sampled once before polling. This is
/// the only identity an Adapter may use when binding terminal worker results in
/// this cycle. Delivery application precedes preroll observation so a terminal
/// delivery cannot be followed by readiness for a superseded demand.
pub(crate) fn pump_playback_preview(
    state: &mut AppState,
    adapter: &impl PlaybackPreviewAdapter,
) -> PlaybackPreviewPumpOutcome {
    let pending_demand = state.pending_playback_frame_demand_identity();
    let poll = adapter.poll_playback_work(pending_demand);
    let delivery_changed =
        poll.frame_deliveries.iter().copied().fold(false, |changed, delivery| {
            // Completion draining and realtime-stall expiry are independent
            // bounded sources. They may both report a terminal fact for the
            // identity sampled at the start of this turn. The first accepted
            // fact consumes that authority; later facts are expected losing
            // races, not rejected Playback observations.
            if state.pending_playback_frame_demand_identity() == Some(delivery.identity()) {
                state.observe_frame_delivery(delivery) || changed
            } else {
                changed
            }
        });
    let preroll_changed = observe_playback_video_preroll(state, adapter);

    PlaybackPreviewPumpOutcome {
        visible_change: poll.visible_change,
        transport_change: poll.transport_change || delivery_changed || preroll_changed,
        needs_follow_up_poll: poll.needs_follow_up_poll,
    }
}

/// Apply current-epoch video lookahead through the Playback Engine.
pub(crate) fn observe_playback_video_preroll(
    state: &mut AppState,
    adapter: &impl PlaybackPreviewAdapter,
) -> bool {
    adapter.video_preroll(state).is_some_and(|readiness| {
        state.observe_video_preroll(
            readiness.ready_media_frames,
            readiness.available_media_frames,
        )
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use mondrian_playback::{FrameDelivery, FrameDeliveryKind};

    use super::*;

    struct FakePreviewAdapter {
        poll: RefCell<Option<PreviewWorkPoll>>,
        preroll: Option<PreviewVideoPreroll>,
    }

    impl PlaybackPreviewAdapter for FakePreviewAdapter {
        fn poll_playback_work(
            &self,
            _pending_demand: Option<FrameDemandIdentity>,
        ) -> PreviewWorkPoll {
            self.poll.borrow_mut().take().unwrap_or_default()
        }

        fn video_preroll(&self, _state: &AppState) -> Option<PreviewVideoPreroll> {
            self.preroll
        }
    }

    #[test]
    fn pump_projects_bounded_adapter_work_without_ui_state() {
        let mut state = AppState::new();
        let adapter = FakePreviewAdapter {
            poll: RefCell::new(Some(PreviewWorkPoll {
                visible_change: true,
                transport_change: false,
                needs_follow_up_poll: true,
                frame_deliveries: Vec::new(),
            })),
            preroll: None,
        };

        assert_eq!(
            pump_playback_preview(&mut state, &adapter),
            PlaybackPreviewPumpOutcome {
                visible_change: true,
                transport_change: false,
                needs_follow_up_poll: true,
            }
        );
    }

    #[test]
    fn pump_applies_terminal_delivery_before_any_preroll_observation() {
        let mut state = AppState::new();
        state.set_playback_frame_running(4);
        let identity = state.pending_playback_frame_demand_identity().expect("active demand");
        let adapter = FakePreviewAdapter {
            poll: RefCell::new(Some(PreviewWorkPoll {
                frame_deliveries: vec![FrameDelivery::for_demand(
                    identity,
                    FrameDeliveryKind::Blocked,
                )],
                ..PreviewWorkPoll::default()
            })),
            preroll: Some(PreviewVideoPreroll { ready_media_frames: 8, available_media_frames: 8 }),
        };

        let outcome = pump_playback_preview(&mut state, &adapter);

        assert!(outcome.transport_change);
        assert!(!state.is_playing());
    }

    #[test]
    fn pump_silently_retires_losing_terminal_facts_for_consumed_demand() {
        let mut state = AppState::new();
        state.set_playback_frame_running(4);
        let identity = state.pending_playback_frame_demand_identity().expect("active demand");
        let adapter = FakePreviewAdapter {
            poll: RefCell::new(Some(PreviewWorkPoll {
                frame_deliveries: vec![
                    FrameDelivery::for_demand(identity, FrameDeliveryKind::Late),
                    FrameDelivery::for_demand(identity, FrameDeliveryKind::Late),
                ],
                ..PreviewWorkPoll::default()
            })),
            preroll: None,
        };

        pump_playback_preview(&mut state, &adapter);
        let evidence = state.playback_evidence_report();

        assert_eq!(evidence.deliveries.late, 1);
        assert_eq!(evidence.deliveries.rejected, 0);
    }
}
