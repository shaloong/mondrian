//! Canonical recursive dependency closure for one prepared visual range.
//!
//! [`crate::PreparedVisualProgram`] owns one Sequence revision's interval and
//! Effect interpretation. This Module composes those immutable Programs across
//! nested Sequence ranges so Preview lookahead and Export capture share one
//! recursion, cycle/depth, Transition, retime, temporal-extent, and media
//! reachability implementation.

use crate::{
    BasicTitleFontQuery, PreparedVisualNestedRange, PreparedVisualProgram,
    PreparedVisualProgramBinding,
};
use mondrian_core::{
    AssetId, FramePosition, SequenceId, SequenceRevision, TimelineTime, VideoTransitionId,
};
use mondrian_timeline::{sequence::MAX_NESTED_SEQUENCE_RENDER_DEPTH, Sequence};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// Immutable recursive dependency closure for one inclusive root range.
#[derive(Debug, Clone)]
pub struct PreparedVisualRangeClosure {
    root_sequence_id: SequenceId,
    effect_registry_revision: u64,
    sequence_ids: BTreeSet<SequenceId>,
    programs: BTreeMap<SequenceId, Arc<PreparedVisualProgram>>,
    media_asset_ids: BTreeSet<AssetId>,
    transition_ids: BTreeMap<SequenceId, BTreeSet<VideoTransitionId>>,
    basic_title_font_queries: BTreeSet<BasicTitleFontQuery>,
}

impl PreparedVisualRangeClosure {
    /// Root Sequence whose requested range produced this closure.
    pub const fn root_sequence_id(&self) -> SequenceId {
        self.root_sequence_id
    }

    /// Exact Effect-definition registry revision shared by every Program.
    pub const fn effect_registry_revision(&self) -> u64 {
        self.effect_registry_revision
    }

    /// Root and selected nested Sequence identities.
    pub fn sequence_ids(&self) -> &BTreeSet<SequenceId> {
        &self.sequence_ids
    }

    /// Exact immutable Program selected for one Sequence identity.
    pub fn program(&self, sequence_id: SequenceId) -> Option<&Arc<PreparedVisualProgram>> {
        self.programs.get(&sequence_id)
    }

    /// Programs selected by this closure in stable Sequence-identity order.
    pub fn programs(
        &self,
    ) -> impl ExactSizeIterator<Item = (&SequenceId, &Arc<PreparedVisualProgram>)> {
        self.programs.iter()
    }

    /// File-backed picture media identities reachable in the requested range.
    pub fn media_asset_ids(&self) -> &BTreeSet<AssetId> {
        &self.media_asset_ids
    }

    /// Selected Transition identities grouped by their owning Sequence.
    pub fn transition_ids(&self) -> &BTreeMap<SequenceId, BTreeSet<VideoTransitionId>> {
        &self.transition_ids
    }

    /// Static Basic Title font queries reachable in this closure.
    pub fn basic_title_font_queries(&self) -> &BTreeSet<BasicTitleFontQuery> {
        &self.basic_title_font_queries
    }

    /// Whether at least one file-backed picture source can contribute.
    pub fn has_media_dependencies(&self) -> bool {
        !self.media_asset_ids.is_empty()
    }
}

/// Failure to prepare one recursive visual range dependency closure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PreparedVisualRangeClosureError {
    /// Candidate input contains the same Sequence identity more than once.
    #[error("visual range candidates contain duplicate Sequence {sequence_id}")]
    DuplicateSequenceIdentity {
        /// Repeated identity.
        sequence_id: SequenceId,
    },
    /// A selected nested placement has no immutable Sequence candidate.
    #[error(
        "Sequence {parent_sequence_id} visual range references missing nested Sequence {nested_sequence_id}"
    )]
    MissingNestedSequence {
        /// Sequence that owns the placement.
        parent_sequence_id: SequenceId,
        /// Missing child identity.
        nested_sequence_id: SequenceId,
    },
    /// Selected nested placements form a recursive graph.
    #[error("visual range closure contains a nested Sequence cycle: {path:?}")]
    NestedCycle {
        /// Closed cycle path, including the repeated final identity.
        path: Vec<SequenceId>,
    },
    /// Selected nesting exceeds the shared renderer contract.
    #[error("visual range closure exceeds nested depth {maximum} at Sequence {sequence_id}")]
    NestedDepthExceeded {
        /// Sequence that crossed the limit.
        sequence_id: SequenceId,
        /// Shared maximum nesting depth.
        maximum: usize,
    },
    /// Whole-Sequence range resolution failed.
    #[error("failed to resolve visual range for Sequence {sequence_id}: {reason}")]
    Range {
        /// Sequence whose range failed.
        sequence_id: SequenceId,
        /// Exact lower-level diagnostic.
        reason: String,
    },
    /// One immutable Program could not be prepared.
    #[error("failed to prepare visual Program for Sequence {sequence_id}: {reason}")]
    Program {
        /// Sequence whose Program failed.
        sequence_id: SequenceId,
        /// Exact lower-level diagnostic.
        reason: String,
    },
    /// A resolver returned a Program for another author snapshot.
    #[error(
        "visual Program identity mismatch for Sequence {expected_sequence_id}: expected revision {expected_revision:?}, observed Sequence {actual_sequence_id} revision {actual_revision:?}"
    )]
    ProgramIdentityMismatch {
        /// Requested Sequence.
        expected_sequence_id: SequenceId,
        /// Requested author revision.
        expected_revision: SequenceRevision,
        /// Returned Program Sequence.
        actual_sequence_id: SequenceId,
        /// Returned Program revision.
        actual_revision: SequenceRevision,
    },
    /// The candidate Sequence visual projection could not be fingerprinted.
    #[error("failed to fingerprint visual author state for Sequence {sequence_id}: {reason}")]
    AuthorFingerprint {
        /// Sequence whose projection failed.
        sequence_id: SequenceId,
        /// Canonical encoding diagnostic.
        reason: String,
    },
    /// A resolver returned a Program prepared from different visual author
    /// content despite matching identity/revision metadata.
    #[error(
        "visual Program author fingerprint mismatch for Sequence {sequence_id}: expected {expected:?}, observed {actual:?}"
    )]
    ProgramAuthorFingerprintMismatch {
        /// Sequence whose frozen author state did not match.
        sequence_id: SequenceId,
        /// Fingerprint recomputed from the supplied immutable author snapshot.
        expected: [u8; 32],
        /// Fingerprint bound into the prepared Program.
        actual: [u8; 32],
    },
    /// One closure attempted to combine Programs prepared from different
    /// definition registries.
    #[error(
        "visual range Programs span Effect registry revisions {expected} and {actual} at Sequence {sequence_id}"
    )]
    EffectRegistryRevisionMismatch {
        /// Revision pinned by the first Program.
        expected: u64,
        /// Revision observed on the conflicting Program.
        actual: u64,
        /// Sequence whose Program conflicted.
        sequence_id: SequenceId,
    },
    /// A prepared Program rejected the exact selected range.
    #[error("visual dependency query failed for Sequence {sequence_id}: {reason}")]
    Dependency {
        /// Sequence whose dependency query failed.
        sequence_id: SequenceId,
        /// Exact lower-level diagnostic.
        reason: String,
    },
    /// Preview lookahead supplied a negative root frame.
    #[error("visual media lookahead requires nonnegative frames, observed {frame}")]
    NegativeFrame {
        /// Invalid frame.
        frame: i64,
    },
    /// Root frame/time projection failed.
    #[error("failed to project root frame {frame} into Timeline Time: {reason}")]
    FrameTimeProjection {
        /// Root frame.
        frame: i64,
        /// Exact lower-level diagnostic.
        reason: String,
    },
}

/// Prepare the sole recursive visual dependency closure for one inclusive
/// root Sequence range.
///
/// `prepare_program` may use a consumer-owned bounded cache or a frozen Export
/// Program bundle. The closure pins the first observed Effect registry
/// revision and rejects a mixed set atomically.
pub fn prepare_visual_range_closure(
    root_sequence: &Sequence,
    sequences: &[Sequence],
    root_range: PreparedVisualNestedRange,
    mut prepare_program: impl FnMut(&Sequence) -> Result<Arc<PreparedVisualProgram>, String>,
) -> Result<PreparedVisualRangeClosure, PreparedVisualRangeClosureError> {
    prepare_bound_visual_range_closure(root_sequence, sequences, root_range, |sequence| {
        let program = prepare_program(sequence)?;
        PreparedVisualProgramBinding::checked(sequence, program).map_err(|error| error.to_string())
    })
}

/// Prepare one recursive dependency closure from Programs already certified
/// against an immutable author snapshot.
pub fn prepare_bound_visual_range_closure(
    root_sequence: &Sequence,
    sequences: &[Sequence],
    root_range: PreparedVisualNestedRange,
    prepare_program: impl FnMut(&Sequence) -> Result<PreparedVisualProgramBinding, String>,
) -> Result<PreparedVisualRangeClosure, PreparedVisualRangeClosureError> {
    let mut programs = PinnedProgramResolver::new(prepare_program);
    prepare_visual_range_closure_with_resolver(root_sequence, sequences, root_range, &mut programs)
}

/// Find the earliest root frame after `after_frame` and no later than
/// `horizon_frame` whose canonical recursive visual closure can demand
/// file-backed picture media.
///
/// Prefix queries use the same prepared range closure as Export. A binary
/// search therefore handles Transition endpoints, nested blank leaders,
/// retime, and finite temporal extent without scanning author Tracks/Clips or
/// enumerating every frame in the lookahead window.
pub fn next_prepared_visual_media_demand_frame(
    root_sequence: &Sequence,
    sequences: &[Sequence],
    after_frame: i64,
    horizon_frame: i64,
    mut prepare_program: impl FnMut(&Sequence) -> Result<Arc<PreparedVisualProgram>, String>,
) -> Result<Option<i64>, PreparedVisualRangeClosureError> {
    next_bound_prepared_visual_media_demand_frame(
        root_sequence,
        sequences,
        after_frame,
        horizon_frame,
        |sequence| {
            let program = prepare_program(sequence)?;
            PreparedVisualProgramBinding::checked(sequence, program)
                .map_err(|error| error.to_string())
        },
    )
}

/// Find the earliest media-demand frame using author-certified Program
/// bindings without reserializing the Sequence on prefix queries.
pub fn next_bound_prepared_visual_media_demand_frame(
    root_sequence: &Sequence,
    sequences: &[Sequence],
    after_frame: i64,
    horizon_frame: i64,
    prepare_program: impl FnMut(&Sequence) -> Result<PreparedVisualProgramBinding, String>,
) -> Result<Option<i64>, PreparedVisualRangeClosureError> {
    if after_frame < 0 {
        return Err(PreparedVisualRangeClosureError::NegativeFrame { frame: after_frame });
    }
    if horizon_frame < 0 {
        return Err(PreparedVisualRangeClosureError::NegativeFrame { frame: horizon_frame });
    }
    let Some(first_frame) = after_frame.checked_add(1) else {
        return Ok(None);
    };
    if first_frame > horizon_frame {
        return Ok(None);
    }

    let first_time = root_frame_time(root_sequence, first_frame)?;
    let mut programs = PinnedProgramResolver::new(prepare_program);
    let mut low = first_frame;
    let mut high = horizon_frame;
    if !prefix_has_media(root_sequence, sequences, first_time, high, &mut programs)? {
        return Ok(None);
    }

    while low < high {
        let middle = frame_midpoint(low, high);
        if prefix_has_media(root_sequence, sequences, first_time, middle, &mut programs)? {
            high = middle;
        } else {
            low = middle.saturating_add(1);
        }
    }
    Ok(Some(low))
}

fn prefix_has_media<Prepare>(
    root_sequence: &Sequence,
    sequences: &[Sequence],
    first_time: TimelineTime,
    last_frame: i64,
    programs: &mut PinnedProgramResolver<Prepare>,
) -> Result<bool, PreparedVisualRangeClosureError>
where
    Prepare: FnMut(&Sequence) -> Result<PreparedVisualProgramBinding, String>,
{
    let last_time = root_frame_time(root_sequence, last_frame)?;
    let closure = prepare_visual_range_closure_with_resolver(
        root_sequence,
        sequences,
        PreparedVisualNestedRange::Bounded { first: first_time, last: last_time },
        programs,
    )?;
    Ok(closure.has_media_dependencies())
}

fn root_frame_time(
    root_sequence: &Sequence,
    frame: i64,
) -> Result<TimelineTime, PreparedVisualRangeClosureError> {
    TimelineTime::from_frame_position(FramePosition::new(frame, root_sequence.time_base())).map_err(
        |error| PreparedVisualRangeClosureError::FrameTimeProjection {
            frame,
            reason: error.to_string(),
        },
    )
}

fn frame_midpoint(low: i64, high: i64) -> i64 {
    low + (high - low) / 2
}

fn prepare_visual_range_closure_with_resolver<Prepare>(
    root_sequence: &Sequence,
    sequences: &[Sequence],
    root_range: PreparedVisualNestedRange,
    programs: &mut PinnedProgramResolver<Prepare>,
) -> Result<PreparedVisualRangeClosure, PreparedVisualRangeClosureError>
where
    Prepare: FnMut(&Sequence) -> Result<PreparedVisualProgramBinding, String>,
{
    let sequence_index = sequence_index(root_sequence, sequences)?;
    let mut builder = PreparedVisualRangeClosureBuilder {
        sequence_index,
        programs,
        active_path: Vec::new(),
        selected_sequence_ids: BTreeSet::new(),
        media_asset_ids: BTreeSet::new(),
        transition_ids: BTreeMap::new(),
        basic_title_font_queries: BTreeSet::new(),
    };
    builder.visit(root_sequence, root_range, 0)?;
    let effect_registry_revision =
        builder.programs.effect_registry_revision().ok_or_else(|| {
            PreparedVisualRangeClosureError::Program {
                sequence_id: root_sequence.id,
                reason: "recursive closure completed without a root visual Program".to_owned(),
            }
        })?;
    let selected_sequence_ids = builder.selected_sequence_ids;
    let mut selected_programs = BTreeMap::new();
    for sequence_id in &selected_sequence_ids {
        let program = builder.programs.program(*sequence_id).ok_or_else(|| {
            PreparedVisualRangeClosureError::Program {
                sequence_id: *sequence_id,
                reason: "selected recursive Sequence has no pinned visual Program".to_owned(),
            }
        })?;
        selected_programs.insert(*sequence_id, Arc::clone(program));
    }
    Ok(PreparedVisualRangeClosure {
        root_sequence_id: root_sequence.id,
        effect_registry_revision,
        sequence_ids: selected_sequence_ids,
        programs: selected_programs,
        media_asset_ids: builder.media_asset_ids,
        transition_ids: builder.transition_ids,
        basic_title_font_queries: builder.basic_title_font_queries,
    })
}

fn sequence_index<'a>(
    root_sequence: &'a Sequence,
    sequences: &'a [Sequence],
) -> Result<HashMap<SequenceId, &'a Sequence>, PreparedVisualRangeClosureError> {
    let mut index = HashMap::with_capacity(sequences.len().saturating_add(1));
    for sequence in sequences {
        if sequence.id == root_sequence.id && sequence != root_sequence {
            return Err(PreparedVisualRangeClosureError::DuplicateSequenceIdentity {
                sequence_id: sequence.id,
            });
        }
        let canonical = if sequence.id == root_sequence.id {
            root_sequence
        } else {
            sequence
        };
        if index.insert(sequence.id, canonical).is_some() {
            return Err(PreparedVisualRangeClosureError::DuplicateSequenceIdentity {
                sequence_id: sequence.id,
            });
        }
    }
    index.entry(root_sequence.id).or_insert(root_sequence);
    Ok(index)
}

struct PinnedProgramResolver<Prepare> {
    prepare: Prepare,
    effect_registry_revision: Option<u64>,
    programs: BTreeMap<SequenceId, Arc<PreparedVisualProgram>>,
}

impl<Prepare> PinnedProgramResolver<Prepare>
where
    Prepare: FnMut(&Sequence) -> Result<PreparedVisualProgramBinding, String>,
{
    fn new(prepare: Prepare) -> Self {
        Self {
            prepare,
            effect_registry_revision: None,
            programs: BTreeMap::new(),
        }
    }

    fn resolve(
        &mut self,
        sequence: &Sequence,
    ) -> Result<Arc<PreparedVisualProgram>, PreparedVisualRangeClosureError> {
        if let Some(program) = self.programs.get(&sequence.id) {
            if program.sequence_revision() != sequence.revision {
                return Err(PreparedVisualRangeClosureError::ProgramIdentityMismatch {
                    expected_sequence_id: sequence.id,
                    expected_revision: sequence.revision,
                    actual_sequence_id: program.sequence_id(),
                    actual_revision: program.sequence_revision(),
                });
            }
            return Ok(Arc::clone(program));
        }
        let binding = (self.prepare)(sequence).map_err(|reason| {
            PreparedVisualRangeClosureError::Program { sequence_id: sequence.id, reason }
        })?;
        binding.validate_for_sequence(sequence).map_err(|error| {
            PreparedVisualRangeClosureError::Program {
                sequence_id: sequence.id,
                reason: error.to_string(),
            }
        })?;
        let program = Arc::clone(binding.program());
        if program.sequence_id() != sequence.id || program.sequence_revision() != sequence.revision
        {
            return Err(PreparedVisualRangeClosureError::ProgramIdentityMismatch {
                expected_sequence_id: sequence.id,
                expected_revision: sequence.revision,
                actual_sequence_id: program.sequence_id(),
                actual_revision: program.sequence_revision(),
            });
        }
        let revision = program.effect_registry_revision();
        if let Some(expected) = self.effect_registry_revision {
            if revision != expected {
                return Err(
                    PreparedVisualRangeClosureError::EffectRegistryRevisionMismatch {
                        expected,
                        actual: revision,
                        sequence_id: sequence.id,
                    },
                );
            }
        } else {
            self.effect_registry_revision = Some(revision);
        }
        self.programs.insert(sequence.id, Arc::clone(&program));
        Ok(program)
    }

    const fn effect_registry_revision(&self) -> Option<u64> {
        self.effect_registry_revision
    }

    fn program(&self, sequence_id: SequenceId) -> Option<&Arc<PreparedVisualProgram>> {
        self.programs.get(&sequence_id)
    }
}

struct PreparedVisualRangeClosureBuilder<'a, 'programs, Prepare> {
    sequence_index: HashMap<SequenceId, &'a Sequence>,
    programs: &'programs mut PinnedProgramResolver<Prepare>,
    active_path: Vec<SequenceId>,
    selected_sequence_ids: BTreeSet<SequenceId>,
    media_asset_ids: BTreeSet<AssetId>,
    transition_ids: BTreeMap<SequenceId, BTreeSet<VideoTransitionId>>,
    basic_title_font_queries: BTreeSet<BasicTitleFontQuery>,
}

impl<Prepare> PreparedVisualRangeClosureBuilder<'_, '_, Prepare>
where
    Prepare: FnMut(&Sequence) -> Result<PreparedVisualProgramBinding, String>,
{
    fn visit(
        &mut self,
        sequence: &Sequence,
        requested_range: PreparedVisualNestedRange,
        depth: usize,
    ) -> Result<(), PreparedVisualRangeClosureError> {
        if depth > MAX_NESTED_SEQUENCE_RENDER_DEPTH {
            return Err(PreparedVisualRangeClosureError::NestedDepthExceeded {
                sequence_id: sequence.id,
                maximum: MAX_NESTED_SEQUENCE_RENDER_DEPTH,
            });
        }
        if let Some(cycle_start) =
            self.active_path.iter().position(|candidate| *candidate == sequence.id)
        {
            let mut path = self.active_path[cycle_start..].to_vec();
            path.push(sequence.id);
            return Err(PreparedVisualRangeClosureError::NestedCycle { path });
        }
        self.active_path.push(sequence.id);
        let result = (|| {
            self.selected_sequence_ids.insert(sequence.id);
            let program = self.programs.resolve(sequence)?;
            let (first, last) = visual_range_bounds(sequence, requested_range)?;
            let reachability =
                program.preflight_range_dependencies(first, last).map_err(|error| {
                    PreparedVisualRangeClosureError::Dependency {
                        sequence_id: sequence.id,
                        reason: error.to_string(),
                    }
                })?;
            self.media_asset_ids.extend(reachability.media_asset_ids);
            if !reachability.transition_ids.is_empty() {
                self.transition_ids
                    .entry(sequence.id)
                    .or_default()
                    .extend(reachability.transition_ids);
            }
            self.basic_title_font_queries.extend(reachability.basic_title_font_queries);
            for demand in reachability.nested_demands {
                let child = self.sequence_index.get(&demand.sequence_id).copied().ok_or(
                    PreparedVisualRangeClosureError::MissingNestedSequence {
                        parent_sequence_id: sequence.id,
                        nested_sequence_id: demand.sequence_id,
                    },
                )?;
                self.visit(child, demand.range, depth.saturating_add(1))?;
            }
            Ok(())
        })();
        self.active_path.pop();
        result
    }
}

fn visual_range_bounds(
    sequence: &Sequence,
    range: PreparedVisualNestedRange,
) -> Result<(TimelineTime, TimelineTime), PreparedVisualRangeClosureError> {
    match range {
        PreparedVisualNestedRange::Bounded { first, last } => Ok((first, last)),
        PreparedVisualNestedRange::WholeSequence => Ok((
            TimelineTime::ZERO,
            sequence
                .total_duration()
                .map_err(|error| PreparedVisualRangeClosureError::Range {
                    sequence_id: sequence.id,
                    reason: error.to_string(),
                })?,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{BasicTitleFontStyle, Color, Rational, TimelineTimeRange};
    use mondrian_timeline::{Clip, Track, VideoTransition};

    fn tt(frame: i64, rate: Rational) -> TimelineTime {
        TimelineTime::from_frame_position(FramePosition::new(frame, rate)).expect("test time")
    }

    fn prepare(sequence: &Sequence) -> Result<Arc<PreparedVisualProgram>, String> {
        for _ in 0..32 {
            match PreparedVisualProgram::prepare(sequence) {
                Ok(program) => return Ok(Arc::new(program)),
                Err(crate::PreparedVisualProgramError::EffectRegistryChanged { .. }) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        Err("Effect registry did not stabilize during test".to_owned())
    }

    fn stable_range_closure(
        root: &Sequence,
        sequences: &[Sequence],
        range: PreparedVisualNestedRange,
    ) -> PreparedVisualRangeClosure {
        for _ in 0..64 {
            match prepare_visual_range_closure(root, sequences, range, prepare) {
                Ok(closure) => return closure,
                Err(PreparedVisualRangeClosureError::EffectRegistryRevisionMismatch { .. }) => {}
                Err(error) => panic!("range closure: {error}"),
            }
        }
        panic!("Effect registry did not stabilize during range-closure test")
    }

    fn stable_next_media_frame(
        root: &Sequence,
        sequences: &[Sequence],
        after: i64,
        horizon: i64,
    ) -> Option<i64> {
        for _ in 0..64 {
            match next_prepared_visual_media_demand_frame(root, sequences, after, horizon, prepare)
            {
                Ok(frame) => return frame,
                Err(PreparedVisualRangeClosureError::EffectRegistryRevisionMismatch { .. }) => {}
                Err(error) => panic!("media activation: {error}"),
            }
        }
        panic!("Effect registry did not stabilize during media-activation test")
    }

    #[test]
    fn range_closure_collects_only_selected_nested_media_and_programs() {
        let mut child = Sequence::new("child");
        child.video_tracks.clear();
        let rate = child.time_base();
        let selected_asset = AssetId::new();
        let mut child_track = Track::new_video("child V1");
        child_track
            .add_clip(Clip::new(selected_asset, tt(6, rate), tt(4, rate)).expect("child media"))
            .expect("add child media");
        child.video_tracks.push(child_track);

        let mut root = Sequence::new("root");
        root.video_tracks.clear();
        let mut root_track = Track::new_video("root V1");
        root_track
            .add_clip(
                Clip::new_nested_sequence(
                    child.id,
                    TimelineTime::ZERO,
                    tt(20, rate),
                    Some("child".to_owned()),
                )
                .expect("nested"),
            )
            .expect("add nested");
        root_track
            .add_clip(
                Clip::new_basic_title(
                    "selected title",
                    "Mondrian Test Face",
                    tt(2, rate),
                    tt(3, rate),
                )
                .expect("title"),
            )
            .expect("add title");
        let off_range_asset = AssetId::new();
        root_track
            .add_clip(
                Clip::new(off_range_asset, tt(40, rate), tt(5, rate)).expect("off-range media"),
            )
            .expect("add off-range");
        root.video_tracks.push(root_track);

        let closure = stable_range_closure(
            &root,
            &[child.clone()],
            PreparedVisualNestedRange::Bounded { first: TimelineTime::ZERO, last: tt(20, rate) },
        );

        assert_eq!(closure.sequence_ids(), &BTreeSet::from([root.id, child.id]));
        assert_eq!(closure.media_asset_ids(), &BTreeSet::from([selected_asset]));
        assert!(closure.program(root.id).is_some());
        assert!(closure.program(child.id).is_some());
        assert!(!closure.media_asset_ids().contains(&off_range_asset));
        assert_eq!(
            closure.basic_title_font_queries(),
            &BTreeSet::from([BasicTitleFontQuery {
                family: "Mondrian Test Face".to_owned(),
                weight: 400,
                style: BasicTitleFontStyle::Normal,
            }])
        );
    }

    #[test]
    fn next_media_frame_projects_through_a_nested_blank_leader() {
        let mut child = Sequence::new("child with leader");
        child.video_tracks.clear();
        let rate = child.time_base();
        let asset_id = AssetId::new();
        let mut child_track = Track::new_video("child V1");
        child_track
            .add_clip(Clip::new(asset_id, tt(12, rate), tt(5, rate)).expect("child media"))
            .expect("add child media");
        child.video_tracks.push(child_track);

        let mut root = Sequence::new("root");
        root.video_tracks.clear();
        let mut track = Track::new_video("V1");
        track
            .add_clip(
                Clip::new_nested_sequence(
                    child.id,
                    TimelineTime::ZERO,
                    tt(30, rate),
                    Some("child".to_owned()),
                )
                .expect("nested"),
            )
            .expect("add nested");
        root.video_tracks.push(track);

        assert_eq!(stable_next_media_frame(&root, &[child], 0, 20), Some(12));
    }

    #[test]
    fn next_media_frame_observes_transition_endpoint_activation() {
        let mut sequence = Sequence::new("transition activation");
        sequence.video_tracks.clear();
        let rate = sequence.time_base();
        let mut track = Track::new_video("V1");
        let left = Clip::new_solid_color(
            AssetId::new(),
            Color::BLACK,
            TimelineTime::ZERO,
            tt(10, rate),
        )
        .expect("left");
        let left_id = left.id;
        let right = Clip::new(AssetId::new(), tt(10, rate), tt(10, rate)).expect("right media");
        let right_id = right.id;
        track.add_clip(left).expect("add left");
        track.add_clip(right).expect("add right");
        sequence.video_tracks.push(track);
        sequence.video_transitions.push(VideoTransition::cross_dissolve(
            left_id,
            right_id,
            TimelineTimeRange::new(tt(8, rate), tt(4, rate)).expect("transition range"),
        ));

        assert_eq!(stable_next_media_frame(&sequence, &[], 0, 20), Some(8));
    }

    #[test]
    fn frozen_program_cannot_authorize_changed_same_revision_content() {
        let mut original = Sequence::new("original");
        original.video_tracks.clear();
        original.video_tracks.push(Track::new_video("V1"));
        let frozen = Arc::new(prepare(&original).expect("frozen Program"));

        let mut changed = original.clone();
        changed.video_tracks[0].is_visible = false;
        assert_eq!(changed.revision, original.revision);
        let error = prepare_visual_range_closure(
            &changed,
            &[],
            PreparedVisualNestedRange::WholeSequence,
            |_| Ok(Arc::clone(&frozen)),
        )
        .expect_err("same metadata cannot replace author fingerprint");

        assert!(matches!(
            error,
            PreparedVisualRangeClosureError::Program { reason, .. }
                if reason.contains("fingerprint")
        ));
    }
}
