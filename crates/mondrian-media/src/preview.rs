//! 预览帧解码
//!
//! 使用 avformat_seek_file（安全 Rust API）定位到目标前的关键帧，
//! flush 解码器后向前解码到目标 PTS，保证返回精确帧。

use crate::decoder::{
    decoded_video_range_from_ffmpeg, DecodedFrameResidency, DecodedGpuFrameHandleKind,
    DecodedVideoChromaLocation, DecodedVideoMatrix, DecodedVideoRange, DecodedVideoRangeContract,
    DecodedVideoSampling, DecodedVideoSurfaceFormat, HwAccelBackend, HwAccelCodecConfigProbe,
    HwAccelDeviceContext, HwAccelDeviceContextProbe, HwAccelDeviceSelector, HwAccelPixelFormat,
    HwAccelProbe,
};
use ffmpeg_next as ffmpeg;
use mondrian_core::types::ColorSpace;
use mondrian_core::{ColorMatrixCoefficients, MondrianError, Result, TimelineTime};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::{c_void, CString};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::io::{self, Read};
use std::num::NonZeroU64;
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

mod cancellation;

use cancellation::{
    preview_decode_interrupt_callback, PreviewDecodeCancelProbe, PreviewDecodeInterruptState,
};
pub use cancellation::{
    PreviewDecodeCancellation, PreviewDecodeCancellationCheckpoint,
    PreviewDecodeCancellationCheckpointEvidence, PreviewDecodeCancellationEvidence,
    PreviewDecodeCancellationSource,
};

/// Exact still/playback safety limit for forward decode from a keyframe.
///
/// This remains high enough for pathological long-GOP material, but interactive
/// scrub uses a smaller access-policy budget so latest-wins navigation cannot
/// spend seconds draining an old GOP on the CPU fallback path.
const PREVIEW_EXACT_FORWARD_DECODE_BUDGET_FRAMES: usize = 1800;
const PREVIEW_SCRUB_FORWARD_DECODE_BUDGET_FRAMES: usize = 240;
const PREVIEW_SCRUB_UNINDEXED_FORWARD_DECODE_BUDGET_FRAMES: usize = 96;
const PREVIEW_SCRUB_MIN_FORWARD_DECODE_BUDGET_FRAMES: usize = 12;
// Frame-threaded decoders may retain several reordered frames after a seek.
// Leave headroom beyond the indexed presentation-frame distance so an
// ordinary long-GOP seek is not abandoned just before its target is emitted.
const PREVIEW_SCRUB_SEEK_BUDGET_PADDING_FRAMES: usize = 16;
const PREVIEW_SCRUB_HOT_FORWARD_DECODE_BUDGET_FRAMES: usize = 72;
const PREVIEW_SCRUB_RECOVERY_FORWARD_DECODE_BUDGET_FRAMES: usize = 48;
const PREVIEW_SCRUB_HOT_ANY_SEEK_WINDOW_MS: u64 = 250;
const PREVIEW_SCRUB_RECOVERY_ANY_SEEK_WINDOW_MS: u64 = 180;
const PREVIEW_FRAME_CACHE_CAPACITY: usize = 256;
const PREVIEW_SEEK_INDEX_CACHE_CAPACITY: usize = 32;
const PREVIEW_PLAYBACK_SESSION_RING_CAPACITY: usize = 8;
// Native preview frames may outlive one codec call in the bounded global media
// cache (4), the currently presented GPU output (1), and the exact before/after
// selector (2). HEVC frame threading and reorder must retain independent
// headroom beyond those external leases; otherwise a canceled random seek can
// block inside the driver waiting for a surface and never reach its next
// cooperative checkpoint.
const PREVIEW_NATIVE_DECODE_EXTRA_HW_FRAMES: i32 = 32;
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
    /// In-process FFmpeg decoder preserved scene-linear CPU RGBA f32 samples.
    InProcessFfmpegCpuFloat,
    /// In-process FFmpeg decoder returned a retained native hardware surface.
    InProcessFfmpegNative,
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
    /// Prefer a hardware decoder even when the only available product path
    /// transfers decoded frames back to CPU RGBA.
    PreferHardwareDecode,
    /// Prefer a hardware decoder that can produce GPU-resident native frames.
    PreferGpuResident,
    /// Require a hardware-resident decode path; fail closed when unavailable.
    RequireGpuResident,
}

impl PreviewHardwareDecodeRequest {
    fn prefers_gpu_residency(self) -> bool {
        matches!(self, Self::PreferGpuResident | Self::RequireGpuResident)
    }

    fn requires_gpu_residency(self) -> bool {
        self == Self::RequireGpuResident
    }
}

fn preview_hardware_extra_frames(request: PreviewHardwareDecodeRequest) -> i32 {
    if request.prefers_gpu_residency() {
        PREVIEW_NATIVE_DECODE_EXTRA_HW_FRAMES
    } else {
        0
    }
}

/// Media-layer hardware decode selection outcome for one preview request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PreviewHardwareDecodeDecision {
    /// Hardware decode was not requested and CPU RGBA is the selected path.
    #[default]
    CpuRgbaNotRequested,
    /// Hardware decode was requested but no active GPU-resident adapter exists.
    CpuRgbaHardwareUnavailable,
    /// Hardware decode was requested but the selected backend cannot provide native residency.
    CpuRgbaBackendUnavailable,
    /// Hardware decode was requested but FFmpeg has no matching codec/backend config.
    CpuRgbaCodecUnsupported,
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
    /// Native decoded-frame residency is not blocked by the media probe.
    #[default]
    None,
    /// The active media boundary still returns CPU RGBA frames.
    TextureResidencyNotConnected,
    /// A GPU-resident decoder did not report a native handle family.
    GpuHandleMissing,
}

/// Structured reason a GPU-preferred decode returned a CPU payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreviewNativeDecodeFallback {
    /// FFmpeg returned a software frame after hardware decode was requested.
    SoftwareFrame,
    /// The decoded hardware pixel format has no native media resource adapter.
    ResourceAdapterUnavailable,
    /// The hardware frame did not expose an unambiguous native surface format.
    SurfaceFormatUnavailable,
    /// Required range, chroma-location, or bit-depth metadata was incomplete.
    SamplingMetadataIncomplete,
    /// FFmpeg could not retain or expose the native decoder resource.
    ResourceRetentionFailed,
}

impl PreviewHardwareDecodeBlocker {
    fn from_probe(probe: &HwAccelProbe) -> Self {
        if probe.hardware_decode_active
            && probe.zero_copy_active
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
    /// Exact media-source-local target; FFmpeg PTS lowering occurs inside the Adapter.
    pub source_time: TimelineTime,
    /// Optional maximum output width.
    pub max_width: Option<u32>,
    /// Optional maximum output height.
    pub max_height: Option<u32>,
    /// Access pattern that drives decoder residency and seek policy.
    pub access_mode: PreviewDecodeAccessMode,
    /// Optional stable file fingerprint already resolved by the caller.
    pub fingerprint: Option<MediaFileFingerprint>,
    /// Adaptive scheduling hints selected by the caller.
    pub adaptive_hints: PreviewDecodeAdaptiveHints,
    /// Hardware decode/native-residency preference selected by the caller.
    pub hardware_decode_request: PreviewHardwareDecodeRequest,
    /// Renderer-selected hardware decoder device for native frame residency.
    pub hardware_decode_device_selector: Option<HwAccelDeviceSelector>,
    /// Resolved source color and range contract required by CPU YUV conversion.
    pub source_color: PreviewSourceColorContract,
}

/// App-resolved source color facts required before media can convert YUV to RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PreviewSourceColorContract {
    /// Effective input/source color space after interpretation policy.
    pub color_space: ColorSpace,
    /// Authority-aware encoded quantization-range interpretation.
    pub range: DecodedVideoRangeContract,
}

impl PreviewSourceColorContract {
    /// Build a source color contract from resolved input color and range authority.
    pub const fn new(color_space: ColorSpace, range: DecodedVideoRangeContract) -> Self {
        Self { color_space, range }
    }

    /// Build an automatic contract with a stream-probe fallback.
    pub const fn automatic(color_space: ColorSpace, probed_range: DecodedVideoRange) -> Self {
        Self::new(
            color_space,
            DecodedVideoRangeContract::Automatic { probed_range },
        )
    }

    /// Build a source contract directly from persistent asset interpretation and probe facts.
    pub const fn from_interpretation(
        color_space: ColorSpace,
        interpretation: mondrian_core::timeline_data::MediaRangeInterpretation,
        probed_range: DecodedVideoRange,
    ) -> Self {
        Self::new(
            color_space,
            DecodedVideoRangeContract::from_interpretation(interpretation, probed_range),
        )
    }
}

impl<'a> PreviewDecodeRequest<'a> {
    /// Create a request for one scaled preview decode outcome.
    pub fn new(
        path: &'a Path,
        source_time: TimelineTime,
        access_mode: PreviewDecodeAccessMode,
        source_color: PreviewSourceColorContract,
    ) -> Self {
        Self {
            path,
            source_time,
            max_width: None,
            max_height: None,
            access_mode,
            fingerprint: None,
            adaptive_hints: PreviewDecodeAdaptiveHints::default(),
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            hardware_decode_device_selector: None,
            source_color,
        }
    }

    /// Set optional maximum output dimensions.
    pub fn with_max_size(mut self, max_width: Option<u32>, max_height: Option<u32>) -> Self {
        self.max_width = max_width;
        self.max_height = max_height;
        self
    }

    /// Attach a caller-resolved file fingerprint.
    pub fn with_fingerprint(mut self, fingerprint: MediaFileFingerprint) -> Self {
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

    /// Attach the renderer-selected hardware decoder device.
    pub fn with_hardware_decode_device_selector(
        mut self,
        selector: Option<HwAccelDeviceSelector>,
    ) -> Self {
        self.hardware_decode_device_selector = selector;
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
    keyframe_only: bool,
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
                keyframe_only: false,
                seek_strategy: PreviewDecodeSeekStrategy::KeyframeBefore,
                any_seek_window_ms: 0,
                scrub_adaptive_class: PreviewScrubAdaptiveClass::Normal,
            },
            PreviewDecodeAccessMode::ScrubCursor => Self {
                access_mode,
                forward_reuse_frame_window: PREVIEW_SCRUB_FORWARD_REUSE_FRAMES,
                forward_decode_budget_frames: PREVIEW_SCRUB_FORWARD_DECODE_BUDGET_FRAMES,
                use_playback_ring: false,
                preserve_session_on_cancel: true,
                keyframe_only: true,
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
                keyframe_only: false,
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
                    .min(PREVIEW_SCRUB_RECOVERY_FORWARD_DECODE_BUDGET_FRAMES);
                self.any_seek_window_ms =
                    self.any_seek_window_ms.min(PREVIEW_SCRUB_RECOVERY_ANY_SEEK_WINDOW_MS);
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

    fn accepts_first_decoded_approximation(
        self,
        selected_pts: i64,
        target_pts: i64,
        max_distance_pts: i64,
    ) -> bool {
        self.keyframe_only
            && selected_pts.saturating_sub(target_pts).abs() <= max_distance_pts.max(1)
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
            .min(source_adjusted_budget.max(PREVIEW_SCRUB_UNINDEXED_FORWARD_DECODE_BUDGET_FRAMES));
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

fn temporal_selection_is_approximate(
    requested_pts: i64,
    selected_pts: Option<i64>,
    hit_tolerance_pts: i64,
    policy: PreviewDecodeAccessPolicy,
) -> bool {
    selected_pts.is_some_and(|selected_pts| {
        let distance = selected_pts.saturating_sub(requested_pts).abs();
        if policy.keyframe_only {
            distance > 0
        } else {
            distance > hit_tolerance_pts.max(0)
        }
    })
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
            Self::InProcessFfmpegCpuFloat => "InProcessFfmpegCpuFloat",
            Self::InProcessFfmpegNative => "InProcessFfmpegNative",
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
    /// Requested stream timestamp before any interactive approximation.
    #[serde(default)]
    pub requested_pts: Option<i64>,
    /// Stream timestamp actually selected for presentation.
    #[serde(default)]
    pub selected_pts: Option<i64>,
    /// Whether interactive scrubbing intentionally presented a nearby keyframe.
    #[serde(default)]
    pub temporal_approximation: bool,
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
    /// Structured reason hardware decode / zero-copy is not active.
    #[serde(default)]
    pub hardware_decode_blocker: PreviewHardwareDecodeBlocker,
    /// Why a GPU-preferred request fell back to CPU materialization.
    #[serde(default)]
    pub native_decode_fallback: Option<PreviewNativeDecodeFallback>,
    /// Decoder output surface format before preview conversion to CPU RGBA.
    #[serde(default)]
    pub decoded_surface_format: DecodedVideoSurfaceFormat,
    /// Decoder-reported sampling facts before preview conversion to CPU RGBA.
    #[serde(default)]
    pub decoded_video_sampling: DecodedVideoSampling,
}

/// Actual decode execution that produced a reusable frame payload.
///
/// This is frame-local provenance, not a request or capability decision. It is
/// retained across playback-ring and preview-cache reuse so acceptance gates
/// can bind the frame ultimately presented for a Frame Demand to the decode
/// work that originally produced it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreviewDecodeExecutionPath {
    #[default]
    SoftwareCpu,
    HardwareCpuTransfer {
        backend: HwAccelBackend,
        surface: DecodedVideoSurfaceFormat,
        sampling: DecodedVideoSampling,
    },
    HardwareNative {
        backend: HwAccelBackend,
        handle_kind: DecodedGpuFrameHandleKind,
        surface: DecodedVideoSurfaceFormat,
        sampling: DecodedVideoSampling,
    },
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
            cpu_resident: path != PreviewDecodePath::InProcessFfmpegNative,
            seek_performed: false,
            requested_pts: None,
            selected_pts: None,
            temporal_approximation: false,
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
            hardware_decode_blocker: PreviewHardwareDecodeBlocker::TextureResidencyNotConnected,
            native_decode_fallback: None,
            decoded_surface_format: DecodedVideoSurfaceFormat::Unknown,
            decoded_video_sampling: DecodedVideoSampling::default(),
        }
    }

    /// Return only execution facts that prove actual hardware frame work.
    pub fn execution_path(&self) -> PreviewDecodeExecutionPath {
        let native_handle = self.gpu_frame_handle_kind.filter(|_| {
            self.hardware_decode_decision == PreviewHardwareDecodeDecision::GpuResidentNative
                && self.hardware_decode_active
                && self.zero_copy_active
                && self.decoded_frame_residency == DecodedFrameResidency::GpuTexture
        });
        if let Some(handle_kind) = native_handle {
            return PreviewDecodeExecutionPath::HardwareNative {
                backend: self.hw_accel_backend,
                handle_kind,
                surface: self.decoded_surface_format,
                sampling: self.decoded_video_sampling,
            };
        }
        if self.hardware_decode_cpu_transfer_observed && self.hardware_decode_active {
            return PreviewDecodeExecutionPath::HardwareCpuTransfer {
                backend: self.hw_accel_backend,
                surface: self.decoded_surface_format,
                sampling: self.decoded_video_sampling,
            };
        }
        PreviewDecodeExecutionPath::SoftwareCpu
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

    fn with_hw_accel_probe(mut self, probe: &HwAccelProbe) -> Self {
        self.hw_accel_backend = probe.selected_backend;
        self.hardware_decode_candidate_backend = probe.candidate_backend;
        self.hardware_decode_candidate_handle_kind = probe.candidate_handle_kind;
        self.hardware_decode_adapter_available = probe.decoder_adapter_available;
        self.hardware_decode_active = probe.hardware_decode_active;
        self.zero_copy_active = probe.zero_copy_active;
        self.decoded_frame_residency = probe.frame_residency;
        self.gpu_frame_handle_kind = probe.gpu_frame_handle_kind;
        self.hardware_decode_blocker = PreviewHardwareDecodeBlocker::from_probe(probe);
        self
    }

    fn with_hardware_decode_plan(mut self, plan: &PreviewHardwareDecodePlan) -> Self {
        self.hardware_decode_request = plan.request;
        self.hardware_decode_decision = plan.decision;
        self.hardware_decode_ffmpeg_device_type_available =
            plan.ffmpeg_codec_config.ffmpeg_device_type_available;
        self.hardware_decode_ffmpeg_codec_config_available =
            plan.ffmpeg_codec_config.ffmpeg_codec_config_available;
        self.hardware_decode_ffmpeg_hw_pixel_format = plan.ffmpeg_codec_config.hw_pixel_format;
        self.hardware_decode_ffmpeg_device_context_attempted =
            plan.ffmpeg_device_context.device_create_attempted;
        self.hardware_decode_ffmpeg_device_context_created =
            plan.ffmpeg_device_context.device_context_created;
        self.hardware_decode_ffmpeg_device_context_error_code =
            plan.ffmpeg_device_context.device_create_error_code;
        self.hardware_decode_cpu_transfer_configured = plan.hardware_cpu_transfer_configured;
        self.hardware_decode_cpu_transfer_observed = plan.hardware_cpu_transfer_observed;
        self.hardware_decode_cpu_transfer_status = plan.hardware_cpu_transfer_status;
        self.native_decode_fallback = plan.native_decode_fallback;
        self.with_hw_accel_probe(&plan.probe)
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
    /// Color and alpha semantics of the decoded pixels.
    pub color_contract: DecodedRgbaFrameContract,
    /// Decode/cache diagnostics for this frame.
    pub diagnostics: PreviewDecodeDiagnostics,
    /// Frame-local execution provenance retained across every cache layer.
    pub decode_execution: PreviewDecodeExecutionPath,
}

/// CPU-resident RGBA f32 preview frame that preserves scene-linear samples.
#[derive(Debug, Clone)]
pub struct FloatRgbaFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Shared CPU-resident interleaved RGBA f32 pixels.
    data: Arc<Vec<f32>>,
    /// Color and alpha semantics of the decoded pixels.
    pub color_contract: DecodedRgbaFrameContract,
    /// Decode/cache diagnostics for this frame.
    pub diagnostics: PreviewDecodeDiagnostics,
    /// Frame-local execution provenance retained across every cache layer.
    pub decode_execution: PreviewDecodeExecutionPath,
}

/// Encoding represented by a decoded CPU RGBA payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DecodedRgbaEncoding {
    /// RGB channels retain the source transfer function and primaries.
    SourceEncodedRgb,
    /// RGB channels retain scene-linear source values and primaries.
    SourceLinearRgb,
}

/// Alpha representation of a decoded CPU RGBA payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DecodedRgbaAlphaMode {
    /// Alpha is independent of the RGB channels.
    Straight,
}

/// Applied conversion contract for a decoded CPU RGBA payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DecodedRgbaFrameContract {
    /// Effective source color interpretation retained by RGB channels.
    pub source: PreviewSourceColorContract,
    /// Encoding of the RGB channels after decode.
    pub encoding: DecodedRgbaEncoding,
    /// Alpha representation after decode.
    pub alpha_mode: DecodedRgbaAlphaMode,
    /// Matrix actually applied while converting decoder pixels to RGB.
    pub applied_matrix: DecodedVideoMatrix,
    /// Quantization range actually applied while converting decoder pixels to RGB.
    pub applied_range: DecodedVideoRange,
}

impl DecodedRgbaFrameContract {
    fn source_encoded(
        source: PreviewSourceColorContract,
        applied_matrix: DecodedVideoMatrix,
        applied_range: DecodedVideoRange,
    ) -> Self {
        Self {
            source,
            encoding: DecodedRgbaEncoding::SourceEncodedRgb,
            alpha_mode: DecodedRgbaAlphaMode::Straight,
            applied_matrix,
            applied_range,
        }
    }

    fn source_linear(source: PreviewSourceColorContract) -> Self {
        Self {
            source,
            encoding: DecodedRgbaEncoding::SourceLinearRgb,
            alpha_mode: DecodedRgbaAlphaMode::Straight,
            applied_matrix: DecodedVideoMatrix::Rgb,
            applied_range: DecodedVideoRange::Full,
        }
    }
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
    /// Process-local native decoder handle token that owns the frame.
    pub handle: PreviewNativeDecodedFrameHandle,
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
        handle: PreviewNativeDecodedFrameHandle,
        surface_format: DecodedVideoSurfaceFormat,
        decoded_video_sampling: DecodedVideoSampling,
        mut diagnostics: PreviewDecodeDiagnostics,
    ) -> std::result::Result<Self, PreviewNativeDecodedFrameError> {
        if width == 0 || height == 0 {
            return Err(PreviewNativeDecodedFrameError::EmptyExtent { width, height });
        }
        if !surface_format.supports_native_gpu_payload() {
            return Err(PreviewNativeDecodedFrameError::UnsupportedSurfaceFormat {
                surface_format,
            });
        }
        validate_native_decoded_video_sampling(surface_format, decoded_video_sampling)?;
        diagnostics.cpu_resident = false;
        diagnostics.decoded_frame_residency = DecodedFrameResidency::GpuTexture;
        diagnostics.gpu_frame_handle_kind = Some(handle.kind());
        diagnostics.decoded_surface_format = surface_format;
        diagnostics.decoded_video_sampling = decoded_video_sampling;
        Ok(Self { width, height, handle, surface_format, diagnostics })
    }

    /// Native decoder handle family that owns the frame.
    pub fn handle_kind(&self) -> DecodedGpuFrameHandleKind {
        self.handle.kind()
    }
}

/// Backend-owned native decoder resource retained by a preview frame.
///
/// Implementations own the concrete decoder surface or registry lease. Dropping
/// the final handle clone must release that ownership according to the backend's
/// normal resource lifetime rules.
pub trait PreviewNativeDecodedFrameResource: fmt::Debug + Any + Send + Sync {
    /// Native decoder handle family exposed by this resource.
    fn handle_kind(&self) -> DecodedGpuFrameHandleKind;

    /// Process-local backend identity. This is never an OS handle.
    fn handle_id(&self) -> NonZeroU64;

    /// Type-erased access for the matching renderer import backend.
    fn as_any(&self) -> &dyn Any;
}

static NEXT_FFMPEG_NATIVE_FRAME_ID: AtomicU64 = AtomicU64::new(1);

/// FFmpeg-owned reference to one native hardware-decoded frame.
///
/// Construction retains the source frame with `av_frame_clone`, which in turn
/// retains its `AVBufferRef`-backed decoder surface. The final resource drop
/// releases that reference with `av_frame_free`.
pub struct FfmpegNativeDecodedFrameResource {
    frame: NonNull<ffmpeg::ffi::AVFrame>,
    pixel_format: ffmpeg::util::format::pixel::Pixel,
    kind: DecodedGpuFrameHandleKind,
    id: NonZeroU64,
}

// SAFETY: This resource has the same ownership and synchronization contract as
// ffmpeg-next's Frame, which explicitly implements Send and Sync. The AVFrame
// is immutable after retention and is released only when the final Arc drops.
unsafe impl Send for FfmpegNativeDecodedFrameResource {}
// SAFETY: See the Send implementation. Accessors expose immutable metadata and
// borrowed native handles; mutation remains owned by the decoder/backend.
unsafe impl Sync for FfmpegNativeDecodedFrameResource {}

impl FfmpegNativeDecodedFrameResource {
    /// Retain a hardware-decoded FFmpeg frame without copying its surface.
    pub fn retain(
        frame: &ffmpeg::util::frame::video::Video,
    ) -> std::result::Result<Self, FfmpegNativeDecodedFrameResourceError> {
        let pixel_format = frame.format();
        let kind = decoded_handle_kind_from_hardware_pixel(pixel_format).ok_or(
            FfmpegNativeDecodedFrameResourceError::UnsupportedPixelFormat { pixel_format },
        )?;
        let id = next_ffmpeg_native_frame_id()?;
        // SAFETY: frame.as_ptr() is valid for this borrow. av_frame_clone creates
        // an independently owned AVFrame whose buffer references are retained.
        let retained = unsafe { ffmpeg::ffi::av_frame_clone(frame.as_ptr()) };
        let frame = NonNull::new(retained)
            .ok_or(FfmpegNativeDecodedFrameResourceError::FrameReferenceAllocationFailed)?;
        Ok(Self { frame, pixel_format, kind, id })
    }

    /// Borrow the preferred FFmpeg D3D11 texture ABI view.
    pub fn d3d11_texture(
        &self,
    ) -> std::result::Result<FfmpegD3D11TextureView, FfmpegNativeDecodedFrameResourceError> {
        parse_ffmpeg_d3d11_texture(self.frame, self.pixel_format)
    }

    /// Borrow FFmpeg's D3D12 resource and decode-completion fence ABI.
    pub fn d3d12_texture(
        &self,
    ) -> std::result::Result<FfmpegD3D12TextureView, FfmpegNativeDecodedFrameResourceError> {
        parse_ffmpeg_d3d12_texture(self.frame, self.pixel_format)
    }

    /// Hardware pixel format retained by this frame.
    pub fn pixel_format(&self) -> ffmpeg::util::format::pixel::Pixel {
        self.pixel_format
    }
}

impl fmt::Debug for FfmpegNativeDecodedFrameResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FfmpegNativeDecodedFrameResource")
            .field("pixel_format", &self.pixel_format())
            .field("kind", &self.kind)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Drop for FfmpegNativeDecodedFrameResource {
    fn drop(&mut self) {
        let mut frame = self.frame.as_ptr();
        // SAFETY: retain obtained sole ownership of this AVFrame allocation from
        // av_frame_clone. Drop runs exactly once and av_frame_free accepts &mut.
        unsafe { ffmpeg::ffi::av_frame_free(&mut frame) };
    }
}

impl PreviewNativeDecodedFrameResource for FfmpegNativeDecodedFrameResource {
    fn handle_kind(&self) -> DecodedGpuFrameHandleKind {
        self.kind
    }

    fn handle_id(&self) -> NonZeroU64 {
        self.id
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Borrowed view of FFmpeg's preferred D3D11 hardware-frame ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfmpegD3D11TextureView {
    texture: NonNull<c_void>,
    array_slice: u32,
}

/// Borrowed view of FFmpeg's `AVD3D12VAFrame` resource and sync contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfmpegD3D12TextureView {
    texture: NonNull<c_void>,
    fence: NonNull<c_void>,
    fence_value: u64,
}

impl FfmpegD3D12TextureView {
    /// Borrowed `ID3D12Resource` pointer owned by the retained AVFrame.
    pub fn texture_ptr(self) -> *mut c_void {
        self.texture.as_ptr()
    }

    /// Borrowed `ID3D12Fence` pointer signaling decode completion.
    pub fn fence_ptr(self) -> *mut c_void {
        self.fence.as_ptr()
    }

    /// Fence value that must complete before the resource is read.
    pub fn fence_value(self) -> u64 {
        self.fence_value
    }
}

impl FfmpegD3D11TextureView {
    /// Borrowed `ID3D11Texture2D` pointer stored in `AVFrame::data[0]`.
    pub fn texture_ptr(self) -> *mut c_void {
        self.texture.as_ptr()
    }

    /// Array-texture slice stored as `intptr_t` in `AVFrame::data[1]`.
    pub fn array_slice(self) -> u32 {
        self.array_slice
    }
}

/// Error retaining or interpreting an FFmpeg native decoded frame.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum FfmpegNativeDecodedFrameResourceError {
    /// The frame is not backed by a supported FFmpeg hardware pixel format.
    #[error("FFmpeg pixel format {pixel_format:?} is not a supported native decode surface")]
    UnsupportedPixelFormat {
        /// Unsupported source pixel format.
        pixel_format: ffmpeg::util::format::pixel::Pixel,
    },
    /// FFmpeg could not allocate a retained AVFrame reference.
    #[error("FFmpeg could not retain the native decoded frame reference")]
    FrameReferenceAllocationFailed,
    /// Process-local diagnostic handle identifiers were exhausted.
    #[error("process-local FFmpeg native frame identifiers are exhausted")]
    HandleIdExhausted,
    /// Only AV_PIX_FMT_D3D11 uses the preferred texture-plus-slice ABI.
    #[error("FFmpeg frame format {pixel_format:?} does not use the preferred D3D11 texture ABI")]
    NotPreferredD3D11Frame {
        /// Actual retained hardware pixel format.
        pixel_format: ffmpeg::util::format::pixel::Pixel,
    },
    /// A preferred D3D11 frame did not carry an ID3D11Texture2D pointer.
    #[error("FFmpeg D3D11 frame is missing its ID3D11Texture2D pointer")]
    MissingD3D11Texture,
    /// FFmpeg reported a D3D11 array slice that cannot fit Mondrian's contract.
    #[error("FFmpeg D3D11 array slice {array_slice} exceeds u32")]
    D3D11ArraySliceOverflow {
        /// FFmpeg `intptr_t` value interpreted as an unsigned index.
        array_slice: usize,
    },
    /// Only AV_PIX_FMT_D3D12 uses the `AVD3D12VAFrame` ABI.
    #[error("FFmpeg frame format {pixel_format:?} does not use the D3D12 resource ABI")]
    NotD3D12Frame {
        /// Actual retained hardware pixel format.
        pixel_format: ffmpeg::util::format::pixel::Pixel,
    },
    /// A D3D12 hardware frame did not carry its decoded texture.
    #[error("FFmpeg D3D12 frame is missing its ID3D12Resource pointer")]
    MissingD3D12Texture,
    /// A D3D12 hardware frame did not carry its decode-completion fence.
    #[error("FFmpeg D3D12 frame is missing its ID3D12Fence pointer")]
    MissingD3D12Fence,
}

fn next_ffmpeg_native_frame_id(
) -> std::result::Result<NonZeroU64, FfmpegNativeDecodedFrameResourceError> {
    let raw = NEXT_FFMPEG_NATIVE_FRAME_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| FfmpegNativeDecodedFrameResourceError::HandleIdExhausted)?;
    NonZeroU64::new(raw).ok_or(FfmpegNativeDecodedFrameResourceError::HandleIdExhausted)
}

fn decoded_handle_kind_from_hardware_pixel(
    pixel_format: ffmpeg::util::format::pixel::Pixel,
) -> Option<DecodedGpuFrameHandleKind> {
    use ffmpeg::util::format::pixel::Pixel;

    match pixel_format {
        Pixel::D3D12 => Some(DecodedGpuFrameHandleKind::D3D12Resource),
        Pixel::D3D11 | Pixel::D3D11VA_VLD => Some(DecodedGpuFrameHandleKind::D3D11Texture2D),
        Pixel::DXVA2_VLD => Some(DecodedGpuFrameHandleKind::Dxva2Surface),
        Pixel::VIDEOTOOLBOX => Some(DecodedGpuFrameHandleKind::CVPixelBuffer),
        Pixel::VAAPI => Some(DecodedGpuFrameHandleKind::VaapiSurface),
        Pixel::VDPAU => Some(DecodedGpuFrameHandleKind::VdpauVideoSurface),
        Pixel::CUDA => Some(DecodedGpuFrameHandleKind::CudaDeviceMemory),
        _ => None,
    }
}

fn parse_ffmpeg_d3d11_texture(
    frame: NonNull<ffmpeg::ffi::AVFrame>,
    pixel_format: ffmpeg::util::format::pixel::Pixel,
) -> std::result::Result<FfmpegD3D11TextureView, FfmpegNativeDecodedFrameResourceError> {
    // SAFETY: callers retain ownership of the AVFrame for the duration of this
    // function. FFmpeg documents data[0]/data[1] for AV_PIX_FMT_D3D11.
    let frame = unsafe { frame.as_ref() };
    if pixel_format != ffmpeg::util::format::pixel::Pixel::D3D11 {
        return Err(FfmpegNativeDecodedFrameResourceError::NotPreferredD3D11Frame { pixel_format });
    }
    let texture = NonNull::new(frame.data[0].cast::<c_void>())
        .ok_or(FfmpegNativeDecodedFrameResourceError::MissingD3D11Texture)?;
    let array_slice = frame.data[1] as usize;
    let array_slice = u32::try_from(array_slice).map_err(|_| {
        FfmpegNativeDecodedFrameResourceError::D3D11ArraySliceOverflow { array_slice }
    })?;
    Ok(FfmpegD3D11TextureView { texture, array_slice })
}

#[repr(C)]
struct FfmpegAvD3D12VaSyncContext {
    fence: *mut c_void,
    event: *mut c_void,
    fence_value: u64,
}

#[repr(C)]
struct FfmpegAvD3D12VaFrame {
    texture: *mut c_void,
    sync_ctx: FfmpegAvD3D12VaSyncContext,
}

fn parse_ffmpeg_d3d12_texture(
    frame: NonNull<ffmpeg::ffi::AVFrame>,
    pixel_format: ffmpeg::util::format::pixel::Pixel,
) -> std::result::Result<FfmpegD3D12TextureView, FfmpegNativeDecodedFrameResourceError> {
    if pixel_format != ffmpeg::util::format::pixel::Pixel::D3D12 {
        return Err(FfmpegNativeDecodedFrameResourceError::NotD3D12Frame { pixel_format });
    }
    // SAFETY: the retained AVFrame owns data[0] for this borrow. FFmpeg 7.x+
    // defines AV_PIX_FMT_D3D12 data[0] as `AVD3D12VAFrame*`.
    let raw_frame = unsafe { frame.as_ref() };
    let native = NonNull::new(raw_frame.data[0].cast::<FfmpegAvD3D12VaFrame>())
        .ok_or(FfmpegNativeDecodedFrameResourceError::MissingD3D12Texture)?;
    // SAFETY: the data[0] ABI was validated above and remains AVFrame-owned.
    let native = unsafe { native.as_ref() };
    let texture = NonNull::new(native.texture)
        .ok_or(FfmpegNativeDecodedFrameResourceError::MissingD3D12Texture)?;
    let fence = NonNull::new(native.sync_ctx.fence)
        .ok_or(FfmpegNativeDecodedFrameResourceError::MissingD3D12Fence)?;
    Ok(FfmpegD3D12TextureView {
        texture,
        fence,
        fence_value: native.sync_ctx.fence_value,
    })
}

/// Shared lease for one backend-owned native decoder resource.
#[derive(Clone)]
pub struct PreviewNativeDecodedFrameHandle {
    resource: Arc<dyn PreviewNativeDecodedFrameResource>,
}

impl PreviewNativeDecodedFrameHandle {
    /// Retain a backend-owned native decoder resource.
    pub fn new<R>(resource: R) -> Self
    where
        R: PreviewNativeDecodedFrameResource,
    {
        Self { resource: Arc::new(resource) }
    }

    /// Native decoder handle family for this token.
    pub fn kind(&self) -> DecodedGpuFrameHandleKind {
        self.resource.handle_kind()
    }

    /// Process-local backend handle id.
    pub fn id(&self) -> NonZeroU64 {
        self.resource.handle_id()
    }

    /// Access the concrete resource only from its matching import backend.
    pub fn resource<R>(&self) -> Option<&R>
    where
        R: PreviewNativeDecodedFrameResource,
    {
        self.resource.as_any().downcast_ref()
    }
}

impl fmt::Debug for PreviewNativeDecodedFrameHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreviewNativeDecodedFrameHandle")
            .field("kind", &self.kind())
            .field("id", &self.id())
            .finish_non_exhaustive()
    }
}

impl PartialEq for PreviewNativeDecodedFrameHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.resource, &other.resource)
    }
}

impl Eq for PreviewNativeDecodedFrameHandle {}

impl Hash for PreviewNativeDecodedFrameHandle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let resource_identity = Arc::as_ptr(&self.resource) as *const ();
        resource_identity.hash(state);
    }
}

fn validate_native_decoded_video_sampling(
    surface_format: DecodedVideoSurfaceFormat,
    sampling: DecodedVideoSampling,
) -> std::result::Result<(), PreviewNativeDecodedFrameError> {
    if sampling.range == DecodedVideoRange::Unknown {
        return Err(PreviewNativeDecodedFrameError::MissingVideoRange { surface_format });
    }
    let expected_bit_depth = surface_format
        .fixed_bit_depth()
        .ok_or(PreviewNativeDecodedFrameError::UnsupportedSurfaceFormat { surface_format })?;
    if sampling.bit_depth != expected_bit_depth {
        return Err(PreviewNativeDecodedFrameError::BitDepthMismatch {
            surface_format,
            expected: expected_bit_depth,
            actual: sampling.bit_depth,
        });
    }
    if matches!(
        surface_format,
        DecodedVideoSurfaceFormat::Nv12 | DecodedVideoSurfaceFormat::P010
    ) && sampling.chroma_location == DecodedVideoChromaLocation::Unknown
    {
        return Err(PreviewNativeDecodedFrameError::MissingVideoChromaLocation { surface_format });
    }
    Ok(())
}

/// Error returned when constructing a native decoded preview frame payload.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum PreviewNativeDecodedFrameError {
    /// Native GPU preview frames require a non-empty extent.
    #[error("native decoded preview frame requires a non-empty extent, got {width}x{height}")]
    EmptyExtent {
        /// Frame width.
        width: u32,
        /// Frame height.
        height: u32,
    },
    /// The decoded surface format cannot be carried as a native GPU payload.
    #[error("decoded surface format {surface_format:?} cannot be carried as a native GPU payload")]
    UnsupportedSurfaceFormat {
        /// Unsupported decoded surface format.
        surface_format: DecodedVideoSurfaceFormat,
    },
    /// Native GPU preview payloads require explicit video range metadata.
    #[error(
        "native decoded preview frame {surface_format:?} requires explicit video range metadata"
    )]
    MissingVideoRange {
        /// Decoded surface format whose range metadata was missing.
        surface_format: DecodedVideoSurfaceFormat,
    },
    /// Subsampled native GPU preview payloads require explicit chroma siting.
    #[error("native decoded preview frame {surface_format:?} requires explicit chroma location metadata")]
    MissingVideoChromaLocation {
        /// Decoded surface format whose chroma metadata was missing.
        surface_format: DecodedVideoSurfaceFormat,
    },
    /// Native GPU preview payload bit depth must match its surface format.
    #[error("native decoded preview frame {surface_format:?} requires {expected}-bit sampling metadata, got {actual}")]
    BitDepthMismatch {
        /// Decoded surface format whose bit-depth metadata mismatched.
        surface_format: DecodedVideoSurfaceFormat,
        /// Required bit depth for the surface format.
        expected: u8,
        /// Reported bit depth.
        actual: u8,
    },
}

impl RgbaFrame {
    pub(crate) fn new(
        width: u32,
        height: u32,
        data: Vec<u8>,
        color_contract: DecodedRgbaFrameContract,
        path: PreviewDecodePath,
    ) -> Self {
        Self {
            width,
            height,
            data: Arc::new(data),
            color_contract,
            diagnostics: PreviewDecodeDiagnostics::new(path),
            decode_execution: PreviewDecodeExecutionPath::SoftwareCpu,
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

    fn with_temporal_selection(
        mut self,
        requested_pts: i64,
        selected_pts: Option<i64>,
        hit_tolerance_pts: i64,
        policy: PreviewDecodeAccessPolicy,
    ) -> Self {
        self.diagnostics.requested_pts = Some(requested_pts);
        self.diagnostics.selected_pts = selected_pts;
        self.diagnostics.temporal_approximation = temporal_selection_is_approximate(
            requested_pts,
            selected_pts,
            hit_tolerance_pts,
            policy,
        );
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

    fn with_hardware_decode_plan(mut self, plan: &PreviewHardwareDecodePlan) -> Self {
        self.diagnostics = self.diagnostics.with_hardware_decode_plan(plan);
        self
    }

    fn with_decoded_surface_format(mut self, format: DecodedVideoSurfaceFormat) -> Self {
        if self.diagnostics.decoded_surface_format == DecodedVideoSurfaceFormat::Unknown {
            self.diagnostics.decoded_surface_format = format;
        }
        self
    }

    fn with_decoded_video_sampling(mut self, sampling: DecodedVideoSampling) -> Self {
        self.diagnostics.decoded_video_sampling = sampling;
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

    fn with_decode_execution(mut self) -> Self {
        self.decode_execution = self.diagnostics.execution_path();
        self
    }

    fn into_cache_hit(mut self, elapsed: Duration, access_mode: PreviewDecodeAccessMode) -> Self {
        let decoded_surface_format = self.diagnostics.decoded_surface_format;
        let decoded_video_sampling = self.diagnostics.decoded_video_sampling;
        self.diagnostics = PreviewDecodeDiagnostics::cache_hit_for_mode(elapsed, access_mode);
        self.diagnostics.decoded_surface_format = decoded_surface_format;
        self.diagnostics.decoded_video_sampling = decoded_video_sampling;
        self
    }

    fn into_playback_ring_hit(mut self, elapsed: Duration) -> Self {
        let decoded_surface_format = self.diagnostics.decoded_surface_format;
        let decoded_video_sampling = self.diagnostics.decoded_video_sampling;
        self.diagnostics = PreviewDecodeDiagnostics::playback_ring_hit(elapsed);
        self.diagnostics.decoded_surface_format = decoded_surface_format;
        self.diagnostics.decoded_video_sampling = decoded_video_sampling;
        self
    }
}

impl FloatRgbaFrame {
    fn new(
        width: u32,
        height: u32,
        data: Vec<f32>,
        color_contract: DecodedRgbaFrameContract,
        path: PreviewDecodePath,
    ) -> Self {
        debug_assert_eq!(data.len(), width as usize * height as usize * 4);
        let mut diagnostics = PreviewDecodeDiagnostics::new(path);
        diagnostics.decoded_frame_residency = DecodedFrameResidency::CpuFloat;
        Self {
            width,
            height,
            data: Arc::new(data),
            color_contract,
            diagnostics,
            decode_execution: PreviewDecodeExecutionPath::SoftwareCpu,
        }
    }

    /// Borrow decoded RGBA f32 pixels.
    pub fn rgba(&self) -> &[f32] {
        self.data.as_slice()
    }

    /// Consume this frame and return shared decoded RGBA f32 pixels.
    pub fn into_shared_data(self) -> Arc<Vec<f32>> {
        self.data
    }

    /// Consume this frame and return owned decoded RGBA f32 pixels.
    pub fn into_data(self) -> Vec<f32> {
        Arc::try_unwrap(self.data).unwrap_or_else(|data| data.as_ref().clone())
    }

    fn with_elapsed(mut self, elapsed: Duration) -> Self {
        self.diagnostics = self.diagnostics.with_elapsed(elapsed);
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

    fn with_decode_work(mut self, seek_performed: bool, decoded_frame_count: usize) -> Self {
        self.diagnostics.seek_performed = seek_performed;
        self.diagnostics.decoded_frame_count = decoded_frame_count.min(u32::MAX as usize) as u32;
        self
    }

    fn with_temporal_selection(
        mut self,
        requested_pts: i64,
        selected_pts: Option<i64>,
        hit_tolerance_pts: i64,
        policy: PreviewDecodeAccessPolicy,
    ) -> Self {
        self.diagnostics.requested_pts = Some(requested_pts);
        self.diagnostics.selected_pts = selected_pts;
        self.diagnostics.temporal_approximation = temporal_selection_is_approximate(
            requested_pts,
            selected_pts,
            hit_tolerance_pts,
            policy,
        );
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

    fn with_hardware_decode_plan(mut self, plan: &PreviewHardwareDecodePlan) -> Self {
        self.diagnostics = self.diagnostics.with_hardware_decode_plan(plan);
        self.diagnostics.decoded_frame_residency = DecodedFrameResidency::CpuFloat;
        self
    }

    fn with_decoded_surface_format(mut self, format: DecodedVideoSurfaceFormat) -> Self {
        if self.diagnostics.decoded_surface_format == DecodedVideoSurfaceFormat::Unknown {
            self.diagnostics.decoded_surface_format = format;
        }
        self
    }

    fn with_decoded_video_sampling(mut self, sampling: DecodedVideoSampling) -> Self {
        self.diagnostics.decoded_video_sampling = sampling;
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

    fn with_decode_execution(mut self) -> Self {
        self.decode_execution = self.diagnostics.execution_path();
        self
    }

    fn into_cache_hit(mut self, elapsed: Duration, access_mode: PreviewDecodeAccessMode) -> Self {
        let decoded_surface_format = self.diagnostics.decoded_surface_format;
        let decoded_video_sampling = self.diagnostics.decoded_video_sampling;
        self.diagnostics = PreviewDecodeDiagnostics::cache_hit_for_mode(elapsed, access_mode);
        self.diagnostics.decoded_frame_residency = DecodedFrameResidency::CpuFloat;
        self.diagnostics.decoded_surface_format = decoded_surface_format;
        self.diagnostics.decoded_video_sampling = decoded_video_sampling;
        self
    }

    fn into_playback_ring_hit(mut self, elapsed: Duration) -> Self {
        let decoded_surface_format = self.diagnostics.decoded_surface_format;
        let decoded_video_sampling = self.diagnostics.decoded_video_sampling;
        self.diagnostics = PreviewDecodeDiagnostics::playback_ring_hit(elapsed);
        self.diagnostics.decoded_frame_residency = DecodedFrameResidency::CpuFloat;
        self.diagnostics.decoded_surface_format = decoded_surface_format;
        self.diagnostics.decoded_video_sampling = decoded_video_sampling;
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
    should_cancel: impl Fn() -> bool + Send + Sync + 'static,
) -> Result<PreviewDecodeOutcome> {
    let should_cancel: PreviewDecodeCancelProbe = Arc::new(should_cancel);
    decode_preview_frame_outcome(
        request.path,
        request.source_time,
        request.max_width,
        request.max_height,
        request.access_mode,
        request.fingerprint,
        request.adaptive_hints,
        request.hardware_decode_request,
        request.hardware_decode_device_selector,
        request.source_color,
        should_cancel,
    )
}

/// Result of a cancellable preview decode request.
#[derive(Debug, Clone)]
pub enum PreviewDecodeOutcome {
    /// Decode completed with a CPU RGBA preview frame.
    Frame(RgbaFrame),
    /// Decode completed with a CPU scene-linear RGBA f32 preview frame.
    FloatFrame(FloatRgbaFrame),
    /// Decode completed with a GPU-resident native frame.
    NativeGpuFrame(PreviewNativeDecodedFrame),
    /// The caller marked this request obsolete before a frame was returned.
    Canceled(PreviewDecodeCancellation),
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
    device_selector: Option<HwAccelDeviceSelector>,
    hardware_cpu_transfer_configured: bool,
    hardware_cpu_transfer_observed: bool,
    hardware_cpu_transfer_status: PreviewHardwareDecodeCpuTransferStatus,
    native_decode_fallback: Option<PreviewNativeDecodeFallback>,
}

impl PreviewHardwareDecodePlan {
    fn resolve(
        request: PreviewHardwareDecodeRequest,
        access_mode: PreviewDecodeAccessMode,
        backend: PreviewDecodeBackend,
        codec_id: ffmpeg::codec::Id,
        device_selector: Option<HwAccelDeviceSelector>,
    ) -> Self {
        let probe = HwAccelBackend::probe();
        let (probe, ffmpeg_codec_config, ffmpeg_device_context) = Self::resolve_backend_probes(
            request,
            access_mode,
            backend,
            codec_id,
            device_selector,
            probe,
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
            device_selector,
            hardware_cpu_transfer_configured: false,
            hardware_cpu_transfer_observed: false,
            hardware_cpu_transfer_status: PreviewHardwareDecodeCpuTransferStatus::NotAttempted,
            native_decode_fallback: None,
        }
    }

    fn resolve_backend_probes(
        request: PreviewHardwareDecodeRequest,
        access_mode: PreviewDecodeAccessMode,
        backend: PreviewDecodeBackend,
        codec_id: ffmpeg::codec::Id,
        device_selector: Option<HwAccelDeviceSelector>,
        mut probe: HwAccelProbe,
    ) -> (
        HwAccelProbe,
        HwAccelCodecConfigProbe,
        HwAccelDeviceContextProbe,
    ) {
        let mut fallback = None;
        for candidate in probe.candidate_backends.iter().copied() {
            let codec_config = candidate.probe_ffmpeg_codec_config(codec_id);
            let device_context = Self::device_context_probe_for_plan(
                request,
                access_mode,
                backend,
                candidate,
                &codec_config,
                device_selector,
            );
            fallback
                .get_or_insert_with(|| (candidate, codec_config.clone(), device_context.clone()));
            let codec_ready = codec_config.backend_maps_to_ffmpeg_device
                && codec_config.ffmpeg_device_type_available
                && codec_config.ffmpeg_decoder_available
                && codec_config.ffmpeg_codec_config_available;
            if !codec_ready {
                continue;
            }
            if request.prefers_gpu_residency()
                && !ffmpeg_native_resource_adapter_available(&codec_config)
            {
                continue;
            }
            if request.prefers_gpu_residency()
                && device_selector.is_some_and(|selector| !selector.selects_backend(candidate))
            {
                continue;
            }
            if Self::plan_requires_device_context(request, access_mode, backend)
                && !device_context.device_context_created
            {
                continue;
            }
            probe.candidate_backend = Some(candidate);
            probe.candidate_handle_kind = candidate.native_handle_kind();
            probe.candidate_surface_formats = candidate.preferred_surface_formats();
            probe.decoder_adapter_available =
                ffmpeg_native_resource_adapter_available(&codec_config);
            return (probe, codec_config, device_context);
        }

        let (candidate, codec_config, device_context) = fallback.unwrap_or_else(|| {
            let backend = HwAccelBackend::None;
            (
                backend,
                backend.probe_ffmpeg_codec_config(codec_id),
                HwAccelDeviceContextProbe::unavailable(
                    backend,
                    "no platform hardware decode backend candidates are available",
                ),
            )
        });
        probe.candidate_backend = if candidate == HwAccelBackend::None {
            None
        } else {
            Some(candidate)
        };
        probe.candidate_handle_kind = candidate.native_handle_kind();
        probe.candidate_surface_formats = candidate.preferred_surface_formats();
        (probe, codec_config, device_context)
    }

    fn plan_requires_device_context(
        request: PreviewHardwareDecodeRequest,
        _access_mode: PreviewDecodeAccessMode,
        backend: PreviewDecodeBackend,
    ) -> bool {
        matches!(
            request,
            PreviewHardwareDecodeRequest::PreferHardwareDecode
                | PreviewHardwareDecodeRequest::PreferGpuResident
                | PreviewHardwareDecodeRequest::RequireGpuResident
        ) && backend != PreviewDecodeBackend::ExternalFfmpegCpuRgba
    }

    fn should_configure_hardware_decoder(&self, _access_mode: PreviewDecodeAccessMode) -> bool {
        self.request != PreviewHardwareDecodeRequest::Auto
            && self.ffmpeg_codec_config.ffmpeg_codec_config_available
            && self.ffmpeg_device_context.device_context_created
    }

    fn allows_cpu_transfer_fallback(&self) -> bool {
        matches!(
            self.request,
            PreviewHardwareDecodeRequest::PreferHardwareDecode
                | PreviewHardwareDecodeRequest::PreferGpuResident
        )
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
            self.probe.reason = format!(
                "{} FFmpeg hardware decode is active; this request permits transfer to the CPU RGBA boundary",
                backend.as_str()
            );
        }
    }

    fn mark_native_decode_fallback(&mut self, reason: PreviewNativeDecodeFallback) {
        self.native_decode_fallback = Some(reason);
    }

    fn mark_gpu_resident_native_observed(&mut self, kind: DecodedGpuFrameHandleKind) {
        self.hardware_cpu_transfer_configured = false;
        self.hardware_cpu_transfer_observed = false;
        self.hardware_cpu_transfer_status = PreviewHardwareDecodeCpuTransferStatus::NotAttempted;
        self.native_decode_fallback = None;
        self.decision = PreviewHardwareDecodeDecision::GpuResidentNative;
        let backend = self.probe.candidate_backend.unwrap_or(HwAccelBackend::None);
        self.probe.selected_backend = backend;
        self.probe.decoder_adapter_available = true;
        self.probe.hardware_decode_active = true;
        self.probe.zero_copy_active = true;
        self.probe.frame_residency = DecodedFrameResidency::GpuTexture;
        self.probe.gpu_frame_handle_kind = Some(kind);
        self.probe.reason = format!(
            "{} FFmpeg hardware decode produced a retained native decoder surface",
            backend.as_str()
        );
    }

    fn device_context_probe_for_plan(
        request: PreviewHardwareDecodeRequest,
        access_mode: PreviewDecodeAccessMode,
        backend: PreviewDecodeBackend,
        selected_backend: HwAccelBackend,
        ffmpeg_codec_config: &HwAccelCodecConfigProbe,
        device_selector: Option<HwAccelDeviceSelector>,
    ) -> HwAccelDeviceContextProbe {
        if !Self::plan_requires_device_context(request, access_mode, backend)
            || !ffmpeg_codec_config.ffmpeg_codec_config_available
        {
            return HwAccelDeviceContextProbe::unavailable(
                selected_backend,
                "hardware device context creation was not required for this preview plan",
            );
        }
        selected_backend.cached_ffmpeg_device_context_probe_for(device_selector)
    }

    fn decision_for(
        request: PreviewHardwareDecodeRequest,
        _access_mode: PreviewDecodeAccessMode,
        backend: PreviewDecodeBackend,
        probe: &HwAccelProbe,
        ffmpeg_codec_config: &HwAccelCodecConfigProbe,
        ffmpeg_device_context: &HwAccelDeviceContextProbe,
    ) -> PreviewHardwareDecodeDecision {
        if request == PreviewHardwareDecodeRequest::Auto {
            return PreviewHardwareDecodeDecision::CpuRgbaNotRequested;
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
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D12
            | ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D11
            | ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D11VA_VLD
            | ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_DXVA2_VLD
            | ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX
            | ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VAAPI
            | ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VDPAU
            | ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_CUDA
    )
}

fn ffmpeg_native_resource_adapter_available(config: &HwAccelCodecConfigProbe) -> bool {
    matches!(
        config.hw_pixel_format,
        Some(HwAccelPixelFormat::D3D12 | HwAccelPixelFormat::D3D11)
    )
}

struct PreviewDecodeSession {
    path: PathBuf,
    fingerprint: MediaFileFingerprint,
    max_width: Option<u32>,
    max_height: Option<u32>,
    backend: PreviewDecodeBackend,
    hardware_decode_request: PreviewHardwareDecodeRequest,
    hardware_decode_device_selector: Option<HwAccelDeviceSelector>,
    source_color: PreviewSourceColorContract,
    codec_id: ffmpeg::codec::Id,
    input: ffmpeg::format::context::Input,
    // Declared after `input` so the AVFormatContext releases its callback use
    // before the callback state is dropped.
    interrupt_state: Arc<PreviewDecodeInterruptState>,
    decoder: ffmpeg::decoder::Video,
    scaler: Option<ffmpeg::software::scaling::Context>,
    scaler_source_format: Option<ffmpeg::util::format::pixel::Pixel>,
    stream_index: usize,
    stream_tb: ffmpeg::Rational,
    /// Absolute stream PTS representing media-source-local time zero.
    stream_start_pts: i64,
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
    frame: Option<PreviewDecodedFramePayload>,
    selected_pts: Option<i64>,
    decoded_frame_count: usize,
    canceled: bool,
}

#[derive(Debug, Clone)]
enum PreviewDecodedFramePayload {
    CpuRgba(RgbaFrame),
    CpuFloat(FloatRgbaFrame),
    NativeGpu(PreviewNativeDecodedFrame),
}

impl From<RgbaFrame> for PreviewDecodedFramePayload {
    fn from(frame: RgbaFrame) -> Self {
        Self::CpuRgba(frame)
    }
}

impl From<FloatRgbaFrame> for PreviewDecodedFramePayload {
    fn from(frame: FloatRgbaFrame) -> Self {
        Self::CpuFloat(frame)
    }
}

impl PreviewDecodedFramePayload {
    fn cache_cpu_frame(
        &self,
        path: &Path,
        fingerprint: MediaFileFingerprint,
        width: u32,
        height: u32,
        pts: i64,
    ) {
        let frame = match self {
            Self::CpuRgba(frame) => Self::CpuRgba(frame.clone().with_decode_execution()),
            Self::CpuFloat(frame) => Self::CpuFloat(frame.clone().with_decode_execution()),
            Self::NativeGpu(_) => return,
        };
        preview_cache_put_with_fingerprint(path, fingerprint, width, height, pts, frame);
    }

    fn source_color(&self) -> Option<PreviewSourceColorContract> {
        match self {
            Self::CpuRgba(frame) => Some(frame.color_contract.source),
            Self::CpuFloat(frame) => Some(frame.color_contract.source),
            Self::NativeGpu(_) => None,
        }
    }

    fn into_cache_hit(self, elapsed: Duration, access_mode: PreviewDecodeAccessMode) -> Self {
        match self {
            Self::CpuRgba(frame) => Self::CpuRgba(frame.into_cache_hit(elapsed, access_mode)),
            Self::CpuFloat(frame) => Self::CpuFloat(frame.into_cache_hit(elapsed, access_mode)),
            Self::NativeGpu(frame) => Self::NativeGpu(frame),
        }
    }

    fn into_playback_ring_hit(self, elapsed: Duration) -> Self {
        match self {
            Self::CpuRgba(frame) => Self::CpuRgba(frame.into_playback_ring_hit(elapsed)),
            Self::CpuFloat(frame) => Self::CpuFloat(frame.into_playback_ring_hit(elapsed)),
            Self::NativeGpu(frame) => Self::NativeGpu(frame),
        }
    }

    fn with_access_policy(self, policy: PreviewDecodeAccessPolicy) -> Self {
        match self {
            Self::CpuRgba(frame) => Self::CpuRgba(frame.with_access_policy(policy)),
            Self::CpuFloat(frame) => Self::CpuFloat(frame.with_access_policy(policy)),
            Self::NativeGpu(frame) => Self::NativeGpu(frame),
        }
    }

    fn with_seek_index_diagnostics(
        self,
        diagnostics: PreviewSeekIndexDiagnostics,
        resolution: PreviewSeekResolution,
    ) -> Self {
        match self {
            Self::CpuRgba(frame) => {
                Self::CpuRgba(frame.with_seek_index_diagnostics(diagnostics, resolution))
            }
            Self::CpuFloat(frame) => {
                Self::CpuFloat(frame.with_seek_index_diagnostics(diagnostics, resolution))
            }
            Self::NativeGpu(frame) => Self::NativeGpu(frame),
        }
    }

    fn with_stage_durations(self, durations: PreviewDecodeStageDurations) -> Self {
        match self {
            Self::CpuRgba(frame) => Self::CpuRgba(frame.with_stage_durations(durations)),
            Self::CpuFloat(frame) => Self::CpuFloat(frame.with_stage_durations(durations)),
            Self::NativeGpu(frame) => Self::NativeGpu(frame),
        }
    }

    fn with_hardware_decode_plan(self, plan: &PreviewHardwareDecodePlan) -> Self {
        match self {
            Self::CpuRgba(frame) => Self::CpuRgba(frame.with_hardware_decode_plan(plan)),
            Self::CpuFloat(frame) => Self::CpuFloat(frame.with_hardware_decode_plan(plan)),
            Self::NativeGpu(frame) => Self::NativeGpu(frame),
        }
    }

    fn with_decoded_surface_format(self, format: DecodedVideoSurfaceFormat) -> Self {
        match self {
            Self::CpuRgba(frame) => Self::CpuRgba(frame.with_decoded_surface_format(format)),
            Self::CpuFloat(frame) => Self::CpuFloat(frame.with_decoded_surface_format(format)),
            Self::NativeGpu(frame) => Self::NativeGpu(frame),
        }
    }

    fn into_outcome(self) -> PreviewDecodeOutcome {
        match self {
            Self::CpuRgba(frame) => PreviewDecodeOutcome::Frame(frame),
            Self::CpuFloat(frame) => PreviewDecodeOutcome::FloatFrame(frame),
            Self::NativeGpu(frame) => PreviewDecodeOutcome::NativeGpuFrame(frame),
        }
    }
}

struct RetainedDecodedFrame(ffmpeg::util::frame::video::Video);

impl RetainedDecodedFrame {
    fn retain(frame: &ffmpeg::util::frame::video::Video, path: &Path) -> Result<Self> {
        // SAFETY: frame.as_ptr() is valid for this borrow. av_frame_clone
        // creates an independently owned frame and retains every AVBufferRef,
        // including hardware decoder surfaces.
        let retained = unsafe { ffmpeg::ffi::av_frame_clone(frame.as_ptr()) };
        if retained.is_null() {
            return Err(MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: "FFmpeg could not retain a decoded frame candidate".to_owned(),
            });
        }
        // SAFETY: retained is a fresh av_frame_clone allocation. ffmpeg-next's
        // Video drop calls av_frame_free exactly once for this pointer.
        Ok(Self(unsafe {
            ffmpeg::util::frame::video::Video::wrap(retained)
        }))
    }

    fn frame(&self) -> &ffmpeg::util::frame::video::Video {
        &self.0
    }
}

impl PreviewDecodeForwardResult {
    fn frame(
        frame: PreviewDecodedFramePayload,
        selected_pts: i64,
        decoded_frame_count: usize,
    ) -> Self {
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

    fn nearest_keyframe(&self, target_pts: i64) -> Option<i64> {
        let before = self.keyframe_at_or_before(target_pts);
        let after = self.keyframe_after(target_pts);
        match (before, after) {
            (Some(before), Some(after)) => {
                if target_pts.saturating_sub(before) <= after.saturating_sub(target_pts) {
                    Some(before)
                } else {
                    Some(after)
                }
            }
            (Some(before), None) => Some(before),
            (None, Some(after)) => Some(after),
            (None, None) => None,
        }
    }

    fn adjacent_keyframe_radius(&self, target_pts: i64) -> Option<i64> {
        let search = self.keyframe_pts.binary_search(&target_pts);
        let insertion = search.unwrap_or_else(|index| index);
        let before = insertion
            .checked_sub(1)
            .and_then(|index| self.keyframe_pts.get(index))
            .map(|pts| target_pts.saturating_sub(*pts).abs());
        let after_index = match search {
            Ok(index) => index.saturating_add(1),
            Err(index) => index,
        };
        let after = self
            .keyframe_pts
            .get(after_index)
            .map(|pts| pts.saturating_sub(target_pts).abs());
        match (before, after) {
            (Some(before), Some(after)) => Some(before.max(after)),
            (Some(distance), None) | (None, Some(distance)) => Some(distance),
            (None, None) => None,
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
    fingerprint: MediaFileFingerprint,
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
    fingerprint: MediaFileFingerprint,
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
    fingerprint: MediaFileFingerprint,
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
    frame: PreviewDecodedFramePayload,
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

    fn get(&mut self, target_pts: i64, tolerance_pts: i64) -> Option<PreviewDecodedFramePayload> {
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

    fn put(&mut self, pts: i64, frame: impl Into<PreviewDecodedFramePayload>) {
        let frame = frame.into();
        if let Some(index) = self.entries.iter().position(|entry| entry.pts == pts) {
            self.entries.remove(index);
        }
        self.entries.push_front(PreviewPlaybackRingEntry { pts, frame });
        while self.entries.len() > self.capacity {
            self.entries.pop_back();
        }
    }
}

/// Stable media-file revision identity shared by decode and derived-work snapshots.
///
/// The optional fields let callers represent missing/unreadable metadata
/// without falling back to a false stable identity. A successful app/media path
/// probe should prefer [`MediaFileFingerprint::from_metadata`].
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct MediaFileFingerprint {
    /// File length in bytes when available.
    pub len: Option<u64>,
    /// File modification time seconds since Unix epoch when available.
    pub modified_secs: Option<u64>,
    /// File modification time subsecond nanoseconds when available.
    pub modified_nanos: Option<u32>,
}

impl MediaFileFingerprint {
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
    let device_context = backend
        .create_ffmpeg_device_context(plan.device_selector)
        .map_err(|probe| probe.reason)?;
    device_context.attach_to_codec_context(context)?;

    let mut state =
        Box::new(PreviewHardwareDecodeContextState { preferred_hw_pixel_format: hw_pixel_format });
    unsafe {
        (*context.as_mut_ptr()).extra_hw_frames = preview_hardware_extra_frames(plan.request);
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

fn open_preview_input(
    path: &Path,
    interrupt_state: &Arc<PreviewDecodeInterruptState>,
) -> Result<ffmpeg::format::context::Input> {
    let path_string = path.to_string_lossy();
    let path_c =
        CString::new(path_string.as_bytes()).map_err(|error| MondrianError::MediaOpen {
            path: path.display().to_string(),
            reason: format!("media path contains an interior NUL byte: {error}"),
        })?;

    // SAFETY: the allocated context is either transferred into the safe
    // ffmpeg-next Input wrapper or closed on every error path. The callback
    // opaque pointer targets an Arc allocation owned by the resulting session.
    unsafe {
        let mut input = ffmpeg::ffi::avformat_alloc_context();
        if input.is_null() {
            return Err(MondrianError::MediaOpen {
                path: path.display().to_string(),
                reason: "FFmpeg could not allocate an input context".to_string(),
            });
        }
        (*input).interrupt_callback = ffmpeg::ffi::AVIOInterruptCB {
            callback: Some(preview_decode_interrupt_callback),
            opaque: Arc::as_ptr(interrupt_state).cast_mut().cast(),
        };

        interrupt_state.set_checkpoint(PreviewDecodeCancellationCheckpoint::InputOpen);
        let open_result = ffmpeg::ffi::avformat_open_input(
            &mut input,
            path_c.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if open_result < 0 {
            if !input.is_null() {
                ffmpeg::ffi::avformat_close_input(&mut input);
            }
            return Err(MondrianError::MediaOpen {
                path: path.display().to_string(),
                reason: ffmpeg::Error::from(open_result).to_string(),
            });
        }

        interrupt_state.set_checkpoint(PreviewDecodeCancellationCheckpoint::StreamInfo);
        let stream_info_result =
            ffmpeg::ffi::avformat_find_stream_info(input, std::ptr::null_mut());
        if stream_info_result < 0 {
            ffmpeg::ffi::avformat_close_input(&mut input);
            return Err(MondrianError::MediaOpen {
                path: path.display().to_string(),
                reason: ffmpeg::Error::from(stream_info_result).to_string(),
            });
        }

        Ok(ffmpeg::format::context::Input::wrap(input))
    }
}

impl PreviewDecodeSession {
    fn open(
        path: &Path,
        fingerprint: MediaFileFingerprint,
        max_width: Option<u32>,
        max_height: Option<u32>,
        access_mode: PreviewDecodeAccessMode,
        backend: PreviewDecodeBackend,
        hardware_decode_request: PreviewHardwareDecodeRequest,
        hardware_decode_device_selector: Option<HwAccelDeviceSelector>,
        source_color: PreviewSourceColorContract,
        interrupt_state: Arc<PreviewDecodeInterruptState>,
    ) -> Result<Self> {
        let input = open_preview_input(path, &interrupt_state)?;
        Self::from_input(
            input,
            interrupt_state,
            path,
            fingerprint,
            max_width,
            max_height,
            access_mode,
            backend,
            hardware_decode_request,
            hardware_decode_device_selector,
            source_color,
        )
    }

    fn from_input(
        input: ffmpeg::format::context::Input,
        interrupt_state: Arc<PreviewDecodeInterruptState>,
        path: &Path,
        fingerprint: MediaFileFingerprint,
        max_width: Option<u32>,
        max_height: Option<u32>,
        access_mode: PreviewDecodeAccessMode,
        backend: PreviewDecodeBackend,
        hardware_decode_request: PreviewHardwareDecodeRequest,
        hardware_decode_device_selector: Option<HwAccelDeviceSelector>,
        source_color: PreviewSourceColorContract,
    ) -> Result<Self> {
        let (stream_index, parameters, stream_tb, stream_start_pts, stream_rate, seek_index) = {
            let stream = input.streams().best(ffmpeg::media::Type::Video).ok_or_else(|| {
                MondrianError::UnsupportedFormat { format: "no video stream".to_string() }
            })?;
            let stream_index = stream.index();
            let stream_start_pts = match stream.start_time() {
                value if value == ffmpeg::ffi::AV_NOPTS_VALUE => 0,
                value => value,
            };
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
                stream_start_pts,
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
            hardware_decode_device_selector,
        );
        let requested_threading = preview_decode_threading_config_for_codec(codec_id);
        let ffmpeg_threading = ffmpeg::codec::threading::Config {
            kind: requested_threading.kind.to_ffmpeg(),
            count: requested_threading.count,
        };

        let mut hardware_decode_context_state = None;
        let mut context =
            preview_decode_context_from_parameters(parameters.clone(), ffmpeg_threading, path)?;
        if hardware_decode_plan.should_configure_hardware_decoder(access_mode)
            && backend != PreviewDecodeBackend::Software
        {
            match configure_preview_hardware_decode_context(&mut context, &hardware_decode_plan) {
                Ok((state, device_context)) => {
                    if hardware_decode_plan.allows_cpu_transfer_fallback() {
                        hardware_decode_plan
                            .mark_hardware_cpu_transfer_configured(device_context.backend());
                    }
                    hardware_decode_context_state = Some(state);
                }
                Err(reason) => {
                    hardware_decode_plan.mark_hardware_cpu_transfer_setup_failed();
                    if hardware_decode_request.requires_gpu_residency() {
                        return Err(MondrianError::DecodeFailed {
                            asset_id: path.display().to_string(),
                            reason: format!(
                                "required GPU-resident FFmpeg decoder setup failed: {reason}"
                            ),
                        });
                    }
                    preview_trace(format!(
                        "[preview] hardware decode CPU-transfer setup failed, fallback software: {reason}"
                    ));
                }
            }
        }

        let decoder = match context.decoder().video() {
            Ok(decoder) => decoder,
            Err(err) if hardware_decode_context_state.is_some() => {
                if hardware_decode_request.requires_gpu_residency() {
                    return Err(MondrianError::DecodeFailed {
                        asset_id: path.display().to_string(),
                        reason: format!(
                            "required GPU-resident FFmpeg decoder failed to open: {err}"
                        ),
                    });
                }
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

        let defer_or_bypass_rgba_scaler = source_color.color_space.is_scene_linear()
            || (hardware_decode_context_state.is_some()
                && preview_hardware_frame_format(decoder.format()));
        let (scaler, scaler_source_format) = if defer_or_bypass_rgba_scaler {
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
            hardware_decode_device_selector,
            source_color,
            codec_id,
            input,
            interrupt_state,
            decoder,
            scaler,
            scaler_source_format,
            stream_index,
            stream_tb,
            stream_start_pts,
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
        fingerprint: MediaFileFingerprint,
        max_width: Option<u32>,
        max_height: Option<u32>,
        backend: PreviewDecodeBackend,
        hardware_decode_request: PreviewHardwareDecodeRequest,
        hardware_decode_device_selector: Option<HwAccelDeviceSelector>,
        source_color: PreviewSourceColorContract,
    ) -> bool {
        self.path == path
            && self.fingerprint == fingerprint
            && self.max_width == max_width
            && self.max_height == max_height
            && self.backend == backend
            && self.hardware_decode_request == hardware_decode_request
            && self.hardware_decode_device_selector == hardware_decode_device_selector
            && self.source_color == source_color
    }

    fn decode_at(
        &mut self,
        source_time: TimelineTime,
        access_mode: PreviewDecodeAccessMode,
        adaptive_hints: PreviewDecodeAdaptiveHints,
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<PreviewDecodeOutcome> {
        if should_cancel() {
            return Ok(PreviewDecodeOutcome::Canceled(
                self.interrupt_state
                    .cancellation(PreviewDecodeCancellationCheckpoint::BeforeInputOpen),
            ));
        }
        let target_pts =
            source_time_to_stream_pts(source_time, self.stream_tb, self.stream_start_pts).map_err(
                |reason| MondrianError::DecodeFailed {
                    asset_id: self.path.display().to_string(),
                    reason,
                },
            )?;
        let policy = PreviewDecodeAccessPolicy::for_access_mode(access_mode).adapt_for_request(
            &self.seek_index,
            target_pts,
            self.frame_duration_pts,
            adaptive_hints,
        );
        let decode_target_pts = if policy.keyframe_only {
            self.seek_index.nearest_keyframe(target_pts).unwrap_or(target_pts)
        } else {
            target_pts
        };
        self.decoder.skip_frame(if policy.keyframe_only {
            ffmpeg::codec::discard::Discard::NonKey
        } else {
            ffmpeg::codec::discard::Discard::Default
        });

        self.interrupt_state
            .set_checkpoint(PreviewDecodeCancellationCheckpoint::CacheLookup);
        let cache_lookup_started_at = Instant::now();
        let allow_cpu_cache = !self.hardware_decode_request.prefers_gpu_residency();
        if allow_cpu_cache && policy.use_playback_ring {
            if let Some(hit) = self.playback_ring.get(target_pts, self.hit_tolerance_pts) {
                if should_cancel() {
                    return Ok(PreviewDecodeOutcome::Canceled(
                        self.interrupt_state
                            .cancellation(PreviewDecodeCancellationCheckpoint::CacheLookup),
                    ));
                }
                return Ok(hit
                    .into_playback_ring_hit(cache_lookup_started_at.elapsed())
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
                    .with_decoded_surface_format(self.decoded_surface_format)
                    .into_outcome());
            }
        }
        if allow_cpu_cache {
            if let Some(hit) = preview_cache_get(
                &self.path,
                self.fingerprint,
                self.source_color,
                self.target_width,
                self.target_height,
                target_pts,
                self.hit_tolerance_pts,
            ) {
                if should_cancel() {
                    return Ok(PreviewDecodeOutcome::Canceled(
                        self.interrupt_state
                            .cancellation(PreviewDecodeCancellationCheckpoint::CacheLookup),
                    ));
                }
                if policy.use_playback_ring {
                    self.playback_ring.put(hit.pts, hit.frame.clone());
                }
                return Ok(hit
                    .frame
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
                    .with_decoded_surface_format(self.decoded_surface_format)
                    .into_outcome());
            }
        }
        let cache_lookup_us = duration_us(cache_lookup_started_at.elapsed());

        let should_continue_forward = self
            .last_pts
            .map(|last| {
                policy.can_continue_forward(
                    last,
                    decode_target_pts,
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
                return Ok(PreviewDecodeOutcome::Canceled(
                    self.interrupt_state.cancellation(PreviewDecodeCancellationCheckpoint::Seek),
                ));
            }
            self.interrupt_state.set_checkpoint(PreviewDecodeCancellationCheckpoint::Seek);
            let seek_started_at = Instant::now();
            seek_resolution = match self.seek_to_target(decode_target_pts, policy) {
                Ok(resolution) => resolution,
                Err(_) if should_cancel() => {
                    return Ok(PreviewDecodeOutcome::Canceled(
                        self.interrupt_state
                            .cancellation(PreviewDecodeCancellationCheckpoint::Seek),
                    ));
                }
                Err(error) => return Err(error),
            };
            seek_us = duration_us(seek_started_at.elapsed());
        }

        if should_cancel() {
            return Ok(PreviewDecodeOutcome::Canceled(
                self.interrupt_state.cancellation(PreviewDecodeCancellationCheckpoint::Codec),
            ));
        }
        let decode_started_at = Instant::now();
        let result = self.decode_forward_until(decode_target_pts, policy, should_cancel)?;
        if result.canceled {
            return Ok(PreviewDecodeOutcome::Canceled(
                self.interrupt_state.cancellation(PreviewDecodeCancellationCheckpoint::Codec),
            ));
        }
        if let Some(frame) = result.frame {
            match frame {
                PreviewDecodedFramePayload::CpuRgba(frame) => {
                    let conversion_us = frame
                        .diagnostics
                        .stage_durations
                        .hardware_transfer_us
                        .saturating_add(frame.diagnostics.stage_durations.swscale_us)
                        .saturating_add(frame.diagnostics.stage_durations.rgba_copy_us);
                    let packet_decode_us =
                        duration_us(decode_started_at.elapsed()).saturating_sub(conversion_us);
                    let frame = frame
                        .with_access_mode(access_mode)
                        .with_stage_durations(PreviewDecodeStageDurations {
                            cache_lookup_us,
                            seek_us,
                            packet_decode_us,
                            ..PreviewDecodeStageDurations::default()
                        })
                        .with_decode_work(seek_performed, result.decoded_frame_count)
                        .with_temporal_selection(
                            target_pts,
                            result.selected_pts,
                            self.hit_tolerance_pts,
                            policy,
                        )
                        .with_access_policy(policy)
                        .with_forward_reused(should_continue_forward)
                        .with_seek_index_diagnostics(self.seek_index.diagnostics(), seek_resolution)
                        .with_threading(self.threading_kind, self.threading_count)
                        .with_hardware_decode_plan(&self.hardware_decode_plan)
                        .with_decoded_surface_format(self.decoded_surface_format)
                        .with_decode_execution();
                    if policy.use_playback_ring {
                        if let Some(selected_pts) = result.selected_pts {
                            self.playback_ring.put(
                                selected_pts,
                                PreviewDecodedFramePayload::CpuRgba(frame.clone()),
                            );
                        }
                    }
                    return Ok(PreviewDecodeOutcome::Frame(frame));
                }
                PreviewDecodedFramePayload::CpuFloat(frame) => {
                    let conversion_us = frame
                        .diagnostics
                        .stage_durations
                        .hardware_transfer_us
                        .saturating_add(frame.diagnostics.stage_durations.swscale_us)
                        .saturating_add(frame.diagnostics.stage_durations.rgba_copy_us);
                    let packet_decode_us =
                        duration_us(decode_started_at.elapsed()).saturating_sub(conversion_us);
                    let frame = frame
                        .with_access_mode(access_mode)
                        .with_stage_durations(PreviewDecodeStageDurations {
                            cache_lookup_us,
                            seek_us,
                            packet_decode_us,
                            ..PreviewDecodeStageDurations::default()
                        })
                        .with_decode_work(seek_performed, result.decoded_frame_count)
                        .with_temporal_selection(
                            target_pts,
                            result.selected_pts,
                            self.hit_tolerance_pts,
                            policy,
                        )
                        .with_access_policy(policy)
                        .with_forward_reused(should_continue_forward)
                        .with_seek_index_diagnostics(self.seek_index.diagnostics(), seek_resolution)
                        .with_threading(self.threading_kind, self.threading_count)
                        .with_hardware_decode_plan(&self.hardware_decode_plan)
                        .with_decoded_surface_format(self.decoded_surface_format)
                        .with_decode_execution();
                    if policy.use_playback_ring {
                        if let Some(selected_pts) = result.selected_pts {
                            self.playback_ring.put(
                                selected_pts,
                                PreviewDecodedFramePayload::CpuFloat(frame.clone()),
                            );
                        }
                    }
                    return Ok(PreviewDecodeOutcome::FloatFrame(frame));
                }
                PreviewDecodedFramePayload::NativeGpu(mut frame) => {
                    let mut diagnostics = frame.diagnostics.with_access_mode(access_mode);
                    diagnostics.stage_durations.accumulate(PreviewDecodeStageDurations {
                        cache_lookup_us,
                        seek_us,
                        packet_decode_us: duration_us(decode_started_at.elapsed()),
                        ..PreviewDecodeStageDurations::default()
                    });
                    diagnostics.seek_performed = seek_performed;
                    diagnostics.requested_pts = Some(target_pts);
                    diagnostics.selected_pts = result.selected_pts;
                    diagnostics.temporal_approximation = temporal_selection_is_approximate(
                        target_pts,
                        result.selected_pts,
                        self.hit_tolerance_pts,
                        policy,
                    );
                    diagnostics.decoded_frame_count =
                        result.decoded_frame_count.min(u32::MAX as usize) as u32;
                    diagnostics = diagnostics.with_access_policy(policy);
                    diagnostics.forward_reused = should_continue_forward;
                    let seek_index = self.seek_index.diagnostics();
                    diagnostics.seek_index_available = seek_index.available;
                    diagnostics.seek_index_keyframes = seek_index.keyframes;
                    diagnostics.seek_index_observed_packets = seek_index.observed_packets;
                    diagnostics.seek_index_source = seek_index.source;
                    diagnostics.seek_index_used = seek_resolution.used_index;
                    diagnostics.seek_index_anchor_pts = seek_resolution.anchor_pts;
                    diagnostics.threading_kind = self.threading_kind;
                    diagnostics.threading_count =
                        self.threading_count.min(u32::MAX as usize) as u32;
                    frame.diagnostics =
                        diagnostics.with_hardware_decode_plan(&self.hardware_decode_plan);
                    return Ok(PreviewDecodeOutcome::NativeGpuFrame(frame));
                }
            }
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
        let (min_ts, seek_target_ts, max_ts, seek_flags, used_anchor_pts) =
            match policy.seek_strategy {
                PreviewDecodeSeekStrategy::KeyframeBefore => {
                    // 关键帧安全模式：不限制 backward seek 范围，避免长 GOP 时落到不可独立解码帧。
                    (
                        seek_anchor_pts.unwrap_or(i64::MIN),
                        target_pts,
                        target_pts,
                        ffmpeg::ffi::AVSEEK_FLAG_BACKWARD,
                        seek_anchor_pts,
                    )
                }
                PreviewDecodeSeekStrategy::BoundedAnyFrame => {
                    let seek_window_secs = policy.any_seek_window_ms as f64 / 1_000.0;
                    let seek_window_pts = (seek_window_secs / tb_secs).round().max(1.0) as i64;
                    let window_min_ts = target_pts.saturating_sub(seek_window_pts);
                    let used_anchor_pts = seek_anchor_pts.filter(|anchor| {
                        *anchor <= target_pts
                            && pts_distance_to_frames(
                                target_pts.saturating_sub(*anchor),
                                self.frame_duration_pts,
                            ) <= policy.forward_decode_budget_frames
                    });
                    if let Some(anchor_pts) = used_anchor_pts {
                        (
                            anchor_pts,
                            target_pts,
                            target_pts,
                            ffmpeg::ffi::AVSEEK_FLAG_BACKWARD,
                            Some(anchor_pts),
                        )
                    } else {
                        (
                            window_min_ts,
                            target_pts,
                            target_pts.saturating_add(seek_window_pts),
                            ffmpeg::ffi::AVSEEK_FLAG_ANY,
                            None,
                        )
                    }
                }
            };

        let ret = unsafe {
            ffmpeg::ffi::avformat_seek_file(
                self.input.as_mut_ptr(),
                self.stream_index as i32,
                min_ts,
                seek_target_ts,
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
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<PreviewDecodeForwardResult> {
        let interrupt_state = Arc::clone(&self.interrupt_state);
        interrupt_state.set_checkpoint(PreviewDecodeCancellationCheckpoint::Codec);
        let mut best_before: Option<(i64, RetainedDecodedFrame)> = None;
        let mut best_after: Option<(i64, RetainedDecodedFrame)> = None;
        let mut frames_decoded: usize = 0;
        let exact_select_distance_pts =
            self.frame_duration_pts.saturating_mul(2).max(1).min(
                seconds_to_stream_pts(PREVIEW_MAX_SELECT_DISTANCE_SECS, self.stream_tb).max(1),
            );
        let max_select_distance_pts = if policy.keyframe_only {
            self.seek_index
                .adjacent_keyframe_radius(target_pts)
                .unwrap_or(exact_select_distance_pts)
                .max(exact_select_distance_pts)
        } else {
            exact_select_distance_pts
        };

        let choose_and_convert =
            |hardware_decode_plan: &mut PreviewHardwareDecodePlan,
             scaler: &mut Option<ffmpeg::software::scaling::Context>,
             scaler_source_format: &mut Option<ffmpeg::util::format::pixel::Pixel>,
             target_width: u32,
             target_height: u32,
             path: &Path,
             before: Option<&(i64, RetainedDecodedFrame)>,
             after: Option<&(i64, RetainedDecodedFrame)>|
             -> Result<Option<(i64, PreviewDecodedFramePayload)>> {
                if should_cancel() {
                    return Ok(None);
                }
                let selected = match (before, after) {
                    (Some((b_pts, b_frame)), Some((a_pts, a_frame))) => {
                        let before_dist = (target_pts - *b_pts).abs();
                        let after_dist = (*a_pts - target_pts).abs();
                        if before_dist <= after_dist {
                            Some((*b_pts, b_frame.frame()))
                        } else {
                            Some((*a_pts, a_frame.frame()))
                        }
                    }
                    (Some((b_pts, b_frame)), None) => Some((*b_pts, b_frame.frame())),
                    (None, Some((a_pts, a_frame))) => Some((*a_pts, a_frame.frame())),
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
                interrupt_state
                    .set_checkpoint(PreviewDecodeCancellationCheckpoint::FrameMaterialization);
                let frame = materialize_decoded_frame(
                    selected_frame,
                    hardware_decode_plan,
                    scaler,
                    scaler_source_format,
                    target_width,
                    target_height,
                    path,
                    self.source_color,
                )?;
                Ok(Some((selected_pts, frame)))
            };

        // A prior forward request may have returned as soon as it found its
        // target while the frame-threaded decoder still held reordered output.
        // Consume that output before submitting another packet: FFmpeg requires
        // callers to receive frames after AVERROR(EAGAIN), and the retained
        // frames are also the best candidates for the next playback position.
        while let Some(decoded) = receive_decoded_video_frame(&mut self.decoder)? {
            if should_cancel() {
                return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
            }
            frames_decoded += 1;
            let frame_pts = decoded.pts.unwrap_or(i64::MIN);
            if frame_pts != i64::MIN {
                self.last_pts = Some(frame_pts);
                if frame_pts <= target_pts {
                    best_before = Some((
                        frame_pts,
                        RetainedDecodedFrame::retain(&decoded.frame, self.path.as_path())?,
                    ));
                    if policy.accepts_first_decoded_approximation(
                        frame_pts,
                        target_pts,
                        max_select_distance_pts,
                    ) {
                        if let Some((selected_pts, frame)) = choose_and_convert(
                            &mut self.hardware_decode_plan,
                            &mut self.scaler,
                            &mut self.scaler_source_format,
                            self.target_width,
                            self.target_height,
                            self.path.as_path(),
                            best_before.as_ref(),
                            None,
                        )? {
                            if should_cancel() {
                                return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
                            }
                            return Ok(PreviewDecodeForwardResult::frame(
                                frame,
                                selected_pts,
                                frames_decoded,
                            ));
                        }
                    }
                    if frame_pts >= target_pts.saturating_sub(self.hit_tolerance_pts) {
                        interrupt_state.set_checkpoint(
                            PreviewDecodeCancellationCheckpoint::FrameMaterialization,
                        );
                        let frame = materialize_decoded_frame(
                            &decoded.frame,
                            &mut self.hardware_decode_plan,
                            &mut self.scaler,
                            &mut self.scaler_source_format,
                            self.target_width,
                            self.target_height,
                            self.path.as_path(),
                            self.source_color,
                        )?;
                        if should_cancel() {
                            return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
                        }
                        frame.cache_cpu_frame(
                            &self.path,
                            self.fingerprint,
                            self.target_width,
                            self.target_height,
                            frame_pts,
                        );
                        return Ok(PreviewDecodeForwardResult::frame(
                            frame,
                            frame_pts,
                            frames_decoded,
                        ));
                    }
                } else {
                    best_after = Some((
                        frame_pts,
                        RetainedDecodedFrame::retain(&decoded.frame, self.path.as_path())?,
                    ));
                    if let Some((selected_pts, frame)) = choose_and_convert(
                        &mut self.hardware_decode_plan,
                        &mut self.scaler,
                        &mut self.scaler_source_format,
                        self.target_width,
                        self.target_height,
                        self.path.as_path(),
                        best_before.as_ref(),
                        best_after.as_ref(),
                    )? {
                        if should_cancel() {
                            return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
                        }
                        frame.cache_cpu_frame(
                            &self.path,
                            self.fingerprint,
                            self.target_width,
                            self.target_height,
                            selected_pts,
                        );
                        return Ok(PreviewDecodeForwardResult::frame(
                            frame,
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

        let mut packets = self.input.packets();
        loop {
            interrupt_state.set_checkpoint(PreviewDecodeCancellationCheckpoint::PacketRead);
            let Some((s, packet)) = packets.next() else {
                break;
            };
            interrupt_state.set_checkpoint(PreviewDecodeCancellationCheckpoint::Codec);
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
                        best_before = Some((
                            frame_pts,
                            RetainedDecodedFrame::retain(&decoded.frame, self.path.as_path())?,
                        ));
                        if policy.accepts_first_decoded_approximation(
                            frame_pts,
                            target_pts,
                            max_select_distance_pts,
                        ) {
                            if let Some((selected_pts, frame)) = choose_and_convert(
                                &mut self.hardware_decode_plan,
                                &mut self.scaler,
                                &mut self.scaler_source_format,
                                self.target_width,
                                self.target_height,
                                self.path.as_path(),
                                best_before.as_ref(),
                                None,
                            )? {
                                if should_cancel() {
                                    return Ok(PreviewDecodeForwardResult::canceled(
                                        frames_decoded,
                                    ));
                                }
                                return Ok(PreviewDecodeForwardResult::frame(
                                    frame,
                                    selected_pts,
                                    frames_decoded,
                                ));
                            }
                        }
                        if frame_pts >= target_pts.saturating_sub(self.hit_tolerance_pts) {
                            if should_cancel() {
                                return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
                            }
                            interrupt_state.set_checkpoint(
                                PreviewDecodeCancellationCheckpoint::FrameMaterialization,
                            );
                            let frame = materialize_decoded_frame(
                                &decoded.frame,
                                &mut self.hardware_decode_plan,
                                &mut self.scaler,
                                &mut self.scaler_source_format,
                                self.target_width,
                                self.target_height,
                                self.path.as_path(),
                                self.source_color,
                            )?;
                            if should_cancel() {
                                return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
                            }
                            frame.cache_cpu_frame(
                                &self.path,
                                self.fingerprint,
                                self.target_width,
                                self.target_height,
                                frame_pts,
                            );
                            return Ok(PreviewDecodeForwardResult::frame(
                                frame,
                                frame_pts,
                                frames_decoded,
                            ));
                        }
                    } else {
                        best_after = Some((
                            frame_pts,
                            RetainedDecodedFrame::retain(&decoded.frame, self.path.as_path())?,
                        ));
                        if let Some((selected_pts, frame)) = choose_and_convert(
                            &mut self.hardware_decode_plan,
                            &mut self.scaler,
                            &mut self.scaler_source_format,
                            self.target_width,
                            self.target_height,
                            self.path.as_path(),
                            best_before.as_ref(),
                            best_after.as_ref(),
                        )? {
                            if should_cancel() {
                                return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
                            }
                            frame.cache_cpu_frame(
                                &self.path,
                                self.fingerprint,
                                self.target_width,
                                self.target_height,
                                selected_pts,
                            );
                            return Ok(PreviewDecodeForwardResult::frame(
                                frame,
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
                        best_before = Some((
                            frame_pts,
                            RetainedDecodedFrame::retain(&decoded.frame, self.path.as_path())?,
                        ));
                        if policy.accepts_first_decoded_approximation(
                            frame_pts,
                            target_pts,
                            max_select_distance_pts,
                        ) {
                            if let Some((selected_pts, frame)) = choose_and_convert(
                                &mut self.hardware_decode_plan,
                                &mut self.scaler,
                                &mut self.scaler_source_format,
                                self.target_width,
                                self.target_height,
                                self.path.as_path(),
                                best_before.as_ref(),
                                None,
                            )? {
                                if should_cancel() {
                                    return Ok(PreviewDecodeForwardResult::canceled(
                                        frames_decoded,
                                    ));
                                }
                                self.reached_eof = true;
                                return Ok(PreviewDecodeForwardResult::frame(
                                    frame,
                                    selected_pts,
                                    frames_decoded,
                                ));
                            }
                        }
                    } else {
                        best_after = Some((
                            frame_pts,
                            RetainedDecodedFrame::retain(&decoded.frame, self.path.as_path())?,
                        ));
                        if let Some((selected_pts, frame)) = choose_and_convert(
                            &mut self.hardware_decode_plan,
                            &mut self.scaler,
                            &mut self.scaler_source_format,
                            self.target_width,
                            self.target_height,
                            self.path.as_path(),
                            best_before.as_ref(),
                            best_after.as_ref(),
                        )? {
                            if should_cancel() {
                                return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
                            }
                            frame.cache_cpu_frame(
                                &self.path,
                                self.fingerprint,
                                self.target_width,
                                self.target_height,
                                selected_pts,
                            );
                            self.reached_eof = true;
                            return Ok(PreviewDecodeForwardResult::frame(
                                frame,
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

        if let Some((selected_pts, frame)) = choose_and_convert(
            &mut self.hardware_decode_plan,
            &mut self.scaler,
            &mut self.scaler_source_format,
            self.target_width,
            self.target_height,
            self.path.as_path(),
            best_before.as_ref(),
            best_after.as_ref(),
        )? {
            if should_cancel() {
                return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
            }
            frame.cache_cpu_frame(
                &self.path,
                self.fingerprint,
                self.target_width,
                self.target_height,
                selected_pts,
            );
            return Ok(PreviewDecodeForwardResult::frame(
                frame,
                selected_pts,
                frames_decoded,
            ));
        }

        if should_cancel() {
            return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
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
    source_time: TimelineTime,
    max_width: Option<u32>,
    max_height: Option<u32>,
    access_mode: PreviewDecodeAccessMode,
    fingerprint: Option<MediaFileFingerprint>,
    adaptive_hints: PreviewDecodeAdaptiveHints,
    hardware_decode_request: PreviewHardwareDecodeRequest,
    hardware_decode_device_selector: Option<HwAccelDeviceSelector>,
    source_color: PreviewSourceColorContract,
    should_cancel: PreviewDecodeCancelProbe,
) -> Result<PreviewDecodeOutcome> {
    let started_at = Instant::now();
    if should_cancel() {
        return Ok(PreviewDecodeOutcome::Canceled(
            PreviewDecodeCancellation::cooperative(
                PreviewDecodeCancellationCheckpoint::BeforeInputOpen,
            ),
        ));
    }
    ensure_ffmpeg_initialized(path)?;
    let fingerprint = fingerprint.unwrap_or_else(|| MediaFileFingerprint::capture(path));
    PREVIEW_DECODE_SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let slot = sessions.slot_mut(access_mode);
        let backend = preview_decode_backend();
        let mut session_open_us = 0;

        if should_cancel() {
            return Ok(PreviewDecodeOutcome::Canceled(
                PreviewDecodeCancellation::cooperative(
                    PreviewDecodeCancellationCheckpoint::BeforeInputOpen,
                ),
            ));
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
                    hardware_decode_device_selector,
                    source_color,
                )
            })
            .unwrap_or(false);

        if !current_match {
            let open_started_at = Instant::now();
            let interrupt_state = Arc::new(PreviewDecodeInterruptState::new());
            let _interrupt_guard = interrupt_state.install(Arc::clone(&should_cancel));
            let opened = PreviewDecodeSession::open(
                path,
                fingerprint,
                max_width,
                max_height,
                access_mode,
                backend,
                hardware_decode_request,
                hardware_decode_device_selector,
                source_color,
                Arc::clone(&interrupt_state),
            );
            *slot = match opened {
                Ok(session) => Some(session),
                Err(_) if should_cancel() => {
                    return Ok(PreviewDecodeOutcome::Canceled(
                        interrupt_state.cancellation(
                            PreviewDecodeCancellationCheckpoint::InputOpen,
                        ),
                    ));
                }
                Err(error) => return Err(error),
            };
            session_open_us = duration_us(open_started_at.elapsed());
        }

        let session = slot.as_mut().expect("preview decode session must exist");
        let interrupt_state = Arc::clone(&session.interrupt_state);
        let _interrupt_guard = interrupt_state.install(Arc::clone(&should_cancel));
        let mut external_process_us = 0;
        let external_hardware_decode_plan = PreviewHardwareDecodePlan::resolve(
            hardware_decode_request,
            access_mode,
            PreviewDecodeBackend::ExternalFfmpegCpuRgba,
            session.codec_id,
            hardware_decode_device_selector,
        );

        if preview_external_ffmpeg_cpu_rgba_enabled(access_mode) {
            interrupt_state
                .set_checkpoint(PreviewDecodeCancellationCheckpoint::ExternalProcess);
            if should_cancel() {
                return Ok(PreviewDecodeOutcome::Canceled(
                    interrupt_state
                        .cancellation(PreviewDecodeCancellationCheckpoint::ExternalProcess),
                ));
            }
            let external_started_at = Instant::now();
            if let Some(result) = try_decode_with_external_ffmpeg_cpu_rgba(
                path,
                source_time,
                session.target_width,
                session.target_height,
                source_color,
                session.decoder.format(),
                session.decoder.color_space(),
                session.decoder.color_range(),
                should_cancel.as_ref(),
            ) {
                external_process_us = duration_us(external_started_at.elapsed());
                match result {
                    Ok(Some(frame)) => {
                        if should_cancel() {
                            if !access_mode.preserves_session_on_cancel() {
                                *slot = None;
                            }
                            return Ok(PreviewDecodeOutcome::Canceled(
                                interrupt_state.cancellation(
                                    PreviewDecodeCancellationCheckpoint::ExternalProcess,
                                ),
                            ));
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
                    Ok(None) => {
                        if !access_mode.preserves_session_on_cancel() {
                            *slot = None;
                        }
                        return Ok(PreviewDecodeOutcome::Canceled(
                            interrupt_state.cancellation(
                                PreviewDecodeCancellationCheckpoint::ExternalProcess,
                            ),
                        ));
                    }
                    Err(err) => {
                        preview_trace(format!(
                            "[preview] external ffmpeg CPU RGBA decode failed, fallback software: {err}"
                        ));
                    }
                }
            }
        }

        let outcome = session.decode_at(
            source_time,
            access_mode,
            adaptive_hints,
            should_cancel.as_ref(),
        )?;
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
            PreviewDecodeOutcome::FloatFrame(frame) => Ok(PreviewDecodeOutcome::FloatFrame(
                frame
                    .with_access_mode(access_mode)
                    .with_seek_strategy(
                        PreviewDecodeAccessPolicy::for_access_mode(access_mode).seek_strategy,
                    )
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
            PreviewDecodeOutcome::Canceled(cancellation) => {
                if !access_mode.preserves_session_on_cancel() {
                    *slot = None;
                }
                Ok(PreviewDecodeOutcome::Canceled(cancellation))
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

fn preview_decode_threading_config_for_codec(
    codec_id: ffmpeg::codec::Id,
) -> PreviewDecodeThreadingConfig {
    let requested = preview_decode_threading_config();
    apply_preview_codec_threading_policy(codec_id, requested)
}

fn apply_preview_codec_threading_policy(
    codec_id: ffmpeg::codec::Id,
    requested: PreviewDecodeThreadingConfig,
) -> PreviewDecodeThreadingConfig {
    if codec_id == ffmpeg::codec::Id::EXR {
        // FFmpeg's EXR decoder can retain a single image in its frame-thread
        // queue until EOF and then deadlock while the codec context is freed on
        // Windows. Decode-pool concurrency still parallelizes independent image
        // requests, so serializing within one EXR context is the safe policy.
        return PreviewDecodeThreadingConfig { kind: PreviewDecodeThreadingKind::None, count: 1 };
    }
    requested
}

#[derive(Clone)]
struct PreviewCacheHit {
    frame: PreviewDecodedFramePayload,
    pts: i64,
}

#[derive(Clone)]
struct PreviewFrameCacheEntry {
    path: PathBuf,
    fingerprint: MediaFileFingerprint,
    source_color: PreviewSourceColorContract,
    width: u32,
    height: u32,
    pts: i64,
    frame: PreviewDecodedFramePayload,
}

fn preview_cache_get(
    path: &Path,
    fingerprint: MediaFileFingerprint,
    source_color: PreviewSourceColorContract,
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
            || entry.source_color != source_color
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
    fingerprint: MediaFileFingerprint,
    width: u32,
    height: u32,
    pts: i64,
    frame: impl Into<PreviewDecodedFramePayload>,
) {
    let frame = frame.into();
    let Some(source_color) = frame.source_color() else {
        return;
    };
    let cache = preview_frame_cache();
    let mut guard = match cache.lock() {
        Ok(g) => g,
        Err(_) => return,
    };

    if let Some(index) = guard.iter().position(|entry| {
        entry.path == path
            && entry.fingerprint == fingerprint
            && entry.source_color == source_color
            && entry.width == width
            && entry.height == height
            && entry.pts == pts
    }) {
        guard.remove(index);
    }

    guard.push_front(PreviewFrameCacheEntry {
        path: path.to_path_buf(),
        fingerprint,
        source_color,
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
    source_time: TimelineTime,
    width: u32,
    height: u32,
    source_color: PreviewSourceColorContract,
    source_format: ffmpeg::util::format::pixel::Pixel,
    decoded_color_space: ffmpeg::util::color::Space,
    decoded_color_range: ffmpeg::util::color::Range,
    should_cancel: &(dyn Fn() -> bool + Send + Sync),
) -> Option<Result<Option<RgbaFrame>>> {
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

    let color_contract = match resolve_cpu_rgba_contract_from_metadata(
        source_format,
        decoded_color_space,
        decoded_color_range,
        source_color,
        path,
    ) {
        Ok(contract) => contract,
        Err(error) => return Some(Err(error)),
    };
    let matrix_name = match color_contract.applied_matrix {
        DecodedVideoMatrix::Bt709 => "bt709",
        DecodedVideoMatrix::Bt2020NonConstant => "bt2020",
        DecodedVideoMatrix::Fcc => "fcc",
        DecodedVideoMatrix::Bt470Bg => "bt470bg",
        DecodedVideoMatrix::Smpte170M => "smpte170m",
        DecodedVideoMatrix::Smpte240M => "smpte240m",
        DecodedVideoMatrix::Rgb => return None,
        DecodedVideoMatrix::Unknown | DecodedVideoMatrix::Unsupported => return None,
    };
    let range_name = match color_contract.applied_range {
        DecodedVideoRange::Limited => "tv",
        DecodedVideoRange::Full => "pc",
        DecodedVideoRange::Unknown => return None,
    };
    let scale_filter = format!(
        "scale={width}:{height}:flags=fast_bilinear:in_color_matrix={matrix_name}:out_color_matrix={matrix_name}:in_range={range_name}:out_range=pc"
    );

    let source_time_arg = match ffmpeg_source_time_arg(source_time) {
        Ok(value) => value,
        Err(reason) => {
            return Some(Err(MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason,
            }));
        }
    };
    let mut command = Command::new("ffmpeg");
    command
        .arg("-v")
        .arg("error")
        .arg("-hwaccel")
        .arg(hwaccel)
        .arg("-ss")
        .arg(source_time_arg)
        .arg("-i")
        .arg(path)
        .arg("-frames:v")
        .arg("1")
        .arg("-vf")
        .arg(scale_filter)
        .arg("-pix_fmt")
        .arg("rgba")
        .arg("-f")
        .arg("rawvideo")
        .arg("pipe:1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = match run_external_decode_command_cancellable(&mut command, should_cancel) {
        Ok(Some(output)) => output,
        Ok(None) => return Some(Ok(None)),
        Err(_) => return None,
    };

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

    Some(Ok(Some(RgbaFrame::new(
        width,
        height,
        output.stdout.into_iter().take(expected).collect(),
        color_contract,
        PreviewDecodePath::ExternalFfmpegCpuRgba,
    ))))
}

fn run_external_decode_command_cancellable(
    command: &mut Command,
    should_cancel: &(dyn Fn() -> bool + Send + Sync),
) -> io::Result<Option<Output>> {
    let mut child = command.spawn()?;
    let Some(stdout) = child.stdout.take() else {
        terminate_external_decode_child(&mut child);
        return Err(io::Error::other("external decoder stdout was not piped"));
    };
    let Some(stderr) = child.stderr.take() else {
        terminate_external_decode_child(&mut child);
        return Err(io::Error::other("external decoder stderr was not piped"));
    };
    let stdout_reader = thread::spawn(move || read_external_decode_pipe(stdout));
    let stderr_reader = thread::spawn(move || read_external_decode_pipe(stderr));

    let status = loop {
        if should_cancel() {
            terminate_external_decode_child(&mut child);
            let _ = join_external_decode_reader(stdout_reader);
            let _ = join_external_decode_reader(stderr_reader);
            return Ok(None);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                terminate_external_decode_child(&mut child);
                let _ = join_external_decode_reader(stdout_reader);
                let _ = join_external_decode_reader(stderr_reader);
                return Err(error);
            }
        }
        thread::sleep(Duration::from_millis(1));
    };

    let stdout = join_external_decode_reader(stdout_reader)?;
    let stderr = join_external_decode_reader(stderr_reader)?;
    Ok(Some(Output { status, stdout, stderr }))
}

fn terminate_external_decode_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn read_external_decode_pipe(mut pipe: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn join_external_decode_reader(
    reader: thread::JoinHandle<io::Result<Vec<u8>>>,
) -> io::Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| io::Error::other("external decoder pipe reader panicked"))?
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

fn decoded_video_sampling_from_frame(
    frame: &ffmpeg::util::frame::video::Video,
) -> DecodedVideoSampling {
    let surface_format = decoded_surface_format_from_pixel(frame.format());
    decoded_video_sampling_from_frame_and_surface(frame, surface_format)
}

fn decoded_video_sampling_from_frame_and_surface(
    frame: &ffmpeg::util::frame::video::Video,
    surface_format: DecodedVideoSurfaceFormat,
) -> DecodedVideoSampling {
    DecodedVideoSampling {
        matrix: match decoded_video_matrix_from_ffmpeg(frame.color_space()) {
            Ok(Some(matrix)) => matrix,
            Ok(None) => DecodedVideoMatrix::Unknown,
            Err(_) => DecodedVideoMatrix::Unsupported,
        },
        range: decoded_video_range_from_ffmpeg(frame.color_range()),
        chroma_location: decoded_chroma_location_from_ffmpeg(frame.chroma_location()),
        bit_depth: surface_format.fixed_bit_depth().unwrap_or(0),
    }
}

fn decoded_chroma_location_from_ffmpeg(
    location: ffmpeg::util::chroma::Location,
) -> DecodedVideoChromaLocation {
    match location {
        ffmpeg::util::chroma::Location::Left => DecodedVideoChromaLocation::Left,
        ffmpeg::util::chroma::Location::Center => DecodedVideoChromaLocation::Center,
        ffmpeg::util::chroma::Location::TopLeft => DecodedVideoChromaLocation::TopLeft,
        ffmpeg::util::chroma::Location::Top => DecodedVideoChromaLocation::Top,
        ffmpeg::util::chroma::Location::BottomLeft => DecodedVideoChromaLocation::BottomLeft,
        ffmpeg::util::chroma::Location::Bottom => DecodedVideoChromaLocation::Bottom,
        ffmpeg::util::chroma::Location::Unspecified => DecodedVideoChromaLocation::Unknown,
    }
}

fn pixel_format_is_rgb(pixel: ffmpeg::util::format::pixel::Pixel) -> bool {
    pixel.descriptor().is_some_and(|descriptor| unsafe {
        ((*descriptor.as_ptr()).flags & ffmpeg::ffi::AV_PIX_FMT_FLAG_RGB as u64) != 0
    })
}

fn decoded_video_matrix_from_ffmpeg(
    space: ffmpeg::util::color::Space,
) -> std::result::Result<Option<DecodedVideoMatrix>, String> {
    let matrix = match space {
        ffmpeg::util::color::Space::RGB => Some(DecodedVideoMatrix::Rgb),
        ffmpeg::util::color::Space::BT709 => Some(DecodedVideoMatrix::Bt709),
        ffmpeg::util::color::Space::FCC => Some(DecodedVideoMatrix::Fcc),
        ffmpeg::util::color::Space::BT470BG => Some(DecodedVideoMatrix::Bt470Bg),
        ffmpeg::util::color::Space::SMPTE170M => Some(DecodedVideoMatrix::Smpte170M),
        ffmpeg::util::color::Space::SMPTE240M => Some(DecodedVideoMatrix::Smpte240M),
        ffmpeg::util::color::Space::BT2020NCL => Some(DecodedVideoMatrix::Bt2020NonConstant),
        ffmpeg::util::color::Space::Unspecified => None,
        unsupported => {
            return Err(format!(
                "unsupported FFmpeg YUV matrix {unsupported:?}; constant-luminance and derived matrices require a dedicated conversion"
            ));
        }
    };
    Ok(matrix)
}

fn source_video_matrix(source: PreviewSourceColorContract) -> Option<DecodedVideoMatrix> {
    match source.color_space.encoding().matrix {
        ColorMatrixCoefficients::Bt709 => Some(DecodedVideoMatrix::Bt709),
        ColorMatrixCoefficients::Fcc => Some(DecodedVideoMatrix::Fcc),
        ColorMatrixCoefficients::Bt470Bg => Some(DecodedVideoMatrix::Bt470Bg),
        ColorMatrixCoefficients::Smpte170M => Some(DecodedVideoMatrix::Smpte170M),
        ColorMatrixCoefficients::Smpte240M => Some(DecodedVideoMatrix::Smpte240M),
        ColorMatrixCoefficients::Bt2020NonConstant => Some(DecodedVideoMatrix::Bt2020NonConstant),
        ColorMatrixCoefficients::Rgb => Some(DecodedVideoMatrix::Rgb),
        ColorMatrixCoefficients::Unspecified => None,
    }
}

fn resolve_cpu_rgba_contract(
    decoded: &ffmpeg::util::frame::video::Video,
    source: PreviewSourceColorContract,
    path: &Path,
) -> Result<DecodedRgbaFrameContract> {
    resolve_cpu_rgba_contract_from_metadata(
        decoded.format(),
        decoded.color_space(),
        decoded.color_range(),
        source,
        path,
    )
}

fn resolve_cpu_rgba_contract_from_metadata(
    pixel_format: ffmpeg::util::format::pixel::Pixel,
    decoded_color_space: ffmpeg::util::color::Space,
    decoded_color_range: ffmpeg::util::color::Range,
    source: PreviewSourceColorContract,
    path: &Path,
) -> Result<DecodedRgbaFrameContract> {
    if pixel_format_is_rgb(pixel_format) {
        return Ok(DecodedRgbaFrameContract::source_encoded(
            source,
            DecodedVideoMatrix::Rgb,
            DecodedVideoRange::Full,
        ));
    }

    let decoded_matrix =
        decoded_video_matrix_from_ffmpeg(decoded_color_space).map_err(|reason| {
            MondrianError::DecodeFailed { asset_id: path.display().to_string(), reason }
        })?;
    let expected_matrix =
        source_video_matrix(source).filter(|matrix| *matrix != DecodedVideoMatrix::Rgb);
    // Matrix coefficients describe how the encoded YCbCr samples become RGB;
    // the resolved source color space describes how that RGB is interpreted by
    // the color pipeline. CICP permits those facts to differ, so an explicit
    // decoder matrix remains authoritative and the source contract is only a
    // fallback when the frame omits matrix metadata.
    let matrix = decoded_matrix.or(expected_matrix).ok_or_else(|| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: format!(
            "YUV matrix is unspecified for resolved source color space {:?}; refusing implicit swscale defaults",
            source.color_space
        ),
    })?;
    // Preserve the distinction between automatic probe fallback and an
    // explicit Interpret Footage override. Auto accepts a more local frame
    // fact; an authored override remains authoritative over incorrect tags.
    let range = source
        .range
        .resolve_for_frame(decoded_video_range_from_ffmpeg(decoded_color_range));
    if range == DecodedVideoRange::Unknown {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: "YUV quantization range is unspecified; refusing implicit swscale defaults"
                .to_owned(),
        });
    }
    Ok(DecodedRgbaFrameContract::source_encoded(
        source, matrix, range,
    ))
}

fn configure_preview_rgba_scaler(
    scaler: &mut ffmpeg::software::scaling::Context,
    contract: DecodedRgbaFrameContract,
    path: &Path,
) -> Result<()> {
    let coefficient_id = match contract.applied_matrix {
        DecodedVideoMatrix::Bt709 => Some(ffmpeg::ffi::SWS_CS_ITU709),
        DecodedVideoMatrix::Bt2020NonConstant => Some(ffmpeg::ffi::SWS_CS_BT2020),
        DecodedVideoMatrix::Fcc => Some(ffmpeg::ffi::SWS_CS_FCC),
        DecodedVideoMatrix::Bt470Bg | DecodedVideoMatrix::Smpte170M => {
            Some(ffmpeg::ffi::SWS_CS_ITU601)
        }
        DecodedVideoMatrix::Smpte240M => Some(ffmpeg::ffi::SWS_CS_SMPTE240M),
        DecodedVideoMatrix::Rgb => None,
        DecodedVideoMatrix::Unknown | DecodedVideoMatrix::Unsupported => {
            return Err(MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: format!(
                    "resolved CPU color contract contains invalid matrix {:?}",
                    contract.applied_matrix
                ),
            });
        }
    };
    let Some(coefficient_id) = coefficient_id else {
        return Ok(());
    };
    let source_full_range = i32::from(contract.applied_range == DecodedVideoRange::Full);
    let result = unsafe {
        let coefficients = ffmpeg::ffi::sws_getCoefficients(coefficient_id);
        ffmpeg::ffi::sws_setColorspaceDetails(
            scaler.as_mut_ptr(),
            coefficients,
            source_full_range,
            coefficients,
            1,
            0,
            1 << 16,
            1 << 16,
        )
    };
    if result < 0 {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "failed to configure swscale matrix {:?} and range {:?}: {}",
                contract.applied_matrix,
                contract.applied_range,
                ffmpeg::Error::from(result)
            ),
        });
    }
    Ok(())
}

fn source_time_to_stream_pts(
    source_time: TimelineTime,
    stream_tb: ffmpeg::Rational,
    stream_start_pts: i64,
) -> std::result::Result<i64, String> {
    let relative = source_time_to_time_base_ticks(
        source_time,
        i64::from(stream_tb.numerator()),
        i64::from(stream_tb.denominator()),
    )?;
    stream_start_pts.checked_add(relative).ok_or_else(|| {
        format!(
            "source target PTS overflow: time={source_time} stream_time_base={}/{} start_pts={stream_start_pts}",
            stream_tb.numerator(),
            stream_tb.denominator()
        )
    })
}

fn source_time_to_time_base_ticks(
    source_time: TimelineTime,
    time_base_num: i64,
    time_base_den: i64,
) -> std::result::Result<i64, String> {
    if source_time.is_negative() {
        return Err(format!(
            "negative media source target is invalid: {source_time}"
        ));
    }
    if time_base_num <= 0 || time_base_den <= 0 {
        return Err(format!(
            "invalid FFmpeg time base {time_base_num}/{time_base_den} for source target {source_time}"
        ));
    }
    let scaled_numerator = i128::from(source_time.numerator())
        .checked_mul(i128::from(time_base_den))
        .ok_or_else(|| format!("source target numerator overflow: {source_time}"))?;
    let scaled_denominator = i128::from(source_time.denominator())
        .checked_mul(i128::from(time_base_num))
        .ok_or_else(|| format!("source target denominator overflow: {source_time}"))?;
    let quotient = scaled_numerator / scaled_denominator;
    let remainder = scaled_numerator % scaled_denominator;
    let doubled_remainder = remainder
        .checked_mul(2)
        .ok_or_else(|| format!("source target rounding overflow: {source_time}"))?;
    let rounded = if doubled_remainder >= scaled_denominator {
        quotient
            .checked_add(1)
            .ok_or_else(|| format!("source target rounding overflow: {source_time}"))?
    } else {
        quotient
    };
    i64::try_from(rounded).map_err(|_| format!("source target exceeds FFmpeg PTS: {source_time}"))
}

fn ffmpeg_source_time_arg(source_time: TimelineTime) -> std::result::Result<String, String> {
    let micros =
        source_time_to_time_base_ticks(source_time, 1, i64::from(ffmpeg::ffi::AV_TIME_BASE))?;
    let seconds = micros / i64::from(ffmpeg::ffi::AV_TIME_BASE);
    let fractional = micros % i64::from(ffmpeg::ffi::AV_TIME_BASE);
    Ok(format!("{seconds}.{fractional:06}"))
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
    source_color: PreviewSourceColorContract,
) -> Result<RgbaFrame> {
    let decoded_surface_format = decoded_surface_format_from_pixel(decoded.format());
    let decoded_video_sampling = decoded_video_sampling_from_frame(decoded);
    let color_contract = resolve_cpu_rgba_contract(decoded, source_color, path)?;
    configure_preview_rgba_scaler(scaler, color_contract, path)?;
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
        color_contract,
        PreviewDecodePath::InProcessFfmpegCpuRgba,
    )
    .with_decoded_surface_format(decoded_surface_format)
    .with_decoded_video_sampling(decoded_video_sampling)
    .with_stage_durations(PreviewDecodeStageDurations {
        swscale_us,
        rgba_copy_us,
        ..PreviewDecodeStageDurations::default()
    }))
}

fn convert_decoded_to_float_rgba(
    decoded: &ffmpeg::util::frame::video::Video,
    target_width: u32,
    target_height: u32,
    path: &Path,
    source_color: PreviewSourceColorContract,
) -> Result<FloatRgbaFrame> {
    if !source_color.color_space.is_scene_linear() {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "float preview materialization requires a scene-linear source identity, got {:?}",
                source_color.color_space
            ),
        });
    }

    let decoded_surface_format = decoded_surface_format_from_pixel(decoded.format());
    let decoded_video_sampling = decoded_video_sampling_from_frame(decoded);
    let copy_started_at = Instant::now();
    let rgba = unpack_ffmpeg_planar_float_rgba(decoded, path)?;
    let rgba = resize_float_rgba(
        &rgba,
        decoded.width(),
        decoded.height(),
        target_width,
        target_height,
    );
    let rgba_copy_us = duration_us(copy_started_at.elapsed());

    Ok(FloatRgbaFrame::new(
        target_width,
        target_height,
        rgba,
        DecodedRgbaFrameContract::source_linear(source_color),
        PreviewDecodePath::InProcessFfmpegCpuFloat,
    )
    .with_decoded_surface_format(decoded_surface_format)
    .with_decoded_video_sampling(decoded_video_sampling)
    .with_stage_durations(PreviewDecodeStageDurations {
        rgba_copy_us,
        ..PreviewDecodeStageDurations::default()
    }))
}

fn unpack_ffmpeg_planar_float_rgba(
    decoded: &ffmpeg::util::frame::video::Video,
    path: &Path,
) -> Result<Vec<f32>> {
    let (little_endian, has_alpha) = match decoded.format() {
        ffmpeg::util::format::pixel::Pixel::GBRPF32LE => (true, false),
        ffmpeg::util::format::pixel::Pixel::GBRPF32BE => (false, false),
        ffmpeg::util::format::pixel::Pixel::GBRAPF32LE => (true, true),
        ffmpeg::util::format::pixel::Pixel::GBRAPF32BE => (false, true),
        format => {
            return Err(MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: format!(
                    "scene-linear source decoded to unsupported non-planar-f32 format {format:?}; refusing RGBA8 quantization"
                ),
            });
        }
    };

    let width = decoded.width() as usize;
    let height = decoded.height() as usize;
    let mut rgba = vec![0.0_f32; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let pixel = (y * width + x) * 4;
            for (channel, plane) in [(0, 2), (1, 0), (2, 1)] {
                rgba[pixel + channel] =
                    read_ffmpeg_f32_plane_sample(decoded, plane, x, y, little_endian, path)?;
            }
            rgba[pixel + 3] = if has_alpha {
                read_ffmpeg_f32_plane_sample(decoded, 3, x, y, little_endian, path)?
            } else {
                1.0
            };
        }
    }
    Ok(rgba)
}

fn read_ffmpeg_f32_plane_sample(
    decoded: &ffmpeg::util::frame::video::Video,
    plane: usize,
    x: usize,
    y: usize,
    little_endian: bool,
    path: &Path,
) -> Result<f32> {
    let offset = y * decoded.stride(plane) + x * std::mem::size_of::<f32>();
    let end = offset + std::mem::size_of::<f32>();
    let bytes: [u8; 4] = decoded
        .data(plane)
        .get(offset..end)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "FFmpeg float plane {plane} row {y} is shorter than the declared stride"
            ),
        })?;
    Ok(if little_endian {
        f32::from_le_bytes(bytes)
    } else {
        f32::from_be_bytes(bytes)
    })
}

fn resize_float_rgba(
    source: &[f32],
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> Vec<f32> {
    if source_width == target_width && source_height == target_height {
        return source.to_vec();
    }

    let source_width = source_width as usize;
    let source_height = source_height as usize;
    let target_width = target_width as usize;
    let target_height = target_height as usize;
    let scale_x = source_width as f32 / target_width as f32;
    let scale_y = source_height as f32 / target_height as f32;
    let mut output = vec![0.0_f32; target_width * target_height * 4];
    output.par_chunks_mut(target_width * 4).enumerate().for_each(|(target_y, row)| {
        let source_y = ((target_y as f32 + 0.5) * scale_y - 0.5)
            .clamp(0.0, source_height.saturating_sub(1) as f32);
        let y0 = source_y.floor() as usize;
        let y1 = (y0 + 1).min(source_height.saturating_sub(1));
        let fy = source_y - y0 as f32;
        for target_x in 0..target_width {
            let source_x = ((target_x as f32 + 0.5) * scale_x - 0.5)
                .clamp(0.0, source_width.saturating_sub(1) as f32);
            let x0 = source_x.floor() as usize;
            let x1 = (x0 + 1).min(source_width.saturating_sub(1));
            let fx = source_x - x0 as f32;
            for channel in 0..4 {
                let top_left = source[(y0 * source_width + x0) * 4 + channel];
                let top_right = source[(y0 * source_width + x1) * 4 + channel];
                let bottom_left = source[(y1 * source_width + x0) * 4 + channel];
                let bottom_right = source[(y1 * source_width + x1) * 4 + channel];
                let top = top_left + (top_right - top_left) * fx;
                let bottom = bottom_left + (bottom_right - bottom_left) * fx;
                row[target_x * 4 + channel] = top + (bottom - top) * fy;
            }
        }
    });
    output
}

#[derive(Debug, thiserror::Error)]
enum PreviewNativeFrameMaterializationError {
    #[error("FFmpeg hardware pixel format {pixel_format:?} has no native media adapter")]
    UnsupportedHardwarePixelFormat {
        pixel_format: ffmpeg::util::format::pixel::Pixel,
    },
    #[error("FFmpeg hardware frame is missing AVFrame::hw_frames_ctx")]
    MissingHardwareFramesContext,
    #[error("FFmpeg hardware frame has an empty AVHWFramesContext payload")]
    MissingHardwareFramesContextData,
    #[error(
        "FFmpeg hardware frame software layout {software_format:?} is not explicitly NV12/P010"
    )]
    UnsupportedHardwareSurfaceFormat {
        software_format: ffmpeg::ffi::AVPixelFormat,
    },
    #[error(transparent)]
    Resource(#[from] FfmpegNativeDecodedFrameResourceError),
    #[error(transparent)]
    Payload(#[from] PreviewNativeDecodedFrameError),
}

impl PreviewNativeFrameMaterializationError {
    fn fallback_reason(&self) -> PreviewNativeDecodeFallback {
        match self {
            Self::UnsupportedHardwarePixelFormat { .. } => {
                PreviewNativeDecodeFallback::ResourceAdapterUnavailable
            }
            Self::MissingHardwareFramesContext
            | Self::MissingHardwareFramesContextData
            | Self::UnsupportedHardwareSurfaceFormat { .. } => {
                PreviewNativeDecodeFallback::SurfaceFormatUnavailable
            }
            Self::Payload(_) => PreviewNativeDecodeFallback::SamplingMetadataIncomplete,
            Self::Resource(_) => PreviewNativeDecodeFallback::ResourceRetentionFailed,
        }
    }
}

fn materialize_decoded_frame(
    decoded: &ffmpeg::util::frame::video::Video,
    hardware_decode_plan: &mut PreviewHardwareDecodePlan,
    scaler: &mut Option<ffmpeg::software::scaling::Context>,
    scaler_source_format: &mut Option<ffmpeg::util::format::pixel::Pixel>,
    target_width: u32,
    target_height: u32,
    path: &Path,
    source_color: PreviewSourceColorContract,
) -> Result<PreviewDecodedFramePayload> {
    if hardware_decode_plan.request.prefers_gpu_residency() {
        if preview_hardware_frame_format(decoded.format()) {
            match materialize_native_decoded_frame(decoded, source_color) {
                Ok(frame) => {
                    hardware_decode_plan.mark_gpu_resident_native_observed(frame.handle_kind());
                    return Ok(PreviewDecodedFramePayload::NativeGpu(frame));
                }
                Err(error) if hardware_decode_plan.request.requires_gpu_residency() => {
                    return Err(MondrianError::DecodeFailed {
                        asset_id: path.display().to_string(),
                        reason: format!(
                            "required GPU-resident decoded frame could not be materialized: {error}"
                        ),
                    });
                }
                Err(error) => {
                    hardware_decode_plan.mark_native_decode_fallback(error.fallback_reason());
                    preview_trace(format!(
                        "[preview] native FFmpeg frame materialization failed, fallback CPU transfer: {error}"
                    ));
                }
            }
        } else if hardware_decode_plan.request.requires_gpu_residency() {
            return Err(MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: format!(
                    "required GPU-resident decode returned software frame {:?}",
                    decoded.format()
                ),
            });
        } else {
            hardware_decode_plan
                .mark_native_decode_fallback(PreviewNativeDecodeFallback::SoftwareFrame);
        }
    }

    materialize_decoded_to_cpu(
        decoded,
        hardware_decode_plan,
        scaler,
        scaler_source_format,
        target_width,
        target_height,
        path,
        source_color,
    )
}

fn materialize_native_decoded_frame(
    decoded: &ffmpeg::util::frame::video::Video,
    source_color: PreviewSourceColorContract,
) -> std::result::Result<PreviewNativeDecodedFrame, PreviewNativeFrameMaterializationError> {
    use ffmpeg::util::format::pixel::Pixel;

    if !matches!(decoded.format(), Pixel::D3D12 | Pixel::D3D11) {
        return Err(
            PreviewNativeFrameMaterializationError::UnsupportedHardwarePixelFormat {
                pixel_format: decoded.format(),
            },
        );
    }
    let surface_format = decoded_native_surface_format(decoded)?;
    // Hardware AVFrames expose a hardware pixel format (for example D3D11),
    // while `surface_format` is the retained texture's software layout. Use
    // the latter for coded depth instead of re-inferring it from AVFrame::format.
    let mut sampling = decoded_video_sampling_from_frame_and_surface(decoded, surface_format);
    // Native D3D12/D3D11 frames bypass swscale, so apply the same resolved range
    // contract consumed by the CPU conversion path before renderer import.
    sampling.range = source_color.range.resolve_for_frame(sampling.range);
    let resource = FfmpegNativeDecodedFrameResource::retain(decoded)?;
    match decoded.format() {
        Pixel::D3D12 => {
            resource.d3d12_texture()?;
        }
        Pixel::D3D11 => {
            resource.d3d11_texture()?;
        }
        _ => unreachable!("native frame pixel format was validated above"),
    }
    let handle = PreviewNativeDecodedFrameHandle::new(resource);
    Ok(PreviewNativeDecodedFrame::new(
        decoded.width(),
        decoded.height(),
        handle,
        surface_format,
        sampling,
        PreviewDecodeDiagnostics::new(PreviewDecodePath::InProcessFfmpegNative),
    )?)
}

fn decoded_native_surface_format(
    decoded: &ffmpeg::util::frame::video::Video,
) -> std::result::Result<DecodedVideoSurfaceFormat, PreviewNativeFrameMaterializationError> {
    // SAFETY: decoded owns the AVFrame for this borrow. hw_frames_ctx, when
    // present, is an AVBufferRef whose data points to an AVHWFramesContext.
    let hardware_frames_context = unsafe { (*decoded.as_ptr()).hw_frames_ctx };
    if hardware_frames_context.is_null() {
        return Err(PreviewNativeFrameMaterializationError::MissingHardwareFramesContext);
    }
    // SAFETY: hardware_frames_context is non-null and owned by decoded.
    let context_data = unsafe { (*hardware_frames_context).data };
    let context = NonNull::new(context_data.cast::<ffmpeg::ffi::AVHWFramesContext>())
        .ok_or(PreviewNativeFrameMaterializationError::MissingHardwareFramesContextData)?;
    // SAFETY: FFmpeg defines AVBufferRef::data as AVHWFramesContext for
    // AVFrame::hw_frames_ctx and the frame retains that buffer for this borrow.
    let software_format = unsafe { context.as_ref().sw_format };
    decoded_native_surface_format_from_software_format(software_format)
}

fn decoded_native_surface_format_from_software_format(
    software_format: ffmpeg::ffi::AVPixelFormat,
) -> std::result::Result<DecodedVideoSurfaceFormat, PreviewNativeFrameMaterializationError> {
    match software_format {
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NV12 => Ok(DecodedVideoSurfaceFormat::Nv12),
        ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_P010LE => Ok(DecodedVideoSurfaceFormat::P010),
        _ => Err(
            PreviewNativeFrameMaterializationError::UnsupportedHardwareSurfaceFormat {
                software_format,
            },
        ),
    }
}

fn materialize_decoded_to_cpu(
    decoded: &ffmpeg::util::frame::video::Video,
    hardware_decode_plan: &mut PreviewHardwareDecodePlan,
    scaler: &mut Option<ffmpeg::software::scaling::Context>,
    scaler_source_format: &mut Option<ffmpeg::util::format::pixel::Pixel>,
    target_width: u32,
    target_height: u32,
    path: &Path,
    source_color: PreviewSourceColorContract,
) -> Result<PreviewDecodedFramePayload> {
    if !preview_hardware_frame_format(decoded.format()) {
        if source_color.color_space.is_scene_linear() {
            return convert_decoded_to_float_rgba(
                decoded,
                target_width,
                target_height,
                path,
                source_color,
            )
            .map(PreviewDecodedFramePayload::CpuFloat);
        }
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
        return convert_decoded_to_rgba(decoded, scaler, path, source_color)
            .map(PreviewDecodedFramePayload::CpuRgba);
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
    if source_color.color_space.is_scene_linear() {
        return convert_decoded_to_float_rgba(
            &transferred,
            target_width,
            target_height,
            path,
            source_color,
        )
        .map(|frame| {
            PreviewDecodedFramePayload::CpuFloat(frame.with_stage_durations(
                PreviewDecodeStageDurations {
                    hardware_transfer_us,
                    ..PreviewDecodeStageDurations::default()
                },
            ))
        });
    }
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
    convert_decoded_to_rgba(&transferred, scaler, path, source_color).map(|frame| {
        PreviewDecodedFramePayload::CpuRgba(frame.with_stage_durations(
            PreviewDecodeStageDurations {
                hardware_transfer_us,
                ..PreviewDecodeStageDurations::default()
            },
        ))
    })
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
        apply_preview_codec_threading_policy, clear_global_preview_frame_cache,
        clear_thread_local_preview_decode_session, convert_decoded_to_rgba,
        decode_preview_frame_cancellable, decoded_native_surface_format_from_software_format,
        decoded_surface_format_from_pixel, decoded_video_sampling_from_frame, duration_us,
        materialize_decoded_frame, preview_cache_get, preview_cache_put_with_fingerprint,
        preview_create_rgba_scaler, preview_decode_interrupt_callback,
        preview_external_ffmpeg_cpu_rgba_allowed_for_access_mode, preview_hardware_extra_frames,
        preview_seek_index_cache_get, preview_seek_index_cache_put, resolve_cpu_rgba_contract,
        run_external_decode_command_cancellable, temporal_selection_is_approximate,
        DecodedRgbaFrameContract, FfmpegAvD3D12VaFrame, FfmpegAvD3D12VaSyncContext,
        FfmpegNativeDecodedFrameResource, FfmpegNativeDecodedFrameResourceError,
        MediaFileFingerprint, PreviewDecodeAccessMode, PreviewDecodeAccessPolicy,
        PreviewDecodeAdaptiveHints, PreviewDecodeBackend, PreviewDecodeCancellation,
        PreviewDecodeCancellationCheckpoint, PreviewDecodeDiagnostics, PreviewDecodeExecutionPath,
        PreviewDecodeInterruptState, PreviewDecodeOutcome, PreviewDecodePath, PreviewDecodeRequest,
        PreviewDecodeSeekStrategy, PreviewDecodeStageDurations, PreviewDecodeThreadingConfig,
        PreviewDecodeThreadingKind, PreviewDecodedFramePayload, PreviewHardwareDecodeBlocker,
        PreviewHardwareDecodeCpuTransferStatus, PreviewHardwareDecodeDecision,
        PreviewHardwareDecodePlan, PreviewHardwareDecodeRequest, PreviewNativeDecodeFallback,
        PreviewNativeDecodedFrame, PreviewNativeDecodedFrameError, PreviewNativeDecodedFrameHandle,
        PreviewNativeDecodedFrameResource, PreviewPlaybackRing, PreviewScrubAdaptiveClass,
        PreviewSeekIndex, PreviewSeekIndexDiagnostics, PreviewSeekIndexSource,
        PreviewSeekResolution, PreviewSourceColorContract, RgbaFrame,
        PREVIEW_EXACT_FORWARD_DECODE_BUDGET_FRAMES, PREVIEW_NATIVE_DECODE_EXTRA_HW_FRAMES,
        PREVIEW_PLAYBACK_FORWARD_REUSE_FRAMES, PREVIEW_SCRUB_ANY_SEEK_WINDOW_MS,
        PREVIEW_SCRUB_FORWARD_DECODE_BUDGET_FRAMES, PREVIEW_SCRUB_FORWARD_REUSE_FRAMES,
        PREVIEW_SCRUB_HOT_ANY_SEEK_WINDOW_MS, PREVIEW_SCRUB_HOT_FORWARD_DECODE_BUDGET_FRAMES,
        PREVIEW_SCRUB_UNINDEXED_FORWARD_DECODE_BUDGET_FRAMES,
    };
    use crate::decoder::{
        DecodedFrameResidency, DecodedGpuFrameHandleKind, DecodedVideoChromaLocation,
        DecodedVideoMatrix, DecodedVideoRange, DecodedVideoSampling, DecodedVideoSurfaceFormat,
        HwAccelBackend,
    };
    use ffmpeg_next as ffmpeg;
    use mondrian_core::types::ColorSpace;
    use mondrian_core::TimelineTime;
    use serde::Serialize;
    use std::any::Any;
    use std::ffi::c_void;
    use std::net::TcpListener;
    use std::num::NonZeroU64;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn gpu_resident_decode_reserves_external_hardware_frame_leases() {
        assert_eq!(
            preview_hardware_extra_frames(PreviewHardwareDecodeRequest::PreferGpuResident),
            PREVIEW_NATIVE_DECODE_EXTRA_HW_FRAMES
        );
        assert_eq!(
            preview_hardware_extra_frames(PreviewHardwareDecodeRequest::RequireGpuResident),
            PREVIEW_NATIVE_DECODE_EXTRA_HW_FRAMES
        );
        assert_eq!(
            preview_hardware_extra_frames(PreviewHardwareDecodeRequest::PreferHardwareDecode),
            0
        );
        assert_eq!(
            preview_hardware_extra_frames(PreviewHardwareDecodeRequest::Auto),
            0
        );
    }

    #[test]
    fn exact_decode_accepts_container_pts_quantization_within_frame_tolerance() {
        let policy =
            PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::PlaybackCursor);

        assert!(!temporal_selection_is_approximate(
            667,
            Some(672),
            336,
            policy
        ));
        assert!(!temporal_selection_is_approximate(
            667,
            Some(667),
            336,
            policy
        ));
        assert!(!temporal_selection_is_approximate(667, None, 336, policy));
        assert!(temporal_selection_is_approximate(
            667,
            Some(1_004),
            336,
            policy
        ));
    }

    #[test]
    fn keyframe_only_decode_reports_any_non_exact_temporal_selection() {
        let policy =
            PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::ScrubCursor);

        assert!(policy.keyframe_only);
        assert!(temporal_selection_is_approximate(
            667,
            Some(672),
            336,
            policy
        ));
        assert!(!temporal_selection_is_approximate(
            667,
            Some(667),
            336,
            policy
        ));
    }

    fn test_source_color() -> PreviewSourceColorContract {
        PreviewSourceColorContract::automatic(ColorSpace::Rec709, DecodedVideoRange::Limited)
    }

    fn test_linear_source_color() -> PreviewSourceColorContract {
        PreviewSourceColorContract::automatic(ColorSpace::Aces2065_1, DecodedVideoRange::Full)
    }

    fn test_rgba_contract() -> DecodedRgbaFrameContract {
        DecodedRgbaFrameContract::source_encoded(
            test_source_color(),
            DecodedVideoMatrix::Bt709,
            DecodedVideoRange::Limited,
        )
    }

    #[derive(Debug)]
    struct TestNativeDecodedFrameResource {
        kind: DecodedGpuFrameHandleKind,
        id: NonZeroU64,
        drops: Option<Arc<AtomicUsize>>,
    }

    impl PreviewNativeDecodedFrameResource for TestNativeDecodedFrameResource {
        fn handle_kind(&self) -> DecodedGpuFrameHandleKind {
            self.kind
        }

        fn handle_id(&self) -> NonZeroU64 {
            self.id
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    impl Drop for TestNativeDecodedFrameResource {
        fn drop(&mut self) {
            if let Some(drops) = &self.drops {
                drops.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    fn test_native_handle(
        kind: DecodedGpuFrameHandleKind,
        id: u64,
    ) -> PreviewNativeDecodedFrameHandle {
        PreviewNativeDecodedFrameHandle::new(TestNativeDecodedFrameResource {
            kind,
            id: NonZeroU64::new(id).expect("test native handle id must be non-zero"),
            drops: None,
        })
    }

    fn synthetic_d3d11_frame(
        software_format: ffmpeg::ffi::AVPixelFormat,
    ) -> ffmpeg::util::frame::video::Video {
        let mut frame = ffmpeg::util::frame::video::Video::empty();
        frame.set_format(ffmpeg::util::format::pixel::Pixel::D3D11);
        frame.set_width(1920);
        frame.set_height(1080);
        frame.set_color_range(ffmpeg::util::color::Range::MPEG);
        // SAFETY: Every AVBufferRef assigned to the synthetic frame is owned by
        // the frame and released by its normal drop. The hardware-context bytes
        // are initialized before being read, and native pointers are never
        // dereferenced by these media contract tests.
        unsafe {
            let raw = frame.as_mut_ptr();
            let surface = ffmpeg::ffi::av_buffer_alloc(1);
            assert!(
                !surface.is_null(),
                "test surface buffer allocation must succeed"
            );
            (*raw).buf[0] = surface;
            (*raw).data[0] = std::ptr::NonNull::<u8>::dangling().as_ptr();
            (*raw).data[1] = 2usize as *mut u8;

            let context =
                ffmpeg::ffi::av_buffer_alloc(std::mem::size_of::<ffmpeg::ffi::AVHWFramesContext>());
            assert!(
                !context.is_null(),
                "test hardware context allocation must succeed"
            );
            std::ptr::write_bytes(
                (*context).data,
                0,
                std::mem::size_of::<ffmpeg::ffi::AVHWFramesContext>(),
            );
            (*((*context).data.cast::<ffmpeg::ffi::AVHWFramesContext>())).sw_format =
                software_format;
            (*raw).hw_frames_ctx = context;
            (*raw).chroma_location = ffmpeg::util::chroma::Location::Left.into();
        }
        frame
    }

    fn synthetic_d3d12_frame(
        software_format: ffmpeg::ffi::AVPixelFormat,
    ) -> ffmpeg::util::frame::video::Video {
        let mut frame = ffmpeg::util::frame::video::Video::empty();
        frame.set_format(ffmpeg::util::format::pixel::Pixel::D3D12);
        frame.set_width(3840);
        frame.set_height(2160);
        frame.set_color_range(ffmpeg::util::color::Range::MPEG);
        // SAFETY: Both AVBufferRefs are owned by the frame. data[0] points into
        // buf[0], so cloning the AVFrame also retains the synthetic descriptor.
        // The fake COM pointers are only checked for non-null and never called.
        unsafe {
            let raw = frame.as_mut_ptr();
            let descriptor =
                ffmpeg::ffi::av_buffer_alloc(std::mem::size_of::<FfmpegAvD3D12VaFrame>());
            assert!(
                !descriptor.is_null(),
                "test D3D12 descriptor allocation must succeed"
            );
            let native = (*descriptor).data.cast::<FfmpegAvD3D12VaFrame>();
            native.write(FfmpegAvD3D12VaFrame {
                texture: std::ptr::NonNull::<u8>::dangling().as_ptr().cast::<c_void>(),
                sync_ctx: FfmpegAvD3D12VaSyncContext {
                    fence: std::ptr::NonNull::<u16>::dangling().as_ptr().cast::<c_void>(),
                    event: std::ptr::null_mut(),
                    fence_value: 9,
                },
            });
            (*raw).buf[0] = descriptor;
            (*raw).data[0] = native.cast::<u8>();

            let context =
                ffmpeg::ffi::av_buffer_alloc(std::mem::size_of::<ffmpeg::ffi::AVHWFramesContext>());
            assert!(
                !context.is_null(),
                "test hardware context allocation must succeed"
            );
            std::ptr::write_bytes(
                (*context).data,
                0,
                std::mem::size_of::<ffmpeg::ffi::AVHWFramesContext>(),
            );
            (*((*context).data.cast::<ffmpeg::ffi::AVHWFramesContext>())).sw_format =
                software_format;
            (*raw).hw_frames_ctx = context;
            (*raw).chroma_location = ffmpeg::util::chroma::Location::Left.into();
        }
        frame
    }

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
            TimelineTime::ZERO,
            PreviewDecodeAccessMode::PlaybackCursor,
            test_source_color(),
        );

        assert_eq!(
            request.hardware_decode_request,
            PreviewHardwareDecodeRequest::Auto
        );
        assert_eq!(
            request
                .with_hardware_decode_request(PreviewHardwareDecodeRequest::PreferHardwareDecode)
                .hardware_decode_request,
            PreviewHardwareDecodeRequest::PreferHardwareDecode
        );
        assert_eq!(
            request
                .with_hardware_decode_request(PreviewHardwareDecodeRequest::PreferGpuResident)
                .hardware_decode_request,
            PreviewHardwareDecodeRequest::PreferGpuResident
        );
    }

    #[test]
    fn exact_source_time_lowers_to_stream_pts_with_start_offset() {
        assert_eq!(
            super::source_time_to_stream_pts(
                TimelineTime::new(1, 3).expect("exact source time"),
                ffmpeg::Rational(1, 90_000),
                9_000,
            )
            .expect("valid stream target"),
            39_000
        );
    }

    #[test]
    fn exact_source_time_rounds_half_ticks_away_from_zero() {
        assert_eq!(
            super::source_time_to_stream_pts(
                TimelineTime::new(1, 2).expect("exact source time"),
                ffmpeg::Rational(1, 1),
                0,
            )
            .expect("valid stream target"),
            1
        );
    }

    #[test]
    fn exact_source_time_preserves_long_duration_without_float_drift() {
        assert_eq!(
            super::source_time_to_stream_pts(
                TimelineTime::new(360_000, 1).expect("exact source time"),
                ffmpeg::Rational(1, 90_000),
                0,
            )
            .expect("valid stream target"),
            32_400_000_000
        );
    }

    #[test]
    fn exact_source_time_rejects_negative_targets_and_invalid_time_bases() {
        assert!(super::source_time_to_stream_pts(
            TimelineTime::new(-1, 1).expect("exact source time"),
            ffmpeg::Rational(1, 90_000),
            0,
        )
        .is_err());
        assert!(
            super::source_time_to_stream_pts(TimelineTime::ZERO, ffmpeg::Rational(0, 1), 0,)
                .is_err()
        );
    }

    #[test]
    fn external_ffmpeg_argument_is_lowered_only_at_the_cli_adapter() {
        assert_eq!(
            super::ffmpeg_source_time_arg(TimelineTime::new(5, 4).expect("exact source time"))
                .expect("valid CLI target"),
            "1.250000"
        );
    }

    #[test]
    fn hardware_decode_plan_does_not_report_native_before_frame_is_observed() {
        let plan = PreviewHardwareDecodePlan::resolve(
            PreviewHardwareDecodeRequest::PreferGpuResident,
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewDecodeBackend::Auto,
            ffmpeg::codec::Id::H264,
            None,
        );

        assert_eq!(
            plan.request,
            PreviewHardwareDecodeRequest::PreferGpuResident
        );
        assert_ne!(
            plan.decision,
            PreviewHardwareDecodeDecision::GpuResidentNative
        );
        assert_eq!(plan.probe.frame_residency, DecodedFrameResidency::CpuRgba);
        assert!(!plan.probe.hardware_decode_active);
        assert!(!plan.probe.zero_copy_active);
        assert_eq!(
            plan.probe.decoder_adapter_available,
            matches!(
                plan.ffmpeg_codec_config.hw_pixel_format,
                Some(
                    crate::decoder::HwAccelPixelFormat::D3D12
                        | crate::decoder::HwAccelPixelFormat::D3D11
                )
            )
        );
    }

    #[test]
    fn hardware_decode_plan_can_prefer_cpu_transfer_without_requiring_native_residency() {
        let plan = PreviewHardwareDecodePlan::resolve(
            PreviewHardwareDecodeRequest::PreferHardwareDecode,
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewDecodeBackend::Auto,
            ffmpeg::codec::Id::H264,
            None,
        );

        assert_eq!(
            plan.request,
            PreviewHardwareDecodeRequest::PreferHardwareDecode
        );
        if plan.ffmpeg_codec_config.ffmpeg_codec_config_available {
            assert_eq!(
                plan.ffmpeg_device_context.device_create_attempted,
                plan.ffmpeg_device_context.ffmpeg_device_type_available
            );
        }
    }

    #[test]
    fn hardware_decode_plan_configures_required_gpu_without_cpu_transfer_fallback() {
        let plan = PreviewHardwareDecodePlan::resolve(
            PreviewHardwareDecodeRequest::RequireGpuResident,
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewDecodeBackend::Auto,
            ffmpeg::codec::Id::H264,
            None,
        );

        assert_eq!(
            plan.request,
            PreviewHardwareDecodeRequest::RequireGpuResident
        );
        assert!(!plan.allows_cpu_transfer_fallback());
        if plan.ffmpeg_codec_config.ffmpeg_codec_config_available {
            assert_eq!(
                plan.ffmpeg_device_context.device_create_attempted,
                plan.ffmpeg_device_context.ffmpeg_device_type_available
            );
        }
    }

    #[test]
    fn hardware_decode_plan_supports_interactive_access_modes() {
        for access_mode in [
            PreviewDecodeAccessMode::ScrubCursor,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ] {
            assert!(PreviewHardwareDecodePlan::plan_requires_device_context(
                PreviewHardwareDecodeRequest::PreferGpuResident,
                access_mode,
                PreviewDecodeBackend::Auto,
            ));
        }
    }

    #[test]
    fn hardware_decode_plan_marks_external_cpu_rgba_backend_boundary() {
        let plan = PreviewHardwareDecodePlan::resolve(
            PreviewHardwareDecodeRequest::PreferGpuResident,
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewDecodeBackend::ExternalFfmpegCpuRgba,
            ffmpeg::codec::Id::H264,
            None,
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
            None,
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
            None,
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
            None,
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
    fn exr_decode_policy_disables_frame_thread_shutdown_deadlock() {
        let requested =
            PreviewDecodeThreadingConfig { kind: PreviewDecodeThreadingKind::Frame, count: 8 };
        assert_eq!(
            apply_preview_codec_threading_policy(ffmpeg::codec::Id::EXR, requested),
            PreviewDecodeThreadingConfig { kind: PreviewDecodeThreadingKind::None, count: 1 }
        );
        assert_eq!(
            apply_preview_codec_threading_policy(ffmpeg::codec::Id::H264, requested),
            requested
        );
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
        assert!(PreviewDecodeAccessMode::ScrubCursor.preserves_session_on_cancel());
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
        let fingerprint = MediaFileFingerprint {
            len: Some(10),
            modified_secs: Some(20),
            modified_nanos: Some(30),
        };
        let request = PreviewDecodeRequest::new(
            path.as_path(),
            TimelineTime::new(5, 4).expect("exact source time"),
            PreviewDecodeAccessMode::ScrubCursor,
            test_source_color(),
        )
        .with_max_size(Some(640), Some(360))
        .with_fingerprint(fingerprint)
        .with_adaptive_hints(PreviewDecodeAdaptiveHints {
            scrub_class: PreviewScrubAdaptiveClass::HotRegion,
        });

        assert_eq!(request.path, path.as_path());
        assert_eq!(
            request.source_time,
            TimelineTime::new(5, 4).expect("exact source time")
        );
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
        assert!(!playback.keyframe_only);
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
        assert!(scrub.preserve_session_on_cancel);
        assert!(scrub.keyframe_only);
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
        assert!(!still.keyframe_only);
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
    fn only_scrub_accepts_first_decoded_frame_inside_evidenced_gop_radius() {
        let scrub =
            PreviewDecodeAccessPolicy::for_access_mode(PreviewDecodeAccessMode::ScrubCursor);
        let still = PreviewDecodeAccessPolicy::for_access_mode(
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        );

        assert!(scrub.accepts_first_decoded_approximation(1_900, 2_000, 100));
        assert!(!scrub.accepts_first_decoded_approximation(1_899, 2_000, 100));
        assert!(!still.accepts_first_decoded_approximation(1_900, 2_000, 100));
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
            PREVIEW_SCRUB_UNINDEXED_FORWARD_DECODE_BUDGET_FRAMES
        );

        let near_next_keyframe_scrub = scrub.adapt_for_request(
            &probe_index,
            290,
            frame_duration_pts,
            PreviewDecodeAdaptiveHints::default(),
        );
        assert_eq!(
            near_next_keyframe_scrub.forward_decode_budget_frames,
            PREVIEW_SCRUB_UNINDEXED_FORWARD_DECODE_BUDGET_FRAMES
        );

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
        assert_eq!(
            slow.seek_strategy,
            PreviewDecodeSeekStrategy::BoundedAnyFrame
        );
        assert!(slow.any_seek_window_ms > 0);
        assert!(slow.forward_decode_budget_frames > 0);

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
        assert_eq!(index.nearest_keyframe(60), Some(50));
        assert_eq!(index.nearest_keyframe(90), Some(100));
        assert_eq!(index.nearest_keyframe(150), Some(100));
        assert_eq!(index.adjacent_keyframe_radius(50), Some(50));
        assert_eq!(index.adjacent_keyframe_radius(100), Some(100));
        assert_eq!(index.adjacent_keyframe_radius(150), Some(50));
        assert_eq!(index.adjacent_keyframe_radius(200), Some(100));
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
        let fingerprint = MediaFileFingerprint {
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
        let frame = RgbaFrame::new(
            1,
            1,
            vec![0; 4],
            test_rgba_contract(),
            PreviewDecodePath::InProcessFfmpegCpuRgba,
        )
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
        let mut frame = RgbaFrame::new(
            1,
            1,
            vec![0; 4],
            test_rgba_contract(),
            PreviewDecodePath::InProcessFfmpegCpuRgba,
        );
        frame.diagnostics = frame.diagnostics.with_hw_accel_probe(&probe);

        assert_eq!(frame.diagnostics.hw_accel_backend, HwAccelBackend::None);
        assert!(!frame.diagnostics.hardware_decode_active);
        assert!(!frame.diagnostics.zero_copy_active);
        assert_eq!(
            frame.diagnostics.decoded_frame_residency,
            DecodedFrameResidency::CpuRgba
        );
        assert_eq!(frame.diagnostics.gpu_frame_handle_kind, None);
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
    fn decoded_video_sampling_reads_frame_range_chroma_and_bit_depth() {
        let mut frame = ffmpeg::util::frame::video::Video::new(
            ffmpeg::util::format::pixel::Pixel::P010LE,
            16,
            16,
        );
        frame.set_color_range(ffmpeg::util::color::Range::MPEG);
        frame.set_color_space(ffmpeg::util::color::Space::SMPTE170M);
        unsafe {
            (*frame.as_mut_ptr()).chroma_location = ffmpeg::util::chroma::Location::Left.into();
        }

        assert_eq!(
            decoded_video_sampling_from_frame(&frame),
            DecodedVideoSampling {
                matrix: DecodedVideoMatrix::Smpte170M,
                range: DecodedVideoRange::Limited,
                chroma_location: DecodedVideoChromaLocation::Left,
                bit_depth: 10,
            }
        );
    }

    #[test]
    fn decoded_video_sampling_distinguishes_missing_and_unsupported_matrices() {
        let mut frame = ffmpeg::util::frame::video::Video::new(
            ffmpeg::util::format::pixel::Pixel::P010LE,
            16,
            16,
        );
        assert_eq!(
            decoded_video_sampling_from_frame(&frame).matrix,
            DecodedVideoMatrix::Unknown
        );

        frame.set_color_space(ffmpeg::util::color::Space::BT2020CL);
        assert_eq!(
            decoded_video_sampling_from_frame(&frame).matrix,
            DecodedVideoMatrix::Unsupported
        );
    }

    #[test]
    fn cpu_rgba_contract_uses_explicit_bt2020_matrix_and_limited_range() {
        let mut frame = ffmpeg::util::frame::video::Video::new(
            ffmpeg::util::format::pixel::Pixel::YUV420P10LE,
            16,
            16,
        );
        frame.set_color_space(ffmpeg::util::color::Space::BT2020NCL);
        frame.set_color_range(ffmpeg::util::color::Range::MPEG);

        let contract = resolve_cpu_rgba_contract(
            &frame,
            PreviewSourceColorContract::automatic(
                ColorSpace::Rec2100Pq,
                DecodedVideoRange::Limited,
            ),
            Path::new("bt2020-pq.mov"),
        )
        .expect("BT.2020 NCL must resolve exactly");

        assert_eq!(
            contract.applied_matrix,
            DecodedVideoMatrix::Bt2020NonConstant
        );
        assert_eq!(contract.applied_range, DecodedVideoRange::Limited);
        assert_eq!(contract.source.color_space, ColorSpace::Rec2100Pq);
    }

    #[test]
    fn cpu_rgba_contract_falls_back_only_to_explicit_source_facts() {
        let frame = ffmpeg::util::frame::video::Video::new(
            ffmpeg::util::format::pixel::Pixel::YUV420P,
            16,
            16,
        );

        let contract = resolve_cpu_rgba_contract(
            &frame,
            PreviewSourceColorContract::automatic(ColorSpace::Rec709, DecodedVideoRange::Limited),
            Path::new("untagged-rec709.mov"),
        )
        .expect("explicit app contract must resolve missing frame tags");

        assert_eq!(contract.applied_matrix, DecodedVideoMatrix::Bt709);
        assert_eq!(contract.applied_range, DecodedVideoRange::Limited);

        let mut tagged_yuv = frame;
        tagged_yuv.set_color_space(ffmpeg::util::color::Space::BT709);
        let srgb_contract = resolve_cpu_rgba_contract(
            &tagged_yuv,
            PreviewSourceColorContract::automatic(ColorSpace::Srgb, DecodedVideoRange::Limited),
            Path::new("srgb-transfer-yuv.mov"),
        )
        .expect("decoded YUV matrix remains authoritative for an RGB-defined source space");
        assert_eq!(srgb_contract.applied_matrix, DecodedVideoMatrix::Bt709);
    }

    #[test]
    fn cpu_rgba_contract_honors_resolved_source_range_over_frame_tag() {
        let mut frame = ffmpeg::util::frame::video::Video::new(
            ffmpeg::util::format::pixel::Pixel::YUV420P,
            16,
            16,
        );
        frame.set_color_space(ffmpeg::util::color::Space::BT709);
        frame.set_color_range(ffmpeg::util::color::Range::MPEG);

        let contract = resolve_cpu_rgba_contract(
            &frame,
            PreviewSourceColorContract::from_interpretation(
                ColorSpace::Rec709,
                mondrian_core::timeline_data::MediaRangeInterpretation::Override {
                    range: mondrian_core::timeline_data::MediaSignalRange::Full,
                },
                DecodedVideoRange::Limited,
            ),
            Path::new("incorrect-limited-tag.mov"),
        )
        .expect("resolved app contract must override an incorrect frame range tag");

        assert_eq!(contract.applied_range, DecodedVideoRange::Full);
    }

    #[test]
    fn cpu_rgba_contract_auto_prefers_frame_range_over_probe_fallback() {
        let mut frame = ffmpeg::util::frame::video::Video::new(
            ffmpeg::util::format::pixel::Pixel::YUV420P,
            16,
            16,
        );
        frame.set_color_space(ffmpeg::util::color::Space::BT709);
        frame.set_color_range(ffmpeg::util::color::Range::JPEG);

        let contract = resolve_cpu_rgba_contract(
            &frame,
            PreviewSourceColorContract::automatic(ColorSpace::Rec709, DecodedVideoRange::Limited),
            Path::new("frame-full-probe-limited.mov"),
        )
        .expect("automatic range must prefer the decoded frame fact");

        assert_eq!(contract.applied_range, DecodedVideoRange::Full);
    }

    #[test]
    fn cpu_rgba_contract_keeps_explicit_matrix_and_rejects_constant_luminance() {
        let mut conflict = ffmpeg::util::frame::video::Video::new(
            ffmpeg::util::format::pixel::Pixel::YUV420P,
            16,
            16,
        );
        conflict.set_color_space(ffmpeg::util::color::Space::BT709);
        conflict.set_color_range(ffmpeg::util::color::Range::MPEG);
        let contract = resolve_cpu_rgba_contract(
            &conflict,
            PreviewSourceColorContract::automatic(ColorSpace::Rec2020, DecodedVideoRange::Limited),
            Path::new("conflict.mov"),
        )
        .expect("explicit decoded matrix remains authoritative");
        assert_eq!(contract.applied_matrix, DecodedVideoMatrix::Bt709);

        conflict.set_color_space(ffmpeg::util::color::Space::BT2020CL);
        let error = resolve_cpu_rgba_contract(
            &conflict,
            PreviewSourceColorContract::automatic(ColorSpace::Rec2020, DecodedVideoRange::Limited),
            Path::new("bt2020-cl.mov"),
        )
        .expect_err("constant-luminance BT.2020 needs a dedicated conversion");
        assert!(error.to_string().contains("unsupported FFmpeg YUV matrix"));
    }

    #[test]
    fn swscale_expands_bt709_limited_range_to_full_rgba() {
        fn decoded_yuv420(y: u8) -> ffmpeg::util::frame::video::Video {
            let mut frame = ffmpeg::util::frame::video::Video::new(
                ffmpeg::util::format::pixel::Pixel::YUV420P,
                4,
                4,
            );
            frame.set_color_space(ffmpeg::util::color::Space::BT709);
            frame.set_color_range(ffmpeg::util::color::Range::MPEG);
            frame.data_mut(0).fill(y);
            frame.data_mut(1).fill(128);
            frame.data_mut(2).fill(128);
            frame
        }

        let path = Path::new("limited-rec709.mov");
        let mut scaler = preview_create_rgba_scaler(
            ffmpeg::util::format::pixel::Pixel::YUV420P,
            4,
            4,
            4,
            4,
            path,
        )
        .expect("create scaler");
        let black =
            convert_decoded_to_rgba(&decoded_yuv420(16), &mut scaler, path, test_source_color())
                .expect("convert limited black");
        let white =
            convert_decoded_to_rgba(&decoded_yuv420(235), &mut scaler, path, test_source_color())
                .expect("convert limited white");

        assert!(black.rgba()[..3].iter().all(|channel| *channel <= 2));
        assert!(white.rgba()[..3].iter().all(|channel| *channel >= 253));
        assert_eq!(black.rgba()[3], 255);
        assert_eq!(white.rgba()[3], 255);
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
        let frame = RgbaFrame::new(
            2,
            1,
            vec![0; 8],
            test_rgba_contract(),
            PreviewDecodePath::InProcessFfmpegCpuRgba,
        )
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
    fn hardware_execution_provenance_survives_cache_and_playback_ring_reuse() {
        let mut frame = RgbaFrame::new(
            2,
            1,
            vec![0; 8],
            test_rgba_contract(),
            PreviewDecodePath::InProcessFfmpegCpuRgba,
        );
        frame.diagnostics.hardware_decode_active = true;
        frame.diagnostics.hardware_decode_cpu_transfer_observed = true;
        frame.diagnostics.hw_accel_backend = HwAccelBackend::D3D11VA;
        frame.diagnostics.decoded_surface_format = DecodedVideoSurfaceFormat::P010;
        frame.diagnostics.decoded_video_sampling.bit_depth = 10;
        let frame = frame.with_decode_execution();
        let expected = PreviewDecodeExecutionPath::HardwareCpuTransfer {
            backend: HwAccelBackend::D3D11VA,
            surface: DecodedVideoSurfaceFormat::P010,
            sampling: frame.diagnostics.decoded_video_sampling,
        };
        assert_eq!(frame.decode_execution, expected);

        let cached = frame.into_cache_hit(
            std::time::Duration::from_micros(3),
            PreviewDecodeAccessMode::PlaybackCursor,
        );
        assert_eq!(cached.decode_execution, expected);
        assert!(!cached.diagnostics.hardware_decode_cpu_transfer_observed);

        let ring_hit = cached.into_playback_ring_hit(std::time::Duration::from_micros(2));
        assert_eq!(ring_hit.decode_execution, expected);
        assert!(!ring_hit.diagnostics.hardware_decode_cpu_transfer_observed);
    }

    #[test]
    fn playback_session_ring_uses_strict_tolerance_and_lru_capacity() {
        let mut ring = PreviewPlaybackRing::new(2);
        let frame_a = RgbaFrame::new(
            1,
            1,
            vec![1, 2, 3, 4],
            test_rgba_contract(),
            PreviewDecodePath::InProcessFfmpegCpuRgba,
        );
        let frame_b = RgbaFrame::new(
            1,
            1,
            vec![5, 6, 7, 8],
            test_rgba_contract(),
            PreviewDecodePath::InProcessFfmpegCpuRgba,
        );
        let frame_c = RgbaFrame::new(
            1,
            1,
            vec![9, 10, 11, 12],
            test_rgba_contract(),
            PreviewDecodePath::InProcessFfmpegCpuRgba,
        );

        ring.put(100, frame_a.clone());
        ring.put(110, frame_b.clone());

        let PreviewDecodedFramePayload::CpuRgba(hit) = ring.get(103, 3).expect("within tolerance")
        else {
            panic!("RGBA ring entry changed payload kind");
        };
        assert_eq!(hit.rgba(), frame_a.rgba());
        assert!(ring.get(104, 3).is_none());

        ring.put(120, frame_c.clone());

        assert!(ring.get(110, 1).is_none());
        let PreviewDecodedFramePayload::CpuRgba(recent) = ring.get(100, 1).expect("recently used")
        else {
            panic!("recent RGBA ring entry changed payload kind");
        };
        assert_eq!(recent.rgba(), frame_a.rgba());
        let PreviewDecodedFramePayload::CpuRgba(newest) = ring.get(120, 1).expect("newest") else {
            panic!("newest RGBA ring entry changed payload kind");
        };
        assert_eq!(newest.rgba(), frame_c.rgba());
    }

    #[test]
    fn rgba_frame_clone_shares_pixel_payload() {
        let frame = RgbaFrame::new(
            2,
            1,
            vec![0, 64, 128, 255, 255, 128, 64, 32],
            test_rgba_contract(),
            PreviewDecodePath::InProcessFfmpegCpuRgba,
        );
        let cloned = frame.clone();

        assert!(std::sync::Arc::ptr_eq(&frame.data, &cloned.data));
        assert_eq!(cloned.rgba(), frame.rgba());
        assert_eq!(frame.into_data(), vec![0, 64, 128, 255, 255, 128, 64, 32]);
    }

    #[test]
    fn native_decoded_frame_payload_forces_gpu_residency_diagnostics() {
        let handle = test_native_handle(DecodedGpuFrameHandleKind::D3D11Texture2D, 7);
        let frame = PreviewNativeDecodedFrame::new(
            1920,
            1080,
            handle.clone(),
            DecodedVideoSurfaceFormat::P010,
            p010_native_sampling(),
            PreviewDecodeDiagnostics::new(PreviewDecodePath::InProcessFfmpegCpuRgba),
        )
        .expect("valid native frame");

        assert_eq!(frame.width, 1920);
        assert_eq!(frame.height, 1080);
        assert_eq!(frame.handle, handle);
        assert_eq!(
            frame.handle_kind(),
            DecodedGpuFrameHandleKind::D3D11Texture2D
        );
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
        assert_eq!(
            frame.diagnostics.decoded_video_sampling,
            p010_native_sampling()
        );
    }

    #[test]
    fn native_decoded_frame_handle_retains_resource_until_last_clone_drops() {
        let drops = Arc::new(AtomicUsize::new(0));
        let handle = PreviewNativeDecodedFrameHandle::new(TestNativeDecodedFrameResource {
            kind: DecodedGpuFrameHandleKind::D3D11Texture2D,
            id: NonZeroU64::new(17).expect("non-zero test id"),
            drops: Some(Arc::clone(&drops)),
        });
        let cloned = handle.clone();
        let separate_same_id =
            PreviewNativeDecodedFrameHandle::new(TestNativeDecodedFrameResource {
                kind: DecodedGpuFrameHandleKind::D3D11Texture2D,
                id: NonZeroU64::new(17).expect("non-zero test id"),
                drops: None,
            });

        assert_eq!(handle.kind(), DecodedGpuFrameHandleKind::D3D11Texture2D);
        assert_eq!(handle.id().get(), 17);
        assert_eq!(handle, cloned);
        assert_ne!(handle, separate_same_id);
        assert!(handle.resource::<TestNativeDecodedFrameResource>().is_some());

        drop(handle);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(cloned);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn ffmpeg_native_resource_retains_d3d11_surface_buffer_and_abi_view() {
        let mut frame = ffmpeg::util::frame::video::Video::empty();
        frame.set_format(ffmpeg::util::format::pixel::Pixel::D3D11);
        frame.set_width(1920);
        frame.set_height(1080);
        let texture = std::ptr::NonNull::<u8>::dangling().as_ptr();
        // SAFETY: The test frame owns the AVBufferRef assigned to buf[0]. The
        // synthetic data pointers are never dereferenced; they only exercise
        // FFmpeg's documented D3D11 texture-plus-slice metadata ABI.
        let source_buffer = unsafe {
            let raw = frame.as_mut_ptr();
            let buffer = ffmpeg::ffi::av_buffer_alloc(1);
            assert!(
                !buffer.is_null(),
                "test AVBufferRef allocation must succeed"
            );
            (*raw).buf[0] = buffer;
            (*raw).data[0] = texture;
            (*raw).data[1] = 3usize as *mut u8;
            buffer
        };
        // SAFETY: source_buffer remains owned by frame.
        assert_eq!(
            unsafe { ffmpeg::ffi::av_buffer_get_ref_count(source_buffer) },
            1
        );

        let resource = FfmpegNativeDecodedFrameResource::retain(&frame)
            .expect("D3D11 frame with a ref-counted surface must be retained");
        // SAFETY: source_buffer remains owned by frame and the retained resource.
        assert_eq!(
            unsafe { ffmpeg::ffi::av_buffer_get_ref_count(source_buffer) },
            2
        );
        let handle = PreviewNativeDecodedFrameHandle::new(resource);
        drop(frame);

        let retained = handle
            .resource::<FfmpegNativeDecodedFrameResource>()
            .expect("native handle must preserve its concrete FFmpeg resource");
        // SAFETY: retained owns the cloned AVFrame and its buf[0] reference.
        let retained_buffer = unsafe { (*retained.frame.as_ptr()).buf[0] };
        assert!(!retained_buffer.is_null());
        // SAFETY: retained_buffer remains owned by retained.
        assert_eq!(
            unsafe { ffmpeg::ffi::av_buffer_get_ref_count(retained_buffer) },
            1
        );
        assert_eq!(
            retained.pixel_format(),
            ffmpeg::util::format::pixel::Pixel::D3D11
        );
        let view = retained
            .d3d11_texture()
            .expect("preferred D3D11 frame must expose its texture ABI view");
        assert_eq!(view.texture_ptr(), texture.cast());
        assert_eq!(view.array_slice(), 3);
    }

    #[test]
    fn ffmpeg_native_resource_retains_d3d12_resource_and_fence_abi() {
        let mut frame = ffmpeg::util::frame::video::Video::empty();
        frame.set_format(ffmpeg::util::format::pixel::Pixel::D3D12);
        frame.set_width(3840);
        frame.set_height(2160);
        let texture = std::ptr::NonNull::<u8>::dangling().as_ptr().cast::<c_void>();
        let fence = std::ptr::NonNull::<u16>::dangling().as_ptr().cast::<c_void>();
        let native = Box::new(FfmpegAvD3D12VaFrame {
            texture,
            sync_ctx: FfmpegAvD3D12VaSyncContext {
                fence,
                event: std::ptr::null_mut(),
                fence_value: 42,
            },
        });
        // SAFETY: the synthetic native descriptor remains alive until after
        // every parsed view is consumed. The AVBufferRef only exercises the
        // retained AVFrame ownership path and no COM pointer is dereferenced.
        let source_buffer = unsafe {
            let raw = frame.as_mut_ptr();
            let buffer = ffmpeg::ffi::av_buffer_alloc(1);
            assert!(
                !buffer.is_null(),
                "test AVBufferRef allocation must succeed"
            );
            (*raw).buf[0] = buffer;
            (*raw).data[0] = (&*native as *const FfmpegAvD3D12VaFrame).cast_mut().cast();
            buffer
        };
        // SAFETY: source_buffer remains owned by frame.
        assert_eq!(
            unsafe { ffmpeg::ffi::av_buffer_get_ref_count(source_buffer) },
            1
        );

        let resource = FfmpegNativeDecodedFrameResource::retain(&frame)
            .expect("D3D12 frame with a ref-counted resource must be retained");
        // SAFETY: source_buffer remains owned by frame and resource.
        assert_eq!(
            unsafe { ffmpeg::ffi::av_buffer_get_ref_count(source_buffer) },
            2
        );
        let handle = PreviewNativeDecodedFrameHandle::new(resource);
        drop(frame);

        let retained = handle
            .resource::<FfmpegNativeDecodedFrameResource>()
            .expect("native handle must preserve the concrete FFmpeg resource");
        let view = retained.d3d12_texture().expect("D3D12 ABI view");
        assert_eq!(view.texture_ptr(), texture);
        assert_eq!(view.fence_ptr(), fence);
        assert_eq!(view.fence_value(), 42);
        drop(native);
    }

    #[test]
    fn ffmpeg_native_resource_rejects_software_frames() {
        let mut frame = ffmpeg::util::frame::video::Video::empty();
        frame.set_format(ffmpeg::util::format::pixel::Pixel::RGBA);

        let error = FfmpegNativeDecodedFrameResource::retain(&frame)
            .expect_err("software RGBA must not masquerade as a native decoder surface");
        assert_eq!(
            error,
            FfmpegNativeDecodedFrameResourceError::UnsupportedPixelFormat {
                pixel_format: ffmpeg::util::format::pixel::Pixel::RGBA,
            }
        );
    }

    #[test]
    fn ffmpeg_legacy_d3d11va_frame_does_not_use_preferred_texture_abi() {
        let mut frame = ffmpeg::util::frame::video::Video::empty();
        frame.set_format(ffmpeg::util::format::pixel::Pixel::D3D11VA_VLD);
        // SAFETY: The frame allocation is alive for this parse call.
        let raw = unsafe {
            std::ptr::NonNull::new(frame.as_mut_ptr()).expect("AVFrame allocation must succeed")
        };
        let error =
            super::parse_ffmpeg_d3d11_texture(raw, ffmpeg::util::format::pixel::Pixel::D3D11VA_VLD)
                .expect_err("legacy D3D11VA layout must fail the preferred D3D11 ABI contract");
        assert_eq!(
            error,
            FfmpegNativeDecodedFrameResourceError::NotPreferredD3D11Frame {
                pixel_format: ffmpeg::util::format::pixel::Pixel::D3D11VA_VLD,
            }
        );
    }

    #[test]
    fn native_surface_format_requires_explicit_nv12_or_p010_layout() {
        assert_eq!(
            decoded_native_surface_format_from_software_format(
                ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NV12,
            )
            .expect("NV12 hardware layout must be supported"),
            DecodedVideoSurfaceFormat::Nv12
        );
        assert_eq!(
            decoded_native_surface_format_from_software_format(
                ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_P010LE,
            )
            .expect("P010 hardware layout must be supported"),
            DecodedVideoSurfaceFormat::P010
        );
        assert!(matches!(
            decoded_native_surface_format_from_software_format(
                ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_YUV420P,
            ),
            Err(
                super::PreviewNativeFrameMaterializationError::UnsupportedHardwareSurfaceFormat {
                    software_format: ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_YUV420P,
                }
            )
        ));
    }

    #[test]
    fn gpu_preferred_software_frame_falls_back_with_structured_reason() {
        let decoded =
            ffmpeg::util::frame::video::Video::new(ffmpeg::util::format::pixel::Pixel::RGBA, 2, 2);
        let mut plan = PreviewHardwareDecodePlan::resolve(
            PreviewHardwareDecodeRequest::Auto,
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewDecodeBackend::Software,
            ffmpeg::codec::Id::H264,
            None,
        );
        plan.request = PreviewHardwareDecodeRequest::PreferGpuResident;
        let mut scaler = None;
        let mut scaler_source_format = None;
        let payload = materialize_decoded_frame(
            &decoded,
            &mut plan,
            &mut scaler,
            &mut scaler_source_format,
            2,
            2,
            Path::new("synthetic-rgba"),
            test_source_color(),
        )
        .expect("GPU preference may fall back to CPU RGBA with diagnostics");

        assert!(matches!(payload, PreviewDecodedFramePayload::CpuRgba(_)));
        assert_eq!(
            plan.native_decode_fallback,
            Some(PreviewNativeDecodeFallback::SoftwareFrame)
        );
    }

    #[test]
    fn scene_linear_ffmpeg_float_frame_preserves_extended_range_rgba() {
        let pixel_format = if cfg!(target_endian = "little") {
            ffmpeg::util::format::pixel::Pixel::GBRAPF32LE
        } else {
            ffmpeg::util::format::pixel::Pixel::GBRAPF32BE
        };
        let mut decoded = ffmpeg::util::frame::video::Video::new(pixel_format, 2, 1);
        let pixels = [[-0.25_f32, 0.18, 4.0, 0.5], [0.3, 0.4, 0.5, 1.0]];
        for (plane, channel) in [(0, 1), (1, 2), (2, 0), (3, 3)] {
            let stride = decoded.stride(plane);
            let data = decoded.data_mut(plane);
            for (x, pixel) in pixels.iter().enumerate() {
                let start = x * std::mem::size_of::<f32>();
                data[start..start + std::mem::size_of::<f32>()]
                    .copy_from_slice(&pixel[channel].to_ne_bytes());
            }
            assert!(stride >= 2 * std::mem::size_of::<f32>());
        }
        let mut plan = PreviewHardwareDecodePlan::resolve(
            PreviewHardwareDecodeRequest::Auto,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            PreviewDecodeBackend::Software,
            ffmpeg::codec::Id::EXR,
            None,
        );
        let payload = materialize_decoded_frame(
            &decoded,
            &mut plan,
            &mut None,
            &mut None,
            2,
            1,
            Path::new("synthetic-linear.exr"),
            test_linear_source_color(),
        )
        .expect("scene-linear float frame should materialize without RGBA8 quantization");

        let cached_payload = payload.clone();
        let PreviewDecodedFramePayload::CpuFloat(frame) = payload else {
            panic!("scene-linear FFmpeg float output must remain CPU float");
        };
        assert_eq!(frame.rgba(), pixels.as_flattened());
        assert_eq!(
            frame.diagnostics.decoded_frame_residency,
            DecodedFrameResidency::CpuFloat
        );

        clear_global_preview_frame_cache();
        let path = PathBuf::from("synthetic-linear.exr");
        let fingerprint = MediaFileFingerprint {
            len: Some(128),
            modified_secs: Some(1),
            modified_nanos: Some(2),
        };
        cached_payload.cache_cpu_frame(&path, fingerprint, 2, 1, 7);
        let hit = preview_cache_get(&path, fingerprint, test_linear_source_color(), 2, 1, 7, 1)
            .expect("float preview should enter the shared frame cache");
        let PreviewDecodedFramePayload::CpuFloat(cached) = hit.frame else {
            panic!("float preview cache changed payload kind");
        };
        assert_eq!(cached.rgba(), pixels.as_flattened());
        clear_global_preview_frame_cache();
    }

    #[test]
    fn float_preview_resize_preserves_extended_range() {
        let source = vec![
            -1.0, 0.0, 1.0, 1.0, 1.0, 2.0, 3.0, 1.0, 3.0, 4.0, 5.0, 1.0, 5.0, 6.0, 7.0, 1.0,
        ];
        let resized = super::resize_float_rgba(&source, 2, 2, 1, 1);
        assert_eq!(resized, vec![2.0, 3.0, 4.0, 1.0]);
    }

    #[test]
    fn gpu_required_software_frame_fails_closed() {
        let decoded =
            ffmpeg::util::frame::video::Video::new(ffmpeg::util::format::pixel::Pixel::RGBA, 2, 2);
        let mut plan = PreviewHardwareDecodePlan::resolve(
            PreviewHardwareDecodeRequest::Auto,
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewDecodeBackend::Software,
            ffmpeg::codec::Id::H264,
            None,
        );
        plan.request = PreviewHardwareDecodeRequest::RequireGpuResident;
        let error = materialize_decoded_frame(
            &decoded,
            &mut plan,
            &mut None,
            &mut None,
            2,
            2,
            Path::new("synthetic-rgba"),
            test_source_color(),
        )
        .expect_err("required GPU residency must not return a software frame");

        assert!(error.to_string().contains("required GPU-resident decode returned software"));
    }

    #[test]
    fn explicit_d3d11_nv12_frame_materializes_native_without_cpu_cache() {
        clear_global_preview_frame_cache();
        let decoded = synthetic_d3d11_frame(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NV12);
        let mut plan = PreviewHardwareDecodePlan::resolve(
            PreviewHardwareDecodeRequest::Auto,
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewDecodeBackend::Software,
            ffmpeg::codec::Id::H264,
            None,
        );
        plan.request = PreviewHardwareDecodeRequest::PreferGpuResident;
        let payload = materialize_decoded_frame(
            &decoded,
            &mut plan,
            &mut None,
            &mut None,
            960,
            540,
            Path::new("synthetic-d3d11"),
            test_source_color(),
        )
        .expect("explicit D3D11 NV12 frame must materialize as a native payload");

        let frame = match &payload {
            PreviewDecodedFramePayload::NativeGpu(frame) => frame,
            PreviewDecodedFramePayload::CpuRgba(_) | PreviewDecodedFramePayload::CpuFloat(_) => {
                panic!("explicit D3D11 NV12 frame must not transfer to CPU")
            }
        };
        assert_eq!(frame.width, 1920);
        assert_eq!(frame.height, 1080);
        assert_eq!(frame.surface_format, DecodedVideoSurfaceFormat::Nv12);
        assert_eq!(
            frame.diagnostics.path,
            PreviewDecodePath::InProcessFfmpegNative
        );
        assert_eq!(
            plan.decision,
            PreviewHardwareDecodeDecision::GpuResidentNative
        );
        assert_eq!(plan.native_decode_fallback, None);

        let path = PathBuf::from("synthetic-d3d11");
        let fingerprint = MediaFileFingerprint {
            len: None,
            modified_secs: None,
            modified_nanos: None,
        };
        payload.cache_cpu_frame(&path, fingerprint, 960, 540, 42);
        assert!(
            preview_cache_get(&path, fingerprint, test_source_color(), 960, 540, 42, 1).is_none()
        );
    }

    #[test]
    fn explicit_d3d12_p010_frame_materializes_native_with_decode_fence() {
        let decoded = synthetic_d3d12_frame(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_P010LE);
        let mut plan = PreviewHardwareDecodePlan::resolve(
            PreviewHardwareDecodeRequest::Auto,
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewDecodeBackend::Software,
            ffmpeg::codec::Id::HEVC,
            None,
        );
        plan.request = PreviewHardwareDecodeRequest::RequireGpuResident;
        let payload = materialize_decoded_frame(
            &decoded,
            &mut plan,
            &mut None,
            &mut None,
            1920,
            1080,
            Path::new("synthetic-d3d12"),
            PreviewSourceColorContract::automatic(
                ColorSpace::Rec2100Pq,
                DecodedVideoRange::Limited,
            ),
        )
        .expect("explicit D3D12 P010 frame must remain native");

        let PreviewDecodedFramePayload::NativeGpu(frame) = payload else {
            panic!("explicit D3D12 P010 frame must not transfer to CPU");
        };
        assert_eq!(frame.width, 3840);
        assert_eq!(frame.height, 2160);
        assert_eq!(frame.surface_format, DecodedVideoSurfaceFormat::P010);
        assert_eq!(
            frame.handle_kind(),
            DecodedGpuFrameHandleKind::D3D12Resource
        );
        let resource = frame
            .handle
            .resource::<FfmpegNativeDecodedFrameResource>()
            .expect("native frame must retain FFmpeg's D3D12 resource");
        let view = resource.d3d12_texture().expect("D3D12 ABI view");
        assert_eq!(view.fence_value(), 9);
        assert_eq!(
            plan.decision,
            PreviewHardwareDecodeDecision::GpuResidentNative
        );
        assert_eq!(plan.native_decode_fallback, None);
    }

    #[test]
    fn native_decoded_frame_payload_requires_real_handle_and_native_surface() {
        let handle = test_native_handle(DecodedGpuFrameHandleKind::D3D11Texture2D, 7);
        let empty = PreviewNativeDecodedFrame::new(
            0,
            1080,
            handle.clone(),
            DecodedVideoSurfaceFormat::P010,
            p010_native_sampling(),
            PreviewDecodeDiagnostics::new(PreviewDecodePath::InProcessFfmpegCpuRgba),
        )
        .expect_err("empty native payload extent must fail closed");
        assert_eq!(
            empty,
            PreviewNativeDecodedFrameError::EmptyExtent { width: 0, height: 1080 }
        );

        let unsupported = PreviewNativeDecodedFrame::new(
            1920,
            1080,
            handle,
            DecodedVideoSurfaceFormat::Yuv420p,
            DecodedVideoSampling {
                matrix: DecodedVideoMatrix::Bt709,
                range: DecodedVideoRange::Limited,
                chroma_location: DecodedVideoChromaLocation::Left,
                bit_depth: 8,
            },
            PreviewDecodeDiagnostics::new(PreviewDecodePath::InProcessFfmpegCpuRgba),
        )
        .expect_err("planar CPU surface must not masquerade as a native GPU payload");
        assert_eq!(
            unsupported,
            PreviewNativeDecodedFrameError::UnsupportedSurfaceFormat {
                surface_format: DecodedVideoSurfaceFormat::Yuv420p
            }
        );
    }

    #[test]
    fn native_decoded_frame_payload_requires_complete_video_sampling() {
        let handle = test_native_handle(DecodedGpuFrameHandleKind::D3D11Texture2D, 7);

        let missing_range = PreviewNativeDecodedFrame::new(
            1920,
            1080,
            handle.clone(),
            DecodedVideoSurfaceFormat::P010,
            DecodedVideoSampling {
                matrix: DecodedVideoMatrix::Bt2020NonConstant,
                range: DecodedVideoRange::Unknown,
                chroma_location: DecodedVideoChromaLocation::Left,
                bit_depth: 10,
            },
            PreviewDecodeDiagnostics::new(PreviewDecodePath::InProcessFfmpegCpuRgba),
        )
        .expect_err("native payload must not guess range");
        assert_eq!(
            missing_range,
            PreviewNativeDecodedFrameError::MissingVideoRange {
                surface_format: DecodedVideoSurfaceFormat::P010
            }
        );

        let missing_chroma = PreviewNativeDecodedFrame::new(
            1920,
            1080,
            handle.clone(),
            DecodedVideoSurfaceFormat::P010,
            DecodedVideoSampling {
                matrix: DecodedVideoMatrix::Bt2020NonConstant,
                range: DecodedVideoRange::Limited,
                chroma_location: DecodedVideoChromaLocation::Unknown,
                bit_depth: 10,
            },
            PreviewDecodeDiagnostics::new(PreviewDecodePath::InProcessFfmpegCpuRgba),
        )
        .expect_err("native YCbCr payload must not guess chroma siting");
        assert_eq!(
            missing_chroma,
            PreviewNativeDecodedFrameError::MissingVideoChromaLocation {
                surface_format: DecodedVideoSurfaceFormat::P010
            }
        );

        let bit_depth_mismatch = PreviewNativeDecodedFrame::new(
            1920,
            1080,
            handle,
            DecodedVideoSurfaceFormat::P010,
            DecodedVideoSampling {
                matrix: DecodedVideoMatrix::Bt2020NonConstant,
                range: DecodedVideoRange::Limited,
                chroma_location: DecodedVideoChromaLocation::Left,
                bit_depth: 8,
            },
            PreviewDecodeDiagnostics::new(PreviewDecodePath::InProcessFfmpegCpuRgba),
        )
        .expect_err("P010 native payload must require 10-bit sampling");
        assert_eq!(
            bit_depth_mismatch,
            PreviewNativeDecodedFrameError::BitDepthMismatch {
                surface_format: DecodedVideoSurfaceFormat::P010,
                expected: 10,
                actual: 8,
            }
        );
    }

    fn p010_native_sampling() -> DecodedVideoSampling {
        DecodedVideoSampling {
            matrix: DecodedVideoMatrix::Bt2020NonConstant,
            range: DecodedVideoRange::Limited,
            chroma_location: DecodedVideoChromaLocation::TopLeft,
            bit_depth: 10,
        }
    }

    #[test]
    fn cancellable_preview_decode_returns_canceled_before_opening_missing_file() {
        let path = PathBuf::from("E:/definitely-missing/canceled-preview.mov");
        let request = PreviewDecodeRequest::new(
            path.as_path(),
            TimelineTime::ZERO,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            test_source_color(),
        )
        .with_max_size(Some(320), Some(180));
        let outcome = decode_preview_frame_cancellable(request, || true)
            .expect("canceled decode should not fail missing media");

        assert_eq!(
            match outcome {
                PreviewDecodeOutcome::Canceled(cancellation) => cancellation,
                other => panic!("expected cancellation, got {other:?}"),
            },
            PreviewDecodeCancellation::cooperative(
                PreviewDecodeCancellationCheckpoint::BeforeInputOpen,
            )
        );
    }

    #[test]
    fn ffmpeg_input_open_interrupts_a_blocked_http_read() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind local stall server");
        let address = listener.local_addr().expect("stall server address");
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept FFmpeg HTTP connection");
            accepted_tx.send(()).expect("publish accepted connection");
            let _stream = stream;
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
        });

        let canceled = Arc::new(AtomicBool::new(false));
        let worker_canceled = Arc::clone(&canceled);
        let url = PathBuf::from(format!("http://{address}/blocked-open.mp4"));
        let (outcome_tx, outcome_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let request = PreviewDecodeRequest::new(
                url.as_path(),
                TimelineTime::ZERO,
                PreviewDecodeAccessMode::PlaybackCursor,
                test_source_color(),
            )
            .with_fingerprint(MediaFileFingerprint::default())
            .with_max_size(Some(320), Some(180));
            let outcome = decode_preview_frame_cancellable(request, move || {
                worker_canceled.load(Ordering::Acquire)
            });
            let _ = outcome_tx.send(outcome);
        });

        accepted_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("FFmpeg must enter the controlled blocking read");
        let requested_at = Instant::now();
        canceled.store(true, Ordering::Release);
        let outcome = outcome_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("FFmpeg interrupt callback must stop blocked input open")
            .expect("blocked input cancellation must not become a media failure");
        let return_latency = requested_at.elapsed();

        let _ = release_tx.send(());
        worker.join().expect("decode worker must return");
        server.join().expect("stall server must return");
        assert_eq!(
            match outcome {
                PreviewDecodeOutcome::Canceled(cancellation) => cancellation,
                other => panic!("expected cancellation, got {other:?}"),
            },
            PreviewDecodeCancellation::ffmpeg_interrupt(
                PreviewDecodeCancellationCheckpoint::InputOpen,
            )
        );
        assert!(
            return_latency <= Duration::from_millis(500),
            "blocked input open returned too late after cancellation: {return_latency:?}"
        );
    }

    #[test]
    fn format_interrupt_callback_uses_only_the_active_request_probe() {
        let state = Arc::new(PreviewDecodeInterruptState::new());
        let opaque = Arc::as_ptr(&state).cast_mut().cast::<c_void>();
        assert_eq!(unsafe { preview_decode_interrupt_callback(opaque) }, 0);

        let guard = state.install(Arc::new(|| true));
        assert_eq!(unsafe { preview_decode_interrupt_callback(opaque) }, 1);
        drop(guard);

        assert_eq!(unsafe { preview_decode_interrupt_callback(opaque) }, 0);
    }

    #[test]
    fn external_decode_process_is_reaped_when_cancellation_is_observed() {
        let mut command = Command::new("rustc");
        command.arg("--version").stdout(Stdio::piped()).stderr(Stdio::piped());

        let outcome = run_external_decode_command_cancellable(&mut command, &|| true)
            .expect("cancellation should reap the external process");

        assert!(outcome.is_none());
    }

    #[test]
    fn playback_session_drains_reordered_frames_between_sequential_requests() {
        const FIXTURE: &[u8] = include_bytes!("../../../tests/fixtures/small/h264-bframes.mp4");
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("h264-bframes.mp4");
        std::fs::write(&path, FIXTURE).expect("write synthetic H.264 fixture");
        clear_global_preview_frame_cache();
        clear_thread_local_preview_decode_session();

        let mut decoded_pixels = Vec::new();
        for index in 5..20 {
            let request = PreviewDecodeRequest::new(
                path.as_path(),
                TimelineTime::new(i64::from(index), 25).expect("exact source time"),
                PreviewDecodeAccessMode::PlaybackCursor,
                test_source_color(),
            )
            .with_max_size(Some(64), Some(64));
            let outcome = decode_preview_frame_cancellable(request, || false)
                .expect("sequential B-frame playback request must decode");
            let PreviewDecodeOutcome::Frame(frame) = outcome else {
                panic!("CPU playback fixture must return an RGBA frame");
            };
            if index > 5 {
                assert!(
                    frame.diagnostics.session_reused,
                    "sequential playback requests must exercise one decoder session"
                );
            }
            decoded_pixels.push(frame.rgba().to_vec());
        }

        assert!(
            decoded_pixels.windows(2).any(|frames| frames[0] != frames[1]),
            "the moving fixture must produce distinct decoded frames"
        );
        clear_thread_local_preview_decode_session();
        clear_global_preview_frame_cache();
    }

    #[test]
    fn preview_file_fingerprint_changes_when_file_is_replaced() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("proxy.mp4");
        std::fs::write(&path, b"old").expect("old");
        let first = MediaFileFingerprint::capture(&path);
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&path, b"new proxy bytes").expect("new");
        let second = MediaFileFingerprint::capture(&path);

        assert_ne!(first, second);
    }

    #[test]
    fn preview_frame_cache_is_keyed_by_file_fingerprint() {
        clear_global_preview_frame_cache();
        let path = PathBuf::from("same-proxy-path.mp4");
        let old_fingerprint = MediaFileFingerprint {
            len: Some(3),
            modified_secs: Some(1),
            modified_nanos: Some(0),
        };
        let new_fingerprint = MediaFileFingerprint {
            len: Some(15),
            modified_secs: Some(2),
            modified_nanos: Some(0),
        };
        let frame = RgbaFrame::new(
            2,
            1,
            vec![1, 2, 3, 4, 5, 6, 7, 8],
            test_rgba_contract(),
            PreviewDecodePath::InProcessFfmpegCpuRgba,
        );

        preview_cache_put_with_fingerprint(&path, old_fingerprint, 2, 1, 100, frame);

        assert!(
            preview_cache_get(&path, new_fingerprint, test_source_color(), 2, 1, 100, 1).is_none()
        );
        assert!(
            preview_cache_get(&path, old_fingerprint, test_source_color(), 2, 1, 100, 1).is_some()
        );
        clear_global_preview_frame_cache();
    }

    #[test]
    fn preview_frame_cache_respects_strict_pts_tolerance() {
        clear_global_preview_frame_cache();
        let path = PathBuf::from("strict-cache-window.mp4");
        let fingerprint = MediaFileFingerprint {
            len: Some(3),
            modified_secs: Some(1),
            modified_nanos: Some(0),
        };
        let frame = RgbaFrame::new(
            2,
            1,
            vec![1, 2, 3, 4, 5, 6, 7, 8],
            test_rgba_contract(),
            PreviewDecodePath::InProcessFfmpegCpuRgba,
        );

        preview_cache_put_with_fingerprint(&path, fingerprint, 2, 1, 100, frame);

        assert!(preview_cache_get(&path, fingerprint, test_source_color(), 2, 1, 105, 5).is_some());
        assert!(preview_cache_get(&path, fingerprint, test_source_color(), 2, 1, 106, 5).is_none());
        clear_global_preview_frame_cache();
    }

    #[test]
    fn preview_frame_cache_isolated_by_applied_source_color_contract() {
        clear_global_preview_frame_cache();
        let path = PathBuf::from("same-frame-different-color.mov");
        let fingerprint = MediaFileFingerprint {
            len: Some(8),
            modified_secs: Some(3),
            modified_nanos: Some(0),
        };
        let rec709 = test_source_color();
        let frame = RgbaFrame::new(
            2,
            1,
            vec![16, 16, 16, 255, 235, 235, 235, 255],
            test_rgba_contract(),
            PreviewDecodePath::InProcessFfmpegCpuRgba,
        );
        preview_cache_put_with_fingerprint(&path, fingerprint, 2, 1, 100, frame);

        let rec2020 =
            PreviewSourceColorContract::automatic(ColorSpace::Rec2020, DecodedVideoRange::Limited);
        assert!(preview_cache_get(&path, fingerprint, rec709, 2, 1, 100, 1).is_some());
        assert!(preview_cache_get(&path, fingerprint, rec2020, 2, 1, 100, 1).is_none());
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
            TimelineTime::from_f64_quantized(timestamp_secs, 1_000_000)
                .expect("quantized diagnostic source time"),
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            test_source_color(),
        )
        .with_max_size(max_width, max_height);
        let frame = match decode_preview_frame_cancellable(request, || false)
            .expect("decode preview fixture")
        {
            PreviewDecodeOutcome::Frame(frame) => frame,
            PreviewDecodeOutcome::Canceled(_) => {
                panic!("still-frame perf decode canceled")
            }
            PreviewDecodeOutcome::NativeGpuFrame(_) => {
                panic!("still-frame perf decode requires CPU RGBA output")
            }
            PreviewDecodeOutcome::FloatFrame(_) => {
                panic!("Rec.709 still-frame perf fixture unexpectedly decoded as float")
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
        let fingerprint = MediaFileFingerprint::capture(&path);
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
                TimelineTime::from_f64_quantized(timestamp_secs, 1_000_000)
                    .expect("quantized diagnostic source time"),
                PreviewDecodeAccessMode::PlaybackCursor,
                test_source_color(),
            )
            .with_max_size(max_width, max_height)
            .with_fingerprint(fingerprint);
            let frame = match decode_preview_frame_cancellable(request, || false)
                .expect("decode preview fixture frame")
            {
                PreviewDecodeOutcome::Frame(frame) => frame,
                PreviewDecodeOutcome::Canceled(_) => {
                    panic!("playback sequence perf decode canceled")
                }
                PreviewDecodeOutcome::NativeGpuFrame(_) => {
                    panic!("playback sequence perf decode requires CPU RGBA output")
                }
                PreviewDecodeOutcome::FloatFrame(_) => {
                    panic!("Rec.709 playback perf fixture unexpectedly decoded as float")
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
