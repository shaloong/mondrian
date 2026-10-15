//! Whole retained-tree state-entry publication for terminal shared native state.
//!
//! This is an explicit DSP discontinuity. Existing processors/children move;
//! no resolver, factory creation, second evaluator or substitute PCM is used.

use super::native_author_logical_fees::LogicalPlanFee;
use super::*;
use crate::processor_host::state_entry::{PreparedStateEntryRecords, StateEntryContext};
use crate::render::PreparedAudioSessionStorage;
use crate::NativeHostAllocationOwner;
use std::num::NonZeroU64;

struct ScopeLocal {
    current: crate::AudioSessionCursorIdentity,
    next: PreparedAudioSessionStorage,
    affected: bool,
    native: PreparedStateEntryRecords,
    footprint: AudioRuntimeResourceFootprint,
}

/// The sole Runtime retains this cohort across incomplete foreign preparation.
/// Every old buffer survives until install/Ack. Once the first child is retired,
/// the early root execution hold makes old PCM impossible even on cancellation.
pub(super) struct NativeAuthorScopeCohort {
    old_root: crate::AudioSessionCursorIdentity,
    old_serial: NonZeroU64,
    proposal: mondrian_core::NativeAudioAuthorProposal,
    destination: mondrian_core::AudioOwnerInstallationVerifier,
    locals: Vec<ScopeLocal>,
    footprint: AudioRuntimeResourceFootprint,
    unresolved_owners: usize,
    installed: bool,
    retired_claim: Option<mondrian_core::AudioOwnerInstallationClaim>,
    retired_verifier: Option<mondrian_core::AudioOwnerInstallationVerifier>,
    // Last: real cohort Sessions/Plans retire before its logical-role credit.
    logical_fee: Option<LogicalPlanFee>,
}

/// Private exact operation handle, not author, resource or installation authority.
pub(super) struct PreparedScopeHandle {
    old_root: crate::AudioSessionCursorIdentity,
    proposal: mondrian_core::NativeAudioAuthorProposal,
}

fn overflow() -> AudioRuntimeBuildError {
    AudioRuntimeBuildError::ResourceFootprint(crate::AudioResourceFootprintError::ExtentOverflow)
}
fn sum(a: usize, b: usize) -> Result<usize, AudioRuntimeBuildError> {
    a.checked_add(b).ok_or_else(overflow)
}
fn substitute(total: usize, old: usize, next: usize) -> Result<usize, AudioRuntimeBuildError> {
    total
        .checked_sub(old)
        .and_then(|value| value.checked_add(next))
        .ok_or_else(overflow)
}

impl AudioProgramRuntime {
    /// Read-only policy from the complete retained tree. It grants no native
    /// call or installed permission; Media uses it only to retire real output
    /// before a terminal operation can destroy a native incarnation.
    pub fn native_author_state_entry_required(
        &self,
        proposal: &mondrian_core::NativeAudioAuthorProposal,
    ) -> Result<bool, crate::AudioNativeOwnerError> {
        let root = self.session.native_owner_identity();
        if proposal.target().root_plan != root.prepared_plan_id
            || proposal.target().root_session != root.session_id
            || proposal.target().installed_generation != self.native_author_installation_serial()?
        {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable);
        }
        if !proposal.terminal() || proposal.state().is_none() {
            return Ok(false);
        }
        if proposal.target().insertion == mondrian_core::NativeAudioEditorInsertion::Scope {
            return Ok(true);
        }
        Ok(self.native_author_materialization_count(proposal)? != 1)
    }
    fn native_author_materialization_count(
        &self,
        proposal: &mondrian_core::NativeAudioAuthorProposal,
    ) -> Result<usize, crate::AudioNativeOwnerError> {
        let occurrence =
            crate::native_runtime_owner::editor_target_address(proposal.target()).occurrence;
        let mut count = if self.sequence_id == proposal.sequence() {
            self.session
                .resource_plan()
                .schedule
                .processors
                .iter()
                .filter(|processor| processor.occurrence == occurrence)
                .count()
        } else {
            0
        };
        for source in self.sources.entries.values() {
            if let RuntimeSource::Nested(nested) = source {
                count = count
                    .checked_add(nested.runtime.native_author_materialization_count(proposal)?)
                    .ok_or(crate::AudioNativeOwnerError::OwnerUnavailable)?;
            }
        }
        Ok(count)
    }
    pub(super) fn native_scope_control_hold(
        &self,
    ) -> Option<crate::NativeAudioAuthorExecutionHold> {
        self.native_author_scope
            .as_ref()
            .map(|cohort| crate::NativeAudioAuthorExecutionHold {
                target: cohort.proposal.target(),
                reason: crate::NativeAudioAuthorHoldReason::CoherentPeerStateRequired,
            })
    }
    pub(super) fn prepare_native_author_scope_handoff(
        &mut self,
        programs: &[super::native_author::NativeAudioAuthorProgram],
        proposal: &mondrian_core::NativeAudioAuthorProposal,
        destination: mondrian_core::AudioOwnerInstallationVerifier,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<PreparedScopeHandle, super::native_author::NativeAudioParameterHandoffError> {
        use super::native_author::NativeAudioParameterHandoffError as Error;
        if cancellation.is_canceled() {
            return Err(crate::NativeAudioAuthorAdmissionError::Cancelled.into());
        }
        if let Some(cohort) = &self.native_author_scope {
            if cohort.installed
                || !cohort.proposal.same_retained_proposal(proposal)
                || cohort.old_root != self.session.native_owner_identity()
                || cohort.old_serial != self.native_author_installation_serial()?
            {
                return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
            }
            // Canonical immutable data cannot be changed on a retry of the
            // same prefix. The issued author snapshot separately gates commit.
            for local in &cohort.locals {
                if local.affected
                    && !programs.iter().any(|program| {
                        program.sequence == proposal.sequence()
                            && program.program.as_ref() == local.next.plan().program()
                    })
                {
                    return Err(Error::ProgramMismatch);
                }
            }
            self.validate_native_plan_logical_fee(
                cohort
                    .logical_fee
                    .as_ref()
                    .ok_or(AudioRuntimeBuildError::StaleResourceAdmission)?,
            )?;
        } else {
            let old_root = self.session.native_owner_identity();
            let old_serial = self.native_author_installation_serial()?;
            if !proposal.terminal()
                || proposal.target().root_plan != old_root.prepared_plan_id
                || proposal.target().root_session != old_root.session_id
                || proposal.target().installed_generation != old_serial
            {
                return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
            }
            // Every new local Session, including unaffected shared Plans, is
            // admitted under the one Root grant before any local clone/retain.
            let (requested_logical, affected) =
                self.requested_scope_logical_bytes(programs, proposal)?;
            if !affected {
                return Err(Error::ProgramMismatch);
            }
            self.check_resource_ceiling(
                AudioRuntimeResourceCategory::PreparedLogicalBytes,
                sum(
                    self.resource_footprint.prepared_logical_bytes,
                    requested_logical,
                )?,
            )?;
            let mut locals = Vec::new();
            let mut reserved = self.resource_footprint;
            let prior_held_logical = self.held_native_plan_logical_bytes();
            self.allocate_scope_locals(
                programs,
                proposal,
                &mut locals,
                &mut reserved,
                self.resource_grant,
                prior_held_logical,
            )?;
            if !locals.iter().any(|local| local.affected) {
                return Err(Error::ProgramMismatch);
            }
            self.find_resource_session(crate::AudioSessionCursorIdentity {
                prepared_plan_id: proposal.target().local_plan,
                session_id: proposal.target().local_session,
            })
            .ok_or(crate::AudioNativeOwnerError::OwnerUnavailable)?
            .retain_native_author_proposal(proposal, old_root, old_serial)?;
            let mut cohort = Box::new(NativeAuthorScopeCohort {
                old_root,
                old_serial,
                proposal: proposal.clone(),
                destination: destination.clone(),
                locals,
                footprint: self.resource_footprint,
                unresolved_owners: 0,
                installed: false,
                retired_claim: None,
                retired_verifier: None,
                logical_fee: None,
            });
            // This batch is still stack-owned and excluded from prior held fees.
            // Its actual local weights must fit the borrowed whole-tree upper.
            let (old_logical, new_logical) = self.admit_scope_overlap(&mut cohort)?;
            if new_logical > requested_logical {
                return Err(AudioRuntimeBuildError::InvalidResourceReplacement.into());
            }
            let mut cursor = 0;
            self.inspect_scope_sources(&mut cohort, &mut cursor, cancellation)?;
            if cursor != cohort.locals.len() {
                return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
            }
            cohort.logical_fee = Some(self.prepare_native_plan_logical_fee(
                old_logical,
                new_logical,
                cohort.footprint,
            )?);
            cohort
                .logical_fee
                .as_mut()
                .ok_or(AudioRuntimeBuildError::StaleResourceAdmission)?
                .enroll()?;
            // Persisted partial errors/cancellation keep the actual N owner fee;
            // external metadata handles never release it or enroll another one.
            self.native_author_scope = Some(cohort);
        }
        // The cohort leaves its field only while this exclusive Runtime borrow
        // owns it. It is restored for every fallible result, including partial
        // native success/cancellation; its execution hold never gets erased.
        let mut cohort = self
            .native_author_scope
            .take()
            .ok_or(crate::AudioNativeOwnerError::OwnerUnavailable)?;
        cohort.destination = destination;
        let mut cursor = 0;
        let result =
            self.prepare_scope_sources(&mut cohort, &mut cursor, cancellation)
                .and_then(|()| {
                    let mut verified = 0;
                    self.revalidate_scope_sources(&cohort, &mut verified, cancellation)?;
                    if verified != cohort.locals.len() {
                        return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
                    }
                    Ok(())
                });
        let count_matches = cursor == cohort.locals.len();
        let handle = PreparedScopeHandle {
            old_root: cohort.old_root,
            proposal: cohort.proposal.clone(),
        };
        self.native_author_scope = Some(cohort);
        result?;
        if !count_matches {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
        }
        Ok(handle)
    }

    /// Borrow the logical role of every complete new local Session before any
    /// prepared clone, Plan Arc retain or storage allocation. Unlike Preserved,
    /// StateEntry also migrates equal affected peers and unaffected locals.
    fn requested_scope_logical_bytes(
        &self,
        programs: &[super::native_author::NativeAudioAuthorProgram],
        proposal: &mondrian_core::NativeAudioAuthorProposal,
    ) -> Result<(usize, bool), super::native_author::NativeAudioParameterHandoffError> {
        let affected = self.sequence_id == proposal.sequence();
        let current = self.session.resource_plan();
        let mut requested = if affected {
            let program = programs
                .iter()
                .find(|program| {
                    program.sequence == self.sequence_id
                        && program.program.output_id() == current.program().output_id()
                })
                .ok_or(super::native_author::NativeAudioParameterHandoffError::ProgramMismatch)?;
            current.requested_native_author_logical_bytes(
                program.program.as_ref(),
                super::native_author::locator(proposal),
            )?
        } else {
            // This is the local immutable weight, not the cached subtree total.
            current
                .session_resource_footprint()
                .map_err(AudioRuntimeBuildError::from)?
                .prepared_logical_bytes
        };
        let mut any_affected = affected;
        for source in self.sources.entries.values() {
            if let RuntimeSource::Nested(nested) = source {
                let (next, nested_affected) =
                    nested.runtime.requested_scope_logical_bytes(programs, proposal)?;
                requested = sum(requested, next)?;
                any_affected |= nested_affected;
            }
        }
        Ok((requested, any_affected))
    }

    fn allocate_scope_locals(
        &self,
        programs: &[super::native_author::NativeAudioAuthorProgram],
        proposal: &mondrian_core::NativeAudioAuthorProposal,
        locals: &mut Vec<ScopeLocal>,
        reserved: &mut AudioRuntimeResourceFootprint,
        grant: AudioRuntimeResourceGrant,
        prior_held_logical: usize,
    ) -> Result<(), super::native_author::NativeAudioParameterHandoffError> {
        let affected = self.sequence_id == proposal.sequence();
        let plan = if affected {
            let program = programs
                .iter()
                .find(|program| {
                    program.sequence == self.sequence_id
                        && program.program.output_id()
                            == self.session.resource_plan().program().output_id()
                })
                .ok_or(super::native_author::NativeAudioParameterHandoffError::ProgramMismatch)?;
            Arc::new(
                self.session.resource_plan().prepare_native_author_parameters(
                    Arc::clone(&program.program),
                    super::native_author::locator(proposal),
                )?,
            )
        } else {
            Arc::clone(self.session.shared_resource_plan())
        };
        // The sole Prepared Plan's requested storage model is reserved before
        // allocation. Actual owner capacity/alias validation follows before any
        // foreign state entry; no observed footprint enlarges this frozen grant.
        let requested = plan.session_resource_footprint().map_err(AudioRuntimeBuildError::from)?;
        reserved.prepared_logical_bytes = sum(
            reserved.prepared_logical_bytes,
            requested.prepared_logical_bytes,
        )?;
        reserved.fixed_resident_bytes = sum(
            reserved.fixed_resident_bytes,
            requested.fixed_resident_bytes,
        )?;
        let required_logical = sum(reserved.prepared_logical_bytes, prior_held_logical)?;
        if required_logical > grant.max_prepared_logical_bytes {
            return Err(AudioRuntimeBuildError::ResourceOverlapGrantExceeded {
                category: AudioRuntimeResourceCategory::PreparedLogicalBytes,
                required: required_logical,
                granted: grant.max_prepared_logical_bytes,
            }
            .into());
        }
        if reserved.fixed_resident_bytes > grant.max_fixed_resident_bytes {
            return Err(AudioRuntimeBuildError::ResourceOverlapGrantExceeded {
                category: AudioRuntimeResourceCategory::FixedResidentBytes,
                required: reserved.fixed_resident_bytes,
                granted: grant.max_fixed_resident_bytes,
            }
            .into());
        }
        let next = AudioRenderSession::allocate_storage(plan)?;
        next.validate_transfer(&self.session)?;
        locals.push(ScopeLocal {
            current: self.session.native_owner_identity(),
            next,
            affected,
            native: self.session.allocate_state_entry_records(),
            footprint: self.resource_footprint,
        });
        for source in self.sources.entries.values() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.allocate_scope_locals(
                    programs,
                    proposal,
                    locals,
                    reserved,
                    grant,
                    prior_held_logical,
                )?;
            }
        }
        Ok(())
    }

    fn admit_scope_overlap(
        &self,
        cohort: &mut Box<NativeAuthorScopeCohort>,
    ) -> Result<(usize, usize), super::native_author::NativeAudioParameterHandoffError> {
        let mut next_logical = 0;
        let mut removed = crate::AudioSessionResourceFootprint::default();
        let mut inserted = crate::AudioSessionResourceFootprint::default();
        let mut owner_count = 0usize;
        let mut unresolved = 0usize;
        for local in &cohort.locals {
            let current = self
                .find_resource_session(local.current)
                .ok_or(crate::AudioNativeOwnerError::OwnerUnavailable)?;
            let old = current.current_resource_footprint().map_err(AudioRuntimeBuildError::from)?;
            let next =
                local.next.current_resource_footprint().map_err(AudioRuntimeBuildError::from)?;
            next_logical = sum(next_logical, next.prepared_logical_bytes)?;
            removed = merge_session_weights(removed, old)?;
            inserted = merge_session_weights(inserted, next)?;
            local.next.visit_storage(&mut |owner| {
                if owner.complete_allocation().is_some() {
                    owner_count = owner_count.checked_add(1).ok_or(
                        crate::AudioProcessorFactoryStorageError::Owner(
                            crate::NativeHostLedgerError::ExtentOverflow,
                        ),
                    )?;
                } else {
                    unresolved = unresolved.checked_add(1).ok_or(
                        crate::AudioProcessorFactoryStorageError::Owner(
                            crate::NativeHostLedgerError::ExtentOverflow,
                        ),
                    )?;
                }
                Ok(())
            })?;
            for plan in [current.resource_plan(), local.next.plan()] {
                plan.visit_native_host_owned_storage(&mut |owner| {
                    if owner.complete_allocation().is_some() {
                        owner_count = owner_count.checked_add(1).ok_or(
                            crate::AudioProcessorFactoryStorageError::Owner(
                                crate::NativeHostLedgerError::ExtentOverflow,
                            ),
                        )?;
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
        }
        let peak_logical = sum(self.resource_footprint.prepared_logical_bytes, next_logical)?;
        self.check_resource_ceiling(
            AudioRuntimeResourceCategory::PreparedLogicalBytes,
            peak_logical,
        )?;
        let count = owner_count
            .checked_add(cohort.locals.len())
            .and_then(|value| value.checked_add(2))
            .ok_or_else(overflow)?;
        let bookkeeping = crate::NativeHostAllocationLedger::storage_layout(count)
            .map_err(AudioRuntimeBuildError::from)?
            .bytes()
            .map_err(AudioRuntimeBuildError::from)?;
        let remaining = self
            .resource_grant
            .max_fixed_resident_bytes
            .checked_sub(self.resource_footprint.fixed_resident_bytes)
            .ok_or(AudioRuntimeBuildError::InvalidResourceReplacement)?;
        self.check_resource_ceiling(
            AudioRuntimeResourceCategory::FixedResidentBytes,
            sum(self.resource_footprint.fixed_resident_bytes, bookkeeping)?,
        )?;
        let mut ledger = crate::NativeHostAllocationLedger::new(count, remaining)
            .map_err(AudioRuntimeBuildError::from)?;
        ledger
            .record(cohort.host_allocation().map_err(AudioRuntimeBuildError::from)?)
            .map_err(AudioRuntimeBuildError::from)?;
        ledger
            .record(cohort.locals.host_allocation().map_err(AudioRuntimeBuildError::from)?)
            .map_err(AudioRuntimeBuildError::from)?;
        for local in &cohort.locals {
            ledger
                .record(local.native.allocation().map_err(AudioRuntimeBuildError::from)?)
                .map_err(AudioRuntimeBuildError::from)?;
            local.next.visit_storage(&mut |owner| {
                if let Some(owner) = owner.complete_allocation() {
                    ledger.record(owner)?;
                }
                Ok(())
            })?;
            let current = self
                .find_resource_session(local.current)
                .ok_or(crate::AudioNativeOwnerError::OwnerUnavailable)?;
            for plan in [current.resource_plan(), local.next.plan()] {
                plan.visit_native_host_owned_storage(&mut |owner| {
                    if let Some(owner) = owner.complete_allocation() {
                        ledger.record(owner)?;
                    }
                    Ok(())
                })?;
            }
        }
        let observed = ledger.finish().map_err(AudioRuntimeBuildError::from)?;
        let peak_fixed = sum(
            self.resource_footprint.fixed_resident_bytes,
            observed.allocation_bytes(),
        )?;
        self.check_resource_ceiling(AudioRuntimeResourceCategory::FixedResidentBytes, peak_fixed)?;
        let original = self.resource_footprint;
        let footprint = AudioRuntimeResourceFootprint {
            runtime_occurrences: original.runtime_occurrences,
            fixed_resident_bytes: substitute(
                original.fixed_resident_bytes,
                removed.fixed_resident_bytes,
                inserted.fixed_resident_bytes,
            )?,
            prepared_logical_bytes: substitute(
                original.prepared_logical_bytes,
                removed.prepared_logical_bytes,
                inserted.prepared_logical_bytes,
            )?,
            render_scratch_bytes: substitute(
                original.render_scratch_bytes,
                removed.render_scratch_bytes,
                inserted.render_scratch_bytes,
            )?,
            processor_session_bytes: substitute(
                original.processor_session_bytes,
                removed.processor_session_bytes,
                inserted.processor_session_bytes,
            )?,
            compensation_delay_bytes: substitute(
                original.compensation_delay_bytes,
                removed.compensation_delay_bytes,
                inserted.compensation_delay_bytes,
            )?,
            parameter_event_bytes: substitute(
                original.parameter_event_bytes,
                removed.parameter_event_bytes,
                inserted.parameter_event_bytes,
            )?,
            media_window_bytes: original.media_window_bytes,
            nested_window_bytes: original.nested_window_bytes,
        };
        self.check_resource_ceiling(
            AudioRuntimeResourceCategory::FixedResidentBytes,
            footprint.fixed_resident_bytes,
        )?;
        let mut cursor = 0;
        self.prepare_scope_footprints(&mut cohort.locals, &mut cursor)?;
        if cursor != cohort.locals.len() || cohort.locals[0].footprint != footprint {
            return Err(AudioRuntimeBuildError::InvalidResourceReplacement.into());
        }
        cohort.footprint = footprint;
        cohort.unresolved_owners = unresolved;
        Ok((
            removed.prepared_logical_bytes,
            inserted.prepared_logical_bytes,
        ))
    }

    fn inspect_scope_sources(
        &mut self,
        cohort: &mut NativeAuthorScopeCohort,
        cursor: &mut usize,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), super::native_author::NativeAudioParameterHandoffError> {
        let next_root = cohort.locals[0].next.identity();
        let local = cohort
            .locals
            .get_mut(*cursor)
            .ok_or(crate::AudioNativeOwnerError::OwnerUnavailable)?;
        if local.current != self.session.native_owner_identity() {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
        }
        self.session.inspect_state_entry_records(
            &mut local.native,
            &StateEntryContext {
                old_root: cohort.old_root,
                next_root,
                next_local: local.next.identity(),
                serial: cohort.old_serial,
                proposal: &cohort.proposal,
                next_plan: local.next.plan(),
                affected: local.affected,
                cancellation,
            },
        )?;
        *cursor += 1;
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.inspect_scope_sources(cohort, cursor, cancellation)?;
            }
        }
        Ok(())
    }

    fn prepare_scope_sources(
        &mut self,
        cohort: &mut NativeAuthorScopeCohort,
        cursor: &mut usize,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), super::native_author::NativeAudioParameterHandoffError> {
        let next_root = cohort.locals[0].next.identity();
        let local = cohort
            .locals
            .get_mut(*cursor)
            .ok_or(crate::AudioNativeOwnerError::OwnerUnavailable)?;
        if local.current != self.session.native_owner_identity() {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
        }
        self.session.prepare_state_entry_records(
            &mut local.native,
            &StateEntryContext {
                old_root: cohort.old_root,
                next_root,
                next_local: local.next.identity(),
                serial: cohort.old_serial,
                proposal: &cohort.proposal,
                next_plan: local.next.plan(),
                affected: local.affected,
                cancellation,
            },
        )?;
        *cursor += 1;
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.prepare_scope_sources(cohort, cursor, cancellation)?;
            }
        }
        Ok(())
    }
    fn revalidate_scope_sources(
        &mut self,
        cohort: &NativeAuthorScopeCohort,
        cursor: &mut usize,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), super::native_author::NativeAudioParameterHandoffError> {
        let local = cohort
            .locals
            .get(*cursor)
            .ok_or(crate::AudioNativeOwnerError::OwnerUnavailable)?;
        if local.current != self.session.native_owner_identity() {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
        }
        self.session.revalidate_state_entry_sources(&local.native, cancellation)?;
        *cursor += 1;
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.revalidate_scope_sources(cohort, cursor, cancellation)?;
            }
        }
        Ok(())
    }
    fn prepare_scope_footprints(
        &self,
        locals: &mut [ScopeLocal],
        cursor: &mut usize,
    ) -> Result<AudioRuntimeResourceFootprint, AudioRuntimeBuildError> {
        let index = *cursor;
        *cursor += 1;
        let local = locals.get(index).ok_or(AudioRuntimeBuildError::InvalidResourceReplacement)?;
        let old = self.session.current_resource_footprint()?;
        let next = local.next.current_resource_footprint()?;
        let mut footprint = replace_weights(self.resource_footprint, old, next)?;
        for source in self.sources.entries.values() {
            if let RuntimeSource::Nested(nested) = source {
                let after = nested.runtime.prepare_scope_footprints(locals, cursor)?;
                footprint =
                    replace_runtime_weights(footprint, nested.runtime.resource_footprint, after)?;
            }
        }
        locals[index].footprint = footprint;
        Ok(footprint)
    }

    pub(super) fn install_native_author_scope_handoff(
        &mut self,
        handle: &PreparedScopeHandle,
        authorization: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<
        super::native_author::NativeAudioParameterHandoffReceipt,
        super::native_author::NativeAudioParameterHandoffError,
    > {
        if cancellation.is_canceled() {
            return Err(crate::NativeAudioAuthorAdmissionError::Cancelled.into());
        }
        let mut cohort = self
            .native_author_scope
            .take()
            .ok_or(crate::AudioNativeOwnerError::OwnerUnavailable)?;
        let result: Result<
            super::native_author::NativeAudioParameterHandoffReceipt,
            super::native_author::NativeAudioParameterHandoffError,
        > = (|| {
            if cohort.installed
                || cohort.old_root != handle.old_root
                || !cohort.proposal.same_retained_proposal(&handle.proposal)
                || self.session.native_owner_identity() != cohort.old_root
                || self.native_author_installation_serial()? != cohort.old_serial
            {
                return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
            }
            let mut cursor = 0;
            self.validate_scope_swap(&cohort.locals, &mut cursor)?;
            if cursor != cohort.locals.len() {
                return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
            }
            let mut selected = cohort.locals.iter().filter_map(|local| {
                local.native.selected_target(&cohort.proposal, cohort.old_serial)
            });
            let selected_record =
                selected.next().ok_or(crate::AudioNativeOwnerError::OwnerUnavailable)?;
            if selected.next().is_some() {
                return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
            }
            // Target generation is replaced with the actual final CAS generation
            // below; this address never grants permission before publication.
            let logical_fee = cohort
                .logical_fee
                .as_mut()
                .ok_or(AudioRuntimeBuildError::StaleResourceAdmission)?;
            let fee_retirement =
                self.prepare_native_plan_fee_retirement(logical_fee, cohort.footprint)?;
            let next_verifier = cohort.destination.clone();
            let commit = authorization.begin_commit(&cohort.destination)?;
            let generation = commit.generation();
            let mut cursor = 0;
            self.swap_scope_sessions(&mut cohort.locals, &mut cursor);
            self.resource_footprint = cohort.footprint;
            logical_fee.retire(fee_retirement);
            cohort.retired_verifier = self.native_installation.verifier.replace(next_verifier);
            let mut cursor = 0;
            self.publish_scope_bindings(&cohort.locals, &mut cursor, generation);
            let claim = commit.commit();
            cohort.retired_claim = self.native_installation.claim.replace(claim.clone());
            self.refresh_native_entry_context();
            cohort.installed = true;
            let mut target = selected_record;
            target.installed_generation = claim.generation();
            Ok(super::native_author::NativeAudioParameterHandoffReceipt {
                claim,
                target,
                unresolved_factory_owners: Some(cohort.unresolved_owners),
                continuity:
                    super::native_author::NativeAudioParameterHandoffContinuity::StateEntryRequired,
            })
        })();
        // Box and retired owners stay live across BOTH final guards, including
        // an installed prefix awaiting native Ack. No destructor under guards.
        self.native_author_scope = Some(cohort);
        result
    }
    fn validate_scope_swap(
        &self,
        locals: &[ScopeLocal],
        cursor: &mut usize,
    ) -> Result<(), super::native_author::NativeAudioParameterHandoffError> {
        let local = locals.get(*cursor).ok_or(crate::AudioNativeOwnerError::OwnerUnavailable)?;
        if local.current != self.session.native_owner_identity() {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
        }
        local.next.validate_transfer(&self.session)?;
        self.session.validate_state_entry_records(&local.native)?;
        *cursor += 1;
        for source in self.sources.entries.values() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.validate_scope_swap(locals, cursor)?
            }
        }
        Ok(())
    }
    fn swap_scope_sessions(&mut self, locals: &mut [ScopeLocal], cursor: &mut usize) {
        locals[*cursor].next.swap_retained(&mut self.session);
        self.resource_footprint = locals[*cursor].footprint;
        *cursor += 1;
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.swap_scope_sessions(locals, cursor);
                nested.begin_continuity();
            }
        }
    }
    fn publish_scope_bindings(
        &mut self,
        locals: &[ScopeLocal],
        cursor: &mut usize,
        serial: NonZeroU64,
    ) {
        self.session.publish_state_entry_records(&locals[*cursor].native, serial);
        *cursor += 1;
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.publish_scope_bindings(locals, cursor, serial)
            }
        }
    }
    pub(super) fn scope_acknowledged(
        &mut self,
        proposal: &mondrian_core::NativeAudioAuthorProposal,
    ) -> bool {
        if self.native_author_scope.as_ref().is_some_and(|cohort| {
            cohort.installed && cohort.proposal.same_retained_proposal(proposal)
        }) {
            // Called outside final guards after exact child Ack. This retirement
            // releases old buffers/registrations, never the moved processors.
            self.native_author_scope = None;
            true
        } else {
            false
        }
    }
}

fn merge_session_weights(
    a: crate::AudioSessionResourceFootprint,
    b: crate::AudioSessionResourceFootprint,
) -> Result<crate::AudioSessionResourceFootprint, AudioRuntimeBuildError> {
    Ok(crate::AudioSessionResourceFootprint {
        prepared_logical_bytes: sum(a.prepared_logical_bytes, b.prepared_logical_bytes)?,
        fixed_resident_bytes: sum(a.fixed_resident_bytes, b.fixed_resident_bytes)?,
        render_scratch_bytes: sum(a.render_scratch_bytes, b.render_scratch_bytes)?,
        processor_session_bytes: sum(a.processor_session_bytes, b.processor_session_bytes)?,
        compensation_delay_bytes: sum(a.compensation_delay_bytes, b.compensation_delay_bytes)?,
        parameter_event_bytes: sum(a.parameter_event_bytes, b.parameter_event_bytes)?,
    })
}

pub(super) fn replace_weights(
    mut current: AudioRuntimeResourceFootprint,
    old: crate::AudioSessionResourceFootprint,
    next: crate::AudioSessionResourceFootprint,
) -> Result<AudioRuntimeResourceFootprint, AudioRuntimeBuildError> {
    current.fixed_resident_bytes = substitute(
        current.fixed_resident_bytes,
        old.fixed_resident_bytes,
        next.fixed_resident_bytes,
    )?;
    current.prepared_logical_bytes = substitute(
        current.prepared_logical_bytes,
        old.prepared_logical_bytes,
        next.prepared_logical_bytes,
    )?;
    current.render_scratch_bytes = substitute(
        current.render_scratch_bytes,
        old.render_scratch_bytes,
        next.render_scratch_bytes,
    )?;
    current.processor_session_bytes = substitute(
        current.processor_session_bytes,
        old.processor_session_bytes,
        next.processor_session_bytes,
    )?;
    current.compensation_delay_bytes = substitute(
        current.compensation_delay_bytes,
        old.compensation_delay_bytes,
        next.compensation_delay_bytes,
    )?;
    current.parameter_event_bytes = substitute(
        current.parameter_event_bytes,
        old.parameter_event_bytes,
        next.parameter_event_bytes,
    )?;
    Ok(current)
}
pub(super) fn replace_runtime_weights(
    current: AudioRuntimeResourceFootprint,
    old: AudioRuntimeResourceFootprint,
    next: AudioRuntimeResourceFootprint,
) -> Result<AudioRuntimeResourceFootprint, AudioRuntimeBuildError> {
    let mut result = current;
    result.fixed_resident_bytes = substitute(
        current.fixed_resident_bytes,
        old.fixed_resident_bytes,
        next.fixed_resident_bytes,
    )?;
    result.prepared_logical_bytes = substitute(
        current.prepared_logical_bytes,
        old.prepared_logical_bytes,
        next.prepared_logical_bytes,
    )?;
    result.render_scratch_bytes = substitute(
        current.render_scratch_bytes,
        old.render_scratch_bytes,
        next.render_scratch_bytes,
    )?;
    result.processor_session_bytes = substitute(
        current.processor_session_bytes,
        old.processor_session_bytes,
        next.processor_session_bytes,
    )?;
    result.compensation_delay_bytes = substitute(
        current.compensation_delay_bytes,
        old.compensation_delay_bytes,
        next.compensation_delay_bytes,
    )?;
    result.parameter_event_bytes = substitute(
        current.parameter_event_bytes,
        old.parameter_event_bytes,
        next.parameter_event_bytes,
    )?;
    Ok(result)
}

#[cfg(all(test, target_os = "windows"))]
#[path = "native_author_scope_fee_test_controls.rs"]
pub(crate) mod fee_test_controls;
