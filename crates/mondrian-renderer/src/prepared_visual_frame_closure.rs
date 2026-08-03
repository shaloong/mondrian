//! Canonical recursive closure for one prepared visual frame.
//!
//! [`PreparedVisualProgram`] and [`TimelineRenderPlan`] own one Sequence
//! revision's indexed evaluation. This Module composes those per-Sequence
//! plans into the sole recursive execution closure shared by Preview and
//! Export. It owns exact nested-time projection, cycle/depth validation,
//! child-canvas and color-context binding, Transition endpoint identity, and
//! temporal nested demands. It deliberately owns no media pixels, decode
//! scheduling, Preview pending state, export encoding, or presentation.

use crate::{
    PreparedVisualMaterializationContract, PreparedVisualProgram, PreparedVisualProgramBinding,
    TimelineRenderPlan, TimelineRenderPlanElement, TimelineTemporalDemandBatch,
    TimelineTemporalSource, TimelineTransitionInputPlan,
};
use mondrian_core::timeline_data::{NestedColorProcessing, TimelineClipExecutionRef};
use mondrian_core::{
    FramePosition, FrameRounding, Resolution, SequenceId, SequenceRevision, TimelineTime,
    WorkingColorSpace,
};
use mondrian_effects::EffectTemporalFrameRequest;
use mondrian_timeline::sequence::{
    ProgramColorContext, Sequence, MAX_NESTED_SEQUENCE_RENDER_DEPTH,
};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::num::NonZeroU32;
use std::sync::Arc;

/// Stable index of one exact Sequence instance in a prepared frame closure.
///
/// A node identifies one execution instance, not merely one
/// `(SequenceId, frame)` value. Two placements that sample the same child
/// Sequence therefore receive distinct node IDs and instance paths even when
/// their immutable plans happen to be equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PreparedVisualFrameNodeId(u32);

impl PreparedVisualFrameNodeId {
    /// Zero-based deterministic node index within its owning closure.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Whether one child sample is the placement's current value or a historical
/// value requested by a temporal Effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreparedVisualNestedSample {
    /// Current nested value selected by the ordinary Render Plan.
    Current,
    /// Historical/future value emitted by one exact temporal Effect request.
    Temporal(EffectTemporalFrameRequest),
}

/// One exact edge in a nested visual instance path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PreparedVisualNestedInstanceStep {
    /// Parent placement, including ordinary or Transition-endpoint identity.
    pub placement: TimelineClipExecutionRef,
    /// Current or exact temporal sample role.
    pub sample: PreparedVisualNestedSample,
}

/// Consumer-owned execution preparation attached to one canonical frame node.
///
/// The renderer fixes the plan and temporal demand set before the node enters
/// the recursive closure. `payload` lets Preview and Export retain their own
/// execution-admission evidence without interpreting nesting a second time.
#[derive(Debug)]
pub struct PreparedVisualFrameEvaluation<T> {
    plan: TimelineRenderPlan,
    temporal_batches: Vec<TimelineTemporalDemandBatch>,
    payload: T,
}

impl<T> PreparedVisualFrameEvaluation<T> {
    /// Bind one evaluated per-Sequence plan to its temporal demands and
    /// consumer-owned execution evidence.
    pub fn new(
        plan: TimelineRenderPlan,
        temporal_batches: Vec<TimelineTemporalDemandBatch>,
        payload: T,
    ) -> Self {
        Self { plan, temporal_batches, payload }
    }

    /// Evaluated per-Sequence Render Plan.
    pub const fn plan(&self) -> &TimelineRenderPlan {
        &self.plan
    }

    /// Temporal source batches removed from the ordinary compositing plan.
    pub fn temporal_batches(&self) -> &[TimelineTemporalDemandBatch] {
        &self.temporal_batches
    }

    /// Consumer-owned execution evidence.
    pub const fn payload(&self) -> &T {
        &self.payload
    }
}

/// Policy for resolving every nested Sequence's execution canvas.
///
/// The root canvas is always explicit in [`PreparedVisualFrameClosureRequest`].
/// Export keeps nested Sequences at authored resolution. Preview applies the
/// same machine/runtime divisor to each child's own authored Preview scale;
/// it never inherits the parent's raster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreparedVisualChildCanvasPolicy {
    /// Preserve every child's authored Sequence raster.
    Authored,
    /// Apply authored Preview scale and one non-zero runtime divisor.
    PreviewScaled {
        /// Runtime dimension divisor, currently 1, 2, or 4.
        runtime_dimension_divisor: NonZeroU32,
    },
}

impl PreparedVisualChildCanvasPolicy {
    /// Construct the Preview policy from a validated non-zero divisor.
    pub fn preview_scaled(
        runtime_dimension_divisor: u32,
    ) -> Result<Self, PreparedVisualFrameClosureError> {
        let runtime_dimension_divisor = NonZeroU32::new(runtime_dimension_divisor)
            .ok_or(PreparedVisualFrameClosureError::InvalidPreviewDimensionDivisor)?;
        Ok(Self::PreviewScaled { runtime_dimension_divisor })
    }

    fn child_resolution(
        self,
        sequence_id: SequenceId,
        materialization: PreparedVisualMaterializationContract,
    ) -> Result<Resolution, PreparedVisualFrameClosureError> {
        let logical = materialization.author_resolution();
        let resolution = match self {
            Self::Authored => logical,
            Self::PreviewScaled { runtime_dimension_divisor } => {
                let authored_scale = normalize_preview_resolution_scale(
                    materialization.authored_preview_resolution_scale(),
                );
                let divisor = runtime_dimension_divisor.get();
                Resolution {
                    width: ((logical.width as f32 * authored_scale).round() as u32)
                        .max(1)
                        .div_ceil(divisor),
                    height: ((logical.height as f32 * authored_scale).round() as u32)
                        .max(1)
                        .div_ceil(divisor),
                }
            }
        };
        validate_resolution(sequence_id, resolution)?;
        Ok(resolution)
    }
}

/// Complete root request for one canonical recursive visual closure.
pub struct PreparedVisualFrameClosureRequest<'a> {
    /// Root Sequence snapshot.
    pub root_sequence: &'a Sequence,
    /// Reachable Sequence snapshots available to nested lookup.
    pub sequences: &'a [Sequence],
    /// Exact nonnegative root frame on the root Sequence Evaluation Grid.
    pub root_frame: i64,
    /// Explicit root execution/output raster.
    pub root_resolution: Resolution,
    /// Exact root Program color context.
    pub root_color_context: ProgramColorContext,
    /// Child canvas policy selected by the consumer.
    pub child_canvas_policy: PreparedVisualChildCanvasPolicy,
}

/// One exact nested edge emitted by the canonical recursive closure.
#[derive(Debug, Clone)]
pub struct PreparedVisualNestedBinding {
    parent: PreparedVisualFrameNodeId,
    child: PreparedVisualFrameNodeId,
    placement: TimelineClipExecutionRef,
    sample: PreparedVisualNestedSample,
    source_time: TimelineTime,
    parent_working_color_space: WorkingColorSpace,
    child_working_color_space: WorkingColorSpace,
}

impl PreparedVisualNestedBinding {
    /// Parent node containing the nested placement or temporal source.
    pub const fn parent(&self) -> PreparedVisualFrameNodeId {
        self.parent
    }

    /// Exact child execution instance.
    pub const fn child(&self) -> PreparedVisualFrameNodeId {
        self.child
    }

    /// Parent placement, including Transition endpoint identity.
    pub const fn placement(&self) -> TimelineClipExecutionRef {
        self.placement
    }

    /// Current or temporal role of this sample.
    pub const fn sample(&self) -> PreparedVisualNestedSample {
        self.sample
    }

    /// Exact child-local time before projection to the child Evaluation Grid.
    pub const fn source_time(&self) -> TimelineTime {
        self.source_time
    }

    /// Working space produced by the parent node.
    pub const fn parent_working_color_space(&self) -> WorkingColorSpace {
        self.parent_working_color_space
    }

    /// Working space in which the child node is evaluated.
    pub const fn child_working_color_space(&self) -> WorkingColorSpace {
        self.child_working_color_space
    }
}

/// One prepared Sequence instance in the recursive frame closure.
#[derive(Debug)]
pub struct PreparedVisualFrameNode<T> {
    id: PreparedVisualFrameNodeId,
    program: Arc<PreparedVisualProgram>,
    sequence_id: SequenceId,
    sequence_revision: SequenceRevision,
    frame: i64,
    time: TimelineTime,
    materialization: PreparedVisualMaterializationContract,
    execution_resolution: Resolution,
    color_context: ProgramColorContext,
    instance_path: Arc<[PreparedVisualNestedInstanceStep]>,
    evaluation: PreparedVisualFrameEvaluation<T>,
    bindings: Vec<PreparedVisualNestedBinding>,
    binding_index: HashMap<PreparedVisualNestedBindingKey, PreparedVisualFrameNodeId>,
}

impl<T> PreparedVisualFrameNode<T> {
    /// Closure-local node identity.
    pub const fn id(&self) -> PreparedVisualFrameNodeId {
        self.id
    }

    /// Exact immutable Sequence execution program bound before evaluation.
    pub const fn program(&self) -> &Arc<PreparedVisualProgram> {
        &self.program
    }

    /// Sequence snapshot identity.
    pub const fn sequence_id(&self) -> SequenceId {
        self.sequence_id
    }

    /// Exact conservative Sequence author revision.
    pub const fn sequence_revision(&self) -> SequenceRevision {
        self.sequence_revision
    }

    /// Projected frame on this Sequence's Evaluation Grid.
    pub const fn frame(&self) -> i64 {
        self.frame
    }

    /// Exact Sequence-local sample time represented by `frame`.
    pub const fn time(&self) -> TimelineTime {
        self.time
    }

    /// Sequence's authored logical raster.
    pub const fn author_resolution(&self) -> Resolution {
        self.materialization.author_resolution()
    }

    /// Static canvas/title facts frozen with this node's exact Program.
    pub const fn materialization_contract(&self) -> PreparedVisualMaterializationContract {
        self.materialization
    }

    /// Sequence-local Basic Title safe-area margin.
    pub const fn title_safe_margin(&self) -> f32 {
        self.materialization.title_safe_margin()
    }

    /// Consumer-selected execution raster for this node.
    pub const fn execution_resolution(&self) -> Resolution {
        self.execution_resolution
    }

    /// Exact nested/root color context fixed before materialization.
    pub const fn color_context(&self) -> &ProgramColorContext {
        &self.color_context
    }

    /// Exact placement/sample path. Root has an empty path.
    pub fn instance_path(&self) -> &[PreparedVisualNestedInstanceStep] {
        &self.instance_path
    }

    /// Per-Sequence plan, temporal demands, and consumer evidence.
    pub const fn evaluation(&self) -> &PreparedVisualFrameEvaluation<T> {
        &self.evaluation
    }

    /// Deterministically ordered child bindings emitted by this node.
    pub fn bindings(&self) -> &[PreparedVisualNestedBinding] {
        &self.bindings
    }

    /// Resolve an exact current or temporal nested child.
    pub fn nested_child(
        &self,
        placement: TimelineClipExecutionRef,
        sample: PreparedVisualNestedSample,
    ) -> Option<PreparedVisualFrameNodeId> {
        self.binding_index
            .get(&PreparedVisualNestedBindingKey { placement, sample })
            .copied()
    }
}

/// Immutable recursive closure consumed by Preview or Export materialization.
#[derive(Debug)]
pub struct PreparedVisualFrameClosure<T> {
    root: PreparedVisualFrameNodeId,
    nodes: Vec<PreparedVisualFrameNode<T>>,
}

impl<T> PreparedVisualFrameClosure<T> {
    /// Exact root node.
    pub const fn root(&self) -> PreparedVisualFrameNodeId {
        self.root
    }

    /// Number of distinct execution instances. Equal Sequence/frame samples
    /// reached through different placements remain distinct nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the closure contains no nodes. A valid closure is never empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Resolve one closure-local node ID.
    pub fn node(&self, id: PreparedVisualFrameNodeId) -> Option<&PreparedVisualFrameNode<T>> {
        self.nodes.get(id.index())
    }

    /// Deterministic pre-order node sequence.
    pub fn nodes(&self) -> &[PreparedVisualFrameNode<T>] {
        &self.nodes
    }

    /// Conservatively bound the active CPU bytes required while materializing
    /// this complete closure into float working frames.
    ///
    /// The bound counts one full RGBA32F output for every distinct prepared
    /// instance because a parent may retain already-materialized siblings
    /// until its composite runs. It additionally reserves four canvases at
    /// the largest node extent for the compositor's proven worst case
    /// (output, both Cross Dissolve endpoints, and one Effect result).
    /// Preview and Export must admit this value against the same owner-scoped
    /// active-byte grant used by [`crate::TimelineCompositeScratch`] before
    /// materializing any child pixels.
    pub fn conservative_cpu_materialization_active_bytes(
        &self,
    ) -> Result<u64, PreparedVisualFrameClosureError> {
        const FLOAT_PIXEL_BYTES: u64 = std::mem::size_of::<[f32; 4]>() as u64;
        const WORST_LOCAL_COMPOSITOR_CANVASES: u64 = 4;

        let mut retained_outputs = 0_u64;
        let mut largest_output = 0_u64;
        for node in &self.nodes {
            let resolution = node.execution_resolution;
            let bytes = u64::from(resolution.width)
                .checked_mul(u64::from(resolution.height))
                .and_then(|pixels| pixels.checked_mul(FLOAT_PIXEL_BYTES))
                .ok_or(PreparedVisualFrameClosureError::CpuMaterializationEstimateOverflow)?;
            retained_outputs = retained_outputs
                .checked_add(bytes)
                .ok_or(PreparedVisualFrameClosureError::CpuMaterializationEstimateOverflow)?;
            largest_output = largest_output.max(bytes);
        }
        let local_compositor = largest_output
            .checked_mul(WORST_LOCAL_COMPOSITOR_CANVASES)
            .ok_or(PreparedVisualFrameClosureError::CpuMaterializationEstimateOverflow)?;
        retained_outputs
            .checked_add(local_compositor)
            .ok_or(PreparedVisualFrameClosureError::CpuMaterializationEstimateOverflow)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PreparedVisualNestedBindingKey {
    placement: TimelineClipExecutionRef,
    sample: PreparedVisualNestedSample,
}

#[derive(Debug, Clone, Copy)]
struct NestedDemand {
    placement: TimelineClipExecutionRef,
    sample: PreparedVisualNestedSample,
    sequence_id: SequenceId,
    source_time: TimelineTime,
    color_processing: NestedColorProcessing,
}

/// Build the sole recursive visual closure for one root frame.
///
/// `resolve_program` binds each distinct Sequence snapshot to exactly one
/// immutable Program. The Program identity, revision, and conservative author
/// fingerprint are validated before `evaluate` can observe it.
///
/// `evaluate` receives no raw Sequence and may only lower the already prepared
/// Program for the exact frame, execution raster, color context, and normalized
/// authored Preview scale supplied here. It may attach consumer-specific
/// route/admission evidence, but must not recurse. Nested lookup and every
/// child request are issued only by this function.
pub fn prepare_visual_frame_closure<T>(
    request: PreparedVisualFrameClosureRequest<'_>,
    mut resolve_program: impl FnMut(&Sequence) -> Result<Arc<PreparedVisualProgram>, String>,
    evaluate: impl FnMut(
        &Arc<PreparedVisualProgram>,
        i64,
        Resolution,
        &ProgramColorContext,
        f32,
    ) -> Result<PreparedVisualFrameEvaluation<T>, String>,
) -> Result<PreparedVisualFrameClosure<T>, PreparedVisualFrameClosureError> {
    prepare_bound_visual_frame_closure(
        request,
        |sequence| {
            let program = resolve_program(sequence)?;
            PreparedVisualProgramBinding::checked(sequence, program)
                .map_err(|error| error.to_string())
        },
        evaluate,
    )
}

/// Build the recursive visual closure from Programs already certified against
/// one immutable author snapshot.
///
/// Unlike [`prepare_visual_frame_closure`], this Interface performs no
/// canonical author serialization. Binding creation already proved the
/// complete author fingerprint; this builder retains exact Program identity
/// and Sequence revision checks.
pub fn prepare_bound_visual_frame_closure<T>(
    request: PreparedVisualFrameClosureRequest<'_>,
    mut resolve_program: impl FnMut(&Sequence) -> Result<PreparedVisualProgramBinding, String>,
    mut evaluate: impl FnMut(
        &Arc<PreparedVisualProgram>,
        i64,
        Resolution,
        &ProgramColorContext,
        f32,
    ) -> Result<PreparedVisualFrameEvaluation<T>, String>,
) -> Result<PreparedVisualFrameClosure<T>, PreparedVisualFrameClosureError> {
    if request.root_frame < 0 {
        return Err(PreparedVisualFrameClosureError::NegativeRootFrame {
            frame: request.root_frame,
        });
    }
    validate_resolution(request.root_sequence.id, request.root_resolution)?;
    let mut sequence_index = HashMap::with_capacity(request.sequences.len().saturating_add(1));
    for sequence in request.sequences {
        if sequence_index.insert(sequence.id, sequence).is_some() {
            return Err(PreparedVisualFrameClosureError::DuplicateSequenceIdentity {
                sequence_id: sequence.id,
            });
        }
    }
    match sequence_index.get(&request.root_sequence.id).copied() {
        Some(indexed_root) if !std::ptr::eq(indexed_root, request.root_sequence) => {
            return Err(PreparedVisualFrameClosureError::DuplicateSequenceIdentity {
                sequence_id: request.root_sequence.id,
            });
        }
        Some(_) => {}
        None => {
            sequence_index.insert(request.root_sequence.id, request.root_sequence);
        }
    }

    let mut builder = PreparedVisualFrameClosureBuilder {
        sequence_index,
        child_canvas_policy: request.child_canvas_policy,
        resolve_program: &mut resolve_program,
        evaluate: &mut evaluate,
        programs: HashMap::new(),
        nodes: Vec::new(),
        active_path: Vec::new(),
    };
    let root = builder.prepare_node(
        request.root_sequence,
        request.root_frame,
        request.root_resolution,
        request.root_color_context,
        Arc::from([]),
        0,
    )?;
    Ok(PreparedVisualFrameClosure { root, nodes: builder.nodes })
}

struct PreparedVisualFrameClosureBuilder<'a, T, Evaluate> {
    sequence_index: HashMap<SequenceId, &'a Sequence>,
    child_canvas_policy: PreparedVisualChildCanvasPolicy,
    resolve_program: &'a mut dyn FnMut(&Sequence) -> Result<PreparedVisualProgramBinding, String>,
    evaluate: &'a mut Evaluate,
    programs: HashMap<SequenceId, Arc<PreparedVisualProgram>>,
    nodes: Vec<PreparedVisualFrameNode<T>>,
    active_path: Vec<SequenceId>,
}

impl<'a, T, Evaluate> PreparedVisualFrameClosureBuilder<'a, T, Evaluate>
where
    Evaluate: FnMut(
        &Arc<PreparedVisualProgram>,
        i64,
        Resolution,
        &ProgramColorContext,
        f32,
    ) -> Result<PreparedVisualFrameEvaluation<T>, String>,
{
    #[allow(clippy::too_many_arguments)]
    fn prepare_node(
        &mut self,
        sequence: &Sequence,
        frame: i64,
        execution_resolution: Resolution,
        color_context: ProgramColorContext,
        instance_path: Arc<[PreparedVisualNestedInstanceStep]>,
        depth: usize,
    ) -> Result<PreparedVisualFrameNodeId, PreparedVisualFrameClosureError> {
        if depth > MAX_NESTED_SEQUENCE_RENDER_DEPTH {
            return Err(PreparedVisualFrameClosureError::DepthExceeded {
                sequence_id: sequence.id,
                maximum: MAX_NESTED_SEQUENCE_RENDER_DEPTH,
            });
        }
        if let Some(cycle_start) =
            self.active_path.iter().position(|candidate| *candidate == sequence.id)
        {
            let mut path = self.active_path[cycle_start..].to_vec();
            path.push(sequence.id);
            return Err(PreparedVisualFrameClosureError::Cycle { path });
        }
        if frame < 0 {
            return Err(PreparedVisualFrameClosureError::NegativeNestedFrame {
                sequence_id: sequence.id,
                frame,
            });
        }
        validate_resolution(sequence.id, execution_resolution)?;
        let time =
            TimelineTime::from_frame_position(FramePosition::new(frame, sequence.time_base()))
                .map_err(
                    |error| PreparedVisualFrameClosureError::FrameTimeProjection {
                        sequence_id: sequence.id,
                        frame,
                        reason: error.to_string(),
                    },
                )?;

        self.active_path.push(sequence.id);
        let result = (|| {
            let program = self.program(sequence)?;
            let materialization = program.materialization_contract();
            let preview_resolution_scale = normalize_preview_resolution_scale(
                materialization.authored_preview_resolution_scale(),
            );
            let evaluation = (self.evaluate)(
                &program,
                frame,
                execution_resolution,
                &color_context,
                preview_resolution_scale,
            )
            .map_err(|reason| PreparedVisualFrameClosureError::Evaluation {
                sequence_id: sequence.id,
                frame,
                reason,
            })?;
            validate_evaluation(sequence, frame, &evaluation.plan)?;
            let demands = collect_nested_demands(&evaluation.plan, &evaluation.temporal_batches);
            let node_index = u32::try_from(self.nodes.len())
                .map_err(|_| PreparedVisualFrameClosureError::NodeCapacityExceeded)?;
            let id = PreparedVisualFrameNodeId(node_index);
            self.nodes.push(PreparedVisualFrameNode {
                id,
                materialization,
                program,
                sequence_id: sequence.id,
                sequence_revision: sequence.revision,
                frame,
                time,
                execution_resolution,
                color_context: color_context.clone(),
                instance_path: Arc::clone(&instance_path),
                evaluation,
                bindings: Vec::new(),
                binding_index: HashMap::new(),
            });

            for demand in demands {
                let child = self.sequence(demand.sequence_id).ok_or(
                    PreparedVisualFrameClosureError::MissingNestedSequence {
                        parent_sequence_id: sequence.id,
                        nested_sequence_id: demand.sequence_id,
                        placement: Box::new(demand.placement),
                    },
                )?;
                let child_materialization = self.program(child)?.materialization_contract();
                let child_resolution =
                    self.child_canvas_policy.child_resolution(child.id, child_materialization)?;
                if let PreparedVisualNestedSample::Temporal(effect_request) = demand.sample {
                    let extent = effect_request.frame_extent();
                    if child_resolution.width != extent.width()
                        || child_resolution.height != extent.height()
                    {
                        return Err(
                            PreparedVisualFrameClosureError::TemporalNestedExtentMismatch {
                                parent_sequence_id: sequence.id,
                                nested_sequence_id: child.id,
                                placement: Box::new(demand.placement),
                                child_resolution,
                                required_resolution: Resolution {
                                    width: extent.width(),
                                    height: extent.height(),
                                },
                            },
                        );
                    }
                }
                let child_frame = demand
                    .source_time
                    .to_frame_position(child.settings.frame_rate, FrameRounding::Floor)
                    .map_err(
                        |error| PreparedVisualFrameClosureError::NestedTimeProjection {
                            parent_sequence_id: sequence.id,
                            nested_sequence_id: child.id,
                            placement: Box::new(demand.placement),
                            source_time: demand.source_time,
                            reason: error.to_string(),
                        },
                    )?
                    .frame;
                if child_frame < 0 {
                    return Err(PreparedVisualFrameClosureError::InsufficientNestedHandle {
                        parent_sequence_id: sequence.id,
                        nested_sequence_id: child.id,
                        placement: Box::new(demand.placement),
                        source_time: demand.source_time,
                    });
                }
                let child_context = child
                    .settings
                    .nested_render_color_context(color_context.clone(), demand.color_processing);
                let mut child_path = Vec::with_capacity(instance_path.len().saturating_add(1));
                child_path.extend_from_slice(&instance_path);
                child_path.push(PreparedVisualNestedInstanceStep {
                    placement: demand.placement,
                    sample: demand.sample,
                });
                let child_id = self.prepare_node(
                    child,
                    child_frame,
                    child_resolution,
                    child_context.clone(),
                    Arc::from(child_path),
                    depth.saturating_add(1),
                )?;
                let binding = PreparedVisualNestedBinding {
                    parent: id,
                    child: child_id,
                    placement: demand.placement,
                    sample: demand.sample,
                    source_time: demand.source_time,
                    parent_working_color_space: color_context.working_color_space,
                    child_working_color_space: child_context.working_color_space,
                };
                let node = self
                    .nodes
                    .get_mut(id.index())
                    .ok_or(PreparedVisualFrameClosureError::InternalNodeMissing { node: id })?;
                let key = PreparedVisualNestedBindingKey {
                    placement: demand.placement,
                    sample: demand.sample,
                };
                if node.binding_index.insert(key, child_id).is_some() {
                    return Err(PreparedVisualFrameClosureError::DuplicateNestedBinding {
                        parent_sequence_id: sequence.id,
                        placement: Box::new(demand.placement),
                        sample: demand.sample,
                    });
                }
                node.bindings.push(binding);
            }
            Ok(id)
        })();
        let popped = self.active_path.pop();
        debug_assert_eq!(popped, Some(sequence.id));
        result
    }

    fn sequence(&self, id: SequenceId) -> Option<&'a Sequence> {
        self.sequence_index.get(&id).copied()
    }

    fn program(
        &mut self,
        sequence: &Sequence,
    ) -> Result<Arc<PreparedVisualProgram>, PreparedVisualFrameClosureError> {
        if let Some(program) = self.programs.get(&sequence.id) {
            return Ok(Arc::clone(program));
        }
        let binding = (self.resolve_program)(sequence).map_err(|reason| {
            PreparedVisualFrameClosureError::ProgramResolution { sequence_id: sequence.id, reason }
        })?;
        binding.validate_for_sequence(sequence).map_err(|error| {
            PreparedVisualFrameClosureError::ProgramResolution {
                sequence_id: sequence.id,
                reason: error.to_string(),
            }
        })?;
        let program = Arc::clone(binding.program());
        if program.sequence_id() != sequence.id || program.sequence_revision() != sequence.revision
        {
            return Err(PreparedVisualFrameClosureError::ProgramIdentityMismatch {
                sequence_id: sequence.id,
                sequence_revision: sequence.revision,
                program_sequence_id: program.sequence_id(),
                program_sequence_revision: program.sequence_revision(),
            });
        }
        self.programs.insert(sequence.id, Arc::clone(&program));
        Ok(program)
    }
}

fn validate_evaluation(
    sequence: &Sequence,
    frame: i64,
    plan: &TimelineRenderPlan,
) -> Result<(), PreparedVisualFrameClosureError> {
    if plan.position.frame != frame {
        return Err(PreparedVisualFrameClosureError::EvaluationFrameMismatch {
            sequence_id: sequence.id,
            requested: frame,
            evaluated: plan.position.frame,
        });
    }
    let expected_time_base = sequence.time_base();
    if plan.position.time_base != expected_time_base {
        return Err(PreparedVisualFrameClosureError::EvaluationGridMismatch {
            sequence_id: sequence.id,
            expected: expected_time_base,
            evaluated: plan.position.time_base,
        });
    }
    Ok(())
}

fn collect_nested_demands(
    plan: &TimelineRenderPlan,
    temporal_batches: &[TimelineTemporalDemandBatch],
) -> Vec<NestedDemand> {
    let mut demands = Vec::new();
    let temporal_placements = temporal_batches
        .iter()
        .map(TimelineTemporalDemandBatch::placement)
        .collect::<HashSet<_>>();
    for element in &plan.elements {
        match element {
            TimelineRenderPlanElement::NestedSequence(nested) => {
                if !temporal_placements.contains(&nested.placement) {
                    demands.push(NestedDemand {
                        placement: nested.placement,
                        sample: PreparedVisualNestedSample::Current,
                        sequence_id: nested.sequence_id,
                        source_time: nested.source_time,
                        color_processing: nested.color_processing,
                    });
                }
            }
            TimelineRenderPlanElement::CrossDissolve(transition) => {
                collect_transition_nested_demand(
                    &transition.left,
                    &temporal_placements,
                    &mut demands,
                );
                collect_transition_nested_demand(
                    &transition.right,
                    &temporal_placements,
                    &mut demands,
                );
            }
            TimelineRenderPlanElement::Media(_)
            | TimelineRenderPlanElement::Adjustment(_)
            | TimelineRenderPlanElement::SolidColor(_)
            | TimelineRenderPlanElement::BasicTitle(_) => {}
        }
    }
    for batch in temporal_batches {
        for demand in batch.source_demands() {
            if let TimelineTemporalSource::NestedSequence {
                sequence_id,
                source_time,
                color_processing,
            } = &demand.source
            {
                demands.push(NestedDemand {
                    placement: demand.placement,
                    sample: PreparedVisualNestedSample::Temporal(demand.effect_request),
                    sequence_id: *sequence_id,
                    source_time: *source_time,
                    color_processing: *color_processing,
                });
            }
        }
    }
    demands
}

fn collect_transition_nested_demand(
    input: &TimelineTransitionInputPlan,
    temporal_placements: &HashSet<TimelineClipExecutionRef>,
    demands: &mut Vec<NestedDemand>,
) {
    if let TimelineTransitionInputPlan::NestedSequence(nested) = input {
        if temporal_placements.contains(&nested.placement) {
            return;
        }
        demands.push(NestedDemand {
            placement: nested.placement,
            sample: PreparedVisualNestedSample::Current,
            sequence_id: nested.sequence_id,
            source_time: nested.source_time,
            color_processing: nested.color_processing,
        });
    }
}

fn normalize_preview_resolution_scale(scale: f32) -> f32 {
    if scale.is_finite() {
        scale.clamp(0.125, 1.0)
    } else {
        0.5
    }
}

fn validate_resolution(
    sequence_id: SequenceId,
    resolution: Resolution,
) -> Result<(), PreparedVisualFrameClosureError> {
    if resolution.width == 0 || resolution.height == 0 {
        return Err(PreparedVisualFrameClosureError::InvalidResolution { sequence_id, resolution });
    }
    Ok(())
}

/// Fail-closed recursive visual-closure construction error.
#[derive(Debug)]
pub enum PreparedVisualFrameClosureError {
    /// Root requests are expressed only on a nonnegative Evaluation Grid.
    NegativeRootFrame {
        /// Rejected root frame.
        frame: i64,
    },
    /// Preview runtime divisor was zero.
    InvalidPreviewDimensionDivisor,
    /// Sequence execution raster was empty.
    InvalidResolution {
        /// Sequence whose raster is invalid.
        sequence_id: SequenceId,
        /// Rejected raster.
        resolution: Resolution,
    },
    /// The immutable request carried the same Sequence identity more than once.
    DuplicateSequenceIdentity {
        /// Ambiguous Sequence identity.
        sequence_id: SequenceId,
    },
    /// The consumer could not bind an immutable Program to a Sequence.
    ProgramResolution {
        /// Sequence requiring preparation.
        sequence_id: SequenceId,
        /// Consumer-owned preparation diagnostic.
        reason: String,
    },
    /// A resolved Program belonged to a different author identity or revision.
    ProgramIdentityMismatch {
        /// Requested Sequence identity.
        sequence_id: SequenceId,
        /// Requested Sequence revision.
        sequence_revision: SequenceRevision,
        /// Resolved Program identity.
        program_sequence_id: SequenceId,
        /// Resolved Program revision.
        program_sequence_revision: SequenceRevision,
    },
    /// The Sequence's conservative author projection could not be fingerprinted.
    ProgramAuthorFingerprint {
        /// Sequence whose projection failed.
        sequence_id: SequenceId,
        /// Canonical encoding diagnostic.
        reason: String,
    },
    /// A Program did not compile the exact supplied author projection.
    ProgramAuthorMismatch {
        /// Sequence whose author projection differed.
        sequence_id: SequenceId,
        /// Revision that was insufficient to prove equality.
        sequence_revision: SequenceRevision,
    },
    /// Nested depth exceeded the validated product limit.
    DepthExceeded {
        /// Sequence reached beyond the limit.
        sequence_id: SequenceId,
        /// Maximum allowed child depth.
        maximum: usize,
    },
    /// A Sequence repeated on the active instance path.
    Cycle {
        /// Closed cycle path, including the repeated final identity.
        path: Vec<SequenceId>,
    },
    /// A nested plan referred to a Sequence absent from the immutable closure.
    MissingNestedSequence {
        /// Sequence containing the bad reference.
        parent_sequence_id: SequenceId,
        /// Missing child identity.
        nested_sequence_id: SequenceId,
        /// Exact parent placement.
        placement: Box<TimelineClipExecutionRef>,
    },
    /// Nested source time could not project to the child grid.
    NestedTimeProjection {
        /// Parent Sequence.
        parent_sequence_id: SequenceId,
        /// Child Sequence.
        nested_sequence_id: SequenceId,
        /// Exact placement.
        placement: Box<TimelineClipExecutionRef>,
        /// Exact source time.
        source_time: TimelineTime,
        /// Arithmetic failure.
        reason: String,
    },
    /// Nested source demand projected before child-domain zero.
    InsufficientNestedHandle {
        /// Parent Sequence.
        parent_sequence_id: SequenceId,
        /// Child Sequence.
        nested_sequence_id: SequenceId,
        /// Exact placement.
        placement: Box<TimelineClipExecutionRef>,
        /// Rejected child-local source time.
        source_time: TimelineTime,
    },
    /// Internal recursion attempted to prepare a negative child frame.
    NegativeNestedFrame {
        /// Child Sequence.
        sequence_id: SequenceId,
        /// Rejected frame.
        frame: i64,
    },
    /// A frame could not convert to exact Sequence-local time.
    FrameTimeProjection {
        /// Sequence being prepared.
        sequence_id: SequenceId,
        /// Exact frame.
        frame: i64,
        /// Arithmetic failure.
        reason: String,
    },
    /// Consumer evaluation failed before source materialization.
    Evaluation {
        /// Sequence being evaluated.
        sequence_id: SequenceId,
        /// Exact frame.
        frame: i64,
        /// Consumer-owned diagnostic.
        reason: String,
    },
    /// Consumer returned a plan for another frame.
    EvaluationFrameMismatch {
        /// Sequence being evaluated.
        sequence_id: SequenceId,
        /// Requested frame.
        requested: i64,
        /// Frame carried by the returned plan.
        evaluated: i64,
    },
    /// Consumer returned a plan on another Evaluation Grid.
    EvaluationGridMismatch {
        /// Sequence being evaluated.
        sequence_id: SequenceId,
        /// Sequence grid.
        expected: mondrian_core::Rational,
        /// Plan grid.
        evaluated: mondrian_core::Rational,
    },
    /// Temporal Effect extent did not equal the nested child canvas.
    TemporalNestedExtentMismatch {
        /// Parent Sequence.
        parent_sequence_id: SequenceId,
        /// Child Sequence.
        nested_sequence_id: SequenceId,
        /// Exact placement.
        placement: Box<TimelineClipExecutionRef>,
        /// Canonical child canvas.
        child_resolution: Resolution,
        /// Effect-declared source extent.
        required_resolution: Resolution,
    },
    /// A plan emitted the same exact nested binding twice.
    DuplicateNestedBinding {
        /// Parent Sequence.
        parent_sequence_id: SequenceId,
        /// Exact placement.
        placement: Box<TimelineClipExecutionRef>,
        /// Current or temporal sample identity.
        sample: PreparedVisualNestedSample,
    },
    /// Closure exceeded its typed node-index capacity.
    NodeCapacityExceeded,
    /// Conservative materialized-output byte arithmetic overflowed.
    CpuMaterializationEstimateOverflow,
    /// Internal node insertion invariant failed.
    InternalNodeMissing {
        /// Missing node.
        node: PreparedVisualFrameNodeId,
    },
}

impl fmt::Display for PreparedVisualFrameClosureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NegativeRootFrame { frame } => {
                write!(formatter, "root visual frame {frame} is negative")
            }
            Self::InvalidPreviewDimensionDivisor => {
                write!(formatter, "Preview child-canvas divisor must be non-zero")
            }
            Self::InvalidResolution { sequence_id, resolution } => write!(
                formatter,
                "Sequence {sequence_id} visual canvas is invalid: {}x{}",
                resolution.width, resolution.height
            ),
            Self::DuplicateSequenceIdentity { sequence_id } => write!(
                formatter,
                "visual frame closure request contains duplicate Sequence identity {sequence_id}"
            ),
            Self::ProgramResolution { sequence_id, reason } => write!(
                formatter,
                "Sequence {sequence_id} visual Program resolution failed: {reason}"
            ),
            Self::ProgramIdentityMismatch {
                sequence_id,
                sequence_revision,
                program_sequence_id,
                program_sequence_revision,
            } => write!(
                formatter,
                "Sequence {sequence_id} revision {sequence_revision:?} resolved visual Program {program_sequence_id} revision {program_sequence_revision:?}"
            ),
            Self::ProgramAuthorFingerprint { sequence_id, reason } => write!(
                formatter,
                "Sequence {sequence_id} visual Program author validation failed: {reason}"
            ),
            Self::ProgramAuthorMismatch { sequence_id, sequence_revision } => write!(
                formatter,
                "Sequence {sequence_id} revision {sequence_revision:?} visual Program author fingerprint mismatched"
            ),
            Self::DepthExceeded { sequence_id, maximum } => write!(
                formatter,
                "nested Sequence depth exceeded {maximum} at {sequence_id}"
            ),
            Self::Cycle { path } => write!(
                formatter,
                "nested Sequence cycle detected: {}",
                path.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ),
            Self::MissingNestedSequence {
                parent_sequence_id,
                nested_sequence_id,
                placement,
            } => write!(
                formatter,
                "Sequence {parent_sequence_id} placement {} references missing nested Sequence {nested_sequence_id}",
                placement.clip_id
            ),
            Self::NestedTimeProjection {
                parent_sequence_id,
                nested_sequence_id,
                placement,
                source_time,
                reason,
            } => write!(
                formatter,
                "Sequence {parent_sequence_id} placement {} cannot project nested Sequence {nested_sequence_id} source time {source_time}: {reason}",
                placement.clip_id
            ),
            Self::InsufficientNestedHandle {
                parent_sequence_id,
                nested_sequence_id,
                placement,
                source_time,
            } => write!(
                formatter,
                "Sequence {parent_sequence_id} placement {} has insufficient source handle for nested Sequence {nested_sequence_id} at {source_time}",
                placement.clip_id
            ),
            Self::NegativeNestedFrame { sequence_id, frame } => write!(
                formatter,
                "nested Sequence {sequence_id} projected to negative frame {frame}"
            ),
            Self::FrameTimeProjection { sequence_id, frame, reason } => write!(
                formatter,
                "Sequence {sequence_id} frame {frame} cannot project to exact time: {reason}"
            ),
            Self::Evaluation { sequence_id, frame, reason } => write!(
                formatter,
                "Sequence {sequence_id} frame {frame} visual evaluation failed: {reason}"
            ),
            Self::EvaluationFrameMismatch { sequence_id, requested, evaluated } => write!(
                formatter,
                "Sequence {sequence_id} visual evaluation returned frame {evaluated} for request {requested}"
            ),
            Self::EvaluationGridMismatch { sequence_id, expected, evaluated } => write!(
                formatter,
                "Sequence {sequence_id} visual evaluation grid mismatch: expected {expected}, returned {evaluated}"
            ),
            Self::TemporalNestedExtentMismatch {
                parent_sequence_id,
                nested_sequence_id,
                placement,
                child_resolution,
                required_resolution,
            } => write!(
                formatter,
                "Sequence {parent_sequence_id} placement {} temporal nested Sequence {nested_sequence_id} canvas {}x{} differs from required {}x{}",
                placement.clip_id,
                child_resolution.width,
                child_resolution.height,
                required_resolution.width,
                required_resolution.height
            ),
            Self::DuplicateNestedBinding { parent_sequence_id, placement, sample } => write!(
                formatter,
                "Sequence {parent_sequence_id} placement {} emitted duplicate nested binding {sample:?}",
                placement.clip_id
            ),
            Self::NodeCapacityExceeded => {
                write!(formatter, "visual frame closure exceeds u32 node capacity")
            }
            Self::CpuMaterializationEstimateOverflow => {
                write!(formatter, "visual frame closure CPU materialization estimate overflowed")
            }
            Self::InternalNodeMissing { node } => {
                write!(formatter, "visual frame closure lost node {}", node.index())
            }
        }
    }
}

impl std::error::Error for PreparedVisualFrameClosureError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        evaluate_prepared_visual_program, PreparedVisualProgram, TimelineCompositeScratch,
        TimelineCpuCompositePrecision, TimelineCpuWorkingSetError, TimelineCpuWorkingSetGrant,
    };
    use mondrian_core::timeline_data::TimelineClipEndpointContext;
    use mondrian_core::{
        ExecutionCancellationToken, ProjectColorEnvironment, Rational, TimelineTimeRange,
    };
    use mondrian_effects::{
        register_effect_definition, EffectColorDomainContract, EffectDefinition, EffectDeterminism,
        EffectExecutionContinuity, EffectExecutionContract, EffectExecutionModes,
        EffectFrameExtent, EffectGraphTopology, EffectNode, EffectRenderOp, EffectResourceLifetime,
        EffectRoiPropagation, EffectStateModel, EffectTemporalInputExtent, EffectTemporalSpan,
        EffectType,
    };
    use mondrian_timeline::{Clip, Track, VideoTransition};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn frame_time(frame: i64, time_base: Rational) -> TimelineTime {
        TimelineTime::from_frame_position(FramePosition::new(frame, time_base))
            .expect("exact frame time")
    }

    fn root_color_context(sequence: &Sequence) -> ProgramColorContext {
        sequence
            .settings
            .root_program_color_context(&ProjectColorEnvironment::default())
    }

    fn prepare_direct_closure(
        root: &Sequence,
        sequences: &[Sequence],
        root_frame: i64,
        root_resolution: Resolution,
        child_canvas_policy: PreparedVisualChildCanvasPolicy,
    ) -> Result<PreparedVisualFrameClosure<()>, PreparedVisualFrameClosureError> {
        prepare_visual_frame_closure(
            PreparedVisualFrameClosureRequest {
                root_sequence: root,
                sequences,
                root_frame,
                root_resolution,
                root_color_context: root_color_context(root),
                child_canvas_policy,
            },
            |sequence| {
                PreparedVisualProgram::prepare(sequence)
                    .map(Arc::new)
                    .map_err(|error| error.to_string())
            },
            |program, frame, _, _, _| {
                let plan = evaluate_prepared_visual_program(
                    program,
                    crate::TimelineEvaluationRequest::export(FramePosition::new(
                        frame,
                        program.evaluation_time_base(),
                    )),
                )
                .map_err(|error| error.to_string())?;
                Ok(PreparedVisualFrameEvaluation::new(plan, Vec::new(), ()))
            },
        )
    }

    #[test]
    fn frame_node_copies_materialization_contract_from_its_exact_program() {
        let mut sequence = Sequence::new("node materialization contract");
        sequence.settings.resolution = Resolution { width: 4096, height: 2160 };
        sequence.settings.preview.resolution_scale = 0.625;
        sequence.settings.title_safe_margin = 0.08125;
        let execution_resolution = Resolution { width: 1920, height: 1080 };

        let closure = prepare_direct_closure(
            &sequence,
            &[],
            0,
            execution_resolution,
            PreparedVisualChildCanvasPolicy::Authored,
        )
        .expect("prepare closure");
        let node = closure.node(closure.root()).expect("root node");

        assert_eq!(node.execution_resolution(), execution_resolution);
        assert_eq!(node.author_resolution(), sequence.settings.resolution);
        assert_eq!(
            node.materialization_contract(),
            node.program().materialization_contract()
        );
        assert_eq!(
            node.materialization_contract().authored_preview_resolution_scale().to_bits(),
            sequence.settings.preview.resolution_scale.to_bits()
        );
        assert_eq!(
            node.title_safe_margin().to_bits(),
            sequence.settings.title_safe_margin.to_bits()
        );
    }

    #[test]
    fn frame_closure_does_not_resample_preview_scale_from_raw_sequence() {
        let production = include_str!("prepared_visual_frame_closure.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production frame-closure source");

        assert!(!production.contains("settings.preview.resolution_scale"));
        assert!(production.contains("authored_preview_resolution_scale"));
    }

    fn temporal_effect(offset: TimelineTime) -> EffectNode {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let effect_type = EffectType::Plugin(format!("test.prepared-visual-closure.temporal.{id}"));
        register_effect_definition(
            EffectDefinition::new(
                effect_type.key(),
                "Prepared visual closure temporal test",
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
                graph.append_unary(EffectRenderOp::TemporalFrameBlend {
                    sample_offset: TimelineTime::ZERO.checked_sub(offset).expect("past offset"),
                    mix: 0.25,
                });
                Ok(())
            })),
        )
        .expect("register temporal test Effect");
        EffectNode::new(effect_type)
    }

    fn prepare_temporal_closure(
        root: &Sequence,
        sequences: &[Sequence],
        root_frame: i64,
        root_resolution: Resolution,
        child_canvas_policy: PreparedVisualChildCanvasPolicy,
    ) -> Result<PreparedVisualFrameClosure<()>, PreparedVisualFrameClosureError> {
        prepare_visual_frame_closure(
            PreparedVisualFrameClosureRequest {
                root_sequence: root,
                sequences,
                root_frame,
                root_resolution,
                root_color_context: root_color_context(root),
                child_canvas_policy,
            },
            |sequence| {
                PreparedVisualProgram::prepare(sequence)
                    .map(Arc::new)
                    .map_err(|error| error.to_string())
            },
            |program, frame, resolution, _, _| {
                let plan = evaluate_prepared_visual_program(
                    program,
                    crate::TimelineEvaluationRequest::export(FramePosition::new(
                        frame,
                        program.evaluation_time_base(),
                    )),
                )
                .map_err(|error| error.to_string())?;
                let extent = EffectFrameExtent::new(resolution.width, resolution.height);
                let prepared = crate::timeline_temporal::prepare_timeline_temporal_execution(
                    program,
                    &plan,
                    7,
                    EffectExecutionContinuity::Discontinuous,
                    extent,
                    extent.full_frame_roi(),
                    ExecutionCancellationToken::new(),
                )
                .map_err(|error| error.to_string())?;
                let (plan, temporal_batches) = prepared.into_parts();
                Ok(PreparedVisualFrameEvaluation::new(
                    plan,
                    temporal_batches,
                    (),
                ))
            },
        )
    }

    #[test]
    fn duplicate_sequence_identities_fail_before_frame_evaluation() {
        let root = Sequence::new("root");
        let duplicate = root.clone();
        let mut resolutions = 0_usize;
        let mut evaluations = 0_usize;
        let error = prepare_visual_frame_closure::<()>(
            PreparedVisualFrameClosureRequest {
                root_sequence: &root,
                sequences: &[duplicate],
                root_frame: 0,
                root_resolution: root.settings.resolution,
                root_color_context: root_color_context(&root),
                child_canvas_policy: PreparedVisualChildCanvasPolicy::Authored,
            },
            |_| {
                resolutions = resolutions.saturating_add(1);
                Err("duplicate request must not resolve a Program".to_owned())
            },
            |_, _, _, _, _| {
                evaluations = evaluations.saturating_add(1);
                Err("duplicate request must not evaluate".to_owned())
            },
        )
        .expect_err("duplicate Sequence identity must fail closed");

        assert_eq!(resolutions, 0);
        assert_eq!(evaluations, 0);
        assert!(matches!(
            error,
            PreparedVisualFrameClosureError::DuplicateSequenceIdentity { sequence_id }
                if sequence_id == root.id
        ));
    }

    #[test]
    fn stale_same_revision_program_is_rejected_before_frame_evaluation() {
        let mut root = Sequence::new("stale Program");
        let stale =
            Arc::new(PreparedVisualProgram::prepare(&root).expect("prepare original Program"));
        root.settings.title_safe_margin += 0.01;
        let evaluated = std::cell::Cell::new(false);

        let error = prepare_visual_frame_closure(
            PreparedVisualFrameClosureRequest {
                root_sequence: &root,
                sequences: &[],
                root_frame: 0,
                root_resolution: Resolution { width: 64, height: 36 },
                root_color_context: root_color_context(&root),
                child_canvas_policy: PreparedVisualChildCanvasPolicy::Authored,
            },
            |_| Ok(Arc::clone(&stale)),
            |_, _, _, _, _| {
                evaluated.set(true);
                Err::<PreparedVisualFrameEvaluation<()>, _>(
                    "stale Program reached frame evaluation".to_owned(),
                )
            },
        )
        .expect_err("stale same-revision Program must fail closed");

        assert!(!evaluated.get());
        assert!(matches!(
            error,
            PreparedVisualFrameClosureError::ProgramResolution {
                sequence_id,
                reason,
            } if sequence_id == root.id && reason.contains("fingerprint")
        ));
    }

    #[test]
    fn closure_rejects_an_evaluator_plan_on_a_parallel_grid() {
        let root = Sequence::new("parallel evaluator grid");
        let expected = root.time_base();
        let equivalent_but_not_identical = Rational::new(expected.num * 2, expected.den * 2);

        let error = prepare_visual_frame_closure(
            PreparedVisualFrameClosureRequest {
                root_sequence: &root,
                sequences: &[],
                root_frame: 3,
                root_resolution: Resolution { width: 64, height: 36 },
                root_color_context: root_color_context(&root),
                child_canvas_policy: PreparedVisualChildCanvasPolicy::Authored,
            },
            |sequence| {
                PreparedVisualProgram::prepare(sequence)
                    .map(Arc::new)
                    .map_err(|error| error.to_string())
            },
            |program, frame, _, _, _| {
                let mut plan = evaluate_prepared_visual_program(
                    program,
                    crate::TimelineEvaluationRequest::export(FramePosition::new(
                        frame,
                        program.evaluation_time_base(),
                    )),
                )
                .map_err(|error| error.to_string())?;
                plan.position.time_base = equivalent_but_not_identical;
                Ok(PreparedVisualFrameEvaluation::new(plan, Vec::new(), ()))
            },
        )
        .expect_err("closure must reject an evaluator plan on another grid");

        assert!(matches!(
            error,
            PreparedVisualFrameClosureError::EvaluationGridMismatch {
                sequence_id,
                expected: returned_expected,
                evaluated,
            } if sequence_id == root.id
                && returned_expected == expected
                && evaluated == equivalent_but_not_identical
        ));
    }

    #[test]
    fn root_role_may_alias_its_exact_snapshot_in_the_sequence_collection() {
        let root = Sequence::new("root");
        let closure = prepare_direct_closure(
            &root,
            std::slice::from_ref(&root),
            0,
            root.settings.resolution,
            PreparedVisualChildCanvasPolicy::Authored,
        )
        .expect("the root role is allowed to alias its exact indexed snapshot");

        assert_eq!(closure.len(), 1);
        assert_eq!(
            closure.node(closure.root()).expect("root node").sequence_id(),
            root.id
        );
    }

    #[test]
    fn equal_child_samples_from_distinct_placements_keep_distinct_instance_paths() {
        let mut child = Sequence::new("child");
        child.settings.resolution = Resolution { width: 2, height: 1 };

        let mut root = Sequence::new("root");
        root.settings.resolution = Resolution { width: 4, height: 2 };
        let duration = frame_time(24, root.time_base());
        let first = Clip::new_nested_sequence(
            child.id,
            TimelineTime::ZERO,
            duration,
            Some("first".to_owned()),
        )
        .expect("first placement");
        let first_id = first.id;
        root.video_tracks[0].add_clip(first).expect("first track");
        let mut second_track = Track::new_video("V2");
        let second = Clip::new_nested_sequence(
            child.id,
            TimelineTime::ZERO,
            duration,
            Some("second".to_owned()),
        )
        .expect("second placement");
        let second_id = second.id;
        second_track.add_clip(second).expect("second track");
        root.video_tracks.push(second_track);

        let closure = prepare_direct_closure(
            &root,
            std::slice::from_ref(&child),
            0,
            root.settings.resolution,
            PreparedVisualChildCanvasPolicy::Authored,
        )
        .expect("prepared closure");
        let root_node = closure.node(closure.root()).expect("root node");
        assert_eq!(root_node.bindings().len(), 2);
        let first_child = root_node
            .bindings()
            .iter()
            .find(|binding| binding.placement().clip_id == first_id)
            .expect("first binding")
            .child();
        let second_child = root_node
            .bindings()
            .iter()
            .find(|binding| binding.placement().clip_id == second_id)
            .expect("second binding")
            .child();
        assert_ne!(first_child, second_child);
        for child_id in [first_child, second_child] {
            let node = closure.node(child_id).expect("child node");
            assert_eq!(node.sequence_id(), child.id);
            assert_eq!(node.frame(), 0);
            assert_eq!(node.instance_path().len(), 1);
        }
        assert_ne!(
            closure.node(first_child).expect("first child").instance_path(),
            closure.node(second_child).expect("second child").instance_path()
        );
    }

    #[test]
    fn mixed_rate_time_and_preview_child_canvas_are_bound_once() {
        let mut child = Sequence::new("30 fps child");
        child.settings.frame_rate = Rational::new(30, 1);
        child.settings.resolution = Resolution { width: 1280, height: 720 };
        child.settings.preview.resolution_scale = 0.5;
        child.settings.color.working_color_space = WorkingColorSpace::LinearRec2020;

        let mut root = Sequence::new("24 fps root");
        root.settings.frame_rate = Rational::new(24, 1);
        root.settings.color.working_color_space = WorkingColorSpace::LinearRec709;
        let duration = frame_time(24, root.time_base());
        root.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(
                    child.id,
                    TimelineTime::ZERO,
                    duration,
                    Some("child".to_owned()),
                )
                .expect("nested placement"),
            )
            .expect("add nested placement");

        let closure = prepare_direct_closure(
            &root,
            std::slice::from_ref(&child),
            12,
            Resolution { width: 64, height: 36 },
            PreparedVisualChildCanvasPolicy::preview_scaled(4).expect("Preview policy"),
        )
        .expect("prepared closure");
        let binding = &closure.node(closure.root()).expect("root").bindings()[0];
        assert_eq!(
            binding.source_time(),
            TimelineTime::new(1, 2).expect("half second")
        );
        let child_node = closure.node(binding.child()).expect("child");
        assert_eq!(child_node.frame(), 15);
        assert_eq!(
            child_node.time(),
            TimelineTime::new(1, 2).expect("half second")
        );
        assert_eq!(
            child_node.execution_resolution(),
            Resolution { width: 160, height: 90 }
        );
        assert_eq!(
            binding.parent_working_color_space(),
            WorkingColorSpace::LinearRec709
        );
        assert_eq!(
            binding.child_working_color_space(),
            WorkingColorSpace::LinearRec2020
        );
        assert_eq!(
            child_node.color_context().working_color_space,
            WorkingColorSpace::LinearRec2020
        );
    }

    #[test]
    fn transition_endpoints_are_distinct_instance_path_edges() {
        let child = Sequence::new("transition child");
        let mut root = Sequence::new("transition root");
        root.settings.frame_rate = Rational::new(30, 1);
        root.video_tracks.clear();
        let mut track = Track::new_video("V1");
        let left = Clip::new_nested_sequence(
            child.id,
            TimelineTime::ZERO,
            TimelineTime::new(1, 1).expect("one second"),
            Some("left".to_owned()),
        )
        .expect("left");
        let left_id = left.id;
        let right = Clip::new_nested_sequence(
            child.id,
            TimelineTime::new(1, 1).expect("right start"),
            TimelineTime::new(1, 1).expect("one second"),
            Some("right".to_owned()),
        )
        .expect("right");
        let right_id = right.id;
        track.add_clip(left).expect("left Clip");
        track.add_clip(right).expect("right Clip");
        root.video_tracks.push(track);
        let transition = VideoTransition::cross_dissolve(
            left_id,
            right_id,
            TimelineTimeRange::new(
                TimelineTime::new(29, 30).expect("transition start"),
                TimelineTime::new(2, 30).expect("transition duration"),
            )
            .expect("transition range"),
        );
        let transition_id = transition.id;
        root.video_transitions.push(transition);

        let closure = prepare_direct_closure(
            &root,
            std::slice::from_ref(&child),
            30,
            root.settings.resolution,
            PreparedVisualChildCanvasPolicy::Authored,
        )
        .expect("prepared closure");
        let bindings = closure.node(closure.root()).expect("root").bindings();
        assert_eq!(bindings.len(), 2);
        assert!(bindings.iter().any(|binding| {
            binding.placement().endpoint
                == TimelineClipEndpointContext::TransitionLeft { transition_id }
        }));
        assert!(bindings.iter().any(|binding| {
            binding.placement().endpoint
                == TimelineClipEndpointContext::TransitionRight { transition_id }
        }));
        assert!(bindings.iter().all(|binding| {
            closure.node(binding.child()).is_some_and(|node| {
                node.instance_path()
                    == [PreparedVisualNestedInstanceStep {
                        placement: binding.placement(),
                        sample: PreparedVisualNestedSample::Current,
                    }]
            })
        }));
    }

    #[test]
    fn temporal_nested_requests_bind_exact_history_without_current_alias() {
        let mut child = Sequence::new("temporal child");
        child.settings.frame_rate = Rational::new(30, 1);
        child.settings.resolution = Resolution { width: 4, height: 2 };
        let mut root = Sequence::new("temporal root");
        root.settings.frame_rate = Rational::new(30, 1);
        root.settings.resolution = Resolution { width: 4, height: 2 };
        let mut nested = Clip::new_nested_sequence(
            child.id,
            TimelineTime::ZERO,
            TimelineTime::new(1, 1).expect("duration"),
            Some("temporal child".to_owned()),
        )
        .expect("nested placement");
        nested.add_effect_node(temporal_effect(
            TimelineTime::new(1, 30).expect("one frame"),
        ));
        root.video_tracks[0].add_clip(nested).expect("nested Clip");

        let closure = prepare_temporal_closure(
            &root,
            std::slice::from_ref(&child),
            1,
            root.settings.resolution,
            PreparedVisualChildCanvasPolicy::Authored,
        )
        .expect("temporal closure");
        let root_node = closure.node(closure.root()).expect("root");
        assert_eq!(root_node.bindings().len(), 2);
        assert!(root_node
            .bindings()
            .iter()
            .all(|binding| matches!(binding.sample(), PreparedVisualNestedSample::Temporal(_))));
        let mut child_frames = root_node
            .bindings()
            .iter()
            .map(|binding| closure.node(binding.child()).expect("child").frame())
            .collect::<Vec<_>>();
        child_frames.sort_unstable();
        assert_eq!(child_frames, vec![0, 1]);
    }

    #[test]
    fn temporal_nested_canvas_mismatch_fails_before_pixel_materialization() {
        let mut child = Sequence::new("small temporal child");
        child.settings.resolution = Resolution { width: 2, height: 1 };
        let mut root = Sequence::new("large temporal root");
        root.settings.resolution = Resolution { width: 4, height: 2 };
        let mut nested = Clip::new_nested_sequence(
            child.id,
            TimelineTime::ZERO,
            TimelineTime::new(1, 1).expect("duration"),
            None,
        )
        .expect("nested placement");
        nested.add_effect_node(temporal_effect(
            TimelineTime::new(1, 24).expect("one frame"),
        ));
        root.video_tracks[0].add_clip(nested).expect("nested Clip");

        let error = prepare_temporal_closure(
            &root,
            std::slice::from_ref(&child),
            1,
            root.settings.resolution,
            PreparedVisualChildCanvasPolicy::Authored,
        )
        .expect_err("temporal source resampling is not an admitted hidden fallback");
        assert!(
            matches!(
                &error,
                PreparedVisualFrameClosureError::TemporalNestedExtentMismatch {
                    child_resolution: Resolution { width: 2, height: 1 },
                    required_resolution: Resolution { width: 4, height: 2 },
                    ..
                }
            ),
            "unexpected temporal mismatch error: {error:?}"
        );
    }

    #[test]
    fn cycles_fail_before_any_recursive_materialization() {
        let mut root = Sequence::new("cycle root");
        let mut child = Sequence::new("cycle child");
        let root_duration = frame_time(24, root.time_base());
        let child_duration = frame_time(24, child.time_base());
        root.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(child.id, TimelineTime::ZERO, root_duration, None)
                    .expect("root to child"),
            )
            .expect("root placement");
        child.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(root.id, TimelineTime::ZERO, child_duration, None)
                    .expect("child to root"),
            )
            .expect("child placement");

        let error = prepare_direct_closure(
            &root,
            std::slice::from_ref(&child),
            0,
            root.settings.resolution,
            PreparedVisualChildCanvasPolicy::Authored,
        )
        .expect_err("cycle must fail");
        assert!(matches!(
            error,
            PreparedVisualFrameClosureError::Cycle { .. }
        ));
    }

    #[test]
    fn depth_limit_is_enforced_by_the_canonical_active_path() {
        let mut chain = (0..=MAX_NESTED_SEQUENCE_RENDER_DEPTH + 1)
            .map(|index| Sequence::new(format!("depth {index}")))
            .collect::<Vec<_>>();
        for index in 0..chain.len() - 1 {
            let child_id = chain[index + 1].id;
            let duration = frame_time(24, chain[index].time_base());
            chain[index].video_tracks[0]
                .add_clip(
                    Clip::new_nested_sequence(child_id, TimelineTime::ZERO, duration, None)
                        .expect("nested depth placement"),
                )
                .expect("add nested depth placement");
        }
        let root = chain.remove(0);
        let error = prepare_direct_closure(
            &root,
            &chain,
            0,
            root.settings.resolution,
            PreparedVisualChildCanvasPolicy::Authored,
        )
        .expect_err("depth beyond the product limit must fail");
        assert!(matches!(
            error,
            PreparedVisualFrameClosureError::DepthExceeded {
                maximum: MAX_NESTED_SEQUENCE_RENDER_DEPTH,
                ..
            }
        ));
    }

    #[test]
    fn materialization_estimate_overflow_fails_closed() {
        let root = Sequence::new("overflow");
        let closure = prepare_direct_closure(
            &root,
            &[],
            0,
            Resolution { width: u32::MAX, height: u32::MAX },
            PreparedVisualChildCanvasPolicy::Authored,
        )
        .expect("semantic closure can represent the nonzero extent");
        assert!(matches!(
            closure.conservative_cpu_materialization_active_bytes(),
            Err(PreparedVisualFrameClosureError::CpuMaterializationEstimateOverflow)
        ));
    }

    #[test]
    fn conservative_nested_outputs_share_the_compositor_hard_grant() {
        let mut child = Sequence::new("small child");
        child.settings.resolution = Resolution { width: 2, height: 1 };
        let mut root = Sequence::new("small root");
        root.settings.resolution = Resolution { width: 4, height: 2 };
        let root_time_base = root.time_base();
        root.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(
                    child.id,
                    TimelineTime::ZERO,
                    frame_time(24, root_time_base),
                    None,
                )
                .expect("nested placement"),
            )
            .expect("add nested");
        let closure = prepare_direct_closure(
            &root,
            std::slice::from_ref(&child),
            0,
            root.settings.resolution,
            PreparedVisualChildCanvasPolicy::Authored,
        )
        .expect("prepared closure");
        // Node outputs: (4*2 + 2*1) * 16 = 160. Largest-node local
        // compositor reserve: 4 * (4*2*16) = 512.
        let required = closure
            .conservative_cpu_materialization_active_bytes()
            .expect("checked estimate");
        assert_eq!(required, 672);

        let mut scratch = TimelineCompositeScratch::default();
        scratch.reconfigure_cpu_working_set(TimelineCpuWorkingSetGrant {
            max_active_bytes: required - 1,
            max_retained_scratch_bytes: u64::MAX,
        });
        assert!(matches!(
            scratch.admit_cpu_active_working_set(required, TimelineCpuCompositePrecision::Float32),
            Err(TimelineCpuWorkingSetError::ActiveGrantExceeded {
                required_bytes: 672,
                granted_bytes: 671,
                precision: TimelineCpuCompositePrecision::Float32,
            })
        ));
        scratch.reconfigure_cpu_working_set(TimelineCpuWorkingSetGrant {
            max_active_bytes: required,
            max_retained_scratch_bytes: u64::MAX,
        });
        scratch
            .admit_cpu_active_working_set(required, TimelineCpuCompositePrecision::Float32)
            .expect("exact hard grant");
    }
}
