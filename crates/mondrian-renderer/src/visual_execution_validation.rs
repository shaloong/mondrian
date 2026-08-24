//! Validation-only semantic trace for prepared visual execution.
//!
//! This Module observes the immutable [`crate::PreparedVisualFrameClosure`]
//! produced by a real consumer. It does not evaluate a Timeline, project
//! nested time, collect Effect demands, or materialize pixels. Preview and
//! Export can therefore compare one canonical ledger without acquiring a
//! second interpretation path.

use crate::{
    PreparedVisualFrameClosure, PreparedVisualFrameNodeId, PreparedVisualNestedSample,
    TimelineTemporalSource,
};
use mondrian_core::timeline_data::{
    AlphaInterpretation, NestedColorProcessing, TimelineClipExecutionRef,
};
use mondrian_core::{
    AssetId, ColorSpace, Resolution, SequenceId, SequenceRevision, TimelineTime, WorkingColorSpace,
};
use mondrian_effects::{
    EffectExecutionContinuity, EffectFrameExtent, EffectInputRoi, EffectRoiHalo,
    EffectWorkingPrecision,
};
use std::sync::Arc;

/// Canonical validation ledger emitted from one already-prepared visual
/// closure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedVisualExecutionSemanticTrace {
    /// Deterministic pre-order execution instances.
    pub nodes: Arc<[PreparedVisualExecutionNodeTrace]>,
}

/// One exact Sequence occurrence in a prepared visual closure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedVisualExecutionNodeTrace {
    /// Closure-local node index.
    pub node_index: usize,
    /// Exact Sequence snapshot identity.
    pub sequence_id: SequenceId,
    /// Exact Sequence author revision.
    pub sequence_revision: SequenceRevision,
    /// Projected frame on this Sequence's Evaluation Grid.
    pub frame: i64,
    /// Exact Sequence-local sample time represented by `frame`.
    pub time: TimelineTime,
    /// Authored Sequence raster.
    pub author_resolution: Resolution,
    /// Consumer-selected execution raster.
    pub execution_resolution: Resolution,
    /// Conservative fingerprint of the immutable prepared Program.
    pub visual_author_fingerprint: [u8; 32],
    /// Exact placement/sample path from the root to this occurrence.
    pub instance_path: Arc<[PreparedVisualExecutionInstanceStepTrace]>,
    /// Exact nested bindings emitted from this occurrence.
    pub nested_bindings: Arc<[PreparedVisualExecutionNestedBindingTrace]>,
    /// Finite temporal batches removed from this occurrence's ordinary plan.
    pub temporal_batches: Arc<[PreparedVisualExecutionTemporalBatchTrace]>,
}

/// One normalized edge in a prepared visual instance path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedVisualExecutionInstanceStepTrace {
    /// Parent placement, including Transition endpoint identity.
    pub placement: TimelineClipExecutionRef,
    /// Current or temporal sample semantics. Scheduler generation is
    /// intentionally excluded because it does not alter authored pixels.
    pub sample: PreparedVisualExecutionSampleTrace,
}

/// Consumer-independent current or finite temporal sample identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreparedVisualExecutionSampleTrace {
    /// Current nested value from the ordinary plan.
    Current,
    /// Exact historical/future request emitted by one compiled Effect graph.
    Temporal(PreparedVisualExecutionEffectRequestTrace),
}

/// Generation-independent semantic projection of one Effect frame request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PreparedVisualExecutionEffectRequestTrace {
    /// Exact Clip visual-author-domain sample time.
    pub time: TimelineTime,
    /// Complete Effect coordinate extent.
    pub frame_extent: EffectFrameExtent,
    /// Required input region and strength of ROI evidence.
    pub input_roi: EffectInputRoi,
    /// Exact finite halo when the ROI law proves one.
    pub exact_halo: Option<EffectRoiHalo>,
    /// Required working representation.
    pub precision: EffectWorkingPrecision,
}

/// One exact parent-to-child projection in the prepared closure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedVisualExecutionNestedBindingTrace {
    /// Parent closure node index.
    pub parent_node_index: usize,
    /// Child closure node index.
    pub child_node_index: usize,
    /// Parent placement owning this sample.
    pub placement: TimelineClipExecutionRef,
    /// Current or temporal sample role.
    pub sample: PreparedVisualExecutionSampleTrace,
    /// Exact child-local sample contract before Evaluation Grid projection.
    pub source_sample: mondrian_core::SourceSampleTarget,
    /// Parent working domain.
    pub parent_working_color_space: WorkingColorSpace,
    /// Child working domain.
    pub child_working_color_space: WorkingColorSpace,
}

/// One finite temporal graph demand batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedVisualExecutionTemporalBatchTrace {
    /// Placement whose value enters the compiled graph.
    pub placement: TimelineClipExecutionRef,
    /// Complete semantic fingerprint of the sole compiled Effect IR.
    pub effect_graph_fingerprint: [u8; 32],
    /// Continuity fact supplied by the consumer.
    pub continuity: EffectExecutionContinuity,
    /// Exact Clip visual-author-domain output time.
    pub output_time: TimelineTime,
    /// Deterministic seed used by current-time unary Effects.
    pub output_frame_seed: i64,
    /// Complete output coordinate extent.
    pub frame_extent: EffectFrameExtent,
    /// Requested output region.
    pub output_roi: mondrian_effects::EffectPixelRoi,
    /// Exact, ordered source sample set.
    pub source_samples: Arc<[PreparedVisualExecutionTemporalSourceTrace]>,
}

/// One exact source sample required by a temporal Effect graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedVisualExecutionTemporalSourceTrace {
    /// Effect-owned Clip-domain frame request.
    pub effect_request: PreparedVisualExecutionEffectRequestTrace,
    /// Prepared placement whose mapping is authoritative.
    pub placement: TimelineClipExecutionRef,
    /// Concrete source projection.
    pub source: PreparedVisualExecutionTemporalSourceKindTrace,
}

/// Canonical validation projection of a temporal source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreparedVisualExecutionTemporalSourceKindTrace {
    /// File-backed media projection.
    Media {
        /// Stable project Asset identity.
        asset_id: AssetId,
        /// Exact source-local sample contract.
        source_sample: mondrian_core::SourceSampleTarget,
        /// Explicit authored input override.
        color_space_override: Option<ColorSpace>,
        /// Placement-local picture interpretation overrides.
        picture_overrides: mondrian_core::PictureInterpretationOverrides,
        /// Explicit alpha interpretation.
        alpha_interpretation: AlphaInterpretation,
        /// Sequence input tone-map policy.
        auto_tone_map: bool,
    },
    /// Nested Sequence projection and the exact prepared child occurrence.
    NestedSequence {
        /// Child Sequence identity.
        sequence_id: SequenceId,
        /// Exact child-local sample contract before grid projection.
        source_sample: mondrian_core::SourceSampleTarget,
        /// Child-to-parent working-space contract.
        color_processing: NestedColorProcessing,
        /// Closure-local child occurrence selected by this temporal request.
        child_node_index: usize,
    },
    /// Constant generated scene-linear source, represented by exact float
    /// payload bits.
    SolidColor {
        /// RGBA bit patterns in channel order.
        rgba_bits: [u32; 4],
    },
}

/// Observe one consumer's immutable prepared visual closure.
///
/// This function performs no Timeline or Effect interpretation. A missing
/// nested temporal binding indicates a corrupted closure and fails validation
/// instead of inventing an occurrence.
pub fn prepared_visual_execution_semantic_trace<T>(
    closure: &PreparedVisualFrameClosure<T>,
) -> Result<PreparedVisualExecutionSemanticTrace, String> {
    let nodes = closure
        .nodes()
        .iter()
        .map(|node| {
            let instance_path = node
                .instance_path()
                .iter()
                .map(|step| PreparedVisualExecutionInstanceStepTrace {
                    placement: step.placement,
                    sample: normalized_sample(step.sample),
                })
                .collect::<Vec<_>>();
            let nested_bindings = node
                .bindings()
                .iter()
                .map(|binding| PreparedVisualExecutionNestedBindingTrace {
                    parent_node_index: binding.parent().index(),
                    child_node_index: binding.child().index(),
                    placement: binding.placement(),
                    sample: normalized_sample(binding.sample()),
                    source_sample: binding.source_sample(),
                    parent_working_color_space: binding.parent_working_color_space(),
                    child_working_color_space: binding.child_working_color_space(),
                })
                .collect::<Vec<_>>();
            let temporal_batches = node
                .evaluation()
                .temporal_batches()
                .iter()
                .map(|batch| trace_temporal_batch(closure, node.id(), batch))
                .collect::<Result<Vec<_>, String>>()?;
            Ok(PreparedVisualExecutionNodeTrace {
                node_index: node.id().index(),
                sequence_id: node.sequence_id(),
                sequence_revision: node.sequence_revision(),
                frame: node.frame(),
                time: node.time(),
                author_resolution: node.author_resolution(),
                execution_resolution: node.execution_resolution(),
                visual_author_fingerprint: node.program().visual_author_fingerprint(),
                instance_path: instance_path.into(),
                nested_bindings: nested_bindings.into(),
                temporal_batches: temporal_batches.into(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(PreparedVisualExecutionSemanticTrace { nodes: nodes.into() })
}

fn trace_temporal_batch<T>(
    closure: &PreparedVisualFrameClosure<T>,
    parent_node_id: PreparedVisualFrameNodeId,
    batch: &crate::TimelineTemporalDemandBatch,
) -> Result<PreparedVisualExecutionTemporalBatchTrace, String> {
    let parent = closure.node(parent_node_id).ok_or_else(|| {
        format!(
            "prepared visual validation trace lost parent node {}",
            parent_node_id.index()
        )
    })?;
    let source_samples = batch
        .source_demands()
        .iter()
        .map(|demand| {
            let source = match &demand.source {
                TimelineTemporalSource::Media {
                    asset_id,
                    source_sample,
                    color_space_override,
                    picture_overrides,
                    alpha_interpretation,
                    auto_tone_map,
                } => PreparedVisualExecutionTemporalSourceKindTrace::Media {
                    asset_id: *asset_id,
                    source_sample: *source_sample,
                    color_space_override: *color_space_override,
                    picture_overrides: *picture_overrides,
                    alpha_interpretation: *alpha_interpretation,
                    auto_tone_map: *auto_tone_map,
                },
                TimelineTemporalSource::NestedSequence {
                    sequence_id,
                    source_sample,
                    color_processing,
                } => {
                    let child = parent
                        .nested_child(
                            demand.placement,
                            PreparedVisualNestedSample::Temporal(demand.effect_request),
                        )
                        .ok_or_else(|| {
                            format!(
                                "prepared visual validation trace has no temporal child for Clip {}",
                                demand.placement.clip_id
                            )
                        })?;
                    PreparedVisualExecutionTemporalSourceKindTrace::NestedSequence {
                        sequence_id: *sequence_id,
                        source_sample: *source_sample,
                        color_processing: *color_processing,
                        child_node_index: child.index(),
                    }
                }
                TimelineTemporalSource::SolidColor { color } => {
                    PreparedVisualExecutionTemporalSourceKindTrace::SolidColor {
                        rgba_bits: [
                            color.r.to_bits(),
                            color.g.to_bits(),
                            color.b.to_bits(),
                            color.a.to_bits(),
                        ],
                    }
                }
            };
            Ok(PreparedVisualExecutionTemporalSourceTrace {
                effect_request: normalized_effect_request(demand.effect_request),
                placement: demand.placement,
                source,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let execution = batch.execution_request();
    Ok(PreparedVisualExecutionTemporalBatchTrace {
        placement: batch.placement(),
        effect_graph_fingerprint: batch.graph().semantic_fingerprint(),
        continuity: execution.continuity(),
        output_time: execution.output_time(),
        output_frame_seed: execution.output_frame_seed(),
        frame_extent: execution.frame_extent(),
        output_roi: execution.output_roi(),
        source_samples: source_samples.into(),
    })
}

fn normalized_sample(sample: PreparedVisualNestedSample) -> PreparedVisualExecutionSampleTrace {
    match sample {
        PreparedVisualNestedSample::Current => PreparedVisualExecutionSampleTrace::Current,
        PreparedVisualNestedSample::Temporal(request) => {
            PreparedVisualExecutionSampleTrace::Temporal(normalized_effect_request(request))
        }
    }
}

fn normalized_effect_request(
    request: mondrian_effects::EffectTemporalFrameRequest,
) -> PreparedVisualExecutionEffectRequestTrace {
    PreparedVisualExecutionEffectRequestTrace {
        time: request.time(),
        frame_extent: request.frame_extent(),
        input_roi: request.input_roi(),
        exact_halo: request.exact_halo(),
        precision: request.precision(),
    }
}
