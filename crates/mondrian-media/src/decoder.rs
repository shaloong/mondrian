//! 解码器池
//!
//! 管理多个并发 FFmpeg 解码上下文，支持帧精确随机访问。

use crate::cache::{FrameCache, RawVideoFrame};
use crate::preview::{
    decode_video_frame_at_time_rgba, decode_video_frame_at_time_rgba_scaled, RgbaFrame,
};
use dashmap::DashMap;
use lru::LruCache;
use mondrian_core::{types::*, Result};
use parking_lot::Mutex;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::runtime::Runtime;
use tokio::sync::{Notify, Semaphore};

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

impl HwAccelBackend {
    /// 自动检测当前平台最优硬解后端
    pub fn detect() -> Self {
        #[cfg(target_os = "windows")]
        {
            // TODO: 实际检测 CUDA / D3D11VA 可用性
            return Self::D3D11VA;
        }
        #[cfg(target_os = "macos")]
        {
            return Self::VideoToolbox;
        }
        #[cfg(target_os = "linux")]
        {
            return Self::Vaapi;
        }
        #[allow(unreachable_code)]
        Self::None
    }
}

/// 单个媒体文件的解码上下文（FFmpeg AVFormatContext 包装）
#[allow(dead_code)]
struct DecoderContext {
    asset_id: AssetId,
    media_path: PathBuf,
    hw_accel: HwAccelBackend,
    // TODO: ffmpeg_next::format::context::Input
}

#[derive(Clone)]
struct PrefetchTask {
    asset_id: AssetId,
    cancelled: Arc<AtomicBool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RgbaFrameKey {
    asset_id: AssetId,
    frame_num: u64,
    width: u32,
    height: u32,
}

#[derive(Default)]
struct DecoderMetrics {
    yuv_requests: AtomicU64,
    yuv_cache_hits: AtomicU64,
    rgba_requests: AtomicU64,
    rgba_cache_hits: AtomicU64,
    decode_executions: AtomicU64,
    decode_failures: AtomicU64,
    prefetch_started: AtomicU64,
    prefetch_cancelled: AtomicU64,
    prefetch_completed: AtomicU64,
    total_decode_ns: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct DecoderMetricsSnapshot {
    pub yuv_requests: u64,
    pub yuv_cache_hits: u64,
    pub rgba_requests: u64,
    pub rgba_cache_hits: u64,
    pub decode_executions: u64,
    pub decode_failures: u64,
    pub prefetch_started: u64,
    pub prefetch_cancelled: u64,
    pub prefetch_completed: u64,
    pub avg_decode_ms: f64,
    pub avg_decode_exec_ms: f64,
    pub decode_miss_rate_pct: f64,
}

impl DecoderMetrics {
    fn snapshot(&self) -> DecoderMetricsSnapshot {
        let yuv_requests = self.yuv_requests.load(Ordering::Relaxed);
        let rgba_requests = self.rgba_requests.load(Ordering::Relaxed);
        let decode_requests = yuv_requests + rgba_requests;
        let decode_executions = self.decode_executions.load(Ordering::Relaxed);
        let total_decode_ns = self.total_decode_ns.load(Ordering::Relaxed);
        DecoderMetricsSnapshot {
            yuv_requests,
            yuv_cache_hits: self.yuv_cache_hits.load(Ordering::Relaxed),
            rgba_requests,
            rgba_cache_hits: self.rgba_cache_hits.load(Ordering::Relaxed),
            decode_executions,
            decode_failures: self.decode_failures.load(Ordering::Relaxed),
            prefetch_started: self.prefetch_started.load(Ordering::Relaxed),
            prefetch_cancelled: self.prefetch_cancelled.load(Ordering::Relaxed),
            prefetch_completed: self.prefetch_completed.load(Ordering::Relaxed),
            avg_decode_ms: if decode_requests > 0 {
                (total_decode_ns as f64 / decode_requests as f64) / 1_000_000.0
            } else {
                0.0
            },
            avg_decode_exec_ms: if decode_executions > 0 {
                (total_decode_ns as f64 / decode_executions as f64) / 1_000_000.0
            } else {
                0.0
            },
            decode_miss_rate_pct: if decode_requests > 0 {
                decode_executions as f64 / decode_requests as f64 * 100.0
            } else {
                0.0
            },
        }
    }
}

impl DecoderContext {
    fn open(asset_id: AssetId, path: PathBuf, hw_accel: HwAccelBackend) -> Result<Self> {
        tracing::debug!("Opened decoder for asset {asset_id}");
        Ok(Self { asset_id, media_path: path, hw_accel })
    }
}

/// 解码器池
///
/// - 每个素材最多持有一个 `DecoderContext`
/// - 通过 `Semaphore` 限制并发解码数 ≤ `max_concurrent`
/// - 解码结果通过 `FrameCache` 缓存
pub struct DecoderPool {
    contexts: DashMap<AssetId, Arc<tokio::sync::Mutex<DecoderContext>>>,
    frame_cache: Arc<FrameCache>,
    semaphore: Arc<Semaphore>,
    hw_accel: HwAccelBackend,
    prefetch_tasks: DashMap<u64, PrefetchTask>,
    next_prefetch_task_id: AtomicU64,
    rgba_cache: Mutex<LruCache<RgbaFrameKey, Arc<RgbaFrame>>>,
    rgba_inflight: DashMap<RgbaFrameKey, Arc<Notify>>,
    preview_decode_runtime: Arc<Runtime>,
    background_runtime: Arc<Runtime>,
    metrics: DecoderMetrics,
}

impl DecoderPool {
    pub fn new(frame_cache: Arc<FrameCache>) -> Arc<Self> {
        let max_concurrent = (num_cpus() - 2).max(1);
        Arc::new(Self {
            contexts: DashMap::new(),
            frame_cache,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            hw_accel: HwAccelBackend::detect(),
            prefetch_tasks: DashMap::new(),
            next_prefetch_task_id: AtomicU64::new(1),
            rgba_cache: Mutex::new(LruCache::new(NonZeroUsize::new(256).unwrap())),
            rgba_inflight: DashMap::new(),
            preview_decode_runtime: Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(preview_decode_worker_threads())
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

    pub async fn get_video_frame_rgba(
        &self,
        asset_id: AssetId,
        path: PathBuf,
        timecode: TimeCode,
        target_width: u32,
        target_height: u32,
    ) -> Result<Arc<RgbaFrame>> {
        self.metrics.rgba_requests.fetch_add(1, Ordering::Relaxed);
        let frame_num = timecode.frame.max(0) as u64;
        let key = RgbaFrameKey {
            asset_id,
            frame_num,
            width: target_width,
            height: target_height,
        };

        if let Some(hit) = self.rgba_cache.lock().get(&key).cloned() {
            self.metrics.rgba_cache_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(hit);
        }

        if let Some(waiter) = self.rgba_inflight.get(&key).map(|entry| Arc::clone(entry.value())) {
            waiter.notified().await;
            if let Some(hit) = self.rgba_cache.lock().get(&key).cloned() {
                self.metrics.rgba_cache_hits.fetch_add(1, Ordering::Relaxed);
                return Ok(hit);
            }
        }

        let notify = Arc::new(Notify::new());
        if let Some(existing) = self.rgba_inflight.insert(key.clone(), Arc::clone(&notify)) {
            existing.notified().await;
            if let Some(hit) = self.rgba_cache.lock().get(&key).cloned() {
                self.metrics.rgba_cache_hits.fetch_add(1, Ordering::Relaxed);
                return Ok(hit);
            }
        }

        self.metrics.decode_executions.fetch_add(1, Ordering::Relaxed);
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| mondrian_core::MondrianError::Cancelled)?;

        tracing::debug!(
            "[decoder] rgba request start asset={} frame={} target={}x{}",
            asset_id,
            frame_num,
            target_width,
            target_height
        );

        let _ctx = self.get_or_open_context(asset_id, path.clone()).await?;
        tracing::debug!(
            "[decoder] rgba context ready asset={} frame={}",
            asset_id,
            frame_num
        );

        let secs = timecode.to_secs().max(0.0);

        let started = Instant::now();
        let mut decode_task = self.preview_decode_runtime.spawn(async move {
            decode_video_frame_at_time_rgba_scaled(
                path.as_path(),
                secs,
                Some(target_width.max(1)),
                Some(target_height.max(1)),
            )
            .map(Arc::new)
        });

        let decode_timeout_ms = decode_timeout_budget_ms();
        let decode_result: Result<Arc<RgbaFrame>> = if decode_timeout_ms == 0 {
            decode_task.await.map_err(|e| mondrian_core::MondrianError::DecodeFailed {
                asset_id: asset_id.to_string(),
                reason: e.to_string(),
            })?
        } else {
            let timeout = tokio::time::sleep(tokio::time::Duration::from_millis(decode_timeout_ms));
            tokio::pin!(timeout);

            tokio::select! {
                joined = &mut decode_task => {
                    joined.map_err(|e| mondrian_core::MondrianError::DecodeFailed {
                        asset_id: asset_id.to_string(),
                        reason: e.to_string(),
                    })?
                }
                _ = &mut timeout => {
                    decode_task.abort();
                    tracing::warn!(
                        "MONDRIAN_DECODE_TIMEOUT_JSON={{\"asset_id\":\"{}\",\"frame\":{},\"secs\":{:.3},\"budget_ms\":{},\"target_width\":{},\"target_height\":{},\"reason\":\"decode timeout\"}}",
                        asset_id,
                        frame_num,
                        secs,
                        decode_timeout_ms,
                        target_width,
                        target_height
                    );
                    Err(mondrian_core::MondrianError::DecodeFailed {
                        asset_id: asset_id.to_string(),
                        reason: format!(
                            "preview decode timeout after {}ms (frame={} secs={:.3})",
                            decode_timeout_ms,
                            frame_num,
                            secs
                        ),
                    })
                }
            }
        };

        let decode_result = decode_result.map_err(|e| {
            self.metrics.decode_failures.fetch_add(1, Ordering::Relaxed);
            e
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

        match decode_result {
            Ok(frame) => {
                self.rgba_cache.lock().put(key.clone(), frame.clone());
                self.rgba_inflight.remove(&key);
                notify.notify_waiters();
                Ok(frame)
            }
            Err(err) => {
                self.rgba_inflight.remove(&key);
                notify.notify_waiters();
                Err(err)
            }
        }
    }

    /// 获取或创建指定素材的解码上下文
    async fn get_or_open_context(
        &self,
        asset_id: AssetId,
        path: PathBuf,
    ) -> Result<Arc<tokio::sync::Mutex<DecoderContext>>> {
        if let Some(ctx) = self.contexts.get(&asset_id) {
            return Ok(ctx.clone());
        }
        let ctx = tokio::task::spawn_blocking({
            let hw = self.hw_accel;
            let path = path.clone();
            move || DecoderContext::open(asset_id, path, hw)
        })
        .await
        .map_err(|e| mondrian_core::MondrianError::DecodeFailed {
            asset_id: asset_id.to_string(),
            reason: e.to_string(),
        })??;

        let ctx = Arc::new(tokio::sync::Mutex::new(ctx));
        self.contexts.insert(asset_id, ctx.clone());
        Ok(ctx)
    }

    /// 获取指定时间码处的视频帧（优先从缓存读取）
    pub async fn get_video_frame(
        &self,
        asset_id: AssetId,
        path: PathBuf,
        timecode: TimeCode,
    ) -> Result<Arc<RawVideoFrame>> {
        self.metrics.yuv_requests.fetch_add(1, Ordering::Relaxed);
        let frame_num = timecode.frame.max(0) as u64;

        // 1. 检查缓存
        if let Some(frame) = self.frame_cache.get(asset_id, frame_num) {
            self.metrics.yuv_cache_hits.fetch_add(1, Ordering::Relaxed);
            tracing::trace!("Cache hit: asset={asset_id} frame={frame_num}");
            return Ok(frame);
        }

        self.metrics.decode_executions.fetch_add(1, Ordering::Relaxed);
        // 2. 限流（最多 N 个并发解码）
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| mondrian_core::MondrianError::Cancelled)?;

        // 3. 获取 context
        let ctx = self.get_or_open_context(asset_id, path.clone()).await?;

        // 4. 解码（在阻塞线程池执行）
        let started = Instant::now();
        let frame = tokio::task::spawn_blocking(move || {
            let _ctx_lock = ctx.blocking_lock();
            tracing::debug!("Decoding frame {frame_num} for asset {asset_id}");

            let timestamp_secs = timecode.to_secs().max(0.0);
            let rgba = decode_video_frame_at_time_rgba(path.as_path(), timestamp_secs)?;
            let (planes, strides) = rgba_to_yuv420p(&rgba)?;

            Ok::<Arc<RawVideoFrame>, mondrian_core::MondrianError>(Arc::new(RawVideoFrame {
                asset_id,
                pts: timecode,
                width: rgba.width,
                height: rgba.height,
                planes,
                strides,
                frame_num,
            }))
        })
        .await
        .map_err(|e| mondrian_core::MondrianError::DecodeFailed {
            asset_id: asset_id.to_string(),
            reason: e.to_string(),
        })
        .and_then(|r| r)
        .map_err(|e| {
            self.metrics.decode_failures.fetch_add(1, Ordering::Relaxed);
            e
        })?;

        self.metrics
            .total_decode_ns
            .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);

        // 5. 写入缓存
        self.frame_cache.insert(frame.clone());

        Ok(frame)
    }

    /// 关闭并移除指定素材的解码上下文（素材被删除时）
    pub fn close_context(&self, asset_id: AssetId) {
        self.contexts.remove(&asset_id);
        self.cancel_prefetch_for_asset(asset_id);
        self.frame_cache.evict_asset(asset_id);
        self.evict_rgba_asset(asset_id);
    }

    /// 启动可取消的预取任务，返回任务 ID。
    ///
    /// 预取会顺序请求 `lookahead_frames` 帧，并尽量填充 `FrameCache`。
    pub fn spawn_prefetch(
        self: &Arc<Self>,
        asset_id: AssetId,
        path: PathBuf,
        start_timecode: TimeCode,
        lookahead_frames: u32,
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
            let fps = start_timecode.time_base;
            let mut was_cancelled = false;
            for offset in 0..lookahead_frames {
                if cancelled.load(Ordering::Relaxed) {
                    was_cancelled = true;
                    break;
                }

                let tc = TimeCode::new(start_timecode.frame + offset as i64, fps);
                if let Err(err) = pool.get_video_frame(asset_id, path.clone(), tc).await {
                    if matches!(err, mondrian_core::MondrianError::Cancelled) {
                        was_cancelled = true;
                        break;
                    }
                    tracing::debug!(
                        "prefetch frame failed: task_id={} asset={} frame={} err={}",
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
                    .get_video_frame_rgba(
                        asset_id,
                        path.clone(),
                        tc,
                        target_width.max(1),
                        target_height.max(1),
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
        self.metrics.snapshot()
    }

    pub fn clear_all_caches(&self) {
        self.cancel_all_prefetch_tasks();
        self.prefetch_tasks.clear();
        self.contexts.clear();
        self.frame_cache.clear_all();
        self.rgba_cache.lock().clear();
    }

    /// 清除全部 RGBA 图层帧缓存（大幅 seek 后调用，淘汰远离新位置的旧缓存帧）。
    pub fn evict_rgba_cache(&self) {
        self.rgba_cache.lock().clear();
        // 同步清除进程全局预览帧缓存（decode_video_frame_at_time 使用的缓存）
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
    let max_threads = (num_cpus() - 1).clamp(1, 8);
    std::env::var("MONDRIAN_PREVIEW_DECODE_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.clamp(1, max_threads))
        .unwrap_or(2.min(max_threads))
}

fn num_cpus() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)
}

fn decode_timeout_budget_ms() -> u64 {
    static TIMEOUT_MS: OnceLock<u64> = OnceLock::new();
    *TIMEOUT_MS.get_or_init(|| {
        std::env::var("MONDRIAN_DECODE_TIMEOUT_BUDGET_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .or_else(|| {
                std::env::var("MONDRIAN_PREVIEW_DECODE_TIMEOUT_MS")
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
            })
            .filter(|value| *value >= 100)
            .unwrap_or(2500)
    })
}

fn rgba_to_yuv420p(frame: &RgbaFrame) -> Result<([Vec<u8>; 3], [u32; 3])> {
    let width = frame.width as usize;
    let height = frame.height as usize;
    let expected_len = width.saturating_mul(height).saturating_mul(4);
    if frame.data.len() < expected_len {
        return Err(mondrian_core::MondrianError::DecodeFailed {
            asset_id: "rgba_to_yuv420p".to_string(),
            reason: format!(
                "rgba buffer too small: actual={} expected={}",
                frame.data.len(),
                expected_len
            ),
        });
    }

    let mut y_plane = vec![0u8; width * height];
    let uv_width = width.div_ceil(2);
    let uv_height = height.div_ceil(2);
    let mut u_plane = vec![0u8; uv_width * uv_height];
    let mut v_plane = vec![0u8; uv_width * uv_height];

    for y in 0..height {
        for x in 0..width {
            let i = (y * width + x) * 4;
            let r = frame.data[i] as f32;
            let g = frame.data[i + 1] as f32;
            let b = frame.data[i + 2] as f32;

            let luma = (0.257 * r + 0.504 * g + 0.098 * b + 16.0).round().clamp(0.0, 255.0);
            y_plane[y * width + x] = luma as u8;
        }
    }

    for uv_y in 0..uv_height {
        for uv_x in 0..uv_width {
            let base_x = uv_x * 2;
            let base_y = uv_y * 2;

            let mut u_acc = 0.0f32;
            let mut v_acc = 0.0f32;
            let mut count = 0.0f32;

            for oy in 0..2 {
                for ox in 0..2 {
                    let px = base_x + ox;
                    let py = base_y + oy;
                    if px >= width || py >= height {
                        continue;
                    }

                    let i = (py * width + px) * 4;
                    let r = frame.data[i] as f32;
                    let g = frame.data[i + 1] as f32;
                    let b = frame.data[i + 2] as f32;

                    let u = (-0.148 * r - 0.291 * g + 0.439 * b + 128.0).round().clamp(0.0, 255.0);
                    let v = (0.439 * r - 0.368 * g - 0.071 * b + 128.0).round().clamp(0.0, 255.0);

                    u_acc += u;
                    v_acc += v;
                    count += 1.0;
                }
            }

            let idx = uv_y * uv_width + uv_x;
            u_plane[idx] = (u_acc / count.max(1.0)).round().clamp(0.0, 255.0) as u8;
            v_plane[idx] = (v_acc / count.max(1.0)).round().clamp(0.0, 255.0) as u8;
        }
    }

    Ok((
        [y_plane, u_plane, v_plane],
        [frame.width, uv_width as u32, uv_width as u32],
    ))
}
