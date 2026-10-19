//! One retained restart on Playback's existing producer and retirement queues.
use super::*;
use std::num::NonZeroU64;
use std::sync::Weak;

/// Same installed renderer; this provider never owns a second Runtime.
pub trait AudioPcmNativeReprepareHandoff: Send + 'static {
    fn renderer(&self) -> &Weak<dyn AudioPcmRenderer>;
    /// Called only on the original serial producer, with actual Source authority.
    fn install(
        &mut self,
        expected_notice: mondrian_core::NativeAudioReprepareNotice,
        destination: AudioOwnerInstallationVerifier,
        authorization: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<AudioOwnerInstallationClaim, AudioPcmNativeReprepareRefusal>;
    /// Called only on the original retirement thread. Do not upgrade a renderer,
    /// acquire Runtime/Source locks, normalize credit, or enter DSP here.
    fn retire(&mut self) -> Result<(), AudioPcmNativeReprepareRefusal>;
    /// Consume the actual retirement completion on the original producer.
    /// Old PCM cancellation does not cancel this resource-only obligation.
    fn settle(&mut self) -> Result<(), AudioPcmNativeReprepareRefusal>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPcmNativeReprepareRefusal {
    UnsupportedAdapter,
    Superseded,
    Cancelled,
    ResourceAdmission,
    ProcessorRejected,
    WorkerUnavailable,
    RetirementUnconfirmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPcmNativeReprepareStatus {
    Settled { installed_generation: NonZeroU64 },
    Refused(AudioPcmNativeReprepareRefusal),
}

// Only admission/result facts live in the UI hold. Queue execution steps and
// actual opaque owners live in the ONE work/provider below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NativeReprepareAdmission {
    Unstarted,
    InFlight { generation: u64 },
    Settled { installed_generation: NonZeroU64 },
    Refused,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Unadmitted,
    Install,
    Retire,
    Settle,
    Terminal,
}

struct Admission {
    generation: u64,
    renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
    renderer: Arc<dyn AudioPcmRenderer>,
    source: Arc<AudioOwnerInstallationSource>,
    expected: AudioOwnerInstallationClaim,
    notice: mondrian_core::NativeAudioReprepareNotice,
    cancellation: ExecutionCancellationToken,
    return_queue: Weak<RenderWorkQueue>,
    installed: Option<AudioOwnerInstallationClaim>,
}

pub(super) struct NativeReprepareWork {
    request: Box<dyn AudioPcmNativeReprepareHandoff>,
    // None is the real Unadmitted shape: no fabricated notice/claim/Source.
    admission: Option<Admission>,
    step: Step,
}

pub(super) struct NativeReprepareCompletion {
    generation: u64,
    renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
    renderer: Weak<dyn AudioPcmRenderer>,
    result: Result<AudioOwnerInstallationClaim, AudioPcmNativeReprepareRefusal>,
}

pub(super) enum Execution {
    Retire,
    Finished(Result<AudioOwnerInstallationClaim, AudioPcmNativeReprepareRefusal>),
}

impl NativeReprepareWork {
    pub(super) fn preserve_for_slot(
        &self,
        renderer_id: Option<mondrian_core::AudioPcmRendererMaterializationId>,
        installed: Option<std::num::NonZeroU64>,
    ) -> bool {
        // Resource settlement survives old PCM/claim invalidation. The original
        // Runtime will consume or refuse its own exact completion, without DSP.
        self.step == Step::Settle
            || self.admission.as_ref().is_some_and(|a| {
                Some(a.renderer_id) == renderer_id
                    && Some(a.installed.as_ref().unwrap_or(&a.expected).generation()) == installed
            })
    }
    pub(super) fn terminal(&mut self) {
        self.step = Step::Terminal;
    }
}

impl AudioPlayback {
    /// Existence of the retained restart owner, including execution failure.
    /// This fact grants no output proof, service readiness or native permission.
    pub fn has_retained_native_reprepare(&self) -> bool {
        matches!(
            self.native_control_hold,
            Some(NativeControlHold::Reprepare { .. })
        )
    }

    fn current_reprepare_output(&self, proof: NativeReprepareOutputRetired) -> bool {
        proof.generation == self.generation
            && self
                .renderer
                .as_ref()
                .is_some_and(|r| r.materialization_id == proof.renderer_id)
            && proof.token == self.quiescence_token
            && proof.token.is_none_or(|token| {
                self.output_generation_ready
                    && proof.stream_generation == Some(token.stream_generation)
                    && matches!(self.output.is_quiescent(token), Ok(true))
            })
            && !self.pcm_journal.old_work_unsettled()
            && self.in_flight == 0
            && self.native_control_output_retirement.is_none()
    }

    /// Consume one privately finalized output proof; no native work runs here.
    /// Even rejected opaque providers retire on the original retirement queue.
    pub fn request_native_reprepare(
        &mut self,
        pending_generation: u64,
        request: Box<dyn AudioPcmNativeReprepareHandoff>,
    ) -> Result<bool, AudioPlaybackError> {
        let mut work =
            Box::new(NativeReprepareWork { request, admission: None, step: Step::Unadmitted });
        let admitted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.ensure_source_close_idle()?;
            self.ensure_render_execution_available()?;
            let Some(NativeControlHold::Reprepare {
                notice,
                phase: NativeRepreparePhase::OutputRetired(proof),
                admission: NativeReprepareAdmission::Unstarted,
            }) = self.native_control_hold
            else {
                return Ok(None);
            };
            if pending_generation != self.generation || !self.current_reprepare_output(proof) {
                return Ok(None);
            }
            let renderer = self.renderer.as_ref().expect("actual output proof renderer");
            let expected = self.owner_installation.current_claim()?;
            let Some(expected) = expected else {
                return Ok(None);
            };
            if Some(expected.generation()) != proof.installed_generation
                || !Weak::ptr_eq(&Arc::downgrade(&renderer.owner), work.request.renderer())
                || self.native_control_cancellation.is_canceled()
            {
                return Ok(None);
            }
            Ok(Some(Admission {
                generation: self.generation,
                renderer_id: renderer.materialization_id,
                renderer: Arc::clone(&renderer.owner),
                source: Arc::clone(&self.owner_installation),
                expected,
                notice,
                cancellation: self.native_control_cancellation.clone(),
                return_queue: Arc::downgrade(&self.render_queue),
                installed: None,
            }))
        }));
        let admitted = match admitted {
            Ok(result) => result,
            Err(payload) => {
                dispose_canonical_or_abandon_opaque_panic_payload(payload);
                self.handoff_retired_render_owners(
                    VecDeque::from([RenderWork::NativeReprepare(work)]),
                    None,
                );
                return Err(AudioPlaybackError::OwnerRegistrationPanicked);
            }
        };
        let admission = match admitted {
            Ok(Some(admission)) => admission,
            result => {
                self.handoff_retired_render_owners(
                    VecDeque::from([RenderWork::NativeReprepare(work)]),
                    None,
                );
                return result.map(|_| false);
            }
        };
        let generation = admission.generation;
        work.admission = Some(admission);
        work.step = Step::Install;
        if let Some(NativeControlHold::Reprepare { admission, .. }) = &mut self.native_control_hold
        {
            *admission = NativeReprepareAdmission::InFlight { generation };
        }
        match self.render_queue.push(RenderWork::NativeReprepare(work)) {
            Ok(()) => {
                self.native_owner_binding_status = None;
                Ok(true)
            }
            Err(mut work) => {
                if let RenderWork::NativeReprepare(work) = &mut work {
                    work.terminal();
                }
                self.handoff_retired_render_owners(VecDeque::from([work]), None);
                if let Some(NativeControlHold::Reprepare { admission, .. }) =
                    &mut self.native_control_hold
                {
                    *admission = NativeReprepareAdmission::Unstarted;
                }
                Ok(false)
            }
        }
    }

    pub(super) fn accept_native_reprepare_completion(
        &mut self,
        completion: NativeReprepareCompletion,
        events: &mut Vec<AudioPlaybackEvent>,
    ) {
        if !matches!(self.native_control_hold, Some(NativeControlHold::Reprepare {
            admission: NativeReprepareAdmission::InFlight { generation }, ..
        }) if generation == completion.generation)
            || !self.renderer.as_ref().is_some_and(|r| {
                r.materialization_id == completion.renderer_id
                    && Weak::ptr_eq(&Arc::downgrade(&r.owner), &completion.renderer)
            })
        {
            return;
        }
        let status = match completion.result {
            Ok(claim)
                if self.owner_installation.current_claim().is_ok_and(|current| {
                    current.is_some_and(|current| current.generation() == claim.generation())
                }) =>
            {
                AudioPcmNativeReprepareStatus::Settled { installed_generation: claim.generation() }
            }
            Ok(_) => {
                AudioPcmNativeReprepareStatus::Refused(AudioPcmNativeReprepareRefusal::Superseded)
            }
            Err(reason) => AudioPcmNativeReprepareStatus::Refused(reason),
        };
        if let Some(NativeControlHold::Reprepare { admission, .. }) = &mut self.native_control_hold
        {
            *admission = match status {
                AudioPcmNativeReprepareStatus::Settled { installed_generation } => {
                    NativeReprepareAdmission::Settled { installed_generation }
                }
                AudioPcmNativeReprepareStatus::Refused(_) => NativeReprepareAdmission::Refused,
            };
        }
        // This only reports an exact resource result. HOLD and output admission remain.
        events.push(AudioPlaybackEvent::NativeReprepareObserved {
            renderer_id: completion.renderer_id,
            generation: self.generation,
            status,
        });
    }

    /// An explicit fresh generation after BOTH actual resource settlement and
    /// the current physical-output/PCM proof. This never selects Continue.
    pub fn finish_native_reprepare(
        &mut self,
        pending_generation: u64,
        anchor: AudioSamplePosition,
    ) -> Result<bool, AudioPlaybackError> {
        let Some(NativeControlHold::Reprepare {
            phase: NativeRepreparePhase::OutputRetired(proof),
            admission: NativeReprepareAdmission::Settled { installed_generation },
            ..
        }) = self.native_control_hold
        else {
            return Ok(false);
        };
        if pending_generation != self.generation
            || !self.current_reprepare_output(proof)
            || !self
                .owner_installation
                .current_claim()?
                .is_some_and(|claim| claim.generation() == installed_generation)
        {
            return Ok(false);
        }
        // proof.installed_generation is historical. The private successful work
        // chain and actual current claim prove old->new; do not rewrite history.
        self.validate_reprime_anchor_pure(anchor)?;
        self.ensure_render_execution_available()?;
        self.validate_reprime_output()?;
        self.reprime_prevalidated(
            anchor,
            false,
            self.renderer.clone(),
            PcmRetirementPolicy::FreshContinuity,
        )?;
        self.native_control_hold = None;
        self.render_blocked = false;
        Ok(true)
    }
}

pub(super) fn execute(work: &mut NativeReprepareWork) -> Execution {
    use AudioPcmNativeReprepareRefusal as R;
    let Some(a) = work.admission.as_mut() else {
        return Execution::Finished(Err(R::Superseded));
    };
    match work.step {
        Step::Install => {
            if a.cancellation.is_canceled() {
                return Execution::Finished(Err(R::Cancelled));
            }
            let Some(actual) = a.renderer.owner_installation_verifier() else {
                return Execution::Finished(Err(R::UnsupportedAdapter));
            };
            if actual.validate(&a.expected).is_err()
                || !a.source.owns_registration(&actual.registration())
            {
                return Execution::Finished(Err(R::Superseded));
            }
            let mut authority = match a
                .source
                .reserve_install(&actual.registration(), Some(&a.expected))
                .and_then(|reservation| a.source.authorize_runtime_installation(reservation))
            {
                Ok(value) => value,
                Err(_) => return Execution::Finished(Err(R::Superseded)),
            };
            match work.request.install(a.notice, actual.clone(), &mut authority, &a.cancellation) {
                Ok(claim)
                    if claim.generation() != a.expected.generation()
                        && actual.validate(&claim).is_ok() =>
                {
                    a.installed = Some(claim);
                    Execution::Retire
                }
                Ok(_) => Execution::Finished(Err(R::Superseded)),
                Err(reason) => Execution::Finished(Err(reason)),
            }
        }
        Step::Settle => {
            // No cancellation/old PCM generation gate: this is resource custody,
            // not another capture, installation, callback or DSP permission.
            let result =
                work.request.settle().and_then(|()| a.installed.clone().ok_or(R::Superseded));
            Execution::Finished(result)
        }
        _ => Execution::Finished(Err(R::Superseded)),
    }
}

pub(super) fn complete(
    mut work: Box<NativeReprepareWork>,
    result: Execution,
    queue: &RenderWorkQueue,
) -> Option<NativeReprepareCompletion> {
    let completion = match result {
        Execution::Retire => {
            work.step = Step::Retire;
            None
        }
        Execution::Finished(result) => {
            work.step = Step::Terminal;
            work.admission.as_ref().map(|a| NativeReprepareCompletion {
                generation: a.generation,
                renderer_id: a.renderer_id,
                renderer: Arc::downgrade(&a.renderer),
                result,
            })
        }
    };
    enqueue_retired_render_owners(
        &queue.retirement_queue,
        &queue.retirement_tracker,
        RetiredRenderOwners {
            pending: VecDeque::from([RenderWork::NativeReprepare(work)]),
            ..Default::default()
        },
    );
    completion
}

pub(super) fn retire_work(
    mut work: Box<NativeReprepareWork>,
    retirement: &RenderOwnerRetirementQueue,
    tracker: &ForeignOwnerRetirementTracker,
) {
    if work.step != Step::Retire {
        return;
    } // Terminal/Unadmitted drop only here.
    if work.request.retire().is_err() {
        retirement.mark_faulted();
        mondrian_core::NativeChildSettlementReporter::unavailable(tracker);
        return;
    }
    work.step = Step::Settle;
    let Some(queue) = work.admission.as_ref().and_then(|a| a.return_queue.upgrade()) else {
        // Queue expiry is not proof that an externally aliased Runtime died.
        // Drop only this carrier here; its actual scope/fence remains truthful.
        return;
    };
    match queue.try_push(RenderWork::NativeReprepare(work)) {
        Ok(()) => {}
        Err(RenderQueuePushError::Closed(_work)) => {
            // Normal close: original retirement thread releases this carrier.
            // No normalization/Ready claim, and no synthetic retirement fault.
        }
        Err(RenderQueuePushError::Busy(_work)) => {
            retirement.mark_faulted();
            mondrian_core::NativeChildSettlementReporter::unavailable(tracker);
        }
    }
}

#[cfg(test)]
mod tests {
    // Portable route/ownership controls only. This renderer/provider has no DLL,
    // seed codec, native DSP state, device or full-heap qualification.
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Facts {
        calls: Mutex<Vec<(&'static str, thread::ThreadId)>>,
        install: AtomicUsize,
        settle: AtomicUsize,
        drop_count: AtomicUsize,
        entered_retirement: AtomicBool,
        release_retirement: AtomicBool,
        high: AtomicUsize,
    }
    impl Facts {
        fn record(&self, name: &'static str) {
            self.calls.lock().push((name, thread::current().id()));
        }
    }
    struct Renderer {
        verifier: Mutex<Option<AudioOwnerInstallationVerifier>>,
        scope: Mutex<Option<mondrian_core::NativeChildSettlementRegistrar>>,
        notice: mondrian_core::NativeAudioReprepareNotice,
        restarted: AtomicBool,
        requests: Mutex<Vec<AudioPcmRenderRequest>>,
    }
    impl AudioPcmRenderer for Renderer {
        fn bind_native_child_settlement(
            &self,
            root: &mondrian_core::NativeChildSettlementRegistrar,
        ) -> Result<(), mondrian_core::NativeChildSettlementError> {
            let mut scope = self.scope.lock();
            if scope.is_none() {
                *scope = Some(root.try_fork()?);
            }
            Ok(())
        }
        fn owner_installation_verifier(&self) -> Option<AudioOwnerInstallationVerifier> {
            self.verifier.lock().clone()
        }
        fn bind_owner_installation(
            &self,
            actual: AudioOwnerInstallationVerifier,
        ) -> Result<bool, AudioOwnerInstallationError> {
            *self.verifier.lock() = Some(actual);
            Ok(true)
        }
        fn install_native_owner_bindings(
            &self,
            _: &AudioOwnerInstallationClaim,
            _: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioPcmNativeOwnerBindingStatus> {
            Ok(AudioPcmNativeOwnerBindingStatus::Ready)
        }
        fn render(
            &self,
            request: AudioPcmRenderRequest,
            _: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
            self.requests.lock().push(request);
            Ok(if self.restarted.load(Ordering::Acquire) {
                AudioPcmRenderOutcome::Rendered(AudioBuffer::silent(
                    request.sample_rate,
                    request.channel_layout,
                    request.frame_count,
                ))
            } else {
                AudioPcmRenderOutcome::DeferredForReprepare(self.notice)
            })
        }
    }
    struct Provider {
        renderer: Weak<Renderer>,
        erased: Weak<dyn AudioPcmRenderer>,
        facts: Arc<Facts>,
        fail: u8,
    }
    impl Drop for Provider {
        fn drop(&mut self) {
            self.facts.record("drop");
            self.facts.drop_count.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl AudioPcmNativeReprepareHandoff for Provider {
        fn renderer(&self) -> &Weak<dyn AudioPcmRenderer> {
            &self.erased
        }
        fn install(
            &mut self,
            notice: mondrian_core::NativeAudioReprepareNotice,
            destination: AudioOwnerInstallationVerifier,
            authority: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
            _: &ExecutionCancellationToken,
        ) -> Result<AudioOwnerInstallationClaim, AudioPcmNativeReprepareRefusal> {
            self.facts.record("install");
            self.facts.install.fetch_add(1, Ordering::SeqCst);
            let renderer = self.renderer.upgrade().unwrap();
            assert_eq!(notice, renderer.notice);
            if self.fail == 1 {
                return Err(AudioPcmNativeReprepareRefusal::ProcessorRejected);
            }
            if self.fail == 4 {
                // An old valid claim is not evidence of consuming replacement
                // authority or actually installing a new Session.
                return Ok(destination.current_claim().unwrap());
            }
            let claim = authority.begin_commit(&destination).unwrap().commit();
            renderer.restarted.store(true, Ordering::Release);
            self.facts.high.store(1, Ordering::SeqCst);
            Ok(claim)
        }
        fn retire(&mut self) -> Result<(), AudioPcmNativeReprepareRefusal> {
            self.facts.record("retire");
            self.facts.entered_retirement.store(true, Ordering::Release);
            if self.fail == 2 {
                panic!("portable retirement panic control");
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while !self.facts.release_retirement.load(Ordering::Acquire) {
                assert!(Instant::now() < deadline, "test must release retirement");
                thread::yield_now();
            }
            Ok(())
        }
        fn settle(&mut self) -> Result<(), AudioPcmNativeReprepareRefusal> {
            self.facts.record("settle");
            self.facts.settle.fetch_add(1, Ordering::SeqCst);
            if self.fail == 3 {
                return Err(AudioPcmNativeReprepareRefusal::RetirementUnconfirmed);
            }
            self.facts.high.store(0, Ordering::SeqCst);
            Ok(())
        }
    }
    struct Release(Arc<Facts>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.release_retirement.store(true, Ordering::Release);
        }
    }
    fn playback_config() -> AudioPlaybackConfig {
        // The reused parent FakeOutput advertises an exact 1 kHz contract.
        AudioPlaybackConfig {
            sample_rate: 1_000,
            chunk_frames: 4,
            preroll_frames: 4,
            high_watermark_frames: 12,
            max_in_flight: 2,
            ..AudioPlaybackConfig::product_default()
        }
    }
    fn anchor(n: i64) -> AudioSamplePosition {
        AudioSamplePosition::new(n, mondrian_core::AudioSampleRate::new(1_000).unwrap())
    }
    fn renderer() -> Arc<Renderer> {
        Arc::new(Renderer {
            verifier: Mutex::new(None),
            scope: Mutex::new(None),
            notice: mondrian_core::NativeAudioReprepareNotice {
                instance_id: mondrian_core::AudioProcessorInstanceId::new(),
                request: mondrian_core::NativeAudioReprepareRequest::ClapRestart,
            },
            restarted: AtomicBool::new(false),
            requests: Mutex::new(Vec::new()),
        })
    }
    fn request(
        renderer: &Arc<Renderer>,
        facts: &Arc<Facts>,
        fail: u8,
    ) -> Box<dyn AudioPcmNativeReprepareHandoff> {
        let erased: Arc<dyn AudioPcmRenderer> = renderer.clone();
        Box::new(Provider {
            renderer: Arc::downgrade(renderer),
            erased: Arc::downgrade(&erased),
            facts: facts.clone(),
            fail,
        })
    }
    fn pump(playback: &mut AudioPlayback, mut done: impl FnMut(&AudioPlayback) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !done(playback) {
            assert!(Instant::now() < deadline, "bounded portable queue control");
            playback.poll(AudioPlaybackMode::Preroll, anchor(0)).unwrap();
            thread::yield_now();
        }
    }
    fn held() -> (AudioPlayback, Arc<Renderer>) {
        let (output, _) = super::super::tests::fake_output();
        let mut playback = AudioPlayback::with_output(playback_config(), output).unwrap();
        let renderer = renderer();
        playback
            .prepare(
                anchor(0),
                renderer.clone(),
                AudioPcmContinuityModel::GenerationState,
            )
            .unwrap();
        pump(&mut playback, |p| {
            p.native_control_output_retirement.is_some_and(|r| r.acknowledged)
        });
        let generation = playback.generation;
        assert!(playback.finalize_native_control_anchor(generation, anchor(0)).unwrap());
        assert!(playback.owner_installation.current_claim().unwrap().is_some());
        (playback, renderer)
    }
    fn settled(playback: &AudioPlayback) -> bool {
        matches!(
            playback.native_control_hold,
            Some(NativeControlHold::Reprepare {
                admission: NativeReprepareAdmission::Settled { .. },
                ..
            })
        )
    }
    fn refused(playback: &AudioPlayback) -> bool {
        matches!(
            playback.native_control_hold,
            Some(NativeControlHold::Reprepare { admission: NativeReprepareAdmission::Refused, .. })
        )
    }

    #[test]
    fn rejected_restart_providers_retire_without_installing_or_caller_drop() {
        let (output, _) = super::super::tests::fake_output();
        let mut empty = AudioPlayback::with_output(playback_config(), output).unwrap();
        let uninstalled = renderer();
        let facts = Arc::new(Facts::default());
        assert!(!empty
            .request_native_reprepare(empty.generation, request(&uninstalled, &facts, 0))
            .unwrap());
        pump(&mut empty, |_| facts.drop_count.load(Ordering::SeqCst) == 1);
        assert_eq!(facts.install.load(Ordering::SeqCst), 0);
        assert!(facts.calls.lock().iter().all(|(_, id)| *id != thread::current().id()));
        let (mut playback, actual) = held();
        let caller = thread::current().id();
        for (target, generation) in [
            (renderer(), playback.generation),
            (actual.clone(), playback.generation - 1),
        ] {
            let facts = Arc::new(Facts::default());
            assert!(!playback
                .request_native_reprepare(generation, request(&target, &facts, 0))
                .unwrap());
            pump(&mut playback, |_| {
                facts.drop_count.load(Ordering::SeqCst) == 1
            });
            assert_eq!(facts.install.load(Ordering::SeqCst), 0);
            assert!(facts.calls.lock().iter().all(|(_, id)| *id != caller));
        }
        // A stale Source claim is also a real preflight refusal, with the same
        // opaque provider retirement obligation as an absent Source.
        playback.owner_installation.revoke().unwrap();
        let facts = Arc::new(Facts::default());
        assert!(!playback
            .request_native_reprepare(playback.generation, request(&actual, &facts, 0))
            .unwrap());
        pump(&mut playback, |_| {
            facts.drop_count.load(Ordering::SeqCst) == 1
        });
        assert_eq!(facts.install.load(Ordering::SeqCst), 0);
        playback.invalidate_native_reprepare_output_retirement();
        let facts = Arc::new(Facts::default());
        assert!(!playback
            .request_native_reprepare(playback.generation, request(&actual, &facts, 0))
            .unwrap());
        pump(&mut playback, |_| {
            facts.drop_count.load(Ordering::SeqCst) == 1
        });
        assert_eq!(facts.install.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn restart_uses_original_producer_retirement_and_settlement_before_fresh_entry() {
        let (mut playback, renderer) = held();
        let facts = Arc::new(Facts::default());
        facts.release_retirement.store(true, Ordering::Release);
        let caller = thread::current().id();
        let generation = playback.generation;
        let old_requests = renderer.requests.lock().len();
        assert!(playback
            .request_native_reprepare(generation, request(&renderer, &facts, 0))
            .unwrap());
        assert_eq!(playback.native_owner_binding_status(), None);
        pump(&mut playback, settled);
        assert_eq!(facts.install.load(Ordering::SeqCst), 1);
        assert_eq!(facts.settle.load(Ordering::SeqCst), 1);
        assert_eq!(facts.high.load(Ordering::SeqCst), 0);
        assert_eq!(
            renderer.requests.lock().len(),
            old_requests,
            "settlement cannot render PCM"
        );
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Preroll).state,
            AudioPlaybackState::RepreparePending
        );
        let calls = facts.calls.lock();
        let install = calls.iter().find(|(name, _)| *name == "install").unwrap().1;
        let retire = calls.iter().find(|(name, _)| *name == "retire").unwrap().1;
        let settle = calls.iter().find(|(name, _)| *name == "settle").unwrap().1;
        assert_eq!(install, settle);
        assert_ne!(install, retire);
        assert_ne!(caller, install);
        assert_ne!(caller, retire);
        drop(calls);
        assert!(playback.finish_native_reprepare(generation, anchor(0)).unwrap());
        let fresh = playback.generation;
        assert!(fresh > generation);
        pump(&mut playback, |_| {
            renderer.requests.lock().len() > old_requests
        });
        assert_eq!(
            renderer.requests.lock()[old_requests].continuity,
            AudioPcmContinuity::Enter(AudioPcmRenderGeneration::new(fresh))
        );
    }

    #[test]
    fn restart_refusal_keeps_hold_and_does_not_retry_installation() {
        let (mut playback, renderer) = held();
        let facts = Arc::new(Facts::default());
        let old = playback.owner_installation.current_claim().unwrap().unwrap().generation();
        assert!(playback
            .request_native_reprepare(playback.generation, request(&renderer, &facts, 1))
            .unwrap());
        pump(&mut playback, refused);
        assert!(!playback
            .request_native_reprepare(playback.generation, request(&renderer, &facts, 1))
            .unwrap());
        assert_eq!(facts.install.load(Ordering::SeqCst), 1);
        assert_eq!(facts.settle.load(Ordering::SeqCst), 0);
        assert_eq!(
            playback.owner_installation.current_claim().unwrap().unwrap().generation(),
            old
        );
        assert!(!playback.finish_native_reprepare(playback.generation, anchor(0)).unwrap());
    }

    #[test]
    fn returning_the_old_valid_claim_cannot_fake_restart_installation() {
        let (mut playback, renderer) = held();
        let facts = Arc::new(Facts::default());
        let expected = playback.owner_installation.current_claim().unwrap().unwrap();
        assert!(playback
            .request_native_reprepare(playback.generation, request(&renderer, &facts, 4))
            .unwrap());
        pump(&mut playback, refused);
        assert_eq!(
            playback.owner_installation.current_claim().unwrap().unwrap().generation(),
            expected.generation()
        );
        assert_eq!(facts.install.load(Ordering::SeqCst), 1);
        assert_eq!(facts.settle.load(Ordering::SeqCst), 0);
        assert!(!facts.entered_retirement.load(Ordering::Acquire));
        assert!(matches!(
            playback.native_control_hold,
            Some(NativeControlHold::Reprepare { admission: NativeReprepareAdmission::Refused, .. })
        ));
        assert!(!playback.finish_native_reprepare(playback.generation, anchor(0)).unwrap());
        assert!(facts
            .calls
            .lock()
            .iter()
            .all(|(name, _)| *name != "retire" && *name != "settle"));
    }

    #[test]
    fn output_rotation_revokes_only_proof_and_cannot_cancel_resource_settlement() {
        let (mut playback, renderer) = held();
        let facts = Arc::new(Facts::default());
        let _release = Release(facts.clone());
        let original = playback.generation;
        assert!(playback
            .request_native_reprepare(original, request(&renderer, &facts, 0))
            .unwrap());
        pump(&mut playback, |_| {
            facts.entered_retirement.load(Ordering::Acquire)
        });
        let installed = playback.owner_installation.current_claim().unwrap().unwrap().generation();
        let old_cancel = playback.generation_cancellation.clone();
        playback.reprime(anchor(20)).unwrap();
        assert!(old_cancel.is_canceled());
        assert!(playback.generation > original);
        assert_eq!(
            playback.owner_installation.current_claim().unwrap().unwrap().generation(),
            installed
        );
        assert!(
            matches!(playback.native_control_hold, Some(NativeControlHold::Reprepare {
            admission: NativeReprepareAdmission::InFlight { generation }, ..
        }) if generation == original)
        );
        assert!(!playback.finish_native_reprepare(playback.generation, anchor(20)).unwrap());
        facts.release_retirement.store(true, Ordering::Release);
        pump(&mut playback, settled);
        assert_eq!(facts.settle.load(Ordering::SeqCst), 1);
        pump(&mut playback, |p| {
            p.native_control_output_retirement.is_some_and(|r| r.acknowledged)
        });
        assert!(playback
            .finalize_native_control_anchor(playback.generation, anchor(20))
            .unwrap());
        assert!(playback.finish_native_reprepare(playback.generation, anchor(20)).unwrap());
    }

    #[test]
    fn resume_before_restart_service_preserves_original_claim_and_native_control() {
        let (mut playback, renderer) = held();
        let facts = Arc::new(Facts::default());
        facts.release_retirement.store(true, Ordering::Release);
        let original = playback.owner_installation.current_claim().unwrap().unwrap().generation();
        let control = playback.native_control_cancellation.clone();
        playback.suspend(anchor(20)).unwrap();
        let paused = playback.generation;
        playback.reprime(anchor(20)).unwrap();
        assert!(playback.generation > paused);
        assert_eq!(
            playback.owner_installation.current_claim().unwrap().unwrap().generation(),
            original
        );
        assert!(
            !control.is_canceled(),
            "Unstarted restart still owns its native control frontier"
        );
        assert!(matches!(
            playback.native_control_hold,
            Some(NativeControlHold::Reprepare {
                admission: NativeReprepareAdmission::Unstarted,
                ..
            })
        ));
        assert!(!playback
            .request_native_reprepare(playback.generation, request(&renderer, &facts, 0))
            .unwrap());
        pump(&mut playback, |p| {
            p.native_control_output_retirement.is_some_and(|r| r.acknowledged)
        });
        assert!(playback
            .finalize_native_control_anchor(playback.generation, anchor(20))
            .unwrap());
        assert!(playback
            .request_native_reprepare(playback.generation, request(&renderer, &facts, 0))
            .unwrap());
        pump(&mut playback, settled);
        assert_eq!(facts.install.load(Ordering::SeqCst), 1);
        assert_eq!(facts.settle.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn paused_output_ack_is_consumed_once_and_resources_settle_before_resume_entry() {
        let (mut playback, renderer) = held();
        let facts = Arc::new(Facts::default());
        facts.release_retirement.store(true, Ordering::Release);
        let old_requests = renderer.requests.lock().len();
        let cancellation = playback.native_control_cancellation.clone();
        playback.suspend(anchor(20)).unwrap();
        let paused_generation = playback.generation;
        assert!(!cancellation.is_canceled());
        assert!(!playback
            .request_native_reprepare(paused_generation, request(&renderer, &facts, 0))
            .unwrap());
        let mut acknowledgements = 0;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !playback.native_control_output_retirement.is_some_and(|r| r.acknowledged) {
            assert!(Instant::now() < deadline);
            for event in playback.poll(AudioPlaybackMode::Idle, anchor(20)).unwrap().events {
                if matches!(event, AudioPlaybackEvent::NativeReprepareOutputQuiesced {
                    pending_generation, ..
                } if pending_generation == paused_generation)
                {
                    acknowledgements += 1;
                }
            }
            thread::yield_now();
        }
        assert_eq!(
            acknowledgements, 1,
            "consume the real one-shot output event while paused"
        );
        assert_eq!(facts.install.load(Ordering::SeqCst), 0);
        assert!(playback.finalize_native_control_anchor(paused_generation, anchor(20)).unwrap());
        assert!(playback
            .request_native_reprepare(paused_generation, request(&renderer, &facts, 0))
            .unwrap());
        while !settled(&playback) {
            assert!(Instant::now() < deadline);
            let poll = playback.poll(AudioPlaybackMode::Idle, anchor(20)).unwrap();
            assert!(poll.events.iter().all(|event| !matches!(
                event,
                AudioPlaybackEvent::NativeReprepareOutputQuiesced { .. }
            )));
            thread::yield_now();
        }
        assert_eq!(facts.install.load(Ordering::SeqCst), 1);
        assert_eq!(facts.settle.load(Ordering::SeqCst), 1);
        assert_eq!(renderer.requests.lock().len(), old_requests);
        assert!(
            playback.native_control_hold.is_some(),
            "no paused implicit Enter"
        );
        // The transport's later explicit resume rotates only current output
        // proof first; the installed resource result survives that rotation.
        playback.reprime(anchor(20)).unwrap();
        pump(&mut playback, |p| {
            p.native_control_output_retirement.is_some_and(|r| r.acknowledged)
        });
        assert!(playback
            .finalize_native_control_anchor(playback.generation, anchor(20))
            .unwrap());
        let generation = playback.generation;
        assert!(playback.finish_native_reprepare(generation, anchor(20)).unwrap());
        let fresh = playback.generation;
        pump(&mut playback, |_| {
            renderer.requests.lock().len() > old_requests
        });
        assert_eq!(facts.install.load(Ordering::SeqCst), 1);
        assert_eq!(
            renderer.requests.lock()[old_requests].continuity,
            AudioPcmContinuity::Enter(AudioPcmRenderGeneration::new(fresh))
        );
    }

    #[test]
    fn queued_restart_does_not_count_as_an_old_pcm_completion() {
        let (mut playback, renderer) = held();
        let facts = Arc::new(Facts::default());
        let _release = Release(facts.clone());
        let proof = match playback.native_control_hold.unwrap() {
            NativeControlHold::Reprepare {
                phase: NativeRepreparePhase::OutputRetired(proof),
                ..
            } => proof,
            _ => unreachable!(),
        };
        playback
            .pcm_journal
            .hold(proof.renderer_id, proof.generation - 1, proof.generation, 1)
            .unwrap();
        assert!(!playback
            .request_native_reprepare(playback.generation, request(&renderer, &facts, 0))
            .unwrap());
        assert!(playback.pcm_journal.old_work_unsettled());
        playback.pcm_journal.settle_old_completion();
        assert!(playback
            .request_native_reprepare(playback.generation, request(&renderer, &facts, 0))
            .unwrap());
        assert_eq!(playback.in_flight, 0);
        assert!(!playback.pcm_journal.old_work_unsettled());
        pump(&mut playback, |_| {
            facts.entered_retirement.load(Ordering::Acquire)
        });
        assert_eq!(facts.install.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn retirement_panic_and_settlement_refusal_never_release_the_hold() {
        for fail in [2, 3] {
            let (mut playback, renderer) = held();
            let facts = Arc::new(Facts::default());
            facts.release_retirement.store(true, Ordering::Release);
            assert!(playback
                .request_native_reprepare(playback.generation, request(&renderer, &facts, fail))
                .unwrap());
            if fail == 2 {
                pump(&mut playback, |p| {
                    p.render_retirement_faulted.load(AtomicOrdering::Acquire)
                });
                assert_eq!(facts.settle.load(Ordering::SeqCst), 0);
            } else {
                pump(&mut playback, refused);
            }
            assert_eq!(facts.high.load(Ordering::SeqCst), 1);
            assert!(!playback.finish_native_reprepare(playback.generation, anchor(0)).unwrap());
            assert!(playback.native_control_hold.is_some());
        }
    }

    #[test]
    fn normal_queue_close_does_not_fake_a_retirement_fault_or_external_alias_closure() {
        for external in [false, true] {
            let (mut playback, renderer) = held();
            let facts = Arc::new(Facts::default());
            let _release = Release(facts.clone());
            let alias = external.then(|| renderer.clone());
            assert!(playback
                .request_native_reprepare(playback.generation, request(&renderer, &facts, 0))
                .unwrap());
            drop(renderer);
            pump(&mut playback, |_| {
                facts.entered_retirement.load(Ordering::Acquire)
            });
            playback.begin_shutdown();
            facts.release_retirement.store(true, Ordering::Release);
            let deadline = Instant::now() + Duration::from_secs(2);
            while facts.drop_count.load(Ordering::SeqCst) == 0 {
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
            assert!(!playback.render_retirement_faulted.load(AtomicOrdering::Acquire));
            assert_eq!(facts.settle.load(Ordering::SeqCst), 0);
            let tracker = playback.foreign_owner_retirements.clone();
            let evidence = playback.shutdown_and_wait();
            assert_eq!(
                evidence.shutdown_owner_lifetime_unresolved_at_deadline,
                external
            );
            if external {
                assert!(!tracker.seal());
            }
            drop(alias);
            assert!(tracker.seal());
        }
    }

    #[test]
    fn queue_distinguishes_busy_from_closed_under_its_original_lock() {
        let fault = Arc::new(AtomicBool::new(false));
        let retirement = Arc::new(RenderOwnerRetirementQueue::new(fault));
        let tracker = Arc::new(ForeignOwnerRetirementTracker::default());
        let queue = RenderWorkQueue::new(1, retirement, tracker);
        let renderer = renderer();
        let make = || {
            RenderWork::Pcm(PcmRenderWork {
                generation: 1,
                renderer_id: mondrian_core::AudioPcmRendererMaterializationId::new(),
                request: AudioPcmRenderRequest {
                    start_sample: 0,
                    frame_count: 4,
                    sample_rate: 48_000,
                    channel_layout: AudioChannelLayout::Stereo,
                    continuity: AudioPcmContinuity::Enter(AudioPcmRenderGeneration::new(1)),
                },
                allow_window_silence: false,
                renderer: renderer.clone(),
                cancellation: ExecutionCancellationToken::new(),
            })
        };
        assert!(queue.try_push(make()).is_ok());
        assert!(matches!(
            queue.try_push(make()),
            Err(RenderQueuePushError::Busy(_))
        ));
        drop(queue.stop());
        assert!(matches!(
            queue.try_push(make()),
            Err(RenderQueuePushError::Closed(_))
        ));

        // Exercise the actual return helper's Busy branch with the same bounded
        // queue implementation, without an artificial producer/SDK retry.
        let (playback, actual) = held();
        let fault = Arc::new(AtomicBool::new(false));
        let retirement = Arc::new(RenderOwnerRetirementQueue::new(fault.clone()));
        let tracker = Arc::new(ForeignOwnerRetirementTracker::default());
        let return_queue = Arc::new(RenderWorkQueue::new(1, retirement.clone(), tracker.clone()));
        let pending = Arc::new(Facts::default());
        assert!(return_queue
            .push(RenderWork::NativeReprepare(Box::new(NativeReprepareWork {
                request: request(&actual, &pending, 0),
                admission: None,
                step: Step::Unadmitted,
            })))
            .is_ok());
        let facts = Arc::new(Facts::default());
        facts.release_retirement.store(true, Ordering::Release);
        let claim = playback.owner_installation.current_claim().unwrap().unwrap();
        let work = Box::new(NativeReprepareWork {
            request: request(&actual, &facts, 0),
            step: Step::Retire,
            admission: Some(Admission {
                generation: playback.generation,
                renderer_id: playback.renderer.as_ref().unwrap().materialization_id,
                renderer: actual.clone(),
                source: playback.owner_installation.clone(),
                expected: claim.clone(),
                notice: actual.notice,
                cancellation: ExecutionCancellationToken::new(),
                return_queue: Arc::downgrade(&return_queue),
                installed: Some(claim),
            }),
        });
        thread::spawn(move || retire_work(work, &retirement, &tracker)).join().unwrap();
        assert!(fault.load(Ordering::Acquire));
        assert_eq!(facts.settle.load(Ordering::SeqCst), 0);
        assert_eq!(facts.drop_count.load(Ordering::SeqCst), 1);
        drop(return_queue.stop());
    }
}
