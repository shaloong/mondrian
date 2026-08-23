//! Time-expanded execution projection for finite temporal Effect graphs.
//!
//! The projection references the sole production [`CompiledEffectGraph`] IR.
//! It does not copy Effect semantics: it only addresses graph values by exact
//! evaluation time and records the cross-time edges introduced by finite
//! temporal operations.

use super::*;

const MAX_TEMPORAL_GRAPH_CONTEXTS: usize = 512;
const MAX_TEMPORAL_EXPANDED_VALUES: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum TemporalValueAddress {
    Source(TimelineTime),
    Graph {
        context: usize,
        node_id: EffectGraphNodeId,
    },
}

#[derive(Debug, Clone)]
pub(super) struct TemporalGraphContext {
    pub(super) time: TimelineTime,
    pub(super) frame_seed: i64,
    pub(super) graph: Arc<CompiledEffectGraph>,
}

#[derive(Debug, Clone)]
pub(super) struct PreparedTemporalValueProgram {
    contexts: Arc<[TemporalGraphContext]>,
    ordered_values: Arc<[TemporalValueAddress]>,
    root: TemporalValueAddress,
    remaining_uses: HashMap<TemporalValueAddress, usize>,
    temporal_samples: HashMap<TemporalValueAddress, TemporalValueAddress>,
    source_times: Arc<[TimelineTime]>,
    fingerprint: [u8; 32],
    temporal_nodes: usize,
}

impl PreparedTemporalValueProgram {
    pub(super) fn contexts(&self) -> &[TemporalGraphContext] {
        &self.contexts
    }

    pub(super) fn ordered_values(&self) -> &[TemporalValueAddress] {
        &self.ordered_values
    }

    pub(super) const fn root(&self) -> TemporalValueAddress {
        self.root
    }

    pub(super) fn use_counts(&self) -> &HashMap<TemporalValueAddress, usize> {
        &self.remaining_uses
    }

    pub(super) fn temporal_sample(
        &self,
        address: TemporalValueAddress,
    ) -> Option<TemporalValueAddress> {
        self.temporal_samples.get(&address).copied()
    }

    pub(super) fn source_times(&self) -> &[TimelineTime] {
        &self.source_times
    }

    pub(super) const fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }

    pub(super) const fn temporal_nodes(&self) -> usize {
        self.temporal_nodes
    }

    pub(super) fn context(
        &self,
        index: usize,
    ) -> Result<&TemporalGraphContext, EffectTemporalExecutionError> {
        self.contexts
            .get(index)
            .ok_or(EffectTemporalExecutionError::InvalidGraphLiveness {
                reason: "time-expanded graph context is missing",
            })
    }

    pub(super) fn address_for_input(
        &self,
        context: usize,
        node_id: EffectGraphNodeId,
    ) -> Result<TemporalValueAddress, EffectTemporalExecutionError> {
        let graph = &self.context(context)?.graph;
        let node = graph.graph().node(node_id).ok_or(
            EffectTemporalExecutionError::UnsupportedGraphNode { node_id, kind: "missing" },
        )?;
        Ok(if matches!(node.kind, EffectGraphNodeKind::Source) {
            TemporalValueAddress::Source(self.context(context)?.time)
        } else {
            TemporalValueAddress::Graph { context, node_id }
        })
    }
}

pub(super) fn prepare_temporal_value_program(
    root_graph: Arc<CompiledEffectGraph>,
    request: &EffectTemporalExecutionRequest,
    evaluate_sample: Option<TemporalSampleEvaluator<'_>>,
) -> Result<PreparedTemporalValueProgram, EffectTemporalExecutionError> {
    let root_output = root_graph
        .graph()
        .output
        .ok_or(EffectTemporalExecutionError::MissingGraphOutput)?;
    let mut builder = TemporalProgramBuilder {
        root_contracts: root_graph
            .stage_bindings()
            .iter()
            .map(|binding| (binding.stage_index(), binding.contract()))
            .collect(),
        contexts: vec![TemporalGraphContext {
            time: request.output_time,
            frame_seed: request.output_frame_seed,
            graph: root_graph,
        }],
        context_by_time: HashMap::from([(request.output_time, 0)]),
        evaluate_sample,
        visit: HashMap::new(),
        ordered_values: Vec::new(),
        remaining_uses: HashMap::new(),
        temporal_samples: HashMap::new(),
        source_times: Vec::new(),
        seen_source_times: HashSet::new(),
        temporal_nodes: 0,
    };
    let root = builder.visit_graph_value(0, root_output)?;
    let fingerprint = fingerprint_program(
        &builder.contexts,
        &builder.ordered_values,
        &builder.remaining_uses,
        &builder.temporal_samples,
        root,
    );
    Ok(PreparedTemporalValueProgram {
        contexts: builder.contexts.into(),
        ordered_values: builder.ordered_values.into(),
        root,
        remaining_uses: builder.remaining_uses,
        temporal_samples: builder.temporal_samples,
        source_times: builder.source_times.into(),
        fingerprint,
        temporal_nodes: builder.temporal_nodes,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisitState {
    Visiting,
    Complete,
}

struct TemporalProgramBuilder<'a> {
    root_contracts: Vec<(usize, crate::EffectExecutionContract)>,
    contexts: Vec<TemporalGraphContext>,
    context_by_time: HashMap<TimelineTime, usize>,
    evaluate_sample: Option<TemporalSampleEvaluator<'a>>,
    visit: HashMap<TemporalValueAddress, VisitState>,
    ordered_values: Vec<TemporalValueAddress>,
    remaining_uses: HashMap<TemporalValueAddress, usize>,
    temporal_samples: HashMap<TemporalValueAddress, TemporalValueAddress>,
    source_times: Vec<TimelineTime>,
    seen_source_times: HashSet<TimelineTime>,
    temporal_nodes: usize,
}

impl TemporalProgramBuilder<'_> {
    fn visit_graph_value(
        &mut self,
        context: usize,
        node_id: EffectGraphNodeId,
    ) -> Result<TemporalValueAddress, EffectTemporalExecutionError> {
        let context_ref = self.contexts.get(context).ok_or(
            EffectTemporalExecutionError::InvalidGraphLiveness {
                reason: "time-expanded graph context disappeared during preparation",
            },
        )?;
        let node = context_ref.graph.graph().node(node_id).cloned().ok_or(
            EffectTemporalExecutionError::UnsupportedGraphNode { node_id, kind: "missing" },
        )?;
        if matches!(node.kind, EffectGraphNodeKind::Source) {
            return self.visit_source(context_ref.time);
        }
        let address = TemporalValueAddress::Graph { context, node_id };
        match self.visit.get(&address) {
            Some(VisitState::Complete) => return Ok(address),
            Some(VisitState::Visiting) => {
                return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
                    reason: "time-expanded Effect dependency contains a cycle",
                });
            }
            None => {}
        }
        if self.visit.len() >= MAX_TEMPORAL_EXPANDED_VALUES {
            return Err(EffectTemporalExecutionError::TemporalProgramLimitExceeded {
                kind: "expanded values",
                limit: MAX_TEMPORAL_EXPANDED_VALUES,
            });
        }
        self.visit.insert(address, VisitState::Visiting);
        match node.kind {
            EffectGraphNodeKind::Source => unreachable!("source was normalized above"),
            EffectGraphNodeKind::UnaryEffect { input, op }
            | EffectGraphNodeKind::DomainEffect { input, op, .. } => {
                let input_address = self.visit_graph_value(context, input)?;
                self.add_use(input_address)?;
                if let crate::EffectRenderOp::TemporalFrameBlend { sample_offset, mix } = op {
                    validate_temporal_blend(sample_offset, mix)?;
                    self.temporal_nodes = self.temporal_nodes.checked_add(1).ok_or(
                        EffectTemporalExecutionError::InvalidGraphLiveness {
                            reason: "temporal node count overflowed",
                        },
                    )?;
                    let stage_index = self.temporal_owner_stage(context, node_id, input)?;
                    let sample_time =
                        temporal_sample_time(self.contexts[context].time, sample_offset)?;
                    let sample_address = if sample_time == self.contexts[context].time {
                        input_address
                    } else if stage_index == 0 {
                        self.visit_source(sample_time)?
                    } else {
                        let sample_context = self.context_for_sample(sample_time)?;
                        let binding = self.contexts[sample_context]
                            .graph
                            .stage_bindings()
                            .get(stage_index)
                            .ok_or(EffectTemporalExecutionError::SampledStageContractChanged {
                                stage_index,
                            })?;
                        if binding.stage_index() != stage_index {
                            return Err(
                                EffectTemporalExecutionError::SampledStageContractChanged {
                                    stage_index,
                                },
                            );
                        }
                        self.visit_graph_value(sample_context, binding.input_value())?
                    };
                    self.add_use(sample_address)?;
                    if self.temporal_samples.insert(address, sample_address).is_some() {
                        return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
                            reason: "temporal graph value received more than one sample edge",
                        });
                    }
                }
            }
            EffectGraphNodeKind::Blend { base, overlay, .. } => {
                let base = self.visit_graph_value(context, base)?;
                let overlay = self.visit_graph_value(context, overlay)?;
                self.add_use(base)?;
                self.add_use(overlay)?;
            }
            EffectGraphNodeKind::Mask { input, mask, .. } => {
                let input = self.visit_graph_value(context, input)?;
                let mask = self.visit_graph_value(context, mask)?;
                self.add_use(input)?;
                self.add_use(mask)?;
            }
            EffectGraphNodeKind::MaskSource { .. } => {}
            EffectGraphNodeKind::MultiInput { inputs, .. } => {
                if inputs.is_empty() {
                    return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
                        reason: "multi-input node has no inputs",
                    });
                }
                for input in inputs {
                    let input = self.visit_graph_value(context, input)?;
                    self.add_use(input)?;
                }
            }
        }
        self.visit.insert(address, VisitState::Complete);
        self.ordered_values.push(address);
        Ok(address)
    }

    fn visit_source(
        &mut self,
        time: TimelineTime,
    ) -> Result<TemporalValueAddress, EffectTemporalExecutionError> {
        let address = TemporalValueAddress::Source(time);
        if self.visit.contains_key(&address) {
            return Ok(address);
        }
        if self.visit.len() >= MAX_TEMPORAL_EXPANDED_VALUES {
            return Err(EffectTemporalExecutionError::TemporalProgramLimitExceeded {
                kind: "expanded values",
                limit: MAX_TEMPORAL_EXPANDED_VALUES,
            });
        }
        self.visit.insert(address, VisitState::Complete);
        self.ordered_values.push(address);
        if self.seen_source_times.insert(time) {
            self.source_times.push(time);
        }
        Ok(address)
    }

    fn add_use(
        &mut self,
        address: TemporalValueAddress,
    ) -> Result<(), EffectTemporalExecutionError> {
        let uses = self.remaining_uses.entry(address).or_insert(0);
        *uses = uses.checked_add(1).ok_or(EffectTemporalExecutionError::InvalidGraphLiveness {
            reason: "time-expanded value use count overflowed",
        })?;
        Ok(())
    }

    fn temporal_owner_stage(
        &self,
        context: usize,
        node_id: EffectGraphNodeId,
        input: EffectGraphNodeId,
    ) -> Result<usize, EffectTemporalExecutionError> {
        let graph = &self.contexts[context].graph;
        let mut owners = graph
            .stage_bindings()
            .iter()
            .filter(|binding| binding.emitted_nodes().contains(&node_id));
        let owner =
            owners.next().ok_or(EffectTemporalExecutionError::UnsupportedTemporalShape {
                reason: "temporal node has no Definition-stage ownership evidence",
            })?;
        if owners.next().is_some() {
            return Err(EffectTemporalExecutionError::UnsupportedTemporalShape {
                reason: "temporal node has ambiguous Definition-stage ownership evidence",
            });
        }
        if owner.input_value() != input {
            return Err(EffectTemporalExecutionError::UnsupportedTemporalShape {
                reason: "temporal operation reads a same-stage derived value",
            });
        }
        Ok(owner.stage_index())
    }

    fn context_for_sample(
        &mut self,
        time: TimelineTime,
    ) -> Result<usize, EffectTemporalExecutionError> {
        if let Some(context) = self.context_by_time.get(&time).copied() {
            return Ok(context);
        }
        if self.contexts.len() >= MAX_TEMPORAL_GRAPH_CONTEXTS {
            return Err(EffectTemporalExecutionError::TemporalProgramLimitExceeded {
                kind: "graph contexts",
                limit: MAX_TEMPORAL_GRAPH_CONTEXTS,
            });
        }
        let evaluator = self.evaluate_sample.as_deref_mut().ok_or(
            EffectTemporalExecutionError::UnsupportedTemporalShape {
                reason: "temporal input has upstream Effects but no exact-time graph evaluator",
            },
        )?;
        let (graph, frame_seed) = evaluator(time).map_err(|reason| {
            EffectTemporalExecutionError::SampleGraphEvaluation { time, reason }
        })?;
        admit_temporal_scalar(&graph)?;
        if graph.domain_plan().requires_conversion() || !graph.domain_plan().blockers.is_empty() {
            return Err(EffectTemporalExecutionError::ColorDomainUnsupported);
        }
        let sampled_contracts = graph
            .stage_bindings()
            .iter()
            .map(|binding| (binding.stage_index(), binding.contract()))
            .collect::<Vec<_>>();
        if sampled_contracts != self.root_contracts {
            let first = sampled_contracts
                .iter()
                .zip(&self.root_contracts)
                .position(|(left, right)| left != right)
                .unwrap_or(sampled_contracts.len().min(self.root_contracts.len()));
            return Err(EffectTemporalExecutionError::SampledStageContractChanged {
                stage_index: first,
            });
        }
        let index = self.contexts.len();
        self.contexts.push(TemporalGraphContext { time, frame_seed, graph });
        self.context_by_time.insert(time, index);
        Ok(index)
    }
}

fn fingerprint_program(
    contexts: &[TemporalGraphContext],
    schedule: &[TemporalValueAddress],
    remaining_uses: &HashMap<TemporalValueAddress, usize>,
    temporal_samples: &HashMap<TemporalValueAddress, TemporalValueAddress>,
    root: TemporalValueAddress,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.effect-temporal-value-program.v1");
    hasher.update((contexts.len() as u64).to_le_bytes());
    for context in contexts {
        hash_time(&mut hasher, context.time);
        hasher.update(context.frame_seed.to_le_bytes());
        hasher.update(context.graph.semantic_fingerprint());
    }
    hasher.update((schedule.len() as u64).to_le_bytes());
    for address in schedule {
        hash_address(&mut hasher, *address);
        hasher.update(
            u64::try_from(remaining_uses.get(address).copied().unwrap_or(0))
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        if let Some(sample) = temporal_samples.get(address) {
            hasher.update([1]);
            hash_address(&mut hasher, *sample);
        } else {
            hasher.update([0]);
        }
    }
    hash_address(&mut hasher, root);
    hasher.finalize().into()
}

fn hash_address(hasher: &mut Sha256, address: TemporalValueAddress) {
    match address {
        TemporalValueAddress::Source(time) => {
            hasher.update([0]);
            hash_time(hasher, time);
        }
        TemporalValueAddress::Graph { context, node_id } => {
            hasher.update([1]);
            hasher.update((context as u64).to_le_bytes());
            hasher.update(node_id.0.to_le_bytes());
        }
    }
}
