//! Atomic authoring for the Sequence-owned Audio Routing graph.
//!
//! A single typed Route graph represents both principal paths and parallel
//! sends. This Module owns Bus/Route identity, strong-reference admission,
//! Track-lock policy, exact Route-gain automation, graph validation, and
//! transactional publication so callers never patch routing collections.

use std::collections::{HashMap, HashSet};

use crate::audio::{
    AudioAuthoringError, AudioChannelStrip, AudioMixBus, AudioRoute, AudioRouteDestination,
    AudioRouteSource,
};
use crate::sequence::Sequence;
use mondrian_core::{AudioRouteId, MixBusId, ProgramOutputId, TrackId};
use serde::{Deserialize, Serialize};

/// Explicit dependency policy for deleting one Mix Bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioBusRemovalPolicy {
    /// Reject while any incoming or outgoing Route still names the Bus.
    RejectIfConnected,
    /// Atomically delete the Bus and every Route strongly connected to it.
    Disconnect,
}

/// One fine-grained mutation of the Sequence Audio Routing graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum AudioRoutingEdit {
    /// Create an identity Channel Strip Bus and optionally route it onward.
    CreateBus {
        /// User-facing name, trimmed and required to be non-empty.
        name: String,
        /// Optional initial Bus-output destination in the same Sequence.
        route_to: Option<AudioRouteDestination>,
    },
    /// Rename one existing Bus without changing its stable identity.
    RenameBus {
        /// Stable Bus identity.
        bus_id: MixBusId,
        /// User-facing name, trimmed and required to be non-empty.
        name: String,
    },
    /// Remove one Bus under an explicit strong-reference policy.
    RemoveBus {
        /// Stable Bus identity.
        bus_id: MixBusId,
        /// Whether connected Routes reject or participate in the deletion.
        policy: AudioBusRemovalPolicy,
    },
    /// Add one enabled unity-gain principal Route or parallel send.
    CreateRoute {
        /// Stable Track/Bus output port.
        source: AudioRouteSource,
        /// Stable Bus/Program Output input.
        destination: AudioRouteDestination,
    },
    /// Delete one Route by stable identity.
    RemoveRoute {
        /// Stable Route identity.
        route_id: AudioRouteId,
    },
    /// Atomically rewire one Route while preserving its identity and controls.
    SetRouteEndpoints {
        /// Stable Route identity.
        route_id: AudioRouteId,
        /// Replacement Track/Bus output port.
        source: AudioRouteSource,
        /// Replacement Bus/Program Output input.
        destination: AudioRouteDestination,
    },
    /// Enable or retain one authored Route edge.
    SetRouteEnabled {
        /// Stable Route identity.
        route_id: AudioRouteId,
        /// Whether the Route contributes to compiled Signal Closures.
        enabled: bool,
    },
    /// Replace the Route's static gain while no automation is authoritative.
    SetRouteGainDb {
        /// Stable Route identity.
        route_id: AudioRouteId,
        /// Decibel value validated by the complete Audio Program contract.
        value: f64,
    },
}

/// Complete Routing edit intent for one author transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioRoutingEditRequest {
    /// Exact graph mutation.
    pub edit: AudioRoutingEdit,
}

/// Receipt from one successful Routing edit attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioRoutingEditOutcome {
    /// Whether canonical author state changed.
    pub changed: bool,
    /// Stable Bus identity allocated by `CreateBus`.
    pub created_bus_id: Option<MixBusId>,
    /// Stable Route identity allocated by `CreateBus` or `CreateRoute`.
    pub created_route_id: Option<AudioRouteId>,
}

impl AudioRoutingEditOutcome {
    const fn unchanged() -> Self {
        Self {
            changed: false,
            created_bus_id: None,
            created_route_id: None,
        }
    }

    const fn changed() -> Self {
        Self {
            changed: true,
            created_bus_id: None,
            created_route_id: None,
        }
    }
}

/// Fail-closed address error for one Routing author operation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioRoutingAddressError {
    /// Track source is absent from the Sequence audio Timeline or mixer map.
    #[error("Audio Route references an unknown audio Track: {0}")]
    UnknownTrack(TrackId),
    /// Mix Bus is absent from the Sequence Audio Program.
    #[error("Audio Routing references an unknown Mix Bus: {0}")]
    UnknownBus(MixBusId),
    /// Program Output is absent from the Sequence Audio Program.
    #[error("Audio Route references an unknown Program Output: {0}")]
    UnknownProgramOutput(ProgramOutputId),
    /// Route identity is absent from the Sequence Audio Program.
    #[error("Audio Routing references an unknown Route: {0}")]
    UnknownRoute(AudioRouteId),
}

/// Temporary author-state condition blocking an otherwise valid Routing edit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioRoutingEditBlocker {
    /// A persistent Track lock protects every Route sourced from that Track.
    #[error("Audio Routing cannot modify a Route sourced from locked Track: {0}")]
    LockedTrack(TrackId),
}

/// Fail-closed Routing address, admission, or mutation error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioRoutingEditError {
    /// Stable author identity could not be resolved in this Sequence snapshot.
    #[error(transparent)]
    Address(#[from] AudioRoutingAddressError),
    /// Valid author state is currently protected by Track-lock policy.
    #[error(transparent)]
    Blocked(#[from] AudioRoutingEditBlocker),
    /// Bus names are canonical non-empty trimmed strings.
    #[error("Audio Mix Bus name cannot be empty")]
    InvalidBusName,
    /// Reject-if-connected deletion found strong incoming or outgoing Routes.
    #[error("Audio Mix Bus {bus_id} remains connected by {route_count} Route(s)")]
    BusConnected {
        /// Bus whose deletion was rejected.
        bus_id: MixBusId,
        /// Number of connected Routes in the admitted snapshot.
        route_count: usize,
    },
    /// A static Route-gain edit would change a value that is not signal authority.
    #[error("Audio Route gain automation is active; edit the curve or clear it explicitly")]
    RouteGainAutomationActive,
    /// Complete resulting Audio Program is invalid.
    #[error("Audio Routing edit produced invalid author state: {0}")]
    AuthorState(AudioAuthoringError),
}

/// Read-only projection of one Route and its Track-lock admission.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioRouteInspection<'a> {
    route: &'a AudioRoute,
    edit_blocker: Option<AudioRoutingEditBlocker>,
}

impl<'a> AudioRouteInspection<'a> {
    /// Resolved canonical Route.
    pub fn route(&self) -> &'a AudioRoute {
        self.route
    }

    /// Authoritative reason this valid Route cannot currently be edited.
    pub fn edit_blocker(&self) -> Option<&AudioRoutingEditBlocker> {
        self.edit_blocker.as_ref()
    }

    /// Whether mutations of this Route may enter an author transaction.
    pub fn is_editable(&self) -> bool {
        self.edit_blocker.is_none()
    }
}

/// Read-only projection of one Bus and its strong Route dependencies.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioMixBusInspection<'a> {
    bus: &'a AudioMixBus,
    connected_route_count: usize,
    disconnect_blocker: Option<AudioRoutingEditBlocker>,
}

/// Snapshot-local bulk admission projection for adding Route edges.
///
/// The inspection validates every existing endpoint once and precomputes Bus
/// reachability across enabled and disabled Routes. Callers can then build
/// large create/rewire menus without reimplementing lock, address, or cycle
/// policy and without cloning candidate Sequences per option.
/// Preparation is `O(B * (B + R))` time and `O(B²)` worst-case reachability
/// storage for `B` Buses and `R` Routes; each addition query is expected `O(1)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioRouteCandidateInspection {
    editable_tracks: HashSet<TrackId>,
    buses: HashSet<MixBusId>,
    outputs: HashSet<ProgramOutputId>,
    bus_reachability: HashMap<MixBusId, HashSet<MixBusId>>,
}

impl AudioRouteCandidateInspection {
    /// Whether adding this exact edge to the inspected graph passes address,
    /// Track-lock, self-edge, and instantaneous-cycle admission.
    ///
    /// Complete Audio Program validation remains authoritative when the edit
    /// transaction commits.
    pub fn allows_addition(
        &self,
        source: AudioRouteSource,
        destination: AudioRouteDestination,
    ) -> bool {
        let source_exists_and_is_editable = match source {
            AudioRouteSource::Track { track_id, .. } => self.editable_tracks.contains(&track_id),
            AudioRouteSource::Bus { bus_id, .. } => self.buses.contains(&bus_id),
        };
        if !source_exists_and_is_editable {
            return false;
        }
        match (source, destination) {
            (
                AudioRouteSource::Bus { bus_id: source, .. },
                AudioRouteDestination::Bus(destination),
            ) => {
                source != destination
                    && self.buses.contains(&destination)
                    && self
                        .bus_reachability
                        .get(&destination)
                        .is_some_and(|reachable| !reachable.contains(&source))
            }
            (AudioRouteSource::Track { .. }, AudioRouteDestination::Bus(bus_id)) => {
                self.buses.contains(&bus_id)
            }
            (_, AudioRouteDestination::Output(output_id)) => self.outputs.contains(&output_id),
        }
    }
}

impl<'a> AudioMixBusInspection<'a> {
    /// Resolved canonical Mix Bus.
    pub fn bus(&self) -> &'a AudioMixBus {
        self.bus
    }

    /// Number of incoming and outgoing Routes strongly naming this Bus.
    pub fn connected_route_count(&self) -> usize {
        self.connected_route_count
    }

    /// Track-lock blocker for disconnect-and-delete, if any.
    pub fn disconnect_blocker(&self) -> Option<&AudioRoutingEditBlocker> {
        self.disconnect_blocker.as_ref()
    }

    /// Whether disconnect-and-delete can enter an author transaction.
    pub fn can_disconnect_and_remove(&self) -> bool {
        self.disconnect_blocker.is_none()
    }
}

/// Resolve one Route and inspect Track-lock admission from a Sequence snapshot.
pub fn inspect_audio_route(
    sequence: &Sequence,
    route_id: AudioRouteId,
) -> Result<AudioRouteInspection<'_>, AudioRoutingAddressError> {
    let route = resolve_route(sequence, route_id)?;
    let edit_blocker =
        source_locked_track(sequence, route.source)?.map(AudioRoutingEditBlocker::LockedTrack);
    Ok(AudioRouteInspection { route, edit_blocker })
}

/// Resolve one Bus and inspect its connected Route deletion obligations.
pub fn inspect_audio_mix_bus(
    sequence: &Sequence,
    bus_id: MixBusId,
) -> Result<AudioMixBusInspection<'_>, AudioRoutingAddressError> {
    let bus = resolve_bus(sequence, bus_id)?;
    let mut connected_route_count = 0;
    let mut disconnect_blocker = None;
    for route in sequence
        .audio_program
        .routes
        .iter()
        .filter(|route| route_names_bus(route, bus_id))
    {
        connected_route_count += 1;
        if let Some(track_id) = source_locked_track(sequence, route.source)? {
            disconnect_blocker = Some(AudioRoutingEditBlocker::LockedTrack(track_id));
        }
    }
    Ok(AudioMixBusInspection { bus, connected_route_count, disconnect_blocker })
}

/// Validate Routing addresses and prepare bulk Route-addition admission.
pub fn inspect_audio_route_candidates(
    sequence: &Sequence,
) -> Result<AudioRouteCandidateInspection, AudioRoutingAddressError> {
    let known_tracks = sequence.audio_tracks.iter().map(|track| track.id).collect::<HashSet<_>>();
    for track_id in sequence.audio_program.track_channels.keys().copied() {
        if !known_tracks.contains(&track_id) {
            return Err(AudioRoutingAddressError::UnknownTrack(track_id));
        }
    }
    let mut editable_tracks = HashSet::with_capacity(known_tracks.len());
    for track in &sequence.audio_tracks {
        if !sequence.audio_program.track_channels.contains_key(&track.id) {
            return Err(AudioRoutingAddressError::UnknownTrack(track.id));
        }
        if !track.is_locked {
            editable_tracks.insert(track.id);
        }
    }
    let buses = sequence.audio_program.buses.iter().map(|bus| bus.id).collect::<HashSet<_>>();
    let outputs = sequence
        .audio_program
        .outputs
        .iter()
        .map(|output| output.id)
        .collect::<HashSet<_>>();
    let mut bus_edges = HashMap::<MixBusId, Vec<MixBusId>>::new();
    for route in &sequence.audio_program.routes {
        match route.source {
            AudioRouteSource::Track { track_id, .. } if !known_tracks.contains(&track_id) => {
                return Err(AudioRoutingAddressError::UnknownTrack(track_id));
            }
            AudioRouteSource::Bus { bus_id, .. } if !buses.contains(&bus_id) => {
                return Err(AudioRoutingAddressError::UnknownBus(bus_id));
            }
            AudioRouteSource::Track { .. } | AudioRouteSource::Bus { .. } => {}
        }
        match route.destination {
            AudioRouteDestination::Bus(bus_id) if !buses.contains(&bus_id) => {
                return Err(AudioRoutingAddressError::UnknownBus(bus_id));
            }
            AudioRouteDestination::Output(output_id) if !outputs.contains(&output_id) => {
                return Err(AudioRoutingAddressError::UnknownProgramOutput(output_id));
            }
            AudioRouteDestination::Bus(_) | AudioRouteDestination::Output(_) => {}
        }
        if let (
            AudioRouteSource::Bus { bus_id: source, .. },
            AudioRouteDestination::Bus(destination),
        ) = (route.source, route.destination)
        {
            bus_edges.entry(source).or_default().push(destination);
        }
    }
    let bus_reachability = buses
        .iter()
        .copied()
        .map(|bus_id| (bus_id, reachable_buses(bus_id, &bus_edges)))
        .collect();
    Ok(AudioRouteCandidateInspection { editable_tracks, buses, outputs, bus_reachability })
}

fn reachable_buses(
    source: MixBusId,
    edges: &HashMap<MixBusId, Vec<MixBusId>>,
) -> HashSet<MixBusId> {
    let mut reachable = HashSet::new();
    let mut pending = edges.get(&source).cloned().unwrap_or_default();
    while let Some(bus_id) = pending.pop() {
        if reachable.insert(bus_id) {
            pending.extend(edges.get(&bus_id).into_iter().flatten().copied());
        }
    }
    reachable
}

/// Apply one Routing edit atomically to a structurally shared Sequence candidate.
///
/// Strong references, explicit deletion policy, and Track locks are admitted
/// before any COW collection detaches. Only a changed, completely validated
/// Audio Program replaces the input Sequence.
pub fn apply_audio_routing_edit(
    sequence: &mut Sequence,
    request: &AudioRoutingEditRequest,
) -> Result<AudioRoutingEditOutcome, AudioRoutingEditError> {
    admit_edit(sequence, &request.edit)?;
    let mut candidate = sequence.clone();
    let outcome = apply_to_candidate(&mut candidate, &request.edit)?;
    if !outcome.changed {
        return Ok(outcome);
    }
    candidate
        .audio_program
        .validate(
            &candidate.audio_tracks,
            &candidate.audio_roles,
            candidate.settings.audio_channel_layout,
        )
        .map_err(AudioRoutingEditError::AuthorState)?;
    *sequence = candidate;
    Ok(outcome)
}

fn admit_edit(sequence: &Sequence, edit: &AudioRoutingEdit) -> Result<(), AudioRoutingEditError> {
    match edit {
        AudioRoutingEdit::CreateBus { name, route_to } => {
            normalize_bus_name(name)?;
            if let Some(destination) = route_to {
                resolve_destination(sequence, *destination)?;
            }
        }
        AudioRoutingEdit::RenameBus { bus_id, name } => {
            resolve_bus(sequence, *bus_id)?;
            normalize_bus_name(name)?;
        }
        AudioRoutingEdit::RemoveBus { bus_id, policy } => {
            let inspection = inspect_audio_mix_bus(sequence, *bus_id)?;
            if inspection.connected_route_count > 0 {
                match policy {
                    AudioBusRemovalPolicy::RejectIfConnected => {
                        return Err(AudioRoutingEditError::BusConnected {
                            bus_id: *bus_id,
                            route_count: inspection.connected_route_count,
                        });
                    }
                    AudioBusRemovalPolicy::Disconnect => {
                        if let Some(blocker) = inspection.disconnect_blocker {
                            return Err(blocker.into());
                        }
                    }
                }
            }
        }
        AudioRoutingEdit::CreateRoute { source, destination } => {
            admit_source(sequence, *source)?;
            resolve_destination(sequence, *destination)?;
        }
        AudioRoutingEdit::RemoveRoute { route_id }
        | AudioRoutingEdit::SetRouteEnabled { route_id, .. }
        | AudioRoutingEdit::SetRouteGainDb { route_id, .. } => {
            admit_route(sequence, *route_id)?;
            if matches!(edit, AudioRoutingEdit::SetRouteGainDb { .. })
                && resolve_route(sequence, *route_id)?.gain_automation.is_some()
            {
                return Err(AudioRoutingEditError::RouteGainAutomationActive);
            }
        }
        AudioRoutingEdit::SetRouteEndpoints { route_id, source, destination } => {
            admit_route(sequence, *route_id)?;
            admit_source(sequence, *source)?;
            resolve_destination(sequence, *destination)?;
        }
    }
    Ok(())
}

fn apply_to_candidate(
    sequence: &mut Sequence,
    edit: &AudioRoutingEdit,
) -> Result<AudioRoutingEditOutcome, AudioRoutingEditError> {
    match edit {
        AudioRoutingEdit::CreateBus { name, route_to } => {
            let bus_id = MixBusId::new();
            sequence.audio_program.buses.push(AudioMixBus {
                id: bus_id,
                name: normalize_bus_name(name)?,
                strip: AudioChannelStrip::default(),
            });
            let created_route_id = route_to.map(|destination| {
                let route = AudioRoute::new(
                    AudioRouteSource::Bus {
                        bus_id,
                        port: crate::audio::AudioChannelStripOutputPort::PostMute,
                    },
                    destination,
                );
                let route_id = route.id;
                sequence.audio_program.routes.push(route);
                route_id
            });
            Ok(AudioRoutingEditOutcome {
                changed: true,
                created_bus_id: Some(bus_id),
                created_route_id,
            })
        }
        AudioRoutingEdit::RenameBus { bus_id, name } => {
            let name = normalize_bus_name(name)?;
            let bus = resolve_bus_mut(sequence, *bus_id)?;
            if bus.name == name {
                return Ok(AudioRoutingEditOutcome::unchanged());
            }
            bus.name = name;
            Ok(AudioRoutingEditOutcome::changed())
        }
        AudioRoutingEdit::RemoveBus { bus_id, policy } => {
            let before = sequence.audio_program.buses.len();
            sequence.audio_program.buses.retain(|bus| bus.id != *bus_id);
            if matches!(policy, AudioBusRemovalPolicy::Disconnect) {
                sequence.audio_program.routes.retain(|route| !route_names_bus(route, *bus_id));
            }
            debug_assert_eq!(sequence.audio_program.buses.len() + 1, before);
            Ok(AudioRoutingEditOutcome::changed())
        }
        AudioRoutingEdit::CreateRoute { source, destination } => {
            let route = AudioRoute::new(*source, *destination);
            let route_id = route.id;
            sequence.audio_program.routes.push(route);
            Ok(AudioRoutingEditOutcome {
                changed: true,
                created_bus_id: None,
                created_route_id: Some(route_id),
            })
        }
        AudioRoutingEdit::RemoveRoute { route_id } => {
            sequence.audio_program.routes.retain(|route| route.id != *route_id);
            Ok(AudioRoutingEditOutcome::changed())
        }
        AudioRoutingEdit::SetRouteEndpoints { route_id, source, destination } => {
            let route = resolve_route_mut(sequence, *route_id)?;
            if route.source == *source && route.destination == *destination {
                return Ok(AudioRoutingEditOutcome::unchanged());
            }
            route.source = *source;
            route.destination = *destination;
            Ok(AudioRoutingEditOutcome::changed())
        }
        AudioRoutingEdit::SetRouteEnabled { route_id, enabled } => {
            let route = resolve_route_mut(sequence, *route_id)?;
            if route.enabled == *enabled {
                return Ok(AudioRoutingEditOutcome::unchanged());
            }
            route.enabled = *enabled;
            Ok(AudioRoutingEditOutcome::changed())
        }
        AudioRoutingEdit::SetRouteGainDb { route_id, value } => {
            let route = resolve_route_mut(sequence, *route_id)?;
            if route.gain_db == *value {
                return Ok(AudioRoutingEditOutcome::unchanged());
            }
            route.gain_db = *value;
            Ok(AudioRoutingEditOutcome::changed())
        }
    }
}

fn normalize_bus_name(name: &str) -> Result<String, AudioRoutingEditError> {
    let name = name.trim();
    if name.is_empty() {
        Err(AudioRoutingEditError::InvalidBusName)
    } else {
        Ok(name.to_owned())
    }
}

fn admit_route(sequence: &Sequence, route_id: AudioRouteId) -> Result<(), AudioRoutingEditError> {
    let inspection = inspect_audio_route(sequence, route_id)?;
    if let Some(blocker) = inspection.edit_blocker {
        Err(blocker.into())
    } else {
        Ok(())
    }
}

fn admit_source(
    sequence: &Sequence,
    source: AudioRouteSource,
) -> Result<(), AudioRoutingEditError> {
    if let Some(track_id) = source_locked_track(sequence, source)? {
        return Err(AudioRoutingEditBlocker::LockedTrack(track_id).into());
    }
    Ok(())
}

fn source_locked_track(
    sequence: &Sequence,
    source: AudioRouteSource,
) -> Result<Option<TrackId>, AudioRoutingAddressError> {
    match source {
        AudioRouteSource::Track { track_id, .. } => {
            let track = sequence
                .audio_tracks
                .iter()
                .find(|track| track.id == track_id)
                .ok_or(AudioRoutingAddressError::UnknownTrack(track_id))?;
            if !sequence.audio_program.track_channels.contains_key(&track_id) {
                return Err(AudioRoutingAddressError::UnknownTrack(track_id));
            }
            Ok(track.is_locked.then_some(track_id))
        }
        AudioRouteSource::Bus { bus_id, .. } => {
            resolve_bus(sequence, bus_id)?;
            Ok(None)
        }
    }
}

fn resolve_destination(
    sequence: &Sequence,
    destination: AudioRouteDestination,
) -> Result<(), AudioRoutingAddressError> {
    match destination {
        AudioRouteDestination::Bus(bus_id) => {
            resolve_bus(sequence, bus_id)?;
        }
        AudioRouteDestination::Output(output_id) => {
            if !sequence.audio_program.outputs.iter().any(|output| output.id == output_id) {
                return Err(AudioRoutingAddressError::UnknownProgramOutput(output_id));
            }
        }
    }
    Ok(())
}

fn resolve_bus(
    sequence: &Sequence,
    bus_id: MixBusId,
) -> Result<&AudioMixBus, AudioRoutingAddressError> {
    sequence
        .audio_program
        .buses
        .iter()
        .find(|bus| bus.id == bus_id)
        .ok_or(AudioRoutingAddressError::UnknownBus(bus_id))
}

fn resolve_bus_mut(
    sequence: &mut Sequence,
    bus_id: MixBusId,
) -> Result<&mut AudioMixBus, AudioRoutingEditError> {
    sequence
        .audio_program
        .buses
        .iter_mut()
        .find(|bus| bus.id == bus_id)
        .ok_or(AudioRoutingAddressError::UnknownBus(bus_id).into())
}

fn resolve_route(
    sequence: &Sequence,
    route_id: AudioRouteId,
) -> Result<&AudioRoute, AudioRoutingAddressError> {
    sequence
        .audio_program
        .routes
        .iter()
        .find(|route| route.id == route_id)
        .ok_or(AudioRoutingAddressError::UnknownRoute(route_id))
}

fn resolve_route_mut(
    sequence: &mut Sequence,
    route_id: AudioRouteId,
) -> Result<&mut AudioRoute, AudioRoutingEditError> {
    sequence
        .audio_program
        .routes
        .iter_mut()
        .find(|route| route.id == route_id)
        .ok_or(AudioRoutingAddressError::UnknownRoute(route_id).into())
}

fn route_names_bus(route: &AudioRoute, bus_id: MixBusId) -> bool {
    matches!(route.source, AudioRouteSource::Bus { bus_id: source, .. } if source == bus_id)
        || route.destination == AudioRouteDestination::Bus(bus_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioChannelStripOutputPort;
    use crate::{
        apply_audio_automation_edit, AudioAutomationEdit, AudioAutomationEditRequest,
        AudioAutomationTarget,
    };
    use mondrian_core::{ExactAutomationKeyframe, TimelineTime};

    fn request(edit: AudioRoutingEdit) -> AudioRoutingEditRequest {
        AudioRoutingEditRequest { edit }
    }

    #[test]
    fn bus_lifecycle_and_connected_route_policy_are_one_atomic_interface() {
        let mut sequence = Sequence::new("Routing authoring");
        let output_id = sequence.audio_program.outputs[0].id;
        let created = apply_audio_routing_edit(
            &mut sequence,
            &request(AudioRoutingEdit::CreateBus {
                name: "  Dialogue  ".to_owned(),
                route_to: Some(AudioRouteDestination::Output(output_id)),
            }),
        )
        .expect("create routed Bus");
        let bus_id = created.created_bus_id.expect("Bus identity");
        let route_id = created.created_route_id.expect("Route identity");
        assert_eq!(
            resolve_bus(&sequence, bus_id).expect("Bus").name,
            "Dialogue"
        );
        assert_eq!(
            resolve_route(&sequence, route_id).expect("Route").destination,
            AudioRouteDestination::Output(output_id)
        );
        assert_eq!(
            apply_audio_routing_edit(
                &mut sequence,
                &request(AudioRoutingEdit::RenameBus {
                    bus_id,
                    name: "  Dialogue Stem  ".to_owned(),
                }),
            )
            .expect("rename Bus"),
            AudioRoutingEditOutcome::changed()
        );
        assert_eq!(
            resolve_bus(&sequence, bus_id).expect("Bus").name,
            "Dialogue Stem"
        );
        assert_eq!(
            apply_audio_routing_edit(
                &mut sequence,
                &request(AudioRoutingEdit::RenameBus { bus_id, name: "Dialogue Stem".to_owned() }),
            )
            .expect("canonical no-op rename"),
            AudioRoutingEditOutcome::unchanged()
        );

        assert_eq!(
            apply_audio_routing_edit(
                &mut sequence,
                &request(AudioRoutingEdit::RemoveBus {
                    bus_id,
                    policy: AudioBusRemovalPolicy::RejectIfConnected,
                }),
            ),
            Err(AudioRoutingEditError::BusConnected { bus_id, route_count: 1 })
        );
        assert!(resolve_bus(&sequence, bus_id).is_ok());

        apply_audio_routing_edit(
            &mut sequence,
            &request(AudioRoutingEdit::RemoveBus {
                bus_id,
                policy: AudioBusRemovalPolicy::Disconnect,
            }),
        )
        .expect("disconnect and remove");
        assert_eq!(
            resolve_bus(&sequence, bus_id),
            Err(AudioRoutingAddressError::UnknownBus(bus_id))
        );
        assert_eq!(
            resolve_route(&sequence, route_id),
            Err(AudioRoutingAddressError::UnknownRoute(route_id))
        );
    }

    #[test]
    fn route_identity_controls_and_exact_gain_automation_remain_distinct() {
        let mut sequence = Sequence::new("Route controls");
        let track_id = sequence.audio_tracks[0].id;
        let output_id = sequence.audio_program.outputs[0].id;
        let bus_id = apply_audio_routing_edit(
            &mut sequence,
            &request(AudioRoutingEdit::CreateBus {
                name: "Rewire destination".to_owned(),
                route_to: Some(AudioRouteDestination::Output(output_id)),
            }),
        )
        .expect("create destination Bus")
        .created_bus_id
        .expect("Bus identity");
        let route = apply_audio_routing_edit(
            &mut sequence,
            &request(AudioRoutingEdit::CreateRoute {
                source: AudioRouteSource::Track {
                    track_id,
                    port: AudioChannelStripOutputPort::PreFader,
                },
                destination: AudioRouteDestination::Output(output_id),
            }),
        )
        .expect("create parallel Route");
        let route_id = route.created_route_id.expect("Route identity");
        apply_audio_routing_edit(
            &mut sequence,
            &request(AudioRoutingEdit::SetRouteGainDb { route_id, value: -6.0 }),
        )
        .expect("set static gain");
        let keyframe = ExactAutomationKeyframe::linear(TimelineTime::ONE, -12.0);
        let keyframe_id = keyframe.id;
        apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest {
                target: AudioAutomationTarget::RouteGain { route_id },
                edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
            },
        )
        .expect("add exact key");
        apply_audio_routing_edit(
            &mut sequence,
            &request(AudioRoutingEdit::SetRouteEndpoints {
                route_id,
                source: AudioRouteSource::Track {
                    track_id,
                    port: AudioChannelStripOutputPort::PostMute,
                },
                destination: AudioRouteDestination::Bus(bus_id),
            }),
        )
        .expect("rewire stable Route");
        let rewired = resolve_route(&sequence, route_id).expect("rewired Route");
        assert_eq!(
            rewired.source,
            AudioRouteSource::Track {
                track_id,
                port: AudioChannelStripOutputPort::PostMute,
            }
        );
        assert_eq!(rewired.destination, AudioRouteDestination::Bus(bus_id));
        assert_eq!(rewired.gain_db, -6.0);
        assert!(rewired.gain_automation.is_some());
        let before = sequence.clone();
        assert_eq!(
            apply_audio_routing_edit(
                &mut sequence,
                &request(AudioRoutingEdit::SetRouteGainDb { route_id, value: -3.0 }),
            ),
            Err(AudioRoutingEditError::RouteGainAutomationActive)
        );
        assert_eq!(sequence, before);

        apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest {
                target: AudioAutomationTarget::RouteGain { route_id },
                edit: AudioAutomationEdit::RemoveKeyframe { keyframe_id },
            },
        )
        .expect("remove final key");
        let route = resolve_route(&sequence, route_id).expect("Route");
        assert!(route.gain_automation.is_none());
        assert_eq!(route.gain_db, -6.0);
    }

    #[test]
    fn bulk_route_candidate_inspection_is_transitive_locked_and_fail_closed() {
        let mut sequence = Sequence::new("Route candidates");
        let track_id = sequence.audio_tracks[0].id;
        let output_id = sequence.audio_program.outputs[0].id;
        let first = MixBusId::new();
        let second = MixBusId::new();
        let third = MixBusId::new();
        sequence.audio_program.buses.extend([
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
            AudioMixBus {
                id: third,
                name: "Third".to_owned(),
                strip: AudioChannelStrip::default(),
            },
        ]);
        let mut first_edge = AudioRoute::new(
            AudioRouteSource::Bus {
                bus_id: first,
                port: AudioChannelStripOutputPort::PostMute,
            },
            AudioRouteDestination::Bus(second),
        );
        first_edge.enabled = false;
        sequence.audio_program.routes.extend([
            first_edge,
            AudioRoute::new(
                AudioRouteSource::Bus {
                    bus_id: second,
                    port: AudioChannelStripOutputPort::PostMute,
                },
                AudioRouteDestination::Bus(third),
            ),
        ]);

        let candidates = inspect_audio_route_candidates(&sequence).expect("candidate index");
        assert!(!candidates.allows_addition(
            AudioRouteSource::Bus {
                bus_id: third,
                port: AudioChannelStripOutputPort::PostMute,
            },
            AudioRouteDestination::Bus(first),
        ));
        assert!(candidates.allows_addition(
            AudioRouteSource::Bus {
                bus_id: third,
                port: AudioChannelStripOutputPort::PostMute,
            },
            AudioRouteDestination::Output(output_id),
        ));
        assert!(!candidates.allows_addition(
            AudioRouteSource::Bus {
                bus_id: MixBusId::new(),
                port: AudioChannelStripOutputPort::PostMute,
            },
            AudioRouteDestination::Output(output_id),
        ));

        sequence.audio_tracks[0].is_locked = true;
        let candidates = inspect_audio_route_candidates(&sequence).expect("locked candidate index");
        assert!(!candidates.allows_addition(
            AudioRouteSource::Track {
                track_id,
                port: AudioChannelStripOutputPort::PostMute,
            },
            AudioRouteDestination::Bus(first),
        ));

        sequence.audio_program.routes.push(AudioRoute::new(
            AudioRouteSource::Bus {
                bus_id: MixBusId::new(),
                port: AudioChannelStripOutputPort::PostMute,
            },
            AudioRouteDestination::Output(output_id),
        ));
        assert!(matches!(
            inspect_audio_route_candidates(&sequence),
            Err(AudioRoutingAddressError::UnknownBus(_))
        ));
    }

    #[test]
    fn locks_cycles_invalid_names_and_rejected_edits_never_publish_partial_state() {
        let mut locked_disconnect = Sequence::new("Locked Bus dependency");
        let locked_track_id = locked_disconnect.audio_tracks[0].id;
        let locked_bus_id = apply_audio_routing_edit(
            &mut locked_disconnect,
            &request(AudioRoutingEdit::CreateBus {
                name: "Locked destination".to_owned(),
                route_to: None,
            }),
        )
        .expect("Bus")
        .created_bus_id
        .expect("Bus identity");
        apply_audio_routing_edit(
            &mut locked_disconnect,
            &request(AudioRoutingEdit::CreateRoute {
                source: AudioRouteSource::Track {
                    track_id: locked_track_id,
                    port: AudioChannelStripOutputPort::PostMute,
                },
                destination: AudioRouteDestination::Bus(locked_bus_id),
            }),
        )
        .expect("Track Route");
        locked_disconnect.audio_tracks[0].is_locked = true;
        let inspection = inspect_audio_mix_bus(&locked_disconnect, locked_bus_id).expect("Bus");
        assert_eq!(
            inspection.disconnect_blocker(),
            Some(&AudioRoutingEditBlocker::LockedTrack(locked_track_id))
        );
        let before_disconnect = locked_disconnect.clone();
        assert_eq!(
            apply_audio_routing_edit(
                &mut locked_disconnect,
                &request(AudioRoutingEdit::RemoveBus {
                    bus_id: locked_bus_id,
                    policy: AudioBusRemovalPolicy::Disconnect,
                }),
            ),
            Err(AudioRoutingEditError::Blocked(
                AudioRoutingEditBlocker::LockedTrack(locked_track_id)
            ))
        );
        assert_eq!(locked_disconnect, before_disconnect);

        let mut sequence = Sequence::new("Routing admission");
        let track_id = sequence.audio_tracks[0].id;
        let output_id = sequence.audio_program.outputs[0].id;
        sequence.audio_tracks[0].is_locked = true;
        let before_locked = sequence.clone();
        assert_eq!(
            apply_audio_routing_edit(
                &mut sequence,
                &request(AudioRoutingEdit::CreateRoute {
                    source: AudioRouteSource::Track {
                        track_id,
                        port: AudioChannelStripOutputPort::PostMute,
                    },
                    destination: AudioRouteDestination::Output(output_id),
                }),
            ),
            Err(AudioRoutingEditError::Blocked(
                AudioRoutingEditBlocker::LockedTrack(track_id)
            ))
        );
        assert_eq!(sequence, before_locked);
        sequence.audio_tracks[0].is_locked = false;

        let first = apply_audio_routing_edit(
            &mut sequence,
            &request(AudioRoutingEdit::CreateBus { name: "First".to_owned(), route_to: None }),
        )
        .expect("first Bus")
        .created_bus_id
        .expect("first identity");
        let second = apply_audio_routing_edit(
            &mut sequence,
            &request(AudioRoutingEdit::CreateBus {
                name: "Second".to_owned(),
                route_to: Some(AudioRouteDestination::Bus(first)),
            }),
        )
        .expect("second Bus")
        .created_bus_id
        .expect("second identity");
        let before_cycle = sequence.clone();
        assert_eq!(
            apply_audio_routing_edit(
                &mut sequence,
                &request(AudioRoutingEdit::CreateRoute {
                    source: AudioRouteSource::Bus {
                        bus_id: first,
                        port: AudioChannelStripOutputPort::PostMute,
                    },
                    destination: AudioRouteDestination::Bus(second),
                }),
            ),
            Err(AudioRoutingEditError::AuthorState(
                AudioAuthoringError::RouteCycle
            ))
        );
        assert_eq!(sequence, before_cycle);

        assert_eq!(
            apply_audio_routing_edit(
                &mut sequence,
                &request(AudioRoutingEdit::RenameBus { bus_id: first, name: "   ".to_owned() }),
            ),
            Err(AudioRoutingEditError::InvalidBusName)
        );
    }
}
