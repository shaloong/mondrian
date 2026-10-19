//! Supervised process isolation for untrusted audio processor Adapters.
//!
//! The parent retains Audio Program authority and exchanges only one admitted
//! processor block through fixed Session-owned shared storage. Native plugin
//! discovery and ABI loading live behind the Worker factory Interface; they do
//! not enter Timeline compilation or the in-process Processor Host.

mod clap_audio_role;
mod clap_discovery;
#[cfg(windows)]
mod clap_gui_owner;
mod clap_gui_requests;
mod clap_main_services;
mod clap_metadata;
mod clap_note_input_contract;
mod clap_note_input_metadata;
pub use clap_discovery::probe_clap_note_input_contract;
pub use clap_discovery::probe_clap_note_processing_offer;
pub use clap_note_input_contract::ClapNoteInputContractProbe;
pub use clap_note_input_contract::ClapNoteProcessingOfferV5;
/// Bounded typed ownership of the actual CLAP output callbacks.
/// These source facts do not grant an author Session or installed runtime lease.
pub mod clap_output_events;
mod clap_parameter_events;
mod clap_parameter_prefix;
pub mod clap_parameter_source;
mod clap_signal_fields;
mod clap_worker;
mod native_audio_author_state_entry_wire;
mod native_audio_author_wire;
mod native_audio_editor_wire;
mod native_audio_log;
mod native_audio_object_wire;
mod native_audio_parameter_wire;
mod native_configuration_control;
mod native_configuration_seed;
mod native_configuration_seed_wire;
mod native_configuration_wire;
mod native_request_cohort;
mod native_worker_command;
mod native_worker_lane_wire;
mod protocol;
mod supervisor;
mod vst3_catalog;
mod vst3_discovery;
mod vst3_processing_metadata;
mod vst3_worker;
mod worker;

#[cfg(test)]
mod tests;

use crate::{
    AudioParameterEventBatch, AudioProcessingMode, AudioProcessor, AudioProcessorAudioIo,
    AudioProcessorAuxiliaryInputContract, AudioProcessorAuxiliaryOutputContract,
    AudioProcessorExecutionContract, AudioProcessorFactory, AudioProcessorHostError,
    AudioProcessorPrepareRequest, AudioProcessorProcessContext, AudioProcessorResolver,
    AudioRenderContract, BuiltInAudioProcessorResolver,
};
use mondrian_core::{AudioChannelLayout, ParameterId};
use mondrian_timeline::audio::AudioProcessorDefinitionRef;
use protocol::{
    CommandKind, SharedBlockLayout, SharedNoteEnd, SharedNoteEvent, SharedParameterEvent,
    WorkerCommand, MAX_CONTROL_FRAME_BYTES, MAX_PREPARATION_PAYLOAD_BYTES,
};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use supervisor::SupervisedAudioWorker;

pub use native_configuration_seed::{
    NativeConfigurationOwnedSeed, NativeConfigurationSeedOutcome, NativeConfigurationSeedReceipt,
    NativeConfigurationSeedRefusal, NativeConfigurationSeedRequest, NativeConfigurationSeedTicket,
};

pub use native_configuration_control::{
    NativeConfigurationCandidate, NativeConfigurationControlOutcome,
    NativeConfigurationControlRequest, NativeConfigurationFormat, NativeConfigurationRefusal,
    NativeControlBufferReceipt, NativeObservedRequest, NativeRequestCohort,
};

pub use clap_discovery::{
    probe_clap_note_input_metadata, probe_clap_plugin_registration, run_clap_discovery_worker,
    scan_clap_library_descriptors, ClapParameterDescriptor, ClapPluginDescriptor,
    CLAP_DISCOVERY_WORKER_ARGUMENT,
};
pub use clap_note_input_metadata::{
    ClapNoteExecution, ClapNoteInputDescriptor, ClapNoteInputMetadata,
    ClapNotePreferredDialectStatus,
};
pub use clap_worker::{
    ClapAudioProcessorSpecResolver, ClapAudioProcessorWorkerFactory, ClapMainAudioInput,
    ClapPluginRegistration, DiscoveredClapAudioProcessorSpecResolver,
    InstalledClapAudioProcessorSpecResolver,
};
pub use vst3_catalog::{
    DiscoveredVst3AudioProcessorSpecResolver, InstalledVst3AudioProcessorSpecResolver,
};
pub use vst3_discovery::{
    probe_vst3_plugin_registration, run_vst3_discovery_worker, scan_vst3_binary_descriptors,
    Vst3PluginDescriptor, VST3_DISCOVERY_WORKER_ARGUMENT,
};
pub use vst3_processing_metadata::{
    Vst3EventBusContractRow, Vst3EventBusContractRows, Vst3EventBusContractV1, Vst3EventBusRole,
};
pub use vst3_worker::{
    Vst3AudioProcessorSpecResolver, Vst3AudioProcessorWorkerFactory, Vst3MainAudioInput,
    Vst3ParameterDescriptor, Vst3PluginRegistration, VST3_AUDIO_WORKER_ARGUMENT,
};

/// Hidden endpoint environment used only between a parent Adapter and its Worker.
pub const ISOLATED_AUDIO_PROCESSOR_ENDPOINT_ENV: &str =
    "MONDRIAN_INTERNAL_AUDIO_PROCESSOR_ENDPOINT";
/// Hidden nonce environment paired with [`ISOLATED_AUDIO_PROCESSOR_ENDPOINT_ENV`].
pub const ISOLATED_AUDIO_PROCESSOR_NONCE_ENV: &str = "MONDRIAN_INTERNAL_AUDIO_PROCESSOR_NONCE";
/// Recommended product-executable dispatch argument for an audio Processor Worker.
pub const ISOLATED_AUDIO_PROCESSOR_WORKER_ARGUMENT: &str = "--internal-audio-processor-worker-v1";

pub(super) struct NativeAudioInputMetadata {
    pub(super) contract: AudioProcessorAuxiliaryInputContract,
    pub(super) names: Vec<crate::AudioProcessorInputName>,
}

#[derive(Clone)]
enum PreparationEvidence {
    Unprobed,
    NativeProbed {
        input_names: Vec<crate::AudioProcessorInputName>,
    },
}

/// Immutable launch and capacity facts for one process-isolated processor definition.
#[derive(Clone)]
pub struct IsolatedAudioProcessorWorkerSpec {
    musical_time_reference: Option<mondrian_core::MusicalTimeReference>,
    helper_executable: PathBuf,
    helper_arguments: Vec<OsString>,
    dispatch_argument: Option<OsString>,
    preparation_payload: Vec<u8>,
    preparation_evidence: PreparationEvidence,
    plugin_execution_contract: AudioProcessorExecutionContract,
    auxiliary_inputs: AudioProcessorAuxiliaryInputContract,
    auxiliary_outputs: AudioProcessorAuxiliaryOutputContract,
    note_input_contract: crate::AudioProcessorNoteInputContract,
    parameter_ids: Vec<ParameterId>,
    render_contract: AudioRenderContract,
    startup_timeout: Duration,
    state_entry_timeout: Duration,
    operation_timeout: Duration,
    native_lane: Option<crate::NativeWorkerLaneResourceProfile>,
}

impl IsolatedAudioProcessorWorkerSpec {
    /// Construct one exact Worker request.
    ///
    /// `preparation_payload` is opaque to Mondrian and interpreted only by the
    /// concrete child-side native Adapter. Its size is bounded before process
    /// creation. Parameter identities must use the same stable order as the
    /// prepared processor's event lanes.
    pub fn new(
        helper_executable: PathBuf,
        preparation_payload: Vec<u8>,
        plugin_execution_contract: AudioProcessorExecutionContract,
        auxiliary_inputs: AudioProcessorAuxiliaryInputContract,
        parameter_ids: Vec<ParameterId>,
        render_contract: AudioRenderContract,
    ) -> Result<Self, AudioProcessorHostError> {
        let operation_timeout = match render_contract.processing_mode {
            AudioProcessingMode::Realtime => Duration::from_millis(100),
            AudioProcessingMode::Offline => Duration::from_secs(30),
        };
        let spec = Self {
            musical_time_reference: None,
            helper_executable,
            helper_arguments: Vec::new(),
            dispatch_argument: Some(OsString::from(ISOLATED_AUDIO_PROCESSOR_WORKER_ARGUMENT)),
            preparation_payload,
            preparation_evidence: PreparationEvidence::Unprobed,
            plugin_execution_contract,
            auxiliary_inputs,
            auxiliary_outputs: AudioProcessorAuxiliaryOutputContract::default(),
            note_input_contract: crate::AudioProcessorNoteInputContract::default(),
            parameter_ids,
            render_contract,
            startup_timeout: Duration::from_secs(5),
            state_entry_timeout: Duration::from_secs(5),
            operation_timeout,
            native_lane: None,
        };
        spec.validate()?;
        Ok(spec)
    }

    // Only a fresh format-specific native probe may supply this evidence. It is
    // consumed into the Factory's existing presentation snapshot, not a second
    // retained owner or permission to skip the actual instance's Ready checks.
    pub(crate) fn with_native_probe_input_names(
        mut self,
        input_names: Vec<crate::AudioProcessorInputName>,
    ) -> Result<Self, AudioProcessorHostError> {
        crate::AudioProcessorInputName::validate_all(&input_names, &self.auxiliary_inputs)?;
        self.preparation_evidence = PreparationEvidence::NativeProbed { input_names };
        Ok(self)
    }

    /// Prepend helper-specific arguments before the optional dispatch argument.
    ///
    /// This supports dedicated host executables and test harnesses without
    /// putting native plugin paths or state on the process command line.
    pub fn with_helper_arguments(mut self, arguments: Vec<OsString>) -> Self {
        self.helper_arguments = arguments;
        self
    }

    /// Bind the owning Sequence's immutable musical reference, separate from
    /// the opaque plugin configuration and any native capability declaration.
    pub fn with_musical_time_reference(
        mut self,
        reference: Option<mondrian_core::MusicalTimeReference>,
    ) -> Result<Self, AudioProcessorHostError> {
        crate::processor::musical_tempo(reference)?;
        self.musical_time_reference = reference;
        Ok(self)
    }

    /// Override or remove the standard product dispatch argument.
    pub fn with_dispatch_argument(mut self, argument: Option<OsString>) -> Self {
        self.dispatch_argument = argument;
        self
    }

    /// Bind the exact restored input-port and fixed Note capacity contract.
    pub fn with_note_input_contract(
        mut self,
        contract: crate::AudioProcessorNoteInputContract,
    ) -> Result<Self, AudioProcessorHostError> {
        contract.validate()?;
        self.note_input_contract = contract;
        self.validate()?;
        Ok(self)
    }

    /// Bind every additional output, including currently unrouted native ports.
    pub fn with_auxiliary_output_contract(
        mut self,
        contract: AudioProcessorAuxiliaryOutputContract,
    ) -> Result<Self, AudioProcessorHostError> {
        contract.validate(self.render_contract.channel_layout)?;
        if contract.buses.len() > 31 {
            return Err(invalid_contract(
                "isolated output contract exceeds 31 auxiliary ports",
            ));
        }
        self.auxiliary_outputs = contract;
        self.validate()?;
        Ok(self)
    }

    /// Declare the concrete adapter's measured additional native lane request.
    /// The sole Session grant remains authority; the child must match this exact
    /// request before native load and compare the actual retained Worker Box
    /// header and created roles before Ready. Nested cold allocations are separate.
    pub fn with_native_worker_lane_resources(
        mut self,
        profile: Option<crate::NativeWorkerLaneResourceProfile>,
    ) -> Self {
        self.native_lane = profile;
        self
    }

    /// Override bounded startup and per-operation deadlines.
    pub fn with_deadlines(
        mut self,
        startup_timeout: Duration,
        operation_timeout: Duration,
    ) -> Result<Self, AudioProcessorHostError> {
        self.startup_timeout = startup_timeout;
        self.operation_timeout = operation_timeout;
        self.validate()?;
        Ok(self)
    }

    /// Bound a discontinuity reset separately from one realtime audio block.
    pub fn with_state_entry_timeout(
        mut self,
        timeout: Duration,
    ) -> Result<Self, AudioProcessorHostError> {
        self.state_entry_timeout = timeout;
        self.validate()?;
        Ok(self)
    }

    fn validate(&self) -> Result<(), AudioProcessorHostError> {
        crate::processor::musical_tempo(self.musical_time_reference)?;
        if self.helper_executable.as_os_str().is_empty()
            || self.preparation_payload.len() > MAX_PREPARATION_PAYLOAD_BYTES
            || self.render_contract.sample_rate == 0
            || self.render_contract.max_block_frames == 0
            || self.startup_timeout.is_zero()
            || self.state_entry_timeout.is_zero()
            || self.operation_timeout.is_zero()
            || self.parameter_ids.len() > 4096
        {
            return Err(invalid_contract(
                "isolated Worker path, payload, render extent, deadlines, or parameter capacity is invalid",
            ));
        }
        self.auxiliary_inputs.validate(self.render_contract.channel_layout)?;
        self.auxiliary_outputs.validate(self.render_contract.channel_layout)?;
        if self.auxiliary_outputs.buses.len() > 31 {
            return Err(invalid_contract(
                "isolated output contract exceeds 31 auxiliary ports",
            ));
        }
        self.note_input_contract.validate()?;
        if let PreparationEvidence::NativeProbed { input_names } = &self.preparation_evidence {
            crate::AudioProcessorInputName::validate_all(input_names, &self.auxiliary_inputs)?;
        }
        let mut parameter_ids = BTreeSet::new();
        if self
            .parameter_ids
            .iter()
            .any(|parameter_id| !parameter_ids.insert(parameter_id))
        {
            return Err(invalid_contract(
                "isolated Worker parameter identities must be unique",
            ));
        }
        if !self.plugin_execution_contract.admits(self.render_contract.processing_mode) {
            return Err(invalid_contract(
                "isolated Worker processor does not admit the requested processing mode",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for IsolatedAudioProcessorWorkerSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IsolatedAudioProcessorWorkerSpec")
            .field("helper_executable", &self.helper_executable)
            .field("plugin_execution_contract", &self.plugin_execution_contract)
            .field("auxiliary_inputs", &self.auxiliary_inputs)
            .field("auxiliary_outputs", &self.auxiliary_outputs)
            .field("note_input_contract", &self.note_input_contract)
            .field("parameter_count", &self.parameter_ids.len())
            .field("render_contract", &self.render_contract)
            .field("startup_timeout", &self.startup_timeout)
            .field("state_entry_timeout", &self.state_entry_timeout)
            .field("operation_timeout", &self.operation_timeout)
            .finish_non_exhaustive()
    }
}

/// Non-realtime Adapter that resolves one native definition into an isolated Worker spec.
pub trait IsolatedAudioProcessorSpecResolver: Send + Sync {
    /// Explicit cold-probe forwarding capability; unknown is never no-Child evidence.
    fn native_child_settlement_requirement(
        &self,
    ) -> mondrian_core::NativeChildSettlementRequirement {
        mondrian_core::NativeChildSettlementRequirement::Unavailable
    }

    /// Resolve discovery identity, native binary, opaque state, buses, parameters,
    /// deadlines, and child preparation payload without loading plugin code in
    /// the Mondrian process.
    fn resolve(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<IsolatedAudioProcessorWorkerSpec, AudioProcessorHostError>;
}

/// One resolver that keeps built-ins local and forces VST3/CLAP through isolation.
pub struct IsolatedAudioProcessorResolver {
    external: Arc<dyn IsolatedAudioProcessorSpecResolver>,
    #[cfg(test)]
    retained_create_test_hook: Option<RetainedCreateTestHook>,
}

// Applied only after actual retained Ready, never to the validation owner.
#[cfg(test)]
type RetainedCreateTestHook =
    Arc<dyn Fn(&mut SupervisedAudioWorker) -> Result<(), AudioProcessorHostError> + Send + Sync>;

impl IsolatedAudioProcessorResolver {
    /// Bind one concrete native discovery/ABI Adapter.
    pub fn new(external: Arc<dyn IsolatedAudioProcessorSpecResolver>) -> Self {
        Self {
            external,
            #[cfg(test)]
            retained_create_test_hook: None,
        }
    }
    // Existing create seam applies only to the actual retained Ready owner.
    // A single-occurrence test owns the one-shot witness, not a global switch.
    #[cfg(test)]
    pub(crate) fn discard_next_seed_commit_reply_for_test(
        &mut self,
        observed: Arc<std::sync::atomic::AtomicBool>,
    ) {
        self.retained_create_test_hook = Some(Arc::new(move |worker| {
            worker.discard_next_seed_commit_reply_for_test(Arc::clone(&observed));
            Ok(())
        }));
    }
}

impl AudioProcessorResolver for IsolatedAudioProcessorResolver {
    fn native_child_settlement_requirement(
        &self,
    ) -> mondrian_core::NativeChildSettlementRequirement {
        // This outer resolver owns validation launches; it cannot wash an unknown inner probe.
        match self.external.native_child_settlement_requirement() {
            mondrian_core::NativeChildSettlementRequirement::Unavailable => {
                mondrian_core::NativeChildSettlementRequirement::Unavailable
            }
            _ => mondrian_core::NativeChildSettlementRequirement::Required,
        }
    }

    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        if matches!(
            request.definition(),
            AudioProcessorDefinitionRef::BuiltIn { .. }
        ) {
            return BuiltInAudioProcessorResolver.prepare(request);
        }
        let native_child_settlement = request.native_child_settlement_registrar();
        if let Some(registrar) = native_child_settlement {
            crate::processor_host::mark_unavailable_if_native_forwarding_unknown(registrar, || {
                self.external.native_child_settlement_requirement()
            });
        }
        let expected_contract = request.render_contract();
        let expected_parameter_ids = request.parameters().keys().cloned().collect::<Vec<_>>();
        let note_input_requested = request.note_input_requested();
        let expected_reference = request.musical_time_reference();
        let spec = self.external.resolve(request)?;
        if spec.render_contract != expected_contract
            || spec.parameter_ids != expected_parameter_ids
            || spec.musical_time_reference != expected_reference
        {
            return Err(invalid_contract(
                "external resolver changed the prepared render contract or parameter-lane identity",
            ));
        }
        if note_input_requested && spec.note_input_contract.ports.is_empty() {
            return Err(invalid_contract(
                "external adapter does not admit requested Note input",
            ));
        }
        let factory = IsolatedAudioProcessorFactory::prepare_inner(spec, native_child_settlement)?;
        #[cfg(test)]
        let factory = {
            let mut factory = factory;
            Arc::get_mut(&mut factory)
                .expect("new test Factory has one owner")
                .retained_create_test_hook = self.retained_create_test_hook.clone();
            factory
        };
        Ok(factory as Arc<dyn AudioProcessorFactory>)
    }
}

impl fmt::Debug for IsolatedAudioProcessorResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("IsolatedAudioProcessorResolver").finish_non_exhaustive()
    }
}

/// Render-contract-bound Factory backed by one supervised Worker per instance.
pub struct IsolatedAudioProcessorFactory {
    input_names: Vec<crate::AudioProcessorInputName>,
    spec: IsolatedAudioProcessorWorkerSpec,
    execution_contract: AudioProcessorExecutionContract,
    layout: SharedBlockLayout,
    #[cfg(test)]
    retained_create_test_hook: Option<RetainedCreateTestHook>,
}

impl IsolatedAudioProcessorFactory {
    // A new owning payload is prepared before publication. In particular, do
    // not clone the old preparation Vec only to replace and retire that copy.
    fn rebind_configuration_seed(
        &self,
        format: NativeConfigurationFormat,
        configuration: &[u8],
    ) -> Result<Self, AudioProcessorHostError> {
        let preparation_payload = match format {
            NativeConfigurationFormat::Vst3ComponentAndController => {
                vst3_worker::payload_with_configuration_seed(
                    &self.spec.preparation_payload,
                    configuration,
                )?
            }
            NativeConfigurationFormat::ClapState => clap_worker::payload_with_configuration_seed(
                &self.spec.preparation_payload,
                configuration,
            )?,
        };
        if preparation_payload.len() > MAX_PREPARATION_PAYLOAD_BYTES {
            return Err(invalid_contract(
                "native seed payload exceeds the existing preparation limit",
            ));
        }
        let spec = IsolatedAudioProcessorWorkerSpec {
            musical_time_reference: self.spec.musical_time_reference,
            helper_executable: self.spec.helper_executable.clone(),
            helper_arguments: self.spec.helper_arguments.clone(),
            dispatch_argument: self.spec.dispatch_argument.clone(),
            preparation_payload,
            preparation_evidence: PreparationEvidence::Unprobed,
            plugin_execution_contract: self.spec.plugin_execution_contract,
            auxiliary_inputs: self.spec.auxiliary_inputs.clone(),
            auxiliary_outputs: self.spec.auxiliary_outputs.clone(),
            note_input_contract: self.spec.note_input_contract.clone(),
            parameter_ids: self.spec.parameter_ids.clone(),
            render_contract: self.spec.render_contract,
            startup_timeout: self.spec.startup_timeout,
            state_entry_timeout: self.spec.state_entry_timeout,
            operation_timeout: self.spec.operation_timeout,
            native_lane: self.spec.native_lane,
        };
        Ok(Self {
            input_names: self.input_names.clone(),
            spec,
            execution_contract: self.execution_contract,
            layout: self.layout,
            #[cfg(test)]
            retained_create_test_hook: self.retained_create_test_hook.clone(),
        })
    }

    fn admits_reprepare_identity(
        &self,
        identity: NativeConfigurationSeedTicket,
        configuration_bytes: usize,
    ) -> bool {
        let old = self.spec.plugin_execution_contract;
        let next = identity.observed_contract;
        NativeConfigurationFormat::for_retained_restart(identity.serviced_cohort.request)
            == Some(identity.format)
            && identity.configuration_bytes == configuration_bytes
            && identity.serviced_cohort.serial != 0
            && !identity.serviced_cohort.exhausted()
            && self.spec.note_input_contract.ports.is_empty()
            && old.requires_state_entry() == next.requires_state_entry()
            && old.realtime_capable() == next.realtime_capable()
            && old.offline_capable() == next.offline_capable()
            && old.session_scratch_bytes() == next.session_scratch_bytes()
    }

    // Observed known-parent storage only: platform buffers and Arc prefixes keep
    // their existing coverage limitation. No allocator/SDK total is inferred.
    fn known_reprepare_factory_bytes(&self) -> Option<usize> {
        let mut bytes = std::mem::size_of::<Self>();
        self.visit_host_owned_storage(&mut |owner| {
            bytes = bytes
                .checked_add(owner.layout().size())
                .ok_or(crate::NativeHostLedgerError::ExtentOverflow)?;
            Ok(())
        })
        .ok()?;
        Some(bytes)
    }

    fn rebind_execution_configuration(
        &self,
        identity: NativeConfigurationSeedTicket,
        configuration: &[u8],
        report_peak: &mut dyn FnMut(usize),
    ) -> Result<Option<Arc<Self>>, AudioProcessorHostError> {
        use sha2::{Digest, Sha256};
        if !self.admits_reprepare_identity(identity, configuration.len()) {
            return Ok(None);
        }
        if <[u8; 32]>::from(Sha256::digest(configuration)) != identity.configuration_sha256 {
            return Err(invalid_contract(
                "retained restart configuration digest changed",
            ));
        }
        let n = self.spec.preparation_payload.len();
        let codec = match identity.format {
            NativeConfigurationFormat::ClapState => clap_worker::reprepare_codec_bound(n),
            NativeConfigurationFormat::Vst3ComponentAndController => {
                vst3_worker::reprepare_codec_bound(n)
            }
        }
        .ok_or_else(|| {
            invalid_contract(match identity.format {
                NativeConfigurationFormat::ClapState => {
                    "retained CLAP metadata exceeds the raw codec bound"
                }
                NativeConfigurationFormat::Vst3ComponentAndController => {
                    "retained VST3 metadata exceeds the raw codec bound"
                }
            })
        })?;
        let preparation_payload = match identity.format {
            NativeConfigurationFormat::ClapState => {
                clap_worker::payload_with_reprepare_configuration(
                    &self.spec.preparation_payload,
                    configuration,
                    report_peak,
                )
            }
            NativeConfigurationFormat::Vst3ComponentAndController => {
                vst3_worker::payload_with_reprepare_configuration(
                    &self.spec.preparation_payload,
                    configuration,
                    report_peak,
                )
            }
        }?;
        // Unlike prepare_inner this does not consume/probe new evidence or create
        // a worker. For the concrete format, original immutable metadata passed
        // its typed worker/catalog admission; this is not external JSON admission
        // or a claim about arbitrary custom helper implementations.
        let spec = IsolatedAudioProcessorWorkerSpec {
            musical_time_reference: self.spec.musical_time_reference,
            helper_executable: self.spec.helper_executable.clone(),
            helper_arguments: self.spec.helper_arguments.clone(),
            dispatch_argument: self.spec.dispatch_argument.clone(),
            preparation_payload,
            preparation_evidence: PreparationEvidence::Unprobed,
            plugin_execution_contract: identity.observed_contract,
            auxiliary_inputs: self.spec.auxiliary_inputs.clone(),
            auxiliary_outputs: self.spec.auxiliary_outputs.clone(),
            note_input_contract: self.spec.note_input_contract.clone(),
            parameter_ids: self.spec.parameter_ids.clone(),
            render_contract: self.spec.render_contract,
            startup_timeout: self.spec.startup_timeout,
            state_entry_timeout: self.spec.state_entry_timeout,
            operation_timeout: self.spec.operation_timeout,
            native_lane: self.spec.native_lane,
        };
        let mut next = Self {
            input_names: self.input_names.clone(),
            spec,
            execution_contract: self.execution_contract,
            layout: self.layout,
            #[cfg(test)]
            retained_create_test_hook: self.retained_create_test_hook.clone(),
        };
        let actual = next.known_reprepare_factory_bytes().unwrap_or(usize::MAX);
        report_peak(codec.saturating_add(actual));
        next.execution_contract = Self::requested_execution_contract(&next.spec, next.layout)?;
        // After this Arc forms there is no fallible query/cap check: the Runtime
        // first adopts it, then checks actual peak/cancellation/Source identity.
        Ok(Some(Arc::new(next)))
    }

    /// Validate immutable preparation facts before a Plan retains this Factory.
    /// A fresh native probe supplies presentation metadata without an additional
    /// Worker; generic/manual specs retain the real validation launch and shutdown.
    /// Every actual instance still passes the complete same-owner Ready checks.
    pub fn prepare(
        spec: IsolatedAudioProcessorWorkerSpec,
    ) -> Result<Arc<Self>, AudioProcessorHostError> {
        Self::prepare_inner(spec, None)
    }

    fn prepare_inner(
        mut spec: IsolatedAudioProcessorWorkerSpec,
        native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    ) -> Result<Arc<Self>, AudioProcessorHostError> {
        spec.validate()?;
        let layout = SharedBlockLayout::new(
            spec.render_contract.max_block_frames,
            spec.render_contract.channel_count(),
            spec.auxiliary_inputs.buses.len(),
            spec.parameter_ids.len(),
        )?
        .with_auxiliary_outputs(spec.auxiliary_outputs.buses.len())?
        .with_notes(
            spec.note_input_contract.limits.input_events,
            spec.note_input_contract.limits.output_ends,
        )?;
        let execution_contract = Self::requested_execution_contract(&spec, layout)?;
        let input_names = match std::mem::replace(
            &mut spec.preparation_evidence,
            PreparationEvidence::Unprobed,
        ) {
            PreparationEvidence::NativeProbed { input_names } => input_names,
            PreparationEvidence::Unprobed => {
                let (mut validation, input_names) = SupervisedAudioWorker::launch(
                    &spec,
                    spec.render_contract,
                    layout,
                    None,
                    native_child_settlement,
                )?;
                validation.shutdown_observed().validation_result()?;
                input_names
            }
        };
        Ok(Arc::new(Self {
            spec,
            execution_contract,
            layout,
            input_names,
            #[cfg(test)]
            retained_create_test_hook: None,
        }))
    }

    fn requested_execution_contract(
        spec: &IsolatedAudioProcessorWorkerSpec,
        layout: SharedBlockLayout,
    ) -> Result<AudioProcessorExecutionContract, AudioProcessorHostError> {
        spec.plugin_execution_contract
            .with_additional_session_scratch_bytes(layout.total_bytes)?
            .with_additional_session_scratch_bytes(
                isolated_output_contract_clone_reservation_bytes(&spec.auxiliary_outputs)?,
            )?
            .with_additional_session_scratch_bytes(spec.native_lane.map_or(
                0,
                crate::NativeWorkerLaneResourceProfile::additional_session_reservation_bytes,
            ))?
            .with_additional_session_scratch_bytes(supervisor::child_settlement_slot_bytes())
    }
}

impl AudioProcessorFactory for IsolatedAudioProcessorFactory {
    fn native_child_settlement_requirement(
        &self,
    ) -> mondrian_core::NativeChildSettlementRequirement {
        mondrian_core::NativeChildSettlementRequirement::Required
    }

    fn visit_host_owned_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(
            crate::AudioProcessorFactoryStorageBorrow<'a>,
        ) -> Result<(), crate::AudioProcessorFactoryStorageError>,
    ) -> Result<(), crate::AudioProcessorFactoryStorageError> {
        use crate::AudioProcessorFactoryStorageBorrow as Storage;
        visitor(Storage::complete(&self.input_names)?)?;
        for name in &self.input_names {
            visitor(Storage::complete(&name.bus_key)?)?;
            visitor(Storage::complete(&name.name)?)?;
        }
        visitor(Storage::path(&self.spec.helper_executable)?)?;
        visitor(Storage::complete(&self.spec.helper_arguments)?)?;
        for argument in &self.spec.helper_arguments {
            visitor(Storage::os_string(argument)?)?;
        }
        if let Some(argument) = &self.spec.dispatch_argument {
            visitor(Storage::os_string(argument)?)?;
        }
        visitor(Storage::complete(&self.spec.preparation_payload)?)?;
        visitor(Storage::complete(&self.spec.auxiliary_inputs.buses)?)?;
        for bus in &self.spec.auxiliary_inputs.buses {
            visitor(Storage::complete(&bus.bus_key)?)?;
        }
        visitor(Storage::complete(&self.spec.auxiliary_outputs.buses)?)?;
        for bus in &self.spec.auxiliary_outputs.buses {
            visitor(Storage::complete(&bus.bus_key)?)?;
        }
        visitor(Storage::complete(&self.spec.note_input_contract.ports)?)?;
        for port in &self.spec.note_input_contract.ports {
            visitor(Storage::complete(&port.port_key)?)?;
        }
        visitor(Storage::complete(&self.spec.parameter_ids)?)?;
        for id in &self.spec.parameter_ids {
            visitor(Storage::complete(id.storage_owner())?)?;
        }
        Ok(())
    }

    fn with_native_configuration_seed(
        &self,
        format: NativeConfigurationFormat,
        configuration: &[u8],
    ) -> Result<Option<Arc<dyn AudioProcessorFactory>>, AudioProcessorHostError> {
        Ok(Some(Arc::new(
            self.rebind_configuration_seed(format, configuration)?,
        )))
    }

    fn native_reprepare_factory_bound(
        &self,
        identity: NativeConfigurationSeedTicket,
        configuration_bytes: usize,
    ) -> Option<usize> {
        if !self.admits_reprepare_identity(identity, configuration_bytes) {
            return None;
        }
        let n = self.spec.preparation_payload.len();
        let (codec, output) = match identity.format {
            NativeConfigurationFormat::ClapState => (
                clap_worker::reprepare_codec_bound(n)?,
                clap_worker::reprepare_payload_output_bound(n, configuration_bytes)?,
            ),
            NativeConfigurationFormat::Vst3ComponentAndController => (
                vst3_worker::reprepare_codec_bound(n)?,
                vst3_worker::reprepare_payload_output_bound(n, configuration_bytes)?,
            ),
        };
        let clones = self
            .known_reprepare_factory_bytes()?
            .checked_sub(self.spec.preparation_payload.capacity())?;
        codec.checked_add(output)?.checked_add(clones)
    }

    fn with_native_reprepare_configuration(
        &self,
        identity: NativeConfigurationSeedTicket,
        configuration: &[u8],
        report_peak: &mut dyn FnMut(usize),
    ) -> Result<Option<Arc<dyn AudioProcessorFactory>>, AudioProcessorHostError> {
        self.rebind_execution_configuration(identity, configuration, report_peak)
            .map(|factory| factory.map(|factory| factory as Arc<dyn AudioProcessorFactory>))
    }

    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        self.execution_contract
    }

    fn auxiliary_input_contract(&self) -> AudioProcessorAuxiliaryInputContract {
        self.spec.auxiliary_inputs.clone()
    }

    fn auxiliary_output_contract(&self) -> AudioProcessorAuxiliaryOutputContract {
        self.spec.auxiliary_outputs.clone()
    }

    fn note_input_contract(&self) -> crate::AudioProcessorNoteInputContract {
        self.spec.note_input_contract.clone()
    }

    fn auxiliary_input_names(&self) -> Vec<crate::AudioProcessorInputName> {
        self.input_names.clone()
    }

    fn create(
        &self,
        native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    ) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        // This is an additional parent instance owner. Charge it through the
        // original private Session scratch contract and verify the actual clone
        // before launching foreign code. Move this exact owner into the instance.
        let auxiliary_outputs = self.spec.auxiliary_outputs.clone();
        if isolated_output_contract_allocated_bytes(&auxiliary_outputs)?
            > isolated_output_contract_clone_reservation_bytes(&self.spec.auxiliary_outputs)?
        {
            return Err(invalid_contract(
                "isolated parent output metadata exceeds its admitted clone reservation",
            ));
        }
        let (worker, _) = SupervisedAudioWorker::launch(
            &self.spec,
            self.spec.render_contract,
            self.layout,
            Some(&self.input_names),
            native_child_settlement,
        )?;
        #[cfg(test)]
        let worker = {
            let mut worker = worker;
            if let Some(hook) = &self.retained_create_test_hook {
                hook(&mut worker)?;
            }
            worker
        };
        Ok(Box::new(IsolatedAudioProcessor {
            musical_time_reference: self.spec.musical_time_reference,
            worker,
            parameter_ids: self.spec.parameter_ids.clone(),
            auxiliary_inputs: self.spec.auxiliary_inputs.clone(),
            auxiliary_outputs,
            note_input_contract: self.spec.note_input_contract.clone(),
            render_contract: self.spec.render_contract,
            failed: false,
            pending_reprepare: None,
            staged_configuration_seed: None,
            native_plugin_contract: self.spec.plugin_execution_contract,
        }))
    }
}

// Two pure passes of the SAME concrete serde serializer determine and then
// write the exact bounded payload, without an oversized geometric output Vec.
fn encode_native_configuration_payload<T: serde::Serialize>(
    payload: &T,
) -> Result<Vec<u8>, AudioProcessorHostError> {
    encode_native_configuration_payload_observed(payload, &mut |_| {})
}

fn encode_native_configuration_payload_observed<T: serde::Serialize>(
    payload: &T,
    report_capacity: &mut dyn FnMut(usize),
) -> Result<Vec<u8>, AudioProcessorHostError> {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
            let next = self
                .0
                .checked_add(input.len())
                .filter(|bytes| *bytes <= MAX_PREPARATION_PAYLOAD_BYTES)
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "native seed preparation payload limit exceeded",
                    )
                })?;
            self.0 = next;
            Ok(input.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    serde_json::to_writer(&mut count, payload).map_err(|error| {
        invalid_contract(&format!("native seed payload measurement refused: {error}"))
    })?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(count.0).map_err(|error| {
        invalid_contract(&format!("native seed payload allocation refused: {error}"))
    })?;
    report_capacity(bytes.capacity());
    if bytes.capacity() > MAX_PREPARATION_PAYLOAD_BYTES {
        return Err(invalid_contract(
            "native seed actual output capacity exceeds the preparation bound",
        ));
    }
    struct Exact<'a> {
        bytes: &'a mut Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Exact<'_> {
        fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
            self.bytes
                .len()
                .checked_add(input.len())
                .filter(|bytes| *bytes <= self.limit)
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "native seed exceeded its measured serialization extent",
                    )
                })?;
            self.bytes.extend_from_slice(input);
            Ok(input.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(&mut Exact { bytes: &mut bytes, limit: count.0 }, payload).map_err(
        |error| invalid_contract(&format!("native seed payload encoding refused: {error}")),
    )?;
    if bytes.len() != count.0 {
        return Err(invalid_contract(
            "native seed payload changed between pure serialization passes",
        ));
    }
    Ok(bytes)
}
impl fmt::Debug for IsolatedAudioProcessorFactory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IsolatedAudioProcessorFactory")
            .field("spec", &self.spec)
            .field("execution_contract", &self.execution_contract)
            .field("shared_bytes", &self.layout.total_bytes)
            .finish()
    }
}

// Private parent instance metadata only; the Factory/spec and native child
// are separate owners charged by their existing visitors/reservations.
fn isolated_output_contract_clone_reservation_bytes(
    contract: &AudioProcessorAuxiliaryOutputContract,
) -> Result<usize, AudioProcessorHostError> {
    let bytes = contract
        .buses
        .len()
        .checked_mul(size_of::<crate::AudioProcessorAuxiliaryOutputBusContract>())
        .ok_or_else(|| {
            invalid_contract("isolated parent output metadata reservation overflowed")
        })?;
    contract.buses.iter().try_fold(bytes, |bytes, bus| {
        bytes
            .checked_add(bus.bus_key.len())
            .ok_or_else(|| invalid_contract("isolated parent output key reservation overflowed"))
    })
}
fn isolated_output_contract_allocated_bytes(
    contract: &AudioProcessorAuxiliaryOutputContract,
) -> Result<usize, AudioProcessorHostError> {
    let bytes = contract
        .buses
        .capacity()
        .checked_mul(size_of::<crate::AudioProcessorAuxiliaryOutputBusContract>())
        .ok_or_else(|| invalid_contract("isolated parent output metadata capacity overflowed"))?;
    contract.buses.iter().try_fold(bytes, |bytes, bus| {
        bytes
            .checked_add(bus.bus_key.capacity())
            .ok_or_else(|| invalid_contract("isolated parent output key capacity overflowed"))
    })
}

struct IsolatedAudioProcessor {
    musical_time_reference: Option<mondrian_core::MusicalTimeReference>,
    worker: SupervisedAudioWorker,
    parameter_ids: Vec<ParameterId>,
    auxiliary_inputs: AudioProcessorAuxiliaryInputContract,
    auxiliary_outputs: AudioProcessorAuxiliaryOutputContract,
    note_input_contract: crate::AudioProcessorNoteInputContract,
    render_contract: AudioRenderContract,
    failed: bool,
    pending_reprepare: Option<crate::NativeObservedRequest>,
    staged_configuration_seed: Option<NativeConfigurationSeedReceipt>,
    native_plugin_contract: AudioProcessorExecutionContract,
}

impl AudioProcessor for IsolatedAudioProcessor {
    fn bind_native_child_settlement(
        &mut self,
        registrar: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<
        mondrian_core::NativeChildSettlementBinding,
        mondrian_core::NativeChildSettlementError,
    > {
        self.worker.bind_native_child_settlement(registrar)?;
        Ok(mondrian_core::NativeChildSettlementBinding::Registered)
    }

    fn open_native_audio_editor(
        &mut self,
        request: crate::NativeAudioEditorOpenRequest,
    ) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
        if self.failed {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        match self.worker.open_native_audio_editor(request) {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    fn close_native_audio_editor(
        &mut self,
        binding: crate::NativeAudioObjectBindRequest,
    ) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
        if self.failed {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        match self.worker.close_native_audio_editor(binding) {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    fn native_audio_object_observation(
        &mut self,
    ) -> Result<Option<crate::NativeAudioObjectObservation>, AudioProcessorHostError> {
        if self.failed {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        match self.worker.observe_native_audio_object() {
            Ok(object) => Ok(object),
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    fn native_observations_supported(&self) -> bool {
        true
    }

    fn poll_native_observations(
        &mut self,
        consumption: crate::NativeAudioObservationConsumption,
    ) -> Result<crate::NativeAudioObservations, AudioProcessorHostError> {
        if self.failed {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        match self.worker.poll_native_observations(consumption) {
            Ok(mut observations) => {
                if let crate::NativeAudioObservationConsumption::AcknowledgeAuthor(expected) =
                    consumption
                    && observations.object == Some(expected.installed_binding.expected_object)
                    && expected.source_binding.expected_object
                        == expected.installed_binding.expected_object
                    && let Some(crate::NativeAudioAuthorDeliveryState::Acknowledged {
                        acknowledgement,
                        request_cohort: Some(original),
                    }) = observations.author.as_ref().map(|author| &author.delivery)
                    && *acknowledgement == expected
                    && original.serial != 0
                    && !original.exhausted()
                    && original.request
                        == (mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 })
                {
                    // Only this exact successful Ack retires its original A
                    // from the adapter cache. A later/unknown fact stays held;
                    // ordinary Poll/None cannot establish retirement.
                    self.pending_reprepare =
                        observations.pending_reprepare.or(self.pending_reprepare.filter(
                            |request| *request != crate::NativeObservedRequest::Cohort(*original),
                        ));
                    observations.pending_reprepare = self.pending_reprepare;
                }
                Ok(observations)
            }
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    fn preflight_native_observations(
        &mut self,
        _expected_object: crate::NativeAudioObjectObservation,
    ) -> Result<crate::NativeAudioObservations, AudioProcessorHostError> {
        if self.failed {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        match self.worker.preflight_native_observations() {
            Ok(mut observations) => {
                // Preserve the adapter's existing sticky request: a serviced
                // native callback becoming empty is not graph/PDC installation.
                self.pending_reprepare = observations.pending_reprepare.or(self.pending_reprepare);
                observations.pending_reprepare = self.pending_reprepare;
                Ok(observations)
            }
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    fn bind_native_audio_object(
        &mut self,
        request: crate::NativeAudioObjectBindRequest,
    ) -> Result<crate::NativeAudioObjectBindOutcome, AudioProcessorHostError> {
        if self.failed {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        match self.worker.bind_native_audio_object(request) {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    fn prepare_native_author_state_entry(
        &mut self,
        request: crate::NativeAudioAuthorStateEntryRequest<'_>,
    ) -> Result<crate::NativeAudioAuthorStateEntryOutcome, AudioProcessorHostError> {
        if self.failed {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        match self.worker.prepare_native_author_state_entry(request) {
            Ok(crate::NativeAudioAuthorStateEntryOutcome::Prepared(receipt)) => {
                if receipt.contract() != self.native_plugin_contract {
                    self.failed = true;
                    return Err(AudioProcessorHostError::InvalidContract(
                        "restored native plugin contract changed".to_owned(),
                    ));
                }
                Ok(crate::NativeAudioAuthorStateEntryOutcome::Prepared(receipt))
            }
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    fn native_configuration_cohort(
        &mut self,
    ) -> Result<Option<NativeRequestCohort>, AudioProcessorHostError> {
        if self.failed {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        self.worker.query_native_configuration_cohort()
    }

    fn service_native_configuration(
        &mut self,
        request: NativeConfigurationControlRequest<'_>,
    ) -> Result<NativeConfigurationControlOutcome, AudioProcessorHostError> {
        if self.failed {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        let expected = request.expected_cohort.request;
        let outcome = self.worker.service_native_configuration(request);
        match outcome {
            Ok(outcome) => {
                self.pending_reprepare = Some(crate::NativeObservedRequest::RequestOnly(expected));
                Ok(outcome)
            }
            Err(AudioProcessorHostError::InvalidContract(detail)) => {
                Err(AudioProcessorHostError::InvalidContract(detail))
            }
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    fn stage_native_configuration_seed(
        &mut self,
        request: NativeConfigurationSeedRequest<'_>,
    ) -> Result<NativeConfigurationSeedOutcome, AudioProcessorHostError> {
        if self.failed {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        match self.worker.stage_native_configuration_seed(request) {
            Ok(outcome) => {
                if let NativeConfigurationSeedOutcome::Accepted(receipt) = outcome {
                    self.staged_configuration_seed = Some(receipt);
                }
                Ok(outcome)
            }
            Err(error @ AudioProcessorHostError::InvalidContract(_)) => Err(error),
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    fn commit_native_configuration_seed(
        &mut self,
        ticket: NativeConfigurationSeedTicket,
    ) -> Result<NativeConfigurationSeedOutcome, AudioProcessorHostError> {
        if self.failed {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        let Some(staged) =
            self.staged_configuration_seed.filter(|receipt| receipt.ticket == ticket)
        else {
            return Ok(NativeConfigurationSeedOutcome::Refused(
                NativeConfigurationSeedRefusal::SeedNotStaged,
            ));
        };
        match self.worker.commit_native_configuration_seed(staged) {
            Ok(outcome) => {
                if let NativeConfigurationSeedOutcome::Accepted(_) = outcome {
                    // The authenticated reply matched this exact staged ticket.
                    // Keep the parent aligned with the committed native contract;
                    // this does not install a Runtime plan or authorize PCM.
                    self.native_plugin_contract = ticket.observed_contract;
                    self.staged_configuration_seed = None;
                    self.pending_reprepare = None;
                }
                Ok(outcome)
            }
            Err(error @ AudioProcessorHostError::InvalidContract(_)) => Err(error),
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    fn preflight_reprepare(
        &mut self,
    ) -> Result<Option<crate::NativeObservedRequest>, AudioProcessorHostError> {
        if self.failed {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        match self.worker.invoke(
            "pending query",
            WorkerCommand {
                kind: CommandKind::PollPending,
                start_sample: 0,
                frames: 0,
                event_count: 0,
                note_count: 0,
            },
        ) {
            Ok(pending) => {
                self.pending_reprepare = pending.or(self.pending_reprepare);
                Ok(self.pending_reprepare)
            }
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    fn enter_state(&mut self, start_sample: i64) -> Result<(), AudioProcessorHostError> {
        if self.failed {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        let result = self.worker.invoke(
            "state entry",
            WorkerCommand {
                kind: CommandKind::EnterState,
                start_sample,
                frames: 0,
                event_count: 0,
                note_count: 0,
            },
        );
        match result {
            Ok(pending) => {
                self.pending_reprepare = pending.or(self.pending_reprepare);
                Ok(())
            }
            Err(AudioProcessorHostError::ReprepareRequired(request)) => {
                self.pending_reprepare = Some(request);
                Err(AudioProcessorHostError::ReprepareRequired(request))
            }
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    fn process(
        &mut self,
        context: AudioProcessorProcessContext,
        audio: &mut dyn AudioProcessorAudioIo,
        parameters: AudioParameterEventBatch<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        if self.failed {
            return Err(AudioProcessorHostError::PoisonedInstance);
        }
        // Pure caller-contract refusal precedes foreign polling and every
        // shared PCM/event write. Equal tempo with a different zero is drift.
        if context.musical_time_reference() != self.musical_time_reference {
            return Err(invalid_contract(
                "isolated processor musical reference changed",
            ));
        }
        if !context.has_root_window_admission()
            && let Some(request) = self.preflight_reprepare()?
        {
            return Err(AudioProcessorHostError::ReprepareRequired(request));
        }
        let request = context.request();
        if context.sample_rate().hz() != self.render_contract.sample_rate
            || context.channel_layout() != self.render_contract.channel_layout
            || context.processing_mode() != self.render_contract.processing_mode
            || request.frames > self.render_contract.max_block_frames
            || audio.main_layout() != self.render_contract.channel_layout
            || audio.frames() != request.frames
            || parameters.lane_count() != self.parameter_ids.len()
        {
            self.failed = true;
            return Err(invalid_contract(
                "isolated processor callback drifted from its prepared render contract",
            ));
        }
        let channel_count = self.render_contract.channel_count();
        let samples = request
            .frames
            .checked_mul(channel_count)
            .ok_or_else(|| invalid_contract("isolated processor block extent overflowed"))?;
        let layout = self.worker.layout();
        for bus in &self.auxiliary_outputs.buses {
            let output = audio.auxiliary_output(&bus.bus_key).ok_or_else(|| {
                invalid_contract("isolated output bus is absent from the prepared callback")
            })?;
            if output.channel_layout != bus.channel_layout
                || output.frames != request.frames
                || output.interleaved.len() != samples
            {
                self.failed = true;
                return Err(invalid_contract(
                    "isolated output bus has the wrong layout or extent",
                ));
            }
        }
        let note_count =
            {
                let notes = audio.note_events();
                let note_count = notes.len();
                if note_count > layout.note_event_capacity {
                    return Err(invalid_contract(
                        "isolated Note input exceeds admitted capacity",
                    ));
                }
                let mut previous_note = None;
                for event in notes {
                    if !self.note_input_contract.accepts_event(*event, request.frames) {
                        return Err(invalid_contract(
                            "isolated Note channel exceeds admitted input channels",
                        ));
                    }
                    let packed = SharedNoteEvent::capture(
                        *event,
                        request.frames,
                        self.note_input_contract.ports.len(),
                    )?;
                    let order = (packed.sample_offset, packed.kind);
                    if previous_note.is_some_and(|previous| previous > order) {
                        return Err(invalid_contract(
                            "isolated Note input is not in prepared sample/phase order",
                        ));
                    }
                    previous_note = Some(order);
                }
                // Validate the single borrowed input slice before shared PCM or
                // any native call; its count, validation and copy use that same view.
                let byte_count = layout
                    .note_event_capacity
                    .checked_mul(size_of::<SharedNoteEvent>())
                    .ok_or_else(|| invalid_contract("isolated Note input bytes overflowed"))?;
                let end = layout
                    .note_events_offset
                    .checked_add(byte_count)
                    .ok_or_else(|| invalid_contract("isolated Note input range overflowed"))?;
                let storage: &mut [SharedNoteEvent] = bytemuck::try_cast_slice_mut(
                    self.worker.shared_mut().get_mut(layout.note_events_offset..end).ok_or_else(
                        || invalid_contract("isolated Note input range is unavailable"),
                    )?,
                )
                .map_err(|_| invalid_contract("isolated Note input storage is not aligned"))?;
                for (destination, event) in storage.iter_mut().zip(notes) {
                    *destination = SharedNoteEvent::capture(
                        *event,
                        request.frames,
                        self.note_input_contract.ports.len(),
                    )?;
                }
                note_count
            };
        let mut event_count = 0_usize;
        {
            let shared = self.worker.shared_mut();
            let (before_events, events_and_after) = shared.split_at_mut(layout.events_offset);
            let main_bytes = layout
                .main_offset
                .checked_add(samples * size_of::<f32>())
                .ok_or_else(|| invalid_contract("isolated main range overflowed"))?;
            let (inputs_and_gap, outputs_and_gap) =
                before_events.split_at_mut(layout.auxiliary_outputs_offset);
            let output_bytes = samples
                .checked_mul(self.auxiliary_outputs.buses.len())
                .and_then(|samples| samples.checked_mul(size_of::<f32>()))
                .ok_or_else(|| invalid_contract("isolated output extent overflowed"))?;
            outputs_and_gap
                .get_mut(..output_bytes)
                .ok_or_else(|| invalid_contract("isolated output range is unavailable"))?
                .fill(0);
            let (main_and_gap, auxiliary_and_gap) =
                inputs_and_gap.split_at_mut(layout.auxiliary_offset);
            let main: &mut [f32] = bytemuck::try_cast_slice_mut(
                main_and_gap
                    .get_mut(layout.main_offset..main_bytes)
                    .ok_or_else(|| invalid_contract("isolated main range is unavailable"))?,
            )
            .map_err(|_| invalid_contract("isolated main storage is not aligned"))?;
            let source_main = audio.main_interleaved();
            if source_main.len() != samples {
                self.failed = true;
                return Err(invalid_contract("isolated main bus has the wrong extent"));
            }
            main.copy_from_slice(source_main);

            let auxiliary_samples = samples
                .checked_mul(self.auxiliary_inputs.buses.len())
                .ok_or_else(|| invalid_contract("isolated auxiliary extent overflowed"))?;
            let auxiliary_bytes = auxiliary_samples
                .checked_mul(size_of::<f32>())
                .ok_or_else(|| invalid_contract("isolated auxiliary bytes overflowed"))?;
            let auxiliary: &mut [f32] = bytemuck::try_cast_slice_mut(
                auxiliary_and_gap
                    .get_mut(..auxiliary_bytes)
                    .ok_or_else(|| invalid_contract("isolated auxiliary range is unavailable"))?,
            )
            .map_err(|_| invalid_contract("isolated auxiliary storage is not aligned"))?;
            for (bus_index, bus) in self.auxiliary_inputs.buses.iter().enumerate() {
                let start = bus_index
                    .checked_mul(samples)
                    .ok_or_else(|| invalid_contract("isolated auxiliary offset overflowed"))?;
                let end = start
                    .checked_add(samples)
                    .ok_or_else(|| invalid_contract("isolated auxiliary range overflowed"))?;
                let input = audio.auxiliary_input(&bus.bus_key).ok_or_else(|| {
                    invalid_contract("isolated auxiliary bus is absent from the prepared callback")
                })?;
                if input.channel_layout != bus.channel_layout
                    || input.frames != request.frames
                    || input.interleaved.len() != samples
                {
                    self.failed = true;
                    return Err(invalid_contract(
                        "isolated auxiliary bus drifted from its prepared contract",
                    ));
                }
                auxiliary[start..end].copy_from_slice(input.interleaved);
            }

            let event_storage: &mut [SharedParameterEvent] = bytemuck::try_cast_slice_mut(
                events_and_after
                    .get_mut(..layout.event_capacity * size_of::<SharedParameterEvent>())
                    .ok_or_else(|| invalid_contract("isolated event range is unavailable"))?,
            )
            .map_err(|_| invalid_contract("isolated event storage is not aligned"))?;
            for (lane, expected_id) in self.parameter_ids.iter().enumerate() {
                if parameters.parameter_id(lane) != Some(expected_id) {
                    self.failed = true;
                    return Err(invalid_contract(
                        "isolated parameter lane identity drifted from preparation",
                    ));
                }
                let lane_events = parameters.events(lane).ok_or_else(|| {
                    invalid_contract("isolated parameter lane has no prepared event range")
                })?;
                for event in lane_events {
                    let destination = event_storage.get_mut(event_count).ok_or_else(|| {
                        invalid_contract("isolated parameter events exceed admitted capacity")
                    })?;
                    *destination = SharedParameterEvent {
                        lane: u32::try_from(lane).map_err(|_| {
                            invalid_contract(
                                "isolated parameter lane is not protocol-representable",
                            )
                        })?,
                        sample_offset: event.sample_offset,
                        value: event.value,
                    };
                    event_count += 1;
                }
            }
        }
        let result = self.worker.invoke_process(
            "block processing",
            WorkerCommand {
                kind: CommandKind::Process,
                start_sample: request.start_sample,
                frames: u32::try_from(request.frames)
                    .map_err(|_| invalid_contract("block extent is not protocol-representable"))?,
                event_count: u32::try_from(event_count)
                    .map_err(|_| invalid_contract("event count is not protocol-representable"))?,
                note_count: u32::try_from(note_count)
                    .map_err(|_| invalid_contract("Note count is not protocol-representable"))?,
            },
        );
        let note_end_count = match result {
            Ok((pending, note_end_count)) => {
                self.pending_reprepare = pending.or(self.pending_reprepare);
                note_end_count
            }
            Err(AudioProcessorHostError::ReprepareRequired(request)) => {
                self.pending_reprepare = Some(request);
                return Err(AudioProcessorHostError::ReprepareRequired(request));
            }
            Err(error) => {
                self.failed = true;
                return Err(error);
            }
        };
        // Native DSP has returned. Every subsequent refusal must retire this
        // adapter; no retry can repeat the already delivered input events.
        self.failed = true;
        let note_mode = !self.note_input_contract.ports.is_empty();
        if note_mode != note_end_count.is_some()
            || note_end_count.is_some_and(|count| count as usize > layout.note_end_capacity)
        {
            self.failed = true;
            return Err(invalid_contract(
                "isolated Process reply has another Note mode/count",
            ));
        }
        let end_count = note_end_count.unwrap_or(0) as usize;
        let end_bytes = end_count
            .checked_mul(size_of::<SharedNoteEnd>())
            .ok_or_else(|| invalid_contract("isolated Note End bytes overflowed"))?;
        let end_range = layout
            .note_ends_offset
            .checked_add(end_bytes)
            .ok_or_else(|| invalid_contract("isolated Note End range overflowed"))?;
        let copied_ends: &[SharedNoteEnd] = bytemuck::try_cast_slice(
            self.worker
                .shared_mut()
                .get(layout.note_ends_offset..end_range)
                .ok_or_else(|| invalid_contract("isolated Note End range is unavailable"))?,
        )
        .map_err(|_| invalid_contract("isolated Note End storage is not aligned"))?;
        // Validate the entire copied prefix before pushing any observation. Its
        // origin is the retained actual command, never a field in these events.
        for end in copied_ends {
            if let Err(error) = end.realize(self.note_input_contract.ports.len()) {
                self.failed = true;
                return Err(error);
            }
        }
        for end in copied_ends {
            if !audio.try_push_note_end(end.realize(self.note_input_contract.ports.len())?) {
                self.failed = true;
                return Err(invalid_contract(
                    "parent refused copied Note End output after native processing",
                ));
            }
        }
        let shared = self.worker.shared_mut();
        let main_end = layout
            .main_offset
            .checked_add(samples * size_of::<f32>())
            .ok_or_else(|| invalid_contract("isolated output range overflowed"))?;
        let output: &[f32] = bytemuck::try_cast_slice(
            shared
                .get(layout.main_offset..main_end)
                .ok_or_else(|| invalid_contract("isolated output range is unavailable"))?,
        )
        .map_err(|_| invalid_contract("isolated output storage is not aligned"))?;
        if output.iter().any(|sample| !sample.is_finite()) {
            self.failed = true;
            return Err(invalid_contract(
                "isolated main output contains nonfinite PCM",
            ));
        }
        let outputs_end = samples
            .checked_mul(self.auxiliary_outputs.buses.len())
            .and_then(|samples| samples.checked_mul(size_of::<f32>()))
            .and_then(|bytes| layout.auxiliary_outputs_offset.checked_add(bytes))
            .ok_or_else(|| invalid_contract("isolated output range overflowed"))?;
        let auxiliary: &[f32] = bytemuck::try_cast_slice(
            shared
                .get(layout.auxiliary_outputs_offset..outputs_end)
                .ok_or_else(|| invalid_contract("isolated output range is unavailable"))?,
        )
        .map_err(|_| invalid_contract("isolated output storage is not aligned"))?;
        if auxiliary.iter().any(|sample| !sample.is_finite()) {
            self.failed = true;
            return Err(invalid_contract(
                "isolated auxiliary output contains nonfinite PCM",
            ));
        }
        // The host owns these destinations; validate the entire view set before copying any PCM.
        for bus in &self.auxiliary_outputs.buses {
            let destination = audio
                .auxiliary_output(&bus.bus_key)
                .ok_or_else(|| invalid_contract("isolated output destination disappeared"))?;
            if destination.channel_layout != bus.channel_layout
                || destination.frames != request.frames
                || destination.interleaved.len() != samples
            {
                self.failed = true;
                return Err(invalid_contract(
                    "isolated output destination changed extent",
                ));
            }
        }
        audio.main_interleaved().copy_from_slice(output);
        for (index, bus) in self.auxiliary_outputs.buses.iter().enumerate() {
            let destination = audio
                .auxiliary_output(&bus.bus_key)
                .ok_or_else(|| invalid_contract("isolated output destination disappeared"))?;
            destination
                .interleaved
                .copy_from_slice(&auxiliary[index * samples..(index + 1) * samples]);
        }
        self.failed = false;
        Ok(())
    }
}

/// Immutable request received by a child-side native plugin Adapter.
pub struct IsolatedAudioProcessorWorkerPrepareRequest {
    musical_time_reference: Option<mondrian_core::MusicalTimeReference>,
    native_lane: Option<crate::NativeWorkerLaneResourceProfile>,
    payload: Vec<u8>,
    plugin_execution_contract: AudioProcessorExecutionContract,
    auxiliary_inputs: AudioProcessorAuxiliaryInputContract,
    auxiliary_outputs: AudioProcessorAuxiliaryOutputContract,
    note_input_contract: crate::AudioProcessorNoteInputContract,
    parameter_ids: Vec<ParameterId>,
    render_contract: AudioRenderContract,
}

impl IsolatedAudioProcessorWorkerPrepareRequest {
    /// Same immutable Sequence reference carried in the startup envelope.
    pub const fn musical_time_reference(&self) -> Option<mondrian_core::MusicalTimeReference> {
        self.musical_time_reference
    }
    /// Matching known native lane request, never a complete grant or owner lease.
    pub const fn native_worker_lane_resources(
        &self,
    ) -> Option<crate::NativeWorkerLaneResourceProfile> {
        self.native_lane
    }
    /// Opaque parent-supplied native Adapter payload.
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Consume the request and return its opaque native Adapter payload.
    pub fn into_payload(self) -> Vec<u8> {
        self.payload
    }

    /// Exact plugin-owned latency/tail/mode/state contract admitted by the parent.
    pub const fn plugin_execution_contract(&self) -> AudioProcessorExecutionContract {
        self.plugin_execution_contract
    }

    /// Exact prepared auxiliary buses.
    pub const fn auxiliary_inputs(&self) -> &AudioProcessorAuxiliaryInputContract {
        &self.auxiliary_inputs
    }

    /// Exact prepared additional outputs, including unrouted ports.
    pub const fn auxiliary_outputs(&self) -> &AudioProcessorAuxiliaryOutputContract {
        &self.auxiliary_outputs
    }

    /// Exact admitted input-port and fixed Note capacities, including empty.
    pub const fn note_input_contract(&self) -> &crate::AudioProcessorNoteInputContract {
        &self.note_input_contract
    }

    /// Stable prepared parameter-lane order.
    pub fn parameter_ids(&self) -> &[ParameterId] {
        &self.parameter_ids
    }

    /// Exact render contract for every later block.
    pub const fn render_contract(&self) -> AudioRenderContract {
        self.render_contract
    }
}

/// Child-side factory for a concrete VST3, CLAP, or other native Adapter.
pub trait IsolatedAudioProcessorWorkerFactory {
    /// Same-binary owning-type request, computed before foreign plugin load.
    fn native_worker_lane_resources(
        &self,
    ) -> Result<Option<crate::NativeWorkerLaneResourceProfile>, AudioProcessorHostError> {
        Ok(None)
    }
    /// Derive a same-binary preparation-specific request before foreign load.
    /// Parsing may allocate on this non-RT path; this must not query or launch
    /// plugins. Other adapters retain their existing static request by default.
    fn native_worker_lane_resources_for_preparation(
        &self,
        _request: &IsolatedAudioProcessorWorkerPrepareRequest,
    ) -> Result<Option<crate::NativeWorkerLaneResourceProfile>, AudioProcessorHostError> {
        self.native_worker_lane_resources()
    }

    /// Load, restore, negotiate, and return one exclusive native instance.
    fn prepare(
        &self,
        request: IsolatedAudioProcessorWorkerPrepareRequest,
    ) -> Result<Box<dyn IsolatedAudioProcessorWorker>, AudioProcessorHostError>;
}

/// Exclusive native processor instance owned entirely by one Worker process.
pub trait IsolatedAudioProcessorWorker {
    /// Revalidated restored instance's Note input contract; empty by default.
    fn note_input_contract(&self) -> crate::AudioProcessorNoteInputContract {
        crate::AudioProcessorNoteInputContract::default()
    }
    /// Exact request matched before native load and retained by this actual owner.
    fn native_worker_lane_resources(&self) -> Option<crate::NativeWorkerLaneResourceProfile> {
        None
    }
    /// Actual retained staged-seed allocation, independent of the decoded request.
    /// Byte capacity and alignment are observed from the current native owner.
    fn native_staged_seed_layout(&self) -> crate::NativeHostBufferLayout {
        crate::NativeHostBufferLayout { bytes: 0, alignment: 1 }
    }
    /// Whether this concrete native adapter admits actual disjoint thread roles.
    /// This is an execution capability, never editor or authoring permission.
    fn uses_native_processing_split(&self) -> bool {
        false
    }

    /// Borrow only the same native main role while processing is not admitted.
    ///
    /// Derive this capability from the existing owner lifecycle, never a second
    /// processing flag. `None` permits the original disjoint processing borrow;
    /// `Some` must keep controller/GUI service alive without creating a DSP role.
    /// Replacement/control commands run only after this borrow has ended.
    fn native_control_only_role(
        &mut self,
    ) -> Result<Option<Box<dyn IsolatedAudioProcessorMainRole + '_>>, AudioProcessorHostError> {
        Ok(None)
    }

    /// Borrow the same native instance into its real main and DSP roles.
    /// No native owner can be replaced until both roles end and DSP joins.
    fn split_native_processing(
        &mut self,
    ) -> Result<IsolatedAudioProcessorWorkerSplit<'_>, AudioProcessorHostError> {
        Err(AudioProcessorHostError::Unavailable(
            "native adapter has no scoped main/DSP roles".into(),
        ))
    }

    /// Lower the actual native parent Close source intent through this same binding.
    /// Canonical host-frame cleanup may remain pending; no fake close is returned.
    /// Replacement/termination separately requires complete lifecycle cleanup.
    fn service_native_editor_close_request(
        &mut self,
    ) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
        Ok(crate::NativeAudioEditorOutcome::Refused(
            crate::NativeAudioEditorRefusal::UnsupportedAdapter,
        ))
    }

    /// Strict lifecycle cleanup before replacement/termination of this owner.
    /// It must succeed completely or cause isolated retirement.
    fn close_native_editor_owner(&mut self) -> Result<(), AudioProcessorHostError> {
        Ok(())
    }

    /// Actual current load facts on this child owner; no installed/author permission.
    fn native_audio_object_observation(
        &self,
    ) -> Result<Option<crate::NativeAudioObjectObservation>, AudioProcessorHostError> {
        Ok(None)
    }

    /// Observe one bounded diagnostic batch after the scoped DSP role joins.
    /// This is the sole observation request, not a second callback transport.
    fn poll_native_observations(
        &mut self,
        _consumption: crate::NativeAudioObservationConsumption,
    ) -> Result<crate::NativeAudioObservations, AudioProcessorHostError> {
        Ok(crate::NativeAudioObservations {
            object: self.native_audio_object_observation()?,
            editor_cleanup: self.native_editor_detach_stage(),
            ..Default::default()
        })
    }

    /// Acknowledge one exact object after the scoped DSP role has ended/joined.
    /// Unsupported adapters remain explicit; seed tickets are not object bindings.
    fn bind_native_audio_object(
        &mut self,
        _request: crate::NativeAudioObjectBindRequest,
    ) -> Result<crate::NativeAudioObjectBindOutcome, AudioProcessorHostError> {
        Ok(crate::NativeAudioObjectBindOutcome::Refused(
            crate::NativeAudioObjectBindRefusal::UnsupportedAdapter,
        ))
    }

    /// Attach this actual bound native object's view after DSP join.
    /// The sole Runtime has already admitted its installed owner lease.
    fn open_native_audio_editor(
        &mut self,
        _request: crate::NativeAudioEditorOpenRequest,
    ) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
        Ok(crate::NativeAudioEditorOutcome::Refused(
            crate::NativeAudioEditorRefusal::UnsupportedAdapter,
        ))
    }

    /// Close this same bound view; native teardown failure retains its owner.
    fn close_native_audio_editor(
        &mut self,
        _binding: crate::NativeAudioObjectBindRequest,
    ) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
        Ok(crate::NativeAudioEditorOutcome::Refused(
            crate::NativeAudioEditorRefusal::UnsupportedAdapter,
        ))
    }

    /// Read only the sole actual native attachment's unresolved detach state.
    /// This is lifecycle observation, never an independent author permission.
    fn native_editor_detach_stage(&self) -> Option<crate::NativeAudioEditorCloseStage> {
        None
    }

    /// Pure snapshot for a subsequent bounded non-RT service on this exact owner.
    fn prepare_native_author_state_entry(
        &mut self,
        _request: crate::NativeAudioAuthorStateEntryRequest<'_>,
    ) -> Result<crate::NativeAudioAuthorStateEntryOutcome, AudioProcessorHostError> {
        Ok(crate::NativeAudioAuthorStateEntryOutcome::Refused(
            crate::NativeAudioAuthorStateEntryRefusal::UnsupportedAdapter,
        ))
    }

    /// Pure snapshot for a subsequent bounded non-RT service on this exact owner.
    fn native_configuration_cohort(&self) -> Option<NativeRequestCohort> {
        None
    }

    /// Legally reactivate and capture retained configuration; never install a plan.
    fn service_native_configuration(
        &mut self,
        _request: NativeConfigurationControlRequest<'_>,
    ) -> Result<NativeConfigurationControlOutcome, AudioProcessorHostError> {
        Ok(NativeConfigurationControlOutcome::CapabilityRefused(
            NativeConfigurationRefusal::UnsupportedAdapter,
        ))
    }

    /// Adopt a bounded same-child seed without processing, plan or clock publication.
    fn stage_native_configuration_seed(
        &mut self,
        _seed: NativeConfigurationOwnedSeed,
    ) -> Result<NativeConfigurationSeedOutcome, AudioProcessorHostError> {
        Ok(NativeConfigurationSeedOutcome::Refused(
            NativeConfigurationSeedRefusal::UnsupportedAdapter,
        ))
    }

    /// Move a preowned exact seed after a current native request serial claim.
    fn commit_native_configuration_seed(
        &mut self,
        _ticket: NativeConfigurationSeedTicket,
    ) -> Result<NativeConfigurationSeedOutcome, AudioProcessorHostError> {
        Ok(NativeConfigurationSeedOutcome::Refused(
            NativeConfigurationSeedRefusal::UnsupportedAdapter,
        ))
    }

    /// Pure observation of recorded callbacks; never service control or metadata.
    fn pending_reprepare(&self) -> Option<crate::NativeObservedRequest> {
        None
    }

    /// Revalidated native scheduling and mode facts.
    fn execution_contract(&self) -> AudioProcessorExecutionContract;

    /// Revalidated native auxiliary-bus facts.
    fn auxiliary_input_contract(&self) -> AudioProcessorAuxiliaryInputContract;

    /// Revalidated additional outputs. Old single-main adapters retain an empty contract.
    fn auxiliary_output_contract(&self) -> AudioProcessorAuxiliaryOutputContract {
        AudioProcessorAuxiliaryOutputContract::default()
    }

    /// Optional native display names captured during control-side preparation.
    fn auxiliary_input_names(&self) -> Vec<crate::AudioProcessorInputName> {
        Vec::new()
    }

    /// Reset native history for one fresh continuity epoch.
    fn enter_state(&mut self, start_sample: i64) -> Result<(), AudioProcessorHostError>;

    /// Process one exact block through the concrete native ABI.
    fn process(
        &mut self,
        block: IsolatedAudioProcessorWorkerBlock<'_>,
    ) -> Result<(), AudioProcessorHostError>;
}

/// One immutable parameter event exposed inside the Worker process.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IsolatedAudioParameterEvent {
    /// Zero-based sample offset in the current block.
    pub sample_offset: u32,
    /// Exact definition-domain value supplied by the common Processor Host.
    pub value: f64,
}

/// Allocation-free iterator over one stable parameter lane.
pub struct IsolatedAudioParameterEvents<'a> {
    lane: u32,
    events: &'a [SharedParameterEvent],
    index: usize,
}

/// Allocation-free ordered input Note view, validated before native dispatch.
pub struct IsolatedAudioNoteEvents<'a> {
    events: std::slice::Iter<'a, SharedNoteEvent>,
}

impl Iterator for IsolatedAudioNoteEvents<'_> {
    type Item = crate::AudioNoteEvent;

    fn next(&mut self) -> Option<Self::Item> {
        let event = *self.events.next()?;
        Some(crate::AudioNoteEvent {
            sample_offset: event.sample_offset,
            native_note_id: event.native_id,
            port_index: event.port_index as u16,
            channel: event.channel as u8,
            key: event.key as u8,
            kind: if event.kind == 0 {
                crate::AudioNoteEventKind::Off
            } else {
                crate::AudioNoteEventKind::On
            },
            velocity: event.velocity,
        })
    }
}

static EMPTY_NOTE_INPUT_CONTRACT: crate::AudioProcessorNoteInputContract =
    crate::AudioProcessorNoteInputContract {
        completion: crate::AudioNoteCompletionPolicy::NativeEnd,
        ports: Vec::new(),
        limits: crate::AudioNoteExecutionLimits {
            input_events: 0,
            output_ends: 0,
            retained_records: 0,
            live_voices: 0,
        },
    };

struct WorkerNoteOutput<'a> {
    events: &'a mut [SharedNoteEnd],
    count: &'a std::cell::Cell<usize>,
    refused: &'a std::cell::Cell<bool>,
}

impl Iterator for IsolatedAudioParameterEvents<'_> {
    type Item = IsolatedAudioParameterEvent;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(event) = self.events.get(self.index) {
            self.index += 1;
            if event.lane == self.lane {
                return Some(IsolatedAudioParameterEvent {
                    sample_offset: event.sample_offset,
                    value: event.value,
                });
            }
        }
        None
    }
}

/// Borrowed fixed-capacity audio and parameter views inside one Worker callback.
pub struct IsolatedAudioProcessorWorkerBlock<'a> {
    musical_time_reference: Option<mondrian_core::MusicalTimeReference>,
    start_sample: i64,
    frames: usize,
    render_contract: AudioRenderContract,
    main: &'a mut [f32],
    auxiliary_inputs: &'a AudioProcessorAuxiliaryInputContract,
    auxiliary: &'a [f32],
    auxiliary_outputs: Option<&'a AudioProcessorAuxiliaryOutputContract>,
    output: &'a mut [f32],
    parameter_ids: &'a [ParameterId],
    events: &'a [SharedParameterEvent],
    note_input_contract: Option<&'a crate::AudioProcessorNoteInputContract>,
    note_events: &'a [SharedNoteEvent],
    note_output: Option<WorkerNoteOutput<'a>>,
}

impl<'a> IsolatedAudioProcessorWorkerBlock<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        start_sample: i64,
        frames: usize,
        render_contract: AudioRenderContract,
        main: &'a mut [f32],
        auxiliary_inputs: &'a AudioProcessorAuxiliaryInputContract,
        auxiliary: &'a [f32],
        parameter_ids: &'a [ParameterId],
        events: &'a [SharedParameterEvent],
    ) -> Self {
        Self {
            start_sample,
            musical_time_reference: None,
            frames,
            render_contract,
            main,
            auxiliary_inputs,
            auxiliary,
            auxiliary_outputs: None,
            output: &mut [],
            parameter_ids,
            events,
            note_input_contract: None,
            note_events: &[],
            note_output: None,
        }
    }

    fn with_auxiliary_outputs(
        mut self,
        contract: &'a AudioProcessorAuxiliaryOutputContract,
        output: &'a mut [f32],
    ) -> Self {
        self.auxiliary_outputs = Some(contract);
        self.output = output;
        self
    }

    fn with_musical_time_reference(
        mut self,
        reference: Option<mondrian_core::MusicalTimeReference>,
    ) -> Self {
        self.musical_time_reference = reference;
        self
    }

    /// Prepared Sequence reference; no beat or bar facts are implied.
    pub const fn musical_time_reference(&self) -> Option<mondrian_core::MusicalTimeReference> {
        self.musical_time_reference
    }

    /// Mutable additional output in the frozen native order, keyed by definition identity.
    pub fn auxiliary_output(&mut self, bus_key: &str) -> Option<&mut [f32]> {
        let index = self.auxiliary_outputs?.buses.iter().position(|bus| bus.bus_key == bus_key)?;
        let samples = self.frames.checked_mul(self.render_contract.channel_count())?;
        let start = index.checked_mul(samples)?;
        self.output.get_mut(start..start.checked_add(samples)?)
    }

    fn with_note_regions(
        mut self,
        contract: &'a crate::AudioProcessorNoteInputContract,
        events: &'a [SharedNoteEvent],
        ends: &'a mut [SharedNoteEnd],
        count: &'a std::cell::Cell<usize>,
        refused: &'a std::cell::Cell<bool>,
    ) -> Self {
        self.note_input_contract = Some(contract);
        self.note_events = events;
        self.note_output = Some(WorkerNoteOutput { events: ends, count, refused });
        self
    }

    /// Exact restored Note input metadata borrowed from this prepared operation.
    pub fn note_input_contract(&self) -> &crate::AudioProcessorNoteInputContract {
        self.note_input_contract.unwrap_or(&EMPTY_NOTE_INPUT_CONTRACT)
    }

    /// Iterate original f64 Note values in the admitted prepared order.
    pub fn note_events(&self) -> IsolatedAudioNoteEvents<'_> {
        IsolatedAudioNoteEvents { events: self.note_events.iter() }
    }

    /// Copy a bounded End address without a worker-side voice ledger. Any
    /// refusal remains latched for the whole call, even if its return is ignored.
    pub fn try_push_note_end(&mut self, event: crate::AudioNoteEndEvent) -> bool {
        let ports = self.note_input_contract().ports.len();
        let Some(output) = self.note_output.as_mut() else {
            return false;
        };
        if output.refused.get() {
            return false;
        }
        let Ok(event) = SharedNoteEnd::capture(event, ports) else {
            output.refused.set(true);
            return false;
        };
        let Some(destination) = output.events.get_mut(output.count.get()) else {
            output.refused.set(true);
            return false;
        };
        *destination = event;
        output.count.set(output.count.get() + 1);
        true
    }

    /// First absolute Sequence-domain sample in this block.
    pub const fn start_sample(&self) -> i64 {
        self.start_sample
    }

    /// Exact frame count in this block.
    pub const fn frames(&self) -> usize {
        self.frames
    }

    /// Prepared sample rate.
    pub const fn sample_rate(&self) -> u32 {
        self.render_contract.sample_rate
    }

    /// Prepared semantic layout.
    pub const fn channel_layout(&self) -> AudioChannelLayout {
        self.render_contract.channel_layout
    }

    /// Prepared realtime/offline mode.
    pub const fn processing_mode(&self) -> AudioProcessingMode {
        self.render_contract.processing_mode
    }

    /// Mutable interleaved main-bus PCM.
    pub fn main_interleaved(&mut self) -> &mut [f32] {
        self.main
    }

    /// Read one exact auxiliary bus by its stable definition key.
    pub fn auxiliary_input(&self, bus_key: &str) -> Option<&[f32]> {
        let bus_index =
            self.auxiliary_inputs.buses.iter().position(|bus| bus.bus_key == bus_key)?;
        let samples = self.frames.checked_mul(self.render_contract.channel_count())?;
        let start = bus_index.checked_mul(samples)?;
        let end = start.checked_add(samples)?;
        self.auxiliary.get(start..end)
    }

    /// Borrow mutable main PCM and one disjoint read-only auxiliary bus together.
    pub fn main_and_auxiliary_input(&mut self, bus_key: &str) -> Option<(&mut [f32], &[f32])> {
        let bus_index =
            self.auxiliary_inputs.buses.iter().position(|bus| bus.bus_key == bus_key)?;
        let samples = self.frames.checked_mul(self.render_contract.channel_count())?;
        let start = bus_index.checked_mul(samples)?;
        let end = start.checked_add(samples)?;
        Some((self.main, self.auxiliary.get(start..end)?))
    }

    /// Number of stable parameter lanes.
    pub fn parameter_lane_count(&self) -> usize {
        self.parameter_ids.len()
    }

    /// Stable parameter identity for one lane.
    pub fn parameter_id(&self, lane: usize) -> Option<&ParameterId> {
        self.parameter_ids.get(lane)
    }

    /// Iterate exact sample-accurate events for one lane without allocation.
    pub fn parameter_events(&self, lane: usize) -> Option<IsolatedAudioParameterEvents<'_>> {
        self.parameter_ids.get(lane)?;
        Some(IsolatedAudioParameterEvents {
            lane: u32::try_from(lane).ok()?,
            events: self.events,
            index: 0,
        })
    }
}

/// Run one packaged isolated Worker mode in the current process.
///
/// Product entrypoints must dispatch this before UI, media, GPU, or audio-device
/// initialization. The endpoint and nonce are inherited through private
/// environment variables; native paths and opaque state never appear in argv.
pub fn run_isolated_audio_processor_worker(
    factory: &dyn IsolatedAudioProcessorWorkerFactory,
) -> Result<(), AudioProcessorHostError> {
    worker::run_worker(factory)
}

#[cfg(test)]
mod note_spec_contracts {
    use super::*;

    #[test]
    fn native_child_parent_slot_adds_exactly_one_existing_session_charge() {
        let spec = spec();
        let layout = SharedBlockLayout::new(2, 1, 0, 0).unwrap();
        let base = spec
            .plugin_execution_contract
            .session_scratch_bytes()
            .checked_add(layout.total_bytes)
            .unwrap()
            .checked_add(
                isolated_output_contract_clone_reservation_bytes(&spec.auxiliary_outputs).unwrap(),
            )
            .unwrap();
        assert!(spec.native_lane.is_none());
        let expected = base
            .checked_add(
                std::alloc::Layout::new::<Option<mondrian_core::NativeChildResultProducer>>()
                    .size(),
            )
            .unwrap();
        let contract =
            IsolatedAudioProcessorFactory::requested_execution_contract(&spec, layout).unwrap();
        assert_eq!(contract.session_scratch_bytes(), expected);
        let max = AudioProcessorExecutionContract::new(
            0,
            crate::AudioProcessorTail::None,
            false,
            true,
            true,
            usize::MAX - supervisor::child_settlement_slot_bytes() + 1,
        )
        .unwrap();
        assert!(max
            .with_additional_session_scratch_bytes(supervisor::child_settlement_slot_bytes())
            .is_err());
    }

    fn spec() -> IsolatedAudioProcessorWorkerSpec {
        IsolatedAudioProcessorWorkerSpec::new(
            PathBuf::from("typed-only-not-launched"),
            Vec::new(),
            AudioProcessorExecutionContract::new(
                0,
                crate::AudioProcessorTail::None,
                false,
                true,
                true,
                0,
            )
            .unwrap(),
            AudioProcessorAuxiliaryInputContract::default(),
            Vec::new(),
            AudioRenderContract {
                sample_rate: 48_000,
                channel_layout: AudioChannelLayout::Mono,
                max_block_frames: 2,
                processing_mode: AudioProcessingMode::Offline,
                processor_session_scratch_budget_bytes: 0,
                public_output_lookahead_budget_frames: 0,
                compensation_delay_scratch_budget_bytes: 0,
            },
        )
        .unwrap()
    }

    fn contract() -> crate::AudioProcessorNoteInputContract {
        crate::AudioProcessorNoteInputContract {
            completion: crate::AudioNoteCompletionPolicy::NativeEnd,
            ports: vec![crate::AudioProcessorNoteInputPort {
                channel_count: 16,
                port_key: "main-notes".to_owned(),
                native_id: 27,
                native_index: 0,
            }],
            limits: crate::AudioNoteExecutionLimits {
                input_events: 2,
                output_ends: 2,
                retained_records: 2,
                live_voices: 2,
            },
        }
    }

    #[test]
    fn note_spec_default_is_zero_and_invalid_or_boundary_plus_one_contract_refuses() {
        assert_eq!(
            spec().note_input_contract,
            crate::AudioProcessorNoteInputContract::default()
        );
        let valid = contract();
        assert_eq!(
            spec().with_note_input_contract(valid.clone()).unwrap().note_input_contract,
            valid
        );
        let mut invalid = valid.clone();
        invalid.limits.input_events = 4097;
        assert!(spec().with_note_input_contract(invalid).is_err());
        let mut invalid = valid.clone();
        invalid.limits.output_ends = 4097;
        assert!(spec().with_note_input_contract(invalid).is_err());
        let mut invalid = valid.clone();
        invalid.ports[0].native_index = 1;
        assert!(spec().with_note_input_contract(invalid).is_err());
        let mut invalid = crate::AudioProcessorNoteInputContract::default();
        invalid.limits.input_events = 1;
        assert!(spec().with_note_input_contract(invalid).is_err());
        let mut max = valid;
        max.limits.input_events = 4096;
        max.limits.output_ends = 4096;
        assert!(spec().with_note_input_contract(max).is_ok());
    }

    #[test]
    fn execution_restart_factory_bound_and_rebind_preserve_fixed_metadata_and_old_owner() {
        use sha2::{Digest, Sha256};
        let mut spec = spec();
        spec.plugin_execution_contract = AudioProcessorExecutionContract::new(
            3,
            crate::AudioProcessorTail::Finite(3),
            true,
            true,
            true,
            17,
        )
        .unwrap();
        spec.helper_arguments.push(OsString::from("immutable-argument"));
        spec.preparation_payload = serde_json::to_vec(&serde_json::json!({
            "main_audio_input": clap_worker::ClapMainAudioInput::Present,
            "main_audio_output_id": 0, "auxiliary_outputs": [],
            "library_path": "unchanged.clap", "plugin_id": "unchanged",
            "binary_sha256": vec![7;32], "state": [0,1],
            "parameters": [], "parameter_ids": []
        }))
        .unwrap();
        let layout = SharedBlockLayout::new(2, 1, 0, 0).unwrap();
        let factory = IsolatedAudioProcessorFactory {
            execution_contract: IsolatedAudioProcessorFactory::requested_execution_contract(
                &spec, layout,
            )
            .unwrap(),
            spec,
            layout,
            input_names: Vec::new(),
            retained_create_test_hook: None,
        };
        // Manual metadata fixture only. No worker/probe/native authority is
        // claimed by this direct test construction or by the synthetic ticket.
        let old_payload = factory.spec.preparation_payload.clone();
        let old_contract = factory.execution_contract();
        let configuration = [255, 0, 9];
        let identity = NativeConfigurationSeedTicket {
            serviced_cohort: NativeRequestCohort::new(
                1,
                mondrian_core::NativeAudioReprepareRequest::ClapRestart,
            )
            .unwrap(),
            control_token: std::num::NonZeroU64::new(1).unwrap(),
            observed_contract: AudioProcessorExecutionContract::new(
                7,
                crate::AudioProcessorTail::Finite(7),
                true,
                true,
                true,
                17,
            )
            .unwrap(),
            format: NativeConfigurationFormat::ClapState,
            configuration_bytes: configuration.len(),
            configuration_sha256: Sha256::digest(configuration).into(),
        };
        let (bound, counts) = measure_host_allocations_for_test(|| {
            factory.native_reprepare_factory_bound(identity, configuration.len())
        });
        let bound = bound.unwrap();
        assert_eq!(
            counts, [0; 4],
            "bound must not allocate, even for metadata ownership"
        );
        let codec = clap_worker::reprepare_codec_bound(old_payload.len()).unwrap();
        let clones = factory.known_reprepare_factory_bytes().unwrap()
            - factory.spec.preparation_payload.capacity();
        assert_eq!(
            bound,
            codec
                + clones
                + clap_worker::reprepare_payload_output_bound(
                    old_payload.len(),
                    configuration.len()
                )
                .unwrap()
        );
        let mut wrong = identity;
        wrong.configuration_sha256[0] ^= 1;
        assert!(factory
            .with_native_reprepare_configuration(wrong, &configuration, &mut |_| {})
            .is_err());
        wrong = identity;
        wrong.observed_contract = AudioProcessorExecutionContract::new(
            7,
            crate::AudioProcessorTail::Finite(7),
            true,
            false,
            true,
            17,
        )
        .unwrap();
        assert!(factory.native_reprepare_factory_bound(wrong, configuration.len()).is_none());
        assert!(factory
            .with_native_reprepare_configuration(wrong, &configuration, &mut |_| {})
            .unwrap()
            .is_none());
        let mut peak = 0;
        let next = factory
            .rebind_execution_configuration(identity, &configuration, &mut |bytes| {
                peak = peak.max(bytes)
            })
            .unwrap()
            .unwrap();
        assert_eq!(
            next.execution_contract(),
            IsolatedAudioProcessorFactory::requested_execution_contract(&next.spec, next.layout)
                .unwrap()
        );
        assert_eq!(next.execution_contract().algorithmic_latency_frames(), 7);
        assert_eq!(
            next.execution_contract().tail(),
            crate::AudioProcessorTail::Finite(7)
        );
        assert_eq!(
            next.execution_contract().session_scratch_bytes(),
            old_contract.session_scratch_bytes()
        );
        assert!(peak >= codec + next.known_reprepare_factory_bytes().unwrap());
        assert!(peak <= bound);
        let before: serde_json::Value = serde_json::from_slice(&old_payload).unwrap();
        let mut after: serde_json::Value =
            serde_json::from_slice(&next.spec.preparation_payload).unwrap();
        assert_eq!(after["state"], serde_json::json!(configuration));
        after["state"] = before["state"].clone();
        assert_eq!(after, before, "all eight non-state fields remain identical");
        assert_eq!(next.spec.helper_arguments, factory.spec.helper_arguments);
        assert_eq!(next.spec.render_contract, factory.spec.render_contract);
        let public = factory
            .with_native_reprepare_configuration(identity, &configuration, &mut |_| {})
            .unwrap()
            .unwrap();
        assert_eq!(public.execution_contract(), next.execution_contract());
        assert_eq!(factory.spec.preparation_payload, old_payload);
        assert_eq!(factory.execution_contract(), old_contract);
    }

    #[test]
    fn vst3_execution_restart_factory_bound_and_rebind_require_exact_flags_and_contract() {
        use sha2::{Digest, Sha256};
        let mut spec = spec();
        spec.plugin_execution_contract = AudioProcessorExecutionContract::new(
            3,
            crate::AudioProcessorTail::Finite(3),
            true,
            true,
            true,
            17,
        )
        .unwrap();
        spec.preparation_payload = serde_json::to_vec(&serde_json::json!({
            "schema_version": 3, "main_audio_input": Vst3MainAudioInput::Present,
            "event_bus_contract": Vst3EventBusContractV1::empty(),
            "binary_path": "not-loaded.vst3", "class_id": "unchanged", "vendor": "",
            "binary_sha256": vec![7;32], "state": [1,2],
            "parameters": [], "parameter_ids": [],
        }))
        .unwrap();
        let layout = SharedBlockLayout::new(2, 1, 0, 0).unwrap();
        let factory = IsolatedAudioProcessorFactory {
            execution_contract: IsolatedAudioProcessorFactory::requested_execution_contract(
                &spec, layout,
            )
            .unwrap(),
            spec,
            layout,
            input_names: Vec::new(),
            retained_create_test_hook: None,
        };
        // Manual storage/codec control only, not a native plugin or an installed
        // lease. The ignored mode5/6 consumer supplies the actual candidate.
        let old_payload = factory.spec.preparation_payload.clone();
        let old_contract = factory.execution_contract();
        let configuration = [255, 0, 9];
        let identity = NativeConfigurationSeedTicket {
            serviced_cohort: NativeRequestCohort::new(
                1,
                mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 8 },
            )
            .unwrap(),
            control_token: std::num::NonZeroU64::new(1).unwrap(),
            observed_contract: AudioProcessorExecutionContract::new(
                7,
                crate::AudioProcessorTail::Finite(7),
                true,
                true,
                true,
                17,
            )
            .unwrap(),
            format: NativeConfigurationFormat::Vst3ComponentAndController,
            configuration_bytes: configuration.len(),
            configuration_sha256: Sha256::digest(configuration).into(),
        };
        let (bound, counts) = measure_host_allocations_for_test(|| {
            factory.native_reprepare_factory_bound(identity, configuration.len())
        });
        let bound = bound.unwrap();
        assert_eq!(counts, [0; 4], "Factory bound is a pure read");
        let codec = vst3_worker::reprepare_codec_bound(old_payload.len()).unwrap();
        assert_eq!(
            bound,
            codec
                + vst3_worker::reprepare_payload_output_bound(
                    old_payload.len(),
                    configuration.len()
                )
                .unwrap()
                + factory.known_reprepare_factory_bytes().unwrap()
                - factory.spec.preparation_payload.capacity()
        );
        let mut rejected = Vec::new();
        for flags in [2, 9, u32::MAX] {
            let mut wrong = identity;
            wrong.serviced_cohort = NativeRequestCohort::new(
                1,
                mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags },
            )
            .unwrap();
            rejected.push(wrong);
        }
        let mut wrong = identity;
        wrong.serviced_cohort.request =
            mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 0 };
        rejected.push(wrong);
        wrong = identity;
        wrong.serviced_cohort.serial = 0;
        rejected.push(wrong);
        wrong = identity;
        wrong.serviced_cohort.serial = u32::MAX;
        rejected.push(wrong);
        wrong = identity;
        wrong.format = NativeConfigurationFormat::ClapState;
        rejected.push(wrong);
        wrong = identity;
        wrong.serviced_cohort =
            NativeRequestCohort::new(1, mondrian_core::NativeAudioReprepareRequest::ClapRestart)
                .unwrap();
        rejected.push(wrong);
        wrong = identity;
        wrong.configuration_bytes += 1;
        rejected.push(wrong);
        for contract in [
            AudioProcessorExecutionContract::new(
                0,
                crate::AudioProcessorTail::None,
                false,
                true,
                true,
                17,
            )
            .unwrap(),
            AudioProcessorExecutionContract::new(
                7,
                crate::AudioProcessorTail::Finite(7),
                true,
                false,
                true,
                17,
            )
            .unwrap(),
            AudioProcessorExecutionContract::new(
                7,
                crate::AudioProcessorTail::Finite(7),
                true,
                true,
                false,
                17,
            )
            .unwrap(),
            AudioProcessorExecutionContract::new(
                7,
                crate::AudioProcessorTail::Finite(7),
                true,
                true,
                true,
                18,
            )
            .unwrap(),
        ] {
            let mut wrong = identity;
            wrong.observed_contract = contract;
            rejected.push(wrong);
        }
        for wrong in rejected {
            assert!(factory.native_reprepare_factory_bound(wrong, configuration.len()).is_none());
            let mut queried = false;
            assert!(factory
                .with_native_reprepare_configuration(wrong, &configuration, &mut |_| queried = true)
                .unwrap()
                .is_none());
            assert!(!queried);
        }
        wrong = identity;
        wrong.configuration_sha256[0] ^= 1;
        assert!(factory
            .with_native_reprepare_configuration(wrong, &configuration, &mut |_| {})
            .is_err());
        let mut peak = 0;
        let next = factory
            .rebind_execution_configuration(identity, &configuration, &mut |n| peak = peak.max(n))
            .unwrap()
            .unwrap();
        assert_eq!(
            next.spec.plugin_execution_contract,
            identity.observed_contract
        );
        assert_eq!(
            next.execution_contract(),
            IsolatedAudioProcessorFactory::requested_execution_contract(&next.spec, next.layout)
                .unwrap()
        );
        assert_eq!(
            next.execution_contract().session_scratch_bytes(),
            old_contract.session_scratch_bytes()
        );
        assert!(peak >= codec + next.known_reprepare_factory_bytes().unwrap());
        assert!(peak <= bound);
        let before: serde_json::Value = serde_json::from_slice(&old_payload).unwrap();
        let mut after: serde_json::Value =
            serde_json::from_slice(&next.spec.preparation_payload).unwrap();
        assert_eq!(after["state"], serde_json::json!(configuration));
        after["state"] = before["state"].clone();
        assert_eq!(after, before, "all nine non-state fields remain identical");
        let public = factory
            .with_native_reprepare_configuration(identity, &configuration, &mut |_| {})
            .unwrap()
            .unwrap();
        assert_eq!(public.execution_contract(), next.execution_contract());
        assert_eq!(factory.spec.preparation_payload, old_payload);
        assert_eq!(factory.execution_contract(), old_contract);
    }

    #[test]
    fn note_contract_clone_and_actual_factory_visitor_preserve_complete_metadata_owners() {
        let spec = spec().with_note_input_contract(contract()).unwrap();
        let cloned = spec.clone();
        assert_eq!(cloned.note_input_contract, spec.note_input_contract);
        assert_ne!(
            cloned.note_input_contract.ports.as_ptr(),
            spec.note_input_contract.ports.as_ptr()
        );
        let layout = SharedBlockLayout::new(2, 1, 0, 0).unwrap().with_notes(2, 2).unwrap();
        // Manual metadata fixture: no validation launch or native qualification.
        let factory = IsolatedAudioProcessorFactory {
            input_names: Vec::new(),
            execution_contract: spec.plugin_execution_contract,
            spec,
            layout,
            retained_create_test_hook: None,
        };
        assert_eq!(factory.note_input_contract(), contract());
        let mut visited = Vec::new();
        factory
            .visit_host_owned_storage(&mut |storage| {
                visited.push(storage);
                Ok(())
            })
            .unwrap();
        let ports = &factory.spec.note_input_contract.ports;
        let key = &ports[0].port_key;
        assert!(visited.iter().any(|storage| storage.kind()
            == crate::AudioProcessorFactoryStorageKind::CompleteAllocation
            && storage.address() == ports.as_ptr() as usize
            && storage.layout().size()
                == ports.capacity() * size_of::<crate::AudioProcessorNoteInputPort>()));
        assert!(visited.iter().any(|storage| storage.kind()
            == crate::AudioProcessorFactoryStorageKind::CompleteAllocation
            && storage.address() == key.as_ptr() as usize
            && storage.layout().size() == key.capacity()));
    }
}

fn invalid_contract(detail: &str) -> AudioProcessorHostError {
    AudioProcessorHostError::InvalidContract(detail.to_owned())
}

mod native_worker_ingress;
mod native_worker_processing;
mod native_worker_roles;
pub use native_worker_roles::{
    IsolatedAudioProcessorMainRole, IsolatedAudioProcessorProcessingRole,
    IsolatedAudioProcessorWorkerSplit,
};

#[cfg(test)]
pub(crate) use clap_worker::measure_host_allocations_for_test;
