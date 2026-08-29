//! 预览帧解码
//!
//! 使用 avformat_seek_file（安全 Rust API）定位到目标前的关键帧，
//! flush 解码器后向前解码到目标 PTS，保证返回精确帧。

use crate::decoder::{
    DecodedFrameResidency, DecodedGpuFrameHandleKind, DecodedVideoMatrix, DecodedVideoRange,
    DecodedVideoRangeContract, DecodedVideoSampling, DecodedVideoSurfaceFormat, HwAccelBackend,
    HwAccelCodecConfigProbe, HwAccelDeviceContext, HwAccelDeviceContextProbe,
    HwAccelDeviceSelector, HwAccelPixelFormat, HwAccelProbe, HwDeviceContextPool,
};
use ffmpeg_next as ffmpeg;
use mondrian_core::types::ColorSpace;
use mondrian_core::{
    FrameRounding, MondrianError, Rational, Result, SourceSampleTarget, TimelineTime,
};
pub use mondrian_core::{MediaFileChangeStamp, MediaFileFingerprint, MediaFileObjectIdentity};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::{c_void, CString};
use std::path::Path;
use std::path::PathBuf;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

mod cancellation;
mod decode_contract;
mod decode_session;
mod decoded_frame;
mod decoded_surface_window;
mod demux_process;
mod demux_protocol;
mod demux_protocol_ffi;
mod demux_source;
mod demux_worker;
mod execution_progress;
mod external_decode;
mod frame_contract;
mod frame_materialization;
pub(crate) use frame_materialization::resize_float_rgba;
mod hardware_decode;
mod native_frame;
mod playback_ring;
mod seek_index;

pub use decode_contract::{
    CameraRawDecodeIntent, PreviewCompactCpuYuvHint, PreviewDecodeAlphaPresence,
    PreviewDecodeContractError, PreviewDecodeKey, PreviewDecodePayloadRequirement,
    PreviewDecodeRepresentation, PreviewDecodeSource, PreviewNativeSurfaceHint,
    PreviewRepresentationQuality,
};
pub use demux_worker::run_preview_demux_worker;
pub use seek_index::{
    PreviewSeekIndexCache, PreviewSeekIndexCacheDiagnostics, PreviewSeekIndexCachePolicy,
};

fn verify_preview_source_revision(path: &Path, expected: MediaFileFingerprint) -> Result<()> {
    if !expected.authorizes_reuse() {
        return Err(MondrianError::MediaSourceRevisionUnavailable {
            path: path.display().to_string(),
            reason: "request did not carry object identity and filesystem change generation"
                .to_owned(),
        });
    }
    let actual = MediaFileFingerprint::capture(path);
    if actual != expected {
        return Err(MondrianError::MediaSourceRevisionChanged {
            path: path.display().to_string(),
            expected: Box::new(expected),
            actual: Box::new(actual),
        });
    }
    Ok(())
}

fn resolve_preview_execution_fingerprint(
    path: &Path,
    requested: Option<MediaFileFingerprint>,
) -> Result<MediaFileFingerprint> {
    match requested {
        Some(expected) => {
            verify_preview_source_revision(path, expected)?;
            Ok(expected)
        }
        None => {
            let observed = MediaFileFingerprint::capture(path);
            if observed.authorizes_reuse() {
                Ok(observed)
            } else {
                Err(MondrianError::MediaSourceRevisionUnavailable {
                    path: path.display().to_string(),
                    reason:
                        "filesystem did not expose a stable object identity and change generation"
                            .to_owned(),
                })
            }
        }
    }
}

use cancellation::{
    preview_decode_interrupt_callback, PreviewDecodeCancelProbe, PreviewDecodeInterruptState,
};
pub use cancellation::{
    PreviewDecodeCancellation, PreviewDecodeCancellationCheckpoint,
    PreviewDecodeCancellationCheckpointEvidence, PreviewDecodeCancellationEvidence,
    PreviewDecodeCancellationSource,
};
pub use execution_progress::{
    PreviewDecodeExecutionObserver, PreviewDecodeExecutionProgress, PreviewDecodeExecutionStage,
    PreviewIsolatedDemuxExecutionEvidence,
};
use frame_contract::{decoded_surface_format_from_pixel, resolve_cpu_rgba_contract_from_metadata};
#[cfg(test)]
use frame_contract::{decoded_video_sampling_from_frame, resolve_cpu_rgba_contract};
#[cfg(test)]
use frame_materialization::materialize_decoded_frame;
use frame_materialization::materialize_decoded_frame_with_session_output_lease;
#[cfg(test)]
use frame_materialization::{
    convert_decoded_to_rgba, decoded_native_surface_format_from_software_format,
    PreviewNativeFrameMaterializationError,
};
#[cfg(test)]
use native_frame::parse_ffmpeg_d3d11_texture;
#[cfg(all(test, mondrian_ffmpeg_7_1))]
use native_frame::{FfmpegAvD3D12VaFrame, FfmpegAvD3D12VaSyncContext};
pub use native_frame::{
    FfmpegD3D11TextureView, FfmpegD3D12TextureView, FfmpegNativeDecodedFrameResource,
    FfmpegNativeDecodedFrameResourceError, PreviewNativeDecodedFrame,
    PreviewNativeDecodedFrameError, PreviewNativeDecodedFrameHandle,
    PreviewNativeDecodedFrameResource,
};
#[cfg(target_os = "linux")]
pub use native_frame::{
    FfmpegDrmPrimeFrame, FfmpegDrmPrimeLayer, FfmpegDrmPrimeObject, FfmpegDrmPrimePlane,
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
const PREVIEW_SEEK_INDEX_CACHE_CAPACITY: usize = 32;
const PREVIEW_PLAYBACK_SESSION_RING_CAPACITY: usize = 8;
// Session-local forward reuse is an optimization, not an independent
// residency authority. A byte limit prevents eight large CPU frames from
// bypassing the App-owned Preview Frame Store budget.
const PREVIEW_PLAYBACK_SESSION_RING_BYTE_BUDGET: usize = 96 * 1024 * 1024;
// Prefetch is allowed to advance the one resident Playback decoder while an
// older current-frame request is queued. Retain the same eight-frame bounded
// temporal prefix as startup preroll so that scheduling overlap never turns
// back into a long-GOP seek. At 256 MiB the window can hold eight 4K 4:2:2
// 10-bit software surfaces while remaining independently byte bounded.
const PREVIEW_PLAYBACK_DECODE_WINDOW_CAPACITY: usize = 8;
const PREVIEW_PLAYBACK_DECODE_WINDOW_BYTE_BUDGET: usize = 256 * 1024 * 1024;
// Reverse playback must replay a forward-decoded GOP tail instead of seeking
// to the same keyframe for every descending frame. The window shares the same
// conservative memory envelope as the output ring and independently caps
// retained decoder references so hardware surface pools cannot be exhausted.
const PREVIEW_REVERSE_DECODE_WINDOW_CAPACITY: usize = 4;
const PREVIEW_REVERSE_DECODE_WINDOW_BYTE_BUDGET: usize = 96 * 1024 * 1024;
// Native preview frames may outlive one codec call in the bounded App
// completion transport (8 queued plus at most 2 worker-held results), Preview
// Frame Store (8), renderer import (4), and exact selector/transient ownership
// (2). HEVC frame threading and reorder must retain independent headroom beyond
// those external leases; otherwise a canceled random seek can block inside the
// driver waiting for a surface and never reach its next cooperative checkpoint.
const PREVIEW_NATIVE_DECODE_EXTRA_HW_FRAMES: i32 = 32;
// Exact random access can discard dependency-free non-reference output while
// traversing the distant prefix of a long GOP. Restore full decode well before
// the target so reordered output and the exact requested picture remain
// available. Sixty-four frames exceeds ordinary H.264/HEVC DPB plus decoder
// thread headroom without turning the optimization into approximate seeking.
const PREVIEW_EXACT_SEEK_FULL_DECODE_PREROLL_FRAMES: i64 = 64;
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
    /// In-process DNG/CinemaDNG Adapter returned developed scene-linear RGBA32F.
    InProcessCameraRawDng,
    /// In-process FFmpeg decoder retained compact CPU YUV planes for GPU materialization.
    InProcessFfmpegCpuYuv,
    /// In-process FFmpeg decoder returned a retained native hardware surface.
    InProcessFfmpegNative,
    /// Experimental external `ffmpeg` process returned CPU RGBA bytes.
    ExternalFfmpegCpuRgba,
    /// Playback cursor reused a frame from its session-local forward ring.
    PlaybackSessionRingHit,
}

/// Decode-session lifecycle fact for the frame that was actually returned.
///
/// This is deliberately not a Boolean: a session-local ring hit bypasses the
/// decoder, while a contract change replaces an existing session. Consumers
/// must not infer either case from `reused == false`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PreviewDecodeSessionDisposition {
    /// The producer did not attach lifecycle evidence.
    #[default]
    Unspecified,
    /// No compatible session existed and a new session was opened.
    Opened,
    /// An incompatible or retired session was replaced before decoding.
    Replaced,
    /// A compatible access-mode-local session was reused.
    Reused,
    /// The result came from a cache/ring path and did not enter a decoder session.
    BypassedCache,
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
    /// Ordered traversal direction for session-local playback reuse.
    ///
    /// This is an execution hint only: source-time selection remains exact and
    /// independent of traversal direction.
    pub playback_direction: PreviewPlaybackDirection,
}

/// Ordered playback traversal supplied to the media Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PreviewPlaybackDirection {
    /// Increasing source time.
    #[default]
    Forward,
    /// Decreasing source time, enabling bounded decoded-GOP replay.
    Reverse,
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
    /// A hardware-preferred Session failed during runtime decode and the same
    /// semantic request was recovered through a fresh software Session.
    RuntimeHardwareFailure,
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
    /// Optional exact physical video stream selected by the caller's probe.
    pub video_stream_index: Option<u32>,
    /// Exact media-source-local target; FFmpeg PTS lowering occurs inside the Adapter.
    pub source_sample: SourceSampleTarget,
    /// Optional maximum output width.
    pub max_width: Option<u32>,
    /// Optional maximum output height.
    pub max_height: Option<u32>,
    /// Exact decoded-payload representation promised by the request identity.
    ///
    /// Session reuse and materialization must preserve this value; a compact
    /// YUV request cannot silently return an RGBA payload under the same key.
    pub representation: PreviewDecodeRepresentation,
    /// Access pattern that drives decoder residency and seek policy.
    pub access_mode: PreviewDecodeAccessMode,
    /// Optional complete bounded file-revision evidence resolved by the caller.
    ///
    /// A complete value is revalidated at the execution worker before Session
    /// reuse/open; a mismatch fails closed instead of decoding under stale
    /// probe or color semantics.
    pub fingerprint: Option<MediaFileFingerprint>,
    /// Adaptive scheduling hints selected by the caller.
    pub adaptive_hints: PreviewDecodeAdaptiveHints,
    /// Hardware decode/native-residency preference selected by the caller.
    pub hardware_decode_request: PreviewHardwareDecodeRequest,
    /// Renderer-selected hardware decoder device for native frame residency.
    pub hardware_decode_device_selector: Option<HwAccelDeviceSelector>,
    /// Resolved source color and range contract required by CPU YUV conversion.
    pub source_color: PreviewSourceColorContract,
    /// Probe-admitted Camera RAW development identity.
    pub camera_raw: Option<CameraRawDecodeIntent>,
}

/// Semantic interpretation of decoded RGB samples at the Media/Renderer seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewSourceSampleIdentity {
    /// Picture samples retain one explicit source color identity.
    ColorManaged(ColorSpace),
    /// Numeric RGB(A) channels are technical data and must bypass OCIO.
    DataTexture,
}

/// App-resolved source sample and range facts required before Media materialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PreviewSourceColorContract {
    /// Whether decoded channels are color-managed picture samples or numeric data.
    pub identity: PreviewSourceSampleIdentity,
    /// Authority-aware encoded quantization-range interpretation.
    pub range: DecodedVideoRangeContract,
    /// Explicit policy/authored fallback used only when a decoded YUV frame
    /// omits its matrix coefficient. Stream/frame metadata still wins.
    #[serde(default)]
    pub yuv_matrix_fallback: Option<DecodedVideoMatrix>,
}

impl PreviewSourceColorContract {
    /// Build a source color contract from resolved input color and range authority.
    pub const fn new(color_space: ColorSpace, range: DecodedVideoRangeContract) -> Self {
        Self {
            identity: PreviewSourceSampleIdentity::ColorManaged(color_space),
            range,
            yuv_matrix_fallback: None,
        }
    }

    /// Build a numeric data-texture contract that carries no color identity.
    pub const fn data_texture(range: DecodedVideoRangeContract) -> Self {
        Self {
            identity: PreviewSourceSampleIdentity::DataTexture,
            range,
            yuv_matrix_fallback: None,
        }
    }

    /// Return the effective source color identity for color-managed picture samples.
    pub const fn color_space(self) -> Option<ColorSpace> {
        match self.identity {
            PreviewSourceSampleIdentity::ColorManaged(color_space) => Some(color_space),
            PreviewSourceSampleIdentity::DataTexture => None,
        }
    }

    /// Whether decoded channels are an explicit numeric data texture.
    pub const fn is_data_texture(self) -> bool {
        matches!(self.identity, PreviewSourceSampleIdentity::DataTexture)
    }

    /// Whether color-managed samples carry a scene-linear identity.
    pub fn is_scene_linear(self) -> bool {
        match self.identity {
            PreviewSourceSampleIdentity::ColorManaged(color_space) => color_space.is_scene_linear(),
            PreviewSourceSampleIdentity::DataTexture => false,
        }
    }

    /// Bind an explicit missing-matrix policy into decode and cache identity.
    pub const fn with_yuv_matrix_fallback(mut self, matrix: DecodedVideoMatrix) -> Self {
        if !self.is_data_texture() {
            self.yuv_matrix_fallback = Some(matrix);
        }
        self
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
    /// Project one validated physical decode key into the existing execution request.
    ///
    /// This keeps access-mode scheduling and adaptive/hardware execution hints
    /// outside [`PreviewDecodeKey`] while preventing callers from rebuilding
    /// path, revision, stream, time, representation, or source color
    /// independently. The request's extent cap is the representation's own
    /// extent — never the consumer's output extent.
    pub fn from_key(key: &'a PreviewDecodeKey, access_mode: PreviewDecodeAccessMode) -> Self {
        let (max_width, max_height) =
            key.representation().maximum_dimensions_for_source(key.source().source_extent());
        Self {
            path: key.source().path(),
            video_stream_index: Some(key.source().video_stream_index()),
            source_sample: key.source_sample(),
            max_width,
            max_height,
            representation: key.representation(),
            access_mode,
            fingerprint: Some(key.source().fingerprint()),
            adaptive_hints: PreviewDecodeAdaptiveHints::default(),
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            hardware_decode_device_selector: None,
            source_color: key.source_color(),
            camera_raw: key.camera_raw(),
        }
    }

    /// Create a request for one scaled preview decode outcome.
    pub fn new(
        path: &'a Path,
        source_sample: SourceSampleTarget,
        access_mode: PreviewDecodeAccessMode,
        source_color: PreviewSourceColorContract,
    ) -> Self {
        Self {
            path,
            video_stream_index: None,
            source_sample,
            max_width: None,
            max_height: None,
            representation: PreviewDecodeRepresentation::NativeCpu,
            access_mode,
            fingerprint: None,
            adaptive_hints: PreviewDecodeAdaptiveHints::default(),
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            hardware_decode_device_selector: None,
            source_color,
            camera_raw: None,
        }
    }

    /// Select one exact physical video stream instead of FFmpeg's best-stream heuristic.
    pub fn with_video_stream_index(mut self, video_stream_index: u32) -> Self {
        self.video_stream_index = Some(video_stream_index);
        self
    }

    /// Set optional maximum output dimensions.
    pub fn with_max_size(mut self, max_width: Option<u32>, max_height: Option<u32>) -> Self {
        self.max_width = max_width;
        self.max_height = max_height;
        self
    }

    /// Attach a caller-resolved file fingerprint.
    ///
    /// Complete revisions are execution preconditions, not cache hints.
    pub fn with_fingerprint(mut self, fingerprint: MediaFileFingerprint) -> Self {
        self.fingerprint = Some(fingerprint);
        self
    }

    /// Select a probe-admitted Camera RAW development path.
    pub const fn with_camera_raw(mut self, camera_raw: CameraRawDecodeIntent) -> Self {
        self.camera_raw = Some(camera_raw);
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
        // Forward continuation can only produce a strictly newer target.
        // Equality requires an exact retained artifact; without one, decoding
        // another packet would select the following frame and permanently
        // phase-shift a GPU-resident/no-ring playback session.
        if reached_eof || self.forward_reuse_frame_window <= 0 || target_pts <= last_pts {
            return false;
        }
        let max_distance =
            frame_duration_pts.max(1).saturating_mul(self.forward_reuse_frame_window);
        target_pts.saturating_sub(last_pts) <= max_distance
    }

    fn forward_decode_budget_exhausted(self, frames_decoded: usize) -> bool {
        frames_decoded >= self.forward_decode_budget_frames
    }

    /// Preserve the caller's exact source sample as temporal selection
    /// authority for every access mode.
    ///
    /// Scrub may seek from or present a nearby keyframe, but a container index
    /// can include negative decode-preroll keyframes that are not themselves
    /// presentable frames. Rewriting the target to such an anchor makes the
    /// selector scan for an impossible output until its budget is exhausted.
    fn decode_target_pts(self, requested_pts: i64) -> i64 {
        match self.access_mode {
            PreviewDecodeAccessMode::PlaybackCursor
            | PreviewDecodeAccessMode::ScrubCursor
            | PreviewDecodeAccessMode::RandomAccessStillFrame => requested_pts,
        }
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

/// Evidence used to establish one decoded frame's presentation interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PreviewTemporalExtentSource {
    /// No positive interval boundary is known; only the exact start PTS is proven.
    #[default]
    Unknown,
    /// FFmpeg supplied a positive decoded-frame/packet duration.
    FrameDuration,
    /// A later decoded frame supplied the exclusive successor boundary.
    SuccessorBoundary,
}

/// Narrow immutable temporal-selection evidence retained beyond the decoder.
///
/// This is the physical PTS interval actually selected for one exact source
/// target. It intentionally excludes decoder policy and performance diagnostics
/// so Preview caches, presentation, and acceptance evidence can retain temporal
/// identity without depending on the complete decoder implementation record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewDecodeTemporalSelection {
    /// Requested stream-local PTS after exact source-target lowering.
    pub requested_pts: i64,
    /// First PTS of the selected presentation interval.
    pub selected_pts: i64,
    /// Positive duration of the selected presentation interval.
    pub selected_duration_pts: i64,
    /// Evidence that established the interval's exclusive end.
    pub extent_source: PreviewTemporalExtentSource,
    /// Whether the selection deliberately approximated the request.
    pub temporal_approximation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DecodedTemporalExtent {
    start_pts: i64,
    duration_pts: Option<i64>,
    source: PreviewTemporalExtentSource,
}

impl DecodedTemporalExtent {
    fn point(start_pts: i64) -> Self {
        Self {
            start_pts,
            duration_pts: None,
            source: PreviewTemporalExtentSource::Unknown,
        }
    }

    fn from_duration(start_pts: i64, duration_pts: i64) -> Self {
        if duration_pts <= 0 || start_pts.checked_add(duration_pts).is_none() {
            return Self::point(start_pts);
        }
        Self {
            start_pts,
            duration_pts: Some(duration_pts),
            source: PreviewTemporalExtentSource::FrameDuration,
        }
    }

    fn from_decoded_frame(start_pts: i64, frame: &ffmpeg::util::frame::video::Video) -> Self {
        Self::from_duration(start_pts, frame.packet().duration)
    }

    fn from_diagnostics(diagnostics: PreviewDecodeDiagnostics) -> Option<Self> {
        diagnostics.selected_pts.map(|start_pts| {
            let Some(duration_pts) =
                diagnostics.selected_duration_pts.filter(|duration| *duration > 0).filter(|_| {
                    diagnostics.selected_temporal_extent_source
                        != PreviewTemporalExtentSource::Unknown
                })
            else {
                return Self::point(start_pts);
            };
            let mut extent = Self::from_duration(start_pts, duration_pts);
            if extent.duration_pts.is_some() {
                extent.source = diagnostics.selected_temporal_extent_source;
            }
            extent
        })
    }

    fn with_successor(self, successor_pts: i64) -> Self {
        let Some(successor_duration) = successor_pts.checked_sub(self.start_pts) else {
            return self;
        };
        if successor_duration <= 0 {
            return self;
        }
        // The next decoded presentation timestamp is the authoritative
        // exclusive boundary for the predecessor. Container/packet duration
        // may be shorter (VFR cadence gaps) or longer (overlap); in both cases
        // a video presentation holds the predecessor until its successor.
        Self {
            start_pts: self.start_pts,
            duration_pts: Some(successor_duration),
            source: PreviewTemporalExtentSource::SuccessorBoundary,
        }
    }

    fn end_pts(self) -> Option<i64> {
        self.duration_pts.and_then(|duration| self.start_pts.checked_add(duration))
    }

    fn covers(self, requested_pts: i64) -> bool {
        requested_pts == self.start_pts
            || (requested_pts > self.start_pts
                && self.end_pts().is_some_and(|end_pts| requested_pts < end_pts))
    }

    fn distance_to(self, requested_pts: i64) -> i64 {
        self.start_pts.saturating_sub(requested_pts).saturating_abs()
    }
}

fn temporal_selection_is_approximate(
    requested_pts: i64,
    selected_extent: Option<DecodedTemporalExtent>,
) -> bool {
    selected_extent.is_some_and(|extent| !extent.covers(requested_pts))
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
    /// App-level preview decode workers to spawn for playback/non-playback lanes.
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
        // Playback and Interactive decoder residency are mutually exclusive.
        // A Playback Session may therefore use the available decode budget
        // without multiplying it across the idle NonPlayback workers. Keep a
        // finite cap so very large hosts do not create excessive codec queues.
        let max_decoder_threads_per_worker = usable_threads.clamp(1, 12);
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
            Self::InProcessCameraRawDng => "InProcessCameraRawDng",
            Self::InProcessFfmpegCpuYuv => "InProcessFfmpegCpuYuv",
            Self::InProcessFfmpegNative => "InProcessFfmpegNative",
            Self::ExternalFfmpegCpuRgba => "ExternalFfmpegCpuRgba",
            Self::PlaybackSessionRingHit => "PlaybackSessionRingHit",
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
    /// Time spent waiting for downstream native-output leases before decoder reuse.
    #[serde(default)]
    pub output_lease_wait_us: u64,
    /// Time spent checking the decoder-session-local playback ring.
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
        self.output_lease_wait_us =
            self.output_lease_wait_us.saturating_add(other.output_lease_wait_us);
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
    /// Whether this result came from the decoder-session-local playback ring.
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
    /// Positive stream-tick duration of the selected frame's proven presentation interval.
    #[serde(default)]
    pub selected_duration_pts: Option<i64>,
    /// Evidence source that established the selected frame's presentation interval.
    #[serde(default)]
    pub selected_temporal_extent_source: PreviewTemporalExtentSource,
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
    /// Exact decoder-session lifecycle used by this result.
    #[serde(default)]
    pub session_disposition: PreviewDecodeSessionDisposition,
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
/// retained across playback-ring and App-owned Frame Store reuse so acceptance gates
/// can bind the frame ultimately presented for a Frame Demand to the decode
/// work that originally produced it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
            cache_hit: path == PreviewDecodePath::PlaybackSessionRingHit,
            access_mode: PreviewDecodeAccessMode::RandomAccessStillFrame,
            external_process: path == PreviewDecodePath::ExternalFfmpegCpuRgba,
            cpu_resident: path != PreviewDecodePath::InProcessFfmpegNative,
            seek_performed: false,
            requested_pts: None,
            selected_pts: None,
            selected_duration_pts: None,
            selected_temporal_extent_source: PreviewTemporalExtentSource::Unknown,
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
            session_disposition: PreviewDecodeSessionDisposition::Unspecified,
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

    /// Project complete decoder diagnostics into stable temporal identity.
    pub fn temporal_selection(&self) -> Option<PreviewDecodeTemporalSelection> {
        let requested_pts = self.requested_pts?;
        let selected_pts = self.selected_pts?;
        let selected_duration_pts = self.selected_duration_pts.filter(|duration| *duration > 0)?;
        (self.selected_temporal_extent_source != PreviewTemporalExtentSource::Unknown
            && selected_pts.checked_add(selected_duration_pts).is_some())
        .then_some(PreviewDecodeTemporalSelection {
            requested_pts,
            selected_pts,
            selected_duration_pts,
            extent_source: self.selected_temporal_extent_source,
            temporal_approximation: self.temporal_approximation,
        })
    }

    fn with_elapsed(mut self, elapsed: Duration) -> Self {
        self.elapsed_us = elapsed.as_micros().min(u128::from(u64::MAX)) as u64;
        self
    }

    fn playback_ring_hit(elapsed: Duration) -> Self {
        Self::new(PreviewDecodePath::PlaybackSessionRingHit)
            .with_access_mode(PreviewDecodeAccessMode::PlaybackCursor)
            .with_session_disposition(PreviewDecodeSessionDisposition::BypassedCache)
            .with_elapsed(elapsed)
    }

    fn with_access_mode(mut self, access_mode: PreviewDecodeAccessMode) -> Self {
        self.access_mode = access_mode;
        self = self.with_access_policy(PreviewDecodeAccessPolicy::for_access_mode(access_mode));
        self
    }

    fn with_temporal_selection(
        mut self,
        requested_pts: i64,
        selected_extent: Option<DecodedTemporalExtent>,
    ) -> Self {
        self.requested_pts = Some(requested_pts);
        self.selected_pts = selected_extent.map(|extent| extent.start_pts);
        self.selected_duration_pts = selected_extent.and_then(|extent| extent.duration_pts);
        self.selected_temporal_extent_source = selected_extent
            .map(|extent| extent.source)
            .unwrap_or(PreviewTemporalExtentSource::Unknown);
        self.temporal_approximation =
            temporal_selection_is_approximate(requested_pts, selected_extent);
        self
    }

    fn with_session_disposition(mut self, disposition: PreviewDecodeSessionDisposition) -> Self {
        self.session_disposition = disposition;
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

/// Current preview decode backend selection.
///
/// The product currently always runs `Auto`; a process-global setter had no
/// production caller and created an unowned mutable seam, so it was removed.
/// Backend choice belongs to explicit Session/worker configuration, not a
/// latent global. `Auto` still honors the bounded
/// `MONDRIAN_PREVIEW_EXTERNAL_FFMPEG_CPU_RGBA` environment opt-in for
/// diagnostics.
pub fn preview_decode_backend() -> PreviewDecodeBackend {
    PreviewDecodeBackend::Auto
}

pub use decoded_frame::*;

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
    decode_preview_frame_outcome(request, should_cancel)
}

/// Result of a cancellable preview decode request.
#[derive(Debug, Clone)]
pub enum PreviewDecodeOutcome {
    /// Decode completed with a CPU RGBA preview frame.
    Frame(RgbaFrame),
    /// Decode completed with a CPU scene-linear RGBA f32 preview frame.
    FloatFrame(FloatRgbaFrame),
    /// Decode completed with compact CPU YUV planes for direct GPU materialization.
    CpuYuvFrame(CpuYuvFrame),
    /// Decode completed with a GPU-resident native frame.
    NativeGpuFrame(PreviewNativeDecodedFrame),
    /// The caller marked this request obsolete before a frame was returned.
    Canceled(PreviewDecodeCancellation),
}

pub use decode_session::{
    clear_thread_local_preview_decode_session, PreviewDecodeSessionContext,
    PreviewDecodeSessionContextBootstrap, PreviewDecodeSessionResidencyConfig,
    PreviewDecodeWorkerResources,
};
use decode_session::{
    decode_preview_frame_outcome, preview_create_rgba_scaler, PreviewDecodedFramePayload,
};
#[cfg(test)]
use decode_session::{
    decoded_temporal_candidate_within_selection_distance,
    exact_seek_non_reference_discard_until_pts, forward_decode_work_units,
    select_decoded_temporal_candidate, DecodedTemporalCandidate,
};
use hardware_decode::{preview_hardware_frame_format, PreviewHardwareDecodePlan};
#[cfg(test)]
use playback_ring::PreviewPlaybackRing;
use seek_index::{PreviewSeekIndex, PreviewSeekIndexDiagnostics, PreviewSeekResolution};

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

fn preview_decode_threading_config(
    access_mode: PreviewDecodeAccessMode,
    decode_pixels: u64,
) -> PreviewDecodeThreadingConfig {
    let kind = std::env::var("MONDRIAN_PREVIEW_DECODE_THREADING")
        .ok()
        .and_then(|value| PreviewDecodeThreadingKind::from_env(&value))
        .unwrap_or_else(|| default_threading_kind_for_software_decode(access_mode, decode_pixels));
    let budget = preview_decode_cpu_budget();
    let count = std::env::var("MONDRIAN_PREVIEW_DECODE_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.clamp(1, budget.max_decoder_threads_per_worker))
        .unwrap_or_else(|| default_decoder_threads_for_access_mode(budget, access_mode));
    PreviewDecodeThreadingConfig { kind, count }
}

/// Default software-decode threading kind.
///
/// Frame threading pipelines coded pictures and therefore provides stable
/// throughput independently of how many slices the bitstream author placed in
/// each picture. A declared slice-thread count is not evidence that a camera
/// stream contains enough independent slices to use it. Real H.264 High 4:2:2
/// 10-bit 3840x2160@60000/1001 playback measured about 9.4 ms steady work per
/// frame with frame threading versus 15-25 ms and load-sensitive underruns
/// with slice threading. An explicit `MONDRIAN_PREVIEW_DECODE_THREADING`
/// override remains available for codec/source qualification.
fn default_threading_kind_for_software_decode(
    _access_mode: PreviewDecodeAccessMode,
    _decode_pixels: u64,
) -> PreviewDecodeThreadingKind {
    PreviewDecodeThreadingKind::Frame
}

fn default_decoder_threads_for_access_mode(
    budget: PreviewDecodeCpuBudget,
    access_mode: PreviewDecodeAccessMode,
) -> usize {
    if access_mode == PreviewDecodeAccessMode::PlaybackCursor {
        budget.max_decoder_threads_per_worker
    } else {
        budget.decoder_threads_per_worker
    }
}

fn preview_decode_threading_config_for_codec(
    codec_id: ffmpeg::codec::Id,
    access_mode: PreviewDecodeAccessMode,
    decode_pixels: u64,
    worker_thread_limit: Option<usize>,
) -> PreviewDecodeThreadingConfig {
    let requested = preview_decode_threading_config(access_mode, decode_pixels);
    let requested = cap_preview_decode_threading_config(requested, worker_thread_limit);
    apply_preview_codec_threading_policy(codec_id, requested)
}

fn cap_preview_decode_threading_config(
    requested: PreviewDecodeThreadingConfig,
    worker_thread_limit: Option<usize>,
) -> PreviewDecodeThreadingConfig {
    PreviewDecodeThreadingConfig {
        kind: requested.kind,
        count: requested.count.min(worker_thread_limit.unwrap_or(usize::MAX).max(1)),
    }
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

use external_decode::{
    ensure_ffmpeg_initialized, fit_target_size, preview_external_ffmpeg_cpu_rgba_enabled,
    try_decode_with_external_ffmpeg_cpu_rgba,
};
#[cfg(test)]
use external_decode::{
    preview_external_ffmpeg_cpu_rgba_allowed_for_access_mode,
    run_external_decode_command_cancellable,
};

pub(super) fn source_sample_to_stream_pts(
    source_sample: SourceSampleTarget,
    stream_tb: ffmpeg::Rational,
    stream_start_pts: i64,
) -> std::result::Result<i64, String> {
    let relative = source_sample_to_time_base_ticks(
        source_sample,
        i64::from(stream_tb.numerator()),
        i64::from(stream_tb.denominator()),
    )?;
    stream_start_pts.checked_add(relative).ok_or_else(|| {
        format!(
            "source target PTS overflow: target={source_sample:?} stream_time_base={}/{} start_pts={stream_start_pts}",
            stream_tb.numerator(),
            stream_tb.denominator()
        )
    })
}

fn source_sample_to_time_base_ticks(
    source_sample: SourceSampleTarget,
    time_base_num: i64,
    time_base_den: i64,
) -> std::result::Result<i64, String> {
    if source_sample.time().is_negative() {
        return Err(format!(
            "negative media source target is invalid: {source_sample:?}"
        ));
    }
    if time_base_num <= 0 || time_base_den <= 0 {
        return Err(format!(
            "invalid FFmpeg time base {time_base_num}/{time_base_den} for source target {source_sample:?}"
        ));
    }
    let rate = Rational::new(time_base_den, time_base_num);
    let frame = source_sample
        .to_frame_position(rate)
        .map_err(|error| format!("source target cannot project to FFmpeg PTS: {error}"))?
        .frame;
    if frame < 0 {
        return Err(format!(
            "media source target precedes origin: {source_sample:?}"
        ));
    }
    Ok(frame)
}

fn duration_to_time_base_ticks(
    duration: TimelineTime,
    time_base_num: i64,
    time_base_den: i64,
) -> std::result::Result<i64, String> {
    if duration.is_negative() || time_base_num <= 0 || time_base_den <= 0 {
        return Err(format!(
            "invalid duration {duration} or FFmpeg time base {time_base_num}/{time_base_den}"
        ));
    }
    let rate = Rational::new(time_base_den, time_base_num);
    duration
        .to_frame_position(rate, FrameRounding::Ceil)
        .map(|position| position.frame)
        .map_err(|error| format!("duration cannot project to FFmpeg ticks: {error}"))
}

fn ffmpeg_source_time_arg(
    source_sample: SourceSampleTarget,
    stream_tb: ffmpeg::Rational,
) -> std::result::Result<String, String> {
    let relative_pts = source_sample_to_time_base_ticks(
        source_sample,
        i64::from(stream_tb.numerator()),
        i64::from(stream_tb.denominator()),
    )?;
    let numerator = relative_pts
        .checked_mul(i64::from(stream_tb.numerator()))
        .ok_or_else(|| format!("source seek time overflow: {source_sample:?}"))?;
    let exact = TimelineTime::new(numerator, i64::from(stream_tb.denominator()))
        .map_err(|error| format!("invalid source seek time: {error}"))?;
    let micros = SourceSampleTarget::covering(exact)
        .to_frame_position(Rational::new(i64::from(ffmpeg::ffi::AV_TIME_BASE), 1))
        .map_err(|error| format!("source seek time cannot project to microseconds: {error}"))?
        .frame;
    let seconds = micros / i64::from(ffmpeg::ffi::AV_TIME_BASE);
    let fractional = micros % i64::from(ffmpeg::ffi::AV_TIME_BASE);
    Ok(format!("{seconds}.{fractional:06}"))
}

struct DecodedVideoFrame {
    frame: ffmpeg::util::frame::video::Video,
    pts: Option<i64>,
}

enum DecodedVideoReceive {
    Frame(DecodedVideoFrame),
    NeedInput,
    EndOfStream,
}

fn receive_decoded_video_frame(
    decoder: &mut ffmpeg::decoder::Video,
    interrupt_state: &PreviewDecodeInterruptState,
    path: &Path,
) -> Result<DecodedVideoReceive> {
    interrupt_state.set_execution_stage(PreviewDecodeExecutionStage::CodecReceiveFrame);
    let mut decoded = ffmpeg::util::frame::video::Video::empty();
    match decoder.receive_frame(&mut decoded) {
        Ok(()) => {
            let mut pts = decoded.pts();
            if pts.is_none() {
                let best_effort = unsafe { (*decoded.as_ptr()).best_effort_timestamp };
                if best_effort != ffmpeg::ffi::AV_NOPTS_VALUE {
                    pts = Some(best_effort);
                }
            }

            Ok(DecodedVideoReceive::Frame(DecodedVideoFrame {
                frame: decoded,
                pts,
            }))
        }
        Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => {
            Ok(DecodedVideoReceive::NeedInput)
        }
        Err(ffmpeg::Error::Eof) => Ok(DecodedVideoReceive::EndOfStream),
        Err(error) => Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!("FFmpeg receive_frame failed: {error}"),
        }),
    }
}

#[cfg(test)]
mod tests;
