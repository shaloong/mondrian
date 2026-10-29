//! Plan-time processor resolution and Session-owned callback dispatch.

pub(crate) mod host_output_fader;
pub(crate) mod native_author;
pub(crate) mod native_reprepare;
mod note_ledger;
mod notes;
pub(crate) mod state_entry;

use crate::processor_parameters::{prepare_parameter_event_batch, ProcessorParameterEventScratch};
use crate::schedule::{
    PreparedAudioSchedule, PreparedProcessor, PreparedProcessorAuxiliaryBus, PreparedRack,
    PreparedSidechainBinding,
};
#[cfg(test)]
use crate::AudioParameterEvent;
use crate::{delay::FixedDelayLine, dsp};
use crate::{
    AudioExecutionError, AudioKernelBackend, AudioProcessor, AudioProcessorAudioIo,
    AudioProcessorAuxiliaryInputContract, AudioProcessorExecutionContract, AudioProcessorFactory,
    AudioProcessorHostError, AudioProcessorInputBus, AudioProcessorMainAndInputBuses,
    AudioProcessorOccurrence, AudioProcessorOccurrenceOwner, AudioProcessorPrepareRequest,
    AudioProcessorProcessContext, AudioProcessorResolver, AudioRenderContract, AudioRenderRequest,
    AudioStateEntry, BuiltInAudioProcessorResolver,
};
#[cfg(test)]
use mondrian_core::ParameterId;
use mondrian_core::{AudioChannelLayout, AudioSampleRate};
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct PreparedProcessorFactoryBinding {
    contract: AudioProcessorExecutionContract,
    auxiliary_inputs: AudioProcessorAuxiliaryInputContract,
    auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract,
    note_inputs: crate::AudioProcessorNoteInputContract,
    factory: Arc<dyn AudioProcessorFactory>,
}

impl PreparedProcessorFactoryBinding {
    pub(crate) fn resolve(
        resolver: &dyn AudioProcessorResolver,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Self, AudioProcessorHostError> {
        if let Some(registrar) = request.native_child_settlement_registrar() {
            mark_unavailable_if_native_forwarding_unknown(registrar, || {
                resolver.native_child_settlement_requirement()
            });
        }
        let render_contract = request.render_contract();
        let mode = render_contract.processing_mode;
        let factory = isolate_adapter_call("resolution", || resolver.prepare(request))?;
        let contract =
            isolate_adapter_value("execution-contract query", || factory.execution_contract())?;
        let auxiliary_inputs = isolate_adapter_value("auxiliary-input contract query", || {
            factory.auxiliary_input_contract()
        })?;
        auxiliary_inputs.validate(render_contract.channel_layout)?;
        let auxiliary_outputs = isolate_adapter_value("auxiliary-output contract query", || {
            factory.auxiliary_output_contract()
        })?;
        auxiliary_outputs.validate(render_contract.channel_layout)?;
        let note_inputs = isolate_adapter_value("Note input contract query", || {
            factory.note_input_contract()
        })?;
        note_inputs.validate()?;
        if !contract.admits(mode) {
            return Err(AudioProcessorHostError::InvalidContract(format!(
                "processor does not admit {mode:?} execution"
            )));
        }
        Ok(Self {
            contract,
            auxiliary_inputs,
            auxiliary_outputs,
            note_inputs,
            factory,
        })
    }

    pub(crate) fn visit_host_owned_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(
            crate::AudioProcessorFactoryStorageBorrow<'a>,
        ) -> Result<(), crate::AudioProcessorFactoryStorageError>,
    ) -> Result<(), crate::AudioProcessorFactoryStorageError> {
        use crate::AudioProcessorFactoryStorageBorrow as Storage;
        visitor(Storage::arc_payload(&self.factory))?;
        visitor(Storage::complete(&self.auxiliary_inputs.buses)?)?;
        for bus in &self.auxiliary_inputs.buses {
            visitor(Storage::complete(&bus.bus_key)?)?;
        }
        self.visit_auxiliary_output_storage(visitor)?;
        visitor(Storage::complete(&self.note_inputs.ports)?)?;
        for port in &self.note_inputs.ports {
            visitor(Storage::complete(&port.port_key)?)?;
        }
        isolate_adapter_value("factory storage observation", || {
            self.factory.visit_host_owned_storage(visitor)
        })?
    }

    pub(crate) fn visit_auxiliary_output_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(
            crate::AudioProcessorFactoryStorageBorrow<'a>,
        ) -> Result<(), crate::AudioProcessorFactoryStorageError>,
    ) -> Result<(), crate::AudioProcessorFactoryStorageError> {
        use crate::AudioProcessorFactoryStorageBorrow as Storage;
        visitor(Storage::complete(&self.auxiliary_outputs.buses)?)?;
        for bus in &self.auxiliary_outputs.buses {
            visitor(Storage::complete(&bus.bus_key)?)?;
        }
        Ok(())
    }

    fn native_child_settlement_requirement(
        &self,
    ) -> mondrian_core::NativeChildSettlementRequirement {
        isolate_adapter_value("native child settlement requirement", || {
            self.factory.native_child_settlement_requirement()
        })
        .unwrap_or(mondrian_core::NativeChildSettlementRequirement::Unavailable)
    }

    pub(crate) const fn contract(&self) -> AudioProcessorExecutionContract {
        self.contract
    }

    pub(crate) fn auxiliary_inputs(&self) -> &AudioProcessorAuxiliaryInputContract {
        &self.auxiliary_inputs
    }

    pub(crate) fn auxiliary_outputs(&self) -> &crate::AudioProcessorAuxiliaryOutputContract {
        &self.auxiliary_outputs
    }

    pub(crate) fn note_inputs(&self) -> &crate::AudioProcessorNoteInputContract {
        &self.note_inputs
    }

    pub(crate) fn input_names(
        &self,
    ) -> Result<Vec<crate::AudioProcessorInputName>, AudioProcessorHostError> {
        let names = isolate_adapter_value("auxiliary-input name query", || {
            self.factory.auxiliary_input_names()
        })?;
        crate::AudioProcessorInputName::validate_all(&names, &self.auxiliary_inputs)?;
        Ok(names)
    }

    pub(crate) fn with_native_author_seed(
        &self,
        format: crate::NativeConfigurationFormat,
        bytes: &[u8],
    ) -> Result<Self, AudioProcessorHostError> {
        let factory = isolate_adapter_call("native author factory seed", || {
            self.factory.with_native_configuration_seed(format, bytes)
        })?
        .ok_or_else(|| {
            AudioProcessorHostError::InvalidContract(
                "prepared factory does not support native state seeds".to_owned(),
            )
        })?;
        let contract = isolate_adapter_value("native seed execution-contract query", || {
            factory.execution_contract()
        })?;
        let auxiliary_inputs = isolate_adapter_value("native seed auxiliary-input query", || {
            factory.auxiliary_input_contract()
        })?;
        let auxiliary_outputs =
            isolate_adapter_value("native seed auxiliary-output query", || {
                factory.auxiliary_output_contract()
            })?;
        let note_inputs = isolate_adapter_value("native seed Note-input query", || {
            factory.note_input_contract()
        })?;
        note_inputs.validate()?;
        if contract != self.contract
            || auxiliary_inputs != self.auxiliary_inputs
            || auxiliary_outputs != self.auxiliary_outputs
            || note_inputs != self.note_inputs
        {
            return Err(AudioProcessorHostError::InvalidContract(
                "native state seed changed the prepared factory contract".to_owned(),
            ));
        }
        let before = self.input_names()?;
        let after = isolate_adapter_value("native seed auxiliary-input name query", || {
            factory.auxiliary_input_names()
        })?;
        crate::AudioProcessorInputName::validate_all(&after, &auxiliary_inputs)?;
        if before != after {
            return Err(AudioProcessorHostError::InvalidContract(
                "native state seed changed the prepared auxiliary-input names".to_owned(),
            ));
        }
        Ok(Self {
            contract,
            auxiliary_inputs,
            auxiliary_outputs,
            note_inputs,
            factory,
        })
    }

    pub(crate) fn shared_factory(&self) -> Arc<dyn AudioProcessorFactory> {
        Arc::clone(&self.factory)
    }

    pub(crate) fn native_reprepare_factory_bound(
        &self,
        identity: crate::NativeConfigurationSeedTicket,
        bytes: usize,
        inputs: Option<&crate::ClapAudioInputProposal>,
    ) -> Option<usize> {
        catch_unwind(AssertUnwindSafe(|| {
            self.factory.native_reprepare_factory_bound(identity, bytes, inputs)
        }))
        .ok()
        .flatten()
    }

    pub(crate) fn reprepare_factory(
        &self,
        identity: crate::NativeConfigurationSeedTicket,
        bytes: &[u8],
        inputs: Option<&crate::ClapAudioInputProposal>,
        report_peak: &mut dyn FnMut(usize),
    ) -> Result<Option<Arc<dyn AudioProcessorFactory>>, AudioProcessorHostError> {
        isolate_adapter_call("execution-only restart Factory", || {
            self.factory
                .with_native_reprepare_configuration(identity, bytes, inputs, report_peak)
        })
    }

    fn create(
        &self,
        native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    ) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        let current_contract = isolate_adapter_value("execution-contract revalidation", || {
            self.factory.execution_contract()
        })?;
        if current_contract != self.contract {
            return Err(AudioProcessorHostError::InvalidContract(
                "prepared factory changed its execution contract before Session creation"
                    .to_owned(),
            ));
        }
        let current_auxiliary_inputs =
            isolate_adapter_value("auxiliary-input contract revalidation", || {
                self.factory.auxiliary_input_contract()
            })?;
        if current_auxiliary_inputs != self.auxiliary_inputs {
            return Err(AudioProcessorHostError::InvalidContract(
                "prepared factory changed its auxiliary-input contract before Session creation"
                    .to_owned(),
            ));
        }
        let current_auxiliary_outputs =
            isolate_adapter_value("auxiliary-output contract revalidation", || {
                self.factory.auxiliary_output_contract()
            })?;
        if current_auxiliary_outputs != self.auxiliary_outputs {
            return Err(AudioProcessorHostError::InvalidContract(
                "prepared factory changed its auxiliary-output contract before Session creation"
                    .to_owned(),
            ));
        }
        let current_note_inputs =
            isolate_adapter_value("Note input contract revalidation", || {
                self.factory.note_input_contract()
            })?;
        if current_note_inputs != self.note_inputs {
            return Err(AudioProcessorHostError::InvalidContract(
                "prepared factory changed its Note input contract before Session creation"
                    .to_owned(),
            ));
        }
        if let Some(registrar) = native_child_settlement
            && self.native_child_settlement_requirement()
                == mondrian_core::NativeChildSettlementRequirement::Unavailable
        {
            registrar.mark_unavailable();
        }
        isolate_adapter_call("instance creation", || {
            self.factory.create(native_child_settlement)
        })
    }
}

impl fmt::Debug for PreparedProcessorFactoryBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedProcessorFactoryBinding")
            .field("contract", &self.contract)
            .field("auxiliary_inputs", &self.auxiliary_inputs)
            .field("auxiliary_outputs", &self.auxiliary_outputs)
            .field("note_inputs", self.note_inputs())
            .finish_non_exhaustive()
    }
}

struct MainBusAudioIo<'a> {
    channel_layout: AudioChannelLayout,
    frames: usize,
    pcm: &'a mut [f32],
    auxiliary_buses: &'a [PreparedProcessorAuxiliaryBus],
    auxiliary_pcm: &'a [f32],
    notes: Option<NoteBlockIo<'a>>,
    output_buses: &'a [crate::schedule::PreparedProcessorOutputBus],
    output_pcm: &'a mut [Vec<f32>],
}

struct NoteBlockIo<'a> {
    inputs: &'a [crate::AudioNoteEvent],
    ends: &'a mut Vec<crate::AudioNoteEndEvent>,
    end_limit: usize,
    ports: usize,
    refused: &'a mut bool,
}

impl AudioProcessorAudioIo for MainBusAudioIo<'_> {
    fn note_events(&self) -> &[crate::AudioNoteEvent] {
        self.notes.as_ref().map_or(&[], |notes| notes.inputs)
    }

    fn try_push_note_end(&mut self, end: crate::AudioNoteEndEvent) -> bool {
        let Some(notes) = &mut self.notes else {
            return false;
        };
        if *notes.refused || !end.valid(notes.ports) || notes.ends.len() >= notes.end_limit {
            *notes.refused = true;
            return false;
        }
        notes.ends.push(end);
        true
    }

    fn main_layout(&self) -> AudioChannelLayout {
        self.channel_layout
    }

    fn frames(&self) -> usize {
        self.frames
    }

    fn main_interleaved(&mut self) -> &mut [f32] {
        self.pcm
    }

    fn auxiliary_output(&mut self, bus_key: &str) -> Option<crate::AudioProcessorOutputBus<'_>> {
        let bus = self.output_buses.iter().find(|bus| bus.bus_key == bus_key)?;
        let samples = self.frames.checked_mul(self.channel_layout.channel_count())?;
        Some(crate::AudioProcessorOutputBus {
            bus_key: &bus.bus_key,
            channel_layout: self.channel_layout,
            frames: self.frames,
            interleaved: self.output_pcm.get_mut(bus.buffer_slot)?.get_mut(..samples)?,
        })
    }

    fn auxiliary_input(&self, bus_key: &str) -> Option<AudioProcessorInputBus<'_>> {
        let bus = self.auxiliary_buses.iter().find(|bus| bus.bus_key == bus_key)?;
        Some(AudioProcessorInputBus {
            bus_key: &bus.bus_key,
            channel_layout: bus.channel_layout,
            frames: self.frames,
            interleaved: self.auxiliary_pcm.get(bus.sample_range(self.frames)?)?,
        })
    }

    fn main_and_auxiliary_input(
        &mut self,
        bus_key: &str,
    ) -> Option<AudioProcessorMainAndInputBuses<'_>> {
        let bus = self.auxiliary_buses.iter().find(|bus| bus.bus_key == bus_key)?;
        let auxiliary = AudioProcessorInputBus {
            bus_key: &bus.bus_key,
            channel_layout: bus.channel_layout,
            frames: self.frames,
            interleaved: self.auxiliary_pcm.get(bus.sample_range(self.frames)?)?,
        };
        Some(AudioProcessorMainAndInputBuses {
            main_layout: self.channel_layout,
            frames: self.frames,
            main_interleaved: self.pcm,
            auxiliary,
        })
    }
}

struct HostedProcessorInstance {
    occurrence: AudioProcessorOccurrence,
    processor: Box<dyn AudioProcessor>,
    continuity: HostedProcessorContinuity,
    notes: Option<Box<notes::HostedNoteState>>,
    native_binding: crate::native_runtime_owner::NativeHostedBinding,
    native_author_outstanding: Option<native_author::NativeHostedAuthorDelivery>,
    // Actual committed child seed owner, not callback/install authority.
    native_reprepare_seed_bytes: usize,
    native_editor_detached: Option<mondrian_core::NativeAudioEditorTarget>,
    native_editor_attachment: Option<(
        crate::AudioNativeProcessorOwnerLease,
        crate::NativeAudioObjectObservation,
    )>,
}

#[derive(Debug, Clone, Copy)]
enum HostedProcessorContinuity {
    Stateless,
    Unentered,
    Failed,
    Pending {
        epoch: crate::AudioContinuityEpoch,
    },
    Active {
        epoch: crate::AudioContinuityEpoch,
        next_sample: i64,
    },
}

impl HostedProcessorContinuity {
    fn native_epoch(
        self,
    ) -> Result<Option<crate::AudioContinuityEpoch>, crate::AudioNativeOwnerError> {
        match self {
            Self::Stateless | Self::Unentered => Ok(None),
            Self::Pending { epoch } | Self::Active { epoch, .. } => Ok(Some(epoch)),
            Self::Failed => Err(crate::AudioNativeOwnerError::ContinuityUnavailable),
        }
    }
}

pub(crate) struct PreparedProcessorHost {
    musical_time_reference: Option<mondrian_core::MusicalTimeReference>,
    instances: Vec<HostedProcessorInstance>,
    entry_binding: Option<(
        crate::native_runtime_owner::NativeStateEntryBindingContext,
        crate::AudioSessionCursorIdentity,
    )>,
    parameter_events: ProcessorParameterEventScratch,
    render_contract: AudioRenderContract,
    maximum_parameter_lanes: usize,
    parameter_event_capacity: usize,
    session_scratch_bytes: usize,
    auxiliary_pcm: Vec<f32>,
    sidechain_frame_db: Vec<f64>,
    sidechain_gains: Vec<f32>,
    sidechain_mix_pcm: Vec<f32>,
    prepared_notes: Vec<Option<Box<notes::HostedNoteState>>>,
}

impl PreparedProcessorHost {
    pub(crate) fn swap_parameter_events(
        &mut self,
        next: &mut crate::processor_parameters::ProcessorParameterEventScratch,
        maximum_lanes: usize,
        event_capacity: usize,
    ) {
        self.maximum_parameter_lanes = maximum_lanes;
        self.parameter_event_capacity = event_capacity;
        std::mem::swap(&mut self.parameter_events, next)
    }

    pub(crate) fn native_observation_sources(&self) -> usize {
        self.instances
            .iter()
            .filter(|instance| instance.processor.native_observations_supported())
            .count()
    }

    pub(crate) fn poll_native_observation_at(
        &mut self,
        index: usize,
        root: crate::AudioSessionCursorIdentity,
        local: crate::AudioSessionCursorIdentity,
        sequence: mondrian_core::SequenceId,
        installed_serial: Option<std::num::NonZeroU64>,
        cancellation: &mondrian_core::ExecutionCancellationToken,
    ) -> Result<Option<mondrian_core::NativeAudioObservationBatch>, crate::AudioProcessorHostError>
    {
        if cancellation.is_canceled() {
            return Ok(None);
        }
        let instance = self
            .instances
            .iter_mut()
            .filter(|instance| instance.processor.native_observations_supported())
            .nth(index)
            .ok_or_else(|| {
                crate::AudioProcessorHostError::InvalidContract(
                    "native observation occurrence changed".to_owned(),
                )
            })?;
        let installed = match instance.native_binding {
            crate::native_runtime_owner::NativeHostedBinding::Installed { lease, object }
                if lease.address().root == root
                    && lease.address().local == local
                    && lease.address().occurrence == instance.occurrence
                    && lease.address().epoch
                        == instance.continuity.native_epoch().map_err(|error| {
                            crate::AudioProcessorHostError::InvalidContract(error.to_string())
                        })?
                    && Some(lease.installed_root_serial()) == installed_serial =>
            {
                Some((lease, object))
            }
            _ => None,
        };
        let consumption = match installed {
            Some((lease, object)) if instance.native_author_outstanding.is_none() => {
                crate::NativeAudioObservationConsumption::DrainAuthor(
                    crate::NativeAudioObjectBindRequest {
                        binding: lease.binding(),
                        expected_object: object,
                    },
                )
            }
            _ => crate::NativeAudioObservationConsumption::Drain,
        };
        let mut observations = isolate_adapter_call("native observation", || {
            instance.processor.poll_native_observations(consumption)
        })?;
        let author = observations.author.take();
        let proposal = match installed {
            Some((lease, object)) => {
                if observations.object != Some(object) {
                    instance.native_editor_attachment = None;
                    instance.native_binding =
                        crate::native_runtime_owner::NativeHostedBinding::Unbound;
                    return Err(crate::AudioProcessorHostError::InvalidContract(
                        "native author object changed".to_owned(),
                    ));
                }
                if let Some(author) = author {
                    author.source.validate().map_err(|reason| {
                        crate::AudioProcessorHostError::InvalidContract(format!(
                            "native author source refused: {reason:?}"
                        ))
                    })?;
                    if author.source.failure().is_some() {
                        return Err(crate::AudioProcessorHostError::InvalidContract(
                            "native author source is lifetime-incomplete".to_owned(),
                        ));
                    }
                    let expected = crate::NativeAudioObjectBindRequest {
                        binding: lease.binding(),
                        expected_object: object,
                    };
                    match author.delivery {
                        crate::NativeAudioAuthorDeliveryState::Delivered(delivery) => {
                            delivery.validate(author.source).map_err(|reason| {
                                crate::AudioProcessorHostError::InvalidContract(format!(
                                    "native author delivery refused: {reason:?}"
                                ))
                            })?;
                            if instance.native_author_outstanding.is_some()
                                || delivery.binding() != expected
                            {
                                return Err(crate::AudioProcessorHostError::InvalidContract(
                                    "native author delivery binding changed".to_owned(),
                                ));
                            }
                            let request_cohort = delivery.request_cohort();
                            let (_, revision, events, configuration, completion) =
                                delivery.into_parts();
                            let mut callbacks = Vec::with_capacity(events.len());
                            for event in events {
                                callbacks.push(match event {
                                    crate::NativeAudioAuthorEvent::Parameter(event) => mondrian_core::NativeAudioAuthorCallback::Parameter {
                                        parameter: event.parameter,
                                        phase: match event.phase {
                                            crate::NativeAudioAuthorParameterPhase::Begin => mondrian_core::NativeAudioParameterCallbackPhase::Begin,
                                            crate::NativeAudioAuthorParameterPhase::Value => mondrian_core::NativeAudioParameterCallbackPhase::Value,
                                            crate::NativeAudioAuthorParameterPhase::End => mondrian_core::NativeAudioParameterCallbackPhase::End,
                                        },
                                        value_bits: event.value.map(crate::NativeAudioAuthorScalar::bits),
                                    },
                                    crate::NativeAudioAuthorEvent::ParameterAtExecution { parameter: event, sample_position, sample_rate } => mondrian_core::NativeAudioAuthorCallback::ParameterAtExecution {
                                        parameter: event.parameter,
                                        phase: match event.phase {
                                            crate::NativeAudioAuthorParameterPhase::Begin => mondrian_core::NativeAudioParameterCallbackPhase::Begin,
                                            crate::NativeAudioAuthorParameterPhase::Value => mondrian_core::NativeAudioParameterCallbackPhase::Value,
                                            crate::NativeAudioAuthorParameterPhase::End => mondrian_core::NativeAudioParameterCallbackPhase::End,
                                        },
                                        value_bits: event.value.map(crate::NativeAudioAuthorScalar::bits),
                                        position: mondrian_core::DomainTime::new(
                                            mondrian_core::AuthoringTimeDomain::Sequence(sequence),
                                            mondrian_core::TimelineTime::new(sample_position, i64::from(sample_rate))
                                                .map_err(|_| crate::AudioProcessorHostError::NativeParameterSourceUnavailable)?,
                                        ),
                                    },
                                    crate::NativeAudioAuthorEvent::ParameterValuesRescanned { kind, values } => {
                                        let limit = mondrian_core::NATIVE_AUDIO_AUTHOR_RESCAN_VALUES;
                                        if values.len() > limit || values.capacity() > limit {
                                            return Err(crate::AudioProcessorHostError::InvalidContract(
                                                mondrian_core::NativeAudioAuthorProjectionError::Extent.to_string(),
                                            ));
                                        }
                                        // A fallible collect can recycle the larger native-event
                                        // allocation as tuples and inflate its element capacity.
                                        // Own a bounded tuple allocation instead. It overlaps the
                                        // source briefly on this non-RT poll; no full startup/RT
                                        // aggregate resource grant is asserted by this projection.
                                        let mut projected = Vec::new();
                                        projected.try_reserve_exact(values.len()).map_err(|_| {
                                            crate::AudioProcessorHostError::NativeParameterSourceUnavailable
                                        })?;
                                        if projected.capacity() > limit {
                                            return Err(crate::AudioProcessorHostError::InvalidContract(
                                                mondrian_core::NativeAudioAuthorProjectionError::Extent.to_string(),
                                            ));
                                        }
                                        for event in values {
                                            let scalar = event.value.ok_or(
                                                crate::AudioProcessorHostError::NativeParameterSourceUnavailable,
                                            )?;
                                            projected.push((event.parameter, scalar.bits()));
                                        }
                                        mondrian_core::NativeAudioAuthorCallback::ParameterValuesRescanned {
                                            kind, values: projected,
                                        }
                                    },
                                    crate::NativeAudioAuthorEvent::ParameterReferencesCleared { parameter, flags } => mondrian_core::NativeAudioAuthorCallback::ParameterReferencesCleared { parameter, flags },
                                    crate::NativeAudioAuthorEvent::DirtyChanged(value) => mondrian_core::NativeAudioAuthorCallback::DirtyChanged(value),
                                    crate::NativeAudioAuthorEvent::GroupStarted => mondrian_core::NativeAudioAuthorCallback::GroupStarted,
                                    crate::NativeAudioAuthorEvent::GroupFinished => mondrian_core::NativeAudioAuthorCallback::GroupFinished,
                                });
                            }
                            let proposal = mondrian_core::NativeAudioAuthorProposal::project(
                                lease.editor_target(),
                                sequence,
                                revision,
                                mondrian_core::NativeAudioAuthorBoundaryFacts {
                                    completion,
                                    open_parameter_edits: author.source.open_parameter_edits(),
                                    group_depth: author.source.group_depth(),
                                },
                                callbacks,
                                configuration,
                            )
                            .map_err(|error| {
                                crate::AudioProcessorHostError::InvalidContract(error.to_string())
                            })?;
                            instance.native_author_outstanding =
                                Some(native_author::NativeHostedAuthorDelivery::new(
                                    proposal,
                                    request_cohort,
                                ));
                        }
                        crate::NativeAudioAuthorDeliveryState::AwaitingAcknowledgement {
                            binding,
                            revision,
                            request_cohort,
                        } if instance.native_author_outstanding.as_ref().is_some_and(
                            |retained| {
                                retained.matches_source_and_current(
                                    binding,
                                    revision,
                                    request_cohort,
                                    lease.editor_target(),
                                    object,
                                )
                            },
                        ) => {}
                        crate::NativeAudioAuthorDeliveryState::Preserved
                            if instance.native_author_outstanding.is_none() => {}
                        crate::NativeAudioAuthorDeliveryState::Refused(
                            crate::NativeAudioAuthorRefusal::DspFeedbackPending,
                        ) if author.source.pending_dsp_feedback_values() != 0
                            && instance.native_author_outstanding.is_none() =>
                        {
                            // Main may publish a newer Value after the joined
                            // zero-sample DSP catchup. No event has been drained
                            // by this reply. Keep the actual source for the next
                            // fair serial Poll; PCM/lifecycle still use their
                            // independent current typed source admission.
                        }
                        crate::NativeAudioAuthorDeliveryState::Refused(reason) => {
                            return Err(crate::AudioProcessorHostError::InvalidContract(format!(
                                "native author producer refused: {reason:?}"
                            )));
                        }
                        _ => {
                            return Err(crate::AudioProcessorHostError::InvalidContract(
                                "native author outstanding receipt mismatch".to_owned(),
                            ))
                        }
                    }
                } else if instance.native_author_outstanding.is_some() {
                    return Err(crate::AudioProcessorHostError::InvalidContract(
                        "native author source disappeared with outstanding work".to_owned(),
                    ));
                }
                instance
                    .native_author_outstanding
                    .as_ref()
                    .map(|retained| retained.proposal.clone())
            }
            None => None,
        };
        let parameters = match installed {
            Some((lease, object)) if !observations.parameters.is_empty() => Some(
                mondrian_core::NativeAudioParameterObservation::project(
                    lease.editor_target(),
                    object.process_id(),
                    object.object_serial(),
                    observations.parameter_revision,
                    std::mem::take(&mut observations.parameters),
                )
                .map_err(|error| {
                    crate::AudioProcessorHostError::InvalidContract(error.to_string())
                })?,
            ),
            _ => None,
        };
        // Retain the actual author result before cancellation/completion gates.
        // A cancelled or stale completion cannot erase an unaccepted callback.
        let diagnostics = crate::native_diagnostic_projection::project(
            root,
            local,
            instance.occurrence,
            observations,
        )?;
        if cancellation.is_canceled() {
            return Ok(None);
        }
        let attachment = installed.and_then(|(lease, object)| {
            if let Some((prior, attached)) = instance.native_editor_attachment
                && attached == object
                && prior.editor_target() != lease.editor_target()
            {
                return Some(mondrian_core::NativeAudioEditorAttachmentObservation {
                    previous: prior.editor_target(),
                    current: lease.editor_target(),
                    process_id: object.process_id(),
                    object_serial: object.object_serial(),
                    retained: true,
                });
            }
            instance.native_editor_detached.map(|previous| {
                mondrian_core::NativeAudioEditorAttachmentObservation {
                    previous,
                    current: lease.editor_target(),
                    process_id: object.process_id(),
                    object_serial: object.object_serial(),
                    retained: false,
                }
            })
        });
        let batch = mondrian_core::NativeAudioObservationBatch {
            diagnostics,
            author: proposal,
            parameters,
            attachment,
        };
        Ok((!batch.is_empty_complete()).then_some(batch))
    }

    pub(crate) fn validate_native_author_source_closed(
        &mut self,
        root: crate::AudioSessionCursorIdentity,
        local: crate::AudioSessionCursorIdentity,
        installed_serial: std::num::NonZeroU64,
        cancellation: &mondrian_core::ExecutionCancellationToken,
    ) -> Result<(), crate::NativeAudioAuthorAdmissionError> {
        for instance in &mut self.instances {
            if cancellation.is_canceled() {
                return Err(crate::NativeAudioAuthorAdmissionError::Cancelled);
            }
            if !instance.processor.native_observations_supported() {
                continue;
            }
            let (lease, object) = match instance.native_binding {
                crate::native_runtime_owner::NativeHostedBinding::Installed { lease, object }
                    if lease.address().root == root
                        && lease.address().local == local
                        && lease.address().occurrence == instance.occurrence
                        && lease.address().epoch == instance.continuity.native_epoch()?
                        && lease.installed_root_serial() == installed_serial =>
                {
                    (lease, object)
                }
                _ => return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into()),
            };
            let observations =
                isolate_adapter_call("native author closed-source admission", || {
                    instance.processor.preflight_native_observations(object)
                })?;
            // Cancellation wins even when a foreign callback made the reply
            // incomplete. No consumed source or author mutation occurs here.
            if cancellation.is_canceled() {
                return Err(crate::NativeAudioAuthorAdmissionError::Cancelled);
            }
            if observations.object != Some(object) {
                instance.native_binding = crate::native_runtime_owner::NativeHostedBinding::Unbound;
                return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
            }
            crate::native_audio_author_runtime::closed_source(
                observations.author.as_ref(),
                instance.native_author_outstanding.is_some(),
            )
            .map_err(|reason| crate::NativeAudioAuthorAdmissionError::Pending {
                target: Box::new(lease.editor_target()),
                reason,
            })?;
        }
        Ok(())
    }

    pub(crate) fn set_native_entry_context(
        &mut self,
        context: Option<crate::native_runtime_owner::NativeStateEntryBindingContext>,
        local: crate::AudioSessionCursorIdentity,
    ) {
        self.entry_binding = context.map(|context| (context, local));
    }

    pub(crate) fn revoke_native_bindings_for_entry(&mut self) {
        // Retain only cleanup identity for a view that may survive its actual
        // first signal. This receipt grants neither Open nor author permission.
        for instance in &mut self.instances {
            instance.native_binding = crate::native_runtime_owner::NativeHostedBinding::Unbound;
        }
    }

    pub(crate) fn revoke_native_bindings(&mut self) {
        self.clear_note_entry_permits();
        for instance in &mut self.instances {
            instance.native_editor_attachment = None;
            instance.native_editor_detached = None;
            instance.native_binding = crate::native_runtime_owner::NativeHostedBinding::Unbound;
        }
    }

    fn prepare_native_binding(
        instance: &mut HostedProcessorInstance,
        root: crate::AudioSessionCursorIdentity,
        local: crate::AudioSessionCursorIdentity,
    ) -> Result<bool, crate::AudioNativeOwnerError> {
        // Observation/publication rebind cannot carry a lifecycle transaction.
        notes::clear_pending_entry(instance);
        let Some(object) = isolate_adapter_call("native object observation", || {
            instance.processor.native_audio_object_observation()
        })?
        else {
            return Ok(false);
        };
        let epoch = instance.continuity.native_epoch()?;
        let request = crate::NativeAudioObjectBindRequest {
            binding: mondrian_core::AudioProcessorBindingId::new(),
            expected_object: object,
        };
        let receipt = match isolate_adapter_call("native object binding", || {
            instance.processor.bind_native_audio_object(request)
        })? {
            crate::NativeAudioObjectBindOutcome::Bound(receipt)
                if receipt.binding() == request.binding && receipt.object() == object =>
            {
                receipt
            }
            crate::NativeAudioObjectBindOutcome::Bound(_) => {
                return Err(crate::AudioNativeOwnerError::BindingRefused(
                    crate::NativeAudioObjectBindRefusal::ObjectChanged,
                ))
            }
            crate::NativeAudioObjectBindOutcome::Refused(reason) => {
                return Err(crate::AudioNativeOwnerError::BindingRefused(reason))
            }
        };
        instance.native_binding = crate::native_runtime_owner::NativeHostedBinding::Prepared {
            address: crate::AudioNativeProcessorAddress {
                root,
                local,
                occurrence: instance.occurrence,
                epoch,
            },
            receipt,
        };
        Ok(true)
    }

    fn bind_entered_instance(
        instance: &mut HostedProcessorInstance,
        context: Option<&(
            crate::native_runtime_owner::NativeStateEntryBindingContext,
            crate::AudioSessionCursorIdentity,
        )>,
        cancellation: Option<&mondrian_core::ExecutionCancellationToken>,
    ) -> Result<(), crate::AudioNativeOwnerError> {
        let Some((context, local)) = context else {
            return Ok(());
        };
        context.validate(cancellation)?;
        if !Self::prepare_native_binding(instance, context.root, *local)? {
            return Ok(());
        }
        if let Err(error) = context.validate(cancellation) {
            instance.native_binding = crate::native_runtime_owner::NativeHostedBinding::Unbound;
            return Err(error);
        }
        if let crate::native_runtime_owner::NativeHostedBinding::Prepared { address, receipt } =
            instance.native_binding
        {
            instance.native_binding = crate::native_runtime_owner::NativeHostedBinding::Installed {
                lease: crate::AudioNativeProcessorOwnerLease::issue(
                    address,
                    receipt.binding(),
                    context.claim.generation(),
                ),
                object: receipt.object(),
            };
            if let Some((prior, object)) = instance.native_editor_attachment
                && object != receipt.object()
            {
                instance.native_editor_detached = Some(prior.editor_target());
                instance.native_editor_attachment = None;
            }
        }
        Ok(())
    }

    /// Bind observation/control permission on existing local objects. This
    /// never enters/reset DSP or supplies an execution position. The next real
    /// local Enter revokes and rebinds its actual new continuity epoch.
    pub(crate) fn bind_native_observation_owners(
        &mut self,
        cancellation: &mondrian_core::ExecutionCancellationToken,
    ) -> Result<(), crate::AudioNativeOwnerError> {
        let context = self.entry_binding.clone();
        for instance in &mut self.instances {
            if let Some((context, _)) = &context {
                context.validate(Some(cancellation))?;
            }
            if cancellation.is_canceled() {
                return Err(crate::AudioNativeOwnerError::Cancelled);
            }
            if matches!(
                instance.native_binding,
                crate::native_runtime_owner::NativeHostedBinding::Unbound
            ) {
                Self::bind_entered_instance(instance, context.as_ref(), Some(cancellation))?;
            }
        }
        Ok(())
    }

    pub(crate) fn prepare_native_bindings(
        &mut self,
        root: crate::AudioSessionCursorIdentity,
        local: crate::AudioSessionCursorIdentity,
        cancellation: &mondrian_core::ExecutionCancellationToken,
    ) -> Result<usize, crate::AudioNativeOwnerError> {
        let mut prepared = 0usize;
        for instance in &mut self.instances {
            if cancellation.is_canceled() {
                return Err(crate::AudioNativeOwnerError::Cancelled);
            }
            if !Self::prepare_native_binding(instance, root, local)? {
                continue;
            }
            instance.native_editor_attachment = None;
            prepared = prepared
                .checked_add(1)
                .ok_or(crate::AudioNativeOwnerError::RecordExtentOverflow)?;
        }
        Ok(prepared)
    }

    pub(crate) fn validate_prepared_native_bindings(
        &self,
        root: crate::AudioSessionCursorIdentity,
        local: crate::AudioSessionCursorIdentity,
    ) -> Result<usize, crate::AudioNativeOwnerError> {
        let mut prepared = 0usize;
        for instance in &self.instances {
            match instance.native_binding {
                crate::native_runtime_owner::NativeHostedBinding::Prepared { address, .. } => {
                    if address.root != root
                        || address.local != local
                        || address.occurrence != instance.occurrence
                        || address.epoch != instance.continuity.native_epoch()?
                    {
                        return Err(crate::AudioNativeOwnerError::OwnerUnavailable);
                    }
                    prepared = prepared
                        .checked_add(1)
                        .ok_or(crate::AudioNativeOwnerError::RecordExtentOverflow)?;
                }
                crate::native_runtime_owner::NativeHostedBinding::Installed { .. } => {
                    return Err(crate::AudioNativeOwnerError::OwnerUnavailable)
                }
                crate::native_runtime_owner::NativeHostedBinding::Unbound => {}
            }
        }
        Ok(prepared)
    }

    pub(crate) fn publish_native_bindings(&mut self, installed_root_serial: std::num::NonZeroU64) {
        // All foreign acknowledgements and cancellation/claim checks precede
        // this bounded record move. It allocates nothing and performs no DSP.
        for instance in &mut self.instances {
            if let crate::native_runtime_owner::NativeHostedBinding::Prepared { address, receipt } =
                instance.native_binding
            {
                instance.native_editor_attachment = None;
                instance.native_binding =
                    crate::native_runtime_owner::NativeHostedBinding::Installed {
                        lease: crate::AudioNativeProcessorOwnerLease::issue(
                            address,
                            receipt.binding(),
                            installed_root_serial,
                        ),
                        object: receipt.object(),
                    };
            }
        }
    }

    pub(crate) fn native_owner_lease(
        &self,
        address: crate::AudioNativeProcessorAddress,
    ) -> Option<crate::AudioNativeProcessorOwnerLease> {
        self.instances.iter().find_map(|instance| match instance.native_binding {
            crate::native_runtime_owner::NativeHostedBinding::Installed { lease, .. }
                if lease.address() == address =>
            {
                Some(lease)
            }
            _ => None,
        })
    }

    pub(crate) fn resolve_native_editor_target(
        &self,
        locator: mondrian_core::NativeAudioEditorLocator,
        root: crate::AudioSessionCursorIdentity,
        local: crate::AudioSessionCursorIdentity,
        installed_serial: std::num::NonZeroU64,
    ) -> Result<Option<mondrian_core::NativeAudioEditorTarget>, crate::AudioNativeOwnerError> {
        let mut found = None;
        for instance in &self.instances {
            if let crate::native_runtime_owner::NativeHostedBinding::Installed { lease, .. } =
                instance.native_binding
            {
                let target = lease.editor_target();
                if lease.address().root == root
                    && lease.address().local == local
                    && lease.address().occurrence == instance.occurrence
                    && lease.address().epoch == instance.continuity.native_epoch()?
                    && lease.installed_root_serial() == installed_serial
                    && locator.matches_target(target)
                    && found.replace(target).is_some()
                {
                    return Err(crate::AudioNativeOwnerError::OwnerUnavailable);
                }
            }
        }
        Ok(found)
    }

    pub(crate) fn validate_native_owner(
        &mut self,
        lease: crate::AudioNativeProcessorOwnerLease,
    ) -> Result<crate::NativeAudioObjectObservation, crate::AudioNativeOwnerError> {
        let instance = self
            .instances
            .iter_mut()
            .find(|instance| instance.occurrence == lease.address().occurrence)
            .ok_or(crate::AudioNativeOwnerError::OwnerUnavailable)?;
        let object = match instance.native_binding {
            crate::native_runtime_owner::NativeHostedBinding::Installed {
                lease: current,
                object,
            } if lease == current => object,
            _ => return Err(crate::AudioNativeOwnerError::OwnerUnavailable),
        };
        if isolate_adapter_call("native owner validation", || {
            instance.processor.native_audio_object_observation()
        })? != Some(object)
        {
            instance.native_editor_attachment = None;
            instance.native_editor_detached = None;
            instance.native_binding = crate::native_runtime_owner::NativeHostedBinding::Unbound;
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable);
        }
        Ok(object)
    }

    pub(crate) fn retained_native_close_lease(
        &self,
        previous: mondrian_core::NativeAudioEditorTarget,
        root: crate::AudioSessionCursorIdentity,
        local: crate::AudioSessionCursorIdentity,
        serial: std::num::NonZeroU64,
    ) -> Option<crate::AudioNativeProcessorOwnerLease> {
        self.instances.iter().find_map(|instance| {
            let (lease, object) = match instance.native_binding {
                crate::native_runtime_owner::NativeHostedBinding::Installed { lease, object }
                    if lease.address().root == root
                        && lease.address().local == local
                        && lease.installed_root_serial() == serial
                        && lease.address().occurrence == instance.occurrence
                        && instance.continuity.native_epoch().ok()
                            == Some(lease.address().epoch) =>
                {
                    (lease, object)
                }
                _ => return None,
            };
            if lease.editor_target() == previous {
                return Some(lease);
            }
            let (prior, prior_object) = instance.native_editor_attachment?;
            (prior.editor_target() == previous
                && prior_object == object
                && prior.address().root.session_id == root.session_id
                && prior.address().local.session_id == local.session_id
                && prior.address().occurrence == lease.address().occurrence)
                .then_some(lease)
        })
    }

    pub(crate) fn query_native_parameter(
        &mut self,
        lease: crate::AudioNativeProcessorOwnerLease,
        parameter: &mondrian_core::ParameterId,
        native_id: u32,
    ) -> Result<mondrian_core::NativeAudioParameterQueryOutcome, crate::AudioNativeOwnerError> {
        use mondrian_core::{
            NativeAudioParameterQueryOutcome as Out, NativeAudioParameterQueryRefusal as Refusal,
        };
        let object = match self.validate_native_owner(lease) {
            Ok(object) => object,
            Err(crate::AudioNativeOwnerError::OwnerUnavailable) => {
                return Ok(Out::Refused(Refusal::OwnerChanged))
            }
            Err(error) => return Err(error),
        };
        let instance = self
            .instances
            .iter_mut()
            .find(|instance| instance.occurrence == lease.address().occurrence)
            .ok_or(crate::AudioNativeOwnerError::OwnerUnavailable)?;
        if instance.native_author_outstanding.is_some() || instance.native_editor_detached.is_some()
        {
            return Ok(Out::Refused(Refusal::Busy));
        }
        let binding = crate::NativeAudioObjectBindRequest {
            binding: lease.binding(),
            expected_object: object,
        };
        let result = isolate_adapter_call("native parameter query", || {
            instance.processor.query_native_parameter(binding, native_id)
        });
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                instance.native_binding = crate::native_runtime_owner::NativeHostedBinding::Unbound;
                return Err(error.into());
            }
        };
        let (revision, facts) = match result {
            Ok(current) => current,
            Err(refusal) => return Ok(Out::Refused(refusal)),
        };
        if facts.len() != 1
            || facts[0].parameter != *parameter
            || facts[0].revision != revision
            || facts[0].event_flags != 0
            || facts[0].origin
                != (mondrian_core::NativeAudioParameterObservationOrigin::MainQuery {
                    rescan_flags: 0,
                })
        {
            instance.native_binding = crate::native_runtime_owner::NativeHostedBinding::Unbound;
            return Err(crate::AudioNativeOwnerError::Processor(
                AudioProcessorHostError::InvalidContract(
                    "native parameter query did not return its exact selected ID".into(),
                ),
            ));
        }
        let observation = match mondrian_core::NativeAudioParameterObservation::project(
            lease.editor_target(),
            object.process_id(),
            object.object_serial(),
            revision,
            facts,
        ) {
            Ok(observation) => observation,
            Err(error) => {
                instance.native_binding = crate::native_runtime_owner::NativeHostedBinding::Unbound;
                return Err(crate::AudioNativeOwnerError::Processor(
                    AudioProcessorHostError::InvalidContract(error.to_string()),
                ));
            }
        };
        Ok(Out::Observed(Box::new(observation)))
    }
    pub(crate) fn control_native_editor(
        &mut self,
        lease: crate::AudioNativeProcessorOwnerLease,
        title: Option<mondrian_core::NativeAudioEditorTitle>,
        static_parameter_values: Vec<(u32, u64)>,
    ) -> Result<crate::NativeAudioEditorOutcome, crate::AudioNativeOwnerError> {
        self.validate_native_owner(lease)?;
        let instance = self
            .instances
            .iter_mut()
            .find(|instance| instance.occurrence == lease.address().occurrence)
            .ok_or(crate::AudioNativeOwnerError::OwnerUnavailable)?;
        let object = match instance.native_binding {
            crate::native_runtime_owner::NativeHostedBinding::Installed {
                lease: current,
                object,
            } if current == lease => object,
            _ => return Err(crate::AudioNativeOwnerError::OwnerUnavailable),
        };
        let binding = crate::NativeAudioObjectBindRequest {
            binding: lease.binding(),
            expected_object: object,
        };
        let result = match title {
            Some(title) => isolate_adapter_call("native editor open", || {
                instance.processor.open_native_audio_editor(
                    crate::NativeAudioEditorOpenRequest::new(binding, title)
                        .with_static_parameter_values(static_parameter_values)?,
                )
            }),
            None => isolate_adapter_call("native editor close", || {
                instance.processor.close_native_audio_editor(binding)
            }),
        };
        match result {
            Ok(outcome) => {
                let actual = match outcome {
                    crate::NativeAudioEditorOutcome::Opened { binding, .. }
                    | crate::NativeAudioEditorOutcome::Closed { binding }
                    | crate::NativeAudioEditorOutcome::ClosePending { binding, .. } => {
                        Some(binding)
                    }
                    crate::NativeAudioEditorOutcome::Refused(_) => None,
                };
                if actual.is_some_and(|actual| actual != binding) {
                    instance.native_editor_attachment = None;
                    instance.native_binding =
                        crate::native_runtime_owner::NativeHostedBinding::Unbound;
                    return Err(crate::AudioNativeOwnerError::OwnerUnavailable);
                }
                match outcome {
                    crate::NativeAudioEditorOutcome::Opened { .. } => {
                        // A single actual view attachment receipt is stable
                        // over any number of same-object parameter Plan swaps.
                        instance.native_editor_attachment = Some((lease, object));
                        instance.native_editor_detached = None;
                    }
                    crate::NativeAudioEditorOutcome::Closed { .. } => {
                        instance.native_editor_attachment = None;
                        instance.native_editor_detached = None;
                    }
                    crate::NativeAudioEditorOutcome::Refused(_)
                    | crate::NativeAudioEditorOutcome::ClosePending { .. } => {}
                }
                Ok(outcome)
            }
            Err(error) => {
                instance.native_editor_attachment = None;
                instance.native_binding = crate::native_runtime_owner::NativeHostedBinding::Unbound;
                Err(error.into())
            }
        }
    }

    pub(crate) fn visit_continuity_evidence(
        &self,
        identity: crate::AudioSessionCursorIdentity,
        sample_rate: AudioSampleRate,
        visitor: &mut impl crate::AudioContinuityEvidenceVisitor,
    ) -> Result<(), AudioExecutionError> {
        for instance in &self.instances {
            let state = match instance.continuity {
                HostedProcessorContinuity::Stateless => crate::AudioProcessorCursorState::Stateless,
                HostedProcessorContinuity::Unentered => crate::AudioProcessorCursorState::Unentered,
                HostedProcessorContinuity::Failed => crate::AudioProcessorCursorState::Failed,
                HostedProcessorContinuity::Pending { epoch } => {
                    crate::AudioProcessorCursorState::WaitingForFirstSignal { epoch }
                }
                HostedProcessorContinuity::Active { epoch, next_sample } => {
                    crate::AudioProcessorCursorState::Active {
                        epoch,
                        next_input_signal_sample: mondrian_core::AudioSamplePosition::new(
                            next_sample,
                            sample_rate,
                        ),
                    }
                }
            };
            visitor.processor(crate::AudioProcessorCursorEvidence {
                identity,
                occurrence: instance.occurrence,
                state,
            })?;
        }
        Ok(())
    }

    /// Prepare only the canonical Session-owned host buffers. Native creation
    /// belongs to `populate_instances`, after all canonical Session storage checks.
    pub(crate) fn allocate_storage(
        schedule: &PreparedAudioSchedule,
        render_contract: AudioRenderContract,
    ) -> Result<Self, AudioExecutionError> {
        if !schedule.notes.matches_factory_contracts(&schedule.processors) {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        let maximum_parameter_lanes = schedule
            .processors
            .iter()
            .map(|processor| processor.parameter_ids.len())
            .max()
            .unwrap_or(0);
        if schedule
            .processors
            .iter()
            .any(|processor| processor.parameter_ids.len() != processor.parameter_curves.len())
        {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        let parameter_event_capacity = schedule.summary.maximum_parameter_events_per_block;
        let session_scratch_bytes = schedule
            .processors
            .iter()
            .try_fold(0_usize, |bytes, processor| {
                bytes.checked_add(processor.factory.contract().session_scratch_bytes())
            })
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        if session_scratch_bytes != schedule.summary.processor_session_scratch_bytes
            || session_scratch_bytes
                .checked_add(
                    Self::note_storage_bytes(schedule)
                        .map_err(|_| AudioExecutionError::BufferTooLarge)?,
                )
                .is_none_or(|bytes| bytes > render_contract.processor_session_scratch_budget_bytes)
        {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        let gain_samples = render_contract
            .max_block_frames
            .checked_mul(
                render_contract
                    .channel_count()
                    .max(schedule.summary.maximum_sidechain_mix_channels),
            )
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        // All Note storage is admitted and allocated before ANY native create.
        let mut prepared_notes = Vec::new();
        if !schedule.notes.is_empty() {
            prepared_notes
                .try_reserve_exact(schedule.processors.len())
                .map_err(|_| notes::refusal("Note occurrence storage unavailable"))?;
            if prepared_notes.capacity() != schedule.processors.len() {
                return Err(notes::refusal("Note occurrence allocation exceeds admission").into());
            }
            for (slot, processor) in schedule.processors.iter().enumerate() {
                prepared_notes.push(
                    if schedule.notes.targets().iter().any(|target| target.processor_slot == slot) {
                        notes::HostedNoteState::prepare(processor.factory.note_inputs())?
                    } else {
                        None
                    },
                );
            }
        }
        Ok(Self {
            instances: Vec::with_capacity(schedule.processors.len()),
            musical_time_reference: schedule.musical_time_reference,
            entry_binding: None,
            parameter_events: ProcessorParameterEventScratch::new(
                maximum_parameter_lanes,
                parameter_event_capacity,
            ),
            render_contract,
            maximum_parameter_lanes,
            parameter_event_capacity,
            session_scratch_bytes,
            auxiliary_pcm: vec![
                0.0;
                schedule
                    .summary
                    .maximum_processor_auxiliary_channels
                    .checked_mul(render_contract.max_block_frames)
                    .ok_or(AudioExecutionError::BufferTooLarge)?
            ],
            sidechain_frame_db: vec![0.0; render_contract.max_block_frames],
            sidechain_gains: vec![0.0; gain_samples],
            sidechain_mix_pcm: vec![
                0.0;
                schedule
                    .summary
                    .maximum_sidechain_mix_channels
                    .checked_mul(render_contract.max_block_frames)
                    .ok_or(AudioExecutionError::BufferTooLarge)?
            ],
            prepared_notes,
        })
    }

    /// Populate the already reserved occurrence storage exactly once. This
    /// private step invokes factories and may fail; it is never a commit guard.
    pub(crate) fn populate_instances(
        &mut self,
        schedule: &PreparedAudioSchedule,
        native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    ) -> Result<(), AudioExecutionError> {
        if !self.instances.is_empty() || self.instances.capacity() < schedule.processors.len() {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        for (slot, processor) in schedule.processors.iter().enumerate() {
            let continuity = if processor.factory.contract().requires_state_entry() {
                HostedProcessorContinuity::Unentered
            } else {
                HostedProcessorContinuity::Stateless
            };
            self.instances.push(HostedProcessorInstance {
                occurrence: processor.occurrence,
                processor: processor.factory.create(native_child_settlement)?,
                continuity,
                notes: self.prepared_notes.get_mut(slot).and_then(Option::take),
                native_binding: crate::native_runtime_owner::NativeHostedBinding::Unbound,
                native_author_outstanding: None,
                native_reprepare_seed_bytes: 0,
                native_editor_attachment: None,
                native_editor_detached: None,
            });
        }
        Ok(())
    }

    pub(crate) fn installed_native_child_settlement_requirement(
        &self,
        schedule: &PreparedAudioSchedule,
    ) -> mondrian_core::NativeChildSettlementRequirement {
        use mondrian_core::NativeChildSettlementRequirement as Requirement;
        if self.instances.len() != schedule.processors.len() {
            return Requirement::Unavailable;
        }
        let mut aggregate = Requirement::NotApplicable;
        for (instance, prepared) in self.instances.iter().zip(&schedule.processors) {
            if instance.occurrence != prepared.occurrence
                || matches!(instance.continuity, HostedProcessorContinuity::Failed)
            {
                return Requirement::Unavailable;
            }
            // Use the original panic-isolated factory declaration, never the
            // author format name, resolver default or absence of callbacks.
            match prepared.factory.native_child_settlement_requirement() {
                Requirement::Unavailable => return Requirement::Unavailable,
                Requirement::Required => aggregate = Requirement::Required,
                Requirement::NotApplicable => {}
            }
        }
        aggregate
    }

    pub(crate) fn bind_native_child_settlement(
        &mut self,
        schedule: &PreparedAudioSchedule,
        registrar: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<(), mondrian_core::NativeChildSettlementError> {
        use mondrian_core::{
            NativeChildSettlementBinding as Binding, NativeChildSettlementError as Error,
            NativeChildSettlementRequirement as Requirement,
        };
        if self.instances.len() != schedule.processors.len() {
            registrar.mark_unavailable();
            return Err(Error::Unavailable);
        }
        let mut failure = None;
        for (instance, prepared) in self.instances.iter_mut().zip(&schedule.processors) {
            let requirement = prepared.factory.native_child_settlement_requirement();
            let result = isolate_adapter_value("native child settlement binding", || {
                instance.processor.bind_native_child_settlement(registrar)
            })
            .unwrap_or(Err(Error::Unavailable));
            let matched = matches!(
                (requirement, &result),
                (Requirement::Required, Ok(Binding::Registered))
                    | (Requirement::NotApplicable, Ok(Binding::NotApplicable))
            );
            if !matched {
                registrar.mark_unavailable();
                failure.get_or_insert_with(|| result.err().unwrap_or(Error::Unavailable));
            }
        }
        failure.map_or(Ok(()), Err)
    }

    pub(crate) fn validate_reprepare_capture(
        &mut self,
        schedule: &PreparedAudioSchedule,
        selection: &native_reprepare::ReprepareSelection,
        cancellation: &mondrian_core::ExecutionCancellationToken,
    ) -> Result<(), crate::NativeAudioReprepareError> {
        use crate::native_runtime_owner::NativeHostedBinding;
        use crate::NativeAudioReprepareError as Error;
        if self.instances.len() != schedule.processors.len()
            || selection.index >= self.instances.len()
        {
            return Err(Error::Admission("restart Hosted shape changed"));
        }
        for (index, (instance, prepared)) in
            self.instances.iter_mut().zip(&schedule.processors).enumerate()
        {
            if cancellation.is_canceled() {
                return Err(Error::Cancelled);
            }
            if instance.native_author_outstanding.is_some()
                || instance.native_editor_detached.is_some()
                || instance.native_editor_attachment.is_some()
                || instance.notes.is_some()
            {
                return Err(Error::Admission("restart author or editor work is pending"));
            }
            let NativeHostedBinding::Installed { lease, object } = instance.native_binding else {
                if index != selection.index
                    && !instance.processor.native_observations_supported()
                    && prepared.factory.native_child_settlement_requirement()
                        == mondrian_core::NativeChildSettlementRequirement::NotApplicable
                    && isolate_adapter_call("restart peer recheck", || {
                        instance.processor.preflight_reprepare()
                    })?
                    .is_none()
                {
                    continue;
                }
                return Err(Error::Admission("retained restart binding is unavailable"));
            };
            if index == selection.index && (lease != selection.lease || object != selection.object)
            {
                return Err(Error::Admission("retained restart object changed"));
            }
            let observed = isolate_adapter_call("restart prepared Source recheck", || {
                instance.processor.preflight_native_observations(object)
            })?;
            if observed.object != Some(object)
                || observed.editor_cleanup.is_some()
                || instance.native_editor_attachment.is_some()
            {
                return Err(Error::Admission("restart native owner or editor changed"));
            }
            crate::native_audio_author_runtime::closed_source(observed.author.as_ref(), false)
                .map_err(|_| Error::Admission("restart author source is not closed and empty"))?;
            if index == selection.index {
                // Capture retires the serviced callback, while its issued phase
                // and the parent Session ticket remain HOLD. A fresh callback
                // must not be confused with this captured configuration.
                if observed.pending_reprepare
                    != Some(crate::NativeObservedRequest::RequestOnly(
                        selection.cohort.request,
                    ))
                {
                    return Err(Error::Admission("restart callback cohort changed"));
                }
                let cohort = isolate_adapter_call("restart captured cohort recheck", || {
                    instance.processor.native_configuration_cohort()
                })?;
                if cohort.is_some() {
                    return Err(Error::Cohort(cohort));
                }
            } else if observed.pending_reprepare.is_some() {
                return Err(Error::Admission("another native restart is pending"));
            }
        }
        Ok(())
    }

    pub(crate) fn occurrence_count(&self) -> usize {
        self.instances.len()
    }

    pub(crate) const fn maximum_parameter_lanes(&self) -> usize {
        self.maximum_parameter_lanes
    }

    pub(crate) const fn parameter_event_capacity(&self) -> usize {
        self.parameter_event_capacity
    }

    pub(crate) const fn session_scratch_bytes(&self) -> usize {
        self.session_scratch_bytes
    }

    pub(crate) fn has_failed_instance(&self) -> bool {
        self.instances
            .iter()
            .any(|instance| matches!(instance.continuity, HostedProcessorContinuity::Failed))
    }

    pub(crate) fn preflight_native_control(
        &mut self,
        entry_program: Option<&crate::CompiledAudioProgram>,
    ) -> Result<Option<(AudioProcessorOccurrence, crate::NativeObservedRequest)>, AudioExecutionError>
    {
        for instance in &mut self.instances {
            if let crate::native_runtime_owner::NativeHostedBinding::Installed { lease, object } =
                instance.native_binding
            {
                let observations = isolate_adapter_call("native control preflight", || {
                    instance.processor.preflight_native_observations(object)
                })?;
                if observations.object != Some(object) {
                    instance.native_editor_attachment = None;
                    instance.native_binding =
                        crate::native_runtime_owner::NativeHostedBinding::Unbound;
                    return Err(AudioProcessorHostError::PoisonedInstance.into());
                }
                match observations.editor_cleanup {
                    Some(crate::NativeAudioEditorCloseStage::ClearHostFrame) => {
                        return Err(AudioExecutionError::EditorCleanupRequired(Box::new(
                            lease.editor_target(),
                        )))
                    }
                    Some(crate::NativeAudioEditorCloseStage::RemoveView) => {
                        return Err(AudioProcessorHostError::PoisonedInstance.into())
                    }
                    None => {}
                }
                if let Some(program) = entry_program
                    && instance.notes.as_ref().is_some_and(|notes| notes.ledger.origin().is_some())
                    && matches!(
                        &crate::schedule::processor(program, instance.occurrence)
                            .map_err(|_| AudioExecutionError::InvalidPreparedSchedule)?
                            .definition,
                        mondrian_timeline::AudioProcessorDefinitionRef::Vst3 { .. }
                    )
                {
                    if instance.native_editor_attachment.is_some()
                        || instance.native_editor_detached.is_some()
                    {
                        return Err(notes::refusal(
                            "Note replacement requires the native editor to be closed",
                        )
                        .into());
                    }
                    crate::native_audio_author_runtime::closed_source(
                        observations.author.as_ref(),
                        instance.native_author_outstanding.is_some(),
                    )
                    .map_err(|reason| {
                        AudioExecutionError::NativeAuthorPending(Box::new(
                            crate::NativeAudioAuthorExecutionHold {
                                target: lease.editor_target(),
                                reason,
                            },
                        ))
                    })?;
                }
                // Preserve ordered source edits before configuration service
                // or constant parameter delivery can overwrite the controller.
                // Open edits/groups with no pending batch remain audible.
                if let Err(reason) = crate::native_audio_author_runtime::author_pcm_source(
                    observations.author.as_ref(),
                    instance.native_author_outstanding.is_some(),
                ) {
                    return Err(AudioExecutionError::NativeAuthorPending(Box::new(
                        crate::NativeAudioAuthorExecutionHold {
                            target: lease.editor_target(),
                            reason,
                        },
                    )));
                }
                if let Some(request) = observations.pending_reprepare {
                    return Ok(Some((instance.occurrence, request)));
                }
                // This exact installed object's same snapshot already covered
                // pending flags; another PollPending round trip is redundant.
                continue;
            }
            if let Some(request) = isolate_adapter_call("pending preflight", || {
                instance.processor.preflight_reprepare()
            })? {
                return Ok(Some((instance.occurrence, request)));
            }
        }
        Ok(None)
    }

    pub(crate) fn enter_state(
        &mut self,
        program: &crate::CompiledAudioProgram,
        processors: &[PreparedProcessor],
        entry: AudioStateEntry,
        cancellation: Option<&mondrian_core::ExecutionCancellationToken>,
    ) -> Result<(), AudioExecutionError> {
        if processors.len() != self.instances.len() {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        let entry_binding = &self.entry_binding;
        for (processor, instance) in processors.iter().zip(&mut self.instances) {
            if cancellation.is_some_and(mondrian_core::ExecutionCancellationToken::is_canceled) {
                return Err(crate::AudioNativeOwnerError::Cancelled.into());
            }
            if let Some((context, _)) = entry_binding {
                context.validate(cancellation).map_err(AudioExecutionError::from)?;
            }
            if processor.occurrence != instance.occurrence {
                return Err(AudioExecutionError::InvalidPreparedSchedule);
            }
            if !processor.factory.contract().requires_state_entry() {
                if !matches!(instance.continuity, HostedProcessorContinuity::Stateless) {
                    return Err(AudioExecutionError::InvalidPreparedSchedule);
                }
                Self::bind_entered_instance(instance, entry_binding.as_ref(), cancellation)?;
                continue;
            }
            // Revoke before every actual lifecycle attempt, including failed or
            // lazy Contribution entry. A retained PID cannot preserve the lease.
            instance.native_binding = crate::native_runtime_owner::NativeHostedBinding::Unbound;
            if matches!(
                processor.occurrence.owner,
                AudioProcessorOccurrenceOwner::Contribution { .. }
            ) {
                instance.continuity = HostedProcessorContinuity::Pending { epoch: entry.epoch };
            } else {
                let signal_start_sample =
                    subtract_signal_delay(entry.start_sample, processor.input_signal_delay_frames)?;
                let replacement_required =
                    instance.notes.as_ref().is_some_and(|notes| notes.ledger.origin().is_some())
                        && matches!(
                            &crate::schedule::processor(program, instance.occurrence)
                                .map_err(|_| AudioExecutionError::InvalidPreparedSchedule)?
                                .definition,
                            mondrian_timeline::AudioProcessorDefinitionRef::Vst3 { .. }
                        );
                let pending_note_entry = match Self::take_note_entry(
                    instance,
                    entry,
                    entry_binding.as_ref(),
                    cancellation,
                    replacement_required,
                ) {
                    Ok(pending) => pending,
                    Err(error) => {
                        instance.continuity = HostedProcessorContinuity::Failed;
                        return Err(error);
                    }
                };
                if let Err(error) = isolate_adapter_call("state entry", || {
                    instance.processor.enter_state(signal_start_sample)
                }) {
                    instance.continuity = HostedProcessorContinuity::Failed;
                    return Err(error.into());
                }
                instance.continuity = HostedProcessorContinuity::Active {
                    epoch: entry.epoch,
                    next_sample: signal_start_sample,
                };
                if let Err(error) =
                    Self::bind_entered_instance(instance, entry_binding.as_ref(), cancellation)
                        .map_err(AudioExecutionError::from)
                        .and_then(|()| {
                            Self::bind_note_cold_entry(instance, pending_note_entry, entry)
                        })
                {
                    // Preserve the existing note-free cancellation evidence.
                    // A Note owner cannot resume an unconfirmed ledger entry.
                    if instance.notes.is_some() {
                        instance.continuity = HostedProcessorContinuity::Failed;
                    }
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process_rack(
        &mut self,
        rack: &PreparedRack,
        processors: &[PreparedProcessor],
        score: &crate::schedule::PreparedNoteScore,
        processor_auxiliary_buses: &[PreparedProcessorAuxiliaryBus],
        processor_output_buses: &[crate::schedule::PreparedProcessorOutputBus],
        output_pcm: &mut [Vec<f32>],
        sidechain_bindings: &[PreparedSidechainBinding],
        request: AudioRenderRequest,
        sample_rate: AudioSampleRate,
        channel_layout: AudioChannelLayout,
        backend: AudioKernelBackend,
        pcm: &mut [f32],
        sidechain_source_buffers: &[Vec<f32>],
        sidechain_delay_lines: &mut [FixedDelayLine],
        processor_delay_lines: &mut [FixedDelayLine],
        sidechain_offset_frames: usize,
        cancellation: Option<&mondrian_core::ExecutionCancellationToken>,
    ) -> Result<(), AudioExecutionError> {
        let expected_samples = request
            .frames
            .checked_mul(channel_layout.channel_count())
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        if pcm.len() != expected_samples
            || sample_rate.hz() != self.render_contract.sample_rate
            || channel_layout != self.render_contract.channel_layout
            || request.frames > self.render_contract.max_block_frames
        {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        let entry_binding = &self.entry_binding;
        let instances = &mut self.instances;
        let parameter_events = &mut self.parameter_events;
        let auxiliary_pcm = &mut self.auxiliary_pcm;
        let sidechain_frame_db = &mut self.sidechain_frame_db;
        let sidechain_gains = &mut self.sidechain_gains;
        let sidechain_mix_pcm = &mut self.sidechain_mix_pcm;
        for processor_index in rack.processors.clone() {
            let processor = processors
                .get(processor_index)
                .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
            let instance = instances
                .get_mut(processor_index)
                .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
            if instance.occurrence != processor.occurrence {
                return Err(AudioExecutionError::InvalidPreparedSchedule);
            }
            Self::refuse_pending_note_entry_pcm(instance)?;
            let signal_request = AudioRenderRequest {
                transport_activity: request.transport_activity,
                start_sample: subtract_signal_delay(
                    request.start_sample,
                    processor.input_signal_delay_frames,
                )?,
                frames: request.frames,
            };
            match instance.continuity {
                HostedProcessorContinuity::Stateless => {
                    if processor.factory.contract().requires_state_entry() {
                        return Err(AudioExecutionError::InvalidPreparedSchedule);
                    }
                }
                HostedProcessorContinuity::Unentered => {
                    return Err(AudioExecutionError::StateEntryRequired);
                }
                HostedProcessorContinuity::Failed => {
                    return Err(AudioProcessorHostError::PoisonedInstance.into());
                }
                HostedProcessorContinuity::Pending { epoch } => {
                    if let Some((context, _)) = entry_binding {
                        context.validate(cancellation)?;
                    }
                    if cancellation
                        .is_some_and(mondrian_core::ExecutionCancellationToken::is_canceled)
                    {
                        return Err(crate::AudioNativeOwnerError::Cancelled.into());
                    }
                    instance.native_binding =
                        crate::native_runtime_owner::NativeHostedBinding::Unbound;
                    if let Err(error) = isolate_adapter_call("lazy state entry", || {
                        instance.processor.enter_state(signal_request.start_sample)
                    }) {
                        instance.continuity = HostedProcessorContinuity::Failed;
                        return Err(error.into());
                    }
                    instance.continuity = HostedProcessorContinuity::Active {
                        epoch,
                        next_sample: signal_request.start_sample,
                    };
                    if let Err(error) =
                        Self::bind_entered_instance(instance, entry_binding.as_ref(), cancellation)
                    {
                        instance.continuity = HostedProcessorContinuity::Failed;
                        return Err(error.into());
                    }
                }
                HostedProcessorContinuity::Active { next_sample, .. }
                    if next_sample != signal_request.start_sample =>
                {
                    return Err(AudioExecutionError::InvalidPreparedSchedule);
                }
                HostedProcessorContinuity::Active { .. } => {}
            }
            let batch = prepare_parameter_event_batch(
                processor,
                signal_request,
                sample_rate,
                parameter_events,
            )?;
            let context = AudioProcessorProcessContext::new(
                signal_request,
                sample_rate,
                channel_layout,
                self.render_contract.processing_mode,
                backend,
            )
            .with_musical_time_reference(self.musical_time_reference)
            .admitted_root_window();
            let auxiliary_buses = prepare_processor_auxiliary_buses(
                processor,
                processor_auxiliary_buses,
                sidechain_bindings,
                sidechain_source_buffers,
                sidechain_delay_lines,
                signal_request,
                sample_rate,
                expected_samples,
                sidechain_offset_frames,
                channel_layout.channel_count(),
                auxiliary_pcm,
                sidechain_frame_db,
                sidechain_gains,
                sidechain_mix_pcm,
                backend,
            )?;
            let auxiliary_samples = auxiliary_buses
                .last()
                .map(|bus| bus.sample_range(signal_request.frames).map(|range| range.end))
                .unwrap_or(Some(0))
                .ok_or(AudioExecutionError::BufferTooLarge)?;
            processor_delay_lines
                .get_mut(processor_index)
                .ok_or(AudioExecutionError::InvalidPreparedSchedule)?
                .delay_interleaved_in_place(pcm);
            let note_origin = if instance.notes.is_some() {
                Some(notes::origin(instance)?)
            } else {
                None
            };
            if let Some(state) = &mut instance.notes
                && let Err(error) = state.prepare_admitted_block(
                    score,
                    processor_index,
                    note_origin.expect("Note operation origin"),
                    signal_request,
                )
            {
                instance.continuity = HostedProcessorContinuity::Failed;
                return Err(error.into());
            }
            let output_buses = processor_output_buses
                .get(processor.output_buses.clone())
                .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
            for bus in output_buses {
                output_pcm
                    .get_mut(bus.buffer_slot)
                    .and_then(|pcm| pcm.get_mut(..expected_samples))
                    .ok_or(AudioExecutionError::InvalidPreparedSchedule)?
                    .fill(0.0);
            }
            let result = {
                let note_io = instance.notes.as_mut().map(|state| NoteBlockIo {
                    inputs: &state.inputs,
                    ends: &mut state.ends,
                    end_limit: state.end_limit,
                    ports: state.ports,
                    refused: &mut state.output_refused,
                });
                let mut audio = MainBusAudioIo {
                    channel_layout,
                    frames: request.frames,
                    pcm,
                    auxiliary_buses,
                    auxiliary_pcm: &auxiliary_pcm[..auxiliary_samples],
                    notes: note_io,
                    output_buses,
                    output_pcm,
                };
                isolate_adapter_call("block processing", || {
                    instance.processor.process(context, &mut audio, batch)
                })
            };
            if let Err(error) = result {
                instance.continuity = HostedProcessorContinuity::Failed;
                return Err(error.into());
            }
            if let Some(state) = &mut instance.notes
                && let Err(error) =
                    state.accept_native_return(note_origin.expect("Note operation origin"))
            {
                instance.continuity = HostedProcessorContinuity::Failed;
                return Err(error.into());
            }
            if let Some(slot) = processor.main_output_buffer {
                output_pcm
                    .get_mut(slot)
                    .and_then(|output| output.get_mut(..expected_samples))
                    .ok_or(AudioExecutionError::InvalidPreparedSchedule)?
                    .copy_from_slice(pcm);
            }
            if let HostedProcessorContinuity::Active { epoch, .. } = instance.continuity {
                let next_sample = signal_request
                    .start_sample
                    .checked_add(
                        i64::try_from(request.frames)
                            .map_err(|_| AudioExecutionError::BufferTooLarge)?,
                    )
                    .ok_or(AudioExecutionError::BufferTooLarge)?;
                instance.continuity = HostedProcessorContinuity::Active { epoch, next_sample };
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn parameter_events_for_test(
        &mut self,
        processors: &[PreparedProcessor],
        processor_index: usize,
        request: AudioRenderRequest,
        sample_rate: AudioSampleRate,
    ) -> Result<Vec<(ParameterId, Vec<AudioParameterEvent>)>, AudioExecutionError> {
        let processor = processors
            .get(processor_index)
            .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
        let signal_request = AudioRenderRequest {
            transport_activity: request.transport_activity,
            start_sample: subtract_signal_delay(
                request.start_sample,
                processor.input_signal_delay_frames,
            )?,
            frames: request.frames,
        };
        let batch = prepare_parameter_event_batch(
            processor,
            signal_request,
            sample_rate,
            &mut self.parameter_events,
        )?;
        (0..batch.lane_count())
            .map(|lane| {
                let parameter_id = batch
                    .parameter_id(lane)
                    .ok_or(AudioExecutionError::InvalidPreparedSchedule)?
                    .clone();
                let events = batch
                    .events(lane)
                    .ok_or(AudioExecutionError::InvalidPreparedSchedule)?
                    .to_vec();
                Ok((parameter_id, events))
            })
            .collect()
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_processor_auxiliary_buses<'a>(
    processor: &PreparedProcessor,
    processor_auxiliary_buses: &'a [PreparedProcessorAuxiliaryBus],
    sidechain_bindings: &[PreparedSidechainBinding],
    sidechain_source_buffers: &[Vec<f32>],
    sidechain_delay_lines: &mut [FixedDelayLine],
    signal_request: AudioRenderRequest,
    sample_rate: AudioSampleRate,
    samples: usize,
    source_offset_frames: usize,
    channels: usize,
    auxiliary_pcm: &mut [f32],
    sidechain_frame_db: &mut [f64],
    sidechain_gains: &mut [f32],
    sidechain_mix_pcm: &mut [f32],
    backend: AudioKernelBackend,
) -> Result<&'a [PreparedProcessorAuxiliaryBus], AudioExecutionError> {
    let buses = processor_auxiliary_buses
        .get(processor.auxiliary_buses.clone())
        .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
    let required_samples = buses
        .last()
        .map(|bus| bus.sample_range(signal_request.frames).map(|range| range.end))
        .unwrap_or(Some(0))
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    let auxiliary_pcm = auxiliary_pcm
        .get_mut(..required_samples)
        .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
    auxiliary_pcm.fill(0.0);
    let source_start = source_offset_frames
        .checked_mul(channels)
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    let source_end =
        source_start.checked_add(samples).ok_or(AudioExecutionError::BufferTooLarge)?;
    for bus in buses {
        let range = bus
            .sample_range(signal_request.frames)
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        let destination = auxiliary_pcm
            .get_mut(range)
            .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
        let bus_samples = destination.len();
        let bus_channels = bus.channel_layout.channel_count();
        for binding_index in bus.bindings.clone() {
            let binding = sidechain_bindings
                .get(binding_index)
                .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
            let source = sidechain_source_buffers
                .get(binding.source_buffer_slot)
                .and_then(|source| source.get(source_start..source_end))
                .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
            let delay_line = sidechain_delay_lines
                .get_mut(binding_index)
                .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
            // The immutable matrix precedes the route's destination-width PDC.
            // Gain still evaluates at target signal time after the delay.
            let source = if let Some(mixer) = &bus.channel_mixer {
                let mapped = sidechain_mix_pcm
                    .get_mut(..bus_samples)
                    .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
                mixer.mix_into(backend, signal_request.frames, source, mapped)?;
                &mapped[..]
            } else {
                source
            };
            if let Some(curve) = &binding.gain_automation {
                let frame_db = sidechain_frame_db
                    .get_mut(..signal_request.frames)
                    .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
                let mut cursor = curve.initial_cursor(signal_request.start_sample);
                for (frame, value) in frame_db.iter_mut().enumerate() {
                    let sample = signal_request
                        .start_sample
                        .checked_add(
                            i64::try_from(frame)
                                .map_err(|_| AudioExecutionError::BufferTooLarge)?,
                        )
                        .ok_or(AudioExecutionError::BufferTooLarge)?;
                    *value = curve.evaluate_sample(sample, sample_rate, &mut cursor)?;
                }
                let gains = sidechain_gains
                    .get_mut(..bus_samples)
                    .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
                dsp::expand_frame_db_to_interleaved_gains(frame_db, bus_channels, gains);
                delay_line.add_interleaved_with_gains(source, destination, gains)?;
            } else {
                let gain =
                    binding.constant_gain.ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
                delay_line.add_interleaved_constant(source, destination, gain)?;
            }
        }
    }
    Ok(buses)
}

pub(crate) fn mark_unavailable_if_native_forwarding_unknown(
    registrar: &mondrian_core::NativeChildSettlementRegistrar,
    query: impl FnOnce() -> mondrian_core::NativeChildSettlementRequirement,
) {
    if !matches!(
        isolate_adapter_value("native child settlement forwarding", query),
        Ok(mondrian_core::NativeChildSettlementRequirement::Required
            | mondrian_core::NativeChildSettlementRequirement::NotApplicable)
    ) {
        registrar.mark_unavailable();
    }
}

fn isolate_adapter_call<T>(
    operation: &'static str,
    call: impl FnOnce() -> Result<T, AudioProcessorHostError>,
) -> Result<T, AudioProcessorHostError> {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(result) => result,
        Err(_) => Err(AudioProcessorHostError::AdapterPanicked(operation)),
    }
}

fn isolate_adapter_value<T>(
    operation: &'static str,
    call: impl FnOnce() -> T,
) -> Result<T, AudioProcessorHostError> {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(value) => Ok(value),
        Err(_) => Err(AudioProcessorHostError::AdapterPanicked(operation)),
    }
}

fn subtract_signal_delay(
    execution_sample: i64,
    signal_delay_frames: usize,
) -> Result<i64, AudioExecutionError> {
    let signal_delay =
        i64::try_from(signal_delay_frames).map_err(|_| AudioExecutionError::BufferTooLarge)?;
    execution_sample
        .checked_sub(signal_delay)
        .ok_or(AudioExecutionError::BufferTooLarge)
}

impl fmt::Debug for PreparedProcessorHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedProcessorHost")
            .field("occurrence_count", &self.instances.len())
            .field("render_contract", &self.render_contract)
            .field("maximum_parameter_lanes", &self.maximum_parameter_lanes)
            .field("parameter_event_capacity", &self.parameter_event_capacity)
            .field("session_scratch_bytes", &self.session_scratch_bytes)
            .finish()
    }
}

pub(crate) fn default_processor_resolver() -> &'static BuiltInAudioProcessorResolver {
    static RESOLVER: BuiltInAudioProcessorResolver = BuiltInAudioProcessorResolver;
    &RESOLVER
}

pub(crate) fn prepare_processor_factory(
    resolver: &dyn AudioProcessorResolver,
    processor: &crate::plan::CompiledProcessor,
    occurrence: AudioProcessorOccurrence,
    contract: AudioRenderContract,
    note_input_requested: bool,
    musical_time_reference: Option<mondrian_core::MusicalTimeReference>,
    native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
) -> Result<PreparedProcessorFactoryBinding, AudioProcessorHostError> {
    PreparedProcessorFactoryBinding::resolve(
        resolver,
        AudioProcessorPrepareRequest::new(
            occurrence,
            &processor.definition,
            &processor.parameters,
            processor.opaque_state.as_deref(),
            contract,
        )
        .with_note_input_requested(note_input_requested)
        .with_musical_time_reference(musical_time_reference)
        .with_native_child_settlement_registrar(native_child_settlement),
    )
}
