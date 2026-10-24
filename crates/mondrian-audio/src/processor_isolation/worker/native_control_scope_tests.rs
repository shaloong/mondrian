//! Real socket/ingress controls for control-only and scoped DSP command routing.
use super::*;
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

struct Main {
    cohort: Option<super::super::NativeRequestCohort>,
    ticks: usize,
    started: mpsc::Sender<()>,
    author_binding: Option<crate::NativeAudioObjectBindRequest>,
    binding_checks: std::cell::Cell<usize>,
}
impl super::super::IsolatedAudioProcessorMainRole for Main {
    fn service_native_main_thread(&mut self) -> Result<(), AudioProcessorHostError> {
        self.ticks += 1;
        if self.ticks <= 2 {
            self.started.send(()).expect("idle main remains serviced");
        }
        Ok(())
    }
    fn finish_processing_scope(&mut self) -> Result<(), AudioProcessorHostError> {
        panic!("control-only service has no DSP scope to finish")
    }
    fn native_configuration_cohort(
        &self,
    ) -> Result<Option<super::super::NativeRequestCohort>, AudioProcessorHostError> {
        Ok(self.cohort)
    }
    fn pending_reprepare(
        &self,
    ) -> Result<Option<crate::NativeObservedRequest>, AudioProcessorHostError> {
        Ok(self.cohort.map(crate::NativeObservedRequest::Cohort))
    }
    fn native_author_binding_matches(
        &self,
        binding: crate::NativeAudioObjectBindRequest,
    ) -> Result<bool, AudioProcessorHostError> {
        self.binding_checks.set(self.binding_checks.get() + 1);
        Ok(self.author_binding == Some(binding))
    }
    fn preserve_native_observations(
        &self,
    ) -> Result<crate::NativeAudioObservations, AudioProcessorHostError> {
        panic!("DrainAuthor must hand off to its owner, not consume a main Preserve path")
    }
}

fn command(kind: CommandKind, frames: u32) -> WorkerCommand {
    WorkerCommand {
        transport_activity: None,
        kind,
        start_sample: 0,
        frames,
        event_count: 0,
        note_count: 0,
    }
}

#[test]
fn control_only_main_keeps_servicing_and_preserves_pending_without_dsp() {
    use super::super::native_configuration_wire::read_cohort_reply;
    use super::super::native_worker_command::NativeCommand;
    use super::super::native_worker_ingress::with_native_ingress;
    let request = mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 8 };
    let cohort = super::super::NativeRequestCohort::new(7, request).unwrap();
    for pending in [Some(cohort), None] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut parent = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut child, _) = listener.accept().unwrap();
        for socket in [&parent, &child] {
            socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            socket.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
        }
        let (started, ticks) = mpsc::channel();
        std::thread::scope(|scope| {
            let child = scope.spawn(move || {
                let mut main = Main {
                    cohort: pending,
                    ticks: 0,
                    started,
                    author_binding: None,
                    binding_checks: std::cell::Cell::new(0),
                };
                with_native_ingress(&mut child, 0, None, |control, ingress| {
                    let exit = service_native_main_scope(&mut main, None, ingress, control)?;
                    match (pending, exit) {
                        (Some(_), NativeScopeExit::OwnerCommand(NativeCommand::Plain(header))) => {
                            // Lifecycle/control commands return to the original
                            // full owner; the main borrow does not execute them.
                            assert_eq!(header.kind, CommandKind::Shutdown);
                            WorkerResponse::Completed.write_to(control).unwrap();
                            ingress.acknowledge()?;
                        }
                        (None, NativeScopeExit::Completed) => {}
                        _ => panic!("unexpected control-only dispatch outcome"),
                    }
                    Ok(())
                })
                .unwrap();
                assert_eq!(main.cohort, pending, "no synthetic retirement or ACK");
                assert!(main.ticks >= 2);
            });
            // Two idle ticks prove the main role keeps being called while the
            // original ingress waits. No timing sleep or processing job is used.
            for _ in 0..2 {
                ticks.recv_timeout(Duration::from_secs(2)).unwrap();
            }
            command(CommandKind::PollConfigurationCohort, 0).write_to(&mut parent).unwrap();
            assert_eq!(read_cohort_reply(&mut parent).unwrap(), pending);
            command(CommandKind::Process, 4).write_to(&mut parent).unwrap();
            let result = WorkerResponse::read_from(&mut parent).unwrap();
            if pending.is_some() {
                assert_eq!(
                    result,
                    WorkerResponse::DeferredForConfiguration(crate::NativeObservedRequest::Cohort(
                        cohort
                    ))
                );
                command(CommandKind::PollPending, 0).write_to(&mut parent).unwrap();
                assert_eq!(
                    WorkerResponse::read_from(&mut parent).unwrap(),
                    WorkerResponse::CompletedWithPending(crate::NativeObservedRequest::Cohort(
                        cohort
                    ))
                );
                command(CommandKind::PollConfigurationCohort, 0).write_to(&mut parent).unwrap();
                assert_eq!(read_cohort_reply(&mut parent).unwrap(), pending);
                command(CommandKind::Shutdown, 0).write_to(&mut parent).unwrap();
                assert_eq!(
                    WorkerResponse::read_from(&mut parent).unwrap(),
                    WorkerResponse::Completed
                );
            } else {
                assert!(
                    matches!(result, WorkerResponse::Failed(detail) if detail.contains("explicit state entry"))
                );
            }
            child.join().unwrap();
        });
    }
}

#[test]
fn control_only_matching_author_drain_returns_intact_to_owner_without_dsp_or_reply() {
    use super::super::native_audio_object_wire::write_observation_request;
    use super::super::native_worker_command::NativeCommand;
    use super::super::native_worker_ingress::with_native_ingress;
    use crate::{
        NativeAudioObjectBindRequest, NativeAudioObjectObservation,
        NativeAudioObservationConsumption,
    };
    use std::num::{NonZeroU32, NonZeroU64};
    let binding = NativeAudioObjectBindRequest {
        binding: mondrian_core::AudioProcessorBindingId::new(),
        expected_object: NativeAudioObjectObservation::new(
            NonZeroU32::new(std::process::id()).unwrap(),
            NonZeroU64::new(19).unwrap(),
        ),
    };
    let cohort = super::super::NativeRequestCohort::new(
        7,
        mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 8 },
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut parent = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut child, _) = listener.accept().unwrap();
    for socket in [&parent, &child] {
        socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        socket.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
    }
    std::thread::scope(|scope| {
        let child = scope.spawn(move || {
            let (started, _ticks) = mpsc::channel();
            let mut main = Main {
                cohort: Some(cohort),
                ticks: 0,
                started,
                author_binding: Some(binding),
                binding_checks: std::cell::Cell::new(0),
            };
            with_native_ingress(&mut child, 0, None, |control, ingress| {
                // No DSP dispatch exists. In particular this cannot acknowledge
                // a successful flush, finish a DSP scope or drain author data.
                let exit = service_native_main_scope(&mut main, None, ingress, control)?;
                let NativeScopeExit::OwnerCommand(NativeCommand::PollObservations(
                    NativeAudioObservationConsumption::DrainAuthor(actual),
                )) = exit
                else {
                    panic!("original author Drain must return to the full owner");
                };
                assert_eq!(actual, binding);
                assert_eq!(main.binding_checks.get(), 1, "the binding actually matched");
                assert_eq!(main.cohort, Some(cohort));
                assert!(
                    ingress.receive()?.is_none(),
                    "no next decode before the owner's response ACK"
                );
                // Test-only closing reply at the returned-command boundary. It
                // is neither real full-owner polling nor a native author flush
                // or delivery, and must never become a successful author ACK.
                WorkerResponse::Failed("test owner handoff boundary; no author flush".into())
                    .write_to(control)
                    .unwrap();
                ingress.acknowledge()?;
                let exit = service_native_main_scope(&mut main, None, ingress, control)?;
                assert!(
                    matches!(exit, NativeScopeExit::OwnerCommand(NativeCommand::Plain(header))
                    if header.kind == CommandKind::Shutdown)
                );
                assert_eq!(main.binding_checks.get(), 1);
                assert_eq!(main.cohort, Some(cohort));
                WorkerResponse::Completed.write_to(control).unwrap();
                ingress.acknowledge()?;
                Ok(())
            })
            .unwrap();
        });
        command(CommandKind::PollNativeObservations, 0).write_to(&mut parent).unwrap();
        write_observation_request(
            &mut parent,
            NativeAudioObservationConsumption::DrainAuthor(binding),
        )
        .unwrap();
        command(CommandKind::Shutdown, 0).write_to(&mut parent).unwrap();
        assert_eq!(
            WorkerResponse::read_from(&mut parent).unwrap(),
            WorkerResponse::Failed("test owner handoff boundary; no author flush".into())
        );
        assert_eq!(
            WorkerResponse::read_from(&mut parent).unwrap(),
            WorkerResponse::Completed
        );
        child.join().unwrap();
    });
}

// These synthetic roles prove real command/slot dispatch, not a native author
// producer or prepared-entry/Ack lifecycle. Actual adapter evidence is separate.
struct DispatchMain {
    binding: crate::NativeAudioObjectBindRequest,
    eligible: bool,
    checks: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    finished: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl super::super::IsolatedAudioProcessorMainRole for DispatchMain {
    fn allocation_layouts(&self) -> Option<[crate::NativeHostBufferLayout; 2]> {
        Some([
            dispatch_layout(std::alloc::Layout::for_value(self)),
            dispatch_empty(),
        ])
    }
    fn service_native_main_thread(&mut self) -> Result<(), AudioProcessorHostError> {
        Ok(())
    }
    fn finish_processing_scope(&mut self) -> Result<(), AudioProcessorHostError> {
        self.finished.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
    fn native_configuration_cohort(
        &self,
    ) -> Result<Option<super::super::NativeRequestCohort>, AudioProcessorHostError> {
        Ok(None)
    }
    fn pending_reprepare(
        &self,
    ) -> Result<Option<crate::NativeObservedRequest>, AudioProcessorHostError> {
        Ok(None)
    }
    fn native_author_binding_matches(
        &self,
        binding: crate::NativeAudioObjectBindRequest,
    ) -> Result<bool, AudioProcessorHostError> {
        self.checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(self.eligible && self.binding == binding)
    }
}
struct DispatchDsp {
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pending: std::sync::Arc<std::sync::Mutex<Vec<f64>>>,
}
impl super::super::IsolatedAudioProcessorProcessingRole for DispatchDsp {
    fn allocation_layouts(&self) -> Option<[crate::NativeHostBufferLayout; 2]> {
        Some([
            dispatch_layout(std::alloc::Layout::for_value(self)),
            dispatch_empty(),
        ])
    }
    fn process(
        &mut self,
        _: super::super::IsolatedAudioProcessorWorkerBlock<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        panic!("DrainAuthor cannot become a PCM job")
    }
    fn flush_native_author_parameters(
        &mut self,
    ) -> Result<crate::NativeAudioAuthorParameterFlushOutcome, AudioProcessorHostError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.pending.lock().unwrap().clear();
        Ok(crate::NativeAudioAuthorParameterFlushOutcome::Flushed)
    }
}
fn dispatch_empty() -> crate::NativeHostBufferLayout {
    crate::NativeHostBufferLayout { bytes: 0, alignment: 1 }
}
fn dispatch_layout(layout: std::alloc::Layout) -> crate::NativeHostBufferLayout {
    crate::NativeHostBufferLayout { bytes: layout.size(), alignment: layout.align() }
}

#[test]
fn author_drain_eligibility_controls_the_real_scoped_dsp_slot_before_owner_handoff() {
    use super::super::native_audio_object_wire::write_observation_request;
    use super::super::native_worker_command::NativeCommand;
    use super::super::native_worker_ingress::{
        with_native_ingress, NativeCommandIngress, NativeReaderSlots,
    };
    use super::super::native_worker_processing::{
        with_processing_scope, NativeDspDispatch, NativeProcessingSlots,
    };
    use crate::NativeAudioObservationConsumption as Consumption;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };
    for eligible in [false, true] {
        for matching in [false, true] {
            let binding = crate::NativeAudioObjectBindRequest {
                binding: mondrian_core::AudioProcessorBindingId::new(),
                expected_object: crate::NativeAudioObjectObservation::new(
                    std::num::NonZeroU32::new(std::process::id()).unwrap(),
                    std::num::NonZeroU64::new(29).unwrap(),
                ),
            };
            let requested = if matching {
                binding
            } else {
                crate::NativeAudioObjectBindRequest {
                    binding: mondrian_core::AudioProcessorBindingId::new(),
                    ..binding
                }
            };
            let calls = Arc::new(AtomicUsize::new(0));
            let checks = Arc::new(AtomicUsize::new(0));
            let finished = Arc::new(AtomicUsize::new(0));
            let pending = Arc::new(Mutex::new(vec![0.875]));
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let mut parent = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (mut child, _) = listener.accept().unwrap();
            for socket in [&parent, &child] {
                socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                socket.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
            }
            let owners = crate::NativeWorkerLaneOwnedLayouts {
                // This control constructs roles, not an adapter Worker Box.
                adapter_resident: dispatch_empty(),
                reader_slots: dispatch_layout(std::alloc::Layout::new::<NativeReaderSlots>()),
                processing_slots: dispatch_layout(
                    std::alloc::Layout::new::<NativeProcessingSlots>(),
                ),
                main_inline: dispatch_layout(std::alloc::Layout::new::<(
                    NativeCommandIngress<'static>,
                    WorkerProcessContext<'static>,
                    NativeDspDispatch<'static>,
                )>()),
                runtime_main_role: dispatch_layout(std::alloc::Layout::new::<DispatchMain>()),
                runtime_dsp_role: dispatch_layout(std::alloc::Layout::new::<DispatchDsp>()),
                sdk_main_role: dispatch_empty(),
                sdk_dsp_role: dispatch_empty(),
            };
            let bytes = super::super::protocol::MAX_CONTROL_FRAME_BYTES;
            let profile = crate::NativeWorkerLaneResourceProfile::scoped_main_dsp(
                owners,
                crate::NativeWorkerControlOverlapLayout {
                    decoded_input: crate::NativeHostBufferLayout {
                        bytes,
                        alignment: std::mem::align_of::<f64>(),
                    },
                    reply_storage: crate::NativeHostBufferLayout { bytes, alignment: 1 },
                    staged_seed: crate::NativeHostBufferLayout { bytes, alignment: 1 },
                },
            )
            .unwrap();
            let contract = AudioRenderContract {
                sample_rate: 48000,
                channel_layout: mondrian_core::AudioChannelLayout::Stereo,
                max_block_frames: 1,
                processing_mode: crate::AudioProcessingMode::Offline,
                processor_session_scratch_budget_bytes: 0,
                public_output_lookahead_budget_frames: 0,
                compensation_delay_scratch_budget_bytes: 0,
            };
            let auxiliary = crate::AudioProcessorAuxiliaryInputContract::default();
            let auxiliary_outputs = crate::AudioProcessorAuxiliaryOutputContract::default();
            let notes = crate::AudioProcessorNoteInputContract::default();
            let layout = SharedBlockLayout::new(2, 1, 0, 0).unwrap();
            let context = WorkerProcessContext {
                musical_time_reference: None,
                layout,
                auxiliary_inputs: &auxiliary,
                auxiliary_outputs: &auxiliary_outputs,
                note_input_contract: &notes,
                parameter_ids: &[],
                render_contract: contract,
            };
            let mut storage = vec![0_u64; layout.total_bytes.div_ceil(size_of::<u64>())];
            let shared = bytemuck::cast_slice_mut(&mut storage);
            std::thread::scope(|scope| {
                let child = scope.spawn(|| {
                    with_native_ingress(&mut child, 0, Some(profile), |control, ingress| {
                        let split = super::super::IsolatedAudioProcessorWorkerSplit {
                            main: Box::new(DispatchMain { binding, eligible, checks: checks.clone(), finished: finished.clone() }),
                            processing: Box::new(DispatchDsp { calls: calls.clone(), pending: pending.clone() }),
                        };
                        let result = with_processing_scope(split, profile, shared, context,
                            |main, dsp| service_native_main_scope(main, Some(dsp), ingress, control))?;
                        assert!(matches!(result.value, NativeScopeExit::OwnerCommand(NativeCommand::PollObservations(
                            Consumption::DrainAuthor(actual))) if actual == requested));
                        // This is a synthetic owner boundary reply, not native
                        // AwaitingA or a successful author delivery/acceptance.
                        WorkerResponse::Failed("test joined owner boundary; no author reply".into()).write_to(control).unwrap();
                        ingress.acknowledge()?;
                        Ok(())
                    }).unwrap();
                });
                command(CommandKind::PollNativeObservations, 0).write_to(&mut parent).unwrap();
                write_observation_request(&mut parent, Consumption::DrainAuthor(requested))
                    .unwrap();
                assert_eq!(
                    WorkerResponse::read_from(&mut parent).unwrap(),
                    WorkerResponse::Failed("test joined owner boundary; no author reply".into())
                );
                child.join().unwrap();
            });
            let expected = usize::from(eligible && matching);
            assert_eq!(checks.load(Ordering::Relaxed), 1);
            assert_eq!(calls.load(Ordering::Relaxed), expected);
            assert_eq!(
                finished.load(Ordering::Relaxed),
                1,
                "the actual DSP thread joined"
            );
            assert_eq!(
                *pending.lock().unwrap(),
                if expected == 0 { vec![0.875] } else { vec![] }
            );
        }
    }
}

/// Synthetic ABI control: real ingress, DSP thread join and the original owner
/// dispatcher. It does not substitute for the separate owned COM query gate.
#[test]
fn explicit_parameter_query_returns_to_joined_full_owner_without_flush_process_or_ack() {
    use super::super::native_audio_parameter_query_wire::{self as wire, QueryReply};
    use super::super::native_worker_command::NativeCommand;
    use super::super::native_worker_ingress::{
        with_native_ingress, NativeCommandIngress, NativeReaderSlots,
    };
    use super::super::native_worker_processing::{
        with_processing_scope, NativeDspDispatch, NativeProcessingSlots,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };
    struct JoinedQuery {
        binding: crate::NativeAudioObjectBindRequest,
        finished: Arc<AtomicUsize>,
        flushes: Arc<AtomicUsize>,
        queries: usize,
    }
    impl IsolatedAudioProcessorWorker for JoinedQuery {
        fn query_native_parameter(
            &mut self,
            binding: crate::NativeAudioObjectBindRequest,
            id: u32,
        ) -> Result<crate::NativeAudioParameterQueryResult, AudioProcessorHostError> {
            assert_eq!(
                self.finished.load(Ordering::Acquire),
                1,
                "real scope finished after DSP join before this full-owner getter"
            );
            assert_eq!(
                self.flushes.load(Ordering::Acquire),
                0,
                "query cannot use author flush"
            );
            assert_eq!(binding, self.binding);
            assert_eq!(id, 4095);
            self.queries += 1;
            Ok(Err(
                mondrian_core::NativeAudioParameterQueryRefusal::ParameterUnavailable,
            ))
        }
        fn execution_contract(&self) -> crate::AudioProcessorExecutionContract {
            crate::AudioProcessorExecutionContract::stateless()
        }
        fn auxiliary_input_contract(&self) -> crate::AudioProcessorAuxiliaryInputContract {
            Default::default()
        }
        fn enter_state(&mut self, _: i64) -> Result<(), AudioProcessorHostError> {
            panic!("query is not state entry")
        }
        fn process(
            &mut self,
            _: IsolatedAudioProcessorWorkerBlock<'_>,
        ) -> Result<(), AudioProcessorHostError> {
            panic!("query is not PCM")
        }
        fn poll_native_observations(
            &mut self,
            _: crate::NativeAudioObservationConsumption,
        ) -> Result<crate::NativeAudioObservations, AudioProcessorHostError> {
            panic!("query cannot Drain or Ack")
        }
    }
    let binding = crate::NativeAudioObjectBindRequest {
        binding: mondrian_core::AudioProcessorBindingId::new(),
        expected_object: crate::NativeAudioObjectObservation::new(
            std::num::NonZeroU32::new(std::process::id()).unwrap(),
            std::num::NonZeroU64::new(29).unwrap(),
        ),
    };
    let flushes = Arc::new(AtomicUsize::new(0));
    let checks = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicUsize::new(0));
    let pending = Arc::new(Mutex::new(vec![0.875]));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut parent = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut child, _) = listener.accept().unwrap();
    for socket in [&parent, &child] {
        socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        socket.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
    }
    let owners = crate::NativeWorkerLaneOwnedLayouts {
        adapter_resident: dispatch_empty(),
        reader_slots: dispatch_layout(std::alloc::Layout::new::<NativeReaderSlots>()),
        processing_slots: dispatch_layout(std::alloc::Layout::new::<NativeProcessingSlots>()),
        main_inline: dispatch_layout(std::alloc::Layout::new::<(
            NativeCommandIngress<'static>,
            WorkerProcessContext<'static>,
            NativeDspDispatch<'static>,
        )>()),
        runtime_main_role: dispatch_layout(std::alloc::Layout::new::<DispatchMain>()),
        runtime_dsp_role: dispatch_layout(std::alloc::Layout::new::<DispatchDsp>()),
        sdk_main_role: dispatch_empty(),
        sdk_dsp_role: dispatch_empty(),
    };
    let bytes = super::super::protocol::MAX_CONTROL_FRAME_BYTES;
    let profile = crate::NativeWorkerLaneResourceProfile::scoped_main_dsp(
        owners,
        crate::NativeWorkerControlOverlapLayout {
            decoded_input: crate::NativeHostBufferLayout { bytes, alignment: align_of::<f64>() },
            reply_storage: crate::NativeHostBufferLayout { bytes, alignment: 1 },
            staged_seed: crate::NativeHostBufferLayout { bytes, alignment: 1 },
        },
    )
    .unwrap();
    let render_contract = AudioRenderContract {
        sample_rate: 48000,
        channel_layout: mondrian_core::AudioChannelLayout::Stereo,
        max_block_frames: 1,
        processing_mode: crate::AudioProcessingMode::Offline,
        processor_session_scratch_budget_bytes: 0,
        public_output_lookahead_budget_frames: 0,
        compensation_delay_scratch_budget_bytes: 0,
    };
    let auxiliary = crate::AudioProcessorAuxiliaryInputContract::default();
    let outputs = crate::AudioProcessorAuxiliaryOutputContract::default();
    let notes = crate::AudioProcessorNoteInputContract::default();
    let layout = SharedBlockLayout::new(2, 1, 0, 0).unwrap();
    let context = WorkerProcessContext {
        musical_time_reference: None,
        layout,
        auxiliary_inputs: &auxiliary,
        auxiliary_outputs: &outputs,
        note_input_contract: &notes,
        parameter_ids: &[],
        render_contract,
    };
    let mut storage = vec![0_u64; layout.total_bytes.div_ceil(size_of::<u64>())];
    let shared = bytemuck::cast_slice_mut(&mut storage);
    std::thread::scope(|scope| {
        let child = scope.spawn(|| {
            with_native_ingress(&mut child, 0, Some(profile), |control, ingress| {
                let split = super::super::IsolatedAudioProcessorWorkerSplit {
                    main: Box::new(DispatchMain {
                        binding,
                        eligible: true,
                        checks: checks.clone(),
                        finished: finished.clone(),
                    }),
                    processing: Box::new(DispatchDsp {
                        calls: flushes.clone(),
                        pending: pending.clone(),
                    }),
                };
                let result =
                    with_processing_scope(split, profile, shared, context, |main, dsp| {
                        service_native_main_scope(main, Some(dsp), ingress, control)
                    })?;
                let NativeScopeExit::OwnerCommand(actual @ NativeCommand::QueryParameter(_, _)) =
                    result.value
                else {
                    panic!("original query owner handoff");
                };
                assert_eq!(
                    checks.load(Ordering::Acquire),
                    0,
                    "no MainRole getter or author binding probe"
                );
                let mut owner = JoinedQuery {
                    binding,
                    finished: finished.clone(),
                    flushes: flushes.clone(),
                    queries: 0,
                };
                assert!(dispatch_native_owner_command(
                    &mut owner, actual, control, shared, context
                )?);
                assert_eq!(owner.queries, 1);
                ingress.acknowledge()?;
                Ok(())
            })
            .unwrap();
        });
        command(CommandKind::QueryParameter, 0).write_to(&mut parent).unwrap();
        wire::write_request(&mut parent, binding, 4095).unwrap();
        assert!(matches!(
            wire::read_reply(&mut parent, binding, 4095).unwrap(),
            QueryReply::Outcome(Err(
                mondrian_core::NativeAudioParameterQueryRefusal::ParameterUnavailable
            ))
        ));
        child.join().unwrap();
    });
    assert_eq!(finished.load(Ordering::Acquire), 1);
    assert_eq!(flushes.load(Ordering::Acquire), 0);
    assert_eq!(
        *pending.lock().unwrap(),
        [0.875],
        "query preserved pending author input"
    );
}
