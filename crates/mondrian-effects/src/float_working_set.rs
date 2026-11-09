//! Cold scalar raster ownership, including differently sized auxiliary values.

use std::collections::HashMap;

use crate::{
    CompiledEffectGraph, EffectFloatExecutionError, EffectFloatFrameError, EffectFrameExtent,
    EffectGraphNodeId, EffectGraphNodeKind,
};

#[derive(Clone, Copy)]
struct Raster {
    extent: EffectFrameExtent,
    capacity: usize,
    borrowed: bool,
}

fn bytes(extent: EffectFrameExtent) -> Result<usize, EffectFloatExecutionError> {
    (extent.width() as usize)
        .checked_mul(extent.height() as usize)
        .and_then(|pixels| pixels.checked_mul(std::mem::size_of::<[f32; 4]>()))
        .ok_or_else(|| EffectFloatFrameError::ExtentOverflow.into())
}

#[derive(Default)]
struct Pool {
    capacities: Vec<usize>,
    allocated: usize,
}

impl Pool {
    fn acquire(&mut self, required: usize, reuse: bool) -> usize {
        let previous = if reuse {
            self.capacities.pop().unwrap_or(0)
        } else {
            0
        };
        // The executor discards an undersized slot rather than growing it
        // geometrically; a large authored map must not inflate every clone.
        let capacity = previous.max(required);
        self.allocated = self.allocated.saturating_add(capacity.saturating_sub(previous));
        capacity
    }

    fn release(&mut self, raster: Raster) {
        if !raster.borrowed {
            self.capacities.push(raster.capacity);
        }
    }
}

/// Raster reservations at each scalar node. Borrowed input rasters and cache
/// retention belong to their owners and are excluded; native scratch is included.
pub(crate) fn reservations(
    compiled: &CompiledEffectGraph,
    primary_extent: EffectFrameExtent,
    auxiliary: &[(u32, EffectFrameExtent)],
    control: &mut crate::EffectProcessorControl<'_>,
) -> Result<Vec<(usize, EffectFrameExtent)>, EffectFloatExecutionError> {
    compiled.output_raster_bounds(primary_extent)?;
    let mut outputs = HashMap::<EffectGraphNodeId, Raster>::new();
    let mut remaining = compiled.node_use_counts().clone();
    let mut pool = Pool::default();
    let mut result = Vec::with_capacity(compiled.schedule().ordered_nodes.len());
    for id in &compiled.schedule().ordered_nodes {
        control
            .checkpoint()
            .map_err(|reason| EffectFloatExecutionError::Stopped { reason })?;
        let missing = |node_id| EffectFloatExecutionError::MissingOutput { node_id };
        let node = compiled.graph().node(*id).ok_or_else(|| missing(*id))?;
        let mut taken = Vec::<Raster>::new();
        let sequential = matches!(node.kind, EffectGraphNodeKind::MultiInput { .. });
        let pixelwise = matches!(
            node.kind,
            EffectGraphNodeKind::Blend { .. }
                | EffectGraphNodeKind::Mask { .. }
                | EffectGraphNodeKind::MaskCombine { .. }
                | EffectGraphNodeKind::MatteMix { .. }
                | EffectGraphNodeKind::MultiInput { .. }
        );
        for input in node.input_ids() {
            let uses = remaining.get_mut(&input).ok_or_else(|| missing(input))?;
            *uses = uses.checked_sub(1).ok_or_else(|| missing(input))?;
            let raster = if *uses == 0 {
                outputs.remove(&input).ok_or_else(|| missing(input))?
            } else {
                let raster = outputs.get(&input).copied().ok_or_else(|| missing(input))?;
                if raster.borrowed {
                    raster
                } else {
                    Raster {
                        capacity: pool.acquire(bytes(raster.extent)?, true),
                        ..raster
                    }
                }
            };
            if pixelwise
                && let Some(first) = taken.first()
                && first.extent != raster.extent
            {
                return Err(EffectFloatFrameError::GridExtentMismatch {
                    expected: first.extent,
                    actual: raster.extent,
                }
                .into());
            }
            if sequential && !taken.is_empty() {
                pool.release(raster);
            } else {
                taken.push(raster);
            }
        }
        let outset = match &node.kind {
            EffectGraphNodeKind::AuxiliaryEffect {
                op: crate::EffectRenderOp::DisplacementMap { grade },
                ..
            } if !primary_extent.is_empty() => grade.output_outset(),
            _ => [0, 0],
        };
        let grows = outset != [0, 0];
        let (raster, scratch) = match &node.kind {
            EffectGraphNodeKind::Source => (
                Raster {
                    extent: primary_extent,
                    capacity: pool.acquire(bytes(primary_extent)?, true),
                    borrowed: false,
                },
                0,
            ),
            EffectGraphNodeKind::CustomSource { op, .. } => (
                Raster {
                    extent: primary_extent,
                    capacity: pool.acquire(bytes(primary_extent)?, true),
                    borrowed: false,
                },
                crate::adjustment::render_op_f32_scratch_frames(op),
            ),
            EffectGraphNodeKind::ExternalInput { slot, .. } => {
                let extent = auxiliary
                    .iter()
                    .find(|(candidate, _)| candidate == slot)
                    .ok_or(EffectFloatExecutionError::AuxiliaryInput(
                        crate::EffectExecutionError::AuxiliaryFrameMissing { slot: *slot },
                    ))?
                    .1;
                let borrowed = compiled.external_auxiliary_is_read_only(*id);
                (
                    Raster {
                        extent,
                        capacity: if borrowed {
                            0
                        } else {
                            pool.acquire(bytes(extent)?, false)
                        },
                        borrowed,
                    },
                    0,
                )
            }
            EffectGraphNodeKind::MaskSource { .. } => (
                Raster {
                    extent: primary_extent,
                    capacity: pool.acquire(bytes(primary_extent)?, false),
                    borrowed: false,
                },
                0,
            ),
            EffectGraphNodeKind::UnaryEffect { op, .. }
            | EffectGraphNodeKind::DomainEffect { op, .. } => (
                *taken.first().ok_or_else(|| missing(*id))?,
                crate::adjustment::render_op_f32_scratch_frames(op),
            ),
            EffectGraphNodeKind::AuxiliaryEffect { .. } if grows => {
                let input = taken.first().ok_or_else(|| missing(*id))?;
                let extent = crate::EffectRasterBounds::from_extent(input.extent)
                    .checked_outset(outset)
                    .ok_or(EffectFloatFrameError::ExtentOverflow)?
                    .extent();
                // Growth writes a disjoint output while borrowing the primary
                // allocation as its premultiplied read plane.
                (
                    Raster {
                        extent,
                        capacity: pool.acquire(bytes(extent)?, false),
                        borrowed: false,
                    },
                    0,
                )
            }
            EffectGraphNodeKind::AuxiliaryEffect { op, .. } => (
                *taken.first().ok_or_else(|| missing(*id))?,
                crate::adjustment::render_op_f32_scratch_frames(op),
            ),
            EffectGraphNodeKind::Blend { .. }
            | EffectGraphNodeKind::Mask { .. }
            | EffectGraphNodeKind::MaskCombine { .. }
            | EffectGraphNodeKind::MatteMix { .. }
            | EffectGraphNodeKind::MultiInput { .. } => {
                (*taken.first().ok_or_else(|| missing(*id))?, 0)
            }
        };
        let mask_bytes = if matches!(node.kind, EffectGraphNodeKind::MaskSource { .. }) {
            crate::mask_raster::PreparedMaskRasterSet::required_retained_bytes(compiled.graph())
                .and_then(|retained| {
                    crate::mask_raster::PreparedMaskRasterSet::required_max_scratch_bytes(
                        compiled.graph(),
                    )
                    .map(|scratch| retained.saturating_add(scratch))
                })
                .map_err(|source| EffectFloatExecutionError::MaskRasterFailed {
                    node_id: *id,
                    source,
                })?
        } else {
            0
        };
        let auxiliary_scratch = match &node.kind {
            EffectGraphNodeKind::AuxiliaryEffect { op, .. } => {
                bytes(taken.get(1).ok_or_else(|| missing(*id))?.extent)?
                    .saturating_mul(crate::auxiliary::render_op_auxiliary_scratch_frames(op))
            }
            _ => 0,
        };
        result.push((
            pool.allocated
                .saturating_add(bytes(raster.extent)?.saturating_mul(scratch))
                .saturating_add(auxiliary_scratch)
                .saturating_add(mask_bytes),
            raster.extent,
        ));
        for overlay in taken.into_iter().skip(usize::from(!grows)) {
            pool.release(overlay);
        }
        outputs.insert(*id, raster);
    }
    Ok(result)
}

/// Estimate the cold Float32 scalar raster peak without allocating pixels.
///
/// Extents must describe every required auxiliary slot exactly once. This
/// includes retained pool storage, native primary/auxiliary scratch, and mask
/// raster workspace. Borrowed host rasters, optional output-cache retention,
/// and provider-owned processor state have separate owners and grants.
pub fn estimate_compiled_float_raster_working_bytes(
    compiled: &CompiledEffectGraph,
    primary_extent: EffectFrameExtent,
    auxiliary_extents: &[(u32, EffectFrameExtent)],
) -> Result<usize, EffectFloatExecutionError> {
    let invalid = |slot, reason| {
        EffectFloatExecutionError::AuxiliaryInput(
            crate::EffectExecutionError::InvalidAuxiliaryFrame { slot, reason },
        )
    };
    for (slot, extent) in auxiliary_extents {
        if !compiled.auxiliary_inputs().iter().any(|spec| spec.slot() == *slot) {
            return Err(invalid(*slot, "undeclared_slot"));
        }
        if extent.is_empty() {
            return Err(invalid(*slot, "empty_extent"));
        }
        if auxiliary_extents.iter().filter(|(candidate, _)| candidate == slot).count() != 1 {
            return Err(invalid(*slot, "duplicate_slot"));
        }
    }
    Ok(reservations(
        compiled,
        primary_extent,
        auxiliary_extents,
        &mut crate::EffectProcessorControl::uncancelled(),
    )?
    .into_iter()
    .map(|(occupied, _)| occupied)
    .max()
    .unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use crate::*;
    use mondrian_core::{ColorFrameAlpha, ImageSampling};

    fn frame(pixels: Vec<[f32; 4]>, width: u32) -> EffectFloatFrame {
        EffectFloatFrame::from_pixels(
            EffectFrameExtent::new(width, 1),
            EffectValueFormat::new(
                EffectWorkingPrecision::Float32,
                EffectColorDomain::SceneLinearRgb,
                ColorFrameAlpha::StraightCoverage,
            ),
            ImageSampling::UNIT_SQUARE,
            pixels,
        )
        .unwrap()
    }

    fn graph(repetitions: usize, preprocess: bool) -> std::sync::Arc<CompiledEffectGraph> {
        let mut builder = EffectGraphBuilderState::new();
        let mut primary = builder.current_output();
        let external = builder.append_external_input(
            0,
            frame(vec![], 0).format(),
            EffectColorDomainContract::SCENE_LINEAR,
        );
        let auxiliary = if preprocess {
            // The raw slot has its explicit host-consuming edge. Another
            // graph-local branch preprocesses and shares the authored raster.
            primary = builder.add_auxiliary_effect_with_role(
                primary,
                external,
                EffectRenderOp::Fill {
                    grade: FillGrade::new(0.0, 0.0, false, [1.0, 0.0, 0.0], 1.0).unwrap(),
                },
                AuxiliaryRole::Matte,
            );
            builder.set_current_output(external);
            builder.append_unary(EffectRenderOp::GaussianBlur { radius: 1.0 });
            builder.current_output()
        } else {
            external
        };
        builder.set_current_output(primary);
        for _ in 0..repetitions {
            let output = builder.add_auxiliary_effect_with_role(
                builder.current_output(),
                auxiliary,
                EffectRenderOp::Fill {
                    grade: FillGrade::new(0.0, 0.0, false, [1.0, 0.0, 0.0], 1.0).unwrap(),
                },
                AuxiliaryRole::Matte,
            );
            builder.set_current_output(output);
        }
        compile_reference_render_graph(builder.finish()).unwrap()
    }

    fn execute(
        graph: &CompiledEffectGraph,
        matte: &EffectFloatFrame,
        budget: usize,
    ) -> Result<EffectFloatFrame, EffectFloatExecutionError> {
        let primary = frame(vec![[0.0, 0.0, 2.0, 1.0]], 1);
        EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(budget))
            .apply_compiled_rgba_f32_with_auxiliary(
                primary.view(),
                graph,
                0,
                &[AuxiliaryFloatInput {
                    slot: 0,
                    raster: AuxiliaryFloatFrame::new(
                        matte.view(),
                        AuxiliaryPolicy::new(AuxiliaryResolution::Authored),
                    ),
                }],
                &mut EffectProcessorControl::uncancelled(),
            )
    }

    #[test]
    fn borrowed_authored_auxiliary_has_only_the_primary_pixel_budget() {
        let graph = graph(1, false);
        let matte = frame(vec![[0.0, 0.0, 0.0, 0.25]; 4], 4);
        // The four authored pixels stay in the caller's plane.
        assert_eq!(
            execute(&graph, &matte, 16).unwrap().pixels(),
            &[[0.25, 0.0, 1.5, 1.0]]
        );
        assert!(matches!(
            execute(&graph, &matte, 15),
            Err(EffectFloatExecutionError::AuxiliaryInput(
                EffectExecutionError::AuxiliaryWorkingSetBudgetExceeded {
                    required_bytes: 16,
                    budget_bytes: 15,
                }
            ))
        ));
    }

    #[test]
    fn shared_authored_rasters_are_borrowed_by_each_consumer() {
        let graph = graph(40, false);
        let matte = frame(vec![[0.0, 0.0, 0.0, 0.25]; 4], 4);
        let output = execute(&graph, &matte, 16).unwrap();
        let residual = 0.75f32.powi(40);
        assert!((output.pixels()[0][0] - (1.0 - residual)).abs() < 1e-6);
        assert!((output.pixels()[0][2] - 2.0 * residual).abs() < 1e-6);
        assert!(matches!(
            execute(&graph, &matte, 15),
            Err(EffectFloatExecutionError::AuxiliaryInput(
                EffectExecutionError::AuxiliaryWorkingSetBudgetExceeded { required_bytes: 16, .. }
            ))
        ));
    }

    #[test]
    fn two_external_slots_can_borrow_the_same_authored_plane() {
        let matte = frame(vec![[0.0, 0.0, 0.0, 0.25]; 4], 4);
        let primary = frame(vec![[0.0, 0.0, 2.0, 1.0]], 1);
        let mut builder = EffectGraphBuilderState::new();
        for slot in [3, 19] {
            let input = builder.current_output();
            let auxiliary = builder.append_external_input(
                slot,
                matte.format(),
                EffectColorDomainContract::SCENE_LINEAR,
            );
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
        let graph = compile_reference_render_graph(builder.finish()).unwrap();
        let raster = AuxiliaryFloatFrame::new(
            matte.view(),
            AuxiliaryPolicy::new(AuxiliaryResolution::Authored),
        );
        let inputs = [3, 19].map(|slot| AuxiliaryFloatInput { slot, raster });
        let output = EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(16))
            .apply_compiled_rgba_f32_with_auxiliary(
                primary.view(),
                &graph,
                0,
                &inputs,
                &mut EffectProcessorControl::uncancelled(),
            )
            .unwrap();
        assert_eq!(output.pixels(), &[[0.4375, 0.0, 1.125, 1.0]]);
        assert!(matte.pixels().iter().all(|pixel| *pixel == [0.0, 0.0, 0.0, 0.25]));
    }

    #[test]
    fn a_preprocessed_larger_auxiliary_uses_its_own_kernel_and_clone_extent() {
        let matte = frame(
            vec![
                [0.0, 0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0, 0.2],
                [0.0, 0.0, 0.0, 0.8],
                [0.0, 0.0, 0.0, 0.2],
            ],
            4,
        );
        let mut blur = EffectGraphBuilderState::new();
        blur.append_unary(EffectRenderOp::GaussianBlur { radius: 1.0 });
        let blur = compile_reference_render_graph(blur.finish()).unwrap();
        let reference = apply_compiled_effect_graph_rgba_f32(matte.view(), &blur, 0).unwrap();
        let alpha_at_centre = |image: &EffectFloatFrame| {
            sample_auxiliary_f32(
                AuxiliaryFloatFrame::new(
                    image.view(),
                    AuxiliaryPolicy::new(AuxiliaryResolution::Authored),
                ),
                [0.5, 0.5],
                AuxiliaryRole::Matte.default_sampling(),
            )[3]
        };
        let residual =
            (1.0 - alpha_at_centre(&matte)) * (1.0 - alpha_at_centre(&reference)).powi(2);
        let graph = graph(2, true);
        let extent = EffectFrameExtent::new(1, 1);
        let peak =
            estimate_compiled_float_raster_working_bytes(&graph, extent, &[(0, matte.extent())])
                .unwrap();
        assert!(
            peak >= 16 + 2 * 64,
            "preprocessing owns the large auxiliary raster"
        );
        assert!(matches!(
            execute(&graph, &matte, peak - 1),
            Err(EffectFloatExecutionError::AuxiliaryInput(
                EffectExecutionError::AuxiliaryWorkingSetBudgetExceeded { required_bytes, .. }
            )) if required_bytes == peak
        ));
        let actual = execute(&graph, &matte, peak).unwrap();
        assert!((actual.pixels()[0][0] - (1.0 - residual)).abs() < 1e-6);
        assert!((actual.pixels()[0][2] - 2.0 * residual).abs() < 1e-6);
    }

    #[test]
    fn large_auxiliary_estimates_are_decode_free_and_keep_primary_and_map_scratch_separate() {
        let primary = EffectFrameExtent::new(1920, 1080);
        let authored = EffectFrameExtent::new(8192, 4320);
        let owned = graph(40, true);
        let graph = graph(40, false);
        let peak = estimate_compiled_float_raster_working_bytes(&graph, primary, &[(0, authored)])
            .unwrap();
        assert_eq!(peak, 1920 * 1080 * 16);
        let owned_peak =
            estimate_compiled_float_raster_working_bytes(&owned, primary, &[(0, authored)])
                .unwrap();
        assert!(owned_peak > 512 * 1024 * 1024);
        for (extents, expected_reason) in [
            (vec![(0, authored), (0, authored)], "duplicate_slot"),
            (vec![(42, authored)], "undeclared_slot"),
            (vec![(0, EffectFrameExtent::new(0, 0))], "empty_extent"),
        ] {
            assert!(matches!(
                estimate_compiled_float_raster_working_bytes(&graph, primary, &extents),
                Err(EffectFloatExecutionError::AuxiliaryInput(
                    EffectExecutionError::InvalidAuxiliaryFrame { reason, .. }
                )) if reason == expected_reason
            ));
        }

        let mut builder = EffectGraphBuilderState::new();
        let input = builder.current_output();
        let auxiliary = builder.append_external_input(
            0,
            frame(vec![], 0).format(),
            EffectColorDomainContract::SCENE_LINEAR,
        );
        let output = builder.add_auxiliary_effect_with_role(
            input,
            auxiliary,
            EffectRenderOp::DisplacementMap {
                grade: DisplacementMapGrade::new(
                    [1.0, 0.0],
                    [DisplacementMapChannel::Red, DisplacementMapChannel::Green],
                    DisplacementMapBehavior::Center,
                    false,
                    false,
                )
                .unwrap(),
            },
            AuxiliaryRole::Map,
        );
        builder.set_current_output(output);
        let graph = compile_reference_render_graph(builder.finish()).unwrap();
        let peak = estimate_compiled_float_raster_working_bytes(&graph, primary, &[(0, authored)])
            .unwrap();
        // Displacement snapshots the primary and reads the host's map in place.
        assert_eq!(peak, 2 * 1920 * 1080 * 16);
    }

    #[test]
    fn optional_auxiliary_output_cache_copies_cannot_raise_the_admitted_raster_peak() {
        let graph = graph(40, false);
        assert!(graph.output_cache_enabled());
        let primary = frame(vec![[0.0, 0.0, 2.0, 1.0]; 8], 8);
        let matte = frame(vec![[0.0, 0.0, 0.0, 0.25]], 1);
        let rasters = [AuxiliaryFloatInput {
            slot: 0,
            raster: AuxiliaryFloatFrame::new(
                matte.view(),
                AuxiliaryPolicy::new(AuxiliaryResolution::Authored),
            ),
        }];
        let peak = estimate_compiled_float_raster_working_bytes(
            &graph,
            primary.extent(),
            &[(0, matte.extent())],
        )
        .unwrap();
        assert_eq!(peak, 128);
        assert!(primary.retained_bytes() * 2 > peak);
        // A separate compositor can grant exactly this cold peak even when
        // the Effect session has a larger local grant and a usable cache.
        let mut session = EffectExecutionSession::default();
        for _ in 0..2 {
            let output = session
                .apply_compiled_rgba_f32_with_auxiliary(
                    primary.view(),
                    &graph,
                    0,
                    &rasters,
                    &mut EffectProcessorControl::uncancelled(),
                )
                .unwrap();
            assert!(output.pixels().iter().all(|pixel| pixel[0] > 0.99));
            assert_eq!(session.diagnostics().cache_entries, 0);
        }
    }

    #[test]
    fn pixelwise_mask_joins_refuse_different_auxiliary_grids_instead_of_zipping_a_prefix() {
        for (primary_extent, auxiliary_extent) in [
            (EffectFrameExtent::new(1, 1), EffectFrameExtent::new(4, 1)),
            (EffectFrameExtent::new(2, 1), EffectFrameExtent::new(1, 2)),
        ] {
            let primary_pixels = vec![
                [0.0, 0.0, 2.0, 1.0];
                (primary_extent.width() * primary_extent.height()) as usize
            ];
            let auxiliary_pixels = vec![
                [0.0, 0.0, 0.0, 0.25];
                (auxiliary_extent.width() * auxiliary_extent.height())
                    as usize
            ];
            for join in 0..3 {
                let mut builder = EffectGraphBuilderState::new();
                let base = builder.current_output();
                let mask_format =
                    frame(vec![], 0).format().with_domain(EffectColorDomain::AlphaMask);
                let auxiliary = builder.append_external_input(
                    0,
                    mask_format,
                    EffectColorDomainContract::preserving(EffectColorDomain::AlphaMask),
                );
                let graded = builder.add_auxiliary_effect_with_role(
                    base,
                    auxiliary,
                    EffectRenderOp::Fill {
                        grade: FillGrade::new(0.0, 0.0, false, [1.0, 0.0, 0.0], 1.0).unwrap(),
                    },
                    AuxiliaryRole::Matte,
                );
                let output = match join {
                    0 => builder.add_mask(graded, auxiliary, false, crate::mask::MaskOp::Add),
                    1 => {
                        let local = builder.add_mask_source(
                            crate::mask::MaskShape::Rectangle {
                                x: 0.0,
                                y: 0.0,
                                width: 1.0,
                                height: 1.0,
                                corner_radius: 0.0,
                            },
                            0.0,
                            0.0,
                            1.0,
                        );
                        let combined =
                            builder.add_mask_combine(auxiliary, local, crate::mask::MaskOp::Add);
                        builder.add_mask(graded, combined, false, crate::mask::MaskOp::Add)
                    }
                    2 => builder.add_matte_mix(base, graded, auxiliary),
                    _ => unreachable!(),
                };
                builder.set_current_output(output);
                let graph = compile_reference_render_graph(builder.finish()).unwrap();
                assert!(
                    matches!(
                        estimate_compiled_float_raster_working_bytes(
                            &graph,
                            primary_extent,
                            &[(0, auxiliary_extent)],
                        ),
                        Err(EffectFloatExecutionError::FrameContract(
                            EffectFloatFrameError::GridExtentMismatch { .. }
                        ))
                    ),
                    "grid refusal must be possible before allocating pixels or invoking a provider"
                );
                let primary = EffectFloatFrameView::new(
                    primary_extent,
                    frame(vec![], 0).format(),
                    ImageSampling::UNIT_SQUARE,
                    &primary_pixels,
                );
                let rasters = [AuxiliaryFloatInput {
                    slot: 0,
                    raster: AuxiliaryFloatFrame::new(
                        EffectFloatFrameView::new(
                            auxiliary_extent,
                            mask_format,
                            ImageSampling::UNIT_SQUARE,
                            &auxiliary_pixels,
                        ),
                        AuxiliaryPolicy::new(AuxiliaryResolution::Authored),
                    ),
                }];
                let result = EffectExecutionSession::default()
                    .apply_compiled_rgba_f32_with_auxiliary(
                        primary,
                        &graph,
                        0,
                        &rasters,
                        &mut EffectProcessorControl::uncancelled(),
                    );
                assert!(matches!(result,
                    Err(EffectFloatExecutionError::FrameContract(EffectFloatFrameError::GridExtentMismatch { .. }))
                ), "join={join}, primary={primary_extent:?}, auxiliary={auxiliary_extent:?}: {result:?}");
            }
        }
    }
}
