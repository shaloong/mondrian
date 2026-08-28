use crate::{
    effect::{
        effect_definition, effect_registry_revision, EffectDefinition, EffectEvalContext,
        EffectGraphBuildError, EffectPreparationContext, EffectResourceDependency,
        PreparedEffectEvaluator, PreparedLut3D,
    },
    graph::{
        identity_compiled_effect_graph, prepare_effect_graph_topology, CompiledEffectGraph,
        CompiledEffectStageBinding, EffectGraphBuilderState, EffectGraphNode, EffectGraphNodeId,
        EffectGraphNodeKind, EffectRenderGraph, PreparedEffectGraphTopology,
    },
    mask::MaskComponent,
    EffectDeterminism, EffectExecutionContract, EffectExecutionEnvelope, EffectExecutionModes,
    EffectExecutionSession, EffectGraphTopology, EffectResourceLifetime, EffectRoiPropagation,
    EffectStateModel, EffectTemporalInputExtent, LutPreparationCache,
};
use mondrian_core::{
    effect_data::EffectNode, EffectId, GradeGraph, GradeGraphNodeId, GradeGraphNodeKind,
    TimelineTime, WorkingColorSpace,
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    panic::{catch_unwind, AssertUnwindSafe},
    path::PathBuf,
    sync::Arc,
};

const MAX_DEFINITION_BIND_RETRIES: usize = 32;

#[derive(Clone)]
struct PreparedEffectInstance {
    effect: EffectNode,
    definition: Arc<EffectDefinition>,
    evaluator: PreparedEffectEvaluator,
}

/// Definition-bound, resource-prepared visual effect stack.
#[derive(Clone)]
pub struct PreparedEffectStack {
    instances: Arc<[PreparedEffectInstance]>,
    working_color_space: WorkingColorSpace,
    execution_envelope: EffectExecutionEnvelope,
    dependencies: Arc<[EffectResourceDependency]>,
    definition_registry_revision: u64,
    zero_graph: Arc<EffectRenderGraph>,
    zero_stage_bindings: Arc<[CompiledEffectStageBinding]>,
}

#[derive(Debug, Clone)]
struct EvaluatedEffectGraph {
    graph: EffectRenderGraph,
    stage_bindings: Arc<[CompiledEffectStageBinding]>,
}

impl std::fmt::Debug for PreparedEffectStack {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedEffectStack")
            .field("instance_count", &self.instances.len())
            .field("working_color_space", &self.working_color_space)
            .field("execution_envelope", &self.execution_envelope)
            .field("dependencies", &self.dependencies)
            .field(
                "definition_registry_revision",
                &self.definition_registry_revision,
            )
            .finish_non_exhaustive()
    }
}

impl PreparedEffectStack {
    /// Bind definitions, validate exact parameter schemas, resolve immutable
    /// resources, and validate the initial graph topology.
    pub fn prepare(
        effects: &[EffectNode],
        working_color_space: WorkingColorSpace,
    ) -> Result<Self, EffectGraphBuildError> {
        let lut_cache = LutPreparationCache::uncached();
        Self::prepare_with_lut_cache(effects, working_color_space, &lut_cache)
    }

    /// Bind definitions and immutable resources through one caller-owned LUT
    /// preparation cache.
    pub fn prepare_with_lut_cache(
        effects: &[EffectNode],
        working_color_space: WorkingColorSpace,
        lut_cache: &LutPreparationCache,
    ) -> Result<Self, EffectGraphBuildError> {
        let mut concurrent_change = None;
        for _ in 0..MAX_DEFINITION_BIND_RETRIES {
            match Self::prepare_once(effects, working_color_space, lut_cache) {
                Err(error @ EffectGraphBuildError::DefinitionRegistryChanged { .. }) => {
                    concurrent_change = Some(error);
                }
                result => return result,
            }
        }
        match concurrent_change {
            Some(error) => Err(error),
            None => Err(EffectGraphBuildError::InvalidGraph),
        }
    }

    fn prepare_once(
        effects: &[EffectNode],
        working_color_space: WorkingColorSpace,
        lut_cache: &LutPreparationCache,
    ) -> Result<Self, EffectGraphBuildError> {
        let definition_registry_revision = effect_registry_revision();
        let mut instances = Vec::new();
        let mut dependencies = Vec::new();
        let mut execution_contract = EffectExecutionContract::IDENTITY;
        let mut stage_contracts = Vec::new();

        for effect in effects.iter().filter(|effect| effect.is_enabled) {
            let effect_key = effect.effect_type.key();
            let definition = effect_definition(&effect.effect_type).ok_or_else(|| {
                EffectGraphBuildError::DefinitionUnavailable {
                    effect_key: effect_key.clone(),
                    effect_id: effect.id,
                }
            })?;
            if !crate::plugin_contract::effect_plugin_is_runtime_available(
                definition.key(),
                definition.definition_registry_revision(),
                definition.plugin_contract(),
            ) {
                return Err(EffectGraphBuildError::RuntimeUnavailable {
                    effect_key,
                    effect_id: effect.id,
                });
            }
            if !definition.has_evaluator() {
                return Err(EffectGraphBuildError::EvaluationUnsupported {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                });
            }

            validate_effect_schema(effect, &definition)?;
            let contract = definition.execution_contract();
            contract.validate().map_err(|error| {
                EffectGraphBuildError::InvalidExecutionContract {
                    reason: format!("{}: {error}", definition.key()),
                }
            })?;
            if contract.execution_modes.is_empty() {
                return Err(EffectGraphBuildError::ExecutionContractUnavailable {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                });
            }
            execution_contract = execution_contract.compose(contract).map_err(|error| {
                EffectGraphBuildError::InvalidExecutionContract { reason: error.to_string() }
            })?;
            stage_contracts.push(contract);

            let evaluator = definition.prepare_evaluator(
                effect,
                working_color_space,
                EffectPreparationContext::new(lut_cache),
            )?;
            if !evaluator.dependencies().is_empty()
                && contract.resource_lifetime < EffectResourceLifetime::PreparedProgram
            {
                return Err(EffectGraphBuildError::ExecutionContractViolation {
                    effect_key: effect.effect_type.key(),
                    effect_id: effect.id,
                    violation: Box::new(
                        crate::EffectExecutionContractViolation::ResourceLifetimeTooShort {
                            declared: contract.resource_lifetime,
                            required: EffectResourceLifetime::PreparedProgram,
                        },
                    ),
                });
            }
            dependencies.extend_from_slice(evaluator.dependencies());
            instances.push(PreparedEffectInstance {
                effect: effect.clone(),
                definition,
                evaluator,
            });
        }
        let instances: Arc<[PreparedEffectInstance]> = instances.into();
        let zero_evaluation =
            evaluate_instances(&instances, working_color_space, TimelineTime::ZERO)?;
        // Preparation proves both acyclicity and the declared topology at one
        // canonical instant. Every later evaluation repeats the per-instance
        // topology check before a new shape can enter its execution Session.
        prepare_effect_graph_topology(&zero_evaluation.graph)
            .ok_or(EffectGraphBuildError::InvalidGraph)?;
        let final_registry_revision = effect_registry_revision();
        if final_registry_revision != definition_registry_revision {
            return Err(EffectGraphBuildError::DefinitionRegistryChanged {
                before: definition_registry_revision,
                after: final_registry_revision,
            });
        }

        Ok(Self {
            instances,
            working_color_space,
            execution_envelope: EffectExecutionEnvelope::new(
                execution_contract,
                Arc::<[EffectExecutionContract]>::from(stage_contracts),
            ),
            dependencies: dependencies.into(),
            definition_registry_revision,
            zero_graph: Arc::new(zero_evaluation.graph),
            zero_stage_bindings: zero_evaluation.stage_bindings,
        })
    }

    /// Evaluate only frame-varying parameters into a graph instance.
    pub fn evaluate_graph(
        &self,
        time: TimelineTime,
    ) -> Result<EffectRenderGraph, EffectGraphBuildError> {
        if time == TimelineTime::ZERO {
            return Ok((*self.zero_graph).clone());
        }
        Ok(evaluate_instances(&self.instances, self.working_color_space, time)?.graph)
    }

    fn evaluate_with_stage_bindings(
        &self,
        time: TimelineTime,
    ) -> Result<EvaluatedEffectGraph, EffectGraphBuildError> {
        if time == TimelineTime::ZERO {
            return Ok(EvaluatedEffectGraph {
                graph: (*self.zero_graph).clone(),
                stage_bindings: Arc::clone(&self.zero_stage_bindings),
            });
        }
        evaluate_instances(&self.instances, self.working_color_space, time)
    }

    /// Aggregated execution contract for the complete stack.
    pub const fn execution_contract(&self) -> EffectExecutionContract {
        self.execution_envelope.aggregate()
    }

    /// Ordered per-stage contracts and homogeneous-backend evidence.
    pub fn execution_envelope(&self) -> &EffectExecutionEnvelope {
        &self.execution_envelope
    }

    /// Definition registry revision bound during preparation.
    pub const fn definition_registry_revision(&self) -> u64 {
        self.definition_registry_revision
    }

    /// Immutable resources retained by the stack.
    pub fn dependencies(&self) -> &[EffectResourceDependency] {
        &self.dependencies
    }

    /// Whether this stack retains any resource whose currentness is external
    /// to the owning Sequence revision.
    pub fn has_external_dependencies(&self) -> bool {
        !self.dependencies.is_empty()
    }

    fn retained_bytes_estimate(&self) -> usize {
        let instances = self
            .instances
            .iter()
            .map(|instance| {
                let author_bytes = serde_json::to_vec(&instance.effect)
                    .map_or(1024, |bytes| bytes.len().max(1024));
                std::mem::size_of::<PreparedEffectInstance>()
                    .saturating_add(author_bytes)
                    .saturating_add(instance.definition.retained_bytes_estimate())
                    .saturating_add(instance.evaluator.retained_bytes_estimate())
            })
            .fold(0_usize, usize::saturating_add);
        let dependencies = self
            .dependencies
            .iter()
            .map(|dependency| match dependency {
                EffectResourceDependency::CubeLut { path, .. } => path.as_os_str().len(),
                EffectResourceDependency::PluginManaged { identity } => identity.capacity(),
            })
            .fold(0_usize, usize::saturating_add);
        let binding_nodes = self
            .zero_stage_bindings
            .iter()
            .map(|binding| {
                binding
                    .emitted_nodes()
                    .len()
                    .saturating_mul(std::mem::size_of::<EffectGraphNodeId>())
            })
            .fold(0_usize, usize::saturating_add);
        std::mem::size_of::<Self>()
            .saturating_add(
                self.instances
                    .len()
                    .saturating_mul(std::mem::size_of::<PreparedEffectInstance>()),
            )
            .saturating_add(instances)
            .saturating_add(
                self.execution_envelope
                    .stages()
                    .len()
                    .saturating_mul(std::mem::size_of::<EffectExecutionContract>()),
            )
            .saturating_add(
                self.dependencies
                    .len()
                    .saturating_mul(std::mem::size_of::<EffectResourceDependency>()),
            )
            .saturating_add(dependencies)
            .saturating_add(self.zero_graph.retained_bytes_estimate())
            .saturating_add(
                self.zero_stage_bindings
                    .len()
                    .saturating_mul(std::mem::size_of::<CompiledEffectStageBinding>()),
            )
            .saturating_add(binding_nodes)
            .saturating_add(std::mem::size_of::<usize>().saturating_mul(8))
    }
}

fn validate_effect_schema(
    effect: &EffectNode,
    definition: &EffectDefinition,
) -> Result<(), EffectGraphBuildError> {
    effect
        .validate_author_state()
        .map_err(|error| EffectGraphBuildError::InvalidAuthorState {
            effect_key: effect.effect_type.key(),
            effect_id: effect.id,
            reason: error.to_string(),
        })?;

    let expected = definition
        .default_properties()
        .iter()
        .map(|(_, property)| {
            (
                property.descriptor.parameter_id().clone(),
                &property.descriptor.schema,
            )
        })
        .collect::<HashMap<_, _>>();
    let actual = effect
        .properties
        .iter()
        .map(|(_, property)| {
            (
                property.descriptor.parameter_id().clone(),
                &property.descriptor.schema,
            )
        })
        .collect::<HashMap<_, _>>();
    if expected.len() != actual.len() {
        return Err(EffectGraphBuildError::InvalidAuthorState {
            effect_key: effect.effect_type.key(),
            effect_id: effect.id,
            reason: format!(
                "definition declares {} parameters but the instance carries {}",
                expected.len(),
                actual.len()
            ),
        });
    }
    for (parameter_id, expected_schema) in expected {
        let Some(actual_schema) = actual.get(&parameter_id) else {
            return Err(EffectGraphBuildError::InvalidAuthorState {
                effect_key: effect.effect_type.key(),
                effect_id: effect.id,
                reason: format!("missing parameter schema `{parameter_id}`"),
            });
        };
        if *actual_schema != expected_schema {
            return Err(EffectGraphBuildError::InvalidAuthorState {
                effect_key: effect.effect_type.key(),
                effect_id: effect.id,
                reason: format!(
                    "parameter schema `{parameter_id}` differs from the bound definition"
                ),
            });
        }
    }
    Ok(())
}

fn evaluate_instances(
    instances: &[PreparedEffectInstance],
    working_color_space: WorkingColorSpace,
    time: TimelineTime,
) -> Result<EvaluatedEffectGraph, EffectGraphBuildError> {
    let mut builder = EffectGraphBuilderState::new();
    let mut stage_bindings = Vec::with_capacity(instances.len());
    for (stage_index, instance) in instances.iter().enumerate() {
        stage_bindings.push(evaluate_instance_into_builder(
            instance,
            stage_index,
            working_color_space,
            time,
            &mut builder,
        )?);
    }
    Ok(EvaluatedEffectGraph {
        graph: builder.finish(),
        stage_bindings: stage_bindings.into(),
    })
}

fn evaluate_instance_into_builder(
    instance: &PreparedEffectInstance,
    stage_index: usize,
    working_color_space: WorkingColorSpace,
    time: TimelineTime,
    builder: &mut EffectGraphBuilderState,
) -> Result<CompiledEffectStageBinding, EffectGraphBuildError> {
    let effect_key = instance.effect.effect_type.key();
    let input_value = builder.current_output();
    let mut staged = builder.clone();
    let checkpoint = staged.checkpoint();
    staged.set_active_domain_contract(instance.definition.color_domain_contract());
    let context = EffectEvalContext { time, working_color_space };
    match catch_unwind(AssertUnwindSafe(|| {
        (instance.evaluator.evaluator())(&instance.effect, context, &mut staged)
    })) {
        Ok(Ok(())) => {
            staged.bind_custom_runtime_owner_since(
                checkpoint,
                instance.definition.key(),
                instance.definition.definition_registry_revision(),
                instance.definition.plugin_contract(),
            );
            if !staged.satisfies_topology_since(
                checkpoint,
                instance.definition.execution_contract().topology,
            ) {
                return Err(EffectGraphBuildError::TopologyContractViolation {
                    effect_key,
                    effect_id: instance.effect.id,
                });
            }
            staged
                .validate_execution_contract_since(
                    checkpoint,
                    instance.definition.execution_contract(),
                )
                .map_err(
                    |violation| EffectGraphBuildError::ExecutionContractViolation {
                        effect_key: effect_key.clone(),
                        effect_id: instance.effect.id,
                        violation: Box::new(violation),
                    },
                )?;
            let binding = CompiledEffectStageBinding::new(
                stage_index,
                instance.definition.execution_contract(),
                input_value,
                staged.current_output(),
                staged.node_ids_since(checkpoint),
            );
            *builder = staged;
            Ok(binding)
        }
        Ok(Err(error)) => Err(error),
        Err(_) => {
            crate::plugin_contract::record_plugin_runtime_failure(
                instance.definition.key(),
                instance.definition.definition_registry_revision(),
                instance.definition.plugin_contract(),
                "effect graph builder panicked",
            );
            Err(EffectGraphBuildError::BuilderPanicked {
                effect_key,
                effect_id: instance.effect.id,
            })
        }
    }
}

#[derive(Debug)]
struct PreparedGradeGraphInner {
    author_graph: GradeGraph,
    stack: PreparedEffectStack,
    effect_instances: HashMap<EffectId, usize>,
    execution_envelope: EffectExecutionEnvelope,
    zero_evaluation: EvaluatedEffectGraph,
}

/// Definition-bound, resource-prepared grade authoring DAG.
///
/// Clones share the complete immutable preparation, allowing many Clip/group
/// assignments to reference one shared grade without multiplying LUTs,
/// evaluators, or zero-time topology state.
#[derive(Clone, Debug)]
pub struct PreparedGradeGraph {
    inner: Arc<PreparedGradeGraphInner>,
}

impl PreparedGradeGraph {
    /// Prepare one closed grade graph through an uncached LUT owner.
    pub fn prepare(
        graph: &GradeGraph,
        working_color_space: WorkingColorSpace,
    ) -> Result<Self, EffectGraphBuildError> {
        Self::prepare_with_lut_cache(graph, working_color_space, &LutPreparationCache::uncached())
    }

    /// Prepare one closed grade graph through the caller's bounded LUT cache.
    pub fn prepare_with_lut_cache(
        graph: &GradeGraph,
        working_color_space: WorkingColorSpace,
        lut_cache: &LutPreparationCache,
    ) -> Result<Self, EffectGraphBuildError> {
        graph.validate_author_state().map_err(|error| {
            EffectGraphBuildError::GradeGraphInvalidAuthorState {
                output_node_id: graph.output,
                reason: error.to_string(),
            }
        })?;
        let effects = graph
            .nodes
            .iter()
            .filter_map(|node| match &node.kind {
                GradeGraphNodeKind::Effect { effect, .. } => Some(effect.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let stack =
            PreparedEffectStack::prepare_with_lut_cache(&effects, working_color_space, lut_cache)?;
        let effect_instances = stack
            .instances
            .iter()
            .enumerate()
            .map(|(index, instance)| (instance.effect.id, index))
            .collect::<HashMap<_, _>>();
        let zero_evaluation = evaluate_grade_graph(
            graph,
            &stack,
            &effect_instances,
            working_color_space,
            TimelineTime::ZERO,
        )?;
        prepare_effect_graph_topology(&zero_evaluation.graph)
            .ok_or(EffectGraphBuildError::InvalidGraph)?;
        let execution_envelope = execution_envelope_for_bindings(&zero_evaluation.stage_bindings)?;
        Ok(Self {
            inner: Arc::new(PreparedGradeGraphInner {
                author_graph: graph.clone(),
                stack,
                effect_instances,
                execution_envelope,
                zero_evaluation,
            }),
        })
    }

    fn evaluate_with_stage_bindings(
        &self,
        time: TimelineTime,
    ) -> Result<EvaluatedEffectGraph, EffectGraphBuildError> {
        if time == TimelineTime::ZERO {
            return Ok(self.inner.zero_evaluation.clone());
        }
        evaluate_grade_graph(
            &self.inner.author_graph,
            &self.inner.stack,
            &self.inner.effect_instances,
            self.inner.stack.working_color_space,
            time,
        )
    }

    /// Definition-stage execution contract for this shared grade.
    pub fn execution_envelope(&self) -> &EffectExecutionEnvelope {
        &self.inner.execution_envelope
    }

    /// Process-local definition and immutable-resource cache identity.
    pub fn dependency_identity(&self) -> EffectProgramDependencyIdentity {
        dependency_identity(&self.inner.stack)
    }

    pub fn has_external_dependencies(&self) -> bool {
        self.inner.stack.has_external_dependencies()
    }

    pub fn dependencies_are_current(&self) -> Result<bool, EffectDependencyCheckError> {
        dependencies_are_current_for_stack(&self.inner.stack)
    }

    pub fn retained_bytes_estimate(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(std::mem::size_of::<PreparedGradeGraphInner>())
            .saturating_add(self.inner.stack.retained_bytes_estimate())
            .saturating_add(
                serde_json::to_vec(&self.inner.author_graph)
                    .map_or(4096, |bytes| bytes.len().max(4096)),
            )
            .saturating_add(self.inner.zero_evaluation.graph.retained_bytes_estimate())
    }
}

fn evaluate_grade_graph(
    graph: &GradeGraph,
    stack: &PreparedEffectStack,
    effect_instances: &HashMap<EffectId, usize>,
    working_color_space: WorkingColorSpace,
    time: TimelineTime,
) -> Result<EvaluatedEffectGraph, EffectGraphBuildError> {
    let nodes = graph.nodes.iter().map(|node| (node.id, node)).collect::<HashMap<_, _>>();
    let mut builder = EffectGraphBuilderState::new();
    let mut outputs = HashMap::with_capacity(nodes.len());
    let mut bindings = Vec::with_capacity(effect_instances.len());
    let output = evaluate_grade_node(
        graph.output,
        &nodes,
        stack,
        effect_instances,
        working_color_space,
        time,
        &mut builder,
        &mut outputs,
        &mut bindings,
    )?;
    builder.set_current_output(output);
    Ok(EvaluatedEffectGraph {
        graph: builder.finish(),
        stage_bindings: bindings.into(),
    })
}

#[allow(clippy::too_many_arguments)]
fn evaluate_grade_node(
    node_id: GradeGraphNodeId,
    nodes: &HashMap<GradeGraphNodeId, &mondrian_core::GradeGraphNode>,
    stack: &PreparedEffectStack,
    effect_instances: &HashMap<EffectId, usize>,
    working_color_space: WorkingColorSpace,
    time: TimelineTime,
    builder: &mut EffectGraphBuilderState,
    outputs: &mut HashMap<GradeGraphNodeId, EffectGraphNodeId>,
    bindings: &mut Vec<CompiledEffectStageBinding>,
) -> Result<EffectGraphNodeId, EffectGraphBuildError> {
    if let Some(output) = outputs.get(&node_id) {
        return Ok(*output);
    }
    let node = nodes.get(&node_id).ok_or(EffectGraphBuildError::InvalidGraph)?;
    let output = match &node.kind {
        GradeGraphNodeKind::Input => builder.source(),
        GradeGraphNodeKind::Effect { input, effect } => {
            let input = evaluate_grade_node(
                *input,
                nodes,
                stack,
                effect_instances,
                working_color_space,
                time,
                builder,
                outputs,
                bindings,
            )?;
            let Some(instance_index) = effect_instances.get(&effect.id).copied() else {
                outputs.insert(node_id, input);
                return Ok(input);
            };
            builder.set_current_output(input);
            let binding = evaluate_instance_into_builder(
                &stack.instances[instance_index],
                bindings.len(),
                working_color_space,
                time,
                builder,
            )?;
            let output = binding.output_value();
            bindings.push(binding);
            output
        }
        GradeGraphNodeKind::Parallel { inputs, blend_mode, opacity } => {
            let mut evaluated = Vec::with_capacity(inputs.len());
            for input in inputs {
                evaluated.push(evaluate_grade_node(
                    *input,
                    nodes,
                    stack,
                    effect_instances,
                    working_color_space,
                    time,
                    builder,
                    outputs,
                    bindings,
                )?);
            }
            let input = evaluated.first().copied().ok_or(EffectGraphBuildError::InvalidGraph)?;
            let output = builder.add_multi_input(evaluated, *blend_mode, *opacity);
            bindings.push(CompiledEffectStageBinding::new(
                bindings.len(),
                grade_graph_composite_contract(),
                input,
                output,
                [output],
            ));
            output
        }
        GradeGraphNodeKind::Layer { base, overlay, blend_mode, opacity } => {
            let base = evaluate_grade_node(
                *base,
                nodes,
                stack,
                effect_instances,
                working_color_space,
                time,
                builder,
                outputs,
                bindings,
            )?;
            let overlay = evaluate_grade_node(
                *overlay,
                nodes,
                stack,
                effect_instances,
                working_color_space,
                time,
                builder,
                outputs,
                bindings,
            )?;
            let output = builder.add_blend(base, overlay, *blend_mode, *opacity);
            bindings.push(CompiledEffectStageBinding::new(
                bindings.len(),
                grade_graph_composite_contract(),
                base,
                output,
                [output],
            ));
            output
        }
    };
    outputs.insert(node_id, output);
    Ok(output)
}

fn execution_envelope_for_bindings(
    bindings: &[CompiledEffectStageBinding],
) -> Result<EffectExecutionEnvelope, EffectGraphBuildError> {
    let mut aggregate = EffectExecutionContract::IDENTITY;
    let mut stages = Vec::with_capacity(bindings.len());
    for binding in bindings {
        let contract = binding.contract();
        aggregate = aggregate.compose(contract).map_err(|error| {
            EffectGraphBuildError::InvalidExecutionContract { reason: error.to_string() }
        })?;
        stages.push(contract);
    }
    Ok(EffectExecutionEnvelope::new(aggregate, stages))
}

fn grade_graph_composite_contract() -> EffectExecutionContract {
    EffectExecutionContract {
        execution_modes: EffectExecutionModes::CPU_U8
            .union(EffectExecutionModes::CPU_F32)
            .union(EffectExecutionModes::GPU_F32),
        determinism: EffectDeterminism::Deterministic,
        state_model: EffectStateModel::Stateless,
        temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
        roi_propagation: EffectRoiPropagation::PixelLocal,
        resource_lifetime: EffectResourceLifetime::Frame,
        topology: EffectGraphTopology::GeneralDag,
    }
}

/// Cache identity beyond Sequence revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectProgramDependencyIdentity {
    /// Process-local definition/plugin registry revision.
    pub definition_registry_revision: u64,
    /// Stable digest of every retained immutable resource identity.
    pub resource_fingerprint: [u8; 32],
}

#[derive(Debug)]
struct PreparedEffectProgramInner {
    stack: PreparedEffectStack,
    masks: Arc<[MaskComponent]>,
    grade_before: Arc<[PreparedGradeGraph]>,
    grade_after: Arc<[PreparedGradeGraph]>,
    execution_envelope: EffectExecutionEnvelope,
    dependency_identity: EffectProgramDependencyIdentity,
    zero_topology: Arc<PreparedEffectGraphTopology>,
    zero_compiled: Arc<CompiledEffectGraph>,
}

/// Resource-bound effect and mask program shared by Preview and Export.
#[derive(Clone, Debug)]
pub struct PreparedEffectProgram {
    inner: Arc<PreparedEffectProgramInner>,
}

impl PreparedEffectProgram {
    /// Prepare one Clip-owned effect/mask program.
    pub fn prepare(
        effects: &[EffectNode],
        masks: &[MaskComponent],
        working_color_space: WorkingColorSpace,
    ) -> Result<Self, EffectGraphBuildError> {
        let lut_cache = LutPreparationCache::uncached();
        Self::prepare_with_lut_cache(effects, masks, working_color_space, &lut_cache)
    }

    /// Prepare one Clip-owned effect/mask program through a caller-owned,
    /// owner-scoped LUT preparation cache.
    pub fn prepare_with_lut_cache(
        effects: &[EffectNode],
        masks: &[MaskComponent],
        working_color_space: WorkingColorSpace,
        lut_cache: &LutPreparationCache,
    ) -> Result<Self, EffectGraphBuildError> {
        Self::prepare_hierarchical_with_lut_cache(
            effects,
            masks,
            &[],
            &[],
            working_color_space,
            lut_cache,
        )
    }

    /// Prepare the canonical hierarchy around one Clip's legacy stack.
    ///
    /// `grade_before` contains group pre-Clip grades. `grade_after` contains
    /// the Clip grade followed by group post-Clip grades. Each prepared grade
    /// may be shared by arbitrarily many assignments.
    pub fn prepare_hierarchical_with_lut_cache(
        effects: &[EffectNode],
        masks: &[MaskComponent],
        grade_before: &[PreparedGradeGraph],
        grade_after: &[PreparedGradeGraph],
        working_color_space: WorkingColorSpace,
        lut_cache: &LutPreparationCache,
    ) -> Result<Self, EffectGraphBuildError> {
        let stack =
            PreparedEffectStack::prepare_with_lut_cache(effects, working_color_space, lut_cache)?;
        let masks = masks.iter().filter(|mask| mask.enabled).cloned().collect::<Vec<_>>();
        let grade_before: Arc<[PreparedGradeGraph]> = grade_before.to_vec().into();
        let grade_after: Arc<[PreparedGradeGraph]> = grade_after.to_vec().into();
        let zero_evaluation = evaluate_effect_pipeline(
            &grade_before,
            &stack,
            &masks,
            &grade_after,
            TimelineTime::ZERO,
        );
        let zero_evaluation = zero_evaluation?;
        let zero_graph = zero_evaluation.graph;
        let zero_stage_bindings = zero_evaluation.stage_bindings;
        let execution_envelope = execution_envelope_for_bindings(&zero_stage_bindings)?;
        if zero_graph.is_identity() {
            let topology = Arc::new(
                prepare_effect_graph_topology(&zero_graph)
                    .ok_or(EffectGraphBuildError::InvalidGraph)?,
            );
            let zero_compiled = if execution_envelope.stages().is_empty() {
                identity_compiled_effect_graph().ok_or(EffectGraphBuildError::InvalidGraph)?
            } else {
                topology
                    .bind_with_execution_bindings(
                        zero_graph,
                        execution_envelope.clone(),
                        zero_stage_bindings,
                    )
                    .ok_or(EffectGraphBuildError::InvalidGraph)?
            };
            let dependency_identity =
                hierarchical_dependency_identity(&stack, &grade_before, &grade_after);
            return Ok(Self {
                inner: Arc::new(PreparedEffectProgramInner {
                    stack,
                    masks: masks.into(),
                    grade_before,
                    grade_after,
                    execution_envelope,
                    dependency_identity,
                    zero_topology: topology,
                    zero_compiled,
                }),
            });
        }

        let topology = Arc::new(
            prepare_effect_graph_topology(&zero_graph)
                .ok_or(EffectGraphBuildError::InvalidGraph)?,
        );
        let zero_compiled = topology
            .bind_with_execution_bindings(
                zero_graph,
                execution_envelope.clone(),
                zero_stage_bindings,
            )
            .ok_or(EffectGraphBuildError::InvalidGraph)?;
        let dependency_identity =
            hierarchical_dependency_identity(&stack, &grade_before, &grade_after);
        Ok(Self {
            inner: Arc::new(PreparedEffectProgramInner {
                stack,
                masks: masks.into(),
                grade_before,
                grade_after,
                execution_envelope,
                dependency_identity,
                zero_topology: topology,
                zero_compiled,
            }),
        })
    }

    /// Bind current animated values through the immutable zero-time topology
    /// or an uncached reference compilation.
    ///
    /// Production owners should use [`Self::evaluate_with_session`] so valid
    /// dynamic topology variants can reuse owner-scoped bounded residency.
    pub fn evaluate(
        &self,
        time: TimelineTime,
    ) -> Result<Arc<CompiledEffectGraph>, EffectGraphBuildError> {
        if time == TimelineTime::ZERO {
            return Ok(Arc::clone(&self.inner.zero_compiled));
        }
        let evaluation = evaluate_effect_pipeline(
            &self.inner.grade_before,
            &self.inner.stack,
            &self.inner.masks,
            &self.inner.grade_after,
            time,
        )?;
        let graph = evaluation.graph;
        if graph.is_identity() && self.inner.execution_envelope.stages().is_empty() {
            return identity_compiled_effect_graph().ok_or(EffectGraphBuildError::InvalidGraph);
        }
        let topology = if self.inner.zero_topology.matches(&graph) {
            Arc::clone(&self.inner.zero_topology)
        } else {
            Arc::new(
                prepare_effect_graph_topology(&graph).ok_or(EffectGraphBuildError::InvalidGraph)?,
            )
        };
        topology
            .bind_with_execution_bindings(
                graph,
                self.inner.execution_envelope.clone(),
                evaluation.stage_bindings,
            )
            .ok_or(EffectGraphBuildError::InvalidGraph)
    }

    /// Bind current animated values with dynamic-topology reuse owned by one
    /// Preview or Export Effect execution Session.
    ///
    /// The Prepared Program remains immutable. A topology that exceeds the
    /// Session's entry or byte grant is still used for this evaluation but is
    /// not retained for a later call.
    pub fn evaluate_with_session(
        &self,
        time: TimelineTime,
        session: &mut EffectExecutionSession,
    ) -> Result<Arc<CompiledEffectGraph>, EffectGraphBuildError> {
        if time == TimelineTime::ZERO {
            return Ok(Arc::clone(&self.inner.zero_compiled));
        }
        let evaluation = evaluate_effect_pipeline(
            &self.inner.grade_before,
            &self.inner.stack,
            &self.inner.masks,
            &self.inner.grade_after,
            time,
        )?;
        let graph = evaluation.graph;
        if graph.is_identity() && self.inner.execution_envelope.stages().is_empty() {
            return identity_compiled_effect_graph().ok_or(EffectGraphBuildError::InvalidGraph);
        }
        let (topology, retain_after_binding) = if self.inner.zero_topology.matches(&graph) {
            (Arc::clone(&self.inner.zero_topology), false)
        } else if let Some(topology) = session.get_effect_topology(&graph) {
            (topology, false)
        } else {
            (
                Arc::new(
                    prepare_effect_graph_topology(&graph)
                        .ok_or(EffectGraphBuildError::InvalidGraph)?,
                ),
                true,
            )
        };
        let compiled = topology
            .bind_with_execution_bindings(
                graph,
                self.inner.execution_envelope.clone(),
                evaluation.stage_bindings,
            )
            .ok_or(EffectGraphBuildError::InvalidGraph)?;
        if retain_after_binding {
            session.retain_effect_topology(topology);
        }
        Ok(compiled)
    }

    /// Aggregated execution contract for the effect stack and masks.
    pub fn execution_contract(&self) -> EffectExecutionContract {
        self.inner.execution_envelope.aggregate()
    }

    /// Ordered per-stage contracts and homogeneous-backend evidence.
    pub fn execution_envelope(&self) -> &EffectExecutionEnvelope {
        &self.inner.execution_envelope
    }

    /// Cache identity that must accompany the owning Sequence revision.
    pub fn dependency_identity(&self) -> EffectProgramDependencyIdentity {
        self.inner.dependency_identity
    }

    /// Whether cross-revision reuse requires an explicit external-dependency
    /// observation.
    pub fn has_external_dependencies(&self) -> bool {
        self.inner.stack.has_external_dependencies()
            || self
                .inner
                .grade_before
                .iter()
                .chain(self.inner.grade_after.iter())
                .any(PreparedGradeGraph::has_external_dependencies)
    }

    /// Conservative logical bytes retained by this Prepared Program.
    ///
    /// The charge includes author snapshots, bound definitions/evaluators and
    /// their declared immutable resources, masks, execution evidence, the
    /// immutable zero-time topology, and its compiled graph. Dynamic topology
    /// variants belong to an `EffectExecutionSession` and cannot change this
    /// charge. Shared `Arc` payloads may be charged more than once so one cache
    /// owner never relies on another owner's residency. This is not allocator,
    /// RSS, or GPU-memory evidence.
    pub fn retained_bytes_estimate(&self) -> usize {
        let masks = self
            .inner
            .masks
            .iter()
            .map(|mask| {
                serde_json::to_vec(mask).map_or(
                    std::mem::size_of::<MaskComponent>().saturating_add(1024),
                    |bytes| bytes.len().max(1024),
                )
            })
            .fold(0_usize, usize::saturating_add);
        std::mem::size_of::<Self>()
            .saturating_add(std::mem::size_of::<PreparedEffectProgramInner>())
            .saturating_add(self.inner.stack.retained_bytes_estimate())
            .saturating_add(
                self.inner
                    .grade_before
                    .iter()
                    .chain(self.inner.grade_after.iter())
                    .map(PreparedGradeGraph::retained_bytes_estimate)
                    .fold(0_usize, usize::saturating_add),
            )
            .saturating_add(
                self.inner.masks.len().saturating_mul(std::mem::size_of::<MaskComponent>()),
            )
            .saturating_add(masks)
            .saturating_add(
                self.inner
                    .execution_envelope
                    .stages()
                    .len()
                    .saturating_mul(std::mem::size_of::<EffectExecutionContract>()),
            )
            .saturating_add(self.inner.zero_topology.retained_bytes_estimate())
            .saturating_add(self.inner.zero_compiled.retained_bytes_estimate())
            .saturating_add(std::mem::size_of::<usize>().saturating_mul(8))
    }

    /// Revalidate process-local plugin definitions and external resources.
    ///
    /// This is an explicit low-frequency cache-admission/invalidation check. It
    /// may read and parse external files and must never run in per-frame
    /// evaluation or frame-cache lookup.
    ///
    /// Plugin-managed resources remain current only while their definition
    /// registry revision is unchanged; plugins must re-register when the
    /// identity they supplied during preparation changes.
    pub fn dependencies_are_current(&self) -> Result<bool, EffectDependencyCheckError> {
        if !dependencies_are_current_for_stack(&self.inner.stack)? {
            return Ok(false);
        }
        for grade in self.inner.grade_before.iter().chain(self.inner.grade_after.iter()) {
            if !grade.dependencies_are_current()? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Cheap process-local definition/plugin revision check.
    ///
    /// This does not read external resources and can reject a cached program
    /// before scheduling low-frequency dependency revalidation.
    pub fn definition_revision_is_current(&self) -> bool {
        effect_registry_revision() == self.inner.dependency_identity.definition_registry_revision
    }
}

/// Failure while revalidating a prepared program dependency.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EffectDependencyCheckError {
    /// A previously prepared external resource can no longer be read.
    #[error("cannot revalidate effect dependency `{path}`: {reason}")]
    Unreadable {
        /// Dependency path.
        path: PathBuf,
        /// Structured adapter error rendered for diagnostics.
        reason: String,
    },
}

fn dependency_identity(stack: &PreparedEffectStack) -> EffectProgramDependencyIdentity {
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.prepared-effect-dependencies.v1");
    hasher.update(stack.definition_registry_revision().to_le_bytes());
    for dependency in stack.dependencies() {
        match dependency {
            EffectResourceDependency::CubeLut { path, semantic_fingerprint } => {
                hasher.update([0]);
                let path = path.to_string_lossy();
                hasher.update((path.len() as u64).to_le_bytes());
                hasher.update(path.as_bytes());
                hasher.update(semantic_fingerprint);
            }
            EffectResourceDependency::PluginManaged { identity } => {
                hasher.update([1]);
                hasher.update((identity.len() as u64).to_le_bytes());
                hasher.update(identity.as_bytes());
            }
        }
    }
    EffectProgramDependencyIdentity {
        definition_registry_revision: stack.definition_registry_revision(),
        resource_fingerprint: hasher.finalize().into(),
    }
}

fn hierarchical_dependency_identity(
    stack: &PreparedEffectStack,
    before: &[PreparedGradeGraph],
    after: &[PreparedGradeGraph],
) -> EffectProgramDependencyIdentity {
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.hierarchical-effect-dependencies.v1");
    let legacy = dependency_identity(stack);
    hasher.update(legacy.resource_fingerprint);
    for grade in before.iter().chain(after) {
        hasher.update(grade.dependency_identity().resource_fingerprint);
    }
    EffectProgramDependencyIdentity {
        definition_registry_revision: stack.definition_registry_revision(),
        resource_fingerprint: hasher.finalize().into(),
    }
}

fn evaluate_effect_pipeline(
    before: &[PreparedGradeGraph],
    stack: &PreparedEffectStack,
    masks: &[MaskComponent],
    after: &[PreparedGradeGraph],
    time: TimelineTime,
) -> Result<EvaluatedEffectGraph, EffectGraphBuildError> {
    let mut combined = EvaluatedEffectGraph {
        graph: EffectRenderGraph::identity(),
        stage_bindings: Arc::from([]),
    };
    for grade in before {
        let evaluation = grade.evaluate_with_stage_bindings(time)?;
        append_evaluated_graph(&mut combined, evaluation)?;
    }
    append_evaluated_graph(
        &mut combined,
        inject_masks(stack.evaluate_with_stage_bindings(time)?, masks, time),
    )?;
    for grade in after {
        let evaluation = grade.evaluate_with_stage_bindings(time)?;
        append_evaluated_graph(&mut combined, evaluation)?;
    }
    Ok(combined)
}

fn append_evaluated_graph(
    combined: &mut EvaluatedEffectGraph,
    evaluation: EvaluatedEffectGraph,
) -> Result<(), EffectGraphBuildError> {
    let current = combined.graph.output.unwrap_or(EffectGraphNodeId(0));
    let mut ids = HashMap::with_capacity(evaluation.graph.nodes.len());
    ids.insert(EffectGraphNodeId(0), current);
    let mut next_id = u32::try_from(combined.graph.nodes.len())
        .map_err(|_| EffectGraphBuildError::InvalidGraph)?;
    for node in evaluation.graph.nodes.iter().skip(1) {
        let id = EffectGraphNodeId(next_id);
        next_id = next_id.checked_add(1).ok_or(EffectGraphBuildError::InvalidGraph)?;
        ids.insert(node.id, id);
        combined
            .graph
            .nodes
            .push(EffectGraphNode { id, kind: remap_graph_node_kind(&node.kind, &ids)? });
    }
    combined.graph.output = Some(remap_graph_id(
        evaluation.graph.output.unwrap_or(EffectGraphNodeId(0)),
        &ids,
    )?);
    let mut bindings = combined.stage_bindings.to_vec();
    for binding in evaluation.stage_bindings.iter() {
        bindings.push(CompiledEffectStageBinding::new(
            bindings.len(),
            binding.contract(),
            remap_graph_id(binding.input_value(), &ids)?,
            remap_graph_id(binding.output_value(), &ids)?,
            binding
                .emitted_nodes()
                .iter()
                .map(|id| remap_graph_id(*id, &ids))
                .collect::<Result<Vec<_>, _>>()?,
        ));
    }
    combined.stage_bindings = bindings.into();
    Ok(())
}

fn remap_graph_node_kind(
    kind: &EffectGraphNodeKind,
    ids: &HashMap<EffectGraphNodeId, EffectGraphNodeId>,
) -> Result<EffectGraphNodeKind, EffectGraphBuildError> {
    let remap = |id| remap_graph_id(id, ids);
    Ok(match kind {
        EffectGraphNodeKind::Source => return Err(EffectGraphBuildError::InvalidGraph),
        EffectGraphNodeKind::UnaryEffect { input, op } => {
            EffectGraphNodeKind::UnaryEffect { input: remap(*input)?, op: op.clone() }
        }
        EffectGraphNodeKind::DomainEffect { input, op, domain_contract } => {
            EffectGraphNodeKind::DomainEffect {
                input: remap(*input)?,
                op: op.clone(),
                domain_contract: *domain_contract,
            }
        }
        EffectGraphNodeKind::Blend { base, overlay, blend_mode, opacity } => {
            EffectGraphNodeKind::Blend {
                base: remap(*base)?,
                overlay: remap(*overlay)?,
                blend_mode: *blend_mode,
                opacity: *opacity,
            }
        }
        EffectGraphNodeKind::Mask { input, mask, invert, mask_op } => EffectGraphNodeKind::Mask {
            input: remap(*input)?,
            mask: remap(*mask)?,
            invert: *invert,
            mask_op: *mask_op,
        },
        EffectGraphNodeKind::MaskSource { shape, feather, expansion, opacity, invert } => {
            EffectGraphNodeKind::MaskSource {
                shape: shape.clone(),
                feather: *feather,
                expansion: *expansion,
                opacity: *opacity,
                invert: *invert,
            }
        }
        EffectGraphNodeKind::MaskCombine { left, right, mask_op } => {
            EffectGraphNodeKind::MaskCombine {
                left: remap(*left)?,
                right: remap(*right)?,
                mask_op: *mask_op,
            }
        }
        EffectGraphNodeKind::MatteMix { base, graded, matte } => EffectGraphNodeKind::MatteMix {
            base: remap(*base)?,
            graded: remap(*graded)?,
            matte: remap(*matte)?,
        },
        EffectGraphNodeKind::MultiInput { inputs, blend_mode, opacity } => {
            EffectGraphNodeKind::MultiInput {
                inputs: inputs.iter().map(|id| remap(*id)).collect::<Result<Vec<_>, _>>()?,
                blend_mode: *blend_mode,
                opacity: *opacity,
            }
        }
    })
}

fn remap_graph_id(
    id: EffectGraphNodeId,
    ids: &HashMap<EffectGraphNodeId, EffectGraphNodeId>,
) -> Result<EffectGraphNodeId, EffectGraphBuildError> {
    ids.get(&id).copied().ok_or(EffectGraphBuildError::InvalidGraph)
}

fn dependencies_are_current_for_stack(
    stack: &PreparedEffectStack,
) -> Result<bool, EffectDependencyCheckError> {
    if effect_registry_revision() != stack.definition_registry_revision() {
        return Ok(false);
    }
    for dependency in stack.dependencies() {
        match dependency {
            EffectResourceDependency::CubeLut { path, semantic_fingerprint } => {
                let current = crate::Lut3D::from_cube_file(path).map_err(|error| {
                    EffectDependencyCheckError::Unreadable {
                        path: path.clone(),
                        reason: error.to_string(),
                    }
                })?;
                if PreparedLut3D::new(current).semantic_fingerprint() != semantic_fingerprint {
                    return Ok(false);
                }
            }
            EffectResourceDependency::PluginManaged { .. } => {}
        }
    }
    Ok(true)
}

fn inject_masks(
    mut evaluation: EvaluatedEffectGraph,
    masks: &[MaskComponent],
    time: TimelineTime,
) -> EvaluatedEffectGraph {
    let graph = &mut evaluation.graph;
    let base = EffectGraphNodeId(0);
    let graded = graph.output.unwrap_or(base);
    let stage_input = graded;
    let mut emitted_nodes = Vec::with_capacity(masks.len().saturating_mul(2));
    let mut matte = None;
    let mut next_id = graph.nodes.len() as u32;
    for mask in masks {
        let params = mask.evaluate_at(time);
        let source_id = EffectGraphNodeId(next_id);
        next_id += 1;
        graph.nodes.push(EffectGraphNode {
            id: source_id,
            kind: EffectGraphNodeKind::MaskSource {
                shape: params.shape,
                feather: params.feather,
                expansion: params.expansion,
                opacity: params.opacity,
                invert: params.invert,
            },
        });
        emitted_nodes.push(source_id);
        matte = Some(match matte {
            None => source_id,
            Some(left) => {
                let combine_id = EffectGraphNodeId(next_id);
                next_id += 1;
                graph.nodes.push(EffectGraphNode {
                    id: combine_id,
                    kind: EffectGraphNodeKind::MaskCombine {
                        left,
                        right: source_id,
                        mask_op: params.mask_op,
                    },
                });
                emitted_nodes.push(combine_id);
                combine_id
            }
        });
    }
    if let Some(matte) = matte {
        let mix_id = EffectGraphNodeId(next_id);
        graph.nodes.push(EffectGraphNode {
            id: mix_id,
            kind: EffectGraphNodeKind::MatteMix { base, graded, matte },
        });
        emitted_nodes.push(mix_id);
        graph.output = Some(mix_id);
    }
    if !masks.is_empty() {
        let mut stage_bindings = evaluation.stage_bindings.to_vec();
        stage_bindings.push(CompiledEffectStageBinding::new(
            stage_bindings.len(),
            mask_execution_contract(),
            stage_input,
            graph.output.unwrap_or(stage_input),
            emitted_nodes,
        ));
        evaluation.stage_bindings = stage_bindings.into();
    }
    evaluation
}

fn mask_execution_contract() -> EffectExecutionContract {
    EffectExecutionContract {
        execution_modes: EffectExecutionModes::CPU_U8
            .union(EffectExecutionModes::CPU_F32)
            .union(EffectExecutionModes::GPU_F32),
        determinism: EffectDeterminism::Deterministic,
        state_model: EffectStateModel::Stateless,
        temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
        roi_propagation: EffectRoiPropagation::PixelLocal,
        resource_lifetime: EffectResourceLifetime::Frame,
        topology: EffectGraphTopology::GeneralDag,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        register_effect_definition, EffectColorDomainContract, EffectDefinition,
        EffectGraphBuilder, EffectGraphPreparer, EffectNodeExt, EffectRenderOp, MaskEvaluation,
        MaskShape,
    };
    use mondrian_core::{
        automation::{Keyframe, ParameterResourceReference, PropertyValue},
        effect_data::EffectType,
        types::BlendMode,
    };
    use std::{
        path::{Path, PathBuf},
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    const IDENTITY_CUBE_2: &str = "LUT_3D_SIZE 2
0 0 0
1 0 0
0 1 0
1 1 0
0 0 1
1 0 1
0 1 1
1 1 1
";

    fn tt(value: i64) -> TimelineTime {
        TimelineTime::new(value, 1).expect("test time")
    }

    fn temporary_cube(prefix: &str) -> PathBuf {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{unique}.cube"));
        std::fs::write(&path, IDENTITY_CUBE_2).expect("write cube");
        path
    }

    fn bound_lut_effect(path: &Path) -> EffectNode {
        let mut effect = EffectNode::with_defaults(EffectType::Lut3D);
        let processing_space_id = EffectType::Lut3D
            .parameter_id("processing_space")
            .expect("processing-space parameter ID");
        let path_id = EffectType::Lut3D.parameter_id("path").expect("path parameter ID");
        effect
            .set_static_value_by_parameter(
                &processing_space_id,
                PropertyValue::Enum("scene_linear".to_owned()),
            )
            .expect("bind processing space");
        effect
            .set_static_value_by_parameter(
                &path_id,
                PropertyValue::Resource(ParameterResourceReference::ExternalFile {
                    path: path.to_path_buf(),
                }),
            )
            .expect("bind LUT path");
        effect
    }

    fn prepared_lut_from_program(program: &PreparedEffectProgram) -> Arc<PreparedLut3D> {
        let compiled = program.evaluate(TimelineTime::ZERO).expect("evaluate prepared LUT program");
        compiled
            .graph()
            .nodes
            .iter()
            .find_map(|node| match &node.kind {
                EffectGraphNodeKind::UnaryEffect {
                    op: EffectRenderOp::Lut3D { lut, .. }, ..
                }
                | EffectGraphNodeKind::DomainEffect {
                    op: EffectRenderOp::Lut3D { lut, .. }, ..
                } => Some(Arc::clone(lut)),
                _ => None,
            })
            .expect("prepared LUT operation")
    }

    fn cpu_linear_contract() -> EffectExecutionContract {
        EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: EffectDeterminism::Deterministic,
            state_model: EffectStateModel::Stateless,
            temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
            roi_propagation: EffectRoiPropagation::PixelLocal,
            resource_lifetime: EffectResourceLifetime::Frame,
            topology: EffectGraphTopology::LinearChain,
        }
    }

    fn dynamic_topology_program(key: &str) -> PreparedEffectProgram {
        let effect_type = EffectType::Plugin(key.to_owned());
        register_effect_definition(
            EffectDefinition::new(
                effect_type.key(),
                "Dynamic Topology",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(cpu_linear_contract())
            .with_graph_builder(Arc::new(|_, context, graph| {
                if context.time != TimelineTime::ZERO {
                    graph.append_unary(EffectRenderOp::Vignette { intensity: 0.25, feather: 0.75 });
                }
                Ok(())
            })),
        )
        .expect("register dynamic topology definition");
        PreparedEffectProgram::prepare(
            &[EffectNode::new(effect_type)],
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .expect("prepare dynamic topology program")
    }

    fn dynamic_temporal_topology_program(key: &str) -> PreparedEffectProgram {
        let dynamic_effect_type = EffectType::Plugin(key.to_owned());
        let temporal_effect_type = EffectType::Plugin(format!("{key}.temporal"));
        let offset = TimelineTime::new(1, 2).expect("temporal offset");
        register_effect_definition(
            EffectDefinition::new(
                dynamic_effect_type.key(),
                "Dynamic Temporal Topology",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(cpu_linear_contract())
            .with_graph_builder(Arc::new(|_, context, graph| {
                if context.time != TimelineTime::ZERO {
                    graph.append_unary(EffectRenderOp::Vignette { intensity: 0.25, feather: 0.75 });
                }
                Ok(())
            })),
        )
        .expect("register dynamic topology definition");
        register_effect_definition(
            EffectDefinition::new(
                temporal_effect_type.key(),
                "Temporal Topology Consumer",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                temporal_input: EffectTemporalInputExtent {
                    past: crate::EffectTemporalSpan::Finite(offset),
                    future: crate::EffectTemporalSpan::None,
                },
                ..cpu_linear_contract()
            })
            .with_graph_builder(Arc::new(move |_, _, graph| {
                graph.append_unary(EffectRenderOp::TemporalFrameBlend {
                    sample_offset: TimelineTime::ZERO.checked_sub(offset).expect("past offset"),
                    mix: 0.25,
                });
                Ok(())
            })),
        )
        .expect("register temporal topology definition");
        PreparedEffectProgram::prepare(
            &[
                EffectNode::new(dynamic_effect_type),
                EffectNode::new(temporal_effect_type),
            ],
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .expect("prepare dynamic temporal topology program")
    }

    fn topology_session_config(
        max_cache_entries: usize,
        max_cache_bytes: usize,
    ) -> crate::EffectExecutionSessionConfig {
        crate::EffectExecutionSessionConfig {
            max_cache_entries,
            max_cache_bytes,
            max_working_bytes: 1024 * 1024,
            max_gpu_plan_entries: 0,
            max_gpu_plan_bytes: 0,
        }
    }

    #[test]
    fn preparation_rejects_missing_definition() {
        let effect = EffectNode::new(EffectType::Plugin("plugin.prepared.missing".to_owned()));
        assert!(matches!(
            PreparedEffectProgram::prepare(&[effect], &[], WorkingColorSpace::LinearRec709),
            Err(EffectGraphBuildError::DefinitionUnavailable { .. })
        ));
    }

    #[test]
    fn prepared_mask_contract_preserves_exact_partial_roi() {
        let mask = MaskComponent::new(
            "subject".to_owned(),
            MaskEvaluation {
                shape: MaskShape::Rectangle {
                    x: 0.2,
                    y: 0.25,
                    width: 0.5,
                    height: 0.4,
                    corner_radius: 0.05,
                },
                feather: 7.0,
                ..MaskEvaluation::default()
            },
        );
        let program = PreparedEffectProgram::prepare(&[], &[mask], WorkingColorSpace::LinearRec709)
            .expect("prepare Mask program");
        let graph = program.evaluate(tt(3)).expect("evaluate Mask graph");
        let output_roi = crate::EffectPixelRoi::new(17, 9, 23, 11);
        let demand = graph
            .plan_execution_demand(tt(3), crate::EffectFrameExtent::new(1920, 1080), output_roi)
            .expect("plan exact Mask demand");

        assert_eq!(
            graph.execution_envelope().aggregate().roi_propagation,
            EffectRoiPropagation::PixelLocal
        );
        assert_eq!(demand.input_roi().region(), output_roi);
    }

    fn exposure_effect(stops: f32) -> EffectNode {
        let mut effect = EffectNode::with_defaults(EffectType::BasicCorrection);
        let exposure_id = EffectType::BasicCorrection
            .parameter_id("exposure")
            .expect("exposure parameter ID");
        effect
            .set_static_value_by_parameter(&exposure_id, PropertyValue::Float(stops))
            .expect("set exposure");
        effect
    }

    fn serial_grade(effect: EffectNode) -> GradeGraph {
        let input = GradeGraphNodeId::new();
        let output = GradeGraphNodeId::new();
        GradeGraph {
            nodes: mondrian_core::AuthoringList::from([
                mondrian_core::GradeGraphNode { id: input, kind: GradeGraphNodeKind::Input },
                mondrian_core::GradeGraphNode {
                    id: output,
                    kind: GradeGraphNodeKind::Effect { input, effect },
                },
            ]),
            output,
        }
    }

    #[test]
    fn prepared_grade_lowers_parallel_and_layer_to_shared_execution_ir() {
        let input = GradeGraphNodeId::new();
        let left = GradeGraphNodeId::new();
        let right = GradeGraphNodeId::new();
        let parallel = GradeGraphNodeId::new();
        let output = GradeGraphNodeId::new();
        let graph = GradeGraph {
            nodes: mondrian_core::AuthoringList::from([
                mondrian_core::GradeGraphNode { id: input, kind: GradeGraphNodeKind::Input },
                mondrian_core::GradeGraphNode {
                    id: left,
                    kind: GradeGraphNodeKind::Effect { input, effect: exposure_effect(1.0) },
                },
                mondrian_core::GradeGraphNode {
                    id: right,
                    kind: GradeGraphNodeKind::Effect { input, effect: exposure_effect(-1.0) },
                },
                mondrian_core::GradeGraphNode {
                    id: parallel,
                    kind: GradeGraphNodeKind::Parallel {
                        inputs: mondrian_core::AuthoringList::from([left, right]),
                        blend_mode: BlendMode::Normal,
                        opacity: 0.75,
                    },
                },
                mondrian_core::GradeGraphNode {
                    id: output,
                    kind: GradeGraphNodeKind::Layer {
                        base: input,
                        overlay: parallel,
                        blend_mode: BlendMode::Normal,
                        opacity: 0.5,
                    },
                },
            ]),
            output,
        };
        let grade = PreparedGradeGraph::prepare(&graph, WorkingColorSpace::LinearRec709)
            .expect("prepare grade DAG");
        let program = PreparedEffectProgram::prepare_hierarchical_with_lut_cache(
            &[],
            &[],
            &[grade],
            &[],
            WorkingColorSpace::LinearRec709,
            &LutPreparationCache::uncached(),
        )
        .expect("prepare hierarchical program");
        let compiled = program.evaluate(TimelineTime::ZERO).expect("compile grade DAG");

        assert!(compiled
            .graph()
            .nodes
            .iter()
            .any(|node| matches!(&node.kind, EffectGraphNodeKind::MultiInput { .. })));
        assert!(compiled
            .graph()
            .nodes
            .iter()
            .any(|node| matches!(&node.kind, EffectGraphNodeKind::Blend { .. })));
        assert_eq!(compiled.stage_bindings().len(), 4);
    }

    #[test]
    fn hierarchical_program_orders_group_pre_clip_stack_and_clip_group_post() {
        let before = PreparedGradeGraph::prepare(
            &serial_grade(exposure_effect(1.0)),
            WorkingColorSpace::LinearRec709,
        )
        .expect("prepare before grade");
        let after = PreparedGradeGraph::prepare(
            &serial_grade(exposure_effect(-1.0)),
            WorkingColorSpace::LinearRec709,
        )
        .expect("prepare after grade");
        let program = PreparedEffectProgram::prepare_hierarchical_with_lut_cache(
            &[exposure_effect(0.5)],
            &[],
            &[before],
            &[after],
            WorkingColorSpace::LinearRec709,
            &LutPreparationCache::uncached(),
        )
        .expect("prepare hierarchy");
        let compiled = program.evaluate(TimelineTime::ZERO).expect("compile hierarchy");
        let bindings = compiled.stage_bindings();

        assert_eq!(bindings.len(), 3);
        assert_eq!(bindings[0].stage_index(), 0);
        assert_eq!(bindings[1].stage_index(), 1);
        assert_eq!(bindings[2].stage_index(), 2);
        assert_eq!(bindings[1].input_value(), bindings[0].output_value());
        assert_eq!(bindings[2].input_value(), bindings[1].output_value());
        assert_eq!(compiled.graph().output, Some(bindings[2].output_value()));
    }

    #[test]
    fn prepared_grade_clones_share_resources_and_animated_evaluation_changes_only_frame_graph() {
        let mut effect = EffectNode::with_defaults(EffectType::BasicCorrection);
        let exposure_id = EffectType::BasicCorrection
            .parameter_id("exposure")
            .expect("exposure parameter ID");
        let path = effect
            .properties
            .iter()
            .find(|(_, property)| property.descriptor.parameter_id() == &exposure_id)
            .map(|(_, property)| property.descriptor.path.clone())
            .expect("exposure property");
        effect
            .properties
            .set_keyframe(&path, Keyframe::linear(tt(0), PropertyValue::Float(0.0)))
            .expect("first key");
        effect
            .properties
            .set_keyframe(&path, Keyframe::linear(tt(10), PropertyValue::Float(2.0)))
            .expect("second key");
        let prepared =
            PreparedGradeGraph::prepare(&serial_grade(effect), WorkingColorSpace::LinearRec709)
                .expect("prepare animated grade");
        let shared = prepared.clone();
        assert!(Arc::ptr_eq(&prepared.inner, &shared.inner));

        let program = PreparedEffectProgram::prepare_hierarchical_with_lut_cache(
            &[],
            &[],
            &[prepared],
            &[],
            WorkingColorSpace::LinearRec709,
            &LutPreparationCache::uncached(),
        )
        .expect("prepare animated hierarchy");
        let retained = program.retained_bytes_estimate();
        let early = program.evaluate(tt(2)).expect("early frame");
        let late = program.evaluate(tt(8)).expect("late frame");
        assert_ne!(early.signature_hash(), late.signature_hash());
        assert_eq!(program.retained_bytes_estimate(), retained);
    }

    #[test]
    fn prepared_grade_reports_stable_graph_identity_and_external_lut_dependency() {
        let missing_output = GradeGraphNodeId::new();
        let invalid = GradeGraph {
            nodes: mondrian_core::AuthoringList::from([mondrian_core::GradeGraphNode {
                id: GradeGraphNodeId::new(),
                kind: GradeGraphNodeKind::Input,
            }]),
            output: missing_output,
        };
        for _ in 0..2 {
            assert!(matches!(
                PreparedGradeGraph::prepare(&invalid, WorkingColorSpace::LinearRec709),
                Err(EffectGraphBuildError::GradeGraphInvalidAuthorState {
                    output_node_id,
                    ..
                }) if output_node_id == missing_output
            ));
        }

        let path = temporary_cube("mondrian-prepared-grade-lut");
        let grade = PreparedGradeGraph::prepare(
            &serial_grade(bound_lut_effect(&path)),
            WorkingColorSpace::LinearRec709,
        )
        .expect("prepare grade LUT");
        assert!(grade.has_external_dependencies());
        assert!(grade.dependencies_are_current().expect("check grade LUT"));
        assert!(grade.retained_bytes_estimate() > std::mem::size_of::<PreparedGradeGraph>());
        let _ = std::fs::remove_file(path);
    }

    fn execute_window(shape: MaskShape, invert: bool) -> Vec<[f32; 4]> {
        let mask = MaskComponent::new(
            "window".to_owned(),
            MaskEvaluation { shape, invert, ..MaskEvaluation::default() },
        );
        let graph = PreparedEffectProgram::prepare(
            &[exposure_effect(1.0)],
            &[mask],
            WorkingColorSpace::LinearRec709,
        )
        .expect("prepare Power Window program")
        .evaluate(TimelineTime::ZERO)
        .expect("evaluate Power Window graph");
        let mask_binding = graph.stage_bindings().last().expect("Mask stage binding");
        let node_modes = mask_binding
            .emitted_nodes()
            .iter()
            .map(|node| graph.node_execution_modes(*node).expect("node execution modes"))
            .collect::<Vec<_>>();
        assert!(node_modes.iter().any(|modes| {
            modes.contains(
                crate::EffectProcessingBackend::Cpu,
                crate::EffectWorkingPrecision::NormalizedU8,
            )
        }));
        assert!(node_modes.iter().any(|modes| {
            modes.contains(
                crate::EffectProcessingBackend::Gpu,
                crate::EffectWorkingPrecision::Float32,
            )
        }));

        let input = vec![[1.5, -0.25, 0.125, 0.37]; 64];
        crate::apply_compiled_effect_graph_rgba_f32(&input, 8, 8, &graph, 0)
            .expect("execute Power Window graph")
    }

    #[test]
    fn prepared_rectangle_ellipse_and_bezier_are_grade_mattes_not_alpha_masks() {
        let shapes = [
            MaskShape::Rectangle {
                x: 0.25,
                y: 0.25,
                width: 0.5,
                height: 0.5,
                corner_radius: 0.08,
            },
            MaskShape::Ellipse {
                center: glam::Vec2::splat(0.5),
                radii: glam::Vec2::splat(0.3),
            },
            MaskShape::Path {
                points: vec![
                    crate::BezierPoint::new(glam::Vec2::new(0.25, 0.25)),
                    crate::BezierPoint::new(glam::Vec2::new(0.75, 0.25)),
                    crate::BezierPoint::new(glam::Vec2::new(0.75, 0.75)),
                    crate::BezierPoint::new(glam::Vec2::new(0.25, 0.75)),
                ],
                closed: true,
            },
        ];

        for shape in shapes {
            let output = execute_window(shape, false);
            let outside = output[0];
            let inside = output[4 * 8 + 4];
            assert_eq!(outside, [1.5, -0.25, 0.125, 0.37]);
            assert_eq!(inside, [3.0, -0.5, 0.25, 0.37]);
            assert!(
                inside[0] > 1.0 && inside[1] < 0.0,
                "HDR/negative RGB must survive"
            );
        }
    }

    #[test]
    fn prepared_power_window_invert_reverses_grade_selection_without_touching_alpha() {
        let output = execute_window(
            MaskShape::Rectangle {
                x: 0.25,
                y: 0.25,
                width: 0.5,
                height: 0.5,
                corner_radius: 0.0,
            },
            true,
        );

        assert_eq!(output[0], [3.0, -0.5, 0.25, 0.37]);
        assert_eq!(output[4 * 8 + 4], [1.5, -0.25, 0.125, 0.37]);
        assert!(output.iter().all(|pixel| pixel[3] == 0.37));
    }

    #[test]
    fn conservative_plugin_default_fails_closed() {
        let effect_type = EffectType::Plugin("plugin.prepared.conservative".to_owned());
        register_effect_definition(
            EffectDefinition::new(
                effect_type.key(),
                "Conservative",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_graph_builder(Arc::new(|_, _, _| Ok(()))),
        )
        .expect("register plugin");
        let effect = EffectNode::new(effect_type);
        assert!(matches!(
            PreparedEffectProgram::prepare(&[effect], &[], WorkingColorSpace::LinearRec709),
            Err(EffectGraphBuildError::ExecutionContractUnavailable { .. })
        ));
    }

    #[test]
    fn explicit_lut_cache_shares_payload_and_program_charge_includes_table() {
        let path = temporary_cube("mondrian-prepared-explicit-lut-cache");
        let effect = bound_lut_effect(&path);
        let cache = LutPreparationCache::new(crate::LutPreparationCacheConfig::new(4, 1024 * 1024));

        let first = PreparedEffectProgram::prepare_with_lut_cache(
            std::slice::from_ref(&effect),
            &[],
            WorkingColorSpace::LinearRec709,
            &cache,
        )
        .expect("prepare first program");
        let second = PreparedEffectProgram::prepare_with_lut_cache(
            &[effect],
            &[],
            WorkingColorSpace::LinearRec709,
            &cache,
        )
        .expect("prepare second program");
        let first_lut = prepared_lut_from_program(&first);
        let second_lut = prepared_lut_from_program(&second);
        assert!(Arc::ptr_eq(&first_lut, &second_lut));

        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entries, 1);
        assert!(diagnostics.retained_bytes >= first_lut.retained_bytes_estimate());
        assert!(diagnostics.retained_bytes <= diagnostics.max_bytes);
        let identity = PreparedEffectProgram::prepare(&[], &[], WorkingColorSpace::LinearRec709)
            .expect("prepare identity");
        assert!(first.retained_bytes_estimate() > identity.retained_bytes_estimate());
        assert!(first.retained_bytes_estimate() >= first_lut.retained_bytes_estimate());

        cache.clear();
        assert!(cache.is_empty());
        let retained_after_eviction = prepared_lut_from_program(&first);
        assert!(
            Arc::ptr_eq(&retained_after_eviction, &first_lut),
            "cache eviction must not mutate an already prepared immutable program"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn convenience_preparation_has_no_hidden_lut_residency() {
        let path = temporary_cube("mondrian-prepared-uncached-lut");
        let effect = bound_lut_effect(&path);
        let first = PreparedEffectProgram::prepare(
            std::slice::from_ref(&effect),
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .expect("prepare first uncached program");
        let second =
            PreparedEffectProgram::prepare(&[effect], &[], WorkingColorSpace::LinearRec709)
                .expect("prepare second uncached program");

        assert!(
            !Arc::ptr_eq(
                &prepared_lut_from_program(&first),
                &prepared_lut_from_program(&second),
            ),
            "convenience preparation must not borrow process-global residency"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn immutable_resource_is_prepared_once_for_many_frames() {
        let effect_type = EffectType::Plugin("plugin.prepared.resource_once".to_owned());
        let prepare_count = Arc::new(AtomicUsize::new(0));
        let prepare_count_for_factory = Arc::clone(&prepare_count);
        let preparer: EffectGraphPreparer = Arc::new(move |_, _, _| {
            prepare_count_for_factory.fetch_add(1, Ordering::SeqCst);
            let evaluator: EffectGraphBuilder = Arc::new(|_, _, graph| {
                graph.append_unary(EffectRenderOp::Vignette { intensity: 0.25, feather: 0.5 });
                Ok(())
            });
            Ok(PreparedEffectEvaluator::new(evaluator).with_dependency(
                EffectResourceDependency::PluginManaged { identity: "resource-v1".to_owned() },
            ))
        });
        register_effect_definition(
            EffectDefinition::new(
                effect_type.key(),
                "Prepared Resource",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                resource_lifetime: EffectResourceLifetime::PreparedProgram,
                ..cpu_linear_contract()
            })
            .with_prepared_graph_builder(preparer),
        )
        .expect("register plugin");

        let program = PreparedEffectProgram::prepare(
            &[EffectNode::new(effect_type)],
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .expect("prepare program");
        assert!(program.has_external_dependencies());
        let count_after_registry_stable_prepare = prepare_count.load(Ordering::SeqCst);
        assert!(count_after_registry_stable_prepare >= 1);
        program.evaluate(tt(1)).expect("frame one");
        program.evaluate(tt(2)).expect("frame two");
        assert_eq!(
            prepare_count.load(Ordering::SeqCst),
            count_after_registry_stable_prepare,
            "frame evaluation must reuse the successfully prepared immutable resource"
        );
    }

    #[test]
    fn prepared_dependency_rejects_frame_lifetime_declaration() {
        let effect_type =
            EffectType::Plugin("plugin.prepared.invalid_resource_lifetime".to_owned());
        let preparer: EffectGraphPreparer = Arc::new(|_, _, _| {
            Ok(
                PreparedEffectEvaluator::new(Arc::new(|_, _, _| Ok(()))).with_dependency(
                    EffectResourceDependency::PluginManaged {
                        identity: "resource-with-invalid-lifetime".to_owned(),
                    },
                ),
            )
        });
        register_effect_definition(
            EffectDefinition::new(
                effect_type.key(),
                "Invalid Resource Lifetime",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(cpu_linear_contract())
            .with_prepared_graph_builder(preparer),
        )
        .expect("register invalid resource definition");

        assert!(matches!(
            PreparedEffectProgram::prepare(
                &[EffectNode::new(effect_type)],
                &[],
                WorkingColorSpace::LinearRec709,
            ),
            Err(EffectGraphBuildError::ExecutionContractViolation {
                violation,
                ..
            }) if matches!(
                *violation,
                crate::EffectExecutionContractViolation::ResourceLifetimeTooShort { .. }
            )
        ));
    }

    #[test]
    fn animated_values_reuse_topology_and_change_frame_graph() {
        let mut effect = EffectNode::with_defaults(EffectType::GaussianBlur);
        let radius_id = EffectType::GaussianBlur.parameter_id("radius").expect("radius ID");
        let (_, property) = effect
            .properties
            .iter()
            .find(|(_, property)| property.descriptor.parameter_id() == &radius_id)
            .expect("radius property");
        let path = property.descriptor.path.clone();
        effect
            .properties
            .set_keyframe(&path, Keyframe::linear(tt(0), PropertyValue::Float(2.0)))
            .expect("first key");
        effect
            .properties
            .set_keyframe(&path, Keyframe::linear(tt(10), PropertyValue::Float(8.0)))
            .expect("second key");

        let program =
            PreparedEffectProgram::prepare(&[effect], &[], WorkingColorSpace::LinearRec709)
                .expect("prepare animated program");
        assert!(!program.has_external_dependencies());
        let retained_before = program.retained_bytes_estimate();
        let first = program.evaluate(tt(2)).expect("first frame");
        let second = program.evaluate(tt(8)).expect("second frame");
        assert_ne!(first.signature_hash(), second.signature_hash());
        assert_eq!(program.retained_bytes_estimate(), retained_before);
    }

    #[test]
    fn dynamic_topology_residency_is_session_owned_and_program_charge_is_immutable() {
        let program = dynamic_topology_program("plugin.prepared.session_topology_owner");
        let retained_before = program.retained_bytes_estimate();
        let mut preview = EffectExecutionSession::new(topology_session_config(10, 2 * 1024 * 1024));
        let mut export = EffectExecutionSession::new(topology_session_config(10, 2 * 1024 * 1024));
        preview.bind_generation(11);
        export.bind_generation(29);

        let preview_first = program
            .evaluate_with_session(tt(1), &mut preview)
            .expect("prepare Preview topology");
        let preview_second = program
            .evaluate_with_session(tt(2), &mut preview)
            .expect("reuse Preview topology");
        assert_eq!(
            preview_first.signature_hash(),
            preview_second.signature_hash()
        );
        assert_eq!(preview.diagnostics().topology_entries, 1);
        assert!(preview.diagnostics().topology_bytes <= preview.diagnostics().max_topology_bytes);
        assert!(preview.diagnostics().cache_entries <= preview.diagnostics().max_cache_entries);
        assert!(preview.diagnostics().cache_bytes <= preview.diagnostics().max_cache_bytes);
        assert_eq!(export.diagnostics().topology_entries, 0);
        assert_eq!(program.retained_bytes_estimate(), retained_before);

        program
            .evaluate_with_session(tt(1), &mut export)
            .expect("prepare independent Export topology");
        assert_eq!(export.diagnostics().topology_entries, 1);
        preview.clear_pixel_caches();
        assert_eq!(
            preview.diagnostics().topology_entries,
            1,
            "clearing pixel caches must retain frame-independent topology residency"
        );
        assert_eq!(
            export.diagnostics().topology_entries,
            1,
            "one owner cannot evict another owner's topology residency"
        );

        program
            .evaluate_with_session(tt(1), &mut preview)
            .expect("reuse retained Preview topology");
        assert_eq!(preview.diagnostics().topology_entries, 1);
        preview.bind_generation(12);
        assert_eq!(
            preview.diagnostics().topology_entries,
            1,
            "generation rotation must retain frame-independent topology residency"
        );

        export.reconfigure(topology_session_config(0, 0));
        assert_eq!(export.diagnostics().topology_entries, 0);
        assert_eq!(export.diagnostics().topology_bytes, 0);
        assert_eq!(program.retained_bytes_estimate(), retained_before);
    }

    #[test]
    fn temporal_preparation_reuses_owner_topology_and_rotates_with_generation() {
        let program = dynamic_temporal_topology_program("plugin.prepared.temporal_topology_owner");
        let extent = crate::EffectFrameExtent::new(8, 4);
        let request = crate::EffectTemporalExecutionRequest::new(
            11,
            crate::EffectExecutionContinuity::Continuous,
            TimelineTime::ONE,
            extent,
            extent.full_frame_roi(),
            mondrian_core::ExecutionCancellationToken::new(),
        );
        let mut session = EffectExecutionSession::new(topology_session_config(10, 2 * 1024 * 1024));

        let first = session
            .prepare_temporal_frame_execution(&program, &request, |_| Ok(1))
            .expect("prepare first temporal frame");
        assert_eq!(first.demands().requests().len(), 2);
        assert_eq!(session.diagnostics().generation, Some(11));
        assert_eq!(
            session.diagnostics().topology_entries,
            1,
            "root and sampled evaluations must share one retained dynamic topology"
        );

        session
            .prepare_temporal_frame_execution(&program, &request, |_| Ok(1))
            .expect("reuse temporal topology");
        assert_eq!(
            session.diagnostics().topology_entries,
            1,
            "repeated preparation in one generation must not duplicate topology residency"
        );

        let next_request = crate::EffectTemporalExecutionRequest::new(
            12,
            crate::EffectExecutionContinuity::Discontinuous,
            TimelineTime::ONE,
            extent,
            extent.full_frame_roi(),
            mondrian_core::ExecutionCancellationToken::new(),
        );
        session
            .prepare_temporal_frame_execution(&program, &next_request, |_| Ok(1))
            .expect("prepare next generation");
        assert_eq!(session.diagnostics().generation, Some(12));
        assert_eq!(
            session.diagnostics().topology_entries,
            1,
            "generation rotation must clear and rebuild, not accumulate, residency"
        );
    }

    #[test]
    fn over_budget_dynamic_topology_executes_without_entering_residency() {
        let program = dynamic_topology_program("plugin.prepared.oversize_topology");
        let reference = program.evaluate(tt(1)).expect("uncached reference");
        let retained_before = program.retained_bytes_estimate();
        let mut session = EffectExecutionSession::new(topology_session_config(5, 5));
        session.bind_generation(1);

        let first = program
            .evaluate_with_session(tt(1), &mut session)
            .expect("oversize topology still executes");
        let second = program
            .evaluate_with_session(tt(1), &mut session)
            .expect("oversize topology recompiles correctly");
        assert_eq!(first.signature_hash(), reference.signature_hash());
        assert_eq!(second.signature_hash(), reference.signature_hash());
        assert_eq!(session.diagnostics().topology_entries, 0);
        assert_eq!(session.diagnostics().topology_bytes, 0);
        assert_eq!(program.retained_bytes_estimate(), retained_before);
    }

    #[test]
    fn stack_contract_aggregates_exact_modes_determinism_and_roi() {
        let deterministic_type = EffectType::Plugin("plugin.prepared.contract_a".to_owned());
        let seeded_type = EffectType::Plugin("plugin.prepared.contract_b".to_owned());
        let evaluator: EffectGraphBuilder = Arc::new(|_, _, _| Ok(()));
        register_effect_definition(
            EffectDefinition::new(
                deterministic_type.key(),
                "A",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_F32.union(EffectExecutionModes::GPU_F32),
                roi_propagation: EffectRoiPropagation::Expand {
                    horizontal_pixels: 2,
                    vertical_pixels: 3,
                },
                ..cpu_linear_contract()
            })
            .with_graph_builder(Arc::clone(&evaluator)),
        )
        .expect("register A");
        register_effect_definition(
            EffectDefinition::new(
                seeded_type.key(),
                "B",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                determinism: EffectDeterminism::FrameSeeded,
                roi_propagation: EffectRoiPropagation::Expand {
                    horizontal_pixels: 5,
                    vertical_pixels: 7,
                },
                ..cpu_linear_contract()
            })
            .with_graph_builder(evaluator),
        )
        .expect("register B");

        let program = PreparedEffectProgram::prepare(
            &[
                EffectNode::new(deterministic_type),
                EffectNode::new(seeded_type),
            ],
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .expect("prepare stack");
        let contract = program.execution_contract();
        assert_eq!(contract.execution_modes, EffectExecutionModes::CPU_F32);
        assert_eq!(contract.determinism, EffectDeterminism::FrameSeeded);
        assert_eq!(
            contract.roi_propagation,
            EffectRoiPropagation::Expand { horizontal_pixels: 7, vertical_pixels: 10 }
        );
        let compiled = program.evaluate(tt(1)).expect("bind identity stages");
        assert_eq!(compiled.stage_bindings().len(), 2);
        assert!(compiled
            .stage_bindings()
            .iter()
            .all(|binding| binding.emitted_nodes().is_empty()
                && binding.input_value() == EffectGraphNodeId(0)
                && binding.output_value() == EffectGraphNodeId(0)));
    }

    #[test]
    fn general_dag_definition_binds_every_emitted_value_to_one_stage() {
        let effect_type = EffectType::Plugin("plugin.prepared.bound_general_dag".to_owned());
        register_effect_definition(
            EffectDefinition::new(
                effect_type.key(),
                "Bound DAG",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                topology: EffectGraphTopology::GeneralDag,
                ..cpu_linear_contract()
            })
            .with_branching_graph_builder(Arc::new(|_, _, graph| {
                let source = graph.current_output();
                let left = graph.add_unary_from(
                    source,
                    EffectRenderOp::Vignette { intensity: 0.2, feather: 0.8 },
                );
                let right = graph.add_unary_from(
                    source,
                    EffectRenderOp::Vignette { intensity: 0.4, feather: 0.6 },
                );
                let output = graph.add_blend(left, right, BlendMode::Normal, 0.5);
                graph.set_current_output(output);
                Ok(())
            })),
        )
        .expect("register DAG definition");

        let compiled = PreparedEffectProgram::prepare(
            &[EffectNode::new(effect_type)],
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .expect("prepare DAG")
        .evaluate(tt(1))
        .expect("bind DAG");
        let binding = &compiled.stage_bindings()[0];
        assert_eq!(binding.stage_index(), 0);
        assert_eq!(binding.input_value(), EffectGraphNodeId(0));
        assert_eq!(
            binding.output_value(),
            compiled.graph().output.expect("output")
        );
        assert_eq!(binding.emitted_nodes().len(), 3);
        assert!(binding.emitted_nodes().iter().all(|node_id| {
            compiled
                .node_execution_modes(*node_id)
                .is_some_and(|modes| modes == EffectExecutionModes::CPU_F32)
        }));
    }

    #[test]
    fn heterogeneous_backend_chain_remains_valid_and_retains_stage_contracts() {
        let cpu_type = EffectType::Plugin("plugin.prepared.heterogeneous_cpu".to_owned());
        let gpu_type = EffectType::Plugin("plugin.prepared.heterogeneous_gpu".to_owned());
        let cpu_evaluator: EffectGraphBuilder = Arc::new(|_, _, graph| {
            graph.append_unary(EffectRenderOp::GaussianBlur { radius: 1.0 });
            Ok(())
        });
        let gpu_evaluator: EffectGraphBuilder = Arc::new(|_, _, graph| {
            graph.append_unary(EffectRenderOp::Vignette { intensity: 0.25, feather: 0.75 });
            Ok(())
        });
        register_effect_definition(
            EffectDefinition::new(
                cpu_type.key(),
                "CPU",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                roi_propagation: EffectRoiPropagation::Expand {
                    horizontal_pixels: 3,
                    vertical_pixels: 3,
                },
                ..cpu_linear_contract()
            })
            .with_graph_builder(cpu_evaluator),
        )
        .expect("register CPU definition");
        register_effect_definition(
            EffectDefinition::new(
                gpu_type.key(),
                "GPU",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::GPU_F32,
                ..cpu_linear_contract()
            })
            .with_graph_builder(gpu_evaluator),
        )
        .expect("register GPU definition");

        let program = PreparedEffectProgram::prepare(
            &[EffectNode::new(cpu_type), EffectNode::new(gpu_type)],
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .expect("heterogeneous preparation is valid");
        assert!(program.execution_envelope().requires_execution_transitions());
        assert_eq!(program.execution_envelope().stages().len(), 2);
        assert!(program.execution_envelope().homogeneous_processing_backends().is_empty());

        let graph = program.evaluate(tt(1)).expect("bind heterogeneous graph");
        assert_eq!(graph.stage_bindings().len(), 2);
        let cpu_node = graph.stage_bindings()[0].emitted_nodes()[0];
        let gpu_node = graph.stage_bindings()[1].emitted_nodes()[0];
        assert_eq!(
            graph.node_execution_modes(cpu_node),
            Some(EffectExecutionModes::CPU_F32)
        );
        assert_eq!(
            graph.node_execution_modes(gpu_node),
            Some(EffectExecutionModes::GPU_F32)
        );
        assert!(matches!(
            crate::apply_compiled_effect_graph_rgba_f32(
                &[[0.0, 0.0, 0.0, 1.0]; 9],
                3,
                3,
                &graph,
                0,
            ),
            Err(crate::EffectFloatExecutionError::ExecutionContract(
                crate::EffectExecutionAdmissionError::ExecutionModeNotAdmitted {
                    stage_index: 1,
                    backend: crate::EffectProcessingBackend::Cpu,
                    precision: crate::EffectWorkingPrecision::Float32,
                    admitted: crate::EffectExecutionModes::GPU_F32,
                }
            ))
        ));
        assert!(matches!(
            crate::lower_effect_graph_to_gpu_plan(&graph),
            Err(crate::EffectGpuPlanBlocker::ExecutionContract(
                crate::EffectExecutionAdmissionError::ExecutionModeNotAdmitted {
                    stage_index: 0,
                    backend: crate::EffectProcessingBackend::Gpu,
                    precision: crate::EffectWorkingPrecision::Float32,
                    admitted: crate::EffectExecutionModes::CPU_F32,
                }
            ))
        ));
    }

    #[test]
    fn stateful_contract_prepares_but_single_frame_executor_fails_closed() {
        let effect_type = EffectType::Plugin("plugin.prepared.stateful".to_owned());
        register_effect_definition(
            EffectDefinition::new(
                effect_type.key(),
                "Stateful",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                state_model: EffectStateModel::StatefulSequential,
                resource_lifetime: EffectResourceLifetime::ContinuitySession,
                ..cpu_linear_contract()
            })
            .with_graph_builder(Arc::new(|_, _, graph| {
                graph.append_unary(EffectRenderOp::Vignette { intensity: 0.25, feather: 0.5 });
                Ok(())
            })),
        )
        .expect("register stateful definition");

        let program = PreparedEffectProgram::prepare(
            &[EffectNode::new(effect_type)],
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .expect("valid stateful author contract prepares");
        let graph = program.evaluate(tt(1)).expect("compile stateful graph");
        assert_eq!(
            graph
                .plan_execution_demand(
                    tt(1),
                    crate::EffectFrameExtent::new(1, 1),
                    crate::EffectPixelRoi::new(0, 0, 1, 1),
                )
                .expect("stateful demand")
                .obligations()
                .state_model(),
            EffectStateModel::StatefulSequential
        );
        assert!(matches!(
            crate::apply_compiled_effect_graph_rgba_f32(&[[0.0, 0.0, 0.0, 1.0]], 1, 1, &graph, 0,),
            Err(crate::EffectFloatExecutionError::ExecutionContract(
                crate::EffectExecutionAdmissionError::ContinuitySessionRequired
            ))
        ));
    }

    #[test]
    fn prepared_custom_processor_is_immutable_across_definition_replacement() {
        let effect_type = EffectType::Plugin("plugin.prepared.immutable_processor".to_owned());
        let definition = |red: u8| {
            EffectDefinition::new(
                effect_type.key(),
                "Immutable Processor",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_U8,
                roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
                ..cpu_linear_contract()
            })
            .with_custom_render_processor(
                Arc::new(|_, _| Ok(Some(serde_json::json!({})))),
                Arc::new(move |pixels, _, _, _, _| {
                    for pixel in pixels.chunks_exact_mut(4) {
                        pixel[0] = red;
                    }
                    Ok(())
                }),
            )
        };

        register_effect_definition(definition(17)).expect("register first processor");
        let effect = EffectNode::new(effect_type.clone());
        let first = PreparedEffectProgram::prepare(
            std::slice::from_ref(&effect),
            &[],
            WorkingColorSpace::LinearRec709,
        )
        .expect("prepare first processor");
        let first_graph = first.evaluate(tt(1)).expect("bind first processor");

        register_effect_definition(definition(203)).expect("replace processor definition");
        let second =
            PreparedEffectProgram::prepare(&[effect], &[], WorkingColorSpace::LinearRec709)
                .expect("prepare replacement processor");
        let second_graph = second.evaluate(tt(1)).expect("bind replacement processor");

        let input = [0, 0, 0, 255];
        let first_output = crate::apply_compiled_effect_graph(&input, 1, 1, &first_graph, 0)
            .expect("execute retained first processor");
        let second_output = crate::apply_compiled_effect_graph(&input, 1, 1, &second_graph, 0)
            .expect("execute replacement processor");
        assert_eq!(first_output[0], 17);
        assert_eq!(second_output[0], 203);
        assert_ne!(first_graph.signature_hash(), second_graph.signature_hash());
    }
}
