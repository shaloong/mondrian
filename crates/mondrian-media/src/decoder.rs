//! 解码器池
//!
//! Coordinates preview decode concurrency, cache residency, and access-mode requests.

use crate::preview::{
    decode_preview_rgba_scaled_cancellable, PreviewDecodeAccessMode, PreviewDecodeRgbaRequest,
    PreviewFileFingerprint, RgbaFrame,
};
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use lru::LruCache;
use mondrian_core::{types::*, MondrianError, Result};
use parking_lot::Mutex;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::runtime::Runtime;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

/// GPU 硬件加速后端
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwAccelBackend {
    /// 纯 CPU 软解码
    None,
    /// NVIDIA NVDEC
    Cuda,
    /// Windows DirectX 11 Video Acceleration
    D3D11VA,
    /// macOS/iOS VideoToolbox
    VideoToolbox,
    /// Linux VA-API
    Vaapi,
}

/// Residency of frames produced by the decoder boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodedFrameResidency {
    /// Decoder output is CPU RGBA memory.
    CpuRgba,
    /// Decoder output is a GPU texture or hardware frame.
    GpuTexture,
}

/// Native hardware-frame handle family produced by a decoder.
///
/// This enum names the cross-crate contract only. It does not claim that
/// Mondrian can import the handle into the renderer; that requires a separate
/// renderer/platform import probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecodedGpuFrameHandleKind {
    /// Windows D3D11 `ID3D11Texture2D` hardware decode surface.
    D3D11Texture2D,
    /// macOS/iOS `CVPixelBuffer` backed by an IOSurface.
    CVPixelBuffer,
    /// Linux VA-API `VASurfaceID`/DMABUF-exportable surface.
    VaapiSurface,
    /// CUDA/NVDEC device allocation.
    CudaDeviceMemory,
}

impl DecodedGpuFrameHandleKind {
    /// Stable handle-kind name for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::D3D11Texture2D => "D3D11Texture2D",
            Self::CVPixelBuffer => "CVPixelBuffer",
            Self::VaapiSurface => "VaapiSurface",
            Self::CudaDeviceMemory => "CudaDeviceMemory",
        }
    }
}

/// Hardware decode / zero-copy probe result for the current process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HwAccelProbe {
    /// Backend that is actually active for the media decode boundary.
    pub selected_backend: HwAccelBackend,
    /// Whether the media decode boundary currently uses a hardware decoder.
    pub hardware_decode_active: bool,
    /// Whether decoded frames currently remain GPU-resident through the media boundary.
    pub zero_copy_active: bool,
    /// Residency produced by the active decode path.
    pub frame_residency: DecodedFrameResidency,
    /// Native handle family produced by the active decoder, if GPU-resident.
    pub gpu_frame_handle_kind: Option<DecodedGpuFrameHandleKind>,
    /// Whether the active decode path has a renderer texture-import contract.
    pub renderer_import_ready: bool,
    /// Stable diagnostic reason for the selected path.
    pub reason: String,
}

impl HwAccelBackend {
    /// Return the backend that is actually active for the media decode boundary.
    ///
    /// This intentionally fails closed to `None` until Mondrian has a real
    /// hardware-frame path that exports/imports decoder textures into the
    /// renderer. Platform preference alone must not be reported as active
    /// hardware decode.
    pub fn detect() -> Self {
        Self::probe().selected_backend
    }

    /// Probe the active hardware decode / zero-copy residency state.
    pub fn probe() -> HwAccelProbe {
        HwAccelProbe {
            selected_backend: Self::None,
            hardware_decode_active: false,
            zero_copy_active: false,
            frame_residency: DecodedFrameResidency::CpuRgba,
            gpu_frame_handle_kind: None,
            renderer_import_ready: false,
            reason: hardware_decode_unavailable_reason().to_owned(),
        }
    }

    /// Stable backend name for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Cuda => "Cuda",
            Self::D3D11VA => "D3D11VA",
            Self::VideoToolbox => "VideoToolbox",
            Self::Vaapi => "Vaapi",
        }
    }
}

impl DecodedFrameResidency {
    /// Stable residency name for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CpuRgba => "CpuRgba",
            Self::GpuTexture => "GpuTexture",
        }
    }
}

fn hardware_decode_unavailable_reason() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "D3D11VA/DXVA hardware decode texture residency is not connected; using CPU RGBA decode"
    }
    #[cfg(target_os = "macos")]
    {
        "VideoToolbox hardware decode texture residency is not connected; using CPU RGBA decode"
    }
    #[cfg(target_os = "linux")]
    {
        "VA-API hardware decode texture residency is not connected; using CPU RGBA decode"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        "hardware decode texture residency is not connected for this platform; using CPU RGBA decode"
    }
}

#[derive(Clone)]
struct PrefetchTask {
    asset_id: AssetId,
    cancelled: Arc<AtomicBool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RgbaFrameKey {
    asset_id: AssetId,
    path: PathBuf,
    fingerprint: PreviewFileFingerprint,
    source_micros: i64,
    width: u32,
    height: u32,
    access_mode: PreviewDecodeAccessMode,
}

#[derive(Default)]
struct DecoderMetrics {
    rgba_requests: AtomicU64,
    rgba_cache_hits: AtomicU64,
    decode_executions: AtomicU64,
    decode_failures: AtomicU64,
    decode_timeouts: AtomicU64,
    decode_budget_exhausted: AtomicU64,
    decode_failures_by_access_mode: DecoderAccessModeCounters,
    decode_timeouts_by_access_mode: DecoderAccessModeCounters,
    decode_budget_exhausted_by_access_mode: DecoderAccessModeCounters,
    prefetch_started: AtomicU64,
    prefetch_cancelled: AtomicU64,
    prefetch_completed: AtomicU64,
    total_decode_ns: AtomicU64,
}

#[derive(Default)]
struct DecoderAccessModeCounters {
    playback_cursor: AtomicU64,
    scrub_cursor: AtomicU64,
    random_access_still: AtomicU64,
}

/// Per-access-mode decoder counter snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecoderAccessModeMetricsSnapshot {
    /// Count attributed to sustained timeline playback requests.
    pub playback_cursor: u64,
    /// Count attributed to active playhead dragging / jog / shuttle requests.
    pub scrub_cursor: u64,
    /// Count attributed to deterministic still-frame extraction requests.
    pub random_access_still: u64,
}

impl DecoderAccessModeCounters {
    fn increment(&self, access_mode: PreviewDecodeAccessMode) {
        match access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => {
                self.playback_cursor.fetch_add(1, Ordering::Relaxed);
            }
            PreviewDecodeAccessMode::ScrubCursor => {
                self.scrub_cursor.fetch_add(1, Ordering::Relaxed);
            }
            PreviewDecodeAccessMode::RandomAccessStillFrame => {
                self.random_access_still.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn snapshot(&self) -> DecoderAccessModeMetricsSnapshot {
        DecoderAccessModeMetricsSnapshot {
            playback_cursor: self.playback_cursor.load(Ordering::Relaxed),
            scrub_cursor: self.scrub_cursor.load(Ordering::Relaxed),
            random_access_still: self.random_access_still.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DecoderMetricsSnapshot {
    pub hw_accel_backend: HwAccelBackend,
    pub decoded_frame_residency: DecodedFrameResidency,
    pub decoded_gpu_frame_handle_kind: Option<DecodedGpuFrameHandleKind>,
    pub renderer_import_ready: bool,
    pub hardware_decode_active: bool,
    pub zero_copy_active: bool,
    pub hw_accel_reason: String,
    pub rgba_requests: u64,
    pub rgba_cache_hits: u64,
    pub decode_executions: u64,
    pub decode_failures: u64,
    pub decode_timeouts: u64,
    pub decode_budget_exhausted: u64,
    /// Decode failures grouped by the caller's access-mode contract.
    pub decode_failures_by_access_mode: DecoderAccessModeMetricsSnapshot,
    /// Decode timeouts grouped by the caller's access-mode contract.
    pub decode_timeouts_by_access_mode: DecoderAccessModeMetricsSnapshot,
    /// Forward-decode budget exhaustions grouped by access-mode policy.
    pub decode_budget_exhausted_by_access_mode: DecoderAccessModeMetricsSnapshot,
    pub prefetch_started: u64,
    pub prefetch_cancelled: u64,
    pub prefetch_completed: u64,
    pub avg_decode_ms: f64,
    pub avg_decode_exec_ms: f64,
    pub decode_miss_rate_pct: f64,
}

impl DecoderMetrics {
    fn snapshot(&self, hw_probe: &HwAccelProbe) -> DecoderMetricsSnapshot {
        let rgba_requests = self.rgba_requests.load(Ordering::Relaxed);
        let decode_executions = self.decode_executions.load(Ordering::Relaxed);
        let total_decode_ns = self.total_decode_ns.load(Ordering::Relaxed);
        DecoderMetricsSnapshot {
            hw_accel_backend: hw_probe.selected_backend,
            decoded_frame_residency: hw_probe.frame_residency,
            decoded_gpu_frame_handle_kind: hw_probe.gpu_frame_handle_kind,
            renderer_import_ready: hw_probe.renderer_import_ready,
            hardware_decode_active: hw_probe.hardware_decode_active,
            zero_copy_active: hw_probe.zero_copy_active,
            hw_accel_reason: hw_probe.reason.clone(),
            rgba_requests,
            rgba_cache_hits: self.rgba_cache_hits.load(Ordering::Relaxed),
            decode_executions,
            decode_failures: self.decode_failures.load(Ordering::Relaxed),
            decode_timeouts: self.decode_timeouts.load(Ordering::Relaxed),
            decode_budget_exhausted: self.decode_budget_exhausted.load(Ordering::Relaxed),
            decode_failures_by_access_mode: self.decode_failures_by_access_mode.snapshot(),
            decode_timeouts_by_access_mode: self.decode_timeouts_by_access_mode.snapshot(),
            decode_budget_exhausted_by_access_mode: self
                .decode_budget_exhausted_by_access_mode
                .snapshot(),
            prefetch_started: self.prefetch_started.load(Ordering::Relaxed),
            prefetch_cancelled: self.prefetch_cancelled.load(Ordering::Relaxed),
            prefetch_completed: self.prefetch_completed.load(Ordering::Relaxed),
            avg_decode_ms: if rgba_requests > 0 {
                (total_decode_ns as f64 / rgba_requests as f64) / 1_000_000.0
            } else {
                0.0
            },
            avg_decode_exec_ms: if decode_executions > 0 {
                (total_decode_ns as f64 / decode_executions as f64) / 1_000_000.0
            } else {
                0.0
            },
            decode_miss_rate_pct: if rgba_requests > 0 {
                decode_executions as f64 / rgba_requests as f64 * 100.0
            } else {
                0.0
            },
        }
    }
}

impl DecoderMetrics {
    fn record_decode_failure(&self, access_mode: PreviewDecodeAccessMode, err: &MondrianError) {
        self.decode_failures.fetch_add(1, Ordering::Relaxed);
        self.decode_failures_by_access_mode.increment(access_mode);
        if matches!(err, MondrianError::DecodeTimeout { .. }) {
            self.decode_timeouts.fetch_add(1, Ordering::Relaxed);
            self.decode_timeouts_by_access_mode.increment(access_mode);
        }
        if matches!(err, MondrianError::DecodeBudgetExhausted { .. }) {
            self.decode_budget_exhausted.fetch_add(1, Ordering::Relaxed);
            self.decode_budget_exhausted_by_access_mode.increment(access_mode);
        }
    }
}

/// 解码器池
///
/// - Limits concurrent preview decode work with a semaphore.
/// - Coalesces in-flight RGBA preview requests by access-mode-aware cache key.
/// - Keeps decoded frame caches and prefetch task cancellation state.
pub struct DecoderPool {
    semaphore: Arc<Semaphore>,
    hw_accel_probe: HwAccelProbe,
    prefetch_tasks: DashMap<u64, PrefetchTask>,
    next_prefetch_task_id: AtomicU64,
    rgba_cache: Arc<Mutex<LruCache<RgbaFrameKey, Arc<RgbaFrame>>>>,
    rgba_inflight: Arc<DashMap<RgbaFrameKey, Arc<Notify>>>,
    preview_decode_runtime: Arc<Runtime>,
    background_runtime: Arc<Runtime>,
    metrics: DecoderMetrics,
}

/// Request for one access-mode-aware DecoderPool RGBA preview frame.
///
/// This is the DecoderPool interface for playback, scrub, and deterministic
/// still-frame preview work. Callers choose an explicit access mode; DecoderPool
/// owns how that mode maps to in-flight coalescing, cache keys, cancellation,
/// runtime scheduling, and future hardware-resident decode adapters.
#[derive(Debug, Clone)]
pub struct DecoderPoolPreviewRgbaRequest {
    /// Asset identity used for diagnostics and decoder context ownership.
    pub asset_id: AssetId,
    /// Source media path to decode.
    pub path: PathBuf,
    /// Source timeline/media time to decode.
    pub timecode: TimeCode,
    /// Maximum output width requested by the preview surface.
    pub target_width: u32,
    /// Maximum output height requested by the preview surface.
    pub target_height: u32,
    /// Access pattern that drives decoder residency and seek policy.
    pub access_mode: PreviewDecodeAccessMode,
    cancelled: Option<Arc<AtomicBool>>,
}

impl DecoderPoolPreviewRgbaRequest {
    /// Create one DecoderPool RGBA preview request.
    pub fn new(
        asset_id: AssetId,
        path: PathBuf,
        timecode: TimeCode,
        target_width: u32,
        target_height: u32,
        access_mode: PreviewDecodeAccessMode,
    ) -> Self {
        Self {
            asset_id,
            path,
            timecode,
            target_width,
            target_height,
            access_mode,
            cancelled: None,
        }
    }

    fn with_cancellation_flag(mut self, cancelled: Arc<AtomicBool>) -> Self {
        self.cancelled = Some(cancelled);
        self
    }
}

impl DecoderPool {
    pub fn new() -> Arc<Self> {
        let max_concurrent = (num_cpus() - 2).max(1);
        let hw_accel_probe = HwAccelBackend::probe();
        Arc::new(Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            hw_accel_probe,
            prefetch_tasks: DashMap::new(),
            next_prefetch_task_id: AtomicU64::new(1),
            rgba_cache: Arc::new(Mutex::new(LruCache::new(
                NonZeroUsize::new(256).expect("256 is non-zero"),
            ))),
            rgba_inflight: Arc::new(DashMap::new()),
            preview_decode_runtime: Arc::new(
                // FFmpeg preview decode is synchronous CPU work; keep async
                // orchestration tiny and run actual decode on bounded blocking
                // threads coordinated by PreviewDecodeCpuBudget.
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .max_blocking_threads(preview_decode_worker_threads())
                    .enable_all()
                    .build()
                    .expect("failed to create DecoderPool preview decode runtime"),
            ),
            background_runtime: Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("failed to create DecoderPool background runtime"),
            ),
            metrics: DecoderMetrics::default(),
        })
    }

    /// Get one access-mode-aware preview frame as CPU RGBA memory.
    pub async fn get_preview_rgba(
        &self,
        request: DecoderPoolPreviewRgbaRequest,
    ) -> Result<Arc<RgbaFrame>> {
        self.metrics.rgba_requests.fetch_add(1, Ordering::Relaxed);
        let DecoderPoolPreviewRgbaRequest {
            asset_id,
            path,
            timecode,
            target_width,
            target_height,
            access_mode,
            cancelled,
        } = request;
        if decode_cancelled(cancelled.as_ref()) {
            return Err(mondrian_core::MondrianError::Cancelled);
        }
        let frame_num = timecode.frame.max(0) as u64;
        let secs = timecode.to_secs().max(0.0);
        let source_micros = source_time_micros(secs);
        let fingerprint = PreviewFileFingerprint::capture(path.as_path());
        let key = RgbaFrameKey {
            asset_id,
            path: path.clone(),
            fingerprint,
            source_micros,
            width: target_width,
            height: target_height,
            access_mode,
        };

        let notify = loop {
            if decode_cancelled(cancelled.as_ref()) {
                return Err(mondrian_core::MondrianError::Cancelled);
            }
            if let Some(hit) = self.rgba_cache.lock().get(&key).cloned() {
                self.metrics.rgba_cache_hits.fetch_add(1, Ordering::Relaxed);
                return Ok(hit);
            }
            match self.rgba_inflight.entry(key.clone()) {
                Entry::Occupied(entry) => {
                    let waiter = Arc::clone(entry.get());
                    drop(entry);
                    wait_for_inflight_or_cancel(waiter, cancelled.clone()).await?;
                }
                Entry::Vacant(entry) => {
                    let notify = Arc::new(Notify::new());
                    entry.insert(Arc::clone(&notify));
                    break notify;
                }
            }
        };

        self.metrics.decode_executions.fetch_add(1, Ordering::Relaxed);
        let permit =
            acquire_decode_permit_or_cancel(Arc::clone(&self.semaphore), cancelled.clone()).await?;
        if decode_cancelled(cancelled.as_ref()) {
            self.rgba_inflight.remove(&key);
            notify.notify_waiters();
            return Err(mondrian_core::MondrianError::Cancelled);
        }

        tracing::debug!(
            "[decoder] rgba request start asset={} frame={} target={}x{}",
            asset_id,
            frame_num,
            target_width,
            target_height
        );

        let started = Instant::now();
        let decode_cancel_flag = cancelled.clone();
        let decode_cache = Arc::clone(&self.rgba_cache);
        let decode_inflight = Arc::clone(&self.rgba_inflight);
        let decode_key = key.clone();
        let decode_notify = Arc::clone(&notify);
        let mut decode_task = self.preview_decode_runtime.spawn_blocking(move || {
            let _permit = permit;
            let decode_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let request = PreviewDecodeRgbaRequest::new(path.as_path(), secs, access_mode)
                    .with_max_size(Some(target_width.max(1)), Some(target_height.max(1)))
                    .with_fingerprint(fingerprint);
                decode_preview_rgba_scaled_cancellable(request, || {
                    decode_cancel_flag
                        .as_ref()
                        .map(|flag| flag.load(Ordering::Relaxed))
                        .unwrap_or(false)
                })
                .and_then(|outcome| match outcome {
                    crate::preview::PreviewDecodeOutcome::Frame(frame) => Ok(frame),
                    crate::preview::PreviewDecodeOutcome::Canceled => {
                        Err(mondrian_core::MondrianError::Cancelled)
                    }
                })
                .map(Arc::new)
            }));
            match decode_result {
                Ok(result) => {
                    finish_rgba_inflight_owner(
                        &decode_cache,
                        &decode_inflight,
                        &decode_key,
                        &decode_notify,
                        &result,
                    );
                    result
                }
                Err(payload) => {
                    let cleanup_result: Result<Arc<RgbaFrame>> =
                        Err(anyhow::anyhow!("preview decode panicked").into());
                    finish_rgba_inflight_owner(
                        &decode_cache,
                        &decode_inflight,
                        &decode_key,
                        &decode_notify,
                        &cleanup_result,
                    );
                    std::panic::resume_unwind(payload);
                }
            }
        });

        let decode_timeout_ms = decode_timeout_budget_ms(access_mode);
        let decode_result: Result<Arc<RgbaFrame>> = if decode_timeout_ms == 0 {
            decode_task.await.map_err(|e| MondrianError::DecodeFailed {
                asset_id: asset_id.to_string(),
                reason: e.to_string(),
            })?
        } else {
            let timeout = tokio::time::sleep(tokio::time::Duration::from_millis(decode_timeout_ms));
            let cancel_watch = wait_for_decode_cancel(cancelled.clone());
            tokio::pin!(timeout);
            tokio::pin!(cancel_watch);

            tokio::select! {
                joined = &mut decode_task => {
                    joined.map_err(|e| MondrianError::DecodeFailed {
                        asset_id: asset_id.to_string(),
                        reason: e.to_string(),
                    })?
                }
                _ = &mut timeout => {
                    tracing::warn!(
                        "MONDRIAN_DECODE_TIMEOUT_JSON={{\"asset_id\":\"{}\",\"frame\":{},\"source_micros\":{},\"access_mode\":\"{}\",\"secs\":{:.3},\"budget_ms\":{},\"target_width\":{},\"target_height\":{},\"reason\":\"decode timeout\"}}",
                        asset_id,
                        frame_num,
                        source_micros,
                        access_mode.as_str(),
                        secs,
                        decode_timeout_ms,
                        target_width,
                        target_height
                    );
                    Err(MondrianError::DecodeTimeout {
                        asset_id: asset_id.to_string(),
                        access_mode: access_mode.as_str().to_owned(),
                        budget_ms: decode_timeout_ms,
                        frame: frame_num,
                        secs,
                    })
                }
                _ = &mut cancel_watch => {
                    Err(MondrianError::Cancelled)
                }
            }
        };

        let decode_result = decode_result.inspect_err(|err| {
            self.metrics.record_decode_failure(access_mode, err);
        });

        self.metrics
            .total_decode_ns
            .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);

        let elapsed_ms = started.elapsed().as_millis() as u64;
        if elapsed_ms >= 40 {
            tracing::warn!(
                "[decoder] rgba request slow asset={} frame={} elapsed={}ms",
                asset_id,
                frame_num,
                elapsed_ms
            );
        }

        decode_result
    }

    /// Cancel background work and evict decoded frames for an asset.
    pub fn close_asset(&self, asset_id: AssetId) {
        self.cancel_prefetch_for_asset(asset_id);
        self.evict_rgba_asset(asset_id);
    }

    /// 启动 RGBA 预取任务（按目标预览分辨率缓存）。
    pub fn spawn_prefetch_rgba(
        self: &Arc<Self>,
        asset_id: AssetId,
        path: PathBuf,
        start_timecode: TimeCode,
        lookahead_frames: u32,
        target_width: u32,
        target_height: u32,
    ) -> u64 {
        let task_id = self.next_prefetch_task_id.fetch_add(1, Ordering::Relaxed);
        let cancelled = Arc::new(AtomicBool::new(false));
        self.prefetch_tasks.insert(
            task_id,
            PrefetchTask { asset_id, cancelled: cancelled.clone() },
        );
        self.metrics.prefetch_started.fetch_add(1, Ordering::Relaxed);

        let pool = Arc::clone(self);
        self.background_runtime.spawn(async move {
            let tb = start_timecode.time_base;
            let mut was_cancelled = false;
            for offset in 0..lookahead_frames {
                if cancelled.load(Ordering::Relaxed) {
                    was_cancelled = true;
                    break;
                }

                let tc = TimeCode::new(start_timecode.frame + offset as i64, tb);
                if let Err(err) = pool
                    .get_preview_rgba(
                        DecoderPoolPreviewRgbaRequest::new(
                            asset_id,
                            path.clone(),
                            tc,
                            target_width.max(1),
                            target_height.max(1),
                            PreviewDecodeAccessMode::PlaybackCursor,
                        )
                        .with_cancellation_flag(Arc::clone(&cancelled)),
                    )
                    .await
                {
                    if matches!(err, mondrian_core::MondrianError::Cancelled) {
                        was_cancelled = true;
                        break;
                    }
                    tracing::debug!(
                        "rgba prefetch failed: task_id={} asset={} frame={} err={}",
                        task_id,
                        asset_id,
                        tc.frame,
                        err
                    );
                    break;
                }
            }

            pool.prefetch_tasks.remove(&task_id);
            if was_cancelled {
                pool.metrics.prefetch_cancelled.fetch_add(1, Ordering::Relaxed);
            } else {
                pool.metrics.prefetch_completed.fetch_add(1, Ordering::Relaxed);
            }
        });

        task_id
    }

    /// 取消指定预取任务。
    pub fn cancel_prefetch_task(&self, task_id: u64) {
        if let Some(task) = self.prefetch_tasks.get(&task_id) {
            task.cancelled.store(true, Ordering::Relaxed);
        }
    }

    /// 取消某个素材的所有预取任务。
    pub fn cancel_prefetch_for_asset(&self, asset_id: AssetId) {
        for entry in self.prefetch_tasks.iter() {
            if entry.value().asset_id == asset_id {
                entry.value().cancelled.store(true, Ordering::Relaxed);
            }
        }
    }

    /// 取消所有预取任务。
    pub fn cancel_all_prefetch_tasks(&self) {
        for entry in self.prefetch_tasks.iter() {
            entry.value().cancelled.store(true, Ordering::Relaxed);
        }
    }

    /// 当前活跃预取任务数量。
    pub fn active_prefetch_task_count(&self) -> usize {
        self.prefetch_tasks.len()
    }

    /// 指定任务是否仍在活跃执行。
    pub fn is_prefetch_task_active(&self, task_id: u64) -> bool {
        self.prefetch_tasks.contains_key(&task_id)
    }

    pub fn metrics_snapshot(&self) -> DecoderMetricsSnapshot {
        self.metrics.snapshot(&self.hw_accel_probe)
    }

    pub fn clear_all_caches(&self) {
        self.cancel_all_prefetch_tasks();
        self.prefetch_tasks.clear();
        self.rgba_cache.lock().clear();
    }

    /// 清除全部 RGBA 图层帧缓存（大幅 seek 后调用，淘汰远离新位置的旧缓存帧）。
    pub fn evict_rgba_cache(&self) {
        self.rgba_cache.lock().clear();
        // 同步清除进程全局预览帧缓存。
        crate::preview::clear_global_preview_frame_cache();
    }

    fn evict_rgba_asset(&self, asset_id: AssetId) {
        let mut cache = self.rgba_cache.lock();
        let keys = cache
            .iter()
            .filter_map(|(key, _)| (key.asset_id == asset_id).then_some(key.clone()))
            .collect::<Vec<_>>();
        for key in keys {
            let _ = cache.pop(&key);
        }
    }
}

fn preview_decode_worker_threads() -> usize {
    let budget = crate::preview::preview_decode_cpu_budget();
    let max_threads = budget.preview_worker_count.max(1);
    std::env::var("MONDRIAN_PREVIEW_DECODE_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.clamp(1, max_threads))
        .unwrap_or(max_threads)
}

fn num_cpus() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PreviewDecodeTimeoutBudget {
    playback_cursor_ms: u64,
    scrub_cursor_ms: u64,
    random_access_still_ms: u64,
}

impl PreviewDecodeTimeoutBudget {
    const DEFAULT: Self = Self {
        playback_cursor_ms: 2500,
        scrub_cursor_ms: 1000,
        random_access_still_ms: 5000,
    };

    fn for_access_mode(self, access_mode: PreviewDecodeAccessMode) -> u64 {
        match access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => self.playback_cursor_ms,
            PreviewDecodeAccessMode::ScrubCursor => self.scrub_cursor_ms,
            PreviewDecodeAccessMode::RandomAccessStillFrame => self.random_access_still_ms,
        }
    }
}

fn decode_timeout_budget_ms(access_mode: PreviewDecodeAccessMode) -> u64 {
    static TIMEOUT_BUDGET: OnceLock<PreviewDecodeTimeoutBudget> = OnceLock::new();
    TIMEOUT_BUDGET
        .get_or_init(decode_timeout_budget_from_env)
        .for_access_mode(access_mode)
}

fn decode_timeout_budget_from_env() -> PreviewDecodeTimeoutBudget {
    let global = first_timeout_override([
        "MONDRIAN_DECODE_TIMEOUT_BUDGET_MS",
        "MONDRIAN_PREVIEW_DECODE_TIMEOUT_MS",
    ]);
    PreviewDecodeTimeoutBudget {
        playback_cursor_ms: timeout_override_or_default(
            ["MONDRIAN_PREVIEW_PLAYBACK_DECODE_TIMEOUT_MS"],
            global,
            PreviewDecodeTimeoutBudget::DEFAULT.playback_cursor_ms,
        ),
        scrub_cursor_ms: timeout_override_or_default(
            ["MONDRIAN_PREVIEW_SCRUB_DECODE_TIMEOUT_MS"],
            global,
            PreviewDecodeTimeoutBudget::DEFAULT.scrub_cursor_ms,
        ),
        random_access_still_ms: timeout_override_or_default(
            [
                "MONDRIAN_PREVIEW_STILL_DECODE_TIMEOUT_MS",
                "MONDRIAN_PREVIEW_RANDOM_ACCESS_STILL_DECODE_TIMEOUT_MS",
            ],
            global,
            PreviewDecodeTimeoutBudget::DEFAULT.random_access_still_ms,
        ),
    }
}

fn timeout_override_or_default<const N: usize>(
    keys: [&str; N],
    global: Option<u64>,
    default_ms: u64,
) -> u64 {
    first_timeout_override(keys).or(global).unwrap_or(default_ms)
}

fn first_timeout_override<const N: usize>(keys: [&str; N]) -> Option<u64> {
    keys.into_iter()
        .find_map(|key| std::env::var(key).ok().and_then(|value| parse_timeout_override_ms(&value)))
}

fn parse_timeout_override_ms(value: &str) -> Option<u64> {
    value.parse::<u64>().ok().filter(|value| *value == 0 || *value >= 100)
}

fn source_time_micros(secs: f64) -> i64 {
    (secs.max(0.0) * 1_000_000.0).round() as i64
}

fn finish_rgba_inflight_owner(
    cache: &Mutex<LruCache<RgbaFrameKey, Arc<RgbaFrame>>>,
    inflight: &DashMap<RgbaFrameKey, Arc<Notify>>,
    key: &RgbaFrameKey,
    notify: &Notify,
    result: &Result<Arc<RgbaFrame>>,
) {
    if let Ok(frame) = result {
        cache.lock().put(key.clone(), Arc::clone(frame));
    }
    inflight.remove(key);
    notify.notify_waiters();
}

fn decode_cancelled(cancelled: Option<&Arc<AtomicBool>>) -> bool {
    cancelled.map(|flag| flag.load(Ordering::Relaxed)).unwrap_or(false)
}

async fn wait_for_decode_cancel(cancelled: Option<Arc<AtomicBool>>) {
    let Some(cancelled) = cancelled else {
        std::future::pending::<()>().await;
        return;
    };
    while !cancelled.load(Ordering::Relaxed) {
        tokio::time::sleep(tokio::time::Duration::from_millis(2)).await;
    }
}

async fn acquire_decode_permit_or_cancel(
    semaphore: Arc<Semaphore>,
    cancelled: Option<Arc<AtomicBool>>,
) -> Result<OwnedSemaphorePermit> {
    if decode_cancelled(cancelled.as_ref()) {
        return Err(mondrian_core::MondrianError::Cancelled);
    }
    let acquire = semaphore.acquire_owned();
    let cancel_watch = wait_for_decode_cancel(cancelled);
    tokio::pin!(acquire);
    tokio::pin!(cancel_watch);
    tokio::select! {
        permit = &mut acquire => permit.map_err(|_| mondrian_core::MondrianError::Cancelled),
        _ = &mut cancel_watch => Err(mondrian_core::MondrianError::Cancelled),
    }
}

async fn wait_for_inflight_or_cancel(
    notify: Arc<Notify>,
    cancelled: Option<Arc<AtomicBool>>,
) -> Result<()> {
    if decode_cancelled(cancelled.as_ref()) {
        return Err(mondrian_core::MondrianError::Cancelled);
    }
    let notified = notify.notified();
    let cancel_watch = wait_for_decode_cancel(cancelled);
    tokio::pin!(notified);
    tokio::pin!(cancel_watch);
    tokio::select! {
        _ = &mut notified => Ok(()),
        _ = &mut cancel_watch => Err(mondrian_core::MondrianError::Cancelled),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hw_accel_probe_fails_closed_until_texture_residency_exists() {
        let probe = HwAccelBackend::probe();

        assert_eq!(probe.selected_backend, HwAccelBackend::None);
        assert!(!probe.hardware_decode_active);
        assert!(!probe.zero_copy_active);
        assert_eq!(probe.frame_residency, DecodedFrameResidency::CpuRgba);
        assert_eq!(probe.gpu_frame_handle_kind, None);
        assert!(!probe.renderer_import_ready);
        assert!(probe.reason.contains("CPU RGBA decode"));
    }

    #[test]
    fn decoded_gpu_frame_handle_kind_has_stable_names() {
        assert_eq!(
            DecodedGpuFrameHandleKind::D3D11Texture2D.as_str(),
            "D3D11Texture2D"
        );
        assert_eq!(
            DecodedGpuFrameHandleKind::CVPixelBuffer.as_str(),
            "CVPixelBuffer"
        );
        assert_eq!(
            DecodedGpuFrameHandleKind::VaapiSurface.as_str(),
            "VaapiSurface"
        );
        assert_eq!(
            DecodedGpuFrameHandleKind::CudaDeviceMemory.as_str(),
            "CudaDeviceMemory"
        );
    }

    #[test]
    fn decoder_metrics_report_cpu_residency_and_no_zero_copy() {
        let pool = DecoderPool::new();

        let snapshot = pool.metrics_snapshot();

        assert_eq!(snapshot.hw_accel_backend, HwAccelBackend::None);
        assert_eq!(
            snapshot.decoded_frame_residency,
            DecodedFrameResidency::CpuRgba
        );
        assert_eq!(snapshot.decoded_gpu_frame_handle_kind, None);
        assert!(!snapshot.renderer_import_ready);
        assert!(!snapshot.hardware_decode_active);
        assert!(!snapshot.zero_copy_active);
        assert!(snapshot.hw_accel_reason.contains("CPU RGBA decode"));
        assert_eq!(snapshot.decode_budget_exhausted, 0);
    }

    #[test]
    fn decoder_metrics_report_forward_budget_exhaustion() {
        let metrics = DecoderMetrics::default();
        metrics.record_decode_failure(
            PreviewDecodeAccessMode::PlaybackCursor,
            &MondrianError::DecodeTimeout {
                asset_id: "asset-playback".to_owned(),
                access_mode: PreviewDecodeAccessMode::PlaybackCursor.as_str().to_owned(),
                budget_ms: 200,
                frame: 10,
                secs: 0.4,
            },
        );
        metrics.record_decode_failure(
            PreviewDecodeAccessMode::ScrubCursor,
            &MondrianError::DecodeBudgetExhausted {
                asset_id: "asset-scrub".to_owned(),
                access_mode: PreviewDecodeAccessMode::ScrubCursor.as_str().to_owned(),
                decoded_frames: 240,
                budget_frames: 240,
                target_pts: 120,
            },
        );
        metrics.record_decode_failure(
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            &MondrianError::DecodeFailed {
                asset_id: "asset-still".to_owned(),
                reason: "synthetic still failure".to_owned(),
            },
        );

        let snapshot = metrics.snapshot(&HwAccelBackend::probe());

        assert_eq!(snapshot.decode_failures, 3);
        assert_eq!(snapshot.decode_timeouts, 1);
        assert_eq!(snapshot.decode_budget_exhausted, 1);
        assert_eq!(
            snapshot.decode_failures_by_access_mode,
            DecoderAccessModeMetricsSnapshot {
                playback_cursor: 1,
                scrub_cursor: 1,
                random_access_still: 1,
            }
        );
        assert_eq!(
            snapshot.decode_timeouts_by_access_mode,
            DecoderAccessModeMetricsSnapshot {
                playback_cursor: 1,
                scrub_cursor: 0,
                random_access_still: 0,
            }
        );
        assert_eq!(
            snapshot.decode_budget_exhausted_by_access_mode,
            DecoderAccessModeMetricsSnapshot {
                playback_cursor: 0,
                scrub_cursor: 1,
                random_access_still: 0,
            }
        );
    }

    #[test]
    fn rgba_frame_key_uses_source_time_not_bare_frame_number() {
        let asset_id = AssetId::new();
        let fingerprint = PreviewFileFingerprint {
            len: Some(10),
            modified_secs: Some(20),
            modified_nanos: Some(30),
        };
        let at_one_second = TimeCode::new(24, Rational::new(1, 24));
        let before_one_second = TimeCode::new(24, Rational::new(1, 30));

        let first = RgbaFrameKey {
            asset_id,
            path: PathBuf::from("E:/media/source.mov"),
            fingerprint,
            source_micros: source_time_micros(at_one_second.to_secs()),
            width: 1920,
            height: 1080,
            access_mode: PreviewDecodeAccessMode::PlaybackCursor,
        };
        let second = RgbaFrameKey {
            source_micros: source_time_micros(before_one_second.to_secs()),
            ..first.clone()
        };

        assert_ne!(first.source_micros, second.source_micros);
        assert_ne!(first, second);
    }

    #[test]
    fn decoder_pool_preview_rgba_request_preserves_explicit_access_contract() {
        let asset_id = AssetId::new();
        let path = PathBuf::from("E:/media/source.mov");
        let timecode = TimeCode::new(42, Rational::new(1, 24));
        let cancel = Arc::new(AtomicBool::new(false));

        let request = DecoderPoolPreviewRgbaRequest::new(
            asset_id,
            path.clone(),
            timecode,
            1920,
            1080,
            PreviewDecodeAccessMode::ScrubCursor,
        )
        .with_cancellation_flag(Arc::clone(&cancel));

        assert_eq!(request.asset_id, asset_id);
        assert_eq!(request.path, path);
        assert_eq!(request.timecode, timecode);
        assert_eq!(request.target_width, 1920);
        assert_eq!(request.target_height, 1080);
        assert_eq!(request.access_mode, PreviewDecodeAccessMode::ScrubCursor);
        assert!(Arc::ptr_eq(
            request.cancelled.as_ref().expect("cancel flag"),
            &cancel
        ));
    }

    #[test]
    fn preview_decode_timeout_budget_is_access_mode_specific() {
        let budget = PreviewDecodeTimeoutBudget::DEFAULT;

        assert_eq!(
            budget.for_access_mode(PreviewDecodeAccessMode::PlaybackCursor),
            2500
        );
        assert_eq!(
            budget.for_access_mode(PreviewDecodeAccessMode::ScrubCursor),
            1000
        );
        assert_eq!(
            budget.for_access_mode(PreviewDecodeAccessMode::RandomAccessStillFrame),
            5000
        );
    }

    #[test]
    fn preview_decode_timeout_override_parser_allows_disable_and_rejects_tiny_values() {
        assert_eq!(parse_timeout_override_ms("0"), Some(0));
        assert_eq!(parse_timeout_override_ms("100"), Some(100));
        assert_eq!(parse_timeout_override_ms("2500"), Some(2500));
        assert_eq!(parse_timeout_override_ms("99"), None);
        assert_eq!(parse_timeout_override_ms("-1"), None);
        assert_eq!(parse_timeout_override_ms("abc"), None);
    }

    #[test]
    fn rgba_frame_key_isolates_path_fingerprint_and_access_mode() {
        let base = RgbaFrameKey {
            asset_id: AssetId::new(),
            path: PathBuf::from("E:/media/source.mov"),
            fingerprint: PreviewFileFingerprint {
                len: Some(10),
                modified_secs: Some(20),
                modified_nanos: Some(30),
            },
            source_micros: 1_000_000,
            width: 1920,
            height: 1080,
            access_mode: PreviewDecodeAccessMode::PlaybackCursor,
        };

        assert_ne!(
            base,
            RgbaFrameKey {
                path: PathBuf::from("E:/media/proxy.mov"),
                ..base.clone()
            }
        );
        assert_ne!(
            base,
            RgbaFrameKey {
                fingerprint: PreviewFileFingerprint { len: Some(11), ..base.fingerprint },
                ..base.clone()
            }
        );
        assert_ne!(
            base,
            RgbaFrameKey {
                access_mode: PreviewDecodeAccessMode::ScrubCursor,
                ..base.clone()
            }
        );
    }

    #[test]
    fn decoder_cancel_flag_helper_reports_state() {
        let cancelled = Arc::new(AtomicBool::new(false));

        assert!(!decode_cancelled(None));
        assert!(!decode_cancelled(Some(&cancelled)));

        cancelled.store(true, Ordering::Relaxed);

        assert!(decode_cancelled(Some(&cancelled)));
    }

    #[test]
    fn inflight_wait_returns_cancelled_when_prefetch_is_cancelled() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        let notify = Arc::new(Notify::new());
        let cancelled = Arc::new(AtomicBool::new(true));

        let result = runtime.block_on(wait_for_inflight_or_cancel(
            notify,
            Some(Arc::clone(&cancelled)),
        ));

        assert!(matches!(
            result,
            Err(mondrian_core::MondrianError::Cancelled)
        ));
    }

    #[test]
    fn inflight_wait_returns_when_decode_notifies() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        let notify = Arc::new(Notify::new());
        notify.notify_one();

        let result = runtime.block_on(wait_for_inflight_or_cancel(notify, None));

        assert!(result.is_ok());
    }

    #[test]
    fn decode_permit_wait_returns_cancelled_when_cancelled_before_available() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        let semaphore = Arc::new(Semaphore::new(0));
        let cancelled = Arc::new(AtomicBool::new(true));

        let result = runtime.block_on(acquire_decode_permit_or_cancel(
            semaphore,
            Some(Arc::clone(&cancelled)),
        ));

        assert!(matches!(
            result,
            Err(mondrian_core::MondrianError::Cancelled)
        ));
    }

    #[test]
    fn owned_decode_permit_holds_capacity_until_dropped() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        let semaphore = Arc::new(Semaphore::new(1));

        let permit = runtime
            .block_on(acquire_decode_permit_or_cancel(
                Arc::clone(&semaphore),
                None,
            ))
            .expect("permit");

        assert_eq!(semaphore.available_permits(), 0);
        drop(permit);
        assert_eq!(semaphore.available_permits(), 1);
    }

    #[test]
    fn finish_inflight_owner_caches_success_and_removes_waiter() {
        let cache = Mutex::new(LruCache::new(NonZeroUsize::new(4).expect("non-zero")));
        let inflight = DashMap::new();
        let notify = Arc::new(Notify::new());
        let key = RgbaFrameKey {
            asset_id: AssetId::new(),
            path: PathBuf::from("E:/media/source.mov"),
            fingerprint: PreviewFileFingerprint {
                len: Some(10),
                modified_secs: Some(20),
                modified_nanos: Some(30),
            },
            source_micros: 1_000_000,
            width: 2,
            height: 1,
            access_mode: PreviewDecodeAccessMode::PlaybackCursor,
        };
        let frame = Arc::new(RgbaFrame::new(
            2,
            1,
            vec![0, 1, 2, 3, 4, 5, 6, 7],
            crate::preview::PreviewDecodePath::InProcessFfmpegCpuRgba,
        ));
        inflight.insert(key.clone(), Arc::clone(&notify));

        finish_rgba_inflight_owner(&cache, &inflight, &key, &notify, &Ok(Arc::clone(&frame)));

        assert!(!inflight.contains_key(&key));
        assert!(Arc::ptr_eq(
            cache.lock().get(&key).expect("cached frame"),
            &frame
        ));
    }
}
