//! Actual serial Media queue/Core Source with a portable two-member provider.
//! This is not canonical Session publication, an SDK or a real plugin workflow.
use super::*;
use author_batch::{Continuation, Lane};
use std::sync::atomic::AtomicUsize;

#[derive(Default)]
struct Control {
    published: AtomicBool,
    prepare_entered: AtomicBool,
    prepare_gate: AtomicBool,
    install_entered: AtomicBool,
    install_gate: AtomicBool,
    refuse_install: AtomicBool,
    fail_second_ack: AtomicBool,
    panic_prepared_drop: AtomicBool,
    panic_request_drop: AtomicBool,
    retire_entered: AtomicBool,
    retire_gate: AtomicBool,
    prepares: AtomicUsize,
    installs: AtomicUsize,
    acks: [AtomicUsize; 2],
    prepared_drops: Mutex<Vec<thread::ThreadId>>,
    request_drops: Mutex<Vec<thread::ThreadId>>,
}
struct Preparation(Arc<Control>);
impl Drop for Preparation {
    fn drop(&mut self) {
        self.0.prepared_drops.lock().push(thread::current().id());
        assert!(
            !self.0.panic_prepared_drop.load(Ordering::Acquire),
            "controlled prepared Drop panic"
        );
    }
}
struct BatchRequest {
    renderer: Arc<Renderer>,
    erased: Arc<dyn AudioPcmRenderer>,
    proposal: NativeAudioAuthorProposal,
    target: Target,
    control: Arc<Control>,
    destination: Option<AudioOwnerInstallationVerifier>,
    prepared: Option<Preparation>,
    installed: Option<(AudioOwnerInstallationClaim, Target)>,
    next_ack: usize,
}
impl Drop for BatchRequest {
    fn drop(&mut self) {
        self.control.retire_entered.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.control.retire_gate.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(
            !self.control.retire_gate.load(Ordering::Acquire),
            "bounded retirement gate"
        );
        self.control.request_drops.lock().push(thread::current().id());
        assert!(
            !self.control.panic_request_drop.load(Ordering::Acquire),
            "controlled request Drop panic"
        );
    }
}
impl BatchRequest {
    fn ack_remaining(
        &mut self,
        cancel: &ExecutionCancellationToken,
    ) -> AudioPcmNativeAuthorTerminalBatchOutcome {
        let (claim, target) = self.installed.as_ref().expect("actual Source publication").clone();
        while self.next_ack < 2 {
            if cancel.is_canceled()
                || (self.next_ack == 1 && self.control.fail_second_ack.load(Ordering::Acquire))
            {
                return AudioPcmNativeAuthorTerminalBatchOutcome::InstalledAwaitingAcknowledgement {
                    claim,
                    target,
                };
            }
            self.control.acks[self.next_ack].fetch_add(1, Ordering::AcqRel);
            self.next_ack += 1;
        }
        // The literal model target was actually installed above. Copy it before
        // dropping the preparation on the producer, never in a Source guard.
        drop(self.prepared.take());
        self.renderer.pending.store(false, Ordering::Release);
        AudioPcmNativeAuthorTerminalBatchOutcome::Acknowledged { claim, target }
    }
}
impl AudioPcmNativeAuthorHandoff for BatchRequest {
    fn mode(&self) -> AudioPcmNativeAuthorHandoffMode {
        AudioPcmNativeAuthorHandoffMode::TerminalBatch
    }
    fn renderer(&self) -> &Arc<dyn AudioPcmRenderer> {
        &self.erased
    }
    fn proposal(&self) -> &NativeAudioAuthorProposal {
        &self.proposal
    }
    fn expected_target(&self) -> Target {
        self.target
    }
    fn intent(&self) -> AudioPcmNativeAuthorHandoffIntent {
        AudioPcmNativeAuthorHandoffIntent::Install
    }
    fn continuity_policy(
        &self,
        _: &ExecutionCancellationToken,
    ) -> Result<AudioPcmNativeAuthorHandoffContinuity, AudioPcmNativeAuthorHandoffRefusal> {
        panic!("batch cannot use Inline policy")
    }
    fn apply(
        &self,
        _: AudioOwnerInstallationVerifier,
        _: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
        _: &ExecutionCancellationToken,
    ) -> AudioPcmNativeAuthorHandoffOutcome {
        panic!("batch cannot use Inline apply")
    }
    fn acknowledge(&self, _: &ExecutionCancellationToken) -> AudioPcmNativeAuthorHandoffOutcome {
        panic!("batch cannot use Inline Ack")
    }
    fn prepare_terminal_batch(
        &mut self,
        destination: AudioOwnerInstallationVerifier,
        cancel: &ExecutionCancellationToken,
    ) -> Result<(), AudioPcmNativeAuthorHandoffRefusal> {
        assert!(self.prepared.is_none());
        self.control.prepares.fetch_add(1, Ordering::AcqRel);
        self.prepared = Some(Preparation(self.control.clone()));
        self.destination = Some(destination);
        self.control.prepare_entered.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.control.prepare_gate.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(
            !self.control.prepare_gate.load(Ordering::Acquire),
            "bounded producer gate"
        );
        if cancel.is_canceled() {
            return Err(AudioPcmNativeAuthorHandoffRefusal::Cancelled);
        }
        Ok(())
    }
    fn apply_prepared_terminal_batch(
        &mut self,
        authorization: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
        cancel: &ExecutionCancellationToken,
    ) -> AudioPcmNativeAuthorTerminalBatchOutcome {
        assert!(self.prepared.is_some());
        assert!(self.installed.is_none());
        self.control.install_entered.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.control.install_gate.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(
            !self.control.install_gate.load(Ordering::Acquire),
            "bounded install gate"
        );
        if !self.control.published.load(Ordering::Acquire)
            || self.control.refuse_install.load(Ordering::Acquire)
        {
            return AudioPcmNativeAuthorTerminalBatchOutcome::Refused(
                AudioPcmNativeAuthorHandoffRefusal::AuthorStatePending,
            );
        }
        let destination = self.destination.as_ref().unwrap().clone();
        // Real Core authorization and model ownership swap, with all allocation
        // before the short Source guard. No fabricated installed claim.
        let mut next = self.target;
        next.root_plan = mondrian_core::AudioPreparedPlanId::new();
        next.local_plan = next.root_plan;
        next.binding = mondrian_core::AudioProcessorBindingId::new();
        let mut verifier = self.renderer.installed.verifier.lock();
        let mut target = self.renderer.target.lock();
        let commit = authorization.begin_commit(&destination).expect("same original authorization");
        next.installed_generation = commit.generation();
        let retired = verifier.replace(destination);
        *target = Some(next);
        let claim = commit.commit();
        drop(target);
        drop(verifier);
        drop(retired);
        self.installed = Some((claim, next));
        self.control.installs.fetch_add(1, Ordering::AcqRel);
        self.ack_remaining(cancel)
    }
    fn acknowledge_terminal_batch(
        &mut self,
        cancel: &ExecutionCancellationToken,
    ) -> AudioPcmNativeAuthorTerminalBatchOutcome {
        self.ack_remaining(cancel)
    }
}
fn request(
    renderer: &Arc<Renderer>,
    target: Target,
    c: &Arc<Control>,
) -> Box<dyn AudioPcmNativeAuthorHandoff> {
    let proposal = NativeAudioAuthorProposal::project(
        target,
        mondrian_core::SequenceId::new(),
        31,
        NativeAudioAuthorBoundaryFacts {
            completion: mondrian_core::NativeAudioAuthorCompletion::ParametersOnly,
            open_parameter_edits: 0,
            group_depth: 0,
        },
        vec![NativeAudioAuthorCallback::Parameter {
            parameter: mondrian_core::ParameterId::new_static("native.gain"),
            phase: NativeAudioParameterCallbackPhase::Value,
            value_bits: Some(0.75f64.to_bits()),
        }],
        None,
    )
    .unwrap();
    Box::new(BatchRequest {
        renderer: renderer.clone(),
        erased: renderer.clone(),
        proposal,
        target,
        control: c.clone(),
        destination: None,
        prepared: None,
        installed: None,
        next_ack: 0,
    })
}
fn spin_until(predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !predicate() && Instant::now() < deadline {
        thread::yield_now();
    }
    assert!(predicate(), "bounded actual queue transition");
}
fn event(playback: &mut AudioPlayback) -> AudioPlaybackEvent {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        for event in playback.poll(AudioPlaybackMode::Idle, sample_position(0)).unwrap().events {
            if matches!(
                event,
                AudioPlaybackEvent::NativeAuthorTerminalBatchPrepared { .. }
                    | AudioPlaybackEvent::NativeAuthorTerminalBatchObserved { .. }
            ) {
                return event;
            }
        }
        thread::yield_now();
    }
    panic!("original batch completion not observed")
}
fn key(event: &AudioPlaybackEvent) -> AudioPcmNativeAuthorResumeKey {
    match event {
        AudioPlaybackEvent::NativeAuthorTerminalBatchPrepared { key, .. }
        | AudioPlaybackEvent::NativeAuthorTerminalBatchObserved { key, .. } => key.clone(),
        _ => panic!("batch DTO"),
    }
}
fn box_address(playback: &AudioPlayback) -> usize {
    let state = playback.render_queue.state.lock();
    match &state.author_batch {
        Lane::Parked(w) => (&**w as *const native_author::NativeAuthorHandoffWork) as usize,
        _ => panic!("actual parked owner"),
    }
}
fn with_case(
    panic_expected: bool,
    body: impl FnOnce(&mut AudioPlayback, &Arc<Renderer>, Target, &Arc<Control>),
) -> Arc<Control> {
    let (mut playback, _, renderer, target) = setup(false);
    let c = Arc::new(Control::default());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        body(&mut playback, &renderer, target, &c)
    }));
    c.prepare_gate.store(false, Ordering::Release);
    c.install_gate.store(false, Ordering::Release);
    c.retire_gate.store(false, Ordering::Release);
    drop(renderer);
    let evidence = playback.shutdown_and_wait();
    if let Err(payload) = result {
        if !evidence.all_workers_terminated() {
            eprintln!("batch cleanup after original failure: {evidence:?}");
        }
        std::panic::resume_unwind(payload)
    }
    assert_eq!(
        evidence.render_workers_started, evidence.render_workers_terminated,
        "{evidence:?}"
    );
    assert_eq!(
        evidence.render_retirement_workers_started, evidence.render_retirement_workers_terminated,
        "{evidence:?}"
    );
    if panic_expected {
        assert!(
            !evidence.all_workers_terminated(),
            "panic never becomes clean evidence"
        );
    } else {
        assert!(evidence.all_workers_terminated(), "{evidence:?}");
    }
    c
}

#[test]
fn terminal_batch_same_box_prepares_then_installs_after_one_real_resume() {
    let c = with_case(false, |p, r, t, c| {
        let producer = p.render_worker.as_ref().unwrap().thread().id();
        let retired = p.render_retirement_worker.as_ref().unwrap().thread().id();
        let req = request(r, t, c);
        let original = req.proposal().clone();
        assert!(p.request_native_author_handoff(req).unwrap());
        let first = event(p);
        assert!(
            matches!(&first,AudioPlaybackEvent::NativeAuthorTerminalBatchPrepared{proposal,..} if proposal.same_retained_proposal(&original))
        );
        assert!(p.native_author_state_entry_request.is_none());
        let k = key(&first);
        assert_eq!(c.prepares.load(Ordering::Acquire), 1);
        assert_eq!(c.installs.load(Ordering::Acquire), 0);
        assert!(c.prepared_drops.lock().is_empty());
        c.published.store(true, Ordering::Release);
        assert!(p.resume_native_author_handoff(&k).unwrap());
        let last = event(p);
        assert!(
            matches!(last,AudioPlaybackEvent::NativeAuthorTerminalBatchObserved{status:AudioPcmNativeAuthorTerminalBatchStatus::Acknowledged{target},..} if target.installed_generation!=t.installed_generation)
        );
        assert_eq!(c.prepared_drops.lock().as_slice(), &[producer]);
        assert_eq!(c.installs.load(Ordering::Acquire), 1);
        assert!(matches!(
            p.resume_native_author_handoff(&k),
            Err(AudioPlaybackError::OwnerInstallation(
                AudioOwnerInstallationError::Superseded
            ))
        ));
        spin_until(|| !c.request_drops.lock().is_empty());
        assert_eq!(c.request_drops.lock().as_slice(), &[retired]);
        assert!(!p.render_queue.batch_busy());
    });
    assert_eq!(c.prepares.load(Ordering::Acquire), 1);
    assert_eq!(c.acks[0].load(Ordering::Acquire), 1);
    assert_eq!(c.acks[1].load(Ordering::Acquire), 1);
}

#[test]
fn terminal_batch_no_cas_retry_and_partial_ack_keep_prepared_new_claim_and_fresh_keys() {
    with_case(false, |p, r, t, c| {
        assert!(p.request_native_author_handoff(request(r, t, c)).unwrap());
        let prepared = event(p);
        let k0 = key(&prepared);
        let address = box_address(p);
        assert!(p.resume_native_author_handoff(&k0).unwrap());
        let refusal = event(p);
        let k1 = key(&refusal);
        assert_ne!(k0, k1);
        assert_eq!(address, box_address(p));
        assert!(matches!(
            refusal,
            AudioPlaybackEvent::NativeAuthorTerminalBatchObserved {
                status: AudioPcmNativeAuthorTerminalBatchStatus::Refused(
                    AudioPcmNativeAuthorHandoffRefusal::AuthorStatePending
                ),
                ..
            }
        ));
        assert_eq!(
            p.owner_installation.current_claim().unwrap().unwrap().generation(),
            t.installed_generation
        );
        assert_eq!(c.prepares.load(Ordering::Acquire), 1);
        c.published.store(true, Ordering::Release);
        c.fail_second_ack.store(true, Ordering::Release);
        assert!(p.resume_native_author_handoff(&k1).unwrap());
        let partial = event(p);
        let k2 = key(&partial);
        assert_ne!(k1, k2);
        assert_eq!(box_address(p), address);
        let claim = p.owner_installation.current_claim().unwrap().unwrap().generation();
        assert_ne!(claim, t.installed_generation);
        assert!(
            matches!(partial,AudioPlaybackEvent::NativeAuthorTerminalBatchObserved{status:AudioPcmNativeAuthorTerminalBatchStatus::InstalledAwaitingAcknowledgement{target},..} if target.installed_generation==claim)
        );
        assert_eq!(c.acks[0].load(Ordering::Acquire), 1);
        assert_eq!(c.acks[1].load(Ordering::Acquire), 0);
        assert!(p.resume_native_author_handoff(&k1).is_err());
        assert_eq!(box_address(p), address);
        c.fail_second_ack.store(false, Ordering::Release);
        assert!(p.resume_native_author_handoff(&k2).unwrap());
        assert!(matches!(
            event(p),
            AudioPlaybackEvent::NativeAuthorTerminalBatchObserved {
                status: AudioPcmNativeAuthorTerminalBatchStatus::Acknowledged { .. },
                ..
            }
        ));
        assert_eq!(c.prepares.load(Ordering::Acquire), 1);
        assert_eq!(c.installs.load(Ordering::Acquire), 1);
        assert_eq!(c.acks[0].load(Ordering::Acquire), 1);
        assert_eq!(c.acks[1].load(Ordering::Acquire), 1);
        assert_eq!(
            p.owner_installation.current_claim().unwrap().unwrap().generation(),
            claim
        );
    });
}

#[test]
fn terminal_batch_queued_and_active_credit_use_the_original_control_frontier() {
    with_case(false, |p, r, t, c| {
        let q = Arc::new(RenderWorkQueue::new(
            2,
            p.render_retirement_queue.clone(),
            p.foreign_owner_retirements.clone(),
        ));
        let req = request(r, t, c);
        let continuation = Some(Box::new(Continuation::new(&q, req.as_ref())));
        let work =
            RenderWork::NativeAuthorHandoff(Box::new(native_author::NativeAuthorHandoffWork {
                generation: p.generation,
                renderer_id: p.renderer.as_ref().unwrap().materialization_id,
                request: req,
                source: p.owner_installation.clone(),
                expected: p.owner_installation.current_claim().unwrap().unwrap(),
                cancellation: ExecutionCancellationToken::new(),
                output_quiescent: false,
                continuation,
            }));
        assert!(q.try_push(work).is_ok());
        {
            let state = q.state.lock();
            assert!(matches!(state.author_batch, Lane::Queued(_)));
            assert_eq!(state.author_batch.outside_pending(), 0);
        }
        let work = q.pop().unwrap();
        {
            let state = q.state.lock();
            assert!(matches!(state.author_batch, Lane::Active(_)));
            assert_eq!(state.author_batch.outside_pending(), 1);
        }
        for _ in 0..2 {
            assert!(q
                .try_push(RenderWork::NativeEditor(Box::new(NativeEditorWork {
                    renderer_id: p.renderer.as_ref().unwrap().materialization_id,
                    renderer: r.clone(),
                    claim: p.owner_installation.current_claim().unwrap().unwrap(),
                    cancellation: ExecutionCancellationToken::new(),
                    intent: Intent::Close { target: t }
                })))
                .is_ok());
        }
        assert!(q
            .try_push(RenderWork::NativeObservations(NativeObservationWork {
                generation: p.generation,
                renderer_id: p.renderer.as_ref().unwrap().materialization_id,
                renderer: r.clone(),
                cancellation: ExecutionCancellationToken::new(),
                installation_generation: Some(t.installed_generation),
                operation: NativeObservationOperation::Poll
            }))
            .is_ok());
        assert!(q
            .try_push(RenderWork::NativeOwnerBindings(NativeOwnerBindingWork {
                generation: p.generation,
                renderer_id: p.renderer.as_ref().unwrap().materialization_id,
                renderer: r.clone(),
                claim: p.owner_installation.current_claim().unwrap().unwrap(),
                verifier: r.installed.verifier(),
                cancellation: ExecutionCancellationToken::new()
            }))
            .is_ok());
        {
            let state = q.state.lock();
            assert_eq!(
                state.pending.len() + state.author_batch.outside_pending(),
                MAX_PENDING_NATIVE_WORK
            );
        }
        let rejected = q.try_push(RenderWork::NativeObservations(NativeObservationWork {
            generation: p.generation,
            renderer_id: p.renderer.as_ref().unwrap().materialization_id,
            renderer: r.clone(),
            cancellation: ExecutionCancellationToken::new(),
            installation_generation: Some(t.installed_generation),
            operation: NativeObservationOperation::Poll,
        }));
        assert!(matches!(rejected, Err(RenderQueuePushError::Busy(_))));
        let mut pending = q.stop();
        pending.push_back(work);
        p.handoff_retired_render_owners(pending, None);
    });
}

#[test]
fn terminal_batch_lost_completion_and_stop_during_prepare_retire_original_owners() {
    for stop in [false, true] {
        with_case(false, |p, r, t, c| {
            c.prepare_gate.store(true, Ordering::Release);
            assert!(p.request_native_author_handoff(request(r, t, c)).unwrap());
            spin_until(|| c.prepare_entered.load(Ordering::Acquire));
            if stop {
                p.begin_shutdown();
            } else {
                let (_, receiver) = mpsc::channel();
                let old = std::mem::replace(&mut p.completion_rx, receiver);
                drop(old);
            }
            c.prepare_gate.store(false, Ordering::Release);
            spin_until(|| !c.request_drops.lock().is_empty());
            assert_eq!(c.prepared_drops.lock().len(), 1);
            assert_eq!(c.installs.load(Ordering::Acquire), 0);
            assert!(p.render_queue.batch_stopped());
        });
    }
}

#[test]
fn terminal_batch_old_completion_and_resume_keys_cannot_take_a_later_parked_owner() {
    with_case(false, |p, r, t, c| {
        assert!(p.request_native_author_handoff(request(r, t, c)).unwrap());
        let completion = match p.completion_rx.recv_timeout(Duration::from_secs(2)).unwrap() {
            RenderCompletion::NativeAuthorTerminalBatch(c) => c,
            other => {
                retire_render_completion(
                    other,
                    &p.render_retirement_queue,
                    &p.foreign_owner_retirements,
                );
                panic!("actual terminal completion")
            }
        };
        let late = completion.clone();
        let mut events = Vec::new();
        p.accept_terminal_author_completion(completion, &mut events);
        let k = key(&events[0]);
        assert!(p.resume_native_author_handoff(&k).unwrap());
        let retry = event(p);
        let next = key(&retry);
        assert_ne!(next, k);
        let address = box_address(p);
        late.retire_parked();
        assert_eq!(box_address(p), address);
        assert!(c.request_drops.lock().is_empty());
        assert!(p.resume_native_author_handoff(&k).is_err());
    });
}

#[test]
fn terminal_batch_suspend_and_post_cas_cancel_preserve_prepared_without_republishing() {
    with_case(false, |p, r, t, c| {
        assert!(p.request_native_author_handoff(request(r, t, c)).unwrap());
        let k = key(&event(p));
        let address = box_address(p);
        let before = p.owner_installation.current_claim().unwrap().unwrap().generation();
        p.suspend(sample_position(0)).unwrap();
        assert_eq!(box_address(p), address);
        assert_eq!(
            p.owner_installation.current_claim().unwrap().unwrap().generation(),
            before
        );
        c.published.store(true, Ordering::Release);
        c.fail_second_ack.store(true, Ordering::Release);
        assert!(p.resume_native_author_handoff(&k).unwrap());
        let partial = event(p);
        let k = key(&partial);
        p.native_control_cancellation.cancel();
        assert!(p.resume_native_author_handoff(&k).unwrap());
        assert!(matches!(
            event(p),
            AudioPlaybackEvent::NativeAuthorTerminalBatchObserved {
                status: AudioPcmNativeAuthorTerminalBatchStatus::InstalledAwaitingAcknowledgement { .. },
                ..
            }
        ));
        assert_eq!(c.prepares.load(Ordering::Acquire), 1);
        assert_eq!(c.installs.load(Ordering::Acquire), 1);
        assert_eq!(c.acks[0].load(Ordering::Acquire), 1);
        assert_eq!(c.acks[1].load(Ordering::Acquire), 0);
        assert!(c.prepared_drops.lock().is_empty());
    });
}

// Forward the original software output unchanged, except for one bounded
// deactivate barrier. No callback acknowledgement or claim is manufactured.
struct InstallDuringDeactivate {
    inner: Box<dyn AudioOutputAdapter>,
    queue: Arc<RenderWorkQueue>,
    source: Arc<mondrian_core::AudioOwnerInstallationSource>,
    old: std::num::NonZeroU64,
    control: Arc<Control>,
    armed: AtomicBool,
}
impl AudioOutputAdapter for InstallDuringDeactivate {
    fn shutdown_signal(&self) -> Option<Arc<AtomicBool>> {
        self.inner.shutdown_signal()
    }
    fn poll(&mut self) -> Option<RealtimeAudioOutputEvent> {
        self.inner.poll()
    }
    fn enqueue(&mut self, b: &AudioBuffer) -> Result<(), RealtimeAudioOutputEnqueueError> {
        self.inner.enqueue(b)
    }
    fn clear(&mut self) {
        self.inner.clear();
    }
    fn validate_deactivation(&self) -> Result<(), RealtimeAudioOutputControlError> {
        self.inner.validate_deactivation()
    }
    fn deactivate(
        &self,
    ) -> Result<Option<RealtimeAudioOutputQuiescenceToken>, RealtimeAudioOutputControlError> {
        if self.armed.swap(false, Ordering::AcqRel) {
            // reprime has already captured the old claim before this call.
            assert_eq!(
                self.source.current_claim().unwrap().unwrap().generation(),
                self.old
            );
            assert!(self.control.install_entered.load(Ordering::Acquire));
            self.control.install_gate.store(false, Ordering::Release);
            spin_until(|| {
                let state = self.queue.state.lock();
                matches!(&state.author_batch, Lane::Parked(w)
                    if w.expected.generation() != self.old)
            });
            assert_eq!(self.control.installs.load(Ordering::Acquire), 1);
            assert_eq!(self.control.acks[0].load(Ordering::Acquire), 1);
            assert_eq!(self.control.acks[1].load(Ordering::Acquire), 0);
        }
        self.inner.deactivate()
    }
    fn is_quiescent(
        &self,
        t: RealtimeAudioOutputQuiescenceToken,
    ) -> Result<bool, RealtimeAudioOutputControlError> {
        self.inner.is_quiescent(t)
    }
    fn activate_after_discard(
        &self,
        t: RealtimeAudioOutputQuiescenceToken,
        frames: usize,
    ) -> Result<(), RealtimeAudioOutputControlError> {
        self.inner.activate_after_discard(t, frames)
    }
    fn buffered_frames(&self) -> usize {
        self.inner.buffered_frames()
    }
    fn capacity_frames(&self) -> Option<usize> {
        self.inner.capacity_frames()
    }
    fn snapshot(&self) -> Option<RealtimeAudioOutputSnapshot> {
        self.inner.snapshot()
    }
    fn shutdown_and_wait(&mut self) -> RealtimeAudioOutputShutdownEvidence {
        self.inner.shutdown_and_wait()
    }
    fn set_device_selection(&self, selection: RealtimeAudioOutputDeviceSelection) -> bool {
        self.inner.set_device_selection(selection)
    }
    fn device_selection(&self) -> RealtimeAudioOutputDeviceSelection {
        self.inner.device_selection()
    }
    #[cfg(feature = "validation")]
    fn request_controlled_recycle(
        &self,
        expected: u64,
    ) -> Result<(), AudioPlaybackValidationError> {
        self.inner.request_controlled_recycle(expected)
    }
}

#[test]
fn terminal_batch_suspend_across_own_cas_retains_same_prepared_and_remaining_ack() {
    with_case(false, |p, r, t, c| {
        assert!(p.request_native_author_handoff(request(r, t, c)).unwrap());
        let k = key(&event(p));
        let address = box_address(p);
        let old = p.owner_installation.current_claim().unwrap().unwrap().generation();
        c.published.store(true, Ordering::Release);
        c.fail_second_ack.store(true, Ordering::Release);
        c.install_gate.store(true, Ordering::Release);
        assert!(p.resume_native_author_handoff(&k).unwrap());
        spin_until(|| c.install_entered.load(Ordering::Acquire));
        let inner = p.output.take().unwrap();
        p.output = OwnedAudioOutput::new(Box::new(InstallDuringDeactivate {
            inner,
            queue: p.render_queue.clone(),
            source: p.owner_installation.clone(),
            old,
            control: c.clone(),
            armed: AtomicBool::new(true),
        }));
        p.suspend(sample_position(0)).unwrap();
        assert_eq!(
            box_address(p),
            address,
            "same prepared owner survives Suspend"
        );
        let installed = p.owner_installation.current_claim().unwrap().unwrap().generation();
        assert_ne!(installed, old);
        assert!(c.prepared_drops.lock().is_empty());
        assert!(c.request_drops.lock().is_empty());
        let partial = event(p);
        assert!(matches!(partial,
            AudioPlaybackEvent::NativeAuthorTerminalBatchObserved {
                status: AudioPcmNativeAuthorTerminalBatchStatus::InstalledAwaitingAcknowledgement { target }, ..
            } if target.installed_generation == installed));
        let next = key(&partial);
        c.fail_second_ack.store(false, Ordering::Release);
        assert!(p.resume_native_author_handoff(&next).unwrap());
        assert!(matches!(event(p),
            AudioPlaybackEvent::NativeAuthorTerminalBatchObserved {
                status: AudioPcmNativeAuthorTerminalBatchStatus::Acknowledged { target }, ..
            } if target.installed_generation == installed));
        assert_eq!(c.prepares.load(Ordering::Acquire), 1);
        assert_eq!(c.installs.load(Ordering::Acquire), 1);
        assert_eq!(c.acks[0].load(Ordering::Acquire), 1);
        assert_eq!(c.acks[1].load(Ordering::Acquire), 1);
        assert_eq!(c.prepared_drops.lock().len(), 1);
    });
}

#[test]
fn terminal_batch_prepared_and_retirement_panics_poison_before_any_ui_poll() {
    for producer in [false, true] {
        with_case(true, |p, r, t, c| {
            assert!(p.request_native_author_handoff(request(r, t, c)).unwrap());
            let k = key(&event(p));
            if producer {
                c.published.store(true, Ordering::Release);
                c.panic_prepared_drop.store(true, Ordering::Release);
                assert!(p.resume_native_author_handoff(&k).unwrap());
            } else {
                c.panic_request_drop.store(true, Ordering::Release);
                let pending = p.render_queue.stop();
                p.handoff_retired_render_owners(pending, None);
            }
            spin_until(|| matches!(p.render_queue.state.lock().author_batch, Lane::Poisoned(_)));
            assert!(p.render_queue.batch_stopped());
            assert!(matches!(
                p.resume_native_author_handoff(&k),
                Err(AudioPlaybackError::ExecutionUnavailable)
            ));
            // Neither an ordinary Drop nor an old key can release poisoned credit.
            assert!(p.render_queue.batch_busy());
        });
    }
}

#[test]
fn terminal_batch_unadmitted_request_counts_and_retires_on_the_original_worker() {
    with_case(false, |p, r, t, c| {
        let (mut empty, _) = playback_with_buffered_frames(0);
        let retired = empty.render_retirement_worker.as_ref().unwrap().thread().id();
        assert!(!empty.request_native_author_handoff(request(r, t, c)).unwrap());
        spin_until(|| !c.request_drops.lock().is_empty());
        let evidence = empty.shutdown_and_wait();
        assert!(evidence.all_workers_terminated(), "{evidence:?}");
        assert_eq!(c.request_drops.lock().as_slice(), &[retired]);
        assert_eq!(c.prepares.load(Ordering::Acquire), 0);
        assert!(p.renderer.is_some());
    });
    with_case(false, |p, _r, t, c| {
        let wrong = Renderer::new(false);
        assert!(!p.request_native_author_handoff(request(&wrong, t, c)).unwrap());
        spin_until(|| !c.request_drops.lock().is_empty());
        assert_eq!(c.prepares.load(Ordering::Acquire), 0);
    });
    with_case(false, |p, r, t, c| {
        assert!(p.request_native_author_handoff(request(r, t, c)).unwrap());
        let _ = event(p);
        let address = box_address(p);
        let rejected = Arc::new(Control::default());
        assert!(!p.request_native_author_handoff(request(r, t, &rejected)).unwrap());
        spin_until(|| !rejected.request_drops.lock().is_empty());
        assert_eq!(rejected.prepares.load(Ordering::Acquire), 0);
        assert_eq!(address, box_address(p));
        assert!(c.request_drops.lock().is_empty());
    });
    with_case(false, |p, r, t, c| {
        let retirement = p.render_retirement_worker.as_ref().unwrap().thread().id();
        let carrier = RetiredRenderOwners {
            native_author_requests: VecDeque::from([request(r, t, c)]),
            ..Default::default()
        };
        assert_eq!(carrier.owner_units(), 1);
        assert!(!carrier.is_empty());
        enqueue_retired_render_owners(
            &p.render_retirement_queue,
            &p.foreign_owner_retirements,
            carrier,
        );
        spin_until(|| !c.request_drops.lock().is_empty());
        assert_eq!(c.request_drops.lock().as_slice(), &[retirement]);
        assert_eq!(c.prepares.load(Ordering::Acquire), 0);
    });
    with_case(false, |p, r, t, c| {
        p.begin_shutdown();
        assert!(matches!(
            p.request_native_author_handoff(request(r, t, c)),
            Err(AudioPlaybackError::ExecutionUnavailable)
        ));
        spin_until(|| !c.request_drops.lock().is_empty());
        assert_eq!(c.request_drops.lock().len(), 1);
        assert_eq!(c.prepares.load(Ordering::Acquire), 0);
    });
}

#[test]
fn terminal_batch_true_source_replacement_retires_prepared_without_clearing_published_fact() {
    with_case(false, |p, r, t, c| {
        let retirement = p.render_retirement_worker.as_ref().unwrap().thread().id();
        assert!(p.request_native_author_handoff(request(r, t, c)).unwrap());
        let k = key(&event(p));
        c.published.store(true, Ordering::Release);
        let old = p.owner_installation.current_claim().unwrap().unwrap();
        let (registration, _) = p.owner_installation.register().unwrap();
        let replacement = p.owner_installation.reserve_install(&registration, Some(&old)).unwrap();
        let new = p.owner_installation.begin_commit(replacement).unwrap().commit().unwrap();
        assert!(matches!(
            p.resume_native_author_handoff(&k),
            Err(AudioPlaybackError::OwnerInstallation(
                AudioOwnerInstallationError::Superseded
            ))
        ));
        spin_until(|| !c.request_drops.lock().is_empty());
        spin_until(|| !p.render_queue.batch_busy());
        assert_eq!(c.request_drops.lock().as_slice(), &[retirement]);
        assert_eq!(c.prepared_drops.lock().as_slice(), &[retirement]);
        assert_eq!(c.installs.load(Ordering::Acquire), 0);
        assert!(
            c.published.load(Ordering::Acquire),
            "transport failure cannot roll back caller publication"
        );
        assert_eq!(
            p.owner_installation.current_claim().unwrap().unwrap().generation(),
            new.generation()
        );
        assert!(
            matches!(
                p.native_control_hold,
                Some(NativeControlHold::AuthorPending { .. })
            ),
            "retirement does not acknowledge an author group"
        );
    });
}
