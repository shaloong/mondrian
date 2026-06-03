//! 代理媒体 / 多级缓存管理
//!
//! L1: 内存路径解析缓存（最近使用素材）
//! L2: 磁盘代理索引缓存（跨会话保留）
//! L3: 原始素材路径回退

use crate::proxy::ProxyGenerator;
use lru::LruCache;
use mondrian_core::types::AssetId;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheTier {
    MemoryL1,
    DiskProxyL2,
    SourceL3,
}

#[derive(Debug, Clone)]
pub struct ResolvedMediaPath {
    pub path: PathBuf,
    pub tier: CacheTier,
    pub is_proxy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProxyIndexEntry {
    asset_id: AssetId,
    source_path: PathBuf,
    proxy_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ProxyIndexFile {
    entries: Vec<ProxyIndexEntry>,
}

pub struct MultiLevelCache {
    l1_path_cache: Mutex<LruCache<AssetId, ResolvedMediaPath>>,
    l2_proxy_index_path: PathBuf,
    l2_proxy_index: Mutex<HashMap<AssetId, ProxyIndexEntry>>,
}

impl MultiLevelCache {
    pub fn new(cache_root: PathBuf, l1_capacity: usize) -> Arc<Self> {
        let _ = std::fs::create_dir_all(&cache_root);
        let l2_proxy_index_path = cache_root.join("proxy_index.json");
        let l2_proxy_index = load_proxy_index(&l2_proxy_index_path);
        Arc::new(Self {
            l1_path_cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(l1_capacity.max(1)).expect("max(1) ensures non-zero"),
            )),
            l2_proxy_index_path,
            l2_proxy_index: Mutex::new(l2_proxy_index),
        })
    }

    pub fn resolve_playback_path(
        &self,
        asset_id: AssetId,
        source_path: &Path,
        prefer_proxy: bool,
        proxy_generator: &ProxyGenerator,
    ) -> ResolvedMediaPath {
        if let Some(hit) = self.l1_path_cache.lock().get(&asset_id).cloned() {
            return ResolvedMediaPath { tier: CacheTier::MemoryL1, ..hit };
        }

        if prefer_proxy {
            let proxy_from_generator = proxy_generator.proxy_path(source_path);
            if proxy_from_generator.exists() {
                let resolved = ResolvedMediaPath {
                    path: proxy_from_generator.clone(),
                    tier: CacheTier::DiskProxyL2,
                    is_proxy: true,
                };
                self.put_l1(asset_id, resolved.clone());
                self.upsert_l2(asset_id, source_path, &proxy_from_generator);
                return resolved;
            }

            if let Some(entry) = self.l2_proxy_index.lock().get(&asset_id).cloned() {
                if entry.source_path == source_path && entry.proxy_path.exists() {
                    let resolved = ResolvedMediaPath {
                        path: entry.proxy_path,
                        tier: CacheTier::DiskProxyL2,
                        is_proxy: true,
                    };
                    self.put_l1(asset_id, resolved.clone());
                    return resolved;
                }
            }
        }

        let resolved = ResolvedMediaPath {
            path: source_path.to_path_buf(),
            tier: CacheTier::SourceL3,
            is_proxy: false,
        };
        self.put_l1(asset_id, resolved.clone());
        resolved
    }

    pub fn invalidate_asset(&self, asset_id: AssetId) {
        self.l1_path_cache.lock().pop(&asset_id);
        self.l2_proxy_index.lock().remove(&asset_id);
        self.persist_l2();
    }

    pub fn clear_l1(&self) {
        self.l1_path_cache.lock().clear();
    }

    fn put_l1(&self, asset_id: AssetId, resolved: ResolvedMediaPath) {
        self.l1_path_cache.lock().put(asset_id, resolved);
    }

    fn upsert_l2(&self, asset_id: AssetId, source_path: &Path, proxy_path: &Path) {
        self.l2_proxy_index.lock().insert(
            asset_id,
            ProxyIndexEntry {
                asset_id,
                source_path: source_path.to_path_buf(),
                proxy_path: proxy_path.to_path_buf(),
            },
        );
        self.persist_l2();
    }

    fn persist_l2(&self) {
        let map = self.l2_proxy_index.lock();
        let mut entries = map.values().cloned().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.asset_id.to_string());
        let file = ProxyIndexFile { entries };

        if let Ok(serialized) = serde_json::to_vec_pretty(&file) {
            if let Some(parent) = self.l2_proxy_index_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&self.l2_proxy_index_path, serialized);
        }
    }
}

fn load_proxy_index(path: &Path) -> HashMap<AssetId, ProxyIndexEntry> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return HashMap::new(),
    };

    let parsed = match serde_json::from_slice::<ProxyIndexFile>(&bytes) {
        Ok(parsed) => parsed,
        Err(_) => return HashMap::new(),
    };

    parsed.entries.into_iter().map(|entry| (entry.asset_id, entry)).collect()
}
