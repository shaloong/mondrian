//! Published temporal Filter through the ordinary Timeline, worker, and Export paths.

use super::*;
use crate::app::product_action::*;
use mondrian_core::automation::{
    AnimationChannelValue, KeyframeInterpolation, ParameterValueWrite,
};
use mondrian_core::{FramePosition, PropertyValue, Rational, TimelineTime};

#[test]
fn cpu_deadline_terminal_keeps_original_demand_and_never_poisons_frame_failure_memory() {
    use crate::app::preview_cpu_fallback_task::{
        PreviewCpuFallbackAttempt, PreviewCpuFallbackPending, PreviewCpuFallbackStopped,
        PreviewCpuOutputTarget,
    };
    let _guard = crate::app_ui::test_utils::theme_test_guard();
    let mut state = state_with_solid_color_clip(Color::from_hex(0x244C7A));
    state.play().unwrap();
    let identity = state.pending_playback_frame_demand_identity().unwrap();
    let runtime = PreviewProductionRuntime::<()>::new_without_workers_for_test();
    runtime.synchronize_transport_intent(state.preview_transport_intent());
    let generation = runtime.execution.borrow().generation();
    let cancellation = runtime.execution.borrow().generation_cancellation();
    let key = PreviewOutputKey::new(
        state.active_sequence().unwrap().id,
        1,
        1,
        test_preview_semantic_identity(7782),
    );
    let attempt = PreviewCpuFallbackAttempt {
        generation,
        epoch: identity.epoch,
        semantic_key: key.clone(),
        output_key: key.clone(),
        intent: state.preview_execution_snapshot(Instant::now()).transport().playback_intent(),
        target: PreviewCpuOutputTarget::WorkingGpu,
        candidate_id: Some(7),
        reusable: false,
    };
    let stopped = || PreviewCpuFallbackStopped {
        attempt: Some(attempt.clone()),
        generation,
        epoch: identity.epoch,
        output_key: key.clone(),
        demand_identity: Some(identity),
        reason: mondrian_effects::EffectProcessorStopReason::DeadlineExpired,
    };
    runtime.cpu_fallback_in_flight.replace(Some(PreviewCpuFallbackPending {
        attempt: attempt.clone(),
        completed: false,
        completed_resource: None,
    }));
    let outcome = runtime.observe_cpu_fallback_stopped(stopped());
    assert_eq!(
        outcome.frame_delivery_candidates,
        vec![mondrian_playback::FrameDeliveryCandidate::for_demand(
            identity,
            mondrian_playback::FrameDeliveryKind::Late,
        ),]
    );
    assert!(runtime.cpu_fallback_in_flight.borrow().is_none());
    assert!(runtime.cpu_fallback_failure.borrow().is_none());
    assert!(
        !cancellation.is_canceled(),
        "child expiry cannot cancel its generation"
    );
    assert_eq!(
        state.pending_playback_frame_demand_identity(),
        Some(identity),
        "the adapter reports a candidate; only Playback may consume it"
    );

    let mut successor = attempt.clone();
    successor.output_key = key.with_execution_nonce(8);
    runtime.cpu_fallback_in_flight.replace(Some(PreviewCpuFallbackPending {
        attempt: successor.clone(),
        completed: false,
        completed_resource: None,
    }));
    let outcome = runtime.observe_cpu_fallback_stopped(stopped());
    assert!(outcome.frame_delivery_candidates.is_empty());
    assert_eq!(
        runtime.cpu_fallback_in_flight.borrow().as_ref().unwrap().attempt,
        successor,
        "an obsolete completion cannot retire the newer physical attempt"
    );
    assert!(runtime.cpu_fallback_failure.borrow().is_none());
}

#[test]
fn complete_cpu_route_remains_authoritative_for_gpu_capable_source_pixels() {
    use crate::app::preview_viewer_plan::gpu_composite_layers_for_resolved_with_session;
    use mondrian_renderer::GpuCompositingBlockerReason;
    let _guard = crate::app_ui::test_utils::theme_test_guard();
    ensure_test_ocio_loaded();
    let mut elements = [ResolvedPreviewElement::Media {
        frame: test_media_frame(80),
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_graph: mondrian_effects::identity_compiled_effect_graph().unwrap(),
        prepared_effect_route: None,
        frame_seed: 0,
    }];
    let mut scratch = TimelineCompositeScratch::default();
    assert!(
        gpu_composite_layers_for_resolved_with_session(
            &elements,
            WorkingColorSpace::LinearRec709,
            &mut scratch,
        )
        .is_ok(),
        "control: the source and graph themselves support GPU lowering"
    );
    let ResolvedPreviewElement::Media { prepared_effect_route, .. } = &mut elements[0] else {
        unreachable!()
    };
    *prepared_effect_route = Some(Box::new(
        mondrian_renderer::PreparedTimelinePreviewEffectRoute::Cpu { processor_input: None },
    ));
    assert_eq!(
        gpu_composite_layers_for_resolved_with_session(
            &elements,
            WorkingColorSpace::LinearRec709,
            &mut scratch,
        )
        .err()
        .expect("the preselected complete CPU route must reach its existing compositor"),
        GpuCompositingBlockerReason::EffectRequiresCpu
    );
    let [ResolvedPreviewElement::Media { frame, effect_graph, prepared_effect_route, .. }] =
        elements
    else {
        unreachable!()
    };
    let transition = [ResolvedPreviewElement::CrossDissolve {
        left: Box::new(
            crate::app::preview_viewer_plan::ResolvedPreviewTransitionInput::Media {
                frame,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph,
                prepared_effect_route,
                frame_seed: 0,
            },
        ),
        right: Box::new(
            crate::app::preview_viewer_plan::ResolvedPreviewTransitionInput::Transparent,
        ),
        progress: 0.5,
    }];
    assert_eq!(
        gpu_composite_layers_for_resolved_with_session(
            &transition,
            WorkingColorSpace::LinearRec709,
            &mut scratch,
        )
        .err()
        .expect("transition media must honor the same preselected route"),
        GpuCompositingBlockerReason::EffectRequiresCpu,
    );
}

fn schedule_working(
    runtime: &PreviewProductionRuntime<()>,
    state: &mut AppState,
    frame: i64,
) -> Arc<ResolvedFrameEvaluation> {
    state.seek(frame).unwrap();
    runtime.synchronize_transport_intent(state.preview_transport_intent());
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let sequence = state.active_sequence().unwrap();
    let target = sequence.settings.resolution;
    let color = sequence
        .settings
        .root_program_color_context(state.project_color_environment())
        .unwrap();
    let key = FrameEvaluationKey {
        sequence_id: sequence.id,
        sequence_revision: sequence.revision,
        author_generation: snapshot.authoring().unwrap().author_generation(),
        frame,
        width: target.width,
        height: target.height,
        runtime_scale: snapshot.transport().runtime_scale(),
        raster_requirement: PreviewRasterRequirement::Viewer,
        display_color_space: ColorSpace::Srgb,
        display_contract_identity: None,
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let evaluation = loop {
        runtime.poll_playback_work(None, snapshot.transport().intent());
        match runtime.acquire_frame_evaluation(
            &snapshot,
            state,
            sequence,
            frame,
            target.width,
            target.height,
            color.clone(),
            key,
        ) {
            FrameResolutionOutcome::Ready(ready) => break ready,
            FrameResolutionOutcome::Pending(_) => {}
            FrameResolutionOutcome::Unavailable(reason) => {
                panic!("actual temporal Preview frame {frame}: {}", reason.detail())
            }
            FrameResolutionOutcome::Empty => panic!("actual media Timeline is not empty"),
        }
        assert!(Instant::now() < deadline, "temporal Preview did not settle");
        std::thread::sleep(Duration::from_millis(1));
    };
    // Ready means the immutable Timeline plan is ready. Ordinary current-frame
    // Custom processors still run in the existing CPU composition task, which
    // owns the late-source client. The synchronous no-input test helper cannot
    // execute a temporal native Filter.
    assert_eq!(evaluation.reuse_policy, EvaluationReusePolicy::Transient);
    let resolved = ResolvedPlanView {
        sampling: evaluation.sampling,
        cpu_materialization_active_bytes: evaluation.cpu_materialization_active_bytes,
        elements: Arc::clone(&evaluation.elements),
        cache_key: evaluation.output_key.clone(),
        cpu_cache_key: evaluation.output_key.clone(),
        cache_reusable: false,
        color_context: evaluation.color_context.clone(),
        render_cache_identity: evaluation.render_cache_identity,
    };
    assert!(matches!(
        runtime.schedule_cpu_fallback(
            &snapshot,
            &resolved,
            target.width,
            target.height,
            false,
            crate::app::preview_cpu_fallback_task::PreviewCpuOutputTarget::WorkingGpu,
        ),
        PreviewGpuFrameState::Loading
    ));
    evaluation
}

fn acquire_working(
    runtime: &PreviewProductionRuntime<()>,
    state: &mut AppState,
    frame: i64,
) -> (
    mondrian_renderer::CpuColorFrame,
    mondrian_effects::EffectProcessorFeedbackBatch,
) {
    let _evaluation = schedule_working(runtime, state, frame);
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut feedback = None;
    loop {
        if let Some(update) = runtime
            .poll_playback_work(None, snapshot.transport().intent())
            .processor_feedback
        {
            feedback = Some(update.feedback);
        }
        if let Some((_, failure)) = runtime.cpu_fallback_failure.borrow().as_ref() {
            panic!("actual CPU Preview frame {frame}: {}", failure.detail);
        }
        if let Some(pending) = runtime.cpu_fallback_in_flight.borrow().as_ref()
            && pending.completed
        {
            let working = runtime
                .frame_store
                .borrow_mut()
                .viewer_working_frame(&pending.attempt.output_key)
                .expect("original CPU task published its admitted working frame");
            return (
                (*working).clone(),
                feedback.expect("CPU result delivered native feedback through the production poll"),
            );
        }
        assert!(
            Instant::now() < deadline,
            "actual CPU Preview did not settle"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn export_snapshot(state: &AppState) -> mondrian_export::preset::TimelineExportSnapshot {
    crate::app::exporting::capture_timeline_export_snapshot(
        state,
        state.active_sequence().unwrap().clone(),
        vec![],
        mondrian_export::preset::TimelineExportRange::WorkArea {
            start_frame: 0,
            end_frame_exclusive: 8,
        },
        false,
    )
    .unwrap()
}

fn reference_source_samples(
    state: &AppState,
    varying_media: bool,
) -> std::collections::BTreeMap<i64, mondrian_renderer::CpuColorFrame> {
    let ticks = [6, 7, 8, 9, 14, 15, 16, 17];
    let target = state.active_sequence().unwrap().settings.resolution;
    if !varying_media {
        let base = mondrian_export::queue::export_visual_frame_validation(
            &export_snapshot(state),
            2,
            target,
        )
        .unwrap()
        .working_frame;
        assert_eq!(base.rgba_f32().data.len(), 256);
        assert!(base.rgba_f32().data.iter().all(|p| p[0] > 0.01 && p[3] == 1.0));
        return ticks.into_iter().map(|tick| (tick, base.clone())).collect();
    }
    // This source-only reference uses a finer evaluation grid to express exact
    // quarter-frame author times. The real 24fps program is unchanged. Original
    // media metadata/Source Sample Target/decoder and color conversion still
    // decide the physical samples; the test introduces no rounding policy.
    let mut reference = state.active_sequence().unwrap().clone();
    reference.settings.frame_rate = Rational::new(96, 1);
    let snapshot = crate::app::exporting::capture_timeline_export_snapshot(
        state,
        reference,
        vec![],
        mondrian_export::preset::TimelineExportRange::WorkArea {
            start_frame: 0,
            end_frame_exclusive: 32,
        },
        false,
    )
    .unwrap();
    let sources: std::collections::BTreeMap<_, _> = ticks
        .into_iter()
        .map(|tick| {
            let frame =
                mondrian_export::queue::export_visual_frame_validation(&snapshot, tick, target)
                    .unwrap()
                    .working_frame;
            assert_eq!(frame.rgba_f32().data.len(), 256);
            assert!(frame
                .rgba_f32()
                .data
                .iter()
                .all(|p| p[3] == 1.0 && p.iter().all(|v| v.is_finite())));
            (tick, frame)
        })
        .collect();
    // Millisecond Matroska PTS: frame 4 starts at .167, after 16/96.
    // Keep the preceding picture until that actual boundary; 17/96 crosses it.
    for (left, right) in [(6, 7), (8, 9), (14, 15), (15, 16)] {
        assert!(
            sources[&left] == sources[&right],
            "same decoded presentation interval {left}/96 and {right}/96"
        );
    }
    assert!(
        sources[&16] != sources[&17],
        "fractional request must cross the actual .167s presentation boundary"
    );
    assert!(
        sources[&6] != sources[&8],
        "varying-media control must distinguish source frames"
    );
    assert!(
        sources[&14] != sources[&17],
        "later source frames must also differ"
    );
    sources
}

fn with_preview_runtime<R>(run: impl FnOnce(&PreviewProductionRuntime<()>) -> R) -> R {
    let runtime = PreviewProductionRuntime::<()>::try_new_boxed().unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&runtime)));
    runtime.shutdown_and_wait();
    result.unwrap_or_else(|error| std::panic::resume_unwind(error))
}

fn reference_retime_source_samples(
    state: &AppState,
) -> std::collections::BTreeMap<i64, mondrian_renderer::CpuColorFrame> {
    // Capture the original untrimmed, unretimed, effect-free source. Eighth
    // source-frame coordinates express 3/2 times quarter Clip-frame offsets.
    // The oracle below uses literal media coordinates, never the retimed mapper.
    let mut reference = state.active_sequence().unwrap().clone();
    let original = &reference.video_tracks[0].clips[0];
    assert_eq!(original.position, TimelineTime::ZERO);
    assert_eq!(original.clip_time_in, TimelineTime::ZERO);
    assert_eq!(original.source_origin(), TimelineTime::ZERO);
    assert_eq!(
        original.source_time_scale(),
        mondrian_core::TimeScale::new(1, 1).unwrap()
    );
    assert!(original.effects.is_empty());
    reference.settings.frame_rate = Rational::new(192, 1);
    let target = reference.settings.resolution;
    let snapshot = crate::app::exporting::capture_timeline_export_snapshot(
        state,
        reference,
        vec![],
        mondrian_export::preset::TimelineExportRange::WorkArea {
            start_frame: 0,
            end_frame_exclusive: 96,
        },
        false,
    )
    .unwrap();
    let sources: std::collections::BTreeMap<_, _> = [34, 37, 40, 43, 46, 52, 58, 61, 64, 67, 70]
        .into_iter()
        .map(|tick| {
            let frame =
                mondrian_export::queue::export_visual_frame_validation(&snapshot, tick, target)
                    .unwrap()
                    .working_frame;
            assert_eq!(frame.rgba_f32().data.len(), 256);
            assert!(frame.rgba_f32().data.iter().all(|p| p[3] == 1.0));
            (tick, frame)
        })
        .collect();
    for (left, right) in [(34, 37), (40, 43), (43, 46), (58, 61), (64, 67), (67, 70)] {
        assert_eq!(
            sources[&left], sources[&right],
            "controlled actual PTS interval"
        );
    }
    for (left, right) in [(34, 40), (40, 52), (52, 58), (58, 64)] {
        assert_ne!(
            sources[&left], sources[&right],
            "oracle distinguishes source pictures"
        );
    }
    sources
}

fn place_trimmed_time_blur_clip(state: &mut AppState, clip_id: mondrian_core::ClipId) {
    use crate::app::product_action::{
        TimelineMoveClipPayload, TimelineTrimClipsPayload, TimelineTrimPayloadEdge,
    };
    // Original media [0,12) -> trimmed [2,8), placed at Sequence frame17.
    // Clip-local author time remains [2,8), independently of source retiming.
    let time_base = state.active_sequence().unwrap().time_base();
    for (edge, frame) in [
        (TimelineTrimPayloadEdge::Out, 8),
        (TimelineTrimPayloadEdge::In, 2),
    ] {
        state
            .dispatch_action(crate::app::ui_actions::timeline_trim_clips_action(
                TimelineTrimClipsPayload {
                    clip_ids: vec![clip_id],
                    edge,
                    position: FramePosition::new(frame, time_base),
                },
            ))
            .unwrap();
    }
    let track = state.active_sequence().unwrap().video_tracks[0].id;
    state
        .dispatch_action(crate::app::ui_actions::timeline_move_clip_action(
            TimelineMoveClipPayload {
                target_track_id: track,
                clip_id,
                position: FramePosition::new(17, time_base),
            },
        ))
        .unwrap();
    let clip = state.active_sequence().unwrap().find_clip(clip_id).unwrap();
    assert_eq!(clip.position, TimelineTime::new(17, 24).unwrap());
    assert_eq!(clip.clip_time_in, TimelineTime::new(2, 24).unwrap());
    assert_eq!(clip.duration, TimelineTime::new(6, 24).unwrap());
    assert_eq!(clip.source_origin(), TimelineTime::new(2, 24).unwrap());
}

fn verify_time_blur_frame(
    runtime: &PreviewProductionRuntime<()>,
    state: &mut AppState,
    sources: &std::collections::BTreeMap<i64, mondrian_renderer::CpuColorFrame>,
    frame: i64,
    offsets: &[i64],
) -> mondrian_effects::EffectProcessorFeedbackBatch {
    let (output, feedback) = acquire_working(runtime, state, frame);
    assert_eq!(output.rgba_f32().data.len(), 256);
    for (pixel_index, pixel) in output.rgba_f32().data.iter().enumerate() {
        for channel in 0..3 {
            let expected = offsets
                .iter()
                .map(|offset| {
                    let tick = frame * 4 + offset;
                    sources[&tick].rgba_f32().data[pixel_index][channel]
                        * 2.0_f32.powf(tick as f32 / 16.0)
                })
                .sum::<f32>()
                / offsets.len() as f32;
            assert!((pixel[channel] - expected).abs() < 2e-5,
                "frame {frame} pixel {pixel_index} channel {channel}: {pixel:?}, expected={expected}");
        }
        assert_eq!(pixel[3], 1.0);
    }
    assert_eq!(feedback.len(), 1);
    assert_ne!(
        feedback.iter().next().unwrap().feedback.snapshot.native_owner(),
        0
    );
    let exported = mondrian_export::queue::export_visual_frame_validation(
        &export_snapshot(state),
        frame,
        state.active_sequence().unwrap().settings.resolution,
    )
    .unwrap();
    assert_eq!(
        output, exported.working_frame,
        "real Preview/Export working-frame parity"
    );
    feedback
}

fn verify_time_blur_retimed_frame(
    runtime: &PreviewProductionRuntime<()>,
    state: &mut AppState,
    sources: &std::collections::BTreeMap<i64, mondrian_renderer::CpuColorFrame>,
    sequence_frame: i64,
    clip_frame: i64,
    media_ticks: [i64; 4],
) {
    let (output, feedback) = acquire_working(runtime, state, sequence_frame);
    assert_eq!(output.rgba_f32().data.len(), 256);
    for (index, pixel) in output.rgba_f32().data.iter().enumerate() {
        for channel in 0..3 {
            let expected = media_ticks
                .iter()
                .zip([-2, -1, 0, 1])
                .map(|(source, offset)| {
                    // Exposure belongs to Clip time, not Sequence placement or
                    // media time. Media samples use the independent192fps grid.
                    let exposure = (clip_frame * 4 + offset) as f32 / 16.0;
                    sources[source].rgba_f32().data[index][channel] * 2.0_f32.powf(exposure)
                })
                .sum::<f32>()
                / 4.0;
            assert!((pixel[channel] - expected).abs() < 2e-5,
                "Sequence{sequence_frame}/Clip{clip_frame}, source{media_ticks:?}, pixel{index}/{channel}: {pixel:?}, expected={expected}");
        }
        assert_eq!(pixel[3], 1.0);
    }
    assert_eq!(feedback.len(), 1);
    assert_ne!(
        feedback.iter().next().unwrap().feedback.snapshot.native_owner(),
        0
    );
    let snapshot = crate::app::exporting::capture_timeline_export_snapshot(
        state,
        state.active_sequence().unwrap().clone(),
        vec![],
        mondrian_export::preset::TimelineExportRange::WorkArea {
            start_frame: 17,
            end_frame_exclusive: 23,
        },
        false,
    )
    .unwrap();
    let exported = mondrian_export::queue::export_visual_frame_validation(
        &snapshot,
        sequence_frame,
        state.active_sequence().unwrap().settings.resolution,
    )
    .unwrap();
    assert_eq!(
        output, exported.working_frame,
        "retimed real Preview/Export working pixels"
    );
}

fn qualify_time_blur_retime(
    runtime: &PreviewProductionRuntime<()>,
    state: &mut AppState,
    sources: &std::collections::BTreeMap<i64, mondrian_renderer::CpuColorFrame>,
    clip_id: mondrian_core::ClipId,
) {
    use crate::app::product_action::{ClipHoldFramePayload, ClipSetRatePayload};
    use mondrian_core::TimeScale;
    // Clip starts at Sequence17, Clip-local2, media2. Its duration is6 frames.
    // Forward source=2+3/2*offset; reversal anchors at exclusive terminal11.
    let forward = [[34, 37, 40, 43], [58, 61, 64, 67]];
    let reverse = [[70, 67, 64, 61], [46, 43, 40, 37]];
    let held = [[52; 4]; 2]; // Reverse sample at Sequence20 is media6.5/24.
    let author = |state: &AppState| {
        serde_json::to_value(state.active_sequence().unwrap().find_clip(clip_id).unwrap()).unwrap()
    };
    let verify = |state: &mut AppState, samples: [[i64; 4]; 2]| {
        let before = author(state);
        for index in [0, 1, 0] {
            verify_time_blur_retimed_frame(
                runtime,
                state,
                sources,
                [19, 21][index],
                [4, 6][index],
                samples[index],
            );
            assert_eq!(
                author(state),
                before,
                "processing cannot mutate authored Clip state"
            );
        }
    };
    for (numerator, origin, samples) in [(3, 2, forward), (-3, 11, reverse)] {
        let rate = TimeScale::new(numerator, 2).unwrap();
        state
            .dispatch_action(crate::app::ui_actions::clip_set_rate_action(
                ClipSetRatePayload { clip_id, rate, include_linked: false },
            ))
            .unwrap();
        let clip = state.active_sequence().unwrap().find_clip(clip_id).unwrap();
        assert_eq!(clip.source_time_scale(), rate);
        assert_eq!(clip.source_origin(), TimelineTime::new(origin, 24).unwrap());
        assert_eq!(clip.clip_time_in, TimelineTime::new(2, 24).unwrap());
        assert_eq!(clip.position, TimelineTime::new(17, 24).unwrap());
        assert_eq!(clip.duration, TimelineTime::new(6, 24).unwrap());
        verify(state, samples);
    }
    let reversed_author = author(state);
    state
        .dispatch_action(crate::app::ui_actions::clip_hold_frame_action(
            ClipHoldFramePayload {
                clip_id,
                sequence_time: FramePosition::new(20, Rational::new(1, 24)),
            },
        ))
        .unwrap();
    let clip = state.active_sequence().unwrap().find_clip(clip_id).unwrap();
    assert_eq!(clip.source_time_scale(), TimeScale::new(0, 1).unwrap());
    assert_eq!(clip.source_origin(), TimelineTime::new(13, 48).unwrap());
    let held_author = author(state);
    verify(state, held);
    assert!(state.undo_timeline().unwrap());
    assert_eq!(author(state), reversed_author);
    verify(state, reverse);
    assert!(state.redo_timeline().unwrap());
    assert_eq!(author(state), held_author);
    verify(state, held);
}

fn cancel_time_blur_source_wait(
    runtime: &PreviewProductionRuntime<()>,
    state: &mut AppState,
    sources: &std::collections::BTreeMap<i64, mondrian_renderer::CpuColorFrame>,
    kind: &mondrian_effects::EffectType,
    frame: i64,
    destination: i64,
) {
    let evaluation = schedule_working(runtime, state, frame);
    let old_epoch = state.preview_transport_intent().epoch();
    let old_cancel = runtime.execution.borrow().generation_cancellation();
    let attempt = runtime.cpu_fallback_in_flight.borrow().as_ref().unwrap().attempt.clone();
    let deadline = Instant::now() + Duration::from_secs(10);
    // Drain only the original CPU task transport. Do not serve the Source
    // request: the actual native invocation must still be waiting at seek.
    loop {
        if let Some(result) = runtime.cpu_fallback_task.as_ref().unwrap().try_poll() {
            match result {
                PreviewCpuFallbackResult::SourceRequest(request) => {
                    assert!(request.is_active());
                    runtime.observe_processor_source_request(*request);
                    break;
                }
                PreviewCpuFallbackResult::Failed(failed) => {
                    panic!("before actual wait: {}", failed.reason)
                }
                PreviewCpuFallbackResult::Ready(_) => {
                    panic!("temporal invocation completed without its required Source wait")
                }
                _ => {}
            }
        }
        assert!(
            Instant::now() < deadline,
            "native TimeBlur never reached Source wait"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(runtime.pending_processor_sources.borrow().len(), 1);
    assert!(!old_cancel.is_canceled());
    state.seek(destination).unwrap();
    assert_ne!(state.preview_transport_intent().epoch(), old_epoch);
    runtime.synchronize_transport_intent(state.preview_transport_intent());
    assert!(
        old_cancel.is_canceled(),
        "public seek must retire original generation authority"
    );
    assert!(runtime.pending_processor_sources.borrow().is_empty());
    assert!(runtime.cpu_fallback_in_flight.borrow().is_none());
    let clear = runtime
        .poll_playback_work(None, state.preview_transport_intent())
        .processor_feedback
        .expect("seek must deliver the current empty feedback view");
    assert!(
        clear.feedback.is_empty(),
        "old native facts cannot survive retirement"
    );
    assert_eq!(clear.scope.key, None);
    assert_eq!(clear.scope.epoch, state.preview_transport_intent().epoch());
    assert_eq!(
        clear.scope.generation,
        runtime.execution.borrow().generation()
    );
    assert!(runtime
        .frame_store
        .borrow_mut()
        .viewer_working_frame(&attempt.output_key)
        .is_none());
    drop(evaluation);
    verify_time_blur_frame(runtime, state, sources, destination, &[-2, -1, 0, 1]);
    assert!(runtime.pending_processor_sources.borrow().is_empty());
    assert!(runtime.cpu_fallback_failure.borrow().is_none());
    assert!(
        runtime
            .frame_store
            .borrow_mut()
            .viewer_working_frame(&attempt.output_key)
            .is_none(),
        "canceled native work must never publish its old output"
    );
    assert!(
        !mondrian_effects::effect_extension_runtime_status(&kind.key()).unwrap().disabled,
        "scheduler cancellation is not a plugin fault"
    );
}

fn install_time_blur_catalog(
    state: &mut AppState,
    binary: &std::path::Path,
    kind: &mondrian_effects::EffectType,
    preferences_path: &std::path::Path,
) {
    use crate::app::openfx_catalog::OpenFxBundleSelection;
    use crate::app_ui::preferences_store::{persist_app_ui_preferences_to, AppUiPreferences};
    let bundle = binary.parent().unwrap().parent().unwrap().parent().unwrap().to_path_buf();
    assert!(bundle.file_name().unwrap().to_string_lossy().ends_with(".ofx.bundle"));
    let selection = OpenFxBundleSelection {
        path: bundle.clone(),
        context: crate::openfx_host::OpenFxContext::Filter,
    };
    let generation = state.project_author_generation();
    state
        .dispatch_action(
            ProductAction::VisualEffect(VisualEffectProductAction::InstallOpenFxBundle(
                VisualEffectInstallOpenFxBundlePayload { path: bundle, context: selection.context },
            ))
            .into_external_action(),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let completed = loop {
        let changed = state.poll_openfx_catalog();
        let completed = state.take_completed_openfx_installs();
        if !completed.is_empty() {
            break completed;
        }
        assert!(
            !(changed && state.status_hint.as_ref().is_some_and(|(_, failed)| *failed)),
            "catalog installation failed: {:?}",
            state.status_hint
        );
        assert!(
            Instant::now() < deadline,
            "catalog installation did not settle"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(completed, vec![selection.clone()]);
    assert_eq!(state.project_author_generation(), generation);
    assert!(
        mondrian_effects::effect_library_types().contains(kind),
        "catalog must publish TimeBlur"
    );
    let mut preferences = AppUiPreferences::default();
    preferences.record_openfx_bundle(selection);
    persist_app_ui_preferences_to(preferences_path, &preferences).unwrap();
}

fn qualify_time_blur_playback(state: &mut AppState, kind: &mondrian_effects::EffectType) {
    use crate::app::headless_realtime_playback::{
        HeadlessGpuExecutionDisposition, HeadlessGpuExecutionObserver,
        HeadlessRealtimeIntervalOutcome, HeadlessRealtimePlaybackSession,
    };
    use crate::app::headless_viewer_gpu::HeadlessViewerGpuExecution;
    use mondrian_playback::FrameDemandIdentity;

    // Only bounded scalar observations occur inside the actual realtime driver.
    #[derive(Default)]
    struct Presentations {
        epoch: Option<mondrian_playback::PlaybackEpoch>,
        first: Option<i64>,
        last: Option<i64>,
        count: usize,
        wrong_epoch: bool,
    }
    impl Presentations {
        fn observe(&mut self, demand: Option<FrameDemandIdentity>) {
            if let Some(demand) = demand {
                self.wrong_epoch |= self.epoch != Some(demand.epoch);
                self.first.get_or_insert(demand.target_frame);
                self.last = Some(demand.target_frame);
                self.count += 1;
            }
        }
    }
    impl HeadlessGpuExecutionObserver for Presentations {
        fn execution_completed(
            &mut self,
            _: HeadlessViewerGpuExecution,
            disposition: HeadlessGpuExecutionDisposition,
            demand: Option<FrameDemandIdentity>,
        ) {
            if disposition == HeadlessGpuExecutionDisposition::PublishedCurrent {
                self.observe(demand);
            }
        }
        fn current_output_presented(&mut self, demand: Option<FrameDemandIdentity>) {
            self.observe(demand);
        }
        fn successor_preparation(&mut self, _: bool) {}
    }

    let authored = serde_json::to_value(state.active_sequence().unwrap()).unwrap();
    let mut session = HeadlessRealtimePlaybackSession::new().unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Stopped Play intentionally starts at zero. Establish the ordinary
        // paused coordinate before starting the first timed Playback Session.
        state.play().unwrap();
        state.pause().unwrap();
        state.seek(2).unwrap();
        let mut observer = Presentations::default();
        session
            .prepare_stopped_picture(
                state,
                &mut observer,
                Instant::now() + Duration::from_secs(10),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "{error:#}; actual Viewer refusal: {:?}",
                    session.preview().unwrap().last_unavailability()
                )
            });
        state.play().unwrap();
        let first_epoch = state.playback_epoch();
        observer = Presentations { epoch: Some(first_epoch), ..Default::default() };
        let deadline = Instant::now() + Duration::from_secs(10);
        session.begin_realtime(state, Some(deadline)).unwrap();
        session
            .complete_current_video_opportunity(state, &mut observer, Duration::from_secs(10))
            .unwrap();
        while state.current_frame() < 4 {
            assert!(
                Instant::now() < deadline,
                "actual Playback clock did not advance"
            );
            assert!(matches!(
                session
                    .run_video_interval(state, &mut observer, Duration::from_secs(10))
                    .unwrap(),
                HeadlessRealtimeIntervalOutcome::Advanced { .. }
            ));
        }
        assert!(!state.is_playback_priming());
        assert!(observer.count >= 2 && observer.last.is_some_and(|frame| frame > 2));
        assert!(!observer.wrong_epoch);
        state.pause().unwrap();
        session.finish_realtime().unwrap();
        let paused = state.current_frame();
        session
            .prepare_stopped_picture(
                state,
                &mut Presentations::default(),
                Instant::now() + Duration::from_secs(10),
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(50));
        state.advance_playback_clock_at(Instant::now());
        assert_eq!(
            state.current_frame(),
            paused,
            "paused clock holds the actual picture"
        );

        state.play().unwrap();
        let resumed_epoch = state.playback_epoch();
        assert_ne!(resumed_epoch, first_epoch);
        observer = Presentations { epoch: Some(resumed_epoch), ..Default::default() };
        let deadline = Instant::now() + Duration::from_secs(10);
        session.begin_realtime(state, Some(deadline)).unwrap();
        session
            .complete_current_video_opportunity(state, &mut observer, Duration::from_secs(10))
            .unwrap();
        loop {
            assert!(
                Instant::now() < deadline,
                "resumed Playback did not reach natural end"
            );
            if let HeadlessRealtimeIntervalOutcome::NaturalEnd { terminal_frame } = session
                .run_video_interval(state, &mut observer, Duration::from_secs(10))
                .unwrap()
            {
                assert_eq!(terminal_frame, 11);
                break;
            }
        }
        session.finish_realtime().unwrap();
        assert!(observer.count >= 2 && observer.last.is_some_and(|frame| frame > paused));
        assert!(
            !observer.wrong_epoch,
            "old epoch cannot complete a resumed demand"
        );
        assert_eq!(
            state.playback_engine.snapshot().state,
            mondrian_playback::TransportState::Ended
        );
        assert_eq!(state.current_frame(), 11);
        session
            .prepare_stopped_picture(
                state,
                &mut Presentations::default(),
                Instant::now() + Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(
            serde_json::to_value(state.active_sequence().unwrap()).unwrap(),
            authored
        );
        assert!(!mondrian_effects::effect_extension_runtime_status(&kind.key()).unwrap().disabled);
    }));
    let owners = session.into_shutdown_owners();
    let last_refusal = owners.preview().last_unavailability();
    let terminal_context = (
        state.playback_engine.snapshot(),
        state.pending_playback_frame_demand_identity(),
        owners
            .preview()
            .cpu_fallback_in_flight
            .borrow()
            .as_ref()
            .map(|pending| (pending.attempt.clone(), pending.completed)),
        owners.preview().pending_processor_sources.borrow().len(),
        owners.preview().execution.borrow().is_pending(),
    );
    let shutdown = owners.shutdown_until(Instant::now() + Duration::from_secs(10));
    assert!(shutdown.gpu_preview_dependency_barrier.is_complete());
    assert!(shutdown.preview.all_workers_terminated());
    assert!(shutdown.gpu.qualifies_normal_runtime());
    result.unwrap_or_else(|error| {
        eprintln!("actual TimeBlur Viewer refusal: {last_refusal:?}");
        eprintln!("transport/demand/CPU/source waits/execution: {terminal_context:?}");
        std::panic::resume_unwind(error)
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TimeBlurNleCase {
    Uniform,
    Varying,
    AuthorHistory,
    CancelAndRecover,
    TimedPlayback,
    RetimedPlacement,
}

#[test]
#[ignore = "requires ordinary IPC17 Product, published OpenFX-Misc TimeBlur 1.0, FFmpeg and OCIO"]
fn time_blur_real_media_preview_export_evaluate_upstream_animation_at_requested_times() {
    qualify_time_blur_nle(TimeBlurNleCase::Uniform);
}

#[test]
#[ignore = "requires ordinary IPC17 Product, published OpenFX-Misc TimeBlur 1.0, FFmpeg and OCIO"]
fn time_blur_varying_media_preserves_fractional_source_selection_and_animation() {
    qualify_time_blur_nle(TimeBlurNleCase::Varying);
}

#[test]
#[ignore = "requires ordinary IPC17 Product, published TimeBlur, catalog restore, FFmpeg and OCIO"]
fn time_blur_public_parameter_history_and_project_reopen_preserve_temporal_output() {
    qualify_time_blur_nle(TimeBlurNleCase::AuthorHistory);
}

#[test]
#[ignore = "requires ordinary IPC17 Product, published TimeBlur, FFmpeg and OCIO"]
fn time_blur_seek_cancels_actual_source_wait_and_recovers_without_stale_output() {
    qualify_time_blur_nle(TimeBlurNleCase::CancelAndRecover);
}

#[test]
#[ignore = "requires ordinary IPC17 Product, published TimeBlur, FFmpeg, OCIO and real Viewer GPU"]
fn time_blur_timed_playback_pause_resume_and_natural_end_keep_current_authority() {
    qualify_time_blur_nle(TimeBlurNleCase::TimedPlayback);
}

#[test]
#[ignore = "requires ordinary IPC17 Product, published TimeBlur, FFmpeg and OCIO"]
fn time_blur_public_trim_move_forward_reverse_hold_preserve_three_time_domains() {
    qualify_time_blur_nle(TimeBlurNleCase::RetimedPlacement);
}

fn qualify_time_blur_nle(case: TimeBlurNleCase) {
    let varying_media = case != TimeBlurNleCase::Uniform;
    let history = case == TimeBlurNleCase::AuthorHistory;
    let _guard = crate::app_ui::test_utils::theme_test_guard();
    ensure_test_ocio_loaded();
    let helper = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_TEST_HELPER").unwrap());
    let binary = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_INVERT_BINARY").unwrap());
    let inspection = crate::openfx_adapter::inspect_openfx_binary(&helper, &binary).unwrap();
    let description =
        crate::openfx_host::describe_openfx_filter(&helper, &inspection, "net.sf.openfx.TimeBlur")
            .unwrap();
    let kind = if history {
        use sha2::{Digest, Sha256};
        assert_eq!(
            inspection
                .plugins
                .iter()
                .filter(|p| p.identifier == "net.sf.openfx.TimeBlur"
                    && p.version_major == 1
                    && p.version_minor == 0)
                .count(),
            1
        );
        let identity = format!(
            "{}\0{}.{}\0{}",
            "net.sf.openfx.TimeBlur", 1, 0, inspection.binary_sha256
        );
        mondrian_effects::EffectType::External(format!(
            "openfx.{:x}",
            Sha256::digest(identity.as_bytes())
        ))
    } else {
        crate::openfx_effect::register_selected_openfx_filter(
            &helper,
            &inspection,
            "net.sf.openfx.TimeBlur",
        )
        .unwrap()
    };
    let directory = tempfile::tempdir().unwrap();
    // The uniform control isolates animation. The varying control supplies
    // distinct Y/U/V planes per frame and independently samples source time.
    let raw = directory.path().join("source.y4m");
    let source = directory.path().join("source.mkv");
    let mut media = b"YUV4MPEG2 W16 H16 F24:1 Ip A1:1 C444 XCOLORRANGE=LIMITED\n".to_vec();
    for frame in 0..(if varying_media { 12_u8 } else { 8_u8 }) {
        media.extend_from_slice(b"FRAME\n");
        media.extend_from_slice(&[if varying_media { 48 + frame * 12 } else { 80 }; 256]);
        media.extend_from_slice(&[if varying_media { 112 + frame * 2 } else { 128 }; 256]);
        media.extend_from_slice(&[if varying_media { 144 - frame * 2 } else { 128 }; 256]);
    }
    std::fs::write(&raw, media).unwrap();
    // Y4M cannot carry all required color metadata. Encode losslessly rather
    // than inventing probe metadata or relaxing the production decode contract.
    let encoded = std::process::Command::new("ffmpeg.exe")
        .args(["-nostdin", "-loglevel", "error", "-y", "-i"])
        .arg(&raw)
        .args([
            "-c:v",
            "ffv1",
            "-level",
            "3",
            "-color_range",
            "tv",
            "-colorspace",
            "bt709",
            "-color_trc",
            "bt709",
            "-color_primaries",
            "bt709",
            "-an",
        ])
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        encoded.status.success(),
        "{}",
        String::from_utf8_lossy(&encoded.stderr)
    );
    if varying_media {
        let timing = std::process::Command::new("ffprobe.exe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_frames",
                "-show_entries",
                "stream=time_base:frame=best_effort_timestamp",
                "-of",
                "json",
            ])
            .arg(&source)
            .output()
            .unwrap();
        assert!(
            timing.status.success(),
            "{}",
            String::from_utf8_lossy(&timing.stderr)
        );
        let timing: serde_json::Value = serde_json::from_slice(&timing.stdout).unwrap();
        assert_eq!(timing["streams"][0]["time_base"], "1/1000");
        let pts: Vec<_> = timing["frames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|frame| frame["best_effort_timestamp"].as_i64().unwrap())
            .collect();
        assert_eq!(pts.len(), 12);
        assert_eq!(&pts[..5], &[0, 42, 83, 125, 167], "controlled fixture PTS");
    }
    let info = mondrian_media::probe_media_info(&source).unwrap();
    assert_eq!(info.primary_video().unwrap().frame_rate, Rational::FPS_24);
    let mut state = AppState::new();
    pin_standard_execution_resources(&mut state);
    state
        .create_new_project_at(
            directory.path().join("temporal.mdp"),
            "Temporal media",
            16,
            16,
            Rational::FPS_24,
        )
        .unwrap();
    let asset = commit_preview_test_media(state.asset_library().unwrap(), source, info);
    let sequence = state.active_sequence().unwrap();
    state
        .dispatch_action(
            ProductAction::Timeline(TimelineProductAction::PlaceAsset(
                TimelineDropAssetPayload {
                    asset_id: asset,
                    target_track_id: sequence.video_tracks[0].id,
                    position: FramePosition::new(0, sequence.time_base()),
                },
            ))
            .into_external_action(),
        )
        .unwrap();
    let clip_id = state.active_sequence().unwrap().video_tracks[0].clips[0].id;
    let preferences_path = directory.path().join("preferences.json");
    if history {
        assert_eq!(
            crate::app::packaged_worker::discover_openfx_worker().unwrap(),
            helper
        );
        install_time_blur_catalog(&mut state, &binary, &kind, &preferences_path);
    }
    let sources = if case == TimeBlurNleCase::RetimedPlacement {
        reference_retime_source_samples(&state)
    } else {
        reference_source_samples(&state, varying_media)
    };
    let saved = with_preview_runtime(|runtime| {
        state
            .add_effect_to_clip(clip_id, mondrian_effects::EffectType::BasicCorrection)
            .unwrap();
        let correction = &state.active_sequence().unwrap().find_clip(clip_id).unwrap().effects[0];
        let correction_id = correction.id;
        let exposure_id =
            mondrian_effects::EffectType::BasicCorrection.parameter_id("exposure").unwrap();
        let exposure = correction
            .properties
            .iter()
            .find(|(_, p)| p.descriptor.parameter_id() == &exposure_id)
            .expect("canonical BasicCorrection exposure definition")
            .1
            .address();
        for (frame, value) in [(0, 0.0), (7, 1.75)] {
            state
                .dispatch_action(
                    ProductAction::VisualEffect(VisualEffectProductAction::EditNumericCurve(
                        Box::new(VisualEffectEditNumericCurvePayload {
                            clip_id,
                            effect_id: correction_id,
                            parameter: exposure.clone(),
                            edit: ClipCurveEditPayload::Upsert {
                                keyframe_id: None,
                                point: ClipCurveAuthorPoint {
                                    time: TimelineTime::new(frame, 24).unwrap(),
                                    value: AnimationChannelValue::Number(value),
                                },
                            },
                        }),
                    ))
                    .into_external_action(),
                )
                .unwrap();
        }
        let exposure_curve = state.active_sequence().unwrap().find_clip(clip_id).unwrap().effects
            [0]
        .properties
        .property_by_address(&exposure)
        .unwrap()
        .1
        .channel(0)
        .unwrap();
        assert_eq!(exposure_curve.keyframes().len(), 2);
        assert!(exposure_curve
            .keyframes()
            .iter()
            .all(|key| key.interp_in == KeyframeInterpolation::Linear
                && key.interp_out == KeyframeInterpolation::Linear));
        state.add_effect_to_clip(clip_id, kind.clone()).unwrap();
        let effect =
            state.active_sequence().unwrap().find_clip(clip_id).unwrap().effects[1].clone();
        let centered = &description
            .parameters
            .iter()
            .find(|p| p.name == "shutterOffset")
            .unwrap()
            .choices[0]
            .key;
        for (name, value) in [
            ("division", PropertyValue::Int(4)),
            ("shutter", PropertyValue::Double(1.0)),
            ("shutterOffset", PropertyValue::Enum(centered.clone())),
        ] {
            let label = &description.parameters.iter().find(|p| p.name == name).unwrap().label;
            let property = effect
                .properties
                .iter()
                .find(|(_, p)| p.descriptor.display_name == *label)
                .unwrap()
                .1;
            let address = property.address();
            if property.static_value() == &value {
                continue;
            }
            state
                .dispatch_action(
                    ProductAction::VisualEffect(VisualEffectProductAction::SetParameterValue(
                        Box::new(VisualEffectSetParameterValuePayload {
                            clip_id,
                            effect_id: effect.id,
                            write: ParameterValueWrite::Value { parameter: address, value },
                        }),
                    ))
                    .into_external_action(),
                )
                .unwrap();
        }
        if case == TimeBlurNleCase::RetimedPlacement {
            // Author the curve while its keys are visible, then preserve that
            // Clip-local animation through the public trim/move operations.
            place_trimmed_time_blur_clip(&mut state, clip_id);
            qualify_time_blur_retime(runtime, &mut state, &sources, clip_id);
            return None;
        }
        let author_before =
            serde_json::to_value(state.active_sequence().unwrap().find_clip(clip_id).unwrap())
                .unwrap();
        if case == TimeBlurNleCase::CancelAndRecover {
            for (frame, destination) in [(2, 4), (4, 2), (2, 4)] {
                cancel_time_blur_source_wait(
                    runtime,
                    &mut state,
                    &sources,
                    &kind,
                    frame,
                    destination,
                );
            }
        }
        let mut previous = None;
        for frame in [2, 4, 2] {
            let feedback =
                verify_time_blur_frame(runtime, &mut state, &sources, frame, &[-2, -1, 0, 1]);
            let receipt = feedback.iter().next().unwrap();
            let identity = (
                receipt.feedback.owner,
                receipt.feedback.origin,
                receipt.feedback.snapshot.native_owner(),
            );
            assert_ne!(identity.2, 0);
            if let Some((owner, invocation)) = previous {
                if owner == identity {
                    assert!(receipt.feedback.snapshot.invocation() > invocation);
                }
            }
            previous = Some((identity, receipt.feedback.snapshot.invocation()));
            assert_eq!(
                serde_json::to_value(state.active_sequence().unwrap().find_clip(clip_id).unwrap())
                    .unwrap(),
                author_before
            );
        }
        if !history {
            return None;
        }
        let shutter_label =
            &description.parameters.iter().find(|p| p.name == "shutter").unwrap().label;
        let shutter = effect
            .properties
            .iter()
            .find(|(_, p)| p.descriptor.display_name == *shutter_label)
            .unwrap()
            .1
            .address();
        state
            .dispatch_action(
                ProductAction::VisualEffect(VisualEffectProductAction::SetParameterValue(
                    Box::new(VisualEffectSetParameterValuePayload {
                        clip_id,
                        effect_id: effect.id,
                        write: ParameterValueWrite::Value {
                            parameter: shutter,
                            value: PropertyValue::Double(0.0),
                        },
                    }),
                ))
                .into_external_action(),
            )
            .unwrap();
        verify_time_blur_frame(runtime, &mut state, &sources, 4, &[0]);
        assert!(state.undo_timeline().unwrap());
        verify_time_blur_frame(runtime, &mut state, &sources, 4, &[-2, -1, 0, 1]);
        assert!(state.redo_timeline().unwrap());
        verify_time_blur_frame(runtime, &mut state, &sources, 4, &[0]);
        assert!(state.undo_timeline().unwrap());
        let feedback = verify_time_blur_frame(runtime, &mut state, &sources, 4, &[-2, -1, 0, 1]);
        let owner = feedback.iter().next().unwrap().feedback.owner;
        let authored =
            serde_json::to_value(state.active_sequence().unwrap().find_clip(clip_id).unwrap())
                .unwrap();
        assert_eq!(
            authored, author_before,
            "undo returns to exact authored Clip state"
        );
        let saved_path = directory.path().join("saved.mdp");
        state.save_project_file_as(saved_path.clone()).unwrap();
        Some((saved_path, authored, owner))
    });
    if case == TimeBlurNleCase::TimedPlayback {
        qualify_time_blur_playback(&mut state, &kind);
    }
    if let Some((saved_path, authored, old_owner)) = saved {
        state.close_project().unwrap();
        drop(state);
        let installed = mondrian_effects::effect_definition(&kind).unwrap();
        let mut reopened = AppState::new();
        pin_standard_execution_resources(&mut reopened);
        let preferences =
            crate::app_ui::preferences_store::load_app_ui_preferences_from(&preferences_path);
        assert_eq!(preferences.installed_openfx_bundles.len(), 1);
        let generation = reopened.project_author_generation();
        reopened.schedule_openfx_catalog_restore(preferences.installed_openfx_bundles);
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            reopened.poll_openfx_catalog();
            if let Some(current) = mondrian_effects::effect_definition(&kind)
                && !Arc::ptr_eq(&current, &installed)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "catalog restore did not publish a fresh definition"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(reopened.take_completed_openfx_installs().is_empty());
        assert_eq!(reopened.project_author_generation(), generation);
        reopened.open_project_file(saved_path).unwrap();
        assert_eq!(
            serde_json::to_value(reopened.active_sequence().unwrap().find_clip(clip_id).unwrap())
                .unwrap(),
            authored
        );
        with_preview_runtime(|runtime| {
            for frame in [2, 4, 2] {
                let feedback = verify_time_blur_frame(
                    runtime,
                    &mut reopened,
                    &sources,
                    frame,
                    &[-2, -1, 0, 1],
                );
                assert_ne!(
                    feedback.iter().next().unwrap().feedback.owner,
                    old_owner,
                    "reopened processing must use a fresh bank owner"
                );
                assert_eq!(
                    serde_json::to_value(
                        reopened.active_sequence().unwrap().find_clip(clip_id).unwrap()
                    )
                    .unwrap(),
                    authored
                );
            }
        });
        reopened.close_project().unwrap();
    }
}
