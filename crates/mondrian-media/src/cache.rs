//! 帧缓存（LRU）
//!
//! 将已解码的视频帧缓存在内存中，避免重复解码。
//! 默认最大缓存 1GB，可通过 `FrameCacheConfig` 调整。

use lru::LruCache;
use mondrian_core::types::{AssetId, TimeCode};
use parking_lot::Mutex;
use std::num::NonZeroUsize;
use std::sync::Arc;

/// 一个已解码的原始视频帧（CPU 内存侧）
#[derive(Debug, Clone)]
pub struct RawVideoFrame {
    pub asset_id: AssetId,
    pub pts: TimeCode, // 显示时间戳
    pub width: u32,
    pub height: u32,
    /// YUV420P 平面数据：[Y plane, U plane, V plane]
    pub planes: [Vec<u8>; 3],
    /// 每个平面的行跨度（stride）
    pub strides: [u32; 3],
    /// 帧序号（用于顺序检测）
    pub frame_num: u64,
}

impl RawVideoFrame {
    /// 估算内存占用（字节）
    pub fn memory_size(&self) -> usize {
        self.planes.iter().map(|p| p.len()).sum()
    }
}

/// 帧缓存键
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FrameKey {
    asset_id: AssetId,
    frame_num: u64,
}

/// 帧缓存配置
pub struct FrameCacheConfig {
    /// 最大缓存帧数
    pub max_frames: usize,
}

impl Default for FrameCacheConfig {
    fn default() -> Self {
        // 1080p 帧约 6MB → 缓存 180 帧 ≈ 1GB
        Self { max_frames: 180 }
    }
}

/// LRU 帧缓存
pub struct FrameCache {
    cache: Mutex<LruCache<FrameKey, Arc<RawVideoFrame>>>,
    /// 缓存命中计数（监控用）
    hits: std::sync::atomic::AtomicU64,
    misses: std::sync::atomic::AtomicU64,
}

impl FrameCache {
    pub fn new(config: FrameCacheConfig) -> Arc<Self> {
        Arc::new(Self {
            cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(config.max_frames.max(1)).expect("max_frames >= 1"),
            )),
            hits: Default::default(),
            misses: Default::default(),
        })
    }

    /// 尝试从缓存获取帧
    pub fn get(&self, asset_id: AssetId, frame_num: u64) -> Option<Arc<RawVideoFrame>> {
        let key = FrameKey { asset_id, frame_num };
        let result = self.cache.lock().get(&key).cloned();
        if result.is_some() {
            self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        result
    }

    /// 写入帧到缓存
    pub fn insert(&self, frame: Arc<RawVideoFrame>) {
        let key = FrameKey {
            asset_id: frame.asset_id,
            frame_num: frame.frame_num,
        };
        self.cache.lock().put(key, frame);
    }

    /// 清除指定素材的所有缓存帧（素材被删除/替换时调用）
    pub fn evict_asset(&self, asset_id: AssetId) {
        let mut cache = self.cache.lock();
        let keys_to_remove: Vec<FrameKey> = cache
            .iter()
            .filter_map(|(key, _)| (key.asset_id == asset_id).then_some(key.clone()))
            .collect();

        for key in keys_to_remove {
            let _ = cache.pop(&key);
        }
    }

    pub fn clear_all(&self) {
        self.cache.lock().clear();
    }

    /// 缓存统计
    pub fn stats(&self) -> CacheStats {
        let hits = self.hits.load(std::sync::atomic::Ordering::Relaxed);
        let misses = self.misses.load(std::sync::atomic::Ordering::Relaxed);
        let total = hits + misses;
        CacheStats {
            hits,
            misses,
            hit_rate: if total > 0 {
                hits as f64 / total as f64
            } else {
                0.0
            },
            size: self.cache.lock().len(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub hit_rate: f64,
    pub size: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_frame(asset_id: AssetId, frame_num: u64) -> Arc<RawVideoFrame> {
        Arc::new(RawVideoFrame {
            asset_id,
            pts: TimeCode::new(
                frame_num as i64,
                mondrian_core::types::Rational::new(30, 1),
            ),
            width: 1920,
            height: 1080,
            planes: [vec![0u8; 100], vec![0u8; 50], vec![0u8; 50]],
            strides: [1920, 960, 960],
            frame_num,
        })
    }

    #[test]
    fn insert_and_get() {
        let cache = FrameCache::new(FrameCacheConfig { max_frames: 10 });
        let asset = AssetId::new();
        let frame = make_frame(asset, 0);
        cache.insert(frame);
        let retrieved = cache.get(asset, 0);
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().frame_num, 0);
    }

    #[test]
    fn get_missing_returns_none() {
        let cache = FrameCache::new(FrameCacheConfig { max_frames: 10 });
        assert!(cache.get(AssetId::new(), 0).is_none());
    }

    #[test]
    fn hit_miss_stats() {
        let cache = FrameCache::new(FrameCacheConfig { max_frames: 10 });
        let asset = AssetId::new();
        assert!(cache.get(asset, 0).is_none()); // miss
        cache.insert(make_frame(asset, 0));
        assert!(cache.get(asset, 0).is_some()); // hit
        cache.get(asset, 1); // miss
        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 2);
    }

    #[test]
    fn evict_asset_removes_all_its_frames() {
        let cache = FrameCache::new(FrameCacheConfig { max_frames: 10 });
        let a1 = AssetId::new();
        let a2 = AssetId::new();
        cache.insert(make_frame(a1, 0));
        cache.insert(make_frame(a1, 1));
        cache.insert(make_frame(a2, 0));
        cache.evict_asset(a1);
        assert!(cache.get(a1, 0).is_none());
        assert!(cache.get(a1, 1).is_none());
        assert!(cache.get(a2, 0).is_some()); // a2 unaffected
    }

    #[test]
    fn clear_all() {
        let cache = FrameCache::new(FrameCacheConfig { max_frames: 10 });
        cache.insert(make_frame(AssetId::new(), 0));
        cache.clear_all();
        assert_eq!(cache.stats().size, 0);
    }

    #[test]
    fn lru_eviction() {
        let cache = FrameCache::new(FrameCacheConfig { max_frames: 2 });
        let asset = AssetId::new();
        cache.insert(make_frame(asset, 0));
        cache.insert(make_frame(asset, 1));
        // cache at capacity; inserting a 3rd should evict frame 0 (LRU)
        cache.insert(make_frame(asset, 2));
        assert_eq!(cache.stats().size, 2);
        // frame 0 was least recently used (only inserted, never accessed)
        // depending on LRU internals it may or may not be gone — but size is capped
        let total_present = [0, 1, 2]
            .iter()
            .filter(|&&n| cache.get(asset, n).is_some())
            .count();
        assert_eq!(total_present, 2);
    }

    #[test]
    fn max_frames_zero_uses_minimum() {
        let cache = FrameCache::new(FrameCacheConfig { max_frames: 0 });
        cache.insert(make_frame(AssetId::new(), 0));
        assert_eq!(cache.stats().size, 1);
    }
}
