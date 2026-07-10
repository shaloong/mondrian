//! 代理媒体 / 多级缓存管理
//!
//! L1: 内存路径解析缓存（最近使用素材）
//! L2: 磁盘代理索引缓存（跨会话保留）
//! L3: 原始素材路径回退

use crate::proxy::{ProxyColorContract, ProxyGenerator, ProxyStatus};
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
        proxy_color: ProxyColorContract,
    ) -> ResolvedMediaPath {
        if let Some(hit) = self.fresh_l1_hit(
            asset_id,
            source_path,
            prefer_proxy,
            proxy_generator,
            proxy_color,
        ) {
            return hit;
        }

        if prefer_proxy {
            let proxy_from_generator = proxy_generator.proxy_path(source_path, proxy_color).ok();
            if let Some(proxy_from_generator) = proxy_from_generator.as_ref().filter(|_| {
                proxy_generator.proxy_status(source_path, proxy_color) == ProxyStatus::Fresh
            }) {
                let resolved = ResolvedMediaPath {
                    path: proxy_from_generator.clone(),
                    tier: CacheTier::DiskProxyL2,
                    is_proxy: true,
                };
                self.put_l1(asset_id, resolved.clone());
                self.upsert_l2(asset_id, source_path, proxy_from_generator);
                return resolved;
            }

            if let (Some(entry), Some(proxy_from_generator)) = (
                self.l2_proxy_index.lock().get(&asset_id).cloned(),
                proxy_from_generator.as_ref(),
            ) {
                if self.l2_entry_is_fresh(
                    &entry,
                    source_path,
                    proxy_from_generator,
                    proxy_generator,
                    proxy_color,
                ) {
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

    fn fresh_l1_hit(
        &self,
        asset_id: AssetId,
        source_path: &Path,
        prefer_proxy: bool,
        proxy_generator: &ProxyGenerator,
        proxy_color: ProxyColorContract,
    ) -> Option<ResolvedMediaPath> {
        let hit = self.l1_path_cache.lock().get(&asset_id).cloned()?;
        if !prefer_proxy {
            if !hit.is_proxy && hit.path == source_path {
                return Some(ResolvedMediaPath { tier: CacheTier::MemoryL1, ..hit });
            }
            self.l1_path_cache.lock().pop(&asset_id);
            return None;
        }

        let expected_proxy_path = proxy_generator.proxy_path(source_path, proxy_color).ok();
        let expected_proxy_status = proxy_generator.proxy_status(source_path, proxy_color);
        if hit.is_proxy
            && expected_proxy_path.as_ref().is_some_and(|path| hit.path == *path)
            && expected_proxy_status == ProxyStatus::Fresh
        {
            return Some(ResolvedMediaPath { tier: CacheTier::MemoryL1, ..hit });
        }
        if !hit.is_proxy && hit.path == source_path && expected_proxy_status != ProxyStatus::Fresh {
            return Some(ResolvedMediaPath { tier: CacheTier::MemoryL1, ..hit });
        }
        self.l1_path_cache.lock().pop(&asset_id);
        None
    }

    fn l2_entry_is_fresh(
        &self,
        entry: &ProxyIndexEntry,
        source_path: &Path,
        expected_proxy_path: &Path,
        proxy_generator: &ProxyGenerator,
        proxy_color: ProxyColorContract,
    ) -> bool {
        if entry.source_path != source_path
            || entry.proxy_path != expected_proxy_path
            || !entry.proxy_path.exists()
        {
            return false;
        }

        proxy_generator.proxy_status(source_path, proxy_color) == ProxyStatus::Fresh
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

#[cfg(test)]
mod tests {
    use super::{CacheTier, MultiLevelCache};
    use crate::{DecodedVideoRange, ProxyColorContract, ProxyConfig, ProxyGenerator};
    use mondrian_core::types::{AssetId, ColorSpace};
    use std::path::PathBuf;
    use std::time::Duration;

    fn test_proxy_config(cache_dir: PathBuf) -> ProxyConfig {
        ProxyConfig { cache_dir, ..ProxyConfig::default() }
    }

    fn proxy_color() -> ProxyColorContract {
        ProxyColorContract::new(ColorSpace::Rec709, 8, DecodedVideoRange::Limited)
    }

    fn write_fresh_proxy(
        root: &tempfile::TempDir,
        source_name: &str,
    ) -> (PathBuf, ProxyGenerator, PathBuf) {
        let source = root.path().join(source_name);
        std::fs::write(&source, b"source").expect("source");
        std::thread::sleep(Duration::from_millis(20));
        let generator = ProxyGenerator::new(test_proxy_config(root.path().join("proxy")));
        let color = proxy_color();
        let proxy = generator.proxy_path(&source, color).expect("proxy path");
        std::fs::create_dir_all(proxy.parent().expect("proxy parent")).expect("proxy parent");
        std::fs::write(&proxy, b"proxy").expect("proxy");
        generator.install_test_manifest(&source, color);
        (source, generator, proxy)
    }

    #[test]
    fn resolve_playback_path_uses_fresh_proxy() {
        let root = tempfile::tempdir().expect("tempdir");
        let cache = MultiLevelCache::new(root.path().join("cache"), 8);
        let asset_id = AssetId::new();
        let (source, generator, proxy) = write_fresh_proxy(&root, "source.mp4");

        let resolved =
            cache.resolve_playback_path(asset_id, &source, true, &generator, proxy_color());

        assert_eq!(resolved.path, proxy);
        assert_eq!(resolved.tier, CacheTier::DiskProxyL2);
        assert!(resolved.is_proxy);
    }

    #[test]
    fn resolve_playback_path_falls_back_to_source_when_proxy_is_stale() {
        let root = tempfile::tempdir().expect("tempdir");
        let cache = MultiLevelCache::new(root.path().join("cache"), 8);
        let asset_id = AssetId::new();
        let source = root.path().join("source.mp4");
        let generator = ProxyGenerator::new(test_proxy_config(root.path().join("proxy")));
        let proxy = generator.proxy_path(&source, proxy_color()).expect("proxy path");
        std::fs::create_dir_all(proxy.parent().expect("proxy parent")).expect("proxy parent");
        std::fs::write(&proxy, b"proxy").expect("proxy");
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&source, b"newer source").expect("source");

        let resolved =
            cache.resolve_playback_path(asset_id, &source, true, &generator, proxy_color());

        assert_eq!(resolved.path, source);
        assert_eq!(resolved.tier, CacheTier::SourceL3);
        assert!(!resolved.is_proxy);
    }

    #[test]
    fn resolve_playback_path_upgrades_source_l1_when_proxy_becomes_fresh() {
        let root = tempfile::tempdir().expect("tempdir");
        let cache = MultiLevelCache::new(root.path().join("cache"), 8);
        let asset_id = AssetId::new();
        let source = root.path().join("source.mp4");
        std::fs::write(&source, b"source").expect("source");
        let generator = ProxyGenerator::new(test_proxy_config(root.path().join("proxy")));

        let first = cache.resolve_playback_path(asset_id, &source, true, &generator, proxy_color());
        assert_eq!(first.path, source);
        assert_eq!(first.tier, CacheTier::SourceL3);

        std::thread::sleep(Duration::from_millis(20));
        let color = proxy_color();
        let proxy = generator.proxy_path(&source, color).expect("proxy path");
        std::fs::create_dir_all(proxy.parent().expect("proxy parent")).expect("proxy parent");
        std::fs::write(&proxy, b"proxy").expect("proxy");
        generator.install_test_manifest(&source, color);

        let second =
            cache.resolve_playback_path(asset_id, &source, true, &generator, proxy_color());

        assert_eq!(second.path, proxy);
        assert_eq!(second.tier, CacheTier::DiskProxyL2);
        assert!(second.is_proxy);
    }

    #[test]
    fn resolve_playback_path_does_not_return_stale_proxy_from_l1() {
        let root = tempfile::tempdir().expect("tempdir");
        let cache = MultiLevelCache::new(root.path().join("cache"), 8);
        let asset_id = AssetId::new();
        let (source, generator, proxy) = write_fresh_proxy(&root, "source.mp4");

        let first = cache.resolve_playback_path(asset_id, &source, true, &generator, proxy_color());
        assert_eq!(first.path, proxy);
        assert!(first.is_proxy);

        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&source, b"newer source").expect("source");

        let second =
            cache.resolve_playback_path(asset_id, &source, true, &generator, proxy_color());

        assert_eq!(second.path, source);
        assert_eq!(second.tier, CacheTier::SourceL3);
        assert!(!second.is_proxy);
    }

    #[test]
    fn resolve_playback_path_does_not_return_stale_proxy_from_l2_index() {
        let root = tempfile::tempdir().expect("tempdir");
        let cache = MultiLevelCache::new(root.path().join("cache"), 8);
        let asset_id = AssetId::new();
        let (source, generator, proxy) = write_fresh_proxy(&root, "source.mp4");

        let first = cache.resolve_playback_path(asset_id, &source, true, &generator, proxy_color());
        assert_eq!(first.path, proxy);
        cache.clear_l1();

        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&source, b"newer source").expect("source");

        let second =
            cache.resolve_playback_path(asset_id, &source, true, &generator, proxy_color());

        assert_eq!(second.path, source);
        assert_eq!(second.tier, CacheTier::SourceL3);
        assert!(!second.is_proxy);
    }

    #[test]
    fn resolve_playback_path_does_not_return_l2_proxy_from_old_config() {
        let root = tempfile::tempdir().expect("tempdir");
        let cache = MultiLevelCache::new(root.path().join("cache"), 8);
        let asset_id = AssetId::new();
        let (source, first_generator, first_proxy) = write_fresh_proxy(&root, "source.mp4");

        let first =
            cache.resolve_playback_path(asset_id, &source, true, &first_generator, proxy_color());
        assert_eq!(first.path, first_proxy);
        cache.clear_l1();

        let second_generator =
            ProxyGenerator::new(test_proxy_config(root.path().join("other-proxy-config")));
        let second =
            cache.resolve_playback_path(asset_id, &source, true, &second_generator, proxy_color());

        assert_eq!(second.path, source);
        assert_eq!(second.tier, CacheTier::SourceL3);
        assert!(!second.is_proxy);
    }
}
