//! CLAP ABI execution inside the supervised audio worker process.

use super::clap_discovery::{
    fingerprint_clap_binary, inspect_audio_inputs, list_parameters,
    probe_clap_plugin_registration_with_state, validate_parameters,
};
use super::{
    probe_clap_plugin_registration, scan_clap_library_descriptors, ClapPluginDescriptor,
    IsolatedAudioProcessorSpecResolver, IsolatedAudioProcessorWorker,
    IsolatedAudioProcessorWorkerBlock, IsolatedAudioProcessorWorkerFactory,
    IsolatedAudioProcessorWorkerPrepareRequest, IsolatedAudioProcessorWorkerSpec,
};
use crate::{
    AudioProcessorAuxiliaryInputContract, AudioProcessorExecutionContract, AudioProcessorHostError,
    AudioProcessorPrepareRequest,
};
use clack_extensions::latency::PluginLatency;
use clack_extensions::render::{PluginRender, RenderMode};
use clack_extensions::state::PluginState;
use clack_extensions::tail::{PluginTail, TailLength};
use clack_extensions::thread_check::{HostThreadCheck, HostThreadCheckImpl};
use clack_host::events::event_types::{ParamValueEvent, TransportEvent, TransportFlags};
use clack_host::events::{EventFlags, EventHeader};
use clack_host::prelude::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, ClapId, EventBuffer, HostExtensions,
    HostHandlers, HostInfo, InputChannel, OutputEvents, Pckn, PluginAudioConfiguration,
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
use std::marker::PhantomData;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::{self, ThreadId};

pub(super) struct ClapHost;

#[derive(Clone)]
pub(super) struct ClapHostShared {
    restart_requested: Arc<AtomicBool>,
    callback_requested: Arc<AtomicBool>,
    main_thread: ThreadId,
    audio_thread: Arc<AtomicBool>,
}

impl ClapHostShared {
    pub(super) fn new() -> Self {
        Self {
            restart_requested: Arc::new(AtomicBool::new(false)),
            callback_requested: Arc::new(AtomicBool::new(false)),
            main_thread: thread::current().id(),
            audio_thread: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Mark only the synchronous CLAP audio-role calls, retaining fixed main identity.
    ///
    /// The guard can only be released on its owner thread and unwind restores
    /// the previous mark. Nested markers preserve the enclosing role; they do
    /// not detect native processor reentry.
    pub(super) fn enter_audio_thread(&self) -> ClapAudioThreadGuard<'_> {
        debug_assert_eq!(thread::current().id(), self.main_thread);
        let previous = self.audio_thread.swap(true, Ordering::AcqRel);
        ClapAudioThreadGuard {
            active: &self.audio_thread,
            previous,
            owner_thread: PhantomData,
        }
    }

    pub(super) fn service_callbacks(
        &self,
        instance: &mut PluginInstance<ClapHost>,
    ) -> Result<(), AudioProcessorHostError> {
        for _ in 0..16 {
            if !self.callback_requested.swap(false, Ordering::AcqRel) {
                return if self.restart_requested.load(Ordering::Acquire) {
                    Err(invalid("CLAP plugin requested a host restart"))
                } else {
                    Ok(())
                };
            }
            instance.call_on_main_thread_callback();
        }
        Err(invalid(
            "CLAP plugin requested unbounded main-thread callbacks",
        ))
    }

    pub(super) fn restart_requested(&self) -> bool {
        self.restart_requested.load(Ordering::Acquire)
    }
}

/// An owner-thread-only audio-role marker that restores its previous mark.
///
/// The zero-sized `Rc` marker makes this guard neither Send nor Sync, so its
/// release cannot migrate to another thread. It does not detect processor reentry.
pub(super) struct ClapAudioThreadGuard<'a> {
    active: &'a AtomicBool,
    previous: bool,
    owner_thread: PhantomData<Rc<()>>,
}

impl Drop for ClapAudioThreadGuard<'_> {
    fn drop(&mut self) {
        self.active.store(self.previous, Ordering::Release);
    }
}

impl HostThreadCheckImpl for ClapHostShared {
    fn is_main_thread(&self) -> bool {
        thread::current().id() == self.main_thread
    }

    fn is_audio_thread(&self) -> bool {
        self.is_main_thread() && self.audio_thread.load(Ordering::Acquire)
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
        self.restart_requested.store(true, Ordering::Release);
    }

    fn request_process(&self) {}

    fn request_callback(&self) {
        self.callback_requested.store(true, Ordering::Release);
    }
}

impl HostHandlers for ClapHost {
    type Shared<'a> = ClapHostShared;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder.register::<HostThreadCheck>();
    }
}

/// Child-only CLAP factory. Plugin binaries are never loaded by the editor process.
pub struct ClapAudioProcessorWorkerFactory;

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

    /// Capture one selected plugin's current plain values into a portable author instance.
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
        parameter
            .set_automation(
                ExactAutomationCurve::new(parameter_id.clone(), descriptor.current_value).map_err(
                    |error| invalid(format!("CLAP current parameter value is invalid: {error}")),
                )?,
            )
            .map_err(|error| invalid(format!("CLAP parameter value is invalid: {error}")))?;
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
        let registration = probe_clap_plugin_registration(
            &self.helper_executable,
            library_path,
            plugin_id,
            request.render_contract(),
            request.opaque_state(),
        )?;
        if registration.binary_sha256 != *fingerprint {
            return Err(unavailable(
                "CLAP binary changed after descriptor discovery",
            ));
        }
        ClapAudioProcessorSpecResolver::new(self.helper_executable.clone(), [registration])?
            .resolve(request)
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
                || by_id.insert(plugin.plugin_id.clone(), plugin).is_some()
            {
                return Err(invalid("CLAP registrations need unique IDs, absolute libraries, and explicit state entry"));
            }
        }
        Ok(Self { helper_executable, plugins: by_id })
    }
}

impl IsolatedAudioProcessorSpecResolver for ClapAudioProcessorSpecResolver {
    fn resolve(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
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
        let plugin = self
            .plugins
            .get(plugin_id)
            .ok_or_else(|| unavailable(format!("CLAP plugin {plugin_id} is not registered")))?;
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
            state: request.opaque_state().map(ToOwned::to_owned),
            parameters: plugin.parameters.clone(),
            parameter_ids: clap_parameter_ids,
        })
        .map_err(|error| invalid(format!("CLAP worker payload encoding failed: {error}")))?;
        IsolatedAudioProcessorWorkerSpec::new(
            self.helper_executable.clone(),
            payload,
            plugin.execution_contract,
            plugin.auxiliary_inputs.clone(),
            parameter_ids,
            request.render_contract(),
        )
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ClapPayload {
    library_path: PathBuf,
    plugin_id: String,
    binary_sha256: [u8; 32],
    #[serde(default)]
    state: Option<Vec<u8>>,
    parameters: Vec<super::ClapParameterDescriptor>,
    parameter_ids: Vec<u32>,
}

impl IsolatedAudioProcessorWorkerFactory for ClapAudioProcessorWorkerFactory {
    fn prepare(
        &self,
        request: IsolatedAudioProcessorWorkerPrepareRequest,
    ) -> Result<Box<dyn IsolatedAudioProcessorWorker>, AudioProcessorHostError> {
        let payload: ClapPayload = serde_json::from_slice(request.payload())
            .map_err(|error| invalid(format!("invalid CLAP worker payload: {error}")))?;
        if !payload.library_path.is_absolute() || payload.plugin_id.is_empty() {
            return Err(invalid(
                "CLAP library path must be absolute and plugin ID nonempty",
            ));
        }
        validate_parameters(&payload.parameters)?;
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
        let entry = unsafe { PluginEntry::load(payload.library_path.as_os_str()) }
            .map_err(|error| unavailable(format!("CLAP library load failed: {error}")))?;
        prepare_loaded(
            request,
            entry,
            plugin_id.as_c_str(),
            payload.state.as_deref(),
            &payload.parameters,
            &payload.parameter_ids,
        )
    }
}

fn prepare_loaded(
    request: IsolatedAudioProcessorWorkerPrepareRequest,
    entry: PluginEntry,
    plugin_id: &CStr,
    state: Option<&[u8]>,
    expected_parameters: &[super::ClapParameterDescriptor],
    parameter_ids: &[u32],
) -> Result<Box<dyn IsolatedAudioProcessorWorker>, AudioProcessorHostError> {
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| unavailable("CLAP library has no plugin factory"))?;
    if factory
        .plugin_descriptors()
        .filter(|descriptor| descriptor.id() == Some(plugin_id))
        .take(2)
        .count()
        != 1
    {
        return Err(unavailable(
            "CLAP plugin ID is missing or duplicated in the library",
        ));
    }
    let info = HostInfo::new(
        "Mondrian",
        "Mondrian",
        "https://github.com/shaloong/mondrian",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|error| invalid(format!("invalid CLAP host identity: {error}")))?;
    let shared = ClapHostShared::new();
    let mut instance =
        PluginInstance::<ClapHost>::new(|_| shared.clone(), |_| (), &entry, plugin_id, &info)
            .map_err(|error| unavailable(format!("CLAP instance creation failed: {error}")))?;
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
    if list_parameters(&handle)? != expected_parameters {
        return Err(invalid(
            "CLAP parameter metadata changed since contract probe",
        ));
    }
    let input_metadata = inspect_audio_inputs(&handle, request.render_contract().channel_layout)?;
    let auxiliary_inputs = input_metadata.contract;
    if &auxiliary_inputs != request.auxiliary_inputs() {
        return Err(invalid("CLAP auxiliary ports changed since contract probe"));
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
    let event_capacity = frames
        .checked_mul(parameter_ids.len())
        .filter(|capacity| *capacity <= 1_048_576)
        .ok_or_else(|| invalid("CLAP parameter event capacity exceeds the supported bound"))?;
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
    let input_port_count = 1 + auxiliary_inputs.buses.len();
    let input = vec![vec![vec![0.0; frames]; channel_count]; input_port_count];
    let output = vec![vec![0.0; frames]; channel_count];
    Ok(Box::new(ClapWorker {
        processor: Some(processor),
        instance,
        contract: request.plugin_execution_contract(),
        input,
        output,
        auxiliary_inputs,
        input_names: input_metadata.names,
        input_ports: AudioPorts::with_capacity(channel_count * input_port_count, input_port_count),
        output_ports: AudioPorts::with_capacity(channel_count, 1),
        parameter_lanes,
        input_events: EventBuffer::with_capacity(event_capacity),
        last_end: None,
        shared,
    }))
}

struct ClapWorker {
    processor: Option<PluginAudioProcessor<ClapHost>>,
    instance: PluginInstance<ClapHost>,
    contract: AudioProcessorExecutionContract,
    input: Vec<Vec<Vec<f32>>>,
    auxiliary_inputs: AudioProcessorAuxiliaryInputContract,
    input_names: Vec<crate::AudioProcessorInputName>,
    output: Vec<Vec<f32>>,
    input_ports: AudioPorts,
    output_ports: AudioPorts,
    parameter_lanes: Vec<(ClapId, f64, f64, bool)>,
    input_events: EventBuffer,
    last_end: Option<i64>,
    shared: ClapHostShared,
}

impl IsolatedAudioProcessorWorker for ClapWorker {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        self.contract
    }

    fn auxiliary_input_contract(&self) -> AudioProcessorAuxiliaryInputContract {
        self.auxiliary_inputs.clone()
    }

    fn auxiliary_input_names(&self) -> Vec<crate::AudioProcessorInputName> {
        self.input_names.clone()
    }

    fn enter_state(&mut self, start_sample: i64) -> Result<(), AudioProcessorHostError> {
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
        mut block: IsolatedAudioProcessorWorkerBlock<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        let frames = block.frames();
        let end_sample = block.start_sample().checked_add(frames as i64).ok_or_else(|| {
            AudioProcessorHostError::Process("CLAP sample position overflowed".to_owned())
        })?;
        let channels = self.output.len();
        if self.last_end != Some(block.start_sample())
            || frames == 0
            || frames > self.input[0][0].len()
            || block.main_interleaved().len() != frames * channels
        {
            return Err(AudioProcessorHostError::Process(
                "CLAP block violates prepared continuity or extent".to_owned(),
            ));
        }
        if self.shared.restart_requested() {
            return Err(AudioProcessorHostError::Process(
                "CLAP plugin requested a host restart; prepare a new instance".to_owned(),
            ));
        }
        if block.parameter_lane_count() != self.parameter_lanes.len() {
            return Err(AudioProcessorHostError::Process(
                "CLAP parameter lane count changed during processing".to_owned(),
            ));
        }
        self.input_events.clear();
        for (lane, &(clap_id, min, max, stepped)) in self.parameter_lanes.iter().enumerate() {
            let events = block.parameter_events(lane).ok_or_else(|| {
                AudioProcessorHostError::Process("CLAP parameter lane is missing".to_owned())
            })?;
            for event in events {
                if event.sample_offset as usize >= frames
                    || !event.value.is_finite()
                    || event.value < min
                    || event.value > max
                    || (stepped && event.value.fract() != 0.0)
                {
                    return Err(AudioProcessorHostError::Process(
                        "CLAP parameter event violates its admitted contract".to_owned(),
                    ));
                }
                self.input_events.push(&ParamValueEvent::new(
                    event.sample_offset,
                    clap_id,
                    Pckn::match_all(),
                    event.value,
                ));
            }
        }
        self.input_events.sort();
        for (frame, samples) in block.main_interleaved().chunks_exact(channels).enumerate() {
            for (channel, sample) in samples.iter().copied().enumerate() {
                self.input[0][channel][frame] = sample;
            }
        }
        for (index, bus) in self.auxiliary_inputs.buses.iter().enumerate() {
            let samples = block.auxiliary_input(&bus.bus_key).ok_or_else(|| {
                AudioProcessorHostError::Process("CLAP auxiliary PCM is missing".to_owned())
            })?;
            if samples.len() != frames * channels {
                return Err(AudioProcessorHostError::Process(
                    "CLAP auxiliary extent changed".to_owned(),
                ));
            }
            for (frame, samples) in samples.chunks_exact(channels).enumerate() {
                for (channel, sample) in samples.iter().copied().enumerate() {
                    self.input[index + 1][channel][frame] = sample;
                }
            }
        }
        for channel in &mut self.output {
            channel[..frames].fill(0.0);
        }
        let transport = clap_transport(block.start_sample(), block.sample_rate())?;
        let input_audio = self.input_ports.with_input_buffers(self.input.iter_mut().map(|port| {
            AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_input_only(
                    port.iter_mut().map(|channel| InputChannel::variable(&mut channel[..frames])),
                ),
            }
        }));
        let mut output_audio = self.output_ports.with_output_buffers([AudioPortBuffer {
            latency: 0,
            channels: AudioPortBufferType::f32_output_only(
                self.output.iter_mut().map(|channel| &mut channel[..frames]),
            ),
        }]);
        {
            let _audio_role = self.shared.enter_audio_thread();
            let processor = self
                .processor
                .as_mut()
                .ok_or_else(|| {
                    AudioProcessorHostError::Process("CLAP processor is closed".to_owned())
                })?
                .ensure_processing_started()
                .map_err(|error| {
                    AudioProcessorHostError::Process(format!("CLAP start failed: {error}"))
                })?;
            processor
                .process(
                    &input_audio,
                    &mut output_audio,
                    &self.input_events.as_input(),
                    &mut OutputEvents::void(),
                    None,
                    Some(&transport),
                )
                .map_err(|error| {
                    AudioProcessorHostError::Process(format!("CLAP process failed: {error}"))
                })?;
        }
        self.shared
            .service_callbacks(&mut self.instance)
            .map_err(|error| AudioProcessorHostError::Process(error.to_string()))?;
        for (frame, samples) in block.main_interleaved().chunks_exact_mut(channels).enumerate() {
            for (channel, sample) in samples.iter_mut().enumerate() {
                let value = self.output[channel][frame];
                if !value.is_finite() {
                    return Err(AudioProcessorHostError::Process(
                        "CLAP produced nonfinite PCM".to_owned(),
                    ));
                }
                *sample = value;
            }
        }
        self.last_end = Some(end_sample);
        Ok(())
    }
}

impl Drop for ClapWorker {
    fn drop(&mut self) {
        if let Some(mut processor) = self.processor.take() {
            {
                let _audio_role = self.shared.enter_audio_thread();
                processor.ensure_processing_stopped();
            }
            drop(processor);
        }
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

fn clap_transport(
    start_sample: i64,
    sample_rate: u32,
) -> Result<TransportEvent, AudioProcessorHostError> {
    Ok(TransportEvent {
        header: EventHeader::new_core(0, EventFlags::empty()),
        flags: TransportFlags::HAS_SECONDS_TIMELINE,
        song_pos_seconds: clap_seconds_position(start_sample, sample_rate)?,
        song_pos_beats: BeatTime::from_bits(0),
        tempo: 0.0,
        tempo_inc: 0.0,
        loop_start_beats: BeatTime::from_bits(0),
        loop_end_beats: BeatTime::from_bits(0),
        loop_start_seconds: SecondsTime::from_bits(0),
        loop_end_seconds: SecondsTime::from_bits(0),
        bar_start: BeatTime::from_bits(0),
        bar_number: 0,
        time_signature_numerator: 0,
        time_signature_denominator: 0,
    })
}

fn invalid(detail: impl Into<String>) -> AudioProcessorHostError {
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
        HostMainThreadHandle, HostSharedHandle, Plugin, PluginAudioConfiguration as PluginConfig,
        PluginDescriptor, PluginError, PluginExtensions, PluginMainThread, PluginShared, Process,
        ProcessStatus, SinglePluginEntry,
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

    struct SidechainPlugin<const CASE: u8>;
    struct SidechainShared(AtomicBool);
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
            Ok(SidechainShared(AtomicBool::new(false)))
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
                if CASE == 4 {
                    33
                } else {
                    3
                }
            } else {
                1
            }
        }

        fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
            if index >= self.count(is_input) || (CASE == 5 && index == 2) {
                return;
            }
            let id = match index {
                0 => 0,
                1 => 11,
                _ if CASE == 1 => 11,
                _ => 7,
            };
            writer.set(&AudioPortInfo {
                id: ClapId::new(id),
                name: b"same display name",
                channel_count: if CASE == 3 && index != 0 { 1 } else { 2 },
                flags: if index == 0 || (CASE == 2 && index == 1) {
                    AudioPortFlags::IS_MAIN
                } else {
                    AudioPortFlags::empty()
                },
                port_type: Some(if CASE == 3 && index != 0 {
                    AudioPortType::MONO
                } else {
                    AudioPortType::STEREO
                }),
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
            let mut expected = [[0.0; 4]; 2];
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
        refuse_sidechain_ports::<3>();
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
            plugin_id: "org.example.gain".to_owned(),
            library_path: std::env::current_exe().expect("test executable path"),
            binary_sha256: [3; 32],
            execution_contract: contract(),
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameters: vec![
                descriptor(1, "Output Gain", false, false),
                descriptor(2, "Private meter", true, false),
                descriptor(3, "Read only", false, true),
            ],
        };
        let instance = author_instance_from_probe(registration, Some(vec![7, 8]))
            .expect("authored CLAP instance");
        assert_eq!(instance.parameters.len(), 2);
        let visible = &instance.parameters
            [&mondrian_core::ParameterId::new("clap.param.1").expect("visible ID")];
        assert_eq!(visible.ui.display_name.as_deref(), Some("Output Gain"));
        assert!(!visible.ui.hidden);
        assert_eq!(visible.automation.default_value, 0.25);
        let hidden = &instance.parameters
            [&mondrian_core::ParameterId::new("clap.param.2").expect("hidden ID")];
        assert!(hidden.ui.hidden);
        let reopened: AudioProcessorInstance = serde_json::from_slice(
            &serde_json::to_vec(&instance).expect("serialize CLAP instance"),
        )
        .expect("reopen CLAP instance");
        assert_eq!(reopened, instance);
    }

    #[test]
    fn registration_rejects_duplicate_identity_and_relative_paths() {
        let helper = std::env::current_exe().expect("test executable path");
        let registration = ClapPluginRegistration {
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
            library_path: library,
            plugin_id: "org.example.gain".to_owned(),
            binary_sha256: [7; 32],
            state: None,
            parameters: Vec::new(),
            parameter_ids: Vec::new(),
        })
        .expect("payload");
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
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
        assert_eq!(
            parameters[&mondrian_core::ParameterId::new("clap.param.1").expect("volume ID")]
                .automation
                .default_value,
            0.5
        );
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
            library_path: registration.library_path,
            plugin_id: registration.plugin_id,
            binary_sha256: registration.binary_sha256,
            state: None,
            parameters: registration.parameters,
            parameter_ids: vec![1],
        })
        .expect("encode worker payload");
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
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
    }
    impl LifecycleAudit {
        const fn new() -> Self {
            Self {
                visited: AtomicU32::new(0),
                failures: AtomicU32::new(0),
            }
        }
        fn clear(&self) {
            self.visited.store(0, TestOrdering::SeqCst);
            self.failures.store(0, TestOrdering::SeqCst);
        }
    }
    static LIFECYCLE_AUDITS: [LifecycleAudit; 6] = [const { LifecycleAudit::new() }; 6];
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
        }
    }
    impl PluginAudioPortsImpl for LifecycleMain<'_> {
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
    impl PluginRenderImpl for LifecycleMain<'_> {
        fn has_hard_realtime_requirement(&self) -> bool {
            self.0.check(RENDER, false);
            self.0.case == 1
        }
        fn set(&self, mode: RenderMode) -> Result<(), PluginError> {
            self.0.check(RENDER, false);
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
        worker.enter_state(71).expect("audio reset role");
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
