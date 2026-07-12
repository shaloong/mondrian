//! Persistent audio authoring owned by a [`Sequence`](crate::Sequence).
//!
//! This module records user intent only. Runtime plugin objects, scheduler
//! state, device handles, decoded PCM, and compiler-generated nodes belong in
//! the audio execution layer.

use mondrian_core::{
    AudioContributionId, AudioProcessorInstanceId, AudioRoleId, AudioRouteId,
    AudioSourceComponentId, AudioTransitionId, ClipId, ExactAutomationCurve, MixBusId, ParameterId,
    ProgramOutputId, SequenceId, TimelineTimeRange, TrackId,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Stable built-in definition identity for a gain processor.
pub const BUILTIN_GAIN_DEFINITION_ID: &str = "mondrian.audio.gain";
/// Stable parameter identity for gain in decibels.
pub const GAIN_DB_PARAMETER_ID: &str = "mondrian.audio.gain.db";
/// Stable parameter identity for a channel-strip fader in decibels.
pub const FADER_DB_PARAMETER_ID: &str = "mondrian.audio.fader.db";

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

/// One persistent processor instance used at every supported insertion point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioProcessorInstance {
    /// Stable identity of this authored instance and its automation.
    pub id: AudioProcessorInstanceId,
    /// Stable processor definition reference.
    pub definition: AudioProcessorDefinitionRef,
    /// Explicit user bypass state.
    pub bypassed: bool,
    /// Parameter curves keyed by the same stable identity stored in each curve.
    pub parameters: BTreeMap<ParameterId, ExactAutomationCurve>,
    /// Opaque, versioned plugin state preserved even when the dependency is unavailable.
    pub opaque_state: Option<Vec<u8>>,
}

impl AudioProcessorInstance {
    /// Create an empty built-in processor instance.
    pub fn built_in(definition_id: impl Into<String>, schema_version: u32) -> Self {
        Self {
            id: AudioProcessorInstanceId::new(),
            definition: AudioProcessorDefinitionRef::BuiltIn {
                definition_id: definition_id.into(),
                schema_version,
            },
            bypassed: false,
            parameters: BTreeMap::new(),
            opaque_state: None,
        }
    }

    /// Validate persistent parameter identity and curves.
    pub fn validate(&self) -> Result<(), AudioAuthoringError> {
        for (parameter_id, curve) in &self.parameters {
            if parameter_id != &curve.parameter_id {
                return Err(AudioAuthoringError::ParameterKeyMismatch);
            }
            curve.validate().map_err(|error| AudioAuthoringError::InvalidAutomation {
                reason: error.to_string(),
            })?;
        }
        Ok(())
    }
}

/// An ordered processor chain. Order is author intent, not a UI presentation detail.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioProcessorRack {
    /// Processors in signal-flow order.
    pub processors: Vec<AudioProcessorInstance>,
}

impl AudioProcessorRack {
    /// Validate every instance and reject duplicate instance identities.
    pub fn validate(&self) -> Result<(), AudioAuthoringError> {
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
    /// Validate numeric values, racks, and the fader parameter contract.
    pub fn validate(&self) -> Result<(), AudioAuthoringError> {
        if !self.input_trim_db.is_finite() || !self.fader_db.is_finite() {
            return Err(AudioAuthoringError::NonFiniteGain);
        }
        self.pre_fader.validate()?;
        self.post_fader.validate()?;
        if let Some(curve) = &self.fader_automation {
            if curve.parameter_id.as_str() != FADER_DB_PARAMETER_ID {
                return Err(AudioAuthoringError::WrongFaderParameter);
            }
            curve.validate().map_err(|error| AudioAuthoringError::InvalidAutomation {
                reason: error.to_string(),
            })?;
        }
        Ok(())
    }
}

/// The stable origin of one independently processable PCM contribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioContributionSource {
    /// One stable audio component exposed by an audiovisual Clip.
    ClipComponent {
        clip_id: ClipId,
        component_id: AudioSourceComponentId,
    },
    /// One public output of a nested Sequence instance.
    NestedOutput {
        sequence_id: SequenceId,
        output_id: ProgramOutputId,
    },
}

/// One independently processable PCM-bearing timeline component.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioContribution {
    /// Stable contribution identity.
    pub id: AudioContributionId,
    /// Media component or nested output that supplies PCM.
    pub source: AudioContributionSource,
    /// Editorial audio Track receiving this contribution.
    pub track_id: TrackId,
    /// Optional Sequence-owned semantic Role assignment.
    pub role_id: Option<AudioRoleId>,
    /// Sequence-time audible interval; source mapping remains owned by the Clip/nesting transform.
    pub sequence_range: TimelineTimeRange,
    /// Contribution-local ordered processor rack.
    pub processors: AudioProcessorRack,
    /// Static contribution gain in dB.
    pub gain_db: f64,
    /// Optional contribution-local gain automation.
    pub gain_automation: Option<ExactAutomationCurve>,
}

impl AudioContribution {
    /// Validate the contribution without resolving external dependencies.
    pub fn validate(&self) -> Result<(), AudioAuthoringError> {
        if self.sequence_range.is_empty() {
            return Err(AudioAuthoringError::EmptyContributionRange(self.id));
        }
        if !self.gain_db.is_finite() {
            return Err(AudioAuthoringError::NonFiniteGain);
        }
        self.processors.validate()?;
        if let Some(curve) = &self.gain_automation {
            if curve.parameter_id.as_str() != GAIN_DB_PARAMETER_ID {
                return Err(AudioAuthoringError::WrongGainParameter);
            }
            curve.validate().map_err(|error| AudioAuthoringError::InvalidAutomation {
                reason: error.to_string(),
            })?;
        }
        Ok(())
    }
}

/// Track mixer state keyed by the editorial Track identity.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioTrackMixerChannel {
    /// Common channel-strip processing.
    pub strip: AudioChannelStrip,
}

/// A user-created mix bus with an independent lifetime.
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
    /// Sum only explicit routes targeting this output.
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
    /// Output-local processing before publication.
    pub strip: AudioChannelStrip,
}

/// Sequence-owned semantic audio Role. Project templates may suggest keys but
/// cannot own or silently rebind this identity.
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

/// Typed route source; compiler-generated operations are never routable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AudioRouteSource {
    /// Output of one Track mixer channel.
    Track(TrackId),
    /// Output of one authored Mix Bus.
    Bus(MixBusId),
}

/// Typed route destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AudioRouteDestination {
    /// Main input of one Mix Bus.
    Bus(MixBusId),
    /// Main input of one Program Output.
    Output(ProgramOutputId),
}

/// Explicit signal route. Sends and sidechains will extend the tap/port contract,
/// not masquerade as main-input routes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioRoute {
    /// Stable route identity.
    pub id: AudioRouteId,
    /// Typed author source.
    pub source: AudioRouteSource,
    /// Typed author destination.
    pub destination: AudioRouteDestination,
}

/// Transition curve contract for a two-input crossfade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioTransitionCurve {
    /// Linear amplitude ramps whose sum is one.
    ConstantGain,
    /// Sin/cos ramps preserving perceived energy for uncorrelated material.
    EqualPower,
}

/// An explicit crossfade between exactly two contributions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioTransition {
    /// Stable transition identity.
    pub id: AudioTransitionId,
    /// First contribution.
    pub left: AudioContributionId,
    /// Second contribution.
    pub right: AudioContributionId,
    /// Exact Sequence-time transition interval.
    pub sequence_range: TimelineTimeRange,
    /// Paired transition curve policy.
    pub curve: AudioTransitionCurve,
}

/// Sequence-owned audio author aggregate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioProgram {
    /// Independently processable timeline contributions.
    pub contributions: Vec<AudioContribution>,
    /// Explicit two-input transitions.
    pub transitions: Vec<AudioTransition>,
    /// Mixer state for every and only audio Track in the owning Sequence.
    pub track_channels: BTreeMap<TrackId, AudioTrackMixerChannel>,
    /// User-created intermediate buses.
    pub buses: Vec<AudioMixBus>,
    /// Stable public outputs.
    pub outputs: Vec<AudioProgramOutput>,
    /// Explicit typed routes.
    pub routes: Vec<AudioRoute>,
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
        let mut track_channels = BTreeMap::new();
        let mut routes = Vec::new();
        for track_id in track_ids {
            track_channels.insert(track_id, AudioTrackMixerChannel::default());
            routes.push(AudioRoute {
                id: AudioRouteId::new(),
                source: AudioRouteSource::Track(track_id),
                destination: AudioRouteDestination::Output(output.id),
            });
        }
        Self {
            contributions: Vec::new(),
            transitions: Vec::new(),
            track_channels,
            buses: Vec::new(),
            outputs: vec![output],
            routes,
        }
    }

    /// Add the mixer channel and default main-output route for a new Track.
    pub fn add_track(&mut self, track_id: TrackId) {
        self.track_channels.entry(track_id).or_default();
        if let Some(output) = self.outputs.first() {
            self.routes.push(AudioRoute {
                id: AudioRouteId::new(),
                source: AudioRouteSource::Track(track_id),
                destination: AudioRouteDestination::Output(output.id),
            });
        }
    }

    /// Remove a Track's mixer state, routes, and contributions.
    pub fn remove_track(&mut self, track_id: TrackId) {
        self.track_channels.remove(&track_id);
        self.contributions.retain(|contribution| contribution.track_id != track_id);
        self.routes.retain(|route| route.source != AudioRouteSource::Track(track_id));
    }

    /// Validate closed references, unique identities, legal route endpoints, and cycles.
    pub fn validate(
        &self,
        audio_track_ids: &[TrackId],
        audio_roles: &[AudioRole],
    ) -> Result<(), AudioAuthoringError> {
        let expected_tracks = audio_track_ids.iter().copied().collect::<BTreeSet<_>>();
        let actual_tracks = self.track_channels.keys().copied().collect::<BTreeSet<_>>();
        if actual_tracks != expected_tracks {
            return Err(AudioAuthoringError::TrackChannelSetMismatch);
        }

        let bus_ids = unique_ids(self.buses.iter().map(|bus| bus.id))
            .ok_or(AudioAuthoringError::DuplicateBus)?;
        let output_ids = unique_ids(self.outputs.iter().map(|output| output.id))
            .ok_or(AudioAuthoringError::DuplicateOutput)?;
        let contribution_ids = unique_ids(self.contributions.iter().map(|item| item.id))
            .ok_or(AudioAuthoringError::DuplicateContribution)?;
        let role_ids = unique_ids(audio_roles.iter().map(|role| role.id))
            .ok_or(AudioAuthoringError::DuplicateRole)?;
        if self.outputs.is_empty() {
            return Err(AudioAuthoringError::MissingProgramOutput);
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
        for contribution in &self.contributions {
            contribution.validate()?;
            if !expected_tracks.contains(&contribution.track_id) {
                return Err(AudioAuthoringError::UnknownContributionTrack(
                    contribution.track_id,
                ));
            }
            if contribution.role_id.is_some_and(|role_id| !role_ids.contains(&role_id)) {
                return Err(AudioAuthoringError::UnknownContributionRole(
                    contribution.id,
                ));
            }
        }

        let route_ids = unique_ids(self.routes.iter().map(|route| route.id))
            .ok_or(AudioAuthoringError::DuplicateRoute)?;
        let _ = route_ids;
        for route in &self.routes {
            match route.source {
                AudioRouteSource::Track(id) if !expected_tracks.contains(&id) => {
                    return Err(AudioAuthoringError::UnknownRouteSource)
                }
                AudioRouteSource::Bus(id) if !bus_ids.contains(&id) => {
                    return Err(AudioAuthoringError::UnknownRouteSource)
                }
                _ => {}
            }
            match route.destination {
                AudioRouteDestination::Bus(id) if !bus_ids.contains(&id) => {
                    return Err(AudioAuthoringError::UnknownRouteDestination)
                }
                AudioRouteDestination::Output(id) if !output_ids.contains(&id) => {
                    return Err(AudioAuthoringError::UnknownRouteDestination)
                }
                _ => {}
            }
            if matches!(
                (route.source, route.destination),
                (AudioRouteSource::Bus(source), AudioRouteDestination::Bus(destination)) if source == destination
            ) {
                return Err(AudioAuthoringError::RouteCycle);
            }
        }
        validate_bus_cycles(&self.routes, &bus_ids)?;

        let mut transition_ids = BTreeSet::new();
        for transition in &self.transitions {
            if !transition_ids.insert(transition.id) {
                return Err(AudioAuthoringError::DuplicateTransition);
            }
            if transition.left == transition.right
                || !contribution_ids.contains(&transition.left)
                || !contribution_ids.contains(&transition.right)
                || transition.sequence_range.is_empty()
            {
                return Err(AudioAuthoringError::InvalidTransition(transition.id));
            }
        }
        Ok(())
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
            if route.source == AudioRouteSource::Bus(bus) {
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

/// Invalid author state. Such state must not enter an immutable execution snapshot.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioAuthoringError {
    /// Track mixer state does not exactly match the Sequence audio Tracks.
    #[error("audio Track mixer channels do not match Sequence audio Tracks")]
    TrackChannelSetMismatch,
    /// Stable parameter map key and curve identity disagree.
    #[error("audio parameter map key does not match curve parameter identity")]
    ParameterKeyMismatch,
    /// A persisted automation curve is invalid.
    #[error("invalid audio automation: {reason}")]
    InvalidAutomation { reason: String },
    /// Static gain values must be finite.
    #[error("audio gain must be finite")]
    NonFiniteGain,
    /// Channel fader automation used a different parameter identity.
    #[error("channel fader automation must use {FADER_DB_PARAMETER_ID}")]
    WrongFaderParameter,
    /// Contribution gain automation used a different parameter identity.
    #[error("contribution gain automation must use {GAIN_DB_PARAMETER_ID}")]
    WrongGainParameter,
    /// Rack contains one processor identity more than once.
    #[error("duplicate audio processor instance {0}")]
    DuplicateProcessorInstance(AudioProcessorInstanceId),
    /// Contribution interval is empty.
    #[error("audio contribution {0} has an empty interval")]
    EmptyContributionRange(AudioContributionId),
    /// Contribution identities must be unique.
    #[error("duplicate audio contribution identity")]
    DuplicateContribution,
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
    /// User-facing Bus and Output labels cannot be blank.
    #[error("audio Bus or Program Output name cannot be empty")]
    EmptyName,
    /// Contribution targets an absent audio Track.
    #[error("audio contribution targets unknown Track {0}")]
    UnknownContributionTrack(TrackId),
    /// Contribution names a Role absent from this Sequence.
    #[error("audio contribution {0} targets an unknown Role")]
    UnknownContributionRole(AudioContributionId),
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
    fn default_program_has_one_channel_and_route_per_track() {
        let tracks = [TrackId::new(), TrackId::new()];
        let program = AudioProgram::for_tracks(tracks);
        program.validate(&tracks, &[]).expect("valid program");
        assert_eq!(program.track_channels.len(), 2);
        assert_eq!(program.routes.len(), 2);
        assert_eq!(program.outputs.len(), 1);
    }

    #[test]
    fn instantaneous_bus_cycle_is_rejected() {
        let track = TrackId::new();
        let mut program = AudioProgram::for_tracks([track]);
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
            AudioRoute {
                id: AudioRouteId::new(),
                source: AudioRouteSource::Bus(first),
                destination: AudioRouteDestination::Bus(second),
            },
            AudioRoute {
                id: AudioRouteId::new(),
                source: AudioRouteSource::Bus(second),
                destination: AudioRouteDestination::Bus(first),
            },
        ]);
        assert_eq!(
            program.validate(&[track], &[]),
            Err(AudioAuthoringError::RouteCycle)
        );
    }

    #[test]
    fn role_hierarchy_must_be_closed_and_acyclic() {
        let track = TrackId::new();
        let program = AudioProgram::for_tracks([track]);
        let first = AudioRoleId::new();
        let second = AudioRoleId::new();
        let roles = [
            AudioRole {
                id: first,
                parent_id: Some(second),
                name: "Dialog".to_owned(),
                standard_semantic_key: Some("dialog".to_owned()),
            },
            AudioRole {
                id: second,
                parent_id: Some(first),
                name: "Principal".to_owned(),
                standard_semantic_key: None,
            },
        ];
        assert_eq!(
            program.validate(&[track], &roles),
            Err(AudioAuthoringError::RoleCycle)
        );
    }
}
