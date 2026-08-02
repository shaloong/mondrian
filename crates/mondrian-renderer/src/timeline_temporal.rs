//! Two-phase finite-history preparation for Timeline visual execution.
//!
//! Graph demand collection is pure and decode-free. Concrete Preview and
//! Export Adapters resolve every [`TimelineTemporalSourceDemand`] through
//! their existing scheduler/job infrastructure, freeze the resulting tiles in
//! `PreparedTemporalFrameSet`, and only then execute the Effect graph.

use crate::{
    PreparedVisualProgram, TimelineBasicTitlePlan, TimelineMediaPlan, TimelineNestedSequencePlan,
    TimelineRenderPlan, TimelineRenderPlanElement, TimelineSolidColorPlan,
    TimelineTransitionInputPlan,
};
use mondrian_core::timeline_data::{
    AlphaInterpretation, NestedColorProcessing, TimelineClipExecutionRef,
};
use mondrian_core::{
    AssetId, Color, ColorSpace, ExecutionCancellationToken, FrameRounding, SequenceId, TimelineTime,
};
use mondrian_effects::{
    collect_temporal_frame_demands, identity_compiled_effect_graph, CompiledEffectGraph,
    EffectExecutionContinuity, EffectExecutionSession, EffectFrameExtent, EffectPixelRoi,
    EffectTemporalExecutionError, EffectTemporalExecutionOutput, EffectTemporalExecutionRequest,
    EffectTemporalFrameDemandBatch, EffectTemporalFrameRequest, EffectTemporalSpan,
    PreparedTemporalFrameSet,
};
use std::sync::Arc;

/// One concrete source value required before temporal Effect execution.
#[derive(Debug, Clone)]
pub struct TimelineTemporalSourceDemand {
    /// Exact graph request this source value must satisfy.
    pub effect_request: EffectTemporalFrameRequest,
    /// Prepared placement whose canonical mapping is authoritative.
    pub placement: TimelineClipExecutionRef,
    /// Concrete source semantics to resolve into straight-alpha scene-linear
    /// Float32 pixels in the Sequence working space.
    pub source: TimelineTemporalSource,
}

/// Closed source algebra admitted by the first temporal production tracer.
#[derive(Debug, Clone)]
pub enum TimelineTemporalSource {
    /// File-backed media at one exact interpreted decode target.
    Media {
        /// Stable project asset identity.
        asset_id: AssetId,
        /// Exact source time after the Clip retime and optional interpretation
        /// frame grid.
        source_time: TimelineTime,
        /// Explicit input color override, if authored.
        color_space_override: Option<ColorSpace>,
        /// Explicit alpha interpretation that must be normalized before the
        /// tile enters the frozen set.
        alpha_interpretation: AlphaInterpretation,
        /// Sequence input policy for automatic tone mapping.
        auto_tone_map: bool,
    },
    /// Child Sequence composite sampled through the parent placement.
    NestedSequence {
        /// Child Sequence identity.
        sequence_id: SequenceId,
        /// Exact child-local time from the sole Clip retime map.
        source_time: TimelineTime,
        /// Child-to-parent working-space contract.
        color_processing: NestedColorProcessing,
    },
    /// Generated constant scene-linear straight-alpha source.
    SolidColor {
        /// Authored color value.
        color: Color,
    },
}

/// One graph plus its immutable source-demand batch.
#[derive(Debug, Clone)]
pub struct TimelineTemporalDemandBatch {
    placement: TimelineClipExecutionRef,
    graph: Arc<CompiledEffectGraph>,
    execution: EffectTemporalExecutionRequest,
    effect_demands: EffectTemporalFrameDemandBatch,
    source_demands: Arc<[TimelineTemporalSourceDemand]>,
}

/// One ordinary compositing plan plus every temporal source batch already
/// removed from that plan.
///
/// Temporal graphs in `execution_plan` are replaced by the canonical identity
/// graph exactly once. A consumer must resolve and execute every batch before
/// lowering the corresponding placement from this plan.
#[derive(Debug, Clone)]
pub struct PreparedTimelineTemporalExecution {
    execution_plan: TimelineRenderPlan,
    batches: Vec<TimelineTemporalDemandBatch>,
}

impl PreparedTimelineTemporalExecution {
    /// Render plan admitted only after all returned temporal batches execute.
    pub const fn execution_plan(&self) -> &TimelineRenderPlan {
        &self.execution_plan
    }

    /// Exact temporal batches in deterministic render order.
    pub fn batches(&self) -> &[TimelineTemporalDemandBatch] {
        &self.batches
    }

    /// Consume the preparation into its two owned products.
    pub fn into_parts(self) -> (TimelineRenderPlan, Vec<TimelineTemporalDemandBatch>) {
        (self.execution_plan, self.batches)
    }
}

impl TimelineTemporalDemandBatch {
    /// Placement whose source value enters the graph.
    pub const fn placement(&self) -> TimelineClipExecutionRef {
        self.placement
    }

    /// Unique compiled semantic IR that emitted these demands.
    pub fn graph(&self) -> &Arc<CompiledEffectGraph> {
        &self.graph
    }

    /// Exact Effect execution request.
    pub const fn execution_request(&self) -> &EffectTemporalExecutionRequest {
        &self.execution
    }

    /// Exact Effect-owned demand batch used when freezing resolved tiles.
    pub const fn effect_demands(&self) -> &EffectTemporalFrameDemandBatch {
        &self.effect_demands
    }

    /// Concrete source work that Preview or Export must resolve.
    pub fn source_demands(&self) -> &[TimelineTemporalSourceDemand] {
        &self.source_demands
    }
}

/// Fail-closed Timeline temporal preparation error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimelineTemporalPreparationError {
    /// Placement mapping or media interpretation failed.
    #[error("temporal source sampling failed for Clip {clip_id}: {reason}")]
    SourceSampling {
        /// Clip occurrence that failed.
        clip_id: mondrian_core::ClipId,
        /// Exact mapping failure.
        reason: String,
    },
    /// The one compiled graph rejected temporal demand collection.
    #[error("temporal Effect demand collection failed for Clip {clip_id}: {reason}")]
    EffectDemand {
        /// Clip occurrence that failed.
        clip_id: mondrian_core::ClipId,
        /// Exact graph admission or arithmetic failure.
        reason: String,
    },
    /// A Basic Title snapshot was evaluated only at the current Clip time and
    /// cannot truthfully supply historical title pixels.
    #[error("Clip {clip_id} is a Basic Title; historical title evaluation is not yet admitted")]
    BasicTitleUnsupported {
        /// Basic Title Clip.
        clip_id: mondrian_core::ClipId,
    },
    /// Adjustment Layers have the accumulated lower Timeline stack as input,
    /// not a single placement source.
    #[error(
        "Clip {clip_id} is an Adjustment Layer; temporal stack-input execution is not yet admitted"
    )]
    AdjustmentUnsupported {
        /// Adjustment Clip.
        clip_id: mondrian_core::ClipId,
    },
    /// The canonical no-op graph was unavailable while lowering prepared
    /// temporal output into ordinary compositing.
    #[error("the canonical identity Effect graph is unavailable")]
    IdentityGraphUnavailable,
    /// A prepared placement did not resolve to exactly one graph value in the
    /// evaluated Render Plan.
    #[error(
        "temporal placement for Clip {clip_id} resolved to {matches} render-plan values; expected exactly one"
    )]
    PlacementResolution {
        /// Clip whose exact placement was prepared.
        clip_id: mondrian_core::ClipId,
        /// Number of exact placement matches.
        matches: usize,
    },
}

/// Collect finite-history demands and remove their already-accounted graphs
/// from one cloned ordinary compositing plan.
#[allow(clippy::too_many_arguments)]
pub fn prepare_timeline_temporal_execution(
    program: &PreparedVisualProgram,
    plan: &TimelineRenderPlan,
    generation: u64,
    continuity: EffectExecutionContinuity,
    frame_extent: EffectFrameExtent,
    output_roi: EffectPixelRoi,
    cancellation: ExecutionCancellationToken,
) -> Result<PreparedTimelineTemporalExecution, TimelineTemporalPreparationError> {
    let batches = collect_timeline_temporal_demands(
        program,
        plan,
        generation,
        continuity,
        frame_extent,
        output_roi,
        cancellation,
    )?;
    let mut execution_plan = plan.clone();
    if !batches.is_empty() {
        let identity = identity_compiled_effect_graph()
            .ok_or(TimelineTemporalPreparationError::IdentityGraphUnavailable)?;
        for batch in &batches {
            replace_effect_graph(
                &mut execution_plan,
                batch.placement(),
                Arc::clone(&identity),
            )?;
        }
    }
    Ok(PreparedTimelineTemporalExecution { execution_plan, batches })
}

/// Collect all admitted finite-history source batches in deterministic render
/// order without resolving media.
pub fn collect_timeline_temporal_demands(
    program: &PreparedVisualProgram,
    plan: &TimelineRenderPlan,
    generation: u64,
    continuity: EffectExecutionContinuity,
    frame_extent: EffectFrameExtent,
    output_roi: EffectPixelRoi,
    cancellation: ExecutionCancellationToken,
) -> Result<Vec<TimelineTemporalDemandBatch>, TimelineTemporalPreparationError> {
    let mut batches = Vec::new();
    for element in &plan.elements {
        match element {
            TimelineRenderPlanElement::Media(media) => collect_media_batch(
                &mut batches,
                program,
                media,
                generation,
                continuity,
                frame_extent,
                output_roi,
                cancellation.clone(),
            )?,
            TimelineRenderPlanElement::NestedSequence(nested) => collect_nested_batch(
                &mut batches,
                program,
                nested,
                generation,
                continuity,
                frame_extent,
                output_roi,
                cancellation.clone(),
            )?,
            TimelineRenderPlanElement::SolidColor(solid) => collect_solid_batch(
                &mut batches,
                solid,
                generation,
                continuity,
                frame_extent,
                output_roi,
                cancellation.clone(),
            )?,
            TimelineRenderPlanElement::BasicTitle(title) => {
                reject_temporal_title(title)?;
            }
            TimelineRenderPlanElement::Adjustment(adjustment) => {
                if graph_requires_temporal(&adjustment.effect_graph) {
                    return Err(TimelineTemporalPreparationError::AdjustmentUnsupported {
                        clip_id: adjustment.placement.clip_id,
                    });
                }
            }
            TimelineRenderPlanElement::CrossDissolve(transition) => {
                for input in [&transition.left, &transition.right] {
                    match input {
                        TimelineTransitionInputPlan::Transparent => {}
                        TimelineTransitionInputPlan::Media(media) => collect_media_batch(
                            &mut batches,
                            program,
                            media,
                            generation,
                            continuity,
                            frame_extent,
                            output_roi,
                            cancellation.clone(),
                        )?,
                        TimelineTransitionInputPlan::NestedSequence(nested) => {
                            collect_nested_batch(
                                &mut batches,
                                program,
                                nested,
                                generation,
                                continuity,
                                frame_extent,
                                output_roi,
                                cancellation.clone(),
                            )?
                        }
                        TimelineTransitionInputPlan::SolidColor(solid) => collect_solid_batch(
                            &mut batches,
                            solid,
                            generation,
                            continuity,
                            frame_extent,
                            output_roi,
                            cancellation.clone(),
                        )?,
                        TimelineTransitionInputPlan::BasicTitle(title) => {
                            reject_temporal_title(title)?;
                        }
                    }
                }
            }
        }
    }
    Ok(batches)
}

/// Execute one already-frozen Timeline temporal batch.
///
/// This function cannot resolve a missing frame: the only provider it accepts
/// is the immutable prepared set.
pub fn execute_prepared_timeline_temporal_batch(
    session: &mut EffectExecutionSession,
    batch: &TimelineTemporalDemandBatch,
    prepared: &mut PreparedTemporalFrameSet,
) -> Result<EffectTemporalExecutionOutput, EffectTemporalExecutionError> {
    session.execute_temporal_roi_f32(&batch.graph, &batch.execution, prepared)
}

#[allow(clippy::too_many_arguments)]
fn collect_media_batch(
    batches: &mut Vec<TimelineTemporalDemandBatch>,
    program: &PreparedVisualProgram,
    media: &TimelineMediaPlan,
    generation: u64,
    continuity: EffectExecutionContinuity,
    frame_extent: EffectFrameExtent,
    output_roi: EffectPixelRoi,
    cancellation: ExecutionCancellationToken,
) -> Result<(), TimelineTemporalPreparationError> {
    if !graph_requires_temporal(&media.effect_graph) {
        return Ok(());
    }
    collect_batch(
        batches,
        media.placement,
        Arc::clone(&media.effect_graph),
        generation,
        continuity,
        frame_extent,
        output_roi,
        media.frame_seed,
        cancellation,
        |request| {
            let source_time =
                program.sample_clip_source(media.placement, request.time()).map_err(|error| {
                    TimelineTemporalPreparationError::SourceSampling {
                        clip_id: media.placement.clip_id,
                        reason: error.to_string(),
                    }
                })?;
            let source_time = if let Some(rate) = media.frame_rate_override {
                let position =
                    source_time.to_frame_position(rate, FrameRounding::Floor).map_err(|error| {
                        TimelineTemporalPreparationError::SourceSampling {
                            clip_id: media.placement.clip_id,
                            reason: error.to_string(),
                        }
                    })?;
                TimelineTime::from_frame_position(position).map_err(|error| {
                    TimelineTemporalPreparationError::SourceSampling {
                        clip_id: media.placement.clip_id,
                        reason: error.to_string(),
                    }
                })?
            } else {
                source_time
            };
            Ok(TimelineTemporalSource::Media {
                asset_id: media.asset_id,
                source_time,
                color_space_override: media.color_space_override,
                alpha_interpretation: media.alpha_interpretation,
                auto_tone_map: media.auto_tone_map,
            })
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn collect_nested_batch(
    batches: &mut Vec<TimelineTemporalDemandBatch>,
    program: &PreparedVisualProgram,
    nested: &TimelineNestedSequencePlan,
    generation: u64,
    continuity: EffectExecutionContinuity,
    frame_extent: EffectFrameExtent,
    output_roi: EffectPixelRoi,
    cancellation: ExecutionCancellationToken,
) -> Result<(), TimelineTemporalPreparationError> {
    if !graph_requires_temporal(&nested.effect_graph) {
        return Ok(());
    }
    collect_batch(
        batches,
        nested.placement,
        Arc::clone(&nested.effect_graph),
        generation,
        continuity,
        frame_extent,
        output_roi,
        nested.frame_seed,
        cancellation,
        |request| {
            let source_time = program
                .sample_clip_source(nested.placement, request.time())
                .map_err(|error| TimelineTemporalPreparationError::SourceSampling {
                    clip_id: nested.placement.clip_id,
                    reason: error.to_string(),
                })?;
            Ok(TimelineTemporalSource::NestedSequence {
                sequence_id: nested.sequence_id,
                source_time,
                color_processing: nested.color_processing,
            })
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn collect_solid_batch(
    batches: &mut Vec<TimelineTemporalDemandBatch>,
    solid: &TimelineSolidColorPlan,
    generation: u64,
    continuity: EffectExecutionContinuity,
    frame_extent: EffectFrameExtent,
    output_roi: EffectPixelRoi,
    cancellation: ExecutionCancellationToken,
) -> Result<(), TimelineTemporalPreparationError> {
    if !graph_requires_temporal(&solid.effect_graph) {
        return Ok(());
    }
    collect_batch(
        batches,
        solid.placement,
        Arc::clone(&solid.effect_graph),
        generation,
        continuity,
        frame_extent,
        output_roi,
        solid.frame_seed,
        cancellation,
        |_request| Ok(TimelineTemporalSource::SolidColor { color: solid.color }),
    )
}

#[allow(clippy::too_many_arguments)]
fn collect_batch(
    batches: &mut Vec<TimelineTemporalDemandBatch>,
    placement: TimelineClipExecutionRef,
    graph: Arc<CompiledEffectGraph>,
    generation: u64,
    continuity: EffectExecutionContinuity,
    frame_extent: EffectFrameExtent,
    output_roi: EffectPixelRoi,
    output_frame_seed: i64,
    cancellation: ExecutionCancellationToken,
    mut resolve_source: impl FnMut(
        EffectTemporalFrameRequest,
    )
        -> Result<TimelineTemporalSource, TimelineTemporalPreparationError>,
) -> Result<(), TimelineTemporalPreparationError> {
    let execution = EffectTemporalExecutionRequest::new(
        generation,
        continuity,
        placement.clip_time,
        frame_extent,
        output_roi,
        cancellation,
    )
    .with_output_frame_seed(output_frame_seed);
    let effect_demands = collect_temporal_frame_demands(&graph, &execution).map_err(|error| {
        TimelineTemporalPreparationError::EffectDemand {
            clip_id: placement.clip_id,
            reason: error.to_string(),
        }
    })?;
    let source_demands = effect_demands
        .requests()
        .iter()
        .copied()
        .map(|effect_request| {
            Ok(TimelineTemporalSourceDemand {
                effect_request,
                placement,
                source: resolve_source(effect_request)?,
            })
        })
        .collect::<Result<Vec<_>, TimelineTemporalPreparationError>>()?;
    batches.push(TimelineTemporalDemandBatch {
        placement,
        graph,
        execution,
        effect_demands,
        source_demands: source_demands.into(),
    });
    Ok(())
}

fn reject_temporal_title(
    title: &TimelineBasicTitlePlan,
) -> Result<(), TimelineTemporalPreparationError> {
    if graph_requires_temporal(&title.effect_graph) {
        Err(TimelineTemporalPreparationError::BasicTitleUnsupported {
            clip_id: title.placement.clip_id,
        })
    } else {
        Ok(())
    }
}

fn graph_requires_temporal(graph: &CompiledEffectGraph) -> bool {
    let temporal = graph.execution_envelope().aggregate().temporal_input;
    !matches!(temporal.past, EffectTemporalSpan::None)
        || !matches!(temporal.future, EffectTemporalSpan::None)
}

fn replace_effect_graph(
    plan: &mut TimelineRenderPlan,
    placement: TimelineClipExecutionRef,
    replacement: Arc<CompiledEffectGraph>,
) -> Result<(), TimelineTemporalPreparationError> {
    let mut matches = 0usize;
    for element in &mut plan.elements {
        match element {
            TimelineRenderPlanElement::Media(media) if media.placement == placement => {
                media.effect_graph = Arc::clone(&replacement);
                matches += 1;
            }
            TimelineRenderPlanElement::Adjustment(adjustment)
                if adjustment.placement == placement =>
            {
                adjustment.effect_graph = Arc::clone(&replacement);
                matches += 1;
            }
            TimelineRenderPlanElement::SolidColor(solid) if solid.placement == placement => {
                solid.effect_graph = Arc::clone(&replacement);
                matches += 1;
            }
            TimelineRenderPlanElement::BasicTitle(title) if title.placement == placement => {
                title.effect_graph = Arc::clone(&replacement);
                matches += 1;
            }
            TimelineRenderPlanElement::NestedSequence(nested) if nested.placement == placement => {
                nested.effect_graph = Arc::clone(&replacement);
                matches += 1;
            }
            TimelineRenderPlanElement::CrossDissolve(transition) => {
                for input in [&mut transition.left, &mut transition.right] {
                    match input {
                        TimelineTransitionInputPlan::Media(media)
                            if media.placement == placement =>
                        {
                            media.effect_graph = Arc::clone(&replacement);
                            matches += 1;
                        }
                        TimelineTransitionInputPlan::SolidColor(solid)
                            if solid.placement == placement =>
                        {
                            solid.effect_graph = Arc::clone(&replacement);
                            matches += 1;
                        }
                        TimelineTransitionInputPlan::BasicTitle(title)
                            if title.placement == placement =>
                        {
                            title.effect_graph = Arc::clone(&replacement);
                            matches += 1;
                        }
                        TimelineTransitionInputPlan::NestedSequence(nested)
                            if nested.placement == placement =>
                        {
                            nested.effect_graph = Arc::clone(&replacement);
                            matches += 1;
                        }
                        TimelineTransitionInputPlan::Transparent
                        | TimelineTransitionInputPlan::Media(_)
                        | TimelineTransitionInputPlan::SolidColor(_)
                        | TimelineTransitionInputPlan::BasicTitle(_)
                        | TimelineTransitionInputPlan::NestedSequence(_) => {}
                    }
                }
            }
            TimelineRenderPlanElement::Media(_)
            | TimelineRenderPlanElement::Adjustment(_)
            | TimelineRenderPlanElement::SolidColor(_)
            | TimelineRenderPlanElement::BasicTitle(_)
            | TimelineRenderPlanElement::NestedSequence(_) => {}
        }
    }
    if matches == 1 {
        Ok(())
    } else {
        Err(TimelineTemporalPreparationError::PlacementResolution {
            clip_id: placement.clip_id,
            matches,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluate_prepared_visual_program;
    use mondrian_core::timeline_data::{ClipContent, MediaInterpretation};
    use mondrian_core::{ColorSpace, FramePosition, Rational, TimeScale};
    use mondrian_effects::{
        register_effect_definition, EffectColorDomainContract, EffectDefinition, EffectDeterminism,
        EffectExecutionContract, EffectExecutionModes, EffectGraphTopology, EffectNode,
        EffectRenderOp, EffectResourceLifetime, EffectRoiPropagation, EffectStateModel,
        EffectTemporalInputExtent, EffectType,
    };
    use mondrian_timeline::{Clip, Sequence, Track};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temporal_effect(offset: TimelineTime) -> EffectNode {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let effect_type = EffectType::Plugin(format!("test.timeline.temporal.{id}"));
        register_effect_definition(
            EffectDefinition::new(
                effect_type.key(),
                "Timeline temporal test",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32,
                determinism: EffectDeterminism::Deterministic,
                state_model: EffectStateModel::Stateless,
                temporal_input: EffectTemporalInputExtent {
                    past: EffectTemporalSpan::Finite(offset),
                    future: EffectTemporalSpan::None,
                },
                roi_propagation: EffectRoiPropagation::PixelLocal,
                resource_lifetime: EffectResourceLifetime::Frame,
                topology: EffectGraphTopology::LinearChain,
            })
            .with_graph_builder(Arc::new(move |_, _, graph| {
                graph.append_unary(EffectRenderOp::TemporalFrameMix {
                    past_offset: offset,
                    mix: 0.25,
                });
                Ok(())
            })),
        )
        .expect("register temporal test definition");
        EffectNode::new(effect_type)
    }

    #[test]
    fn media_demands_preserve_retime_color_alpha_and_generation() {
        let rate = Rational::new(30, 1);
        let offset = TimelineTime::new(1, 2).expect("offset");
        let mut sequence = Sequence::new("temporal media");
        sequence.settings.frame_rate = rate;
        sequence.video_tracks.clear();
        let mut track = Track::new_video("V1");
        let asset_id = AssetId::new();
        let mut clip = Clip::new(
            asset_id,
            TimelineTime::ZERO,
            TimelineTime::new(5, 1).expect("duration"),
        )
        .expect("Clip");
        clip.set_constant_source_time_map(
            TimelineTime::new(10, 1).expect("origin"),
            TimeScale::new(2, 1).expect("2x"),
        )
        .expect("retime");
        clip.content = ClipContent::Media {
            asset_id,
            interpretation: MediaInterpretation {
                color_space_override: Some(ColorSpace::Rec2100Pq),
                alpha: AlphaInterpretation::Premultiplied,
                ..MediaInterpretation::default()
            },
        };
        clip.add_effect_node(temporal_effect(offset));
        track.add_clip(clip).expect("add Clip");
        sequence.video_tracks.push(track);
        let program = PreparedVisualProgram::prepare(&sequence).expect("program");
        let plan = evaluate_prepared_visual_program(
            &program,
            crate::TimelineEvaluationRequest::export(FramePosition::new(30, sequence.time_base())),
        )
        .expect("plan");
        let extent = EffectFrameExtent::new(4, 2);
        let batches = collect_timeline_temporal_demands(
            &program,
            &plan,
            42,
            EffectExecutionContinuity::Discontinuous,
            extent,
            extent.full_frame_roi(),
            ExecutionCancellationToken::new(),
        )
        .expect("demands");
        assert_eq!(batches.len(), 1);
        let batch = &batches[0];
        assert_eq!(batch.effect_demands().generation(), 42);
        assert_eq!(batch.execution_request().output_frame_seed(), 30);
        assert_eq!(batch.source_demands().len(), 2);
        let expected = [
            TimelineTime::new(12, 1).expect("current"),
            TimelineTime::new(11, 1).expect("past"),
        ];
        for (demand, expected_time) in batch.source_demands().iter().zip(expected) {
            let TimelineTemporalSource::Media {
                asset_id: demand_asset,
                source_time,
                color_space_override,
                alpha_interpretation,
                ..
            } = &demand.source
            else {
                panic!("media demand");
            };
            assert_eq!(*demand_asset, asset_id);
            assert_eq!(*source_time, expected_time);
            assert_eq!(*color_space_override, Some(ColorSpace::Rec2100Pq));
            assert_eq!(*alpha_interpretation, AlphaInterpretation::Premultiplied);
        }
    }

    #[test]
    fn nested_transition_demands_keep_exact_endpoint_side() {
        let rate = Rational::new(30, 1);
        let offset = TimelineTime::new(1, 30).expect("offset");
        let child_id = SequenceId::new();
        let mut sequence = Sequence::new("temporal nested transition");
        sequence.settings.frame_rate = rate;
        sequence.video_tracks.clear();
        let mut track = Track::new_video("V1");
        let mut left = Clip::new_nested_sequence(
            child_id,
            TimelineTime::ZERO,
            TimelineTime::new(1, 1).expect("duration"),
            None,
        )
        .expect("left");
        left.add_effect_node(temporal_effect(offset));
        let left_id = left.id;
        let mut right = Clip::new_nested_sequence(
            child_id,
            TimelineTime::new(1, 1).expect("position"),
            TimelineTime::new(1, 1).expect("duration"),
            None,
        )
        .expect("right");
        right.add_effect_node(temporal_effect(offset));
        let right_id = right.id;
        track.add_clip(left).expect("left");
        track.add_clip(right).expect("right");
        sequence.video_tracks.push(track);
        let transition = mondrian_timeline::VideoTransition::cross_dissolve(
            left_id,
            right_id,
            mondrian_core::TimelineTimeRange::new(
                TimelineTime::new(29, 30).expect("start"),
                TimelineTime::new(2, 30).expect("duration"),
            )
            .expect("range"),
        );
        let transition_id = transition.id;
        sequence.video_transitions.push(transition);
        let program = PreparedVisualProgram::prepare(&sequence).expect("program");
        let plan = evaluate_prepared_visual_program(
            &program,
            crate::TimelineEvaluationRequest::export(FramePosition::new(30, sequence.time_base())),
        )
        .expect("plan");
        let extent = EffectFrameExtent::new(2, 2);
        let batches = collect_timeline_temporal_demands(
            &program,
            &plan,
            7,
            EffectExecutionContinuity::Continuous,
            extent,
            extent.full_frame_roi(),
            ExecutionCancellationToken::new(),
        )
        .expect("demands");
        assert_eq!(batches.len(), 2);
        assert_eq!(
            batches[0].placement().endpoint,
            mondrian_core::timeline_data::TimelineClipEndpointContext::TransitionLeft {
                transition_id
            }
        );
        assert_eq!(
            batches[1].placement().endpoint,
            mondrian_core::timeline_data::TimelineClipEndpointContext::TransitionRight {
                transition_id
            }
        );
        assert!(
            batches.iter().all(|batch| batch.source_demands().iter().all(|demand| matches!(
                &demand.source,
                TimelineTemporalSource::NestedSequence {
                    sequence_id,
                    color_processing: NestedColorProcessing::PreserveChildWorkingSpace,
                    ..
                } if *sequence_id == child_id
            )))
        );
    }
}
