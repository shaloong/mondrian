//! Native author Plan installation on the existing serial PCM execution queue.
//! The App adapter owns canonical immutable input; Media owns the actual source
//! slot/current claim and its one sealed installation authorization.

use super::*;

/// Exact refusal keeps the actual retained source delivery outstanding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPcmNativeAuthorHandoffRefusal {
    /// No same-object author-plan adapter exists.
    UnsupportedAdapter,
    /// Current renderer, native owner, canonical snapshot or proposal changed.
    Superseded,
    /// Work was cancelled before its next admission boundary.
    Cancelled,
    /// Original resource grant cannot admit complete known ownership overlap.
    ResourceAdmission,
    /// Native producer/processor did not accept the complete operation.
    ProcessorRejected,
    /// Exact source or peer state remains unaccepted; preserve the retained child.
    AuthorStatePending,
    /// Existing serial queue cannot accept this operation.
    WorkerUnavailable,
}

/// Actual DSP consequence of this author operation, not transport permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPcmNativeAuthorHandoffContinuity {
    /// The same live Session may Continue after successful Ack.
    Preserved,
    /// Real output must retire before preparation; fresh Entry/preroll follows.
    StateEntryRequired,
}

/// Closed operation on the exact retained author delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPcmNativeAuthorHandoffIntent {
    /// Prepare and install a newly accepted canonical parameter Plan.
    Install,
    /// Retry only native acknowledgement of an already installed Plan.
    Acknowledge,
}

/// A typed immutable handoff request from the concrete author/execution adapter.
///
/// This is not a closure, editor-only instance or alternate author manager.
/// `renderer` returns the exact captured Arc, never a lookup or lazy replacement.
/// Media compares it with its actual current source before invoking the provider.
/// The provider must prove exact private retained delivery, all shared Scope
/// materializations, canonical generation, actual owners and original grant.
pub trait AudioPcmNativeAuthorHandoff: Send + Sync + 'static {
    /// Actual captured renderer owner; metadata alone cannot substitute for it.
    fn renderer(&self) -> &Arc<dyn AudioPcmRenderer>;
    /// Shared exact source proposal accepted by the canonical author adapter.
    fn proposal(&self) -> &mondrian_core::NativeAudioAuthorProposal;
    /// Current installed target at queuing; public facts are revalidated by the
    /// actual private Hosted outstanding delivery before any native work.
    fn expected_target(&self) -> mondrian_core::NativeAudioEditorTarget;
    /// An Ack-only retry never repeats author or Source installation.
    fn intent(&self) -> AudioPcmNativeAuthorHandoffIntent;
    /// Query the actual retained Runtime on the existing worker, before any
    /// destructive SDK work. Public UI target facts cannot choose this policy.
    fn continuity_policy(
        &self,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<AudioPcmNativeAuthorHandoffContinuity, AudioPcmNativeAuthorHandoffRefusal>;
    /// Retry only the same installed private delivery, outside final guards.
    fn acknowledge(
        &self,
        cancellation: &ExecutionCancellationToken,
    ) -> AudioPcmNativeAuthorHandoffOutcome;
    /// Prepare foreign/resource work then perform one bounded same-source swap.
    ///
    /// Allocation, SDK/provider calls and retirement happen outside final author
    /// and Source guards. Only actual infallible Plan/scratch/record moves belong
    /// inside them. Native Ack follows successful installation and keeps later
    /// callbacks intact. Refusal never silently consumes the retained delivery.
    fn apply(
        &self,
        destination: AudioOwnerInstallationVerifier,
        authorization: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
        cancellation: &ExecutionCancellationToken,
    ) -> AudioPcmNativeAuthorHandoffOutcome;
}

/// Historical execution result, distinct from current-author permission.
pub enum AudioPcmNativeAuthorHandoffOutcome {
    /// Actual Plan install and exact same-object source acknowledgement completed.
    Acknowledged {
        /// Current claim from the same Media Source final CAS.
        claim: AudioOwnerInstallationClaim,
        /// Actual newly installed editor target, never an old public address.
        target: Box<mondrian_core::NativeAudioEditorTarget>,
        /// Exact installed execution consequence, never inferred from GUI metadata.
        continuity: AudioPcmNativeAuthorHandoffContinuity,
    },
    /// The canonical Plan was installed but native acknowledgement requires
    /// retry. Preserve this current claim; do not repeat the author transaction.
    InstalledAwaitingAcknowledgement {
        /// Actual current claim published by the same source final CAS.
        claim: AudioOwnerInstallationClaim,
        /// Exact newly installed target that still owns the retained batch.
        target: Box<mondrian_core::NativeAudioEditorTarget>,
        /// Exact installed execution consequence, never inferred from GUI metadata.
        continuity: AudioPcmNativeAuthorHandoffContinuity,
    },
    /// No successful complete acknowledgement is asserted.
    Refused(AudioPcmNativeAuthorHandoffRefusal),
}

/// Product result after same renderer/generation/new claim admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioPcmNativeAuthorHandoffStatus {
    /// The actual retained batch reached canonical Plan plus native acknowledgement.
    Acknowledged {
        target: Box<mondrian_core::NativeAudioEditorTarget>,
        /// Exact installed execution consequence, never inferred from GUI metadata.
        continuity: AudioPcmNativeAuthorHandoffContinuity,
    },
    /// Canonical installation completed; only exact native Ack remains.
    InstalledAwaitingAcknowledgement {
        target: Box<mondrian_core::NativeAudioEditorTarget>,
        /// Exact installed execution consequence, never inferred from GUI metadata.
        continuity: AudioPcmNativeAuthorHandoffContinuity,
    },
    /// Preserve the exact native and canonical pending-author barrier.
    Refused(AudioPcmNativeAuthorHandoffRefusal),
}

pub(super) struct NativeAuthorHandoffWork {
    pub(super) generation: u64,
    pub(super) renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
    pub(super) request: Box<dyn AudioPcmNativeAuthorHandoff>,
    pub(super) source: Arc<AudioOwnerInstallationSource>,
    pub(super) expected: AudioOwnerInstallationClaim,
    pub(super) cancellation: ExecutionCancellationToken,
    pub(super) output_quiescent: bool,
}

pub(super) struct NativeAuthorHandoffCompletion {
    generation: u64,
    renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
    proposal: mondrian_core::NativeAudioAuthorProposal,
    result: NativeAuthorHandoffCompletionResult,
}

enum NativeAuthorHandoffCompletionResult {
    Finished(AudioPcmNativeAuthorHandoffOutcome),
    AwaitingOutputQuiescence(Box<NativeAuthorHandoffWork>),
}
pub(super) enum NativeAuthorHandoffExecution {
    Finished(AudioPcmNativeAuthorHandoffOutcome),
    AwaitingOutputQuiescence,
}

impl NativeAuthorHandoffCompletion {
    pub(super) fn into_retired_work(self) -> Option<Box<NativeAuthorHandoffWork>> {
        let Self { generation, renderer_id, proposal, result } = self;
        // These are Core-owned facts. Only Awaiting retains the actual request
        // provider and its original installation Source; never invoke it here.
        drop((generation, renderer_id, proposal));
        match result {
            NativeAuthorHandoffCompletionResult::Finished(outcome) => {
                drop(outcome);
                None
            }
            NativeAuthorHandoffCompletionResult::AwaitingOutputQuiescence(work) => Some(work),
        }
    }
}

impl AudioPlayback {
    pub(super) fn begin_native_author_hold(
        &mut self,
        target: mondrian_core::NativeAudioEditorTarget,
        first_unrendered_sample: Option<i64>,
    ) {
        match self.native_control_hold {
            Some(NativeControlHold::AuthorPending {
                target: current,
                first_unrendered_sample: previous,
            }) if current == target => {
                let first = match (previous, first_unrendered_sample) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, None) => a,
                    (None, b) => b,
                };
                self.native_control_hold = Some(NativeControlHold::AuthorPending {
                    target,
                    first_unrendered_sample: first,
                });
            }
            None => {
                self.native_control_hold =
                    Some(NativeControlHold::AuthorPending { target, first_unrendered_sample })
            }
            _ => {} // Cleanup/reprepare owns its stricter lifecycle; never erase it.
        }
        self.render_blocked = self.native_control_hold.is_some();
    }

    /// Queue one typed same-source author Plan request without touching DSP.
    /// UI queuing grants neither installed owner nor author acknowledgement.
    pub fn request_native_author_handoff(
        &mut self,
        request: Box<dyn AudioPcmNativeAuthorHandoff>,
    ) -> Result<bool, AudioPlaybackError> {
        self.ensure_render_execution_available()?;
        let Some(renderer) = &self.renderer else {
            return Ok(false);
        };
        if self.native_editor_cleanup_target().is_some()
            || !Arc::ptr_eq(&renderer.owner, request.renderer())
            || self.native_author_handoff_in_flight.is_some()
            || self.native_author_state_entry_request.is_some()
        {
            return Ok(false);
        }
        let Some(expected) = self.owner_installation.current_claim()? else {
            return Ok(false);
        };
        if request.expected_target().installed_generation != expected.generation() {
            return Ok(false);
        }
        let request_target = request.expected_target();
        let identity = (self.generation, renderer.materialization_id);
        let work = RenderWork::NativeAuthorHandoff(Box::new(NativeAuthorHandoffWork {
            generation: self.generation,
            renderer_id: renderer.materialization_id,
            request,
            source: Arc::clone(&self.owner_installation),
            expected,
            cancellation: self.native_control_cancellation.clone(),
            output_quiescent: matches!(self.native_control_hold,Some(NativeControlHold::AuthorStateEntry {
                target,phase:AuthorStateEntryPhase::OutputRetired {..}
                    | AuthorStateEntryPhase::InstalledAwaitingAck {..},
            }) if target==request_target),
        }));
        match self.render_queue.push(work) {
            Ok(()) => {
                self.native_author_handoff_in_flight = Some(identity);
                // Block further PCM admission before any work can be queued
                // behind this handoff. Earlier PCM settles on the same worker.
                self.begin_native_author_hold(request_target, None);
                Ok(true)
            }
            Err(work) => {
                self.handoff_retired_render_owners(VecDeque::from([work]), None);
                Ok(false)
            }
        }
    }

    pub(super) fn accept_native_author_handoff_completion(
        &mut self,
        completion: NativeAuthorHandoffCompletion,
        events: &mut Vec<AudioPlaybackEvent>,
    ) {
        let identity = (completion.generation, completion.renderer_id);
        if self.native_author_handoff_in_flight != Some(identity) {
            return;
        }
        self.native_author_handoff_in_flight = None;
        if !self
            .renderer
            .as_ref()
            .is_some_and(|renderer| renderer.materialization_id == completion.renderer_id)
        {
            return;
        }
        let outcome = match completion.result {
            NativeAuthorHandoffCompletionResult::AwaitingOutputQuiescence(work) => {
                self.retain_state_entry_request(work);
                return;
            }
            NativeAuthorHandoffCompletionResult::Finished(outcome) => outcome,
        };
        let status = match outcome {
            AudioPcmNativeAuthorHandoffOutcome::Acknowledged { claim, target, continuity }
                if self
                    .owner_installation
                    .current_claim()
                    .ok()
                    .flatten()
                    .is_some_and(|current| current.generation() == claim.generation())
                    && target.installed_generation == claim.generation() =>
            {
                AudioPcmNativeAuthorHandoffStatus::Acknowledged { target, continuity }
            }
            AudioPcmNativeAuthorHandoffOutcome::InstalledAwaitingAcknowledgement {
                claim,
                target,
                continuity,
            } if self
                .owner_installation
                .current_claim()
                .ok()
                .flatten()
                .is_some_and(|current| current.generation() == claim.generation())
                && target.installed_generation == claim.generation() =>
            {
                AudioPcmNativeAuthorHandoffStatus::InstalledAwaitingAcknowledgement {
                    target,
                    continuity,
                }
            }
            AudioPcmNativeAuthorHandoffOutcome::InstalledAwaitingAcknowledgement { .. }
            | AudioPcmNativeAuthorHandoffOutcome::Acknowledged { .. } => {
                AudioPcmNativeAuthorHandoffStatus::Refused(
                    AudioPcmNativeAuthorHandoffRefusal::Superseded,
                )
            }
            AudioPcmNativeAuthorHandoffOutcome::Refused(reason) => {
                AudioPcmNativeAuthorHandoffStatus::Refused(reason)
            }
        };
        match &status {
            AudioPcmNativeAuthorHandoffStatus::Acknowledged { target, continuity } => {
                if *continuity == AudioPcmNativeAuthorHandoffContinuity::StateEntryRequired {
                    self.publish_author_state_entry_install(**target, true);
                } else {
                    // Queue order proves all pre-handoff PCM has settled before
                    // this exact Ack. Rewind scheduling only; Session owns DSP time.
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
                            // Actual cumulative underruns remain reported. The resumed
                            // admission starts a new recovery interval without Enter.
                            if let Some(output) = self.output.snapshot() {
                                self.underrun_baseline_frames = output.underrun_frames;
                                self.last_underrun_frames = output.underrun_frames;
                            }
                        } else {
                            // The same-child Ack completes its real control, but a
                            // suspended PCM generation has no continuation to resume.
                            self.render_blocked = true;
                        }
                    } else if matches!(
                        self.native_control_hold,
                        Some(NativeControlHold::AuthorPending { .. })
                    ) {
                        // Retain truthful control hold if serial order is violated;
                        // never fake continuation or cancel already admitted DSP.
                        self.render_blocked = true;
                    }
                    let _ = target;
                }
            }
            AudioPcmNativeAuthorHandoffStatus::InstalledAwaitingAcknowledgement {
                target,
                continuity,
            } => {
                if *continuity == AudioPcmNativeAuthorHandoffContinuity::StateEntryRequired {
                    self.publish_author_state_entry_install(**target, false);
                } else {
                    if let Some(NativeControlHold::AuthorPending {
                        first_unrendered_sample, ..
                    }) = self.native_control_hold
                    {
                        self.native_control_hold = Some(NativeControlHold::AuthorPending {
                            target: **target,
                            first_unrendered_sample,
                        });
                    }
                }
            }
            AudioPcmNativeAuthorHandoffStatus::Refused(_) => {}
        }
        events.push(AudioPlaybackEvent::NativeAuthorHandoffObserved {
            renderer_id: completion.renderer_id,
            generation: self.generation,
            proposal: Box::new(completion.proposal),
            status,
        });
    }
}

pub(super) fn execute_native_author_handoff(
    work: &NativeAuthorHandoffWork,
) -> NativeAuthorHandoffExecution {
    use AudioPcmNativeAuthorHandoffOutcome as Outcome;
    use AudioPcmNativeAuthorHandoffRefusal as Refusal;
    if work.cancellation.is_canceled() {
        return NativeAuthorHandoffExecution::Finished(Outcome::Refused(Refusal::Cancelled));
    }
    let Some(current) = work.request.renderer().owner_installation_verifier() else {
        return NativeAuthorHandoffExecution::Finished(Outcome::Refused(
            Refusal::UnsupportedAdapter,
        ));
    };
    if current.validate(&work.expected).is_err() {
        return NativeAuthorHandoffExecution::Finished(Outcome::Refused(Refusal::Superseded));
    }
    if !work.output_quiescent && work.request.intent() == AudioPcmNativeAuthorHandoffIntent::Install
    {
        match work.request.continuity_policy(&work.cancellation) {
            Ok(AudioPcmNativeAuthorHandoffContinuity::StateEntryRequired) => {
                return NativeAuthorHandoffExecution::AwaitingOutputQuiescence
            }
            Ok(AudioPcmNativeAuthorHandoffContinuity::Preserved) => {}
            Err(refusal) => {
                return NativeAuthorHandoffExecution::Finished(Outcome::Refused(refusal))
            }
        }
    }
    NativeAuthorHandoffExecution::Finished(execute_admitted(work))
}

pub(super) fn complete_native_author_handoff(
    work: Box<NativeAuthorHandoffWork>,
    execution: NativeAuthorHandoffExecution,
    queue: &RenderWorkQueue,
) -> NativeAuthorHandoffCompletion {
    let generation = work.generation;
    let renderer_id = work.renderer_id;
    let proposal = work.request.proposal().clone();
    let result = match execution {
        NativeAuthorHandoffExecution::AwaitingOutputQuiescence => {
            NativeAuthorHandoffCompletionResult::AwaitingOutputQuiescence(work)
        }
        NativeAuthorHandoffExecution::Finished(outcome) => {
            enqueue_retired_render_owners(
                &queue.retirement_queue,
                &queue.retirement_tracker,
                RetiredRenderOwners {
                    pending: VecDeque::from([RenderWork::NativeAuthorHandoff(work)]),
                    renderers: VecDeque::new(),
                    errors: VecDeque::new(),
                },
            );
            NativeAuthorHandoffCompletionResult::Finished(outcome)
        }
    };
    NativeAuthorHandoffCompletion { generation, renderer_id, proposal, result }
}

fn execute_admitted(work: &NativeAuthorHandoffWork) -> AudioPcmNativeAuthorHandoffOutcome {
    use AudioPcmNativeAuthorHandoffOutcome as Outcome;
    use AudioPcmNativeAuthorHandoffRefusal as Refusal;
    let Some(current) = work.request.renderer().owner_installation_verifier() else {
        return Outcome::Refused(Refusal::UnsupportedAdapter);
    };
    if current.validate(&work.expected).is_err() {
        return Outcome::Refused(Refusal::Superseded);
    }
    if work.request.intent() == AudioPcmNativeAuthorHandoffIntent::Acknowledge {
        return work.request.acknowledge(&work.cancellation);
    }
    let (registration, destination) = match work.source.register() {
        Ok(value) => value,
        Err(_) => return Outcome::Refused(Refusal::Superseded),
    };
    let reservation = match work.source.reserve_install(&registration, Some(&work.expected)) {
        Ok(value) => value,
        Err(_) => return Outcome::Refused(Refusal::Superseded),
    };
    let mut authorization = match work.source.authorize_runtime_installation(reservation) {
        Ok(value) => value,
        Err(_) => return Outcome::Refused(Refusal::Superseded),
    };
    if work.cancellation.is_canceled() {
        return Outcome::Refused(Refusal::Cancelled);
    }
    work.request.apply(destination, &mut authorization, &work.cancellation)
}
