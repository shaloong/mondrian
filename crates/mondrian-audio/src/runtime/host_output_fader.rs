//! A static terminal Host fader on the retained root Session, at its real frontier.

use super::native_author_logical_fees::LogicalPlanFee;
use super::*;
use crate::processor_host::host_output_fader::PreparedHostFaderBindings;
use crate::render::PreparedNativeAudioParameterPlan;
use crate::NativeHostAllocationOwner;
use std::num::NonZeroU64;

/// Explicit refusal before dependent Source or author publication.
#[derive(Debug, thiserror::Error)]
pub enum AudioHostFaderHandoffError {
    /// Canonical closure differs beyond the admitted terminal scalar.
    #[error(transparent)]
    Compile(#[from] crate::AudioCompileError),
    /// Actual native source, pending work or identity is not admitted.
    #[error(transparent)]
    Admission(#[from] crate::NativeAudioAuthorAdmissionError),
    /// Installed Source/owner is no longer current.
    #[error(transparent)]
    Owner(#[from] crate::AudioNativeOwnerError),
    /// Existing resource grant cannot admit the real simultaneous owners.
    #[error(transparent)]
    Resources(#[from] AudioRuntimeBuildError),
    /// A storage observation was not representable.
    #[error(transparent)]
    Storage(#[from] crate::AudioProcessorFactoryStorageError),
    /// Session is unentered, unprimed, poisoned or has advanced since preparation.
    #[error(transparent)]
    Execution(#[from] crate::AudioExecutionError),
    /// The original registered Source CAS refused publication.
    #[error(transparent)]
    Installation(#[from] mondrian_core::AudioOwnerInstallationError),
    /// First capability excludes nested/held plans and initial binding preparation.
    #[error("Host fader handoff requires a settled direct root")]
    UnsettledRoot,
}

/// Preparation owns real immutable/scratch overlap, never an installed permission.
/// Keep it until all Runtime/metadata/Source locks have ended after installation.
pub struct PreparedHostOutputFaderHandoff {
    frontier: crate::AudioSessionCursorEvidence,
    old_serial: NonZeroU64,
    destination: mondrian_core::AudioOwnerInstallationVerifier,
    plan: PreparedNativeAudioParameterPlan,
    bindings: PreparedHostFaderBindings,
    resources: crate::AudioParameterPlanResourceCommit,
    unresolved_factory_owners: usize,
    retired_claim: Option<mondrian_core::AudioOwnerInstallationClaim>,
    retired_verifier: Option<mondrian_core::AudioOwnerInstallationVerifier>,
    // Last: old Plan and scratch owners must retire before credit is released.
    logical_fee: LogicalPlanFee,
}

/// Actual live execution boundary; no author time or arbitrary DSP checkpoint.
pub struct HostOutputFaderHandoffReceipt {
    /// Current claim from the sole private Media Source publication.
    pub claim: mondrian_core::AudioOwnerInstallationClaim,
    /// Actual old Plan/Session and its unchanged public/execution frontier.
    pub frontier: crate::AudioSessionCursorEvidence,
    /// New Plan identity on the same retained Session.
    pub identity: crate::AudioSessionCursorIdentity,
    /// Count of actual installed records validated and migrated in this publication.
    pub native_bindings: usize,
    /// Opaque/Arc storage boundaries not claimed as complete physical allocations.
    pub unresolved_factory_owners: usize,
}

impl AudioProgramRuntime {
    fn validate_host_fader_root(&self) -> Result<(), AudioHostFaderHandoffError> {
        if self.native_author_scope.is_some()
            || self.native_installation.prepared.is_some()
            || self
                .sources
                .entries
                .values()
                .any(|source| matches!(source, RuntimeSource::Nested(_)))
        {
            return Err(AudioHostFaderHandoffError::UnsettledRoot);
        }
        Ok(())
    }

    /// Prepare only unchanged processors and one static terminal fader. No SDK
    /// Bind, state entry, author mutation, queue discard or claim is performed.
    pub fn prepare_host_output_fader_handoff(
        &mut self,
        next_program: Arc<crate::CompiledAudioProgram>,
        destination: mondrian_core::AudioOwnerInstallationVerifier,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<PreparedHostOutputFaderHandoff, AudioHostFaderHandoffError> {
        if cancellation.is_canceled() {
            return Err(crate::NativeAudioAuthorAdmissionError::Cancelled.into());
        }
        self.validate_host_fader_root()?;
        if self.held_native_plan_logical_bytes() != 0 {
            return Err(AudioHostFaderHandoffError::UnsettledRoot);
        }
        let old_serial = self.native_author_installation_serial()?;
        if !self
            .native_installation
            .verifier
            .as_ref()
            .is_some_and(|old| old.same_source(&destination))
        {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
        }
        let frontier = self.session.host_fader_frontier()?;
        let requested = self
            .session
            .resource_plan()
            .requested_host_output_fader_logical_bytes(&next_program)?;
        let peak = self
            .resource_footprint
            .prepared_logical_bytes
            .checked_add(requested)
            .ok_or(AudioRuntimeBuildError::InvalidResourceReplacement)?;
        self.check_resource_ceiling(AudioRuntimeResourceCategory::PreparedLogicalBytes, peak)?;
        let plan = self.session.prepare_host_output_fader_plan(next_program)?;
        let next = crate::AudioSessionCursorIdentity {
            prepared_plan_id: plan.next_plan().preparation_id(),
            session_id: frontier.identity.session_id,
        };
        let bindings = self.session.prepare_host_fader_bindings(next, old_serial, cancellation)?;
        let (events, lanes) = plan.resource_buffers();
        let replacements = vec![crate::AudioParameterPlanResourceReplacement::new(
            &self.session,
            plan.next_plan(),
            events,
            lanes,
        )];
        let mut extra = vec![
            bindings
                .resource_owner()
                .host_allocation()
                .map_err(AudioRuntimeBuildError::from)?,
            replacements.host_allocation().map_err(AudioRuntimeBuildError::from)?,
        ];
        let mut unresolved = 0usize;
        for plan in [self.session.resource_plan(), plan.next_plan().as_ref()] {
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
        let resources = self
            .admit_parameter_plan_overlap(&replacements, &extra)?
            .into_commit(self, &replacements)?;
        let footprint = super::native_author_scope::replace_weights(
            self.resource_footprint,
            self.session
                .current_resource_footprint()
                .map_err(AudioRuntimeBuildError::from)?,
            plan.resource_footprint().map_err(AudioRuntimeBuildError::from)?,
        )?;
        if footprint != resources.footprint() {
            return Err(AudioRuntimeBuildError::InvalidResourceReplacement.into());
        }
        drop(extra);
        drop(replacements);
        let (old, new) = resources.logical_role_weights();
        if new > requested {
            return Err(AudioRuntimeBuildError::InvalidResourceReplacement.into());
        }
        let logical_fee = self.prepare_native_plan_logical_fee(old, new, footprint)?;
        let mut prepared = PreparedHostOutputFaderHandoff {
            frontier,
            old_serial,
            destination,
            plan,
            bindings,
            resources,
            unresolved_factory_owners: unresolved,
            retired_claim: None,
            retired_verifier: None,
            logical_fee,
        };
        prepared.logical_fee.enroll()?;
        Ok(prepared)
    }

    /// Publish on the exact original Runtime at its still-current frontier.
    /// Caller retains the actual Runtime and metadata locks through the original
    /// author ticket installation. CAS success has no later business refusal.
    pub fn install_host_output_fader_handoff(
        &mut self,
        prepared: &mut PreparedHostOutputFaderHandoff,
        authorization: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<HostOutputFaderHandoffReceipt, AudioHostFaderHandoffError> {
        if cancellation.is_canceled() {
            return Err(crate::NativeAudioAuthorAdmissionError::Cancelled.into());
        }
        self.validate_host_fader_root()?;
        if prepared.retired_claim.is_some()
            || prepared.retired_verifier.is_some()
            || self.native_author_installation_serial()? != prepared.old_serial
            || self.session.host_fader_frontier()? != prepared.frontier
            || prepared.resources.current_root() != prepared.frontier.identity
        {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
        }
        self.session.validate_host_fader_bindings(
            &prepared.bindings,
            prepared.old_serial,
            cancellation,
        )?;
        let footprint = prepared.resources.footprint();
        let retirement =
            self.prepare_native_plan_fee_retirement(&prepared.logical_fee, footprint)?;
        let next_verifier = prepared.destination.clone();
        if cancellation.is_canceled() {
            return Err(crate::NativeAudioAuthorAdmissionError::Cancelled.into());
        }
        let commit = authorization.begin_commit(&prepared.destination)?;
        self.session.swap_parameter_plan(&mut prepared.plan);
        self.resource_footprint = footprint;
        prepared.logical_fee.retire(retirement);
        prepared.retired_verifier = self.native_installation.verifier.replace(next_verifier);
        self.session
            .publish_host_fader_bindings(&prepared.bindings, commit.generation());
        let claim = commit.commit();
        prepared.retired_claim = self.native_installation.claim.replace(claim.clone());
        self.refresh_native_entry_context();
        Ok(HostOutputFaderHandoffReceipt {
            claim,
            frontier: prepared.frontier,
            identity: self.session.native_owner_identity(),
            native_bindings: prepared.bindings.len(),
            unresolved_factory_owners: prepared.unresolved_factory_owners,
        })
    }
}
