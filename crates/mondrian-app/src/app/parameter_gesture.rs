//! Continuous parameter edits over the canonical visual and audio author paths.

use super::product_action::{
    AudioProductAction, ClipEditNumericCurvePayload, ClipProductAction,
    ClipWriteParameterValuesPayload, ProductAction, VisualEffectEditNumericCurvePayload,
    VisualEffectProductAction, VisualEffectSetParameterValuePayload, VisualMaskProductAction,
    VisualMaskSetParameterValuePayload,
};
use super::AppState;
use mondrian_core::{
    AnimationParameterAddress, AuthoringGestureId, ClipId, EffectId, MaskId, MondrianError, Result,
    SequenceId,
};
use mondrian_editor_state::{Action, AuthoringSessionId};
use mondrian_timeline::{
    AudioAutomationEditRequest, AudioAutomationTarget, AudioProcessorRackAddress,
    AudioProcessorRackEdit, AudioProcessorRackEditRequest,
};
use serde::{Deserialize, Serialize};

const NAMESPACE: &str = "mondrian.parameter_gesture";
const NAME: &str = "edit";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "owner",
    content = "edit",
    rename_all = "snake_case",
    deny_unknown_fields
)]
/// A canonical parameter intent accepted by continuous authoring interactions.
pub(crate) enum ParameterGestureEdit {
    Clip(ClipWriteParameterValuesPayload),
    Effect(VisualEffectSetParameterValuePayload),
    Mask(VisualMaskSetParameterValuePayload),
    ClipCurve(ClipEditNumericCurvePayload),
    EffectCurve(VisualEffectEditNumericCurvePayload),
    AudioCurve(AudioAutomationEditRequest),
    AudioScalar(AudioScalarValueEdit),
}

/// Exact open-author lifetime captured by a fresh audio scalar control projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AudioScalarAuthorLease {
    /// A reopened project is a new author lifetime even when every entity ID matches.
    pub(crate) session_id: AuthoringSessionId,
    /// Parameter edits cannot transfer onto another active Sequence.
    pub(crate) sequence_id: SequenceId,
}

/// Closed author owner of one scalar control, without presentation or runtime state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum AudioScalarOwner {
    /// One static processor parameter with its exact materialization and definition contract.
    Parameter {
        owner: mondrian_audio::AudioProcessorInputInspectionTarget,
        processor_id: mondrian_core::AudioProcessorInstanceId,
        definition: mondrian_timeline::audio::AudioProcessorDefinitionRef,
        parameter_schema: Box<mondrian_core::automation::ParameterSchema>,
    },
    /// Channel input trim is a distinct canonical field, without a fabricated schema.
    Trim {
        owner: mondrian_timeline::AudioChannelStripOwner,
    },
    /// Channel fader must remain static to accept scalar edits.
    Fader {
        owner: mondrian_timeline::AudioChannelStripOwner,
    },
    /// One ordinary principal/send route, including its exact endpoints.
    RouteLevel {
        route_id: mondrian_core::AudioRouteId,
        source: mondrian_timeline::audio::AudioRouteSource,
        destination: mondrian_timeline::audio::AudioRouteDestination,
    },
    /// One retained processor input route in the currently inspected occurrence.
    ProcessorSidechainLevel {
        route_id: mondrian_core::AudioRouteId,
        source: mondrian_timeline::audio::AudioRouteSource,
        bus_key: String,
        owner: mondrian_audio::AudioProcessorInputInspectionTarget,
        processor_id: mondrian_core::AudioProcessorInstanceId,
        definition: mondrian_timeline::audio::AudioProcessorDefinitionRef,
    },
}

/// Exact open-author lifetime and closed scalar owner captured by a fresh control.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AudioScalarGestureTarget {
    pub(crate) lease: AudioScalarAuthorLease,
    pub(crate) owner: AudioScalarOwner,
}

impl AudioScalarAuthorLease {
    /// Bind only a concrete projection of the current open Sequence.
    pub(crate) fn from_state(state: &AppState, source_sequence_id: SequenceId) -> Option<Self> {
        (state.active_sequence_id() == Some(source_sequence_id)).then_some(Self {
            session_id: state.authoring_session_id()?,
            sequence_id: source_sequence_id,
        })
    }
}

impl AudioScalarOwner {
    /// Canonical Rack address when this scalar belongs to a processor owner.
    pub(crate) fn processor_address(&self) -> Option<AudioProcessorRackAddress> {
        match self {
            Self::Parameter { owner, .. } | Self::ProcessorSidechainLevel { owner, .. } => {
                Some(audio_scalar_processor_rack_address(*owner))
            }
            Self::Trim { .. } | Self::Fader { .. } | Self::RouteLevel { .. } => None,
        }
    }
}

impl AudioScalarGestureTarget {
    /// Encode one finite scalar intent; owner/range admission remains canonical.
    pub(crate) fn set_action(&self, value: f64) -> Option<Action> {
        let valid = value.is_finite()
            && (matches!(&self.owner, AudioScalarOwner::Parameter { .. })
                || (mondrian_timeline::audio::AUDIO_GAIN_DB_MIN
                    ..=mondrian_timeline::audio::AUDIO_GAIN_DB_MAX)
                    .contains(&value));
        valid.then(|| {
            ParameterGestureAction::SetAudioScalar {
                edit: AudioScalarValueEdit { target: Box::new(self.clone()), value },
            }
            .action()
        })
    }
}

fn audio_scalar_processor_rack_address(
    owner: mondrian_audio::AudioProcessorInputInspectionTarget,
) -> AudioProcessorRackAddress {
    match owner {
        mondrian_audio::AudioProcessorInputInspectionTarget::ChannelStrip { owner, rack } => {
            AudioProcessorRackAddress::ChannelStrip { owner, rack }
        }
        mondrian_audio::AudioProcessorInputInspectionTarget::ProcessingScope {
            scope_id, ..
        } => AudioProcessorRackAddress::ProcessingScope { scope_id },
    }
}

/// One closed-domain scalar intent; it cannot contain an arbitrary nested author operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AudioScalarValueEdit {
    /// Captured fresh owner; boxing keeps ordinary gesture messages compact.
    pub(crate) target: Box<AudioScalarGestureTarget>,
    /// Canonical numeric value, validated by the existing Timeline mutation.
    pub(crate) value: f64,
}

/// Dynamic installation identity of an already displayed canonical MainQuery value.
/// The scalar owner supplies instance/occurrence/insertion; this is no native lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DisplayedNativeParameterIdentity {
    pub(crate) root_plan: mondrian_core::AudioPreparedPlanId,
    pub(crate) root_session: mondrian_core::AudioRenderSessionId,
    pub(crate) local_plan: mondrian_core::AudioPreparedPlanId,
    pub(crate) local_session: mondrian_core::AudioRenderSessionId,
    pub(crate) continuity_epoch: Option<u64>,
    pub(crate) binding: mondrian_core::AudioProcessorBindingId,
    pub(crate) installed_generation: std::num::NonZeroU64,
    pub(crate) process_id: std::num::NonZeroU32,
    pub(crate) object_serial: std::num::NonZeroU64,
    pub(crate) source_revision: u64,
    pub(crate) fact_revision: u64,
    pub(crate) author_value_bits: u64,
}

impl DisplayedNativeParameterIdentity {
    pub(super) fn from_observation(
        observation: &mondrian_core::NativeAudioParameterObservation,
        fact: &mondrian_core::NativeAudioParameterFact,
    ) -> Option<Self> {
        let target = observation.target();
        Some(Self {
            root_plan: target.root_plan,
            root_session: target.root_session,
            local_plan: target.local_plan,
            local_session: target.local_session,
            continuity_epoch: target.continuity_epoch,
            binding: target.binding,
            installed_generation: target.installed_generation,
            process_id: observation.process_id(),
            object_serial: observation.object_serial(),
            source_revision: observation.source_revision(),
            fact_revision: fact.revision,
            author_value_bits: fact.author_value_bits?,
        })
    }
}

/// One explicit adoption of the canonical snapshot painted by a fresh control.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdoptDisplayedNativeParameterEdit {
    pub(crate) target: Box<AudioScalarGestureTarget>,
    pub(crate) expected_author_generation: u64,
    pub(crate) expected: DisplayedNativeParameterIdentity,
}

/// Stable owner of a local numeric curve draft, before its single release commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "owner", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ParameterCurveGestureTarget {
    Clip {
        clip_id: ClipId,
        parameter: AnimationParameterAddress,
    },
    Effect {
        clip_id: ClipId,
        effect_id: EffectId,
        parameter: AnimationParameterAddress,
    },
    Audio {
        target: AudioAutomationTarget,
    },
}

impl ParameterCurveGestureTarget {
    fn target(&self) -> ParameterGestureTarget {
        let (clip, owner, parameter) = match self {
            Self::Clip { clip_id, parameter } => (*clip_id, ParameterOwner::Clip, parameter),
            Self::Effect { clip_id, effect_id, parameter } => {
                (*clip_id, ParameterOwner::Effect(*effect_id), parameter)
            }
            Self::Audio { target } => return ParameterGestureTarget::AudioCurve(target.clone()),
        };
        ParameterGestureTarget::Visual(VisualParameterGestureTarget {
            clip,
            owner,
            kind: ParameterGestureKind::NumericCurve,
            parameters: vec![(parameter.clone(), None)],
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
/// Explicit lifecycle events; no nested or opaque author commands are admitted.
pub(crate) enum ParameterGestureAction {
    /// Adopt one displayed native snapshot into a previously unowned parameter.
    AdoptDisplayedNativeParameter {
        edit: AdoptDisplayedNativeParameterEdit,
    },
    /// Confirm one exact audio scalar value from keyboard or numeric input.
    SetAudioScalar {
        edit: AudioScalarValueEdit,
    },
    BeginCurve {
        gesture_id: AuthoringGestureId,
        target: ParameterCurveGestureTarget,
    },
    Begin {
        gesture_id: AuthoringGestureId,
        edit: ParameterGestureEdit,
    },
    Update {
        gesture_id: AuthoringGestureId,
        edit: ParameterGestureEdit,
    },
    Finish {
        gesture_id: AuthoringGestureId,
    },
    Cancel {
        gesture_id: AuthoringGestureId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParameterOwner {
    Clip,
    Effect(EffectId),
    Mask(MaskId),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParameterGestureKind {
    Value,
    NumericCurve,
}

#[derive(Debug, Clone, PartialEq)]
enum ParameterGestureTarget {
    Visual(VisualParameterGestureTarget),
    AudioCurve(AudioAutomationTarget),
    AudioScalar(Box<AudioScalarGestureTarget>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VisualParameterGestureTarget {
    kind: ParameterGestureKind,
    clip: ClipId,
    owner: ParameterOwner,
    parameters: Vec<(AnimationParameterAddress, Option<Vec<usize>>)>,
}

/// Ephemeral UI owner identity, separate from authoritative project content.
pub(crate) struct ActiveParameterGesture {
    id: AuthoringGestureId,
    session: AuthoringSessionId,
    sequence: SequenceId,
    target: ParameterGestureTarget,
}

/// Candidate source for one authorized live command.
#[derive(Clone, Copy)]
pub(crate) enum ParameterGestureCommand {
    /// A mutation relative to the current author state.
    Current(AuthoringGestureId),
    /// An absolute positional curve target relative to the interaction origin.
    CurveOrigin(AuthoringGestureId),
}

impl ParameterGestureEdit {
    /// Extract only canonical parameter edits from a widget action.
    pub(crate) fn from_action(action: &Action) -> Option<Self> {
        if let Ok(Some(ParameterGestureAction::SetAudioScalar { edit })) =
            ParameterGestureAction::decode(action)
        {
            return Some(Self::AudioScalar(edit));
        }
        match ProductAction::decode_external(action).ok().flatten()? {
            ProductAction::Clip(ClipProductAction::WriteParameterValues(edit)) => {
                Some(Self::Clip(*edit))
            }
            ProductAction::VisualEffect(VisualEffectProductAction::SetParameterValue(edit)) => {
                Some(Self::Effect(*edit))
            }
            ProductAction::VisualMask(VisualMaskProductAction::SetParameterValue(edit)) => {
                Some(Self::Mask(*edit))
            }
            ProductAction::Clip(ClipProductAction::EditNumericCurve(edit)) => {
                Some(Self::ClipCurve(*edit))
            }
            ProductAction::VisualEffect(VisualEffectProductAction::EditNumericCurve(edit)) => {
                Some(Self::EffectCurve(*edit))
            }
            ProductAction::Audio(AudioProductAction::EditAutomation(edit)) => {
                Some(Self::AudioCurve(edit))
            }
            _ => None,
        }
    }
    fn into_product(self) -> ProductAction {
        match self {
            Self::AudioCurve(edit) => {
                ProductAction::Audio(AudioProductAction::EditAutomation(edit))
            }
            Self::AudioScalar(edit) => {
                let value = edit.value;
                let action = match edit.target.owner {
                    AudioScalarOwner::Parameter { owner, processor_id, parameter_schema, .. } => {
                        AudioProductAction::EditProcessorRack(AudioProcessorRackEditRequest {
                            address: audio_scalar_processor_rack_address(owner),
                            edit: AudioProcessorRackEdit::SetParameterStaticValue {
                                processor_id, parameter_id: parameter_schema.parameter_id, value,
                            },
                        })
                    }
                    AudioScalarOwner::Trim { owner } => AudioProductAction::EditChannelStrip(
                        mondrian_timeline::AudioChannelStripEditRequest { owner, edit: mondrian_timeline::AudioChannelStripEdit::SetInputTrimDb { value } }),
                    AudioScalarOwner::Fader { owner } => AudioProductAction::EditChannelStrip(
                        mondrian_timeline::AudioChannelStripEditRequest { owner, edit: mondrian_timeline::AudioChannelStripEdit::SetFaderDb { value } }),
                    AudioScalarOwner::RouteLevel { route_id, .. } => AudioProductAction::EditRouting(
                        mondrian_timeline::AudioRoutingEditRequest { edit: mondrian_timeline::AudioRoutingEdit::SetRouteGainDb { route_id, value } }),
                    AudioScalarOwner::ProcessorSidechainLevel { route_id, .. } => AudioProductAction::EditRouting(
                        mondrian_timeline::AudioRoutingEditRequest { edit: mondrian_timeline::AudioRoutingEdit::SetProcessorSidechainGainDb { route_id, value } }),
                };
                ProductAction::Audio(action)
            }
            Self::ClipCurve(edit) => {
                ProductAction::Clip(ClipProductAction::EditNumericCurve(Box::new(edit)))
            }
            Self::EffectCurve(edit) => ProductAction::VisualEffect(
                VisualEffectProductAction::EditNumericCurve(Box::new(edit)),
            ),
            Self::Clip(edit) => {
                ProductAction::Clip(ClipProductAction::WriteParameterValues(Box::new(edit)))
            }
            Self::Effect(edit) => ProductAction::VisualEffect(
                VisualEffectProductAction::SetParameterValue(Box::new(edit)),
            ),
            Self::Mask(edit) => ProductAction::VisualMask(
                VisualMaskProductAction::SetParameterValue(Box::new(edit)),
            ),
        }
    }
    fn target(&self) -> ParameterGestureTarget {
        use mondrian_core::automation::ParameterValueWrite;
        let parameter = |write: &ParameterValueWrite| {
            (
                write.parameter().clone(),
                match write {
                    ParameterValueWrite::Channels { channel_values, .. } => {
                        Some(channel_values.iter().map(|(index, _)| *index).collect())
                    }
                    ParameterValueWrite::Value { .. }
                    | ParameterValueWrite::ResetToDefault { .. } => None,
                },
            )
        };
        match self {
            Self::AudioCurve(edit) => ParameterGestureTarget::AudioCurve(edit.target.clone()),
            Self::AudioScalar(edit) => ParameterGestureTarget::AudioScalar(edit.target.clone()),
            Self::ClipCurve(edit) => ParameterCurveGestureTarget::Clip {
                clip_id: edit.clip_id,
                parameter: edit.parameter.clone(),
            }
            .target(),
            Self::EffectCurve(edit) => ParameterCurveGestureTarget::Effect {
                clip_id: edit.clip_id,
                effect_id: edit.effect_id,
                parameter: edit.parameter.clone(),
            }
            .target(),
            Self::Clip(edit) => ParameterGestureTarget::Visual(VisualParameterGestureTarget {
                kind: ParameterGestureKind::Value,
                clip: edit.clip_id,
                owner: ParameterOwner::Clip,
                parameters: edit.writes.iter().map(parameter).collect(),
            }),
            Self::Effect(edit) => ParameterGestureTarget::Visual(VisualParameterGestureTarget {
                kind: ParameterGestureKind::Value,
                clip: edit.clip_id,
                owner: ParameterOwner::Effect(edit.effect_id),
                parameters: vec![parameter(&edit.write)],
            }),
            Self::Mask(edit) => ParameterGestureTarget::Visual(VisualParameterGestureTarget {
                kind: ParameterGestureKind::Value,
                clip: edit.clip_id,
                owner: ParameterOwner::Mask(edit.mask_id),
                parameters: vec![parameter(&edit.write)],
            }),
        }
    }
}

impl ParameterGestureAction {
    /// Encode one typed interaction event for the shared action dispatch seam.
    pub(crate) fn action(self) -> Action {
        Action::Custom {
            namespace: NAMESPACE.into(),
            name: NAME.into(),
            payload: serde_json::json!(self),
        }
    }
    /// Decode this registered protocol, rejecting malformed or unknown operations.
    pub(crate) fn decode(action: &Action) -> Result<Option<Self>> {
        let Action::Custom { namespace, name, payload } = action else {
            return Ok(None);
        };
        if namespace != NAMESPACE {
            return Ok(None);
        }
        if name != NAME {
            return Err(gesture_error("unknown parameter gesture operation"));
        }
        serde_json::from_value(payload.clone())
            .map(Some)
            .map_err(|error| gesture_error(error.to_string()))
    }
}

impl AppState {
    fn current_audio_scalar_processor(
        &self,
        owner: mondrian_audio::AudioProcessorInputInspectionTarget,
        processor_id: mondrian_core::AudioProcessorInstanceId,
        definition: &mondrian_timeline::audio::AudioProcessorDefinitionRef,
    ) -> Option<&mondrian_timeline::audio::AudioProcessorInstance> {
        let sequence = self.active_sequence()?;
        if !mondrian_timeline::inspect_audio_processor_rack(
            sequence,
            &audio_scalar_processor_rack_address(owner),
        )
        .is_ok_and(|inspection| inspection.is_editable())
        {
            return None;
        }
        if let mondrian_audio::AudioProcessorInputInspectionTarget::ProcessingScope {
            scope_id,
            occurrence,
        } = owner
        {
            if self.primary_selected_clip().is_none_or(|selected| {
                selected.is_video_track || selected.clip_id != occurrence.clip_id
            }) {
                return None;
            }
            if let Some(selected) =
                self.selection.audio_scope_occurrences.get(&(sequence.id, scope_id))
            {
                if *selected != occurrence {
                    return None;
                }
            } else {
                // Match the existing Inspector projection: only one binding may
                // be selected implicitly. Ambiguous materializations need an
                // explicit occurrence, never a guessed first component.
                let clip = sequence.find_clip(occurrence.clip_id)?;
                let mut bindings = clip
                    .audio_components
                    .iter()
                    .filter(|edit| edit.processing.scope_id == scope_id);
                if bindings.next().is_none_or(|edit| edit.id != occurrence.edit_id)
                    || bindings.next().is_some()
                {
                    return None;
                }
            }
        }
        let processor =
            mondrian_audio::inspect_audio_processor_input_author(sequence, owner, processor_id)
                .ok()?;
        (processor.definition == *definition).then_some(processor)
    }

    fn audio_scalar_target_is_current(&self, target: &AudioScalarGestureTarget) -> bool {
        if self.authoring_session_id() != Some(target.lease.session_id)
            || self.active_sequence_id() != Some(target.lease.sequence_id)
        {
            return false;
        }
        let Some(sequence) = self.active_sequence() else {
            return false;
        };
        match &target.owner {
            AudioScalarOwner::Parameter { owner, processor_id, definition, parameter_schema } => {
                self.current_audio_scalar_processor(*owner, *processor_id, definition)
                    .is_some_and(|processor| {
                        processor.parameters.get(&parameter_schema.parameter_id).is_some_and(
                            |parameter| {
                                parameter.schema == **parameter_schema
                                    && parameter.schema.numeric.is_some()
                                    && parameter
                                        .automation
                                        .as_ref()
                                        .is_some_and(|curve| curve.keyframes.is_empty())
                            },
                        )
                    })
            }
            AudioScalarOwner::Trim { owner } => {
                mondrian_timeline::inspect_audio_channel_strip(sequence, *owner)
                    .is_ok_and(|inspection| inspection.is_editable())
            }
            AudioScalarOwner::Fader { owner } => mondrian_timeline::inspect_audio_channel_strip(
                sequence, *owner,
            )
            .is_ok_and(|inspection| {
                inspection.is_editable() && inspection.strip().fader_automation.is_none()
            }),
            AudioScalarOwner::RouteLevel { route_id, source, destination } => {
                mondrian_timeline::inspect_audio_route(sequence, *route_id).is_ok_and(
                    |inspection| {
                        let route = inspection.route();
                        inspection.is_editable()
                            && route.source == *source
                            && route.destination == *destination
                            && route.gain_automation.is_none()
                    },
                )
            }
            AudioScalarOwner::ProcessorSidechainLevel {
                route_id,
                source,
                bus_key,
                owner,
                processor_id,
                definition,
            } => {
                self.current_audio_scalar_processor(*owner, *processor_id, definition).is_some()
                    && mondrian_timeline::inspect_audio_processor_sidechain(sequence, *route_id)
                        .is_ok_and(|inspection| {
                            let route = inspection.route();
                            inspection.is_editable()
                                && route.processor_id == *processor_id
                                && route.source == *source
                                && route.bus_key == *bus_key
                                && route.gain_automation.is_none()
                        })
            }
        }
    }

    /// Project a one-shot intent from the same current author and presentation view.
    pub(crate) fn displayed_native_parameter_adoption(
        &self,
        target: AudioScalarGestureTarget,
    ) -> Option<AdoptDisplayedNativeParameterEdit> {
        let edit = AdoptDisplayedNativeParameterEdit {
            expected: self.native_audio_parameter_adoption_snapshot(&target)?,
            target: Box::new(target),
            expected_author_generation: self.project_author_generation(),
        };
        self.validate_displayed_native_parameter_adoption(&edit)?;
        Some(edit)
    }

    fn validate_displayed_native_parameter_adoption(
        &self,
        edit: &AdoptDisplayedNativeParameterEdit,
    ) -> Option<f64> {
        if edit.expected_author_generation == 0
            || self.project_author_generation() != edit.expected_author_generation
            || self.authoring_session_id() != Some(edit.target.lease.session_id)
            || self.active_sequence_id() != Some(edit.target.lease.sequence_id)
        {
            return None;
        }
        let AudioScalarOwner::Parameter {
            owner, processor_id, definition, parameter_schema,
        } = &edit.target.owner else {
            return None;
        };
        let processor =
            self.current_audio_scalar_processor(*owner, *processor_id, definition)?;
        let parameter = processor.parameters.get(&parameter_schema.parameter_id)?;
        if parameter.schema != **parameter_schema
            || parameter.schema.numeric.is_none()
            || parameter.automation.is_some()
        {
            return None;
        }
        let current = self.native_audio_parameter_adoption_snapshot(&edit.target)?;
        if current != edit.expected {
            return None;
        }
        let value = f64::from_bits(current.author_value_bits);
        // Validate through the unique Timeline implementation before finish or
        // author mutation. This clone is never installed or sent to a worker.
        let mut candidate = parameter.clone();
        candidate.set_automation(
            mondrian_core::ExactAutomationCurve::new(
                parameter_schema.parameter_id.clone(), value,
            ).ok()?,
        ).ok()?;
        Some(value)
    }

    fn adopt_displayed_native_parameter(
        &mut self,
        edit: AdoptDisplayedNativeParameterEdit,
    ) -> Result<()> {
        self.validate_displayed_native_parameter_adoption(&edit)
            .ok_or_else(|| gesture_error("displayed native parameter is no longer current"))?;
        self.finish_parameter_gesture()?;
        let value = self.validate_displayed_native_parameter_adoption(&edit)
            .ok_or_else(|| gesture_error("displayed native parameter changed while finishing"))?;
        self.dispatch_product_action(
            ParameterGestureEdit::AudioScalar(AudioScalarValueEdit {
                target: edit.target,
                value,
            }).into_product(),
        )
    }

    fn parameter_target_is_current(&self, target: &ParameterGestureTarget) -> bool {
        let target = match target {
            ParameterGestureTarget::Visual(target) => target,
            ParameterGestureTarget::AudioScalar(target) => {
                return self.audio_scalar_target_is_current(target);
            }
            ParameterGestureTarget::AudioCurve(target) => {
                let Some(sequence) = self.active_sequence() else {
                    return false;
                };
                if !mondrian_timeline::inspect_audio_automation(sequence, target)
                    .is_ok_and(|inspection| inspection.is_editable())
                {
                    return false;
                }
                return match target {
                    AudioAutomationTarget::ComponentVolume { clip_id, .. }
                    | AudioAutomationTarget::ComponentPan { clip_id, .. } => self
                        .primary_selected_clip()
                        .is_some_and(|selection| selection.clip_id == *clip_id),
                    AudioAutomationTarget::ProcessingScopeInputGain { scope_id }
                    | AudioAutomationTarget::ProcessorParameter {
                        rack: AudioProcessorRackAddress::ProcessingScope { scope_id },
                        ..
                    } => self
                        .primary_selected_clip()
                        .and_then(|selection| sequence.find_clip(selection.clip_id))
                        .is_some_and(|clip| {
                            clip.audio_components
                                .iter()
                                .any(|edit| edit.processing.scope_id == *scope_id)
                        }),
                    _ => true,
                };
            }
        };
        if self
            .primary_selected_clip()
            .is_none_or(|selection| selection.clip_id != target.clip)
        {
            return false;
        }
        let Some(sequence) = self.active_sequence() else {
            return false;
        };
        if sequence
            .clip_track_location(target.clip)
            .is_none_or(|location| location.is_locked)
        {
            return false;
        }
        let Some(clip) = sequence.find_clip(target.clip) else {
            return false;
        };
        match target.owner {
            ParameterOwner::Clip => true,
            ParameterOwner::Effect(id) => clip.effects.iter().any(|effect| effect.id == id),
            ParameterOwner::Mask(id) => clip.mask(id).is_some_and(|mask| !mask.locked),
        }
    }

    fn begin_parameter_gesture(
        &mut self,
        id: AuthoringGestureId,
        target: ParameterGestureTarget,
    ) -> Result<bool> {
        self.finish_parameter_gesture()?;
        if !self.parameter_target_is_current(&target) {
            return Ok(false);
        }
        if let ParameterGestureTarget::Visual(target) = &target
            && target.kind == ParameterGestureKind::NumericCurve
        {
            let clip = self
                .active_sequence()
                .and_then(|sequence| sequence.find_clip(target.clip))
                .ok_or_else(|| gesture_error("missing curve Clip"))?;
            let valid = match target.owner {
                ParameterOwner::Clip => {
                    let bag = clip.intrinsic_parameter_bag();
                    target.parameters.iter().all(|(address, _)| {
                        bag.property_by_address(address)
                            .is_some_and(|(_, property)| property.descriptor.schema.is_animatable)
                    })
                }
                ParameterOwner::Effect(id) => {
                    clip.effects.iter().find(|effect| effect.id == id).is_some_and(|effect| {
                        target.parameters.iter().all(|(address, _)| {
                            effect.properties.property_by_address(address).is_some_and(
                                |(_, property)| property.descriptor.schema.is_animatable,
                            )
                        })
                    })
                }
                ParameterOwner::Mask(_) => false,
            };
            if !valid {
                return Err(gesture_error("invalid numeric curve parameter owner"));
            }
        }
        let sequence =
            self.active_sequence_id().ok_or_else(|| gesture_error("no active Sequence"))?;
        let session =
            self.authoring.as_mut().ok_or_else(|| gesture_error("no authoring Session"))?;
        session.begin_sequence_gesture(id, sequence)?;
        self.active_parameter_gesture = Some(ActiveParameterGesture {
            id,
            session: session.session_id(),
            sequence,
            target,
        });
        Ok(true)
    }

    /// Whether the UI owner and its authoritative author-generation lease still match.
    pub(crate) fn parameter_gesture_is_current(&self) -> bool {
        self.active_parameter_gesture.as_ref().is_some_and(|active| {
            self.authoring_session_id() == Some(active.session)
                && self.active_sequence_id() == Some(active.sequence)
                && self.parameter_target_is_current(&active.target)
                && self.authoring.as_ref().is_some_and(|session| {
                    session.sequence_gesture_is_current(active.id, active.sequence)
                })
        })
    }

    /// Whether one matching pointer identity owns the active author interaction.
    pub(crate) fn parameter_gesture_owns(&self, id: AuthoringGestureId) -> bool {
        self.active_parameter_gesture.as_ref().is_some_and(|active| active.id == id)
    }

    /// Seal the active interaction before discarding its UI owner identity.
    pub(crate) fn finish_parameter_gesture(&mut self) -> Result<()> {
        let Some((id, owner)) =
            self.active_parameter_gesture.as_ref().map(|active| (active.id, active.session))
        else {
            return Ok(());
        };
        if self.authoring_session_id() == Some(owner)
            && let Some(session) = self.authoring.as_mut()
        {
            session.end_sequence_gesture(id)?;
        }
        // Keep the lease available for retry if atomic history sealing fails.
        self.active_parameter_gesture = None;
        Ok(())
    }

    /// Restore a current interaction through canonical Session cancellation.
    fn cancel_parameter_gesture(&mut self, id: AuthoringGestureId) -> Result<()> {
        if self.active_parameter_gesture.as_ref().is_none_or(|active| active.id != id) {
            return Ok(());
        }
        if !self.parameter_gesture_is_current() {
            return self.finish_parameter_gesture();
        }
        let commit = self
            .authoring
            .as_mut()
            .ok_or_else(|| gesture_error("no authoring Session"))?
            .cancel_sequence_gesture(id)?;
        self.active_parameter_gesture = None;
        if let Some(commit) = commit {
            self.consume_authoring_commit(commit)?;
        }
        Ok(())
    }

    /// Apply live parameter intent through the existing author modules in one explicit group.
    pub(super) fn dispatch_parameter_gesture(
        &mut self,
        action: ParameterGestureAction,
    ) -> Result<()> {
        let (id, edit, begin) = match action {
            ParameterGestureAction::AdoptDisplayedNativeParameter { edit } => {
                return self.adopt_displayed_native_parameter(edit);
            }
            ParameterGestureAction::SetAudioScalar { edit } => {
                if !self.audio_scalar_target_is_current(&edit.target) {
                    return Err(gesture_error("audio scalar author owner changed"));
                }
                self.finish_parameter_gesture()?;
                return self.dispatch_product_action(
                    ParameterGestureEdit::AudioScalar(edit).into_product(),
                );
            }
            ParameterGestureAction::BeginCurve { gesture_id, target } => {
                self.begin_parameter_gesture(gesture_id, target.target())?;
                return Ok(());
            }
            ParameterGestureAction::Cancel { gesture_id } => {
                return self.cancel_parameter_gesture(gesture_id)
            }
            ParameterGestureAction::Begin { gesture_id, edit } => (gesture_id, edit, true),
            ParameterGestureAction::Update { gesture_id, edit } => (gesture_id, edit, false),
            ParameterGestureAction::Finish { gesture_id } => {
                if self
                    .active_parameter_gesture
                    .as_ref()
                    .is_some_and(|active| active.id == gesture_id)
                {
                    self.finish_parameter_gesture()?;
                }
                return Ok(());
            }
        };
        let target = edit.target();
        let current_intent = if begin {
            self.parameter_target_is_current(&target)
        } else {
            self.parameter_gesture_is_current()
                && self
                    .active_parameter_gesture
                    .as_ref()
                    .is_some_and(|active| active.id == id && active.target == target)
        };
        // This first retained-F consumer admits the original nongesture
        // prepared ticket. Live gesture updates do not yet expose that split.
        // Refuse before opening a group or entering error->cancel restoration.
        if current_intent
            && self.is_playing()
            && let ParameterGestureEdit::AudioScalar(scalar) = &edit
            && let AudioScalarOwner::Fader {
                owner: mondrian_timeline::AudioChannelStripOwner::ProgramOutput { output_id },
            } = &scalar.target.owner
            && self
                .active_sequence()
                .and_then(|sequence| sequence.audio_program.outputs.first())
                .is_some_and(|output| {
                    output.id == *output_id
                        && output.strip.fader_db.to_bits() != scalar.value.to_bits()
                })
            && self.host_output_fader_native_renderer()?.is_some()
        {
            return Err(MondrianError::ActionNotExecuted {
                action: "authoring_commit".to_owned(),
                reason: "playing Host output fader gestures require prepared author publication"
                    .to_owned(),
            });
        }
        if begin {
            // Reject a queued stale scalar start before sealing any other live
            // gesture. A refused control cannot advance author history.
            if matches!(&target, ParameterGestureTarget::AudioScalar(_))
                && !self.parameter_target_is_current(&target)
            {
                return Ok(());
            }
            if !self.begin_parameter_gesture(id, target)? {
                return Ok(());
            }
        } else if !self.parameter_gesture_is_current()
            || self
                .active_parameter_gesture
                .as_ref()
                .is_none_or(|active| active.id != id || active.target != target)
        {
            return Ok(());
        }
        use super::product_action::ClipCurveEditPayload;
        let positional = match &edit {
            ParameterGestureEdit::ClipCurve(edit) => matches!(
                edit.edit,
                ClipCurveEditPayload::Move { .. }
                    | ClipCurveEditPayload::TransformMany { .. }
                    | ClipCurveEditPayload::EditHandle { .. }
                    | ClipCurveEditPayload::Upsert { keyframe_id: Some(_), .. }
            ),
            ParameterGestureEdit::EffectCurve(edit) => matches!(
                edit.edit,
                ClipCurveEditPayload::Move { .. }
                    | ClipCurveEditPayload::TransformMany { .. }
                    | ClipCurveEditPayload::EditHandle { .. }
                    | ClipCurveEditPayload::Upsert { keyframe_id: Some(_), .. }
            ),
            ParameterGestureEdit::AudioCurve(edit) => matches!(
                edit.edit,
                mondrian_timeline::AudioAutomationEdit::EditHandle { .. }
                    | mondrian_timeline::AudioAutomationEdit::TransformKeyframes { .. }
                    | mondrian_timeline::AudioAutomationEdit::UpsertKeyframe { .. }
            ),
            _ => false,
        };
        self.parameter_gesture_command = Some(if positional {
            ParameterGestureCommand::CurveOrigin(id)
        } else {
            ParameterGestureCommand::Current(id)
        });
        let result = self.dispatch_product_action(edit.into_product());
        self.parameter_gesture_command = None;
        if result.is_err() {
            self.cancel_parameter_gesture(id)?;
        }
        result
    }
}

fn gesture_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "parameter_gesture".into(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::product_action::ClipCurveEditPayload;
    use mondrian_core::automation::{ParameterValueWrite, PropertyValue};
    use mondrian_core::{
        mask_data::{MaskComponent, MaskEvaluation, MASK_PROP_OPACITY},
        TimelineTime,
    };
    use mondrian_effects::EffectType;
    use mondrian_timeline::{clip::Transform2D, Clip, Sequence};

    #[derive(Clone, Copy)]
    enum TestOwner {
        Clip,
        Effect,
        Mask,
    }

    fn fixture(owner: TestOwner) -> (AppState, ParameterGestureEdit) {
        let mut clip = Clip::new_basic_title(
            "Title",
            "Arial",
            TimelineTime::ZERO,
            TimelineTime::new(1, 1).expect("duration"),
        )
        .expect("generated Clip");
        let clip_id = clip.id;
        let edit = match owner {
            TestOwner::Clip => {
                let parameter = clip
                    .intrinsic_parameter_bag()
                    .address_for_path(Transform2D::OPACITY_PATH)
                    .expect("opacity");
                ParameterGestureEdit::Clip(ClipWriteParameterValuesPayload {
                    clip_id,
                    writes: vec![ParameterValueWrite::Value {
                        parameter,
                        value: PropertyValue::Float(0.2),
                    }],
                })
            }
            TestOwner::Effect => {
                let effect = mondrian_effects::instantiate_effect_node(EffectType::GaussianBlur)
                    .expect("effect");
                let parameter = effect
                    .properties
                    .iter()
                    .find(|(_, property)| {
                        matches!(property.static_value(), PropertyValue::Float(_))
                    })
                    .expect("numeric property")
                    .1
                    .address();
                let effect_id = effect.id;
                clip.add_effect_node(effect);
                ParameterGestureEdit::Effect(VisualEffectSetParameterValuePayload {
                    clip_id,
                    effect_id,
                    write: ParameterValueWrite::Value {
                        parameter,
                        value: PropertyValue::Float(0.2),
                    },
                })
            }
            TestOwner::Mask => {
                let mask = MaskComponent::new("Mask".into(), MaskEvaluation::default());
                let parameter =
                    mask.properties.property(MASK_PROP_OPACITY).expect("opacity").address();
                let mask_id = mask.id;
                clip.add_mask_component(mask);
                ParameterGestureEdit::Mask(VisualMaskSetParameterValuePayload {
                    clip_id,
                    mask_id,
                    write: ParameterValueWrite::Value {
                        parameter,
                        value: PropertyValue::Float(0.2),
                    },
                })
            }
        };
        let mut sequence = Sequence::new("Gesture acceptance");
        sequence.video_tracks[0].add_clip(clip).expect("Clip");
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        state.select_clip_by_id(clip_id).expect("select owner");
        (state, edit)
    }

    fn with_value(mut edit: ParameterGestureEdit, value: f32) -> ParameterGestureEdit {
        let write = match &mut edit {
            ParameterGestureEdit::Clip(edit) => &mut edit.writes[0],
            ParameterGestureEdit::Effect(edit) => &mut edit.write,
            ParameterGestureEdit::Mask(edit) => &mut edit.write,
            ParameterGestureEdit::ClipCurve(_)
            | ParameterGestureEdit::EffectCurve(_)
            | ParameterGestureEdit::AudioCurve(_)
            | ParameterGestureEdit::AudioScalar(_) => {
                panic!("scalar fixture expected")
            }
        };
        let ParameterValueWrite::Value { value: current, .. } = write else {
            panic!("scalar fixture");
        };
        *current = PropertyValue::Float(value);
        edit
    }

    fn numeric_curve_fixture(
        owner: TestOwner,
    ) -> (
        AppState,
        ParameterCurveGestureTarget,
        ParameterGestureEdit,
        ParameterGestureEdit,
    ) {
        let (state, value_edit) = fixture(owner);
        let (target, curve_edit) = match &value_edit {
            ParameterGestureEdit::Clip(value) => {
                let parameter = value.writes[0].parameter().clone();
                (
                    ParameterCurveGestureTarget::Clip {
                        clip_id: value.clip_id,
                        parameter: parameter.clone(),
                    },
                    ParameterGestureEdit::ClipCurve(ClipEditNumericCurvePayload {
                        clip_id: value.clip_id,
                        parameter: parameter.clone(),
                        edit: ClipCurveEditPayload::Upsert {
                            keyframe_id: None,
                            point: crate::app::test_curve_authoring::curve_author_point(
                                &state,
                                value.clip_id,
                                &parameter,
                                0.5,
                                0.8,
                            ),
                        },
                    }),
                )
            }
            ParameterGestureEdit::Effect(value) => {
                let parameter = value.write.parameter().clone();
                (
                    ParameterCurveGestureTarget::Effect {
                        clip_id: value.clip_id,
                        effect_id: value.effect_id,
                        parameter: parameter.clone(),
                    },
                    ParameterGestureEdit::EffectCurve(VisualEffectEditNumericCurvePayload {
                        clip_id: value.clip_id,
                        effect_id: value.effect_id,
                        parameter: parameter.clone(),
                        edit: ClipCurveEditPayload::Upsert {
                            keyframe_id: None,
                            point: crate::app::test_curve_authoring::curve_author_point(
                                &state,
                                value.clip_id,
                                &parameter,
                                0.5,
                                0.8,
                            ),
                        },
                    }),
                )
            }
            _ => panic!("numeric fixture"),
        };
        (state, target, curve_edit, value_edit)
    }

    fn with_curve_payload(
        mut edit: ParameterGestureEdit,
        payload: super::super::product_action::ClipCurveEditPayload,
    ) -> ParameterGestureEdit {
        match &mut edit {
            ParameterGestureEdit::ClipCurve(edit) => edit.edit = payload,
            ParameterGestureEdit::EffectCurve(edit) => edit.edit = payload,
            _ => panic!("visual numeric curve fixture"),
        }
        edit
    }

    fn curve_property(
        state: &AppState,
        target: &ParameterCurveGestureTarget,
    ) -> mondrian_core::automation::AnimatedProperty {
        match target {
            ParameterCurveGestureTarget::Clip { clip_id, parameter } => state
                .active_sequence()
                .expect("Sequence")
                .find_clip(*clip_id)
                .expect("Clip")
                .intrinsic_parameter_bag()
                .property_by_address(parameter)
                .expect("property")
                .1
                .clone(),
            ParameterCurveGestureTarget::Effect { clip_id, effect_id, parameter } => state
                .active_sequence()
                .expect("Sequence")
                .find_clip(*clip_id)
                .expect("Clip")
                .effects
                .iter()
                .find(|effect| effect.id == *effect_id)
                .expect("Effect")
                .properties
                .property_by_address(parameter)
                .expect("property")
                .1
                .clone(),
            _ => panic!("visual curve fixture"),
        }
    }

    #[test]
    fn curve_origin_candidate_time_upserts_retain_each_distinct_key() {
        for owner in [TestOwner::Clip, TestOwner::Effect] {
            let (mut state, target, edit, _) = numeric_curve_fixture(owner);
            let original = state.active_sequence().expect("origin").clone();
            let id = AuthoringGestureId::new();
            state
                .dispatch_action(
                    ParameterGestureAction::BeginCurve { gesture_id: id, target: target.clone() }
                        .action(),
                )
                .expect("begin");
            let (clip_id, parameter) = match &edit {
                ParameterGestureEdit::ClipCurve(edit) => (edit.clip_id, &edit.parameter),
                ParameterGestureEdit::EffectCurve(edit) => (edit.clip_id, &edit.parameter),
                _ => panic!("curve owner"),
            };
            for ratio in [0.25, 0.75] {
                state
                    .dispatch_action(
                        ParameterGestureAction::Update {
                            gesture_id: id,
                            edit: with_curve_payload(
                                edit.clone(),
                                ClipCurveEditPayload::Upsert {
                                    keyframe_id: None,
                                    point: crate::app::test_curve_authoring::curve_author_point(
                                        &state, clip_id, parameter, ratio, 0.8,
                                    ),
                                },
                            ),
                        }
                        .action(),
                    )
                    .expect("time-addressed insertion");
            }
            assert_eq!(
                curve_property(&state, &target).keyframe_times(),
                vec![
                    TimelineTime::new(1, 4).expect("quarter"),
                    TimelineTime::new(3, 4).expect("three quarters")
                ],
                "time-addressed inserts are cumulative author operations"
            );
            state
                .dispatch_action(ParameterGestureAction::Finish { gesture_id: id }.action())
                .expect("finish");
            assert!(state.undo_timeline().expect("one Undo"));
            assert!(state
                .active_sequence()
                .expect("restored")
                .author_state_eq_ignoring_revision(&original));
            assert!(state.redo_timeline().expect("Redo"));
            assert_eq!(curve_property(&state, &target).keyframe_times().len(), 2);
        }
    }

    #[test]
    fn curve_origin_candidate_move_admits_a_key_created_in_the_same_gesture() {
        use crate::app::product_action::ClipCurveEditPayload;
        for owner in [TestOwner::Clip, TestOwner::Effect] {
            let (mut state, target, edit, _) = numeric_curve_fixture(owner);
            let original = state.active_sequence().expect("origin").clone();
            let id = AuthoringGestureId::new();
            state
                .dispatch_action(
                    ParameterGestureAction::BeginCurve { gesture_id: id, target: target.clone() }
                        .action(),
                )
                .expect("begin");
            state
                .dispatch_action(
                    ParameterGestureAction::Update { gesture_id: id, edit: edit.clone() }.action(),
                )
                .expect("insert");
            let key = curve_property(&state, &target)
                .keyframe_at(TimelineTime::new(1, 2).expect("half"))
                .expect("inserted key");
            state
                .dispatch_action(
                    ParameterGestureAction::Update {
                        gesture_id: id,
                        edit: with_curve_payload(
                            edit,
                            ClipCurveEditPayload::Move {
                                keyframe_id: key.id,
                                time: Some(TimelineTime::new(3, 4).expect("new time")),
                                value: None,
                            },
                        ),
                    }
                    .action(),
                )
                .expect("move a key created during this same interaction");
            let property = curve_property(&state, &target);
            assert_eq!(
                property.keyframe_times(),
                vec![TimelineTime::new(3, 4).expect("new time")]
            );
            let moved = property.keyframe_by_id(key.id).expect("retained identity");
            assert_eq!(
                moved.value, key.value,
                "omitted value retains the current authored scalar"
            );
            state
                .dispatch_action(ParameterGestureAction::Finish { gesture_id: id }.action())
                .expect("finish");
            assert!(state.undo_timeline().expect("Undo"));
            assert!(state
                .active_sequence()
                .expect("restored")
                .author_state_eq_ignoring_revision(&original));
            assert!(state.redo_timeline().expect("Redo"));
            assert_eq!(
                curve_property(&state, &target).keyframe_by_id(key.id).expect("stable Redo"),
                moved
            );
        }
    }

    #[test]
    fn deferred_numeric_curve_gestures_share_exact_owner_history_and_undo_redo() {
        for owner in [TestOwner::Clip, TestOwner::Effect] {
            let (mut state, target, curve_edit, value_edit) = numeric_curve_fixture(owner);
            let original = state.active_sequence().expect("origin").clone();
            let before = state.authoring_history().expect("history").diagnostics();
            let id = AuthoringGestureId::new();
            state
                .dispatch_action(
                    ParameterGestureAction::BeginCurve { gesture_id: id, target }.action(),
                )
                .expect("open local draft");
            assert!(state.parameter_gesture_is_current());
            assert_eq!(state.active_sequence().expect("draft"), &original);
            assert_eq!(
                state.authoring_history().expect("history").diagnostics(),
                before
            );
            state
                .dispatch_action(
                    ParameterGestureAction::Update {
                        gesture_id: id,
                        edit: with_value(value_edit, 0.7),
                    }
                    .action(),
                )
                .expect("foreign edit family ignored");
            assert_eq!(state.active_sequence().expect("draft"), &original);
            state
                .dispatch_action(
                    ParameterGestureAction::Update { gesture_id: id, edit: curve_edit.clone() }
                        .action(),
                )
                .expect("release curve edit");
            assert!(
                state.parameter_gesture_is_current(),
                "the canonical curve commit must retain its owner generation lease"
            );
            assert_eq!(
                state.authoring_history().expect("history").diagnostics().pending_entries,
                1
            );
            let updated = state.active_sequence().expect("updated").clone();
            let updated_history = state.authoring_history().expect("history").diagnostics();
            state
                .dispatch_action(
                    ParameterGestureAction::Update { gesture_id: id, edit: curve_edit }.action(),
                )
                .expect("identical curve update keeps the gesture");
            assert!(state.parameter_gesture_is_current());
            assert_eq!(state.active_sequence().expect("unchanged update"), &updated);
            assert_eq!(
                state.authoring_history().expect("history").diagnostics(),
                updated_history
            );
            state
                .dispatch_action(ParameterGestureAction::Finish { gesture_id: id }.action())
                .expect("finish curve");
            let accepted = state.active_sequence().expect("accepted").clone();
            assert!(!accepted.author_state_eq_ignoring_revision(&original));
            assert_eq!(
                state.authoring_history().expect("history").diagnostics().undo_entries,
                before.undo_entries + 1
            );
            assert!(state.undo_timeline().expect("Undo curve"));
            assert!(state
                .active_sequence()
                .expect("Undo")
                .author_state_eq_ignoring_revision(&original));
            assert!(state.redo_timeline().expect("Redo curve"));
            assert!(state
                .active_sequence()
                .expect("Redo")
                .author_state_eq_ignoring_revision(&accepted));
        }
    }

    #[test]
    fn parameter_gesture_cancel_restores_all_three_owners_and_original_redo() {
        for owner in [TestOwner::Clip, TestOwner::Effect, TestOwner::Mask] {
            let (mut state, edit) = fixture(owner);
            state
                .dispatch_action(
                    ParameterGestureAction::Begin {
                        gesture_id: AuthoringGestureId::new(),
                        edit: with_value(edit.clone(), 0.8),
                    }
                    .action(),
                )
                .expect("original branch begin");
            state.finish_parameter_gesture().expect("original branch finish");
            assert!(state.undo_timeline().expect("original branch Undo"));
            let original = state.active_sequence().expect("origin").clone();
            let history = state.authoring_history().expect("history").diagnostics();
            let id = AuthoringGestureId::new();
            state
                .dispatch_action(
                    ParameterGestureAction::Begin { gesture_id: id, edit: edit.clone() }.action(),
                )
                .expect("begin");
            state
                .dispatch_action(
                    ParameterGestureAction::Update {
                        gesture_id: id,
                        edit: with_value(edit.clone(), 0.6),
                    }
                    .action(),
                )
                .expect("live");
            assert!(!state
                .active_sequence()
                .expect("live")
                .author_state_eq_ignoring_revision(&original));
            assert_eq!(
                state.authoring_history().expect("history").diagnostics().redo_entries,
                history.redo_entries
            );
            state
                .dispatch_action(
                    ParameterGestureAction::Cancel { gesture_id: AuthoringGestureId::new() }
                        .action(),
                )
                .expect("foreign ignored");
            assert!(state.parameter_gesture_is_current());
            state
                .dispatch_action(ParameterGestureAction::Cancel { gesture_id: id }.action())
                .expect("cancel");
            assert!(state.active_parameter_gesture.is_none());
            assert!(state
                .active_sequence()
                .expect("origin")
                .author_state_eq_ignoring_revision(&original));
            assert_eq!(
                state.authoring_history().expect("history").diagnostics(),
                history
            );
            state
                .dispatch_action(ParameterGestureAction::Finish { gesture_id: id }.action())
                .expect("late finish ignored");
            state
                .dispatch_action(
                    ParameterGestureAction::Update { gesture_id: id, edit: with_value(edit, 0.4) }
                        .action(),
                )
                .expect("late update ignored");
            assert_eq!(
                state.authoring_history().expect("history").diagnostics(),
                history
            );
            assert!(state.redo_timeline().expect("original Redo"));
            assert!(!state
                .active_sequence()
                .expect("redone branch")
                .author_state_eq_ignoring_revision(&original));
        }
    }

    #[test]
    fn parameter_gesture_failed_update_cancels_the_entire_interaction() {
        let (mut state, edit) = fixture(TestOwner::Clip);
        let original = state.active_sequence().expect("origin").clone();
        let history = state.authoring_history().expect("history").diagnostics();
        let id = AuthoringGestureId::new();
        state
            .dispatch_action(
                ParameterGestureAction::Begin { gesture_id: id, edit: edit.clone() }.action(),
            )
            .expect("begin");
        assert!(
            state
                .dispatch_action(
                    ParameterGestureAction::Update { gesture_id: id, edit: with_value(edit, 2.0) }
                        .action()
                )
                .is_err(),
            "invalid opacity rejects the write"
        );
        assert!(state.active_parameter_gesture.is_none());
        assert!(state
            .active_sequence()
            .expect("restored")
            .author_state_eq_ignoring_revision(&original));
        assert_eq!(
            state.authoring_history().expect("history").diagnostics(),
            history
        );
    }

    #[test]
    fn clip_effect_and_mask_parameter_gestures_share_live_authoring_and_one_undo_redo() {
        for owner in [TestOwner::Clip, TestOwner::Effect, TestOwner::Mask] {
            let (mut state, edit) = fixture(owner);
            let before = state.active_sequence().expect("Sequence").clone();
            let entries = state.authoring_history().expect("history").diagnostics().undo_entries;
            let gesture_id = AuthoringGestureId::new();
            state
                .dispatch_action(
                    ParameterGestureAction::Begin { gesture_id, edit: edit.clone() }.action(),
                )
                .expect("begin live");
            let first = state.active_sequence().expect("first").clone();
            assert!(!first.author_state_eq_ignoring_revision(&before));
            for value in [0.4, 0.6] {
                state
                    .dispatch_action(
                        ParameterGestureAction::Update {
                            gesture_id,
                            edit: with_value(edit.clone(), value),
                        }
                        .action(),
                    )
                    .expect("live update");
                assert!(state.parameter_gesture_is_current());
                assert_eq!(
                    state.authoring_history().expect("history").diagnostics().undo_entries,
                    entries + 1
                );
            }
            let after = state.active_sequence().expect("last").clone();
            assert!(!after.author_state_eq_ignoring_revision(&first));
            state
                .dispatch_action(ParameterGestureAction::Finish { gesture_id }.action())
                .expect("finish");
            assert!(state.active_parameter_gesture.is_none());
            assert!(state.undo_timeline().expect("one Undo"));
            assert!(state
                .active_sequence()
                .expect("restored")
                .author_state_eq_ignoring_revision(&before));
            assert!(state.redo_timeline().expect("one Redo"));
            assert!(state
                .active_sequence()
                .expect("redone")
                .author_state_eq_ignoring_revision(&after));
        }
    }

    #[test]
    fn interleaved_owner_lock_and_delayed_gesture_events_do_not_mutate_or_close_newer_edit() {
        use crate::app::ui_actions::{
            track_set_author_control_action, TrackAuthorControl, TrackSetAuthorControlPayload,
        };
        let (mut state, edit) = fixture(TestOwner::Clip);
        let original = state.active_sequence().expect("Sequence").clone();
        let old = AuthoringGestureId::new();
        state
            .dispatch_action(
                ParameterGestureAction::Begin { gesture_id: old, edit: edit.clone() }.action(),
            )
            .expect("begin");
        state
            .dispatch_action(
                ParameterGestureAction::Update {
                    gesture_id: old,
                    edit: with_value(edit.clone(), 0.4),
                }
                .action(),
            )
            .expect("update");
        let track_id = state.active_sequence().expect("Sequence").video_tracks[0].id;
        state
            .dispatch_action(track_set_author_control_action(
                TrackSetAuthorControlPayload {
                    track_id,
                    control: TrackAuthorControl::Lock,
                    enabled: true,
                },
            ))
            .expect("lock");
        let locked = state.active_sequence().expect("locked").clone();
        state
            .dispatch_action(
                ParameterGestureAction::Update {
                    gesture_id: old,
                    edit: with_value(edit.clone(), 0.8),
                }
                .action(),
            )
            .expect("stale event handled");
        assert_eq!(state.active_sequence().expect("unchanged"), &locked);
        assert!(state.undo_timeline().expect("Undo lock"));
        let restored_drag = state.active_sequence().expect("drag").clone();
        let new = AuthoringGestureId::new();
        state
            .dispatch_action(
                ParameterGestureAction::Begin {
                    gesture_id: new,
                    edit: with_value(edit.clone(), 0.6),
                }
                .action(),
            )
            .expect("new gesture");
        let latest = state.active_sequence().expect("new").clone();
        for action in [
            ParameterGestureAction::Finish { gesture_id: old },
            ParameterGestureAction::Update { gesture_id: old, edit: with_value(edit, 0.8) },
        ] {
            state.dispatch_action(action.action()).expect("delayed old event");
            assert!(state.parameter_gesture_is_current());
            assert_eq!(state.active_sequence().expect("unchanged new"), &latest);
        }
        state
            .dispatch_action(ParameterGestureAction::Finish { gesture_id: new }.action())
            .expect("finish new");
        state.undo_timeline().expect("Undo new");
        assert!(state
            .active_sequence()
            .expect("first drag")
            .author_state_eq_ignoring_revision(&restored_drag));
        state.undo_timeline().expect("Undo original gesture");
        assert!(state
            .active_sequence()
            .expect("original")
            .author_state_eq_ignoring_revision(&original));
    }
}
