//! Viewer preview service for the app UI host.
//!
//! The service owns render-plan interpretation and preview-frame cache keys.
//! Panels stay read-only and only consume renderer-ready viewer frame content.

use std::cell::{Cell, RefCell};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::UNIX_EPOCH;

use mondrian_assets::AssetKind;
use mondrian_core::types::{AssetId, BlendMode, ColorEngine, ColorSpace, SequenceId};
use mondrian_effects::{CompiledEffectGraph, EffectCachePolicy};
use mondrian_media::VideoColorDiagnostic;
use mondrian_renderer::{
    composite_timeline_elements_color_frame_with_diagnostics, evaluate_timeline_render_plan,
    execute_cpu_input_stage, execute_cpu_output_boundary_rgba8, CpuColorFrame,
    CpuEncodedColorFrame, RenderColorStageDiagnostics, RenderColorTransformDiagnostics,
    RenderColorTransformDirection, RenderInputTransform, RenderOutputColorBoundary,
    TimelineAdjustmentLayer, TimelineCompositeDiagnostics, TimelineCompositeElement,
    TimelineCompositeOptions, TimelineCompositeScratch, TimelineEvaluationRequest,
    TimelineMediaLayer, TimelineRenderPlanElement, TimelineSolidColorLayer,
};
use mondrian_timeline::sequence::{ColorContext, Sequence};
use mondrian_ui_widgets::{ViewerExternalTextureFrame, ViewerFrameContent, ViewerFrameImage};

use crate::app::AppState;
use crate::app_ui::panels::{ViewerPreviewSource, ViewerPreviewState};
use crate::app_ui::preview_scale::normalize_preview_resolution_scale;

const MAX_NESTED_PREVIEW_DEPTH: usize = 4;
const MEDIA_PREVIEW_CACHE_CAPACITY: usize = 96;
const MEDIA_PREVIEW_FAILURE_CACHE_CAPACITY: usize = MEDIA_PREVIEW_CACHE_CAPACITY * 2;
const VIEWER_PREVIEW_FRAME_CACHE_CAPACITY: usize = 48;
const MEDIA_PREVIEW_FORWARD_PREFETCH_FRAMES: i64 = 2;
const MEDIA_PREVIEW_JOB_QUEUE_CAPACITY: usize = 48;
const MEDIA_PREVIEW_MAX_PENDING_REQUESTS: usize = MEDIA_PREVIEW_JOB_QUEUE_CAPACITY;

/// Host-owned preview renderer used by the app UI viewer panel.
///
/// This first path renders solid-color render-plan elements through the shared
/// renderer compositor. Media and nested-sequence decode can attach here without
/// changing panel models or widget APIs.
pub struct AppUiPreviewService {
    jobs: mpsc::SyncSender<MediaPreviewJob>,
    results: RefCell<mpsc::Receiver<MediaPreviewResult>>,
    media_cache: RefCell<MediaPreviewCache>,
    media_failures: RefCell<MediaPreviewFailureCache>,
    viewer_frame_cache: RefCell<ViewerPreviewFrameCache>,
    external_viewer_frame: RefCell<Option<ScopedExternalViewerFrame>>,
    scheduler: MediaPreviewScheduler,
    scratch: RefCell<TimelineCompositeScratch>,
    current_generation: Cell<u64>,
    current_frame_pending: Cell<bool>,
    last_ready_frame: RefCell<Option<ScopedViewerFrame>>,
    metrics: AppUiPreviewMetrics,
}

impl AppUiPreviewService {
    /// Create an empty preview service.
    pub fn new() -> Self {
        let (job_tx, job_rx) =
            mpsc::sync_channel::<MediaPreviewJob>(MEDIA_PREVIEW_JOB_QUEUE_CAPACITY);
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
            media_failures: RefCell::new(MediaPreviewFailureCache::new(
                MEDIA_PREVIEW_FAILURE_CACHE_CAPACITY,
            )),
            viewer_frame_cache: RefCell::new(ViewerPreviewFrameCache::new(
                VIEWER_PREVIEW_FRAME_CACHE_CAPACITY,
            )),
            external_viewer_frame: RefCell::new(None),
            scheduler,
            scratch: RefCell::new(TimelineCompositeScratch::default()),
            current_generation: Cell::new(0),
            current_frame_pending: Cell::new(false),
            last_ready_frame: RefCell::new(None),
            metrics: AppUiPreviewMetrics::default(),
        }
    }

    /// Return a point-in-time snapshot of preview scheduling and cache health.
    pub fn diagnostics(&self) -> AppUiPreviewDiagnostics {
        let scheduler = self.scheduler.diagnostics();
        AppUiPreviewDiagnostics {
            render_requests: self.metrics.render_requests.get(),
            ready_frames: self.metrics.ready_frames.get(),
            loading_frames: self.metrics.loading_frames.get(),
            stale_frames: self.metrics.stale_frames.get(),
            unavailable_frames: self.metrics.unavailable_frames.get(),
            media_cache_hits: self.metrics.media_cache_hits.get(),
            media_cache_misses: self.metrics.media_cache_misses.get(),
            media_failure_hits: self.metrics.media_failure_hits.get(),
            decode_successes: self.metrics.decode_successes.get(),
            decode_failures: self.metrics.decode_failures.get(),
            enqueued_jobs: self.metrics.enqueued_jobs.get(),
            queue_full_drops: self.metrics.queue_full_drops.get(),
            worker_disconnected_drops: self.metrics.worker_disconnected_drops.get(),
            scheduler,
            viewer_frame_cache_hits: self.metrics.viewer_frame_cache_hits.get(),
            viewer_frame_cache_misses: self.metrics.viewer_frame_cache_misses.get(),
            viewer_frame_cache_entries: self.viewer_frame_cache.borrow().len(),
            media_cache_entries: self.media_cache.borrow().len(),
            media_failure_entries: self.media_failures.borrow().len(),
            color_input_transform_calls: self.metrics.color_input_transform_calls.get(),
            color_input_transform_pixels: self.metrics.color_input_transform_pixels.get(),
            color_output_transform_calls: self.metrics.color_output_transform_calls.get(),
            color_output_transform_pixels: self.metrics.color_output_transform_pixels.get(),
            color_rgba8_boundary_calls: self.metrics.color_rgba8_boundary_calls.get(),
            color_stage_plans: self.metrics.color_stage_plans.get(),
            color_stage_total_stages: self.metrics.color_stage_total_stages.get(),
            color_stage_cpu_input_stages: self.metrics.color_stage_cpu_input_stages.get(),
            color_stage_cpu_output_stages: self.metrics.color_stage_cpu_output_stages.get(),
            color_stage_gpu_color_stages: self.metrics.color_stage_gpu_color_stages.get(),
            color_stage_upload_stages: self.metrics.color_stage_upload_stages.get(),
            color_stage_readback_stages: self.metrics.color_stage_readback_stages.get(),
            color_stage_gpu_blockers: self.metrics.color_stage_gpu_blockers.get(),
            color_stage_pixels: self.metrics.color_stage_pixels.get(),
            color_composite_plans: self.metrics.color_composite_plans.get(),
            color_composite_elements: self.metrics.color_composite_elements.get(),
            color_composite_float_linear: self.metrics.color_composite_float_linear.get(),
            color_composite_legacy_rgba8: self.metrics.color_composite_legacy_rgba8.get(),
            color_composite_legacy_media_blend_mode: self
                .metrics
                .color_composite_legacy_media_blend_mode
                .get(),
            color_composite_legacy_media_transform: self
                .metrics
                .color_composite_legacy_media_transform
                .get(),
            color_composite_legacy_media_effect: self
                .metrics
                .color_composite_legacy_media_effect
                .get(),
            color_composite_legacy_solid_blend_mode: self
                .metrics
                .color_composite_legacy_solid_blend_mode
                .get(),
            color_composite_legacy_solid_transform: self
                .metrics
                .color_composite_legacy_solid_transform
                .get(),
            color_composite_legacy_solid_effect: self
                .metrics
                .color_composite_legacy_solid_effect
                .get(),
            color_composite_legacy_adjustment_blend_mode: self
                .metrics
                .color_composite_legacy_adjustment_blend_mode
                .get(),
            color_composite_legacy_adjustment_effect: self
                .metrics
                .color_composite_legacy_adjustment_effect
                .get(),
        }
    }

    /// Poll completed background media preview decodes.
    pub fn poll_finished(&self) -> bool {
        let mut changed = false;
        while let Ok(result) = self.results.borrow().try_recv() {
            let is_current = self.scheduler.complete(&result.key, result.generation);
            match result.frame {
                Some(frame) => {
                    bump(&self.metrics.decode_successes);
                    if let Some(diagnostics) = result.color_diagnostics {
                        self.record_color_transform(diagnostics);
                    }
                    if let Some(diagnostics) = result.color_stage_diagnostics {
                        self.record_color_stage(diagnostics);
                    }
                    self.media_cache.borrow_mut().insert(result.key.clone(), frame);
                    self.media_failures.borrow_mut().remove(&result.key);
                    changed |= is_current;
                }
                None => {
                    bump(&self.metrics.decode_failures);
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

    fn render_preview(&self, state: &AppState) -> ViewerPreviewState {
        bump(&self.metrics.render_requests);
        let generation = self.scheduler.begin_generation();
        self.current_generation.set(generation);
        self.current_frame_pending.set(false);
        let Some(sequence) = state.sequence.as_ref() else {
            self.scheduler.prune_obsolete();
            self.last_ready_frame.replace(None);
            bump(&self.metrics.unavailable_frames);
            return ViewerPreviewState::Unavailable;
        };
        let frame = state.current_frame().max(0);
        let (width, height) = preview_dimensions_for_sequence(sequence);
        let display_color_space =
            preview_display_color_space(sequence, &state.project_settings.color_management);
        let color_context = sequence.settings.root_preview_color_context(
            &state.project_settings.color_management,
            display_color_space,
        );
        let preview_state = match self.resolve_sequence_elements(
            state,
            sequence,
            frame,
            width,
            height,
            0,
            color_context,
        ) {
            Some(resolved) => {
                if let Some(frame) = resolved
                    .cache_key
                    .as_ref()
                    .and_then(|cache_key| self.external_viewer_frame_for_key(cache_key))
                {
                    ViewerPreviewState::Ready(ViewerFrameContent::ExternalTexture(frame))
                } else if let Some(frame) = resolved
                    .cache_key
                    .as_ref()
                    .and_then(|cache_key| self.cached_viewer_frame(cache_key))
                {
                    self.last_ready_frame.replace(Some(ScopedViewerFrame {
                        sequence_id: sequence.id,
                        width,
                        height,
                        frame: frame.clone(),
                    }));
                    ViewerPreviewState::Ready(ViewerFrameContent::Raster(frame))
                } else {
                    let output = match composite_resolved_preview(
                        width,
                        height,
                        &resolved.elements,
                        &resolved.color_context,
                        &mut self.scratch.borrow_mut(),
                    ) {
                        Ok(rgba) => rgba,
                        Err(err) => {
                            tracing::warn!("viewer preview color render failed: {err}");
                            return ViewerPreviewState::Unavailable;
                        }
                    };
                    self.record_composite(output.composite_diagnostics);
                    self.record_color_transform(output.color_diagnostics);
                    self.record_color_stage(output.color_stage_diagnostics);
                    let rgba = output.rgba;
                    let key = preview_cache_key(frame, width, height, &rgba);
                    match ViewerFrameImage::new(key, width, height, rgba) {
                        Some(frame) => {
                            if let Some(cache_key) = resolved.cache_key {
                                self.viewer_frame_cache
                                    .borrow_mut()
                                    .insert(cache_key, frame.clone());
                            }
                            self.last_ready_frame.replace(Some(ScopedViewerFrame {
                                sequence_id: sequence.id,
                                width,
                                height,
                                frame: frame.clone(),
                            }));
                            ViewerPreviewState::Ready(ViewerFrameContent::Raster(frame))
                        }
                        None => ViewerPreviewState::Unavailable,
                    }
                }
            }
            None if self.current_frame_pending.get() => self
                .stale_frame_for_sequence(sequence, width, height)
                .map(|frame| ViewerPreviewState::Stale(ViewerFrameContent::Raster(frame)))
                .unwrap_or(ViewerPreviewState::Loading),
            None => ViewerPreviewState::Unavailable,
        };
        self.schedule_media_prefetches(state, sequence, frame, width, height);
        self.scheduler.prune_obsolete();
        self.record_preview_state(&preview_state);
        preview_state
    }

    /// Build a CPU working-space preview frame suitable for the app-window GPU output boundary.
    ///
    /// The returned frame is not display encoded. The app window owns wgpu recording,
    /// output texture registration, and texture lifetime.
    pub(crate) fn gpu_preview_frame_for_state(
        &self,
        state: &AppState,
    ) -> AppUiGpuPreviewFrameState {
        let generation = self.scheduler.begin_generation();
        self.current_generation.set(generation);
        self.current_frame_pending.set(false);
        let Some(sequence) = state.sequence.as_ref() else {
            self.scheduler.prune_obsolete();
            self.external_viewer_frame.replace(None);
            return AppUiGpuPreviewFrameState::Unavailable;
        };
        let frame = state.current_frame().max(0);
        let (width, height) = preview_dimensions_for_sequence(sequence);
        let display_color_space =
            preview_display_color_space(sequence, &state.project_settings.color_management);
        let color_context = sequence.settings.root_preview_color_context(
            &state.project_settings.color_management,
            display_color_space,
        );
        let resolved = match self.resolve_sequence_elements(
            state,
            sequence,
            frame,
            width,
            height,
            0,
            color_context,
        ) {
            Some(resolved) => resolved,
            None if self.current_frame_pending.get() => {
                self.schedule_media_prefetches(state, sequence, frame, width, height);
                self.scheduler.prune_obsolete();
                return AppUiGpuPreviewFrameState::Loading;
            }
            None => {
                self.scheduler.prune_obsolete();
                self.external_viewer_frame.replace(None);
                return AppUiGpuPreviewFrameState::Unavailable;
            }
        };
        let Some(cache_key) = resolved.cache_key.clone() else {
            self.scheduler.prune_obsolete();
            self.external_viewer_frame.replace(None);
            return AppUiGpuPreviewFrameState::Unavailable;
        };
        if self.external_viewer_frame_for_key(&cache_key).is_some() {
            self.schedule_media_prefetches(state, sequence, frame, width, height);
            self.scheduler.prune_obsolete();
            return AppUiGpuPreviewFrameState::Current;
        }
        let output = match composite_resolved_preview_working(
            width,
            height,
            &resolved.elements,
            &resolved.color_context,
            &mut self.scratch.borrow_mut(),
        ) {
            Ok(output) => output,
            Err(err) => {
                tracing::warn!("viewer GPU preview working composite failed: {err}");
                self.scheduler.prune_obsolete();
                return AppUiGpuPreviewFrameState::Unavailable;
            }
        };
        self.record_composite(output.composite_diagnostics);
        self.schedule_media_prefetches(state, sequence, frame, width, height);
        self.scheduler.prune_obsolete();
        AppUiGpuPreviewFrameState::Ready(Box::new(AppUiGpuPreviewFrame {
            cache_key,
            sequence_id: sequence.id,
            frame,
            width,
            height,
            working_frame: output.frame,
            boundary: output.boundary,
        }))
    }

    /// Mark a GPU preview output texture as the current viewer frame for its resolved plan.
    pub(crate) fn set_external_viewer_frame(
        &self,
        frame: &AppUiGpuPreviewFrame,
        texture_key: impl Into<String>,
    ) -> bool {
        let Some(content) = ViewerExternalTextureFrame::new(texture_key, frame.width, frame.height)
        else {
            return false;
        };
        self.external_viewer_frame.replace(Some(ScopedExternalViewerFrame {
            cache_key: frame.cache_key.clone(),
            content,
        }));
        true
    }

    /// Clear any external GPU viewer frame currently advertised by the service.
    pub(crate) fn clear_external_viewer_frame(&self) {
        self.external_viewer_frame.replace(None);
    }

    fn cached_viewer_frame(&self, key: &ViewerPreviewCacheKey) -> Option<ViewerFrameImage> {
        let frame = self.viewer_frame_cache.borrow_mut().get(key);
        if frame.is_some() {
            bump(&self.metrics.viewer_frame_cache_hits);
        } else {
            bump(&self.metrics.viewer_frame_cache_misses);
        }
        frame
    }

    fn external_viewer_frame_for_key(
        &self,
        key: &ViewerPreviewCacheKey,
    ) -> Option<ViewerExternalTextureFrame> {
        let frame = self.external_viewer_frame.borrow();
        let frame = frame.as_ref()?;
        (&frame.cache_key == key).then(|| frame.content.clone())
    }

    fn record_preview_state(&self, state: &ViewerPreviewState) {
        match state {
            ViewerPreviewState::Ready(_) => bump(&self.metrics.ready_frames),
            ViewerPreviewState::Loading => bump(&self.metrics.loading_frames),
            ViewerPreviewState::Stale(_) => bump(&self.metrics.stale_frames),
            ViewerPreviewState::Unavailable => bump(&self.metrics.unavailable_frames),
        }
    }

    fn record_color_transform(&self, diagnostics: RenderColorTransformDiagnostics) {
        match diagnostics.direction {
            RenderColorTransformDirection::InputToWorking => {
                bump(&self.metrics.color_input_transform_calls);
                add_cell(
                    &self.metrics.color_input_transform_pixels,
                    diagnostics.pixel_count as u64,
                );
            }
            RenderColorTransformDirection::WorkingToOutput => {
                bump(&self.metrics.color_output_transform_calls);
                add_cell(
                    &self.metrics.color_output_transform_pixels,
                    diagnostics.pixel_count as u64,
                );
            }
        }
        if diagnostics.used_rgba8_boundary {
            bump(&self.metrics.color_rgba8_boundary_calls);
        }
    }

    fn record_color_stage(&self, diagnostics: RenderColorStageDiagnostics) {
        bump(&self.metrics.color_stage_plans);
        add_cell(
            &self.metrics.color_stage_total_stages,
            diagnostics.total_stages,
        );
        add_cell(
            &self.metrics.color_stage_cpu_input_stages,
            diagnostics.cpu_input_stages,
        );
        add_cell(
            &self.metrics.color_stage_cpu_output_stages,
            diagnostics.cpu_output_stages,
        );
        add_cell(
            &self.metrics.color_stage_gpu_color_stages,
            diagnostics.gpu_color_stages,
        );
        add_cell(
            &self.metrics.color_stage_upload_stages,
            diagnostics.upload_stages,
        );
        add_cell(
            &self.metrics.color_stage_readback_stages,
            diagnostics.readback_stages,
        );
        add_cell(
            &self.metrics.color_stage_gpu_blockers,
            diagnostics.gpu_blockers,
        );
        add_cell(&self.metrics.color_stage_pixels, diagnostics.stage_pixels);
    }

    fn record_composite(&self, diagnostics: TimelineCompositeDiagnostics) {
        bump(&self.metrics.color_composite_plans);
        add_cell(&self.metrics.color_composite_elements, diagnostics.elements);
        add_cell(
            &self.metrics.color_composite_float_linear,
            diagnostics.float_linear_composites,
        );
        add_cell(
            &self.metrics.color_composite_legacy_rgba8,
            diagnostics.legacy_rgba8_composites,
        );
        add_cell(
            &self.metrics.color_composite_legacy_media_blend_mode,
            diagnostics.legacy_media_blend_mode,
        );
        add_cell(
            &self.metrics.color_composite_legacy_media_transform,
            diagnostics.legacy_media_transform,
        );
        add_cell(
            &self.metrics.color_composite_legacy_media_effect,
            diagnostics.legacy_media_effect,
        );
        add_cell(
            &self.metrics.color_composite_legacy_solid_blend_mode,
            diagnostics.legacy_solid_blend_mode,
        );
        add_cell(
            &self.metrics.color_composite_legacy_solid_transform,
            diagnostics.legacy_solid_transform,
        );
        add_cell(
            &self.metrics.color_composite_legacy_solid_effect,
            diagnostics.legacy_solid_effect,
        );
        add_cell(
            &self.metrics.color_composite_legacy_adjustment_blend_mode,
            diagnostics.legacy_adjustment_blend_mode,
        );
        add_cell(
            &self.metrics.color_composite_legacy_adjustment_effect,
            diagnostics.legacy_adjustment_effect,
        );
    }

    fn stale_frame_for_sequence(
        &self,
        sequence: &Sequence,
        width: u32,
        height: u32,
    ) -> Option<ViewerFrameImage> {
        let last = self.last_ready_frame.borrow();
        let frame = last.as_ref()?;
        (frame.sequence_id == sequence.id && frame.width == width && frame.height == height)
            .then(|| frame.frame.clone())
    }

    fn render_nested_sequence_frame(
        &self,
        state: &AppState,
        sequence: &Sequence,
        frame: i64,
        depth: usize,
        parent_color_context: ColorContext,
    ) -> Option<MediaPreviewFrame> {
        if depth >= MAX_NESTED_PREVIEW_DEPTH {
            return None;
        }
        let (width, height) = preview_dimensions_for_sequence(sequence);
        let color_context = sequence.settings.nested_render_color_context(parent_color_context);
        let resolved = self
            .resolve_sequence_elements(
                state,
                sequence,
                frame.max(0),
                width,
                height,
                depth,
                color_context.clone(),
            )?
            .elements;
        let mut scratch = TimelineCompositeScratch::default();
        let output =
            composite_resolved_preview(width, height, &resolved, &color_context, &mut scratch)
                .ok()?;
        self.record_composite(output.composite_diagnostics);
        self.record_color_transform(output.color_diagnostics);
        self.record_color_stage(output.color_stage_diagnostics);
        let rgba = output.rgba;
        let signature =
            nested_preview_frame_signature(sequence.id, frame.max(0), width, height, &rgba);
        let source = CpuEncodedColorFrame::source_rgba8(
            width,
            height,
            color_context.output_color_space,
            rgba,
        );
        let frame = execute_cpu_input_stage(
            &source,
            &RenderInputTransform::to_working(
                color_context.output_color_space,
                false,
                color_context.engine.clone(),
            ),
        )
        .ok()?;
        self.record_color_transform(frame.result.diagnostics);
        self.record_color_stage(frame.stage_diagnostics);
        Some(MediaPreviewFrame { frame: frame.result.frame, signature })
    }

    fn resolve_sequence_elements(
        &self,
        state: &AppState,
        sequence: &Sequence,
        frame: i64,
        width: u32,
        height: u32,
        depth: usize,
        color_context: ColorContext,
    ) -> Option<ResolvedPreviewPlan> {
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
                        media.color_space_override,
                        media.source_frame,
                        media.source_secs,
                        width,
                        height,
                        &color_context,
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
                        color_context.clone(),
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
        let cache_key = Some(viewer_preview_cache_key_for_resolved_plan(
            sequence.id,
            width,
            height,
            &resolved,
            &color_context,
        ));

        Some(ResolvedPreviewPlan { elements: resolved, cache_key, color_context })
    }
}

struct ResolvedPreviewPlan {
    elements: Vec<ResolvedPreviewElement>,
    cache_key: Option<ViewerPreviewCacheKey>,
    color_context: ColorContext,
}

/// Point-in-time preview service counters for local performance diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AppUiPreviewDiagnostics {
    /// Viewer preview render requests received by the service.
    pub render_requests: u64,
    /// Requests that produced a current ready frame.
    pub ready_frames: u64,
    /// Requests waiting for the first current frame.
    pub loading_frames: u64,
    /// Requests that reused a scoped previous frame while the current frame is pending.
    pub stale_frames: u64,
    /// Requests with no renderable preview frame.
    pub unavailable_frames: u64,
    /// Media preview cache hits.
    pub media_cache_hits: u64,
    /// Media preview cache misses.
    pub media_cache_misses: u64,
    /// Requests skipped because a media preview key is known to have failed.
    pub media_failure_hits: u64,
    /// Successful background media decodes received by the UI service.
    pub decode_successes: u64,
    /// Failed background media decodes received by the UI service.
    pub decode_failures: u64,
    /// Media preview jobs accepted by the worker queue.
    pub enqueued_jobs: u64,
    /// Media preview jobs dropped because the bounded worker queue was full.
    pub queue_full_drops: u64,
    /// Media preview jobs dropped because the worker channel was disconnected.
    pub worker_disconnected_drops: u64,
    /// Scheduler-side request, drop, completion, and pruning counters.
    pub scheduler: MediaPreviewSchedulerDiagnostics,
    /// Final viewer preview frame cache hits.
    pub viewer_frame_cache_hits: u64,
    /// Final viewer preview frame cache misses.
    pub viewer_frame_cache_misses: u64,
    /// Current number of final viewer preview frames in the bounded cache.
    pub viewer_frame_cache_entries: usize,
    /// Current number of frames in the media preview LRU cache.
    pub media_cache_entries: usize,
    /// Current number of keys in the media preview failure LRU cache.
    pub media_failure_entries: usize,
    /// Source/media color transforms into the timeline working space.
    pub color_input_transform_calls: u64,
    /// Pixels processed by source/media color transforms into the timeline working space.
    pub color_input_transform_pixels: u64,
    /// Timeline working-space transforms into preview/output encoding.
    pub color_output_transform_calls: u64,
    /// Pixels processed by timeline working-space transforms into preview/output encoding.
    pub color_output_transform_pixels: u64,
    /// Color transforms that crossed the temporary RGBA8 CPU boundary.
    pub color_rgba8_boundary_calls: u64,
    /// Render color stage plans executed by preview.
    pub color_stage_plans: u64,
    /// Render color stages executed by preview.
    pub color_stage_total_stages: u64,
    /// CPU source/media input stages executed by preview.
    pub color_stage_cpu_input_stages: u64,
    /// CPU working-to-output stages executed by preview.
    pub color_stage_cpu_output_stages: u64,
    /// GPU color stages planned by preview.
    pub color_stage_gpu_color_stages: u64,
    /// CPU-to-GPU upload stages planned by preview.
    pub color_stage_upload_stages: u64,
    /// GPU-to-CPU readback stages planned by preview.
    pub color_stage_readback_stages: u64,
    /// GPU color stage blockers surfaced by preview.
    pub color_stage_gpu_blockers: u64,
    /// Pixels covered by preview color stage plans.
    pub color_stage_pixels: u64,
    /// Timeline composite plans executed by preview.
    pub color_composite_plans: u64,
    /// Timeline composite elements processed by preview.
    pub color_composite_elements: u64,
    /// Composite plans that stayed on the float/linear path.
    pub color_composite_float_linear: u64,
    /// Composite plans that fell back to the legacy RGBA8 path.
    pub color_composite_legacy_rgba8: u64,
    /// Legacy RGBA8 fallbacks caused by media layer blend modes.
    pub color_composite_legacy_media_blend_mode: u64,
    /// Legacy RGBA8 fallbacks caused by media layer transforms.
    pub color_composite_legacy_media_transform: u64,
    /// Legacy RGBA8 fallbacks caused by media effect graphs.
    pub color_composite_legacy_media_effect: u64,
    /// Legacy RGBA8 fallbacks caused by solid layer blend modes.
    pub color_composite_legacy_solid_blend_mode: u64,
    /// Legacy RGBA8 fallbacks caused by solid layer transforms.
    pub color_composite_legacy_solid_transform: u64,
    /// Legacy RGBA8 fallbacks caused by solid layer effects.
    pub color_composite_legacy_solid_effect: u64,
    /// Legacy RGBA8 fallbacks caused by adjustment layer blend modes.
    pub color_composite_legacy_adjustment_blend_mode: u64,
    /// Legacy RGBA8 fallbacks caused by adjustment effect graphs.
    pub color_composite_legacy_adjustment_effect: u64,
}

/// Result of asking the preview service for a GPU-output viewer frame candidate.
pub(crate) enum AppUiGpuPreviewFrameState {
    /// The current resolved viewer frame is already backed by a registered external texture.
    Current,
    /// A working-space frame is ready for GPU output-boundary recording.
    Ready(Box<AppUiGpuPreviewFrame>),
    /// Required media is still decoding or rendering.
    Loading,
    /// No viewer preview frame is expected for the current state.
    Unavailable,
}

/// Working-space viewer frame plus the output boundary needed by the app-window GPU path.
pub(crate) struct AppUiGpuPreviewFrame {
    cache_key: ViewerPreviewCacheKey,
    /// Active sequence that produced this frame.
    pub sequence_id: SequenceId,
    /// Timeline frame number.
    pub frame: i64,
    /// Preview frame width in pixels.
    pub width: u32,
    /// Preview frame height in pixels.
    pub height: u32,
    /// CPU working-space composite frame that enters the GPU output boundary.
    pub working_frame: CpuColorFrame,
    /// Display/output boundary to execute on the GPU.
    pub boundary: RenderOutputColorBoundary,
}

impl AppUiGpuPreviewFrame {
    /// Stable external texture key for the resolved plan represented by this frame.
    pub fn external_texture_key(&self) -> String {
        format!(
            "app-ui.viewer.gpu:{}:{}x{}:{:016x}",
            self.sequence_id, self.width, self.height, self.cache_key.plan_signature
        )
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
    fn viewer_preview_for_state(&self, state: &AppState) -> ViewerPreviewState {
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
    input_color_space: ColorSpace,
    working_color_space: ColorSpace,
    tone_map: bool,
    engine: ColorEngine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ModifiedStamp {
    secs: u64,
    nanos: u32,
}

#[derive(Debug, Clone)]
struct MediaPreviewFrame {
    frame: CpuColorFrame,
    signature: u64,
}

impl MediaPreviewFrame {
    fn width(&self) -> u32 {
        self.frame.descriptor().width
    }

    fn height(&self) -> u32 {
        self.frame.descriptor().height
    }
}

#[derive(Debug, Clone)]
struct ScopedViewerFrame {
    sequence_id: SequenceId,
    width: u32,
    height: u32,
    frame: ViewerFrameImage,
}

#[derive(Debug, Clone)]
struct ScopedExternalViewerFrame {
    cache_key: ViewerPreviewCacheKey,
    content: ViewerExternalTextureFrame,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ViewerPreviewCacheKey {
    sequence_id: SequenceId,
    width: u32,
    height: u32,
    plan_signature: u64,
}

struct ViewerPreviewFrameCache {
    capacity: usize,
    entries: HashMap<ViewerPreviewCacheKey, ViewerFrameImage>,
    lru: VecDeque<ViewerPreviewCacheKey>,
}

impl ViewerPreviewFrameCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: HashMap::new(),
            lru: VecDeque::new(),
        }
    }

    fn get(&mut self, key: &ViewerPreviewCacheKey) -> Option<ViewerFrameImage> {
        let frame = self.entries.get(key)?.clone();
        self.touch(key);
        Some(frame)
    }

    fn insert(&mut self, key: ViewerPreviewCacheKey, frame: ViewerFrameImage) {
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

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn touch(&mut self, key: &ViewerPreviewCacheKey) {
        self.lru.retain(|candidate| candidate != key);
        self.lru.push_back(key.clone());
    }
}

struct MediaPreviewCache {
    capacity: usize,
    entries: HashMap<MediaPreviewKey, MediaPreviewFrame>,
    lru: VecDeque<MediaPreviewKey>,
}

struct MediaPreviewFailureCache {
    capacity: usize,
    entries: HashSet<MediaPreviewKey>,
    lru: VecDeque<MediaPreviewKey>,
}

impl MediaPreviewFailureCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: HashSet::new(),
            lru: VecDeque::new(),
        }
    }

    fn contains(&mut self, key: &MediaPreviewKey) -> bool {
        let contains = self.entries.contains(key);
        if contains {
            self.touch(key);
        }
        contains
    }

    fn insert(&mut self, key: MediaPreviewKey) {
        self.entries.insert(key.clone());
        self.touch(&key);
        while self.entries.len() > self.capacity {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            if self.entries.remove(&oldest) {
                break;
            }
        }
    }

    fn remove(&mut self, key: &MediaPreviewKey) {
        self.entries.remove(key);
        self.lru.retain(|candidate| candidate != key);
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn touch(&mut self, key: &MediaPreviewKey) {
        self.lru.retain(|candidate| candidate != key);
        self.lru.push_back(key.clone());
    }
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

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn touch(&mut self, key: &MediaPreviewKey) {
        self.lru.retain(|candidate| candidate != key);
        self.lru.push_back(key.clone());
    }
}

#[derive(Clone)]
struct MediaPreviewScheduler {
    state: Arc<Mutex<MediaPreviewSchedulerState>>,
    max_pending: usize,
}

#[derive(Default)]
struct MediaPreviewSchedulerState {
    latest_generation: u64,
    pending: HashMap<MediaPreviewKey, u64>,
    metrics: MediaPreviewSchedulerMetrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaPreviewRequestStatus {
    Scheduled,
    AlreadyPending,
    DroppedBackpressure,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct MediaPreviewSchedulerMetrics {
    scheduled_requests: u64,
    already_pending_requests: u64,
    dropped_backpressure_requests: u64,
    skipped_decode_jobs: u64,
    completed_current_results: u64,
    completed_stale_results: u64,
    canceled_requests: u64,
    pruned_obsolete_requests: u64,
}

/// Scheduler-side preview media request counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct MediaPreviewSchedulerDiagnostics {
    /// Latest render generation observed by the scheduler.
    pub latest_generation: u64,
    /// Requests currently waiting to decode or complete.
    pub pending_requests: usize,
    /// New requests accepted into the pending set.
    pub scheduled_requests: u64,
    /// Requests that updated an already-pending key.
    pub already_pending_requests: u64,
    /// Requests rejected by generation or pending-window backpressure.
    pub dropped_backpressure_requests: u64,
    /// Worker jobs skipped because their key was no longer pending/current.
    pub skipped_decode_jobs: u64,
    /// Completed jobs still relevant to the latest generation.
    pub completed_current_results: u64,
    /// Completed jobs that were stale by the time the UI polled them.
    pub completed_stale_results: u64,
    /// Pending requests canceled before completion.
    pub canceled_requests: u64,
    /// Obsolete pending requests removed during generation pruning.
    pub pruned_obsolete_requests: u64,
}

impl Default for MediaPreviewScheduler {
    fn default() -> Self {
        Self::with_max_pending(MEDIA_PREVIEW_MAX_PENDING_REQUESTS)
    }
}

impl MediaPreviewScheduler {
    fn with_max_pending(max_pending: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(MediaPreviewSchedulerState::default())),
            max_pending: max_pending.max(1),
        }
    }

    fn begin_generation(&self) -> u64 {
        let mut state = self.state.lock().expect("media preview scheduler poisoned");
        state.latest_generation = state.latest_generation.saturating_add(1);
        state.latest_generation
    }

    fn request(&self, key: MediaPreviewKey, generation: u64) -> MediaPreviewRequestStatus {
        let mut state = self.state.lock().expect("media preview scheduler poisoned");
        if generation < state.latest_generation {
            bump_value(&mut state.metrics.dropped_backpressure_requests);
            return MediaPreviewRequestStatus::DroppedBackpressure;
        }
        if let Some(pending_generation) = state.pending.get_mut(&key) {
            *pending_generation = generation;
            Self::prune_obsolete_locked(&mut state);
            bump_value(&mut state.metrics.already_pending_requests);
            return MediaPreviewRequestStatus::AlreadyPending;
        }
        Self::prune_obsolete_locked(&mut state);
        if state.pending.len() >= self.max_pending {
            bump_value(&mut state.metrics.dropped_backpressure_requests);
            return MediaPreviewRequestStatus::DroppedBackpressure;
        }
        state.pending.insert(key, generation);
        bump_value(&mut state.metrics.scheduled_requests);
        MediaPreviewRequestStatus::Scheduled
    }

    fn should_decode(&self, key: &MediaPreviewKey) -> bool {
        let mut state = self.state.lock().expect("media preview scheduler poisoned");
        let Some(generation) = state.pending.get(key).copied() else {
            bump_value(&mut state.metrics.skipped_decode_jobs);
            return false;
        };
        if generation >= state.latest_generation {
            return true;
        }
        state.pending.remove(key);
        bump_value(&mut state.metrics.skipped_decode_jobs);
        false
    }

    fn complete(&self, key: &MediaPreviewKey, result_generation: u64) -> bool {
        let mut state = self.state.lock().expect("media preview scheduler poisoned");
        let pending_generation = state.pending.remove(key).unwrap_or(result_generation);
        let is_current = pending_generation >= state.latest_generation
            || result_generation >= state.latest_generation;
        if is_current {
            bump_value(&mut state.metrics.completed_current_results);
        } else {
            bump_value(&mut state.metrics.completed_stale_results);
        }
        is_current
    }

    fn cancel(&self, key: &MediaPreviewKey) {
        let mut state = self.state.lock().expect("media preview scheduler poisoned");
        if state.pending.remove(key).is_some() {
            bump_value(&mut state.metrics.canceled_requests);
        }
    }

    fn prune_obsolete(&self) {
        let mut state = self.state.lock().expect("media preview scheduler poisoned");
        Self::prune_obsolete_locked(&mut state);
    }

    fn prune_obsolete_locked(state: &mut MediaPreviewSchedulerState) {
        let latest_generation = state.latest_generation;
        let before = state.pending.len();
        state.pending.retain(|_, generation| *generation >= latest_generation);
        let pruned = before.saturating_sub(state.pending.len()) as u64;
        state.metrics.pruned_obsolete_requests =
            state.metrics.pruned_obsolete_requests.saturating_add(pruned);
    }

    fn diagnostics(&self) -> MediaPreviewSchedulerDiagnostics {
        let state = self.state.lock().expect("media preview scheduler poisoned");
        MediaPreviewSchedulerDiagnostics {
            latest_generation: state.latest_generation,
            pending_requests: state.pending.len(),
            scheduled_requests: state.metrics.scheduled_requests,
            already_pending_requests: state.metrics.already_pending_requests,
            dropped_backpressure_requests: state.metrics.dropped_backpressure_requests,
            skipped_decode_jobs: state.metrics.skipped_decode_jobs,
            completed_current_results: state.metrics.completed_current_results,
            completed_stale_results: state.metrics.completed_stale_results,
            canceled_requests: state.metrics.canceled_requests,
            pruned_obsolete_requests: state.metrics.pruned_obsolete_requests,
        }
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
    color_diagnostics: Option<RenderColorTransformDiagnostics>,
    color_stage_diagnostics: Option<RenderColorStageDiagnostics>,
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
        let display_color_space =
            preview_display_color_space(sequence, &state.project_settings.color_management);
        let color_context = sequence.settings.root_preview_color_context(
            &state.project_settings.color_management,
            display_color_space,
        );
        for offset in 1..=MEDIA_PREVIEW_FORWARD_PREFETCH_FRAMES {
            self.schedule_media_prefetch_for_sequence(
                state,
                sequence,
                frame.saturating_add(offset),
                target_width,
                target_height,
                0,
                color_context.clone(),
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
        color_context: ColorContext,
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
                        media.color_space_override,
                        media.source_frame,
                        media.source_secs,
                        target_width,
                        target_height,
                        &color_context,
                    ) else {
                        continue;
                    };
                    if self.cached_media_frame(&key).is_none() && !self.failed_media_key(&key) {
                        self.request_media_preview(key, source_secs);
                    }
                }
                TimelineRenderPlanElement::NestedSequence(nested) => {
                    if let Some(nested_sequence) = state.sequence_by_id(nested.sequence_id) {
                        let (nested_width, nested_height) =
                            preview_dimensions_for_sequence(nested_sequence);
                        let nested_context = nested_sequence
                            .settings
                            .nested_render_color_context(color_context.clone());
                        self.schedule_media_prefetch_for_sequence(
                            state,
                            nested_sequence,
                            nested.source_frame,
                            nested_width,
                            nested_height,
                            depth + 1,
                            nested_context,
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
        color_space_override: Option<ColorSpace>,
        source_frame: i64,
        source_secs: f64,
        target_width: u32,
        target_height: u32,
        color_context: &ColorContext,
    ) -> Option<MediaPreviewFrame> {
        let (key, source_secs) = self.media_preview_key_for_asset(
            state,
            asset_id,
            color_space_override,
            source_frame,
            source_secs,
            target_width,
            target_height,
            color_context,
        )?;
        if let Some(frame) = self.cached_media_frame(&key) {
            return Some(frame);
        }
        if self.failed_media_key(&key) {
            self.current_frame_pending.set(true);
            return None;
        }
        self.current_frame_pending.set(true);
        self.request_media_preview(key, source_secs);
        None
    }

    fn cached_media_frame(&self, key: &MediaPreviewKey) -> Option<MediaPreviewFrame> {
        let frame = self.media_cache.borrow_mut().get(key);
        if frame.is_some() {
            bump(&self.metrics.media_cache_hits);
        } else {
            bump(&self.metrics.media_cache_misses);
        }
        frame
    }

    fn failed_media_key(&self, key: &MediaPreviewKey) -> bool {
        let failed = self.media_failures.borrow_mut().contains(key);
        if failed {
            bump(&self.metrics.media_failure_hits);
        }
        failed
    }

    fn media_preview_key_for_asset(
        &self,
        state: &AppState,
        asset_id: &AssetId,
        color_space_override: Option<ColorSpace>,
        source_frame: i64,
        source_secs: f64,
        target_width: u32,
        target_height: u32,
        color_context: &ColorContext,
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
        let detected_color_space = asset
            .media_info
            .video_streams
            .first()
            .and_then(|video| video.detected_color_space);
        let input_color_space = match resolve_preview_input_color_space(
            color_space_override,
            detected_color_space,
            color_context,
        ) {
            Some(color_space) => color_space,
            None => {
                let diagnostic = asset
                    .media_info
                    .primary_video()
                    .map(VideoColorDiagnostic::from_stream)
                    .map(|diagnostic| diagnostic.summary())
                    .unwrap_or_else(|| "unavailable".to_string());
                tracing::warn!(
                    asset_id = %asset_id,
                    path = %asset.path.display(),
                    missing_metadata_policy = ?color_context.missing_metadata_policy,
                    color_diagnostic = %diagnostic,
                    "viewer preview rejected media with missing color metadata"
                );
                return None;
            }
        };
        Some((
            MediaPreviewKey {
                asset_id: *asset_id,
                path: asset.path.clone(),
                modified,
                source_frame: source_frame.max(0),
                source_micros: source_micros(source_secs),
                target_width,
                target_height,
                input_color_space,
                working_color_space: color_context.working_color_space,
                tone_map: color_context.tone_map,
                engine: color_context.engine.clone(),
            },
            source_secs.max(0.0),
        ))
    }

    fn request_media_preview(&self, key: MediaPreviewKey, source_secs: f64) {
        let generation = self.current_generation.get();
        match self.scheduler.request(key.clone(), generation) {
            MediaPreviewRequestStatus::Scheduled => {}
            MediaPreviewRequestStatus::AlreadyPending => return,
            MediaPreviewRequestStatus::DroppedBackpressure => {
                tracing::trace!(
                    asset_id = %key.asset_id,
                    source_frame = key.source_frame,
                    "viewer preview request dropped by backpressure"
                );
                return;
            }
        }
        let job = MediaPreviewJob { key: key.clone(), source_secs, generation };
        match self.jobs.try_send(job) {
            Ok(()) => bump(&self.metrics.enqueued_jobs),
            Err(mpsc::TrySendError::Full(job)) => {
                bump(&self.metrics.queue_full_drops);
                self.scheduler.cancel(&job.key);
                tracing::trace!(
                    asset_id = %job.key.asset_id,
                    source_frame = job.key.source_frame,
                    "viewer preview queue full; dropping media preview request"
                );
            }
            Err(mpsc::TrySendError::Disconnected(job)) => {
                bump(&self.metrics.worker_disconnected_drops);
                self.scheduler.cancel(&job.key);
                tracing::debug!("viewer preview worker unavailable");
            }
        }
    }
}

fn resolve_preview_input_color_space(
    override_color_space: Option<ColorSpace>,
    detected_color_space: Option<ColorSpace>,
    color_context: &ColorContext,
) -> Option<ColorSpace> {
    override_color_space.or_else(|| {
        color_context
            .missing_metadata_policy
            .resolve_input(detected_color_space, color_context.working_color_space)
    })
}

#[derive(Default)]
struct AppUiPreviewMetrics {
    render_requests: Cell<u64>,
    ready_frames: Cell<u64>,
    loading_frames: Cell<u64>,
    stale_frames: Cell<u64>,
    unavailable_frames: Cell<u64>,
    viewer_frame_cache_hits: Cell<u64>,
    viewer_frame_cache_misses: Cell<u64>,
    media_cache_hits: Cell<u64>,
    media_cache_misses: Cell<u64>,
    media_failure_hits: Cell<u64>,
    decode_successes: Cell<u64>,
    decode_failures: Cell<u64>,
    enqueued_jobs: Cell<u64>,
    queue_full_drops: Cell<u64>,
    worker_disconnected_drops: Cell<u64>,
    color_input_transform_calls: Cell<u64>,
    color_input_transform_pixels: Cell<u64>,
    color_output_transform_calls: Cell<u64>,
    color_output_transform_pixels: Cell<u64>,
    color_rgba8_boundary_calls: Cell<u64>,
    color_stage_plans: Cell<u64>,
    color_stage_total_stages: Cell<u64>,
    color_stage_cpu_input_stages: Cell<u64>,
    color_stage_cpu_output_stages: Cell<u64>,
    color_stage_gpu_color_stages: Cell<u64>,
    color_stage_upload_stages: Cell<u64>,
    color_stage_readback_stages: Cell<u64>,
    color_stage_gpu_blockers: Cell<u64>,
    color_stage_pixels: Cell<u64>,
    color_composite_plans: Cell<u64>,
    color_composite_elements: Cell<u64>,
    color_composite_float_linear: Cell<u64>,
    color_composite_legacy_rgba8: Cell<u64>,
    color_composite_legacy_media_blend_mode: Cell<u64>,
    color_composite_legacy_media_transform: Cell<u64>,
    color_composite_legacy_media_effect: Cell<u64>,
    color_composite_legacy_solid_blend_mode: Cell<u64>,
    color_composite_legacy_solid_transform: Cell<u64>,
    color_composite_legacy_solid_effect: Cell<u64>,
    color_composite_legacy_adjustment_blend_mode: Cell<u64>,
    color_composite_legacy_adjustment_effect: Cell<u64>,
}

fn bump(counter: &Cell<u64>) {
    counter.set(counter.get().saturating_add(1));
}

fn add_cell(counter: &Cell<u64>, delta: u64) {
    counter.set(counter.get().saturating_add(delta));
}

fn bump_value(counter: &mut u64) {
    *counter = counter.saturating_add(1);
}

fn viewer_preview_cache_key_for_resolved_plan(
    sequence_id: SequenceId,
    width: u32,
    height: u32,
    elements: &[ResolvedPreviewElement],
    color_context: &ColorContext,
) -> ViewerPreviewCacheKey {
    let mut hasher = DefaultHasher::new();
    color_context.working_color_space.hash(&mut hasher);
    color_context.output_color_space.hash(&mut hasher);
    color_context.tone_map.hash(&mut hasher);
    color_context.engine.hash(&mut hasher);
    color_context.display_management.hash(&mut hasher);
    color_context.ocio_display.hash(&mut hasher);
    color_context.ocio_view.hash(&mut hasher);
    elements.len().hash(&mut hasher);
    for element in elements {
        match element {
            ResolvedPreviewElement::SolidColor(solid) => {
                0u8.hash(&mut hasher);
                hash_color(solid.color, &mut hasher);
                solid.opacity.to_bits().hash(&mut hasher);
                solid.blend_mode.hash(&mut hasher);
                hash_transform(solid.transform, &mut hasher);
                hash_effect_graph_signature(&solid.effect_graph, solid.frame_seed, &mut hasher);
            }
            ResolvedPreviewElement::Adjustment(adjustment) => {
                1u8.hash(&mut hasher);
                adjustment.opacity.to_bits().hash(&mut hasher);
                adjustment.blend_mode.hash(&mut hasher);
                hash_effect_graph_signature(
                    &adjustment.effect_graph,
                    adjustment.frame_seed,
                    &mut hasher,
                );
            }
            ResolvedPreviewElement::Media {
                frame,
                opacity,
                blend_mode,
                transform,
                effect_graph,
                frame_seed,
            } => {
                2u8.hash(&mut hasher);
                frame.signature.hash(&mut hasher);
                frame.width().hash(&mut hasher);
                frame.height().hash(&mut hasher);
                opacity.to_bits().hash(&mut hasher);
                blend_mode.hash(&mut hasher);
                hash_transform(*transform, &mut hasher);
                hash_effect_graph_signature(effect_graph, *frame_seed, &mut hasher);
            }
        }
    }
    ViewerPreviewCacheKey {
        sequence_id,
        width,
        height,
        plan_signature: hasher.finish(),
    }
}

fn hash_color(color: mondrian_core::Color, hasher: &mut impl Hasher) {
    color.r.to_bits().hash(hasher);
    color.g.to_bits().hash(hasher);
    color.b.to_bits().hash(hasher);
    color.a.to_bits().hash(hasher);
}

fn hash_transform(transform: [f32; 6], hasher: &mut impl Hasher) {
    for value in transform {
        value.to_bits().hash(hasher);
    }
}

fn hash_effect_graph_signature(
    graph: &CompiledEffectGraph,
    frame_seed: i64,
    hasher: &mut impl Hasher,
) {
    graph.signature_hash.hash(hasher);
    graph.output_cache_policy.hash(hasher);
    if graph.output_cache_policy == EffectCachePolicy::FrameDependent {
        frame_seed.hash(hasher);
    }
}

fn media_preview_frame_signature(key: &MediaPreviewKey) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

fn nested_preview_frame_signature(
    sequence_id: SequenceId,
    frame: i64,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> u64 {
    let mut hasher = DefaultHasher::new();
    sequence_id.hash(&mut hasher);
    frame.hash(&mut hasher);
    width.hash(&mut hasher);
    height.hash(&mut hasher);
    rgba.hash(&mut hasher);
    hasher.finish()
}

fn preview_dimensions_for_sequence(sequence: &Sequence) -> (u32, u32) {
    let resolution = sequence.settings.resolution;
    let scale = normalize_preview_resolution_scale(sequence.settings.preview.resolution_scale);
    let width = ((resolution.width as f32 * scale).round() as u32).max(1);
    let height = ((resolution.height as f32 * scale).round() as u32).max(1);
    (width, height)
}

fn preview_display_color_space(
    sequence: &Sequence,
    project_cm: &mondrian_core::ProjectColorManagement,
) -> ColorSpace {
    let sequence_output = sequence.settings.color_management.output_color_space;
    let display_management = if sequence.settings.color_management.inherit {
        &project_cm.display_management
    } else {
        &sequence.settings.color_management.display_management
    };
    let profile_space = display_management
        .monitor_profile
        .managed_color_space(sequence_output)
        .unwrap_or(sequence_output);

    match display_management.viewer_mode.resolve(profile_space) {
        mondrian_core::ResolvedViewerDisplayMode::Sdr => {
            if profile_space.is_hdr() {
                ColorSpace::Rec709
            } else {
                profile_space
            }
        }
        mondrian_core::ResolvedViewerDisplayMode::HdrPq => ColorSpace::Rec2100Pq,
        mondrian_core::ResolvedViewerDisplayMode::HdrHlg => ColorSpace::Rec2100Hlg,
    }
}

struct PreviewCompositeOutput {
    rgba: Vec<u8>,
    composite_diagnostics: TimelineCompositeDiagnostics,
    color_diagnostics: RenderColorTransformDiagnostics,
    color_stage_diagnostics: RenderColorStageDiagnostics,
}

struct PreviewWorkingCompositeOutput {
    frame: CpuColorFrame,
    boundary: RenderOutputColorBoundary,
    composite_diagnostics: TimelineCompositeDiagnostics,
}

fn composite_resolved_preview_working(
    width: u32,
    height: u32,
    resolved: &[ResolvedPreviewElement],
    color_context: &ColorContext,
    scratch: &mut TimelineCompositeScratch,
) -> Result<PreviewWorkingCompositeOutput, String> {
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
                frame: &frame.frame,
                opacity: *opacity,
                blend_mode: *blend_mode,
                transform: *transform,
                effect_graph: Arc::clone(effect_graph),
                frame_seed: *frame_seed,
            }),
        })
        .collect();
    let composite = composite_timeline_elements_color_frame_with_diagnostics(
        width,
        height,
        &elements,
        TimelineCompositeOptions::default(),
        color_context.working_color_space,
        scratch,
    );
    let boundary = RenderOutputColorBoundary::display(
        color_context.output_color_space,
        color_context.tone_map,
        color_context.engine.clone(),
    );
    Ok(PreviewWorkingCompositeOutput {
        frame: composite.frame,
        boundary,
        composite_diagnostics: composite.diagnostics,
    })
}

fn composite_resolved_preview(
    width: u32,
    height: u32,
    resolved: &[ResolvedPreviewElement],
    color_context: &ColorContext,
    scratch: &mut TimelineCompositeScratch,
) -> Result<PreviewCompositeOutput, String> {
    let composite =
        composite_resolved_preview_working(width, height, resolved, color_context, scratch)?;
    execute_cpu_output_boundary_rgba8(&composite.frame, &composite.boundary)
        .map(|output| PreviewCompositeOutput {
            rgba: output.rgba,
            composite_diagnostics: composite.composite_diagnostics,
            color_diagnostics: output.color_diagnostics,
            color_stage_diagnostics: output.stage_diagnostics,
        })
        .map_err(|err| format!("viewer preview final color transform failed: {err}"))
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
    let signature = media_preview_frame_signature(&job.key);
    match mondrian_media::decode_video_frame_at_time_rgba_scaled(
        job.key.path.as_path(),
        job.source_secs,
        Some(job.key.target_width.max(1)),
        Some(job.key.target_height.max(1)),
    ) {
        Ok(frame) => {
            let source = CpuEncodedColorFrame::source_rgba8(
                frame.width,
                frame.height,
                job.key.input_color_space,
                frame.data,
            );
            let working = match execute_cpu_input_stage(
                &source,
                &RenderInputTransform::to_working(
                    job.key.working_color_space,
                    job.key.tone_map,
                    job.key.engine.clone(),
                ),
            ) {
                Ok(frame) => frame,
                Err(err) => {
                    return MediaPreviewResult {
                        key: job.key,
                        frame: None,
                        error: Some(format!(
                            "viewer preview input color transform failed: {err}"
                        )),
                        generation: job.generation,
                        color_diagnostics: None,
                        color_stage_diagnostics: None,
                    };
                }
            };
            let color_diagnostics = Some(working.result.diagnostics);
            let color_stage_diagnostics = Some(working.stage_diagnostics);

            MediaPreviewResult {
                key: job.key,
                frame: Some(MediaPreviewFrame { frame: working.result.frame, signature }),
                error: None,
                generation: job.generation,
                color_diagnostics,
                color_stage_diagnostics,
            }
        }
        Err(err) => MediaPreviewResult {
            key: job.key,
            frame: None,
            error: Some(err.to_string()),
            generation: job.generation,
            color_diagnostics: None,
            color_stage_diagnostics: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use mondrian_core::types::{AssetId, TimeCode};
    use mondrian_core::{Color, ProjectColorManagement};
    use mondrian_effects::{get_or_compile_scheduled_effect_graph, EffectRenderPlan};
    use mondrian_renderer::ColorFrameDomain;
    use mondrian_timeline::clip::Clip;
    use mondrian_timeline::sequence::{MissingColorMetadataPolicy, Sequence};

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

    fn test_color_context(output_color_space: ColorSpace) -> ColorContext {
        Sequence::new("color-context")
            .settings
            .root_preview_color_context(&ProjectColorManagement::default(), output_color_space)
    }

    #[test]
    fn preview_display_color_space_resolves_managed_monitor_profile() {
        let mut sequence = Sequence::new("p3-preview");
        sequence.settings.color_management.inherit = false;
        sequence.settings.color_management.display_management =
            mondrian_core::DisplayManagementPolicy {
                monitor_profile: mondrian_core::MonitorProfileReference::ColorSpace(
                    ColorSpace::DciP3,
                ),
                viewer_mode: mondrian_core::ViewerDisplayMode::Sdr,
                tone_map_policy: mondrian_core::DisplayToneMapPolicy::Automatic,
            };

        assert_eq!(
            preview_display_color_space(&sequence, &ProjectColorManagement::default()),
            ColorSpace::DciP3
        );
    }

    #[test]
    fn preview_display_color_space_resolves_explicit_hdr_viewer_mode() {
        let mut sequence = Sequence::new("hdr-preview");
        sequence.settings.color_management.inherit = false;
        sequence.settings.color_management.display_management =
            mondrian_core::DisplayManagementPolicy {
                monitor_profile: mondrian_core::MonitorProfileReference::ColorSpace(
                    ColorSpace::Rec709,
                ),
                viewer_mode: mondrian_core::ViewerDisplayMode::HdrPq,
                tone_map_policy: mondrian_core::DisplayToneMapPolicy::Automatic,
            };

        assert_eq!(
            preview_display_color_space(&sequence, &ProjectColorManagement::default()),
            ColorSpace::Rec2100Pq
        );
    }

    fn ready_frame(state: ViewerPreviewState) -> ViewerFrameImage {
        match state {
            ViewerPreviewState::Ready(ViewerFrameContent::Raster(frame)) => frame,
            ViewerPreviewState::Ready(other) => {
                panic!("expected raster ready frame, got {other:?}")
            }
            other => panic!("expected ready frame, got {other:?}"),
        }
    }

    #[test]
    fn solid_color_sequence_returns_preview_frame_at_preview_scale() {
        let service = AppUiPreviewService::new();
        let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));

        let frame = service.viewer_preview_for_state(&state);
        let frame = ready_frame(frame);

        assert_eq!(frame.width, 960);
        assert_eq!(frame.height, 540);
        assert_eq!(frame.rgba.len(), 960 * 540 * 4);
        assert!(frame.key.contains("app UI-viewer:960x540:f4:"));
    }

    #[test]
    fn gpu_preview_frame_for_state_returns_working_frame_candidate() {
        let service = AppUiPreviewService::new();
        let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));

        let frame = match service.gpu_preview_frame_for_state(&state) {
            AppUiGpuPreviewFrameState::Ready(frame) => frame,
            AppUiGpuPreviewFrameState::Current => panic!("expected new GPU preview candidate"),
            AppUiGpuPreviewFrameState::Loading => panic!("expected ready GPU preview candidate"),
            AppUiGpuPreviewFrameState::Unavailable => {
                panic!("expected available GPU preview candidate")
            }
        };

        assert_eq!(frame.width, 960);
        assert_eq!(frame.height, 540);
        assert_eq!(frame.working_frame.descriptor().width, 960);
        assert_eq!(frame.working_frame.descriptor().height, 540);
        assert!(frame.external_texture_key().starts_with("app-ui.viewer.gpu:"));
    }

    #[test]
    fn external_gpu_preview_frame_overrides_raster_preview_for_same_plan() {
        let service = AppUiPreviewService::new();
        let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
        let frame = match service.gpu_preview_frame_for_state(&state) {
            AppUiGpuPreviewFrameState::Ready(frame) => frame,
            _ => panic!("expected ready GPU preview candidate"),
        };
        let key = frame.external_texture_key();

        assert!(service.set_external_viewer_frame(&frame, key.clone()));
        match service.gpu_preview_frame_for_state(&state) {
            AppUiGpuPreviewFrameState::Current => {}
            _ => panic!("expected current external GPU preview frame"),
        }
        match service.viewer_preview_for_state(&state) {
            ViewerPreviewState::Ready(ViewerFrameContent::ExternalTexture(frame)) => {
                assert_eq!(frame.key, key);
                assert_eq!(frame.width, 960);
                assert_eq!(frame.height, 540);
            }
            other => panic!("expected external GPU preview frame, got {other:?}"),
        }
    }

    #[test]
    fn preview_diagnostics_count_ready_render_requests() {
        let service = AppUiPreviewService::new();
        let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.render_requests, 0);

        let frame = service.viewer_preview_for_state(&state);
        let _ = ready_frame(frame);

        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.render_requests, 1);
        assert_eq!(diagnostics.ready_frames, 1);
        assert_eq!(diagnostics.loading_frames, 0);
        assert_eq!(diagnostics.stale_frames, 0);
        assert_eq!(diagnostics.unavailable_frames, 0);
        assert_eq!(diagnostics.viewer_frame_cache_hits, 0);
        assert_eq!(diagnostics.viewer_frame_cache_misses, 1);
        assert_eq!(diagnostics.viewer_frame_cache_entries, 1);
        assert_eq!(diagnostics.media_cache_entries, 0);
        assert_eq!(diagnostics.media_failure_entries, 0);
        assert_eq!(diagnostics.color_input_transform_calls, 0);
        assert_eq!(diagnostics.color_input_transform_pixels, 0);
        assert_eq!(diagnostics.color_output_transform_calls, 1);
        assert_eq!(diagnostics.color_output_transform_pixels, 960_u64 * 540);
        assert_eq!(diagnostics.color_rgba8_boundary_calls, 1);
        assert_eq!(diagnostics.color_stage_plans, 1);
        assert_eq!(diagnostics.color_stage_total_stages, 1);
        assert_eq!(diagnostics.color_stage_cpu_input_stages, 0);
        assert_eq!(diagnostics.color_stage_cpu_output_stages, 1);
        assert_eq!(diagnostics.color_stage_gpu_color_stages, 0);
        assert_eq!(diagnostics.color_stage_upload_stages, 0);
        assert_eq!(diagnostics.color_stage_readback_stages, 0);
        assert_eq!(diagnostics.color_stage_gpu_blockers, 0);
        assert_eq!(diagnostics.color_stage_pixels, 960_u64 * 540);
        assert_eq!(diagnostics.color_composite_plans, 1);
        assert_eq!(diagnostics.color_composite_elements, 1);
        assert_eq!(diagnostics.color_composite_float_linear, 1);
        assert_eq!(diagnostics.color_composite_legacy_rgba8, 0);
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
        let frame = service.viewer_preview_for_state(&state);
        let frame = ready_frame(frame);

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

        assert!(matches!(
            service.viewer_preview_for_state(&state),
            ViewerPreviewState::Unavailable
        ));
    }

    #[test]
    fn preview_cache_key_changes_when_pixels_change() {
        let service = AppUiPreviewService::new();
        let first = service.viewer_preview_for_state(&state_with_solid_color_clip(
            Color::from_rgba8(255, 0, 0, 255),
        ));
        let first = ready_frame(first);
        let second = service.viewer_preview_for_state(&state_with_solid_color_clip(
            Color::from_rgba8(0, 0, 255, 255),
        ));
        let second = ready_frame(second);

        assert_ne!(first.key, second.key);
    }

    #[test]
    fn deterministic_solid_preview_is_stable_across_frames() {
        let service = AppUiPreviewService::new();
        let mut state = state_with_solid_color_clip(Color::from_rgba8(255, 128, 0, 255));

        let first = ready_frame(service.viewer_preview_for_state(&state));
        state.seek(5);
        let second = ready_frame(service.viewer_preview_for_state(&state));

        assert_eq!(first.rgba, second.rgba);
        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.render_requests, 2);
        assert_eq!(diagnostics.ready_frames, 2);
        assert!(diagnostics.viewer_frame_cache_entries >= 1);
        assert!(diagnostics.color_output_transform_calls >= 1);
        assert_eq!(
            diagnostics.color_output_transform_pixels,
            diagnostics.color_output_transform_calls * 960_u64 * 540
        );
        assert_eq!(
            diagnostics.color_stage_plans,
            diagnostics.color_output_transform_calls
        );
        assert_eq!(
            diagnostics.color_stage_total_stages,
            diagnostics.color_output_transform_calls
        );
        assert_eq!(
            diagnostics.color_stage_cpu_output_stages,
            diagnostics.color_output_transform_calls
        );
        assert_eq!(
            diagnostics.color_stage_pixels,
            diagnostics.color_output_transform_calls * 960_u64 * 540
        );
        assert_eq!(
            diagnostics.color_composite_plans,
            diagnostics.color_output_transform_calls
        );
        assert_eq!(
            diagnostics.color_composite_float_linear,
            diagnostics.color_composite_plans
        );
        assert_eq!(diagnostics.color_composite_legacy_rgba8, 0);
    }

    #[test]
    fn resolved_media_preview_cache_key_includes_media_frame_signature() {
        let effect_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
            .expect("default effect graph");
        let make_plan = |signature| {
            vec![ResolvedPreviewElement::Media {
                frame: test_media_frame_with_size(0, 2, 2, signature),
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                effect_graph: Arc::clone(&effect_graph),
                frame_seed: 12,
            }]
        };
        let sequence_id = SequenceId::new();

        let color_context = test_color_context(ColorSpace::Rec709);
        let first = viewer_preview_cache_key_for_resolved_plan(
            sequence_id,
            320,
            180,
            &make_plan(100),
            &color_context,
        );
        let second = viewer_preview_cache_key_for_resolved_plan(
            sequence_id,
            320,
            180,
            &make_plan(200),
            &color_context,
        );

        assert_ne!(first, second);
    }

    #[test]
    fn resolved_media_preview_cache_key_includes_color_context() {
        let effect_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
            .expect("default effect graph");
        let resolved = vec![ResolvedPreviewElement::Media {
            frame: test_media_frame_with_size(0, 2, 2, 100),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            effect_graph,
            frame_seed: 12,
        }];
        let sequence_id = SequenceId::new();

        let rec709 = test_color_context(ColorSpace::Rec709);
        let srgb = test_color_context(ColorSpace::Srgb);
        let first =
            viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &rec709);
        let second =
            viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &srgb);

        assert_ne!(first, second);
    }

    #[test]
    fn resolved_media_preview_cache_key_includes_display_management_policy() {
        let effect_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
            .expect("default effect graph");
        let resolved = vec![ResolvedPreviewElement::Media {
            frame: test_media_frame_with_size(0, 2, 2, 100),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            effect_graph,
            frame_seed: 12,
        }];
        let sequence_id = SequenceId::new();

        let mut sdr = test_color_context(ColorSpace::Rec709);
        sdr.display_management = mondrian_core::DisplayManagementPolicy {
            monitor_profile: mondrian_core::MonitorProfileReference::ColorSpace(ColorSpace::Rec709),
            viewer_mode: mondrian_core::ViewerDisplayMode::Sdr,
            tone_map_policy: mondrian_core::DisplayToneMapPolicy::Automatic,
        };
        let mut p3 = sdr.clone();
        p3.display_management = mondrian_core::DisplayManagementPolicy {
            monitor_profile: mondrian_core::MonitorProfileReference::ColorSpace(ColorSpace::DciP3),
            viewer_mode: mondrian_core::ViewerDisplayMode::Sdr,
            tone_map_policy: mondrian_core::DisplayToneMapPolicy::Automatic,
        };
        let first =
            viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &sdr);
        let second =
            viewer_preview_cache_key_for_resolved_plan(sequence_id, 320, 180, &resolved, &p3);

        assert_ne!(first, second);
    }

    #[test]
    fn resolved_media_preview_cache_key_includes_display_view_context() {
        let effect_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
            .expect("default effect graph");
        let resolved = vec![ResolvedPreviewElement::Media {
            frame: test_media_frame_with_size(0, 2, 2, 100),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            effect_graph,
            frame_seed: 12,
        }];
        let sequence_id = SequenceId::new();

        let mut rec709_view = test_color_context(ColorSpace::Rec709);
        rec709_view.ocio_display = Some("sRGB - Display".to_owned());
        rec709_view.ocio_view = Some("ACES 2.0 - SDR 100 nits (Rec.709)".to_owned());
        let mut colorimetric_view = rec709_view.clone();
        colorimetric_view.ocio_view = Some("Video (colorimetric)".to_owned());
        let first = viewer_preview_cache_key_for_resolved_plan(
            sequence_id,
            320,
            180,
            &resolved,
            &rec709_view,
        );
        let second = viewer_preview_cache_key_for_resolved_plan(
            sequence_id,
            320,
            180,
            &resolved,
            &colorimetric_view,
        );

        assert_ne!(first, second);
    }

    #[test]
    fn preview_input_color_resolution_honors_override_metadata_and_missing_policy() {
        let mut color_context = test_color_context(ColorSpace::Rec709);
        color_context.working_color_space = ColorSpace::Rec2020;
        color_context.missing_metadata_policy =
            MissingColorMetadataPolicy::AssumeSequenceWorkingSpace;

        assert_eq!(
            resolve_preview_input_color_space(
                Some(ColorSpace::SLog3),
                Some(ColorSpace::Srgb),
                &color_context,
            ),
            Some(ColorSpace::SLog3)
        );
        assert_eq!(
            resolve_preview_input_color_space(None, Some(ColorSpace::Srgb), &color_context),
            Some(ColorSpace::Srgb)
        );
        assert_eq!(
            resolve_preview_input_color_space(None, None, &color_context),
            Some(ColorSpace::Rec2020)
        );

        color_context.missing_metadata_policy = MissingColorMetadataPolicy::RejectMedia;
        assert_eq!(
            resolve_preview_input_color_space(None, None, &color_context),
            None
        );
    }

    #[test]
    fn preview_single_media_color_output_matches_export_composite_contract() {
        let effect_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
            .expect("default effect graph");
        let frame = test_media_frame_rgba(vec![200, 100, 40, 255], 1, 1, 77);
        let color_context = test_color_context(ColorSpace::Srgb);
        let resolved = vec![ResolvedPreviewElement::Media {
            frame: frame.clone(),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            effect_graph: Arc::clone(&effect_graph),
            frame_seed: 0,
        }];
        let mut preview_scratch = TimelineCompositeScratch::default();
        let preview =
            composite_resolved_preview(1, 1, &resolved, &color_context, &mut preview_scratch)
                .expect("preview color composite");
        assert_eq!(
            preview.color_diagnostics.output.domain,
            ColorFrameDomain::Display
        );
        assert_eq!(preview.composite_diagnostics.float_linear_composites, 0);
        assert_eq!(preview.composite_diagnostics.legacy_rgba8_composites, 1);
        assert_eq!(preview.composite_diagnostics.legacy_media_transform, 1);

        let export_elements = vec![TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &frame.frame,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            effect_graph,
            frame_seed: 0,
        })];
        let mut export_scratch = TimelineCompositeScratch::default();
        let expected_frame = mondrian_renderer::composite_timeline_elements_color_frame(
            1,
            1,
            &export_elements,
            TimelineCompositeOptions::default(),
            color_context.working_color_space,
            &mut export_scratch,
        );
        assert_eq!(
            expected_frame.descriptor().color_space,
            color_context.working_color_space
        );
        let export = mondrian_renderer::execute_cpu_output_boundary(
            &expected_frame,
            &RenderOutputColorBoundary::export(
                color_context.output_color_space,
                color_context.tone_map,
                color_context.engine.clone(),
            ),
        )
        .expect("export color transform");
        assert_eq!(
            export.result.diagnostics.output.domain,
            ColorFrameDomain::Export
        );
        assert_eq!(
            preview.color_stage_diagnostics.cpu_output_stages,
            export.stage_diagnostics.cpu_output_stages
        );
        let expected = export.result.frame.into_rgba();

        assert_eq!(preview.rgba, expected);
    }

    #[test]
    fn preview_multilayer_color_output_matches_export_frame_hash() {
        let effect_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
            .expect("default effect graph");
        let source = CpuEncodedColorFrame::source_rgba8(
            2,
            2,
            ColorSpace::Srgb,
            vec![
                200, 24, 16, 255, 40, 220, 96, 255, 12, 64, 240, 255, 240, 220, 40, 255,
            ],
        );
        let frame = execute_cpu_input_stage(
            &source,
            &RenderInputTransform::to_working(
                ColorSpace::Rec2020,
                false,
                ColorEngine::MondrianSmart,
            ),
        )
        .expect("media input transform")
        .result
        .frame;
        let media = MediaPreviewFrame { frame, signature: 2_020 };
        let solid = TimelineSolidColorLayer {
            color: Color::from_rgba8(32, 180, 220, 255),
            opacity: 0.35,
            blend_mode: BlendMode::Screen,
            transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            effect_graph: Arc::clone(&effect_graph),
            frame_seed: 14,
        };
        let mut color_context = test_color_context(ColorSpace::Srgb);
        color_context.working_color_space = ColorSpace::Rec2020;

        let resolved = vec![
            ResolvedPreviewElement::Media {
                frame: media.clone(),
                opacity: 0.85,
                blend_mode: BlendMode::Multiply,
                transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                effect_graph: Arc::clone(&effect_graph),
                frame_seed: 7,
            },
            ResolvedPreviewElement::SolidColor(solid.clone()),
        ];
        let mut preview_scratch = TimelineCompositeScratch::default();
        let preview =
            composite_resolved_preview(2, 2, &resolved, &color_context, &mut preview_scratch)
                .expect("preview multilayer composite");

        let export_elements = vec![
            TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &media.frame,
                opacity: 0.85,
                blend_mode: BlendMode::Multiply,
                transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                effect_graph,
                frame_seed: 7,
            }),
            TimelineCompositeElement::SolidColor(solid),
        ];
        let mut export_scratch = TimelineCompositeScratch::default();
        let export_working = mondrian_renderer::composite_timeline_elements_color_frame(
            2,
            2,
            &export_elements,
            TimelineCompositeOptions::default(),
            color_context.working_color_space,
            &mut export_scratch,
        );
        let export = mondrian_renderer::execute_cpu_output_boundary(
            &export_working,
            &RenderOutputColorBoundary::export(
                color_context.output_color_space,
                color_context.tone_map,
                color_context.engine.clone(),
            ),
        )
        .expect("export multilayer color transform")
        .result
        .frame
        .into_rgba();

        assert_eq!(preview.rgba, export);
        assert_eq!(stable_rgba_hash(&preview.rgba), stable_rgba_hash(&export));
        assert_eq!(preview.composite_diagnostics.legacy_rgba8_composites, 1);
        assert_eq!(preview.composite_diagnostics.legacy_media_blend_mode, 0);
        assert_eq!(preview.composite_diagnostics.legacy_media_transform, 1);
        assert_eq!(preview.composite_diagnostics.legacy_solid_blend_mode, 0);
        assert_eq!(preview.composite_diagnostics.legacy_solid_transform, 1);
    }

    #[test]
    fn stale_viewer_frame_is_scoped_to_sequence_and_dimensions() {
        let service = AppUiPreviewService::new();
        let state = state_with_solid_color_clip(Color::from_rgba8(24, 80, 160, 255));
        let sequence = state.sequence.as_ref().expect("sequence");
        let (width, height) = preview_dimensions_for_sequence(sequence);

        let ready = ready_frame(service.viewer_preview_for_state(&state));
        let same_scope = service
            .stale_frame_for_sequence(sequence, width, height)
            .expect("same sequence can reuse stale frame");
        assert_eq!(same_scope.key, ready.key);

        let different_sequence = Sequence::new("other");
        assert!(service.stale_frame_for_sequence(&different_sequence, width, height).is_none());
        assert!(service
            .stale_frame_for_sequence(sequence, width.saturating_add(1), height)
            .is_none());
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
            input_color_space: ColorSpace::Rec709,
            working_color_space: ColorSpace::Rec709,
            tone_map: false,
            engine: ColorEngine::MondrianSmart,
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
        assert!(result.color_diagnostics.is_none());
        assert!(result.color_stage_diagnostics.is_none());
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
            input_color_space: ColorSpace::Rec709,
            working_color_space: ColorSpace::Rec709,
            tone_map: false,
            engine: ColorEngine::MondrianSmart,
        }
    }

    fn test_media_frame(seed: u8) -> MediaPreviewFrame {
        test_media_frame_rgba(vec![seed, 0, 0, 255], 1, 1, seed as u64)
    }

    fn test_media_frame_with_size(
        seed: u8,
        width: u32,
        height: u32,
        signature: u64,
    ) -> MediaPreviewFrame {
        test_media_frame_rgba(
            std::iter::repeat_n([seed, 0, 0, 255], width as usize * height as usize)
                .flatten()
                .collect(),
            width,
            height,
            signature,
        )
    }

    fn test_media_frame_rgba(
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        signature: u64,
    ) -> MediaPreviewFrame {
        let source = CpuEncodedColorFrame::source_rgba8(width, height, ColorSpace::Rec709, rgba);
        let frame = execute_cpu_input_stage(
            &source,
            &RenderInputTransform::to_working(
                ColorSpace::Rec709,
                false,
                ColorEngine::MondrianSmart,
            ),
        )
        .expect("test media input transform");
        let frame = frame.result.frame;
        MediaPreviewFrame { frame, signature }
    }

    fn test_media_frame_rgba8(frame: &MediaPreviewFrame) -> Vec<u8> {
        mondrian_renderer::execute_cpu_output_boundary(
            &frame.frame,
            &RenderOutputColorBoundary::display(
                ColorSpace::Rec709,
                false,
                ColorEngine::MondrianSmart,
            ),
        )
        .expect("test media output transform")
        .result
        .frame
        .into_rgba()
    }

    fn stable_rgba_hash(rgba: &[u8]) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in rgba {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
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
        assert_eq!(test_media_frame_rgba8(&frame), vec![9, 0, 0, 255]);
    }

    #[test]
    fn media_preview_failure_cache_evicts_least_recently_used_key() {
        let mut cache = MediaPreviewFailureCache::new(2);
        let first = test_media_key(1);
        let second = test_media_key(2);
        let third = test_media_key(3);

        cache.insert(first.clone());
        cache.insert(second.clone());
        assert!(cache.contains(&first));

        cache.insert(third.clone());

        assert_eq!(cache.len(), 2);
        assert!(cache.contains(&first));
        assert!(!cache.contains(&second));
        assert!(cache.contains(&third));
    }

    #[test]
    fn media_preview_failure_cache_updates_existing_key_without_growing() {
        let mut cache = MediaPreviewFailureCache::new(2);
        let key = test_media_key(1);

        cache.insert(key.clone());
        cache.insert(key.clone());

        assert_eq!(cache.len(), 1);
        assert!(cache.contains(&key));
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
            input_color_space: ColorSpace::Rec709,
            working_color_space: ColorSpace::Rec709,
            tone_map: false,
            engine: ColorEngine::MondrianSmart,
        };
        assert_eq!(
            scheduler.request(key.clone(), first_generation),
            MediaPreviewRequestStatus::Scheduled
        );

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
            input_color_space: ColorSpace::Rec709,
            working_color_space: ColorSpace::Rec709,
            tone_map: false,
            engine: ColorEngine::MondrianSmart,
        };
        assert_eq!(
            scheduler.request(key.clone(), first_generation),
            MediaPreviewRequestStatus::Scheduled
        );

        let second_generation = scheduler.begin_generation();
        assert_eq!(
            scheduler.request(key.clone(), second_generation),
            MediaPreviewRequestStatus::AlreadyPending
        );

        assert!(scheduler.should_decode(&key));
        assert!(scheduler.complete(&key, first_generation));
        assert_eq!(scheduler.pending_len(), 0);
    }

    #[test]
    fn media_preview_scheduler_prunes_obsolete_pending_requests() {
        let scheduler = MediaPreviewScheduler::default();
        let first_generation = scheduler.begin_generation();
        let first = test_media_key(1);
        let second = test_media_key(2);
        assert_eq!(
            scheduler.request(first.clone(), first_generation),
            MediaPreviewRequestStatus::Scheduled
        );
        assert_eq!(
            scheduler.request(second.clone(), first_generation),
            MediaPreviewRequestStatus::Scheduled
        );

        scheduler.begin_generation();
        scheduler.prune_obsolete();

        assert_eq!(scheduler.pending_len(), 0);
        assert!(!scheduler.should_decode(&first));
        assert!(!scheduler.should_decode(&second));
    }

    #[test]
    fn media_preview_scheduler_drops_new_requests_when_pending_window_is_full() {
        let scheduler = MediaPreviewScheduler::with_max_pending(2);
        let generation = scheduler.begin_generation();
        let first = test_media_key(1);
        let second = test_media_key(2);
        let third = test_media_key(3);

        assert_eq!(
            scheduler.request(first, generation),
            MediaPreviewRequestStatus::Scheduled
        );
        assert_eq!(
            scheduler.request(second, generation),
            MediaPreviewRequestStatus::Scheduled
        );
        assert_eq!(
            scheduler.request(third, generation),
            MediaPreviewRequestStatus::DroppedBackpressure
        );

        assert_eq!(scheduler.pending_len(), 2);
    }

    #[test]
    fn media_preview_scheduler_rejects_obsolete_generation_requests() {
        let scheduler = MediaPreviewScheduler::default();
        let first_generation = scheduler.begin_generation();
        scheduler.begin_generation();

        assert_eq!(
            scheduler.request(test_media_key(1), first_generation),
            MediaPreviewRequestStatus::DroppedBackpressure
        );
        assert_eq!(scheduler.pending_len(), 0);
    }

    #[test]
    fn media_preview_scheduler_reports_request_and_drop_diagnostics() {
        let scheduler = MediaPreviewScheduler::with_max_pending(1);
        let generation = scheduler.begin_generation();
        let first = test_media_key(1);
        let second = test_media_key(2);

        assert_eq!(
            scheduler.request(first.clone(), generation),
            MediaPreviewRequestStatus::Scheduled
        );
        assert_eq!(
            scheduler.request(first.clone(), generation),
            MediaPreviewRequestStatus::AlreadyPending
        );
        assert_eq!(
            scheduler.request(second.clone(), generation),
            MediaPreviewRequestStatus::DroppedBackpressure
        );

        scheduler.begin_generation();
        scheduler.prune_obsolete();
        assert!(!scheduler.should_decode(&first));
        assert!(!scheduler.complete(&second, generation));

        let diagnostics = scheduler.diagnostics();
        assert_eq!(diagnostics.latest_generation, generation + 1);
        assert_eq!(diagnostics.pending_requests, 0);
        assert_eq!(diagnostics.scheduled_requests, 1);
        assert_eq!(diagnostics.already_pending_requests, 1);
        assert_eq!(diagnostics.dropped_backpressure_requests, 1);
        assert_eq!(diagnostics.pruned_obsolete_requests, 1);
        assert_eq!(diagnostics.skipped_decode_jobs, 1);
        assert_eq!(diagnostics.completed_current_results, 0);
        assert_eq!(diagnostics.completed_stale_results, 1);
    }
}
