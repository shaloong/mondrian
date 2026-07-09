//! 预览帧解码
//!
//! 使用 avformat_seek_file（安全 Rust API）定位到目标前的关键帧，
//! flush 解码器后向前解码到目标 PTS，保证返回精确帧。

use crate::decoder::{
    DecodedFrameResidency, DecodedGpuFrameHandleKind, DecodedVideoSurfaceFormat, HwAccelBackend,
    HwAccelCodecConfigProbe, HwAccelDeviceContext, HwAccelDeviceContextProbe, HwAccelPixelFormat,
    HwAccelProbe,
};
use ffmpeg_next as ffmpeg;
use mondrian_core::{MondrianError, Result};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Exact still/playback safety limit for forward decode from a keyframe.
///
/// This remains high enough for pathological long-GOP material, but interactive
/// scrub uses a smaller access-policy budget so latest-wins navigation cannot
/// spend seconds draining an old GOP on the CPU fallback path.
const PREVIEW_EXACT_FORWARD_DECODE_BUDGET_FRAMES: usize = 1800;
const PREVIEW_SCRUB_FORWARD_DECODE_BUDGET_FRAMES: usize = 240;
const PREVIEW_SCRUB_UNINDEXED_FORWARD_DECODE_BUDGET_FRAMES: usize = 96;
const PREVIEW_SCRUB_MIN_FORWARD_DECODE_BUDGET_FRAMES: usize = 12;
const PREVIEW_SCRUB_SEEK_BUDGET_PADDING_FRAMES: usize = 4;
const PREVIEW_SCRUB_HOT_FORWARD_DECODE_BUDGET_FRAMES: usize = 72;
const PREVIEW_SCRUB_SLOW_FORWARD_DECODE_BUDGET_FRAMES: usize = 36;
const PREVIEW_SCRUB_RECOVERY_FORWARD_DECODE_BUDGET_FRAMES: usize = 48;
const PREVIEW_SCRUB_HOT_ANY_SEEK_WINDOW_MS: u64 = 250;
const PREVIEW_SCRUB_SLOW_ANY_SEEK_WINDOW_MS: u64 = 120;
const PREVIEW_SCRUB_RECOVERY_ANY_SEEK_WINDOW_MS: u64 = 180;
const PREVIEW_FRAME_CACHE_CAPACITY: usize = 256;
const PREVIEW_SEEK_INDEX_CACHE_CAPACITY: usize = 32;
const PREVIEW_PLAYBACK_SESSION_RING_CAPACITY: usize = 8;
const PREVIEW_HIT_TOLERANCE_SECS: f64 = 0.025;
const PREVIEW_MAX_SELECT_DISTANCE_SECS: f64 = 0.100;
const PREVIEW_PLAYBACK_FORWARD_REUSE_FRAMES: i64 = 48;
const PREVIEW_SCRUB_FORWARD_REUSE_FRAMES: i64 = 2;
const PREVIEW_SCRUB_ANY_SEEK_WINDOW_MS: u64 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PreviewDecodeBackend {
    /// Use the normal in-process FFmpeg decode session.
    #[default]
    Auto,
    /// Force the normal in-process FFmpeg software decode session.
    Software,
    /// Experimental external `ffmpeg` process path that may request platform
    /// hwaccel, but always returns CPU RGBA bytes through stdout.
    ///
    /// This is not Mondrian hardware decode residency and must never be
    /// reported as zero-copy or GPU-resident decode.
    ExternalFfmpegCpuRgba,
}

/// Concrete path that produced a preview RGBA frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreviewDecodePath {
    /// In-process FFmpeg decoder and software scaler returned CPU RGBA bytes.
    InProcessFfmpegCpuRgba,
    /// Experimental external `ffmpeg` process returned CPU RGBA bytes.
    ExternalFfmpegCpuRgba,
    /// Playback cursor reused a frame from its session-local forward ring.
    PlaybackSessionRingHit,
    /// The frame was served from the process-global preview frame cache.
    PreviewCacheHit,
}

/// Caller intent for a preview decode request.
///
/// Mature NLEs treat sustained playback, interactive scrubbing, and precise
/// still-frame extraction as different access patterns. This enum is the media
/// layer contract for that split. The current CPU RGBA adapter may still share
/// implementation code, but callers must choose one mode so future hardware,
/// streaming, and proxy paths can specialize behind this seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PreviewDecodeAccessMode {
    /// Mostly-forward decode for sustained timeline playback and forward prefetch.
    PlaybackCursor,
    /// Latest-wins interactive decode for playhead dragging, jog, and shuttle.
    ScrubCursor,
    /// Deterministic random-access still-frame decode for thumbnails, export
    /// fallback, diagnostics, and exact frame requests.
    RandomAccessStillFrame,
}

/// FFmpeg seek strategy selected by a preview access mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PreviewDecodeSeekStrategy {
    /// Seek to a preceding keyframe and decode forward for exact frame selection.
    #[default]
    KeyframeBefore,
    /// Seek close to the target using FFmpeg's any-frame seek flag for low-latency interaction.
    BoundedAnyFrame,
}

/// Source of keyframe seek-index evidence available to a preview decode session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PreviewSeekIndexSource {
    /// No keyframe seek-index evidence is currently available.
    #[default]
    None,
    /// Evidence was learned incrementally from packets decoded by this session.
    SessionObserved,
    /// Evidence was seeded from the container/probe index before decode work.
    ProbeBacked,
}

/// App-provided scrub scheduling pressure for one preview decode request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PreviewScrubAdaptiveClass {
    /// No scrub-specific pressure was observed by the caller.
    #[default]
    Normal,
    /// Recent scrub requests are clustered in a hot seek region.
    HotRegion,
    /// Recent scrub decode latency exceeded the interactive budget.
    SlowLatency,
    /// Latency recently recovered but remains under a conservative scrub cap.
    Recovery,
}

/// Decode tuning hints that do not change the requested frame semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PreviewDecodeAdaptiveHints {
    /// Scrub pressure selected by the app scheduler for interactive requests.
    pub scrub_class: PreviewScrubAdaptiveClass,
}

/// Caller preference for preview hardware decode / native frame residency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PreviewHardwareDecodeRequest {
    /// Use the current media default; do not require a hardware-resident path.
    #[default]
    Auto,
    /// Prefer a hardware decoder that can produce GPU-resident native frames.
    PreferGpuResident,
    /// Require a hardware-resident decode path; fail closed when unavailable.
    RequireGpuResident,
}

/// Media-layer hardware decode selection outcome for one preview request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PreviewHardwareDecodeDecision {
    /// Hardware decode was not requested and CPU RGBA is the selected path.
    #[default]
    CpuRgbaNotRequested,
    /// Hardware decode was requested but no active GPU-resident adapter exists.
    CpuRgbaHardwareUnavailable,
    /// Hardware decode was requested for an access mode that is not allowed to use it.
    CpuRgbaAccessModeUnsupported,
    /// Hardware decode was requested but the selected backend cannot provide native residency.
    CpuRgbaBackendUnavailable,
    /// Hardware decode was requested but FFmpeg has no matching codec/backend config.
    CpuRgbaCodecUnsupported,
    /// Hardware decode was requested but renderer import for the native surface is unavailable.
    CpuRgbaRendererImportUnavailable,
    /// Hardware decode was requested, but this decode backend only returns CPU RGBA bytes.
    CpuRgbaBackendBoundary,
    /// FFmpeg hardware decode is active, but frames are transferred back to CPU RGBA.
    HardwareDecodeCpuTransfer,
    /// A hardware decoder produced a GPU-resident native frame.
    GpuResidentNative,
}

/// FFmpeg hardware-decode CPU-transfer setup state for one preview result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PreviewHardwareDecodeCpuTransferStatus {
    /// Hardware CPU-transfer fallback was not attempted for this request.
    #[default]
    NotAttempted,
    /// FFmpeg hardware decode was configured; no hardware frame was observed yet.
    ConfiguredAwaitingFrame,
    /// FFmpeg hardware device/context setup failed before opening the decoder.
    SetupFailed,
    /// The hardware-configured decoder failed to open and fell back to software.
    DecoderOpenFailed,
    /// At least one hardware frame was transferred back to CPU.
    Observed,
}

/// Structured hardware-decode blocker observed by the preview decode boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PreviewHardwareDecodeBlocker {
    /// Hardware decode and renderer import are not blocked by the media probe.
    #[default]
    None,
    /// The active media boundary still returns CPU RGBA frames.
    TextureResidencyNotConnected,
    /// A GPU-resident decoder did not report a native handle family.
    GpuHandleMissing,
    /// Decoder GPU residency exists, but renderer import is not ready.
    RendererImportNotReady,
}

impl PreviewHardwareDecodeBlocker {
    fn from_probe(probe: &HwAccelProbe) -> Self {
        if probe.hardware_decode_active
            && probe.zero_copy_active
            && probe.renderer_import_ready
            && probe.gpu_frame_handle_kind.is_some()
        {
            return Self::None;
        }
        if probe.frame_residency == DecodedFrameResidency::CpuRgba {
            return Self::TextureResidencyNotConnected;
        }
        if probe.gpu_frame_handle_kind.is_none() {
            return Self::GpuHandleMissing;
        }
        if !probe.renderer_import_ready {
            return Self::RendererImportNotReady;
        }
        Self::None
    }
}

impl PreviewDecodeSeekStrategy {
    /// Stable seek strategy name for telemetry and diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::KeyframeBefore => "KeyframeBefore",
            Self::BoundedAnyFrame => "BoundedAnyFrame",
        }
    }
}

impl PreviewDecodeAccessMode {
    /// Stable access-mode name for telemetry and diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PlaybackCursor => "PlaybackCursor",
            Self::ScrubCursor => "ScrubCursor",
            Self::RandomAccessStillFrame => "RandomAccessStillFrame",
        }
    }

    fn preserves_session_on_cancel(self) -> bool {
        PreviewDecodeAccessPolicy::for_access_mode(self).preserve_session_on_cancel
    }
}

/// Explicit media-layer request for one scaled preview decode outcome.
///
/// Callers choose a [`PreviewDecodeAccessMode`] as part of the request instead
/// of reimplementing mode-specific FFmpeg routing outside `mondrian-media`.
/// The media layer owns how playback, scrubbing, and still-frame extraction map
/// to session residency, seeking, caching, and future hardware-backed paths.
#[derive(Debug, Clone, Copy)]
pub struct PreviewDecodeRequest<'a> {
    /// Source media path to decode.
    pub path: &'a Path,
    /// Source timestamp in seconds.
    pub timestamp_secs: f64,
    /// Optional maximum output width.
    pub max_width: Option<u32>,
    /// Optional maximum output height.
    pub max_height: Option<u32>,
    /// Access pattern that drives decoder residency and seek policy.
    pub access_mode: PreviewDecodeAccessMode,
    /// Optional stable file fingerprint already resolved by the caller.
    pub fingerprint: Option<PreviewFileFingerprint>,
    /// Adaptive scheduling hints selected by the caller.
    pub adaptive_hints: PreviewDecodeAdaptiveHints,
    /// Hardware decode/native-residency preference selected by the caller.
    pub hardware_decode_request: PreviewHardwareDecodeRequest,
}

impl<'a> PreviewDecodeRequest<'a> {
    /// Create a request for one scaled preview decode outcome.
    pub fn new(path: &'a Path, timestamp_secs: f64, access_mode: PreviewDecodeAccessMode) -> Self {
        Self {
            path,
            timestamp_secs,
            max_width: None,
            max_height: None,
            access_mode,
            fingerprint: None,
            adaptive_hints: PreviewDecodeAdaptiveHints::default(),
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
        }
    }

    /// Set optional maximum output dimensions.
    pub fn with_max_size(mut self, max_width: Option<u32>, max_height: Option<u32>) -> Self {
        self.max_width = max_width;
        self.max_height = max_height;
        self
    }

    /// Attach a caller-resolved file fingerprint.
    pub fn with_fingerprint(mut self, fingerprint: PreviewFileFingerprint) -> Self {
        self.fingerprint = Some(fingerprint);
        self
    }

    /// Attach decode tuning hints that preserve the requested frame semantics.
    pub fn with_adaptive_hints(mut self, adaptive_hints: PreviewDecodeAdaptiveHints) -> Self {
        self.adaptive_hints = adaptive_hints;
        self
    }

    /// Attach a hardware decode/native-residency preference.
    pub fn with_hardware_decode_request(
        mut self,
        hardware_decode_request: PreviewHardwareDecodeRequest,
    ) -> Self {
        self.hardware_decode_request = hardware_decode_request;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PreviewDecodeAccessPolicy {
    access_mode: PreviewDecodeAccessMode,
    forward_reuse_frame_window: i64,
    forward_decode_budget_frames: usize,
    use_playback_ring: bool,
    preserve_session_on_cancel: bool,
    seek_strategy: PreviewDecodeSeekStrategy,
    any_seek_window_ms: u64,
    scrub_adaptive_class: PreviewScrubAdaptiveClass,
}

impl PreviewDecodeAccessPolicy {
    fn for_access_mode(access_mode: PreviewDecodeAccessMode) -> Self {
        match access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => Self {
                access_mode,
                forward_reuse_frame_window: PREVIEW_PLAYBACK_FORWARD_REUSE_FRAMES,
                forward_decode_budget_frames: PREVIEW_EXACT_FORWARD_DECODE_BUDGET_FRAMES,
                use_playback_ring: true,
                preserve_session_on_cancel: true,
                seek_strategy: PreviewDecodeSeekStrategy::KeyframeBefore,
                any_seek_window_ms: 0,
                scrub_adaptive_class: PreviewScrubAdaptiveClass::Normal,
            },
            PreviewDecodeAccessMode::ScrubCursor => Self {
                access_mode,
                forward_reuse_frame_window: PREVIEW_SCRUB_FORWARD_REUSE_FRAMES,
                forward_decode_budget_frames: PREVIEW_SCRUB_FORWARD_DECODE_BUDGET_FRAMES,
                use_playback_ring: false,
                preserve_session_on_cancel: false,
                seek_strategy: PreviewDecodeSeekStrategy::BoundedAnyFrame,
                any_seek_window_ms: PREVIEW_SCRUB_ANY_SEEK_WINDOW_MS,
                scrub_adaptive_class: PreviewScrubAdaptiveClass::Normal,
            },
            PreviewDecodeAccessMode::RandomAccessStillFrame => Self {
                access_mode,
                forward_reuse_frame_window: 0,
                forward_decode_budget_frames: PREVIEW_EXACT_FORWARD_DECODE_BUDGET_FRAMES,
                use_playback_ring: false,
                preserve_session_on_cancel: false,
                seek_strategy: PreviewDecodeSeekStrategy::KeyframeBefore,
                any_seek_window_ms: 0,
                scrub_adaptive_class: PreviewScrubAdaptiveClass::Normal,
            },
        }
    }

    fn apply_adaptive_hints(mut self, adaptive_hints: PreviewDecodeAdaptiveHints) -> Self {
        if self.access_mode != PreviewDecodeAccessMode::ScrubCursor {
            return self;
        }
        self.scrub_adaptive_class = adaptive_hints.scrub_class;
        match adaptive_hints.scrub_class {
            PreviewScrubAdaptiveClass::Normal => {}
            PreviewScrubAdaptiveClass::HotRegion => {
                self.forward_decode_budget_frames = self
                    .forward_decode_budget_frames
                    .min(PREVIEW_SCRUB_HOT_FORWARD_DECODE_BUDGET_FRAMES);
                self.any_seek_window_ms =
                    self.any_seek_window_ms.min(PREVIEW_SCRUB_HOT_ANY_SEEK_WINDOW_MS);
            }
            PreviewScrubAdaptiveClass::SlowLatency => {
                self.forward_decode_budget_frames = self
                    .forward_decode_budget_frames
                    .min(PREVIEW_SCRUB_SLOW_FORWARD_DECODE_BUDGET_FRAMES);
                self.any_seek_window_ms =
                    self.any_seek_window_ms.min(PREVIEW_SCRUB_SLOW_ANY_SEEK_WINDOW_MS);
            }
            PreviewScrubAdaptiveClass::Recovery => {
                self.forward_decode_budget_frames = self
                    .forward_decode_budget_frames
                    .min(PREVIEW_SCRUB_RECOVERY_FORWARD_DECODE_BUDGET_FRAMES);
                self.any_seek_window_ms =
                    self.any_seek_window_ms.min(PREVIEW_SCRUB_RECOVERY_ANY_SEEK_WINDOW_MS);
            }
        }
        self
    }

    fn can_continue_forward(
        self,
        last_pts: i64,
        target_pts: i64,
        frame_duration_pts: i64,
        reached_eof: bool,
    ) -> bool {
        if reached_eof || self.forward_reuse_frame_window <= 0 || target_pts < last_pts {
            return false;
        }
        let max_distance =
            frame_duration_pts.max(1).saturating_mul(self.forward_reuse_frame_window);
        target_pts.saturating_sub(last_pts) <= max_distance
    }

    fn forward_decode_budget_exhausted(self, frames_decoded: usize) -> bool {
        frames_decoded >= self.forward_decode_budget_frames
    }

    fn adapt_for_request(
        mut self,
        seek_index: &PreviewSeekIndex,
        target_pts: i64,
        frame_duration_pts: i64,
        adaptive_hints: PreviewDecodeAdaptiveHints,
    ) -> Self {
        if self.access_mode != PreviewDecodeAccessMode::ScrubCursor {
            return self;
        }
        self = self.apply_adaptive_hints(adaptive_hints);

        let Some(anchor_pts) = seek_index.keyframe_at_or_before(target_pts) else {
            self.forward_decode_budget_frames = self
                .forward_decode_budget_frames
                .min(PREVIEW_SCRUB_UNINDEXED_FORWARD_DECODE_BUDGET_FRAMES);
            return self;
        };

        let frames_from_anchor =
            pts_distance_to_frames(target_pts.saturating_sub(anchor_pts), frame_duration_pts);
        let padded_target_budget =
            frames_from_anchor.saturating_add(PREVIEW_SCRUB_SEEK_BUDGET_PADDING_FRAMES);
        let source_adjusted_budget = match seek_index.source {
            PreviewSeekIndexSource::ProbeBacked => {
                let gop_limited_budget =
                    seek_index.keyframe_after(target_pts).map(|next_keyframe_pts| {
                        pts_distance_to_frames(
                            next_keyframe_pts.saturating_sub(anchor_pts),
                            frame_duration_pts,
                        )
                        .saturating_add(PREVIEW_SCRUB_SEEK_BUDGET_PADDING_FRAMES)
                    });
                gop_limited_budget
                    .map(|gop_budget| padded_target_budget.min(gop_budget))
                    .unwrap_or(padded_target_budget)
            }
            PreviewSeekIndexSource::SessionObserved => padded_target_budget.clamp(
                PREVIEW_SCRUB_MIN_FORWARD_DECODE_BUDGET_FRAMES,
                PREVIEW_SCRUB_UNINDEXED_FORWARD_DECODE_BUDGET_FRAMES,
            ),
            PreviewSeekIndexSource::None => PREVIEW_SCRUB_UNINDEXED_FORWARD_DECODE_BUDGET_FRAMES,
        };

        self.forward_decode_budget_frames = self
            .forward_decode_budget_frames
            .min(source_adjusted_budget.max(PREVIEW_SCRUB_MIN_FORWARD_DECODE_BUDGET_FRAMES));
        self
    }
}

fn pts_distance_to_frames(distance_pts: i64, frame_duration_pts: i64) -> usize {
    if distance_pts <= 0 {
        return 0;
    }
    let distance = distance_pts as u128;
    let frame_duration = frame_duration_pts.max(1) as u128;
    distance
        .saturating_add(frame_duration.saturating_sub(1))
        .saturating_div(frame_duration)
        .min(usize::MAX as u128) as usize
}

/// FFmpeg decoder threading mode requested for preview software decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PreviewDecodeThreadingKind {
    /// Decoder threading disabled.
    None,
    /// Frame-level decoder threading.
    #[default]
    Frame,
    /// Slice-level decoder threading.
    Slice,
}

impl PreviewDecodeThreadingKind {
    /// Stable threading kind name for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Frame => "Frame",
            Self::Slice => "Slice",
        }
    }

    fn from_env(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "none" | "off" | "false" | "0" => Some(Self::None),
            "frame" | "frames" => Some(Self::Frame),
            "slice" | "slices" => Some(Self::Slice),
            _ => None,
        }
    }

    fn to_ffmpeg(self) -> ffmpeg::codec::threading::Type {
        match self {
            Self::None => ffmpeg::codec::threading::Type::None,
            Self::Frame => ffmpeg::codec::threading::Type::Frame,
            Self::Slice => ffmpeg::codec::threading::Type::Slice,
        }
    }

    fn from_ffmpeg(value: ffmpeg::codec::threading::Type) -> Self {
        match value {
            ffmpeg::codec::threading::Type::None => Self::None,
            ffmpeg::codec::threading::Type::Frame => Self::Frame,
            ffmpeg::codec::threading::Type::Slice => Self::Slice,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PreviewDecodeThreadingConfig {
    kind: PreviewDecodeThreadingKind,
    count: usize,
}

/// CPU budget used by preview decode access modes.
///
/// The budget coordinates app-level preview worker count with FFmpeg decoder
/// threads per worker. Keeping these values together prevents the default
/// preview path from multiplying worker threads by decoder threads and
/// starving the UI while software decode is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewDecodeCpuBudget {
    /// Hardware threads visible to the process.
    pub available_parallelism: usize,
    /// Threads intentionally left for UI, render submission, OS, and audio work.
    pub reserved_interactive_threads: usize,
    /// App-level preview decode workers to spawn for playback/scrub/still lanes.
    pub preview_worker_count: usize,
    /// Default FFmpeg decoder threads to request per preview worker.
    pub decoder_threads_per_worker: usize,
    /// Highest decoder thread override accepted for one preview worker.
    pub max_decoder_threads_per_worker: usize,
}

impl PreviewDecodeCpuBudget {
    /// Build the default preview decode CPU budget for a given machine size.
    pub fn for_available_parallelism(available_parallelism: usize) -> Self {
        let available_parallelism = available_parallelism.max(1);
        let reserved_interactive_threads = if available_parallelism >= 8 {
            2
        } else if available_parallelism >= 3 {
            1
        } else {
            0
        };
        let usable_threads =
            available_parallelism.saturating_sub(reserved_interactive_threads).max(1);
        let preview_worker_count = if available_parallelism >= 12 {
            3
        } else if available_parallelism >= 6 {
            2
        } else {
            1
        }
        .min(usable_threads)
        .max(1);
        let max_decoder_threads_per_worker = if available_parallelism >= 16 {
            6
        } else if available_parallelism >= 8 {
            4
        } else {
            3
        }
        .min(usable_threads)
        .max(1);
        let decoder_threads_per_worker = (usable_threads / preview_worker_count)
            .max(1)
            .min(max_decoder_threads_per_worker);

        Self {
            available_parallelism,
            reserved_interactive_threads,
            preview_worker_count,
            decoder_threads_per_worker,
            max_decoder_threads_per_worker,
        }
    }
}

impl Default for PreviewDecodeCpuBudget {
    fn default() -> Self {
        preview_decode_cpu_budget()
    }
}

/// Return the current default preview decode CPU budget.
pub fn preview_decode_cpu_budget() -> PreviewDecodeCpuBudget {
    PreviewDecodeCpuBudget::for_available_parallelism(
        std::thread::available_parallelism()
            .map(|parallelism| parallelism.get())
            .unwrap_or(1),
    )
}

impl PreviewDecodePath {
    /// Stable path name for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InProcessFfmpegCpuRgba => "InProcessFfmpegCpuRgba",
            Self::ExternalFfmpegCpuRgba => "ExternalFfmpegCpuRgba",
            Self::PlaybackSessionRingHit => "PlaybackSessionRingHit",
            Self::PreviewCacheHit => "PreviewCacheHit",
        }
    }
}

/// Stage-level wall-clock timings for preview decode.
///
/// These timings are diagnostic evidence for playback tuning. They are not a
/// real-time scheduling contract because FFmpeg may overlap work internally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PreviewDecodeStageDurations {
    /// Time spent opening or reconfiguring the in-process decode session.
    #[serde(default)]
    pub session_open_us: u64,
    /// Time spent checking the process-global preview frame cache.
    #[serde(default)]
    pub cache_lookup_us: u64,
    /// Time spent seeking and flushing the decoder before forward decode.
    #[serde(default)]
    pub seek_us: u64,
    /// Time spent demuxing packets and receiving decoded frames.
    #[serde(default)]
    pub packet_decode_us: u64,
    /// Time spent transferring FFmpeg hardware frames back to CPU memory.
    #[serde(default)]
    pub hardware_transfer_us: u64,
    /// Time spent in FFmpeg software scaling / pixel-format conversion.
    #[serde(default)]
    pub swscale_us: u64,
    /// Time spent copying packed RGBA rows into Mondrian-owned CPU memory.
    #[serde(default)]
    pub rgba_copy_us: u64,
    /// Time spent waiting for the experimental external ffmpeg process path.
    #[serde(default)]
    pub external_process_us: u64,
}

impl PreviewDecodeStageDurations {
    /// Saturating-add another stage duration set into this one.
    pub fn accumulate(&mut self, other: Self) {
        self.session_open_us = self.session_open_us.saturating_add(other.session_open_us);
        self.cache_lookup_us = self.cache_lookup_us.saturating_add(other.cache_lookup_us);
        self.seek_us = self.seek_us.saturating_add(other.seek_us);
        self.packet_decode_us = self.packet_decode_us.saturating_add(other.packet_decode_us);
        self.hardware_transfer_us =
            self.hardware_transfer_us.saturating_add(other.hardware_transfer_us);
        self.swscale_us = self.swscale_us.saturating_add(other.swscale_us);
        self.rgba_copy_us = self.rgba_copy_us.saturating_add(other.rgba_copy_us);
        self.external_process_us =
            self.external_process_us.saturating_add(other.external_process_us);
    }
}

/// Diagnostics attached to a decoded preview RGBA frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewDecodeDiagnostics {
    /// Concrete decode/cache path that produced the frame.
    pub path: PreviewDecodePath,
    /// End-to-end decode call duration in microseconds.
    pub elapsed_us: u64,
    /// Whether this result came from the preview frame cache.
    pub cache_hit: bool,
    /// Caller intent that selected this decode path.
    pub access_mode: PreviewDecodeAccessMode,
    /// Whether this result came from an external process.
    pub external_process: bool,
    /// Whether the returned payload is CPU-resident memory.
    pub cpu_resident: bool,
    /// Whether the decoder had to seek before producing this frame.
    #[serde(default)]
    pub seek_performed: bool,
    /// Seek strategy requested by the access-mode policy for this frame.
    #[serde(default)]
    pub seek_strategy: PreviewDecodeSeekStrategy,
    /// Forward session reuse window from the access-mode policy, in timeline frames.
    #[serde(default)]
    pub forward_reuse_frame_window: i64,
    /// Maximum decoded frames allowed while scanning forward for this access mode.
    #[serde(default)]
    pub forward_decode_budget_frames: u32,
    /// Bounded-any seek window from the access-mode policy, in milliseconds.
    #[serde(default)]
    pub any_seek_window_ms: u64,
    /// Adaptive scrub pressure applied by the caller.
    #[serde(default)]
    pub scrub_adaptive_class: PreviewScrubAdaptiveClass,
    /// Hardware decode/native-residency preference selected by the caller.
    #[serde(default)]
    pub hardware_decode_request: PreviewHardwareDecodeRequest,
    /// Hardware decode/native-residency selection outcome for this frame.
    #[serde(default)]
    pub hardware_decode_decision: PreviewHardwareDecodeDecision,
    /// Platform-preferred hardware backend candidate, if one is known.
    #[serde(default)]
    pub hardware_decode_candidate_backend: Option<HwAccelBackend>,
    /// Native handle family expected from the candidate backend, if known.
    #[serde(default)]
    pub hardware_decode_candidate_handle_kind: Option<DecodedGpuFrameHandleKind>,
    /// Whether Mondrian has a decoder adapter for the candidate backend.
    #[serde(default)]
    pub hardware_decode_adapter_available: bool,
    /// Whether the linked FFmpeg build lists the requested hardware device type.
    #[serde(default)]
    pub hardware_decode_ffmpeg_device_type_available: bool,
    /// Whether the FFmpeg decoder advertises a hardware config for this backend.
    #[serde(default)]
    pub hardware_decode_ffmpeg_codec_config_available: bool,
    /// Hardware pixel format advertised by FFmpeg for the selected codec/backend.
    #[serde(default)]
    pub hardware_decode_ffmpeg_hw_pixel_format: Option<HwAccelPixelFormat>,
    /// Whether FFmpeg hardware device context creation was attempted.
    #[serde(default)]
    pub hardware_decode_ffmpeg_device_context_attempted: bool,
    /// Whether FFmpeg created a hardware device context for this backend.
    #[serde(default)]
    pub hardware_decode_ffmpeg_device_context_created: bool,
    /// FFmpeg error code returned by hardware device creation, when any.
    #[serde(default)]
    pub hardware_decode_ffmpeg_device_context_error_code: Option<i32>,
    /// Whether the preview session configured FFmpeg hardware decode with CPU transfer.
    #[serde(default)]
    pub hardware_decode_cpu_transfer_configured: bool,
    /// Whether the session observed at least one hardware frame and transferred it to CPU.
    #[serde(default)]
    pub hardware_decode_cpu_transfer_observed: bool,
    /// Structured FFmpeg hardware CPU-transfer setup state.
    #[serde(default)]
    pub hardware_decode_cpu_transfer_status: PreviewHardwareDecodeCpuTransferStatus,
    /// Whether an existing access-mode-local decode session was reused.
    #[serde(default)]
    pub session_reused: bool,
    /// Whether this frame was produced by continuing forward in an existing session without seeking.
    #[serde(default)]
    pub forward_reused: bool,
    /// Whether the session has observed any keyframe index evidence for this stream.
    #[serde(default)]
    pub seek_index_available: bool,
    /// Number of distinct keyframe PTS entries observed by the session-local seek index.
    #[serde(default)]
    pub seek_index_keyframes: u32,
    /// Number of video packets observed while building the session-local seek index.
    #[serde(default)]
    pub seek_index_observed_packets: u32,
    /// Source of keyframe seek-index evidence for this decode session.
    #[serde(default)]
    pub seek_index_source: PreviewSeekIndexSource,
    /// Whether the current seek used a known keyframe anchor from the session-local index.
    #[serde(default)]
    pub seek_index_used: bool,
    /// Keyframe PTS used to bound the current seek, when available.
    #[serde(default)]
    pub seek_index_anchor_pts: Option<i64>,
    /// Decoded frames consumed by this request before selecting the output frame.
    #[serde(default)]
    pub decoded_frame_count: u32,
    /// FFmpeg decoder threading mode active for the decode session.
    #[serde(default)]
    pub threading_kind: PreviewDecodeThreadingKind,
    /// FFmpeg decoder thread count active for the decode session.
    #[serde(default)]
    pub threading_count: u32,
    /// Stage-level decode timings in microseconds.
    #[serde(default)]
    pub stage_durations: PreviewDecodeStageDurations,
    /// Hardware decode backend reported by the media decode boundary.
    #[serde(default)]
    pub hw_accel_backend: HwAccelBackend,
    /// Whether the media decode boundary actively used hardware decode.
    #[serde(default)]
    pub hardware_decode_active: bool,
    /// Whether decoded frames stayed GPU-resident through the media boundary.
    #[serde(default)]
    pub zero_copy_active: bool,
    /// Residency reported by the active preview decode path.
    #[serde(default)]
    pub decoded_frame_residency: DecodedFrameResidency,
    /// Native GPU frame handle family reported by hardware decode, if any.
    #[serde(default)]
    pub gpu_frame_handle_kind: Option<DecodedGpuFrameHandleKind>,
    /// Whether renderer import for the native decoded frame is ready.
    #[serde(default)]
    pub renderer_import_ready: bool,
    /// Structured reason hardware decode / zero-copy is not active.
    #[serde(default)]
    pub hardware_decode_blocker: PreviewHardwareDecodeBlocker,
    /// Decoder output surface format before preview conversion to CPU RGBA.
    #[serde(default)]
    pub decoded_surface_format: DecodedVideoSurfaceFormat,
}

impl PreviewDecodeDiagnostics {
    fn new(path: PreviewDecodePath) -> Self {
        Self {
            path,
            elapsed_us: 0,
            cache_hit: matches!(
                path,
                PreviewDecodePath::PreviewCacheHit | PreviewDecodePath::PlaybackSessionRingHit
            ),
            access_mode: PreviewDecodeAccessMode::RandomAccessStillFrame,
            external_process: path == PreviewDecodePath::ExternalFfmpegCpuRgba,
            cpu_resident: true,
            seek_performed: false,
            seek_strategy: PreviewDecodeSeekStrategy::KeyframeBefore,
            forward_reuse_frame_window: 0,
            forward_decode_budget_frames: 0,
            any_seek_window_ms: 0,
            scrub_adaptive_class: PreviewScrubAdaptiveClass::Normal,
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            hardware_decode_decision: PreviewHardwareDecodeDecision::CpuRgbaNotRequested,
            hardware_decode_candidate_backend: None,
            hardware_decode_candidate_handle_kind: None,
            hardware_decode_adapter_available: false,
            hardware_decode_ffmpeg_device_type_available: false,
            hardware_decode_ffmpeg_codec_config_available: false,
            hardware_decode_ffmpeg_hw_pixel_format: None,
            hardware_decode_ffmpeg_device_context_attempted: false,
            hardware_decode_ffmpeg_device_context_created: false,
            hardware_decode_ffmpeg_device_context_error_code: None,
            hardware_decode_cpu_transfer_configured: false,
            hardware_decode_cpu_transfer_observed: false,
            hardware_decode_cpu_transfer_status:
                PreviewHardwareDecodeCpuTransferStatus::NotAttempted,
            session_reused: false,
            forward_reused: false,
            seek_index_available: false,
            seek_index_keyframes: 0,
            seek_index_observed_packets: 0,
            seek_index_source: PreviewSeekIndexSource::None,
            seek_index_used: false,
            seek_index_anchor_pts: None,
            decoded_frame_count: 0,
            threading_kind: PreviewDecodeThreadingKind::None,
            threading_count: 0,
            stage_durations: PreviewDecodeStageDurations::default(),
            hw_accel_backend: HwAccelBackend::None,
            hardware_decode_active: false,
            zero_copy_active: false,
            decoded_frame_residency: DecodedFrameResidency::CpuRgba,
            gpu_frame_handle_kind: None,
            renderer_import_ready: false,
            hardware_decode_blocker: PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
            decoded_surface_format: DecodedVideoSurfaceFormat::Unknown,
        }
    }

    fn with_elapsed(mut self, elapsed: Duration) -> Self {
        self.elapsed_us = elapsed.as_micros().min(u128::from(u64::MAX)) as u64;
        self
    }

    fn cache_hit_for_mode(elapsed: Duration, access_mode: PreviewDecodeAccessMode) -> Self {
        Self::new(PreviewDecodePath::PreviewCacheHit)
            .with_access_mode(access_mode)
            .with_elapsed(elapsed)
    }

    fn playback_ring_hit(elapsed: Duration) -> Self {
        Self::new(PreviewDecodePath::PlaybackSessionRingHit)
            .with_access_mode(PreviewDecodeAccessMode::PlaybackCursor)
            .with_elapsed(elapsed)
    }

    fn with_access_mode(mut self, access_mode: PreviewDecodeAccessMode) -> Self {
        self.access_mode = access_mode;
        self = self.with_access_policy(PreviewDecodeAccessPolicy::for_access_mode(access_mode));
        self
    }

    fn with_access_policy(mut self, policy: PreviewDecodeAccessPolicy) -> Self {
        self.seek_strategy = policy.seek_strategy;
        self.forward_reuse_frame_window = policy.forward_reuse_frame_window;
        self.forward_decode_budget_frames =
            policy.forward_decode_budget_frames.min(u32::MAX as usize) as u32;
        self.any_seek_window_ms = policy.any_seek_window_ms;
        self.scrub_adaptive_class = policy.scrub_adaptive_class;
        self
    }
}

impl PreviewDecodeBackend {
    fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Software => 1,
            Self::ExternalFfmpegCpuRgba => 2,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Software,
            2 => Self::ExternalFfmpegCpuRgba,
            _ => Self::Auto,
        }
    }
}

static PREVIEW_DECODE_BACKEND: AtomicU8 = AtomicU8::new(0);

pub fn set_preview_decode_backend(backend: PreviewDecodeBackend) {
    PREVIEW_DECODE_BACKEND.store(backend.as_u8(), Ordering::Relaxed);
}

pub fn preview_decode_backend() -> PreviewDecodeBackend {
    PreviewDecodeBackend::from_u8(PREVIEW_DECODE_BACKEND.load(Ordering::Relaxed))
}

#[derive(Debug, Clone)]
pub struct RgbaFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Shared CPU-resident RGBA8 pixels.
    pub data: Arc<Vec<u8>>,
    /// Decode/cache diagnostics for this frame.
    pub diagnostics: PreviewDecodeDiagnostics,
}

/// GPU-resident native decoded preview frame.
///
/// This is the media-layer payload contract for future hardware decoders. The
/// concrete native handle stays behind backend-specific adapters; callers must
/// not reinterpret this as CPU RGBA. Until a platform adapter attaches a real
/// handle, production decode paths continue to return [`RgbaFrame`].
#[derive(Debug, Clone)]
pub struct PreviewNativeDecodedFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Native decoder handle family that owns the frame.
    pub handle_kind: DecodedGpuFrameHandleKind,
    /// Decoder output surface format before renderer import.
    pub surface_format: DecodedVideoSurfaceFormat,
    /// Decode/cache diagnostics for this frame.
    pub diagnostics: PreviewDecodeDiagnostics,
}

impl PreviewNativeDecodedFrame {
    /// Create a GPU-resident native decoded frame payload.
    pub fn new(
        width: u32,
        height: u32,
        handle_kind: DecodedGpuFrameHandleKind,
        surface_format: DecodedVideoSurfaceFormat,
        mut diagnostics: PreviewDecodeDiagnostics,
    ) -> Self {
        diagnostics.cpu_resident = false;
        diagnostics.decoded_frame_residency = DecodedFrameResidency::GpuTexture;
        diagnostics.gpu_frame_handle_kind = Some(handle_kind);
        diagnostics.decoded_surface_format = surface_format;
        Self {
            width,
            height,
            handle_kind,
            surface_format,
            diagnostics,
        }
    }
}

impl RgbaFrame {
    pub(crate) fn new(width: u32, height: u32, data: Vec<u8>, path: PreviewDecodePath) -> Self {
        Self {
            width,
            height,
            data: Arc::new(data),
            diagnostics: PreviewDecodeDiagnostics::new(path),
        }
    }

    /// Borrow decoded RGBA8 pixels.
    pub fn rgba(&self) -> &[u8] {
        self.data.as_slice()
    }

    /// Consume this frame and return shared decoded RGBA8 pixels.
    pub fn into_shared_data(self) -> Arc<Vec<u8>> {
        self.data
    }

    /// Consume this frame and return owned decoded RGBA8 pixels.
    pub fn into_data(self) -> Vec<u8> {
        Arc::try_unwrap(self.data).unwrap_or_else(|data| data.as_ref().clone())
    }

    fn with_elapsed(mut self, elapsed: Duration) -> Self {
        self.diagnostics = self.diagnostics.with_elapsed(elapsed);
        self
    }

    fn with_decode_work(mut self, seek_performed: bool, decoded_frame_count: usize) -> Self {
        self.diagnostics.seek_performed = seek_performed;
        self.diagnostics.decoded_frame_count = decoded_frame_count.min(u32::MAX as usize) as u32;
        self
    }

    fn with_seek_strategy(mut self, seek_strategy: PreviewDecodeSeekStrategy) -> Self {
        self.diagnostics.seek_strategy = seek_strategy;
        self
    }

    fn with_session_reused(mut self, session_reused: bool) -> Self {
        self.diagnostics.session_reused = session_reused;
        self
    }

    fn with_forward_reused(mut self, forward_reused: bool) -> Self {
        self.diagnostics.forward_reused = forward_reused;
        self
    }

    fn with_seek_index_diagnostics(
        mut self,
        diagnostics: PreviewSeekIndexDiagnostics,
        resolution: PreviewSeekResolution,
    ) -> Self {
        self.diagnostics.seek_index_available = diagnostics.available;
        self.diagnostics.seek_index_keyframes = diagnostics.keyframes;
        self.diagnostics.seek_index_observed_packets = diagnostics.observed_packets;
        self.diagnostics.seek_index_source = diagnostics.source;
        self.diagnostics.seek_index_used = resolution.used_index;
        self.diagnostics.seek_index_anchor_pts = resolution.anchor_pts;
        self
    }

    fn with_threading(mut self, kind: PreviewDecodeThreadingKind, count: usize) -> Self {
        self.diagnostics.threading_kind = kind;
        self.diagnostics.threading_count = count.min(u32::MAX as usize) as u32;
        self
    }

    fn with_hw_accel_probe(mut self, probe: &HwAccelProbe) -> Self {
        self.diagnostics.hw_accel_backend = probe.selected_backend;
        self.diagnostics.hardware_decode_candidate_backend = probe.candidate_backend;
        self.diagnostics.hardware_decode_candidate_handle_kind = probe.candidate_handle_kind;
        self.diagnostics.hardware_decode_adapter_available = probe.decoder_adapter_available;
        self.diagnostics.hardware_decode_active = probe.hardware_decode_active;
        self.diagnostics.zero_copy_active = probe.zero_copy_active;
        self.diagnostics.decoded_frame_residency = probe.frame_residency;
        self.diagnostics.gpu_frame_handle_kind = probe.gpu_frame_handle_kind;
        self.diagnostics.renderer_import_ready = probe.renderer_import_ready;
        self.diagnostics.hardware_decode_blocker = PreviewHardwareDecodeBlocker::from_probe(probe);
        self
    }

    fn with_hardware_decode_plan(mut self, plan: &PreviewHardwareDecodePlan) -> Self {
        self.diagnostics.hardware_decode_request = plan.request;
        self.diagnostics.hardware_decode_decision = plan.decision;
        self.diagnostics.hardware_decode_ffmpeg_device_type_available =
            plan.ffmpeg_codec_config.ffmpeg_device_type_available;
        self.diagnostics.hardware_decode_ffmpeg_codec_config_available =
            plan.ffmpeg_codec_config.ffmpeg_codec_config_available;
        self.diagnostics.hardware_decode_ffmpeg_hw_pixel_format =
            plan.ffmpeg_codec_config.hw_pixel_format;
        self.diagnostics.hardware_decode_ffmpeg_device_context_attempted =
            plan.ffmpeg_device_context.device_create_attempted;
        self.diagnostics.hardware_decode_ffmpeg_device_context_created =
            plan.ffmpeg_device_context.device_context_created;
        self.diagnostics.hardware_decode_ffmpeg_device_context_error_code =
            plan.ffmpeg_device_context.device_create_error_code;
        self.diagnostics.hardware_decode_cpu_transfer_configured =
            plan.hardware_cpu_transfer_configured;
        self.diagnostics.hardware_decode_cpu_transfer_observed =
            plan.hardware_cpu_transfer_observed;
        self.diagnostics.hardware_decode_cpu_transfer_status = plan.hardware_cpu_transfer_status;
        self.with_hw_accel_probe(&plan.probe)
    }

    fn with_decoded_surface_format(mut self, format: DecodedVideoSurfaceFormat) -> Self {
        self.diagnostics.decoded_surface_format = format;
        self
    }

    fn with_access_mode(mut self, access_mode: PreviewDecodeAccessMode) -> Self {
        self.diagnostics = self.diagnostics.with_access_mode(access_mode);
        self
    }

    fn with_access_policy(mut self, policy: PreviewDecodeAccessPolicy) -> Self {
        self.diagnostics = self.diagnostics.with_access_policy(policy);
        self
    }

    fn with_stage_durations(mut self, durations: PreviewDecodeStageDurations) -> Self {
        self.diagnostics.stage_durations.accumulate(durations);
        self
    }

    fn into_cache_hit(mut self, elapsed: Duration, access_mode: PreviewDecodeAccessMode) -> Self {
        self.diagnostics = PreviewDecodeDiagnostics::cache_hit_for_mode(elapsed, access_mode);
        self
    }

    fn into_playback_ring_hit(mut self, elapsed: Duration) -> Self {
        self.diagnostics = PreviewDecodeDiagnostics::playback_ring_hit(elapsed);
        self
    }
}

/// Decode one scaled preview frame outcome from an explicit media request.
///
/// This is the single access-mode aware decode boundary. Callers must express
/// playback, scrub, and still-frame work with [`PreviewDecodeRequest`] so
/// `mondrian-media` owns routing, session residency, seeking, caching, and
/// future hardware-backed decode selection.
pub fn decode_preview_frame_cancellable(
    request: PreviewDecodeRequest<'_>,
    should_cancel: impl Fn() -> bool,
) -> Result<PreviewDecodeOutcome> {
    decode_preview_frame_outcome(
        request.path,
        request.timestamp_secs,
        request.max_width,
        request.max_height,
        request.access_mode,
        request.fingerprint,
        request.adaptive_hints,
        request.hardware_decode_request,
        should_cancel,
    )
}

/// Result of a cancellable preview decode request.
#[derive(Debug, Clone)]
pub enum PreviewDecodeOutcome {
    /// Decode completed with a CPU RGBA preview frame.
    Frame(RgbaFrame),
    /// Decode completed with a GPU-resident native frame.
    NativeGpuFrame(PreviewNativeDecodedFrame),
    /// The caller marked this request obsolete before a frame was returned.
    Canceled,
}

thread_local! {
    static PREVIEW_DECODE_SESSIONS: RefCell<PreviewDecodeSessions> = const {
        RefCell::new(PreviewDecodeSessions {
            playback: None,
            scrub: None,
            still: None,
        })
    };
}

/// Drop the current thread's cached preview decode sessions.
///
/// Preview playback, scrubbing, and still-frame extraction keep independent
/// thread-local FFmpeg sessions so one access pattern cannot poison another's
/// decoder state. Call this at explicit lifecycle boundaries, such as perf
/// probes, project/media shutdown, or tests that intentionally open threaded
/// software decoders.
pub fn clear_thread_local_preview_decode_session() {
    PREVIEW_DECODE_SESSIONS.with(|sessions| {
        sessions.borrow_mut().clear();
    });
}

struct PreviewDecodeSessions {
    playback: Option<PreviewDecodeSession>,
    scrub: Option<PreviewDecodeSession>,
    still: Option<PreviewDecodeSession>,
}

impl PreviewDecodeSessions {
    fn slot_mut(
        &mut self,
        access_mode: PreviewDecodeAccessMode,
    ) -> &mut Option<PreviewDecodeSession> {
        match access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => &mut self.playback,
            PreviewDecodeAccessMode::ScrubCursor => &mut self.scrub,
            PreviewDecodeAccessMode::RandomAccessStillFrame => &mut self.still,
        }
    }

    fn clear(&mut self) {
        self.playback = None;
        self.scrub = None;
        self.still = None;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreviewHardwareDecodePlan {
    request: PreviewHardwareDecodeRequest,
    decision: PreviewHardwareDecodeDecision,
    probe: HwAccelProbe,
    ffmpeg_codec_config: HwAccelCodecConfigProbe,
    ffmpeg_device_context: HwAccelDeviceContextProbe,
    hardware_cpu_transfer_configured: bool,
    hardware_cpu_transfer_observed: bool,
    hardware_cpu_transfer_status: PreviewHardwareDecodeCpuTransferStatus,
}

impl PreviewHardwareDecodePlan {
    fn resolve(
        request: PreviewHardwareDecodeRequest,
        access_mode: PreviewDecodeAccessMode,
        backend: PreviewDecodeBackend,
        codec_id: ffmpeg::codec::Id,
    ) -> Self {
        let probe = HwAccelBackend::probe();
        let ffmpeg_codec_config = probe
            .candidate_backend
            .unwrap_or(HwAccelBackend::None)
            .probe_ffmpeg_codec_config(codec_id);
        let ffmpeg_device_context = Self::device_context_probe_for_plan(
            request,
            access_mode,
            backend,
            &probe,
            &ffmpeg_codec_config,
        );
        let decision = Self::decision_for(
            request,
            access_mode,
            backend,
            &probe,
            &ffmpeg_codec_config,
            &ffmpeg_device_context,
        );
        Self {
            request,
            decision,
            probe,
            ffmpeg_codec_config,
            ffmpeg_device_context,
            hardware_cpu_transfer_configured: false,
            hardware_cpu_transfer_observed: false,
            hardware_cpu_transfer_status: PreviewHardwareDecodeCpuTransferStatus::NotAttempted,
        }
    }

    fn should_attempt_hardware_cpu_transfer(&self, access_mode: PreviewDecodeAccessMode) -> bool {
        self.request != PreviewHardwareDecodeRequest::Auto
            && access_mode == PreviewDecodeAccessMode::PlaybackCursor
            && self.ffmpeg_codec_config.ffmpeg_codec_config_available
            && self.ffmpeg_device_context.device_context_created
    }

    fn mark_hardware_cpu_transfer_configured(&mut self, backend: HwAccelBackend) {
        self.hardware_cpu_transfer_configured = true;
        self.hardware_cpu_transfer_status =
            PreviewHardwareDecodeCpuTransferStatus::ConfiguredAwaitingFrame;
        self.probe.reason = format!(
            "{} FFmpeg hardware decode is configured; waiting for hardware frames before reporting active CPU-transfer decode",
            backend.as_str()
        );
    }

    fn mark_hardware_cpu_transfer_setup_failed(&mut self) {
        self.hardware_cpu_transfer_status = PreviewHardwareDecodeCpuTransferStatus::SetupFailed;
    }

    fn mark_hardware_cpu_transfer_decoder_open_failed(&mut self) {
        self.hardware_cpu_transfer_status =
            PreviewHardwareDecodeCpuTransferStatus::DecoderOpenFailed;
    }

    fn mark_hardware_cpu_transfer_observed(&mut self) {
        if self.hardware_cpu_transfer_configured {
            self.hardware_cpu_transfer_observed = true;
            self.hardware_cpu_transfer_status = PreviewHardwareDecodeCpuTransferStatus::Observed;
            self.decision = PreviewHardwareDecodeDecision::HardwareDecodeCpuTransfer;
            let backend = self.probe.candidate_backend.unwrap_or(HwAccelBackend::None);
            self.probe.selected_backend = backend;
            self.probe.hardware_decode_active = true;
            self.probe.zero_copy_active = false;
            self.probe.frame_residency = DecodedFrameResidency::CpuRgba;
            self.probe.gpu_frame_handle_kind = None;
            self.probe.renderer_import_ready = false;
            self.probe.reason = format!(
                "{} FFmpeg hardware decode is active; decoded frames transfer to CPU RGBA until native renderer import is connected",
                backend.as_str()
            );
        }
    }

    fn device_context_probe_for_plan(
        request: PreviewHardwareDecodeRequest,
        access_mode: PreviewDecodeAccessMode,
        backend: PreviewDecodeBackend,
        probe: &HwAccelProbe,
        ffmpeg_codec_config: &HwAccelCodecConfigProbe,
    ) -> HwAccelDeviceContextProbe {
        let selected_backend = probe.candidate_backend.unwrap_or(HwAccelBackend::None);
        if request == PreviewHardwareDecodeRequest::Auto
            || access_mode != PreviewDecodeAccessMode::PlaybackCursor
            || backend == PreviewDecodeBackend::ExternalFfmpegCpuRgba
            || !ffmpeg_codec_config.ffmpeg_codec_config_available
        {
            return HwAccelDeviceContextProbe::unavailable(
                selected_backend,
                "hardware device context creation was not required for this preview plan",
            );
        }
        selected_backend.cached_ffmpeg_device_context_probe()
    }

    fn decision_for(
        request: PreviewHardwareDecodeRequest,
        access_mode: PreviewDecodeAccessMode,
        backend: PreviewDecodeBackend,
        probe: &HwAccelProbe,
        ffmpeg_codec_config: &HwAccelCodecConfigProbe,
        ffmpeg_device_context: &HwAccelDeviceContextProbe,
    ) -> PreviewHardwareDecodeDecision {
        if request == PreviewHardwareDecodeRequest::Auto {
            return PreviewHardwareDecodeDecision::CpuRgbaNotRequested;
        }
        if access_mode != PreviewDecodeAccessMode::PlaybackCursor {
            return PreviewHardwareDecodeDecision::CpuRgbaAccessModeUnsupported;
        }
        if backend == PreviewDecodeBackend::ExternalFfmpegCpuRgba {
            return PreviewHardwareDecodeDecision::CpuRgbaBackendBoundary;
        }
        if probe.candidate_backend.is_none()
            || !ffmpeg_codec_config.backend_maps_to_ffmpeg_device
            || !ffmpeg_codec_config.ffmpeg_device_type_available
        {
            return PreviewHardwareDecodeDecision::CpuRgbaHardwareUnavailable;
        }
        if !ffmpeg_codec_config.ffmpeg_decoder_available
            || !ffmpeg_codec_config.ffmpeg_codec_config_available
        {
            return PreviewHardwareDecodeDecision::CpuRgbaCodecUnsupported;
        }
        if !ffmpeg_device_context.device_context_created {
            return PreviewHardwareDecodeDecision::CpuRgbaHardwareUnavailable;
        }
        if !probe.decoder_adapter_available {
            return PreviewHardwareDecodeDecision::CpuRgbaBackendUnavailable;
        }
        if !probe.hardware_decode_active
            || !probe.zero_copy_active
            || probe.frame_residency != DecodedFrameResidency::GpuTexture
        {
            return PreviewHardwareDecodeDecision::CpuRgbaHardwareUnavailable;
        }
        if probe.gpu_frame_handle_kind.is_none() {
            return PreviewHardwareDecodeDecision::CpuRgbaBackendUnavailable;
        }
        if !probe.renderer_import_ready {
            return PreviewHardwareDecodeDecision::CpuRgbaRendererImportUnavailable;
        }
        PreviewHardwareDecodeDecision::GpuResidentNative
    }
}

struct PreviewHardwareDecodeContextState {
    preferred_hw_pixel_format: ffmpeg::ffi::AVPixelFormat,
}

unsafe extern "C" fn preview_hardware_decode_get_format(
    context: *mut ffmpeg::ffi::AVCodecContext,
    pixel_formats: *const ffmpeg::ffi::AVPixelFormat,
) -> ffmpeg::ffi::AVPixelFormat {
    if context.is_null() || pixel_formats.is_null() {
        return ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NONE;
    }
    let state = unsafe { (*context).opaque as *const PreviewHardwareDecodeContextState };
    if state.is_null() {
        return ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NONE;
    }
    let preferred = unsafe { (*state).preferred_hw_pixel_format };
    let mut index = 0usize;
    let mut first = ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NONE;
    loop {
        let candidate = unsafe { *pixel_formats.add(index) };
        if candidate == ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            return first;
        }
        if first == ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            first = candidate;
        }
        if candidate == preferred {
            return candidate;
        }
        index = index.saturating_add(1);
    }
}

fn preview_hardware_frame_format(format: ffmpeg::util::format::pixel::Pixel) -> bool {
    let format: ffmpeg::ffi::AVPixelFormat = format.into();
    matches!(
        format,
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D11
            | ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D11VA_VLD
            | ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX
            | ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VAAPI
            | ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_CUDA
    )
}

struct PreviewDecodeSession {
    path: PathBuf,
    fingerprint: PreviewFileFingerprint,
    max_width: Option<u32>,
    max_height: Option<u32>,
    backend: PreviewDecodeBackend,
    hardware_decode_request: PreviewHardwareDecodeRequest,
    codec_id: ffmpeg::codec::Id,
    input: ffmpeg::format::context::Input,
    decoder: ffmpeg::decoder::Video,
    scaler: Option<ffmpeg::software::scaling::Context>,
    scaler_source_format: Option<ffmpeg::util::format::pixel::Pixel>,
    stream_index: usize,
    stream_tb: ffmpeg::Rational,
    frame_duration_pts: i64,
    hit_tolerance_pts: i64,
    target_width: u32,
    target_height: u32,
    _hardware_decode_context_state: Option<Box<PreviewHardwareDecodeContextState>>,
    threading_kind: PreviewDecodeThreadingKind,
    threading_count: usize,
    hardware_decode_plan: PreviewHardwareDecodePlan,
    decoded_surface_format: DecodedVideoSurfaceFormat,
    last_pts: Option<i64>,
    reached_eof: bool,
    playback_ring: PreviewPlaybackRing,
    seek_index: PreviewSeekIndex,
}

struct PreviewDecodeForwardResult {
    frame: Option<RgbaFrame>,
    selected_pts: Option<i64>,
    decoded_frame_count: usize,
    canceled: bool,
}

impl PreviewDecodeForwardResult {
    fn frame(frame: RgbaFrame, selected_pts: i64, decoded_frame_count: usize) -> Self {
        Self {
            frame: Some(frame),
            selected_pts: Some(selected_pts),
            decoded_frame_count,
            canceled: false,
        }
    }

    fn empty(decoded_frame_count: usize) -> Self {
        Self {
            frame: None,
            selected_pts: None,
            decoded_frame_count,
            canceled: false,
        }
    }

    fn canceled(decoded_frame_count: usize) -> Self {
        Self {
            frame: None,
            selected_pts: None,
            decoded_frame_count,
            canceled: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PreviewSeekIndexDiagnostics {
    available: bool,
    keyframes: u32,
    observed_packets: u32,
    source: PreviewSeekIndexSource,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PreviewSeekResolution {
    used_index: bool,
    anchor_pts: Option<i64>,
}

#[derive(Debug, Default)]
struct PreviewSeekIndex {
    keyframe_pts: Vec<i64>,
    observed_packets: u32,
    source: PreviewSeekIndexSource,
}

impl PreviewSeekIndex {
    fn from_probe_keyframes(mut keyframe_pts: Vec<i64>) -> Self {
        keyframe_pts.sort_unstable();
        keyframe_pts.dedup();
        let source = if keyframe_pts.is_empty() {
            PreviewSeekIndexSource::None
        } else {
            PreviewSeekIndexSource::ProbeBacked
        };
        Self { keyframe_pts, observed_packets: 0, source }
    }

    fn observe_packet(&mut self, packet: &ffmpeg::Packet) {
        self.observed_packets = self.observed_packets.saturating_add(1);
        if !packet.is_key() {
            return;
        }
        let Some(pts) = packet.pts().or_else(|| packet.dts()) else {
            return;
        };
        let inserted = self.insert_keyframe_pts(pts);
        if inserted && self.source == PreviewSeekIndexSource::None {
            self.source = PreviewSeekIndexSource::SessionObserved;
        }
    }

    fn insert_keyframe_pts(&mut self, pts: i64) -> bool {
        match self.keyframe_pts.binary_search(&pts) {
            Ok(_) => false,
            Err(index) => {
                self.keyframe_pts.insert(index, pts);
                true
            }
        }
    }

    fn keyframe_at_or_before(&self, target_pts: i64) -> Option<i64> {
        match self.keyframe_pts.binary_search(&target_pts) {
            Ok(index) => self.keyframe_pts.get(index).copied(),
            Err(0) => None,
            Err(index) => self.keyframe_pts.get(index - 1).copied(),
        }
    }

    fn keyframe_after(&self, target_pts: i64) -> Option<i64> {
        match self.keyframe_pts.binary_search(&target_pts) {
            Ok(index) => self.keyframe_pts.get(index + 1).copied(),
            Err(index) => self.keyframe_pts.get(index).copied(),
        }
    }

    fn diagnostics(&self) -> PreviewSeekIndexDiagnostics {
        PreviewSeekIndexDiagnostics {
            available: !self.keyframe_pts.is_empty(),
            keyframes: self.keyframe_pts.len().min(u32::MAX as usize) as u32,
            observed_packets: self.observed_packets,
            source: self.source,
        }
    }
}

#[derive(Debug, Clone)]
struct PreviewSeekIndexCacheEntry {
    path: PathBuf,
    fingerprint: PreviewFileFingerprint,
    stream_index: usize,
    keyframe_pts: Vec<i64>,
}

static PREVIEW_SEEK_INDEX_CACHE: OnceLock<Mutex<VecDeque<PreviewSeekIndexCacheEntry>>> =
    OnceLock::new();

fn preview_seek_index_cache() -> &'static Mutex<VecDeque<PreviewSeekIndexCacheEntry>> {
    PREVIEW_SEEK_INDEX_CACHE.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn preview_seek_index_cache_get(
    path: &Path,
    fingerprint: PreviewFileFingerprint,
    stream_index: usize,
) -> Option<PreviewSeekIndex> {
    let mut cache = preview_seek_index_cache().lock().ok()?;
    let position = cache.iter().position(|entry| {
        entry.path == path && entry.fingerprint == fingerprint && entry.stream_index == stream_index
    })?;
    let entry = cache.remove(position)?;
    let seek_index = PreviewSeekIndex::from_probe_keyframes(entry.keyframe_pts.clone());
    cache.push_front(entry);
    Some(seek_index)
}

fn preview_seek_index_cache_put(
    path: &Path,
    fingerprint: PreviewFileFingerprint,
    stream_index: usize,
    keyframe_pts: &[i64],
) {
    if keyframe_pts.is_empty() {
        return;
    }
    let Ok(mut cache) = preview_seek_index_cache().lock() else {
        return;
    };
    if let Some(position) = cache.iter().position(|entry| {
        entry.path == path && entry.fingerprint == fingerprint && entry.stream_index == stream_index
    }) {
        cache.remove(position);
    }
    cache.push_front(PreviewSeekIndexCacheEntry {
        path: path.to_path_buf(),
        fingerprint,
        stream_index,
        keyframe_pts: keyframe_pts.to_vec(),
    });
    while cache.len() > PREVIEW_SEEK_INDEX_CACHE_CAPACITY {
        cache.pop_back();
    }
}

fn preview_seek_index_from_stream(stream: &ffmpeg::format::stream::Stream<'_>) -> PreviewSeekIndex {
    let stream_ptr = unsafe { stream.as_ptr() };
    if stream_ptr.is_null() {
        return PreviewSeekIndex::default();
    }
    let entry_count = unsafe { ffmpeg::ffi::avformat_index_get_entries_count(stream_ptr) };
    if entry_count <= 0 {
        return PreviewSeekIndex::default();
    }

    let mut keyframe_pts = Vec::with_capacity(entry_count as usize);
    for entry_index in 0..entry_count {
        let entry_ptr =
            unsafe { ffmpeg::ffi::avformat_index_get_entry(stream_ptr.cast_mut(), entry_index) };
        if entry_ptr.is_null() {
            continue;
        }
        let entry = unsafe { &*entry_ptr };
        if entry.timestamp == ffmpeg::ffi::AV_NOPTS_VALUE {
            continue;
        }
        if entry.flags() & ffmpeg::ffi::AVINDEX_KEYFRAME == 0 {
            continue;
        }
        keyframe_pts.push(entry.timestamp);
    }
    PreviewSeekIndex::from_probe_keyframes(keyframe_pts)
}

struct PreviewPlaybackRingEntry {
    pts: i64,
    frame: RgbaFrame,
}

struct PreviewPlaybackRing {
    capacity: usize,
    entries: VecDeque<PreviewPlaybackRingEntry>,
}

impl PreviewPlaybackRing {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: VecDeque::new(),
        }
    }

    fn get(&mut self, target_pts: i64, tolerance_pts: i64) -> Option<RgbaFrame> {
        let mut best_index = None;
        let mut best_distance = i64::MAX;
        for (index, entry) in self.entries.iter().enumerate() {
            let distance = (entry.pts - target_pts).abs();
            if distance < best_distance {
                best_distance = distance;
                best_index = Some(index);
            }
        }
        let index = best_index?;
        if best_distance > tolerance_pts.max(1) {
            return None;
        }

        let entry = self.entries.remove(index)?;
        let frame = entry.frame.clone();
        self.entries.push_front(entry);
        Some(frame)
    }

    fn put(&mut self, pts: i64, frame: RgbaFrame) {
        if let Some(index) = self.entries.iter().position(|entry| entry.pts == pts) {
            self.entries.remove(index);
        }
        self.entries.push_front(PreviewPlaybackRingEntry { pts, frame });
        while self.entries.len() > self.capacity {
            self.entries.pop_back();
        }
    }
}

/// Stable file identity used to invalidate preview decode sessions and frames.
///
/// The optional fields let callers represent missing/unreadable metadata
/// without falling back to a false stable identity. A successful app/media path
/// probe should prefer [`PreviewFileFingerprint::from_metadata`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PreviewFileFingerprint {
    /// File length in bytes when available.
    pub len: Option<u64>,
    /// File modification time seconds since Unix epoch when available.
    pub modified_secs: Option<u64>,
    /// File modification time subsecond nanoseconds when available.
    pub modified_nanos: Option<u32>,
}

impl PreviewFileFingerprint {
    /// Capture a fingerprint from the filesystem, preserving missing metadata.
    pub fn capture(path: &Path) -> Self {
        let Ok(metadata) = std::fs::metadata(path) else {
            return Self {
                len: None,
                modified_secs: None,
                modified_nanos: None,
            };
        };
        Self::from_metadata(&metadata)
    }

    /// Build a fingerprint from a metadata record the caller already fetched.
    pub fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        let modified =
            metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok());
        Self {
            len: Some(metadata.len()),
            modified_secs: modified.map(|duration| duration.as_secs()),
            modified_nanos: modified.map(|duration| duration.subsec_nanos()),
        }
    }
}

fn preview_decode_context_from_parameters(
    parameters: ffmpeg::codec::Parameters,
    threading: ffmpeg::codec::threading::Config,
    path: &Path,
) -> Result<ffmpeg::codec::context::Context> {
    let mut context =
        ffmpeg::codec::context::Context::from_parameters(parameters).map_err(|e| {
            MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: e.to_string(),
            }
        })?;
    context.set_threading(threading);
    Ok(context)
}

fn configure_preview_hardware_decode_context(
    context: &mut ffmpeg::codec::context::Context,
    plan: &PreviewHardwareDecodePlan,
) -> std::result::Result<(Box<PreviewHardwareDecodeContextState>, HwAccelDeviceContext), String> {
    let backend = plan
        .probe
        .candidate_backend
        .ok_or_else(|| "no platform hardware decode backend candidate".to_owned())?;
    let hw_pixel_format = plan
        .ffmpeg_codec_config
        .hw_pixel_format
        .and_then(HwAccelPixelFormat::to_ffmpeg)
        .ok_or_else(|| {
            "FFmpeg codec config did not expose a usable hardware pixel format".to_owned()
        })?;
    let device_context = backend.create_ffmpeg_device_context().map_err(|probe| probe.reason)?;
    device_context.attach_to_codec_context(context)?;

    let mut state =
        Box::new(PreviewHardwareDecodeContextState { preferred_hw_pixel_format: hw_pixel_format });
    unsafe {
        (*context.as_mut_ptr()).opaque = (&mut *state) as *mut _ as *mut c_void;
        (*context.as_mut_ptr()).get_format = Some(preview_hardware_decode_get_format);
    }
    Ok((state, device_context))
}

fn preview_create_rgba_scaler(
    source_format: ffmpeg::util::format::pixel::Pixel,
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
    path: &Path,
) -> Result<ffmpeg::software::scaling::Context> {
    ffmpeg::software::scaling::Context::get(
        source_format,
        source_width,
        source_height,
        ffmpeg::util::format::pixel::Pixel::RGBA,
        target_width,
        target_height,
        ffmpeg::software::scaling::flag::Flags::FAST_BILINEAR,
    )
    .map_err(|e| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: e.to_string(),
    })
}

impl PreviewDecodeSession {
    fn open(
        path: &Path,
        fingerprint: PreviewFileFingerprint,
        max_width: Option<u32>,
        max_height: Option<u32>,
        access_mode: PreviewDecodeAccessMode,
        backend: PreviewDecodeBackend,
        hardware_decode_request: PreviewHardwareDecodeRequest,
    ) -> Result<Self> {
        let input = ffmpeg::format::input(path).map_err(|e| MondrianError::MediaOpen {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
        Self::from_input(
            input,
            path,
            fingerprint,
            max_width,
            max_height,
            access_mode,
            backend,
            hardware_decode_request,
        )
    }

    fn from_input(
        input: ffmpeg::format::context::Input,
        path: &Path,
        fingerprint: PreviewFileFingerprint,
        max_width: Option<u32>,
        max_height: Option<u32>,
        access_mode: PreviewDecodeAccessMode,
        backend: PreviewDecodeBackend,
        hardware_decode_request: PreviewHardwareDecodeRequest,
    ) -> Result<Self> {
        let (stream_index, parameters, stream_tb, stream_rate, seek_index) = {
            let stream = input.streams().best(ffmpeg::media::Type::Video).ok_or_else(|| {
                MondrianError::UnsupportedFormat { format: "no video stream".to_string() }
            })?;
            let stream_index = stream.index();
            let seek_index = preview_seek_index_cache_get(path, fingerprint, stream_index)
                .unwrap_or_else(|| {
                    let seek_index = preview_seek_index_from_stream(&stream);
                    if seek_index.source == PreviewSeekIndexSource::ProbeBacked {
                        preview_seek_index_cache_put(
                            path,
                            fingerprint,
                            stream_index,
                            &seek_index.keyframe_pts,
                        );
                    }
                    seek_index
                });
            (
                stream_index,
                stream.parameters(),
                stream.time_base(),
                stream.rate(),
                seek_index,
            )
        };
        let codec_id = parameters.id();

        let mut hardware_decode_plan = PreviewHardwareDecodePlan::resolve(
            hardware_decode_request,
            access_mode,
            backend,
            codec_id,
        );
        let requested_threading = preview_decode_threading_config();
        let ffmpeg_threading = ffmpeg::codec::threading::Config {
            kind: requested_threading.kind.to_ffmpeg(),
            count: requested_threading.count,
        };

        let mut hardware_decode_context_state = None;
        let mut context =
            preview_decode_context_from_parameters(parameters.clone(), ffmpeg_threading, path)?;
        if hardware_decode_plan.should_attempt_hardware_cpu_transfer(access_mode)
            && backend != PreviewDecodeBackend::Software
        {
            match configure_preview_hardware_decode_context(&mut context, &hardware_decode_plan) {
                Ok((state, device_context)) => {
                    hardware_decode_plan
                        .mark_hardware_cpu_transfer_configured(device_context.backend());
                    hardware_decode_context_state = Some(state);
                }
                Err(reason) => {
                    hardware_decode_plan.mark_hardware_cpu_transfer_setup_failed();
                    preview_trace(format!(
                        "[preview] hardware decode CPU-transfer setup failed, fallback software: {reason}"
                    ));
                }
            }
        }

        let decoder = match context.decoder().video() {
            Ok(decoder) => decoder,
            Err(err) if hardware_decode_context_state.is_some() => {
                preview_trace(format!(
                    "[preview] hardware decode open failed, fallback software: {err}"
                ));
                hardware_decode_plan.mark_hardware_cpu_transfer_decoder_open_failed();
                hardware_decode_context_state = None;
                preview_decode_context_from_parameters(parameters, ffmpeg_threading, path)?
                    .decoder()
                    .video()
                    .map_err(|e| MondrianError::DecodeFailed {
                        asset_id: path.display().to_string(),
                        reason: e.to_string(),
                    })?
            }
            Err(err) => {
                return Err(MondrianError::DecodeFailed {
                    asset_id: path.display().to_string(),
                    reason: err.to_string(),
                });
            }
        };
        let active_threading = decoder.threading();
        let threading_kind = PreviewDecodeThreadingKind::from_ffmpeg(active_threading.kind);
        let threading_count = active_threading.count;
        let decoded_surface_format = decoded_surface_format_from_pixel(decoder.format());

        let (target_width, target_height) =
            fit_target_size(decoder.width(), decoder.height(), max_width, max_height);

        let defer_scaler_until_cpu_transfer = hardware_decode_context_state.is_some()
            && preview_hardware_frame_format(decoder.format());
        let (scaler, scaler_source_format) = if defer_scaler_until_cpu_transfer {
            (None, None)
        } else {
            (
                Some(preview_create_rgba_scaler(
                    decoder.format(),
                    decoder.width(),
                    decoder.height(),
                    target_width,
                    target_height,
                    path,
                )?),
                Some(decoder.format()),
            )
        };

        let frame_duration_pts = estimate_frame_duration_pts(stream_tb, stream_rate).max(1);
        let max_hit_tolerance_pts = seconds_to_stream_pts(PREVIEW_HIT_TOLERANCE_SECS, stream_tb);
        let hit_tolerance_pts = (frame_duration_pts / 2).max(1).min(max_hit_tolerance_pts.max(1));

        Ok(Self {
            path: path.to_path_buf(),
            fingerprint,
            max_width,
            max_height,
            backend,
            hardware_decode_request,
            codec_id,
            input,
            decoder,
            scaler,
            scaler_source_format,
            stream_index,
            stream_tb,
            frame_duration_pts,
            hit_tolerance_pts,
            target_width,
            target_height,
            _hardware_decode_context_state: hardware_decode_context_state,
            threading_kind,
            threading_count,
            hardware_decode_plan,
            decoded_surface_format,
            last_pts: None,
            reached_eof: false,
            playback_ring: PreviewPlaybackRing::new(PREVIEW_PLAYBACK_SESSION_RING_CAPACITY),
            seek_index,
        })
    }

    fn matches(
        &self,
        path: &Path,
        fingerprint: PreviewFileFingerprint,
        max_width: Option<u32>,
        max_height: Option<u32>,
        backend: PreviewDecodeBackend,
        hardware_decode_request: PreviewHardwareDecodeRequest,
    ) -> bool {
        self.path == path
            && self.fingerprint == fingerprint
            && self.max_width == max_width
            && self.max_height == max_height
            && self.backend == backend
            && self.hardware_decode_request == hardware_decode_request
    }

    fn decode_at(
        &mut self,
        timestamp_secs: f64,
        access_mode: PreviewDecodeAccessMode,
        adaptive_hints: PreviewDecodeAdaptiveHints,
        should_cancel: &impl Fn() -> bool,
    ) -> Result<PreviewDecodeOutcome> {
        if should_cancel() {
            return Ok(PreviewDecodeOutcome::Canceled);
        }
        let target_pts = timestamp_to_stream_pts(timestamp_secs, self.stream_tb);
        let policy = PreviewDecodeAccessPolicy::for_access_mode(access_mode).adapt_for_request(
            &self.seek_index,
            target_pts,
            self.frame_duration_pts,
            adaptive_hints,
        );

        let cache_lookup_started_at = Instant::now();
        if policy.use_playback_ring {
            if let Some(hit) = self.playback_ring.get(target_pts, self.hit_tolerance_pts) {
                if should_cancel() {
                    return Ok(PreviewDecodeOutcome::Canceled);
                }
                return Ok(PreviewDecodeOutcome::Frame(
                    hit.into_playback_ring_hit(cache_lookup_started_at.elapsed())
                        .with_access_policy(policy)
                        .with_seek_index_diagnostics(
                            self.seek_index.diagnostics(),
                            PreviewSeekResolution::default(),
                        )
                        .with_stage_durations(PreviewDecodeStageDurations {
                            cache_lookup_us: duration_us(cache_lookup_started_at.elapsed()),
                            ..PreviewDecodeStageDurations::default()
                        })
                        .with_hardware_decode_plan(&self.hardware_decode_plan)
                        .with_decoded_surface_format(self.decoded_surface_format),
                ));
            }
        }
        if let Some(hit) = preview_cache_get(
            &self.path,
            self.fingerprint,
            self.target_width,
            self.target_height,
            target_pts,
            self.hit_tolerance_pts,
        ) {
            if should_cancel() {
                return Ok(PreviewDecodeOutcome::Canceled);
            }
            if policy.use_playback_ring {
                self.playback_ring.put(hit.pts, hit.frame.clone());
            }
            return Ok(PreviewDecodeOutcome::Frame(
                hit.frame
                    .into_cache_hit(cache_lookup_started_at.elapsed(), access_mode)
                    .with_access_policy(policy)
                    .with_seek_index_diagnostics(
                        self.seek_index.diagnostics(),
                        PreviewSeekResolution::default(),
                    )
                    .with_stage_durations(PreviewDecodeStageDurations {
                        cache_lookup_us: duration_us(cache_lookup_started_at.elapsed()),
                        ..PreviewDecodeStageDurations::default()
                    })
                    .with_hardware_decode_plan(&self.hardware_decode_plan)
                    .with_decoded_surface_format(self.decoded_surface_format),
            ));
        }
        let cache_lookup_us = duration_us(cache_lookup_started_at.elapsed());

        let should_continue_forward = self
            .last_pts
            .map(|last| {
                policy.can_continue_forward(
                    last,
                    target_pts,
                    self.frame_duration_pts,
                    self.reached_eof,
                )
            })
            .unwrap_or(false);

        let seek_performed = !should_continue_forward;
        let mut seek_resolution = PreviewSeekResolution::default();
        let mut seek_us = 0;
        if seek_performed {
            if should_cancel() {
                return Ok(PreviewDecodeOutcome::Canceled);
            }
            let seek_started_at = Instant::now();
            seek_resolution = self.seek_to_target(target_pts, policy)?;
            seek_us = duration_us(seek_started_at.elapsed());
        }

        if should_cancel() {
            return Ok(PreviewDecodeOutcome::Canceled);
        }
        let decode_started_at = Instant::now();
        let result = self.decode_forward_until(target_pts, policy, should_cancel)?;
        if result.canceled {
            return Ok(PreviewDecodeOutcome::Canceled);
        }
        if let Some(frame) = result.frame {
            if policy.use_playback_ring {
                if let Some(selected_pts) = result.selected_pts {
                    self.playback_ring.put(selected_pts, frame.clone());
                }
            }
            let conversion_us = frame
                .diagnostics
                .stage_durations
                .hardware_transfer_us
                .saturating_add(frame.diagnostics.stage_durations.swscale_us)
                .saturating_add(frame.diagnostics.stage_durations.rgba_copy_us);
            let packet_decode_us =
                duration_us(decode_started_at.elapsed()).saturating_sub(conversion_us);
            return Ok(PreviewDecodeOutcome::Frame(
                frame
                    .with_access_mode(access_mode)
                    .with_stage_durations(PreviewDecodeStageDurations {
                        cache_lookup_us,
                        seek_us,
                        packet_decode_us,
                        ..PreviewDecodeStageDurations::default()
                    })
                    .with_decode_work(seek_performed, result.decoded_frame_count)
                    .with_access_policy(policy)
                    .with_forward_reused(should_continue_forward)
                    .with_seek_index_diagnostics(self.seek_index.diagnostics(), seek_resolution)
                    .with_threading(self.threading_kind, self.threading_count)
                    .with_hardware_decode_plan(&self.hardware_decode_plan)
                    .with_decoded_surface_format(self.decoded_surface_format),
            ));
        }

        Err(MondrianError::DecodeFailed {
            asset_id: self.path.display().to_string(),
            reason: "no decodable frame".to_string(),
        })
    }

    fn seek_to_target(
        &mut self,
        target_pts: i64,
        policy: PreviewDecodeAccessPolicy,
    ) -> Result<PreviewSeekResolution> {
        let tb_num = self.stream_tb.numerator() as f64;
        let tb_den = self.stream_tb.denominator() as f64;

        if tb_den <= 0.0 || tb_num <= 0.0 {
            // time_base 无效：无法计算合理的安全窗口，直接报错
            // 而非静默返回 Ok(())（静默返回会导致从文件当前位置解码，产生错误帧）
            return Err(MondrianError::DecodeFailed {
                asset_id: self.path.display().to_string(),
                reason: format!(
                    "invalid stream time_base {}/{}: cannot seek to pts={}",
                    self.stream_tb.numerator(),
                    self.stream_tb.denominator(),
                    target_pts
                ),
            });
        }

        let tb_secs = tb_num / tb_den;

        let seek_anchor_pts = self.seek_index.keyframe_at_or_before(target_pts);
        let (min_ts, max_ts, seek_flags, used_anchor_pts) = match policy.seek_strategy {
            PreviewDecodeSeekStrategy::KeyframeBefore => {
                // 关键帧安全模式：不限制 backward seek 范围，避免长 GOP 时落到不可独立解码帧。
                (
                    seek_anchor_pts.unwrap_or(i64::MIN),
                    target_pts,
                    ffmpeg::ffi::AVSEEK_FLAG_BACKWARD,
                    seek_anchor_pts,
                )
            }
            PreviewDecodeSeekStrategy::BoundedAnyFrame => {
                let seek_window_secs = policy.any_seek_window_ms as f64 / 1_000.0;
                let seek_window_pts = (seek_window_secs / tb_secs).round().max(1.0) as i64;
                let window_min_ts = target_pts.saturating_sub(seek_window_pts);
                let used_anchor_pts = seek_anchor_pts
                    .filter(|anchor| *anchor >= window_min_ts && *anchor <= target_pts);
                let min_ts = used_anchor_pts.unwrap_or(window_min_ts);
                (
                    min_ts,
                    target_pts.saturating_add(seek_window_pts),
                    ffmpeg::ffi::AVSEEK_FLAG_ANY,
                    used_anchor_pts,
                )
            }
        };

        let ret = unsafe {
            ffmpeg::ffi::avformat_seek_file(
                self.input.as_mut_ptr(),
                self.stream_index as i32,
                min_ts,
                target_pts,
                max_ts,
                seek_flags,
            )
        };

        if ret >= 0 {
            unsafe {
                ffmpeg::ffi::avcodec_flush_buffers(self.decoder.as_mut_ptr());
            }
            self.reached_eof = false;
            self.last_pts = None;
            return Ok(PreviewSeekResolution {
                used_index: used_anchor_pts.is_some(),
                anchor_pts: used_anchor_pts,
            });
        }

        Err(MondrianError::DecodeFailed {
            asset_id: self.path.display().to_string(),
            reason: format!("seek failed with code {ret}"),
        })
    }

    fn decode_forward_until(
        &mut self,
        target_pts: i64,
        policy: PreviewDecodeAccessPolicy,
        should_cancel: &impl Fn() -> bool,
    ) -> Result<PreviewDecodeForwardResult> {
        let mut best_before: Option<(i64, ffmpeg::util::frame::video::Video)> = None;
        let mut best_after: Option<(i64, ffmpeg::util::frame::video::Video)> = None;
        let mut frames_decoded: usize = 0;
        let max_select_distance_pts =
            self.frame_duration_pts.saturating_mul(2).max(1).min(
                seconds_to_stream_pts(PREVIEW_MAX_SELECT_DISTANCE_SECS, self.stream_tb).max(1),
            );

        let choose_and_convert =
            |hardware_decode_plan: &mut PreviewHardwareDecodePlan,
             scaler: &mut Option<ffmpeg::software::scaling::Context>,
             scaler_source_format: &mut Option<ffmpeg::util::format::pixel::Pixel>,
             target_width: u32,
             target_height: u32,
             path: &Path,
             before: Option<&(i64, ffmpeg::util::frame::video::Video)>,
             after: Option<&(i64, ffmpeg::util::frame::video::Video)>|
             -> Result<Option<(i64, RgbaFrame)>> {
                if should_cancel() {
                    return Ok(None);
                }
                let selected = match (before, after) {
                    (Some((b_pts, b_frame)), Some((a_pts, a_frame))) => {
                        let before_dist = (target_pts - *b_pts).abs();
                        let after_dist = (*a_pts - target_pts).abs();
                        if before_dist <= after_dist {
                            Some((*b_pts, b_frame))
                        } else {
                            Some((*a_pts, a_frame))
                        }
                    }
                    (Some((b_pts, b_frame)), None) => Some((*b_pts, b_frame)),
                    (None, Some((a_pts, a_frame))) => Some((*a_pts, a_frame)),
                    (None, None) => None,
                };

                let Some((selected_pts, selected_frame)) = selected else {
                    return Ok(None);
                };

                let selected_distance = (selected_pts - target_pts).abs();
                if selected_distance > max_select_distance_pts {
                    return Ok(None);
                }

                if should_cancel() {
                    return Ok(None);
                }
                let rgba = materialize_decoded_to_rgba(
                    selected_frame,
                    hardware_decode_plan,
                    scaler,
                    scaler_source_format,
                    target_width,
                    target_height,
                    path,
                )?;
                Ok(Some((selected_pts, rgba)))
            };

        for (s, packet) in self.input.packets() {
            if should_cancel() {
                return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
            }
            if s.index() != self.stream_index {
                continue;
            }
            self.seek_index.observe_packet(&packet);
            if policy.forward_decode_budget_exhausted(frames_decoded) {
                break;
            }

            self.decoder.send_packet(&packet).map_err(|e| MondrianError::DecodeFailed {
                asset_id: self.path.display().to_string(),
                reason: e.to_string(),
            })?;

            while let Some(decoded) = receive_decoded_video_frame(&mut self.decoder)? {
                if should_cancel() {
                    return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
                }
                frames_decoded += 1;
                let frame_pts = decoded.pts.unwrap_or(i64::MIN);
                if frame_pts != i64::MIN {
                    self.last_pts = Some(frame_pts);
                    if frame_pts <= target_pts {
                        best_before = Some((frame_pts, decoded.frame.clone()));
                        if frame_pts >= target_pts.saturating_sub(self.hit_tolerance_pts) {
                            if should_cancel() {
                                return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
                            }
                            let rgba = materialize_decoded_to_rgba(
                                &decoded.frame,
                                &mut self.hardware_decode_plan,
                                &mut self.scaler,
                                &mut self.scaler_source_format,
                                self.target_width,
                                self.target_height,
                                self.path.as_path(),
                            )?;
                            preview_cache_put_with_fingerprint(
                                &self.path,
                                self.fingerprint,
                                self.target_width,
                                self.target_height,
                                frame_pts,
                                rgba.clone(),
                            );
                            return Ok(PreviewDecodeForwardResult::frame(
                                rgba,
                                frame_pts,
                                frames_decoded,
                            ));
                        }
                    } else {
                        best_after = Some((frame_pts, decoded.frame.clone()));
                        if let Some((selected_pts, rgba)) = choose_and_convert(
                            &mut self.hardware_decode_plan,
                            &mut self.scaler,
                            &mut self.scaler_source_format,
                            self.target_width,
                            self.target_height,
                            self.path.as_path(),
                            best_before.as_ref(),
                            best_after.as_ref(),
                        )? {
                            preview_cache_put_with_fingerprint(
                                &self.path,
                                self.fingerprint,
                                self.target_width,
                                self.target_height,
                                selected_pts,
                                rgba.clone(),
                            );
                            return Ok(PreviewDecodeForwardResult::frame(
                                rgba,
                                selected_pts,
                                frames_decoded,
                            ));
                        }
                    }
                }

                if policy.forward_decode_budget_exhausted(frames_decoded) {
                    break;
                }
            }

            if policy.forward_decode_budget_exhausted(frames_decoded) {
                break;
            }
        }

        if !self.reached_eof && !policy.forward_decode_budget_exhausted(frames_decoded) {
            if should_cancel() {
                return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
            }
            self.decoder.send_eof().map_err(|e| MondrianError::DecodeFailed {
                asset_id: self.path.display().to_string(),
                reason: e.to_string(),
            })?;

            while let Some(decoded) = receive_decoded_video_frame(&mut self.decoder)? {
                if should_cancel() {
                    return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
                }
                frames_decoded += 1;
                let frame_pts = decoded.pts.unwrap_or(i64::MIN);
                if frame_pts != i64::MIN {
                    self.last_pts = Some(frame_pts);
                    if frame_pts <= target_pts {
                        best_before = Some((frame_pts, decoded.frame.clone()));
                    } else {
                        best_after = Some((frame_pts, decoded.frame.clone()));
                        if let Some((selected_pts, rgba)) = choose_and_convert(
                            &mut self.hardware_decode_plan,
                            &mut self.scaler,
                            &mut self.scaler_source_format,
                            self.target_width,
                            self.target_height,
                            self.path.as_path(),
                            best_before.as_ref(),
                            best_after.as_ref(),
                        )? {
                            preview_cache_put_with_fingerprint(
                                &self.path,
                                self.fingerprint,
                                self.target_width,
                                self.target_height,
                                selected_pts,
                                rgba.clone(),
                            );
                            self.reached_eof = true;
                            return Ok(PreviewDecodeForwardResult::frame(
                                rgba,
                                selected_pts,
                                frames_decoded,
                            ));
                        }
                    }
                }

                if policy.forward_decode_budget_exhausted(frames_decoded) {
                    break;
                }
            }

            self.reached_eof = true;
        }

        if let Some((selected_pts, rgba)) = choose_and_convert(
            &mut self.hardware_decode_plan,
            &mut self.scaler,
            &mut self.scaler_source_format,
            self.target_width,
            self.target_height,
            self.path.as_path(),
            best_before.as_ref(),
            best_after.as_ref(),
        )? {
            preview_cache_put_with_fingerprint(
                &self.path,
                self.fingerprint,
                self.target_width,
                self.target_height,
                selected_pts,
                rgba.clone(),
            );
            return Ok(PreviewDecodeForwardResult::frame(
                rgba,
                selected_pts,
                frames_decoded,
            ));
        }

        if policy.forward_decode_budget_exhausted(frames_decoded) {
            return Err(MondrianError::DecodeBudgetExhausted {
                asset_id: self.path.display().to_string(),
                access_mode: policy.access_mode.as_str().to_owned(),
                decoded_frames: frames_decoded as u64,
                budget_frames: policy.forward_decode_budget_frames as u64,
                target_pts,
            });
        }

        Ok(PreviewDecodeForwardResult::empty(frames_decoded))
    }
}

fn decode_preview_frame_outcome(
    path: &Path,
    timestamp_secs: f64,
    max_width: Option<u32>,
    max_height: Option<u32>,
    access_mode: PreviewDecodeAccessMode,
    fingerprint: Option<PreviewFileFingerprint>,
    adaptive_hints: PreviewDecodeAdaptiveHints,
    hardware_decode_request: PreviewHardwareDecodeRequest,
    should_cancel: impl Fn() -> bool,
) -> Result<PreviewDecodeOutcome> {
    let started_at = Instant::now();
    if should_cancel() {
        return Ok(PreviewDecodeOutcome::Canceled);
    }
    ensure_ffmpeg_initialized(path)?;
    let fingerprint = fingerprint.unwrap_or_else(|| PreviewFileFingerprint::capture(path));
    PREVIEW_DECODE_SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let slot = sessions.slot_mut(access_mode);
        let backend = preview_decode_backend();
        let mut session_open_us = 0;

        if should_cancel() {
            return Ok(PreviewDecodeOutcome::Canceled);
        }
        let current_match = slot
            .as_ref()
            .map(|session| {
                session.matches(
                    path,
                    fingerprint,
                    max_width,
                    max_height,
                    backend,
                    hardware_decode_request,
                )
            })
            .unwrap_or(false);

        if !current_match {
            let open_started_at = Instant::now();
            *slot = Some(PreviewDecodeSession::open(
                path,
                fingerprint,
                max_width,
                max_height,
                access_mode,
                backend,
                hardware_decode_request,
            )?);
            session_open_us = duration_us(open_started_at.elapsed());
        }

        let session = slot.as_mut().expect("preview decode session must exist");
        if hardware_decode_request == PreviewHardwareDecodeRequest::RequireGpuResident
            && session.hardware_decode_plan.decision != PreviewHardwareDecodeDecision::GpuResidentNative
        {
            return Err(MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: format!(
                    "required GPU-resident preview decode is unavailable: {:?}",
                    session.hardware_decode_plan.decision
                ),
            });
        }
        let mut external_process_us = 0;
        let external_hardware_decode_plan = PreviewHardwareDecodePlan::resolve(
            hardware_decode_request,
            access_mode,
            PreviewDecodeBackend::ExternalFfmpegCpuRgba,
            session.codec_id,
        );

        if preview_external_ffmpeg_cpu_rgba_enabled(access_mode) {
            if should_cancel() {
                return Ok(PreviewDecodeOutcome::Canceled);
            }
            let external_started_at = Instant::now();
            if let Some(result) = try_decode_with_external_ffmpeg_cpu_rgba(
                path,
                timestamp_secs,
                session.target_width,
                session.target_height,
            ) {
                external_process_us = duration_us(external_started_at.elapsed());
                match result {
                    Ok(frame) => {
                        if should_cancel() {
                            if !access_mode.preserves_session_on_cancel() {
                                *slot = None;
                            }
                            return Ok(PreviewDecodeOutcome::Canceled);
                        }
                        return Ok(PreviewDecodeOutcome::Frame(frame
                            .with_access_mode(access_mode)
                            .with_seek_strategy(
                                PreviewDecodeAccessPolicy::for_access_mode(access_mode)
                                    .seek_strategy,
                            )
                            .with_session_reused(current_match)
                            .with_stage_durations(PreviewDecodeStageDurations {
                                session_open_us,
                                external_process_us,
                                ..PreviewDecodeStageDurations::default()
                            })
                            .with_hardware_decode_plan(&external_hardware_decode_plan)
                            .with_elapsed(started_at.elapsed())));
                    }
                    Err(err) => {
                        preview_trace(format!(
                            "[preview] external ffmpeg CPU RGBA decode failed, fallback software: {err}"
                        ));
                    }
                }
            }
        }

        let outcome =
            session.decode_at(timestamp_secs, access_mode, adaptive_hints, &should_cancel)?;
        match outcome {
            PreviewDecodeOutcome::Frame(frame) => Ok(PreviewDecodeOutcome::Frame(
                frame
                    .with_access_mode(access_mode)
                    .with_seek_strategy(PreviewDecodeAccessPolicy::for_access_mode(access_mode).seek_strategy)
                    .with_session_reused(current_match)
                    .with_stage_durations(PreviewDecodeStageDurations {
                        session_open_us,
                        external_process_us,
                        ..PreviewDecodeStageDurations::default()
                    })
                    .with_elapsed(started_at.elapsed()),
            )),
            PreviewDecodeOutcome::NativeGpuFrame(mut frame) => {
                frame.diagnostics = frame
                    .diagnostics
                    .with_access_mode(access_mode)
                    .with_access_policy(PreviewDecodeAccessPolicy::for_access_mode(access_mode))
                    .with_elapsed(started_at.elapsed());
                frame.diagnostics.session_reused = current_match;
                frame.diagnostics.stage_durations.accumulate(PreviewDecodeStageDurations {
                    session_open_us,
                    external_process_us,
                    ..PreviewDecodeStageDurations::default()
                });
                Ok(PreviewDecodeOutcome::NativeGpuFrame(frame))
            }
            PreviewDecodeOutcome::Canceled => {
                if !access_mode.preserves_session_on_cancel() {
                    *slot = None;
                }
                Ok(PreviewDecodeOutcome::Canceled)
            }
        }
    })
}

fn duration_us(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn estimate_frame_duration_pts(stream_tb: ffmpeg::Rational, stream_rate: ffmpeg::Rational) -> i64 {
    let tb_num = stream_tb.numerator() as f64;
    let tb_den = stream_tb.denominator() as f64;
    let rate_num = stream_rate.numerator() as f64;
    let rate_den = stream_rate.denominator() as f64;

    if tb_num <= 0.0 || tb_den <= 0.0 || rate_num <= 0.0 || rate_den <= 0.0 {
        return 1;
    }

    let fps = rate_num / rate_den;
    if fps <= 0.0 {
        return 1;
    }

    let tb_secs = tb_num / tb_den;
    ((1.0 / fps) / tb_secs).round().max(1.0) as i64
}

fn seconds_to_stream_pts(seconds: f64, stream_tb: ffmpeg::Rational) -> i64 {
    let tb_num = stream_tb.numerator() as f64;
    let tb_den = stream_tb.denominator() as f64;
    if tb_num <= 0.0 || tb_den <= 0.0 {
        return 1;
    }

    let tb_secs = tb_num / tb_den;
    if tb_secs <= 0.0 {
        return 1;
    }

    (seconds.max(0.0) / tb_secs).round().max(1.0) as i64
}

fn preview_trace(message: String) {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    let enabled = *ENABLED.get_or_init(|| {
        std::env::var("MONDRIAN_PREVIEW_TRACE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    });

    if enabled {
        eprintln!("{message}");
    }
}

fn preview_decode_threading_config() -> PreviewDecodeThreadingConfig {
    let kind = std::env::var("MONDRIAN_PREVIEW_DECODE_THREADING")
        .ok()
        .and_then(|value| PreviewDecodeThreadingKind::from_env(&value))
        .unwrap_or_default();
    let budget = preview_decode_cpu_budget();
    let count = std::env::var("MONDRIAN_PREVIEW_DECODE_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.clamp(1, budget.max_decoder_threads_per_worker))
        .unwrap_or(budget.decoder_threads_per_worker);
    PreviewDecodeThreadingConfig { kind, count }
}

#[derive(Clone)]
struct PreviewCacheHit {
    frame: RgbaFrame,
    pts: i64,
}

#[derive(Clone)]
struct PreviewFrameCacheEntry {
    path: PathBuf,
    fingerprint: PreviewFileFingerprint,
    width: u32,
    height: u32,
    pts: i64,
    frame: RgbaFrame,
}

fn preview_cache_get(
    path: &Path,
    fingerprint: PreviewFileFingerprint,
    width: u32,
    height: u32,
    target_pts: i64,
    tolerance_pts: i64,
) -> Option<PreviewCacheHit> {
    let cache = preview_frame_cache();
    let mut guard = cache.lock().ok()?;

    let mut best_index: Option<usize> = None;
    let mut best_distance = i64::MAX;

    for (index, entry) in guard.iter().enumerate() {
        if entry.path != path
            || entry.fingerprint != fingerprint
            || entry.width != width
            || entry.height != height
        {
            continue;
        }
        let distance = (entry.pts - target_pts).abs();
        if distance < best_distance {
            best_distance = distance;
            best_index = Some(index);
        }
    }

    let index = best_index?;
    if best_distance > tolerance_pts.max(1) {
        return None;
    }

    let entry = guard.remove(index)?;
    let hit = PreviewCacheHit { frame: entry.frame.clone(), pts: entry.pts };
    guard.push_front(entry);
    Some(hit)
}

fn preview_cache_put_with_fingerprint(
    path: &Path,
    fingerprint: PreviewFileFingerprint,
    width: u32,
    height: u32,
    pts: i64,
    frame: RgbaFrame,
) {
    let cache = preview_frame_cache();
    let mut guard = match cache.lock() {
        Ok(g) => g,
        Err(_) => return,
    };

    if let Some(index) = guard.iter().position(|entry| {
        entry.path == path
            && entry.fingerprint == fingerprint
            && entry.width == width
            && entry.height == height
            && entry.pts == pts
    }) {
        guard.remove(index);
    }

    guard.push_front(PreviewFrameCacheEntry {
        path: path.to_path_buf(),
        fingerprint,
        width,
        height,
        pts,
        frame,
    });

    while guard.len() > PREVIEW_FRAME_CACHE_CAPACITY {
        guard.pop_back();
    }
}

fn preview_frame_cache() -> &'static std::sync::Mutex<VecDeque<PreviewFrameCacheEntry>> {
    static CACHE: OnceLock<std::sync::Mutex<VecDeque<PreviewFrameCacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(VecDeque::new()))
}

/// 清除进程全局的预览帧缓存。
/// 在大幅 seek 后调用，避免旧帧被误用。
pub fn clear_global_preview_frame_cache() {
    if let Ok(mut guard) = preview_frame_cache().lock() {
        guard.clear();
    }
}

fn preview_external_ffmpeg_cpu_rgba_enabled(access_mode: PreviewDecodeAccessMode) -> bool {
    if !preview_external_ffmpeg_cpu_rgba_allowed_for_access_mode(access_mode) {
        return false;
    }
    match preview_decode_backend() {
        PreviewDecodeBackend::ExternalFfmpegCpuRgba => true,
        PreviewDecodeBackend::Software => false,
        PreviewDecodeBackend::Auto => {
            static ENABLED: OnceLock<bool> = OnceLock::new();
            *ENABLED.get_or_init(|| {
                std::env::var("MONDRIAN_PREVIEW_EXTERNAL_FFMPEG_CPU_RGBA")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false)
            })
        }
    }
}

fn preview_external_ffmpeg_cpu_rgba_allowed_for_access_mode(
    access_mode: PreviewDecodeAccessMode,
) -> bool {
    access_mode == PreviewDecodeAccessMode::RandomAccessStillFrame
}

fn ensure_ffmpeg_initialized(path: &Path) -> Result<()> {
    crate::ffmpeg_runtime::ensure_ffmpeg_initialized(path)
}

fn try_decode_with_external_ffmpeg_cpu_rgba(
    path: &Path,
    timestamp_secs: f64,
    width: u32,
    height: u32,
) -> Option<Result<RgbaFrame>> {
    if width == 0 || height == 0 {
        return None;
    }

    let hwaccel = if cfg!(target_os = "windows") {
        "d3d11va"
    } else if cfg!(target_os = "macos") {
        "videotoolbox"
    } else {
        "auto"
    };

    let output = Command::new("ffmpeg")
        .arg("-v")
        .arg("error")
        .arg("-hwaccel")
        .arg(hwaccel)
        .arg("-ss")
        .arg(format!("{:.6}", timestamp_secs.max(0.0)))
        .arg("-i")
        .arg(path)
        .arg("-frames:v")
        .arg("1")
        .arg("-vf")
        .arg(format!("scale={}:{}:flags=fast_bilinear", width, height))
        .arg("-pix_fmt")
        .arg("rgba")
        .arg("-f")
        .arg("rawvideo")
        .arg("pipe:1")
        .output()
        .ok()?;

    if !output.status.success() {
        return Some(Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "external ffmpeg CPU RGBA decode failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        }));
    }

    let expected = width as usize * height as usize * 4;
    if output.stdout.len() < expected {
        return Some(Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "external ffmpeg CPU RGBA decode returned insufficient bytes: got {}, expect {}",
                output.stdout.len(),
                expected
            ),
        }));
    }

    Some(Ok(RgbaFrame::new(
        width,
        height,
        output.stdout.into_iter().take(expected).collect(),
        PreviewDecodePath::ExternalFfmpegCpuRgba,
    )))
}

fn fit_target_size(
    src_width: u32,
    src_height: u32,
    max_width: Option<u32>,
    max_height: Option<u32>,
) -> (u32, u32) {
    let Some(max_w) = max_width.filter(|v| *v > 0) else {
        return (src_width, src_height);
    };
    let Some(max_h) = max_height.filter(|v| *v > 0) else {
        return (src_width, src_height);
    };

    let src_w = src_width as f64;
    let src_h = src_height as f64;
    let scale = (max_w as f64 / src_w).min(max_h as f64 / src_h).min(1.0);

    let mut out_w = (src_w * scale).round().max(1.0) as u32;
    let mut out_h = (src_h * scale).round().max(1.0) as u32;

    if out_w % 2 == 1 {
        out_w = out_w.saturating_sub(1).max(1);
    }
    if out_h % 2 == 1 {
        out_h = out_h.saturating_sub(1).max(1);
    }

    (out_w, out_h)
}

fn decoded_surface_format_from_pixel(
    pixel: ffmpeg::util::format::pixel::Pixel,
) -> DecodedVideoSurfaceFormat {
    match pixel {
        ffmpeg::util::format::pixel::Pixel::NV12 => DecodedVideoSurfaceFormat::Nv12,
        ffmpeg::util::format::pixel::Pixel::P010LE => DecodedVideoSurfaceFormat::P010,
        ffmpeg::util::format::pixel::Pixel::YUV420P => DecodedVideoSurfaceFormat::Yuv420p,
        ffmpeg::util::format::pixel::Pixel::YUV420P10LE => DecodedVideoSurfaceFormat::Yuv420p10le,
        ffmpeg::util::format::pixel::Pixel::RGBA => DecodedVideoSurfaceFormat::Rgba8,
        ffmpeg::util::format::pixel::Pixel::BGRA => DecodedVideoSurfaceFormat::Bgra8,
        _ => DecodedVideoSurfaceFormat::Other,
    }
}

fn timestamp_to_stream_pts(timestamp_secs: f64, stream_tb: ffmpeg::Rational) -> i64 {
    if stream_tb.denominator() == 0 {
        return 0;
    }

    let timestamp_us = (timestamp_secs.max(0.0) * ffmpeg::ffi::AV_TIME_BASE as f64).round() as i64;
    unsafe {
        ffmpeg::ffi::av_rescale_q(
            timestamp_us,
            ffmpeg::ffi::AVRational { num: 1, den: ffmpeg::ffi::AV_TIME_BASE },
            ffmpeg::ffi::AVRational {
                num: stream_tb.numerator(),
                den: stream_tb.denominator(),
            },
        )
    }
}

struct DecodedVideoFrame {
    frame: ffmpeg::util::frame::video::Video,
    pts: Option<i64>,
}

fn receive_decoded_video_frame(
    decoder: &mut ffmpeg::decoder::Video,
) -> Result<Option<DecodedVideoFrame>> {
    let mut decoded = ffmpeg::util::frame::video::Video::empty();
    if decoder.receive_frame(&mut decoded).is_ok() {
        let mut pts = decoded.pts();
        if pts.is_none() {
            let best_effort = unsafe { (*decoded.as_ptr()).best_effort_timestamp };
            if best_effort != ffmpeg::ffi::AV_NOPTS_VALUE {
                pts = Some(best_effort);
            }
        }

        return Ok(Some(DecodedVideoFrame { frame: decoded, pts }));
    }

    Ok(None)
}

fn convert_decoded_to_rgba(
    decoded: &ffmpeg::util::frame::video::Video,
    scaler: &mut ffmpeg::software::scaling::Context,
    path: &Path,
) -> Result<RgbaFrame> {
    let mut rgba = ffmpeg::util::frame::video::Video::empty();
    let swscale_started_at = Instant::now();
    scaler.run(decoded, &mut rgba).map_err(|e| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: e.to_string(),
    })?;
    let swscale_us = duration_us(swscale_started_at.elapsed());

    let width = rgba.width();
    let height = rgba.height();
    let stride = rgba.stride(0);
    let row_bytes = width as usize * 4;
    let src = rgba.data(0);
    let copy_started_at = Instant::now();
    let out = if stride == row_bytes {
        src[..row_bytes * height as usize].to_vec()
    } else {
        let mut out = vec![0u8; row_bytes * height as usize];
        for y in 0..height as usize {
            let src_start = y * stride;
            let src_end = src_start + row_bytes;
            let dst_start = y * row_bytes;
            let dst_end = dst_start + row_bytes;
            out[dst_start..dst_end].copy_from_slice(&src[src_start..src_end]);
        }
        out
    };
    let rgba_copy_us = duration_us(copy_started_at.elapsed());

    Ok(RgbaFrame::new(
        width,
        height,
        out,
        PreviewDecodePath::InProcessFfmpegCpuRgba,
    )
    .with_stage_durations(PreviewDecodeStageDurations {
        swscale_us,
        rgba_copy_us,
        ..PreviewDecodeStageDurations::default()
    }))
}

fn materialize_decoded_to_rgba(
    decoded: &ffmpeg::util::frame::video::Video,
    hardware_decode_plan: &mut PreviewHardwareDecodePlan,
    scaler: &mut Option<ffmpeg::software::scaling::Context>,
    scaler_source_format: &mut Option<ffmpeg::util::format::pixel::Pixel>,
    target_width: u32,
    target_height: u32,
    path: &Path,
) -> Result<RgbaFrame> {
    if !preview_hardware_frame_format(decoded.format()) {
        let scaler = ensure_preview_rgba_scaler(
            scaler,
            scaler_source_format,
            decoded.format(),
            decoded.width(),
            decoded.height(),
            target_width,
            target_height,
            path,
        )?;
        return convert_decoded_to_rgba(decoded, scaler, path);
    }

    let mut transferred = ffmpeg::util::frame::video::Video::empty();
    let transfer_started_at = Instant::now();
    let ret = unsafe {
        ffmpeg::ffi::av_hwframe_transfer_data(transferred.as_mut_ptr(), decoded.as_ptr(), 0)
    };
    let hardware_transfer_us = duration_us(transfer_started_at.elapsed());
    if ret < 0 {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "hardware frame transfer to CPU failed: {}",
                ffmpeg::Error::from(ret)
            ),
        });
    }
    unsafe {
        ffmpeg::ffi::av_frame_copy_props(transferred.as_mut_ptr(), decoded.as_ptr());
    }
    hardware_decode_plan.mark_hardware_cpu_transfer_observed();
    let scaler = ensure_preview_rgba_scaler(
        scaler,
        scaler_source_format,
        transferred.format(),
        transferred.width(),
        transferred.height(),
        target_width,
        target_height,
        path,
    )?;
    Ok(
        convert_decoded_to_rgba(&transferred, scaler, path)?.with_stage_durations(
            PreviewDecodeStageDurations {
                hardware_transfer_us,
                ..PreviewDecodeStageDurations::default()
            },
        ),
    )
}

fn ensure_preview_rgba_scaler<'a>(
    scaler: &'a mut Option<ffmpeg::software::scaling::Context>,
    scaler_source_format: &mut Option<ffmpeg::util::format::pixel::Pixel>,
    source_format: ffmpeg::util::format::pixel::Pixel,
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
    path: &Path,
) -> Result<&'a mut ffmpeg::software::scaling::Context> {
    if scaler.is_none() || *scaler_source_format != Some(source_format) {
        *scaler = Some(preview_create_rgba_scaler(
            source_format,
            source_width,
            source_height,
            target_width,
            target_height,
            path,
        )?);
        *scaler_source_format = Some(source_format);
    }
    scaler.as_mut().ok_or_else(|| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: "preview RGBA scaler was not initialized".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        clear_global_preview_frame_cache, clear_thread_local_preview_decode_session,
        decode_preview_frame_cancellable, decoded_surface_format_from_pixel, duration_us,
        preview_cache_get, preview_cache_put_with_fingerprint,
        preview_external_ffmpeg_cpu_rgba_allowed_for_access_mode, preview_seek_index_cache_get,
        preview_seek_index_cache_put, PreviewDecodeAccessMode, PreviewDecodeAccessPolicy,
        PreviewDecodeAdaptiveHints, PreviewDecodeBackend, PreviewDecodeDiagnostics,
        PreviewDecodeOutcome, PreviewDecodePath, PreviewDecodeRequest, PreviewDecodeSeekStrategy,
        PreviewDecodeStageDurations, PreviewDecodeThreadingKind, PreviewFileFingerprint,
        PreviewHardwareDecodeBlocker, PreviewHardwareDecodeCpuTransferStatus,
        PreviewHardwareDecodeDecision, PreviewHardwareDecodePlan, PreviewHardwareDecodeRequest,
        PreviewNativeDecodedFrame, PreviewPlaybackRing, PreviewScrubAdaptiveClass,
        PreviewSeekIndex, PreviewSeekIndexDiagnostics, PreviewSeekIndexSource,
        PreviewSeekResolution, RgbaFrame, PREVIEW_EXACT_FORWARD_DECODE_BUDGET_FRAMES,
        PREVIEW_PLAYBACK_FORWARD_REUSE_FRAMES, PREVIEW_SCRUB_ANY_SEEK_WINDOW_MS,
        PREVIEW_SCRUB_FORWARD_DECODE_BUDGET_FRAMES, PREVIEW_SCRUB_FORWARD_REUSE_FRAMES,
        PREVIEW_SCRUB_HOT_ANY_SEEK_WINDOW_MS, PREVIEW_SCRUB_HOT_FORWARD_DECODE_BUDGET_FRAMES,
        PREVIEW_SCRUB_MIN_FORWARD_DECODE_BUDGET_FRAMES, PREVIEW_SCRUB_SLOW_ANY_SEEK_WINDOW_MS,
        PREVIEW_SCRUB_SLOW_FORWARD_DECODE_BUDGET_FRAMES,
        PREVIEW_SCRUB_UNINDEXED_FORWARD_DECODE_BUDGET_FRAMES,
    };
    use crate::decoder::{
        DecodedFrameResidency, DecodedGpuFrameHandleKind, DecodedVideoSurfaceFormat, HwAccelBackend,
    };
    use ffmpeg_next as ffmpeg;
    use serde::Serialize;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    #[test]
    fn preview_decode_backend_codes_are_explicit_and_cpu_resident() {
        assert_eq!(PreviewDecodeBackend::from_u8(0), PreviewDecodeBackend::Auto);
        assert_eq!(
            PreviewDecodeBackend::from_u8(1),
            PreviewDecodeBackend::Software
        );
        assert_eq!(
            PreviewDecodeBackend::from_u8(2),
            PreviewDecodeBackend::ExternalFfmpegCpuRgba
        );
        assert_eq!(PreviewDecodeBackend::Auto.as_u8(), 0);
        assert_eq!(PreviewDecodeBackend::Software.as_u8(), 1);
        assert_eq!(PreviewDecodeBackend::ExternalFfmpegCpuRgba.as_u8(), 2);
        assert_eq!(
            PreviewDecodeBackend::from_u8(255),
            PreviewDecodeBackend::Auto
        );
    }

    #[test]
    fn external_ffmpeg_cpu_rgba_policy_is_still_frame_only() {
        assert!(!preview_external_ffmpeg_cpu_rgba_allowed_for_access_mode(
            PreviewDecodeAccessMode::PlaybackCursor
        ));
        assert!(!preview_external_ffmpeg_cpu_rgba_allowed_for_access_mode(
            PreviewDecodeAccessMode::ScrubCursor
        ));
        assert!(preview_external_ffmpeg_cpu_rgba_allowed_for_access_mode(
            PreviewDecodeAccessMode::RandomAccessStillFrame
        ));
    }

    #[test]
    fn preview_decode_request_defaults_to_auto_hardware_decode() {
        let request = PreviewDecodeRequest::new(
            Path::new("clip.mov"),
            0.0,
            PreviewDecodeAccessMode::PlaybackCursor,
        );

        assert_eq!(
            request.hardware_decode_request,
            PreviewHardwareDecodeRequest::Auto
        );
        assert_eq!(
            request
                .with_hardware_decode_request(PreviewHardwareDecodeRequest::PreferGpuResident)
                .hardware_decode_request,
            PreviewHardwareDecodeRequest::PreferGpuResident
        );
    }

    #[test]
    fn hardware_decode_plan_fails_closed_for_playback_prefer_gpu_until_adapter_exists() {
        let plan = PreviewHardwareDecodePlan::resolve(
            PreviewHardwareDecodeRequest::PreferGpuResident,
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewDecodeBackend::Auto,
            ffmpeg::codec::Id::H264,
        );

        assert_eq!(
            plan.request,
            PreviewHardwareDecodeRequest::PreferGpuResident
        );
        let expected_decision = if plan.probe.candidate_backend.is_none()
            || !plan.ffmpeg_codec_config.ffmpeg_device_type_available
            || (plan.ffmpeg_codec_config.ffmpeg_codec_config_available
                && !plan.ffmpeg_device_context.device_context_created)
        {
            PreviewHardwareDecodeDecision::CpuRgbaHardwareUnavailable
        } else if plan.ffmpeg_codec_config.ffmpeg_codec_config_available {
            PreviewHardwareDecodeDecision::CpuRgbaBackendUnavailable
        } else {
            PreviewHardwareDecodeDecision::CpuRgbaCodecUnsupported
        };
        assert_eq!(plan.decision, expected_decision);
        assert_eq!(plan.probe.frame_residency, DecodedFrameResidency::CpuRgba);
        assert!(!plan.probe.decoder_adapter_available);
    }

    #[test]
    fn hardware_decode_plan_rejects_non_playback_access_modes() {
        let plan = PreviewHardwareDecodePlan::resolve(
            PreviewHardwareDecodeRequest::PreferGpuResident,
            PreviewDecodeAccessMode::ScrubCursor,
            PreviewDecodeBackend::Auto,
            ffmpeg::codec::Id::H264,
        );

        assert_eq!(
            plan.decision,
            PreviewHardwareDecodeDecision::CpuRgbaAccessModeUnsupported
        );
    }

    #[test]
    fn hardware_decode_plan_marks_external_cpu_rgba_backend_boundary() {
        let plan = PreviewHardwareDecodePlan::resolve(
            PreviewHardwareDecodeRequest::PreferGpuResident,
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewDecodeBackend::ExternalFfmpegCpuRgba,
            ffmpeg::codec::Id::H264,
        );

        assert_eq!(
            plan.decision,
            PreviewHardwareDecodeDecision::CpuRgbaBackendBoundary
        );
    }

    #[test]
    fn hardware_decode_cpu_transfer_is_active_but_not_gpu_resident() {
        let mut plan = PreviewHardwareDecodePlan::resolve(
            PreviewHardwareDecodeRequest::PreferGpuResident,
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewDecodeBackend::Auto,
            ffmpeg::codec::Id::H264,
        );

        plan.mark_hardware_cpu_transfer_configured(HwAccelBackend::D3D11VA);
        assert!(plan.hardware_cpu_transfer_configured);
        assert!(!plan.hardware_cpu_transfer_observed);
        assert_eq!(
            plan.hardware_cpu_transfer_status,
            PreviewHardwareDecodeCpuTransferStatus::ConfiguredAwaitingFrame
        );
        assert!(!plan.probe.hardware_decode_active);
        assert!(!plan.probe.zero_copy_active);
        assert_eq!(plan.probe.frame_residency, DecodedFrameResidency::CpuRgba);
        assert_eq!(
            PreviewHardwareDecodeBlocker::from_probe(&plan.probe),
            PreviewHardwareDecodeBlocker::TextureResidencyNotConnected
        );

        plan.mark_hardware_cpu_transfer_observed();
        assert!(plan.hardware_cpu_transfer_observed);
        assert_eq!(
            plan.hardware_cpu_transfer_status,
            PreviewHardwareDecodeCpuTransferStatus::Observed
        );
        assert_eq!(
            plan.decision,
            PreviewHardwareDecodeDecision::HardwareDecodeCpuTransfer
        );
        assert_eq!(
            plan.probe.selected_backend,
            plan.probe.candidate_backend.unwrap_or(HwAccelBackend::None)
        );
        assert!(plan.probe.hardware_decode_active);
        assert!(!plan.probe.zero_copy_active);
    }

    #[test]
    fn hardware_decode_cpu_transfer_setup_states_are_structured() {
        let mut setup_failed = PreviewHardwareDecodePlan::resolve(
            PreviewHardwareDecodeRequest::PreferGpuResident,
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewDecodeBackend::Auto,
            ffmpeg::codec::Id::H264,
        );
        setup_failed.mark_hardware_cpu_transfer_setup_failed();
        assert_eq!(
            setup_failed.hardware_cpu_transfer_status,
            PreviewHardwareDecodeCpuTransferStatus::SetupFailed
        );
        assert!(!setup_failed.hardware_cpu_transfer_configured);
        assert!(!setup_failed.hardware_cpu_transfer_observed);

        let mut open_failed = PreviewHardwareDecodePlan::resolve(
            PreviewHardwareDecodeRequest::PreferGpuResident,
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewDecodeBackend::Auto,
            ffmpeg::codec::Id::H264,
        );
        open_failed.mark_hardware_cpu_transfer_configured(HwAccelBackend::D3D11VA);
        open_failed.mark_hardware_cpu_transfer_decoder_open_failed();
        assert_eq!(
            open_failed.hardware_cpu_transfer_status,
            PreviewHardwareDecodeCpuTransferStatus::DecoderOpenFailed
        );
        assert!(open_failed.hardware_cpu_transfer_configured);
        assert!(!open_failed.hardware_cpu_transfer_observed);
    }

    #[test]
    fn preview_decode_threading_kind_names_and_env_values_are_stable() {
        assert_eq!(PreviewDecodeThreadingKind::None.as_str(), "None");
        assert_eq!(PreviewDecodeThreadingKind::Frame.as_str(), "Frame");
        assert_eq!(PreviewDecodeThreadingKind::Slice.as_str(), "Slice");
        assert_eq!(
            PreviewDecodeThreadingKind::default(),
            PreviewDecodeThreadingKind::Frame
        );
        assert_eq!(
            PreviewDecodeThreadingKind::from_env("off"),
            Some(PreviewDecodeThreadingKind::None)
        );
        assert_eq!(
            PreviewDecodeThreadingKind::from_env("frame"),
            Some(PreviewDecodeThreadingKind::Frame)
        );
        assert_eq!(
            PreviewDecodeThreadingKind::from_env("slice"),
            Some(PreviewDecodeThreadingKind::Slice)
        );
        assert_eq!(PreviewDecodeThreadingKind::from_env("surprise"), None);
    }

    #[test]
    fn preview_decode_cpu_budget_coordinates_workers_and_decoder_threads() {
        let small = super::PreviewDecodeCpuBudget::for_available_parallelism(4);
        assert_eq!(small.preview_worker_count, 1);
        assert_eq!(small.decoder_threads_per_worker, 3);
        assert_eq!(small.reserved_interactive_threads, 1);

        let common = super::PreviewDecodeCpuBudget::for_available_parallelism(8);
        assert_eq!(common.preview_worker_count, 2);
        assert_eq!(common.decoder_threads_per_worker, 3);
        assert_eq!(common.reserved_interactive_threads, 2);

        let workstation = super::PreviewDecodeCpuBudget::for_available_parallelism(32);
        assert_eq!(workstation.preview_worker_count, 3);
        assert_eq!(workstation.decoder_threads_per_worker, 6);
        assert_eq!(workstation.max_decoder_threads_per_worker, 6);
    }

    #[test]
    fn preview_decode_access_mode_names_and_defaults_are_stable() {
        assert_eq!(
            PreviewDecodeAccessMode::PlaybackCursor.as_str(),
            "PlaybackCursor"
        );
        assert_eq!(PreviewDecodeAccessMode::ScrubCursor.as_str(), "ScrubCursor");
        assert_eq!(
            PreviewDecodeAccessMode::RandomAccessStillFrame.as_str(),
            "RandomAccessStillFrame"
        );
        assert!(PreviewDecodeAccessMode::PlaybackCursor.preserves_session_on_cancel());
        assert!(!PreviewDecodeAccessMode::ScrubCursor.preserves_session_on_cancel());
        assert!(!PreviewDecodeAccessMode::RandomAccessStillFrame.preserves_session_on_cancel());
        assert_eq!(
            PreviewDecodeSeekStrategy::KeyframeBefore.as_str(),
            "KeyframeBefore"
        );
        assert_eq!(
            PreviewDecodeSeekStrategy::BoundedAnyFrame.as_str(),
            "BoundedAnyFrame"
        );
    }

    #[test]
    fn preview_decode_rgba_request_preserves_explicit_contract_fields() {
        let path = PathBuf::from("E:/media/source.mov");
        let fingerprint = PreviewFileFingerprint {
            len: Some(10),
            modified_secs: Some(20),
            modified_nanos: Some(30),
        };
        let request =
            PreviewDecodeRequest::new(path.as_path(), 1.25, PreviewDecodeAccessMode::ScrubCursor)
                .with_max_size(Some(640), Some(360))
                .with_fingerprint(fingerprint)
                .with_adaptive_hints(PreviewDecodeAdaptiveHints {
                    scrub_class: PreviewScrubAdaptiveClass::HotRegion,
                });

        assert_eq!(request.path, path.as_path());
        assert_eq!(request.timestamp_secs, 1.25);
        assert_eq!(request.max_width, Some(640));
        assert_eq!(request.max_height, Some(360));
        assert_eq!(request.access_mode, PreviewDecodeAccessMode::ScrubCursor);
        assert_eq!(request.fingerprint, Some(fingerprint));
        assert_eq!(
            request.adaptive_hints.scrub_class,
            PreviewScrubAdaptiveClass::HotRegion
        );
    }

    #[test]
    fn preview_decode_cancel_session_policy_is_mode_specific() {
        for access_mode in [
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewDecodeAccessMode::ScrubCursor,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ] {
            let policy = PreviewDecodeAccessPolicy::for_access_mode(access_mode);
            assert_eq!(
                policy.preserve_session_on_cancel,
                access_mode.preserves_session_on_cancel()
            );
        }
    }

    #[test]
    fn preview_decode_access_mode_policies_are_distinct() {
        let playback =
            PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::PlaybackCursor);
        let scrub =
            PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::ScrubCursor);
        let still = PreviewDecodeAccessPolicy::for_access_mode(
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        );

        assert_eq!(
            playback.forward_reuse_frame_window,
            PREVIEW_PLAYBACK_FORWARD_REUSE_FRAMES
        );
        assert_eq!(
            playback.forward_decode_budget_frames,
            PREVIEW_EXACT_FORWARD_DECODE_BUDGET_FRAMES
        );
        assert!(playback.use_playback_ring);
        assert!(playback.preserve_session_on_cancel);
        assert_eq!(
            playback.seek_strategy,
            PreviewDecodeSeekStrategy::KeyframeBefore
        );
        assert_eq!(playback.any_seek_window_ms, 0);

        assert_eq!(
            scrub.forward_reuse_frame_window,
            PREVIEW_SCRUB_FORWARD_REUSE_FRAMES
        );
        assert_eq!(
            scrub.forward_decode_budget_frames,
            PREVIEW_SCRUB_FORWARD_DECODE_BUDGET_FRAMES
        );
        assert!(scrub.forward_decode_budget_frames < playback.forward_decode_budget_frames);
        assert!(!scrub.use_playback_ring);
        assert!(!scrub.preserve_session_on_cancel);
        assert_eq!(
            scrub.seek_strategy,
            PreviewDecodeSeekStrategy::BoundedAnyFrame
        );
        assert_eq!(scrub.any_seek_window_ms, PREVIEW_SCRUB_ANY_SEEK_WINDOW_MS);

        assert_eq!(still.forward_reuse_frame_window, 0);
        assert_eq!(
            still.forward_decode_budget_frames,
            PREVIEW_EXACT_FORWARD_DECODE_BUDGET_FRAMES
        );
        assert!(!still.use_playback_ring);
        assert!(!still.preserve_session_on_cancel);
        assert_eq!(
            still.seek_strategy,
            PreviewDecodeSeekStrategy::KeyframeBefore
        );
        assert_eq!(still.any_seek_window_ms, 0);
    }

    #[test]
    fn preview_decode_access_policy_budget_exhaustion_is_inclusive() {
        let scrub =
            PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::ScrubCursor);

        assert!(!scrub
            .forward_decode_budget_exhausted(scrub.forward_decode_budget_frames.saturating_sub(1)));
        assert!(scrub.forward_decode_budget_exhausted(scrub.forward_decode_budget_frames));
        assert!(scrub.forward_decode_budget_exhausted(scrub.forward_decode_budget_frames + 1));
    }

    #[test]
    fn preview_decode_access_policy_adapts_scrub_budget_from_seek_index() {
        let frame_duration_pts = 10;
        let scrub =
            PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::ScrubCursor);
        let playback =
            PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::PlaybackCursor);
        let probe_index = PreviewSeekIndex::from_probe_keyframes(vec![0, 300, 600]);

        let close_scrub = scrub.adapt_for_request(
            &probe_index,
            40,
            frame_duration_pts,
            PreviewDecodeAdaptiveHints::default(),
        );
        assert_eq!(
            close_scrub.forward_decode_budget_frames,
            PREVIEW_SCRUB_MIN_FORWARD_DECODE_BUDGET_FRAMES
        );

        let near_next_keyframe_scrub = scrub.adapt_for_request(
            &probe_index,
            290,
            frame_duration_pts,
            PreviewDecodeAdaptiveHints::default(),
        );
        assert_eq!(near_next_keyframe_scrub.forward_decode_budget_frames, 33);

        let unindexed_scrub = scrub.adapt_for_request(
            &PreviewSeekIndex::default(),
            290,
            frame_duration_pts,
            PreviewDecodeAdaptiveHints::default(),
        );
        assert_eq!(
            unindexed_scrub.forward_decode_budget_frames,
            PREVIEW_SCRUB_UNINDEXED_FORWARD_DECODE_BUDGET_FRAMES
        );

        let playback_after_adapt = playback.adapt_for_request(
            &probe_index,
            290,
            frame_duration_pts,
            PreviewDecodeAdaptiveHints::default(),
        );
        assert_eq!(
            playback_after_adapt.forward_decode_budget_frames,
            playback.forward_decode_budget_frames
        );
    }

    #[test]
    fn scrub_policy_applies_adaptive_latency_hints() {
        let frame_duration_pts = 1;
        let scrub =
            PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::ScrubCursor);
        let probe_index = PreviewSeekIndex::from_probe_keyframes(vec![0, 240]);

        let slow = scrub.adapt_for_request(
            &probe_index,
            120,
            frame_duration_pts,
            PreviewDecodeAdaptiveHints {
                scrub_class: PreviewScrubAdaptiveClass::SlowLatency,
            },
        );
        assert_eq!(
            slow.scrub_adaptive_class,
            PreviewScrubAdaptiveClass::SlowLatency
        );
        assert!(
            slow.forward_decode_budget_frames <= PREVIEW_SCRUB_SLOW_FORWARD_DECODE_BUDGET_FRAMES
        );
        assert!(slow.any_seek_window_ms <= PREVIEW_SCRUB_SLOW_ANY_SEEK_WINDOW_MS);

        let hot = scrub.adapt_for_request(
            &probe_index,
            120,
            frame_duration_pts,
            PreviewDecodeAdaptiveHints { scrub_class: PreviewScrubAdaptiveClass::HotRegion },
        );
        assert_eq!(
            hot.scrub_adaptive_class,
            PreviewScrubAdaptiveClass::HotRegion
        );
        assert!(hot.forward_decode_budget_frames <= PREVIEW_SCRUB_HOT_FORWARD_DECODE_BUDGET_FRAMES);
        assert!(hot.any_seek_window_ms <= PREVIEW_SCRUB_HOT_ANY_SEEK_WINDOW_MS);
    }

    #[test]
    fn preview_decode_access_policy_forward_reuse_is_mode_specific() {
        let playback =
            PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::PlaybackCursor);
        let scrub =
            PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::ScrubCursor);
        let still = PreviewDecodeAccessPolicy::for_access_mode(
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        );
        let frame_duration = 100;
        let last_pts = 1_000;

        assert!(playback.can_continue_forward(
            last_pts,
            last_pts + frame_duration * PREVIEW_PLAYBACK_FORWARD_REUSE_FRAMES,
            frame_duration,
            false
        ));
        assert!(!playback.can_continue_forward(
            last_pts,
            last_pts + frame_duration * (PREVIEW_PLAYBACK_FORWARD_REUSE_FRAMES + 1),
            frame_duration,
            false
        ));
        assert!(scrub.can_continue_forward(
            last_pts,
            last_pts + frame_duration * PREVIEW_SCRUB_FORWARD_REUSE_FRAMES,
            frame_duration,
            false
        ));
        assert!(!scrub.can_continue_forward(
            last_pts,
            last_pts + frame_duration * (PREVIEW_SCRUB_FORWARD_REUSE_FRAMES + 1),
            frame_duration,
            false
        ));
        assert!(!still.can_continue_forward(last_pts, last_pts, frame_duration, false));
        assert!(!playback.can_continue_forward(
            last_pts,
            last_pts - frame_duration,
            frame_duration,
            false
        ));
        assert!(!playback.can_continue_forward(
            last_pts,
            last_pts + frame_duration,
            frame_duration,
            true
        ));
    }

    #[test]
    fn preview_seek_index_records_distinct_keyframe_packets() {
        let mut index = PreviewSeekIndex::default();

        index.observe_packet(&test_packet(Some(200), None, true));
        index.observe_packet(&test_packet(Some(100), None, true));
        index.observe_packet(&test_packet(Some(200), None, true));
        index.observe_packet(&test_packet(Some(150), None, false));
        index.observe_packet(&test_packet(None, Some(50), true));

        assert_eq!(index.keyframe_at_or_before(49), None);
        assert_eq!(index.keyframe_at_or_before(50), Some(50));
        assert_eq!(index.keyframe_at_or_before(199), Some(100));
        assert_eq!(index.keyframe_at_or_before(200), Some(200));
        assert_eq!(index.keyframe_at_or_before(1_000), Some(200));
        assert_eq!(
            index.diagnostics(),
            PreviewSeekIndexDiagnostics {
                available: true,
                keyframes: 3,
                observed_packets: 5,
                source: PreviewSeekIndexSource::SessionObserved,
            }
        );
    }

    #[test]
    fn preview_seek_index_can_be_seeded_from_probe_keyframes() {
        let index = PreviewSeekIndex::from_probe_keyframes(vec![200, 100, 200]);

        assert_eq!(index.keyframe_at_or_before(99), None);
        assert_eq!(index.keyframe_at_or_before(100), Some(100));
        assert_eq!(index.keyframe_at_or_before(150), Some(100));
        assert_eq!(index.keyframe_at_or_before(200), Some(200));
        assert_eq!(
            index.diagnostics(),
            PreviewSeekIndexDiagnostics {
                available: true,
                keyframes: 2,
                observed_packets: 0,
                source: PreviewSeekIndexSource::ProbeBacked,
            }
        );
    }

    #[test]
    fn preview_seek_index_cache_is_keyed_by_path_fingerprint_and_stream() {
        let path = Path::new("cache-keyed-video.mov");
        let fingerprint = PreviewFileFingerprint {
            len: Some(10),
            modified_secs: Some(20),
            modified_nanos: Some(30),
        };

        preview_seek_index_cache_put(path, fingerprint, 1, &[300, 100, 300]);

        assert!(preview_seek_index_cache_get(path, fingerprint, 0).is_none());
        assert!(
            preview_seek_index_cache_get(Path::new("other-video.mov"), fingerprint, 1).is_none()
        );

        let index = preview_seek_index_cache_get(path, fingerprint, 1)
            .expect("probe-backed seek index should round-trip through cache");
        assert_eq!(index.keyframe_at_or_before(250), Some(100));
        assert_eq!(index.keyframe_at_or_before(400), Some(300));
        assert_eq!(
            index.diagnostics(),
            PreviewSeekIndexDiagnostics {
                available: true,
                keyframes: 2,
                observed_packets: 0,
                source: PreviewSeekIndexSource::ProbeBacked,
            }
        );
    }

    #[test]
    fn rgba_frame_diagnostics_record_seek_index_evidence() {
        let frame = RgbaFrame::new(1, 1, vec![0; 4], PreviewDecodePath::InProcessFfmpegCpuRgba)
            .with_access_mode(PreviewDecodeAccessMode::ScrubCursor)
            .with_seek_index_diagnostics(
                PreviewSeekIndexDiagnostics {
                    available: true,
                    keyframes: 4,
                    observed_packets: 12,
                    source: PreviewSeekIndexSource::ProbeBacked,
                },
                PreviewSeekResolution { used_index: true, anchor_pts: Some(240) },
            );

        assert!(frame.diagnostics.seek_index_available);
        assert_eq!(
            frame.diagnostics.access_mode,
            PreviewDecodeAccessMode::ScrubCursor
        );
        assert_eq!(
            frame.diagnostics.seek_strategy,
            PreviewDecodeSeekStrategy::BoundedAnyFrame
        );
        assert_eq!(
            frame.diagnostics.forward_reuse_frame_window,
            PREVIEW_SCRUB_FORWARD_REUSE_FRAMES
        );
        assert_eq!(
            frame.diagnostics.forward_decode_budget_frames,
            PREVIEW_SCRUB_FORWARD_DECODE_BUDGET_FRAMES as u32
        );
        assert_eq!(
            frame.diagnostics.any_seek_window_ms,
            PREVIEW_SCRUB_ANY_SEEK_WINDOW_MS
        );
        assert_eq!(frame.diagnostics.seek_index_keyframes, 4);
        assert_eq!(frame.diagnostics.seek_index_observed_packets, 12);
        assert_eq!(
            frame.diagnostics.seek_index_source,
            PreviewSeekIndexSource::ProbeBacked
        );
        assert!(frame.diagnostics.seek_index_used);
        assert_eq!(frame.diagnostics.seek_index_anchor_pts, Some(240));
    }

    #[test]
    fn rgba_frame_diagnostics_record_hw_accel_probe_fail_closed() {
        let probe = HwAccelBackend::probe();
        let frame = RgbaFrame::new(1, 1, vec![0; 4], PreviewDecodePath::InProcessFfmpegCpuRgba)
            .with_hw_accel_probe(&probe);

        assert_eq!(frame.diagnostics.hw_accel_backend, HwAccelBackend::None);
        assert!(!frame.diagnostics.hardware_decode_active);
        assert!(!frame.diagnostics.zero_copy_active);
        assert_eq!(
            frame.diagnostics.decoded_frame_residency,
            DecodedFrameResidency::CpuRgba
        );
        assert_eq!(frame.diagnostics.gpu_frame_handle_kind, None);
        assert!(!frame.diagnostics.renderer_import_ready);
        assert_eq!(
            frame.diagnostics.hardware_decode_blocker,
            PreviewHardwareDecodeBlocker::TextureResidencyNotConnected
        );
    }

    #[test]
    fn decoded_surface_format_maps_native_yuv_candidates() {
        assert_eq!(
            decoded_surface_format_from_pixel(ffmpeg::util::format::pixel::Pixel::NV12),
            DecodedVideoSurfaceFormat::Nv12
        );
        assert_eq!(
            decoded_surface_format_from_pixel(ffmpeg::util::format::pixel::Pixel::P010LE),
            DecodedVideoSurfaceFormat::P010
        );
        assert_eq!(
            decoded_surface_format_from_pixel(ffmpeg::util::format::pixel::Pixel::YUV420P10LE),
            DecodedVideoSurfaceFormat::Yuv420p10le
        );
        assert_eq!(
            decoded_surface_format_from_pixel(ffmpeg::util::format::pixel::Pixel::RGBA),
            DecodedVideoSurfaceFormat::Rgba8
        );
    }

    #[test]
    fn percentile_upper_bound_uses_sorted_nearest_rank() {
        assert_eq!(percentile_upper_bound_us([30, 10, 20].into_iter(), 95), 30);
        assert_eq!(percentile_upper_bound_us([30, 10, 20].into_iter(), 50), 20);
        assert_eq!(percentile_upper_bound_us(std::iter::empty(), 95), 0);
    }

    #[test]
    fn preview_decode_stage_durations_accumulate_saturating() {
        let mut durations = PreviewDecodeStageDurations {
            session_open_us: u64::MAX,
            cache_lookup_us: 2,
            seek_us: 3,
            packet_decode_us: 4,
            hardware_transfer_us: 5,
            swscale_us: 6,
            rgba_copy_us: 7,
            external_process_us: 8,
        };

        durations.accumulate(PreviewDecodeStageDurations {
            session_open_us: 1,
            cache_lookup_us: 20,
            seek_us: 30,
            packet_decode_us: 40,
            hardware_transfer_us: 50,
            swscale_us: 60,
            rgba_copy_us: 70,
            external_process_us: 80,
        });

        assert_eq!(durations.session_open_us, u64::MAX);
        assert_eq!(durations.cache_lookup_us, 22);
        assert_eq!(durations.seek_us, 33);
        assert_eq!(durations.packet_decode_us, 44);
        assert_eq!(durations.hardware_transfer_us, 55);
        assert_eq!(durations.swscale_us, 66);
        assert_eq!(durations.rgba_copy_us, 77);
        assert_eq!(durations.external_process_us, 88);
    }

    #[test]
    fn rgba_frame_diagnostics_record_cpu_residency_and_cache_hits() {
        let frame = RgbaFrame::new(2, 1, vec![0; 8], PreviewDecodePath::InProcessFfmpegCpuRgba)
            .with_elapsed(std::time::Duration::from_micros(42));

        assert_eq!(
            frame.diagnostics.path,
            PreviewDecodePath::InProcessFfmpegCpuRgba
        );
        assert_eq!(frame.diagnostics.elapsed_us, 42);
        assert!(!frame.diagnostics.cache_hit);
        assert!(!frame.diagnostics.external_process);
        assert!(frame.diagnostics.cpu_resident);
        assert!(!frame.diagnostics.session_reused);
        assert!(!frame.diagnostics.forward_reused);
        assert!(!frame.diagnostics.seek_index_available);
        assert_eq!(frame.diagnostics.seek_index_keyframes, 0);
        assert_eq!(frame.diagnostics.seek_index_observed_packets, 0);
        assert!(!frame.diagnostics.seek_index_used);
        assert_eq!(frame.diagnostics.seek_index_anchor_pts, None);
        assert_eq!(
            frame.diagnostics.threading_kind,
            PreviewDecodeThreadingKind::None
        );
        assert_eq!(frame.diagnostics.threading_count, 0);
        assert_eq!(
            frame.diagnostics.access_mode,
            PreviewDecodeAccessMode::RandomAccessStillFrame
        );
        assert_eq!(
            frame.diagnostics.seek_strategy,
            PreviewDecodeSeekStrategy::KeyframeBefore
        );
        assert_eq!(frame.diagnostics.forward_reuse_frame_window, 0);
        assert_eq!(frame.diagnostics.forward_decode_budget_frames, 0);
        assert_eq!(frame.diagnostics.any_seek_window_ms, 0);
        let reused = frame.clone().with_session_reused(true).with_forward_reused(true);
        assert!(reused.diagnostics.session_reused);
        assert!(reused.diagnostics.forward_reused);

        let cached = frame.into_cache_hit(
            std::time::Duration::from_micros(3),
            PreviewDecodeAccessMode::PlaybackCursor,
        );
        assert_eq!(cached.diagnostics.path, PreviewDecodePath::PreviewCacheHit);
        assert_eq!(cached.diagnostics.elapsed_us, 3);
        assert!(cached.diagnostics.cache_hit);
        assert!(cached.diagnostics.cpu_resident);
        assert_eq!(
            cached.diagnostics.access_mode,
            PreviewDecodeAccessMode::PlaybackCursor
        );
        assert_eq!(
            cached.diagnostics.seek_strategy,
            PreviewDecodeSeekStrategy::KeyframeBefore
        );
        assert_eq!(
            cached.diagnostics.forward_reuse_frame_window,
            PREVIEW_PLAYBACK_FORWARD_REUSE_FRAMES
        );
        assert_eq!(
            cached.diagnostics.forward_decode_budget_frames,
            PREVIEW_EXACT_FORWARD_DECODE_BUDGET_FRAMES as u32
        );
        assert_eq!(cached.diagnostics.any_seek_window_ms, 0);

        let ring_hit = cached.into_playback_ring_hit(std::time::Duration::from_micros(2));
        assert_eq!(
            ring_hit.diagnostics.path,
            PreviewDecodePath::PlaybackSessionRingHit
        );
        assert_eq!(ring_hit.diagnostics.elapsed_us, 2);
        assert!(ring_hit.diagnostics.cache_hit);
        assert!(ring_hit.diagnostics.cpu_resident);
        assert_eq!(
            ring_hit.diagnostics.access_mode,
            PreviewDecodeAccessMode::PlaybackCursor
        );
        assert_eq!(
            ring_hit.diagnostics.forward_reuse_frame_window,
            PREVIEW_PLAYBACK_FORWARD_REUSE_FRAMES
        );
        assert_eq!(
            ring_hit.diagnostics.forward_decode_budget_frames,
            PREVIEW_EXACT_FORWARD_DECODE_BUDGET_FRAMES as u32
        );
        assert_eq!(ring_hit.diagnostics.any_seek_window_ms, 0);
    }

    #[test]
    fn playback_session_ring_uses_strict_tolerance_and_lru_capacity() {
        let mut ring = PreviewPlaybackRing::new(2);
        let frame_a = RgbaFrame::new(
            1,
            1,
            vec![1, 2, 3, 4],
            PreviewDecodePath::InProcessFfmpegCpuRgba,
        );
        let frame_b = RgbaFrame::new(
            1,
            1,
            vec![5, 6, 7, 8],
            PreviewDecodePath::InProcessFfmpegCpuRgba,
        );
        let frame_c = RgbaFrame::new(
            1,
            1,
            vec![9, 10, 11, 12],
            PreviewDecodePath::InProcessFfmpegCpuRgba,
        );

        ring.put(100, frame_a.clone());
        ring.put(110, frame_b.clone());

        assert_eq!(
            ring.get(103, 3).expect("within tolerance").rgba(),
            frame_a.rgba()
        );
        assert!(ring.get(104, 3).is_none());

        ring.put(120, frame_c.clone());

        assert!(ring.get(110, 1).is_none());
        assert_eq!(
            ring.get(100, 1).expect("recently used").rgba(),
            frame_a.rgba()
        );
        assert_eq!(ring.get(120, 1).expect("newest").rgba(), frame_c.rgba());
    }

    #[test]
    fn rgba_frame_clone_shares_pixel_payload() {
        let frame = RgbaFrame::new(
            2,
            1,
            vec![0, 64, 128, 255, 255, 128, 64, 32],
            PreviewDecodePath::InProcessFfmpegCpuRgba,
        );
        let cloned = frame.clone();

        assert!(std::sync::Arc::ptr_eq(&frame.data, &cloned.data));
        assert_eq!(cloned.rgba(), frame.rgba());
        assert_eq!(frame.into_data(), vec![0, 64, 128, 255, 255, 128, 64, 32]);
    }

    #[test]
    fn native_decoded_frame_payload_forces_gpu_residency_diagnostics() {
        let frame = PreviewNativeDecodedFrame::new(
            1920,
            1080,
            DecodedGpuFrameHandleKind::D3D11Texture2D,
            DecodedVideoSurfaceFormat::P010,
            PreviewDecodeDiagnostics::new(PreviewDecodePath::InProcessFfmpegCpuRgba),
        );

        assert_eq!(frame.width, 1920);
        assert_eq!(frame.height, 1080);
        assert_eq!(frame.handle_kind, DecodedGpuFrameHandleKind::D3D11Texture2D);
        assert_eq!(frame.surface_format, DecodedVideoSurfaceFormat::P010);
        assert!(!frame.diagnostics.cpu_resident);
        assert_eq!(
            frame.diagnostics.decoded_frame_residency,
            DecodedFrameResidency::GpuTexture
        );
        assert_eq!(
            frame.diagnostics.gpu_frame_handle_kind,
            Some(DecodedGpuFrameHandleKind::D3D11Texture2D)
        );
        assert_eq!(
            frame.diagnostics.decoded_surface_format,
            DecodedVideoSurfaceFormat::P010
        );
    }

    #[test]
    fn cancellable_preview_decode_returns_canceled_before_opening_missing_file() {
        let path = PathBuf::from("E:/definitely-missing/canceled-preview.mov");
        let request = PreviewDecodeRequest::new(
            path.as_path(),
            0.0,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        )
        .with_max_size(Some(320), Some(180));
        let outcome = decode_preview_frame_cancellable(request, || true)
            .expect("canceled decode should not fail missing media");

        assert!(matches!(outcome, PreviewDecodeOutcome::Canceled));
    }

    #[test]
    fn preview_file_fingerprint_changes_when_file_is_replaced() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("proxy.mp4");
        std::fs::write(&path, b"old").expect("old");
        let first = PreviewFileFingerprint::capture(&path);
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&path, b"new proxy bytes").expect("new");
        let second = PreviewFileFingerprint::capture(&path);

        assert_ne!(first, second);
    }

    #[test]
    fn preview_frame_cache_is_keyed_by_file_fingerprint() {
        clear_global_preview_frame_cache();
        let path = PathBuf::from("same-proxy-path.mp4");
        let old_fingerprint = PreviewFileFingerprint {
            len: Some(3),
            modified_secs: Some(1),
            modified_nanos: Some(0),
        };
        let new_fingerprint = PreviewFileFingerprint {
            len: Some(15),
            modified_secs: Some(2),
            modified_nanos: Some(0),
        };
        let frame = RgbaFrame::new(
            2,
            1,
            vec![1, 2, 3, 4, 5, 6, 7, 8],
            PreviewDecodePath::InProcessFfmpegCpuRgba,
        );

        preview_cache_put_with_fingerprint(&path, old_fingerprint, 2, 1, 100, frame);

        assert!(preview_cache_get(&path, new_fingerprint, 2, 1, 100, 1).is_none());
        assert!(preview_cache_get(&path, old_fingerprint, 2, 1, 100, 1).is_some());
        clear_global_preview_frame_cache();
    }

    #[test]
    fn preview_frame_cache_respects_strict_pts_tolerance() {
        clear_global_preview_frame_cache();
        let path = PathBuf::from("strict-cache-window.mp4");
        let fingerprint = PreviewFileFingerprint {
            len: Some(3),
            modified_secs: Some(1),
            modified_nanos: Some(0),
        };
        let frame = RgbaFrame::new(
            2,
            1,
            vec![1, 2, 3, 4, 5, 6, 7, 8],
            PreviewDecodePath::InProcessFfmpegCpuRgba,
        );

        preview_cache_put_with_fingerprint(&path, fingerprint, 2, 1, 100, frame);

        assert!(preview_cache_get(&path, fingerprint, 2, 1, 105, 5).is_some());
        assert!(preview_cache_get(&path, fingerprint, 2, 1, 106, 5).is_none());
        clear_global_preview_frame_cache();
    }

    fn test_packet(pts: Option<i64>, dts: Option<i64>, key: bool) -> ffmpeg::Packet {
        let mut packet = ffmpeg::Packet::empty();
        packet.set_pts(pts);
        packet.set_dts(dts);
        if key {
            packet.set_flags(ffmpeg::codec::packet::Flags::KEY);
        }
        packet
    }

    #[test]
    #[ignore = "manual decode performance diagnostic; set MONDRIAN_PREVIEW_DECODE_FIXTURE"]
    fn preview_decode_fixture_perf_smoke() {
        let Some(path) = std::env::var_os("MONDRIAN_PREVIEW_DECODE_FIXTURE").map(PathBuf::from)
        else {
            eprintln!(
                "MONDRIAN_PREVIEW_DECODE_PERF_JSON={{\"skipped\":\"MONDRIAN_PREVIEW_DECODE_FIXTURE not set\"}}"
            );
            return;
        };
        let timestamp_secs = std::env::var("MONDRIAN_PREVIEW_DECODE_TIMESTAMP")
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(1.0);
        let max_width = std::env::var("MONDRIAN_PREVIEW_DECODE_MAX_WIDTH")
            .ok()
            .and_then(|value| value.parse::<u32>().ok());
        let max_height = std::env::var("MONDRIAN_PREVIEW_DECODE_MAX_HEIGHT")
            .ok()
            .and_then(|value| value.parse::<u32>().ok());

        let started = Instant::now();
        let request = PreviewDecodeRequest::new(
            path.as_path(),
            timestamp_secs,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        )
        .with_max_size(max_width, max_height);
        let frame = match decode_preview_frame_cancellable(request, || false)
            .expect("decode preview fixture")
        {
            PreviewDecodeOutcome::Frame(frame) => frame,
            PreviewDecodeOutcome::Canceled => {
                panic!("still-frame perf decode canceled")
            }
            PreviewDecodeOutcome::NativeGpuFrame(_) => {
                panic!("still-frame perf decode requires CPU RGBA output")
            }
        };
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let report = PreviewDecodePerfReport {
            path: path.display().to_string(),
            timestamp_secs,
            max_width,
            max_height,
            decoded_width: frame.width,
            decoded_height: frame.height,
            rgba_bytes: frame.data.len(),
            elapsed_ms,
            diagnostics_elapsed_us: frame.diagnostics.elapsed_us,
            path_kind: frame.diagnostics.path.as_str(),
            cache_hit: frame.diagnostics.cache_hit,
            cpu_resident: frame.diagnostics.cpu_resident,
            seek_performed: frame.diagnostics.seek_performed,
            decoded_frame_count: frame.diagnostics.decoded_frame_count,
            threading_kind: frame.diagnostics.threading_kind.as_str(),
            threading_count: frame.diagnostics.threading_count,
            stage_durations: frame.diagnostics.stage_durations,
        };
        let json = serde_json::to_string(&report).expect("serialize decode perf report");
        eprintln!("MONDRIAN_PREVIEW_DECODE_PERF_JSON={json}");
        clear_thread_local_preview_decode_session();
    }

    #[test]
    #[ignore = "manual sequential decode performance diagnostic; set MONDRIAN_PREVIEW_DECODE_FIXTURE"]
    fn preview_decode_fixture_sequence_perf_smoke() {
        let Some(path) = std::env::var_os("MONDRIAN_PREVIEW_DECODE_FIXTURE").map(PathBuf::from)
        else {
            eprintln!(
                "MONDRIAN_PREVIEW_DECODE_SEQUENCE_PERF_JSON={{\"skipped\":\"MONDRIAN_PREVIEW_DECODE_FIXTURE not set\"}}"
            );
            return;
        };
        let start_secs = std::env::var("MONDRIAN_PREVIEW_DECODE_TIMESTAMP")
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(0.0);
        let frame_rate = std::env::var("MONDRIAN_PREVIEW_DECODE_FRAME_RATE")
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(25.0)
            .max(1.0);
        let frame_count = std::env::var("MONDRIAN_PREVIEW_DECODE_SEQUENCE_FRAMES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(25)
            .clamp(1, 240);
        let max_width = std::env::var("MONDRIAN_PREVIEW_DECODE_MAX_WIDTH")
            .ok()
            .and_then(|value| value.parse::<u32>().ok());
        let max_height = std::env::var("MONDRIAN_PREVIEW_DECODE_MAX_HEIGHT")
            .ok()
            .and_then(|value| value.parse::<u32>().ok());
        let p95_budget_us = std::env::var("MONDRIAN_PREVIEW_DECODE_P95_BUDGET_US")
            .ok()
            .and_then(|value| value.parse::<u64>().ok());

        clear_thread_local_preview_decode_session();
        let access_mode = PreviewDecodeAccessMode::PlaybackCursor;
        let fingerprint = PreviewFileFingerprint::capture(&path);
        let mut frames = Vec::with_capacity(frame_count);
        let mut total_us = 0u64;
        let mut max_us = 0u64;
        let mut uncached_total_us = 0u64;
        let mut uncached_frame_count = 0usize;
        let mut uncached_max_us = 0u64;
        let mut total_stage_durations = PreviewDecodeStageDurations::default();
        let mut max_frame_stage_durations = PreviewDecodeStageDurations::default();
        let started = Instant::now();
        for index in 0..frame_count {
            let timestamp_secs = start_secs + index as f64 / frame_rate;
            let frame_started = Instant::now();
            let request = PreviewDecodeRequest::new(
                path.as_path(),
                timestamp_secs,
                PreviewDecodeAccessMode::PlaybackCursor,
            )
            .with_max_size(max_width, max_height)
            .with_fingerprint(fingerprint);
            let frame = match decode_preview_frame_cancellable(request, || false)
                .expect("decode preview fixture frame")
            {
                PreviewDecodeOutcome::Frame(frame) => frame,
                PreviewDecodeOutcome::Canceled => {
                    panic!("playback sequence perf decode canceled")
                }
                PreviewDecodeOutcome::NativeGpuFrame(_) => {
                    panic!("playback sequence perf decode requires CPU RGBA output")
                }
            };
            let elapsed_us = duration_us(frame_started.elapsed());
            total_us = total_us.saturating_add(elapsed_us);
            max_us = max_us.max(elapsed_us);
            if !frame.diagnostics.cache_hit {
                uncached_total_us = uncached_total_us.saturating_add(elapsed_us);
                uncached_frame_count = uncached_frame_count.saturating_add(1);
                uncached_max_us = uncached_max_us.max(elapsed_us);
            }
            total_stage_durations.accumulate(frame.diagnostics.stage_durations);
            max_frame_stage_durations =
                max_stage_durations(max_frame_stage_durations, frame.diagnostics.stage_durations);
            frames.push(PreviewDecodeSequenceFrameReport {
                index,
                timestamp_secs,
                elapsed_us,
                decoded_width: frame.width,
                decoded_height: frame.height,
                cache_hit: frame.diagnostics.cache_hit,
                seek_performed: frame.diagnostics.seek_performed,
                decoded_frame_count: frame.diagnostics.decoded_frame_count,
                threading_kind: frame.diagnostics.threading_kind.as_str(),
                threading_count: frame.diagnostics.threading_count,
                stage_durations: frame.diagnostics.stage_durations,
            });
        }
        let wall_us = duration_us(started.elapsed());
        let p95_us = percentile_upper_bound_us(frames.iter().map(|frame| frame.elapsed_us), 95);
        let uncached_p95_us = percentile_upper_bound_us(
            frames.iter().filter(|frame| !frame.cache_hit).map(|frame| frame.elapsed_us),
            95,
        );
        let report = PreviewDecodeSequencePerfReport {
            path: path.display().to_string(),
            access_mode: access_mode.as_str(),
            start_secs,
            frame_rate,
            frame_count,
            max_width,
            max_height,
            total_us,
            wall_us,
            avg_us: total_us / frame_count as u64,
            p95_us,
            max_us,
            uncached_frame_count,
            uncached_avg_us: average_us(uncached_total_us, uncached_frame_count),
            uncached_p95_us,
            uncached_max_us,
            p95_budget_us,
            total_stage_durations,
            max_frame_stage_durations,
            frames,
        };
        let json = serde_json::to_string(&report).expect("serialize sequence decode perf report");
        eprintln!(
            "MONDRIAN_PREVIEW_DECODE_SEQUENCE_PERF_SUMMARY path=\"{}\" access_mode={} frames={} avg_us={} p95_us={} max_us={} uncached_frames={} uncached_avg_us={} uncached_p95_us={} uncached_max_us={} p95_budget_us={:?} packet_decode_us={} hardware_transfer_us={} swscale_us={} rgba_copy_us={}",
            report.path,
            report.access_mode,
            report.frame_count,
            report.avg_us,
            report.p95_us,
            report.max_us,
            report.uncached_frame_count,
            report.uncached_avg_us,
            report.uncached_p95_us,
            report.uncached_max_us,
            report.p95_budget_us,
            report.total_stage_durations.packet_decode_us,
            report.total_stage_durations.hardware_transfer_us,
            report.total_stage_durations.swscale_us,
            report.total_stage_durations.rgba_copy_us,
        );
        eprintln!("MONDRIAN_PREVIEW_DECODE_SEQUENCE_PERF_JSON={json}");
        if let Some(p95_budget_us) = report.p95_budget_us {
            assert!(
                report.p95_us <= p95_budget_us,
                "preview decode p95 {}us exceeded budget {}us",
                report.p95_us,
                p95_budget_us
            );
        }
        clear_thread_local_preview_decode_session();
    }

    fn average_us(total_us: u64, frame_count: usize) -> u64 {
        if frame_count == 0 {
            return 0;
        }
        total_us / frame_count as u64
    }

    fn max_stage_durations(
        lhs: PreviewDecodeStageDurations,
        rhs: PreviewDecodeStageDurations,
    ) -> PreviewDecodeStageDurations {
        PreviewDecodeStageDurations {
            session_open_us: lhs.session_open_us.max(rhs.session_open_us),
            cache_lookup_us: lhs.cache_lookup_us.max(rhs.cache_lookup_us),
            seek_us: lhs.seek_us.max(rhs.seek_us),
            packet_decode_us: lhs.packet_decode_us.max(rhs.packet_decode_us),
            hardware_transfer_us: lhs.hardware_transfer_us.max(rhs.hardware_transfer_us),
            swscale_us: lhs.swscale_us.max(rhs.swscale_us),
            rgba_copy_us: lhs.rgba_copy_us.max(rhs.rgba_copy_us),
            external_process_us: lhs.external_process_us.max(rhs.external_process_us),
        }
    }

    fn percentile_upper_bound_us(samples: impl Iterator<Item = u64>, percentile: usize) -> u64 {
        let mut samples = samples.collect::<Vec<_>>();
        if samples.is_empty() {
            return 0;
        }
        samples.sort_unstable();
        let percentile = percentile.min(100);
        let rank = samples.len().saturating_mul(percentile).saturating_add(99) / 100;
        samples[rank.saturating_sub(1).min(samples.len() - 1)]
    }

    #[derive(Debug, Serialize)]
    struct PreviewDecodePerfReport {
        path: String,
        timestamp_secs: f64,
        max_width: Option<u32>,
        max_height: Option<u32>,
        decoded_width: u32,
        decoded_height: u32,
        rgba_bytes: usize,
        elapsed_ms: u64,
        diagnostics_elapsed_us: u64,
        path_kind: &'static str,
        cache_hit: bool,
        cpu_resident: bool,
        seek_performed: bool,
        decoded_frame_count: u32,
        threading_kind: &'static str,
        threading_count: u32,
        stage_durations: PreviewDecodeStageDurations,
    }

    #[derive(Debug, Serialize)]
    struct PreviewDecodeSequencePerfReport {
        path: String,
        access_mode: &'static str,
        start_secs: f64,
        frame_rate: f64,
        frame_count: usize,
        max_width: Option<u32>,
        max_height: Option<u32>,
        total_us: u64,
        wall_us: u64,
        avg_us: u64,
        p95_us: u64,
        max_us: u64,
        uncached_frame_count: usize,
        uncached_avg_us: u64,
        uncached_p95_us: u64,
        uncached_max_us: u64,
        p95_budget_us: Option<u64>,
        total_stage_durations: PreviewDecodeStageDurations,
        max_frame_stage_durations: PreviewDecodeStageDurations,
        frames: Vec<PreviewDecodeSequenceFrameReport>,
    }

    #[derive(Debug, Serialize)]
    struct PreviewDecodeSequenceFrameReport {
        index: usize,
        timestamp_secs: f64,
        elapsed_us: u64,
        decoded_width: u32,
        decoded_height: u32,
        cache_hit: bool,
        seek_performed: bool,
        decoded_frame_count: u32,
        threading_kind: &'static str,
        threading_count: u32,
        stage_durations: PreviewDecodeStageDurations,
    }
}
