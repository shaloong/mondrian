//! Portable actual Runtime/Plan/Hosted contracts, with an explicit test ABI.
//! These tests do not prove native Windows messages, COM or physical GUI.

use super::*;
use mondrian_core::{
    NativeAudioEditorControlIntent as Intent, NativeAudioEditorControlOutcome as Outcome,
    NativeAudioEditorControlRefusal as Refusal,
};
use std::num::{NonZeroU32, NonZeroU64};

#[path = "native_author_handoff_note_controls.rs"]
mod note_controls;

#[path = "native_author_ack_cancellation.rs"]
mod ack_cancellation;

#[derive(Default)]
struct HandoffProbe {
    revision: u64,
    open: u32,
    parameter_only: bool,
    pending: Vec<NativeAudioAuthorEvent>,
    retained: Option<(
        NativeAudioObjectBindRequest,
        u64,
        Option<crate::NativeRequestCohort>,
    )>,
    request_cohort: Option<crate::NativeRequestCohort>,
    // Deliberate corrupt-reply control, not another source identity.
    reported_request_cohort: Option<Option<crate::NativeRequestCohort>>,
    binding: Option<NativeAudioObjectBindRequest>,
    reject_ack: bool,
    cancel_on_accepted_ack: Option<ExecutionCancellationToken>,
    ack_attempts: usize,
    accepted_acks: usize,
    creates: usize,
    binds: usize,
    configuration: Option<Vec<u8>>,
    retained_configuration: Option<Vec<u8>>,
    accepted_seed: Option<Vec<u8>>,
    entries: usize,
    processed: Vec<(i64, f64)>,
    opens: usize,
    closes: usize,
    catchup_pending: Option<u32>,
    catchup_polls: usize,
    source_failure: Option<crate::NativeAudioAuthorSourceFailure>,
    // Independent portable VST ABI controls; the default remains the original
    // CLAP test ABI and has no live request or Ack-side mutation.
    vst_rescan_abi: bool,
    live_request: Option<crate::NativeObservedRequest>,
    live_after_ack: Option<Option<crate::NativeObservedRequest>>,
}
struct HandoffResolver(Arc<Mutex<HandoffProbe>>);
struct HandoffFactory {
    probe: Arc<Mutex<HandoffProbe>>,
    seed: Option<Vec<u8>>,
}
struct HandoffProcessor {
    probe: Arc<Mutex<HandoffProbe>>,
    object_serial: NonZeroU64,
}
impl AudioProcessorResolver for HandoffResolver {
    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        if matches!(request.definition(), mondrian_timeline::AudioProcessorDefinitionRef::Clap { plugin_id, .. }
            if plugin_id == "test.mondrian.handoff")
            || matches!(request.definition(), mondrian_timeline::AudioProcessorDefinitionRef::Vst3 { class_id, .. }
                if class_id == "portable.actual.rescan")
        {
            Ok(Arc::new(HandoffFactory {
                probe: Arc::clone(&self.0),
                seed: None,
            }))
        } else {
            BuiltInAudioProcessorResolver.prepare(request)
        }
    }
}
impl AudioProcessorFactory for HandoffFactory {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        AudioProcessorExecutionContract::new(0, AudioProcessorTail::None, true, true, true, 0)
            .expect("exact zero-latency test ABI")
    }
    fn visit_host_owned_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(
            AudioProcessorFactoryStorageBorrow<'a>,
        ) -> Result<(), AudioProcessorFactoryStorageError>,
    ) -> Result<(), AudioProcessorFactoryStorageError> {
        visitor(AudioProcessorFactoryStorageBorrow::arc_payload(&self.probe))?;
        if let Some(seed) = &self.seed {
            visitor(AudioProcessorFactoryStorageBorrow::complete(seed)?)?;
        }
        Ok(())
    }
    fn with_native_configuration_seed(
        &self,
        format: NativeConfigurationFormat,
        configuration: &[u8],
    ) -> Result<Option<Arc<dyn AudioProcessorFactory>>, AudioProcessorHostError> {
        assert_eq!(
            format,
            if self.probe.lock().unwrap().vst_rescan_abi {
                NativeConfigurationFormat::Vst3ComponentAndController
            } else {
                NativeConfigurationFormat::ClapState
            },
            "explicit portable test ABI, not a native SDK state codec"
        );
        assert_eq!(configuration.len(), 8, "independent scalar state extent");
        Ok(Some(Arc::new(Self {
            probe: Arc::clone(&self.probe),
            seed: Some(configuration.to_vec()),
        })))
    }
    fn create(&self) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        self.probe.lock().expect("fixture owner").creates += 1;
        Ok(Box::new(HandoffProcessor {
            probe: Arc::clone(&self.probe),
            object_serial: NonZeroU64::new(1).expect("nonzero actual test load"),
        }))
    }
}
impl HandoffProcessor {
    fn object(&self) -> NativeAudioObjectObservation {
        NativeAudioObjectObservation::new(
            NonZeroU32::new(std::process::id()).expect("test PID"),
            self.object_serial,
        )
    }
}
impl AudioProcessor for HandoffProcessor {
    fn native_observations_supported(&self) -> bool {
        true
    }
    fn native_audio_object_observation(
        &mut self,
    ) -> Result<Option<NativeAudioObjectObservation>, AudioProcessorHostError> {
        Ok(Some(self.object()))
    }
    fn bind_native_audio_object(
        &mut self,
        request: NativeAudioObjectBindRequest,
    ) -> Result<NativeAudioObjectBindOutcome, AudioProcessorHostError> {
        let receipt = NativeAudioObjectBindReceipt::acknowledge(request, self.object())
            .map_err(|_| AudioProcessorHostError::PoisonedInstance)?;
        let mut probe = self.probe.lock().expect("fixture owner");
        probe.binding = Some(request);
        probe.binds += 1;
        Ok(NativeAudioObjectBindOutcome::Bound(receipt))
    }
    fn poll_native_observations(
        &mut self,
        consumption: NativeAudioObservationConsumption,
    ) -> Result<NativeAudioObservations, AudioProcessorHostError> {
        let object = self.object();
        let mut probe = self.probe.lock().expect("fixture owner");
        let delivery = match consumption {
            NativeAudioObservationConsumption::DrainAuthor(_)
            | NativeAudioObservationConsumption::Drain
                if probe.catchup_pending.is_some() =>
            {
                // A genuine main callback may arrive after the last completed
                // DSP catchup. This test ABI owns its exact source until the
                // next processing operation, without draining any event.
                probe.catchup_polls += 1;
                NativeAudioAuthorDeliveryState::Refused(
                    NativeAudioAuthorRefusal::DspFeedbackPending,
                )
            }
            NativeAudioObservationConsumption::DrainAuthor(binding)
                if probe.binding == Some(binding)
                    && probe.retained.is_none()
                    && !probe.pending.is_empty() =>
            {
                let events = std::mem::take(&mut probe.pending);
                let revision = probe.revision;
                probe.retained = Some((binding, revision, probe.request_cohort));
                let configuration = probe.configuration.take();
                probe.retained_configuration = configuration.clone();
                NativeAudioAuthorDeliveryState::Delivered(NativeAudioAuthorDelivery {
                    binding,
                    revision,
                    request_cohort: probe.request_cohort,
                    events,
                    completion: if configuration.is_some() {
                        mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration
                    } else if probe.parameter_only {
                        mondrian_core::NativeAudioAuthorCompletion::ParametersOnly
                    } else {
                        mondrian_core::NativeAudioAuthorCompletion::Nonterminal
                    },
                    configuration: configuration.map(|bytes| {
                        NativeAudioAuthorConfiguration::admit(bytes).expect("bounded test state")
                    }),
                })
            }
            NativeAudioObservationConsumption::AcknowledgeAuthor(ack) => {
                probe.ack_attempts += 1;
                if !probe.reject_ack
                    && probe.retained.is_some_and(|(binding, revision, _)| {
                        binding == ack.source_binding && revision == ack.revision
                    })
                    && probe.binding == Some(ack.installed_binding)
                    && ack.source_binding.expected_object == object
                {
                    let original = probe.retained.expect("checked retained prefix").2;
                    let reported = probe.reported_request_cohort.unwrap_or(original);
                    if reported == original {
                        probe.retained = None;
                        if let Some(configuration) = probe.retained_configuration.take() {
                            probe.accepted_seed = Some(configuration);
                        }
                        probe.accepted_acks += 1;
                        if probe.vst_rescan_abi {
                            probe.live_request = probe.live_after_ack.take().unwrap_or_else(|| {
                                probe.live_request.filter(|live| {
                                    Some(*live)
                                        != original.map(crate::NativeObservedRequest::Cohort)
                                })
                            });
                        }
                        if let Some(cancellation) = probe.cancel_on_accepted_ack.take() {
                            cancellation.cancel();
                        }
                    }
                    NativeAudioAuthorDeliveryState::Acknowledged {
                        acknowledgement: ack,
                        request_cohort: reported,
                    }
                } else {
                    NativeAudioAuthorDeliveryState::Refused(NativeAudioAuthorRefusal::StaleDelivery)
                }
            }
            _ => probe.retained.map_or(
                NativeAudioAuthorDeliveryState::Preserved,
                |(binding, revision, request_cohort)| {
                    NativeAudioAuthorDeliveryState::AwaitingAcknowledgement {
                        binding,
                        revision,
                        request_cohort: probe.reported_request_cohort.unwrap_or(request_cohort),
                    }
                },
            ),
        };
        let source = NativeAudioAuthorSourceStatus {
            revision: probe.revision,
            pending_events: u32::try_from(probe.pending.len()).expect("bounded fixture"),
            pending_dsp_feedback_values: probe.catchup_pending.unwrap_or(0),
            open_parameter_edits: probe.open,
            group_depth: 0,
            failure: probe.source_failure,

            parameter_flush_pending: false,
            publishing: false,

            pending_parameter_observations: 0,
        };
        Ok(NativeAudioObservations {
            object: Some(object),
            pending_reprepare: probe.live_request,
            author: Some(NativeAudioAuthorObservation { source, delivery }),
            ..Default::default()
        })
    }
    fn enter_state(&mut self, _start: i64) -> Result<(), AudioProcessorHostError> {
        self.probe.lock().expect("fixture owner").entries += 1;
        self.object_serial =
            NonZeroU64::new(self.object_serial.get() + 1).expect("bounded fixture load");
        Ok(())
    }
    fn process(
        &mut self,
        context: AudioProcessorProcessContext,
        audio: &mut dyn AudioProcessorAudioIo,
        parameters: AudioParameterEventBatch<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        let value = parameters
            .events(0)
            .and_then(|events| events.first())
            .expect("canonical prepared parameter lane")
            .value;
        self.probe
            .lock()
            .expect("fixture owner")
            .processed
            .push((context.request().start_sample, value));
        for sample in audio.main_interleaved() {
            *sample *= value as f32;
        }
        Ok(())
    }
    fn open_native_audio_editor(
        &mut self,
        request: NativeAudioEditorOpenRequest,
    ) -> Result<NativeAudioEditorOutcome, AudioProcessorHostError> {
        let mut probe = self.probe.lock().expect("fixture owner");
        if probe.binding != Some(request.binding()) {
            return Ok(NativeAudioEditorOutcome::Refused(
                NativeAudioEditorRefusal::OwnerChanged,
            ));
        }
        probe.opens += 1;
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
        let mut probe = self.probe.lock().expect("fixture owner");
        if probe.binding != Some(binding) {
            return Ok(NativeAudioEditorOutcome::Refused(
                NativeAudioEditorRefusal::OwnerChanged,
            ));
        }
        probe.closes += 1;
        Ok(NativeAudioEditorOutcome::Closed { binding })
    }
}

struct HandoffFixture {
    sequence: Sequence,
    runtime: AudioProgramRuntime,
    source: mondrian_core::AudioOwnerInstallationSource,
    probe: Arc<Mutex<HandoffProbe>>,
    target: mondrian_core::NativeAudioEditorTarget,
    attachment: mondrian_core::NativeAudioEditorTarget,
}
impl HandoffFixture {
    fn new() -> Self {
        Self::with_definition(mondrian_timeline::AudioProcessorDefinitionRef::Clap {
            plugin_id: "test.mondrian.handoff".into(),
            schema_version: 1,
            binary_sha256: None,
        })
    }
    fn with_definition(definition: mondrian_timeline::AudioProcessorDefinitionRef) -> Self {
        use mondrian_core::automation::{ParameterSchema, PropertyValue};
        let probe = Arc::new(Mutex::new(HandoffProbe {
            vst_rescan_abi: matches!(
                definition,
                mondrian_timeline::AudioProcessorDefinitionRef::Vst3 { .. }
            ),
            ..Default::default()
        }));
        let mut sequence = sequence_with_audio_clip();
        let parameter = ParameterId::new_static("native.gain");
        let mut processor = AudioProcessorInstance {
            id: mondrian_core::AudioProcessorInstanceId::new(),
            definition,
            bypassed: false,
            parameters: Default::default(),
            opaque_state: None,
        };
        processor.parameters.insert(
            parameter.clone(),
            mondrian_timeline::audio::AudioProcessorParameter::from_schema(ParameterSchema::v1(
                parameter,
                PropertyValue::Double(0.5),
            ))
            .expect("numeric author schema"),
        );
        let processor_id = processor.id;
        let output = sequence.audio_program.outputs[0].id;
        sequence.audio_program.outputs[0].strip.pre_fader.processors.push(processor);
        let contract = AudioRenderContract {
            sample_rate: 2,
            channel_layout: AudioChannelLayout::Mono,
            max_block_frames: 1,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes:
                AudioRenderContract::DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES,
            public_output_lookahead_budget_frames:
                AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
            compensation_delay_scratch_budget_bytes:
                AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
        };
        let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
            &sequence,
            &[],
            &RampResolver,
            &HandoffResolver(Arc::clone(&probe)),
            contract,
            None,
            AudioRuntimeResourceGrant::new(1, 64 * 1024 * 1024, 64 * 1024 * 1024),
        )
        .expect("actual prepared frozen-grant Runtime");
        let epoch = AudioContinuityEpoch::new(47);
        runtime
            .enter_state(AudioStateEntry { epoch, start_sample: 0 })
            .expect("one real Session entry");
        let source = mondrian_core::AudioOwnerInstallationSource::new();
        let (registration, verifier) = source.register().expect("actual Source registration");
        runtime
            .bind_native_owner_installation(verifier)
            .expect("actual Runtime registration");
        let claim = source
            .begin_commit(source.reserve_install(&registration, None).expect("slot reservation"))
            .expect("real installation boundary")
            .commit()
            .expect("initial slot commit");
        runtime
            .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
            .expect("actual child ack");
        let identity = runtime.resource_root_identity();
        let lease = runtime
            .native_owner_lease(AudioNativeProcessorAddress {
                root: identity,
                local: identity,
                epoch: Some(epoch),
                occurrence: AudioProcessorOccurrence {
                    instance_id: processor_id,
                    owner: AudioProcessorOccurrenceOwner::Output(output),
                    insertion: AudioProcessorInsertionPoint::PreFader,
                },
            })
            .expect("actual issued installed lease");
        let target = lease.editor_target();
        assert!(matches!(
            runtime
                .control_native_editor(
                    Intent::Open {
                        target,
                        title: mondrian_core::NativeAudioEditorTitle::new(
                            "portable handoff".into()
                        )
                        .expect("title")
                    },
                    &ExecutionCancellationToken::new()
                )
                .expect("same actual view open"),
            Outcome::Opened { .. }
        ));
        Self {
            sequence,
            runtime,
            source,
            probe,
            target,
            attachment: target,
        }
    }
    fn queue_value(&mut self, value: f64) {
        let mut probe = self.probe.lock().expect("fixture owner");
        if probe.open == 0 {
            probe.open = 1;
            probe.revision += 1;
            probe.pending.push(NativeAudioAuthorEvent::Parameter(
                NativeAudioAuthorParameterEvent::admit(
                    ParameterId::new_static("native.gain"),
                    NativeAudioAuthorParameterPhase::Begin,
                    None,
                )
                .expect("actual Begin"),
            ));
        }
        probe.revision += 1;
        probe.pending.push(NativeAudioAuthorEvent::Parameter(
            NativeAudioAuthorParameterEvent::admit(
                ParameterId::new_static("native.gain"),
                NativeAudioAuthorParameterPhase::Value,
                Some(NativeAudioAuthorScalar::admit(value).expect("finite actual Value")),
            )
            .expect("actual Value"),
        ));
        drop(probe);
    }
    fn drain(&mut self) -> mondrian_core::NativeAudioAuthorProposal {
        self.runtime
            .poll_native_observations(&ExecutionCancellationToken::new())
            .expect("sole actual source poll")
            .expect("owned actual completion")
            .author
            .expect("retained exact proposal")
    }
    fn value(&mut self, value: f64) -> mondrian_core::NativeAudioAuthorProposal {
        self.queue_value(value);
        self.drain()
    }
    fn install(&mut self, proposal: &mondrian_core::NativeAudioAuthorProposal, value: f64) {
        if let Some(state) = proposal.state() {
            self.sequence.audio_program.outputs[0].strip.pre_fader.processors[0].opaque_state =
                Some(state.clone());
        }
        self.sequence.audio_program.outputs[0].strip.pre_fader.processors[0]
            .set_parameter_automation(
                ExactAutomationCurve::new(ParameterId::new_static("native.gain"), value)
                    .expect("exact unkeyed canonical curve"),
            )
            .expect("schema-validated author curve");
        let programs = [NativeAudioAuthorProgram {
            sequence: self.sequence.id,
            program: Arc::new(
                compile_audio_program(
                    &self.sequence,
                    AudioCompileRequest::program(self.sequence.audio_program.outputs[0].id),
                )
                .expect("sole canonical compiler"),
            ),
        }];
        let (registration, destination) = self.source.register().expect("next actual registration");
        let expected = self.source.current_claim().expect("current slot").expect("installed slot");
        let reservation = self
            .source
            .reserve_install(&registration, Some(&expected))
            .expect("same source reservation");
        let mut authorization = self
            .source
            .authorize_runtime_installation(reservation)
            .expect("sealed exact authorization");
        let mut prepared = self
            .runtime
            .prepare_native_author_parameter_handoff(
                &programs,
                proposal,
                destination,
                &ExecutionCancellationToken::new(),
            )
            .expect("complete native/owner preparation");
        let receipt = self
            .runtime
            .install_native_author_parameter_handoff(
                &mut prepared,
                &mut authorization,
                &ExecutionCancellationToken::new(),
            )
            .expect("bounded real Plan/binding swap");
        assert_ne!(self.target.binding, receipt.target.binding);
        assert_eq!(self.target.local_session, receipt.target.local_session);
        assert_eq!(
            self.target.continuity_epoch,
            receipt.target.continuity_epoch
        );
        self.target = receipt.target;
        drop(prepared);
    }
    fn ack(&mut self, proposal: &mondrian_core::NativeAudioAuthorProposal) {
        let receipt = self
            .runtime
            .acknowledge_native_author_parameter_handoff(
                proposal,
                &ExecutionCancellationToken::new(),
            )
            .expect("exact native source/current installed Ack");
        assert_eq!(receipt.target, self.target);
    }
    fn block(&mut self, start: i64, value: f64) {
        let mut pcm = [91.0];
        self.runtime
            .render_into(
                AudioRenderRequest { start_sample: start, frames: 1 },
                &mut pcm,
            )
            .expect("same Session exact continued sample");
        assert_eq!(pcm, [(start as f32 + 1.0) * value as f32]);
        assert_eq!(
            self.probe.lock().expect("fixture owner").processed.last(),
            Some(&(start, value))
        );
        assert_eq!(
            self.probe.lock().expect("fixture owner").entries,
            1,
            "handoff may not Enter/reset DSP"
        );
    }
}

#[test]
fn native_author_handoff_nonterminal_values_ack_and_continue_same_session_without_entry() {
    let mut fixture = HandoffFixture::new();
    fixture.block(0, 0.5);
    for (start, value) in [(1, 0.25), (2, 0.75), (3, 1.0)] {
        let proposal = fixture.value(value);
        let mut destination = [91.0];
        assert!(matches!(
            fixture.runtime.render_into(
                AudioRenderRequest { start_sample: start, frames: 1 },
                &mut destination
            ),
            Err(AudioExecutionError::NativeAuthorPending(_))
        ));
        assert_eq!(
            destination,
            [91.0],
            "unaccepted source cannot reach destination or old constants"
        );
        fixture.install(&proposal, value);
        fixture.ack(&proposal);
        fixture.block(start, value);
        assert_eq!(
            fixture.probe.lock().expect("fixture owner").open,
            1,
            "audition must continue while the actual gesture remains open"
        );
    }
}

#[test]
fn native_author_handoff_ack_retry_does_not_reinstall_plan_or_erase_newer_suffix() {
    let mut fixture = HandoffFixture::new();
    fixture.block(0, 0.5);
    let proposal = fixture.value(0.75);
    fixture.install(&proposal, 0.75);
    let target = fixture.target;
    let owner_counts = {
        let probe = fixture.probe.lock().expect("same installed owner");
        (
            probe.creates,
            probe.binds,
            probe.entries,
            probe.processed.len(),
        )
    };
    fixture.probe.lock().expect("fixture owner").reject_ack = true;
    let rejected = fixture
        .runtime
        .acknowledge_native_author_parameter_handoff(&proposal, &ExecutionCancellationToken::new());
    assert!(rejected.is_err());
    assert!(matches!(
        rejected,
        Err(NativeAudioParameterHandoffError::Admission(
            NativeAudioAuthorAdmissionError::Pending {
                reason: NativeAudioAuthorHoldReason::OutstandingDelivery,
                ..
            }
        ))
    ));
    {
        let probe = fixture.probe.lock().expect("rejected actual Ack");
        assert_eq!(probe.ack_attempts, 1);
        assert_eq!(probe.accepted_acks, 0);
        assert!(probe.retained.is_some());
        assert_eq!(
            (
                probe.creates,
                probe.binds,
                probe.entries,
                probe.processed.len()
            ),
            owner_counts
        );
    }
    fixture.queue_value(0.25);
    let mut destination = [91.0];
    assert!(matches!(
        fixture.runtime.render_into(
            AudioRenderRequest { start_sample: 1, frames: 1 },
            &mut destination
        ),
        Err(AudioExecutionError::NativeAuthorPending(_))
    ));
    assert_eq!(destination, [91.0]);
    fixture.probe.lock().expect("fixture owner").reject_ack = false;
    fixture.ack(&proposal);
    {
        let probe = fixture.probe.lock().expect("accepted original Ack retry");
        assert_eq!(probe.ack_attempts, 2);
        assert_eq!(probe.accepted_acks, 1);
        assert_eq!(probe.retained, None);
        assert_eq!(
            (
                probe.creates,
                probe.binds,
                probe.entries,
                probe.processed.len()
            ),
            owner_counts
        );
    }
    assert_eq!(
        fixture.target, target,
        "retry only Ack, no second Source/Plan installation"
    );
    assert_eq!(
        fixture.probe.lock().expect("fixture owner").pending.len(),
        1,
        "prefix Ack retry must leave the newer callback owned by its actual source"
    );
    assert!(matches!(
        fixture.runtime.render_into(
            AudioRenderRequest { start_sample: 1, frames: 1 },
            &mut destination
        ),
        Err(AudioExecutionError::NativeAuthorPending(_))
    ));
    assert_eq!(
        destination,
        [91.0],
        "newer unaccepted source still prevents old-constant processing"
    );
    let suffix = fixture.drain();
    assert!(suffix.revision() > proposal.revision());
    fixture.install(&suffix, 0.25);
    fixture.ack(&suffix);
    fixture.block(1, 0.25);
}

#[test]
fn native_author_handoff_original_attachment_closes_after_arbitrary_parameter_rebindings() {
    let mut fixture = HandoffFixture::new();
    for index in 0..32 {
        let value = f64::from(index) / 32.0;
        let proposal = fixture.value(value);
        fixture.install(&proposal, value);
        fixture.ack(&proposal);
    }
    assert!(
        matches!(fixture.runtime.control_native_editor(Intent::Close { target: fixture.attachment },
        &ExecutionCancellationToken::new()).expect("original actual attachment cleanup"),
        Outcome::Closed { target } if target == fixture.target)
    );
    assert_eq!(fixture.probe.lock().expect("fixture owner").closes, 1);
    assert!(matches!(
        fixture
            .runtime
            .control_native_editor(
                Intent::Open {
                    target: fixture.target,
                    title: mondrian_core::NativeAudioEditorTitle::new(
                        "reopened same object".into()
                    )
                    .expect("title")
                },
                &ExecutionCancellationToken::new()
            )
            .expect("current exact reopening"),
        Outcome::Opened { .. }
    ));
    assert_eq!(fixture.probe.lock().expect("fixture owner").opens, 2);
}

#[test]
fn native_author_handoff_reload_revokes_old_attachment_cleanup_permission() {
    let mut fixture = HandoffFixture::new();
    fixture
        .runtime
        .enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(48),
            start_sample: 0,
        })
        .expect("actual fresh entry");
    assert!(matches!(
        fixture
            .runtime
            .control_native_editor(
                Intent::Close { target: fixture.attachment },
                &ExecutionCancellationToken::new()
            )
            .expect("stale actual attachment"),
        Outcome::Refused(Refusal::NotInstalled | Refusal::OwnerChanged)
    ));
    assert_eq!(fixture.probe.lock().expect("fixture owner").closes, 0);
}

#[test]
fn native_author_handoff_ack_preserves_newer_callback_suffix_until_its_own_plan() {
    let mut fixture = HandoffFixture::new();
    fixture.block(0, 0.5);
    let prefix = fixture.value(0.25);
    fixture.queue_value(0.75);
    fixture.install(&prefix, 0.25);
    fixture.ack(&prefix);
    assert_eq!(
        fixture.probe.lock().expect("fixture owner").pending.len(),
        1,
        "Ack must not erase a callback appended after the exact retained revision"
    );
    let mut destination = [91.0];
    assert!(matches!(
        fixture.runtime.render_into(
            AudioRenderRequest { start_sample: 1, frames: 1 },
            &mut destination
        ),
        Err(AudioExecutionError::NativeAuthorPending(_))
    ));
    assert_eq!(
        destination,
        [91.0],
        "old accepted constants cannot overwrite an unaccepted suffix"
    );
    let suffix = fixture.drain();
    assert!(suffix.revision() > prefix.revision());
    assert_eq!(suffix.callbacks().len(), 1);
    fixture.install(&suffix, 0.75);
    fixture.ack(&suffix);
    fixture.block(1, 0.75);
}

impl HandoffFixture {
    fn terminal(&mut self) -> mondrian_core::NativeAudioAuthorProposal {
        let mut probe = self.probe.lock().expect("fixture owner");
        assert_eq!(probe.open, 1, "an actual Begin must still be open");
        probe.open = 0;
        probe.revision += 1;
        probe.pending.push(NativeAudioAuthorEvent::Parameter(
            NativeAudioAuthorParameterEvent::admit(
                ParameterId::new_static("native.gain"),
                NativeAudioAuthorParameterPhase::End,
                None,
            )
            .expect("actual End"),
        ));
        // Explicit test ABI's actual control-side state; no runtime permission.
        let value = probe.processed.last().expect("positive processed control").1;
        probe.configuration = Some(value.to_le_bytes().to_vec());
        drop(probe);
        let proposal = self.drain();
        assert!(proposal.terminal());
        assert_eq!(proposal.open_parameter_edits(), 0);
        assert_eq!(proposal.group_depth(), 0);
        proposal
    }
}

#[test]
fn native_author_terminal_single_strip_installs_coherent_seed_and_continues_without_entry() {
    let mut fixture = HandoffFixture::new();
    fixture.block(0, 0.5);
    let value = fixture.value(0.75);
    fixture.install(&value, 0.75);
    fixture.ack(&value);
    fixture.block(1, 0.75);
    let terminal = fixture.terminal();
    let before = fixture.runtime.resource_root_identity();
    let count = fixture.probe.lock().expect("fixture owner").processed.len();
    let mut sentinel = [91.0];
    assert!(matches!(
        fixture.runtime.render_into(
            AudioRenderRequest { start_sample: 2, frames: 1 },
            &mut sentinel,
        ),
        Err(AudioExecutionError::NativeAuthorPending(_))
    ));
    assert_eq!(sentinel, [91.0]);
    assert_eq!(
        fixture.probe.lock().expect("fixture owner").processed.len(),
        count
    );
    fixture.install(&terminal, 0.75);
    fixture.ack(&terminal);
    assert_eq!(fixture.target.local_session, before.session_id);
    assert_eq!(
        fixture.probe.lock().expect("fixture owner").accepted_seed.as_deref(),
        Some(0.75_f64.to_le_bytes().as_slice())
    );
    fixture
        .runtime
        .validate_native_author_source_closed(&ExecutionCancellationToken::new())
        .expect("terminal exact Ack releases the canonical source barrier");
    fixture.block(2, 0.75);
    assert!(matches!(
        fixture
            .runtime
            .control_native_editor(
                Intent::Close { target: fixture.attachment },
                &ExecutionCancellationToken::new(),
            )
            .expect("same retained real attachment"),
        Outcome::Closed { .. }
    ));
    assert_eq!(fixture.probe.lock().expect("fixture owner").closes, 1);
}

#[test]
fn native_author_terminal_single_strip_ack_retry_preserves_a_new_open_suffix() {
    let mut fixture = HandoffFixture::new();
    fixture.block(0, 0.5);
    let value = fixture.value(0.75);
    fixture.install(&value, 0.75);
    fixture.ack(&value);
    fixture.block(1, 0.75);
    let terminal = fixture.terminal();
    // A later real Begin/Value cannot block acceptance of the already captured
    // exact closed prefix or be erased by its Ack. It owns its own next batch.
    fixture.queue_value(0.25);
    fixture.install(&terminal, 0.75);
    let installed = fixture.target;
    fixture.probe.lock().expect("fixture owner").reject_ack = true;
    assert!(fixture
        .runtime
        .acknowledge_native_author_parameter_handoff(&terminal, &ExecutionCancellationToken::new(),)
        .is_err());
    fixture.queue_value(0.5);
    let mut sentinel = [91.0];
    assert!(matches!(
        fixture.runtime.render_into(
            AudioRenderRequest { start_sample: 2, frames: 1 },
            &mut sentinel,
        ),
        Err(AudioExecutionError::NativeAuthorPending(_))
    ));
    assert_eq!(sentinel, [91.0]);
    fixture.probe.lock().expect("fixture owner").reject_ack = false;
    fixture.ack(&terminal);
    assert_eq!(
        fixture.target, installed,
        "retry Ack must not publish another Plan/claim"
    );
    assert_eq!(
        fixture.probe.lock().expect("fixture owner").pending.len(),
        3
    );
    assert_eq!(fixture.probe.lock().expect("fixture owner").open, 1);
    assert!(matches!(
        fixture.runtime.render_into(
            AudioRenderRequest { start_sample: 2, frames: 1 },
            &mut sentinel,
        ),
        Err(AudioExecutionError::NativeAuthorPending(_))
    ));
    let suffix = fixture.drain();
    assert!(!suffix.terminal());
    assert!(suffix.revision() > terminal.revision());
    assert_eq!(suffix.callbacks().len(), 3);
    fixture.install(&suffix, 0.5);
    fixture.ack(&suffix);
    fixture.block(2, 0.5);
    assert_eq!(
        fixture.probe.lock().expect("fixture owner").accepted_seed.as_deref(),
        Some(0.75_f64.to_le_bytes().as_slice()),
        "nonterminal suffix must not wipe the accepted future seed"
    );
}

#[cfg(target_os = "windows")]
mod scope_native {
    use super::*;
    use crate::processor_isolation::{
        DiscoveredVst3AudioProcessorSpecResolver, IsolatedAudioProcessorResolver,
    };
    use std::ffi::c_void;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    #[link(name = "user32")]
    unsafe extern "system" {
        #[link_name = "FindWindowW"]
        fn scope_find_window_w(class: *const u16, title: *const u16) -> *mut c_void;
        #[link_name = "FindWindowExW"]
        fn scope_find_window_ex_w(
            parent: *mut c_void,
            after: *mut c_void,
            class: *const u16,
            title: *const u16,
        ) -> *mut c_void;
        #[link_name = "GetWindowThreadProcessId"]
        fn scope_window_process_id(window: *mut c_void, process: *mut u32) -> u32;
        #[link_name = "IsWindow"]
        fn scope_is_window(window: *mut c_void) -> i32;
        #[link_name = "PostMessageW"]
        fn scope_post_message_w(window: *mut c_void, message: u32, word: usize, long: isize)
            -> i32;
    }
    fn scope_wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }
    fn scope_initial_fixture_state() -> Vec<u8> {
        // Independent owned fixture envelope: component and controller each
        // contain MEV1/version1/gain=.5/balance=.5/mode0/nonce0, exactly32 bytes.
        let mut part = Vec::with_capacity(32);
        part.extend_from_slice(b"MEV1");
        part.extend_from_slice(&1_u32.to_le_bytes());
        part.extend_from_slice(&0.5_f64.to_le_bytes());
        part.extend_from_slice(&0.5_f64.to_le_bytes());
        part.extend_from_slice(&0_u32.to_le_bytes());
        part.extend_from_slice(&0_u32.to_le_bytes());
        let mut bytes = Vec::with_capacity(92);
        bytes.extend_from_slice(b"VST3HOST_STATE\0\0");
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&32_u32.to_le_bytes());
        bytes.extend_from_slice(&32_u32.to_le_bytes());
        bytes.extend_from_slice(&part);
        bytes.extend_from_slice(&part);
        assert_eq!(bytes.len(), 92);
        bytes
    }
    struct ScopeUnityStereoSource;
    impl AudioDecodedSource for ScopeUnityStereoSource {
        fn read_interleaved(
            &self,
            _start: i64,
            frames: usize,
            pcm: &mut [f32],
            _cancellation: &ExecutionCancellationToken,
        ) -> Result<(), String> {
            if frames.checked_mul(2) != Some(pcm.len()) {
                return Err("exact stereo source extent changed".into());
            }
            pcm.fill(1.0);
            Ok(())
        }
    }
    struct ScopeUnityStereoResolver;
    impl AudioMediaResolver for ScopeUnityStereoResolver {
        fn resolve(
            &self,
            _asset: AssetId,
            _component: AudioSourceComponentId,
            _rate: u32,
        ) -> Result<ResolvedAudioSource, String> {
            Ok(ResolvedAudioSource::new(
                AudioChannelLayout::Stereo,
                Arc::new(ScopeUnityStereoSource),
            ))
        }
    }
    struct ScopeActualCursorVisitor {
        instance: mondrian_core::AudioProcessorInstanceId,
        processors: Vec<AudioProcessorCursorEvidence>,
    }
    impl AudioContinuityEvidenceVisitor for ScopeActualCursorVisitor {
        fn session(
            &mut self,
            _evidence: AudioSessionCursorEvidence,
        ) -> Result<(), AudioExecutionError> {
            Ok(())
        }
        fn processor(
            &mut self,
            evidence: AudioProcessorCursorEvidence,
        ) -> Result<(), AudioExecutionError> {
            if evidence.occurrence.instance_id == self.instance {
                assert!(
                    self.processors.len() < 2,
                    "bounded exact two native peer fixture"
                );
                self.processors.push(evidence);
            }
            Ok(())
        }
    }
    fn scope_real_parent(title: &str) -> *mut c_void {
        let title = scope_wide(title);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let parent = unsafe { scope_find_window_w(std::ptr::null(), title.as_ptr()) };
            if !parent.is_null() {
                return parent;
            }
            std::thread::yield_now();
        }
        panic!("actual Runtime-owned native parent did not appear");
    }
    fn run_scope_terminal_old_api(nested: bool) {
        run_scope_terminal_contract(nested, false, false);
    }
    fn run_scope_terminal_contract(nested: bool, qualify_install: bool, cleanup_retry: bool) {
        use mondrian_core::NativeAudioAuthorCallback as Callback;
        let helper = std::env::var_os("MONDRIAN_VST3_TEST_HELPER")
            .map(PathBuf::from)
            .expect("fresh matching product helper");
        let binary = std::env::var_os("MONDRIAN_VST3_EDITOR_TEST_PLUGIN")
            .map(PathBuf::from)
            .expect("rebuilt owned real IPlugView fixture");
        let catalog = Arc::new(
            DiscoveredVst3AudioProcessorSpecResolver::discover(helper, [binary])
                .expect("actual exact native catalog"),
        );
        let descriptors = catalog.descriptors();
        assert_eq!(descriptors.len(), 1, "owned exact one native class");
        let contract = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 4,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 128 * 1024 * 1024,
            public_output_lookahead_budget_frames:
                AudioRenderContract::DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES,
            compensation_delay_scratch_budget_bytes:
                AudioRenderContract::DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES,
        };
        let processor = catalog
            .create_instance(
                &descriptors[0].class_id,
                contract,
                Some({
                    let mut state = scope_initial_fixture_state();
                    if cleanup_retry {
                        for offset in [52, 84] {
                            state[offset..offset + 4].copy_from_slice(&2u32.to_le_bytes());
                        }
                    }
                    state
                }),
            )
            .expect("genuine native schema/state probe");
        let instance_id = processor.id;
        let mut canonical = sequence_with_audio_clip();
        canonical.settings.audio_channel_layout = AudioChannelLayout::Stereo;
        let scope_id = canonical.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
        canonical
            .audio_program
            .processing_scopes
            .iter_mut()
            .find(|scope| scope.id == scope_id)
            .expect("canonical Scope")
            .processors
            .processors
            .push(processor);
        if !nested {
            let track = canonical.audio_tracks[0].id;
            let clip =
                Clip::new(AssetId::new(), TimelineTime::ZERO, tt(4, 1)).expect("second Clip");
            let id = canonical
                .add_media_audio_clip(track, clip, AudioSourceComponentId::primary())
                .expect("second canonical Contribution");
            canonical.audio_tracks[0]
                .clips
                .iter_mut()
                .find(|clip| clip.id == id)
                .expect("actual second Clip")
                .audio_components[0]
                .processing
                .scope_id = scope_id;
            canonical.audio_program.compact_for_tracks(&canonical.audio_tracks);
        }
        let (root, dependencies) = if nested {
            let mut root = Sequence::new("two simultaneous native child placements");
            root.settings.audio_channel_layout = AudioChannelLayout::Stereo;
            let track = root.audio_tracks[0].id;
            for name in ["first actual child", "second actual child"] {
                let clip = Clip::new_nested_sequence(
                    canonical.id,
                    TimelineTime::ZERO,
                    tt(4, 1),
                    Some(name.to_owned()),
                )
                .expect("exact nested placement");
                root.add_nested_audio_clip(track, clip, canonical.audio_program.outputs[0].id)
                    .expect("canonical nested audio placement");
            }
            (root, vec![canonical.clone()])
        } else {
            (canonical.clone(), Vec::new())
        };
        let resolver = IsolatedAudioProcessorResolver::new(catalog);
        let mut runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
            &root,
            &dependencies,
            &ScopeUnityStereoResolver,
            &resolver,
            contract,
            None,
            AudioRuntimeResourceGrant::new(3, 512 * 1024 * 1024, 512 * 1024 * 1024),
        )
        .expect("actual full retained Runtime tree");
        assert_eq!(
            runtime.resource_footprint().runtime_occurrences,
            if nested { 3 } else { 1 }
        );
        runtime
            .enter_state(AudioStateEntry {
                epoch: AudioContinuityEpoch::new(701),
                start_sample: 0,
            })
            .expect("one real root continuity before any GUI editing");
        let mut positive = [91.0; 8];
        runtime
            .render_into(
                AudioRenderRequest { start_sample: 0, frames: 4 },
                &mut positive,
            )
            .expect("both real native peers processed positive PCM");
        assert_eq!(
            positive, [1.0; 8],
            "two .5 native peer outputs, independently summed"
        );
        let source = mondrian_core::AudioOwnerInstallationSource::new();
        let (registration, verifier) = source.register().expect("actual slot registration");
        runtime
            .bind_native_owner_installation(verifier)
            .expect("same actual root verifier");
        let claim = source
            .begin_commit(
                source.reserve_install(&registration, None).expect("initial slot reservation"),
            )
            .expect("actual root linearization")
            .commit()
            .expect("initial exact installed claim");
        runtime
            .install_native_owner_bindings(&claim, &ExecutionCancellationToken::new())
            .expect("same actual children binding acknowledgements");
        let root_identity = runtime.resource_root_identity();
        let mut observed = ScopeActualCursorVisitor {
            instance: instance_id,
            processors: Vec::with_capacity(2),
        };
        runtime
            .visit_continuity_evidence(&mut observed)
            .expect("sole actual occurrence/epoch observation");
        assert_eq!(observed.processors.len(), 2);
        if nested {
            assert_ne!(
                observed.processors[0].identity.session_id,
                observed.processors[1].identity.session_id
            );
        } else {
            assert_eq!(
                observed.processors[0].identity.session_id,
                observed.processors[1].identity.session_id
            );
            assert_ne!(
                observed.processors[0].occurrence.owner,
                observed.processors[1].occurrence.owner
            );
        }
        let mut targets = Vec::with_capacity(2);
        let mut parents = Vec::with_capacity(2);
        let mut process_ids = Vec::with_capacity(2);
        for cursor in &observed.processors {
            let epoch = match cursor.state {
                AudioProcessorCursorState::Active { epoch, next_input_signal_sample } => {
                    assert_eq!(next_input_signal_sample.sample(), 4);
                    epoch
                }
                other => {
                    panic!("actual first PCM must enter every lazy native occurrence: {other:?}")
                }
            };
            let lease = runtime
                .native_owner_lease(AudioNativeProcessorAddress {
                    root: root_identity,
                    local: cursor.identity,
                    epoch: Some(epoch),
                    occurrence: cursor.occurrence,
                })
                .expect("only actual current Hosted record issues this lease");
            let target = lease.editor_target();
            let title = format!("Scope peer qualification {}", target.binding);
            assert!(matches!(runtime.control_native_editor(Intent::Open {
                target, title: mondrian_core::NativeAudioEditorTitle::new(title.clone()).expect("bounded native title"),
            }, &ExecutionCancellationToken::new()).expect("actual same installed owner open"),
                Outcome::Opened { target: opened, .. } if opened == target));
            let parent = scope_real_parent(&title);
            let mut process_id = 0;
            assert_ne!(
                unsafe { scope_window_process_id(parent, &mut process_id) },
                0
            );
            assert_ne!(process_id, 0);
            targets.push(target);
            parents.push(parent);
            process_ids.push(process_id);
        }
        assert_ne!(parents[0], parents[1], "two actual owned views");
        assert_ne!(
            process_ids[0], process_ids[1],
            "two genuine isolated child instances, not one mirrored controller"
        );
        let class = scope_wide("MondrianOwnedVst3EditorFixture");
        let view = unsafe {
            scope_find_window_ex_w(
                parents[0],
                std::ptr::null_mut(),
                class.as_ptr(),
                std::ptr::null(),
            )
        };
        assert!(!view.is_null(), "real selected native child view");
        assert_ne!(
            unsafe { scope_post_message_w(view, 0x0100, b'G' as usize, 0) },
            0,
            "genuine selected main-thread group callback"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        let proposal = loop {
            assert!(
                Instant::now() < deadline,
                "actual Scope terminal callback delivery timed out"
            );
            if let Some(batch) = runtime
                .poll_native_observations(&ExecutionCancellationToken::new())
                .expect("same serial native observation consumer")
                && let Some(proposal) = batch.author
            {
                break proposal;
            }
            std::thread::yield_now();
        };
        assert_eq!(proposal.target(), targets[0]);
        assert_eq!(proposal.sequence(), canonical.id);
        assert_eq!(proposal.open_parameter_edits(), 0);
        assert_eq!(proposal.group_depth(), 0);
        assert!(
            proposal.terminal(),
            "actual closed native state, not a parameter-only prefix"
        );
        assert_eq!(proposal.callbacks().first(), Some(&Callback::GroupStarted));
        assert_eq!(
            proposal.callbacks().last(),
            Some(&Callback::DirtyChanged(false))
        );
        let state = proposal.state().expect("real coherent terminal bytes");
        assert_eq!(state.len(), 92);
        assert_eq!(&state[..16], b"VST3HOST_STATE\0\0");
        for part in [&state[28..60], &state[60..92]] {
            assert_eq!(&part[..4], b"MEV1");
            assert_eq!(
                f64::from_le_bytes(part[8..16].try_into().expect("gain extent")),
                0.75
            );
            assert_eq!(
                f64::from_le_bytes(part[16..24].try_into().expect("balance extent")),
                0.25
            );
        }
        let mut blocked = [91.0; 8];
        assert!(matches!(
            runtime.render_into(
                AudioRenderRequest { start_sample: 4, frames: 4 },
                &mut blocked
            ),
            Err(AudioExecutionError::NativeAuthorPending(_))
        ));
        assert_eq!(
            blocked, [91.0; 8],
            "no old constants/peer PCM while native author state is unaccepted"
        );
        let processor = &mut canonical
            .audio_program
            .processing_scopes
            .iter_mut()
            .find(|scope| scope.id == scope_id)
            .expect("same canonical Scope")
            .processors
            .processors[0];
        for (id, value) in [("vst3.param.0", 0.75), ("vst3.param.1", 0.25)] {
            processor
                .set_parameter_automation(
                    ExactAutomationCurve::new(ParameterId::new_static(id), value)
                        .expect("exact canonical authored scalar"),
                )
                .expect("native schema-validated curve");
        }
        processor.opaque_state = Some(state.clone());
        let programs = [NativeAudioAuthorProgram {
            sequence: canonical.id,
            program: Arc::new(
                compile_audio_program(
                    &canonical,
                    AudioCompileRequest::program(canonical.audio_program.outputs[0].id),
                )
                .expect("sole canonical next Program"),
            ),
        }];
        let (next_registration, destination) =
            source.register().expect("actual next destination registration");
        eprintln!("SCOPE_NATIVE_PRIMARY: two distinct actual child PIDs {process_ids:?}, positive PCM and coherent92B terminal passed; nested={nested}");
        if cleanup_retry {
            for attempt in 0..2 {
                let failure = runtime.prepare_native_author_parameter_handoff(
                    &programs,
                    &proposal,
                    destination.clone(),
                    &ExecutionCancellationToken::new(),
                );
                assert!(
                    matches!(
                        failure,
                        Err(NativeAudioParameterHandoffError::Admission(
                            NativeAudioAuthorAdmissionError::Pending {
                                reason: NativeAudioAuthorHoldReason::CoherentPeerStateRequired,
                                ..
                            }
                        ))
                    ),
                    "each actual mode2 peer must refuse exactly its first host-frame clear; attempt={attempt} failure={:?}",
                    failure.as_ref().err()
                );
                let mut untouched = [93.0; 8];
                assert!(runtime
                    .render_into(
                        AudioRenderRequest { start_sample: 4, frames: 4 },
                        &mut untouched
                    )
                    .is_err());
                assert_eq!(
                    untouched, [93.0; 8],
                    "partial native state entry never resumes old PCM"
                );
                let current = source.current_claim().expect("source").expect("installed slot");
                assert_eq!(
                    current.generation(),
                    claim.generation(),
                    "no partial peer success may publish the new Source claim"
                );
                let verifier = runtime
                    .native_owner_installation_verifier()
                    .expect("actual Runtime registration");
                verifier
                    .validate(&current)
                    .expect("actual Source claim still belongs to this Runtime");
                verifier.validate(&claim).expect("original installed claim remains current");
                let cancellation = ExecutionCancellationToken::new();
                cancellation.cancel();
                assert!(matches!(
                    runtime.prepare_native_author_parameter_handoff(
                        &programs,
                        &proposal,
                        destination.clone(),
                        &cancellation
                    ),
                    Err(NativeAudioParameterHandoffError::Admission(
                        NativeAudioAuthorAdmissionError::Cancelled
                    ))
                ));
                eprintln!("SCOPE_NATIVE_PARTIAL: peer cleanup failure {attempt}; held exact source/PCM and cancelled retry preserved");
            }
        }
        let mut prepared = runtime.prepare_native_author_parameter_handoff(
            &programs, &proposal, destination, &ExecutionCancellationToken::new(),
        ).expect("all actual shared Scope materializations must complete terminal native state preparation");

        if !qualify_install {
            drop(prepared);
            // OLD gate ends at complete native preparation; no installed claim.
            return;
        }
        assert!(
            parents.iter().all(|parent| unsafe { scope_is_window(*parent) } == 0),
            "every actual old Scope window must close before publication"
        );
        let expected = source.current_claim().expect("current source").expect("old installed slot");
        let unrelated = mondrian_core::AudioOwnerInstallationSource::new();
        let (foreign_registration, _) = unrelated.register().expect("unrelated registration");
        let foreign_reservation =
            unrelated.reserve_install(&foreign_registration, None).expect("unrelated slot");
        let mut wrong_authorization = unrelated
            .authorize_runtime_installation(foreign_reservation)
            .expect("foreign authorization");
        assert!(
            runtime
                .install_native_author_parameter_handoff(
                    &mut prepared,
                    &mut wrong_authorization,
                    &ExecutionCancellationToken::new()
                )
                .is_err(),
            "wrong final CAS cannot publish partially reentered peers"
        );
        let current = source.current_claim().expect("source").expect("installed slot");
        assert_eq!(current.generation(), expected.generation());
        let verifier = runtime
            .native_owner_installation_verifier()
            .expect("actual Runtime registration");
        verifier
            .validate(&current)
            .expect("actual Source claim still belongs to this Runtime");
        verifier.validate(&expected).expect("failed CAS retains exact old claim");
        assert_eq!(runtime.resource_root_identity(), root_identity);
        let mut retained = [93.0; 8];
        assert!(runtime
            .render_into(
                AudioRenderRequest { start_sample: 4, frames: 4 },
                &mut retained
            )
            .is_err());
        assert_eq!(
            retained, [93.0; 8],
            "failed final CAS retains whole old hold and prepared cohort"
        );
        let reservation = source
            .reserve_install(&next_registration, Some(&expected))
            .expect("same actual source replacement reservation");
        let mut authorization = source
            .authorize_runtime_installation(reservation)
            .expect("one exact destination authorization");
        let receipt = runtime
            .install_native_author_parameter_handoff(
                &mut prepared,
                &mut authorization,
                &ExecutionCancellationToken::new(),
            )
            .expect("bounded whole-tree actual Session/Hosted/claim publication");
        assert_eq!(
            receipt.continuity,
            NativeAudioParameterHandoffContinuity::StateEntryRequired
        );
        if nested {
            // Only the child Sequence's canonical program changed. The root
            // retains the same actual immutable Plan in its fresh Session.
            assert_eq!(receipt.target.root_plan, proposal.target().root_plan);
        } else {
            assert_ne!(receipt.target.root_plan, proposal.target().root_plan);
        }
        assert_ne!(receipt.target.local_plan, proposal.target().local_plan);
        assert_ne!(receipt.target.root_session, proposal.target().root_session);
        assert_ne!(
            receipt.target.local_session,
            proposal.target().local_session
        );
        assert_ne!(receipt.target.binding, proposal.target().binding);
        let next_root = runtime.resource_root_identity();
        assert_eq!(next_root.prepared_plan_id, receipt.target.root_plan);
        assert_eq!(next_root.session_id, receipt.target.root_session);
        let mut refused = [93.0; 8];
        assert!(
            runtime
                .render_into(
                    AudioRenderRequest { start_sample: 4, frames: 4 },
                    &mut refused
                )
                .is_err(),
            "no old or fresh PCM before the exact native prefix acknowledgement"
        );
        assert_eq!(refused, [93.0; 8], "held PCM remains untouched");
        let ack = runtime
            .acknowledge_native_author_parameter_handoff(
                &proposal,
                &ExecutionCancellationToken::new(),
            )
            .expect("new actual incarnation acknowledges the original retained prefix");
        assert_eq!(ack.target, receipt.target);
        assert_eq!(
            ack.continuity,
            NativeAudioParameterHandoffContinuity::StateEntryRequired
        );
        let current = source.current_claim().expect("source").expect("installed slot");
        assert_eq!(current.generation(), receipt.claim.generation());
        let verifier = runtime
            .native_owner_installation_verifier()
            .expect("actual Runtime registration");
        verifier.validate(&current).expect("new Source claim belongs to this Runtime");
        verifier
            .validate(&receipt.claim)
            .expect("exact installed receipt remains current");
        drop(prepared);
        // A fresh admitted epoch is required. Prepared child first-entry consumes
        // the restored object once rather than performing a second SDK restore.
        runtime
            .enter_state(AudioStateEntry {
                epoch: AudioContinuityEpoch::new(702),
                start_sample: 4,
            })
            .expect("fresh whole-tree entry");
        let mut accepted = [94.0; 8];
        runtime
            .render_into(
                AudioRenderRequest { start_sample: 4, frames: 4 },
                &mut accepted,
            )
            .expect("every actual peer processes accepted state");
        assert_eq!(
            accepted,
            [2.25, 0.75, 2.25, 0.75, 2.25, 0.75, 2.25, 0.75],
            "two real gain=.75/balance=.25 native peers independently sum"
        );
        let mut after = ScopeActualCursorVisitor {
            instance: instance_id,
            processors: Vec::with_capacity(2),
        };
        runtime.visit_continuity_evidence(&mut after).expect("actual new Session tree");
        assert_eq!(after.processors.len(), 2);
        for (previous, current) in observed.processors.iter().zip(&after.processors) {
            assert_ne!(previous.identity.session_id, current.identity.session_id);
            assert_eq!(previous.occurrence, current.occurrence);
            // Each nested occurrence owns its own monotonic continuity epoch;
            // root entry 702 invalidates its cache, not its local epoch namespace.
            let expected_epoch = if nested {
                let AudioProcessorCursorState::Active { epoch, .. } = previous.state else {
                    panic!("the actual previous peer was not active");
                };
                AudioContinuityEpoch::new(epoch.get().checked_add(1).expect("fresh local epoch"))
            } else {
                AudioContinuityEpoch::new(702)
            };
            assert!(
                matches!(current.state,AudioProcessorCursorState::Active {
                epoch, next_input_signal_sample,
            } if epoch==expected_epoch && next_input_signal_sample.sample()==8),
                "the exact local continuity must advance once: previous={:?}, current={:?}",
                previous.state,
                current.state
            );
        }
        // Ordinary first entry revokes old lazy bindings; issue only from the
        // actual current Hosted records and the same current Source claim.
        let current_claim = source.current_claim().expect("source").expect("installed");
        runtime
            .install_native_owner_bindings(&current_claim, &ExecutionCancellationToken::new())
            .expect("fresh exact current epoch binding acknowledgements");
        for (index, cursor) in after.processors.iter().enumerate() {
            let lease = runtime
                .native_owner_lease(AudioNativeProcessorAddress {
                    root: runtime.resource_root_identity(),
                    local: cursor.identity,
                    occurrence: cursor.occurrence,
                    epoch: match cursor.state {
                        AudioProcessorCursorState::Active { epoch, .. } => Some(epoch),
                        _ => panic!("actual accepted peer is not active"),
                    },
                })
                .expect("fresh actual retained native lease");
            let target = lease.editor_target();
            let title = format!("Scope accepted peer {}", target.binding);
            assert!(matches!(runtime.control_native_editor(Intent::Open {
                target,title:mondrian_core::NativeAudioEditorTitle::new(title.clone()).expect("title"),
            },&ExecutionCancellationToken::new()).expect("reopen actual retained child"),
                Outcome::Opened {target:opened,..} if opened==target));
            let parent = scope_real_parent(&title);
            let mut pid = 0;
            assert_ne!(unsafe { scope_window_process_id(parent, &mut pid) }, 0);
            assert_eq!(
                pid, process_ids[index],
                "same exclusive native child; no hidden replacement process"
            );
            let first_close = runtime
                .control_native_editor(Intent::Close { target }, &ExecutionCancellationToken::new())
                .expect("close fresh exact native view");
            if cleanup_retry {
                assert!(
                    matches!(first_close,Outcome::ClosePending {target:actual,stage:
                    mondrian_core::NativeAudioEditorDetachStage::ClearHostFrame} if actual==target)
                );
                assert!(
                    matches!(runtime.control_native_editor(Intent::Close {target},
                    &ExecutionCancellationToken::new()).expect("retry only real host-frame clear"),
                    Outcome::Closed {target:closed} if closed==target)
                );
            } else {
                assert!(matches!(first_close,Outcome::Closed {target:closed} if closed==target));
            }
        }
        eprintln!("SCOPE_NATIVE_INSTALLED: nested={nested}; full retained tree, fresh Sessions, same actual children, original-prefix Ack and independent accepted PCM passed");
    }

    #[test]
    #[ignore = "fresh matching product helper + owned real Windows IPlugView; full Scope native install qualification"]
    fn native_scope_installed_multiple_clips_new_epoch_same_children_and_independent_pcm() {
        run_scope_terminal_contract(false, true, false);
    }
    #[test]
    #[ignore = "fresh matching product helper + owned real Windows IPlugView; full nested Scope native install qualification"]
    fn native_scope_installed_repeated_nested_new_epoch_same_children_and_independent_pcm() {
        run_scope_terminal_contract(true, true, false);
    }
    #[test]
    #[ignore = "fresh matching product helper + owned real Windows IPlugView; actual partial peer cleanup/retry qualification"]
    fn native_scope_installed_partial_peer_cleanup_retains_hold_and_retries_each_object_once() {
        run_scope_terminal_contract(false, true, true);
        // This old-API gate ends at complete preparation. Actual atomic install,
        // new incarnation Ack/reprime/save is a separate new-contract qualification.
    }

    #[test]
    #[ignore = "fresh matching product helper + owned real Windows IPlugView; actual OLD Scope behavior gate"]
    fn native_author_scope_multiple_clips_complete_terminal_native_preparation() {
        run_scope_terminal_old_api(false);
    }
    #[test]
    #[ignore = "fresh matching product helper + owned real Windows IPlugView; actual OLD nested Scope behavior gate"]
    fn native_author_scope_repeated_nested_placements_complete_terminal_native_preparation() {
        run_scope_terminal_old_api(true);
    }

    mod fee_lifecycle_controls {
        include!("native_author_scope_fee_tests.rs");
    }
    mod whole_budget_controls {
        include!("native_author_scope_budget_tests.rs");
        mod exact_budget_control {
            include!("native_author_scope_exact_budget_tests.rs");
        }
    }
}

#[test]
fn native_author_transient_dsp_feedback_retains_exact_source_then_handoff_and_pcm_continue() {
    let mut fixture = HandoffFixture::new();
    fixture.block(0, 0.5);
    fixture.queue_value(0.75);
    let canonical_before = fixture.sequence.clone();
    let (events, revision, entries) = {
        let mut probe = fixture.probe.lock().expect("actual fixture source");
        probe.catchup_pending = Some(1);
        (probe.pending.clone(), probe.revision, probe.entries)
    };
    assert!(fixture
        .runtime
        .poll_native_observations(&ExecutionCancellationToken::new())
        .expect("pending DSP catchup is not an invalid native author contract")
        .is_none());
    {
        let probe = fixture.probe.lock().expect("actual retained source");
        assert_eq!(
            probe.pending, events,
            "no successful prefix may be silently drained"
        );
        assert_eq!(probe.revision, revision);
        assert_eq!(
            probe.retained, None,
            "pending is not a fabricated author delivery"
        );
        assert_eq!(
            probe.catchup_polls, 1,
            "one poll performs one source attempt"
        );
        assert_eq!(
            probe.entries, entries,
            "catchup cannot reset native continuity"
        );
    }
    assert_eq!(
        fixture.sequence, canonical_before,
        "pending is not an author commit"
    );
    assert!(
        matches!(
            fixture
                .runtime
                .validate_native_author_source_closed(&ExecutionCancellationToken::new()),
            Err(NativeAudioAuthorAdmissionError::Pending { .. })
        ),
        "an absent presentation batch cannot prove native source closure"
    );
    let mut untouched = [91.0];
    assert!(matches!(
        fixture.runtime.render_into(
            AudioRenderRequest { start_sample: 1, frames: 1 },
            &mut untouched
        ),
        Err(AudioExecutionError::NativeAuthorPending(_))
    ));
    assert_eq!(
        untouched,
        [91.0],
        "source pending publishes no substitute PCM"
    );
    fixture.probe.lock().expect("actual catchup completion").catchup_pending = None;
    let proposal = fixture.drain();
    assert_eq!(proposal.target(), fixture.target);
    assert_eq!(proposal.revision(), revision);
    assert_eq!(
        proposal.callbacks().len(),
        events.len(),
        "the exact original prefix is delivered once"
    );
    fixture.install(&proposal, 0.75);
    fixture.ack(&proposal);
    fixture.block(1, 0.75);
}

#[test]
fn native_author_transient_dsp_feedback_cannot_hide_a_sticky_source_failure() {
    let mut fixture = HandoffFixture::new();
    fixture.queue_value(0.75);
    let events = {
        let mut probe = fixture.probe.lock().expect("actual fixture source");
        probe.catchup_pending = Some(1);
        probe.source_failure = Some(crate::NativeAudioAuthorSourceFailure::WrongThread);
        probe.pending.clone()
    };
    assert!(
        matches!(
            fixture.runtime.poll_native_observations(&ExecutionCancellationToken::new()),
            Err(AudioProcessorHostError::InvalidContract(_))
        ),
        "sticky failure must retain its refusal"
    );
    let probe = fixture.probe.lock().expect("failed source remains owned");
    assert_eq!(probe.pending, events);
    assert_eq!(probe.retained, None);
}

#[test]
fn native_author_transient_dsp_feedback_requires_an_actual_pending_feedback_value() {
    let mut fixture = HandoffFixture::new();
    fixture.queue_value(0.75);
    let events = {
        let mut probe = fixture.probe.lock().expect("actual fixture source");
        probe.catchup_pending = Some(0);
        probe.pending.clone()
    };
    assert!(
        matches!(
            fixture.runtime.poll_native_observations(&ExecutionCancellationToken::new()),
            Err(AudioProcessorHostError::InvalidContract(_))
        ),
        "a contradictory producer reply is not a transient observation"
    );
    let probe = fixture.probe.lock().expect("contradictory source remains owned");
    assert_eq!(probe.pending, events);
    assert_eq!(probe.retained, None);
}

#[test]
fn native_author_transient_dsp_feedback_cannot_hide_an_outstanding_hosted_prefix() {
    let mut fixture = HandoffFixture::new();
    let original = fixture.value(0.75);
    fixture.probe.lock().expect("actual suffix waiting for catchup").catchup_pending = Some(1);
    assert!(
        matches!(
            fixture.runtime.poll_native_observations(&ExecutionCancellationToken::new()),
            Err(AudioProcessorHostError::InvalidContract(_))
        ),
        "pending cannot replace an already owned delivery receipt"
    );
    fixture.probe.lock().expect("actual catchup completion").catchup_pending = None;
    let retained = fixture.drain();
    assert!(
        retained.same_retained_proposal(&original),
        "the exact Hosted prefix remains privately retained"
    );
    let probe = fixture.probe.lock().expect("actual native prefix");
    assert_eq!(
        probe.retained,
        Some((
            probe.binding.expect("actual binding"),
            original.revision(),
            None
        ))
    );
}

#[test]
fn native_author_parameter_completion_handoff_ack_continues_same_runtime_without_seed() {
    use mondrian_core::NativeAudioAuthorCompletion as Completion;
    let mut fixture = HandoffFixture::new();
    fixture.block(0, 0.5);
    let value = fixture.value(0.75);
    assert_eq!(value.completion(), Completion::Nonterminal);
    fixture.install(&value, 0.75);
    fixture.ack(&value);
    fixture.block(1, 0.75);
    {
        let mut probe = fixture.probe.lock().expect("actual test ABI source");
        assert_eq!(probe.open, 1);
        probe.open = 0;
        probe.parameter_only = true;
        probe.revision += 1;
        probe.pending.push(NativeAudioAuthorEvent::Parameter(
            NativeAudioAuthorParameterEvent::admit(
                ParameterId::new_static("native.gain"),
                NativeAudioAuthorParameterPhase::End,
                None,
            )
            .expect("actual End"),
        ));
    }
    let terminal = fixture.drain();
    assert_eq!(terminal.completion(), Completion::ParametersOnly);
    assert!(terminal.terminal());
    assert!(terminal.state().is_none());
    let old = fixture.runtime.resource_root_identity();
    fixture.install(&terminal, 0.75);
    fixture.ack(&terminal);
    fixture
        .runtime
        .validate_native_author_source_closed(&ExecutionCancellationToken::new())
        .expect("actual prefix accepted");
    assert_eq!(fixture.target.local_session, old.session_id);
    assert!(fixture.probe.lock().expect("actual child owner").accepted_seed.is_none());
    fixture.block(2, 0.75);
    assert_eq!(
        fixture.probe.lock().expect("actual child owner").opens,
        1,
        "same view/child, no Enter"
    );
}

#[test]
fn native_scope_foreign_source_refused_before_native_binding_or_plan_mutation() {
    let mut fixture = HandoffFixture::new();
    fixture.block(0, 0.5);
    let proposal = fixture.value(0.75);
    fixture.sequence.audio_program.outputs[0].strip.pre_fader.processors[0]
        .set_parameter_automation(
            ExactAutomationCurve::new(ParameterId::new_static("native.gain"), 0.75)
                .expect("exact canonical curve"),
        )
        .expect("schema validated");
    let programs = [NativeAudioAuthorProgram {
        sequence: fixture.sequence.id,
        program: Arc::new(
            compile_audio_program(
                &fixture.sequence,
                AudioCompileRequest::program(fixture.sequence.audio_program.outputs[0].id),
            )
            .expect("sole compiler"),
        ),
    }];
    let foreign = mondrian_core::AudioOwnerInstallationSource::new();
    let (_, destination) = foreign.register().expect("unrelated actual source registration");
    let previous_binding = fixture.probe.lock().expect("owner").binding;
    assert!(matches!(
        fixture.runtime.prepare_native_author_parameter_handoff(
            &programs,
            &proposal,
            destination,
            &ExecutionCancellationToken::new()
        ),
        Err(NativeAudioParameterHandoffError::Owner(
            AudioNativeOwnerError::OwnerUnavailable
        ))
    ));
    assert_eq!(
        fixture.probe.lock().expect("owner").binding,
        previous_binding
    );
    let mut untouched = [93.0];
    assert!(matches!(
        fixture.runtime.render_into(
            AudioRenderRequest { start_sample: 1, frames: 1 },
            &mut untouched
        ),
        Err(AudioExecutionError::NativeAuthorPending(_))
    ));
    assert_eq!(untouched, [93.0]);
    fixture.install(&proposal, 0.75);
    fixture.ack(&proposal);
    fixture.block(1, 0.75);
}

#[path = "native_author_preflight.rs"]
mod native_author_preflight_tests;

#[test]
fn native_author_prefix_cohort_drift_never_replaces_or_discards_hosted_outstanding_receipt() {
    let a = crate::NativeRequestCohort::new(
        41,
        mondrian_core::NativeAudioReprepareRequest::ClapRestart,
    )
    .unwrap();
    let b = crate::NativeRequestCohort::new(42, a.request).unwrap();
    for original in [None, Some(a)] {
        let mut fixture = HandoffFixture::new();
        fixture.block(0, 0.5);
        fixture.probe.lock().unwrap().request_cohort = original;
        let proposal = fixture.value(0.75);
        let bad = if original.is_some() {
            [None, Some(b)]
        } else {
            [Some(a), Some(b)]
        };
        for reported in bad {
            fixture.probe.lock().unwrap().reported_request_cohort = Some(reported);
            let error = match fixture
                .runtime
                .poll_native_observations(&ExecutionCancellationToken::new())
            {
                Err(error) => error,
                Ok(_) => panic!("cohort drift must be a typed refusal"),
            };
            assert!(
                matches!(&error, AudioProcessorHostError::InvalidContract(detail)
                if detail == "native author outstanding receipt mismatch"),
                "actual={error:?}"
            );
            assert_eq!(fixture.probe.lock().unwrap().retained.unwrap().2, original);
        }
        fixture.probe.lock().unwrap().reported_request_cohort = None;
        assert!(
            fixture.drain().same_retained_proposal(&proposal),
            "repairing an invalid observation must expose the original outstanding prefix"
        );
        fixture.install(&proposal, 0.75);
        let root = fixture.runtime.resource_root_identity();
        for reported in bad {
            fixture.probe.lock().unwrap().reported_request_cohort = Some(reported);
            let error = match fixture.runtime.acknowledge_native_author_parameter_handoff(
                &proposal,
                &ExecutionCancellationToken::new(),
            ) {
                Err(error) => error,
                Ok(_) => panic!("Ack cohort drift must preserve the outstanding prefix"),
            };
            assert!(
                matches!(
                    &error,
                    NativeAudioParameterHandoffError::Admission(
                        crate::NativeAudioAuthorAdmissionError::Pending {
                            reason: crate::NativeAudioAuthorHoldReason::OutstandingDelivery,
                            ..
                        }
                    )
                ),
                "actual={error:?}"
            );
            assert_eq!(fixture.probe.lock().unwrap().retained.unwrap().2, original);
            assert_eq!(fixture.runtime.resource_root_identity(), root);
        }
        fixture.probe.lock().unwrap().reported_request_cohort = None;
        fixture.ack(&proposal);
        assert_eq!(fixture.probe.lock().unwrap().retained, None);
        assert!(
            fixture
                .runtime
                .acknowledge_native_author_parameter_handoff(
                    &proposal,
                    &ExecutionCancellationToken::new()
                )
                .is_err(),
            "a confirmed receipt cannot be acknowledged twice"
        );
        fixture.block(1, 0.75);
    }
}

// Portable actual Runtime/Source/Session ownership, not DLL/SDK Note evidence.
mod continuing_rescan_controls {
    use super::*;

    fn cohort(serial: u32, flags: u32) -> crate::NativeRequestCohort {
        crate::NativeRequestCohort::new(
            serial,
            mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags },
        )
        .unwrap()
    }
    fn observed(cohort: crate::NativeRequestCohort) -> crate::NativeObservedRequest {
        crate::NativeObservedRequest::Cohort(cohort)
    }
    fn fixture() -> HandoffFixture {
        HandoffFixture::with_definition(mondrian_timeline::AudioProcessorDefinitionRef::Vst3 {
            class_id: "portable.actual.rescan".into(),
            vendor: None,
            schema_version: 1,
            binary_sha256: None,
        })
    }
    pub(super) fn rescan(
        fixture: &mut HandoffFixture,
        a: crate::NativeRequestCohort,
    ) -> mondrian_core::NativeAudioAuthorProposal {
        let mut probe = fixture.probe.lock().unwrap();
        assert_eq!(probe.open, 0);
        probe.request_cohort = Some(a);
        probe.revision += 1;
        probe.configuration = Some(0.75_f64.to_le_bytes().to_vec());
        probe.pending.push(NativeAudioAuthorEvent::ParameterValuesRescanned {
            kind: mondrian_core::NativeAudioParameterRescanKind::Values,
            values: vec![NativeAudioAuthorParameterEvent::admit(
                ParameterId::new_static("native.gain"),
                NativeAudioAuthorParameterPhase::Value,
                Some(NativeAudioAuthorScalar::admit(0.75).unwrap()),
            )
            .unwrap()],
        });
        drop(probe);
        let proposal = fixture.drain();
        assert_eq!(
            proposal.completion(),
            mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration
        );
        assert_eq!(
            proposal.state().unwrap().as_slice(),
            &0.75_f64.to_le_bytes()
        );
        proposal
    }
    fn ticket(fixture: &HandoffFixture) -> Option<AudioProcessorReprepareTicket> {
        fixture
            .runtime
            .find_resource_session(fixture.runtime.resource_root_identity())
            .unwrap()
            .pending_reprepare_ticket()
    }
    pub(super) fn hold(fixture: &mut HandoffFixture) -> AudioProcessorReprepareTicket {
        let mut pcm = [93.0];
        let result = fixture
            .runtime
            .render_into(AudioRenderRequest { start_sample: 1, frames: 1 }, &mut pcm);
        let Err(AudioExecutionError::ReprepareRequired(ticket)) = result else {
            panic!("expected the real Session ticket, actual={result:?}");
        };
        assert_eq!(pcm, [93.0]);
        assert_eq!(self::ticket(fixture), Some(ticket));
        ticket
    }
    pub(super) fn programs(
        fixture: &mut HandoffFixture,
        proposal: &mondrian_core::NativeAudioAuthorProposal,
    ) -> [NativeAudioAuthorProgram; 1] {
        let strip = match fixture.target.owner {
            mondrian_core::NativeAudioEditorOccurrenceOwner::Output(_) => {
                &mut fixture.sequence.audio_program.outputs[0].strip
            }
            mondrian_core::NativeAudioEditorOccurrenceOwner::Track(track) => {
                &mut fixture.sequence.audio_program.track_channels.get_mut(&track).unwrap().strip
            }
            other => panic!("unsupported portable test placement: {other:?}"),
        };
        let rack = match fixture.target.insertion {
            mondrian_core::NativeAudioEditorInsertion::PreFader => &mut strip.pre_fader,
            mondrian_core::NativeAudioEditorInsertion::PostFader => &mut strip.post_fader,
            other => panic!("unsupported portable test insertion: {other:?}"),
        };
        let processor = rack
            .processors
            .iter_mut()
            .find(|processor| processor.id == fixture.target.instance)
            .unwrap();
        processor.opaque_state = proposal.state().cloned();
        processor
            .set_parameter_automation(
                ExactAutomationCurve::new(ParameterId::new_static("native.gain"), 0.75).unwrap(),
            )
            .unwrap();
        [NativeAudioAuthorProgram {
            sequence: fixture.sequence.id,
            program: Arc::new(
                compile_audio_program(
                    &fixture.sequence,
                    AudioCompileRequest::program(fixture.sequence.audio_program.outputs[0].id),
                )
                .unwrap(),
            ),
        }]
    }
    pub(super) fn prepare(
        fixture: &mut HandoffFixture,
        proposal: &mondrian_core::NativeAudioAuthorProposal,
    ) -> (
        PreparedNativeAudioParameterHandoff,
        mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
    ) {
        let claim = fixture.source.current_claim().unwrap().unwrap();
        let current = fixture.runtime.native_owner_installation_verifier().unwrap();
        current.validate(&claim).unwrap();
        let programs = programs(fixture, proposal);
        let (registration, destination) = fixture.source.register().unwrap();
        let reservation = fixture.source.reserve_install(&registration, Some(&claim)).unwrap();
        let authorization = fixture.source.authorize_runtime_installation(reservation).unwrap();
        let prepared = fixture
            .runtime
            .prepare_native_author_parameter_handoff(
                &programs,
                proposal,
                destination,
                &ExecutionCancellationToken::new(),
            )
            .unwrap();
        let unchanged = fixture.source.current_claim().unwrap().unwrap();
        current.validate(&unchanged).unwrap();
        assert_eq!(unchanged.generation(), claim.generation());
        assert!(fixture
            .runtime
            .native_owner_installation_verifier()
            .unwrap()
            .same_registration(&current));
        (prepared, authorization)
    }
    pub(super) fn install(
        fixture: &mut HandoffFixture,
        proposal: &mondrian_core::NativeAudioAuthorProposal,
        prepared: &mut PreparedNativeAudioParameterHandoff,
        authorization: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
    ) -> mondrian_core::AudioOwnerInstallationClaim {
        let old = fixture.target;
        let old_claim = fixture.source.current_claim().unwrap().unwrap();
        let old_verifier = fixture.runtime.native_owner_installation_verifier().unwrap();
        old_verifier.validate(&old_claim).unwrap();
        assert_eq!(old.installed_generation, old_claim.generation());
        let receipt = fixture
            .runtime
            .install_native_author_parameter_handoff(
                prepared,
                authorization,
                &ExecutionCancellationToken::new(),
            )
            .unwrap();
        assert_eq!(
            receipt.continuity,
            NativeAudioParameterHandoffContinuity::Preserved
        );
        assert_ne!(receipt.target.binding, old.binding);
        assert_eq!(receipt.target.local_session, old.local_session);
        assert_eq!(receipt.target.continuity_epoch, old.continuity_epoch);
        assert_eq!(proposal.target(), old);
        let current = fixture.source.current_claim().unwrap().unwrap();
        let destination = fixture.runtime.native_owner_installation_verifier().unwrap();
        destination.validate(&receipt.claim).unwrap();
        destination.validate(&current).unwrap();
        assert_eq!(current.generation(), receipt.claim.generation());
        assert_eq!(
            receipt.target.installed_generation,
            receipt.claim.generation()
        );
        assert!(receipt.claim.generation() > old_claim.generation());
        assert_eq!(
            old_verifier.validate(&current),
            Err(mondrian_core::AudioOwnerInstallationError::WrongRegistration)
        );
        assert_eq!(
            old_verifier.validate(&old_claim),
            Err(mondrian_core::AudioOwnerInstallationError::Superseded)
        );
        assert!(matches!(
            authorization.begin_commit(&destination),
            Err(mondrian_core::AudioOwnerInstallationError::ConsumedAuthorization)
        ));
        destination.validate(&fixture.source.current_claim().unwrap().unwrap()).unwrap();
        fixture.target = receipt.target;
        receipt.claim
    }
    #[derive(Debug, Default, PartialEq, Eq)]
    struct Cursors {
        sessions: Vec<AudioSessionCursorEvidence>,
        processors: Vec<AudioProcessorCursorEvidence>,
    }
    impl AudioContinuityEvidenceVisitor for Cursors {
        fn session(&mut self, e: AudioSessionCursorEvidence) -> Result<(), AudioExecutionError> {
            self.sessions.push(e);
            Ok(())
        }
        fn processor(
            &mut self,
            e: AudioProcessorCursorEvidence,
        ) -> Result<(), AudioExecutionError> {
            self.processors.push(e);
            Ok(())
        }
    }
    fn cursors(fixture: &HandoffFixture) -> Cursors {
        let mut value = Cursors::default();
        fixture.runtime.visit_continuity_evidence(&mut value).unwrap();
        value
    }
    fn counts(fixture: &HandoffFixture) -> (usize, usize, usize, usize, usize, usize) {
        let p = fixture.probe.lock().unwrap();
        (
            p.creates,
            p.binds,
            p.entries,
            p.processed.len(),
            p.ack_attempts,
            p.accepted_acks,
        )
    }

    #[test]
    fn authentic_vst_rescan_install_preserves_later_request_at_all_four_boundaries() {
        let a = cohort(41, 4);
        let b = cohort(42, 4);
        for arrival in 0..4 {
            let mut f = fixture();
            f.block(0, 0.5);
            f.probe.lock().unwrap().live_request = Some(observed(a));
            let old_ticket = hold(&mut f);
            let old_cursors = cursors(&f);
            let proposal = rescan(&mut f, a);
            if arrival == 0 {
                f.probe.lock().unwrap().live_request = Some(observed(b));
            }
            let (mut prepared, mut authorization) = prepare(&mut f, &proposal);
            if arrival == 1 {
                f.probe.lock().unwrap().live_request = Some(observed(b));
            }
            // Registration and reserve_install share the Source serial issuer;
            // the installed identity is the actual one-shot receipt, not old + 1.
            let installed_claim = install(&mut f, &proposal, &mut prepared, &mut authorization);
            let moved = ticket(&f).unwrap();
            assert_eq!(
                moved,
                AudioProcessorReprepareTicket {
                    prepared_plan_id: f.runtime.resource_root_identity().prepared_plan_id,
                    ..old_ticket
                }
            );
            let installed = cursors(&f);
            assert_eq!(old_cursors.sessions[0].state, installed.sessions[0].state);
            assert_eq!(
                old_cursors.processors[0].state,
                installed.processors[0].state
            );
            if arrival == 2 {
                f.probe.lock().unwrap().live_request = Some(observed(b));
            }
            if arrival == 3 {
                f.probe.lock().unwrap().live_after_ack = Some(Some(observed(b)));
            }
            let before = counts(&f);
            f.ack(&proposal);
            f.runtime
                .native_owner_installation_verifier()
                .unwrap()
                .validate(&installed_claim)
                .unwrap();
            assert_eq!(
                f.source.current_claim().unwrap().unwrap().generation(),
                installed_claim.generation(),
                "Ack cannot publish a second Source generation"
            );
            assert_eq!(
                ticket(&f).unwrap(),
                AudioProcessorReprepareTicket { request: observed(b), ..moved }
            );
            assert_eq!(cursors(&f), installed);
            let after = counts(&f);
            assert_eq!(
                (after.0, after.1, after.2, after.3),
                (before.0, before.1, before.2, before.3)
            );
            assert_eq!((after.4, after.5), (1, 1));
            assert_eq!(f.probe.lock().unwrap().retained, None);
            assert_eq!(hold(&mut f).request, observed(b));
            assert_eq!(counts(&f), after);
            assert_eq!(cursors(&f), installed);
            drop(prepared);
            let suffix = rescan(&mut f, b);
            assert!(suffix.revision() > proposal.revision());
            let (mut suffix_prepared, mut suffix_authorization) = prepare(&mut f, &suffix);
            install(
                &mut f,
                &suffix,
                &mut suffix_prepared,
                &mut suffix_authorization,
            );
            f.ack(&suffix);
            assert_eq!(ticket(&f), None);
            assert_eq!(counts(&f).2, 1);
            assert_eq!(counts(&f).3, 1, "neither transaction may perform DSP");
            f.block(1, 0.75);
            drop(suffix_prepared);
        }
    }

    #[test]
    fn authentic_vst_rescan_ack_clears_only_a_and_keeps_non_a_or_unknown_live_hold() {
        let a = cohort(71, 4);
        let b = cohort(72, 4);
        for existing_b in [false, true] {
            for live in [
                Some(observed(b)),
                Some(crate::NativeObservedRequest::RequestOnly(
                    mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 0x4000 },
                )),
                None,
            ] {
                let mut f = fixture();
                f.block(0, 0.5);
                f.probe.lock().unwrap().live_request =
                    Some(observed(if existing_b { b } else { a }));
                let old = hold(&mut f);
                let proposal = rescan(&mut f, a);
                let (mut prepared, mut authorization) = prepare(&mut f, &proposal);
                install(&mut f, &proposal, &mut prepared, &mut authorization);
                let installed = ticket(&f).unwrap();
                assert_eq!(installed.request, old.request);
                let cancellation = ExecutionCancellationToken::new();
                {
                    let mut p = f.probe.lock().unwrap();
                    p.live_after_ack = Some(live);
                    p.cancel_on_accepted_ack = Some(cancellation.clone());
                }
                let before = counts(&f);
                let before_cursors = cursors(&f);
                let receipt = f
                    .runtime
                    .acknowledge_native_author_parameter_handoff(&proposal, &cancellation)
                    .unwrap();
                assert!(cancellation.is_canceled());
                assert_eq!(receipt.target, f.target);
                assert_eq!(cursors(&f), before_cursors);
                let expected = if existing_b {
                    Some(installed)
                } else {
                    live.map(|request| AudioProcessorReprepareTicket { request, ..installed })
                };
                assert_eq!(ticket(&f), expected);
                let after = counts(&f);
                assert_eq!(
                    (after.0, after.1, after.2, after.3),
                    (before.0, before.1, before.2, before.3)
                );
                if let Some(expected) = expected {
                    assert_eq!(hold(&mut f), expected);
                } else {
                    f.block(1, 0.75);
                }
                assert!(f
                    .runtime
                    .acknowledge_native_author_parameter_handoff(
                        &proposal,
                        &ExecutionCancellationToken::new()
                    )
                    .is_err());
                assert_eq!(counts(&f).4, 1);
                drop(prepared);
            }
        }
    }

    #[test]
    fn authentic_vst_rescan_external_retire_conflict_retries_only_original_ack() {
        let a = cohort(91, 4);
        let mut f = fixture();
        f.block(0, 0.5);
        f.probe.lock().unwrap().live_request = Some(observed(a));
        hold(&mut f);
        let proposal = rescan(&mut f, a);
        let (mut prepared, mut authorization) = prepare(&mut f, &proposal);
        install(&mut f, &proposal, &mut prepared, &mut authorization);
        let before = cursors(&f);
        let target = f.target;
        let claim = f.source.current_claim().unwrap().unwrap();
        let c = counts(&f);
        // Portable model of SDK settle conflict, not a claim of actual SDK
        // external-retirement coverage. The old receipt remains authoritative.
        {
            let mut p = f.probe.lock().unwrap();
            p.reject_ack = true;
            p.live_request = None;
        }
        assert!(matches!(
            f.runtime.acknowledge_native_author_parameter_handoff(
                &proposal,
                &ExecutionCancellationToken::new()
            ),
            Err(NativeAudioParameterHandoffError::Admission(
                NativeAudioAuthorAdmissionError::Pending {
                    reason: NativeAudioAuthorHoldReason::OutstandingDelivery,
                    ..
                }
            ))
        ));
        assert_eq!(f.target, target);
        assert_eq!(cursors(&f), before);
        assert_eq!(
            f.source.current_claim().unwrap().unwrap().generation(),
            claim.generation()
        );
        assert_eq!(f.probe.lock().unwrap().retained.unwrap().2, Some(a));
        assert_eq!(hold(&mut f).request, observed(a));
        {
            let mut p = f.probe.lock().unwrap();
            p.reject_ack = false;
        }
        f.ack(&proposal);
        assert_eq!(ticket(&f), None);
        assert_eq!(cursors(&f), before);
        let after = counts(&f);
        assert_eq!((after.0, after.1, after.2, after.3), (c.0, c.1, c.2, c.3));
        assert_eq!((after.4, after.5), (2, 1));
        f.block(1, 0.75);
        drop(prepared);
    }

    #[test]
    fn authentic_vst_rescan_pending_permission_refuses_raw_mixed_exhausted_and_backwards_identity()
    {
        let a = cohort(101, 4);
        for live in [
            crate::NativeObservedRequest::RequestOnly(a.request),
            observed(cohort(100, 4)),
            observed(cohort(102, 20)),
            observed(cohort(u32::MAX, 4)),
        ] {
            let mut f = fixture();
            f.block(0, 0.5);
            f.probe.lock().unwrap().live_request = Some(observed(a));
            hold(&mut f);
            let proposal = rescan(&mut f, a);
            f.probe.lock().unwrap().live_request = Some(live);
            let before = cursors(&f);
            let c = counts(&f);
            let root = f.runtime.resource_root_identity();
            let claim = f.source.current_claim().unwrap().unwrap();
            let programs = programs(&mut f, &proposal);
            let (_, destination) = f.source.register().unwrap();
            let result = f.runtime.prepare_native_author_parameter_handoff(
                &programs,
                &proposal,
                destination,
                &ExecutionCancellationToken::new(),
            );
            assert!(matches!(
                result,
                Err(NativeAudioParameterHandoffError::Admission(
                    NativeAudioAuthorAdmissionError::Pending {
                        reason: NativeAudioAuthorHoldReason::PendingCallbacks,
                        ..
                    }
                ))
            ));
            assert_eq!(counts(&f), c);
            assert_eq!(f.runtime.resource_root_identity(), root);
            assert_eq!(cursors(&f), before);
            assert_eq!(
                f.source.current_claim().unwrap().unwrap().generation(),
                claim.generation()
            );
            assert_eq!(f.probe.lock().unwrap().retained.unwrap().2, Some(a));
        }
    }

    #[test]
    fn pending_rescan_permission_requires_coherent_vst_exact_four_prefix() {
        let a = cohort(141, 4);
        for case in 0..5 {
            let mut f = if case == 0 {
                HandoffFixture::new()
            } else {
                fixture()
            };
            f.block(0, 0.5);
            let original = match case {
                1 => cohort(142, 20),
                2 => cohort(u32::MAX, 4),
                _ => a,
            };
            f.probe.lock().unwrap().live_request = Some(observed(original));
            let proposal = if case >= 3 {
                {
                    let mut p = f.probe.lock().unwrap();
                    p.request_cohort = Some(original);
                    p.parameter_only = case == 4;
                }
                f.queue_value(0.75);
                if case == 4 {
                    let mut p = f.probe.lock().unwrap();
                    p.open = 0;
                    p.revision += 1;
                    p.pending.push(NativeAudioAuthorEvent::Parameter(
                        NativeAudioAuthorParameterEvent::admit(
                            ParameterId::new_static("native.gain"),
                            NativeAudioAuthorParameterPhase::End,
                            None,
                        )
                        .unwrap(),
                    ));
                }
                f.drain()
            } else {
                rescan(&mut f, original)
            };
            let before = cursors(&f);
            let c = counts(&f);
            let root = f.runtime.resource_root_identity();
            let claim = f.source.current_claim().unwrap().unwrap();
            let programs = programs(&mut f, &proposal);
            let (_, destination) = f.source.register().unwrap();
            let error = f
                .runtime
                .prepare_native_author_parameter_handoff(
                    &programs,
                    &proposal,
                    destination,
                    &ExecutionCancellationToken::new(),
                )
                .err()
                .expect("invalid Some must never fall back to ordinary permission");
            assert!(
                matches!(
                    error,
                    NativeAudioParameterHandoffError::Admission(
                        NativeAudioAuthorAdmissionError::Pending {
                            reason: NativeAudioAuthorHoldReason::OutstandingDelivery,
                            ..
                        }
                    )
                ),
                "case={case}, actual={error:?}"
            );
            assert_eq!(cursors(&f), before);
            assert_eq!(counts(&f), c);
            assert_eq!(f.runtime.resource_root_identity(), root);
            assert_eq!(
                f.source.current_claim().unwrap().unwrap().generation(),
                claim.generation()
            );
            assert_eq!(f.probe.lock().unwrap().retained.unwrap().2, Some(original));
        }
    }

    #[test]
    fn authentic_vst_rescan_final_commit_rechecks_live_request_against_prior_lease() {
        let a = cohort(111, 4);
        for live in [
            None,
            Some(crate::NativeObservedRequest::RequestOnly(a.request)),
            Some(observed(cohort(110, 4))),
            Some(observed(cohort(112, 20))),
            Some(observed(cohort(u32::MAX, 4))),
        ] {
            let mut f = fixture();
            f.block(0, 0.5);
            f.probe.lock().unwrap().live_request = Some(observed(a));
            let original = hold(&mut f);
            let proposal = rescan(&mut f, a);
            let (mut prepared, mut authorization) = prepare(&mut f, &proposal);
            let before = cursors(&f);
            let c = counts(&f);
            let claim = f.source.current_claim().unwrap().unwrap();
            f.probe.lock().unwrap().live_request = live;
            let error = f
                .runtime
                .install_native_author_parameter_handoff(
                    &mut prepared,
                    &mut authorization,
                    &ExecutionCancellationToken::new(),
                )
                .err()
                .expect("final native fact must be rechecked");
            assert!(
                matches!(
                    error,
                    NativeAudioParameterHandoffError::Admission(
                        NativeAudioAuthorAdmissionError::Pending {
                            reason: NativeAudioAuthorHoldReason::PendingCallbacks,
                            ..
                        }
                    )
                ),
                "actual={error:?}"
            );
            assert_eq!(cursors(&f), before);
            assert_eq!(counts(&f), c);
            assert_eq!(ticket(&f), Some(original));
            assert_eq!(
                f.source.current_claim().unwrap().unwrap().generation(),
                claim.generation()
            );
            assert_eq!(f.probe.lock().unwrap().retained.unwrap().2, Some(a));
            f.probe.lock().unwrap().live_request = Some(observed(a));
            install(&mut f, &proposal, &mut prepared, &mut authorization);
            f.ack(&proposal);
            f.block(1, 0.75);
        }
    }

    #[test]
    fn uninstalled_session_ticket_survives_empty_child_pending_without_rebinding_or_pcm() {
        let a = cohort(121, 4);
        let mut f = fixture();
        f.block(0, 0.5);
        f.probe.lock().unwrap().live_request = Some(observed(a));
        let original = hold(&mut f);
        let before = cursors(&f);
        let c = counts(&f);
        let claim = f.source.current_claim().unwrap().unwrap();
        let target = f.target;
        // This changes only the same portable native owner's live fact, not
        // the real Session's ticket or the author/Source installation. The
        // Product gate separately proves actual adapter Ack-A settlement.
        f.probe.lock().unwrap().live_request = None;
        assert_eq!(hold(&mut f), original);
        assert_eq!(cursors(&f), before);
        assert_eq!(counts(&f), c);
        assert_eq!(f.target, target);
        assert_eq!(
            f.source.current_claim().unwrap().unwrap().generation(),
            claim.generation()
        );
        assert!(f.probe.lock().unwrap().retained.is_none());
    }
}
