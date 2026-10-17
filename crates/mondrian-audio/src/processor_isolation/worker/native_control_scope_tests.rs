//! Real socket/ingress controls for the existing main service with no DSP role.
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
