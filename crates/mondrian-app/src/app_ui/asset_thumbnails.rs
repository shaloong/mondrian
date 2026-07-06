//! Host-owned thumbnail cache for app UI asset cards.
//!
//! This module keeps media decoding and cache invalidation out of reusable
//! widgets. Panels ask for already-renderable thumbnails through
//! `AssetThumbnailSource`; missing video thumbnails are decoded on a background
//! worker and picked up by the host on the next event-loop wake.

use crate::app_ui::panels::{AssetThumbnailSource, AssetThumbnailState};
use mondrian_assets::{AssetKind, AssetRecord};
use mondrian_core::types::AssetId;
use mondrian_ui_widgets::RasterImage;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

const THUMBNAIL_MAX_WIDTH: u32 = 320;
const THUMBNAIL_MAX_HEIGHT: u32 = 180;

#[derive(Debug, Clone)]
struct ThumbnailCacheEntry {
    path: PathBuf,
    modified: Option<SystemTime>,
    image: RasterImage,
}

#[derive(Debug, Clone)]
struct ThumbnailFailureEntry {
    path: PathBuf,
    modified: Option<SystemTime>,
}

#[derive(Debug)]
struct ThumbnailJob {
    asset_id: AssetId,
    path: PathBuf,
    modified: Option<SystemTime>,
}

#[derive(Debug)]
struct ThumbnailResult {
    asset_id: AssetId,
    path: PathBuf,
    modified: Option<SystemTime>,
    image: Option<RasterImage>,
    error: Option<String>,
}

/// Background thumbnail cache for project media assets.
pub struct AssetThumbnailCache {
    jobs: mpsc::Sender<ThumbnailJob>,
    results: RefCell<mpsc::Receiver<ThumbnailResult>>,
    cache: RefCell<HashMap<AssetId, ThumbnailCacheEntry>>,
    failures: RefCell<HashMap<AssetId, ThumbnailFailureEntry>>,
    pending: RefCell<HashSet<AssetId>>,
}

impl AssetThumbnailCache {
    /// Create a thumbnail cache and start its decode worker.
    pub fn new() -> Self {
        let (job_tx, job_rx) = mpsc::channel::<ThumbnailJob>();
        let (result_tx, result_rx) = mpsc::channel::<ThumbnailResult>();
        if let Err(err) = std::thread::Builder::new()
            .name("mondrian-ui-asset-thumbnails".to_owned())
            .spawn(move || thumbnail_worker(job_rx, result_tx))
        {
            tracing::warn!("failed to start app UI asset thumbnail worker: {err}");
        }

        Self {
            jobs: job_tx,
            results: RefCell::new(result_rx),
            cache: RefCell::new(HashMap::new()),
            failures: RefCell::new(HashMap::new()),
            pending: RefCell::new(HashSet::new()),
        }
    }

    /// Poll completed background decodes. Returns true when visible model data
    /// may have changed and the host should refresh/repaint.
    pub fn poll_finished(&self) -> bool {
        let mut changed = false;
        while let Ok(result) = self.results.borrow().try_recv() {
            self.pending.borrow_mut().remove(&result.asset_id);
            if let Some(image) = result.image {
                self.cache.borrow_mut().insert(
                    result.asset_id,
                    ThumbnailCacheEntry {
                        path: result.path,
                        modified: result.modified,
                        image,
                    },
                );
                self.failures.borrow_mut().remove(&result.asset_id);
                changed = true;
            } else {
                if let Some(error) = result.error {
                    tracing::debug!(
                        asset_id = %result.asset_id,
                        path = %result.path.display(),
                        "asset thumbnail decode failed: {error}"
                    );
                }
                self.failures.borrow_mut().insert(
                    result.asset_id,
                    ThumbnailFailureEntry { path: result.path, modified: result.modified },
                );
            }
        }
        changed
    }

    fn request_thumbnail(&self, asset: &AssetRecord, modified: Option<SystemTime>) {
        if self.pending.borrow().contains(&asset.id) {
            return;
        }
        let job = ThumbnailJob {
            asset_id: asset.id,
            path: asset.path.clone(),
            modified,
        };
        match self.jobs.send(job) {
            Ok(()) => {
                self.pending.borrow_mut().insert(asset.id);
            }
            Err(err) => {
                tracing::debug!(asset_id = %asset.id, "asset thumbnail worker unavailable: {err}");
            }
        }
    }

    #[cfg(test)]
    pub fn pending_count(&self) -> usize {
        self.pending.borrow().len()
    }
}

impl Default for AssetThumbnailCache {
    fn default() -> Self {
        Self::new()
    }
}

impl AssetThumbnailSource for AssetThumbnailCache {
    fn thumbnail_for_asset(&self, asset: &AssetRecord) -> AssetThumbnailState {
        if asset.kind != AssetKind::Video {
            return AssetThumbnailState::Unavailable;
        }
        let modified = std::fs::metadata(&asset.path).ok().and_then(|m| m.modified().ok());
        if let Some(entry) = self.cache.borrow().get(&asset.id) {
            if entry.path == asset.path && entry.modified == modified {
                return AssetThumbnailState::Ready(entry.image.clone());
            }
        }
        if let Some(failure) = self.failures.borrow().get(&asset.id) {
            if failure.path == asset.path && failure.modified == modified {
                return AssetThumbnailState::Failed;
            }
        }
        if self.pending.borrow().contains(&asset.id) {
            return AssetThumbnailState::Loading;
        }
        if asset.path.exists() {
            self.request_thumbnail(asset, modified);
            return AssetThumbnailState::Loading;
        }
        AssetThumbnailState::Failed
    }
}

fn thumbnail_worker(jobs: mpsc::Receiver<ThumbnailJob>, results: mpsc::Sender<ThumbnailResult>) {
    while let Ok(job) = jobs.recv() {
        let result = decode_thumbnail(job);
        if results.send(result).is_err() {
            break;
        }
    }
}

fn decode_thumbnail(job: ThumbnailJob) -> ThumbnailResult {
    match mondrian_media::decode_video_frame_at_time_rgba_scaled(
        job.path.as_path(),
        0.0,
        Some(THUMBNAIL_MAX_WIDTH),
        Some(THUMBNAIL_MAX_HEIGHT),
    ) {
        Ok(frame) => {
            let width = frame.width;
            let height = frame.height;
            let key = thumbnail_key(job.asset_id, width, height, job.modified);
            let image = RasterImage::new(key, width, height, frame.into_data());
            ThumbnailResult {
                asset_id: job.asset_id,
                path: job.path,
                modified: job.modified,
                image,
                error: None,
            }
        }
        Err(err) => ThumbnailResult {
            asset_id: job.asset_id,
            path: job.path,
            modified: job.modified,
            image: None,
            error: Some(err.to_string()),
        },
    }
}

fn thumbnail_key(
    asset_id: AssetId,
    width: u32,
    height: u32,
    modified: Option<SystemTime>,
) -> String {
    let modified = modified
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| format!("{}-{}", duration.as_secs(), duration.subsec_nanos()))
        .unwrap_or_else(|| "unknown".to_owned());
    format!("asset-thumb:{asset_id}:{width}x{height}:{modified}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::timeline_data::AssetMediaInterpretation;
    use mondrian_media::MediaInfo;

    fn asset(kind: AssetKind, path: PathBuf) -> AssetRecord {
        AssetRecord {
            id: AssetId::new(),
            name: "Asset".to_owned(),
            kind,
            path: path.clone(),
            source: None,
            folder_id: None,
            interpretation: AssetMediaInterpretation::default(),
            media_info: MediaInfo::synthetic_solid_color(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn non_video_assets_do_not_schedule_thumbnail_decodes() {
        let cache = AssetThumbnailCache::new();
        let asset = asset(
            AssetKind::SolidColor,
            PathBuf::from("mondrian://solid-color"),
        );

        assert!(matches!(
            cache.thumbnail_for_asset(&asset),
            AssetThumbnailState::Unavailable
        ));
        assert_eq!(cache.pending_count(), 0);
    }

    #[test]
    fn missing_video_files_fail_without_scheduling_thumbnail_decodes() {
        let cache = AssetThumbnailCache::new();
        let asset = asset(AssetKind::Video, PathBuf::from("E:/missing/video.mov"));

        assert!(matches!(
            cache.thumbnail_for_asset(&asset),
            AssetThumbnailState::Failed
        ));
        assert_eq!(cache.pending_count(), 0);
    }

    #[test]
    fn existing_video_files_return_loading_while_decode_is_pending() {
        let cache = AssetThumbnailCache::new();
        let path = std::env::temp_dir().join(format!("mondrian-thumb-test-{}.mov", AssetId::new()));
        std::fs::write(&path, b"not actually a video").expect("write test file");
        let asset = asset(AssetKind::Video, path.clone());

        assert!(matches!(
            cache.thumbnail_for_asset(&asset),
            AssetThumbnailState::Loading
        ));
        assert_eq!(cache.pending_count(), 1);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn thumbnail_keys_include_asset_dimensions_and_modification_stamp() {
        let asset_id = AssetId::new();
        let modified = UNIX_EPOCH + std::time::Duration::from_millis(42);

        let key = thumbnail_key(asset_id, 320, 180, Some(modified));

        assert_eq!(key, format!("asset-thumb:{asset_id}:320x180:0-42000000"));
    }
}
