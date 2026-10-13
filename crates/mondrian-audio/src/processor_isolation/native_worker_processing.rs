//! Persistent scoped DSP with one job/completion slot and actual main service.
//!
//! This is the processing lane of the same worker connection, not a native
//! editor owner, second processor or a second root-window scheduler.

use super::native_worker_ingress::{FixedSlot, NativeSlotReceive, NATIVE_MAIN_SERVICE_INTERVAL};
use super::protocol::{CommandKind, WorkerCommand};
use super::worker::{WorkerProcessCompletion, WorkerProcessContext};
use super::{IsolatedAudioProcessorMainRole, IsolatedAudioProcessorWorkerSplit};
use crate::{AudioProcessingMode, AudioProcessorHostError};
use mondrian_audio_scheduling::{
    RealtimeAudioThreadEvidence, RealtimeAudioThreadExitEvidence, RealtimeAudioThreadLease,
    RealtimeAudioThreadRole,
};
use std::cell::Cell;
use std::panic::{catch_unwind, AssertUnwindSafe};

enum NativeDspJob {
    Pcm(WorkerCommand),
    AuthorParameterFlush,
}

enum NativeDspCompletion {
    Pcm(WorkerProcessCompletion),
    AuthorParameterFlush(crate::NativeAudioAuthorParameterFlushOutcome),
}

pub(super) struct NativeProcessingSlots {
    jobs: FixedSlot<NativeDspJob>,
    completed: FixedSlot<Result<NativeDspCompletion, AudioProcessorHostError>>,
    ready: FixedSlot<Result<Option<RealtimeAudioThreadEvidence>, AudioProcessorHostError>>,
}

/// Main-only dispatcher of the sole shared PCM borrow in this scope.
/// The inflight flag is a transport exclusion rule, not author/runtime authority.
pub(super) struct NativeDspDispatch<'a> {
    jobs: &'a FixedSlot<NativeDspJob>,
    completed: &'a FixedSlot<Result<NativeDspCompletion, AudioProcessorHostError>>,
    inflight: Cell<bool>,
    scheduling_entry: Option<RealtimeAudioThreadEvidence>,
}

impl NativeDspDispatch<'_> {
    /// The actual DSP thread's successful native entry, or Offline None.
    /// This source fact is not a deadline, author or root-window permission.
    pub(super) fn scheduling_entry(&self) -> Option<RealtimeAudioThreadEvidence> {
        self.scheduling_entry
    }

    pub(super) fn submit(&self, command: WorkerCommand) -> Result<(), AudioProcessorHostError> {
        if command.kind != CommandKind::Process {
            return Err(failed("only a Process command may enter the DSP role"));
        }
        if self.inflight.get() {
            return Err(failed("shared PCM already has an outstanding DSP block"));
        }
        self.jobs.publish(NativeDspJob::Pcm(command))?;
        self.inflight.set(true);
        Ok(())
    }

    pub(super) fn submit_author_flush(&self) -> Result<(), AudioProcessorHostError> {
        if self.inflight.get() {
            return Err(failed("native DSP job already owns the completion slot"));
        }
        self.jobs.publish(NativeDspJob::AuthorParameterFlush)?;
        self.inflight.set(true);
        Ok(())
    }

    pub(super) fn complete_author_flush(
        &self,
        main: &mut dyn IsolatedAudioProcessorMainRole,
    ) -> Result<crate::NativeAudioAuthorParameterFlushOutcome, AudioProcessorHostError> {
        match self.receive_completion(main)? {
            NativeDspCompletion::AuthorParameterFlush(outcome) => Ok(outcome),
            NativeDspCompletion::Pcm(_) => {
                Err(failed("PCM completion cannot settle an author control job"))
            }
        }
    }

    pub(super) fn complete(
        &self,
        main: &mut dyn IsolatedAudioProcessorMainRole,
    ) -> Result<WorkerProcessCompletion, AudioProcessorHostError> {
        match self.receive_completion(main)? {
            NativeDspCompletion::Pcm(completion) => Ok(completion),
            NativeDspCompletion::AuthorParameterFlush(_) => {
                Err(failed("author control completion cannot publish PCM"))
            }
        }
    }

    /// Await the actual completion while continuing the same main role's service.
    /// UI/pending observations cannot truncate an already admitted root window.
    fn receive_completion(
        &self,
        main: &mut dyn IsolatedAudioProcessorMainRole,
    ) -> Result<NativeDspCompletion, AudioProcessorHostError> {
        if !self.inflight.get() {
            return Err(failed("no DSP block owns a completion"));
        }
        loop {
            main.service_native_main_thread()?;
            if let Some(result) = self.completed.take_timeout(NATIVE_MAIN_SERVICE_INTERVAL)? {
                self.inflight.set(false);
                return result;
            }
        }
    }
}

/// Actual joined DSP scope result; explicit same-thread scheduler restoration
/// is retained as an exit fact. This is not a physical callback deadline proof.
pub(super) struct NativeDspScopeOutcome<T> {
    pub(super) value: T,
    pub(super) scheduling_exit: Option<RealtimeAudioThreadExitEvidence>,
}

/// Run a native processing scope using the canonical audio scheduling adapter.
///
/// RT requires real NativeDsp entry on this DSP thread before ready/first Process.
/// Offline performs no registration. Non-Windows RT admission is explicitly
/// UnsupportedPlatform until a platform policy/adapter is qualified; no ordinary
/// thread is silently reported as a successful native realtime registration.
/// Main/UI/reader threads never acquire this lease. The child consumer must
/// explicitly carry entry/exit facts before parent qualification can claim them.
pub(super) fn with_processing_scope<T>(
    split: IsolatedAudioProcessorWorkerSplit<'_>,
    profile: crate::NativeWorkerLaneResourceProfile,
    shared: &mut [u8],
    context: WorkerProcessContext<'_>,
    main: impl FnOnce(
        &mut dyn IsolatedAudioProcessorMainRole,
        &NativeDspDispatch<'_>,
    ) -> Result<T, AudioProcessorHostError>,
) -> Result<NativeDspScopeOutcome<T>, AudioProcessorHostError> {
    let slots = NativeProcessingSlots {
        jobs: FixedSlot::new(),
        completed: FixedSlot::new(),
        ready: FixedSlot::new(),
    };
    let jobs = &slots.jobs;
    let completed = &slots.completed;
    let ready = &slots.ready;
    let mut main_role = split.main;
    let mut processor = split.processing;
    let [runtime_main, sdk_main] = main_role
        .allocation_layouts()
        .ok_or_else(|| failed("actual native main role cannot report its complete Box layouts"))?;
    let [runtime_dsp, sdk_dsp] = processor
        .allocation_layouts()
        .ok_or_else(|| failed("actual native DSP role cannot report its complete Box layouts"))?;
    let mut actual = profile.owning_layouts();
    let slots_layout = std::alloc::Layout::for_value(&slots);
    actual.processing_slots = crate::NativeHostBufferLayout {
        bytes: slots_layout.size(),
        alignment: slots_layout.align(),
    };
    actual.runtime_main_role = runtime_main;
    actual.runtime_dsp_role = runtime_dsp;
    actual.sdk_main_role = sdk_main;
    actual.sdk_dsp_role = sdk_dsp;
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
    std::thread::scope(|scope| {
        let worker = std::thread::Builder::new()
            .name("native-audio-dsp".to_owned())
            .stack_size(crate::SCOPED_NATIVE_DSP_STACK_BYTES)
            .spawn_scoped(scope, || {
                let mut announced = false;
                let service = catch_unwind(AssertUnwindSafe(|| {
                    let schedule = match context.render_contract.processing_mode {
                        AudioProcessingMode::Realtime => Some(
                            RealtimeAudioThreadLease::enter(RealtimeAudioThreadRole::NativeDsp)
                                .map_err(AudioProcessorHostError::from)?,
                        ),
                        AudioProcessingMode::Offline => None,
                    };
                    let run = catch_unwind(AssertUnwindSafe(|| {
                        let idle_interval = processor.idle_service_interval();
                        if idle_interval.is_some_and(|interval| interval.is_zero()) {
                            return Err(failed(
                                "native DSP idle service requires a positive interval",
                            ));
                        }
                        ready.publish(Ok(schedule
                            .as_ref()
                            .map(RealtimeAudioThreadLease::evidence)))?;
                        announced = true;
                        loop {
                            let command = if let Some(interval) = idle_interval {
                                match jobs.receive_timeout(interval)? {
                                    NativeSlotReceive::Value(command) => command,
                                    NativeSlotReceive::Closed => break,
                                    NativeSlotReceive::TimedOut => {
                                        processor.service_native_idle()?;
                                        continue;
                                    }
                                }
                            } else {
                                let Some(command) = jobs.receive_until_closed()? else {
                                    break;
                                };
                                command
                            };
                            let result = match command {
                                NativeDspJob::Pcm(command) => context
                                    .process(|block| processor.process(block), shared, command)
                                    .map(NativeDspCompletion::Pcm),
                                NativeDspJob::AuthorParameterFlush => processor
                                    .flush_native_author_parameters()
                                    .map(NativeDspCompletion::AuthorParameterFlush),
                            };
                            completed.publish(result)?;
                        }
                        Ok::<(), AudioProcessorHostError>(())
                    }))
                    .unwrap_or(Err(
                        AudioProcessorHostError::AdapterPanicked("native DSP scope"),
                    ));
                    // This same actual DSP thread explicitly restores scheduling,
                    // including after processing or publication fails. A run error
                    // must not hide a separate scheduler restoration error.
                    let restored = schedule
                        .map(RealtimeAudioThreadLease::finish)
                        .transpose()
                        .map_err(AudioProcessorHostError::from);
                    settle_scope_result(
                        run,
                        restored.as_ref().map(|_| ()).map_err(Clone::clone),
                        Ok(()),
                    )?;
                    restored
                }))
                .unwrap_or(Err(AudioProcessorHostError::AdapterPanicked(
                    "native DSP entry/exit",
                )));
                if !announced {
                    let failure = service
                        .as_ref()
                        .err()
                        .cloned()
                        .unwrap_or_else(|| failed("DSP exited without readiness"));
                    let _ = ready.publish(Err(failure));
                } else if service.is_err() {
                    // Wake an outstanding completion waiter without publishing
                    // a fictitious successful PCM result. The join still carries
                    // the exact failure, including failed scheduler restoration.
                    completed.close();
                }
                service
            });
        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                let removed = finish_main_role(main_role.as_mut());
                return settle_scope_result(
                    Err(failed(&format!("DSP thread spawn failed: {error}"))),
                    Ok(()),
                    removed,
                );
            }
        };
        let readiness = ready.take().and_then(|ready| ready);
        let dispatch = NativeDspDispatch {
            jobs,
            completed,
            inflight: Cell::new(false),
            scheduling_entry: readiness.as_ref().ok().copied().flatten(),
        };
        // No early `?`: readiness, main callback, thread restoration and native
        // main role completion all pass through the same join/finish path.
        let result = readiness.and_then(|_| {
            let result = catch_unwind(AssertUnwindSafe(|| main(main_role.as_mut(), &dispatch)))
                .unwrap_or(Err(AudioProcessorHostError::AdapterPanicked(
                    "native main DSP dispatch",
                )));
            if result.is_ok() && dispatch.inflight.get() {
                Err(failed(
                    "native main scope exited before its actual DSP completion",
                ))
            } else {
                result
            }
        });
        jobs.close();
        completed.close();
        ready.close();
        // An actual native Process must return before any borrowed storage/view
        // can be released. Supervisor deadline kills a stuck child; this scope
        // cannot substitute opaque getState for an actual Process completion.
        let joined = worker
            .join()
            .map_err(|_| AudioProcessorHostError::AdapterPanicked("native DSP join"))
            .and_then(|result| result);
        let finished_main = finish_main_role(main_role.as_mut());
        let value = settle_scope_result(
            result,
            joined.as_ref().map(|_| ()).map_err(Clone::clone),
            finished_main,
        )?;
        Ok(NativeDspScopeOutcome { value, scheduling_exit: joined? })
    })
}

fn finish_main_role(
    main: &mut dyn IsolatedAudioProcessorMainRole,
) -> Result<(), AudioProcessorHostError> {
    catch_unwind(AssertUnwindSafe(|| main.finish_processing_scope())).unwrap_or(Err(
        AudioProcessorHostError::AdapterPanicked("native main role finish"),
    ))
}

/// Keep the original error when cleanup succeeds; otherwise report every exit
/// failure. An RT restoration or native detach failure must not disappear behind
/// the earlier operation error and cannot authorize native object reuse.
fn settle_scope_result<T>(
    result: Result<T, AudioProcessorHostError>,
    dsp_exit: Result<(), AudioProcessorHostError>,
    main_exit: Result<(), AudioProcessorHostError>,
) -> Result<T, AudioProcessorHostError> {
    match (result, dsp_exit, main_exit) {
        (result, Ok(()), Ok(())) => result,
        (Err(error), Err(dsp_error), Ok(())) if error == dsp_error => Err(error),
        (result, dsp_exit, main_exit) => {
            let mut detail = String::new();
            if let Err(error) = result {
                detail.push_str(&format!("operation: {error}; "));
            }
            if let Err(error) = dsp_exit {
                detail.push_str(&format!("DSP exit: {error}; "));
            }
            if let Err(error) = main_exit {
                detail.push_str(&format!("native main exit: {error}; "));
            }
            Err(failed(&detail))
        }
    }
}

fn failed(detail: &str) -> AudioProcessorHostError {
    AudioProcessorHostError::WorkerFailed {
        operation: "native processing scope",
        detail: detail.to_owned(),
    }
}
