//! Canonical custody for one finite headless terminal frontier. Native prepared
//! storage never crosses this UI boundary; Media retains the original Work.

use super::*;
use crate::app::audio_rendering::TimelineAudioPcmRenderer;
use crate::app::playback::AppAudioPlayback;
use mondrian_core::{AudioPcmRendererMaterializationId, NativeAudioAuthorProposal};
use mondrian_editor_state::{
    AuthorExecutionSnapshot, AuthoringSession, NativeAudioTerminalAuthorGroup,
    NativeAudioTerminalAuthorMember, PreparedNativeAudioTerminalAuthorGroup,
};
use mondrian_media::{AudioPcmNativeAuthorResumeKey, AudioPcmNativeAuthorTerminalBatchStatus};
use std::sync::{Arc, Weak};

#[cfg(test)]
mod tests;

struct Candidate {
    ticket: PreparedNativeAudioTerminalAuthorGroup,
    programs: Option<Arc<[mondrian_audio::NativeAudioAuthorProgram]>>,
    publication: Arc<parking_lot::Mutex<Option<Arc<AuthorExecutionSnapshot>>>>,
    submitted: usize,
    admission: Option<AudioPcmNativeAuthorResumeKey>,
    prepared_key: Option<AudioPcmNativeAuthorResumeKey>,
}

enum Phase {
    // A failed unchanged frontier is not prepared on every diagnostic reoffer.
    Collecting {
        attempted_members: usize,
    },
    Preparing(Candidate),
    Published {
        key: AudioPcmNativeAuthorResumeKey,
        queued: bool,
    },
    // Latch actual all-member Ack before fallible canonical History completion.
    AcknowledgedAwaitingFinish,
    // Nonparked failure: accepted document/barrier remain, no Work can resume.
    PublishedWorkUnavailable,
    Finished,
}

pub(in crate::app) struct TerminalAuthorWork {
    group: NativeAudioTerminalAuthorGroup,
    renderer: Weak<TimelineAudioPcmRenderer>,
    renderer_id: AudioPcmRendererMaterializationId,
    members: Vec<NativeAudioAuthorProposal>,
    phase: Phase,
}

impl TerminalAuthorWork {
    pub(super) fn blocks_source_replacement(&self) -> bool {
        !matches!(self.phase, Phase::Finished)
    }

    pub(super) fn snapshot_current_for_source(
        &self,
        source: &Weak<TimelineAudioPcmRenderer>,
    ) -> bool {
        matches!(self.phase, Phase::Finished)
            && Weak::ptr_eq(source, &self.renderer)
            && self.renderer.upgrade().is_some_and(|r| r.native_author_snapshot_current())
    }

    fn matches(
        &self,
        renderer: AudioPcmRendererMaterializationId,
        proposal: &NativeAudioAuthorProposal,
    ) -> bool {
        renderer == self.renderer_id
            && self.members.first().is_some_and(|p| p.same_retained_proposal(proposal))
    }
}

fn member(
    author: &AuthoringSession,
    proposal: &NativeAudioAuthorProposal,
) -> Result<NativeAudioTerminalAuthorMember> {
    let target = proposal.target();
    if !proposal.terminal()
        || target.root_plan != target.local_plan
        || target.root_session != target.local_session
    {
        return Err(failure(
            "headless author requires a terminal direct-root prefix",
        ));
    }
    let rack = match target.insertion {
        Insertion::PreFader => AudioChannelStripRack::PreFader,
        Insertion::PostFader => AudioChannelStripRack::PostFader,
        Insertion::Scope => return Err(failure("headless Scope authoring is unsupported")),
    };
    let channel = match target.owner {
        Owner::Track(track_id) => AudioChannelStripOwner::Track { track_id },
        Owner::Bus(bus_id) => AudioChannelStripOwner::Bus { bus_id },
        Owner::Output(output_id) => AudioChannelStripOwner::ProgramOutput { output_id },
        Owner::Contribution { .. } => {
            return Err(failure("headless contribution authoring is unsupported"))
        }
    };
    let owner = NativeAudioAuthorOwner::ChannelStrip { owner: channel, rack };
    let sequence = author
        .sequence(proposal.sequence())
        .ok_or_else(|| failure("native author Sequence disappeared"))?;
    let address = owner.rack_address();
    let processor = mondrian_timeline::audio_processor_rack(sequence, &address)
        .map_err(failure)?
        .processors
        .iter()
        .find(|p| p.id == target.instance)
        .ok_or_else(|| failure("native author processor disappeared"))?;
    let processor =
        inspect_native_audio_authoring(sequence, &address, target.instance, &processor.definition)
            .map_err(failure)?;
    Ok(NativeAudioTerminalAuthorMember {
        owner,
        processor_id: target.instance,
        expected_definition: processor.definition.clone(),
        update: author_projection::project_update(
            sequence, owner, processor, None, proposal, None,
        )?,
        completion: proposal.completion(),
    })
}

impl AppState {
    /// Preserve the actual Media provenance at the first admission. The legacy
    /// GUI path still owns its original surface/time/schema semantics.
    pub(in crate::app) fn observe_native_audio_author_delivery(
        &mut self,
        renderer_id: AudioPcmRendererMaterializationId,
        generation: u64,
        proposal: NativeAudioAuthorProposal,
    ) -> Result<()> {
        if matches!(&self.native_audio_author_work, Some(NativeAudioAuthorWork::Terminal(work))
            if matches!(work.phase, Phase::Finished) && work.members.iter().any(|p| p.same_retained_proposal(&proposal)))
        {
            return Ok(());
        }
        let terminal = matches!(self.native_audio_author_work, Some(NativeAudioAuthorWork::Terminal(ref w)) if w.blocks_source_replacement());
        let legacy_busy = matches!(self.native_audio_author_work, Some(NativeAudioAuthorWork::Single(ref w)) if w.blocks_source_replacement());
        if !terminal && (self.native_audio_editor.is_some() || legacy_busy || !proposal.terminal())
        {
            return self.observe_native_audio_author_proposal(proposal);
        }
        if generation != self.audio_playback_snapshot().generation {
            return Ok(());
        }
        let author =
            self.authoring.as_ref().ok_or_else(|| failure("native author Session closed"))?;
        if self.active_sequence().is_none_or(|s| s.id != proposal.sequence()) {
            return Err(failure("native author active Sequence changed"));
        }
        if terminal {
            let Some(NativeAudioAuthorWork::Terminal(work)) = &mut self.native_audio_author_work
            else {
                unreachable!()
            };
            if work.group.session_id() != author.session_id()
                || work.group.sequence_id() != proposal.sequence()
                || work.renderer_id != renderer_id
                || !self
                    .native_audio_renderer
                    .as_ref()
                    .is_some_and(|r| Weak::ptr_eq(r, &work.renderer))
            {
                return Err(failure(
                    "terminal author delivery belongs to another Source",
                ));
            }
            if matches!(
                work.phase,
                Phase::Published { .. }
                    | Phase::AcknowledgedAwaitingFinish
                    | Phase::PublishedWorkUnavailable
            ) {
                // An already acknowledged anchor is correlation only. Late
                // suffixes remain outstanding at the original Runtime.
                return self.retry_native_audio_terminal_author();
            }
            if !work.members.iter().any(|p| p.same_retained_proposal(&proposal)) {
                let anchor = work.members[0].target();
                let target = proposal.target();
                if anchor.root_plan != target.root_plan
                    || anchor.root_session != target.root_session
                    || anchor.installed_generation != target.installed_generation
                {
                    return Err(failure(
                        "terminal author peer belongs to another installed frontier",
                    ));
                }
                if work.members.iter().any(|p| p.target() == proposal.target()) {
                    return Err(failure(
                        "terminal author prefix changed before acknowledgement",
                    ));
                }
                member(author, &proposal)?;
                work.members.push(proposal);
            }
        } else {
            // Pure canonical admission. Audio checks the actual native pin,
            // retained objects/frontier and Preserved policy before publication.
            let first = member(author, &proposal)?;
            let renderer = self
                .native_audio_renderer
                .as_ref()
                .ok_or_else(|| failure("native author captured Source retired"))?
                .clone();
            // Match the actual slot before any upgrade; a stale Weak probe
            // must never make the UI the last native renderer owner.
            let actual: Weak<dyn mondrian_media::AudioPcmRenderer> = renderer.clone();
            if !self.audio_playback.matches_source_weak(&actual) {
                return Err(failure(
                    "native author captured renderer is not the Media Source",
                ));
            }
            let author = self.authoring.as_mut().expect("admitted Session");
            let group = author.begin_native_audio_terminal_group(proposal.sequence(), first)?;
            // Retain the real group before compilation or queue admission can fail.
            self.native_audio_author_work = Some(NativeAudioAuthorWork::Terminal(Box::new(
                TerminalAuthorWork {
                    group,
                    renderer,
                    renderer_id,
                    members: vec![proposal],
                    phase: Phase::Collecting { attempted_members: 0 },
                },
            )));
        }
        self.retry_native_audio_terminal_author()
    }

    /// Retry only a retained host candidate or the original Media resume key.
    /// No post-publication path builds another request or author candidate.
    pub(in crate::app) fn retry_native_audio_terminal_author(&mut self) -> Result<()> {
        let Some(NativeAudioAuthorWork::Terminal(work)) = &mut self.native_audio_author_work else {
            return Ok(());
        };
        if matches!(work.phase, Phase::Finished) {
            return Ok(());
        }
        let author =
            self.authoring.as_mut().ok_or_else(|| failure("native author Session closed"))?;
        let actual: Weak<dyn mondrian_media::AudioPcmRenderer> = work.renderer.clone();
        if author.session_id() != work.group.session_id()
            || !self
                .native_audio_renderer
                .as_ref()
                .is_some_and(|r| Weak::ptr_eq(r, &work.renderer))
            || !self.audio_playback.matches_source_weak(&actual)
        {
            return Err(failure("terminal author Source or Session changed"));
        }
        let renderer = work
            .renderer
            .upgrade()
            .ok_or_else(|| failure("captured native renderer retired"))?;
        if let Phase::Collecting { attempted_members } = work.phase {
            if work.members.len() <= attempted_members {
                return Ok(());
            }
            let members =
                work.members.iter().map(|p| member(author, p)).collect::<Result<Vec<_>>>()?;
            let ticket = author.prepare_native_audio_terminal_group(&mut work.group, members)?;
            work.phase = Phase::Preparing(Candidate {
                ticket,
                programs: None,
                publication: Arc::new(parking_lot::Mutex::new(None)),
                submitted: work.members.len(),
                admission: None,
                prepared_key: None,
            });
        }
        let AppAudioPlayback::Available(playback) = &mut self.audio_playback else {
            return Err(failure("native author Media worker is unavailable"));
        };
        match &mut work.phase {
            Phase::Preparing(candidate) => {
                if candidate.prepared_key.is_some() {
                    return self.publish_native_audio_terminal_candidate();
                }
                if candidate.admission.is_some() {
                    return Ok(());
                }
                if candidate.programs.is_none() {
                    let sequence = candidate
                        .ticket
                        .candidate_sequence()
                        .ok_or_else(|| failure("native author candidate was consumed"))?;
                    let mut programs = Vec::with_capacity(sequence.audio_program.outputs.len());
                    for output in &sequence.audio_program.outputs {
                        programs.push(mondrian_audio::NativeAudioAuthorProgram {
                            sequence: sequence.id,
                            program: Arc::new(
                                mondrian_audio::compile_audio_program(
                                    sequence,
                                    renderer.native_author_compile_request(output.id),
                                )
                                .map_err(failure)?,
                            ),
                        });
                    }
                    candidate.programs = Some(programs.into());
                }
                // Construct a foreign-owning provider only after the concrete
                // Media owner is available; rejected admission retires it there.
                let request = renderer.native_author_terminal_batch_request(
                    work.members[0].clone(),
                    work.members[..candidate.submitted].to_vec().into(),
                    candidate.programs.as_ref().expect("compiled candidate").clone(),
                    candidate.publication.clone(),
                );
                candidate.admission =
                    playback.request_native_author_terminal_batch(request).map_err(failure)?;
            }
            Phase::Published { key, queued } => {
                if !*queued {
                    *queued = playback.resume_native_author_handoff(key).map_err(failure)?;
                }
            }
            Phase::AcknowledgedAwaitingFinish => {
                author.finish_native_audio_terminal_group(&mut work.group)?;
                work.phase = Phase::Finished;
                self.audio_playback_author_binding =
                    self.current_audio_playback_author_binding()?;
            }
            Phase::Collecting { .. } | Phase::PublishedWorkUnavailable | Phase::Finished => {}
        }
        Ok(())
    }

    fn publish_native_audio_terminal_candidate(&mut self) -> Result<()> {
        let Some(NativeAudioAuthorWork::Terminal(work)) = &mut self.native_audio_author_work else {
            return Ok(());
        };
        let Phase::Preparing(candidate) = &mut work.phase else {
            return Ok(());
        };
        let Some(key) = candidate.prepared_key.clone() else {
            return Ok(());
        };
        let actual: Weak<dyn mondrian_media::AudioPcmRenderer> = work.renderer.clone();
        if !self
            .native_audio_renderer
            .as_ref()
            .is_some_and(|r| Weak::ptr_eq(r, &work.renderer))
            || !self.audio_playback.matches_source_weak(&actual)
        {
            return Err(failure("prepared native author Source changed"));
        }
        let author =
            self.authoring.as_mut().ok_or_else(|| failure("native author Session closed"))?;
        let commit =
            author.commit_native_audio_terminal_group(&mut work.group, &mut candidate.ticket)?;
        *candidate.publication.lock() = Some(Arc::new(author.execution_snapshot()));
        // Canonical publication already happened. Any later failure retains
        // this phase/group and resumes only the original producer-owned Work.
        work.phase = Phase::Published { key, queued: false };
        if let Some(commit) = commit {
            self.consume_native_audio_authoring_commit(commit)?;
        }
        self.retry_native_audio_terminal_author()
    }

    pub(in crate::app) fn observe_native_audio_terminal_prepared(
        &mut self,
        renderer_id: AudioPcmRendererMaterializationId,
        generation: u64,
        proposal: NativeAudioAuthorProposal,
        key: AudioPcmNativeAuthorResumeKey,
    ) -> Result<()> {
        let Some(NativeAudioAuthorWork::Terminal(work)) = &mut self.native_audio_author_work else {
            return Ok(());
        };
        if !work.matches(renderer_id, &proposal) {
            return Ok(());
        }
        // A legal Suspend rotates PCM generation while preserving this Source
        // and Work. Generation is provenance; the actual key/slot admits resume.
        tracing::debug!(
            ?renderer_id,
            generation,
            "terminal native preparation observed"
        );
        if let Phase::Preparing(candidate) = &mut work.phase {
            if !candidate.admission.as_ref().is_some_and(|admitted| key.continues(admitted)) {
                return Ok(());
            }
            if candidate.prepared_key.as_ref().is_some_and(|k| k != &key) {
                return Ok(());
            }
            candidate.prepared_key = Some(key);
            self.publish_native_audio_terminal_candidate()?;
        }
        Ok(())
    }

    pub(in crate::app) fn observe_native_audio_terminal_status(
        &mut self,
        renderer_id: AudioPcmRendererMaterializationId,
        generation: u64,
        proposal: NativeAudioAuthorProposal,
        key: AudioPcmNativeAuthorResumeKey,
        status: AudioPcmNativeAuthorTerminalBatchStatus,
    ) -> Result<bool> {
        let Some(NativeAudioAuthorWork::Terminal(work)) = &mut self.native_audio_author_work else {
            return Ok(false);
        };
        if !work.matches(renderer_id, &proposal) {
            return Ok(false);
        }
        tracing::debug!(
            ?renderer_id,
            generation,
            ?status,
            "terminal native result observed"
        );
        let actual: Weak<dyn mondrian_media::AudioPcmRenderer> = work.renderer.clone();
        if self.authoring.as_ref().map(AuthoringSession::session_id)
            != Some(work.group.session_id())
            || !self.audio_playback.matches_source_weak(&actual)
        {
            return Err(failure(
                "terminal author completion no longer has its Source or Session",
            ));
        }
        let previous = match &work.phase {
            Phase::Preparing(c) if c.prepared_key.is_none() => c.admission.as_ref(),
            Phase::Published { key, queued: true } => Some(key),
            _ => None,
        };
        let Some(previous) = previous else {
            return Ok(false);
        };
        let parked = key.continues(previous);
        if key != *previous && !parked {
            return Ok(false);
        }
        match (&mut work.phase, status) {
            (
                Phase::Preparing(candidate),
                AudioPcmNativeAuthorTerminalBatchStatus::Refused(reason),
            ) if !parked => {
                // No canonical publication occurred. This completed Prepare
                // cannot resume; Media retains retirement custody and credit.
                let author = self
                    .authoring
                    .as_mut()
                    .ok_or_else(|| failure("native author Session closed"))?;
                author.discard_native_audio_terminal_candidate(
                    &mut work.group,
                    &mut candidate.ticket,
                )?;
                let attempted_members = candidate.submitted;
                work.phase = Phase::Collecting { attempted_members };
                tracing::debug!(
                    ?reason,
                    "terminal native frontier awaits additional real members"
                );
                self.retry_native_audio_terminal_author()?;
            }
            (
                Phase::Published { .. },
                AudioPcmNativeAuthorTerminalBatchStatus::Acknowledged { .. },
            ) if !parked => {
                work.phase = Phase::AcknowledgedAwaitingFinish;
                let author = self
                    .authoring
                    .as_mut()
                    .ok_or_else(|| failure("native author Session closed"))?;
                author.finish_native_audio_terminal_group(&mut work.group)?;
                work.phase = Phase::Finished;
                return Ok(true);
            }
            (Phase::Published { .. }, _) => {
                work.phase = if parked {
                    Phase::Published { key, queued: false }
                } else {
                    Phase::PublishedWorkUnavailable
                };
                self.retry_native_audio_terminal_author()?;
            }
            _ => {}
        }
        Ok(false)
    }
}
