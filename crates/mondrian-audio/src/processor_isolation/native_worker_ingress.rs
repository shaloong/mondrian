//! One decoded command slot for the existing authenticated child connection.
//!
//! The reader never touches native objects, author state, or PCM storage. Only
//! one command may be decoded until the main owner acknowledges its response.

use super::native_worker_command::{decode, NativeCommand};
use crate::AudioProcessorHostError;
use std::io::{self, Read};
use std::net::{Shutdown, TcpStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

pub(super) const NATIVE_MAIN_SERVICE_INTERVAL: Duration = Duration::from_millis(8);
const NATIVE_READER_CANCEL_INTERVAL: Duration = Duration::from_millis(100);

struct SlotState<T> {
    value: Option<T>,
    closed: bool,
}

/// An inline fixed slot borrowed by scoped workers; no Arc or hidden queue heap.
/// Poison or closure is explicit and cannot publish a phantom completion.
pub(super) struct FixedSlot<T> {
    state: Mutex<SlotState<T>>,
    ready: Condvar,
}

pub(super) enum NativeSlotReceive<T> {
    Value(T),
    TimedOut,
    Closed,
}

impl<T> FixedSlot<T> {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(SlotState { value: None, closed: false }),
            ready: Condvar::new(),
        }
    }

    pub(super) fn publish(&self, value: T) -> Result<(), AudioProcessorHostError> {
        let mut state = self.state.lock().map_err(|_| failed("native slot", "poisoned state"))?;
        while state.value.is_some() && !state.closed {
            state = self.ready.wait(state).map_err(|_| failed("native slot", "poisoned wait"))?;
        }
        if state.closed {
            return Err(failed("native slot", "owner closed"));
        }
        state.value = Some(value);
        self.ready.notify_all();
        Ok(())
    }

    pub(super) fn take(&self) -> Result<T, AudioProcessorHostError> {
        self.receive_until_closed()?
            .ok_or_else(|| failed("native slot", "owner closed"))
    }

    /// Receive one value, or a deliberate owner closure. Poison remains an error.
    /// Closed state refuses even a queued value from this retired lifetime.
    pub(super) fn receive_until_closed(&self) -> Result<Option<T>, AudioProcessorHostError> {
        let mut state = self.state.lock().map_err(|_| failed("native slot", "poisoned state"))?;
        loop {
            if state.closed {
                return Ok(None);
            }
            if let Some(value) = state.value.take() {
                self.ready.notify_all();
                return Ok(Some(value));
            }
            state = self.ready.wait(state).map_err(|_| failed("native slot", "poisoned wait"))?;
        }
    }

    pub(super) fn take_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Option<T>, AudioProcessorHostError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| failed("native slot", "wait deadline overflowed"))?;
        let mut state = self.state.lock().map_err(|_| failed("native slot", "poisoned state"))?;
        loop {
            if state.closed {
                return Err(failed("native slot", "owner closed"));
            }
            if let Some(value) = state.value.take() {
                self.ready.notify_all();
                return Ok(Some(value));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            state = self
                .ready
                .wait_timeout(state, deadline.duration_since(now))
                .map_err(|_| failed("native slot", "poisoned timed wait"))?
                .0;
        }
    }

    /// One monotonic wait. Closure is not an idle timeout and cannot invoke a
    /// native callback after the scoped owner has requested teardown.
    pub(super) fn receive_timeout(
        &self,
        timeout: Duration,
    ) -> Result<NativeSlotReceive<T>, AudioProcessorHostError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| failed("native slot", "wait deadline overflowed"))?;
        let mut state = self.state.lock().map_err(|_| failed("native slot", "poisoned state"))?;
        loop {
            if state.closed {
                return Ok(NativeSlotReceive::Closed);
            }
            if let Some(value) = state.value.take() {
                self.ready.notify_all();
                return Ok(NativeSlotReceive::Value(value));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(NativeSlotReceive::TimedOut);
            }
            state = self
                .ready
                .wait_timeout(state, deadline.duration_since(now))
                .map_err(|_| failed("native slot", "poisoned timed wait"))?
                .0;
        }
    }

    pub(super) fn close(&self) {
        // Teardown must wake a borrowed worker even if a callback poisoned the
        // state. Further receives refuse; a queued payload is dropped after join.
        let mut state = self.state.lock().unwrap_or_else(|poison| poison.into_inner());
        state.closed = true;
        self.ready.notify_all();
    }
}

pub(super) struct NativeReaderSlots {
    commands: FixedSlot<Result<NativeCommand, AudioProcessorHostError>>,
    completed: FixedSlot<()>,
}

/// Poll the existing owner closure without interrupting a partially decoded
/// frame. Socket timeouts stay inside each read, so read_exact retains its
/// progress through both command headers and bounded payloads.
struct NativeCommandReader<'a> {
    stream: &'a mut TcpStream,
    owner: &'a FixedSlot<Result<NativeCommand, AudioProcessorHostError>>,
}

impl Read for NativeCommandReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            let closed = {
                let state = self
                    .owner
                    .state
                    .lock()
                    .map_err(|_| io::Error::other("native reader slot poisoned"))?;
                state.closed
            };
            if closed {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "native reader owner closed",
                ));
            }
            match self.stream.read(buffer) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                result => return result,
            }
        }
    }
}

pub(super) struct NativeCommandIngress<'a> {
    commands: &'a FixedSlot<Result<NativeCommand, AudioProcessorHostError>>,
    completed: &'a FixedSlot<()>,
}

impl NativeCommandIngress<'_> {
    pub(super) fn receive(&self) -> Result<Option<NativeCommand>, AudioProcessorHostError> {
        self.commands.take_timeout(NATIVE_MAIN_SERVICE_INTERVAL)?.transpose()
    }

    /// Acknowledge only after the sole main writer completed the correlated reply.
    /// The reader then decodes exactly the next command on that same connection.
    pub(super) fn acknowledge(&self) -> Result<(), AudioProcessorHostError> {
        self.completed.publish(())
    }
}

pub(super) fn with_native_ingress<T>(
    control: &mut TcpStream,
    parameter_lanes: usize,
    profile: Option<crate::NativeWorkerLaneResourceProfile>,
    main: impl FnOnce(&mut TcpStream, &NativeCommandIngress<'_>) -> Result<T, AudioProcessorHostError>,
) -> Result<T, AudioProcessorHostError> {
    let mut reader = control.try_clone().map_err(|error| failed("native reader", error))?;
    // This is a cancellation polling interval, not an idle command deadline.
    // The single reader retains partial frames across polls while main keeps
    // its independent bounded message service and the parent its own deadline.
    reader
        .set_read_timeout(Some(NATIVE_READER_CANCEL_INTERVAL))
        .map_err(|error| failed("native reader timeout", error))?;
    let slots = NativeReaderSlots {
        commands: FixedSlot::new(),
        completed: FixedSlot::new(),
    };
    if let Some(profile) = profile {
        let mut actual = profile.owning_layouts();
        let layout = std::alloc::Layout::for_value(&slots);
        actual.reader_slots =
            crate::NativeHostBufferLayout { bytes: layout.size(), alignment: layout.align() };
        let empty = crate::NativeHostBufferLayout { bytes: 0, alignment: 1 };
        profile
            .admit_observed_owners(
                actual,
                crate::NativeWorkerControlOverlapLayout {
                    decoded_input: empty,
                    reply_storage: empty,
                    staged_seed: empty,
                },
            )
            .map_err(AudioProcessorHostError::from)?;
    }
    let commands = &slots.commands;
    let completed = &slots.completed;
    std::thread::scope(|scope| {
        let task = std::thread::Builder::new()
            .name("native-audio-control-reader".to_owned())
            .stack_size(crate::SCOPED_NATIVE_READER_STACK_BYTES)
            .spawn_scoped(scope, || {
                let mut reader = NativeCommandReader { stream: &mut reader, owner: commands };
                let result = catch_unwind(AssertUnwindSafe(|| loop {
                    let command = decode(&mut reader, parameter_lanes).and_then(|command| {
                        if let Some(profile) = profile {
                            let empty = crate::NativeHostBufferLayout { bytes: 0, alignment: 1 };
                            profile
                                .admit_observed_owners(
                                    profile.owning_layouts(),
                                    crate::NativeWorkerControlOverlapLayout {
                                        decoded_input: command.owned_input_layout()?,
                                        reply_storage: empty,
                                        staged_seed: empty,
                                    },
                                )
                                .map_err(AudioProcessorHostError::from)?;
                        }
                        Ok(command)
                    });
                    let stop = command.is_err();
                    if commands.publish(command).is_err() || stop || completed.take().is_err() {
                        return;
                    }
                }));
                if result.is_err() {
                    let _ = commands.publish(Err(failed("native reader", "decoder panicked")));
                }
            })
            .map_err(|error| failed("native reader spawn", error))?;
        let result = catch_unwind(AssertUnwindSafe(|| {
            main(control, &NativeCommandIngress { commands, completed })
        }))
        .unwrap_or(Err(AudioProcessorHostError::AdapterPanicked(
            "native main command service",
        )));
        // Closing slots releases a blocked publish/ack and the reader's next
        // bounded cancellation poll. Read-half shutdown preserves the parent's
        // ability to observe an already-written shutdown/error response.
        commands.close();
        completed.close();
        let shutdown = control.shutdown(Shutdown::Read);
        let joined = task.join();
        let shutdown = match shutdown {
            Err(error) if error.kind() != std::io::ErrorKind::NotConnected => {
                Err(failed("native reader shutdown", error))
            }
            _ => Ok(()),
        };
        let joined = joined.map_err(|_| failed("native reader join", "reader panicked"));
        settle_ingress(result, shutdown, joined)
    })
}

/// No primary error can skip join or hide a separate cancellation failure.
fn settle_ingress<T>(
    result: Result<T, AudioProcessorHostError>,
    shutdown: Result<(), AudioProcessorHostError>,
    joined: Result<(), AudioProcessorHostError>,
) -> Result<T, AudioProcessorHostError> {
    match (result, shutdown, joined) {
        (result, Ok(()), Ok(())) => result,
        (result, shutdown, joined) => {
            let mut detail = String::new();
            if let Err(error) = result {
                detail.push_str(&format!("operation: {error}; "));
            }
            if let Err(error) = shutdown {
                detail.push_str(&format!("reader cancellation: {error}; "));
            }
            if let Err(error) = joined {
                detail.push_str(&format!("reader join: {error}; "));
            }
            Err(failed("native reader lifetime", detail))
        }
    }
}

fn failed(operation: &'static str, detail: impl std::fmt::Display) -> AudioProcessorHostError {
    AudioProcessorHostError::WorkerFailed { operation, detail: detail.to_string() }
}

#[cfg(test)]
mod tests {
    use super::super::protocol::{CommandKind, WorkerCommand};
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn native_fixed_slot_preserves_backpressure_and_close_wakes_writer() {
        let slot = FixedSlot::new();
        slot.publish(1).expect("first value");
        std::thread::scope(|scope| {
            let (entered, observe) = mpsc::channel();
            let borrowed_slot = &slot;
            let writer = scope.spawn(move || {
                entered.send(()).expect("entered signal");
                borrowed_slot.publish(2)
            });
            observe.recv().expect("writer entered");
            assert_eq!(slot.take().expect("first admitted value"), 1);
            assert_eq!(slot.take().expect("second admitted value"), 2);
            writer.join().expect("writer join").expect("both published");
            slot.publish(3).expect("third value");
            let (entered, observe) = mpsc::channel();
            let borrowed_slot = &slot;
            let writer = scope.spawn(move || {
                entered.send(()).expect("entered signal");
                borrowed_slot.publish(4)
            });
            observe.recv().expect("writer entered");
            slot.close();
            assert!(writer.join().expect("writer join").is_err());
            assert!(
                slot.take().is_err(),
                "closed owners cannot leak a queued result into a fresh lifetime"
            );
        });
    }

    #[test]
    fn native_fixed_slot_timeout_does_not_drop_published_value() {
        let slot = FixedSlot::new();
        assert!(slot.take_timeout(Duration::ZERO).expect("empty timeout").is_none());
        slot.publish(9).expect("published");
        assert_eq!(
            slot.take_timeout(Duration::ZERO).expect("ready timeout"),
            Some(9)
        );
        assert!(slot.take_timeout(Duration::ZERO).expect("drained timeout").is_none());
    }

    #[test]
    fn native_reader_does_not_decode_a_second_request_before_first_response_ack() {
        use super::super::protocol::WorkerResponse;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("test endpoint");
        let endpoint = listener.local_addr().expect("test address");
        std::thread::scope(|scope| {
            let parent = scope.spawn(move || {
                let mut socket = TcpStream::connect(endpoint).expect("parent connection");
                socket.set_read_timeout(Some(Duration::from_secs(2))).expect("parent deadline");
                for start_sample in [17, 31] {
                    WorkerCommand {
                        kind: CommandKind::EnterState,
                        start_sample,
                        frames: 0,
                        event_count: 0,
                    }
                    .write_to(&mut socket)
                    .expect("queued parent request");
                }
                for _ in 0..2 {
                    assert_eq!(
                        WorkerResponse::read_from(&mut socket).expect("correlated response"),
                        WorkerResponse::Completed
                    );
                }
            });
            let (mut socket, _) = listener.accept().expect("existing child connection");
            with_native_ingress(&mut socket, 0, None, |writer, ingress| {
                let first = loop {
                    if let Some(command) = ingress.receive()? {
                        break command;
                    }
                };
                let NativeCommand::Plain(first) = first else {
                    panic!("exact plain request");
                };
                assert_eq!(first.start_sample, 17);
                assert!(
                    ingress.receive()?.is_none(),
                    "unacknowledged request must own the only decode slot"
                );
                WorkerResponse::Completed.write_to(writer).expect("first completed response");
                ingress.acknowledge()?;
                let second = loop {
                    if let Some(command) = ingress.receive()? {
                        break command;
                    }
                };
                let NativeCommand::Plain(second) = second else {
                    panic!("exact second request");
                };
                assert_eq!(second.start_sample, 31);
                WorkerResponse::Completed.write_to(writer).expect("second completed response");
                // No next decode is admitted. Deliberate owner teardown wakes
                // the reader waiting for this final response acknowledgement.
                Ok(())
            })
            .expect("joined sole-reader lifetime");
            parent.join().expect("parent completed");
        });
    }

    #[test]
    fn native_reader_owner_cancellation_joins_a_partial_header_read() {
        use std::io::Write;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("test endpoint");
        let endpoint = listener.local_addr().expect("test address");
        std::thread::scope(|scope| {
            let (sent, received) = mpsc::channel();
            let (released, release) = mpsc::channel();
            let parent = scope.spawn(move || {
                let mut socket = TcpStream::connect(endpoint).expect("parent connection");
                socket.write_all(&[77, 65, 85, 68]).expect("only partial command header");
                sent.send(()).expect("partial header published");
                release
                    .recv_timeout(Duration::from_secs(2))
                    .expect("child must finish its owned cancellation");
            });
            let (mut socket, _) = listener.accept().expect("existing child connection");
            received
                .recv_timeout(Duration::from_secs(2))
                .expect("real parent partial header");
            with_native_ingress(&mut socket, 0, None, |_, ingress| {
                assert!(
                    ingress.receive()?.is_none(),
                    "partial bytes cannot produce a phantom command"
                );
                Ok(())
            })
            .expect("cancellation must shut down and join the exact blocked reader");
            released.send(()).expect("native owner joined");
            parent.join().expect("parent completed");
        });
    }

    fn assert_partial_request_survives_cancel_polls(
        wire: Vec<u8>,
        split: usize,
        parameter_lanes: usize,
        check: impl FnOnce(NativeCommand),
    ) {
        use super::super::protocol::WorkerResponse;
        use std::io::Write;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("test endpoint");
        let endpoint = listener.local_addr().expect("test address");
        std::thread::scope(|scope| {
            let (sent, received) = mpsc::channel();
            let (continue_write, continue_read) = mpsc::channel();
            let parent = scope.spawn(move || {
                let mut socket = TcpStream::connect(endpoint).expect("parent connection");
                socket.set_read_timeout(Some(Duration::from_secs(2))).expect("parent deadline");
                socket.write_all(&wire[..split]).expect("partial request");
                sent.send(()).expect("partial request published");
                continue_read.recv_timeout(Duration::from_secs(2)).expect("child polled");
                socket.write_all(&wire[split..]).expect("remaining request");
                assert_eq!(
                    WorkerResponse::read_from(&mut socket).expect("one correlated response"),
                    WorkerResponse::Completed
                );
            });
            let (mut socket, _) = listener.accept().expect("existing child connection");
            received.recv_timeout(Duration::from_secs(2)).expect("real partial request");
            with_native_ingress(&mut socket, parameter_lanes, None, |writer, ingress| {
                let pending = Instant::now();
                let poll_span = NATIVE_READER_CANCEL_INTERVAL * 2 + Duration::from_millis(20);
                while pending.elapsed() < poll_span {
                    assert!(
                        ingress.receive()?.is_none(),
                        "idle polls must retain partial bytes without publishing a command/error"
                    );
                }
                continue_write.send(()).expect("allow exact request completion");
                let command = loop {
                    if let Some(command) = ingress.receive()? {
                        break command;
                    }
                };
                check(command);
                WorkerResponse::Completed.write_to(writer).expect("one completed response");
                ingress.acknowledge()?;
                Ok(())
            })
            .expect("partial request retains one decoder and joins its owner");
            parent.join().expect("parent completed");
        });
    }

    #[test]
    fn native_reader_partial_header_survives_multiple_cancel_polls() {
        let mut wire = Vec::new();
        WorkerCommand {
            kind: CommandKind::EnterState,
            start_sample: 17,
            frames: 0,
            event_count: 0,
        }
        .write_to(&mut wire)
        .expect("current typed header");
        assert_eq!(wire.len(), 32);
        assert_partial_request_survives_cancel_polls(wire, 4, 0, |command| {
            let NativeCommand::Plain(command) = command else {
                panic!("exact plain request");
            };
            assert_eq!(command.kind, CommandKind::EnterState);
            assert_eq!(command.start_sample, 17);
            assert_eq!(command.frames, 0);
            assert_eq!(command.event_count, 0);
        });
    }

    #[test]
    fn native_reader_partial_control_body_survives_multiple_cancel_polls() {
        use super::super::native_configuration_control::{
            NativeConfigurationControlRequest, NativeRequestCohort,
        };
        use super::super::native_configuration_wire;
        use std::num::NonZeroU64;
        let cohort =
            NativeRequestCohort::new(7, mondrian_core::NativeAudioReprepareRequest::ClapRestart)
                .expect("cohort");
        let token = NonZeroU64::new(31).expect("token");
        let values = [-9.25, 0.75];
        let mut wire = Vec::new();
        WorkerCommand {
            kind: CommandKind::ServiceConfiguration,
            start_sample: 0,
            frames: 0,
            event_count: 0,
        }
        .write_to(&mut wire)
        .expect("current typed header");
        native_configuration_wire::write_request(
            &mut wire,
            &NativeConfigurationControlRequest {
                expected_cohort: cohort,
                control_token: token,
                canonical_constant_values: &values,
                configuration_limit_bytes: 32,
                control_buffer_grant_bytes: 64,
            },
        )
        .expect("current bounded control body");
        // Leave only the final half of the second f64 unsent, exercising
        // read_exact progress through a payload as well as the command header.
        let split = wire.len() - 4;
        assert_partial_request_survives_cancel_polls(wire, split, 2, |command| {
            let NativeCommand::Configuration(command) = command else {
                panic!("exact configuration request");
            };
            let owned_value_bytes = command.value_bytes().expect("actual value allocation");
            let request = command.borrow().expect("admitted bounded values");
            assert_eq!(request.expected_cohort, cohort);
            assert_eq!(request.control_token, token);
            assert_eq!(request.canonical_constant_values, &values);
            assert_eq!(request.configuration_limit_bytes, 32);
            assert_eq!(request.control_buffer_grant_bytes, 64 - owned_value_bytes);
        });
    }

    #[test]
    fn native_reader_primary_failure_does_not_hide_independent_cancel_or_join_failure() {
        let original = AudioProcessorHostError::InvalidContract("original admission".into());
        assert_eq!(
            settle_ingress::<()>(Err(original.clone()), Ok(()), Ok(())),
            Err(original)
        );
        let result = settle_ingress::<()>(
            Err(AudioProcessorHostError::InvalidContract(
                "original admission".into(),
            )),
            Err(failed("native reader shutdown", "cancel failure")),
            Err(failed("native reader join", "join failure")),
        )
        .expect_err("all actual lifetime failures remain visible");
        let AudioProcessorHostError::WorkerFailed { operation, detail } = result else {
            panic!("typed lifetime failure");
        };
        assert_eq!(operation, "native reader lifetime");
        assert!(detail.contains("original admission"));
        assert!(detail.contains("cancel failure"));
        assert!(detail.contains("join failure"));
    }
}
