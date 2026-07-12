//! Host-owned thumbnail cache for app UI asset cards.
//!
//! This module keeps media decoding and cache invalidation out of reusable
//! widgets. Panels ask for already-renderable thumbnails through
//! `AssetThumbnailSource`; missing video thumbnails are decoded on a background
//! worker and picked up by the host on the next event-loop wake.

use crate::app_ui::panels::{
    AssetThumbnailFailure, AssetThumbnailFailureReason, AssetThumbnailSource, AssetThumbnailState,
};
use crate::app_ui::preview_access_mode::{
    media_preview_access_mode_for_intent, MediaPreviewAccessIntent,
};
use mondrian_assets::{AssetKind, AssetRecord};
use mondrian_core::types::{AssetId, ColorEngine, ColorSpace};
use mondrian_core::WorkingColorSpace;
use mondrian_media::{
    decode_preview_frame_cancellable, DecodedVideoRange, PreviewDecodeAccessMode,
    PreviewDecodeOutcome, PreviewDecodeRequest, PreviewFileFingerprint, PreviewSourceColorContract,
};
use mondrian_renderer::{
    execute_cpu_input_stage, execute_cpu_output_boundary_rgba8, CpuEncodedColorFrame,
    RenderInputTransform, RenderOutputColorBoundary,
};
use mondrian_timeline::sequence::{ColorContext, ResolvedInputColor};
use mondrian_ui_core::RasterImageColorSpace;
use mondrian_ui_widgets::RasterImage;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const THUMBNAIL_MAX_WIDTH: u32 = 320;
const THUMBNAIL_MAX_HEIGHT: u32 = 180;
const THUMBNAIL_MAX_COMPLETED_RESULTS_PER_POLL: usize = 8;
const THUMBNAIL_COMPLETED_RESULTS_POLL_BUDGET_US: u64 = 2_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ThumbnailColorContract {
    source_color_space: ColorSpace,
    source_range: DecodedVideoRange,
    working_color_space: WorkingColorSpace,
    output_color_space: ColorSpace,
    tone_map: bool,
    engine: ColorEngine,
    display: Option<String>,
    view: Option<String>,
    ocio_generation: u64,
    raster_color_space: RasterImageColorSpace,
}

impl ThumbnailColorContract {
    fn resolve(asset: &AssetRecord, context: &ColorContext) -> Result<Self, AssetThumbnailFailure> {
        let primary_video = asset.media_info.primary_video().ok_or_else(|| {
            thumbnail_failure(
                AssetThumbnailFailureReason::MissingVideoStreamContract,
                "video asset has no probed primary video stream",
            )
        })?;
        let detected = primary_video.detected_color_space;
        let source_color_space = match context
            .missing_metadata_policy
            .resolve_asset_input_decision(
                None,
                asset.interpretation,
                detected,
                context.working_color_space,
            )
            .resolved
        {
            ResolvedInputColor::Color(color_space) => color_space,
            ResolvedInputColor::Data => {
                return Err(thumbnail_failure(
                    AssetThumbnailFailureReason::NonColorDataUnsupported,
                    "video thumbnail decode cannot interpret YUV non-color data without a color sampling contract",
                ));
            }
            ResolvedInputColor::Rejected => {
                return Err(thumbnail_failure(
                    AssetThumbnailFailureReason::InputColorRejected,
                    "input color resolution rejected media metadata",
                ));
            }
        };
        if primary_video.color_range == DecodedVideoRange::Unknown {
            return Err(thumbnail_failure(
                AssetThumbnailFailureReason::UnresolvedSourceRange,
                "thumbnail decode requires an explicit full or limited source range",
            ));
        }
        let output_color_space = context.output_color_space.encoded().ok_or_else(|| {
            thumbnail_failure(
                AssetThumbnailFailureReason::InternalOutputIdentity,
                "thumbnail presentation requires an encoded output identity",
            )
        })?;
        let raster_color_space = match output_color_space {
            ColorSpace::Srgb => RasterImageColorSpace::Srgb,
            _ => {
                return Err(thumbnail_failure(
                    AssetThumbnailFailureReason::UnsupportedRasterOutput,
                    format!(
                        "thumbnail raster atlas does not support {output_color_space:?} output"
                    ),
                ));
            }
        };
        Ok(Self {
            source_color_space,
            source_range: primary_video.color_range,
            working_color_space: context.working_color_space,
            output_color_space,
            tone_map: context.tone_map,
            engine: context.engine.clone(),
            display: context.ocio_display.clone(),
            view: context.ocio_view.clone(),
            ocio_generation: mondrian_core::ocio_config_generation(),
            raster_color_space,
        })
    }

    fn output_boundary(&self) -> RenderOutputColorBoundary {
        match (&self.display, &self.view) {
            (Some(display), Some(view)) => RenderOutputColorBoundary::display_view(
                self.output_color_space,
                display.clone(),
                view.clone(),
                self.tone_map,
                self.engine.clone(),
            ),
            _ => RenderOutputColorBoundary::display(
                self.output_color_space,
                self.tone_map,
                self.engine.clone(),
            ),
        }
    }
}

fn thumbnail_failure(
    reason: AssetThumbnailFailureReason,
    detail: impl Into<String>,
) -> AssetThumbnailFailure {
    AssetThumbnailFailure::new(reason, detail)
}

#[derive(Debug, Clone)]
struct ThumbnailCacheEntry {
    path: PathBuf,
    fingerprint: PreviewFileFingerprint,
    color: ThumbnailColorContract,
    image: RasterImage,
}

#[derive(Debug, Clone)]
struct ThumbnailFailureEntry {
    path: PathBuf,
    fingerprint: PreviewFileFingerprint,
    color: Option<ThumbnailColorContract>,
    failure: AssetThumbnailFailure,
}

#[derive(Debug)]
struct ThumbnailJob {
    asset_id: AssetId,
    path: PathBuf,
    fingerprint: PreviewFileFingerprint,
    color: ThumbnailColorContract,
}

#[derive(Debug)]
struct ThumbnailResult {
    asset_id: AssetId,
    path: PathBuf,
    fingerprint: PreviewFileFingerprint,
    color: ThumbnailColorContract,
    result: Result<RasterImage, AssetThumbnailFailure>,
}

/// Deduplicated thumbnail failure counters for diagnostics and performance reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetThumbnailDiagnostics {
    failures: HashMap<AssetThumbnailFailureReason, u64>,
}

impl AssetThumbnailDiagnostics {
    /// Count failures recorded for one stable reason.
    pub fn failure_count(&self, reason: AssetThumbnailFailureReason) -> u64 {
        self.failures.get(&reason).copied().unwrap_or(0)
    }

    /// Total deduplicated failures recorded by the cache.
    pub fn total_failures(&self) -> u64 {
        self.failures.values().copied().fold(0, u64::saturating_add)
    }

    fn record(&mut self, reason: AssetThumbnailFailureReason) {
        let count = self.failures.entry(reason).or_default();
        *count = count.saturating_add(1);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ThumbnailRequestKey {
    asset_id: AssetId,
    path: PathBuf,
    fingerprint: PreviewFileFingerprint,
    color: ThumbnailColorContract,
}

impl ThumbnailRequestKey {
    fn new(
        asset_id: AssetId,
        path: PathBuf,
        fingerprint: PreviewFileFingerprint,
        color: ThumbnailColorContract,
    ) -> Self {
        Self { asset_id, path, fingerprint, color }
    }
}

/// Background thumbnail cache for project media assets.
pub struct AssetThumbnailCache {
    jobs: mpsc::Sender<ThumbnailJob>,
    results: RefCell<mpsc::Receiver<ThumbnailResult>>,
    cache: RefCell<HashMap<AssetId, ThumbnailCacheEntry>>,
    failures: RefCell<HashMap<AssetId, ThumbnailFailureEntry>>,
    pending: RefCell<HashSet<ThumbnailRequestKey>>,
    active_requests: RefCell<HashMap<AssetId, ThumbnailRequestKey>>,
    color_context: RefCell<Option<ColorContext>>,
    diagnostics: RefCell<AssetThumbnailDiagnostics>,
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
            active_requests: RefCell::new(HashMap::new()),
            color_context: RefCell::new(None),
            diagnostics: RefCell::new(AssetThumbnailDiagnostics::default()),
        }
    }

    /// Update the sequence/project color context used for new thumbnail requests.
    pub fn set_color_context(&self, context: Option<ColorContext>) {
        if *self.color_context.borrow() == context {
            return;
        }
        self.color_context.replace(context);
        self.cache.borrow_mut().clear();
        self.failures.borrow_mut().clear();
        self.pending.borrow_mut().clear();
        self.active_requests.borrow_mut().clear();
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
            let request_key = ThumbnailRequestKey::new(
                result.asset_id,
                result.path.clone(),
                result.fingerprint,
                result.color.clone(),
            );
            self.pending.borrow_mut().remove(&request_key);
            let is_active = self
                .active_requests
                .borrow()
                .get(&result.asset_id)
                .is_some_and(|active| active == &request_key);
            if !is_active {
                continue;
            }
            self.active_requests.borrow_mut().remove(&result.asset_id);
            match result.result {
                Ok(image) => {
                    self.cache.borrow_mut().insert(
                        result.asset_id,
                        ThumbnailCacheEntry {
                            path: result.path,
                            fingerprint: result.fingerprint,
                            color: result.color,
                            image,
                        },
                    );
                    self.failures.borrow_mut().remove(&result.asset_id);
                }
                Err(failure) => {
                    tracing::debug!(
                        asset_id = %result.asset_id,
                        path = %result.path.display(),
                        reason = failure.reason.code(),
                        detail = %failure.detail,
                        "asset thumbnail failed"
                    );
                    self.diagnostics.borrow_mut().record(failure.reason);
                    self.failures.borrow_mut().insert(
                        result.asset_id,
                        ThumbnailFailureEntry {
                            path: result.path,
                            fingerprint: result.fingerprint,
                            color: Some(result.color),
                            failure,
                        },
                    );
                }
            }
            changed = true;
        }
        if max_results > 0 && drained == max_results {
            changed = true;
        }
        changed
    }

    fn request_thumbnail(
        &self,
        asset: &AssetRecord,
        fingerprint: PreviewFileFingerprint,
        color: ThumbnailColorContract,
    ) -> Result<(), AssetThumbnailFailure> {
        let request_key =
            ThumbnailRequestKey::new(asset.id, asset.path.clone(), fingerprint, color.clone());
        if self.pending.borrow().contains(&request_key) {
            return Ok(());
        }
        let job = ThumbnailJob {
            asset_id: asset.id,
            path: asset.path.clone(),
            fingerprint,
            color,
        };
        match self.jobs.send(job) {
            Ok(()) => {
                self.pending.borrow_mut().insert(request_key.clone());
                self.active_requests.borrow_mut().insert(asset.id, request_key);
                Ok(())
            }
            Err(err) => Err(thumbnail_failure(
                AssetThumbnailFailureReason::WorkerUnavailable,
                format!("asset thumbnail worker unavailable: {err}"),
            )),
        }
    }

    #[cfg(test)]
    pub fn pending_count(&self) -> usize {
        self.pending.borrow().len()
    }

    /// Snapshot deduplicated thumbnail failure diagnostics.
    pub fn diagnostics(&self) -> AssetThumbnailDiagnostics {
        self.diagnostics.borrow().clone()
    }

    fn cache_failure(
        &self,
        asset_id: AssetId,
        path: PathBuf,
        fingerprint: PreviewFileFingerprint,
        color: Option<ThumbnailColorContract>,
        failure: AssetThumbnailFailure,
    ) -> AssetThumbnailState {
        let already_recorded = self.failures.borrow().get(&asset_id).is_some_and(|entry| {
            entry.path == path
                && entry.fingerprint == fingerprint
                && entry.color == color
                && entry.failure.reason == failure.reason
        });
        if !already_recorded {
            self.diagnostics.borrow_mut().record(failure.reason);
            self.failures.borrow_mut().insert(
                asset_id,
                ThumbnailFailureEntry { path, fingerprint, color, failure: failure.clone() },
            );
        }
        AssetThumbnailState::Failed(failure)
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
            return self.cache_failure(
                asset.id,
                asset.path.clone(),
                PreviewFileFingerprint {
                    len: None,
                    modified_secs: None,
                    modified_nanos: None,
                },
                None,
                thumbnail_failure(
                    AssetThumbnailFailureReason::MissingSourceFile,
                    format!("thumbnail source does not exist: {}", asset.path.display()),
                ),
            );
        };
        let fingerprint = PreviewFileFingerprint::from_metadata(&metadata);
        let Some(color_context) = self.color_context.borrow().clone() else {
            return AssetThumbnailState::Unavailable;
        };
        let color = match ThumbnailColorContract::resolve(asset, &color_context) {
            Ok(color) => color,
            Err(failure) => {
                return self.cache_failure(
                    asset.id,
                    asset.path.clone(),
                    fingerprint,
                    None,
                    failure,
                );
            }
        };
        if let Some(entry) = self.cache.borrow().get(&asset.id) {
            if entry.path == asset.path && entry.fingerprint == fingerprint && entry.color == color
            {
                return AssetThumbnailState::Ready(entry.image.clone());
            }
        }
        if let Some(failure) = self.failures.borrow().get(&asset.id) {
            if failure.path == asset.path
                && failure.fingerprint == fingerprint
                && failure.color.as_ref() == Some(&color)
            {
                return AssetThumbnailState::Failed(failure.failure.clone());
            }
        }
        let request_key =
            ThumbnailRequestKey::new(asset.id, asset.path.clone(), fingerprint, color.clone());
        if self.pending.borrow().contains(&request_key) {
            return AssetThumbnailState::Loading;
        }
        match self.request_thumbnail(asset, fingerprint, color.clone()) {
            Ok(()) => AssetThumbnailState::Loading,
            Err(failure) => self.cache_failure(
                asset.id,
                asset.path.clone(),
                fingerprint,
                Some(color),
                failure,
            ),
        }
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
    let raster_color_space = job.color.raster_color_space;
    let request = PreviewDecodeRequest::new(
        job.path.as_path(),
        0.0,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        PreviewSourceColorContract::new(job.color.source_color_space, job.color.source_range),
    )
    .with_max_size(Some(THUMBNAIL_MAX_WIDTH), Some(THUMBNAIL_MAX_HEIGHT))
    .with_fingerprint(job.fingerprint);
    let result = match decode_preview_frame_cancellable(request, || false) {
        Ok(PreviewDecodeOutcome::Frame(frame)) => {
            let width = frame.width;
            let height = frame.height;
            let rgba = color_manage_thumbnail_rgba(width, height, frame.into_data(), &job.color);
            let key = thumbnail_key(job.asset_id, width, height, job.fingerprint, &job.color);
            rgba.and_then(|rgba| {
                RasterImage::new(key, width, height, raster_color_space, rgba).ok_or_else(|| {
                    thumbnail_failure(
                        AssetThumbnailFailureReason::InvalidRasterPayload,
                        "thumbnail raster payload is invalid",
                    )
                })
            })
        }
        Ok(PreviewDecodeOutcome::Canceled) => Err(thumbnail_failure(
            AssetThumbnailFailureReason::DecodeCanceled,
            "thumbnail still-frame decode canceled unexpectedly",
        )),
        Ok(PreviewDecodeOutcome::NativeGpuFrame(frame)) => Err(thumbnail_failure(
            AssetThumbnailFailureReason::UnexpectedGpuFrame,
            format!(
                "thumbnail decode requires CPU RGBA, got native GPU {} {:?}",
                frame.handle_kind().as_str(),
                frame.surface_format
            ),
        )),
        Err(err) => Err(thumbnail_failure(
            AssetThumbnailFailureReason::DecodeFailed,
            err.to_string(),
        )),
    };
    ThumbnailResult {
        asset_id: job.asset_id,
        path: job.path,
        fingerprint: job.fingerprint,
        color: job.color,
        result,
    }
}

fn color_manage_thumbnail_rgba(
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    color: &ThumbnailColorContract,
) -> Result<Vec<u8>, AssetThumbnailFailure> {
    let source = CpuEncodedColorFrame::source_rgba8(width, height, color.source_color_space, rgba);
    let input =
        RenderInputTransform::to_working(color.working_color_space, false, color.engine.clone());
    let working = execute_cpu_input_stage(&source, &input).map_err(|err| {
        thumbnail_failure(
            AssetThumbnailFailureReason::InputTransformFailed,
            format!("thumbnail input color transform failed: {err}"),
        )
    })?;
    execute_cpu_output_boundary_rgba8(&working.result.frame, &color.output_boundary())
        .map(|output| output.rgba)
        .map_err(|err| {
            thumbnail_failure(
                AssetThumbnailFailureReason::OutputTransformFailed,
                format!("thumbnail display color transform failed: {err}"),
            )
        })
}

fn thumbnail_key(
    asset_id: AssetId,
    width: u32,
    height: u32,
    fingerprint: PreviewFileFingerprint,
    color: &ThumbnailColorContract,
) -> String {
    let mut color_hasher = std::collections::hash_map::DefaultHasher::new();
    color.hash(&mut color_hasher);
    let color_signature = color_hasher.finish();
    let len = fingerprint.len.map_or_else(|| "unknown".to_owned(), |len| len.to_string());
    let modified = match (fingerprint.modified_secs, fingerprint.modified_nanos) {
        (Some(secs), Some(nanos)) => format!("{secs}-{nanos}"),
        _ => "unknown".to_owned(),
    };
    format!(
        "asset-thumb:{asset_id}:{width}x{height}:len{len}:mtime{modified}:src{:?}:work{:?}:out{:?}:ocio{}:sig{color_signature:016x}",
        color.source_color_space,
        color.working_color_space,
        color.output_color_space,
        color.ocio_generation
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::timeline_data::AssetMediaInterpretation;
    use mondrian_core::Rational;
    use mondrian_media::info::{PixelFormat, VideoCodec};
    use mondrian_media::{
        DetectedColorInterpretation, MediaInfo, VideoColorDetectionMethod,
        VideoColorInterpretationConfidence, VideoColorSpaceSource, VideoStreamInfo,
    };
    use mondrian_timeline::sequence::MissingColorMetadataPolicy;

    fn asset(kind: AssetKind, path: PathBuf) -> AssetRecord {
        let mut media_info = MediaInfo::synthetic_solid_color();
        if kind == AssetKind::Video {
            media_info.has_video = true;
            media_info.video_streams.push(VideoStreamInfo {
                index: 0,
                codec: VideoCodec::H264,
                codec_profile: mondrian_media::VideoCodecProfile::Unknown,
                width: 1920,
                height: 1080,
                frame_rate: Rational::FPS_24,
                frame_rate_proven: true,
                pixel_format: PixelFormat::Yuv420p,
                pixel_format_proven: true,
                color_range: DecodedVideoRange::Limited,
                detected_color_space: Some(ColorSpace::Rec709),
                color_interpretation: DetectedColorInterpretation {
                    color_space: Some(ColorSpace::Rec709),
                    confidence: VideoColorInterpretationConfidence::High,
                    source: VideoColorSpaceSource::Metadata,
                    method: VideoColorDetectionMethod::CicpTags,
                    evidence: Vec::new(),
                    warnings: Vec::new(),
                    user_overridable: true,
                },
                color_space_source: VideoColorSpaceSource::Metadata,
                color_detection_method: VideoColorDetectionMethod::CicpTags,
                color_metadata: None,
                color_metadata_hints: Vec::new(),
                hdr_metadata: Vec::new(),
                bit_depth: 8,
                has_alpha: false,
                avg_bitrate: 10_000_000,
                total_frames: Some(240),
            });
        }
        AssetRecord {
            id: AssetId::new(),
            name: "Asset".to_owned(),
            kind,
            path: path.clone(),
            source: None,
            folder_id: None,
            interpretation: AssetMediaInterpretation::default(),
            media_info,
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

    fn thumbnail_color_contract() -> ThumbnailColorContract {
        ThumbnailColorContract {
            source_color_space: ColorSpace::Rec709,
            source_range: DecodedVideoRange::Limited,
            working_color_space: WorkingColorSpace::LinearRec709,
            output_color_space: ColorSpace::Srgb,
            tone_map: true,
            engine: ColorEngine::MondrianSmart,
            display: None,
            view: None,
            ocio_generation: mondrian_core::ocio_config_generation(),
            raster_color_space: RasterImageColorSpace::Srgb,
        }
    }

    fn configured_cache() -> AssetThumbnailCache {
        let cache = AssetThumbnailCache::new();
        let context = mondrian_timeline::sequence::SequenceSettings::default()
            .root_preview_color_context(
                &mondrian_core::ProjectColorManagement::default(),
                ColorSpace::Srgb,
            );
        cache.set_color_context(Some(context));
        cache
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
        let color = thumbnail_color_contract();
        ThumbnailResult {
            asset_id,
            path: PathBuf::from(format!("E:/media/thumb-{seed}.mov")),
            fingerprint,
            result: Ok(RasterImage::new(
                thumbnail_key(asset_id, 1, 1, fingerprint, &color),
                1,
                1,
                RasterImageColorSpace::Srgb,
                vec![seed as u8, 0, 0, 255],
            )
            .expect("test thumbnail image should be valid")),
            color,
        }
    }

    fn mark_thumbnail_result_active(cache: &AssetThumbnailCache, result: &ThumbnailResult) {
        let key = ThumbnailRequestKey::new(
            result.asset_id,
            result.path.clone(),
            result.fingerprint,
            result.color.clone(),
        );
        cache.pending.borrow_mut().insert(key.clone());
        cache.active_requests.borrow_mut().insert(result.asset_id, key);
    }

    #[test]
    fn non_video_assets_do_not_schedule_thumbnail_decodes() {
        let cache = configured_cache();
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
        let cache = configured_cache();
        let asset = asset(AssetKind::Video, PathBuf::from("E:/missing/video.mov"));

        assert!(matches!(
            cache.thumbnail_for_asset(&asset),
            AssetThumbnailState::Failed(_)
        ));
        assert_eq!(cache.pending_count(), 0);
        assert_eq!(
            cache
                .diagnostics()
                .failure_count(AssetThumbnailFailureReason::MissingSourceFile),
            1
        );

        assert!(matches!(
            cache.thumbnail_for_asset(&asset),
            AssetThumbnailState::Failed(_)
        ));
        assert_eq!(cache.diagnostics().total_failures(), 1);
    }

    #[test]
    fn thumbnail_color_contract_rejections_have_distinct_reasons() {
        let mut asset = asset(AssetKind::Video, PathBuf::from("E:/media/source.mov"));
        let context = mondrian_timeline::sequence::SequenceSettings::default()
            .root_preview_color_context(
                &mondrian_core::ProjectColorManagement::default(),
                ColorSpace::Srgb,
            );

        asset.interpretation.payload =
            mondrian_core::timeline_data::AssetColorPayload::NonColorData;
        assert_eq!(
            ThumbnailColorContract::resolve(&asset, &context)
                .expect_err("non-color data is unsupported")
                .reason,
            AssetThumbnailFailureReason::NonColorDataUnsupported
        );

        asset.interpretation = AssetMediaInterpretation::default();
        asset.media_info.video_streams[0].detected_color_space = None;
        let mut rejecting_context = context.clone();
        rejecting_context.missing_metadata_policy = MissingColorMetadataPolicy::RejectMedia;
        assert_eq!(
            ThumbnailColorContract::resolve(&asset, &rejecting_context)
                .expect_err("missing metadata is rejected")
                .reason,
            AssetThumbnailFailureReason::InputColorRejected
        );

        asset.media_info.video_streams[0].detected_color_space = Some(ColorSpace::Rec709);
        asset.media_info.video_streams[0].color_range = DecodedVideoRange::Unknown;
        assert_eq!(
            ThumbnailColorContract::resolve(&asset, &context)
                .expect_err("unknown range is unsupported")
                .reason,
            AssetThumbnailFailureReason::UnresolvedSourceRange
        );

        asset.media_info.video_streams[0].color_range = DecodedVideoRange::Limited;
        let mut internal_context = context.clone();
        internal_context.output_color_space =
            mondrian_core::OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec709);
        assert_eq!(
            ThumbnailColorContract::resolve(&asset, &internal_context)
                .expect_err("working output identity is internal")
                .reason,
            AssetThumbnailFailureReason::InternalOutputIdentity
        );

        asset.media_info.video_streams.clear();
        assert_eq!(
            ThumbnailColorContract::resolve(&asset, &context)
                .expect_err("missing stream contract")
                .reason,
            AssetThumbnailFailureReason::MissingVideoStreamContract
        );
    }

    #[test]
    fn disconnected_thumbnail_worker_fails_without_stuck_loading_state() {
        let (jobs, job_rx) = mpsc::channel();
        drop(job_rx);
        let (_result_tx, results) = mpsc::channel();
        let cache = AssetThumbnailCache {
            jobs,
            results: RefCell::new(results),
            cache: RefCell::new(HashMap::new()),
            failures: RefCell::new(HashMap::new()),
            pending: RefCell::new(HashSet::new()),
            active_requests: RefCell::new(HashMap::new()),
            color_context: RefCell::new(None),
            diagnostics: RefCell::new(AssetThumbnailDiagnostics::default()),
        };
        let context = mondrian_timeline::sequence::SequenceSettings::default()
            .root_preview_color_context(
                &mondrian_core::ProjectColorManagement::default(),
                ColorSpace::Srgb,
            );
        cache.set_color_context(Some(context));
        let path = std::env::temp_dir().join(format!(
            "mondrian-disconnected-thumbnail-worker-{}.mov",
            AssetId::new()
        ));
        std::fs::write(&path, b"fixture").expect("write fixture");
        let asset = asset(AssetKind::Video, path.clone());

        let state = cache.thumbnail_for_asset(&asset);

        assert!(matches!(
            state,
            AssetThumbnailState::Failed(AssetThumbnailFailure {
                reason: AssetThumbnailFailureReason::WorkerUnavailable,
                ..
            })
        ));
        assert_eq!(cache.pending_count(), 0);
        assert_eq!(
            cache
                .diagnostics()
                .failure_count(AssetThumbnailFailureReason::WorkerUnavailable),
            1
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn existing_video_files_return_loading_while_decode_is_pending() {
        let cache = configured_cache();
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

        let color = thumbnail_color_contract();
        let key = thumbnail_key(asset_id, 320, 180, fingerprint, &color);

        assert!(key.starts_with(&format!(
            "asset-thumb:{asset_id}:320x180:len1234:mtime0-42000000"
        )));
        assert!(key.contains("srcRec709:workLinearRec709:outSrgb:ocio"));
    }

    #[test]
    fn thumbnail_color_contract_changes_raster_identity() {
        let asset_id = AssetId::new();
        let fingerprint = fingerprint(42);
        let rec709 = thumbnail_color_contract();
        let mut pq = rec709.clone();
        pq.source_color_space = ColorSpace::Rec2100Pq;

        assert_ne!(
            thumbnail_key(asset_id, 320, 180, fingerprint, &rec709),
            thumbnail_key(asset_id, 320, 180, fingerprint, &pq)
        );
    }

    #[test]
    fn thumbnail_color_contract_uses_global_output_and_tone_map_policy() {
        let asset = asset(AssetKind::Video, PathBuf::from("E:/media/source.mov"));
        let mut context = mondrian_timeline::sequence::SequenceSettings::default()
            .root_preview_color_context(
                &mondrian_core::ProjectColorManagement::default(),
                ColorSpace::Srgb,
            );
        context.tone_map = false;

        let color = ThumbnailColorContract::resolve(&asset, &context)
            .expect("synthetic video has an input color interpretation");

        assert_eq!(
            Some(color.output_color_space),
            context.output_color_space.encoded()
        );
        assert_eq!(color.tone_map, context.tone_map);
        assert_eq!(color.raster_color_space, RasterImageColorSpace::Srgb);
    }

    #[test]
    fn thumbnail_decode_rejects_unsupported_output_before_media_decode() {
        let asset = asset(AssetKind::Video, PathBuf::from("E:/media/source.mov"));
        let context = mondrian_timeline::sequence::SequenceSettings::default()
            .root_preview_color_context(
                &mondrian_core::ProjectColorManagement::default(),
                ColorSpace::DciP3,
            );

        let failure = ThumbnailColorContract::resolve(&asset, &context)
            .expect_err("P3 output is unsupported by the raster atlas");

        assert_eq!(
            failure.reason,
            AssetThumbnailFailureReason::UnsupportedRasterOutput
        );
        assert!(failure.detail.contains("DciP3"));
    }

    #[test]
    fn thumbnail_pixels_cross_explicit_srgb_display_boundary() {
        let rec709 = thumbnail_color_contract();
        let mut pq = rec709.clone();
        pq.source_color_space = ColorSpace::Rec2100Pq;
        let source = vec![128, 96, 64, 255];

        let rec709_output = color_manage_thumbnail_rgba(1, 1, source.clone(), &rec709)
            .expect("Rec.709 thumbnail transform");
        let pq_output =
            color_manage_thumbnail_rgba(1, 1, source, &pq).expect("PQ thumbnail transform");

        assert_eq!(rec709_output.len(), 4);
        assert_eq!(pq_output.len(), 4);
        assert_ne!(rec709_output, pq_output);
    }

    #[test]
    fn thumbnail_completion_poll_respects_result_count_budget() {
        let cache = configured_cache();
        let results = install_thumbnail_result_channel_for_test(&cache);
        let ids = [AssetId::new(), AssetId::new(), AssetId::new()];
        for (index, id) in ids.iter().copied().enumerate() {
            let result = thumbnail_result(id, index as u64 + 1);
            mark_thumbnail_result_active(&cache, &result);
            results.send(result).expect("send thumbnail result");
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
        let cache = configured_cache();
        let results = install_thumbnail_result_channel_for_test(&cache);
        let ids = [AssetId::new(), AssetId::new()];
        for (index, id) in ids.iter().copied().enumerate() {
            let result = thumbnail_result(id, index as u64 + 1);
            mark_thumbnail_result_active(&cache, &result);
            results.send(result).expect("send thumbnail result");
        }

        assert!(cache.poll_finished_with_budget(8, Duration::ZERO));
        assert_eq!(cache.cache.borrow().len(), 1);
        assert_eq!(cache.pending.borrow().len(), 1);
    }

    #[test]
    fn thumbnail_failure_completion_requests_visible_refresh() {
        let cache = configured_cache();
        let results = install_thumbnail_result_channel_for_test(&cache);
        let asset_id = AssetId::new();
        let fingerprint = fingerprint(9);
        let color = thumbnail_color_contract();
        let path = PathBuf::from("E:/media/failed.mov");
        let result = ThumbnailResult {
            asset_id,
            path,
            fingerprint,
            color,
            result: Err(thumbnail_failure(
                AssetThumbnailFailureReason::OutputTransformFailed,
                "color transform failed",
            )),
        };
        mark_thumbnail_result_active(&cache, &result);
        results.send(result).expect("send failed thumbnail result");

        assert!(cache.poll_finished_with_budget(1, Duration::from_secs(1)));
        assert!(cache.pending.borrow().is_empty());
        assert!(cache.failures.borrow().contains_key(&asset_id));
        assert_eq!(
            cache
                .diagnostics()
                .failure_count(AssetThumbnailFailureReason::OutputTransformFailed),
            1
        );
    }

    #[test]
    fn stale_thumbnail_completion_cannot_overwrite_newer_color_request() {
        let cache = configured_cache();
        let results = install_thumbnail_result_channel_for_test(&cache);
        let asset_id = AssetId::new();
        let stale = thumbnail_result(asset_id, 10);
        let mut current = thumbnail_result(asset_id, 11);
        current.color.tone_map = !stale.color.tone_map;
        let current_tone_map = current.color.tone_map;
        mark_thumbnail_result_active(&cache, &stale);
        mark_thumbnail_result_active(&cache, &current);

        results.send(current).expect("send current result");
        results.send(stale).expect("send stale result");

        assert!(cache.poll_finished_with_budget(2, Duration::from_secs(1)));
        let entry = cache.cache.borrow().get(&asset_id).cloned().expect("current result cached");
        assert_eq!(entry.fingerprint, fingerprint(11));
        assert_eq!(entry.color.tone_map, current_tone_map);
        assert!(cache.pending.borrow().is_empty());
    }
}
