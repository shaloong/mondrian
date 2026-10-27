//! CLAP ABI execution inside the supervised audio worker process.

#[cfg(all(test, windows))]
#[path = "tests/native_clap_note_gate.rs"]
mod native_note_gate_controls;

#[cfg(test)]
#[path = "tests/native_clap_outputs.rs"]
mod native_outputs_controls;

use super::clap_audio_role::{ClapAudioRole, ClapAudioThreadGuard};
#[cfg(test)]
use super::clap_discovery::list_parameters;
use super::clap_discovery::{
    fingerprint_clap_binary, inspect_audio_ports, list_parameters_observed,
    probe_clap_plugin_registration_with_names, probe_clap_plugin_registration_with_state,
    validate_parameters, ParameterQueryCapture,
};
use super::clap_main_services::{
    ClapHostMainThread, ClapNativeServicesOwner, ClapNativeServicesShared,
};
use super::clap_output_events::{ClapOutputEvents, ClapOutputOrigin};
use super::clap_parameter_events::ClapInputEvents;
use super::clap_signal_fields::ClapSignalFields;
use super::native_request_cohort::{NativeRequestMarker, RecordedRequestCohort};
#[cfg(test)]
use super::probe_clap_plugin_registration;
use super::{
    scan_clap_library_descriptors, ClapPluginDescriptor, IsolatedAudioProcessorSpecResolver,
    IsolatedAudioProcessorWorker, IsolatedAudioProcessorWorkerBlock,
    IsolatedAudioProcessorWorkerFactory, IsolatedAudioProcessorWorkerPrepareRequest,
    IsolatedAudioProcessorWorkerSpec,
};
use crate::{
    AudioProcessorAuxiliaryInputContract, AudioProcessorAuxiliaryOutputContract,
    AudioProcessorExecutionContract, AudioProcessorHostError, AudioProcessorPrepareRequest,
};
use clack_extensions::latency::PluginLatency;
use clack_extensions::log::{HostLog, HostLogImpl, HostLogMessage};
use clack_extensions::render::{PluginRender, RenderMode};
use clack_extensions::state::PluginState;
use clack_extensions::tail::{PluginTail, TailLength};
use clack_extensions::thread_check::{HostThreadCheck, HostThreadCheckImpl};
use clack_host::events::event_types::{ParamValueEvent, TransportEvent, TransportFlags};
use clack_host::events::{EventFlags, EventHeader};
use clack_host::prelude::{
    AudioPorts, ClapId, HostExtensions, HostHandlers, HostInfo, Pckn, PluginAudioConfiguration,
    PluginAudioProcessor, PluginEntry, PluginInstance, SharedHandler,
};
use clack_host::utils::{BeatTime, SecondsTime};
use mondrian_core::{AudioProcessorInstanceId, AuthoringMap, ExactAutomationCurve};
use mondrian_timeline::audio::{
    AudioProcessorDefinitionRef, AudioProcessorInstance, AudioProcessorParameter,
    AudioProcessorParameterUiMetadata,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::{self, ThreadId};

#[path = "clap_worker/beat_position.rs"]
mod beat_position;
#[path = "clap_worker/native_author.rs"]
mod native_author;
#[path = "clap_worker/parameter_observations.rs"]
mod parameter_observations;
#[cfg(windows)]
#[path = "clap_worker/static_parameters.rs"]
mod static_parameters;

pub(super) struct ClapHost;

#[derive(Clone)]
pub(super) struct ClapHostShared {
    pub(super) note_input_enabled: bool,
    restart_requested: Arc<NativeRequestMarker>,
    callback_requested: Arc<AtomicBool>,
    main_thread: ThreadId,
    audio_thread: Arc<ClapAudioRole>,
    pub(super) parameter_source: Arc<super::clap_parameter_source::ClapParameterSource>,
}

impl ClapHostShared {
    pub(super) fn new() -> Self {
        Self::new_mode(false)
    }

    pub(super) fn new_with_note_input() -> Self {
        Self::new_mode(true)
    }

    fn new_mode(note_input_enabled: bool) -> Self {
        Self {
            note_input_enabled,
            restart_requested: Arc::new(NativeRequestMarker::new()),
            callback_requested: Arc::new(AtomicBool::new(false)),
            main_thread: thread::current().id(),
            audio_thread: Arc::new(ClapAudioRole::new()),
            parameter_source: Arc::new(super::clap_parameter_source::ClapParameterSource::new()),
        }
    }

    /// Mark only the synchronous CLAP audio-role calls, retaining fixed main identity.
    ///
    /// The guard can only be released on its owner thread and unwind restores
    /// the previous mark. Nested markers preserve the enclosing role; they do
    /// not detect native processor reentry.
    pub(super) fn enter_audio_thread(&self) -> ClapAudioThreadGuard<'_> {
        self.audio_thread.enter(self.main_thread)
    }

    pub(super) fn service_callbacks(
        &self,
        instance: &mut PluginInstance<ClapHost>,
    ) -> Result<(), AudioProcessorHostError> {
        for _ in 0..16 {
            if !self.callback_requested.swap(false, Ordering::AcqRel) {
                return Ok(());
            }
            instance.call_on_main_thread_callback();
        }
        // A request made by the last callback remains pending for the next
        // bounded main-thread tick. Continuous requests are legal; yielding
        // must neither consume that request nor classify it as a plugin fault.
        Ok(())
    }

    // Read the existing bounded callback source without retiring requests.
    pub(super) fn has_unserviced_callbacks(&self) -> bool {
        self.callback_requested.load(Ordering::Acquire)
    }

    pub(super) fn restart_requested(&self) -> bool {
        self.restart_requested.snapshot().pending()
    }

    pub(super) fn restart_cohort(&self) -> RecordedRequestCohort {
        self.restart_requested.snapshot()
    }

    pub(super) fn claim_retired_restart_serial(&self, serial: u32) -> bool {
        self.restart_requested.claim_retired_serial(serial)
    }

    pub(super) fn retire_restart_cohort(&self, expected: RecordedRequestCohort) -> bool {
        self.restart_requested.retire(expected)
    }
}

impl HostThreadCheckImpl for ClapHostShared {
    fn is_main_thread(&self) -> bool {
        thread::current().id() == self.main_thread
    }

    fn is_audio_thread(&self) -> bool {
        self.audio_thread.is_current(self.is_main_thread())
    }
}

/// Set the optional native render mode while the restored instance is deactivated.
pub(super) fn configure_render_mode(
    handle: &clack_host::prelude::PluginMainThreadHandle<'_>,
    mode: crate::AudioProcessingMode,
) -> Result<(), AudioProcessorHostError> {
    let Some(extension) = handle.get_extension::<PluginRender>() else {
        // Absence declares the same default realtime algorithm for both consumers.
        return Ok(());
    };
    if mode == crate::AudioProcessingMode::Offline && extension.has_realtime_requirement(handle) {
        return Err(unavailable("CLAP plugin requires real-time rendering"));
    }
    extension
        .set(
            handle,
            match mode {
                crate::AudioProcessingMode::Realtime => RenderMode::Realtime,
                crate::AudioProcessingMode::Offline => RenderMode::Offline,
            },
        )
        .map_err(|error| unavailable(format!("CLAP render mode was declined: {error}")))
}

impl SharedHandler<'_> for ClapHostShared {
    fn request_restart(&self) {
        self.restart_requested.record(1);
    }

    fn request_process(&self) {}

    fn request_callback(&self) {
        self.callback_requested.store(true, Ordering::Release);
    }
}

impl HostHandlers for ClapHost {
    type Shared<'a> = ClapNativeHostShared;
    type MainThread<'a> = ClapHostMainThread<'a>;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, shared: &Self::Shared<'_>) {
        builder.register::<HostThreadCheck>().register::<HostLog>();
        if shared.control.note_input_enabled {
            // One same-ID raw/shared table, never also legacy HostNotePorts.
            builder.register::<clack_extensions::note_ports::HostNotePortsRaw>();
        }
        #[cfg(windows)]
        builder
            .register::<clack_extensions::gui::HostGui>()
            .register::<clack_extensions::timer::HostTimer>()
            .register::<clack_extensions::params::HostParams>()
            .register::<clack_extensions::state::HostState>();
        // Parameters/state are exposed on the Windows main/DSP lane: void
        // requests retain one ordered source until canonical Plan/Ack acceptance.
        // Other platforms need their actual idle audio/main service integration.
    }
}

/// The actual SDK shared owner contains its bounded log ingress inline.
/// It is pinned by the existing HostWrapper allocation, not a second log Arc.
pub(super) struct ClapNativeHostShared {
    pub(super) control: ClapHostShared,
    pub(super) services: ClapNativeServicesOwner,
    pub(super) logs: super::native_audio_log::NativeAudioLogIngress,
}

impl ClapHostShared {
    pub(super) fn with_native_logs(self) -> Result<ClapNativeHostShared, AudioProcessorHostError> {
        Ok(ClapNativeHostShared {
            control: self,
            services: ClapNativeServicesOwner::Inline(ClapNativeServicesShared::new()),
            logs: super::native_audio_log::NativeAudioLogIngress::new()?,
        })
    }
}
impl ClapHostShared {
    /// Explicit query-only ownership: caller and SDK refer to the one Arc
    /// payload until native destroy returns. Other operations use inline services.
    pub(super) fn with_retained_native_services(
        self,
        services: Arc<ClapNativeServicesShared>,
    ) -> Result<ClapNativeHostShared, AudioProcessorHostError> {
        Ok(ClapNativeHostShared {
            control: self,
            services: ClapNativeServicesOwner::Retained(services),
            logs: super::native_audio_log::NativeAudioLogIngress::new()?,
        })
    }
}

impl<'a> SharedHandler<'a> for ClapNativeHostShared {
    fn initializing(&self, handle: clack_host::prelude::InitializingPluginHandle<'a>) {
        self.services.timer_available.store(
            handle.get_extension::<clack_extensions::timer::PluginTimer>().is_some(),
            Ordering::Release,
        );
    }
    fn request_restart(&self) {
        self.control.request_restart();
    }
    fn request_process(&self) {
        self.control.request_process();
    }
    fn request_callback(&self) {
        self.control.request_callback();
    }
}
impl HostThreadCheckImpl for ClapNativeHostShared {
    fn is_main_thread(&self) -> bool {
        self.control.is_main_thread()
    }
    fn is_audio_thread(&self) -> bool {
        self.control.is_audio_thread()
    }
}
impl HostLogImpl for ClapNativeHostShared {
    fn log(&self, message: HostLogMessage<'_>) {
        self.logs.observe(message);
    }
}

/// Child-only CLAP factory. Plugin binaries are never loaded by the editor process.
pub struct ClapAudioProcessorWorkerFactory;

/// Native CLAP main-input shape, independent of the graph's output/working PCM.
/// An absent main input may still have ordered auxiliary native inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClapMainAudioInput {
    Absent,
    Present,
}

impl ClapMainAudioInput {
    pub(super) fn port_offset(self) -> usize {
        match self {
            Self::Absent => 0,
            Self::Present => 1,
        }
    }

    pub(super) fn validate(
        self,
        auxiliary: &AudioProcessorAuxiliaryInputContract,
    ) -> Result<(), AudioProcessorHostError> {
        self.validate_auxiliary_count(auxiliary.buses.len())
    }

    pub(super) fn validate_auxiliary_count(
        self,
        count: usize,
    ) -> Result<(), AudioProcessorHostError> {
        if count >= super::clap_discovery::MAX_INPUT_PORTS as usize {
            return Err(invalid(
                "CLAP auxiliary inputs exceed the existing 31-port bound",
            ));
        }
        Ok(())
    }
}

/// Installed CLAP definition admitted by the application after discovery.
#[derive(Debug, Clone)]
pub struct ClapPluginRegistration {
    /// CLAP descriptor ID, independent of the installed binary path.
    pub plugin_id: String,
    /// Absolute path to the selected installed library.
    pub library_path: PathBuf,
    /// SHA-256 of the exact installed binary admitted by discovery and probe.
    pub binary_sha256: [u8; 32],
    /// Exact scheduling contract supplied by the caller and rechecked in the child.
    pub execution_contract: AudioProcessorExecutionContract,
    /// Definition-local parameter IDs and plain-value contracts from the probe.
    pub parameters: Vec<super::ClapParameterDescriptor>,
    /// Actual native main-input presence captured from this restored instance.
    pub main_audio_input: ClapMainAudioInput,
    /// Stable native identity of output index zero.
    pub main_audio_output_id: u32,
    /// Ordered additional native outputs, including currently unrouted ports.
    pub auxiliary_outputs: AudioProcessorAuxiliaryOutputContract,
    /// Ordered auxiliary inputs captured after restoring the occurrence state.
    pub auxiliary_inputs: AudioProcessorAuxiliaryInputContract,
}

/// Parent-side registry that packages an installed CLAP reference for the child.
///
/// Registering does not load native code. The child revalidates the descriptor,
/// ports, latency, and tail against this registry before it executes any block.
pub struct ClapAudioProcessorSpecResolver {
    helper_executable: PathBuf,
    plugins: BTreeMap<String, ClapPluginRegistration>,
}

/// CLAP resolver that discovers selected installed libraries and probes each
/// occurrence's exact render contract before worker admission.
#[derive(Clone)]
pub struct DiscoveredClapAudioProcessorSpecResolver {
    helper_executable: PathBuf,
    plugins: BTreeMap<String, (ClapPluginDescriptor, PathBuf, [u8; 32])>,
}

/// Mutable installed-plugin catalog shared by Preview and Export resolvers.
///
/// Native discovery runs outside the catalog lock. Each installation publishes
/// every descriptor from one library together or leaves the previous snapshot
/// intact. Existing prepared sessions retain their binary fingerprint.
pub struct InstalledClapAudioProcessorSpecResolver {
    catalog: RwLock<DiscoveredClapAudioProcessorSpecResolver>,
}

impl InstalledClapAudioProcessorSpecResolver {
    /// Start one empty catalog bound to the packaged helper executable.
    pub fn new(helper_executable: PathBuf) -> Result<Self, AudioProcessorHostError> {
        Ok(Self {
            catalog: RwLock::new(DiscoveredClapAudioProcessorSpecResolver::discover(
                helper_executable,
                [],
            )?),
        })
    }

    /// Discover and atomically publish one explicitly selected native library.
    ///
    /// Selecting the same path again replaces all definitions from that path,
    /// so a changed binary has to pass fresh discovery before being used.
    pub fn install_library(
        &self,
        library_path: PathBuf,
    ) -> Result<Vec<ClapPluginDescriptor>, AudioProcessorHostError> {
        let selected_path = std::fs::canonicalize(&library_path)
            .map_err(|error| unavailable(format!("CLAP library is unavailable: {error}")))?;
        let selected_fingerprint = fingerprint_clap_binary(&selected_path)?;
        let helper_executable = self
            .catalog
            .read()
            .map_err(|_| unavailable("CLAP catalog lock is poisoned"))?
            .helper_executable
            .clone();
        let discovered = DiscoveredClapAudioProcessorSpecResolver::discover(
            helper_executable,
            [selected_path.clone()],
        )?;
        if fingerprint_clap_binary(&selected_path)? != selected_fingerprint {
            return Err(unavailable("CLAP binary changed during installation"));
        }
        let mut catalog =
            self.catalog.write().map_err(|_| unavailable("CLAP catalog lock is poisoned"))?;
        for plugin_id in discovered.plugins.keys() {
            if catalog
                .plugins
                .get(plugin_id)
                .is_some_and(|(_, path, _)| path != &selected_path)
            {
                return Err(invalid(format!(
                    "CLAP plugin ID {plugin_id} is already supplied by another library"
                )));
            }
        }
        catalog.plugins.retain(|_, (_, path, _)| path != &selected_path);
        let descriptors = discovered
            .plugins
            .values()
            .map(|(descriptor, _, _)| descriptor.clone())
            .collect();
        catalog.plugins.extend(discovered.plugins);
        Ok(descriptors)
    }

    /// Current installed descriptors for an insertion picker.
    pub fn descriptors(&self) -> Result<Vec<ClapPluginDescriptor>, AudioProcessorHostError> {
        Ok(self
            .catalog
            .read()
            .map_err(|_| unavailable("CLAP catalog lock is poisoned"))?
            .plugins
            .values()
            .map(|(descriptor, _, _)| descriptor.clone())
            .collect())
    }

    /// Capture an author instance from one currently selected definition.
    pub fn create_instance(
        &self,
        plugin_id: &str,
        render_contract: crate::AudioRenderContract,
        state: Option<Vec<u8>>,
    ) -> Result<AudioProcessorInstance, AudioProcessorHostError> {
        let snapshot = self
            .catalog
            .read()
            .map_err(|_| unavailable("CLAP catalog lock is poisoned"))?
            .clone();
        snapshot.create_instance(plugin_id, render_contract, state)
    }
}

impl IsolatedAudioProcessorSpecResolver for InstalledClapAudioProcessorSpecResolver {
    fn native_child_settlement_requirement(
        &self,
    ) -> mondrian_core::NativeChildSettlementRequirement {
        mondrian_core::NativeChildSettlementRequirement::Required
    }

    fn resolve(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<IsolatedAudioProcessorWorkerSpec, AudioProcessorHostError> {
        let snapshot = self
            .catalog
            .read()
            .map_err(|_| unavailable("CLAP catalog lock is poisoned"))?
            .clone();
        snapshot.resolve(request)
    }
}

impl DiscoveredClapAudioProcessorSpecResolver {
    /// Discover explicitly selected installed libraries in isolated children.
    /// Duplicate plugin IDs require the caller to choose one installed binary.
    pub fn discover(
        helper_executable: PathBuf,
        libraries: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, AudioProcessorHostError> {
        if !helper_executable.is_absolute() {
            return Err(invalid("CLAP helper executable must be absolute"));
        }
        let mut plugins = BTreeMap::new();
        for library in libraries {
            let path = std::fs::canonicalize(&library)
                .map_err(|error| unavailable(format!("CLAP library is unavailable: {error}")))?;
            let fingerprint = fingerprint_clap_binary(&path)?;
            let descriptors = scan_clap_library_descriptors(&helper_executable, &path)?;
            if fingerprint_clap_binary(&path)? != fingerprint {
                return Err(unavailable(
                    "CLAP binary changed during descriptor discovery",
                ));
            }
            for descriptor in descriptors {
                if plugins
                    .insert(
                        descriptor.plugin_id.clone(),
                        (descriptor, path.clone(), fingerprint),
                    )
                    .is_some()
                {
                    return Err(invalid(
                        "duplicate installed CLAP plugin ID requires selection",
                    ));
                }
            }
        }
        Ok(Self { helper_executable, plugins })
    }

    /// Stable plugin descriptors available for an insertion picker.
    pub fn descriptors(&self) -> Vec<&ClapPluginDescriptor> {
        self.plugins.values().map(|(descriptor, _, _)| descriptor).collect()
    }

    /// Capture one selected plugin's native state and complete author parameter schema.
    ///
    /// The selected binary is probed again for this render contract and state;
    /// a later Preview or Export preparation repeats the same probe.
    pub fn create_instance(
        &self,
        plugin_id: &str,
        render_contract: crate::AudioRenderContract,
        state: Option<Vec<u8>>,
    ) -> Result<AudioProcessorInstance, AudioProcessorHostError> {
        let (_, library_path, fingerprint) = self
            .plugins
            .get(plugin_id)
            .ok_or_else(|| unavailable(format!("CLAP plugin {plugin_id} is not installed")))?;
        let (registration, captured_state) = probe_clap_plugin_registration_with_state(
            &self.helper_executable,
            library_path,
            plugin_id,
            render_contract,
            state.as_deref(),
            state.is_none(),
        )?;
        if registration.binary_sha256 != *fingerprint {
            return Err(unavailable(
                "CLAP binary changed after descriptor discovery",
            ));
        }
        author_instance_from_probe(registration, state.or(captured_state))
    }
}

fn author_instance_from_probe(
    registration: ClapPluginRegistration,
    opaque_state: Option<Vec<u8>>,
) -> Result<AudioProcessorInstance, AudioProcessorHostError> {
    validate_parameters(&registration.parameters)?;
    let mut parameters = AuthoringMap::new();
    for descriptor in registration.parameters.iter().filter(|parameter| !parameter.read_only) {
        let schema = descriptor.authoring_schema()?;
        let parameter_id = schema.parameter_id.clone();
        let mut parameter = AudioProcessorParameter::from_schema(schema)
            .map_err(|error| invalid(format!("CLAP parameter schema is invalid: {error}")))?;
        parameter = parameter
            .with_ui_metadata(AudioProcessorParameterUiMetadata {
                display_name: Some(descriptor.name.clone()),
                hidden: descriptor.hidden,
            })
            .map_err(|error| invalid(format!("CLAP parameter UI metadata is invalid: {error}")))?;
        if opaque_state.is_some() {
            // Native state owns the initial value; from_schema's default curve
            // is not an explicit author override, including for an empty state.
            parameter.automation = None;
        } else {
            parameter
                .set_automation(
                    ExactAutomationCurve::new(parameter_id.clone(), descriptor.current_value)
                        .map_err(|error| {
                            invalid(format!("CLAP current parameter value is invalid: {error}"))
                        })?,
                )
                .map_err(|error| invalid(format!("CLAP parameter value is invalid: {error}")))?;
        }
        parameters.insert(parameter_id, parameter);
    }
    Ok(AudioProcessorInstance {
        id: AudioProcessorInstanceId::new(),
        definition: AudioProcessorDefinitionRef::Clap {
            plugin_id: registration.plugin_id,
            schema_version: 1,
            binary_sha256: Some(registration.binary_sha256),
        },
        bypassed: false,
        parameters,
        opaque_state: opaque_state.map(Into::into),
    })
}

impl IsolatedAudioProcessorSpecResolver for DiscoveredClapAudioProcessorSpecResolver {
    fn native_child_settlement_requirement(
        &self,
    ) -> mondrian_core::NativeChildSettlementRequirement {
        mondrian_core::NativeChildSettlementRequirement::Required
    }

    fn resolve(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<IsolatedAudioProcessorWorkerSpec, AudioProcessorHostError> {
        let AudioProcessorDefinitionRef::Clap { plugin_id, .. } = request.definition() else {
            return Err(unavailable(
                "only CLAP definitions are handled by this resolver",
            ));
        };
        let (_, library_path, fingerprint) = self
            .plugins
            .get(plugin_id)
            .ok_or_else(|| unavailable(format!("CLAP plugin {plugin_id} is not installed")))?;
        if request.note_input_requested() {
            let offer = super::clap_discovery::probe_clap_note_processing_offer_with_native_child_settlement(
                &self.helper_executable,
                library_path,
                plugin_id,
                request.render_contract(),
                request.opaque_state(),
                request.native_child_settlement_registrar(),
            )?;
            if offer.notes.binary_sha256 != *fingerprint {
                return Err(unavailable(
                    "CLAP binary changed after descriptor discovery",
                ));
            }
            let execution_contract = AudioProcessorExecutionContract::new(
                offer.latency_frames() as usize,
                offer.tail(),
                true,
                offer.processing_mode() == crate::AudioProcessingMode::Realtime,
                offer.processing_mode() == crate::AudioProcessingMode::Offline,
                0,
            )?;
            let registration = ClapPluginRegistration {
                plugin_id: plugin_id.clone(),
                library_path: library_path.clone(),
                binary_sha256: offer.notes.binary_sha256,
                execution_contract,
                parameters: offer.parameters().to_vec(),
                main_audio_input: offer.main_audio_input(),
                main_audio_output_id: offer.main_audio_output_id(),
                auxiliary_outputs: offer.auxiliary_outputs(),
                auxiliary_inputs: AudioProcessorAuxiliaryInputContract {
                    buses: offer
                        .auxiliary_inputs()
                        .map(|(key, channel_layout)| {
                            crate::AudioProcessorAuxiliaryInputBusContract {
                                bus_key: key.to_owned(),
                                channel_layout,
                            }
                        })
                        .collect(),
                },
            };
            return resolve_clap_registration(
                &self.helper_executable,
                &registration,
                request,
                offer.notes.contract,
            )?
            .with_native_probe_input_names(offer.input_names);
        }
        let probe = probe_clap_plugin_registration_with_names(
            &self.helper_executable,
            library_path,
            plugin_id,
            request.render_contract(),
            request.opaque_state(),
            false,
            request.native_child_settlement_registrar(),
        )?;
        if probe.registration.binary_sha256 != *fingerprint {
            return Err(unavailable(
                "CLAP binary changed after descriptor discovery",
            ));
        }
        ClapAudioProcessorSpecResolver::new(self.helper_executable.clone(), [probe.registration])?
            .resolve(request)?
            .with_native_probe_input_names(probe.input_names)
    }
}

impl ClapAudioProcessorSpecResolver {
    /// Build an exact registry for one application executable and installed set.
    pub fn new(
        helper_executable: PathBuf,
        plugins: impl IntoIterator<Item = ClapPluginRegistration>,
    ) -> Result<Self, AudioProcessorHostError> {
        if !helper_executable.is_absolute() {
            return Err(invalid("CLAP helper executable must be absolute"));
        }
        let mut by_id = BTreeMap::new();
        for plugin in plugins {
            if plugin.plugin_id.is_empty()
                || plugin.plugin_id.contains('\0')
                || !plugin.library_path.is_absolute()
                || plugin.binary_sha256 == [0; 32]
                || !plugin.execution_contract.requires_state_entry()
                || validate_parameters(&plugin.parameters).is_err()
                || plugin.main_audio_input.validate(&plugin.auxiliary_inputs).is_err()
                || super::clap_discovery::validate_audio_outputs(
                    plugin.main_audio_output_id,
                    &plugin.auxiliary_outputs,
                    plugin
                        .auxiliary_outputs
                        .buses
                        .first()
                        .map_or(mondrian_core::AudioChannelLayout::Stereo, |bus| {
                            bus.channel_layout
                        }),
                )
                .is_err()
                || by_id.insert(plugin.plugin_id.clone(), plugin).is_some()
            {
                return Err(invalid("CLAP registrations need unique IDs, absolute libraries, and explicit state entry"));
            }
        }
        Ok(Self { helper_executable, plugins: by_id })
    }
}

impl IsolatedAudioProcessorSpecResolver for ClapAudioProcessorSpecResolver {
    fn native_child_settlement_requirement(
        &self,
    ) -> mondrian_core::NativeChildSettlementRequirement {
        // This registry only interprets already-admitted immutable metadata.
        mondrian_core::NativeChildSettlementRequirement::NotApplicable
    }

    fn resolve(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<IsolatedAudioProcessorWorkerSpec, AudioProcessorHostError> {
        let AudioProcessorDefinitionRef::Clap { plugin_id, schema_version, .. } =
            request.definition()
        else {
            return Err(unavailable(
                "only CLAP definitions are handled by this registry",
            ));
        };
        if *schema_version != 1 {
            return Err(unavailable("CLAP definition schema version is unsupported"));
        }
        let plugin = self
            .plugins
            .get(plugin_id)
            .ok_or_else(|| unavailable(format!("CLAP plugin {plugin_id} is not registered")))?;
        resolve_clap_registration(
            &self.helper_executable,
            plugin,
            request,
            crate::AudioProcessorNoteInputContract::default(),
        )
    }
}

/// Adapter-local consumer of coherent processing metadata. The registration
/// remains the existing payload interpreter; no second author/state registry.
fn resolve_clap_registration(
    helper_executable: &std::path::Path,
    plugin: &ClapPluginRegistration,
    request: AudioProcessorPrepareRequest<'_>,
    notes: crate::AudioProcessorNoteInputContract,
) -> Result<IsolatedAudioProcessorWorkerSpec, AudioProcessorHostError> {
    let AudioProcessorDefinitionRef::Clap { plugin_id, schema_version, binary_sha256 } =
        request.definition()
    else {
        return Err(unavailable(
            "only CLAP definitions are handled by this registry",
        ));
    };
    if *schema_version != 1 {
        return Err(unavailable("CLAP definition schema version is unsupported"));
    }
    super::clap_note_input_contract::validate_clap_contract(&notes)?;
    plugin.main_audio_input.validate(&plugin.auxiliary_inputs)?;
    super::clap_discovery::validate_audio_outputs(
        plugin.main_audio_output_id,
        &plugin.auxiliary_outputs,
        request.render_contract().channel_layout,
    )?;
    if request.note_input_requested() && notes.ports.is_empty() {
        return Err(invalid(
            "CLAP registry has no same-instance Note processing offer",
        ));
    }
    match binary_sha256 {
        Some(expected) if *expected == plugin.binary_sha256 => {}
        Some(_) => {
            return Err(unavailable(format!(
                "CLAP plugin {plugin_id} binary revision differs from the authored instance"
            )))
        }
        None => {
            return Err(unavailable(format!(
                "CLAP plugin {plugin_id} has no authored binary revision; rebind it explicitly"
            )))
        }
    }
    let mut parameter_ids = Vec::with_capacity(request.parameters().len());
    let mut clap_parameter_ids = Vec::with_capacity(request.parameters().len());
    for (parameter_id, parameter) in request.parameters() {
        let descriptor = plugin
            .parameters
            .iter()
            .find(|descriptor| descriptor.parameter_id().as_ref().ok() == Some(parameter_id))
            .ok_or_else(|| invalid("authored CLAP parameter is absent from the plugin"))?;
        if descriptor.read_only || parameter.schema != descriptor.authoring_schema()? {
            return Err(invalid(
                "authored CLAP parameter contract differs from the plugin",
            ));
        }
        parameter_ids.push(parameter_id.clone());
        clap_parameter_ids.push(descriptor.id);
    }
    let payload = serde_json::to_vec(&ClapPayload {
        library_path: plugin.library_path.clone(),
        plugin_id: plugin.plugin_id.clone(),
        binary_sha256: plugin.binary_sha256,
        main_audio_input: plugin.main_audio_input,
        main_audio_output_id: plugin.main_audio_output_id,
        auxiliary_outputs: super::protocol::outputs_to_wire(&plugin.auxiliary_outputs),
        state: request.opaque_state().map(ToOwned::to_owned),
        parameters: plugin.parameters.clone(),
        parameter_ids: clap_parameter_ids,
    })
    .map_err(|error| invalid(format!("CLAP worker payload encoding failed: {error}")))?;
    let note_owner_reservation = if notes.ports.is_empty() {
        output_source_reservation_bytes(plugin.parameters.len())?
    } else {
        note_source_reservation_bytes(plugin.parameters.len(), &notes)?
    };
    IsolatedAudioProcessorWorkerSpec::new(
        helper_executable.to_path_buf(),
        payload,
        plugin.execution_contract.with_additional_session_scratch_bytes(
            note_owner_reservation
                .checked_add(ClapInputEvents::reservation_bytes(
                    parameter_input_capacity(request.render_contract(), parameter_ids.len())?,
                    notes.limits.input_events,
                )?)
                .and_then(|bytes| {
                    bytes.checked_add(
                        clap_audio_storage_reservation(
                            request.render_contract(),
                            plugin.main_audio_input,
                            &plugin.auxiliary_inputs,
                            &plugin.auxiliary_outputs,
                        )
                        .ok()?,
                    )
                })
                .ok_or_else(|| invalid("CLAP input/output owner reservation overflowed"))?,
        )?,
        plugin.auxiliary_inputs.clone(),
        parameter_ids,
        request.render_contract(),
    )?
    .with_musical_time_reference(request.musical_time_reference())?
    .with_auxiliary_output_contract(plugin.auxiliary_outputs.clone())?
    .with_native_worker_lane_resources(clap_native_worker_lane_profile()?)
    .with_note_input_contract(notes)
}

// The concrete preparation codec remains the only interpreter for this format.
// This non-RT helper neither loads a plugin nor grants author/install authority.
pub(super) fn payload_with_configuration_seed(
    payload: &[u8],
    configuration: &[u8],
) -> Result<Vec<u8>, AudioProcessorHostError> {
    if configuration.len() > super::MAX_CONTROL_FRAME_BYTES / 2 {
        return Err(invalid(
            "native configuration seed exceeds the existing state limit",
        ));
    }
    let mut decoded: ClapPayload = serde_json::from_slice(payload)
        .map_err(|error| invalid(format!("invalid concrete payload for native seed: {error}")))?;
    // Retire the decoded old seed outside publication/SDK guards before
    // allocating its replacement; preserve all other concrete metadata.
    decoded.state = None;
    let mut seed = Vec::new();
    seed.try_reserve_exact(configuration.len()).map_err(|error| {
        invalid(format!(
            "native configuration seed allocation refused: {error}"
        ))
    })?;
    seed.extend_from_slice(configuration);
    decoded.state = Some(seed);
    super::encode_native_configuration_payload(&decoded)
}
// Only the immutable payload of an already admitted actual Factory enters this
// view. NativeProbed evidence is crate-private; public manual Specs are first
// typed-decoded by the validation child. This is NOT a decoder for new external
// metadata. The old native-author codec above remains typed and unchanged.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClapRepreparePayload<'a> {
    #[serde(borrow)]
    main_audio_input: &'a serde_json::value::RawValue,
    #[serde(borrow)]
    main_audio_output_id: &'a serde_json::value::RawValue,
    #[serde(borrow)]
    auxiliary_outputs: &'a serde_json::value::RawValue,
    #[serde(borrow)]
    library_path: &'a serde_json::value::RawValue,
    #[serde(borrow)]
    plugin_id: &'a serde_json::value::RawValue,
    #[serde(borrow)]
    binary_sha256: &'a serde_json::value::RawValue,
    #[serde(default, borrow)]
    state: Option<&'a serde_json::value::RawValue>,
    #[serde(borrow)]
    parameters: &'a serde_json::value::RawValue,
    #[serde(borrow)]
    parameter_ids: &'a serde_json::value::RawValue,
}

// A requested-layout envelope for the pinned serde_json raw scanner's scratch,
// escaped field keys and cold single-layer Display errors, not heap telemetry.
// 1024 covers these nine static field names and fixed error prefixes. Raw nested
// values are skipped iteratively (including numbers), not decoded to containers.
pub(super) fn reprepare_codec_bound(payload_bytes: usize) -> Option<usize> {
    if payload_bytes > super::MAX_CONTROL_FRAME_BYTES / 2 {
        return None;
    }
    payload_bytes.checked_add(1024)?.checked_mul(16)
}

pub(super) fn reprepare_payload_output_bound(
    payload_bytes: usize,
    configuration_bytes: usize,
) -> Option<usize> {
    reprepare_codec_bound(payload_bytes)?;
    if configuration_bytes > super::MAX_CONTROL_FRAME_BYTES / 2 {
        return None;
    }
    configuration_bytes
        .checked_mul(4)?
        .checked_add(payload_bytes)?
        .checked_add(1024)
}

pub(super) fn payload_with_reprepare_configuration(
    payload: &[u8],
    configuration: &[u8],
    report_peak: &mut dyn FnMut(usize),
) -> Result<Vec<u8>, AudioProcessorHostError> {
    let codec = reprepare_codec_bound(payload.len())
        .ok_or_else(|| invalid("retained CLAP metadata exceeds the raw codec bound"))?;
    reprepare_payload_output_bound(payload.len(), configuration.len())
        .ok_or_else(|| invalid("retained CLAP configuration exceeds the raw codec bound"))?;
    report_peak(codec);
    if payload.iter().copied().find(|b| !matches!(b, b' ' | b'\n' | b'\r' | b'\t')) != Some(b'{') {
        return Err(invalid("retained CLAP metadata must be a JSON object"));
    }
    let view: ClapRepreparePayload<'_> = serde_json::from_slice(payload)
        .map_err(|error| invalid(format!("invalid retained CLAP metadata: {error}")))?;
    // Missing/null old state is allowed by the original schema. Its raw bytes
    // are borrowed only; no old state Vec is decoded just to discard it.
    let _old_state = view.state;
    struct Replacement<'a, 'b> {
        metadata: &'a ClapRepreparePayload<'b>,
        state: &'a [u8],
    }
    impl Serialize for Replacement<'_, '_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeStruct;
            let v = self.metadata;
            let mut object = serializer.serialize_struct("ClapPayload", 9)?;
            object.serialize_field("main_audio_input", v.main_audio_input)?;
            object.serialize_field("main_audio_output_id", v.main_audio_output_id)?;
            object.serialize_field("auxiliary_outputs", v.auxiliary_outputs)?;
            object.serialize_field("library_path", v.library_path)?;
            object.serialize_field("plugin_id", v.plugin_id)?;
            object.serialize_field("binary_sha256", v.binary_sha256)?;
            object.serialize_field("state", &self.state)?;
            object.serialize_field("parameters", v.parameters)?;
            object.serialize_field("parameter_ids", v.parameter_ids)?;
            object.end()
        }
    }
    super::encode_native_configuration_payload_observed(
        &Replacement { metadata: &view, state: configuration },
        &mut |capacity| report_peak(codec.saturating_add(capacity)),
    )
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ClapPayload {
    main_audio_input: ClapMainAudioInput,
    main_audio_output_id: u32,
    auxiliary_outputs: Vec<super::protocol::AuxiliaryBusWire>,
    library_path: PathBuf,
    plugin_id: String,
    binary_sha256: [u8; 32],
    #[serde(default)]
    state: Option<Vec<u8>>,
    parameters: Vec<super::ClapParameterDescriptor>,
    parameter_ids: Vec<u32>,
}

// Context only: this does not attribute the invalid descriptor to its writer.
// Successful validation and unrelated/count errors do not allocate here.
fn parameter_refusal_at_site(
    error: AudioProcessorHostError,
    site: &'static str,
) -> AudioProcessorHostError {
    match error {
        AudioProcessorHostError::InvalidContract(mut detail)
            if detail.starts_with("CLAP parameter metadata is invalid or ambiguous: ") =>
        {
            detail.push_str(" validation_site=");
            detail.push_str(site);
            AudioProcessorHostError::InvalidContract(detail)
        }
        other => other,
    }
}

impl IsolatedAudioProcessorWorkerFactory for ClapAudioProcessorWorkerFactory {
    fn native_worker_lane_resources(
        &self,
    ) -> Result<Option<crate::NativeWorkerLaneResourceProfile>, AudioProcessorHostError> {
        clap_native_worker_lane_profile()
    }
    fn prepare(
        &self,
        request: IsolatedAudioProcessorWorkerPrepareRequest,
    ) -> Result<Box<dyn IsolatedAudioProcessorWorker>, AudioProcessorHostError> {
        super::clap_note_input_contract::validate_clap_contract(request.note_input_contract())?;
        if request.native_worker_lane_resources() != clap_native_worker_lane_profile()? {
            return Err(invalid(
                "CLAP prepared native lane profile differs from the actual adapter",
            ));
        }
        let payload: ClapPayload = serde_json::from_slice(request.payload())
            .map_err(|error| invalid(format!("invalid CLAP worker payload: {error}")))?;
        if !payload.library_path.is_absolute() || payload.plugin_id.is_empty() {
            return Err(invalid(
                "CLAP library path must be absolute and plugin ID nonempty",
            ));
        }
        payload.main_audio_input.validate(request.auxiliary_inputs())?;
        let outputs = super::protocol::outputs_from_wire(payload.auxiliary_outputs.clone());
        super::clap_discovery::validate_audio_outputs(
            payload.main_audio_output_id,
            &outputs,
            request.render_contract().channel_layout,
        )?;
        if &outputs != request.auxiliary_outputs() {
            return Err(invalid(
                "CLAP payload outputs differ from the admitted contract",
            ));
        }
        validate_parameters(&payload.parameters)
            .map_err(|error| parameter_refusal_at_site(error, "decoded_payload"))?;
        if payload.parameter_ids.len() != request.parameter_ids().len() {
            return Err(invalid(
                "CLAP parameter lane count differs from the worker request",
            ));
        }
        for (parameter_id, clap_id) in request.parameter_ids().iter().zip(&payload.parameter_ids) {
            let descriptor = payload
                .parameters
                .iter()
                .find(|parameter| parameter.id == *clap_id)
                .ok_or_else(|| invalid("CLAP parameter lane has no descriptor"))?;
            if descriptor.read_only || descriptor.parameter_id()? != *parameter_id {
                return Err(invalid("CLAP parameter lane identity is invalid"));
            }
        }
        if fingerprint_clap_binary(&payload.library_path)? != payload.binary_sha256 {
            return Err(unavailable("CLAP binary changed since processor admission"));
        }
        let plugin_id = CString::new(payload.plugin_id)
            .map_err(|_| invalid("CLAP plugin ID contains a NUL byte"))?;
        #[cfg(windows)]
        let native_main = std::rc::Rc::new(
            vst3_host::window::WindowsEditorMainThread::initialize().map_err(|error| {
                unavailable(format!("CLAP native main initialization failed: {error}"))
            })?,
        );
        let entry = unsafe { PluginEntry::load(payload.library_path.as_os_str()) }
            .map_err(|error| unavailable(format!("CLAP library load failed: {error}")))?;
        prepare_loaded_on_main(
            #[cfg(windows)]
            native_main,
            request,
            entry,
            plugin_id.as_c_str(),
            payload.state.as_deref(),
            payload.main_audio_input,
            payload.main_audio_output_id,
            &payload.parameters,
            &payload.parameter_ids,
        )
        .map(|worker| worker as Box<dyn IsolatedAudioProcessorWorker>)
    }
}

// Additional known owner subset, not a claim about complete SDK/plugin/OS heaps.
fn output_source_reservation_bytes(
    parameter_count: usize,
) -> Result<usize, AudioProcessorHostError> {
    let signatures =
        std::alloc::Layout::array::<parameter_observations::ParameterSignature>(parameter_count)
            .map_err(|_| invalid("CLAP parameter signature reservation overflowed"))?
            .size();
    std::mem::size_of::<ClapWorker>()
        .checked_add(ClapOutputEvents::reservation_bytes(parameter_count)?)
        .and_then(|bytes| bytes.checked_add(signatures))
        .and_then(|bytes| {
            bytes.checked_add(std::mem::size_of::<
                super::clap_parameter_source::ClapParameterSource,
            >())
        })
        .and_then(|bytes| {
            bytes.checked_add(std::mem::size_of::<
                super::clap_parameter_source::ClapParameterControlSource<'static>,
            >())
        })
        .ok_or_else(|| invalid("CLAP output source reservation overflowed"))
}

fn note_source_reservation_bytes(
    parameter_count: usize,
    notes: &crate::AudioProcessorNoteInputContract,
) -> Result<usize, AudioProcessorHostError> {
    let original = output_source_reservation_bytes(parameter_count)?;
    let extra_ends =
        ClapOutputEvents::reservation_bytes_with_notes(parameter_count, notes.limits.output_ends)?
            .checked_sub(ClapOutputEvents::reservation_bytes(parameter_count)?)
            .ok_or_else(|| invalid("CLAP Note output reservation underflowed"))?;
    original
        .checked_add(extra_ends)
        .and_then(|bytes| {
            bytes.checked_add(
                super::clap_note_input_contract::RestoredNoteInputs::reservation_bytes(
                    notes.ports.len(),
                )
                .ok()?,
            )
        })
        .ok_or_else(|| invalid("CLAP Note owner reservation overflowed"))
}

// Known host-owned audio buffers only. SDK/foreign heaps remain outside this request.
fn clap_input_audio_shape(
    main_layout: mondrian_core::AudioChannelLayout,
    main_input: ClapMainAudioInput,
    inputs: &AudioProcessorAuxiliaryInputContract,
) -> Result<(usize, usize), AudioProcessorHostError> {
    let ports = main_input
        .port_offset()
        .checked_add(inputs.buses.len())
        .ok_or_else(|| invalid("CLAP input count overflowed"))?;
    let channels = inputs
        .buses
        .iter()
        .try_fold(
            main_input.port_offset() * main_layout.channel_count(),
            |channels, bus| channels.checked_add(bus.channel_layout.channel_count()),
        )
        .ok_or_else(|| invalid("CLAP input channel count overflowed"))?;
    Ok((ports, channels))
}

fn clap_audio_storage_reservation(
    render: crate::AudioRenderContract,
    main_input: ClapMainAudioInput,
    inputs: &AudioProcessorAuxiliaryInputContract,
    outputs: &AudioProcessorAuxiliaryOutputContract,
) -> Result<usize, AudioProcessorHostError> {
    let (input_ports, input_channels) =
        clap_input_audio_shape(render.channel_layout, main_input, inputs)?;
    let output_ports = 1usize
        .checked_add(outputs.buses.len())
        .ok_or_else(|| invalid("CLAP output count overflowed"))?;
    let output_channels = output_ports
        .checked_mul(render.channel_count())
        .ok_or_else(|| invalid("CLAP channel count overflowed"))?;
    let input = super::clap_audio_input_buffers::plane_heap_reservation(
        input_ports,
        input_channels,
        render.max_block_frames,
    );
    let output = super::clap_audio_input_buffers::plane_heap_reservation(
        output_ports,
        output_channels,
        render.max_block_frames,
    );
    let metadata = outputs
        .buses
        .len()
        .checked_mul(size_of::<crate::AudioProcessorAuxiliaryOutputBusContract>())
        .and_then(|n| outputs.buses.iter().try_fold(n, |n, bus| n.checked_add(bus.bus_key.len())));
    input
        .and_then(|n| n.checked_add(output?))
        .and_then(|n| n.checked_add(metadata?))
        .ok_or_else(|| invalid("CLAP audio storage reservation overflowed"))
}

fn clap_actual_audio_storage(
    input: &[Vec<Vec<f32>>],
    output: &[Vec<Vec<f32>>],
    outer_port_capacity: usize,
    input_ports: &AudioPorts,
    output_ports: &AudioPorts,
    outputs: &AudioProcessorAuxiliaryOutputContract,
) -> Result<usize, AudioProcessorHostError> {
    let mut bytes = outer_port_capacity
        .checked_mul(size_of::<Vec<Vec<f32>>>())
        .and_then(|n| {
            n.checked_add(super::clap_audio_input_buffers::nested_storage_bytes(
                input,
                input_ports,
            )?)
        })
        .and_then(|n| {
            n.checked_add(super::clap_audio_input_buffers::nested_storage_bytes(
                output,
                output_ports,
            )?)
        })
        .ok_or_else(|| invalid("CLAP audio owner overflowed"))?;
    bytes = bytes
        .checked_add(
            outputs
                .buses
                .capacity()
                .checked_mul(size_of::<crate::AudioProcessorAuxiliaryOutputBusContract>())
                .ok_or_else(|| invalid("CLAP output metadata overflowed"))?,
        )
        .ok_or_else(|| invalid("CLAP output metadata overflowed"))?;
    for bus in &outputs.buses {
        bytes = bytes
            .checked_add(bus.bus_key.capacity())
            .ok_or_else(|| invalid("CLAP output key owner overflowed"))?;
    }
    Ok(bytes)
}

fn parameter_input_capacity(
    render: crate::AudioRenderContract,
    parameter_lanes: usize,
) -> Result<usize, AudioProcessorHostError> {
    render
        .max_block_frames
        .checked_mul(parameter_lanes)
        .filter(|capacity| *capacity <= 1_048_576)
        .ok_or_else(|| invalid("CLAP parameter event capacity exceeds the supported bound"))
}

#[cfg(test)]
fn prepare_loaded(
    request: IsolatedAudioProcessorWorkerPrepareRequest,
    entry: PluginEntry,
    plugin_id: &CStr,
    state: Option<&[u8]>,
    expected_parameters: &[super::ClapParameterDescriptor],
    parameter_ids: &[u32],
) -> Result<Box<dyn IsolatedAudioProcessorWorker>, AudioProcessorHostError> {
    prepare_loaded_with_audio_input(
        request,
        entry,
        plugin_id,
        state,
        ClapMainAudioInput::Present,
        expected_parameters,
        parameter_ids,
    )
}

#[cfg(test)]
fn prepare_loaded_with_audio_input(
    request: IsolatedAudioProcessorWorkerPrepareRequest,
    entry: PluginEntry,
    plugin_id: &CStr,
    state: Option<&[u8]>,
    main_audio_input: ClapMainAudioInput,
    expected_parameters: &[super::ClapParameterDescriptor],
    parameter_ids: &[u32],
) -> Result<Box<dyn IsolatedAudioProcessorWorker>, AudioProcessorHostError> {
    #[cfg(windows)]
    let native_main = std::rc::Rc::new(
        vst3_host::window::WindowsEditorMainThread::initialize().map_err(|error| {
            unavailable(format!(
                "CLAP owned native main initialization failed: {error}"
            ))
        })?,
    );
    prepare_loaded_on_main(
        #[cfg(windows)]
        native_main,
        request,
        entry,
        plugin_id,
        state,
        main_audio_input,
        0,
        expected_parameters,
        parameter_ids,
    )
    .map(|worker| worker as Box<dyn IsolatedAudioProcessorWorker>)
}

fn prepare_loaded_on_main(
    #[cfg(windows)] native_main: std::rc::Rc<vst3_host::window::WindowsEditorMainThread>,
    request: IsolatedAudioProcessorWorkerPrepareRequest,
    entry: PluginEntry,
    plugin_id: &CStr,
    state: Option<&[u8]>,
    main_audio_input: ClapMainAudioInput,
    main_audio_output_id: u32,
    expected_parameters: &[super::ClapParameterDescriptor],
    parameter_ids: &[u32],
) -> Result<Box<ClapWorker>, AudioProcessorHostError> {
    let event_capacity = parameter_input_capacity(request.render_contract(), parameter_ids.len())?;
    super::clap_note_input_contract::validate_clap_contract(request.note_input_contract())?;
    let notes_enabled = !request.note_input_contract().ports.is_empty();
    let note_limits = request.note_input_contract().limits;
    let input_events =
        ClapInputEvents::try_with_note_capacity(event_capacity, note_limits.input_events)?;
    if note_source_reservation_bytes(expected_parameters.len(), request.note_input_contract())?
        .checked_add(input_events.allocated_bytes()?)
        .ok_or_else(|| invalid("CLAP input/output owner reservation overflowed"))?
        > request.render_contract().processor_session_scratch_budget_bytes
    {
        return Err(invalid(
            "CLAP output owner exceeds the current Session scratch ceiling",
        ));
    }
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| unavailable("CLAP library has no plugin factory"))?;
    super::clap_note_input_metadata::validate_factory_plugin_id(&factory, plugin_id)?;
    let info = HostInfo::new(
        "Mondrian",
        "Mondrian",
        "https://github.com/shaloong/mondrian",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|error| invalid(format!("invalid CLAP host identity: {error}")))?;
    let shared = if notes_enabled {
        ClapHostShared::new_with_note_input()
    } else {
        ClapHostShared::new()
    };
    let output_events = ClapOutputEvents::new_with_note_capacity(
        expected_parameters,
        Arc::clone(&shared.parameter_source),
        note_limits.output_ends,
    )
    .map_err(|error| parameter_refusal_at_site(error, "expected_output_contract"))?;
    let native_shared = shared.clone().with_native_logs()?;
    let mut instance = PluginInstance::<ClapHost>::new(
        |_| native_shared,
        |shared| ClapHostMainThread::new(shared),
        &entry,
        plugin_id,
        &info,
    )
    .map_err(|error| unavailable(format!("CLAP instance creation failed: {error}")))?;
    let log_receiver = instance
        .access_shared_handler(|host| host.logs.claim_receiver())
        .ok_or_else(|| invalid("CLAP native log receiver already claimed"))?;
    shared.service_callbacks(&mut instance)?;
    if let Some(state) = state {
        let handle = instance.plugin_handle();
        let extension = handle
            .get_extension::<PluginState>()
            .ok_or_else(|| unavailable("CLAP plugin does not support state restoration"))?;
        let mut reader = state;
        extension
            .load(&handle, &mut reader)
            .map_err(|error| unavailable(format!("CLAP state restore failed: {error}")))?;
    }

    shared.service_callbacks(&mut instance)?;
    configure_render_mode(
        &instance.plugin_handle(),
        request.render_contract().processing_mode,
    )?;
    shared.service_callbacks(&mut instance)?;
    let channel_count = request.render_contract().channel_count();
    let handle = instance.plugin_handle();
    {
        // Own the one existing query snapshot only through the original check;
        // its Vec is dropped before signature/port/activation work as before.
        let mut capture = ParameterQueryCapture::from_environment();
        let parameters = list_parameters_observed(&handle, capture.as_mut())
            .map_err(|error| parameter_refusal_at_site(error, "native_after_state_render_mode"))?;
        if !same_clap_parameter_structure(&parameters, expected_parameters) {
            let detail = match capture.as_ref() {
                Some(capture) => first_clap_parameter_structure_mismatch_observed(
                    &parameters,
                    expected_parameters,
                    Some(capture),
                ),
                None => first_clap_parameter_structure_mismatch(&parameters, expected_parameters),
            }
            .unwrap_or_else(|| "CLAP parameter metadata changed since contract probe".to_owned());
            return Err(invalid(detail));
        }
    }
    let parameter_signatures = parameter_observations::capture_signatures(&handle)?;
    let signature_bytes = std::alloc::Layout::array::<parameter_observations::ParameterSignature>(
        parameter_signatures.capacity(),
    )
    .map_err(|_| invalid("CLAP actual parameter signature allocation overflow"))?
    .size();
    if output_events
        .allocated_bytes()?
        .checked_add(input_events.allocated_bytes()?)
        .and_then(|bytes| bytes.checked_add(signature_bytes))
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<ClapWorker>()))
        .and_then(|bytes| {
            bytes.checked_add(std::mem::size_of::<
                super::clap_parameter_source::ClapParameterSource,
            >())
        })
        .and_then(|bytes| {
            bytes.checked_add(std::mem::size_of::<
                super::clap_parameter_source::ClapParameterControlSource<'static>,
            >())
        })
        .is_none_or(|bytes| {
            bytes > request.render_contract().processor_session_scratch_budget_bytes
        })
    {
        return Err(invalid(
            "CLAP source/signature owners exceed current Session scratch ceiling",
        ));
    }
    let audio_metadata = inspect_audio_ports(&handle, request.render_contract().channel_layout)?;
    let auxiliary_inputs = audio_metadata.contract;
    main_audio_input.validate(request.auxiliary_inputs())?;
    if audio_metadata.main_audio_input != main_audio_input
        || &auxiliary_inputs != request.auxiliary_inputs()
        || audio_metadata.main_audio_output_id != main_audio_output_id
        || &audio_metadata.outputs != request.auxiliary_outputs()
    {
        return Err(invalid("CLAP audio ports changed since contract probe"));
    }

    let note_inputs = if notes_enabled {
        Some(super::clap_note_input_contract::negotiate_restored(
            &mut instance,
            request.note_input_contract(),
        )?)
    } else {
        None
    };
    if note_inputs.as_ref().map_or(Ok(0), |inputs| inputs.allocated_bytes())?
        > super::clap_note_input_contract::RestoredNoteInputs::reservation_bytes(
            request.note_input_contract().ports.len(),
        )?
    {
        return Err(invalid(
            "CLAP restored Note binding exceeds admitted storage",
        ));
    }

    let render = request.render_contract();
    let configuration = PluginAudioConfiguration {
        sample_rate: f64::from(render.sample_rate),
        min_frames_count: 1,
        max_frames_count: u32::try_from(render.max_block_frames)
            .map_err(|_| invalid("CLAP block size exceeds u32"))?,
    };
    let mut processor: PluginAudioProcessor<ClapHost> = instance
        .activate(|_, _| (), configuration)
        .map_err(|error| unavailable(format!("CLAP activation failed: {error}")))?
        .into();
    instance.access_shared_handler(|host| host.services.active.store(true, Ordering::Release));
    if let Some(inputs) = &note_inputs
        && let Err(error) = super::clap_note_input_contract::service_names(&mut instance, inputs)
    {
        {
            let _audio_role = shared.enter_audio_thread();
            processor.ensure_processing_stopped();
        }
        drop(processor);
        // ALL is explicitly legal from the native deactivate callback.
        instance.access_shared_handler(|host| host.services.active.store(false, Ordering::Release));
        instance.try_deactivate().map_err(|cleanup| {
            unavailable(format!("CLAP Note activation cleanup failed: {cleanup}"))
        })?;
        return Err(error);
    }
    let handle = instance.plugin_handle();
    let latency = handle
        .get_extension::<PluginLatency>()
        .map(|extension| extension.get(&handle) as usize)
        .unwrap_or(0);
    let tail = {
        let _audio_role = shared.enter_audio_thread();
        processor
            .plugin_handle()
            .get_extension::<PluginTail>()
            .map(|extension| extension.get(&processor.plugin_handle()))
            .unwrap_or(TailLength::Finite(0))
    };
    let admitted_tail = match tail {
        TailLength::Finite(0) => crate::AudioProcessorTail::None,
        TailLength::Finite(frames) => crate::AudioProcessorTail::Finite(frames as usize),
        TailLength::Infinite => crate::AudioProcessorTail::Infinite,
    };
    if latency != request.plugin_execution_contract().algorithmic_latency_frames()
        || admitted_tail != request.plugin_execution_contract().tail()
    {
        {
            let _audio_role = shared.enter_audio_thread();
            processor.ensure_processing_stopped();
        }
        drop(processor);
        let _ = instance.try_deactivate();
        return Err(invalid(
            if latency != request.plugin_execution_contract().algorithmic_latency_frames() {
                "CLAP latency differs from the admitted execution contract"
            } else {
                "CLAP tail differs from the admitted execution contract"
            },
        ));
    }
    let frames = render.max_block_frames;
    let parameter_lanes = parameter_ids
        .iter()
        .map(|id| {
            let descriptor = expected_parameters
                .iter()
                .find(|parameter| parameter.id == *id)
                .ok_or_else(|| invalid("CLAP parameter lane is missing"))?;
            let clap_id = ClapId::from_raw(*id)
                .ok_or_else(|| invalid("CLAP parameter lane ID is invalid"))?;
            Ok((
                clap_id,
                descriptor.min_value,
                descriptor.max_value,
                descriptor.stepped,
            ))
        })
        .collect::<Result<Vec<_>, AudioProcessorHostError>>()?;
    let (input_port_count, input_channels) =
        clap_input_audio_shape(render.channel_layout, main_audio_input, &auxiliary_inputs)?;
    // Allocate only actual native ports, each at its own validated width.
    let input = (main_audio_input == ClapMainAudioInput::Present)
        .then_some(render.channel_layout)
        .into_iter()
        .chain(auxiliary_inputs.buses.iter().map(|bus| bus.channel_layout))
        .map(|layout| vec![vec![0.0; frames]; layout.channel_count()])
        .collect::<Vec<_>>();
    let output =
        vec![vec![vec![0.0; frames]; channel_count]; 1 + request.auxiliary_outputs().buses.len()];
    let input_ports = AudioPorts::with_capacity(input_channels, input_port_count);
    let output_ports = AudioPorts::with_capacity(channel_count * output.len(), output.len());
    let auxiliary_outputs = request.auxiliary_outputs().clone();
    let actual_audio_bytes = clap_actual_audio_storage(
        &input,
        &output,
        input
            .capacity()
            .checked_add(output.capacity())
            .ok_or_else(|| invalid("CLAP outer audio owner overflowed"))?,
        &input_ports,
        &output_ports,
        &auxiliary_outputs,
    )?;
    let reserved_audio_bytes = clap_audio_storage_reservation(
        render,
        main_audio_input,
        &auxiliary_inputs,
        request.auxiliary_outputs(),
    )?;
    if actual_audio_bytes > reserved_audio_bytes {
        return Err(invalid(
            "CLAP actual audio owners exceed their admitted capacity",
        ));
    }
    let native_object = next_clap_native_object()?;
    Ok(Box::new(ClapWorker {
        native_lane: request.native_worker_lane_resources(),
        native_object,
        native_binding: None,
        native_author_delivery: None,
        parameter_signatures,
        parameter_observation_revision: None,
        #[cfg(windows)]
        gui: super::clap_gui_owner::ClapGuiOwner::new(std::rc::Rc::clone(&native_main)),
        #[cfg(windows)]
        _native_main: native_main,
        render,
        expected_parameters: expected_parameters.to_vec(),
        configuration_phase:
            super::native_configuration_control::NativeConfigurationPhase::Admitted,
        configuration_seed: None,
        staged_audio_inputs: None,
        processor: Some(processor),
        instance,
        contract: request.plugin_execution_contract(),
        input,
        output,
        main_audio_input,
        main_audio_output_id,
        auxiliary_outputs,
        auxiliary_inputs,
        note_inputs,
        input_names: audio_metadata.names,
        input_ports,
        output_ports,
        parameter_lanes,
        input_events,
        output_events,
        last_end: None,
        initial_signal_available: true,
        log_receiver,
        shared,
    }))
}

struct ClapWorker {
    note_inputs: Option<super::clap_note_input_contract::RestoredNoteInputs>,
    #[cfg(windows)]
    gui: super::clap_gui_owner::ClapGuiOwner,
    native_lane: Option<crate::NativeWorkerLaneResourceProfile>,
    native_object: crate::NativeAudioObjectObservation,
    native_binding: Option<crate::NativeAudioObjectBindReceipt>,
    native_author_delivery: Option<native_author::PendingDelivery>,
    parameter_signatures: Vec<parameter_observations::ParameterSignature>,
    // Observation freshness only, never author acceptance or ClosedEmpty proof.
    parameter_observation_revision: Option<u64>,
    render: crate::AudioRenderContract,
    expected_parameters: Vec<super::ClapParameterDescriptor>,
    configuration_phase: super::native_configuration_control::NativeConfigurationPhase,
    configuration_seed: Option<Vec<u8>>,
    staged_audio_inputs: Option<Box<super::clap_audio_input_buffers::StagedClapAudioInputs>>,
    processor: Option<PluginAudioProcessor<ClapHost>>,
    instance: PluginInstance<ClapHost>,
    contract: AudioProcessorExecutionContract,
    input: Vec<Vec<Vec<f32>>>,
    main_audio_input: ClapMainAudioInput,
    auxiliary_inputs: AudioProcessorAuxiliaryInputContract,
    input_names: Vec<crate::AudioProcessorInputName>,
    output: Vec<Vec<Vec<f32>>>,
    main_audio_output_id: u32,
    auxiliary_outputs: AudioProcessorAuxiliaryOutputContract,
    input_ports: AudioPorts,
    output_ports: AudioPorts,
    parameter_lanes: Vec<(ClapId, f64, f64, bool)>,
    input_events: ClapInputEvents,
    output_events: ClapOutputEvents,
    last_end: Option<i64>,
    // Actual accepted activation, consumed once by the first signal entry.
    initial_signal_available: bool,
    log_receiver: super::native_audio_log::NativeAudioLogReceiver,
    shared: ClapHostShared,
    // Last: OLE/main lifetime outlives every native GUI/processor/instance drop.
    #[cfg(windows)]
    _native_main: std::rc::Rc<vst3_host::window::WindowsEditorMainThread>,
}

impl ClapWorker {
    fn service_note_ports(&mut self) -> Result<(), AudioProcessorHostError> {
        if let Some(inputs) = &self.note_inputs {
            super::clap_note_input_contract::service_names(&mut self.instance, inputs)?;
        }
        Ok(())
    }
    #[cfg(windows)]
    fn native_editor_binding_matches(&self, request: crate::NativeAudioObjectBindRequest) -> bool {
        self.native_binding.is_some_and(|receipt| {
            receipt.binding() == request.binding
                && receipt.object() == self.native_object
                && request.expected_object == self.native_object
        })
    }
}

impl IsolatedAudioProcessorWorker for ClapWorker {
    fn note_input_contract(&self) -> crate::AudioProcessorNoteInputContract {
        self.note_inputs
            .as_ref()
            .map_or_else(Default::default, |inputs| inputs.contract.clone())
    }
    fn native_worker_lane_resources(&self) -> Option<crate::NativeWorkerLaneResourceProfile> {
        self.native_lane
    }

    fn native_staged_seed_layout(&self) -> crate::NativeHostBufferLayout {
        let bytes = match &self.configuration_phase {
            super::native_configuration_control::NativeConfigurationPhase::ReadyForInstall {
                candidate,
                ..
            } => candidate
                .staged_receipt()
                .map_or(0, |receipt| receipt.child_seed_capacity_bytes),
            _ => 0,
        };
        crate::NativeHostBufferLayout { bytes, alignment: align_of::<u8>() }
    }

    fn uses_native_processing_split(&self) -> bool {
        cfg!(windows)
    }

    #[cfg(windows)]
    fn native_control_only_role(
        &mut self,
    ) -> Result<Option<Box<dyn super::IsolatedAudioProcessorMainRole + '_>>, AudioProcessorHostError>
    {
        // A new CLAP instance is initially Stopped but can acquire its original
        // DSP scheduling role for Ready. A committed seed instead waits for the
        // next explicit Enter; these facts already belong to the retained owner.
        if self.configuration_phase.blocked_request().is_none()
            && !(self.configuration_seed.is_some() && self.last_end.is_none())
        {
            if self.processor.is_none() {
                return Err(unavailable("CLAP processor is closed"));
            }
            return Ok(None);
        }
        Ok(Some(Box::new(ClapMainRole {
            note_inputs: self.note_inputs.as_ref(),
            instance: &mut self.instance,
            gui: &mut self.gui,
            shared: &self.shared,
            phase: &self.configuration_phase,
            object: self.native_object,
            receiver: std::cell::RefCell::new(&mut self.log_receiver),
            binding: self.native_binding,
            author_delivery: &self.native_author_delivery,
        })))
    }

    #[cfg(windows)]
    fn split_native_processing(
        &mut self,
    ) -> Result<super::IsolatedAudioProcessorWorkerSplit<'_>, AudioProcessorHostError> {
        let native =
            self.processor.as_mut().ok_or_else(|| unavailable("CLAP processor is closed"))?;
        let signal = ClapSignalFields {
            configuration_phase: &self.configuration_phase,
            main_audio_input: self.main_audio_input,
            input: &mut self.input,
            output: &mut self.output,
            auxiliary_inputs: &self.auxiliary_inputs,
            auxiliary_outputs: &self.auxiliary_outputs,
            input_ports: &mut self.input_ports,
            output_ports: &mut self.output_ports,
            parameter_lanes: &self.parameter_lanes,
            input_events: &mut self.input_events,
            output_events: &mut self.output_events,
            last_end: &mut self.last_end,
            shared: &self.shared,
        };
        Ok(super::IsolatedAudioProcessorWorkerSplit {
            main: Box::new(ClapMainRole {
                note_inputs: self.note_inputs.as_ref(),
                instance: &mut self.instance,
                gui: &mut self.gui,
                shared: &self.shared,
                phase: &self.configuration_phase,
                object: self.native_object,
                receiver: std::cell::RefCell::new(&mut self.log_receiver),
                binding: self.native_binding,
                author_delivery: &self.native_author_delivery,
            }),
            processing: Box::new(ClapDspRole { native, signal }),
        })
    }

    fn native_audio_object_observation(
        &self,
    ) -> Result<Option<crate::NativeAudioObjectObservation>, AudioProcessorHostError> {
        Ok(Some(self.native_object))
    }

    fn bind_native_audio_object(
        &mut self,
        request: crate::NativeAudioObjectBindRequest,
    ) -> Result<crate::NativeAudioObjectBindOutcome, AudioProcessorHostError> {
        match crate::NativeAudioObjectBindReceipt::acknowledge(request, self.native_object) {
            Ok(receipt) => {
                if self.native_binding != Some(receipt) {
                    // A new target needs one real query even when Source did not change.
                    self.parameter_observation_revision = None;
                }
                self.native_binding = Some(receipt);
                Ok(crate::NativeAudioObjectBindOutcome::Bound(receipt))
            }
            Err(reason) => Ok(crate::NativeAudioObjectBindOutcome::Refused(reason)),
        }
    }

    #[cfg(windows)]
    fn open_native_audio_editor(
        &mut self,
        request: crate::NativeAudioEditorOpenRequest,
    ) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
        use crate::{NativeAudioEditorOutcome as Outcome, NativeAudioEditorRefusal as Refusal};
        let binding = request.binding();
        if !self.native_editor_binding_matches(binding) {
            return Ok(Outcome::Refused(Refusal::OwnerChanged));
        }
        if self.pending_reprepare().is_some()
            || self.configuration_phase.ready_token().is_some()
            || self.gui.close_requested()?
            || self.instance.access_shared_handler(|host| host.services.close_requested())
        {
            return Ok(Outcome::Refused(Refusal::ConfigurationPending));
        }
        let preflight = self.gui.prepare_open(&mut self.instance);
        match preflight {
            super::clap_gui_owner::ClapGuiOpenPreflight::Unavailable => {
                return Ok(Outcome::Refused(Refusal::NoEditor));
            }
            super::clap_gui_owner::ClapGuiOpenPreflight::Attached(size) => {
                // Preserve live Open idempotence; this synchronization slice
                // initializes only a genuinely new surface, never re-flushes it.
                self.parameter_observation_revision = None;
                return Ok(Outcome::Opened { binding, width: size.width, height: size.height });
            }
            super::clap_gui_owner::ClapGuiOpenPreflight::Create(_) => {}
        }
        if let Some(refusal) = self.project_idle_static_parameters(&request)? {
            return Ok(Outcome::Refused(refusal));
        }
        let projected = !request.static_parameter_values().is_empty();
        let Some(size) = self.gui.open_prepared(&mut self.instance, request.title(), preflight)?
        else {
            if projected {
                return Err(invalid(
                    "CLAP native GUI create failed after static parameter exchange",
                ));
            }
            return Ok(Outcome::Refused(Refusal::NoEditor));
        };
        // A newly opened surface gets one actual presentation query even when
        // no native source callbacks were needed to create its view.
        self.parameter_observation_revision = None;
        Ok(Outcome::Opened { binding, width: size.width, height: size.height })
    }

    #[cfg(windows)]
    fn close_native_audio_editor(
        &mut self,
        binding: crate::NativeAudioObjectBindRequest,
    ) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
        use crate::{NativeAudioEditorOutcome as Outcome, NativeAudioEditorRefusal as Refusal};
        if !self.native_editor_binding_matches(binding) {
            return Ok(Outcome::Refused(Refusal::OwnerChanged));
        }
        self.gui.close(&mut self.instance)?;
        Ok(Outcome::Closed { binding })
    }

    #[cfg(windows)]
    fn close_native_editor_owner(&mut self) -> Result<(), AudioProcessorHostError> {
        self.gui.close(&mut self.instance)
    }

    #[cfg(windows)]
    fn service_native_editor_close_request(
        &mut self,
    ) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
        let Some(receipt) = self.native_binding else {
            self.close_native_editor_owner()?;
            return Ok(crate::NativeAudioEditorOutcome::Refused(
                crate::NativeAudioEditorRefusal::OwnerChanged,
            ));
        };
        self.close_native_audio_editor(crate::NativeAudioObjectBindRequest {
            binding: receipt.binding(),
            expected_object: receipt.object(),
        })
    }

    fn poll_native_observations(
        &mut self,
        consumption: crate::NativeAudioObservationConsumption,
    ) -> Result<crate::NativeAudioObservations, AudioProcessorHostError> {
        // Only the actual child main/control path drains the one receiver.
        let logs = self.instance.access_shared_handler(|host| {
            self.log_receiver.observations(&host.logs, consumption)
        })?;
        let (author, parameter_revision, parameters) = self.observe_native_author(consumption)?;
        Ok(crate::NativeAudioObservations {
            object: Some(self.native_object),
            logs,
            author: Some(author),
            parameter_revision,
            parameters,
            ..Default::default()
        })
    }

    fn native_configuration_cohort(&self) -> Option<super::NativeRequestCohort> {
        clap_native_cohort(&self.shared)
    }

    fn service_native_configuration(
        &mut self,
        request: super::NativeConfigurationControlRequest<'_>,
    ) -> Result<super::NativeConfigurationControlOutcome, AudioProcessorHostError> {
        if self.note_inputs.is_some() {
            // This slice never reuses a Note binding across native reactivation.
            // Root's coordinated configuration/contract migration remains an
            // explicit subsequent consumer; no cohort/marker is cleared here.
            return Ok(super::NativeConfigurationControlOutcome::CapabilityRefused(
                super::NativeConfigurationRefusal::UnsupportedAdapter,
            ));
        }
        use super::native_configuration_control::{
            observed_contract, FixedConfigurationWriter, NativeConfigurationPhase as Phase,
        };
        use super::{
            NativeConfigurationCandidate as Candidate,
            NativeConfigurationControlOutcome as Outcome, NativeConfigurationFormat as Format,
            NativeConfigurationRefusal as Refusal, NativeControlBufferReceipt as Buffers,
        };
        use clack_extensions::params::PluginParams;
        if self.staged_audio_inputs.is_some()
            || self.configuration_phase.ready_token().is_some()
            || matches!(self.configuration_phase, Phase::AudioInputsProposed(_))
        {
            return Ok(Outcome::CapabilityRefused(Refusal::AwaitingInstallation));
        }
        let expected_native = self.shared.restart_cohort();
        let actual = expected_native.pending().then_some(super::NativeRequestCohort {
            serial: expected_native.serial(),
            request: mondrian_core::NativeAudioReprepareRequest::ClapRestart,
        });
        if actual != Some(request.expected_cohort) {
            return Ok(Outcome::StillPending(actual));
        }
        self.configuration_phase = Phase::Blocked(request.expected_cohort.request);
        if request.expected_cohort.exhausted() {
            return Ok(Outcome::CapabilityRefused(Refusal::SerialExhausted));
        }
        if request.expected_cohort.request
            != mondrian_core::NativeAudioReprepareRequest::ClapRestart
        {
            return Err(invalid(
                "CLAP configuration control received a different ABI cohort",
            ));
        }
        if request.configuration_limit_bytes == 0 || request.configuration_limit_bytes > 64 * 1024 {
            return Ok(Outcome::CapabilityRefused(Refusal::ConfigurationTooLarge));
        }
        if request.canonical_constant_values.len() != self.parameter_lanes.len()
            || self.parameter_lanes.iter().zip(request.canonical_constant_values).any(
                |(&(_, min, max, stepped), value)| {
                    value.is_some_and(|value| {
                        !value.is_finite()
                            || value < min
                            || value > max
                            || (stepped && value.fract() != 0.0)
                    })
                },
            )
        {
            return Ok(Outcome::CapabilityRefused(
                Refusal::UnsupportedConstantProfile,
            ));
        }
        // Grant every state byte before entering the native lifecycle/state code.
        let mut writer = match FixedConfigurationWriter::new(
            request.configuration_limit_bytes,
            request.control_buffer_grant_bytes,
        ) {
            Ok(writer) => writer,
            Err(refusal) => return Ok(Outcome::CapabilityRefused(refusal)),
        };
        if let Some(mut processor) = self.processor.take() {
            {
                let _audio_role = self.shared.enter_audio_thread();
                processor.ensure_processing_stopped();
            }
            drop(processor);
        }
        self.instance.try_deactivate().map_err(|error| {
            unavailable(format!("CLAP configuration deactivation failed: {error}"))
        })?;
        self.instance
            .access_shared_handler(|host| host.services.active.store(false, Ordering::Release));
        self.shared.service_callbacks(&mut self.instance)?;
        self.input_events.clear();
        for (&(id, _, _, _), &value) in
            self.parameter_lanes.iter().zip(request.canonical_constant_values)
        {
            let Some(value) = value else { continue };
            if !self
                .input_events
                .try_push(ParamValueEvent::new(0, id, Pckn::match_all(), value))
            {
                return Err(invalid(
                    "CLAP configuration values exceed already admitted parameter storage",
                ));
            }
        }
        {
            let mut handle = self
                .instance
                .inactive_plugin_handle()
                .ok_or_else(|| invalid("CLAP service still active before parameter flush"))?;
            // Native-owned lanes have no synthetic inactive parameter write.
            // An all-None profile must not create an empty flush just because
            // the admitted directory is nonempty.
            if request.canonical_constant_values.iter().any(Option::is_some) {
                let params = handle
                    .get_extension::<PluginParams>()
                    .ok_or_else(|| invalid("CLAP admitted parameters lost their extension"))?;
                let mut output = self
                    .output_events
                    .writer(ClapOutputOrigin::InactiveFlush)
                    .map_err(|_| AudioProcessorHostError::NativeParameterSourceUnavailable)?;
                params.flush(
                    &mut handle,
                    &self.input_events.as_input(),
                    &mut output.as_output(),
                );
            }
        }
        self.shared.service_callbacks(&mut self.instance)?;
        configure_render_mode(&self.instance.plugin_handle(), self.render.processing_mode)?;
        self.shared.service_callbacks(&mut self.instance)?;
        let handle = self.instance.plugin_handle();
        let mut metadata_scratch = super::clap_metadata::MetadataScanScratch::new();
        // Current values change after a valid flush; structural metadata must not.
        if !super::clap_metadata::revalidate_parameters(
            &handle,
            &self.expected_parameters,
            &mut metadata_scratch,
        )? {
            return Ok(Outcome::CapabilityRefused(Refusal::MetadataChanged));
        }
        let mut proposal =
            super::ClapAudioInputProposal::new(request.expected_cohort, request.control_token)
                .map_err(invalid)?;
        let mut proposal_error = None;
        let (inputs_match, main_and_outputs_match) =
            super::clap_metadata::inspect_audio_port_configuration(
                &handle,
                self.render.channel_layout,
                self.main_audio_input,
                &self.auxiliary_inputs,
                self.main_audio_output_id,
                &self.auxiliary_outputs,
                &mut metadata_scratch,
                |port| {
                    if let Err(error) = proposal.push(port) {
                        proposal_error.get_or_insert(error);
                    }
                },
            )?;
        if let Some(error) = proposal_error {
            return Err(invalid(error));
        }
        if !main_and_outputs_match {
            return Ok(Outcome::CapabilityRefused(Refusal::MetadataChanged));
        }
        if !inputs_match {
            // No native plane allocation, activation or cohort retirement. The
            // existing owner remains stopped; Root must admit and coordinate
            // the prospective geometry before any further lifecycle action.
            let actual = self.native_configuration_cohort();
            if actual != Some(request.expected_cohort) {
                return Ok(Outcome::StillPending(actual));
            }
            self.configuration_phase = Phase::AudioInputsProposed(proposal);
            return Ok(Outcome::ClapAudioInputsChanged(proposal));
        }
        let configuration = PluginAudioConfiguration {
            sample_rate: f64::from(self.render.sample_rate),
            min_frames_count: 1,
            max_frames_count: u32::try_from(self.render.max_block_frames)
                .map_err(|_| invalid("CLAP retained frame extent exceeds u32"))?,
        };
        let mut processor: PluginAudioProcessor<ClapHost> = self
            .instance
            .activate(|_, _| (), configuration)
            .map_err(|error| unavailable(format!("CLAP configuration activation failed: {error}")))?
            .into();
        self.instance
            .access_shared_handler(|host| host.services.active.store(true, Ordering::Release));
        let latency = {
            let handle = self.instance.plugin_handle();
            handle
                .get_extension::<PluginLatency>()
                .map(|extension| extension.get(&handle) as usize)
                .unwrap_or(0)
        };
        let tail = {
            let _audio_role = self.shared.enter_audio_thread();
            processor
                .plugin_handle()
                .get_extension::<PluginTail>()
                .map(|extension| extension.get(&processor.plugin_handle()))
                .unwrap_or(TailLength::Finite(0))
        };
        self.processor = Some(processor);
        let tail = match tail {
            TailLength::Finite(0) => crate::AudioProcessorTail::None,
            TailLength::Finite(frames) => crate::AudioProcessorTail::Finite(frames as usize),
            TailLength::Infinite => crate::AudioProcessorTail::Infinite,
        };
        let observed = observed_contract(self.contract, latency, tail)?;
        self.shared.service_callbacks(&mut self.instance)?;
        let handle = self.instance.plugin_handle();
        // CLAP audio-ports may only be scanned while deactivated. The inactive
        // shape+auxiliary check above remains authoritative through activation;
        // active changes require the existing restart/cohort control path.
        if !super::clap_metadata::revalidate_parameters(
            &handle,
            &self.expected_parameters,
            &mut metadata_scratch,
        )? {
            return Ok(Outcome::CapabilityRefused(Refusal::MetadataChanged));
        }
        let Some(state) = handle.get_extension::<PluginState>() else {
            return Ok(Outcome::CapabilityRefused(
                Refusal::ConfigurationUnavailable,
            ));
        };
        let result = state.save(&handle, &mut writer);
        let capacity = writer.capacity();
        // Inspect the host writer first: native getState may ignore write failure.
        let configuration = match writer.finish() {
            Ok(bytes) => bytes,
            Err(refusal) => return Ok(Outcome::CapabilityRefused(refusal)),
        };
        if result.is_err() {
            return Ok(Outcome::CapabilityRefused(
                Refusal::ConfigurationUnavailable,
            ));
        }
        self.shared.service_callbacks(&mut self.instance)?;
        if !self.shared.retire_restart_cohort(expected_native) {
            return Ok(Outcome::StillPending(self.native_configuration_cohort()));
        }
        let candidate = Candidate {
            serviced_cohort: request.expected_cohort,
            control_token: request.control_token,
            observed_contract: observed,
            format: Format::ClapState,
            configuration,
            buffers: Buffers {
                request_value_capacity_bytes: 0,
                capture_payload_bytes: 0,
                configuration_capacity_bytes: capacity,
            },
            parent_configuration_capacity_bytes: 0,
        };
        self.configuration_phase = Phase::issued(&candidate);
        Ok(Outcome::ReadyForInstall(candidate))
    }
    fn stage_clap_audio_input_buffers(
        &mut self,
        request: super::ClapAudioInputBufferRequest,
    ) -> Result<super::ClapAudioInputBufferOutcome, AudioProcessorHostError> {
        use super::native_configuration_control::NativeConfigurationPhase as Phase;
        use super::{
            ClapAudioInputBufferOutcome as Outcome, NativeConfigurationRefusal as Refusal,
        };
        if self.staged_audio_inputs.is_some() {
            return Ok(Outcome::Refused(Refusal::AwaitingInstallation));
        }
        if !matches!(&self.configuration_phase, Phase::AudioInputsProposed(proposal) if *proposal == request.proposal)
            || request.max_block_frames != self.render.max_block_frames
            || self.processor.is_some()
            || self.instance.inactive_plugin_handle().is_none()
        {
            return Ok(Outcome::Refused(Refusal::UnsupportedRequest));
        }
        let Some(bound) = super::clap_audio_input_buffers::reservation(
            &request.proposal,
            request.max_block_frames,
        ) else {
            return Ok(Outcome::Refused(Refusal::BufferGrantExceeded));
        };
        if bound > request.buffer_grant_bytes {
            return Ok(Outcome::Refused(Refusal::BufferGrantExceeded));
        }
        if self.native_configuration_cohort() != Some(request.proposal.observed_cohort) {
            return Ok(Outcome::Refused(Refusal::MetadataChanged));
        }
        let mut scratch = super::clap_metadata::MetadataScanScratch::new();
        let handle = self.instance.plugin_handle();
        if !super::clap_metadata::revalidate_parameters(
            &handle,
            &self.expected_parameters,
            &mut scratch,
        )? {
            return Ok(Outcome::Refused(Refusal::MetadataChanged));
        }
        let mut expected = request.proposal.ports();
        let mut matches = true;
        let (_, main_and_outputs_match) = super::clap_metadata::inspect_audio_port_configuration(
            &handle,
            self.render.channel_layout,
            self.main_audio_input,
            &self.auxiliary_inputs,
            self.main_audio_output_id,
            &self.auxiliary_outputs,
            &mut scratch,
            |port| {
                matches &= expected.next() == Some(&port);
            },
        )?;
        if !matches || expected.next().is_some() || !main_and_outputs_match {
            return Ok(Outcome::Refused(Refusal::MetadataChanged));
        }
        let staged = match super::clap_audio_input_buffers::StagedClapAudioInputs::prepare(request)
        {
            Ok(staged) => staged,
            Err(refusal) => return Ok(Outcome::Refused(refusal)),
        };
        let receipt = staged.receipt;
        // Adopt before the final callback check. A stale request must not expose
        // either geometry or accidentally credit still-retained resource owners.
        self.staged_audio_inputs = Some(staged);
        if self.native_configuration_cohort() != Some(request.proposal.observed_cohort) {
            self.configuration_phase = Phase::Blocked(request.proposal.observed_cohort.request);
            return Ok(Outcome::Refused(Refusal::MetadataChanged));
        }
        self.configuration_phase = Phase::AudioInputsStaged(request.proposal);
        Ok(Outcome::Staged(receipt))
    }

    fn stage_native_configuration_seed(
        &mut self,
        seed: super::NativeConfigurationOwnedSeed,
    ) -> Result<super::NativeConfigurationSeedOutcome, AudioProcessorHostError> {
        let current = self.shared.restart_cohort();
        if current.pending()
            || current.serial() != seed.ticket.serviced_cohort.serial
            || self.configuration_phase.seed_ticket() != Some(seed.ticket)
        {
            return Ok(super::NativeConfigurationSeedOutcome::Refused(
                super::NativeConfigurationSeedRefusal::StaleTicket,
            ));
        }
        Ok(self.configuration_phase.stage_seed(seed))
    }

    fn commit_native_configuration_seed(
        &mut self,
        ticket: super::NativeConfigurationSeedTicket,
    ) -> Result<super::NativeConfigurationSeedOutcome, AudioProcessorHostError> {
        if let Err(refusal) = self.configuration_phase.seed_receipt(ticket) {
            return Ok(super::NativeConfigurationSeedOutcome::Refused(refusal));
        }
        if !self.shared.claim_retired_restart_serial(ticket.serviced_cohort.serial) {
            return Ok(super::NativeConfigurationSeedOutcome::Refused(
                super::NativeConfigurationSeedRefusal::StaleTicket,
            ));
        }
        let (seed, receipt) = match self.configuration_phase.take_seed(ticket) {
            Ok(value) => value,
            Err(refusal) => return Ok(super::NativeConfigurationSeedOutcome::Refused(refusal)),
        };
        // The legally activated stopped component already has this configuration.
        // Retain the seed owner for this installed candidate without reloading or
        // pretending it is private DSP history. Explicit Enter resets processing.
        let previous = self.configuration_seed.replace(seed);
        self.contract = ticket.observed_contract;
        self.last_end = None;
        self.configuration_phase =
            super::native_configuration_control::NativeConfigurationPhase::Admitted;
        drop(previous);
        Ok(super::NativeConfigurationSeedOutcome::Accepted(receipt))
    }

    fn pending_reprepare(&self) -> Option<crate::NativeObservedRequest> {
        clap_native_cohort(&self.shared)
            .map(crate::NativeObservedRequest::Cohort)
            .or(self
                .configuration_phase
                .blocked_request()
                .map(crate::NativeObservedRequest::RequestOnly))
    }

    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        self.contract
    }

    fn auxiliary_input_contract(&self) -> AudioProcessorAuxiliaryInputContract {
        self.auxiliary_inputs.clone()
    }

    fn auxiliary_output_contract(&self) -> AudioProcessorAuxiliaryOutputContract {
        self.auxiliary_outputs.clone()
    }

    fn auxiliary_input_names(&self) -> Vec<crate::AudioProcessorInputName> {
        self.input_names.clone()
    }

    fn enter_state(&mut self, start_sample: i64) -> Result<(), AudioProcessorHostError> {
        if let Some(request) = self.pending_reprepare() {
            return Err(AudioProcessorHostError::ReprepareRequired(request));
        }
        self.native_binding = None;
        if self.initial_signal_available {
            self.initial_signal_available = false;
            self.last_end = Some(start_sample);
            return Ok(());
        }
        let _audio_role = self.shared.enter_audio_thread();
        self.processor
            .as_mut()
            .ok_or_else(|| {
                AudioProcessorHostError::StateEntry("CLAP processor is closed".to_owned())
            })?
            .reset();
        self.last_end = Some(start_sample);
        Ok(())
    }

    fn process(
        &mut self,
        block: IsolatedAudioProcessorWorkerBlock<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        self.service_note_ports()?;
        let processor = self.processor.as_mut().ok_or_else(|| {
            AudioProcessorHostError::Process("CLAP processor is closed".to_owned())
        })?;
        let mut signal = ClapSignalFields {
            configuration_phase: &self.configuration_phase,
            main_audio_input: self.main_audio_input,
            input: &mut self.input,
            output: &mut self.output,
            auxiliary_inputs: &self.auxiliary_inputs,
            auxiliary_outputs: &self.auxiliary_outputs,
            input_ports: &mut self.input_ports,
            output_ports: &mut self.output_ports,
            parameter_lanes: &self.parameter_lanes,
            input_events: &mut self.input_events,
            output_events: &mut self.output_events,
            last_end: &mut self.last_end,
            shared: &self.shared,
        };
        signal.process(processor, block)?;
        self.instance.access_shared_handler(|host| host.services.note_ports.check())?;
        super::clap_main_services::service_main_tick(&self.shared, &mut self.instance)
    }
}

/// Compare definition facts, excluding current values changed by a legal flush.
fn same_clap_parameter_structure(
    actual: &[super::ClapParameterDescriptor],
    expected: &[super::ClapParameterDescriptor],
) -> bool {
    actual.len() == expected.len()
        && actual.iter().zip(expected).all(|(a, e)| {
            super::clap_metadata::same_parameter_structure(
                super::clap_metadata::ParameterRow::from_descriptor(a),
                super::clap_metadata::ParameterRow::from_descriptor(e),
            )
        })
}

// Error-only projection of the already captured vectors. This is not a native
// query, author schema, or validation substitute. The comparison above remains
// authoritative, including numeric signed-zero equivalence and field order.
fn first_clap_parameter_structure_mismatch(
    actual: &[super::ClapParameterDescriptor],
    expected: &[super::ClapParameterDescriptor],
) -> Option<String> {
    first_clap_parameter_structure_mismatch_observed(actual, expected, None)
}

fn first_clap_parameter_structure_mismatch_observed(
    actual: &[super::ClapParameterDescriptor],
    expected: &[super::ClapParameterDescriptor],
    capture: Option<&ParameterQueryCapture>,
) -> Option<String> {
    if actual.len() != expected.len() {
        let mut detail = format!(
            "CLAP parameter metadata changed since contract probe: validation_site=native_after_state_render_mode field=count expected_count={} actual_count={}",
            expected.len(), actual.len(),
        );
        if let Some(capture) = capture {
            detail.push_str(&capture.suffix(None, None));
        }
        return Some(detail);
    }
    for (index, (a, e)) in actual.iter().zip(expected).enumerate() {
        // IDs and booleans below are unsigned scalar projections; float fields
        // retain their direct f64 bits. These are not full raw native flag bits.
        let (field, expected_bits, actual_bits) = if a.id != e.id {
            ("id", u64::from(e.id), u64::from(a.id))
        } else if a.min_value != e.min_value {
            ("min_value", e.min_value.to_bits(), a.min_value.to_bits())
        } else if a.max_value != e.max_value {
            ("max_value", e.max_value.to_bits(), a.max_value.to_bits())
        } else if a.default_value != e.default_value {
            (
                "default_value",
                e.default_value.to_bits(),
                a.default_value.to_bits(),
            )
        } else if a.automatable != e.automatable {
            ("automatable", e.automatable as u64, a.automatable as u64)
        } else if a.stepped != e.stepped {
            ("stepped", e.stepped as u64, a.stepped as u64)
        } else if a.enumeration != e.enumeration {
            ("enumeration", e.enumeration as u64, a.enumeration as u64)
        } else if a.read_only != e.read_only {
            ("read_only", e.read_only as u64, a.read_only as u64)
        } else {
            continue;
        };
        let mut detail = format!(
            "CLAP parameter metadata changed since contract probe: validation_site=native_after_state_render_mode index={index} field={field} expected_native_id={} actual_native_id={} expected_bits={expected_bits:016X} actual_bits={actual_bits:016X}",
            e.id, a.id,
        );
        if let Some(capture) = capture {
            detail.push_str(&capture.suffix(Some(index), Some(a)));
        }
        return Some(detail);
    }
    None
}

impl Drop for ClapWorker {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.gui.close(&mut self.instance).is_err() {
            // An attached/indeterminate foreign GUI must not outlive its DLL.
            // The parent supervised connection observes this isolated retirement.
            std::process::abort();
        }
        if let Some(mut processor) = self.processor.take() {
            {
                let _audio_role = self.shared.enter_audio_thread();
                processor.ensure_processing_stopped();
            }
            drop(processor);
        }
        self.instance
            .access_shared_handler(|host| host.services.active.store(false, Ordering::Release));
        let _ = self.instance.try_deactivate();
    }
}

/// Convert the canonical signed sample grid at the CLAP seconds boundary.
///
/// Round to the nearest fixed-point tick, with half ties away from zero, using
/// integer arithmetic. Reject values outside CLAP's representable seconds range.
fn clap_seconds_position(
    start_sample: i64,
    sample_rate: u32,
) -> Result<SecondsTime, AudioProcessorHostError> {
    if sample_rate == 0 {
        return Err(invalid("CLAP transport sample rate is zero"));
    }
    let scaled = i128::from(start_sample) * i128::from(SecondsTime::FACTOR);
    let denominator = i128::from(sample_rate);
    let quotient = scaled / denominator;
    let remainder = scaled % denominator;
    let rounded = quotient
        + if remainder.abs() * 2 >= denominator {
            scaled.signum()
        } else {
            0
        };
    let bits = i64::try_from(rounded).map_err(|_| {
        AudioProcessorHostError::Process(
            "CLAP signed timeline position exceeds the seconds representation".to_owned(),
        )
    })?;
    Ok(SecondsTime::from_bits(bits))
}

pub(super) fn clap_transport(
    start_sample: i64,
    sample_rate: u32,
    reference: Option<mondrian_core::MusicalTimeReference>,
    activity: Option<mondrian_core::TimelineTransportActivity>,
) -> Result<TransportEvent, AudioProcessorHostError> {
    let tempo = crate::processor::musical_tempo(reference)?;
    let seconds = clap_seconds_position(start_sample, sample_rate)?;
    let beats = reference
        .map(|reference| beat_position::position(start_sample, sample_rate, reference))
        .transpose()
        .map_err(|error| match error {
            beat_position::BeatPositionError::ZeroSampleRate => {
                invalid("CLAP transport sample rate is zero")
            }
            beat_position::BeatPositionError::OutOfRange => AudioProcessorHostError::Process(
                "CLAP signed musical position exceeds the beats representation".to_owned(),
            ),
        })?;
    let bar = reference
        .map(|reference| beat_position::bar_position(start_sample, sample_rate, reference))
        .transpose()
        .map_err(|error| match error {
            beat_position::BeatPositionError::ZeroSampleRate => {
                invalid("CLAP transport sample rate is zero")
            }
            beat_position::BeatPositionError::OutOfRange => AudioProcessorHostError::Process(
                "CLAP musical bar exceeds the bar-number or beat representation".to_owned(),
            ),
        })?
        .flatten();
    let signature = reference.and_then(|reference| reference.time_signature());
    Ok(TransportEvent {
        header: EventHeader::new_core(0, EventFlags::empty()),
        flags: TransportFlags::HAS_SECONDS_TIMELINE
            | if activity == Some(mondrian_core::TimelineTransportActivity::Playing) {
                TransportFlags::IS_PLAYING
            } else {
                TransportFlags::empty()
            }
            | if tempo.is_some() {
                TransportFlags::HAS_TEMPO | TransportFlags::HAS_BEATS_TIMELINE
            } else {
                TransportFlags::empty()
            }
            | if signature.is_some() {
                TransportFlags::HAS_TIME_SIGNATURE
            } else {
                TransportFlags::empty()
            },
        song_pos_seconds: seconds,
        song_pos_beats: beats.unwrap_or(BeatTime::from_bits(0)),
        tempo: tempo.unwrap_or(0.0),
        tempo_inc: 0.0,
        loop_start_beats: BeatTime::from_bits(0),
        loop_end_beats: BeatTime::from_bits(0),
        loop_start_seconds: SecondsTime::from_bits(0),
        loop_end_seconds: SecondsTime::from_bits(0),
        bar_start: bar.map_or(BeatTime::from_bits(0), |(_, start)| start),
        bar_number: bar.map_or(0, |(number, _)| number),
        time_signature_numerator: signature.map_or(0, |signature| signature.numerator()),
        time_signature_denominator: signature.map_or(0, |signature| signature.denominator()),
    })
}

pub(super) fn invalid(detail: impl Into<String>) -> AudioProcessorHostError {
    AudioProcessorHostError::InvalidContract(detail.into())
}

fn unavailable(detail: impl Into<String>) -> AudioProcessorHostError {
    AudioProcessorHostError::Unavailable(detail.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioProcessingMode, AudioProcessorTail, AudioRenderContract};
    use clack_extensions::audio_ports::{
        AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
        PluginAudioPortsImpl,
    };
    use clack_extensions::latency::{PluginLatency, PluginLatencyImpl};
    use clack_extensions::params::{
        ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
        PluginMainThreadParams, PluginParams,
    };
    use clack_extensions::render::{PluginRender, PluginRenderImpl, RenderMode};
    use clack_extensions::state::PluginStateImpl;
    use clack_extensions::tail::{PluginTail, PluginTailImpl, TailLength};
    use clack_extensions::thread_check::HostThreadCheck;
    use clack_plugin::events::spaces::CoreEventSpace;
    use clack_plugin::prelude::{
        Audio, ChannelPair, ClapId, DefaultPluginFactory, Events, HostAudioProcessorHandle,
        HostMainThreadHandle, HostSharedHandle, OutputEvents, Plugin,
        PluginAudioConfiguration as PluginConfig, PluginDescriptor, PluginError, PluginExtensions,
        PluginMainThread, PluginShared, Process, ProcessStatus, SinglePluginEntry,
    };
    use clack_plugin::stream::{InputStream, OutputStream};
    use mondrian_core::AudioChannelLayout;
    use std::io::{Read as _, Write as _};
    use std::sync::atomic::AtomicU32;
    use std::sync::atomic::{AtomicUsize, Ordering as TestOrdering};

    static DEACTIVATIONS: AtomicUsize = AtomicUsize::new(0);

    struct GainPlugin;
    struct GainMain;
    struct GainProcessor;

    struct AutomatedGainPlugin;
    struct AutomatedGainShared(AtomicU32);
    struct AutomatedGainMain<'a>(&'a AutomatedGainShared);
    struct AutomatedGainProcessor<'a>(&'a AutomatedGainShared);

    impl PluginShared<'_> for AutomatedGainShared {}

    impl Plugin for AutomatedGainPlugin {
        type AudioProcessor<'a> = AutomatedGainProcessor<'a>;
        type Shared<'a> = AutomatedGainShared;
        type MainThread<'a> = AutomatedGainMain<'a>;

        fn declare_extensions(
            builder: &mut PluginExtensions<Self>,
            _shared: Option<&AutomatedGainShared>,
        ) {
            builder
                .register::<PluginAudioPorts>()
                .register::<PluginParams>()
                .register::<PluginState>();
        }
    }

    impl DefaultPluginFactory for AutomatedGainPlugin {
        fn get_descriptor() -> PluginDescriptor {
            PluginDescriptor::new("org.mondrian.test.automated-gain", "Automated Gain Fixture")
        }

        fn new_shared(_host: HostSharedHandle<'_>) -> Result<AutomatedGainShared, PluginError> {
            Ok(AutomatedGainShared(AtomicU32::new(1.0_f32.to_bits())))
        }

        fn new_main_thread<'a>(
            _host: HostMainThreadHandle<'a>,
            shared: &'a AutomatedGainShared,
        ) -> Result<AutomatedGainMain<'a>, PluginError> {
            Ok(AutomatedGainMain(shared))
        }
    }

    impl<'a> PluginMainThread<'a, AutomatedGainShared> for AutomatedGainMain<'a> {}

    impl PluginStateImpl for AutomatedGainMain<'_> {
        fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
            output.write_all(&self.0 .0.load(Ordering::Relaxed).to_le_bytes())?;
            Ok(())
        }

        fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
            let mut bytes = [0; 4];
            input.read_exact(&mut bytes)?;
            self.0 .0.store(u32::from_le_bytes(bytes), Ordering::Relaxed);
            Ok(())
        }
    }

    impl PluginAudioPortsImpl for AutomatedGainMain<'_> {
        fn count(&self, _is_input: bool) -> u32 {
            1
        }

        fn get(&self, index: u32, _is_input: bool, writer: &mut AudioPortInfoWriter) {
            if index == 0 {
                writer.set(&AudioPortInfo {
                    id: ClapId::new(0),
                    name: b"main",
                    channel_count: 2,
                    flags: AudioPortFlags::IS_MAIN,
                    port_type: Some(AudioPortType::STEREO),
                    in_place_pair: None,
                });
            }
        }
    }

    impl PluginMainThreadParams for AutomatedGainMain<'_> {
        fn count(&self) -> u32 {
            1
        }

        fn get_info(&self, index: u32, writer: &mut ParamInfoWriter) {
            if index == 0 {
                writer.set(&ParamInfo {
                    id: ClapId::new(1),
                    flags: ParamInfoFlags::IS_AUTOMATABLE,
                    cookie: Default::default(),
                    name: b"Gain",
                    module: b"",
                    min_value: 0.0,
                    max_value: 1.0,
                    default_value: 1.0,
                });
            }
        }

        fn get_value(&self, id: ClapId) -> Option<f64> {
            (id == ClapId::new(1)).then(|| f32::from_bits(self.0 .0.load(Ordering::Relaxed)) as f64)
        }

        fn value_to_text(
            &self,
            _id: ClapId,
            _value: f64,
            _writer: &mut ParamDisplayWriter,
        ) -> std::fmt::Result {
            Err(std::fmt::Error)
        }

        fn text_to_value(&self, _id: ClapId, _text: &CStr) -> Option<f64> {
            None
        }

        fn flush(&self, _input: &clack_plugin::prelude::InputEvents, _output: &mut OutputEvents) {}
    }

    impl PluginAudioProcessorParams for AutomatedGainProcessor<'_> {
        fn flush(
            &mut self,
            _input: &clack_plugin::prelude::InputEvents,
            _output: &mut OutputEvents,
        ) {
        }
    }

    impl<'a>
        clack_plugin::plugin::PluginAudioProcessor<'a, AutomatedGainShared, AutomatedGainMain<'a>>
        for AutomatedGainProcessor<'a>
    {
        fn activate(
            _host: HostAudioProcessorHandle<'a>,
            _main: &AutomatedGainMain<'a>,
            shared: &'a AutomatedGainShared,
            _config: PluginConfig,
        ) -> Result<Self, PluginError> {
            Ok(Self(shared))
        }

        fn process(
            &mut self,
            _process: Process,
            mut audio: Audio,
            events: Events,
        ) -> Result<ProcessStatus, PluginError> {
            let mut gains = [f32::from_bits(self.0 .0.load(Ordering::Relaxed)); 4];
            let mut gain = gains[0];
            let mut next = events.input.iter().peekable();
            for (frame, slot) in gains.iter_mut().enumerate() {
                while next.peek().is_some_and(|event| event.header().time() as usize == frame) {
                    let Some(event) = next.next() else { break };
                    if let Some(CoreEventSpace::ParamValue(value)) = event.as_core_event()
                        && value.param_id() == Some(ClapId::new(1))
                    {
                        gain = value.value() as f32;
                    }
                }
                *slot = gain;
            }
            self.0 .0.store(gain.to_bits(), Ordering::Relaxed);
            let mut pair = audio.port_pair(0).ok_or(PluginError::Message("missing main port"))?;
            let mut channels =
                pair.channels()?.into_f32().ok_or(PluginError::Message("missing f32"))?;
            for channel in channels.iter_mut() {
                match channel {
                    ChannelPair::InputOutput(input, output) => {
                        for ((source, destination), gain) in
                            input.iter().zip(output.iter_mut()).zip(gains)
                        {
                            *destination = *source * gain;
                        }
                    }
                    ChannelPair::InPlace(samples) => {
                        for (sample, gain) in samples.iter_mut().zip(gains) {
                            *sample *= gain;
                        }
                    }
                    _ => return Err(PluginError::Message("unpaired channel")),
                }
            }
            Ok(ProcessStatus::Continue)
        }
    }

    impl Plugin for GainPlugin {
        type AudioProcessor<'a> = GainProcessor;
        type Shared<'a> = ();
        type MainThread<'a> = GainMain;

        fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&()>) {
            builder.register::<PluginAudioPorts>();
        }
    }

    impl DefaultPluginFactory for GainPlugin {
        fn get_descriptor() -> PluginDescriptor {
            PluginDescriptor::new("org.mondrian.test.gain", "Mondrian Test Gain")
        }

        fn new_shared(_host: HostSharedHandle<'_>) -> Result<(), PluginError> {
            Ok(())
        }

        fn new_main_thread<'a>(
            _host: HostMainThreadHandle<'a>,
            _shared: &'a (),
        ) -> Result<GainMain, PluginError> {
            Ok(GainMain)
        }
    }

    impl PluginMainThread<'_, ()> for GainMain {}

    impl PluginAudioPortsImpl for GainMain {
        fn count(&self, _is_input: bool) -> u32 {
            1
        }

        fn get(&self, index: u32, _is_input: bool, writer: &mut AudioPortInfoWriter) {
            if index == 0 {
                writer.set(&AudioPortInfo {
                    id: ClapId::new(0),
                    name: b"main",
                    channel_count: 2,
                    flags: AudioPortFlags::IS_MAIN,
                    port_type: Some(AudioPortType::STEREO),
                    in_place_pair: None,
                });
            }
        }
    }

    impl<'a> clack_plugin::plugin::PluginAudioProcessor<'a, (), GainMain> for GainProcessor {
        fn activate(
            _host: HostAudioProcessorHandle<'a>,
            _main_thread: &GainMain,
            _shared: &'a (),
            _config: PluginConfig,
        ) -> Result<Self, PluginError> {
            Ok(Self)
        }

        fn process(
            &mut self,
            _process: Process,
            mut audio: Audio,
            _events: Events,
        ) -> Result<ProcessStatus, PluginError> {
            let mut pair = audio.port_pair(0).ok_or(PluginError::Message("missing main port"))?;
            let mut channels = pair
                .channels()?
                .into_f32()
                .ok_or(PluginError::Message("missing Float32 channels"))?;
            for channel in channels.iter_mut() {
                match channel {
                    ChannelPair::InputOutput(input, output) => {
                        for (source, target) in input.iter().zip(output.iter_mut()) {
                            *target = *source * 2.0;
                        }
                    }
                    ChannelPair::InPlace(samples) => {
                        for sample in samples.iter_mut() {
                            *sample *= 2.0;
                        }
                    }
                    _ => return Err(PluginError::Message("unpaired channel")),
                }
            }
            Ok(ProcessStatus::Continue)
        }

        fn deactivate(self, _main_thread: &GainMain) {
            DEACTIVATIONS.fetch_add(1, TestOrdering::SeqCst);
        }
    }

    #[test]
    fn clap_transport_delivers_only_authored_meter_and_exact_negative_bar() {
        use mondrian_core::{MusicalTimeReference, MusicalTimeSignature, TimelineTime};
        let plain = MusicalTimeReference::new(TimelineTime::ZERO, TimelineTime::new(1, 2).unwrap())
            .unwrap();
        let authored = plain.with_time_signature(Some(MusicalTimeSignature::new(7, 8).unwrap()));
        for activity in [
            None,
            Some(mondrian_core::TimelineTransportActivity::Playing),
            Some(mondrian_core::TimelineTransportActivity::Stopped),
        ] {
            let unknown = clap_transport(-1, 48_000, Some(plain), activity).unwrap();
            let actual = clap_transport(-1, 48_000, Some(authored), activity).unwrap();
            assert_eq!(
                actual.flags,
                unknown.flags | TransportFlags::HAS_TIME_SIGNATURE
            );
            assert_eq!(
                (
                    actual.time_signature_numerator,
                    actual.time_signature_denominator
                ),
                (7, 8)
            );
            assert_eq!(actual.bar_number, -1);
            assert_eq!(actual.bar_start.to_bits(), -(7_i64 << 30));
            assert_eq!(actual.song_pos_beats, unknown.song_pos_beats);
            assert_eq!(actual.song_pos_seconds, unknown.song_pos_seconds);
            assert_eq!(actual.tempo, unknown.tempo);
        }
    }

    #[test]
    fn clap_transport_music_advertises_exact_beats_and_explicit_finite_tempo() {
        use mondrian_core::{MusicalTimeReference, TimelineTime};
        for rate in [44_100, 48_000] {
            for sample in [-120_007, -1, 0, 99_317] {
                let old = clap_transport(sample, rate, None, None).unwrap();
                assert_eq!(old.flags, TransportFlags::HAS_SECONDS_TIMELINE);
                assert_eq!(old.tempo, 0.0);
                for zero in [-91, 713] {
                    let music = MusicalTimeReference::new(
                        TimelineTime::new(zero, 7).unwrap(),
                        TimelineTime::new(3, 4).unwrap(),
                    )
                    .unwrap();
                    let actual = clap_transport(sample, rate, Some(music), None).unwrap();
                    assert_eq!(
                        actual.flags,
                        TransportFlags::HAS_SECONDS_TIMELINE
                            | TransportFlags::HAS_TEMPO
                            | TransportFlags::HAS_BEATS_TIMELINE
                    );
                    assert_eq!(actual.song_pos_seconds, old.song_pos_seconds);
                    assert_eq!(actual.tempo, 80.0);
                    assert_eq!(actual.tempo_inc, 0.0);
                    assert_eq!(
                        actual.song_pos_beats,
                        beat_position::position(sample, rate, music).unwrap()
                    );
                    assert_eq!(actual.bar_start.to_bits(), 0);
                    assert_eq!(actual.bar_number, 0);
                    assert_eq!(actual.time_signature_numerator, 0);
                    assert_eq!(actual.time_signature_denominator, 0);
                }
            }
        }
    }

    #[test]
    fn clap_transport_activity_is_independent_of_music_and_never_invents_validity() {
        use mondrian_core::{MusicalTimeReference, TimelineTime, TimelineTransportActivity};
        let music = MusicalTimeReference::new(TimelineTime::ZERO, TimelineTime::new(1, 2).unwrap()).unwrap();
        for reference in [None, Some(music)] {
            let baseline = clap_transport(-17, 48_000, reference, None).unwrap();
            for activity in [Some(TimelineTransportActivity::Playing), Some(TimelineTransportActivity::Stopped), None] {
                let actual = clap_transport(-17, 48_000, reference, activity).unwrap();
                let playing = if activity == Some(TimelineTransportActivity::Playing) {
                    TransportFlags::IS_PLAYING
                } else {
                    TransportFlags::empty()
                };
                assert_eq!(actual.flags, baseline.flags | playing);
                assert_eq!(actual.song_pos_seconds, baseline.song_pos_seconds);
                assert_eq!(actual.song_pos_beats, baseline.song_pos_beats);
                assert_eq!(actual.tempo, baseline.tempo);
                assert_eq!(actual.time_signature_numerator, 0);
                assert_eq!(actual.time_signature_denominator, 0);
            }
        }
    }

    #[test]
    fn clap_transport_reports_beat_range_error_with_representable_seconds() {
        let music = mondrian_core::MusicalTimeReference::new(
            mondrian_core::TimelineTime::new(i64::MIN, 1).unwrap(),
            mondrian_core::TimelineTime::new(1, 3).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            clap_transport(0, 48_000, Some(music), None),
            Err(AudioProcessorHostError::Process(detail))
                if detail == "CLAP signed musical position exceeds the beats representation"
        ));
        let seconds_only = clap_transport(0, 48_000, None, None).unwrap();
        assert_eq!(seconds_only.flags, TransportFlags::HAS_SECONDS_TIMELINE);
        assert_eq!(seconds_only.song_pos_seconds.to_bits(), 0);
        assert_eq!(seconds_only.song_pos_beats.to_bits(), 0);
    }

    struct SidechainPlugin<const CASE: u8>;
    struct SidechainShared(AtomicBool, bool, usize);
    struct SidechainMain<'a, const CASE: u8>(&'a SidechainShared);
    struct SidechainProcessor<'a>(&'a SidechainShared);
    static LATENCY_DEACTIVATIONS: AtomicUsize = AtomicUsize::new(0);

    impl PluginShared<'_> for SidechainShared {}

    impl<const CASE: u8> Plugin for SidechainPlugin<CASE> {
        type AudioProcessor<'a> = SidechainProcessor<'a>;
        type Shared<'a> = SidechainShared;
        type MainThread<'a> = SidechainMain<'a, CASE>;

        fn declare_extensions(
            builder: &mut PluginExtensions<Self>,
            _shared: Option<&SidechainShared>,
        ) {
            builder.register::<PluginAudioPorts>();
            if CASE == 6 {
                builder.register::<PluginLatency>();
            }
        }
    }

    impl<const CASE: u8> DefaultPluginFactory for SidechainPlugin<CASE> {
        fn get_descriptor() -> PluginDescriptor {
            PluginDescriptor::new("org.mondrian.test.sidechain", "Sidechain ABI Fixture")
        }

        fn new_shared(_host: HostSharedHandle<'_>) -> Result<SidechainShared, PluginError> {
            Ok(SidechainShared(
                AtomicBool::new(false),
                matches!(CASE, 7 | 8),
                if matches!(CASE, 7 | 8) {
                    0
                } else if matches!(CASE, 9 | 11 | 12 | 23) {
                    2
                } else {
                    3
                },
            ))
        }

        fn new_main_thread<'a>(
            _host: HostMainThreadHandle<'a>,
            shared: &'a SidechainShared,
        ) -> Result<SidechainMain<'a, CASE>, PluginError> {
            Ok(SidechainMain(shared))
        }
    }

    impl<'a, const CASE: u8> PluginMainThread<'a, SidechainShared> for SidechainMain<'a, CASE> {}

    impl<const CASE: u8> clack_extensions::latency::PluginLatencyImpl for SidechainMain<'_, CASE> {
        fn get(&self) -> u32 {
            if self.0 .0.load(Ordering::Relaxed) {
                3
            } else {
                u32::MAX
            }
        }
    }

    impl<const CASE: u8> PluginAudioPortsImpl for SidechainMain<'_, CASE> {
        fn count(&self, is_input: bool) -> u32 {
            if is_input {
                if matches!(CASE, 7 | 8) {
                    0
                } else if CASE == 4 {
                    33
                } else if matches!(CASE, 9 | 11 | 12 | 23) {
                    2
                } else if CASE == 10 {
                    32
                } else {
                    3
                }
            } else if CASE == 13 {
                33
            } else if CASE == 14 {
                0
            } else if matches!(CASE, 11 | 15..=21) {
                3
            } else {
                1
            }
        }

        fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
            if index >= self.count(is_input) || (CASE == 5 && index == 2) {
                return;
            }
            let id = if is_input && CASE == 10 {
                100 + index
            } else if is_input && matches!(CASE, 9 | 11 | 12 | 23) {
                if index == 0 {
                    11
                } else {
                    7
                }
            } else {
                match index {
                    0 => 0,
                    1 => 11,
                    _ if CASE == 1 || (!is_input && CASE == 16) => 11,
                    _ => 7,
                }
            };
            writer.set(&AudioPortInfo {
                id: ClapId::new(id),
                name: b"same display name",
                channel_count: if CASE == 22 && is_input && index != 0 {
                    3
                } else if (CASE == 3 && index != 0)
                    || matches!(CASE, 8 | 12)
                    || (CASE == 23 && is_input && index == 0)
                    || (!is_input && CASE == 18 && index == 2)
                {
                    1
                } else {
                    2
                },
                flags: if (is_input && (matches!(CASE, 9..=12) || CASE == 23))
                    || (!is_input && CASE == 15)
                {
                    AudioPortFlags::empty()
                } else if index == 0
                    || (CASE == 2 && index == 1)
                    || (!is_input && CASE == 17 && index == 1)
                {
                    AudioPortFlags::IS_MAIN
                } else {
                    AudioPortFlags::empty()
                },
                port_type: Some(
                    if (CASE == 3 && index != 0)
                        || matches!(CASE, 8 | 12)
                        || (CASE == 22 && is_input && index != 0)
                        || (CASE == 23 && is_input && index == 0)
                    {
                        AudioPortType::MONO
                    } else {
                        AudioPortType::STEREO
                    },
                ),
                in_place_pair: None,
            });
        }
    }

    impl<'a, const CASE: u8>
        clack_plugin::plugin::PluginAudioProcessor<'a, SidechainShared, SidechainMain<'a, CASE>>
        for SidechainProcessor<'a>
    {
        fn activate(
            _host: HostAudioProcessorHandle<'a>,
            _main: &SidechainMain<'a, CASE>,
            shared: &'a SidechainShared,
            config: PluginConfig,
        ) -> Result<Self, PluginError> {
            if config.max_frames_count > 4 {
                return Err(PluginError::Message("fixture block limit"));
            }
            shared.0.store(true, Ordering::Relaxed);
            Ok(Self(shared))
        }

        fn process(
            &mut self,
            _process: Process,
            mut audio: Audio,
            _events: Events,
        ) -> Result<ProcessStatus, PluginError> {
            if matches!(CASE, 19..=21) {
                if audio.input_port_count() != 3 || audio.output_port_count() != 3 {
                    return Err(PluginError::Message("wrong multi-output test extent"));
                }
                for (index, mut port) in audio.output_ports().enumerate() {
                    if CASE == 21 {
                        port.set_constant_mask(clack_plugin::process::ConstantMask::from_bits(3));
                    }
                    let mut channels =
                        port.channels()?.into_f32().ok_or(PluginError::Message("f32 output"))?;
                    for samples in channels.iter_mut() {
                        samples.fill(if CASE == 19 && index == 2 {
                            f32::NAN
                        } else {
                            (index + 1) as f32 * 0.25
                        });
                    }
                }
                if CASE == 20 {
                    return Err(PluginError::Message("owned post-write process error"));
                }
                return Ok(ProcessStatus::Continue);
            }
            if audio.input_port_count() != self.0 .2 || audio.output_port_count() != 1 {
                return Err(PluginError::Message(
                    "fixture received the wrong native port counts",
                ));
            }
            let mut expected = if self.0 .1 {
                [[0.375; 4], [-0.125; 4]]
            } else {
                [[0.0; 4]; 2]
            };
            for (port_index, port) in audio.input_ports().enumerate() {
                let gain = [2.0, 3.0, 5.0][port_index];
                let channels =
                    port.channels()?.into_f32().ok_or(PluginError::Message("f32 input"))?;
                for (channel_index, samples) in channels.iter().enumerate() {
                    for (target, sample) in expected[channel_index].iter_mut().zip(samples.iter()) {
                        *target += sample * gain;
                    }
                }
            }
            let mut output = audio.output_ports().next().ok_or(PluginError::Message("output"))?;
            let mut channels =
                output.channels()?.into_f32().ok_or(PluginError::Message("f32 output"))?;
            for (channel_index, samples) in channels.iter_mut().enumerate() {
                for (target, sample) in samples.iter_mut().zip(expected[channel_index]) {
                    *target = sample;
                }
            }
            Ok(ProcessStatus::Continue)
        }
        fn deactivate(self, _main: &SidechainMain<'a, CASE>) {
            self.0 .0.store(false, Ordering::Relaxed);
            if CASE == 6 {
                LATENCY_DEACTIVATIONS.fetch_add(1, TestOrdering::SeqCst);
            }
        }
    }

    fn sidechain_request() -> IsolatedAudioProcessorWorkerPrepareRequest {
        IsolatedAudioProcessorWorkerPrepareRequest {
            musical_time_reference: None,
            auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
            note_input_contract: crate::AudioProcessorNoteInputContract::default(),
            native_lane: None,
            payload: Vec::new(),
            plugin_execution_contract: contract(),
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract {
                buses: [11, 7]
                    .map(|id| crate::AudioProcessorAuxiliaryInputBusContract {
                        bus_key: format!("clap.audio-input.{id}"),
                        channel_layout: AudioChannelLayout::Stereo,
                    })
                    .to_vec(),
            },
            parameter_ids: Vec::new(),
            render_contract: AudioRenderContract {
                sample_rate: 48_000,
                channel_layout: AudioChannelLayout::Stereo,
                max_block_frames: 4,
                processing_mode: AudioProcessingMode::Offline,
                processor_session_scratch_budget_bytes: 1024 * 1024,
                public_output_lookahead_budget_frames: 4096,
                compensation_delay_scratch_budget_bytes: 1024 * 1024,
            },
        }
    }

    #[test]
    fn clap_zero_input_uses_output_extent_and_never_passes_graph_main_as_native_input() {
        for layout in [AudioChannelLayout::Mono, AudioChannelLayout::Stereo] {
            let entry = if layout == AudioChannelLayout::Mono {
                PluginEntry::load_from_clack::<SinglePluginEntry<SidechainPlugin<8>>>(
                    c"/test/zero-input-mono",
                )
            } else {
                PluginEntry::load_from_clack::<SinglePluginEntry<SidechainPlugin<7>>>(
                    c"/test/zero-input-stereo",
                )
            }
            .unwrap();
            let mut request = sidechain_request();
            request.auxiliary_inputs = AudioProcessorAuxiliaryInputContract::default();
            request.render_contract.channel_layout = layout;
            let render = request.render_contract;
            let probe = super::super::clap_discovery::probe_loaded(
                &entry,
                "org.mondrian.test.sidechain",
                render,
                None,
                false,
            )
            .unwrap();
            assert_eq!(probe.main_audio_input, ClapMainAudioInput::Absent);
            assert!(probe.auxiliary_buses.is_empty());
            let mut worker = prepare_loaded_with_audio_input(
                request,
                entry,
                c"org.mondrian.test.sidechain",
                None,
                ClapMainAudioInput::Absent,
                &[],
                &[],
            )
            .unwrap();
            worker.enter_state(0).unwrap();
            let aux = AudioProcessorAuxiliaryInputContract::default();
            let channels = layout.channel_count();
            let mut start = 0;
            for frames in [4, 1, 2] {
                let mut graph_pcm = vec![19.0; frames * channels];
                worker
                    .process(IsolatedAudioProcessorWorkerBlock::new(
                        start,
                        frames,
                        render,
                        &mut graph_pcm,
                        &aux,
                        &[],
                        &[],
                        &[],
                    ))
                    .unwrap();
                for frame in graph_pcm.chunks_exact(channels) {
                    assert_eq!(frame[0].to_bits(), 0.375_f32.to_bits());
                    if channels == 2 {
                        assert_eq!(frame[1].to_bits(), (-0.125_f32).to_bits());
                    }
                }
                start += frames as i64;
            }
            // Extent and continuity checks precede any slicing of output storage.
            let mut oversized = vec![19.0; 5 * channels];
            assert!(matches!(
                worker.process(IsolatedAudioProcessorWorkerBlock::new(
                    start,
                    5,
                    render,
                    &mut oversized,
                    &aux,
                    &[],
                    &[],
                    &[]
                )),
                Err(AudioProcessorHostError::Process(_))
            ));
            assert!(oversized.iter().all(|sample| *sample == 19.0));
            let mut final_pcm = vec![19.0; channels];
            worker
                .process(IsolatedAudioProcessorWorkerBlock::new(
                    start,
                    1,
                    render,
                    &mut final_pcm,
                    &aux,
                    &[],
                    &[],
                    &[],
                ))
                .unwrap();
            assert_eq!(final_pcm[0], 0.375);
        }
    }

    fn raw_reprepare_payload() -> Vec<u8> {
        serde_json::to_vec(&ClapPayload {
            main_audio_input: ClapMainAudioInput::Present,
            main_audio_output_id: 0,
            auxiliary_outputs: Vec::new(),
            library_path: std::path::PathBuf::from("unchanged.clap"),
            plugin_id: "unchanged".to_owned(),
            binary_sha256: [7; 32],
            state: Some(vec![1, 2, 3]),
            parameters: Vec::new(),
            parameter_ids: Vec::new(),
        })
        .unwrap()
    }

    #[test]
    fn clap_reprepare_raw_codec_changes_only_state_and_preserves_metadata_fragments() {
        let payload = String::from_utf8(raw_reprepare_payload()).unwrap().replace(
            "\"parameters\":[]",
            "\"parameters\":[ {\"raw\": [1e99999,-0.0,1.2300]} ]",
        );
        // Synthetic scanner stress, not typed-admitted CLAP parameter metadata:
        // skipping raw numbers/containers must not decode their numeric values.
        let before: ClapRepreparePayload<'_> = serde_json::from_str(&payload).unwrap();
        let mut peak = 0;
        let output =
            payload_with_reprepare_configuration(payload.as_bytes(), &[255, 0, 9], &mut |bytes| {
                peak = peak.max(bytes)
            })
            .unwrap();
        let after: ClapRepreparePayload<'_> = serde_json::from_slice(&output).unwrap();
        assert_eq!(before.parameters.get(), after.parameters.get());
        assert_eq!(before.library_path.get(), after.library_path.get());
        assert_eq!(before.binary_sha256.get(), after.binary_sha256.get());
        assert_eq!(after.state.unwrap().get(), "[255,0,9]");
        assert!(peak >= reprepare_codec_bound(payload.len()).unwrap() + output.capacity());
        assert!(
            peak <= reprepare_codec_bound(payload.len()).unwrap()
                + reprepare_payload_output_bound(payload.len(), 3).unwrap()
        );
    }

    #[test]
    fn clap_reprepare_raw_codec_preserves_root_field_validation_and_default_state() {
        let payload = String::from_utf8(raw_reprepare_payload()).unwrap();
        for invalid in [
            payload.replacen("{", "{\"extra\":0,", 1),
            payload.replacen("{", "{\"plugin_\\u0069d\":\"duplicate\",", 1),
            payload.replace("\"main_audio_output_id\":0,", ""),
            "[]".to_owned(),
        ] {
            assert!(
                payload_with_reprepare_configuration(invalid.as_bytes(), &[8], &mut |_| {})
                    .is_err()
            );
        }
        let absent = payload.replace("\"state\":[1,2,3],", "");
        let output =
            payload_with_reprepare_configuration(absent.as_bytes(), &[8], &mut |_| {}).unwrap();
        let decoded: ClapPayload = serde_json::from_slice(&output).unwrap();
        assert_eq!(decoded.state.as_deref(), Some([8].as_slice()));
        assert!(reprepare_codec_bound(super::super::MAX_CONTROL_FRAME_BYTES / 2).is_some());
        assert!(reprepare_codec_bound(super::super::MAX_CONTROL_FRAME_BYTES / 2 + 1).is_none());
        assert!(reprepare_payload_output_bound(payload.len(), usize::MAX).is_none());
    }

    #[test]
    fn clap_payload_requires_explicit_shape_and_rejects_legacy_or_unknown_fields() {
        for shape in [ClapMainAudioInput::Absent, ClapMainAudioInput::Present] {
            let payload = ClapPayload {
                main_audio_output_id: 0,
                auxiliary_outputs: Vec::new(),
                main_audio_input: shape,
                library_path: std::env::current_exe().unwrap(),
                plugin_id: "fixture".to_owned(),
                binary_sha256: [7; 32],
                state: None,
                parameters: Vec::new(),
                parameter_ids: Vec::new(),
            };
            let value = serde_json::to_value(payload).unwrap();
            let parsed: ClapPayload = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(parsed.main_audio_input, shape);
            let mut legacy = value.clone();
            legacy.as_object_mut().unwrap().remove("main_audio_input");
            assert!(serde_json::from_value::<ClapPayload>(legacy).is_err());
            let mut unknown = value;
            unknown["extra"] = serde_json::json!(true);
            assert!(serde_json::from_value::<ClapPayload>(unknown).is_err());
            assert!(shape.validate(&AudioProcessorAuxiliaryInputContract::default()).is_ok());
        }
        assert!(frozen_legacy_main_input_validation(
            ClapMainAudioInput::Absent,
            &sidechain_request().auxiliary_inputs,
        )
        .is_err());
        assert!(ClapMainAudioInput::Present
            .validate(&sidechain_request().auxiliary_inputs)
            .is_ok());
    }

    // Source-only mixed-version control: this is the former validator, not an
    // assertion that an old Product binary has executed during this unit test.
    fn frozen_legacy_main_input_validation(
        shape: ClapMainAudioInput,
        auxiliary: &AudioProcessorAuxiliaryInputContract,
    ) -> Result<(), AudioProcessorHostError> {
        if shape == ClapMainAudioInput::Absent && !auxiliary.buses.is_empty() {
            return Err(invalid(
                "CLAP absent main input cannot have auxiliary inputs",
            ));
        }
        Ok(())
    }

    pub(super) fn output_metadata_entry<const CASE: u8>() -> PluginEntry {
        PluginEntry::load_from_clack::<SinglePluginEntry<SidechainPlugin<CASE>>>(
            c"/test/output-shape",
        )
        .unwrap()
    }

    #[test]
    fn clap_auxiliary_only_abi_preserves_native_zero_index_order_and_ignores_graph_main() {
        for layout in [AudioChannelLayout::Mono, AudioChannelLayout::Stereo] {
            let entry = if layout == AudioChannelLayout::Mono {
                PluginEntry::load_from_clack::<SinglePluginEntry<SidechainPlugin<12>>>(
                    c"/test/aux-only-mono",
                )
            } else {
                PluginEntry::load_from_clack::<SinglePluginEntry<SidechainPlugin<9>>>(
                    c"/test/aux-only-stereo",
                )
            }
            .expect("auxiliary-only entry");
            let mut request = sidechain_request();
            request.render_contract.channel_layout = layout;
            for bus in &mut request.auxiliary_inputs.buses {
                bus.channel_layout = layout;
            }
            let render = request.render_contract();
            let auxiliary = request.auxiliary_inputs.clone();
            let probe = super::super::clap_discovery::probe_loaded(
                &entry,
                "org.mondrian.test.sidechain",
                render,
                None,
                false,
            )
            .expect("optional Main is discovered from actual flags");
            assert_eq!(probe.main_audio_input, ClapMainAudioInput::Absent);
            assert_eq!(
                super::super::protocol::auxiliary_from_wire(probe.auxiliary_buses),
                auxiliary
            );
            assert!(
                frozen_legacy_main_input_validation(ClapMainAudioInput::Absent, &auxiliary)
                    .is_err()
            );
            let mut worker = prepare_loaded_with_audio_input(
                request,
                entry,
                c"org.mondrian.test.sidechain",
                None,
                ClapMainAudioInput::Absent,
                &[],
                &[],
            )
            .expect("prepare the same ordered auxiliary-only contract");
            assert_eq!(worker.auxiliary_input_contract(), auxiliary);
            assert_eq!(
                worker
                    .auxiliary_input_names()
                    .iter()
                    .map(|name| name.bus_key.as_str())
                    .collect::<Vec<_>>(),
                vec!["clap.audio-input.11", "clap.audio-input.7"]
            );
            worker.enter_state(12).expect("actual Enter");
            let (mut main, aux, expected) = if layout == AudioChannelLayout::Mono {
                (
                    vec![99.0, -101.0],
                    vec![1.0, 2.0, -1.0, -2.0],
                    vec![-1.0, -2.0],
                )
            } else {
                (
                    vec![99.0, -101.0, 103.0, -107.0],
                    vec![1.0, 2.0, 3.0, 4.0, -1.0, -2.0, -3.0, -4.0],
                    vec![-1.0, -2.0, -3.0, -4.0],
                )
            };
            worker
                .process(IsolatedAudioProcessorWorkerBlock::new(
                    12,
                    2,
                    render,
                    &mut main,
                    &auxiliary,
                    &aux,
                    &[],
                    &[],
                ))
                .expect("actual native input0 and input1 are the two auxiliary buses");
            assert_eq!(
                main, expected,
                "the independent native port gains are 2 and 3, not a graph-Main mix"
            );
            let channels = layout.channel_count();
            let mut main = vec![113.0; channels];
            worker
                .process(IsolatedAudioProcessorWorkerBlock::new(
                    14,
                    1,
                    render,
                    &mut main,
                    &auxiliary,
                    &vec![0.0; channels * 2],
                    &[],
                    &[],
                ))
                .expect("disconnected auxiliary silence overwrites the preceding signal");
            assert_eq!(main, vec![0.0; channels]);
            let mut main = vec![127.0; channels];
            assert!(worker
                .process(IsolatedAudioProcessorWorkerBlock::new(
                    15,
                    1,
                    render,
                    &mut main,
                    &auxiliary,
                    &[],
                    &[],
                    &[],
                ))
                .is_err());
            assert_eq!(
                main,
                vec![127.0; channels],
                "missing auxiliary PCM is never substituted with graph Main"
            );
        }
    }

    #[test]
    fn clap_auxiliary_only_admission_refuses_changed_contract_capacity_and_multiple_outputs() {
        for changed in [false, true] {
            let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SidechainPlugin<9>>>(
                c"/test/aux-only-changed",
            )
            .unwrap();
            let mut request = sidechain_request();
            if changed {
                request.auxiliary_inputs.buses.reverse();
            } else {
                request.auxiliary_inputs.buses.clear();
            }
            assert!(matches!(prepare_loaded_with_audio_input(
                request, entry, c"org.mondrian.test.sidechain", None, ClapMainAudioInput::Absent, &[], &[],
            ), Err(AudioProcessorHostError::InvalidContract(detail)) if detail == "CLAP audio ports changed since contract probe"));
        }
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SidechainPlugin<10>>>(
            c"/test/aux-only-32",
        )
        .unwrap();
        let request = sidechain_request();
        let error = super::super::clap_discovery::probe_loaded(
            &entry,
            "org.mondrian.test.sidechain",
            request.render_contract(),
            None,
            false,
        );
        assert!(
            matches!(error, Err(AudioProcessorHostError::InvalidContract(detail)) if detail == "CLAP auxiliary inputs exceed the existing 31-port bound")
        );
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SidechainPlugin<11>>>(
            c"/test/aux-only-three-outputs",
        )
        .unwrap();
        let probe = super::super::clap_discovery::probe_loaded(
            &entry,
            "org.mondrian.test.sidechain",
            request.render_contract(),
            None,
            false,
        )
        .unwrap();
        assert_eq!(probe.main_audio_output_id, 0);
        assert_eq!(
            super::super::protocol::outputs_from_wire(probe.auxiliary_outputs)
                .buses
                .iter()
                .map(|bus| bus.bus_key.as_str())
                .collect::<Vec<_>>(),
            vec!["clap.audio-output.11", "clap.audio-output.7"]
        );
        assert!(
            matches!(prepare_loaded_with_audio_input(request, entry, c"org.mondrian.test.sidechain", None, ClapMainAudioInput::Absent, &[], &[]),
            Err(AudioProcessorHostError::InvalidContract(detail)) if detail == "CLAP audio ports changed since contract probe")
        );
    }

    #[test]
    fn clap_optional_main_shape_keeps_legacy_rejection_and_factory_preload_capacity_guard() {
        let auxiliary = AudioProcessorAuxiliaryInputContract {
            buses: (0..32)
                .map(|id| crate::AudioProcessorAuxiliaryInputBusContract {
                    bus_key: format!("clap.audio-input.{id}"),
                    channel_layout: AudioChannelLayout::Stereo,
                })
                .collect(),
        };
        let admitted =
            AudioProcessorAuxiliaryInputContract { buses: auxiliary.buses[..31].to_vec() };
        assert!(ClapMainAudioInput::Absent.validate(&admitted).is_ok());
        assert!(ClapMainAudioInput::Present.validate(&admitted).is_ok());
        assert!(
            frozen_legacy_main_input_validation(ClapMainAudioInput::Absent, &admitted).is_err()
        );
        assert!(frozen_legacy_main_input_validation(
            ClapMainAudioInput::Absent,
            &Default::default()
        )
        .is_ok());
        assert!(
            frozen_legacy_main_input_validation(ClapMainAudioInput::Present, &admitted).is_ok()
        );
        let directory = tempfile::tempdir().unwrap();
        let nonexistent_library = directory.path().join("must-not-load.clap");
        assert!(!nonexistent_library.exists());
        for shape in [ClapMainAudioInput::Absent, ClapMainAudioInput::Present] {
            assert!(shape.validate(&auxiliary).is_err());
            let mut request = sidechain_request();
            request.native_lane = clap_native_worker_lane_profile().unwrap();
            request.auxiliary_inputs = auxiliary.clone();
            request.payload = serde_json::to_vec(&ClapPayload {
                main_audio_output_id: 0,
                auxiliary_outputs: Vec::new(),
                main_audio_input: shape,
                library_path: nonexistent_library.clone(),
                plugin_id: "org.mondrian.test.sidechain".to_owned(),
                binary_sha256: [7; 32],
                state: None,
                parameters: Vec::new(),
                parameter_ids: Vec::new(),
            })
            .unwrap();
            assert!(
                matches!(ClapAudioProcessorWorkerFactory.prepare(request),
                Err(AudioProcessorHostError::InvalidContract(detail)) if detail == "CLAP auxiliary inputs exceed the existing 31-port bound"),
                "capacity rejection precedes fingerprinting/loading the nonexistent library"
            );
            assert!(ClapAudioProcessorSpecResolver::new(
                std::env::current_exe().unwrap(),
                [ClapPluginRegistration {
                    auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
                    main_audio_output_id: 0,
                    plugin_id: "org.mondrian.test.sidechain".to_owned(),
                    library_path: nonexistent_library.clone(),
                    binary_sha256: [7; 32],
                    execution_contract: contract(),
                    parameters: Vec::new(),
                    main_audio_input: shape,
                    auxiliary_inputs: auxiliary.clone(),
                }]
            )
            .is_err());
        }
    }

    #[test]
    fn clap_configuration_refuses_both_inactive_main_input_shape_changes_before_reactivation() {
        for case in [9, 10] {
            LIFECYCLE_AUDITS[case].clear();
            let (entry, admitted) = if case == 9 {
                (
                    PluginEntry::load_from_clack::<SinglePluginEntry<LifecyclePlugin<9>>>(
                        c"/test/absent-to-present",
                    )
                    .unwrap(),
                    ClapMainAudioInput::Absent,
                )
            } else {
                (
                    PluginEntry::load_from_clack::<SinglePluginEntry<LifecyclePlugin<10>>>(
                        c"/test/present-to-absent",
                    )
                    .unwrap(),
                    ClapMainAudioInput::Present,
                )
            };
            let mut request = sidechain_request();
            request.auxiliary_inputs = AudioProcessorAuxiliaryInputContract::default();
            request.render_contract.max_block_frames = 16;
            request.plugin_execution_contract = AudioProcessorExecutionContract::new(
                5,
                AudioProcessorTail::Finite(5),
                true,
                true,
                true,
                0,
            )
            .unwrap();
            let mut worker = prepare_loaded_with_audio_input(
                request,
                entry,
                c"org.mondrian.test.render-lifecycle",
                None,
                admitted,
                &[],
                &[],
            )
            .unwrap();
            let cohort = worker.native_configuration_cohort().unwrap();
            assert!(matches!(
                worker
                    .service_native_configuration(super::super::NativeConfigurationControlRequest {
                        expected_cohort: cohort,
                        control_token: std::num::NonZeroU64::new(73).unwrap(),
                        canonical_constant_values: &[],
                        configuration_limit_bytes: 64,
                        control_buffer_grant_bytes: 128 * 1024,
                    })
                    .unwrap(),
                super::super::NativeConfigurationControlOutcome::CapabilityRefused(
                    super::super::NativeConfigurationRefusal::MetadataChanged
                )
            ));
            assert_eq!(
                worker.native_configuration_cohort(),
                Some(cohort),
                "refusal must not consume the actual request"
            );
            #[cfg(windows)]
            {
                let object = worker.native_audio_object_observation().unwrap();
                assert!(matches!(worker.split_native_processing(),
                    Err(AudioProcessorHostError::Unavailable(detail))
                        if detail == "CLAP processor is closed"));
                {
                    let mut main = worker
                        .native_control_only_role()
                        .unwrap()
                        .expect("stopped refused owner retains its real main role");
                    main.service_native_main_thread().unwrap();
                    assert_eq!(main.native_configuration_cohort().unwrap(), Some(cohort));
                    assert_eq!(
                        main.pending_reprepare().unwrap(),
                        Some(crate::NativeObservedRequest::Cohort(cohort))
                    );
                    assert_eq!(main.preserve_native_observations().unwrap().object, object);
                }
                assert_eq!(worker.native_audio_object_observation().unwrap(), object);
                assert_eq!(worker.native_configuration_cohort(), Some(cohort));
                assert_eq!(
                    worker.pending_reprepare(),
                    Some(crate::NativeObservedRequest::Cohort(cohort))
                );
            }
            assert_eq!(
                LIFECYCLE_AUDITS[case].activations.load(TestOrdering::SeqCst),
                1,
                "shape mismatch must refuse before a second activation"
            );
            assert_eq!(
                LIFECYCLE_AUDITS[case].active_port_queries.load(TestOrdering::SeqCst),
                0
            );
            drop(worker);
            assert_eq!(
                LIFECYCLE_AUDITS[case].failures.load(TestOrdering::SeqCst),
                0
            );
        }
    }

    #[test]
    fn clap_activation_latency_probe_admission_and_cleanup_follow_sdk_lifecycle() {
        let before = LATENCY_DEACTIVATIONS.load(TestOrdering::SeqCst);
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SidechainPlugin<6>>>(
            c"/test/activation-latency",
        )
        .expect("activation-dependent latency fixture");
        let probe = super::super::clap_discovery::probe_loaded(
            &entry,
            "org.mondrian.test.sidechain",
            AudioRenderContract {
                sample_rate: 48_000,
                channel_layout: AudioChannelLayout::Stereo,
                max_block_frames: 4,
                processing_mode: AudioProcessingMode::Offline,
                processor_session_scratch_budget_bytes: usize::MAX,
                public_output_lookahead_budget_frames: usize::MAX,
                compensation_delay_scratch_budget_bytes: usize::MAX,
            },
            None,
            false,
        )
        .expect("probe activated latency");
        assert_eq!(
            probe.latency_frames, 3,
            "latency must be queried while activated"
        );
        assert_eq!(LATENCY_DEACTIVATIONS.load(TestOrdering::SeqCst), before + 1);
        let mut request = sidechain_request();
        request.plugin_execution_contract =
            AudioProcessorExecutionContract::new(3, AudioProcessorTail::None, true, true, true, 0)
                .expect("activated latency contract");
        let worker = prepare_loaded(
            request,
            entry,
            c"org.mondrian.test.sidechain",
            None,
            &[],
            &[],
        )
        .expect("admit actual activated latency");
        assert_eq!(worker.execution_contract().algorithmic_latency_frames(), 3);
        drop(worker);
        assert_eq!(LATENCY_DEACTIVATIONS.load(TestOrdering::SeqCst), before + 2);
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SidechainPlugin<6>>>(
            c"/test/activation-latency",
        )
        .expect("fresh lifecycle fixture");
        assert!(
            matches!(prepare_loaded(sidechain_request(), entry, c"org.mondrian.test.sidechain", None, &[], &[]),
            Err(AudioProcessorHostError::InvalidContract(detail)) if detail.contains("latency"))
        );
        assert_eq!(
            LATENCY_DEACTIVATIONS.load(TestOrdering::SeqCst),
            before + 3,
            "stale latency refusal deactivates the prepared instance"
        );
    }

    #[test]
    fn clap_sidechain_abi_preserves_port_ids_order_pcm_and_silence() {
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SidechainPlugin<0>>>(
            c"/test/sidechain",
        )
        .expect("sidechain entry");
        let request = sidechain_request();
        let render = request.render_contract();
        let auxiliary = request.auxiliary_inputs().clone();
        let probe = super::super::clap_discovery::probe_loaded(
            &entry,
            "org.mondrian.test.sidechain",
            AudioRenderContract {
                sample_rate: 48_000,
                channel_layout: AudioChannelLayout::Stereo,
                max_block_frames: 4,
                processing_mode: AudioProcessingMode::Offline,
                processor_session_scratch_budget_bytes: usize::MAX,
                public_output_lookahead_budget_frames: usize::MAX,
                compensation_delay_scratch_budget_bytes: usize::MAX,
            },
            None,
            false,
        )
        .expect("sidechain discovery");
        assert_eq!(
            super::super::protocol::auxiliary_from_wire(probe.auxiliary_buses),
            auxiliary
        );
        let mut worker = prepare_loaded(
            request,
            entry,
            c"org.mondrian.test.sidechain",
            None,
            &[],
            &[],
        )
        .expect("prepare sidechains");
        assert_eq!(worker.auxiliary_input_contract(), auxiliary);
        worker.enter_state(12).expect("enter state");
        let mut main = [0.25, -0.5, 1.0, 0.125];
        let aux = [1.0, 2.0, 3.0, 4.0, -1.0, -2.0, -3.0, -4.0];
        worker
            .process(IsolatedAudioProcessorWorkerBlock::new(
                12,
                2,
                render,
                &mut main,
                &auxiliary,
                &aux,
                &[],
                &[],
            ))
            .expect("process distinct sidechain PCM");
        assert_eq!(main, [-1.5, -5.0, -4.0, -7.75]);
        let mut main = [0.75, -0.25];
        worker
            .process(IsolatedAudioProcessorWorkerBlock::new(
                14,
                1,
                render,
                &mut main,
                &auxiliary,
                &[0.0; 4],
                &[],
                &[],
            ))
            .expect("silent disconnected buses overwrite prior samples");
        assert_eq!(main, [1.5, -0.5]);
        let mut main = [0.25, -0.5];
        assert!(worker
            .process(IsolatedAudioProcessorWorkerBlock::new(
                15,
                1,
                render,
                &mut main,
                &auxiliary,
                &[],
                &[],
                &[],
            ))
            .is_err());
        assert_eq!(main, [0.25, -0.5]);
    }

    #[test]
    fn clap_mixed_input_storage_matches_actual_plane_and_sdk_owners() {
        let request = sidechain_request();
        let render = request.render_contract;
        let mut inputs = request.auxiliary_inputs;
        inputs.buses[0].channel_layout = AudioChannelLayout::Mono;
        let outputs = AudioProcessorAuxiliaryOutputContract::default();
        for (main, widths) in [
            (ClapMainAudioInput::Present, vec![2, 1, 2]),
            (ClapMainAudioInput::Absent, vec![1, 2]),
        ] {
            let input =
                widths.iter().map(|&width| vec![vec![0.0_f32; 4]; width]).collect::<Vec<_>>();
            let output = vec![vec![vec![0.0_f32; 4]; 2]];
            let channels = widths.iter().sum::<usize>();
            let input_ports = AudioPorts::with_capacity(channels, widths.len());
            let output_ports = AudioPorts::with_capacity(2, 1);
            assert_eq!(
                clap_input_audio_shape(render.channel_layout, main, &inputs).unwrap(),
                (widths.len(), channels)
            );
            let actual = clap_actual_audio_storage(
                &input,
                &output,
                input.capacity() + output.capacity(),
                &input_ports,
                &output_ports,
                &outputs,
            )
            .unwrap();
            let declared = clap_audio_storage_reservation(render, main, &inputs, &outputs).unwrap();
            assert_eq!(actual, declared);
        }
    }

    #[test]
    fn clap_mixed_input_abi_preserves_each_layout_with_and_without_main() {
        check_mixed_input_abi::<3>(ClapMainAudioInput::Present);
        check_mixed_input_abi::<23>(ClapMainAudioInput::Absent);
    }

    fn check_mixed_input_abi<const CASE: u8>(main_input: ClapMainAudioInput) {
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SidechainPlugin<CASE>>>(
            c"/test/mixed-inputs",
        )
        .unwrap();
        let mut request = sidechain_request();
        request.auxiliary_inputs.buses[0].channel_layout = AudioChannelLayout::Mono;
        if main_input == ClapMainAudioInput::Present {
            request.auxiliary_inputs.buses[1].channel_layout = AudioChannelLayout::Mono;
        }
        let render = request.render_contract;
        let auxiliary = request.auxiliary_inputs.clone();
        let probe = super::super::clap_discovery::probe_loaded(
            &entry,
            "org.mondrian.test.sidechain",
            render,
            None,
            false,
        )
        .unwrap();
        assert_eq!(probe.main_audio_input, main_input);
        assert_eq!(
            super::super::protocol::auxiliary_from_wire(probe.auxiliary_buses),
            auxiliary
        );
        // Same counts/keys cannot authorize a stale per-port layout.
        assert!(prepare_loaded_with_audio_input(
            sidechain_request(),
            entry.clone(),
            c"org.mondrian.test.sidechain",
            None,
            main_input,
            &[],
            &[]
        )
        .is_err());
        let mut worker = prepare_loaded_with_audio_input(
            request,
            entry,
            c"org.mondrian.test.sidechain",
            None,
            main_input,
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(worker.auxiliary_input_contract(), auxiliary);
        worker.enter_state(12).unwrap();
        let mut main = [0.25, -0.5, 1.0, 0.125];
        let aux = if main_input == ClapMainAudioInput::Present {
            vec![1.0, 2.0, 3.0, 4.0]
        } else {
            vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]
        };
        worker
            .process(IsolatedAudioProcessorWorkerBlock::new(
                12,
                2,
                render,
                &mut main,
                &auxiliary,
                &aux,
                &[],
                &[],
            ))
            .unwrap();
        assert_eq!(
            main,
            if main_input == ClapMainAudioInput::Present {
                [18.5, -1.0, 28.0, 0.25]
            } else {
                [11.0, 12.0, 19.0, 18.0]
            }
        );
        let mut main = [0.75, -0.25];
        let silent = vec![
            0.0;
            if main_input == ClapMainAudioInput::Present {
                2
            } else {
                3
            }
        ];
        worker
            .process(IsolatedAudioProcessorWorkerBlock::new(
                14,
                1,
                render,
                &mut main,
                &auxiliary,
                &silent,
                &[],
                &[],
            ))
            .unwrap();
        assert_eq!(
            main,
            if main_input == ClapMainAudioInput::Present {
                [1.5, -0.5]
            } else {
                [0.0; 2]
            }
        );
        let mut main = [0.25, -0.5];
        assert!(worker
            .process(IsolatedAudioProcessorWorkerBlock::new(
                15,
                1,
                render,
                &mut main,
                &auxiliary,
                &[],
                &[],
                &[]
            ))
            .is_err());
        assert_eq!(main, [0.25, -0.5]);
    }

    fn refuse_sidechain_ports<const CASE: u8>() {
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SidechainPlugin<CASE>>>(
            c"/test/sidechain",
        )
        .expect("sidechain entry");
        assert!(super::super::clap_discovery::probe_loaded(
            &entry,
            "org.mondrian.test.sidechain",
            AudioRenderContract {
                sample_rate: 48_000,
                channel_layout: AudioChannelLayout::Stereo,
                max_block_frames: 4,
                processing_mode: AudioProcessingMode::Offline,
                processor_session_scratch_budget_bytes: usize::MAX,
                public_output_lookahead_budget_frames: usize::MAX,
                compensation_delay_scratch_budget_bytes: usize::MAX
            },
            None,
            false,
        )
        .is_err());
        assert!(prepare_loaded(
            sidechain_request(),
            entry,
            c"org.mondrian.test.sidechain",
            None,
            &[],
            &[]
        )
        .is_err());
    }

    #[test]
    fn clap_sidechain_abi_refuses_duplicate_main_layout_count_and_missing_metadata() {
        refuse_sidechain_ports::<1>();
        refuse_sidechain_ports::<2>();
        refuse_sidechain_ports::<22>();
        refuse_sidechain_ports::<4>();
        refuse_sidechain_ports::<5>();
    }

    #[test]
    fn clap_sidechain_abi_refuses_changed_order_and_unadmitted_inputs() {
        for changed in [false, true] {
            let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SidechainPlugin<0>>>(
                c"/test/sidechain",
            )
            .expect("sidechain entry");
            let mut request = sidechain_request();
            if changed {
                request.auxiliary_inputs.buses.reverse();
            } else {
                request.auxiliary_inputs.buses.clear();
            }
            assert!(prepare_loaded(
                request,
                entry,
                c"org.mondrian.test.sidechain",
                None,
                &[],
                &[]
            )
            .is_err());
        }
    }

    fn contract() -> AudioProcessorExecutionContract {
        AudioProcessorExecutionContract::new(0, AudioProcessorTail::None, true, true, true, 0)
            .expect("valid CLAP contract")
    }

    #[test]
    fn probed_parameter_names_and_visibility_survive_author_instance_round_trip() {
        let descriptor =
            |id, name: &str, hidden, read_only| super::super::ClapParameterDescriptor {
                id,
                name: name.to_owned(),
                min_value: 0.0,
                max_value: 1.0,
                default_value: 0.5,
                current_value: 0.25,
                automatable: true,
                stepped: false,
                enumeration: false,
                read_only,
                hidden,
            };
        let registration = ClapPluginRegistration {
            auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
            main_audio_output_id: 0,
            main_audio_input: ClapMainAudioInput::Present,
            plugin_id: "org.example.gain".to_owned(),
            library_path: std::env::current_exe().expect("test executable path"),
            binary_sha256: [3; 32],
            execution_contract: contract(),
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameters: vec![
                descriptor(1, "Output Gain", false, false),
                super::super::ClapParameterDescriptor {
                    automatable: false,
                    ..descriptor(2, "Private meter", true, false)
                },
                descriptor(3, "Read only", false, true),
            ],
        };
        for opaque_state in [Some(vec![7, 8]), Some(Vec::new()), None] {
            let mut instance =
                author_instance_from_probe(registration.clone(), opaque_state.clone())
                    .expect("authored CLAP instance");
            assert_eq!(
                instance.opaque_state.as_deref().map(Vec::as_slice),
                opaque_state.as_deref()
            );
            assert_eq!(instance.parameters.len(), 2);
            let visible_id = mondrian_core::ParameterId::new("clap.param.1").unwrap();
            let hidden_id = mondrian_core::ParameterId::new("clap.param.2").unwrap();
            let visible = &instance.parameters[&visible_id];
            assert_eq!(visible.ui.display_name.as_deref(), Some("Output Gain"));
            assert!(!visible.ui.hidden);
            let hidden = &instance.parameters[&hidden_id];
            assert_eq!(hidden.ui.display_name.as_deref(), Some("Private meter"));
            assert!(hidden.ui.hidden);
            assert!(
                !hidden.schema.is_animatable,
                "nonautomatable writable is retained"
            );
            for (index, id) in [&visible_id, &hidden_id].into_iter().enumerate() {
                let parameter = &instance.parameters[id];
                assert_eq!(
                    parameter.schema,
                    registration.parameters[index].authoring_schema().unwrap()
                );
                if opaque_state.is_some() {
                    assert!(parameter.automation.is_none());
                } else {
                    assert_eq!(
                        parameter.automation.as_ref().unwrap().default_value,
                        0.25,
                        "stateless fallback retains current rather than default 0.5"
                    );
                }
            }
            let reopened: AudioProcessorInstance = serde_json::from_slice(
                &serde_json::to_vec(&instance).expect("serialize CLAP instance"),
            )
            .expect("reopen CLAP instance");
            assert_eq!(reopened, instance);
            instance
                .parameters
                .get_mut(&visible_id)
                .unwrap()
                .set_automation(ExactAutomationCurve::new(visible_id.clone(), 0.875).unwrap())
                .unwrap();
            let reopened: AudioProcessorInstance =
                serde_json::from_slice(&serde_json::to_vec(&instance).unwrap()).unwrap();
            assert_eq!(reopened, instance, "explicit Some survives save/load");
            assert_eq!(
                reopened.parameters[&visible_id].automation.as_ref().unwrap().default_value,
                0.875
            );
        }
    }

    #[test]
    fn catalog_native_state_does_not_bypass_current_value_validation() {
        for opaque_state in [Some(Vec::new()), Some(vec![7, 8]), None] {
            let registration = ClapPluginRegistration {
                auxiliary_outputs: Default::default(),
                main_audio_output_id: 0,
                main_audio_input: ClapMainAudioInput::Present,
                plugin_id: "org.example.gain".to_owned(),
                library_path: std::env::current_exe().unwrap(),
                binary_sha256: [3; 32],
                execution_contract: contract(),
                auxiliary_inputs: Default::default(),
                parameters: vec![super::super::ClapParameterDescriptor {
                    id: 1,
                    name: "Output Gain".to_owned(),
                    min_value: 0.0,
                    max_value: 1.0,
                    default_value: 0.5,
                    current_value: f64::NAN,
                    automatable: true,
                    stepped: false,
                    enumeration: false,
                    read_only: false,
                    hidden: false,
                }],
            };
            assert!(author_instance_from_probe(registration, opaque_state).is_err());
        }
    }

    #[test]
    fn registration_rejects_duplicate_identity_and_relative_paths() {
        let helper = std::env::current_exe().expect("test executable path");
        let registration = ClapPluginRegistration {
            auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
            main_audio_output_id: 0,
            main_audio_input: ClapMainAudioInput::Present,
            plugin_id: "org.example.gain".to_owned(),
            library_path: helper.clone(),
            binary_sha256: [1; 32],
            execution_contract: contract(),
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameters: Vec::new(),
        };
        assert!(ClapAudioProcessorSpecResolver::new(
            helper.clone(),
            [registration.clone(), registration.clone()],
        )
        .is_err());
        assert!(ClapAudioProcessorSpecResolver::new(
            helper,
            [ClapPluginRegistration {
                auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
                main_audio_output_id: 0,
                library_path: PathBuf::from("gain.clap"),
                ..registration
            }],
        )
        .is_err());
    }

    #[test]
    fn authored_clap_binary_revision_is_required_and_matches_before_worker_admission() {
        use crate::{
            AudioProcessorInsertionPoint, AudioProcessorOccurrence, AudioProcessorOccurrenceOwner,
        };
        use mondrian_core::{AudioProcessorInstanceId, ProgramOutputId};

        let helper = std::env::current_exe().expect("test executable path");
        let auxiliary = sidechain_request().auxiliary_inputs;
        let resolver = ClapAudioProcessorSpecResolver::new(
            helper.clone(),
            [ClapPluginRegistration {
                auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
                main_audio_output_id: 0,
                main_audio_input: ClapMainAudioInput::Present,
                plugin_id: "org.example.gain".to_owned(),
                library_path: helper,
                binary_sha256: [7; 32],
                execution_contract: contract(),
                auxiliary_inputs: auxiliary.clone(),
                parameters: Vec::new(),
            }],
        )
        .expect("reference registration");
        let parameters = BTreeMap::new();
        let render_contract = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 512,
            processing_mode: AudioProcessingMode::Realtime,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 4096,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        let occurrence = AudioProcessorOccurrence {
            instance_id: AudioProcessorInstanceId::new(),
            owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
            insertion: AudioProcessorInsertionPoint::PreFader,
        };
        let resolve = |hash| {
            let definition = AudioProcessorDefinitionRef::Clap {
                plugin_id: "org.example.gain".to_owned(),
                schema_version: 1,
                binary_sha256: hash,
            };
            resolver.resolve(AudioProcessorPrepareRequest::new(
                occurrence,
                &definition,
                &parameters,
                None,
                render_contract,
            ))
        };
        assert!(matches!(
            resolve(None),
            Err(AudioProcessorHostError::Unavailable(message)) if message.contains("no authored binary revision")
        ));
        assert!(matches!(
            resolve(Some([8; 32])),
            Err(AudioProcessorHostError::Unavailable(message)) if message.contains("binary revision differs")
        ));
        assert_eq!(
            resolve(Some([7; 32])).expect("admitted binary and ports").auxiliary_inputs,
            auxiliary
        );
    }

    #[test]
    fn installed_catalog_preserves_empty_snapshot_after_failed_scan() {
        let helper = std::env::current_exe().expect("test executable path");
        let catalog =
            InstalledClapAudioProcessorSpecResolver::new(helper).expect("empty installed catalog");
        let temp = tempfile::tempdir().expect("temporary plugin directory");
        let invalid = temp.path().join("invalid.clap");
        std::fs::write(&invalid, b"not a native plugin").expect("write invalid plugin");
        assert!(catalog.install_library(invalid).is_err());
        assert!(catalog.descriptors().expect("catalog remains readable").is_empty());
    }

    #[test]
    #[ignore = "requires a built Mondrian executable and the Clack gain reference DLL"]
    fn installed_catalog_atomically_reselects_and_rejects_duplicate_plugin_ids() {
        let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_HELPER to mondrian executable");
        let reference = std::env::var_os("MONDRIAN_CLAP_TEST_PLUGIN")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_PLUGIN to Clack gain DLL");
        let temp = tempfile::tempdir().expect("temporary plugin directory");
        let copy = temp.path().join("gain-copy.dll");
        std::fs::copy(&reference, &copy).expect("copy reference plugin");
        let catalog =
            InstalledClapAudioProcessorSpecResolver::new(helper).expect("empty installed catalog");
        assert_eq!(
            catalog.install_library(copy.clone()).expect("first installation").len(),
            1
        );
        assert_eq!(
            catalog.install_library(copy).expect("same-path reselection").len(),
            1
        );
        let before = catalog.descriptors().expect("installed descriptors");
        assert!(catalog.install_library(reference).is_err());
        assert_eq!(
            catalog.descriptors().expect("unchanged descriptors"),
            before
        );
    }

    fn parameter_site_descriptor() -> super::super::ClapParameterDescriptor {
        super::super::ClapParameterDescriptor {
            id: 1,
            name: "Gain".to_owned(),
            min_value: 0.0,
            max_value: 1.0,
            default_value: 1.0,
            current_value: 1.0,
            automatable: true,
            stepped: false,
            enumeration: false,
            read_only: false,
            hidden: false,
        }
    }

    fn parameter_site_request() -> IsolatedAudioProcessorWorkerPrepareRequest {
        let mut request = sidechain_request();
        request.auxiliary_inputs = AudioProcessorAuxiliaryInputContract::default();
        request.parameter_ids = vec![
            mondrian_core::ParameterId::new("clap.param.1").expect("owned parameter identity")
        ];
        request
    }

    fn parameter_site_error(
        result: Result<Box<dyn IsolatedAudioProcessorWorker>, AudioProcessorHostError>,
    ) -> String {
        match result {
            Err(AudioProcessorHostError::InvalidContract(detail)) => detail,
            Err(other) => panic!("expected InvalidContract, received {other:?}"),
            Ok(_) => panic!("invalid parameter contract was admitted"),
        }
    }

    fn parameter_site_decoded_request(
        parameters: Vec<super::super::ClapParameterDescriptor>,
        library_path: PathBuf,
    ) -> IsolatedAudioProcessorWorkerPrepareRequest {
        let mut request = parameter_site_request();
        request.native_lane =
            clap_native_worker_lane_profile().expect("actual native lane profile");
        request.payload = serde_json::to_vec(&ClapPayload {
            main_audio_output_id: 0,
            auxiliary_outputs: Vec::new(),
            main_audio_input: ClapMainAudioInput::Present,
            library_path,
            plugin_id: "org.mondrian.test.automated-gain".to_owned(),
            binary_sha256: [0; 32],
            state: None,
            parameters,
            parameter_ids: vec![1],
        })
        .expect("finite owned payload");
        request
    }

    #[test]
    fn parameter_refusal_site_decoded_payload_is_classified_before_library_loading() {
        let directory = tempfile::tempdir().expect("temporary missing-library parent");
        let missing_library = directory.path().join("never-created.clap");
        assert!(!missing_library.exists());
        let mut descriptor = parameter_site_descriptor();
        descriptor.default_value = 2.0;
        let request = parameter_site_decoded_request(vec![descriptor], missing_library);
        assert_eq!(
            parameter_site_error(ClapAudioProcessorWorkerFactory.prepare(request)),
            "CLAP parameter metadata is invalid or ambiguous: index=0 native_id=1 reason=default_out_of_range min_bits=0000000000000000 max_bits=3FF0000000000000 default_bits=4000000000000000 current_bits=3FF0000000000000 validation_site=decoded_payload"
        );
    }

    #[test]
    fn parameter_refusal_site_expected_output_contract_uses_existing_loaded_consumer() {
        // The public factory already validated this same immutable slice. This
        // existing test seam checks the real redundant output consumer directly;
        // it does not imply that a normal factory can legally reach this refusal.
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<AutomatedGainPlugin>>(
            c"/test/parameter-site-output",
        )
        .expect("existing owned ABI entry");
        let mut descriptor = parameter_site_descriptor();
        descriptor.default_value = 2.0;
        assert_eq!(parameter_site_error(prepare_loaded(
            parameter_site_request(), entry, c"org.mondrian.test.automated-gain",
            None, &[descriptor], &[1],
        )),
            "CLAP parameter metadata is invalid or ambiguous: index=0 native_id=1 reason=default_out_of_range min_bits=0000000000000000 max_bits=3FF0000000000000 default_bits=4000000000000000 current_bits=3FF0000000000000 validation_site=expected_output_contract"
        );
    }

    #[test]
    fn parameter_refusal_site_native_after_state_render_mode_uses_owned_state_restore() {
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<AutomatedGainPlugin>>(
            c"/test/parameter-site-native",
        )
        .expect("existing owned ABI entry");
        // Existing fixture state stores these raw f32 bits. Its native get_value
        // widens them to f64 after load; no diagnostic callback or new query.
        let restored = f32::from_bits(0x7FC0_0042);
        let state = restored.to_le_bytes();
        let error = parameter_site_error(prepare_loaded(
            parameter_site_request(),
            entry,
            c"org.mondrian.test.automated-gain",
            Some(&state),
            &[parameter_site_descriptor()],
            &[1],
        ));
        assert_eq!(error, format!(
            "CLAP parameter metadata is invalid or ambiguous: index=0 native_id=1 reason=current_nonfinite min_bits=0000000000000000 max_bits=3FF0000000000000 default_bits=3FF0000000000000 current_bits={:016X} validation_site=native_after_state_render_mode",
            (restored as f64).to_bits(),
        ));
    }

    #[test]
    fn parameter_refusal_sites_preserve_count_other_errors_and_valid_state_admission() {
        let directory = tempfile::tempdir().expect("temporary missing-library parent");
        let request = parameter_site_decoded_request(
            vec![parameter_site_descriptor(); 1025],
            directory.path().join("never-created.clap"),
        );
        assert_eq!(
            parameter_site_error(ClapAudioProcessorWorkerFactory.prepare(request)),
            "CLAP parameter count exceeds the supported bound"
        );
        let mut request = parameter_site_request();
        request.native_lane =
            clap_native_worker_lane_profile().expect("actual native lane profile");
        request.payload = b"{".to_vec();
        let error = parameter_site_error(ClapAudioProcessorWorkerFactory.prepare(request));
        assert!(error.starts_with("invalid CLAP worker payload: "));
        assert!(!error.contains("validation_site="));
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<AutomatedGainPlugin>>(
            c"/test/parameter-site-valid",
        )
        .expect("existing owned ABI entry");
        let state = 0.25_f32.to_le_bytes();
        let worker = prepare_loaded(
            parameter_site_request(),
            entry,
            c"org.mondrian.test.automated-gain",
            Some(&state),
            &[parameter_site_descriptor()],
            &[1],
        )
        .expect("valid native observation retains prior admission");
        drop(worker);
        for error in [
            AudioProcessorHostError::InvalidContract(
                "CLAP parameter metadata changed since contract probe".to_owned(),
            ),
            AudioProcessorHostError::Unavailable(
                "CLAP parameter metadata is invalid or ambiguous: unavailable".to_owned(),
            ),
        ] {
            let before = format!("{error:?}");
            assert_eq!(
                format!("{:?}", parameter_refusal_at_site(error, "decoded_payload")),
                before
            );
        }
    }

    #[test]
    fn clap_structure_diagnostic_each_compared_field_reports_first_exact_projection() {
        let mut expected = parameter_site_descriptor();
        expected.id = u32::MAX - 1;
        let mut cases = Vec::new();
        let mut actual = expected.clone();
        actual.id = u32::MAX - 2;
        cases.push((
            actual,
            "id",
            u64::from(expected.id),
            u64::from(u32::MAX - 2),
        ));
        let mut actual = expected.clone();
        actual.min_value = -0.25;
        cases.push((
            actual,
            "min_value",
            0.0_f64.to_bits(),
            (-0.25_f64).to_bits(),
        ));
        let mut actual = expected.clone();
        actual.max_value = 2.0;
        cases.push((actual, "max_value", 1.0_f64.to_bits(), 2.0_f64.to_bits()));
        let mut actual = expected.clone();
        actual.default_value = 0.5;
        cases.push((
            actual,
            "default_value",
            1.0_f64.to_bits(),
            0.5_f64.to_bits(),
        ));
        let mut actual = expected.clone();
        actual.automatable = false;
        cases.push((actual, "automatable", 1, 0));
        let mut actual = expected.clone();
        actual.stepped = true;
        cases.push((actual, "stepped", 0, 1));
        let mut actual = expected.clone();
        actual.enumeration = true;
        cases.push((actual, "enumeration", 0, 1));
        let mut actual = expected.clone();
        actual.read_only = true;
        cases.push((actual, "read_only", 0, 1));
        for (mut actual, field, expected_bits, actual_bits) in cases {
            let mut expected = expected.clone();
            if field == "enumeration" {
                // Both candidates are stepped so enum-only change is valid.
                expected.stepped = true;
                actual.stepped = true;
            }
            validate_parameters(std::slice::from_ref(&expected))
                .expect("valid expected projection");
            validate_parameters(std::slice::from_ref(&actual)).expect("valid actual projection");
            assert!(!same_clap_parameter_structure(
                std::slice::from_ref(&actual),
                std::slice::from_ref(&expected),
            ));
            let detail = first_clap_parameter_structure_mismatch(
                std::slice::from_ref(&actual),
                std::slice::from_ref(&expected),
            )
            .expect("one changed structural field");
            assert_eq!(detail, format!(
                "CLAP parameter metadata changed since contract probe: validation_site=native_after_state_render_mode index=0 field={field} expected_native_id=4294967294 actual_native_id={} expected_bits={expected_bits:016X} actual_bits={actual_bits:016X}",
                actual.id,
            ));
            assert!(detail.is_ascii());
            assert!(detail.len() < 512);
        }
    }

    #[test]
    fn clap_structure_diagnostic_count_order_and_overlap_preserve_original_precedence() {
        let first = parameter_site_descriptor();
        let mut second = first.clone();
        second.id = 2;
        let expected = [first.clone(), second.clone()];
        assert!(!same_clap_parameter_structure(&[second.clone()], &expected));
        assert_eq!(first_clap_parameter_structure_mismatch(&[second.clone()], &expected).as_deref(),
            Some("CLAP parameter metadata changed since contract probe: validation_site=native_after_state_render_mode field=count expected_count=2 actual_count=1"));
        let reordered = [second, first.clone()];
        assert!(!same_clap_parameter_structure(&reordered, &expected));
        assert_eq!(first_clap_parameter_structure_mismatch(&reordered, &expected).as_deref(),
            Some("CLAP parameter metadata changed since contract probe: validation_site=native_after_state_render_mode index=0 field=id expected_native_id=1 actual_native_id=2 expected_bits=0000000000000001 actual_bits=0000000000000002"));
        let mut later = expected.clone();
        later[1].default_value = 0.5;
        assert!(!same_clap_parameter_structure(&later, &expected));
        assert_eq!(first_clap_parameter_structure_mismatch(&later, &expected).as_deref(),
            Some("CLAP parameter metadata changed since contract probe: validation_site=native_after_state_render_mode index=1 field=default_value expected_native_id=2 actual_native_id=2 expected_bits=3FF0000000000000 actual_bits=3FE0000000000000"));
        let mut overlapping = first.clone();
        overlapping.min_value = -0.25;
        overlapping.max_value = 2.0;
        overlapping.default_value = 0.5;
        let detail = first_clap_parameter_structure_mismatch(&[overlapping], &[first])
            .expect("first overlap");
        assert_eq!(detail,
            "CLAP parameter metadata changed since contract probe: validation_site=native_after_state_render_mode index=0 field=min_value expected_native_id=1 actual_native_id=1 expected_bits=0000000000000000 actual_bits=BFD0000000000000");
    }

    #[test]
    fn clap_structure_diagnostic_ignored_fields_signed_zero_and_empty_do_not_create_refusal() {
        let expected = parameter_site_descriptor();
        let mut actual = expected.clone();
        actual.current_value = 0.25;
        actual.name = "主线程的新显示名称".to_owned();
        actual.hidden = true;
        actual.min_value = -0.0;
        assert!(same_clap_parameter_structure(
            std::slice::from_ref(&actual),
            std::slice::from_ref(&expected),
        ));
        assert!(first_clap_parameter_structure_mismatch(&[actual], &[expected]).is_none());
        let mut expected = parameter_site_descriptor();
        expected.max_value = 0.0;
        expected.default_value = 0.0;
        expected.current_value = 0.0;
        let mut actual = expected.clone();
        actual.max_value = -0.0;
        actual.default_value = -0.0;
        assert!(same_clap_parameter_structure(
            &[actual.clone()],
            &[expected.clone()]
        ));
        assert!(first_clap_parameter_structure_mismatch(&[actual], &[expected]).is_none());
        assert!(same_clap_parameter_structure(&[], &[]));
        assert!(first_clap_parameter_structure_mismatch(&[], &[]).is_none());
        let mut invalid_current = parameter_site_descriptor();
        invalid_current.current_value = f64::from_bits(0x7FF8_0000_0000_0042);
        assert!(
            validate_parameters(&[invalid_current]).is_err(),
            "structure projection never replaces native validity admission"
        );
    }

    #[test]
    fn clap_structure_diagnostic_native_consumer_reports_default_bits() {
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<AutomatedGainPlugin>>(
            c"/test/parameter-structure-consumer",
        )
        .expect("existing owned ABI entry");
        let mut expected = parameter_site_descriptor();
        expected.default_value = 0.5;
        validate_parameters(std::slice::from_ref(&expected)).expect("valid expected schema");
        let detail = parameter_site_error(prepare_loaded(
            parameter_site_request(),
            entry,
            c"org.mondrian.test.automated-gain",
            None,
            &[expected],
            &[1],
        ));
        assert_eq!(detail,
            "CLAP parameter metadata changed since contract probe: validation_site=native_after_state_render_mode index=0 field=default_value expected_native_id=1 actual_native_id=1 expected_bits=3FE0000000000000 actual_bits=3FF0000000000000");
    }

    #[test]
    fn worker_rejects_malformed_payload_before_native_loading() {
        let render_contract = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 512,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 4096,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
            musical_time_reference: None,
            auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
            note_input_contract: crate::AudioProcessorNoteInputContract::default(),
            native_lane: None,
            payload: b"not json".to_vec(),
            plugin_execution_contract: contract(),
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameter_ids: Vec::new(),
            render_contract,
        };
        assert!(matches!(
            ClapAudioProcessorWorkerFactory.prepare(request),
            Err(AudioProcessorHostError::InvalidContract(_))
        ));
    }

    #[test]
    fn worker_rejects_changed_binary_before_native_loading() {
        let temp = tempfile::tempdir().expect("temporary plugin directory");
        let library = temp.path().join("gain.clap");
        std::fs::write(&library, b"replacement binary").expect("write fake plugin");
        let payload = serde_json::to_vec(&ClapPayload {
            main_audio_output_id: 0,
            auxiliary_outputs: Vec::new(),
            main_audio_input: ClapMainAudioInput::Present,
            library_path: library,
            plugin_id: "org.example.gain".to_owned(),
            binary_sha256: [7; 32],
            state: None,
            parameters: Vec::new(),
            parameter_ids: Vec::new(),
        })
        .expect("payload");
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
            musical_time_reference: None,
            auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
            note_input_contract: crate::AudioProcessorNoteInputContract::default(),
            native_lane: clap_native_worker_lane_profile().expect("actual native lane profile"),
            payload,
            plugin_execution_contract: contract(),
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameter_ids: Vec::new(),
            render_contract: AudioRenderContract {
                sample_rate: 48_000,
                channel_layout: AudioChannelLayout::Stereo,
                max_block_frames: 512,
                processing_mode: AudioProcessingMode::Offline,
                processor_session_scratch_budget_bytes: 1024 * 1024,
                public_output_lookahead_budget_frames: 4096,
                compensation_delay_scratch_budget_bytes: 1024 * 1024,
            },
        };
        assert!(matches!(
            ClapAudioProcessorWorkerFactory.prepare(request),
            Err(AudioProcessorHostError::Unavailable(message)) if message.contains("changed")
        ));
    }

    #[test]
    fn probe_captures_factory_state_and_restores_supplied_state() {
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<AutomatedGainPlugin>>(
            c"/test/automated-gain-state",
        )
        .expect("static stateful CLAP entry");
        let default_probe = super::super::clap_discovery::probe_loaded(
            &entry,
            "org.mondrian.test.automated-gain",
            AudioRenderContract {
                sample_rate: 48_000,
                channel_layout: AudioChannelLayout::Stereo,
                max_block_frames: 512,
                processing_mode: AudioProcessingMode::Offline,
                processor_session_scratch_budget_bytes: usize::MAX,
                public_output_lookahead_budget_frames: usize::MAX,
                compensation_delay_scratch_budget_bytes: usize::MAX,
            },
            None,
            true,
        )
        .expect("capture factory state");
        assert_eq!(
            default_probe.captured_state,
            Some(1.0_f32.to_le_bytes().to_vec())
        );

        let authored = 0.25_f32.to_le_bytes();
        let restored_probe = super::super::clap_discovery::probe_loaded(
            &entry,
            "org.mondrian.test.automated-gain",
            AudioRenderContract {
                sample_rate: 48_000,
                channel_layout: AudioChannelLayout::Stereo,
                max_block_frames: 512,
                processing_mode: AudioProcessingMode::Offline,
                processor_session_scratch_budget_bytes: usize::MAX,
                public_output_lookahead_budget_frames: usize::MAX,
                compensation_delay_scratch_budget_bytes: usize::MAX,
            },
            Some(&authored),
            true,
        )
        .expect("restore authored state");
        assert_eq!(restored_probe.captured_state, None);
    }

    #[test]
    fn clap_abi_processes_stereo_blocks_with_continuity() {
        let deactivations_before = DEACTIVATIONS.load(TestOrdering::SeqCst);
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<GainPlugin>>(c"/test/gain")
            .expect("static test CLAP entry");
        let render_contract = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 4,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 4096,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
            musical_time_reference: None,
            auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
            note_input_contract: crate::AudioProcessorNoteInputContract::default(),
            native_lane: None,
            payload: Vec::new(),
            plugin_execution_contract: contract(),
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameter_ids: Vec::new(),
            render_contract,
        };
        let mut worker = prepare_loaded(request, entry, c"org.mondrian.test.gain", None, &[], &[])
            .expect("prepare test CLAP plugin");
        worker.enter_state(12).expect("enter CLAP state");
        let mut samples = [0.25, -0.5, 1.0, 0.125];
        let auxiliary = AudioProcessorAuxiliaryInputContract::default();
        let block = IsolatedAudioProcessorWorkerBlock::new(
            12,
            2,
            render_contract,
            &mut samples,
            &auxiliary,
            &[],
            &[],
            &[],
        );
        worker.process(block).expect("process stereo CLAP block");
        assert_eq!(samples, [0.5, -1.0, 2.0, 0.25]);
        let bad_block = IsolatedAudioProcessorWorkerBlock::new(
            12,
            2,
            render_contract,
            &mut samples,
            &auxiliary,
            &[],
            &[],
            &[],
        );
        assert!(matches!(
            worker.process(bad_block),
            Err(AudioProcessorHostError::Process(_))
        ));
        drop(worker);
        assert_eq!(
            DEACTIVATIONS.load(TestOrdering::SeqCst),
            deactivations_before + 1
        );
    }

    #[test]
    #[ignore = "requires a built Mondrian executable and the Clack gain reference DLL"]
    fn discovered_resolver_lists_installed_reference_plugin() {
        let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_HELPER to mondrian executable");
        let plugin = std::env::var_os("MONDRIAN_CLAP_TEST_PLUGIN")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_PLUGIN to Clack gain DLL");
        let resolver = DiscoveredClapAudioProcessorSpecResolver::discover(helper, [plugin])
            .expect("discover installed CLAP plugin");
        assert_eq!(resolver.descriptors().len(), 1);
        assert_eq!(
            resolver.descriptors()[0].plugin_id,
            "org.rust-audio.clack.gain"
        );
    }

    #[test]
    #[ignore = "requires a built Mondrian executable and the Clack gain reference DLL"]
    fn discovered_resolver_probes_and_restores_author_state() {
        use crate::{
            AudioProcessingMode, AudioProcessorInsertionPoint, AudioProcessorOccurrence,
            AudioProcessorOccurrenceOwner,
        };
        use mondrian_core::{AudioProcessorInstanceId, ProgramOutputId};

        let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_HELPER to mondrian executable");
        let plugin = std::env::var_os("MONDRIAN_CLAP_TEST_PLUGIN")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_PLUGIN to Clack gain DLL");
        let resolver = DiscoveredClapAudioProcessorSpecResolver::discover(helper, [plugin])
            .expect("discover installed CLAP plugin");
        let state = 0.5_f32.to_le_bytes();
        let instance = resolver
            .create_instance(
                "org.rust-audio.clack.gain",
                AudioRenderContract {
                    sample_rate: 48_000,
                    channel_layout: AudioChannelLayout::Stereo,
                    max_block_frames: 512,
                    processing_mode: AudioProcessingMode::Realtime,
                    processor_session_scratch_budget_bytes: 1024 * 1024,
                    public_output_lookahead_budget_frames: 4096,
                    compensation_delay_scratch_budget_bytes: 1024 * 1024,
                },
                Some(state.to_vec()),
            )
            .expect("capture installed plugin parameters");
        let definition = instance.definition.clone();
        let parameters = instance
            .parameters
            .iter()
            .map(|(id, parameter)| (id.clone(), parameter.clone()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(parameters.len(), 1);
        assert!(
            parameters[&mondrian_core::ParameterId::new("clap.param.1").expect("volume ID")]
                .automation
                .is_none()
        );
        let (_, library_path, _) = resolver
            .plugins
            .get("org.rust-audio.clack.gain")
            .expect("selected reference plugin");
        let (restored, _) = probe_clap_plugin_registration_with_state(
            &resolver.helper_executable,
            library_path,
            "org.rust-audio.clack.gain",
            AudioRenderContract {
                sample_rate: 48_000,
                channel_layout: AudioChannelLayout::Stereo,
                max_block_frames: 512,
                processing_mode: AudioProcessingMode::Realtime,
                processor_session_scratch_budget_bytes: 1024 * 1024,
                public_output_lookahead_budget_frames: 4096,
                compensation_delay_scratch_budget_bytes: 1024 * 1024,
            },
            Some(&state),
            false,
        )
        .expect("query actual current value after native state restore");
        assert_eq!(restored.parameters.len(), 1);
        assert_eq!(restored.parameters[0].id, 1);
        assert_eq!(restored.parameters[0].current_value, 0.5);
        let request = AudioProcessorPrepareRequest::new(
            AudioProcessorOccurrence {
                instance_id: AudioProcessorInstanceId::new(),
                owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
                insertion: AudioProcessorInsertionPoint::PreFader,
            },
            &definition,
            &parameters,
            Some(&state),
            AudioRenderContract {
                sample_rate: 48_000,
                channel_layout: AudioChannelLayout::Stereo,
                max_block_frames: 512,
                processing_mode: AudioProcessingMode::Realtime,
                processor_session_scratch_budget_bytes: 1024 * 1024,
                public_output_lookahead_budget_frames: 4096,
                compensation_delay_scratch_budget_bytes: 1024 * 1024,
            },
        );
        let spec = resolver.resolve(request).expect("probe installed processor contract");
        let payload: ClapPayload =
            serde_json::from_slice(&spec.preparation_payload).expect("CLAP worker payload");
        assert_eq!(payload.state, Some(state.to_vec()));
        assert_eq!(payload.parameter_ids, vec![1]);
        assert_eq!(
            spec.plugin_execution_contract.algorithmic_latency_frames(),
            0
        );
    }

    #[test]
    #[ignore = "requires a built Mondrian executable and the Clack gain reference DLL"]
    fn reference_clap_receives_multiple_parameter_events() {
        use super::super::SharedParameterEvent;
        use mondrian_core::ParameterId;

        let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_HELPER to mondrian executable");
        let plugin = std::env::var_os("MONDRIAN_CLAP_TEST_PLUGIN")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_PLUGIN to Clack gain DLL");
        let render_contract = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 4,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 4096,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        let registration = probe_clap_plugin_registration(
            &helper,
            &plugin,
            "org.rust-audio.clack.gain",
            render_contract,
            None,
        )
        .expect("probe Clack gain");
        let parameter_ids = vec![ParameterId::new("clap.param.1").expect("stable parameter ID")];
        let payload = serde_json::to_vec(&ClapPayload {
            main_audio_output_id: 0,
            auxiliary_outputs: Vec::new(),
            main_audio_input: ClapMainAudioInput::Present,
            library_path: registration.library_path,
            plugin_id: registration.plugin_id,
            binary_sha256: registration.binary_sha256,
            state: None,
            parameters: registration.parameters,
            parameter_ids: vec![1],
        })
        .expect("encode worker payload");
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
            musical_time_reference: None,
            auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
            note_input_contract: crate::AudioProcessorNoteInputContract::default(),
            native_lane: None,
            payload,
            plugin_execution_contract: registration.execution_contract,
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameter_ids: parameter_ids.clone(),
            render_contract,
        };
        let mut worker = ClapAudioProcessorWorkerFactory.prepare(request).expect("prepare CLAP");
        worker.enter_state(0).expect("enter CLAP state");
        let mut samples = [1.0_f32; 8];
        let auxiliary = AudioProcessorAuxiliaryInputContract::default();
        let events = [
            SharedParameterEvent { lane: 0, sample_offset: 0, value: 0.25 },
            SharedParameterEvent { lane: 0, sample_offset: 2, value: 0.5 },
        ];
        let block = IsolatedAudioProcessorWorkerBlock::new(
            0,
            4,
            render_contract,
            &mut samples,
            &auxiliary,
            &[],
            &parameter_ids,
            &events,
        );
        worker.process(block).expect("process exact CLAP parameter events");
        // This Clack example multiplies the entire buffer once per event batch.
        // Its result verifies delivery of both events, but is not a sample-accurate oracle.
        assert_eq!(samples, [0.125; 8]);
    }

    #[test]
    fn clap_abi_preserves_sample_accurate_parameter_offsets() {
        use super::super::SharedParameterEvent;
        use mondrian_core::ParameterId;

        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<AutomatedGainPlugin>>(
            c"/test/automated-gain",
        )
        .expect("static automated CLAP fixture");
        let render_contract = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 4,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 4096,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        let parameter_ids = vec![ParameterId::new("clap.param.1").expect("parameter identity")];
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
            musical_time_reference: None,
            auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
            note_input_contract: crate::AudioProcessorNoteInputContract::default(),
            native_lane: None,
            payload: Vec::new(),
            plugin_execution_contract: contract(),
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameter_ids: parameter_ids.clone(),
            render_contract,
        };
        let descriptor = super::super::ClapParameterDescriptor {
            id: 1,
            name: "Gain".to_owned(),
            min_value: 0.0,
            max_value: 1.0,
            default_value: 1.0,
            current_value: 1.0,
            automatable: true,
            stepped: false,
            enumeration: false,
            read_only: false,
            hidden: false,
        };
        let mut worker = prepare_loaded(
            request,
            entry,
            c"org.mondrian.test.automated-gain",
            None,
            &[descriptor],
            &[1],
        )
        .expect("prepare automated fixture");
        worker.enter_state(0).expect("enter fixture state");
        let mut samples = [1.0_f32; 8];
        let auxiliary = AudioProcessorAuxiliaryInputContract::default();
        let events = [
            SharedParameterEvent { lane: 0, sample_offset: 0, value: 0.25 },
            SharedParameterEvent { lane: 0, sample_offset: 2, value: 0.5 },
        ];
        let block = IsolatedAudioProcessorWorkerBlock::new(
            0,
            4,
            render_contract,
            &mut samples,
            &auxiliary,
            &[],
            &parameter_ids,
            &events,
        );
        worker.process(block).expect("process exact parameter offsets");
        assert_eq!(samples, [0.25, 0.25, 0.25, 0.25, 0.5, 0.5, 0.5, 0.5]);
    }
    // Owned ABI fixture: processing mode controls both the independent gain and
    // declared delay, so silently retaining CLAP's default realtime mode fails.
    struct LifecyclePlugin<const CASE: usize>;
    struct LifecycleShared<'a> {
        host: HostSharedHandle<'a>,
        case: usize,
        render_mode: AtomicU32,
        active: AtomicBool,
        mode_configured: AtomicBool,
        input_present: AtomicBool,
        configuration_values: [std::sync::atomic::AtomicU64; 2],
    }
    struct LifecycleMain<'a>(&'a LifecycleShared<'a>);
    struct LifecycleProcessor<'a, const CASE: usize> {
        shared: &'a LifecycleShared<'a>,
        delay: Vec<[f32; 2]>,
        cursor: usize,
    }
    struct LifecycleAudit {
        visited: AtomicU32,
        failures: AtomicU32,
        active_port_queries: AtomicU32,
        activations: AtomicU32,
        configuration_flushes: AtomicU32,
        configuration_events: AtomicU32,
    }
    impl LifecycleAudit {
        const fn new() -> Self {
            Self {
                visited: AtomicU32::new(0),
                failures: AtomicU32::new(0),
                active_port_queries: AtomicU32::new(0),
                activations: AtomicU32::new(0),
                configuration_flushes: AtomicU32::new(0),
                configuration_events: AtomicU32::new(0),
            }
        }
        fn clear(&self) {
            self.visited.store(0, TestOrdering::SeqCst);
            self.failures.store(0, TestOrdering::SeqCst);
            self.active_port_queries.store(0, TestOrdering::SeqCst);
            self.activations.store(0, TestOrdering::SeqCst);
            self.configuration_flushes.store(0, TestOrdering::SeqCst);
            self.configuration_events.store(0, TestOrdering::SeqCst);
        }
    }
    static LIFECYCLE_AUDITS: [LifecycleAudit; 13] = [const { LifecycleAudit::new() }; 13];
    static CALLBACK_COUNTS: [AtomicU32; 2] = [const { AtomicU32::new(0) }; 2];
    const INIT: u32 = 1;
    const CALLBACK: u32 = 2;
    const RENDER: u32 = 4;
    const ACTIVATE: u32 = 8;
    const LATENCY: u32 = 16;
    const TAIL: u32 = 32;
    const RESET: u32 = 64;
    const START: u32 = 128;
    const PROCESS: u32 = 256;
    const STOP: u32 = 512;
    const DEACTIVATE: u32 = 1024;
    const DESTROY: u32 = 2048;
    const BACKGROUND: u32 = 4096;
    const FLUSH: u32 = 8192;

    impl LifecycleShared<'_> {
        fn check(&self, stage: u32, audio: bool) {
            let audit = &LIFECYCLE_AUDITS[self.case];
            audit.visited.fetch_or(stage, TestOrdering::SeqCst);
            if let Some(check) = self.host.get_extension::<HostThreadCheck>() {
                if check.is_main_thread(&self.host) != Some(true)
                    || check.is_audio_thread(&self.host) != Some(audio)
                {
                    audit.failures.fetch_add(1, TestOrdering::SeqCst);
                }
            } else if self.case >= 3 {
                audit.failures.fetch_add(1, TestOrdering::SeqCst);
            }
        }
        fn audio_port_query(&self) {
            if self.active.load(TestOrdering::SeqCst) {
                LIFECYCLE_AUDITS[self.case]
                    .active_port_queries
                    .fetch_add(1, TestOrdering::SeqCst);
            }
        }
        fn delay_frames(&self) -> usize {
            if self.render_mode.load(TestOrdering::SeqCst) == 0 {
                3
            } else {
                5
            }
        }
    }
    impl<'a> PluginShared<'a> for LifecycleShared<'a> {}
    impl Drop for LifecycleShared<'_> {
        fn drop(&mut self) {
            self.check(DESTROY, false);
        }
    }

    // Case12 is the retained optional-profile consumer with two native values.
    // Case11 retains its zero-parameter active flush callback and
    // native start/process stages distinguish failure from consuming a request.
    impl PluginMainThreadParams for LifecycleMain<'_> {
        fn count(&self) -> u32 {
            if self.0.case == 12 {
                2
            } else {
                0
            }
        }
        fn get_info(&self, index: u32, writer: &mut ParamInfoWriter) {
            if self.0.case == 12 && index < 2 {
                writer.set(&ParamInfo {
                    id: ClapId::new(index + 1),
                    flags: ParamInfoFlags::IS_AUTOMATABLE,
                    cookie: Default::default(),
                    name: if index == 0 { b"First" } else { b"Second" },
                    module: b"",
                    min_value: 0.0,
                    max_value: 1.0,
                    default_value: if index == 0 { 0.25 } else { 0.75 },
                });
            }
        }
        fn get_value(&self, id: ClapId) -> Option<f64> {
            if self.0.case != 12 {
                return None;
            }
            [ClapId::new(1), ClapId::new(2)]
                .iter()
                .position(|candidate| *candidate == id)
                .map(|index| {
                    f64::from_bits(self.0.configuration_values[index].load(TestOrdering::SeqCst))
                })
        }
        fn value_to_text(
            &self,
            _id: ClapId,
            _value: f64,
            _writer: &mut ParamDisplayWriter,
        ) -> std::fmt::Result {
            Err(std::fmt::Error)
        }
        fn text_to_value(&self, _id: ClapId, _text: &CStr) -> Option<f64> {
            None
        }
        fn flush(&self, input: &clack_plugin::prelude::InputEvents, _output: &mut OutputEvents) {
            self.0.check(FLUSH, false);
            if self.0.case == 12 {
                assert!(!self.0.active.load(TestOrdering::SeqCst));
                let audit = &LIFECYCLE_AUDITS[12];
                audit.configuration_flushes.fetch_add(1, TestOrdering::SeqCst);
                let mut previous = 0;
                for event in input.iter() {
                    let Some(CoreEventSpace::ParamValue(value)) = event.as_core_event() else {
                        panic!("only explicit scalar events are admitted");
                    };
                    let index = [ClapId::new(1), ClapId::new(2)]
                        .iter()
                        .position(|id| Some(*id) == value.param_id())
                        .expect("actual native id");
                    assert!(index + 1 > previous, "actual events preserve lane order");
                    previous = index + 1;
                    assert_eq!(event.header().time(), 0);
                    self.0.configuration_values[index]
                        .store(value.value().to_bits(), TestOrdering::SeqCst);
                    audit.configuration_events.fetch_add(1, TestOrdering::SeqCst);
                }
            }
        }
    }

    impl<const CASE: usize> PluginAudioProcessorParams for LifecycleProcessor<'_, CASE> {
        fn flush(
            &mut self,
            _input: &clack_plugin::prelude::InputEvents,
            _output: &mut OutputEvents,
        ) {
            self.shared.check(FLUSH, true);
        }
    }

    impl<const CASE: usize> Plugin for LifecyclePlugin<CASE> {
        type Shared<'a> = LifecycleShared<'a>;
        type MainThread<'a> = LifecycleMain<'a>;
        type AudioProcessor<'a> = LifecycleProcessor<'a, CASE>;
        fn declare_extensions(
            builder: &mut PluginExtensions<Self>,
            _shared: Option<&Self::Shared<'_>>,
        ) {
            builder
                .register::<PluginAudioPorts>()
                .register::<PluginRender>()
                .register::<PluginLatency>()
                .register::<PluginTail>();
            if matches!(CASE, 8..=10 | 12) {
                builder.register::<PluginState>();
            }
            if matches!(CASE, 11 | 12) {
                builder.register::<PluginParams>();
            }
        }
    }
    impl<const CASE: usize> DefaultPluginFactory for LifecyclePlugin<CASE> {
        fn get_descriptor() -> PluginDescriptor {
            PluginDescriptor::new(
                "org.mondrian.test.render-lifecycle",
                "Render Lifecycle Fixture",
            )
        }
        fn new_shared(host: HostSharedHandle<'_>) -> Result<Self::Shared<'_>, PluginError> {
            if CASE >= 3 && host.get_extension::<HostThreadCheck>().is_none() {
                return Err(PluginError::Message("missing host thread-check"));
            }
            let shared = LifecycleShared {
                host,
                case: CASE,
                render_mode: AtomicU32::new(0),
                active: AtomicBool::new(false),
                mode_configured: AtomicBool::new(false),
                input_present: AtomicBool::new(CASE != 9),
                configuration_values: [
                    std::sync::atomic::AtomicU64::new(0.25_f64.to_bits()),
                    std::sync::atomic::AtomicU64::new(0.75_f64.to_bits()),
                ],
            };
            shared.check(INIT, false);
            if let Some(check) = host.get_extension::<HostThreadCheck>() {
                let valid = std::thread::scope(|scope| {
                    scope
                        .spawn(move || {
                            check.is_main_thread(&host) == Some(false)
                                && check.is_audio_thread(&host) == Some(false)
                        })
                        .join()
                        .expect("background extension check")
                });
                LIFECYCLE_AUDITS[CASE].visited.fetch_or(BACKGROUND, TestOrdering::SeqCst);
                if !valid {
                    LIFECYCLE_AUDITS[CASE].failures.fetch_add(1, TestOrdering::SeqCst);
                }
            }
            host.request_callback();
            if matches!(CASE, 8..=10 | 12) {
                host.request_restart();
            }
            Ok(shared)
        }
        fn new_main_thread<'a>(
            _host: HostMainThreadHandle<'a>,
            shared: &'a Self::Shared<'a>,
        ) -> Result<Self::MainThread<'a>, PluginError> {
            Ok(LifecycleMain(shared))
        }
    }
    impl<'a> PluginMainThread<'a, LifecycleShared<'a>> for LifecycleMain<'a> {
        fn on_main_thread(&self) {
            self.0.check(CALLBACK, false);
            if matches!(self.0.case, 6 | 7) {
                let count = CALLBACK_COUNTS[self.0.case - 6].fetch_add(1, TestOrdering::SeqCst) + 1;
                let limit = if self.0.case == 6 { 40 } else { 16 };
                if count < limit {
                    self.0.host.request_callback();
                }
            }
        }
    }
    impl PluginAudioPortsImpl for LifecycleMain<'_> {
        fn count(&self, is_input: bool) -> u32 {
            self.0.audio_port_query();
            if is_input {
                u32::from(self.0.input_present.load(TestOrdering::SeqCst))
            } else {
                1
            }
        }
        fn get(&self, index: u32, _is_input: bool, writer: &mut AudioPortInfoWriter) {
            self.0.audio_port_query();
            if index == 0 {
                writer.set(&AudioPortInfo {
                    id: ClapId::new(0),
                    name: b"main",
                    channel_count: 2,
                    flags: AudioPortFlags::IS_MAIN,
                    port_type: Some(AudioPortType::STEREO),
                    in_place_pair: None,
                });
            }
        }
    }
    impl PluginStateImpl for LifecycleMain<'_> {
        fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
            output.write_all(&[0x43])?;
            if self.0.case == 12 {
                for value in &self.0.configuration_values {
                    output.write_all(&value.load(TestOrdering::SeqCst).to_le_bytes())?;
                }
            }
            Ok(())
        }
        fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
            let mut byte = [0];
            input.read_exact(&mut byte)?;
            if byte != [0x43] {
                return Err(PluginError::Message("owned lifecycle state"));
            }
            if self.0.case == 12 {
                for value in &self.0.configuration_values {
                    let mut bytes = [0; 8];
                    input.read_exact(&mut bytes)?;
                    value.store(u64::from_le_bytes(bytes), TestOrdering::SeqCst);
                }
            }
            Ok(())
        }
    }
    impl PluginRenderImpl for LifecycleMain<'_> {
        fn has_hard_realtime_requirement(&self) -> bool {
            self.0.check(RENDER, false);
            self.0.case == 1
        }
        fn set(&self, mode: RenderMode) -> Result<(), PluginError> {
            self.0.check(RENDER, false);
            if matches!(self.0.case, 9 | 10)
                && self.0.mode_configured.swap(true, TestOrdering::SeqCst)
            {
                assert!(!self.0.active.load(TestOrdering::SeqCst));
                self.0.input_present.fetch_xor(true, TestOrdering::SeqCst);
            }
            if self.0.case == 2 && mode == RenderMode::Offline {
                return Err(PluginError::Message("offline rendering declined"));
            }
            self.0
                .render_mode
                .store((mode == RenderMode::Offline) as u32, TestOrdering::SeqCst);
            Ok(())
        }
    }
    impl PluginLatencyImpl for LifecycleMain<'_> {
        fn get(&self) -> u32 {
            self.0.check(LATENCY, false);
            if self.0.active.load(TestOrdering::SeqCst) {
                self.0.delay_frames() as u32
            } else {
                u32::MAX
            }
        }
    }
    impl<const CASE: usize> PluginTailImpl for LifecycleProcessor<'_, CASE> {
        fn get(&self) -> TailLength {
            self.shared.check(TAIL, true);
            TailLength::Finite(self.delay.len() as u32)
        }
    }
    impl<'a, const CASE: usize>
        clack_plugin::plugin::PluginAudioProcessor<'a, LifecycleShared<'a>, LifecycleMain<'a>>
        for LifecycleProcessor<'a, CASE>
    {
        fn activate(
            _host: HostAudioProcessorHandle<'a>,
            _main: &LifecycleMain<'a>,
            shared: &'a LifecycleShared<'a>,
            _config: PluginConfig,
        ) -> Result<Self, PluginError> {
            shared.check(ACTIVATE, false);
            if CASE == 11 {
                let params = shared
                    .host
                    .get_extension::<clack_extensions::params::HostParams>()
                    .ok_or(PluginError::Message("missing host params"))?;
                params.request_flush(&shared.host);
            }
            LIFECYCLE_AUDITS[CASE].activations.fetch_add(1, TestOrdering::SeqCst);
            shared.active.store(true, TestOrdering::SeqCst);
            Ok(Self {
                shared,
                delay: vec![[0.0; 2]; shared.delay_frames()],
                cursor: 0,
            })
        }
        fn reset(&mut self) {
            self.shared.check(RESET, true);
            self.delay.fill([0.0; 2]);
            self.cursor = 0;
        }
        fn start_processing(&mut self) -> Result<(), PluginError> {
            self.shared.check(START, true);
            if CASE == 4 {
                Err(PluginError::Message("fixture start failure"))
            } else {
                Ok(())
            }
        }
        fn process(
            &mut self,
            _process: Process,
            mut audio: Audio,
            _events: Events,
        ) -> Result<ProcessStatus, PluginError> {
            self.shared.check(PROCESS, true);
            self.shared.host.request_callback();
            if CASE == 5 {
                return Err(PluginError::Message("fixture process failure"));
            }
            let mut pair = audio.port_pair(0).ok_or(PluginError::Message("missing pair"))?;
            let mut channels =
                pair.channels()?.into_f32().ok_or(PluginError::Message("Float32 pair"))?;
            let gain = if self.shared.render_mode.load(TestOrdering::SeqCst) == 0 {
                2.0
            } else {
                3.0
            };
            let mut next_cursor = self.cursor;
            for (channel, pair) in channels.iter_mut().enumerate() {
                let mut cursor = self.cursor;
                match pair {
                    ChannelPair::InputOutput(input, output) => {
                        for (target, source) in output.iter_mut().zip(input.iter()) {
                            let delayed = self.delay[cursor][channel];
                            self.delay[cursor][channel] = source * gain;
                            *target = delayed;
                            cursor = (cursor + 1) % self.delay.len();
                        }
                    }
                    _ => return Err(PluginError::Message("separate input/output fixture")),
                }
                next_cursor = cursor;
            }
            self.cursor = next_cursor;
            Ok(ProcessStatus::Continue)
        }
        fn stop_processing(&mut self) {
            self.shared.check(STOP, true);
        }
        fn deactivate(self, _main: &LifecycleMain<'a>) {
            self.shared.check(DEACTIVATE, false);
            self.shared.active.store(false, TestOrdering::SeqCst);
        }
    }

    #[test]
    fn clap_configuration_service_preserves_native_owned_lanes_and_flushes_only_explicit_constants()
    {
        use super::super::{
            ClapParameterDescriptor, NativeConfigurationControlOutcome,
            NativeConfigurationControlRequest,
        };
        for (profile, expected, flushes, event_count) in [
            ([None, None], [0.25, 0.75], 0, 0),
            ([None, Some(0.5)], [0.25, 0.5], 1, 1),
            ([Some(0.5), None], [0.5, 0.75], 1, 1),
            ([Some(0.5), Some(1.0)], [0.5, 1.0], 1, 2),
        ] {
            LIFECYCLE_AUDITS[12].clear();
            let entry = PluginEntry::load_from_clack::<SinglePluginEntry<LifecyclePlugin<12>>>(
                c"/test/configuration-native-owned-values",
            )
            .unwrap();
            let mut request = sidechain_request();
            request.auxiliary_inputs = AudioProcessorAuxiliaryInputContract::default();
            request.render_contract.max_block_frames = 16;
            request.plugin_execution_contract = AudioProcessorExecutionContract::new(
                5,
                AudioProcessorTail::Finite(5),
                true,
                true,
                true,
                0,
            )
            .unwrap();
            let parameters = [0.25, 0.75]
                .into_iter()
                .enumerate()
                .map(|(index, value)| ClapParameterDescriptor {
                    id: index as u32 + 1,
                    name: if index == 0 { "First" } else { "Second" }.to_owned(),
                    min_value: 0.0,
                    max_value: 1.0,
                    default_value: value,
                    current_value: value,
                    automatable: true,
                    stepped: false,
                    enumeration: false,
                    read_only: false,
                    hidden: false,
                })
                .collect::<Vec<_>>();
            #[cfg(windows)]
            let native_main = std::rc::Rc::new(
                vst3_host::window::WindowsEditorMainThread::initialize()
                    .expect("same existing owned native-main initialization"),
            );
            let mut worker = prepare_loaded_on_main(
                #[cfg(windows)]
                native_main,
                request,
                entry,
                c"org.mondrian.test.render-lifecycle",
                None,
                ClapMainAudioInput::Present,
                0,
                &parameters,
                &[1, 2],
            )
            .unwrap();
            let current_values = |worker: &mut ClapWorker| {
                let handle = worker.instance.plugin_handle();
                let params = handle.get_extension::<PluginParams>().unwrap();
                [ClapId::new(1), ClapId::new(2)].map(|id| params.get_value(&handle, id).unwrap())
            };
            assert_eq!(current_values(&mut worker), [0.25, 0.75]);
            let cohort = worker.native_configuration_cohort().unwrap();
            let outcome = worker
                .service_native_configuration(NativeConfigurationControlRequest {
                    expected_cohort: cohort,
                    control_token: std::num::NonZeroU64::new(79).unwrap(),
                    canonical_constant_values: &profile,
                    configuration_limit_bytes: 64,
                    control_buffer_grant_bytes: 128 * 1024,
                })
                .unwrap();
            let NativeConfigurationControlOutcome::ReadyForInstall(candidate) = outcome else {
                panic!("actual retained candidate: {outcome:?}");
            };
            assert_eq!(candidate.serviced_cohort, cohort);
            assert_eq!(current_values(&mut worker), expected);
            let mut expected_state = vec![0x43];
            for value in expected {
                expected_state.extend_from_slice(&value.to_le_bytes());
            }
            assert_eq!(
                candidate.configuration, expected_state,
                "actual State save retains None values"
            );
            assert_eq!(
                LIFECYCLE_AUDITS[12].configuration_flushes.load(TestOrdering::SeqCst),
                flushes
            );
            assert_eq!(
                LIFECYCLE_AUDITS[12].configuration_events.load(TestOrdering::SeqCst),
                event_count
            );
            drop(worker);
            assert_eq!(LIFECYCLE_AUDITS[12].failures.load(TestOrdering::SeqCst), 0);
        }
    }
    #[test]
    fn clap_configuration_service_scans_audio_ports_only_while_deactivated() {
        LIFECYCLE_AUDITS[8].clear();
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<LifecyclePlugin<8>>>(
            c"/test/configuration-port-phase",
        )
        .expect("owned configuration lifecycle fixture");
        let mut request = sidechain_request();
        request.auxiliary_inputs = AudioProcessorAuxiliaryInputContract::default();
        request.render_contract.max_block_frames = 16;
        request.plugin_execution_contract = AudioProcessorExecutionContract::new(
            5,
            AudioProcessorTail::Finite(5),
            true,
            true,
            true,
            0,
        )
        .unwrap();
        let mut worker = prepare_loaded(
            request,
            entry,
            c"org.mondrian.test.render-lifecycle",
            None,
            &[],
            &[],
        )
        .expect("actual one-input retained object");
        let cohort = worker.native_configuration_cohort().expect("actual plugin request_restart");
        assert_eq!(
            cohort.request,
            mondrian_core::NativeAudioReprepareRequest::ClapRestart
        );
        let outcome = worker
            .service_native_configuration(super::super::NativeConfigurationControlRequest {
                expected_cohort: cohort,
                control_token: std::num::NonZeroU64::new(73).unwrap(),
                canonical_constant_values: &[],
                configuration_limit_bytes: 64,
                control_buffer_grant_bytes: 128 * 1024,
            })
            .expect("actual configuration lifecycle");
        let super::super::NativeConfigurationControlOutcome::ReadyForInstall(candidate) = outcome
        else {
            panic!("configuration was not captured: {outcome:?}");
        };
        assert_eq!(candidate.serviced_cohort, cohort);
        assert_eq!(candidate.configuration, [0x43]);
        assert_eq!(candidate.observed_contract.algorithmic_latency_frames(), 5);
        assert_eq!(
            candidate.observed_contract.tail(),
            AudioProcessorTail::Finite(5)
        );
        drop(worker);
        assert_eq!(LIFECYCLE_AUDITS[8].failures.load(TestOrdering::SeqCst), 0);
        assert_eq!(
            LIFECYCLE_AUDITS[8].active_port_queries.load(TestOrdering::SeqCst),
            0,
            "CLAP audio-ports count/get must not run while activated"
        );
    }

    #[test]
    fn clap_render_mode_selects_independent_pcm_and_scheduling_facts() {
        for mode in [AudioProcessingMode::Offline, AudioProcessingMode::Realtime] {
            LIFECYCLE_AUDITS[0].clear();
            let expected_delay = if mode == AudioProcessingMode::Realtime {
                3
            } else {
                5
            };
            let expected_gain = if mode == AudioProcessingMode::Realtime {
                2.0
            } else {
                3.0
            };
            let entry = PluginEntry::load_from_clack::<SinglePluginEntry<LifecyclePlugin<0>>>(
                c"/test/render-mode",
            )
            .expect("owned rendering entry");
            let render = AudioRenderContract {
                sample_rate: 48_000,
                channel_layout: AudioChannelLayout::Stereo,
                max_block_frames: 16,
                processing_mode: mode,
                processor_session_scratch_budget_bytes: 1024 * 1024,
                public_output_lookahead_budget_frames: 1024,
                compensation_delay_scratch_budget_bytes: 1024 * 1024,
            };
            let contract = AudioProcessorExecutionContract::new(
                expected_delay,
                AudioProcessorTail::Finite(expected_delay),
                true,
                mode == AudioProcessingMode::Realtime,
                mode == AudioProcessingMode::Offline,
                0,
            )
            .expect("mode facts");
            let request = IsolatedAudioProcessorWorkerPrepareRequest {
                musical_time_reference: None,
                auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
                note_input_contract: crate::AudioProcessorNoteInputContract::default(),
                native_lane: None,
                payload: Vec::new(),
                plugin_execution_contract: contract,
                auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
                parameter_ids: Vec::new(),
                render_contract: render,
            };
            let mut worker = prepare_loaded(
                request,
                entry,
                c"org.mondrian.test.render-lifecycle",
                None,
                &[],
                &[],
            )
            .expect("prepare selected render mode");
            for start in [73, 19] {
                worker.enter_state(start).expect("reset selected mode");
                let auxiliary = AudioProcessorAuxiliaryInputContract::default();
                let mut samples = [0.0; 24];
                for (frame, pair) in samples.chunks_exact_mut(2).enumerate() {
                    let source = (frame as f32 + 1.0) / 16.0;
                    pair.copy_from_slice(&[source, -source]);
                }
                worker
                    .process(IsolatedAudioProcessorWorkerBlock::new(
                        start,
                        12,
                        render,
                        &mut samples,
                        &auxiliary,
                        &[],
                        &[],
                        &[],
                    ))
                    .expect("mode-specific DSP");
                for (frame, pair) in samples.chunks_exact(2).enumerate() {
                    let expected = if frame < expected_delay {
                        0.0
                    } else {
                        (frame - expected_delay + 1) as f32 / 16.0 * expected_gain
                    };
                    assert_eq!(
                        pair,
                        [expected, -expected],
                        "independent delayed gain oracle"
                    );
                }
            }
            drop(worker);
            assert_eq!(LIFECYCLE_AUDITS[0].failures.load(TestOrdering::SeqCst), 0);
            let stages = INIT
                | CALLBACK
                | RENDER
                | ACTIVATE
                | LATENCY
                | TAIL
                | RESET
                | START
                | PROCESS
                | STOP
                | DEACTIVATE
                | DESTROY;
            assert_eq!(
                LIFECYCLE_AUDITS[0].visited.load(TestOrdering::SeqCst) & stages,
                stages
            );
        }
    }

    #[test]
    fn clap_main_callback_continuous_requests_yield_without_fault_or_loss() {
        let audit = &LIFECYCLE_AUDITS[6];
        audit.clear();
        CALLBACK_COUNTS[0].store(0, TestOrdering::SeqCst);
        let (shared, mut instance) = callback_instance::<6>();
        assert!(shared.callback_requested.load(Ordering::Acquire));
        for expected in [16, 32, 40] {
            shared
                .service_callbacks(&mut instance)
                .expect("legal repeating callback is not a plugin fault");
            assert_eq!(
                CALLBACK_COUNTS[0].load(TestOrdering::SeqCst),
                expected,
                "a tick is bounded to sixteen real ABI calls"
            );
            assert_eq!(
                shared.callback_requested.load(Ordering::Acquire),
                expected < 40,
                "request made inside native callback survives the yield"
            );
        }
        shared.service_callbacks(&mut instance).expect("idle tick");
        assert_eq!(
            CALLBACK_COUNTS[0].load(TestOrdering::SeqCst),
            40,
            "no unsolicited callback"
        );
        assert_eq!(
            audit.failures.load(TestOrdering::SeqCst),
            0,
            "all actual callbacks were on the main role"
        );
        drop(instance);
        assert_eq!(audit.failures.load(TestOrdering::SeqCst), 0);
    }

    #[test]
    fn clap_main_callback_exact_tick_limit_is_success_when_no_request_remains() {
        let audit = &LIFECYCLE_AUDITS[7];
        audit.clear();
        CALLBACK_COUNTS[1].store(0, TestOrdering::SeqCst);
        let (shared, mut instance) = callback_instance::<7>();
        shared
            .service_callbacks(&mut instance)
            .expect("sixteen finite callbacks are valid");
        assert_eq!(CALLBACK_COUNTS[1].load(TestOrdering::SeqCst), 16);
        assert!(!shared.callback_requested.load(Ordering::Acquire));
        shared.service_callbacks(&mut instance).expect("next idle tick");
        assert_eq!(CALLBACK_COUNTS[1].load(TestOrdering::SeqCst), 16);
        assert_eq!(audit.failures.load(TestOrdering::SeqCst), 0);
    }

    fn callback_instance<const CASE: usize>() -> (ClapHostShared, PluginInstance<ClapHost>) {
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<LifecyclePlugin<CASE>>>(
            c"/test/main-callback",
        )
        .expect("real in-process CLAP ABI entry");
        let info = HostInfo::new(
            "Mondrian",
            "Mondrian",
            "https://github.com/shaloong/mondrian",
            env!("CARGO_PKG_VERSION"),
        )
        .expect("host identity");
        let shared = ClapHostShared::new();
        let native = shared.clone().with_native_logs().expect("actual declared host extensions");
        let instance = PluginInstance::<ClapHost>::new(
            |_| native,
            |shared| ClapHostMainThread::new(shared),
            &entry,
            c"org.mondrian.test.render-lifecycle",
            &info,
        )
        .expect("instantiate owned callback fixture");
        (shared, instance)
    }

    #[test]
    fn clap_render_mode_rejects_hard_realtime_and_declined_offline() {
        lifecycle_offline_rejection::<1>();
        lifecycle_offline_rejection::<2>();
    }

    fn lifecycle_offline_rejection<const CASE: usize>() {
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<LifecyclePlugin<CASE>>>(
            c"/test/render-rejection",
        )
        .expect("owned mode rejection entry");
        let render = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 16,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 1024,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
            musical_time_reference: None,
            auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
            note_input_contract: crate::AudioProcessorNoteInputContract::default(),
            native_lane: None,
            payload: Vec::new(),
            plugin_execution_contract: AudioProcessorExecutionContract::new(
                3,
                AudioProcessorTail::Finite(3),
                true,
                false,
                true,
                0,
            )
            .expect("untrusted mode facts"),
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameter_ids: Vec::new(),
            render_contract: render,
        };
        assert!(
            matches!(prepare_loaded(request, entry, c"org.mondrian.test.render-lifecycle", None, &[], &[]),
            Err(AudioProcessorHostError::Unavailable(detail)) if detail.contains("render") || detail.contains("real-time")),
            "a realtime-only or declined mode cannot be admitted as offline"
        );
    }

    #[test]
    fn clap_thread_check_covers_processing_and_failure_teardown() {
        lifecycle_thread_case::<3>(false);
        lifecycle_thread_case::<4>(true);
        lifecycle_thread_case::<5>(true);
    }

    fn lifecycle_thread_case<const CASE: usize>(fail: bool) {
        let audit = &LIFECYCLE_AUDITS[CASE];
        audit.clear();
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<LifecyclePlugin<CASE>>>(
            c"/test/thread-lifecycle",
        )
        .expect("owned lifecycle entry");
        let render = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 16,
            processing_mode: AudioProcessingMode::Realtime,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 1024,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
            musical_time_reference: None,
            auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
            note_input_contract: crate::AudioProcessorNoteInputContract::default(),
            native_lane: None,
            payload: Vec::new(),
            plugin_execution_contract: AudioProcessorExecutionContract::new(
                3,
                AudioProcessorTail::Finite(3),
                true,
                true,
                false,
                0,
            )
            .expect("fixture contract"),
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameter_ids: Vec::new(),
            render_contract: render,
        };
        let mut worker = prepare_loaded(
            request,
            entry,
            c"org.mondrian.test.render-lifecycle",
            None,
            &[],
            &[],
        )
        .expect("host thread extension and lifecycle preparation");
        worker.enter_state(71).expect("first accepted cold entry");
        assert_eq!(
            audit.visited.load(TestOrdering::SeqCst) & RESET,
            0,
            "first entry preserves the accepted native initial state"
        );
        worker.enter_state(71).expect("later explicit audio reset role");
        let auxiliary = AudioProcessorAuxiliaryInputContract::default();
        let mut pcm = [0.25; 16];
        assert_eq!(
            worker
                .process(IsolatedAudioProcessorWorkerBlock::new(
                    71,
                    8,
                    render,
                    &mut pcm,
                    &auxiliary,
                    &[],
                    &[],
                    &[]
                ))
                .is_err(),
            fail
        );
        drop(worker);
        assert_eq!(
            audit.failures.load(TestOrdering::SeqCst),
            0,
            "all roles are native-instance specific"
        );
        let expected = INIT
            | CALLBACK
            | RENDER
            | ACTIVATE
            | LATENCY
            | TAIL
            | RESET
            | START
            | DEACTIVATE
            | DESTROY
            | BACKGROUND
            | if CASE == 4 { 0 } else { PROCESS | STOP };
        assert_eq!(
            audit.visited.load(TestOrdering::SeqCst) & expected,
            expected,
            "all lifecycle paths executed"
        );
    }

    #[test]
    fn clap_beat_overflow_precedes_actual_sdk_start_process_and_flush_claim() {
        let audit = &LIFECYCLE_AUDITS[11];
        audit.clear();
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<LifecyclePlugin<11>>>(
            c"/test/beat-overflow",
        )
        .unwrap();
        let render = AudioRenderContract {
            sample_rate: 48_000,
            max_block_frames: 4,
            processing_mode: AudioProcessingMode::Realtime,
            channel_layout: AudioChannelLayout::Stereo,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 1024,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        // At sample0 this reference is exactly i64::MIN ticks. At sample-1
        // seconds remain representable but the beat position is below i64::MIN.
        let reference = mondrian_core::MusicalTimeReference::new(
            mondrian_core::TimelineTime::new(1_i64 << 32, 1).unwrap(),
            mondrian_core::TimelineTime::ONE,
        )
        .unwrap();
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
            musical_time_reference: Some(reference),
            auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
            note_input_contract: crate::AudioProcessorNoteInputContract::default(),
            native_lane: None,
            payload: Vec::new(),
            plugin_execution_contract: AudioProcessorExecutionContract::new(
                3,
                AudioProcessorTail::Finite(3),
                true,
                true,
                false,
                0,
            )
            .unwrap(),
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameter_ids: Vec::new(),
            render_contract: render,
        };
        #[cfg(windows)]
        let native_main =
            std::rc::Rc::new(vst3_host::window::WindowsEditorMainThread::initialize().unwrap());
        let mut worker = prepare_loaded_on_main(
            #[cfg(windows)]
            native_main,
            request,
            entry,
            c"org.mondrian.test.render-lifecycle",
            None,
            ClapMainAudioInput::Present,
            0,
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(audit.activations.load(TestOrdering::SeqCst), 1);
        let flush_pending = |worker: &ClapWorker| {
            worker
                .instance
                .access_shared_handler(|host| host.services.flush_requested.load(Ordering::Acquire))
        };
        assert!(
            flush_pending(&worker),
            "the actual plugin requested flush during activation"
        );
        worker.enter_state(-1).unwrap();
        let auxiliary = AudioProcessorAuxiliaryInputContract::default();
        let mut pcm = [0.25; 8];
        let rejected = worker.process(
            IsolatedAudioProcessorWorkerBlock::new(
                -1,
                4,
                render,
                &mut pcm,
                &auxiliary,
                &[],
                &[],
                &[],
            )
            .with_musical_time_reference(Some(reference)),
        );
        assert!(
            matches!(rejected, Err(AudioProcessorHostError::Process(detail))
            if detail == "CLAP signed musical position exceeds the beats representation")
        );
        assert_eq!(pcm, [0.25; 8]);
        assert_eq!(worker.last_end, Some(-1));
        assert_eq!(
            audit.visited.load(TestOrdering::SeqCst) & (START | PROCESS | FLUSH),
            0
        );
        assert!(
            flush_pending(&worker),
            "overflow cannot claim the real pending flush"
        );
        // Same prepared reference/instance, explicitly entered at its valid
        // boundary, reaches the real ABI and consumes the request in process.
        worker.enter_state(0).unwrap();
        worker
            .process(
                IsolatedAudioProcessorWorkerBlock::new(
                    0,
                    4,
                    render,
                    &mut pcm,
                    &auxiliary,
                    &[],
                    &[],
                    &[],
                )
                .with_musical_time_reference(Some(reference)),
            )
            .unwrap();
        assert_eq!(pcm, [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5]);
        assert_eq!(
            audit.visited.load(TestOrdering::SeqCst) & (START | PROCESS),
            START | PROCESS
        );
        assert!(!flush_pending(&worker));
        assert_eq!(worker.last_end, Some(4));
        drop(worker);
        assert_eq!(audit.failures.load(TestOrdering::SeqCst), 0);
    }

    #[test]
    fn clap_thread_check_audio_role_is_instance_local_and_unwind_safe() {
        let owner = ClapHostShared::new();
        let sibling = ClapHostShared::new();
        assert!(owner.is_main_thread());
        assert!(!owner.is_audio_thread());
        {
            let _role = owner.enter_audio_thread();
            assert!(owner.is_main_thread());
            assert!(owner.is_audio_thread());
            assert!(!sibling.is_audio_thread());
            std::thread::scope(|scope| {
                let shared = &owner;
                scope
                    .spawn(move || {
                        assert!(!shared.is_main_thread());
                        assert!(!shared.is_audio_thread());
                    })
                    .join()
                    .expect("background query during owner audio role");
            });
        }
        assert!(!owner.is_audio_thread());
        let unwind = std::panic::catch_unwind(|| {
            let _role = owner.enter_audio_thread();
            assert!(owner.is_audio_thread());
            panic!("controlled host unwind");
        });
        assert!(unwind.is_err());
        assert!(!owner.is_audio_thread());
    }
}

#[cfg(test)]
mod canonical_transport_tests {
    use super::*;
    #[test]
    fn signed_seconds_use_checked_integer_rounding() {
        for (sample, rate, bits) in [
            (1, 48_000, 44_739),
            (-1, 48_000, -44_739),
            (1, 3, 715_827_883),
            (-1, 3, -715_827_883),
            (96_000, 48_000, 4_294_967_296),
        ] {
            assert_eq!(
                clap_seconds_position(sample, rate).expect("representable").to_bits(),
                bits
            );
        }
        assert!(clap_seconds_position(i64::MAX, 1).is_err());
        assert!(clap_seconds_position(i64::MIN, 1).is_err());
    }
}

#[cfg(test)]
#[path = "tests/clap_parameter_allocations.rs"]
mod parameter_allocation_tests;

#[cfg(test)]
#[path = "clap_output_owning_tests.rs"]
mod clap_output_owning_tests;

#[cfg(test)]
#[path = "clap_output_owner_contracts.rs"]
mod clap_output_owner_contracts;

#[cfg(windows)]
struct ClapMainRole<'a> {
    note_inputs: Option<&'a super::clap_note_input_contract::RestoredNoteInputs>,
    instance: &'a mut PluginInstance<ClapHost>,
    gui: &'a mut super::clap_gui_owner::ClapGuiOwner,
    shared: &'a ClapHostShared,
    phase: &'a super::native_configuration_control::NativeConfigurationPhase,
    object: crate::NativeAudioObjectObservation,
    receiver: std::cell::RefCell<&'a mut super::native_audio_log::NativeAudioLogReceiver>,
    binding: Option<crate::NativeAudioObjectBindReceipt>,
    author_delivery: &'a Option<native_author::PendingDelivery>,
}

#[cfg(windows)]
impl super::IsolatedAudioProcessorMainRole for ClapMainRole<'_> {
    fn allocation_layouts(&self) -> Option<[crate::NativeHostBufferLayout; 2]> {
        // Clack's existing PluginInstance/PluginAudioProcessor are borrowed
        // directly. It allocates no additional SDK role Box for this split.
        Some([
            clap_native_layout(std::alloc::Layout::for_value(self)),
            clap_empty_layout(),
        ])
    }

    fn service_native_main_thread(&mut self) -> Result<(), AudioProcessorHostError> {
        if let Some(inputs) = self.note_inputs {
            super::clap_note_input_contract::service_names(self.instance, inputs)?;
        }
        self.gui.service(self.instance)?;
        super::clap_main_services::service_main_tick(self.shared, self.instance)
    }

    fn finish_processing_scope(&mut self) -> Result<(), AudioProcessorHostError> {
        // Temporary DSP scope completion preserves GUI attachment and object.
        // Only the full owner closes after join, before actual replacement/drop.
        self.service_native_main_thread()
    }

    fn native_configuration_cohort(
        &self,
    ) -> Result<Option<super::NativeRequestCohort>, AudioProcessorHostError> {
        Ok(clap_native_cohort(self.shared))
    }

    fn pending_reprepare(
        &self,
    ) -> Result<Option<crate::NativeObservedRequest>, AudioProcessorHostError> {
        Ok(clap_native_cohort(self.shared)
            .map(crate::NativeObservedRequest::Cohort)
            .or(self.phase.blocked_request().map(crate::NativeObservedRequest::RequestOnly)))
    }

    fn preserve_native_observations(
        &self,
    ) -> Result<crate::NativeAudioObservations, AudioProcessorHostError> {
        // Only the actual main role can borrow its one receiver. Preserve checks
        // the same ingress incarnation and observes loss; it never drains logs.
        let logs = self.instance.access_shared_handler(|host| {
            self.receiver.borrow_mut().observations(
                &host.logs,
                crate::NativeAudioObservationConsumption::Preserve,
            )
        })?;
        let source = native_author::source_status(
            self.shared.parameter_source.snapshot(),
            self.instance.access_shared_handler(|host| {
                host.services.flush_requested.load(Ordering::Acquire)
            }),
        );
        Ok(crate::NativeAudioObservations {
            object: Some(self.object),
            logs,
            author: Some(crate::NativeAudioAuthorObservation {
                source,
                delivery: self.author_delivery.as_ref().map_or(
                    crate::NativeAudioAuthorDeliveryState::Preserved,
                    native_author::PendingDelivery::projection,
                ),
            }),
            ..Default::default()
        })
    }

    fn native_author_binding_matches(
        &self,
        request: crate::NativeAudioObjectBindRequest,
    ) -> Result<bool, AudioProcessorHostError> {
        Ok(self.binding.is_some_and(|receipt| {
            receipt.binding() == request.binding
                && receipt.object() == request.expected_object
                && request.expected_object == self.object
        }) && self.author_delivery.is_none())
    }

    fn native_editor_close_requested(&self) -> Result<bool, AudioProcessorHostError> {
        Ok(self.gui.close_requested()?
            || self.instance.access_shared_handler(|host| host.services.close_requested()))
    }
}

#[cfg(windows)]
struct ClapDspRole<'a> {
    native: &'a mut PluginAudioProcessor<ClapHost>,
    signal: ClapSignalFields<'a>,
}

#[cfg(windows)]
impl super::IsolatedAudioProcessorProcessingRole for ClapDspRole<'_> {
    fn allocation_layouts(&self) -> Option<[crate::NativeHostBufferLayout; 2]> {
        Some([
            clap_native_layout(std::alloc::Layout::for_value(self)),
            clap_empty_layout(),
        ])
    }

    fn process(
        &mut self,
        block: IsolatedAudioProcessorWorkerBlock<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        self.native.access_shared_handler(|host| host.services.note_ports.check())?;
        self.signal.process(self.native, block)?;
        self.native.access_shared_handler(|host| host.services.note_ports.check())
    }

    fn idle_service_interval(&self) -> Option<std::time::Duration> {
        Some(super::native_worker_ingress::NATIVE_MAIN_SERVICE_INTERVAL)
    }

    fn service_native_idle(&mut self) -> Result<(), AudioProcessorHostError> {
        self.signal.flush_parameters(self.native, false).map(|_| ())
    }

    fn flush_native_author_parameters(
        &mut self,
    ) -> Result<crate::NativeAudioAuthorParameterFlushOutcome, AudioProcessorHostError> {
        self.signal.flush_parameters(self.native, true)
    }
}

fn clap_native_cohort(shared: &ClapHostShared) -> Option<super::NativeRequestCohort> {
    let cohort = shared.restart_cohort();
    cohort.pending().then_some(super::NativeRequestCohort {
        serial: cohort.serial(),
        request: mondrian_core::NativeAudioReprepareRequest::ClapRestart,
    })
}

#[cfg(windows)]
fn clap_native_layout(layout: std::alloc::Layout) -> crate::NativeHostBufferLayout {
    crate::NativeHostBufferLayout { bytes: layout.size(), alignment: layout.align() }
}

#[cfg(windows)]
const fn clap_empty_layout() -> crate::NativeHostBufferLayout {
    crate::NativeHostBufferLayout { bytes: 0, alignment: 1 }
}

/// One actual successful CLAP object load, not a class ID or address guess.
/// Same-object reset/deactivate/reactivate does not invent a fresh load serial.
fn next_clap_native_object() -> Result<crate::NativeAudioObjectObservation, AudioProcessorHostError>
{
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let serial = NEXT
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            value.checked_add(1)
        })
        .map_err(|_| invalid("CLAP native object load serial exhausted"))?;
    let serial = std::num::NonZeroU64::new(serial)
        .ok_or_else(|| invalid("CLAP native load serial is zero"))?;
    let pid = std::num::NonZeroU32::new(std::process::id())
        .ok_or_else(|| invalid("CLAP native process ID is zero"))?;
    Ok(crate::NativeAudioObjectObservation::new(pid, serial))
}

pub(super) fn clap_native_worker_lane_profile(
) -> Result<Option<crate::NativeWorkerLaneResourceProfile>, AudioProcessorHostError> {
    #[cfg(windows)]
    {
        let owners = crate::NativeWorkerLaneOwnedLayouts {
            adapter_resident: clap_native_layout(std::alloc::Layout::new::<ClapWorker>()),
            reader_slots: clap_native_layout(std::alloc::Layout::new::<
                super::native_worker_ingress::NativeReaderSlots,
            >()),
            processing_slots: clap_native_layout(std::alloc::Layout::new::<
                super::native_worker_processing::NativeProcessingSlots,
            >()),
            main_inline: clap_native_layout(std::alloc::Layout::new::<(
                super::native_worker_ingress::NativeCommandIngress<'static>,
                super::worker::WorkerProcessContext<'static>,
                super::native_worker_processing::NativeDspDispatch<'static>,
                crate::NativeAudioObservations,
                super::clap_metadata::MetadataScanScratch,
                clack_extensions::params::ParamInfoBuffer,
            )>()),
            runtime_main_role: clap_native_layout(
                std::alloc::Layout::new::<ClapMainRole<'static>>(),
            ),
            runtime_dsp_role: clap_native_layout(std::alloc::Layout::new::<ClapDspRole<'static>>()),
            sdk_main_role: clap_empty_layout(),
            sdk_dsp_role: clap_empty_layout(),
        };
        let bytes = super::MAX_CONTROL_FRAME_BYTES;
        let control = crate::NativeWorkerControlOverlapLayout {
            decoded_input: crate::NativeHostBufferLayout { bytes, alignment: align_of::<f64>() },
            reply_storage: crate::NativeHostBufferLayout { bytes, alignment: 1 },
            staged_seed: crate::NativeHostBufferLayout { bytes, alignment: 1 },
        };
        Ok(Some(
            crate::NativeWorkerLaneResourceProfile::scoped_main_dsp(owners, control)
                .map_err(AudioProcessorHostError::from)?,
        ))
    }
    #[cfg(not(windows))]
    {
        Ok(None)
    }
}

#[cfg(all(test, windows))]
#[path = "clap_native_gui_contracts.rs"]
mod native_gui_contract_tests;

#[cfg(all(test, windows))]
#[path = "clap_paused_params_contracts.rs"]
mod paused_parameter_contract_tests;

#[cfg(test)]
mod same_call_structure_contracts {
    use super::*;

    fn descriptor(id: u32) -> super::super::ClapParameterDescriptor {
        super::super::ClapParameterDescriptor {
            id,
            name: "Mix".to_owned(),
            min_value: 0.0,
            max_value: 1.0,
            default_value: 0.5,
            current_value: 0.5,
            automatable: true,
            stepped: false,
            enumeration: false,
            read_only: false,
            hidden: false,
        }
    }

    #[test]
    fn same_call_structure_uses_original_first_index_and_ignores_legal_fields() {
        let expected = [descriptor(7), descriptor(9)];
        let mut actual = expected.clone();
        actual[0].name = "新名称".to_owned();
        actual[0].current_value = 0.25;
        actual[0].hidden = true;
        actual[0].min_value = -0.0;
        let capture = ParameterQueryCapture::default();
        assert!(same_clap_parameter_structure(&actual, &expected));
        assert!(first_clap_parameter_structure_mismatch_observed(
            &actual,
            &expected,
            Some(&capture)
        )
        .is_none());
        actual[1].default_value = f64::from_bits(0x378F_FE40_0000_0000);
        assert!(!same_clap_parameter_structure(&actual, &expected));
        let original =
            first_clap_parameter_structure_mismatch(&actual, &expected).expect("original mismatch");
        let observed =
            first_clap_parameter_structure_mismatch_observed(&actual, &expected, Some(&capture))
                .expect("same mismatch");
        assert!(original.contains("index=1 field=default_value expected_native_id=9 actual_native_id=9 expected_bits=3FE0000000000000 actual_bits=378FFE4000000000"));
        assert_eq!(
            observed,
            original + &capture.suffix(Some(1), Some(&actual[1]))
        );
        assert!(observed
            .contains("incomplete=1 selected_index=Some(1) selected_observation=unavailable"));
        assert!(observed.contains("copied=00000009/01/0000000000000000/3FF0000000000000/378FFE4000000000/3FE0000000000000"));
    }

    #[test]
    fn same_call_structure_count_mismatch_has_no_invented_parameter_index() {
        let expected = [descriptor(7), descriptor(9)];
        let actual = [descriptor(7)];
        let capture = ParameterQueryCapture::default();
        let original = first_clap_parameter_structure_mismatch(&actual, &expected)
            .expect("original count mismatch");
        let observed =
            first_clap_parameter_structure_mismatch_observed(&actual, &expected, Some(&capture))
                .expect("same count mismatch");
        assert!(original.contains("field=count expected_count=2 actual_count=1"));
        assert_eq!(observed, original + &capture.suffix(None, None));
        assert!(observed
            .contains("selected_index=None selected_observation=unavailable copied=unavailable"));
        assert!(!observed.contains("before_origin=FreshZeroedConstructor"));
        assert!(observed.is_ascii());
    }
}

#[cfg(test)]
pub(crate) use parameter_allocation_tests::measure_host_allocations_for_test;
