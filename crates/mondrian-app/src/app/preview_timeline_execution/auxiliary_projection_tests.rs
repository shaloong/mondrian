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

fn set_matte_source(
    root: &mut Sequence,
    reference: VisualSourceReference,
) -> mondrian_core::EffectId {
    let effect = &mut root.video_tracks[0].clips[0].effects[0];
    let parameter = effect.effect_type.parameter_id("source").unwrap();
    let path = effect
        .properties
        .iter()
        .find(|(_, property)| property.descriptor.parameter_id() == &parameter)
        .unwrap()
        .0
        .to_owned();
    effect
        .properties
        .set_static_value(&path, PropertyValue::VisualSource(reference))
        .unwrap();
    effect.id
}

#[test]
fn a_sequence_matte_materializes_its_own_picture_and_changes_the_preview_cache_identity() {
    use mondrian_core::{Color, TimelineTime};
    use mondrian_timeline::clip::Clip;

    let mut child = Sequence::new("Auxiliary picture");
    child.settings.resolution = Resolution { width: 6, height: 2 };
    child.video_tracks[0]
        .add_clip(
            Clip::new_solid_color(
                AssetId::new(),
                Color::from_rgba8(0, 255, 0, 255),
                TimelineTime::ZERO,
                TimelineTime::new(2, 1).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let mut root = auxiliary_matte_sequence(AssetId::new());
    set_matte_source(
        &mut root,
        VisualSourceReference::Sequence { sequence_id: child.id, retime: Default::default() },
    );

    let evaluate = |child| {
        output(resolve(
            &root,
            &[child],
            &RefCell::new(PreparedVisualProgramCache::for_auxiliary_preview()),
            &RefCell::new(TimelineCompositeScratch::default()),
            &mut |_| panic!("Sequence never queries Asset authority"),
            &mut |_| panic!("procedural child never decodes media"),
            None,
        ))
    };
    let opaque = evaluate(child.clone());
    assert_eq!(pixels(&opaque), vec![[1.0, 0.0, 0.0, 1.0]; 3]);

    child.video_tracks[0].clips[0]
        .set_solid_color(Color::from_rgba8(0, 255, 0, 0))
        .unwrap();
    let transparent = evaluate(child);
    assert_eq!(pixels(&transparent), vec![[0.0, 0.0, 0.0, 1.0]; 3]);
    assert!(opaque.plan.render_cache_identity.is_some());
    assert!(transparent.plan.render_cache_identity.is_some());
    assert_ne!(
        opaque.plan.render_cache_identity,
        transparent.plan.render_cache_identity
    );
}

#[test]
fn video_collage_preview_keeps_each_tile_on_its_exact_sequence_sample() {
    use mondrian_core::{Color, SourceSampleTarget, TimelineTime};
    let kind = EffectType::VideoCollage;
    let mut effect = instantiate_effect_node(kind.clone()).unwrap();
    let child = |name, width, height, color| {
        let mut sequence = Sequence::new(name);
        sequence.settings.resolution = Resolution { width, height };
        sequence.settings.frame_rate = mondrian_core::Rational::new(30_000, 1_001);
        sequence.video_tracks[0]
            .add_clip(
                mondrian_timeline::Clip::new_solid_color(
                    AssetId::new(),
                    color,
                    TimelineTime::ZERO,
                    TimelineTime::ONE,
                )
                .unwrap(),
            )
            .unwrap();
        sequence
    };
    let a = child("Collage A", 9, 5, Color::from_hex(0xff0000));
    let b = child("Collage B", 5, 7, Color::from_hex(0x00ff00));
    let reference = |sequence_id, time_offset| VisualSourceReference::Sequence {
        sequence_id,
        retime: mondrian_core::VisualSourceRetime { time_offset, ..Default::default() },
    };
    for (field, value) in [
        ("columns", PropertyValue::Int(2)),
        ("rows", PropertyValue::Int(1)),
        ("spacing", PropertyValue::Float(0.0)),
        ("tile_delay", PropertyValue::Float(0.125)),
        (
            "tile_source_01",
            PropertyValue::VisualSource(reference(a.id, TimelineTime::ZERO)),
        ),
        (
            "tile_source_02",
            PropertyValue::VisualSource(reference(b.id, TimelineTime::new(3, 8).unwrap())),
        ),
    ] {
        effect
            .set_static_value_by_parameter(&kind.parameter_id(field).unwrap(), value)
            .unwrap();
    }
    let root = super::auxiliary_sequence_with_effect(effect);
    let resolved = output(resolve(
        &root,
        &[a, b],
        &RefCell::new(PreparedVisualProgramCache::for_auxiliary_preview()),
        &RefCell::new(TimelineCompositeScratch::default()),
        &mut |_| panic!("procedural Sequence cannot query an Asset"),
        &mut |_| panic!("procedural Sequence cannot decode media"),
        None,
    ));
    let mut bindings = Vec::new();
    collect_auxiliary_bindings(&resolved.plan.elements[0], &mut bindings);
    let [binding] = bindings.as_slice() else {
        panic!("collage sources share one consumer");
    };
    let targets = binding
        .sources
        .iter()
        .map(|source| {
            let crate::app::preview_auxiliary::PreviewAuxiliaryEvidence::Visual(evidence) =
                &source.evidence
            else {
                panic!("each tile must retain the visual time evidence");
            };
            evidence.time.source_sample.unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        targets,
        [
            SourceSampleTarget::covering(TimelineTime::ZERO),
            SourceSampleTarget::covering(TimelineTime::new(1, 4).unwrap())
        ]
    );
    let pixels = pixels(&resolved);
    assert_eq!(pixels.len(), 3);
    for (pixel, channel) in pixels.iter().zip([0, 1, 1]) {
        assert!(
            pixel[channel] > 0.8 && (pixel[3] - 1.0).abs() < 1.0e-6,
            "{pixel:?}"
        );
        assert!((0..3).filter(|other| *other != channel).all(|other| pixel[other] < 0.6));
    }
}

#[test]
fn echo_preview_uses_each_temporal_auxiliary_context_after_seeking() {
    use mondrian_core::{Color, TimelineTime};
    let half = TimelineTime::new(1, 2).unwrap();
    let mut child = Sequence::new("animated matte");
    child.settings.resolution = Resolution { width: 5, height: 3 };
    child.settings.frame_rate = mondrian_core::Rational::new(8, 1);
    for (start, color) in [
        (TimelineTime::ZERO, Color::WHITE),
        (half, Color::TRANSPARENT),
    ] {
        child.video_tracks[0]
            .add_clip(
                mondrian_timeline::Clip::new_solid_color(AssetId::new(), color, start, half)
                    .unwrap(),
            )
            .unwrap();
    }
    let mut matte = instantiate_effect_node(EffectType::SetMatte).unwrap();
    for (field, value) in [
        (
            "matte_source",
            PropertyValue::VisualSource(VisualSourceReference::Sequence {
                sequence_id: child.id,
                retime: Default::default(),
            }),
        ),
        ("stretch", PropertyValue::Bool(true)),
    ] {
        matte
            .set_static_value_by_parameter(
                &EffectType::SetMatte.parameter_id(field).unwrap(),
                value,
            )
            .unwrap();
    }
    let mut echo = instantiate_effect_node(EffectType::Echo).unwrap();
    for (field, value) in [
        ("echo_time", PropertyValue::Float(-0.25)),
        ("operator", PropertyValue::Enum("normal".to_owned())),
        ("starting_intensity", PropertyValue::Float(0.5)),
    ] {
        echo.set_static_value_by_parameter(&EffectType::Echo.parameter_id(field).unwrap(), value)
            .unwrap();
    }
    let mut sequence = super::auxiliary_sequence_with_effect(matte);
    sequence.settings.frame_rate = child.settings.frame_rate;
    let clip = &mut sequence.video_tracks[0].clips[0];
    let matte_id = clip.effects[0].id;
    clip.set_solid_color(Color::WHITE).unwrap();
    clip.add_effect_node(echo);
    let programs = RefCell::new(PreparedVisualProgramCache::for_auxiliary_preview());
    let scratch = RefCell::new(TimelineCompositeScratch::default());
    let cancellation = ExecutionCancellationToken::new();
    let render = |frame, children: &[Sequence]| {
        let color = sequence.settings.root_program_color_context(&Default::default()).unwrap();
        output(resolve_preview_timeline_with_graph(
            PreviewTimelineFrameRequest::new(
                &sequence,
                children,
                frame,
                sequence.settings.resolution,
                PreviewResolutionScale::Full,
                color,
            ),
            PreviewTimelineSourceAdapters::new(&mut |_| panic!("no media"), &mut |_| {
                panic!("no title")
            }),
            PreviewTimelineGraph {
                programs: &programs,
                scratch: &scratch,
                dependency_observer: None,
                generation: 0,
                cancellation: &cancellation,
                author_snapshot: None,
                heterogeneous_graph_budget: standalone_preview_heterogeneous_graph_budget(),
            },
            None,
        ))
    };
    let children = std::slice::from_ref(&child);
    let mixed = pixels(&render(4, children));
    assert!(
        mixed.iter().all(|pixel| (pixel[3] - 0.5).abs() < 1e-6),
        "{mixed:?}"
    );
    let earlier = pixels(&render(2, children));
    assert!(
        earlier.iter().all(|pixel| (pixel[3] - 1.0).abs() < 1e-6),
        "{earlier:?}"
    );
    assert_eq!(pixels(&render(4, children)), mixed);
    let missing = render(4, &[]);
    assert!(pixels(&missing).iter().all(|pixel| (pixel[3] - 1.0).abs() < 1e-6));
    assert!(contexts(&missing)
        .iter()
        .flat_map(|context| context.notices.iter())
        .any(|notice| notice.fallback.effect_id() == matte_id));
    assert_eq!(pixels(&render(4, children)), mixed);
}

#[test]
fn historical_video_collage_preview_clamps_and_reuses_its_bound_prefix_after_seeking() {
    use mondrian_core::{Color, Keyframe, TimelineTime};
    let kind = EffectType::VideoCollage;
    let mut collage = instantiate_effect_node(kind.clone()).unwrap();
    for (key, value) in [
        ("columns", PropertyValue::Int(3)),
        ("rows", PropertyValue::Int(1)),
        ("spacing", PropertyValue::Float(0.0)),
        ("tile_delay", PropertyValue::Float(0.25)),
    ] {
        collage
            .set_static_value_by_parameter(&kind.parameter_id(key).unwrap(), value)
            .unwrap();
    }
    let mut prefix = instantiate_effect_node(EffectType::BasicCorrection).unwrap();
    let exposure = EffectType::BasicCorrection.property_path("exposure");
    for (time, stops) in [
        (TimelineTime::ZERO, 0.0),
        (TimelineTime::new(1, 2).unwrap(), 2.0),
    ] {
        prefix
            .properties
            .set_keyframe(
                &exposure,
                Keyframe::linear(time, PropertyValue::Float(stops)),
            )
            .unwrap();
    }
    let mut sequence = super::auxiliary_sequence_with_effect(prefix);
    sequence.settings.frame_rate = mondrian_core::Rational::new(8, 1);
    sequence.video_tracks[0].clips[0].content =
        mondrian_core::timeline_data::ClipContent::SolidColor {
            asset_id: AssetId::new(),
            color: Color::from_rgba8(40, 64, 80, 255),
        };
    sequence.video_tracks[0].clips[0].add_effect_node(collage);
    let programs = RefCell::new(PreparedVisualProgramCache::for_auxiliary_preview());
    let scratch = RefCell::new(TimelineCompositeScratch::default());
    let cancellation = ExecutionCancellationToken::new();
    let render = |frame| {
        let color = sequence.settings.root_program_color_context(&Default::default()).unwrap();
        output(resolve_preview_timeline_with_graph(
            PreviewTimelineFrameRequest::new(
                &sequence,
                &[],
                frame,
                sequence.settings.resolution,
                PreviewResolutionScale::Full,
                color,
            ),
            PreviewTimelineSourceAdapters::new(
                &mut |_| panic!("procedural clip has no media"),
                &mut |_| panic!("procedural clip has no title"),
            )
            .with_auxiliary_assets(&mut |_| panic!("no Asset auxiliary")),
            PreviewTimelineGraph {
                programs: &programs,
                scratch: &scratch,
                dependency_observer: None,
                generation: 0,
                cancellation: &cancellation,
                author_snapshot: None,
                heterogeneous_graph_budget: standalone_preview_heterogeneous_graph_budget(),
            },
            None,
        ))
    };
    let later = render(4);
    let later_pixels = pixels(&later);
    assert!(later_pixels[0][1] > later_pixels[1][1]);
    assert!(later_pixels[1][1] > later_pixels[2][1]);
    let early = render(0);
    let early_pixels = pixels(&early);
    assert_eq!(early_pixels[0], early_pixels[1]);
    assert_eq!(early_pixels[0], early_pixels[2]);
    assert_eq!(later_pixels, pixels(&render(4)));
    let mut bindings = Vec::new();
    collect_auxiliary_bindings(&later.plan.elements[0], &mut bindings);
    let crate::app::preview_auxiliary::PreviewAuxiliaryEvidence::Visual(evidence) =
        &bindings[0].sources[0].evidence
    else {
        panic!("expected a historical picture binding");
    };
    assert_eq!(
        evidence.address,
        PreparedVisualAuxiliaryAddress::InputHistory(TimelineTime::new(-1, 4).unwrap())
    );
    assert_eq!(
        evidence.time.requested_time,
        TimelineTime::new(1, 4).unwrap()
    );

    let mut bindings = Vec::new();
    collect_auxiliary_bindings(&early.plan.elements[0], &mut bindings);
    let [first, second] = bindings[0].sources.as_ref() else {
        panic!("expected both delayed slots to retain evidence");
    };
    assert!(first.evidence != second.evidence);
    let first = first.frame.as_ref().unwrap().working_frame().unwrap();
    let second = second.frame.as_ref().unwrap().working_frame().unwrap();
    assert!(std::ptr::eq(
        first.frame.rgba_f32(),
        second.frame.rgba_f32()
    ));
}

#[test]
fn a_track_matte_from_an_earlier_layer_renders_in_preview_and_falls_back_after_reordering() {
    use mondrian_core::{Color, SourceSampleTarget, TimelineTime};
    use mondrian_timeline::clip::Clip;

    let mut sequence = auxiliary_matte_sequence(AssetId::new());
    sequence.video_tracks.swap(0, 1);
    let source = Clip::new_solid_color(
        AssetId::new(),
        Color::from_rgba8(0, 255, 0, 255),
        TimelineTime::ONE,
        TimelineTime::ONE,
    )
    .unwrap();
    let clip_id = source.id;
    sequence.video_tracks[0].add_clip(source).unwrap();
    let consumer = &mut sequence.video_tracks[1].clips[0].effects[0];
    let effect_id = consumer.id;
    let parameter = consumer.effect_type.parameter_id("source").unwrap();
    let path = consumer
        .properties
        .iter()
        .find(|(_, property)| property.descriptor.parameter_id() == &parameter)
        .unwrap()
        .0
        .to_owned();
    let reference = VisualSourceReference::Clip { clip_id, retime: Default::default() };
    consumer
        .properties
        .set_static_value(&path, PropertyValue::VisualSource(reference))
        .unwrap();

    let programs = RefCell::new(PreparedVisualProgramCache::for_auxiliary_preview());
    let scratch = RefCell::new(TimelineCompositeScratch::default());
    let evaluate = |sequence: &Sequence, programs: &RefCell<PreparedVisualProgramCache>| {
        resolve(
            sequence,
            &[],
            programs,
            &scratch,
            &mut |_| panic!("Clip never queries Asset authority"),
            &mut |_| panic!("procedural Clip never decodes media"),
            None,
        )
    };
    let earlier = output(evaluate(&sequence, &programs));
    assert_eq!(pixels(&earlier), vec![[1.0, 0.0, 0.0, 1.0]; 3]);
    let mut bindings = Vec::new();
    collect_auxiliary_bindings(&earlier.plan.elements[0], &mut bindings);
    let crate::app::preview_auxiliary::PreviewAuxiliaryEvidence::Visual(evidence) =
        &bindings[0].sources[0].evidence
    else {
        panic!("expected the exact retimed Clip frame");
    };
    assert_eq!(
        evidence.address,
        PreparedVisualAuxiliaryAddress::Authored(reference)
    );
    assert_eq!(evidence.time.requested_time, TimelineTime::ZERO);
    assert_eq!(
        evidence.time.source_sample,
        Some(SourceSampleTarget::covering(TimelineTime::ONE))
    );
    assert!(evidence.time.out_of_range);

    sequence.video_tracks.swap(0, 1);
    let author = serde_json::to_value(&sequence).unwrap();
    let later = output(evaluate(&sequence, &programs));
    assert_eq!(pixels(&later), vec![[0.0, 0.0, 0.0, 1.0]; 3]);
    assert_ne!(
        earlier.plan.render_cache_identity,
        later.plan.render_cache_identity
    );
    let contexts = contexts(&later);
    let [notice] = contexts[0].notices.as_ref() else {
        panic!("expected one scoped notice")
    };
    assert_eq!(notice.fallback.effect_id(), effect_id);
    assert_eq!(notice.fallback.reference(), reference);
    assert_eq!(serde_json::to_value(&sequence).unwrap(), author);
    assert!(matches!(
        evaluate(
            &sequence,
            &RefCell::new(PreparedVisualProgramCache::default())
        ),
        PreviewTimelineResolution::Unavailable { .. }
    ));
}

#[test]
fn a_missing_sequence_reprojects_only_its_consumer_and_preserves_the_authored_reference() {
    let mut root = grey_with_exposure(AssetId::new());
    let reference = VisualSourceReference::Sequence {
        sequence_id: SequenceId::new(),
        retime: Default::default(),
    };
    let effect_id = set_matte_source(&mut root, reference);
    let authored = serde_json::to_value(&root).unwrap();
    let scratch = RefCell::new(TimelineCompositeScratch::default());
    let evaluate = |root, cache| {
        resolve(
            root,
            &[],
            &RefCell::new(cache),
            &scratch,
            &mut |_| panic!("Sequence never queries Asset authority"),
            &mut |_| panic!("procedural layer never decodes media"),
            None,
        )
    };
    let preview = output(evaluate(
        &root,
        PreparedVisualProgramCache::for_auxiliary_preview(),
    ));
    let context = contexts(&preview);
    let [notice] = context[0].notices.as_ref() else {
        panic!("expected one consumer notice")
    };
    assert!(notice.path.is_empty());
    assert_eq!(notice.fallback.effect_id(), effect_id);
    assert_eq!(notice.fallback.reference(), reference);
    assert!(context[0].unavailable_queries.is_empty());
    assert!(preview.plan.render_cache_identity.is_some());
    assert!(matches!(
        evaluate(&root, PreparedVisualProgramCache::default()),
        PreviewTimelineResolution::Unavailable { .. }
    ));
    let mut expected = root.clone();
    expected.video_tracks[0].clips[0].set_effect_enabled(effect_id, false).unwrap();
    let expected = output(evaluate(&expected, PreparedVisualProgramCache::default()));
    assert_eq!(pixels(&preview), pixels(&expected));
    assert_eq!(serde_json::to_value(&root).unwrap(), authored);
}

#[test]
fn export_matches_preview_for_a_layer_with_a_second_input() {
    use mondrian_core::{Color, TimelineTime};
    use mondrian_export::preset::{TimelineExportRange, TimelineExportSnapshot};
    use mondrian_timeline::clip::Clip;

    let mut child = Sequence::new("Parity auxiliary");
    child.settings.resolution = Resolution { width: 6, height: 2 };
    child.settings.preview.resolution_scale = 1.0;
    child.video_tracks[0]
        .add_clip(
            Clip::new_solid_color(
                AssetId::new(),
                Color { r: 0.0, g: 1.0, b: 0.0, a: 0.5 },
                TimelineTime::ZERO,
                TimelineTime::ONE,
            )
            .unwrap(),
        )
        .unwrap();
    let mut sequence = auxiliary_matte_sequence(AssetId::new());
    sequence.settings.preview.resolution_scale = 1.0;
    set_matte_source(
        &mut sequence,
        VisualSourceReference::Sequence { sequence_id: child.id, retime: Default::default() },
    );

    let preview = output(resolve(
        &sequence,
        std::slice::from_ref(&child),
        &RefCell::new(PreparedVisualProgramCache::for_auxiliary_preview()),
        &RefCell::new(TimelineCompositeScratch::default()),
        &mut |_| panic!("procedural auxiliary source has no Asset query"),
        &mut |_| panic!("procedural auxiliary source has no media decode"),
        None,
    ));
    let range = TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 1 };
    let prepared = mondrian_export::prepare_timeline_export_dependencies(
        &sequence,
        std::slice::from_ref(&child),
        range,
        false,
    )
    .unwrap();
    let resolution = sequence.settings.resolution;
    let export = TimelineExportSnapshot::captured(
        Default::default(),
        sequence,
        vec![child],
        HashMap::new(),
        range,
        prepared.execution_snapshot().clone(),
    );
    let export =
        mondrian_export::queue::export_visual_frame_validation(&export, 0, resolution).unwrap();
    let preview_pixels = pixels(&preview);
    assert_eq!(preview_pixels, vec![[0.5, 0.0, 0.0, 1.0]; 3]);
    assert_eq!(preview_pixels, export.working_frame.rgba_f32().data);
}

#[test]
fn a_cycled_sequence_reference_is_rejected_before_rendering() {
    let mut root = auxiliary_matte_sequence(AssetId::new());
    let sequence_id = root.id;
    set_matte_source(
        &mut root,
        VisualSourceReference::Sequence { sequence_id, retime: Default::default() },
    );
    let result = resolve(
        &root,
        &[],
        &RefCell::new(PreparedVisualProgramCache::for_auxiliary_preview()),
        &RefCell::new(TimelineCompositeScratch::default()),
        &mut |_| panic!("a cycle never queries Asset authority"),
        &mut |_| panic!("a cycle never requests media pixels"),
        None,
    );
    let PreviewTimelineResolution::Unavailable { reason } = result else {
        panic!("expected a Sequence cycle refusal, got {result:?}");
    };
    assert!(
        reason.detail().contains("nested Sequence cycle detected"),
        "{reason:?}"
    );
    assert!(
        reason.detail().contains(&sequence_id.to_string()),
        "{reason:?}"
    );
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
    assert!(contexts(&result).iter().all(|context| context.notices.is_empty()));
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

    let demands = collect_preview_timeline_picture_demands_with_programs(
        &sequence,
        &[],
        0,
        sequence.settings.resolution,
        PreviewResolutionScale::Full,
        PreviewRasterRequirement::Viewer,
        sequence.settings.root_program_color_context(&Default::default()).unwrap(),
        &programs,
        &scratch,
        None,
        standalone_preview_heterogeneous_graph_budget(),
        Some(&mut |request| match request.reference {
            VisualSourceReference::Asset { asset_id, .. } if asset_id == asset.id => {
                Ok(planned(&asset, request))
            }
            _ => Err(missing()),
        }),
    )
    .expect("prefetch shares the live demand projection");
    assert!(
        demands.is_empty(),
        "a skipped Definition cannot prefetch its other input"
    );
}

#[test]
fn missing_auxiliary_notices_identify_each_transition_endpoint() {
    use mondrian_core::{
        timeline_data::TimelineClipEndpointContext, TimelineTime, TimelineTimeRange,
    };
    let mut sequence = auxiliary_matte_sequence(AssetId::new());
    let mut incoming = auxiliary_matte_sequence(AssetId::new());
    let mut left = sequence.video_tracks[0].clips.pop().unwrap();
    let mut right = incoming.video_tracks[0].clips.pop().unwrap();
    let time_base = sequence.time_base();
    let time = |frame| TimelineTime::new(frame * time_base.num, time_base.den).unwrap();
    let (start, cut, end, length) = (time(0), time(2), time(3), time(2));
    left.position = start;
    left.duration = length;
    right.position = cut;
    right.duration = length;
    let (left_id, right_id) = (left.id, right.id);
    let (left_effect, right_effect) = (left.effects[0].id, right.effects[0].id);
    sequence.video_tracks[0].add_clip(left).unwrap();
    sequence.video_tracks[0].add_clip(right).unwrap();
    let transition = mondrian_timeline::VideoTransition::cross_dissolve(
        left_id,
        right_id,
        TimelineTimeRange::new(time(1), end).unwrap(),
    );
    let transition_id = transition.id;
    sequence.video_transitions.push(transition);
    sequence.validate_author_identities().unwrap();
    let programs = RefCell::new(PreparedVisualProgramCache::for_auxiliary_preview());
    let scratch = RefCell::new(TimelineCompositeScratch::default());
    let cancellation = ExecutionCancellationToken::new();
    let result = output(resolve_preview_timeline_with_graph(
        PreviewTimelineFrameRequest::new(
            &sequence,
            &[],
            2,
            sequence.settings.resolution,
            PreviewResolutionScale::Full,
            sequence.settings.root_program_color_context(&Default::default()).unwrap(),
        ),
        PreviewTimelineSourceAdapters::new(
            &mut |_| panic!("missing auxiliary endpoints have no pixel demand"),
            &mut |_| panic!("no title"),
        )
        .with_auxiliary_assets(&mut |_| Err(missing())),
        PreviewTimelineGraph {
            programs: &programs,
            scratch: &scratch,
            dependency_observer: None,
            generation: 0,
            cancellation: &cancellation,
            author_snapshot: None,
            heterogeneous_graph_budget: standalone_preview_heterogeneous_graph_budget(),
        },
        None,
    ));
    let notices = &contexts(&result)[0].notices;
    assert_eq!(notices.len(), 2);
    for (clip, effect, endpoint) in [
        (
            left_id,
            left_effect,
            TimelineClipEndpointContext::TransitionLeft { transition_id },
        ),
        (
            right_id,
            right_effect,
            TimelineClipEndpointContext::TransitionRight { transition_id },
        ),
    ] {
        let notice = notices.iter().find(|notice| notice.fallback.effect_id() == effect).unwrap();
        let placement = notice.placement.unwrap();
        assert_eq!(placement.clip_id, clip);
        assert_eq!(placement.endpoint, endpoint);
    }
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

#[test]
fn auxiliary_advisories_locate_each_shared_grade_assignment() {
    use mondrian_core::{GradeDefinition, GradeGraphNode, GradeGraphNodeId, GradeGraphNodeKind};
    use mondrian_renderer::{PreparedAuxiliaryConsumerLocation, PreparedGradeAssignmentScope};
    let mut sequence = auxiliary_matte_sequence(AssetId::new());
    let mut grade = GradeDefinition::new("Shared");
    let node = GradeGraphNodeId::new();
    let graph = &mut grade.versions[0].graph;
    let effect = sequence.video_tracks[0].clips[0].effects.remove(0);
    let effect_id = effect.id;
    graph.nodes.push(GradeGraphNode {
        id: node,
        kind: GradeGraphNodeKind::Effect { input: graph.output, effect },
    });
    graph.output = node;
    let definition = grade.id;
    sequence.grade_definitions.push(grade);
    let mut group = mondrian_timeline::grade::GradeGroup::new("Shared group");
    group.pre_clip_grade = Some(definition);
    group.post_clip_grade = Some(definition);
    let clip = &mut sequence.video_tracks[0].clips[0];
    clip.grade = Some(definition);
    clip.grade_group = Some(group.id);
    sequence.grade_groups.push(group);
    sequence.timeline_grade = Some(definition);

    let resolved = output(resolve(
        &sequence,
        &[],
        &RefCell::new(PreparedVisualProgramCache::for_auxiliary_preview()),
        &RefCell::new(TimelineCompositeScratch::default()),
        &mut |_| Err(missing()),
        &mut |_| panic!("a missing source cannot start decode"),
        None,
    ));
    let contexts = contexts(&resolved);
    let scopes: HashSet<_> = contexts[0]
        .notices
        .iter()
        .map(|notice| {
            assert_eq!(notice.fallback.effect_id(), effect_id);
            let PreparedAuxiliaryConsumerLocation::Grade {
                scope,
                definition: owner,
                node: owner_node,
            } = notice.location
            else {
                panic!("grade consumer resolved as a clip effect")
            };
            assert_eq!((owner, owner_node), (definition, node));
            assert_eq!(
                notice.placement.is_none(),
                scope == PreparedGradeAssignmentScope::Timeline
            );
            scope
        })
        .collect();
    assert_eq!(
        scopes,
        HashSet::from([
            PreparedGradeAssignmentScope::GroupPreClip,
            PreparedGradeAssignmentScope::Clip,
            PreparedGradeAssignmentScope::GroupPostClip,
            PreparedGradeAssignmentScope::Timeline,
        ])
    );
    assert_eq!(contexts[0].notices.len(), scopes.len());
}
