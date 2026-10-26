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
        CancelGeneration,
        Deadline,
        Shutdown,
    }
    for (mixed, completion) in [
        (false, Completion::Pixels),
        (true, Completion::Pixels),
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
        let policy = PreviewMediaWorkPolicy {
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

        let (mut request, plan, routes, facts, weak) =
            source_job_request_with_media(mixed, Some(current));
        request.generation = generation;
        request.epoch = epoch;
        request.cancellation = runtime.execution.borrow().generation_cancellation();
        if completion == Completion::Deadline {
            request.cancellation =
                request.cancellation.with_deadline(Instant::now() + Duration::from_millis(500));
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
        if completion != Completion::Pixels {
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
            if completion == Completion::Deadline {
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
            if completion == Completion::Deadline {
                assert!(
                    !original_generation.is_canceled(),
                    "child expiry must not cancel the parent generation"
                );
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
