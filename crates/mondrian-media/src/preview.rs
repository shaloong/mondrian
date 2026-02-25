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
use std::sync::OnceLock;

/// 预览分辨率硬上限（1080p 项目保持全分辨率）
const PREVIEW_MAX_WIDTH: u32 = 1920;
const PREVIEW_MAX_HEIGHT: u32 = 1080;

/// 从关键帧向前解码的最大帧数安全限制
/// 提高到 1800（足以覆盖常见 2 分钟超长 GOP 文件，例如广播流）
const DECODE_BUDGET: usize = 1800;
const PREVIEW_FRAME_CACHE_CAPACITY: usize = 256;
const PREVIEW_HIT_TOLERANCE_SECS: f64 = 0.025;
const PREVIEW_CACHE_TOLERANCE_SECS: f64 = 0.050;
const PREVIEW_MAX_SELECT_DISTANCE_SECS: f64 = 0.100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreviewDecodeBackend {
    Auto,
    Software,
    GpuAssist,
}

impl Default for PreviewDecodeBackend {
    fn default() -> Self {
        Self::Auto
    }
}

impl PreviewDecodeBackend {
    fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Software => 1,
            Self::GpuAssist => 2,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Software,
            2 => Self::GpuAssist,
            _ => Self::Auto,
        }
    }
}

static PREVIEW_DECODE_BACKEND: AtomicU8 = AtomicU8::new(PreviewDecodeBackend::Auto as u8);

pub fn set_preview_decode_backend(backend: PreviewDecodeBackend) {
    PREVIEW_DECODE_BACKEND.store(backend.as_u8(), Ordering::Relaxed);
}

pub fn preview_decode_backend() -> PreviewDecodeBackend {
    PreviewDecodeBackend::from_u8(PREVIEW_DECODE_BACKEND.load(Ordering::Relaxed))
}

#[derive(Debug, Clone)]
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

pub fn decode_first_video_frame_rgba(path: &Path) -> Result<RgbaFrame> {
    decode_video_frame_at_time_rgba_scaled(path, 0.0, None, None)
}

pub fn decode_video_frame_at_time_rgba(path: &Path, timestamp_secs: f64) -> Result<RgbaFrame> {
    decode_video_frame_at_time_rgba_scaled(path, timestamp_secs, None, None)
}

pub fn decode_video_frame_at_time_rgba_scaled(
    path: &Path,
    timestamp_secs: f64,
    max_width: Option<u32>,
    max_height: Option<u32>,
) -> Result<RgbaFrame> {
    decode_video_frame_at_time_impl(path, timestamp_secs, max_width, max_height)
}

thread_local! {
    static PREVIEW_DECODE_SESSION: RefCell<Option<PreviewDecodeSession>> = RefCell::new(None);
}

struct PreviewDecodeSession {
    path: PathBuf,
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
    cache_tolerance_pts: i64,
    target_width: u32,
    target_height: u32,
    last_pts: Option<i64>,
    reached_eof: bool,
}

impl PreviewDecodeSession {
    fn open(
        path: &Path,
        max_width: Option<u32>,
        max_height: Option<u32>,
        backend: PreviewDecodeBackend,
    ) -> Result<Self> {
        let input = ffmpeg::format::input(path).map_err(|e| MondrianError::MediaOpen {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
        Self::from_input(input, path, max_width, max_height, backend)
    }

    fn from_input(
        input: ffmpeg::format::context::Input,
        path: &Path,
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

        let context =
            ffmpeg::codec::context::Context::from_parameters(parameters).map_err(|e| {
                MondrianError::DecodeFailed {
                    asset_id: path.display().to_string(),
                    reason: e.to_string(),
                }
            })?;
        let decoder = context.decoder().video().map_err(|e| MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: e.to_string(),
        })?;

        let (target_width, target_height) = fit_target_size(
            decoder.width(),
            decoder.height(),
            max_width.map(|v| v.min(PREVIEW_MAX_WIDTH)),
            max_height.map(|v| v.min(PREVIEW_MAX_HEIGHT)),
        );

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
        let max_cache_tolerance_pts =
            seconds_to_stream_pts(PREVIEW_CACHE_TOLERANCE_SECS, stream_tb);
        let hit_tolerance_pts = (frame_duration_pts / 2).max(1).min(max_hit_tolerance_pts.max(1));
        let cache_tolerance_pts = frame_duration_pts.max(1).min(max_cache_tolerance_pts.max(1));

        Ok(Self {
            path: path.to_path_buf(),
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
            cache_tolerance_pts,
            target_width,
            target_height,
            last_pts: None,
            reached_eof: false,
        })
    }

    fn matches(
        &self,
        path: &Path,
        max_width: Option<u32>,
        max_height: Option<u32>,
        backend: PreviewDecodeBackend,
    ) -> bool {
        self.path == path
            && self.max_width == max_width
            && self.max_height == max_height
            && self.backend == backend
    }

    fn decode_at(&mut self, timestamp_secs: f64) -> Result<RgbaFrame> {
        let target_pts = timestamp_to_stream_pts(timestamp_secs, self.stream_tb);

        if let Some(hit) = preview_cache_get(
            &self.path,
            self.target_width,
            self.target_height,
            target_pts,
            self.cache_tolerance_pts,
        ) {
            return Ok(hit.frame);
        }

        let should_continue_forward = self
            .last_pts
            .map(|last| {
                target_pts >= last
                    && target_pts.saturating_sub(last) <= self.frame_duration_pts.saturating_mul(24)
                    && !self.reached_eof
            })
            .unwrap_or(false);

        if !should_continue_forward {
            self.seek_to_target(target_pts)?;
        }

        if let Some(frame) = self.decode_forward_until(target_pts)? {
            return Ok(frame);
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

    fn decode_forward_until(&mut self, target_pts: i64) -> Result<Option<RgbaFrame>> {
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

            let rgba =
                convert_decoded_to_rgba(selected_frame, &mut self.scaler, self.path.as_path())?;
            Ok(Some((selected_pts, rgba)))
        };

        for (s, packet) in self.input.packets() {
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
                frames_decoded += 1;
                let frame_pts = decoded.pts.unwrap_or(i64::MIN);
                if frame_pts != i64::MIN {
                    self.last_pts = Some(frame_pts);
                    if frame_pts <= target_pts {
                        best_before = Some((frame_pts, decoded.frame.clone()));
                        if frame_pts >= target_pts.saturating_sub(self.hit_tolerance_pts) {
                            let rgba = convert_decoded_to_rgba(
                                &decoded.frame,
                                &mut self.scaler,
                                &self.path,
                            )?;
                            preview_cache_put(
                                &self.path,
                                self.target_width,
                                self.target_height,
                                frame_pts,
                                rgba.clone(),
                            );
                            return Ok(Some(rgba));
                        }
                    } else {
                        best_after = Some((frame_pts, decoded.frame.clone()));
                        if let Some((selected_pts, rgba)) =
                            choose_and_convert(best_before.as_ref(), best_after.as_ref())?
                        {
                            preview_cache_put(
                                &self.path,
                                self.target_width,
                                self.target_height,
                                selected_pts,
                                rgba.clone(),
                            );
                            return Ok(Some(rgba));
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
            self.decoder.send_eof().map_err(|e| MondrianError::DecodeFailed {
                asset_id: self.path.display().to_string(),
                reason: e.to_string(),
            })?;

            while let Some(decoded) = receive_decoded_video_frame(&mut self.decoder)? {
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
                            preview_cache_put(
                                &self.path,
                                self.target_width,
                                self.target_height,
                                selected_pts,
                                rgba.clone(),
                            );
                            self.reached_eof = true;
                            return Ok(Some(rgba));
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
            preview_cache_put(
                &self.path,
                self.target_width,
                self.target_height,
                selected_pts,
                rgba.clone(),
            );
            return Ok(Some(rgba));
        }

        Ok(None)
    }
}

fn decode_video_frame_at_time_impl(
    path: &Path,
    timestamp_secs: f64,
    max_width: Option<u32>,
    max_height: Option<u32>,
) -> Result<RgbaFrame> {
    ensure_ffmpeg_initialized(path)?;
    PREVIEW_DECODE_SESSION.with(|slot| {
        let mut slot = slot.borrow_mut();
        let backend = preview_decode_backend();

        let current_match = slot
            .as_ref()
            .map(|session| session.matches(path, max_width, max_height, backend))
            .unwrap_or(false);

        if !current_match {
            *slot = Some(PreviewDecodeSession::open(
                path, max_width, max_height, backend,
            )?);
        }

        let session = slot.as_mut().expect("preview decode session must exist");

        if preview_hwaccel_enabled() {
            if let Some(result) = try_decode_with_ffmpeg_hwaccel(
                path,
                timestamp_secs,
                session.target_width,
                session.target_height,
            ) {
                match result {
                    Ok(frame) => return Ok(frame),
                    Err(err) => {
                        preview_trace(format!(
                            "[preview] hwaccel failed, fallback software: {err}"
                        ));
                    }
                }
            }
        }

        session.decode_at(timestamp_secs)
    })
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

#[derive(Clone)]
struct PreviewCacheHit {
    frame: RgbaFrame,
}

#[derive(Clone)]
struct PreviewFrameCacheEntry {
    path: PathBuf,
    width: u32,
    height: u32,
    pts: i64,
    frame: RgbaFrame,
}

fn preview_cache_get(
    path: &Path,
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
        if entry.path != path || entry.width != width || entry.height != height {
            continue;
        }
        let distance = (entry.pts - target_pts).abs();
        if distance < best_distance {
            best_distance = distance;
            best_index = Some(index);
        }
    }

    let Some(index) = best_index else {
        return None;
    };
    if best_distance > tolerance_pts.max(1) {
        return None;
    }

    let entry = guard.remove(index)?;
    let hit = PreviewCacheHit { frame: entry.frame.clone() };
    guard.push_front(entry);
    Some(hit)
}

fn preview_cache_put(path: &Path, width: u32, height: u32, pts: i64, frame: RgbaFrame) {
    let cache = preview_frame_cache();
    let mut guard = match cache.lock() {
        Ok(g) => g,
        Err(_) => return,
    };

    if let Some(index) = guard.iter().position(|entry| {
        entry.path == path && entry.width == width && entry.height == height && entry.pts == pts
    }) {
        guard.remove(index);
    }

    guard.push_front(PreviewFrameCacheEntry {
        path: path.to_path_buf(),
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

/// 清除进程全局的预览帧缓存（decode_video_frame_at_time 使用的全局 VecDeque）。
/// 在大幅 seek 后调用，避免旧帧被误用。
pub fn clear_global_preview_frame_cache() {
    if let Ok(mut guard) = preview_frame_cache().lock() {
        guard.clear();
    }
}

fn preview_hwaccel_enabled() -> bool {
    match preview_decode_backend() {
        PreviewDecodeBackend::GpuAssist => true,
        PreviewDecodeBackend::Software => false,
        PreviewDecodeBackend::Auto => {
            static ENABLED: OnceLock<bool> = OnceLock::new();
            *ENABLED.get_or_init(|| {
                std::env::var("MONDRIAN_PREVIEW_HWACCEL")
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

fn try_decode_with_ffmpeg_hwaccel(
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
                "ffmpeg hwaccel decode failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        }));
    }

    let expected = width as usize * height as usize * 4;
    if output.stdout.len() < expected {
        return Some(Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "ffmpeg hwaccel returned insufficient bytes: got {}, expect {}",
                output.stdout.len(),
                expected
            ),
        }));
    }

    Some(Ok(RgbaFrame {
        width,
        height,
        data: output.stdout.into_iter().take(expected).collect(),
    }))
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
    while decoder.receive_frame(&mut decoded).is_ok() {
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
    scaler.run(decoded, &mut rgba).map_err(|e| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: e.to_string(),
    })?;

    let width = rgba.width();
    let height = rgba.height();
    let stride = rgba.stride(0);
    let row_bytes = width as usize * 4;
    let src = rgba.data(0);
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

    Ok(RgbaFrame { width, height, data: out })
}
