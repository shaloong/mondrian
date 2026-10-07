//! Unified stable-address authoring for Sequence audio automation.
//!
//! Every audio control that owns an exact numeric curve is addressed through
//! this Module. Mixer, Inspector, plugin, and scripting Adapters therefore do
//! not need to know which persistent struct stores a curve, which owner-time
//! domain gives its keyframe coordinates meaning, or how Track locks propagate
//! through shared Processing Scopes.

use crate::audio::{
    AudioAuthoringError, AudioChannelStripOwner, AUDIO_GAIN_DB_MAX, AUDIO_GAIN_DB_MIN,
    CLIP_VOLUME_DB_PARAMETER_ID, FADER_DB_PARAMETER_ID, INPUT_GAIN_DB_PARAMETER_ID,
    ROUTE_GAIN_DB_PARAMETER_ID,
};
use crate::audio_processor_edit::{
    inspect_audio_processor_rack, AudioChannelStripRack, AudioProcessorRackAddress,
};
use crate::sequence::Sequence;
use mondrian_core::{
    AudioComponentEditId, AudioProcessingScopeId, AudioProcessorInstanceId, AudioRouteId,
    AuthoringTimeDomain, BezierHandleSide, ClipId, ExactAutomationCurve, ExactAutomationKeyframe,
    ExactBezierHandleEdit, InterpolationType, KeyframeId, MixBusId, ParameterId,
    ParameterInterpolation, ParameterNumericRange, ParameterSchema, ProgramOutputId,
    PropertyValueType, TimelineTime, TrackId,
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
    /// Transform a complete stable group with one final definition admission.
    TransformKeyframes {
        /// Nonempty unique complete key identities.
        keyframe_ids: Vec<KeyframeId>,
        /// Signed exact owner-time translation.
        time_delta: TimelineTime,
        /// Finite common displacement in author value units.
        value_delta: f64,
        /// Signed value increment count.
        steps: i8,
    },
    /// Paste one complete exact group with a single final constraint normalization.
    PasteKeyframes {
        /// Complete fresh-identity keys in the addressed owner's time domain.
        keyframes: Vec<ExactAutomationKeyframe>,
    },
    /// Remove a unique nonempty key set with one constraint normalization.
    RemoveKeyframes {
        /// Stable keys in the addressed curve.
        keyframe_ids: Vec<KeyframeId>,
        /// Original evaluation retained when deleting every key.
        retain_time: TimelineTime,
    },
    /// Translate a complete unique key set by one exact owner-time delta.
    ShiftKeyframes {
        /// Stable keys in the addressed curve.
        keyframe_ids: Vec<KeyframeId>,
        /// Signed exact owner-time translation.
        delta: TimelineTime,
    },
    /// Offset selected values with one common definition-bounded displacement.
    OffsetKeyframeValues {
        /// Unique nonempty stable key identities.
        keyframe_ids: Vec<KeyframeId>,
        /// Finite author-unit increment.
        delta: f64,
        /// Signed increment count.
        steps: i8,
    },
    /// Edit one Bezier handle using canonical constrained geometry.
    EditHandle {
        /// Stable keyframe to edit.
        keyframe_id: KeyframeId,
        /// Temporal side controlling the addressed segment.
        side: BezierHandleSide,
        /// Changed exact coordinates; omitted axes retain author values.
        edit: ExactBezierHandleEdit,
    },
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
    /// Set one interpolation preset on a complete stable selection atomically.
    SetInterpolations {
        /// Unique nonempty stable key identities.
        keyframe_ids: Vec<KeyframeId>,
        /// Definition-admitted hold, linear or Bezier mode.
        interpolation: InterpolationType,
    },
    /// Change the outgoing segment and the selected key's tangent constraint.
    SetInterpolation {
        /// Stable keyframe to edit.
        keyframe_id: KeyframeId,
        /// One of the supported hold, linear, or Bezier modes.
        interpolation: InterpolationType,
    },
    /// Restore the current definition default and remove every authored key.
    ResetToDefault,
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

/// Immutable definition admission shared by audio curve previews and author transactions.
/// Produced from a resolved target; it contains no mutable Project or execution state.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioAutomationCurveContract {
    schema: ParameterSchema,
    diagnostic: AudioCurveDiagnostic,
}

#[derive(Debug, Clone, PartialEq)]
enum AudioCurveDiagnostic {
    Gain,
    Pan(AudioComponentEditId),
    Processor,
}

impl AudioAutomationCurveContract {
    fn gain(parameter_id: &'static str) -> Self {
        Self {
            schema: crate::audio::gain_curve_schema(parameter_id),
            diagnostic: AudioCurveDiagnostic::Gain,
        }
    }
    fn pan(edit_id: AudioComponentEditId) -> Self {
        Self {
            schema: crate::audio::pan_parameter_schema(),
            diagnostic: AudioCurveDiagnostic::Pan(edit_id),
        }
    }
    fn processor(schema: ParameterSchema) -> Self {
        Self {
            schema,
            diagnostic: AudioCurveDiagnostic::Processor,
        }
    }
    fn validate(&self, curve: &ExactAutomationCurve) -> Result<(), AudioAutomationEditError> {
        crate::audio::validate_numeric_automation(&self.schema, curve)
            .map_err(|error| match self.diagnostic {
                AudioCurveDiagnostic::Gain => crate::audio::gain_curve_error(error),
                AudioCurveDiagnostic::Pan(edit_id) => crate::audio::pan_curve_error(error, edit_id),
                AudioCurveDiagnostic::Processor => error,
            })
            .map_err(AudioAutomationEditError::AuthorState)
    }
    /// Definition schema used to admit complete canonical clipboard values.
    pub fn schema(&self) -> &ParameterSchema {
        &self.schema
    }

    /// Prepare one exact curve without publishing state, generation or author history.
    /// Identity, scalar and handle values, animation and interpolation are definition-admitted.
    pub fn preview_edit(
        &self,
        curve: &ExactAutomationCurve,
        edit: &AudioAutomationEdit,
    ) -> Result<ExactAutomationCurve, AudioAutomationEditError> {
        self.validate(curve)?;
        let family = match edit {
            AudioAutomationEdit::EditHandle { .. } => Some(ParameterInterpolation::Bezier),
            AudioAutomationEdit::SetInterpolation { interpolation, .. }
            | AudioAutomationEdit::SetInterpolations { interpolation, .. } => {
                Some(match interpolation {
                    InterpolationType::Hold => ParameterInterpolation::Hold,
                    InterpolationType::Linear => ParameterInterpolation::Linear,
                    _ => ParameterInterpolation::Bezier,
                })
            }
            _ => None,
        };
        if family.is_some_and(|family| !self.schema.allowed_interpolations.contains(&family)) {
            return Err(AudioAutomationEditError::InterpolationNotAllowed);
        }
        let mut candidate = curve.clone();
        edit_curve(&mut candidate, edit, &self.schema)?;
        self.validate(&candidate)?;
        Ok(candidate)
    }
}

/// Read-only projection of one resolved audio automation target.
#[derive(Debug, Clone)]
pub struct AudioAutomationInspection<'a> {
    target: AudioAutomationTarget,
    time_domain: AuthoringTimeDomain,
    curve_contract: AudioAutomationCurveContract,
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
        &self.curve_contract.schema.parameter_id
    }

    /// Owned definition snapshot shared by preview and fresh author admission.
    pub fn curve_contract(&self) -> &AudioAutomationCurveContract {
        &self.curve_contract
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
    /// The addressed parameter schema disallows this interpolation family.
    #[error("Audio parameter does not allow the requested interpolation")]
    InterpolationNotAllowed,
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
                AudioAutomationCurveContract::gain(CLIP_VOLUME_DB_PARAMETER_ID),
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
                AudioAutomationCurveContract::pan(*edit_id),
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
                AudioAutomationCurveContract::gain(INPUT_GAIN_DB_PARAMETER_ID),
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
                AudioAutomationCurveContract::gain(FADER_DB_PARAMETER_ID),
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
                .map(|route| (route.source, route.gain_db, route.gain_automation.as_ref()))
                .or_else(|| {
                    sequence
                        .audio_program
                        .sidechain_routes
                        .iter()
                        .find(|route| route.id == *route_id)
                        .map(|route| (route.source, route.gain_db, route.gain_automation.as_ref()))
                })
                .ok_or(AudioAutomationAddressError::UnknownRoute(*route_id))?;
            let blocker = match route.0 {
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
                AudioAutomationCurveContract::gain(ROUTE_GAIN_DB_PARAMETER_ID),
                route.1,
                route.2,
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
            let Some(numeric) = parameter
                .schema
                .numeric
                .as_ref()
                .and_then(|contracts| contracts.first())
                .copied()
            else {
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
                curve_contract: AudioAutomationCurveContract::processor(parameter.schema.clone()),
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
    let source = match inspection.curve() {
        Some(curve) => curve.clone(),
        None => ExactAutomationCurve::new(
            inspection.parameter_id().clone(),
            inspection.default_value(),
        )
        .map_err(invalid_automation)?,
    };
    let prepared = inspection.curve_contract().preview_edit(&source, &request.edit)?;
    if source == prepared {
        return Ok(AudioAutomationEditOutcome { changed: false });
    }
    let mut candidate = sequence.clone();
    publish_curve(&mut candidate, &request.target, prepared)?;
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
    curve_contract: AudioAutomationCurveContract,
    default_value: f64,
    curve: Option<&'a ExactAutomationCurve>,
    value_contract: AudioAutomationValueContract,
    allowed_interpolations: Vec<ParameterInterpolation>,
    edit_blocker: Option<AudioAutomationEditBlocker>,
) -> AudioAutomationInspection<'a> {
    AudioAutomationInspection {
        target: target.clone(),
        time_domain,
        curve_contract,
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

fn publish_curve(
    sequence: &mut Sequence,
    target: &AudioAutomationTarget,
    curve: ExactAutomationCurve,
) -> Result<(), AudioAutomationEditError> {
    match target {
        AudioAutomationTarget::ComponentVolume { track_id, clip_id, edit_id } => {
            let component = component_edit_mut(sequence, *track_id, *clip_id, *edit_id)?;
            publish_optional_curve(
                &mut component.volume_automation,
                &mut component.volume_db,
                curve,
            );
        }
        AudioAutomationTarget::ComponentPan { track_id, clip_id, edit_id } => {
            let component = component_edit_mut(sequence, *track_id, *clip_id, *edit_id)?;
            publish_optional_curve(&mut component.pan_automation, &mut component.pan, curve);
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
            publish_optional_curve(
                &mut scope.input_gain_automation,
                &mut scope.input_gain_db,
                curve,
            );
        }
        AudioAutomationTarget::ChannelFader { owner } => {
            let strip = channel_strip_mut(sequence, *owner)?;
            publish_optional_curve(&mut strip.fader_automation, &mut strip.fader_db, curve);
        }
        AudioAutomationTarget::RouteGain { route_id } => {
            if let Some(route) =
                sequence.audio_program.routes.iter_mut().find(|route| route.id == *route_id)
            {
                publish_optional_curve(&mut route.gain_automation, &mut route.gain_db, curve);
            } else {
                let route = sequence
                    .audio_program
                    .sidechain_routes
                    .iter_mut()
                    .find(|route| route.id == *route_id)
                    .ok_or(AudioAutomationAddressError::UnknownRoute(*route_id))?;
                publish_optional_curve(&mut route.gain_automation, &mut route.gain_db, curve);
            }
        }
        AudioAutomationTarget::ProcessorParameter { rack, processor_id, parameter_id } => {
            let processor = processor_mut(sequence, rack, *processor_id)?;
            let parameter = processor.parameters.get_mut(parameter_id).ok_or_else(|| {
                AudioAutomationAddressError::UnknownParameter {
                    processor_id: *processor_id,
                    parameter_id: parameter_id.clone(),
                }
            })?;
            parameter.set_automation(curve).map_err(AudioAutomationEditError::AuthorState)?;
        }
    }
    Ok(())
}

fn publish_optional_curve(
    curve: &mut Option<ExactAutomationCurve>,
    static_value: &mut f64,
    prepared: ExactAutomationCurve,
) {
    if prepared.keyframes.is_empty() {
        *static_value = prepared.default_value;
        *curve = None;
    } else {
        *curve = Some(prepared);
    }
}

fn edit_curve(
    curve: &mut ExactAutomationCurve,
    edit: &AudioAutomationEdit,
    schema: &ParameterSchema,
) -> Result<(), AudioAutomationEditError> {
    match edit {
        AudioAutomationEdit::TransformKeyframes {
            keyframe_ids,
            time_delta,
            value_delta,
            steps,
        } => {
            let contract = schema
                .numeric
                .as_ref()
                .and_then(|contracts| contracts.first())
                .copied()
                .ok_or_else(|| invalid_automation("numeric channel contract is absent"))?;
            curve
                .transform_keyframes(keyframe_ids, *time_delta, contract, *value_delta, *steps)
                .map_err(invalid_automation)?;
        }
        AudioAutomationEdit::PasteKeyframes { keyframes } => {
            curve.paste_keyframes(keyframes).map_err(invalid_automation)?;
        }
        AudioAutomationEdit::RemoveKeyframes { keyframe_ids, retain_time } => {
            curve.remove_keyframes(keyframe_ids, *retain_time).map_err(invalid_automation)?;
        }
        AudioAutomationEdit::ShiftKeyframes { keyframe_ids, delta } => {
            curve.shift_keyframes(keyframe_ids, *delta).map_err(invalid_automation)?;
        }
        AudioAutomationEdit::OffsetKeyframeValues { keyframe_ids, delta, steps } => {
            let contract = schema
                .numeric
                .as_ref()
                .and_then(|contracts| contracts.first())
                .copied()
                .ok_or_else(|| invalid_automation("numeric channel contract is absent"))?;
            curve
                .offset_keyframe_values(keyframe_ids, contract, *delta, *steps)
                .map_err(invalid_automation)?;
        }
        AudioAutomationEdit::EditHandle { keyframe_id, side, edit } => {
            curve.edit_keyframe_handle(*keyframe_id, *side, *edit).map_err(
                |error| match error {
                    mondrian_core::AutomationError::UnknownKeyframe => {
                        AudioAutomationEditError::UnknownKeyframe(*keyframe_id)
                    }
                    other => invalid_automation(other),
                },
            )?;
        }
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
            curve.remove_keyframe(*keyframe_id).map_err(|error| match error {
                mondrian_core::AutomationError::UnknownKeyframe => {
                    AudioAutomationEditError::UnknownKeyframe(*keyframe_id)
                }
                other => invalid_automation(other),
            })?;
        }
        AudioAutomationEdit::SetInterpolations { keyframe_ids, interpolation } => {
            curve
                .set_keyframes_interpolation(keyframe_ids, *interpolation)
                .map_err(invalid_automation)?;
        }
        AudioAutomationEdit::SetInterpolation { keyframe_id, interpolation } => {
            curve
                .set_keyframe_interpolation(*keyframe_id, *interpolation)
                .map_err(|error| match error {
                    mondrian_core::AutomationError::UnknownKeyframe => {
                        AudioAutomationEditError::UnknownKeyframe(*keyframe_id)
                    }
                    other => invalid_automation(other),
                })?;
        }
        AudioAutomationEdit::ResetToDefault => {
            *curve = crate::audio::default_numeric_automation(schema)
                .map_err(AudioAutomationEditError::AuthorState)?;
        }
        AudioAutomationEdit::Clear { default_value } => {
            curve.default_value = *default_value;
            curve.keyframes.clear();
            curve.validate().map_err(invalid_automation)?;
        }
    }
    Ok(())
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
    #[test]
    fn clipboard_paste_group_shares_definition_preview_author_codec_and_locks() {
        let mut sequence = Sequence::new("Clipboard group");
        let track_id = sequence.audio_tracks[0].id;
        let target = AudioAutomationTarget::ChannelFader {
            owner: AudioChannelStripOwner::Track { track_id },
        };
        let keys = vec![
            ExactAutomationKeyframe::linear(TimelineTime::new(1, 3).expect("exact"), -24.125),
            ExactAutomationKeyframe::linear(TimelineTime::new(7, 11).expect("exact"), -12.2),
        ];
        let request = AudioAutomationEditRequest {
            target: target.clone(),
            edit: AudioAutomationEdit::PasteKeyframes { keyframes: keys.clone() },
        };
        let decoded: AudioAutomationEditRequest =
            serde_json::from_str(&serde_json::to_string(&request).expect("encode"))
                .expect("decode");
        assert_eq!(decoded, request);
        let inspection = inspect_audio_automation(&sequence, &target).expect("definition");
        let source = ExactAutomationCurve::new(
            inspection.parameter_id().clone(),
            inspection.default_value(),
        )
        .expect("empty source");
        let preview = inspection
            .curve_contract()
            .preview_edit(&source, &request.edit)
            .expect("whole group preview");
        assert!(apply_audio_automation_edit(&mut sequence, &request).expect("commit").changed);
        let authored = inspect_audio_automation(&sequence, &target)
            .expect("definition")
            .curve()
            .expect("curve")
            .clone();
        assert_eq!(authored, preview);
        assert_eq!(authored.keyframes[0], keys[0]);
        assert_eq!(authored.keyframes[1], keys[1]);
        let before = sequence.clone();
        assert!(!apply_audio_automation_edit(&mut sequence, &request).expect("noop").changed);
        assert_eq!(sequence, before);
        let mut invalid_keys = keys;
        invalid_keys[1].value = 1e200;
        let invalid = AudioAutomationEditRequest {
            target: target.clone(),
            edit: AudioAutomationEdit::PasteKeyframes { keyframes: invalid_keys },
        };
        assert!(inspect_audio_automation(&sequence, &target)
            .expect("definition")
            .curve_contract()
            .preview_edit(&authored, &invalid.edit)
            .is_err());
        assert!(apply_audio_automation_edit(&mut sequence, &invalid).is_err());
        assert_eq!(
            sequence, before,
            "definition refuses before publishing any key"
        );
        sequence.audio_tracks[0].is_locked = true;
        let locked = sequence.clone();
        assert!(matches!(
            apply_audio_automation_edit(&mut sequence, &request),
            Err(AudioAutomationEditError::Blocked(_))
        ));
        assert_eq!(sequence, locked);
    }

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
    fn single_handle_edit_obeys_constraints_noops_and_track_locks() {
        let mut sequence = Sequence::new("Handle authoring");
        let track_id = sequence.audio_tracks[0].id;
        let target = AudioAutomationTarget::ChannelFader {
            owner: AudioChannelStripOwner::Track { track_id },
        };
        let keys = [
            ExactAutomationKeyframe::linear(TimelineTime::ZERO, -12.0),
            ExactAutomationKeyframe::linear(TimelineTime::ONE, -6.0),
            ExactAutomationKeyframe::linear(TimelineTime::new(2, 1).expect("time"), 0.0),
        ];
        for keyframe in keys.iter().cloned() {
            apply_audio_automation_edit(
                &mut sequence,
                &AudioAutomationEditRequest {
                    target: target.clone(),
                    edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
                },
            )
            .expect("insert");
        }
        apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest {
                target: target.clone(),
                edit: AudioAutomationEdit::SetInterpolation {
                    keyframe_id: keys[1].id,
                    interpolation: InterpolationType::AutoBezier,
                },
            },
        )
        .expect("auto");
        let request = AudioAutomationEditRequest {
            target: target.clone(),
            edit: AudioAutomationEdit::EditHandle {
                keyframe_id: keys[1].id,
                side: BezierHandleSide::Outgoing,
                edit: ExactBezierHandleEdit { time_offset: None, value_offset: Some(1.5) },
            },
        };
        let restored = serde_json::from_str(&serde_json::to_string(&request).expect("encode"))
            .expect("decode");
        assert_eq!(request, restored);
        assert!(apply_audio_automation_edit(&mut sequence, &request).expect("handle").changed);
        let key = &inspect_audio_automation(&sequence, &target)
            .expect("inspection")
            .curve()
            .expect("curve")
            .keyframes[1];
        assert_eq!(
            key.tangent_mode,
            mondrian_core::ExactAutomationTangentMode::Continuous
        );
        assert_eq!(
            key.out_handle.expect("outgoing").time_offset,
            TimelineTime::new(1, 3).expect("exact unchanged axis")
        );
        assert_eq!(key.out_handle.expect("outgoing").value_offset, 1.5);
        assert_eq!(key.in_handle.expect("partner").value_offset, -1.5);
        let before = sequence.clone();
        assert!(!apply_audio_automation_edit(&mut sequence, &request).expect("noop").changed);
        assert_eq!(sequence, before);
        let invalid = AudioAutomationEditRequest {
            target,
            edit: AudioAutomationEdit::EditHandle {
                keyframe_id: keys[1].id,
                side: BezierHandleSide::Incoming,
                edit: ExactBezierHandleEdit {
                    time_offset: Some(TimelineTime::ONE),
                    value_offset: None,
                },
            },
        };
        assert!(matches!(
            apply_audio_automation_edit(&mut sequence, &invalid),
            Err(AudioAutomationEditError::InvalidAutomation { .. })
        ));
        assert_eq!(sequence, before);
        sequence.audio_tracks[0].is_locked = true;
        let locked = sequence.clone();
        assert!(matches!(
            apply_audio_automation_edit(&mut sequence, &request),
            Err(AudioAutomationEditError::Blocked(_))
        ));
        assert_eq!(sequence, locked);
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
    fn interpolation_edit_is_atomic_and_survives_neighbor_motion() {
        let mut sequence = Sequence::new("Audio tangent modes");
        let track_id = sequence.audio_tracks[0].id;
        let target = AudioAutomationTarget::ChannelFader {
            owner: AudioChannelStripOwner::Track { track_id },
        };
        let keys = [
            ExactAutomationKeyframe::linear(TimelineTime::ZERO, -12.0),
            ExactAutomationKeyframe::linear(TimelineTime::ONE, -6.0),
            ExactAutomationKeyframe::linear(TimelineTime::new(2, 1).expect("time"), 0.0),
        ];
        for keyframe in keys.iter().cloned() {
            apply_audio_automation_edit(
                &mut sequence,
                &AudioAutomationEditRequest {
                    target: target.clone(),
                    edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
                },
            )
            .expect("insert keyframe");
        }
        apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest {
                target: target.clone(),
                edit: AudioAutomationEdit::SetInterpolation {
                    keyframe_id: keys[1].id,
                    interpolation: InterpolationType::AutoBezier,
                },
            },
        )
        .expect("set auto");
        let mut moved = keys[2].clone();
        moved.value = -4.0;
        apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest {
                target: target.clone(),
                edit: AudioAutomationEdit::UpsertKeyframe { keyframe: moved },
            },
        )
        .expect("move neighbor");
        let curve = inspect_audio_automation(&sequence, &target)
            .expect("inspection")
            .curve()
            .expect("curve");
        assert_eq!(
            curve.keyframes[1].tangent_mode,
            mondrian_core::ExactAutomationTangentMode::Auto
        );
        assert!(curve.keyframes[1].out_handle.expect("out tangent").value_offset > 0.0);
        let before = sequence.clone();
        let unknown_id = KeyframeId::new();
        assert_eq!(
            apply_audio_automation_edit(
                &mut sequence,
                &AudioAutomationEditRequest {
                    target,
                    edit: AudioAutomationEdit::SetInterpolation {
                        keyframe_id: unknown_id,
                        interpolation: InterpolationType::ContinuousBezier,
                    },
                },
            ),
            Err(AudioAutomationEditError::UnknownKeyframe(unknown_id))
        );
        assert_eq!(sequence, before);
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

    #[test]
    fn selected_values_share_definition_preview_and_author_admission() {
        for mode in [
            InterpolationType::AutoBezier,
            InterpolationType::ContinuousBezier,
        ] {
            let mut sequence = Sequence::new("Group gain values");
            let track_id = sequence.audio_tracks[0].id;
            let target = AudioAutomationTarget::ChannelFader {
                owner: AudioChannelStripOwner::Track { track_id },
            };
            let mut ids = Vec::new();
            for (time, value) in [(1, -6.0), (2, -3.0), (3, 0.0)] {
                let keyframe = ExactAutomationKeyframe::linear(
                    TimelineTime::new(time, 7).expect("time"),
                    value,
                );
                ids.push(keyframe.id);
                apply_audio_automation_edit(
                    &mut sequence,
                    &AudioAutomationEditRequest {
                        target: target.clone(),
                        edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
                    },
                )
                .expect("seed");
            }
            for id in &ids {
                apply_audio_automation_edit(
                    &mut sequence,
                    &AudioAutomationEditRequest {
                        target: target.clone(),
                        edit: AudioAutomationEdit::SetInterpolation {
                            keyframe_id: *id,
                            interpolation: mode,
                        },
                    },
                )
                .expect("mode");
            }
            let source = inspect_audio_automation(&sequence, &target)
                .expect("inspection")
                .curve()
                .expect("curve")
                .clone();
            let contract = inspect_audio_automation(&sequence, &target)
                .expect("inspection")
                .curve_contract()
                .clone();
            let edit = AudioAutomationEdit::OffsetKeyframeValues {
                keyframe_ids: ids.clone(),
                delta: AUDIO_GAIN_DB_MAX,
                steps: 5,
            };
            let request = AudioAutomationEditRequest { target: target.clone(), edit: edit.clone() };
            let reopened: AudioAutomationEditRequest =
                serde_json::from_slice(&serde_json::to_vec(&request).expect("encode intent"))
                    .expect("decode intent");
            assert_eq!(reopened, request);
            let candidate = contract.preview_edit(&source, &edit).expect("preview");
            apply_audio_automation_edit(&mut sequence, &request).expect("commit");
            assert_eq!(
                inspect_audio_automation(&sequence, &target).expect("inspection").curve(),
                Some(&candidate)
            );
            for (old, new) in source.keyframes.iter().zip(&candidate.keyframes) {
                assert_eq!(new.id, old.id);
                assert_eq!(new.time, old.time);
                assert_eq!(new.value, old.value + AUDIO_GAIN_DB_MAX);
                assert_eq!(new.tangent_mode, old.tangent_mode);
            }
            let before = sequence.clone();
            apply_audio_automation_edit(&mut sequence, &request).expect("bound no-op");
            assert_eq!(sequence, before);
            for selection in [
                vec![],
                vec![ids[0], ids[0]],
                vec![ids[0], KeyframeId::new()],
            ] {
                assert!(apply_audio_automation_edit(
                    &mut sequence,
                    &AudioAutomationEditRequest {
                        target: target.clone(),
                        edit: AudioAutomationEdit::OffsetKeyframeValues {
                            keyframe_ids: selection,
                            delta: 0.0,
                            steps: 0
                        }
                    }
                )
                .is_err());
                assert_eq!(sequence, before);
            }
            sequence.audio_tracks[0].is_locked = true;
            let locked = sequence.clone();
            assert!(matches!(
                apply_audio_automation_edit(&mut sequence, &request),
                Err(AudioAutomationEditError::Blocked(_))
            ));
            assert_eq!(sequence, locked);
        }
    }

    #[test]
    fn selected_values_refuse_new_control_overflow_before_preview_or_publication() {
        use mondrian_core::{AutomationSegmentInterpolation, ExactBezierHandle};
        let mut sequence = Sequence::new("Group gain control bounds");
        let target = AudioAutomationTarget::ChannelFader {
            owner: AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id },
        };
        let mut left = ExactAutomationKeyframe::linear(TimelineTime::ZERO, -119.0);
        left.interpolation_to_next = AutomationSegmentInterpolation::Bezier;
        let mut right = ExactAutomationKeyframe::linear(TimelineTime::ONE, -117.0);
        right.in_handle = Some(ExactBezierHandle {
            time_offset: TimelineTime::NEGATIVE_ONE_THIRD,
            value_offset: 2.0,
        });
        let ids = vec![left.id, right.id];
        for keyframe in [right, left] {
            apply_audio_automation_edit(
                &mut sequence,
                &AudioAutomationEditRequest {
                    target: target.clone(),
                    edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
                },
            )
            .expect("legal controls");
        }
        let inspection = inspect_audio_automation(&sequence, &target).expect("inspection");
        let contract = inspection.curve_contract().clone();
        let curve = inspection.curve().expect("curve").clone();
        let original = sequence.clone();
        let edit = AudioAutomationEdit::OffsetKeyframeValues {
            keyframe_ids: ids.clone(),
            delta: f64::MAX,
            steps: 1,
        };
        let error = contract
            .preview_edit(&curve, &edit)
            .expect_err("control exceeds gain bound despite legal keys");
        assert_eq!(
            apply_audio_automation_edit(
                &mut sequence,
                &AudioAutomationEditRequest { target: target.clone(), edit }
            )
            .expect_err("canonical refusal"),
            error
        );
        assert!(matches!(
            error,
            AudioAutomationEditError::AuthorState(AudioAuthoringError::InvalidGain)
        ));
        assert_eq!(sequence, original);
        let edit =
            AudioAutomationEdit::OffsetKeyframeValues { keyframe_ids: ids, delta: 1.0, steps: 1 };
        let candidate = contract.preview_edit(&curve, &edit).expect("legal recovery");
        apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest { target: target.clone(), edit },
        )
        .expect("legal commit");
        assert_eq!(
            inspect_audio_automation(&sequence, &target).expect("inspection").curve(),
            Some(&candidate)
        );
    }

    #[test]
    fn selected_time_shift_is_atomic_in_owner_domain_and_obeys_shared_locks() {
        let mut sequence = Sequence::new("Group owner translation");
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
            .expect("audio");
        let component = &sequence.audio_tracks[0].clips[0].audio_components[0];
        let scope_id = component.processing.scope_id;
        let edit_id = component.id;
        for target in [
            AudioAutomationTarget::ComponentVolume { track_id, clip_id, edit_id },
            AudioAutomationTarget::ProcessingScopeInputGain { scope_id },
        ] {
            let mut ids = Vec::new();
            for time in [7, 8, 11] {
                let keyframe = ExactAutomationKeyframe::linear(
                    TimelineTime::new(time, 1).expect("owner time"),
                    -6.0,
                );
                ids.push(keyframe.id);
                apply_audio_automation_edit(
                    &mut sequence,
                    &AudioAutomationEditRequest {
                        target: target.clone(),
                        edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
                    },
                )
                .expect("seed owner");
            }
            let original = sequence.clone();
            let request = AudioAutomationEditRequest {
                target: target.clone(),
                edit: AudioAutomationEdit::ShiftKeyframes {
                    keyframe_ids: vec![ids[1], ids[0]],
                    delta: TimelineTime::ONE,
                },
            };
            let encoded = serde_json::to_vec(&request).expect("encode closed intent");
            let decoded: AudioAutomationEditRequest =
                serde_json::from_slice(&encoded).expect("decode closed intent");
            assert_eq!(decoded, request);
            let curve = inspect_audio_automation(&sequence, &target)
                .expect("owner")
                .curve()
                .expect("curve")
                .clone();
            let projected = inspect_audio_automation(&sequence, &target)
                .expect("owner")
                .curve_contract()
                .preview_edit(&curve, &request.edit)
                .expect("canonical projection");
            assert!(
                apply_audio_automation_edit(&mut sequence, &decoded)
                    .expect("one group edit")
                    .changed
            );
            assert_eq!(
                inspect_audio_automation(&sequence, &target).expect("owner").curve(),
                Some(&projected)
            );
            assert_eq!(
                projected.keyframes[0].time,
                TimelineTime::new(8, 1).expect("time")
            );
            assert_eq!(
                projected.keyframes[1].time,
                TimelineTime::new(9, 1).expect("time")
            );
            assert_eq!(projected.keyframes[2], curve.keyframes[2]);
            let moved = sequence.clone();
            for (keyframe_ids, delta) in [
                (vec![], TimelineTime::ZERO),
                (vec![ids[0], ids[0]], TimelineTime::ZERO),
                (vec![ids[0], KeyframeId::new()], TimelineTime::ZERO),
                (
                    vec![ids[0], ids[1]],
                    TimelineTime::new(2, 1).expect("collision"),
                ),
            ] {
                assert!(apply_audio_automation_edit(
                    &mut sequence,
                    &AudioAutomationEditRequest {
                        target: target.clone(),
                        edit: AudioAutomationEdit::ShiftKeyframes { keyframe_ids, delta }
                    }
                )
                .is_err());
                assert_eq!(sequence, moved, "no partial mutation on refusal");
            }
            assert!(
                !apply_audio_automation_edit(
                    &mut sequence,
                    &AudioAutomationEditRequest {
                        target: target.clone(),
                        edit: AudioAutomationEdit::ShiftKeyframes {
                            keyframe_ids: ids[..2].to_vec(),
                            delta: TimelineTime::ZERO
                        }
                    }
                )
                .expect("valid zero delta")
                .changed
            );
            assert_eq!(sequence, moved);
            sequence.audio_tracks[0].is_locked = true;
            let locked = sequence.clone();
            assert!(matches!(
                apply_audio_automation_edit(&mut sequence, &request),
                Err(AudioAutomationEditError::Blocked(_))
            ));
            assert_eq!(sequence, locked);
            sequence.audio_tracks[0].is_locked = false;
            assert!(
                apply_audio_automation_edit(
                    &mut sequence,
                    &AudioAutomationEditRequest {
                        target,
                        edit: AudioAutomationEdit::ShiftKeyframes {
                            keyframe_ids: ids[..2].to_vec(),
                            delta: TimelineTime::new(-1, 1).expect("reverse")
                        }
                    }
                )
                .expect("reverse group")
                .changed
            );
            assert_eq!(sequence, original);
        }
    }

    #[test]
    fn curve_contract_admits_same_controls_as_canonical_gain_pan_and_scope() {
        use mondrian_core::{AutomationSegmentInterpolation, ExactBezierHandle};
        let mut sequence = Sequence::new("Definition admission");
        let track_id = sequence.audio_tracks[0].id;
        let clip = crate::Clip::new(
            mondrian_core::AssetId::new(),
            TimelineTime::ZERO,
            TimelineTime::ONE,
        )
        .expect("clip");
        let clip_id = clip.id;
        sequence
            .add_media_audio_clip(
                track_id,
                clip,
                mondrian_core::AudioSourceComponentId::primary(),
            )
            .expect("audio");
        let component = &sequence.audio_tracks[0].clips[0].audio_components[0];
        let edit_id = component.id;
        let scope_id = component.processing.scope_id;
        for target in [
            AudioAutomationTarget::ComponentVolume { track_id, clip_id, edit_id },
            AudioAutomationTarget::ComponentPan { track_id, clip_id, edit_id },
            AudioAutomationTarget::ProcessingScopeInputGain { scope_id },
            AudioAutomationTarget::ChannelFader {
                owner: AudioChannelStripOwner::Track { track_id },
            },
        ] {
            for time in [TimelineTime::ZERO, TimelineTime::ONE] {
                apply_audio_automation_edit(
                    &mut sequence,
                    &AudioAutomationEditRequest {
                        target: target.clone(),
                        edit: AudioAutomationEdit::UpsertKeyframe {
                            keyframe: ExactAutomationKeyframe::linear(time, 0.0),
                        },
                    },
                )
                .expect("seed");
            }
            let inspection = inspect_audio_automation(&sequence, &target).expect("owner");
            let contract = inspection.curve_contract().clone();
            let source = inspection.curve().expect("curve").clone();
            let original = sequence.clone();
            let mut keyframe = source.keyframes[0].clone();
            keyframe.interpolation_to_next = AutomationSegmentInterpolation::Bezier;
            keyframe.out_handle = Some(ExactBezierHandle {
                time_offset: TimelineTime::new(1, 3).expect("offset"),
                value_offset: if matches!(target, AudioAutomationTarget::ComponentPan { .. }) {
                    2.0
                } else {
                    25.0
                },
            });
            let edit = AudioAutomationEdit::UpsertKeyframe { keyframe: keyframe.clone() };
            let error = contract
                .preview_edit(&source, &edit)
                .expect_err("control bounds apply in preview");
            let canonical_error = apply_audio_automation_edit(
                &mut sequence,
                &AudioAutomationEditRequest { target: target.clone(), edit },
            )
            .expect_err("same author refusal");
            assert_eq!(error, canonical_error);
            assert!(matches!(
                error,
                AudioAutomationEditError::AuthorState(
                    AudioAuthoringError::InvalidGain | AudioAuthoringError::InvalidPan(_)
                )
            ));
            assert_eq!(sequence, original);
            let mut wrong = source.clone();
            wrong.parameter_id = ParameterId::new_static("test.unrelated");
            assert_eq!(
                contract
                    .preview_edit(
                        &wrong,
                        &AudioAutomationEdit::ShiftKeyframes {
                            keyframe_ids: vec![source.keyframes[0].id],
                            delta: TimelineTime::ZERO
                        }
                    )
                    .expect_err("even zero edits require the right definition"),
                AudioAutomationEditError::AuthorState(AudioAuthoringError::ParameterKeyMismatch)
            );
            keyframe.out_handle.as_mut().expect("control").value_offset = 0.5;
            let edit = AudioAutomationEdit::UpsertKeyframe { keyframe };
            let prepared = contract.preview_edit(&source, &edit).expect("legal control");
            apply_audio_automation_edit(
                &mut sequence,
                &AudioAutomationEditRequest { target: target.clone(), edit },
            )
            .expect("legal canonical control");
            assert_eq!(
                inspect_audio_automation(&sequence, &target).expect("owner").curve(),
                Some(&prepared)
            );
        }
    }

    #[test]
    fn processor_curve_contract_checks_integer_family_and_animation_permission() {
        use mondrian_core::{
            ParameterInvalidValuePolicy, ParameterNumericContract, ParameterUnit, PropertyValue,
        };
        let schema = ParameterSchema::v1(
            ParameterId::new_static("test.discrete"),
            PropertyValue::Int(2),
        )
        .with_numeric_contract(
            ParameterUnit::Unitless,
            ParameterNumericContract::closed(
                0.0,
                10.0,
                Some(1.0),
                ParameterInvalidValuePolicy::Reject,
            )
            .expect("numeric"),
        );
        let contract = AudioAutomationCurveContract::processor(schema.clone());
        let mut source =
            ExactAutomationCurve::new(schema.parameter_id.clone(), 2.0).expect("source");
        assert_eq!(
            contract
                .preview_edit(&source, &AudioAutomationEdit::Clear { default_value: 3.5 })
                .expect_err("fractional integer"),
            AudioAutomationEditError::AuthorState(
                AudioAuthoringError::InvalidProcessorParameterValue
            )
        );
        let mut keyframe = ExactAutomationKeyframe::linear(TimelineTime::ZERO, 3.0);
        keyframe.interpolation_to_next = mondrian_core::AutomationSegmentInterpolation::Hold;
        source = contract
            .preview_edit(
                &source,
                &AudioAutomationEdit::UpsertKeyframe { keyframe: keyframe.clone() },
            )
            .expect("integer Hold");
        assert_eq!(
            contract
                .preview_edit(
                    &source,
                    &AudioAutomationEdit::SetInterpolation {
                        keyframe_id: keyframe.id,
                        interpolation: InterpolationType::AutoBezier
                    }
                )
                .expect_err("definition disallows Bezier"),
            AudioAutomationEditError::InterpolationNotAllowed
        );
        for interpolation in [
            InterpolationType::AutoBezier,
            InterpolationType::ContinuousBezier,
            InterpolationType::Linear,
        ] {
            assert_eq!(
                contract
                    .preview_edit(
                        &source,
                        &AudioAutomationEdit::SetInterpolations {
                            keyframe_ids: vec![keyframe.id],
                            interpolation,
                        }
                    )
                    .expect_err("batch obeys current integer family"),
                AudioAutomationEditError::InterpolationNotAllowed
            );
        }
        let constant =
            ExactAutomationCurve::new(schema.parameter_id.clone(), 2.0).expect("constant");
        let static_contract =
            AudioAutomationCurveContract::processor(schema.with_animatable(false));
        assert_eq!(
            static_contract
                .preview_edit(&constant, &AudioAutomationEdit::UpsertKeyframe { keyframe })
                .expect_err("static definition"),
            AudioAutomationEditError::AuthorState(
                AudioAuthoringError::ParameterDoesNotAdmitAutomation
            )
        );
    }

    #[test]
    fn selected_interpolations_share_definition_preview_author_codec_and_locks() {
        for mode in [
            InterpolationType::Hold,
            InterpolationType::Linear,
            InterpolationType::Bezier,
            InterpolationType::AutoBezier,
            InterpolationType::ContinuousBezier,
        ] {
            let mut sequence = Sequence::new("Complete interpolation");
            let target = AudioAutomationTarget::ChannelFader {
                owner: AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id },
            };
            let mut ids = Vec::new();
            for (time, value) in [(0, -6.0), (1, -3.0), (4, 0.0)] {
                let keyframe = ExactAutomationKeyframe::linear(
                    TimelineTime::new(time, 7).expect("time"),
                    value,
                );
                ids.push(keyframe.id);
                apply_audio_automation_edit(
                    &mut sequence,
                    &AudioAutomationEditRequest {
                        target: target.clone(),
                        edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
                    },
                )
                .expect("seed");
            }
            let inspection = inspect_audio_automation(&sequence, &target).expect("inspection");
            let source = inspection.curve().expect("curve").clone();
            let contract = inspection.curve_contract().clone();
            let edit = AudioAutomationEdit::SetInterpolations {
                keyframe_ids: ids.clone(),
                interpolation: mode,
            };
            let candidate = contract.preview_edit(&source, &edit).expect("preview");
            let request = AudioAutomationEditRequest { target: target.clone(), edit: edit.clone() };
            let decoded: AudioAutomationEditRequest =
                serde_json::from_slice(&serde_json::to_vec(&request).expect("encode"))
                    .expect("decode");
            assert_eq!(decoded, request);
            apply_audio_automation_edit(&mut sequence, &request).expect("commit");
            assert_eq!(
                inspect_audio_automation(&sequence, &target).expect("inspection").curve(),
                Some(&candidate)
            );
            for (old, new) in source.keyframes.iter().zip(&candidate.keyframes) {
                assert_eq!(new.id, old.id);
                assert_eq!(new.time, old.time);
                assert_eq!(new.value.to_bits(), old.value.to_bits());
            }
            let before = sequence.clone();
            assert!(
                !apply_audio_automation_edit(&mut sequence, &request)
                    .expect("same preset")
                    .changed
            );
            assert_eq!(sequence, before);
            for selected in [
                vec![],
                vec![ids[0], ids[0]],
                vec![ids[0], KeyframeId::new()],
            ] {
                let edit = AudioAutomationEdit::SetInterpolations {
                    keyframe_ids: selected,
                    interpolation: mode,
                };
                assert!(contract.preview_edit(&candidate, &edit).is_err());
                assert!(apply_audio_automation_edit(
                    &mut sequence,
                    &AudioAutomationEditRequest { target: target.clone(), edit }
                )
                .is_err());
                assert_eq!(sequence, before);
            }
            sequence.audio_tracks[0].is_locked = true;
            let locked = sequence.clone();
            assert!(matches!(
                apply_audio_automation_edit(&mut sequence, &request),
                Err(AudioAutomationEditError::Blocked(_))
            ));
            assert_eq!(sequence, locked);
        }
    }

    #[test]
    fn selected_continuous_interpolation_refuses_new_control_bounds_and_recovers() {
        use mondrian_core::ExactBezierHandle;
        let mut sequence = Sequence::new("Constrained group admission");
        let target = AudioAutomationTarget::ChannelFader {
            owner: AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id },
        };
        let left = ExactAutomationKeyframe::linear(TimelineTime::ZERO, -119.0);
        let mut middle = ExactAutomationKeyframe::linear(TimelineTime::ONE, -119.0);
        middle.in_handle = Some(ExactBezierHandle {
            time_offset: TimelineTime::NEGATIVE_ONE_THIRD,
            value_offset: 4.0,
        });
        middle.out_handle = Some(ExactBezierHandle {
            time_offset: TimelineTime::ONE_THIRD,
            value_offset: 0.0,
        });
        let ids = vec![left.id, middle.id];
        for keyframe in [
            left,
            middle,
            ExactAutomationKeyframe::linear(TimelineTime::new(2, 1).expect("time"), -117.0),
        ] {
            apply_audio_automation_edit(
                &mut sequence,
                &AudioAutomationEditRequest {
                    target: target.clone(),
                    edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
                },
            )
            .expect("legal source");
        }
        let inspection = inspect_audio_automation(&sequence, &target).expect("inspection");
        let source = inspection.curve().expect("curve").clone();
        let contract = inspection.curve_contract().clone();
        let before = sequence.clone();
        let edit = AudioAutomationEdit::SetInterpolations {
            keyframe_ids: ids.clone(),
            interpolation: InterpolationType::ContinuousBezier,
        };
        let error = contract
            .preview_edit(&source, &edit)
            .expect_err("new continuous control below gain floor");
        assert_eq!(
            error,
            AudioAutomationEditError::AuthorState(AudioAuthoringError::InvalidGain)
        );
        assert_eq!(
            apply_audio_automation_edit(
                &mut sequence,
                &AudioAutomationEditRequest { target: target.clone(), edit }
            )
            .expect_err("author refusal"),
            error
        );
        assert_eq!(sequence, before);
        let edit = AudioAutomationEdit::SetInterpolations {
            keyframe_ids: ids,
            interpolation: InterpolationType::AutoBezier,
        };
        let candidate = contract.preview_edit(&source, &edit).expect("legal recovery");
        apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest { target: target.clone(), edit },
        )
        .expect("commit recovery");
        assert_eq!(
            inspect_audio_automation(&sequence, &target).expect("inspection").curve(),
            Some(&candidate)
        );
    }
}
