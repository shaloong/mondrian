//! 预览帧解码
//!
//! 使用 avformat_seek_file（安全 Rust API）定位到目标前的关键帧，
//! flush 解码器后向前解码到目标 PTS，保证返回精确帧。

use ffmpeg_next as ffmpeg;
use mondrian_core::{MondrianError, Result};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// 从关键帧向前解码的最大帧数安全限制
/// 提高到 1800（足以覆盖常见 2 分钟超长 GOP 文件，例如广播流）
const DECODE_BUDGET: usize = 1800;
const PREVIEW_FRAME_CACHE_CAPACITY: usize = 256;
const PREVIEW_HIT_TOLERANCE_SECS: f64 = 0.025;
const PREVIEW_MAX_SELECT_DISTANCE_SECS: f64 = 0.100;

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
        matches!(self, Self::PlaybackCursor)
    }
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

impl PreviewDecodePath {
    /// Stable path name for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InProcessFfmpegCpuRgba => "InProcessFfmpegCpuRgba",
            Self::ExternalFfmpegCpuRgba => "ExternalFfmpegCpuRgba",
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
}

impl PreviewDecodeDiagnostics {
    fn new(path: PreviewDecodePath) -> Self {
        Self {
            path,
            elapsed_us: 0,
            cache_hit: path == PreviewDecodePath::PreviewCacheHit,
            access_mode: PreviewDecodeAccessMode::RandomAccessStillFrame,
            external_process: path == PreviewDecodePath::ExternalFfmpegCpuRgba,
            cpu_resident: true,
            seek_performed: false,
            decoded_frame_count: 0,
            threading_kind: PreviewDecodeThreadingKind::None,
            threading_count: 0,
            stage_durations: PreviewDecodeStageDurations::default(),
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

    fn with_access_mode(mut self, access_mode: PreviewDecodeAccessMode) -> Self {
        self.access_mode = access_mode;
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

impl RgbaFrame {
    fn new(width: u32, height: u32, data: Vec<u8>, path: PreviewDecodePath) -> Self {
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

    fn with_threading(mut self, kind: PreviewDecodeThreadingKind, count: usize) -> Self {
        self.diagnostics.threading_kind = kind;
        self.diagnostics.threading_count = count.min(u32::MAX as usize) as u32;
        self
    }

    fn with_access_mode(mut self, access_mode: PreviewDecodeAccessMode) -> Self {
        self.diagnostics.access_mode = access_mode;
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
}

/// Decode the first video frame through the random-access still-frame path.
pub fn decode_first_still_frame_rgba(path: &Path) -> Result<RgbaFrame> {
    decode_still_frame_rgba_scaled(path, 0.0, None, None)
}

/// Decode one deterministic still frame as CPU RGBA8.
pub fn decode_still_frame_rgba(path: &Path, timestamp_secs: f64) -> Result<RgbaFrame> {
    decode_still_frame_rgba_scaled(path, timestamp_secs, None, None)
}

/// Decode one deterministic still frame as scaled CPU RGBA8.
pub fn decode_still_frame_rgba_scaled(
    path: &Path,
    timestamp_secs: f64,
    max_width: Option<u32>,
    max_height: Option<u32>,
) -> Result<RgbaFrame> {
    match decode_preview_rgba_frame_outcome(
        path,
        timestamp_secs,
        max_width,
        max_height,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        None,
        || false,
    )? {
        PreviewDecodeOutcome::Frame(frame) => Ok(frame),
        PreviewDecodeOutcome::Canceled => Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: "preview decode canceled unexpectedly".to_owned(),
        }),
    }
}

/// Decode a preview frame, allowing the caller to cancel stale interactive work.
///
/// Cancellation is cooperative. It is checked before expensive decode phases,
/// between packets, between received frames, and before software conversion.
/// External-process decode cannot be interrupted while the child process is
/// running, but a stale result is discarded before it is returned.
pub fn decode_still_frame_rgba_scaled_cancellable(
    path: &Path,
    timestamp_secs: f64,
    max_width: Option<u32>,
    max_height: Option<u32>,
    should_cancel: impl Fn() -> bool,
) -> Result<PreviewDecodeOutcome> {
    decode_preview_rgba_frame_outcome(
        path,
        timestamp_secs,
        max_width,
        max_height,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        None,
        should_cancel,
    )
}

/// Decode a preview frame using a caller-supplied file fingerprint.
///
/// App-level preview planning often already probes source/proxy metadata to
/// decide which media path to decode. Passing that fingerprint through avoids a
/// duplicate filesystem metadata lookup on the decode worker hot path while
/// preserving the same cache invalidation semantics as
/// [`decode_still_frame_rgba_scaled_cancellable`].
pub fn decode_still_frame_rgba_scaled_cancellable_with_fingerprint(
    path: &Path,
    timestamp_secs: f64,
    max_width: Option<u32>,
    max_height: Option<u32>,
    fingerprint: PreviewFileFingerprint,
    should_cancel: impl Fn() -> bool,
) -> Result<PreviewDecodeOutcome> {
    decode_preview_rgba_frame_outcome(
        path,
        timestamp_secs,
        max_width,
        max_height,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        Some(fingerprint),
        should_cancel,
    )
}

/// Decode one playback-cursor preview frame using a caller-supplied file fingerprint.
///
/// Playback cursor requests represent sustained timeline playback and forward
/// prefetch. Callers should use this entry point instead of passing a generic
/// access-mode flag so hardware decode, low-copy residency, deadline/drop
/// policy, and GPU input transforms can specialize behind a stable contract.
pub fn decode_playback_cursor_frame_rgba_scaled_cancellable_with_fingerprint(
    path: &Path,
    timestamp_secs: f64,
    max_width: Option<u32>,
    max_height: Option<u32>,
    fingerprint: PreviewFileFingerprint,
    should_cancel: impl Fn() -> bool,
) -> Result<PreviewDecodeOutcome> {
    decode_access_mode_rgba_scaled_cancellable_with_fingerprint(
        path,
        timestamp_secs,
        max_width,
        max_height,
        PreviewDecodeAccessMode::PlaybackCursor,
        fingerprint,
        should_cancel,
    )
}

/// Decode one playback-cursor preview frame.
///
/// Prefer
/// [`decode_playback_cursor_frame_rgba_scaled_cancellable_with_fingerprint`]
/// when the caller already resolved source/proxy file metadata.
pub fn decode_playback_cursor_frame_rgba_scaled_cancellable(
    path: &Path,
    timestamp_secs: f64,
    max_width: Option<u32>,
    max_height: Option<u32>,
    should_cancel: impl Fn() -> bool,
) -> Result<PreviewDecodeOutcome> {
    decode_access_mode_rgba_scaled_cancellable(
        path,
        timestamp_secs,
        max_width,
        max_height,
        PreviewDecodeAccessMode::PlaybackCursor,
        should_cancel,
    )
}

/// Decode one scrub-cursor preview frame using a caller-supplied file fingerprint.
///
/// Scrub cursor requests are latest-wins playhead navigation work. They must
/// favor cancellation and seek latency over forward queue warmth.
pub fn decode_scrub_cursor_frame_rgba_scaled_cancellable_with_fingerprint(
    path: &Path,
    timestamp_secs: f64,
    max_width: Option<u32>,
    max_height: Option<u32>,
    fingerprint: PreviewFileFingerprint,
    should_cancel: impl Fn() -> bool,
) -> Result<PreviewDecodeOutcome> {
    decode_access_mode_rgba_scaled_cancellable_with_fingerprint(
        path,
        timestamp_secs,
        max_width,
        max_height,
        PreviewDecodeAccessMode::ScrubCursor,
        fingerprint,
        should_cancel,
    )
}

/// Decode one scrub-cursor preview frame.
///
/// Prefer
/// [`decode_scrub_cursor_frame_rgba_scaled_cancellable_with_fingerprint`]
/// when the caller already resolved source/proxy file metadata.
pub fn decode_scrub_cursor_frame_rgba_scaled_cancellable(
    path: &Path,
    timestamp_secs: f64,
    max_width: Option<u32>,
    max_height: Option<u32>,
    should_cancel: impl Fn() -> bool,
) -> Result<PreviewDecodeOutcome> {
    decode_access_mode_rgba_scaled_cancellable(
        path,
        timestamp_secs,
        max_width,
        max_height,
        PreviewDecodeAccessMode::ScrubCursor,
        should_cancel,
    )
}

pub(crate) fn decode_access_mode_rgba_scaled_cancellable_with_fingerprint(
    path: &Path,
    timestamp_secs: f64,
    max_width: Option<u32>,
    max_height: Option<u32>,
    access_mode: PreviewDecodeAccessMode,
    fingerprint: PreviewFileFingerprint,
    should_cancel: impl Fn() -> bool,
) -> Result<PreviewDecodeOutcome> {
    decode_preview_rgba_frame_outcome(
        path,
        timestamp_secs,
        max_width,
        max_height,
        access_mode,
        Some(fingerprint),
        should_cancel,
    )
}

pub(crate) fn decode_access_mode_rgba_scaled_cancellable(
    path: &Path,
    timestamp_secs: f64,
    max_width: Option<u32>,
    max_height: Option<u32>,
    access_mode: PreviewDecodeAccessMode,
    should_cancel: impl Fn() -> bool,
) -> Result<PreviewDecodeOutcome> {
    decode_preview_rgba_frame_outcome(
        path,
        timestamp_secs,
        max_width,
        max_height,
        access_mode,
        None,
        should_cancel,
    )
}

/// Result of a cancellable preview decode request.
#[derive(Debug, Clone)]
pub enum PreviewDecodeOutcome {
    /// Decode completed with a CPU RGBA preview frame.
    Frame(RgbaFrame),
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

struct PreviewDecodeSession {
    path: PathBuf,
    fingerprint: PreviewFileFingerprint,
    max_width: Option<u32>,
    max_height: Option<u32>,
    backend: PreviewDecodeBackend,
    input: ffmpeg::format::context::Input,
    decoder: ffmpeg::decoder::Video,
    scaler: ffmpeg::software::scaling::Context,
    stream_index: usize,
    stream_tb: ffmpeg::Rational,
    frame_duration_pts: i64,
    hit_tolerance_pts: i64,
    target_width: u32,
    target_height: u32,
    threading_kind: PreviewDecodeThreadingKind,
    threading_count: usize,
    last_pts: Option<i64>,
    reached_eof: bool,
}

struct PreviewDecodeForwardResult {
    frame: Option<RgbaFrame>,
    decoded_frame_count: usize,
    canceled: bool,
}

impl PreviewDecodeForwardResult {
    fn frame(frame: RgbaFrame, decoded_frame_count: usize) -> Self {
        Self {
            frame: Some(frame),
            decoded_frame_count,
            canceled: false,
        }
    }

    fn empty(decoded_frame_count: usize) -> Self {
        Self { frame: None, decoded_frame_count, canceled: false }
    }

    fn canceled(decoded_frame_count: usize) -> Self {
        Self { frame: None, decoded_frame_count, canceled: true }
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

impl PreviewDecodeSession {
    fn open(
        path: &Path,
        fingerprint: PreviewFileFingerprint,
        max_width: Option<u32>,
        max_height: Option<u32>,
        backend: PreviewDecodeBackend,
    ) -> Result<Self> {
        let input = ffmpeg::format::input(path).map_err(|e| MondrianError::MediaOpen {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
        Self::from_input(input, path, fingerprint, max_width, max_height, backend)
    }

    fn from_input(
        input: ffmpeg::format::context::Input,
        path: &Path,
        fingerprint: PreviewFileFingerprint,
        max_width: Option<u32>,
        max_height: Option<u32>,
        backend: PreviewDecodeBackend,
    ) -> Result<Self> {
        let (stream_index, parameters, stream_tb, stream_rate) = {
            let stream = input.streams().best(ffmpeg::media::Type::Video).ok_or_else(|| {
                MondrianError::UnsupportedFormat { format: "no video stream".to_string() }
            })?;
            (
                stream.index(),
                stream.parameters(),
                stream.time_base(),
                stream.rate(),
            )
        };

        let mut context =
            ffmpeg::codec::context::Context::from_parameters(parameters).map_err(|e| {
                MondrianError::DecodeFailed {
                    asset_id: path.display().to_string(),
                    reason: e.to_string(),
                }
            })?;
        let requested_threading = preview_decode_threading_config();
        let ffmpeg_threading = ffmpeg::codec::threading::Config {
            kind: requested_threading.kind.to_ffmpeg(),
            count: requested_threading.count,
        };
        context.set_threading(ffmpeg_threading);
        let decoder = context.decoder().video().map_err(|e| MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: e.to_string(),
        })?;
        let active_threading = decoder.threading();
        let threading_kind = PreviewDecodeThreadingKind::from_ffmpeg(active_threading.kind);
        let threading_count = active_threading.count;

        let (target_width, target_height) =
            fit_target_size(decoder.width(), decoder.height(), max_width, max_height);

        let scaler = ffmpeg::software::scaling::Context::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            ffmpeg::util::format::pixel::Pixel::RGBA,
            target_width,
            target_height,
            ffmpeg::software::scaling::flag::Flags::FAST_BILINEAR,
        )
        .map_err(|e| MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: e.to_string(),
        })?;

        let frame_duration_pts = estimate_frame_duration_pts(stream_tb, stream_rate).max(1);
        let max_hit_tolerance_pts = seconds_to_stream_pts(PREVIEW_HIT_TOLERANCE_SECS, stream_tb);
        let hit_tolerance_pts = (frame_duration_pts / 2).max(1).min(max_hit_tolerance_pts.max(1));

        Ok(Self {
            path: path.to_path_buf(),
            fingerprint,
            max_width,
            max_height,
            backend,
            input,
            decoder,
            scaler,
            stream_index,
            stream_tb,
            frame_duration_pts,
            hit_tolerance_pts,
            target_width,
            target_height,
            threading_kind,
            threading_count,
            last_pts: None,
            reached_eof: false,
        })
    }

    fn matches(
        &self,
        path: &Path,
        fingerprint: PreviewFileFingerprint,
        max_width: Option<u32>,
        max_height: Option<u32>,
        backend: PreviewDecodeBackend,
    ) -> bool {
        self.path == path
            && self.fingerprint == fingerprint
            && self.max_width == max_width
            && self.max_height == max_height
            && self.backend == backend
    }

    fn decode_at(
        &mut self,
        timestamp_secs: f64,
        access_mode: PreviewDecodeAccessMode,
        should_cancel: &impl Fn() -> bool,
    ) -> Result<PreviewDecodeOutcome> {
        if should_cancel() {
            return Ok(PreviewDecodeOutcome::Canceled);
        }
        let target_pts = timestamp_to_stream_pts(timestamp_secs, self.stream_tb);

        let cache_lookup_started_at = Instant::now();
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
            return Ok(PreviewDecodeOutcome::Frame(
                hit.frame
                    .into_cache_hit(cache_lookup_started_at.elapsed(), access_mode)
                    .with_stage_durations(PreviewDecodeStageDurations {
                        cache_lookup_us: duration_us(cache_lookup_started_at.elapsed()),
                        ..PreviewDecodeStageDurations::default()
                    }),
            ));
        }
        let cache_lookup_us = duration_us(cache_lookup_started_at.elapsed());

        let should_continue_forward = self
            .last_pts
            .map(|last| {
                target_pts >= last
                    && target_pts.saturating_sub(last) <= self.frame_duration_pts.saturating_mul(24)
                    && !self.reached_eof
            })
            .unwrap_or(false);

        let seek_performed = !should_continue_forward;
        let mut seek_us = 0;
        if seek_performed {
            if should_cancel() {
                return Ok(PreviewDecodeOutcome::Canceled);
            }
            let seek_started_at = Instant::now();
            self.seek_to_target(target_pts)?;
            seek_us = duration_us(seek_started_at.elapsed());
        }

        if should_cancel() {
            return Ok(PreviewDecodeOutcome::Canceled);
        }
        let decode_started_at = Instant::now();
        let result = self.decode_forward_until(target_pts, should_cancel)?;
        if result.canceled {
            return Ok(PreviewDecodeOutcome::Canceled);
        }
        if let Some(frame) = result.frame {
            let conversion_us = frame
                .diagnostics
                .stage_durations
                .swscale_us
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
                    .with_threading(self.threading_kind, self.threading_count),
            ));
        }

        Err(MondrianError::DecodeFailed {
            asset_id: self.path.display().to_string(),
            reason: "no decodable frame".to_string(),
        })
    }

    fn seek_to_target(&mut self, target_pts: i64) -> Result<()> {
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

        let (min_ts, max_ts, seek_flags) = if preview_fast_any_seek_enabled() {
            let seek_window_pts = (2.0 / tb_secs).round().max(1.0) as i64;
            (
                target_pts.saturating_sub(seek_window_pts),
                target_pts.saturating_add(seek_window_pts),
                ffmpeg::ffi::AVSEEK_FLAG_ANY,
            )
        } else {
            // 关键帧安全模式：不限制 backward seek 范围，避免长 GOP 时落到不可独立解码帧。
            (i64::MIN, target_pts, ffmpeg::ffi::AVSEEK_FLAG_BACKWARD)
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
            return Ok(());
        }

        Err(MondrianError::DecodeFailed {
            asset_id: self.path.display().to_string(),
            reason: format!("seek failed with code {ret}"),
        })
    }

    fn decode_forward_until(
        &mut self,
        target_pts: i64,
        should_cancel: &impl Fn() -> bool,
    ) -> Result<PreviewDecodeForwardResult> {
        let mut best_before: Option<(i64, ffmpeg::util::frame::video::Video)> = None;
        let mut best_after: Option<(i64, ffmpeg::util::frame::video::Video)> = None;
        let mut frames_decoded: usize = 0;
        let max_select_distance_pts =
            self.frame_duration_pts.saturating_mul(2).max(1).min(
                seconds_to_stream_pts(PREVIEW_MAX_SELECT_DISTANCE_SECS, self.stream_tb).max(1),
            );

        let mut choose_and_convert = |before: Option<&(i64, ffmpeg::util::frame::video::Video)>,
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
            let rgba =
                convert_decoded_to_rgba(selected_frame, &mut self.scaler, self.path.as_path())?;
            Ok(Some((selected_pts, rgba)))
        };

        for (s, packet) in self.input.packets() {
            if should_cancel() {
                return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
            }
            if s.index() != self.stream_index {
                continue;
            }
            if frames_decoded >= DECODE_BUDGET {
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
                            let rgba = convert_decoded_to_rgba(
                                &decoded.frame,
                                &mut self.scaler,
                                &self.path,
                            )?;
                            preview_cache_put_with_fingerprint(
                                &self.path,
                                self.fingerprint,
                                self.target_width,
                                self.target_height,
                                frame_pts,
                                rgba.clone(),
                            );
                            return Ok(PreviewDecodeForwardResult::frame(rgba, frames_decoded));
                        }
                    } else {
                        best_after = Some((frame_pts, decoded.frame.clone()));
                        if let Some((selected_pts, rgba)) =
                            choose_and_convert(best_before.as_ref(), best_after.as_ref())?
                        {
                            preview_cache_put_with_fingerprint(
                                &self.path,
                                self.fingerprint,
                                self.target_width,
                                self.target_height,
                                selected_pts,
                                rgba.clone(),
                            );
                            return Ok(PreviewDecodeForwardResult::frame(rgba, frames_decoded));
                        }
                    }
                }

                if frames_decoded >= DECODE_BUDGET {
                    break;
                }
            }

            if frames_decoded >= DECODE_BUDGET {
                break;
            }
        }

        if !self.reached_eof {
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
                        if let Some((selected_pts, rgba)) =
                            choose_and_convert(best_before.as_ref(), best_after.as_ref())?
                        {
                            preview_cache_put_with_fingerprint(
                                &self.path,
                                self.fingerprint,
                                self.target_width,
                                self.target_height,
                                selected_pts,
                                rgba.clone(),
                            );
                            self.reached_eof = true;
                            return Ok(PreviewDecodeForwardResult::frame(rgba, frames_decoded));
                        }
                    }
                }

                if frames_decoded >= DECODE_BUDGET {
                    break;
                }
            }

            self.reached_eof = true;
        }

        if let Some((selected_pts, rgba)) =
            choose_and_convert(best_before.as_ref(), best_after.as_ref())?
        {
            preview_cache_put_with_fingerprint(
                &self.path,
                self.fingerprint,
                self.target_width,
                self.target_height,
                selected_pts,
                rgba.clone(),
            );
            return Ok(PreviewDecodeForwardResult::frame(rgba, frames_decoded));
        }

        Ok(PreviewDecodeForwardResult::empty(frames_decoded))
    }
}

fn decode_preview_rgba_frame_outcome(
    path: &Path,
    timestamp_secs: f64,
    max_width: Option<u32>,
    max_height: Option<u32>,
    access_mode: PreviewDecodeAccessMode,
    fingerprint: Option<PreviewFileFingerprint>,
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
            .map(|session| session.matches(path, fingerprint, max_width, max_height, backend))
            .unwrap_or(false);

        if !current_match {
            let open_started_at = Instant::now();
            *slot = Some(PreviewDecodeSession::open(
                path,
                fingerprint,
                max_width,
                max_height,
                backend,
            )?);
            session_open_us = duration_us(open_started_at.elapsed());
        }

        let session = slot.as_mut().expect("preview decode session must exist");
        let mut external_process_us = 0;

        if preview_external_ffmpeg_cpu_rgba_enabled() {
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
                            *slot = None;
                            return Ok(PreviewDecodeOutcome::Canceled);
                        }
                        return Ok(PreviewDecodeOutcome::Frame(frame
                            .with_access_mode(access_mode)
                            .with_stage_durations(PreviewDecodeStageDurations {
                                session_open_us,
                                external_process_us,
                                ..PreviewDecodeStageDurations::default()
                            })
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

        let outcome = session.decode_at(timestamp_secs, access_mode, &should_cancel)?;
        match outcome {
            PreviewDecodeOutcome::Frame(frame) => Ok(PreviewDecodeOutcome::Frame(
                frame
                    .with_access_mode(access_mode)
                    .with_stage_durations(PreviewDecodeStageDurations {
                        session_open_us,
                        external_process_us,
                        ..PreviewDecodeStageDurations::default()
                    })
                    .with_elapsed(started_at.elapsed()),
            )),
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

fn preview_fast_any_seek_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("MONDRIAN_PREVIEW_FAST_ANY_SEEK")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

fn preview_decode_threading_config() -> PreviewDecodeThreadingConfig {
    let kind = std::env::var("MONDRIAN_PREVIEW_DECODE_THREADING")
        .ok()
        .and_then(|value| PreviewDecodeThreadingKind::from_env(&value))
        .unwrap_or_default();
    let count = std::env::var("MONDRIAN_PREVIEW_DECODE_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_else(default_preview_decode_thread_count);
    PreviewDecodeThreadingConfig { kind, count }
}

fn default_preview_decode_thread_count() -> usize {
    std::thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(1)
        .saturating_sub(2)
        .clamp(1, 8)
}

#[derive(Clone)]
struct PreviewCacheHit {
    frame: RgbaFrame,
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
    let hit = PreviewCacheHit { frame: entry.frame.clone() };
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

fn preview_external_ffmpeg_cpu_rgba_enabled() -> bool {
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

fn ensure_ffmpeg_initialized(path: &Path) -> Result<()> {
    static INIT_RESULT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    let init = INIT_RESULT.get_or_init(|| {
        let log_level = std::env::var("MONDRIAN_FFMPEG_LOG_LEVEL")
            .ok()
            .map(|value| value.to_ascii_lowercase())
            .and_then(|value| match value.as_str() {
                "quiet" => Some(ffmpeg::ffi::AV_LOG_QUIET),
                "panic" => Some(ffmpeg::ffi::AV_LOG_PANIC),
                "fatal" => Some(ffmpeg::ffi::AV_LOG_FATAL),
                "error" => Some(ffmpeg::ffi::AV_LOG_ERROR),
                "warning" => Some(ffmpeg::ffi::AV_LOG_WARNING),
                "info" => Some(ffmpeg::ffi::AV_LOG_INFO),
                "verbose" => Some(ffmpeg::ffi::AV_LOG_VERBOSE),
                "debug" => Some(ffmpeg::ffi::AV_LOG_DEBUG),
                "trace" => Some(ffmpeg::ffi::AV_LOG_TRACE),
                _ => None,
            })
            .unwrap_or(ffmpeg::ffi::AV_LOG_ERROR);

        unsafe {
            ffmpeg::ffi::av_log_set_level(log_level);
        }

        ffmpeg::init().map_err(|e| format!("{e}"))
    });
    match init {
        Ok(()) => Ok(()),
        Err(reason) => Err(MondrianError::MediaOpen {
            path: path.display().to_string(),
            reason: format!("ffmpeg init failed: {reason}"),
        }),
    }
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

#[cfg(test)]
mod tests {
    use super::{
        clear_global_preview_frame_cache, clear_thread_local_preview_decode_session,
        decode_playback_cursor_frame_rgba_scaled_cancellable_with_fingerprint,
        decode_still_frame_rgba_scaled, decode_still_frame_rgba_scaled_cancellable, duration_us,
        preview_cache_get, preview_cache_put_with_fingerprint, PreviewDecodeAccessMode,
        PreviewDecodeBackend, PreviewDecodeOutcome, PreviewDecodePath, PreviewDecodeStageDurations,
        PreviewDecodeThreadingKind, PreviewFileFingerprint, RgbaFrame,
    };
    use serde::Serialize;
    use std::path::PathBuf;
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
    }

    #[test]
    fn preview_decode_stage_durations_accumulate_saturating() {
        let mut durations = PreviewDecodeStageDurations {
            session_open_us: u64::MAX,
            cache_lookup_us: 2,
            seek_us: 3,
            packet_decode_us: 4,
            swscale_us: 5,
            rgba_copy_us: 6,
            external_process_us: 7,
        };

        durations.accumulate(PreviewDecodeStageDurations {
            session_open_us: 1,
            cache_lookup_us: 20,
            seek_us: 30,
            packet_decode_us: 40,
            swscale_us: 50,
            rgba_copy_us: 60,
            external_process_us: 70,
        });

        assert_eq!(durations.session_open_us, u64::MAX);
        assert_eq!(durations.cache_lookup_us, 22);
        assert_eq!(durations.seek_us, 33);
        assert_eq!(durations.packet_decode_us, 44);
        assert_eq!(durations.swscale_us, 55);
        assert_eq!(durations.rgba_copy_us, 66);
        assert_eq!(durations.external_process_us, 77);
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
        assert_eq!(
            frame.diagnostics.threading_kind,
            PreviewDecodeThreadingKind::None
        );
        assert_eq!(frame.diagnostics.threading_count, 0);
        assert_eq!(
            frame.diagnostics.access_mode,
            PreviewDecodeAccessMode::RandomAccessStillFrame
        );

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
    fn cancellable_preview_decode_returns_canceled_before_opening_missing_file() {
        let outcome = decode_still_frame_rgba_scaled_cancellable(
            &PathBuf::from("E:/definitely-missing/canceled-preview.mov"),
            0.0,
            Some(320),
            Some(180),
            || true,
        )
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
        let frame = decode_still_frame_rgba_scaled(&path, timestamp_secs, max_width, max_height)
            .expect("decode preview fixture");
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
            let frame = match decode_playback_cursor_frame_rgba_scaled_cancellable_with_fingerprint(
                &path,
                timestamp_secs,
                max_width,
                max_height,
                fingerprint,
                || false,
            )
            .expect("decode preview fixture frame")
            {
                PreviewDecodeOutcome::Frame(frame) => frame,
                PreviewDecodeOutcome::Canceled => {
                    panic!("playback sequence perf decode canceled")
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
            max_us,
            uncached_frame_count,
            uncached_avg_us: average_us(uncached_total_us, uncached_frame_count),
            uncached_max_us,
            total_stage_durations,
            max_frame_stage_durations,
            frames,
        };
        let json = serde_json::to_string(&report).expect("serialize sequence decode perf report");
        eprintln!(
            "MONDRIAN_PREVIEW_DECODE_SEQUENCE_PERF_SUMMARY path=\"{}\" access_mode={} frames={} avg_us={} max_us={} uncached_frames={} uncached_avg_us={} uncached_max_us={} packet_decode_us={} swscale_us={} rgba_copy_us={}",
            report.path,
            report.access_mode,
            report.frame_count,
            report.avg_us,
            report.max_us,
            report.uncached_frame_count,
            report.uncached_avg_us,
            report.uncached_max_us,
            report.total_stage_durations.packet_decode_us,
            report.total_stage_durations.swscale_us,
            report.total_stage_durations.rgba_copy_us,
        );
        eprintln!("MONDRIAN_PREVIEW_DECODE_SEQUENCE_PERF_JSON={json}");
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
            swscale_us: lhs.swscale_us.max(rhs.swscale_us),
            rgba_copy_us: lhs.rgba_copy_us.max(rhs.rgba_copy_us),
            external_process_us: lhs.external_process_us.max(rhs.external_process_us),
        }
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
        max_us: u64,
        uncached_frame_count: usize,
        uncached_avg_us: u64,
        uncached_max_us: u64,
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
