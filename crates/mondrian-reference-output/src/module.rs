use std::collections::VecDeque;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::{
    ReferenceOutputAdapter, ReferenceOutputAdapterError, ReferenceOutputAdapterEvent,
    ReferenceOutputAdapterSession, ReferenceOutputBundle, ReferenceOutputDeviceDescriptor,
    ReferenceOutputDeviceId, ReferenceOutputHardwareTime, ReferenceOutputOpenRequest,
    ReferenceOutputProviderEvidence, ReferenceOutputProviderShutdownFailure,
    ReferenceOutputReferencePolicy, ReferenceOutputSessionShutdownReceipt,
    ReferenceOutputShutdownCoordinatorFacts,
};

/// Product-visible lifecycle of one Reference Output Module instance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceOutputState {
    /// No device ownership or scheduled output.
    #[default]
    Disabled,
    /// Device is open and accumulating complete preroll.
    Priming,
    /// Scheduled playout is active.
    Running,
    /// Exact capability or external runtime condition prevents output.
    Blocked,
    /// Provider execution failed after admission.
    Failed,
    /// Session stopped cleanly and released ownership.
    Stopped,
}

/// Cumulative bounded-session evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceOutputDiagnostics {
    /// Diagnostics schema version.
    pub schema_version: u32,
    /// Current lifecycle state.
    pub state: ReferenceOutputState,
    /// Provider/runtime evidence, when discovery has occurred.
    pub provider: Option<ReferenceOutputProviderEvidence>,
    /// Open stable device identity.
    pub device_id: Option<ReferenceOutputDeviceId>,
    /// Captured device/profile generation.
    pub device_generation: Option<u64>,
    /// Bundles accepted into the provider queue.
    pub scheduled_frames: u64,
    /// Bundles completed by the provider.
    pub completed_frames: u64,
    /// Provider late-frame callbacks.
    pub late_frames: u64,
    /// Provider dropped-frame callbacks.
    pub dropped_frames: u64,
    /// Provider flushed-frame callbacks.
    pub flushed_frames: u64,
    /// Queued frames explicitly aborted by stop, block, or failure.
    pub aborted_frames: u64,
    /// Current frames awaiting one terminal provider callback.
    pub outstanding_frames: u64,
    /// Total provider callback/status events consumed.
    pub callback_events: u64,
    /// Exact embedded-audio sample frames accepted with video.
    pub scheduled_audio_frames: u64,
    /// Canonical ancillary packets accepted atomically with video/audio.
    pub scheduled_ancillary_packets: u64,
    /// Complete ST 291 words accepted, including ADF/checksum overhead.
    pub scheduled_ancillary_words: u64,
    /// Completed frames whose actual ANC inventory digest matched the schedule.
    pub verified_ancillary_readbacks: u64,
    /// Highest simultaneous scheduled queue depth.
    pub scheduled_high_water: u32,
    /// External-reference lock-loss transitions after a positive lock.
    pub reference_lock_losses: u64,
    /// Completion callbacks carrying valid hardware-clock evidence.
    pub hardware_timestamp_callbacks: u64,
    /// Invalid, regressing, or rate-changing hardware timestamps.
    pub hardware_time_failures: u64,
    /// First valid provider hardware timestamp.
    pub first_hardware_time: Option<ReferenceOutputHardwareTime>,
    /// Latest valid provider hardware timestamp.
    pub last_hardware_time: Option<ReferenceOutputHardwareTime>,
    /// Largest adjacent hardware-clock gap in ticks.
    pub maximum_hardware_time_gap_ticks: u64,
    /// Latest continuous external reference status.
    pub reference_locked: Option<bool>,
    /// Most recent stable blocker/failure detail.
    pub last_error: Option<String>,
}

impl Default for ReferenceOutputDiagnostics {
    fn default() -> Self {
        Self {
            schema_version: 1,
            state: ReferenceOutputState::Disabled,
            provider: None,
            device_id: None,
            device_generation: None,
            scheduled_frames: 0,
            completed_frames: 0,
            late_frames: 0,
            dropped_frames: 0,
            flushed_frames: 0,
            aborted_frames: 0,
            outstanding_frames: 0,
            callback_events: 0,
            scheduled_audio_frames: 0,
            scheduled_ancillary_packets: 0,
            scheduled_ancillary_words: 0,
            verified_ancillary_readbacks: 0,
            scheduled_high_water: 0,
            reference_lock_losses: 0,
            hardware_timestamp_callbacks: 0,
            hardware_time_failures: 0,
            first_hardware_time: None,
            last_hardware_time: None,
            maximum_hardware_time_gap_ticks: 0,
            reference_locked: None,
            last_error: None,
        }
    }
}

/// Consuming shutdown evidence for one Reference Output Module owner.
///
/// The receipt combines the provider Session's terminal resource facts with
/// cumulative scheduler diagnostics and the queue depth observed before the
/// Module relinquished its own scheduling authority. On schema 2 bounded
/// shutdown, the nested coordinator facts cover consumption and destruction of
/// the entire Module owner, including its Adapter/bridge, not only the Session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceOutputModuleShutdownReceipt {
    /// Receipt schema version.
    pub schema_version: u32,
    /// Provider Session shutdown evidence, or a clean never-opened record.
    pub session: ReferenceOutputSessionShutdownReceipt,
    /// Cumulative diagnostics after shutdown accounting was finalized.
    pub diagnostics: ReferenceOutputDiagnostics,
    /// Module-owned scheduled frames observed when shutdown began.
    pub outstanding_frames_before_shutdown: u64,
    /// Module accounting failure encountered while finalizing shutdown.
    pub module_failure: Option<String>,
}

/// Completion of an ordinary product stop coordinated away from the caller.
///
/// A clean stop returns the reusable Module, including its discovery Adapter.
/// Any provider, coordinator, or accounting failure consumes that Module on
/// the coordinator and returns terminal fail-closed evidence instead. This
/// prevents a caller from accidentally reusing an owner whose device release
/// was not positively proven.
pub enum ReferenceOutputModuleStopOutcome<A>
where
    A: ReferenceOutputAdapter + 'static,
{
    /// The Session was consumed cleanly and the Adapter can be reused.
    Stopped(Box<ReferenceOutputModule<A>>),
    /// The Module could not be recovered with proven resource closure.
    Terminal(Box<ReferenceOutputModuleShutdownReceipt>),
}

impl<A> ReferenceOutputModuleStopOutcome<A>
where
    A: ReferenceOutputAdapter + 'static,
{
    fn stopped(module: ReferenceOutputModule<A>) -> Self {
        Self::Stopped(Box::new(module))
    }

    fn terminal(receipt: ReferenceOutputModuleShutdownReceipt) -> Self {
        Self::Terminal(Box::new(receipt))
    }
}

/// In-flight ordinary product stop for one complete Module owner.
///
/// The coordinator owns Session consumption. Dropping this value never joins
/// the provider worker and never destroys a returned Module on the dropping
/// thread; an unobserved completion is handed to a detached reaper instead.
pub struct ReferenceOutputModuleStopCoordinator<A>
where
    A: ReferenceOutputAdapter + 'static,
{
    handle: Option<thread::JoinHandle<ReferenceOutputModuleStopOutcome<A>>>,
    immediate_failure: Option<ReferenceOutputModuleShutdownReceipt>,
    panic_failure: Option<ReferenceOutputModuleShutdownReceipt>,
    timeout_failure: Option<ReferenceOutputModuleShutdownReceipt>,
    shutdown_request_admitted: bool,
    completed_at: Arc<Mutex<Option<Instant>>>,
}

impl<A> ReferenceOutputModuleStopCoordinator<A>
where
    A: ReferenceOutputAdapter + 'static,
{
    /// Whether coordinator completion can be consumed without waiting.
    pub fn is_finished(&self) -> bool {
        self.immediate_failure.is_some()
            || self.handle.as_ref().is_none_or(thread::JoinHandle::is_finished)
    }

    /// Whether the provider accepted the non-blocking shutdown request.
    ///
    /// A false value does not return ownership to the caller: the coordinator
    /// still consumes the fail-closed Session and produces terminal evidence.
    pub const fn shutdown_request_admitted(&self) -> bool {
        self.shutdown_request_admitted
    }

    /// Observe completion only until one shared absolute deadline.
    ///
    /// Completion wins at the deadline boundary. A still-running worker is
    /// transferred to a detached reaper so its eventual Module/Adapter Drop
    /// cannot run on this caller.
    pub fn finish_until(mut self, deadline: Instant) -> ReferenceOutputModuleStopOutcome<A> {
        if let Some(receipt) = self.immediate_failure.take() {
            return ReferenceOutputModuleStopOutcome::terminal(receipt);
        }
        let Some(handle) = self.handle.take() else {
            return ReferenceOutputModuleStopOutcome::terminal(
                self.panic_failure.take().unwrap_or_else(|| {
                    missing_stop_coordinator_receipt("ordinary stop coordinator owner is missing")
                }),
            );
        };

        let mut handle = Some(handle);
        loop {
            if handle.as_ref().is_some_and(thread::JoinHandle::is_finished) {
                let Some(handle) = handle.take() else {
                    return ReferenceOutputModuleStopOutcome::terminal(
                        missing_stop_coordinator_receipt(
                            "ordinary stop coordinator finished without an owned handle",
                        ),
                    );
                };
                let Some(completed_at) = completion_time(&self.completed_at) else {
                    let _reaper_started = detach_stop_coordinator(handle);
                    return ReferenceOutputModuleStopOutcome::terminal(
                        missing_stop_coordinator_receipt(
                            "ordinary stop coordinator finished without a completion timestamp",
                        ),
                    );
                };
                if completed_at > deadline {
                    return self.timeout_outcome(handle);
                }
                return match handle.join() {
                    Ok(outcome) => mark_stop_coordinator_joined(outcome),
                    Err(_) => ReferenceOutputModuleStopOutcome::terminal(
                        self.panic_failure.take().unwrap_or_else(|| {
                            missing_stop_coordinator_receipt(
                                "ordinary stop coordinator panic receipt is missing",
                            )
                        }),
                    ),
                };
            }

            let now = Instant::now();
            if now >= deadline {
                if handle.as_ref().is_some_and(thread::JoinHandle::is_finished) {
                    continue;
                }
                let Some(handle) = handle.take() else {
                    return ReferenceOutputModuleStopOutcome::terminal(
                        missing_stop_coordinator_receipt(
                            "ordinary stop coordinator timed out without an owned handle",
                        ),
                    );
                };
                return self.timeout_outcome(handle);
            }
            thread::sleep((deadline - now).min(Duration::from_millis(1)));
        }
    }

    fn timeout_outcome(
        &mut self,
        handle: thread::JoinHandle<ReferenceOutputModuleStopOutcome<A>>,
    ) -> ReferenceOutputModuleStopOutcome<A> {
        let reaper_started = detach_stop_coordinator(handle);
        let mut receipt = self.timeout_failure.take().unwrap_or_else(|| {
            missing_stop_coordinator_receipt("ordinary stop coordinator timeout receipt is missing")
        });
        if !reaper_started {
            receipt.session.coordinator.owner_abandoned = true;
            append_shutdown_failure(
                &mut receipt.module_failure,
                "ordinary stop reaper could not be spawned; coordinator owner was abandoned"
                    .to_owned(),
            );
        }
        ReferenceOutputModuleStopOutcome::terminal(receipt)
    }
}

impl<A> Drop for ReferenceOutputModuleStopCoordinator<A>
where
    A: ReferenceOutputAdapter + 'static,
{
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _reaper_started = detach_stop_coordinator(handle);
        }
    }
}

impl ReferenceOutputModuleShutdownReceipt {
    /// Whether both Module and provider facts prove complete resource release.
    pub const fn all_resources_released(&self) -> bool {
        self.schema_version == 2
            && self.session.all_resources_released()
            && self.diagnostics.outstanding_frames == 0
            && self.module_failure.is_none()
    }
}

/// Deep scheduler/lifecycle Module over one physical-provider Adapter.
pub struct ReferenceOutputModule<A> {
    adapter: A,
    session: Option<Box<dyn ReferenceOutputAdapterSession>>,
    request: Option<ReferenceOutputOpenRequest>,
    scheduled: VecDeque<ScheduledBundleEvidence>,
    next_frame_index: u64,
    diagnostics: ReferenceOutputDiagnostics,
    shutdown_request_attempted: bool,
    shutdown_request_failure: Option<ReferenceOutputProviderShutdownFailure>,
    shutdown_module_failure: Option<String>,
    outstanding_frames_at_shutdown_request: Option<u64>,
    completed_stop_session: Option<ReferenceOutputSessionShutdownReceipt>,
    completed_stop_outstanding_frames: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScheduledBundleEvidence {
    frame_index: u64,
    ancillary_sha256: [u8; 32],
}

struct CompletionStamp {
    completed_at: Arc<Mutex<Option<Instant>>>,
}

impl Drop for CompletionStamp {
    fn drop(&mut self) {
        match self.completed_at.lock() {
            Ok(mut completed_at) => *completed_at = Some(Instant::now()),
            Err(poisoned) => *poisoned.into_inner() = Some(Instant::now()),
        }
    }
}

impl<A> ReferenceOutputModule<A>
where
    A: ReferenceOutputAdapter,
{
    /// Construct an inactive Module. No hardware is acquired at startup.
    pub fn new(adapter: A) -> Self {
        Self {
            adapter,
            session: None,
            request: None,
            scheduled: VecDeque::new(),
            next_frame_index: 0,
            diagnostics: ReferenceOutputDiagnostics::default(),
            shutdown_request_attempted: false,
            shutdown_request_failure: None,
            shutdown_module_failure: None,
            outstanding_frames_at_shutdown_request: None,
            completed_stop_session: None,
            completed_stop_outstanding_frames: None,
        }
    }

    /// Enumerate exact device modes without changing an active Session.
    pub fn discover(
        &mut self,
    ) -> Result<Vec<ReferenceOutputDeviceDescriptor>, ReferenceOutputError> {
        match self.adapter.discover() {
            Ok(devices) => {
                self.diagnostics.provider = Some(self.adapter.evidence().clone());
                Ok(devices)
            }
            Err(error) => {
                self.record_blocked(&error);
                Err(error.into())
            }
        }
    }

    /// Open one exact device generation. Active Sessions must be stopped first
    /// so hardware ownership never changes under queued callbacks.
    pub fn open(
        &mut self,
        device: &ReferenceOutputDeviceDescriptor,
        request: ReferenceOutputOpenRequest,
        first_frame_index: u64,
    ) -> Result<(), ReferenceOutputError> {
        if self.session.is_some() {
            return Err(ReferenceOutputError::AlreadyOpen);
        }
        request.validate()?;
        device.admit(&request)?;
        match self.adapter.open(device, &request) {
            Ok(session) => {
                self.diagnostics = ReferenceOutputDiagnostics {
                    state: ReferenceOutputState::Priming,
                    provider: Some(session.evidence().clone()),
                    device_id: Some(device.id.clone()),
                    device_generation: Some(session.device_generation()),
                    reference_locked: None,
                    ..ReferenceOutputDiagnostics::default()
                };
                self.request = Some(request);
                self.session = Some(session);
                self.next_frame_index = first_frame_index;
                self.scheduled.clear();
                self.shutdown_request_attempted = false;
                self.shutdown_request_failure = None;
                self.shutdown_module_failure = None;
                self.outstanding_frames_at_shutdown_request = None;
                self.completed_stop_session = None;
                self.completed_stop_outstanding_frames = None;
                Ok(())
            }
            Err(error) => {
                self.record_blocked(&error);
                Err(error.into())
            }
        }
    }

    /// Schedule one complete clean-feed bundle in exact frame order.
    pub fn schedule(&mut self, bundle: ReferenceOutputBundle) -> Result<(), ReferenceOutputError> {
        let request = self.request.as_ref().ok_or(ReferenceOutputError::NotOpen)?;
        if !matches!(
            self.diagnostics.state,
            ReferenceOutputState::Priming | ReferenceOutputState::Running
        ) {
            return Err(ReferenceOutputError::NotSchedulable { state: self.diagnostics.state });
        }
        bundle.validate()?;
        if bundle.video.signal() != &request.signal {
            return Err(ReferenceOutputError::PayloadSignalMismatch);
        }
        if bundle.frame_index() != self.next_frame_index {
            return Err(ReferenceOutputError::NonContiguousFrame {
                expected: self.next_frame_index,
                actual: bundle.frame_index(),
            });
        }
        if !request.ancillary_policy.is_required() && !bundle.ancillary.packets().is_empty() {
            return Err(ReferenceOutputError::AncillaryNotEnabled);
        }
        let audio_frames = bundle.audio.sample_frames() as u64;
        let ancillary_packets = bundle.ancillary.packets().len() as u64;
        let ancillary_sha256 = bundle.ancillary.sha256();
        let ancillary_words = bundle
            .ancillary
            .packets()
            .iter()
            .try_fold(0u64, |total, packet| {
                total.checked_add(packet.packet.encoded_word_count() as u64)
            })
            .ok_or(ReferenceOutputError::AncillaryWordCountOverflow)?;
        self.session.as_mut().ok_or(ReferenceOutputError::NotOpen)?.schedule(bundle)?;
        self.scheduled.push_back(ScheduledBundleEvidence {
            frame_index: self.next_frame_index,
            ancillary_sha256,
        });
        self.next_frame_index = self
            .next_frame_index
            .checked_add(1)
            .ok_or(ReferenceOutputError::FrameIndexOverflow)?;
        self.diagnostics.scheduled_frames += 1;
        self.diagnostics.scheduled_audio_frames = self
            .diagnostics
            .scheduled_audio_frames
            .checked_add(audio_frames)
            .ok_or(ReferenceOutputError::AudioFrameCountOverflow)?;
        self.diagnostics.scheduled_ancillary_packets = self
            .diagnostics
            .scheduled_ancillary_packets
            .checked_add(ancillary_packets)
            .ok_or(ReferenceOutputError::AncillaryPacketCountOverflow)?;
        self.diagnostics.scheduled_ancillary_words = self
            .diagnostics
            .scheduled_ancillary_words
            .checked_add(ancillary_words)
            .ok_or(ReferenceOutputError::AncillaryWordCountOverflow)?;
        self.diagnostics.scheduled_high_water =
            self.diagnostics.scheduled_high_water.max(self.scheduled.len() as u32);
        self.diagnostics.outstanding_frames = self.scheduled.len() as u64;
        Ok(())
    }

    /// Start provider playback after complete bounded preroll.
    pub fn start(&mut self) -> Result<(), ReferenceOutputError> {
        if self.diagnostics.state != ReferenceOutputState::Priming {
            return Err(ReferenceOutputError::NotPriming { state: self.diagnostics.state });
        }
        if self.request.as_ref().is_some_and(|request| {
            request.reference_policy == ReferenceOutputReferencePolicy::RequireExternalLock
        }) && self.diagnostics.reference_locked != Some(true)
        {
            return Err(ReferenceOutputError::ExternalReferenceNotLocked);
        }
        self.session.as_mut().ok_or(ReferenceOutputError::NotOpen)?.start()?;
        self.diagnostics.state = ReferenceOutputState::Running;
        Ok(())
    }

    /// Drain at most `limit` provider callbacks on the controlling thread.
    pub fn poll(&mut self, limit: usize) -> Result<usize, ReferenceOutputError> {
        let mut processed = 0;
        while processed < limit {
            let event = match self.session.as_mut().ok_or(ReferenceOutputError::NotOpen)?.poll() {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(error) => {
                    self.record_failed(&error);
                    self.abort_outstanding()?;
                    return Err(error.into());
                }
            };
            processed += 1;
            self.handle_event(event)?;
            if matches!(
                self.diagnostics.state,
                ReferenceOutputState::Blocked | ReferenceOutputState::Failed
            ) {
                break;
            }
        }
        Ok(processed)
    }

    /// Signal Session shutdown without waiting for provider-owned termination.
    ///
    /// This closes Module scheduling authority immediately and records the
    /// queue depth at the signal boundary. The Adapter Session contract
    /// forbids [`ReferenceOutputAdapterSession::begin_shutdown`] from waiting
    /// for callbacks or device release, so a caller can invoke this before
    /// shutting down other owners that share one absolute deadline.
    pub fn begin_shutdown(&mut self) -> Result<(), ReferenceOutputError> {
        if self.shutdown_request_attempted {
            return match &self.shutdown_request_failure {
                Some(failure) => Err(ReferenceOutputError::SessionShutdownRequestFailed {
                    detail: failure.detail.clone(),
                }),
                None => Ok(()),
            };
        }

        self.shutdown_request_attempted = true;
        self.outstanding_frames_at_shutdown_request = match u64::try_from(self.scheduled.len()) {
            Ok(count) => Some(count),
            Err(_) => {
                let detail =
                    "Module outstanding-frame count exceeded the receipt representation".to_owned();
                self.shutdown_module_failure = Some(detail);
                None
            }
        };
        self.request = None;

        let request_result = self.session.as_mut().map_or(Ok(()), |session| {
            panic::catch_unwind(AssertUnwindSafe(|| session.begin_shutdown())).unwrap_or_else(
                |_| {
                    Err(ReferenceOutputAdapterError::Vendor {
                        operation: "begin_shutdown",
                        detail: "provider Session panicked while requesting shutdown".to_owned(),
                    })
                },
            )
        });
        let accounting_result = self.abort_outstanding();
        if let Err(error) = &accounting_result {
            self.shutdown_module_failure = Some(error.to_string());
        }

        match request_result {
            Ok(()) => accounting_result,
            Err(error) => {
                let detail = error.to_string();
                self.shutdown_request_failure = Some(ReferenceOutputProviderShutdownFailure::new(
                    "begin_shutdown",
                    detail.clone(),
                ));
                self.record_failed_detail(detail.clone());
                Err(ReferenceOutputError::SessionShutdownRequestFailed { detail })
            }
        }
    }

    /// Stop and release the exact provider Session.
    ///
    /// Success requires the provider's consuming shutdown receipt to prove
    /// playback stop, callback termination, device release, and zero unresolved
    /// resources. Call [`Self::shutdown`] when the caller must retain that
    /// receipt as qualification evidence.
    pub fn stop(&mut self) -> Result<(), ReferenceOutputError> {
        if self.session.is_none() {
            self.request = None;
            self.abort_outstanding()?;
            return Ok(());
        }
        let outstanding_frames_before_shutdown = self.shutdown_outstanding_count();
        let session_shutdown = self.session.take().map_or_else(
            ReferenceOutputSessionShutdownReceipt::never_opened,
            |session| session.shutdown(),
        );
        self.completed_stop_session = Some(session_shutdown.clone());
        self.completed_stop_outstanding_frames = Some(outstanding_frames_before_shutdown);
        self.abort_outstanding()?;
        self.request = None;
        if !session_shutdown.all_resources_released() {
            let detail = session_shutdown.provider_failure.as_ref().map_or_else(
                || "provider Session shutdown did not prove complete release".to_owned(),
                |failure| {
                    format!(
                        "provider {} failed during Session shutdown: {}",
                        failure.operation, failure.detail
                    )
                },
            );
            self.record_failed_detail(detail);
            return Err(ReferenceOutputError::SessionShutdownIncomplete {
                receipt: session_shutdown,
            });
        }
        self.diagnostics.state = ReferenceOutputState::Stopped;
        Ok(())
    }

    /// Admit an ordinary product stop without consuming vendor shutdown on
    /// the calling thread.
    ///
    /// Admission closes scheduling authority and issues only the provider's
    /// non-blocking shutdown request. Session consumption then runs on a
    /// dedicated coordinator. Callers must treat the returned owner as
    /// `Stopping` until [`ReferenceOutputModuleStopCoordinator::finish_until`]
    /// returns a clean reusable Module; admission alone is not provider release
    /// evidence.
    pub fn begin_stop(self) -> ReferenceOutputModuleStopCoordinator<A>
    where
        A: 'static,
    {
        begin_module_stop(self)
    }

    /// Consume the Module and return complete scheduler/provider shutdown evidence.
    ///
    /// This operation never discards a provider failure behind an error return:
    /// the Session owner is consumed exactly once and every terminal fact is
    /// retained in the returned receipt.
    pub fn shutdown(mut self) -> ReferenceOutputModuleShutdownReceipt {
        let outstanding_frames_before_shutdown = self.shutdown_outstanding_count();
        let session = match self.session.take() {
            Some(session) => session.shutdown(),
            None => self
                .completed_stop_session
                .take()
                .unwrap_or_else(ReferenceOutputSessionShutdownReceipt::never_opened),
        };
        self.finalize_shutdown(session, outstanding_frames_before_shutdown)
    }

    /// Consume the Module and wait for provider shutdown only until `deadline`.
    ///
    /// The entire Module is moved to a dedicated coordinator after the
    /// non-blocking shutdown request. A successful coordinator join therefore
    /// covers provider Session consumption plus Adapter/bridge destruction;
    /// no potentially blocking Module field is destroyed on the deadline
    /// caller. Spawn failure, panic, timeout, and detachment are retained as
    /// stable, fail-closed receipt facts.
    pub fn shutdown_until(self, deadline: Instant) -> ReferenceOutputModuleShutdownReceipt
    where
        A: 'static,
    {
        shutdown_module_until(self, deadline)
    }

    fn shutdown_outstanding_count(&mut self) -> u64 {
        if self.session.is_none()
            && self.completed_stop_session.is_some()
            && let Some(count) = self.completed_stop_outstanding_frames
        {
            return count;
        }
        if let Some(count) = self.outstanding_frames_at_shutdown_request {
            return count;
        }
        match u64::try_from(self.scheduled.len()) {
            Ok(count) => count,
            Err(_) => {
                append_shutdown_failure(
                    &mut self.shutdown_module_failure,
                    "Module outstanding-frame count exceeded the receipt representation".to_owned(),
                );
                u64::MAX
            }
        }
    }

    fn finalize_shutdown(
        mut self,
        mut session: ReferenceOutputSessionShutdownReceipt,
        outstanding_frames_before_shutdown: u64,
    ) -> ReferenceOutputModuleShutdownReceipt {
        self.request = None;
        if let Err(error) = self.abort_outstanding() {
            append_shutdown_failure(&mut self.shutdown_module_failure, error.to_string());
        }
        if let Some(failure) = self.shutdown_request_failure.take() {
            append_shutdown_failure(
                &mut self.shutdown_module_failure,
                format!(
                    "provider {} failed during Session shutdown request: {}",
                    failure.operation, failure.detail
                ),
            );
            session.shutdown_request_completed = false;
            if session.provider_failure.is_none() {
                session.provider_failure = Some(failure);
            }
        }

        let module_failure = self.shutdown_module_failure.take();
        if module_failure.is_none() && session.all_resources_released() {
            if session.session_present {
                self.diagnostics.state = ReferenceOutputState::Stopped;
            }
        } else {
            let detail = module_failure.clone().unwrap_or_else(|| {
                session.provider_failure.as_ref().map_or_else(
                    || "provider Session shutdown did not prove complete release".to_owned(),
                    |failure| {
                        format!(
                            "provider {} failed during Session shutdown: {}",
                            failure.operation, failure.detail
                        )
                    },
                )
            });
            self.record_failed_detail(detail);
        }

        ReferenceOutputModuleShutdownReceipt {
            schema_version: 2,
            session,
            diagnostics: self.diagnostics,
            outstanding_frames_before_shutdown,
            module_failure,
        }
    }

    fn finish_ordinary_stop(mut self) -> ReferenceOutputModuleStopOutcome<A>
    where
        A: 'static,
    {
        let outstanding_frames_before_shutdown = self.shutdown_outstanding_count();
        let session = self.session.take().map_or_else(
            ReferenceOutputSessionShutdownReceipt::never_opened,
            |session| session.shutdown(),
        );
        self.request = None;
        if let Err(error) = self.abort_outstanding() {
            append_shutdown_failure(&mut self.shutdown_module_failure, error.to_string());
        }

        if self.shutdown_request_failure.is_none()
            && self.shutdown_module_failure.is_none()
            && session.all_resources_released()
        {
            let session_present = session.session_present;
            self.completed_stop_session = Some(session);
            self.completed_stop_outstanding_frames = Some(outstanding_frames_before_shutdown);
            if session_present {
                self.diagnostics.state = ReferenceOutputState::Stopped;
            }
            self.shutdown_request_attempted = false;
            self.shutdown_request_failure = None;
            self.shutdown_module_failure = None;
            self.outstanding_frames_at_shutdown_request = None;
            return ReferenceOutputModuleStopOutcome::stopped(self);
        }

        ReferenceOutputModuleStopOutcome::terminal(
            self.finalize_shutdown(session, outstanding_frames_before_shutdown),
        )
    }

    fn unresolved_shutdown_receipt(
        &self,
        outstanding_frames_before_shutdown: u64,
        operation: impl Into<String>,
        detail: impl Into<String>,
        coordinator: ReferenceOutputShutdownCoordinatorFacts,
    ) -> ReferenceOutputModuleShutdownReceipt {
        let mut session = unresolved_session_shutdown(
            self.session.is_some(),
            self.shutdown_request_failure.is_none(),
            outstanding_frames_before_shutdown,
            operation,
            detail,
            coordinator,
        );
        let mut module_failure = self.shutdown_module_failure.clone();
        if let Some(failure) = &self.shutdown_request_failure {
            append_shutdown_failure(
                &mut module_failure,
                format!(
                    "provider {} failed during Session shutdown request: {}",
                    failure.operation, failure.detail
                ),
            );
            session.shutdown_request_completed = false;
        }

        let mut diagnostics = self.diagnostics.clone();
        let failure_detail = module_failure.clone().unwrap_or_else(|| {
            session.provider_failure.as_ref().map_or_else(
                || "bounded Module shutdown did not prove complete release".to_owned(),
                |failure| {
                    format!(
                        "provider {} failed during Module shutdown: {}",
                        failure.operation, failure.detail
                    )
                },
            )
        });
        diagnostics.state = ReferenceOutputState::Failed;
        diagnostics.last_error = Some(failure_detail);

        ReferenceOutputModuleShutdownReceipt {
            schema_version: 2,
            session,
            diagnostics,
            outstanding_frames_before_shutdown,
            module_failure,
        }
    }

    /// Current immutable diagnostic snapshot.
    pub const fn diagnostics(&self) -> &ReferenceOutputDiagnostics {
        &self.diagnostics
    }

    fn handle_event(
        &mut self,
        event: ReferenceOutputAdapterEvent,
    ) -> Result<(), ReferenceOutputError> {
        self.diagnostics.callback_events = self
            .diagnostics
            .callback_events
            .checked_add(1)
            .ok_or(ReferenceOutputError::CallbackCountOverflow)?;
        match event {
            ReferenceOutputAdapterEvent::FrameCompleted {
                frame_index,
                ancillary_readback_sha256,
                hardware_time,
            } => {
                let expected = self.consume_scheduled(frame_index)?;
                if let Some(hardware_time) = hardware_time
                    && let Err(error) = self.record_hardware_time(hardware_time)
                {
                    self.abort_consumed_frame()?;
                    self.abort_outstanding()?;
                    return Err(error);
                }
                if self
                    .request
                    .as_ref()
                    .is_some_and(|request| request.ancillary_policy.requires_readback())
                {
                    let Some(actual) = ancillary_readback_sha256 else {
                        self.record_failed_detail(format!(
                            "provider omitted required ancillary readback for frame {frame_index}"
                        ));
                        self.abort_consumed_frame()?;
                        self.abort_outstanding()?;
                        return Err(ReferenceOutputError::AncillaryReadbackMissing { frame_index });
                    };
                    if actual != expected.ancillary_sha256 {
                        self.record_failed_detail(format!(
                            "provider ancillary readback differed for frame {frame_index}"
                        ));
                        self.abort_consumed_frame()?;
                        self.abort_outstanding()?;
                        return Err(ReferenceOutputError::AncillaryReadbackMismatch {
                            frame_index,
                        });
                    }
                    self.diagnostics.verified_ancillary_readbacks = self
                        .diagnostics
                        .verified_ancillary_readbacks
                        .checked_add(1)
                        .ok_or(ReferenceOutputError::AncillaryReadbackCountOverflow)?;
                }
                self.diagnostics.completed_frames += 1;
            }
            ReferenceOutputAdapterEvent::FrameLate { frame_index } => {
                self.consume_scheduled(frame_index)?;
                self.diagnostics.late_frames += 1;
            }
            ReferenceOutputAdapterEvent::FrameDropped { frame_index } => {
                self.consume_scheduled(frame_index)?;
                self.diagnostics.dropped_frames += 1;
            }
            ReferenceOutputAdapterEvent::FrameFlushed { frame_index } => {
                self.consume_scheduled(frame_index)?;
                self.diagnostics.flushed_frames += 1;
            }
            ReferenceOutputAdapterEvent::ReferenceLockChanged { locked } => {
                if !locked && self.diagnostics.reference_locked == Some(true) {
                    self.diagnostics.reference_lock_losses = self
                        .diagnostics
                        .reference_lock_losses
                        .checked_add(1)
                        .ok_or(ReferenceOutputError::ReferenceLockCountOverflow)?;
                }
                self.diagnostics.reference_locked = Some(locked);
                if !locked
                    && self.request.as_ref().is_some_and(|request| {
                        request.reference_policy
                            == ReferenceOutputReferencePolicy::RequireExternalLock
                    })
                {
                    self.block_active("required external reference lock was lost")?;
                }
            }
            ReferenceOutputAdapterEvent::DeviceLost => {
                self.block_active("reference output device was removed")?;
            }
            ReferenceOutputAdapterEvent::ProfileChanged => {
                self.block_active("reference output device profile changed")?;
            }
        }
        Ok(())
    }

    fn consume_scheduled(
        &mut self,
        actual: u64,
    ) -> Result<ScheduledBundleEvidence, ReferenceOutputError> {
        let expected = self
            .scheduled
            .front()
            .copied()
            .ok_or(ReferenceOutputError::UnexpectedCompletion { actual })?;
        if actual != expected.frame_index {
            self.record_failed_detail(format!(
                "out-of-order provider completion: expected {}, got {actual}",
                expected.frame_index
            ));
            self.abort_outstanding()?;
            return Err(ReferenceOutputError::OutOfOrderCompletion {
                expected: expected.frame_index,
                actual,
            });
        }
        self.scheduled.pop_front();
        self.diagnostics.outstanding_frames = self.scheduled.len() as u64;
        Ok(expected)
    }

    fn abort_consumed_frame(&mut self) -> Result<(), ReferenceOutputError> {
        self.diagnostics.aborted_frames = self
            .diagnostics
            .aborted_frames
            .checked_add(1)
            .ok_or(ReferenceOutputError::AbortedFrameCountOverflow)?;
        Ok(())
    }

    fn record_hardware_time(
        &mut self,
        current: ReferenceOutputHardwareTime,
    ) -> Result<(), ReferenceOutputError> {
        if current.ticks_per_second == 0 {
            self.diagnostics.hardware_time_failures =
                self.diagnostics.hardware_time_failures.saturating_add(1);
            self.record_failed_detail(
                "provider hardware timestamp has a zero tick rate".to_owned(),
            );
            return Err(ReferenceOutputError::InvalidHardwareTime);
        }
        if let Some(previous) = self.diagnostics.last_hardware_time {
            if current.ticks_per_second != previous.ticks_per_second
                || current.ticks <= previous.ticks
            {
                self.diagnostics.hardware_time_failures =
                    self.diagnostics.hardware_time_failures.saturating_add(1);
                self.record_failed_detail(
                    "provider hardware timestamp regressed or changed tick rate".to_owned(),
                );
                return Err(ReferenceOutputError::InvalidHardwareTime);
            }
            self.diagnostics.maximum_hardware_time_gap_ticks = self
                .diagnostics
                .maximum_hardware_time_gap_ticks
                .max(current.ticks - previous.ticks);
        } else {
            self.diagnostics.first_hardware_time = Some(current);
        }
        self.diagnostics.last_hardware_time = Some(current);
        self.diagnostics.hardware_timestamp_callbacks = self
            .diagnostics
            .hardware_timestamp_callbacks
            .checked_add(1)
            .ok_or(ReferenceOutputError::HardwareTimestampCountOverflow)?;
        Ok(())
    }

    fn abort_outstanding(&mut self) -> Result<(), ReferenceOutputError> {
        let outstanding = u64::try_from(self.scheduled.len())
            .map_err(|_| ReferenceOutputError::AbortedFrameCountOverflow)?;
        self.diagnostics.aborted_frames = self
            .diagnostics
            .aborted_frames
            .checked_add(outstanding)
            .ok_or(ReferenceOutputError::AbortedFrameCountOverflow)?;
        self.scheduled.clear();
        self.diagnostics.outstanding_frames = 0;
        Ok(())
    }

    fn block_active(&mut self, detail: &str) -> Result<(), ReferenceOutputError> {
        if let Some(session) = self.session.as_mut()
            && let Err(error) = session.stop()
        {
            self.record_failed(&error);
            self.abort_outstanding()?;
            return Err(error.into());
        }
        self.abort_outstanding()?;
        self.diagnostics.state = ReferenceOutputState::Blocked;
        self.diagnostics.last_error = Some(detail.to_owned());
        Ok(())
    }

    fn record_blocked(&mut self, error: &ReferenceOutputAdapterError) {
        self.diagnostics.provider = Some(self.adapter.evidence().clone());
        self.diagnostics.state = ReferenceOutputState::Blocked;
        self.diagnostics.last_error = Some(error.to_string());
    }

    fn record_failed(&mut self, error: &ReferenceOutputAdapterError) {
        self.record_failed_detail(error.to_string());
    }

    fn record_failed_detail(&mut self, detail: String) {
        self.diagnostics.state = ReferenceOutputState::Failed;
        self.diagnostics.last_error = Some(detail);
    }
}

fn begin_module_stop<A>(module: ReferenceOutputModule<A>) -> ReferenceOutputModuleStopCoordinator<A>
where
    A: ReferenceOutputAdapter + 'static,
{
    begin_module_stop_with_spawner(module, |work| {
        thread::Builder::new()
            .name("mondrian-reference-output-stop".to_owned())
            .spawn(work)
            .map_err(|error| error.to_string())
    })
}

fn begin_module_stop_with_spawner<A, F>(
    mut module: ReferenceOutputModule<A>,
    spawn: F,
) -> ReferenceOutputModuleStopCoordinator<A>
where
    A: ReferenceOutputAdapter + 'static,
    F: FnOnce(
        Box<dyn FnOnce() -> ReferenceOutputModuleStopOutcome<A> + Send>,
    ) -> Result<thread::JoinHandle<ReferenceOutputModuleStopOutcome<A>>, String>,
{
    if !module.shutdown_request_attempted {
        let _request_failed = module.begin_shutdown().is_err();
    }
    let shutdown_request_admitted = module.shutdown_request_failure.is_none();
    let outstanding_frames_before_shutdown = module.shutdown_outstanding_count();
    let payload_failure = module.unresolved_shutdown_receipt(
        outstanding_frames_before_shutdown,
        "stop_coordinator_payload",
        "ordinary stop coordinator could not acquire the Reference Output Module owner",
        ReferenceOutputShutdownCoordinatorFacts {
            required: true,
            spawned: true,
            joined: true,
            panicked: false,
            timed_out: false,
            detached: false,
            owner_abandoned: true,
        },
    );
    let mut spawn_failure = module.unresolved_shutdown_receipt(
        outstanding_frames_before_shutdown,
        "stop_coordinator_spawn",
        "ordinary stop coordinator thread could not be spawned",
        ReferenceOutputShutdownCoordinatorFacts {
            required: true,
            spawned: false,
            joined: false,
            panicked: false,
            timed_out: false,
            detached: false,
            owner_abandoned: true,
        },
    );
    let panic_failure = module.unresolved_shutdown_receipt(
        outstanding_frames_before_shutdown,
        "stop_coordinator_panic",
        "ordinary stop coordinator panicked while consuming the provider Session",
        ReferenceOutputShutdownCoordinatorFacts {
            required: true,
            spawned: true,
            joined: true,
            panicked: true,
            timed_out: false,
            detached: false,
            owner_abandoned: false,
        },
    );
    let timeout_failure = module.unresolved_shutdown_receipt(
        outstanding_frames_before_shutdown,
        "stop_coordinator_timeout",
        "absolute stop deadline elapsed before coordinator completion was observed",
        ReferenceOutputShutdownCoordinatorFacts {
            required: true,
            spawned: true,
            joined: false,
            panicked: false,
            timed_out: true,
            detached: true,
            owner_abandoned: false,
        },
    );

    // Retain the Module outside the closure until the worker starts. A failed
    // thread spawn drops its closure on this caller; the explicit leak below
    // is preferable to running an unbounded provider/Adapter destructor here.
    let payload = Arc::new(Mutex::new(Some(module)));
    let worker_payload = Arc::clone(&payload);
    let completed_at = Arc::new(Mutex::new(None));
    let worker_completed_at = Arc::clone(&completed_at);
    let work = Box::new(move || {
        let _completion_stamp = CompletionStamp { completed_at: worker_completed_at };
        match take_shutdown_module(&worker_payload) {
            Some(module) => module.finish_ordinary_stop(),
            None => ReferenceOutputModuleStopOutcome::terminal(payload_failure),
        }
    });
    let spawn_result = panic::catch_unwind(AssertUnwindSafe(|| spawn(work)));
    let spawn_result = match spawn_result {
        Ok(result) => result,
        Err(_) => Err("ordinary stop coordinator spawner panicked".to_owned()),
    };
    match spawn_result {
        Ok(handle) => ReferenceOutputModuleStopCoordinator {
            handle: Some(handle),
            immediate_failure: None,
            panic_failure: Some(panic_failure),
            timeout_failure: Some(timeout_failure),
            shutdown_request_admitted,
            completed_at,
        },
        Err(error) => {
            if let Some(module) = take_shutdown_module(&payload) {
                std::mem::forget(module);
            }
            if let Some(failure) = spawn_failure.session.provider_failure.as_mut() {
                failure.detail = error.clone();
            }
            spawn_failure.diagnostics.last_error = Some(format!(
                "provider stop_coordinator_spawn failed during ordinary stop: {error}"
            ));
            ReferenceOutputModuleStopCoordinator {
                handle: None,
                immediate_failure: Some(spawn_failure),
                panic_failure: Some(panic_failure),
                timeout_failure: Some(timeout_failure),
                shutdown_request_admitted,
                completed_at,
            }
        }
    }
}

fn detach_stop_coordinator<A>(
    handle: thread::JoinHandle<ReferenceOutputModuleStopOutcome<A>>,
) -> bool
where
    A: ReferenceOutputAdapter + 'static,
{
    detach_stop_coordinator_with_spawner(handle, |work| {
        thread::Builder::new()
            .name("mondrian-reference-output-stop-reaper".to_owned())
            .spawn(work)
            .map_err(|error| error.to_string())
    })
}

fn detach_stop_coordinator_with_spawner<A, F>(
    handle: thread::JoinHandle<ReferenceOutputModuleStopOutcome<A>>,
    spawn: F,
) -> bool
where
    A: ReferenceOutputAdapter + 'static,
    F: FnOnce(Box<dyn FnOnce() + Send>) -> Result<thread::JoinHandle<()>, String>,
{
    // The JoinHandle can contain a completed reusable Module. Retain it outside
    // the reaper closure until the new thread starts so spawn failure cannot
    // destroy that Module on this caller.
    let payload = Arc::new(Mutex::new(Some(handle)));
    let worker_payload = Arc::clone(&payload);
    let work = Box::new(move || {
        if let Some(handle) = take_stop_handle(&worker_payload) {
            let _outcome = handle.join();
        }
    });
    let spawn_result = panic::catch_unwind(AssertUnwindSafe(|| spawn(work)));
    if matches!(spawn_result, Ok(Ok(_))) {
        true
    } else {
        if let Some(handle) = take_stop_handle(&payload) {
            std::mem::forget(handle);
        }
        false
    }
}

fn mark_stop_coordinator_joined<A>(
    mut outcome: ReferenceOutputModuleStopOutcome<A>,
) -> ReferenceOutputModuleStopOutcome<A>
where
    A: ReferenceOutputAdapter + 'static,
{
    let facts = match &mut outcome {
        ReferenceOutputModuleStopOutcome::Stopped(module) => {
            module.completed_stop_session.as_mut().map(|session| &mut session.coordinator)
        }
        ReferenceOutputModuleStopOutcome::Terminal(receipt) => {
            Some(&mut receipt.session.coordinator)
        }
    };
    if let Some(facts) = facts {
        let provider = *facts;
        *facts = ReferenceOutputShutdownCoordinatorFacts {
            required: true,
            spawned: true,
            joined: true,
            panicked: provider.panicked,
            timed_out: provider.timed_out,
            detached: provider.detached,
            owner_abandoned: provider.owner_abandoned,
        };
    }
    outcome
}

fn take_stop_handle<A>(
    payload: &Mutex<Option<thread::JoinHandle<ReferenceOutputModuleStopOutcome<A>>>>,
) -> Option<thread::JoinHandle<ReferenceOutputModuleStopOutcome<A>>>
where
    A: ReferenceOutputAdapter + 'static,
{
    match payload.lock() {
        Ok(mut guard) => guard.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    }
}

fn completion_time(completed_at: &Mutex<Option<Instant>>) -> Option<Instant> {
    match completed_at.lock() {
        Ok(completed_at) => *completed_at,
        Err(poisoned) => *poisoned.into_inner(),
    }
}

fn missing_stop_coordinator_receipt(detail: &str) -> ReferenceOutputModuleShutdownReceipt {
    let coordinator = ReferenceOutputShutdownCoordinatorFacts {
        required: true,
        spawned: false,
        joined: false,
        panicked: false,
        timed_out: false,
        detached: false,
        owner_abandoned: true,
    };
    ReferenceOutputModuleShutdownReceipt {
        schema_version: 2,
        session: unresolved_session_shutdown(
            true,
            false,
            0,
            "stop_coordinator_state",
            detail,
            coordinator,
        ),
        diagnostics: ReferenceOutputDiagnostics {
            state: ReferenceOutputState::Failed,
            last_error: Some(detail.to_owned()),
            ..ReferenceOutputDiagnostics::default()
        },
        outstanding_frames_before_shutdown: 0,
        module_failure: Some(detail.to_owned()),
    }
}

fn shutdown_module_until<A>(
    module: ReferenceOutputModule<A>,
    deadline: Instant,
) -> ReferenceOutputModuleShutdownReceipt
where
    A: ReferenceOutputAdapter + 'static,
{
    shutdown_module_until_with_spawner(module, deadline, |work| {
        thread::Builder::new()
            .name("mondrian-reference-output-shutdown".to_owned())
            .spawn(work)
            .map_err(|error| error.to_string())
    })
}

fn shutdown_module_until_with_spawner<A, F>(
    mut module: ReferenceOutputModule<A>,
    deadline: Instant,
    spawn: F,
) -> ReferenceOutputModuleShutdownReceipt
where
    A: ReferenceOutputAdapter + 'static,
    F: FnOnce(
        Box<dyn FnOnce() -> ReferenceOutputModuleShutdownReceipt + Send>,
    ) -> Result<thread::JoinHandle<ReferenceOutputModuleShutdownReceipt>, String>,
{
    if !module.shutdown_request_attempted {
        let _request_failed = module.begin_shutdown().is_err();
    }
    let outstanding_frames_before_shutdown = module.shutdown_outstanding_count();
    let payload_failure = module.unresolved_shutdown_receipt(
        outstanding_frames_before_shutdown,
        "coordinator_payload",
        "coordinator could not acquire the Reference Output Module owner",
        ReferenceOutputShutdownCoordinatorFacts {
            required: true,
            spawned: true,
            joined: true,
            panicked: false,
            timed_out: false,
            detached: false,
            owner_abandoned: true,
        },
    );
    let mut spawn_failure = module.unresolved_shutdown_receipt(
        outstanding_frames_before_shutdown,
        "coordinator_spawn",
        "coordinator thread could not be spawned",
        ReferenceOutputShutdownCoordinatorFacts {
            required: true,
            spawned: false,
            joined: false,
            panicked: false,
            timed_out: false,
            detached: false,
            owner_abandoned: true,
        },
    );
    let panic_failure = module.unresolved_shutdown_receipt(
        outstanding_frames_before_shutdown,
        "coordinator_panic",
        "coordinator thread panicked while consuming the Reference Output Module",
        ReferenceOutputShutdownCoordinatorFacts {
            required: true,
            spawned: true,
            joined: true,
            panicked: true,
            timed_out: false,
            detached: false,
            owner_abandoned: false,
        },
    );
    let timeout_failure = module.unresolved_shutdown_receipt(
        outstanding_frames_before_shutdown,
        "coordinator_timeout",
        "absolute shutdown deadline elapsed before coordinator completion was observed",
        ReferenceOutputShutdownCoordinatorFacts {
            required: true,
            spawned: true,
            joined: false,
            panicked: false,
            timed_out: true,
            detached: true,
            owner_abandoned: false,
        },
    );

    // Keep the whole Module outside the closure until the new thread has
    // actually started. `Builder::spawn` drops an unstarted closure on error;
    // moving the Module directly into that closure could therefore run an
    // unbounded Session, Adapter, or bridge Drop on the caller. The explicit
    // leak on spawn failure is fail-closed but deadline-safe, and the unresolved
    // Module owner remains visible in the returned resource count.
    let payload = Arc::new(Mutex::new(Some(module)));
    let worker_payload = Arc::clone(&payload);
    let completed_at = Arc::new(Mutex::new(None));
    let worker_completed_at = Arc::clone(&completed_at);
    let work = Box::new(move || {
        let _completion_stamp = CompletionStamp { completed_at: worker_completed_at };
        match take_shutdown_module(&worker_payload) {
            Some(module) => module.shutdown(),
            None => payload_failure,
        }
    });
    let coordinator = panic::catch_unwind(AssertUnwindSafe(|| spawn(work)));
    let coordinator = match coordinator {
        Ok(result) => result,
        Err(_) => Err("Module shutdown coordinator spawner panicked".to_owned()),
    };
    let handle = match coordinator {
        Ok(handle) => handle,
        Err(error) => {
            if let Some(module) = take_shutdown_module(&payload) {
                std::mem::forget(module);
            }
            if let Some(failure) = spawn_failure.session.provider_failure.as_mut() {
                failure.detail = error.clone();
            }
            if spawn_failure.module_failure.is_none() {
                spawn_failure.diagnostics.last_error = Some(format!(
                    "provider coordinator_spawn failed during Module shutdown: {error}"
                ));
            }
            return spawn_failure;
        }
    };

    loop {
        // Completion wins at the deadline boundary. Once `is_finished` is
        // observable, joining is non-blocking and positively proves that the
        // Module destructor already ran on the coordinator.
        if handle.is_finished() {
            if completion_time(&completed_at).is_none_or(|completed_at| completed_at > deadline) {
                return timeout_failure;
            }
            return match handle.join() {
                Ok(mut receipt) => {
                    let provider_coordinator = receipt.session.coordinator;
                    receipt.session.coordinator = ReferenceOutputShutdownCoordinatorFacts {
                        required: true,
                        spawned: true,
                        joined: true,
                        panicked: provider_coordinator.panicked,
                        timed_out: provider_coordinator.timed_out,
                        detached: provider_coordinator.detached,
                        owner_abandoned: provider_coordinator.owner_abandoned,
                    };
                    receipt
                }
                Err(_) => panic_failure,
            };
        }

        let now = Instant::now();
        if now >= deadline {
            if handle.is_finished() {
                continue;
            }
            return timeout_failure;
        }
        thread::sleep((deadline - now).min(Duration::from_millis(1)));
    }
}

fn take_shutdown_module<A>(
    payload: &Mutex<Option<ReferenceOutputModule<A>>>,
) -> Option<ReferenceOutputModule<A>> {
    match payload.lock() {
        Ok(mut guard) => guard.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    }
}

fn unresolved_session_shutdown(
    session_present: bool,
    shutdown_request_completed: bool,
    outstanding_frames: u64,
    operation: impl Into<String>,
    detail: impl Into<String>,
    coordinator: ReferenceOutputShutdownCoordinatorFacts,
) -> ReferenceOutputSessionShutdownReceipt {
    ReferenceOutputSessionShutdownReceipt {
        schema_version: 2,
        session_present,
        shutdown_request_completed,
        playback_stopped: !session_present,
        callback_execution_terminated: !session_present,
        device_released: !session_present,
        outstanding_frames: if session_present {
            outstanding_frames
        } else {
            0
        },
        // Even a never-opened Module still owns its Adapter/bridge until the
        // coordinator joins. Schema 2 uses this resource fact plus coordinator
        // lifecycle closure to cover the complete Module owner.
        outstanding_resources: 1,
        provider_failure: Some(ReferenceOutputProviderShutdownFailure::new(
            operation, detail,
        )),
        coordinator,
    }
}

fn append_shutdown_failure(target: &mut Option<String>, detail: String) {
    *target = Some(match target.take() {
        Some(previous) => format!("{previous}; {detail}"),
        None => detail,
    });
}

/// Reference Output Module failure.
#[derive(Debug, thiserror::Error)]
pub enum ReferenceOutputError {
    /// Provider/Session operation failed.
    #[error(transparent)]
    Adapter(#[from] ReferenceOutputAdapterError),
    /// Signal/mode request is invalid.
    #[error(transparent)]
    Mode(#[from] crate::ReferenceOutputModeError),
    /// Bundle payload is invalid.
    #[error(transparent)]
    Payload(#[from] crate::ReferenceOutputPayloadError),
    /// A Session already owns the device.
    #[error("reference output Session is already open; stop it before reconfiguration")]
    AlreadyOpen,
    /// Operation requires an open Session.
    #[error("reference output Session is not open")]
    NotOpen,
    /// Consuming provider shutdown did not prove complete resource release.
    #[error("reference output Session shutdown did not prove complete resource release")]
    SessionShutdownIncomplete {
        /// Provider receipt retaining every terminal lifetime fact and failure.
        receipt: ReferenceOutputSessionShutdownReceipt,
    },
    /// A prior non-blocking shutdown request failed and cannot be retried.
    #[error("reference output Session shutdown request already failed: {detail}")]
    SessionShutdownRequestFailed { detail: String },
    /// Scheduling is disallowed in the current lifecycle state.
    #[error("reference output is not schedulable in state {state:?}")]
    NotSchedulable { state: ReferenceOutputState },
    /// Start requires Priming state.
    #[error("reference output cannot start in state {state:?}")]
    NotPriming { state: ReferenceOutputState },
    /// Required external reference must be positively observed before start.
    #[error("reference output requires proven external reference lock before start")]
    ExternalReferenceNotLocked,
    /// Bundle signal differs from the open request.
    #[error("reference output bundle signal differs from the open Session")]
    PayloadSignalMismatch,
    /// Frame schedule must be contiguous.
    #[error("reference output frame schedule expected {expected}, got {actual}")]
    NonContiguousFrame { expected: u64, actual: u64 },
    /// Frame coordinate overflowed.
    #[error("reference output frame index overflow")]
    FrameIndexOverflow,
    /// Audio accounting overflowed.
    #[error("reference output audio frame accounting overflow")]
    AudioFrameCountOverflow,
    /// A non-empty ANC inventory was supplied to a Session opened without ANC.
    #[error("reference output ancillary packets were not enabled for this Session")]
    AncillaryNotEnabled,
    /// ANC packet accounting overflowed.
    #[error("reference output ancillary packet accounting overflow")]
    AncillaryPacketCountOverflow,
    /// ANC word accounting overflowed.
    #[error("reference output ancillary word accounting overflow")]
    AncillaryWordCountOverflow,
    /// Required provider readback was absent for a completed frame.
    #[error("reference output provider omitted ancillary readback for frame {frame_index}")]
    AncillaryReadbackMissing { frame_index: u64 },
    /// Provider readback did not match the scheduled packet inventory.
    #[error("reference output provider ancillary readback mismatch for frame {frame_index}")]
    AncillaryReadbackMismatch { frame_index: u64 },
    /// Verified readback accounting overflowed.
    #[error("reference output ancillary readback accounting overflow")]
    AncillaryReadbackCountOverflow,
    /// Provider callback accounting overflowed.
    #[error("reference output callback accounting overflow")]
    CallbackCountOverflow,
    /// External-reference transition accounting overflowed.
    #[error("reference output reference-lock accounting overflow")]
    ReferenceLockCountOverflow,
    /// Hardware timestamp accounting overflowed.
    #[error("reference output hardware timestamp accounting overflow")]
    HardwareTimestampCountOverflow,
    /// Provider hardware timestamp was invalid or non-monotonic.
    #[error("reference output provider hardware timestamp is invalid")]
    InvalidHardwareTime,
    /// Aborted outstanding-frame accounting overflowed.
    #[error("reference output aborted-frame accounting overflow")]
    AbortedFrameCountOverflow,
    /// Provider completed a frame that was never scheduled.
    #[error("reference output provider completed unscheduled frame {actual}")]
    UnexpectedCompletion { actual: u64 },
    /// Provider callback order violated scheduled playout identity.
    #[error("reference output completion expected {expected}, got {actual}")]
    OutOfOrderCompletion { expected: u64, actual: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        pack_encoded_rgb_to_v210, ReferenceAudioFrame, ReferenceOutputMode,
        ReferenceOutputPixelFormat, ReferenceOutputRange, ReferenceOutputScan,
        ReferenceOutputSignal, ReferenceVideoFrame, SimulatedReferenceOutputAdapter,
    };
    use mondrian_broadcast::{
        ActiveFormatDescription, AncillaryField, AncillaryOrigin, AncillaryPacket,
        AncillaryPlacement, AncillarySpace, AncillaryValidationLevel,
    };
    use mondrian_core::{AudioChannelLayout, ColorSpace, Rational};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct DropTrackedAdapter<A> {
        inner: A,
        dropped: Arc<AtomicBool>,
        drop_delay: Duration,
    }

    impl<A> Drop for DropTrackedAdapter<A> {
        fn drop(&mut self) {
            thread::sleep(self.drop_delay);
            self.dropped.store(true, Ordering::Release);
        }
    }

    impl<A> ReferenceOutputAdapter for DropTrackedAdapter<A>
    where
        A: ReferenceOutputAdapter,
    {
        fn evidence(&self) -> &ReferenceOutputProviderEvidence {
            self.inner.evidence()
        }

        fn discover(
            &mut self,
        ) -> Result<Vec<ReferenceOutputDeviceDescriptor>, ReferenceOutputAdapterError> {
            self.inner.discover()
        }

        fn open(
            &mut self,
            device: &ReferenceOutputDeviceDescriptor,
            request: &ReferenceOutputOpenRequest,
        ) -> Result<Box<dyn ReferenceOutputAdapterSession>, ReferenceOutputAdapterError> {
            self.inner.open(device, request)
        }
    }

    fn request(reference_policy: ReferenceOutputReferencePolicy) -> ReferenceOutputOpenRequest {
        ReferenceOutputOpenRequest {
            signal: ReferenceOutputSignal {
                width: 6,
                height: 1,
                frame_rate: Rational::FPS_25,
                scan: ReferenceOutputScan::Progressive,
                pixel_format: ReferenceOutputPixelFormat::Yuv422TenV210,
                color_space: ColorSpace::Rec709,
                range: ReferenceOutputRange::Legal,
                hdr: None,
                audio_layout: AudioChannelLayout::Stereo,
            },
            reference_policy,
            ancillary_policy: crate::ReferenceOutputAncillaryPolicy::Disabled,
            preroll_frames: 2,
            max_scheduled_frames: 3,
        }
    }

    fn bundle(request: &ReferenceOutputOpenRequest, frame_index: u64) -> ReferenceOutputBundle {
        let rgba = [[0.0, 0.0, 0.0, 1.0]; 6];
        let (row_bytes, bytes) = pack_encoded_rgb_to_v210(&request.signal, &rgba).expect("pack");
        let video = ReferenceVideoFrame::from_program_output(
            &request.signal,
            frame_index,
            row_bytes,
            bytes,
        )
        .expect("video");
        let sample_frames =
            request.signal.audio_frames_for_video_frame(frame_index).expect("audio cadence")
                as usize;
        let audio =
            ReferenceAudioFrame::new(&request.signal, frame_index, vec![0; sample_frames * 2])
                .expect("audio");
        ReferenceOutputBundle {
            video,
            audio,
            ancillary: crate::AncillaryFrame::empty(frame_index),
        }
    }

    fn module(
        request: &ReferenceOutputOpenRequest,
        events: impl IntoIterator<Item = ReferenceOutputAdapterEvent>,
    ) -> (
        ReferenceOutputModule<SimulatedReferenceOutputAdapter>,
        ReferenceOutputDeviceDescriptor,
    ) {
        let mode = ReferenceOutputMode {
            signal: request.signal.clone(),
            supports_hdr_signal: false,
            supports_static_hdr_metadata: false,
            supports_reference_status: true,
            supports_ancillary: true,
            supports_ancillary_readback: true,
        };
        let adapter = SimulatedReferenceOutputAdapter::new(vec![mode])
            .expect("adapter")
            .with_scripted_events(events);
        let mut module = ReferenceOutputModule::new(adapter);
        let device = module.discover().expect("discover").remove(0);
        (module, device)
    }

    fn drop_tracked_module(
        request: &ReferenceOutputOpenRequest,
    ) -> (
        ReferenceOutputModule<DropTrackedAdapter<SimulatedReferenceOutputAdapter>>,
        ReferenceOutputDeviceDescriptor,
        Arc<AtomicBool>,
    ) {
        drop_tracked_module_with_delay(request, Duration::ZERO)
    }

    fn drop_tracked_module_with_delay(
        request: &ReferenceOutputOpenRequest,
        drop_delay: Duration,
    ) -> (
        ReferenceOutputModule<DropTrackedAdapter<SimulatedReferenceOutputAdapter>>,
        ReferenceOutputDeviceDescriptor,
        Arc<AtomicBool>,
    ) {
        let mode = ReferenceOutputMode {
            signal: request.signal.clone(),
            supports_hdr_signal: false,
            supports_static_hdr_metadata: false,
            supports_reference_status: true,
            supports_ancillary: true,
            supports_ancillary_readback: true,
        };
        let inner = SimulatedReferenceOutputAdapter::new(vec![mode]).expect("adapter");
        let dropped = Arc::new(AtomicBool::new(false));
        let adapter = DropTrackedAdapter { inner, dropped: Arc::clone(&dropped), drop_delay };
        let mut module = ReferenceOutputModule::new(adapter);
        let device = module.discover().expect("discover").remove(0);
        (module, device, dropped)
    }

    fn module_with_stop_failure(
        request: &ReferenceOutputOpenRequest,
        events: impl IntoIterator<Item = ReferenceOutputAdapterEvent>,
    ) -> (
        ReferenceOutputModule<SimulatedReferenceOutputAdapter>,
        ReferenceOutputDeviceDescriptor,
    ) {
        let mode = ReferenceOutputMode {
            signal: request.signal.clone(),
            supports_hdr_signal: false,
            supports_static_hdr_metadata: false,
            supports_reference_status: true,
            supports_ancillary: true,
            supports_ancillary_readback: true,
        };
        let adapter = SimulatedReferenceOutputAdapter::new(vec![mode])
            .expect("adapter")
            .with_scripted_events(events)
            .with_stop_failure();
        let mut module = ReferenceOutputModule::new(adapter);
        let device = module.discover().expect("discover").remove(0);
        (module, device)
    }

    fn module_with_shutdown_panic(
        request: &ReferenceOutputOpenRequest,
    ) -> (
        ReferenceOutputModule<SimulatedReferenceOutputAdapter>,
        ReferenceOutputDeviceDescriptor,
    ) {
        let mode = ReferenceOutputMode {
            signal: request.signal.clone(),
            supports_hdr_signal: false,
            supports_static_hdr_metadata: false,
            supports_reference_status: true,
            supports_ancillary: true,
            supports_ancillary_readback: true,
        };
        let adapter = SimulatedReferenceOutputAdapter::new(vec![mode])
            .expect("adapter")
            .with_shutdown_panic();
        let mut module = ReferenceOutputModule::new(adapter);
        let device = module.discover().expect("discover").remove(0);
        (module, device)
    }

    fn module_with_begin_shutdown_panic(
        request: &ReferenceOutputOpenRequest,
    ) -> (
        ReferenceOutputModule<SimulatedReferenceOutputAdapter>,
        ReferenceOutputDeviceDescriptor,
    ) {
        let mode = ReferenceOutputMode {
            signal: request.signal.clone(),
            supports_hdr_signal: false,
            supports_static_hdr_metadata: false,
            supports_reference_status: true,
            supports_ancillary: true,
            supports_ancillary_readback: true,
        };
        let adapter = SimulatedReferenceOutputAdapter::new(vec![mode])
            .expect("adapter")
            .with_begin_shutdown_panic();
        let mut module = ReferenceOutputModule::new(adapter);
        let device = module.discover().expect("discover").remove(0);
        (module, device)
    }

    fn module_with_shutdown_delay(
        request: &ReferenceOutputOpenRequest,
        delay: Duration,
    ) -> (
        ReferenceOutputModule<SimulatedReferenceOutputAdapter>,
        ReferenceOutputDeviceDescriptor,
    ) {
        let mode = ReferenceOutputMode {
            signal: request.signal.clone(),
            supports_hdr_signal: false,
            supports_static_hdr_metadata: false,
            supports_reference_status: true,
            supports_ancillary: true,
            supports_ancillary_readback: true,
        };
        let adapter = SimulatedReferenceOutputAdapter::new(vec![mode])
            .expect("adapter")
            .with_shutdown_delay(delay);
        let mut module = ReferenceOutputModule::new(adapter);
        let device = module.discover().expect("discover").remove(0);
        (module, device)
    }

    fn module_with_stale_shutdown_schema(
        request: &ReferenceOutputOpenRequest,
    ) -> (
        ReferenceOutputModule<SimulatedReferenceOutputAdapter>,
        ReferenceOutputDeviceDescriptor,
    ) {
        let mode = ReferenceOutputMode {
            signal: request.signal.clone(),
            supports_hdr_signal: false,
            supports_static_hdr_metadata: false,
            supports_reference_status: true,
            supports_ancillary: true,
            supports_ancillary_readback: true,
        };
        let adapter = SimulatedReferenceOutputAdapter::new(vec![mode])
            .expect("adapter")
            .with_stale_shutdown_schema();
        let mut module = ReferenceOutputModule::new(adapter);
        let device = module.discover().expect("discover").remove(0);
        (module, device)
    }

    #[test]
    fn simulated_path_prerolls_and_accounts_exact_audio() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device) = module(&request, []);
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");
        module.schedule(bundle(&request, 1)).expect("frame 1");
        module.start().expect("start");
        assert_eq!(module.poll(8).expect("poll"), 2);
        let diagnostics = module.diagnostics();
        assert_eq!(diagnostics.state, ReferenceOutputState::Running);
        assert_eq!(diagnostics.completed_frames, 2);
        assert_eq!(diagnostics.outstanding_frames, 0);
        assert_eq!(
            diagnostics.scheduled_frames,
            diagnostics.completed_frames
                + diagnostics.late_frames
                + diagnostics.dropped_frames
                + diagnostics.flushed_frames
                + diagnostics.aborted_frames
                + diagnostics.outstanding_frames
        );
        assert_eq!(diagnostics.scheduled_audio_frames, 3_840);
        assert_eq!(diagnostics.scheduled_high_water, 2);
        assert!(!diagnostics.provider.as_ref().expect("evidence").hardware_backed);
    }

    #[test]
    fn required_reference_loss_stops_and_blocks() {
        let request = request(ReferenceOutputReferencePolicy::RequireExternalLock);
        let (mut module, device) = module(
            &request,
            [
                ReferenceOutputAdapterEvent::ReferenceLockChanged { locked: true },
                ReferenceOutputAdapterEvent::ReferenceLockChanged { locked: false },
            ],
        );
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");
        module.schedule(bundle(&request, 1)).expect("frame 1");
        module.poll(1).expect("poll initial lock");
        module.start().expect("start");
        module.poll(1).expect("poll loss");
        assert_eq!(module.diagnostics().state, ReferenceOutputState::Blocked);
        assert_eq!(module.diagnostics().reference_locked, Some(false));
        assert!(module
            .diagnostics()
            .last_error
            .as_deref()
            .is_some_and(|detail| detail.contains("reference lock")));
    }

    #[test]
    fn required_reference_cannot_start_without_positive_lock_evidence() {
        let request = request(ReferenceOutputReferencePolicy::RequireExternalLock);
        let (mut module, device) = module(&request, []);
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");
        module.schedule(bundle(&request, 1)).expect("frame 1");

        assert!(matches!(
            module.start(),
            Err(ReferenceOutputError::ExternalReferenceNotLocked)
        ));
        assert_eq!(module.diagnostics().state, ReferenceOutputState::Priming);
        assert_eq!(module.diagnostics().reference_locked, None);
    }

    #[test]
    fn schedule_rejects_frame_gap_before_adapter_call() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device) = module(&request, []);
        module.open(&device, request.clone(), 7).expect("open");
        let error = module.schedule(bundle(&request, 8)).expect_err("gap");
        assert!(matches!(
            error,
            ReferenceOutputError::NonContiguousFrame { expected: 7, actual: 8 }
        ));
    }

    #[test]
    fn ancillary_inventory_is_scheduled_atomically_and_accounted() {
        let mut request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        request.ancillary_policy = crate::ReferenceOutputAncillaryPolicy::RequiredWithReadback;
        request.preroll_frames = 1;
        let (mut module, device) = module(&request, []);
        module.open(&device, request.clone(), 0).expect("open ANC Session");
        let placement =
            AncillaryPlacement::new(AncillarySpace::Vanc, AncillaryField::Progressive, 9, 0)
                .expect("placement");
        let packet = ActiveFormatDescription::new(8, true, None)
            .expect("AFD")
            .packet()
            .expect("ST 291 packet");
        let expected_words = packet.encoded_word_count() as u64;
        let ancillary = crate::AncillaryFrame::new(
            0,
            vec![AncillaryPacket {
                placement,
                packet,
                origin: AncillaryOrigin::Derived,
                validation: AncillaryValidationLevel::Semantic,
            }],
        )
        .expect("ANC frame");
        let mut first = bundle(&request, 0);
        first.ancillary = ancillary;
        module.schedule(first).expect("atomic ANC bundle");
        module.start().expect("start ANC Session");
        assert_eq!(module.poll(1).expect("ANC completion"), 1);
        assert_eq!(module.diagnostics().scheduled_frames, 1);
        assert_eq!(module.diagnostics().completed_frames, 1);
        assert_eq!(module.diagnostics().scheduled_ancillary_packets, 1);
        assert_eq!(
            module.diagnostics().scheduled_ancillary_words,
            expected_words
        );
        assert_eq!(module.diagnostics().verified_ancillary_readbacks, 1);
    }

    #[test]
    fn required_ancillary_readback_fails_closed_when_provider_omits_digest() {
        let mut request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        request.ancillary_policy = crate::ReferenceOutputAncillaryPolicy::RequiredWithReadback;
        request.preroll_frames = 1;
        let (mut module, device) = module(
            &request,
            [ReferenceOutputAdapterEvent::FrameCompleted {
                frame_index: 0,
                hardware_time: Some(ReferenceOutputHardwareTime {
                    ticks: 123,
                    ticks_per_second: 25_000,
                }),
                ancillary_readback_sha256: None,
            }],
        );
        module.open(&device, request.clone(), 0).expect("open ANC Session");
        module.schedule(bundle(&request, 0)).expect("schedule frame");
        module.start().expect("start Session");
        assert!(matches!(
            module.poll(1),
            Err(ReferenceOutputError::AncillaryReadbackMissing { frame_index: 0 })
        ));
        assert_eq!(module.diagnostics().state, ReferenceOutputState::Failed);
        assert_eq!(module.diagnostics().completed_frames, 0);
        assert_eq!(module.diagnostics().aborted_frames, 1);
        assert_eq!(module.diagnostics().outstanding_frames, 0);
    }

    #[test]
    fn hardware_time_is_typed_monotonic_and_bounded() {
        let mut request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        request.preroll_frames = 2;
        let events = [
            ReferenceOutputAdapterEvent::FrameCompleted {
                frame_index: 0,
                hardware_time: Some(ReferenceOutputHardwareTime {
                    ticks: 1_000,
                    ticks_per_second: 25_000,
                }),
                ancillary_readback_sha256: None,
            },
            ReferenceOutputAdapterEvent::FrameCompleted {
                frame_index: 1,
                hardware_time: Some(ReferenceOutputHardwareTime {
                    ticks: 2_000,
                    ticks_per_second: 25_000,
                }),
                ancillary_readback_sha256: None,
            },
        ];
        let (mut module, device) = module(&request, events);
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");
        module.schedule(bundle(&request, 1)).expect("frame 1");
        module.start().expect("start");
        assert_eq!(module.poll(2).expect("poll"), 2);

        let diagnostics = module.diagnostics();
        assert_eq!(diagnostics.hardware_timestamp_callbacks, 2);
        assert_eq!(diagnostics.hardware_time_failures, 0);
        assert_eq!(diagnostics.maximum_hardware_time_gap_ticks, 1_000);
        assert_eq!(
            diagnostics.last_hardware_time,
            Some(ReferenceOutputHardwareTime { ticks: 2_000, ticks_per_second: 25_000 })
        );
    }

    #[test]
    fn regressing_hardware_time_fails_and_closes_frame_accounting() {
        let mut request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        request.preroll_frames = 2;
        let events = [
            ReferenceOutputAdapterEvent::FrameCompleted {
                frame_index: 0,
                hardware_time: Some(ReferenceOutputHardwareTime {
                    ticks: 1_000,
                    ticks_per_second: 25_000,
                }),
                ancillary_readback_sha256: None,
            },
            ReferenceOutputAdapterEvent::FrameCompleted {
                frame_index: 1,
                hardware_time: Some(ReferenceOutputHardwareTime {
                    ticks: 999,
                    ticks_per_second: 25_000,
                }),
                ancillary_readback_sha256: None,
            },
        ];
        let (mut module, device) = module(&request, events);
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");
        module.schedule(bundle(&request, 1)).expect("frame 1");
        module.start().expect("start");
        assert!(matches!(
            module.poll(2),
            Err(ReferenceOutputError::InvalidHardwareTime)
        ));

        let diagnostics = module.diagnostics();
        assert_eq!(diagnostics.state, ReferenceOutputState::Failed);
        assert_eq!(diagnostics.hardware_time_failures, 1);
        assert_eq!(diagnostics.completed_frames, 1);
        assert_eq!(diagnostics.aborted_frames, 1);
        assert_eq!(diagnostics.outstanding_frames, 0);
        assert_eq!(diagnostics.scheduled_frames, 2);
    }

    #[test]
    fn out_of_order_callback_fails_and_aborts_the_entire_queue() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device) = module(
            &request,
            [ReferenceOutputAdapterEvent::FrameDropped { frame_index: 1 }],
        );
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");
        module.schedule(bundle(&request, 1)).expect("frame 1");
        module.start().expect("start");

        assert!(matches!(
            module.poll(1),
            Err(ReferenceOutputError::OutOfOrderCompletion { expected: 0, actual: 1 })
        ));
        let diagnostics = module.diagnostics();
        assert_eq!(diagnostics.state, ReferenceOutputState::Failed);
        assert_eq!(diagnostics.aborted_frames, 2);
        assert_eq!(diagnostics.outstanding_frames, 0);
        assert_eq!(
            diagnostics.scheduled_frames,
            diagnostics.completed_frames
                + diagnostics.late_frames
                + diagnostics.dropped_frames
                + diagnostics.flushed_frames
                + diagnostics.aborted_frames
                + diagnostics.outstanding_frames
        );
    }

    #[test]
    fn device_loss_stop_failure_still_fails_and_closes_accounting() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device) =
            module_with_stop_failure(&request, [ReferenceOutputAdapterEvent::DeviceLost]);
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");
        module.schedule(bundle(&request, 1)).expect("frame 1");
        module.start().expect("start");

        assert!(matches!(
            module.poll(1),
            Err(ReferenceOutputError::Adapter(
                ReferenceOutputAdapterError::Vendor { operation: "stop", .. }
            ))
        ));
        let diagnostics = module.diagnostics();
        assert_eq!(diagnostics.state, ReferenceOutputState::Failed);
        assert_eq!(diagnostics.aborted_frames, 2);
        assert_eq!(diagnostics.outstanding_frames, 0);
        assert_eq!(diagnostics.scheduled_frames, diagnostics.aborted_frames);
    }

    #[test]
    fn never_opened_module_shutdown_is_explicitly_clean() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (module, _device) = module(&request, []);

        let receipt = module.shutdown();

        assert!(!receipt.session.session_present);
        assert!(receipt.session.playback_stopped);
        assert!(receipt.session.callback_execution_terminated);
        assert!(receipt.session.device_released);
        assert_eq!(receipt.session.outstanding_frames, 0);
        assert_eq!(receipt.session.outstanding_resources, 0);
        assert!(receipt.session.provider_failure.is_none());
        assert_eq!(receipt.outstanding_frames_before_shutdown, 0);
        assert_eq!(receipt.diagnostics.state, ReferenceOutputState::Disabled);
        assert!(receipt.all_resources_released());

        let mut stale_module_schema = receipt.clone();
        stale_module_schema.schema_version = 1;
        assert!(!stale_module_schema.all_resources_released());
        let mut stale_session_schema = receipt;
        stale_session_schema.session.schema_version = 1;
        assert!(!stale_session_schema.all_resources_released());
    }

    #[test]
    fn module_shutdown_consumes_session_and_preserves_queue_accounting() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device) = module(&request, []);
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");
        module.schedule(bundle(&request, 1)).expect("frame 1");

        let receipt = module.shutdown();

        assert!(receipt.session.session_present);
        assert!(receipt.session.playback_stopped);
        assert!(receipt.session.callback_execution_terminated);
        assert!(receipt.session.device_released);
        assert_eq!(receipt.session.outstanding_frames, 0);
        assert_eq!(receipt.session.outstanding_resources, 0);
        assert_eq!(receipt.outstanding_frames_before_shutdown, 2);
        assert_eq!(receipt.diagnostics.scheduled_frames, 2);
        assert_eq!(receipt.diagnostics.aborted_frames, 2);
        assert_eq!(receipt.diagnostics.outstanding_frames, 0);
        assert_eq!(receipt.diagnostics.state, ReferenceOutputState::Stopped);
        assert!(receipt.all_resources_released());
    }

    #[test]
    fn provider_shutdown_failure_remains_in_fail_closed_receipt() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device) = module_with_stop_failure(&request, []);
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");

        let receipt = module.shutdown();

        assert!(receipt.session.session_present);
        assert!(!receipt.session.playback_stopped);
        assert!(!receipt.session.callback_execution_terminated);
        assert!(!receipt.session.device_released);
        assert_eq!(receipt.session.outstanding_frames, 1);
        assert_eq!(receipt.session.outstanding_resources, 1);
        assert!(receipt.session.provider_failure.is_some());
        assert_eq!(receipt.outstanding_frames_before_shutdown, 1);
        assert_eq!(receipt.diagnostics.aborted_frames, 1);
        assert_eq!(receipt.diagnostics.outstanding_frames, 0);
        assert_eq!(receipt.diagnostics.state, ReferenceOutputState::Failed);
        assert!(!receipt.all_resources_released());
    }

    #[test]
    fn ordinary_stop_admission_never_waits_for_blocking_session_shutdown() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device) = module_with_shutdown_delay(&request, Duration::from_millis(150));
        module.open(&device, request, 0).expect("open");

        let started = Instant::now();
        let stopping = module.begin_stop();
        assert!(started.elapsed() < Duration::from_millis(75));

        let outcome = stopping.finish_until(Instant::now() + Duration::from_secs(1));
        let ReferenceOutputModuleStopOutcome::Stopped(module) = outcome else {
            panic!("ordinary stop should return a reusable Module");
        };
        assert_eq!(module.diagnostics().state, ReferenceOutputState::Stopped);
        let receipt = module.shutdown_until(Instant::now() + Duration::from_secs(1));
        assert!(receipt.session.session_present);
        assert!(receipt.session.coordinator.required);
        assert!(receipt.session.coordinator.joined);
        assert!(receipt.all_resources_released());
    }

    #[test]
    fn ordinary_stop_timeout_is_bounded_and_fail_closed() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device) = module_with_shutdown_delay(&request, Duration::from_millis(150));
        module.open(&device, request, 0).expect("open");

        let started = Instant::now();
        let outcome = module.begin_stop().finish_until(Instant::now() + Duration::from_millis(5));
        assert!(started.elapsed() < Duration::from_millis(75));
        let ReferenceOutputModuleStopOutcome::Terminal(receipt) = outcome else {
            panic!("deadline must not recover a still-running Module");
        };
        assert!(receipt.session.coordinator.timed_out);
        assert!(receipt.session.coordinator.detached);
        assert!(!receipt.all_resources_released());
        thread::sleep(Duration::from_millis(200));
    }

    #[test]
    fn ordinary_stop_finished_after_deadline_cannot_be_promoted_to_clean() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device, adapter_dropped) = drop_tracked_module(&request);
        module.open(&device, request, 0).expect("open");
        let deadline = Instant::now();
        let stopping = module.begin_stop();
        while !stopping.is_finished() {
            thread::yield_now();
        }

        let outcome = stopping.finish_until(deadline);
        let ReferenceOutputModuleStopOutcome::Terminal(receipt) = outcome else {
            panic!("late completion must remain fail closed");
        };
        assert!(receipt.session.coordinator.timed_out);
        assert!(receipt.session.coordinator.detached);
        assert!(!receipt.all_resources_released());
        let cleanup_deadline = Instant::now() + Duration::from_secs(1);
        while !adapter_dropped.load(Ordering::Acquire) && Instant::now() < cleanup_deadline {
            thread::yield_now();
        }
        assert!(adapter_dropped.load(Ordering::Acquire));
    }

    #[test]
    fn ordinary_stop_spawn_failure_never_drops_module_on_caller() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device, adapter_dropped) = drop_tracked_module(&request);
        module.open(&device, request, 0).expect("open");

        let stopping = begin_module_stop_with_spawner(module, |_work| {
            Err("synthetic ordinary stop spawn failure".to_owned())
        });
        let outcome = stopping.finish_until(Instant::now());
        let ReferenceOutputModuleStopOutcome::Terminal(receipt) = outcome else {
            panic!("spawn failure must be terminal");
        };
        assert!(!receipt.session.coordinator.spawned);
        assert!(receipt.session.coordinator.owner_abandoned);
        assert!(!receipt.all_resources_released());
        assert!(!adapter_dropped.load(Ordering::Acquire));
    }

    #[test]
    fn ordinary_stop_panicking_spawner_abandons_owner_off_caller() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device, adapter_dropped) =
            drop_tracked_module_with_delay(&request, Duration::from_millis(150));
        module.open(&device, request, 0).expect("open");

        let started = Instant::now();
        let stopping = begin_module_stop_with_spawner(module, |_work| {
            panic!("synthetic ordinary stop spawner panic")
        });
        assert!(started.elapsed() < Duration::from_millis(75));
        let outcome = stopping.finish_until(Instant::now());
        let ReferenceOutputModuleStopOutcome::Terminal(receipt) = outcome else {
            panic!("spawner panic must be terminal");
        };
        assert!(receipt.session.coordinator.owner_abandoned);
        assert!(!adapter_dropped.load(Ordering::Acquire));
    }

    #[test]
    fn panicking_reaper_spawner_never_drops_completed_module_on_caller() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device, adapter_dropped) = drop_tracked_module(&request);
        module.open(&device, request, 0).expect("open");
        let mut stopping = module.begin_stop();
        while !stopping.is_finished() {
            thread::yield_now();
        }
        let handle = stopping.handle.take().expect("completed coordinator handle");
        drop(stopping);

        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            detach_stop_coordinator_with_spawner(handle, |_work| {
                panic!("synthetic reaper spawner panic")
            })
        }));
        assert!(matches!(result, Ok(false)));
        assert!(!adapter_dropped.load(Ordering::Acquire));
    }

    #[test]
    fn ordinary_stop_preserves_stale_provider_receipt_schema() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device) = module_with_stale_shutdown_schema(&request);
        module.open(&device, request, 0).expect("open");

        let outcome = module.begin_stop().finish_until(Instant::now() + Duration::from_secs(1));
        let ReferenceOutputModuleStopOutcome::Terminal(receipt) = outcome else {
            panic!("stale provider schema must fail closed");
        };
        assert_eq!(receipt.schema_version, 2);
        assert_eq!(receipt.session.schema_version, 1);
        assert!(receipt.session.coordinator.joined);
        assert!(!receipt.all_resources_released());
    }

    #[test]
    fn ordinary_stop_request_failure_still_consumes_owner_on_coordinator() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device) = module_with_begin_shutdown_panic(&request);
        module.open(&device, request, 0).expect("open");

        let stopping = module.begin_stop();
        assert!(!stopping.shutdown_request_admitted());
        let outcome = stopping.finish_until(Instant::now() + Duration::from_secs(1));
        let ReferenceOutputModuleStopOutcome::Terminal(receipt) = outcome else {
            panic!("failed request admission must remain terminal");
        };
        assert!(receipt.session.coordinator.joined);
        assert!(!receipt.session.shutdown_request_completed);
        assert_eq!(
            receipt
                .session
                .provider_failure
                .as_ref()
                .map(|failure| failure.operation.as_str()),
            Some("begin_shutdown")
        );
        assert!(!receipt.all_resources_released());
        thread::sleep(Duration::from_millis(120));
    }

    #[test]
    fn bounded_shutdown_signals_first_and_joins_clean_coordinator() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device, adapter_dropped) = drop_tracked_module(&request);
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");

        module.begin_shutdown().expect("non-blocking signal");
        let receipt = module.shutdown_until(Instant::now() + Duration::from_secs(1));

        assert_eq!(receipt.outstanding_frames_before_shutdown, 1);
        assert!(receipt.session.shutdown_request_completed);
        assert!(receipt.session.coordinator.required);
        assert!(receipt.session.coordinator.spawned);
        assert!(receipt.session.coordinator.joined);
        assert!(!receipt.session.coordinator.panicked);
        assert!(!receipt.session.coordinator.timed_out);
        assert!(!receipt.session.coordinator.detached);
        assert!(receipt.all_resources_released());
        assert!(adapter_dropped.load(Ordering::Acquire));
    }

    #[test]
    fn bounded_shutdown_never_upgrades_a_stale_provider_receipt_schema() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device) = module_with_stale_shutdown_schema(&request);
        module.open(&device, request, 0).expect("open");

        let receipt = module.shutdown_until(Instant::now() + Duration::from_secs(1));

        assert_eq!(receipt.schema_version, 2);
        assert_eq!(receipt.session.schema_version, 1);
        assert!(receipt.session.coordinator.spawned);
        assert!(receipt.session.coordinator.joined);
        assert_eq!(receipt.diagnostics.state, ReferenceOutputState::Failed);
        assert!(!receipt.session.all_resources_released());
        assert!(!receipt.all_resources_released());
    }

    #[test]
    fn bounded_never_opened_module_still_coordinates_adapter_destruction() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (module, _device, adapter_dropped) = drop_tracked_module(&request);

        let receipt = module.shutdown_until(Instant::now() + Duration::from_secs(1));

        assert!(!receipt.session.session_present);
        assert!(receipt.session.coordinator.required);
        assert!(receipt.session.coordinator.spawned);
        assert!(receipt.session.coordinator.joined);
        assert_eq!(receipt.session.outstanding_resources, 0);
        assert!(receipt.all_resources_released());
        assert!(adapter_dropped.load(Ordering::Acquire));
    }

    #[test]
    fn bounded_provider_failure_remains_fail_closed_after_clean_join() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device) = module_with_stop_failure(&request, []);
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");

        assert!(module.begin_shutdown().is_err());
        let receipt = module.shutdown_until(Instant::now() + Duration::from_secs(1));

        assert!(receipt.session.coordinator.spawned);
        assert!(receipt.session.coordinator.joined);
        assert!(!receipt.session.shutdown_request_completed);
        assert!(receipt.session.provider_failure.is_some());
        assert!(receipt.module_failure.is_some());
        assert!(!receipt.all_resources_released());
    }

    #[test]
    fn begin_shutdown_provider_panic_is_latched_and_does_not_escape_signal_phase() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device) = module_with_begin_shutdown_panic(&request);
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");

        let signal = module.begin_shutdown();
        assert!(matches!(
            signal,
            Err(ReferenceOutputError::SessionShutdownRequestFailed { .. })
        ));

        let receipt = module.shutdown_until(Instant::now() + Duration::from_secs(1));
        assert_eq!(receipt.outstanding_frames_before_shutdown, 1);
        assert!(receipt.session.coordinator.spawned);
        assert!(receipt.session.coordinator.joined);
        assert!(!receipt.session.shutdown_request_completed);
        assert_eq!(
            receipt
                .session
                .provider_failure
                .as_ref()
                .map(|failure| failure.operation.as_str()),
            Some("begin_shutdown")
        );
        assert!(receipt.module_failure.is_some());
        assert!(!receipt.all_resources_released());
    }

    #[test]
    fn bounded_shutdown_records_coordinator_panic_without_unbounded_join() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device) = module_with_shutdown_panic(&request);
        module.open(&device, request, 0).expect("open");

        let receipt = module.shutdown_until(Instant::now() + Duration::from_secs(1));

        assert!(receipt.session.coordinator.spawned);
        assert!(receipt.session.coordinator.joined);
        assert!(receipt.session.coordinator.panicked);
        assert!(!receipt.session.coordinator.detached);
        assert_eq!(
            receipt
                .session
                .provider_failure
                .as_ref()
                .map(|failure| failure.operation.as_str()),
            Some("coordinator_panic")
        );
        assert!(!receipt.all_resources_released());
    }

    #[test]
    fn bounded_shutdown_times_out_and_detaches_at_absolute_deadline() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device) = module_with_shutdown_delay(&request, Duration::from_millis(100));
        module.open(&device, request, 0).expect("open");

        let deadline = Instant::now() + Duration::from_millis(5);
        let receipt = module.shutdown_until(deadline);

        assert!(receipt.session.coordinator.spawned);
        assert!(!receipt.session.coordinator.joined);
        assert!(receipt.session.coordinator.timed_out);
        assert!(receipt.session.coordinator.detached);
        assert_eq!(
            receipt
                .session
                .provider_failure
                .as_ref()
                .map(|failure| failure.operation.as_str()),
            Some("coordinator_timeout")
        );
        assert!(!receipt.all_resources_released());
    }

    #[test]
    fn whole_module_finished_after_deadline_cannot_be_promoted_to_clean() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (module, _device, adapter_dropped) = drop_tracked_module(&request);
        let deadline = Instant::now();
        let receipt = shutdown_module_until_with_spawner(module, deadline, |work| {
            let handle = thread::spawn(work);
            while !handle.is_finished() {
                thread::yield_now();
            }
            Ok(handle)
        });

        assert!(adapter_dropped.load(Ordering::Acquire));
        assert!(receipt.session.coordinator.timed_out);
        assert!(receipt.session.coordinator.detached);
        assert!(!receipt.all_resources_released());
    }

    #[test]
    fn bounded_shutdown_deadline_covers_blocking_adapter_destruction() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (module, _device, adapter_dropped) =
            drop_tracked_module_with_delay(&request, Duration::from_millis(100));

        let receipt = module.shutdown_until(Instant::now() + Duration::from_millis(5));

        assert!(receipt.session.coordinator.spawned);
        assert!(!receipt.session.coordinator.joined);
        assert!(receipt.session.coordinator.timed_out);
        assert!(receipt.session.coordinator.detached);
        assert!(!adapter_dropped.load(Ordering::Acquire));
        assert!(!receipt.all_resources_released());
        let cleanup_deadline = Instant::now() + Duration::from_secs(1);
        while !adapter_dropped.load(Ordering::Acquire) && Instant::now() < cleanup_deadline {
            thread::yield_now();
        }
        assert!(adapter_dropped.load(Ordering::Acquire));
    }

    #[test]
    fn bounded_shutdown_spawn_failure_does_not_drop_provider_on_caller() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device, adapter_dropped) = drop_tracked_module(&request);
        module.open(&device, request.clone(), 0).expect("open");
        module.schedule(bundle(&request, 0)).expect("frame 0");

        let receipt = shutdown_module_until_with_spawner(
            module,
            Instant::now() + Duration::from_secs(1),
            |_work| Err("synthetic coordinator spawn failure".to_owned()),
        );

        assert!(receipt.session.session_present);
        assert!(!receipt.session.coordinator.spawned);
        assert!(!receipt.session.coordinator.joined);
        assert!(!receipt.session.coordinator.detached);
        assert!(receipt.session.coordinator.owner_abandoned);
        assert_eq!(receipt.session.outstanding_resources, 1);
        assert_eq!(receipt.outstanding_frames_before_shutdown, 1);
        assert_eq!(
            receipt
                .session
                .provider_failure
                .as_ref()
                .map(|failure| failure.operation.as_str()),
            Some("coordinator_spawn")
        );
        assert!(!adapter_dropped.load(Ordering::Acquire));
        assert!(!receipt.all_resources_released());
    }

    #[test]
    fn bounded_shutdown_panicking_spawner_does_not_drop_provider_on_caller() {
        let request = request(ReferenceOutputReferencePolicy::FreeRunAllowed);
        let (mut module, device, adapter_dropped) =
            drop_tracked_module_with_delay(&request, Duration::from_millis(150));
        module.open(&device, request, 0).expect("open");

        let started = Instant::now();
        let receipt = shutdown_module_until_with_spawner(
            module,
            Instant::now() + Duration::from_secs(1),
            |_work| panic!("synthetic Module shutdown spawner panic"),
        );

        assert!(started.elapsed() < Duration::from_millis(75));
        assert!(!receipt.session.coordinator.spawned);
        assert!(receipt.session.coordinator.owner_abandoned);
        assert!(!adapter_dropped.load(Ordering::Acquire));
        assert!(!receipt.all_resources_released());
    }
}
