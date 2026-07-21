use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

/// Execution point at which a Preview decode first observed cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum PreviewDecodeCancellationCheckpoint {
    /// Cancellation existed before any input work was admitted.
    BeforeInputOpen = 1,
    /// FFmpeg was opening the input/protocol.
    InputOpen = 2,
    /// FFmpeg was discovering stream information.
    StreamInfo = 3,
    /// Preview caches and request policy were being evaluated.
    CacheLookup = 4,
    /// FFmpeg was seeking the input.
    Seek = 5,
    /// FFmpeg was reading the next demuxed packet.
    PacketRead = 6,
    /// The codec was accepting packets or producing frames.
    Codec = 7,
    /// A decoded frame was being materialized into its output residency.
    FrameMaterialization = 8,
    /// The optional external FFmpeg process was being executed or reaped.
    ExternalProcess = 9,
    /// A bounded decoder slot was waiting for downstream native-output release.
    OutputLease = 10,
}

impl PreviewDecodeCancellationCheckpoint {
    /// Stable evidence name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BeforeInputOpen => "before_input_open",
            Self::InputOpen => "input_open",
            Self::StreamInfo => "stream_info",
            Self::CacheLookup => "cache_lookup",
            Self::Seek => "seek",
            Self::PacketRead => "packet_read",
            Self::Codec => "codec",
            Self::FrameMaterialization => "frame_materialization",
            Self::ExternalProcess => "external_process",
            Self::OutputLease => "output_lease",
        }
    }

    fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::BeforeInputOpen),
            2 => Some(Self::InputOpen),
            3 => Some(Self::StreamInfo),
            4 => Some(Self::CacheLookup),
            5 => Some(Self::Seek),
            6 => Some(Self::PacketRead),
            7 => Some(Self::Codec),
            8 => Some(Self::FrameMaterialization),
            9 => Some(Self::ExternalProcess),
            10 => Some(Self::OutputLease),
            _ => None,
        }
    }
}

/// Mechanism that first made a Preview cancellation observable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PreviewDecodeCancellationSource {
    /// A normal cooperative checkpoint in Mondrian observed cancellation.
    CooperativeCheckpoint,
    /// FFmpeg's `AVIOInterruptCB` interrupted blocking format/protocol work.
    FfmpegIoInterrupt,
}

/// Typed cancellation fact returned by the concrete Preview decode Adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PreviewDecodeCancellation {
    /// First execution point that observed cancellation.
    pub checkpoint: PreviewDecodeCancellationCheckpoint,
    /// Mechanism that observed cancellation at that point.
    pub source: PreviewDecodeCancellationSource,
}

impl PreviewDecodeCancellation {
    pub(super) fn cooperative(checkpoint: PreviewDecodeCancellationCheckpoint) -> Self {
        Self {
            checkpoint,
            source: PreviewDecodeCancellationSource::CooperativeCheckpoint,
        }
    }

    pub(super) fn ffmpeg_interrupt(checkpoint: PreviewDecodeCancellationCheckpoint) -> Self {
        Self {
            checkpoint,
            source: PreviewDecodeCancellationSource::FfmpegIoInterrupt,
        }
    }
}

/// Aggregate of concrete decode checkpoints that observed cancellation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewDecodeCancellationEvidence {
    /// Cancellations returned by the Preview decode Adapter.
    pub total: u64,
    /// Cancellations first observed at an ordinary cooperative checkpoint.
    pub cooperative_checkpoint: u64,
    /// Cancellations first observed by FFmpeg's blocking-I/O interrupt callback.
    pub ffmpeg_io_interrupt: u64,
    /// Per-checkpoint observation counts.
    pub checkpoints: PreviewDecodeCancellationCheckpointEvidence,
}

impl PreviewDecodeCancellationEvidence {
    /// Record one typed cancellation fact.
    pub fn observe(&mut self, cancellation: PreviewDecodeCancellation) {
        self.total = self.total.saturating_add(1);
        match cancellation.source {
            PreviewDecodeCancellationSource::CooperativeCheckpoint => {
                self.cooperative_checkpoint = self.cooperative_checkpoint.saturating_add(1);
            }
            PreviewDecodeCancellationSource::FfmpegIoInterrupt => {
                self.ffmpeg_io_interrupt = self.ffmpeg_io_interrupt.saturating_add(1);
            }
        }
        self.checkpoints.observe(cancellation.checkpoint);
    }
}

/// Per-checkpoint Preview decode cancellation counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewDecodeCancellationCheckpointEvidence {
    /// Requests canceled before input work was admitted.
    pub before_input_open: u64,
    /// Requests canceled while opening an input or protocol.
    pub input_open: u64,
    /// Requests canceled while discovering stream information.
    pub stream_info: u64,
    /// Requests canceled while evaluating caches or request policy.
    pub cache_lookup: u64,
    /// Requests canceled while seeking.
    pub seek: u64,
    /// Requests canceled while reading a demuxed packet.
    pub packet_read: u64,
    /// Requests canceled while interacting with a codec.
    pub codec: u64,
    /// Requests canceled while materializing a decoded frame.
    pub frame_materialization: u64,
    /// Requests canceled while executing or reaping an external process.
    pub external_process: u64,
    /// Requests canceled while waiting for a native-output lease to retire.
    pub output_lease: u64,
}

impl PreviewDecodeCancellationCheckpointEvidence {
    fn observe(&mut self, checkpoint: PreviewDecodeCancellationCheckpoint) {
        let counter = match checkpoint {
            PreviewDecodeCancellationCheckpoint::BeforeInputOpen => &mut self.before_input_open,
            PreviewDecodeCancellationCheckpoint::InputOpen => &mut self.input_open,
            PreviewDecodeCancellationCheckpoint::StreamInfo => &mut self.stream_info,
            PreviewDecodeCancellationCheckpoint::CacheLookup => &mut self.cache_lookup,
            PreviewDecodeCancellationCheckpoint::Seek => &mut self.seek,
            PreviewDecodeCancellationCheckpoint::PacketRead => &mut self.packet_read,
            PreviewDecodeCancellationCheckpoint::Codec => &mut self.codec,
            PreviewDecodeCancellationCheckpoint::FrameMaterialization => {
                &mut self.frame_materialization
            }
            PreviewDecodeCancellationCheckpoint::ExternalProcess => &mut self.external_process,
            PreviewDecodeCancellationCheckpoint::OutputLease => &mut self.output_lease,
        };
        *counter = counter.saturating_add(1);
    }
}

pub(super) type PreviewDecodeCancelProbe = Arc<dyn Fn() -> bool + Send + Sync>;

/// Request-local bridge between Mondrian cancellation and FFmpeg blocking I/O.
///
/// The state owns the active probe and records only the first callback
/// observation. A reused decode session installs a fresh guard per request, so
/// cancellation evidence cannot leak across generations.
pub(super) struct PreviewDecodeInterruptState {
    active_probe: Mutex<Option<PreviewDecodeCancelProbe>>,
    current_checkpoint: AtomicU8,
    first_interrupt_checkpoint: AtomicU8,
}

impl PreviewDecodeInterruptState {
    pub(super) fn new() -> Self {
        Self {
            active_probe: Mutex::new(None),
            current_checkpoint: AtomicU8::new(
                PreviewDecodeCancellationCheckpoint::BeforeInputOpen as u8,
            ),
            first_interrupt_checkpoint: AtomicU8::new(0),
        }
    }

    pub(super) fn install(
        self: &Arc<Self>,
        probe: PreviewDecodeCancelProbe,
    ) -> PreviewDecodeInterruptGuard {
        self.current_checkpoint.store(
            PreviewDecodeCancellationCheckpoint::BeforeInputOpen as u8,
            Ordering::Release,
        );
        self.first_interrupt_checkpoint.store(0, Ordering::Release);
        match self.active_probe.lock() {
            Ok(mut active_probe) => *active_probe = Some(probe),
            Err(poisoned) => *poisoned.into_inner() = Some(probe),
        }
        PreviewDecodeInterruptGuard { state: Arc::clone(self) }
    }

    pub(super) fn set_checkpoint(&self, checkpoint: PreviewDecodeCancellationCheckpoint) {
        self.current_checkpoint.store(checkpoint as u8, Ordering::Release);
    }

    pub(super) fn cancellation(
        &self,
        fallback: PreviewDecodeCancellationCheckpoint,
    ) -> PreviewDecodeCancellation {
        PreviewDecodeCancellationCheckpoint::from_u8(
            self.first_interrupt_checkpoint.load(Ordering::Acquire),
        )
        .map(PreviewDecodeCancellation::ffmpeg_interrupt)
        .unwrap_or_else(|| PreviewDecodeCancellation::cooperative(fallback))
    }

    fn should_cancel(&self) -> bool {
        let probe = match self.active_probe.lock() {
            Ok(active_probe) => active_probe.clone(),
            Err(_) => return true,
        };
        let canceled = probe
            .is_some_and(|probe| panic::catch_unwind(AssertUnwindSafe(|| probe())).unwrap_or(true));
        if canceled {
            let checkpoint = self.current_checkpoint.load(Ordering::Acquire);
            let _ = self.first_interrupt_checkpoint.compare_exchange(
                0,
                checkpoint,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        canceled
    }
}

pub(super) struct PreviewDecodeInterruptGuard {
    state: Arc<PreviewDecodeInterruptState>,
}

impl Drop for PreviewDecodeInterruptGuard {
    fn drop(&mut self) {
        match self.state.active_probe.lock() {
            Ok(mut active_probe) => *active_probe = None,
            Err(poisoned) => *poisoned.into_inner() = None,
        }
    }
}

pub(super) unsafe extern "C" fn preview_decode_interrupt_callback(opaque: *mut c_void) -> i32 {
    if opaque.is_null() {
        return 1;
    }
    // SAFETY: every session owns the Arc allocation referenced by the format
    // context until after the input context is dropped.
    let state = unsafe { &*(opaque.cast::<PreviewDecodeInterruptState>()) };
    i32::from(state.should_cancel())
}
