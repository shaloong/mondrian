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
            cache: Mutex::new(LruCache::new(NonZeroUsize::new(config.max_frames).unwrap())),
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
