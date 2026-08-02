//! UI-independent coordination between Playback and preview execution.
//!
//! A Preview Adapter owns media work and reports facts. This module owns the
//! ordered application of those facts to [`AppState`], so Window and Headless
//! consumers cannot drift into different Frame Delivery or preroll policies.

use mondrian_playback::{FrameDeliveryCandidate, FrameDemandIdentity, PlaybackEpoch};

use super::preview_runtime::PreviewVideoPrerollRequest;
use super::AppState;

/// Completed preview-work facts returned by one bounded Adapter poll.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct PreviewWorkPoll {
    /// A decoded frame or terminal decode failure changed visible Viewer state.
    pub(crate) visible_change: bool,
    /// Pending execution state changed without requiring Viewer reconstruction.
    pub(crate) transport_change: bool,
    /// A pending Viewer candidate should retry after capacity was released.
    pub(crate) candidate_retry_required: bool,
    /// More completions should be drained on a follow-up runtime turn.
    pub(crate) needs_follow_up_poll: bool,
    /// Timestamp-free terminal candidates produced before usable presentation.
    ///
    /// Successful readiness remains nonterminal and is completed only by a
    /// Presentation Adapter using its exact Frame Presentation Ticket.
    /// The App consumption seam binds the real completion timestamp for the
    /// whole bounded poll batch before applying any candidate to Playback.
    pub(crate) frame_delivery_candidates: Vec<FrameDeliveryCandidate>,
}

impl PreviewWorkPoll {
    /// Merge facts sampled by multiple bounded queues in the same runtime turn.
    pub(crate) fn merge(&mut self, mut other: Self) {
        self.visible_change |= other.visible_change;
        self.transport_change |= other.transport_change;
        self.candidate_retry_required |= other.candidate_retry_required;
        self.needs_follow_up_poll |= other.needs_follow_up_poll;
        self.frame_delivery_candidates.append(&mut other.frame_delivery_candidates);
    }
}

/// Immediate media lookahead available to the current Playback Epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewVideoPreroll {
    pub(crate) ready_media_frames: usize,
    pub(crate) preservable_media_frames: usize,
}

/// Minimal authoritative transport identity consumed by Preview execution.
///
/// The Playback Epoch distinguishes seek/restart discontinuities from ordinary
/// frame advancement. `playing` selects the mutually exclusive playback or
/// interactive decoder family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewTransportIntent {
    playing: bool,
    epoch: PlaybackEpoch,
}

impl PreviewTransportIntent {
    /// Bind one Playback Epoch to its decoder-family selection.
    pub(crate) const fn new(playing: bool, epoch: PlaybackEpoch) -> Self {
        Self { playing, epoch }
    }

    /// Whether the Playback decoder family is authoritative.
    pub(crate) const fn playing(self) -> bool {
        self.playing
    }

    /// Playback Session identity used to detect discontinuities.
    pub(crate) const fn epoch(self) -> PlaybackEpoch {
        self.epoch
    }
}

/// Preview execution facts required by the Playback coordination loop.
///
/// Implementations may schedule and cache differently, but cannot apply Frame
/// Deliveries or decide whether preroll changes Transport State themselves.
pub(crate) trait PlaybackPreviewAdapter {
    /// Poll completed and expired work against one sampled pending demand and
    /// the transport activity sampled in the same coordination turn.
    fn poll_playback_work(
        &self,
        pending_demand: Option<FrameDemandIdentity>,
        transport_intent: PreviewTransportIntent,
    ) -> PreviewWorkPoll;

    /// Observe current-epoch media lookahead without changing Playback state.
    fn video_preroll(&self, request: PreviewVideoPrerollRequest<'_>)
        -> Option<PreviewVideoPreroll>;
}

/// Observable result of one production preview-execution pump.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaybackPreviewPumpOutcome {
    pub(crate) visible_change: bool,
    pub(crate) transport_change: bool,
    pub(crate) candidate_retry_required: bool,
    pub(crate) needs_follow_up_poll: bool,
}

/// Apply one ordered production preview-execution cycle to Playback state.
///
/// The pending Frame Demand identity is sampled once before polling. This is
/// the only identity an Adapter may use when classifying terminal worker
/// results in this cycle. The App binds one real timestamp for the bounded
/// candidate batch before application. Application precedes preroll
/// observation so a terminal fact cannot be followed by readiness for a
/// superseded demand.
pub(crate) fn pump_playback_preview(
    state: &mut AppState,
    adapter: &impl PlaybackPreviewAdapter,
) -> PlaybackPreviewPumpOutcome {
    let pending_demand = state.pending_playback_frame_demand_identity();
    let transport_intent = state.preview_transport_intent();
    let poll = adapter.poll_playback_work(pending_demand, transport_intent);
    let observed_at = std::time::Instant::now();
    let delivery_changed =
        poll.frame_delivery_candidates
            .iter()
            .copied()
            .fold(false, |changed, candidate| {
                // Completion draining and realtime-stall expiry are independent
                // bounded sources. They may both report a terminal fact for the
                // identity sampled at the start of this turn. The first accepted
                // fact consumes that authority; later facts are expected losing
                // races, not rejected Playback observations.
                if state.pending_playback_frame_demand_identity() == Some(candidate.identity()) {
                    state.observe_frame_delivery_candidate(candidate, observed_at) || changed
                } else {
                    changed
                }
            });
    let preroll_changed = observe_playback_video_preroll(state, adapter);

    PlaybackPreviewPumpOutcome {
        visible_change: poll.visible_change,
        transport_change: poll.transport_change || delivery_changed || preroll_changed,
        candidate_retry_required: poll.candidate_retry_required,
        needs_follow_up_poll: poll.needs_follow_up_poll,
    }
}

/// Apply current-epoch video lookahead through the Playback Engine.
pub(crate) fn observe_playback_video_preroll(
    state: &mut AppState,
    adapter: &impl PlaybackPreviewAdapter,
) -> bool {
    // Terminal deliveries may have rotated or stopped the Playback Session.
    // Capture preroll only after those facts were applied; reusing the poll
    // turn's transport intent would observe a stale Epoch or Priming state.
    let request = state.preview_video_preroll_request(std::time::Instant::now());
    let demand = request.demand();
    let readiness = adapter.video_preroll(request);
    demand.zip(readiness).is_some_and(|(demand, readiness)| {
        let observed_at = std::time::Instant::now();
        state.observe_video_preroll_at_wall(
            demand,
            readiness.ready_media_frames,
            readiness.preservable_media_frames,
            observed_at,
        )
    })
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use mondrian_playback::{FrameDeliveryCandidate, FrameDeliveryKind};

    use super::*;

    struct FakePreviewAdapter {
        poll: RefCell<Option<PreviewWorkPoll>>,
        preroll: Option<PreviewVideoPreroll>,
        transport_intent: Cell<Option<PreviewTransportIntent>>,
        preroll_transport_intent: Cell<Option<PreviewTransportIntent>>,
        preroll_demand: Cell<Option<FrameDemandIdentity>>,
    }

    impl PlaybackPreviewAdapter for FakePreviewAdapter {
        fn poll_playback_work(
            &self,
            _pending_demand: Option<FrameDemandIdentity>,
            transport_intent: PreviewTransportIntent,
        ) -> PreviewWorkPoll {
            self.transport_intent.set(Some(transport_intent));
            self.poll.borrow_mut().take().unwrap_or_default()
        }

        fn video_preroll(
            &self,
            request: PreviewVideoPrerollRequest<'_>,
        ) -> Option<PreviewVideoPreroll> {
            self.preroll_transport_intent.set(Some(request.snapshot().transport().intent()));
            self.preroll_demand.set(request.demand());
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
                candidate_retry_required: false,
                needs_follow_up_poll: true,
                frame_delivery_candidates: Vec::new(),
            })),
            preroll: None,
            transport_intent: Cell::new(None),
            preroll_transport_intent: Cell::new(None),
            preroll_demand: Cell::new(None),
        };

        assert_eq!(
            pump_playback_preview(&mut state, &adapter),
            PlaybackPreviewPumpOutcome {
                visible_change: true,
                transport_change: false,
                candidate_retry_required: false,
                needs_follow_up_poll: true,
            }
        );
        assert!(adapter.transport_intent.get().is_some_and(|intent| !intent.playing()));
    }

    #[test]
    fn pump_applies_terminal_delivery_before_any_preroll_observation() {
        let mut state = AppState::new();
        state.set_playback_frame_running(4);
        let identity = state.pending_playback_frame_demand_identity().expect("active demand");
        let expected_transport_intent = state.preview_transport_intent();
        let adapter = FakePreviewAdapter {
            poll: RefCell::new(Some(PreviewWorkPoll {
                frame_delivery_candidates: vec![FrameDeliveryCandidate::for_demand(
                    identity,
                    FrameDeliveryKind::Blocked,
                )],
                ..PreviewWorkPoll::default()
            })),
            preroll: Some(PreviewVideoPreroll {
                ready_media_frames: 8,
                preservable_media_frames: 8,
            }),
            transport_intent: Cell::new(None),
            preroll_transport_intent: Cell::new(None),
            preroll_demand: Cell::new(None),
        };

        let outcome = pump_playback_preview(&mut state, &adapter);

        assert!(outcome.transport_change);
        assert!(!state.is_playing());
        assert_eq!(
            adapter.transport_intent.get(),
            Some(expected_transport_intent),
            "transport identity must be sampled before terminal candidates mutate state"
        );
        assert!(
            adapter.preroll_transport_intent.get().is_some_and(|intent| !intent.playing()),
            "preroll must recapture transport after the terminal candidate is applied"
        );
    }

    #[test]
    fn pump_binds_preroll_to_the_recaptured_exact_demand() {
        let mut state = AppState::new();
        state.set_playback_frame_running(4);
        let identity =
            state.pending_playback_frame_demand_identity().expect("active priming demand");
        assert!(
            !state.observe_viewer_frame_delivery(FrameDeliveryKind::Ready),
            "current presentation alone must not release video priming"
        );
        let adapter = FakePreviewAdapter {
            poll: RefCell::new(None),
            preroll: Some(PreviewVideoPreroll {
                ready_media_frames: 0,
                preservable_media_frames: 0,
            }),
            transport_intent: Cell::new(None),
            preroll_transport_intent: Cell::new(None),
            preroll_demand: Cell::new(None),
        };

        let outcome = pump_playback_preview(&mut state, &adapter);

        assert!(outcome.transport_change);
        assert!(state.is_playing());
        assert_eq!(adapter.preroll_demand.get(), Some(identity));
    }

    #[test]
    fn pump_silently_retires_losing_terminal_facts_for_consumed_demand() {
        let mut state = AppState::new();
        state.set_playback_frame_running(4);
        let identity = state.pending_playback_frame_demand_identity().expect("active demand");
        let adapter = FakePreviewAdapter {
            poll: RefCell::new(Some(PreviewWorkPoll {
                frame_delivery_candidates: vec![
                    FrameDeliveryCandidate::for_demand(identity, FrameDeliveryKind::Late),
                    FrameDeliveryCandidate::for_demand(identity, FrameDeliveryKind::Late),
                ],
                ..PreviewWorkPoll::default()
            })),
            preroll: None,
            transport_intent: Cell::new(None),
            preroll_transport_intent: Cell::new(None),
            preroll_demand: Cell::new(None),
        };

        pump_playback_preview(&mut state, &adapter);
        let evidence = state.playback_evidence_report();

        assert_eq!(evidence.deliveries.late, 1);
        assert_eq!(evidence.deliveries.rejected, 0);
    }

    #[test]
    fn headless_seek_cancel_and_dropped_gpu_facts_share_one_terminal_authority() {
        let mut state = AppState::new();
        state.set_playback_frame_running(4);
        let stale_identity =
            state.pending_playback_frame_demand_identity().expect("pre-seek demand");
        let stale_ticket = state
            .playback_frame_presentation_ticket(mondrian_playback::FramePresentationQuality::Ready)
            .expect("pre-seek presentation ticket");

        // A Headless consumer models the seek as a fresh epoch. A GPU
        // completion from the retired epoch and a queued cancellation may race
        // the current presentation timeout, but only the current identity owns
        // terminal authority.
        state.set_playback_frame_running(12);
        let current_identity =
            state.pending_playback_frame_demand_identity().expect("post-seek demand");
        assert_ne!(stale_identity, current_identity);
        assert_eq!(
            state.complete_frame_presentation(stale_ticket, std::time::Instant::now()),
            None
        );

        let adapter = FakePreviewAdapter {
            poll: RefCell::new(Some(PreviewWorkPoll {
                transport_change: true,
                frame_delivery_candidates: vec![
                    FrameDeliveryCandidate::for_demand(stale_identity, FrameDeliveryKind::Blocked),
                    FrameDeliveryCandidate::for_demand(current_identity, FrameDeliveryKind::Late),
                ],
                ..PreviewWorkPoll::default()
            })),
            preroll: None,
            transport_intent: Cell::new(None),
            preroll_transport_intent: Cell::new(None),
            preroll_demand: Cell::new(None),
        };
        let outcome = pump_playback_preview(&mut state, &adapter);
        let evidence = state.playback_evidence_report();

        assert!(outcome.transport_change);
        assert_eq!(evidence.deliveries.late, 1);
        assert_eq!(evidence.deliveries.blocked, 0);
        assert_eq!(evidence.deliveries.rejected, 0);
    }
}
