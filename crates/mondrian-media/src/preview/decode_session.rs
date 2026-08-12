//! Worker-family-specific FFmpeg Preview decode sessions.
//!
//! This deep module owns format/codec setup, request-scoped interrupt state,
//! stream discovery, seek/index use, packet/codec backpressure, forward reuse,
//! typed frame materialization, and final cancellation publication. The
//! parent module exposes only the request/outcome contract and session reset.

use super::demux_source::{
    PreviewDemuxWorkerConfig, PreviewPacketRead, PreviewPacketSeek, PreviewPacketSource,
    PreviewPacketSourceOpen, PreviewPacketSourceOpenError,
};
use super::native_frame::{PreviewDecodeSessionOutputLease, PreviewNativeOutputTracker};
use super::*;

thread_local! {
    static THREAD_PREVIEW_DECODE_CONTEXT: RefCell<PreviewDecodeSessionContext> = {
        RefCell::new(PreviewDecodeSessionContext::new())
    };
}

/// Reusable resources explicitly owned by one Preview decode worker family.
///
/// Clone this value only between workers in the same scheduling family.
/// Preview, Thumbnail, and Export families should receive distinct owners so
/// their cache policy and hardware-residency pressure remain independently
/// governable.
#[derive(Clone, Debug, Default)]
pub struct PreviewDecodeWorkerResources {
    seek_index_cache: PreviewSeekIndexCache,
    hardware_device_contexts: HwDeviceContextPool,
    native_outputs: PreviewNativeOutputTracker,
}

impl PreviewDecodeWorkerResources {
    /// Construct a worker-family resource owner from explicit cache and device pools.
    pub fn new(
        seek_index_cache: PreviewSeekIndexCache,
        hardware_device_contexts: HwDeviceContextPool,
    ) -> Self {
        Self {
            seek_index_cache,
            hardware_device_contexts,
            native_outputs: PreviewNativeOutputTracker::default(),
        }
    }

    /// Shared seek-index cache for this worker family.
    pub fn seek_index_cache(&self) -> &PreviewSeekIndexCache {
        &self.seek_index_cache
    }

    /// Shared hardware-device context pool for this worker family.
    pub fn hardware_device_context_pool(&self) -> &HwDeviceContextPool {
        &self.hardware_device_contexts
    }

    /// Exact number of logical native outputs still owned anywhere in this
    /// worker family, including outputs whose originating Session was dropped.
    pub fn outstanding_native_output_count(&self) -> usize {
        self.native_outputs.outstanding()
    }

    fn native_output_tracker(&self) -> &PreviewNativeOutputTracker {
        &self.native_outputs
    }
}

/// Explicit owner of worker-family-specific FFmpeg Preview sessions.
///
/// Production schedulers should create one context per decode worker and keep
/// it on that worker thread. This makes codec, DPB, and hardware-surface-pool
/// residency follow the worker lifecycle instead of depending on an implicit
/// thread-local cache. The top-level convenience decode function retains a
/// thread-local context only for standalone compatibility callers and tests;
/// production Preview, Thumbnail, and Export workers own explicit contexts.
pub struct PreviewDecodeSessionContext {
    sessions: PreviewDecodeSessions,
    execution_observer: PreviewDecodeExecutionObserver,
    demux_worker: Option<PreviewDemuxWorkerConfig>,
    resources: PreviewDecodeWorkerResources,
}

/// Thread-safe construction authority for a Preview decode context.
///
/// The bootstrap contains no codec, demux, DPB, or frame-pool state and may
/// cross a worker-thread seam. Injected family resources may retain immutable,
/// FFmpeg-refcounted hardware device roots. It deliberately does not implement
/// `Clone`: concurrently built contexts need independent observers. Consuming
/// the bootstrap constructs the non-`Send` Session context on its owner thread.
pub struct PreviewDecodeSessionContextBootstrap {
    execution_observer: PreviewDecodeExecutionObserver,
    demux_worker: Option<PreviewDemuxWorkerConfig>,
    resources: PreviewDecodeWorkerResources,
}

impl PreviewDecodeSessionContextBootstrap {
    /// Copy this construction authority for sequential worker recovery.
    ///
    /// The context built from the returned authority shares the same execution
    /// observer and worker-family resources. Call this only after the context
    /// built from the original authority has stopped executing; it must never
    /// create concurrent stage writers.
    pub fn clone_for_sequential_recovery(&self) -> Self {
        Self {
            execution_observer: self.execution_observer.clone(),
            demux_worker: self.demux_worker.clone(),
            resources: self.resources.clone(),
        }
    }

    /// Supply resources shared by the decode workers in this scheduling family.
    pub fn with_worker_resources(mut self, resources: PreviewDecodeWorkerResources) -> Self {
        self.resources = resources;
        self
    }

    /// Construct the worker-owned context on the current thread.
    pub fn build(self) -> PreviewDecodeSessionContext {
        PreviewDecodeSessionContext::with_execution_observer(
            self.execution_observer,
            self.demux_worker,
            self.resources,
        )
    }
}

impl PreviewDecodeSessionContext {
    /// Create an empty worker-local decode context.
    pub fn new() -> Self {
        Self::with_execution_observer(
            PreviewDecodeExecutionObserver::new(),
            None,
            PreviewDecodeWorkerResources::default(),
        )
    }

    /// Create an empty worker-local context with explicit family resources.
    pub fn with_worker_resources(resources: PreviewDecodeWorkerResources) -> Self {
        Self::with_execution_observer(PreviewDecodeExecutionObserver::new(), None, resources)
    }

    /// Create a worker bootstrap without retaining its observer.
    pub fn bootstrap() -> PreviewDecodeSessionContextBootstrap {
        PreviewDecodeSessionContextBootstrap {
            execution_observer: PreviewDecodeExecutionObserver::new(),
            demux_worker: None,
            resources: PreviewDecodeWorkerResources::default(),
        }
    }

    /// Create a worker bootstrap together with its read-only observer.
    ///
    /// The bootstrap is consumed on the worker thread. Its explicit sequential
    /// recovery copy may replace that context only after the previous context
    /// stops executing; callers must not create concurrent stage writers for
    /// the same evidence stream. The returned observer remains independently
    /// cloneable for diagnostics.
    pub fn observed_bootstrap() -> (
        PreviewDecodeSessionContextBootstrap,
        PreviewDecodeExecutionObserver,
    ) {
        let observer = PreviewDecodeExecutionObserver::new();
        (
            PreviewDecodeSessionContextBootstrap {
                execution_observer: observer.clone(),
                demux_worker: None,
                resources: PreviewDecodeWorkerResources::default(),
            },
            observer,
        )
    }

    /// Create an observed production bootstrap that isolates exact-still
    /// container I/O in the packaged Mondrian executable.
    ///
    /// The executable must dispatch `--internal-demux-worker-v2` before
    /// starting UI state. Codec and GPU resources remain in this context's
    /// owner thread; only FFmpeg format operations cross the process seam.
    pub fn observed_bootstrap_with_demux_worker(
        executable: PathBuf,
    ) -> (
        PreviewDecodeSessionContextBootstrap,
        PreviewDecodeExecutionObserver,
    ) {
        let observer = PreviewDecodeExecutionObserver::new();
        (
            PreviewDecodeSessionContextBootstrap {
                execution_observer: observer.clone(),
                demux_worker: Some(PreviewDemuxWorkerConfig::new(executable, observer.clone())),
                resources: PreviewDecodeWorkerResources::default(),
            },
            observer,
        )
    }

    fn with_execution_observer(
        execution_observer: PreviewDecodeExecutionObserver,
        demux_worker: Option<PreviewDemuxWorkerConfig>,
        resources: PreviewDecodeWorkerResources,
    ) -> Self {
        Self {
            sessions: PreviewDecodeSessions { playback: None, interactive: None, cpu_still: None },
            execution_observer,
            demux_worker,
            resources,
        }
    }

    /// Return the reusable resources owned by this context's worker family.
    pub fn worker_resources(&self) -> &PreviewDecodeWorkerResources {
        &self.resources
    }

    /// Release all codec sessions and their owned decode resources.
    pub fn clear(&mut self) {
        if !self.sessions.is_empty() {
            self.execution_observer
                .publish_stage(PreviewDecodeExecutionStage::SessionRetire);
            self.sessions.clear();
            self.execution_observer.finish_idle();
        }
        self.resources.hardware_device_contexts.release_idle();
    }

    /// Whether every decoder-native output issued by this context is released.
    ///
    /// A scheduler must prove this before acknowledging a decoder-residency
    /// family retirement; dropping the codec owner alone does not revoke frame
    /// references already published to a completion queue or renderer.
    pub fn native_outputs_released(&self) -> bool {
        self.resources.native_outputs.is_released()
    }

    /// Number of live decoder Sessions owned by this context.
    ///
    /// This lightweight lifecycle fact lets worker/job owners prove that an
    /// explicit retirement boundary released codec, DPB, demux, and
    /// hardware-surface-pool residency. It does not expose decoder internals.
    pub fn resident_session_count(&self) -> usize {
        self.sessions.resident_session_count()
    }

    /// Decode one request using sessions explicitly owned by this context.
    pub fn decode_cancellable(
        &mut self,
        request: PreviewDecodeRequest<'_>,
        should_cancel: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<PreviewDecodeOutcome> {
        let _execution = self.execution_observer.begin_request();
        let should_cancel: PreviewDecodeCancelProbe = Arc::new(should_cancel);
        decode_preview_frame_outcome_in_sessions(
            &mut self.sessions,
            &self.execution_observer,
            request,
            &self.resources,
            self.demux_worker.as_ref(),
            should_cancel,
        )
    }
}

impl Default for PreviewDecodeSessionContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Drop the current thread's cached preview decode sessions.
///
/// Preview playback has an independent FFmpeg session; latest-wins Viewer
/// scrub and GPU-resident still requests share one interactive session while
/// CPU still extraction remains physically separate. Call this at explicit
/// lifecycle boundaries, such as perf probes, project/media shutdown, or tests
/// that intentionally open threaded software decoders.
pub fn clear_thread_local_preview_decode_session() {
    THREAD_PREVIEW_DECODE_CONTEXT.with(|context| {
        context.borrow_mut().clear();
    });
}

struct PreviewDecodeSessions {
    playback: Option<PreviewDecodeSession>,
    /// Shared latest-wins Viewer session. Scrub and exact Still have distinct
    /// seek policies but never need simultaneous codec/DPB residency.
    interactive: Option<PreviewDecodeSession>,
    cpu_still: Option<PreviewDecodeSession>,
}

impl PreviewDecodeSessions {
    fn is_empty(&self) -> bool {
        self.playback.is_none() && self.interactive.is_none() && self.cpu_still.is_none()
    }

    fn available_slot(
        &self,
        access_mode: PreviewDecodeAccessMode,
        hardware_decode_request: PreviewHardwareDecodeRequest,
    ) -> Option<PreviewDecodeSessionSlot> {
        let slot = match access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => PreviewDecodeSessionSlot::Playback,
            PreviewDecodeAccessMode::ScrubCursor => PreviewDecodeSessionSlot::Interactive,
            PreviewDecodeAccessMode::RandomAccessStillFrame
                if hardware_decode_request.prefers_gpu_residency() =>
            {
                PreviewDecodeSessionSlot::Interactive
            }
            PreviewDecodeAccessMode::RandomAccessStillFrame => PreviewDecodeSessionSlot::CpuStill,
        };
        if !slot.requires_released_native_outputs() {
            return Some(slot);
        }
        self.interactive
            .as_ref()
            .is_none_or(PreviewDecodeSession::native_output_released)
            .then_some(slot)
    }

    fn slot_mut(&mut self, slot: PreviewDecodeSessionSlot) -> &mut Option<PreviewDecodeSession> {
        match slot {
            PreviewDecodeSessionSlot::Playback => &mut self.playback,
            PreviewDecodeSessionSlot::Interactive => &mut self.interactive,
            PreviewDecodeSessionSlot::CpuStill => &mut self.cpu_still,
        }
    }

    fn resident_session_count(&self) -> usize {
        [&self.playback, &self.interactive, &self.cpu_still]
            .into_iter()
            .filter(|session| session.is_some())
            .count()
    }

    fn clear(&mut self) {
        self.playback = None;
        self.interactive = None;
        self.cpu_still = None;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewDecodeSessionSlot {
    Playback,
    Interactive,
    CpuStill,
}

impl PreviewDecodeSessionSlot {
    fn requires_released_native_outputs(self) -> bool {
        self == Self::Interactive
    }
}

use hardware_decode::{
    preview_hardware_decode_get_format, preview_hardware_frame_format,
    PreviewHardwareDecodeContextState, PreviewHardwareDecodePlan,
};

struct PreviewDecodeSession {
    path: PathBuf,
    fingerprint: MediaFileFingerprint,
    requested_video_stream_index: Option<u32>,
    max_width: Option<u32>,
    max_height: Option<u32>,
    backend: PreviewDecodeBackend,
    hardware_decode_request: PreviewHardwareDecodeRequest,
    hardware_decode_device_selector: Option<HwAccelDeviceSelector>,
    source_color: PreviewSourceColorContract,
    codec_id: ffmpeg::codec::Id,
    packet_source: PreviewPacketSource,
    // Declared after `packet_source` so a direct AVFormatContext releases its callback use
    // before the callback state is dropped.
    interrupt_state: Arc<PreviewDecodeInterruptState>,
    // These decoder-dependent frame owners must be declared before `decoder`
    // and its hardware contexts. Rust drops struct fields in declaration order;
    // releasing every retained AVFrame/native surface first keeps the codec,
    // DPB, surface pool, and device alive while FFmpeg unrefs those frames.
    // Relying on Session retirement call sites to clear them is insufficient:
    // error unwinding and ordinary Option replacement must have the same order.
    /// Decoded candidate selected by the previous request.
    last_decoded_frame: Option<RetainedDecodedCandidate>,
    /// First decoded successor retained to prove the selected frame's exclusive
    /// presentation boundary and seed the next forward request.
    next_decoded_frame: Option<RetainedDecodedCandidate>,
    playback_ring: PreviewPlaybackRing,
    decoder: ffmpeg::decoder::Video,
    scaler: Option<ffmpeg::software::scaling::Context>,
    scaler_source_format: Option<ffmpeg::util::format::pixel::Pixel>,
    /// Last color contract applied to the retained scaler. Per-frame
    /// reconfiguration is skipped while the contract is unchanged, which is
    /// the ordinary case inside one session.
    scaler_color_contract: Option<DecodedRgbaFrameContract>,
    stream_index: usize,
    stream_tb: ffmpeg::Rational,
    /// Absolute stream PTS representing media-source-local time zero.
    stream_start_pts: i64,
    frame_duration_pts: i64,
    target_width: u32,
    target_height: u32,
    _hardware_decode_context_state: Option<Box<PreviewHardwareDecodeContextState>>,
    hardware_device_context: Option<HwAccelDeviceContext>,
    threading_kind: PreviewDecodeThreadingKind,
    threading_count: usize,
    hardware_decode_plan: PreviewHardwareDecodePlan,
    decoded_surface_format: DecodedVideoSurfaceFormat,
    family_native_outputs: PreviewNativeOutputTracker,
    session_native_outputs: PreviewNativeOutputTracker,
    /// Monotonic maximum decoded presentation timestamp since the last seek.
    last_pts: Option<i64>,
    duplicate_decoded_pts: Option<i64>,
    reached_eof: bool,
    seek_index: PreviewSeekIndex,
}

impl Drop for PreviewDecodeSession {
    fn drop(&mut self) {
        // Drop runs before fields are destroyed. Clear every private AVFrame and
        // native-frame owner explicitly so this invariant remains correct even
        // if a future refactor accidentally changes field declaration order.
        self.last_decoded_frame = None;
        self.next_decoded_frame = None;
        self.playback_ring.clear();
    }
}

struct PreviewDecodeForwardResult {
    frame: Option<PreviewDecodedFramePayload>,
    selected_extent: Option<DecodedTemporalExtent>,
    retained_selected_frame: Option<RetainedDecodedCandidate>,
    retained_next_frame: Option<RetainedDecodedCandidate>,
    decoded_frame_count: usize,
    canceled: bool,
    isolated_demux_terminated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DecodedTemporalCandidate {
    Before,
    After,
}

pub(super) fn select_decoded_temporal_candidate(
    requested_pts: i64,
    access_mode: PreviewDecodeAccessMode,
    before: Option<DecodedTemporalExtent>,
    after: Option<DecodedTemporalExtent>,
) -> Option<(DecodedTemporalCandidate, DecodedTemporalExtent)> {
    let before = before.map(|extent| {
        after.map_or(extent, |successor| {
            extent.with_successor(successor.start_pts)
        })
    });
    if let Some(extent) = before.filter(|extent| extent.covers(requested_pts)) {
        return Some((DecodedTemporalCandidate::Before, extent));
    }
    if let Some(extent) = after.filter(|extent| extent.covers(requested_pts)) {
        return Some((DecodedTemporalCandidate::After, extent));
    }

    match access_mode {
        PreviewDecodeAccessMode::ScrubCursor => match (before, after) {
            (Some(before), Some(after)) => {
                if before.distance_to(requested_pts) <= after.distance_to(requested_pts) {
                    Some((DecodedTemporalCandidate::Before, before))
                } else {
                    Some((DecodedTemporalCandidate::After, after))
                }
            }
            (Some(before), None) => Some((DecodedTemporalCandidate::Before, before)),
            (None, Some(after)) => Some((DecodedTemporalCandidate::After, after)),
            (None, None) => None,
        },
        PreviewDecodeAccessMode::PlaybackCursor
        | PreviewDecodeAccessMode::RandomAccessStillFrame => None,
    }
}

pub(super) fn decoded_temporal_candidate_within_selection_distance(
    selected_extent: DecodedTemporalExtent,
    requested_pts: i64,
    max_select_distance_pts: i64,
) -> bool {
    selected_extent.covers(requested_pts)
        || selected_extent.distance_to(requested_pts) <= max_select_distance_pts
}

enum PreviewSeekToTarget {
    Complete(PreviewSeekResolution),
    DirectCanceled,
    IsolatedCanceled,
}

#[derive(Debug, Clone)]
pub(super) enum PreviewDecodedFramePayload {
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
    pub(super) fn reserved_cpu_bytes(&self) -> usize {
        match self {
            Self::CpuRgba(frame) => frame.rgba().len(),
            Self::CpuFloat(frame) => frame.rgba().len().saturating_mul(std::mem::size_of::<f32>()),
            Self::NativeGpu(_) => 0,
        }
    }

    pub(super) fn into_playback_ring_hit(self, elapsed: Duration) -> Self {
        match self {
            Self::CpuRgba(frame) => Self::CpuRgba(frame.into_playback_ring_hit(elapsed)),
            Self::CpuFloat(frame) => Self::CpuFloat(frame.into_playback_ring_hit(elapsed)),
            Self::NativeGpu(frame) => Self::NativeGpu(frame),
        }
    }

    pub(super) fn with_temporal_selection(
        self,
        requested_pts: i64,
        selected_extent: Option<DecodedTemporalExtent>,
    ) -> Self {
        match self {
            Self::CpuRgba(frame) => {
                Self::CpuRgba(frame.with_temporal_selection(requested_pts, selected_extent))
            }
            Self::CpuFloat(frame) => {
                Self::CpuFloat(frame.with_temporal_selection(requested_pts, selected_extent))
            }
            Self::NativeGpu(frame) => Self::NativeGpu(frame),
        }
    }

    pub(super) fn with_access_policy(self, policy: PreviewDecodeAccessPolicy) -> Self {
        match self {
            Self::CpuRgba(frame) => Self::CpuRgba(frame.with_access_policy(policy)),
            Self::CpuFloat(frame) => Self::CpuFloat(frame.with_access_policy(policy)),
            Self::NativeGpu(frame) => Self::NativeGpu(frame),
        }
    }

    pub(super) fn with_seek_index_diagnostics(
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

    pub(super) fn with_stage_durations(self, durations: PreviewDecodeStageDurations) -> Self {
        match self {
            Self::CpuRgba(frame) => Self::CpuRgba(frame.with_stage_durations(durations)),
            Self::CpuFloat(frame) => Self::CpuFloat(frame.with_stage_durations(durations)),
            Self::NativeGpu(frame) => Self::NativeGpu(frame),
        }
    }

    pub(super) fn with_hardware_decode_plan(self, plan: &PreviewHardwareDecodePlan) -> Self {
        match self {
            Self::CpuRgba(frame) => Self::CpuRgba(frame.with_hardware_decode_plan(plan)),
            Self::CpuFloat(frame) => Self::CpuFloat(frame.with_hardware_decode_plan(plan)),
            Self::NativeGpu(frame) => Self::NativeGpu(frame),
        }
    }

    pub(super) fn with_decoded_surface_format(self, format: DecodedVideoSurfaceFormat) -> Self {
        match self {
            Self::CpuRgba(frame) => Self::CpuRgba(frame.with_decoded_surface_format(format)),
            Self::CpuFloat(frame) => Self::CpuFloat(frame.with_decoded_surface_format(format)),
            Self::NativeGpu(frame) => Self::NativeGpu(frame),
        }
    }

    pub(super) fn into_outcome(self) -> PreviewDecodeOutcome {
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

struct RetainedDecodedCandidate {
    extent: DecodedTemporalExtent,
    frame: RetainedDecodedFrame,
}

impl RetainedDecodedCandidate {
    fn retain(
        start_pts: i64,
        frame: &ffmpeg::util::frame::video::Video,
        path: &Path,
    ) -> Result<Self> {
        Ok(Self {
            extent: DecodedTemporalExtent::from_decoded_frame(start_pts, frame),
            frame: RetainedDecodedFrame::retain(frame, path)?,
        })
    }
}

/// Bounded candidate window around one requested stream PTS.
///
/// Insertion is the sole owner of candidate ordering: regardless of decoder
/// output order it retains the greatest PTS at/before the target and the least
/// PTS after it. Equal-PTS decoded outputs keep the first frame deterministically
/// and record ambiguity so exact access can fail closed after the ready queue is
/// drained. Scrub may still use that deterministic first candidate; its ordinary
/// interval-coverage evidence continues to decide whether the result is Degraded.
struct RetainedDecodedCandidateWindow {
    target_pts: i64,
    before: Option<RetainedDecodedCandidate>,
    after: Option<RetainedDecodedCandidate>,
    duplicate_pts: Option<i64>,
}

fn advance_decoded_pts_high_water(high_water: &mut Option<i64>, observed_pts: i64) {
    *high_water = Some(high_water.map_or(observed_pts, |current| current.max(observed_pts)));
}

impl RetainedDecodedCandidateWindow {
    fn new(target_pts: i64) -> Self {
        Self {
            target_pts,
            before: None,
            after: None,
            duplicate_pts: None,
        }
    }

    fn seed(&mut self, candidate: RetainedDecodedCandidate) {
        self.insert(candidate, false);
    }

    fn observe(
        &mut self,
        frame_pts: i64,
        frame: &ffmpeg::util::frame::video::Video,
        path: &Path,
        pts_high_water: &mut Option<i64>,
    ) -> Result<()> {
        advance_decoded_pts_high_water(pts_high_water, frame_pts);
        // Only clone when this decoded frame can enter the window. During a
        // long-GOP forward scan almost every intermediate frame is outside
        // the retained before/after slots; cloning each one would allocate an
        // AVFrame plus buffer references per candidate for nothing.
        if self.would_retain(frame_pts) {
            self.insert(
                RetainedDecodedCandidate::retain(frame_pts, frame, path)?,
                true,
            );
        }
        Ok(())
    }

    fn would_retain(&self, candidate_pts: i64) -> bool {
        let slot = if candidate_pts <= self.target_pts {
            &self.before
        } else {
            &self.after
        };
        slot.as_ref().is_none_or(|current| {
            let current_pts = current.extent.start_pts;
            candidate_pts == current_pts
                || if candidate_pts <= self.target_pts {
                    candidate_pts > current_pts
                } else {
                    candidate_pts < current_pts
                }
        })
    }

    fn insert(&mut self, candidate: RetainedDecodedCandidate, record_duplicate: bool) {
        let candidate_pts = candidate.extent.start_pts;
        let slot = if candidate_pts <= self.target_pts {
            &mut self.before
        } else {
            &mut self.after
        };
        let current_pts = slot.as_ref().map(|current| current.extent.start_pts);
        let duplicate = current_pts == Some(candidate_pts);
        let replace = current_pts.is_none_or(|current_pts| {
            if candidate_pts == current_pts {
                false
            } else if candidate_pts <= self.target_pts {
                candidate_pts > current_pts
            } else {
                candidate_pts < current_pts
            }
        });
        if replace {
            *slot = Some(candidate);
        }
        if record_duplicate && duplicate {
            self.duplicate_pts.get_or_insert(candidate_pts);
        }
    }

    fn before(&self) -> Option<&RetainedDecodedCandidate> {
        self.before.as_ref()
    }

    fn after(&self) -> Option<&RetainedDecodedCandidate> {
        self.after.as_ref()
    }

    fn duplicate_pts(&self) -> Option<i64> {
        self.duplicate_pts
    }

    fn validate_exact_ordering(
        &self,
        path: &Path,
        access_mode: PreviewDecodeAccessMode,
    ) -> Result<()> {
        if access_mode == PreviewDecodeAccessMode::ScrubCursor {
            return Ok(());
        }
        let Some(duplicate_pts) = self.duplicate_pts() else {
            return Ok(());
        };
        Err(MondrianError::DecodeTemporalMismatch {
            asset_id: path.display().to_string(),
            access_mode: access_mode.as_str().to_owned(),
            requested_pts: Some(self.target_pts),
            selected_pts: Some(duplicate_pts),
            selected_duration_pts: None,
        })
    }

    fn selected_extent(
        &self,
        access_mode: PreviewDecodeAccessMode,
    ) -> Option<DecodedTemporalExtent> {
        select_decoded_temporal_candidate(
            self.target_pts,
            access_mode,
            self.before().map(|candidate| candidate.extent),
            self.after().map(|candidate| candidate.extent),
        )
        .map(|(_, extent)| extent)
    }

    fn take_successor(
        &mut self,
        selected_extent: DecodedTemporalExtent,
    ) -> Option<RetainedDecodedCandidate> {
        self.after
            .take()
            .filter(|candidate| candidate.extent.start_pts > selected_extent.start_pts)
    }
}

impl PreviewDecodeForwardResult {
    fn frame(
        frame: PreviewDecodedFramePayload,
        selected_extent: DecodedTemporalExtent,
        retained_selected_frame: RetainedDecodedCandidate,
        retained_next_frame: Option<RetainedDecodedCandidate>,
        decoded_frame_count: usize,
    ) -> Self {
        debug_assert_eq!(selected_extent, retained_selected_frame.extent);
        debug_assert!(retained_next_frame
            .as_ref()
            .is_none_or(|next| next.extent.start_pts > selected_extent.start_pts));
        Self {
            frame: Some(frame),
            selected_extent: Some(selected_extent),
            retained_selected_frame: Some(retained_selected_frame),
            retained_next_frame,
            decoded_frame_count,
            canceled: false,
            isolated_demux_terminated: false,
        }
    }

    fn empty(decoded_frame_count: usize) -> Self {
        Self {
            frame: None,
            selected_extent: None,
            retained_selected_frame: None,
            retained_next_frame: None,
            decoded_frame_count,
            canceled: false,
            isolated_demux_terminated: false,
        }
    }

    fn canceled(decoded_frame_count: usize) -> Self {
        Self {
            frame: None,
            selected_extent: None,
            retained_selected_frame: None,
            retained_next_frame: None,
            decoded_frame_count,
            canceled: true,
            isolated_demux_terminated: false,
        }
    }

    fn isolated_demux_canceled(decoded_frame_count: usize) -> Self {
        Self {
            frame: None,
            selected_extent: None,
            retained_selected_frame: None,
            retained_next_frame: None,
            decoded_frame_count,
            canceled: true,
            isolated_demux_terminated: true,
        }
    }
}

use seek_index::{PreviewSeekIndex, PreviewSeekIndexDiagnostics, PreviewSeekResolution};

use playback_ring::PreviewPlaybackRing;

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
    device_context_pool: &HwDeviceContextPool,
) -> std::result::Result<
    (Box<PreviewHardwareDecodeContextState>, HwAccelDeviceContext),
    HwAccelDeviceContextProbe,
> {
    let backend = plan.probe.candidate_backend.ok_or_else(|| {
        HwAccelDeviceContextProbe::unavailable(
            HwAccelBackend::None,
            "no platform hardware decode backend candidate",
        )
    })?;
    let hw_pixel_format = plan
        .ffmpeg_codec_config
        .hw_pixel_format
        .and_then(HwAccelPixelFormat::to_ffmpeg)
        .ok_or_else(|| {
            HwAccelDeviceContextProbe::deferred(
                backend,
                plan.ffmpeg_codec_config.ffmpeg_device_type_available,
                "FFmpeg codec config did not expose a usable hardware pixel format",
            )
        })?;
    let device_context = device_context_pool.acquire(backend, plan.device_selector)?;
    if let Err(reason) = device_context.attach_to_codec_context(context) {
        let newly_created = device_context.newly_created();
        device_context.retire_after_setup_failure(format!(
            "{} hardware device codec attachment failed: {reason}",
            backend.as_str()
        ));
        return Err(HwAccelDeviceContextProbe::acquired(
            backend,
            newly_created,
            format!("hardware device was acquired but codec attachment failed: {reason}"),
        ));
    }

    let mut state =
        Box::new(PreviewHardwareDecodeContextState { preferred_hw_pixel_format: hw_pixel_format });
    unsafe {
        (*context.as_mut_ptr()).extra_hw_frames = preview_hardware_extra_frames(plan.request);
        (*context.as_mut_ptr()).opaque = (&mut *state) as *mut _ as *mut c_void;
        (*context.as_mut_ptr()).get_format = Some(preview_hardware_decode_get_format);
    }
    Ok((state, device_context))
}

pub(super) fn preview_create_rgba_scaler(
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

#[derive(Debug, Clone, Copy)]
struct PreviewDecodeSessionOpenRequest<'a> {
    path: &'a Path,
    fingerprint: MediaFileFingerprint,
    video_stream_index: Option<u32>,
    max_width: Option<u32>,
    max_height: Option<u32>,
    access_mode: PreviewDecodeAccessMode,
    backend: PreviewDecodeBackend,
    hardware_decode_request: PreviewHardwareDecodeRequest,
    hardware_decode_device_selector: Option<HwAccelDeviceSelector>,
    source_color: PreviewSourceColorContract,
}

impl PreviewDecodeSession {
    fn open(
        request: PreviewDecodeSessionOpenRequest<'_>,
        resources: &PreviewDecodeWorkerResources,
        demux_worker: Option<&PreviewDemuxWorkerConfig>,
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
        interrupt_state: Arc<PreviewDecodeInterruptState>,
    ) -> std::result::Result<Self, PreviewPacketSourceOpenError> {
        let source = PreviewPacketSource::open(
            request.path,
            request.fingerprint,
            request.video_stream_index,
            resources.seek_index_cache(),
            demux_worker,
            &interrupt_state,
            should_cancel,
        )?;
        Self::from_packet_source(
            source,
            interrupt_state,
            request,
            resources.hardware_device_context_pool(),
            resources.native_output_tracker(),
        )
        .map_err(PreviewPacketSourceOpenError::Failed)
    }

    fn from_packet_source(
        source: PreviewPacketSourceOpen,
        interrupt_state: Arc<PreviewDecodeInterruptState>,
        request: PreviewDecodeSessionOpenRequest<'_>,
        hardware_device_context_pool: &HwDeviceContextPool,
        family_native_outputs: &PreviewNativeOutputTracker,
    ) -> Result<Self> {
        let PreviewDecodeSessionOpenRequest {
            path,
            fingerprint,
            video_stream_index: requested_video_stream_index,
            max_width,
            max_height,
            access_mode,
            backend,
            hardware_decode_request,
            hardware_decode_device_selector,
            source_color,
        } = request;
        let PreviewPacketSourceOpen {
            source,
            parameters,
            stream_index,
            stream_tb,
            stream_start_pts,
            stream_rate,
            seek_index,
        } = source;
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
        let mut hardware_device_context = None;
        interrupt_state.set_execution_stage(PreviewDecodeExecutionStage::SessionSetup);
        let hardware_decoder = if hardware_decode_plan
            .should_configure_hardware_decoder(access_mode)
            && backend != PreviewDecodeBackend::Software
        {
            loop {
                let mut context = preview_decode_context_from_parameters(
                    parameters.clone(),
                    ffmpeg_threading,
                    path,
                )?;
                interrupt_state.set_execution_stage(PreviewDecodeExecutionStage::HardwareDevice);
                match configure_preview_hardware_decode_context(
                    &mut context,
                    &hardware_decode_plan,
                    hardware_device_context_pool,
                ) {
                    Ok((state, device_context)) => {
                        hardware_decode_plan.mark_device_context_acquired(
                            device_context.backend(),
                            device_context.newly_created(),
                        );
                        if hardware_decode_plan.allows_cpu_transfer_fallback() {
                            hardware_decode_plan
                                .mark_hardware_cpu_transfer_configured(device_context.backend());
                        }
                        interrupt_state.set_execution_stage(PreviewDecodeExecutionStage::CodecOpen);
                        match context.decoder().video() {
                            Ok(decoder) => {
                                hardware_decode_context_state = Some(state);
                                hardware_device_context = Some(device_context);
                                break Some(decoder);
                            }
                            Err(error) => {
                                let reason = error.to_string();
                                device_context.retire_after_setup_failure(format!(
                                    "{} hardware decoder failed to open: {reason}",
                                    device_context.backend().as_str()
                                ));
                                hardware_decode_plan.mark_decoder_open_failed(reason.clone());
                                if hardware_decode_plan
                                    .advance_hardware_candidate(access_mode, backend)
                                {
                                    continue;
                                }
                                if hardware_decode_request.requires_gpu_residency() {
                                    return Err(MondrianError::DecodeFailed {
                                        asset_id: path.display().to_string(),
                                        reason: format!(
                                            "required GPU-resident FFmpeg decoders failed to open: {}",
                                            hardware_decode_plan.probe.reason
                                        ),
                                    });
                                }
                                preview_trace(format!(
                                    "[preview] compatible hardware decoder backends failed to open, fallback software: {reason}"
                                ));
                                break None;
                            }
                        }
                    }
                    Err(probe) => {
                        let reason = probe.reason.clone();
                        hardware_decode_plan.mark_device_context_setup_failed(probe);
                        if hardware_decode_plan.advance_hardware_candidate(access_mode, backend) {
                            continue;
                        }
                        if hardware_decode_request.requires_gpu_residency() {
                            return Err(MondrianError::DecodeFailed {
                                asset_id: path.display().to_string(),
                                reason: format!(
                                    "required GPU-resident FFmpeg decoder setup failed: {}",
                                    hardware_decode_plan.probe.reason
                                ),
                            });
                        }
                        preview_trace(format!(
                            "[preview] compatible hardware device backends failed, fallback software: {reason}"
                        ));
                        break None;
                    }
                }
            }
        } else {
            None
        };

        let decoder = match hardware_decoder {
            Some(decoder) => decoder,
            None => {
                interrupt_state.set_execution_stage(PreviewDecodeExecutionStage::CodecOpen);
                preview_decode_context_from_parameters(parameters, ffmpeg_threading, path)?
                    .decoder()
                    .video()
                    .map_err(|error| MondrianError::DecodeFailed {
                        asset_id: path.display().to_string(),
                        reason: error.to_string(),
                    })?
            }
        };
        interrupt_state.set_execution_stage(PreviewDecodeExecutionStage::SessionSetup);
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

        Ok(Self {
            path: path.to_path_buf(),
            fingerprint,
            requested_video_stream_index,
            max_width,
            max_height,
            backend,
            hardware_decode_request,
            hardware_decode_device_selector,
            source_color,
            codec_id,
            packet_source: source,
            interrupt_state,
            decoder,
            scaler,
            scaler_source_format,
            scaler_color_contract: None,
            stream_index,
            stream_tb,
            stream_start_pts,
            frame_duration_pts,
            target_width,
            target_height,
            _hardware_decode_context_state: hardware_decode_context_state,
            hardware_device_context,
            threading_kind,
            threading_count,
            hardware_decode_plan,
            decoded_surface_format,
            family_native_outputs: family_native_outputs.clone(),
            session_native_outputs: PreviewNativeOutputTracker::default(),
            last_pts: None,
            duplicate_decoded_pts: None,
            last_decoded_frame: None,
            next_decoded_frame: None,
            reached_eof: false,
            playback_ring: PreviewPlaybackRing::new(
                PREVIEW_PLAYBACK_SESSION_RING_CAPACITY,
                PREVIEW_PLAYBACK_SESSION_RING_BYTE_BUDGET,
            ),
            seek_index,
        })
    }

    fn matches(
        &self,
        request: &PreviewDecodeSessionOpenRequest<'_>,
        demux_worker_available: bool,
    ) -> bool {
        self.packet_source.is_healthy()
            && packet_source_execution_family_matches(
                self.packet_source.is_isolated(),
                demux_worker_available,
            )
            && self.path == request.path
            && request.fingerprint.authorizes_reuse()
            && self.fingerprint == request.fingerprint
            && self.requested_video_stream_index == request.video_stream_index
            && self.max_width == request.max_width
            && self.max_height == request.max_height
            && self.backend == request.backend
            && self.hardware_decode_request == request.hardware_decode_request
            && self.hardware_decode_device_selector == request.hardware_decode_device_selector
            && self.source_color == request.source_color
    }

    fn native_output_released(&self) -> bool {
        self.session_native_outputs.is_released()
    }

    fn retire_hardware_device_context(&self) {
        if let Some(device_context) = &self.hardware_device_context {
            device_context.retire_after_runtime_failure(
                "runtime video decode failure retired the hardware device context",
            );
        }
    }

    /// Restore a deterministic decode entry after cooperative cancellation.
    ///
    /// The codec allocation and immutable hardware device remain reusable, but
    /// demux/codec position cannot: cancellation may have occurred after a
    /// packet was submitted or while reordered output was only partly drained.
    /// Clearing position forces the next request through indexed seek + flush.
    fn recover_after_cancellation(&mut self) {
        // SAFETY: this worker exclusively owns the open decoder and calls flush
        // only after the interrupted decode operation has returned.
        self.interrupt_state
            .set_execution_stage(PreviewDecodeExecutionStage::CodecFlush);
        unsafe {
            ffmpeg::ffi::avcodec_flush_buffers(self.decoder.as_mut_ptr());
        }
        self.decoder.skip_frame(ffmpeg::codec::discard::Discard::Default);
        self.last_pts = None;
        self.duplicate_decoded_pts = None;
        self.last_decoded_frame = None;
        self.next_decoded_frame = None;
        self.reached_eof = false;
        self.playback_ring.clear();
    }

    fn decode_at(
        &mut self,
        source_sample: SourceSampleTarget,
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
            source_sample_to_stream_pts(source_sample, self.stream_tb, self.stream_start_pts)
                .map_err(|reason| MondrianError::DecodeFailed {
                    asset_id: self.path.display().to_string(),
                    reason,
                })?;
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
        if allow_cpu_cache
            && policy.use_playback_ring
            && let Some((selected_extent, hit)) = self.playback_ring.get(target_pts)
        {
            if should_cancel() {
                return Ok(PreviewDecodeOutcome::Canceled(
                    self.interrupt_state
                        .cancellation(PreviewDecodeCancellationCheckpoint::CacheLookup)
                        .with_session_attempt(PreviewDecodeSessionDisposition::BypassedCache, 0),
                ));
            }
            return Ok(hit
                .into_playback_ring_hit(cache_lookup_started_at.elapsed())
                .with_temporal_selection(target_pts, Some(selected_extent))
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
        let cache_lookup_us = duration_us(cache_lookup_started_at.elapsed());

        let retained_selection_covers_target = [
            self.last_decoded_frame.as_ref(),
            self.next_decoded_frame.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|candidate| candidate.extent.covers(target_pts));
        let should_continue_forward = retained_selection_covers_target
            || self
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
            seek_resolution = match self.seek_to_target(decode_target_pts, policy, should_cancel) {
                Ok(PreviewSeekToTarget::Complete(resolution)) => resolution,
                Ok(PreviewSeekToTarget::DirectCanceled) => {
                    return Ok(PreviewDecodeOutcome::Canceled(
                        self.interrupt_state
                            .cancellation(PreviewDecodeCancellationCheckpoint::Seek),
                    ));
                }
                Ok(PreviewSeekToTarget::IsolatedCanceled) => {
                    return Ok(PreviewDecodeOutcome::Canceled(
                        PreviewDecodeCancellation::isolated_demux_termination(
                            PreviewDecodeCancellationCheckpoint::Seek,
                        ),
                    ));
                }
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
        let session_output_lease = PreviewDecodeSessionOutputLease::acquire(
            &self.family_native_outputs,
            &self.session_native_outputs,
        )
        .map_err(|error| MondrianError::DecodeFailed {
            asset_id: self.path.display().to_string(),
            reason: error.to_string(),
        })?;
        let mut result = self.decode_forward_until(
            decode_target_pts,
            policy,
            &session_output_lease,
            should_cancel,
        )?;
        self.last_decoded_frame = result.retained_selected_frame.take();
        self.next_decoded_frame = result.retained_next_frame.take();
        if result.canceled {
            let cancellation = if result.isolated_demux_terminated {
                PreviewDecodeCancellation::isolated_demux_termination(
                    PreviewDecodeCancellationCheckpoint::PacketRead,
                )
            } else {
                self.interrupt_state.cancellation(PreviewDecodeCancellationCheckpoint::Codec)
            };
            return Ok(PreviewDecodeOutcome::Canceled(cancellation));
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
                        .with_temporal_selection(target_pts, result.selected_extent)
                        .with_access_policy(policy)
                        .with_forward_reused(should_continue_forward)
                        .with_seek_index_diagnostics(self.seek_index.diagnostics(), seek_resolution)
                        .with_threading(self.threading_kind, self.threading_count)
                        .with_hardware_decode_plan(&self.hardware_decode_plan)
                        .with_decoded_surface_format(self.decoded_surface_format)
                        .with_decode_execution();
                    if policy.use_playback_ring
                        && let Some(selected_extent) = result.selected_extent
                    {
                        self.playback_ring.put(
                            selected_extent,
                            PreviewDecodedFramePayload::CpuRgba(frame.clone()),
                        );
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
                        .with_temporal_selection(target_pts, result.selected_extent)
                        .with_access_policy(policy)
                        .with_forward_reused(should_continue_forward)
                        .with_seek_index_diagnostics(self.seek_index.diagnostics(), seek_resolution)
                        .with_threading(self.threading_kind, self.threading_count)
                        .with_hardware_decode_plan(&self.hardware_decode_plan)
                        .with_decoded_surface_format(self.decoded_surface_format)
                        .with_decode_execution();
                    if policy.use_playback_ring
                        && let Some(selected_extent) = result.selected_extent
                    {
                        self.playback_ring.put(
                            selected_extent,
                            PreviewDecodedFramePayload::CpuFloat(frame.clone()),
                        );
                    }
                    return Ok(PreviewDecodeOutcome::FloatFrame(frame));
                }
                PreviewDecodedFramePayload::NativeGpu(mut frame) => {
                    let mut diagnostics = frame
                        .diagnostics
                        .with_access_mode(access_mode)
                        .with_temporal_selection(target_pts, result.selected_extent);
                    diagnostics.stage_durations.accumulate(PreviewDecodeStageDurations {
                        cache_lookup_us,
                        seek_us,
                        packet_decode_us: duration_us(decode_started_at.elapsed()),
                        ..PreviewDecodeStageDurations::default()
                    });
                    diagnostics.seek_performed = seek_performed;
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
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<PreviewSeekToTarget> {
        if self.stream_tb.denominator() <= 0 || self.stream_tb.numerator() <= 0 {
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

        let seek_anchor_pts = self.seek_index.keyframe_at_or_before(target_pts);
        let (min_ts, seek_target_ts, max_ts, seek_flags, used_anchor_pts) = match policy
            .seek_strategy
        {
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
                let window_numerator = i64::try_from(policy.any_seek_window_ms).map_err(|_| {
                    MondrianError::DecodeFailed {
                        asset_id: self.path.display().to_string(),
                        reason: format!(
                            "seek window {} ms exceeds exact time range",
                            policy.any_seek_window_ms
                        ),
                    }
                })?;
                let seek_window = TimelineTime::new(window_numerator, 1_000).map_err(|error| {
                    MondrianError::DecodeFailed {
                        asset_id: self.path.display().to_string(),
                        reason: format!("invalid exact seek window: {error}"),
                    }
                })?;
                let seek_window_pts = duration_to_time_base_ticks(
                    seek_window,
                    i64::from(self.stream_tb.numerator()),
                    i64::from(self.stream_tb.denominator()),
                )
                .map_err(|reason| MondrianError::DecodeFailed {
                    asset_id: self.path.display().to_string(),
                    reason,
                })?
                .max(1);
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

        let seek = self.packet_source.seek(
            self.stream_index,
            min_ts,
            seek_target_ts,
            max_ts,
            seek_flags,
            &self.path,
            should_cancel,
        )?;
        match seek {
            PreviewPacketSeek::DirectCanceled => return Ok(PreviewSeekToTarget::DirectCanceled),
            PreviewPacketSeek::IsolatedCanceled => {
                return Ok(PreviewSeekToTarget::IsolatedCanceled)
            }
            PreviewPacketSeek::Complete => {}
        }
        self.interrupt_state
            .set_execution_stage(PreviewDecodeExecutionStage::CodecFlush);
        unsafe {
            ffmpeg::ffi::avcodec_flush_buffers(self.decoder.as_mut_ptr());
        }
        self.reached_eof = false;
        self.last_pts = None;
        self.duplicate_decoded_pts = None;
        self.last_decoded_frame = None;
        self.next_decoded_frame = None;
        Ok(PreviewSeekToTarget::Complete(PreviewSeekResolution {
            used_index: used_anchor_pts.is_some(),
            anchor_pts: used_anchor_pts,
        }))
    }

    fn decode_forward_until(
        &mut self,
        target_pts: i64,
        policy: PreviewDecodeAccessPolicy,
        session_output_lease: &PreviewDecodeSessionOutputLease,
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<PreviewDecodeForwardResult> {
        let interrupt_state = Arc::clone(&self.interrupt_state);
        interrupt_state.set_checkpoint(PreviewDecodeCancellationCheckpoint::Codec);
        let mut candidates = RetainedDecodedCandidateWindow::new(target_pts);
        candidates.duplicate_pts = self.duplicate_decoded_pts;
        for retained in [
            self.last_decoded_frame.take(),
            self.next_decoded_frame.take(),
        ]
        .into_iter()
        .flatten()
        {
            candidates.seed(retained);
        }
        let mut frames_decoded: usize = 0;
        let mut video_packets_submitted: usize = 0;
        let mut non_reference_discard_until_pts =
            exact_seek_non_reference_discard_until_pts(policy, target_pts, self.frame_duration_pts);
        self.decoder.skip_frame(
            non_reference_discard_until_pts
                .map_or(ffmpeg::codec::discard::Discard::Default, |_| {
                    ffmpeg::codec::discard::Discard::NonReference
                }),
        );
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

        let choose_and_convert = |hardware_decode_plan: &mut PreviewHardwareDecodePlan,
                                  scaler: &mut Option<ffmpeg::software::scaling::Context>,
                                  scaler_source_format: &mut Option<
            ffmpeg::util::format::pixel::Pixel,
        >,
                                  scaler_color_contract: &mut Option<DecodedRgbaFrameContract>,
                                  target_width: u32,
                                  target_height: u32,
                                  path: &Path,
                                  before: Option<&RetainedDecodedCandidate>,
                                  after: Option<&RetainedDecodedCandidate>|
         -> Result<
            Option<(
                DecodedTemporalExtent,
                PreviewDecodedFramePayload,
                RetainedDecodedCandidate,
            )>,
        > {
            if should_cancel() {
                return Ok(None);
            }
            let before_extent = before.map(|candidate| candidate.extent);
            let after_extent = after.map(|candidate| candidate.extent);
            let Some((candidate, selected_extent)) = select_decoded_temporal_candidate(
                target_pts,
                policy.access_mode,
                before_extent,
                after_extent,
            ) else {
                return Ok(None);
            };
            let selected_frame = match candidate {
                DecodedTemporalCandidate::Before => before.map(|candidate| candidate.frame.frame()),
                DecodedTemporalCandidate::After => after.map(|candidate| candidate.frame.frame()),
            }
            .ok_or_else(|| MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: "temporal selector lost its retained decoded-frame candidate".to_owned(),
            })?;

            if !decoded_temporal_candidate_within_selection_distance(
                selected_extent,
                target_pts,
                max_select_distance_pts,
            ) {
                return Ok(None);
            }

            if should_cancel() {
                return Ok(None);
            }
            let retained_selected_frame = RetainedDecodedCandidate {
                extent: selected_extent,
                frame: RetainedDecodedFrame::retain(selected_frame, path)?,
            };
            interrupt_state
                .set_checkpoint(PreviewDecodeCancellationCheckpoint::FrameMaterialization);
            let frame = materialize_decoded_frame_with_session_output_lease(
                selected_frame,
                hardware_decode_plan,
                scaler,
                scaler_source_format,
                scaler_color_contract,
                target_width,
                target_height,
                path,
                self.source_color,
                session_output_lease.clone(),
            )?;
            Ok(Some((selected_extent, frame, retained_selected_frame)))
        };

        let retained_selection_is_exact = candidates
            .selected_extent(policy.access_mode)
            .is_some_and(|extent| extent.covers(target_pts));
        if retained_selection_is_exact {
            self.duplicate_decoded_pts = candidates.duplicate_pts();
            candidates.validate_exact_ordering(self.path.as_path(), policy.access_mode)?;
            let Some((selected_extent, frame, retained_selected_frame)) = choose_and_convert(
                &mut self.hardware_decode_plan,
                &mut self.scaler,
                &mut self.scaler_source_format,
                &mut self.scaler_color_contract,
                self.target_width,
                self.target_height,
                self.path.as_path(),
                candidates.before(),
                candidates.after(),
            )?
            else {
                return Err(MondrianError::DecodeFailed {
                    asset_id: self.path.display().to_string(),
                    reason:
                        "exact retained temporal candidate could not be materialized consistently"
                            .to_owned(),
                });
            };
            if should_cancel() {
                return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
            }
            return Ok(PreviewDecodeForwardResult::frame(
                frame,
                selected_extent,
                retained_selected_frame,
                candidates.take_successor(selected_extent),
                frames_decoded,
            ));
        }

        // A prior forward request may have returned as soon as it found its
        // target while the frame-threaded decoder still held reordered output.
        // Consume that output before submitting another packet: FFmpeg requires
        // callers to receive frames after AVERROR(EAGAIN), and the retained
        // frames are also the best candidates for the next playback position.
        while let DecodedVideoReceive::Frame(decoded) =
            receive_decoded_video_frame(&mut self.decoder, &interrupt_state, self.path.as_path())?
        {
            if should_cancel() {
                return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
            }
            frames_decoded += 1;
            if let Some(frame_pts) = decoded.pts {
                candidates.observe(
                    frame_pts,
                    &decoded.frame,
                    self.path.as_path(),
                    &mut self.last_pts,
                )?;
            }
        }

        // Selection happens only after FFmpeg's complete ready queue reaches
        // EAGAIN. A first future frame cannot hide a later ready frame with a
        // smaller PTS, and duplicate PTS evidence is visible before exact
        // publication is considered.
        self.duplicate_decoded_pts = candidates.duplicate_pts();
        candidates.validate_exact_ordering(self.path.as_path(), policy.access_mode)?;
        if let Some((selected_extent, frame, retained_selected_frame)) = choose_and_convert(
            &mut self.hardware_decode_plan,
            &mut self.scaler,
            &mut self.scaler_source_format,
            &mut self.scaler_color_contract,
            self.target_width,
            self.target_height,
            self.path.as_path(),
            candidates.before(),
            candidates.after(),
        )? {
            if should_cancel() {
                return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
            }
            return Ok(PreviewDecodeForwardResult::frame(
                frame,
                selected_extent,
                retained_selected_frame,
                candidates.take_successor(selected_extent),
                frames_decoded,
            ));
        }

        loop {
            interrupt_state.set_checkpoint(PreviewDecodeCancellationCheckpoint::PacketRead);
            let packet = match self.packet_source.read_next(&self.path, should_cancel)? {
                PreviewPacketRead::Packet(packet) => packet,
                PreviewPacketRead::End => break,
                PreviewPacketRead::DirectCanceled => {
                    return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
                }
                PreviewPacketRead::IsolatedCanceled => {
                    return Ok(PreviewDecodeForwardResult::isolated_demux_canceled(
                        frames_decoded,
                    ));
                }
            };
            interrupt_state.set_checkpoint(PreviewDecodeCancellationCheckpoint::Codec);
            if should_cancel() {
                return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
            }
            if packet.stream() != self.stream_index {
                continue;
            }
            self.seek_index.observe_packet(&packet);
            if policy.forward_decode_budget_exhausted(forward_decode_work_units(
                frames_decoded,
                video_packets_submitted,
            )) {
                break;
            }

            if non_reference_discard_until_pts.is_some_and(|switch_pts| {
                packet
                    .pts()
                    .or_else(|| packet.dts())
                    .is_none_or(|packet_pts| packet_pts >= switch_pts)
            }) {
                self.decoder.skip_frame(ffmpeg::codec::discard::Discard::Default);
                non_reference_discard_until_pts = None;
            }

            interrupt_state.set_execution_stage(PreviewDecodeExecutionStage::CodecSendInput);
            self.decoder.send_packet(&packet).map_err(|e| MondrianError::DecodeFailed {
                asset_id: self.path.display().to_string(),
                reason: e.to_string(),
            })?;
            video_packets_submitted = video_packets_submitted.saturating_add(1);

            while let DecodedVideoReceive::Frame(decoded) = receive_decoded_video_frame(
                &mut self.decoder,
                &interrupt_state,
                self.path.as_path(),
            )? {
                if should_cancel() {
                    return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
                }
                frames_decoded += 1;
                if let Some(frame_pts) = decoded.pts {
                    candidates.observe(
                        frame_pts,
                        &decoded.frame,
                        self.path.as_path(),
                        &mut self.last_pts,
                    )?;
                }
            }

            self.duplicate_decoded_pts = candidates.duplicate_pts();
            candidates.validate_exact_ordering(self.path.as_path(), policy.access_mode)?;
            if let Some((selected_extent, frame, retained_selected_frame)) = choose_and_convert(
                &mut self.hardware_decode_plan,
                &mut self.scaler,
                &mut self.scaler_source_format,
                &mut self.scaler_color_contract,
                self.target_width,
                self.target_height,
                self.path.as_path(),
                candidates.before(),
                candidates.after(),
            )? {
                if should_cancel() {
                    return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
                }
                return Ok(PreviewDecodeForwardResult::frame(
                    frame,
                    selected_extent,
                    retained_selected_frame,
                    candidates.take_successor(selected_extent),
                    frames_decoded,
                ));
            }

            if policy.forward_decode_budget_exhausted(forward_decode_work_units(
                frames_decoded,
                video_packets_submitted,
            )) {
                break;
            }
        }

        if !self.reached_eof
            && !policy.forward_decode_budget_exhausted(forward_decode_work_units(
                frames_decoded,
                video_packets_submitted,
            ))
        {
            if should_cancel() {
                return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
            }
            interrupt_state.set_execution_stage(PreviewDecodeExecutionStage::CodecSendInput);
            self.decoder.send_eof().map_err(|e| MondrianError::DecodeFailed {
                asset_id: self.path.display().to_string(),
                reason: e.to_string(),
            })?;

            while let DecodedVideoReceive::Frame(decoded) = receive_decoded_video_frame(
                &mut self.decoder,
                &interrupt_state,
                self.path.as_path(),
            )? {
                if should_cancel() {
                    return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
                }
                frames_decoded += 1;
                if let Some(frame_pts) = decoded.pts {
                    candidates.observe(
                        frame_pts,
                        &decoded.frame,
                        self.path.as_path(),
                        &mut self.last_pts,
                    )?;
                }
            }

            self.reached_eof = true;
        }

        self.duplicate_decoded_pts = candidates.duplicate_pts();
        candidates.validate_exact_ordering(self.path.as_path(), policy.access_mode)?;
        if let Some((selected_extent, frame, retained_selected_frame)) = choose_and_convert(
            &mut self.hardware_decode_plan,
            &mut self.scaler,
            &mut self.scaler_source_format,
            &mut self.scaler_color_contract,
            self.target_width,
            self.target_height,
            self.path.as_path(),
            candidates.before(),
            candidates.after(),
        )? {
            if should_cancel() {
                return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
            }
            return Ok(PreviewDecodeForwardResult::frame(
                frame,
                selected_extent,
                retained_selected_frame,
                candidates.take_successor(selected_extent),
                frames_decoded,
            ));
        }

        if should_cancel() {
            return Ok(PreviewDecodeForwardResult::canceled(frames_decoded));
        }
        if self.reached_eof && policy.access_mode != PreviewDecodeAccessMode::ScrubCursor {
            let selected_extent = candidates.before().map(|candidate| candidate.extent);
            return Err(MondrianError::DecodeTemporalMismatch {
                asset_id: self.path.display().to_string(),
                access_mode: policy.access_mode.as_str().to_owned(),
                requested_pts: Some(target_pts),
                selected_pts: selected_extent.map(|extent| extent.start_pts),
                selected_duration_pts: selected_extent.and_then(|extent| extent.duration_pts),
            });
        }
        let forward_decode_work_units =
            forward_decode_work_units(frames_decoded, video_packets_submitted);
        if policy.forward_decode_budget_exhausted(forward_decode_work_units) {
            return Err(MondrianError::DecodeBudgetExhausted {
                asset_id: self.path.display().to_string(),
                access_mode: policy.access_mode.as_str().to_owned(),
                decoded_frames: forward_decode_work_units as u64,
                budget_frames: policy.forward_decode_budget_frames as u64,
                target_pts,
            });
        }

        Ok(PreviewDecodeForwardResult::empty(frames_decoded))
    }
}

pub(super) fn exact_seek_non_reference_discard_until_pts(
    policy: PreviewDecodeAccessPolicy,
    target_pts: i64,
    frame_duration_pts: i64,
) -> Option<i64> {
    (policy.access_mode == PreviewDecodeAccessMode::RandomAccessStillFrame
        && !policy.keyframe_only
        && policy.seek_strategy == PreviewDecodeSeekStrategy::KeyframeBefore)
        .then(|| {
            target_pts.saturating_sub(
                frame_duration_pts
                    .max(1)
                    .saturating_mul(PREVIEW_EXACT_SEEK_FULL_DECODE_PREROLL_FRAMES),
            )
        })
}

pub(super) fn forward_decode_work_units(
    frames_decoded: usize,
    video_packets_submitted: usize,
) -> usize {
    frames_decoded.max(video_packets_submitted)
}

pub(super) fn decode_preview_frame_outcome(
    request: PreviewDecodeRequest<'_>,
    should_cancel: PreviewDecodeCancelProbe,
) -> Result<PreviewDecodeOutcome> {
    THREAD_PREVIEW_DECODE_CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        let execution_observer = context.execution_observer.clone();
        let resources = context.resources.clone();
        let _execution = execution_observer.begin_request();
        decode_preview_frame_outcome_in_sessions(
            &mut context.sessions,
            &execution_observer,
            request,
            &resources,
            None,
            should_cancel,
        )
    })
}

fn decode_preview_frame_outcome_in_sessions(
    sessions: &mut PreviewDecodeSessions,
    execution_observer: &PreviewDecodeExecutionObserver,
    request: PreviewDecodeRequest<'_>,
    resources: &PreviewDecodeWorkerResources,
    demux_worker: Option<&PreviewDemuxWorkerConfig>,
    should_cancel: PreviewDecodeCancelProbe,
) -> Result<PreviewDecodeOutcome> {
    let PreviewDecodeRequest {
        path,
        video_stream_index,
        source_sample,
        max_width,
        max_height,
        access_mode,
        fingerprint,
        adaptive_hints,
        hardware_decode_request,
        hardware_decode_device_selector,
        source_color,
    } = request;
    let started_at = Instant::now();
    if should_cancel() {
        return Ok(PreviewDecodeOutcome::Canceled(
            PreviewDecodeCancellation::cooperative(
                PreviewDecodeCancellationCheckpoint::BeforeInputOpen,
            ),
        ));
    }
    let output_lease_wait_started_at = Instant::now();
    let selected_slot = loop {
        if let Some(slot) = sessions.available_slot(access_mode, hardware_decode_request) {
            break slot;
        }
        execution_observer.publish_stage(PreviewDecodeExecutionStage::OutputLeaseWait);
        if should_cancel() {
            return Ok(PreviewDecodeOutcome::Canceled(
                PreviewDecodeCancellation::cooperative(
                    PreviewDecodeCancellationCheckpoint::OutputLease,
                ),
            ));
        }
        // Native-output release is driven by the UI-independent result pump,
        // Frame Store generation release, and renderer copy fence. Keep the
        // worker responsive to latest-wins cancellation while those owners run.
        std::thread::sleep(Duration::from_micros(250));
    };
    let output_lease_wait_us = duration_us(output_lease_wait_started_at.elapsed());
    ensure_ffmpeg_initialized(path)?;
    // A caller-supplied complete revision authorized the request's probe,
    // color contract, and cache identity. Recheck it at the execution worker
    // after any output-lease wait so a replacement cannot enter an existing or
    // newly opened decoder Session under stale semantics.
    let fingerprint = resolve_preview_execution_fingerprint(path, fingerprint)?;
    let outcome: Result<PreviewDecodeOutcome> = {
        let slot = sessions.slot_mut(selected_slot);
        let backend = preview_decode_backend();
        let open_request = PreviewDecodeSessionOpenRequest {
            path,
            fingerprint,
            video_stream_index,
            max_width,
            max_height,
            access_mode,
            backend,
            hardware_decode_request,
            hardware_decode_device_selector,
            source_color,
        };
        let mut session_open_us = 0;

        if should_cancel() {
            return Ok(PreviewDecodeOutcome::Canceled(
                PreviewDecodeCancellation::cooperative(
                    PreviewDecodeCancellationCheckpoint::BeforeInputOpen,
                ),
            ));
        }
        let had_session = slot.is_some();
        let current_match = slot
            .as_ref()
            .map(|session| session.matches(&open_request, demux_worker.is_some()))
            .unwrap_or(false);
        let session_disposition = if current_match {
            PreviewDecodeSessionDisposition::Reused
        } else if had_session {
            PreviewDecodeSessionDisposition::Replaced
        } else {
            PreviewDecodeSessionDisposition::Opened
        };

        if !current_match {
            // A changed source or decode contract cannot reuse this decoder.
            // Release its DPB/surface pool before opening the replacement so
            // incompatible hardware pools never overlap.
            if slot.is_some() {
                execution_observer.publish_stage(PreviewDecodeExecutionStage::SessionRetire);
                *slot = None;
            }
            execution_observer.publish_stage(PreviewDecodeExecutionStage::SessionSetup);
            let open_started_at = Instant::now();
            let interrupt_state = Arc::new(PreviewDecodeInterruptState::with_execution_observer(
                execution_observer.clone(),
            ));
            let _interrupt_guard = interrupt_state.install(Arc::clone(&should_cancel));
            let opened = PreviewDecodeSession::open(
                open_request,
                resources,
                demux_worker,
                should_cancel.as_ref(),
                Arc::clone(&interrupt_state),
            );
            *slot = match opened {
                Ok(session) => Some(session),
                Err(PreviewPacketSourceOpenError::DirectCanceled) => {
                    let session_open_us = duration_us(open_started_at.elapsed());
                    return Ok(PreviewDecodeOutcome::Canceled(
                        interrupt_state
                            .cancellation(PreviewDecodeCancellationCheckpoint::InputOpen)
                            .with_session_attempt(session_disposition, session_open_us),
                    ));
                }
                Err(PreviewPacketSourceOpenError::IsolatedCanceled(checkpoint)) => {
                    let session_open_us = duration_us(open_started_at.elapsed());
                    return Ok(PreviewDecodeOutcome::Canceled(
                        PreviewDecodeCancellation::isolated_demux_termination(checkpoint)
                            .with_session_attempt(session_disposition, session_open_us),
                    ));
                }
                Err(PreviewPacketSourceOpenError::Failed(error)) => return Err(error),
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
            interrupt_state.set_checkpoint(PreviewDecodeCancellationCheckpoint::ExternalProcess);
            if should_cancel() {
                return Ok(PreviewDecodeOutcome::Canceled(
                    interrupt_state
                        .cancellation(PreviewDecodeCancellationCheckpoint::ExternalProcess)
                        .with_session_attempt(session_disposition, session_open_us),
                ));
            }
            let external_started_at = Instant::now();
            if let Some(result) = try_decode_with_external_ffmpeg_cpu_rgba(
                path,
                video_stream_index,
                source_sample,
                session.stream_tb,
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
                            return Ok(PreviewDecodeOutcome::Canceled(
                                interrupt_state
                                    .cancellation(
                                        PreviewDecodeCancellationCheckpoint::ExternalProcess,
                                    )
                                    .with_session_attempt(session_disposition, session_open_us),
                            ));
                        }
                        let frame = frame
                            .with_access_mode(access_mode)
                            .with_seek_strategy(
                                PreviewDecodeAccessPolicy::for_access_mode(access_mode)
                                    .seek_strategy,
                            )
                            .with_session_disposition(session_disposition)
                            .with_stage_durations(PreviewDecodeStageDurations {
                                session_open_us,
                                output_lease_wait_us,
                                external_process_us,
                                ..PreviewDecodeStageDurations::default()
                            })
                            .with_hardware_decode_plan(&external_hardware_decode_plan)
                            .with_elapsed(started_at.elapsed());
                        if external_exact_frame_is_publishable(path, &frame)? {
                            return finalize_preview_decode_outcome(
                                path,
                                fingerprint,
                                PreviewDecodeOutcome::Frame(frame),
                            );
                        }
                        // Rawvideo stdout proves raster bytes only. Without a
                        // selected stream PTS and presentation extent it cannot
                        // satisfy deterministic Still semantics, so discard it
                        // and continue through the in-process exact decoder.
                        preview_trace(
                            "[preview] external ffmpeg still lacks exact temporal evidence; fallback in-process"
                                .to_owned(),
                        );
                    }
                    Ok(None) => {
                        return Ok(PreviewDecodeOutcome::Canceled(
                            interrupt_state
                                .cancellation(PreviewDecodeCancellationCheckpoint::ExternalProcess)
                                .with_session_attempt(session_disposition, session_open_us),
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

        let outcome = match session.decode_at(
            source_sample,
            access_mode,
            adaptive_hints,
            should_cancel.as_ref(),
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                // A failed send/receive/materialization contract cannot leave
                // mutable demux, codec, DPB, or device state eligible for the
                // next request. Retire this device generation from new
                // acquisitions while existing Session Arcs remain valid.
                session.retire_hardware_device_context();
                execution_observer.publish_stage(PreviewDecodeExecutionStage::SessionRetire);
                *slot = None;
                return Err(error);
            }
        };
        match outcome {
            PreviewDecodeOutcome::Frame(frame) => {
                let result_session_disposition = if frame.diagnostics.session_disposition
                    == PreviewDecodeSessionDisposition::BypassedCache
                {
                    PreviewDecodeSessionDisposition::BypassedCache
                } else {
                    session_disposition
                };
                Ok(PreviewDecodeOutcome::Frame(
                    frame
                        .with_access_mode(access_mode)
                        .with_seek_strategy(
                            PreviewDecodeAccessPolicy::for_access_mode(access_mode).seek_strategy,
                        )
                        .with_session_disposition(result_session_disposition)
                        .with_stage_durations(PreviewDecodeStageDurations {
                            session_open_us,
                            output_lease_wait_us,
                            external_process_us,
                            ..PreviewDecodeStageDurations::default()
                        })
                        .with_elapsed(started_at.elapsed()),
                ))
            }
            PreviewDecodeOutcome::FloatFrame(frame) => {
                let result_session_disposition = if frame.diagnostics.session_disposition
                    == PreviewDecodeSessionDisposition::BypassedCache
                {
                    PreviewDecodeSessionDisposition::BypassedCache
                } else {
                    session_disposition
                };
                Ok(PreviewDecodeOutcome::FloatFrame(
                    frame
                        .with_access_mode(access_mode)
                        .with_seek_strategy(
                            PreviewDecodeAccessPolicy::for_access_mode(access_mode).seek_strategy,
                        )
                        .with_session_disposition(result_session_disposition)
                        .with_stage_durations(PreviewDecodeStageDurations {
                            session_open_us,
                            output_lease_wait_us,
                            external_process_us,
                            ..PreviewDecodeStageDurations::default()
                        })
                        .with_elapsed(started_at.elapsed()),
                ))
            }
            PreviewDecodeOutcome::NativeGpuFrame(mut frame) => {
                frame.diagnostics = frame
                    .diagnostics
                    .with_access_mode(access_mode)
                    .with_access_policy(PreviewDecodeAccessPolicy::for_access_mode(access_mode))
                    .with_elapsed(started_at.elapsed());
                frame.diagnostics.session_disposition = session_disposition;
                frame.diagnostics.stage_durations.accumulate(PreviewDecodeStageDurations {
                    session_open_us,
                    output_lease_wait_us,
                    external_process_us,
                    ..PreviewDecodeStageDurations::default()
                });
                Ok(PreviewDecodeOutcome::NativeGpuFrame(frame))
            }
            PreviewDecodeOutcome::Canceled(cancellation) => {
                if cancellation_requires_session_retirement(cancellation) {
                    execution_observer.publish_stage(PreviewDecodeExecutionStage::SessionRetire);
                    *slot = None;
                } else {
                    session.recover_after_cancellation();
                }
                let cancellation = if cancellation.session_disposition
                    == PreviewDecodeSessionDisposition::Unspecified
                {
                    cancellation.with_session_attempt(session_disposition, session_open_us)
                } else {
                    cancellation
                };
                Ok(PreviewDecodeOutcome::Canceled(cancellation))
            }
        }
    };
    finalize_preview_decode_outcome(path, fingerprint, outcome?)
}

pub(super) fn external_exact_frame_is_publishable(path: &Path, frame: &RgbaFrame) -> Result<bool> {
    match validate_preview_temporal_contract(path, &frame.diagnostics) {
        Ok(()) => Ok(true),
        Err(MondrianError::DecodeTemporalMismatch { .. }) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Bind a successful frame to the exact revision that authorized its decode,
/// then revalidate that revision after all demux, codec, conversion, and copy
/// work. This closes the replacement race between execution admission and
/// result publication.
///
/// The filesystem revalidation is intentionally skipped for reused-session
/// results: the request start already verified the revision after the output
/// lease wait and before any session admission, and a reused session decodes
/// from its own open file descriptor, so the residual swap window is empty.
/// Only long-lived session opens/replacements revalidate here.
pub(super) fn finalize_preview_decode_outcome(
    path: &Path,
    fingerprint: MediaFileFingerprint,
    outcome: PreviewDecodeOutcome,
) -> Result<PreviewDecodeOutcome> {
    let diagnostics = match &outcome {
        PreviewDecodeOutcome::Frame(frame) => Some(&frame.diagnostics),
        PreviewDecodeOutcome::FloatFrame(frame) => Some(&frame.diagnostics),
        PreviewDecodeOutcome::NativeGpuFrame(frame) => Some(&frame.diagnostics),
        PreviewDecodeOutcome::Canceled(_) => None,
    };
    if let Some(diagnostics) = diagnostics {
        if matches!(
            diagnostics.session_disposition,
            PreviewDecodeSessionDisposition::Opened | PreviewDecodeSessionDisposition::Replaced
        ) {
            verify_preview_source_revision(path, fingerprint)?;
        }
        validate_preview_temporal_contract(path, diagnostics)?;
    }
    Ok(outcome)
}

fn validate_preview_temporal_contract(
    path: &Path,
    diagnostics: &PreviewDecodeDiagnostics,
) -> Result<()> {
    let selected_extent = DecodedTemporalExtent::from_diagnostics(*diagnostics);
    let covers_request = diagnostics
        .requested_pts
        .zip(selected_extent)
        .is_some_and(|(requested_pts, extent)| extent.covers(requested_pts));
    let has_selection_evidence = diagnostics.requested_pts.is_some() && selected_extent.is_some();
    let approximation_is_consistent = diagnostics.temporal_approximation != covers_request;
    let access_contract_satisfied = match diagnostics.access_mode {
        PreviewDecodeAccessMode::ScrubCursor => {
            has_selection_evidence && approximation_is_consistent
        }
        PreviewDecodeAccessMode::PlaybackCursor
        | PreviewDecodeAccessMode::RandomAccessStillFrame => {
            has_selection_evidence
                && covers_request
                && approximation_is_consistent
                && !diagnostics.temporal_approximation
        }
    };
    if access_contract_satisfied {
        return Ok(());
    }
    Err(MondrianError::DecodeTemporalMismatch {
        asset_id: path.display().to_string(),
        access_mode: diagnostics.access_mode.as_str().to_owned(),
        requested_pts: diagnostics.requested_pts,
        selected_pts: diagnostics.selected_pts,
        selected_duration_pts: diagnostics.selected_duration_pts,
    })
}

fn cancellation_requires_session_retirement(cancellation: PreviewDecodeCancellation) -> bool {
    cancellation.source == PreviewDecodeCancellationSource::IsolatedDemuxTermination
}

fn packet_source_execution_family_matches(
    source_is_isolated: bool,
    demux_worker_available: bool,
) -> bool {
    source_is_isolated == demux_worker_available
}

#[cfg(test)]
mod session_topology_tests {
    use super::*;

    fn empty_sessions() -> PreviewDecodeSessions {
        PreviewDecodeSessions { playback: None, interactive: None, cpu_still: None }
    }

    fn retained_candidate(start_pts: i64, duration_pts: i64) -> RetainedDecodedCandidate {
        RetainedDecodedCandidate {
            extent: DecodedTemporalExtent::from_duration(start_pts, duration_pts),
            frame: RetainedDecodedFrame(ffmpeg::util::frame::video::Video::empty()),
        }
    }

    #[test]
    fn isolated_demux_termination_retires_instead_of_flushing_the_session() {
        assert!(cancellation_requires_session_retirement(
            PreviewDecodeCancellation::isolated_demux_termination(
                PreviewDecodeCancellationCheckpoint::PacketRead,
            )
        ));
        assert!(!cancellation_requires_session_retirement(
            PreviewDecodeCancellation::cooperative(PreviewDecodeCancellationCheckpoint::Codec)
        ));
        assert!(!cancellation_requires_session_retirement(
            PreviewDecodeCancellation::ffmpeg_interrupt(
                PreviewDecodeCancellationCheckpoint::PacketRead,
            )
        ));
    }

    #[test]
    fn gpu_scrub_and_exact_share_one_latest_wins_session_slot() {
        let sessions = empty_sessions();
        let request = PreviewHardwareDecodeRequest::PreferGpuResident;

        assert_eq!(
            sessions.available_slot(PreviewDecodeAccessMode::ScrubCursor, request),
            Some(PreviewDecodeSessionSlot::Interactive)
        );
        assert_eq!(
            sessions.available_slot(PreviewDecodeAccessMode::RandomAccessStillFrame, request),
            Some(PreviewDecodeSessionSlot::Interactive)
        );
    }

    #[test]
    fn cpu_exact_keeps_a_physically_separate_slot() {
        let sessions = empty_sessions();
        let cpu_slot = sessions
            .available_slot(
                PreviewDecodeAccessMode::RandomAccessStillFrame,
                PreviewHardwareDecodeRequest::Auto,
            )
            .expect("CPU still slot should be available");
        assert_eq!(cpu_slot, PreviewDecodeSessionSlot::CpuStill);
    }

    #[test]
    fn realtime_access_modes_keep_dedicated_single_slots() {
        let sessions = empty_sessions();
        assert_eq!(
            sessions.available_slot(
                PreviewDecodeAccessMode::PlaybackCursor,
                PreviewHardwareDecodeRequest::PreferGpuResident,
            ),
            Some(PreviewDecodeSessionSlot::Playback)
        );
        assert_eq!(
            sessions.available_slot(
                PreviewDecodeAccessMode::ScrubCursor,
                PreviewHardwareDecodeRequest::PreferGpuResident,
            ),
            Some(PreviewDecodeSessionSlot::Interactive)
        );
        assert!(!PreviewDecodeSessionSlot::Playback.requires_released_native_outputs());
        assert!(PreviewDecodeSessionSlot::Interactive.requires_released_native_outputs());
        assert!(!PreviewDecodeSessionSlot::CpuStill.requires_released_native_outputs());
    }

    #[test]
    fn context_clear_retires_sessions_without_erasing_family_output_evidence() {
        const FIXTURE: &[u8] = include_bytes!("../../../../tests/fixtures/small/h264-bframes.mp4");
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("context-clear-output-evidence.mp4");
        std::fs::write(&path, FIXTURE).expect("write synthetic H.264 fixture");
        let resources = PreviewDecodeWorkerResources::default();
        let mut context = PreviewDecodeSessionContext::with_worker_resources(resources.clone());
        let request = PreviewDecodeRequest::new(
            path.as_path(),
            SourceSampleTarget::covering(TimelineTime::ZERO),
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            PreviewSourceColorContract::automatic(ColorSpace::Rec709, DecodedVideoRange::Limited),
        )
        .with_max_size(Some(64), Some(64));
        context
            .decode_cancellable(request, || false)
            .expect("fixture must create one decoder Session");
        assert_eq!(context.resident_session_count(), 1);

        let session_tracker = PreviewNativeOutputTracker::default();
        let output = PreviewDecodeSessionOutputLease::acquire(
            resources.native_output_tracker(),
            &session_tracker,
        )
        .expect("logical native output evidence");
        assert!(!context.native_outputs_released());

        context.clear();
        assert_eq!(context.resident_session_count(), 0);
        assert!(!context.native_outputs_released());

        drop(output);
        assert!(context.native_outputs_released());
    }

    #[test]
    fn packet_source_execution_family_tracks_worker_availability_only() {
        assert!(!packet_source_execution_family_matches(false, true));
        assert!(packet_source_execution_family_matches(true, true));
        assert!(packet_source_execution_family_matches(false, false));
        assert!(!packet_source_execution_family_matches(true, false));
    }

    #[test]
    fn temporal_lookahead_is_retained_only_beyond_the_selected_frame() {
        let selected = DecodedTemporalExtent::from_duration(100, 20);
        let mut candidates = RetainedDecodedCandidateWindow::new(100);
        candidates.seed(retained_candidate(100, 20));
        candidates.seed(retained_candidate(120, 20));
        let retained = candidates
            .take_successor(selected)
            .expect("decoded successor must survive the selection boundary");
        assert_eq!(retained.extent.start_pts, 120);
        assert!(candidates.after().is_none());

        let mut selected_after = RetainedDecodedCandidateWindow::new(100);
        selected_after.seed(retained_candidate(120, 20));
        assert!(selected_after
            .take_successor(DecodedTemporalExtent::from_duration(120, 20))
            .is_none());
    }

    #[test]
    fn candidate_window_keeps_extrema_under_non_monotonic_output() {
        let mut candidates = RetainedDecodedCandidateWindow::new(110);
        for candidate in [
            retained_candidate(130, 1),
            retained_candidate(80, 1),
            retained_candidate(120, 1),
            retained_candidate(100, 1),
            retained_candidate(90, 1),
            retained_candidate(140, 1),
        ] {
            candidates.insert(candidate, true);
        }

        assert_eq!(
            candidates.before().map(|value| value.extent.start_pts),
            Some(100)
        );
        assert_eq!(
            candidates.after().map(|value| value.extent.start_pts),
            Some(120)
        );

        let mut high_water = None;
        for pts in [130, 80, 120, 100, 140] {
            advance_decoded_pts_high_water(&mut high_water, pts);
        }
        assert_eq!(high_water, Some(140));
    }

    #[test]
    fn duplicate_pts_keep_first_for_scrub_but_fail_exact_closed() {
        let mut candidates = RetainedDecodedCandidateWindow::new(100);
        candidates.insert(retained_candidate(100, 10), true);
        candidates.insert(retained_candidate(100, 40), true);

        assert_eq!(
            candidates.before().map(|value| value.extent.duration_pts),
            Some(Some(10))
        );
        assert!(candidates
            .validate_exact_ordering(
                Path::new("duplicate-pts.mov"),
                PreviewDecodeAccessMode::RandomAccessStillFrame,
            )
            .is_err());
        candidates
            .validate_exact_ordering(
                Path::new("duplicate-pts.mov"),
                PreviewDecodeAccessMode::ScrubCursor,
            )
            .expect("scrub uses the deterministic first duplicate candidate");
    }
}
