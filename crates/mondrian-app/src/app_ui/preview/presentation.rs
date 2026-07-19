//! Final Viewer output arbitration for the Window Preview Adapter.
//!
//! Timeline evaluation and media adaptation produce a resolved plan. This deep
//! Module alone decides whether that plan uses an exact registered GPU output,
//! a raster cache entry, an explicitly scoped stale output, or the CPU output
//! boundary. The parent Adapter retains scheduling and diagnostics ownership.

use super::*;

impl AppUiPreviewService {
    pub(super) fn render_preview(&self, state: &AppState) -> ViewerPreviewState {
        bump(&self.metrics.render_requests);
        self.execution.borrow_mut().set_pending(false);
        self.last_color_rejection.replace(None);
        let Some(sequence) = state.sequence.as_ref() else {
            self.invalidate_preview_generation();
            self.scheduler.prune_obsolete();
            self.frame_store.borrow_mut().clear_pinned_viewer_frame();
            bump(&self.metrics.unavailable_frames);
            return ViewerPreviewState::Unavailable;
        };
        let frame = state.current_frame().max(0);
        let (width, height) = preview_dimensions_for_state(state, sequence);
        let display_snapshot = self.display_snapshot.borrow();
        let display_color_space = match preview_display_color_space(
            sequence,
            &state.project_settings.color_management,
            display_snapshot.as_ref(),
        ) {
            Ok(color_space) => color_space,
            Err(blocker) => {
                self.record_preview_gpu_output_blocker(&blocker);
                self.scheduler.prune_obsolete();
                self.frame_store.borrow_mut().clear_pinned_viewer_frame();
                bump(&self.metrics.unavailable_frames);
                return ViewerPreviewState::Unavailable;
            }
        };
        let color_context = sequence
            .settings
            .root_program_color_context(&state.project_settings.color_management);
        self.activate_preview_generation(ViewerPreviewGenerationKey::from_state(
            state,
            sequence,
            frame,
            width,
            height,
            display_color_space,
        ));
        let render_started_at = Instant::now();
        let resolve_started_at = Instant::now();
        let resolved =
            self.resolve_sequence_elements(state, sequence, frame, width, height, 0, color_context);
        let mut render_stage_durations = AppUiPreviewRenderStageDurations {
            resolve_us: app_duration_us(resolve_started_at.elapsed()),
            ..AppUiPreviewRenderStageDurations::default()
        };
        let preview_state = match resolved {
            Some(resolved) => {
                self.execution.borrow_mut().set_presentation_quality(
                    resolved_preview_presentation_quality(&resolved.elements),
                );
                let final_cache_lookup_started_at = Instant::now();
                // GPU outputs include monitor adaptation; raster cache identity
                // remains display-independent because CPU packaging already
                // records its concrete presentation color space.
                let external_cache_key = resolved.cache_key.as_ref().and_then(|cache_key| {
                    let program_output_color_space =
                        resolved.color_context.output_color_space.color()?;
                    RenderMonitorAdaptation::new(
                        program_output_color_space,
                        display_color_space,
                        resolved.color_context.engine.clone(),
                    )
                    .ok()
                    .map(|adaptation| cache_key.with_monitor_adaptation(&adaptation))
                });
                if let Some(frame) = external_cache_key
                    .as_ref()
                    .and_then(|cache_key| self.external_viewer_frame_for_key(cache_key))
                {
                    render_stage_durations.final_cache_lookup_us =
                        app_duration_us(final_cache_lookup_started_at.elapsed());
                    self.record_render_stage_durations(
                        app_duration_us(render_started_at.elapsed()),
                        render_stage_durations,
                    );
                    ViewerPreviewState::Ready(ViewerFrameContent::ExternalTexture(frame))
                } else if let Some(frame) = resolved
                    .cache_key
                    .as_ref()
                    .and_then(|cache_key| self.cached_viewer_frame(cache_key))
                {
                    render_stage_durations.final_cache_lookup_us =
                        app_duration_us(final_cache_lookup_started_at.elapsed());
                    self.record_render_stage_durations(
                        app_duration_us(render_started_at.elapsed()),
                        render_stage_durations,
                    );
                    self.frame_store.borrow_mut().pin_viewer_frame(ScopedPreviewRasterFrame {
                        sequence_id: sequence.id,
                        width,
                        height,
                        frame: frame.clone(),
                    });
                    viewer_frame_image(&frame)
                        .map(|frame| ViewerPreviewState::Ready(ViewerFrameContent::Raster(frame)))
                        .unwrap_or(ViewerPreviewState::Unavailable)
                } else if state.is_playing()
                    && preview_elements_require_deferred_composite(&resolved.elements)
                {
                    render_stage_durations.final_cache_lookup_us =
                        app_duration_us(final_cache_lookup_started_at.elapsed());
                    self.record_render_stage_durations(
                        app_duration_us(render_started_at.elapsed()),
                        render_stage_durations,
                    );
                    self.stale_viewer_content_for_sequence(sequence, width, height)
                        .map(ViewerPreviewState::Stale)
                        .unwrap_or(ViewerPreviewState::Loading)
                } else {
                    render_stage_durations.final_cache_lookup_us =
                        app_duration_us(final_cache_lookup_started_at.elapsed());
                    let raster_contract =
                        match preview_raster_presentation_contract(&resolved.color_context) {
                            Ok(contract) => contract,
                            Err(output_color_space) => {
                                tracing::warn!(
                                    output_color_space = ?output_color_space,
                                    "CPU raster viewer has no compatible presentation contract"
                                );
                                return ViewerPreviewState::Unavailable;
                            }
                        };
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
                    render_stage_durations.accumulate_cpu_execution(output.execution_durations);
                    self.record_cpu_execution_evidence(&output);
                    self.record_composite(output.composite_diagnostics);
                    self.record_color_transform(output.color_diagnostics);
                    if let Some(diagnostics) = output.monitor_color_diagnostics {
                        self.record_color_transform(diagnostics);
                    }
                    self.record_color_stage(output.color_stage_diagnostics);
                    let frame_packaging_started_at = Instant::now();
                    let key = resolved
                        .cache_key
                        .as_ref()
                        .map(preview_raster_resource_key)
                        .unwrap_or_else(|| {
                            uncached_preview_raster_resource_key(sequence.id, frame, width, height)
                        });
                    match PreviewRasterFrame::new(
                        key,
                        width,
                        height,
                        raster_contract.color_space,
                        output.rgba,
                    ) {
                        Some(frame) => {
                            if let Some(cache_key) = resolved.cache_key {
                                self.frame_store
                                    .borrow_mut()
                                    .insert_viewer_frame(cache_key, frame.clone());
                            }
                            render_stage_durations.frame_packaging_us =
                                app_duration_us(frame_packaging_started_at.elapsed());
                            self.record_render_stage_durations(
                                app_duration_us(render_started_at.elapsed()),
                                render_stage_durations,
                            );
                            self.frame_store.borrow_mut().pin_viewer_frame(
                                ScopedPreviewRasterFrame {
                                    sequence_id: sequence.id,
                                    width,
                                    height,
                                    frame: frame.clone(),
                                },
                            );
                            viewer_frame_image(&frame)
                                .map(|frame| {
                                    ViewerPreviewState::Ready(ViewerFrameContent::Raster(frame))
                                })
                                .unwrap_or(ViewerPreviewState::Unavailable)
                        }
                        None => ViewerPreviewState::Unavailable,
                    }
                }
            }
            None if self.execution.borrow().is_pending() => self
                .stale_viewer_content_for_sequence(sequence, width, height)
                .map(ViewerPreviewState::Stale)
                .unwrap_or(ViewerPreviewState::Loading),
            None => ViewerPreviewState::Unavailable,
        };
        self.schedule_media_prefetches(state, sequence, frame, width, height);
        self.scheduler.prune_obsolete();
        self.record_preview_state(&preview_state);
        preview_state
    }

    fn cached_viewer_frame(&self, key: &ViewerPreviewCacheKey) -> Option<PreviewRasterFrame> {
        let frame = self.frame_store.borrow_mut().viewer_frame(key);
        if frame.is_some() {
            bump(&self.metrics.viewer_frame_cache_hits);
        } else {
            bump(&self.metrics.viewer_frame_cache_misses);
        }
        frame
    }

    pub(super) fn stale_viewer_content_for_sequence(
        &self,
        sequence: &Sequence,
        width: u32,
        height: u32,
    ) -> Option<ViewerFrameContent> {
        self.execution
            .borrow()
            .current_output()
            .filter(|(key, _)| key.sequence_id == sequence.id)
            .map(|(_, output)| ViewerFrameContent::ExternalTexture(output.clone()))
            .or_else(|| {
                self.stale_frame_for_sequence(sequence, width, height)
                    .as_ref()
                    .and_then(viewer_frame_image)
                    .map(ViewerFrameContent::Raster)
            })
    }

    fn record_preview_state(&self, state: &ViewerPreviewState) {
        match state {
            ViewerPreviewState::Ready(_) => bump(&self.metrics.ready_frames),
            ViewerPreviewState::Loading => bump(&self.metrics.loading_frames),
            ViewerPreviewState::Stale(_) => bump(&self.metrics.stale_frames),
            ViewerPreviewState::Unavailable => bump(&self.metrics.unavailable_frames),
        }
    }
}

/// Final Window Adapter conversion. Cached and pinned Preview state never
/// retains Widget payloads; only an about-to-be-presented raster crosses this
/// boundary.
fn viewer_frame_image(frame: &PreviewRasterFrame) -> Option<ViewerFrameImage> {
    let color_space = match frame.color_space {
        PreviewRasterColorSpace::Srgb => mondrian_ui_core::RasterImageColorSpace::Srgb,
    };
    ViewerFrameImage::new(
        frame.resource_key.clone(),
        frame.width,
        frame.height,
        color_space,
        Arc::clone(&frame.rgba),
    )
}
