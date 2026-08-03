//! Immutable, revision-bound visual execution schedule.
//!
//! Authoring keeps `Track -> Clip` placement as the sole source of truth. This
//! module compiles that validated state into interval indexes so repeated
//! Preview and Export evaluation does not scan every placement on every frame.

use crate::sequence::{
    flat_video_transition_definition_snapshot, flatten_visual_clip_with_effect_snapshots,
    flatten_visual_transition_with_effect_snapshots, validate_video_transition, Sequence,
};
use crate::track::Track;
use crate::video_transition::VideoTransition;
use mondrian_core::timeline_data::{
    ClipContent, FlatVideoTransitionDefinitionSnapshot, FlatVisualItem, RenderPlanSource,
    TimelineClipEndpointContext, TimelineClipExecutionRef,
};
use mondrian_core::{
    ClipId, MondrianError, Rational, Result, SequenceId, SequenceRevision, TimelineTime,
    TimelineTimeRange, VideoTransitionId, WorkingColorSpace,
};
use std::collections::HashMap;
use std::sync::Arc;

/// Stable identity of one prepared visual author snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PreparedVisualScheduleKey {
    sequence_id: SequenceId,
    revision: SequenceRevision,
}

impl PreparedVisualScheduleKey {
    const fn for_sequence(sequence: &Sequence) -> Self {
        Self {
            sequence_id: sequence.id,
            revision: sequence.revision,
        }
    }
}

/// Static facts recorded when one Sequence visual schedule is prepared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedVisualScheduleDiagnostics {
    /// Sequence whose author state was compiled.
    pub sequence_id: SequenceId,
    /// Exact conservative author revision compiled by this schedule.
    pub revision: SequenceRevision,
    /// Video Tracks retained in authored order.
    pub video_tracks: usize,
    /// Enabled Clip placement intervals admitted to the schedule.
    pub clip_intervals: usize,
    /// Enabled visual Transition intervals admitted to the schedule.
    pub transition_intervals: usize,
    /// Merged visible-Track activity intervals admitted to the global index.
    pub track_activity_intervals: usize,
}

/// Work performed by one exact-time interval query.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PreparedVisualScheduleQueryDiagnostics {
    /// Visible authored Tracks selected by the global activity index.
    pub queried_tracks: usize,
    /// Global Track-activity interval-tree nodes visited.
    pub visited_track_activity_nodes: usize,
    /// Global Track-activity interval records inspected.
    pub inspected_track_activity_entries: usize,
    /// Interval-tree nodes visited across Clip and Transition indexes.
    ///
    /// This aggregate includes the global Track-activity query.
    pub visited_interval_nodes: usize,
    /// Interval records inspected across Clip and Transition indexes.
    ///
    /// This aggregate includes the global Track-activity query.
    pub inspected_interval_entries: usize,
    /// Ordered visual items emitted for render-plan construction.
    pub emitted_items: usize,
}

/// One visible prepared Clip occurrence whose execution interval intersects a
/// requested inclusive Sequence-time window.
///
/// This is range-level dependency evidence, not a second render plan. The
/// canonical schedule owns visibility, mute, disabled-Clip, Transition
/// endpoint, placement, and Clip-local-time interpretation.
#[derive(Debug, Clone)]
pub struct PreparedVisualScheduleRangeClip {
    /// Stable Clip/Transition-endpoint execution identity.
    pub placement: TimelineClipExecutionRef,
    /// Immutable payload selected by the prepared author revision.
    pub content: ClipContent,
    /// First Clip-local time conservatively reachable in the requested window.
    pub first_clip_time: TimelineTime,
    /// Last Clip-local time conservatively reachable in the requested window.
    pub last_clip_time: TimelineTime,
}

impl PreparedVisualScheduleQueryDiagnostics {
    fn accumulate(&mut self, query: IntervalQueryDiagnostics) {
        self.visited_interval_nodes =
            self.visited_interval_nodes.saturating_add(query.visited_nodes);
        self.inspected_interval_entries =
            self.inspected_interval_entries.saturating_add(query.inspected_entries);
    }
}

/// Immutable Sequence visual execution index shared by Preview and Export.
///
/// The schedule retains only video Tracks and visual Transitions. Audio,
/// navigation, Project color-engine state, and consumer scheduling policy do
/// not enter it. Dynamic Clip properties are still evaluated at the requested
/// exact Sequence time after the interval index selects the active author
/// entities.
pub struct PreparedVisualSchedule {
    key: PreparedVisualScheduleKey,
    time_base: Rational,
    working_color_space: WorkingColorSpace,
    auto_tone_map_media: bool,
    tracks: Vec<PreparedVisualTrack>,
    active_tracks: IntervalIndex<usize>,
    clip_locations: HashMap<ClipId, (usize, usize)>,
    diagnostics: PreparedVisualScheduleDiagnostics,
}

impl PreparedVisualSchedule {
    /// Compile one validated Sequence revision into immutable interval indexes.
    pub fn compile(sequence: &Sequence) -> Result<Self> {
        let mut tracks = sequence
            .video_tracks
            .iter()
            .enumerate()
            .map(|(track_index, track)| {
                PreparedVisualTrack::compile(sequence.id, track_index, track)
            })
            .collect::<Result<Vec<_>>>()?;

        for transition in &sequence.video_transitions {
            validate_video_transition(&sequence.video_tracks, transition)?;
            let (track_index, left_index, right_index) =
                locate_transition_endpoints(&sequence.video_tracks, transition).ok_or_else(
                    || {
                        schedule_error(
                            sequence.id,
                            format!(
                                "visual Transition {} endpoints disappeared during preparation",
                                transition.id
                            ),
                        )
                    },
                )?;
            tracks[track_index].add_transition(transition, left_index, right_index)?;
        }
        for track in &mut tracks {
            track.finish_transitions();
        }
        let mut active_track_entries = Vec::new();
        for (track_index, track) in tracks.iter().enumerate() {
            active_track_entries.extend(
                track
                    .merged_activity_intervals()?
                    .into_iter()
                    .map(|(start, end)| IntervalEntry { start, end, value: track_index }),
            );
        }
        let active_tracks = IntervalIndex::new(active_track_entries);
        let track_activity_intervals = active_tracks.len();
        let mut clip_locations = HashMap::new();
        for (prepared_track_index, prepared) in tracks.iter().enumerate() {
            for (clip_index, clip) in prepared.track.clips.iter().enumerate() {
                if clip_locations.insert(clip.id, (prepared_track_index, clip_index)).is_some() {
                    return Err(schedule_error(
                        sequence.id,
                        format!("Clip identity {} occurs more than once", clip.id),
                    ));
                }
            }
        }

        let clip_intervals = tracks.iter().map(|track| track.clip_intervals.len()).sum::<usize>();
        let transition_intervals =
            tracks.iter().map(|track| track.transition_intervals.len()).sum::<usize>();
        Ok(Self {
            key: PreparedVisualScheduleKey::for_sequence(sequence),
            time_base: sequence.time_base(),
            working_color_space: sequence.settings.color.working_color_space,
            auto_tone_map_media: sequence.settings.color.input.auto_tone_map_media,
            active_tracks,
            clip_locations,
            diagnostics: PreparedVisualScheduleDiagnostics {
                sequence_id: sequence.id,
                revision: sequence.revision,
                video_tracks: tracks.len(),
                clip_intervals,
                transition_intervals,
                track_activity_intervals,
            },
            tracks,
        })
    }

    /// Sequence identity compiled by this schedule.
    pub const fn sequence_id(&self) -> SequenceId {
        self.key.sequence_id
    }

    /// Exact author revision compiled by this schedule.
    pub const fn revision(&self) -> SequenceRevision {
        self.key.revision
    }

    /// Return immutable preparation facts.
    pub const fn diagnostics(&self) -> PreparedVisualScheduleDiagnostics {
        self.diagnostics
    }

    /// Map one exact signed temporal Clip-local Effect request through the sole
    /// prepared placement/retime authority.
    ///
    /// This method deliberately does not clamp to the visible placement. A
    /// finite temporal Effect may use valid hidden source handles before a
    /// trimmed in-edge or after a trimmed out-edge. Media interpretation grids are applied by the concrete
    /// media Adapter after this exact author-domain result.
    pub fn sample_clip_source(
        &self,
        placement: TimelineClipExecutionRef,
        requested_clip_time: TimelineTime,
    ) -> Result<TimelineTime> {
        let sequence_time = self.clip_to_sequence_time(placement, requested_clip_time)?;
        let (track_index, clip_index) = self.clip_location(placement)?;
        self.tracks[track_index].track.clips[clip_index].timeline_to_source_time(sequence_time)
    }

    /// Map one exact Clip-local Effect instant back into the owning Sequence
    /// author-time domain without applying source retime.
    ///
    /// Temporal Effect parameter evaluation and frame-seed derivation use
    /// this mapping; media sampling continues through [`Self::sample_clip_source`].
    pub fn clip_to_sequence_time(
        &self,
        placement: TimelineClipExecutionRef,
        requested_clip_time: TimelineTime,
    ) -> Result<TimelineTime> {
        let (track_index, clip_index) = self.clip_location(placement)?;
        self.tracks[track_index].track.clips[clip_index].clip_to_timeline_time(requested_clip_time)
    }

    fn clip_location(&self, placement: TimelineClipExecutionRef) -> Result<(usize, usize)> {
        if placement.sequence_id != self.key.sequence_id
            || placement.sequence_revision != self.key.revision
        {
            return Err(schedule_error(
                self.key.sequence_id,
                format!(
                    "Clip {} execution reference belongs to Sequence {} revision {:?}, expected revision {:?}",
                    placement.clip_id,
                    placement.sequence_id,
                    placement.sequence_revision,
                    self.key.revision
                ),
            ));
        }
        let (track_index, clip_index) =
            self.clip_locations.get(&placement.clip_id).copied().ok_or_else(|| {
                schedule_error(
                    self.key.sequence_id,
                    format!(
                        "Clip {} is absent from prepared revision {:?}",
                        placement.clip_id, self.key.revision
                    ),
                )
            })?;
        let prepared = &self.tracks[track_index];
        validate_endpoint_context(
            self.key.sequence_id,
            prepared,
            clip_index,
            placement.endpoint,
        )?;
        Ok((track_index, clip_index))
    }

    /// Evaluate the ordered visual program and report interval-query work.
    pub fn flat_visual_items_at_with_diagnostics(
        &self,
        time: TimelineTime,
    ) -> Result<(Vec<FlatVisualItem>, PreparedVisualScheduleQueryDiagnostics)> {
        let mut items = Vec::new();
        let mut diagnostics = PreparedVisualScheduleQueryDiagnostics::default();
        let mut active_tracks = self.active_tracks.query(time);
        diagnostics.visited_track_activity_nodes = active_tracks.diagnostics.visited_nodes;
        diagnostics.inspected_track_activity_entries = active_tracks.diagnostics.inspected_entries;
        diagnostics.accumulate(active_tracks.diagnostics);
        active_tracks.values.sort_unstable();
        active_tracks.values.dedup();
        diagnostics.queried_tracks = active_tracks.values.len();
        for track_index in active_tracks.values {
            let prepared = &self.tracks[track_index];
            debug_assert!(prepared.track.is_visible && !prepared.track.is_muted);
            let track_opacity = prepared.track.evaluate_opacity(time).clamp(0.0, 1.0);
            let transition_query = prepared.transition_intervals.query(time);
            diagnostics.accumulate(transition_query.diagnostics);
            if transition_query.values.len() > 1 {
                return Err(schedule_error(
                    self.key.sequence_id,
                    format!(
                        "multiple visual Transitions are active on Track {}",
                        prepared.track.id
                    ),
                ));
            }
            let active_transition =
                transition_query.values.first().map(|index| &prepared.transitions[*index]);
            let replaced_endpoints =
                active_transition.map(|transition| (transition.left_index, transition.right_index));
            if let Some(transition) = active_transition {
                items.push(flatten_visual_transition_with_effect_snapshots(
                    transition.transition_id,
                    transition.sequence_range,
                    Arc::clone(&transition.definition),
                    &prepared.track.clips[transition.left_index],
                    &prepared.track.clips[transition.right_index],
                    &prepared.track,
                    prepared.track_index,
                    track_opacity,
                    time,
                    Arc::clone(&prepared.clip_effects[transition.left_index]),
                    Arc::clone(&prepared.clip_masks[transition.left_index]),
                    Arc::clone(&prepared.clip_effects[transition.right_index]),
                    Arc::clone(&prepared.clip_masks[transition.right_index]),
                )?);
            }

            let mut clip_query = prepared.clip_intervals.query(time);
            diagnostics.accumulate(clip_query.diagnostics);
            clip_query.values.sort_unstable();
            for clip_index in clip_query.values {
                if replaced_endpoints
                    .is_some_and(|(left, right)| clip_index == left || clip_index == right)
                {
                    continue;
                }
                items.push(FlatVisualItem::Clip(
                    flatten_visual_clip_with_effect_snapshots(
                        &prepared.track.clips[clip_index],
                        &prepared.track,
                        prepared.track_index,
                        track_opacity,
                        time,
                        Arc::clone(&prepared.clip_effects[clip_index]),
                        Arc::clone(&prepared.clip_masks[clip_index]),
                    )?,
                ));
            }
        }
        diagnostics.emitted_items = items.len();
        Ok((items, diagnostics))
    }

    /// Collect visible Clip dependency intervals intersecting one inclusive
    /// Sequence-time window without enumerating its frames.
    ///
    /// Ordinary Clip intervals and active Transition endpoint intervals are
    /// queried from the same immutable indexes used by frame evaluation.
    /// Callers may conservatively merge duplicate endpoint/ordinary evidence.
    pub fn range_clips(
        &self,
        first: TimelineTime,
        last: TimelineTime,
    ) -> Result<Vec<PreparedVisualScheduleRangeClip>> {
        if last < first {
            return Err(schedule_error(
                self.key.sequence_id,
                "visual dependency window ends before it starts",
            ));
        }
        let mut result = Vec::new();
        let mut active_tracks = self.active_tracks.query_range(first, last).values;
        active_tracks.sort_unstable();
        active_tracks.dedup();
        for track_index in active_tracks {
            let prepared = &self.tracks[track_index];
            debug_assert!(prepared.track.is_visible && !prepared.track.is_muted);

            let mut clip_indexes = prepared.clip_intervals.query_range(first, last).values;
            clip_indexes.sort_unstable();
            clip_indexes.dedup();
            for clip_index in clip_indexes {
                let clip = &prepared.track.clips[clip_index];
                prepared.push_range_clip(
                    self.key,
                    clip_index,
                    TimelineClipEndpointContext::Ordinary,
                    first.max(clip.position),
                    last.min(clip.end_position()?),
                    &mut result,
                )?;
            }

            let mut transition_indexes =
                prepared.transition_intervals.query_range(first, last).values;
            transition_indexes.sort_unstable();
            transition_indexes.dedup();
            for transition_index in transition_indexes {
                let transition = &prepared.transitions[transition_index];
                let transition_end = transition.sequence_range.end()?;
                let overlap_first = first.max(transition.sequence_range.start);
                let overlap_last = last.min(transition_end);
                prepared.push_range_clip(
                    self.key,
                    transition.left_index,
                    TimelineClipEndpointContext::TransitionLeft {
                        transition_id: transition.transition_id,
                    },
                    overlap_first,
                    overlap_last,
                    &mut result,
                )?;
                prepared.push_range_clip(
                    self.key,
                    transition.right_index,
                    TimelineClipEndpointContext::TransitionRight {
                        transition_id: transition.transition_id,
                    },
                    overlap_first,
                    overlap_last,
                    &mut result,
                )?;
            }
        }
        Ok(result)
    }
}

impl RenderPlanSource for PreparedVisualSchedule {
    fn flat_visual_items_at(&self, time: TimelineTime) -> Result<Vec<FlatVisualItem>> {
        self.flat_visual_items_at_with_diagnostics(time).map(|(items, _)| items)
    }

    fn source_sequence_id(&self) -> SequenceId {
        self.key.sequence_id
    }

    fn source_sequence_revision(&self) -> SequenceRevision {
        self.key.revision
    }

    fn source_time_base(&self) -> Rational {
        self.time_base
    }

    fn source_working_color_space(&self) -> WorkingColorSpace {
        self.working_color_space
    }

    fn auto_tone_map_media(&self) -> bool {
        self.auto_tone_map_media
    }
}

struct PreparedVisualTrack {
    track_index: usize,
    track: Track,
    clip_effects: Vec<Arc<[mondrian_core::effect_data::EffectNode]>>,
    clip_masks: Vec<Arc<[mondrian_core::mask_data::MaskComponent]>>,
    clip_intervals: IntervalIndex<usize>,
    transitions: Vec<PreparedVisualTransition>,
    transition_intervals: IntervalIndex<usize>,
}

impl PreparedVisualTrack {
    fn compile(sequence_id: SequenceId, track_index: usize, track: &Track) -> Result<Self> {
        let mut intervals = Vec::with_capacity(track.clips.len());
        for (clip_index, clip) in track.clips.iter().enumerate() {
            clip.validate_time_state().map_err(|error| {
                schedule_error(
                    sequence_id,
                    format!("Clip {} has invalid time state: {error}", clip.id),
                )
            })?;
            let end = clip.end_position()?;
            if end <= clip.position {
                return Err(schedule_error(
                    sequence_id,
                    format!(
                        "Clip {} has an empty or reversed placement interval",
                        clip.id
                    ),
                ));
            }
            if !clip.is_disabled {
                intervals.push(IntervalEntry { start: clip.position, end, value: clip_index });
            }
        }
        let mut track = track.clone();
        let mut clip_effects = Vec::with_capacity(track.clips.len());
        let mut clip_masks = Vec::with_capacity(track.clips.len());
        for clip in &mut track.clips {
            clip_effects.push(Arc::from(std::mem::take(&mut clip.effects).into_vec()));
            clip_masks.push(Arc::from(std::mem::take(&mut clip.masks).into_vec()));
        }
        Ok(Self {
            track_index,
            track,
            clip_effects,
            clip_masks,
            clip_intervals: IntervalIndex::new(intervals),
            transitions: Vec::new(),
            transition_intervals: IntervalIndex::default(),
        })
    }

    fn add_transition(
        &mut self,
        transition: &VideoTransition,
        left_index: usize,
        right_index: usize,
    ) -> Result<()> {
        if !transition.is_enabled {
            return Ok(());
        }
        let end = transition.sequence_range.end()?;
        let index = self.transitions.len();
        self.transitions.push(PreparedVisualTransition {
            transition_id: transition.id,
            sequence_range: transition.sequence_range,
            definition: flat_video_transition_definition_snapshot(transition),
            left_index,
            right_index,
        });
        self.transition_intervals.pending.push(IntervalEntry {
            start: transition.sequence_range.start,
            end,
            value: index,
        });
        Ok(())
    }

    fn finish_transitions(&mut self) {
        self.transition_intervals.rebuild();
    }

    fn merged_activity_intervals(&self) -> Result<Vec<(TimelineTime, TimelineTime)>> {
        if !self.track.is_visible || self.track.is_muted {
            return Ok(Vec::new());
        }
        let mut intervals =
            Vec::with_capacity(self.track.clips.len().saturating_add(self.transitions.len()));
        for clip in self.track.clips.iter().filter(|clip| !clip.is_disabled) {
            intervals.push((clip.position, clip.end_position()?));
        }
        for transition in &self.transitions {
            intervals.push((
                transition.sequence_range.start,
                transition.sequence_range.end()?,
            ));
        }
        intervals.sort_unstable_by_key(|(start, end)| (*start, *end));

        let mut merged = Vec::with_capacity(intervals.len());
        for (next_start, next_end) in intervals {
            if let Some((_, current_end)) = merged.last_mut() {
                if next_start <= *current_end {
                    *current_end = (*current_end).max(next_end);
                    continue;
                }
            }
            merged.push((next_start, next_end));
        }
        Ok(merged)
    }

    fn push_range_clip(
        &self,
        key: PreparedVisualScheduleKey,
        clip_index: usize,
        endpoint: TimelineClipEndpointContext,
        requested_first: TimelineTime,
        requested_last: TimelineTime,
        result: &mut Vec<PreparedVisualScheduleRangeClip>,
    ) -> Result<()> {
        let clip = &self.track.clips[clip_index];
        result.push(PreparedVisualScheduleRangeClip {
            placement: TimelineClipExecutionRef {
                sequence_id: key.sequence_id,
                sequence_revision: key.revision,
                clip_id: clip.id,
                clip_time: clip.timeline_to_clip_time(requested_first)?,
                endpoint,
            },
            content: clip.content.clone(),
            first_clip_time: clip.timeline_to_clip_time(requested_first)?,
            last_clip_time: clip.timeline_to_clip_time(requested_last)?,
        });
        Ok(())
    }
}

struct PreparedVisualTransition {
    transition_id: VideoTransitionId,
    sequence_range: TimelineTimeRange,
    definition: Arc<FlatVideoTransitionDefinitionSnapshot>,
    left_index: usize,
    right_index: usize,
}

fn validate_endpoint_context(
    sequence_id: SequenceId,
    track: &PreparedVisualTrack,
    clip_index: usize,
    endpoint: TimelineClipEndpointContext,
) -> Result<()> {
    let valid = match endpoint {
        TimelineClipEndpointContext::Ordinary => true,
        TimelineClipEndpointContext::TransitionLeft { transition_id } => {
            track.transitions.iter().any(|transition| {
                transition.transition_id == transition_id && transition.left_index == clip_index
            })
        }
        TimelineClipEndpointContext::TransitionRight { transition_id } => {
            track.transitions.iter().any(|transition| {
                transition.transition_id == transition_id && transition.right_index == clip_index
            })
        }
    };
    if valid {
        Ok(())
    } else {
        Err(schedule_error(
            sequence_id,
            "Clip execution reference does not match its Transition endpoint",
        ))
    }
}

fn locate_transition_endpoints(
    tracks: &[Track],
    transition: &VideoTransition,
) -> Option<(usize, usize, usize)> {
    tracks.iter().enumerate().find_map(|(track_index, track)| {
        let left_index = track.clips.iter().position(|clip| clip.id == transition.left)?;
        let right_index = track.clips.iter().position(|clip| clip.id == transition.right)?;
        Some((track_index, left_index, right_index))
    })
}

fn schedule_error(sequence_id: SequenceId, reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "prepare_visual_schedule".to_owned(),
        reason: format!("Sequence {sequence_id}: {}", reason.into()),
    }
}

#[derive(Clone, Copy)]
struct IntervalEntry<T> {
    start: TimelineTime,
    end: TimelineTime,
    value: T,
}

struct IntervalIndex<T> {
    root: Option<Box<IntervalNode<T>>>,
    len: usize,
    pending: Vec<IntervalEntry<T>>,
}

impl<T> Default for IntervalIndex<T> {
    fn default() -> Self {
        Self { root: None, len: 0, pending: Vec::new() }
    }
}

impl<T: Copy> IntervalIndex<T> {
    fn new(entries: Vec<IntervalEntry<T>>) -> Self {
        let len = entries.len();
        Self {
            root: IntervalNode::build(entries),
            len,
            pending: Vec::new(),
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn rebuild(&mut self) {
        let entries = std::mem::take(&mut self.pending);
        self.len = entries.len();
        self.root = IntervalNode::build(entries);
    }

    fn query(&self, time: TimelineTime) -> IntervalQuery<T> {
        let mut values = Vec::new();
        let mut diagnostics = IntervalQueryDiagnostics::default();
        if let Some(root) = &self.root {
            root.query(time, &mut values, &mut diagnostics);
        }
        IntervalQuery { values, diagnostics }
    }

    fn query_range(&self, first: TimelineTime, last: TimelineTime) -> IntervalQuery<T> {
        let mut values = Vec::new();
        let mut diagnostics = IntervalQueryDiagnostics::default();
        if let Some(root) = &self.root {
            root.query_range(first, last, &mut values, &mut diagnostics);
        }
        IntervalQuery { values, diagnostics }
    }
}

struct IntervalNode<T> {
    center: TimelineTime,
    crossing_by_start: Vec<IntervalEntry<T>>,
    crossing_by_end: Vec<IntervalEntry<T>>,
    left: Option<Box<IntervalNode<T>>>,
    right: Option<Box<IntervalNode<T>>>,
}

impl<T: Copy> IntervalNode<T> {
    fn build(entries: Vec<IntervalEntry<T>>) -> Option<Box<Self>> {
        if entries.is_empty() {
            return None;
        }
        let mut starts = entries.iter().map(|entry| entry.start).collect::<Vec<_>>();
        starts.sort_unstable();
        let center = starts[starts.len() / 2];
        let mut left = Vec::new();
        let mut right = Vec::new();
        let mut crossing = Vec::new();
        for entry in entries {
            if entry.end <= center {
                left.push(entry);
            } else if entry.start > center {
                right.push(entry);
            } else {
                crossing.push(entry);
            }
        }
        debug_assert!(
            !crossing.is_empty(),
            "median-start interval partition must retain at least one crossing"
        );
        crossing.sort_unstable_by_key(|entry| (entry.start, entry.end));
        let mut crossing_by_end = crossing.clone();
        crossing_by_end.sort_unstable_by(|left, right| {
            right.end.cmp(&left.end).then_with(|| left.start.cmp(&right.start))
        });
        Some(Box::new(Self {
            center,
            crossing_by_start: crossing,
            crossing_by_end,
            left: Self::build(left),
            right: Self::build(right),
        }))
    }

    fn query(
        &self,
        time: TimelineTime,
        values: &mut Vec<T>,
        diagnostics: &mut IntervalQueryDiagnostics,
    ) {
        diagnostics.visited_nodes = diagnostics.visited_nodes.saturating_add(1);
        if time < self.center {
            for entry in &self.crossing_by_start {
                diagnostics.inspected_entries = diagnostics.inspected_entries.saturating_add(1);
                if entry.start > time {
                    break;
                }
                values.push(entry.value);
            }
            if let Some(left) = &self.left {
                left.query(time, values, diagnostics);
            }
        } else {
            for entry in &self.crossing_by_end {
                diagnostics.inspected_entries = diagnostics.inspected_entries.saturating_add(1);
                if entry.end <= time {
                    break;
                }
                values.push(entry.value);
            }
            if let Some(right) = &self.right {
                right.query(time, values, diagnostics);
            }
        }
    }

    fn query_range(
        &self,
        first: TimelineTime,
        last: TimelineTime,
        values: &mut Vec<T>,
        diagnostics: &mut IntervalQueryDiagnostics,
    ) {
        diagnostics.visited_nodes = diagnostics.visited_nodes.saturating_add(1);
        for entry in &self.crossing_by_start {
            diagnostics.inspected_entries = diagnostics.inspected_entries.saturating_add(1);
            if entry.start > last {
                break;
            }
            if entry.end > first {
                values.push(entry.value);
            }
        }
        if first < self.center {
            if let Some(left) = &self.left {
                left.query_range(first, last, values, diagnostics);
            }
        }
        if last > self.center {
            if let Some(right) = &self.right {
                right.query_range(first, last, values, diagnostics);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct IntervalQueryDiagnostics {
    visited_nodes: usize,
    inspected_entries: usize,
}

struct IntervalQuery<T> {
    values: Vec<T>,
    diagnostics: IntervalQueryDiagnostics,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{clip::Transform2D, Clip, Track};
    use glam::Vec2;
    use mondrian_core::automation::{
        AnimatedProperty, Keyframe, PropertyDescriptor, PropertyValue,
    };
    use mondrian_core::effect_data::{EffectNode, EffectType};
    use mondrian_core::mask_data::{MaskComponent, MaskKeyframe, MaskShape, MASK_PROP_OPACITY};
    use mondrian_core::timeline_data::{
        FlatActiveClip, FlatVideoTransitionDefinition, FlatVisualItem,
    };
    use mondrian_core::{
        AnimationTrackId, AssetId, BlendMode, ClipId, Color, EffectId, KeyframeId, MaskId,
        ParameterId, SequenceId, TimeScale, TimelineTimeRange, TrackId, VideoTransitionId,
    };
    use serde_json::{json, Value};
    use std::collections::BTreeSet;
    use uuid::Uuid;

    fn tt(frame: i64) -> TimelineTime {
        TimelineTime::new(frame, 25).expect("valid test time")
    }

    #[derive(Debug, PartialEq, Eq)]
    enum VisualSignature {
        Clip {
            clip_id: mondrian_core::ClipId,
            clip_time: TimelineTime,
            source_time: TimelineTime,
            opacity: u32,
            track_index: usize,
        },
        Transition {
            transition_id: mondrian_core::VideoTransitionId,
            left: mondrian_core::ClipId,
            right: mondrian_core::ClipId,
            elapsed: TimelineTime,
            duration: TimelineTime,
        },
    }

    fn signatures(items: Vec<FlatVisualItem>) -> Vec<VisualSignature> {
        items
            .into_iter()
            .map(|item| match item {
                FlatVisualItem::Clip(clip) => VisualSignature::Clip {
                    clip_id: clip.clip_id,
                    clip_time: clip.clip_time,
                    source_time: clip.source_time,
                    opacity: clip.opacity.to_bits(),
                    track_index: clip.track_index,
                },
                FlatVisualItem::Transition(transition) => VisualSignature::Transition {
                    transition_id: transition.transition_id,
                    left: transition.left.clip_id,
                    right: transition.right.clip_id,
                    elapsed: transition.progress.elapsed,
                    duration: transition.progress.duration,
                },
            })
            .collect()
    }

    #[derive(Debug, PartialEq, Eq)]
    struct CanonicalVisualFingerprint(Value);

    fn flat_clip_fingerprint(clip: &FlatActiveClip) -> Value {
        json!({
            "clip_id": clip.clip_id,
            "content": &clip.content,
            "is_disabled": clip.is_disabled,
            "effects": clip.effects.as_ref(),
            "masks": clip.masks.as_ref(),
            "clip_time": clip.clip_time,
            "source_time": clip.source_time,
            "transform_matrix_bits": clip.transform_matrix.map(f32::to_bits),
            "opacity_bits": clip.opacity.to_bits(),
            "blend_mode": clip.blend_mode,
            "track_index": clip.track_index,
        })
    }

    fn canonical_visual_fingerprint(items: &[FlatVisualItem]) -> CanonicalVisualFingerprint {
        let items = items
            .iter()
            .map(|item| match item {
                FlatVisualItem::Clip(clip) => json!({
                    "kind": "clip",
                    "clip": flat_clip_fingerprint(clip),
                }),
                FlatVisualItem::Transition(transition) => {
                    let definition = match &transition.definition.definition {
                        FlatVideoTransitionDefinition::CrossDissolve => json!({
                            "kind": "cross_dissolve",
                        }),
                        FlatVideoTransitionDefinition::Plugin { definition_id } => json!({
                            "kind": "plugin",
                            "definition_id": definition_id,
                        }),
                    };
                    json!({
                        "kind": "transition",
                        "transition_id": transition.transition_id,
                        "definition": definition,
                        "properties": &transition.definition.properties,
                        "params": &transition.definition.params,
                        "left": flat_clip_fingerprint(&transition.left),
                        "right": flat_clip_fingerprint(&transition.right),
                        "progress": {
                            "elapsed": transition.progress.elapsed,
                            "duration": transition.progress.duration,
                        },
                    })
                }
            })
            .collect::<Vec<_>>();
        CanonicalVisualFingerprint(json!({
            "schema": "mondrian.test.flat-visual-fingerprint.v1",
            "items": items,
        }))
    }

    fn fixed_keyframe(id: u128, frame: i64, value: PropertyValue) -> Keyframe<PropertyValue> {
        let mut keyframe = Keyframe::linear(tt(frame), value);
        keyframe.id = KeyframeId(Uuid::from_u128(id));
        keyframe
    }

    fn solid_clip(id: u128, asset: u128, color: u32, start: i64, duration: i64) -> Clip {
        let mut clip = Clip::new_solid_color(
            AssetId(Uuid::from_u128(asset)),
            Color::from_hex(color),
            tt(start),
            tt(duration),
        )
        .expect("valid deterministic solid Clip");
        clip.id = ClipId(Uuid::from_u128(id));
        clip
    }

    fn rich_visual_sequence() -> Sequence {
        let mut sequence = Sequence::new("prepared visual differential reference");
        sequence.id = SequenceId(Uuid::from_u128(1));
        sequence.video_tracks.clear();

        let mut main = Track::new_video("V1 main");
        main.id = TrackId(Uuid::from_u128(10));
        main.blend_mode = BlendMode::Multiply;
        for keyframe in [
            fixed_keyframe(4_000, 0, PropertyValue::Float(1.0)),
            fixed_keyframe(4_001, 20, PropertyValue::Float(0.45)),
            fixed_keyframe(4_002, 48, PropertyValue::Float(0.8)),
        ] {
            main.opacity.set_exact_keyframe(keyframe).expect("track opacity keyframe");
        }

        let mut left = solid_clip(100, 1_000, 0x18_36_5f, 0, 12);
        left.set_source_origin(tt(100)).expect("left source origin");
        left.transform.set_position(Vec2::new(320.0, 180.0));
        left.transform.set_scale(Vec2::new(0.75, 1.25));
        left.transform.set_anchor_point(Vec2::new(32.0, 16.0));
        for keyframe in [
            fixed_keyframe(4_100, 0, PropertyValue::Float(-12.0)),
            fixed_keyframe(4_101, 12, PropertyValue::Float(33.0)),
        ] {
            left.transform
                .apply_property_mutation(mondrian_core::automation::PropertyMutation::SetKeyframe {
                    path: Transform2D::ROTATION_PATH.to_owned(),
                    keyframe,
                })
                .expect("rotation keyframe");
        }
        for keyframe in [
            fixed_keyframe(4_102, 0, PropertyValue::Float(0.2)),
            fixed_keyframe(4_103, 12, PropertyValue::Float(0.9)),
        ] {
            left.transform
                .apply_property_mutation(mondrian_core::automation::PropertyMutation::SetKeyframe {
                    path: Transform2D::OPACITY_PATH.to_owned(),
                    keyframe,
                })
                .expect("Clip opacity keyframe");
        }
        left.blend_mode = Some(BlendMode::Screen);

        let mut blur = EffectNode::new(EffectType::GaussianBlur);
        blur.id = EffectId(Uuid::from_u128(2_000));
        blur.params = json!({"quality": "high", "edge_mode": "mirror"});
        let mut radius = AnimatedProperty::from_descriptor(
            PropertyDescriptor::new("radius", "Radius", PropertyValue::Float(2.0))
                .with_parameter_id(ParameterId::new_static(
                    "mondrian.effect.gaussian_blur.radius",
                )),
        );
        radius.track_id = AnimationTrackId(Uuid::from_u128(2_100));
        radius
            .set_exact_keyframe(fixed_keyframe(4_200, 0, PropertyValue::Float(2.0)))
            .expect("blur start keyframe");
        radius
            .set_exact_keyframe(fixed_keyframe(4_201, 9, PropertyValue::Float(18.0)))
            .expect("blur end keyframe");
        blur.properties.upsert(radius);
        left.add_effect_node(blur);

        let mut disabled_effect = EffectNode::new(EffectType::Sharpen);
        disabled_effect.id = EffectId(Uuid::from_u128(2_001));
        disabled_effect.params = json!({"amount": 0.375, "threshold": 0.125});
        disabled_effect.is_enabled = false;
        left.add_effect_node(disabled_effect);

        let mut mask = MaskComponent::new(
            "Animated isolation".to_owned(),
            MaskKeyframe {
                shape: MaskShape::Rectangle {
                    x: 0.1,
                    y: 0.2,
                    width: 0.6,
                    height: 0.5,
                    corner_radius: 0.05,
                },
                feather: 12.0,
                opacity: 0.8,
                expansion: 3.0,
                invert: false,
                mask_op: mondrian_core::mask_data::MaskOp::Intersect,
            },
        );
        mask.id = MaskId(Uuid::from_u128(3_000));
        let stable_mask_properties = mask
            .properties
            .iter()
            .enumerate()
            .map(|(index, (_, property))| {
                let mut property = property.clone();
                property.track_id = AnimationTrackId(Uuid::from_u128(3_100 + index as u128));
                property
            })
            .collect::<Vec<_>>();
        for property in stable_mask_properties {
            mask.properties.upsert(property);
        }
        mask.properties
            .set_keyframe(
                MASK_PROP_OPACITY,
                fixed_keyframe(4_300, 0, PropertyValue::Float(0.8)),
            )
            .expect("mask start opacity");
        mask.properties
            .set_keyframe(
                MASK_PROP_OPACITY,
                fixed_keyframe(4_301, 8, PropertyValue::Float(0.35)),
            )
            .expect("mask end opacity");
        mask.shape_animation_enabled = true;
        mask.shape_keyframes.push((
            tt(8),
            MaskShape::Ellipse {
                center: Vec2::new(0.55, 0.45),
                radii: Vec2::new(0.25, 0.3),
            },
        ));
        left.masks.push(mask);
        let left_id = left.id;

        let mut right = solid_clip(101, 1_001, 0xc0_3a_54, 12, 12);
        right
            .set_constant_source_time_map(
                tt(200),
                TimeScale::new(2, 1).expect("valid double-speed map"),
            )
            .expect("right source map");
        right.transform.set_position(Vec2::new(640.0, 360.0));
        let right_id = right.id;

        let mut overlap_left = solid_clip(102, 1_002, 0x2a_9d_8f, 30, 12);
        overlap_left
            .set_constant_source_time_map(
                tt(400),
                TimeScale::new(-1, 1).expect("valid reverse source map"),
            )
            .expect("reverse source map");
        overlap_left.blend_mode = Some(BlendMode::Overlay);
        let overlap_right = solid_clip(103, 1_003, 0xe9_c4_6a, 36, 12);

        let mut disabled = solid_clip(104, 1_004, 0xff_00_ff, 50, 8);
        disabled.is_disabled = true;
        disabled.add_effect_node(EffectNode::new(EffectType::ChromaticAberration));
        let tail = solid_clip(105, 1_005, 0x78_56_a7, 60, 4);

        for clip in [left, right, overlap_left, overlap_right, disabled, tail] {
            main.add_clip(clip).expect("add main-track Clip");
        }

        let mut overlay = Track::new_video("V2 overlaps");
        overlay.id = TrackId(Uuid::from_u128(11));
        overlay.blend_mode = BlendMode::SoftLight;
        overlay
            .opacity
            .set_static_value(PropertyValue::Float(0.65))
            .expect("overlay opacity");
        for clip in [
            solid_clip(200, 1_100, 0x26_48_53, 4, 14),
            solid_clip(201, 1_101, 0xe7_6f_51, 8, 6),
            solid_clip(202, 1_102, 0x2a_9d_8f, 25, 4),
        ] {
            overlay.add_clip(clip).expect("add overlapping overlay Clip");
        }

        let mut sparse = Track::new_video("V3 sparse index load");
        sparse.id = TrackId(Uuid::from_u128(12));
        for index in 0..256_u128 {
            sparse
                .add_clip(solid_clip(
                    10_000 + index,
                    20_000 + index,
                    0x11_22_33,
                    100 + i64::try_from(index).expect("small index") * 10,
                    1,
                ))
                .expect("add sparse Clip");
        }

        let mut muted = Track::new_video("V4 muted");
        muted.id = TrackId(Uuid::from_u128(13));
        muted.is_muted = true;
        muted
            .add_clip(solid_clip(300, 1_200, 0xff_ff_ff, -10, 3_010))
            .expect("add muted Clip");

        sequence.video_tracks.extend([main, overlay, sparse, muted]);
        let mut transition = VideoTransition::cross_dissolve(
            left_id,
            right_id,
            TimelineTimeRange::new(tt(10), tt(4)).expect("legal Transition range"),
        );
        transition.id = VideoTransitionId(Uuid::from_u128(5_000));
        transition.params = json!({"curve": "scene_linear", "mix_bias": 0.125});
        sequence.video_transitions.push(transition);
        sequence
    }

    fn insert_boundary_neighborhood(samples: &mut BTreeSet<TimelineTime>, boundary: TimelineTime) {
        samples.insert(boundary.checked_sub(tt(1)).expect("bounded test time"));
        samples.insert(boundary);
        samples.insert(boundary.checked_add(tt(1)).expect("bounded test time"));
    }

    fn visual_boundary_samples(sequence: &Sequence) -> Vec<TimelineTime> {
        let mut boundaries = BTreeSet::new();
        for track in &sequence.video_tracks {
            boundaries.extend(track.opacity.keyframe_times());
            for clip in &track.clips {
                boundaries.insert(clip.position);
                boundaries.insert(clip.end_position().expect("valid Clip end"));
                for (_, property) in clip.transform.to_property_bag().iter() {
                    for clip_time in property.keyframe_times() {
                        boundaries.insert(
                            clip.clip_to_timeline_time(clip_time)
                                .expect("valid transform Sequence time"),
                        );
                    }
                }
                for effect in &clip.effects {
                    for (_, property) in effect.properties.iter() {
                        for clip_time in property.keyframe_times() {
                            boundaries.insert(
                                clip.clip_to_timeline_time(clip_time)
                                    .expect("valid effect Sequence time"),
                            );
                        }
                    }
                }
                for mask in &clip.masks {
                    for (clip_time, _) in &mask.shape_keyframes {
                        boundaries.insert(
                            clip.clip_to_timeline_time(*clip_time)
                                .expect("valid mask shape Sequence time"),
                        );
                    }
                    for (_, property) in mask.properties.iter() {
                        for clip_time in property.keyframe_times() {
                            boundaries.insert(
                                clip.clip_to_timeline_time(clip_time)
                                    .expect("valid mask property Sequence time"),
                            );
                        }
                    }
                }
            }
        }
        for transition in &sequence.video_transitions {
            boundaries.insert(transition.sequence_range.start);
            boundaries.insert(transition.sequence_range.end().expect("valid Transition end"));
        }

        let mut samples = BTreeSet::new();
        for boundary in boundaries {
            insert_boundary_neighborhood(&mut samples, boundary);
        }
        samples.extend([tt(12), tt(24), tt(37), tt(54)]);
        samples.into_iter().collect()
    }

    fn transition_sequence() -> Sequence {
        let mut sequence = Sequence::new("prepared visual schedule parity");
        sequence.video_tracks.clear();
        let mut track = Track::new_video("V1");
        let left =
            Clip::new_solid_color(AssetId::new(), Color::BLACK, tt(0), tt(10)).expect("left Clip");
        let right = Clip::new_solid_color(AssetId::new(), Color::WHITE, tt(10), tt(10))
            .expect("right Clip");
        let left_id = left.id;
        let right_id = right.id;
        track.add_clip(left).expect("add left");
        track.add_clip(right).expect("add right");
        sequence.video_tracks.push(track);
        sequence.video_transitions.push(VideoTransition::cross_dissolve(
            left_id,
            right_id,
            TimelineTimeRange::new(tt(8), tt(4)).expect("transition range"),
        ));
        sequence
    }

    #[test]
    fn preparation_rejects_deserialized_source_terminal_overflow() {
        let clip =
            Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE).expect("valid Clip");
        let clip_id = clip.id;
        let mut encoded = serde_json::to_value(clip).expect("serialize Clip");
        encoded["source_time_map"]["source_origin"]["numerator"] =
            serde_json::Value::from(i64::MAX);
        let invalid: Clip = serde_json::from_value(encoded).expect("deserialize invalid Clip");
        let mut track = Track::new_video("V1");
        track.add_clip(invalid).expect("place invalid Clip");
        let mut sequence = Sequence::new("invalid source terminal");
        sequence.video_tracks.clear();
        sequence.video_tracks.push(track);

        let error = match PreparedVisualSchedule::compile(&sequence) {
            Ok(_) => panic!("overflow must fail preparation"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(message.contains(&clip_id.to_string()));
        assert!(message.contains("invalid time state"));
    }

    #[test]
    fn prepared_schedule_matches_sequence_semantics_across_transition_edges() {
        let sequence = transition_sequence();
        let schedule = PreparedVisualSchedule::compile(&sequence).expect("prepare schedule");
        for time in [
            tt(0),
            tt(7),
            tt(8),
            tt(9),
            tt(10),
            tt(11),
            tt(12),
            tt(19),
            tt(20),
        ] {
            let direct = RenderPlanSource::flat_visual_items_at(&sequence, time)
                .expect("direct Sequence evaluation");
            let prepared = RenderPlanSource::flat_visual_items_at(&schedule, time)
                .expect("prepared evaluation");
            assert_eq!(signatures(prepared), signatures(direct), "time={time:?}");
        }
    }

    #[test]
    fn repeated_transition_queries_share_prepared_definition_state() {
        let sequence = transition_sequence();
        let schedule = PreparedVisualSchedule::compile(&sequence).expect("prepare schedule");
        let first = schedule
            .flat_visual_items_at(tt(8))
            .expect("first Transition query")
            .into_iter()
            .find_map(|item| match item {
                FlatVisualItem::Transition(transition) => Some(transition.definition),
                FlatVisualItem::Clip(_) => None,
            })
            .expect("first Transition");
        let second = schedule
            .flat_visual_items_at(tt(9))
            .expect("second Transition query")
            .into_iter()
            .find_map(|item| match item {
                FlatVisualItem::Transition(transition) => Some(transition.definition),
                FlatVisualItem::Clip(_) => None,
            })
            .expect("second Transition");

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn canonical_temporal_sampling_covers_forward_reverse_and_hold_maps() {
        for (scale, expected) in [
            (TimeScale::new(2, 1).expect("forward"), tt(104)),
            (TimeScale::new(-1, 1).expect("reverse"), tt(98)),
            (TimeScale::new(0, 1).expect("hold"), tt(100)),
        ] {
            let mut sequence = Sequence::new("temporal retime sample");
            sequence.video_tracks.clear();
            let mut track = Track::new_video("V1");
            let mut clip = Clip::new(AssetId::new(), tt(10), tt(20)).expect("media Clip");
            clip.clip_time_in = tt(3);
            clip.set_constant_source_time_map(tt(100), scale).expect("source map");
            let clip_id = clip.id;
            track.add_clip(clip).expect("add Clip");
            sequence.video_tracks.push(track);
            let schedule = PreparedVisualSchedule::compile(&sequence).expect("schedule");
            let placement = TimelineClipExecutionRef {
                sequence_id: sequence.id,
                sequence_revision: sequence.revision,
                clip_id,
                clip_time: tt(3),
                endpoint: TimelineClipEndpointContext::Ordinary,
            };
            assert_eq!(
                schedule.sample_clip_source(placement, tt(5)).expect("historical source sample"),
                expected
            );
        }
    }

    #[test]
    fn transition_endpoint_sampling_is_bound_to_exact_side_and_identity() {
        let sequence = transition_sequence();
        let transition = &sequence.video_transitions[0];
        let schedule = PreparedVisualSchedule::compile(&sequence).expect("schedule");
        let left = TimelineClipExecutionRef {
            sequence_id: sequence.id,
            sequence_revision: sequence.revision,
            clip_id: transition.left,
            clip_time: tt(9),
            endpoint: TimelineClipEndpointContext::TransitionLeft { transition_id: transition.id },
        };
        assert!(schedule.sample_clip_source(left, tt(8)).is_ok());
        let wrong_side = TimelineClipExecutionRef {
            endpoint: TimelineClipEndpointContext::TransitionRight { transition_id: transition.id },
            ..left
        };
        assert!(schedule.sample_clip_source(wrong_side, tt(8)).is_err());
        let wrong_revision = TimelineClipExecutionRef {
            sequence_revision: sequence.revision.checked_next().expect("revision"),
            ..left
        };
        assert!(schedule.sample_clip_source(wrong_revision, tt(8)).is_err());
    }

    #[test]
    fn temporal_sampling_preserves_negative_clip_owner_time() {
        let mut sequence = Sequence::new("signed Clip time");
        sequence.video_tracks.clear();
        let mut track = Track::new_video("V1");
        let mut clip = Clip::new(AssetId::new(), tt(10), tt(20)).expect("media Clip");
        clip.clip_time_in = tt(-4);
        clip.set_constant_source_time_map(tt(100), TimeScale::new(2, 1).expect("forward"))
            .expect("source map");
        let clip_id = clip.id;
        track.add_clip(clip).expect("add Clip");
        sequence.video_tracks.push(track);
        let schedule = PreparedVisualSchedule::compile(&sequence).expect("schedule");
        let placement = TimelineClipExecutionRef {
            sequence_id: sequence.id,
            sequence_revision: sequence.revision,
            clip_id,
            clip_time: tt(-4),
            endpoint: TimelineClipEndpointContext::Ordinary,
        };
        assert_eq!(
            schedule.sample_clip_source(placement, tt(-5)).expect("hidden signed sample"),
            tt(98)
        );
    }

    #[test]
    fn repeated_queries_share_prepared_effect_snapshots() {
        let mut sequence = Sequence::new("shared prepared effect snapshots");
        sequence.video_tracks.clear();
        let mut track = Track::new_video("V1");
        let mut clip =
            Clip::new_solid_color(AssetId::new(), Color::BLACK, tt(0), tt(10)).expect("Clip");
        clip.add_effect_node(EffectNode::new(EffectType::GaussianBlur));
        track.add_clip(clip).expect("add Clip");
        sequence.video_tracks.push(track);

        let schedule = PreparedVisualSchedule::compile(&sequence).expect("prepare schedule");
        assert!(schedule.tracks[0].track.clips[0].effects.is_empty());
        assert!(schedule.tracks[0].track.clips[0].masks.is_empty());
        let first = schedule.flat_visual_items_at(tt(1)).expect("first prepared query");
        let second = schedule.flat_visual_items_at(tt(2)).expect("second prepared query");

        let FlatVisualItem::Clip(first) = &first[0] else {
            panic!("expected Clip");
        };
        let FlatVisualItem::Clip(second) = &second[0] else {
            panic!("expected Clip");
        };
        assert!(Arc::ptr_eq(&first.effects, &second.effects));
        assert_eq!(first.effects.len(), 1);
    }

    #[test]
    fn rich_prepared_schedule_matches_the_complete_scalar_visual_contract() {
        const MAX_VISITED_INTERVAL_NODES_PER_QUERY: usize = 32;
        const MAX_INSPECTED_INTERVAL_ENTRIES_PER_QUERY: usize = 32;

        let sequence = rich_visual_sequence();
        let sample_times = visual_boundary_samples(&sequence);
        let schedule = PreparedVisualSchedule::compile(&sequence).expect("prepare rich schedule");

        assert!(
            sample_times.len() > 1_000,
            "every authored boundary and both adjacent frames must be sampled"
        );
        assert_eq!(
            schedule.diagnostics(),
            PreparedVisualScheduleDiagnostics {
                sequence_id: sequence.id,
                revision: sequence.revision,
                video_tracks: 4,
                clip_intervals: 265,
                transition_intervals: 1,
                track_activity_intervals: 261,
            }
        );

        for time in sample_times {
            let scalar = RenderPlanSource::flat_visual_items_at(&sequence, time)
                .expect("scalar Sequence evaluation");
            let (prepared, diagnostics) = schedule
                .flat_visual_items_at_with_diagnostics(time)
                .expect("Prepared Visual Schedule evaluation");

            assert_eq!(
                canonical_visual_fingerprint(&prepared),
                canonical_visual_fingerprint(&scalar),
                "complete visual contract diverged at {time:?}"
            );
            assert_eq!(diagnostics.emitted_items, prepared.len(), "time={time:?}");
            assert!(
                diagnostics.visited_interval_nodes <= MAX_VISITED_INTERVAL_NODES_PER_QUERY,
                "time={time:?}: visited {} interval nodes",
                diagnostics.visited_interval_nodes
            );
            assert!(
                diagnostics.inspected_interval_entries <= MAX_INSPECTED_INTERVAL_ENTRIES_PER_QUERY,
                "time={time:?}: inspected {} interval entries",
                diagnostics.inspected_interval_entries
            );

            if time == tt(12) {
                assert!(
                    prepared.iter().any(|item| matches!(item, FlatVisualItem::Transition(_))),
                    "the legal two-input Transition must replace its endpoints"
                );
            }
            if time == tt(24) || time == tt(54) {
                assert!(
                    prepared.is_empty(),
                    "gaps, disabled Clips, and muted Tracks must not emit visual work at {time:?}"
                );
            }
            if time == tt(37) {
                assert_eq!(
                    prepared.iter().filter(|item| matches!(item, FlatVisualItem::Clip(_))).count(),
                    2,
                    "the same-Track overlap must preserve both active placements"
                );
            }
        }
    }

    #[test]
    fn sparse_large_schedule_query_inspects_only_a_bounded_interval_path() {
        let mut sequence = Sequence::new("sparse large schedule");
        sequence.video_tracks.clear();
        let mut track = Track::new_video("V1");
        for index in 0..10_000_i64 {
            track
                .add_clip(
                    Clip::new_solid_color(AssetId::new(), Color::BLACK, tt(index * 10), tt(1))
                        .expect("sparse Clip"),
                )
                .expect("add sparse Clip");
        }
        sequence.video_tracks.push(track);

        let schedule = PreparedVisualSchedule::compile(&sequence).expect("prepare large schedule");
        let (items, diagnostics) = schedule
            .flat_visual_items_at_with_diagnostics(tt(9_000 * 10))
            .expect("query large schedule");

        assert_eq!(items.len(), 1);
        assert!(
            diagnostics.inspected_interval_entries < 128,
            "sparse query inspected {} entries",
            diagnostics.inspected_interval_entries
        );
        assert!(diagnostics.visited_interval_nodes < 64);
    }

    #[test]
    fn global_activity_index_does_not_query_one_thousand_empty_tracks() {
        let mut sequence = Sequence::new("many empty Tracks");
        sequence.video_tracks =
            (0..1_000).map(|index| Track::new_video(format!("empty {index}"))).collect();
        let mut active = Track::new_video("active");
        active
            .add_clip(
                Clip::new_solid_color(AssetId::new(), Color::BLACK, tt(100), tt(10))
                    .expect("active Clip"),
            )
            .expect("add active Clip");
        sequence.video_tracks.push(active);

        let schedule = PreparedVisualSchedule::compile(&sequence).expect("prepare schedule");
        let (items, diagnostics) = schedule
            .flat_visual_items_at_with_diagnostics(tt(105))
            .expect("query active Track");

        assert_eq!(schedule.diagnostics().video_tracks, 1_001);
        assert_eq!(schedule.diagnostics().track_activity_intervals, 1);
        assert_eq!(items.len(), 1);
        assert_eq!(diagnostics.queried_tracks, 1);
        assert_eq!(diagnostics.visited_track_activity_nodes, 1);
        assert_eq!(diagnostics.inspected_track_activity_entries, 1);
    }

    #[test]
    fn activity_intervals_merge_adjacency_but_preserve_real_gaps() {
        let mut sequence = Sequence::new("half-open Track activity");
        sequence.video_tracks.clear();
        let mut track = Track::new_video("V1");
        for (start, duration) in [(0, 10), (10, 10), (30, 10)] {
            track
                .add_clip(
                    Clip::new_solid_color(AssetId::new(), Color::BLACK, tt(start), tt(duration))
                        .expect("Clip"),
                )
                .expect("add Clip");
        }
        sequence.video_tracks.push(track);

        let schedule = PreparedVisualSchedule::compile(&sequence).expect("prepare schedule");
        assert_eq!(schedule.diagnostics().track_activity_intervals, 2);
        let at_adjacent_boundary =
            schedule.flat_visual_items_at(tt(10)).expect("query adjacent boundary");
        assert_eq!(at_adjacent_boundary.len(), 1);
        let FlatVisualItem::Clip(clip) = &at_adjacent_boundary[0] else {
            panic!("expected Clip");
        };
        assert_eq!(clip.clip_time, tt(0));

        for gap_time in [tt(20), tt(29)] {
            let (items, diagnostics) = schedule
                .flat_visual_items_at_with_diagnostics(gap_time)
                .expect("query real gap");
            assert!(items.is_empty());
            assert_eq!(diagnostics.queried_tracks, 0);
        }
        assert_eq!(
            schedule.flat_visual_items_at(tt(30)).expect("query next interval").len(),
            1
        );
    }

    #[test]
    fn disabled_hidden_and_muted_content_do_not_create_track_activity() {
        let mut sequence = transition_sequence();
        for clip in &mut sequence.video_tracks[0].clips {
            clip.is_disabled = true;
        }

        let mut disabled = Track::new_video("disabled");
        let mut disabled_clip =
            Clip::new_solid_color(AssetId::new(), Color::BLACK, tt(8), tt(4)).expect("Clip");
        disabled_clip.is_disabled = true;
        disabled.add_clip(disabled_clip).expect("add disabled Clip");

        let mut hidden = Track::new_video("hidden");
        hidden.is_visible = false;
        hidden
            .add_clip(
                Clip::new_solid_color(AssetId::new(), Color::BLACK, tt(8), tt(4)).expect("Clip"),
            )
            .expect("add hidden Clip");

        let mut muted = Track::new_video("muted");
        muted.is_muted = true;
        muted
            .add_clip(
                Clip::new_solid_color(AssetId::new(), Color::BLACK, tt(8), tt(4)).expect("Clip"),
            )
            .expect("add muted Clip");
        sequence.video_tracks.extend([disabled, hidden, muted]);

        let schedule = PreparedVisualSchedule::compile(&sequence).expect("prepare schedule");
        assert_eq!(
            schedule.diagnostics().track_activity_intervals,
            1,
            "the enabled Transition alone keeps its visible Track active"
        );
        let (items, diagnostics) = schedule
            .flat_visual_items_at_with_diagnostics(tt(9))
            .expect("query Transition-only activity");
        assert_eq!(diagnostics.queried_tracks, 1);
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0], FlatVisualItem::Transition(_)));
    }

    #[test]
    fn active_track_query_restores_authored_track_order() {
        let mut sequence = Sequence::new("authored Track order");
        sequence.video_tracks.clear();
        for index in 0..3 {
            let mut track = Track::new_video(format!("V{index}"));
            if index != 1 {
                track
                    .add_clip(
                        Clip::new_solid_color(
                            AssetId::new(),
                            Color::BLACK,
                            tt(0),
                            tt(if index == 0 { 100 } else { 10 }),
                        )
                        .expect("Clip"),
                    )
                    .expect("add Clip");
            }
            sequence.video_tracks.push(track);
        }
        let schedule = PreparedVisualSchedule::compile(&sequence).expect("prepare schedule");
        let items = schedule.flat_visual_items_at(tt(5)).expect("query Tracks");
        let track_indices = items
            .into_iter()
            .map(|item| match item {
                FlatVisualItem::Clip(clip) => clip.track_index,
                FlatVisualItem::Transition(_) => unreachable!("test has no Transitions"),
            })
            .collect::<Vec<_>>();
        assert_eq!(track_indices, [0, 2]);
    }

    #[test]
    fn prepared_activity_index_matches_scalar_evaluation_on_every_frame() {
        let sequence = rich_visual_sequence();
        let schedule = PreparedVisualSchedule::compile(&sequence).expect("prepare rich schedule");

        for frame in -1..=3_011 {
            let time = tt(frame);
            let scalar = RenderPlanSource::flat_visual_items_at(&sequence, time)
                .expect("scalar Sequence evaluation");
            let prepared =
                schedule.flat_visual_items_at(time).expect("prepared Sequence evaluation");
            assert_eq!(
                canonical_visual_fingerprint(&prepared),
                canonical_visual_fingerprint(&scalar),
                "complete per-frame visual contract diverged at frame {frame}"
            );
        }
    }
}
