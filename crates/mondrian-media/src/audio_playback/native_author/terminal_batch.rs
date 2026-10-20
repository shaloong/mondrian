//! Terminal-frontier continuation on the original queue. No native owner crosses UI.
use super::*;
use std::sync::Weak;

/// Frozen request metadata. Runtime admission, not this value, proves support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPcmNativeAuthorHandoffMode {
    Inline,
    TerminalBatch,
}

/// Host-only association. Clones grant no author, Source or native permission.
#[derive(Debug, Clone)]
pub struct AudioPcmNativeAuthorResumeKey(Arc<()>);
impl PartialEq for AudioPcmNativeAuthorResumeKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for AudioPcmNativeAuthorResumeKey {}
impl AudioPcmNativeAuthorResumeKey {
    fn new() -> Self {
        Self(Arc::new(()))
    }
}

/// Actual adapter result. The target is copied from the prepared original anchor.
pub enum AudioPcmNativeAuthorTerminalBatchOutcome {
    /// All exact prefixes acknowledged and the owned preparation already dropped.
    Acknowledged {
        claim: AudioOwnerInstallationClaim,
        target: mondrian_core::NativeAudioEditorTarget,
    },
    /// CAS happened. Preserve the new claim and retry only remaining Acks.
    InstalledAwaitingAcknowledgement {
        claim: AudioOwnerInstallationClaim,
        target: mondrian_core::NativeAudioEditorTarget,
    },
    /// No CAS is asserted. A still-current published candidate stays prepared.
    Refused(AudioPcmNativeAuthorHandoffRefusal),
}

/// Same-source historical observation, never permission to synthesize a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPcmNativeAuthorTerminalBatchStatus {
    Acknowledged {
        target: mondrian_core::NativeAudioEditorTarget,
    },
    InstalledAwaitingAcknowledgement {
        target: mondrian_core::NativeAudioEditorTarget,
    },
    Refused(AudioPcmNativeAuthorHandoffRefusal),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Prepare,
    AwaitCanonical,
    Install,
    AwaitInstall,
    Ack,
    AwaitAck,
    Terminal,
}

pub(in crate::audio_playback) struct Continuation {
    pub(in crate::audio_playback) identity: AudioPcmNativeAuthorResumeKey,
    key: AudioPcmNativeAuthorResumeKey,
    step: Step,
    destination: Option<(
        mondrian_core::AudioOwnerInstallationRegistration,
        AudioOwnerInstallationVerifier,
    )>,
    authorization: Option<mondrian_core::AudioOwnerRuntimeInstallationAuthorization>,
    target: Option<mondrian_core::NativeAudioEditorTarget>,
    queue: Weak<RenderWorkQueue>,
    anchor: mondrian_core::NativeAudioAuthorProposal,
    renderer: Arc<dyn AudioPcmRenderer>,
}
impl Continuation {
    pub(in crate::audio_playback) fn new(
        queue: &Arc<RenderWorkQueue>,
        request: &dyn AudioPcmNativeAuthorHandoff,
    ) -> Self {
        let identity = AudioPcmNativeAuthorResumeKey::new();
        Self {
            key: identity.clone(),
            identity,
            step: Step::Prepare,
            destination: None,
            authorization: None,
            target: None,
            queue: Arc::downgrade(queue),
            anchor: request.proposal().clone(),
            renderer: Arc::clone(request.renderer()),
        }
    }
    fn waiting(&self) -> bool {
        matches!(
            self.step,
            Step::AwaitCanonical | Step::AwaitInstall | Step::AwaitAck
        )
    }
}

/// Custody/credit only. Protocol and Ack progress remain in the original Work/provider.
pub(in crate::audio_playback) enum Lane {
    Vacant,
    Queued(AudioPcmNativeAuthorResumeKey),
    Active(AudioPcmNativeAuthorResumeKey),
    Parked(Box<NativeAuthorHandoffWork>),
    Retiring(AudioPcmNativeAuthorResumeKey),
    Poisoned(AudioPcmNativeAuthorResumeKey),
}
impl Lane {
    pub(in crate::audio_playback) fn busy(&self) -> bool {
        !matches!(self, Self::Vacant)
    }
    pub(in crate::audio_playback) fn parked(&self) -> bool {
        matches!(self, Self::Parked(_))
    }
    pub(in crate::audio_playback) fn outside_pending(&self) -> usize {
        usize::from(!matches!(self, Self::Vacant | Self::Queued(_)))
    }
}

// Only called under the original queue lock: queued/parked Work cannot change
// its expected claim until pop/resume takes that same lock. The active worker
// releases every Source guard before parking, so this queue -> short Source
// metadata read has no reverse Source-guard -> queue acquisition. No renderer,
// Runtime or native callback belongs here. This preserves custody, not authority.
fn preserve_author_work(
    work: &NativeAuthorHandoffWork,
    renderer: Option<mondrian_core::AudioPcmRendererMaterializationId>,
    source: &Arc<mondrian_core::AudioOwnerInstallationSource>,
    policy: PcmRetirementPolicy,
) -> bool {
    matches!(
        policy,
        PcmRetirementPolicy::Suspend
            | PcmRetirementPolicy::PreserveNativeHold
            | PcmRetirementPolicy::BeginNativeHold
    ) && work.continuation.is_some()
        && Some(work.renderer_id) == renderer
        && Arc::ptr_eq(&work.source, source)
        && source
            .current_claim()
            .ok()
            .flatten()
            .is_some_and(|claim| claim.generation() == work.expected.generation())
}
pub(in crate::audio_playback) fn preserve_work(
    work: &RenderWork,
    renderer: Option<mondrian_core::AudioPcmRendererMaterializationId>,
    source: &Arc<mondrian_core::AudioOwnerInstallationSource>,
    policy: PcmRetirementPolicy,
) -> bool {
    matches!(work, RenderWork::NativeAuthorHandoff(w) if preserve_author_work(w, renderer, source, policy))
}
pub(in crate::audio_playback) fn drain_parked(
    state: &mut RenderQueueState,
    retired: &mut VecDeque<RenderWork>,
) {
    if let Lane::Parked(_) = &state.author_batch {
        let Lane::Parked(work) = std::mem::replace(&mut state.author_batch, Lane::Vacant) else {
            unreachable!()
        };
        state.author_batch = Lane::Retiring(work.continuation.as_ref().unwrap().identity.clone());
        retired.push_back(RenderWork::NativeAuthorHandoff(work));
    }
}
pub(in crate::audio_playback) fn retiring_queued(
    state: &mut RenderQueueState,
    retired: &VecDeque<RenderWork>,
) {
    for work in retired {
        if let RenderWork::NativeAuthorHandoff(w) = work
            && let Some(c) = &w.continuation
            && matches!(&state.author_batch,Lane::Queued(k) if *k==c.identity)
        {
            state.author_batch = Lane::Retiring(c.identity.clone());
        }
    }
}
pub(in crate::audio_playback) fn drain_unpreserved(
    state: &mut RenderQueueState,
    retired: &mut VecDeque<RenderWork>,
    renderer: Option<mondrian_core::AudioPcmRendererMaterializationId>,
    source: &Arc<mondrian_core::AudioOwnerInstallationSource>,
    policy: PcmRetirementPolicy,
) {
    let keep = match &state.author_batch {
        Lane::Parked(w) => preserve_author_work(w, renderer, source, policy),
        _ => true,
    };
    if !keep {
        drain_parked(state, retired);
    }
}

impl RenderWorkQueue {
    pub(in crate::audio_playback) fn batch_busy(&self) -> bool {
        self.state.lock().author_batch.busy()
    }
    pub(in crate::audio_playback) fn batch_stopped(&self) -> bool {
        self.state.lock().stopped
    }
    pub(in crate::audio_playback) fn batch_poisoned(&self) -> bool {
        matches!(self.state.lock().author_batch, Lane::Poisoned(_))
    }
    fn retire_matching(
        &self,
        identity: &AudioPcmNativeAuthorResumeKey,
        key: &AudioPcmNativeAuthorResumeKey,
    ) -> Option<Box<NativeAuthorHandoffWork>> {
        let mut state = self.state.lock();
        if !matches!(&state.author_batch,Lane::Parked(w) if w.continuation.as_ref().is_some_and(|c| c.identity==*identity && c.key==*key))
        {
            return None;
        }
        let Lane::Parked(work) =
            std::mem::replace(&mut state.author_batch, Lane::Retiring(identity.clone()))
        else {
            unreachable!()
        };
        Some(work)
    }
    fn release_batch(&self, identity: &AudioPcmNativeAuthorResumeKey) {
        let mut state = self.state.lock();
        if matches!(&state.author_batch,Lane::Retiring(k) if k==identity) {
            state.author_batch = Lane::Vacant;
        }
    }
    fn poison_batch(&self, identity: &AudioPcmNativeAuthorResumeKey) {
        let pending = {
            let mut state = self.state.lock();
            state.stopped = true;
            let mut pending = std::mem::take(&mut state.pending);
            drain_parked(&mut state, &mut pending);
            let first_failure = match &state.author_batch {
                Lane::Poisoned(first) => first.clone(),
                _ => identity.clone(),
            };
            state.author_batch = Lane::Poisoned(first_failure);
            self.wake.notify_all();
            pending
        };
        self.retirement_queue.mark_faulted();
        enqueue_retired_render_owners(
            &self.retirement_queue,
            &self.retirement_tracker,
            RetiredRenderOwners { pending, ..Default::default() },
        );
    }
}

/// Pure completion data. A stale/lost event takes only its original parked Box.
#[derive(Clone)]
pub(in crate::audio_playback) struct Completion {
    identity: AudioPcmNativeAuthorResumeKey,
    key: AudioPcmNativeAuthorResumeKey,
    generation: u64,
    renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
    installed: std::num::NonZeroU64,
    proposal: mondrian_core::NativeAudioAuthorProposal,
    status: Option<AudioPcmNativeAuthorTerminalBatchStatus>,
    parked: bool,
    queue: Weak<RenderWorkQueue>,
}
impl Completion {
    pub(in crate::audio_playback) fn retire_parked(self) {
        if let Some(queue) = self.queue.upgrade()
            && let Some(work) = queue.retire_matching(&self.identity, &self.key)
        {
            enqueue_retired_render_owners(
                &queue.retirement_queue,
                &queue.retirement_tracker,
                RetiredRenderOwners {
                    pending: VecDeque::from([RenderWork::NativeAuthorHandoff(work)]),
                    ..Default::default()
                },
            );
        }
    }
}

type Ticket = (Weak<RenderWorkQueue>, AudioPcmNativeAuthorResumeKey);
pub(in crate::audio_playback) fn retirement_ticket(work: &RenderWork) -> Option<Ticket> {
    match work {
        RenderWork::NativeAuthorHandoff(w) => {
            w.continuation.as_ref().map(|c| (c.queue.clone(), c.identity.clone()))
        }
        _ => None,
    }
}
pub(in crate::audio_playback) fn finish_retirement(ticket: Option<Ticket>, success: bool) {
    if let Some((weak, key)) = ticket
        && let Some(queue) = weak.upgrade()
    {
        if success {
            queue.release_batch(&key);
        } else {
            queue.poison_batch(&key);
        }
    }
}

impl AudioPlayback {
    // Even a rejected cold provider follows the original retirement domain.
    pub(in crate::audio_playback) fn handoff_retired_author_request(
        &mut self,
        request: Box<dyn AudioPcmNativeAuthorHandoff>,
    ) {
        enqueue_retired_render_owners(
            &self.render_retirement_queue,
            &self.foreign_owner_retirements,
            RetiredRenderOwners {
                native_author_requests: VecDeque::from([request]),
                ..Default::default()
            },
        );
    }
    pub(in crate::audio_playback) fn request_terminal_author_batch(
        &mut self,
        request: Box<dyn AudioPcmNativeAuthorHandoff>,
    ) -> Result<bool, AudioPlaybackError> {
        let admitted = (|| {
            self.ensure_source_close_idle()?;
            self.ensure_render_execution_available()?;
            // A normally stopped queue retains existing Inline/restart error
            // precedence. Only this new batch admission treats it as unavailable.
            if self.render_queue.batch_stopped() {
                return Err(AudioPlaybackError::ExecutionUnavailable);
            }
            let Some(renderer) = &self.renderer else {
                return Ok(None);
            };
            if self.render_queue.batch_busy()
                || self.native_author_handoff_in_flight.is_some()
                || self.native_author_state_entry_request.is_some()
                || self.native_editor_cleanup_target().is_some()
                || !Arc::ptr_eq(&renderer.owner, request.renderer())
                || matches!(
                    self.native_control_hold,
                    Some(
                        NativeControlHold::Reprepare { .. }
                            | NativeControlHold::AuthorStateEntry { .. }
                    )
                )
            {
                return Ok(None);
            }
            let Some(expected) = self.owner_installation.current_claim()? else {
                return Ok(None);
            };
            if request.expected_target().installed_generation != expected.generation() {
                return Ok(None);
            }
            Ok(Some((renderer.materialization_id, expected)))
        })();
        let (renderer_id, expected) = match admitted {
            Ok(Some(v)) => v,
            other => {
                self.handoff_retired_author_request(request);
                return other.map(|_| false);
            }
        };
        let target = request.expected_target();
        let continuation = Some(Box::new(Continuation::new(
            &self.render_queue,
            request.as_ref(),
        )));
        let work = Box::new(NativeAuthorHandoffWork {
            generation: self.generation,
            renderer_id,
            request,
            source: Arc::clone(&self.owner_installation),
            expected,
            cancellation: self.native_control_cancellation.clone(),
            output_quiescent: false,
            continuation,
        });
        match self.render_queue.try_push(RenderWork::NativeAuthorHandoff(work)) {
            Ok(()) => {
                self.native_author_handoff_in_flight = Some((self.generation, renderer_id));
                self.begin_native_author_hold(target, None);
                Ok(true)
            }
            Err(RenderQueuePushError::Busy(work)) => {
                self.handoff_retired_render_owners(VecDeque::from([work]), None);
                Ok(false)
            }
            Err(RenderQueuePushError::Closed(work)) => {
                self.handoff_retired_render_owners(VecDeque::from([work]), None);
                Err(AudioPlaybackError::ExecutionUnavailable)
            }
        }
    }
    /// Resume only the original parked Work. UI must first publish its genuine
    /// canonical candidate and fill the adapter's host-only snapshot slot.
    /// Busy preserves this key; successful queuing consumes this phase once.
    pub fn resume_native_author_handoff(
        &mut self,
        key: &AudioPcmNativeAuthorResumeKey,
    ) -> Result<bool, AudioPlaybackError> {
        self.ensure_render_execution_available()?;
        self.ensure_source_close_idle()?;
        let claim = self.owner_installation.current_claim()?;
        let renderer = self.renderer.as_ref().map(|r| r.materialization_id);
        let mut state = self.render_queue.state.lock();
        if state.stopped || matches!(state.author_batch, Lane::Poisoned(_)) {
            return Err(AudioPlaybackError::ExecutionUnavailable);
        }
        let Lane::Parked(work) = &state.author_batch else {
            return Err(AudioOwnerInstallationError::Superseded.into());
        };
        let c = work.continuation.as_ref().expect("parked batch");
        if c.key != *key || !c.waiting() {
            return Err(AudioOwnerInstallationError::Superseded.into());
        }
        if Some(work.renderer_id) != renderer
            || !claim
                .as_ref()
                .is_some_and(|current| current.generation() == work.expected.generation())
        {
            let identity = c.identity.clone();
            drop(state);
            if let Some(work) = self.render_queue.retire_matching(&identity, key) {
                self.handoff_retired_render_owners(
                    VecDeque::from([RenderWork::NativeAuthorHandoff(work)]),
                    None,
                );
            }
            return Err(AudioOwnerInstallationError::Superseded.into());
        }
        // The parked credit already reserves its original kind/frontier entry.
        if state.pending.len() >= state.pending.capacity() {
            return Ok(false);
        }
        let identity = c.identity.clone();
        let Lane::Parked(mut work) =
            std::mem::replace(&mut state.author_batch, Lane::Queued(identity))
        else {
            unreachable!()
        };
        let c = work.continuation.as_mut().unwrap();
        c.step = match c.step {
            Step::AwaitCanonical | Step::AwaitInstall => Step::Install,
            Step::AwaitAck => Step::Ack,
            _ => unreachable!(),
        };
        state.pending.push_back(RenderWork::NativeAuthorHandoff(work));
        self.render_queue.wake.notify_one();
        Ok(true)
    }
    pub(in crate::audio_playback) fn accept_terminal_author_completion(
        &mut self,
        c: Completion,
        events: &mut Vec<AudioPlaybackEvent>,
    ) {
        let current = self.renderer.as_ref().is_some_and(|r| r.materialization_id == c.renderer_id)
            && self.native_author_handoff_in_flight == Some((c.generation, c.renderer_id))
            && self
                .owner_installation
                .current_claim()
                .ok()
                .flatten()
                .is_some_and(|claim| claim.generation() == c.installed);
        if !current {
            c.retire_parked();
            return;
        }
        if c.parked {
            let state = self.render_queue.state.lock();
            if !matches!(&state.author_batch,Lane::Parked(w) if w.continuation.as_ref().is_some_and(|b|b.identity==c.identity && b.key==c.key))
            {
                return;
            }
        } else {
            self.native_author_handoff_in_flight = None;
        }
        let Completion { key, renderer_id, proposal, status, .. } = c;
        if let Some(status) = status {
            match status {
                AudioPcmNativeAuthorTerminalBatchStatus::Acknowledged { .. } => {
                    if let Some(NativeControlHold::AuthorPending {
                        first_unrendered_sample, ..
                    }) = self.native_control_hold
                        && self.in_flight == 0
                    {
                        self.native_control_hold = None;
                        if self.generation_render_anchor.is_some() {
                            if let Some(start) = first_unrendered_sample {
                                self.next_start_sample = start;
                            }
                            self.render_blocked = false;
                            if let Some(output) = self.output.snapshot() {
                                self.underrun_baseline_frames = output.underrun_frames;
                                self.last_underrun_frames = output.underrun_frames;
                            }
                        } else {
                            self.render_blocked = true;
                        }
                    }
                }
                AudioPcmNativeAuthorTerminalBatchStatus::InstalledAwaitingAcknowledgement {
                    target,
                } => {
                    if let Some(NativeControlHold::AuthorPending {
                        first_unrendered_sample, ..
                    }) = self.native_control_hold
                    {
                        self.native_control_hold = Some(NativeControlHold::AuthorPending {
                            target,
                            first_unrendered_sample,
                        });
                    }
                }
                AudioPcmNativeAuthorTerminalBatchStatus::Refused(_) => {}
            }
            events.push(AudioPlaybackEvent::NativeAuthorTerminalBatchObserved {
                renderer_id,
                generation: self.generation,
                proposal: Box::new(proposal),
                key,
                status,
            });
        } else {
            events.push(AudioPlaybackEvent::NativeAuthorTerminalBatchPrepared {
                renderer_id,
                generation: self.generation,
                proposal: Box::new(proposal),
                key,
            });
        }
    }
}

fn source_current(work: &NativeAuthorHandoffWork) -> bool {
    work.continuation
        .as_ref()
        .unwrap()
        .renderer
        .owner_installation_verifier()
        .is_some_and(|v| {
            work.source.owns_registration(&v.registration()) && v.validate(&work.expected).is_ok()
        })
}
enum Execution {
    Park(Option<AudioPcmNativeAuthorTerminalBatchStatus>),
    Finish(AudioPcmNativeAuthorTerminalBatchStatus),
}
fn execute(work: &mut NativeAuthorHandoffWork) -> Execution {
    use AudioPcmNativeAuthorHandoffRefusal as R;
    use AudioPcmNativeAuthorTerminalBatchStatus as S;
    if !source_current(work) {
        return Execution::Finish(S::Refused(R::Superseded));
    }
    let step = work.continuation.as_ref().unwrap().step;
    if work.cancellation.is_canceled() {
        return match work.continuation.as_ref().unwrap().target {
            Some(target) => {
                work.continuation.as_mut().unwrap().step = Step::AwaitAck;
                Execution::Park(Some(S::InstalledAwaitingAcknowledgement { target }))
            }
            None => Execution::Finish(S::Refused(R::Cancelled)),
        };
    }
    if step == Step::Prepare {
        let pair = match work.source.register() {
            Ok(v) => v,
            Err(_) => return Execution::Finish(S::Refused(R::Superseded)),
        };
        let result = work.request.prepare_terminal_batch(pair.1.clone(), &work.cancellation);
        work.continuation.as_mut().unwrap().destination = Some(pair);
        return match result {
            Ok(()) if source_current(work) => {
                work.continuation.as_mut().unwrap().step = Step::AwaitCanonical;
                Execution::Park(None)
            }
            Ok(()) => Execution::Finish(S::Refused(R::Superseded)),
            Err(r) => Execution::Finish(S::Refused(r)),
        };
    }
    let result = if step == Step::Install {
        let c = work.continuation.as_mut().unwrap();
        if c.authorization.is_none() {
            let Some((registration, _)) = &c.destination else {
                return Execution::Finish(S::Refused(R::Superseded));
            };
            let auth = work
                .source
                .reserve_install(registration, Some(&work.expected))
                .and_then(|r| work.source.authorize_runtime_installation(r));
            match auth {
                Ok(a) => c.authorization = Some(a),
                Err(_) => return Execution::Finish(S::Refused(R::Superseded)),
            }
        }
        work.request
            .apply_prepared_terminal_batch(c.authorization.as_mut().unwrap(), &work.cancellation)
    } else {
        assert!(step == Step::Ack, "only a resumed private phase executes");
        work.request.acknowledge_terminal_batch(&work.cancellation)
    };
    let (claim, target, acknowledged) = match result {
        AudioPcmNativeAuthorTerminalBatchOutcome::Acknowledged { claim, target } => {
            (claim, target, true)
        }
        AudioPcmNativeAuthorTerminalBatchOutcome::InstalledAwaitingAcknowledgement {
            claim,
            target,
        } => (claim, target, false),
        AudioPcmNativeAuthorTerminalBatchOutcome::Refused(reason) => {
            if !source_current(work) {
                return Execution::Finish(S::Refused(R::Superseded));
            }
            let c = work.continuation.as_mut().unwrap();
            return if let Some(target) = c.target {
                c.step = Step::AwaitAck;
                Execution::Park(Some(S::InstalledAwaitingAcknowledgement { target }))
            } else {
                c.step = Step::AwaitInstall;
                Execution::Park(Some(S::Refused(reason)))
            };
        }
    };
    let actual = work.continuation.as_ref().unwrap().renderer.owner_installation_verifier();
    if !actual.is_some_and(|v| v.validate(&claim).is_ok())
        || target.installed_generation != claim.generation()
        || (step == Step::Install && claim.generation() == work.expected.generation())
        || (step == Step::Ack && claim.generation() != work.expected.generation())
    {
        return Execution::Finish(S::Refused(R::Superseded));
    }
    work.expected = claim;
    let c = work.continuation.as_mut().unwrap();
    c.target = Some(target);
    if acknowledged {
        c.step = Step::Terminal;
        Execution::Finish(S::Acknowledged { target })
    } else {
        c.step = Step::AwaitAck;
        Execution::Park(Some(S::InstalledAwaitingAcknowledgement { target }))
    }
}

pub(in crate::audio_playback) enum RunOutcome {
    Continue,
    Stopped,
    Panicked(bool),
}

pub(in crate::audio_playback) fn run(
    mut work: Box<NativeAuthorHandoffWork>,
    queue: &RenderWorkQueue,
    sender: &mpsc::Sender<RenderCompletion>,
) -> RunOutcome {
    let identity = work.continuation.as_ref().unwrap().identity.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(&mut work)));
    let result = match result {
        Ok(r) => r,
        Err(payload) => {
            queue.poison_batch(&identity);
            enqueue_retired_render_owners(
                &queue.retirement_queue,
                &queue.retirement_tracker,
                RetiredRenderOwners {
                    pending: VecDeque::from([RenderWork::NativeAuthorHandoff(work)]),
                    ..Default::default()
                },
            );
            let abandoned = dispose_canonical_or_abandon_opaque_panic_payload(payload);
            return RunOutcome::Panicked(abandoned);
        }
    };
    let parked = matches!(result, Execution::Park(_));
    let status = match result {
        Execution::Park(s) => s,
        Execution::Finish(s) => Some(s),
    };
    if parked {
        work.continuation.as_mut().unwrap().key = AudioPcmNativeAuthorResumeKey::new();
    }
    let c = work.continuation.as_ref().unwrap();
    let completion = Completion {
        identity: identity.clone(),
        key: c.key.clone(),
        generation: work.generation,
        renderer_id: work.renderer_id,
        installed: work.expected.generation(),
        proposal: c.anchor.clone(),
        status,
        parked,
        queue: c.queue.clone(),
    };
    let acknowledged = matches!(
        status,
        Some(AudioPcmNativeAuthorTerminalBatchStatus::Acknowledged { .. })
    );
    let mut retired = None;
    let sent = {
        let mut state = queue.state.lock();
        if state.stopped {
            if !matches!(state.author_batch, Lane::Poisoned(_)) {
                state.author_batch = Lane::Retiring(identity.clone());
            }
            retired = Some(work);
            false
        } else {
            assert!(
                matches!(&state.author_batch,Lane::Active(k) if *k==identity),
                "the original active credit owns this Work"
            );
            if parked {
                state.author_batch = Lane::Parked(work);
            } else {
                state.author_batch = if acknowledged {
                    Lane::Vacant
                } else {
                    Lane::Retiring(identity.clone())
                };
                retired = Some(work);
            }
            match sender.send(RenderCompletion::NativeAuthorTerminalBatch(completion)) {
                Ok(()) => true,
                Err(_) => {
                    if let Lane::Parked(_) = &state.author_batch {
                        let Lane::Parked(w) = std::mem::replace(
                            &mut state.author_batch,
                            Lane::Retiring(identity.clone()),
                        ) else {
                            unreachable!()
                        };
                        retired = Some(w);
                    }
                    state.stopped = true;
                    queue.wake.notify_all();
                    false
                }
            }
        }
    };
    if let Some(work) = retired {
        enqueue_retired_render_owners(
            &queue.retirement_queue,
            &queue.retirement_tracker,
            RetiredRenderOwners {
                pending: VecDeque::from([RenderWork::NativeAuthorHandoff(work)]),
                ..Default::default()
            },
        );
    }
    if sent {
        RunOutcome::Continue
    } else {
        RunOutcome::Stopped
    }
}
