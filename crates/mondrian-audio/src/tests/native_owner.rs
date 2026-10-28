//! Portable Runtime/Session owner contracts with an explicit test ABI adapter.
//! These gates do not prove native COM, physical GUI or real DSP-thread roles.

use super::*;
use std::num::{NonZeroU32, NonZeroU64};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

#[derive(Default)]
struct OwnerProbeState {
    next_load: AtomicU64,
    created: AtomicUsize,
    binds: AtomicUsize,
    process_author: AtomicBool,
    processes: AtomicUsize,
    process_starts: std::sync::Mutex<Vec<i64>>,
    process_output: std::sync::Mutex<Option<(i64, u32)>>,
    bind_source: std::sync::Mutex<Option<mondrian_core::AudioOwnerInstallationSource>>,
    bind_cancel: std::sync::Mutex<Option<ExecutionCancellationToken>>,
    keep_first_object: AtomicBool,
    entries: AtomicUsize,
    reject_binding: AtomicBool,
    replace_during_validation: AtomicBool,
    object_observations: AtomicUsize,
    observation_revoke_source:
        std::sync::Mutex<Option<mondrian_core::AudioOwnerInstallationSource>>,
    parameter_queries: AtomicUsize,
    query_revoke_source: std::sync::Mutex<Option<mondrian_core::AudioOwnerInstallationSource>>,
    query_cancel: std::sync::Mutex<Option<ExecutionCancellationToken>>,
    editor_opens: AtomicUsize,
    editor_closes: AtomicUsize,
    editor_cleanup: AtomicBool,
    snapshot_calls: AtomicUsize,
    pending_queries: AtomicUsize,
    pending_restart: AtomicBool,
    native_author: std::sync::Mutex<Option<crate::NativeAudioAuthorObservation>>,
}

struct OwnerProbeResolver(Arc<OwnerProbeState>);
struct OwnerProbeFactory {
    state: Arc<OwnerProbeState>,
    contract: AudioProcessorExecutionContract,
}
struct OwnerProbeProcessor {
    state: Arc<OwnerProbeState>,
    serial: NonZeroU64,
    binding: Option<NativeAudioObjectBindReceipt>,
}

impl AudioProcessorResolver for OwnerProbeResolver {
    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        if !matches!(request.definition(), mondrian_timeline::AudioProcessorDefinitionRef::Clap { plugin_id, .. } if plugin_id == "test.mondrian.delay")
        {
            return BuiltInAudioProcessorResolver.prepare(request);
        }
        Ok(Arc::new(OwnerProbeFactory {
            state: self.0.clone(),
            contract: AudioProcessorExecutionContract::new(
                0,
                AudioProcessorTail::None,
                true,
                true,
                true,
                0,
            )?,
        }))
    }
}

fn next_probe_load(state: &OwnerProbeState) -> NonZeroU64 {
    NonZeroU64::new(state.next_load.fetch_add(1, Ordering::AcqRel) + 1)
        .expect("bounded test load serial")
}

impl AudioProcessorFactory for OwnerProbeFactory {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        self.contract
    }
    fn create(
        &self,
        _native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    ) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        self.state.created.fetch_add(1, Ordering::AcqRel);
        Ok(Box::new(OwnerProbeProcessor {
            state: self.state.clone(),
            serial: next_probe_load(&self.state),
            binding: None,
        }))
    }
}

impl OwnerProbeProcessor {
    fn object(&self) -> NativeAudioObjectObservation {
        NativeAudioObjectObservation::new(
            NonZeroU32::new(std::process::id()).expect("test adapter PID"),
            self.serial,
        )
    }
}

impl AudioProcessor for OwnerProbeProcessor {
    fn native_observations_supported(&self) -> bool {
        true
    }
    fn poll_native_observations(
        &mut self,
        consumption: NativeAudioObservationConsumption,
    ) -> Result<NativeAudioObservations, AudioProcessorHostError> {
        Ok(NativeAudioObservations {
            object: Some(self.object()),
            author: {
                let mut author =
                    self.state.native_author.lock().expect("actual test source").clone();
                if self.state.process_author.load(Ordering::Acquire) {
                    match consumption {
                        NativeAudioObservationConsumption::DrainAuthor(request)
                            if self.binding.is_some_and(|binding| {
                                binding.binding() == request.binding
                                    && binding.object() == request.expected_object
                            }) =>
                        {
                            if let Some((start, rate)) = *self
                                .state
                                .process_output
                                .lock()
                                .expect("actual accepted raw output")
                                && let Some(author) = &mut author
                            {
                                author.delivery = crate::NativeAudioAuthorDeliveryState::Delivered(
                                    crate::NativeAudioAuthorDelivery {
                                        binding: request,
                                        revision: 1,
                                        request_cohort: None,
                                        events: vec![crate::NativeAudioAuthorEvent::ParameterAtExecution {
                                            parameter: crate::NativeAudioAuthorParameterEvent::admit(
                                                ParameterId::new("clap.param.1").expect("stable native ID"),
                                                crate::NativeAudioAuthorParameterPhase::Value,
                                                Some(crate::NativeAudioAuthorScalar::admit(0.625).expect("actual plain value")),
                                            ).expect("legal unwrapped CLAP value"),
                                            sample_position: start,
                                            sample_rate: rate,
                                        }],
                                        configuration: None,
                                        completion: mondrian_core::NativeAudioAuthorCompletion::ParametersOnly,
                                    },
                                );
                            }
                        }
                        NativeAudioObservationConsumption::Preserve => {
                            if let Some(author) = &mut author {
                                author.delivery = crate::NativeAudioAuthorDeliveryState::Preserved;
                            }
                        }
                        _ => author = None,
                    }
                }
                author
            },
            editor_cleanup: self
                .state
                .editor_cleanup
                .load(Ordering::Acquire)
                .then_some(NativeAudioEditorCloseStage::ClearHostFrame),
            ..Default::default()
        })
    }
    fn preflight_reprepare(
        &mut self,
    ) -> Result<Option<crate::NativeObservedRequest>, AudioProcessorHostError> {
        self.state.pending_queries.fetch_add(1, Ordering::AcqRel);
        Ok(
            self.state.pending_restart.load(Ordering::Acquire).then_some(
                crate::NativeObservedRequest::RequestOnly(
                    mondrian_core::NativeAudioReprepareRequest::ClapRestart,
                ),
            ),
        )
    }
    fn preflight_native_observations(
        &mut self,
        _expected_object: NativeAudioObjectObservation,
    ) -> Result<NativeAudioObservations, AudioProcessorHostError> {
        self.state.snapshot_calls.fetch_add(1, Ordering::AcqRel);
        let mut observations =
            self.poll_native_observations(NativeAudioObservationConsumption::Preserve)?;
        observations.pending_reprepare =
            self.state.pending_restart.load(Ordering::Acquire).then_some(
                crate::NativeObservedRequest::RequestOnly(
                    mondrian_core::NativeAudioReprepareRequest::ClapRestart,
                ),
            );
        Ok(observations)
    }
    fn query_native_parameter(
        &mut self,
        binding: NativeAudioObjectBindRequest,
        id: u32,
    ) -> Result<crate::NativeAudioParameterQueryResult, AudioProcessorHostError> {
        if self.binding.is_none_or(|receipt| {
            receipt.binding() != binding.binding || receipt.object() != self.object()
        }) || binding.expected_object != self.object()
        {
            return Ok(Err(
                mondrian_core::NativeAudioParameterQueryRefusal::OwnerChanged,
            ));
        }
        self.state.parameter_queries.fetch_add(1, Ordering::AcqRel);
        if let Some(source) = self.state.query_revoke_source.lock().unwrap().take() {
            source.revoke().unwrap();
        }
        if let Some(cancel) = self.state.query_cancel.lock().unwrap().take() {
            cancel.cancel();
        }
        Ok(Ok((
            3,
            vec![mondrian_core::NativeAudioParameterFact {
                revision: 3,
                parameter: ParameterId::new(format!("vst3.param.{id}")).unwrap(),
                event_flags: 0,
                value_bits: Some(0.25_f64.to_bits()),
                author_value_bits: Some(0.25_f64.to_bits()),
                origin: mondrian_core::NativeAudioParameterObservationOrigin::MainQuery {
                    rescan_flags: 0,
                },
                hidden: Some(false),
                periodic: None,
                name: Some("Actual probe caption".into()),
                module: None,
                info_flags: Some(1),
                display: Some("0.25".into()),
            }],
        )))
    }
    fn open_native_audio_editor(
        &mut self,
        request: NativeAudioEditorOpenRequest,
    ) -> Result<NativeAudioEditorOutcome, AudioProcessorHostError> {
        if request.binding().expected_object != self.object() {
            return Ok(NativeAudioEditorOutcome::Refused(
                NativeAudioEditorRefusal::OwnerChanged,
            ));
        }
        self.state.editor_opens.fetch_add(1, Ordering::AcqRel);
        Ok(NativeAudioEditorOutcome::Opened {
            binding: request.binding(),
            width: 320,
            height: 160,
        })
    }
    fn close_native_audio_editor(
        &mut self,
        binding: NativeAudioObjectBindRequest,
    ) -> Result<NativeAudioEditorOutcome, AudioProcessorHostError> {
        if binding.expected_object != self.object() {
            return Ok(NativeAudioEditorOutcome::Refused(
                NativeAudioEditorRefusal::OwnerChanged,
            ));
        }
        self.state.editor_closes.fetch_add(1, Ordering::AcqRel);
        self.state.editor_cleanup.store(false, Ordering::Release);
        Ok(NativeAudioEditorOutcome::Closed { binding })
    }
    fn native_audio_object_observation(
        &mut self,
    ) -> Result<Option<NativeAudioObjectObservation>, AudioProcessorHostError> {
        self.state.object_observations.fetch_add(1, Ordering::AcqRel);
        if self.state.replace_during_validation.swap(false, Ordering::AcqRel) {
            self.serial = next_probe_load(&self.state);
        }
        if let Some(source) = self
            .state
            .observation_revoke_source
            .lock()
            .expect("actual observation Source revocation")
            .take()
        {
            source.revoke().expect("actual Source revoked during native observation");
        }
        Ok(Some(self.object()))
    }
    fn bind_native_audio_object(
        &mut self,
        request: NativeAudioObjectBindRequest,
    ) -> Result<NativeAudioObjectBindOutcome, AudioProcessorHostError> {
        if self.state.reject_binding.load(Ordering::Acquire) {
            return Ok(NativeAudioObjectBindOutcome::Refused(
                NativeAudioObjectBindRefusal::EditorStatePending,
            ));
        }
        let receipt = NativeAudioObjectBindReceipt::acknowledge(request, self.object())
            .map_err(|_| AudioProcessorHostError::InvalidContract("test object changed".into()))?;
        self.binding = Some(receipt);
        self.state.binds.fetch_add(1, Ordering::AcqRel);
        if let Some(cancellation) =
            self.state.bind_cancel.lock().expect("actual Bind cancellation").take()
        {
            cancellation.cancel();
        }
        if let Some(source) = self.state.bind_source.lock().expect("bounded Source probe").take() {
            source.revoke().expect("actual Source revoked during native Bind");
        }
        Ok(NativeAudioObjectBindOutcome::Bound(receipt))
    }
    fn enter_state(&mut self, _start_sample: i64) -> Result<(), AudioProcessorHostError> {
        self.binding = None;
        self.state.entries.fetch_add(1, Ordering::AcqRel);
        if !self.state.keep_first_object.swap(false, Ordering::AcqRel) {
            self.serial = next_probe_load(&self.state);
        }
        Ok(())
    }
    fn process(
        &mut self,
        context: AudioProcessorProcessContext,
        audio: &mut dyn AudioProcessorAudioIo,
        _parameters: AudioParameterEventBatch<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        self.state.processes.fetch_add(1, Ordering::AcqRel);
        self.state
            .process_starts
            .lock()
            .expect("signal coordinates")
            .push(context.request().start_sample);
        if self.state.process_author.load(Ordering::Acquire) {
            for sample in audio.main_interleaved() {
                *sample *= 0.625;
            }
            let mut author = native_author_probe_source();
            author.source.revision = 1;
            author.source.pending_events = 1;
            *self.state.process_output.lock().expect("accepted raw source") =
                Some((context.request().start_sample, context.sample_rate().hz()));
            *self.state.native_author.lock().expect("retained native output") = Some(author);
        }
        Ok(())
    }
}

fn owner_probe_contract() -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 2,
        channel_layout: AudioChannelLayout::Mono,
        max_block_frames: 8,
        processing_mode: AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
        public_output_lookahead_budget_frames:
            AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
        compensation_delay_scratch_budget_bytes:
            AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
    }
}

fn owner_probe_runtime(
    root: &Sequence,
    children: &[Sequence],
    state: &Arc<OwnerProbeState>,
) -> AudioProgramRuntime {
    AudioProgramRuntime::build_with_processor_resolver(
        root,
        children,
        &RampResolver,
        &OwnerProbeResolver(state.clone()),
        owner_probe_contract(),
        None,
    )
    .expect("actual compiled whole-root Runtime")
}

#[derive(Default)]
struct OwnerAddresses {
    root: Option<AudioSessionCursorIdentity>,
    addresses: Vec<AudioNativeProcessorAddress>,
}
impl AudioContinuityEvidenceVisitor for OwnerAddresses {
    fn session(&mut self, evidence: AudioSessionCursorEvidence) -> Result<(), AudioExecutionError> {
        if self.root.is_none() {
            self.root = Some(evidence.identity);
        }
        Ok(())
    }
    fn processor(
        &mut self,
        evidence: AudioProcessorCursorEvidence,
    ) -> Result<(), AudioExecutionError> {
        let epoch = match evidence.state {
            AudioProcessorCursorState::Stateless | AudioProcessorCursorState::Unentered => None,
            AudioProcessorCursorState::WaitingForFirstSignal { epoch }
            | AudioProcessorCursorState::Active { epoch, .. } => Some(epoch),
            AudioProcessorCursorState::Failed => return Err(AudioExecutionError::BufferTooLarge),
        };
        self.addresses.push(AudioNativeProcessorAddress {
            root: self.root.expect("actual root visited before local records"),
            local: evidence.identity,
            occurrence: evidence.occurrence,
            epoch,
        });
        Ok(())
    }
}
fn owner_addresses(runtime: &AudioProgramRuntime) -> Vec<AudioNativeProcessorAddress> {
    let mut evidence = OwnerAddresses::default();
    runtime
        .visit_continuity_evidence(&mut evidence)
        .expect("read-only actual prepared/session addresses");
    evidence.addresses
}

fn install_probe_runtime(
    runtime: &mut AudioProgramRuntime,
    source: &mondrian_core::AudioOwnerInstallationSource,
) -> (
    mondrian_core::AudioOwnerInstallationVerifier,
    mondrian_core::AudioOwnerInstallationClaim,
) {
    let (registration, verifier) = source.register().expect("same actual host registration");
    runtime
        .bind_native_owner_installation(verifier.clone())
        .expect("bind actual Runtime once");
    let claim = source
        .reserve_install(
            &registration,
            source.current_claim().expect("current actual slot").as_ref(),
        )
        .and_then(|reservation| source.begin_commit(reservation))
        .expect("actual slot exchange")
        .commit()
        .expect("native-capable slot");
    runtime
        .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
        .expect("same Hosted objects acknowledge before publication");
    (verifier, claim)
}

#[test]
fn native_owner_registration_and_prepared_binding_do_not_grant_early_root_permission() {
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let address = owner_addresses(&runtime)[0];
    assert_eq!(
        runtime.prepare_native_owner_bindings(&ExecutionCancellationToken::new()).err(),
        Some(AudioNativeOwnerError::UnregisteredRuntime)
    );
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    let (registration, verifier) = source.register().expect("actual private host pair");
    runtime
        .bind_native_owner_installation(verifier.clone())
        .expect("one actual Runtime registration");
    let prepared = runtime
        .prepare_native_owner_bindings(&ExecutionCancellationToken::new())
        .expect("actual ABI ack, still no installed lease");
    assert_eq!(prepared.binding_count(), 1);
    assert!(matches!(
        runtime.native_owner_lease(address),
        Err(AudioNativeOwnerError::OwnerUnavailable)
    ));
    let claim = source
        .reserve_install(&registration, None)
        .and_then(|reservation| source.begin_commit(reservation))
        .expect("actual host slot exchange")
        .commit()
        .expect("current native-capable root");
    assert_eq!(
        runtime
            .publish_prepared_native_owner_bindings(prepared, &claim)
            .expect("bounded publication after actual swap"),
        1
    );
    let lease = runtime.native_owner_lease(address).expect("exact current Hosted record");
    runtime.validate_native_owner(lease).expect("actual object revalidated");
    assert_eq!(
        state.created.load(Ordering::Acquire),
        1,
        "issuer may not create an editor-only processor"
    );
    assert_eq!(
        state.entries.load(Ordering::Acquire),
        0,
        "paused inspection must not reset private DSP"
    );
}

#[test]
fn native_owner_object_observation_returns_only_current_facts_without_author_or_dsp_work() {
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    let (verifier, claim) = install_probe_runtime(&mut runtime, &source);
    let lease = runtime.native_owner_lease(owner_addresses(&runtime)[0]).unwrap();
    let mut pending = native_author_probe_source();
    pending.source.revision = 1;
    pending.source.pending_events = 1;
    *state.native_author.lock().unwrap() = Some(pending.clone());
    let before = state.object_observations.load(Ordering::Acquire);
    let binds = state.binds.load(Ordering::Acquire);
    let observed = runtime.observe_native_owner_object(lease).expect("actual retained object");
    assert_eq!(observed.process_id().get(), std::process::id());
    assert_eq!(observed.object_serial().get(), 1);
    assert_eq!(
        state.object_observations.load(Ordering::Acquire),
        before + 1
    );
    assert_eq!(state.binds.load(Ordering::Acquire), binds);
    assert_eq!(state.created.load(Ordering::Acquire), 1);
    assert_eq!(state.entries.load(Ordering::Acquire), 0);
    assert_eq!(state.processes.load(Ordering::Acquire), 0);
    assert_eq!(state.snapshot_calls.load(Ordering::Acquire), 0);
    assert_eq!(*state.native_author.lock().unwrap(), Some(pending.clone()));
    verifier.validate(&claim).expect("observation grants no new installation");
    assert_eq!(runtime.native_owner_lease(lease.address()).unwrap(), lease);

    let foreign_state = Arc::new(OwnerProbeState::default());
    let mut foreign = owner_probe_runtime(&root, &[], &foreign_state);
    let foreign_before = foreign_state.object_observations.load(Ordering::Acquire);
    assert_eq!(
        foreign.observe_native_owner_object(lease),
        Err(AudioNativeOwnerError::UnregisteredRuntime)
    );
    assert_eq!(
        foreign_state.object_observations.load(Ordering::Acquire),
        foreign_before
    );
    let foreign_source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut foreign, &foreign_source);
    let foreign_before = foreign_state.object_observations.load(Ordering::Acquire);
    assert_eq!(
        foreign.observe_native_owner_object(lease),
        Err(AudioNativeOwnerError::OwnerUnavailable)
    );
    assert_eq!(
        foreign_state.object_observations.load(Ordering::Acquire),
        foreign_before
    );

    state.replace_during_validation.store(true, Ordering::Release);
    assert_eq!(
        runtime.observe_native_owner_object(lease),
        Err(AudioNativeOwnerError::OwnerUnavailable)
    );
    assert!(runtime.native_owner_lease(lease.address()).is_err());
    assert_eq!(*state.native_author.lock().unwrap(), Some(pending));
}

#[test]
fn native_owner_observation_rechecks_source_without_a_second_native_query() {
    for observe_object in [false, true] {
        for revoke_before_query in [false, true] {
            let state = Arc::new(OwnerProbeState::default());
            let root = sequence_with_parallel_hosted_delay();
            let mut runtime = owner_probe_runtime(&root, &[], &state);
            let source = mondrian_core::AudioOwnerInstallationSource::new();
            install_probe_runtime(&mut runtime, &source);
            let lease = runtime.native_owner_lease(owner_addresses(&runtime)[0]).unwrap();
            let before = state.object_observations.load(Ordering::Acquire);
            if revoke_before_query {
                source.revoke().unwrap();
            } else {
                *state.observation_revoke_source.lock().unwrap() = Some(source);
            }
            let result = if observe_object {
                runtime.observe_native_owner_object(lease).map(|_| ())
            } else {
                runtime.validate_native_owner(lease)
            };
            assert_eq!(
                result,
                Err(AudioNativeOwnerError::Installation(
                    mondrian_core::AudioOwnerInstallationError::NotInstalled
                ))
            );
            assert_eq!(
                state.object_observations.load(Ordering::Acquire),
                before + usize::from(!revoke_before_query)
            );
            assert!(runtime.native_owner_lease(lease.address()).is_err());
            assert_eq!(state.entries.load(Ordering::Acquire), 0);
            assert_eq!(state.processes.load(Ordering::Acquire), 0);
            assert_eq!(state.snapshot_calls.load(Ordering::Acquire), 0);
        }
    }
}

#[test]
fn native_owner_wrong_source_root_occurrence_and_epoch_cannot_reuse_current_hosted_record() {
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    let (_, claim) = install_probe_runtime(&mut runtime, &source);
    let address = owner_addresses(&runtime)[0];
    let lease = runtime.native_owner_lease(address).expect("current exact lease");
    let other_source = mondrian_core::AudioOwnerInstallationSource::new();
    let (other, foreign) = other_source.register().expect("unrelated source");
    assert_eq!(
        runtime.bind_native_owner_installation(foreign),
        Err(mondrian_core::AudioOwnerInstallationError::WrongRegistration)
    );
    let unrelated = other_source
        .reserve_install(&other, None)
        .and_then(|reservation| other_source.begin_commit(reservation))
        .expect("its actual slot only")
        .commit()
        .expect("foreign claim");
    assert!(matches!(
        runtime.install_native_owner_bindings(&unrelated, &ExecutionCancellationToken::new()),
        Err(AudioNativeOwnerError::Installation(
            mondrian_core::AudioOwnerInstallationError::WrongRegistration
        ))
    ));
    let mut wrong = address;
    wrong.root.session_id = mondrian_core::AudioRenderSessionId::new();
    assert!(runtime.native_owner_lease(wrong).is_err());
    let mut wrong = address;
    wrong.local.prepared_plan_id = mondrian_core::AudioPreparedPlanId::new();
    assert!(runtime.native_owner_lease(wrong).is_err());
    let mut wrong = address;
    wrong.occurrence.instance_id = mondrian_core::AudioProcessorInstanceId::new();
    assert!(runtime.native_owner_lease(wrong).is_err());
    let mut wrong = address;
    wrong.epoch = Some(AudioContinuityEpoch::new(99));
    assert!(runtime.native_owner_lease(wrong).is_err());
    runtime
        .validate_native_owner(lease)
        .expect("failed foreign admissions preserve valid actual record");
    assert_eq!(lease.installed_root_serial(), claim.generation());
}

#[test]
fn native_owner_real_session_entry_and_admitted_object_replacement_revoke_old_lease() {
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    let (_, claim) = install_probe_runtime(&mut runtime, &source);
    let before = runtime
        .native_owner_lease(owner_addresses(&runtime)[0])
        .expect("unentered paused owner");
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(17),
            start_sample: 0,
        })
        .expect("actual sole Session entry");
    assert!(
        runtime.validate_native_owner(before).is_err(),
        "Enter invalidates before actual object lifecycle"
    );
    assert_eq!(state.entries.load(Ordering::Acquire), 1);
    runtime
        .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
        .expect("fresh entered object actual acknowledgement");
    let after = runtime
        .native_owner_lease(owner_addresses(&runtime)[0])
        .expect("exact current epoch");
    assert_ne!(before.binding(), after.binding());
    assert_eq!(after.address().epoch, Some(AudioContinuityEpoch::new(17)));
    state.replace_during_validation.store(true, Ordering::Release);
    assert!(
        runtime.validate_native_owner(after).is_err(),
        "same PID and author ID cannot hide an actual object reload"
    );
    assert!(
        runtime.native_owner_lease(after.address()).is_err(),
        "mismatched object withdraws the current record"
    );
    assert_eq!(state.created.load(Ordering::Acquire), 1);
}

#[test]
fn native_owner_canceled_or_refused_root_preparation_publishes_no_partial_records() {
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    let (_, claim) = install_probe_runtime(&mut runtime, &source);
    let address = owner_addresses(&runtime)[0];
    let cancellation = ExecutionCancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        runtime.prepare_native_owner_bindings(&cancellation).err(),
        Some(AudioNativeOwnerError::Cancelled)
    );
    let retained = runtime
        .native_owner_lease(address)
        .expect("preflight cancellation keeps existing record");
    runtime.validate_native_owner(retained).expect("still valid");
    state.reject_binding.store(true, Ordering::Release);
    assert_eq!(
        runtime
            .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
            .err(),
        Some(AudioNativeOwnerError::BindingRefused(
            NativeAudioObjectBindRefusal::EditorStatePending
        ))
    );
    assert!(runtime.native_owner_lease(address).is_err());
    state.reject_binding.store(false, Ordering::Release);
    runtime
        .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
        .expect("retained same processor recovers");
    let recovered = runtime.native_owner_lease(address).expect("fresh coherent current record");
    runtime.validate_native_owner(recovered).expect("recovered");
    assert_eq!(state.created.load(Ordering::Acquire), 1);
}

#[test]
fn native_owner_duplicate_nested_materializations_keep_distinct_local_binding_records() {
    let state = Arc::new(OwnerProbeState::default());
    let child = sequence_with_parallel_hosted_delay();
    let output = child.audio_program.outputs[0].id;
    let mut root = Sequence::new("two actual native nested materializations");
    root.settings.audio_channel_layout = AudioChannelLayout::Mono;
    let track = root.audio_tracks[0].id;
    for position in [TimelineTime::ZERO, tt(4, 1)] {
        root.add_nested_audio_clip(
            track,
            Clip::new_nested_sequence(child.id, position, tt(4, 1), None)
                .expect("actual nested placement"),
            output,
        )
        .expect("canonical nested component");
    }
    let mut runtime = owner_probe_runtime(&root, &[child], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    let addresses = owner_addresses(&runtime);
    assert_eq!(addresses.len(), 2);
    let a = runtime.native_owner_lease(addresses[0]).expect("first exact nested owner");
    let b = runtime.native_owner_lease(addresses[1]).expect("second exact nested owner");
    assert_eq!(a.address().root, b.address().root);
    assert_ne!(a.address().local, b.address().local);
    assert_eq!(
        a.address().occurrence,
        b.address().occurrence,
        "shared author ID does not collapse actual occurrences"
    );
    assert_ne!(a.binding(), b.binding());
    assert_eq!(state.created.load(Ordering::Acquire), 2);
    runtime.validate_native_owner(a).expect("first actual Hosted record");
    runtime.validate_native_owner(b).expect("second actual Hosted record");
    let object_a = runtime
        .observe_native_owner_object(a)
        .expect("first authenticated nested object");
    let object_b = runtime
        .observe_native_owner_object(b)
        .expect("second authenticated nested object");
    assert_eq!(object_a.process_id().get(), std::process::id());
    assert_eq!(object_b.process_id(), object_a.process_id());
    assert_ne!(object_a.object_serial(), object_b.object_serial());
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(23),
            start_sample: 0,
        })
        .expect("new actual root entry");
    assert_eq!(
        state.entries.load(Ordering::Acquire),
        0,
        "nested native entry remains lazy before actual child demand"
    );
    assert!(
        runtime.validate_native_owner(a).is_err(),
        "root entry immediately revokes old nested authority before its lazy reload"
    );
    assert!(runtime.validate_native_owner(b).is_err());
    source.revoke().expect("actual current renderer retired");
    assert!(runtime.validate_native_owner(a).is_err());
    assert!(runtime.validate_native_owner(b).is_err());
}

#[test]
fn native_editor_runtime_public_address_does_not_mint_permission_and_real_enter_revokes_old_target()
{
    use mondrian_core::{
        NativeAudioEditorControlIntent as Intent, NativeAudioEditorControlOutcome as Outcome,
        NativeAudioEditorControlRefusal as Refusal, NativeAudioEditorTitle,
    };
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    let (_, claim) = install_probe_runtime(&mut runtime, &source);
    let lease = runtime
        .native_owner_lease(owner_addresses(&runtime)[0])
        .expect("actual installed record");
    let target = lease.editor_target();
    let open = |target| Intent::Open {
        target,
        title: NativeAudioEditorTitle::new("portable runtime fixture".into())
            .expect("bounded title"),
    };
    let cancellation = ExecutionCancellationToken::new();
    assert_eq!(
        runtime
            .control_native_editor(open(target), &cancellation)
            .expect("same sole Runtime"),
        Outcome::Opened { target, width: 320, height: 160 }
    );
    assert_eq!(
        state.created.load(Ordering::Acquire),
        1,
        "an editor is not a second processor"
    );
    let mut forged = target;
    forged.binding = mondrian_core::AudioProcessorBindingId::new();
    assert_eq!(
        runtime
            .control_native_editor(open(forged), &cancellation)
            .expect("public facts can be forged but cannot authorize"),
        Outcome::Refused(Refusal::OwnerChanged)
    );
    assert_eq!(
        state.editor_opens.load(Ordering::Acquire),
        1,
        "refusal must precede actual adapter call"
    );
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(13),
            start_sample: 0,
        })
        .expect("real actual owner entry");
    assert!(matches!(
        runtime
            .control_native_editor(open(target), &cancellation)
            .expect("old target after Enter"),
        Outcome::Refused(Refusal::NotInstalled | Refusal::OwnerChanged)
    ));
    assert_eq!(state.editor_opens.load(Ordering::Acquire), 1);
    runtime
        .install_native_owner_bindings(&claim, &cancellation)
        .expect("new actual object acknowledgements");
    let fresh = runtime
        .native_owner_lease(owner_addresses(&runtime)[0])
        .expect("fresh exact epoch")
        .editor_target();
    assert_ne!(target.binding, fresh.binding);
    assert_eq!(
        runtime
            .control_native_editor(open(fresh), &cancellation)
            .expect("fresh same native owner"),
        Outcome::Opened { target: fresh, width: 320, height: 160 }
    );
}

#[test]
fn native_editor_runtime_cleanup_refuses_before_any_root_dsp_and_exact_close_keeps_the_installed_record(
) {
    use mondrian_core::{
        NativeAudioEditorControlIntent as Intent, NativeAudioEditorControlOutcome as Outcome,
    };
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    let address = owner_addresses(&runtime)[0];
    let lease = runtime.native_owner_lease(address).expect("same actual installed processor");
    let target = lease.editor_target();
    state.editor_cleanup.store(true, Ordering::Release);
    assert_eq!(
        runtime.render_into(
            AudioRenderRequest {
                transport_activity: None,
                start_sample: 0,
                frames: 1
            },
            &mut [991.0]
        ),
        Err(AudioExecutionError::EditorCleanupRequired(Box::new(target))),
        "canonical pending frame clear must hold before entering any root DSP window"
    );
    assert_eq!(state.entries.load(Ordering::Acquire), 0);
    assert_eq!(
        runtime
            .control_native_editor(Intent::Close { target }, &ExecutionCancellationToken::new())
            .expect("exact close retry"),
        Outcome::Closed { target }
    );
    assert_eq!(
        runtime.render_into(
            AudioRenderRequest {
                transport_activity: None,
                start_sample: 0,
                frames: 1
            },
            &mut [991.0]
        ),
        Err(AudioExecutionError::StateEntryRequired),
        "cleanup does not fabricate a continuity entry"
    );
    runtime
        .validate_native_owner(lease)
        .expect("cleanup did not replace/remint the only installed object");
    assert_eq!(state.created.load(Ordering::Acquire), 1);
    assert_eq!(state.editor_closes.load(Ordering::Acquire), 1);
}

#[test]
fn native_preflight_snapshot_root_installed_owner_uses_one_snapshot_without_second_pending_query() {
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    state.snapshot_calls.store(0, Ordering::Release);
    state.pending_queries.store(0, Ordering::Release);
    let mut pcm = [991.0];
    assert_eq!(
        runtime.render_into(
            AudioRenderRequest {
                transport_activity: None,
                start_sample: 0,
                frames: 1
            },
            &mut pcm
        ),
        Err(AudioExecutionError::StateEntryRequired)
    );
    assert_eq!(state.snapshot_calls.load(Ordering::Acquire), 1);
    assert_eq!(
        state.pending_queries.load(Ordering::Acquire),
        0,
        "installed record must not add PollPending after Preserve9"
    );
    assert_eq!(
        pcm,
        [0.0],
        "existing normal admitted state-entry refusal initializes PCM"
    );
    assert_eq!(state.entries.load(Ordering::Acquire), 0);
}
#[test]
fn native_preflight_snapshot_root_pending_flag_is_nonconsuming_and_session_sticky_after_empty() {
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    state.pending_restart.store(true, Ordering::Release);
    state.snapshot_calls.store(0, Ordering::Release);
    state.pending_queries.store(0, Ordering::Release);
    let mut pcm = [771.0];
    let request = AudioRenderRequest {
        transport_activity: None,
        start_sample: 0,
        frames: 1,
    };
    let ticket = match runtime.render_into(request, &mut pcm) {
        Err(AudioExecutionError::ReprepareRequired(ticket)) => ticket,
        other => panic!("exact pending refusal: {other:?}"),
    };
    assert!(
        state.pending_restart.load(Ordering::Acquire),
        "snapshot must not retire request flags"
    );
    state.pending_restart.store(false, Ordering::Release);
    assert_eq!(
        runtime.render_into(request, &mut pcm),
        Err(AudioExecutionError::ReprepareRequired(ticket)),
        "empty snapshot cannot install a new graph"
    );
    assert_eq!(
        state.snapshot_calls.load(Ordering::Acquire),
        1,
        "retained Session ticket refuses before new foreign query"
    );
    assert_eq!(state.pending_queries.load(Ordering::Acquire), 0);
    assert_eq!(pcm, [771.0]);
}
#[test]
fn native_preflight_snapshot_root_cleanup_precedes_pending_and_exact_close_keeps_owner() {
    use mondrian_core::{
        NativeAudioEditorControlIntent as Intent, NativeAudioEditorControlOutcome as Outcome,
    };
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    let lease = runtime
        .native_owner_lease(owner_addresses(&runtime)[0])
        .expect("actual installed owner");
    state.editor_cleanup.store(true, Ordering::Release);
    state.pending_restart.store(true, Ordering::Release);
    let mut pcm = [333.0];
    let request = AudioRenderRequest {
        transport_activity: None,
        start_sample: 0,
        frames: 1,
    };
    assert_eq!(
        runtime.render_into(request, &mut pcm),
        Err(AudioExecutionError::EditorCleanupRequired(Box::new(
            lease.editor_target()
        )))
    );
    assert!(
        state.pending_restart.load(Ordering::Acquire),
        "cleanup priority cannot discard pending configuration"
    );
    assert_eq!(
        runtime
            .control_native_editor(
                Intent::Close { target: lease.editor_target() },
                &ExecutionCancellationToken::new()
            )
            .expect("same object close"),
        Outcome::Closed { target: lease.editor_target() }
    );
    assert!(matches!(
        runtime.render_into(request, &mut pcm),
        Err(AudioExecutionError::ReprepareRequired(_))
    ));
    runtime
        .validate_native_owner(lease)
        .expect("same object claim retained after actual cleanup");
    assert_eq!(pcm, [333.0]);
}

fn native_author_probe_source() -> crate::NativeAudioAuthorObservation {
    crate::NativeAudioAuthorObservation {
        source: crate::NativeAudioAuthorSourceStatus {
            parameter_flush_pending: false,
            publishing: false,
            revision: 8,
            pending_events: 0,
            pending_dsp_feedback_values: 0,
            open_parameter_edits: 0,
            group_depth: 0,
            failure: None,

            pending_parameter_observations: 0,
        },
        delivery: crate::NativeAudioAuthorDeliveryState::Preserved,
    }
}

#[test]
fn native_author_closed_source_runtime_real_paused_install_preserves_same_owner_without_enter_or_bind(
) {
    let state = Arc::new(OwnerProbeState::default());
    *state.native_author.lock().expect("actual source") = Some(native_author_probe_source());
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    let lease = runtime
        .native_owner_lease(owner_addresses(&runtime)[0])
        .expect("actual installed paused owner");
    let binds = state.binds.load(Ordering::Acquire);
    runtime
        .validate_native_author_source_closed(&ExecutionCancellationToken::new())
        .expect("actual closed empty source");
    assert_eq!(
        state.snapshot_calls.load(Ordering::Acquire),
        1,
        "one same-object non-consuming snapshot per exact occurrence"
    );
    assert_eq!(
        state.pending_queries.load(Ordering::Acquire),
        0,
        "source admission cannot add a second pending poll"
    );
    assert_eq!(
        state.entries.load(Ordering::Acquire),
        0,
        "idle-before-first-Play inspection performs no DSP reload"
    );
    assert_eq!(state.binds.load(Ordering::Acquire), binds);
    assert_eq!(
        runtime.native_owner_lease(lease.address()).expect("unchanged exact owner"),
        lease
    );
    assert_eq!(
        state.native_author.lock().expect("source").as_ref(),
        Some(&native_author_probe_source()),
        "Preserve does not erase native author work"
    );
}

#[test]
fn native_author_closed_source_runtime_refuses_open_group_callback_and_feedback_without_dirty_history(
) {
    use crate::{NativeAudioAuthorAdmissionError as Error, NativeAudioAuthorHoldReason as Reason};
    for (scenario, reason) in [
        (0, Reason::OpenBoundary),
        (1, Reason::OpenBoundary),
        (2, Reason::PendingCallbacks),
        (3, Reason::DspFeedbackPending),
        (
            4,
            Reason::SourceFailure(crate::NativeAudioAuthorSourceFailure::Overflow),
        ),
    ] {
        let state = Arc::new(OwnerProbeState::default());
        let root = sequence_with_parallel_hosted_delay();
        let mut author = native_author_probe_source();
        match scenario {
            0 => author.source.open_parameter_edits = 2,
            1 => author.source.group_depth = 1,
            2 => author.source.pending_events = 1,
            3 => author.source.pending_dsp_feedback_values = 1,
            _ => author.source.failure = Some(crate::NativeAudioAuthorSourceFailure::Overflow),
        }
        *state.native_author.lock().expect("actual source") = Some(author.clone());
        let mut runtime = owner_probe_runtime(&root, &[], &state);
        let source = mondrian_core::AudioOwnerInstallationSource::new();
        install_probe_runtime(&mut runtime, &source);
        let lease = runtime
            .native_owner_lease(owner_addresses(&runtime)[0])
            .expect("same installed object");
        assert_eq!(
            runtime.validate_native_author_source_closed(&ExecutionCancellationToken::new()),
            Err(Error::Pending { target: Box::new(lease.editor_target()), reason }),
            "actual source facts cannot become closed by an author generation snapshot"
        );
        assert_eq!(
            state.native_author.lock().expect("source").as_ref(),
            Some(&author)
        );
        assert_eq!(state.entries.load(Ordering::Acquire), 0);
        assert_eq!(
            runtime
                .native_owner_lease(lease.address())
                .expect("recoverable source refusal retains same owner"),
            lease
        );
    }
}

#[test]
fn native_author_closed_source_runtime_missing_producer_and_delivered_empty_source_are_not_closed_proofs(
) {
    use crate::{NativeAudioAuthorAdmissionError as Error, NativeAudioAuthorHoldReason as Reason};
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    let lease = runtime
        .native_owner_lease(owner_addresses(&runtime)[0])
        .expect("installed owner");
    assert_eq!(
        runtime.validate_native_author_source_closed(&ExecutionCancellationToken::new()),
        Err(Error::Pending {
            target: Box::new(lease.editor_target()),
            reason: Reason::UnsupportedSource
        }),
        "sourceNone is unavailable, including an adapter with diagnostics support"
    );
    let mut author = native_author_probe_source();
    let object = NativeAudioObjectObservation::new(
        NonZeroU32::new(std::process::id()).expect("actual PID"),
        NonZeroU64::new(state.next_load.load(Ordering::Acquire)).expect("actual created serial"),
    );
    author.delivery = crate::NativeAudioAuthorDeliveryState::AwaitingAcknowledgement {
        binding: NativeAudioObjectBindRequest { binding: lease.binding(), expected_object: object },
        revision: 8,
        request_cohort: None,
    };
    assert!(
        author.source.closed_empty(),
        "positive test control deliberately has an empty source"
    );
    *state.native_author.lock().expect("actual pending record") = Some(author);
    assert_eq!(
        runtime.validate_native_author_source_closed(&ExecutionCancellationToken::new()),
        Err(Error::Pending {
            target: Box::new(lease.editor_target()),
            reason: Reason::OutstandingDelivery
        }),
        "already-drained native group still awaits canonical Session/Plan/ack"
    );
}

#[test]
fn native_author_closed_source_runtime_cancellation_and_revoked_media_claim_refuse_before_foreign_entry(
) {
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    *state.native_author.lock().expect("actual source") = Some(native_author_probe_source());
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    let cancellation = ExecutionCancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        runtime.validate_native_author_source_closed(&cancellation),
        Err(crate::NativeAudioAuthorAdmissionError::Cancelled)
    );
    assert_eq!(
        state.snapshot_calls.load(Ordering::Acquire),
        0,
        "cancelled public entry cannot poll the native object"
    );
    source.revoke().expect("actual slot removal");
    assert!(matches!(
        runtime.validate_native_author_source_closed(&ExecutionCancellationToken::new()),
        Err(crate::NativeAudioAuthorAdmissionError::Owner(_))
    ));
    assert_eq!(
        state.snapshot_calls.load(Ordering::Acquire),
        0,
        "old actual Hosted records cannot revive a revoked renderer claim"
    );
    assert_eq!(state.entries.load(Ordering::Acquire), 0);
}

#[test]
fn native_author_closed_source_runtime_refuses_observations_without_fabricating_author_edits() {
    use crate::{NativeAudioAuthorAdmissionError as Error, NativeAudioAuthorHoldReason as Reason};
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    let mut author = native_author_probe_source();
    author.source.pending_parameter_observations = 1;
    assert_eq!(author.source.pending_events(), 0);
    *state.native_author.lock().expect("original source") = Some(author.clone());
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    let lease = runtime
        .native_owner_lease(owner_addresses(&runtime)[0])
        .expect("actual installed same object");
    assert_eq!(
        runtime.validate_native_author_source_closed(&ExecutionCancellationToken::new()),
        Err(Error::Pending {
            target: Box::new(lease.editor_target()),
            reason: Reason::PendingParameterObservations
        })
    );
    assert_eq!(
        *state.native_author.lock().expect("source retained"),
        Some(author)
    );
    assert_eq!(state.entries.load(Ordering::Acquire), 0);
    assert_eq!(
        runtime.native_owner_lease(lease.address()).expect("same source owner"),
        lease
    );
}

#[test]
fn native_entry_binding_root_actual_pcm_then_author_proposal_uses_entered_epoch() {
    let state = Arc::new(OwnerProbeState::default());
    state.process_author.store(true, Ordering::Release);
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    let (_, claim) = install_probe_runtime(&mut runtime, &source);
    let old = runtime
        .native_owner_lease(owner_addresses(&runtime)[0])
        .expect("initial actual object bound");
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(27),
            start_sample: 1,
        })
        .expect("actual signed-domain entry");
    let mut pcm = [991.0; 2];
    runtime
        .render_into(
            AudioRenderRequest {
                transport_activity: None,
                start_sample: 1,
                frames: 2,
            },
            &mut pcm,
        )
        .expect("real processor receives signal");
    assert_eq!(
        pcm,
        [3.25, 4.875],
        "independent dry Ramp + 0.625 native path proves actual Process"
    );
    assert_eq!(state.processes.load(Ordering::Acquire), 1);
    assert_eq!(
        state
            .native_author
            .lock()
            .expect("actual accepted source")
            .as_ref()
            .expect("Process emitted")
            .source
            .pending_events(),
        1,
        "raw output is accepted even without author Bind"
    );
    let batch = runtime
        .poll_native_observations(&ExecutionCancellationToken::new())
        .expect("same sole Poll")
        .expect("actual native callback batch");
    let proposal = batch.author.expect(
        "ENTER_BIND_PRIMARY: real Process output must reach author after Enter, without GUI",
    );
    assert_eq!(proposal.target().continuity_epoch, Some(27));
    assert_eq!(proposal.target().installed_generation, claim.generation());
    assert_ne!(proposal.target().binding, old.binding());
    assert!(
        runtime.validate_native_owner(old).is_err(),
        "old epoch is never reused"
    );
}

#[test]
fn native_entry_binding_contribution_is_lazy_and_binds_only_actual_first_signal() {
    let state = Arc::new(OwnerProbeState::default());
    let mut root = sequence_with_audio_clip();
    let processor = sequence_with_parallel_hosted_delay()
        .audio_program
        .buses
        .remove(0)
        .strip
        .pre_fader
        .processors
        .remove(0);
    let scope = root.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
    root.audio_program
        .processing_scopes
        .iter_mut()
        .find(|item| item.id == scope)
        .expect("actual Scope")
        .processors
        .processors
        .push(processor);
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(41),
            start_sample: -2,
        })
        .expect("root epoch");
    assert_eq!(
        state.entries.load(Ordering::Acquire),
        0,
        "no eager Contribution signal coordinate"
    );
    let binds = state.binds.load(Ordering::Acquire);
    let mut silence = [99.0; 2];
    runtime
        .render_into(
            AudioRenderRequest {
                transport_activity: None,
                start_sample: -2,
                frames: 2,
            },
            &mut silence,
        )
        .expect("before authored extent");
    assert_eq!(silence, [0.0; 2]);
    assert_eq!(state.entries.load(Ordering::Acquire), 0);
    let mut pcm = [99.0; 2];
    runtime
        .render_into(
            AudioRenderRequest {
                transport_activity: None,
                start_sample: 0,
                frames: 2,
            },
            &mut pcm,
        )
        .expect("actual first local signal");
    assert_eq!(pcm, [1.0, 2.0]);
    assert_eq!(state.entries.load(Ordering::Acquire), 1);
    assert_eq!(
        state.binds.load(Ordering::Acquire),
        binds + 1,
        "ENTER_BIND_LAZY_PRIMARY: rebind after actual lazy Enter"
    );
    let address = owner_addresses(&runtime)[0];
    let lease = runtime.native_owner_lease(address).expect("current Contribution epoch");
    assert_eq!(lease.address().epoch, Some(AudioContinuityEpoch::new(41)));
    runtime
        .render_into(
            AudioRenderRequest {
                transport_activity: None,
                start_sample: 2,
                frames: 2,
            },
            &mut pcm,
        )
        .expect("steady continuation");
    assert_eq!(
        state.binds.load(Ordering::Acquire),
        binds + 1,
        "no Bind IPC on steady PCM windows"
    );
}

#[test]
fn native_entry_binding_nested_actual_local_epochs_survive_forward_and_backward_root_seek() {
    let state = Arc::new(OwnerProbeState::default());
    let child = sequence_with_parallel_hosted_delay();
    let output = child.audio_program.outputs[0].id;
    let mut root = Sequence::new("actual nested native owner");
    root.settings.audio_channel_layout = AudioChannelLayout::Mono;
    root.add_nested_audio_clip(
        root.audio_tracks[0].id,
        Clip::new_nested_sequence(child.id, tt(2, 1), tt(4, 1), None)
            .expect("nested exact placement"),
        output,
    )
    .expect("actual nested component");
    let mut runtime = owner_probe_runtime(&root, &[child], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    let (_, claim) = install_probe_runtime(&mut runtime, &source);
    for (root_epoch, start, local_start, expected) in [
        (51, 4, 0, [2.0, 4.0]),
        (52, 6, 2, [6.0, 8.0]),
        (53, 4, 0, [2.0, 4.0]),
    ] {
        let previous_entries = state.entries.load(Ordering::Acquire);
        runtime
            .enter_state(AudioStateEntry {
                epoch: AudioContinuityEpoch::new(root_epoch),
                start_sample: start,
            })
            .expect("actual root seek");
        assert_eq!(
            state.entries.load(Ordering::Acquire),
            previous_entries,
            "child waits for exact time-map demand"
        );
        let mut pcm = [99.0; 2];
        runtime
            .render_into(
                AudioRenderRequest {
                    transport_activity: None,
                    start_sample: start,
                    frames: 2,
                },
                &mut pcm,
            )
            .expect("actual child demand");
        assert_eq!(pcm, expected);
        assert_eq!(
            state.process_starts.lock().expect("actual coordinates").last(),
            Some(&local_start)
        );
        let address = owner_addresses(&runtime)[0];
        let lease = runtime
            .native_owner_lease(address)
            .expect("ENTER_BIND_NESTED_PRIMARY: lazy child rebind under inherited root Source");
        assert_ne!(address.root, address.local);
        assert_eq!(lease.installed_root_serial(), claim.generation());
        assert_ne!(
            address.epoch,
            Some(AudioContinuityEpoch::new(root_epoch)),
            "child keeps independent epochs"
        );
    }
}

#[test]
fn native_entry_binding_stale_source_refuses_before_real_enter() {
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    source.revoke().expect("actual renderer retired");
    assert!(
        runtime
            .enter_state(AudioStateEntry {
                epoch: AudioContinuityEpoch::new(70),
                start_sample: 0
            })
            .is_err(),
        "ENTER_BIND_SOURCE_PRIMARY: stale actual slot cannot enter under old permission"
    );
    assert_eq!(state.entries.load(Ordering::Acquire), 0);
    assert_eq!(state.processes.load(Ordering::Acquire), 0);
}

#[test]
fn native_entry_binding_first_same_object_retains_cleanup_only_prior_editor_target() {
    use mondrian_core::{
        NativeAudioEditorControlIntent as Intent, NativeAudioEditorControlOutcome as Outcome,
        NativeAudioEditorControlRefusal as Refusal, NativeAudioEditorTitle,
    };
    let state = Arc::new(OwnerProbeState::default());
    state.keep_first_object.store(true, Ordering::Release);
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    let prior = runtime
        .native_owner_lease(owner_addresses(&runtime)[0])
        .expect("initial accepted object")
        .editor_target();
    let token = ExecutionCancellationToken::new();
    assert!(matches!(
        runtime
            .control_native_editor(
                Intent::Open {
                    target: prior,
                    title: NativeAudioEditorTitle::new("initial actual view".into())
                        .expect("title")
                },
                &token
            )
            .expect("same owner open"),
        Outcome::Opened { .. }
    ));
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(81),
            start_sample: -7,
        })
        .expect("real first signal");
    let current = runtime
        .native_owner_lease(owner_addresses(&runtime)[0])
        .expect("ENTER_BIND_VIEW_PRIMARY: initial same object obtains current epoch")
        .editor_target();
    assert_ne!(prior.binding, current.binding);
    assert_ne!(prior.continuity_epoch, current.continuity_epoch);
    assert_eq!(
        runtime
            .control_native_editor(
                Intent::Open {
                    target: prior,
                    title: NativeAudioEditorTitle::new("stale Open".into()).expect("title")
                },
                &token
            )
            .expect("stale Open cannot use cleanup receipt"),
        Outcome::Refused(Refusal::NotInstalled)
    );
    assert_eq!(
        runtime
            .control_native_editor(Intent::Close { target: prior }, &token)
            .expect("only exact same native attachment may close"),
        Outcome::Closed { target: current }
    );
    assert_eq!(state.editor_opens.load(Ordering::Acquire), 1);
    assert_eq!(state.editor_closes.load(Ordering::Acquire), 1);
}

#[test]
fn native_entry_binding_canceled_cold_entry_touches_no_native_source_or_dsp() {
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    state.snapshot_calls.store(0, Ordering::Release);
    state.pending_queries.store(0, Ordering::Release);
    let binds = state.binds.load(Ordering::Acquire);
    let cancellation = ExecutionCancellationToken::new();
    cancellation.cancel();
    assert!(
        matches!(runtime.enter_state_cancellable(AudioStateEntry {epoch:AudioContinuityEpoch::new(90),start_sample:0},&cancellation),Err(AudioExecutionError::NativeOwner(error)) if *error==AudioNativeOwnerError::Cancelled)
    );
    assert_eq!(state.entries.load(Ordering::Acquire), 0);
    assert_eq!(state.processes.load(Ordering::Acquire), 0);
    assert_eq!(state.snapshot_calls.load(Ordering::Acquire), 0);
    assert_eq!(state.pending_queries.load(Ordering::Acquire), 0);
    assert_eq!(state.binds.load(Ordering::Acquire), binds);
}

#[test]
fn native_entry_binding_source_revoked_inside_bind_never_publishes_ready_prefix() {
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    *state.bind_source.lock().expect("actual Source hook") = Some(source);
    assert!(matches!(
        runtime.enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(91),
            start_sample: 0
        }),
        Err(AudioExecutionError::NativeOwner(_))
    ));
    assert_eq!(
        state.entries.load(Ordering::Acquire),
        1,
        "actual entry did happen before Source changed"
    );
    assert!(
        runtime.native_owner_lease(owner_addresses(&runtime)[0]).is_err(),
        "accepted Bind is not published after stale Source"
    );
    assert_eq!(state.processes.load(Ordering::Acquire), 0);
}

#[test]
fn native_entry_binding_cancellation_inside_bind_revokes_all_exact_records() {
    let state = Arc::new(OwnerProbeState::default());
    let mut root = sequence_with_parallel_hosted_delay();
    let mut next = root.audio_program.buses[0].strip.pre_fader.processors[0].clone();
    next.id = mondrian_core::AudioProcessorInstanceId::new();
    root.audio_program.buses[0].strip.pre_fader.processors.push(next);
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    let cancellation = ExecutionCancellationToken::new();
    *state.bind_cancel.lock().expect("real cancel hook") = Some(cancellation.clone());
    assert!(
        matches!(runtime.enter_state_cancellable(AudioStateEntry {epoch:AudioContinuityEpoch::new(92),start_sample:0},&cancellation),Err(AudioExecutionError::NativeOwner(error)) if *error==AudioNativeOwnerError::Cancelled)
    );
    for address in owner_addresses(&runtime) {
        assert!(runtime.native_owner_lease(address).is_err());
    }
    assert_eq!(
        state.entries.load(Ordering::Acquire),
        1,
        "cancel refused before next native Entry"
    );
    assert_eq!(state.processes.load(Ordering::Acquire), 0);
}

#[test]
fn native_entry_binding_standalone_has_actual_dsp_but_no_source_authority_or_bind() {
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(93),
            start_sample: 0,
        })
        .expect("standalone DSP has no UI Source requirement");
    let mut pcm = [99.0; 2];
    runtime
        .render_into(
            AudioRenderRequest {
                transport_activity: None,
                start_sample: 0,
                frames: 2,
            },
            &mut pcm,
        )
        .expect("actual standalone PCM");
    assert_eq!(pcm, [2.0, 4.0]);
    assert_eq!(state.processes.load(Ordering::Acquire), 1);
    assert_eq!(state.binds.load(Ordering::Acquire), 0);
    assert_eq!(
        runtime.native_owner_lease(owner_addresses(&runtime)[0]),
        Err(AudioNativeOwnerError::UnregisteredRuntime)
    );
}

#[test]
fn native_entry_binding_track_and_master_rebind_actual_same_occurrence_before_process() {
    for master in [false, true] {
        let state = Arc::new(OwnerProbeState::default());
        state.process_author.store(true, Ordering::Release);
        let mut root = sequence_with_parallel_hosted_delay();
        let processor = root.audio_program.buses[0].strip.pre_fader.processors.remove(0);
        if master {
            root.audio_program.outputs[0].strip.pre_fader.processors.push(processor);
        } else {
            let track = root.audio_tracks[0].id;
            root.audio_program
                .track_channels
                .get_mut(&track)
                .expect("actual mixer channel")
                .strip
                .pre_fader
                .processors
                .push(processor);
        }
        let mut runtime = owner_probe_runtime(&root, &[], &state);
        let source = mondrian_core::AudioOwnerInstallationSource::new();
        install_probe_runtime(&mut runtime, &source);
        runtime
            .enter_state(AudioStateEntry {
                epoch: AudioContinuityEpoch::new(96),
                start_sample: 0,
            })
            .expect("actual root strip entry");
        let mut pcm = [99.0; 2];
        runtime
            .render_into(
                AudioRenderRequest {
                    transport_activity: None,
                    start_sample: 0,
                    frames: 2,
                },
                &mut pcm,
            )
            .expect("actual processor signal");
        assert_eq!(
            pcm,
            [1.25, 2.5],
            "independent parallel mix with one actual 0.625 strip"
        );
        let proposal = runtime
            .poll_native_observations(&ExecutionCancellationToken::new())
            .expect("same Poll")
            .expect("actual native output")
            .author
            .expect("strip callback is bound");
        assert_eq!(proposal.target().continuity_epoch, Some(96));
        assert_eq!(state.entries.load(Ordering::Acquire), 1);
        assert_eq!(state.processes.load(Ordering::Acquire), 1);
    }
}

#[test]
fn native_entry_binding_outstanding_real_prefix_holds_before_actual_reentry() {
    let state = Arc::new(OwnerProbeState::default());
    state.process_author.store(true, Ordering::Release);
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(94),
            start_sample: 0,
        })
        .expect("first current signal");
    runtime
        .render_into(
            AudioRenderRequest {
                transport_activity: None,
                start_sample: 0,
                frames: 2,
            },
            &mut [99.0; 2],
        )
        .expect("native accepted raw output");
    let proposal = runtime
        .poll_native_observations(&ExecutionCancellationToken::new())
        .expect("sole Poll")
        .expect("actual raw source")
        .author
        .expect("retained prefix");
    let entries = state.entries.load(Ordering::Acquire);
    let binds = state.binds.load(Ordering::Acquire);
    assert!(
        matches!(runtime.enter_state(AudioStateEntry {epoch:AudioContinuityEpoch::new(95),start_sample:2}),Err(AudioExecutionError::NativeAuthorPending(hold)) if hold.reason==crate::NativeAudioAuthorHoldReason::OutstandingDelivery)
    );
    assert_eq!(state.entries.load(Ordering::Acquire), entries);
    assert_eq!(state.binds.load(Ordering::Acquire), binds);
    assert_eq!(proposal.target().continuity_epoch, Some(94));
    assert!(
        runtime.native_owner_lease(owner_addresses(&runtime)[0]).is_ok(),
        "same outstanding source owner is retained"
    );
}

#[test]
fn native_observation_entry_gap_contribution_has_current_ui_binding_without_signal_entry_old_api() {
    let state = Arc::new(OwnerProbeState::default());
    let mut root = sequence_with_audio_clip();
    let processor = sequence_with_parallel_hosted_delay()
        .audio_program
        .buses
        .remove(0)
        .strip
        .pre_fader
        .processors
        .remove(0);
    let scope = root.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
    root.audio_program
        .processing_scopes
        .iter_mut()
        .find(|item| item.id == scope)
        .expect("Scope")
        .processors
        .processors
        .push(processor);
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    let (_, claim) = install_probe_runtime(&mut runtime, &source);
    let prior = runtime
        .native_owner_lease(owner_addresses(&runtime)[0])
        .expect("initial UI owner");
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(201),
            start_sample: -2,
        })
        .expect("root enters before contribution");
    assert_eq!(state.entries.load(Ordering::Acquire), 0);
    assert_eq!(state.processes.load(Ordering::Acquire), 0);
    let lease = runtime.native_owner_lease(owner_addresses(&runtime)[0]).expect(
        "LAZY_UI_BIND_PRIMARY: unchanged local object must remain observable before signal demand",
    );
    assert_ne!(lease.binding(), prior.binding());
    assert_eq!(lease.installed_root_serial(), claim.generation());
    let object = NativeAudioObjectObservation::new(
        NonZeroU32::new(std::process::id()).expect("PID"),
        NonZeroU64::new(state.next_load.load(Ordering::Acquire)).expect("actual load"),
    );
    let mut author = native_author_probe_source();
    author.source.pending_events = 1;
    author.delivery =
        crate::NativeAudioAuthorDeliveryState::Delivered(crate::NativeAudioAuthorDelivery {
            binding: NativeAudioObjectBindRequest {
                binding: lease.binding(),
                expected_object: object,
            },
            revision: 8,
            request_cohort: None,
            events: vec![crate::NativeAudioAuthorEvent::Parameter(
                crate::NativeAudioAuthorParameterEvent::admit(
                    ParameterId::new("clap.param.1").expect("id"),
                    crate::NativeAudioAuthorParameterPhase::Value,
                    Some(
                        crate::NativeAudioAuthorScalar::admit(0.75).expect("native UI plain value"),
                    ),
                )
                .expect("unwrapped UI value"),
            )],
            configuration: None,
            completion: mondrian_core::NativeAudioAuthorCompletion::ParametersOnly,
        });
    *state.native_author.lock().expect("same retained source") = Some(author);
    let batch = runtime
        .poll_native_observations(&ExecutionCancellationToken::new())
        .expect("sole Poll")
        .expect("UI callback");
    let proposal = batch.author.expect("actual gap UI proposal");
    assert!(
        matches!(
            &proposal.callbacks()[0],
            mondrian_core::NativeAudioAuthorCallback::Parameter { .. }
        ),
        "UI origin must not fabricate future Process coordinates"
    );
    let mut pcm = [887.0; 2];
    assert!(matches!(
        runtime.render_into(
            AudioRenderRequest {
                transport_activity: None,
                start_sample: -2,
                frames: 2
            },
            &mut pcm
        ),
        Err(AudioExecutionError::NativeAuthorPending(_))
    ));
    assert_eq!(pcm, [887.0; 2]);
    assert_eq!(state.entries.load(Ordering::Acquire), 0);
}

#[test]
fn native_observation_entry_gap_nested_after_seek_keeps_actual_old_local_epoch_without_process_old_api(
) {
    let state = Arc::new(OwnerProbeState::default());
    let child = sequence_with_parallel_hosted_delay();
    let output = child.audio_program.outputs[0].id;
    let mut root = Sequence::new("pending nested control owner");
    root.settings.audio_channel_layout = AudioChannelLayout::Mono;
    root.add_nested_audio_clip(
        root.audio_tracks[0].id,
        Clip::new_nested_sequence(child.id, tt(2, 1), tt(4, 1), None).expect("placement"),
        output,
    )
    .expect("component");
    let mut runtime = owner_probe_runtime(&root, &[child], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(211),
            start_sample: 4,
        })
        .expect("root first entry");
    let mut pcm = [997.0; 2];
    runtime
        .render_into(
            AudioRenderRequest {
                transport_activity: None,
                start_sample: 4,
                frames: 2,
            },
            &mut pcm,
        )
        .expect("actual old local Process");
    assert_eq!(pcm, [2.0, 4.0]);
    let old = runtime
        .native_owner_lease(owner_addresses(&runtime)[0])
        .expect("old local signal epoch");
    let entries = state.entries.load(Ordering::Acquire);
    let processes = state.processes.load(Ordering::Acquire);
    let object_serial = state.next_load.load(Ordering::Acquire);
    runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(212),
            start_sample: -2,
        })
        .expect("root seek to gap");
    let current = runtime.native_owner_lease(owner_addresses(&runtime)[0]).expect(
        "LAZY_NESTED_UI_PRIMARY: same actual local object requires a current observation binding",
    );
    assert_eq!(
        current.address().epoch,
        old.address().epoch,
        "observation binding keeps actual old local epoch; future position is absent"
    );
    assert_ne!(current.binding(), old.binding());
    assert_eq!(state.entries.load(Ordering::Acquire), entries);
    assert_eq!(state.processes.load(Ordering::Acquire), processes);
    assert_eq!(state.next_load.load(Ordering::Acquire), object_serial);
    let binds = state.binds.load(Ordering::Acquire);
    runtime
        .render_into(
            AudioRenderRequest {
                transport_activity: None,
                start_sample: -2,
                frames: 2,
            },
            &mut pcm,
        )
        .expect("silent gap");
    assert_eq!(pcm, [0.0; 2]);
    assert_eq!(
        state.binds.load(Ordering::Acquire),
        binds,
        "steady silent windows do not rebind the tree"
    );
}

#[test]
fn native_observation_entry_attachment_same_object_target_fact_and_replacement_clear() {
    use mondrian_core::{
        NativeAudioEditorControlIntent as Intent, NativeAudioEditorControlOutcome as Outcome,
        NativeAudioEditorTitle,
    };
    for retain in [true, false] {
        let state = Arc::new(OwnerProbeState::default());
        state.keep_first_object.store(retain, Ordering::Release);
        let root = sequence_with_parallel_hosted_delay();
        let mut runtime = owner_probe_runtime(&root, &[], &state);
        let source = mondrian_core::AudioOwnerInstallationSource::new();
        install_probe_runtime(&mut runtime, &source);
        let token = ExecutionCancellationToken::new();
        let prior = runtime
            .native_owner_lease(owner_addresses(&runtime)[0])
            .expect("real initial binding")
            .editor_target();
        assert!(matches!(
            runtime
                .control_native_editor(
                    Intent::Open {
                        target: prior,
                        title: NativeAudioEditorTitle::new("actual view".into()).expect("title")
                    },
                    &token
                )
                .expect("Open"),
            Outcome::Opened { .. }
        ));
        let serial = state.next_load.load(Ordering::Acquire);
        runtime
            .enter_state(AudioStateEntry {
                epoch: AudioContinuityEpoch::new(221),
                start_sample: -7,
            })
            .expect("actual entry");
        let current = runtime
            .native_owner_lease(owner_addresses(&runtime)[0])
            .expect("real new binding")
            .editor_target();
        let batch = runtime
            .poll_native_observations(&token)
            .expect("sole observation")
            .expect("actual target transition");
        assert!(batch.author.is_none());
        assert!(batch.parameters.is_none());
        let observation = batch.attachment.expect("actual view lifetime fact");
        assert_eq!(
            (observation.previous, observation.current),
            (prior, current)
        );
        assert_eq!(observation.retained, retain);
        assert_eq!(observation.process_id.get(), std::process::id());
        assert_eq!(
            observation.object_serial.get(),
            state.next_load.load(Ordering::Acquire)
        );
        assert_eq!(
            state.editor_opens.load(Ordering::Acquire),
            1,
            "target projection must never reopen a view"
        );
        assert_eq!(state.next_load.load(Ordering::Acquire) == serial, retain);
        let repeated = runtime
            .poll_native_observations(&token)
            .expect("repeated target fact")
            .expect("fact remains until UI can observe");
        assert_eq!(repeated.attachment, Some(observation));
        assert!(
            matches!(
                runtime
                    .control_native_editor(Intent::Close { target: prior }, &token)
                    .expect("same target re-admitted"),
                Outcome::Closed { .. }
            ) == retain,
            "only exact same native object may resolve prior attachment Close"
        );
    }
}

#[test]
fn native_observation_entry_gap_cancellation_during_cold_control_bind_revokes_all_without_enter() {
    let state = Arc::new(OwnerProbeState::default());
    let mut root = sequence_with_audio_clip();
    let processor = sequence_with_parallel_hosted_delay()
        .audio_program
        .buses
        .remove(0)
        .strip
        .pre_fader
        .processors
        .remove(0);
    let scope = root.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
    let rack = &mut root
        .audio_program
        .processing_scopes
        .iter_mut()
        .find(|item| item.id == scope)
        .expect("Scope")
        .processors
        .processors;
    rack.push(processor.clone());
    let mut second = processor;
    second.id = mondrian_core::AudioProcessorInstanceId::new();
    rack.push(second);
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    let token = ExecutionCancellationToken::new();
    *state.bind_cancel.lock().expect("real ack hook") = Some(token.clone());
    assert!(
        matches!(runtime.enter_state_cancellable(AudioStateEntry {epoch:AudioContinuityEpoch::new(222),start_sample:-2},&token),Err(AudioExecutionError::NativeOwner(error)) if *error==AudioNativeOwnerError::Cancelled)
    );
    assert_eq!(state.entries.load(Ordering::Acquire), 0);
    assert_eq!(state.processes.load(Ordering::Acquire), 0);
    for address in owner_addresses(&runtime) {
        assert!(runtime.native_owner_lease(address).is_err());
    }
}

#[test]
fn native_observation_entry_gap_source_revoke_during_ui_bind_does_not_publish_prefix() {
    let state = Arc::new(OwnerProbeState::default());
    let child = sequence_with_parallel_hosted_delay();
    let output = child.audio_program.outputs[0].id;
    let mut root = Sequence::new("nested Source revoked while bind");
    root.settings.audio_channel_layout = AudioChannelLayout::Mono;
    root.add_nested_audio_clip(
        root.audio_tracks[0].id,
        Clip::new_nested_sequence(child.id, tt(2, 1), tt(4, 1), None).expect("placement"),
        output,
    )
    .expect("component");
    let mut runtime = owner_probe_runtime(&root, &[child], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    *state.bind_source.lock().expect("actual Source hook") = Some(source);
    assert!(matches!(
        runtime.enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(223),
            start_sample: -2
        }),
        Err(AudioExecutionError::NativeOwner(_))
    ));
    assert_eq!(state.entries.load(Ordering::Acquire), 0);
    assert_eq!(state.processes.load(Ordering::Acquire), 0);
    for address in owner_addresses(&runtime) {
        assert!(runtime.native_owner_lease(address).is_err());
    }
}

fn submitted_delivery_owner_probe(
    nested: bool,
) -> (
    AudioProgramDeliveryRuntime,
    Arc<OwnerProbeState>,
    mondrian_core::AudioOwnerInstallationSource,
    mondrian_core::NativeAudioEditorTarget,
) {
    let state = Arc::new(OwnerProbeState::default());
    *state.native_author.lock().expect("actual test source") = Some(native_author_probe_source());
    let child = sequence_with_parallel_hosted_delay();
    let mut root = if nested {
        Sequence::new("submitted nested delivery")
    } else {
        child.clone()
    };
    root.settings.audio_channel_layout = AudioChannelLayout::Mono;
    if nested {
        let clip = Clip::new_nested_sequence(child.id, TimelineTime::ZERO, tt(4, 1), None)
            .expect("actual nested placement");
        root.add_nested_audio_clip(
            root.audio_tracks[0].id,
            clip,
            child.audio_program.outputs[0].id,
        )
        .expect("actual nested Program");
    }
    let children = if nested { vec![child] } else { Vec::new() };
    let mut runtime = owner_probe_runtime(&root, &children, &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    let mut delivery =
        AudioProgramDeliveryRuntime::prepare_standard(runtime, AudioChannelLayout::Mono)
            .expect("same owned Runtime delivery");
    delivery
        .enter_state(AudioContinuityEpoch::new(117), 0)
        .expect("actual cold whole-root entry");
    let mut addresses = OwnerAddresses::default();
    delivery
        .visit_continuity_evidence(&mut addresses)
        .expect("actual retained tree");
    let lease = delivery
        .native_owner_lease(addresses.addresses[0])
        .expect("actual installed object");
    (delivery, state, source, lease.editor_target())
}

#[test]
fn delivery_submitted_windows_canceled_before_preflight_keeps_exact_next_window() {
    for nested in [false, true] {
        let (mut delivery, state, _source, _target) = submitted_delivery_owner_probe(nested);
        state.snapshot_calls.store(0, Ordering::Release);
        state.pending_queries.store(0, Ordering::Release);
        let cancellation = ExecutionCancellationToken::new();
        cancellation.cancel();
        let mut pcm = [73.0; 2];
        let result = delivery.render_into_cancellable(
            AudioRenderRequest {
                transport_activity: None,
                start_sample: 9,
                frames: 2,
            },
            &mut pcm,
            &cancellation,
        );
        assert!(
            matches!(result, Err(AudioDeliveryError::ProgramExecution(AudioExecutionError::SourceUnavailable(detail))) if detail.contains("canceled"))
        );
        assert_eq!(state.snapshot_calls.load(Ordering::Acquire), 0);
        assert_eq!(state.pending_queries.load(Ordering::Acquire), 0);
        assert_eq!(state.processes.load(Ordering::Acquire), 0);
        assert_eq!(pcm, [73.0; 2]);
        delivery
            .render_into_cancellable(
                AudioRenderRequest {
                    transport_activity: None,
                    start_sample: 0,
                    frames: 2,
                },
                &mut pcm,
                &ExecutionCancellationToken::new(),
            )
            .expect("pre-canceled admission consumed no submitted window");
        assert_eq!(state.processes.load(Ordering::Acquire), 1);
        assert!(pcm.iter().any(|sample| *sample != 0.0));
    }
}

#[test]
fn delivery_submitted_windows_cleanup_hold_precedes_gap_and_exact_close_resumes() {
    for nested in [false, true] {
        let (mut delivery, state, _source, target) = submitted_delivery_owner_probe(nested);
        let cancellation = ExecutionCancellationToken::new();
        delivery
            .control_native_editor(
                mondrian_core::NativeAudioEditorControlIntent::Open {
                    target,
                    title: mondrian_core::NativeAudioEditorTitle::new("submitted delivery".into())
                        .expect("bounded actual title"),
                },
                &cancellation,
            )
            .expect("same native editor control lane");
        state.editor_cleanup.store(true, Ordering::Release);
        let mut pcm = [73.0; 2];
        assert!(matches!(
            delivery.render_into_cancellable(
                AudioRenderRequest { transport_activity: None, start_sample: 9, frames: 2 },
                &mut pcm,
                &cancellation,
            ),
            Err(AudioDeliveryError::ProgramExecution(AudioExecutionError::EditorCleanupRequired(actual)))
                if *actual == target
        ));
        assert_eq!(state.processes.load(Ordering::Acquire), 0);
        assert_eq!(pcm, [73.0; 2]);
        assert_eq!(
            delivery
                .control_native_editor(
                    mondrian_core::NativeAudioEditorControlIntent::Close { target },
                    &cancellation,
                )
                .expect("same-object cleanup"),
            mondrian_core::NativeAudioEditorControlOutcome::Closed { target },
        );
        delivery
            .render_into_cancellable(
                AudioRenderRequest {
                    transport_activity: None,
                    start_sample: 0,
                    frames: 2,
                },
                &mut pcm,
                &cancellation,
            )
            .expect("typed cleanup hold consumed no submitted window");
        assert_eq!(state.processes.load(Ordering::Acquire), 1);
    }
}

#[test]
fn delivery_submitted_windows_author_hold_precedes_gap_and_preserves_source() {
    for nested in [false, true] {
        let (mut delivery, state, _source, target) = submitted_delivery_owner_probe(nested);
        let mut pending = native_author_probe_source();
        pending.source.pending_events = 1;
        *state.native_author.lock().expect("owned raw observation") = Some(pending.clone());
        let cancellation = ExecutionCancellationToken::new();
        let mut pcm = [73.0; 2];
        assert!(matches!(
            delivery.render_into_cancellable(
                AudioRenderRequest { transport_activity: None, start_sample: 9, frames: 2 },
                &mut pcm,
                &cancellation,
            ),
            Err(AudioDeliveryError::ProgramExecution(AudioExecutionError::NativeAuthorPending(hold)))
                if hold.target == target && hold.reason == crate::NativeAudioAuthorHoldReason::PendingCallbacks
        ));
        assert_eq!(state.processes.load(Ordering::Acquire), 0);
        assert_eq!(pcm, [73.0; 2]);
        assert_eq!(
            *state.native_author.lock().expect("same retained source"),
            Some(pending)
        );
        *state.native_author.lock().expect("source after exact fixture acknowledgement") =
            Some(native_author_probe_source());
        delivery
            .render_into_cancellable(
                AudioRenderRequest {
                    transport_activity: None,
                    start_sample: 0,
                    frames: 2,
                },
                &mut pcm,
                &cancellation,
            )
            .expect("typed native author hold consumed no submitted window");
        assert_eq!(state.processes.load(Ordering::Acquire), 1);
    }
}

// These controls exercise bounded DTO projection through the actual Runtime
// Source/owner/poll seam. The ABI probe does not register 1024 native schemas:
// accepting a proposal here grants no canonical parameter/schema transaction,
// SDK compatibility, native producer compliance or author handoff qualification.
fn native_rescan_probe_values(count: usize) -> Vec<crate::NativeAudioAuthorParameterEvent> {
    let mut values = Vec::new();
    values.try_reserve_exact(count).expect("bounded native rescan source owner");
    for index in 0..count {
        values.push(
            crate::NativeAudioAuthorParameterEvent::admit(
                ParameterId::new(format!("clap.param.{index}")).expect("stable ordered native ID"),
                crate::NativeAudioAuthorParameterPhase::Value,
                Some(
                    crate::NativeAudioAuthorScalar::admit(index as f64 / 1024.0)
                        .expect("independent finite binary scalar"),
                ),
            )
            .expect("legal native rescan Value"),
        );
    }
    values
}

fn publish_native_rescan_probe(
    runtime: &mut AudioProgramRuntime,
    state: &OwnerProbeState,
    events: Vec<crate::NativeAudioAuthorEvent>,
) -> mondrian_core::NativeAudioEditorTarget {
    let lease = runtime
        .native_owner_lease(owner_addresses(runtime)[0])
        .expect("actual installed Runtime Source owner");
    let object = runtime
        .observe_native_owner_object(lease)
        .expect("same authenticated probe object");
    let mut author = native_author_probe_source();
    author.source.pending_events = u32::try_from(events.len()).expect("bounded source prefix");
    author.delivery =
        crate::NativeAudioAuthorDeliveryState::Delivered(crate::NativeAudioAuthorDelivery {
            binding: NativeAudioObjectBindRequest {
                binding: lease.binding(),
                expected_object: object,
            },
            revision: 8,
            request_cohort: None,
            events,
            configuration: None,
            completion: mondrian_core::NativeAudioAuthorCompletion::ParametersOnly,
        });
    *state.native_author.lock().expect("retained original source") = Some(author);
    lease.editor_target()
}

#[test]
fn native_author_rescan_runtime_poll_preserves_all_1024_ordered_values() {
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    let (verifier, claim) = install_probe_runtime(&mut runtime, &source);
    let values = native_rescan_probe_values(1024);
    assert_eq!(values.len(), 1024);
    assert!(
        values.capacity() <= 1024,
        "the fixture source DTO actual capacity is within the existing limit"
    );
    eprintln!(
        "native-rescan source len={} cap={} event_size={} event_align={} tuple_size={} tuple_align={}",
        values.len(), values.capacity(),
        std::mem::size_of::<crate::NativeAudioAuthorParameterEvent>(),
        std::mem::align_of::<crate::NativeAudioAuthorParameterEvent>(),
        std::mem::size_of::<(ParameterId, u64)>(), std::mem::align_of::<(ParameterId, u64)>(),
    );
    let target = publish_native_rescan_probe(
        &mut runtime,
        &state,
        vec![crate::NativeAudioAuthorEvent::ParameterValuesRescanned {
            kind: mondrian_core::NativeAudioParameterRescanKind::Values,
            values,
        }],
    );
    let batch = runtime
        .poll_native_observations(&ExecutionCancellationToken::new())
        .expect("bounded 1024-value DTO must project through the actual Runtime poll")
        .expect("actual installed source observation");
    let proposal = batch.author.expect("complete actual rescan proposal");
    assert_eq!(proposal.target(), target);
    assert_eq!(proposal.sequence(), root.id);
    assert_eq!(proposal.revision(), 8);
    assert_eq!(
        proposal.completion(),
        mondrian_core::NativeAudioAuthorCompletion::ParametersOnly
    );
    assert!(proposal.state().is_none());
    assert_eq!(proposal.callbacks().len(), 1);
    let mondrian_core::NativeAudioAuthorCallback::ParameterValuesRescanned { kind, values } =
        &proposal.callbacks()[0]
    else {
        panic!("actual rescan callback kind must be retained")
    };
    assert_eq!(*kind, mondrian_core::NativeAudioParameterRescanKind::Values);
    assert_eq!(
        values.len(),
        1024,
        "no truncation to satisfy a capacity guard"
    );
    assert!(
        values.capacity() <= 1024,
        "projected actual capacity remains bounded"
    );
    for (index, (parameter, bits)) in values.iter().enumerate() {
        assert_eq!(parameter.as_str(), format!("clap.param.{index}"));
        assert_eq!(*bits, (index as f64 / 1024.0).to_bits());
    }
    verifier.validate(&claim).expect("poll grants no new Source generation");
    assert_eq!(state.entries.load(Ordering::Acquire), 0);
    assert_eq!(state.processes.load(Ordering::Acquire), 0);
}

#[test]
fn native_author_rescan_runtime_poll_preserves_exact_512_plus_512_aggregate() {
    for count in [512, 2048] {
        let state = Arc::new(OwnerProbeState::default());
        let root = sequence_with_parallel_hosted_delay();
        let mut runtime = owner_probe_runtime(&root, &[], &state);
        let source = mondrian_core::AudioOwnerInstallationSource::new();
        let (verifier, claim) = install_probe_runtime(&mut runtime, &source);
        let mut events = Vec::with_capacity(2);
        for kind in [mondrian_core::NativeAudioParameterRescanKind::Values; 2] {
            let values = native_rescan_probe_values(count);
            assert_eq!(values.len(), count);
            assert!(values.capacity() <= mondrian_core::NATIVE_AUDIO_AUTHOR_RESCAN_VALUES);
            events.push(crate::NativeAudioAuthorEvent::ParameterValuesRescanned { kind, values });
        }
        let target = publish_native_rescan_probe(&mut runtime, &state, events);
        let batch = runtime
            .poll_native_observations(&ExecutionCancellationToken::new())
            .expect("the exact combined retained-value boundary must project")
            .expect("actual installed source observation");
        let proposal = batch.author.expect("both complete rescan callbacks retained");
        assert_eq!(proposal.target(), target);
        assert_eq!(proposal.sequence(), root.id);
        assert_eq!(proposal.revision(), 8);
        assert_eq!(
            proposal.completion(),
            mondrian_core::NativeAudioAuthorCompletion::ParametersOnly
        );
        assert!(proposal.state().is_none());
        assert_eq!(proposal.callbacks().len(), 2);
        let mut actual_capacity = 0;
        for callback in proposal.callbacks() {
            let mondrian_core::NativeAudioAuthorCallback::ParameterValuesRescanned { kind, values } =
                callback
            else {
                panic!("each original ordered rescan must be retained")
            };
            assert_eq!(*kind, mondrian_core::NativeAudioParameterRescanKind::Values);
            assert_eq!(values.len(), count);
            actual_capacity += values.capacity();
            for (index, (parameter, bits)) in values.iter().enumerate() {
                assert_eq!(parameter.as_str(), format!("clap.param.{index}"));
                assert_eq!(*bits, (index as f64 / 1024.0).to_bits());
            }
        }
        assert_eq!(
            actual_capacity,
            2 * count,
            "actual aggregate capacity, not only initialized count"
        );
        verifier.validate(&claim).expect("poll grants no new Source generation");
        assert_eq!(state.entries.load(Ordering::Acquire), 0);
        assert_eq!(state.processes.load(Ordering::Acquire), 0);
    }
}

#[test]
fn native_author_rescan_runtime_poll_refuses_aggregate_values_above_existing_extent() {
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    let mut events = Vec::with_capacity(2);
    for count in [2048, 2049] {
        let values = native_rescan_probe_values(count);
        assert_eq!(values.len(), count);
        assert!(values.capacity() <= mondrian_core::NATIVE_AUDIO_AUTHOR_RESCAN_VALUES);
        events.push(crate::NativeAudioAuthorEvent::ParameterValuesRescanned {
            kind: mondrian_core::NativeAudioParameterRescanKind::Values,
            values,
        });
    }
    publish_native_rescan_probe(&mut runtime, &state, events);
    let error = runtime
        .poll_native_observations(&ExecutionCancellationToken::new())
        .expect_err("two individually bounded rescans cannot bypass the aggregate 4096 limit");
    assert!(
        matches!(error, AudioProcessorHostError::InvalidContract(detail)
        if detail == "native author completion extent is invalid")
    );
    assert_eq!(
        state.native_author.lock().unwrap().as_ref().unwrap().source.pending_events(),
        2
    );
    assert_eq!(state.entries.load(Ordering::Acquire), 0);
    assert_eq!(state.processes.load(Ordering::Acquire), 0);
}

#[test]
fn native_author_rescan_runtime_poll_refuses_missing_scalar_without_fabricating_value() {
    let state = Arc::new(OwnerProbeState::default());
    let root = sequence_with_parallel_hosted_delay();
    let mut runtime = owner_probe_runtime(&root, &[], &state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    install_probe_runtime(&mut runtime, &source);
    let mut values = native_rescan_probe_values(1);
    // Deliberately malformed adapter output: public admission would refuse this
    // Value-without-scalar. The Runtime must not substitute zero or omit the event.
    values[0].value = None;
    publish_native_rescan_probe(
        &mut runtime,
        &state,
        vec![crate::NativeAudioAuthorEvent::ParameterValuesRescanned {
            kind: mondrian_core::NativeAudioParameterRescanKind::Values,
            values,
        }],
    );
    assert!(matches!(
        runtime.poll_native_observations(&ExecutionCancellationToken::new()),
        Err(AudioProcessorHostError::NativeParameterSourceUnavailable)
    ));
    assert_eq!(
        state.native_author.lock().unwrap().as_ref().unwrap().source.pending_events(),
        1
    );
    assert_eq!(state.entries.load(Ordering::Acquire), 0);
    assert_eq!(state.processes.load(Ordering::Acquire), 0);
}

// Portable explicit ABI adapter: this exercises Runtime/plan/Hosted admission,
// not a real VST3 binary or GUI. Actual COM query qualification is separate.
fn parameter_query_probe_runtime(
    state: &Arc<OwnerProbeState>,
) -> (AudioProgramRuntime, ParameterId) {
    struct Resolver(Arc<OwnerProbeState>);
    impl AudioProcessorResolver for Resolver {
        fn prepare(
            &self,
            request: AudioProcessorPrepareRequest<'_>,
        ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
            if matches!(request.definition(), mondrian_timeline::AudioProcessorDefinitionRef::Vst3 { class_id, .. }
                if class_id == "00112233445566778899AABBCCDDEEFF")
            {
                return Ok(Arc::new(OwnerProbeFactory {
                    state: self.0.clone(),
                    contract: AudioProcessorExecutionContract::new(
                        0,
                        AudioProcessorTail::None,
                        true,
                        true,
                        true,
                        0,
                    )?,
                }));
            }
            BuiltInAudioProcessorResolver.prepare(request)
        }
    }
    let id = ParameterId::new("vst3.param.0").unwrap();
    let schema =
        mondrian_core::ParameterSchema::v1(id.clone(), mondrian_core::PropertyValue::Double(0.5));
    let mut parameter =
        mondrian_timeline::audio::AudioProcessorParameter::from_schema(schema).unwrap();
    parameter.automation = None;
    let mut processor = AudioProcessorInstance {
        id: mondrian_core::AudioProcessorInstanceId::new(),
        definition: mondrian_timeline::AudioProcessorDefinitionRef::Vst3 {
            class_id: "00112233445566778899AABBCCDDEEFF".into(),
            vendor: None,
            schema_version: 1,
            binary_sha256: Some([3; 32]),
        },
        bypassed: false,
        parameters: Default::default(),
        opaque_state: None,
    };
    processor.parameters.insert(id.clone(), parameter);
    let root = sequence_with_parallel_processor(processor);
    let runtime = AudioProgramRuntime::build_with_processor_resolver(
        &root,
        &[],
        &RampResolver,
        &Resolver(state.clone()),
        owner_probe_contract(),
        None,
    )
    .unwrap();
    (runtime, id)
}

#[test]
fn explicit_parameter_query_requires_current_target_and_admitted_id_without_author_or_dsp_work() {
    use mondrian_core::{
        NativeAudioParameterQuery as Query, NativeAudioParameterQueryOutcome as Out,
        NativeAudioParameterQueryRefusal as Refusal,
    };
    let state = Arc::new(OwnerProbeState::default());
    let (mut runtime, id) = parameter_query_probe_runtime(&state);
    let source = mondrian_core::AudioOwnerInstallationSource::new();
    let (_, claim) = install_probe_runtime(&mut runtime, &source);
    let lease = runtime.native_owner_lease(owner_addresses(&runtime)[0]).unwrap();
    let query = Query {
        target: lease.editor_target(),
        parameter: id.clone(),
    };
    let cancel = ExecutionCancellationToken::new();
    let Out::Observed(actual) = runtime.query_native_parameter(&query, &cancel).unwrap() else {
        panic!("installed exact query");
    };
    assert_eq!(actual.target(), query.target);
    assert_eq!(
        actual.object_serial(),
        NonZeroU64::new(state.next_load.load(Ordering::Acquire)).unwrap()
    );
    assert_eq!(actual.process_id().get(), std::process::id());
    assert_eq!(actual.facts().len(), 1);
    assert_eq!(actual.facts()[0].parameter, id);
    assert_eq!(
        actual.facts()[0].name.as_deref(),
        Some("Actual probe caption")
    );
    assert_eq!(state.parameter_queries.load(Ordering::Acquire), 1);
    let mut wrong = query.clone();
    wrong.parameter = ParameterId::new("vst3.param.4095").unwrap();
    assert_eq!(
        runtime.query_native_parameter(&wrong, &cancel).unwrap(),
        Out::Refused(Refusal::ParameterUnavailable)
    );
    wrong = query.clone();
    wrong.target.binding = mondrian_core::AudioProcessorBindingId::new();
    assert_eq!(
        runtime.query_native_parameter(&wrong, &cancel).unwrap(),
        Out::Refused(Refusal::OwnerChanged)
    );
    assert_eq!(state.parameter_queries.load(Ordering::Acquire), 1);
    assert_eq!(state.created.load(Ordering::Acquire), 1);
    assert_eq!(state.processes.load(Ordering::Acquire), 0);
    assert_eq!(state.editor_opens.load(Ordering::Acquire), 0);
    assert_eq!(runtime.native_owner_lease(lease.address()).unwrap(), lease);
    assert!(
        runtime
            .find_resource_session(runtime.resource_root_identity())
            .unwrap()
            .resource_plan()
            .schedule
            .processors
            .iter()
            .filter(|p| p.occurrence == lease.address().occurrence)
            .flat_map(|p| &p.parameter_curves)
            .all(Option::is_none),
        "query does not fill None or mutate author automation"
    );
    source.revoke().unwrap();
    assert!(matches!(
        runtime.query_native_parameter(&query, &cancel).unwrap(),
        Out::Refused(Refusal::OwnerChanged | Refusal::Unavailable)
    ));
    assert_eq!(state.parameter_queries.load(Ordering::Acquire), 1);
    drop(claim);
}

#[test]
fn explicit_parameter_query_discards_fact_on_actual_source_revocation_or_cancellation_during_query()
{
    use mondrian_core::{
        NativeAudioParameterQuery as Query, NativeAudioParameterQueryOutcome as Out,
        NativeAudioParameterQueryRefusal as Refusal,
    };
    for revoke in [false, true] {
        let state = Arc::new(OwnerProbeState::default());
        let (mut runtime, id) = parameter_query_probe_runtime(&state);
        let source = mondrian_core::AudioOwnerInstallationSource::new();
        install_probe_runtime(&mut runtime, &source);
        let lease = runtime.native_owner_lease(owner_addresses(&runtime)[0]).unwrap();
        let query = Query { target: lease.editor_target(), parameter: id };
        let cancel = ExecutionCancellationToken::new();
        if revoke {
            *state.query_revoke_source.lock().unwrap() = Some(source);
        } else {
            *state.query_cancel.lock().unwrap() = Some(cancel.clone());
        }
        let result = runtime.query_native_parameter(&query, &cancel);
        if revoke {
            assert_eq!(result.unwrap(), Out::Refused(Refusal::OwnerChanged));
        } else {
            assert_eq!(result.unwrap_err(), AudioNativeOwnerError::Cancelled);
        }
        assert_eq!(state.parameter_queries.load(Ordering::Acquire), 1);
        assert_eq!(state.created.load(Ordering::Acquire), 1);
        assert_eq!(state.processes.load(Ordering::Acquire), 0);
    }
}
