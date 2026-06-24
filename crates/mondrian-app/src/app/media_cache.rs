//! Media-cache filesystem maintenance shared by app surfaces.
//!
//! Viewer widgets own GPU textures, decoder pools, and prefetch state. This
//! module owns only filesystem-level cache policy so legacy egui, self-hosted
//! preferences, and future background maintenance do not duplicate deletion
//! rules.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Policy for automatic media-cache cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MediaCachePolicy {
    pub(crate) max_size_bytes: u64,
    pub(crate) max_age_days: u64,
}

/// Filesystem deletion summary for a media-cache cleanup pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MediaCacheCleanupStats {
    pub(crate) deleted_files: usize,
    pub(crate) deleted_bytes: u64,
}

/// Current filesystem usage for one media-cache root.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MediaCacheUsageStats {
    pub(crate) file_count: usize,
    pub(crate) total_bytes: u64,
}

#[derive(Debug, Clone)]
struct CacheFileEntry {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}

/// Return current usage for all files below `cache_dir`.
pub(crate) fn media_cache_usage_stats(cache_dir: &Path) -> anyhow::Result<MediaCacheUsageStats> {
    if !cache_dir.exists() {
        return Ok(MediaCacheUsageStats::default());
    }

    let entries = list_cache_files(cache_dir)?;
    Ok(MediaCacheUsageStats {
        file_count: entries.len(),
        total_bytes: entries.iter().map(|entry| entry.size).sum(),
    })
}

/// Delete the entire media-cache directory and recreate it empty.
pub(crate) fn clear_media_cache_dir(cache_dir: &Path) -> anyhow::Result<MediaCacheCleanupStats> {
    let mut stats = MediaCacheCleanupStats::default();

    if cache_dir.exists() {
        let entries = list_cache_files(cache_dir)?;
        stats.deleted_files = entries.len();
        stats.deleted_bytes = entries.iter().map(|entry| entry.size).sum();
        std::fs::remove_dir_all(cache_dir)?;
    }

    std::fs::create_dir_all(cache_dir)?;
    Ok(stats)
}

/// Run an age pass, then a size pass, for all files below `cache_dir`.
pub(crate) fn run_media_cache_maintenance_for_dir(
    cache_dir: &Path,
    policy: MediaCachePolicy,
) -> anyhow::Result<MediaCacheCleanupStats> {
    if !cache_dir.exists() {
        return Ok(MediaCacheCleanupStats::default());
    }

    let mut entries = list_cache_files(cache_dir)?;
    let mut stats = MediaCacheCleanupStats::default();

    if policy.max_age_days > 0 {
        let max_age = Duration::from_secs(policy.max_age_days.saturating_mul(24 * 60 * 60));
        let now = SystemTime::now();
        for entry in &entries {
            if let Ok(age) = now.duration_since(entry.modified) {
                if age > max_age && std::fs::remove_file(&entry.path).is_ok() {
                    stats.deleted_files += 1;
                    stats.deleted_bytes = stats.deleted_bytes.saturating_add(entry.size);
                }
            }
        }
        entries = list_cache_files(cache_dir)?;
    }

    let mut total_size: u64 = entries.iter().map(|entry| entry.size).sum();
    if policy.max_size_bytes > 0 && total_size > policy.max_size_bytes {
        entries.sort_by_key(|entry| entry.modified);
        for entry in entries {
            if total_size <= policy.max_size_bytes {
                break;
            }
            if std::fs::remove_file(&entry.path).is_ok() {
                stats.deleted_files += 1;
                stats.deleted_bytes = stats.deleted_bytes.saturating_add(entry.size);
                total_size = total_size.saturating_sub(entry.size);
            }
        }
    }

    Ok(stats)
}

fn list_cache_files(root: &Path) -> anyhow::Result<Vec<CacheFileEntry>> {
    let mut stack = vec![root.to_path_buf()];
    let mut files = Vec::new();

    while let Some(dir) = stack.pop() {
        let read_dir = std::fs::read_dir(&dir)?;
        for entry in read_dir {
            let entry = entry?;
            let path = entry.path();
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };

            if metadata.is_dir() {
                stack.push(path);
                continue;
            }

            if !metadata.is_file() {
                continue;
            }

            files.push(CacheFileEntry {
                path,
                size: metadata.len(),
                modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(1);

    fn temp_cache_dir(name: &str) -> PathBuf {
        let unique = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "mondrian-media-cache-{name}-{}-{unique}",
            std::process::id()
        ))
    }

    fn write_file(path: &Path, bytes: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        let mut file = fs::File::create(path).expect("create file");
        file.write_all(&vec![7; bytes]).expect("write file");
    }

    #[test]
    fn usage_stats_missing_cache_dir_is_empty() {
        let dir = temp_cache_dir("missing");
        let usage = media_cache_usage_stats(&dir).expect("usage");

        assert_eq!(usage, MediaCacheUsageStats::default());
        assert!(!dir.exists());
    }

    #[test]
    fn usage_stats_counts_nested_files_only() {
        let dir = temp_cache_dir("nested");
        write_file(&dir.join("a.bin"), 3);
        write_file(&dir.join("nested").join("b.bin"), 5);
        fs::create_dir_all(dir.join("empty")).expect("empty dir");

        let usage = media_cache_usage_stats(&dir).expect("usage");

        assert_eq!(usage.file_count, 2);
        assert_eq!(usage.total_bytes, 8);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn size_cleanup_deletes_until_under_limit() {
        let dir = temp_cache_dir("size");
        write_file(&dir.join("a.bin"), 10);
        std::thread::sleep(Duration::from_millis(20));
        write_file(&dir.join("b.bin"), 20);

        let stats = run_media_cache_maintenance_for_dir(
            &dir,
            MediaCachePolicy { max_size_bytes: 20, max_age_days: 0 },
        )
        .expect("cleanup");
        let usage = media_cache_usage_stats(&dir).expect("usage");

        assert!(stats.deleted_files >= 1);
        assert!(stats.deleted_bytes >= 10);
        assert!(usage.total_bytes <= 20);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn clear_cache_deletes_files_and_recreates_root() {
        let dir = temp_cache_dir("clear");
        write_file(&dir.join("a.bin"), 6);
        write_file(&dir.join("nested").join("b.bin"), 4);

        let stats = clear_media_cache_dir(&dir).expect("clear");
        let usage = media_cache_usage_stats(&dir).expect("usage");

        assert_eq!(stats.deleted_files, 2);
        assert_eq!(stats.deleted_bytes, 10);
        assert!(dir.exists());
        assert_eq!(usage, MediaCacheUsageStats::default());
        let _ = fs::remove_dir_all(dir);
    }
}
