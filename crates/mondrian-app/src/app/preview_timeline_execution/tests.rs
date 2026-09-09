use mondrian_core::types::Rational;
use mondrian_core::{ensure_mondrian_default_ocio_loaded, Color, TimelineTime};
use mondrian_timeline::clip::Clip;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use super::*;

fn pending_media_frame() -> PreviewTimelineMediaFrame {
    PreviewTimelineMediaFrame::Pending { wait: PreviewTimelineMediaWait::Producer }
}

fn tt(frame: i64, time_base: Rational) -> mondrian_core::TimelineTime {
    mondrian_core::TimelineTime::new(
        frame.checked_mul(time_base.num).expect("test time fits"),
        time_base.den,
    )
    .expect("valid test time")
}

fn solid_sequence(name: &str, color: Color) -> Sequence {
    let mut sequence = Sequence::new(name);
    let time_base = sequence.time_base();
    sequence.video_tracks[0]
        .add_clip(
            Clip::new_solid_color(AssetId::new(), color, tt(0, time_base), tt(24, time_base))
                .expect("valid solid clip"),
        )
        .expect("insert solid clip");
    sequence
}

fn color_context(sequence: &Sequence) -> ProgramColorContext {
    sequence
        .settings
        .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
        .expect("valid test context")
}

fn temporal_blend_effect(offset: TimelineTime) -> mondrian_effects::EffectNode {
    temporal_sample_effect(TimelineTime::ZERO.checked_sub(offset).expect("past offset"))
}

fn temporal_sample_effect(sample_offset: TimelineTime) -> mondrian_effects::EffectNode {
    use mondrian_effects::{
        register_effect_definition, EffectColorDomainContract, EffectDefinition, EffectDeterminism,
        EffectExecutionContract, EffectExecutionModes, EffectGraphTopology, EffectRenderOp,
        EffectResourceLifetime, EffectRoiPropagation, EffectStateModel, EffectTemporalInputExtent,
        EffectTemporalSpan, EffectType,
    };

    static NEXT_TEMPORAL_EFFECT: AtomicU64 = AtomicU64::new(1);
    let serial = NEXT_TEMPORAL_EFFECT.fetch_add(1, Ordering::Relaxed);
    let effect_type = EffectType::Plugin(format!("test.preview.temporal-blend.{serial}"));
    let temporal_input = if sample_offset.is_negative() {
        EffectTemporalInputExtent {
            past: EffectTemporalSpan::Finite(
                TimelineTime::ZERO.checked_sub(sample_offset).expect("past duration"),
            ),
            future: EffectTemporalSpan::None,
        }
    } else {
        EffectTemporalInputExtent {
            past: EffectTemporalSpan::None,
            future: if sample_offset.is_zero() {
                EffectTemporalSpan::None
            } else {
                EffectTemporalSpan::Finite(sample_offset)
            },
        }
    };
    register_effect_definition(
        EffectDefinition::new(
            effect_type.key(),
            "Preview temporal blend test",
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: EffectDeterminism::Deterministic,
            state_model: EffectStateModel::Stateless,
            temporal_input,
            roi_propagation: EffectRoiPropagation::PixelLocal,
            resource_lifetime: EffectResourceLifetime::Frame,
            topology: EffectGraphTopology::LinearChain,
        })
        .with_graph_builder(Arc::new(move |_, _, graph| {
            graph.append_unary(EffectRenderOp::TemporalFrameBlend { sample_offset, mix: 0.25 });
            Ok(())
        })),
    )
    .expect("register Preview temporal test definition");
    mondrian_effects::EffectNode::new(effect_type)
}

fn preview_gpu_only_point_effect(label: &str) -> mondrian_effects::EffectNode {
    use mondrian_core::WorkingColorSpace;
    use mondrian_effects::{
        register_effect_definition, EffectColorDomainContract, EffectDefinition, EffectDeterminism,
        EffectExecutionContract, EffectExecutionModes, EffectGraphTopology, EffectNode,
        EffectRenderOp, EffectResourceLifetime, EffectRoiPropagation, EffectStateModel,
        EffectTemporalInputExtent, EffectType,
    };

    let effect_type =
        EffectType::Plugin(format!("test.preview.gpu-only.{label}.{}", AssetId::new()));
    register_effect_definition(
        EffectDefinition::new(
            effect_type.key(),
            "Preview GPU-only point Effect",
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::GPU_F32,
            determinism: EffectDeterminism::Deterministic,
            state_model: EffectStateModel::Stateless,
            temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
            roi_propagation: EffectRoiPropagation::PixelLocal,
            resource_lifetime: EffectResourceLifetime::Frame,
            topology: EffectGraphTopology::LinearChain,
        })
        .with_graph_builder(Arc::new(|_, _, graph| {
            graph.append_unary(EffectRenderOp::ColorAdjust {
                exposure: 0.25,
                contrast: 1.0,
                saturation: 1.0,
                working_color_space: WorkingColorSpace::LinearRec709,
            });
            Ok(())
        })),
    )
    .expect("register Preview GPU-only point Effect");
    EffectNode::new(effect_type)
}

fn temporal_media_sequence() -> (Sequence, AssetId, mondrian_core::ClipId) {
    let mut sequence = Sequence::new("Preview temporal media");
    sequence.settings.frame_rate = Rational::new(30, 1);
    sequence.settings.resolution = Resolution { width: 2, height: 1 };
    let time_base = sequence.time_base();
    let asset_id = AssetId::new();
    let mut clip =
        Clip::new(asset_id, TimelineTime::ZERO, tt(60, time_base)).expect("temporal media Clip");
    clip.add_effect_node(temporal_blend_effect(
        TimelineTime::new(1, 30).expect("one-frame history"),
    ));
    let clip_id = clip.id;
    sequence.video_tracks[0].add_clip(clip).expect("insert temporal media Clip");
    (sequence, asset_id, clip_id)
}

fn ready_temporal_media_frame(
    request: &PreviewTimelineMediaRequest,
    red: f32,
    identity_salt: u64,
) -> PreviewTimelineMediaFrame {
    let mut identity =
        PreviewSemanticIdentityBuilder::new(b"mondrian.preview.test-temporal-media.v1");
    std::hash::Hash::hash(&request.asset_id, &mut identity);
    std::hash::Hash::hash(&request.source_sample, &mut identity);
    std::hash::Hasher::write_u64(&mut identity, identity_salt);
    PreviewTimelineMediaFrame::Ready(MediaPreviewFrame::from_working(
        CpuColorFrame::working(WorkingRgbaF32Frame {
            width: request.target_resolution.width,
            height: request.target_resolution.height,
            data: vec![
                [red, 0.0, 0.0, 1.0];
                request.target_resolution.width as usize
                    * request.target_resolution.height as usize
            ],
            color_space: request.input_color.working_color_space,
        }),
        request.target_resolution,
        identity.finish_identity(),
        FramePresentationQuality::Ready,
        PreviewDecodeExecutionSummary {
            media_layers: 1,
            software_cpu_layers: 1,
            ..PreviewDecodeExecutionSummary::default()
        },
    ))
}

#[test]
fn solid_plan_is_ui_independent_and_has_mandatory_cache_identity() {
    ensure_mondrian_default_ocio_loaded().expect("default OCIO");
    let sequence = solid_sequence("root", Color::from_rgba8(20, 40, 80, 255));
    let target = Resolution { width: 64, height: 36 };
    let mut unexpected_media = |_| panic!("solid plan must not request media");

    let first = resolve_preview_timeline(
        &sequence,
        &[],
        3,
        target,
        PreviewResolutionScale::Full,
        color_context(&sequence),
        &mut unexpected_media,
        &mut |_| panic!("solid plan must not request titles"),
    );
    let PreviewTimelineResolution::Ready(first) = first else {
        panic!("solid Timeline should resolve");
    };
    assert_eq!(first.plan.elements.len(), 1);
    assert!(first.facts.is_empty());

    let mut unexpected_media = |_| panic!("solid plan must not request media");
    let second = resolve_preview_timeline(
        &sequence,
        &[],
        3,
        target,
        PreviewResolutionScale::Full,
        color_context(&sequence),
        &mut unexpected_media,
        &mut |_| panic!("solid plan must not request titles"),
    );
    let PreviewTimelineResolution::Ready(second) = second else {
        panic!("solid Timeline should resolve twice");
    };
    assert_eq!(first.plan.cache_key, second.plan.cache_key);
    assert_eq!(
        first.plan.render_cache_identity,
        second.plan.render_cache_identity
    );
    assert!(first.plan.render_cache_identity.is_some());

    let mut cache_disabled = sequence.clone();
    cache_disabled.settings.preview.cache_enabled = false;
    let disabled = resolve_preview_timeline(
        &cache_disabled,
        &[],
        3,
        target,
        PreviewResolutionScale::Full,
        color_context(&cache_disabled),
        &mut |_| panic!("solid plan must not request media"),
        &mut |_| panic!("solid plan must not request titles"),
    );
    let PreviewTimelineResolution::Ready(disabled) = disabled else {
        panic!("cache-disabled Timeline should resolve");
    };
    assert!(disabled.plan.render_cache_identity.is_none());
}

#[test]
fn program_output_only_change_reuses_pre_output_working_cache_identity() {
    ensure_mondrian_default_ocio_loaded().expect("default OCIO");
    let sequence = solid_sequence("program-output-cache", Color::from_rgba8(30, 60, 90, 255));
    let target = Resolution { width: 64, height: 36 };
    let resolve = |sequence: &Sequence| {
        let mut unexpected_media = |_| panic!("solid plan must not request media");
        let result = resolve_preview_timeline(
            sequence,
            &[],
            3,
            target,
            PreviewResolutionScale::Full,
            color_context(sequence),
            &mut unexpected_media,
            &mut |_| panic!("solid plan must not request titles"),
        );
        let PreviewTimelineResolution::Ready(result) = result else {
            panic!("solid Timeline should resolve");
        };
        result
    };

    let rec709 = resolve(&sequence);
    let mut display_p3 = sequence.clone();
    display_p3.settings.color.program_output.color_space = ColorSpace::DisplayP3;
    let display_p3 = resolve(&display_p3);

    assert_ne!(rec709.plan.cache_key, display_p3.plan.cache_key);
    assert_eq!(
        rec709.plan.render_cache_identity, display_p3.plan.render_cache_identity,
        "encoded Program Output must remain downstream of cached working pixels"
    );
}

#[test]
fn preview_execution_admission_fails_before_media_demand_or_decode() {
    use mondrian_effects::{
        register_effect_definition, EffectColorDomainContract, EffectDefinition, EffectDeterminism,
        EffectExecutionContract, EffectExecutionModes, EffectGraphTopology, EffectNode,
        EffectResourceLifetime, EffectRoiPropagation, EffectStateModel, EffectTemporalInputExtent,
        EffectType,
    };
    use std::sync::Arc;

    let effect_type = EffectType::Plugin(format!(
        "test.preview.predecode.unavailable.{}",
        AssetId::new()
    ));
    register_effect_definition(
        EffectDefinition::new(
            effect_type.key(),
            "Unavailable identity",
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::NONE,
            determinism: EffectDeterminism::Deterministic,
            state_model: EffectStateModel::Stateless,
            temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
            roi_propagation: EffectRoiPropagation::PixelLocal,
            resource_lifetime: EffectResourceLifetime::Frame,
            topology: EffectGraphTopology::LinearChain,
        })
        .with_graph_builder(Arc::new(|_, _, _| Ok(()))),
    )
    .expect("register GPU-only identity definition");

    let mut sequence = Sequence::new("predecode admission");
    let time_base = sequence.time_base();
    let mut clip =
        Clip::new(AssetId::new(), tt(0, time_base), tt(24, time_base)).expect("media Clip");
    clip.add_effect_node(EffectNode::new(effect_type));
    sequence.video_tracks[0].add_clip(clip).expect("insert media Clip");

    assert!(
        collect_preview_timeline_media_demands(
            &sequence,
            &[],
            0,
            Resolution { width: 64, height: 36 },
            PreviewResolutionScale::Full,
            color_context(&sequence),
        )
        .is_err(),
        "unsupported execution must reject before publishing a media demand"
    );

    let mut media_called = false;
    let mut media = |_| {
        media_called = true;
        pending_media_frame()
    };
    let result = resolve_preview_timeline(
        &sequence,
        &[],
        0,
        Resolution { width: 64, height: 36 },
        PreviewResolutionScale::Full,
        color_context(&sequence),
        &mut media,
        &mut |_| panic!("media plan must not request title rasterization"),
    );
    let PreviewTimelineResolution::Unavailable { reason } = result else {
        panic!("unsupported execution must be terminally unavailable");
    };
    assert_eq!(
        reason.stage(),
        crate::app::preview_unavailability::PreviewOutputStage::TimelineEvaluation
    );
    assert!(
        !media_called,
        "media callback is the decode-adapter Seam and must remain untouched"
    );
}

#[test]
fn media_demand_requests_cpu_working_pixels_only_when_exact_gpu_lowering_is_unavailable() {
    use mondrian_effects::{EffectNode, EffectNodeExt, EffectType};

    let target = Resolution { width: 64, height: 36 };
    let plain_asset = AssetId::new();
    let mut plain = Sequence::new("full GPU media demand");
    let plain_time_base = plain.time_base();
    plain.video_tracks[0]
        .add_clip(
            Clip::new(plain_asset, TimelineTime::ZERO, tt(24, plain_time_base))
                .expect("plain media Clip"),
        )
        .expect("insert plain media Clip");
    let plain_demands = collect_preview_timeline_media_demands(
        &plain,
        &[],
        0,
        target,
        PreviewResolutionScale::Full,
        color_context(&plain),
    )
    .expect("plain media demand");
    assert_eq!(plain_demands.len(), 1);
    assert!(
        !plain_demands[0].cpu_working_required,
        "an exact full-GPU identity graph must preserve source/native residency"
    );

    let heterogeneous_asset = AssetId::new();
    let mut heterogeneous = Sequence::new("heterogeneous media demand");
    let heterogeneous_time_base = heterogeneous.time_base();
    let mut clip = Clip::new(
        heterogeneous_asset,
        TimelineTime::ZERO,
        tt(24, heterogeneous_time_base),
    )
    .expect("heterogeneous media Clip");
    clip.add_effect_node(EffectNode::with_defaults(EffectType::GaussianBlur));
    heterogeneous.video_tracks[0]
        .add_clip(clip)
        .expect("insert heterogeneous media Clip");
    let heterogeneous_demands = collect_preview_timeline_media_demands(
        &heterogeneous,
        &[],
        0,
        target,
        PreviewResolutionScale::Full,
        color_context(&heterogeneous),
    )
    .expect("heterogeneous media demand");
    assert_eq!(heterogeneous_demands.len(), 1);
    assert!(
        heterogeneous_demands[0].cpu_working_required,
        "renderer rejection of the exact full-GPU graph must request a CPU working payload"
    );
}

#[test]
fn production_timeline_prepares_heterogeneous_route_before_media_materialization() {
    use mondrian_core::WorkingColorSpace;
    use mondrian_effects::{
        register_effect_definition, EffectColorDomainContract, EffectDefinition, EffectDeterminism,
        EffectExecutionContract, EffectExecutionModes, EffectGraphTopology, EffectNode,
        EffectNodeExt, EffectRenderOp, EffectResourceLifetime, EffectRoiPropagation,
        EffectStateModel, EffectTemporalInputExtent, EffectType,
    };

    let target = Resolution { width: 64, height: 36 };
    let asset_id = AssetId::new();
    let mut sequence = Sequence::new("prepared heterogeneous Preview route");
    let time_base = sequence.time_base();
    let mut clip = Clip::new(asset_id, TimelineTime::ZERO, tt(24, time_base)).expect("media Clip");
    let blur = EffectNode::with_defaults(EffectType::GaussianBlur);
    let gpu_only_type = EffectType::Plugin(format!("test.preview.gpu-only.{}", AssetId::new()));
    register_effect_definition(
        EffectDefinition::new(
            gpu_only_type.key(),
            "Preview GPU-only route test",
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::GPU_F32,
            determinism: EffectDeterminism::Deterministic,
            state_model: EffectStateModel::Stateless,
            temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
            roi_propagation: EffectRoiPropagation::PixelLocal,
            resource_lifetime: EffectResourceLifetime::Frame,
            topology: EffectGraphTopology::LinearChain,
        })
        .with_graph_builder(Arc::new(|_, _, graph| {
            graph.append_unary(EffectRenderOp::ColorAdjust {
                exposure: 0.25,
                contrast: 1.0,
                saturation: 1.0,
                working_color_space: WorkingColorSpace::LinearRec709,
            });
            Ok(())
        })),
    )
    .expect("register GPU-only test Effect");
    clip.add_effect_node(blur);
    clip.add_effect_node(EffectNode::new(gpu_only_type));
    sequence.video_tracks[0].add_clip(clip).expect("insert media Clip");

    let mut media_called = false;
    let resolution = resolve_preview_timeline(
        &sequence,
        &[],
        0,
        target,
        PreviewResolutionScale::Full,
        color_context(&sequence),
        &mut |request: PreviewTimelineMediaRequest| {
            media_called = true;
            assert!(request.cpu_working_required);
            ready_temporal_media_frame(&request, 0.25, 91)
        },
        &mut |_| panic!("media-only plan must not request title rasterization"),
    );
    let resolved = match resolution {
        PreviewTimelineResolution::Ready(resolved) => resolved,
        PreviewTimelineResolution::Unavailable { reason } => panic!(
            "prepared heterogeneous Timeline must reach source materialization: {}",
            reason.detail()
        ),
        PreviewTimelineResolution::Empty | PreviewTimelineResolution::Pending { .. } => {
            panic!("prepared heterogeneous Timeline returned no ready plan")
        }
    };
    assert!(
        media_called,
        "route admission must precede, not suppress, decode adaptation"
    );
    assert!(matches!(
        resolved.plan.elements.as_slice(),
        [ResolvedPreviewElement::Media { prepared_heterogeneous_route: Some(_), .. }]
    ));
}

#[test]
fn production_timeline_prepares_procedural_solid_heterogeneous_route_without_media() {
    use mondrian_effects::{EffectNodeExt, EffectType};

    let target = Resolution { width: 64, height: 36 };
    let mut sequence = solid_sequence(
        "prepared heterogeneous Solid Color",
        Color::from_rgba8(40, 90, 180, 192),
    );
    sequence.settings.resolution = target;
    let clip = sequence.video_tracks[0].clips.first_mut().expect("Solid Color Clip");
    clip.add_effect_node(mondrian_effects::EffectNode::with_defaults(
        EffectType::GaussianBlur,
    ));
    clip.add_effect_node(preview_gpu_only_point_effect("solid-source"));

    let resolution = resolve_preview_timeline(
        &sequence,
        &[],
        0,
        target,
        PreviewResolutionScale::Full,
        color_context(&sequence),
        &mut |_| panic!("procedural Solid Color must not invoke the media Adapter"),
        &mut |_| panic!("procedural Solid Color must not invoke title rasterization"),
    );
    let resolved = match resolution {
        PreviewTimelineResolution::Ready(resolved) => resolved,
        PreviewTimelineResolution::Unavailable { reason } => panic!(
            "procedural Solid Color must reach the prepared heterogeneous route: {}",
            reason.detail()
        ),
        PreviewTimelineResolution::Empty | PreviewTimelineResolution::Pending { .. } => {
            panic!("procedural heterogeneous Timeline returned no ready plan")
        }
    };
    let [ResolvedPreviewElement::HeterogeneousSolidColor { prepared_route, .. }] =
        resolved.plan.elements.as_slice()
    else {
        panic!("Solid Color must retain its typed procedural source and frozen route")
    };
    assert_eq!(
        prepared_route.frame_extent(),
        EffectFrameExtent::new(64, 36)
    );
}

#[test]
fn production_timeline_binds_nested_cpu_materialization_to_parent_heterogeneous_route() {
    use mondrian_effects::{EffectNodeExt, EffectType};

    ensure_mondrian_default_ocio_loaded().expect("default OCIO");
    let target = Resolution { width: 64, height: 36 };
    let mut child = solid_sequence(
        "nested heterogeneous child",
        Color::from_rgba8(40, 90, 180, 255),
    );
    child.settings.resolution = target;
    let child_id = child.id;

    let mut parent = Sequence::new("nested heterogeneous parent");
    parent.settings.resolution = target;
    let time_base = parent.time_base();
    let mut nested = Clip::new_nested_sequence(
        child_id,
        TimelineTime::ZERO,
        tt(24, time_base),
        Some("nested heterogeneous child".to_owned()),
    )
    .expect("nested Clip");
    nested.add_effect_node(mondrian_effects::EffectNode::with_defaults(
        EffectType::GaussianBlur,
    ));
    nested.add_effect_node(preview_gpu_only_point_effect("nested-parent"));
    parent.video_tracks[0].add_clip(nested).expect("insert nested Clip");

    let resolution = resolve_preview_timeline(
        &parent,
        &[child],
        0,
        target,
        PreviewResolutionScale::Full,
        color_context(&parent),
        &mut |_| panic!("solid nested child must not request media"),
        &mut |_| panic!("solid nested child must not request title rasterization"),
    );
    let resolved = match resolution {
        PreviewTimelineResolution::Ready(resolved) => resolved,
        PreviewTimelineResolution::Unavailable { reason } => panic!(
            "nested CPU materialization must reach the parent heterogeneous route: {}",
            reason.detail()
        ),
        PreviewTimelineResolution::Empty | PreviewTimelineResolution::Pending { .. } => {
            panic!("nested heterogeneous Timeline returned no ready plan")
        }
    };
    let [ResolvedPreviewElement::Media {
        frame, prepared_heterogeneous_route: Some(route), ..
    }] = resolved.plan.elements.as_slice()
    else {
        panic!("nested placement must bind its materialized frame to the frozen parent route")
    };
    assert_eq!(
        route.frame_extent(),
        EffectFrameExtent::new(frame.width(), frame.height()),
        "the parent route must bind the exact child materialization raster"
    );
}

#[test]
fn nested_child_still_requires_a_complete_cpu_materialization_route() {
    use mondrian_effects::{EffectNodeExt, EffectType};

    let target = Resolution { width: 64, height: 36 };
    let mut child = Sequence::new("nested heterogeneous child blocker");
    child.settings.resolution = target;
    let child_time_base = child.time_base();
    let mut child_media = Clip::new(AssetId::new(), TimelineTime::ZERO, tt(24, child_time_base))
        .expect("child media Clip");
    child_media.add_effect_node(mondrian_effects::EffectNode::with_defaults(
        EffectType::GaussianBlur,
    ));
    child_media.add_effect_node(preview_gpu_only_point_effect("nested-child-blocker"));
    child.video_tracks[0].add_clip(child_media).expect("insert child media Clip");
    let child_id = child.id;

    let mut parent = Sequence::new("nested heterogeneous child blocker parent");
    parent.settings.resolution = target;
    let parent_time_base = parent.time_base();
    parent.video_tracks[0]
        .add_clip(
            Clip::new_nested_sequence(
                child_id,
                TimelineTime::ZERO,
                tt(24, parent_time_base),
                Some("nested heterogeneous child blocker".to_owned()),
            )
            .expect("nested Clip"),
        )
        .expect("insert nested Clip");

    let mut media_called = false;
    let resolution = resolve_preview_timeline(
        &parent,
        &[child],
        0,
        target,
        PreviewResolutionScale::Full,
        color_context(&parent),
        &mut |_| {
            media_called = true;
            pending_media_frame()
        },
        &mut |_| panic!("media-only nested child must not request title rasterization"),
    );
    let PreviewTimelineResolution::Unavailable { reason } = resolution else {
        panic!("a heterogeneous child cannot masquerade as a CPU materialization route")
    };
    assert_eq!(
        reason.stage(),
        crate::app::preview_unavailability::PreviewOutputStage::TimelineEvaluation
    );
    assert!(reason.detail().contains("cannot materialize through the CPU Adapter"));
    assert!(
        !media_called,
        "nested route admission must fail before the child media Adapter"
    );
}

#[test]
fn ordinary_current_layers_are_admitted_together_before_pending_is_returned() {
    ensure_mondrian_default_ocio_loaded().expect("default OCIO");
    let mut sequence = Sequence::new("video and static Current batch");
    sequence.settings.resolution = Resolution { width: 2, height: 1 };
    let video = AssetId::new();
    let still = AssetId::new();
    let duration = tt(60, sequence.time_base());
    sequence.video_tracks[0]
        .add_clip(Clip::new(video, TimelineTime::ZERO, duration).expect("video"))
        .expect("insert video");
    sequence.video_tracks[1]
        .add_clip(Clip::new_still_image(still, TimelineTime::ZERO, duration).expect("still"))
        .expect("insert still");
    for all_ready in [false, true] {
        let mut requests = Vec::new();
        let result = resolve_preview_timeline(
            &sequence,
            &[],
            5,
            sequence.settings.resolution,
            PreviewResolutionScale::Full,
            color_context(&sequence),
            &mut |request: PreviewTimelineMediaRequest| {
                requests.push(request.clone());
                if all_ready || request.asset_id == still {
                    ready_temporal_media_frame(&request, 0.25, 87)
                } else {
                    pending_media_frame()
                }
            },
            &mut |_| panic!("no title"),
        );
        assert_eq!(
            requests.len(),
            2,
            "each exact layer is admitted once per evaluation"
        );
        assert!(requests.iter().any(|request| request.asset_id == video));
        assert!(requests.iter().any(|request| request.asset_id == still
            && request.source_sample.time() == TimelineTime::ZERO));
        if all_ready {
            assert!(matches!(result, PreviewTimelineResolution::Ready(_)));
        } else {
            assert!(matches!(result, PreviewTimelineResolution::Pending { .. }));
        }
    }
}

#[test]
fn temporal_preview_schedules_the_complete_cross_zero_set_before_publishing() {
    let (sequence, asset_id, clip_id) = temporal_media_sequence();
    let target = sequence.settings.resolution;
    let mut scheduled = Vec::new();
    let pending = resolve_preview_timeline(
        &sequence,
        &[],
        0,
        target,
        PreviewResolutionScale::Full,
        color_context(&sequence),
        &mut |request: PreviewTimelineMediaRequest| {
            scheduled.push(request);
            pending_media_frame()
        },
        &mut |_| panic!("temporal media must not request titles"),
    );
    assert!(matches!(
        pending,
        PreviewTimelineResolution::Pending {
            dependency: PreviewTimelinePendingDependency::Temporal {
                clip_id: pending_clip,
                pending_sources: 2,
            },
            ..
        } if pending_clip == clip_id
    ));
    assert_eq!(scheduled.len(), 2);
    assert!(scheduled
        .iter()
        .all(|request| request.asset_id == asset_id && request.cpu_working_required));
    assert_eq!(scheduled[0].source_sample.time(), TimelineTime::ZERO);
    assert_eq!(
        scheduled[1].source_sample.time(),
        TimelineTime::new(-1, 30).expect("signed source request"),
        "the generic temporal planner must not clamp history at Timeline zero"
    );

    let ready = resolve_preview_timeline(
        &sequence,
        &[],
        0,
        target,
        PreviewResolutionScale::Full,
        color_context(&sequence),
        &mut |request: PreviewTimelineMediaRequest| {
            let red = if request.source_sample.time() < TimelineTime::ZERO {
                0.0
            } else {
                1.0
            };
            ready_temporal_media_frame(&request, red, 1)
        },
        &mut |_| panic!("temporal media must not request titles"),
    );
    let PreviewTimelineResolution::Ready(ready) = ready else {
        panic!("a complete frozen temporal source set must publish");
    };
    let [ResolvedPreviewElement::Media { frame, .. }] = ready.plan.elements.as_slice() else {
        panic!("temporal media must lower through the ordinary media compositor seam");
    };
    let pixel = frame.working_frame().expect("working temporal output").frame.rgba_f32().data[0];
    assert!(
        (pixel[0] - 0.75).abs() < 1.0e-6,
        "unexpected temporal blend: {pixel:?}"
    );
}

#[test]
fn temporal_preview_schedules_and_publishes_finite_lookahead() {
    let mut sequence = Sequence::new("Preview future temporal media");
    sequence.settings.frame_rate = Rational::new(30, 1);
    sequence.settings.resolution = Resolution { width: 2, height: 1 };
    let time_base = sequence.time_base();
    let asset_id = AssetId::new();
    let mut clip =
        Clip::new(asset_id, TimelineTime::ZERO, tt(60, time_base)).expect("temporal media Clip");
    clip.add_effect_node(temporal_sample_effect(
        TimelineTime::new(1, 30).expect("one-frame lookahead"),
    ));
    sequence.video_tracks[0].add_clip(clip).expect("insert temporal media Clip");
    let target = sequence.settings.resolution;
    let mut scheduled = Vec::new();

    let pending = resolve_preview_timeline(
        &sequence,
        &[],
        0,
        target,
        PreviewResolutionScale::Full,
        color_context(&sequence),
        &mut |request: PreviewTimelineMediaRequest| {
            scheduled.push(request);
            pending_media_frame()
        },
        &mut |_| panic!("temporal media must not request titles"),
    );
    assert!(matches!(
        pending,
        PreviewTimelineResolution::Pending {
            dependency: PreviewTimelinePendingDependency::Temporal { pending_sources: 2, .. },
            ..
        }
    ));
    assert_eq!(
        scheduled.iter().map(|request| request.source_sample.time()).collect::<Vec<_>>(),
        vec![
            TimelineTime::ZERO,
            TimelineTime::new(1, 30).expect("future time")
        ]
    );

    let ready = resolve_preview_timeline(
        &sequence,
        &[],
        0,
        target,
        PreviewResolutionScale::Full,
        color_context(&sequence),
        &mut |request: PreviewTimelineMediaRequest| {
            let red = if request.source_sample.time().is_zero() {
                0.0
            } else {
                1.0
            };
            ready_temporal_media_frame(&request, red, 2)
        },
        &mut |_| panic!("temporal media must not request titles"),
    );
    let PreviewTimelineResolution::Ready(ready) = ready else {
        panic!("complete finite lookahead must publish");
    };
    let [ResolvedPreviewElement::Media { frame, .. }] = ready.plan.elements.as_slice() else {
        panic!("future temporal media must use the ordinary compositor seam");
    };
    let pixel = frame.working_frame().expect("working temporal output").frame.rgba_f32().data[0];
    assert!(
        (pixel[0] - 0.25).abs() < 1.0e-6,
        "unexpected lookahead blend: {pixel:?}"
    );
}

#[test]
fn temporal_preview_cache_identity_is_generation_and_source_complete() {
    let (sequence, _, _) = temporal_media_sequence();
    let target = sequence.settings.resolution;
    let programs = RefCell::new(PreparedVisualProgramCache::default());
    let scratch = RefCell::new(TimelineCompositeScratch::default());

    let resolve = |generation: u64,
                   cancellation: ExecutionCancellationToken,
                   identity_salt: u64|
     -> PreviewTimelineResolution {
        let graph = PreviewTimelineGraph {
            programs: &programs,
            scratch: &scratch,
            dependency_observer: None,
            generation,
            cancellation: &cancellation,
            author_snapshot: None,
            heterogeneous_graph_budget: standalone_preview_heterogeneous_graph_budget(),
        };
        let mut media = |request: PreviewTimelineMediaRequest| {
            let red = if request.source_sample.time() < TimelineTime::ZERO {
                0.0
            } else {
                1.0
            };
            ready_temporal_media_frame(&request, red, identity_salt)
        };
        let mut title = |_| panic!("temporal media must not request titles");
        resolve_preview_timeline_with_graph(
            PreviewTimelineFrameRequest::new(
                &sequence,
                &[],
                0,
                target,
                PreviewResolutionScale::Full,
                color_context(&sequence),
            ),
            PreviewTimelineSourceAdapters::new(&mut media, &mut title),
            graph,
        )
    };
    let key = |resolution: PreviewTimelineResolution| {
        let PreviewTimelineResolution::Ready(ready) = resolution else {
            panic!("temporal Preview should resolve");
        };
        ready.plan.cache_key
    };

    let first = key(resolve(71, ExecutionCancellationToken::new(), 10));
    let same = key(resolve(71, ExecutionCancellationToken::new(), 10));
    assert_eq!(
        first, same,
        "same generation and complete source identity must be stable"
    );
    let changed_source = key(resolve(71, ExecutionCancellationToken::new(), 11));
    assert_ne!(
        first, changed_source,
        "provider revision evidence must participate in final Preview identity"
    );
    let changed_generation = key(resolve(72, ExecutionCancellationToken::new(), 10));
    assert_ne!(
        first, changed_generation,
        "a seek-generation rotation must not reuse the previous temporal output"
    );

    let canceled = ExecutionCancellationToken::new();
    canceled.cancel();
    let mut media_calls = 0usize;
    let graph = PreviewTimelineGraph {
        programs: &programs,
        scratch: &scratch,
        dependency_observer: None,
        generation: 73,
        cancellation: &canceled,
        author_snapshot: None,
        heterogeneous_graph_budget: standalone_preview_heterogeneous_graph_budget(),
    };
    let mut media = |_: PreviewTimelineMediaRequest| {
        media_calls += 1;
        pending_media_frame()
    };
    let mut title = |_| panic!("temporal media must not request titles");
    let canceled_result = resolve_preview_timeline_with_graph(
        PreviewTimelineFrameRequest::new(
            &sequence,
            &[],
            0,
            target,
            PreviewResolutionScale::Full,
            color_context(&sequence),
        ),
        PreviewTimelineSourceAdapters::new(&mut media, &mut title),
        graph,
    );
    assert!(matches!(
        canceled_result,
        PreviewTimelineResolution::Unavailable { .. }
    ));
    assert_eq!(
        media_calls, 0,
        "cancellation must stop before any scheduler/Frame Store source request"
    );
}

#[test]
fn preview_executes_cross_dissolve_through_shared_working_compositor() {
    ensure_mondrian_default_ocio_loaded().expect("default OCIO");
    let mut sequence = Sequence::new("preview Cross Dissolve");
    let time_base = sequence.time_base();
    let left = Clip::new_solid_color(
        AssetId::new(),
        Color::from_rgba8(255, 0, 0, 255),
        tt(0, time_base),
        tt(2, time_base),
    )
    .expect("left solid");
    let right = Clip::new_solid_color(
        AssetId::new(),
        Color::from_rgba8(0, 0, 255, 255),
        tt(2, time_base),
        tt(2, time_base),
    )
    .expect("right solid");
    let (left_id, right_id) = (left.id, right.id);
    sequence.video_tracks[0].add_clip(left).expect("left placement");
    sequence.video_tracks[0].add_clip(right).expect("right placement");
    sequence
        .video_transitions
        .push(mondrian_timeline::VideoTransition::cross_dissolve(
            left_id,
            right_id,
            mondrian_core::TimelineTimeRange::new(tt(1, time_base), tt(2, time_base))
                .expect("transition range"),
        ));
    sequence.validate_author_identities().expect("valid author graph");
    let target = Resolution { width: 1, height: 1 };
    let context = color_context(&sequence);
    let mut unexpected_media = |_| panic!("solid Transition must not request media");
    let PreviewTimelineResolution::Ready(resolved) = resolve_preview_timeline(
        &sequence,
        &[],
        2,
        target,
        PreviewResolutionScale::Full,
        context.clone(),
        &mut unexpected_media,
        &mut |_| panic!("solid Transition must not request titles"),
    ) else {
        panic!("Cross Dissolve must resolve");
    };
    assert!(matches!(
        resolved.plan.elements.as_slice(),
        [ResolvedPreviewElement::CrossDissolve { progress, .. }] if *progress == 0.5
    ));
    let mut scratch = TimelineCompositeScratch::default();
    let output = composite_resolved_preview_working(
        1,
        1,
        &resolved.plan.elements,
        &resolved.plan.color_context,
        &mut scratch,
    )
    .expect("Preview Cross Dissolve composite");
    let pixel = output.frame.rgba_f32().data[0];
    assert!((pixel[0] - 0.5).abs() < 1.0e-6, "unexpected red: {pixel:?}");
    assert_eq!(pixel[1], 0.0);
    assert!(
        (pixel[2] - 0.5).abs() < 1.0e-6,
        "unexpected blue: {pixel:?}"
    );
    assert_eq!(pixel[3], 1.0);
    assert_eq!(output.composite_diagnostics.float_linear_composites, 1);
}

#[test]
fn temporal_transition_keeps_both_exact_endpoint_values() {
    let mut sequence = Sequence::new("temporal Cross Dissolve");
    sequence.settings.frame_rate = Rational::new(30, 1);
    sequence.settings.resolution = Resolution { width: 2, height: 1 };
    let time_base = sequence.time_base();
    let mut left = Clip::new_solid_color(
        AssetId::new(),
        Color::from_rgba8(255, 0, 0, 255),
        tt(0, time_base),
        tt(30, time_base),
    )
    .expect("left solid");
    left.add_effect_node(temporal_blend_effect(
        TimelineTime::new(1, 30).expect("one-frame history"),
    ));
    let mut right = Clip::new_solid_color(
        AssetId::new(),
        Color::from_rgba8(0, 0, 255, 255),
        tt(30, time_base),
        tt(30, time_base),
    )
    .expect("right solid");
    right.add_effect_node(temporal_blend_effect(
        TimelineTime::new(1, 30).expect("one-frame history"),
    ));
    let (left_id, right_id) = (left.id, right.id);
    sequence.video_tracks[0].add_clip(left).expect("left placement");
    sequence.video_tracks[0].add_clip(right).expect("right placement");
    sequence
        .video_transitions
        .push(mondrian_timeline::VideoTransition::cross_dissolve(
            left_id,
            right_id,
            mondrian_core::TimelineTimeRange::new(
                TimelineTime::new(29, 30).expect("transition start"),
                TimelineTime::new(2, 30).expect("transition duration"),
            )
            .expect("transition range"),
        ));
    sequence.validate_author_identities().expect("valid author graph");

    let mut unexpected_media = |_| panic!("temporal Solid Color must not request media");
    let PreviewTimelineResolution::Ready(resolved) = resolve_preview_timeline(
        &sequence,
        &[],
        30,
        sequence.settings.resolution,
        PreviewResolutionScale::Full,
        color_context(&sequence),
        &mut unexpected_media,
        &mut |_| panic!("temporal Solid Color must not request titles"),
    ) else {
        panic!("temporal Transition should resolve");
    };
    let [ResolvedPreviewElement::CrossDissolve { left, right, progress }] =
        resolved.plan.elements.as_slice()
    else {
        panic!("both temporal endpoints must lower as their prepared pixel values");
    };
    assert!(matches!(
        left.as_ref(),
        ResolvedPreviewTransitionInput::Media { .. }
    ));
    assert!(matches!(
        right.as_ref(),
        ResolvedPreviewTransitionInput::Media { .. }
    ));
    assert_eq!(*progress, 0.5);
}

#[test]
fn temporal_nested_history_fails_closed_before_zero_and_recurses_when_valid() {
    ensure_mondrian_default_ocio_loaded().expect("default OCIO");
    let mut child = solid_sequence("temporal child", Color::from_rgba8(32, 96, 224, 255));
    child.settings.frame_rate = Rational::new(30, 1);
    child.settings.resolution = Resolution { width: 2, height: 1 };
    child.settings.preview.resolution_scale = 1.0;
    let mut parent = Sequence::new("temporal parent");
    parent.settings.frame_rate = Rational::new(30, 1);
    parent.settings.resolution = Resolution { width: 2, height: 1 };
    let time_base = parent.time_base();
    let mut nested = Clip::new_nested_sequence(
        child.id,
        TimelineTime::ZERO,
        tt(30, time_base),
        Some("temporal child".to_owned()),
    )
    .expect("nested Clip");
    nested.add_effect_node(temporal_blend_effect(
        TimelineTime::new(1, 30).expect("one-frame history"),
    ));
    parent.video_tracks[0].add_clip(nested).expect("nested placement");

    let before_handle = resolve_preview_timeline(
        &parent,
        std::slice::from_ref(&child),
        0,
        parent.settings.resolution,
        PreviewResolutionScale::Full,
        color_context(&parent),
        &mut |_| panic!("nested Solid Color must not request media"),
        &mut |_| panic!("nested Solid Color must not request titles"),
    );
    assert!(
        matches!(
            before_handle,
            PreviewTimelineResolution::Unavailable { ref reason }
                if reason.detail().contains("insufficient source handle")
        ),
        "negative nested history must not clamp to the child's current frame"
    );

    let valid = resolve_preview_timeline(
        &parent,
        std::slice::from_ref(&child),
        1,
        parent.settings.resolution,
        PreviewResolutionScale::Full,
        color_context(&parent),
        &mut |_| panic!("nested Solid Color must not request media"),
        &mut |_| panic!("nested Solid Color must not request titles"),
    );
    let PreviewTimelineResolution::Ready(valid) = valid else {
        panic!("nested history with complete child handles should resolve");
    };
    assert!(matches!(
        valid.plan.elements.as_slice(),
        [ResolvedPreviewElement::Media { .. }]
    ));
}

#[test]
fn nested_temporal_preview_and_export_share_prepared_semantics_and_pixels() {
    ensure_mondrian_default_ocio_loaded().expect("default OCIO");
    let resolution = Resolution { width: 2, height: 1 };
    let frame_rate = Rational::new(30, 1);
    let one_frame = TimelineTime::new(1, 30).expect("one frame");

    let mut child = Sequence::new("temporal parity child");
    child.settings.frame_rate = frame_rate;
    child.settings.resolution = resolution;
    child.settings.preview.resolution_scale = 1.0;
    child.video_tracks.clear();
    let mut child_track = mondrian_timeline::Track::new_video("V1");
    child_track
        .add_clip(
            Clip::new_solid_color(
                AssetId::new(),
                Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 },
                TimelineTime::ZERO,
                one_frame,
            )
            .expect("red child frame"),
        )
        .expect("insert red child frame");
    child_track
        .add_clip(
            Clip::new_solid_color(
                AssetId::new(),
                Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 },
                one_frame,
                TimelineTime::new(2, 30).expect("two frames"),
            )
            .expect("blue child frames"),
        )
        .expect("insert blue child frames");
    child.video_tracks.push(child_track);

    let mut parent = Sequence::new("temporal parity parent");
    parent.settings.frame_rate = frame_rate;
    parent.settings.resolution = resolution;
    parent.settings.preview.resolution_scale = 1.0;
    let mut nested = Clip::new_nested_sequence(
        child.id,
        TimelineTime::ZERO,
        TimelineTime::new(3, 30).expect("three frames"),
        Some("temporal parity child".to_owned()),
    )
    .expect("nested Clip");
    nested.add_effect_node(temporal_blend_effect(one_frame));
    let nested_clip_id = nested.id;
    parent.video_tracks[0].add_clip(nested).expect("insert nested Clip");

    let preview = resolve_preview_timeline(
        &parent,
        std::slice::from_ref(&child),
        1,
        resolution,
        PreviewResolutionScale::Full,
        color_context(&parent),
        &mut |_| panic!("nested Solid Color must not request media"),
        &mut |_| panic!("nested Solid Color must not request titles"),
    );
    let PreviewTimelineResolution::Ready(preview) = preview else {
        panic!("Preview temporal parity frame must resolve");
    };
    let mut preview_scratch = TimelineCompositeScratch::default();
    let preview_working = composite_resolved_preview_working(
        resolution.width,
        resolution.height,
        &preview.plan.elements,
        &preview.plan.color_context,
        &mut preview_scratch,
    )
    .expect("Preview working composite");

    let range = mondrian_export::preset::TimelineExportRange::WorkArea {
        start_frame: 1,
        end_frame_exclusive: 2,
    };
    let prepared = mondrian_export::prepare_timeline_export_dependencies(
        &parent,
        std::slice::from_ref(&child),
        range,
        false,
    )
    .expect("prepare immutable Export Programs");
    let export_snapshot = mondrian_export::preset::TimelineExportSnapshot::captured(
        mondrian_core::ProjectColorEnvironment::default(),
        parent.clone(),
        vec![child.clone()],
        std::collections::HashMap::new(),
        range,
        prepared.execution_snapshot().clone(),
    );
    let export =
        mondrian_export::queue::export_visual_frame_validation(&export_snapshot, 1, resolution)
            .expect("Export working composite");

    assert_eq!(
        preview.semantic_trace, export.semantic_trace,
        "Preview and Export must consume the same prepared Program, nested projection, temporal sample set, instance paths, ROI, and compiled graph identity"
    );
    assert_eq!(
        preview_working.frame, export.working_frame,
        "the same prepared semantics must produce identical working-linear pixels"
    );

    let root = &preview.semantic_trace.nodes[0];
    assert_eq!(root.sequence_id, parent.id);
    assert_eq!(root.frame, 1);
    assert_eq!(root.temporal_batches.len(), 1);
    let batch = &root.temporal_batches[0];
    assert_eq!(batch.placement.clip_id, nested_clip_id);
    assert_eq!(batch.output_time, one_frame);
    assert_eq!(batch.frame_extent, EffectFrameExtent::new(2, 1));
    assert_eq!(
        batch.output_roi,
        EffectFrameExtent::new(2, 1).full_frame_roi()
    );
    assert_ne!(
        batch.effect_graph_fingerprint,
        mondrian_effects::identity_compiled_effect_graph()
            .expect("identity graph")
            .semantic_fingerprint(),
        "the parity gate must exercise the real temporal graph"
    );
    assert_eq!(batch.source_samples.len(), 2);
    assert_eq!(
        batch
            .source_samples
            .iter()
            .map(|sample| sample.effect_request.time)
            .collect::<Vec<_>>(),
        vec![one_frame, TimelineTime::ZERO]
    );
    for (sample, expected_time) in batch.source_samples.iter().zip([one_frame, TimelineTime::ZERO])
    {
        let mondrian_renderer::PreparedVisualExecutionTemporalSourceKindTrace::NestedSequence {
            sequence_id,
            source_sample,
            child_node_index,
            ..
        } = &sample.source
        else {
            panic!("the parity gate must retain nested temporal source semantics");
        };
        assert_eq!(*sequence_id, child.id);
        assert_eq!(source_sample.time(), expected_time);
        let child_node = &preview.semantic_trace.nodes[*child_node_index];
        assert_eq!(child_node.sequence_id, child.id);
        assert_eq!(child_node.time, expected_time);
        assert_eq!(child_node.instance_path.len(), 1);
        assert_eq!(
            child_node.instance_path[0],
            mondrian_renderer::PreparedVisualExecutionInstanceStepTrace {
                placement: sample.placement,
                sample: mondrian_renderer::PreparedVisualExecutionSampleTrace::Temporal(
                    sample.effect_request,
                ),
            }
        );
    }
    assert_eq!(root.nested_bindings.len(), 2);
    assert!(root.nested_bindings.iter().all(|binding| {
        matches!(
            binding.sample,
            mondrian_renderer::PreparedVisualExecutionSampleTrace::Temporal(_)
        )
    }));
    assert_eq!(
        root.nested_bindings
            .iter()
            .map(|binding| binding.source_sample.time())
            .collect::<Vec<_>>(),
        vec![one_frame, TimelineTime::ZERO]
    );
    assert_eq!(preview.semantic_trace.nodes.len(), 3);
    assert!(preview.semantic_trace.nodes[1..].iter().all(|node| {
        node.sequence_id == child.id
            && node.instance_path.len() == 1
            && matches!(
                node.instance_path[0].sample,
                mondrian_renderer::PreparedVisualExecutionSampleTrace::Temporal(_)
            )
    }));
    let pixel = preview_working.frame.rgba_f32().data[0];
    assert!(
        (pixel[0] - 0.25).abs() < 1.0e-6
            && pixel[1].abs() < 1.0e-6
            && (pixel[2] - 0.75).abs() < 1.0e-6
            && (pixel[3] - 1.0).abs() < 1.0e-6,
        "past temporal blend must combine child frame 1 blue with child frame 0 red: {pixel:?}"
    );
}

#[test]
fn nested_sequence_uses_shared_recursion_and_emits_execution_facts() {
    ensure_mondrian_default_ocio_loaded().expect("default OCIO");
    let child = solid_sequence("child", Color::from_rgba8(48, 120, 220, 255));
    let child_id = child.id;
    let mut parent = Sequence::new("parent");
    let time_base = parent.time_base();
    parent.video_tracks[0]
        .add_clip(
            Clip::new_nested_sequence(
                child_id,
                tt(0, time_base),
                tt(24, time_base),
                Some("child".to_owned()),
            )
            .expect("valid nested clip"),
        )
        .expect("insert nested clip");
    let mut unexpected_media = |_| panic!("nested solid plan must not request media");

    let resolution = resolve_preview_timeline(
        &parent,
        std::slice::from_ref(&child),
        3,
        Resolution { width: 64, height: 36 },
        PreviewResolutionScale::Full,
        color_context(&parent),
        &mut unexpected_media,
        &mut |_| panic!("nested solid plan must not request titles"),
    );
    let PreviewTimelineResolution::Ready(resolved) = resolution else {
        panic!("nested Timeline should resolve");
    };
    assert_eq!(resolved.plan.elements.len(), 1);
    assert!(resolved
        .facts
        .iter()
        .any(|fact| matches!(fact, PreviewTimelineExecutionFact::Composite(_))));
    assert!(resolved
        .facts
        .iter()
        .any(|fact| matches!(fact, PreviewTimelineExecutionFact::CpuExecution(_))));
}

#[test]
fn nested_sequence_keeps_its_own_canvas_under_shared_runtime_quality() {
    let mut child = Sequence::new("child media");
    child.settings.resolution = Resolution { width: 1280, height: 720 };
    child.settings.preview.resolution_scale = 0.5;
    child.settings.color.input.auto_tone_map_media = false;
    let asset_id = AssetId::new();
    let child_time_base = child.time_base();
    child.video_tracks[0]
        .add_clip(
            Clip::new(asset_id, tt(0, child_time_base), tt(24, child_time_base))
                .expect("media clip"),
        )
        .expect("insert child media");

    let mut parent = Sequence::new("parent");
    let parent_time_base = parent.time_base();
    parent.video_tracks[0]
        .add_clip(
            Clip::new_nested_sequence(
                child.id,
                tt(0, parent_time_base),
                tt(24, parent_time_base),
                Some("child media".to_owned()),
            )
            .expect("nested clip"),
        )
        .expect("insert nested clip");

    let demands = collect_preview_timeline_media_demands(
        &parent,
        std::slice::from_ref(&child),
        0,
        Resolution { width: 64, height: 36 },
        PreviewResolutionScale::Quarter,
        color_context(&parent),
    )
    .expect("nested media demands");
    assert_eq!(demands.len(), 1);
    assert_eq!(demands[0].asset_id, asset_id);
    assert!(
        !demands[0].input_color.input_tone_map,
        "nested media input policy must come from the child Sequence, not parent Program Output"
    );
    assert_eq!(
        demands[0].target_resolution,
        Resolution { width: 160, height: 90 }
    );

    let mut observed_resolution = None;
    let mut media = |request: PreviewTimelineMediaRequest| {
        observed_resolution = Some(request.target_resolution);
        pending_media_frame()
    };
    let result = resolve_preview_timeline(
        &parent,
        std::slice::from_ref(&child),
        0,
        Resolution { width: 64, height: 36 },
        PreviewResolutionScale::Quarter,
        color_context(&parent),
        &mut media,
        &mut |_| panic!("nested media plan must not request titles"),
    );

    assert!(matches!(
        result,
        PreviewTimelineResolution::Pending {
            dependency: PreviewTimelinePendingDependency::Media { asset_id: pending_id, .. },
            ..
        } if pending_id == asset_id
    ));
    assert_eq!(
        observed_resolution,
        Some(Resolution { width: 160, height: 90 })
    );
}

#[test]
fn nested_sequence_projects_exact_time_onto_the_child_evaluation_grid() {
    let mut child = Sequence::new("30 fps child");
    child.settings.frame_rate = Rational::new(30, 1);
    let asset_id = AssetId::new();
    let child_time_base = child.time_base();
    child.video_tracks[0]
        .add_clip(
            Clip::new(asset_id, tt(0, child_time_base), tt(30, child_time_base))
                .expect("child media clip"),
        )
        .expect("insert child media");

    let mut parent = Sequence::new("24 fps parent");
    parent.settings.frame_rate = Rational::new(24, 1);
    let parent_time_base = parent.time_base();
    parent.video_tracks[0]
        .add_clip(
            Clip::new_nested_sequence(
                child.id,
                tt(0, parent_time_base),
                tt(24, parent_time_base),
                Some("30 fps child".to_owned()),
            )
            .expect("nested clip"),
        )
        .expect("insert nested clip");

    let demands = collect_preview_timeline_media_demands(
        &parent,
        std::slice::from_ref(&child),
        12,
        Resolution { width: 64, height: 36 },
        PreviewResolutionScale::Full,
        color_context(&parent),
    )
    .expect("mixed-rate nested media demands");

    assert_eq!(demands.len(), 1);
    assert_eq!(demands[0].asset_id, asset_id);
    assert_eq!(
        demands[0].source_sample.time(),
        mondrian_core::TimelineTime::new(1, 2).expect("exact half second")
    );
}

#[test]
fn media_pending_and_unavailable_are_distinct_terminal_shapes() {
    let mut sequence = Sequence::new("media");
    let asset_id = AssetId::new();
    let time_base = sequence.time_base();
    sequence.video_tracks[0]
        .add_clip(Clip::new(asset_id, tt(0, time_base), tt(24, time_base)).expect("media clip"))
        .expect("insert media clip");
    let target = Resolution { width: 64, height: 36 };

    let mut pending = |_| pending_media_frame();
    assert!(matches!(
        resolve_preview_timeline(
            &sequence,
            &[],
            0,
            target,
            PreviewResolutionScale::Full,
            color_context(&sequence),
            &mut pending,
            &mut |_| panic!("media plan must not request titles"),
        ),
        PreviewTimelineResolution::Pending {
            dependency: PreviewTimelinePendingDependency::Media { asset_id: pending_id, .. },
            ..
        } if pending_id == asset_id
    ));

    let mut unavailable = |_| PreviewTimelineMediaFrame::Unavailable {
        reason: PreviewUnavailability::blocked(PreviewOutputStage::MediaResolution, "offline"),
    };
    assert!(matches!(
        resolve_preview_timeline(
            &sequence,
            &[],
            0,
            target,
            PreviewResolutionScale::Full,
            color_context(&sequence),
            &mut unavailable,
            &mut |_| panic!("media plan must not request titles"),
        ),
        PreviewTimelineResolution::Unavailable { reason } if reason.detail().contains("offline")
    ));
}

#[test]
fn missing_nested_sequence_is_explicitly_unavailable() {
    let mut parent = Sequence::new("parent");
    let time_base = parent.time_base();
    let missing_id = SequenceId::new();
    parent.video_tracks[0]
        .add_clip(
            Clip::new_nested_sequence(missing_id, tt(0, time_base), tt(24, time_base), None)
                .expect("nested clip"),
        )
        .expect("insert nested clip");
    let mut unexpected_media = |_| panic!("missing nested plan must not request media");

    let demand_error = collect_preview_timeline_media_demands(
        &parent,
        &[],
        0,
        Resolution { width: 64, height: 36 },
        PreviewResolutionScale::Full,
        color_context(&parent),
    )
    .expect_err("missing nested demand must fail");
    assert!(demand_error.detail().contains(&missing_id.to_string()));

    assert!(matches!(
        resolve_preview_timeline(
            &parent,
            &[],
            0,
            Resolution { width: 64, height: 36 },
            PreviewResolutionScale::Full,
            color_context(&parent),
            &mut unexpected_media,
            &mut |_| panic!("missing nested plan must not request titles"),
        ),
        PreviewTimelineResolution::Unavailable { reason } if reason.detail().contains(&missing_id.to_string())
    ));
}

#[test]
fn empty_nested_sequence_is_transparent_instead_of_blocking_parent_output() {
    let child = Sequence::new("empty-child");
    let mut parent = Sequence::new("parent");
    let time_base = parent.time_base();
    parent.video_tracks[0]
        .add_clip(
            Clip::new_nested_sequence(child.id, tt(0, time_base), tt(24, time_base), None)
                .expect("nested clip"),
        )
        .expect("insert nested clip");
    let mut unexpected_media = |_| panic!("empty nested Sequence must not request media");

    let PreviewTimelineResolution::Ready(resolved) = resolve_preview_timeline(
        &parent,
        &[child],
        0,
        Resolution { width: 64, height: 36 },
        PreviewResolutionScale::Full,
        color_context(&parent),
        &mut unexpected_media,
        &mut |_| panic!("empty nested plan must not request titles"),
    ) else {
        panic!("empty nested Sequence must resolve as a transparent layer");
    };
    assert_eq!(resolved.plan.elements.len(), 1);
    let ResolvedPreviewElement::Media { frame, .. } = &resolved.plan.elements[0] else {
        panic!("nested Sequence must lower to a media layer");
    };
    let working = frame.working_frame().expect("transparent working frame");
    assert!(working.frame.rgba_f32().data.iter().all(|pixel| *pixel == [0.0; 4]));
}

#[test]
fn basic_title_enters_the_shared_working_linear_preview_path() {
    ensure_mondrian_default_ocio_loaded().expect("default OCIO");
    let mut sequence = Sequence::new("Basic Title");
    sequence.settings.resolution = Resolution { width: 640, height: 360 };
    let time_base = sequence.time_base();
    sequence.video_tracks[0]
        .add_clip(
            Clip::new_basic_title(
                "Mondrian",
                mondrian_core::default_basic_title_font_family(),
                tt(0, time_base),
                tt(24, time_base),
            )
            .expect("title"),
        )
        .expect("title placement");
    let mut rasterizer = mondrian_renderer::BasicTitleRasterizer::new();
    let mut title_frame = |request: PreviewTimelineTitleRequest| {
        PreviewTimelineTitleFrame::Ready(
            rasterizer
                .rasterize(
                    &request.title,
                    request.author_resolution,
                    request.title_safe_margin,
                    request.target_resolution,
                    request.working_color_space,
                )
                .expect("title raster"),
        )
    };
    let mut unexpected_media = |_| panic!("Basic Title must not request media decode");

    let PreviewTimelineResolution::Ready(resolved) = resolve_preview_timeline(
        &sequence,
        &[],
        0,
        Resolution { width: 320, height: 180 },
        PreviewResolutionScale::Full,
        color_context(&sequence),
        &mut unexpected_media,
        &mut title_frame,
    ) else {
        panic!("Basic Title must resolve");
    };
    let [ResolvedPreviewElement::Media { frame, .. }] = resolved.plan.elements.as_slice() else {
        panic!("Basic Title must lower to the shared source path");
    };
    let working = frame.working_frame().expect("working title");
    assert_eq!(
        working.frame.descriptor().alpha,
        mondrian_renderer::ColorFrameAlpha::StraightCoverage
    );
    assert!(working.frame.rgba_f32().data.iter().any(|pixel| pixel[3] > 0.0));
    assert_eq!(frame.decode_execution().media_layers, 0);
}

#[test]
fn nested_sequence_propagates_non_reusable_inner_execution_semantics() {
    ensure_mondrian_default_ocio_loaded().expect("default OCIO");
    let target = Resolution { width: 1, height: 1 };
    let asset_id = AssetId::new();
    let mut child = Sequence::new("nested-cache-policy-child");
    child.settings.resolution = target;
    let child_time_base = child.time_base();
    child.video_tracks[0]
        .add_clip(
            Clip::new(asset_id, TimelineTime::ZERO, tt(24, child_time_base)).expect("child media"),
        )
        .expect("insert child media");

    let mut root = Sequence::new("nested-cache-policy-root");
    root.settings.resolution = target;
    let root_time_base = root.time_base();
    root.video_tracks[0]
        .add_clip(
            Clip::new_nested_sequence(child.id, TimelineTime::ZERO, tt(24, root_time_base), None)
                .expect("nested placement"),
        )
        .expect("insert nested placement");

    let mut media_frame = |request: PreviewTimelineMediaRequest| {
        assert_eq!(request.asset_id, asset_id);
        let mut identity =
            PreviewSemanticIdentityBuilder::new(b"mondrian.preview.test-nested-source.v1");
        std::hash::Hash::hash(&request.asset_id, &mut identity);
        std::hash::Hash::hash(&request.source_sample, &mut identity);
        PreviewTimelineMediaFrame::Ready(
            MediaPreviewFrame::from_working(
                CpuColorFrame::working(mondrian_core::WorkingRgbaF32Frame {
                    width: request.target_resolution.width,
                    height: request.target_resolution.height,
                    data: vec![[0.25, 0.5, 0.75, 1.0]],
                    color_space: request.input_color.working_color_space,
                }),
                request.target_resolution,
                identity.finish_identity(),
                mondrian_playback::FramePresentationQuality::Ready,
                super::super::preview_execution::PreviewDecodeExecutionSummary::default(),
            )
            .with_cross_call_reuse(false),
        )
    };
    let PreviewTimelineResolution::Ready(resolved) = resolve_preview_timeline(
        &root,
        std::slice::from_ref(&child),
        0,
        target,
        PreviewResolutionScale::Full,
        color_context(&root),
        &mut media_frame,
        &mut |_| panic!("media-only nesting must not request titles"),
    ) else {
        panic!("nested Preview must resolve");
    };
    let [ResolvedPreviewElement::Media { frame, .. }] = resolved.plan.elements.as_slice() else {
        panic!("nested Sequence must lower to one resolved media layer");
    };

    assert!(
        !frame.permits_cross_call_reuse() && !resolved.plan.cache_reusable,
        "a nested stateful/uncacheable dependency must prevent outer Viewer reuse"
    );
    assert!(resolved.plan.render_cache_identity.is_none());
}

#[test]
fn nested_child_source_change_rotates_root_render_cache_identity() {
    ensure_mondrian_default_ocio_loaded().expect("default OCIO");
    let target = Resolution { width: 1, height: 1 };
    let asset_id = AssetId::new();
    let mut child = Sequence::new("nested-identity-child");
    child.settings.resolution = target;
    let child_time_base = child.time_base();
    child.video_tracks[0]
        .add_clip(
            Clip::new(asset_id, TimelineTime::ZERO, tt(24, child_time_base)).expect("child media"),
        )
        .expect("insert child media");

    let mut root = Sequence::new("nested-identity-root");
    root.settings.resolution = target;
    let root_time_base = root.time_base();
    root.video_tracks[0]
        .add_clip(
            Clip::new_nested_sequence(child.id, TimelineTime::ZERO, tt(24, root_time_base), None)
                .expect("nested placement"),
        )
        .expect("insert nested placement");

    let resolve = |salt: u64| {
        let mut media_frame = |request: PreviewTimelineMediaRequest| {
            let mut identity =
                PreviewSemanticIdentityBuilder::new(b"mondrian.preview.test-nested-identity.v1");
            std::hash::Hash::hash(&request.asset_id, &mut identity);
            std::hash::Hasher::write_u64(&mut identity, salt);
            PreviewTimelineMediaFrame::Ready(MediaPreviewFrame::from_working(
                CpuColorFrame::working(mondrian_core::WorkingRgbaF32Frame {
                    width: 1,
                    height: 1,
                    data: vec![[0.25, 0.5, 0.75, 1.0]],
                    color_space: request.input_color.working_color_space,
                }),
                target,
                identity.finish_identity(),
                FramePresentationQuality::Ready,
                PreviewDecodeExecutionSummary::default(),
            ))
        };
        let result = resolve_preview_timeline(
            &root,
            std::slice::from_ref(&child),
            0,
            target,
            PreviewResolutionScale::Full,
            color_context(&root),
            &mut media_frame,
            &mut |_| panic!("media-only nesting must not request titles"),
        );
        let PreviewTimelineResolution::Ready(result) = result else {
            panic!("nested Preview must resolve");
        };
        result.plan.render_cache_identity.expect("reusable identity")
    };

    assert_ne!(resolve(1), resolve(2));
}

#[test]
fn preview_materializer_has_no_raw_sequence_relookup_seam() {
    let source = include_str!("../preview_timeline_execution.rs");
    let context = source
        .split("struct PreviewTimelineExecutionAdapter")
        .nth(1)
        .and_then(|suffix| suffix.split("fn prepared_visual_node").next())
        .expect("Preview materialization context source");

    assert!(!context.contains("Sequence"));
    assert!(!context.contains("root_sequence"));
    assert!(!context.contains("sequences:"));
    assert!(!source.contains("fn sequence_by_id"));
    assert!(!source.contains(".settings.resolution"));
    assert!(!source.contains(".settings.preview.resolution_scale"));
    assert!(!source.contains(".settings.title_safe_margin"));
}
