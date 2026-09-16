//! Supervised process isolation for untrusted audio processor Adapters.
//!
//! The parent retains Audio Program authority and exchanges only one admitted
//! processor block through fixed Session-owned shared storage. Native plugin
//! discovery and ABI loading live behind the Worker factory Interface; they do
//! not enter Timeline compilation or the in-process Processor Host.

mod protocol;
mod supervisor;
mod worker;

#[cfg(test)]
mod tests;

use crate::{
    AudioParameterEventBatch, AudioProcessingMode, AudioProcessor, AudioProcessorAudioIo,
    AudioProcessorAuxiliaryInputContract, AudioProcessorExecutionContract, AudioProcessorFactory,
    AudioProcessorHostError, AudioProcessorPrepareRequest, AudioProcessorProcessContext,
    AudioProcessorResolver, AudioRenderContract, BuiltInAudioProcessorResolver,
};
use mondrian_core::{AudioChannelLayout, ParameterId};
use mondrian_timeline::audio::AudioProcessorDefinitionRef;
use protocol::{
    CommandKind, SharedBlockLayout, SharedParameterEvent, WorkerCommand, MAX_CONTROL_FRAME_BYTES,
};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use supervisor::SupervisedAudioWorker;

/// Hidden endpoint environment used only between a parent Adapter and its Worker.
pub const ISOLATED_AUDIO_PROCESSOR_ENDPOINT_ENV: &str =
    "MONDRIAN_INTERNAL_AUDIO_PROCESSOR_ENDPOINT";
/// Hidden nonce environment paired with [`ISOLATED_AUDIO_PROCESSOR_ENDPOINT_ENV`].
pub const ISOLATED_AUDIO_PROCESSOR_NONCE_ENV: &str = "MONDRIAN_INTERNAL_AUDIO_PROCESSOR_NONCE";
/// Recommended product-executable dispatch argument for an audio Processor Worker.
pub const ISOLATED_AUDIO_PROCESSOR_WORKER_ARGUMENT: &str = "--internal-audio-processor-worker-v1";

/// Immutable launch and capacity facts for one process-isolated processor definition.
#[derive(Clone)]
pub struct IsolatedAudioProcessorWorkerSpec {
    helper_executable: PathBuf,
    helper_arguments: Vec<OsString>,
    dispatch_argument: Option<OsString>,
    preparation_payload: Vec<u8>,
    plugin_execution_contract: AudioProcessorExecutionContract,
    auxiliary_inputs: AudioProcessorAuxiliaryInputContract,
    parameter_ids: Vec<ParameterId>,
    render_contract: AudioRenderContract,
    startup_timeout: Duration,
    operation_timeout: Duration,
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
            helper_executable,
            helper_arguments: Vec::new(),
            dispatch_argument: Some(OsString::from(ISOLATED_AUDIO_PROCESSOR_WORKER_ARGUMENT)),
            preparation_payload,
            plugin_execution_contract,
            auxiliary_inputs,
            parameter_ids,
            render_contract,
            startup_timeout: Duration::from_secs(5),
            operation_timeout,
        };
        spec.validate()?;
        Ok(spec)
    }

    /// Prepend helper-specific arguments before the optional dispatch argument.
    ///
    /// This supports dedicated host executables and test harnesses without
    /// putting native plugin paths or state on the process command line.
    pub fn with_helper_arguments(mut self, arguments: Vec<OsString>) -> Self {
        self.helper_arguments = arguments;
        self
    }

    /// Override or remove the standard product dispatch argument.
    pub fn with_dispatch_argument(mut self, argument: Option<OsString>) -> Self {
        self.dispatch_argument = argument;
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

    fn validate(&self) -> Result<(), AudioProcessorHostError> {
        if self.helper_executable.as_os_str().is_empty()
            || self.preparation_payload.len() > MAX_CONTROL_FRAME_BYTES / 2
            || self.render_contract.sample_rate == 0
            || self.render_contract.max_block_frames == 0
            || self.startup_timeout.is_zero()
            || self.operation_timeout.is_zero()
            || self.parameter_ids.len() > 4096
        {
            return Err(invalid_contract(
                "isolated Worker path, payload, render extent, deadlines, or parameter capacity is invalid",
            ));
        }
        self.auxiliary_inputs.validate(self.render_contract.channel_layout)?;
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
            .field("parameter_count", &self.parameter_ids.len())
            .field("render_contract", &self.render_contract)
            .field("startup_timeout", &self.startup_timeout)
            .field("operation_timeout", &self.operation_timeout)
            .finish_non_exhaustive()
    }
}

/// Non-realtime Adapter that resolves one native definition into an isolated Worker spec.
pub trait IsolatedAudioProcessorSpecResolver: Send + Sync {
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
}

impl IsolatedAudioProcessorResolver {
    /// Bind one concrete native discovery/ABI Adapter.
    pub fn new(external: Arc<dyn IsolatedAudioProcessorSpecResolver>) -> Self {
        Self { external }
    }
}

impl AudioProcessorResolver for IsolatedAudioProcessorResolver {
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
        let expected_contract = request.render_contract();
        let expected_parameter_ids = request.parameters().keys().cloned().collect::<Vec<_>>();
        let spec = self.external.resolve(request)?;
        if spec.render_contract != expected_contract || spec.parameter_ids != expected_parameter_ids
        {
            return Err(invalid_contract(
                "external resolver changed the prepared render contract or parameter-lane identity",
            ));
        }
        IsolatedAudioProcessorFactory::prepare(spec)
            .map(|factory| factory as Arc<dyn AudioProcessorFactory>)
    }
}

impl fmt::Debug for IsolatedAudioProcessorResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("IsolatedAudioProcessorResolver").finish_non_exhaustive()
    }
}

/// Render-contract-bound Factory backed by one supervised Worker per instance.
pub struct IsolatedAudioProcessorFactory {
    spec: IsolatedAudioProcessorWorkerSpec,
    execution_contract: AudioProcessorExecutionContract,
    layout: SharedBlockLayout,
}

impl IsolatedAudioProcessorFactory {
    /// Validate a Worker contract through a real launch/handshake before a Plan
    /// retains this Factory.
    pub fn prepare(
        spec: IsolatedAudioProcessorWorkerSpec,
    ) -> Result<Arc<Self>, AudioProcessorHostError> {
        spec.validate()?;
        let layout = SharedBlockLayout::new(
            spec.render_contract.max_block_frames,
            spec.render_contract.channel_count(),
            spec.auxiliary_inputs.buses.len(),
            spec.parameter_ids.len(),
        )?;
        let execution_contract = spec
            .plugin_execution_contract
            .with_additional_session_scratch_bytes(layout.total_bytes)?;
        let mut validation = SupervisedAudioWorker::launch(&spec, spec.render_contract, layout)?;
        if !validation.shutdown() {
            return Err(AudioProcessorHostError::WorkerFailed {
                operation: "validation shutdown",
                detail: "Worker did not acknowledge and exit within the bounded shutdown"
                    .to_owned(),
            });
        }
        Ok(Arc::new(Self { spec, execution_contract, layout }))
    }
}

impl AudioProcessorFactory for IsolatedAudioProcessorFactory {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        self.execution_contract
    }

    fn auxiliary_input_contract(&self) -> AudioProcessorAuxiliaryInputContract {
        self.spec.auxiliary_inputs.clone()
    }

    fn create(&self) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        let worker =
            SupervisedAudioWorker::launch(&self.spec, self.spec.render_contract, self.layout)?;
        Ok(Box::new(IsolatedAudioProcessor {
            worker,
            parameter_ids: self.spec.parameter_ids.clone(),
            auxiliary_inputs: self.spec.auxiliary_inputs.clone(),
            render_contract: self.spec.render_contract,
            failed: false,
        }))
    }
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

struct IsolatedAudioProcessor {
    worker: SupervisedAudioWorker,
    parameter_ids: Vec<ParameterId>,
    auxiliary_inputs: AudioProcessorAuxiliaryInputContract,
    render_contract: AudioRenderContract,
    failed: bool,
}

impl AudioProcessor for IsolatedAudioProcessor {
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
            },
        );
        if result.is_err() {
            self.failed = true;
        }
        result
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
        let mut event_count = 0_usize;
        {
            let shared = self.worker.shared_mut();
            let (before_events, events_and_after) = shared.split_at_mut(layout.events_offset);
            let main_bytes = layout
                .main_offset
                .checked_add(samples * size_of::<f32>())
                .ok_or_else(|| invalid_contract("isolated main range overflowed"))?;
            let (main_and_gap, auxiliary_and_gap) =
                before_events.split_at_mut(layout.auxiliary_offset);
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
        let result = self.worker.invoke(
            "block processing",
            WorkerCommand {
                kind: CommandKind::Process,
                start_sample: request.start_sample,
                frames: u32::try_from(request.frames)
                    .map_err(|_| invalid_contract("block extent is not protocol-representable"))?,
                event_count: u32::try_from(event_count)
                    .map_err(|_| invalid_contract("event count is not protocol-representable"))?,
            },
        );
        if let Err(error) = result {
            self.failed = true;
            return Err(error);
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
        audio.main_interleaved().copy_from_slice(output);
        Ok(())
    }
}

/// Immutable request received by a child-side native plugin Adapter.
pub struct IsolatedAudioProcessorWorkerPrepareRequest {
    payload: Vec<u8>,
    plugin_execution_contract: AudioProcessorExecutionContract,
    auxiliary_inputs: AudioProcessorAuxiliaryInputContract,
    parameter_ids: Vec<ParameterId>,
    render_contract: AudioRenderContract,
}

impl IsolatedAudioProcessorWorkerPrepareRequest {
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
    /// Load, restore, negotiate, and return one exclusive native instance.
    fn prepare(
        &self,
        request: IsolatedAudioProcessorWorkerPrepareRequest,
    ) -> Result<Box<dyn IsolatedAudioProcessorWorker>, AudioProcessorHostError>;
}

/// Exclusive native processor instance owned entirely by one Worker process.
pub trait IsolatedAudioProcessorWorker {
    /// Revalidated native scheduling and mode facts.
    fn execution_contract(&self) -> AudioProcessorExecutionContract;

    /// Revalidated native auxiliary-bus facts.
    fn auxiliary_input_contract(&self) -> AudioProcessorAuxiliaryInputContract;

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
    start_sample: i64,
    frames: usize,
    render_contract: AudioRenderContract,
    main: &'a mut [f32],
    auxiliary_inputs: &'a AudioProcessorAuxiliaryInputContract,
    auxiliary: &'a [f32],
    parameter_ids: &'a [ParameterId],
    events: &'a [SharedParameterEvent],
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
            frames,
            render_contract,
            main,
            auxiliary_inputs,
            auxiliary,
            parameter_ids,
            events,
        }
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

fn invalid_contract(detail: &str) -> AudioProcessorHostError {
    AudioProcessorHostError::InvalidContract(detail.to_owned())
}
