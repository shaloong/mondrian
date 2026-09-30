//! Execution-owned, bounded reuse of frame-independent Float32 processors.

use mondrian_core::{EffectId, MondrianError, Result};
use std::collections::VecDeque;
use std::sync::Arc;

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
    ) -> Result<()>;
}

/// Immutable recipe retained by a prepared program, without shared mutable
/// native instances. Each execution session prepares its own instance.
pub trait EffectFloatProcessorFactory: std::fmt::Debug + Send + Sync {
    /// Conservative resident byte charge, including owned worker/frame buffers.
    /// This charge is checked before constructing the instance.
    fn resident_bytes(&self, request: EffectFloatProcessorPreparation) -> Result<usize>;
    /// Create one instance for this occurrence and exact frame extent.
    fn prepare(
        &self,
        request: EffectFloatProcessorPreparation,
    ) -> Result<Box<dyn EffectFloatProcessorInstance>>;
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

    pub(crate) fn trim(&mut self, max_instances: usize, max_bytes: usize) {
        while self.entries.len() > max_instances || self.bytes > max_bytes {
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
        max_working_bytes: usize,
        pixels: &mut Vec<[f32; 4]>,
        params: &serde_json::Value,
        frame_seed: i64,
    ) -> Result<()> {
        let count = (request.width as usize).checked_mul(request.height as usize);
        if request.width == 0 || request.height == 0 || count != Some(pixels.len()) {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "effect_processor_frame".to_owned(),
                reason: "processor frame extent does not match its pixels".to_owned(),
            });
        }
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
        let (charge, mut instance) = if let Some((_, charge, instance)) = cached {
            self.bytes -= charge;
            (charge, instance)
        } else {
            let charge = factory.resident_bytes(request)?;
            if charge > max_working_bytes || (max_instances > 0 && charge > max_bytes) {
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "effect_processor_budget".to_owned(),
                    reason: "processor resident charge exceeds the execution grant".to_owned(),
                });
            }
            // Retire old workers before allocating a replacement.
            while !self.entries.is_empty()
                && (self.entries.len() >= max_instances
                    || charge > max_bytes.saturating_sub(self.bytes))
            {
                self.evict_oldest();
            }
            (charge, factory.prepare(request)?)
        };
        // An error or unwind drops the removed instance; damaged state cannot
        // survive into the next call. The caller stages pixels atomically.
        if charge > max_working_bytes {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "effect_processor_budget".to_owned(),
                reason: "processor resident charge exceeds the execution grant".to_owned(),
            });
        }
        let pixel_count = pixels.len();
        instance.process(pixels, params, frame_seed)?;
        if pixels.len() != pixel_count {
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "effect_processor_frame".to_owned(),
                reason: "processor changed the pixel count".to_owned(),
            });
        }
        if max_instances > 0 {
            self.bytes += charge;
            self.entries.push_back((key, charge, instance));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;
    use mondrian_core::{automation::PropertyBag, TimelineTime, WorkingColorSpace};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Default)]
    struct ProbeFactory {
        created: Arc<AtomicUsize>,
        dropped: Arc<AtomicUsize>,
    }
    #[derive(Debug)]
    struct ProbeInstance {
        dropped: Arc<AtomicUsize>,
    }
    impl Drop for ProbeInstance {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl EffectFloatProcessorFactory for ProbeFactory {
        fn resident_bytes(&self, _: EffectFloatProcessorPreparation) -> Result<usize> {
            Ok(64)
        }
        fn prepare(
            &self,
            _: EffectFloatProcessorPreparation,
        ) -> Result<Box<dyn EffectFloatProcessorInstance>> {
            self.created.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(ProbeInstance {
                dropped: Arc::clone(&self.dropped),
            }))
        }
    }
    impl EffectFloatProcessorInstance for ProbeInstance {
        fn process(
            &mut self,
            pixels: &mut Vec<[f32; 4]>,
            params: &serde_json::Value,
            _: i64,
        ) -> Result<()> {
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
            if params["resize"].as_bool() == Some(true) {
                pixels.clear();
                return Ok(());
            }
            for pixel in pixels {
                pixel[0] *= 2.0;
            }
            Ok(())
        }
    }

    fn graph(factory: Arc<ProbeFactory>) -> Arc<CompiledEffectGraph> {
        graph_with_tail(factory, false)
    }

    fn graph_with_tail(factory: Arc<ProbeFactory>, gpu_tail: bool) -> Arc<CompiledEffectGraph> {
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
        .with_custom_float_processor_factory(
            Arc::new(|_, _| Ok(Some(serde_json::json!({})))),
            None,
            EffectCachePolicy::Uncacheable,
            factory,
        );
        register_effect_definition(definition).expect("register factory");
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
    fn compiled_factory_instances_are_separate_for_occurrences_consumers_and_generations() {
        let factory = Arc::new(ProbeFactory::default());
        let graph = graph(Arc::clone(&factory));
        let mut preview = EffectExecutionSession::default();
        let mut export = EffectExecutionSession::default();
        let input = [[0.25, 0.5, 0.75, 1.0]];
        for _ in 0..2 {
            assert_eq!(
                preview.apply_compiled_rgba_f32(&input, 1, 1, &graph, 0).expect("preview"),
                [[1.0, 0.5, 0.75, 1.0]]
            );
        }
        assert_eq!(factory.created.load(Ordering::SeqCst), 2);
        assert_eq!(preview.diagnostics().processor_instances, 2);
        assert_eq!(preview.diagnostics().processor_bytes, 128);
        export.apply_compiled_rgba_f32(&input, 1, 1, &graph, 0).expect("export");
        assert_eq!(factory.created.load(Ordering::SeqCst), 4);
        preview.bind_generation(1);
        assert_eq!(preview.diagnostics().processor_instances, 0);
        assert_eq!(export.diagnostics().processor_instances, 2);
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
        for key in ["fail", "resize", "panic"] {
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
                Some(&mut session)
            )
            .is_err());
            assert_eq!(working, original);
            assert_eq!(session.diagnostics().processor_instances, 0);
        }
        assert_eq!(factory.created.load(Ordering::SeqCst), 3);
        assert_eq!(factory.dropped.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn heterogeneous_cpu_prefix_reuses_session_owned_factory_instances() {
        let factory = Arc::new(ProbeFactory::default());
        let graph = graph_with_tail(Arc::clone(&factory), true);
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
        let work = PreparedHeterogeneousEffectWork::prepare(
            graph,
            &environment,
            EffectGraphExecutionRequest::new(
                EffectFrameExtent::new(1, 1),
                EffectValueResidency::new(cpu, format),
                EffectValueResidency::new(gpu, format),
                EffectGraphExecutionBudget::new(1024 * 1024, 1024 * 1024, 1024 * 1024, 128, 256),
            ),
        )
        .expect("prepare mixed route");
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
            destination.resize(
                (roi.width() * roi.height()) as usize,
                [0.25, 0.5, 0.75, 1.0],
            );
            Ok(())
        }
    }

    #[test]
    fn temporal_execution_reuses_occurrences_without_crossing_generations() {
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
            assert_eq!(
                factory.created.load(Ordering::SeqCst),
                generation as usize * 2
            );
        }
        assert_eq!(factory.dropped.load(Ordering::SeqCst), 2);
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
                63,
                1024,
                &mut pixels,
                &serde_json::json!({}),
                0
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
                0
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
                0
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
        )
        .expect("changed extent");
        assert_eq!(bank.counts(), (1, 64));
        assert_eq!(concrete.dropped.load(Ordering::SeqCst), 3);
        bank.trim(0, 0);
        assert_eq!(bank.counts(), (0, 0));
    }
}
