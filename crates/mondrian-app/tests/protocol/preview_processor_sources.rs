use super::*;
use crate::app::preview_access_mode::PreviewMediaWorkPolicy;
use crate::app::preview_processor_sources::PreviewProcessorSourceWork;
use crate::app::preview_timeline_execution::PreviewTimelineMediaFrame;
use std::sync::atomic::Ordering;

fn decoded_frame(
    runtime: &PreviewProductionRuntime<()>,
    key: &MediaPreviewKey,
    policy: PreviewMediaWorkPolicy,
) -> MediaPreviewFrame {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        runtime.poll_finished_outcome(None);
        match runtime.media_frame_for_frozen_key(key.clone(), policy, true) {
            PreviewTimelineMediaFrame::Ready(frame) => return frame,
            PreviewTimelineMediaFrame::Unavailable { reason } => {
                panic!("actual source decode failed: {}", reason.detail())
            }
            PreviewTimelineMediaFrame::Pending { .. } => {
                assert!(
                    Instant::now() < deadline,
                    "actual source producer did not settle"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

fn finite_temporal_prefix() -> mondrian_core::effect_data::EffectNode {
    use mondrian_effects::*;
    let kind = mondrian_core::effect_data::EffectType::External(format!(
        "test.preview.prepared-temporal.{}",
        AssetId::new()
    ));
    let offset = mondrian_core::TimelineTime::new(1, 4).unwrap();
    register_effect_definition(
        EffectDefinition::new(
            kind.key(),
            "Finite input time",
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            temporal_input: EffectTemporalInputExtent {
                past: EffectTemporalSpan::None,
                future: EffectTemporalSpan::Finite(offset),
            },
            roi_propagation: EffectRoiPropagation::PixelLocal,
            ..EffectExecutionContract::IDENTITY
        })
        .with_graph_builder(Arc::new(move |_, _, graph| {
            graph.append_unary(EffectRenderOp::TemporalFrameBlend {
                sample_offset: offset,
                mix: 0.25,
            });
            Ok(())
        })),
    )
    .unwrap();
    mondrian_core::effect_data::EffectNode::new(kind)
}

#[test]
fn processor_source_preplanned_production_acquire_preserves_gpu_only_layers_and_one_evaluation() {
    use crate::app::preview_cpu_fallback_task::source_job_request_with_media;
    use mondrian_effects::*;
    ensure_test_ocio_loaded();
    let (request, plan, routes, facts, old_program, mut sequence) =
        source_job_request_with_media(false, None);
    drop((request, plan, routes));
    assert!(old_program.upgrade().is_none());
    sequence.video_tracks[0].clips[0].insert_effect_node_at(1, finite_temporal_prefix());
    let kind = mondrian_core::effect_data::EffectType::External(format!(
        "test.preview.gpu-only.{}",
        AssetId::new()
    ));
    register_effect_definition(
        EffectDefinition::new(
            kind.key(),
            "GPU-only control",
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::GPU_F32,
            ..EffectExecutionContract::IDENTITY
        })
        .with_graph_builder(Arc::new(|_, _, graph| {
            graph.append_unary(EffectRenderOp::ColorAdjust {
                exposure: 0.25,
                contrast: 1.0,
                saturation: 1.0,
                working_color_space: mondrian_core::WorkingColorSpace::LinearRec709,
            });
            Ok(())
        })),
    )
    .unwrap();
    let mut gpu = Clip::new_solid_color(
        AssetId::new(),
        Color { r: 0.1, g: 0.2, b: 0.3, a: 1.0 },
        mondrian_core::TimelineTime::ZERO,
        mondrian_core::TimelineTime::new(10, 1).unwrap(),
    )
    .unwrap();
    gpu.add_effect_node(mondrian_core::effect_data::EffectNode::new(kind));
    let mut track = mondrian_timeline::Track::new_video("GPU-only layer");
    track.add_clip(gpu).unwrap();
    sequence.video_tracks.push(track);
    let mut state = AppState::new();
    state.test_set_sequence(Some(sequence));
    state.seek(72).unwrap();
    let runtime = PreviewProductionRuntime::<()>::try_new_boxed().unwrap();
    runtime.synchronize_transport_intent(state.preview_transport_intent());
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let sequence = state.active_sequence().unwrap();
    let color = sequence
        .settings
        .root_program_color_context(state.project_color_environment())
        .unwrap();
    let key = FrameEvaluationKey {
        sequence_id: sequence.id,
        sequence_revision: sequence.revision,
        author_generation: snapshot.authoring().unwrap().author_generation(),
        frame: 72,
        width: 1,
        height: 1,
        runtime_scale: snapshot.transport().runtime_scale(),
        raster_requirement: PreviewRasterRequirement::Viewer,
        display_color_space: ColorSpace::Srgb,
        display_contract_identity: None,
    };
    assert!(matches!(
        runtime.acquire_frame_evaluation(&snapshot, &state, sequence, 72, 1, 1, color.clone(), key),
        FrameResolutionOutcome::Pending(
            crate::app::preview_timeline_execution::PreviewTimelinePendingDependency::Execution { .. }
        )
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    let evaluation = loop {
        runtime.poll_playback_work(None, snapshot.transport().intent());
        match runtime.acquire_frame_evaluation(
            &snapshot,
            &state,
            sequence,
            72,
            1,
            1,
            color.clone(),
            key,
        ) {
            FrameResolutionOutcome::Ready(ready) => break ready,
            FrameResolutionOutcome::Pending(_) => {}
            FrameResolutionOutcome::Unavailable(reason) => {
                panic!("production evaluation failed: {}", reason.detail())
            }
            FrameResolutionOutcome::Empty => panic!("nonempty production evaluation"),
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(evaluation.elements.len(), 2);
    let ResolvedPreviewElement::Media { frame, .. } = &evaluation.elements[0] else {
        panic!("temporal result")
    };
    assert_eq!(
        frame.working_frame().unwrap().frame.rgba_f32().data,
        [[0.5, -1.0, 2.5, 0.5]]
    );
    assert!(
        matches!(crate::app::preview_viewer_plan::prepare_gpu_composite_layers_with_heterogeneous_effects(
        &evaluation.elements,color.working_color_space(),evaluation.sampling,&mut runtime.scratch.borrow_mut(),
        runtime.heterogeneous_effect_decision.get().cpu_prefix_grant()).unwrap(),
        crate::app::preview_viewer_plan::PreparedPreviewViewerGpuLayers::Ordinary{layers} if layers.len()==2)
    );
    let FrameResolutionOutcome::Ready(reused) =
        runtime.acquire_frame_evaluation(&snapshot, &state, sequence, 72, 1, 1, color, key)
    else {
        panic!("original evaluation must survive subsequent acquires")
    };
    assert!(Arc::ptr_eq(&evaluation, &reused));
    assert_eq!(facts.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *facts.threads.lock().unwrap(),
        ["mondrian-preview-visual-execution"]
    );
    runtime.shutdown_and_wait();
    assert_eq!(
        facts.created.load(Ordering::SeqCst),
        facts.dropped.load(Ordering::SeqCst)
    );
}

#[test]
fn processor_source_preplanned_user_block_keeps_current_feedback_authority() {
    use crate::app::preview_cpu_fallback_task::source_job_request_with_media;
    use crate::app::preview_processor_feedback::{
        PreviewProcessorFeedbackScope, PreviewProcessorFeedbackUpdate,
    };

    ensure_test_ocio_loaded();
    for speculative in [false, true] {
        let (request, plan, routes, facts, old_program, mut sequence) =
            source_job_request_with_media(false, None);
        drop((request, plan, routes));
        assert!(old_program.upgrade().is_none());
        facts.blocked.store(true, Ordering::SeqCst);
        sequence.video_tracks[0].clips[0].insert_effect_node_at(1, finite_temporal_prefix());
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        state.seek(72).unwrap();
        state.play().unwrap();
        let delivered =
            deliver_test_current_frame(&mut state, mondrian_playback::FrameDeliveryKind::Ready);
        state.observe_video_preroll_at_wall(delivered, 1, 1, true, true, Instant::now());
        let runtime = PreviewProductionRuntime::<()>::try_new_boxed().unwrap();
        runtime.synchronize_transport_intent(state.preview_transport_intent());
        let request = if speculative {
            state.preview_successor_execution_request(Instant::now()).unwrap()
        } else {
            state.preview_frame_execution_request(Instant::now())
        };
        let snapshot = request.snapshot();
        assert_eq!(
            snapshot.transport().is_speculative_preparation(),
            speculative
        );
        let sequence = state.active_sequence().unwrap();
        let color = sequence
            .settings
            .root_program_color_context(state.project_color_environment())
            .unwrap();
        let frame = snapshot.transport().current_frame();
        let key = FrameEvaluationKey {
            sequence_id: sequence.id,
            sequence_revision: sequence.revision,
            author_generation: snapshot.authoring().unwrap().author_generation(),
            frame,
            width: 1,
            height: 1,
            runtime_scale: snapshot.transport().runtime_scale(),
            raster_requirement: PreviewRasterRequirement::Viewer,
            display_color_space: ColorSpace::Srgb,
            display_contract_identity: None,
        };
        let current = PreviewProcessorFeedbackUpdate {
            scope: PreviewProcessorFeedbackScope {
                key: None,
                generation: runtime.execution.borrow().generation(),
                epoch: snapshot.transport().epoch(),
            },
            feedback: mondrian_effects::EffectProcessorFeedbackBatch::new(),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        let reason = loop {
            runtime.poll_playback_work(None, snapshot.transport().intent());
            // A speculative failure must not displace a pending current update.
            runtime.current_processor_feedback_scope.set(Some(current.scope));
            *runtime.pending_processor_feedback.borrow_mut() = Some(current.clone());
            match runtime.acquire_frame_evaluation(
                snapshot,
                &state,
                sequence,
                frame,
                1,
                1,
                color.clone(),
                key,
            ) {
                FrameResolutionOutcome::Unavailable(reason) => break reason,
                FrameResolutionOutcome::Pending(_) => {}
                _ => panic!("blocked processor must not publish an evaluation"),
            }
            assert!(Instant::now() < deadline, "blocked worker did not settle");
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(
            reason.disposition(),
            PreviewUnavailabilityDisposition::Blocked
        );
        assert!(
            runtime.visual_failures.borrow().is_empty(),
            "consumed attempt must retire its failure"
        );
        let feedback = runtime.pending_processor_feedback.borrow().clone().unwrap();
        if speculative {
            assert_eq!(feedback, current);
            assert_eq!(
                runtime.current_processor_feedback_scope.get(),
                Some(current.scope)
            );
        } else {
            assert_eq!(
                runtime.current_processor_feedback_scope.get(),
                Some(feedback.scope)
            );
            let receipt = feedback.feedback.iter().next().expect("original blocked receipt");
            assert_eq!(
                receipt.feedback.snapshot.persistent().unwrap().text(),
                "Repair the authored source parameter"
            );
        }
        assert_eq!(facts.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *facts.threads.lock().unwrap(),
            ["mondrian-preview-visual-execution"]
        );
        runtime.shutdown_and_wait();
        assert_eq!(
            facts.created.load(Ordering::SeqCst),
            facts.dropped.load(Ordering::SeqCst)
        );
    }
}

#[test]
fn processor_source_preplanned_retired_failures_release_original_observation() {
    use crate::app::preview_cpu_fallback_task::source_job_request_with_media;
    use crate::app::preview_timeline_execution::PreviewTimelineResolution;
    use crate::app::preview_visual_execution_task::{
        VisualExecutionTaskPoll, VisualExecutionTaskResult,
    };

    ensure_test_ocio_loaded();
    let (request, plan, routes, facts, old_program, mut sequence) =
        source_job_request_with_media(false, None);
    drop((request, plan, routes));
    assert!(old_program.upgrade().is_none());
    facts.blocked.store(true, Ordering::SeqCst);
    sequence.video_tracks[0].clips[0].insert_effect_node_at(1, finite_temporal_prefix());
    let mut state = AppState::new();
    state.test_set_sequence(Some(sequence));
    state.seek(72).unwrap();
    let runtime = PreviewProductionRuntime::<()>::try_new_boxed().unwrap();
    runtime.synchronize_transport_intent(state.preview_transport_intent());
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let sequence = state.active_sequence().unwrap();
    let color = sequence
        .settings
        .root_program_color_context(state.project_color_environment())
        .unwrap();
    let parent = runtime.execution.borrow().generation_cancellation();
    let generation = runtime.execution.borrow().generation();
    for phase in 0..3 {
        let PreviewTimelineResolution::Pending { preparation, .. } = runtime
            .resolve_timeline_for_test(&snapshot, &state, sequence, 72, 1, 1, color.clone(), None)
        else {
            panic!("original work must be dispatched")
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        let failed = loop {
            match runtime.visual_execution.as_ref().unwrap().try_poll() {
                VisualExecutionTaskPoll::Result(result) => match *result {
                    VisualExecutionTaskResult::Failed(failed) => break failed,
                    VisualExecutionTaskResult::Progress(_) => {}
                    _ => panic!("blocked processor must only publish its failure"),
                },
                VisualExecutionTaskPoll::Empty => {}
                VisualExecutionTaskPoll::Disconnected => panic!("worker disconnected"),
            }
            assert!(Instant::now() < deadline, "original failure did not settle");
            std::thread::sleep(Duration::from_millis(1));
        };
        assert!(failed.preparation_owner_is_active());
        assert!(failed.failure().processor_blocked().is_some());
        if phase == 1 {
            // Already queued failure loses publication authority with its owner.
            drop(preparation);
            assert!(!failed.preparation_owner_is_active());
            runtime.observe_visual_execution_failure(
                &failed,
                generation,
                Some(snapshot.transport().epoch()),
            );
        } else {
            runtime.observe_visual_execution_failure(
                &failed,
                generation,
                Some(snapshot.transport().epoch()),
            );
            assert_eq!(runtime.visual_failures.borrow().len(), 1);
            if phase == 0 {
                drop(preparation);
                runtime.pump_visual_execution_results(None);
            } else {
                assert!(matches!(
                    runtime.resolve_timeline_for_test(&snapshot, &state, sequence, 72, 1, 1, color.clone(), Some(preparation)),
                    PreviewTimelineResolution::Unavailable { reason }
                        if reason.disposition() == PreviewUnavailabilityDisposition::Blocked
                ));
            }
        }
        assert!(runtime.visual_failures.borrow().is_empty());
        assert_eq!(
            facts.calls.load(Ordering::SeqCst),
            phase + 1,
            "one invocation per original attempt"
        );
        assert_eq!(runtime.execution.borrow().generation(), generation);
        assert!(
            !parent.is_canceled(),
            "retired attempt cannot cancel its parent generation"
        );
    }
    runtime.shutdown_and_wait();
    assert_eq!(
        facts.created.load(Ordering::SeqCst),
        facts.dropped.load(Ordering::SeqCst)
    );
}

#[test]
fn processor_source_preplanned_real_media_runs_on_original_visual_worker_and_retires_waits() {
    preplanned_real_media(2, 2, &[0, 1, 2, 3, 4]);
}

#[test]
fn processor_source_preplanned_preserves_non_square_native_media_domain() {
    preplanned_real_media(4, 2, &[0]);
}

fn preplanned_real_media(source_width: u32, source_height: u32, modes: &[u8]) {
    let source_mapping = if source_width != source_height {
        [-1.0, 0.0, source_width as f32, 0.0, 1.0, 0.0]
    } else {
        [1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
    };
    use crate::app::preview_cpu_fallback_task::source_job_request_with_media;
    use crate::app::preview_timeline_execution::*;
    use crate::app::preview_visual_execution_task::*;

    ensure_test_ocio_loaded();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("prepared-history.y4m");
    let mut bytes =
        format!("YUV4MPEG2 W{source_width} H{source_height} F1:1 Ip A1:1 C444\n").into_bytes();
    let pixels = (source_width * source_height) as usize;
    for [y, u, v] in [[76u8, 84, 255], [29, 255, 107]] {
        bytes.extend_from_slice(b"FRAME\n");
        bytes.extend(std::iter::repeat_n(y, pixels));
        bytes.extend(std::iter::repeat_n(u, pixels));
        bytes.extend(std::iter::repeat_n(v, pixels));
    }
    std::fs::write(&path, bytes).unwrap();
    let path = std::fs::canonicalize(path).unwrap();
    // Success, sole preparation-owner retirement, seek/generation, caller
    // deadline and full service closure all exercise an actual live Source wait.
    for &mode in modes {
        let runtime = PreviewProductionRuntime::<()>::try_new_boxed().unwrap();
        let mut state = AppState::new();
        runtime.synchronize_transport_intent(state.preview_transport_intent());
        let epoch = state.preview_execution_snapshot(Instant::now()).transport().epoch();
        let generation = runtime.execution.borrow().generation();
        let policy = PreviewMediaWorkPolicy {
            intent: MediaPreviewRequestIntent::Current(
                mondrian_playback::MediaWorkDemandId::for_preview_generation(epoch, generation, 72),
            ),
            access_mode: PreviewDecodeAccessMode::RandomAccessStillFrame,
            deadline: None,
            demand_identity: None,
            direction: mondrian_media::PreviewPlaybackDirection::Forward,
        };
        let color = Sequence::new("prepared color")
            .settings
            .root_program_color_context(state.project_color_environment())
            .unwrap();
        let mut base = MediaPreviewKey::test_cpu(
            path.clone(),
            MediaFileFingerprint::capture(&path),
            mondrian_core::TimelineTime::ZERO,
            Resolution { width: source_width, height: source_height },
            mondrian_media::PreviewSourceColorContract::automatic(
                ColorSpace::Rec709,
                mondrian_media::DecodedVideoRange::Full,
            )
            .with_yuv_matrix_fallback(mondrian_media::DecodedVideoMatrix::Bt709),
        );
        base.preparation_intent = mondrian_renderer::RenderInputTransform::to_working(
            color.working_color_space(),
            false,
            mondrian_core::ColorEngine::mondrian_standard(),
        )
        .into();
        let historical = decoded_frame(&runtime, &base, policy).working_frame().unwrap().frame;
        assert_eq!(
            (historical.rgba_f32().width, historical.rgba_f32().height),
            (source_width, source_height)
        );
        let expected = historical.rgba_f32().data.clone();
        let mut current_key = base.clone();
        current_key.decode = base
            .decode
            .at_source_sample(mondrian_core::SourceSampleTarget::covering(
                mondrian_core::TimelineTime::new(1, 1).unwrap(),
            ))
            .unwrap();
        let current = decoded_frame(&runtime, &current_key, policy);
        assert_ne!(
            current.working_frame().unwrap().frame.rgba_f32().data,
            expected
        );
        let (request, plan, routes, facts, old_program, mut sequence) =
            source_job_request_with_media(false, Some(current));
        drop((request, plan, routes));
        assert!(old_program.upgrade().is_none());
        sequence.settings.resolution = Resolution { width: 2, height: 2 };
        sequence.video_tracks[0].clips[0].insert_effect_node_at(1, finite_temporal_prefix());
        state.test_set_sequence(Some(sequence));
        state.seek(72).unwrap();
        runtime.synchronize_transport_intent(state.preview_transport_intent());
        let snapshot = state.preview_execution_snapshot(Instant::now());
        let sequence = state.active_sequence().unwrap();
        let generation = runtime.execution.borrow().generation();
        let parent_cancel = runtime.execution.borrow().generation_cancellation();
        let cancellation = if mode == 3 {
            parent_cancel.with_deadline(Instant::now() + Duration::from_secs(2))
        } else {
            parent_cancel.clone()
        };
        runtime.frame_store.borrow_mut().clear_media_frames();
        let author = snapshot.authoring().unwrap().visual_author_snapshot_identity();
        let mut preparation = None;
        let mut work_weak = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let mut media = |demand: PreviewTimelineMediaRequest| {
                let mut key = base.clone();
                key.decode = key.decode.at_source_sample(demand.source_sample).unwrap();
                match runtime.media_frame_for_frozen_key(
                    key,
                    runtime.media_source_work_policy(&snapshot),
                    true,
                ) {
                    PreviewTimelineMediaFrame::Ready(frame) => PreviewTimelineMediaFrame::Ready(
                        frame.with_source_to_display_affine(source_mapping),
                    ),
                    other => other,
                }
            };
            let mut title = |_| panic!("no title");
            let mut execute = |work: Arc<PreparedPreviewTimelineWork>| {
                if work_weak.is_none() {
                    work_weak = Some(Arc::downgrade(&work));
                }
                runtime.execute_prepared_timeline_work(&snapshot, work)
            };
            let result = resolve_preview_timeline_with_programs_and_observer(
                PreviewTimelineFrameRequest::new(
                    sequence,
                    &[],
                    72,
                    Resolution { width: 2, height: 2 },
                    mondrian_playback::PreviewResolutionScale::Full,
                    color.clone(),
                ),
                PreviewTimelineSourceAdapters::new(&mut media, &mut title)
                    .with_execution(&mut execute),
                PreviewTimelineExecutionBinding::new(
                    &runtime.visual_programs,
                    &runtime.scratch,
                    generation,
                    cancellation.clone(),
                    author,
                    &runtime.visual_dependencies,
                    runtime
                        .heterogeneous_effect_decision
                        .get()
                        .cpu_prefix_grant()
                        .graph_execution(),
                ),
                preparation.take(),
            );
            match result {
                PreviewTimelineResolution::Pending { dependency, preparation: pending } => {
                    let executing = matches!(
                        dependency,
                        PreviewTimelinePendingDependency::Execution { .. }
                    );
                    preparation = Some(pending);
                    if executing {
                        break;
                    }
                    assert_eq!(
                        facts.calls.load(Ordering::SeqCst),
                        0,
                        "static sources precede DSP"
                    );
                    runtime.poll_playback_work(None, snapshot.transport().intent());
                }
                other => panic!("expected original preparation wait: {other:?}"),
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        // Hold the original dynamic request so completion cannot race the
        // ownership/cancellation assertions or conceal processor replay.
        loop {
            match runtime.visual_execution.as_ref().unwrap().try_poll() {
                VisualExecutionTaskPoll::Result(result) => match *result {
                    VisualExecutionTaskResult::SourceRequest(request) => {
                        runtime.observe_processor_source_request(*request);
                        break;
                    }
                    VisualExecutionTaskResult::Failed(failure) => {
                        panic!("prepared source failed: {failure:?}")
                    }
                    VisualExecutionTaskResult::PrefixReady(_) => {
                        panic!("history source was bypassed")
                    }
                    _ => {}
                },
                VisualExecutionTaskPoll::Disconnected => panic!("visual worker disconnected"),
                VisualExecutionTaskPoll::Empty => {}
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(facts.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *facts.threads.lock().unwrap(),
            ["mondrian-preview-visual-execution"]
        );
        if mode != 0 {
            if mode == 1 {
                drop(preparation.take());
            }
            if mode == 2 {
                runtime.invalidate_preview_generation();
                drop(preparation.take());
            }
            if mode == 4 {
                drop(preparation.take());
                runtime.shutdown();
            }
            loop {
                if mode != 3 {
                    runtime.pump_processor_source_requests();
                }
                if !runtime
                    .pending_processor_sources
                    .borrow()
                    .iter()
                    .any(|request| request.is_active())
                {
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            if mode == 1 || mode == 3 {
                assert!(
                    !parent_cancel.is_canceled(),
                    "child retirement/expiry cannot cancel its generation"
                );
            }
            if mode == 3 {
                assert!(
                    cancellation.is_canceled(),
                    "hold source until original caller expiry"
                );
            }
            runtime.pump_processor_source_requests();
            drop(preparation.take());
        } else {
            let output = loop {
                runtime.poll_playback_work(None, snapshot.transport().intent());
                runtime.pump_processor_source_requests();
                let mut media = |_| panic!("ready static source cannot be reacquired");
                let mut title = |_| panic!("no title");
                let mut execute = |work| runtime.execute_prepared_timeline_work(&snapshot, work);
                match resolve_preview_timeline_with_programs_and_observer(
                    PreviewTimelineFrameRequest::new(
                        sequence,
                        &[],
                        72,
                        Resolution { width: 2, height: 2 },
                        mondrian_playback::PreviewResolutionScale::Full,
                        color.clone(),
                    ),
                    PreviewTimelineSourceAdapters::new(&mut media, &mut title)
                        .with_execution(&mut execute),
                    PreviewTimelineExecutionBinding::new(
                        &runtime.visual_programs,
                        &runtime.scratch,
                        generation,
                        cancellation.clone(),
                        author,
                        &runtime.visual_dependencies,
                        runtime
                            .heterogeneous_effect_decision
                            .get()
                            .cpu_prefix_grant()
                            .graph_execution(),
                    ),
                    preparation.take(),
                ) {
                    PreviewTimelineResolution::Ready(ready) => break ready,
                    PreviewTimelineResolution::Pending { preparation: pending, .. } => {
                        preparation = Some(pending)
                    }
                    other => panic!("prepared result failed: {other:?}"),
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            };
            let ResolvedPreviewElement::Media { frame, .. } = &output.plan.elements[0] else {
                panic!("temporal media")
            };
            let actual = frame.working_frame().unwrap().frame;
            assert_eq!(
                frame.source_to_display_affine(),
                source_mapping,
                "temporal pixels preserve the source's resolved picture mapping"
            );
            assert_eq!(
                (actual.rgba_f32().width, actual.rgba_f32().height),
                (source_width, source_height)
            );
            assert_eq!(actual.rgba_f32().data.len(), expected.len());
            for (actual, source) in actual.rgba_f32().data.iter().zip(&expected) {
                for c in 0..4 {
                    assert!((actual[c] - source[c] * if c == 3 { 1.0 } else { 2.0 }).abs() < 1e-6);
                }
            }
            assert!(matches!(crate::app::preview_viewer_plan::prepare_gpu_composite_layers_with_heterogeneous_effects(
                &output.plan.elements,color.working_color_space(),output.plan.sampling,&mut runtime.scratch.borrow_mut(),
                runtime.heterogeneous_effect_decision.get().cpu_prefix_grant()).unwrap(),
                crate::app::preview_viewer_plan::PreparedPreviewViewerGpuLayers::Ordinary{..}));
            drop(output);
        }
        runtime.shutdown_and_wait();
        assert_eq!(
            facts.calls.load(Ordering::SeqCst),
            1,
            "one original DSP invocation"
        );
        assert_eq!(
            facts.created.load(Ordering::SeqCst),
            facts.dropped.load(Ordering::SeqCst)
        );
        assert!(
            work_weak.unwrap().upgrade().is_none(),
            "neither Broker nor factory retains completed preparation"
        );
    }
}

#[test]
fn processor_source_real_media_resumes_original_cpu_and_broker_jobs_without_dsp_replay() {
    use crate::app::preview_cpu_fallback_task::{
        source_job_request_with_media, PreviewCpuFallbackSubmission,
    };
    use crate::app::preview_processor_feedback::{
        PreviewProcessorFeedbackScope, PreviewProcessorWorkKey,
    };
    use crate::app::preview_visual_execution_task::*;
    use mondrian_effects::EffectFrameExtent;
    use mondrian_renderer::{
        HeterogeneousCpuPrefixBatchGrant, HeterogeneousCpuPrefixBatchItem,
        HeterogeneousCpuPrefixBatchRequest, HeterogeneousGpuResourceGrant,
    };

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("two-colors.y4m");
    // Two actual FFmpeg-demuxed frames at 1fps; no image result is synthesized.
    let mut y4m = b"YUV4MPEG2 W2 H2 F1:1 Ip A1:1 C444\n".to_vec();
    for [y, u, v] in [[76u8, 84, 255], [29, 255, 107]] {
        y4m.extend_from_slice(b"FRAME\n");
        y4m.extend([y; 4]);
        y4m.extend([u; 4]);
        y4m.extend([v; 4]);
    }
    std::fs::write(&path, y4m).unwrap();
    let path = std::fs::canonicalize(path).unwrap();

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Completion {
        Pixels,
        FeedbackViewChanged,
        MediaDeadline,
        CancelGeneration,
        Deadline,
        Shutdown,
    }
    for (mixed, completion) in [
        (false, Completion::Pixels),
        (true, Completion::Pixels),
        (false, Completion::FeedbackViewChanged),
        (true, Completion::FeedbackViewChanged),
        (false, Completion::MediaDeadline),
        (false, Completion::CancelGeneration),
        (true, Completion::CancelGeneration),
        (false, Completion::Deadline),
        (false, Completion::Shutdown),
        (true, Completion::Shutdown),
    ] {
        let runtime =
            PreviewProductionRuntime::<()>::try_new_boxed().expect("actual Runtime workers");
        let epoch = mondrian_playback::PlaybackEngine::default().snapshot().epoch;
        runtime.synchronize_transport_intent(PreviewTransportIntent::new(false, false, epoch));
        let generation = runtime.execution.borrow().generation();
        let mut policy = PreviewMediaWorkPolicy {
            intent: MediaPreviewRequestIntent::Current(
                mondrian_playback::MediaWorkDemandId::for_preview_generation(epoch, generation, 72),
            ),
            access_mode: PreviewDecodeAccessMode::RandomAccessStillFrame,
            deadline: None,
            demand_identity: None,
            direction: mondrian_media::PreviewPlaybackDirection::Forward,
        };
        let working = Sequence::new("source worker color")
            .settings
            .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
            .unwrap()
            .working_color_space();
        let mut first = MediaPreviewKey::test_cpu(
            path.clone(),
            MediaFileFingerprint::capture(&path),
            mondrian_core::TimelineTime::ZERO,
            Resolution { width: 2, height: 2 },
            mondrian_media::PreviewSourceColorContract::automatic(
                ColorSpace::Rec709,
                mondrian_media::DecodedVideoRange::Full,
            )
            .with_yuv_matrix_fallback(mondrian_media::DecodedVideoMatrix::Bt709),
        );
        first.preparation_intent = mondrian_renderer::RenderInputTransform::to_working(
            working,
            false,
            mondrian_core::ColorEngine::mondrian_standard(),
        )
        .into();
        let mut current_key = first.clone();
        current_key.decode = first
            .decode
            .at_source_sample(mondrian_core::SourceSampleTarget::covering(
                mondrian_core::TimelineTime::new(1, 1).unwrap(),
            ))
            .unwrap();
        let original = decoded_frame(&runtime, &first, policy);
        let expected = original.working_frame().unwrap().frame.rgba_f32().data.clone();
        drop(original);
        let current = decoded_frame(&runtime, &current_key, policy);
        let current_pixels = current.working_frame().unwrap().frame.rgba_f32().data.clone();
        assert_ne!(
            expected, current_pixels,
            "fixture must distinguish history from current"
        );
        assert_eq!(current.source_key(), Some(&current_key));
        runtime.frame_store.borrow_mut().clear_media_frames();

        let (mut request, plan, routes, facts, weak, _) =
            source_job_request_with_media(mixed, Some(current));
        request.generation = generation;
        request.epoch = epoch;
        request.cancellation = runtime.execution.borrow().generation_cancellation();
        if completion == Completion::Deadline {
            request.cancellation =
                request.cancellation.with_deadline(Instant::now() + Duration::from_millis(500));
        }
        if completion == Completion::MediaDeadline {
            policy.deadline = Some(Instant::now() + Duration::from_millis(500));
            assert_eq!(request.cancellation.deadline(), None);
        }
        let work = PreviewProcessorSourceWork::from_elements(policy, &request.elements);
        let visual_key = VisualExecutionTaskKey::from_complete_semantic_fingerprint([191; 32]);
        let scope = if mixed {
            PreviewProcessorFeedbackScope {
                key: Some(PreviewProcessorWorkKey::Visual(visual_key)),
                generation,
                epoch,
            }
        } else {
            PreviewProcessorFeedbackScope::scalar(&request.output_key, generation, epoch)
        };
        runtime.current_processor_feedback_scope.set(Some(scope));
        if mixed {
            let mondrian_renderer::TimelineRenderPlanElement::Media(layer) = &plan.elements[0]
            else {
                panic!("media placement")
            };
            let route = routes
                .route_for(layer.placement, &layer.effect_graph)
                .unwrap()
                .heterogeneous()
                .unwrap()
                .clone();
            let source = match &request.elements[0] {
                ResolvedPreviewElement::Media { frame, .. } => frame.working_frame().unwrap().frame,
                _ => unreachable!(),
            };
            assert_eq!(route.frame_extent(), EffectFrameExtent::new(2, 2));
            let grant = HeterogeneousCpuPrefixBatchGrant::new(
                request.effect_session_config,
                route.graph_budget(),
                1,
                1024 * 1024,
            );
            let item =
                HeterogeneousCpuPrefixBatchItem::new(7, route, source, working, layer.frame_seed);
            let payload = VisualExecutionTaskPayload::heterogeneous_cpu_prefix_batch(
                epoch,
                HeterogeneousCpuPrefixBatchRequest::new(grant, vec![item]),
                HeterogeneousGpuResourceGrant::new(1024 * 1024, 1024 * 1024, 64, 0),
                Vec::new(),
            )
            .with_processor_sources(work);
            assert!(matches!(
                runtime.visual_execution.as_ref().unwrap().submit(VisualExecutionAdmission::new(
                    visual_key,
                    generation,
                    mondrian_playback::FrameWorkPriority::Current,
                    mondrian_playback::FrameWorkClass::Playback,
                    None,
                    None,
                    payload
                )),
                mondrian_playback::FrameWorkSubmission::Queued { .. }
            ));
            drop(request);
        } else {
            request.source_work = Some(work);
            request.working_reservation =
                runtime.frame_store.borrow_mut().reserve_working_result(64);
            assert!(request.working_reservation.is_some());
            assert_eq!(
                runtime.cpu_fallback_task.as_ref().unwrap().submit(request),
                PreviewCpuFallbackSubmission::Scheduled
            );
        }
        drop((plan, routes));
        let deadline = Instant::now() + Duration::from_secs(10);
        // Hold the actual source request before serving it. The live reader must
        // have entered once and must not be replayed after decoder readiness.
        loop {
            if mixed {
                match runtime.visual_execution.as_ref().unwrap().try_poll() {
                    VisualExecutionTaskPoll::Result(result) => match *result {
                        VisualExecutionTaskResult::SourceRequest(request) => {
                            runtime.observe_processor_source_request(*request);
                            break;
                        }
                        VisualExecutionTaskResult::Failed(failed) => {
                            panic!("mixed source failed before wait: {failed:?}")
                        }
                        VisualExecutionTaskResult::PrefixReady(_) => {
                            panic!("history must use original producer")
                        }
                        _ => {}
                    },
                    VisualExecutionTaskPoll::Empty => {}
                    VisualExecutionTaskPoll::Disconnected => panic!("worker disconnected"),
                }
            } else if let Some(result) = runtime.cpu_fallback_task.as_ref().unwrap().try_poll() {
                match result {
                    PreviewCpuFallbackResult::SourceRequest(request) => {
                        runtime.observe_processor_source_request(*request);
                        break;
                    }
                    PreviewCpuFallbackResult::Failed(failed) => {
                        panic!("source failed before wait: {}", failed.reason)
                    }
                    PreviewCpuFallbackResult::Ready(_) => {
                        panic!("history must use original producer")
                    }
                    _ => {}
                }
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(facts.calls.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.pending_processor_sources.borrow().len(), 1);
        if completion == Completion::FeedbackViewChanged {
            runtime.retire_processor_feedback_view();
            assert_ne!(runtime.current_processor_feedback_scope.get(), Some(scope));
            assert_eq!(runtime.execution.borrow().generation(), generation);
            assert_eq!(runtime.transport_epoch.get(), Some(epoch));
            assert!(!runtime.execution.borrow().generation_cancellation().is_canceled());
        }
        if !matches!(
            completion,
            Completion::Pixels | Completion::FeedbackViewChanged
        ) {
            let original_generation = runtime.execution.borrow().generation_cancellation();
            if completion == Completion::CancelGeneration {
                runtime.invalidate_preview_generation();
            }
            if completion == Completion::Shutdown {
                runtime.shutdown();
                assert!(
                    runtime.pending_processor_sources.borrow().is_empty(),
                    "shutdown must retire source leases before joining media workers"
                );
            }
            if matches!(completion, Completion::Deadline | Completion::MediaDeadline) {
                // Keep the request unserved until the actual caller scope
                // expires; servicing it here would race the condition under test.
                while runtime
                    .pending_processor_sources
                    .borrow()
                    .iter()
                    .any(|request| request.is_active())
                {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            while facts.dropped.load(Ordering::SeqCst) != facts.created.load(Ordering::SeqCst)
                || weak.upgrade().is_some()
            {
                runtime.pump_processor_source_requests();
                assert!(
                    Instant::now() < deadline,
                    "canceled source wait retained its owner"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            runtime.pump_processor_source_requests();
            assert!(runtime.pending_processor_sources.borrow().is_empty());
            assert_eq!(facts.calls.load(Ordering::SeqCst), 1);
            if matches!(completion, Completion::Deadline | Completion::MediaDeadline) {
                assert!(
                    !original_generation.is_canceled(),
                    "child expiry must not cancel the parent generation"
                );
            }
            if completion == Completion::MediaDeadline {
                loop {
                    match runtime.cpu_fallback_task.as_ref().unwrap().try_poll() {
                        Some(PreviewCpuFallbackResult::Stopped(stopped)) => {
                            assert_eq!(
                                stopped.reason,
                                mondrian_effects::EffectProcessorStopReason::DeadlineExpired
                            );
                            assert_eq!(stopped.generation, generation);
                            assert_eq!(stopped.epoch, epoch);
                            assert!(stopped.demand_identity.is_none());
                            break;
                        }
                        Some(PreviewCpuFallbackResult::Failed(failed)) => {
                            panic!(
                                "deadline became a plugin/semantic failure: {}",
                                failed.reason
                            )
                        }
                        Some(PreviewCpuFallbackResult::Ready(_)) => {
                            panic!("expired Source wait published pixels")
                        }
                        _ => {}
                    }
                    assert!(Instant::now() < deadline, "missing typed deadline terminal");
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert!(runtime.cpu_fallback_failure.borrow().is_none());
            }
            let _shutdown = runtime.shutdown_and_wait();
            assert_eq!(
                facts.created.load(Ordering::SeqCst),
                facts.dropped.load(Ordering::SeqCst)
            );
            continue;
        }
        let output = loop {
            runtime.poll_finished_outcome(None);
            runtime.pump_processor_source_requests();
            if mixed {
                match runtime.visual_execution.as_ref().unwrap().try_poll() {
                    VisualExecutionTaskPoll::Result(result) => match *result {
                        VisualExecutionTaskResult::PrefixReady(ready) => {
                            let (output, lease) = ready.into_parts();
                            let (output, _, _) = output.into_heterogeneous_cpu_prefix_batch();
                            let pixels = output.completions()[0].completion().boundary_values()[0]
                                .pixels()
                                .to_vec();
                            drop(output);
                            lease.finalize_gpu(true);
                            break pixels;
                        }
                        VisualExecutionTaskResult::Failed(failed) => {
                            panic!("mixed source failed after wait: {failed:?}")
                        }
                        _ => {}
                    },
                    VisualExecutionTaskPoll::Empty => {}
                    VisualExecutionTaskPoll::Disconnected => panic!("worker disconnected"),
                }
            } else if let Some(result) = runtime.cpu_fallback_task.as_ref().unwrap().try_poll() {
                match result {
                    PreviewCpuFallbackResult::Ready(ready) => {
                        let crate::app::preview_frame_store::PreviewViewerCpuFrame::Working(frame) =
                            &ready.frame
                        else {
                            panic!("working frame")
                        };
                        break frame.rgba_f32().data.clone();
                    }
                    PreviewCpuFallbackResult::Failed(failed) => {
                        panic!("source failed after wait: {}", failed.reason)
                    }
                    _ => {}
                }
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(output.len(), expected.len());
        for (actual, original) in output.iter().zip(&expected) {
            for channel in 0..4 {
                let expected = original[channel] * if channel == 3 { 1.0 } else { 2.0 };
                assert!(
                    (actual[channel] - expected).abs() < 1e-6,
                    "history pixel mismatch: {actual:?}, {original:?}"
                );
            }
        }
        assert_eq!(
            facts.calls.load(Ordering::SeqCst),
            1,
            "waiting must not replay the processor"
        );
        assert!(runtime.pending_processor_sources.borrow().is_empty());
        let _shutdown = runtime.shutdown_and_wait();
        assert_eq!(
            facts.created.load(Ordering::SeqCst),
            facts.dropped.load(Ordering::SeqCst)
        );
        assert!(weak.upgrade().is_none());
    }
}
