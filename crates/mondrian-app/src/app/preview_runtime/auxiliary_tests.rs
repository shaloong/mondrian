use super::*;
use crate::app::preview_auxiliary::{
    reserve_auxiliary_working_set, resolved_auxiliary_working_bytes,
};
use crate::app::preview_timeline_execution::auxiliary_tests::auxiliary_matte_sequence;

#[test]
fn an_asset_backed_image_matte_renders_in_preview() {
    ensure_test_ocio_loaded();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("alpha-matte.png");
    let alpha = [0u8, 64, 128, 255];
    let image =
        image::RgbaImage::from_fn(4, 2, |x, _| image::Rgba([128, 128, 128, alpha[x as usize]]));
    image.save(&path).unwrap();
    let path = path.canonicalize().unwrap();
    let info = mondrian_media::probe_media_info(&path).unwrap();
    let picture = info.primary_video().unwrap();
    assert_eq!((picture.width, picture.height), (4, 2));
    assert!(picture.has_alpha);
    let library = AssetLibrary::open(directory.path().join("library")).unwrap();
    let asset = commit_preview_test_media(&library, path, info);
    let mut state = AppState::new();
    pin_standard_execution_resources(&mut state);
    state.test_set_asset_library(Some(library));
    state.test_set_sequence(Some(auxiliary_matte_sequence(asset)));
    state.seek(0).unwrap();
    let runtime = PreviewProductionRuntime::<()>::try_new_boxed().unwrap();
    runtime.synchronize_transport_intent(state.preview_transport_intent());
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let sequence = state.active_sequence().unwrap();
    let color = sequence
        .settings
        .root_program_color_context(state.project_color_environment())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut preparation = None;
    let resolved = loop {
        runtime.poll_playback_work(None, snapshot.transport().intent());
        match runtime.resolve_timeline_for_test(
            &snapshot,
            &state,
            sequence,
            0,
            3,
            1,
            color.clone(),
            preparation.take(),
        ) {
            PreviewTimelineResolution::Ready(output) => break output,
            PreviewTimelineResolution::Pending { preparation: next, .. } => {
                preparation = Some(next)
            }
            PreviewTimelineResolution::Unavailable { reason } => {
                panic!("real auxiliary preview failed: {}", reason.detail())
            }
            PreviewTimelineResolution::Empty => panic!("Asset matte must produce a preview plan"),
        }
        assert!(
            Instant::now() < deadline,
            "original auxiliary producer did not settle"
        );
        std::thread::sleep(Duration::from_millis(1));
    };
    let ResolvedPreviewElement::WithAuxiliary { inputs, .. } = &resolved.plan.elements[0] else {
        panic!("real source lease must reach the CPU invocation context");
    };
    let source = inputs[0].sources[0].frame.as_ref().unwrap();
    assert!(source.retains_media_residency());
    assert_eq!(
        source.cpu_image_domain().unwrap().0,
        mondrian_effects::EffectFrameExtent::new(4, 2)
    );
    assert_eq!(
        source.source_key(),
        Some(&inputs[0].sources[0].plan.validated_key)
    );
    let media_charge = source.reserved_cpu_bytes();
    assert!(
        media_charge >= 4 * 2 * 16,
        "original source charge includes its lazy working cache"
    );
    let required = resolved_auxiliary_working_bytes(&resolved.plan.elements).unwrap();
    let temporary =
        reserve_auxiliary_working_set(&mut runtime.frame_store.borrow_mut(), required).unwrap();
    let result_reservation = runtime.frame_store.borrow_mut().reserve_working_result(48).unwrap();
    let output = crate::app::preview_cpu_execution::composite_resolved_preview_working(
        3,
        1,
        &resolved.plan.elements,
        &resolved.plan.color_context,
        resolved.plan.sampling,
        &mut runtime.scratch.borrow_mut(),
        &mut mondrian_effects::EffectProcessorControl::uncancelled(),
    )
    .unwrap();
    let result_resource =
        result_reservation.finish(output.frame.rgba_f32().data.capacity() * 16).unwrap();
    // Independent pixel-centre alpha interpolation of the PNG bytes, followed
    // by red paint Over an opaque black primary. No decoder result is supplied.
    let expected = [
        64.0 / (255.0 * 6.0),
        (64.0 + 128.0) / (255.0 * 2.0),
        128.0 / (255.0 * 6.0) + 5.0 / 6.0,
    ];
    for (pixel, red) in output.frame.rgba_f32().data.iter().zip(expected) {
        for (actual, expected) in pixel.iter().zip([red, 0.0, 0.0, 1.0]) {
            assert!((actual - expected).abs() < 1e-6, "{pixel:?}");
        }
    }
    assert_eq!(
        source.reserved_cpu_bytes(),
        media_charge,
        "materializing the precharged lazy working cache does not lose its original lease"
    );
    drop(temporary);
    assert_eq!(
        runtime.frame_store.borrow().diagnostics().viewer_aggregate_reserved_bytes,
        48
    );
    drop(output);
    drop(result_resource);
    drop(resolved);
    assert_eq!(
        runtime.frame_store.borrow().diagnostics().viewer_aggregate_reserved_bytes,
        0
    );
    runtime.shutdown_and_wait();
}

#[test]
fn auxiliary_same_frame_cache_revalidates_a_missing_and_restored_physical_source() {
    auxiliary_cache_recovery(false);
}

#[test]
fn auxiliary_missing_identity_cache_revalidates_the_original_query_after_the_source_returns() {
    auxiliary_cache_recovery(true);
}

fn auxiliary_cache_recovery(preview_fallback: bool) {
    ensure_test_ocio_loaded();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cached-alpha.png");
    image::RgbaImage::from_pixel(4, 2, image::Rgba([128u8, 128, 128, 128]))
        .save(&path)
        .unwrap();
    let path = path.canonicalize().unwrap();
    let parked = path.with_file_name("parked-cached-alpha.png");
    assert_eq!(path.parent(), parked.parent());
    let info = mondrian_media::probe_media_info(&path).unwrap();
    let library = AssetLibrary::open(directory.path().join("library")).unwrap();
    let asset = commit_preview_test_media(&library, path.clone(), info);
    let mut state = AppState::new();
    pin_standard_execution_resources(&mut state);
    state.test_set_asset_library(Some(library));
    state.test_set_sequence(Some(auxiliary_matte_sequence(asset)));
    state.seek(0).unwrap();
    let runtime = PreviewProductionRuntime::<()>::try_new_boxed().unwrap();
    if preview_fallback {
        *runtime.visual_programs.borrow_mut() =
            mondrian_renderer::PreparedVisualProgramCache::for_auxiliary_preview();
    }
    runtime.synchronize_transport_intent(state.preview_transport_intent());
    let snapshot = state.preview_execution_snapshot(Instant::now());
    let sequence = state.active_sequence().unwrap();
    let author = serde_json::to_value(sequence).unwrap();
    let color = sequence
        .settings
        .root_program_color_context(state.project_color_environment())
        .unwrap();
    let key = FrameEvaluationKey {
        sequence_id: sequence.id,
        sequence_revision: sequence.revision,
        author_generation: snapshot.authoring().unwrap().author_generation(),
        frame: 0,
        width: 3,
        height: 1,
        runtime_scale: mondrian_playback::PreviewResolutionScale::Full,
        raster_requirement: PreviewRasterRequirement::Viewer,
        display_color_space: ColorSpace::Srgb,
        display_contract_identity: None,
    };
    let acquire = || {
        runtime.acquire_frame_evaluation(&snapshot, &state, sequence, 0, 3, 1, color.clone(), key)
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    let ready = || loop {
        assert!(
            Instant::now() < deadline,
            "actual cached auxiliary source did not become Ready"
        );
        runtime.poll_playback_work(None, snapshot.transport().intent());
        match acquire() {
            FrameResolutionOutcome::Ready(output) => break output,
            FrameResolutionOutcome::Pending(_) => std::thread::sleep(Duration::from_millis(2)),
            FrameResolutionOutcome::Unavailable(reason) => {
                panic!("auxiliary cache source failed: {}", reason.detail())
            }
            FrameResolutionOutcome::Empty => panic!("solid matte output is not empty"),
        }
    };
    let first = ready();
    let FrameResolutionOutcome::Ready(hit) = acquire() else {
        panic!("same source reuses Ready evaluation");
    };
    assert!(Arc::ptr_eq(&first, &hit));
    // The Windows decoder's AVIO handle does not share delete access. Retire
    // only worker-owned codec Sessions through their ordinary acknowledged
    // barrier; retain the actual resolved evaluation and decoded CPU image.
    let ResolvedPreviewElement::WithAuxiliary { inputs, .. } = &first.elements[0] else {
        panic!("real auxiliary binding");
    };
    assert_eq!(
        inputs[0].sources[0].frame.as_ref().unwrap().decoder_resource_units(),
        0
    );
    let retry = runtime.decode_residency.actionable_retry_revision();
    runtime.decode_residency.retire_all();
    runtime.jobs.interrupt_workers_for_lifecycle();
    while runtime.decode_residency.actionable_retry_revision() == retry {
        assert!(
            Instant::now() < deadline,
            "original decoder Session retirement was not acknowledged"
        );
        runtime.poll_playback_work(None, snapshot.transport().intent());
        std::thread::sleep(Duration::from_millis(2));
    }
    let retained = runtime.evaluation_working_set.borrow_mut().get(key, 0).unwrap();
    assert!(
        Arc::ptr_eq(&first, &retained),
        "decoder retirement must not erase the cache under test"
    );
    std::fs::rename(&path, &parked).unwrap();
    let missing_identity = if preview_fallback {
        let FrameResolutionOutcome::Ready(missing) = acquire() else {
            panic!("only the missing consumer becomes identity");
        };
        assert!(!Arc::ptr_eq(&first, &missing));
        assert_ne!(first.output_key, missing.output_key);
        let mut bindings = Vec::new();
        for element in missing.elements.iter() {
            crate::app::preview_auxiliary::collect_auxiliary_bindings(element, &mut bindings);
        }
        assert_eq!(
            bindings
                .iter()
                .filter_map(|binding| binding.projection_context.as_ref())
                .map(|context| context.unavailable_queries.len())
                .sum::<usize>(),
            1
        );
        assert!(
            bindings.iter().all(|binding| binding.sources.is_empty()),
            "removed slot retains its original query"
        );
        let FrameResolutionOutcome::Ready(hit) = acquire() else {
            panic!("unchanged missing fact remains reusable");
        };
        assert!(Arc::ptr_eq(&missing, &hit));
        Some(missing)
    } else {
        let FrameResolutionOutcome::Unavailable(reason) = acquire() else {
            panic!(
                "the same picture key cannot authorize pixels after its auxiliary file disappears"
            );
        };
        assert_eq!(
            reason.disposition(),
            crate::app::preview_unavailability::PreviewUnavailabilityDisposition::Blocked
        );
        None
    };
    // Windows updates the physical revision even when the same file returns.
    // Refresh the ordinary Asset probe under the same Asset ID; keep the
    // original author generation, reference, and frame evaluation key.
    std::fs::rename(&parked, &path).unwrap();
    let restored_info = mondrian_media::probe_media_info(&path).unwrap();
    let restored_asset = commit_preview_test_media(
        snapshot.authoring().unwrap().asset_library(),
        path.clone(),
        restored_info,
    );
    assert_eq!(restored_asset, asset);
    let restored = ready();
    assert!(!Arc::ptr_eq(&first, &restored));
    assert_ne!(first.output_key, restored.output_key);
    if let Some(missing) = missing_identity {
        assert!(!Arc::ptr_eq(&missing, &restored));
        assert_ne!(missing.output_key, restored.output_key);
    }
    assert_eq!(serde_json::to_value(sequence).unwrap(), author);
    runtime.shutdown_and_wait();
}
