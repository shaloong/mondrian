//! New private allocation contract. Not an old-API RED or native qualification.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct StorageCountingResolver(Arc<AtomicUsize>);
struct StorageCountingFactory {
    inner: Arc<dyn AudioProcessorFactory>,
    creations: Arc<AtomicUsize>,
}
impl AudioProcessorResolver for StorageCountingResolver {
    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        Ok(Arc::new(StorageCountingFactory {
            inner: BuiltInAudioProcessorResolver.prepare(request)?,
            creations: Arc::clone(&self.0),
        }))
    }
}
impl AudioProcessorFactory for StorageCountingFactory {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        self.inner.execution_contract()
    }
    fn auxiliary_input_contract(&self) -> AudioProcessorAuxiliaryInputContract {
        self.inner.auxiliary_input_contract()
    }
    fn auxiliary_input_names(&self) -> Vec<AudioProcessorInputName> {
        self.inner.auxiliary_input_names()
    }
    fn visit_host_owned_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(
            AudioProcessorFactoryStorageBorrow<'a>,
        ) -> Result<(), AudioProcessorFactoryStorageError>,
    ) -> Result<(), AudioProcessorFactoryStorageError> {
        visitor(AudioProcessorFactoryStorageBorrow::arc_payload(&self.inner))?;
        visitor(AudioProcessorFactoryStorageBorrow::arc_payload(
            &self.creations,
        ))?;
        self.inner.visit_host_owned_storage(visitor)
    }
    fn create(
        &self,
        native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    ) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        self.creations.fetch_add(1, Ordering::SeqCst);
        self.inner.create(native_child_settlement)
    }
}
fn storage_counted_plan(creations: &Arc<AtomicUsize>) -> Arc<PreparedAudioPlan> {
    let sequence = sequence_with_parallel_sample_delay(2);
    let compiled = Arc::new(
        compile_audio_program(
            &sequence,
            AudioCompileRequest::program(sequence.audio_program.outputs[0].id),
        )
        .expect("sole canonical Program"),
    );
    Arc::new(
        PreparedAudioPlan::prepare_with_processor_resolver(
            compiled,
            AudioRenderContract {
                sample_rate: 2,
                channel_layout: AudioChannelLayout::Mono,
                max_block_frames: 5,
                processing_mode: AudioProcessingMode::Offline,
                processor_session_scratch_budget_bytes:
                    AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
                public_output_lookahead_budget_frames:
                    AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
                compensation_delay_scratch_budget_bytes:
                    AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
            },
            AudioKernelBackend::RuntimeVectorized,
            &StorageCountingResolver(Arc::clone(creations)),
        )
        .expect("actual prepared built-in delay Plan"),
    )
}

#[test]
fn session_storage_preparation_and_retirement_create_no_processor() {
    let creations = Arc::new(AtomicUsize::new(0));
    let plan = storage_counted_plan(&creations);
    let storage = AudioRenderSession::allocate_storage(Arc::clone(&plan))
        .expect("canonical buffers without actual native creation");
    assert_eq!(creations.load(Ordering::SeqCst), 0);
    let capacity = storage.capacity();
    assert_eq!(capacity.processor_occurrences, 1);
    assert_eq!(capacity.processor_session_scratch_bytes, 8);
    assert_eq!(capacity.meter_target_count, 3);
    assert_eq!(capacity.meter_channel_state_count, 3);
    drop(storage);
    assert_eq!(
        creations.load(Ordering::SeqCst),
        0,
        "unpublished buffer retirement must not create a replacement instance"
    );
    let session = AudioRenderSession::new(plan).expect("normal sole constructor");
    assert_eq!(session.capacity(), capacity);
    assert_eq!(creations.load(Ordering::SeqCst), 1);
}

#[test]
fn session_storage_population_preserves_exact_delay_pcm_and_meter_envelope() {
    let creations = Arc::new(AtomicUsize::new(0));
    let plan = storage_counted_plan(&creations);
    let storage = AudioRenderSession::allocate_storage(plan).expect("canonical storage");
    assert_eq!(creations.load(Ordering::SeqCst), 0);
    let capacity = storage.capacity();
    let mut session = storage.populate().expect("one actual factory population");
    assert_eq!(creations.load(Ordering::SeqCst), 1);
    assert_eq!(session.capacity(), capacity);
    session
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(1),
            start_sample: 0,
        })
        .expect("ordinary state entry");
    let mut pcm = [91.0; 5];
    session
        .render_into(
            &mut RampSource::default(),
            AudioRenderRequest { transport_activity: None, start_sample: 0, frames: 5 },
            &mut pcm,
        )
        .expect("actual canonical delay processing");
    assert_eq!(
        pcm,
        [1.0, 2.0, 4.0, 6.0, 8.0],
        "independent audible delay oracle"
    );
    let meter = session.latest_meter_frame();
    assert_eq!(meter.block_serial, 1);
    assert_eq!(meter.start_sample, 0);
    assert_eq!(meter.frames, 5);
    assert_eq!(creations.load(Ordering::SeqCst), 1);
}

#[test]
fn session_storage_invalid_scratch_contract_refuses_before_factory_creation() {
    let creations = Arc::new(AtomicUsize::new(0));
    let mut plan = storage_counted_plan(&creations);
    Arc::make_mut(&mut plan).schedule.summary.processor_session_scratch_bytes += 1;
    assert!(matches!(
        AudioRenderSession::allocate_storage(plan),
        Err(AudioExecutionError::InvalidPreparedSchedule)
    ));
    assert_eq!(
        creations.load(Ordering::SeqCst),
        0,
        "invalid prepared scratch must fail before any factory callback"
    );
}
