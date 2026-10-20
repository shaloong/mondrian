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

    /// Move only the retained restart's PCM/output intent to the caller's anchor.
    ///
    /// Stale, refused, canceled or absent holds return `false`. Source-close,
    /// execution, coordinate, continuity and output preflight retain their typed
    /// errors. Success advances the PCM generation once and requires a new
    /// output acknowledgement; the original install/retire/settle work, claim and
    /// native cancellation remain unchanged. No Runtime getter or native call
    /// occurs here. Actual installation and Fresh entry still revalidate there.
    pub fn seek_native_reprepare(
        &mut self,
        pending_generation: u64,
        anchor: AudioSamplePosition,
    ) -> Result<bool, AudioPlaybackError> {
        self.ensure_source_close_idle()?;
        self.ensure_render_execution_available()?;
        if pending_generation != self.generation
            || !matches!(self.native_control_hold,
                Some(NativeControlHold::Reprepare { admission, .. })
                    if admission != NativeReprepareAdmission::Refused)
            || self.renderer.is_none()
            || self.native_control_cancellation.is_canceled()
            || self.owner_installation.current_claim()?.is_none()
        {
            return Ok(false);
        }
        self.validate_reprime_anchor_pure(anchor)?;
        self.validate_reprime_output()?;
        let renderer = self.renderer.as_ref().expect("checked retained Source");
        renderer.continuity_model.validate_fresh_entry(anchor)?;
        self.reprime_prevalidated(
            anchor,
            false,
            self.renderer.clone(),
            PcmRetirementPolicy::PreserveNativeHold,
        )?;
        self.pcm_journal.discard_held_suffix();
        self.render_blocked = true;
        Ok(true)
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
        match self.render_queue.try_push(RenderWork::NativeReprepare(work)) {
            Ok(()) => {
                self.native_owner_binding_status = None;
                Ok(true)
            }
            Err(error) => {
                let (mut work, reason) = match error {
                    RenderQueuePushError::Busy(work) => {
                        (work, AudioPcmNativeReprepareRefusal::ResourceAdmission)
                    }
                    RenderQueuePushError::Closed(work) => {
                        (work, AudioPcmNativeReprepareRefusal::WorkerUnavailable)
                    }
                };
                if let RenderWork::NativeReprepare(work) = &mut work {
                    work.terminal();
                }
                self.handoff_retired_render_owners(VecDeque::from([work]), None);
                if let Some(NativeControlHold::Reprepare { admission, .. }) =
                    &mut self.native_control_hold
                {
                    *admission = NativeReprepareAdmission::Refused;
                }
                // The one physical acknowledgement has already been consumed.
                // Report a real unsent refusal instead of a silent Unstarted wait.
                Err(AudioPlaybackError::NativeReprepareQueueRefused(reason))
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
    struct LatePcm {
        entered: AtomicBool,
        release: AtomicBool,
        outcome: u8,
    }
    struct ReleaseLate(Arc<LatePcm>);
    impl Drop for ReleaseLate {
        fn drop(&mut self) {
            self.0.release.store(true, Ordering::Release);
        }
    }
    struct Renderer {
        verifier: Mutex<Option<AudioOwnerInstallationVerifier>>,
        verifier_reads: AtomicUsize,
        source_closed_queries: Mutex<Vec<thread::ThreadId>>,
        late: Mutex<Option<Arc<LatePcm>>>,
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
            self.verifier_reads.fetch_add(1, Ordering::SeqCst);
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
        fn validate_native_author_source_closed(
            &self,
            _: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<()> {
            self.source_closed_queries.lock().push(thread::current().id());
            Ok(())
        }
        fn render(
            &self,
            request: AudioPcmRenderRequest,
            _: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
            let ordinal = {
                let mut requests = self.requests.lock();
                let ordinal = requests.len();
                requests.push(request);
                ordinal
            };
            let late = self.late.lock().clone();
            if let Some(late) = late
                && ordinal == 1
            {
                late.entered.store(true, Ordering::Release);
                let deadline = Instant::now() + Duration::from_secs(2);
                while !late.release.load(Ordering::Acquire) {
                    assert!(
                        Instant::now() < deadline,
                        "test must release actual old render"
                    );
                    thread::yield_now();
                }
                return match late.outcome {
                    0 => Ok(AudioPcmRenderOutcome::Rendered(AudioBuffer::silent(
                        request.sample_rate,
                        request.channel_layout,
                        request.frame_count,
                    ))),
                    1 => Err(mondrian_core::MondrianError::Cancelled),
                    2 => Ok(AudioPcmRenderOutcome::DeferredForReprepare(self.notice)),
                    _ => unreachable!(),
                };
            }
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
            verifier_reads: AtomicUsize::new(0),
            source_closed_queries: Mutex::new(Vec::new()),
            late: Mutex::new(None),
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
    fn terminal_restart_refusal_can_query_actual_source_closed_before_clear() {
        let (mut playback, renderer) = held();
        let facts = Arc::new(Facts::default());
        let caller = thread::current().id();
        let claim = playback.owner_installation.current_claim().unwrap().unwrap();
        let native_cancel = playback.native_control_cancellation.clone();
        assert!(playback
            .request_native_reprepare(playback.generation, request(&renderer, &facts, 1))
            .unwrap());
        pump(&mut playback, refused);
        assert_eq!(facts.install.load(Ordering::SeqCst), 1);
        assert_eq!(facts.settle.load(Ordering::SeqCst), 0);
        assert!(playback.native_control_output_retirement.is_none());
        assert!(renderer.source_closed_queries.lock().is_empty());
        let erased: Arc<dyn AudioPcmRenderer> = renderer.clone();
        let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let generation = playback.generation;
            let ticket = playback
                .request_source_close(&erased, anchor(20))
                .expect("REFUSED_SOURCE_CLOSE must admit actual SourceClosed query");
            assert_eq!(playback.generation, generation + 1, "one original Suspend");
            assert!(refused(&playback));
            assert!(!native_cancel.is_canceled());
            assert!(playback.matches_source(&erased));
            pump(&mut playback, |p| {
                !matches!(
                    p.source_close_status(&ticket).unwrap(),
                    AudioPcmSourceCloseStatus::Pending
                )
            });
            assert!(matches!(
                playback.source_close_status(&ticket).unwrap(),
                AudioPcmSourceCloseStatus::Closed
            ));
            assert!(refused(&playback), "Closed query alone cannot release HOLD");
            assert!(renderer.verifier.lock().as_ref().unwrap().validate(&claim).is_ok());
            let queries = renderer.source_closed_queries.lock();
            assert_eq!(queries.len(), 1);
            assert_ne!(queries[0], caller, "query uses the original producer");
            assert_eq!(queries[0], facts.calls.lock()[0].1);
            drop(queries);
            let renders = renderer.requests.lock().len();
            playback.finish_source_close(&ticket, anchor(20)).unwrap();
            assert!(!playback.matches_source(&erased));
            assert!(playback.native_control_hold.is_none());
            assert!(playback.owner_installation.current_claim().unwrap().is_none());
            assert!(matches!(
                renderer.verifier.lock().as_ref().unwrap().validate(&claim),
                Err(AudioOwnerInstallationError::NotInstalled)
            ));
            assert_eq!(renderer.requests.lock().len(), renders);
            assert_eq!(facts.install.load(Ordering::SeqCst), 1);
            assert_eq!(facts.settle.load(Ordering::SeqCst), 0);
            assert_eq!(
                playback.finish_source_close(&ticket, anchor(20)),
                Err(AudioPlaybackError::SourceCloseSuperseded)
            );
        }));
        drop((erased, renderer));
        let cleanup = playback.shutdown_and_wait();
        assert!(
            cleanup.all_workers_terminated(),
            "Source-close cleanup: {cleanup:?}"
        );
        if let Err(payload) = target {
            std::panic::resume_unwind(payload);
        }
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
        // Same-Source resume/output rotation, not an explicit fresh seek.
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
        playback
            .prepare(
                anchor(20),
                renderer.clone(),
                AudioPcmContinuityModel::GenerationState,
            )
            .unwrap();
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
        playback
            .prepare(
                anchor(20),
                renderer.clone(),
                AudioPcmContinuityModel::GenerationState,
            )
            .unwrap();
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
        playback
            .prepare(
                anchor(20),
                renderer.clone(),
                AudioPcmContinuityModel::GenerationState,
            )
            .unwrap();
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

    // This provider is only placed in the actual queue; none of its foreign
    // callbacks is allowed to execute in the mixed-frontier admission control.
    struct QueuedAuthor(Arc<dyn AudioPcmRenderer>);
    impl AudioPcmNativeAuthorHandoff for QueuedAuthor {
        fn renderer(&self) -> &Arc<dyn AudioPcmRenderer> {
            &self.0
        }
        fn proposal(&self) -> &mondrian_core::NativeAudioAuthorProposal {
            panic!("queue-only author callback")
        }
        fn expected_target(&self) -> mondrian_core::NativeAudioEditorTarget {
            panic!("queue-only author callback")
        }
        fn intent(&self) -> AudioPcmNativeAuthorHandoffIntent {
            panic!("queue-only author callback")
        }
        fn continuity_policy(
            &self,
            _: &ExecutionCancellationToken,
        ) -> Result<AudioPcmNativeAuthorHandoffContinuity, AudioPcmNativeAuthorHandoffRefusal>
        {
            panic!("queue-only author callback")
        }
        fn acknowledge(
            &self,
            _: &ExecutionCancellationToken,
        ) -> AudioPcmNativeAuthorHandoffOutcome {
            panic!("queue-only author callback")
        }
        fn apply(
            &self,
            _: AudioOwnerInstallationVerifier,
            _: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
            _: &ExecutionCancellationToken,
        ) -> AudioPcmNativeAuthorHandoffOutcome {
            panic!("queue-only author callback")
        }
    }

    fn fill_original_native_frontier(playback: &AudioPlayback, actual: &Arc<Renderer>) {
        let current = playback.renderer.as_ref().unwrap();
        let claim = playback.owner_installation.current_claim().unwrap().unwrap();
        let verifier = actual.owner_installation_verifier().unwrap();
        for _ in 0..MAX_PENDING_NATIVE_EDITOR_WORK {
            assert!(playback
                .render_queue
                .try_push(RenderWork::NativeEditor(Box::new(NativeEditorWork {
                    renderer_id: current.materialization_id,
                    renderer: actual.clone(),
                    claim: claim.clone(),
                    cancellation: ExecutionCancellationToken::new(),
                    intent: mondrian_core::NativeAudioEditorControlIntent::ResolveAndOpen {
                        locator: mondrian_core::NativeAudioEditorLocator {
                            sequence: mondrian_core::SequenceId::new(),
                            instance: actual.notice.instance_id,
                            owner: mondrian_core::NativeAudioEditorOccurrenceOwner::Output(
                                mondrian_core::ProgramOutputId::new()
                            ),
                            insertion: mondrian_core::NativeAudioEditorInsertion::PreFader,
                        },
                        title: mondrian_core::NativeAudioEditorTitle::new("queue only".into())
                            .unwrap(),
                    },
                })))
                .is_ok());
        }
        assert!(playback
            .render_queue
            .try_push(RenderWork::NativeObservations(NativeObservationWork {
                generation: playback.generation,
                renderer_id: current.materialization_id,
                renderer: actual.clone(),
                cancellation: ExecutionCancellationToken::new(),
                installation_generation: Some(claim.generation()),
                operation: NativeObservationOperation::Poll,
            }))
            .is_ok());
        assert!(playback
            .render_queue
            .try_push(RenderWork::NativeOwnerBindings(NativeOwnerBindingWork {
                generation: playback.generation,
                renderer_id: current.materialization_id,
                renderer: actual.clone(),
                claim: claim.clone(),
                verifier,
                cancellation: ExecutionCancellationToken::new(),
            }))
            .is_ok());
        assert!(playback
            .render_queue
            .try_push(RenderWork::NativeAuthorHandoff(Box::new(
                native_author::NativeAuthorHandoffWork {
                    generation: playback.generation,
                    renderer_id: current.materialization_id,
                    request: Box::new(QueuedAuthor(actual.clone())),
                    source: playback.owner_installation.clone(),
                    expected: claim,
                    cancellation: ExecutionCancellationToken::new(),
                    output_quiescent: false,
                }
            )))
            .is_ok());
        assert_eq!(playback.render_queue.state.lock().pending.len(), 5);
        // The five native entries cannot take either reserved PCM position.
        for start in [0, 4] {
            assert!(playback
                .render_queue
                .try_push(RenderWork::Pcm(PcmRenderWork {
                    generation: playback.generation,
                    renderer_id: current.materialization_id,
                    renderer: actual.clone(),
                    request: AudioPcmRenderRequest {
                        start_sample: start,
                        frame_count: 4,
                        sample_rate: playback.config.sample_rate,
                        channel_layout: playback.config.channel_layout,
                        continuity: AudioPcmContinuity::Enter(AudioPcmRenderGeneration::new(
                            playback.generation
                        )),
                    },
                    allow_window_silence: false,
                    cancellation: ExecutionCancellationToken::new(),
                }))
                .is_ok());
        }
    }

    #[test]
    fn restart_queue_refusal_preserves_original_frontier_and_is_not_silent() {
        let mut config = playback_config();
        config.max_in_flight = 250;
        assert_eq!(validate_config(config), Ok(()));
        config.max_in_flight = 251;
        assert_eq!(
            validate_config(config),
            Err(AudioPlaybackConfigError::ExceedsRetirementCapacity)
        );
        for closed in [false, true] {
            let (mut playback, actual) = held();
            // Keep the original producer alive and idle. Substitute only this
            // test's queue so its actual typed entries cannot race a consumer.
            // Restore it even after an assertion failure before normal cleanup.
            let queue = Arc::new(RenderWorkQueue::new(
                2,
                playback.render_retirement_queue.clone(),
                playback.foreign_owner_retirements.clone(),
            ));
            let original = std::mem::replace(&mut playback.render_queue, queue.clone());
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                fill_original_native_frontier(&playback, &actual);
                if closed {
                    let pending = queue.stop();
                    playback.handoff_retired_render_owners(pending, None);
                }
                let claim =
                    playback.owner_installation.current_claim().unwrap().unwrap().generation();
                let facts = Arc::new(Facts::default());
                let reason = if closed {
                    AudioPcmNativeReprepareRefusal::WorkerUnavailable
                } else {
                    AudioPcmNativeReprepareRefusal::ResourceAdmission
                };
                let error = playback
                    .request_native_reprepare(playback.generation, request(&actual, &facts, 0))
                    .unwrap_err();
                assert_eq!(
                    error,
                    AudioPlaybackError::NativeReprepareQueueRefused(reason)
                );
                assert!(
                    error.to_string().contains(&format!("{reason:?}")),
                    "the existing App error mapping can expose the actual refusal"
                );
                assert!(refused(&playback));
                assert!(playback.render_blocked);
                assert_eq!(
                    playback.owner_installation.current_claim().unwrap().unwrap().generation(),
                    claim
                );
                assert!(!playback.finish_native_reprepare(playback.generation, anchor(0)).unwrap());
                let deadline = Instant::now() + Duration::from_secs(2);
                while facts.drop_count.load(Ordering::SeqCst) == 0 {
                    assert!(
                        Instant::now() < deadline,
                        "original retirement must release unsent carrier"
                    );
                    thread::yield_now();
                }
                assert_eq!(facts.install.load(Ordering::SeqCst), 0);
                assert!(!facts.entered_retirement.load(Ordering::Acquire));
                assert_eq!(facts.settle.load(Ordering::SeqCst), 0);
                assert_eq!(facts.drop_count.load(Ordering::SeqCst), 1);
                assert!(facts.calls.lock().iter().all(|(_, id)| *id != thread::current().id()));
                assert!(!playback.render_retirement_faulted.load(AtomicOrdering::Acquire));
                if !closed {
                    assert_eq!(queue.state.lock().pending.len(), 7);
                }
            }));
            let pending = queue.stop();
            playback.handoff_retired_render_owners(pending, None);
            playback.render_queue = original;
            if let Err(payload) = outcome {
                std::panic::resume_unwind(payload);
            }
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

    #[test]
    fn seek_restart_preserves_install_and_settle_through_retirement_and_settled_output_rotations() {
        let (mut playback, renderer) = held();
        let facts = Arc::new(Facts::default());
        let _release = Release(facts.clone());
        let admitted = playback.generation;
        let old_proof = match playback.native_control_hold.unwrap() {
            NativeControlHold::Reprepare {
                phase: NativeRepreparePhase::OutputRetired(p), ..
            } => p,
            _ => unreachable!(),
        };
        assert!(playback
            .request_native_reprepare(admitted, request(&renderer, &facts, 0))
            .unwrap());
        pump(&mut playback, |_| {
            facts.entered_retirement.load(Ordering::Acquire)
        });
        let installed = playback.owner_installation.current_claim().unwrap().unwrap().generation();
        assert!(playback.current_reprepare_output(old_proof));
        let native_cancel = playback.native_control_cancellation.clone();
        let requests = renderer.requests.lock().len();
        for target in [20, 40] {
            let previous = playback.generation;
            let old_pcm_cancel = playback.generation_cancellation.clone();
            let reads = renderer.verifier_reads.load(Ordering::SeqCst);
            assert!(playback.seek_native_reprepare(previous, anchor(target)).unwrap());
            assert_eq!(
                renderer.verifier_reads.load(Ordering::SeqCst),
                reads,
                "pure PCM seek never obtains the Runtime verifier"
            );
            assert_eq!(playback.generation, previous + 1);
            assert_eq!(playback.next_start_sample, target);
            assert!(old_pcm_cancel.is_canceled());
            assert!(!native_cancel.is_canceled());
            assert!(
                matches!(playback.native_control_hold, Some(NativeControlHold::Reprepare {
                phase: NativeRepreparePhase::WaitingOutput,
                admission: NativeReprepareAdmission::InFlight { generation }, ..
            }) if generation == admitted)
            );
            assert_eq!(
                playback.owner_installation.current_claim().unwrap().unwrap().generation(),
                installed
            );
            assert!(!playback.current_reprepare_output(old_proof));
            assert!(!playback.finish_native_reprepare(admitted, anchor(0)).unwrap());
            assert!(!playback
                .request_native_reprepare(playback.generation, request(&renderer, &facts, 0))
                .unwrap());
        }
        facts.release_retirement.store(true, Ordering::Release);
        pump(&mut playback, settled);
        assert_eq!(facts.install.load(Ordering::SeqCst), 1);
        assert_eq!(facts.settle.load(Ordering::SeqCst), 1);
        assert_eq!(renderer.requests.lock().len(), requests);
        let before = playback.generation;
        assert!(playback.seek_native_reprepare(before, anchor(80)).unwrap());
        assert_eq!(playback.generation, before + 1);
        assert!(settled(&playback));
        assert!(!playback.finish_native_reprepare(before, anchor(40)).unwrap());
        assert!(!playback.finish_native_reprepare(playback.generation, anchor(80)).unwrap());
        pump(&mut playback, |p| {
            p.native_control_output_retirement.is_some_and(|r| r.acknowledged)
        });
        let current = playback.generation;
        assert!(playback.finalize_native_control_anchor(current, anchor(80)).unwrap());
        let current_proof = match playback.native_control_hold.unwrap() {
            NativeControlHold::Reprepare {
                phase: NativeRepreparePhase::OutputRetired(p), ..
            } => p,
            _ => unreachable!(),
        };
        assert!(playback.current_reprepare_output(current_proof));
        assert!(playback.finish_native_reprepare(current, anchor(80)).unwrap());
        let fresh = playback.generation;
        pump(&mut playback, |_| renderer.requests.lock().len() > requests);
        assert_eq!(renderer.requests.lock()[requests].start_sample, 80);
        assert_eq!(
            renderer.requests.lock()[requests].continuity,
            AudioPcmContinuity::Enter(AudioPcmRenderGeneration::new(fresh))
        );
        assert_eq!(facts.install.load(Ordering::SeqCst), 1);
        assert_eq!(facts.settle.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn seek_restart_discards_actual_late_success_error_and_deferred_without_recovery_or_revival() {
        for outcome in 0..3 {
            let (output, _) = super::super::tests::fake_output();
            let mut playback = AudioPlayback::with_output(playback_config(), output).unwrap();
            let late = Arc::new(LatePcm {
                entered: AtomicBool::new(false),
                release: AtomicBool::new(false),
                outcome,
            });
            let _release = ReleaseLate(late.clone());
            let renderer = renderer();
            *renderer.late.lock() = Some(late.clone());
            playback
                .prepare(
                    anchor(0),
                    renderer.clone(),
                    AudioPcmContinuityModel::GenerationState,
                )
                .unwrap();
            playback.poll(AudioPlaybackMode::Preroll, anchor(0)).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !late.entered.load(Ordering::Acquire) {
                assert!(
                    Instant::now() < deadline,
                    "actual second old render must be popped before HOLD"
                );
                thread::yield_now();
            }
            let poll = playback.poll(AudioPlaybackMode::Preroll, anchor(0)).unwrap();
            assert_eq!(
                poll.events
                    .iter()
                    .filter(|e| matches!(e, AudioPlaybackEvent::NativeReprepareRequired { .. }))
                    .count(),
                1
            );
            assert!(playback.pcm_journal.old_work_unsettled());
            let source_generation = renderer.requests.lock()[0].continuity.generation().get();
            let renderer_id = playback.renderer.as_ref().unwrap().materialization_id;
            for target in [20, 40] {
                assert!(playback
                    .seek_native_reprepare(playback.generation, anchor(target))
                    .unwrap());
                assert!(playback
                    .pcm_journal
                    .expects_old_completion(renderer_id, source_generation));
                assert_eq!(
                    playback.visit_native_retained_pcm(playback.generation, anchor(0), |_| panic!(
                        "unsettled"
                    )),
                    Err(crate::AudioRetainedPcmError::OldWorkUnsettled)
                );
            }
            let current = playback.generation;
            late.release.store(true, Ordering::Release);
            pump(&mut playback, |p| !p.pcm_journal.old_work_unsettled());
            assert_eq!(
                playback.generation, current,
                "old completion is not another restart or recovery"
            );
            assert!(!playback.pcm_journal.expects_old_completion(renderer_id, source_generation));
            assert_eq!(
                playback.visit_native_retained_pcm(current, anchor(0), |_| panic!("discarded")),
                Err(crate::AudioRetainedPcmError::CoverageUnavailable)
            );
            assert_eq!(renderer.requests.lock().len(), 2);
            assert!(matches!(
                playback.native_control_hold,
                Some(NativeControlHold::Reprepare {
                    admission: NativeReprepareAdmission::Unstarted,
                    ..
                })
            ));
            let snapshot = playback.snapshot(AudioPlaybackMode::Preroll);
            assert_eq!(snapshot.output.unwrap().buffered_frames, 0);
            assert_eq!(snapshot.render_generation_recovery_count, 0);
            assert_eq!(snapshot.render_substitution_count, 0);
            assert_eq!(snapshot.underrun_recovery_count, 0);
            drop(renderer);
            assert!(playback.shutdown_and_wait().all_workers_terminated());
        }
    }

    #[test]
    fn seek_restart_stale_refused_canceled_and_invalid_preflight_leave_the_held_owner_unchanged() {
        let (mut playback, renderer) = held();
        let original = playback.generation;
        let hold = playback.native_control_hold;
        let claim = playback.owner_installation.current_claim().unwrap().unwrap().generation();
        let old_pcm_cancel = playback.generation_cancellation.clone();
        assert!(!playback.seek_native_reprepare(original - 1, anchor(20)).unwrap());
        assert_eq!(
            playback.seek_native_reprepare(original, anchor(-1)),
            Err(AudioPlaybackError::NegativeSampleAnchor)
        );
        let wrong_grid =
            AudioSamplePosition::new(20, mondrian_core::AudioSampleRate::new(999).unwrap());
        assert!(matches!(
            playback.seek_native_reprepare(original, wrong_grid),
            Err(AudioPlaybackError::InvalidSampleAnchor(
                AudioTimeError::RateMismatch { .. }
            ))
        ));
        playback.renderer.as_mut().unwrap().continuity_model =
            AudioPcmContinuityModel::GenerationStateFromOrigin(anchor(0));
        assert_eq!(
            playback.seek_native_reprepare(original, anchor(20)),
            Err(AudioPlaybackError::FreshEntryOriginMismatch {
                required: anchor(0),
                actual: anchor(20)
            })
        );
        playback.renderer.as_mut().unwrap().continuity_model =
            AudioPcmContinuityModel::GenerationState;
        playback.generation = u64::MAX;
        assert_eq!(
            playback.seek_native_reprepare(u64::MAX, anchor(20)),
            Err(AudioPlaybackError::CoordinateOverflow)
        );
        playback.generation = original;
        assert_eq!(playback.native_control_hold, hold);
        assert_eq!(playback.next_start_sample, 0);
        assert!(!old_pcm_cancel.is_canceled());
        assert_eq!(
            playback.owner_installation.current_claim().unwrap().unwrap().generation(),
            claim
        );
        assert_eq!(
            playback.visit_native_retained_pcm(original, anchor(0), |_| panic!("no complete PCM")),
            Err(crate::AudioRetainedPcmError::CoverageUnavailable)
        );
        let facts = Arc::new(Facts::default());
        assert!(playback
            .request_native_reprepare(original, request(&renderer, &facts, 1))
            .unwrap());
        pump(&mut playback, refused);
        assert_eq!(facts.install.load(Ordering::SeqCst), 1);
        assert!(!playback.seek_native_reprepare(original, anchor(20)).unwrap());
        assert_eq!(playback.generation, original);
        playback.clear_source(anchor(0)).unwrap();
        assert!(!playback.seek_native_reprepare(playback.generation, anchor(20)).unwrap());
        drop(renderer);
        assert!(playback.shutdown_and_wait().all_workers_terminated());

        let (mut canceled, renderer) = held();
        let original = canceled.generation;
        canceled.native_control_cancellation.cancel();
        assert!(!canceled.seek_native_reprepare(original, anchor(20)).unwrap());
        assert_eq!(canceled.generation, original);
        drop(renderer);
        assert!(canceled.shutdown_and_wait().all_workers_terminated());
    }

    #[test]
    fn seek_restart_intent_does_not_authorize_a_replaced_runtime_registration() {
        let (mut playback, renderer) = held();
        let old = playback.owner_installation.current_claim().unwrap().unwrap();
        let original_verifier = renderer.owner_installation_verifier().unwrap();
        let (_, replacement) = playback.owner_installation.register().unwrap();
        *renderer.verifier.lock() = Some(replacement);
        let reads = renderer.verifier_reads.load(Ordering::SeqCst);
        assert!(playback.seek_native_reprepare(playback.generation, anchor(20)).unwrap());
        assert_eq!(renderer.verifier_reads.load(Ordering::SeqCst), reads);
        assert_eq!(
            original_verifier.validate(&old),
            Ok(()),
            "Source claim alone is not Runtime permission"
        );
        pump(&mut playback, |p| {
            p.native_control_output_retirement.is_some_and(|r| r.acknowledged)
        });
        let current = playback.generation;
        assert!(playback.finalize_native_control_anchor(current, anchor(20)).unwrap());
        let facts = Arc::new(Facts::default());
        assert!(playback
            .request_native_reprepare(current, request(&renderer, &facts, 0))
            .unwrap());
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut result = None;
        while !refused(&playback) {
            assert!(
                Instant::now() < deadline,
                "original producer must reject the replacement"
            );
            for event in playback.poll(AudioPlaybackMode::Preroll, anchor(20)).unwrap().events {
                if let AudioPlaybackEvent::NativeReprepareObserved { status, .. } = event {
                    assert!(result.replace(status).is_none());
                }
            }
            thread::yield_now();
        }
        assert_eq!(
            result,
            Some(AudioPcmNativeReprepareStatus::Refused(
                AudioPcmNativeReprepareRefusal::Superseded
            ))
        );
        assert_eq!(facts.install.load(Ordering::SeqCst), 0);
        assert_eq!(facts.settle.load(Ordering::SeqCst), 0);
        assert_eq!(
            playback.owner_installation.current_claim().unwrap().unwrap().generation(),
            old.generation()
        );
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Preroll).output.unwrap().buffered_frames,
            0
        );
        assert!(!playback.finish_native_reprepare(current, anchor(20)).unwrap());
    }

    #[test]
    fn seek_restart_retains_source_close_and_closed_execution_error_precedence() {
        let (output, _) = super::super::tests::fake_output();
        let mut playback = AudioPlayback::with_output(playback_config(), output).unwrap();
        let renderer = renderer();
        renderer.restarted.store(true, Ordering::Release);
        playback
            .prepare(
                anchor(0),
                renderer.clone(),
                AudioPcmContinuityModel::GenerationState,
            )
            .unwrap();
        let erased: Arc<dyn AudioPcmRenderer> = renderer.clone();
        let _ticket = playback.request_source_close(&erased, anchor(0)).unwrap();
        let before = playback.generation;
        assert_eq!(
            playback.seek_native_reprepare(before, anchor(20)),
            Err(AudioPlaybackError::SourceClosePending)
        );
        assert_eq!(playback.generation, before);
        drop(erased);
        drop(renderer);
        assert!(playback.shutdown_and_wait().all_workers_terminated());

        let (mut closed, renderer) = held();
        closed.begin_shutdown();
        let before = closed.generation;
        assert_eq!(
            closed.seek_native_reprepare(before, anchor(20)),
            Err(AudioPlaybackError::ExecutionUnavailable)
        );
        assert_eq!(closed.generation, before);
        drop(renderer);
        assert!(closed.shutdown_and_wait().all_workers_terminated());
    }

    #[test]
    fn seek_restart_paused_unstarted_and_waiting_intent_settles_without_entering_pcm() {
        let (mut playback, renderer) = held();
        let requests = renderer.requests.lock().len();
        let native_cancel = playback.native_control_cancellation.clone();
        playback.suspend(anchor(20)).unwrap();
        for target in [40, 60] {
            let before = playback.generation;
            assert!(playback.seek_native_reprepare(before, anchor(target)).unwrap());
            assert_eq!(playback.generation, before + 1);
            assert!(matches!(
                playback.native_control_hold,
                Some(NativeControlHold::Reprepare {
                    phase: NativeRepreparePhase::WaitingOutput,
                    admission: NativeReprepareAdmission::Unstarted,
                    ..
                })
            ));
        }
        let current = playback.generation;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !playback.native_control_output_retirement.is_some_and(|r| r.acknowledged) {
            assert!(Instant::now() < deadline);
            playback.poll(AudioPlaybackMode::Idle, anchor(60)).unwrap();
            thread::yield_now();
        }
        assert!(playback.finalize_native_control_anchor(current, anchor(60)).unwrap());
        let facts = Arc::new(Facts::default());
        facts.release_retirement.store(true, Ordering::Release);
        assert!(playback
            .request_native_reprepare(current, request(&renderer, &facts, 0))
            .unwrap());
        while !settled(&playback) {
            assert!(Instant::now() < deadline);
            playback.poll(AudioPlaybackMode::Idle, anchor(60)).unwrap();
            thread::yield_now();
        }
        assert_eq!(facts.install.load(Ordering::SeqCst), 1);
        assert_eq!(facts.settle.load(Ordering::SeqCst), 1);
        assert_eq!(playback.generation, current);
        assert_eq!(playback.next_start_sample, 60);
        assert_eq!(renderer.requests.lock().len(), requests);
        assert!(!native_cancel.is_canceled());
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Idle).output.unwrap().buffered_frames,
            0
        );
        drop(renderer);
        assert!(playback.shutdown_and_wait().all_workers_terminated());
    }
}
