//! Persistent Sequence-owned audio authoring.
//!
//! Timeline placement remains owned by `Track -> Clip`. This module owns only
//! non-placement audio intent: per-placement component edits, shareable
//! processing scopes, mixer state, routing, transitions, and public outputs.

use crate::{clip::Clip, track::Track};
use mondrian_core::{
    AudioChannelLayout, AudioChannelMixMatrix, AudioComponentEditId, AudioProcessingScopeId,
    AudioProcessorInstanceId, AudioRoleId, AudioRouteId, AudioSourceComponentId, AudioTransitionId,
    AuthoringFootprint, AuthoringFootprintCollector, AuthoringFootprintError, AuthoringList,
    AuthoringMap, AutomationSegmentInterpolation, ClipId, ExactAutomationCurve, MixBusId,
    ParameterCacheImpact, ParameterId, ParameterInterpolation, ParameterInvalidValuePolicy,
    ParameterNumericContract, ParameterNumericRange, ParameterSchema, ParameterUnit,
    ProgramOutputId, PropertyValue, PropertyValueType, TimelineTime, TimelineTimeRange, TrackId,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Stable built-in definition identity for a gain processor.
pub const BUILTIN_GAIN_DEFINITION_ID: &str = "mondrian.audio.gain";
/// Stable parameter identity for a gain processor in decibels.
pub const GAIN_DB_PARAMETER_ID: &str = "mondrian.audio.gain.db";
/// Stable built-in definition identity for a causal sample delay.
pub const BUILTIN_SAMPLE_DELAY_DEFINITION_ID: &str = "mondrian.audio.sample_delay";
/// Stable parameter identity for a sample delay's exact delay length.
pub const SAMPLE_DELAY_FRAMES_PARAMETER_ID: &str = "mondrian.audio.sample_delay.frames";
/// Largest authorable delay retained by one built-in Sample Delay instance.
pub const SAMPLE_DELAY_MAX_FRAMES: i64 = 192_000;
/// Stable built-in definition identity for the linked-channel sample-peak limiter.
pub const BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID: &str = "mondrian.audio.lookahead_limiter";
/// Stable ceiling parameter identity for the built-in Lookahead Limiter.
pub const LOOKAHEAD_LIMITER_CEILING_DB_PARAMETER_ID: &str =
    "mondrian.audio.lookahead_limiter.ceiling_db";
/// Stable lookahead parameter identity for the built-in Lookahead Limiter.
pub const LOOKAHEAD_LIMITER_LOOKAHEAD_MS_PARAMETER_ID: &str =
    "mondrian.audio.lookahead_limiter.lookahead_ms";
/// Stable release parameter identity for the built-in Lookahead Limiter.
pub const LOOKAHEAD_LIMITER_RELEASE_MS_PARAMETER_ID: &str =
    "mondrian.audio.lookahead_limiter.release_ms";
/// Default sample-peak ceiling in dBFS.
pub const LOOKAHEAD_LIMITER_DEFAULT_CEILING_DB: f64 = -1.0;
/// Default hidden lookahead in milliseconds.
pub const LOOKAHEAD_LIMITER_DEFAULT_LOOKAHEAD_MS: f64 = 5.0;
/// Default gain-release time in milliseconds.
pub const LOOKAHEAD_LIMITER_DEFAULT_RELEASE_MS: f64 = 100.0;
/// Maximum hidden lookahead admitted by the built-in processor.
pub const LOOKAHEAD_LIMITER_MAX_LOOKAHEAD_MS: f64 = 20.0;
/// Stable parameter identity for processing-scope input trim.
pub const INPUT_GAIN_DB_PARAMETER_ID: &str = "mondrian.audio.input_gain.db";
/// Stable parameter identity for placement-local volume.
pub const CLIP_VOLUME_DB_PARAMETER_ID: &str = "mondrian.audio.clip_volume.db";
/// Stable parameter identity for placement-local stereo pan/balance.
pub const CLIP_PAN_PARAMETER_ID: &str = "mondrian.audio.clip_pan";
/// Stable parameter identity for a channel-strip fader in decibels.
pub const FADER_DB_PARAMETER_ID: &str = "mondrian.audio.fader.db";
/// Stable parameter identity for one Route edge's send level.
pub const ROUTE_GAIN_DB_PARAMETER_ID: &str = "mondrian.audio.route_gain.db";
/// Lowest admitted authored gain. Values at this floor remain finite linear PCM.
pub const AUDIO_GAIN_DB_MIN: f64 = -120.0;
/// Highest admitted authored gain. This matches the built-in Gain hard limit.
pub const AUDIO_GAIN_DB_MAX: f64 = 24.0;

/// Definition contract for the built-in gain processor's decibel parameter.
///
/// The hard interval protects DSP execution from non-finite or pathological
/// persisted values. The narrower soft interval is the ordinary editor range;
/// users can still enter the full hard interval explicitly.
pub fn gain_parameter_schema() -> ParameterSchema {
    ParameterSchema::v1(
        ParameterId::new_static(GAIN_DB_PARAMETER_ID),
        PropertyValue::Double(0.0),
    )
    .with_numeric_contract(
        ParameterUnit::Decibels,
        ParameterNumericContract {
            hard_range: ParameterNumericRange { min: AUDIO_GAIN_DB_MIN, max: AUDIO_GAIN_DB_MAX },
            soft_range: ParameterNumericRange { min: -60.0, max: 12.0 },
            step: Some(0.1),
            invalid_value_policy: ParameterInvalidValuePolicy::Reject,
        },
    )
}

/// Definition contract for the built-in Sample Delay's exact delay length.
///
/// The value is a non-negative integer count on the active audio Evaluation
/// Grid. It is deliberately non-animatable because changing it alters retained
/// history, Session storage, audible time behavior, and continuity obligations and
/// therefore requires a newly prepared plan rather than a live parameter event.
pub fn sample_delay_frames_parameter_schema() -> ParameterSchema {
    ParameterSchema::v1(
        ParameterId::new_static(SAMPLE_DELAY_FRAMES_PARAMETER_ID),
        PropertyValue::Int(0),
    )
    .with_numeric_contract(
        ParameterUnit::Samples,
        ParameterNumericContract {
            hard_range: ParameterNumericRange { min: 0.0, max: SAMPLE_DELAY_MAX_FRAMES as f64 },
            soft_range: ParameterNumericRange { min: 0.0, max: 4_800.0 },
            step: Some(1.0),
            invalid_value_policy: ParameterInvalidValuePolicy::Reject,
        },
    )
    .with_animatable(false)
    .with_cache_impact(ParameterCacheImpact::Topology)
}

/// Definition contract for the Lookahead Limiter's linked-channel sample ceiling.
pub fn lookahead_limiter_ceiling_parameter_schema() -> ParameterSchema {
    ParameterSchema::v1(
        ParameterId::new_static(LOOKAHEAD_LIMITER_CEILING_DB_PARAMETER_ID),
        PropertyValue::Double(LOOKAHEAD_LIMITER_DEFAULT_CEILING_DB),
    )
    .with_numeric_contract(
        ParameterUnit::Decibels,
        ParameterNumericContract {
            hard_range: ParameterNumericRange { min: -24.0, max: 0.0 },
            soft_range: ParameterNumericRange { min: -12.0, max: 0.0 },
            step: Some(0.1),
            invalid_value_policy: ParameterInvalidValuePolicy::Reject,
        },
    )
}

/// Definition contract for the Lookahead Limiter's hidden analysis window.
///
/// The author value is sample-rate-independent. Preparation rounds upward to
/// an exact number of frames so the realized window is never shorter than the
/// requested duration. Live automation is forbidden because the value changes
/// algorithmic latency, retained history, and PDC obligations.
pub fn lookahead_limiter_lookahead_parameter_schema() -> ParameterSchema {
    ParameterSchema::v1(
        ParameterId::new_static(LOOKAHEAD_LIMITER_LOOKAHEAD_MS_PARAMETER_ID),
        PropertyValue::Double(LOOKAHEAD_LIMITER_DEFAULT_LOOKAHEAD_MS),
    )
    .with_numeric_contract(
        ParameterUnit::Milliseconds,
        ParameterNumericContract {
            hard_range: ParameterNumericRange { min: 0.0, max: LOOKAHEAD_LIMITER_MAX_LOOKAHEAD_MS },
            soft_range: ParameterNumericRange { min: 0.0, max: 10.0 },
            step: Some(0.1),
            invalid_value_policy: ParameterInvalidValuePolicy::Reject,
        },
    )
    .with_animatable(false)
    .with_cache_impact(ParameterCacheImpact::Topology)
}

/// Definition contract for the Lookahead Limiter's gain-release time.
pub fn lookahead_limiter_release_parameter_schema() -> ParameterSchema {
    ParameterSchema::v1(
        ParameterId::new_static(LOOKAHEAD_LIMITER_RELEASE_MS_PARAMETER_ID),
        PropertyValue::Double(LOOKAHEAD_LIMITER_DEFAULT_RELEASE_MS),
    )
    .with_numeric_contract(
        ParameterUnit::Milliseconds,
        ParameterNumericContract {
            hard_range: ParameterNumericRange { min: 5.0, max: 5_000.0 },
            soft_range: ParameterNumericRange { min: 20.0, max: 1_000.0 },
            step: Some(1.0),
            invalid_value_policy: ParameterInvalidValuePolicy::Reject,
        },
    )
}

/// A persistent reference to one processor definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioProcessorDefinitionRef {
    /// A Mondrian processor with a versioned parameter schema.
    BuiltIn {
        definition_id: String,
        schema_version: u32,
    },
    /// A VST3 audio-effect class. Paths and scan order are not identity.
    Vst3 {
        class_id: String,
        vendor: Option<String>,
        schema_version: u32,
    },
    /// A CLAP audio-effect definition.
    Clap {
        plugin_id: String,
        schema_version: u32,
    },
}

/// One processor parameter definition snapshot and its exact author-time value.
///
/// The schema remains editable and serializable when a plugin is unavailable.
/// The curve owns both the current unkeyed value and any automation keys; there
/// is no parallel static-value field that could disagree with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioProcessorParameter {
    /// Stable, versioned definition contract captured for this instance.
    pub schema: ParameterSchema,
    /// Exact owner-local parameter curve, including its unkeyed value.
    pub automation: ExactAutomationCurve,
}

impl AudioProcessorParameter {
    /// Create an unkeyed parameter from a validated numeric schema.
    pub fn from_schema(schema: ParameterSchema) -> Result<Self, AudioAuthoringError> {
        let default_value = processor_parameter_default_value(&schema.default_value)?;
        let automation = ExactAutomationCurve::new(schema.parameter_id.clone(), default_value)
            .map_err(|error| AudioAuthoringError::InvalidAutomation {
                reason: error.to_string(),
            })?;
        let parameter = Self { schema, automation };
        parameter.validate()?;
        Ok(parameter)
    }

    /// Replace the complete exact-time curve after validating it against the schema.
    pub fn set_automation(
        &mut self,
        automation: ExactAutomationCurve,
    ) -> Result<(), AudioAuthoringError> {
        let candidate = Self { schema: self.schema.clone(), automation };
        candidate.validate()?;
        self.automation = candidate.automation;
        Ok(())
    }

    fn validate(&self) -> Result<(), AudioAuthoringError> {
        self.schema
            .validate()
            .map_err(|error| AudioAuthoringError::InvalidParameterSchema {
                reason: error.to_string(),
            })?;
        if !matches!(
            self.schema.value_type,
            PropertyValueType::Double | PropertyValueType::Int
        ) {
            return Err(AudioAuthoringError::UnsupportedProcessorParameterType);
        }
        if self.schema.parameter_id != self.automation.parameter_id {
            return Err(AudioAuthoringError::ParameterKeyMismatch);
        }
        validate_curve(&self.automation)?;
        if !self.schema.is_animatable && !self.automation.keyframes.is_empty() {
            return Err(AudioAuthoringError::ParameterDoesNotAdmitAutomation);
        }
        validate_parameter_value(&self.schema, self.automation.default_value)?;
        for keyframe in &self.automation.keyframes {
            validate_parameter_value(&self.schema, keyframe.value)?;
            if let Some(handle) = keyframe.in_handle {
                validate_parameter_value(&self.schema, keyframe.value + handle.value_offset)?;
            }
            if let Some(handle) = keyframe.out_handle {
                validate_parameter_value(&self.schema, keyframe.value + handle.value_offset)?;
            }
        }
        for keyframe in self
            .automation
            .keyframes
            .iter()
            .take(self.automation.keyframes.len().saturating_sub(1))
        {
            let interpolation = match keyframe.interpolation_to_next {
                AutomationSegmentInterpolation::Hold => ParameterInterpolation::Hold,
                AutomationSegmentInterpolation::Linear => ParameterInterpolation::Linear,
                AutomationSegmentInterpolation::Bezier => ParameterInterpolation::Bezier,
            };
            if !self.schema.allowed_interpolations.contains(&interpolation) {
                return Err(AudioAuthoringError::UnsupportedParameterInterpolation);
            }
        }
        Ok(())
    }
}

/// One persistent processor instance used at every supported insertion point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioProcessorInstance {
    /// Stable identity of this authored instance and its automation.
    pub id: AudioProcessorInstanceId,
    /// Stable processor definition reference.
    pub definition: AudioProcessorDefinitionRef,
    /// Explicit user bypass state.
    pub bypassed: bool,
    /// Definition snapshots and curves keyed by stable parameter identity.
    /// The map detaches only when this processor's parameter state is edited.
    pub parameters: AuthoringMap<ParameterId, AudioProcessorParameter>,
    /// Opaque, versioned plugin state preserved while a dependency is unavailable.
    /// Bytes remain structurally shared across immutable author snapshots.
    pub opaque_state: Option<AuthoringList<u8>>,
}

impl AudioProcessorInstance {
    /// Create a built-in processor instance with its known definition schema.
    pub fn built_in(definition_id: impl Into<String>, schema_version: u32) -> Self {
        let definition_id = definition_id.into();
        let mut parameters = AuthoringMap::new();
        if definition_id == BUILTIN_GAIN_DEFINITION_ID && schema_version == 1 {
            let schema = gain_parameter_schema();
            let parameter_id = schema.parameter_id.clone();
            let automation = ExactAutomationCurve {
                parameter_id: parameter_id.clone(),
                default_value: 0.0,
                keyframes: AuthoringList::new(),
            };
            parameters.insert(parameter_id, AudioProcessorParameter { schema, automation });
        } else if definition_id == BUILTIN_SAMPLE_DELAY_DEFINITION_ID && schema_version == 1 {
            let schema = sample_delay_frames_parameter_schema();
            let parameter_id = schema.parameter_id.clone();
            let automation = ExactAutomationCurve {
                parameter_id: parameter_id.clone(),
                default_value: 0.0,
                keyframes: AuthoringList::new(),
            };
            parameters.insert(parameter_id, AudioProcessorParameter { schema, automation });
        } else if definition_id == BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID && schema_version == 1 {
            for (schema, default_value) in [
                (
                    lookahead_limiter_ceiling_parameter_schema(),
                    LOOKAHEAD_LIMITER_DEFAULT_CEILING_DB,
                ),
                (
                    lookahead_limiter_lookahead_parameter_schema(),
                    LOOKAHEAD_LIMITER_DEFAULT_LOOKAHEAD_MS,
                ),
                (
                    lookahead_limiter_release_parameter_schema(),
                    LOOKAHEAD_LIMITER_DEFAULT_RELEASE_MS,
                ),
            ] {
                let parameter_id = schema.parameter_id.clone();
                let automation = ExactAutomationCurve {
                    parameter_id: parameter_id.clone(),
                    default_value,
                    keyframes: AuthoringList::new(),
                };
                parameters.insert(parameter_id, AudioProcessorParameter { schema, automation });
            }
        }
        Self {
            id: AudioProcessorInstanceId::new(),
            definition: AudioProcessorDefinitionRef::BuiltIn { definition_id, schema_version },
            bypassed: false,
            parameters,
            opaque_state: None,
        }
    }

    /// Replace one known parameter curve by exact stable identity.
    pub fn set_parameter_automation(
        &mut self,
        automation: ExactAutomationCurve,
    ) -> Result<(), AudioAuthoringError> {
        let parameter = self
            .parameters
            .get_mut(&automation.parameter_id)
            .ok_or(AudioAuthoringError::UnknownProcessorParameter)?;
        parameter.set_automation(automation)
    }

    fn validate(&self) -> Result<(), AudioAuthoringError> {
        for (parameter_id, parameter) in &self.parameters {
            if parameter_id != &parameter.schema.parameter_id
                || parameter_id != &parameter.automation.parameter_id
            {
                return Err(AudioAuthoringError::ParameterKeyMismatch);
            }
            parameter.validate()?;
        }
        Ok(())
    }
}

/// An ordered processor chain. Order is author intent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioProcessorRack {
    /// Processors in signal-flow order, structurally shared until this Rack is edited.
    pub processors: AuthoringList<AudioProcessorInstance>,
}

impl AudioProcessorRack {
    fn validate(&self) -> Result<(), AudioAuthoringError> {
        let mut identities = BTreeSet::new();
        for processor in &self.processors {
            if !identities.insert(processor.id) {
                return Err(AudioAuthoringError::DuplicateProcessorInstance(
                    processor.id,
                ));
            }
            processor.validate()?;
        }
        Ok(())
    }
}

/// Shared mixer processing owned by a Track, Bus, or Program Output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioChannelStrip {
    /// Static input trim in dB, applied before the pre-fader rack.
    pub input_trim_db: f64,
    /// Ordered processors before the fader.
    pub pre_fader: AudioProcessorRack,
    /// Static fader value used when no automation curve is present.
    pub fader_db: f64,
    /// Optional Sequence-local fader automation.
    pub fader_automation: Option<ExactAutomationCurve>,
    /// Ordered processors after the fader.
    pub post_fader: AudioProcessorRack,
}

impl Default for AudioChannelStrip {
    fn default() -> Self {
        Self {
            input_trim_db: 0.0,
            pre_fader: AudioProcessorRack::default(),
            fader_db: 0.0,
            fader_automation: None,
            post_fader: AudioProcessorRack::default(),
        }
    }
}

impl AudioChannelStrip {
    fn validate(&self) -> Result<(), AudioAuthoringError> {
        validate_gain_db(self.input_trim_db)?;
        validate_gain_db(self.fader_db)?;
        self.pre_fader.validate()?;
        self.post_fader.validate()?;
        validate_optional_gain_curve(&self.fader_automation, FADER_DB_PARAMETER_ID)
    }
}

/// Source selected by one placement-local audio edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioComponentSource {
    /// A stable audio component of the owning media Clip's Asset.
    Media {
        component_id: AudioSourceComponentId,
    },
    /// A stable public output of the owning nested Sequence Clip.
    NestedOutput { output_id: ProgramOutputId },
}

/// Author intent for mapping one Component signal into its owning Sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AudioComponentChannelMapping {
    /// Resolve Mondrian's versioned, fail-closed standard matrix after the
    /// source dependency's exact semantic layout is known.
    #[default]
    Standard,
    /// Apply this exact authored sparse matrix. Source and destination layouts
    /// are part of the author contract and cannot be inferred from extent.
    Explicit(AudioChannelMixMatrix),
}

/// The deliberately restricted mapping into a non-placement processing scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioProcessingBinding {
    /// Shared author processing definition.
    pub scope_id: AudioProcessingScopeId,
    /// Exact scope-local coordinate corresponding to component-local zero.
    pub scope_in: TimelineTime,
}

/// Curve used by one unary Clip fade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioFadeCurve {
    /// Linear amplitude ramp.
    ConstantGain,
    /// Sin/cos equal-power ramp.
    EqualPower,
}

/// One exact unary fade at a Clip edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFade {
    /// Exact component-local fade duration.
    pub duration: TimelineTime,
    /// Versioned curve mathematics.
    pub curve: AudioFadeCurve,
}

/// Independent fade envelopes at the audible Clip edges.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioClipFades {
    /// Optional fade beginning at the Clip in edge.
    pub fade_in: Option<AudioFade>,
    /// Optional fade ending at the Clip out edge.
    pub fade_out: Option<AudioFade>,
}

/// Placement-local audio behavior attached to exactly one Clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioComponentEdit {
    /// Stable edit identity used by transitions and diagnostics.
    pub id: AudioComponentEditId,
    /// Media component or nested public output selected from the owning Clip.
    pub source: AudioComponentSource,
    /// Mapping from the selected source layout into the Sequence signal layout.
    pub channel_mapping: AudioComponentChannelMapping,
    /// Optional Sequence-local semantic Role.
    pub role_id: Option<AudioRoleId>,
    /// Whether this component contributes signal.
    pub enabled: bool,
    /// Exact edit-local coordinate corresponding to the owning Clip's in edge.
    /// This is a restricted offset, never an independent speed or placement map.
    pub local_time_in: TimelineTime,
    /// Non-placement processing definition and scope-local origin.
    pub processing: AudioProcessingBinding,
    /// Static post-processing placement volume in dB.
    pub volume_db: f64,
    /// Optional component-local volume automation.
    pub volume_automation: Option<ExactAutomationCurve>,
    /// Static stereo pan/balance in the inclusive range `[-1, 1]`.
    pub pan: f64,
    /// Optional component-local pan automation.
    pub pan_automation: Option<ExactAutomationCurve>,
    /// Unary edge fades evaluated after Clip processing.
    pub fades: AudioClipFades,
}

impl AudioComponentEdit {
    /// Create a default media-component edit using one registered scope.
    pub fn media(component_id: AudioSourceComponentId, scope_id: AudioProcessingScopeId) -> Self {
        Self::new(AudioComponentSource::Media { component_id }, scope_id)
    }

    /// Create a default nested-output edit using one registered scope.
    pub fn nested(output_id: ProgramOutputId, scope_id: AudioProcessingScopeId) -> Self {
        Self::new(AudioComponentSource::NestedOutput { output_id }, scope_id)
    }

    fn new(source: AudioComponentSource, scope_id: AudioProcessingScopeId) -> Self {
        Self {
            id: AudioComponentEditId::new(),
            source,
            channel_mapping: AudioComponentChannelMapping::Standard,
            role_id: None,
            enabled: true,
            local_time_in: TimelineTime::ZERO,
            processing: AudioProcessingBinding { scope_id, scope_in: TimelineTime::ZERO },
            volume_db: 0.0,
            volume_automation: None,
            pan: 0.0,
            pan_automation: None,
            fades: AudioClipFades::default(),
        }
    }

    fn validate(
        &self,
        clip: &Clip,
        sequence_layout: AudioChannelLayout,
    ) -> Result<(), AudioAuthoringError> {
        if self.processing.scope_in.is_negative() || self.local_time_in.is_negative() {
            return Err(AudioAuthoringError::NegativeProcessingScopeIn(self.id));
        }
        validate_gain_db(self.volume_db)?;
        if !self.pan.is_finite() || !(-1.0..=1.0).contains(&self.pan) {
            return Err(AudioAuthoringError::InvalidPan(self.id));
        }
        if let AudioComponentChannelMapping::Explicit(matrix) = &self.channel_mapping {
            if matrix.destination_layout() != sequence_layout {
                return Err(AudioAuthoringError::ChannelMappingDestinationMismatch(
                    self.id,
                ));
            }
        }
        validate_optional_gain_curve(&self.volume_automation, CLIP_VOLUME_DB_PARAMETER_ID)?;
        validate_optional_curve(&self.pan_automation, CLIP_PAN_PARAMETER_ID)?;
        for fade in [self.fades.fade_in, self.fades.fade_out].into_iter().flatten() {
            if fade.duration <= TimelineTime::ZERO || fade.duration > clip.duration {
                return Err(AudioAuthoringError::InvalidFade(self.id));
            }
        }
        Ok(())
    }
}

/// Non-placement Clip processing definition, shared only through explicit bindings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioProcessingScope {
    /// Stable author identity and continuity candidate key.
    pub id: AudioProcessingScopeId,
    /// Static input trim before the processor rack.
    pub input_gain_db: f64,
    /// Optional scope-local input trim automation.
    pub input_gain_automation: Option<ExactAutomationCurve>,
    /// Ordered Clip processor rack.
    pub processors: AudioProcessorRack,
}

impl AudioProcessingScope {
    /// Create an identity processing definition.
    pub fn identity() -> Self {
        Self {
            id: AudioProcessingScopeId::new(),
            input_gain_db: 0.0,
            input_gain_automation: None,
            processors: AudioProcessorRack::default(),
        }
    }

    fn validate(&self) -> Result<(), AudioAuthoringError> {
        validate_gain_db(self.input_gain_db)?;
        validate_optional_gain_curve(&self.input_gain_automation, INPUT_GAIN_DB_PARAMETER_ID)?;
        self.processors.validate()
    }
}

/// Track mixer state keyed by the editorial Track identity.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioTrackMixerChannel {
    /// Common channel-strip processing.
    pub strip: AudioChannelStrip,
}

/// A user-created mix Bus with an independent lifetime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioMixBus {
    /// Stable Bus identity.
    pub id: MixBusId,
    /// User-facing label; not semantic identity.
    pub name: String,
    /// Common channel-strip processing.
    pub strip: AudioChannelStrip,
}

/// Closed choice of how a public output obtains its main signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProgramOutputMainSource {
    /// Sum only explicit Routes targeting this output.
    RoutedInputs,
    /// Compile a semantic projection rooted at one Sequence-owned Role.
    SemanticProjection { role_id: AudioRoleId },
}

/// A stable public Sequence output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioProgramOutput {
    /// Stable output identity.
    pub id: ProgramOutputId,
    /// User-facing label.
    pub name: String,
    /// Exclusive source contract.
    pub main_source: ProgramOutputMainSource,
    /// Output-local creative processing before publication.
    pub strip: AudioChannelStrip,
}

/// Sequence-owned semantic audio Role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioRole {
    /// Stable local Role identity.
    pub id: AudioRoleId,
    /// Optional local parent, forming a closed acyclic hierarchy.
    pub parent_id: Option<AudioRoleId>,
    /// User-facing name.
    pub name: String,
    /// Optional standardized semantic key; never identity.
    pub standard_semantic_key: Option<String>,
}

/// Versioned output port of a Track or Bus channel strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AudioChannelStripOutputPort {
    /// Signal after input trim and pre-fader processors, before the fader.
    PreFader,
    /// Signal after the fader/post-fader processors but before Track mute.
    PostFaderPreMute,
    /// Signal after the Track mute gate. This is the ordinary main-route port.
    PostMute,
}

/// Typed Route source; generated operations are never routable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AudioRouteSource {
    /// One explicit output port of a Track Mixer Channel.
    Track {
        track_id: TrackId,
        port: AudioChannelStripOutputPort,
    },
    /// One explicit output port of an authored Mix Bus.
    Bus {
        bus_id: MixBusId,
        port: AudioChannelStripOutputPort,
    },
}

impl AudioRouteSource {
    fn track_id(self) -> Option<TrackId> {
        match self {
            Self::Track { track_id, .. } => Some(track_id),
            Self::Bus { .. } => None,
        }
    }

    fn bus_id(self) -> Option<MixBusId> {
        match self {
            Self::Bus { bus_id, .. } => Some(bus_id),
            Self::Track { .. } => None,
        }
    }
}

/// Typed Route destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AudioRouteDestination {
    /// Main input of one Mix Bus.
    Bus(MixBusId),
    /// Main input of one Program Output.
    Output(ProgramOutputId),
}

/// Explicit signal Route and its Sequence-local edge controls.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioRoute {
    /// Stable Route identity.
    pub id: AudioRouteId,
    /// Typed author source and tap point.
    pub source: AudioRouteSource,
    /// Typed author destination.
    pub destination: AudioRouteDestination,
    /// Whether this edge contributes to the compiled Signal Closure.
    pub enabled: bool,
    /// Static Route/send level used when no automation curve is present.
    pub gain_db: f64,
    /// Optional Sequence-local Route/send-level automation.
    pub gain_automation: Option<ExactAutomationCurve>,
}

impl AudioRoute {
    /// Construct one enabled unity-gain Route edge.
    pub fn new(source: AudioRouteSource, destination: AudioRouteDestination) -> Self {
        Self {
            id: AudioRouteId::new(),
            source,
            destination,
            enabled: true,
            gain_db: 0.0,
            gain_automation: None,
        }
    }

    fn validate(&self) -> Result<(), AudioAuthoringError> {
        validate_gain_db(self.gain_db)?;
        validate_optional_gain_curve(&self.gain_automation, ROUTE_GAIN_DB_PARAMETER_ID)
    }
}

/// Transition curve contract for a two-input crossfade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioTransitionCurve {
    /// Linear amplitude ramps whose sum is one.
    ConstantGain,
    /// Sin/cos ramps preserving perceived energy for uncorrelated material.
    EqualPower,
}

/// An explicit crossfade between exactly two placement-local audio edits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioTransition {
    /// Stable Transition identity.
    pub id: AudioTransitionId,
    /// First endpoint.
    pub left: AudioComponentEditId,
    /// Second endpoint.
    pub right: AudioComponentEditId,
    /// Sole authoritative Sequence-time Transition interval.
    pub sequence_range: TimelineTimeRange,
    /// Paired Transition curve policy.
    pub curve: AudioTransitionCurve,
}

/// Sequence-owned audio author aggregate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioProgram {
    /// Non-placement Clip processing definitions.
    pub processing_scopes: AuthoringList<AudioProcessingScope>,
    /// Explicit two-input Transitions.
    pub transitions: AuthoringList<AudioTransition>,
    /// Mixer state for every and only audio Track in the owning Sequence.
    /// Editing one channel detaches this map without copying unrelated Sequence
    /// collections or immutable plugin payloads below its channel strips.
    pub track_channels: AuthoringMap<TrackId, AudioTrackMixerChannel>,
    /// User-created intermediate Buses.
    pub buses: AuthoringList<AudioMixBus>,
    /// Stable public outputs.
    pub outputs: AuthoringList<AudioProgramOutput>,
    /// Explicit typed Routes.
    pub routes: AuthoringList<AudioRoute>,
}

impl AuthoringFootprint for AudioProcessorDefinitionRef {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::BuiltIn { definition_id, schema_version: _ } => collector.collect(definition_id),
            Self::Vst3 { class_id, vendor, schema_version: _ } => {
                collector.collect(class_id)?;
                collector.collect(vendor)
            }
            Self::Clap { plugin_id, schema_version: _ } => collector.collect(plugin_id),
        }
    }
}

impl AuthoringFootprint for AudioProcessorParameter {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self { schema, automation } = self;
        collector.collect(schema)?;
        collector.collect(automation)
    }
}

impl AuthoringFootprint for AudioProcessorInstance {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self {
            id: _,
            definition,
            bypassed: _,
            parameters,
            opaque_state,
        } = self;
        collector.collect(definition)?;
        collector.collect(parameters)?;
        collector.collect(opaque_state)
    }
}

impl AuthoringFootprint for AudioProcessorRack {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self { processors } = self;
        collector.collect(processors)
    }
}

impl AuthoringFootprint for AudioChannelStrip {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self {
            input_trim_db: _,
            pre_fader,
            fader_db: _,
            fader_automation,
            post_fader,
        } = self;
        collector.collect(pre_fader)?;
        collector.collect(fader_automation)?;
        collector.collect(post_fader)
    }
}

impl AuthoringFootprint for AudioComponentSource {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::Media { component_id: _ } | Self::NestedOutput { output_id: _ } => Ok(()),
        }
    }
}

impl AuthoringFootprint for AudioComponentChannelMapping {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::Standard => Ok(()),
            Self::Explicit(matrix) => collector.collect(matrix),
        }
    }
}

impl AuthoringFootprint for AudioProcessingBinding {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self { scope_id: _, scope_in: _ } = self;
        Ok(())
    }
}

impl AuthoringFootprint for AudioFadeCurve {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::ConstantGain | Self::EqualPower => Ok(()),
        }
    }
}

impl AuthoringFootprint for AudioFade {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self { duration: _, curve } = self;
        collector.collect(curve)
    }
}

impl AuthoringFootprint for AudioClipFades {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self { fade_in, fade_out } = self;
        collector.collect(fade_in)?;
        collector.collect(fade_out)
    }
}

impl AuthoringFootprint for AudioComponentEdit {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self {
            id: _,
            source,
            channel_mapping,
            role_id: _,
            enabled: _,
            local_time_in: _,
            processing,
            volume_db: _,
            volume_automation,
            pan: _,
            pan_automation,
            fades,
        } = self;
        collector.collect(source)?;
        collector.collect(channel_mapping)?;
        collector.collect(processing)?;
        collector.collect(volume_automation)?;
        collector.collect(pan_automation)?;
        collector.collect(fades)
    }
}

impl AuthoringFootprint for AudioProcessingScope {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self {
            id: _,
            input_gain_db: _,
            input_gain_automation,
            processors,
        } = self;
        collector.collect(input_gain_automation)?;
        collector.collect(processors)
    }
}

impl AuthoringFootprint for AudioTrackMixerChannel {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self { strip } = self;
        collector.collect(strip)
    }
}

impl AuthoringFootprint for AudioMixBus {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self { id: _, name, strip } = self;
        collector.collect(name)?;
        collector.collect(strip)
    }
}

impl AuthoringFootprint for ProgramOutputMainSource {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::RoutedInputs | Self::SemanticProjection { role_id: _ } => Ok(()),
        }
    }
}

impl AuthoringFootprint for AudioProgramOutput {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self { id: _, name, main_source, strip } = self;
        collector.collect(name)?;
        collector.collect(main_source)?;
        collector.collect(strip)
    }
}

impl AuthoringFootprint for AudioRole {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self { id: _, parent_id: _, name, standard_semantic_key } = self;
        collector.collect(name)?;
        collector.collect(standard_semantic_key)
    }
}

impl AuthoringFootprint for AudioChannelStripOutputPort {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::PreFader | Self::PostFaderPreMute | Self::PostMute => Ok(()),
        }
    }
}

impl AuthoringFootprint for AudioRouteSource {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::Track { track_id: _, port: _ } | Self::Bus { bus_id: _, port: _ } => Ok(()),
        }
    }
}

impl AuthoringFootprint for AudioRouteDestination {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::Bus(_) | Self::Output(_) => Ok(()),
        }
    }
}

impl AuthoringFootprint for AudioRoute {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self {
            id: _,
            source,
            destination,
            enabled: _,
            gain_db: _,
            gain_automation,
        } = self;
        collector.collect(source)?;
        collector.collect(destination)?;
        collector.collect(gain_automation)
    }
}

impl AuthoringFootprint for AudioTransitionCurve {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::ConstantGain | Self::EqualPower => Ok(()),
        }
    }
}

impl AuthoringFootprint for AudioTransition {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self { id: _, left: _, right: _, sequence_range: _, curve } = self;
        collector.collect(curve)
    }
}

impl AuthoringFootprint for AudioProgram {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self {
            processing_scopes,
            transitions,
            track_channels,
            buses,
            outputs,
            routes,
        } = self;
        collector.collect(processing_scopes)?;
        collector.collect(transitions)?;
        collector.collect(track_channels)?;
        collector.collect(buses)?;
        collector.collect(outputs)?;
        collector.collect(routes)
    }
}

fn audio_transition_fits_edit_ranges(
    transition: &AudioTransition,
    edit_ranges: &BTreeMap<AudioComponentEditId, TimelineTimeRange>,
) -> bool {
    let (Some(left), Some(right)) = (
        edit_ranges.get(&transition.left),
        edit_ranges.get(&transition.right),
    ) else {
        return false;
    };
    let Ok(transition_end) = transition.sequence_range.end() else {
        return false;
    };
    let Ok(left_end) = left.end() else {
        return false;
    };
    let Ok(right_end) = right.end() else {
        return false;
    };
    transition.sequence_range.start >= left.start
        && transition_end <= left_end
        && transition.sequence_range.start >= right.start
        && transition_end <= right_end
}

impl AudioProgram {
    /// Create a routable program for the supplied audio Tracks.
    pub fn for_tracks(track_ids: impl IntoIterator<Item = TrackId>) -> Self {
        let output = AudioProgramOutput {
            id: ProgramOutputId::new(),
            name: "Main".to_owned(),
            main_source: ProgramOutputMainSource::RoutedInputs,
            strip: AudioChannelStrip::default(),
        };
        let mut track_channels = AuthoringMap::new();
        let mut routes = Vec::new();
        for track_id in track_ids {
            track_channels.insert(track_id, AudioTrackMixerChannel::default());
            routes.push(AudioRoute::new(
                AudioRouteSource::Track {
                    track_id,
                    port: AudioChannelStripOutputPort::PostMute,
                },
                AudioRouteDestination::Output(output.id),
            ));
        }
        Self {
            processing_scopes: AuthoringList::new(),
            transitions: AuthoringList::new(),
            track_channels,
            buses: AuthoringList::new(),
            outputs: AuthoringList::from(vec![output]),
            routes: AuthoringList::from(routes),
        }
    }

    /// Register a processing definition and return its stable identity.
    pub fn add_processing_scope(&mut self, scope: AudioProcessingScope) -> AudioProcessingScopeId {
        let id = scope.id;
        self.processing_scopes.push(scope);
        id
    }

    /// Add the mixer channel and default main-output Route for a new Track.
    pub fn add_track(&mut self, track_id: TrackId) {
        self.track_channels.entry(track_id).or_default();
        if let Some(output) = self.outputs.first() {
            if !self.routes.iter().any(|route| route.source.track_id() == Some(track_id)) {
                self.routes.push(AudioRoute::new(
                    AudioRouteSource::Track {
                        track_id,
                        port: AudioChannelStripOutputPort::PostMute,
                    },
                    AudioRouteDestination::Output(output.id),
                ));
            }
        }
    }

    /// Remove a Track's mixer state and Routes. Clip-local authoring is owned by the removed Track.
    pub fn remove_track(&mut self, track_id: TrackId) {
        self.track_channels.remove(&track_id);
        self.routes.retain(|route| route.source.track_id() != Some(track_id));
    }

    /// Remove Transitions whose strong endpoint no longer exists and discard unused scopes.
    pub fn compact_for_tracks(&mut self, audio_tracks: &[Track]) {
        let edit_ranges = audio_tracks
            .iter()
            .flat_map(|track| &track.clips)
            .flat_map(|clip| {
                clip.audio_components.iter().map(move |edit| {
                    (
                        edit.id,
                        TimelineTimeRange::new(clip.position, clip.duration),
                    )
                })
            })
            .filter_map(|(edit_id, range)| range.ok().map(|range| (edit_id, range)))
            .collect::<BTreeMap<_, _>>();
        let scope_ids = audio_tracks
            .iter()
            .flat_map(|track| &track.clips)
            .flat_map(|clip| &clip.audio_components)
            .map(|edit| edit.processing.scope_id)
            .collect::<BTreeSet<_>>();
        if self
            .transitions
            .iter()
            .any(|transition| !audio_transition_fits_edit_ranges(transition, &edit_ranges))
        {
            self.transitions
                .retain(|transition| audio_transition_fits_edit_ranges(transition, &edit_ranges));
        }
        if self.processing_scopes.iter().any(|scope| !scope_ids.contains(&scope.id)) {
            self.processing_scopes.retain(|scope| scope_ids.contains(&scope.id));
        }
    }

    /// Validate the complete Sequence-local author closure against real Track/Clip placement.
    pub fn validate(
        &self,
        audio_tracks: &[Track],
        audio_roles: &[AudioRole],
        sequence_layout: AudioChannelLayout,
    ) -> Result<(), AudioAuthoringError> {
        let expected_tracks = audio_tracks.iter().map(|track| track.id).collect::<BTreeSet<_>>();
        let actual_tracks = self.track_channels.keys().copied().collect::<BTreeSet<_>>();
        if actual_tracks != expected_tracks {
            return Err(AudioAuthoringError::TrackChannelSetMismatch);
        }

        let bus_ids = unique_ids(self.buses.iter().map(|bus| bus.id))
            .ok_or(AudioAuthoringError::DuplicateBus)?;
        let output_ids = unique_ids(self.outputs.iter().map(|output| output.id))
            .ok_or(AudioAuthoringError::DuplicateOutput)?;
        let scope_ids = unique_ids(self.processing_scopes.iter().map(|scope| scope.id))
            .ok_or(AudioAuthoringError::DuplicateProcessingScope)?;
        let role_ids = unique_ids(audio_roles.iter().map(|role| role.id))
            .ok_or(AudioAuthoringError::DuplicateRole)?;
        if self.outputs.is_empty() {
            return Err(AudioAuthoringError::MissingProgramOutput);
        }

        let mut processor_ids = BTreeSet::new();
        for rack in self
            .processing_scopes
            .iter()
            .map(|scope| &scope.processors)
            .chain(
                self.track_channels
                    .values()
                    .flat_map(|channel| [&channel.strip.pre_fader, &channel.strip.post_fader]),
            )
            .chain(self.buses.iter().flat_map(|bus| [&bus.strip.pre_fader, &bus.strip.post_fader]))
            .chain(
                self.outputs
                    .iter()
                    .flat_map(|output| [&output.strip.pre_fader, &output.strip.post_fader]),
            )
        {
            for processor in &rack.processors {
                if !processor_ids.insert(processor.id) {
                    return Err(AudioAuthoringError::DuplicateProcessorInstance(
                        processor.id,
                    ));
                }
            }
        }

        for scope in &self.processing_scopes {
            scope.validate()?;
        }
        for channel in self.track_channels.values() {
            channel.strip.validate()?;
        }
        for bus in &self.buses {
            if bus.name.trim().is_empty() {
                return Err(AudioAuthoringError::EmptyName);
            }
            bus.strip.validate()?;
        }
        for output in &self.outputs {
            if output.name.trim().is_empty() {
                return Err(AudioAuthoringError::EmptyName);
            }
            output.strip.validate()?;
            if let ProgramOutputMainSource::SemanticProjection { role_id } = output.main_source {
                if !role_ids.contains(&role_id) {
                    return Err(AudioAuthoringError::UnknownOutputRole(role_id));
                }
            }
        }
        validate_roles(audio_roles, &role_ids)?;

        let mut edits = BTreeMap::<AudioComponentEditId, (&Clip, TrackId)>::new();
        for track in audio_tracks {
            for clip in &track.clips {
                if clip.audio_components.is_empty() {
                    return Err(AudioAuthoringError::MissingAudioComponents(clip.id));
                }
                let mut source_ids = BTreeSet::new();
                for edit in &clip.audio_components {
                    if edits.insert(edit.id, (clip, track.id)).is_some() {
                        return Err(AudioAuthoringError::DuplicateComponentEdit);
                    }
                    if !scope_ids.contains(&edit.processing.scope_id) {
                        return Err(AudioAuthoringError::UnknownProcessingScope(edit.id));
                    }
                    if edit.role_id.is_some_and(|role_id| !role_ids.contains(&role_id)) {
                        return Err(AudioAuthoringError::UnknownComponentRole(edit.id));
                    }
                    match edit.source {
                        AudioComponentSource::Media { component_id } => {
                            if clip.is_nested_sequence() || !source_ids.insert(component_id) {
                                return Err(AudioAuthoringError::InvalidComponentSource(edit.id));
                            }
                        }
                        AudioComponentSource::NestedOutput { .. } => {
                            if !clip.is_nested_sequence() {
                                return Err(AudioAuthoringError::InvalidComponentSource(edit.id));
                            }
                        }
                    }
                    edit.validate(clip, sequence_layout)?;
                }
            }
        }

        let route_ids = unique_ids(self.routes.iter().map(|route| route.id))
            .ok_or(AudioAuthoringError::DuplicateRoute)?;
        let _ = route_ids;
        for route in &self.routes {
            route.validate()?;
            if route.source.track_id().is_some_and(|id| !expected_tracks.contains(&id))
                || route.source.bus_id().is_some_and(|id| !bus_ids.contains(&id))
            {
                return Err(AudioAuthoringError::UnknownRouteSource);
            }
            match route.destination {
                AudioRouteDestination::Bus(id) if !bus_ids.contains(&id) => {
                    return Err(AudioAuthoringError::UnknownRouteDestination);
                }
                AudioRouteDestination::Output(id) if !output_ids.contains(&id) => {
                    return Err(AudioAuthoringError::UnknownRouteDestination);
                }
                _ => {}
            }
            if matches!(
                (route.source.bus_id(), route.destination),
                (Some(source), AudioRouteDestination::Bus(destination)) if source == destination
            ) {
                return Err(AudioAuthoringError::RouteCycle);
            }
        }
        validate_bus_cycles(&self.routes, &bus_ids)?;

        let transition_ids = unique_ids(self.transitions.iter().map(|transition| transition.id))
            .ok_or(AudioAuthoringError::DuplicateTransition)?;
        let _ = transition_ids;
        for transition in &self.transitions {
            let Some((left_clip, _)) = edits.get(&transition.left).copied() else {
                return Err(AudioAuthoringError::InvalidTransition(transition.id));
            };
            let Some((right_clip, _)) = edits.get(&transition.right).copied() else {
                return Err(AudioAuthoringError::InvalidTransition(transition.id));
            };
            let transition_end = transition
                .sequence_range
                .end()
                .map_err(|_| AudioAuthoringError::InvalidTransition(transition.id))?;
            let left_end = left_clip
                .end_position()
                .map_err(|_| AudioAuthoringError::InvalidTransition(transition.id))?;
            let right_end = right_clip
                .end_position()
                .map_err(|_| AudioAuthoringError::InvalidTransition(transition.id))?;
            if transition.left == transition.right
                || transition.sequence_range.is_empty()
                || transition.sequence_range.start < left_clip.position
                || transition.sequence_range.start < right_clip.position
                || transition_end > left_end
                || transition_end > right_end
            {
                return Err(AudioAuthoringError::InvalidTransition(transition.id));
            }
        }
        Ok(())
    }
}

fn validate_curve(curve: &ExactAutomationCurve) -> Result<(), AudioAuthoringError> {
    curve
        .validate()
        .map_err(|error| AudioAuthoringError::InvalidAutomation { reason: error.to_string() })
}

fn validate_parameter_value(
    schema: &ParameterSchema,
    value: f64,
) -> Result<(), AudioAuthoringError> {
    if !value.is_finite() {
        return Err(AudioAuthoringError::InvalidProcessorParameterValue);
    }
    if schema.value_type == PropertyValueType::Int
        && (value.fract() != 0.0 || value.abs() > 9_007_199_254_740_992.0)
    {
        return Err(AudioAuthoringError::InvalidProcessorParameterValue);
    }
    if schema
        .numeric
        .is_some_and(|numeric| value < numeric.hard_range.min || value > numeric.hard_range.max)
    {
        return Err(AudioAuthoringError::InvalidProcessorParameterValue);
    }
    Ok(())
}

fn processor_parameter_default_value(value: &PropertyValue) -> Result<f64, AudioAuthoringError> {
    match value {
        PropertyValue::Double(value) => Ok(*value),
        PropertyValue::Int(value) if value.unsigned_abs() <= 9_007_199_254_740_992_u64 => {
            Ok(*value as f64)
        }
        PropertyValue::Int(_) => Err(AudioAuthoringError::InvalidProcessorParameterValue),
        _ => Err(AudioAuthoringError::UnsupportedProcessorParameterType),
    }
}

fn validate_optional_curve(
    curve: &Option<ExactAutomationCurve>,
    expected_parameter: &str,
) -> Result<(), AudioAuthoringError> {
    if let Some(curve) = curve {
        if curve.parameter_id.as_str() != expected_parameter {
            return Err(AudioAuthoringError::WrongAutomationParameter {
                expected: expected_parameter.to_owned(),
            });
        }
        validate_curve(curve)?;
    }
    Ok(())
}

fn validate_optional_gain_curve(
    curve: &Option<ExactAutomationCurve>,
    expected_parameter: &str,
) -> Result<(), AudioAuthoringError> {
    validate_optional_curve(curve, expected_parameter)?;
    let Some(curve) = curve else {
        return Ok(());
    };
    validate_gain_db(curve.default_value)?;
    for keyframe in &curve.keyframes {
        validate_gain_db(keyframe.value)?;
        for handle in [keyframe.in_handle, keyframe.out_handle].into_iter().flatten() {
            validate_gain_db(keyframe.value + handle.value_offset)?;
        }
    }
    Ok(())
}

fn validate_gain_db(value: f64) -> Result<(), AudioAuthoringError> {
    if value.is_finite() && (AUDIO_GAIN_DB_MIN..=AUDIO_GAIN_DB_MAX).contains(&value) {
        Ok(())
    } else {
        Err(AudioAuthoringError::InvalidGain)
    }
}

fn validate_roles(
    roles: &[AudioRole],
    role_ids: &BTreeSet<AudioRoleId>,
) -> Result<(), AudioAuthoringError> {
    for role in roles {
        if role.name.trim().is_empty() {
            return Err(AudioAuthoringError::EmptyName);
        }
        if role.parent_id.is_some_and(|parent| !role_ids.contains(&parent)) {
            return Err(AudioAuthoringError::UnknownRoleParent(role.id));
        }
        let mut seen = BTreeSet::new();
        let mut current = Some(role.id);
        while let Some(id) = current {
            if !seen.insert(id) {
                return Err(AudioAuthoringError::RoleCycle);
            }
            current = roles
                .iter()
                .find(|candidate| candidate.id == id)
                .and_then(|item| item.parent_id);
        }
    }
    Ok(())
}

fn unique_ids<T: Copy + Ord>(values: impl IntoIterator<Item = T>) -> Option<BTreeSet<T>> {
    let mut set = BTreeSet::new();
    for value in values {
        if !set.insert(value) {
            return None;
        }
    }
    Some(set)
}

fn validate_bus_cycles(
    routes: &[AudioRoute],
    bus_ids: &BTreeSet<MixBusId>,
) -> Result<(), AudioAuthoringError> {
    fn visit(
        bus: MixBusId,
        routes: &[AudioRoute],
        visiting: &mut BTreeSet<MixBusId>,
        visited: &mut BTreeSet<MixBusId>,
    ) -> Result<(), AudioAuthoringError> {
        if visited.contains(&bus) {
            return Ok(());
        }
        if !visiting.insert(bus) {
            return Err(AudioAuthoringError::RouteCycle);
        }
        for route in routes {
            if route.source.bus_id() == Some(bus) {
                if let AudioRouteDestination::Bus(next) = route.destination {
                    visit(next, routes, visiting, visited)?;
                }
            }
        }
        visiting.remove(&bus);
        visited.insert(bus);
        Ok(())
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for bus in bus_ids {
        visit(*bus, routes, &mut visiting, &mut visited)?;
    }
    Ok(())
}

/// Invalid author state. Such state cannot enter an immutable execution snapshot.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioAuthoringError {
    /// Track mixer state does not exactly match the Sequence audio Tracks.
    #[error("audio Track mixer channels do not match Sequence audio Tracks")]
    TrackChannelSetMismatch,
    /// Stable parameter map key, schema identity, and curve identity disagree.
    #[error("audio parameter map key, schema identity, and curve identity must match")]
    ParameterKeyMismatch,
    /// A captured processor parameter schema is malformed.
    #[error("invalid audio processor parameter schema: {reason}")]
    InvalidParameterSchema { reason: String },
    /// Processor parameters use doubles or exactly representable integer host values.
    #[error("audio processor parameters require a double or exactly representable integer schema")]
    UnsupportedProcessorParameterType,
    /// A parameter curve was supplied for an identity absent from the definition snapshot.
    #[error("audio processor parameter is not present in the instance definition")]
    UnknownProcessorParameter,
    /// A non-animatable processor parameter contains keyframes.
    #[error("audio processor parameter schema does not admit automation")]
    ParameterDoesNotAdmitAutomation,
    /// A curve uses an interpolation semantic not admitted by its schema.
    #[error("audio processor parameter interpolation is not admitted by its schema")]
    UnsupportedParameterInterpolation,
    /// Default, keyframe, or Bezier control values violate the schema's hard range.
    #[error("audio processor parameter value violates its schema")]
    InvalidProcessorParameterValue,
    /// A persisted automation curve is invalid.
    #[error("invalid audio automation: {reason}")]
    InvalidAutomation { reason: String },
    /// An automation curve used the wrong stable parameter identity.
    #[error("audio automation must use parameter {expected}")]
    WrongAutomationParameter { expected: String },
    /// Static, keyed, and Bezier-control gain values must fit the DSP hard range.
    #[error("audio gain must be finite and lie within the supported dB range")]
    InvalidGain,
    /// Rack contains one processor identity more than once.
    #[error("duplicate audio processor instance {0}")]
    DuplicateProcessorInstance(AudioProcessorInstanceId),
    /// Processing-scope identities must be unique.
    #[error("duplicate audio processing-scope identity")]
    DuplicateProcessingScope,
    /// Audio Clip has no explicit audio component authoring.
    #[error("audio Clip {0} has no audio component edits")]
    MissingAudioComponents(ClipId),
    /// Component-edit identities must be unique in one Sequence.
    #[error("duplicate audio component-edit identity")]
    DuplicateComponentEdit,
    /// Component references an absent processing scope.
    #[error("audio component edit {0} references an unknown processing scope")]
    UnknownProcessingScope(AudioComponentEditId),
    /// Component source is incompatible with its owning Clip.
    #[error("audio component edit {0} has an invalid source for its owning Clip")]
    InvalidComponentSource(AudioComponentEditId),
    /// Explicit Component matrix must terminate in the owning Sequence layout.
    #[error("audio component edit {0} channel mapping does not target the Sequence layout")]
    ChannelMappingDestinationMismatch(AudioComponentEditId),
    /// Component names a Role absent from this Sequence.
    #[error("audio component edit {0} targets an unknown Role")]
    UnknownComponentRole(AudioComponentEditId),
    /// Scope-local origin cannot precede the processing scope origin.
    #[error("audio component edit {0} has a negative processing scope offset")]
    NegativeProcessingScopeIn(AudioComponentEditId),
    /// Pan must be finite and lie in the supported range.
    #[error("audio component edit {0} has an invalid pan value")]
    InvalidPan(AudioComponentEditId),
    /// Present fade duration must be strictly positive and fit the owning Clip.
    #[error("audio component edit {0} has an invalid fade")]
    InvalidFade(AudioComponentEditId),
    /// Role identities must be unique.
    #[error("duplicate audio Role identity")]
    DuplicateRole,
    /// Bus identities must be unique.
    #[error("duplicate audio Bus identity")]
    DuplicateBus,
    /// Output identities must be unique.
    #[error("duplicate audio Program Output identity")]
    DuplicateOutput,
    /// Route identities must be unique.
    #[error("duplicate audio Route identity")]
    DuplicateRoute,
    /// Transition identities must be unique.
    #[error("duplicate audio Transition identity")]
    DuplicateTransition,
    /// At least one explicit public output is required.
    #[error("audio Program has no public output")]
    MissingProgramOutput,
    /// User-facing Role, Bus, and Output labels cannot be blank.
    #[error("audio Role, Bus, or Program Output name cannot be empty")]
    EmptyName,
    /// Output projection names a Role absent from this Sequence.
    #[error("audio Program Output targets unknown Role {0}")]
    UnknownOutputRole(AudioRoleId),
    /// Role parent is not in the same Sequence catalog.
    #[error("audio Role {0} has an unknown parent")]
    UnknownRoleParent(AudioRoleId),
    /// Role hierarchy must be acyclic.
    #[error("audio Role hierarchy contains a cycle")]
    RoleCycle,
    /// Route source does not exist in the author aggregate.
    #[error("audio Route source does not exist")]
    UnknownRouteSource,
    /// Route destination does not exist in the author aggregate.
    #[error("audio Route destination does not exist")]
    UnknownRouteDestination,
    /// Instantaneous authored routing cycles are forbidden.
    #[error("audio routing contains an instantaneous cycle")]
    RouteCycle,
    /// Transition references or interval are invalid.
    #[error("invalid audio Transition {0}")]
    InvalidTransition(AudioTransitionId),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_cow_containers_preserve_json_and_detach_only_on_mutation() {
        let mut processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        processor.definition = AudioProcessorDefinitionRef::Vst3 {
            class_id: "00112233445566778899aabbccddeeff".to_owned(),
            vendor: Some("Test Vendor".to_owned()),
            schema_version: 7,
        };
        processor.opaque_state = Some(AuthoringList::from(vec![1, 2, 3, 4]));
        let parameter_allocation = processor.parameters.allocation_id();
        let opaque_allocation =
            processor.opaque_state.as_ref().expect("opaque state").allocation_id();

        let mut rack = AudioProcessorRack::default();
        rack.processors.push(processor);
        let processor_allocation = rack.processors.allocation_id();

        let json = serde_json::to_vec(&rack).expect("serialize COW audio rack");
        let json_value: serde_json::Value =
            serde_json::from_slice(&json).expect("inspect COW audio JSON");
        assert_eq!(
            json_value["processors"][0]["opaque_state"],
            serde_json::json!([1, 2, 3, 4])
        );
        assert!(json_value["processors"][0]["parameters"].is_object());
        let restored: AudioProcessorRack =
            serde_json::from_slice(&json).expect("deserialize COW audio rack");
        assert_eq!(restored, rack);
        assert_eq!(
            serde_json::to_vec(&restored).expect("reserialize COW audio rack"),
            json
        );

        let mut edited = rack.clone();
        assert!(edited.processors.shares_allocation_with(&rack.processors));
        assert!(edited.processors[0]
            .parameters
            .shares_allocation_with(&rack.processors[0].parameters));
        assert!(edited.processors[0]
            .opaque_state
            .as_ref()
            .expect("edited opaque state")
            .shares_allocation_with(
                rack.processors[0].opaque_state.as_ref().expect("original opaque state")
            ));

        edited.processors[0]
            .parameters
            .get_mut(&ParameterId::new_static(GAIN_DB_PARAMETER_ID))
            .expect("gain parameter")
            .automation
            .default_value = 3.0;
        edited.processors[0].opaque_state.as_mut().expect("edited opaque state").push(5);

        assert_ne!(edited.processors.allocation_id(), processor_allocation);
        assert_ne!(
            edited.processors[0].parameters.allocation_id(),
            parameter_allocation
        );
        assert_ne!(
            edited.processors[0]
                .opaque_state
                .as_ref()
                .expect("edited opaque state")
                .allocation_id(),
            opaque_allocation
        );
        assert_eq!(
            rack.processors[0]
                .parameters
                .get(&ParameterId::new_static(GAIN_DB_PARAMETER_ID))
                .expect("original gain parameter")
                .automation
                .default_value,
            0.0
        );
        assert_eq!(
            rack.processors[0]
                .opaque_state
                .as_deref()
                .expect("original opaque state")
                .as_slice(),
            &[1, 2, 3, 4]
        );
    }

    #[test]
    fn track_channel_map_shares_until_one_channel_is_edited() {
        let tracks = [TrackId::new(), TrackId::new()];
        let original = AudioProgram::for_tracks(tracks);
        let mut edited = original.clone();
        assert!(edited.track_channels.shares_allocation_with(&original.track_channels));

        edited
            .track_channels
            .get_mut(&tracks[0])
            .expect("track channel")
            .strip
            .input_trim_db = -3.0;

        assert!(!edited.track_channels.shares_allocation_with(&original.track_channels));
        assert_eq!(original.track_channels[&tracks[0]].strip.input_trim_db, 0.0);
        assert_eq!(edited.track_channels[&tracks[0]].strip.input_trim_db, -3.0);

        let json = serde_json::to_vec(&edited).expect("serialize COW track-channel map");
        let restored: AudioProgram =
            serde_json::from_slice(&json).expect("deserialize COW track-channel map");
        assert_eq!(restored, edited);
    }

    #[test]
    fn default_program_has_one_channel_and_post_mute_route_per_track() {
        let tracks = [TrackId::new(), TrackId::new()];
        let program = AudioProgram::for_tracks(tracks);
        let authored_tracks = tracks.map(|id| {
            let mut track = Track::new_audio("Audio");
            track.id = id;
            track
        });
        program
            .validate(&authored_tracks, &[], AudioChannelLayout::Stereo)
            .expect("valid program");
        assert_eq!(program.track_channels.len(), 2);
        assert!(program.routes.iter().all(|route| matches!(
            route.source,
            AudioRouteSource::Track { port: AudioChannelStripOutputPort::PostMute, .. }
        )));
    }

    #[test]
    fn built_in_gain_owns_one_stable_schema_and_exact_curve() {
        let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        let parameter_id = ParameterId::new_static(GAIN_DB_PARAMETER_ID);
        let parameter = processor.parameters.get(&parameter_id).expect("gain parameter");

        assert_eq!(processor.parameters.len(), 1);
        assert_eq!(parameter.schema, gain_parameter_schema());
        assert_eq!(parameter.schema.value_type, PropertyValueType::Double);
        assert_eq!(parameter.automation.parameter_id, parameter_id);
        assert_eq!(parameter.automation.default_value, 0.0);
        processor.validate().expect("valid built-in definition snapshot");
    }

    #[test]
    fn built_in_sample_delay_owns_an_exact_non_animatable_integer_contract() {
        let processor = AudioProcessorInstance::built_in(BUILTIN_SAMPLE_DELAY_DEFINITION_ID, 1);
        let parameter_id = ParameterId::new_static(SAMPLE_DELAY_FRAMES_PARAMETER_ID);
        let parameter = processor.parameters.get(&parameter_id).expect("delay parameter");

        assert_eq!(processor.parameters.len(), 1);
        assert_eq!(parameter.schema, sample_delay_frames_parameter_schema());
        assert_eq!(parameter.schema.value_type, PropertyValueType::Int);
        assert_eq!(parameter.schema.unit, ParameterUnit::Samples);
        assert!(!parameter.schema.is_animatable);
        assert_eq!(
            parameter.schema.cache_impact,
            ParameterCacheImpact::Topology
        );
        assert_eq!(parameter.automation.default_value, 0.0);
        assert_eq!(
            AudioProcessorParameter::from_schema(sample_delay_frames_parameter_schema())
                .expect("integer schema bridge"),
            *parameter
        );
        processor.validate().expect("valid built-in definition snapshot");
    }

    #[test]
    fn built_in_lookahead_limiter_owns_versioned_latency_and_signal_parameters() {
        let processor =
            AudioProcessorInstance::built_in(BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID, 1);
        let ceiling_id = ParameterId::new_static(LOOKAHEAD_LIMITER_CEILING_DB_PARAMETER_ID);
        let lookahead_id = ParameterId::new_static(LOOKAHEAD_LIMITER_LOOKAHEAD_MS_PARAMETER_ID);
        let release_id = ParameterId::new_static(LOOKAHEAD_LIMITER_RELEASE_MS_PARAMETER_ID);

        assert_eq!(processor.parameters.len(), 3);
        assert_eq!(
            processor.parameters[&ceiling_id].schema,
            lookahead_limiter_ceiling_parameter_schema()
        );
        assert_eq!(
            processor.parameters[&lookahead_id].schema,
            lookahead_limiter_lookahead_parameter_schema()
        );
        assert_eq!(
            processor.parameters[&release_id].schema,
            lookahead_limiter_release_parameter_schema()
        );
        assert_eq!(
            processor.parameters[&ceiling_id].automation.default_value,
            LOOKAHEAD_LIMITER_DEFAULT_CEILING_DB
        );
        assert_eq!(
            processor.parameters[&lookahead_id].automation.default_value,
            LOOKAHEAD_LIMITER_DEFAULT_LOOKAHEAD_MS
        );
        assert_eq!(
            processor.parameters[&release_id].automation.default_value,
            LOOKAHEAD_LIMITER_DEFAULT_RELEASE_MS
        );
        assert!(!processor.parameters[&lookahead_id].schema.is_animatable);
        assert_eq!(
            processor.parameters[&lookahead_id].schema.cache_impact,
            ParameterCacheImpact::Topology
        );
        assert_eq!(
            processor.parameters[&lookahead_id].schema.unit,
            ParameterUnit::Milliseconds
        );
        processor.validate().expect("valid built-in limiter definition snapshot");
        let encoded = serde_json::to_vec(&processor).expect("serialize limiter");
        let restored: AudioProcessorInstance =
            serde_json::from_slice(&encoded).expect("restore limiter");
        assert_eq!(restored, processor);
    }

    #[test]
    fn sample_delay_rejects_fractional_values_and_live_length_automation() {
        let mut processor = AudioProcessorInstance::built_in(BUILTIN_SAMPLE_DELAY_DEFINITION_ID, 1);
        let parameter_id = ParameterId::new_static(SAMPLE_DELAY_FRAMES_PARAMETER_ID);
        let fractional =
            ExactAutomationCurve::new(parameter_id.clone(), 1.5).expect("finite curve");
        assert_eq!(
            processor.set_parameter_automation(fractional),
            Err(AudioAuthoringError::InvalidProcessorParameterValue)
        );

        let mut keyed = ExactAutomationCurve::new(parameter_id, 2.0).expect("finite curve");
        keyed
            .set_keyframe(mondrian_core::ExactAutomationKeyframe::linear(
                TimelineTime::ZERO,
                2.0,
            ))
            .expect("valid exact key");
        assert_eq!(
            processor.set_parameter_automation(keyed),
            Err(AudioAuthoringError::ParameterDoesNotAdmitAutomation)
        );
    }

    #[test]
    fn processor_parameter_mutation_rejects_unknown_identity_and_hard_range_violations() {
        let mut processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        let unknown =
            ExactAutomationCurve::new(ParameterId::new_static("mondrian.audio.unknown"), 0.0)
                .expect("finite curve");
        assert_eq!(
            processor.set_parameter_automation(unknown),
            Err(AudioAuthoringError::UnknownProcessorParameter)
        );

        let out_of_range =
            ExactAutomationCurve::new(ParameterId::new_static(GAIN_DB_PARAMETER_ID), 25.0)
                .expect("finite curve");
        assert_eq!(
            processor.set_parameter_automation(out_of_range),
            Err(AudioAuthoringError::InvalidProcessorParameterValue)
        );
    }

    #[test]
    fn route_gain_contract_rejects_wrong_identity_and_out_of_range_values() {
        let track = Track::new_audio("Audio");
        let mut program = AudioProgram::for_tracks([track.id]);
        program.routes[0].gain_db = AUDIO_GAIN_DB_MAX + 1.0;
        assert_eq!(
            program.validate(
                std::slice::from_ref(&track),
                &[],
                AudioChannelLayout::Stereo
            ),
            Err(AudioAuthoringError::InvalidGain)
        );

        program.routes[0].gain_db = 0.0;
        program.routes[0].gain_automation = Some(
            ExactAutomationCurve::new(ParameterId::new_static(GAIN_DB_PARAMETER_ID), 0.0)
                .expect("finite curve"),
        );
        assert_eq!(
            program.validate(
                std::slice::from_ref(&track),
                &[],
                AudioChannelLayout::Stereo
            ),
            Err(AudioAuthoringError::WrongAutomationParameter {
                expected: ROUTE_GAIN_DB_PARAMETER_ID.to_owned(),
            })
        );

        let mut curve =
            ExactAutomationCurve::new(ParameterId::new_static(ROUTE_GAIN_DB_PARAMETER_ID), 0.0)
                .expect("curve");
        let mut first = mondrian_core::ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0);
        first.interpolation_to_next = mondrian_core::AutomationSegmentInterpolation::Bezier;
        first.out_handle = Some(mondrian_core::ExactBezierHandle {
            time_offset: TimelineTime::new(1, 4).expect("handle time"),
            value_offset: AUDIO_GAIN_DB_MAX + 1.0,
        });
        let mut last = mondrian_core::ExactAutomationKeyframe::linear(TimelineTime::ONE, 0.0);
        last.in_handle = Some(mondrian_core::ExactBezierHandle {
            time_offset: TimelineTime::new(-1, 4).expect("handle time"),
            value_offset: 0.0,
        });
        curve.set_keyframe(first).expect("first key");
        curve.set_keyframe(last).expect("last key");
        program.routes[0].gain_automation = Some(curve);
        assert_eq!(
            program.validate(&[track], &[], AudioChannelLayout::Stereo),
            Err(AudioAuthoringError::InvalidGain)
        );
    }

    #[test]
    fn processor_parameter_schema_governs_automation_and_interpolation() {
        let parameter_id = ParameterId::new_static(GAIN_DB_PARAMETER_ID);
        let mut processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        let parameter = processor.parameters.get_mut(&parameter_id).expect("gain parameter");
        parameter.schema.allowed_interpolations = vec![ParameterInterpolation::Hold];
        let mut curve = ExactAutomationCurve::new(parameter_id, 0.0).expect("curve");
        curve
            .set_keyframe(mondrian_core::ExactAutomationKeyframe::linear(
                TimelineTime::ZERO,
                0.0,
            ))
            .expect("first key");
        curve
            .set_keyframe(mondrian_core::ExactAutomationKeyframe::linear(
                TimelineTime::ONE,
                6.0,
            ))
            .expect("second key");
        parameter.automation = curve;

        assert_eq!(
            processor.validate(),
            Err(AudioAuthoringError::UnsupportedParameterInterpolation)
        );
    }

    #[test]
    fn processor_identity_is_unique_across_all_author_insertion_points() {
        let track = Track::new_audio("Audio");
        let mut program = AudioProgram::for_tracks([track.id]);
        let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        let processor_id = processor.id;
        let channel = program.track_channels.get_mut(&track.id).expect("channel");
        channel.strip.pre_fader.processors.push(processor.clone());
        channel.strip.post_fader.processors.push(processor);

        assert_eq!(
            program.validate(&[track], &[], AudioChannelLayout::Stereo),
            Err(AudioAuthoringError::DuplicateProcessorInstance(
                processor_id
            ))
        );
    }

    #[test]
    fn present_clip_fade_must_have_a_positive_duration() {
        let mut track = Track::new_audio("Audio");
        let scope = AudioProcessingScope::identity();
        let mut clip = Clip::new(
            mondrian_core::AssetId::new(),
            TimelineTime::ZERO,
            TimelineTime::ONE,
        )
        .expect("clip");
        let mut edit = AudioComponentEdit::media(AudioSourceComponentId::primary(), scope.id);
        let edit_id = edit.id;
        edit.fades.fade_in = Some(AudioFade {
            duration: TimelineTime::ZERO,
            curve: AudioFadeCurve::EqualPower,
        });
        clip.audio_components.push(edit);
        track.add_clip(clip).expect("add clip");
        let mut program = AudioProgram::for_tracks([track.id]);
        program.add_processing_scope(scope);

        assert_eq!(
            program.validate(&[track], &[], AudioChannelLayout::Stereo),
            Err(AudioAuthoringError::InvalidFade(edit_id))
        );
    }

    #[test]
    fn nonexistent_or_unscoped_clip_audio_cannot_validate() {
        let mut track = Track::new_audio("Audio");
        let clip = Clip::new(
            mondrian_core::AssetId::new(),
            TimelineTime::ZERO,
            TimelineTime::new(1, 1).expect("duration"),
        )
        .expect("clip");
        let clip_id = clip.id;
        track.add_clip(clip).expect("add clip");
        let program = AudioProgram::for_tracks([track.id]);
        assert_eq!(
            program.validate(&[track], &[], AudioChannelLayout::Stereo),
            Err(AudioAuthoringError::MissingAudioComponents(clip_id))
        );
    }

    #[test]
    fn component_track_and_range_are_derived_from_owning_clip() {
        let mut track = Track::new_audio("Audio");
        let scope = AudioProcessingScope::identity();
        let mut clip = Clip::new(
            mondrian_core::AssetId::new(),
            TimelineTime::new(3, 1).expect("position"),
            TimelineTime::new(2, 1).expect("duration"),
        )
        .expect("clip");
        clip.audio_components.push(AudioComponentEdit::media(
            AudioSourceComponentId::primary(),
            scope.id,
        ));
        track.add_clip(clip).expect("add clip");
        let mut program = AudioProgram::for_tracks([track.id]);
        program.add_processing_scope(scope);
        program
            .validate(&[track], &[], AudioChannelLayout::Stereo)
            .expect("closed authoring");
    }

    #[test]
    fn compact_for_tracks_preserves_valid_transition_and_scope_allocations() {
        let mut track = Track::new_audio("Audio");
        let scope = AudioProcessingScope::identity();
        let mut left = Clip::new(
            mondrian_core::AssetId::new(),
            TimelineTime::ZERO,
            TimelineTime::new(10, 1).expect("left duration"),
        )
        .expect("left Clip");
        left.audio_components.push(AudioComponentEdit::media(
            AudioSourceComponentId::primary(),
            scope.id,
        ));
        let left_edit_id = left.audio_components[0].id;
        let mut right = Clip::new(
            mondrian_core::AssetId::new(),
            TimelineTime::new(8, 1).expect("right position"),
            TimelineTime::new(10, 1).expect("right duration"),
        )
        .expect("right Clip");
        right.audio_components.push(AudioComponentEdit::media(
            AudioSourceComponentId::primary(),
            scope.id,
        ));
        let right_edit_id = right.audio_components[0].id;
        track.add_clip(left).expect("left placement");
        track.add_clip(right).expect("right placement");

        let mut original = AudioProgram::for_tracks([track.id]);
        original.add_processing_scope(scope);
        original.transitions.push(AudioTransition {
            id: AudioTransitionId::new(),
            left: left_edit_id,
            right: right_edit_id,
            sequence_range: TimelineTimeRange::new(
                TimelineTime::new(8, 1).expect("Transition start"),
                TimelineTime::new(2, 1).expect("Transition duration"),
            )
            .expect("Transition range"),
            curve: AudioTransitionCurve::EqualPower,
        });
        original
            .validate(&[track.clone()], &[], AudioChannelLayout::Stereo)
            .expect("valid source Audio Program");

        let mut compacted = original.clone();
        compacted.compact_for_tracks(&[track]);

        assert!(compacted.transitions.shares_allocation_with(&original.transitions));
        assert!(compacted.processing_scopes.shares_allocation_with(&original.processing_scopes));
    }

    #[test]
    fn explicit_component_matrix_must_target_the_owning_sequence_layout() {
        let mut track = Track::new_audio("Audio");
        let scope = AudioProcessingScope::identity();
        let mut clip = Clip::new(
            mondrian_core::AssetId::new(),
            TimelineTime::ZERO,
            TimelineTime::ONE,
        )
        .expect("clip");
        let mut edit = AudioComponentEdit::media(AudioSourceComponentId::primary(), scope.id);
        let edit_id = edit.id;
        edit.channel_mapping = AudioComponentChannelMapping::Explicit(
            AudioChannelMixMatrix::identity(AudioChannelLayout::Mono),
        );
        clip.audio_components.push(edit);
        track.add_clip(clip).expect("add clip");
        let mut program = AudioProgram::for_tracks([track.id]);
        program.add_processing_scope(scope);

        assert_eq!(
            program.validate(&[track], &[], AudioChannelLayout::Stereo),
            Err(AudioAuthoringError::ChannelMappingDestinationMismatch(
                edit_id
            ))
        );
    }

    #[test]
    fn instantaneous_bus_cycle_is_rejected() {
        let track = Track::new_audio("Audio");
        let mut program = AudioProgram::for_tracks([track.id]);
        let first = MixBusId::new();
        let second = MixBusId::new();
        program.buses.extend([
            AudioMixBus {
                id: first,
                name: "First".to_owned(),
                strip: AudioChannelStrip::default(),
            },
            AudioMixBus {
                id: second,
                name: "Second".to_owned(),
                strip: AudioChannelStrip::default(),
            },
        ]);
        program.routes.extend([
            AudioRoute::new(
                AudioRouteSource::Bus {
                    bus_id: first,
                    port: AudioChannelStripOutputPort::PostMute,
                },
                AudioRouteDestination::Bus(second),
            ),
            AudioRoute::new(
                AudioRouteSource::Bus {
                    bus_id: second,
                    port: AudioChannelStripOutputPort::PostMute,
                },
                AudioRouteDestination::Bus(first),
            ),
        ]);
        assert_eq!(
            program.validate(&[track], &[], AudioChannelLayout::Stereo),
            Err(AudioAuthoringError::RouteCycle)
        );
    }
}
