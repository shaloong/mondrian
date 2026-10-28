use super::protocol::{
    auxiliary_from_wire, auxiliary_to_wire, mode_is_realtime, read_json_frame, EncodedPrepare,
    ExecutionContractWire, PrepareWire, ReadyResultWire, ReadyWire, SharedBlockLayout,
    WorkerCommand, WorkerResponse, PROTOCOL_VERSION,
};
use super::{IsolatedAudioProcessorWorkerSpec, ISOLATED_AUDIO_PROCESSOR_ENDPOINT_ENV};
use crate::{AudioProcessorHostError, AudioRenderContract};
use memmap2::{MmapMut, MmapOptions};
use sha2::Digest;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::{NamedTempFile, TempDir};
use uuid::Uuid;

mod exit_wait;
mod shutdown_observation;
use shutdown_observation::{
    response_tag, ChildObservation, CleanupObservation, ShutdownFailure, ShutdownReport, Timed,
};

// Force termination is asynchronous on Windows. This is a separate resource
// settlement wait, not an extension of the original graceful shutdown deadline.
const TERMINATED_CHILD_WAIT_BUDGET: Duration = Duration::from_secs(2);

// Background audio and discovery workers run independently of a caller console.
// Their private launch sites use NUL stdio and IPC, with no competing explicit flags.
#[cfg(windows)]
pub(super) fn configure_background_worker_start(command: &mut Command) -> u32 {
    use std::os::windows::process::CommandExt;

    // WinBase.h DETACHED_PROCESS; std also adds CREATE_UNICODE_ENVIRONMENT.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    command.creation_flags(DETACHED_PROCESS);
    DETACHED_PROCESS
}

pub(super) struct SupervisedAudioWorker {
    process: ProcessOwner,
    control: TcpStream,
    shared: MmapMut,
    _shared_file: NamedTempFile,
    layout: SharedBlockLayout,
    render_contract: AudioRenderContract,
    staged_audio_input_mapping:
        Option<Box<super::clap_audio_input_mapping::StagedClapInputMapping>>,
    operation_timeout: Duration,
    state_entry_timeout: Duration,
    configured_control_timeout: Option<Duration>,
    #[cfg(test)]
    before_drop_test_hook: Option<BeforeDropTestHook>,
    #[cfg(test)]
    discard_seed_commit_reply: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

#[cfg(test)]
type BeforeDropTestHook = Box<dyn FnOnce(&mut SupervisedAudioWorker) + Send + Sync>;

#[cfg(all(test, windows))]
mod native_playback_closure_tests;

// This is private OS ownership, not a second processor/domain lifecycle.
// Retiring revokes submissions without claiming that the retained Child exited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessState {
    Running,
    Retiring,
    Settled,
}

pub(super) struct ProcessOwner {
    child: Child,
    state: ProcessState,
    staging: Option<TempDir>,
    #[cfg(test)]
    cleanup_hooks: Option<CleanupHooks>,
    settlement_producer: Option<mondrian_core::NativeChildResultProducer>,
}

// Known new parent inline slot, charged once through the original Session contract.
// This is not a complete ProcessOwner/std/foreign-heap accounting claim.
pub(super) fn child_settlement_slot_bytes() -> usize {
    std::alloc::Layout::new::<Option<mondrian_core::NativeChildResultProducer>>().size()
}

#[cfg(test)]
type CleanupHooks = (
    fn(&mut Child) -> io::Result<Option<std::process::ExitStatus>>,
    fn(&mut Child) -> io::Result<()>,
    fn(&mut Child) -> io::Result<std::process::ExitStatus>,
);

impl ProcessOwner {
    pub(super) fn bind_native_child_settlement(
        &mut self,
        registrar: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<(), mondrian_core::NativeChildSettlementError> {
        if let Some(producer) = &self.settlement_producer {
            if producer.same_domain(registrar) {
                return Ok(());
            }
            producer.mark_unavailable();
            registrar.mark_unavailable();
            return Err(mondrian_core::NativeChildSettlementError::ForeignDomain);
        }
        self.settlement_producer = Some(registrar.try_child()?);
        Ok(())
    }

    pub(super) fn new(child: Child) -> Self {
        Self {
            child,
            state: ProcessState::Running,
            staging: None,
            #[cfg(test)]
            cleanup_hooks: None,
            settlement_producer: None,
        }
    }

    pub(super) fn with_staging(child: Child, staging: TempDir) -> Self {
        Self {
            child,
            state: ProcessState::Running,
            staging: Some(staging),
            #[cfg(test)]
            cleanup_hooks: None,
            settlement_producer: None,
        }
    }

    pub(super) fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        let observed = self.child.try_wait()?;
        if observed.is_some() {
            self.state = ProcessState::Settled;
        }
        Ok(observed)
    }

    fn terminate(&mut self) -> Result<(), AudioProcessorHostError> {
        self.terminate_observed(None)
    }

    fn terminate_observed(
        &mut self,
        observation: Option<&mut CleanupObservation>,
    ) -> Result<(), AudioProcessorHostError> {
        #[cfg(test)]
        if let Some((observe, kill, wait)) = self.cleanup_hooks {
            return self.terminate_with_observation(observe, kill, wait, observation);
        }
        self.terminate_with_observation(
            Child::try_wait,
            Child::kill,
            Self::wait_after_kill,
            observation,
        )
    }

    fn wait_after_kill(child: &mut Child) -> io::Result<std::process::ExitStatus> {
        let deadline = Instant::now() + TERMINATED_CHILD_WAIT_BUDGET;
        // Preserve Child::wait's stdin release before observing termination.
        drop(child.stdin.take());
        exit_wait::wait_for_termination(child, deadline)
    }

    #[cfg(all(test, windows))]
    fn terminate_with(
        &mut self,
        observe: impl FnMut(&mut Child) -> io::Result<Option<std::process::ExitStatus>>,
        kill: impl FnMut(&mut Child) -> io::Result<()>,
        wait: impl FnMut(&mut Child) -> io::Result<std::process::ExitStatus>,
    ) -> Result<(), AudioProcessorHostError> {
        self.terminate_with_observation(observe, kill, wait, None)
    }

    fn terminate_with_observation(
        &mut self,
        mut observe: impl FnMut(&mut Child) -> io::Result<Option<std::process::ExitStatus>>,
        mut kill: impl FnMut(&mut Child) -> io::Result<()>,
        mut wait: impl FnMut(&mut Child) -> io::Result<std::process::ExitStatus>,
        mut observation: Option<&mut CleanupObservation>,
    ) -> Result<(), AudioProcessorHostError> {
        if self.state == ProcessState::Settled {
            return Ok(());
        }
        self.state = ProcessState::Retiring;
        let started = observation.as_ref().map(|_| Instant::now());
        let observed = observe(&mut self.child);
        if let (Some(report), Some(started)) = (&mut observation, started) {
            report.initial_observe = Some(Timed::new(report.origin, started, (&observed).into()));
        }
        let observation_error = match observed {
            Ok(Some(_)) => {
                self.state = ProcessState::Settled;
                return Ok(());
            }
            Ok(None) => None,
            Err(error) => Some(format!(
                "failed (kind={:?}, raw_os={:?}): {error}",
                error.kind(),
                error.raw_os_error()
            )),
        };
        let started = observation.as_ref().map(|_| Instant::now());
        let killed = kill(&mut self.child);
        if let (Some(report), Some(started)) = (&mut observation, started) {
            report.kill = Some(Timed::new(
                report.origin,
                started,
                killed.as_ref().map(|_| ()).map_err(Into::into),
            ));
        }
        if let Err(error) = killed {
            let started = observation.as_ref().map(|_| Instant::now());
            let observed = observe(&mut self.child);
            if let (Some(report), Some(started)) = (&mut observation, started) {
                report.final_observe = Some(Timed::new(report.origin, started, (&observed).into()));
            }
            let final_observation = match observed {
                Ok(Some(_)) => {
                    self.state = ProcessState::Settled;
                    if let Some(error) = observation_error.as_deref() {
                        tracing::warn!(
                            process_id = self.child.id(),
                            observation_error = error,
                            "Audio child observation failed before confirmed exit"
                        );
                    }
                    return Ok(());
                }
                Ok(None) => "post-kill try_wait: child still running".to_owned(),
                Err(error) => format!(
                    "post-kill try_wait failed (kind={:?}, raw_os={:?}): {error}",
                    error.kind(),
                    error.raw_os_error()
                ),
            };
            return Err(worker_failed_detail("native child cleanup", &format!(
                "termination unconfirmed (pid={}): kill failed (kind={:?}, raw_os={:?}): {error}; {final_observation}; initial try_wait={}",
                self.child.id(), error.kind(), error.raw_os_error(),
                observation_error.as_deref().unwrap_or("child still running")
            )));
        }
        let started = observation.as_ref().map(|_| Instant::now());
        let waited = wait(&mut self.child);
        if let (Some(report), Some(started)) = (&mut observation, started) {
            report.wait = Some(Timed::new(
                report.origin,
                started,
                waited.as_ref().copied().map_err(Into::into),
            ));
        }
        match waited {
            Ok(_) => {
                self.state = ProcessState::Settled;
                if let Some(error) = observation_error.as_deref() {
                    tracing::warn!(
                        process_id = self.child.id(),
                        observation_error = error,
                        "Audio child observation failed before confirmed reap"
                    );
                }
                Ok(())
            }
            Err(error) => {
                Err(worker_failed_detail(
                    "native child cleanup",
                    &format!(
                "reap failed (pid={}, kind={:?}, raw_os={:?}): {error}; initial try_wait={}",
                self.child.id(), error.kind(), error.raw_os_error(),
                observation_error.as_deref().unwrap_or("child still running")
            ),
                ))
            }
        }
    }

    // Only rare cleanup/control failure paths report here. The existing logger
    // may allocate/block; this is not a realtime-safe failure-path guarantee.
    // Preserve the caller's original typed error rather than replacing it.
    pub(super) fn terminate_reported(&mut self, context: &'static str) {
        self.terminate_reported_observed(context, None);
    }

    fn terminate_reported_observed(
        &mut self,
        context: &'static str,
        observation: Option<&mut CleanupObservation>,
    ) {
        if let Err(error) = self.terminate_observed(observation) {
            tracing::warn!(process_id = self.child.id(), state = ?self.state, context, error = %error,
                "Audio native child cleanup failed");
        }
    }
}

impl Drop for ProcessOwner {
    fn drop(&mut self) {
        let result = self.terminate();
        // A permanently live child may still use these files. Take and keep
        // the directory before any diagnostic that could panic. A kept path
        // is an explicit residual resource, never evidence of settlement.
        let retained = if self.state != ProcessState::Settled {
            self.staging.take().map(TempDir::keep)
        } else {
            None
        };
        if let Some(producer) = self.settlement_producer.take() {
            producer.finish(if self.state == ProcessState::Settled {
                mondrian_core::NativeChildSettlement::Settled
            } else {
                mondrian_core::NativeChildSettlement::Unconfirmed
            });
        }
        if let Err(error) = result {
            tracing::warn!(process_id = self.child.id(), state = ?self.state, context = "audio process Drop", error = %error,
                "Audio native child cleanup failed");
        }
        if let Some(path) = retained {
            tracing::warn!(process_id = self.child.id(), staging = %path.display(),
                "Native discovery staging retained because termination is unconfirmed");
        }
    }
}

impl SupervisedAudioWorker {
    pub(super) fn bind_native_child_settlement(
        &mut self,
        registrar: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<(), mondrian_core::NativeChildSettlementError> {
        self.process.bind_native_child_settlement(registrar)
    }

    pub(super) fn launch(
        spec: &IsolatedAudioProcessorWorkerSpec,
        contract: AudioRenderContract,
        layout: SharedBlockLayout,
        expected_input_names: Option<&[crate::AudioProcessorInputName]>,
        native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    ) -> Result<(Self, Vec<crate::AudioProcessorInputName>), AudioProcessorHostError> {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .map_err(|error| worker_failed("startup", error))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| worker_failed("startup", error))?;
        let endpoint = listener.local_addr().map_err(|error| worker_failed("startup", error))?;
        let nonce = Uuid::new_v4().to_string();

        let shared_file = NamedTempFile::new().map_err(|error| worker_failed("startup", error))?;
        shared_file
            .as_file()
            .set_len(
                u64::try_from(layout.total_bytes)
                    .map_err(|_| invalid_contract("shared storage is not file-representable"))?,
            )
            .map_err(|error| worker_failed("startup", error))?;
        // SAFETY: the retained temporary file has just been sized to the exact
        // immutable layout extent. Both processes validate that extent before
        // producing typed views. The parent attempts child settlement before
        // releasing storage; permanent OS refusal is diagnosed, never claimed
        // as a successful reap. The child owns its independently validated map.
        let shared =
            unsafe { MmapOptions::new().len(layout.total_bytes).map_mut(shared_file.as_file()) }
                .map_err(|error| worker_failed("startup", error))?;

        let mut command = Command::new(&spec.helper_executable);
        command.args(&spec.helper_arguments);
        if let Some(argument) = &spec.dispatch_argument {
            command.arg(argument);
        }
        command
            .env(ISOLATED_AUDIO_PROCESSOR_ENDPOINT_ENV, endpoint.to_string())
            .env(super::ISOLATED_AUDIO_PROCESSOR_NONCE_ENV, &nonce)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        configure_background_worker_start(&mut command);
        // Preflight the actual nonce, path and complete metadata before any
        // child exists. Only encoded metadata and a borrow of the retained
        // opaque payload survive this block; no payload clone is made.
        let encoded_prepare = {
            let prepare = PrepareWire {
                musical_time_reference: spec.musical_time_reference,
                schema_version: PROTOCOL_VERSION,
                native_lane: super::native_worker_lane_wire::NativeWorkerLaneWire::capture(
                    spec.native_lane,
                )?,
                nonce,
                payload_bytes: u64::try_from(spec.preparation_payload.len()).map_err(|_| {
                    invalid_contract("preparation payload extent is not protocol-representable")
                })?,
                execution_contract: ExecutionContractWire::capture(spec.plugin_execution_contract)?,
                auxiliary_buses: auxiliary_to_wire(&spec.auxiliary_inputs),
                auxiliary_outputs: super::protocol::outputs_to_wire(&spec.auxiliary_outputs),
                note_input_contract: spec.note_input_contract.clone(),
                parameter_ids: spec.parameter_ids.clone(),
                sample_rate: contract.sample_rate,
                channel_layout: contract.channel_layout,
                max_block_frames: u64::try_from(contract.max_block_frames).map_err(|_| {
                    invalid_contract("maximum block extent is not protocol-representable")
                })?,
                realtime: mode_is_realtime(contract.processing_mode),
                shared_path: shared_file.path().to_string_lossy().into_owned(),
                shared_bytes: u64::try_from(layout.total_bytes)
                    .map_err(|_| invalid_contract("shared extent is not protocol-representable"))?,
            };
            EncodedPrepare::new(&prepare, &spec.preparation_payload)
                .map_err(|error| classify_io("startup handshake", error))?
        };
        let mut process =
            ProcessOwner::new(command.spawn().map_err(|error| worker_failed("startup", error))?);
        if let Some(registrar) = native_child_settlement {
            // Original final Drop reports every later error using the same owner.
            // Evidence failure never replaces actual startup semantics.
            let _ = process.bind_native_child_settlement(registrar);
        }
        let startup_deadline = Instant::now()
            .checked_add(spec.startup_timeout)
            .ok_or_else(|| invalid_contract("Worker startup deadline overflowed"))?;
        let mut control = match accept_worker(&listener, &mut process.child, startup_deadline) {
            Ok(control) => control,
            Err(error) => {
                process.terminate_reported("audio startup failure");
                return Err(error);
            }
        };
        let remaining = startup_deadline.saturating_duration_since(Instant::now());
        if let Err(error) = set_stream_timeout(&control, remaining.max(Duration::from_millis(1))) {
            process.terminate_reported("audio startup failure");
            return Err(worker_failed("startup", error));
        }
        let ready = (|| -> io::Result<ReadyWire> {
            encoded_prepare.write_to(&mut control)?;
            read_json_frame(&mut control)
        })();
        let ready = match ready {
            Ok(ready) => ready,
            Err(error) => {
                process.terminate_reported("audio startup failure");
                return Err(classify_io("startup handshake", error));
            }
        };
        if ready.schema_version != PROTOCOL_VERSION {
            process.terminate_reported("audio startup failure");
            return Err(worker_failed_detail(
                "startup",
                "Worker replied with another protocol version",
            ));
        }
        let input_names = match ready.result {
            ReadyResultWire::Ready {
                musical_time_reference,
                execution_contract,
                auxiliary_buses,
                auxiliary_outputs,
                note_input_contract,
                input_names,
            } => {
                let worker_contract = match execution_contract.realize() {
                    Ok(contract) => contract,
                    Err(error) => {
                        process.terminate_reported("audio startup failure");
                        return Err(error);
                    }
                };
                let worker_auxiliary = auxiliary_from_wire(auxiliary_buses);
                if musical_time_reference != spec.musical_time_reference
                    || worker_contract != spec.plugin_execution_contract
                    || worker_auxiliary != spec.auxiliary_inputs
                    || super::protocol::outputs_from_wire(auxiliary_outputs)
                        != spec.auxiliary_outputs
                    || note_input_contract != spec.note_input_contract
                {
                    process.terminate_reported("audio startup failure");
                    return Err(worker_failed_detail(
                        "startup",
                        "Worker preparation contract drifted from the admitted parent contract",
                    ));
                }
                if let Err(error) =
                    crate::AudioProcessorInputName::validate_all(&input_names, &worker_auxiliary)
                {
                    process.terminate_reported("audio startup failure");
                    return Err(error);
                }
                // Names are keyed presentation metadata. Both lists are already
                // unique/validated; order carries no identity and needs no cache.
                if expected_input_names.is_some_and(|expected| {
                    expected.len() != input_names.len()
                        || expected
                            .iter()
                            .any(|name| !input_names.iter().any(|actual| actual == name))
                }) {
                    process.terminate_reported("audio startup failure");
                    return Err(worker_failed_detail(
                        "startup",
                        "Worker input presentation metadata changed since preparation",
                    ));
                }
                input_names
            }
            ReadyResultWire::Rejected { detail } => {
                process.terminate_reported("audio startup failure");
                return Err(worker_failed_detail("startup", &detail));
            }
        };
        if let Err(error) = set_stream_timeout(&control, spec.operation_timeout) {
            process.terminate_reported("audio startup failure");
            return Err(worker_failed("startup", error));
        }
        Ok((
            Self {
                process,
                control,
                shared,
                _shared_file: shared_file,
                layout,
                render_contract: contract,
                staged_audio_input_mapping: None,
                operation_timeout: spec.operation_timeout,
                state_entry_timeout: spec.state_entry_timeout,
                configured_control_timeout: Some(spec.operation_timeout),
                #[cfg(test)]
                before_drop_test_hook: None,
                #[cfg(test)]
                discard_seed_commit_reply: None,
            },
            input_names,
        ))
    }

    pub(super) const fn layout(&self) -> SharedBlockLayout {
        self.layout
    }

    pub(super) fn shared_mut(&mut self) -> &mut [u8] {
        &mut self.shared
    }

    pub(super) fn invoke(
        &mut self,
        operation: &'static str,
        command: WorkerCommand,
    ) -> Result<Option<crate::NativeObservedRequest>, AudioProcessorHostError> {
        self.request(operation, command, false).map(|(pending, _)| pending)
    }

    pub(super) fn invoke_process(
        &mut self,
        operation: &'static str,
        command: WorkerCommand,
    ) -> Result<(Option<crate::NativeObservedRequest>, Option<u32>), AudioProcessorHostError> {
        if command.kind != super::protocol::CommandKind::Process {
            return Err(invalid_contract(
                "only Process may receive Note completion observations",
            ));
        }
        self.request(operation, command, self.layout.note_event_capacity != 0)
    }

    fn request(
        &mut self,
        operation: &'static str,
        command: WorkerCommand,
        notes_response: bool,
    ) -> Result<(Option<crate::NativeObservedRequest>, Option<u32>), AudioProcessorHostError> {
        if self.process.state != ProcessState::Running {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        let timeout = if command.kind == super::protocol::CommandKind::EnterState {
            self.state_entry_timeout
        } else {
            self.operation_timeout
        };
        if let Err(error) = self.configure_control_timeout(timeout) {
            self.terminate();
            return Err(worker_failed(operation, error));
        }
        let result = (|| -> io::Result<WorkerResponse> {
            command.write_to(&mut self.control)?;
            WorkerResponse::read_from(&mut self.control)
        })();
        match result {
            Ok(WorkerResponse::Completed) if !notes_response => Ok((None, None)),
            Ok(WorkerResponse::CompletedWithPending(request)) if !notes_response => {
                Ok((Some(request), None))
            }
            Ok(WorkerResponse::NotesCompleted { note_end_count, pending })
                if notes_response && note_end_count as usize <= self.layout.note_end_capacity =>
            {
                Ok((pending, Some(note_end_count)))
            }
            Ok(
                WorkerResponse::Completed
                | WorkerResponse::CompletedWithPending(_)
                | WorkerResponse::NotesCompleted { .. },
            ) => {
                self.terminate();
                Err(worker_failed_detail(
                    operation,
                    "Worker completion differs from admitted Note operation",
                ))
            }
            Ok(WorkerResponse::DeferredForConfiguration(request)) => {
                Err(AudioProcessorHostError::ReprepareRequired(request))
            }
            Ok(WorkerResponse::Failed(detail)) => {
                self.terminate();
                Err(worker_failed_detail(operation, &detail))
            }
            Err(error) => {
                let classified = self.classify_operation_io(operation, error);
                self.terminate();
                Err(classified)
            }
        }
    }

    fn admit_native_object(
        &mut self,
        object: crate::NativeAudioObjectObservation,
    ) -> Result<(), AudioProcessorHostError> {
        if self.process.state != ProcessState::Running {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        match self.process.child.try_wait() {
            Ok(None) if self.process.child.id() == object.process_id().get() => Ok(()),
            Ok(_) => {
                self.terminate();
                Err(worker_failed_detail(
                    "native object observation",
                    "native object belongs to a retired or different supervised child",
                ))
            }
            Err(error) => {
                self.terminate();
                Err(worker_failed("native object observation", error))
            }
        }
    }

    pub(super) fn observe_native_audio_object(
        &mut self,
    ) -> Result<Option<crate::NativeAudioObjectObservation>, AudioProcessorHostError> {
        // Capability preparation must not consume unrelated diagnostic events.
        self.poll_native_observations(crate::NativeAudioObservationConsumption::Preserve)
            .map(|observations| observations.object)
    }

    pub(super) fn poll_native_observations(
        &mut self,
        consumption: crate::NativeAudioObservationConsumption,
    ) -> Result<crate::NativeAudioObservations, AudioProcessorHostError> {
        self.poll_native_observations_with_timeout(consumption, self.state_entry_timeout)
    }

    pub(super) fn preflight_native_observations(
        &mut self,
    ) -> Result<crate::NativeAudioObservations, AudioProcessorHostError> {
        self.poll_native_observations_with_timeout(
            crate::NativeAudioObservationConsumption::Preserve,
            self.operation_timeout,
        )
    }

    fn poll_native_observations_with_timeout(
        &mut self,
        consumption: crate::NativeAudioObservationConsumption,
        timeout: Duration,
    ) -> Result<crate::NativeAudioObservations, AudioProcessorHostError> {
        let operation = "native observation";
        if self.process.state != ProcessState::Running {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        if let Err(error) = self.configure_control_timeout(timeout) {
            self.terminate();
            return Err(worker_failed(operation, error));
        }
        let result = (|| -> io::Result<_> {
            WorkerCommand {
                transport_activity: None,
                kind: super::protocol::CommandKind::PollNativeObservations,
                start_sample: 0,
                frames: 0,
                event_count: 0,
                note_count: 0,
            }
            .write_to(&mut self.control)?;
            super::native_audio_object_wire::write_observation_request(
                &mut self.control,
                consumption,
            )?;
            super::native_audio_object_wire::read_observation_reply(&mut self.control)
        })();
        match result {
            Ok(observations) => {
                if observations.owner_process_id().map(|pid| pid.get())
                    != Some(self.process.child.id())
                {
                    self.terminate();
                    return Err(worker_failed_detail(
                        operation,
                        "observation belongs to a different supervised child",
                    ));
                }
                match self.process.child.try_wait() {
                    Ok(None) => {}
                    Ok(_) => {
                        self.terminate();
                        return Err(worker_failed_detail(
                            operation,
                            "native observation child already retired",
                        ));
                    }
                    Err(error) => {
                        self.terminate();
                        return Err(worker_failed(operation, error));
                    }
                }
                if let Some(object) = observations.object {
                    self.admit_native_object(object)?;
                }
                Ok(observations)
            }
            Err(error) => {
                let error = self.classify_operation_io(operation, error);
                self.terminate();
                Err(error)
            }
        }
    }

    pub(super) fn bind_native_audio_object(
        &mut self,
        request: crate::NativeAudioObjectBindRequest,
    ) -> Result<crate::NativeAudioObjectBindOutcome, AudioProcessorHostError> {
        let operation = "native object binding";
        if self.process.child.id() != request.expected_object.process_id().get() {
            // A stale/foreign requested owner is a typed intent refusal, not
            // evidence that the currently retained child/socket is corrupt.
            return Ok(crate::NativeAudioObjectBindOutcome::Refused(
                crate::NativeAudioObjectBindRefusal::ObjectChanged,
            ));
        }
        self.admit_native_object(request.expected_object)?;
        if let Err(error) = self.configure_control_timeout(self.state_entry_timeout) {
            self.terminate();
            return Err(worker_failed(operation, error));
        }
        let result = (|| -> io::Result<_> {
            WorkerCommand {
                transport_activity: None,
                kind: super::protocol::CommandKind::BindNativeObject,
                start_sample: 0,
                frames: 0,
                event_count: 0,
                note_count: 0,
            }
            .write_to(&mut self.control)?;
            super::native_audio_object_wire::write_bind_request(&mut self.control, request)?;
            super::native_audio_object_wire::read_bind_reply(&mut self.control, request)
        })();
        match result {
            Ok(outcome) => {
                if let crate::NativeAudioObjectBindOutcome::Bound(receipt) = outcome {
                    self.admit_native_object(receipt.object())?;
                }
                Ok(outcome)
            }
            Err(error) => {
                let error = self.classify_operation_io(operation, error);
                self.terminate();
                Err(error)
            }
        }
    }

    pub(super) fn prepare_native_author_state_entry(
        &mut self,
        request: crate::NativeAudioAuthorStateEntryRequest<'_>,
    ) -> Result<crate::NativeAudioAuthorStateEntryOutcome, AudioProcessorHostError> {
        use crate::NativeAudioAuthorStateEntryOutcome as Outcome;
        let operation = "native author state-entry preparation";
        if request.binding.expected_object.process_id().get() != self.process.child.id() {
            return Ok(Outcome::Refused(
                crate::NativeAudioAuthorStateEntryRefusal::OwnerChanged,
            ));
        }
        if self.process.state != ProcessState::Running {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        // A retry intentionally names the OLD object. Only the child's private
        // exact preparation may acknowledge that link; an old object cannot be
        // sent through the ordinary new-object admission shortcut.
        if let Err(error) = self.configure_control_timeout(self.state_entry_timeout) {
            self.terminate();
            return Err(worker_failed(operation, error));
        }
        let result = (|| -> io::Result<_> {
            WorkerCommand {
                transport_activity: None,
                kind: super::protocol::CommandKind::PrepareAuthorStateEntry,
                start_sample: 0,
                frames: 0,
                event_count: 0,
                note_count: 0,
            }
            .write_to(&mut self.control)?;
            super::native_audio_author_state_entry_wire::write_request(
                &mut self.control,
                &request,
            )?;
            super::native_audio_author_state_entry_wire::read_reply(&mut self.control)
        })();
        match result {
            Ok(Outcome::Prepared(receipt)) => {
                let expected_digest: [u8; 32] = sha2::Sha256::digest(request.configuration).into();
                if receipt.source_binding() != request.binding
                    || receipt.source() != request.source
                    || receipt.configuration_bytes() != request.configuration.len()
                    || receipt.configuration_sha256() != expected_digest
                {
                    self.terminate();
                    return Err(worker_failed(
                        operation,
                        "actual preparation differs from requested owner/configuration",
                    ));
                }
                self.admit_native_object(receipt.object())?;
                Ok(Outcome::Prepared(receipt))
            }
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                let error = self.classify_operation_io(operation, error);
                self.terminate();
                Err(error)
            }
        }
    }

    pub(super) fn open_native_audio_editor(
        &mut self,
        request: crate::NativeAudioEditorOpenRequest,
    ) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
        request.owned_layout()?;
        self.editor_control(request.binding(), Some(&request))
    }

    pub(super) fn close_native_audio_editor(
        &mut self,
        binding: crate::NativeAudioObjectBindRequest,
    ) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
        self.editor_control(binding, None)
    }

    fn editor_control(
        &mut self,
        binding: crate::NativeAudioObjectBindRequest,
        open: Option<&crate::NativeAudioEditorOpenRequest>,
    ) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
        use super::native_audio_editor_wire::{self as wire, EditorReply};
        if self.process.state != ProcessState::Running {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        if self.process.child.id() != binding.expected_object.process_id().get() {
            return Ok(crate::NativeAudioEditorOutcome::Refused(
                crate::NativeAudioEditorRefusal::OwnerChanged,
            ));
        }
        self.admit_native_object(binding.expected_object)?;
        let operation = if open.is_some() {
            "native editor open"
        } else {
            "native editor close"
        };
        if let Err(error) = self.configure_control_timeout(self.state_entry_timeout) {
            self.terminate();
            return Err(worker_failed(operation, error));
        }
        let result = (|| -> io::Result<_> {
            WorkerCommand {
                transport_activity: None,
                kind: if open.is_some_and(|request| !request.static_parameter_values().is_empty()) {
                    super::protocol::CommandKind::EditorOpenWithStaticParameters
                } else if open.is_some() {
                    super::protocol::CommandKind::EditorOpen
                } else {
                    super::protocol::CommandKind::EditorClose
                },
                start_sample: 0,
                frames: 0,
                event_count: 0,
                note_count: 0,
            }
            .write_to(&mut self.control)?;
            if let Some(request) = open {
                if request.static_parameter_values().is_empty() {
                    wire::write_open_request(&mut self.control, request)?;
                } else {
                    wire::write_projected_open_request(&mut self.control, request)?;
                }
            } else {
                super::native_audio_object_wire::write_bind_request(&mut self.control, binding)?;
            }
            wire::read_reply(&mut self.control, binding, open.is_some())
        })();
        match result {
            Ok(EditorReply::Outcome(outcome)) => {
                self.admit_native_object(binding.expected_object)?;
                Ok(outcome)
            }
            Ok(EditorReply::Failed(detail)) => {
                self.terminate();
                Err(worker_failed_detail(operation, &detail))
            }
            Err(error) => {
                let classified = self.classify_operation_io(operation, error);
                self.terminate();
                Err(classified)
            }
        }
    }

    pub(super) fn query_native_parameter(
        &mut self,
        binding: crate::NativeAudioObjectBindRequest,
        native_parameter_id: u32,
    ) -> Result<crate::NativeAudioParameterQueryResult, AudioProcessorHostError> {
        use super::native_audio_parameter_query_wire::{self as wire, QueryReply};
        let operation = "native parameter query";
        if self.process.state != ProcessState::Running {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        if self.process.child.id() != binding.expected_object.process_id().get() {
            return Ok(Err(
                mondrian_core::NativeAudioParameterQueryRefusal::OwnerChanged,
            ));
        }
        self.admit_native_object(binding.expected_object)?;
        if let Err(error) = self.configure_control_timeout(self.state_entry_timeout) {
            self.terminate();
            return Err(worker_failed(operation, error));
        }
        let result = (|| -> io::Result<_> {
            WorkerCommand {
                transport_activity: None,
                kind: super::protocol::CommandKind::QueryParameter,
                start_sample: 0,
                frames: 0,
                event_count: 0,
                note_count: 0,
            }
            .write_to(&mut self.control)?;
            wire::write_request(&mut self.control, binding, native_parameter_id)?;
            wire::read_reply(&mut self.control, binding, native_parameter_id)
        })();
        match result {
            Ok(QueryReply::Outcome(outcome)) => {
                self.admit_native_object(binding.expected_object)?;
                Ok(outcome)
            }
            Ok(QueryReply::Failed) => {
                self.terminate();
                Err(worker_failed_detail(
                    operation,
                    "native adapter query failed or panicked",
                ))
            }
            Err(error) => {
                let classified = self.classify_operation_io(operation, error);
                self.terminate();
                Err(classified)
            }
        }
    }
    pub(super) fn query_native_configuration_cohort(
        &mut self,
    ) -> Result<Option<super::NativeRequestCohort>, AudioProcessorHostError> {
        let operation = "configuration cohort query";
        if self.process.state != ProcessState::Running {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        if let Err(error) = self.configure_control_timeout(self.state_entry_timeout) {
            self.terminate();
            return Err(worker_failed(operation, error));
        }
        let result = (|| -> io::Result<_> {
            WorkerCommand {
                transport_activity: None,
                kind: super::protocol::CommandKind::PollConfigurationCohort,
                start_sample: 0,
                frames: 0,
                event_count: 0,
                note_count: 0,
            }
            .write_to(&mut self.control)?;
            super::native_configuration_wire::read_cohort_reply(&mut self.control)
        })();
        match result {
            Ok(cohort) => Ok(cohort),
            Err(error) => {
                let classified = self.classify_operation_io(operation, error);
                self.terminate();
                Err(classified)
            }
        }
    }

    pub(super) fn service_native_configuration(
        &mut self,
        request: super::NativeConfigurationControlRequest<'_>,
    ) -> Result<super::NativeConfigurationControlOutcome, AudioProcessorHostError> {
        use super::native_configuration_wire::{self as wire, ControlReply};
        let operation = "configuration control";
        if self.process.state != ProcessState::Running {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        wire::validate_request(&request)
            .map_err(|error| AudioProcessorHostError::InvalidContract(error.to_string()))?;
        if let Err(error) = self.configure_control_timeout(self.state_entry_timeout) {
            self.terminate();
            return Err(worker_failed(operation, error));
        }
        let result = (|| -> io::Result<_> {
            WorkerCommand {
                transport_activity: None,
                kind: super::protocol::CommandKind::ServiceConfiguration,
                start_sample: 0,
                frames: 0,
                event_count: 0,
                note_count: 0,
            }
            .write_to(&mut self.control)?;
            wire::write_request(&mut self.control, &request)?;
            wire::read_reply(&mut self.control, &request)
        })();
        match result {
            Ok(ControlReply::Outcome(outcome)) => Ok(outcome),
            Ok(ControlReply::Failed(detail)) => {
                self.terminate();
                Err(worker_failed_detail(operation, &detail))
            }
            Err(error) => {
                let classified = self.classify_operation_io(operation, error);
                self.terminate();
                Err(classified)
            }
        }
    }

    pub(super) fn stage_clap_audio_input_buffers(
        &mut self,
        request: super::ClapAudioInputBufferRequest,
    ) -> Result<super::ClapAudioInputBufferOutcome, AudioProcessorHostError> {
        use super::clap_audio_input_buffers::wire;
        let operation = "CLAP input buffer stage";
        if self.process.state != ProcessState::Running {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        wire::validate_request(request)
            .map_err(|error| AudioProcessorHostError::InvalidContract(error.to_string()))?;
        if let Err(error) = self.configure_control_timeout(self.state_entry_timeout) {
            self.terminate();
            return Err(worker_failed(operation, error));
        }
        let result = (|| -> io::Result<_> {
            WorkerCommand {
                transport_activity: None,
                kind: super::protocol::CommandKind::StageClapAudioInputBuffers,
                start_sample: 0,
                frames: 0,
                event_count: 0,
                note_count: 0,
            }
            .write_to(&mut self.control)?;
            wire::write_request(&mut self.control, request)?;
            wire::read_reply(&mut self.control, request)
        })();
        match result {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                let classified = self.classify_operation_io(operation, error);
                self.terminate();
                Err(classified)
            }
        }
    }

    pub(super) fn stage_clap_audio_input_mapping(
        &mut self,
        request: super::ClapAudioInputMappingRequest,
    ) -> Result<super::ClapAudioInputMappingOutcome, AudioProcessorHostError> {
        use super::clap_audio_input_mapping::{wire, StagedClapInputMapping};
        use super::{
            ClapAudioInputMappingOutcome as Outcome, NativeConfigurationRefusal as Refusal,
        };
        let operation = "CLAP input mapping stage";
        if self.process.state != ProcessState::Running {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        if self.staged_audio_input_mapping.is_some() {
            return Ok(Outcome::Refused(Refusal::AwaitingInstallation));
        }
        // These admitted context facts remain frozen, independently of the
        // caller's proposal. The native owner checks the exact buffer receipt.
        if request.channel_layout != self.render_contract.channel_layout
            || request.buffers.request.max_block_frames != self.render_contract.max_block_frames
            || request.parameter_lane_count.checked_mul(self.render_contract.max_block_frames)
                != Some(self.layout.event_capacity)
            || request.auxiliary_output_count.checked_mul(self.layout.main_samples)
                != Some(self.layout.auxiliary_outputs_samples)
            || self.layout.note_event_capacity != 0
            || self.layout.note_end_capacity != 0
        {
            return Ok(Outcome::Refused(Refusal::MetadataChanged));
        }
        let directory = self
            ._shared_file
            .path()
            .parent()
            .ok_or_else(|| invalid_contract("original shared directory unavailable"))?;
        let staged = StagedClapInputMapping::create_parent(request, directory)?;
        let parent_bytes = staged
            .known_bytes()
            .ok_or_else(|| invalid_contract("input mapping parent extent overflow"))?;
        let path = staged
            .path()
            .and_then(|p| p.to_str())
            .ok_or_else(|| invalid_contract("input mapping path is not UTF-8"))?;
        wire::validate_request(request, path, parent_bytes)
            .map_err(|e| invalid_contract(&e.to_string()))?;
        // Adopt before any command can reach the child. A lost reply never
        // releases these still-live owners or permits an implicit retry.
        self.staged_audio_input_mapping = Some(staged);
        if let Err(error) = self.configure_control_timeout(self.state_entry_timeout) {
            self.terminate();
            return Err(worker_failed(operation, error));
        }
        let result = (|| -> io::Result<_> {
            let staged = self.staged_audio_input_mapping.as_ref().unwrap();
            let path = staged
                .path()
                .and_then(|p| p.to_str())
                .ok_or_else(|| io::Error::other("staged path unavailable"))?;
            WorkerCommand {
                transport_activity: None,
                kind: super::protocol::CommandKind::StageClapAudioInputMapping,
                start_sample: 0,
                frames: 0,
                event_count: 0,
                note_count: 0,
            }
            .write_to(&mut self.control)?;
            wire::write_request(&mut self.control, request, path, parent_bytes)?;
            wire::read_reply(&mut self.control, request, parent_bytes)
        })();
        match result {
            Ok(outcome) => {
                if matches!(outcome, Outcome::Staged(_))
                    && let Err(error) =
                        self.staged_audio_input_mapping.as_mut().unwrap().verify_child_backing()
                {
                    self.terminate();
                    return Err(error);
                }
                Ok(outcome)
            }
            Err(error) => {
                let classified = self.classify_operation_io(operation, error);
                self.terminate();
                Err(classified)
            }
        }
    }

    #[cfg(all(test, windows))]
    pub(super) fn discard_next_seed_commit_reply_for_test(
        &mut self,
        observed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        self.discard_seed_commit_reply = Some(observed);
    }

    pub(super) fn stage_native_configuration_seed(
        &mut self,
        request: super::NativeConfigurationSeedRequest<'_>,
    ) -> Result<super::NativeConfigurationSeedOutcome, AudioProcessorHostError> {
        super::native_configuration_seed_wire::validate_stage(&request)
            .map_err(|error| AudioProcessorHostError::InvalidContract(error.to_string()))?;
        self.seed_control(
            "configuration seed stage",
            super::protocol::CommandKind::StageConfigurationSeed,
            request.ticket,
            request.seed_capacity_grant_bytes,
            Some(&request),
        )
    }

    pub(super) fn commit_native_configuration_seed(
        &mut self,
        staged: super::NativeConfigurationSeedReceipt,
        current_inputs: &super::AudioProcessorAuxiliaryInputContract,
    ) -> Result<
        (
            super::NativeConfigurationSeedOutcome,
            Option<super::AudioProcessorAuxiliaryInputContract>,
        ),
        AudioProcessorHostError,
    > {
        if self.staged_audio_input_mapping.as_ref().is_some_and(|mapping| {
            !mapping
                .request
                .buffers
                .request
                .proposal
                .preserves_auxiliary_keys(current_inputs)
        }) {
            return Ok((
                super::NativeConfigurationSeedOutcome::Refused(
                    super::NativeConfigurationSeedRefusal::StaleTicket,
                ),
                None,
            ));
        }
        let outcome = self.seed_control(
            "configuration seed commit",
            super::protocol::CommandKind::CommitConfigurationSeed,
            staged.ticket,
            staged.child_seed_capacity_bytes,
            None,
        )?;
        let inputs = if matches!(outcome, super::NativeConfigurationSeedOutcome::Accepted(_)) {
            self.staged_audio_input_mapping.take().map(|staged| {
                let (shared, file, layout, inputs) = staged.into_parent();
                self.shared = shared;
                self._shared_file = file;
                self.layout = layout;
                inputs
            })
        } else {
            None
        };
        Ok((outcome, inputs))
    }

    fn seed_control(
        &mut self,
        operation: &'static str,
        kind: super::protocol::CommandKind,
        ticket: super::NativeConfigurationSeedTicket,
        grant: usize,
        stage: Option<&super::NativeConfigurationSeedRequest<'_>>,
    ) -> Result<super::NativeConfigurationSeedOutcome, AudioProcessorHostError> {
        use super::native_configuration_seed_wire::{self as wire, SeedReply};
        if self.process.state != ProcessState::Running {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        if let Err(error) = self.configure_control_timeout(self.state_entry_timeout) {
            self.terminate();
            return Err(worker_failed(operation, error));
        }
        let result = (|| -> io::Result<_> {
            WorkerCommand {
                transport_activity: None,
                kind,
                start_sample: 0,
                frames: 0,
                event_count: 0,
                note_count: 0,
            }
            .write_to(&mut self.control)?;
            if let Some(request) = stage {
                wire::write_stage(&mut self.control, request)?;
            } else {
                wire::write_commit(&mut self.control, ticket)?;
            }
            let reply = wire::read_reply(&mut self.control, ticket, grant)?;
            #[cfg(test)]
            if matches!(kind, super::protocol::CommandKind::CommitConfigurationSeed)
                && matches!(
                    &reply,
                    SeedReply::Outcome(super::NativeConfigurationSeedOutcome::Accepted(_))
                )
                && let Some(observed) = self.discard_seed_commit_reply.take()
            {
                // Actual authenticated same-ticket Accepted was already read.
                // Model loss to the parent consumer, not physical TCP loss.
                observed.store(true, std::sync::atomic::Ordering::SeqCst);
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "test discarded authenticated seed commit reply",
                ));
            }
            Ok(reply)
        })();
        match result {
            Ok(SeedReply::Outcome(outcome)) => Ok(outcome),
            Ok(SeedReply::Failed(detail)) => {
                self.terminate();
                Err(worker_failed_detail(operation, &detail))
            }
            Err(error) => {
                let classified = self.classify_operation_io(operation, error);
                self.terminate();
                Err(classified)
            }
        }
    }

    pub(super) fn shutdown(&mut self) -> bool {
        self.shutdown_observed().successful()
    }

    pub(super) fn shutdown_observed(&mut self) -> ShutdownReport {
        let mut report = ShutdownReport::new(
            self.process.child.id(),
            self.process.state,
            self.operation_timeout,
        );
        match self.process.state {
            ProcessState::Settled => return report.finish(true, self.process.state),
            ProcessState::Retiring => {
                report.primary_failure = Some(ShutdownFailure::AlreadyRetiring);
                self.process
                    .terminate_reported_observed("audio shutdown retry", Some(&mut report.cleanup));
                // Cleanup without a fresh ACK is not graceful validation.
                return report.finish(false, self.process.state);
            }
            ProcessState::Running => {}
        }
        self.process.state = ProcessState::Retiring;
        let timeout = self.operation_timeout.min(Duration::from_millis(100));
        let started = Instant::now();
        let configured = self.configure_control_timeout(timeout.max(Duration::from_millis(1)));
        report.configure = Some(Timed::new(
            report.started,
            started,
            configured.as_ref().map(|_| ()).map_err(Into::into),
        ));
        if configured.is_err() {
            report.primary_failure = Some(ShutdownFailure::ConfigureControlTimeout);
            self.process
                .terminate_reported_observed("audio worker failure", Some(&mut report.cleanup));
            return report.finish(false, self.process.state);
        }
        let started = Instant::now();
        let written = WorkerCommand {
            transport_activity: None,
            kind: super::protocol::CommandKind::Shutdown,
            start_sample: 0,
            frames: 0,
            event_count: 0,
            note_count: 0,
        }
        .write_to(&mut self.control);
        report.command_write = Some(Timed::new(
            report.started,
            started,
            written.as_ref().map(|_| ()).map_err(Into::into),
        ));
        let completed = match written {
            Err(_) => {
                report.primary_failure = Some(ShutdownFailure::CommandWrite);
                false
            }
            Ok(()) => {
                let started = Instant::now();
                let response = WorkerResponse::read_from(&mut self.control);
                report.response_read = Some(Timed::new(
                    report.started,
                    started,
                    response.as_ref().map(response_tag).map_err(Into::into),
                ));
                match response {
                    Ok(WorkerResponse::Completed) => true,
                    Ok(_) => {
                        report.primary_failure = Some(ShutdownFailure::NonCompletedResponse);
                        false
                    }
                    Err(_) => {
                        report.primary_failure = Some(ShutdownFailure::ResponseRead);
                        false
                    }
                }
            }
        };
        if completed {
            let deadline = Instant::now() + timeout;
            let started = Instant::now();
            match exit_wait::wait_for_exit(&mut self.process.child, deadline) {
                exit_wait::ExitWaitOutcome::Exited(status) => {
                    report.exit_loop = Some(Timed::new(
                        report.started,
                        started,
                        ChildObservation::Exited(status),
                    ));
                    self.process.state = ProcessState::Settled;
                    if !status.success() {
                        report.primary_failure = Some(ShutdownFailure::NonzeroExit);
                    }
                    return report.finish(status.success(), self.process.state);
                }
                exit_wait::ExitWaitOutcome::Deadline(last_observation) => {
                    report.exit_loop = last_observation
                        .map(|observed| Timed::new(report.started, started, observed));
                    report.primary_failure = Some(ShutdownFailure::ExitObservationDeadline);
                }
                exit_wait::ExitWaitOutcome::Io(error) => {
                    report.exit_loop = Some(Timed::new(
                        report.started,
                        started,
                        ChildObservation::Io(error),
                    ));
                    report.primary_failure = Some(ShutdownFailure::ExitObservationIo);
                }
            }
        }
        self.process
            .terminate_reported_observed("audio shutdown failure", Some(&mut report.cleanup));
        report.finish(false, self.process.state)
    }

    fn configure_control_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        configure_control_timeout(
            &mut self.configured_control_timeout,
            timeout,
            |timeout| self.control.set_read_timeout(Some(timeout)),
            |timeout| self.control.set_write_timeout(Some(timeout)),
        )
    }

    fn terminate(&mut self) {
        self.process.terminate_reported("audio worker failure");
    }

    fn classify_operation_io(
        &mut self,
        operation: &'static str,
        error: io::Error,
    ) -> AudioProcessorHostError {
        if matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ) {
            match self.process.child.try_wait() {
                Ok(Some(status)) => worker_failed_detail(
                    operation,
                    &format!("Worker exited before replying ({status})"),
                ),
                Ok(None) => AudioProcessorHostError::WorkerDeadlineExceeded { operation },
                Err(observation_error) => worker_failed(operation, observation_error),
            }
        } else {
            worker_failed(operation, error)
        }
    }
}

impl Drop for SupervisedAudioWorker {
    fn drop(&mut self) {
        #[cfg(test)]
        if let Some(hook) = self.before_drop_test_hook.take() {
            hook(self);
        }
        let _ = self.shutdown();
    }
}

fn accept_worker(
    listener: &TcpListener,
    child: &mut Child,
    deadline: Instant,
) -> Result<TcpStream, AudioProcessorHostError> {
    loop {
        match listener.accept() {
            Ok((stream, address)) if address.ip().is_loopback() => {
                stream.set_nonblocking(false).map_err(|error| worker_failed("startup", error))?;
                stream.set_nodelay(true).map_err(|error| worker_failed("startup", error))?;
                return Ok(stream);
            }
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(worker_failed("startup", error)),
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(worker_failed_detail(
                    "startup",
                    &format!("Worker exited before connecting ({status})"),
                ));
            }
            Ok(None) => {}
            Err(error) => return Err(worker_failed("startup", error)),
        }
        if Instant::now() >= deadline {
            return Err(AudioProcessorHostError::WorkerDeadlineExceeded {
                operation: "startup connection",
            });
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn set_stream_timeout(stream: &TcpStream, timeout: Duration) -> io::Result<()> {
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))
}

// This state describes successfully installed read AND write budgets, not the
// most recent request. A partial failure makes the actual socket configuration
// unknown until both setters succeed again. The worker closes on that failure.
fn configure_control_timeout(
    configured: &mut Option<Duration>,
    requested: Duration,
    set_read: impl FnOnce(Duration) -> io::Result<()>,
    set_write: impl FnOnce(Duration) -> io::Result<()>,
) -> io::Result<()> {
    if *configured == Some(requested) {
        return Ok(());
    }
    *configured = None;
    set_read(requested)?;
    set_write(requested)?;
    *configured = Some(requested);
    Ok(())
}

fn classify_io(operation: &'static str, error: io::Error) -> AudioProcessorHostError {
    if matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        AudioProcessorHostError::WorkerDeadlineExceeded { operation }
    } else {
        worker_failed(operation, error)
    }
}

fn invalid_contract(detail: &str) -> AudioProcessorHostError {
    AudioProcessorHostError::InvalidContract(detail.to_owned())
}

fn worker_failed(
    operation: &'static str,
    error: impl std::fmt::Display,
) -> AudioProcessorHostError {
    worker_failed_detail(operation, &error.to_string())
}

fn worker_failed_detail(operation: &'static str, detail: &str) -> AudioProcessorHostError {
    let mut detail = detail.to_owned();
    if detail.len() > super::protocol::MAX_WORKER_DETAIL_BYTES {
        let mut boundary = super::protocol::MAX_WORKER_DETAIL_BYTES;
        while !detail.is_char_boundary(boundary) {
            boundary -= 1;
        }
        detail.truncate(boundary);
    }
    AudioProcessorHostError::WorkerFailed { operation, detail }
}

#[cfg(test)]
mod control_timeout_qualification {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordedOptions {
        calls: RefCell<Vec<(&'static str, Duration)>>,
    }

    impl RecordedOptions {
        fn apply(
            &self,
            configured: &mut Option<Duration>,
            requested: Duration,
            fail_read: bool,
            fail_write: bool,
        ) -> io::Result<()> {
            configure_control_timeout(
                configured,
                requested,
                |timeout| {
                    self.calls.borrow_mut().push(("read", timeout));
                    if fail_read {
                        Err(io::Error::from(io::ErrorKind::PermissionDenied))
                    } else {
                        Ok(())
                    }
                },
                |timeout| {
                    self.calls.borrow_mut().push(("write", timeout));
                    if fail_write {
                        Err(io::Error::from(io::ErrorKind::ConnectionReset))
                    } else {
                        Ok(())
                    }
                },
            )
        }
    }

    #[test]
    fn native_control_timeout_warmed_process_reuses_both_installed_options() -> io::Result<()> {
        let options = RecordedOptions::default();
        let process = Duration::from_millis(20);
        let mut configured = None;
        options.apply(&mut configured, process, false, false)?;
        for _ in 0..512 {
            options.apply(&mut configured, process, false, false)?;
        }
        assert_eq!(
            &*options.calls.borrow(),
            &[("read", process), ("write", process)]
        );
        assert_eq!(configured, Some(process));
        Ok(())
    }

    #[test]
    fn native_control_timeout_startup_enter_process_shutdown_switches_are_exact() -> io::Result<()>
    {
        let options = RecordedOptions::default();
        let startup = Duration::from_secs(5);
        let process = Duration::from_secs(1);
        let enter = Duration::from_secs(30);
        let shutdown = Duration::from_millis(100);
        let mut configured = None;
        for requested in [
            startup, process, enter, enter, process, process, shutdown, shutdown,
        ] {
            options.apply(&mut configured, requested, false, false)?;
        }
        assert_eq!(
            &*options.calls.borrow(),
            &[
                ("read", startup),
                ("write", startup),
                ("read", process),
                ("write", process),
                ("read", enter),
                ("write", enter),
                ("read", process),
                ("write", process),
                ("read", shutdown),
                ("write", shutdown),
            ],
        );
        assert_eq!(configured, Some(shutdown));
        Ok(())
    }

    #[test]
    fn native_control_timeout_equal_enter_and_process_budgets_need_no_option_change(
    ) -> io::Result<()> {
        let options = RecordedOptions::default();
        let same = Duration::from_millis(50);
        let mut configured = Some(same);
        options.apply(&mut configured, same, true, true)?;
        assert!(options.calls.borrow().is_empty());
        assert_eq!(configured, Some(same));
        Ok(())
    }

    #[test]
    fn native_control_timeout_read_failure_is_not_cached_or_followed_by_write() -> io::Result<()> {
        let options = RecordedOptions::default();
        let old = Duration::from_secs(1);
        let new = Duration::from_secs(30);
        let mut configured = Some(old);
        let failure = options.apply(&mut configured, new, true, false);
        assert!(matches!(failure, Err(error) if error.kind() == io::ErrorKind::PermissionDenied));
        assert_eq!(configured, None);
        options.apply(&mut configured, old, false, false)?;
        assert_eq!(
            &*options.calls.borrow(),
            &[("read", new), ("read", old), ("write", old)]
        );
        assert_eq!(configured, Some(old));
        Ok(())
    }

    #[test]
    fn native_control_timeout_partial_failure_invalidates_previous_budget_and_retries_both(
    ) -> io::Result<()> {
        let options = RecordedOptions::default();
        let old = Duration::from_secs(1);
        let new = Duration::from_millis(100);
        let mut configured = Some(old);
        let failure = options.apply(&mut configured, new, false, true);
        assert!(matches!(failure, Err(error) if error.kind() == io::ErrorKind::ConnectionReset));
        assert_eq!(configured, None);
        options.apply(&mut configured, old, false, false)?;
        options.apply(&mut configured, old, false, false)?;
        assert_eq!(
            &*options.calls.borrow(),
            &[("read", new), ("write", new), ("read", old), ("write", old)],
        );
        assert_eq!(configured, Some(old));
        Ok(())
    }

    #[test]
    fn native_control_timeout_socket_reports_real_budget_after_switch() -> io::Result<()> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let client = TcpStream::connect(listener.local_addr()?)?;
        let (server, _) = listener.accept()?;
        client.set_nodelay(true)?;
        let mut configured = None;
        for timeout in [
            Duration::from_secs(1),
            Duration::from_secs(30),
            Duration::from_secs(1),
        ] {
            configure_control_timeout(
                &mut configured,
                timeout,
                |requested| client.set_read_timeout(Some(requested)),
                |requested| client.set_write_timeout(Some(requested)),
            )?;
            assert_eq!(client.read_timeout()?, Some(timeout));
            assert_eq!(client.write_timeout()?, Some(timeout));
            assert!(client.nodelay()?);
        }
        drop(server);
        Ok(())
    }
}

#[cfg(test)]
mod startup_preflight_tests {
    use super::*;

    #[test]
    fn startup_raw_metadata_overflow_is_refused_before_nonexistent_helper_spawn() {
        let helper =
            std::env::temp_dir().join(format!("mondrian-no-startup-child-{}", Uuid::new_v4()));
        assert!(!helper.exists());
        let render = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: mondrian_core::AudioChannelLayout::Stereo,
            max_block_frames: 4,
            processing_mode: crate::AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: usize::MAX,
            public_output_lookahead_budget_frames: 1024,
            compensation_delay_scratch_budget_bytes: 1024,
        };
        let execution = crate::AudioProcessorExecutionContract::new(
            0,
            crate::AudioProcessorTail::None,
            false,
            false,
            true,
            0,
        )
        .unwrap();
        let mut spec = IsolatedAudioProcessorWorkerSpec::new(
            helper,
            Vec::new(),
            execution,
            crate::AudioProcessorAuxiliaryInputContract::default(),
            Vec::new(),
            render,
        )
        .unwrap();
        // A real metadata field, not an oversized opaque payload. Calling the
        // actual launch seam distinguishes preflight refusal from spawn/NotFound.
        spec.auxiliary_inputs
            .buses
            .push(crate::AudioProcessorAuxiliaryInputBusContract {
                bus_key: "x".repeat(super::super::protocol::MAX_CONTROL_FRAME_BYTES),
                channel_layout: mondrian_core::AudioChannelLayout::Stereo,
            });
        let error = SupervisedAudioWorker::launch(
            &spec,
            render,
            SharedBlockLayout::uniform(4, 2, 1, 0).unwrap(),
            None,
            None,
        )
        .err()
        .expect("preflight must refuse without starting an executable");
        assert!(matches!(error, AudioProcessorHostError::WorkerFailed {
            operation: "startup handshake", detail,
        } if detail.contains("preparation metadata") && detail.contains("protocol bound")));
    }
}

#[cfg(test)]
mod control_timeout_worker_lifecycle {
    use super::*;
    use std::ffi::OsString;
    use std::io::Read;

    fn launch_existing_test_child(
        operation: Duration,
        enter: Duration,
    ) -> Result<SupervisedAudioWorker, Box<dyn std::error::Error>> {
        // Run the existing test fixture's real worker entrypoint in a child.
        // There is no new factory, protocol implementation or DSP reference here.
        let render = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: mondrian_core::AudioChannelLayout::Stereo,
            max_block_frames: 4,
            processing_mode: crate::AudioProcessingMode::Realtime,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 1024,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        let execution = crate::AudioProcessorExecutionContract::new(
            0,
            crate::AudioProcessorTail::None,
            true,
            true,
            true,
            128,
        )?;
        let spec = IsolatedAudioProcessorWorkerSpec::new(
            std::env::current_exe()?,
            vec![0],
            execution,
            crate::AudioProcessorAuxiliaryInputContract::default(),
            Vec::new(),
            render,
        )?
        .with_helper_arguments(vec![
            OsString::from("processor_isolation::tests::isolated_worker_child_entry"),
            OsString::from("--exact"),
            OsString::from("--nocapture"),
        ])
        .with_dispatch_argument(None)
        .with_deadlines(Duration::from_secs(5), operation)?
        .with_state_entry_timeout(enter)?;
        let (worker, names) = SupervisedAudioWorker::launch(
            &spec,
            render,
            SharedBlockLayout::uniform(4, 2, 0, 0)?,
            None,
            None,
        )?;
        assert!(names.is_empty());
        Ok(worker)
    }

    fn command(kind: super::super::protocol::CommandKind, sample: i64) -> WorkerCommand {
        WorkerCommand {
            transport_activity: None,
            kind,
            start_sample: sample,
            frames: if kind == super::super::protocol::CommandKind::Process {
                4
            } else {
                0
            },
            event_count: 0,
            note_count: 0,
        }
    }

    #[test]
    fn native_control_timeout_actual_child_launch_enter_process_shutdown_use_real_budgets(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let operation = Duration::from_secs(1);
        let enter = Duration::from_secs(30);
        let mut worker = launch_existing_test_child(operation, enter)?;
        assert_eq!(worker.configured_control_timeout, Some(operation));
        assert_eq!(worker.control.read_timeout()?, Some(operation));
        assert_eq!(worker.control.write_timeout()?, Some(operation));
        assert!(worker.control.nodelay()?);

        assert_eq!(
            worker.invoke(
                "qualification state entry",
                command(super::super::protocol::CommandKind::EnterState, 0),
            )?,
            None
        );
        assert_eq!(worker.configured_control_timeout, Some(enter));
        assert_eq!(worker.control.read_timeout()?, Some(enter));
        assert_eq!(worker.control.write_timeout()?, Some(enter));
        for block in 0..8 {
            assert_eq!(
                worker.invoke(
                    "qualification process",
                    command(super::super::protocol::CommandKind::Process, block * 4),
                )?,
                None
            );
            assert_eq!(worker.configured_control_timeout, Some(operation));
            assert_eq!(worker.control.read_timeout()?, Some(operation));
            assert_eq!(worker.control.write_timeout()?, Some(operation));
            assert!(worker.control.nodelay()?);
        }
        // Switch back to the long state budget before shutdown, exercising its
        // real transition to the existing shorter control budget.
        assert_eq!(
            worker.invoke(
                "qualification state entry",
                command(super::super::protocol::CommandKind::EnterState, 100),
            )?,
            None
        );
        let _completed_gracefully = worker.shutdown();
        assert_eq!(worker.process.state, ProcessState::Settled);
        assert_eq!(
            worker.configured_control_timeout,
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            worker.control.read_timeout()?,
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            worker.control.write_timeout()?,
            Some(Duration::from_millis(100))
        );
        assert!(worker.process.child.try_wait()?.is_some());
        Ok(())
    }

    #[test]
    fn native_control_timeout_actual_child_equal_budgets_remain_installed_through_shutdown(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let budget = Duration::from_millis(100);
        let mut worker = launch_existing_test_child(budget, budget)?;
        assert_eq!(
            worker.invoke(
                "qualification state entry",
                command(super::super::protocol::CommandKind::EnterState, 0),
            )?,
            None
        );
        assert_eq!(
            worker.invoke(
                "qualification process",
                command(super::super::protocol::CommandKind::Process, 0),
            )?,
            None
        );
        let _completed_gracefully = worker.shutdown();
        assert_eq!(worker.process.state, ProcessState::Settled);
        assert_eq!(worker.configured_control_timeout, Some(budget));
        assert_eq!(worker.control.read_timeout()?, Some(budget));
        assert_eq!(worker.control.write_timeout()?, Some(budget));
        assert!(worker.process.child.try_wait()?.is_some());
        Ok(())
    }

    #[test]
    fn native_control_timeout_actual_child_drop_retires_connection_after_enter(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut worker =
            launch_existing_test_child(Duration::from_secs(1), Duration::from_secs(30))?;
        assert_eq!(
            worker.invoke(
                "qualification state entry",
                command(super::super::protocol::CommandKind::EnterState, 0),
            )?,
            None
        );
        // This alias observes connection retirement, not another handle's options.
        // The actual worker handle's shutdown budget is checked above. A clone
        // retained the old timeout on the qualified Windows implementation.
        let mut observer = worker.control.try_clone()?;
        drop(worker);
        // Only set the observer's read budget after the worker has been destroyed.
        observer.set_read_timeout(Some(Duration::from_millis(100)))?;
        let mut unexpected = [0; 1];
        match observer.read(&mut unexpected) {
            Ok(0) => {}
            Err(error) if error.kind() == io::ErrorKind::ConnectionReset => {}
            other => panic!("retired worker connection returned {other:?}"),
        }
        Ok(())
    }

    const SHUTDOWN_OBSERVATION_MODE: &str = "MONDRIAN_AUDIO_SHUTDOWN_OBSERVATION_MODE";
    const SHUTDOWN_OBSERVATION_ENDPOINT: &str = "MONDRIAN_AUDIO_SHUTDOWN_OBSERVATION_ENDPOINT";

    // A real Child and TCP endpoint exercise the original codecs and cleanup.
    // Without the private parent-provided endpoint this libtest entry is inert.
    #[test]
    fn native_shutdown_observation_child_fixture() {
        let Some(endpoint) = std::env::var_os(SHUTDOWN_OBSERVATION_ENDPOINT) else {
            return;
        };
        let mode = std::env::var(SHUTDOWN_OBSERVATION_MODE).expect("owned fixture mode");
        let mut control = TcpStream::connect(endpoint.to_str().expect("local endpoint"))
            .expect("connect actual parent");
        control.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        control.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
        let command = WorkerCommand::read_from(&mut control).expect("original shutdown command");
        assert_eq!(command.kind, super::super::protocol::CommandKind::Shutdown);
        assert_eq!(
            (
                command.start_sample,
                command.frames,
                command.event_count,
                command.note_count
            ),
            (0, 0, 0, 0)
        );
        match mode.as_str() {
            "ack_exit0" | "ack_exit7" | "ack_hold" | "ack_release_exit0" => {
                WorkerResponse::Completed.write_to(&mut control).unwrap();
                if mode == "ack_exit0" {
                    std::process::exit(0);
                }
                if mode == "ack_exit7" {
                    std::process::exit(7);
                }
                if mode == "ack_release_exit0" {
                    let mut release = [0];
                    control.read_exact(&mut release).expect("parent releases delayed exit");
                    assert_eq!(release, [1]);
                    thread::sleep(Duration::from_millis(10));
                    std::process::exit(0);
                }
            }
            "failed_hold" => {
                WorkerResponse::Failed("private payload must not enter the report".repeat(128))
                    .write_to(&mut control)
                    .unwrap()
            }
            "eof_hold" => control.shutdown(std::net::Shutdown::Write).unwrap(),
            "no_response" => {}
            _ => panic!("unknown owned fixture mode"),
        }
        thread::sleep(Duration::from_secs(5));
        panic!("parent failed to retire its bounded test Child");
    }

    fn launch_shutdown_observation_child(
        mode: &str,
        operation: Duration,
    ) -> Result<SupervisedAudioWorker, Box<dyn std::error::Error>> {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let mut command = Command::new(std::env::current_exe()?);
        command.args([
            "--exact",
            "processor_isolation::supervisor::control_timeout_worker_lifecycle::native_shutdown_observation_child_fixture",
            "--nocapture", "--test-threads=1",
        ]).env(SHUTDOWN_OBSERVATION_ENDPOINT, listener.local_addr()?.to_string())
            .env(SHUTDOWN_OBSERVATION_MODE, mode)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        #[cfg(windows)]
        configure_background_worker_start(&mut command);
        let mut process = ProcessOwner::new(command.spawn()?);
        let control = accept_worker(
            &listener,
            &mut process.child,
            Instant::now() + Duration::from_secs(5),
        )?;
        let layout = SharedBlockLayout::uniform(4, 2, 0, 0)?;
        let shared_file = NamedTempFile::new()?;
        shared_file.as_file().set_len(layout.total_bytes as u64)?;
        // SAFETY: the exclusively owned file is sized to the exact layout and
        // retained with this map; the fixture never receives or truncates it.
        let shared = unsafe { MmapOptions::new().map_mut(shared_file.as_file())? };
        Ok(SupervisedAudioWorker {
            process,
            control,
            shared,
            _shared_file: shared_file,
            layout,
            render_contract: AudioRenderContract {
                sample_rate: 48000,
                channel_layout: mondrian_core::AudioChannelLayout::Stereo,
                processing_mode: crate::AudioProcessingMode::Offline,
                max_block_frames: 4,
                processor_session_scratch_budget_bytes: 1024,
                public_output_lookahead_budget_frames: 0,
                compensation_delay_scratch_budget_bytes: 0,
            },
            staged_audio_input_mapping: None,
            operation_timeout: operation,
            state_entry_timeout: Duration::from_secs(30),
            configured_control_timeout: None,
            before_drop_test_hook: None,
            #[cfg(test)]
            discard_seed_commit_reply: None,
        })
    }

    #[test]
    fn native_termination_wait_deadline_retains_live_child_owner_and_staging(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut worker = launch_shutdown_observation_child("no_response", Duration::from_secs(1))?;
        let pid = worker.process.child.id();
        let staging = tempfile::tempdir()?;
        let path = staging.path().to_path_buf();
        std::fs::write(path.join("retained-input"), b"owned until confirmed exit")?;
        worker.process.staging = Some(staging);
        let mut report =
            ShutdownReport::new(pid, ProcessState::Retiring, Duration::from_millis(100));
        report.primary_failure = Some(ShutdownFailure::ExitObservationDeadline);

        // Emulate a successful asynchronous kill whose completion is not yet
        // observable. The same real Child remains blocked on its original TCP
        // command; no new process, fake exit status, or detached waiter exists.
        let result = worker.process.terminate_with_observation(
            Child::try_wait,
            |_| Ok(()),
            ProcessOwner::wait_after_kill,
            Some(&mut report.cleanup),
        );
        assert!(
            result.is_err(),
            "an accepted kill request is not an exit observation"
        );
        assert_eq!(worker.process.state, ProcessState::Retiring);
        assert_eq!(worker.process.child.id(), pid);
        assert!(worker.process.child.try_wait()?.is_none());
        assert!(path.join("retained-input").is_file());
        assert_eq!(report.cleanup.kill.as_ref().unwrap().outcome, Ok(()));
        assert_eq!(
            report.cleanup.wait.as_ref().unwrap().outcome.unwrap_err().kind,
            io::ErrorKind::TimedOut
        );
        assert!(report.exit_loop.is_none());
        let report = report.finish(false, worker.process.state);
        assert!(!report.successful());
        assert_eq!(
            report.primary_failure,
            Some(ShutdownFailure::ExitObservationDeadline)
        );

        // Recover through the actual production kill and bounded wait, using
        // this exact owner. Only confirmed settlement permits staging removal.
        worker.process.terminate()?;
        assert_eq!(worker.process.state, ProcessState::Settled);
        assert_eq!(worker.process.child.id(), pid);
        assert!(worker.process.child.try_wait()?.is_some());
        drop(worker);
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn native_exit_wait_actual_already_exited_child_keeps_original_status(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut worker = launch_shutdown_observation_child("ack_exit7", Duration::from_secs(1))?;
        command(super::super::protocol::CommandKind::Shutdown, 0).write_to(&mut worker.control)?;
        assert!(matches!(
            WorkerResponse::read_from(&mut worker.control)?,
            WorkerResponse::Completed
        ));
        let original = worker.process.child.wait()?;
        assert_eq!(original.code(), Some(7));
        let observed = exit_wait::wait_for_exit(
            &mut worker.process.child,
            Instant::now() + Duration::from_secs(1),
        );
        assert!(
            matches!(observed, exit_wait::ExitWaitOutcome::Exited(actual) if actual == original)
        );
        assert_eq!(worker.process.try_wait()?, Some(original));
        assert_eq!(worker.process.state, ProcessState::Settled);
        Ok(())
    }

    #[test]
    fn native_exit_wait_actual_delayed_child_uses_same_owner_and_real_exit_status(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut worker =
            launch_shutdown_observation_child("ack_release_exit0", Duration::from_secs(1))?;
        let original_pid = worker.process.child.id();
        command(super::super::protocol::CommandKind::Shutdown, 0).write_to(&mut worker.control)?;
        assert!(matches!(
            WorkerResponse::read_from(&mut worker.control)?,
            WorkerResponse::Completed
        ));
        assert!(
            worker.process.child.try_wait()?.is_none(),
            "child awaits actual parent release"
        );
        std::io::Write::write_all(&mut worker.control, &[1])?;
        let observed = exit_wait::wait_for_exit(
            &mut worker.process.child,
            Instant::now() + Duration::from_secs(1),
        );
        assert!(
            matches!(observed, exit_wait::ExitWaitOutcome::Exited(actual) if actual.code() == Some(0))
        );
        assert_eq!(worker.process.child.id(), original_pid);
        assert!(worker.process.try_wait()?.is_some_and(|status| status.success()));
        assert_eq!(worker.process.state, ProcessState::Settled);
        Ok(())
    }

    #[test]
    fn native_exit_wait_actual_live_child_at_deadline_is_not_terminated_by_wait(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut worker = launch_shutdown_observation_child("ack_hold", Duration::from_secs(1))?;
        command(super::super::protocol::CommandKind::Shutdown, 0).write_to(&mut worker.control)?;
        assert!(matches!(
            WorkerResponse::read_from(&mut worker.control)?,
            WorkerResponse::Completed
        ));
        let observed = exit_wait::wait_for_exit(
            &mut worker.process.child,
            Instant::now() + Duration::from_millis(10),
        );
        assert!(matches!(
            observed,
            exit_wait::ExitWaitOutcome::Deadline(Some(ChildObservation::Running))
        ));
        assert!(
            worker.process.child.try_wait()?.is_none(),
            "waiting does not kill or reap a live Child"
        );
        worker.terminate();
        assert_eq!(worker.process.state, ProcessState::Settled);
        assert!(worker.process.child.try_wait()?.is_some());
        Ok(())
    }

    #[test]
    fn native_shutdown_observation_actual_child_ack_keeps_exit_status_and_bool_semantics(
    ) -> Result<(), Box<dyn std::error::Error>> {
        for (mode, code) in [("ack_exit0", 0), ("ack_exit7", 7)] {
            let mut worker = launch_shutdown_observation_child(mode, Duration::from_secs(1))?;
            let report = worker.shutdown_observed();
            assert_eq!(report.successful(), code == 0);
            assert_eq!(
                report.primary_failure,
                if code == 0 {
                    None
                } else {
                    Some(ShutdownFailure::NonzeroExit)
                }
            );
            assert_eq!(report.response_read.as_ref().unwrap().outcome, Ok(0));
            assert!(matches!(report.exit_loop.as_ref().unwrap().outcome,
                ChildObservation::Exited(status) if status.code() == Some(code)));
            assert!(report.cleanup.initial_observe.is_none());
            assert!(report.cleanup.kill.is_none());
            assert!(report.cleanup.wait.is_none());
            assert_eq!(worker.process.state, ProcessState::Settled);
            assert!(
                worker.shutdown(),
                "Settled remains idempotently successful, even after nonzero exit"
            );
            assert_eq!(report.validation_result().is_ok(), code == 0);
        }
        Ok(())
    }

    #[test]
    fn native_shutdown_observation_actual_tcp_distinguishes_response_tag_eof_and_timeout(
    ) -> Result<(), Box<dyn std::error::Error>> {
        for mode in ["failed_hold", "eof_hold", "no_response"] {
            let mut worker = launch_shutdown_observation_child(mode, Duration::from_millis(100))?;
            let pid = worker.process.child.id();
            let report = worker.shutdown_observed();
            assert!(!report.successful());
            assert_eq!(report.command_write.as_ref().unwrap().outcome, Ok(()));
            let response = report.response_read.as_ref().unwrap();
            if mode == "failed_hold" {
                assert_eq!(
                    report.primary_failure,
                    Some(ShutdownFailure::NonCompletedResponse)
                );
                assert_eq!(response.outcome, Ok(1));
            } else {
                assert_eq!(report.primary_failure, Some(ShutdownFailure::ResponseRead));
                assert!(response.outcome.is_err());
                let actual = response.outcome.unwrap_err();
                if mode == "eof_hold" {
                    assert_eq!(actual.kind, io::ErrorKind::UnexpectedEof);
                } else {
                    assert!(matches!(
                        actual.kind,
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ));
                }
            }
            assert!(
                report.exit_loop.is_none(),
                "no exit-loop query before Completed"
            );
            assert_eq!(worker.process.state, ProcessState::Settled);
            assert!(matches!(
                report.cleanup.initial_observe.as_ref().unwrap().outcome,
                ChildObservation::Running
            ));
            assert_eq!(report.cleanup.kill.as_ref().unwrap().outcome, Ok(()));
            assert!(
                report.cleanup.final_observe.is_none(),
                "post-observe only follows refused kill"
            );
            assert!(report.cleanup.wait.as_ref().unwrap().outcome.is_ok());
            let Err(AudioProcessorHostError::WorkerFailed { operation, detail }) =
                report.validation_result()
            else {
                panic!("original typed validation error");
            };
            assert_eq!(operation, "validation shutdown");
            assert!(detail
                .starts_with("Worker did not acknowledge and exit within the bounded shutdown; "));
            assert!(detail.contains(&format!("pid={pid};")));
            assert!(!detail.contains("private payload"));
            assert!(detail.len() <= super::super::protocol::MAX_WORKER_DETAIL_BYTES);
            let configure = report.configure.as_ref().unwrap();
            let command = report.command_write.as_ref().unwrap();
            let observe = report.cleanup.initial_observe.as_ref().unwrap();
            let kill = report.cleanup.kill.as_ref().unwrap();
            let wait = report.cleanup.wait.as_ref().unwrap();
            assert!(configure.after <= command.after && command.after <= response.after);
            assert!(
                response.after <= observe.after
                    && observe.after <= kill.after
                    && kill.after <= wait.after
            );
        }
        Ok(())
    }

    #[test]
    fn native_shutdown_observation_closed_writer_does_not_attempt_response_read(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut worker = launch_shutdown_observation_child("no_response", Duration::from_secs(1))?;
        worker.control.shutdown(std::net::Shutdown::Write)?;
        let report = worker.shutdown_observed();
        assert!(!report.successful());
        assert_eq!(report.primary_failure, Some(ShutdownFailure::CommandWrite));
        assert!(report.command_write.as_ref().unwrap().outcome.is_err());
        assert!(report.response_read.is_none());
        assert!(report.exit_loop.is_none());
        assert_eq!(worker.process.state, ProcessState::Settled);
        Ok(())
    }

    fn shutdown_refuse_observation(_: &mut Child) -> io::Result<Option<std::process::ExitStatus>> {
        Err(io::Error::from_raw_os_error(6))
    }

    fn shutdown_refuse_kill(_: &mut Child) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(5))
    }

    fn shutdown_forbidden_wait(_: &mut Child) -> io::Result<std::process::ExitStatus> {
        panic!("refused kill must not wait")
    }

    #[test]
    fn native_shutdown_observation_deadline_preserves_primary_and_records_original_fallback(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut worker = launch_shutdown_observation_child("ack_hold", Duration::from_millis(100))?;
        worker.process.cleanup_hooks =
            Some((shutdown_refuse_observation, Child::kill, Child::wait));
        let report = worker.shutdown_observed();
        assert!(!report.successful());
        assert_eq!(
            report.primary_failure,
            Some(ShutdownFailure::ExitObservationDeadline)
        );
        assert_eq!(report.response_read.as_ref().unwrap().outcome, Ok(0));
        assert_eq!(
            report.exit_loop.as_ref().unwrap().outcome,
            ChildObservation::Running,
            "the original exit loop uses real Child.try_wait, never fallback hooks"
        );
        assert_eq!(
            report.cleanup.initial_observe.as_ref().unwrap().outcome,
            ChildObservation::Io((&io::Error::from_raw_os_error(6)).into())
        );
        assert_eq!(report.cleanup.kill.as_ref().unwrap().outcome, Ok(()));
        assert!(report.cleanup.final_observe.is_none());
        assert!(report.cleanup.wait.as_ref().unwrap().outcome.is_ok());
        assert_eq!(worker.process.state, ProcessState::Settled);
        worker.process.cleanup_hooks = None;
        Ok(())
    }

    #[test]
    fn native_shutdown_observation_refused_kill_records_post_observe_and_never_waits(
    ) -> Result<(), Box<dyn std::error::Error>> {
        for fail_observe in [false, true] {
            let mut worker =
                launch_shutdown_observation_child("ack_hold", Duration::from_millis(100))?;
            let observe = if fail_observe {
                shutdown_refuse_observation
            } else {
                Child::try_wait
            };
            worker.process.cleanup_hooks =
                Some((observe, shutdown_refuse_kill, shutdown_forbidden_wait));
            let installed = worker.configured_control_timeout;
            let report = worker.shutdown_observed();
            assert!(!report.successful());
            assert_eq!(
                report.primary_failure,
                Some(ShutdownFailure::ExitObservationDeadline)
            );
            let expected = if fail_observe {
                ChildObservation::Io((&io::Error::from_raw_os_error(6)).into())
            } else {
                ChildObservation::Running
            };
            assert_eq!(
                report.cleanup.initial_observe.as_ref().unwrap().outcome,
                expected
            );
            assert_eq!(
                report.cleanup.kill.as_ref().unwrap().outcome,
                Err((&io::Error::from_raw_os_error(5)).into())
            );
            assert_eq!(
                report.cleanup.final_observe.as_ref().unwrap().outcome,
                expected
            );
            assert!(report.cleanup.wait.is_none());
            assert_eq!(worker.process.state, ProcessState::Retiring);
            assert_eq!(installed, None);
            let before_retry = worker.configured_control_timeout;
            let read_budget = worker.control.read_timeout()?;
            let write_budget = worker.control.write_timeout()?;
            worker.process.cleanup_hooks = None;
            let retry = worker.shutdown_observed();
            assert!(
                !retry.successful(),
                "cleanup retry alone never supplies graceful ACK"
            );
            assert_eq!(
                retry.primary_failure,
                Some(ShutdownFailure::AlreadyRetiring)
            );
            assert!(
                retry.configure.is_none()
                    && retry.command_write.is_none()
                    && retry.response_read.is_none()
            );
            assert_eq!(worker.configured_control_timeout, before_retry);
            assert_eq!(worker.control.read_timeout()?, read_budget);
            assert_eq!(worker.control.write_timeout()?, write_budget);
            assert_eq!(worker.process.state, ProcessState::Settled);
            assert!(worker.shutdown());
        }
        Ok(())
    }

    #[test]
    fn native_shutdown_observation_cleanup_exit_race_keeps_real_status_and_zero_wait(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut worker = launch_shutdown_observation_child("no_response", Duration::from_secs(1))?;
        let mut report = ShutdownReport::new(
            worker.process.child.id(),
            worker.process.state,
            worker.operation_timeout,
        );
        let trace = std::cell::RefCell::new(Vec::new());
        worker.process.terminate_with_observation(
            |child| {
                trace.borrow_mut().push("observe");
                child.try_wait()
            },
            |child| {
                trace.borrow_mut().push("kill");
                // The control deliberately establishes a real exit before
                // reporting a refused kill, reproducing the existing race.
                // These bounded test-only observations are not production calls.
                child.kill()?;
                let deadline = Instant::now() + Duration::from_secs(5);
                while child.try_wait()?.is_none() {
                    assert!(Instant::now() < deadline, "actual test child exit");
                    thread::yield_now();
                }
                Err(io::Error::from_raw_os_error(5))
            },
            |_| panic!("confirmed post-kill exit must not call wait"),
            Some(&mut report.cleanup),
        )?;
        assert_eq!(trace.into_inner(), ["observe", "kill", "observe"]);
        assert_eq!(
            report.cleanup.initial_observe.as_ref().unwrap().outcome,
            ChildObservation::Running
        );
        assert_eq!(
            report.cleanup.kill.as_ref().unwrap().outcome,
            Err((&io::Error::from_raw_os_error(5)).into())
        );
        assert!(matches!(
            report.cleanup.final_observe.as_ref().unwrap().outcome,
            ChildObservation::Exited(_)
        ));
        assert!(report.cleanup.wait.is_none());
        assert_eq!(worker.process.state, ProcessState::Settled);
        Ok(())
    }

    #[cfg(windows)]
    mod process_retirement_tests {
        use super::*;
        use std::cell::RefCell;
        use std::io::Write;
        use std::path::PathBuf;
        use std::process::{Command, ExitStatus, Stdio};

        mod native_child_settlement_tests {
            use super::*;
            use mondrian_core::{
                NativeChildSettlement as Settlement, NativeChildSettlementError as Error,
                NativeChildSettlementRegistrar as Registrar,
                NativeChildSettlementReporter as Reporter,
            };
            use std::os::windows::io::{AsHandle, AsRawHandle, OwnedHandle};
            use std::sync::{
                atomic::{AtomicBool, AtomicUsize, Ordering},
                Arc,
            };
            use windows_sys::Win32::System::Threading::{TerminateProcess, WaitForSingleObject};

            #[derive(Default)]
            struct Counts {
                scopes: AtomicUsize,
                children: AtomicUsize,
                finishes: AtomicUsize,
                unconfirmed: AtomicBool,
                bad: AtomicBool,
                panic_end: AtomicBool,
            }
            impl Reporter for Counts {
                fn begin_scope(&self) -> Result<(), Error> {
                    self.scopes.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
                fn end_scope(&self) {
                    self.scopes.fetch_sub(1, Ordering::SeqCst);
                }
                fn begin_child(&self) -> Result<(), Error> {
                    self.children.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
                fn end_child(&self, result: Settlement) {
                    self.finishes.fetch_add(1, Ordering::SeqCst);
                    self.children.fetch_sub(1, Ordering::SeqCst);
                    if result == Settlement::Unconfirmed {
                        self.unconfirmed.store(true, Ordering::SeqCst);
                        self.unavailable();
                    }
                    if self.panic_end.swap(false, Ordering::SeqCst) {
                        panic!("native child settlement reporter test panic");
                    }
                }
                fn unavailable(&self) {
                    self.bad.store(true, Ordering::SeqCst);
                }
            }
            struct Cleanup(OwnedHandle);
            impl Cleanup {
                fn close(&self) -> io::Result<()> {
                    // SAFETY: the owned handle duplicates the original live Child
                    // process handle with its existing termination/synchronize rights.
                    // No PID reopen or second Child owner is involved.
                    let handle = self.0.as_raw_handle().cast();
                    if unsafe { WaitForSingleObject(handle, 0) } == 0 {
                        return Ok(());
                    }
                    if unsafe { TerminateProcess(handle, 73) } == 0
                        && unsafe { WaitForSingleObject(handle, 0) } != 0
                    {
                        return Err(io::Error::last_os_error());
                    }
                    if unsafe { WaitForSingleObject(handle, 2_000) } != 0 {
                        return Err(io::Error::other(
                            "same-child test cleanup did not confirm exit",
                        ));
                    }
                    Ok(())
                }
            }
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    let _ = self.close();
                }
            }
            fn denied_observe(_: &mut Child) -> io::Result<Option<ExitStatus>> {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "test observation refused",
                ))
            }
            fn denied_kill(_: &mut Child) -> io::Result<()> {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "test kill refused",
                ))
            }
            fn forbidden_wait(_: &mut Child) -> io::Result<ExitStatus> {
                Err(io::Error::other("no wait after refused kill"))
            }

            #[test]
            #[ignore = "actual owner Job required; original ProcessOwner forced-kill result, not native DSP"]
            fn native_child_forced_kill_reports_settled_only_on_final_owner_drop() {
                let (mut owner, _directory) = sleeping_owner();
                let counts = Arc::new(Counts::default());
                let root = Registrar::try_new(counts.clone()).unwrap();
                owner.bind_native_child_settlement(&root).unwrap();
                owner.bind_native_child_settlement(&root).unwrap();
                assert_eq!(counts.children.load(Ordering::SeqCst), 1);
                owner.terminate().expect("real forced kill and reap");
                assert_eq!(owner.state, ProcessState::Settled);
                assert_eq!(
                    counts.finishes.load(Ordering::SeqCst),
                    0,
                    "no early report at terminate"
                );
                drop(owner);
                assert_eq!(counts.finishes.load(Ordering::SeqCst), 1);
                assert_eq!(counts.children.load(Ordering::SeqCst), 0);
                assert!(!counts.unconfirmed.load(Ordering::SeqCst));
                assert!(!counts.bad.load(Ordering::SeqCst));
                drop(root);
                assert_eq!(counts.scopes.load(Ordering::SeqCst), 0);
            }

            #[test]
            #[ignore = "actual owner Job required; original refused cleanup reports unconfirmed before test-only same-handle cleanup"]
            fn native_child_refused_final_cleanup_reports_unconfirmed_and_preserves_domain() {
                let (mut owner, _directory) = sleeping_owner();
                let external = Cleanup(owner.child.as_handle().try_clone_to_owned().unwrap());
                let counts = Arc::new(Counts::default());
                let other = Arc::new(Counts::default());
                let root = Registrar::try_new(counts.clone()).unwrap();
                let foreign = Registrar::try_new(other.clone()).unwrap();
                owner.bind_native_child_settlement(&root).unwrap();
                assert_eq!(
                    owner.bind_native_child_settlement(&foreign),
                    Err(Error::ForeignDomain)
                );
                assert!(counts.bad.load(Ordering::SeqCst) && other.bad.load(Ordering::SeqCst));
                assert_eq!(
                    (
                        counts.children.load(Ordering::SeqCst),
                        other.children.load(Ordering::SeqCst)
                    ),
                    (1, 0)
                );
                let staging = tempfile::tempdir().expect("exclusive live-child staging");
                let staging_path = staging.path().to_owned();
                let sentinel = staging_path.join("still-in-use");
                std::fs::write(&sentinel, b"actual live-child staging").unwrap();
                owner.staging = Some(staging);
                owner.cleanup_hooks = Some((denied_observe, denied_kill, forbidden_wait));
                counts.panic_end.store(true, Ordering::SeqCst);
                let dropped =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(owner)));
                let expected_panic = dropped.as_ref().err().is_some_and(|payload| {
                    payload.downcast_ref::<&str>()
                        == Some(&"native child settlement reporter test panic")
                });
                // Capture before external cleanup. Reporter unwind must not let
                // TempDir field drop delete a still-unconfirmed child's staging.
                let retained_bytes = std::fs::read(&sentinel).ok();
                let receipt = (
                    counts.finishes.load(Ordering::SeqCst),
                    counts.children.load(Ordering::SeqCst),
                    counts.unconfirmed.load(Ordering::SeqCst),
                );
                external.close().expect("test cleanup before result assertions");
                if staging_path.exists() {
                    std::fs::remove_dir_all(&staging_path)
                        .expect("retire test-owned staging after child exit");
                }
                assert!(
                    expected_panic,
                    "only the injected Reporter contract violation is accepted"
                );
                assert_eq!(
                    retained_bytes.as_deref(),
                    Some(b"actual live-child staging".as_slice())
                );
                assert_eq!(receipt, (1, 0, true));
                drop(root);
                drop(foreign);
                assert_eq!(
                    (
                        counts.scopes.load(Ordering::SeqCst),
                        other.scopes.load(Ordering::SeqCst)
                    ),
                    (0, 0)
                );
            }
        }

        const READY_ENV: &str = "MONDRIAN_AUDIO_RETIREMENT_FIXTURE_READY";
        const EXIT_ENV: &str = "MONDRIAN_AUDIO_RETIREMENT_FIXTURE_EXIT";
        const FIXTURE: &str =
        "processor_isolation::supervisor::control_timeout_worker_lifecycle::process_retirement_tests::owned_sleeping_child_fixture";

        fn require_actual_job() {
            let mut inside = 0;
            // SAFETY: the current-process pseudo handle is valid; a null job asks
            // about any Job. `inside` is a live writable BOOL for this query only.
            // No handles are acquired or closed. The outer runner identifies the
            // actual owner Job; this local guard proves membership, not its identity.
            let ok = unsafe {
                windows_sys::Win32::System::JobObjects::IsProcessInJob(
                    windows_sys::Win32::System::Threading::GetCurrentProcess(),
                    std::ptr::null_mut(),
                    &mut inside,
                )
            };
            assert_ne!(ok, 0, "Job query failed: {}", io::Error::last_os_error());
            assert_ne!(
                inside, 0,
                "requires an actual outer owner Job before spawning"
            );
        }

        #[test]
        #[ignore = "direct child fixture; invoke only through a Job-contained retirement test"]
        fn owned_sleeping_child_fixture() {
            require_actual_job();
            let ready = PathBuf::from(std::env::var_os(READY_ENV).expect("exclusive ready path"));
            let exit = PathBuf::from(std::env::var_os(EXIT_ENV).expect("exclusive exit path"));
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(ready)
                .expect("create-only readiness marker");
            writeln!(file, "{}", std::process::id()).expect("complete readiness marker");
            let deadline = Instant::now() + Duration::from_secs(30);
            while Instant::now() < deadline {
                if exit.exists() {
                    std::process::exit(7);
                }
                thread::sleep(Duration::from_millis(10));
            }
            panic!("outer owner failed to settle its sleeping child");
        }

        fn sleeping_owner() -> (ProcessOwner, tempfile::TempDir) {
            require_actual_job();
            let directory = tempfile::tempdir().expect("exclusive child markers");
            let ready = directory.path().join("ready");
            let child = Command::new(std::env::current_exe().expect("current libtest executable"))
                .args([
                    "--exact",
                    FIXTURE,
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(READY_ENV, &ready)
                .env(EXIT_ENV, directory.path().join("exit"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn actual inherited-Job child");
            // Wrap immediately: all later assertion/unwind paths use the same
            // production cleanup, never ignored kill followed by unconditional wait.
            let mut owner = ProcessOwner::new(child);
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                assert!(Instant::now() < deadline, "child readiness deadline");
                assert!(owner.child.try_wait().expect("observe startup").is_none());
                if let Ok(marker) = std::fs::read_to_string(&ready)
                    && let Ok(pid) = marker.trim().parse::<u32>()
                {
                    assert_eq!(
                        pid,
                        owner.child.id(),
                        "marker belongs to exact retained Child"
                    );
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            (owner, directory)
        }

        thread_local! {
            static DISCOVERY_WAIT_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        }

        fn refuse_discovery_kill(_: &mut Child) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected discovery kill refusal",
            ))
        }

        fn forbidden_discovery_wait(_: &mut Child) -> io::Result<ExitStatus> {
            DISCOVERY_WAIT_CALLS.with(|calls| calls.set(calls.get() + 1));
            Err(io::Error::other("forbidden discovery wait was invoked"))
        }

        fn refuse_discovery_observation(_: &mut Child) -> io::Result<Option<ExitStatus>> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected discovery observation refusal",
            ))
        }

        #[test]
        #[ignore = "actual owner Job required for injected discovery retirement"]
        fn discovery_refused_kill_preserves_primary_error_keeps_staging_and_never_waits() {
            use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
            use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
            use windows_sys::Win32::System::Threading::{
                OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
                PROCESS_SYNCHRONIZE,
            };
            let (mut owner, directory) = sleeping_owner();
            let staging = tempfile::tempdir().unwrap();
            let path = staging.path().to_owned();
            owner.staging = Some(staging);
            owner.cleanup_hooks = Some((
                Child::try_wait,
                refuse_discovery_kill,
                forbidden_discovery_wait,
            ));
            DISCOVERY_WAIT_CALLS.with(|calls| calls.set(0));
            let primary =
                AudioProcessorHostError::WorkerDeadlineExceeded { operation: "CLAP discovery" };
            let retained = super::super::super::clap_discovery::retire_discovery_failure(
                &mut owner,
                primary.clone(),
            );
            assert_eq!(retained, primary);
            assert_eq!(owner.state, ProcessState::Retiring);
            assert!(path.exists());
            owner.cleanup_hooks = Some((
                refuse_discovery_observation,
                refuse_discovery_kill,
                forbidden_discovery_wait,
            ));
            let primary = AudioProcessorHostError::Unavailable(
                "CLAP discovery worker wait failed: injected observation refusal".to_owned(),
            );
            assert_eq!(
                super::super::super::clap_discovery::retire_discovery_failure(
                    &mut owner,
                    primary.clone()
                ),
                primary
            );
            assert_eq!(owner.state, ProcessState::Retiring);
            // Hold only an observation handle across the intentionally refused
            // owner Drop. It is not another Child owner or another kill path.
            // SAFETY: The readiness-checked Child remains owned here. OpenProcess
            // requests only query/synchronize rights and a noninheritable handle;
            // null is checked below before exactly one OwnedHandle adoption.
            // PID-to-handle acquisition has a narrow exit/reuse race: this is
            // not the original Child kernel handle or proof of its identity.
            // The enclosing Job must still establish descendant settlement.
            let observation = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    0,
                    owner.child.id(),
                )
            };
            assert!(!observation.is_null());
            // SAFETY: OpenProcess returned one owned nonnull handle. OwnedHandle
            // closes it once on every assertion/unwind path.
            let observation = unsafe { OwnedHandle::from_raw_handle(observation) };
            drop(owner);
            assert!(
                path.exists(),
                "unconfirmed child staging must not be deleted"
            );
            DISCOVERY_WAIT_CALLS.with(|calls| assert_eq!(calls.get(), 0));
            request_natural_exit(&directory);
            // SAFETY: This nonnull OwnedHandle remains live for the call and
            // has synchronize rights. Its borrowed handle is neither closed
            // nor transferred during the existing finite 5,000 ms wait; the
            // result observes the retained object, without another PID lookup.
            let observed = unsafe { WaitForSingleObject(observation.as_raw_handle(), 5_000) };
            assert_eq!(observed, WAIT_OBJECT_0);
            // Cleanup only the exact retained TempDir path, after real exit.
            assert!(path.is_absolute() && path.parent() == Some(std::env::temp_dir().as_path()));
            std::fs::remove_dir_all(path).unwrap();
        }

        #[test]
        #[ignore = "actual owner Job required for discovery staging settlement"]
        fn discovery_confirmed_exit_preserves_status_and_deletes_staging() {
            let (mut owner, directory) = sleeping_owner();
            let staging = tempfile::tempdir().unwrap();
            let path = staging.path().to_owned();
            owner.staging = Some(staging);
            request_natural_exit(&directory);
            let deadline = Instant::now() + Duration::from_secs(5);
            let status = loop {
                if let Some(status) = owner.try_wait().unwrap() {
                    break status;
                }
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(5));
            };
            assert_eq!(status.code(), Some(7));
            assert_eq!(owner.state, ProcessState::Settled);
            assert!(path.exists(), "hold staging through caller decoding scope");
            drop(owner);
            assert!(!path.exists());
        }

        #[test]
        #[ignore = "actual owner Job required for post-spawn unwind"]
        fn discovery_post_spawn_panic_uses_actual_owner_drop_and_releases_settled_staging() {
            let (mut owner, _directory) = sleeping_owner();
            let staging = tempfile::tempdir().unwrap();
            let path = staging.path().to_owned();
            owner.staging = Some(staging);
            let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                let _owned = owner;
                panic!("injected post-spawn failure");
            }));
            assert!(unwind.is_err());
            assert!(
                !path.exists(),
                "confirmed actual kill/reap releases staging on unwind"
            );
        }

        fn await_exit(child: &mut Child) -> ExitStatus {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                assert!(
                    Instant::now() < deadline,
                    "actual exit observation deadline"
                );
                if let Some(status) = child.try_wait().expect("nonblocking exit observation") {
                    return status;
                }
                thread::sleep(Duration::from_millis(5));
            }
        }

        fn request_natural_exit(directory: &tempfile::TempDir) {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(directory.path().join("exit"))
                .expect("create-only natural exit request");
        }

        fn refusal_text(result: Result<(), AudioProcessorHostError>, pid: u32) -> String {
            let Err(AudioProcessorHostError::WorkerFailed { operation, detail: text }) = result
            else {
                panic!("expected exact typed unconfirmed cleanup refusal: {result:?}");
            };
            assert_eq!(operation, "native child cleanup");
            assert!(text.contains(&format!("pid={pid}")), "{text}");
            text
        }

        fn assert_settled_idempotent(owner: &mut ProcessOwner) {
            assert_eq!(owner.state, ProcessState::Settled);
            owner
                .terminate_with(
                    |_| panic!("settled owner must not observe again"),
                    |_| panic!("settled owner must not kill again"),
                    |_| panic!("settled owner must not wait again"),
                )
                .expect("idempotent settlement");
            assert!(owner.child.try_wait().expect("actual child exit retained").is_some());
        }

        #[test]
        #[ignore = "requires actual outer owner Job; spawns one owned native OS child"]
        fn audio_process_initial_exit_settles_without_kill_or_wait() {
            let (mut owner, directory) = sleeping_owner();
            request_natural_exit(&directory);
            assert_eq!(await_exit(&mut owner.child).code(), Some(7));
            let trace = RefCell::new(Vec::new());
            owner
                .terminate_with(
                    |child| {
                        trace.borrow_mut().push("observe");
                        child.try_wait()
                    },
                    |_| panic!("initial exit must not kill"),
                    |_| panic!("initial exit must not wait"),
                )
                .expect("actual initially exited child");
            assert_eq!(trace.into_inner(), ["observe"]);
            assert_settled_idempotent(&mut owner);
        }

        #[test]
        #[ignore = "requires actual outer owner Job; refusal injection with actual live retained child"]
        fn audio_process_refused_live_kill_retains_child_and_retry_settles() {
            let (mut owner, _directory) = sleeping_owner();
            let pid = owner.child.id();
            let trace = RefCell::new(Vec::new());
            let result = owner.terminate_with(
                |child| {
                    trace.borrow_mut().push("observe");
                    child.try_wait()
                },
                |_| {
                    trace.borrow_mut().push("kill");
                    Err(io::Error::from_raw_os_error(5))
                },
                |_| panic!("unconfirmed failed kill must never wait"),
            );
            let text = refusal_text(result, pid);
            assert!(
                text.contains("raw_os=Some(5)") && text.contains("child still running"),
                "{text}"
            );
            assert_eq!(trace.into_inner(), ["observe", "kill", "observe"]);
            assert_eq!(owner.state, ProcessState::Retiring);
            assert_eq!(owner.child.id(), pid);
            assert!(owner.child.try_wait().expect("actual refusal retained live child").is_none());
            assert_ne!(owner.state, ProcessState::Running, "no work admission");
            assert_eq!(owner.state, ProcessState::Retiring, "no re-admission");
            owner.terminate().expect("real kill/reap retry");
            assert_settled_idempotent(&mut owner);
        }

        #[test]
        #[ignore = "requires actual outer owner Job; actual natural exit after controlled kill refusal"]
        fn audio_process_refused_kill_exit_race_observes_settlement_without_wait() {
            let (mut owner, directory) = sleeping_owner();
            let trace = RefCell::new(Vec::new());
            owner
                .terminate_with(
                    |child| {
                        trace.borrow_mut().push("observe");
                        child.try_wait()
                    },
                    |child| {
                        trace.borrow_mut().push("kill");
                        request_natural_exit(&directory);
                        assert_eq!(await_exit(child).code(), Some(7));
                        Err(io::Error::from_raw_os_error(5))
                    },
                    |_| panic!("refused kill with confirmed exit must not wait"),
                )
                .expect("same actual child exited naturally before post-kill observation");
            assert_eq!(trace.into_inner(), ["observe", "kill", "observe"]);
            assert_settled_idempotent(&mut owner);
        }

        #[test]
        #[ignore = "requires actual outer owner Job; observation/kill failure injection"]
        fn audio_process_observation_and_kill_errors_preserve_stages_and_retirement() {
            let (mut owner, _directory) = sleeping_owner();
            let trace = RefCell::new(Vec::new());
            let mut observations = 0;
            let result = owner.terminate_with(
                |_| {
                    trace.borrow_mut().push("observe");
                    observations += 1;
                    Err(io::Error::from_raw_os_error(if observations == 1 {
                        87
                    } else {
                        6
                    }))
                },
                |_| {
                    trace.borrow_mut().push("kill");
                    Err(io::Error::from_raw_os_error(5))
                },
                |_| panic!("failed observations/refused kill must not wait"),
            );
            let text = refusal_text(result, owner.child.id());
            for evidence in [
                "initial try_wait=failed",
                "post-kill try_wait failed",
                "raw_os=Some(87)",
                "raw_os=Some(6)",
                "raw_os=Some(5)",
            ] {
                assert!(text.contains(evidence), "missing {evidence}: {text}");
            }
            assert_eq!(trace.into_inner(), ["observe", "kill", "observe"]);
            assert_eq!(owner.state, ProcessState::Retiring);
            assert!(owner
                .child
                .try_wait()
                .expect("injection retained actual live child")
                .is_none());
            assert_ne!(owner.state, ProcessState::Running, "no work admission");
            owner.terminate().expect("real cleanup after controlled OS errors");
            assert_settled_idempotent(&mut owner);
        }

        #[test]
        #[ignore = "requires actual outer owner Job; real kill with controlled reap failure"]
        fn audio_process_successful_kill_failed_wait_retains_then_observes_exit() {
            let (mut owner, _directory) = sleeping_owner();
            let trace = RefCell::new(Vec::new());
            let result = owner.terminate_with(
                |child| {
                    trace.borrow_mut().push("observe");
                    child.try_wait()
                },
                |child| {
                    trace.borrow_mut().push("kill");
                    child.kill()
                },
                |_| {
                    trace.borrow_mut().push("wait");
                    Err(io::Error::from_raw_os_error(6))
                },
            );
            let text = refusal_text(result, owner.child.id());
            assert!(
                text.contains("reap failed") && text.contains("raw_os=Some(6)"),
                "{text}"
            );
            assert_eq!(trace.into_inner(), ["observe", "kill", "wait"]);
            assert_eq!(owner.state, ProcessState::Retiring);
            await_exit(&mut owner.child);
            owner
                .terminate_with(
                    Child::try_wait,
                    |_| panic!("exit retry must not kill"),
                    |_| panic!("exit retry must not wait"),
                )
                .expect("actual exit observation settles failed reap");
            assert_settled_idempotent(&mut owner);
        }

        #[test]
        #[ignore = "requires actual outer owner Job; real kill and real reap"]
        fn audio_process_successful_kill_and_wait_settles_once() {
            let (mut owner, _directory) = sleeping_owner();
            let trace = RefCell::new(Vec::new());
            owner
                .terminate_with(
                    |child| {
                        trace.borrow_mut().push("observe");
                        child.try_wait()
                    },
                    |child| {
                        trace.borrow_mut().push("kill");
                        child.kill()
                    },
                    |child| {
                        trace.borrow_mut().push("wait");
                        child.wait()
                    },
                )
                .expect("real actual child kill and reap");
            assert_eq!(trace.into_inner(), ["observe", "kill", "wait"]);
            assert_settled_idempotent(&mut owner);
        }

        #[test]
        #[ignore = "requires actual outer owner Job; observation error followed by real kill/reap"]
        fn audio_process_initial_observation_error_does_not_skip_real_cleanup() {
            let (mut owner, _directory) = sleeping_owner();
            let trace = RefCell::new(Vec::new());
            owner
                .terminate_with(
                    |_| {
                        trace.borrow_mut().push("observe");
                        Err(io::Error::from_raw_os_error(6))
                    },
                    |child| {
                        trace.borrow_mut().push("kill");
                        child.kill()
                    },
                    |child| {
                        trace.borrow_mut().push("wait");
                        child.wait()
                    },
                )
                .expect("proven reap settles despite earlier observation error");
            assert_eq!(trace.into_inner(), ["observe", "kill", "wait"]);
            assert_settled_idempotent(&mut owner);
        }

        // Retain an independent native process handle BEFORE owner teardown. PID
        // reuse or releasing the Rust Child handle cannot masquerade as settlement.
        fn retain_process(child: &Child) -> std::os::windows::io::OwnedHandle {
            use std::os::windows::io::FromRawHandle;
            // SAFETY: OpenProcess borrows the known retained Child PID, requests
            // only synchronization, and returns either null or one caller-owned
            // handle. Ownership is transferred exactly once to OwnedHandle below.
            let raw = unsafe {
                windows_sys::Win32::System::Threading::OpenProcess(
                    windows_sys::Win32::System::Threading::PROCESS_SYNCHRONIZE,
                    0,
                    child.id(),
                )
            };
            assert!(
                !raw.is_null(),
                "retain actual process: {}",
                io::Error::last_os_error()
            );
            // SAFETY: raw is a non-null valid process handle uniquely returned by
            // OpenProcess above. OwnedHandle closes it exactly once on all paths.
            unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw) }
        }

        fn assert_handle_settled(handle: &std::os::windows::io::OwnedHandle) {
            use std::os::windows::io::AsRawHandle;
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                // SAFETY: OwnedHandle retains the native process handle for this
                // zero-timeout query. No mutation, transfer or handle close occurs.
                let observed = unsafe {
                    windows_sys::Win32::System::Threading::WaitForSingleObject(
                        handle.as_raw_handle(),
                        0,
                    )
                };
                match observed {
                    windows_sys::Win32::Foundation::WAIT_OBJECT_0 => return,
                    windows_sys::Win32::Foundation::WAIT_TIMEOUT => {
                        assert!(
                            Instant::now() < deadline,
                            "retained actual process did not settle"
                        );
                        thread::sleep(Duration::from_millis(5));
                    }
                    other => panic!(
                        "retained actual process query failed {other}: {}",
                        io::Error::last_os_error()
                    ),
                }
            }
        }

        #[test]
        #[ignore = "requires actual outer owner Job; startup-style implicit error unwinds real Child owner"]
        fn audio_process_startup_implicit_error_reaps_before_owner_release() {
            let (owner, _directory) = sleeping_owner();
            let observer = retain_process(&owner.child);
            let result: Result<(), AudioProcessorHostError> = (move || {
                let _process = owner;
                // Same fallible deadline expression as production, after ownership
                // is acquired. It must never release a bare std Child on `?`.
                Instant::now()
                    .checked_add(Duration::MAX)
                    .ok_or_else(|| invalid_contract("Worker startup deadline overflowed"))?;
                panic!("Duration::MAX unexpectedly fit startup deadline");
            })();
            assert_eq!(
                result,
                Err(invalid_contract("Worker startup deadline overflowed"))
            );
            assert_handle_settled(&observer);
        }

        #[test]
        #[ignore = "requires actual outer owner Job; real Child settlement through panic unwind"]
        fn audio_process_startup_panic_unwind_reaps_retained_actual_child() {
            let (owner, _directory) = sleeping_owner();
            let observer = retain_process(&owner.child);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                let _process = owner;
                panic!("controlled startup unwind after actual child readiness");
            }));
            assert!(result.is_err(), "panic boundary must remain visible");
            assert_handle_settled(&observer);
        }

        #[test]
        #[ignore = "requires actual outer owner Job; finite repeated refusal controls"]
        fn audio_process_repeated_refusal_never_waits_or_reopens_admission() {
            let (mut owner, _directory) = sleeping_owner();
            let pid = owner.child.id();
            let trace = RefCell::new(Vec::new());
            for _ in 0..2 {
                let result = owner.terminate_with(
                    |child| {
                        trace.borrow_mut().push("observe");
                        child.try_wait()
                    },
                    |_| {
                        trace.borrow_mut().push("kill");
                        Err(io::Error::from_raw_os_error(5))
                    },
                    |_| panic!("refused retry must never wait"),
                );
                refusal_text(result, pid);
                assert_eq!(owner.state, ProcessState::Retiring);
                assert!(owner.child.try_wait().expect("same child remains live").is_none());
            }
            assert_eq!(
                trace.into_inner(),
                ["observe", "kill", "observe", "observe", "kill", "observe"]
            );
            let observer = retain_process(&owner.child);
            drop(owner);
            assert_handle_settled(&observer);
        }

        #[test]
        #[ignore = "requires actual outer owner Job; real existing worker and typed no-submission controls"]
        fn audio_retiring_worker_blocks_author_and_process_then_shutdown_cleans_without_ack() {
            require_actual_job();
            let mut worker =
                launch_existing_test_child(Duration::from_secs(1), Duration::from_secs(30))
                    .expect("existing authenticated worker fixture");
            let pid = worker.process.child.id();
            let observer = retain_process(&worker.process.child);
            worker.shared.fill(0xa5);
            let installed_timeout = worker.configured_control_timeout;
            assert_eq!(installed_timeout, Some(Duration::from_secs(1)));
            let result = worker.process.terminate_with(
                Child::try_wait,
                |_| Err(io::Error::from_raw_os_error(5)),
                |_| panic!("refused live child must not wait"),
            );
            refusal_text(result, pid);
            assert_eq!(worker.process.state, ProcessState::Retiring);
            assert!(worker
                .process
                .child
                .try_wait()
                .expect("actual live worker retained")
                .is_none());
            let result = worker.invoke(
                "retired processing",
                command(super::super::super::protocol::CommandKind::Process, 0),
            );
            assert_eq!(result, Err(AudioProcessorHostError::PoisonedInstance));
            let binding = mondrian_core::AudioProcessorBindingId::new();
            let request = |process_id| crate::NativeAudioAuthorStateEntryRequest {
                binding: crate::NativeAudioObjectBindRequest {
                    binding,
                    // Negative intent data, not a claimed native object observation.
                    expected_object: crate::NativeAudioObjectObservation::new(
                        std::num::NonZeroU32::new(process_id).expect("known live PID"),
                        std::num::NonZeroU64::new(1).expect("bounded claimed serial"),
                    ),
                },
                source: crate::NativeAudioAuthorStateEntrySource::CanonicalPeer { revision: 0 },
                format: super::super::super::NativeConfigurationFormat::ClapState,
                configuration: &[],
            };
            assert_ne!(pid, std::process::id());
            let foreign = worker
                .prepare_native_author_state_entry(request(std::process::id()))
                .expect("foreign PID remains a typed intent refusal");
            assert!(matches!(
                foreign,
                crate::NativeAudioAuthorStateEntryOutcome::Refused(
                    crate::NativeAudioAuthorStateEntryRefusal::OwnerChanged
                )
            ));
            let same = worker.prepare_native_author_state_entry(request(pid));
            assert!(
                matches!(same, Err(AudioProcessorHostError::PoisonedInstance)),
                "same-owner retired author command cannot submit"
            );
            assert_eq!(
                worker.configured_control_timeout, installed_timeout,
                "no state-entry socket mutation"
            );
            assert!(
                worker.shared.iter().all(|byte| *byte == 0xa5),
                "no shared PCM/event mutation"
            );
            assert_eq!(worker.process.child.id(), pid);
            assert!(worker.process.child.try_wait().expect("no native command sent").is_none());
            let installed_read = worker.control.read_timeout().expect("actual control read budget");
            let installed_write =
                worker.control.write_timeout().expect("actual control write budget");
            assert_eq!(installed_read, Some(Duration::from_secs(1)));
            assert_eq!(installed_write, Some(Duration::from_secs(1)));
            // Closing this writer makes attempted writes fail, but false alone
            // cannot prove no attempt. Distinct unchanged budgets below reject
            // the existing Running shutdown path's switch to 100ms. The direct
            // Retiring branch proves zero packet submission by static review.
            worker
                .control
                .shutdown(std::net::Shutdown::Write)
                .expect("close local control writer");
            assert!(
                !worker.shutdown(),
                "cleanup alone cannot claim fresh graceful ACK"
            );
            assert_eq!(worker.configured_control_timeout, installed_timeout);
            assert_eq!(
                worker.control.read_timeout().expect("post-cleanup read budget"),
                installed_read
            );
            assert_eq!(
                worker.control.write_timeout().expect("post-cleanup write budget"),
                installed_write
            );
            assert_settled_idempotent(&mut worker.process);
            assert_handle_settled(&observer);
            assert!(
                worker.shutdown(),
                "following Settled shutdown is idempotent"
            );
        }

        #[test]
        #[ignore = "direct success child fixture; invoke only through a Job-contained VST3 owner control"]
        fn vst3_discovery_successful_child_fixture() {
            require_actual_job();
        }

        fn successful_vst3_owner(staging: TempDir) -> ProcessOwner {
            require_actual_job();
            let mut command = Command::new(std::env::current_exe().expect("current libtest"));
            command.args([
                "--exact",
                "processor_isolation::supervisor::control_timeout_worker_lifecycle::process_retirement_tests::vst3_discovery_successful_child_fixture",
                "--ignored", "--nocapture", "--test-threads=1",
            ]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            configure_background_worker_start(&mut command);
            ProcessOwner::with_staging(
                command.spawn().expect("spawn actual Job-contained success child"),
                staging,
            )
        }

        #[test]
        #[ignore = "actual outer Job required; VST3 production retirement consumer with refused kill"]
        fn vst3_discovery_refused_kill_preserves_primary_staging_and_zero_wait() {
            let (mut owner, directory) = sleeping_owner();
            let staging = tempfile::tempdir().expect("VST3 staging");
            let path = staging.path().to_owned();
            owner.staging = Some(staging);
            owner.cleanup_hooks = Some((
                Child::try_wait,
                refuse_discovery_kill,
                forbidden_discovery_wait,
            ));
            DISCOVERY_WAIT_CALLS.with(|calls| calls.set(0));
            let primary =
                AudioProcessorHostError::WorkerDeadlineExceeded { operation: "VST3 discovery" };
            assert_eq!(
                super::super::super::vst3_discovery::retire_discovery_failure(
                    &mut owner,
                    primary.clone()
                ),
                primary
            );
            assert_eq!(owner.state, ProcessState::Retiring);
            assert!(path.exists());
            owner.cleanup_hooks = Some((
                refuse_discovery_observation,
                refuse_discovery_kill,
                forbidden_discovery_wait,
            ));
            let primary = AudioProcessorHostError::Unavailable(
                "VST3 discovery worker wait failed: injected observation refusal".to_owned(),
            );
            assert_eq!(
                super::super::super::vst3_discovery::retire_discovery_failure(
                    &mut owner,
                    primary.clone()
                ),
                primary
            );
            let observer = retain_process(&owner.child);
            drop(owner);
            assert!(
                path.exists(),
                "unconfirmed actual child retains staging through Drop"
            );
            DISCOVERY_WAIT_CALLS.with(|calls| assert_eq!(calls.get(), 0));
            request_natural_exit(&directory);
            assert_handle_settled(&observer);
            assert!(path.is_absolute() && path.parent() == Some(std::env::temp_dir().as_path()));
            std::fs::remove_dir_all(path).expect("remove only settled retained staging");
        }

        #[test]
        #[ignore = "actual outer Job required; VST3 finish consumer preserves nonzero status"]
        fn vst3_discovery_finish_preserves_nonzero_status_and_releases_staging() {
            let (mut owner, directory) = sleeping_owner();
            let staging = tempfile::tempdir().expect("VST3 staging");
            let path = staging.path().to_owned();
            let response = path.join("response.json");
            std::fs::write(&response, b"not valid JSON")
                .expect("response must not be decoded on failure");
            owner.staging = Some(staging);
            request_natural_exit(&directory);
            let status = await_exit(&mut owner.child);
            assert_eq!(status.code(), Some(7));
            assert!(path.exists());
            let result =
                super::super::super::vst3_discovery::finish_discovery_request(owner, &response);
            match result {
                Err(error) => assert_eq!(
                    error,
                    AudioProcessorHostError::Unavailable(format!(
                        "VST3 discovery worker failed: {status}"
                    ))
                ),
                Ok(_) => panic!("nonzero child must not publish response"),
            }
            assert!(
                !path.exists(),
                "confirmed exit releases staging after finish error"
            );
        }

        #[test]
        #[ignore = "actual outer Job required; VST3 real finish response/cap/decode controls"]
        fn vst3_discovery_finish_holds_staging_through_success_and_decode_refusal() {
            use super::super::super::vst3_discovery::{
                finish_discovery_request, DiscoveryResponse,
            };
            for case in 0..5 {
                let staging = tempfile::tempdir().expect("VST3 response staging");
                let path = staging.path().to_owned();
                let response = path.join("response.json");
                match case {
                    0 => std::fs::write(&response, br#"{"kind":"list_v7","schema_version":7,"descriptors":[]}"#).unwrap(),
                    1 => std::fs::write(&response, br#"{"kind":"probe_v7","schema_version":7,"main_audio_input":"present","audio_output_count":1,"main_audio_output_count":1,"input_names":[],"event_bus_contract":{"schema_version":1,"inputs":[],"outputs":[]},"class_id":"0123456789ABCDEF0123456789ABCDEF","vendor":null,"latency_frames":0,"tail_frames":0,"parameters":[],"current_values":[],"captured_state":null,"auxiliary_buses":[]}"#).unwrap(),
                    2 => {},
                    3 => std::fs::write(&response, b"not valid JSON").unwrap(),
                    4 => std::fs::write(&response, vec![b' '; 512 * 1024 + 1]).unwrap(),
                    _ => unreachable!(),
                }
                assert!(path.exists());
                let owner = successful_vst3_owner(staging);
                let result = finish_discovery_request(owner, &response);
                match (case, result) {
                    (0, Ok(DiscoveryResponse::List { descriptors, .. })) => {
                        assert!(descriptors.is_empty())
                    }
                    (
                        1,
                        Ok(DiscoveryResponse::Probe {
                            class_id,
                            parameters,
                            current_values,
                            captured_state,
                            auxiliary_buses,
                            ..
                        }),
                    ) => {
                        assert_eq!(class_id, "0123456789ABCDEF0123456789ABCDEF");
                        assert!(
                            parameters.is_empty()
                                && current_values.is_empty()
                                && auxiliary_buses.is_empty()
                        );
                        assert!(captured_state.is_none());
                    }
                    (2, Err(AudioProcessorHostError::Unavailable(detail))) => {
                        assert!(detail.starts_with("VST3 discovery file is unavailable: "))
                    }
                    (3, Err(AudioProcessorHostError::InvalidContract(detail))) => {
                        assert!(detail.starts_with("VST3 discovery response is invalid: "))
                    }
                    (4, Err(error)) => assert_eq!(
                        error,
                        AudioProcessorHostError::InvalidContract(
                            "VST3 discovery file exceeds its limit".to_owned()
                        )
                    ),
                    _ => panic!("unexpected VST3 finish response or primary for case {case}"),
                }
                assert!(
                    !path.exists(),
                    "staging is released after complete read/decode attempt"
                );
            }
        }

        thread_local! {
            static VST3_PANIC_OBSERVATION_ONCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        }

        fn panic_once_vst3_observation(child: &mut Child) -> io::Result<Option<ExitStatus>> {
            if VST3_PANIC_OBSERVATION_ONCE.with(|flag| flag.replace(false)) {
                panic!("injected unwind in actual VST3 retirement consumer");
            }
            child.try_wait()
        }

        #[test]
        #[ignore = "actual outer Job required; unwind across VST3 production retire consumer"]
        fn vst3_discovery_retirement_unwind_releases_or_keeps_actual_staging() {
            for refuse in [false, true] {
                let (mut owner, directory) = sleeping_owner();
                let staging = tempfile::tempdir().expect("VST3 unwind staging");
                let path = staging.path().to_owned();
                owner.staging = Some(staging);
                let hooks: CleanupHooks = if refuse {
                    (
                        panic_once_vst3_observation,
                        refuse_discovery_kill,
                        forbidden_discovery_wait,
                    )
                } else {
                    (panic_once_vst3_observation, Child::kill, Child::wait)
                };
                owner.cleanup_hooks = Some(hooks);
                let observer = retain_process(&owner.child);
                DISCOVERY_WAIT_CALLS.with(|calls| calls.set(0));
                VST3_PANIC_OBSERVATION_ONCE.with(|flag| flag.set(true));
                let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let mut process = owner;
                    super::super::super::vst3_discovery::retire_discovery_failure(
                        &mut process,
                        AudioProcessorHostError::WorkerDeadlineExceeded {
                            operation: "VST3 discovery",
                        },
                    );
                }));
                assert!(
                    unwind.is_err(),
                    "same production retirement boundary propagates injected panic"
                );
                if refuse {
                    assert!(
                        path.exists(),
                        "refused cleanup keeps actual staging even on unwind"
                    );
                    DISCOVERY_WAIT_CALLS.with(|calls| assert_eq!(calls.get(), 0));
                    request_natural_exit(&directory);
                    assert_handle_settled(&observer);
                    assert!(
                        path.is_absolute() && path.parent() == Some(std::env::temp_dir().as_path())
                    );
                    std::fs::remove_dir_all(path).expect("settled retained staging");
                } else {
                    assert_handle_settled(&observer);
                    assert!(
                        !path.exists(),
                        "real kill/reap after unwind releases actual staging"
                    );
                }
            }
        }
    }
}
