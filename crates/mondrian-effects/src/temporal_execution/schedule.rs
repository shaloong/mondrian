//! Pure working-set proof and deterministic tiling for temporal scalar work.

use super::*;

pub(super) fn temporal_scalar_required_bytes(
    temporal_program: &PreparedTemporalValueProgram,
    demand: &EffectExecutionDemand,
    mask_rasters: &HashMap<usize, Arc<PreparedMaskRasterSet>>,
) -> Result<usize, EffectTemporalExecutionError> {
    let frame_bytes = checked_pixel_count(demand.input_roi().region())
        .and_then(|pixels| pixels.checked_mul(std::mem::size_of::<[f32; 4]>()))
        .ok_or(EffectTemporalExecutionError::WorkingSetBudgetExceeded {
            required_bytes: usize::MAX,
            budget_bytes: usize::MAX,
        })?;
    let output_bytes = checked_pixel_count(demand.output_roi())
        .and_then(|pixels| pixels.checked_mul(std::mem::size_of::<[f32; 4]>()))
        .ok_or(EffectTemporalExecutionError::WorkingSetBudgetExceeded {
            required_bytes: usize::MAX,
            budget_bytes: usize::MAX,
        })?;
    let mut working = ScalarWorkingSet::new(usize::MAX, frame_bytes);
    let mut live = HashSet::with_capacity(temporal_program.ordered_values().len());
    let mut remaining_uses = temporal_program.use_counts().clone();

    for address in temporal_program.ordered_values() {
        match *address {
            TemporalValueAddress::Source(_) => working.reserve_frame()?,
            TemporalValueAddress::Graph { context, node_id } => {
                let context_ref = temporal_program.context(context)?;
                let node = context_ref.graph.graph().node(node_id).ok_or(
                    EffectTemporalExecutionError::UnsupportedGraphNode { node_id, kind: "missing" },
                )?;
                match &node.kind {
                    EffectGraphNodeKind::Source => {
                        return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
                            reason: "source node was not normalized in the time-expanded plan",
                        });
                    }
                    EffectGraphNodeKind::UnaryEffect { input, op }
                    | EffectGraphNodeKind::DomainEffect { input, op, .. } => {
                        let input = temporal_program.address_for_input(context, *input)?;
                        plan_take_value(input, &mut live, &mut remaining_uses, &mut working)?;
                        if matches!(op, crate::EffectRenderOp::TemporalFrameBlend { .. }) {
                            let sample = temporal_program.temporal_sample(*address).ok_or(
                                EffectTemporalExecutionError::InvalidGraphLiveness {
                                    reason: "temporal value has no planned sample edge",
                                },
                            )?;
                            plan_take_value(sample, &mut live, &mut remaining_uses, &mut working)?;
                            working.release_frame()?;
                        } else {
                            let scratch_bytes = render_op_f32_scratch_frames(op)
                                .checked_mul(frame_bytes)
                                .ok_or(EffectTemporalExecutionError::WorkingSetBudgetExceeded {
                                    required_bytes: usize::MAX,
                                    budget_bytes: usize::MAX,
                                })?;
                            working.ensure_transient(scratch_bytes)?;
                        }
                    }
                    EffectGraphNodeKind::Blend { base, overlay, .. } => {
                        let base = temporal_program.address_for_input(context, *base)?;
                        let overlay = temporal_program.address_for_input(context, *overlay)?;
                        plan_take_value(base, &mut live, &mut remaining_uses, &mut working)?;
                        plan_take_value(overlay, &mut live, &mut remaining_uses, &mut working)?;
                        working.release_frame()?;
                    }
                    EffectGraphNodeKind::MultiInput { inputs, .. } => {
                        let Some(first) = inputs.first() else {
                            return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
                                reason: "multi-input node has no inputs",
                            });
                        };
                        let first = temporal_program.address_for_input(context, *first)?;
                        plan_take_value(first, &mut live, &mut remaining_uses, &mut working)?;
                        for input in &inputs[1..] {
                            let input = temporal_program.address_for_input(context, *input)?;
                            plan_take_value(input, &mut live, &mut remaining_uses, &mut working)?;
                            working.release_frame()?;
                        }
                    }
                    EffectGraphNodeKind::Mask { input, mask, .. } => {
                        let input = temporal_program.address_for_input(context, *input)?;
                        let mask = temporal_program.address_for_input(context, *mask)?;
                        plan_take_value(input, &mut live, &mut remaining_uses, &mut working)?;
                        plan_take_value(mask, &mut live, &mut remaining_uses, &mut working)?;
                        working.release_frame()?;
                    }
                    EffectGraphNodeKind::MaskSource { .. } => {
                        working.reserve_frame()?;
                        let raster = mask_rasters
                            .get(&context)
                            .and_then(|rasters| rasters.get(node_id))
                            .ok_or(EffectTemporalExecutionError::InvalidGraphLiveness {
                                reason: "prepared Mask raster is missing for a MaskSource node",
                            })?;
                        working.ensure_transient(raster.max_scratch_bytes())?;
                    }
                }
            }
        }
        if !live.insert(*address) {
            return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
                reason: "time-expanded schedule planned one value more than once",
            });
        }
    }

    let output_id = temporal_program.root();
    if !live.remove(&output_id) {
        return Err(EffectTemporalExecutionError::MissingGraphOutput);
    }
    if !live.is_empty() || remaining_uses.values().any(|remaining| *remaining != 0) {
        return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
            reason: "time-expanded uses did not retire to one output",
        });
    }
    if working.resident_bytes != frame_bytes {
        return Err(EffectTemporalExecutionError::WorkingSetLedgerMismatch {
            expected_bytes: frame_bytes,
            actual_bytes: working.resident_bytes,
        });
    }
    working.ensure_transient(output_bytes)?;
    Ok(working.peak)
}

fn plan_take_value(
    address: TemporalValueAddress,
    live: &mut HashSet<TemporalValueAddress>,
    remaining_uses: &mut HashMap<TemporalValueAddress, usize>,
    working: &mut ScalarWorkingSet,
) -> Result<(), EffectTemporalExecutionError> {
    if !live.contains(&address) {
        return Err(missing_temporal_value(address));
    }
    let remaining = remaining_uses.get_mut(&address).ok_or(
        EffectTemporalExecutionError::InvalidGraphLiveness {
            reason: "compiled graph value has no use-count evidence",
        },
    )?;
    if *remaining == 0 {
        return Err(EffectTemporalExecutionError::InvalidGraphLiveness {
            reason: "compiled graph value was planned more often than declared",
        });
    }
    *remaining -= 1;
    if *remaining == 0 {
        live.remove(&address);
    } else {
        working.reserve_frame()?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn plan_temporal_tiles(
    compiled: &CompiledEffectGraph,
    request: &EffectTemporalExecutionRequest,
    temporal_program: &PreparedTemporalValueProgram,
    output_roi: EffectPixelRoi,
    retained_output_bytes: usize,
    tile_budget: usize,
    total_budget: usize,
    mask_rasters: &HashMap<usize, Arc<PreparedMaskRasterSet>>,
) -> Result<Vec<EffectPixelRoi>, EffectTemporalExecutionError> {
    let mut pending = vec![output_roi];
    let mut tiles = Vec::new();
    while let Some(candidate) = pending.pop() {
        temporal_cancellation_checkpoint(&request.cancellation)?;
        let tile_request = EffectTemporalExecutionRequest {
            generation: request.generation,
            continuity: request.continuity,
            output_time: request.output_time,
            output_frame_seed: request.output_frame_seed,
            frame_extent: request.frame_extent,
            output_roi: candidate,
            cancellation: request.cancellation.clone(),
        };
        let demand = compiled.plan_execution_demand(
            tile_request.output_time,
            tile_request.frame_extent,
            tile_request.output_roi,
        )?;
        let required = temporal_scalar_required_bytes(temporal_program, &demand, mask_rasters)?;
        if required <= tile_budget {
            tiles.push(candidate);
            if tiles.len() > MAX_TEMPORAL_SCALAR_TILES {
                return Err(EffectTemporalExecutionError::TileScheduleLimitExceeded {
                    limit: MAX_TEMPORAL_SCALAR_TILES,
                });
            }
            continue;
        }

        let split_x = candidate.width() > 1
            && (candidate.width() >= candidate.height() || candidate.height() <= 1);
        if split_x {
            let first_width = candidate.width() / 2;
            let second_width = candidate.width() - first_width;
            let second_x = candidate.x().checked_add(first_width).ok_or(
                EffectTemporalExecutionError::InvalidRoiProjection {
                    reason: "temporal tile x split overflowed",
                },
            )?;
            pending.push(EffectPixelRoi::new(
                second_x,
                candidate.y(),
                second_width,
                candidate.height(),
            ));
            pending.push(EffectPixelRoi::new(
                candidate.x(),
                candidate.y(),
                first_width,
                candidate.height(),
            ));
            enforce_tile_limit(tiles.len(), pending.len())?;
            continue;
        }
        if candidate.height() > 1 {
            let first_height = candidate.height() / 2;
            let second_height = candidate.height() - first_height;
            let second_y = candidate.y().checked_add(first_height).ok_or(
                EffectTemporalExecutionError::InvalidRoiProjection {
                    reason: "temporal tile y split overflowed",
                },
            )?;
            pending.push(EffectPixelRoi::new(
                candidate.x(),
                second_y,
                candidate.width(),
                second_height,
            ));
            pending.push(EffectPixelRoi::new(
                candidate.x(),
                candidate.y(),
                candidate.width(),
                first_height,
            ));
            enforce_tile_limit(tiles.len(), pending.len())?;
            continue;
        }

        let required_bytes = retained_output_bytes.saturating_add(required);
        return Err(EffectTemporalExecutionError::WorkingSetBudgetExceeded {
            required_bytes,
            budget_bytes: total_budget,
        });
    }
    Ok(tiles)
}

fn enforce_tile_limit(
    completed: usize,
    pending: usize,
) -> Result<(), EffectTemporalExecutionError> {
    if completed.saturating_add(pending) > MAX_TEMPORAL_SCALAR_TILES {
        Err(EffectTemporalExecutionError::TileScheduleLimitExceeded {
            limit: MAX_TEMPORAL_SCALAR_TILES,
        })
    } else {
        Ok(())
    }
}
