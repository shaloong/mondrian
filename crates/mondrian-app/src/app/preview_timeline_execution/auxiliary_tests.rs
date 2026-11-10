use super::*;
use crate::app::preview_auxiliary::{
    resolve_preview_auxiliary_asset,
    tests::{asset_fixture, request},
};
use crate::app::preview_execution::PreviewSemanticIdentity;
use crate::app::preview_hardware_admission::PreviewHardwareDecodeAdmissionState;
use mondrian_core::{ColorFrameAlpha, VisualSourceBoundary};
use std::time::Duration;

#[path = "auxiliary_projection_tests.rs"]
mod projection_tests;

fn planned(
    asset: &mondrian_assets::AssetRecord,
    request: &PreviewAuxiliaryAssetRequest,
) -> PreviewAuxiliaryAssetPlan {
    resolve_preview_auxiliary_asset(
        request,
        asset,
        PreviewHardwareDecodeAdmissionState::default(),
    )
    .unwrap()
}

fn ready(plan: &PreviewAuxiliaryAssetPlan, salt: u8) -> PreviewTimelineMediaFrame {
    PreviewTimelineMediaFrame::Ready(
        MediaPreviewFrame::from_working(
            CpuColorFrame::working(WorkingRgbaF32Frame {
                width: 4,
                height: 2,
                sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
                alpha: ColorFrameAlpha::StraightCoverage,
                color_space: plan
                    .media_request
                    .as_ref()
                    .expect("Ready source has a decode request")
                    .input_color
                    .working_color_space,
                data: vec![[0.0, 0.0, 0.0, 0.25]; 8],
            }),
            Resolution { width: 4, height: 2 },
            PreviewSemanticIdentity::from_test_fingerprint([salt; 32]),
            FramePresentationQuality::Ready,
            PreviewDecodeExecutionSummary::default(),
        )
        .with_render_cache_source_fingerprint(Some([salt; 32]))
        .with_source_key(plan.validated_key.clone()),
    )
}

pub(crate) fn auxiliary_matte_sequence(asset_id: AssetId) -> Sequence {
    auxiliary_matte_sequence_sources(&[asset_id])
}

fn auxiliary_matte_sequence_sources(asset_ids: &[AssetId]) -> Sequence {
    use mondrian_core::{
        PropertyBag, PropertyDescriptor, PropertyValue, VisualSourceReference, VisualSourceRetime,
    };
    use mondrian_effects::*;
    let kind = EffectType::External(format!(
        "test.preview.asset-matte.{}",
        mondrian_core::EffectId::new()
    ));
    let mut properties = PropertyBag::default();
    let mut parameters = Vec::new();
    for (index, &asset_id) in asset_ids.iter().enumerate() {
        let name = if index == 0 {
            "source".to_owned()
        } else {
            format!("source_{index}")
        };
        let parameter = kind.parameter_id(&name).unwrap();
        properties.define(
            PropertyDescriptor::new(
                name,
                "Picture source",
                PropertyValue::VisualSource(VisualSourceReference::Asset {
                    asset_id,
                    retime: VisualSourceRetime::default(),
                }),
            )
            .with_parameter_id(parameter.clone()),
        );
        parameters.push(parameter);
    }
    register_effect_definition(
        EffectDefinition::new(
            kind.key(),
            "Asset matte test",
            properties,
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            roi_propagation: EffectRoiPropagation::PixelLocal,
            ..EffectExecutionContract::IDENTITY
        })
        .with_branching_graph_builder(Arc::new(move |effect, _, builder| {
            for parameter in &parameters {
                let input = builder.current_output();
                let auxiliary = builder.append_visual_source_input(
                    effect,
                    parameter,
                    EffectValueFormat::new(
                        EffectWorkingPrecision::Float32,
                        EffectColorDomain::SceneLinearRgb,
                        ColorFrameAlpha::StraightCoverage,
                    ),
                    EffectColorDomainContract::SCENE_LINEAR,
                )?;
                let output = builder.add_auxiliary_effect_with_role(
                    input,
                    auxiliary,
                    EffectRenderOp::Fill {
                        grade: FillGrade::new(0.0, 0.0, false, [1.0, 0.0, 0.0], 1.0).unwrap(),
                    },
                    AuxiliaryRole::Matte,
                );
                builder.set_current_output(output);
            }
            Ok(())
        })),
    )
    .unwrap();
    auxiliary_sequence_with_effect(instantiate_effect_node(kind).unwrap())
}

fn auxiliary_sequence_with_effect(effect: mondrian_effects::EffectNode) -> Sequence {
    let mut sequence = Sequence::new("Asset-backed auxiliary host test");
    sequence.settings.resolution = Resolution { width: 3, height: 1 };
    let mut clip = mondrian_timeline::clip::Clip::new_solid_color(
        AssetId::new(),
        mondrian_core::Color::from_rgba8(0, 0, 0, 255),
        mondrian_core::TimelineTime::ZERO,
        mondrian_core::TimelineTime::new(2, 1).unwrap(),
    )
    .unwrap();
    clip.add_effect_node(effect);
    sequence.video_tracks[0].add_clip(clip).unwrap();
    sequence
}

pub(crate) fn resolve_auxiliary_fixture(
    sequence: &Sequence,
    asset: &mondrian_assets::AssetRecord,
) -> ResolvedPreviewTimeline {
    let programs = RefCell::new(PreparedVisualProgramCache::default());
    let scratch = RefCell::new(TimelineCompositeScratch::default());
    let cancellation = ExecutionCancellationToken::new();
    let mut selected = None;
    let mut resolver = |request: &PreviewAuxiliaryAssetRequest| {
        let plan = planned(asset, request);
        selected = Some(plan.clone());
        Ok(plan)
    };
    // The source wave resolves facts before polling its original media Adapter.
    // Derive the Ready key again here without opening a second source authority.
    let mut media = |media: PreviewTimelineMediaRequest| {
        assert!(media.cpu_working_required);
        let mut intent = request(
            asset,
            mondrian_core::TimelineTime::ZERO,
            VisualSourceBoundary::Clamp,
        );
        intent.input_color = media.input_color.clone();
        intent.target_resolution = media.target_resolution;
        ready(&planned(asset, &intent), 7)
    };
    let context = sequence
        .settings
        .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
        .unwrap();
    let result = resolve_preview_timeline_with_graph(
        PreviewTimelineFrameRequest::new(
            sequence,
            &[],
            0,
            sequence.settings.resolution,
            PreviewResolutionScale::Full,
            context,
        ),
        PreviewTimelineSourceAdapters::new(&mut media, &mut |_| panic!("no title"))
            .with_auxiliary_assets(&mut resolver),
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
    );
    assert!(selected.is_some(), "auxiliary source facts were observed");
    match result {
        PreviewTimelineResolution::Ready(output) => output,
        result => panic!("auxiliary host fixture did not resolve: {result:?}"),
    }
}

#[test]
fn auxiliary_asset_host_binding_reaches_cpu_on_the_actual_primary_grid() {
    let (_root, asset) = asset_fixture(Some(Duration::from_secs(2)));
    let sequence = auxiliary_matte_sequence(asset.id);
    let context = sequence
        .settings
        .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
        .unwrap();
    let demands = collect_preview_timeline_picture_demands(
        &sequence,
        &[],
        0,
        sequence.settings.resolution,
        PreviewResolutionScale::Full,
        context,
    )
    .unwrap();
    assert!(matches!(
        demands.as_slice(),
        [PreviewTimelinePictureDemand::Auxiliary(_)]
    ));
    let resolved = resolve_auxiliary_fixture(&sequence, &asset);
    let ResolvedPreviewElement::WithAuxiliary { inputs, .. } = &resolved.plan.elements[0] else {
        panic!("resolved element must retain its exact auxiliary owner");
    };
    assert_eq!(inputs[0].primary_extent, EffectFrameExtent::new(3, 1));
    assert_eq!(
        inputs[0].sources[0].frame.as_ref().unwrap().cpu_image_domain().unwrap().0,
        EffectFrameExtent::new(4, 2),
        "target is a hint; the original decode keeps its actual extent"
    );
    let output = crate::app::preview_cpu_execution::composite_resolved_preview_working(
        3,
        1,
        &resolved.plan.elements,
        &resolved.plan.color_context,
        resolved.plan.sampling,
        &mut TimelineCompositeScratch::default(),
        &mut mondrian_effects::EffectProcessorControl::uncancelled(),
    )
    .unwrap();
    for pixel in &output.frame.rgba_f32().data {
        for (actual, expected) in pixel.iter().zip([0.25, 0.0, 0.0, 1.0]) {
            assert!((actual - expected).abs() < 1e-6, "{pixel:?}");
        }
    }
    let mut scratch = TimelineCompositeScratch::default();
    scratch.reconfigure_cpu_working_set(mondrian_renderer::TimelineCpuWorkingSetGrant {
        max_active_bytes: 1,
        max_retained_scratch_bytes: u64::MAX,
    });
    assert!(matches!(
        crate::app::preview_cpu_execution::composite_resolved_preview_working(
            3,
            1,
            &resolved.plan.elements,
            &resolved.plan.color_context,
            resolved.plan.sampling,
            &mut scratch,
            &mut mondrian_effects::EffectProcessorControl::uncancelled()
        ),
        Err(
            crate::app::preview_cpu_execution::PreviewCpuExecutionError::TimelineComposite(
                mondrian_renderer::TimelineCompositeError::CpuWorkingSet(
                    mondrian_renderer::TimelineCpuWorkingSetError::ActiveGrantExceeded { .. }
                )
            )
        )
    ));
}

#[test]
fn preview_channel_combiner_reads_the_selected_source_on_the_actual_grid() {
    use mondrian_core::{PropertyValue, VisualSourceReference};
    use mondrian_effects::{instantiate_effect_node, EffectType};

    let (_root, asset) = asset_fixture(Some(Duration::from_secs(2)));
    let kind = EffectType::ChannelCombiner;
    let mut effect = instantiate_effect_node(kind.clone()).unwrap();
    for (key, value) in [
        ("use_second_layer", PropertyValue::Bool(true)),
        (
            "second_layer_source",
            PropertyValue::VisualSource(VisualSourceReference::Asset {
                asset_id: asset.id,
                retime: Default::default(),
            }),
        ),
    ] {
        effect
            .set_static_value_by_parameter(&kind.parameter_id(key).unwrap(), value)
            .unwrap();
    }
    let sequence = auxiliary_sequence_with_effect(effect);
    let resolved = resolve_auxiliary_fixture(&sequence, &asset);
    let output = crate::app::preview_cpu_execution::composite_resolved_preview_working(
        3,
        1,
        &resolved.plan.elements,
        &resolved.plan.color_context,
        resolved.plan.sampling,
        &mut TimelineCompositeScratch::default(),
        &mut mondrian_effects::EffectProcessorControl::uncancelled(),
    )
    .unwrap();
    let pixels = &output.frame.rgba_f32().data;
    assert_eq!(pixels.len(), 3);
    for pixel in pixels {
        for (actual, expected) in pixel.iter().zip([0.25, 0.25, 0.0, 1.0]) {
            assert!((actual - expected).abs() < 1e-6, "{pixel:?}");
        }
    }
}

#[test]
fn preview_displacement_map_projects_the_expanded_output_to_the_original_layer() {
    use crate::app::preview_auxiliary::prepare_viewer_auxiliary_rasters;
    use crate::app::preview_viewer_plan::{
        prepare_gpu_composite_layers_with_heterogeneous_effects, PreparedPreviewViewerGpuLayers,
    };
    use mondrian_core::{PropertyValue, VisualSourceReference};
    use mondrian_effects::{
        instantiate_effect_node, EffectExecutionSessionConfig, EffectGraphExecutionBudget,
        EffectType,
    };
    use mondrian_renderer::{
        GpuVisualAuxiliaryPicture, HeterogeneousCpuPrefixBatchGrant, SpatialChainPreference,
        ViewerGpuExecutionLayer, ViewerGpuSourceLayer,
    };

    let (_root, asset) = asset_fixture(Some(Duration::from_secs(2)));
    let kind = EffectType::DisplacementMap;
    let mut effect = instantiate_effect_node(kind.clone()).unwrap();
    for (key, value) in [
        (
            "map_source",
            PropertyValue::VisualSource(VisualSourceReference::Asset {
                asset_id: asset.id,
                retime: Default::default(),
            }),
        ),
        ("horizontal_channel", PropertyValue::Enum("alpha".into())),
        ("horizontal_amount", PropertyValue::Float(1.0)),
        ("vertical_amount", PropertyValue::Float(0.0)),
    ] {
        effect
            .set_static_value_by_parameter(&kind.parameter_id(key).unwrap(), value)
            .unwrap();
    }
    let sequence = auxiliary_sequence_with_effect(effect);
    let resolved = resolve_auxiliary_fixture(&sequence, &asset);
    let output = crate::app::preview_cpu_execution::composite_resolved_preview_working(
        3,
        1,
        &resolved.plan.elements,
        &resolved.plan.color_context,
        resolved.plan.sampling,
        &mut TimelineCompositeScratch::default(),
        &mut mondrian_effects::EffectProcessorControl::uncancelled(),
    )
    .unwrap();
    let pixels = &output.frame.rgba_f32().data;
    assert_eq!(pixels.len(), 3);
    for (pixel, alpha) in pixels.iter().zip([0.5, 1.0, 1.0]) {
        assert!((pixel[3] - alpha).abs() < 1e-6, "{pixel:?}");
    }

    let mut scratch = TimelineCompositeScratch::default();
    scratch.set_spatial_chain_preference(SpatialChainPreference::ForceGpu);
    let auxiliary = prepare_viewer_auxiliary_rasters(&resolved.plan.elements, &mut scratch)
        .expect("register the same Asset for Viewer GPU");
    let plan = prepare_gpu_composite_layers_with_heterogeneous_effects(
        &resolved.plan.elements,
        resolved.plan.color_context.working_color_space(),
        resolved.plan.sampling,
        &mut scratch,
        HeterogeneousCpuPrefixBatchGrant::new(
            EffectExecutionSessionConfig::uncached(0),
            EffectGraphExecutionBudget::new(0, 0, 0, 0, 0),
            0,
            0,
        ),
        &auxiliary,
        mondrian_effects::EffectFrameExtent::new(3, 1),
    )
    .expect("Viewer GPU spatial layer plan");
    let PreparedPreviewViewerGpuLayers::Ordinary { layers } = plan else {
        panic!("the frozen Asset requires a single GPU route");
    };
    let [ViewerGpuExecutionLayer::Source(source)] = layers.as_slice() else {
        panic!("expected the source layer");
    };
    let ViewerGpuSourceLayer::SolidColor { spatial_plan: Some(plan), .. } = source.as_ref() else {
        panic!("expected a solid with a spatial pass");
    };
    let [map] = plan.auxiliary.as_slice() else {
        panic!("expected one Map slot");
    };
    let GpuVisualAuxiliaryPicture::Cpu(map) = &map.picture else {
        panic!("expected actual Asset working pixels");
    };
    assert_eq!((map.descriptor().width, map.descriptor().height), (4, 2));
    assert!(map.rgba_f32().data.iter().all(|pixel| pixel[3] == 0.25));
}

#[test]
fn two_auxiliary_slots_share_one_registered_asset_plane_and_working_charge() {
    use crate::app::preview_auxiliary::{
        prepare_auxiliary_rasters, resolved_auxiliary_working_bytes,
    };

    let (_root, asset) = asset_fixture(Some(Duration::from_secs(2)));
    let sequence = auxiliary_matte_sequence_sources(&[asset.id, asset.id]);
    let resolved = resolve_auxiliary_fixture(&sequence, &asset);
    let ResolvedPreviewElement::WithAuxiliary { inputs, .. } = &resolved.plan.elements[0] else {
        panic!("resolved element must retain the two author slots");
    };
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].sources.len(), 2);
    assert_eq!(
        resolved_auxiliary_working_bytes(&resolved.plan.elements).unwrap(),
        2 * 4 * 2 * 16 + 3 * 16,
    );
    let mut scratch = TimelineCompositeScratch::default();
    let mut control = mondrian_effects::EffectProcessorControl::uncancelled();
    let registered = prepare_auxiliary_rasters(
        &[&inputs[0]],
        &mut scratch,
        &mut control,
        &mut Vec::new(),
        &mut Default::default(),
    )
    .unwrap();
    let rasters = registered[0].inputs();
    assert_ne!(rasters[0].slot, rasters[1].slot);
    assert_eq!(
        rasters[0].raster.frame.extent(),
        EffectFrameExtent::new(3, 1)
    );
    assert!(std::ptr::eq(
        rasters[0].raster.frame.pixels(),
        rasters[1].raster.frame.pixels(),
    ));
    let output = crate::app::preview_cpu_execution::composite_resolved_preview_working(
        3,
        1,
        &resolved.plan.elements,
        &resolved.plan.color_context,
        resolved.plan.sampling,
        &mut scratch,
        &mut control,
    )
    .unwrap();
    assert_eq!(
        output.frame.rgba_f32().data,
        vec![[0.4375, 0.0, 0.0, 1.0]; 3]
    );
}

#[test]
fn auxiliary_host_pixels_require_a_shared_residency_charge_that_retires_with_the_owner() {
    use crate::app::preview_auxiliary::{
        reserve_auxiliary_working_set, resolved_auxiliary_working_bytes,
        validate_auxiliary_working_reservation,
    };
    use crate::app::preview_frame_store::{
        PreviewFrameStoreAdapter, PreviewFrameStoreAdapterConfig,
    };
    let (_root, asset) = asset_fixture(Some(Duration::from_secs(2)));
    let sequence = auxiliary_matte_sequence(asset.id);
    let resolved = resolve_auxiliary_fixture(&sequence, &asset);
    let required = resolved_auxiliary_working_bytes(&resolved.plan.elements).unwrap();
    assert_eq!(required, 2 * 4 * 2 * 16 + 3 * 16);
    assert!(validate_auxiliary_working_reservation(required, None).is_err());
    let mut refused = PreviewFrameStoreAdapter::new(PreviewFrameStoreAdapterConfig {
        viewer_byte_budget: required - 1,
        ..Default::default()
    });
    assert!(reserve_auxiliary_working_set(&mut refused, required).is_err());
    assert_eq!(refused.diagnostics().viewer_aggregate_reserved_bytes, 0);
    let mut admitted = PreviewFrameStoreAdapter::new(PreviewFrameStoreAdapterConfig {
        viewer_byte_budget: required,
        ..Default::default()
    });
    let reservation = reserve_auxiliary_working_set(&mut admitted, required).unwrap();
    assert_eq!(
        admitted.diagnostics().viewer_aggregate_reserved_bytes,
        required
    );
    validate_auxiliary_working_reservation(required, reservation.as_ref()).unwrap();
    assert!(validate_auxiliary_working_reservation(required + 1, reservation.as_ref()).is_err());
    assert!(reserve_auxiliary_working_set(&mut admitted, 1).is_err());
    drop(reservation);
    assert_eq!(admitted.diagnostics().viewer_aggregate_reserved_bytes, 0);
    assert!(reserve_auxiliary_working_set(&mut admitted, required).is_ok());
}

#[test]
fn auxiliary_nested_work_reuses_its_reservation_and_keeps_it_until_execution_retires() {
    use crate::app::preview_frame_store::{
        PreviewFrameStoreAdapter, PreviewFrameStoreAdapterConfig,
    };
    let (_root, asset) = asset_fixture(Some(Duration::from_secs(2)));
    let child = auxiliary_matte_sequence_sources(&[asset.id, asset.id]);
    let mut parent = Sequence::new("nested auxiliary residency");
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
    let programs = RefCell::new(PreparedVisualProgramCache::default());
    let scratch = RefCell::new(TimelineCompositeScratch::default());
    let cancellation = ExecutionCancellationToken::new();
    let mut resolver = |request: &PreviewAuxiliaryAssetRequest| Ok(planned(&asset, request));
    let mut media = |media: PreviewTimelineMediaRequest| {
        let mut intent = request(
            &asset,
            mondrian_core::TimelineTime::ZERO,
            VisualSourceBoundary::Clamp,
        );
        intent.target_resolution = media.target_resolution;
        intent.input_color = media.input_color;
        ready(&planned(&asset, &intent), 9)
    };
    let mut captured = None;
    let mut execute = |work: Arc<PreparedPreviewTimelineWork>| {
        captured = Some(work);
        PreviewTimelineWorkPoll::Pending
    };
    let context = parent
        .settings
        .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
        .unwrap();
    let pending = resolve_preview_timeline_with_graph(
        PreviewTimelineFrameRequest::new(
            &parent,
            std::slice::from_ref(&child),
            0,
            parent.settings.resolution,
            PreviewResolutionScale::Full,
            context,
        ),
        PreviewTimelineSourceAdapters::new(&mut media, &mut |_| panic!("no title"))
            .with_auxiliary_assets(&mut resolver)
            .with_execution(&mut execute),
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
    );
    assert!(matches!(pending, PreviewTimelineResolution::Pending { .. }));
    let work = captured.take().expect("original nested execution was dispatched once");
    assert!(work.is_active());
    let child_node =
        work.closure.nodes().iter().find(|node| node.sequence_id() == child.id).unwrap();
    assert_eq!(
        child_node.execution_resolution(),
        Resolution { width: 2, height: 1 },
        "the frozen nested preview canvas follows its normalized execution scale"
    );
    let required = 2 * 4 * 2 * 16 + 2 * 16;
    assert_eq!(work.auxiliary_preparation_bytes, required);
    let metadata = work.projection_context.as_ref().expect("nested source facts");
    assert_eq!(metadata.available_queries.len(), 1);
    let total_required = required + metadata.bytes;
    assert!(work.validate_auxiliary_working_set().is_err());
    let mut store = PreviewFrameStoreAdapter::new(PreviewFrameStoreAdapterConfig {
        viewer_byte_budget: total_required,
        ..Default::default()
    });
    work.admit_auxiliary_working_set(&mut store).unwrap();
    work.admit_auxiliary_working_set(&mut store).unwrap();
    work.validate_auxiliary_working_set().unwrap();
    assert_eq!(
        store.diagnostics().viewer_aggregate_reserved_bytes,
        total_required,
        "polling never reserves twice"
    );
    let output = work.execute(
        &scratch,
        &mut mondrian_effects::EffectProcessorControl::uncancelled(),
        None,
        &mut None,
    );
    let PreviewTimelineResolution::Ready(output) = output else {
        panic!("nested pixels must execute with the original reservation");
    };
    assert_eq!(
        crate::app::preview_auxiliary::resolved_auxiliary_working_bytes(&output.plan.elements)
            .unwrap(),
        0,
        "child auxiliary planes retire before publishing its baked output"
    );
    drop(output);
    drop(pending);
    assert!(!work.is_active());
    assert_eq!(
        store.diagnostics().viewer_aggregate_reserved_bytes,
        total_required,
        "retiring interest cannot free a still-owned worker charge"
    );
    drop(work);
    assert_eq!(store.diagnostics().viewer_aggregate_reserved_bytes, 0);
}

#[test]
fn auxiliary_viewer_and_persistent_cache_identities_include_source_address_and_revision() {
    use mondrian_core::{PropertyValue, VisualSourceReference, VisualSourceRetime};
    let (_root, asset) = asset_fixture(Some(Duration::from_secs(2)));
    let (_other_root, other) = asset_fixture(Some(Duration::from_secs(2)));
    let sequence = auxiliary_matte_sequence(asset.id);
    let original = resolve_auxiliary_fixture(&sequence, &asset);
    let mut changed = sequence.clone();
    let effect = &mut changed.video_tracks[0].clips[0].effects[0];
    let parameter = effect.effect_type.parameter_id("source").unwrap();
    effect
        .set_static_value_by_parameter(
            &parameter,
            PropertyValue::VisualSource(VisualSourceReference::Asset {
                asset_id: other.id,
                retime: VisualSourceRetime::default(),
            }),
        )
        .unwrap();
    let addressed = resolve_auxiliary_fixture(&changed, &other);
    let mut revised = other.clone();
    revised.id = asset.id;
    let revised = resolve_auxiliary_fixture(&sequence, &revised);
    for changed in [&addressed, &revised] {
        assert_ne!(original.plan.cache_key, changed.plan.cache_key);
        let before =
            crate::app::preview_render_cache_identity::canonical_resolved_node_materialization(
                &original.plan.elements,
            )
            .unwrap();
        let after =
            crate::app::preview_render_cache_identity::canonical_resolved_node_materialization(
                &changed.plan.elements,
            )
            .unwrap();
        assert_ne!(before, after);
    }
}

#[test]
fn auxiliary_prefetch_retains_intents_validates_transparency_and_coalesces_decode() {
    let (_root, asset) = asset_fixture(Some(Duration::from_secs(2)));
    let intent = request(
        &asset,
        mondrian_core::TimelineTime::ZERO,
        VisualSourceBoundary::Clamp,
    );
    let plan = planned(&asset, &intent);
    let media = plan.media_request.clone().unwrap();
    let transparent = request(
        &asset,
        mondrian_core::TimelineTime::new(50, 1).unwrap(),
        VisualSourceBoundary::Transparent,
    );
    let mut validations = Vec::new();
    let requests = resolve_preview_picture_media_demands(
        vec![
            PreviewTimelinePictureDemand::Media(media.clone()),
            PreviewTimelinePictureDemand::Auxiliary(intent.clone()),
            PreviewTimelinePictureDemand::Auxiliary(intent),
            PreviewTimelinePictureDemand::Auxiliary(transparent.clone()),
        ],
        &mut |request| {
            validations.push(request.clone());
            Ok(planned(&asset, request))
        },
    )
    .unwrap();
    assert_eq!(validations.len(), 2);
    assert_eq!(requests, [media]);

    std::fs::remove_file(asset.file_path().unwrap()).unwrap();
    assert!(resolve_preview_picture_media_demands(
        vec![PreviewTimelinePictureDemand::Auxiliary(transparent)],
        &mut |request| resolve_preview_auxiliary_asset(
            request,
            &asset,
            PreviewHardwareDecodeAdmissionState::default(),
        ),
    )
    .is_err());
}

#[test]
fn auxiliary_source_wave_reuses_ready_only_while_the_complete_source_key_matches() {
    let (_root, asset) = asset_fixture(Some(Duration::from_secs(2)));
    let intent = request(
        &asset,
        mondrian_core::TimelineTime::ZERO,
        VisualSourceBoundary::Clamp,
    );
    let plan = planned(&asset, &intent);
    let demands = [PreparedPreviewSourceDemand::Auxiliary(intent.clone())];
    let mut sources = PreviewSourcePreparation::default();
    let mut resolver = |_: &PreviewAuxiliaryAssetRequest| Ok(plan.clone());
    let mut calls = 0;
    let mut media = |_: PreviewTimelineMediaRequest| {
        calls += 1;
        if calls == 1 {
            PreviewTimelineMediaFrame::Pending { wait: PreviewTimelineMediaWait::Producer }
        } else {
            ready(&plan, 1)
        }
    };
    assert!(matches!(
        admit_prepared_preview_sources(
            &mut sources,
            &demands,
            &mut media,
            &mut |_| panic!("no title"),
            Some(&mut resolver),
        ),
        Err(PreviewTimelineAbort::Pending { .. })
    ));
    admit_prepared_preview_sources(
        &mut sources,
        &demands,
        &mut media,
        &mut |_| panic!("no title"),
        Some(&mut resolver),
    )
    .unwrap();
    assert_eq!(calls, 2);
    admit_prepared_preview_sources(
        &mut sources,
        &demands,
        &mut |_| panic!("retain original Ready producer"),
        &mut |_| panic!("no title"),
        Some(&mut resolver),
    )
    .unwrap();

    let (_replacement_root, mut replacement) = asset_fixture(Some(Duration::from_secs(2)));
    replacement.id = asset.id;
    let replacement_plan = planned(&replacement, &intent);
    assert_eq!(plan.media_request, replacement_plan.media_request);
    assert_ne!(plan.validated_key, replacement_plan.validated_key);
    let mut replacement_resolver = |_: &PreviewAuxiliaryAssetRequest| Ok(replacement_plan.clone());
    let mut replacements = 0;
    admit_prepared_preview_sources(
        &mut sources,
        &demands,
        &mut |_| {
            replacements += 1;
            ready(&replacement_plan, 2)
        },
        &mut |_| panic!("no title"),
        Some(&mut replacement_resolver),
    )
    .unwrap();
    assert_eq!(replacements, 1);
    let PreviewTimelineMediaFrame::Ready(frame) =
        sources.media.get(replacement_plan.media_request.as_ref().unwrap()).unwrap()
    else {
        panic!("replacement Ready retained");
    };
    assert_eq!(frame.source_key(), Some(&replacement_plan.validated_key));
    assert_eq!(
        frame.identity(),
        PreviewSemanticIdentity::from_test_fingerprint([2; 32])
    );
}

#[test]
fn auxiliary_transparent_plan_never_decodes_but_still_revalidates_on_retry() {
    let (_root, asset) = asset_fixture(Some(Duration::from_secs(2)));
    let intent = request(
        &asset,
        mondrian_core::TimelineTime::new(50, 1).unwrap(),
        VisualSourceBoundary::Transparent,
    );
    let demands = [PreparedPreviewSourceDemand::Auxiliary(intent.clone())];
    let mut sources = PreviewSourcePreparation::default();
    let mut resolver = |request: &PreviewAuxiliaryAssetRequest| {
        resolve_preview_auxiliary_asset(
            request,
            &asset,
            PreviewHardwareDecodeAdmissionState::default(),
        )
    };
    admit_prepared_preview_sources(
        &mut sources,
        &demands,
        &mut |_| panic!("transparent source needs no decoder"),
        &mut |_| panic!("no title"),
        Some(&mut resolver),
    )
    .unwrap();
    assert!(sources.auxiliary[&intent].time.out_of_range);
    assert!(sources.media.is_empty());
    std::fs::remove_file(asset.file_path().unwrap()).unwrap();
    assert!(matches!(
        admit_prepared_preview_sources(
            &mut sources,
            &demands,
            &mut |_| panic!("do not decode a missing source"),
            &mut |_| panic!("no title"),
            Some(&mut resolver),
        ),
        Err(PreviewTimelineAbort::Unavailable(_))
    ));
    assert!(sources.auxiliary.is_empty());
}

#[test]
fn auxiliary_revision_races_and_approximate_frames_cannot_enter_dsp() {
    let (_root, asset) = asset_fixture(Some(Duration::from_secs(2)));
    let intent = request(
        &asset,
        mondrian_core::TimelineTime::ZERO,
        VisualSourceBoundary::Clamp,
    );
    let plan = planned(&asset, &intent);
    let demands = [PreparedPreviewSourceDemand::Auxiliary(intent)];
    let (_second_root, mut second) = asset_fixture(Some(Duration::from_secs(2)));
    second.id = asset.id;
    let second_plan = planned(
        &second,
        match &demands[0] {
            PreparedPreviewSourceDemand::Auxiliary(request) => request,
            _ => unreachable!(),
        },
    );
    let mut sources = PreviewSourcePreparation::default();
    let mut resolver = |_: &PreviewAuxiliaryAssetRequest| Ok(plan.clone());
    assert!(matches!(
        admit_prepared_preview_sources(
            &mut sources,
            &demands,
            &mut |_| ready(&second_plan, 2),
            &mut |_| panic!("no title"),
            Some(&mut resolver),
        ),
        Err(PreviewTimelineAbort::Pending {
            dependency: PreviewTimelinePendingDependency::Media {
                wait: PreviewTimelineMediaWait::RetryAdmission,
                ..
            }
        })
    ));
    assert!(sources.media.is_empty());
    let mut approximate = ready(&plan, 1);
    let PreviewTimelineMediaFrame::Ready(frame) = &mut approximate else {
        unreachable!()
    };
    frame.set_presentation_quality(FramePresentationQuality::Degraded);
    assert!(matches!(
        admit_prepared_preview_sources(
            &mut sources,
            &demands,
            &mut |_| approximate.clone(),
            &mut |_| panic!("no title"),
            Some(&mut resolver),
        ),
        Err(PreviewTimelineAbort::Pending { .. })
    ));
}
