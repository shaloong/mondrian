//! One finite, terminal, direct-root same-object native author frontier.
//!
//! This is an owned continuation of the original parameter handoff. It carries
//! no borrowed Runtime or author guard and creates no registry or native command.
//! Its caller serializes one pending prepared window on the actual Source; the
//! logical fee is not a whole-heap reservation for unrelated concurrent work.

use super::*;
use crate::processor_host::native_author::ParameterNativeBindingContext;
use std::alloc::Layout;

/// Owned terminal members, immutable Plan/scratch and original admission receipts.
///
/// Keep this owner on the original producer/retirement path through installation
/// and all member acknowledgements. Dropping it never acknowledges a prefix or
/// undoes an SDK Bind. Before CAS it owns new storage; after CAS it owns retired
/// storage. Source/author supersession never turns it into installation authority.
pub struct PreparedNativeAudioTerminalBatch {
    old_root: crate::AudioSessionCursorIdentity,
    old_serial: NonZeroU64,
    proposals: Vec<NativeAudioAuthorProposal>,
    destination: mondrian_core::AudioOwnerInstallationVerifier,
    local: LocalReplacement,
    resources: crate::AudioParameterPlanResourceCommit,
    unresolved_factory_owners: usize,
    installed_serial: Option<NonZeroU64>,
    retired_claim: Option<mondrian_core::AudioOwnerInstallationClaim>,
    retired_verifier: Option<mondrian_core::AudioOwnerInstallationVerifier>,
    // Last: all member/Plan/scratch/registration owners retire before fee credit.
    logical_fee: LogicalPlanFee,
}

impl PreparedNativeAudioTerminalBatch {
    /// Observe only this prepared owner's exact original member after installation.
    /// The returned Copy facts confer no current Source, object or Ack permission.
    pub fn installed_target(
        &self,
        proposal: &NativeAudioAuthorProposal,
    ) -> Option<mondrian_core::NativeAudioEditorTarget> {
        let serial = self.installed_serial?;
        let member =
            self.proposals.iter().find(|member| member.same_retained_proposal(proposal))?;
        self.local
            .bindings
            .selected_record(member)
            .map(|record| record.editor_target(serial))
    }
}

/// Fixed installation facts; the member collection remains owned by prepared.
pub struct NativeAudioTerminalBatchInstallReceipt {
    /// Actual same-Source claim published once for this whole frontier.
    pub claim: mondrian_core::AudioOwnerInstallationClaim,
    /// This first terminal API only preserves the actual existing Sessions.
    pub continuity: NativeAudioParameterHandoffContinuity,
    /// Same incomplete physical observer qualification as the original handoff.
    pub unresolved_factory_owners: usize,
}

fn overflow() -> AudioRuntimeBuildError {
    AudioRuntimeBuildError::ResourceFootprint(crate::AudioResourceFootprintError::ExtentOverflow)
}

fn add(a: usize, b: usize) -> Result<usize, AudioRuntimeBuildError> {
    a.checked_add(b).ok_or_else(overflow)
}

impl AudioProgramRuntime {
    fn terminal_author_batch_identity(
        &self,
        proposals: &[NativeAudioAuthorProposal],
    ) -> Result<(crate::AudioSessionCursorIdentity, NonZeroU64), NativeAudioParameterHandoffError>
    {
        if proposals.is_empty()
            || proposals.len() > self.session.native_observation_sources()
            || self
                .sources
                .entries
                .values()
                .any(|source| matches!(source, RuntimeSource::Nested(_)))
            || self.native_reprepare.attempt.is_some()
            || self.native_author_scope.is_some()
        {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        let root = self.session.native_owner_identity();
        let serial = self.native_author_installation_serial()?;
        for (index, proposal) in proposals.iter().enumerate() {
            let target = proposal.target();
            if !proposal.terminal()
                || proposal.sequence() != self.sequence_id
                || target.root_plan != root.prepared_plan_id
                || target.root_session != root.session_id
                || target.local_plan != root.prepared_plan_id
                || target.local_session != root.session_id
                || target.installed_generation != serial
                || target.insertion == mondrian_core::NativeAudioEditorInsertion::Scope
                || proposals[..index].iter().any(|prior| prior.target() == target)
            {
                return Err(AudioNativeOwnerError::OwnerUnavailable.into());
            }
            self.session.retain_native_author_proposal(proposal, root, serial)?;
            // Position alone cannot prove preserved execution (e.g. multiple
            // materializations). This is the actual original Runtime policy.
            if self.native_author_state_entry_required(proposal)? {
                return Err(AudioNativeOwnerError::OwnerUnavailable.into());
            }
        }
        Ok((root, serial))
    }

    /// Prepare one complete, finite terminal frontier on the same direct Root.
    ///
    /// Public member facts are never permission: every item must share its actual
    /// private Hosted outstanding record. Omitted/unpolled peers retain all prior
    /// guards. All source checks precede the first Bind; every source is rechecked
    /// after the last Bind. This method performs no author or Source publication.
    /// A caller must retain this unique owner and exclude another prepared or
    /// reprepare window on the same Root until the original retirement completes.
    pub fn prepare_native_author_terminal_batch(
        &mut self,
        programs: &[NativeAudioAuthorProgram],
        proposals: &[NativeAudioAuthorProposal],
        destination: mondrian_core::AudioOwnerInstallationVerifier,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<PreparedNativeAudioTerminalBatch, NativeAudioParameterHandoffError> {
        if cancellation.is_canceled() {
            return Err(NativeAudioAuthorAdmissionError::Cancelled.into());
        }
        let (old_root, old_serial) = self.terminal_author_batch_identity(proposals)?;
        if programs.len() != 1
            || programs[0].sequence != self.sequence_id
            || programs[0].program.output_id() != self.session.resource_plan().program().output_id()
        {
            return Err(NativeAudioParameterHandoffError::ProgramMismatch);
        }
        if !self
            .native_installation
            .verifier
            .as_ref()
            .is_some_and(|current| current.same_source(&destination))
        {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        // Checked, actual-source-count-bounded collection requests precede their
        // allocation. Actual Vec capacities enter the original owner ledger below.
        let requested_members = Layout::array::<NativeAudioAuthorProposal>(proposals.len())
            .map_err(|_| overflow())?
            .size();
        let requested_locators = Layout::array::<NativeAudioEditorLocator>(proposals.len())
            .map_err(|_| overflow())?
            .size();
        self.check_resource_ceiling(
            AudioRuntimeResourceCategory::FixedResidentBytes,
            add(
                self.resource_footprint.fixed_resident_bytes,
                add(requested_members, requested_locators)?,
            )?,
        )?;
        let mut members = Vec::new();
        members
            .try_reserve_exact(proposals.len())
            .map_err(|_| AudioRuntimeBuildError::ResourceAdmissionAllocation)?;
        members.extend(proposals.iter().cloned()); // Share actual bounded content, not opaque-byte copies.
        let mut locators = Vec::new();
        locators
            .try_reserve_exact(proposals.len())
            .map_err(|_| AudioRuntimeBuildError::ResourceAdmissionAllocation)?;
        locators.extend(proposals.iter().map(locator));
        let requested_logical = self
            .session
            .resource_plan()
            .requested_terminal_author_logical_bytes(&programs[0].program, &locators)?;
        self.check_resource_ceiling(
            AudioRuntimeResourceCategory::PreparedLogicalBytes,
            add(
                self.resource_footprint.prepared_logical_bytes,
                requested_logical,
            )?,
        )?;
        self.session.preflight_terminal_author_sources(
            ParameterNativeBindingContext {
                current_root: old_root,
                current_local: old_root,
                next_root: old_root,
                next_local: old_root,
                installed_serial: old_serial,
                proposal: &members[0],
                cancellation,
            },
            &members,
        )?;
        let mut local = LocalReplacement {
            current: old_root,
            plan: Some(self.session.prepare_terminal_author_parameter_plan(
                Arc::clone(&programs[0].program),
                &locators,
            )?),
            bindings: self.session.allocate_parameter_native_bindings(),
            footprint: self.resource_footprint,
        };
        let mut extra = Vec::new();
        let mut unresolved = 0usize;
        extra.push(members.host_allocation().map_err(AudioRuntimeBuildError::from)?);
        extra.push(locators.host_allocation().map_err(AudioRuntimeBuildError::from)?);
        extra.push(
            local
                .bindings
                .resource_owner()
                .host_allocation()
                .map_err(AudioRuntimeBuildError::from)?,
        );
        // Only the owned commit escapes the borrowed overlap-admission views.
        let resources = {
            let plan = local.plan.as_ref().ok_or(AudioNativeOwnerError::OwnerUnavailable)?;
            for actual in std::iter::once(self.session.resource_plan())
                .chain(std::iter::once(plan.next_plan().as_ref()))
            {
                actual.visit_native_host_owned_storage(&mut |owner| {
                    if let Some(allocation) = owner.complete_allocation() {
                        extra.push(allocation);
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
            let (events, lanes) = plan.resource_buffers();
            let replacements = [AudioParameterPlanResourceReplacement::new(
                &self.session,
                plan.next_plan(),
                events,
                lanes,
            )];
            let resources = self
                .admit_parameter_plan_overlap(&replacements, &extra)?
                .into_commit(self, &replacements)?;
            drop(extra);
            resources
        };
        drop(locators);
        let (old_logical, new_logical) = resources.logical_role_weights();
        if new_logical > requested_logical {
            return Err(AudioRuntimeBuildError::InvalidResourceReplacement.into());
        }
        let mut cursor = 0;
        let footprint =
            self.prepare_parameter_footprints(std::slice::from_mut(&mut local), &mut cursor)?;
        if cursor != 1 || footprint != resources.footprint() {
            return Err(AudioRuntimeBuildError::InvalidResourceReplacement.into());
        }
        let next = local.next_identity();
        let context = ParameterNativeBindingContext {
            current_root: old_root,
            current_local: old_root,
            next_root: next,
            next_local: next,
            installed_serial: old_serial,
            proposal: &members[0],
            cancellation,
        };
        self.session
            .prepare_terminal_author_bindings(&mut local.bindings, context, &members)?;
        self.session
            .validate_terminal_author_bindings(&local.bindings, context, &members)?;
        self.session.validate_terminal_parameter_plan_swap(
            local.plan.as_ref().ok_or(AudioNativeOwnerError::OwnerUnavailable)?,
            &members,
            &local.bindings,
        )?;
        let logical_fee =
            self.prepare_native_plan_logical_fee(old_logical, new_logical, footprint)?;
        let mut prepared = PreparedNativeAudioTerminalBatch {
            old_root,
            old_serial,
            proposals: members,
            destination,
            local,
            resources,
            unresolved_factory_owners: unresolved,
            installed_serial: None,
            retired_claim: None,
            retired_verifier: None,
            logical_fee,
        };
        prepared.logical_fee.enroll()?;
        Ok(prepared)
    }

    /// Publish exactly one already prepared terminal frontier using the original
    /// same-Source authorization. All SDK checks occur before the Source guard;
    /// the guard contains no allocation, SDK call, owner Drop or fallible Result.
    /// This is not a canonical author publication or a native acknowledgement.
    pub fn install_native_author_terminal_batch(
        &mut self,
        prepared: &mut PreparedNativeAudioTerminalBatch,
        authorization: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<NativeAudioTerminalBatchInstallReceipt, NativeAudioParameterHandoffError> {
        self.install_native_author_terminal_batch_with_commit_guard(
            prepared,
            authorization,
            cancellation,
            || Ok(()),
        )
        .map(|(receipt, ())| receipt)
    }

    /// Recheck under the caller's original Runtime mutex, then acquire its real
    /// canonical commit guard. SDK calls and allocation precede acquire; one
    /// final atomic cancellation check follows it before Source commit. The
    /// returned guard remains alive for the adapter's same
    /// metadata publication; it must end before native Ack or owner retirement.
    /// A failed acquire/Source CAS leaves prepared and every prefix with its owner.
    pub fn install_native_author_terminal_batch_with_commit_guard<G>(
        &mut self,
        prepared: &mut PreparedNativeAudioTerminalBatch,
        authorization: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
        cancellation: &ExecutionCancellationToken,
        acquire: impl FnOnce() -> Result<G, NativeAudioParameterHandoffError>,
    ) -> Result<(NativeAudioTerminalBatchInstallReceipt, G), NativeAudioParameterHandoffError> {
        if cancellation.is_canceled() {
            return Err(NativeAudioAuthorAdmissionError::Cancelled.into());
        }
        if prepared.installed_serial.is_some()
            || prepared.retired_claim.is_some()
            || prepared.retired_verifier.is_some()
        {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        let (root, serial) = self.terminal_author_batch_identity(&prepared.proposals)?;
        if root != prepared.old_root
            || serial != prepared.old_serial
            || prepared.resources.current_root() != root
            || prepared.local.current != root
        {
            return Err(AudioNativeOwnerError::OwnerUnavailable.into());
        }
        let next = prepared.local.next_identity();
        let context = ParameterNativeBindingContext {
            current_root: root,
            current_local: root,
            next_root: next,
            next_local: next,
            installed_serial: serial,
            proposal: &prepared.proposals[0],
            cancellation,
        };
        self.session.validate_terminal_author_bindings(
            &prepared.local.bindings,
            context,
            &prepared.proposals,
        )?;
        self.session.validate_terminal_parameter_plan_swap(
            prepared.local.plan.as_ref().ok_or(AudioNativeOwnerError::OwnerUnavailable)?,
            &prepared.proposals,
            &prepared.local.bindings,
        )?;
        for member in &prepared.proposals {
            if prepared.local.bindings.selected_record(member).is_none() {
                return Err(AudioNativeOwnerError::OwnerUnavailable.into());
            }
        }
        let footprint = prepared.resources.footprint();
        if prepared.local.footprint != footprint {
            return Err(AudioRuntimeBuildError::InvalidResourceReplacement.into());
        }
        let fee_retirement =
            self.prepare_native_plan_fee_retirement(&prepared.logical_fee, footprint)?;
        let next_verifier = prepared.destination.clone();
        if cancellation.is_canceled() {
            return Err(NativeAudioAuthorAdmissionError::Cancelled.into());
        }
        let guard = acquire()?;
        // Acquiring a real author fence may block or cancel this work. The
        // Source CAS, not the preceding SDK preflight, is the irreversible seam.
        if cancellation.is_canceled() {
            return Err(NativeAudioAuthorAdmissionError::Cancelled.into());
        }
        let commit = authorization.begin_commit(&prepared.destination)?;
        let mut cursor = 0;
        self.swap_parameter_plans(std::slice::from_mut(&mut prepared.local), &mut cursor);
        prepared.logical_fee.retire(fee_retirement);
        prepared.retired_verifier = self.native_installation.verifier.replace(next_verifier);
        let generation = commit.generation();
        self.session
            .publish_parameter_native_bindings(&prepared.local.bindings, generation);
        let claim = commit.commit();
        prepared.retired_claim = self.native_installation.claim.replace(claim.clone());
        prepared.installed_serial = Some(generation);
        self.refresh_native_entry_context();
        Ok((
            NativeAudioTerminalBatchInstallReceipt {
                claim,
                continuity: NativeAudioParameterHandoffContinuity::Preserved,
                unresolved_factory_owners: prepared.unresolved_factory_owners,
            },
            guard,
        ))
    }
}

#[cfg(test)]
mod transfer_contract {
    use super::*;

    #[test]
    fn prepared_terminal_batch_is_owned_send_sync_without_runtime_borrow() {
        fn owned<T: Send + Sync + 'static>() {}
        owned::<PreparedNativeAudioTerminalBatch>();
        owned::<PreparedNativeAudioParameterHandoff>();
    }
}
