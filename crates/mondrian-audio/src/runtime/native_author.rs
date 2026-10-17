//! Same live Session parameter-plan publication and exact native Ack.
//!
//! This module never compiles a second author model. Programs originate from
//! one canonical accepted author generation. It prepares immutable plans over
//! actual current Sessions, admits their overlap against the frozen root grant,
//! and moves only already validated owners at the existing Source boundary.

use super::native_author_logical_fees::LogicalPlanFee;
use super::native_author_scope::{replace_runtime_weights, replace_weights};
use super::*;
use crate::processor_host::native_author::PreparedParameterNativeBindings;
use crate::render::PreparedNativeAudioParameterPlan;
use crate::{
    AudioNativeOwnerError, AudioParameterPlanResourceReplacement, NativeAudioAuthorAdmissionError,
    NativeHostAllocationOwner,
};
use mondrian_core::{NativeAudioAuthorProposal, NativeAudioEditorLocator};
use std::num::NonZeroU64;

/// Immutable canonical Program supplied for an actual output of the target
/// author Sequence. Sequence identity is explicit, never inferred from an
/// output ID, selected frame, later UI state or native callback.
pub struct NativeAudioAuthorProgram {
    /// Exact author Sequence from the accepted canonical snapshot.
    pub sequence: SequenceId,
    /// Sole compiled Program with the exact requested public output.
    pub program: Arc<crate::CompiledAudioProgram>,
}

/// Preparation or completion did not prove a full same-object handoff.
#[derive(Debug, thiserror::Error)]
pub enum NativeAudioParameterHandoffError {
    /// Actual source/owner/boundary remains unaccepted.
    #[error(transparent)]
    Admission(#[from] NativeAudioAuthorAdmissionError),
    /// Current native object binding could not be preserved.
    #[error(transparent)]
    Owner(#[from] AudioNativeOwnerError),
    /// Canonical topology/schema or exact automation lowering changed.
    #[error(transparent)]
    Compile(#[from] crate::AudioCompileError),
    /// The frozen root resource policy refused the real owner overlap.
    #[error(transparent)]
    Resources(#[from] AudioRuntimeBuildError),
    /// Complete owner observation is unavailable or malformed.
    #[error(transparent)]
    Storage(#[from] crate::AudioProcessorFactoryStorageError),
    /// The retained live Session cannot admit this parameter-only swap.
    #[error(transparent)]
    Execution(#[from] crate::AudioExecutionError),
    /// Final exact registered Source publication was superseded or refused.
    #[error(transparent)]
    Installation(#[from] mondrian_core::AudioOwnerInstallationError),
    /// Caller omitted, duplicated or supplied an unrelated Program.
    #[error("native author Program set does not match actual target outputs")]
    ProgramMismatch,
}

struct ParameterBindingTreeContext<'a> {
    old_root: crate::AudioSessionCursorIdentity,
    next_root: crate::AudioSessionCursorIdentity,
    serial: NonZeroU64,
    proposal: &'a NativeAudioAuthorProposal,
    cancellation: &'a ExecutionCancellationToken,
}

struct LocalReplacement {
    current: crate::AudioSessionCursorIdentity,
    plan: Option<PreparedNativeAudioParameterPlan>,
    bindings: PreparedParameterNativeBindings,
    footprint: AudioRuntimeResourceFootprint,
}

impl LocalReplacement {
    fn next_identity(&self) -> crate::AudioSessionCursorIdentity {
        crate::AudioSessionCursorIdentity {
            prepared_plan_id: self.plan.as_ref().map_or(self.current.prepared_plan_id, |plan| {
                plan.next_plan().preparation_id()
            }),
            session_id: self.current.session_id,
        }
    }
}

/// Owned immutable/scratch/binding preparation for this exact existing tree.
///
/// No public constructor, Clone, native permission or author history is carried.
/// Its old Plan/scratch owners remain in the same slots after commit and are
/// retired outside final guards. It neither enters/reset DSP nor changes PDC.
struct PreparedSameLiveParameterHandoff {
    old_root: crate::AudioSessionCursorIdentity,
    old_serial: NonZeroU64,
    proposal: NativeAudioAuthorProposal,
    destination: mondrian_core::AudioOwnerInstallationVerifier,
    locals: Vec<LocalReplacement>,
    resources: crate::AudioParameterPlanResourceCommit,
    unresolved_factory_owners: usize,
    retired_claim: Option<mondrian_core::AudioOwnerInstallationClaim>,
    retired_verifier: Option<mondrian_core::AudioOwnerInstallationVerifier>,
    // Last: actual Plan/scratch and other retained owners retire before credit.
    logical_fee: LogicalPlanFee,
}

/// Prepared same-live or explicit state-entry operation on one retained tree.
/// Neither case is an installed lease, author generation or native permission.
pub struct PreparedNativeAudioParameterHandoff {
    kind: PreparedParameterHandoffKind,
}

enum PreparedParameterHandoffKind {
    Preserved(Box<PreparedSameLiveParameterHandoff>),
    StateEntry(Box<super::native_author_scope::PreparedScopeHandle>),
}

/// DSP-continuity fact, distinct from successful canonical author publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioParameterHandoffContinuity {
    /// Existing exact Session/DSP cursors survived; Continue remains legal.
    Preserved,
    /// Fresh Sessions/objects require a new admitted entry and output reprime.
    StateEntryRequired,
}

/// Actual same-object acknowledgement after canonical Plan installation.
/// This is not physical GUI or whole SDK/OS resource acceptance evidence.
pub struct NativeAudioParameterHandoffReceipt {
    /// Current claim from the one private Media Source publication.
    pub claim: mondrian_core::AudioOwnerInstallationClaim,
    /// Exact newly installed native target acknowledged by the retained child.
    pub target: mondrian_core::NativeAudioEditorTarget,
    /// Actual Arc/encoded owner count at preparation. A retry-only Ack returns
    /// None because it performs no inventory; None does not claim zero unknown
    /// owners or complete physical qualification.
    pub unresolved_factory_owners: Option<usize>,
    /// Actual continuity consequence of the completed graph publication.
    pub continuity: NativeAudioParameterHandoffContinuity,
}

pub(super) fn locator(proposal: &NativeAudioAuthorProposal) -> NativeAudioEditorLocator {
    let target = proposal.target();
    NativeAudioEditorLocator {
        sequence: proposal.sequence(),
        instance: target.instance,
        owner: target.owner,
        insertion: target.insertion,
    }
}

impl AudioProgramRuntime {
    /// Prepare every real target Sequence/Scope materialization on this retained
    /// tree, with actual current/next buffers, frozen resource grant and native
    /// Bind acknowledgements. No graph, author state or PCM is published here.
    pub fn prepare_native_author_parameter_handoff(
        &mut self,
        programs: &[NativeAudioAuthorProgram],
        proposal: &NativeAudioAuthorProposal,
        destination: mondrian_core::AudioOwnerInstallationVerifier,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<PreparedNativeAudioParameterHandoff, NativeAudioParameterHandoffError> {
        if cancellation.is_canceled() {
            return Err(NativeAudioAuthorAdmissionError::Cancelled.into());
        }
        let old_root = self.session.native_owner_identity();
        let old_serial = self.native_author_installation_serial()?;
        if proposal.target().root_plan != old_root.prepared_plan_id
            || proposal.target().root_session != old_root.session_id
            || proposal.target().installed_generation != old_serial
        {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        for (index, program) in programs.iter().enumerate() {
            if program.sequence != proposal.sequence()
                || programs[..index]
                    .iter()
                    .any(|prior| prior.program.output_id() == program.program.output_id())
            {
                return Err(NativeAudioParameterHandoffError::ProgramMismatch);
            }
        }
        if !self
            .native_installation
            .verifier
            .as_ref()
            .is_some_and(|current| current.same_source(&destination))
        {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        if self.native_author_state_entry_required(proposal)? {
            return self
                .prepare_native_author_scope_handoff(programs, proposal, destination, cancellation)
                .map(|handle| PreparedNativeAudioParameterHandoff {
                    kind: PreparedParameterHandoffKind::StateEntry(Box::new(handle)),
                });
        }
        let requested_logical = self.preflight_native_author_logical_overlap(programs, proposal)?;
        let mut locals = Vec::new();
        self.prepare_parameter_locals(
            programs,
            locator(proposal),
            proposal.target().local_session,
            &mut locals,
        )?;
        if !locals.iter().any(|local| local.plan.is_some()) {
            return Err(NativeAudioParameterHandoffError::ProgramMismatch);
        }
        self.find_resource_session(crate::AudioSessionCursorIdentity {
            prepared_plan_id: proposal.target().local_plan,
            session_id: proposal.target().local_session,
        })
        .ok_or(AudioNativeOwnerError::OwnerUnavailable)?
        .retain_native_author_proposal(proposal, old_root, old_serial)?;

        // Observe complete allocations via their owners, not free byte guesses.
        // Arc payload and platform-encoded boundaries remain explicit unknowns;
        // they are never mislabelled complete physical allocations or zero RSS.
        let mut extra = Vec::new();
        let mut unresolved = 0usize;
        extra.push(locals.host_allocation().map_err(AudioRuntimeBuildError::from)?);
        let mut replacements = Vec::new();
        for local in &locals {
            let current = self
                .find_resource_session(local.current)
                .ok_or(AudioNativeOwnerError::OwnerUnavailable)?;
            extra.push(
                local
                    .bindings
                    .resource_owner()
                    .host_allocation()
                    .map_err(AudioRuntimeBuildError::from)?,
            );
            for plan in std::iter::once(current.resource_plan())
                .chain(local.plan.iter().map(|plan| plan.next_plan().as_ref()))
            {
                plan.visit_native_host_owned_storage(&mut |owner| {
                    if let Some(owner) = owner.complete_allocation() {
                        extra.push(owner);
                    } else {
                        unresolved = unresolved.checked_add(1).ok_or(
                            crate::AudioProcessorFactoryStorageError::Owner(
                                crate::NativeHostLedgerError::ExtentOverflow,
                            ),
                        )?;
                    }
                    Ok(())
                })?;
            }
            if let Some(plan) = &local.plan {
                let (events, lanes) = plan.resource_buffers();
                replacements.push(AudioParameterPlanResourceReplacement::new(
                    current,
                    plan.next_plan(),
                    events,
                    lanes,
                ));
            }
        }
        // The caller's replacement Vec is itself a simultaneously live owner.
        extra.push(replacements.host_allocation().map_err(AudioRuntimeBuildError::from)?);
        let resources = self
            .admit_parameter_plan_overlap(&replacements, &extra)?
            .into_commit(self, &replacements)?;
        drop(extra);
        drop(replacements);
        let (old_logical, new_logical) = resources.logical_role_weights();
        if new_logical > requested_logical {
            return Err(AudioRuntimeBuildError::InvalidResourceReplacement.into());
        }

        // Compute every cached subtree weight before any foreign Bind. The one
        // original admission receipt remains the authoritative Root result.
        let mut cursor = 0;
        let footprint = self.prepare_parameter_footprints(&mut locals, &mut cursor)?;
        if cursor != locals.len() || footprint != resources.footprint() {
            return Err(AudioRuntimeBuildError::InvalidResourceReplacement.into());
        }
        let next_root = locals[0].next_identity();
        let mut cursor = 0;
        self.prepare_parameter_bindings(
            &mut locals,
            &mut cursor,
            &ParameterBindingTreeContext {
                old_root,
                next_root,
                serial: old_serial,
                proposal,
                cancellation,
            },
        )?;
        if cursor != locals.len() || cancellation.is_canceled() {
            return Err(NativeAudioAuthorAdmissionError::Cancelled.into());
        }
        let logical_fee =
            self.prepare_native_plan_logical_fee(old_logical, new_logical, resources.footprint())?;
        // Keep the large storage owner out of the metadata enum. Allocate the
        // complete owner before its fee is enrolled and before any final guard.
        let mut prepared = Box::new(PreparedSameLiveParameterHandoff {
            old_root,
            old_serial,
            proposal: proposal.clone(),
            destination,
            locals,
            resources,
            unresolved_factory_owners: unresolved,
            retired_claim: None,
            retired_verifier: None,
            logical_fee,
        });
        // Only an assembled owning handle can enroll. No fallible/allocation
        // step follows the successful increment before this owner is returned.
        prepared.logical_fee.enroll()?;
        Ok(PreparedNativeAudioParameterHandoff {
            kind: PreparedParameterHandoffKind::Preserved(prepared),
        })
    }

    fn prepare_parameter_locals(
        &self,
        programs: &[NativeAudioAuthorProgram],
        locator: NativeAudioEditorLocator,
        selected_session: mondrian_core::AudioRenderSessionId,
        locals: &mut Vec<LocalReplacement>,
    ) -> Result<(), NativeAudioParameterHandoffError> {
        let plan =
            if self.sequence_id == locator.sequence {
                let program = programs
                    .iter()
                    .find(|program| {
                        program.program.output_id()
                            == self.session.resource_plan().program().output_id()
                    })
                    .ok_or(NativeAudioParameterHandoffError::ProgramMismatch)?;
                if self.session.resource_plan().program() == program.program.as_ref()
                    && self.session.native_owner_identity().session_id != selected_session
                {
                    None
                } else {
                    Some(self.session.prepare_native_author_parameter_plan(
                        Arc::clone(&program.program),
                        locator,
                    )?)
                }
            } else {
                None
            };
        locals.push(LocalReplacement {
            current: self.session.native_owner_identity(),
            plan,
            bindings: self.session.allocate_parameter_native_bindings(),
            footprint: self.resource_footprint,
        });
        for source in self.sources.entries.values() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.prepare_parameter_locals(
                    programs,
                    locator,
                    selected_session,
                    locals,
                )?;
            }
        }
        Ok(())
    }

    fn prepare_parameter_footprints(
        &self,
        locals: &mut [LocalReplacement],
        cursor: &mut usize,
    ) -> Result<AudioRuntimeResourceFootprint, AudioRuntimeBuildError> {
        let index = *cursor;
        *cursor += 1;
        let local = locals.get(index).ok_or(AudioRuntimeBuildError::InvalidResourceReplacement)?;
        if local.current != self.session.native_owner_identity() {
            return Err(AudioRuntimeBuildError::StaleResourceAdmission);
        }
        let mut footprint = if let Some(plan) = &local.plan {
            replace_weights(
                self.resource_footprint,
                self.session.current_resource_footprint()?,
                plan.resource_footprint()?,
            )?
        } else {
            self.resource_footprint
        };
        for source in self.sources.entries.values() {
            if let RuntimeSource::Nested(nested) = source {
                let after = nested.runtime.prepare_parameter_footprints(locals, cursor)?;
                footprint =
                    replace_runtime_weights(footprint, nested.runtime.resource_footprint, after)?;
            }
        }
        locals[index].footprint = footprint;
        Ok(footprint)
    }

    fn prepare_parameter_bindings(
        &mut self,
        locals: &mut [LocalReplacement],
        cursor: &mut usize,
        context: &ParameterBindingTreeContext<'_>,
    ) -> Result<(), NativeAudioParameterHandoffError> {
        let local = locals.get_mut(*cursor).ok_or(AudioNativeOwnerError::OwnerUnavailable)?;
        if local.current != self.session.native_owner_identity() {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        let next = local.next_identity();
        self.session.prepare_parameter_native_bindings(
            &mut local.bindings,
            crate::processor_host::native_author::ParameterNativeBindingContext {
                current_root: context.old_root,
                current_local: local.current,
                next_root: context.next_root,
                next_local: next,
                installed_serial: context.serial,
                proposal: context.proposal,
                cancellation: context.cancellation,
            },
        )?;
        *cursor += 1;
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.prepare_parameter_bindings(locals, cursor, context)?;
            }
        }
        Ok(())
    }

    /// Complete a prepared handoff while the actual Runtime is exclusively held.
    ///
    /// The App provider must already hold the canonical snapshot read guard.
    /// The final Source CAS publishes only prevalidated bounded owner moves.
    /// This method performs no native Ack. The caller first releases its author
    /// read guard and retired owners, then calls the separate acknowledgement
    /// seam. An Ack failure retains its private InstalledAwaitingAck phase.
    pub fn install_native_author_parameter_handoff(
        &mut self,
        prepared: &mut PreparedNativeAudioParameterHandoff,
        authorization: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<NativeAudioParameterHandoffReceipt, NativeAudioParameterHandoffError> {
        let prepared = match &mut prepared.kind {
            PreparedParameterHandoffKind::StateEntry(handle) => {
                return self.install_native_author_scope_handoff(
                    handle,
                    authorization,
                    cancellation,
                )
            }
            PreparedParameterHandoffKind::Preserved(prepared) => prepared,
        };
        if cancellation.is_canceled() {
            return Err(NativeAudioAuthorAdmissionError::Cancelled.into());
        }
        if self.session.native_owner_identity() != prepared.old_root
            || self.native_author_installation_serial()? != prepared.old_serial
            || prepared.resources.current_root() != prepared.old_root
        {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        let mut cursor = 0;
        self.validate_parameter_swap(&prepared.locals, &mut cursor, &prepared.proposal)?;
        if cursor != prepared.locals.len() {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        if prepared.retired_claim.is_some() || prepared.retired_verifier.is_some() {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        let mut selected = prepared
            .locals
            .iter()
            .filter_map(|local| local.bindings.selected_record(&prepared.proposal));
        let selected_record = selected.next().ok_or(AudioNativeOwnerError::OwnerUnavailable)?;
        if selected.next().is_some() {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        let footprint = prepared.resources.footprint();
        if prepared.locals.first().map(|local| local.footprint) != Some(footprint) {
            return Err(AudioRuntimeBuildError::InvalidResourceReplacement.into());
        }
        let fee_retirement =
            self.prepare_native_plan_fee_retirement(&prepared.logical_fee, footprint)?;
        let next_verifier = prepared.destination.clone();
        if cancellation.is_canceled() {
            return Err(NativeAudioAuthorAdmissionError::Cancelled.into());
        }
        let commit = authorization.begin_commit(&prepared.destination)?;
        // The same actual tree and exclusive borrow were fully validated above.
        // These operations retain retired Plan/scratch owners in prepared slots;
        // no allocation, SDK call, foreign destructor or Result follows here.
        let mut cursor = 0;
        self.swap_parameter_plans(&mut prepared.locals, &mut cursor);
        self.resource_footprint = footprint;
        prepared.logical_fee.retire(fee_retirement);
        prepared.retired_verifier = self.native_installation.verifier.replace(next_verifier);
        let generation = commit.generation();
        let mut cursor = 0;
        self.publish_parameter_bindings(&prepared.locals, &mut cursor, generation);
        let claim = commit.commit();
        prepared.retired_claim = self.native_installation.claim.replace(claim.clone());
        self.refresh_native_entry_context();
        let target = selected_record.editor_target(claim.generation());
        // The caller retains prepared (including retired plans, factories and
        // registration wrappers) until BOTH author and Source guards ended.
        Ok(NativeAudioParameterHandoffReceipt {
            claim,
            target,
            unresolved_factory_owners: Some(prepared.unresolved_factory_owners),
            continuity: NativeAudioParameterHandoffContinuity::Preserved,
        })
    }

    fn validate_parameter_swap(
        &mut self,
        locals: &[LocalReplacement],
        cursor: &mut usize,
        proposal: &NativeAudioAuthorProposal,
    ) -> Result<(), NativeAudioParameterHandoffError> {
        let local = locals.get(*cursor).ok_or(AudioNativeOwnerError::OwnerUnavailable)?;
        if local.current != self.session.native_owner_identity() {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        if let Some(plan) = &local.plan {
            self.session.validate_parameter_plan_swap(plan, proposal, &local.bindings)?;
        }
        self.session.validate_parameter_native_bindings(&local.bindings, proposal)?;
        *cursor += 1;
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.validate_parameter_swap(locals, cursor, proposal)?;
            }
        }
        Ok(())
    }

    fn swap_parameter_plans(&mut self, locals: &mut [LocalReplacement], cursor: &mut usize) {
        let local = &mut locals[*cursor];
        if let Some(plan) = &mut local.plan {
            self.session.swap_parameter_plan(plan);
        }
        self.resource_footprint = local.footprint;
        *cursor += 1;
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.swap_parameter_plans(locals, cursor);
            }
        }
        // Keep delay lines, already rendered caches and exact sample cursors.
        // Invalidating those buffers would replay stateful DSP without a real
        // checkpoint; new parameters apply at each actual next processing seam.
    }

    fn publish_parameter_bindings(
        &mut self,
        locals: &[LocalReplacement],
        cursor: &mut usize,
        serial: NonZeroU64,
    ) {
        self.session
            .publish_parameter_native_bindings(&locals[*cursor].bindings, serial);
        *cursor += 1;
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.publish_parameter_bindings(locals, cursor, serial);
            }
        }
    }

    /// Retry only the actual private installed delivery on this current root.
    /// No author mutation, allocation, Plan swap, Enter/Stop or sample cursor
    /// advancement occurs. SDK work must run outside the author/Source guards.
    pub fn acknowledge_native_author_parameter_handoff(
        &mut self,
        proposal: &NativeAudioAuthorProposal,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<NativeAudioParameterHandoffReceipt, NativeAudioParameterHandoffError> {
        if cancellation.is_canceled() {
            return Err(NativeAudioAuthorAdmissionError::Cancelled.into());
        }
        let serial = self.native_author_installation_serial()?;
        let claim = self
            .native_installation
            .claim
            .as_ref()
            .ok_or(AudioNativeOwnerError::OwnerUnavailable)?
            .clone();
        // The actual Ack may consume the retained source irreversibly. All
        // fallible owner checks precede it; cancellation during a successful
        // Ack cannot turn that confirmed retirement into a retryable error.
        let target = self.ack_parameter_author(proposal, serial, cancellation)?;
        let state_entry = self.scope_acknowledged(proposal);
        Ok(NativeAudioParameterHandoffReceipt {
            claim,
            target,
            unresolved_factory_owners: None,
            continuity: if state_entry {
                NativeAudioParameterHandoffContinuity::StateEntryRequired
            } else {
                NativeAudioParameterHandoffContinuity::Preserved
            },
        })
    }

    fn ack_parameter_author(
        &mut self,
        proposal: &NativeAudioAuthorProposal,
        serial: NonZeroU64,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<mondrian_core::NativeAudioEditorTarget, NativeAudioParameterHandoffError> {
        let local = self.session.native_owner_identity();
        // The selected local Plan changed; Session identity remains exact.
        if local.session_id == proposal.target().local_session
            || self.session.retains_parameter_author(proposal)
        {
            return Ok(self.session.acknowledge_parameter_native_author(
                proposal,
                serial,
                cancellation,
            )?);
        }
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source
                && nested.runtime.contains_retained_parameter_author(proposal)
            {
                return nested.runtime.ack_parameter_author(proposal, serial, cancellation);
            }
        }
        Err(AudioNativeOwnerError::OwnerUnavailable.into())
    }

    fn contains_retained_parameter_author(&self, proposal: &NativeAudioAuthorProposal) -> bool {
        self.session.retains_parameter_author(proposal)
            || self.sources.entries.values().any(|source| {
                matches!(source, RuntimeSource::Nested(nested)
                    if nested.runtime.contains_retained_parameter_author(proposal))
            })
    }
}

#[cfg(test)]
#[path = "native_author_charge_test_controls.rs"]
pub(crate) mod charge_test_controls;
