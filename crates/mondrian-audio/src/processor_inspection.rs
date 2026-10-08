//! Non-realtime processor input inspection using canonical author addresses.

use mondrian_core::{AudioProcessingScopeId, AudioProcessorInstanceId};
use mondrian_timeline::{
    inspect_audio_component, inspect_audio_processor_rack, AudioChannelStripOwner,
    AudioChannelStripRack, AudioComponentAddress, AudioComponentAddressError,
    AudioProcessorRackAddress, AudioProcessorRackEditError, Sequence,
};
use serde::{Deserialize, Serialize};

use crate::processor_host::PreparedProcessorFactoryBinding;
use crate::{
    AudioCompileError, AudioProcessorAuxiliaryInputContract, AudioProcessorExecutionContract,
    AudioProcessorHostError, AudioProcessorInsertionPoint, AudioProcessorOccurrence,
    AudioProcessorOccurrenceOwner, AudioProcessorPrepareRequest, AudioProcessorResolver,
    AudioRenderContract,
};

/// Exact author target for a control-side native input query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AudioProcessorInputInspectionTarget {
    /// One directly owned Track, Bus or Program Output processor rack.
    ChannelStrip {
        /// Canonical channel-strip owner.
        owner: AudioChannelStripOwner,
        /// Exact pre/post-fader insertion stage.
        rack: AudioChannelStripRack,
    },
    /// One explicitly selected materialization of a shared processing Scope.
    ProcessingScope {
        /// Expected Scope identity before any asynchronous work starts.
        scope_id: AudioProcessingScopeId,
        /// Component occurrence whose binding must still name this Scope.
        occurrence: AudioComponentAddress,
    },
}

/// Immutable inputs for one non-realtime processor inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioProcessorInputInspectionRequest {
    /// Canonical owner and explicit Scope occurrence, when required.
    pub target: AudioProcessorInputInspectionTarget,
    /// Stable processor identity within the addressed rack.
    pub processor_id: AudioProcessorInstanceId,
    /// Exact mode, layout, rate and bounds offered to the existing resolver.
    pub render_contract: AudioRenderContract,
}

/// Captured preparation facts suitable for a transient control projection.
///
/// These facts do not replace whole-Program resource admission or authorize an
/// edit. Product adapters must reject stale asynchronous results and submit
/// routing mutations through the canonical author transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioProcessorInputInspection {
    /// Exact generated processor occurrence offered to preparation.
    pub occurrence: AudioProcessorOccurrence,
    /// Render contract against which the input declarations were checked.
    pub render_contract: AudioRenderContract,
    /// Ordered stable auxiliary input keys and exact layouts.
    pub auxiliary_inputs: AudioProcessorAuxiliaryInputContract,
    /// Optional native display names associated with admitted input keys.
    pub input_names: Vec<crate::AudioProcessorInputName>,
    /// Mode, latency, tail and private scratch facts of the prepared factory.
    pub execution_contract: AudioProcessorExecutionContract,
}

/// Address or preparation failure while inspecting a processor.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AudioProcessorInputInspectionError {
    /// Canonical rack or processor address is absent.
    #[error(transparent)]
    Rack(#[from] AudioProcessorRackEditError),
    /// The selected Scope occurrence no longer exists.
    #[error(transparent)]
    Component(#[from] AudioComponentAddressError),
    /// The selected Component now names a different processing Scope.
    #[error("audio processor inspection occurrence no longer belongs to Scope {0}")]
    ScopeBindingChanged(AudioProcessingScopeId),
    /// Rate or block extent cannot enter processor preparation.
    #[error(transparent)]
    RenderContract(#[from] AudioCompileError),
    /// The existing processor resolver or factory refused preparation.
    #[error(transparent)]
    Processor(#[from] AudioProcessorHostError),
}

/// Resolve a control observation's exact canonical author instance.
///
/// This read-only seam verifies Scope occurrence bindings and processor/rack
/// membership without invoking a resolver. Product adapters use it to compare
/// asynchronous snapshots and reject stale results; locks do not block reading.
pub fn inspect_audio_processor_input_author(
    sequence: &Sequence,
    target: AudioProcessorInputInspectionTarget,
    processor_id: AudioProcessorInstanceId,
) -> Result<&mondrian_timeline::audio::AudioProcessorInstance, AudioProcessorInputInspectionError> {
    let address = match target {
        AudioProcessorInputInspectionTarget::ChannelStrip { owner, rack } => {
            AudioProcessorRackAddress::ChannelStrip { owner, rack }
        }
        AudioProcessorInputInspectionTarget::ProcessingScope { scope_id, occurrence } => {
            let component = inspect_audio_component(sequence, occurrence)?;
            if component.component().processing.scope_id != scope_id {
                return Err(AudioProcessorInputInspectionError::ScopeBindingChanged(
                    scope_id,
                ));
            }
            AudioProcessorRackAddress::ProcessingScope { scope_id }
        }
    };
    let inspection = inspect_audio_processor_rack(sequence, &address)?;
    inspection
        .rack()
        .processors
        .iter()
        .find(|processor| processor.id == processor_id)
        .ok_or_else(|| AudioProcessorRackEditError::UnknownProcessor(processor_id).into())
}

/// Inspect one actual author instance through the normal preparation resolver.
///
/// Call from a bounded non-realtime control worker: native resolvers may load
/// the selected class and restore its opaque state in a supervised discovery
/// child. Locked authors remain inspectable. This query never creates a mutable
/// render Session, invokes a processing callback or mutates the Sequence.
pub fn inspect_audio_processor_inputs(
    sequence: &Sequence,
    request: AudioProcessorInputInspectionRequest,
    resolver: &dyn AudioProcessorResolver,
) -> Result<AudioProcessorInputInspection, AudioProcessorInputInspectionError> {
    request.render_contract.validate()?;
    let processor =
        inspect_audio_processor_input_author(sequence, request.target, request.processor_id)?;
    let (owner, insertion) = match request.target {
        AudioProcessorInputInspectionTarget::ChannelStrip { owner, rack } => (
            match owner {
                AudioChannelStripOwner::Track { track_id } => {
                    AudioProcessorOccurrenceOwner::Track(track_id)
                }
                AudioChannelStripOwner::Bus { bus_id } => {
                    AudioProcessorOccurrenceOwner::Bus(bus_id)
                }
                AudioChannelStripOwner::ProgramOutput { output_id } => {
                    AudioProcessorOccurrenceOwner::Output(output_id)
                }
            },
            match rack {
                AudioChannelStripRack::PreFader => AudioProcessorInsertionPoint::PreFader,
                AudioChannelStripRack::PostFader => AudioProcessorInsertionPoint::PostFader,
            },
        ),
        AudioProcessorInputInspectionTarget::ProcessingScope { scope_id, occurrence } => (
            AudioProcessorOccurrenceOwner::Contribution { edit_id: occurrence.edit_id, scope_id },
            AudioProcessorInsertionPoint::Scope,
        ),
    };
    let occurrence = AudioProcessorOccurrence { instance_id: processor.id, owner, insertion };
    let factory = PreparedProcessorFactoryBinding::resolve(
        resolver,
        AudioProcessorPrepareRequest::new(
            occurrence,
            &processor.definition,
            &processor.parameters,
            processor.opaque_state.as_ref().map(|state| state.as_slice()),
            request.render_contract,
        ),
    )?;
    Ok(AudioProcessorInputInspection {
        occurrence,
        render_contract: request.render_contract,
        auxiliary_inputs: factory.auxiliary_inputs().clone(),
        input_names: factory.input_names()?,
        execution_contract: factory.contract(),
    })
}
