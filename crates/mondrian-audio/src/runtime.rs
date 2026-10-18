pub(crate) mod native_author;
mod native_author_logical_fees;
mod native_author_preflight;
mod native_author_scope;
mod preparation_admission;

use crate::dependency::{
    select_program_window, selected_contribution_source_window, AudioDependencyClosure,
    DependencyWindow,
};
use crate::processor_host::default_processor_resolver;
use crate::schedule::{resolve_channel_mapping, AudioKernelBackend, AudioPreparationDependencies};
use crate::{
    compile_audio_program, AudioCompileRequest, AudioContinuityEpoch, AudioExecutionError,
    AudioPcmSource, AudioProcessorResolver, AudioProgramExecutionDemand, AudioRenderContract,
    AudioRenderRequest, AudioRenderSession, AudioSessionResourceFootprint, AudioStateEntry,
    CompiledAudioSource, PreparedAudioPlan,
};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioComponentEditId, AudioSamplePosition, AudioSampleRate,
    AudioSourceComponentId, ExecutionCancellationToken, ProgramOutputId, SequenceId,
    TimelineTimeRange,
};
use mondrian_timeline::{sequence::MAX_NESTED_SEQUENCE_RENDER_DEPTH, Sequence};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const NESTED_SOURCE_CACHE_FRAMES: usize = 4_096;
const MEDIA_SOURCE_CACHE_FRAMES: usize = 4_096;
const RUNTIME_SOURCE_BINDING_BYTES: usize = 256;
const NESTED_RUNTIME_BINDING_BYTES: usize = 512;

/// Closure-wide hard admission grant for one Audio Program Runtime.
///
/// Per-Session processor/PDC/lookahead limits remain in
/// [`AudioRenderContract`]. This independent grant prevents a nested Signal
/// Closure from multiplying those local limits across many mutable
/// occurrences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioRuntimeResourceGrant {
    /// Maximum generated mutable Runtime occurrences in the nested closure.
    pub max_runtime_occurrences: usize,
    /// Maximum aggregate fixed live Session/source-window residency.
    pub max_fixed_resident_bytes: usize,
    /// Maximum aggregate immutable prepared-plan logical bytes.
    pub max_prepared_logical_bytes: usize,
}

impl AudioRuntimeResourceGrant {
    /// Build an explicit closure-wide hard grant.
    pub const fn new(
        max_runtime_occurrences: usize,
        max_fixed_resident_bytes: usize,
        max_prepared_logical_bytes: usize,
    ) -> Self {
        Self {
            max_runtime_occurrences,
            max_fixed_resident_bytes,
            max_prepared_logical_bytes,
        }
    }

    const fn unbounded() -> Self {
        Self::new(usize::MAX, usize::MAX, usize::MAX)
    }
}

/// Auditable resource footprint of one fully built nested Audio Runtime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioRuntimeResourceFootprint {
    /// Generated mutable Runtime occurrences, not unique Sequence identities.
    pub runtime_occurrences: usize,
    /// Aggregate fixed live residency.
    pub fixed_resident_bytes: usize,
    /// Aggregate immutable prepared-plan logical bytes.
    pub prepared_logical_bytes: usize,
    /// Session node/block render scratch.
    pub render_scratch_bytes: usize,
    /// Processor-declared private Session state.
    pub processor_session_bytes: usize,
    /// PDC delay-line payload.
    pub compensation_delay_bytes: usize,
    /// Sample-accurate processor-event storage.
    pub parameter_event_bytes: usize,
    /// Runtime-owned media source-window payload.
    pub media_window_bytes: usize,
    /// Runtime-owned nested-output window and ordering payload.
    pub nested_window_bytes: usize,
}

/// Hard closure resource whose grant was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRuntimeResourceCategory {
    /// Generated mutable Runtime occurrence count.
    RuntimeOccurrences,
    /// Fixed live Session/source-window residency.
    FixedResidentBytes,
    /// Immutable prepared-plan logical bytes.
    PreparedLogicalBytes,
}

#[derive(Debug, Clone, Copy)]
struct AudioRuntimeAdmissionLedger {
    grant: AudioRuntimeResourceGrant,
    footprint: AudioRuntimeResourceFootprint,
}

impl AudioRuntimeAdmissionLedger {
    fn new(grant: AudioRuntimeResourceGrant) -> Self {
        Self {
            grant,
            footprint: AudioRuntimeResourceFootprint::default(),
        }
    }

    fn reserve_session(
        &mut self,
        sequence_id: SequenceId,
        output_id: ProgramOutputId,
        session: AudioSessionResourceFootprint,
    ) -> Result<(), AudioRuntimeBuildError> {
        self.reserve(
            sequence_id,
            output_id,
            AudioRuntimeResourceFootprint {
                runtime_occurrences: 1,
                fixed_resident_bytes: session.fixed_resident_bytes,
                prepared_logical_bytes: session.prepared_logical_bytes,
                render_scratch_bytes: session.render_scratch_bytes,
                processor_session_bytes: session.processor_session_bytes,
                compensation_delay_bytes: session.compensation_delay_bytes,
                parameter_event_bytes: session.parameter_event_bytes,
                ..AudioRuntimeResourceFootprint::default()
            },
        )
    }

    fn reserve_media_window(
        &mut self,
        sequence_id: SequenceId,
        output_id: ProgramOutputId,
        bytes: usize,
    ) -> Result<(), AudioRuntimeBuildError> {
        let bytes = bytes.checked_add(RUNTIME_SOURCE_BINDING_BYTES).ok_or(
            AudioRuntimeBuildError::ResourceExtentOverflow {
                category: AudioRuntimeResourceCategory::FixedResidentBytes,
                sequence_id,
                output_id,
            },
        )?;
        self.reserve(
            sequence_id,
            output_id,
            AudioRuntimeResourceFootprint {
                fixed_resident_bytes: bytes,
                media_window_bytes: bytes,
                ..AudioRuntimeResourceFootprint::default()
            },
        )
    }

    fn reserve_nested_window(
        &mut self,
        sequence_id: SequenceId,
        output_id: ProgramOutputId,
        bytes: usize,
    ) -> Result<(), AudioRuntimeBuildError> {
        let bytes = bytes
            .checked_add(RUNTIME_SOURCE_BINDING_BYTES)
            .and_then(|bytes| bytes.checked_add(NESTED_RUNTIME_BINDING_BYTES))
            .ok_or(AudioRuntimeBuildError::ResourceExtentOverflow {
                category: AudioRuntimeResourceCategory::FixedResidentBytes,
                sequence_id,
                output_id,
            })?;
        self.reserve(
            sequence_id,
            output_id,
            AudioRuntimeResourceFootprint {
                fixed_resident_bytes: bytes,
                nested_window_bytes: bytes,
                ..AudioRuntimeResourceFootprint::default()
            },
        )
    }

    fn reserve(
        &mut self,
        sequence_id: SequenceId,
        output_id: ProgramOutputId,
        delta: AudioRuntimeResourceFootprint,
    ) -> Result<(), AudioRuntimeBuildError> {
        let next = self.footprint.checked_add(delta).ok_or(
            AudioRuntimeBuildError::ResourceExtentOverflow {
                category: AudioRuntimeResourceCategory::FixedResidentBytes,
                sequence_id,
                output_id,
            },
        )?;
        for (category, required, granted) in [
            (
                AudioRuntimeResourceCategory::RuntimeOccurrences,
                next.runtime_occurrences,
                self.grant.max_runtime_occurrences,
            ),
            (
                AudioRuntimeResourceCategory::FixedResidentBytes,
                next.fixed_resident_bytes,
                self.grant.max_fixed_resident_bytes,
            ),
            (
                AudioRuntimeResourceCategory::PreparedLogicalBytes,
                next.prepared_logical_bytes,
                self.grant.max_prepared_logical_bytes,
            ),
        ] {
            if required > granted {
                return Err(AudioRuntimeBuildError::ResourceGrantExceeded {
                    category,
                    sequence_id,
                    output_id,
                    required,
                    granted,
                });
            }
        }
        self.footprint = next;
        Ok(())
    }
}

impl AudioRuntimeResourceFootprint {
    fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            runtime_occurrences: self.runtime_occurrences.checked_add(other.runtime_occurrences)?,
            fixed_resident_bytes: self
                .fixed_resident_bytes
                .checked_add(other.fixed_resident_bytes)?,
            prepared_logical_bytes: self
                .prepared_logical_bytes
                .checked_add(other.prepared_logical_bytes)?,
            render_scratch_bytes: self
                .render_scratch_bytes
                .checked_add(other.render_scratch_bytes)?,
            processor_session_bytes: self
                .processor_session_bytes
                .checked_add(other.processor_session_bytes)?,
            compensation_delay_bytes: self
                .compensation_delay_bytes
                .checked_add(other.compensation_delay_bytes)?,
            parameter_event_bytes: self
                .parameter_event_bytes
                .checked_add(other.parameter_event_bytes)?,
            media_window_bytes: self.media_window_bytes.checked_add(other.media_window_bytes)?,
            nested_window_bytes: self.nested_window_bytes.checked_add(other.nested_window_bytes)?,
        })
    }

    fn difference(self, before: Self) -> Self {
        Self {
            runtime_occurrences: self
                .runtime_occurrences
                .saturating_sub(before.runtime_occurrences),
            fixed_resident_bytes: self
                .fixed_resident_bytes
                .saturating_sub(before.fixed_resident_bytes),
            prepared_logical_bytes: self
                .prepared_logical_bytes
                .saturating_sub(before.prepared_logical_bytes),
            render_scratch_bytes: self
                .render_scratch_bytes
                .saturating_sub(before.render_scratch_bytes),
            processor_session_bytes: self
                .processor_session_bytes
                .saturating_sub(before.processor_session_bytes),
            compensation_delay_bytes: self
                .compensation_delay_bytes
                .saturating_sub(before.compensation_delay_bytes),
            parameter_event_bytes: self
                .parameter_event_bytes
                .saturating_sub(before.parameter_event_bytes),
            media_window_bytes: self.media_window_bytes.saturating_sub(before.media_window_bytes),
            nested_window_bytes: self
                .nested_window_bytes
                .saturating_sub(before.nested_window_bytes),
        }
    }
}

/// Consumer-owned decoded PCM exposed at the media/source Adapter Seam.
pub trait AudioDecodedSource: Send + Sync + 'static {
    /// Fill one exact native-layout interleaved block on the requested sample grid.
    ///
    /// `destination` is pre-zeroed and has exactly `frames` multiplied by the
    /// resolved source layout's channel count samples.
    /// Implementations must preserve silence outside the source range and return
    /// an error rather than publish a partial or shifted block.
    fn read_interleaved(
        &self,
        start_frame: i64,
        frames: usize,
        destination: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), String>;
}

/// One media dependency bound to native-layout PCM at the requested sample rate.
pub struct ResolvedAudioSource {
    channel_layout: AudioChannelLayout,
    source: Arc<dyn AudioDecodedSource>,
}

impl ResolvedAudioSource {
    /// Bind a decoded source to its exact native semantic signal layout.
    pub fn new(channel_layout: AudioChannelLayout, source: Arc<dyn AudioDecodedSource>) -> Self {
        Self { channel_layout, source }
    }

    /// Exact layout produced by every decoded block.
    pub const fn channel_layout(&self) -> AudioChannelLayout {
        self.channel_layout
    }
}

/// Resolve stable authoring source identities into decoded PCM.
pub trait AudioMediaResolver {
    /// Resolve one media component at the requested rate without changing its layout.
    fn resolve(
        &self,
        asset_id: AssetId,
        component_id: AudioSourceComponentId,
        sample_rate: u32,
    ) -> Result<ResolvedAudioSource, String>;
}

/// Shared compiled runtime used by Playback, Export, Audition, and Analysis.
///
/// The runtime owns one exclusive mutable Session and recursively compiled
/// child-output Sessions. Consumers only supply the media decode Adapter.
pub struct AudioProgramRuntime {
    sequence_id: SequenceId,
    session: AudioRenderSession,
    sources: RuntimeSources,
    execution_demand: AudioProgramExecutionDemand,
    resource_footprint: AudioRuntimeResourceFootprint,
    resource_grant: AudioRuntimeResourceGrant,
    native_installation: crate::native_runtime_owner::NativeRuntimeInstallation,
    native_diagnostic_cursor: usize,
    // Only this exclusive Root enrolls extra logical Plan roles; actual owner
    // receipts independently release them after their field drop glue retires.
    native_held_plan_fees: Option<Arc<std::sync::atomic::AtomicUsize>>,
    native_author_scope: Option<Box<native_author_scope::NativeAuthorScopeCohort>>,
}

impl AudioProgramRuntime {
    #[cfg(test)]
    pub(crate) fn prepare_owned_note_model_for_test(
        &mut self,
        program: Arc<crate::CompiledAudioProgram>,
    ) -> Result<(), AudioRuntimeBuildError> {
        if self.native_installation.verifier.is_some()
            || self.native_installation.claim.is_some()
            || self.native_installation.entry_context.is_some()
            || self.native_installation.prepared.is_some()
            || self.native_author_scope.is_some()
            || !self.session.is_unentered_for_owned_note_control()
            || self
                .sources
                .entries
                .values()
                .any(|source| matches!(source, RuntimeSource::Nested(_)))
        {
            return Err(AudioRuntimeBuildError::InvalidResourceReplacement);
        }
        let old = self.session.resource_plan().session_resource_footprint()?;
        let plan = Arc::new(self.session.resource_plan().with_owned_note_score_for_test(program)?);
        let next = plan.session_resource_footprint()?;
        // Only Note immutable/fixed payload changes. Other categories remain the
        // same prepared base graph, and every original policy ceiling is retained.
        if (
            old.render_scratch_bytes,
            old.processor_session_bytes,
            old.compensation_delay_bytes,
            old.parameter_event_bytes,
        ) != (
            next.render_scratch_bytes,
            next.processor_session_bytes,
            next.compensation_delay_bytes,
            next.parameter_event_bytes,
        ) {
            return Err(AudioRuntimeBuildError::InvalidResourceReplacement);
        }
        let checked =
            |value: Option<usize>| value.ok_or(AudioRuntimeBuildError::InvalidResourceReplacement);
        let peak_fixed = checked(
            self.resource_footprint
                .fixed_resident_bytes
                .checked_add(next.fixed_resident_bytes),
        )?;
        let peak_logical = checked(
            self.resource_footprint
                .prepared_logical_bytes
                .checked_add(next.prepared_logical_bytes),
        )?;
        for (category, required, granted) in [
            (
                AudioRuntimeResourceCategory::FixedResidentBytes,
                peak_fixed,
                self.resource_grant.max_fixed_resident_bytes,
            ),
            (
                AudioRuntimeResourceCategory::PreparedLogicalBytes,
                peak_logical,
                self.resource_grant.max_prepared_logical_bytes,
            ),
        ] {
            if required > granted {
                return Err(AudioRuntimeBuildError::ResourceOverlapGrantExceeded {
                    category,
                    required,
                    granted,
                });
            }
        }
        let final_fixed = checked(
            self.resource_footprint
                .fixed_resident_bytes
                .checked_sub(old.fixed_resident_bytes)
                .and_then(|bytes| bytes.checked_add(next.fixed_resident_bytes)),
        )?;
        let final_logical = checked(
            self.resource_footprint
                .prepared_logical_bytes
                .checked_sub(old.prepared_logical_bytes)
                .and_then(|bytes| bytes.checked_add(next.prepared_logical_bytes)),
        )?;
        // Existing canonical allocator/population; no Enter, Bind, lease or origin.
        let mut storage = AudioRenderSession::allocate_storage(plan)?;
        storage.inherit_native_child_settlement(&self.session);
        let session = storage.populate()?;
        self.session = session;
        self.resource_footprint.fixed_resident_bytes = final_fixed;
        self.resource_footprint.prepared_logical_bytes = final_logical;
        Ok(())
    }

    /// Bind child-settlement evidence to every currently owned actual Session.
    /// This performs no DSP or owner replacement and grants no execution permission.
    /// Missing forwarding permanently invalidates the supplied evidence domain.
    pub fn bind_native_child_settlement(
        &mut self,
        registrar: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<(), mondrian_core::NativeChildSettlementError> {
        self.validate_native_child_settlement_domain(registrar)?;
        self.bind_native_child_settlement_admitted(registrar)
    }

    fn validate_native_child_settlement_domain(
        &self,
        registrar: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<(), mondrian_core::NativeChildSettlementError> {
        self.session.validate_native_child_settlement_domain(registrar)?;
        for source in self.sources.entries.values() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.validate_native_child_settlement_domain(registrar)?;
            }
        }
        Ok(())
    }

    fn bind_native_child_settlement_admitted(
        &mut self,
        registrar: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<(), mondrian_core::NativeChildSettlementError> {
        let mut failure = self.session.bind_native_child_settlement(registrar).err();
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                if let Err(error) = nested.runtime.bind_native_child_settlement_admitted(registrar)
                {
                    failure.get_or_insert(error);
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }

    /// Bind one read-only registration supplied by the actual Media slot owner.
    /// A second unrelated pair cannot replace this Runtime's registration.
    /// This metadata operation performs no foreign native work.
    pub fn bind_native_owner_installation(
        &mut self,
        verifier: mondrian_core::AudioOwnerInstallationVerifier,
    ) -> Result<(), mondrian_core::AudioOwnerInstallationError> {
        match &self.native_installation.verifier {
            Some(current) if current.same_registration(&verifier) => Ok(()),
            Some(_) => Err(mondrian_core::AudioOwnerInstallationError::WrongRegistration),
            None => {
                self.native_installation.verifier = Some(verifier);
                Ok(())
            }
        }
    }

    /// Whether one actual root/nested instance can produce native observations.
    /// This read-only capability performs no native control or DSP work.
    pub fn native_observations_supported(&self) -> bool {
        self.session.native_observation_sources() != 0 || self.sources.entries.values().any(|source|
            matches!(source, RuntimeSource::Nested(nested) if nested.runtime.native_observations_supported()))
    }

    /// Poll one exact occurrence fairly on the sole non-RT serial control owner.
    /// Logs require no editor binding; actual parent/child transport still
    /// authenticates the producer. Other native queues remain unconsumed.
    pub fn poll_native_observations(
        &mut self,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<Option<mondrian_core::NativeAudioObservationBatch>, crate::AudioProcessorHostError>
    {
        let count = self.native_observation_sources()?;
        if count == 0 || cancellation.is_canceled() {
            return Ok(None);
        }
        let index = self.native_diagnostic_cursor % count;
        self.native_diagnostic_cursor = if index + 1 == count { 0 } else { index + 1 };
        let root = self.session.native_owner_identity();
        let installed_serial = self.native_author_installation_serial().ok();
        let result =
            self.poll_native_observation_index(index, root, installed_serial, cancellation)?;
        if result.as_ref().is_some_and(|batch| batch.author.is_some())
            && self.native_author_installation_serial().ok() != installed_serial
        {
            return Err(crate::AudioProcessorHostError::InvalidContract(
                "native author installation changed during observation".to_owned(),
            ));
        }
        Ok(result)
    }

    fn native_observation_sources(&self) -> Result<usize, crate::AudioProcessorHostError> {
        // All counts come from existing finite owner Vecs in this prepared root.
        let mut count = self.session.native_observation_sources();
        for source in self.sources.entries.values() {
            if let RuntimeSource::Nested(nested) = source {
                count = count
                    .checked_add(nested.runtime.native_observation_sources()?)
                    .ok_or_else(|| {
                        crate::AudioProcessorHostError::InvalidContract(
                            "native diagnostic source count overflow".to_owned(),
                        )
                    })?;
            }
        }
        Ok(count)
    }

    fn poll_native_observation_index(
        &mut self,
        mut index: usize,
        root: crate::AudioSessionCursorIdentity,
        installed_serial: Option<std::num::NonZeroU64>,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<Option<mondrian_core::NativeAudioObservationBatch>, crate::AudioProcessorHostError>
    {
        let count = self.session.native_observation_sources();
        if index < count {
            return self.session.poll_native_observation_at(
                index,
                root,
                self.sequence_id,
                installed_serial,
                cancellation,
            );
        }
        index -= count;
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                let count = nested.runtime.native_observation_sources()?;
                if index < count {
                    return nested.runtime.poll_native_observation_index(
                        index,
                        root,
                        installed_serial,
                        cancellation,
                    );
                }
                index -= count;
            }
        }
        Err(crate::AudioProcessorHostError::InvalidContract(
            "native diagnostic root occurrence changed".to_owned(),
        ))
    }

    pub fn validate_native_author_source_closed(
        &mut self,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), crate::NativeAudioAuthorAdmissionError> {
        if cancellation.is_canceled() {
            return Err(crate::NativeAudioAuthorAdmissionError::Cancelled);
        }
        if !self.native_observations_supported() {
            return Ok(());
        }
        let installed_serial = self.native_author_installation_serial()?;
        let root = self.session.native_owner_identity();
        self.validate_native_author_sources_recursively(root, installed_serial, cancellation)?;
        if cancellation.is_canceled() {
            return Err(crate::NativeAudioAuthorAdmissionError::Cancelled);
        }
        // The same private Source claim may be revoked by a real Media slot
        // swap while native observation was in progress. No reply can revive it.
        if self.native_author_installation_serial()? != installed_serial {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
        }
        Ok(())
    }

    fn native_author_installation_serial(
        &self,
    ) -> Result<std::num::NonZeroU64, crate::AudioNativeOwnerError> {
        let verifier = self
            .native_installation
            .verifier
            .as_ref()
            .ok_or(crate::AudioNativeOwnerError::UnregisteredRuntime)?;
        let claim = self
            .native_installation
            .claim
            .as_ref()
            .ok_or(crate::AudioNativeOwnerError::OwnerUnavailable)?;
        verifier.validate(claim).map_err(crate::AudioNativeOwnerError::from)?;
        Ok(claim.generation())
    }

    fn validate_native_author_sources_recursively(
        &mut self,
        root: crate::AudioSessionCursorIdentity,
        installed_serial: std::num::NonZeroU64,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), crate::NativeAudioAuthorAdmissionError> {
        self.session
            .validate_native_author_source_closed(root, installed_serial, cancellation)?;
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.validate_native_author_sources_recursively(
                    root,
                    installed_serial,
                    cancellation,
                )?;
            }
        }
        Ok(())
    }

    /// Read-only registration of the actual current root Runtime.
    /// Wrappers must query it again after a proven same-object Session swap.
    pub fn native_owner_installation_verifier(
        &self,
    ) -> Option<mondrian_core::AudioOwnerInstallationVerifier> {
        self.native_installation.verifier.clone()
    }

    /// Prepare all actual root/nested native bindings without publishing leases.
    ///
    /// Foreign acknowledgements run only on the non-RT serial native owner.
    /// Candidates may prepare before their actual root swap. Failed preparation
    /// withdraws all records; a ticket alone has no installation/author power.
    pub fn prepare_native_owner_bindings(
        &mut self,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<crate::NativeAudioBindingPreparation, crate::AudioNativeOwnerError> {
        if self.native_installation.verifier.is_none() {
            return Err(crate::AudioNativeOwnerError::UnregisteredRuntime);
        }
        if cancellation.is_canceled() {
            return Err(crate::AudioNativeOwnerError::Cancelled);
        }
        let serial = std::num::NonZeroU64::new(self.native_installation.next_preparation_serial)
            .ok_or(crate::AudioNativeOwnerError::PreparationSerialExhausted)?;
        self.native_installation.next_preparation_serial = self
            .native_installation
            .next_preparation_serial
            .checked_add(1)
            .ok_or(crate::AudioNativeOwnerError::PreparationSerialExhausted)?;
        let root = self.session.native_owner_identity();
        self.revoke_native_bindings_recursively();
        let count = match self.prepare_native_bindings_recursively(root, cancellation) {
            Ok(count) => count,
            Err(error) => {
                self.revoke_native_bindings_recursively();
                return Err(error);
            }
        };
        if cancellation.is_canceled() {
            self.revoke_native_bindings_recursively();
            return Err(crate::AudioNativeOwnerError::Cancelled);
        }
        self.native_installation.prepared = Some((serial, root, count));
        Ok(crate::NativeAudioBindingPreparation { root, serial, count })
    }

    /// Publish only this exact preparation after the actual renderer/Runtime swap.
    ///
    /// All foreign work precedes this bounded record move; publication allocates
    /// nothing, resets no DSP and writes no author history. A superseded Core
    /// claim refuses editors and does not roll back an already-valid graph.
    pub fn publish_prepared_native_owner_bindings(
        &mut self,
        preparation: crate::NativeAudioBindingPreparation,
        claim: &mondrian_core::AudioOwnerInstallationClaim,
    ) -> Result<usize, crate::AudioNativeOwnerError> {
        let verifier = self
            .native_installation
            .verifier
            .as_ref()
            .ok_or(crate::AudioNativeOwnerError::UnregisteredRuntime)?;
        verifier.validate(claim)?;
        if self.native_installation.prepared
            != Some((preparation.serial, preparation.root, preparation.count))
            || preparation.root != self.session.native_owner_identity()
        {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable);
        }
        if self.validate_prepared_native_bindings_recursively(preparation.root)?
            != preparation.count
        {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable);
        }
        self.publish_native_bindings_recursively(claim.generation());
        self.native_installation.prepared = None;
        self.native_installation.claim = Some(claim.clone());
        self.refresh_native_entry_context();
        Ok(preparation.count)
    }

    /// Prepare/publish current native bindings on the same serial Media worker.
    /// This initial-install wrapper never grants a candidate permission early.
    pub fn install_native_owner_bindings(
        &mut self,
        claim: &mondrian_core::AudioOwnerInstallationClaim,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<usize, crate::AudioNativeOwnerError> {
        let verifier = self
            .native_installation
            .verifier
            .clone()
            .ok_or(crate::AudioNativeOwnerError::UnregisteredRuntime)?;
        verifier.validate(claim)?;
        let preparation = self.prepare_native_owner_bindings(cancellation)?;
        if cancellation.is_canceled() {
            self.revoke_native_bindings_recursively();
            return Err(crate::AudioNativeOwnerError::Cancelled);
        }
        match self.publish_prepared_native_owner_bindings(preparation, claim) {
            Ok(count) => Ok(count),
            Err(error) => {
                self.revoke_native_bindings_recursively();
                Err(error)
            }
        }
    }

    fn revoke_native_bindings_recursively(&mut self) {
        self.native_installation.claim = None;
        self.native_installation.entry_context = None;
        self.session.set_native_entry_context(None);
        self.native_installation.prepared = None;
        self.session.revoke_native_bindings();
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.revoke_native_bindings_recursively();
            }
        }
    }

    fn prepare_native_bindings_recursively(
        &mut self,
        root: crate::AudioSessionCursorIdentity,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<usize, crate::AudioNativeOwnerError> {
        let mut prepared = self.session.prepare_native_bindings(root, cancellation)?;
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                prepared = prepared
                    .checked_add(
                        nested.runtime.prepare_native_bindings_recursively(root, cancellation)?,
                    )
                    .ok_or(crate::AudioNativeOwnerError::RecordExtentOverflow)?;
            }
        }
        Ok(prepared)
    }

    fn publish_native_bindings_recursively(&mut self, installed_root_serial: std::num::NonZeroU64) {
        self.session.publish_native_bindings(installed_root_serial);
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.publish_native_bindings_recursively(installed_root_serial);
            }
        }
    }

    fn validate_prepared_native_bindings_recursively(
        &self,
        root: crate::AudioSessionCursorIdentity,
    ) -> Result<usize, crate::AudioNativeOwnerError> {
        let mut count = self.session.validate_prepared_native_bindings(root)?;
        for source in self.sources.entries.values() {
            if let RuntimeSource::Nested(nested) = source {
                count = count
                    .checked_add(
                        nested.runtime.validate_prepared_native_bindings_recursively(root)?,
                    )
                    .ok_or(crate::AudioNativeOwnerError::RecordExtentOverflow)?;
            }
        }
        Ok(count)
    }

    /// Observe a lease only in this currently installed actual root Runtime.
    /// Full nested occurrence/epoch must match; address IDs cannot mint a lease.
    pub fn native_owner_lease(
        &self,
        address: crate::AudioNativeProcessorAddress,
    ) -> Result<crate::AudioNativeProcessorOwnerLease, crate::AudioNativeOwnerError> {
        let verifier = self
            .native_installation
            .verifier
            .as_ref()
            .ok_or(crate::AudioNativeOwnerError::UnregisteredRuntime)?;
        let claim = self
            .native_installation
            .claim
            .as_ref()
            .ok_or(crate::AudioNativeOwnerError::OwnerUnavailable)?;
        verifier.validate(claim)?;
        if address.root != self.session.native_owner_identity() {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable);
        }
        let lease = self
            .find_native_owner_lease(address)
            .ok_or(crate::AudioNativeOwnerError::OwnerUnavailable)?;
        if lease.installed_root_serial() != claim.generation() {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable);
        }
        Ok(lease)
    }

    fn find_native_owner_lease(
        &self,
        address: crate::AudioNativeProcessorAddress,
    ) -> Option<crate::AudioNativeProcessorOwnerLease> {
        if address.local == self.session.native_owner_identity() {
            return self.session.native_owner_lease(address);
        }
        self.sources.entries.values().find_map(|source| match source {
            RuntimeSource::Nested(nested) => nested.runtime.find_native_owner_lease(address),
            RuntimeSource::Media(_) => None,
        })
    }

    /// Validate current install, exact Hosted record and actual retained object.
    /// This non-RT control admission executes no DSP and changes no author state.
    pub fn validate_native_owner(
        &mut self,
        lease: crate::AudioNativeProcessorOwnerLease,
    ) -> Result<(), crate::AudioNativeOwnerError> {
        self.observe_native_owner_object(lease).map(|_| ())
    }

    /// Observe the actual object after current installed-owner validation.
    ///
    /// This non-RT control uses the same authenticated observation as owner
    /// validation, without querying parameter metadata or draining author work.
    /// Returned PID/load facts grant no execution, editor or author authority;
    /// every later operation must still admit the current opaque owner lease.
    pub fn observe_native_owner_object(
        &mut self,
        lease: crate::AudioNativeProcessorOwnerLease,
    ) -> Result<crate::NativeAudioObjectObservation, crate::AudioNativeOwnerError> {
        if self.native_owner_lease(lease.address())? != lease {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable);
        }
        let object = self.validate_local_native_owner(lease)?;
        // A Source claim can be superseded while the non-RT native control
        // call is in flight. Do not publish facts under its obsolete authority.
        if self.native_owner_lease(lease.address())? != lease {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable);
        }
        Ok(object)
    }

    fn validate_local_native_owner(
        &mut self,
        lease: crate::AudioNativeProcessorOwnerLease,
    ) -> Result<crate::NativeAudioObjectObservation, crate::AudioNativeOwnerError> {
        if lease.address().local == self.session.native_owner_identity() {
            return self.session.validate_native_owner(lease);
        }
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                match nested.runtime.validate_local_native_owner(lease) {
                    Err(crate::AudioNativeOwnerError::OwnerUnavailable) => {}
                    result => return result,
                }
            }
        }
        Err(crate::AudioNativeOwnerError::OwnerUnavailable)
    }

    /// Return read-only editor address facts from one actual installed record.
    /// Queued copies remain metadata and must be re-admitted before native work.
    pub fn native_editor_target(
        &self,
        address: crate::AudioNativeProcessorAddress,
    ) -> Result<mondrian_core::NativeAudioEditorTarget, crate::AudioNativeOwnerError> {
        self.native_owner_lease(address)
            .map(crate::AudioNativeProcessorOwnerLease::editor_target)
    }

    fn resolve_native_editor_locator(
        &self,
        locator: mondrian_core::NativeAudioEditorLocator,
        root: crate::AudioSessionCursorIdentity,
        installed_serial: std::num::NonZeroU64,
    ) -> Result<Option<mondrian_core::NativeAudioEditorTarget>, crate::AudioNativeOwnerError> {
        let mut found = if self.sequence_id == locator.sequence {
            self.session.resolve_native_editor_target(locator, root, installed_serial)?
        } else {
            None
        };
        for source in self.sources.entries.values() {
            if let RuntimeSource::Nested(nested) = source
                && let Some(target) =
                    nested.runtime.resolve_native_editor_locator(locator, root, installed_serial)?
                && found.replace(target).is_some()
            {
                return Err(crate::AudioNativeOwnerError::OwnerUnavailable);
            }
        }
        Ok(found)
    }

    /// Execute a closed editor intent on this sole installed root and child.
    /// Current claim, exact root/local/occurrence/epoch/binding and actual native
    /// object are checked before SDK work. No DSP, author edit or Undo occurs.
    pub fn control_native_editor(
        &mut self,
        intent: mondrian_core::NativeAudioEditorControlIntent,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<mondrian_core::NativeAudioEditorControlOutcome, crate::AudioNativeOwnerError> {
        use mondrian_core::{
            NativeAudioEditorControlOutcome as Out, NativeAudioEditorControlRefusal as Refusal,
        };
        if cancellation.is_canceled() {
            return Ok(Out::Refused(Refusal::Cancelled));
        }
        let (mut target, title) = match intent {
            mondrian_core::NativeAudioEditorControlIntent::ResolveAndOpen { locator, title } => {
                let serial = match self.native_author_installation_serial() {
                    Ok(serial) => serial,
                    Err(_) => return Ok(Out::Refused(Refusal::NotInstalled)),
                };
                let root = self.session.native_owner_identity();
                let target = match self.resolve_native_editor_locator(locator, root, serial) {
                    Ok(Some(target)) => target,
                    Ok(None) => return Ok(Out::Refused(Refusal::NotInstalled)),
                    Err(crate::AudioNativeOwnerError::OwnerUnavailable) => {
                        return Ok(Out::Refused(Refusal::OwnerChanged))
                    }
                    Err(error) => return Err(error),
                };
                (target, Some(title))
            }
            mondrian_core::NativeAudioEditorControlIntent::Open { target, title } => {
                (target, Some(title))
            }
            mondrian_core::NativeAudioEditorControlIntent::Close { target } => (target, None),
        };
        // Close alone may resolve the exact actual view attachment address.
        // Its private receipt proves same actual object/Session/epoch across
        // arbitrarily many parameter-only Plan replacements;
        // new Open and author callbacks never receive this cleanup permission.
        let retained_close = if title.is_none()
            && let Ok(serial) = self.native_author_installation_serial()
        {
            self.retained_native_close_lease(target, self.session.native_owner_identity(), serial)
        } else {
            None
        };
        if let Some(lease) = retained_close {
            target = lease.editor_target();
        }
        let address = crate::native_runtime_owner::editor_target_address(target);
        let lease = match self.native_owner_lease(address) {
            Ok(lease) if lease.editor_target() == target => lease,
            Ok(_) => return Ok(Out::Refused(Refusal::OwnerChanged)),
            Err(
                crate::AudioNativeOwnerError::OwnerUnavailable
                | crate::AudioNativeOwnerError::UnregisteredRuntime
                | crate::AudioNativeOwnerError::ContinuityUnavailable,
            ) => return Ok(Out::Refused(Refusal::NotInstalled)),
            Err(crate::AudioNativeOwnerError::Installation(_)) => {
                return Ok(Out::Refused(Refusal::OwnerChanged))
            }
            Err(error) => return Err(error),
        };
        let outcome = self.control_local_native_editor(lease, title)?;
        let cancelled = cancellation.is_canceled();
        if cancelled || self.native_owner_lease(address) != Ok(lease) {
            // A UI slot swap can revoke the claim during native attach. Clean
            // that exact old object on this worker before discarding evidence.
            if matches!(outcome, crate::NativeAudioEditorOutcome::Opened { .. }) {
                match self.control_local_native_editor(lease, None)? {
                    crate::NativeAudioEditorOutcome::Closed { .. } => {}
                    _ => return Err(crate::AudioNativeOwnerError::OwnerUnavailable),
                }
            }
            self.revoke_native_bindings_recursively();
            return Ok(Out::Refused(if cancelled {
                Refusal::Cancelled
            } else {
                Refusal::OwnerChanged
            }));
        }
        Ok(crate::native_runtime_owner::project_editor_outcome(
            target, outcome,
        ))
    }

    fn retained_native_close_lease(
        &self,
        previous: mondrian_core::NativeAudioEditorTarget,
        root: crate::AudioSessionCursorIdentity,
        serial: std::num::NonZeroU64,
    ) -> Option<crate::AudioNativeProcessorOwnerLease> {
        if previous.root_session != root.session_id {
            return None;
        }
        if previous.local_session == self.session.native_owner_identity().session_id {
            return self.session.retained_native_close_lease(previous, root, serial);
        }
        self.sources.entries.values().find_map(|source| match source {
            RuntimeSource::Nested(nested) => {
                nested.runtime.retained_native_close_lease(previous, root, serial)
            }
            _ => None,
        })
    }

    fn control_local_native_editor(
        &mut self,
        lease: crate::AudioNativeProcessorOwnerLease,
        title: Option<mondrian_core::NativeAudioEditorTitle>,
    ) -> Result<crate::NativeAudioEditorOutcome, crate::AudioNativeOwnerError> {
        if lease.address().local == self.session.native_owner_identity() {
            return self.session.control_native_editor(lease, title);
        }
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source
                && nested.runtime.find_native_owner_lease(lease.address()) == Some(lease)
            {
                return nested.runtime.control_local_native_editor(lease, title);
            }
        }
        Err(crate::AudioNativeOwnerError::OwnerUnavailable)
    }

    /// Compile, prepare, bind media, and recursively instantiate nested public outputs.
    pub fn build(
        root: &Sequence,
        sequences: &[Sequence],
        resolver: &dyn AudioMediaResolver,
        contract: AudioRenderContract,
        output_id: Option<ProgramOutputId>,
    ) -> Result<Self, AudioRuntimeBuildError> {
        Self::build_with_processor_resolver_and_resource_grant(
            root,
            sequences,
            resolver,
            default_processor_resolver(),
            contract,
            output_id,
            AudioRuntimeResourceGrant::unbounded(),
        )
    }

    /// Build with one closure-wide hard resource grant.
    pub fn build_with_resource_grant(
        root: &Sequence,
        sequences: &[Sequence],
        resolver: &dyn AudioMediaResolver,
        contract: AudioRenderContract,
        output_id: Option<ProgramOutputId>,
        resource_grant: AudioRuntimeResourceGrant,
    ) -> Result<Self, AudioRuntimeBuildError> {
        Self::build_with_processor_resolver_and_resource_grant(
            root,
            sequences,
            resolver,
            default_processor_resolver(),
            contract,
            output_id,
            resource_grant,
        )
    }

    /// Build a root Program from one explicit semantic compile request.
    ///
    /// The request may carry a transient root-Sequence audition overlay.
    /// Nested public outputs always compile their own canonical Program without
    /// inheriting parent Track identities or Session audition state.
    pub fn build_with_compile_request_and_resource_grant(
        root: &Sequence,
        sequences: &[Sequence],
        resolver: &dyn AudioMediaResolver,
        contract: AudioRenderContract,
        request: AudioCompileRequest,
        resource_grant: AudioRuntimeResourceGrant,
    ) -> Result<Self, AudioRuntimeBuildError> {
        Self::build_with_compile_request_processor_resolver_and_resource_grant(
            root,
            sequences,
            resolver,
            default_processor_resolver(),
            contract,
            request,
            resource_grant,
        )
    }

    /// Build a semantic root request using one explicit processor resolver
    /// throughout the complete nested Program closure.
    pub fn build_with_compile_request_processor_resolver_and_resource_grant(
        root: &Sequence,
        sequences: &[Sequence],
        resolver: &dyn AudioMediaResolver,
        processor_resolver: &dyn AudioProcessorResolver,
        contract: AudioRenderContract,
        request: AudioCompileRequest,
        resource_grant: AudioRuntimeResourceGrant,
    ) -> Result<Self, AudioRuntimeBuildError> {
        if contract.channel_layout != root.settings.audio_channel_layout {
            return Err(AudioRuntimeBuildError::ProgramLayoutMismatch {
                sequence_id: root.id,
                authored: root.settings.audio_channel_layout,
                prepared: contract.channel_layout,
            });
        }
        let mut stack = BTreeSet::new();
        let mut ledger = AudioRuntimeAdmissionLedger::new(resource_grant);
        Self::build_inner(
            root,
            sequences,
            resolver,
            processor_resolver,
            contract,
            Some(request),
            None,
            None,
            &mut stack,
            0,
            &mut ledger,
        )
    }

    /// Build with an explicit processor resolver shared by the complete nested closure.
    pub fn build_with_processor_resolver(
        root: &Sequence,
        sequences: &[Sequence],
        resolver: &dyn AudioMediaResolver,
        processor_resolver: &dyn AudioProcessorResolver,
        contract: AudioRenderContract,
        output_id: Option<ProgramOutputId>,
    ) -> Result<Self, AudioRuntimeBuildError> {
        Self::build_with_processor_resolver_and_resource_grant(
            root,
            sequences,
            resolver,
            processor_resolver,
            contract,
            output_id,
            AudioRuntimeResourceGrant::unbounded(),
        )
    }

    /// Build with an explicit processor resolver and closure-wide hard grant.
    pub fn build_with_processor_resolver_and_resource_grant(
        root: &Sequence,
        sequences: &[Sequence],
        resolver: &dyn AudioMediaResolver,
        processor_resolver: &dyn AudioProcessorResolver,
        contract: AudioRenderContract,
        output_id: Option<ProgramOutputId>,
        resource_grant: AudioRuntimeResourceGrant,
    ) -> Result<Self, AudioRuntimeBuildError> {
        if contract.channel_layout != root.settings.audio_channel_layout {
            return Err(AudioRuntimeBuildError::ProgramLayoutMismatch {
                sequence_id: root.id,
                authored: root.settings.audio_channel_layout,
                prepared: contract.channel_layout,
            });
        }
        let mut stack = BTreeSet::new();
        let mut ledger = AudioRuntimeAdmissionLedger::new(resource_grant);
        Self::build_inner(
            root,
            sequences,
            resolver,
            processor_resolver,
            contract,
            output_id.map(AudioCompileRequest::program),
            None,
            None,
            &mut stack,
            0,
            &mut ledger,
        )
    }

    /// Build only the routed Signal Closure audible in one exact public window.
    ///
    /// This is the production seam for selected-range Export. Range selection
    /// happens after semantic output routing but before media binding and
    /// nested Runtime construction, so muted and off-range sources
    /// cannot consume resources or become accidental execution dependencies.
    pub fn build_for_range_with_resource_grant(
        root: &Sequence,
        sequences: &[Sequence],
        resolver: &dyn AudioMediaResolver,
        contract: AudioRenderContract,
        output_id: Option<ProgramOutputId>,
        range: TimelineTimeRange,
        resource_grant: AudioRuntimeResourceGrant,
    ) -> Result<Self, AudioRuntimeBuildError> {
        if contract.channel_layout != root.settings.audio_channel_layout {
            return Err(AudioRuntimeBuildError::ProgramLayoutMismatch {
                sequence_id: root.id,
                authored: root.settings.audio_channel_layout,
                prepared: contract.channel_layout,
            });
        }
        let window = DependencyWindow::from_half_open(range).map_err(AudioExecutionError::from)?;
        let mut stack = BTreeSet::new();
        let mut ledger = AudioRuntimeAdmissionLedger::new(resource_grant);
        Self::build_inner(
            root,
            sequences,
            resolver,
            default_processor_resolver(),
            contract,
            output_id.map(AudioCompileRequest::program),
            Some(window),
            None,
            &mut stack,
            0,
            &mut ledger,
        )
    }

    /// Build one selected-range Runtime from the exact root and nested semantic
    /// Programs frozen at an earlier admission boundary.
    ///
    /// No Program is recompiled or reselected. The frozen occurrence key
    /// includes Sequence, public Output, and exact projected dependency window;
    /// it is deliberately not a coarse Sequence-ID cache.
    pub fn build_from_precompiled_closure_for_range_with_resource_grant(
        root: &Sequence,
        sequences: &[Sequence],
        resolver: &dyn AudioMediaResolver,
        contract: AudioRenderContract,
        output_id: Option<ProgramOutputId>,
        range: TimelineTimeRange,
        prepared_closure: &AudioDependencyClosure,
        resource_grant: AudioRuntimeResourceGrant,
    ) -> Result<Self, AudioRuntimeBuildError> {
        Self::build_from_precompiled_closure_for_range_with_processor_resolver_and_resource_grant(
            root,
            sequences,
            resolver,
            default_processor_resolver(),
            contract,
            output_id,
            range,
            prepared_closure,
            resource_grant,
        )
    }

    /// Build a frozen selected-range closure with an explicit processor
    /// resolver shared by the root and all nested public outputs.
    #[allow(clippy::too_many_arguments)]
    pub fn build_from_precompiled_closure_for_range_with_processor_resolver_and_resource_grant(
        root: &Sequence,
        sequences: &[Sequence],
        resolver: &dyn AudioMediaResolver,
        processor_resolver: &dyn AudioProcessorResolver,
        contract: AudioRenderContract,
        output_id: Option<ProgramOutputId>,
        range: TimelineTimeRange,
        prepared_closure: &AudioDependencyClosure,
        resource_grant: AudioRuntimeResourceGrant,
    ) -> Result<Self, AudioRuntimeBuildError> {
        if contract.channel_layout != root.settings.audio_channel_layout {
            return Err(AudioRuntimeBuildError::ProgramLayoutMismatch {
                sequence_id: root.id,
                authored: root.settings.audio_channel_layout,
                prepared: contract.channel_layout,
            });
        }
        let window = DependencyWindow::from_half_open(range).map_err(AudioExecutionError::from)?;
        let mut stack = BTreeSet::new();
        let mut ledger = AudioRuntimeAdmissionLedger::new(resource_grant);
        Self::build_inner(
            root,
            sequences,
            resolver,
            processor_resolver,
            contract,
            output_id.map(AudioCompileRequest::program),
            Some(window),
            Some(prepared_closure),
            &mut stack,
            0,
            &mut ledger,
        )
    }

    fn build_inner(
        sequence: &Sequence,
        sequences: &[Sequence],
        resolver: &dyn AudioMediaResolver,
        processor_resolver: &dyn AudioProcessorResolver,
        contract: AudioRenderContract,
        compile_request: Option<AudioCompileRequest>,
        selection: Option<DependencyWindow>,
        prepared_closure: Option<&AudioDependencyClosure>,
        stack: &mut BTreeSet<SequenceId>,
        depth: usize,
        ledger: &mut AudioRuntimeAdmissionLedger,
    ) -> Result<Self, AudioRuntimeBuildError> {
        if depth > MAX_NESTED_SEQUENCE_RENDER_DEPTH {
            return Err(AudioRuntimeBuildError::NestedDepthExceeded {
                sequence_id: sequence.id,
                maximum: MAX_NESTED_SEQUENCE_RENDER_DEPTH,
            });
        }
        if !stack.insert(sequence.id) {
            return Err(AudioRuntimeBuildError::NestedCycle(sequence.id));
        }
        let footprint_before = ledger.footprint;
        let result = (|| {
            let compile_request = compile_request
                .or_else(|| {
                    sequence
                        .audio_program
                        .outputs
                        .first()
                        .map(|output| AudioCompileRequest::program(output.id))
                })
                .ok_or(AudioRuntimeBuildError::MissingProgramOutput(sequence.id))?;
            let output_id = compile_request.output_id;
            let mut note_reservation = None;
            let program = if let Some(prepared_closure) = prepared_closure {
                let window = selection.ok_or(AudioRuntimeBuildError::MissingPreparedProgram {
                    sequence_id: sequence.id,
                    output_id,
                })?;
                prepared_closure.program(sequence.id, output_id, window).cloned().ok_or(
                    AudioRuntimeBuildError::MissingPreparedProgram {
                        sequence_id: sequence.id,
                        output_id,
                    },
                )?
            } else if selection.is_none() {
                // Choice A: author/non-Note errors precede checked request and
                // grant refusal; later media/child/contract/endpoint errors
                // follow. Only this direct unwindowed selected-Note family
                // changes priority. There is still no Note execution permit.
                let (program, reservation) =
                    crate::compiler::compile_audio_program_with_note_admission(
                        sequence,
                        compile_request,
                        |graph, notes| {
                            ledger.reserve_compiled_note_plan(sequence.id, output_id, graph, notes)
                        },
                    )?;
                note_reservation = reservation;
                Arc::new(program)
            } else {
                let mut program = compile_audio_program(sequence, compile_request)?;
                if let Some(window) = selection {
                    select_program_window(&mut program, window)
                        .map_err(AudioExecutionError::from)?;
                }
                Arc::new(program)
            };
            if depth == 0 {
                if let Some(closure) = prepared_closure {
                    closure.validate_musical_references(sequence, sequences)?;
                    closure.reject_nested_musical_reference()?;
                } else {
                    crate::dependency::reject_nested_musical_reference(
                        sequence, &program, sequences, selection,
                    )?;
                }
            }
            program.graph().admit_musical_transport(program.musical_time_reference)?;
            // Native Note candidates must be scoped and all targets checked
            // before dependency/factory IO. VST additionally requires Some.
            // Other formats retain their accepted refusal/accounting.
            let admitted_note_reservation = if PreparedAudioPlan::has_native_note_targets(&program)
            {
                PreparedAudioPlan::validate_admitted_root_note_preparation(&program, contract)?;
                if depth != 0 || selection.is_some() || prepared_closure.is_some() {
                    let lane = program
                        .note_lanes
                        .iter()
                        .find(|lane| !lane.notes.is_empty())
                        .expect("observed nonempty native Note candidate");
                    return Err(crate::AudioCompileError::NoteExecutionNotSupported {
                        lane_id: lane.id,
                        processor_id: lane.target.processor_id,
                    }
                    .into());
                }
                Some(
                    note_reservation.take().ok_or(AudioRuntimeBuildError::Compile(
                        crate::AudioCompileError::InvalidPreparedGraph(
                            "admitted root Note preparation has no original ledger receipt"
                                .to_owned(),
                        ),
                    ))?,
                )
            } else {
                None
            };
            // Carry one receipt through descendants and complete it once with
            // the actual common Session footprint. Empty/window/precompiled
            // families retain their existing admission and error boundaries.
            let prepared_reservation = if note_reservation.is_some() {
                note_reservation
            } else if program.note_lanes.iter().all(|lane| lane.notes.is_empty()) {
                Some(ledger.reserve_prepared_plan(
                    sequence.id,
                    output_id,
                    PreparedAudioPlan::requested_base_logical_bytes(&program)?,
                )?)
            } else {
                None
            };
            let execution_demand = program.execution_demand();
            let nested_cache_frames =
                contract.max_block_frames.clamp(1, NESTED_SOURCE_CACHE_FRAMES);
            let mut entries = BTreeMap::new();
            let mut dependencies = AudioPreparationDependencies::default();
            for contribution in program.contributions() {
                let source = match contribution.source {
                    CompiledAudioSource::Media { asset_id, component_id } => {
                        let resolved = resolver
                            .resolve(asset_id, component_id, contract.sample_rate)
                            .map_err(|reason| AudioRuntimeBuildError::Media {
                                asset_id,
                                component_id,
                                reason,
                            })?;
                        let channel_mix = resolve_channel_mapping(
                            &contribution.channel_mapping,
                            resolved.channel_layout,
                            contract.channel_layout,
                        )?;
                        dependencies.insert_source(
                            contribution.edit_id,
                            resolved.channel_layout,
                            channel_mix,
                            0,
                            false,
                        )?;
                        let cache_frames =
                            contract.max_block_frames.clamp(1, MEDIA_SOURCE_CACHE_FRAMES);
                        let cache_samples = cache_frames
                            .checked_mul(resolved.channel_layout.channel_count())
                            .ok_or(AudioRuntimeBuildError::ResourceExtentOverflow {
                                category: AudioRuntimeResourceCategory::FixedResidentBytes,
                                sequence_id: sequence.id,
                                output_id,
                            })?;
                        let cache_bytes = cache_samples
                            .checked_mul(std::mem::size_of::<f32>())
                            .ok_or(AudioRuntimeBuildError::ResourceExtentOverflow {
                                category: AudioRuntimeResourceCategory::FixedResidentBytes,
                                sequence_id: sequence.id,
                                output_id,
                            })?;
                        ledger.reserve_media_window(sequence.id, output_id, cache_bytes)?;
                        RuntimeSource::Media(MediaRuntimeSource {
                            source: resolved.source,
                            cache_start: None,
                            cache_frames,
                            cache: vec![0.0; cache_samples],
                            channel_layout: resolved.channel_layout,
                        })
                    }
                    CompiledAudioSource::NestedOutput { sequence_id, output_id } => {
                        let child = sequences
                            .iter()
                            .find(|candidate| candidate.id == sequence_id)
                            .ok_or(AudioRuntimeBuildError::MissingNestedSequence(sequence_id))?;
                        let child_contract = AudioRenderContract {
                            channel_layout: child.settings.audio_channel_layout,
                            ..contract
                        };
                        let child_selection = match selection {
                            Some(window) => Some(
                                selected_contribution_source_window(contribution, window)
                                    .map_err(AudioExecutionError::from)?
                                    .ok_or(AudioExecutionError::InvalidPreparedSchedule)?,
                            ),
                            None => None,
                        };
                        let runtime = Self::build_inner(
                            child,
                            sequences,
                            resolver,
                            processor_resolver,
                            child_contract,
                            Some(AudioCompileRequest::program(output_id)),
                            child_selection,
                            prepared_closure,
                            stack,
                            depth + 1,
                            ledger,
                        )?;
                        if runtime.requires_state_entry()
                            && contribution.source_time_map.scale.numerator() < 0
                        {
                            return Err(
                                AudioRuntimeBuildError::UnsupportedStatefulNestedDirection(
                                    contribution.edit_id,
                                ),
                            );
                        }
                        let channel_mix = resolve_channel_mapping(
                            &contribution.channel_mapping,
                            child_contract.channel_layout,
                            contract.channel_layout,
                        )?;
                        dependencies.insert_source(
                            contribution.edit_id,
                            child_contract.channel_layout,
                            channel_mix,
                            // A public child Runtime already consumes its own
                            // lookahead and returns Timeline-aligned PCM.
                            0,
                            runtime.requires_state_entry(),
                        )?;
                        let cache_samples = nested_cache_frames
                            .checked_mul(child_contract.channel_count())
                            .ok_or(AudioRuntimeBuildError::ResourceExtentOverflow {
                                category: AudioRuntimeResourceCategory::FixedResidentBytes,
                                sequence_id: sequence.id,
                                output_id,
                            })?;
                        let nested_bytes = cache_samples
                            .checked_mul(std::mem::size_of::<f32>())
                            .and_then(|bytes| {
                                contract
                                    .max_block_frames
                                    .checked_mul(std::mem::size_of::<usize>())
                                    .and_then(|order_bytes| bytes.checked_add(order_bytes))
                            })
                            .ok_or(AudioRuntimeBuildError::ResourceExtentOverflow {
                                category: AudioRuntimeResourceCategory::FixedResidentBytes,
                                sequence_id: sequence.id,
                                output_id,
                            })?;
                        ledger.reserve_nested_window(sequence.id, output_id, nested_bytes)?;
                        RuntimeSource::Nested(NestedRuntimeSource {
                            runtime: Box::new(runtime),
                            cache_start: i64::MIN,
                            cache_frames: nested_cache_frames,
                            cache: vec![0.0; cache_samples],
                            channel_layout: child_contract.channel_layout,
                            next_sample: None,
                            next_epoch: 1,
                            last_demanded_sample: None,
                            ordered_output_frames: Vec::with_capacity(contract.max_block_frames),
                        })
                    }
                };
                entries.insert(contribution.edit_id, source);
            }
            let (plan, session_footprint) = if let Some(reservation) = admitted_note_reservation {
                // This helper consumes the sole local receipt and completes the
                // original ledger once. Do not run ordinary completion again.
                let plan = ledger.prepare_admitted_root_notes(
                    reservation,
                    program,
                    contract,
                    &dependencies,
                    processor_resolver,
                )?;
                let footprint = plan.session_resource_footprint()?;
                (plan, footprint)
            } else {
                let plan = Arc::new(PreparedAudioPlan::prepare_with_dependencies(
                    program,
                    contract,
                    AudioKernelBackend::default(),
                    &dependencies,
                    processor_resolver,
                )?);
                let footprint = plan.session_resource_footprint()?;
                if let Some(reservation) = prepared_reservation {
                    ledger.complete_prepared_session(reservation, footprint)?;
                } else {
                    ledger.reserve_session(sequence.id, output_id, footprint)?;
                }
                (plan, footprint)
            };
            let session = AudioRenderSession::new(plan)?;
            let capacity = session.capacity();
            let realized_compensation_bytes = capacity
                .compensation_delay_samples
                .checked_mul(std::mem::size_of::<f32>())
                .ok_or(AudioRuntimeBuildError::ResourceExtentOverflow {
                    category: AudioRuntimeResourceCategory::FixedResidentBytes,
                    sequence_id: sequence.id,
                    output_id,
                })?;
            if capacity.processor_session_scratch_bytes != session_footprint.processor_session_bytes
                || realized_compensation_bytes != session_footprint.compensation_delay_bytes
            {
                return Err(AudioExecutionError::InvalidPreparedSchedule.into());
            }
            Ok(Self {
                sequence_id: sequence.id,
                session,
                sources: RuntimeSources {
                    entries,
                    cancellation: ExecutionCancellationToken::new(),
                },
                execution_demand,
                resource_footprint: ledger.footprint.difference(footprint_before),
                resource_grant: ledger.grant,
                native_installation:
                    crate::native_runtime_owner::NativeRuntimeInstallation::default(),
                native_diagnostic_cursor: 0,
                native_held_plan_fees: None,
                native_author_scope: None,
            })
        })();
        stack.remove(&sequence.id);
        result
    }

    /// Execute one exact block into caller-owned interleaved float PCM.
    pub fn render_into(
        &mut self,
        request: AudioRenderRequest,
        destination: &mut [f32],
    ) -> Result<(), AudioExecutionError> {
        self.render_into_cancellable(request, destination, &ExecutionCancellationToken::new())
    }

    /// Exact immutable Render Contract shared by the root and delivery Seam.
    pub fn render_contract(&self) -> AudioRenderContract {
        self.session.contract()
    }

    /// Internal lookahead needed to return Timeline-aligned public PCM.
    pub fn public_output_lookahead_frames(&self) -> usize {
        self.session.public_output_lookahead_frames()
    }

    /// Frozen execution demand of the selected root Program Output.
    pub const fn execution_demand(&self) -> AudioProgramExecutionDemand {
        self.execution_demand
    }

    /// Visit existing root/nested materializations in canonical source order.
    ///
    /// Traversal performs no source reads, native calls, reset, allocation or
    /// cursor change. Every child retains its own Plan/Session/grid identity.
    /// Poisoned evidence does not provide a recoverable DSP checkpoint.
    pub fn visit_continuity_evidence(
        &self,
        visitor: &mut impl crate::AudioContinuityEvidenceVisitor,
    ) -> Result<(), AudioExecutionError> {
        self.session.visit_continuity_evidence(visitor)?;
        for source in self.sources.entries.values() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.visit_continuity_evidence(visitor)?;
            }
        }
        Ok(())
    }

    /// Original closure-wide policy ceiling frozen by actual preparation.
    /// Observed footprint and replacement candidates cannot enlarge this grant.
    pub const fn resource_grant(&self) -> AudioRuntimeResourceGrant {
        self.resource_grant
    }

    pub(crate) fn resource_root_identity(&self) -> crate::AudioSessionCursorIdentity {
        self.session.native_owner_identity()
    }

    pub(crate) fn find_resource_session(
        &self,
        identity: crate::AudioSessionCursorIdentity,
    ) -> Option<&AudioRenderSession> {
        if self.session.native_owner_identity() == identity {
            return Some(&self.session);
        }
        for source in self.sources.entries.values() {
            if let RuntimeSource::Nested(nested) = source
                && let Some(session) = nested.runtime.find_resource_session(identity)
            {
                return Some(session);
            }
        }
        None
    }

    pub(crate) fn check_resource_ceiling(
        &self,
        category: AudioRuntimeResourceCategory,
        required: usize,
    ) -> Result<usize, AudioRuntimeBuildError> {
        let required = if category == AudioRuntimeResourceCategory::PreparedLogicalBytes {
            self.include_held_native_plan_logical_bytes(required)?
        } else {
            required
        };
        let granted = match category {
            AudioRuntimeResourceCategory::RuntimeOccurrences => {
                self.resource_grant.max_runtime_occurrences
            }
            AudioRuntimeResourceCategory::FixedResidentBytes => {
                self.resource_grant.max_fixed_resident_bytes
            }
            AudioRuntimeResourceCategory::PreparedLogicalBytes => {
                self.resource_grant.max_prepared_logical_bytes
            }
        };
        if required > granted {
            return Err(AudioRuntimeBuildError::ResourceOverlapGrantExceeded {
                category,
                required,
                granted,
            });
        }
        Ok(required)
    }

    /// Admit prepared ordinary-parameter replacements against the original
    /// root ceiling, retaining old plans until their final retirement frontier.
    ///
    /// New events/lanes and extra complete local owners are observed by actual
    /// capacities, with aliased extra allocations charged once. This resource
    /// result is not complete SDK/native heap evidence or author/slot permission.
    /// Existing source, author, same-object and final installation gates remain.
    pub fn admit_parameter_plan_overlap(
        &self,
        replacements: &[crate::AudioParameterPlanResourceReplacement<'_>],
        extra_owned_allocations: &Vec<crate::NativeHostAllocationBorrow<'_>>,
    ) -> Result<crate::AudioParameterPlanResourceAdmission, AudioRuntimeBuildError> {
        crate::resource_overlap::admit(self, replacements, extra_owned_allocations)
    }

    /// Frozen closure-wide resource evidence for this Runtime occurrence.
    pub const fn resource_footprint(&self) -> AudioRuntimeResourceFootprint {
        self.resource_footprint
    }

    /// Latest successfully completed root Program Output meter block.
    pub fn latest_meter_frame(&self) -> crate::AudioMeterFrame {
        self.session.latest_meter_frame()
    }

    /// Obtain a lock-free root Program Output observation handle.
    pub fn meter_observer(&self) -> crate::AudioMeterObserver {
        self.session.meter_observer()
    }

    /// Whether the selected root/child closure owns mutable continuity state.
    pub fn requires_state_entry(&self) -> bool {
        self.session.requires_state_entry()
    }

    /// Required fresh-entry origin of this actual prepared root Session.
    /// This is immutable score metadata, never permission to enter or process.
    pub fn state_entry_origin(&self) -> Result<Option<AudioSamplePosition>, AudioExecutionError> {
        if !self.session.has_notes() {
            return Ok(None);
        }
        Ok(Some(AudioSamplePosition::new(
            0,
            AudioSampleRate::new(self.session.contract().sample_rate)?,
        )))
    }

    #[cfg(test)]
    pub(crate) fn require_state_entry_recursively_for_test(&mut self) {
        self.session.require_state_entry_for_test();
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.require_state_entry_recursively_for_test();
            }
        }
    }

    /// Enter a fresh root continuity epoch before executing a stateful Plan.
    ///
    /// Child instances enter lazily at the first exact child sample demanded by
    /// the parent time map. Each instance owns its own monotonic epoch sequence.
    pub fn enter_state(&mut self, entry: AudioStateEntry) -> Result<(), AudioExecutionError> {
        self.enter_state_cancellable(entry, &ExecutionCancellationToken::new())
    }

    /// Enter under the actual consumer's cancellation and renderer slot proof.
    /// Child entry remains lazy; standalone DSP never mints native author leases.
    pub fn enter_state_cancellable(
        &mut self,
        entry: AudioStateEntry,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), AudioExecutionError> {
        if cancellation.is_canceled() {
            return Err(crate::AudioNativeOwnerError::Cancelled.into());
        }
        self.reject_nested_note_execution()?;
        // A retired actual slot must refuse before even Preserve/PollPending.
        if let Some(verifier) = &self.native_installation.verifier {
            let claim = verifier.current_claim().map_err(crate::AudioNativeOwnerError::from)?;
            verifier.validate(&claim).map_err(crate::AudioNativeOwnerError::from)?;
        } else if let Some(context) = &self.native_installation.entry_context {
            context.validate(Some(cancellation))?;
        }
        // A VST Note producer is initial-entry only. Reject before any native
        // poll, Source/binding revocation, delay reset or ledger mutation.
        self.session.validate_vst3_note_initial_entry(entry)?;
        self.preflight_native_control()?;
        self.enter_state_admitted(entry, cancellation)
    }

    fn enter_state_admitted(
        &mut self,
        entry: AudioStateEntry,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), AudioExecutionError> {
        self.session.validate_state_entry(entry)?;
        let context = if let Some(verifier) = &self.native_installation.verifier {
            Some(
                crate::native_runtime_owner::NativeStateEntryBindingContext {
                    root: self.session.native_owner_identity(),
                    verifier: verifier.clone(),
                    claim: verifier.current_claim().map_err(crate::AudioNativeOwnerError::from)?,
                },
            )
        } else {
            self.native_installation.entry_context.clone()
        };
        if let Some(context) = &context {
            context.validate(Some(cancellation))?;
        }
        if cancellation.is_canceled() {
            return Err(crate::AudioNativeOwnerError::Cancelled.into());
        }
        // Unsupported score entry must retain every existing installed owner.
        self.session.validate_note_entry_context(entry, context.is_some())?;
        self.revoke_native_bindings_for_entry_recursively();
        self.set_native_entry_context_recursively(context.clone());
        let result = self
            .session
            .enter_state_admitted(entry, Some(cancellation))
            .and_then(|()| {
                self.bind_native_observation_owners_recursively(cancellation)
                    .map_err(AudioExecutionError::from)
            })
            .and_then(|()| {
                context.as_ref().map_or(Ok(()), |context| {
                    context.validate(Some(cancellation)).map_err(AudioExecutionError::from)
                })
            });
        if let Err(error) = result {
            // Accepted partial Bind acknowledgements are never published as a
            // mixed ready prefix when Source/cancellation changed mid-entry.
            self.revoke_native_bindings_recursively();
            return Err(error);
        }
        if self.native_installation.verifier.is_some() {
            self.native_installation.claim = context.as_ref().map(|context| context.claim.clone());
        }
        self.sources.begin_continuity();
        Ok(())
    }

    fn bind_native_observation_owners_recursively(
        &mut self,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), crate::AudioNativeOwnerError> {
        self.session.bind_native_observation_owners(cancellation)?;
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.bind_native_observation_owners_recursively(cancellation)?;
            }
        }
        Ok(())
    }

    fn revoke_native_bindings_for_entry_recursively(&mut self) {
        self.native_installation.claim = None;
        self.native_installation.prepared = None;
        self.session.revoke_native_bindings_for_entry();
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.revoke_native_bindings_for_entry_recursively();
            }
        }
    }

    fn set_native_entry_context_recursively(
        &mut self,
        context: Option<crate::native_runtime_owner::NativeStateEntryBindingContext>,
    ) {
        self.native_installation.entry_context = context.clone();
        self.session.set_native_entry_context(context.clone());
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.set_native_entry_context_recursively(context.clone());
            }
        }
    }

    fn refresh_native_entry_context(&mut self) {
        let context = self
            .native_installation
            .verifier
            .as_ref()
            .zip(self.native_installation.claim.as_ref())
            .map(
                |(verifier, claim)| crate::native_runtime_owner::NativeStateEntryBindingContext {
                    root: self.session.native_owner_identity(),
                    verifier: verifier.clone(),
                    claim: claim.clone(),
                },
            );
        // Existing Source/claim Arc owners are shared metadata, not a new native
        // owner or allocation. Refresh is after the final Source guard released.
        self.set_native_entry_context_recursively(context);
    }

    /// Execute one exact block with consumer-generation cancellation authority.
    pub fn render_into_cancellable(
        &mut self,
        request: AudioRenderRequest,
        destination: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), AudioExecutionError> {
        self.preflight_render_cancellable(request, cancellation)?;
        self.render_into_admitted_cancellable(request, destination, cancellation)
    }

    /// Shared cancellation and whole-root native admission before public-window checks.
    pub(crate) fn preflight_render_cancellable(
        &mut self,
        request: AudioRenderRequest,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), AudioExecutionError> {
        // A canceled public request must never query a foreign processor. The
        // admitted path retains its own check for cancellation after preflight.
        if cancellation.is_canceled() {
            return Err(AudioExecutionError::SourceUnavailable(
                "audio render generation was canceled before execution".to_owned(),
            ));
        }
        self.session.reject_empty_note_request(request)?;
        self.reject_nested_note_execution()?;
        self.preflight_native_control()?;
        self.session.preflight_note_window(request)
    }

    fn reject_nested_note_execution(&self) -> Result<(), AudioExecutionError> {
        for source in self.sources.entries.values() {
            if let RuntimeSource::Nested(nested) = source {
                if nested.runtime.session.has_notes() {
                    return Err(crate::AudioProcessorHostError::NoteExecutionRefused(
                        "nested Note score replay is unsupported",
                    )
                    .into());
                }
                nested.runtime.reject_nested_note_execution()?;
            }
        }
        Ok(())
    }

    fn preflight_native_control(&mut self) -> Result<(), AudioExecutionError> {
        if let Some(hold) = self.native_scope_control_hold() {
            return Err(AudioExecutionError::NativeAuthorPending(Box::new(hold)));
        }
        self.session.preflight_native_control()?;
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.preflight_native_control()?;
            }
        }
        Ok(())
    }

    /// Execute after the caller completes the same whole-root native preflight.
    pub(crate) fn render_into_admitted_cancellable(
        &mut self,
        request: AudioRenderRequest,
        destination: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), AudioExecutionError> {
        if cancellation.is_canceled() {
            return Err(AudioExecutionError::SourceUnavailable(
                "audio render generation was canceled before execution".to_owned(),
            ));
        }
        self.reject_nested_note_execution()?;
        self.session.preflight_note_window(request)?;
        self.sources.cancellation = cancellation.clone();
        let result = self.session.render_into_admitted(
            &mut self.sources,
            request,
            destination,
            Some(cancellation),
        );
        if matches!(result, Err(AudioExecutionError::NativeOwner(_))) {
            self.revoke_native_bindings_recursively();
        }
        result
    }
}

struct RuntimeSources {
    entries: BTreeMap<AudioComponentEditId, RuntimeSource>,
    cancellation: ExecutionCancellationToken,
}

impl RuntimeSources {
    fn begin_continuity(&mut self) {
        for source in self.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.begin_continuity();
            }
        }
    }
}

enum RuntimeSource {
    Media(MediaRuntimeSource),
    Nested(NestedRuntimeSource),
}

struct MediaRuntimeSource {
    source: Arc<dyn AudioDecodedSource>,
    cache_start: Option<i64>,
    cache_frames: usize,
    cache: Vec<f32>,
    channel_layout: AudioChannelLayout,
}

struct NestedRuntimeSource {
    runtime: Box<AudioProgramRuntime>,
    cache_start: i64,
    cache_frames: usize,
    cache: Vec<f32>,
    channel_layout: AudioChannelLayout,
    next_sample: Option<i64>,
    next_epoch: u64,
    last_demanded_sample: Option<i64>,
    ordered_output_frames: Vec<usize>,
}

impl AudioPcmSource for RuntimeSources {
    fn read_indexed_interleaved(
        &mut self,
        edit: AudioComponentEditId,
        source_frames: &[i64],
        channel_layout: AudioChannelLayout,
        destination: &mut [f32],
    ) -> Result<(), AudioExecutionError> {
        let channels = channel_layout.channel_count();
        let expected_samples = source_frames
            .len()
            .checked_mul(channels)
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        if destination.len() != expected_samples {
            return Err(AudioExecutionError::OutputSizeMismatch);
        }
        destination.fill(0.0);
        let cancellation = self.cancellation.clone();
        let source = self.entries.get_mut(&edit).ok_or_else(|| {
            AudioExecutionError::SourceUnavailable(format!("component edit {edit}"))
        })?;
        match source {
            RuntimeSource::Media(media) => {
                media.read_indexed(source_frames, channel_layout, destination, &cancellation)
            }
            RuntimeSource::Nested(nested) => nested.read_indexed(
                edit,
                source_frames,
                channel_layout,
                destination,
                &cancellation,
            ),
        }
    }
}

impl MediaRuntimeSource {
    fn read_indexed(
        &mut self,
        source_frames: &[i64],
        channel_layout: AudioChannelLayout,
        destination: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), AudioExecutionError> {
        if channel_layout != self.channel_layout {
            return Err(AudioExecutionError::SourceUnavailable(format!(
                "decoded media layout contract changed from {:?} to {channel_layout:?}",
                self.channel_layout
            )));
        }
        let channels = channel_layout.channel_count();
        let cache_frames_i64 =
            i64::try_from(self.cache_frames).map_err(|_| AudioExecutionError::BufferTooLarge)?;
        for (output_frame, source_frame) in source_frames.iter().copied().enumerate() {
            if source_frame < 0 {
                continue;
            }
            let cache_start = match self.cache_start.filter(|start| {
                source_frame >= *start && source_frame < start.saturating_add(cache_frames_i64)
            }) {
                Some(start) => start,
                None => {
                    let start =
                        source_frame.div_euclid(cache_frames_i64).saturating_mul(cache_frames_i64);
                    // A failed read can overwrite an earlier cache. Publish
                    // this range only after all its PCM was read successfully.
                    self.cache_start = None;
                    self.cache.fill(0.0);
                    self.source
                        .read_interleaved(start, self.cache_frames, &mut self.cache, cancellation)
                        .map_err(AudioExecutionError::SourceUnavailable)?;
                    if cancellation.is_canceled() {
                        return Err(AudioExecutionError::SourceUnavailable(
                            "audio source read was canceled before cache publication".to_owned(),
                        ));
                    }
                    self.cache_start = Some(start);
                    start
                }
            };
            copy_interleaved_frame(
                &self.cache,
                usize::try_from(source_frame - cache_start)
                    .map_err(|_| AudioExecutionError::BufferTooLarge)?,
                destination,
                output_frame,
                channels,
            )?;
        }
        Ok(())
    }
}

impl NestedRuntimeSource {
    fn begin_continuity(&mut self) {
        self.cache_start = i64::MIN;
        self.next_sample = None;
        self.last_demanded_sample = None;
    }

    fn read_indexed(
        &mut self,
        edit: AudioComponentEditId,
        source_frames: &[i64],
        channel_layout: AudioChannelLayout,
        destination: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), AudioExecutionError> {
        if channel_layout != self.channel_layout {
            return Err(AudioExecutionError::SourceUnavailable(format!(
                "nested audio layout contract changed from {:?} to {channel_layout:?}",
                self.channel_layout
            )));
        }
        let channels = channel_layout.channel_count();
        self.validate_demand_order(edit, source_frames)?;
        if source_frames.len() > self.ordered_output_frames.capacity() {
            return Err(AudioExecutionError::BlockTooLarge);
        }
        self.ordered_output_frames.clear();
        self.ordered_output_frames.extend(source_frames.iter().enumerate().filter_map(
            |(output_frame, source_frame)| (*source_frame >= 0).then_some(output_frame),
        ));
        self.ordered_output_frames
            .sort_unstable_by_key(|output_frame| source_frames[*output_frame]);

        for order_index in 0..self.ordered_output_frames.len() {
            let output_frame = self.ordered_output_frames[order_index];
            let source_frame = source_frames[output_frame];
            self.ensure_cached(source_frame, cancellation)?;
            copy_interleaved_frame(
                &self.cache,
                usize::try_from(source_frame - self.cache_start)
                    .map_err(|_| AudioExecutionError::BufferTooLarge)?,
                destination,
                output_frame,
                channels,
            )?;
        }
        if let Some(last_demanded_sample) =
            source_frames.iter().rev().copied().find(|sample| *sample >= 0)
        {
            self.last_demanded_sample = Some(last_demanded_sample);
        }
        Ok(())
    }

    fn validate_demand_order(
        &self,
        edit: AudioComponentEditId,
        source_frames: &[i64],
    ) -> Result<(), AudioExecutionError> {
        if !self.runtime.requires_state_entry() {
            return Ok(());
        }
        let mut previous = self.last_demanded_sample;
        for sample in source_frames.iter().copied().filter(|sample| *sample >= 0) {
            if previous.is_some_and(|previous| sample < previous) {
                return Err(AudioExecutionError::UnsupportedNestedStateDirection(edit));
            }
            previous = Some(sample);
        }
        Ok(())
    }

    fn ensure_cached(
        &mut self,
        source_frame: i64,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), AudioExecutionError> {
        let cache_frames_i64 =
            i64::try_from(self.cache_frames).map_err(|_| AudioExecutionError::BufferTooLarge)?;
        let cache_end = self.cache_start.checked_add(cache_frames_i64).unwrap_or(i64::MAX);
        if source_frame >= self.cache_start && source_frame < cache_end {
            return Ok(());
        }

        if !self.runtime.requires_state_entry() {
            return self.render_cache(source_frame, cancellation);
        }

        match self.next_sample {
            Some(next_sample) if source_frame >= next_sample => {}
            Some(_) | None => self.enter_child(source_frame, cancellation)?,
        }
        while self.next_sample.is_some_and(|next_sample| source_frame >= next_sample) {
            let start_sample =
                self.next_sample.ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
            self.render_cache(start_sample, cancellation)?;
        }
        Ok(())
    }

    fn enter_child(
        &mut self,
        start_sample: i64,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), AudioExecutionError> {
        let epoch = AudioContinuityEpoch::new(self.next_epoch);
        self.next_epoch = self
            .next_epoch
            .checked_add(1)
            .ok_or(AudioExecutionError::ContinuityEpochExhausted)?;
        self.cache_start = i64::MIN;
        self.next_sample = None;
        self.runtime
            .enter_state_admitted(AudioStateEntry { epoch, start_sample }, cancellation)?;
        self.next_sample = Some(start_sample);
        Ok(())
    }

    fn render_cache(
        &mut self,
        start_sample: i64,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), AudioExecutionError> {
        self.cache_start = i64::MIN;
        self.cache.fill(0.0);
        let result = self.runtime.render_into_admitted_cancellable(
            AudioRenderRequest { start_sample, frames: self.cache_frames },
            &mut self.cache,
            cancellation,
        );
        if let Err(error) = result {
            self.next_sample = None;
            return Err(error);
        }
        self.cache_start = start_sample;
        if self.runtime.requires_state_entry() {
            self.next_sample = Some(
                start_sample
                    .checked_add(
                        i64::try_from(self.cache_frames)
                            .map_err(|_| AudioExecutionError::BufferTooLarge)?,
                    )
                    .ok_or(AudioExecutionError::BufferTooLarge)?,
            );
        }
        Ok(())
    }
}

fn copy_interleaved_frame(
    source: &[f32],
    source_frame: usize,
    destination: &mut [f32],
    destination_frame: usize,
    channels: usize,
) -> Result<(), AudioExecutionError> {
    let source_start =
        source_frame.checked_mul(channels).ok_or(AudioExecutionError::BufferTooLarge)?;
    let source_end =
        source_start.checked_add(channels).ok_or(AudioExecutionError::BufferTooLarge)?;
    let destination_start = destination_frame
        .checked_mul(channels)
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    let destination_end = destination_start
        .checked_add(channels)
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    let source = source.get(source_start..source_end).ok_or_else(|| {
        AudioExecutionError::SourceUnavailable("audio source cache extent is invalid".to_owned())
    })?;
    let destination = destination
        .get_mut(destination_start..destination_end)
        .ok_or(AudioExecutionError::OutputSizeMismatch)?;
    destination.copy_from_slice(source);
    Ok(())
}
/// Failure before a render Session is ready. No partial/fallback graph is returned.
#[derive(Debug, thiserror::Error)]
pub enum AudioRuntimeBuildError {
    /// Prepared overlap no longer belongs to these exact root/local Sessions.
    #[error("prepared audio resource admission was superseded")]
    StaleResourceAdmission,
    /// An ordinary-parameter replacement changes retained execution shape,
    /// duplicates a local Session or lacks its required event storage.
    #[error("invalid prepared audio parameter resource replacement")]
    InvalidResourceReplacement,
    /// The unchanged policy ceiling cannot admit old/new overlapping owners.
    #[error("audio {category:?} overlap requires {required}, grant is {granted}")]
    ResourceOverlapGrantExceeded {
        /// Original resource policy category.
        category: AudioRuntimeResourceCategory,
        /// Required overlapping extent.
        required: usize,
        /// The original frozen policy ceiling.
        granted: usize,
    },
    /// Admission bookkeeping could not acquire its bounded storage.
    #[error("audio resource admission bookkeeping is unavailable")]
    ResourceAdmissionAllocation,
    /// A complete local allocation union could not be admitted.
    #[error(transparent)]
    ResourceOwner(#[from] crate::NativeHostLedgerError),
    /// A selected musical reference requires unsupported nested transport.
    #[error(transparent)]
    Dependency(#[from] crate::AudioDependencyError),
    /// Authoring-to-execution compile failure.
    #[error(transparent)]
    Compile(#[from] crate::AudioCompileError),
    /// Scratch/session preparation failure.
    #[error(transparent)]
    Execution(#[from] AudioExecutionError),
    /// Conservative allocation-footprint calculation overflowed.
    #[error(transparent)]
    ResourceFootprint(#[from] crate::AudioResourceFootprintError),
    /// The complete nested closure exceeds its frozen hard resource grant.
    #[error(
        "audio runtime {category:?} for Sequence {sequence_id} output {output_id} requires {required}, grant is {granted}"
    )]
    ResourceGrantExceeded {
        category: AudioRuntimeResourceCategory,
        sequence_id: SequenceId,
        output_id: ProgramOutputId,
        required: usize,
        granted: usize,
    },
    /// A runtime source/session extent cannot be represented on this target.
    #[error(
        "audio runtime {category:?} extent overflowed for Sequence {sequence_id} output {output_id}"
    )]
    ResourceExtentOverflow {
        category: AudioRuntimeResourceCategory,
        sequence_id: SequenceId,
        output_id: ProgramOutputId,
    },
    /// The selected Sequence has no public output.
    #[error("Sequence {0} has no audio Program Output")]
    MissingProgramOutput(SequenceId),
    /// A Sequence Program must execute in its authored semantic layout; device
    /// or export adaptation belongs after the selected public output.
    #[error(
        "Sequence {sequence_id} audio layout is {authored}, but preparation requested {prepared}"
    )]
    ProgramLayoutMismatch {
        sequence_id: SequenceId,
        authored: AudioChannelLayout,
        prepared: AudioChannelLayout,
    },
    /// The frozen selected-range closure has no exact semantic Program for one
    /// required root or nested occurrence.
    #[error(
        "prepared audio closure is missing Sequence {sequence_id} output {output_id} occurrence"
    )]
    MissingPreparedProgram {
        sequence_id: SequenceId,
        output_id: ProgramOutputId,
    },
    /// A nested author reference cannot be resolved.
    #[error("nested Sequence {0} is unavailable")]
    MissingNestedSequence(SequenceId),
    /// Nested author references form a cycle.
    #[error("cyclic nested Sequence reference at {0}")]
    NestedCycle(SequenceId),
    /// Nested author references exceed the shared preview/export recursion contract.
    #[error("nested Sequence {sequence_id} exceeds maximum render depth {maximum}")]
    NestedDepthExceeded {
        sequence_id: SequenceId,
        maximum: usize,
    },
    /// Generic child processor state has no proven reverse-evaluation contract.
    #[error("nested contribution {0} cannot evaluate stateful audio in reverse")]
    UnsupportedStatefulNestedDirection(AudioComponentEditId),
    /// The consumer media Adapter failed to bind a stable component.
    #[error("audio media component {component_id} of Asset {asset_id} is unavailable: {reason}")]
    Media {
        asset_id: AssetId,
        component_id: AudioSourceComponentId,
        reason: String,
    },
}

#[cfg(test)]
#[path = "runtime/note_preclone_admission_tests.rs"]
mod note_preclone_admission_tests;

#[cfg(test)]
#[path = "runtime/note_public_preparation_tests.rs"]
mod note_public_preparation_tests;
