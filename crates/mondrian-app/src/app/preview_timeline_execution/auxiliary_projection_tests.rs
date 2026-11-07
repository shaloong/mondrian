use super::*;
use crate::app::preview_auxiliary::{collect_auxiliary_bindings, PreviewAuxiliaryAssetError};
use mondrian_core::{PropertyValue, VisualSourceReference};
use mondrian_effects::{instantiate_effect_node, EffectType};

fn resolve(
    sequence: &Sequence,
    children: &[Sequence],
    programs: &RefCell<PreparedVisualProgramCache>,
    scratch: &RefCell<TimelineCompositeScratch>,
    resolver: &mut PreviewAuxiliaryAssetResolver<'_>,
    media: &mut impl FnMut(PreviewTimelineMediaRequest) -> PreviewTimelineMediaFrame,
    preparation: Option<Box<PreviewTimelinePreparation>>,
) -> PreviewTimelineResolution {
    let cancellation = ExecutionCancellationToken::new();
    let color = sequence.settings.root_program_color_context(&Default::default()).unwrap();
    resolve_preview_timeline_with_graph(
        PreviewTimelineFrameRequest::new(
            sequence,
            children,
            0,
            sequence.settings.resolution,
            PreviewResolutionScale::Full,
            color,
        ),
        PreviewTimelineSourceAdapters::new(media, &mut |_| panic!("no title"))
            .with_auxiliary_assets(resolver),
        PreviewTimelineGraph {
            programs,
            scratch,
            dependency_observer: None,
            generation: 0,
            cancellation: &cancellation,
            author_snapshot: None,
            heterogeneous_graph_budget: standalone_preview_heterogeneous_graph_budget(),
        },
        preparation,
    )
}

fn output(result: PreviewTimelineResolution) -> ResolvedPreviewTimeline {
    match result {
        PreviewTimelineResolution::Ready(output) => output,
        other => panic!("{other:?}"),
    }
}

fn pixels(output: &ResolvedPreviewTimeline) -> Vec<[f32; 4]> {
    let mut store =
        crate::app::preview_frame_store::PreviewFrameStoreAdapter::new(Default::default());
    crate::app::preview_auxiliary::admit_resolved_auxiliary_projection(
        &output.plan.elements,
        &mut store,
    )
    .unwrap();
    crate::app::preview_cpu_execution::composite_resolved_preview_working(
        3,
        1,
        &output.plan.elements,
        &output.plan.color_context,
        output.plan.sampling,
        &mut TimelineCompositeScratch::default(),
        &mut mondrian_effects::EffectProcessorControl::uncancelled(),
    )
    .unwrap()
    .frame
    .rgba_f32()
    .data
    .clone()
}

fn contexts(
    output: &ResolvedPreviewTimeline,
) -> Vec<&Arc<auxiliary_projection::PreviewAuxiliaryProjectionContext>> {
    let mut bindings = Vec::new();
    for element in &output.plan.elements {
        collect_auxiliary_bindings(element, &mut bindings);
    }
    bindings
        .into_iter()
        .filter_map(|binding| binding.projection_context.as_ref())
        .collect()
}

fn missing() -> PreviewAuxiliaryAssetError {
    PreviewAuxiliaryAssetError::SourceUnavailable(PreviewUnavailability::blocked(
        PreviewOutputStage::MediaResolution,
        "fixture source is physically unavailable",
    ))
}

fn grey_with_exposure(asset: AssetId) -> Sequence {
    let mut sequence = auxiliary_matte_sequence(asset);
    let clip = &mut sequence.video_tracks[0].clips[0];
    clip.set_solid_color(mondrian_core::Color::from_rgba8(96, 96, 96, 255)).unwrap();
    let mut exposure = instantiate_effect_node(EffectType::BasicCorrection).unwrap();
    let parameter = EffectType::BasicCorrection.parameter_id("exposure").unwrap();
    let path = exposure
        .properties
        .iter()
        .find(|(_, property)| property.descriptor.parameter_id() == &parameter)
        .unwrap()
        .0
        .to_owned();
    exposure.properties.set_static_value(&path, PropertyValue::Float(1.0)).unwrap();
    clip.add_effect_node(exposure);
    sequence
}

#[test]
fn a_missing_asset_degrades_only_its_consumer_and_preserves_later_exposure() {
    let sequence = grey_with_exposure(AssetId::new());
    let author = serde_json::to_value(&sequence).unwrap();
    let effect = &sequence.video_tracks[0].clips[0].effects[0];
    let parameter = effect.effect_type.parameter_id("source").unwrap();
    let programs = RefCell::new(PreparedVisualProgramCache::for_auxiliary_preview());
    let scratch = RefCell::new(TimelineCompositeScratch::default());
    let result = output(resolve(
        &sequence,
        &[],
        &programs,
        &scratch,
        &mut |_| Err(missing()),
        &mut |_| panic!("missing consumer owns no pixel demand"),
        None,
    ));
    let context = contexts(&result);
    assert_eq!(context.len(), 1);
    assert_eq!(context[0].notices.len(), 1);
    let notice = &context[0].notices[0];
    assert_eq!(notice.sequence_id, sequence.id);
    assert!(notice.placement.is_some());
    assert_eq!(notice.fallback.effect_id(), effect.id);
    assert_eq!(notice.fallback.parameter_id(), &parameter);
    assert_eq!(context[0].unavailable_queries.len(), 1);
    assert!(
        result.plan.render_cache_identity.is_some(),
        "a missing identity still has complete persistent semantics"
    );
    let mut reference = sequence.clone();
    reference.video_tracks[0].clips[0].set_effect_enabled(effect.id, false).unwrap();
    let strict = RefCell::new(PreparedVisualProgramCache::default());
    let expected = output(resolve(
        &reference,
        &[],
        &strict,
        &scratch,
        &mut |_| panic!("disabled effect has no source query"),
        &mut |_| panic!("no decode"),
        None,
    ));
    assert_eq!(pixels(&result), pixels(&expected));
    reference.video_tracks[0].clips[0].effects[1].is_enabled = false;
    let plain = output(resolve(
        &reference,
        &[],
        &strict,
        &scratch,
        &mut |_| panic!("no query"),
        &mut |_| panic!("no decode"),
        None,
    ));
    assert!(
        pixels(&result)[0][0] > pixels(&plain)[0][0] * 1.9,
        "later exposure executes"
    );
    assert_eq!(serde_json::to_value(&sequence).unwrap(), author);
}

#[test]
fn a_preview_unbound_consumer_has_a_notice_but_strict_preparation_remains_blocked() {
    let mut sequence = grey_with_exposure(AssetId::new());
    let effect = &mut sequence.video_tracks[0].clips[0].effects[0];
    let parameter = effect.effect_type.parameter_id("source").unwrap();
    let path = effect
        .properties
        .iter()
        .find(|(_, p)| p.descriptor.parameter_id() == &parameter)
        .unwrap()
        .0
        .to_owned();
    effect
        .properties
        .set_static_value(
            &path,
            PropertyValue::VisualSource(VisualSourceReference::Unbound),
        )
        .unwrap();
    let author = serde_json::to_value(&sequence).unwrap();
    let scratch = RefCell::new(TimelineCompositeScratch::default());
    let preview = RefCell::new(PreparedVisualProgramCache::for_auxiliary_preview());
    let result = output(resolve(
        &sequence,
        &[],
        &preview,
        &scratch,
        &mut |_| panic!("Unbound has no Asset query"),
        &mut |_| panic!("no decode"),
        None,
    ));
    let context = contexts(&result);
    assert_eq!(context[0].notices.len(), 1);
    assert!(context[0].unavailable_queries.is_empty());
    let strict = RefCell::new(PreparedVisualProgramCache::default());
    assert!(matches!(
        resolve(
            &sequence,
            &[],
            &strict,
            &scratch,
            &mut |_| panic!("no facts"),
            &mut |_| panic!("no decode"),
            None
        ),
        PreviewTimelineResolution::Unavailable { .. }
    ));
    assert_eq!(serde_json::to_value(&sequence).unwrap(), author);
}

#[test]
fn a_valid_second_consumer_keeps_its_source_and_pixels_after_the_first_is_missing() {
    let (_directory, asset) = asset_fixture(Some(Duration::from_secs(2)));
    let mut sequence = grey_with_exposure(AssetId::new());
    let valid = auxiliary_matte_sequence(asset.id).video_tracks[0].clips[0].effects[0].clone();
    sequence.video_tracks[0].clips[0].add_effect_node(valid);
    let scratch = RefCell::new(TimelineCompositeScratch::default());
    let programs = RefCell::new(PreparedVisualProgramCache::for_auxiliary_preview());
    let decodes = std::cell::Cell::new(0);
    let mut source = |request: &PreviewAuxiliaryAssetRequest| match request.reference {
        VisualSourceReference::Asset { asset_id, .. } if asset_id == asset.id => {
            Ok(planned(&asset, request))
        }
        _ => Err(missing()),
    };
    let mut media = |media: PreviewTimelineMediaRequest| {
        decodes.set(decodes.get() + 1);
        let mut intent = request(
            &asset,
            mondrian_core::TimelineTime::ZERO,
            VisualSourceBoundary::Clamp,
        );
        intent.input_color = media.input_color;
        intent.target_resolution = media.target_resolution;
        ready(&planned(&asset, &intent), 12)
    };
    let result = output(resolve(
        &sequence,
        &[],
        &programs,
        &scratch,
        &mut source,
        &mut media,
        None,
    ));
    assert_eq!(decodes.get(), 1);
    assert_eq!(contexts(&result)[0].notices.len(), 1);
    let mut reference = sequence.clone();
    reference.video_tracks[0].clips[0].effects[0].is_enabled = false;
    let strict = RefCell::new(PreparedVisualProgramCache::default());
    let expected = output(resolve(
        &reference,
        &[],
        &strict,
        &scratch,
        &mut source,
        &mut media,
        None,
    ));
    assert_eq!(pixels(&result), pixels(&expected));
}

#[test]
fn a_restored_source_rejoins_the_same_frozen_pending_attempt_before_dsp() {
    let (_a_dir, a) = asset_fixture(Some(Duration::from_secs(2)));
    let (_b_dir, b) = asset_fixture(Some(Duration::from_secs(2)));
    let mut sequence = auxiliary_matte_sequence(a.id);
    let valid = auxiliary_matte_sequence(b.id).video_tracks[0].clips[0].effects[0].clone();
    sequence.video_tracks[0].clips[0].add_effect_node(valid);
    let programs = RefCell::new(PreparedVisualProgramCache::for_auxiliary_preview());
    let scratch = RefCell::new(TimelineCompositeScratch::default());
    let wave = std::cell::Cell::new(0);
    let a_decodes = std::cell::Cell::new(0);
    let facts = std::cell::Cell::new(0);
    let mut resolver = |request: &PreviewAuxiliaryAssetRequest| {
        facts.set(facts.get() + 1);
        match request.reference {
            VisualSourceReference::Asset { asset_id, .. } if asset_id == a.id => {
                if wave.get() == 0 {
                    Err(missing())
                } else {
                    Ok(planned(&a, request))
                }
            }
            _ => Ok(planned(&b, request)),
        }
    };
    let mut media = |media: PreviewTimelineMediaRequest| {
        let asset = if media.asset_id == a.id {
            a_decodes.set(a_decodes.get() + 1);
            &a
        } else {
            if wave.get() < 2 {
                return PreviewTimelineMediaFrame::Pending {
                    wait: PreviewTimelineMediaWait::Producer,
                };
            }
            &b
        };
        let mut intent = request(
            asset,
            mondrian_core::TimelineTime::ZERO,
            VisualSourceBoundary::Clamp,
        );
        intent.input_color = media.input_color;
        intent.target_resolution = media.target_resolution;
        ready(&planned(asset, &intent), 13)
    };
    let PreviewTimelineResolution::Pending { preparation, .. } = resolve(
        &sequence,
        &[],
        &programs,
        &scratch,
        &mut resolver,
        &mut media,
        None,
    ) else {
        panic!("valid B still pending");
    };
    let original = Arc::clone(preparation.original_closure.nodes()[0].program());
    assert!(!Arc::ptr_eq(
        preparation.closure.nodes()[0].program(),
        &original
    ));
    assert_eq!(a_decodes.get(), 0);
    // Exercise the real evaluation working set's memoized-wait seam. Its
    // retained producer remains alive, but cannot hide newly restored facts.
    let mut working = crate::app::preview_runtime::EvaluationWorkingSet::new();
    let key = crate::app::preview_runtime::FrameEvaluationKey {
        sequence_id: sequence.id,
        sequence_revision: sequence.revision,
        author_generation: 0,
        frame: 0,
        width: 3,
        height: 1,
        runtime_scale: PreviewResolutionScale::Full,
        raster_requirement: PreviewRasterRequirement::Viewer,
        display_color_space: ColorSpace::Srgb,
        display_contract_identity: None,
    };
    let priority = crate::app::preview_access_mode::MediaPreviewRequestPriority::Current;
    working.insert_preparing(
        key,
        priority,
        Arc::from([]),
        Arc::from([]),
        true,
        preparation,
        0,
    );
    assert!(
        working.waiting_for(key, priority).is_none(),
        "auxiliary facts precede a retained wait hit"
    );
    let (preparation, _) = working.take_preparation(key).unwrap();
    wave.set(1);
    let PreviewTimelineResolution::Pending { preparation, .. } = resolve(
        &sequence,
        &[],
        &programs,
        &scratch,
        &mut resolver,
        &mut media,
        Some(preparation),
    ) else {
        panic!("B remains pending after A returns");
    };
    assert!(Arc::ptr_eq(
        preparation.closure.nodes()[0].program(),
        &original
    ));
    assert_eq!(a_decodes.get(), 1);
    wave.set(2);
    let result = output(resolve(
        &sequence,
        &[],
        &programs,
        &scratch,
        &mut resolver,
        &mut media,
        Some(preparation),
    ));
    assert_eq!(
        a_decodes.get(),
        1,
        "original Ready lease survives the final retry"
    );
    assert_eq!(
        facts.get(),
        6,
        "both original queries are revalidated once per wave"
    );
    assert!(contexts(&result).is_empty());
}

#[test]
fn invalid_and_stopped_source_facts_never_become_missing_identity() {
    let sequence = grey_with_exposure(AssetId::new());
    let programs = RefCell::new(PreparedVisualProgramCache::for_auxiliary_preview());
    let scratch = RefCell::new(TimelineCompositeScratch::default());
    for reason in [
        PreviewUnavailability::blocked(PreviewOutputStage::InputAdaptation, "wrong format"),
        PreviewUnavailability::stopped(PreviewOutputStage::MediaResolution, "canceled source"),
    ] {
        let disposition = reason.disposition();
        let result = resolve(
            &sequence,
            &[],
            &programs,
            &scratch,
            &mut |_| Err(PreviewAuxiliaryAssetError::Invalid(reason.clone())),
            &mut |_| panic!("no DSP"),
            None,
        );
        let PreviewTimelineResolution::Unavailable { reason } = result else {
            panic!("invalid source was swallowed");
        };
        assert_eq!(reason.disposition(), disposition);
    }
}

#[test]
fn skipping_a_definition_removes_its_other_pending_auxiliary_demand() {
    let (_directory, asset) = asset_fixture(Some(Duration::from_secs(2)));
    let sequence = auxiliary_matte_sequence_sources(&[AssetId::new(), asset.id]);
    let programs = RefCell::new(PreparedVisualProgramCache::for_auxiliary_preview());
    let scratch = RefCell::new(TimelineCompositeScratch::default());
    let mut queried = 0;
    let result = output(resolve(
        &sequence,
        &[],
        &programs,
        &scratch,
        &mut |request| {
            queried += 1;
            match request.reference {
                VisualSourceReference::Asset { asset_id, .. } if asset_id == asset.id => {
                    Ok(planned(&asset, request))
                }
                _ => Err(missing()),
            }
        },
        &mut |_| panic!("a skipped Definition cannot wait for its other input"),
        None,
    ));
    assert_eq!(queried, 2, "original definition facts are retained");
    assert_eq!(contexts(&result)[0].notices.len(), 1);
    assert_eq!(pixels(&result), vec![[0.0, 0.0, 0.0, 1.0]; 3]);
}

#[test]
fn a_nested_missing_consumer_retains_its_instance_path_and_working_pixels() {
    let child = grey_with_exposure(AssetId::new());
    let mut parent = Sequence::new("nested missing auxiliary");
    parent.settings.resolution = child.settings.resolution;
    parent.video_tracks[0]
        .add_clip(
            mondrian_timeline::clip::Clip::new_nested_sequence(
                child.id,
                mondrian_core::TimelineTime::ZERO,
                mondrian_core::TimelineTime::new(2, 1).unwrap(),
                None,
            )
            .unwrap(),
        )
        .unwrap();
    let programs = RefCell::new(PreparedVisualProgramCache::for_auxiliary_preview());
    let scratch = RefCell::new(TimelineCompositeScratch::default());
    let result = output(resolve(
        &parent,
        std::slice::from_ref(&child),
        &programs,
        &scratch,
        &mut |_| Err(missing()),
        &mut |_| panic!("no missing decode"),
        None,
    ));
    let context = contexts(&result);
    assert_eq!(context[0].notices.len(), 1);
    assert_eq!(context[0].notices[0].sequence_id, child.id);
    assert_eq!(context[0].notices[0].path.len(), 1);
    let mut reference = child.clone();
    reference.video_tracks[0].clips[0].effects[0].is_enabled = false;
    let strict = RefCell::new(PreparedVisualProgramCache::default());
    let expected = output(resolve(
        &parent,
        &[reference],
        &strict,
        &scratch,
        &mut |_| panic!("no query"),
        &mut |_| panic!("no decode"),
        None,
    ));
    assert_eq!(pixels(&result), pixels(&expected));
}

#[test]
fn projection_metadata_uses_the_existing_viewer_lease_until_the_cached_owner_retires() {
    use crate::app::preview_frame_store::{
        PreviewFrameStoreAdapter, PreviewFrameStoreAdapterConfig,
    };
    let sequence = grey_with_exposure(AssetId::new());
    let programs = RefCell::new(PreparedVisualProgramCache::for_auxiliary_preview());
    let scratch = RefCell::new(TimelineCompositeScratch::default());
    let result = output(resolve(
        &sequence,
        &[],
        &programs,
        &scratch,
        &mut |_| Err(missing()),
        &mut |_| panic!("no pixel source"),
        None,
    ));
    let context = Arc::clone(contexts(&result)[0]);
    assert!(context.validate().is_err());
    let bytes = context.bytes;
    let mut refused = PreviewFrameStoreAdapter::new(PreviewFrameStoreAdapterConfig {
        viewer_byte_budget: bytes - 1,
        ..Default::default()
    });
    assert!(context.admit(&mut refused).is_err());
    let mut store = PreviewFrameStoreAdapter::new(PreviewFrameStoreAdapterConfig {
        viewer_byte_budget: bytes,
        ..Default::default()
    });
    context.admit(&mut store).unwrap();
    context.admit(&mut store).unwrap();
    context.validate().unwrap();
    assert_eq!(store.diagnostics().viewer_aggregate_reserved_bytes, bytes);
    drop(context);
    assert_eq!(
        store.diagnostics().viewer_aggregate_reserved_bytes,
        bytes,
        "resolved cache still owns the view"
    );
    drop(result);
    assert_eq!(store.diagnostics().viewer_aggregate_reserved_bytes, 0);
}
