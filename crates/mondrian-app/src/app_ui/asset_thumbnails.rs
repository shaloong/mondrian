//! Host-owned thumbnail cache for app UI asset cards.
//!
//! This module keeps media decoding and cache invalidation out of reusable
//! widgets. Panels ask for already-renderable thumbnails through
//! `AssetThumbnailSource`; missing video thumbnails are decoded on a background
//! worker and picked up by the host on the next event-loop wake.

use crate::app_ui::panels::{AssetThumbnailSource, AssetThumbnailState};
use crate::app_ui::preview_access_mode::{
    media_preview_access_mode_for_intent, MediaPreviewAccessIntent,
};
use mondrian_assets::{AssetKind, AssetRecord};
use mondrian_core::types::AssetId;
use mondrian_media::{
    decode_preview_rgba_scaled_cancellable, PreviewDecodeAccessMode, PreviewDecodeOutcome,
    PreviewDecodeRgbaRequest, PreviewFileFingerprint,
};
use mondrian_ui_widgets::RasterImage;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const THUMBNAIL_MAX_WIDTH: u32 = 320;
const THUMBNAIL_MAX_HEIGHT: u32 = 180;
const THUMBNAIL_MAX_COMPLETED_RESULTS_PER_POLL: usize = 8;
const THUMBNAIL_COMPLETED_RESULTS_POLL_BUDGET_US: u64 = 2_000;

#[derive(Debug, Clone)]
struct ThumbnailCacheEntry {
    path: PathBuf,
    fingerprint: PreviewFileFingerprint,
    image: RasterImage,
}

#[derive(Debug, Clone)]
struct ThumbnailFailureEntry {
    path: PathBuf,
    fingerprint: PreviewFileFingerprint,
}

#[derive(Debug)]
struct ThumbnailJob {
    asset_id: AssetId,
    path: PathBuf,
    fingerprint: PreviewFileFingerprint,
}

#[derive(Debug)]
struct ThumbnailResult {
    asset_id: AssetId,
    path: PathBuf,
    fingerprint: PreviewFileFingerprint,
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
        self.poll_finished_with_budget(
            THUMBNAIL_MAX_COMPLETED_RESULTS_PER_POLL,
            Duration::from_micros(THUMBNAIL_COMPLETED_RESULTS_POLL_BUDGET_US),
        )
    }

    fn poll_finished_with_budget(&self, max_results: usize, time_budget: Duration) -> bool {
        let poll_started = Instant::now();
        let mut changed = false;
        let mut drained = 0usize;
        while drained < max_results {
            if drained > 0 && poll_started.elapsed() >= time_budget {
                changed = true;
                break;
            }
            let result = match self.results.borrow().try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            };
            drained += 1;
            self.pending.borrow_mut().remove(&result.asset_id);
            if let Some(image) = result.image {
                self.cache.borrow_mut().insert(
                    result.asset_id,
                    ThumbnailCacheEntry {
                        path: result.path,
                        fingerprint: result.fingerprint,
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
                    ThumbnailFailureEntry { path: result.path, fingerprint: result.fingerprint },
                );
            }
        }
        if max_results > 0 && drained == max_results {
            changed = true;
        }
        changed
    }

    fn request_thumbnail(&self, asset: &AssetRecord, fingerprint: PreviewFileFingerprint) {
        if self.pending.borrow().contains(&asset.id) {
            return;
        }
        let job = ThumbnailJob {
            asset_id: asset.id,
            path: asset.path.clone(),
            fingerprint,
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
        let Ok(metadata) = std::fs::metadata(&asset.path) else {
            return AssetThumbnailState::Failed;
        };
        let fingerprint = PreviewFileFingerprint::from_metadata(&metadata);
        if let Some(entry) = self.cache.borrow().get(&asset.id) {
            if entry.path == asset.path && entry.fingerprint == fingerprint {
                return AssetThumbnailState::Ready(entry.image.clone());
            }
        }
        if let Some(failure) = self.failures.borrow().get(&asset.id) {
            if failure.path == asset.path && failure.fingerprint == fingerprint {
                return AssetThumbnailState::Failed;
            }
        }
        if self.pending.borrow().contains(&asset.id) {
            return AssetThumbnailState::Loading;
        }
        self.request_thumbnail(asset, fingerprint);
        AssetThumbnailState::Loading
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
    debug_assert_eq!(
        media_preview_access_mode_for_intent(MediaPreviewAccessIntent::DeterministicStill),
        PreviewDecodeAccessMode::RandomAccessStillFrame
    );
    let request = PreviewDecodeRgbaRequest::new(
        job.path.as_path(),
        0.0,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
    )
    .with_max_size(Some(THUMBNAIL_MAX_WIDTH), Some(THUMBNAIL_MAX_HEIGHT))
    .with_fingerprint(job.fingerprint);
    match decode_preview_rgba_scaled_cancellable(request, || false) {
        Ok(PreviewDecodeOutcome::Frame(frame)) => {
            let width = frame.width;
            let height = frame.height;
            let key = thumbnail_key(job.asset_id, width, height, job.fingerprint);
            let image = RasterImage::new(key, width, height, frame.into_data());
            ThumbnailResult {
                asset_id: job.asset_id,
                path: job.path,
                fingerprint: job.fingerprint,
                image,
                error: None,
            }
        }
        Ok(PreviewDecodeOutcome::Canceled) => ThumbnailResult {
            asset_id: job.asset_id,
            path: job.path,
            fingerprint: job.fingerprint,
            image: None,
            error: Some("thumbnail still-frame decode canceled unexpectedly".to_owned()),
        },
        Err(err) => ThumbnailResult {
            asset_id: job.asset_id,
            path: job.path,
            fingerprint: job.fingerprint,
            image: None,
            error: Some(err.to_string()),
        },
    }
}

fn thumbnail_key(
    asset_id: AssetId,
    width: u32,
    height: u32,
    fingerprint: PreviewFileFingerprint,
) -> String {
    let len = fingerprint.len.map_or_else(|| "unknown".to_owned(), |len| len.to_string());
    let modified = match (fingerprint.modified_secs, fingerprint.modified_nanos) {
        (Some(secs), Some(nanos)) => format!("{secs}-{nanos}"),
        _ => "unknown".to_owned(),
    };
    format!("asset-thumb:{asset_id}:{width}x{height}:len{len}:mtime{modified}")
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

    fn fingerprint(seed: u64) -> PreviewFileFingerprint {
        PreviewFileFingerprint {
            len: Some(seed),
            modified_secs: Some(seed),
            modified_nanos: Some(seed as u32),
        }
    }

    fn install_thumbnail_result_channel_for_test(
        cache: &AssetThumbnailCache,
    ) -> mpsc::Sender<ThumbnailResult> {
        let (result_tx, result_rx) = mpsc::channel();
        cache.results.replace(result_rx);
        result_tx
    }

    fn thumbnail_result(asset_id: AssetId, seed: u64) -> ThumbnailResult {
        let fingerprint = fingerprint(seed);
        ThumbnailResult {
            asset_id,
            path: PathBuf::from(format!("E:/media/thumb-{seed}.mov")),
            fingerprint,
            image: Some(
                RasterImage::new(
                    thumbnail_key(asset_id, 1, 1, fingerprint),
                    1,
                    1,
                    vec![seed as u8, 0, 0, 255],
                )
                .expect("test thumbnail image should be valid"),
            ),
            error: None,
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
        let fingerprint = PreviewFileFingerprint {
            len: Some(1234),
            modified_secs: Some(0),
            modified_nanos: Some(42_000_000),
        };

        let key = thumbnail_key(asset_id, 320, 180, fingerprint);

        assert_eq!(
            key,
            format!("asset-thumb:{asset_id}:320x180:len1234:mtime0-42000000")
        );
    }

    #[test]
    fn thumbnail_completion_poll_respects_result_count_budget() {
        let cache = AssetThumbnailCache::new();
        let results = install_thumbnail_result_channel_for_test(&cache);
        let ids = [AssetId::new(), AssetId::new(), AssetId::new()];
        for (index, id) in ids.iter().copied().enumerate() {
            cache.pending.borrow_mut().insert(id);
            results
                .send(thumbnail_result(id, index as u64 + 1))
                .expect("send thumbnail result");
        }

        assert!(cache.poll_finished_with_budget(2, Duration::from_secs(1)));
        assert_eq!(cache.cache.borrow().len(), 2);
        assert_eq!(cache.pending.borrow().len(), 1);

        assert!(cache.poll_finished_with_budget(2, Duration::from_secs(1)));
        assert_eq!(cache.cache.borrow().len(), 3);
        assert!(cache.pending.borrow().is_empty());
    }

    #[test]
    fn thumbnail_completion_poll_respects_time_budget() {
        let cache = AssetThumbnailCache::new();
        let results = install_thumbnail_result_channel_for_test(&cache);
        let ids = [AssetId::new(), AssetId::new()];
        for (index, id) in ids.iter().copied().enumerate() {
            cache.pending.borrow_mut().insert(id);
            results
                .send(thumbnail_result(id, index as u64 + 1))
                .expect("send thumbnail result");
        }

        assert!(cache.poll_finished_with_budget(8, Duration::ZERO));
        assert_eq!(cache.cache.borrow().len(), 1);
        assert_eq!(cache.pending.borrow().len(), 1);
    }
}
