use super::protocol::{
    auxiliary_from_wire, auxiliary_to_wire, mode_is_realtime, read_json_frame, write_json_frame,
    ExecutionContractWire, PrepareWire, ReadyResultWire, ReadyWire, SharedBlockLayout,
    WorkerCommand, WorkerResponse, PROTOCOL_VERSION,
};
use super::{IsolatedAudioProcessorWorkerSpec, ISOLATED_AUDIO_PROCESSOR_ENDPOINT_ENV};
use crate::{AudioProcessorHostError, AudioRenderContract};
use memmap2::{MmapMut, MmapOptions};
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;
use uuid::Uuid;

pub(super) struct SupervisedAudioWorker {
    child: Child,
    control: TcpStream,
    shared: MmapMut,
    _shared_file: NamedTempFile,
    layout: SharedBlockLayout,
    operation_timeout: Duration,
    state_entry_timeout: Duration,
    configured_control_timeout: Option<Duration>,
    active: bool,
}

impl SupervisedAudioWorker {
    pub(super) fn launch(
        spec: &IsolatedAudioProcessorWorkerSpec,
        contract: AudioRenderContract,
        layout: SharedBlockLayout,
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
        // producing typed views, and the parent keeps the file alive until the
        // Worker has been terminated and reaped.
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
        let mut child = command.spawn().map_err(|error| worker_failed("startup", error))?;
        let startup_deadline = Instant::now()
            .checked_add(spec.startup_timeout)
            .ok_or_else(|| invalid_contract("Worker startup deadline overflowed"))?;
        let mut control = match accept_worker(&listener, &mut child, startup_deadline) {
            Ok(control) => control,
            Err(error) => {
                terminate_and_reap(&mut child);
                return Err(error);
            }
        };
        let remaining = startup_deadline.saturating_duration_since(Instant::now());
        if let Err(error) = set_stream_timeout(&control, remaining.max(Duration::from_millis(1))) {
            terminate_and_reap(&mut child);
            return Err(worker_failed("startup", error));
        }
        let prepare = PrepareWire {
            schema_version: PROTOCOL_VERSION,
            nonce,
            payload: spec.preparation_payload.clone(),
            execution_contract: ExecutionContractWire::capture(spec.plugin_execution_contract)?,
            auxiliary_buses: auxiliary_to_wire(&spec.auxiliary_inputs),
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
        let ready = (|| -> io::Result<ReadyWire> {
            write_json_frame(&mut control, &prepare)?;
            read_json_frame(&mut control)
        })();
        let ready = match ready {
            Ok(ready) => ready,
            Err(error) => {
                terminate_and_reap(&mut child);
                return Err(classify_io("startup handshake", error));
            }
        };
        if ready.schema_version != PROTOCOL_VERSION {
            terminate_and_reap(&mut child);
            return Err(worker_failed_detail(
                "startup",
                "Worker replied with another protocol version",
            ));
        }
        let input_names = match ready.result {
            ReadyResultWire::Ready { execution_contract, auxiliary_buses, input_names } => {
                let worker_contract = match execution_contract.realize() {
                    Ok(contract) => contract,
                    Err(error) => {
                        terminate_and_reap(&mut child);
                        return Err(error);
                    }
                };
                let worker_auxiliary = auxiliary_from_wire(auxiliary_buses);
                if worker_contract != spec.plugin_execution_contract
                    || worker_auxiliary != spec.auxiliary_inputs
                {
                    terminate_and_reap(&mut child);
                    return Err(worker_failed_detail(
                        "startup",
                        "Worker preparation contract drifted from the admitted parent contract",
                    ));
                }
                if let Err(error) =
                    crate::AudioProcessorInputName::validate_all(&input_names, &worker_auxiliary)
                {
                    terminate_and_reap(&mut child);
                    return Err(error);
                }
                input_names
            }
            ReadyResultWire::Rejected { detail } => {
                terminate_and_reap(&mut child);
                return Err(worker_failed_detail("startup", &detail));
            }
        };
        if let Err(error) = set_stream_timeout(&control, spec.operation_timeout) {
            terminate_and_reap(&mut child);
            return Err(worker_failed("startup", error));
        }
        Ok((
            Self {
                child,
                control,
                shared,
                _shared_file: shared_file,
                layout,
                operation_timeout: spec.operation_timeout,
                state_entry_timeout: spec.state_entry_timeout,
                configured_control_timeout: Some(spec.operation_timeout),
                active: true,
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
    ) -> Result<(), AudioProcessorHostError> {
        if !self.active {
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
            Ok(WorkerResponse::Completed) => Ok(()),
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

    pub(super) fn shutdown(&mut self) -> bool {
        if !self.active {
            return true;
        }
        let timeout = self.operation_timeout.min(Duration::from_millis(100));
        if self.configure_control_timeout(timeout.max(Duration::from_millis(1))).is_err() {
            self.terminate();
            return false;
        }
        let completed = WorkerCommand {
            kind: super::protocol::CommandKind::Shutdown,
            start_sample: 0,
            frames: 0,
            event_count: 0,
        }
        .write_to(&mut self.control)
        .and_then(|_| WorkerResponse::read_from(&mut self.control))
        .is_ok_and(|response| response == WorkerResponse::Completed);
        self.active = false;
        if completed {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                match self.child.try_wait() {
                    Ok(Some(status)) => return status.success(),
                    Ok(None) => thread::yield_now(),
                    Err(_) => break,
                }
            }
        }
        terminate_and_reap(&mut self.child);
        false
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
        if self.active {
            self.active = false;
            terminate_and_reap(&mut self.child);
        }
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
            match self.child.try_wait() {
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

fn terminate_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
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
        let (worker, names) =
            SupervisedAudioWorker::launch(&spec, render, SharedBlockLayout::new(4, 2, 0, 0)?)?;
        assert!(names.is_empty());
        Ok(worker)
    }

    fn command(kind: super::super::protocol::CommandKind, sample: i64) -> WorkerCommand {
        WorkerCommand {
            kind,
            start_sample: sample,
            frames: if kind == super::super::protocol::CommandKind::Process {
                4
            } else {
                0
            },
            event_count: 0,
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

        worker.invoke(
            "qualification state entry",
            command(super::super::protocol::CommandKind::EnterState, 0),
        )?;
        assert_eq!(worker.configured_control_timeout, Some(enter));
        assert_eq!(worker.control.read_timeout()?, Some(enter));
        assert_eq!(worker.control.write_timeout()?, Some(enter));
        for block in 0..8 {
            worker.invoke(
                "qualification process",
                command(super::super::protocol::CommandKind::Process, block * 4),
            )?;
            assert_eq!(worker.configured_control_timeout, Some(operation));
            assert_eq!(worker.control.read_timeout()?, Some(operation));
            assert_eq!(worker.control.write_timeout()?, Some(operation));
            assert!(worker.control.nodelay()?);
        }
        // Switch back to the long state budget before shutdown, exercising its
        // real transition to the existing shorter control budget.
        worker.invoke(
            "qualification state entry",
            command(super::super::protocol::CommandKind::EnterState, 100),
        )?;
        let _completed_gracefully = worker.shutdown();
        assert!(!worker.active);
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
        assert!(worker.child.try_wait()?.is_some());
        Ok(())
    }

    #[test]
    fn native_control_timeout_actual_child_equal_budgets_remain_installed_through_shutdown(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let budget = Duration::from_millis(100);
        let mut worker = launch_existing_test_child(budget, budget)?;
        worker.invoke(
            "qualification state entry",
            command(super::super::protocol::CommandKind::EnterState, 0),
        )?;
        worker.invoke(
            "qualification process",
            command(super::super::protocol::CommandKind::Process, 0),
        )?;
        let _completed_gracefully = worker.shutdown();
        assert!(!worker.active);
        assert_eq!(worker.configured_control_timeout, Some(budget));
        assert_eq!(worker.control.read_timeout()?, Some(budget));
        assert_eq!(worker.control.write_timeout()?, Some(budget));
        assert!(worker.child.try_wait()?.is_some());
        Ok(())
    }

    #[test]
    fn native_control_timeout_actual_child_drop_retires_connection_after_enter(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut worker =
            launch_existing_test_child(Duration::from_secs(1), Duration::from_secs(30))?;
        worker.invoke(
            "qualification state entry",
            command(super::super::protocol::CommandKind::EnterState, 0),
        )?;
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
}
