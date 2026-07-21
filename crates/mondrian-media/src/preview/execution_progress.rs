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
    /// Monotonic stage-publication sequence, including repeated stages.
    pub progress_sequence: u64,
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
            let after = self.state.revision.load(Ordering::Acquire);
            if before == after {
                return PreviewDecodeExecutionProgress {
                    stage: PreviewDecodeExecutionStage::from_u8(stage),
                    request_sequence,
                    progress_sequence: after / 2,
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

    fn publish(&self, stage: PreviewDecodeExecutionStage, begin_request: bool) {
        self.state.revision.fetch_add(1, Ordering::AcqRel);
        if begin_request {
            self.state.request_sequence.fetch_add(1, Ordering::Relaxed);
        }
        self.state.stage.store(stage as u8, Ordering::Relaxed);
        self.state.revision.fetch_add(1, Ordering::Release);
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
            let sending = observer.snapshot();
            assert_eq!(sending.stage, PreviewDecodeExecutionStage::CodecSendInput);
            assert_eq!(sending.request_sequence, 1);
            assert!(sending.progress_sequence >= started.progress_sequence + 2);
        }

        let idle = observer.snapshot();
        assert_eq!(idle.stage, PreviewDecodeExecutionStage::Idle);
        assert_eq!(idle.request_sequence, 1);
    }
}
