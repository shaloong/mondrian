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
    active: bool,
}

impl SupervisedAudioWorker {
    pub(super) fn launch(
        spec: &IsolatedAudioProcessorWorkerSpec,
        contract: AudioRenderContract,
        layout: SharedBlockLayout,
    ) -> Result<Self, AudioProcessorHostError> {
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
        match ready.result {
            ReadyResultWire::Ready { execution_contract, auxiliary_buses } => {
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
            }
            ReadyResultWire::Rejected { detail } => {
                terminate_and_reap(&mut child);
                return Err(worker_failed_detail("startup", &detail));
            }
        }
        if let Err(error) = set_stream_timeout(&control, spec.operation_timeout) {
            terminate_and_reap(&mut child);
            return Err(worker_failed("startup", error));
        }
        Ok(Self {
            child,
            control,
            shared,
            _shared_file: shared_file,
            layout,
            operation_timeout: spec.operation_timeout,
            active: true,
        })
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
        let _ = set_stream_timeout(&self.control, timeout.max(Duration::from_millis(1)));
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
    stream.set_write_timeout(Some(timeout))?;
    stream.set_nodelay(true)
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
