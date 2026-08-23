//! Lock-free observation of the concrete Preview decode Adapter execution stage.
//!
//! This module is evidence only. It does not own deadlines, cancellation,
//! recovery, or worker admission. A worker is the sole writer; diagnostics and
//! Headless acceptance may take bounded point-in-time snapshots concurrently.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

/// Concrete media-Adapter stage currently occupied by one Preview decode worker.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum PreviewDecodeExecutionStage {
    /// No media Adapter request or resource retirement is executing.
    #[default]
    Idle = 0,
    /// A request is waiting for a previously published native output lease.
    OutputLeaseWait = 1,
    /// FFmpeg is opening an input or protocol.
    InputOpen = 2,
    /// FFmpeg is discovering stream information.
    StreamInfo = 3,
    /// Session metadata, seek indexes, and decode policy are being prepared.
    SessionSetup = 4,
    /// A hardware device is being acquired and attached to a codec context.
    HardwareDevice = 5,
    /// FFmpeg is opening a video codec.
    CodecOpen = 6,
    /// Preview caches and request policy are being evaluated.
    CacheLookup = 7,
    /// FFmpeg is seeking and flushing the active codec position.
    Seek = 8,
    /// FFmpeg is reading the next demuxed packet.
    PacketRead = 9,
    /// FFmpeg is accepting one compressed packet or the end-of-input marker.
    CodecSendInput = 10,
    /// FFmpeg is producing one decoded frame or codec backpressure result.
    CodecReceiveFrame = 11,
    /// FFmpeg is flushing buffered codec and DPB state at a discontinuity.
    CodecFlush = 12,
    /// A decoded frame is being converted or wrapped into its output residency.
    FrameMaterialization = 13,
    /// The optional external FFmpeg process is being executed or reaped.
    ExternalProcess = 14,
    /// A codec, DPB, format context, or hardware-surface pool is being retired.
    SessionRetire = 15,
}

impl PreviewDecodeExecutionStage {
    /// Stable evidence name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::OutputLeaseWait => "output_lease_wait",
            Self::InputOpen => "input_open",
            Self::StreamInfo => "stream_info",
            Self::SessionSetup => "session_setup",
            Self::HardwareDevice => "hardware_device",
            Self::CodecOpen => "codec_open",
            Self::CacheLookup => "cache_lookup",
            Self::Seek => "seek",
            Self::PacketRead => "packet_read",
            Self::CodecSendInput => "codec_send_input",
            Self::CodecReceiveFrame => "codec_receive_frame",
            Self::CodecFlush => "codec_flush",
            Self::FrameMaterialization => "frame_materialization",
            Self::ExternalProcess => "external_process",
            Self::SessionRetire => "session_retire",
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::OutputLeaseWait,
            2 => Self::InputOpen,
            3 => Self::StreamInfo,
            4 => Self::SessionSetup,
            5 => Self::HardwareDevice,
            6 => Self::CodecOpen,
            7 => Self::CacheLookup,
            8 => Self::Seek,
            9 => Self::PacketRead,
            10 => Self::CodecSendInput,
            11 => Self::CodecReceiveFrame,
            12 => Self::CodecFlush,
            13 => Self::FrameMaterialization,
            14 => Self::ExternalProcess,
            15 => Self::SessionRetire,
            _ => Self::Idle,
        }
    }
}

/// Immutable progress evidence from one Preview decode worker.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewDecodeExecutionProgress {
    /// Concrete media-Adapter stage observed at snapshot time.
    pub stage: PreviewDecodeExecutionStage,
    /// Number of requests admitted by this observer.
    pub request_sequence: u64,
    /// Monotonic progress-publication sequence, including repeated stages and
    /// FFmpeg interrupt-callback polls.
    pub progress_sequence: u64,
    /// Number of FFmpeg interrupt-callback polls observed by this worker.
    pub interrupt_poll_sequence: u64,
    /// Number of callback polls that observed the active request canceled.
    pub interrupt_cancel_sequence: u64,
    /// Request sequence associated with the last callback cancellation.
    pub interrupt_last_cancel_request_sequence: u64,
    /// Process-isolated demux execution and lifecycle facts for this worker.
    pub isolated_demux: PreviewIsolatedDemuxExecutionEvidence,
}

/// Cumulative execution and lifecycle facts for process-isolated Preview demux.
///
/// These counters describe completed facts only. In particular, a terminal
/// session is counted only after the child has been reaped, and a cross-request
/// reuse is counted only after the same ready helper completes work for a later
/// Preview decode request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewIsolatedDemuxExecutionEvidence {
    /// Helper processes successfully launched by this Preview worker.
    pub session_launches: u64,
    /// Launched helpers that published a validated stream contract.
    pub ready_sessions: u64,
    /// Ready helpers that completed a command for a later decode request.
    pub cross_request_reused_sessions: u64,
    /// Seek commands acknowledged successfully by the helper.
    pub completed_seeks: u64,
    /// Read commands that returned either one packet or end of input.
    pub completed_reads: u64,
    /// Successful read commands that returned one validated packet.
    pub packet_responses: u64,
    /// Successful read commands that returned end of input.
    pub end_responses: u64,
    /// Helpers that acknowledged Close and exited within the close grace.
    pub clean_closes: u64,
    /// Helpers killed and reaped because the owning decode request canceled.
    pub cancellation_terminations: u64,
    /// Helpers reaped after protocol, process, pipe, or FFmpeg failure.
    pub failure_terminations: u64,
    /// Healthy helpers killed and reaped after bounded Close did not complete.
    pub forced_close_terminations: u64,
    /// Successfully launched helpers that have not yet been reaped.
    pub active_sessions: u64,
    /// Maximum active helper count observed by this worker.
    pub peak_active_sessions: u64,
}

impl PreviewIsolatedDemuxExecutionEvidence {
    /// Total helpers whose process has reached a terminal, reaped state.
    pub const fn reaped_sessions(self) -> u64 {
        self.clean_closes
            .saturating_add(self.cancellation_terminations)
            .saturating_add(self.failure_terminations)
            .saturating_add(self.forced_close_terminations)
    }
}

/// Cloneable single-writer observer for one Preview decode worker.
#[derive(Debug, Clone, Default)]
pub struct PreviewDecodeExecutionObserver {
    state: Arc<PreviewDecodeExecutionObserverState>,
}

#[derive(Debug, Default)]
struct PreviewDecodeExecutionObserverState {
    // Even values are stable snapshots; odd values mean the sole writer is
    // publishing. This tiny seqlock keeps stage/request identity coherent
    // without putting locks or allocation on packet/frame execution.
    revision: AtomicU64,
    request_sequence: AtomicU64,
    stage: AtomicU8,
    interrupt_poll_sequence: AtomicU64,
    interrupt_cancel_sequence: AtomicU64,
    interrupt_last_cancel_request_sequence: AtomicU64,
    demux_session_launches: AtomicU64,
    demux_ready_sessions: AtomicU64,
    demux_cross_request_reused_sessions: AtomicU64,
    demux_completed_seeks: AtomicU64,
    demux_completed_reads: AtomicU64,
    demux_packet_responses: AtomicU64,
    demux_end_responses: AtomicU64,
    demux_clean_closes: AtomicU64,
    demux_cancellation_terminations: AtomicU64,
    demux_failure_terminations: AtomicU64,
    demux_forced_close_terminations: AtomicU64,
    demux_active_sessions: AtomicU64,
    demux_peak_active_sessions: AtomicU64,
}

impl PreviewDecodeExecutionObserver {
    /// Create an idle observer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a coherent, bounded point-in-time progress snapshot.
    pub fn snapshot(&self) -> PreviewDecodeExecutionProgress {
        loop {
            let before = self.state.revision.load(Ordering::Acquire);
            if before & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let request_sequence = self.state.request_sequence.load(Ordering::Relaxed);
            let stage = self.state.stage.load(Ordering::Relaxed);
            let interrupt_poll_sequence =
                self.state.interrupt_poll_sequence.load(Ordering::Relaxed);
            let interrupt_cancel_sequence =
                self.state.interrupt_cancel_sequence.load(Ordering::Relaxed);
            let interrupt_last_cancel_request_sequence =
                self.state.interrupt_last_cancel_request_sequence.load(Ordering::Relaxed);
            let isolated_demux = PreviewIsolatedDemuxExecutionEvidence {
                session_launches: self.state.demux_session_launches.load(Ordering::Relaxed),
                ready_sessions: self.state.demux_ready_sessions.load(Ordering::Relaxed),
                cross_request_reused_sessions: self
                    .state
                    .demux_cross_request_reused_sessions
                    .load(Ordering::Relaxed),
                completed_seeks: self.state.demux_completed_seeks.load(Ordering::Relaxed),
                completed_reads: self.state.demux_completed_reads.load(Ordering::Relaxed),
                packet_responses: self.state.demux_packet_responses.load(Ordering::Relaxed),
                end_responses: self.state.demux_end_responses.load(Ordering::Relaxed),
                clean_closes: self.state.demux_clean_closes.load(Ordering::Relaxed),
                cancellation_terminations: self
                    .state
                    .demux_cancellation_terminations
                    .load(Ordering::Relaxed),
                failure_terminations: self.state.demux_failure_terminations.load(Ordering::Relaxed),
                forced_close_terminations: self
                    .state
                    .demux_forced_close_terminations
                    .load(Ordering::Relaxed),
                active_sessions: self.state.demux_active_sessions.load(Ordering::Relaxed),
                peak_active_sessions: self.state.demux_peak_active_sessions.load(Ordering::Relaxed),
            };
            let after = self.state.revision.load(Ordering::Acquire);
            if before == after {
                return PreviewDecodeExecutionProgress {
                    stage: PreviewDecodeExecutionStage::from_u8(stage),
                    request_sequence,
                    progress_sequence: after / 2,
                    interrupt_poll_sequence,
                    interrupt_cancel_sequence,
                    interrupt_last_cancel_request_sequence,
                    isolated_demux,
                };
            }
        }
    }

    pub(super) fn begin_request(&self) -> PreviewDecodeExecutionGuard {
        self.publish(PreviewDecodeExecutionStage::SessionSetup, true);
        PreviewDecodeExecutionGuard { observer: self.clone() }
    }

    pub(super) fn publish_stage(&self, stage: PreviewDecodeExecutionStage) {
        self.publish(stage, false);
    }

    pub(super) fn finish_idle(&self) {
        self.publish(PreviewDecodeExecutionStage::Idle, false);
    }

    pub(super) fn publish_interrupt_poll(&self, canceled: bool) {
        self.state.revision.fetch_add(1, Ordering::AcqRel);
        self.state.interrupt_poll_sequence.fetch_add(1, Ordering::Relaxed);
        if canceled {
            self.state.interrupt_cancel_sequence.fetch_add(1, Ordering::Relaxed);
            let request_sequence = self.state.request_sequence.load(Ordering::Relaxed);
            self.state
                .interrupt_last_cancel_request_sequence
                .store(request_sequence, Ordering::Relaxed);
        }
        self.state.revision.fetch_add(1, Ordering::Release);
    }

    pub(super) fn begin_isolated_demux_session(&self) -> PreviewIsolatedDemuxSessionEvidence {
        let request_sequence = self.state.request_sequence.load(Ordering::Acquire);
        self.publish_evidence(|state| {
            state.demux_session_launches.fetch_add(1, Ordering::Relaxed);
            let active =
                state.demux_active_sessions.fetch_add(1, Ordering::Relaxed).saturating_add(1);
            state.demux_peak_active_sessions.fetch_max(active, Ordering::Relaxed);
        });
        PreviewIsolatedDemuxSessionEvidence {
            observer: self.clone(),
            launch_request_sequence: request_sequence,
            cross_request_reuse_published: false,
            settled: false,
        }
    }

    fn publish_evidence(&self, publish: impl FnOnce(&PreviewDecodeExecutionObserverState)) {
        self.state.revision.fetch_add(1, Ordering::AcqRel);
        publish(&self.state);
        self.state.revision.fetch_add(1, Ordering::Release);
    }

    fn publish(&self, stage: PreviewDecodeExecutionStage, begin_request: bool) {
        self.state.revision.fetch_add(1, Ordering::AcqRel);
        if begin_request {
            self.state.request_sequence.fetch_add(1, Ordering::Relaxed);
        }
        self.state.stage.store(stage as u8, Ordering::Relaxed);
        self.state.revision.fetch_add(1, Ordering::Release);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreviewIsolatedDemuxTermination {
    Canceled,
    Failed,
    CleanClose,
    ForcedClose,
}

/// Single-owner evidence lease paired with one successfully launched helper.
pub(super) struct PreviewIsolatedDemuxSessionEvidence {
    observer: PreviewDecodeExecutionObserver,
    launch_request_sequence: u64,
    cross_request_reuse_published: bool,
    settled: bool,
}

impl PreviewIsolatedDemuxSessionEvidence {
    pub(super) fn record_ready(&self) {
        self.observer.publish_evidence(|state| {
            state.demux_ready_sessions.fetch_add(1, Ordering::Relaxed);
        });
    }

    pub(super) fn record_seek_complete(&mut self) {
        self.record_command_complete(|state| {
            state.demux_completed_seeks.fetch_add(1, Ordering::Relaxed);
        });
    }

    pub(super) fn record_packet(&mut self) {
        self.record_command_complete(|state| {
            state.demux_completed_reads.fetch_add(1, Ordering::Relaxed);
            state.demux_packet_responses.fetch_add(1, Ordering::Relaxed);
        });
    }

    pub(super) fn record_end(&mut self) {
        self.record_command_complete(|state| {
            state.demux_completed_reads.fetch_add(1, Ordering::Relaxed);
            state.demux_end_responses.fetch_add(1, Ordering::Relaxed);
        });
    }

    pub(super) fn settle(&mut self, termination: PreviewIsolatedDemuxTermination) {
        if self.settled {
            return;
        }
        self.observer.publish_evidence(|state| {
            match termination {
                PreviewIsolatedDemuxTermination::Canceled => {
                    state.demux_cancellation_terminations.fetch_add(1, Ordering::Relaxed);
                }
                PreviewIsolatedDemuxTermination::Failed => {
                    state.demux_failure_terminations.fetch_add(1, Ordering::Relaxed);
                }
                PreviewIsolatedDemuxTermination::CleanClose => {
                    state.demux_clean_closes.fetch_add(1, Ordering::Relaxed);
                }
                PreviewIsolatedDemuxTermination::ForcedClose => {
                    state.demux_forced_close_terminations.fetch_add(1, Ordering::Relaxed);
                }
            }
            state.demux_active_sessions.fetch_sub(1, Ordering::Relaxed);
        });
        self.settled = true;
    }

    fn record_command_complete(
        &mut self,
        publish: impl FnOnce(&PreviewDecodeExecutionObserverState),
    ) {
        let request_sequence = self.observer.state.request_sequence.load(Ordering::Acquire);
        let publish_cross_request_reuse = !self.cross_request_reuse_published
            && self.launch_request_sequence > 0
            && request_sequence > 0
            && request_sequence != self.launch_request_sequence;
        self.observer.publish_evidence(|state| {
            publish(state);
            if publish_cross_request_reuse {
                state.demux_cross_request_reused_sessions.fetch_add(1, Ordering::Relaxed);
            }
        });
        self.cross_request_reuse_published |= publish_cross_request_reuse;
    }
}

pub(super) struct PreviewDecodeExecutionGuard {
    observer: PreviewDecodeExecutionObserver,
}

impl Drop for PreviewDecodeExecutionGuard {
    fn drop(&mut self) {
        self.observer.finish_idle();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observer_publishes_coherent_request_progress_and_returns_idle() {
        let observer = PreviewDecodeExecutionObserver::new();
        assert_eq!(
            observer.snapshot(),
            PreviewDecodeExecutionProgress::default()
        );

        {
            let _request = observer.begin_request();
            let started = observer.snapshot();
            assert_eq!(started.stage, PreviewDecodeExecutionStage::SessionSetup);
            assert_eq!(started.request_sequence, 1);

            observer.publish_stage(PreviewDecodeExecutionStage::CodecSendInput);
            observer.publish_stage(PreviewDecodeExecutionStage::CodecSendInput);
            observer.publish_interrupt_poll(false);
            observer.publish_interrupt_poll(true);
            let sending = observer.snapshot();
            assert_eq!(sending.stage, PreviewDecodeExecutionStage::CodecSendInput);
            assert_eq!(sending.request_sequence, 1);
            assert!(sending.progress_sequence >= started.progress_sequence + 4);
            assert_eq!(sending.interrupt_poll_sequence, 2);
            assert_eq!(sending.interrupt_cancel_sequence, 1);
            assert_eq!(sending.interrupt_last_cancel_request_sequence, 1);
        }

        let idle = observer.snapshot();
        assert_eq!(idle.stage, PreviewDecodeExecutionStage::Idle);
        assert_eq!(idle.request_sequence, 1);
    }

    #[test]
    fn observer_accounts_demux_reuse_and_reaping_once() {
        let observer = PreviewDecodeExecutionObserver::new();
        let mut session = {
            let _request = observer.begin_request();
            let mut session = observer.begin_isolated_demux_session();
            session.record_ready();
            session.record_packet();
            session
        };
        {
            let _request = observer.begin_request();
            session.record_seek_complete();
            session.record_end();
        }
        session.settle(PreviewIsolatedDemuxTermination::CleanClose);
        session.settle(PreviewIsolatedDemuxTermination::Failed);

        let evidence = observer.snapshot().isolated_demux;
        assert_eq!(evidence.session_launches, 1);
        assert_eq!(evidence.ready_sessions, 1);
        assert_eq!(evidence.cross_request_reused_sessions, 1);
        assert_eq!(evidence.completed_seeks, 1);
        assert_eq!(evidence.completed_reads, 2);
        assert_eq!(evidence.packet_responses, 1);
        assert_eq!(evidence.end_responses, 1);
        assert_eq!(evidence.clean_closes, 1);
        assert_eq!(evidence.reaped_sessions(), 1);
        assert_eq!(evidence.active_sessions, 0);
        assert_eq!(evidence.peak_active_sessions, 1);
    }
}
