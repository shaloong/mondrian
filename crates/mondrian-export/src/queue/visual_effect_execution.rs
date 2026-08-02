//! Export-owned visual Effect route preparation and execution.
//!
//! This module keeps heterogeneous route vocabulary, conservative preflight
//! evidence, and the CPU-prefix/GPU-tail execution obligation local. The
//! enclosing queue module continues to own the export attempt, visual Session,
//! decoder, audio renderer, encoder, and publication lifecycle.

use super::service::EXPORT_HETEROGENEOUS_ROUTE_CONTRACT_LOGICAL_BYTES;
use super::{
    ExportHeterogeneousCompletionEvidence, ExportHeterogeneousGpuExecutionError,
    ExportVisualRenderSession,
};
use mondrian_core::{ExecutionCancellationToken, Resolution, SequenceId, WorkingColorSpace};
use mondrian_effects::{
    compiled_effect_graph_supports_rgba_f32_with_domain_processor, identity_compiled_effect_graph,
    CompiledEffectGraph, EffectFrameExtent, EffectGraphExecutionBudget, EffectGraphExecutionStep,
    EffectGraphNodeKind, EffectProcessingBackend, EffectRenderOp, EffectValueResidency,
    EffectWorkingPrecision, PreparedHeterogeneousEffectWork, PreparedHeterogeneousEffectWorkError,
};
use mondrian_renderer::{
    admit_timeline_render_plan_for_cpu_compositor, ColorFrameAlpha, ColorFrameDomain,
    ColorFrameEncoding, ColorFrameResidency, CpuColorFrame, HeterogeneousGpuContinuationBinding,
    HeterogeneousGpuContinuationRequest, HeterogeneousGpuResourceGrant, PreparedVisualProgram,
    TimelineRenderPlan, TimelineRenderPlanElement, TimelineTransitionInputPlan,
};
use sha2::Digest;
use sha2::Sha256;
use std::sync::Arc;

const EXPORT_HETEROGENEOUS_ROUTE_SCHEMA_VERSION: u16 = 1;
const EXPORT_HETEROGENEOUS_MAX_MATERIALIZATIONS: usize = 64;
const EXPORT_HETEROGENEOUS_MAX_STEPS: usize = 192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExportHeterogeneousPlacement {
    Media,
    BasicTitle,
    NestedSequence,
    SolidColor,
    Adjustment,
    TransitionInput,
}

impl ExportHeterogeneousPlacement {
    const fn tag(self) -> u8 {
        match self {
            Self::Media => 1,
            Self::BasicTitle => 2,
            Self::NestedSequence => 3,
            Self::SolidColor => 4,
            Self::Adjustment => 5,
            Self::TransitionInput => 6,
        }
    }

    const fn supports_current_adapter(self) -> bool {
        matches!(self, Self::Media | Self::BasicTitle | Self::NestedSequence)
    }

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Media => "media",
            Self::BasicTitle => "basic_title",
            Self::NestedSequence => "nested_sequence",
            Self::SolidColor => "solid_color",
            Self::Adjustment => "adjustment",
            Self::TransitionInput => "transition_input",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExportHeterogeneousRouteContract {
    canonical_fingerprint: [u8; 32],
    shape_fingerprint: [u8; 32],
    placement: ExportHeterogeneousPlacement,
    maximum_extent: EffectFrameExtent,
    maximum_peak_host_bytes: u64,
    maximum_peak_device_bytes: u64,
    maximum_transfer_bytes: u64,
    maximum_cpu_working_bytes: usize,
}

#[derive(Debug, Clone)]
pub(super) struct PreparedExportHeterogeneousElement {
    pub(super) element_index: usize,
    pub(super) placement: ExportHeterogeneousPlacement,
    pub(super) graph: Arc<CompiledEffectGraph>,
    pub(super) frame_seed: i64,
}

#[derive(Debug)]
pub(super) struct PreparedExportEffectFramePlan {
    pub(super) render_plan: TimelineRenderPlan,
    pub(super) heterogeneous: Vec<PreparedExportHeterogeneousElement>,
}

impl PreparedExportEffectFramePlan {
    pub(super) fn into_parts(
        self,
    ) -> (TimelineRenderPlan, Vec<PreparedExportHeterogeneousElement>) {
        (self.render_plan, self.heterogeneous)
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ExportHeterogeneousEffectError {
    #[error("export heterogeneous execution capability is unavailable: {reason}")]
    CapabilityUnavailable { reason: String },
    #[error(
        "export heterogeneous route for {placement} is valid but this placement has no production Adapter"
    )]
    UnsupportedPlacement { placement: &'static str },
    #[error(
        "export heterogeneous route-contract ledger exceeds its frozen grant: contracts={contracts}/{limit}"
    )]
    RouteContractCountExceeded { contracts: usize, limit: usize },
    #[error(
        "export heterogeneous route-contract ledger exceeds its frozen logical-byte grant: logical_bytes={required}/{limit}"
    )]
    RouteContractBytesExceeded { required: usize, limit: usize },
    #[error(
        "export heterogeneous route was not proved by frozen preflight: placement={placement} extent={width}x{height} shape={shape}"
    )]
    RouteContractNotPreflighted {
        placement: &'static str,
        width: u32,
        height: u32,
        shape: String,
    },
    #[error(
        "export heterogeneous route requires GPU execution but the attempt-local backend is unavailable"
    )]
    RequiredGpuUnavailable,
    #[error("export heterogeneous route preparation failed for {placement}: {source}")]
    Preparation {
        placement: &'static str,
        #[source]
        source: PreparedHeterogeneousEffectWorkError,
    },
    #[error("export heterogeneous CPU prefix failed for {placement}: {source}")]
    CpuPrefix {
        placement: &'static str,
        #[source]
        source: PreparedHeterogeneousEffectWorkError,
    },
    #[error("export heterogeneous GPU suffix failed for {placement}: {source}")]
    GpuContinuation {
        placement: &'static str,
        #[source]
        source: Box<ExportHeterogeneousGpuExecutionError>,
    },
    #[error("export heterogeneous completion has no proved GPU readback")]
    MissingReadbackEvidence,
    #[error(
        "export heterogeneous input violates the working-frame contract: expected={expected}, actual={actual}"
    )]
    FrameContractMismatch { expected: String, actual: String },
    #[error("export heterogeneous execution was canceled at {checkpoint}")]
    Canceled { checkpoint: &'static str },
    #[error("renderer could not prepare the identity Effect graph")]
    IdentityUnavailable,
}

fn export_heterogeneous_route_shape_fingerprint(
    graph: &CompiledEffectGraph,
    prepared: &PreparedHeterogeneousEffectWork,
) -> Result<[u8; 32], ExportHeterogeneousEffectError> {
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.export.heterogeneous-route-shape");
    hasher.update(EXPORT_HETEROGENEOUS_ROUTE_SCHEMA_VERSION.to_le_bytes());
    let plan = prepared.plan();
    hasher.update((plan.materializations().len() as u64).to_le_bytes());
    for materialization in plan.materializations() {
        hasher.update(materialization.id().get().to_le_bytes());
        hasher.update(materialization.value().0.to_le_bytes());
        hash_effect_residency(&mut hasher, materialization.residency())?;
        hasher.update(materialization.completion().get().to_le_bytes());
    }
    hasher.update((plan.steps().len() as u64).to_le_bytes());
    for step in plan.steps() {
        match step {
            EffectGraphExecutionStep::Dispatch {
                node,
                lane,
                backend,
                precision,
                inputs,
                output,
                waits,
                signal,
            } => {
                hasher.update([1]);
                hasher.update(node.0.to_le_bytes());
                hasher.update(lane.get().to_le_bytes());
                hasher.update([effect_backend_tag(*backend)]);
                hasher.update([effect_precision_tag(*precision)]);
                hasher.update((inputs.len() as u64).to_le_bytes());
                for input in inputs.iter() {
                    hasher.update(input.get().to_le_bytes());
                }
                hasher.update(output.get().to_le_bytes());
                hasher.update((waits.len() as u64).to_le_bytes());
                for wait in waits.iter() {
                    hasher.update(wait.get().to_le_bytes());
                }
                hasher.update(signal.get().to_le_bytes());
            }
            EffectGraphExecutionStep::Transfer { value, input, output, from, to, wait, signal } => {
                hasher.update([2]);
                hasher.update(value.0.to_le_bytes());
                hasher.update(input.get().to_le_bytes());
                hasher.update(output.get().to_le_bytes());
                hash_effect_residency(&mut hasher, *from)?;
                hash_effect_residency(&mut hasher, *to)?;
                hasher.update(wait.get().to_le_bytes());
                hasher.update(signal.get().to_le_bytes());
            }
            EffectGraphExecutionStep::Release { materialization, after } => {
                hasher.update([3]);
                hasher.update(materialization.get().to_le_bytes());
                hasher.update(after.get().to_le_bytes());
            }
        }
    }
    hasher.update((prepared.cpu_nodes().len() as u64).to_le_bytes());
    for node in prepared.cpu_nodes() {
        hasher.update(node.0.to_le_bytes());
        hash_effect_operation_shape(&mut hasher, graph, *node)?;
    }
    hasher.update((prepared.gpu_plan().node_ids().len() as u64).to_le_bytes());
    for node in prepared.gpu_plan().node_ids() {
        hasher.update(node.0.to_le_bytes());
        hash_effect_operation_shape(&mut hasher, graph, *node)?;
    }
    Ok(hasher.finalize().into())
}

fn export_heterogeneous_route_contract(
    graph: &CompiledEffectGraph,
    prepared: &PreparedHeterogeneousEffectWork,
    placement: ExportHeterogeneousPlacement,
    maximum_extent: EffectFrameExtent,
    budget: EffectGraphExecutionBudget,
) -> Result<ExportHeterogeneousRouteContract, ExportHeterogeneousEffectError> {
    let shape_fingerprint = export_heterogeneous_route_shape_fingerprint(graph, prepared)?;
    let plan = prepared.plan();
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.export.heterogeneous-route-contract");
    hasher.update(EXPORT_HETEROGENEOUS_ROUTE_SCHEMA_VERSION.to_le_bytes());
    hasher.update([placement.tag()]);
    hasher.update(maximum_extent.width().to_le_bytes());
    hasher.update(maximum_extent.height().to_le_bytes());
    hasher.update(shape_fingerprint);
    hasher.update(budget.max_host_bytes().to_le_bytes());
    hasher.update(budget.max_device_bytes().to_le_bytes());
    hasher.update(budget.max_transfer_bytes().to_le_bytes());
    hasher.update((budget.max_materializations() as u64).to_le_bytes());
    hasher.update((budget.max_steps() as u64).to_le_bytes());
    hasher.update(plan.peak_host_bytes().to_le_bytes());
    hasher.update(plan.peak_device_bytes().to_le_bytes());
    hasher.update(plan.transfer_bytes().to_le_bytes());
    hasher.update((prepared.cpu_required_working_bytes() as u64).to_le_bytes());
    Ok(ExportHeterogeneousRouteContract {
        canonical_fingerprint: hasher.finalize().into(),
        shape_fingerprint,
        placement,
        maximum_extent,
        maximum_peak_host_bytes: plan.peak_host_bytes(),
        maximum_peak_device_bytes: plan.peak_device_bytes(),
        maximum_transfer_bytes: plan.transfer_bytes(),
        maximum_cpu_working_bytes: prepared.cpu_required_working_bytes(),
    })
}

fn hash_effect_residency(
    hasher: &mut Sha256,
    residency: EffectValueResidency,
) -> Result<(), ExportHeterogeneousEffectError> {
    hasher.update(residency.lane().get().to_le_bytes());
    hasher.update([effect_precision_tag(residency.format().precision())]);
    match residency.format().domain() {
        mondrian_effects::EffectColorDomain::SceneLinearRgb => hasher.update([1]),
        domain => {
            return Err(ExportHeterogeneousEffectError::FrameContractMismatch {
                expected: "scene_linear_rgb".to_owned(),
                actual: format!("{domain:?}"),
            });
        }
    }
    Ok(())
}

const fn effect_backend_tag(backend: EffectProcessingBackend) -> u8 {
    match backend {
        EffectProcessingBackend::Cpu => 1,
        EffectProcessingBackend::Gpu => 2,
        EffectProcessingBackend::ExternalProcessor => 3,
    }
}

const fn effect_precision_tag(precision: EffectWorkingPrecision) -> u8 {
    match precision {
        EffectWorkingPrecision::NormalizedU8 => 1,
        EffectWorkingPrecision::Float16 => 2,
        EffectWorkingPrecision::Float32 => 3,
    }
}

fn hash_effect_operation_shape(
    hasher: &mut Sha256,
    graph: &CompiledEffectGraph,
    node_id: mondrian_effects::EffectGraphNodeId,
) -> Result<(), ExportHeterogeneousEffectError> {
    let node = graph.graph().node(node_id).ok_or_else(|| {
        ExportHeterogeneousEffectError::FrameContractMismatch {
            expected: "compiled effect node".to_owned(),
            actual: format!("missing node {node_id:?}"),
        }
    })?;
    let (kind_tag, operation) = match &node.kind {
        EffectGraphNodeKind::UnaryEffect { op, .. } => (1, op),
        EffectGraphNodeKind::DomainEffect { op, .. } => (2, op),
        kind => {
            return Err(ExportHeterogeneousEffectError::FrameContractMismatch {
                expected: "unary heterogeneous effect node".to_owned(),
                actual: format!("{kind:?}"),
            });
        }
    };
    hasher.update([kind_tag, effect_operation_shape_tag(operation)]);
    Ok(())
}

const fn effect_operation_shape_tag(operation: &EffectRenderOp) -> u8 {
    match operation {
        EffectRenderOp::ColorAdjust { .. } => 1,
        EffectRenderOp::GaussianBlur { .. } => 2,
        EffectRenderOp::Sharpen { .. } => 3,
        EffectRenderOp::Vignette { .. } => 4,
        EffectRenderOp::ChromaticAberration { .. } => 5,
        EffectRenderOp::Grain { .. } => 6,
        EffectRenderOp::TemporalFrameMix { .. } => 7,
        EffectRenderOp::Lut3D { .. } => 8,
        EffectRenderOp::Custom { .. } => 9,
    }
}

fn hex_fingerprint(fingerprint: [u8; 32]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(fingerprint.len().saturating_mul(2));
    for byte in fingerprint {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

impl ExportVisualRenderSession {
    #[cfg(test)]
    pub(super) fn heterogeneous_route_logical_bytes(&self) -> usize {
        self.heterogeneous_route_contracts
            .len()
            .saturating_mul(EXPORT_HETEROGENEOUS_ROUTE_CONTRACT_LOGICAL_BYTES)
    }

    fn heterogeneous_graph_budget(
        &self,
    ) -> Result<EffectGraphExecutionBudget, ExportHeterogeneousEffectError> {
        let bytes = u64::try_from(self.resource_policy.effect_working_bytes).map_err(|_| {
            ExportHeterogeneousEffectError::CapabilityUnavailable {
                reason: "effect working-byte grant exceeds u64".to_owned(),
            }
        })?;
        Ok(EffectGraphExecutionBudget::new(
            bytes,
            bytes,
            bytes,
            EXPORT_HETEROGENEOUS_MAX_MATERIALIZATIONS,
            EXPORT_HETEROGENEOUS_MAX_STEPS,
        ))
    }

    fn heterogeneous_gpu_grant(
        &self,
    ) -> Result<HeterogeneousGpuResourceGrant, ExportHeterogeneousEffectError> {
        let bytes = u64::try_from(self.resource_policy.effect_working_bytes).map_err(|_| {
            ExportHeterogeneousEffectError::CapabilityUnavailable {
                reason: "effect working-byte grant exceeds u64".to_owned(),
            }
        })?;
        Ok(HeterogeneousGpuResourceGrant::new(bytes, bytes, bytes))
    }

    pub(super) fn prepare_heterogeneous_work(
        &self,
        graph: &Arc<CompiledEffectGraph>,
        extent: EffectFrameExtent,
        placement: ExportHeterogeneousPlacement,
    ) -> Result<
        (PreparedHeterogeneousEffectWork, EffectGraphExecutionBudget),
        ExportHeterogeneousEffectError,
    > {
        let capability = self.heterogeneous_capability.as_ref().map_err(|reason| {
            ExportHeterogeneousEffectError::CapabilityUnavailable { reason: reason.clone() }
        })?;
        let budget = self.heterogeneous_graph_budget()?;
        let prepared = PreparedHeterogeneousEffectWork::prepare(
            Arc::clone(graph),
            capability.environment(),
            capability.request(extent, budget),
        )
        .map_err(|source| ExportHeterogeneousEffectError::Preparation {
            placement: placement.label(),
            source,
        })?;
        Ok((prepared, budget))
    }

    pub(super) fn register_or_validate_route_contract(
        &mut self,
        graph: &CompiledEffectGraph,
        prepared: &PreparedHeterogeneousEffectWork,
        placement: ExportHeterogeneousPlacement,
        maximum_extent: EffectFrameExtent,
        budget: EffectGraphExecutionBudget,
    ) -> Result<(), ExportHeterogeneousEffectError> {
        let contract = export_heterogeneous_route_contract(
            graph,
            prepared,
            placement,
            maximum_extent,
            budget,
        )?;
        if self.route_contracts_sealed {
            if self
                .heterogeneous_route_contracts
                .iter()
                .any(|candidate| candidate.canonical_fingerprint == contract.canonical_fingerprint)
            {
                return Ok(());
            }
            return Err(
                ExportHeterogeneousEffectError::RouteContractNotPreflighted {
                    placement: placement.label(),
                    width: maximum_extent.width(),
                    height: maximum_extent.height(),
                    shape: hex_fingerprint(contract.shape_fingerprint),
                },
            );
        }
        if self
            .heterogeneous_route_contracts
            .iter()
            .any(|candidate| candidate.canonical_fingerprint == contract.canonical_fingerprint)
        {
            return Ok(());
        }
        let next_count = self.heterogeneous_route_contracts.len().saturating_add(1);
        if next_count > self.resource_policy.heterogeneous_route_contract_entries {
            return Err(ExportHeterogeneousEffectError::RouteContractCountExceeded {
                contracts: next_count,
                limit: self.resource_policy.heterogeneous_route_contract_entries,
            });
        }
        let route_bytes = next_count
            .checked_mul(EXPORT_HETEROGENEOUS_ROUTE_CONTRACT_LOGICAL_BYTES)
            .ok_or(ExportHeterogeneousEffectError::RouteContractBytesExceeded {
                required: usize::MAX,
                limit: self.resource_policy.heterogeneous_route_contract_bytes,
            })?;
        if route_bytes > self.resource_policy.heterogeneous_route_contract_bytes {
            return Err(ExportHeterogeneousEffectError::RouteContractBytesExceeded {
                required: route_bytes,
                limit: self.resource_policy.heterogeneous_route_contract_bytes,
            });
        }
        self.heterogeneous_route_contracts.push(contract);
        self.visual_diagnostics.heterogeneous_route_contracts =
            self.heterogeneous_route_contracts.len() as u64;
        Ok(())
    }

    fn validate_exact_route_contract(
        &self,
        graph: &CompiledEffectGraph,
        prepared: &PreparedHeterogeneousEffectWork,
        placement: ExportHeterogeneousPlacement,
        exact_extent: EffectFrameExtent,
    ) -> Result<(), ExportHeterogeneousEffectError> {
        let shape_fingerprint = export_heterogeneous_route_shape_fingerprint(graph, prepared)?;
        let plan = prepared.plan();
        if self.heterogeneous_route_contracts.iter().any(|contract| {
            contract.placement == placement
                && contract.shape_fingerprint == shape_fingerprint
                && exact_extent.width() <= contract.maximum_extent.width()
                && exact_extent.height() <= contract.maximum_extent.height()
                && plan.peak_host_bytes() <= contract.maximum_peak_host_bytes
                && plan.peak_device_bytes() <= contract.maximum_peak_device_bytes
                && plan.transfer_bytes() <= contract.maximum_transfer_bytes
                && prepared.cpu_required_working_bytes() <= contract.maximum_cpu_working_bytes
        }) {
            return Ok(());
        }
        Err(
            ExportHeterogeneousEffectError::RouteContractNotPreflighted {
                placement: placement.label(),
                width: exact_extent.width(),
                height: exact_extent.height(),
                shape: hex_fingerprint(shape_fingerprint),
            },
        )
    }

    pub(super) fn select_heterogeneous_route(
        &mut self,
        graph: &Arc<CompiledEffectGraph>,
        placement: ExportHeterogeneousPlacement,
        maximum_extent: EffectFrameExtent,
    ) -> Result<bool, ExportHeterogeneousEffectError> {
        let cpu_exact = compiled_effect_graph_supports_rgba_f32_with_domain_processor(graph);
        if cpu_exact {
            self.visual_diagnostics.cpu_routes_selected_before_start =
                self.visual_diagnostics.cpu_routes_selected_before_start.saturating_add(1);
            return Ok(false);
        }
        if !graph.execution_envelope().requires_execution_transitions() {
            // A homogeneous graph that cannot enter the Float32 CPU compositor
            // is not thereby a heterogeneous route. Preserve the graph for the
            // single compositor admission below so ordered state, temporal
            // input, blocked color domains, GPU-only execution, and the
            // NormalizedU8 export boundary retain their canonical diagnostics.
            return Ok(false);
        }
        let prepared = self.prepare_heterogeneous_work(graph, maximum_extent, placement)?;
        if !placement.supports_current_adapter() {
            return Err(ExportHeterogeneousEffectError::UnsupportedPlacement {
                placement: placement.label(),
            });
        }
        if self.gpu_output.ensure_ready().is_err() {
            return Err(ExportHeterogeneousEffectError::RequiredGpuUnavailable);
        }
        self.register_or_validate_route_contract(
            graph,
            &prepared.0,
            placement,
            maximum_extent,
            prepared.1,
        )?;
        Ok(true)
    }

    pub(super) fn execute_heterogeneous_element(
        &mut self,
        route: &PreparedExportHeterogeneousElement,
        input: &CpuColorFrame,
        working_color_space: WorkingColorSpace,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<CpuColorFrame, ExportHeterogeneousEffectError> {
        let descriptor = input.descriptor();
        let contract_valid = descriptor.width > 0
            && descriptor.height > 0
            && descriptor.color_space.working() == Some(working_color_space)
            && descriptor.domain == ColorFrameDomain::Working
            && descriptor.encoding == ColorFrameEncoding::LinearFloat
            && descriptor.residency == ColorFrameResidency::Cpu
            && descriptor.alpha == ColorFrameAlpha::StraightCoverage;
        if !contract_valid {
            return Err(ExportHeterogeneousEffectError::FrameContractMismatch {
                expected: format!(
                    "non-empty CPU working linear-float straight-alpha {:?}",
                    working_color_space
                ),
                actual: format!("{descriptor:?}"),
            });
        }
        let extent = EffectFrameExtent::new(descriptor.width, descriptor.height);
        let (prepared, _) =
            self.prepare_heterogeneous_work(&route.graph, extent, route.placement)?;
        self.validate_exact_route_contract(&route.graph, &prepared, route.placement, extent)?;
        let gpu_grant = self.heterogeneous_gpu_grant()?;
        if cancellation.is_canceled() {
            return Err(ExportHeterogeneousEffectError::Canceled {
                checkpoint: "before_cpu_prefix",
            });
        }
        self.visual_diagnostics.heterogeneous_frames_started =
            self.visual_diagnostics.heterogeneous_frames_started.saturating_add(1);
        let generation = self.effect_execution_generation;
        let completion = self
            .composite_scratch
            .execute_prepared_heterogeneous_cpu_prefix_with_checkpoint(
                &prepared,
                generation,
                &input.rgba_f32().data,
                route.frame_seed,
                working_color_space,
                || {
                    cancellation
                        .is_canceled()
                        .then_some(mondrian_effects::HeterogeneousCpuExecutionStopReason::Canceled)
                },
            )
            .map_err(|source| {
                self.visual_diagnostics.heterogeneous_terminal_failures =
                    self.visual_diagnostics.heterogeneous_terminal_failures.saturating_add(1);
                match source {
                    PreparedHeterogeneousEffectWorkError::ExecutionStopped {
                        reason: mondrian_effects::HeterogeneousCpuExecutionStopReason::Canceled,
                        ..
                    } => ExportHeterogeneousEffectError::Canceled { checkpoint: "cpu_prefix" },
                    source => ExportHeterogeneousEffectError::CpuPrefix {
                        placement: route.placement.label(),
                        source,
                    },
                }
            })?;
        if cancellation.is_canceled() {
            self.visual_diagnostics.heterogeneous_terminal_failures =
                self.visual_diagnostics.heterogeneous_terminal_failures.saturating_add(1);
            return Err(ExportHeterogeneousEffectError::Canceled {
                checkpoint: "after_cpu_prefix",
            });
        }
        let request = HeterogeneousGpuContinuationRequest::new(
            HeterogeneousGpuContinuationBinding::new(
                route.graph.semantic_fingerprint(),
                generation,
                extent,
                route.frame_seed,
                working_color_space,
            ),
            gpu_grant,
        );
        let completed = self
            .gpu_output
            .execute_heterogeneous(request, completion, cancellation)
            .map_err(|source| {
                self.visual_diagnostics.heterogeneous_terminal_failures =
                    self.visual_diagnostics.heterogeneous_terminal_failures.saturating_add(1);
                ExportHeterogeneousEffectError::GpuContinuation {
                    placement: route.placement.label(),
                    source: Box::new(source),
                }
            })?;
        if cancellation.is_canceled() {
            self.visual_diagnostics.heterogeneous_terminal_failures =
                self.visual_diagnostics.heterogeneous_terminal_failures.saturating_add(1);
            return Err(ExportHeterogeneousEffectError::Canceled {
                checkpoint: "after_gpu_completion",
            });
        }
        let evidence = ExportHeterogeneousCompletionEvidence::from_renderer(
            completed.evidence(),
            working_color_space,
        )
        .ok_or_else(|| {
            self.visual_diagnostics.heterogeneous_terminal_failures =
                self.visual_diagnostics.heterogeneous_terminal_failures.saturating_add(1);
            ExportHeterogeneousEffectError::MissingReadbackEvidence
        })?;
        let output = completed.into_frame();
        if output.descriptor() != descriptor {
            self.visual_diagnostics.heterogeneous_terminal_failures =
                self.visual_diagnostics.heterogeneous_terminal_failures.saturating_add(1);
            return Err(ExportHeterogeneousEffectError::FrameContractMismatch {
                expected: format!("{descriptor:?}"),
                actual: format!("{:?}", output.descriptor()),
            });
        }
        self.visual_diagnostics.heterogeneous_frames_completed =
            self.visual_diagnostics.heterogeneous_frames_completed.saturating_add(1);
        self.visual_diagnostics.heterogeneous_upload_bytes = self
            .visual_diagnostics
            .heterogeneous_upload_bytes
            .saturating_add(evidence.upload_bytes);
        self.visual_diagnostics.heterogeneous_readback_bytes = self
            .visual_diagnostics
            .heterogeneous_readback_bytes
            .saturating_add(evidence.readback_bytes);
        self.visual_diagnostics.last_heterogeneous_completion = Some(evidence);
        Ok(output)
    }
}

pub(super) fn prepare_export_effect_frame_plan(
    program: &PreparedVisualProgram,
    render_plan: &TimelineRenderPlan,
    resolution: Resolution,
    visual_session: &mut ExportVisualRenderSession,
) -> Result<PreparedExportEffectFramePlan, String> {
    let mut render_plan = render_plan.clone();
    let mut heterogeneous = Vec::new();
    let mut identity = None;
    for (element_index, element) in render_plan.elements.iter_mut().enumerate() {
        match element {
            TimelineRenderPlanElement::Media(media) => {
                let graph = Arc::clone(&media.effect_graph);
                let extent = EffectFrameExtent::new(resolution.width, resolution.height);
                if visual_session
                    .select_heterogeneous_route(&graph, ExportHeterogeneousPlacement::Media, extent)
                    .map_err(|error| error.to_string())?
                {
                    media.effect_graph = export_identity_effect_graph(&mut identity)?;
                    heterogeneous.push(PreparedExportHeterogeneousElement {
                        element_index,
                        placement: ExportHeterogeneousPlacement::Media,
                        graph,
                        frame_seed: media.frame_seed,
                    });
                }
            }
            TimelineRenderPlanElement::BasicTitle(title) => {
                let graph = Arc::clone(&title.effect_graph);
                let extent = EffectFrameExtent::new(resolution.width, resolution.height);
                if visual_session
                    .select_heterogeneous_route(
                        &graph,
                        ExportHeterogeneousPlacement::BasicTitle,
                        extent,
                    )
                    .map_err(|error| error.to_string())?
                {
                    title.effect_graph = export_identity_effect_graph(&mut identity)?;
                    heterogeneous.push(PreparedExportHeterogeneousElement {
                        element_index,
                        placement: ExportHeterogeneousPlacement::BasicTitle,
                        graph,
                        frame_seed: title.frame_seed,
                    });
                }
            }
            TimelineRenderPlanElement::NestedSequence(nested) => {
                let child =
                    visual_session.materialization_contract_for_sequence(nested.sequence_id)?;
                let graph = Arc::clone(&nested.effect_graph);
                let child_resolution = child.author_resolution();
                let extent =
                    EffectFrameExtent::new(child_resolution.width, child_resolution.height);
                if visual_session
                    .select_heterogeneous_route(
                        &graph,
                        ExportHeterogeneousPlacement::NestedSequence,
                        extent,
                    )
                    .map_err(|error| error.to_string())?
                {
                    nested.effect_graph = export_identity_effect_graph(&mut identity)?;
                    heterogeneous.push(PreparedExportHeterogeneousElement {
                        element_index,
                        placement: ExportHeterogeneousPlacement::NestedSequence,
                        graph,
                        frame_seed: nested.frame_seed,
                    });
                }
            }
            TimelineRenderPlanElement::SolidColor(solid) => {
                let extent = EffectFrameExtent::new(resolution.width, resolution.height);
                visual_session
                    .select_heterogeneous_route(
                        &solid.effect_graph,
                        ExportHeterogeneousPlacement::SolidColor,
                        extent,
                    )
                    .map_err(|error| error.to_string())?;
            }
            TimelineRenderPlanElement::Adjustment(adjustment) => {
                let extent = EffectFrameExtent::new(resolution.width, resolution.height);
                visual_session
                    .select_heterogeneous_route(
                        &adjustment.effect_graph,
                        ExportHeterogeneousPlacement::Adjustment,
                        extent,
                    )
                    .map_err(|error| error.to_string())?;
            }
            TimelineRenderPlanElement::CrossDissolve(transition) => {
                validate_export_transition_heterogeneous_placement(
                    program.sequence_id(),
                    resolution,
                    &transition.left,
                    visual_session,
                )?;
                validate_export_transition_heterogeneous_placement(
                    program.sequence_id(),
                    resolution,
                    &transition.right,
                    visual_session,
                )?;
            }
        }
    }
    admit_timeline_render_plan_for_cpu_compositor(&render_plan).map_err(|error| {
        format!(
            "Sequence {} frame {} cannot enter the current export compositor after explicit Effect route selection: {error}",
            program.sequence_id(), render_plan.position.frame
        )
    })?;
    Ok(PreparedExportEffectFramePlan { render_plan, heterogeneous })
}

fn validate_export_transition_heterogeneous_placement(
    parent_sequence_id: SequenceId,
    resolution: Resolution,
    input: &TimelineTransitionInputPlan,
    visual_session: &mut ExportVisualRenderSession,
) -> Result<(), String> {
    let (graph, extent) = match input {
        TimelineTransitionInputPlan::Transparent => return Ok(()),
        TimelineTransitionInputPlan::Media(media) => (
            &media.effect_graph,
            EffectFrameExtent::new(resolution.width, resolution.height),
        ),
        TimelineTransitionInputPlan::SolidColor(solid) => (
            &solid.effect_graph,
            EffectFrameExtent::new(resolution.width, resolution.height),
        ),
        TimelineTransitionInputPlan::BasicTitle(title) => (
            &title.effect_graph,
            EffectFrameExtent::new(resolution.width, resolution.height),
        ),
        TimelineTransitionInputPlan::NestedSequence(nested) => {
            let child = visual_session
                .materialization_contract_for_sequence(nested.sequence_id)
                .map_err(|error| {
                    format!(
                        "export heterogeneous Transition preflight cannot resolve nested Sequence {} from {parent_sequence_id}: {error}",
                        nested.sequence_id
                    )
                })?;
            let child_resolution = child.author_resolution();
            (
                &nested.effect_graph,
                EffectFrameExtent::new(child_resolution.width, child_resolution.height),
            )
        }
    };
    visual_session
        .select_heterogeneous_route(graph, ExportHeterogeneousPlacement::TransitionInput, extent)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn export_identity_effect_graph(
    cached: &mut Option<Arc<CompiledEffectGraph>>,
) -> Result<Arc<CompiledEffectGraph>, String> {
    if let Some(identity) = cached {
        return Ok(Arc::clone(identity));
    }
    let identity = identity_compiled_effect_graph()
        .ok_or_else(|| ExportHeterogeneousEffectError::IdentityUnavailable.to_string())?;
    *cached = Some(Arc::clone(&identity));
    Ok(identity)
}
