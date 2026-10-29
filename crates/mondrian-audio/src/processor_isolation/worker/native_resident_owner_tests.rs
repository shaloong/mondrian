//! Known resident header and sole Root admission controls, without native loading.
use super::super::{IsolatedAudioProcessorFactory, IsolatedAudioProcessorWorkerSpec};
use super::*;
use crate::{
    AudioMediaResolver, AudioProcessor, AudioProcessorExecutionContract, AudioProcessorFactory,
    AudioProcessorFactoryStorageBorrow, AudioProcessorFactoryStorageError,
    AudioProcessorPrepareRequest, AudioProcessorResolver, AudioProgramRuntime,
    AudioRuntimeBuildError, AudioRuntimeResourceCategory, AudioRuntimeResourceGrant,
    BuiltInAudioProcessorResolver, NativeHostBufferLayout, NativeWorkerControlOverlapLayout,
    NativeWorkerLaneOwnedLayouts, NativeWorkerLaneResourceProfile, ResolvedAudioSource,
};
use mondrian_core::{AssetId, AudioChannelLayout, AudioSourceComponentId};
use mondrian_timeline::audio::{AudioProcessorInstance, BUILTIN_GAIN_DEFINITION_ID};
use mondrian_timeline::Sequence;
use std::alloc::Layout;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn measured(layout: Layout) -> NativeHostBufferLayout {
    NativeHostBufferLayout { bytes: layout.size(), alignment: layout.align() }
}

fn profile(resident: NativeHostBufferLayout) -> NativeWorkerLaneResourceProfile {
    let empty = NativeHostBufferLayout { bytes: 0, alignment: 1 };
    NativeWorkerLaneResourceProfile::scoped_main_dsp(
        NativeWorkerLaneOwnedLayouts {
            adapter_resident: resident,
            reader_slots: empty,
            processing_slots: empty,
            main_inline: empty,
            runtime_main_role: empty,
            runtime_dsp_role: empty,
            sdk_main_role: empty,
            sdk_dsp_role: empty,
        },
        NativeWorkerControlOverlapLayout {
            decoded_input: empty,
            reply_storage: empty,
            staged_seed: empty,
        },
    )
    .unwrap()
}

fn render() -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 8,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: usize::MAX,
        public_output_lookahead_budget_frames: usize::MAX,
        compensation_delay_scratch_budget_bytes: usize::MAX,
    }
}

struct ResidentWorker {
    _inline_metadata: [u8; 257],
    drops: Arc<AtomicUsize>,
}
impl Drop for ResidentWorker {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}
impl IsolatedAudioProcessorWorker for ResidentWorker {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        AudioProcessorExecutionContract::stateless()
    }
    fn auxiliary_input_contract(&self) -> crate::AudioProcessorAuxiliaryInputContract {
        Default::default()
    }
    fn enter_state(&mut self, _: i64) -> Result<(), AudioProcessorHostError> {
        panic!("header preparation must not enter DSP")
    }
    fn process(
        &mut self,
        _: IsolatedAudioProcessorWorkerBlock<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        panic!("header preparation must not process")
    }
}

struct ResidentFactory {
    expected: NativeWorkerLaneResourceProfile,
    prepares: AtomicUsize,
    drops: Arc<AtomicUsize>,
}
impl IsolatedAudioProcessorWorkerFactory for ResidentFactory {
    fn native_worker_lane_resources(
        &self,
    ) -> Result<Option<NativeWorkerLaneResourceProfile>, AudioProcessorHostError> {
        Ok(Some(self.expected))
    }
    fn prepare(
        &self,
        _: IsolatedAudioProcessorWorkerPrepareRequest,
    ) -> Result<Box<dyn IsolatedAudioProcessorWorker>, AudioProcessorHostError> {
        self.prepares.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(ResidentWorker {
            _inline_metadata: [0; 257],
            drops: Arc::clone(&self.drops),
        }))
    }
}

fn request(profile: NativeWorkerLaneResourceProfile) -> IsolatedAudioProcessorWorkerPrepareRequest {
    IsolatedAudioProcessorWorkerPrepareRequest {
        musical_time_reference: None,
        native_lane: Some(profile),
        payload: Vec::new(),
        plugin_execution_contract: AudioProcessorExecutionContract::stateless(),
        auxiliary_inputs: Default::default(),
        auxiliary_outputs: Default::default(),
        note_input_contract: Default::default(),
        parameter_ids: Vec::new(),
        render_contract: render(),
    }
}

#[test]
fn resident_header_profile_mismatch_refuses_before_factory_prepare() {
    let actual = measured(Layout::new::<ResidentWorker>());
    let factory = ResidentFactory {
        expected: profile(actual),
        prepares: AtomicUsize::new(0),
        drops: Arc::new(AtomicUsize::new(0)),
    };
    let smaller = NativeHostBufferLayout { bytes: actual.bytes - 1, ..actual };
    let error = prepare_worker(&factory, request(profile(smaller))).err().unwrap();
    assert_eq!(
        error,
        "parent and child preparation-specific native storage requests differ"
    );
    assert_eq!(factory.prepares.load(Ordering::SeqCst), 0);
    assert_eq!(factory.drops.load(Ordering::SeqCst), 0);
}

#[test]
fn resident_header_checks_complete_dynamic_box_before_ready_and_retires_rejected_owner() {
    let actual = measured(Layout::new::<ResidentWorker>());
    assert!(actual.bytes > size_of::<Box<dyn IsolatedAudioProcessorWorker>>());
    let mut factory = ResidentFactory {
        expected: profile(NativeHostBufferLayout { bytes: actual.bytes - 1, ..actual }),
        prepares: AtomicUsize::new(0),
        drops: Arc::new(AtomicUsize::new(0)),
    };
    let error = prepare_worker(&factory, request(factory.expected)).err().unwrap();
    assert!(error.contains("AdapterResident"), "{error}");
    assert_eq!(factory.prepares.load(Ordering::SeqCst), 1);
    assert_eq!(factory.drops.load(Ordering::SeqCst), 1);
    factory.expected = profile(actual);
    let worker = prepare_worker(&factory, request(factory.expected)).unwrap();
    assert_eq!(measured(Layout::for_value(worker.as_ref())), actual);
    assert_eq!(factory.prepares.load(Ordering::SeqCst), 2);
    assert_eq!(factory.drops.load(Ordering::SeqCst), 1);
    drop(worker);
    assert_eq!(factory.drops.load(Ordering::SeqCst), 2);
}

struct ChargedResolver {
    profile: NativeWorkerLaneResourceProfile,
    creates: Arc<AtomicUsize>,
}
struct ChargedFactory {
    inner: Arc<dyn AudioProcessorFactory>,
    contract: AudioProcessorExecutionContract,
    creates: Arc<AtomicUsize>,
}
impl AudioProcessorResolver for ChargedResolver {
    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        let render = request.render_contract();
        let parameters = request.parameters().keys().cloned().collect::<Vec<_>>();
        let inner = BuiltInAudioProcessorResolver.prepare(request)?;
        let spec = IsolatedAudioProcessorWorkerSpec::new(
            std::env::current_exe().unwrap(),
            Vec::new(),
            inner.execution_contract(),
            inner.auxiliary_input_contract(),
            parameters,
            render,
        )?
        .with_native_worker_lane_resources(Some(self.profile));
        let layout = SharedBlockLayout::uniform(
            render.max_block_frames,
            render.channel_count(),
            spec.auxiliary_inputs.buses.len(),
            spec.parameter_ids.len(),
        )?;
        // Same pure fee path as IsolatedFactory::prepare; no validation child is launched.
        let contract = IsolatedAudioProcessorFactory::requested_execution_contract(&spec, layout)?;
        Ok(Arc::new(ChargedFactory {
            inner,
            contract,
            creates: Arc::clone(&self.creates),
        }))
    }
}
impl AudioProcessorFactory for ChargedFactory {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        self.contract
    }
    fn visit_host_owned_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(
            AudioProcessorFactoryStorageBorrow<'a>,
        ) -> Result<(), AudioProcessorFactoryStorageError>,
    ) -> Result<(), AudioProcessorFactoryStorageError> {
        visitor(AudioProcessorFactoryStorageBorrow::arc_payload(&self.inner))?;
        visitor(AudioProcessorFactoryStorageBorrow::arc_payload(
            &self.creates,
        ))?;
        self.inner.visit_host_owned_storage(visitor)
    }
    fn create(
        &self,
        native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    ) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        self.inner.create(native_child_settlement)
    }
}
struct NoMedia;
impl AudioMediaResolver for NoMedia {
    fn resolve(
        &self,
        _: AssetId,
        _: AudioSourceComponentId,
        _: u32,
    ) -> Result<ResolvedAudioSource, String> {
        panic!("this canonical Program has no media")
    }
}

#[test]
fn resident_header_fee_enters_sole_root_fixed_grant_before_session_factory_create() {
    let mut sequence = Sequence::new("known native resident header");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(
        AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1),
    );
    let creates = Arc::new(AtomicUsize::new(0));
    let resident = measured(Layout::new::<ResidentWorker>());
    let resolver = ChargedResolver {
        profile: profile(resident),
        creates: Arc::clone(&creates),
    };
    let build = |resolver: &ChargedResolver, grant| {
        AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
            &sequence,
            &[],
            &NoMedia,
            resolver,
            render(),
            None,
            grant,
        )
    };
    let baseline = build(
        &resolver,
        AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, usize::MAX),
    )
    .unwrap();
    let footprint = baseline.resource_footprint();
    assert_eq!(creates.load(Ordering::SeqCst), 1);
    drop(baseline);
    let without_header = ChargedResolver {
        profile: profile(NativeHostBufferLayout { bytes: 0, alignment: 1 }),
        creates: Arc::clone(&creates),
    };
    let old = build(
        &without_header,
        AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, usize::MAX),
    )
    .unwrap();
    assert_eq!(
        footprint.fixed_resident_bytes - old.resource_footprint().fixed_resident_bytes,
        resident.bytes
    );
    drop(old);
    creates.store(0, Ordering::SeqCst);
    assert!(matches!(
        build(
            &resolver,
            AudioRuntimeResourceGrant::new(
                footprint.runtime_occurrences,
                footprint.fixed_resident_bytes - 1,
                footprint.prepared_logical_bytes,
            )
        ),
        Err(AudioRuntimeBuildError::ResourceGrantExceeded {
            category: AudioRuntimeResourceCategory::FixedResidentBytes,
            ..
        })
    ));
    assert_eq!(creates.load(Ordering::SeqCst), 0);
    let exact = build(
        &resolver,
        AudioRuntimeResourceGrant::new(
            footprint.runtime_occurrences,
            footprint.fixed_resident_bytes,
            footprint.prepared_logical_bytes,
        ),
    )
    .unwrap();
    assert_eq!(exact.resource_footprint(), footprint);
    assert_eq!(creates.load(Ordering::SeqCst), 1);
}

#[test]
fn adapter_note_backing_fee_enters_sole_root_grant_once_before_factory_create() {
    // Synthetic storage-fee control through the real canonical Root admission;
    // this is not a canonical VST Note processing or complete heap qualification.
    let baseline = profile(measured(Layout::new::<ResidentWorker>()));
    let backing = measured(Layout::array::<vst3_host::PluginEvent>(4096).unwrap());
    assert_storage_fee_admission(
        baseline,
        baseline.with_adapter_note_input_storage(backing).unwrap(),
        backing.bytes,
    );
}

#[test]
fn processing_audio_buffer_peak_enters_sole_root_grant_once_before_factory_create() {
    // Compose with existing nonzero input and Note storage. This verifies the
    // original canonical grant, not discovery/manual validation launch order.
    let baseline = profile(measured(Layout::new::<ResidentWorker>()))
        .with_processor_input_storage(64, 32)
        .unwrap()
        .with_adapter_note_input_storage(measured(
            Layout::array::<vst3_host::PluginEvent>(16).unwrap(),
        ))
        .unwrap();
    let peak = vst3_host::audio::ProcessingResourceBounds::new(32, 2, 128, 8)
        .unwrap()
        .host_buffer_peak_bytes()
        .unwrap();
    assert!(peak > 0);
    assert_storage_fee_admission(
        baseline,
        baseline.with_processing_audio_buffer_peak_bytes(peak).unwrap(),
        peak,
    );
}

fn assert_storage_fee_admission(
    baseline: NativeWorkerLaneResourceProfile,
    charged: NativeWorkerLaneResourceProfile,
    expected_delta: usize,
) {
    let mut sequence = Sequence::new("known adapter Note backing");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Mono;
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(
        AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1),
    );
    let creates = Arc::new(AtomicUsize::new(0));
    let resolver = ChargedResolver { profile: charged, creates: Arc::clone(&creates) };
    let build = |resolver: &ChargedResolver, grant| {
        AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
            &sequence,
            &[],
            &NoMedia,
            resolver,
            render(),
            None,
            grant,
        )
    };
    let unlimited = AudioRuntimeResourceGrant::new(usize::MAX, usize::MAX, usize::MAX);
    let runtime = build(&resolver, unlimited).unwrap();
    let footprint = runtime.resource_footprint();
    drop(runtime);
    let no_backing = ChargedResolver { profile: baseline, creates: Arc::clone(&creates) };
    let uncharged = build(&no_backing, unlimited).unwrap();
    assert_eq!(
        footprint.fixed_resident_bytes - uncharged.resource_footprint().fixed_resident_bytes,
        expected_delta
    );
    drop(uncharged);
    creates.store(0, Ordering::SeqCst);
    assert!(matches!(
        build(
            &resolver,
            AudioRuntimeResourceGrant::new(
                footprint.runtime_occurrences,
                footprint.fixed_resident_bytes - 1,
                footprint.prepared_logical_bytes,
            )
        ),
        Err(AudioRuntimeBuildError::ResourceGrantExceeded {
            category: AudioRuntimeResourceCategory::FixedResidentBytes,
            ..
        })
    ));
    assert_eq!(creates.load(Ordering::SeqCst), 0);
    let exact = build(
        &resolver,
        AudioRuntimeResourceGrant::new(
            footprint.runtime_occurrences,
            footprint.fixed_resident_bytes,
            footprint.prepared_logical_bytes,
        ),
    )
    .unwrap();
    assert_eq!(exact.resource_footprint(), footprint);
    assert_eq!(creates.load(Ordering::SeqCst), 1);
}
