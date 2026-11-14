//! Source-fact projection happens before any pixel producer or DSP admission.

use super::*;
use mondrian_core::VisualSourceReference;
use mondrian_effects::AuxiliaryPreviewFallback;
use mondrian_renderer::{
    PreparedAuxiliaryConsumerLocation, PreparedVisualInstanceStep, PreparedVisualProgram,
};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PreviewAuxiliaryNotice {
    pub(crate) sequence_id: SequenceId,
    pub(crate) time: mondrian_core::TimelineTime,
    pub(crate) path: Arc<[PreparedVisualInstanceStep]>,
    pub(crate) placement: Option<TimelineClipExecutionRef>,
    pub(crate) stage_index: usize,
    pub(crate) location: PreparedAuxiliaryConsumerLocation,
    pub(crate) fallback: AuxiliaryPreviewFallback,
}

/// This owner follows the resolved value into the evaluation cache. Its original
/// missing-source queries survive even when no ExternalInput slot remains.
pub(crate) struct PreviewAuxiliaryProjectionContext {
    pub(crate) notices: Box<[PreviewAuxiliaryNotice]>,
    pub(crate) unavailable_queries: Box<[PreviewAuxiliaryAssetRequest]>,
    pub(crate) available_queries: Box<[(PreviewAuxiliaryAssetRequest, PreviewAuxiliaryAssetPlan)]>,
    _programs: Box<[Arc<PreparedVisualProgram>]>,
    pub(crate) bytes: usize,
    reservation: OnceLock<super::super::preview_auxiliary::PreviewAuxiliaryWorkReservation>,
}

impl PreviewAuxiliaryProjectionContext {
    pub(crate) fn hash_identity(&self, hasher: &mut impl std::hash::Hasher) {
        b"mondrian.preview.auxiliary-projection.v2".hash(hasher);
        self.notices.hash(hasher);
        self.unavailable_queries.hash(hasher);
    }

    pub(crate) fn admit(
        &self,
        store: &mut super::super::preview_frame_store::PreviewFrameStoreAdapter,
    ) -> Result<(), PreviewUnavailability> {
        if self.reservation.get().is_none()
            && let Some(reservation) =
                super::super::preview_auxiliary::reserve_auxiliary_working_set(store, self.bytes)?
        {
            let _ = self.reservation.set(reservation);
        }
        Ok(())
    }

    pub(crate) fn validate(&self) -> Result<(), PreviewCpuExecutionError> {
        super::super::preview_auxiliary::validate_auxiliary_working_reservation(
            self.bytes,
            self.reservation.get(),
        )
    }
}

pub(super) struct SourceProjection {
    pub(super) programs: RefCell<HashMap<SequenceId, Arc<PreparedVisualProgram>>>,
    unavailable: Vec<VisualSourceReference>,
    unavailable_visual: UnavailableVisualSources,
    unscoped_views: RefCell<HashMap<SequenceId, Arc<PreparedVisualProgram>>>,
    registry_revision: u64,
    maximum_bytes: usize,
}

pub(super) type UnavailableVisualSources = HashMap<
    Arc<[PreparedVisualInstanceStep]>,
    Vec<(Option<mondrian_core::ClipId>, VisualSourceReference)>,
>;

impl SourceProjection {
    fn new(
        prepared: &PreviewTimelinePreparation,
        graph: PreviewTimelineGraph<'_>,
        unavailable: Vec<VisualSourceReference>,
        unavailable_visual: UnavailableVisualSources,
    ) -> Self {
        Self {
            programs: RefCell::new(prepared.frozen_programs.clone()),
            unavailable,
            unavailable_visual,
            unscoped_views: RefCell::new(HashMap::new()),
            registry_revision: prepared
                .original_closure
                .node(prepared.original_closure.root())
                .unwrap()
                .program()
                .effect_registry_revision(),
            maximum_bytes: graph.programs.borrow().diagnostics().max_retained_bytes,
        }
    }

    pub(super) fn resolve(
        &self,
        sequence: &Sequence,
        graph: PreviewTimelineGraph<'_>,
    ) -> Result<PreparedVisualProgramBinding, String> {
        let captured = self.programs.borrow().get(&sequence.id).cloned();
        let program = match captured {
            Some(program) => program,
            None => {
                let binding = graph.resolve_program(sequence).map_err(|error| error.to_string())?;
                let program = Arc::clone(binding.program());
                if program.effect_registry_revision() != self.registry_revision
                    || !program.auxiliary_preview_enabled()
                {
                    return Err("newly reachable Sequence has no matching frozen Preview definition revision".into());
                }
                self.programs.borrow_mut().insert(sequence.id, Arc::clone(&program));
                program
            }
        };
        PreparedVisualProgramBinding::checked(sequence, program).map_err(|error| error.to_string())
    }

    pub(super) fn view(
        &self,
        program: &Arc<PreparedVisualProgram>,
        path: &[PreparedVisualInstanceStep],
    ) -> Result<Arc<PreparedVisualProgram>, String> {
        let scoped = self.unavailable_visual.get(path).map(Vec::as_slice).unwrap_or_default();
        if self.unavailable.is_empty() && scoped.is_empty() {
            return Ok(Arc::clone(program));
        }
        if scoped.is_empty()
            && let Some(view) = self.unscoped_views.borrow().get(&program.sequence_id())
        {
            return Ok(Arc::clone(view));
        }
        let view = Arc::new(
            program
                .with_auxiliary_preview_unavailable_sources_by_consumer(&self.unavailable, scoped)
                .map_err(|error| error.to_string())?,
        );
        if view.retained_bytes_estimate() > self.maximum_bytes {
            return Err(
                "auxiliary Preview source view exceeds the immutable-program metadata grant".into(),
            );
        }
        if scoped.is_empty() {
            self.unscoped_views
                .borrow_mut()
                .insert(program.sequence_id(), Arc::clone(&view));
        }
        Ok(view)
    }
}

fn record_unavailable_visual_sources(
    closure: &PreparedVisualFrameClosure<PreparedTimelinePreviewEffectRoutes>,
    unavailable: &mut UnavailableVisualSources,
) -> bool {
    let mut changed = false;
    for node in closure.nodes() {
        for (consumer, source) in node.auxiliary_visual_sources() {
            if matches!(
                source,
                PreparedAuxiliaryVisualSource::Unavailable(
                    mondrian_renderer::PreparedAuxiliaryVisualUnavailable::Missing
                        | mondrian_renderer::PreparedAuxiliaryVisualUnavailable::Empty
                        | mondrian_renderer::PreparedAuxiliaryVisualUnavailable::ClipUnavailable
                        | mondrian_renderer::PreparedAuxiliaryVisualUnavailable::ClipNotEarlier
                        | mondrian_renderer::PreparedAuxiliaryVisualUnavailable::ClipNotIndependent
                )
            ) {
                let PreparedVisualAuxiliaryAddress::Authored(reference) = consumer.address else {
                    continue;
                };
                let decisions = unavailable.entry(node.instance_path().into()).or_default();
                let decision = (
                    consumer.placement.map(|placement| placement.clip_id),
                    reference,
                );
                if !decisions.contains(&decision) {
                    decisions.push(decision);
                    changed = true;
                }
            }
        }
    }
    changed
}

fn visit_graphs(
    plan: &mondrian_renderer::TimelineRenderPlan,
    mut visit: impl FnMut(
        Option<TimelineClipExecutionRef>,
        &Arc<CompiledEffectGraph>,
    ) -> Result<(), PreviewTimelineAbort>,
) -> Result<(), PreviewTimelineAbort> {
    for element in &plan.elements {
        element.visit_effect_graphs(&mut visit)?;
    }
    Ok(())
}

fn original_queries(
    prepared: &PreviewTimelinePreparation,
    graph: PreviewTimelineGraph<'_>,
) -> Result<Vec<PreparedPreviewSourceDemand>, PreviewTimelineAbort> {
    let mut requests = Vec::new();
    let mut seen = HashSet::new();
    for node in prepared.closure.nodes() {
        let program = prepared
            .frozen_programs
            .get(&node.sequence_id())
            .ok_or_else(|| invalid("active node lost its original frozen program"))?;
        let request = node.evaluation().execution_request();
        let plan = if let Some(input) = node.clip_effect_input() {
            let input = input.with_program(program).map_err(|error| invalid(error.to_string()))?;
            graph
                .scratch
                .borrow_mut()
                .prepare_clip_effect_input_frame_execution(program, request.clone(), &input)
                .map_err(|error| invalid(error.to_string()))?
                .execution_plan()
                .clone()
        } else if let Some(target) = request.auxiliary_clip() {
            let Some((PreparedVisualInstanceStep::Auxiliary(key), parent_path)) =
                node.instance_path().split_last()
            else {
                return Err(invalid("standalone Clip lost its frozen auxiliary path"));
            };
            let PreparedVisualAuxiliaryAddress::Authored(VisualSourceReference::Clip {
                clip_id,
                retime,
            }) = key.address
            else {
                return Err(invalid(
                    "standalone Clip has a different frozen source type",
                ));
            };
            let schedule = program.schedule();
            let sample = match key.placement {
                Some(consumer) => schedule.evaluate_auxiliary_clip(consumer, clip_id, retime),
                None => {
                    let parent = prepared
                        .closure
                        .nodes()
                        .iter()
                        .find(|candidate| candidate.instance_path() == parent_path)
                        .ok_or_else(|| invalid("standalone Clip lost its Timeline Grade owner"))?;
                    schedule.evaluate_auxiliary_clip_for_grade(parent.time(), clip_id, retime)
                }
            }
            .map_err(|error| invalid(error.to_string()))?;
            if sample.placement() != target {
                return Err(invalid("standalone Clip changed its original sample"));
            }
            graph
                .scratch
                .borrow_mut()
                .prepare_auxiliary_clip_frame_execution(program, request.clone(), &sample)
                .map_err(|error| invalid(error.to_string()))?
                .execution_plan()
                .clone()
        } else {
            let frame = graph
                .scratch
                .borrow_mut()
                .prepare_timeline_frame_execution(program, request.clone())
                .map_err(|error| invalid(error.to_string()))?;
            mondrian_renderer::PreparedVisualFrameEvaluation::new(frame.clone(), ())
                .visit_temporal_auxiliary_demands(
                    |key, _, _| {
                        if let Some(query) = temporal_preview_asset_request(node, key)
                            .map_err(PreviewTimelineAbort::Unavailable)?
                        {
                            if seen.insert(query.clone()) {
                                requests.push(PreparedPreviewSourceDemand::Auxiliary(query));
                            }
                        }
                        Ok(())
                    },
                    |demand| {
                        invalid(format!(
                            "temporal auxiliary slot {} at {:?} has no original binding",
                            demand.slot(),
                            demand.context_time()
                        ))
                    },
                )?;
            frame.execution_plan().clone()
        };
        visit_graphs(&plan, |_, graph| {
            for source in graph.auxiliary_source_bindings() {
                if !matches!(source.reference(), VisualSourceReference::Asset { .. }) {
                    continue;
                }
                let request = PreviewAuxiliaryAssetRequest {
                    reference: source.reference(),
                    sequence_time: node.time(),
                    target_resolution: node.execution_resolution(),
                    input_color: node.color_context().media_input(false),
                };
                if seen.insert(request.clone()) {
                    requests.push(PreparedPreviewSourceDemand::Auxiliary(request));
                }
            }
            Ok(())
        })?;
    }
    Ok(requests)
}

fn invalid(detail: impl Into<String>) -> PreviewTimelineAbort {
    PreviewTimelineAbort::Unavailable(PreviewUnavailability::blocked(
        PreviewOutputStage::TimelineEvaluation,
        detail,
    ))
}

fn missing_references(
    queries: &[PreparedPreviewSourceDemand],
    sources: &PreviewSourcePreparation,
) -> Result<Vec<VisualSourceReference>, PreviewTimelineAbort> {
    let mut asset_states = HashMap::new();
    let mut missing = Vec::new();
    for query in queries {
        let PreparedPreviewSourceDemand::Auxiliary(query) = query else {
            continue;
        };
        let unavailable = sources.unavailable_auxiliary.contains(query);
        let VisualSourceReference::Asset { asset_id, .. } = query.reference else {
            return Err(invalid("unbound source escaped typed compilation"));
        };
        if asset_states
            .insert(asset_id, unavailable)
            .is_some_and(|previous| previous != unavailable)
        {
            return Err(PreviewTimelineAbort::Pending {
                dependency: PreviewTimelinePendingDependency::Media {
                    asset_id,
                    wait: PreviewTimelineMediaWait::RetryAdmission,
                },
            });
        }
        if unavailable && !missing.contains(&query.reference) {
            missing.push(query.reference);
        }
    }
    // Source traversal order may change when an unused nested branch retires.
    missing.sort_by_cached_key(|reference| {
        serde_json::to_vec(reference).expect("typed source serializes")
    });
    Ok(missing)
}

/// Query original definitions on every active node, then reach a stable canonical
/// closure before asking any pixel producer to run. An identity's removed slot
/// cannot erase the query that would restore that definition on the next retry.
pub(super) fn prepare_sources(
    prepared: &mut PreviewTimelinePreparation,
    graph: PreviewTimelineGraph<'_>,
    request: &PreviewTimelineFrameRequest<'_>,
    resolver: &mut Option<&mut PreviewAuxiliaryAssetResolver<'_>>,
) -> Result<Vec<PreparedPreviewSourceDemand>, PreviewTimelineAbort> {
    let preview = prepared
        .original_closure
        .nodes()
        .iter()
        .all(|node| node.program().auxiliary_preview_enabled());
    let mut queried = HashSet::new();
    for _ in 0..32 {
        if graph.cancellation.is_canceled() {
            return Err(PreviewTimelineAbort::Unavailable(
                PreviewUnavailability::stopped(
                    PreviewOutputStage::TimelineEvaluation,
                    "auxiliary projection was canceled",
                ),
            ));
        }
        let queries = if preview
            && (!prepared.unavailable_auxiliary.is_empty()
                || !prepared.unavailable_visual.is_empty())
        {
            original_queries(prepared, graph)?
        } else {
            prepared.demands.clone()
        };
        let PreviewPreparationState::Sources(sources) = &mut prepared.state else {
            unreachable!("projection precedes DSP");
        };
        refresh_prepared_auxiliary_sources(sources, &queries, resolver, preview, &mut queried)?;
        let missing = missing_references(&queries, sources)?;
        let mut unavailable_visual = prepared.unavailable_visual.clone();
        let visual_changed = preview
            && record_unavailable_visual_sources(&prepared.closure, &mut unavailable_visual);
        if missing != prepared.unavailable_auxiliary || visual_changed {
            let projection =
                SourceProjection::new(prepared, graph, missing.clone(), unavailable_visual.clone());
            let closure = prepare_preview_frame_closure_with_projection(
                graph,
                request.root_sequence,
                request.sequences,
                request.frame.max(0),
                request.target_resolution,
                request.color_context.clone(),
                request.runtime_scale,
                request.raster_requirement,
                Some(&projection),
            )
            .map_err(PreviewTimelineAbort::Unavailable)?;
            prepared.frozen_programs = projection.programs.into_inner();
            prepared.closure = Arc::new(closure);
            prepared.unavailable_auxiliary = missing;
            prepared.unavailable_visual = unavailable_visual;
            continue;
        }
        let active = if prepared.unavailable_auxiliary.is_empty()
            && prepared.unavailable_visual.is_empty()
        {
            prepared.demands.clone()
        } else {
            collect_prepared_visual_source_demands(&prepared.closure)
                .map_err(PreviewTimelineAbort::Unavailable)?
        };
        let current: HashSet<_> = queries
            .iter()
            .filter_map(|demand| match demand {
                PreparedPreviewSourceDemand::Auxiliary(request) => Some(request),
                _ => None,
            })
            .collect();
        if let PreviewPreparationState::Sources(sources) = &mut prepared.state {
            sources.auxiliary.retain(|request, _| current.contains(request));
            sources.unavailable_auxiliary.retain(|request| current.contains(request));
        }
        prepared.projection_context = context(prepared, &queries, &active, preview)?;
        return Ok(active);
    }
    Err(invalid(
        "auxiliary source projection did not converge within its metadata admission bound",
    ))
}

fn context(
    prepared: &PreviewTimelinePreparation,
    queries: &[PreparedPreviewSourceDemand],
    active: &[PreparedPreviewSourceDemand],
    preview: bool,
) -> Result<Option<Arc<PreviewAuxiliaryProjectionContext>>, PreviewTimelineAbort> {
    let PreviewPreparationState::Sources(sources) = &prepared.state else {
        unreachable!()
    };
    let mut notices = Vec::new();
    let mut seen_notices = HashSet::new();
    let mut programs = Vec::new();
    let mut seen_programs = HashSet::new();
    for node in prepared.closure.nodes() {
        if prepared
            .frozen_programs
            .get(&node.sequence_id())
            .is_some_and(|original| !Arc::ptr_eq(original, node.program()))
            && seen_programs.insert(Arc::as_ptr(node.program()))
        {
            programs.push(Arc::clone(node.program()));
        }
        let mut add = |placement, graph: &Arc<CompiledEffectGraph>| {
            for stage in graph.stage_bindings() {
                if let Some(fallback) = stage.auxiliary_preview_fallback() {
                    let notice = PreviewAuxiliaryNotice {
                        sequence_id: node.sequence_id(),
                        time: node.time(),
                        path: node.instance_path().into(),
                        placement,
                        stage_index: stage.stage_index(),
                        location: node
                            .program()
                            .auxiliary_consumer_location(placement, stage)
                            .ok_or_else(|| {
                                invalid(
                                    "auxiliary advisory is missing its authored consumer location",
                                )
                            })?,
                        fallback: fallback.clone(),
                    };
                    if seen_notices.insert(notice.clone()) {
                        notices.push(notice);
                    }
                }
            }
            Ok(())
        };
        visit_graphs(node.evaluation().plan(), &mut add)?;
        for batch in node.evaluation().temporal_batches() {
            add(Some(batch.placement()), batch.graph())?;
        }
        for input in node.evaluation().temporal_media_inputs() {
            add(Some(input.media().placement), &input.media().effect_graph)?;
        }
    }
    let unavailable_queries = queries
        .iter()
        .filter_map(|query| match query {
            PreparedPreviewSourceDemand::Auxiliary(request)
                if sources.unavailable_auxiliary.contains(request) =>
            {
                Some(request.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    // Nested nodes become single working frames before root cache insertion.
    // Preserve their source facts after the child's pixel bindings are gone.
    let mut available_queries = Vec::new();
    if preview || prepared.closure.nodes().len() > 1 {
        let mut seen = HashSet::new();
        for demand in active {
            if let PreparedPreviewSourceDemand::Auxiliary(request) = demand
                && seen.insert(request)
            {
                let plan = sources.auxiliary.get(request).ok_or_else(|| {
                    invalid("active auxiliary source is missing its resolved plan")
                })?;
                available_queries.push((request.clone(), plan.clone()));
            }
        }
    }
    if notices.is_empty()
        && unavailable_queries.is_empty()
        && available_queries.is_empty()
        && programs.is_empty()
    {
        return Ok(None);
    }
    let bytes = programs
        .iter()
        .map(|program| program.retained_bytes_estimate())
        .fold(
            std::mem::size_of::<PreviewAuxiliaryProjectionContext>(),
            usize::saturating_add,
        )
        .saturating_add(
            notices
                .iter()
                .map(|notice| {
                    std::mem::size_of::<PreviewAuxiliaryNotice>()
                        .saturating_add(
                            notice
                                .path
                                .len()
                                .saturating_mul(std::mem::size_of::<PreparedVisualInstanceStep>()),
                        )
                        .saturating_add(notice.fallback.parameter_id().storage_owner().capacity())
                })
                .fold(0usize, usize::saturating_add),
        )
        .saturating_add(
            unavailable_queries
                .iter()
                .map(|query| {
                    std::mem::size_of::<PreviewAuxiliaryAssetRequest>()
                        .saturating_add(format!("{query:?}").len().saturating_mul(4))
                })
                .fold(0usize, usize::saturating_add),
        )
        .saturating_add(
            available_queries
                .iter()
                .map(|entry| {
                    std::mem::size_of::<(PreviewAuxiliaryAssetRequest, PreviewAuxiliaryAssetPlan)>()
                        .saturating_add(format!("{entry:?}").len().saturating_mul(4))
                })
                .fold(0usize, usize::saturating_add),
        );
    Ok(Some(Arc::new(PreviewAuxiliaryProjectionContext {
        notices: notices.into(),
        unavailable_queries: unavailable_queries.into(),
        available_queries: available_queries.into(),
        _programs: programs.into(),
        bytes,
        reservation: OnceLock::new(),
    })))
}
