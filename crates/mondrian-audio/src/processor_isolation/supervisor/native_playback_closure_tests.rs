//! Real Product/owned-DLL Runtime retirement through Media's original worker.
//! Lifecycle evidence only: no AppTimeline dynamic wiring, DSP/Note, construction
//! closure, original-owner final reap, 100ms, RT or complete-heap qualification.
use super::*;
use crate::processor_isolation::{
    DiscoveredVst3AudioProcessorSpecResolver, IsolatedAudioProcessorResolver,
};
use crate::{
    AudioMediaResolver, AudioProgramRuntime, AudioRuntimeResourceGrant,
    NativeAudioObjectObservation, ResolvedAudioSource,
};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioOwnerInstallationClaim, AudioOwnerInstallationError,
    AudioOwnerInstallationVerifier, AudioSamplePosition, AudioSampleRate, AudioSourceComponentId,
    ExecutionCancellationToken,
};
use mondrian_media::{
    AudioBuffer, AudioOutputAdapter, AudioPcmContinuityModel, AudioPcmNativeOwnerBindingRefusal,
    AudioPcmNativeOwnerBindingStatus, AudioPcmRenderOutcome, AudioPcmRenderRequest,
    AudioPcmRenderer, AudioPlayback, AudioPlaybackConfig, AudioPlaybackShutdownEvidence,
    RealtimeAudioOutputControlError, RealtimeAudioOutputEnqueueError, RealtimeAudioOutputEvent,
    RealtimeAudioOutputQuiescenceToken, RealtimeAudioOutputSnapshot,
};
use mondrian_timeline::Sequence;
use std::os::windows::io::{AsHandle, AsRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, TerminateProcess, WaitForSingleObject,
};

const CLASS: &str = "AE4C8B1910234F819CA26D315F8702A1";
const WAIT_OBJECT: u32 = 0;
const WAIT_TIMEOUT: u32 = 258;

// Not a Child or production cleanup owner. Duplicate the original Child handle,
// never reopen a PID. A fallback Drop also prevents an earlier setup panic from
// leaving this deliberately unconfirmed test child alive in the outer Job.
struct RetainedHandle(Option<OwnedHandle>);
impl RetainedHandle {
    fn cleanup(&mut self) -> io::Result<(bool, u32)> {
        let Some(handle) = &self.0 else {
            return Err(io::Error::other("test handle was already consumed"));
        };
        // SAFETY: this is an owned duplicate of the original real Child handle.
        let observed = unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) };
        let already_exited = observed == WAIT_OBJECT;
        if observed == WAIT_TIMEOUT {
            // Test-only cleanup AFTER the top-level receipt was copied. This
            // cannot update the now-dropped ProcessOwner's authoritative state.
            if unsafe { TerminateProcess(handle.as_raw_handle().cast(), 73) } == 0 {
                // Natural EOF exit may race the test-only termination request.
                if unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) } != WAIT_OBJECT {
                    return Err(io::Error::last_os_error());
                }
            }
        } else if !already_exited {
            return Err(io::Error::last_os_error());
        }
        if unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 2_000) } != WAIT_OBJECT {
            return Err(io::Error::other(
                "same-process test cleanup did not observe exit",
            ));
        }
        let mut exit = u32::MAX;
        if unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit) } == 0 {
            return Err(io::Error::last_os_error());
        }
        self.0.take();
        Ok((already_exited, exit))
    }
}
impl Drop for RetainedHandle {
    fn drop(&mut self) {
        if self.0.is_some() {
            let _ = self.cleanup();
        }
    }
}

#[derive(Default)]
struct Probe {
    creates: usize,
    ready: Option<NativeAudioObjectObservation>,
    handle: Option<RetainedHandle>,
    drops: usize,
    drop_thread: Option<String>,
    before: Option<ProcessState>,
    after: Option<ProcessState>,
    cleanup: Option<CleanupObservation>,
    cleanup_error: Option<AudioProcessorHostError>,
    graceful: Option<bool>,
    renders: usize,
    settlement_forwards: usize,
    settlement_forward_successes: usize,
    child_producer_registered: bool,
}

fn refuse_observation(_: &mut Child) -> io::Result<Option<std::process::ExitStatus>> {
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "owned cleanup observation refused",
    ))
}
fn refuse_kill(_: &mut Child) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "owned cleanup kill refused",
    ))
}
fn forbid_wait(_: &mut Child) -> io::Result<std::process::ExitStatus> {
    Err(io::Error::other("wait must not follow a refused kill"))
}

fn attach_retained(
    worker: &mut SupervisedAudioWorker,
    probe: Arc<Mutex<Probe>>,
    refuse: bool,
) -> Result<(), AudioProcessorHostError> {
    let object = worker.observe_native_audio_object()?.expect("actual retained Ready object");
    assert_eq!(object.process_id().get(), worker.process.child.id());
    assert_ne!(object.process_id().get(), std::process::id());
    let handle = worker
        .process
        .child
        .as_handle()
        .try_clone_to_owned()
        .map_err(|error| worker_failed("test retained process handle", error))?;
    let mut state = probe.lock().unwrap();
    state.creates += 1;
    assert!(
        state.handle.is_none(),
        "one actual retained Runtime processor"
    );
    state.ready = Some(object);
    state.handle = Some(RetainedHandle(Some(handle)));
    drop(state);
    worker.before_drop_test_hook = Some(Box::new(move |worker| {
        let child_producer_registered = worker.process.settlement_producer.is_some();
        let before = worker.process.state;
        let mut cleanup = CleanupObservation {
            origin: Instant::now(),
            initial_observe: None,
            kill: None,
            final_observe: None,
            wait: None,
        };
        let (error, graceful) = if refuse {
            worker.process.cleanup_hooks = Some((refuse_observation, refuse_kill, forbid_wait));
            (
                worker.process.terminate_observed(Some(&mut cleanup)).err(),
                None,
            )
        } else {
            (None, Some(worker.shutdown_observed().successful()))
        };
        let mut state = probe.lock().unwrap();
        state.drops += 1;
        state.drop_thread = thread::current().name().map(str::to_owned);
        state.before = Some(before);
        state.after = Some(worker.process.state);
        state.cleanup = Some(cleanup);
        state.cleanup_error = error;
        state.graceful = graceful;
        state.child_producer_registered = child_producer_registered;
        // Return to the SAME original SupervisedAudioWorker::Drop and then
        // ProcessOwner::Drop. No fake Settled assignment or second state machine.
    }));
    Ok(())
}

// Legitimately owns no device, callback, stream or worker. It cannot accept PCM.
struct NoDevice;
impl AudioOutputAdapter for NoDevice {
    fn poll(&mut self) -> Option<RealtimeAudioOutputEvent> {
        None
    }
    fn enqueue(&mut self, _: &AudioBuffer) -> Result<(), RealtimeAudioOutputEnqueueError> {
        Err(RealtimeAudioOutputEnqueueError::OutputUnavailable)
    }
    fn clear(&mut self) {}
    fn validate_deactivation(&self) -> Result<(), RealtimeAudioOutputControlError> {
        Ok(())
    }
    fn deactivate(
        &self,
    ) -> Result<Option<RealtimeAudioOutputQuiescenceToken>, RealtimeAudioOutputControlError> {
        Ok(None)
    }
    fn is_quiescent(
        &self,
        _: RealtimeAudioOutputQuiescenceToken,
    ) -> Result<bool, RealtimeAudioOutputControlError> {
        Err(RealtimeAudioOutputControlError::OutputUnavailable)
    }
    fn activate_after_discard(
        &self,
        _: RealtimeAudioOutputQuiescenceToken,
        _: usize,
    ) -> Result<(), RealtimeAudioOutputControlError> {
        Err(RealtimeAudioOutputControlError::OutputUnavailable)
    }
    fn buffered_frames(&self) -> usize {
        0
    }
    fn capacity_frames(&self) -> Option<usize> {
        None
    }
    fn snapshot(&self) -> Option<RealtimeAudioOutputSnapshot> {
        None
    }
}

// Owns the real Runtime, with no second native execution or cleanup model.
// This test schedules no PCM; any accidental render still calls the real Runtime.
struct RuntimeRenderer {
    runtime: Mutex<AudioProgramRuntime>,
    probe: Arc<Mutex<Probe>>,
}
fn render_error(error: impl ToString) -> mondrian_core::MondrianError {
    mondrian_core::MondrianError::WorkflowStepFailed {
        step_id: "owned_native_closure".into(),
        reason: error.to_string(),
    }
}
impl AudioPcmRenderer for RuntimeRenderer {
    fn bind_native_child_settlement(
        &self,
        registrar: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<(), mondrian_core::NativeChildSettlementError> {
        let result = self.runtime.lock().unwrap().bind_native_child_settlement(registrar);
        let mut probe = self.probe.lock().unwrap();
        probe.settlement_forwards += 1;
        if result.is_ok() {
            probe.settlement_forward_successes += 1;
        }
        result
    }

    fn owner_installation_verifier(&self) -> Option<AudioOwnerInstallationVerifier> {
        self.runtime.lock().unwrap().native_owner_installation_verifier()
    }
    fn bind_owner_installation(
        &self,
        verifier: AudioOwnerInstallationVerifier,
    ) -> Result<bool, AudioOwnerInstallationError> {
        self.runtime.lock().unwrap().bind_native_owner_installation(verifier)?;
        Ok(true)
    }
    fn install_native_owner_bindings(
        &self,
        claim: &AudioOwnerInstallationClaim,
        cancellation: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<AudioPcmNativeOwnerBindingStatus> {
        let count = self
            .runtime
            .lock()
            .unwrap()
            .install_native_owner_bindings(claim, cancellation)
            .map_err(render_error)?;
        Ok(if count == 0 {
            AudioPcmNativeOwnerBindingStatus::Unavailable(
                AudioPcmNativeOwnerBindingRefusal::UnsupportedAdapter,
            )
        } else {
            AudioPcmNativeOwnerBindingStatus::Ready
        })
    }
    fn render(
        &self,
        request: AudioPcmRenderRequest,
        cancellation: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
        self.probe.lock().unwrap().renders += 1;
        let mut output = AudioBuffer::silent(
            request.sample_rate,
            request.channel_layout,
            request.frame_count,
        );
        self.runtime
            .lock()
            .unwrap()
            .render_into_cancellable(
                crate::AudioRenderRequest {
                    transport_activity: None,
                    start_sample: request.start_sample,
                    frames: request.frame_count,
                },
                &mut output.samples,
                cancellation,
            )
            .map_err(render_error)?;
        Ok(AudioPcmRenderOutcome::Rendered(output))
    }
}
struct NoMedia;
impl AudioMediaResolver for NoMedia {
    fn resolve(
        &self,
        _: AssetId,
        _: AudioSourceComponentId,
        _: u32,
    ) -> Result<ResolvedAudioSource, String> {
        Err("this lifecycle graph has no media sources".into())
    }
}

fn material(name: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var_os(name).expect("explicit matched native material"));
    assert!(path.is_absolute() && path.is_file(), "{name}");
    path
}

fn observe_closure(refuse: bool) -> AudioPlaybackShutdownEvidence {
    let helper = material("MONDRIAN_VST3_TEST_HELPER");
    let plugin = material("MONDRIAN_VST3_STATEFUL_TEST_PLUGIN");
    let catalog = Arc::new(
        DiscoveredVst3AudioProcessorSpecResolver::discover(helper, [plugin])
            .expect("actual normal Product discovery"),
    );
    let render = AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 4,
        processing_mode: crate::AudioProcessingMode::Offline,
        // Finite one-owner setup allowance, not a budget-edge qualification.
        processor_session_scratch_budget_bytes: 32 * 1024 * 1024,
        public_output_lookahead_budget_frames: 1024,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    };
    let author = catalog.create_instance(CLASS, render, None).expect("actual owned gain capture");
    let mut sequence = Sequence::new("native playback closure evidence");
    sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(author);
    let probe = Arc::new(Mutex::new(Probe::default()));
    let hook_probe = Arc::clone(&probe);
    let mut resolver = IsolatedAudioProcessorResolver::new(catalog);
    resolver.retained_create_test_hook = Some(Arc::new(move |worker| {
        attach_retained(worker, Arc::clone(&hook_probe), refuse)
    }));
    let runtime = AudioProgramRuntime::build_with_processor_resolver_and_resource_grant(
        &sequence,
        &[],
        &NoMedia,
        &resolver,
        render,
        None,
        AudioRuntimeResourceGrant::new(1, 64 * 1024 * 1024, 32 * 1024 * 1024),
    )
    .expect("original actual Runtime/factory/retained Ready path");
    drop(resolver);
    drop(sequence);
    let renderer: Arc<dyn AudioPcmRenderer> = Arc::new(RuntimeRenderer {
        runtime: Mutex::new(runtime),
        probe: Arc::clone(&probe),
    });
    let mut playback = AudioPlayback::with_output(
        AudioPlaybackConfig {
            chunk_frames: 4,
            preroll_frames: 4,
            high_watermark_frames: 8,
            max_in_flight: 1,
            ..AudioPlaybackConfig::product_default()
        },
        Box::new(NoDevice),
    )
    .expect("real Media render and retirement workers");
    playback
        .prepare(
            AudioSamplePosition::new(0, AudioSampleRate::new(48_000).unwrap()),
            renderer,
            AudioPcmContinuityModel::GenerationState,
        )
        .expect("real Playback owns the only renderer Arc");
    // No external renderer/Runtime Arc survives this call. The original Media
    // retirement worker performs the actual Runtime -> Supervisor -> Child Drop.
    let evidence = playback.shutdown_and_wait();
    let mut state = probe.lock().unwrap();
    let (already_exited, exit) = state
        .handle
        .as_mut()
        .expect("same Child handle captured at Ready")
        .cleanup()
        .expect("test-only same-handle cleanup before any target assertion");
    state.handle.take();
    assert_eq!(state.creates, 1);
    assert_eq!(state.drops, 1);
    assert!(state.ready.is_some());
    assert_eq!(state.before, Some(ProcessState::Running));
    assert_eq!(
        state.drop_thread.as_deref(),
        Some("mondrian-audio-render-retirement")
    );
    assert_eq!(state.renders, 0, "lifecycle-only gate scheduled no PCM");
    assert_eq!(
        state.settlement_forwards, 1,
        "aggregate forwarded the actual Runtime"
    );
    assert_eq!(
        state.settlement_forward_successes, 1,
        "actual typed instances bound"
    );
    assert!(
        state.child_producer_registered,
        "the original ProcessOwner held the producer"
    );
    if refuse {
        assert_eq!(state.after, Some(ProcessState::Retiring));
        assert!(matches!(
            state.cleanup_error,
            Some(AudioProcessorHostError::WorkerFailed { operation: "native child cleanup", .. })
        ));
        let cleanup = state.cleanup.as_ref().unwrap();
        for observed in [&cleanup.initial_observe, &cleanup.final_observe] {
            assert!(
                matches!(observed.as_ref().map(|value| value.outcome), Some(ChildObservation::Io(failure))
                if failure.kind == io::ErrorKind::PermissionDenied)
            );
        }
        assert!(
            matches!(cleanup.kill.as_ref().map(|value| value.outcome), Some(Err(failure))
            if failure.kind == io::ErrorKind::PermissionDenied)
        );
        assert!(cleanup.wait.is_none());
        assert_eq!(state.graceful, None);
    } else {
        assert_eq!(state.after, Some(ProcessState::Settled));
        assert_eq!(
            state.graceful,
            Some(true),
            "original shutdown must succeed without retry"
        );
        assert!(state.cleanup_error.is_none());
        assert!(already_exited);
        assert_eq!(exit, 0);
    }
    // Explicit worker evidence is independent of the two target closure flags.
    assert_eq!(
        (
            evidence.render_workers_started,
            evidence.render_workers_terminated
        ),
        (1, 1)
    );
    assert_eq!(
        (
            evidence.render_retirement_workers_started,
            evidence.render_retirement_workers_terminated
        ),
        (1, 1)
    );
    assert_eq!(evidence.render_worker_panics, 0);
    assert_eq!(evidence.render_worker_owner_abandonments, 0);
    assert_eq!(evidence.render_worker_terminal_evidence_missing, 0);
    assert_eq!(evidence.render_current_thread_detachments, 0);
    assert_eq!(evidence.render_retirement_worker_panics, 0);
    assert_eq!(evidence.render_retirement_worker_owner_abandonments, 0);
    assert_eq!(
        evidence.render_retirement_worker_terminal_evidence_missing,
        0
    );
    assert_eq!(evidence.render_retirement_current_thread_detachments, 0);
    assert_eq!(evidence.foreign_owner_panics, 0);
    assert_eq!(evidence.foreign_owner_abandonments, 0);
    assert_eq!(evidence.shutdown_coordinators_started, 0);
    assert_eq!(evidence.shutdown_coordinators_terminated, 0);
    assert_eq!(evidence.shutdown_coordinator_start_failures, 0);
    assert_eq!(evidence.shutdown_coordinator_panics, 0);
    assert_eq!(evidence.shutdown_coordinator_timeouts, 0);
    assert_eq!(evidence.shutdown_coordinator_detachments, 0);
    assert_eq!(evidence.shutdown_coordinator_spawner_panics, 0);
    assert_eq!(evidence.shutdown_coordinator_owner_abandonments, 0);
    assert!(evidence.output.all_workers_terminated());
    println!(
        "{}",
        serde_json::json!({
            "case": "native-playback-closure", "refused_cleanup": refuse,
            "ready_pid": state.ready.unwrap().process_id().get(), "drop_state": format!("{:?}", state.after.unwrap()),
            "render_joined": true, "retirement_joined": true, "panics": 0,
            "facts_complete": evidence.shutdown_resource_facts_complete_at_deadline,
            "owner_unresolved": evidence.shutdown_owner_lifetime_unresolved_at_deadline,
            "test_cleanup_already_exited": already_exited, "test_cleanup_exit": exit,
            "app_timeline_dynamic": false, "original_owner_reaped": !refuse,
            "pcm_qualification": false, "grace_100ms_qualification": false,
            "settlement_forwarded": true, "actual_child_producer_registered": true,
        })
    );
    evidence
}

#[test]
#[ignore = "matched normal Product and owned stateful gain DLL; real Runtime/Media retirement, no AppTimeline/DSP/100ms qualification"]
fn playback_closure_refuses_unconfirmed_native_cleanup() {
    let evidence = observe_closure(true);
    assert!(
        !evidence.shutdown_resource_facts_complete_at_deadline
            && evidence.shutdown_owner_lifetime_unresolved_at_deadline,
        "native cleanup unconfirmed must make playback closure incomplete and unresolved"
    );
}

#[test]
#[ignore = "matched normal Product and owned stateful gain DLL; original confirmed native cleanup through Media retirement"]
fn playback_closure_accepts_confirmed_native_cleanup() {
    let evidence = observe_closure(false);
    assert!(evidence.shutdown_resource_facts_complete_at_deadline);
    assert!(!evidence.shutdown_owner_lifetime_unresolved_at_deadline);
}
