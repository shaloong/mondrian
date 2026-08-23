//! Unified stable-address authoring for Sequence audio automation.
//!
//! Every audio control that owns an exact numeric curve is addressed through
//! this Module. Mixer, Inspector, plugin, and scripting Adapters therefore do
//! not need to know which persistent struct stores a curve, which owner-time
//! domain gives its keyframe coordinates meaning, or how Track locks propagate
//! through shared Processing Scopes.

use crate::audio::{
    AudioAuthoringError, AudioChannelStripOwner, AUDIO_GAIN_DB_MAX, AUDIO_GAIN_DB_MIN,
    CLIP_PAN_PARAMETER_ID, CLIP_VOLUME_DB_PARAMETER_ID, FADER_DB_PARAMETER_ID,
    INPUT_GAIN_DB_PARAMETER_ID, ROUTE_GAIN_DB_PARAMETER_ID,
};
use crate::audio_processor_edit::{
    inspect_audio_processor_rack, AudioChannelStripRack, AudioProcessorRackAddress,
};
use crate::sequence::Sequence;
use mondrian_core::{
    AudioComponentEditId, AudioProcessingScopeId, AudioProcessorInstanceId, AudioRouteId,
    AuthoringTimeDomain, ClipId, ExactAutomationCurve, ExactAutomationKeyframe, KeyframeId,
    MixBusId, ParameterId, ParameterInterpolation, ParameterNumericRange, ProgramOutputId,
    PropertyValueType, TrackId,
};
use serde::{Deserialize, Serialize};

/// Stable address of one numeric audio-automation curve.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AudioAutomationTarget {
    /// Placement-local volume for one exact audio Component Edit.
    ComponentVolume {
        /// Owning audio Track.
        track_id: TrackId,
        /// Owning Clip occurrence.
        clip_id: ClipId,
        /// Stable placement-local edit identity.
        edit_id: AudioComponentEditId,
    },
    /// Placement-local stereo pan/balance for one exact audio Component Edit.
    ComponentPan {
        /// Owning audio Track.
        track_id: TrackId,
        /// Owning Clip occurrence.
        clip_id: ClipId,
        /// Stable placement-local edit identity.
        edit_id: AudioComponentEditId,
    },
    /// Input gain in a shareable, non-placement Processing Scope.
    ProcessingScopeInputGain {
        /// Stable Processing Scope identity.
        scope_id: AudioProcessingScopeId,
    },
    /// Sequence-local fader for a Track, Bus, or Program Output strip.
    ChannelFader {
        /// Stable typed Channel Strip owner.
        owner: AudioChannelStripOwner,
    },
    /// Sequence-local level on one principal Route or Send.
    RouteGain {
        /// Stable Route identity.
        route_id: AudioRouteId,
    },
    /// One definition-backed parameter in an addressed Processor Rack.
    ProcessorParameter {
        /// Stable Rack address and owner-time domain.
        rack: AudioProcessorRackAddress,
        /// Stable Processor Instance identity.
        processor_id: AudioProcessorInstanceId,
        /// Stable parameter schema identity.
        parameter_id: ParameterId,
    },
}

/// One incremental mutation of an exact automation curve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum AudioAutomationEdit {
    /// Insert, replace, or move one keyframe while preserving stable identity.
    UpsertKeyframe {
        /// Complete keyframe state in the target's owner-time domain.
        keyframe: ExactAutomationKeyframe,
    },
    /// Remove exactly one keyframe by stable identity.
    RemoveKeyframe {
        /// Keyframe to remove.
        keyframe_id: KeyframeId,
    },
    /// Remove every keyframe and retain one explicit unkeyed value.
    Clear {
        /// Definition-domain value retained after clearing the curve.
        default_value: f64,
    },
}

/// Complete audio-automation edit intent for one author transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioAutomationEditRequest {
    /// Stable semantic curve address.
    pub target: AudioAutomationTarget,
    /// Incremental curve mutation.
    pub edit: AudioAutomationEdit,
}

/// Numeric editor contract for one automation target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioAutomationValueContract {
    /// Definition-stable numeric representation.
    pub value_type: PropertyValueType,
    /// Complete admitted definition-domain interval.
    pub hard_range: ParameterNumericRange,
    /// Ordinary interactive editor interval.
    pub soft_range: ParameterNumericRange,
    /// Optional definition-domain editor step.
    pub step: Option<f64>,
}

/// Read-only projection of one resolved audio automation target.
#[derive(Debug, Clone)]
pub struct AudioAutomationInspection<'a> {
    target: AudioAutomationTarget,
    time_domain: AuthoringTimeDomain,
    parameter_id: ParameterId,
    default_value: f64,
    curve: Option<&'a ExactAutomationCurve>,
    value_contract: AudioAutomationValueContract,
    allowed_interpolations: Vec<ParameterInterpolation>,
    edit_blocker: Option<AudioAutomationEditBlocker>,
}

impl<'a> AudioAutomationInspection<'a> {
    /// Stable target resolved by this snapshot-local inspection.
    pub fn target(&self) -> &AudioAutomationTarget {
        &self.target
    }

    /// Explicit owner-time domain of every keyframe in this curve.
    pub fn time_domain(&self) -> AuthoringTimeDomain {
        self.time_domain
    }

    /// Stable parameter schema identity.
    pub fn parameter_id(&self) -> &ParameterId {
        &self.parameter_id
    }

    /// Unkeyed value, also used when an optional curve has not been activated.
    pub fn default_value(&self) -> f64 {
        self.default_value
    }

    /// Exact curve when automation is active or intrinsically owned.
    pub fn curve(&self) -> Option<&'a ExactAutomationCurve> {
        self.curve
    }

    /// Numeric hard/soft ranges used by product Adapters.
    pub fn value_contract(&self) -> AudioAutomationValueContract {
        self.value_contract
    }

    /// Interpolation modes admitted by the target definition.
    pub fn allowed_interpolations(&self) -> &[ParameterInterpolation] {
        &self.allowed_interpolations
    }

    /// Authoritative reason this resolved target cannot currently be edited.
    pub fn edit_blocker(&self) -> Option<&AudioAutomationEditBlocker> {
        self.edit_blocker.as_ref()
    }

    /// Whether a mutation may enter an author transaction.
    pub fn is_editable(&self) -> bool {
        self.edit_blocker.is_none()
    }
}

/// Temporary author-state condition blocking a valid curve address.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioAutomationEditBlocker {
    /// A persistent Track lock protects a placement or Channel Strip.
    #[error("Audio automation cannot modify locked Track: {0}")]
    LockedTrack(TrackId),
    /// A shared Processing Scope is bound from a locked Track.
    #[error("Audio Processing Scope {scope_id} is bound from locked Track {track_id}")]
    LockedProcessingScopeBinding {
        /// Shared Scope targeted by the edit.
        scope_id: AudioProcessingScopeId,
        /// Locked Track that makes the shared definition read-only.
        track_id: TrackId,
    },
}

/// Fail-closed stable-address or parameter-contract failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioAutomationAddressError {
    /// Track is absent from the Sequence audio Timeline.
    #[error("Audio automation references an unknown audio Track: {0}")]
    UnknownTrack(TrackId),
    /// Clip is absent from the addressed Track.
    #[error("Audio automation references unknown Clip {clip_id} on Track {track_id}")]
    UnknownClip { track_id: TrackId, clip_id: ClipId },
    /// Component Edit is absent from the addressed Clip.
    #[error("Audio automation references unknown Component Edit {0}")]
    UnknownComponentEdit(AudioComponentEditId),
    /// Processing Scope is absent from the Sequence Audio Program.
    #[error("Audio automation references an unknown Processing Scope: {0}")]
    UnknownProcessingScope(AudioProcessingScopeId),
    /// Mix Bus owner is absent from the Sequence Audio Program.
    #[error("Audio automation references an unknown Mix Bus: {0}")]
    UnknownBus(MixBusId),
    /// Program Output owner is absent from the Sequence Audio Program.
    #[error("Audio automation references an unknown Program Output: {0}")]
    UnknownProgramOutput(ProgramOutputId),
    /// Route is absent from the Sequence Audio Program.
    #[error("Audio automation references an unknown Route: {0}")]
    UnknownRoute(AudioRouteId),
    /// Processor is absent from the addressed Rack.
    #[error("Audio automation references an unknown Processor: {0}")]
    UnknownProcessor(AudioProcessorInstanceId),
    /// Parameter is absent from the Processor definition snapshot.
    #[error("Audio Processor {processor_id} has no parameter {parameter_id}")]
    UnknownParameter {
        /// Addressed Processor.
        processor_id: AudioProcessorInstanceId,
        /// Missing stable parameter identity.
        parameter_id: ParameterId,
    },
    /// Parameter is not a numeric animatable control.
    #[error("Audio parameter {0} does not admit numeric automation")]
    NotAnimatable(ParameterId),
}

/// Fail-closed address, admission, or curve-mutation error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioAutomationEditError {
    /// Stable target could not be resolved.
    #[error(transparent)]
    Address(#[from] AudioAutomationAddressError),
    /// Target is protected by current authoring policy.
    #[error(transparent)]
    Blocked(#[from] AudioAutomationEditBlocker),
    /// Keyframe identity is absent from the addressed curve.
    #[error("Audio automation has no keyframe {0}")]
    UnknownKeyframe(KeyframeId),
    /// A distinct keyframe already owns the requested exact time.
    #[error("Audio automation already has another keyframe at the requested owner time")]
    KeyframeTimeCollision,
    /// Curve construction or exact interpolation validation failed.
    #[error("Audio automation is invalid: {reason}")]
    InvalidAutomation {
        /// Stable diagnostic detail from the exact curve contract.
        reason: String,
    },
    /// Complete resulting Audio Program is invalid.
    #[error("Audio automation edit produced invalid author state: {0}")]
    AuthorState(AudioAuthoringError),
}

/// Receipt from one successful audio-automation edit attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioAutomationEditOutcome {
    /// Whether canonical author state changed.
    pub changed: bool,
}

/// Resolve one curve, its exact owner-time domain, schema, and lock admission.
pub fn inspect_audio_automation<'a>(
    sequence: &'a Sequence,
    target: &AudioAutomationTarget,
) -> Result<AudioAutomationInspection<'a>, AudioAutomationAddressError> {
    match target {
        AudioAutomationTarget::ComponentVolume { track_id, clip_id, edit_id } => {
            let (edit, blocker) = component_edit(sequence, *track_id, *clip_id, *edit_id)?;
            Ok(inspection(
                target,
                AuthoringTimeDomain::AudioComponentEdit(*edit_id),
                CLIP_VOLUME_DB_PARAMETER_ID,
                edit.volume_db,
                edit.volume_automation.as_ref(),
                gain_contract(),
                all_interpolations(),
                blocker,
            ))
        }
        AudioAutomationTarget::ComponentPan { track_id, clip_id, edit_id } => {
            let (edit, blocker) = component_edit(sequence, *track_id, *clip_id, *edit_id)?;
            Ok(inspection(
                target,
                AuthoringTimeDomain::AudioComponentEdit(*edit_id),
                CLIP_PAN_PARAMETER_ID,
                edit.pan,
                edit.pan_automation.as_ref(),
                AudioAutomationValueContract {
                    value_type: PropertyValueType::Double,
                    hard_range: ParameterNumericRange { min: -1.0, max: 1.0 },
                    soft_range: ParameterNumericRange { min: -1.0, max: 1.0 },
                    step: Some(0.01),
                },
                all_interpolations(),
                blocker,
            ))
        }
        AudioAutomationTarget::ProcessingScopeInputGain { scope_id } => {
            let scope = sequence
                .audio_program
                .processing_scopes
                .iter()
                .find(|scope| scope.id == *scope_id)
                .ok_or(AudioAutomationAddressError::UnknownProcessingScope(
                    *scope_id,
                ))?;
            let blocker = processing_scope_blocker(sequence, *scope_id)?;
            Ok(inspection(
                target,
                AuthoringTimeDomain::AudioProcessingScope(*scope_id),
                INPUT_GAIN_DB_PARAMETER_ID,
                scope.input_gain_db,
                scope.input_gain_automation.as_ref(),
                gain_contract(),
                all_interpolations(),
                blocker,
            ))
        }
        AudioAutomationTarget::ChannelFader { owner } => {
            let (strip, blocker) = channel_strip(sequence, *owner)?;
            Ok(inspection(
                target,
                AuthoringTimeDomain::Sequence(sequence.id),
                FADER_DB_PARAMETER_ID,
                strip.fader_db,
                strip.fader_automation.as_ref(),
                gain_contract(),
                all_interpolations(),
                blocker,
            ))
        }
        AudioAutomationTarget::RouteGain { route_id } => {
            let route = sequence
                .audio_program
                .routes
                .iter()
                .find(|route| route.id == *route_id)
                .ok_or(AudioAutomationAddressError::UnknownRoute(*route_id))?;
            let blocker = match route.source {
                crate::audio::AudioRouteSource::Track { track_id, .. } => sequence
                    .audio_tracks
                    .iter()
                    .find(|track| track.id == track_id)
                    .ok_or(AudioAutomationAddressError::UnknownTrack(track_id))?
                    .is_locked
                    .then_some(AudioAutomationEditBlocker::LockedTrack(track_id)),
                crate::audio::AudioRouteSource::Bus { .. } => None,
            };
            Ok(inspection(
                target,
                AuthoringTimeDomain::Sequence(sequence.id),
                ROUTE_GAIN_DB_PARAMETER_ID,
                route.gain_db,
                route.gain_automation.as_ref(),
                gain_contract(),
                all_interpolations(),
                blocker,
            ))
        }
        AudioAutomationTarget::ProcessorParameter { rack, processor_id, parameter_id } => {
            let rack_inspection = inspect_audio_processor_rack(sequence, rack)
                .map_err(|_| rack_address_error(rack))?;
            let processor = rack_inspection
                .rack()
                .processors
                .iter()
                .find(|processor| processor.id == *processor_id)
                .ok_or(AudioAutomationAddressError::UnknownProcessor(*processor_id))?;
            let parameter = processor.parameters.get(parameter_id).ok_or_else(|| {
                AudioAutomationAddressError::UnknownParameter {
                    processor_id: *processor_id,
                    parameter_id: parameter_id.clone(),
                }
            })?;
            let Some(numeric) = parameter.schema.numeric else {
                return Err(AudioAutomationAddressError::NotAnimatable(
                    parameter_id.clone(),
                ));
            };
            if !parameter.schema.is_animatable {
                return Err(AudioAutomationAddressError::NotAnimatable(
                    parameter_id.clone(),
                ));
            }
            let blocker = rack_inspection.edit_blocker().and_then(|error| match error {
                crate::AudioProcessorRackEditError::LockedTrack(track_id) => {
                    Some(AudioAutomationEditBlocker::LockedTrack(*track_id))
                }
                crate::AudioProcessorRackEditError::LockedProcessingScopeBinding {
                    scope_id,
                    track_id,
                } => Some(AudioAutomationEditBlocker::LockedProcessingScopeBinding {
                    scope_id: *scope_id,
                    track_id: *track_id,
                }),
                _ => None,
            });
            Ok(AudioAutomationInspection {
                target: target.clone(),
                time_domain: rack_time_domain(sequence, rack),
                parameter_id: parameter_id.clone(),
                default_value: parameter.automation.default_value,
                curve: Some(&parameter.automation),
                value_contract: AudioAutomationValueContract {
                    value_type: parameter.schema.value_type,
                    hard_range: numeric.hard_range,
                    soft_range: numeric.soft_range,
                    step: numeric.step,
                },
                allowed_interpolations: parameter.schema.allowed_interpolations.clone(),
                edit_blocker: blocker,
            })
        }
    }
}

/// Apply one stable-address curve edit atomically to a Sequence candidate.
pub fn apply_audio_automation_edit(
    sequence: &mut Sequence,
    request: &AudioAutomationEditRequest,
) -> Result<AudioAutomationEditOutcome, AudioAutomationEditError> {
    let inspection = inspect_audio_automation(sequence, &request.target)?;
    if let Some(blocker) = inspection.edit_blocker().cloned() {
        return Err(blocker.into());
    }
    let mut candidate = sequence.clone();
    let changed = apply_to_candidate(&mut candidate, &request.target, &request.edit)?;
    if !changed {
        return Ok(AudioAutomationEditOutcome { changed: false });
    }
    candidate
        .audio_program
        .validate(
            &candidate.audio_tracks,
            &candidate.audio_roles,
            candidate.settings.audio_channel_layout,
        )
        .map_err(AudioAutomationEditError::AuthorState)?;
    *sequence = candidate;
    Ok(AudioAutomationEditOutcome { changed: true })
}

fn inspection<'a>(
    target: &AudioAutomationTarget,
    time_domain: AuthoringTimeDomain,
    parameter_id: &'static str,
    default_value: f64,
    curve: Option<&'a ExactAutomationCurve>,
    value_contract: AudioAutomationValueContract,
    allowed_interpolations: Vec<ParameterInterpolation>,
    edit_blocker: Option<AudioAutomationEditBlocker>,
) -> AudioAutomationInspection<'a> {
    AudioAutomationInspection {
        target: target.clone(),
        time_domain,
        parameter_id: ParameterId::new_static(parameter_id),
        default_value,
        curve,
        value_contract,
        allowed_interpolations,
        edit_blocker,
    }
}

fn gain_contract() -> AudioAutomationValueContract {
    AudioAutomationValueContract {
        value_type: PropertyValueType::Double,
        hard_range: ParameterNumericRange { min: AUDIO_GAIN_DB_MIN, max: AUDIO_GAIN_DB_MAX },
        soft_range: ParameterNumericRange { min: -60.0, max: 12.0 },
        step: Some(0.1),
    }
}

fn all_interpolations() -> Vec<ParameterInterpolation> {
    vec![
        ParameterInterpolation::Hold,
        ParameterInterpolation::Linear,
        ParameterInterpolation::Bezier,
    ]
}

fn component_edit(
    sequence: &Sequence,
    track_id: TrackId,
    clip_id: ClipId,
    edit_id: AudioComponentEditId,
) -> Result<
    (
        &crate::audio::AudioComponentEdit,
        Option<AudioAutomationEditBlocker>,
    ),
    AudioAutomationAddressError,
> {
    let track = sequence
        .audio_tracks
        .iter()
        .find(|track| track.id == track_id)
        .ok_or(AudioAutomationAddressError::UnknownTrack(track_id))?;
    let clip = track
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .ok_or(AudioAutomationAddressError::UnknownClip { track_id, clip_id })?;
    let edit = clip
        .audio_components
        .iter()
        .find(|edit| edit.id == edit_id)
        .ok_or(AudioAutomationAddressError::UnknownComponentEdit(edit_id))?;
    Ok((
        edit,
        track.is_locked.then_some(AudioAutomationEditBlocker::LockedTrack(track_id)),
    ))
}

fn processing_scope_blocker(
    sequence: &Sequence,
    scope_id: AudioProcessingScopeId,
) -> Result<Option<AudioAutomationEditBlocker>, AudioAutomationAddressError> {
    for track in &sequence.audio_tracks {
        if track.is_locked
            && track
                .clips
                .iter()
                .flat_map(|clip| &clip.audio_components)
                .any(|edit| edit.processing.scope_id == scope_id)
        {
            return Ok(Some(
                AudioAutomationEditBlocker::LockedProcessingScopeBinding {
                    scope_id,
                    track_id: track.id,
                },
            ));
        }
    }
    Ok(None)
}

fn channel_strip(
    sequence: &Sequence,
    owner: AudioChannelStripOwner,
) -> Result<
    (
        &crate::audio::AudioChannelStrip,
        Option<AudioAutomationEditBlocker>,
    ),
    AudioAutomationAddressError,
> {
    match owner {
        AudioChannelStripOwner::Track { track_id } => {
            let track = sequence
                .audio_tracks
                .iter()
                .find(|track| track.id == track_id)
                .ok_or(AudioAutomationAddressError::UnknownTrack(track_id))?;
            let strip = sequence
                .audio_program
                .track_channels
                .get(&track_id)
                .map(|channel| &channel.strip)
                .ok_or(AudioAutomationAddressError::UnknownTrack(track_id))?;
            Ok((
                strip,
                track.is_locked.then_some(AudioAutomationEditBlocker::LockedTrack(track_id)),
            ))
        }
        AudioChannelStripOwner::Bus { bus_id } => sequence
            .audio_program
            .buses
            .iter()
            .find(|bus| bus.id == bus_id)
            .map(|bus| (&bus.strip, None))
            .ok_or(AudioAutomationAddressError::UnknownBus(bus_id)),
        AudioChannelStripOwner::ProgramOutput { output_id } => sequence
            .audio_program
            .outputs
            .iter()
            .find(|output| output.id == output_id)
            .map(|output| (&output.strip, None))
            .ok_or(AudioAutomationAddressError::UnknownProgramOutput(output_id)),
    }
}

fn rack_address_error(address: &AudioProcessorRackAddress) -> AudioAutomationAddressError {
    match address {
        AudioProcessorRackAddress::ProcessingScope { scope_id } => {
            AudioAutomationAddressError::UnknownProcessingScope(*scope_id)
        }
        AudioProcessorRackAddress::ChannelStrip {
            owner: AudioChannelStripOwner::Track { track_id },
            ..
        } => AudioAutomationAddressError::UnknownTrack(*track_id),
        AudioProcessorRackAddress::ChannelStrip {
            owner: AudioChannelStripOwner::Bus { bus_id },
            ..
        } => AudioAutomationAddressError::UnknownBus(*bus_id),
        AudioProcessorRackAddress::ChannelStrip {
            owner: AudioChannelStripOwner::ProgramOutput { output_id },
            ..
        } => AudioAutomationAddressError::UnknownProgramOutput(*output_id),
    }
}

fn rack_time_domain(
    sequence: &Sequence,
    address: &AudioProcessorRackAddress,
) -> AuthoringTimeDomain {
    match address {
        AudioProcessorRackAddress::ProcessingScope { scope_id } => {
            AuthoringTimeDomain::AudioProcessingScope(*scope_id)
        }
        AudioProcessorRackAddress::ChannelStrip { .. } => {
            AuthoringTimeDomain::Sequence(sequence.id)
        }
    }
}

fn apply_to_candidate(
    sequence: &mut Sequence,
    target: &AudioAutomationTarget,
    edit: &AudioAutomationEdit,
) -> Result<bool, AudioAutomationEditError> {
    match target {
        AudioAutomationTarget::ComponentVolume { track_id, clip_id, edit_id } => {
            let component = component_edit_mut(sequence, *track_id, *clip_id, *edit_id)?;
            edit_optional_curve(
                &mut component.volume_automation,
                &mut component.volume_db,
                CLIP_VOLUME_DB_PARAMETER_ID,
                edit,
            )
        }
        AudioAutomationTarget::ComponentPan { track_id, clip_id, edit_id } => {
            let component = component_edit_mut(sequence, *track_id, *clip_id, *edit_id)?;
            edit_optional_curve(
                &mut component.pan_automation,
                &mut component.pan,
                CLIP_PAN_PARAMETER_ID,
                edit,
            )
        }
        AudioAutomationTarget::ProcessingScopeInputGain { scope_id } => {
            let scope = sequence
                .audio_program
                .processing_scopes
                .iter_mut()
                .find(|scope| scope.id == *scope_id)
                .ok_or(AudioAutomationAddressError::UnknownProcessingScope(
                    *scope_id,
                ))?;
            edit_optional_curve(
                &mut scope.input_gain_automation,
                &mut scope.input_gain_db,
                INPUT_GAIN_DB_PARAMETER_ID,
                edit,
            )
        }
        AudioAutomationTarget::ChannelFader { owner } => {
            let strip = channel_strip_mut(sequence, *owner)?;
            edit_optional_curve(
                &mut strip.fader_automation,
                &mut strip.fader_db,
                FADER_DB_PARAMETER_ID,
                edit,
            )
        }
        AudioAutomationTarget::RouteGain { route_id } => {
            let route = sequence
                .audio_program
                .routes
                .iter_mut()
                .find(|route| route.id == *route_id)
                .ok_or(AudioAutomationAddressError::UnknownRoute(*route_id))?;
            edit_optional_curve(
                &mut route.gain_automation,
                &mut route.gain_db,
                ROUTE_GAIN_DB_PARAMETER_ID,
                edit,
            )
        }
        AudioAutomationTarget::ProcessorParameter { rack, processor_id, parameter_id } => {
            let processor = processor_mut(sequence, rack, *processor_id)?;
            let parameter = processor.parameters.get_mut(parameter_id).ok_or_else(|| {
                AudioAutomationAddressError::UnknownParameter {
                    processor_id: *processor_id,
                    parameter_id: parameter_id.clone(),
                }
            })?;
            edit_owned_curve(&mut parameter.automation, edit)
        }
    }
}

fn edit_optional_curve(
    curve: &mut Option<ExactAutomationCurve>,
    static_value: &mut f64,
    parameter_id: &'static str,
    edit: &AudioAutomationEdit,
) -> Result<bool, AudioAutomationEditError> {
    let mut candidate = match curve.as_ref() {
        Some(curve) => curve.clone(),
        None => ExactAutomationCurve::new(ParameterId::new_static(parameter_id), *static_value)
            .map_err(invalid_automation)?,
    };
    let changed = edit_curve(&mut candidate, edit)?;
    if !changed {
        return Ok(false);
    }
    if candidate.keyframes.is_empty() {
        *static_value = candidate.default_value;
        *curve = None;
    } else {
        *curve = Some(candidate);
    }
    Ok(true)
}

fn edit_owned_curve(
    curve: &mut ExactAutomationCurve,
    edit: &AudioAutomationEdit,
) -> Result<bool, AudioAutomationEditError> {
    let mut candidate = curve.clone();
    let changed = edit_curve(&mut candidate, edit)?;
    if changed {
        *curve = candidate;
    }
    Ok(changed)
}

fn edit_curve(
    curve: &mut ExactAutomationCurve,
    edit: &AudioAutomationEdit,
) -> Result<bool, AudioAutomationEditError> {
    let before = curve.clone();
    match edit {
        AudioAutomationEdit::UpsertKeyframe { keyframe } => {
            if curve
                .keyframes
                .iter()
                .any(|candidate| candidate.id != keyframe.id && candidate.time == keyframe.time)
            {
                return Err(AudioAutomationEditError::KeyframeTimeCollision);
            }
            curve.keyframes.retain(|candidate| candidate.id != keyframe.id);
            curve.set_keyframe(keyframe.clone()).map_err(invalid_automation)?;
        }
        AudioAutomationEdit::RemoveKeyframe { keyframe_id } => {
            let count = curve.keyframes.len();
            curve.keyframes.retain(|candidate| candidate.id != *keyframe_id);
            if curve.keyframes.len() == count {
                return Err(AudioAutomationEditError::UnknownKeyframe(*keyframe_id));
            }
        }
        AudioAutomationEdit::Clear { default_value } => {
            curve.default_value = *default_value;
            curve.keyframes.clear();
            curve.validate().map_err(invalid_automation)?;
        }
    }
    Ok(*curve != before)
}

fn invalid_automation(error: impl std::fmt::Display) -> AudioAutomationEditError {
    AudioAutomationEditError::InvalidAutomation { reason: error.to_string() }
}

fn component_edit_mut(
    sequence: &mut Sequence,
    track_id: TrackId,
    clip_id: ClipId,
    edit_id: AudioComponentEditId,
) -> Result<&mut crate::audio::AudioComponentEdit, AudioAutomationAddressError> {
    let track = sequence
        .audio_tracks
        .iter_mut()
        .find(|track| track.id == track_id)
        .ok_or(AudioAutomationAddressError::UnknownTrack(track_id))?;
    let clip = track
        .clips
        .iter_mut()
        .find(|clip| clip.id == clip_id)
        .ok_or(AudioAutomationAddressError::UnknownClip { track_id, clip_id })?;
    clip.audio_components
        .iter_mut()
        .find(|edit| edit.id == edit_id)
        .ok_or(AudioAutomationAddressError::UnknownComponentEdit(edit_id))
}

fn channel_strip_mut(
    sequence: &mut Sequence,
    owner: AudioChannelStripOwner,
) -> Result<&mut crate::audio::AudioChannelStrip, AudioAutomationAddressError> {
    match owner {
        AudioChannelStripOwner::Track { track_id } => sequence
            .audio_program
            .track_channels
            .get_mut(&track_id)
            .map(|channel| &mut channel.strip)
            .ok_or(AudioAutomationAddressError::UnknownTrack(track_id)),
        AudioChannelStripOwner::Bus { bus_id } => sequence
            .audio_program
            .buses
            .iter_mut()
            .find(|bus| bus.id == bus_id)
            .map(|bus| &mut bus.strip)
            .ok_or(AudioAutomationAddressError::UnknownBus(bus_id)),
        AudioChannelStripOwner::ProgramOutput { output_id } => sequence
            .audio_program
            .outputs
            .iter_mut()
            .find(|output| output.id == output_id)
            .map(|output| &mut output.strip)
            .ok_or(AudioAutomationAddressError::UnknownProgramOutput(output_id)),
    }
}

fn processor_mut<'a>(
    sequence: &'a mut Sequence,
    address: &AudioProcessorRackAddress,
    processor_id: AudioProcessorInstanceId,
) -> Result<&'a mut crate::audio::AudioProcessorInstance, AudioAutomationAddressError> {
    let rack = match *address {
        AudioProcessorRackAddress::ProcessingScope { scope_id } => {
            &mut sequence
                .audio_program
                .processing_scopes
                .iter_mut()
                .find(|scope| scope.id == scope_id)
                .ok_or(AudioAutomationAddressError::UnknownProcessingScope(
                    scope_id,
                ))?
                .processors
        }
        AudioProcessorRackAddress::ChannelStrip { owner, rack } => {
            let strip = channel_strip_mut(sequence, owner)?;
            match rack {
                AudioChannelStripRack::PreFader => &mut strip.pre_fader,
                AudioChannelStripRack::PostFader => &mut strip.post_fader,
            }
        }
    };
    rack.processors
        .iter_mut()
        .find(|processor| processor.id == processor_id)
        .ok_or(AudioAutomationAddressError::UnknownProcessor(processor_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{
        AudioProcessorInstance, AudioRoute, AudioRouteDestination, AudioRouteSource,
        BUILTIN_GAIN_DEFINITION_ID, GAIN_DB_PARAMETER_ID,
    };
    use mondrian_core::TimelineTime;

    #[test]
    fn one_interface_resolves_all_owner_time_domains() {
        let mut sequence = Sequence::new("Automation domains");
        let track_id = sequence.audio_tracks[0].id;
        let clip = crate::Clip::new(
            mondrian_core::AssetId::new(),
            TimelineTime::ZERO,
            TimelineTime::ONE,
        )
        .expect("Clip");
        let clip_id = clip.id;
        sequence
            .add_media_audio_clip(
                track_id,
                clip,
                mondrian_core::AudioSourceComponentId::primary(),
            )
            .expect("audio Clip");
        let component = &sequence.audio_tracks[0].clips[0].audio_components[0];
        let edit_id = component.id;
        let scope_id = component.processing.scope_id;
        let output_id = sequence.audio_program.outputs[0].id;
        let route = AudioRoute::new(
            AudioRouteSource::Track {
                track_id,
                port: crate::audio::AudioChannelStripOutputPort::PostMute,
            },
            AudioRouteDestination::Output(output_id),
        );
        let route_id = route.id;
        sequence.audio_program.routes.push(route);
        let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        let processor_id = processor.id;
        sequence
            .audio_program
            .processing_scopes
            .iter_mut()
            .find(|scope| scope.id == scope_id)
            .expect("Scope")
            .processors
            .processors
            .push(processor);

        let cases = [
            (
                AudioAutomationTarget::ComponentVolume { track_id, clip_id, edit_id },
                AuthoringTimeDomain::AudioComponentEdit(edit_id),
            ),
            (
                AudioAutomationTarget::ProcessingScopeInputGain { scope_id },
                AuthoringTimeDomain::AudioProcessingScope(scope_id),
            ),
            (
                AudioAutomationTarget::ChannelFader {
                    owner: AudioChannelStripOwner::Track { track_id },
                },
                AuthoringTimeDomain::Sequence(sequence.id),
            ),
            (
                AudioAutomationTarget::RouteGain { route_id },
                AuthoringTimeDomain::Sequence(sequence.id),
            ),
            (
                AudioAutomationTarget::ProcessorParameter {
                    rack: AudioProcessorRackAddress::ProcessingScope { scope_id },
                    processor_id,
                    parameter_id: ParameterId::new_static(GAIN_DB_PARAMETER_ID),
                },
                AuthoringTimeDomain::AudioProcessingScope(scope_id),
            ),
        ];
        for (target, domain) in cases {
            assert_eq!(
                inspect_audio_automation(&sequence, &target).expect("inspection").time_domain(),
                domain
            );
        }
    }

    #[test]
    fn stable_key_moves_and_last_key_canonicalizes_to_static() {
        let mut sequence = Sequence::new("Automation mutation");
        let track_id = sequence.audio_tracks[0].id;
        let target = AudioAutomationTarget::ChannelFader {
            owner: AudioChannelStripOwner::Track { track_id },
        };
        let mut key = ExactAutomationKeyframe::linear(TimelineTime::ZERO, -6.0);
        let keyframe_id = key.id;
        apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest {
                target: target.clone(),
                edit: AudioAutomationEdit::UpsertKeyframe { keyframe: key.clone() },
            },
        )
        .expect("insert");
        key.time = TimelineTime::ONE;
        key.value = -3.0;
        apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest {
                target: target.clone(),
                edit: AudioAutomationEdit::UpsertKeyframe { keyframe: key },
            },
        )
        .expect("move");
        let curve = inspect_audio_automation(&sequence, &target)
            .expect("inspection")
            .curve()
            .expect("active curve");
        assert_eq!(curve.keyframes.len(), 1);
        assert_eq!(curve.keyframes[0].id, keyframe_id);
        assert_eq!(curve.keyframes[0].time, TimelineTime::ONE);

        apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest {
                target: target.clone(),
                edit: AudioAutomationEdit::RemoveKeyframe { keyframe_id },
            },
        )
        .expect("remove");
        let inspection = inspect_audio_automation(&sequence, &target).expect("inspection");
        assert!(inspection.curve().is_none());
        assert_eq!(inspection.default_value(), 0.0);
    }

    #[test]
    fn shared_scope_and_component_locks_fail_before_mutation() {
        let mut sequence = Sequence::new("Automation locks");
        let track_id = sequence.audio_tracks[0].id;
        let clip = crate::Clip::new(
            mondrian_core::AssetId::new(),
            TimelineTime::ZERO,
            TimelineTime::ONE,
        )
        .expect("Clip");
        let clip_id = clip.id;
        sequence
            .add_media_audio_clip(
                track_id,
                clip,
                mondrian_core::AudioSourceComponentId::primary(),
            )
            .expect("audio Clip");
        let edit = &sequence.audio_tracks[0].clips[0].audio_components[0];
        let edit_id = edit.id;
        let scope_id = edit.processing.scope_id;
        sequence.audio_tracks[0].is_locked = true;
        for target in [
            AudioAutomationTarget::ComponentPan { track_id, clip_id, edit_id },
            AudioAutomationTarget::ProcessingScopeInputGain { scope_id },
        ] {
            let error = apply_audio_automation_edit(
                &mut sequence,
                &AudioAutomationEditRequest {
                    target,
                    edit: AudioAutomationEdit::UpsertKeyframe {
                        keyframe: ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0),
                    },
                },
            )
            .expect_err("locked target");
            assert!(matches!(error, AudioAutomationEditError::Blocked(_)));
        }
    }
}
