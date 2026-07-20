//! Process-wide decoded Preview frame cache.
//!
//! This module owns exact cache identity, nearest-PTS lookup, LRU promotion,
//! capacity eviction, and explicit invalidation. It sits downstream of
//! YUV-to-RGB conversion, so source-color interpretation is part of every key.

use super::*;

#[derive(Clone)]
pub(super) struct PreviewCacheHit {
    pub(super) frame: PreviewDecodedFramePayload,
    pub(super) pts: i64,
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

pub(super) fn preview_cache_get(
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

pub(super) fn preview_cache_put_with_fingerprint(
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
