//! Execution-owned, bounded reuse of frame-independent Float32 processors.

use mondrian_core::{EffectId, MondrianError, Result};
use std::collections::VecDeque;
use std::sync::Arc;

/// Authoritative reason the owning scheduler stopped a processor invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectProcessorStopReason {
    /// The execution generation was canceled or superseded.
    Canceled,
    /// The owning scheduler's deadline expired.
    DeadlineExpired,
}

/// Borrowed cooperative control for processor preparation and processing.
///
/// The scheduler remains the authority for cancellation and deadlines. A stop
/// is sticky for this invocation and must be returned as MondrianError::Cancelled;
/// it is not a plugin fault and must not enter runtime quarantine.
pub struct EffectProcessorControl<'a> {
    checkpoint: Option<&'a mut dyn FnMut() -> Option<EffectProcessorStopReason>>,
    stopped: Option<EffectProcessorStopReason>,
}

impl<'a> EffectProcessorControl<'a> {
    /// Bind the owning scheduler's cooperative checkpoint.
    pub fn new(checkpoint: &'a mut dyn FnMut() -> Option<EffectProcessorStopReason>) -> Self {
        Self { checkpoint: Some(checkpoint), stopped: None }
    }

    /// Control for explicitly uncancelled scalar/offline references.
    pub const fn uncancelled() -> Self {
        Self { checkpoint: None, stopped: None }
    }

    /// Poll the owner and preserve its exact sticky stop reason.
    pub fn checkpoint(&mut self) -> std::result::Result<(), EffectProcessorStopReason> {
        if self.stopped.is_none() {
            self.stopped = self.checkpoint.as_mut().and_then(|checkpoint| checkpoint());
        }
        match self.stopped {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    /// Poll before allocation, during bounded native waits and before publication.
    pub fn check(&mut self) -> Result<()> {
        self.checkpoint().map_err(|_| MondrianError::Cancelled)
    }

    /// Exact scheduler reason observed by this invocation, when stopped.
    pub const fn stop_reason(&self) -> Option<EffectProcessorStopReason> {
        self.stopped
    }
}

/// Identity and frame extent for one reusable processor occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectFloatProcessorPreparation {
    /// Author-owned effect identity retained by the compiler.
    pub effect_id: EffectId,
    /// Input/output width in pixels.
    pub width: u32,
    /// Input/output height in pixels.
    pub height: u32,
}

/// A reusable processor owned exclusively by one execution session.
///
/// Results must be independent of prior invocations. Instances may be evicted
/// or recreated between any two frames; sequential state requires the separate
/// continuity execution contract. Drop must retire every owned native worker.
pub trait EffectFloatProcessorInstance: std::fmt::Debug + Send {
    /// Process the complete frame with the current authored parameters.
    fn process(
        &mut self,
        pixels: &mut Vec<[f32; 4]>,
        params: &serde_json::Value,
        frame_seed: i64,
        control: &mut EffectProcessorControl<'_>,
    ) -> Result<()>;
}

/// Immutable recipe retained by a prepared program, without shared mutable
/// native instances. Each execution session prepares its own instance.
pub trait EffectFloatProcessorFactory: std::fmt::Debug + Send + Sync {
    /// Conservative resident byte charge, including owned worker/frame buffers.
    /// This charge is checked before constructing the instance. Sizing must be
    /// deterministic for the request and independent of prior invocations.
    fn resident_bytes(&self, request: EffectFloatProcessorPreparation) -> Result<usize>;
    /// Create one instance for this occurrence and exact frame extent.
    fn prepare(
        &self,
        request: EffectFloatProcessorPreparation,
        control: &mut EffectProcessorControl<'_>,
    ) -> Result<Box<dyn EffectFloatProcessorInstance>>;
}

/// Query immutable recipe sizing before any raster/instance allocation.
pub(crate) fn frame_processor_charge(
    op: &crate::EffectRenderOp,
    width: u32,
    height: u32,
) -> std::result::Result<usize, crate::EffectExecutionError> {
    let crate::EffectRenderOp::Custom { key, processor: Some(binding), .. } = op else {
        return Ok(0);
    };
    let Some(factory) = binding.float32_factory() else {
        return Ok(0);
    };
    let Some(effect_id) = binding.occurrence_id() else {
        return Err(crate::EffectExecutionError::CustomProcessorUnavailable { key: key.clone() });
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        factory.resident_bytes(EffectFloatProcessorPreparation { effect_id, width, height })
    }));
    let reason = match result {
        Ok(Ok(bytes)) => return Ok(bytes),
        Ok(Err(error)) => error.to_string(),
        Err(_) => "custom processor sizing panicked".to_owned(),
    };
    binding.record_runtime_failure(reason.clone());
    Err(crate::EffectExecutionError::CustomProcessorFailed { key: key.clone(), reason })
}

/// Distinguish resource-policy denial from processor execution faults.
#[derive(Debug, thiserror::Error)]
pub(crate) enum FloatProcessorError {
    #[error("processor requires {required_bytes} bytes; available grant is {budget_bytes}")]
    Budget {
        required_bytes: usize,
        budget_bytes: usize,
    },
    #[error(transparent)]
    Processor(#[from] MondrianError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResourceKey {
    request: EffectFloatProcessorPreparation,
    revision: u64,
}

#[derive(Debug, Default)]
pub(crate) struct FloatProcessorResidency {
    entries: VecDeque<(ResourceKey, usize, Box<dyn EffectFloatProcessorInstance>)>,
    bytes: usize,
}

impl FloatProcessorResidency {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    pub(crate) fn retire_occurrence(&mut self, effect_id: EffectId) {
        let mut index = 0;
        while index < self.entries.len() {
            if self.entries[index].0.request.effect_id == effect_id {
                if let Some((_, bytes, _)) = self.entries.remove(index) {
                    self.bytes -= bytes;
                }
            } else {
                index += 1;
            }
        }
    }

    pub(crate) fn trim(&mut self, max_instances: usize, max_bytes: usize) {
        while self.entries.len() > max_instances || self.bytes > max_bytes {
            self.evict_oldest();
        }
    }

    pub(crate) fn trim_working(&mut self, available_bytes: usize) {
        while self.bytes > available_bytes {
            self.evict_oldest();
        }
    }

    fn evict_oldest(&mut self) {
        if let Some((_, bytes, _)) = self.entries.pop_front() {
            self.bytes -= bytes;
        }
    }

    pub(crate) fn counts(&self) -> (usize, usize) {
        (self.entries.len(), self.bytes)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process(
        &mut self,
        factory: &Arc<dyn EffectFloatProcessorFactory>,
        request: EffectFloatProcessorPreparation,
        revision: u64,
        max_instances: usize,
        max_bytes: usize,
        available_working_bytes: usize,
        pixels: &mut Vec<[f32; 4]>,
        params: &serde_json::Value,
        frame_seed: i64,
        control: &mut EffectProcessorControl<'_>,
    ) -> std::result::Result<usize, FloatProcessorError> {
        control.check()?;
        let count = (request.width as usize).checked_mul(request.height as usize);
        if request.width == 0 || request.height == 0 || count != Some(pixels.len()) {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "effect_processor_frame".to_owned(),
                reason: "processor frame extent does not match its pixels".to_owned(),
            }
            .into());
        }
        self.trim(max_instances, max_bytes);
        let key = ResourceKey { request, revision };
        // Dimension/revision changes retire this occurrence's obsolete worker.
        let mut index = 0;
        while index < self.entries.len() {
            if self.entries[index].0.request.effect_id == request.effect_id
                && self.entries[index].0 != key
            {
                if let Some((_, bytes, _)) = self.entries.remove(index) {
                    self.bytes -= bytes;
                }
            } else {
                index += 1;
            }
        }
        let cached = self
            .entries
            .iter()
            .position(|entry| entry.0 == key)
            .and_then(|index| self.entries.remove(index));
        let (charge, cached_instance) = if let Some((_, charge, instance)) = cached {
            self.bytes -= charge;
            (charge, Some(instance))
        } else {
            (factory.resident_bytes(request)?, None)
        };
        if charge > available_working_bytes {
            return Err(FloatProcessorError::Budget {
                required_bytes: charge,
                budget_bytes: available_working_bytes,
            });
        }
        // Optional residency cannot reject an otherwise admitted invocation.
        let retain = max_instances > 0 && charge <= max_bytes;
        while !self.entries.is_empty()
            && (self.bytes > available_working_bytes - charge
                || (retain
                    && (self.entries.len() >= max_instances || self.bytes > max_bytes - charge)))
        {
            self.evict_oldest();
        }
        let peak_bytes = self.bytes + charge;
        // Eviction precedes native allocation. Resident siblings and this active
        // instance share the caller's exact remaining working grant.
        let mut instance = match cached_instance {
            Some(instance) => instance,
            None => factory.prepare(request, control)?,
        };
        let pixel_count = pixels.len();
        control.check()?;
        instance.process(pixels, params, frame_seed, control)?;
        control.check()?;
        if pixels.len() != pixel_count {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "effect_processor_frame".to_owned(),
                reason: "processor changed the pixel count".to_owned(),
            }
            .into());
        }
        if retain {
            self.bytes += charge;
            self.entries.push_back((key, charge, instance));
        }
        Ok(peak_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;
    use mondrian_core::{automation::PropertyBag, TimelineTime, WorkingColorSpace};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Debug, Default)]
    struct ProbeFactory {
        created: Arc<AtomicUsize>,
        dropped: Arc<AtomicUsize>,
        reached: Arc<AtomicBool>,
        cancellation: Option<mondrian_core::ExecutionCancellationToken>,
        resident_charge: Option<usize>,
    }
    #[derive(Debug)]
    struct ProbeInstance {
        dropped: Arc<AtomicUsize>,
        reached: Arc<AtomicBool>,
        cancellation: Option<mondrian_core::ExecutionCancellationToken>,
    }
    impl Drop for ProbeInstance {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl EffectFloatProcessorFactory for ProbeFactory {
        fn resident_bytes(&self, request: EffectFloatProcessorPreparation) -> Result<usize> {
            if request.width == 0 || request.height == 0 {
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "fixture_sizing".to_owned(),
                    reason: "native instance requires nonempty frames".to_owned(),
                });
            }
            Ok(self.resident_charge.unwrap_or(64))
        }
        fn prepare(
            &self,
            _: EffectFloatProcessorPreparation,
            control: &mut EffectProcessorControl<'_>,
        ) -> Result<Box<dyn EffectFloatProcessorInstance>> {
            control.check()?;
            self.created.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(ProbeInstance {
                dropped: Arc::clone(&self.dropped),
                reached: Arc::clone(&self.reached),
                cancellation: self.cancellation.clone(),
            }))
        }
    }
    impl EffectFloatProcessorInstance for ProbeInstance {
        fn process(
            &mut self,
            pixels: &mut Vec<[f32; 4]>,
            params: &serde_json::Value,
            _: i64,
            control: &mut EffectProcessorControl<'_>,
        ) -> Result<()> {
            control.check()?;
            if params["panic"].as_bool() == Some(true) {
                panic!("intentional processor panic");
            }
            if params["fail"].as_bool() == Some(true) {
                pixels[0][0] = 999.0;
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "fixture".to_owned(),
                    reason: "intentional failure".to_owned(),
                });
            }
            if params["cancel_without_owner"].as_bool() == Some(true) {
                pixels[0][0] = 999.0;
                return Err(MondrianError::Cancelled);
            }
            if params["resize"].as_bool() == Some(true) {
                pixels.clear();
                return Ok(());
            }
            for pixel in pixels {
                pixel[0] *= 2.0;
            }
            self.reached.store(true, Ordering::SeqCst);
            if let Some(cancellation) = &self.cancellation {
                cancellation.cancel();
            }
            control.check()?;
            Ok(())
        }
    }

    fn graph(factory: Arc<ProbeFactory>) -> Arc<CompiledEffectGraph> {
        graph_with_tail(factory, false)
    }

    fn register_probe_factory(factory: Arc<ProbeFactory>) -> String {
        let key = format!("test.resident.{}", EffectId::new());
        let definition = EffectDefinition::new(
            &key,
            "Reusable probe",
            PropertyBag::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: EffectDeterminism::Nondeterministic,
            roi_propagation: crate::EffectRoiPropagation::UnknownRequiresFullFrame,
            ..EffectExecutionContract::IDENTITY
        })
        .with_plugin_contract(
            EffectPluginContract::new("1.0.0")
                .with_runtime_failure_policy(EffectPluginRuntimeFailurePolicy::DisableDefinition),
        )
        .with_custom_float_processor_factory(
            Arc::new(|_, _| Ok(Some(serde_json::json!({})))),
            None,
            EffectCachePolicy::Uncacheable,
            factory,
        );
        register_effect_definition(definition).expect("register factory");
        key
    }

    fn graph_with_tail(factory: Arc<ProbeFactory>, gpu_tail: bool) -> Arc<CompiledEffectGraph> {
        let key = register_probe_factory(factory);
        let first =
            instantiate_effect_node(EffectType::Plugin(key.clone())).expect("first occurrence");
        let second = instantiate_effect_node(EffectType::Plugin(key)).expect("second occurrence");
        let mut effects = vec![first, second];
        if gpu_tail {
            let mut correction = EffectNode::with_defaults(EffectType::BasicCorrection);
            correction
                .set_static_value_by_parameter(
                    &EffectType::BasicCorrection
                        .parameter_id("exposure")
                        .expect("exposure parameter"),
                    mondrian_core::automation::PropertyValue::Float(0.5),
                )
                .expect("set GPU tail");
            effects.push(correction);
        }
        PreparedEffectProgram::prepare(&effects, &[], WorkingColorSpace::LinearRec709)
            .expect("prepare factory graph")
            .evaluate(TimelineTime::ZERO)
            .expect("compile factory graph")
    }

    #[test]
    fn empty_scalar_frames_never_query_or_prepare_native_recipes() {
        let factory = Arc::new(ProbeFactory::default());
        let graph = graph(Arc::clone(&factory));
        let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig::uncached(0));
        for (width, height) in [(0, 0), (0, 2), (3, 0)] {
            assert_eq!(
                session
                    .apply_compiled_rgba_f32(
                        &[],
                        width,
                        height,
                        &graph,
                        0,
                        &mut crate::EffectProcessorControl::uncancelled()
                    )
                    .expect("no pixels require no native instance"),
                Vec::<[f32; 4]>::new()
            );
        }
        assert_eq!(factory.created.load(Ordering::SeqCst), 0);
        assert_eq!(session.diagnostics().processor_instances, 0);
        for node in &graph.graph().nodes {
            if let EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::Custom { key, .. }, ..
            } = &node.kind
            {
                let status = effect_plugin_runtime_status(key).expect("status");
                assert!(!status.disabled);
                assert_eq!(status.last_error, None);
            }
        }
    }

    #[test]
    fn compiled_factory_instances_are_owner_exclusive_and_reuse_across_frame_generations() {
        let factory = Arc::new(ProbeFactory::default());
        let graph = graph(Arc::clone(&factory));
        let mut preview = EffectExecutionSession::default();
        let mut export = EffectExecutionSession::default();
        let input = [[0.25, 0.5, 0.75, 1.0]];
        for _ in 0..2 {
            assert_eq!(
                preview
                    .apply_compiled_rgba_f32(
                        &input,
                        1,
                        1,
                        &graph,
                        0,
                        &mut crate::EffectProcessorControl::uncancelled()
                    )
                    .expect("preview"),
                [[1.0, 0.5, 0.75, 1.0]]
            );
        }
        assert_eq!(factory.created.load(Ordering::SeqCst), 2);
        assert_eq!(preview.diagnostics().processor_instances, 2);
        assert_eq!(preview.diagnostics().processor_bytes, 128);
        export
            .apply_compiled_rgba_f32(
                &input,
                1,
                1,
                &graph,
                0,
                &mut crate::EffectProcessorControl::uncancelled(),
            )
            .expect("export");
        assert_eq!(factory.created.load(Ordering::SeqCst), 4);
        for (generation, seed, source) in [(1, 7, 0.5), (2, -3, -0.25), (3, 99, 1.5)] {
            preview.bind_generation(generation);
            let identity = identity_compiled_effect_graph().expect("identity graph");
            assert_eq!(
                preview
                    .apply_compiled_rgba_f32(
                        &[[source, 0.5, 0.75, 1.0]],
                        1,
                        1,
                        &identity,
                        seed,
                        &mut EffectProcessorControl::uncancelled(),
                    )
                    .expect("other clip's identity frame"),
                [[source, 0.5, 0.75, 1.0]]
            );
            assert_eq!(
                preview
                    .apply_compiled_rgba_f32(
                        &[[source, 0.5, 0.75, 1.0]],
                        1,
                        1,
                        &graph,
                        seed,
                        &mut EffectProcessorControl::uncancelled(),
                    )
                    .expect("changed frame"),
                [[source * 4.0, 0.5, 0.75, 1.0]]
            );
        }
        assert_eq!(factory.created.load(Ordering::SeqCst), 4);
        assert_eq!(preview.diagnostics().processor_instances, 2);
        assert_eq!(export.diagnostics().processor_instances, 2);
        assert_eq!(factory.dropped.load(Ordering::SeqCst), 0);
        preview.retire_processor_resources();
        assert_eq!(preview.diagnostics().processor_instances, 0);
        assert_eq!(factory.dropped.load(Ordering::SeqCst), 2);
        drop(export);
        assert_eq!(factory.dropped.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn factory_errors_panics_and_invalid_output_preserve_pixels_and_retire_instances() {
        let factory = Arc::new(ProbeFactory::default());
        let graph = graph(Arc::clone(&factory));
        let mut op = graph
            .graph()
            .nodes
            .iter()
            .find_map(|node| match &node.kind {
                EffectGraphNodeKind::UnaryEffect {
                    op: op @ EffectRenderOp::Custom { .. }, ..
                } => Some(op.clone()),
                _ => None,
            })
            .expect("compiled factory operation");
        let mut session = EffectExecutionSession::default();
        let original = vec![[0.25, 0.5, 0.75, 1.0]];
        for key in ["fail", "resize", "panic", "cancel_without_owner"] {
            if let EffectRenderOp::Custom { params, .. } = &mut op {
                *params = serde_json::json!({key: true});
            }
            let mut working = original.clone();
            assert!(crate::adjustment::apply_custom_render_op_f32(
                &mut working,
                1,
                1,
                &op,
                0,
                Some(&mut session),
                16,
                &mut EffectProcessorControl::uncancelled(),
            )
            .is_err());
            assert_eq!(working, original);
            assert_eq!(session.diagnostics().processor_instances, 0);
        }
        assert_eq!(factory.created.load(Ordering::SeqCst), 4);
        assert_eq!(factory.dropped.load(Ordering::SeqCst), 4);
        let EffectRenderOp::Custom { key, .. } = &op else {
            unreachable!()
        };
        assert!(effect_plugin_runtime_status(key).expect("plugin status").disabled);
    }

    #[test]
    fn stopped_processors_preserve_pixels_retire_instances_and_do_not_quarantine() {
        for reason in [
            EffectProcessorStopReason::Canceled,
            EffectProcessorStopReason::DeadlineExpired,
        ] {
            // Stop at preparation, during processing, and immediately before publication.
            for stop_at in [3, 6, 8] {
                let factory = Arc::new(ProbeFactory::default());
                let graph = graph(Arc::clone(&factory));
                let op = graph
                    .graph()
                    .nodes
                    .iter()
                    .find_map(|node| match &node.kind {
                        EffectGraphNodeKind::UnaryEffect {
                            op: op @ EffectRenderOp::Custom { .. },
                            ..
                        } => Some(op),
                        _ => None,
                    })
                    .expect("compiled operation");
                let EffectRenderOp::Custom { key, .. } = op else {
                    unreachable!()
                };
                let original = vec![[0.25, 0.5, 0.75, 1.0]];
                let mut working = original.clone();
                let mut session = EffectExecutionSession::default();
                let mut polls = 0;
                let mut checkpoint = || {
                    polls += 1;
                    (polls == stop_at).then_some(reason)
                };
                let mut control = EffectProcessorControl::new(&mut checkpoint);
                let result = crate::adjustment::apply_custom_render_op_f32(
                    &mut working,
                    1,
                    1,
                    op,
                    0,
                    Some(&mut session),
                    16,
                    &mut control,
                );
                assert_eq!(
                    result,
                    Err(EffectExecutionError::CustomProcessorStopped { reason })
                );
                assert_eq!(working, original);
                assert_eq!(session.diagnostics().processor_instances, 0);
                assert_eq!(
                    factory.created.load(Ordering::SeqCst),
                    factory.dropped.load(Ordering::SeqCst)
                );
                // A later checkpoint cannot resume a stopped invocation.
                assert!(control.check().is_err());
                assert_eq!(control.stop_reason(), Some(reason));
                let status = effect_plugin_runtime_status(key).expect("plugin status");
                assert!(!status.disabled);
                assert_eq!(status.last_error, None);
                crate::adjustment::apply_custom_render_op_f32(
                    &mut working,
                    1,
                    1,
                    op,
                    0,
                    Some(&mut session),
                    16,
                    &mut EffectProcessorControl::uncancelled(),
                )
                .expect("fresh invocation after cancel");
                assert_eq!(working, [[0.5, 0.5, 0.75, 1.0]]);
                assert_eq!(session.diagnostics().processor_instances, 1);
            }
        }
    }

    fn mixed_work(graph: Arc<CompiledEffectGraph>) -> PreparedHeterogeneousEffectWork {
        let cpu = EffectExecutionLaneId::new(0);
        let gpu = EffectExecutionLaneId::new(1);
        let environment = EffectExecutionEnvironment::new(
            Arc::from([
                EffectExecutionLane::new(
                    cpu,
                    EffectProcessingBackend::Cpu,
                    EffectWorkingPrecisions::FLOAT32,
                    10,
                ),
                EffectExecutionLane::new(
                    gpu,
                    EffectProcessingBackend::Gpu,
                    EffectWorkingPrecisions::FLOAT32,
                    1,
                ),
            ]),
            Arc::from([EffectExecutionTransfer::new(
                cpu,
                EffectWorkingPrecision::Float32,
                gpu,
                EffectWorkingPrecision::Float32,
                2,
            )]),
        )
        .expect("execution lanes");
        let format = EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
        );
        PreparedHeterogeneousEffectWork::prepare(
            graph,
            &environment,
            EffectGraphExecutionRequest::new(
                EffectFrameExtent::new(1, 1),
                EffectValueResidency::new(cpu, format),
                EffectValueResidency::new(gpu, format),
                EffectGraphExecutionBudget::new(1024 * 1024, 1024 * 1024, 1024 * 1024, 128, 256),
            ),
        )
        .expect("prepare mixed route")
    }

    #[test]
    fn heterogeneous_cpu_prefix_reuses_session_owned_factory_instances() {
        let factory = Arc::new(ProbeFactory::default());
        let graph = graph_with_tail(Arc::clone(&factory), true);
        let work = mixed_work(graph);
        assert_eq!(
            work.cpu_required_working_bytes(),
            96,
            "batch admission includes the active recipe"
        );
        assert_eq!(work.cpu_nodes().len(), 2);
        assert_eq!(work.gpu_suffix().node_ids().len(), 1);
        let mut session = EffectExecutionSession::default();
        session.bind_generation(1);
        for _ in 0..2 {
            let completion = work
                .execute_cpu_prefix_uncancelled(
                    &mut session,
                    1,
                    &[[0.25, 0.5, 0.75, 1.0]],
                    0,
                    WorkingColorSpace::LinearRec709,
                )
                .expect("execute factory CPU prefix");
            assert_eq!(
                completion.boundary_values()[0].pixels(),
                &[[1.0, 0.5, 0.75, 1.0]]
            );
        }
        assert_eq!(factory.created.load(Ordering::SeqCst), 2);
        assert_eq!(session.diagnostics().processor_instances, 2);
        for reason in [
            HeterogeneousCpuExecutionStopReason::Canceled,
            HeterogeneousCpuExecutionStopReason::DeadlineExpired,
        ] {
            factory.reached.store(false, Ordering::SeqCst);
            let mut stopped_session = EffectExecutionSession::default();
            stopped_session.bind_generation(42);
            let input = [[0.25, 0.5, 0.75, 1.0]];
            let result = work.execute_cpu_prefix_with_checkpoint(
                &mut stopped_session,
                42,
                &input,
                0,
                WorkingColorSpace::LinearRec709,
                || factory.reached.load(Ordering::SeqCst).then_some(reason),
            );
            assert!(matches!(result,
                Err(PreparedHeterogeneousEffectWorkError::ExecutionStopped { generation: 42, reason: observed }) if observed == reason));
            assert_eq!(input, [[0.25, 0.5, 0.75, 1.0]]);
            assert_eq!(stopped_session.diagnostics().processor_instances, 0);
        }
        // Canceled prefixes leave the definition available for a fresh generation.
        work.execute_cpu_prefix_uncancelled(
            &mut session,
            1,
            &[[0.25, 0.5, 0.75, 1.0]],
            0,
            WorkingColorSpace::LinearRec709,
        )
        .expect("fresh prefix after stops");
    }

    #[test]
    fn temporal_processor_cancellation_preserves_owner_result_and_plugin_availability() {
        let cancellation = mondrian_core::ExecutionCancellationToken::new();
        let factory = Arc::new(ProbeFactory {
            cancellation: Some(cancellation.clone()),
            ..ProbeFactory::default()
        });
        let graph = graph(Arc::clone(&factory));
        let mut session = EffectExecutionSession::default();
        let request = EffectTemporalExecutionRequest::new(
            42,
            EffectExecutionContinuity::Continuous,
            TimelineTime::ZERO,
            EffectFrameExtent::new(1, 1),
            EffectPixelRoi::new(0, 0, 1, 1),
            cancellation,
        );
        assert!(matches!(
            session.execute_temporal_f32(&graph, &request, &mut ConstantProvider),
            Err(EffectTemporalExecutionError::Canceled)
        ));
        assert!(factory.reached.load(Ordering::SeqCst));
        assert_eq!(session.diagnostics().processor_instances, 0);
        assert_eq!(factory.created.load(Ordering::SeqCst), 1);
        assert_eq!(factory.dropped.load(Ordering::SeqCst), 1);
        for node in &graph.graph().nodes {
            if let EffectGraphNodeKind::UnaryEffect {
                op: EffectRenderOp::Custom { key, .. }, ..
            } = &node.kind
            {
                let status = effect_plugin_runtime_status(key).expect("plugin status");
                assert!(!status.disabled);
                assert_eq!(status.last_error, None);
            }
        }
    }

    struct ConstantProvider;

    impl EffectTemporalFrameProvider for ConstantProvider {
        fn source_identity(&self) -> EffectTemporalSourceIdentity {
            EffectTemporalSourceIdentity::from_complete_semantic_fingerprint([19; 32])
        }

        fn retained_coverage_bytes(&self) -> usize {
            0
        }

        fn copy_frame(
            &mut self,
            request: EffectTemporalFrameRequest,
            cancellation: &mondrian_core::ExecutionCancellationToken,
            destination: &mut Vec<[f32; 4]>,
        ) -> std::result::Result<(), EffectTemporalFrameProviderError> {
            if cancellation.is_canceled() {
                return Err(EffectTemporalFrameProviderError::Canceled);
            }
            let roi = request.input_roi().region();
            *destination = vec![[0.25, 0.5, 0.75, 1.0]; (roi.width() * roi.height()) as usize];
            Ok(())
        }
    }

    #[test]
    fn temporal_execution_reuses_independent_instances_across_frame_generations() {
        let factory = Arc::new(ProbeFactory::default());
        let graph = graph(Arc::clone(&factory));
        let mut session = EffectExecutionSession::default();
        let extent = EffectFrameExtent::new(2, 3);
        for generation in [1, 1, 2] {
            let request = EffectTemporalExecutionRequest::new(
                generation,
                EffectExecutionContinuity::Continuous,
                TimelineTime::ZERO,
                extent,
                EffectPixelRoi::new(0, 0, 2, 3),
                mondrian_core::ExecutionCancellationToken::new(),
            );
            let output = session
                .execute_temporal_f32(&graph, &request, &mut ConstantProvider)
                .expect("execute reusable temporal processor");
            assert_eq!(output.tile().pixels(), &[[1.0, 0.5, 0.75, 1.0]; 6]);
            assert_eq!(session.diagnostics().processor_instances, 2);
            assert_eq!(factory.created.load(Ordering::SeqCst), 2);
        }
        assert_eq!(factory.dropped.load(Ordering::SeqCst), 0);
        drop(session);
        assert_eq!(factory.dropped.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn scalar_temporal_and_mixed_routes_share_pixel_and_processor_admission() {
        for budget in [95, 96, 160] {
            for route in ["scalar", "temporal", "mixed"] {
                let factory = Arc::new(ProbeFactory::default());
                let graph = graph_with_tail(Arc::clone(&factory), route == "mixed");
                let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
                    max_working_bytes: budget,
                    ..EffectExecutionSessionConfig::default()
                });
                session.bind_generation(1);
                let input = [[0.25, 0.5, 0.75, 1.0]];
                let result = match route {
                    "scalar" => session
                        .apply_compiled_rgba_f32(
                            &input,
                            1,
                            1,
                            &graph,
                            0,
                            &mut crate::EffectProcessorControl::uncancelled(),
                        )
                        .map_err(|error| format!("{error:?}")),
                    "temporal" => {
                        let request = EffectTemporalExecutionRequest::new(
                            1,
                            EffectExecutionContinuity::Continuous,
                            TimelineTime::ZERO,
                            EffectFrameExtent::new(1, 1),
                            EffectPixelRoi::new(0, 0, 1, 1),
                            mondrian_core::ExecutionCancellationToken::new(),
                        );
                        session
                            .execute_temporal_f32(&graph, &request, &mut ConstantProvider)
                            .map(|output| {
                                assert_eq!(output.peak_working_bytes(), budget);
                                output.tile().pixels().to_vec()
                            })
                            .map_err(|error| format!("{error:?}"))
                    }
                    "mixed" => mixed_work(Arc::clone(&graph))
                        .execute_cpu_prefix_uncancelled(
                            &mut session,
                            1,
                            &input,
                            0,
                            WorkingColorSpace::LinearRec709,
                        )
                        .map(|output| output.boundary_values()[0].pixels().to_vec())
                        .map_err(|error| format!("{error:?}")),
                    _ => unreachable!(),
                };
                assert_eq!(input, [[0.25, 0.5, 0.75, 1.0]]);
                if budget == 95 {
                    assert!(result.is_err(), "{route}: {result:?}");
                    assert_eq!(factory.created.load(Ordering::SeqCst), 0);
                    for node in &graph.graph().nodes {
                        if let EffectGraphNodeKind::UnaryEffect {
                            op: EffectRenderOp::Custom { key, .. },
                            ..
                        } = &node.kind
                        {
                            assert!(!effect_plugin_runtime_status(key).expect("status").disabled);
                        }
                    }
                } else {
                    assert_eq!(result.expect(route), [[1.0, 0.5, 0.75, 1.0]]);
                    let retained = if budget == 96 { 1 } else { 2 };
                    assert_eq!(session.diagnostics().processor_instances, retained);
                    assert_eq!(session.diagnostics().processor_bytes, retained * 64);
                    assert_eq!(factory.created.load(Ordering::SeqCst), 2);
                    assert_eq!(factory.dropped.load(Ordering::SeqCst), 2 - retained);
                }
            }
        }
    }

    #[test]
    fn later_recipe_budget_denial_precedes_all_instances_and_media_fetches() {
        struct CountingProvider(usize);
        impl EffectTemporalFrameProvider for CountingProvider {
            fn source_identity(&self) -> EffectTemporalSourceIdentity {
                ConstantProvider.source_identity()
            }
            fn retained_coverage_bytes(&self) -> usize {
                0
            }
            fn copy_frame(
                &mut self,
                request: EffectTemporalFrameRequest,
                cancellation: &mondrian_core::ExecutionCancellationToken,
                destination: &mut Vec<[f32; 4]>,
            ) -> std::result::Result<(), EffectTemporalFrameProviderError> {
                self.0 += 1;
                ConstantProvider.copy_frame(request, cancellation, destination)
            }
        }
        for route in ["scalar", "temporal", "mixed"] {
            let first = Arc::new(ProbeFactory::default());
            let second = Arc::new(ProbeFactory {
                resident_charge: Some(128),
                ..ProbeFactory::default()
            });
            let keys = [
                register_probe_factory(Arc::clone(&first)),
                register_probe_factory(Arc::clone(&second)),
            ];
            let mut correction = EffectNode::with_defaults(EffectType::BasicCorrection);
            correction
                .set_static_value_by_parameter(
                    &EffectType::BasicCorrection.parameter_id("exposure").expect("exposure"),
                    mondrian_core::automation::PropertyValue::Float(0.5),
                )
                .expect("set GPU tail");
            let effects = [
                instantiate_effect_node(EffectType::Plugin(keys[0].clone())).expect("first"),
                instantiate_effect_node(EffectType::Plugin(keys[1].clone())).expect("second"),
                correction,
            ];
            let graph =
                PreparedEffectProgram::prepare(&effects, &[], WorkingColorSpace::LinearRec709)
                    .expect("prepare")
                    .evaluate(TimelineTime::ZERO)
                    .expect("compile");
            let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
                max_working_bytes: 159,
                ..EffectExecutionSessionConfig::default()
            });
            session.bind_generation(1);
            let input = [[0.25, 0.5, 0.75, 1.0]];
            let mut provider = CountingProvider(0);
            let denied = match route {
                "scalar" => session
                    .apply_compiled_rgba_f32(
                        &input,
                        1,
                        1,
                        &graph,
                        0,
                        &mut crate::EffectProcessorControl::uncancelled(),
                    )
                    .is_err(),
                "temporal" => {
                    let request = EffectTemporalExecutionRequest::new(
                        1,
                        EffectExecutionContinuity::Continuous,
                        TimelineTime::ZERO,
                        EffectFrameExtent::new(1, 1),
                        EffectPixelRoi::new(0, 0, 1, 1),
                        mondrian_core::ExecutionCancellationToken::new(),
                    );
                    session.execute_temporal_f32(&graph, &request, &mut provider).is_err()
                }
                "mixed" => mixed_work(Arc::clone(&graph))
                    .execute_cpu_prefix_uncancelled(
                        &mut session,
                        1,
                        &input,
                        0,
                        WorkingColorSpace::LinearRec709,
                    )
                    .is_err(),
                _ => unreachable!(),
            };
            assert!(denied, "{route}");
            assert_eq!(
                first.created.load(Ordering::SeqCst),
                0,
                "{route}: earlier recipe must not start"
            );
            assert_eq!(second.created.load(Ordering::SeqCst), 0, "{route}");
            assert_eq!(provider.0, 0, "{route}: media must not be fetched");
            assert_eq!(session.diagnostics().processor_instances, 0);
            for key in keys {
                let status = effect_plugin_runtime_status(&key).expect("status");
                assert!(!status.disabled);
                assert_eq!(status.last_error, None);
            }
        }
    }

    #[test]
    fn working_budget_denial_preserves_definition_and_allows_reconfigured_retry() {
        let factory = Arc::new(ProbeFactory::default());
        let graph = graph(Arc::clone(&factory));
        let op = graph
            .graph()
            .nodes
            .iter()
            .find_map(|node| match &node.kind {
                EffectGraphNodeKind::UnaryEffect {
                    op: op @ EffectRenderOp::Custom { .. }, ..
                } => Some(op),
                _ => None,
            })
            .expect("operation");
        let EffectRenderOp::Custom { key, .. } = op else {
            unreachable!()
        };
        let mut config = EffectExecutionSessionConfig {
            max_working_bytes: 63,
            ..EffectExecutionSessionConfig::default()
        };
        let mut session = EffectExecutionSession::new(config);
        let original = vec![[0.25, 0.5, 0.75, 1.0]];
        let mut pixels = original.clone();
        assert_eq!(
            crate::adjustment::apply_custom_render_op_f32(
                &mut pixels,
                1,
                1,
                op,
                0,
                Some(&mut session),
                16,
                &mut EffectProcessorControl::uncancelled(),
            ),
            Err(EffectExecutionError::CustomProcessorBudgetExceeded {
                key: key.clone(),
                required_bytes: 64,
                budget_bytes: 31
            })
        );
        assert_eq!(pixels, original);
        assert_eq!(factory.created.load(Ordering::SeqCst), 0);
        let status = effect_plugin_runtime_status(key).expect("runtime status");
        assert!(!status.disabled);
        assert_eq!(status.last_error, None);
        config.max_working_bytes = 128;
        session.reconfigure(config);
        crate::adjustment::apply_custom_render_op_f32(
            &mut pixels,
            1,
            1,
            op,
            0,
            Some(&mut session),
            16,
            &mut EffectProcessorControl::uncancelled(),
        )
        .expect("retry with sufficient grant");
        assert_eq!(pixels, [[0.5, 0.5, 0.75, 1.0]]);
        assert_eq!(factory.created.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn oversized_optional_residency_bypasses_cache_and_active_charge_retires_siblings() {
        let concrete = Arc::new(ProbeFactory::default());
        let factory: Arc<dyn EffectFloatProcessorFactory> = concrete.clone();
        let mut bank = FloatProcessorResidency::default();
        let run = |bank: &mut FloatProcessorResidency, request, retention, working| {
            bank.process(
                &factory,
                request,
                1,
                8,
                retention,
                working,
                &mut vec![[0.25; 4]],
                &serde_json::json!({}),
                0,
                &mut EffectProcessorControl::uncancelled(),
            )
            .expect("admitted process");
        };
        let first =
            EffectFloatProcessorPreparation { effect_id: EffectId::new(), width: 1, height: 1 };
        run(&mut bank, first, 63, 128);
        assert_eq!(bank.counts(), (0, 0));
        assert_eq!(concrete.created.load(Ordering::SeqCst), 1);
        assert_eq!(concrete.dropped.load(Ordering::SeqCst), 1);
        run(&mut bank, first, 256, 128);
        let second = EffectFloatProcessorPreparation { effect_id: EffectId::new(), ..first };
        run(&mut bank, second, 256, 128);
        assert_eq!(bank.counts(), (2, 128));
        let third = EffectFloatProcessorPreparation { effect_id: EffectId::new(), ..first };
        run(&mut bank, third, 256, 128);
        assert_eq!(bank.counts(), (2, 128));
        assert_eq!(concrete.dropped.load(Ordering::SeqCst), 2);
        assert!(!bank.entries.iter().any(|entry| entry.0.request == first));
        // Reuse under a smaller active grant must evict the other resident
        // before entering the cached instance, just like fresh preparation.
        run(&mut bank, third, 256, 64);
        assert_eq!(bank.counts(), (1, 64));
        assert_eq!(concrete.created.load(Ordering::SeqCst), 4);
        assert_eq!(concrete.dropped.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn residency_evicts_lru_revisions_and_uncached_instances() {
        let concrete = Arc::new(ProbeFactory::default());
        let factory: Arc<dyn EffectFloatProcessorFactory> = concrete.clone();
        let mut bank = FloatProcessorResidency::default();
        let first =
            EffectFloatProcessorPreparation { effect_id: EffectId::new(), width: 1, height: 1 };
        let second = EffectFloatProcessorPreparation { effect_id: EffectId::new(), ..first };
        let third = EffectFloatProcessorPreparation { effect_id: EffectId::new(), ..first };
        let run = |bank: &mut FloatProcessorResidency, request, revision, count, bytes| {
            bank.process(
                &factory,
                request,
                revision,
                count,
                bytes,
                1024,
                &mut vec![[0.25; 4]],
                &serde_json::json!({}),
                0,
                &mut EffectProcessorControl::uncancelled(),
            )
            .expect("bounded processor");
        };
        run(&mut bank, first, 1, 2, 128);
        run(&mut bank, second, 1, 2, 128);
        run(&mut bank, first, 1, 2, 128);
        run(&mut bank, third, 1, 2, 128);
        assert_eq!(concrete.created.load(Ordering::SeqCst), 3);
        assert_eq!(concrete.dropped.load(Ordering::SeqCst), 1);
        assert!(bank.entries.iter().all(|entry| entry.0.request != second));
        run(&mut bank, first, 2, 2, 128);
        assert_eq!(concrete.created.load(Ordering::SeqCst), 4);
        assert_eq!(concrete.dropped.load(Ordering::SeqCst), 2);
        assert!(bank
            .entries
            .iter()
            .all(|entry| entry.0.request != first || entry.0.revision == 2));
        bank.trim(1, 64);
        assert_eq!(bank.counts(), (1, 64));
        bank.trim(0, 0);
        run(&mut bank, first, 2, 0, 0);
        assert_eq!(bank.counts(), (0, 0));
        assert_eq!(concrete.created.load(Ordering::SeqCst), 5);
        assert_eq!(concrete.dropped.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn residency_rejects_ungranted_allocation_and_retires_errors_and_extent_changes() {
        let concrete = Arc::new(ProbeFactory::default());
        let factory: Arc<dyn EffectFloatProcessorFactory> = concrete.clone();
        let mut bank = FloatProcessorResidency::default();
        let request =
            EffectFloatProcessorPreparation { effect_id: EffectId::new(), width: 1, height: 1 };
        let mut pixels = vec![[1.0; 4]];
        assert!(bank
            .process(
                &factory,
                request,
                1,
                1,
                128,
                63,
                &mut pixels,
                &serde_json::json!({}),
                0,
                &mut EffectProcessorControl::uncancelled(),
            )
            .is_err());
        assert_eq!(concrete.created.load(Ordering::SeqCst), 0);
        bank.process(
            &factory,
            request,
            1,
            1,
            64,
            1024,
            &mut pixels,
            &serde_json::json!({}),
            0,
            &mut EffectProcessorControl::uncancelled(),
        )
        .expect("admit");
        assert!(bank
            .process(
                &factory,
                request,
                1,
                1,
                64,
                1024,
                &mut pixels,
                &serde_json::json!({"fail": true}),
                0,
                &mut EffectProcessorControl::uncancelled(),
            )
            .is_err());
        assert_eq!(bank.counts(), (0, 0));
        assert_eq!(concrete.dropped.load(Ordering::SeqCst), 1);
        pixels = vec![[1.0; 4]];
        assert!(bank
            .process(
                &factory,
                request,
                1,
                1,
                64,
                1024,
                &mut pixels,
                &serde_json::json!({"resize": true}),
                0,
                &mut EffectProcessorControl::uncancelled(),
            )
            .is_err());
        assert_eq!(bank.counts(), (0, 0));
        pixels = vec![[1.0; 4]];
        bank.process(
            &factory,
            request,
            1,
            1,
            64,
            1024,
            &mut pixels,
            &serde_json::json!({}),
            0,
            &mut EffectProcessorControl::uncancelled(),
        )
        .expect("recreate");
        pixels = vec![[1.0; 4]; 2];
        let changed = EffectFloatProcessorPreparation { width: 2, ..request };
        bank.process(
            &factory,
            changed,
            1,
            1,
            64,
            1024,
            &mut pixels,
            &serde_json::json!({}),
            0,
            &mut EffectProcessorControl::uncancelled(),
        )
        .expect("changed extent");
        assert_eq!(bank.counts(), (1, 64));
        assert_eq!(concrete.dropped.load(Ordering::SeqCst), 3);
        bank.trim(0, 0);
        assert_eq!(bank.counts(), (0, 0));
    }
}
