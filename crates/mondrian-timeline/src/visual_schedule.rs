//! Immutable, revision-bound visual execution schedule.
//!
//! Authoring keeps `Track -> Clip` placement as the sole source of truth. This
//! module compiles that validated state into interval indexes so repeated
//! Preview and Export evaluation does not scan every placement on every frame.

use crate::sequence::{
    flatten_visual_clip, flatten_visual_transition, validate_video_transition, Sequence,
};
use crate::track::Track;
use crate::video_transition::VideoTransition;
use mondrian_core::timeline_data::{FlatVisualItem, RenderPlanSource};
use mondrian_core::{
    MondrianError, Rational, Result, SequenceId, SequenceRevision, TimelineTime, WorkingColorSpace,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

/// Default number of immutable Sequence revisions retained by one consumer.
pub const DEFAULT_PREPARED_VISUAL_SCHEDULE_CACHE_CAPACITY: usize = 64;

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
}

/// Work performed by one exact-time interval query.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PreparedVisualScheduleQueryDiagnostics {
    /// Interval-tree nodes visited across Clip and Transition indexes.
    pub visited_interval_nodes: usize,
    /// Interval records inspected across Clip and Transition indexes.
    pub inspected_interval_entries: usize,
    /// Ordered visual items emitted for render-plan construction.
    pub emitted_items: usize,
}

impl PreparedVisualScheduleQueryDiagnostics {
    fn accumulate(&mut self, query: IntervalQueryDiagnostics) {
        self.visited_interval_nodes =
            self.visited_interval_nodes.saturating_add(query.visited_nodes);
        self.inspected_interval_entries =
            self.inspected_interval_entries.saturating_add(query.inspected_entries);
    }
}

/// Current and cumulative facts for one bounded prepared-schedule cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedVisualScheduleCacheDiagnostics {
    /// Current retained schedules.
    pub entries: usize,
    /// Configured maximum retained schedules.
    pub capacity: usize,
    /// Exact revision-key cache hits.
    pub hits: u64,
    /// Schedules compiled after a cache miss.
    pub misses: u64,
    /// Old revisions evicted by the bounded policy.
    pub evictions: u64,
}

/// Bounded consumer-owned cache of immutable prepared visual schedules.
///
/// Sequence identity plus `SequenceRevision` is the only reuse authority.
/// Authoring transactions must advance that revision whenever visual semantics
/// change. Compilation failure does not mutate the cache.
pub struct PreparedVisualScheduleCache {
    capacity: usize,
    entries: HashMap<PreparedVisualScheduleKey, Arc<PreparedVisualSchedule>>,
    recency: VecDeque<PreparedVisualScheduleKey>,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl PreparedVisualScheduleCache {
    /// Construct a bounded cache. A zero request is normalized to one entry.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: HashMap::new(),
            recency: VecDeque::new(),
            hits: 0,
            misses: 0,
            evictions: 0,
        }
    }

    /// Return the exact prepared schedule for one authoritative Sequence revision.
    pub fn prepare(&mut self, sequence: &Sequence) -> Result<Arc<PreparedVisualSchedule>> {
        let key = PreparedVisualScheduleKey::for_sequence(sequence);
        if let Some(schedule) = self.entries.get(&key).cloned() {
            self.hits = self.hits.saturating_add(1);
            self.touch(key);
            return Ok(schedule);
        }

        let schedule = Arc::new(PreparedVisualSchedule::compile(sequence)?);
        self.misses = self.misses.saturating_add(1);
        self.evict_obsolete_revisions(key);
        while self.entries.len() >= self.capacity {
            let Some(stale) = self.recency.pop_front() else {
                break;
            };
            if self.entries.remove(&stale).is_some() {
                self.evictions = self.evictions.saturating_add(1);
            }
        }
        self.entries.insert(key, Arc::clone(&schedule));
        self.recency.push_back(key);
        Ok(schedule)
    }

    /// Drop every prepared revision while retaining cumulative evidence.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.recency.clear();
    }

    /// Return bounded residency and reuse evidence.
    pub fn diagnostics(&self) -> PreparedVisualScheduleCacheDiagnostics {
        PreparedVisualScheduleCacheDiagnostics {
            entries: self.entries.len(),
            capacity: self.capacity,
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
        }
    }

    fn touch(&mut self, key: PreparedVisualScheduleKey) {
        self.recency.retain(|candidate| *candidate != key);
        self.recency.push_back(key);
    }

    fn evict_obsolete_revisions(&mut self, current: PreparedVisualScheduleKey) {
        let stale = self
            .entries
            .keys()
            .copied()
            .filter(|candidate| {
                candidate.sequence_id == current.sequence_id
                    && candidate.revision != current.revision
            })
            .collect::<Vec<_>>();
        for key in stale {
            if self.entries.remove(&key).is_some() {
                self.evictions = self.evictions.saturating_add(1);
            }
        }
        self.recency.retain(|candidate| {
            candidate.sequence_id != current.sequence_id || candidate.revision == current.revision
        });
    }
}

impl Default for PreparedVisualScheduleCache {
    fn default() -> Self {
        Self::new(DEFAULT_PREPARED_VISUAL_SCHEDULE_CACHE_CAPACITY)
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

        let clip_intervals = tracks.iter().map(|track| track.clip_intervals.len()).sum::<usize>();
        let transition_intervals =
            tracks.iter().map(|track| track.transition_intervals.len()).sum::<usize>();
        Ok(Self {
            key: PreparedVisualScheduleKey::for_sequence(sequence),
            time_base: sequence.time_base(),
            working_color_space: sequence.settings.color.working_color_space,
            auto_tone_map_media: sequence.settings.color.input.auto_tone_map_media,
            diagnostics: PreparedVisualScheduleDiagnostics {
                sequence_id: sequence.id,
                revision: sequence.revision,
                video_tracks: tracks.len(),
                clip_intervals,
                transition_intervals,
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

    /// Evaluate the ordered visual program and report interval-query work.
    pub fn flat_visual_items_at_with_diagnostics(
        &self,
        time: TimelineTime,
    ) -> Result<(Vec<FlatVisualItem>, PreparedVisualScheduleQueryDiagnostics)> {
        let mut items = Vec::new();
        let mut diagnostics = PreparedVisualScheduleQueryDiagnostics::default();
        for prepared in &self.tracks {
            if !prepared.track.is_visible || prepared.track.is_muted {
                continue;
            }
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
                items.push(flatten_visual_transition(
                    &transition.transition,
                    &prepared.track.clips[transition.left_index],
                    &prepared.track.clips[transition.right_index],
                    &prepared.track,
                    prepared.track_index,
                    track_opacity,
                    time,
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
                items.push(FlatVisualItem::Clip(flatten_visual_clip(
                    &prepared.track.clips[clip_index],
                    &prepared.track,
                    prepared.track_index,
                    track_opacity,
                    time,
                )?));
            }
        }
        diagnostics.emitted_items = items.len();
        Ok((items, diagnostics))
    }
}

impl RenderPlanSource for PreparedVisualSchedule {
    fn flat_visual_items_at(&self, time: TimelineTime) -> Result<Vec<FlatVisualItem>> {
        self.flat_visual_items_at_with_diagnostics(time).map(|(items, _)| items)
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
    clip_intervals: IntervalIndex<usize>,
    transitions: Vec<PreparedVisualTransition>,
    transition_intervals: IntervalIndex<usize>,
}

impl PreparedVisualTrack {
    fn compile(sequence_id: SequenceId, track_index: usize, track: &Track) -> Result<Self> {
        let mut intervals = Vec::with_capacity(track.clips.len());
        for (clip_index, clip) in track.clips.iter().enumerate() {
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
        Ok(Self {
            track_index,
            track: track.clone(),
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
            transition: transition.clone(),
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
}

struct PreparedVisualTransition {
    transition: VideoTransition,
    left_index: usize,
    right_index: usize,
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
    use crate::{Clip, Track};
    use mondrian_core::timeline_data::FlatVisualItem;
    use mondrian_core::{AssetId, Color, TimelineTimeRange};

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
    fn cache_reuses_exact_revision_and_evicts_previous_revision() {
        let mut sequence = transition_sequence();
        let mut cache = PreparedVisualScheduleCache::new(4);
        let first = cache.prepare(&sequence).expect("first prepare");
        let reused = cache.prepare(&sequence).expect("reuse prepare");
        assert!(Arc::ptr_eq(&first, &reused));

        sequence.revision = sequence.revision.checked_next().expect("test revision can advance");
        let second = cache.prepare(&sequence).expect("new revision prepare");
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(
            cache.diagnostics(),
            PreparedVisualScheduleCacheDiagnostics {
                entries: 1,
                capacity: 4,
                hits: 1,
                misses: 2,
                evictions: 1,
            }
        );
    }

    #[test]
    fn failed_preparation_does_not_replace_the_last_valid_revision() {
        let mut sequence = transition_sequence();
        let mut cache = PreparedVisualScheduleCache::new(4);
        let first = cache.prepare(&sequence).expect("first prepare");

        sequence.revision = sequence.revision.checked_next().expect("test revision can advance");
        sequence.video_transitions[0].right = mondrian_core::ClipId::new();
        assert!(cache.prepare(&sequence).is_err());

        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entries, 1);
        assert_eq!(diagnostics.misses, 1);
        assert_eq!(diagnostics.evictions, 0);
        assert_eq!(first.revision(), SequenceRevision::INITIAL);
    }
}
