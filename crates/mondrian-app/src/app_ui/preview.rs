//! Viewer preview service for the app UI host.
//!
//! The service owns render-plan interpretation and preview-frame cache keys.
//! Panels stay read-only and only consume `ViewerFrameImage` payloads.

use std::cell::{Cell, RefCell};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::UNIX_EPOCH;

use mondrian_assets::AssetKind;
use mondrian_core::types::{AssetId, BlendMode};
use mondrian_effects::CompiledEffectGraph;
use mondrian_renderer::{
    composite_timeline_elements, evaluate_timeline_render_plan, TimelineAdjustmentLayer,
    TimelineCompositeElement, TimelineCompositeOptions, TimelineCompositeScratch,
    TimelineEvaluationRequest, TimelineMediaLayer, TimelineRenderPlanElement,
    TimelineSolidColorLayer,
};
use mondrian_timeline::sequence::Sequence;
use mondrian_ui_widgets::ViewerFrameImage;

use crate::app::AppState;
use crate::app_ui::panels::ViewerPreviewSource;
use crate::app_ui::preview_scale::normalize_preview_resolution_scale;

const MAX_NESTED_PREVIEW_DEPTH: usize = 4;
const MEDIA_PREVIEW_CACHE_CAPACITY: usize = 96;
const MEDIA_PREVIEW_FORWARD_PREFETCH_FRAMES: i64 = 2;

/// Host-owned preview renderer used by the app UI viewer panel.
///
/// This first path renders solid-color render-plan elements through the shared
/// renderer compositor. Media and nested-sequence decode can attach here without
/// changing panel models or widget APIs.
pub struct AppUiPreviewService {
    jobs: mpsc::Sender<MediaPreviewJob>,
    results: RefCell<mpsc::Receiver<MediaPreviewResult>>,
    media_cache: RefCell<MediaPreviewCache>,
    media_failures: RefCell<HashSet<MediaPreviewKey>>,
    scheduler: MediaPreviewScheduler,
    scratch: RefCell<TimelineCompositeScratch>,
    current_generation: Cell<u64>,
}

impl AppUiPreviewService {
    /// Create an empty preview service.
    pub fn new() -> Self {
        let (job_tx, job_rx) = mpsc::channel::<MediaPreviewJob>();
        let (result_tx, result_rx) = mpsc::channel::<MediaPreviewResult>();
        let scheduler = MediaPreviewScheduler::default();
        let worker_scheduler = scheduler.clone();
        if let Err(err) = std::thread::Builder::new()
            .name("mondrian-ui-viewer-preview".to_owned())
            .spawn(move || media_preview_worker(job_rx, result_tx, worker_scheduler))
        {
            tracing::warn!("failed to start app UI viewer preview worker: {err}");
        }

        Self {
            jobs: job_tx,
            results: RefCell::new(result_rx),
            media_cache: RefCell::new(MediaPreviewCache::new(MEDIA_PREVIEW_CACHE_CAPACITY)),
            media_failures: RefCell::new(HashSet::new()),
            scheduler,
            scratch: RefCell::new(TimelineCompositeScratch::default()),
            current_generation: Cell::new(0),
        }
    }

    /// Poll completed background media preview decodes.
    pub fn poll_finished(&self) -> bool {
        let mut changed = false;
        while let Ok(result) = self.results.borrow().try_recv() {
            let is_current = self.scheduler.complete(&result.key, result.generation);
            match result.frame {
                Some(frame) => {
                    self.media_cache.borrow_mut().insert(result.key.clone(), frame);
                    self.media_failures.borrow_mut().remove(&result.key);
                    changed |= is_current;
                }
                None => {
                    if let Some(error) = result.error {
                        tracing::debug!(
                            asset_id = %result.key.asset_id,
                            path = %result.key.path.display(),
                            "viewer preview decode failed: {error}"
                        );
                    }
                    self.media_failures.borrow_mut().insert(result.key);
                    changed |= is_current;
                }
            }
        }
        changed
    }

    fn render_preview(&self, state: &AppState) -> Option<ViewerFrameImage> {
        let generation = self.scheduler.begin_generation();
        self.current_generation.set(generation);
        let sequence = state.sequence.as_ref()?;
        let frame = state.current_frame().max(0);
        let (width, height) = preview_dimensions_for_sequence(sequence);
        self.schedule_media_prefetches(state, sequence, frame, width, height);
        let resolved = self.resolve_sequence_elements(state, sequence, frame, width, height, 0)?;
        let rgba =
            composite_resolved_preview(width, height, &resolved, &mut self.scratch.borrow_mut());
        let key = preview_cache_key(frame, width, height, &rgba);
        ViewerFrameImage::new(key, width, height, rgba)
    }

    fn render_nested_sequence_frame(
        &self,
        state: &AppState,
        sequence: &Sequence,
        frame: i64,
        depth: usize,
    ) -> Option<MediaPreviewFrame> {
        if depth >= MAX_NESTED_PREVIEW_DEPTH {
            return None;
        }
        let (width, height) = preview_dimensions_for_sequence(sequence);
        let resolved =
            self.resolve_sequence_elements(state, sequence, frame.max(0), width, height, depth)?;
        let mut scratch = TimelineCompositeScratch::default();
        let rgba = composite_resolved_preview(width, height, &resolved, &mut scratch);
        Some(MediaPreviewFrame { width, height, rgba })
    }

    fn resolve_sequence_elements(
        &self,
        state: &AppState,
        sequence: &Sequence,
        frame: i64,
        width: u32,
        height: u32,
        depth: usize,
    ) -> Option<Vec<ResolvedPreviewElement>> {
        let evaluation = evaluate_timeline_render_plan(
            sequence,
            TimelineEvaluationRequest::preview(
                frame,
                normalize_preview_resolution_scale(sequence.settings.preview.resolution_scale),
            ),
        );
        if evaluation.is_empty() {
            return None;
        }

        let mut resolved = Vec::with_capacity(evaluation.len());
        for element in evaluation.elements {
            match element {
                TimelineRenderPlanElement::SolidColor(solid) => {
                    resolved.push(ResolvedPreviewElement::SolidColor(
                        TimelineSolidColorLayer {
                            color: solid.color,
                            opacity: solid.opacity,
                            blend_mode: solid.blend_mode,
                            transform: solid.transform,
                            effect_graph: solid.effect_graph,
                            frame_seed: solid.frame_seed,
                        },
                    ));
                }
                TimelineRenderPlanElement::Media(media) => {
                    let frame = self.media_frame_for_plan(
                        state,
                        &media.asset_id,
                        media.source_frame,
                        media.source_secs,
                        width,
                        height,
                    )?;
                    resolved.push(ResolvedPreviewElement::Media {
                        frame,
                        opacity: media.opacity,
                        blend_mode: media.blend_mode,
                        transform: media.transform,
                        effect_graph: media.effect_graph,
                        frame_seed: media.frame_seed,
                    });
                }
                TimelineRenderPlanElement::Adjustment(adjustment) => {
                    resolved.push(ResolvedPreviewElement::Adjustment(
                        TimelineAdjustmentLayer {
                            effect_graph: adjustment.effect_graph,
                            opacity: adjustment.opacity,
                            blend_mode: Some(adjustment.blend_mode),
                            frame_seed: adjustment.frame_seed,
                        },
                    ));
                }
                TimelineRenderPlanElement::NestedSequence(nested) => {
                    let nested_sequence = state.sequence_by_id(nested.sequence_id)?;
                    let frame = self.render_nested_sequence_frame(
                        state,
                        nested_sequence,
                        nested.source_frame,
                        depth + 1,
                    )?;
                    resolved.push(ResolvedPreviewElement::Media {
                        frame,
                        opacity: nested.opacity,
                        blend_mode: nested.blend_mode,
                        transform: nested.transform,
                        effect_graph: nested.effect_graph,
                        frame_seed: nested.frame_seed,
                    });
                }
            }
        }

        Some(resolved)
    }
}

enum ResolvedPreviewElement {
    SolidColor(TimelineSolidColorLayer),
    Adjustment(TimelineAdjustmentLayer),
    Media {
        frame: MediaPreviewFrame,
        opacity: f32,
        blend_mode: BlendMode,
        transform: [f32; 6],
        effect_graph: Arc<CompiledEffectGraph>,
        frame_seed: i64,
    },
}

impl ViewerPreviewSource for AppUiPreviewService {
    fn viewer_frame_for_state(&self, state: &AppState) -> Option<ViewerFrameImage> {
        self.render_preview(state)
    }
}

impl Default for AppUiPreviewService {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MediaPreviewKey {
    asset_id: AssetId,
    path: PathBuf,
    modified: Option<ModifiedStamp>,
    source_frame: i64,
    source_micros: i64,
    target_width: u32,
    target_height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ModifiedStamp {
    secs: u64,
    nanos: u32,
}

#[derive(Debug, Clone)]
struct MediaPreviewFrame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

struct MediaPreviewCache {
    capacity: usize,
    entries: HashMap<MediaPreviewKey, MediaPreviewFrame>,
    lru: VecDeque<MediaPreviewKey>,
}

impl MediaPreviewCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: HashMap::new(),
            lru: VecDeque::new(),
        }
    }

    fn get(&mut self, key: &MediaPreviewKey) -> Option<MediaPreviewFrame> {
        let frame = self.entries.get(key)?.clone();
        self.touch(key);
        Some(frame)
    }

    fn insert(&mut self, key: MediaPreviewKey, frame: MediaPreviewFrame) {
        self.entries.insert(key.clone(), frame);
        self.touch(&key);
        while self.entries.len() > self.capacity {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            if self.entries.remove(&oldest).is_some() {
                break;
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn touch(&mut self, key: &MediaPreviewKey) {
        self.lru.retain(|candidate| candidate != key);
        self.lru.push_back(key.clone());
    }
}

#[derive(Clone, Default)]
struct MediaPreviewScheduler {
    state: Arc<Mutex<MediaPreviewSchedulerState>>,
}

#[derive(Default)]
struct MediaPreviewSchedulerState {
    latest_generation: u64,
    pending: HashMap<MediaPreviewKey, u64>,
}

impl MediaPreviewScheduler {
    fn begin_generation(&self) -> u64 {
        let mut state = self.state.lock().expect("media preview scheduler poisoned");
        state.latest_generation = state.latest_generation.saturating_add(1);
        state.latest_generation
    }

    fn request(&self, key: MediaPreviewKey, generation: u64) -> bool {
        let mut state = self.state.lock().expect("media preview scheduler poisoned");
        let is_new = !state.pending.contains_key(&key);
        state.pending.insert(key, generation);
        is_new
    }

    fn should_decode(&self, key: &MediaPreviewKey) -> bool {
        let mut state = self.state.lock().expect("media preview scheduler poisoned");
        let Some(generation) = state.pending.get(key).copied() else {
            return false;
        };
        if generation >= state.latest_generation {
            return true;
        }
        state.pending.remove(key);
        false
    }

    fn complete(&self, key: &MediaPreviewKey, result_generation: u64) -> bool {
        let mut state = self.state.lock().expect("media preview scheduler poisoned");
        let pending_generation = state.pending.remove(key).unwrap_or(result_generation);
        pending_generation >= state.latest_generation
            || result_generation >= state.latest_generation
    }

    fn cancel(&self, key: &MediaPreviewKey) {
        self.state.lock().expect("media preview scheduler poisoned").pending.remove(key);
    }

    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.state.lock().expect("media preview scheduler poisoned").pending.len()
    }
}

#[derive(Debug)]
struct MediaPreviewJob {
    key: MediaPreviewKey,
    source_secs: f64,
    generation: u64,
}

#[derive(Debug)]
struct MediaPreviewResult {
    key: MediaPreviewKey,
    frame: Option<MediaPreviewFrame>,
    error: Option<String>,
    generation: u64,
}

impl AppUiPreviewService {
    fn schedule_media_prefetches(
        &self,
        state: &AppState,
        sequence: &Sequence,
        frame: i64,
        target_width: u32,
        target_height: u32,
    ) {
        if !state.is_playing() {
            return;
        }
        for offset in 1..=MEDIA_PREVIEW_FORWARD_PREFETCH_FRAMES {
            self.schedule_media_prefetch_for_sequence(
                state,
                sequence,
                frame.saturating_add(offset),
                target_width,
                target_height,
                0,
            );
        }
    }

    fn schedule_media_prefetch_for_sequence(
        &self,
        state: &AppState,
        sequence: &Sequence,
        frame: i64,
        target_width: u32,
        target_height: u32,
        depth: usize,
    ) {
        if depth >= MAX_NESTED_PREVIEW_DEPTH {
            return;
        }
        let evaluation = evaluate_timeline_render_plan(
            sequence,
            TimelineEvaluationRequest::preview(
                frame.max(0),
                normalize_preview_resolution_scale(sequence.settings.preview.resolution_scale),
            ),
        );

        for element in evaluation.elements {
            match element {
                TimelineRenderPlanElement::Media(media) => {
                    let Some((key, source_secs)) = self.media_preview_key_for_asset(
                        state,
                        &media.asset_id,
                        media.source_frame,
                        media.source_secs,
                        target_width,
                        target_height,
                    ) else {
                        continue;
                    };
                    if self.media_cache.borrow_mut().get(&key).is_none()
                        && !self.media_failures.borrow().contains(&key)
                    {
                        self.request_media_preview(key, source_secs);
                    }
                }
                TimelineRenderPlanElement::NestedSequence(nested) => {
                    if let Some(nested_sequence) = state.sequence_by_id(nested.sequence_id) {
                        let (nested_width, nested_height) =
                            preview_dimensions_for_sequence(nested_sequence);
                        self.schedule_media_prefetch_for_sequence(
                            state,
                            nested_sequence,
                            nested.source_frame,
                            nested_width,
                            nested_height,
                            depth + 1,
                        );
                    }
                }
                TimelineRenderPlanElement::SolidColor(_)
                | TimelineRenderPlanElement::Adjustment(_) => {}
            }
        }
    }

    fn media_frame_for_plan(
        &self,
        state: &AppState,
        asset_id: &AssetId,
        source_frame: i64,
        source_secs: f64,
        target_width: u32,
        target_height: u32,
    ) -> Option<MediaPreviewFrame> {
        let (key, source_secs) = self.media_preview_key_for_asset(
            state,
            asset_id,
            source_frame,
            source_secs,
            target_width,
            target_height,
        )?;
        if let Some(frame) = self.media_cache.borrow_mut().get(&key) {
            return Some(frame);
        }
        if self.media_failures.borrow().contains(&key) {
            return None;
        }
        self.request_media_preview(key, source_secs);
        None
    }

    fn media_preview_key_for_asset(
        &self,
        state: &AppState,
        asset_id: &AssetId,
        source_frame: i64,
        source_secs: f64,
        target_width: u32,
        target_height: u32,
    ) -> Option<(MediaPreviewKey, f64)> {
        let library = state.asset_library.as_ref()?;
        let asset = match library.get_asset(*asset_id) {
            Ok(Some(asset)) if asset.kind == AssetKind::Video => asset,
            Ok(_) => return None,
            Err(err) => {
                tracing::debug!(asset_id = %asset_id, "viewer preview asset lookup failed: {err}");
                return None;
            }
        };
        if !asset.path.exists() {
            return None;
        }

        let modified = modified_stamp(&asset.path);
        Some((
            MediaPreviewKey {
                asset_id: *asset_id,
                path: asset.path.clone(),
                modified,
                source_frame: source_frame.max(0),
                source_micros: source_micros(source_secs),
                target_width,
                target_height,
            },
            source_secs.max(0.0),
        ))
    }

    fn request_media_preview(&self, key: MediaPreviewKey, source_secs: f64) {
        let generation = self.current_generation.get();
        if !self.scheduler.request(key.clone(), generation) {
            return;
        }
        let job = MediaPreviewJob { key: key.clone(), source_secs, generation };
        match self.jobs.send(job) {
            Ok(()) => {}
            Err(err) => {
                self.scheduler.cancel(&key);
                tracing::debug!("viewer preview worker unavailable: {err}");
            }
        }
    }
}

fn preview_dimensions_for_sequence(sequence: &Sequence) -> (u32, u32) {
    let resolution = sequence.settings.resolution;
    let scale = normalize_preview_resolution_scale(sequence.settings.preview.resolution_scale);
    let width = ((resolution.width as f32 * scale).round() as u32).max(1);
    let height = ((resolution.height as f32 * scale).round() as u32).max(1);
    (width, height)
}

fn composite_resolved_preview(
    width: u32,
    height: u32,
    resolved: &[ResolvedPreviewElement],
    scratch: &mut TimelineCompositeScratch,
) -> Vec<u8> {
    let elements: Vec<_> = resolved
        .iter()
        .map(|element| match element {
            ResolvedPreviewElement::SolidColor(layer) => {
                TimelineCompositeElement::SolidColor(layer.clone())
            }
            ResolvedPreviewElement::Adjustment(layer) => {
                TimelineCompositeElement::Adjustment(layer.clone())
            }
            ResolvedPreviewElement::Media {
                frame,
                opacity,
                blend_mode,
                transform,
                effect_graph,
                frame_seed,
            } => TimelineCompositeElement::Media(TimelineMediaLayer {
                rgba: frame.rgba.as_slice(),
                width: frame.width,
                height: frame.height,
                opacity: *opacity,
                blend_mode: *blend_mode,
                transform: *transform,
                effect_graph: Arc::clone(effect_graph),
                frame_seed: *frame_seed,
            }),
        })
        .collect();
    composite_timeline_elements(
        width,
        height,
        &elements,
        TimelineCompositeOptions::default(),
        scratch,
    )
}

fn preview_cache_key(frame: i64, width: u32, height: u32, rgba: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    rgba.hash(&mut hasher);
    format!(
        "app UI-viewer:{width}x{height}:f{frame}:p{:016x}",
        hasher.finish()
    )
}

fn modified_stamp(path: &std::path::Path) -> Option<ModifiedStamp> {
    std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| ModifiedStamp {
            secs: duration.as_secs(),
            nanos: duration.subsec_nanos(),
        })
}

fn source_micros(source_secs: f64) -> i64 {
    (source_secs.max(0.0) * 1_000_000.0).round() as i64
}

fn media_preview_worker(
    jobs: mpsc::Receiver<MediaPreviewJob>,
    results: mpsc::Sender<MediaPreviewResult>,
    scheduler: MediaPreviewScheduler,
) {
    while let Ok(job) = jobs.recv() {
        if !scheduler.should_decode(&job.key) {
            continue;
        }
        let result = decode_media_preview(job);
        if results.send(result).is_err() {
            break;
        }
    }
}

fn decode_media_preview(job: MediaPreviewJob) -> MediaPreviewResult {
    match mondrian_media::decode_video_frame_at_time_rgba_scaled(
        job.key.path.as_path(),
        job.source_secs,
        Some(job.key.target_width.max(1)),
        Some(job.key.target_height.max(1)),
    ) {
        Ok(frame) => MediaPreviewResult {
            key: job.key,
            frame: Some(MediaPreviewFrame {
                width: frame.width,
                height: frame.height,
                rgba: frame.data,
            }),
            error: None,
            generation: job.generation,
        },
        Err(err) => MediaPreviewResult {
            key: job.key,
            frame: None,
            error: Some(err.to_string()),
            generation: job.generation,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use mondrian_core::types::{AssetId, TimeCode};
    use mondrian_core::Color;
    use mondrian_timeline::clip::Clip;
    use mondrian_timeline::sequence::Sequence;

    fn state_with_solid_color_clip(color: Color) -> AppState {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("preview");
        let tb = sequence.time_base();
        sequence.video_tracks[0]
            .add_clip(Clip::new_solid_color(
                AssetId::new(),
                color,
                TimeCode::new(0, tb),
                TimeCode::new(24, tb),
            ))
            .expect("solid clip should be insertable");
        state.sequence = Some(sequence);
        state.seek(4);
        state
    }

    #[test]
    fn solid_color_sequence_returns_preview_frame_at_preview_scale() {
        let service = AppUiPreviewService::new();
        let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));

        let frame = service
            .viewer_frame_for_state(&state)
            .expect("solid-color timeline should preview");

        assert_eq!(frame.width, 960);
        assert_eq!(frame.height, 540);
        assert_eq!(frame.rgba.len(), 960 * 540 * 4);
        assert!(frame.key.contains("app UI-viewer:960x540:f4:"));
    }

    #[test]
    fn preview_dimensions_clamp_invalid_resolution_scale() {
        let mut below_min = Sequence::new("below");
        below_min.settings.preview.resolution_scale = 0.0;
        assert_eq!(preview_dimensions_for_sequence(&below_min), (240, 135));

        let mut above_max = Sequence::new("above");
        above_max.settings.preview.resolution_scale = 2.0;
        assert_eq!(preview_dimensions_for_sequence(&above_max), (1920, 1080));

        let mut invalid = Sequence::new("invalid");
        invalid.settings.preview.resolution_scale = f32::NAN;
        assert_eq!(preview_dimensions_for_sequence(&invalid), (960, 540));
    }

    #[test]
    fn nested_solid_color_sequence_returns_preview_frame() {
        let mut state = AppState::new();
        let mut child = Sequence::new("child");
        let child_id = child.id;
        let child_tb = child.time_base();
        child.video_tracks[0]
            .add_clip(Clip::new_solid_color(
                AssetId::new(),
                Color::from_rgba8(48, 120, 220, 255),
                TimeCode::new(0, child_tb),
                TimeCode::new(24, child_tb),
            ))
            .expect("child solid clip");

        let mut parent = Sequence::new("parent");
        let parent_tb = parent.time_base();
        parent.video_tracks[0]
            .add_clip(Clip::new_nested_sequence(
                child_id,
                TimeCode::new(0, parent_tb),
                TimeCode::new(24, parent_tb),
                Some("child".to_owned()),
            ))
            .expect("parent nested clip");

        state.sequences.push(child);
        state.sequence = Some(parent);
        state.seek(3);

        let service = AppUiPreviewService::new();
        let frame = service
            .viewer_frame_for_state(&state)
            .expect("nested solid sequence should preview");

        assert_eq!(frame.width, 960);
        assert_eq!(frame.height, 540);
        assert_eq!(frame.rgba.len(), 960 * 540 * 4);
    }

    #[test]
    fn unsupported_media_plan_returns_no_partial_preview() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("media");
        let tb = sequence.time_base();
        sequence.video_tracks[0]
            .add_clip(Clip::new(
                AssetId::new(),
                TimeCode::new(0, tb),
                TimeCode::new(24, tb),
            ))
            .expect("media clip should be insertable");
        state.sequence = Some(sequence);

        let service = AppUiPreviewService::new();

        assert!(service.viewer_frame_for_state(&state).is_none());
    }

    #[test]
    fn preview_cache_key_changes_when_pixels_change() {
        let service = AppUiPreviewService::new();
        let first = service
            .viewer_frame_for_state(&state_with_solid_color_clip(Color::from_rgba8(
                255, 0, 0, 255,
            )))
            .expect("first preview");
        let second = service
            .viewer_frame_for_state(&state_with_solid_color_clip(Color::from_rgba8(
                0, 0, 255, 255,
            )))
            .expect("second preview");

        assert_ne!(first.key, second.key);
    }

    #[test]
    fn decode_media_preview_missing_file_reports_failure_without_frame() {
        let key = MediaPreviewKey {
            asset_id: AssetId::new(),
            path: PathBuf::from("E:/definitely-missing/mondrian-preview.mov"),
            modified: None,
            source_frame: 12,
            source_micros: source_micros(0.5),
            target_width: 320,
            target_height: 180,
        };

        let result = decode_media_preview(MediaPreviewJob {
            key: key.clone(),
            source_secs: 0.5,
            generation: 7,
        });

        assert_eq!(result.key, key);
        assert!(result.frame.is_none());
        assert!(result.error.is_some());
        assert_eq!(result.generation, 7);
    }

    fn test_media_key(source_frame: i64) -> MediaPreviewKey {
        MediaPreviewKey {
            asset_id: AssetId::new(),
            path: PathBuf::from(format!("E:/media/{source_frame}.mov")),
            modified: None,
            source_frame,
            source_micros: source_micros(source_frame as f64),
            target_width: 320,
            target_height: 180,
        }
    }

    fn test_media_frame(seed: u8) -> MediaPreviewFrame {
        MediaPreviewFrame { width: 1, height: 1, rgba: vec![seed, 0, 0, 255] }
    }

    #[test]
    fn media_preview_cache_evicts_least_recently_used_frame() {
        let mut cache = MediaPreviewCache::new(2);
        let first = test_media_key(1);
        let second = test_media_key(2);
        let third = test_media_key(3);

        cache.insert(first.clone(), test_media_frame(1));
        cache.insert(second.clone(), test_media_frame(2));
        assert!(cache.get(&first).is_some());

        cache.insert(third.clone(), test_media_frame(3));

        assert_eq!(cache.len(), 2);
        assert!(cache.get(&first).is_some());
        assert!(cache.get(&second).is_none());
        assert!(cache.get(&third).is_some());
    }

    #[test]
    fn media_preview_cache_updates_existing_frame_without_growing() {
        let mut cache = MediaPreviewCache::new(2);
        let key = test_media_key(1);

        cache.insert(key.clone(), test_media_frame(1));
        cache.insert(key.clone(), test_media_frame(9));

        let frame = cache.get(&key).expect("updated frame");
        assert_eq!(cache.len(), 1);
        assert_eq!(frame.rgba, vec![9, 0, 0, 255]);
    }

    #[test]
    fn media_preview_scheduler_skips_obsolete_generations() {
        let scheduler = MediaPreviewScheduler::default();
        let first_generation = scheduler.begin_generation();
        let key = MediaPreviewKey {
            asset_id: AssetId::new(),
            path: PathBuf::from("E:/media/a.mov"),
            modified: None,
            source_frame: 1,
            source_micros: source_micros(1.0),
            target_width: 320,
            target_height: 180,
        };
        assert!(scheduler.request(key.clone(), first_generation));

        scheduler.begin_generation();

        assert!(!scheduler.should_decode(&key));
        assert_eq!(scheduler.pending_len(), 0);
    }

    #[test]
    fn media_preview_scheduler_keeps_re_requested_key_current() {
        let scheduler = MediaPreviewScheduler::default();
        let first_generation = scheduler.begin_generation();
        let key = MediaPreviewKey {
            asset_id: AssetId::new(),
            path: PathBuf::from("E:/media/a.mov"),
            modified: None,
            source_frame: 1,
            source_micros: source_micros(1.0),
            target_width: 320,
            target_height: 180,
        };
        assert!(scheduler.request(key.clone(), first_generation));

        let second_generation = scheduler.begin_generation();
        assert!(!scheduler.request(key.clone(), second_generation));

        assert!(scheduler.should_decode(&key));
        assert!(scheduler.complete(&key, first_generation));
        assert_eq!(scheduler.pending_len(), 0);
    }
}
