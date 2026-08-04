//! Range-scoped dependency selection for compiled Audio Programs.
//!
//! This module is the sole interpretation of whether a compiled contribution
//! can contribute to a public output window. Snapshot capture and mutable
//! Runtime construction consume the same exact, half-open selection.

use crate::{
    compile_audio_program, AudioCompileError, AudioCompileRequest, AudioProgramExecutionDemand,
    CompiledAudioProgram, CompiledAudioSource,
};
use mondrian_core::{
    AssetId, AudioSourceComponentId, ProgramOutputId, SequenceId, TimelineTime, TimelineTimeError,
    TimelineTimeRange,
};
use mondrian_timeline::{sequence::MAX_NESTED_SEQUENCE_RENDER_DEPTH, Sequence};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// Closed Audio Program dependency set for one exact public output window.
///
/// The set contains exact selected root/nested semantic Program occurrences
/// plus routed contributions that intersect the requested half-open Sequence
/// window. Nested output placement is resolved through the canonical compiled
/// source-time map; callers never inspect Tracks or Clips. Occurrences are
/// keyed by Sequence, public Output, and exact projected window rather than a
/// coarse Sequence identity.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioDependencyClosure {
    sequence_ids: BTreeSet<SequenceId>,
    media_components: BTreeMap<AssetId, BTreeSet<AudioSourceComponentId>>,
    programs: BTreeMap<AudioDependencyProgramKey, Arc<CompiledAudioProgram>>,
    root_program: Arc<CompiledAudioProgram>,
}

impl AudioDependencyClosure {
    /// Sequence identities reached by selected Audio Program contributions.
    pub fn sequence_ids(&self) -> &BTreeSet<SequenceId> {
        &self.sequence_ids
    }

    /// File-backed media Components reached by selected Audio Program contributions.
    pub fn media_components(&self) -> &BTreeMap<AssetId, BTreeSet<AudioSourceComponentId>> {
        &self.media_components
    }

    /// Exact range-selected root Program that produced this closure.
    ///
    /// The compiler is a pure author-to-semantic lowering seam: it performs no
    /// plugin-registry, device, filesystem, or media access.
    pub fn root_program(&self) -> &Arc<CompiledAudioProgram> {
        &self.root_program
    }

    /// Conservative execution evidence derived from the exact root Program.
    pub fn execution_demand(&self) -> AudioProgramExecutionDemand {
        self.root_program.execution_demand()
    }

    pub(crate) fn program(
        &self,
        sequence_id: SequenceId,
        output_id: ProgramOutputId,
        window: DependencyWindow,
    ) -> Option<&Arc<CompiledAudioProgram>> {
        self.programs.get(&AudioDependencyProgramKey { sequence_id, output_id, window })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct AudioDependencyProgramKey {
    sequence_id: SequenceId,
    output_id: ProgramOutputId,
    window: DependencyWindow,
}

/// Failure while lowering one selected Audio Program dependency closure.
#[derive(Debug, thiserror::Error)]
pub enum AudioDependencyError {
    /// Authoring-to-semantic compilation failed.
    #[error(transparent)]
    Compile(#[from] AudioCompileError),
    /// Exact range or source-time arithmetic failed.
    #[error(transparent)]
    Time(#[from] TimelineTimeError),
    /// A selected nested output references no immutable Sequence candidate.
    #[error("audio dependency closure is missing nested Sequence {0}")]
    MissingNestedSequence(SequenceId),
    /// Candidate input contains more than one immutable Sequence with one identity.
    #[error("audio dependency closure contains duplicate Sequence {0}")]
    DuplicateSequence(SequenceId),
    /// Selected nested outputs form a recursive author graph.
    #[error("audio dependency closure contains a nested Sequence cycle at {0}")]
    NestedCycle(SequenceId),
    /// The selected closure exceeds the shared nested-Sequence depth contract.
    #[error("audio dependency closure exceeds nested depth {maximum} at Sequence {sequence_id}")]
    NestedDepthExceeded {
        /// Sequence that crossed the shared depth limit.
        sequence_id: SequenceId,
        /// Maximum admitted nesting depth.
        maximum: usize,
    },
    /// A selected Sequence has no requested public output.
    #[error("Sequence {0} has no audio Program Output")]
    MissingProgramOutput(SequenceId),
    /// Compiled range selection and nested source projection disagreed.
    #[error("selected audio contribution {0} has no source-time intersection")]
    InconsistentSelection(mondrian_core::AudioComponentEditId),
    /// One canonical selected occurrence produced conflicting semantic Programs.
    #[error(
        "selected audio occurrence for Sequence {sequence_id} output {output_id} produced conflicting Programs"
    )]
    InconsistentProgramEvidence {
        /// Sequence occurrence identity.
        sequence_id: SequenceId,
        /// Selected public Program Output.
        output_id: ProgramOutputId,
    },
}

/// Compile the exact audio dependency closure needed by one public root window.
///
/// `output_id == None` selects the root Sequence's first public Program Output,
/// matching Playback and Export. Nested contributions retain their explicit
/// authored output identity.
pub fn compile_audio_dependency_closure(
    root: &Sequence,
    sequences: &[Sequence],
    output_id: Option<ProgramOutputId>,
    range: TimelineTimeRange,
) -> Result<AudioDependencyClosure, AudioDependencyError> {
    let mut candidates = HashMap::with_capacity(sequences.len().saturating_add(1));
    candidates.insert(root.id, root);
    let mut root_candidate_seen = false;
    for sequence in sequences {
        if sequence.id == root.id {
            if root_candidate_seen || sequence != root {
                return Err(AudioDependencyError::DuplicateSequence(sequence.id));
            }
            root_candidate_seen = true;
            continue;
        }
        if candidates.insert(sequence.id, sequence).is_some() {
            return Err(AudioDependencyError::DuplicateSequence(sequence.id));
        }
    }

    let mut builder = AudioDependencyClosureBuilder::default();
    let mut stack = BTreeSet::new();
    let root_program = collect_sequence_dependencies(
        root,
        output_id,
        DependencyWindow::from_half_open(range)?,
        &candidates,
        &mut stack,
        0,
        &mut builder,
    )?;
    Ok(AudioDependencyClosure {
        sequence_ids: builder.sequence_ids,
        media_components: builder.media_components,
        programs: builder.programs,
        root_program,
    })
}

#[derive(Default)]
struct AudioDependencyClosureBuilder {
    sequence_ids: BTreeSet<SequenceId>,
    media_components: BTreeMap<AssetId, BTreeSet<AudioSourceComponentId>>,
    programs: BTreeMap<AudioDependencyProgramKey, Arc<CompiledAudioProgram>>,
}

fn collect_sequence_dependencies(
    sequence: &Sequence,
    output_id: Option<ProgramOutputId>,
    window: DependencyWindow,
    sequences: &HashMap<SequenceId, &Sequence>,
    stack: &mut BTreeSet<SequenceId>,
    depth: usize,
    closure: &mut AudioDependencyClosureBuilder,
) -> Result<Arc<CompiledAudioProgram>, AudioDependencyError> {
    if depth > MAX_NESTED_SEQUENCE_RENDER_DEPTH {
        return Err(AudioDependencyError::NestedDepthExceeded {
            sequence_id: sequence.id,
            maximum: MAX_NESTED_SEQUENCE_RENDER_DEPTH,
        });
    }
    if !stack.insert(sequence.id) {
        return Err(AudioDependencyError::NestedCycle(sequence.id));
    }
    closure.sequence_ids.insert(sequence.id);
    let result = (|| {
        let output_id = output_id
            .or_else(|| sequence.audio_program.outputs.first().map(|output| output.id))
            .ok_or(AudioDependencyError::MissingProgramOutput(sequence.id))?;
        let mut program = compile_audio_program(sequence, AudioCompileRequest::program(output_id))?;
        select_program_window(&mut program, window)?;
        let key = AudioDependencyProgramKey { sequence_id: sequence.id, output_id, window };
        let compiled_program = Arc::new(program);
        let program = if let Some(existing) = closure.programs.get(&key) {
            if existing.as_ref() != compiled_program.as_ref() {
                return Err(AudioDependencyError::InconsistentProgramEvidence {
                    sequence_id: sequence.id,
                    output_id,
                });
            }
            Arc::clone(existing)
        } else {
            closure.programs.insert(key, Arc::clone(&compiled_program));
            compiled_program
        };
        for contribution in program.contributions() {
            match contribution.source {
                CompiledAudioSource::Media { asset_id, component_id } => {
                    closure.media_components.entry(asset_id).or_default().insert(component_id);
                }
                CompiledAudioSource::NestedOutput { sequence_id, output_id } => {
                    let child = sequences
                        .get(&sequence_id)
                        .copied()
                        .ok_or(AudioDependencyError::MissingNestedSequence(sequence_id))?;
                    let child_window = selected_contribution_source_window(contribution, window)?
                        .ok_or(AudioDependencyError::InconsistentSelection(
                        contribution.edit_id,
                    ))?;
                    collect_sequence_dependencies(
                        child,
                        Some(output_id),
                        child_window,
                        sequences,
                        stack,
                        depth.saturating_add(1),
                        closure,
                    )?;
                }
            }
        }
        Ok(program)
    })();
    stack.remove(&sequence.id);
    result
}

/// Exact interval with explicit endpoint inclusion.
///
/// A reverse source-time map turns `[a, b)` into `(map(b), map(a)]`; retaining
/// endpoint flags avoids both false dependency admission and dropped samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct DependencyWindow {
    lower: TimelineTime,
    upper: TimelineTime,
    lower_inclusive: bool,
    upper_inclusive: bool,
}

impl DependencyWindow {
    pub(crate) fn from_half_open(range: TimelineTimeRange) -> Result<Self, TimelineTimeError> {
        Ok(Self {
            lower: range.start,
            upper: range.end()?,
            lower_inclusive: true,
            upper_inclusive: false,
        })
    }

    fn is_empty(self) -> bool {
        self.lower > self.upper
            || (self.lower == self.upper && !(self.lower_inclusive && self.upper_inclusive))
    }

    fn intersect_half_open(
        self,
        range: TimelineTimeRange,
    ) -> Result<Option<Self>, TimelineTimeError> {
        let range_end = range.end()?;
        let (lower, lower_inclusive) = match self.lower.cmp(&range.start) {
            std::cmp::Ordering::Less => (range.start, true),
            std::cmp::Ordering::Equal => (self.lower, self.lower_inclusive),
            std::cmp::Ordering::Greater => (self.lower, self.lower_inclusive),
        };
        let (upper, upper_inclusive) = match self.upper.cmp(&range_end) {
            std::cmp::Ordering::Less => (self.upper, self.upper_inclusive),
            std::cmp::Ordering::Equal => (self.upper, false),
            std::cmp::Ordering::Greater => (range_end, false),
        };
        let intersection = Self { lower, upper, lower_inclusive, upper_inclusive };
        Ok((!intersection.is_empty()).then_some(intersection))
    }

    fn map(self, map: crate::CompiledSourceTimeMap) -> Result<Self, TimelineTimeError> {
        let mapped_lower = map.map(self.lower)?;
        let mapped_upper = map.map(self.upper)?;
        Ok(if map.scale.numerator() < 0 {
            Self {
                lower: mapped_upper,
                upper: mapped_lower,
                lower_inclusive: self.upper_inclusive,
                upper_inclusive: self.lower_inclusive,
            }
        } else if map.scale.numerator() == 0 {
            Self {
                lower: mapped_lower,
                upper: mapped_lower,
                lower_inclusive: true,
                upper_inclusive: true,
            }
        } else {
            Self {
                lower: mapped_lower,
                upper: mapped_upper,
                lower_inclusive: self.lower_inclusive,
                upper_inclusive: self.upper_inclusive,
            }
        })
    }
}

pub(crate) fn selected_contribution_source_window(
    contribution: &crate::CompiledAudioContribution,
    window: DependencyWindow,
) -> Result<Option<DependencyWindow>, TimelineTimeError> {
    window
        .intersect_half_open(contribution.sequence_range)?
        .map(|intersection| intersection.map(contribution.source_time_map))
        .transpose()
}

pub(crate) fn select_program_window(
    program: &mut CompiledAudioProgram,
    window: DependencyWindow,
) -> Result<(), TimelineTimeError> {
    let mut retained = BTreeSet::new();
    let mut selected_contributions = Vec::with_capacity(program.contributions.len());
    for contribution in &program.contributions {
        let is_selected = window.intersect_half_open(contribution.sequence_range)?.is_some();
        if is_selected {
            retained.insert(contribution.edit_id);
            selected_contributions.push(contribution.clone());
        }
    }
    program.contributions = selected_contributions;
    let retained_scopes = program
        .contributions
        .iter()
        .map(|contribution| contribution.processing_scope)
        .collect::<BTreeSet<_>>();
    program
        .processing_scopes
        .retain(|scope_id, _| retained_scopes.contains(scope_id));
    program.transitions.retain(|transition| {
        retained.contains(&transition.left) && retained.contains(&transition.right)
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{AudioSourceComponentId, TimeScale, TimelineTime};
    use mondrian_timeline::audio::{AudioChannelStripOutputPort, AudioRouteSource};
    use mondrian_timeline::clip::Clip;

    fn tt(value: i64) -> TimelineTime {
        TimelineTime::new(value, 1).expect("test TimelineTime")
    }

    #[test]
    fn dependency_window_preserves_reverse_endpoint_semantics() {
        let mut sequence = Sequence::new("reverse dependency child");
        let track_id = sequence.audio_tracks[0].id;
        let before = AssetId::new();
        let selected = AssetId::new();
        sequence
            .add_media_audio_clip(
                track_id,
                Clip::new(before, tt(0), tt(2)).expect("before Clip"),
                AudioSourceComponentId::primary(),
            )
            .expect("before audio");
        sequence
            .add_media_audio_clip(
                track_id,
                Clip::new(selected, tt(2), tt(2)).expect("selected Clip"),
                AudioSourceComponentId::primary(),
            )
            .expect("selected audio");

        let child_output = sequence.audio_program.outputs[0].id;
        let mut root = Sequence::new("reverse dependency root");
        let root_track = root.audio_tracks[0].id;
        let mut nested =
            Clip::new_nested_sequence(sequence.id, tt(0), tt(2), Some("child".to_owned()))
                .expect("nested Clip");
        nested
            .set_constant_source_time_map(tt(4), TimeScale::new(-1, 1).expect("reverse scale"))
            .expect("reverse nested map");
        root.add_nested_audio_clip(root_track, nested, child_output)
            .expect("nested audio");

        let closure = compile_audio_dependency_closure(
            &root,
            &[sequence],
            None,
            TimelineTimeRange::new(tt(0), tt(2)).expect("root range"),
        )
        .expect("dependency closure");
        assert!(closure.media_components().contains_key(&selected));
        assert!(!closure.media_components().contains_key(&before));
    }

    #[test]
    fn muted_program_track_has_no_media_dependency() {
        let mut sequence = Sequence::new("muted dependency");
        let asset_id = AssetId::new();
        let track_id = sequence.audio_tracks[0].id;
        sequence
            .add_media_audio_clip(
                track_id,
                Clip::new(asset_id, tt(0), tt(4)).expect("Clip"),
                AudioSourceComponentId::primary(),
            )
            .expect("audio Clip");
        sequence.audio_tracks[0].is_muted = true;
        let closure = compile_audio_dependency_closure(
            &sequence,
            &[],
            None,
            TimelineTimeRange::new(tt(0), tt(4)).expect("range"),
        )
        .expect("dependency closure");
        assert!(closure.media_components().is_empty());
        assert_eq!(closure.sequence_ids(), &BTreeSet::from([sequence.id]));
        assert_eq!(
            closure.root_program().output_id(),
            sequence.audio_program.outputs[0].id
        );
        assert_eq!(
            closure.execution_demand(),
            AudioProgramExecutionDemand::ProvenSilent
        );
    }

    #[test]
    fn muted_track_pre_mute_send_remains_a_selected_media_dependency() {
        let mut sequence = Sequence::new("pre-mute dependency");
        let asset_id = AssetId::new();
        let track_id = sequence.audio_tracks[0].id;
        sequence
            .add_media_audio_clip(
                track_id,
                Clip::new(asset_id, tt(0), tt(4)).expect("Clip"),
                AudioSourceComponentId::primary(),
            )
            .expect("audio Clip");
        sequence.audio_tracks[0].is_muted = true;
        sequence.audio_program.routes[0].source = AudioRouteSource::Track {
            track_id,
            port: AudioChannelStripOutputPort::PostFaderPreMute,
        };

        let closure = compile_audio_dependency_closure(
            &sequence,
            &[],
            None,
            TimelineTimeRange::new(tt(0), tt(4)).expect("range"),
        )
        .expect("dependency closure");
        assert_eq!(
            closure.media_components().get(&asset_id),
            Some(&BTreeSet::from([AudioSourceComponentId::primary()]))
        );
    }

    #[test]
    fn audio_track_visibility_does_not_remove_a_media_dependency() {
        let mut sequence = Sequence::new("visibility-independent dependency");
        let asset_id = AssetId::new();
        let track_id = sequence.audio_tracks[0].id;
        sequence
            .add_media_audio_clip(
                track_id,
                Clip::new(asset_id, tt(0), tt(4)).expect("Clip"),
                AudioSourceComponentId::primary(),
            )
            .expect("audio Clip");
        sequence.audio_tracks[0].is_visible = false;

        let closure = compile_audio_dependency_closure(
            &sequence,
            &[],
            None,
            TimelineTimeRange::new(tt(0), tt(4)).expect("range"),
        )
        .expect("dependency closure");
        assert_eq!(
            closure.media_components().get(&asset_id),
            Some(&BTreeSet::from([AudioSourceComponentId::primary()]))
        );
    }

    #[test]
    fn zero_scale_maps_a_non_empty_parent_window_to_one_source_point() {
        let range = TimelineTimeRange::new(tt(3), tt(2)).expect("range");
        let window = DependencyWindow::from_half_open(range).expect("window");
        let map = crate::CompiledSourceTimeMap {
            sequence_start: tt(0),
            source_origin: tt(7),
            scale: TimeScale::new(0, 1).expect("freeze scale"),
            sampling_boundary: mondrian_core::SourceSamplingBoundary::Covering,
        };
        let mapped = window.map(map).expect("mapped");
        assert_eq!(mapped.lower, tt(7));
        assert_eq!(mapped.upper, tt(7));
        assert!(mapped.lower_inclusive && mapped.upper_inclusive);
    }
}
