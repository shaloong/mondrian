//! UI-independent final Preview output arbitration.
//!
//! Timeline evaluation and media adaptation produce a resolved plan. This deep
//! Module alone decides whether that plan uses an exact registered GPU output,
//! a raster cache entry, an explicitly scoped stale output, or the CPU output
//! boundary. Window and Headless Adapters only project the returned state.

use super::*;

impl<O: Clone> PreviewProductionRuntime<O> {
    pub(crate) fn presentation(
        &self,
        request: PreviewFrameExecutionRequest<'_>,
    ) -> PreviewPresentationState<O> {
        if self.media_existing_work_retry_pending.replace(false) {
            bump(&self.media_existing_work_retry_acknowledgements);
        }
        let snapshot = request.snapshot();
        let proxy_demands = request.proxy_demands();
        let transport = snapshot.transport();
        let running_without_demand = transport.is_playing() && transport.demand().is_none();
        bump(&self.metrics.render_requests);
        self.synchronize_visual_program_authoring_session(snapshot);
        self.synchronize_transport_intent(transport.intent());
        self.execution.borrow_mut().set_pending(false);
        self.last_color_rejection.replace(None);
        let Some(authoring) = snapshot.authoring() else {
            self.invalidate_preview_generation();
            self.scheduler.prune_obsolete();
            return self.observe_preview_state(PreviewPresentationState::Unavailable(
                PreviewUnavailability::no_content(
                    PreviewOutputStage::Project,
                    "no active Sequence",
                ),
            ));
        };
        let Some(sequence) = authoring.active_sequence() else {
            self.invalidate_preview_generation();
            self.scheduler.prune_obsolete();
            return self.observe_preview_state(PreviewPresentationState::Unavailable(
                PreviewUnavailability::no_content(
                    PreviewOutputStage::Project,
                    "no active Sequence",
                ),
            ));
        };
        let frame = transport.current_frame().max(0);
        let (width, height) = preview_dimensions_for_snapshot(snapshot, sequence);
        let display_snapshot = self.display_snapshot.borrow();
        let display_color_space = match preview_display_color_space(
            sequence,
            snapshot.viewer_display(),
            display_snapshot.as_ref(),
        ) {
            Ok(color_space) => color_space,
            Err(blocker) => {
                self.record_preview_gpu_output_blocker(&blocker);
                self.scheduler.prune_obsolete();
                return self.observe_preview_state(PreviewPresentationState::Unavailable(
                    PreviewUnavailability::blocked(
                        PreviewOutputStage::DisplayContract,
                        blocker.description(),
                    ),
                ));
            }
        };
        let color_context =
            sequence.settings.root_program_color_context(authoring.color_environment());
        self.activate_preview_generation(ViewerPreviewGenerationKey::from_snapshot(
            snapshot,
            sequence,
            frame,
            width,
            height,
            display_color_space,
            display_snapshot.as_ref().map(DisplayOutputSnapshot::contract_identity),
        ));
        let render_started_at = Instant::now();
        let resolve_started_at = Instant::now();
        let evaluation_key = FrameEvaluationKey {
            sequence_id: sequence.id,
            sequence_revision: sequence.revision,
            author_generation: snapshot
                .authoring()
                .map(PreviewAuthoringSnapshot::author_generation)
                .unwrap_or(0),
            frame,
            width,
            height,
            runtime_scale: transport.runtime_scale(),
            display_color_space,
            display_contract_identity: display_snapshot
                .as_ref()
                .map(DisplayOutputSnapshot::contract_identity),
        };
        let resolved = self.acquire_frame_evaluation(
            snapshot,
            proxy_demands,
            sequence,
            frame,
            width,
            height,
            color_context,
            evaluation_key,
        );
        let mut render_stage_durations = PreviewRenderStageDurations {
            resolve_us: app_duration_us(resolve_started_at.elapsed()),
            ..PreviewRenderStageDurations::default()
        };
        let preview_state = match resolved {
            FrameResolutionOutcome::Ready(evaluation) => {
                let output_key =
                    if matches!(evaluation.reuse_policy, EvaluationReusePolicy::Reusable) {
                        evaluation.output_key.clone()
                    } else {
                        // An Uncacheable graph may keep one in-progress attempt
                        // stable across UI polls, but it must never rebound work
                        // across Preview generations merely because its semantic
                        // graph fingerprint is unchanged.
                        let execution_nonce = self.execution.borrow_mut().issue_candidate_id();
                        evaluation.output_key.with_execution_nonce(execution_nonce)
                    };
                self.execution.borrow_mut().set_presentation_quality(
                    resolved_preview_presentation_quality(&evaluation.elements),
                );
                let final_cache_lookup_started_at = Instant::now();
                // GPU outputs include monitor adaptation; raster cache identity
                // remains display-independent because CPU packaging already
                // records its concrete presentation color space.
                let external_cache_key =
                    matches!(evaluation.reuse_policy, EvaluationReusePolicy::Reusable)
                        .then(|| {
                            evaluation.color_context.output_color_space.color().and_then(
                                |program_output_color_space| {
                                    RenderMonitorAdaptation::new(
                                        program_output_color_space,
                                        display_color_space,
                                        evaluation.color_context.engine.clone(),
                                    )
                                    .ok()
                                    .map(|adaptation| {
                                        output_key.with_monitor_adaptation(&adaptation)
                                    })
                                },
                            )
                        })
                        .flatten();
                if let Some(frame) = external_cache_key
                    .as_ref()
                    .and_then(|cache_key| self.registered_gpu_output_for_key(cache_key))
                {
                    self.try_release_settled_transport_media_residency();
                    render_stage_durations.final_cache_lookup_us =
                        app_duration_us(final_cache_lookup_started_at.elapsed());
                    self.record_render_stage_durations(
                        app_duration_us(render_started_at.elapsed()),
                        render_stage_durations,
                    );
                    PreviewPresentationState::Ready(PreviewPresentationCandidate::new(
                        PreviewPresentationContent::Gpu(frame),
                        self.playback_presentation_ticket(snapshot),
                    ))
                } else if let Some(frame) =
                    matches!(evaluation.reuse_policy, EvaluationReusePolicy::Reusable)
                        .then(|| self.cached_viewer_frame(&output_key))
                        .flatten()
                {
                    render_stage_durations.final_cache_lookup_us =
                        app_duration_us(final_cache_lookup_started_at.elapsed());
                    self.record_render_stage_durations(
                        app_duration_us(render_started_at.elapsed()),
                        render_stage_durations,
                    );
                    if running_without_demand {
                        self.stale_viewer_content_for_sequence(sequence, width, height)
                            .map(PreviewPresentationState::Stale)
                            .unwrap_or(PreviewPresentationState::Loading)
                    } else {
                        self.frame_store.borrow_mut().pin_viewer_frame(ScopedPreviewRasterFrame {
                            sequence_id: sequence.id,
                            width,
                            height,
                            frame: frame.clone(),
                        });
                        PreviewPresentationState::Ready(PreviewPresentationCandidate::new(
                            PreviewPresentationContent::Raster(frame),
                            self.playback_presentation_ticket(snapshot),
                        ))
                    }
                } else if transport.is_playing() {
                    // Playback presentation is a read/projection seam on the
                    // UI thread. A cache miss must be executed by the GPU
                    // candidate path (or a future bounded fallback worker),
                    // never by an inline full-frame CPU composite. This rule
                    // applies equally to media and generated-only plans:
                    // source kind cannot become UI scheduling authority.
                    render_stage_durations.final_cache_lookup_us =
                        app_duration_us(final_cache_lookup_started_at.elapsed());
                    self.record_render_stage_durations(
                        app_duration_us(render_started_at.elapsed()),
                        render_stage_durations,
                    );
                    self.stale_viewer_content_for_sequence(sequence, width, height)
                        .map(PreviewPresentationState::Stale)
                        .unwrap_or(PreviewPresentationState::Loading)
                } else {
                    render_stage_durations.final_cache_lookup_us =
                        app_duration_us(final_cache_lookup_started_at.elapsed());
                    let raster_contract =
                        match preview_raster_presentation_contract(&evaluation.color_context) {
                            Ok(contract) => contract,
                            Err(error) => {
                                return self.observe_preview_state(
                                    PreviewPresentationState::Unavailable(error.unavailability()),
                                );
                            }
                        };
                    let output = match composite_resolved_preview(
                        width,
                        height,
                        &evaluation.elements,
                        &evaluation.color_context,
                        &mut self.scratch.borrow_mut(),
                    ) {
                        Ok(rgba) => rgba,
                        Err(err) => {
                            let reason = err.unavailability();
                            tracing::warn!(
                                code = reason.code(),
                                detail = reason.detail(),
                                "viewer Preview CPU execution failed"
                            );
                            return self.observe_preview_state(
                                PreviewPresentationState::Unavailable(reason),
                            );
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
                    let key = preview_raster_resource_key(&output_key);
                    match PreviewRasterFrame::new(
                        key,
                        width,
                        height,
                        raster_contract.color_space,
                        output.rgba,
                    ) {
                        Ok(frame) => {
                            if matches!(evaluation.reuse_policy, EvaluationReusePolicy::Reusable) {
                                self.frame_store
                                    .borrow_mut()
                                    .insert_viewer_frame(output_key.clone(), frame.clone());
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
                            PreviewPresentationState::Ready(PreviewPresentationCandidate::new(
                                PreviewPresentationContent::Raster(frame),
                                self.playback_presentation_ticket(snapshot),
                            ))
                        }
                        Err(error) => {
                            PreviewPresentationState::Unavailable(PreviewUnavailability::failed(
                                PreviewOutputStage::FramePackaging,
                                error.to_string(),
                            ))
                        }
                    }
                }
            }
            FrameResolutionOutcome::Pending(_) => self
                .stale_viewer_content_for_sequence(sequence, width, height)
                .map(PreviewPresentationState::Stale)
                .unwrap_or(PreviewPresentationState::Loading),
            FrameResolutionOutcome::Empty => {
                self.execution
                    .borrow_mut()
                    .set_presentation_quality(mondrian_playback::FramePresentationQuality::Ready);
                if running_without_demand {
                    self.stale_viewer_content_for_sequence(sequence, width, height)
                        .map(PreviewPresentationState::Stale)
                        .unwrap_or(PreviewPresentationState::Loading)
                } else {
                    PreviewPresentationState::Transparent(PreviewPresentationCandidate::new(
                        (),
                        self.playback_presentation_ticket(snapshot),
                    ))
                }
            }
            FrameResolutionOutcome::Unavailable(reason) => {
                PreviewPresentationState::Unavailable(reason)
            }
        };
        self.schedule_media_prefetches(snapshot, proxy_demands, sequence, frame, width, height);
        self.scheduler.prune_obsolete();
        if matches!(&preview_state, PreviewPresentationState::Loading) {
            self.publish_existing_work_retry_if_actionable();
        }
        self.observe_preview_state(preview_state)
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
    ) -> Option<PreviewPresentationContent<O>> {
        self.execution
            .borrow()
            .current_output()
            .filter(|(key, _)| key.sequence_id == sequence.id)
            .map(|(_, output)| PreviewPresentationContent::Gpu(output.clone()))
            .or_else(|| {
                self.stale_frame_for_sequence(sequence, width, height)
                    .map(PreviewPresentationContent::Raster)
            })
    }

    fn observe_preview_state(
        &self,
        state: PreviewPresentationState<O>,
    ) -> PreviewPresentationState<O> {
        if matches!(
            state,
            PreviewPresentationState::Transparent(_) | PreviewPresentationState::Unavailable(_)
        ) {
            self.clear_terminal_viewer_state();
        }
        self.record_preview_state(&state);
        state
    }

    fn record_preview_state(&self, state: &PreviewPresentationState<O>) {
        match state {
            PreviewPresentationState::Ready(_) => bump(&self.metrics.ready_frames),
            PreviewPresentationState::Transparent(_) => bump(&self.metrics.ready_frames),
            PreviewPresentationState::Loading => bump(&self.metrics.loading_frames),
            PreviewPresentationState::Stale(_) => bump(&self.metrics.stale_frames),
            PreviewPresentationState::Unavailable(reason) => {
                bump(&self.metrics.unavailable_frames);
                self.unavailability_evidence.borrow_mut().observe(reason);
            }
        }
    }
}
