//! Atomic authoring for the Sequence-owned Audio Routing graph.
//!
//! A single typed Route graph represents both principal paths and parallel
//! sends. This Module owns Bus/Route identity, strong-reference admission,
//! Track-lock policy, exact Route-gain automation, graph validation, and
//! transactional publication so callers never patch routing collections.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::audio::{
    author_routing_graph, AudioAuthorRoutingNode, AudioAuthoringError, AudioChannelStrip,
    AudioMixBus, AudioProcessorSidechainRoute, AudioRoute, AudioRouteDestination, AudioRouteSource,
};
use crate::sequence::Sequence;
use mondrian_core::{AudioProcessorInstanceId, AudioRouteId, MixBusId, ProgramOutputId, TrackId};
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
    /// Add one enabled unity-gain processor auxiliary-input Route.
    CreateProcessorSidechain {
        /// Stable Track/Bus output port.
        source: AudioRouteSource,
        /// Stable target processor instance.
        processor_id: AudioProcessorInstanceId,
        /// Stable definition-owned auxiliary bus key.
        bus_key: String,
    },
    /// Delete one processor auxiliary-input Route by stable identity.
    RemoveProcessorSidechain {
        /// Stable sidechain Route identity.
        route_id: AudioRouteId,
    },
    /// Rewire one processor auxiliary-input Route while preserving controls.
    SetProcessorSidechainEndpoints {
        /// Stable sidechain Route identity.
        route_id: AudioRouteId,
        /// Replacement Track/Bus output port.
        source: AudioRouteSource,
        /// Replacement target processor instance.
        processor_id: AudioProcessorInstanceId,
        /// Replacement stable auxiliary bus key.
        bus_key: String,
    },
    /// Enable or retain one authored processor auxiliary Route.
    SetProcessorSidechainEnabled {
        /// Stable sidechain Route identity.
        route_id: AudioRouteId,
        /// Whether this Route contributes to compiled Signal Closures.
        enabled: bool,
    },
    /// Replace a sidechain Route's static gain while automation is absent.
    SetProcessorSidechainGainDb {
        /// Stable sidechain Route identity.
        route_id: AudioRouteId,
        /// Decibel value validated by the complete Audio Program contract.
        value: f64,
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
    /// Processor identity is absent from the Sequence Audio Program.
    #[error("Audio sidechain references an unknown processor: {0}")]
    UnknownProcessor(AudioProcessorInstanceId),
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

/// Canonical processor sidechain and source-lock admission.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioProcessorSidechainInspection<'a> {
    route: &'a AudioProcessorSidechainRoute,
    edit_blocker: Option<AudioRoutingEditBlocker>,
}

impl<'a> AudioProcessorSidechainInspection<'a> {
    /// Retained canonical route, even when its native bus is unavailable.
    pub fn route(&self) -> &'a AudioProcessorSidechainRoute {
        self.route
    }
    /// Authoritative source-lock blocker.
    pub fn edit_blocker(&self) -> Option<&AudioRoutingEditBlocker> {
        self.edit_blocker.as_ref()
    }
    /// Whether controls on this retained route may enter a transaction.
    pub fn is_editable(&self) -> bool {
        self.edit_blocker.is_none()
    }
}

/// Inspect a retained sidechain without loading its target plugin or bus.
///
/// Native bus availability does not remove author routes or prevent their removal.
/// Source identity and lock admission are shared with actual routing edits.
pub fn inspect_audio_processor_sidechain(
    sequence: &Sequence,
    route_id: AudioRouteId,
) -> Result<AudioProcessorSidechainInspection<'_>, AudioRoutingAddressError> {
    let route = resolve_sidechain_route(sequence, route_id)?;
    let edit_blocker =
        source_locked_track(sequence, &route.source)?.map(AudioRoutingEditBlocker::LockedTrack);
    Ok(AudioProcessorSidechainInspection { route, edit_blocker })
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
/// The inspection validates retained endpoints and precomputes reachability
/// using the same complete main/sidechain graph as author validation. No
/// candidate Sequence clones or UI-owned graph interpretation are required.
/// Reachability takes `O(N * (N + E) * log N)` time and `O(N²)` storage for `N` author
/// nodes and `E` retained edges; a principal addition query is expected `O(1)`.
/// Processor queries check each Track materialization of a shared Scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioRouteCandidateInspection {
    editable_tracks: HashSet<TrackId>,
    buses: HashSet<MixBusId>,
    outputs: HashSet<ProgramOutputId>,
    reachability: HashMap<AudioAuthorRoutingNode, HashSet<AudioAuthorRoutingNode>>,
    processor_targets: HashMap<AudioProcessorInstanceId, BTreeSet<AudioAuthorRoutingNode>>,
    processor_sources: HashMap<AudioProcessorInstanceId, AudioAuthorRoutingNode>,
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
        let target = match destination {
            AudioRouteDestination::Bus(bus_id) if self.buses.contains(&bus_id) => {
                AudioAuthorRoutingNode::Bus(bus_id, crate::AudioChannelSignalStage::PreFader)
            }
            AudioRouteDestination::Output(output_id) if self.outputs.contains(&output_id) => {
                AudioAuthorRoutingNode::Output(output_id, crate::AudioChannelSignalStage::PreFader)
            }
            _ => return false,
        };
        self.source_is_editable(&source) && self.edge_is_acyclic(&source, target)
    }

    /// Whether adding a source to this processor passes address, source-lock
    /// and cycle admission for every materialized occurrence of its owner.
    ///
    /// This does not inspect native bus keys or layouts; processor preparation
    /// and the complete author transaction remain authoritative for those.
    pub fn allows_processor_sidechain_addition(
        &self,
        source: AudioRouteSource,
        processor_id: AudioProcessorInstanceId,
    ) -> bool {
        self.source_is_editable(&source)
            && self.processor_targets.get(&processor_id).is_some_and(|targets| {
                targets.iter().all(|target| self.edge_is_acyclic(&source, *target))
            })
    }

    fn source_node(&self, source: &AudioRouteSource) -> Option<AudioAuthorRoutingNode> {
        use crate::audio::{AudioChannelSignalStage, AudioProcessorOutputPort};
        Some(match source {
            AudioRouteSource::Track { track_id, port } => {
                AudioAuthorRoutingNode::Track(*track_id, AudioChannelSignalStage::producing(*port))
            }
            AudioRouteSource::Bus { bus_id, port } => {
                AudioAuthorRoutingNode::Bus(*bus_id, AudioChannelSignalStage::producing(*port))
            }
            AudioRouteSource::ProcessorOutput { processor_id, port } => {
                if let AudioProcessorOutputPort::Auxiliary { bus_key } = port
                    && (bus_key.trim().is_empty()
                        || bus_key.len() > 128
                        || bus_key.chars().any(char::is_control))
                {
                    return None;
                }
                *self.processor_sources.get(processor_id)?
            }
        })
    }

    fn source_is_editable(&self, source: &AudioRouteSource) -> bool {
        match self.source_node(source) {
            Some(AudioAuthorRoutingNode::Track(track_id, _)) => {
                self.editable_tracks.contains(&track_id)
            }
            Some(AudioAuthorRoutingNode::Bus(bus_id, _)) => self.buses.contains(&bus_id),
            _ => false,
        }
    }

    fn edge_is_acyclic(&self, source: &AudioRouteSource, target: AudioAuthorRoutingNode) -> bool {
        let Some(source) = self.source_node(source) else {
            return false;
        };
        source != target
            && !self
                .reachability
                .get(&target)
                .is_some_and(|reachable| reachable.contains(&source))
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
        source_locked_track(sequence, &route.source)?.map(AudioRoutingEditBlocker::LockedTrack);
    Ok(AudioRouteInspection { route, edit_blocker })
}

/// Resolve one Bus and inspect its connected Route deletion obligations.
pub fn inspect_audio_mix_bus(
    sequence: &Sequence,
    bus_id: MixBusId,
) -> Result<AudioMixBusInspection<'_>, AudioRoutingAddressError> {
    let bus = resolve_bus(sequence, bus_id)?;
    let mut inspections = inspect_mix_bus_dependencies(sequence, std::iter::once(bus))?;
    inspections.pop().ok_or(AudioRoutingAddressError::UnknownBus(bus_id))
}

/// Inspect every Mix Bus deletion obligation in canonical Bus order.
///
/// Main routes and processor sidechains are traversed once. Enabled and disabled
/// edges remain strong dependencies; a route naming a Bus at both ends counts
/// once for that Bus. Single-Bus mutation admission uses the same implementation.
pub fn inspect_audio_mix_buses(
    sequence: &Sequence,
) -> Result<Vec<AudioMixBusInspection<'_>>, AudioRoutingAddressError> {
    inspect_mix_bus_dependencies(sequence, sequence.audio_program.buses.iter())
}

fn inspect_mix_bus_dependencies<'a>(
    sequence: &'a Sequence,
    buses: impl IntoIterator<Item = &'a AudioMixBus>,
) -> Result<Vec<AudioMixBusInspection<'a>>, AudioRoutingAddressError> {
    let mut inspections = buses
        .into_iter()
        .map(|bus| AudioMixBusInspection {
            bus,
            connected_route_count: 0,
            disconnect_blocker: None,
        })
        .collect::<Vec<_>>();
    let bus_slots = inspections
        .iter()
        .enumerate()
        .map(|(slot, inspection)| (inspection.bus.id, slot))
        .collect::<HashMap<_, _>>();
    let processor_slots = inspections
        .iter()
        .enumerate()
        .flat_map(|(slot, inspection)| {
            [
                &inspection.bus.strip.pre_fader,
                &inspection.bus.strip.post_fader,
            ]
            .into_iter()
            .flat_map(move |rack| rack.processors.iter().map(move |processor| (processor.id, slot)))
        })
        .collect::<HashMap<_, _>>();
    let source_slot = |source: &AudioRouteSource| match source {
        AudioRouteSource::Bus { bus_id, .. } => bus_slots.get(bus_id).copied(),
        AudioRouteSource::ProcessorOutput { processor_id, .. } => {
            processor_slots.get(processor_id).copied()
        }
        AudioRouteSource::Track { .. } => None,
    };
    let mut record = |source: &AudioRouteSource, first: Option<usize>, second: Option<usize>| {
        if first.is_none() && second.is_none() {
            return Ok(());
        }
        let blocker =
            source_locked_track(sequence, source)?.map(AudioRoutingEditBlocker::LockedTrack);
        for slot in [first, second.filter(|slot| Some(*slot) != first)].into_iter().flatten() {
            let inspection = &mut inspections[slot];
            inspection.connected_route_count += 1;
            if blocker.is_some() {
                inspection.disconnect_blocker.clone_from(&blocker);
            }
        }
        Ok::<_, AudioRoutingAddressError>(())
    };
    for route in &sequence.audio_program.routes {
        let destination_slot = match route.destination {
            AudioRouteDestination::Bus(bus_id) => bus_slots.get(&bus_id).copied(),
            AudioRouteDestination::Output(_) => None,
        };
        record(&route.source, source_slot(&route.source), destination_slot)?;
    }
    for route in &sequence.audio_program.sidechain_routes {
        record(
            &route.source,
            source_slot(&route.source),
            processor_slots.get(&route.processor_id).copied(),
        )?;
    }
    Ok(inspections)
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
    for route in &sequence.audio_program.routes {
        match route.source {
            AudioRouteSource::Track { track_id, .. } if !known_tracks.contains(&track_id) => {
                return Err(AudioRoutingAddressError::UnknownTrack(track_id));
            }
            AudioRouteSource::Bus { bus_id, .. } if !buses.contains(&bus_id) => {
                return Err(AudioRoutingAddressError::UnknownBus(bus_id));
            }
            AudioRouteSource::ProcessorOutput { .. } => {
                source_locked_track(sequence, &route.source)?;
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
    }
    let graph = author_routing_graph(&sequence.audio_program, &sequence.audio_tracks);
    for route in &sequence.audio_program.sidechain_routes {
        match route.source {
            AudioRouteSource::Track { track_id, .. } if !known_tracks.contains(&track_id) => {
                return Err(AudioRoutingAddressError::UnknownTrack(track_id));
            }
            AudioRouteSource::Bus { bus_id, .. } if !buses.contains(&bus_id) => {
                return Err(AudioRoutingAddressError::UnknownBus(bus_id));
            }
            AudioRouteSource::ProcessorOutput { .. } => {
                source_locked_track(sequence, &route.source)?;
            }
            AudioRouteSource::Track { .. } | AudioRouteSource::Bus { .. } => {}
        }
        if !graph.processor_targets.contains_key(&route.processor_id) {
            return Err(AudioRoutingAddressError::UnknownProcessor(
                route.processor_id,
            ));
        }
    }
    let reachability = graph
        .outgoing
        .keys()
        .copied()
        .map(|node| (node, reachable_nodes(node, &graph.outgoing)))
        .collect();
    let processor_sources = sequence
        .audio_program
        .track_channels
        .iter()
        .map(|(id, channel)| {
            (
                crate::AudioChannelStripOwner::Track { track_id: *id },
                &channel.strip,
            )
        })
        .chain(sequence.audio_program.buses.iter().map(|bus| {
            (
                crate::AudioChannelStripOwner::Bus { bus_id: bus.id },
                &bus.strip,
            )
        }))
        .flat_map(|(owner, strip)| {
            [
                (&strip.pre_fader, crate::AudioChannelSignalStage::PreFader),
                (&strip.post_fader, crate::AudioChannelSignalStage::PostFader),
            ]
            .into_iter()
            .flat_map(move |(rack, stage)| {
                rack.processors.iter().map(move |processor| {
                    (
                        processor.id,
                        match owner {
                            crate::AudioChannelStripOwner::Track { track_id } => {
                                AudioAuthorRoutingNode::Track(track_id, stage)
                            }
                            crate::AudioChannelStripOwner::Bus { bus_id } => {
                                AudioAuthorRoutingNode::Bus(bus_id, stage)
                            }
                            crate::AudioChannelStripOwner::ProgramOutput { .. } => {
                                unreachable!("only Track/Bus source strips")
                            }
                        },
                    )
                })
            })
        })
        .collect();
    let processor_targets = graph.processor_targets.into_iter().collect();
    Ok(AudioRouteCandidateInspection {
        editable_tracks,
        buses,
        outputs,
        reachability,
        processor_targets,
        processor_sources,
    })
}

fn reachable_nodes(
    source: AudioAuthorRoutingNode,
    edges: &BTreeMap<AudioAuthorRoutingNode, BTreeSet<AudioAuthorRoutingNode>>,
) -> HashSet<AudioAuthorRoutingNode> {
    let mut reachable = HashSet::new();
    let mut pending = edges.get(&source).into_iter().flatten().copied().collect::<Vec<_>>();
    while let Some(node) = pending.pop() {
        if reachable.insert(node) {
            pending.extend(edges.get(&node).into_iter().flatten().copied());
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
            admit_source(sequence, source)?;
            resolve_destination(sequence, *destination)?;
        }
        AudioRoutingEdit::CreateProcessorSidechain { source, processor_id, .. } => {
            admit_source(sequence, source)?;
            resolve_processor(sequence, *processor_id)?;
        }
        AudioRoutingEdit::RemoveProcessorSidechain { route_id }
        | AudioRoutingEdit::SetProcessorSidechainEnabled { route_id, .. }
        | AudioRoutingEdit::SetProcessorSidechainGainDb { route_id, .. } => {
            admit_sidechain_route(sequence, *route_id)?;
            if matches!(edit, AudioRoutingEdit::SetProcessorSidechainGainDb { .. })
                && resolve_sidechain_route(sequence, *route_id)?.gain_automation.is_some()
            {
                return Err(AudioRoutingEditError::RouteGainAutomationActive);
            }
        }
        AudioRoutingEdit::SetProcessorSidechainEndpoints {
            route_id, source, processor_id, ..
        } => {
            admit_sidechain_route(sequence, *route_id)?;
            admit_source(sequence, source)?;
            resolve_processor(sequence, *processor_id)?;
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
            admit_source(sequence, source)?;
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
            let removed_processor_ids = sequence
                .audio_program
                .buses
                .iter()
                .find(|bus| bus.id == *bus_id)
                .map(|bus| {
                    [&bus.strip.pre_fader, &bus.strip.post_fader]
                        .into_iter()
                        .flat_map(|rack| rack.processors.iter().map(|processor| processor.id))
                        .collect::<HashSet<_>>()
                })
                .ok_or(AudioRoutingAddressError::UnknownBus(*bus_id))?;
            let before = sequence.audio_program.buses.len();
            sequence.audio_program.buses.retain(|bus| bus.id != *bus_id);
            if matches!(policy, AudioBusRemovalPolicy::Disconnect) {
                sequence.audio_program.routes.retain(|route| !route_names_bus(route, *bus_id)
                    && !matches!(&route.source, AudioRouteSource::ProcessorOutput { processor_id, .. } if removed_processor_ids.contains(processor_id)));
                sequence.audio_program.sidechain_routes.retain(|route| {
                    !matches!(route.source, AudioRouteSource::Bus { bus_id: source, .. } if source == *bus_id)
                        && !matches!(&route.source, AudioRouteSource::ProcessorOutput { processor_id, .. } if removed_processor_ids.contains(processor_id))
                        && !removed_processor_ids.contains(&route.processor_id)
                });
            }
            debug_assert_eq!(sequence.audio_program.buses.len() + 1, before);
            Ok(AudioRoutingEditOutcome::changed())
        }
        AudioRoutingEdit::CreateRoute { source, destination } => {
            let route = AudioRoute::new(source.clone(), *destination);
            let route_id = route.id;
            sequence.audio_program.routes.push(route);
            Ok(AudioRoutingEditOutcome {
                changed: true,
                created_bus_id: None,
                created_route_id: Some(route_id),
            })
        }
        AudioRoutingEdit::CreateProcessorSidechain { source, processor_id, bus_key } => {
            let route =
                AudioProcessorSidechainRoute::new(source.clone(), *processor_id, bus_key.clone());
            let route_id = route.id;
            sequence.audio_program.sidechain_routes.push(route);
            Ok(AudioRoutingEditOutcome {
                changed: true,
                created_bus_id: None,
                created_route_id: Some(route_id),
            })
        }
        AudioRoutingEdit::RemoveProcessorSidechain { route_id } => {
            sequence.audio_program.sidechain_routes.retain(|route| route.id != *route_id);
            Ok(AudioRoutingEditOutcome::changed())
        }
        AudioRoutingEdit::SetProcessorSidechainEndpoints {
            route_id,
            source,
            processor_id,
            bus_key,
        } => {
            let route = resolve_sidechain_route_mut(sequence, *route_id)?;
            if route.source == *source
                && route.processor_id == *processor_id
                && route.bus_key == *bus_key
            {
                return Ok(AudioRoutingEditOutcome::unchanged());
            }
            route.source.clone_from(source);
            route.processor_id = *processor_id;
            route.bus_key.clone_from(bus_key);
            Ok(AudioRoutingEditOutcome::changed())
        }
        AudioRoutingEdit::SetProcessorSidechainEnabled { route_id, enabled } => {
            let route = resolve_sidechain_route_mut(sequence, *route_id)?;
            if route.enabled == *enabled {
                return Ok(AudioRoutingEditOutcome::unchanged());
            }
            route.enabled = *enabled;
            Ok(AudioRoutingEditOutcome::changed())
        }
        AudioRoutingEdit::SetProcessorSidechainGainDb { route_id, value } => {
            let route = resolve_sidechain_route_mut(sequence, *route_id)?;
            if route.gain_db == *value {
                return Ok(AudioRoutingEditOutcome::unchanged());
            }
            route.gain_db = *value;
            Ok(AudioRoutingEditOutcome::changed())
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
            route.source.clone_from(source);
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

fn admit_sidechain_route(
    sequence: &Sequence,
    route_id: AudioRouteId,
) -> Result<(), AudioRoutingEditError> {
    let inspection = inspect_audio_processor_sidechain(sequence, route_id)?;
    if let Some(blocker) = inspection.edit_blocker {
        Err(blocker.into())
    } else {
        Ok(())
    }
}

fn admit_source(
    sequence: &Sequence,
    source: &AudioRouteSource,
) -> Result<(), AudioRoutingEditError> {
    if let Some(track_id) = source_locked_track(sequence, source)? {
        return Err(AudioRoutingEditBlocker::LockedTrack(track_id).into());
    }
    Ok(())
}

fn source_locked_track(
    sequence: &Sequence,
    source: &AudioRouteSource,
) -> Result<Option<TrackId>, AudioRoutingAddressError> {
    let (owner, _) =
        sequence.audio_program.route_source_channel(source).map_err(|_| match source {
            AudioRouteSource::ProcessorOutput { processor_id, .. } => {
                AudioRoutingAddressError::UnknownProcessor(*processor_id)
            }
            AudioRouteSource::Track { track_id, .. } => {
                AudioRoutingAddressError::UnknownTrack(*track_id)
            }
            AudioRouteSource::Bus { bus_id, .. } => AudioRoutingAddressError::UnknownBus(*bus_id),
        })?;
    match owner {
        crate::AudioChannelStripOwner::Track { track_id } => {
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
        crate::AudioChannelStripOwner::Bus { bus_id } => {
            resolve_bus(sequence, bus_id)?;
            Ok(None)
        }
        crate::AudioChannelStripOwner::ProgramOutput { output_id } => {
            Err(AudioRoutingAddressError::UnknownProgramOutput(output_id))
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

fn resolve_sidechain_route(
    sequence: &Sequence,
    route_id: AudioRouteId,
) -> Result<&AudioProcessorSidechainRoute, AudioRoutingAddressError> {
    sequence
        .audio_program
        .sidechain_routes
        .iter()
        .find(|route| route.id == route_id)
        .ok_or(AudioRoutingAddressError::UnknownRoute(route_id))
}

fn resolve_sidechain_route_mut(
    sequence: &mut Sequence,
    route_id: AudioRouteId,
) -> Result<&mut AudioProcessorSidechainRoute, AudioRoutingEditError> {
    sequence
        .audio_program
        .sidechain_routes
        .iter_mut()
        .find(|route| route.id == route_id)
        .ok_or(AudioRoutingAddressError::UnknownRoute(route_id).into())
}

fn resolve_processor(
    sequence: &Sequence,
    processor_id: AudioProcessorInstanceId,
) -> Result<(), AudioRoutingAddressError> {
    let exists = sequence
        .audio_program
        .processing_scopes
        .iter()
        .map(|scope| &scope.processors)
        .chain(
            sequence
                .audio_program
                .track_channels
                .values()
                .flat_map(|channel| [&channel.strip.pre_fader, &channel.strip.post_fader]),
        )
        .chain(
            sequence
                .audio_program
                .buses
                .iter()
                .flat_map(|bus| [&bus.strip.pre_fader, &bus.strip.post_fader]),
        )
        .chain(
            sequence
                .audio_program
                .outputs
                .iter()
                .flat_map(|output| [&output.strip.pre_fader, &output.strip.post_fader]),
        )
        .any(|rack| rack.processors.iter().any(|processor| processor.id == processor_id));
    if exists {
        Ok(())
    } else {
        Err(AudioRoutingAddressError::UnknownProcessor(processor_id))
    }
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

    #[test]
    fn prefader_source_can_feed_same_strip_postrack_without_feedback() {
        use crate::audio::{AudioProcessorInstance, BUILTIN_GAIN_DEFINITION_ID};
        let mut sequence = Sequence::new("Causal same-strip detector");
        let track_id = sequence.audio_tracks[0].id;
        let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        let processor_id = processor.id;
        sequence
            .audio_program
            .track_channels
            .get_mut(&track_id)
            .expect("Track channel")
            .strip
            .post_fader
            .processors
            .push(processor);
        let admission = inspect_audio_route_candidates(&sequence).expect("Valid graph");
        let source = AudioRouteSource::Track {
            track_id,
            port: AudioChannelStripOutputPort::PreFader,
        };
        // PreFader is after the pre rack, before the fader and post rack.
        assert!(admission.allows_processor_sidechain_addition(source.clone(), processor_id));
        for port in [
            AudioChannelStripOutputPort::PostFaderPreMute,
            AudioChannelStripOutputPort::PostMute,
        ] {
            assert!(!admission.allows_processor_sidechain_addition(
                AudioRouteSource::Track { track_id, port },
                processor_id
            ));
        }
        apply_audio_routing_edit(
            &mut sequence,
            &AudioRoutingEditRequest {
                edit: AudioRoutingEdit::CreateProcessorSidechain {
                    source,
                    processor_id,
                    bus_key: "detector".to_owned(),
                },
            },
        )
        .expect("Forward source must enter one author transaction");
        assert_eq!(sequence.audio_program.sidechain_routes.len(), 1);
    }

    #[test]
    fn bulk_bus_dependencies_include_both_racks_disabled_edges_and_source_locks() {
        use crate::audio::{AudioProcessorInstance, BUILTIN_GAIN_DEFINITION_ID};
        let mut sequence = Sequence::new("Bus dependencies");
        let track_id = sequence.audio_tracks[0].id;
        sequence.audio_tracks[0].is_locked = true;
        let pre = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        let post = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        let destination = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        let (pre_id, post_id, destination_id) = (pre.id, post.id, destination.id);
        let first = MixBusId::new();
        let second = MixBusId::new();
        let empty = MixBusId::new();
        let mut first_strip = AudioChannelStrip::default();
        first_strip.pre_fader.processors.push(pre);
        first_strip.post_fader.processors.push(post);
        let mut second_strip = AudioChannelStrip::default();
        second_strip.post_fader.processors.push(destination);
        for (id, strip) in [
            (first, first_strip),
            (second, second_strip),
            (empty, AudioChannelStrip::default()),
        ] {
            sequence
                .audio_program
                .buses
                .push(AudioMixBus { id, name: id.to_string(), strip });
        }
        let detector = AudioRouteSource::Track {
            track_id,
            port: AudioChannelStripOutputPort::PreFader,
        };
        let first_source = AudioRouteSource::Bus {
            bus_id: first,
            port: AudioChannelStripOutputPort::PostMute,
        };
        sequence.audio_program.routes.extend([
            AudioRoute::new(detector.clone(), AudioRouteDestination::Bus(first)),
            AudioRoute::new(first_source.clone(), AudioRouteDestination::Bus(second)),
        ]);
        sequence.audio_program.sidechain_routes.extend([
            AudioProcessorSidechainRoute::new(detector.clone(), pre_id, "detector.pre"),
            AudioProcessorSidechainRoute::new(detector.clone(), post_id, "detector.post"),
        ]);
        let mut retained = AudioProcessorSidechainRoute::new(
            first_source.clone(),
            destination_id,
            "detector.retained",
        );
        retained.enabled = false;
        sequence.audio_program.sidechain_routes.push(retained);
        let inspections = inspect_audio_mix_buses(&sequence).expect("bulk dependencies");
        for (inspection, (id, count, locked)) in
            inspections
                .iter()
                .zip([(first, 5, true), (second, 2, false), (empty, 0, false)])
        {
            assert_eq!(inspection.bus().id, id, "canonical bus order");
            assert_eq!(inspection.connected_route_count(), count);
            assert_eq!(inspection.can_disconnect_and_remove(), !locked);
            assert_eq!(
                inspection,
                &inspect_audio_mix_bus(&sequence, id).expect("single inspection")
            );
        }
        let missing = TrackId::new();
        sequence.audio_program.sidechain_routes[0].source = AudioRouteSource::Track {
            track_id: missing,
            port: AudioChannelStripOutputPort::PreFader,
        };
        assert_eq!(
            inspect_audio_mix_buses(&sequence),
            Err(AudioRoutingAddressError::UnknownTrack(missing))
        );
        assert_eq!(
            inspect_audio_mix_bus(&sequence, first),
            Err(AudioRoutingAddressError::UnknownTrack(missing))
        );
        assert!(
            inspect_audio_mix_bus(&sequence, empty).is_ok(),
            "single-bus inspection only admits its incident edges"
        );
    }
    use mondrian_core::{ExactAutomationKeyframe, TimelineTime};

    fn request(edit: AudioRoutingEdit) -> AudioRoutingEditRequest {
        AudioRoutingEditRequest { edit }
    }

    #[test]
    fn processor_sidechain_creation_and_gain_automation_share_atomic_routing_authority() {
        let mut sequence = Sequence::new("Sidechain authoring");
        let track_id = sequence.audio_tracks[0].id;
        let processor = crate::audio::AudioProcessorInstance::built_in(
            crate::audio::BUILTIN_GAIN_DEFINITION_ID,
            1,
        );
        let processor_id = processor.id;
        sequence.audio_program.outputs[0].strip.pre_fader.processors.push(processor);
        let created = apply_audio_routing_edit(
            &mut sequence,
            &request(AudioRoutingEdit::CreateProcessorSidechain {
                source: AudioRouteSource::Track {
                    track_id,
                    port: AudioChannelStripOutputPort::PostMute,
                },
                processor_id,
                bus_key: "detector".to_owned(),
            }),
        )
        .expect("create sidechain");
        let route_id = created.created_route_id.expect("stable Route identity");
        assert_eq!(
            sequence.audio_program.sidechain_routes[0].processor_id,
            processor_id
        );

        apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest {
                target: AudioAutomationTarget::RouteGain { route_id },
                edit: AudioAutomationEdit::UpsertKeyframe {
                    keyframe: ExactAutomationKeyframe::linear(TimelineTime::ZERO, -6.0),
                },
            },
        )
        .expect("automate sidechain level");
        assert!(sequence.audio_program.sidechain_routes[0]
            .gain_automation
            .as_ref()
            .is_some_and(|curve| curve.keyframes.len() == 1));

        apply_audio_routing_edit(
            &mut sequence,
            &request(AudioRoutingEdit::RemoveProcessorSidechain { route_id }),
        )
        .expect("remove sidechain");
        assert!(sequence.audio_program.sidechain_routes.is_empty());
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
    fn complete_candidate_matrix_agrees_with_atomic_author_admission() {
        use crate::audio::{
            AudioComponentEdit, AudioProcessingScope, AudioProcessorInstance,
            BUILTIN_GAIN_DEFINITION_ID,
        };
        use crate::clip::Clip;
        use mondrian_core::{AssetId, AudioSourceComponentId};
        let mut sequence = Sequence::new("Complete routing candidates");
        sequence.add_audio_track();
        let tracks = sequence.audio_tracks.iter().map(|track| track.id).collect::<Vec<_>>();
        let mut shared = AudioProcessingScope::identity();
        let mut unused = AudioProcessingScope::identity();
        let mut processors = Vec::new();
        for rack in [&mut shared.processors, &mut unused.processors] {
            let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
            processors.push(processor.id);
            rack.processors.push(processor);
        }
        for track in &mut sequence.audio_tracks {
            let mut clip = Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE)
                .expect("One second audio Clip");
            clip.audio_components.push(AudioComponentEdit::media(
                AudioSourceComponentId::primary(),
                shared.id,
            ));
            track.add_clip(clip).expect("Scope occurrence");
        }
        sequence.audio_program.add_processing_scope(shared);
        sequence.audio_program.add_processing_scope(unused);
        let buses = [MixBusId::new(), MixBusId::new()];
        for (index, id) in buses.into_iter().enumerate() {
            sequence.audio_program.buses.push(AudioMixBus {
                id,
                name: format!("Bus {index}"),
                strip: AudioChannelStrip::default(),
            });
        }
        for strip in sequence
            .audio_program
            .track_channels
            .values_mut()
            .map(|channel| &mut channel.strip)
            .chain(sequence.audio_program.buses.iter_mut().map(|bus| &mut bus.strip))
            .chain(sequence.audio_program.outputs.iter_mut().map(|output| &mut output.strip))
        {
            for rack in [&mut strip.pre_fader, &mut strip.post_fader] {
                let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
                processors.push(processor.id);
                rack.processors.push(processor);
            }
        }
        let source = |bus_id| AudioRouteSource::Bus {
            bus_id,
            port: AudioChannelStripOutputPort::PostMute,
        };
        sequence.audio_program.routes.extend([
            AudioRoute::new(
                AudioRouteSource::Track {
                    track_id: tracks[0],
                    port: AudioChannelStripOutputPort::PostMute,
                },
                AudioRouteDestination::Bus(buses[0]),
            ),
            AudioRoute::new(source(buses[0]), AudioRouteDestination::Bus(buses[1])),
        ]);
        let detector = sequence
            .audio_program
            .track_channels
            .get(&tracks[1])
            .expect("Second Track")
            .strip
            .pre_fader
            .processors[0]
            .id;
        let mut retained =
            AudioProcessorSidechainRoute::new(source(buses[1]), detector, "detector");
        retained.enabled = false;
        sequence.audio_program.sidechain_routes.push(retained);
        sequence
            .audio_program
            .validate(
                &sequence.audio_tracks,
                &sequence.audio_roles,
                sequence.settings.audio_channel_layout,
            )
            .expect("Valid complete graph");
        let destinations = buses
            .into_iter()
            .map(AudioRouteDestination::Bus)
            .chain(
                sequence
                    .audio_program
                    .outputs
                    .iter()
                    .map(|output| AudioRouteDestination::Output(output.id)),
            )
            .collect::<Vec<_>>();
        for locked in [false, true] {
            sequence.audio_tracks[0].is_locked = locked;
            let before = sequence.clone();
            let candidates =
                inspect_audio_route_candidates(&sequence).expect("Complete projection");
            for port in [
                AudioChannelStripOutputPort::PreFader,
                AudioChannelStripOutputPort::PostFaderPreMute,
                AudioChannelStripOutputPort::PostMute,
            ] {
                let sources = tracks
                    .iter()
                    .map(|track_id| AudioRouteSource::Track { track_id: *track_id, port })
                    .chain(buses.into_iter().map(|bus_id| AudioRouteSource::Bus { bus_id, port }));
                for source in sources {
                    for processor_id in &processors {
                        let mut candidate = sequence.clone();
                        let accepted = apply_audio_routing_edit(
                            &mut candidate,
                            &request(AudioRoutingEdit::CreateProcessorSidechain {
                                source: source.clone(),
                                processor_id: *processor_id,
                                bus_key: "detector".to_owned(),
                            }),
                        )
                        .is_ok();
                        assert_eq!(
                            candidates
                                .allows_processor_sidechain_addition(source.clone(), *processor_id),
                            accepted,
                            "processor={processor_id:?} source={source:?} locked={locked}"
                        );
                        if !accepted {
                            assert_eq!(candidate, before);
                        }
                    }
                    for destination in &destinations {
                        let mut candidate = sequence.clone();
                        let accepted = apply_audio_routing_edit(
                            &mut candidate,
                            &request(AudioRoutingEdit::CreateRoute {
                                source: source.clone(),
                                destination: *destination,
                            }),
                        )
                        .is_ok();
                        assert_eq!(
                            candidates.allows_addition(source.clone(), *destination),
                            accepted,
                            "source={source:?} destination={destination:?} locked={locked}"
                        );
                        if !accepted {
                            assert_eq!(candidate, before);
                        }
                    }
                    assert!(!candidates.allows_processor_sidechain_addition(
                        source,
                        AudioProcessorInstanceId::new()
                    ));
                }
            }
            assert!(!candidates
                .allows_processor_sidechain_addition(source(MixBusId::new()), processors[0]));
            assert_eq!(
                sequence, before,
                "Read-only projection preserves author state"
            );
        }
        let before = sequence.clone();
        sequence.audio_program.sidechain_routes[0].processor_id = AudioProcessorInstanceId::new();
        assert!(matches!(
            inspect_audio_route_candidates(&sequence),
            Err(AudioRoutingAddressError::UnknownProcessor(_))
        ));
        sequence = before;
        sequence.audio_program.sidechain_routes[0].source = source(MixBusId::new());
        assert!(matches!(
            inspect_audio_route_candidates(&sequence),
            Err(AudioRoutingAddressError::UnknownBus(_))
        ));
    }

    #[test]
    fn route_candidates_refuse_feedback_through_retained_sidechains() {
        use crate::audio::{AudioProcessorInstance, BUILTIN_GAIN_DEFINITION_ID};
        for enabled in [false, true] {
            let mut sequence = Sequence::new("Sidechain candidate feedback");
            let track_id = sequence.audio_tracks[0].id;
            let bus_id = MixBusId::new();
            sequence.audio_program.buses.push(AudioMixBus {
                id: bus_id,
                name: "Detector source".to_owned(),
                strip: AudioChannelStrip::default(),
            });
            let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
            let processor_id = processor.id;
            sequence
                .audio_program
                .track_channels
                .get_mut(&track_id)
                .expect("Track channel")
                .strip
                .post_fader
                .processors
                .push(processor);
            let source = AudioRouteSource::Bus {
                bus_id,
                port: AudioChannelStripOutputPort::PreFader,
            };
            let mut sidechain =
                AudioProcessorSidechainRoute::new(source.clone(), processor_id, "detector");
            sidechain.enabled = enabled;
            sequence.audio_program.sidechain_routes.push(sidechain);
            let candidate_source = AudioRouteSource::Track {
                track_id,
                port: AudioChannelStripOutputPort::PostMute,
            };
            let destination = AudioRouteDestination::Bus(bus_id);
            let before = sequence.clone();
            let candidates = inspect_audio_route_candidates(&sequence).expect("Candidate snapshot");
            assert!(
                !candidates.allows_addition(candidate_source.clone(), destination),
                "retained sidechain enabled={enabled} must participate in candidate cycles"
            );
            assert_eq!(sequence, before, "Inspection must not mutate authors");
            assert_eq!(
                apply_audio_routing_edit(
                    &mut sequence,
                    &request(AudioRoutingEdit::CreateRoute {
                        source: candidate_source,
                        destination,
                    })
                ),
                Err(AudioRoutingEditError::AuthorState(
                    AudioAuthoringError::RouteCycle
                ))
            );
            assert_eq!(sequence, before, "Refused transaction must be atomic");
        }
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
