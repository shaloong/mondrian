//! Derived cross-Sequence dependency validation.
//!
//! The authoring model remains the sole source of truth. This Module compiles
//! only stable identities, public-output contracts, and nesting obligations so
//! an `AuthoringSession` can validate one Sequence replacement without
//! traversing unrelated Clip bodies.

use crate::audio::{AudioComponentChannelMapping, AudioComponentSource};
use crate::{Sequence, SequenceCollection};
use mondrian_core::{
    AudioChannelLayout, AudioComponentEditId, AuthoringAllocationId, AuthoringList, ClipId,
    MondrianError, ProgramOutputId, Result, SequenceId, SequenceRevision,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Opaque process-local certificate for one validated Sequence dependency graph.
///
/// The certificate has no persistence representation. It strongly retains the
/// exact Sequence-list COW baseline that produced its private facts so cached
/// allocation identities can never outlive their author state. Active
/// navigation is deliberately not anchored, but its target must exist whenever
/// the certificate is reused.
#[derive(Debug, Clone, PartialEq)]
pub struct SequenceDependencyCertificate {
    baseline_sequences: AuthoringList<Sequence>,
    default_sequence_id: SequenceId,
    freshness: Arc<BTreeMap<SequenceId, SequenceRevision>>,
    facts: Arc<BTreeMap<SequenceId, Arc<SequenceDependencyFacts>>>,
    parents_by_child: Arc<BTreeMap<SequenceId, Arc<BTreeSet<SequenceId>>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SequenceDependencyFacts {
    sequence_id: SequenceId,
    outgoing: Vec<SequenceId>,
    nested_output_obligations: Vec<NestedOutputObligation>,
    public_outputs: BTreeMap<ProgramOutputId, AudioChannelLayout>,
    track_facts: BTreeMap<TrackFactKey, Arc<TrackDependencyFacts>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum TrackFactDomain {
    Video,
    Audio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TrackFactKey {
    domain: TrackFactDomain,
    clips_allocation_id: AuthoringAllocationId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackDependencyFacts {
    outgoing: Vec<SequenceId>,
    nested_output_obligations: Vec<TrackNestedOutputObligation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TrackNestedOutputObligation {
    clip_id: ClipId,
    edit_id: AudioComponentEditId,
    child_sequence_id: SequenceId,
    output_id: ProgramOutputId,
    explicit_matrix_source_layout: Option<AudioChannelLayout>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NestedOutputObligation {
    parent_sequence_id: SequenceId,
    clip_id: ClipId,
    edit_id: AudioComponentEditId,
    child_sequence_id: SequenceId,
    output_id: ProgramOutputId,
    explicit_matrix_source_layout: Option<AudioChannelLayout>,
}

impl SequenceDependencyCertificate {
    /// Extract and validate the complete dependency closure of a collection.
    ///
    /// Callers remain responsible for each Sequence's local author contract.
    /// This constructor validates collection identities, nested references,
    /// public-output bindings, and acyclic nesting.
    pub fn build(collection: &SequenceCollection) -> Result<Self> {
        let mut freshness = BTreeMap::new();
        let mut facts = BTreeMap::new();
        for sequence in &collection.sequences {
            if freshness.insert(sequence.id, sequence.revision).is_some() {
                return Err(dependency_error(
                    "validate_author_identities",
                    "duplicate Sequence identity in project document",
                ));
            }
            facts.insert(
                sequence.id,
                Arc::new(extract_sequence_facts(sequence, None)?),
            );
        }
        validate_collection_anchor(collection.default_sequence_id, "default", &facts)?;
        validate_collection_anchor(collection.active_sequence_id, "active", &facts)?;
        for sequence_facts in facts.values() {
            validate_sequence_fact_contract(sequence_facts, &facts, None)?;
        }
        validate_acyclic_graph(&facts)?;
        let parents_by_child = build_reverse_edges(&facts);
        Ok(Self {
            baseline_sequences: collection.sequences.clone(),
            default_sequence_id: collection.default_sequence_id,
            freshness: Arc::new(freshness),
            facts: Arc::new(facts),
            parents_by_child: Arc::new(parents_by_child),
        })
    }

    /// Prove that this certificate still belongs to the exact canonical
    /// Sequence author baseline.
    ///
    /// Active navigation may change between calls, but its target must still
    /// exist. Sequence order, default identity, revisions, and complete
    /// authored bodies are anchored by the retained copy-on-write baseline.
    pub fn validate_baseline(&self, current: &SequenceCollection) -> Result<()> {
        self.validated_freshness(current).map(|_| ())
    }

    /// Whether `sequences` is the exact outer COW root retained by this
    /// certificate.
    #[doc(hidden)]
    pub fn shares_baseline_root_with(&self, sequences: &AuthoringList<Sequence>) -> bool {
        self.baseline_sequences.shares_allocation_with(sequences)
    }

    /// Prepare a validated replacement index without mutating this index.
    ///
    /// The current collection is consulted only for its identity/revision
    /// freshness evidence. Exactly one Sequence body—the replacement—is
    /// extracted. Success returns a complete index that can be installed by
    /// assignment at the same commit boundary as the document and History.
    pub fn prepare_replacement(
        &self,
        current: &SequenceCollection,
        replacement: &Sequence,
    ) -> Result<Self> {
        let target_index = current
            .sequences
            .iter()
            .position(|sequence| sequence.id == replacement.id)
            .ok_or_else(|| {
                dependency_error(
                    "validate_sequence_replacement",
                    format!(
                        "replacement Sequence does not exist in the collection: {}",
                        replacement.id
                    ),
                )
            })?;
        let mut next_sequences = current.sequences.clone();
        next_sequences[target_index] = replacement.clone();
        self.prepare_replacement_with_baseline(current, replacement, &next_sequences)
    }

    /// Prepare a replacement while anchoring the returned index to an already
    /// assembled canonical Sequence-list root.
    ///
    /// This Interface lets the Project transaction construct the outer COW
    /// root exactly once and install that same root with the returned
    /// validation evidence. The supplied list is verified to differ from the
    /// current baseline only at the replacement slot.
    pub fn prepare_replacement_with_baseline(
        &self,
        current: &SequenceCollection,
        replacement: &Sequence,
        next_sequences: &AuthoringList<Sequence>,
    ) -> Result<Self> {
        let mut next_freshness = self.validated_freshness(current)?;
        let old_facts = self.facts.get(&replacement.id).ok_or_else(|| {
            dependency_error(
                "validate_sequence_replacement",
                format!(
                    "replacement Sequence does not exist in the collection: {}",
                    replacement.id
                ),
            )
        })?;
        if next_sequences.len() != current.sequences.len() {
            return Err(dependency_error(
                "validate_sequence_replacement",
                "prepared Sequence list changed collection cardinality",
            ));
        }
        for (current_sequence, next_sequence) in current.sequences.iter().zip(next_sequences) {
            let expected = if current_sequence.id == replacement.id {
                replacement
            } else {
                current_sequence
            };
            if next_sequence != expected {
                return Err(dependency_error(
                    "validate_sequence_replacement",
                    "prepared Sequence list changed state outside the replacement slot",
                ));
            }
        }
        let replacement_facts = extract_sequence_facts(replacement, Some(old_facts))?;

        validate_sequence_fact_contract(&replacement_facts, &self.facts, Some(&replacement_facts))?;
        self.validate_inbound_output_obligations(&replacement_facts)?;
        self.validate_replacement_cycle(&replacement_facts)?;

        let mut next = self.clone();
        next.replace_reverse_edges(old_facts, &replacement_facts);
        next.baseline_sequences = next_sequences.clone();
        next.default_sequence_id = current.default_sequence_id;
        next_freshness.insert(replacement.id, replacement.revision);
        next.freshness = Arc::new(next_freshness);
        Arc::make_mut(&mut next.facts).insert(replacement.id, Arc::new(replacement_facts));
        Ok(next)
    }

    fn validated_freshness(
        &self,
        current: &SequenceCollection,
    ) -> Result<BTreeMap<SequenceId, SequenceRevision>> {
        if self.default_sequence_id != current.default_sequence_id
            || self.baseline_sequences != current.sequences
        {
            return Err(dependency_error(
                "validate_sequence_dependency_certificate_freshness",
                "Sequence dependency certificate does not match the canonical author baseline",
            ));
        }
        if current.sequence(current.active_sequence_id).is_none() {
            return Err(dependency_error(
                "validate_sequence_dependency_certificate_freshness",
                "active Sequence does not exist in the canonical collection",
            ));
        }
        let current_freshness = current
            .sequences
            .iter()
            .map(|sequence| (sequence.id, sequence.revision))
            .collect::<BTreeMap<_, _>>();
        if current_freshness != *self.freshness
            || current_freshness.len() != current.sequences.len()
        {
            return Err(dependency_error(
                "validate_sequence_dependency_certificate_freshness",
                "Sequence dependency certificate does not match the canonical identity/revision set",
            ));
        }
        Ok(current_freshness)
    }

    fn validate_inbound_output_obligations(
        &self,
        replacement: &SequenceDependencyFacts,
    ) -> Result<()> {
        let Some(parent_ids) = self.parents_by_child.get(&replacement.sequence_id) else {
            return Ok(());
        };
        for parent_id in parent_ids.iter() {
            if *parent_id == replacement.sequence_id {
                continue;
            }
            let parent = self.facts.get(parent_id).ok_or_else(|| {
                dependency_error(
                    "validate_sequence_dependency_certificate",
                    format!("indexed parent Sequence does not exist: {parent_id}"),
                )
            })?;
            for obligation in parent
                .nested_output_obligations
                .iter()
                .filter(|obligation| obligation.child_sequence_id == replacement.sequence_id)
            {
                validate_nested_output_obligation(obligation, replacement)?;
            }
        }
        Ok(())
    }

    fn validate_replacement_cycle(&self, replacement: &SequenceDependencyFacts) -> Result<()> {
        let mut ancestors = BTreeSet::new();
        let mut pending = vec![replacement.sequence_id];
        while let Some(child_id) = pending.pop() {
            let Some(parent_ids) = self.parents_by_child.get(&child_id) else {
                continue;
            };
            for parent_id in parent_ids.iter() {
                if ancestors.insert(*parent_id) {
                    pending.push(*parent_id);
                }
            }
        }

        if replacement
            .outgoing
            .iter()
            .any(|child_id| *child_id == replacement.sequence_id || ancestors.contains(child_id))
        {
            return Err(cycle_error(replacement.sequence_id));
        }
        Ok(())
    }

    fn replace_reverse_edges(
        &mut self,
        previous: &SequenceDependencyFacts,
        replacement: &SequenceDependencyFacts,
    ) {
        if previous.outgoing == replacement.outgoing {
            return;
        }
        let parents_by_child = Arc::make_mut(&mut self.parents_by_child);
        for child_id in previous
            .outgoing
            .iter()
            .filter(|child_id| replacement.outgoing.binary_search(child_id).is_err())
        {
            let remove_entry = if let Some(parents) = parents_by_child.get_mut(child_id) {
                let parents = Arc::make_mut(parents);
                parents.remove(&previous.sequence_id);
                parents.is_empty()
            } else {
                false
            };
            if remove_entry {
                parents_by_child.remove(child_id);
            }
        }
        for child_id in replacement
            .outgoing
            .iter()
            .filter(|child_id| previous.outgoing.binary_search(child_id).is_err())
        {
            Arc::make_mut(
                parents_by_child.entry(*child_id).or_insert_with(|| Arc::new(BTreeSet::new())),
            )
            .insert(replacement.sequence_id);
        }
    }
}

fn extract_sequence_facts(
    sequence: &Sequence,
    previous: Option<&SequenceDependencyFacts>,
) -> Result<SequenceDependencyFacts> {
    record_fact_extraction();
    let mut outgoing = BTreeSet::new();
    let mut track_facts = BTreeMap::new();
    for track in &sequence.video_tracks {
        let key = TrackFactKey {
            domain: TrackFactDomain::Video,
            clips_allocation_id: track.clips.allocation_id(),
        };
        let facts = extract_or_reuse_track_facts(key, track, previous)?;
        outgoing.extend(facts.outgoing.iter().copied());
        track_facts.insert(key, facts);
    }

    let mut nested_output_obligations = Vec::new();
    for track in &sequence.audio_tracks {
        let key = TrackFactKey {
            domain: TrackFactDomain::Audio,
            clips_allocation_id: track.clips.allocation_id(),
        };
        let facts = extract_or_reuse_track_facts(key, track, previous)?;
        outgoing.extend(facts.outgoing.iter().copied());
        nested_output_obligations.extend(facts.nested_output_obligations.iter().map(
            |obligation| NestedOutputObligation {
                parent_sequence_id: sequence.id,
                clip_id: obligation.clip_id,
                edit_id: obligation.edit_id,
                child_sequence_id: obligation.child_sequence_id,
                output_id: obligation.output_id,
                explicit_matrix_source_layout: obligation.explicit_matrix_source_layout,
            },
        ));
        track_facts.insert(key, facts);
    }

    let public_outputs = sequence
        .audio_program
        .outputs
        .iter()
        .map(|output| (output.id, sequence.settings.audio_channel_layout))
        .collect();
    Ok(SequenceDependencyFacts {
        sequence_id: sequence.id,
        outgoing: outgoing.into_iter().collect(),
        nested_output_obligations,
        public_outputs,
        track_facts,
    })
}

fn extract_or_reuse_track_facts(
    key: TrackFactKey,
    track: &crate::Track,
    previous: Option<&SequenceDependencyFacts>,
) -> Result<Arc<TrackDependencyFacts>> {
    if let Some(facts) = previous.and_then(|facts| facts.track_facts.get(&key)) {
        return Ok(Arc::clone(facts));
    }

    record_track_fact_extraction(key.domain);
    let mut outgoing = BTreeSet::new();
    let mut nested_output_obligations = Vec::new();
    for clip in &track.clips {
        if let Some(child_id) = clip.nested_sequence_id() {
            outgoing.insert(child_id);
        }
        if key.domain == TrackFactDomain::Video {
            continue;
        }
        for edit in &clip.audio_components {
            let AudioComponentSource::NestedOutput { output_id } = &edit.source else {
                continue;
            };
            let child_sequence_id = clip.nested_sequence_id().ok_or_else(|| {
                dependency_error(
                    "validate_audio_program",
                    format!(
                        "nested audio edit {} on Clip {} has no owning Sequence",
                        edit.id, clip.id
                    ),
                )
            })?;
            let explicit_matrix_source_layout = match &edit.channel_mapping {
                AudioComponentChannelMapping::Standard => None,
                AudioComponentChannelMapping::Explicit(matrix) => Some(matrix.source_layout()),
            };
            nested_output_obligations.push(TrackNestedOutputObligation {
                clip_id: clip.id,
                edit_id: edit.id,
                child_sequence_id,
                output_id: *output_id,
                explicit_matrix_source_layout,
            });
        }
    }

    Ok(Arc::new(TrackDependencyFacts {
        outgoing: outgoing.into_iter().collect(),
        nested_output_obligations,
    }))
}

fn validate_collection_anchor(
    sequence_id: SequenceId,
    role: &str,
    facts: &BTreeMap<SequenceId, Arc<SequenceDependencyFacts>>,
) -> Result<()> {
    if facts.contains_key(&sequence_id) {
        return Ok(());
    }
    Err(dependency_error(
        "validate_sequence_collection",
        format!("{role} Sequence does not exist: {sequence_id}"),
    ))
}

fn validate_sequence_fact_contract(
    sequence: &SequenceDependencyFacts,
    all_facts: &BTreeMap<SequenceId, Arc<SequenceDependencyFacts>>,
    replacement: Option<&SequenceDependencyFacts>,
) -> Result<()> {
    for child_id in &sequence.outgoing {
        if lookup_facts(*child_id, all_facts, replacement).is_none() {
            return Err(dependency_error(
                "validate_nested_sequences",
                format!("嵌套序列不存在: {child_id}"),
            ));
        }
    }
    for obligation in &sequence.nested_output_obligations {
        let child = lookup_facts(obligation.child_sequence_id, all_facts, replacement).ok_or_else(
            || {
                dependency_error(
                    "validate_audio_program",
                    format!(
                        "nested audio Sequence does not exist: {}",
                        obligation.child_sequence_id
                    ),
                )
            },
        )?;
        validate_nested_output_obligation(obligation, child)?;
    }
    Ok(())
}

fn lookup_facts<'a>(
    sequence_id: SequenceId,
    facts: &'a BTreeMap<SequenceId, Arc<SequenceDependencyFacts>>,
    replacement: Option<&'a SequenceDependencyFacts>,
) -> Option<&'a SequenceDependencyFacts> {
    replacement
        .filter(|candidate| candidate.sequence_id == sequence_id)
        .or_else(|| facts.get(&sequence_id).map(Arc::as_ref))
}

fn validate_nested_output_obligation(
    obligation: &NestedOutputObligation,
    child: &SequenceDependencyFacts,
) -> Result<()> {
    let Some(output_layout) = child.public_outputs.get(&obligation.output_id).copied() else {
        return Err(dependency_error(
            "validate_audio_program",
            format!(
                "nested audio output does not exist: output {}, parent Sequence {}, Clip {}, edit {}, child Sequence {}",
                obligation.output_id,
                obligation.parent_sequence_id,
                obligation.clip_id,
                obligation.edit_id,
                obligation.child_sequence_id
            ),
        ));
    };
    if obligation
        .explicit_matrix_source_layout
        .is_some_and(|source_layout| source_layout != output_layout)
    {
        return Err(dependency_error(
            "validate_audio_program",
            format!(
                "nested audio edit {} matrix source layout does not match child Sequence {}",
                obligation.edit_id, obligation.child_sequence_id
            ),
        ));
    }
    Ok(())
}

fn build_reverse_edges(
    facts: &BTreeMap<SequenceId, Arc<SequenceDependencyFacts>>,
) -> BTreeMap<SequenceId, Arc<BTreeSet<SequenceId>>> {
    let mut parents_by_child = BTreeMap::<SequenceId, BTreeSet<SequenceId>>::new();
    for sequence in facts.values() {
        for child_id in &sequence.outgoing {
            parents_by_child.entry(*child_id).or_default().insert(sequence.sequence_id);
        }
    }
    parents_by_child
        .into_iter()
        .map(|(child_id, parents)| (child_id, Arc::new(parents)))
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DependencyVisitState {
    Visiting,
    Complete,
}

fn validate_acyclic_graph(
    facts: &BTreeMap<SequenceId, Arc<SequenceDependencyFacts>>,
) -> Result<()> {
    let mut visit_state = BTreeMap::new();
    for root in facts.keys() {
        if has_cycle(*root, facts, &mut visit_state) {
            return Err(cycle_error(*root));
        }
    }
    Ok(())
}

fn has_cycle(
    root: SequenceId,
    graph: &BTreeMap<SequenceId, Arc<SequenceDependencyFacts>>,
    visit_state: &mut BTreeMap<SequenceId, DependencyVisitState>,
) -> bool {
    if visit_state.get(&root) == Some(&DependencyVisitState::Complete) {
        return false;
    }

    visit_state.insert(root, DependencyVisitState::Visiting);
    let mut stack = vec![(root, 0usize)];
    while let Some((node, next_child_index)) = stack.last_mut() {
        let children = graph.get(node).map(|facts| facts.outgoing.as_slice()).unwrap_or_default();
        if *next_child_index == children.len() {
            let completed = *node;
            stack.pop();
            visit_state.insert(completed, DependencyVisitState::Complete);
            continue;
        }

        let child = children[*next_child_index];
        *next_child_index += 1;
        match visit_state.get(&child).copied() {
            Some(DependencyVisitState::Visiting) => return true,
            Some(DependencyVisitState::Complete) => {}
            None => {
                visit_state.insert(child, DependencyVisitState::Visiting);
                stack.push((child, 0));
            }
        }
    }
    false
}

fn cycle_error(root: SequenceId) -> MondrianError {
    dependency_error(
        "validate_nested_sequences",
        format!("检测到序列嵌套循环: {root}"),
    )
}

fn dependency_error(step_id: &str, reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed { step_id: step_id.to_owned(), reason: reason.into() }
}

#[cfg(test)]
thread_local! {
    static FACT_EXTRACTION_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static VIDEO_TRACK_FACT_EXTRACTION_COUNT: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    static AUDIO_TRACK_FACT_EXTRACTION_COUNT: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn record_fact_extraction() {
    FACT_EXTRACTION_COUNT.with(|count| count.set(count.get() + 1));
}

#[cfg(not(test))]
fn record_fact_extraction() {}

#[cfg(test)]
fn record_track_fact_extraction(domain: TrackFactDomain) {
    let counter = match domain {
        TrackFactDomain::Video => &VIDEO_TRACK_FACT_EXTRACTION_COUNT,
        TrackFactDomain::Audio => &AUDIO_TRACK_FACT_EXTRACTION_COUNT,
    };
    counter.with(|count| count.set(count.get() + 1));
}

#[cfg(not(test))]
fn record_track_fact_extraction(_domain: TrackFactDomain) {}

#[cfg(test)]
fn reset_fact_extraction_count() {
    FACT_EXTRACTION_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
fn fact_extraction_count() -> usize {
    FACT_EXTRACTION_COUNT.with(std::cell::Cell::get)
}

#[cfg(test)]
fn reset_track_fact_extraction_counts() {
    VIDEO_TRACK_FACT_EXTRACTION_COUNT.with(|count| count.set(0));
    AUDIO_TRACK_FACT_EXTRACTION_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
fn track_fact_extraction_counts() -> (usize, usize) {
    (
        VIDEO_TRACK_FACT_EXTRACTION_COUNT.with(std::cell::Cell::get),
        AUDIO_TRACK_FACT_EXTRACTION_COUNT.with(std::cell::Cell::get),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioComponentChannelMapping;
    use crate::Clip;
    use mondrian_core::{AssetId, AudioChannelMixMatrix, TimelineTime, TrackId};

    fn nested_clip(child_id: SequenceId) -> Clip {
        Clip::new_nested_sequence(child_id, TimelineTime::ZERO, TimelineTime::ONE, None)
            .expect("valid nested Clip")
    }

    fn media_clip(position: i64) -> Clip {
        Clip::new(
            AssetId::new(),
            TimelineTime::new(position, 1).expect("exact position"),
            TimelineTime::ONE,
        )
        .expect("valid media Clip")
    }

    fn sequence_with_two_populated_tracks_per_domain(name: &str) -> Sequence {
        let mut sequence = Sequence::new(name);
        sequence.video_tracks[0].add_clip(media_clip(0)).expect("first video Clip");
        sequence.audio_tracks[0].add_clip(media_clip(0)).expect("first audio Clip");
        let second_video = sequence.add_video_track();
        sequence
            .video_track_mut(second_video)
            .expect("second video Track")
            .add_clip(media_clip(0))
            .expect("second video Clip");
        let second_audio = sequence.add_audio_track();
        sequence
            .audio_track_mut(second_audio)
            .expect("second audio Track")
            .add_clip(media_clip(0))
            .expect("second audio Clip");
        sequence
    }

    fn replace_in_clone(
        collection: &SequenceCollection,
        replacement: &Sequence,
    ) -> SequenceCollection {
        let mut candidate = collection.clone();
        let stored = candidate.sequence_mut(replacement.id).expect("replacement identity exists");
        *stored = replacement.clone();
        candidate
    }

    fn assert_incremental_matches_full(collection: &SequenceCollection, replacement: &Sequence) {
        let index = SequenceDependencyCertificate::build(collection).expect("valid base index");
        let incremental = index.prepare_replacement(collection, replacement);
        let full = replace_in_clone(collection, replacement).validate_dependency_closure();
        assert_eq!(
            incremental.is_ok(),
            full.is_ok(),
            "incremental and full dependency validation must agree"
        );
    }

    #[test]
    fn replacement_extracts_only_the_candidate_sequence_body() {
        let mut sequences = (0..32)
            .map(|index| Sequence::new(format!("Sequence {index}")))
            .collect::<Vec<_>>();
        let child_id = sequences[1].id;
        sequences[0].video_tracks[0]
            .add_clip(nested_clip(child_id))
            .expect("place nested Clip");
        for position in 0..256 {
            sequences[31].video_tracks[0]
                .add_clip(
                    Clip::new(
                        AssetId::new(),
                        TimelineTime::new(position, 1).expect("exact position"),
                        TimelineTime::ONE,
                    )
                    .expect("unrelated media Clip"),
                )
                .expect("place unrelated media Clip");
        }
        let first = sequences.remove(0);
        let mut collection = SequenceCollection::new(first);
        for sequence in sequences {
            collection.add_sequence(sequence).expect("unique Sequence");
        }
        let index = SequenceDependencyCertificate::build(&collection).expect("valid index");
        let mut replacement = collection.sequences[0].clone();
        replacement.name = "Renamed".to_owned();

        reset_fact_extraction_count();
        index.prepare_replacement(&collection, &replacement).expect("valid replacement");

        assert_eq!(fact_extraction_count(), 1);
    }

    #[test]
    fn replacement_reuses_unchanged_video_and_audio_track_clip_facts() {
        let sequence = sequence_with_two_populated_tracks_per_domain("Root");
        let collection = SequenceCollection::new(sequence.clone());
        let index = SequenceDependencyCertificate::build(&collection).expect("valid index");
        let unchanged_video_allocation = sequence.video_tracks[1].clips.allocation_id();
        let unchanged_audio_allocation = sequence.audio_tracks[1].clips.allocation_id();
        let changed_video_allocation = sequence.video_tracks[0].clips.allocation_id();
        let changed_audio_allocation = sequence.audio_tracks[0].clips.allocation_id();
        let mut replacement = sequence.clone();

        replacement.video_tracks[0]
            .add_clip(media_clip(2))
            .expect("detached changed video Track");
        replacement.audio_tracks[0]
            .add_clip(media_clip(2))
            .expect("detached changed audio Track");
        assert_ne!(
            replacement.video_tracks[0].clips.allocation_id(),
            changed_video_allocation
        );
        assert_ne!(
            replacement.audio_tracks[0].clips.allocation_id(),
            changed_audio_allocation
        );
        assert_eq!(
            replacement.video_tracks[1].clips.allocation_id(),
            unchanged_video_allocation
        );
        assert_eq!(
            replacement.audio_tracks[1].clips.allocation_id(),
            unchanged_audio_allocation
        );

        reset_track_fact_extraction_counts();
        let next = index.prepare_replacement(&collection, &replacement).expect("valid replacement");

        assert_eq!(track_fact_extraction_counts(), (1, 1));
        let previous = index.facts.get(&sequence.id).expect("previous facts");
        let replacement = next.facts.get(&sequence.id).expect("replacement facts");
        for key in [
            TrackFactKey {
                domain: TrackFactDomain::Video,
                clips_allocation_id: unchanged_video_allocation,
            },
            TrackFactKey {
                domain: TrackFactDomain::Audio,
                clips_allocation_id: unchanged_audio_allocation,
            },
        ] {
            assert!(Arc::ptr_eq(
                previous.track_facts.get(&key).expect("previous Track facts"),
                replacement.track_facts.get(&key).expect("reused Track facts")
            ));
        }
    }

    #[test]
    fn nested_clip_and_output_edits_detach_and_replace_exact_track_facts() {
        let child_a = Sequence::new("Child A");
        let mut child_b = Sequence::new("Child B");
        let second_output_id = ProgramOutputId::new();
        let mut second_output = child_b.audio_program.outputs[0].clone();
        second_output.id = second_output_id;
        child_b.audio_program.outputs.push(second_output);

        let mut parent = Sequence::new("Parent");
        parent.video_tracks[0]
            .add_clip(nested_clip(child_a.id))
            .expect("video nesting edge");
        parent
            .add_nested_audio_clip(
                parent.audio_tracks[0].id,
                nested_clip(child_b.id),
                child_b.audio_program.outputs[0].id,
            )
            .expect("nested audio output");
        let original_video_allocation = parent.video_tracks[0].clips.allocation_id();
        let original_audio_allocation = parent.audio_tracks[0].clips.allocation_id();
        let mut collection = SequenceCollection::new(parent.clone());
        collection.add_sequence(child_a).expect("first child");
        collection.add_sequence(child_b.clone()).expect("second child");
        let index = SequenceDependencyCertificate::build(&collection).expect("valid index");
        let mut replacement = parent.clone();
        replacement.video_tracks[0].clips[0] = nested_clip(child_b.id);
        replacement.audio_tracks[0].clips[0].audio_components[0].source =
            AudioComponentSource::NestedOutput { output_id: second_output_id };

        assert_ne!(
            replacement.video_tracks[0].clips.allocation_id(),
            original_video_allocation
        );
        assert_ne!(
            replacement.audio_tracks[0].clips.allocation_id(),
            original_audio_allocation
        );
        reset_track_fact_extraction_counts();
        let next = index.prepare_replacement(&collection, &replacement).expect("valid replacement");

        assert_eq!(track_fact_extraction_counts(), (1, 1));
        let replacement_facts = next.facts.get(&parent.id).expect("replacement facts");
        assert_eq!(replacement_facts.outgoing, vec![child_b.id]);
        assert_eq!(replacement_facts.nested_output_obligations.len(), 1);
        assert_eq!(
            replacement_facts.nested_output_obligations[0].output_id,
            second_output_id
        );
    }

    #[test]
    fn track_reorder_and_video_track_identity_change_reuse_clip_facts() {
        let sequence = sequence_with_two_populated_tracks_per_domain("Root");
        let collection = SequenceCollection::new(sequence.clone());
        let index = SequenceDependencyCertificate::build(&collection).expect("valid index");
        let mut replacement = sequence.clone();
        replacement.video_tracks.swap(0, 1);
        replacement.audio_tracks.swap(0, 1);
        replacement.video_tracks[0].id = TrackId::new();

        reset_track_fact_extraction_counts();
        let next = index.prepare_replacement(&collection, &replacement).expect("valid replacement");

        assert_eq!(track_fact_extraction_counts(), (0, 0));
        let previous = index.facts.get(&sequence.id).expect("previous facts");
        let replacement = next.facts.get(&sequence.id).expect("replacement facts");
        assert_eq!(
            previous.track_facts.keys().collect::<Vec<_>>(),
            replacement.track_facts.keys().collect::<Vec<_>>()
        );
        for (key, previous_track_facts) in &previous.track_facts {
            assert!(Arc::ptr_eq(
                previous_track_facts,
                replacement.track_facts.get(key).expect("reused Track facts")
            ));
        }
    }

    #[test]
    fn public_outputs_and_layout_are_recomputed_when_all_track_facts_are_reused() {
        let child = sequence_with_two_populated_tracks_per_domain("Child");
        let collection = SequenceCollection::new(child.clone());
        let index = SequenceDependencyCertificate::build(&collection).expect("valid index");
        let mut replacement = child.clone();
        replacement.settings.audio_channel_layout = AudioChannelLayout::Mono;
        let additional_output_id = ProgramOutputId::new();
        let mut additional_output = replacement.audio_program.outputs[0].clone();
        additional_output.id = additional_output_id;
        replacement.audio_program.outputs.push(additional_output);

        reset_track_fact_extraction_counts();
        let next = index.prepare_replacement(&collection, &replacement).expect("valid replacement");

        assert_eq!(track_fact_extraction_counts(), (0, 0));
        let replacement_facts = next.facts.get(&child.id).expect("replacement facts");
        assert_eq!(
            replacement_facts.public_outputs.get(&additional_output_id),
            Some(&AudioChannelLayout::Mono)
        );
        assert!(replacement_facts
            .public_outputs
            .values()
            .all(|layout| *layout == AudioChannelLayout::Mono));
    }

    #[test]
    fn undo_and_redo_reuse_every_unchanged_track_body() {
        let before = sequence_with_two_populated_tracks_per_domain("Root");
        let base_collection = SequenceCollection::new(before.clone());
        let base_index =
            SequenceDependencyCertificate::build(&base_collection).expect("valid index");
        let mut after = before.clone();
        after.video_tracks[0].add_clip(media_clip(2)).expect("edited video Track");
        after.audio_tracks[0].add_clip(media_clip(2)).expect("edited audio Track");

        reset_track_fact_extraction_counts();
        let after_index =
            base_index.prepare_replacement(&base_collection, &after).expect("forward edit");
        assert_eq!(track_fact_extraction_counts(), (1, 1));

        let after_collection = replace_in_clone(&base_collection, &after);
        reset_track_fact_extraction_counts();
        let undo_index = after_index
            .prepare_replacement(&after_collection, &before)
            .expect("Undo replacement");
        assert_eq!(track_fact_extraction_counts(), (1, 1));

        let undo_collection = replace_in_clone(&after_collection, &before);
        reset_track_fact_extraction_counts();
        undo_index
            .prepare_replacement(&undo_collection, &after)
            .expect("Redo replacement");
        assert_eq!(track_fact_extraction_counts(), (1, 1));
    }

    #[test]
    fn dependency_certificate_strongly_anchors_fact_input_allocations() {
        let sequence = sequence_with_two_populated_tracks_per_domain("Root");
        let mut collection = SequenceCollection::new(sequence);
        let certificate =
            SequenceDependencyCertificate::build(&collection).expect("valid certificate");
        let clips_allocation = collection.sequences[0].video_tracks[0].clips.allocation_id();

        collection.sequences[0].video_tracks[0]
            .add_clip(media_clip(2))
            .expect("mutate canonical fact input");
        assert_ne!(
            collection.sequences[0].video_tracks[0].clips.allocation_id(),
            clips_allocation,
            "the live certificate must keep the previous fact-input root shared"
        );
        assert!(certificate
            .validate_baseline(&collection)
            .expect_err("old certificate must reject detached fact input")
            .to_string()
            .contains("author baseline"));

        let mut replacement = collection.sequences[0].clone();
        replacement.name = "candidate".to_owned();
        assert!(certificate.prepare_replacement(&collection, &replacement).is_err());
    }

    #[test]
    fn prepared_dependency_certificate_anchors_the_installation_cow_root() {
        let sequence = sequence_with_two_populated_tracks_per_domain("Root");
        let collection = SequenceCollection::new(sequence.clone());
        let certificate =
            SequenceDependencyCertificate::build(&collection).expect("valid certificate");
        let mut replacement = sequence;
        replacement.name = "Renamed".to_owned();
        let mut next_sequences = collection.sequences.clone();
        next_sequences[0] = replacement;
        let replacement = &next_sequences[0];

        let next = certificate
            .prepare_replacement_with_baseline(&collection, replacement, &next_sequences)
            .expect("prepared replacement");

        assert!(next.baseline_sequences.shares_allocation_with(&next_sequences));
    }

    #[test]
    fn active_navigation_is_validated_but_not_anchored() {
        let first = Sequence::new("First");
        let second = Sequence::new("Second");
        let second_id = second.id;
        let mut collection = SequenceCollection::new(first);
        collection.add_sequence(second).expect("second Sequence");
        let certificate =
            SequenceDependencyCertificate::build(&collection).expect("valid certificate");

        collection.set_active(second_id).expect("navigate to second Sequence");

        certificate
            .validate_baseline(&collection)
            .expect("active navigation must not stale dependency evidence");
    }

    #[test]
    fn systematic_replacements_match_full_collection_validation() {
        let mut parent = Sequence::new("Parent");
        let child = Sequence::new("Child");
        let leaf = Sequence::new("Leaf");
        parent.video_tracks[0]
            .add_clip(nested_clip(child.id))
            .expect("parent child edge");
        let mut collection = SequenceCollection::new(parent.clone());
        collection.add_sequence(child.clone()).expect("child");
        collection.add_sequence(leaf.clone()).expect("leaf");

        let mut ordinary = parent.clone();
        ordinary.name = "Renamed parent".to_owned();
        assert_incremental_matches_full(&collection, &ordinary);

        let mut unknown_edge = parent.clone();
        unknown_edge.video_tracks[0]
            .add_clip(nested_clip(SequenceId::new()))
            .expect("unknown edge placement");
        assert_incremental_matches_full(&collection, &unknown_edge);

        let mut cycle = child.clone();
        cycle.video_tracks[0]
            .add_clip(nested_clip(parent.id))
            .expect("cycle edge placement");
        assert_incremental_matches_full(&collection, &cycle);

        let mut deeper = child;
        deeper.video_tracks[0]
            .add_clip(nested_clip(leaf.id))
            .expect("deeper edge placement");
        assert_incremental_matches_full(&collection, &deeper);
    }

    #[test]
    fn inbound_output_and_layout_replacements_match_full_validation() {
        let child = Sequence::new("Child");
        let child_output = child.audio_program.outputs[0].id;
        let mut parent = Sequence::new("Parent");
        let nested = nested_clip(child.id);
        parent
            .add_nested_audio_clip(parent.audio_tracks[0].id, nested, child_output)
            .expect("nested output binding");
        parent.audio_tracks[0].clips[0].audio_components[0].channel_mapping =
            AudioComponentChannelMapping::Explicit(AudioChannelMixMatrix::identity(
                AudioChannelLayout::Stereo,
            ));
        let mut collection = SequenceCollection::new(parent);
        collection.add_sequence(child.clone()).expect("child");

        let mut removed_output = child.clone();
        removed_output.audio_program.outputs[0].id = ProgramOutputId::new();
        assert_incremental_matches_full(&collection, &removed_output);

        let mut changed_layout = child;
        changed_layout.settings.audio_channel_layout = AudioChannelLayout::Mono;
        assert_incremental_matches_full(&collection, &changed_layout);
    }
}
