use super::*;
use crate::app::preview_timeline_execution::{
    PreviewTimelineMediaWait, PreviewTimelinePendingDependency,
};
use mondrian_core::{Keyframe, PropertyValue, TimeScale, TimelineTime};
use mondrian_effects::{instantiate_effect_node, EffectType};

#[test]
fn production_preview_grain_calibration_freezes_the_source_before_expanding_temporal_taps() {
    verify_production_preview_grain_calibration(false);
}

#[test]
#[ignore = "real GPU Preview denoise parity; run independently with --include-ignored"]
fn production_viewer_records_resident_grain_history() {
    verify_production_preview_grain_calibration(true);
}

fn verify_production_preview_grain_calibration(verify_viewer_gpu: bool) {
    ensure_test_ocio_loaded();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("calibration.mp4");
    std::fs::write(
        &path,
        include_bytes!("../../../../../tests/fixtures/small/h264-bframes.mp4"),
    )
    .unwrap();
    let info = mondrian_media::probe_media_info(&path).unwrap();
    let video = info.primary_video().unwrap();
    assert_eq!((video.width, video.height), (64, 64));
    let end = TimelineTime::new(
        i64::try_from(video.duration.unwrap().as_nanos()).unwrap(),
        1_000_000_000,
    )
    .unwrap();
    let library = AssetLibrary::open(directory.path().join("library")).unwrap();
    let asset_id = commit_preview_test_media(&library, path.clone(), info);
    let step = TimelineTime::new(1, 25).unwrap();
    let fixed_time = TimelineTime::new(4, 25).unwrap();
    let mut sequence = mondrian_timeline::sequence::Sequence::new("Calibration Preview");
    sequence.settings.frame_rate = Rational::new(25, 1);
    sequence.settings.resolution = Resolution { width: 64, height: 64 };
    let duration = TimelineTime::new(12, 25).unwrap();
    let make_denoise = || {
        let kind = EffectType::Denoise;
        let mut effect = instantiate_effect_node(kind.clone()).unwrap();
        for (name, value) in [
            ("sample_frame", PropertyValue::Int(4)),
            ("sample_size", PropertyValue::Int(16)),
            ("temporal_enabled", PropertyValue::Bool(true)),
            ("temporal_amount", PropertyValue::Float(90.0)),
            ("detail_amount", PropertyValue::Float(0.5)),
        ] {
            effect
                .set_static_value_by_parameter(&kind.parameter_id(name).unwrap(), value)
                .unwrap();
        }
        effect
    };
    for reverse in [false, true] {
        let mut clip = Clip::new(
            asset_id,
            if reverse {
                duration
            } else {
                TimelineTime::ZERO
            },
            duration,
        )
        .unwrap();
        clip.media_interpretation_mut().unwrap().color_space_override = Some(ColorSpace::Rec709);
        if reverse {
            clip.set_constant_source_time_map(end, TimeScale::new(-1, 1).unwrap()).unwrap();
        }
        clip.add_effect_node(make_denoise());
        sequence.video_tracks[0].add_clip(clip).unwrap();
    }
    let mut history = Clip::new(asset_id, TimelineTime::new(24, 25).unwrap(), duration).unwrap();
    history.media_interpretation_mut().unwrap().color_space_override = Some(ColorSpace::Rec709);
    let mut denoise = make_denoise();
    for (frame, amount) in [(4, 90.0), (8, 0.0)] {
        denoise
            .properties
            .set_keyframe(
                &EffectType::Denoise.property_path("temporal_amount"),
                Keyframe::linear(
                    TimelineTime::new(frame, 25).unwrap(),
                    PropertyValue::Float(amount),
                ),
            )
            .unwrap();
    }
    history.add_effect_node(denoise);
    let mut collage = instantiate_effect_node(EffectType::VideoCollage).unwrap();
    for (key, value) in [
        ("columns", PropertyValue::Int(2)),
        ("rows", PropertyValue::Int(1)),
        ("tile_delay", PropertyValue::Float(4.0 / 25.0)),
    ] {
        collage
            .set_static_value_by_parameter(
                &EffectType::VideoCollage.parameter_id(key).unwrap(),
                value,
            )
            .unwrap();
    }
    history.add_effect_node(collage.clone());
    sequence.video_tracks[0].add_clip(history).unwrap();
    let mut comparison = Clip::new(asset_id, TimelineTime::new(36, 25).unwrap(), duration).unwrap();
    comparison.media_interpretation_mut().unwrap().color_space_override = Some(ColorSpace::Rec709);
    let mut denoise = make_denoise();
    for (name, value) in [
        ("view", PropertyValue::Enum("preview".into())),
        ("temporal_enabled", PropertyValue::Bool(false)),
        ("preview_center", PropertyValue::Vec2([27.25, 32.0].into())),
        ("preview_width", PropertyValue::Float(50.0)),
        ("preview_height", PropertyValue::Float(50.0)),
    ] {
        denoise
            .set_static_value_by_parameter(&EffectType::Denoise.parameter_id(name).unwrap(), value)
            .unwrap();
    }
    comparison.add_effect_node(denoise);
    sequence.video_tracks[0].add_clip(comparison).unwrap();
    for (start, sample_frame) in [(48, 4), (60, 999)] {
        let mut samples =
            Clip::new(asset_id, TimelineTime::new(start, 25).unwrap(), duration).unwrap();
        samples.media_interpretation_mut().unwrap().color_space_override = Some(ColorSpace::Rec709);
        let mut denoise = make_denoise();
        for (name, value) in [
            ("view", PropertyValue::Enum("samples".into())),
            ("sample_frame", PropertyValue::Int(sample_frame)),
        ] {
            denoise
                .set_static_value_by_parameter(
                    &EffectType::Denoise.parameter_id(name).unwrap(),
                    value,
                )
                .unwrap();
        }
        samples.add_effect_node(denoise);
        sequence.video_tracks[0].add_clip(samples).unwrap();
    }
    let mut dual = Clip::new(asset_id, TimelineTime::new(72, 25).unwrap(), duration).unwrap();
    dual.media_interpretation_mut().unwrap().color_space_override = Some(ColorSpace::Rec709);
    dual.add_effect_node(make_denoise());
    dual.add_effect_node(collage);
    sequence.video_tracks[0].add_clip(dual).unwrap();
    let mut state = AppState::new();
    pin_standard_execution_resources(&mut state);
    state.test_set_asset_library(Some(library));
    state.test_set_sequence(Some(sequence));
    state.seek(0).unwrap();
    let runtime = PreviewProductionRuntime::<()>::try_new_boxed().unwrap();
    runtime.synchronize_transport_intent(state.preview_transport_intent());
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let sequence = state.active_sequence().unwrap();
    let context = sequence
        .settings
        .root_program_color_context(state.project_color_environment())
        .unwrap();
    let lookahead = || {
        runtime
            .future_media_frame_keys_at_scale_for_test(
                &snapshot,
                &state,
                sequence,
                0,
                mondrian_playback::PreviewResolutionScale::Quarter,
                Resolution { width: 16, height: 16 },
                context.clone(),
            )
            .expect("ready lookahead source metadata")
    };
    let warmup_keys = lookahead();
    let fixed = warmup_keys
        .iter()
        .find(|key| key.source_sample().time() == fixed_time)
        .expect("prefetch directs the decoder to the fixed source");
    assert_eq!(
        fixed.residency_resolution(),
        Resolution { width: 64, height: 64 }
    );
    assert!(warmup_keys.iter().all(|key| key.source_sample().time() != step));
    let resolve = |frame, extent: u32, preparation| {
        runtime.resolve_timeline_for_test(
            &snapshot,
            &state,
            sequence,
            frame,
            extent,
            extent,
            context.clone(),
            preparation,
        )
    };
    assert!(matches!(
        resolve(0, 16, None),
        PreviewTimelineResolution::Pending {
            dependency: PreviewTimelinePendingDependency::Media {
                asset_id: pending,
                wait: PreviewTimelineMediaWait::Producer,
            },
            preparation: None,
        } if pending == asset_id
    ));

    let resolve_ready = |frame, extent| {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut preparation = None;
        loop {
            runtime.poll_playback_work(None, snapshot.transport().intent());
            match resolve(frame, extent, preparation.take()) {
                PreviewTimelineResolution::Ready(result) => break result,
                PreviewTimelineResolution::Pending { preparation: next, .. } => preparation = next,
                other => panic!("fixed grain Preview at frame {frame} returned {other:?}"),
            }
            assert!(
                Instant::now() < deadline,
                "fixed grain Preview did not settle"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    let mut first_profile = None;
    for (frame, extent, source_count) in [(0, 16, 2), (15, 64, 3), (8, 16, 3)] {
        let result = resolve_ready(frame, extent);
        let batch = &result.semantic_trace.nodes[0].temporal_batches[0];
        assert_eq!(batch.source_samples.len(), source_count);
        if frame == 0 {
            let times: Vec<_> =
                batch.source_samples.iter().map(|source| source.effect_request.time).collect();
            assert_eq!(times, [TimelineTime::ZERO, step]);
            let planned = lookahead();
            assert!(planned.iter().any(|key| key.source_sample().time() == step));
            assert!(planned.iter().all(|key| key.source_sample().time() != fixed_time));
        }
        let cached = runtime.grain_calibration.borrow();
        let (source, observation) = cached.only_observation();
        let profile = observation.profile.as_ref().expect("usable calibration profile");
        assert_eq!(source.asset_id, asset_id);
        assert_eq!(source.source_sample().time(), fixed_time);
        assert_eq!(observation.picture.target, source.source_sample());
        assert_eq!(
            observation.picture.fingerprint,
            source.decode.source().fingerprint()
        );
        assert_eq!(
            observation.picture.video_stream_index,
            source.decode.source().video_stream_index()
        );
        assert_eq!(
            observation.picture.extent,
            mondrian_effects::EffectFrameExtent::new(64, 64)
        );
        assert_eq!(
            observation.picture.sampling,
            mondrian_core::ImageSampling::UNIT_SQUARE
        );
        assert_eq!(
            source.residency_resolution(),
            Resolution { width: 64, height: 64 }
        );
        assert_eq!(
            source.decode.source().path().canonicalize().unwrap(),
            path.canonicalize().unwrap()
        );
        assert!(profile.sigma.iter().all(|sigma| *sigma > 0.0));
        if let Some(first) = &first_profile {
            assert!(Arc::ptr_eq(first, profile));
        } else {
            first_profile = Some(Arc::clone(profile));
        }
    }

    let historical = resolve_ready(32, 16);
    let [root, delayed] = historical.semantic_trace.nodes.as_ref() else {
        panic!("expected the collage and its single delayed input");
    };
    assert_eq!(
        root.execution_resolution,
        Resolution { width: 16, height: 16 }
    );
    assert!(root.temporal_batches.is_empty());
    assert!(delayed.auxiliary_visual_sources.is_empty());
    let [batch] = delayed.temporal_batches.as_ref() else {
        panic!("delayed input must run the authored temporal denoise");
    };
    assert_eq!(batch.output_time, fixed_time);
    assert_eq!(
        batch
            .source_samples
            .iter()
            .map(|source| source.effect_request.time)
            .collect::<Vec<_>>(),
        [
            fixed_time,
            TimelineTime::new(3, 25).unwrap(),
            TimelineTime::new(5, 25).unwrap()
        ]
    );
    let decoded_grid = mondrian_effects::EffectFrameExtent::new(64, 64);
    assert_eq!(batch.frame_extent, decoded_grid);
    assert!(batch
        .source_samples
        .iter()
        .all(|source| source.effect_request.frame_extent == decoded_grid));
    let cache = runtime.grain_calibration.borrow();
    assert!(Arc::ptr_eq(
        first_profile.as_ref().unwrap(),
        cache.only_observation().1.profile.as_ref().unwrap()
    ));
    drop(cache);

    let dual = resolve_ready(80, 16);
    let [root, delayed] = dual.semantic_trace.nodes.as_ref() else {
        panic!("expected current and historical source-grid programs");
    };
    for (node, current) in [(root, 8), (delayed, 4)] {
        let [batch] = node.temporal_batches.as_ref() else {
            panic!("one temporal batch per cell")
        };
        assert_eq!(batch.frame_extent, decoded_grid);
        assert_eq!(batch.output_time, TimelineTime::new(current, 25).unwrap());
        assert_eq!(
            batch
                .source_samples
                .iter()
                .map(|source| source.effect_request.time)
                .collect::<Vec<_>>(),
            [current, current - 1, current + 1].map(|frame| TimelineTime::new(frame, 25).unwrap()),
        );
    }
    if verify_viewer_gpu {
        verify_dual_grain_viewer(&dual.plan, sequence.id);
        runtime.shutdown_and_wait();
        return;
    }

    let comparison = resolve_ready(40, 16);
    let [node] = comparison.semantic_trace.nodes.as_ref() else {
        panic!("the comparison needs no history or auxiliary picture");
    };
    let [batch] = node.temporal_batches.as_ref() else {
        panic!("the spatial comparison still requires a source-grid program");
    };
    assert_eq!(batch.source_samples.len(), 1);
    assert_eq!(batch.source_samples[0].effect_request.time, fixed_time);
    assert_eq!(batch.frame_extent, decoded_grid);
    let [crate::app::preview_viewer_plan::ResolvedPreviewElement::Media { frame, .. }] =
        comparison.plan.elements.as_slice()
    else {
        panic!("the comparison returns one working source");
    };
    let working = frame
        .working_frame_with_session(
            &mut mondrian_renderer::RenderCpuColorExecutionSession::default(),
        )
        .unwrap();
    assert_eq!(working.frame.rgba_f32().data[7 * 64 + 30], [1.0; 4]);

    let sample_pixels = |frame_index| {
        let sampled = resolve_ready(frame_index, 16);
        let [node] = sampled.semantic_trace.nodes.as_ref() else {
            panic!("single sampled Clip")
        };
        let [batch] = node.temporal_batches.as_ref() else {
            panic!("one fixed-source media batch")
        };
        assert!(batch.source_samples.is_empty());
        assert_eq!(batch.frame_extent, decoded_grid);
        let [crate::app::preview_viewer_plan::ResolvedPreviewElement::Media { frame, .. }] =
            sampled.plan.elements.as_slice()
        else {
            panic!("sampled Clip resolved to a picture")
        };
        frame
            .working_frame_with_session(
                &mut mondrian_renderer::RenderCpuColorExecutionSession::default(),
            )
            .unwrap()
            .frame
            .rgba_f32()
            .data
            .clone()
    };
    let sample = sample_pixels(49);
    assert_eq!(sample_pixels(54), sample);
    let first_window = first_profile.as_ref().unwrap().windows[0];
    let [x, y] = first_window.origin;
    assert_eq!(sample[(y * 64 + x) as usize], [1.0; 4]);
    let missing = resolve(61, 16, None);
    assert!(
        matches!(missing, PreviewTimelineResolution::Unavailable { ref reason } if reason.to_string().contains("requires admitted source frame 999")),
        "{missing:?}"
    );

    let authoring = snapshot.authoring().unwrap();
    let (generation_key, width, height, display_color_space) = runtime
        .resolve_preview_generation_contract(&snapshot, authoring, sequence)
        .unwrap();
    runtime.activate_preview_generation(generation_key);
    let evaluation_key = FrameEvaluationKey {
        sequence_id: sequence.id,
        sequence_revision: sequence.revision,
        author_generation: authoring.author_generation(),
        frame: 0,
        width,
        height,
        runtime_scale: snapshot.transport().runtime_scale(),
        raster_requirement: PreviewRasterRequirement::Viewer,
        display_color_space,
        display_contract_identity: None,
    };
    let acquire = || {
        runtime.acquire_frame_evaluation(
            &snapshot,
            &state,
            sequence,
            0,
            width,
            height,
            context.clone(),
            evaluation_key,
        )
    };
    let acquire_ready = || {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            runtime.poll_playback_work(None, snapshot.transport().intent());
            match acquire() {
                FrameResolutionOutcome::Ready(evaluation) => return evaluation,
                FrameResolutionOutcome::Pending(_) => std::thread::sleep(Duration::from_millis(2)),
                FrameResolutionOutcome::Unavailable(reason) => panic!("{}", reason.detail()),
                FrameResolutionOutcome::Empty => panic!("authored denoise has no resolved picture"),
            }
            assert!(Instant::now() < deadline, "authored denoise did not settle");
        }
    };
    let cached = acquire_ready();
    assert!(Arc::ptr_eq(&cached, &acquire_ready()));
    let calibration_revision = |evaluation: &ResolvedFrameEvaluation| {
        evaluation
            .dependencies
            .iter()
            .find_map(|dependency| match dependency {
                EvaluationDependency::GrainSource(key) => Some(key.decode.source().fingerprint()),
                EvaluationDependency::MediaProducer(_) => None,
            })
            .expect("the resolved plan retains its fixed grain source")
    };
    let old_revision = calibration_revision(&cached);
    let retry = runtime.decode_residency.actionable_retry_revision();
    runtime.decode_residency.retire_all();
    runtime.jobs.interrupt_workers_for_lifecycle();
    let retirement_deadline = Instant::now() + Duration::from_secs(15);
    while runtime.decode_residency.actionable_retry_revision() == retry {
        assert!(
            Instant::now() < retirement_deadline,
            "grain decoder retirement did not settle"
        );
        runtime.poll_playback_work(None, snapshot.transport().intent());
        std::thread::sleep(Duration::from_millis(2));
    }
    let parked = path.with_file_name("previous-calibration.mp4");
    std::fs::rename(&path, &parked).unwrap();
    std::fs::copy(&parked, &path).unwrap();
    let FrameResolutionOutcome::Unavailable(reason) = acquire() else {
        panic!("replacing the fixed source must invalidate the cached evaluation");
    };
    assert!(
        reason.detail().contains("revision changed"),
        "{}",
        reason.detail()
    );
    let replacement_info = mondrian_media::probe_media_info(&path).unwrap();
    assert_eq!(
        commit_preview_test_media(authoring.asset_library(), path, replacement_info),
        asset_id,
    );
    let replaced = acquire_ready();
    assert!(!Arc::ptr_eq(&cached, &replaced));
    assert_ne!(old_revision, calibration_revision(&replaced));

    runtime.shutdown_and_wait();
}

fn verify_dual_grain_viewer(
    plan: &crate::app::preview_timeline_execution::ResolvedPreviewPlan,
    sequence_id: mondrian_core::SequenceId,
) {
    use crate::app::preview_viewer_plan::{
        prepare_gpu_composite_layers_with_heterogeneous_effects, PreparedPreviewViewerGpuLayers,
    };
    use mondrian_effects::{EffectFrameExtent, EffectGraphExecutionBudget, EffectProcessorControl};
    use mondrian_renderer::{
        color::ProgramOutputModule, GpuColorFrameReadback, GpuColorFrameReadbackPlan, GpuContext,
        HeterogeneousCpuPrefixBatchGrant, RenderCpuColorExecutionSession, RenderMonitorAdaptation,
        ViewerGpuExecutionRequest, ViewerGpuExecutionRuntime, ViewerGpuOutputPrecision,
        ViewerSourceRect,
    };

    let extent = 16;
    let mut scratch = TimelineCompositeScratch::default();
    let cpu = crate::app::preview_cpu_execution::composite_resolved_preview_working(
        extent,
        extent,
        &plan.elements,
        &plan.color_context,
        plan.sampling,
        &mut scratch,
        &mut EffectProcessorControl::uncancelled(),
    )
    .unwrap();
    let boundary = output_boundary_from_color_context(&plan.color_context).unwrap();
    let expected = ProgramOutputModule::execute_cpu_rgba8(
        &cpu.frame,
        &boundary,
        &mut RenderCpuColorExecutionSession::new(0),
    )
    .unwrap()
    .rgba;
    let working = plan.color_context.working_color_space();
    let PreparedPreviewViewerGpuLayers::Ordinary { layers } =
        prepare_gpu_composite_layers_with_heterogeneous_effects(
            &plan.elements,
            working,
            plan.sampling,
            &mut scratch,
            HeterogeneousCpuPrefixBatchGrant::new(
                Default::default(),
                EffectGraphExecutionBudget::new(0, 0, 0, 0, 0),
                0,
                0,
            ),
            &[],
            EffectFrameExtent::new(extent, extent),
        )
        .unwrap()
    else {
        panic!("denoising should enter the ordinary Viewer device submission");
    };
    let [mondrian_renderer::ViewerGpuExecutionLayer::Source(source)] = layers.as_slice() else {
        panic!("one composited media source");
    };
    assert!(matches!(
        source.as_ref(),
        mondrian_renderer::ViewerGpuSourceLayer::Temporal(_)
    ));
    let adaptation = RenderMonitorAdaptation::new(
        boundary.output_color_space(),
        boundary.output_color_space(),
        plan.color_context.engine().clone(),
    )
    .unwrap();
    let gpu = pollster::block_on(GpuContext::new()).expect("real Preview GPU adapter");
    eprintln!(
        "Resident grain Preview adapter: {:?}",
        gpu.adapter.get_info()
    );
    let mut runtime =
        ViewerGpuExecutionRuntime::new(&gpu.adapter, &gpu.device, &gpu.queue).unwrap();
    let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    let mut record = runtime
        .record(
            &gpu.device,
            &gpu.queue,
            &mut encoder,
            ViewerGpuExecutionRequest {
                sequence_id,
                timeline_frame: 80,
                width: extent,
                height: extent,
                working_color_space: working,
                layers: &layers,
                heterogeneous_inputs: Vec::new(),
                program_output_boundary: &boundary,
                monitor_adaptation: &adaptation,
                source_rect: ViewerSourceRect::FULL,
                output_width: extent,
                output_height: extent,
                output_precision: ViewerGpuOutputPrecision::Encoded8,
                display_calibration: None,
                program_scopes: None,
                signal_monitoring: None,
            },
        )
        .unwrap();
    assert_eq!(record.residency.temporal_gpu_schedules, 2);
    assert_eq!(record.compositing_diagnostics.cpu_fallback_composites, 0);
    assert!(record.fallback_reasons.is_empty());
    let readback_plan = GpuColorFrameReadbackPlan::encoded_rgba8(record.output.clone()).unwrap();
    let presentation = runtime.take_presentation_output(&mut record).unwrap();
    let readback = GpuColorFrameReadback::record_presentation_copy(
        &gpu.device,
        &mut encoder,
        &readback_plan,
        &presentation,
    )
    .unwrap();
    let submission = gpu.queue.submit([encoder.finish()]);
    let _submitted = record.assert_adapter_submission(submission.clone());
    let (send, receive) = std::sync::mpsc::channel();
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, move |result| {
        send.send(result).unwrap()
    });
    gpu.device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(Duration::from_secs(15)),
        })
        .unwrap();
    receive.recv_timeout(Duration::from_secs(15)).unwrap().unwrap();
    let mapped = slice.get_mapped_range().unwrap();
    let actual = readback_plan.unpack_mapped_rgba8(&mapped).unwrap();
    assert_eq!(expected.len(), actual.rgba().len());
    let (channel, max_error) = expected
        .iter()
        .zip(actual.rgba())
        .enumerate()
        .map(|(channel, (cpu, gpu))| (channel, cpu.abs_diff(*gpu)))
        .max_by_key(|(_, error)| *error)
        .unwrap();
    let pixel = channel / 4 * 4;
    eprintln!(
        "Resident grain Preview maximum error: {max_error} RGBA8 steps at [{}, {}], CPU {:?}, GPU {:?}",
        channel / 4 % extent as usize,
        channel / 4 / extent as usize,
        &expected[pixel..pixel + 4],
        &actual.rgba()[pixel..pixel + 4]
    );
    assert!(max_error <= 1);
}
