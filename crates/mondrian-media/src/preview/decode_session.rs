//! One access-mode-specific FFmpeg Preview decode Session.
//!
//! This deep module owns format/codec setup, request-scoped interrupt state,
//! stream discovery, seek/index use, packet/codec backpressure, forward reuse,
//! typed frame materialization, and final cancellation publication. The
//! parent module exposes only the request/outcome contract and session reset.

use super::*;

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

use hardware_decode::{
    preview_hardware_decode_get_format, preview_hardware_frame_format,
    PreviewHardwareDecodeContextState, PreviewHardwareDecodePlan,
};

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
    pub(super) fn cache_cpu_frame(
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

    pub(super) fn source_color(&self) -> Option<PreviewSourceColorContract> {
        match self {
            Self::CpuRgba(frame) => Some(frame.color_contract.source),
            Self::CpuFloat(frame) => Some(frame.color_contract.source),
            Self::NativeGpu(_) => None,
        }
    }

    pub(super) fn into_cache_hit(
        self,
        elapsed: Duration,
        access_mode: PreviewDecodeAccessMode,
    ) -> Self {
        match self {
            Self::CpuRgba(frame) => Self::CpuRgba(frame.into_cache_hit(elapsed, access_mode)),
            Self::CpuFloat(frame) => Self::CpuFloat(frame.into_cache_hit(elapsed, access_mode)),
            Self::NativeGpu(frame) => Self::NativeGpu(frame),
        }
    }

    pub(super) fn into_playback_ring_hit(self, elapsed: Duration) -> Self {
        match self {
            Self::CpuRgba(frame) => Self::CpuRgba(frame.into_playback_ring_hit(elapsed)),
            Self::CpuFloat(frame) => Self::CpuFloat(frame.into_playback_ring_hit(elapsed)),
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

use seek_index::{
    preview_seek_index_cache_get, preview_seek_index_cache_put, preview_seek_index_from_stream,
    PreviewSeekIndex, PreviewSeekIndexDiagnostics, PreviewSeekResolution,
};

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

            if policy.forward_decode_budget_exhausted(forward_decode_work_units(
                frames_decoded,
                video_packets_submitted,
            )) {
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

            self.decoder.send_packet(&packet).map_err(|e| MondrianError::DecodeFailed {
                asset_id: self.path.display().to_string(),
                reason: e.to_string(),
            })?;
            video_packets_submitted = video_packets_submitted.saturating_add(1);

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

                if policy.forward_decode_budget_exhausted(forward_decode_work_units(
                    frames_decoded,
                    video_packets_submitted,
                )) {
                    break;
                }
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

                if policy.forward_decode_budget_exhausted(forward_decode_work_units(
                    frames_decoded,
                    video_packets_submitted,
                )) {
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
        let current_match = preview_decode_session_may_reuse(access_mode, hardware_decode_request)
            && slot
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
            // Release the previous decoder and all surfaces it owns before
            // opening the replacement. Holding both pools during open can
            // exhaust constrained hardware decoders and make cancellation
            // wait inside the codec driver.
            *slot = None;
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

pub(super) fn preview_decode_session_may_reuse(
    access_mode: PreviewDecodeAccessMode,
    hardware_decode_request: PreviewHardwareDecodeRequest,
) -> bool {
    // A deterministic GPU-resident still request can cross arbitrary GOPs.
    // Reopening releases the prior decoder's DPB/surface pool before the next
    // seek instead of depending on backend-specific flush semantics. CPU still
    // sessions and realtime access modes retain their cheaper reuse path.
    !(matches!(access_mode, PreviewDecodeAccessMode::RandomAccessStillFrame)
        && hardware_decode_request.prefers_gpu_residency())
}
