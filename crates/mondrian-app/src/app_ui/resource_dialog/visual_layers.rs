use super::{Localizer, VisualLayerAddress};
use mondrian_core::timeline_data::{
    ClipContent, TimelineClipEndpointContext, TimelineClipExecutionRef,
};
use mondrian_core::{
    ClipId, GradeGraphNodeKind, PropertyValue, SequenceId, TimelineTime, VisualSourceReference,
};
use mondrian_timeline::{PreparedVisualSchedule, Sequence};
use std::collections::{HashMap, HashSet};

pub(super) struct VisualLayerChoice {
    pub(super) address: VisualLayerAddress,
    pub(super) name: String,
}

impl VisualLayerChoice {
    pub(super) fn label(&self, localizer: &Localizer) -> String {
        let kind = match self.address {
            VisualLayerAddress::Clip(_) => "visual-source-clip-kind",
            VisualLayerAddress::Sequence(_) => "visual-source-sequence-kind",
        };
        format!("{} · {}", localizer.text(kind), self.name)
    }
}

pub(super) fn visual_layer_choices(
    sequence: &Sequence,
    sequences: &[Sequence],
    consumer: ClipId,
) -> mondrian_core::Result<Vec<VisualLayerChoice>> {
    let Some(clip) = sequence.find_clip(consumer) else {
        return Ok(Vec::new());
    };
    let schedule = PreparedVisualSchedule::compile(sequence)?;
    let consumer = TimelineClipExecutionRef {
        sequence_id: sequence.id,
        sequence_revision: sequence.revision,
        clip_id: consumer,
        clip_time: clip.clip_time_in,
        endpoint: TimelineClipEndpointContext::Ordinary,
    };
    let mut choices = sequence
        .video_tracks
        .iter()
        .flat_map(|track| {
            track.clips.iter().filter_map(|clip| {
                schedule.admit_auxiliary_clip(consumer, clip.id).ok()?;
                let label = clip.label.clone().unwrap_or_else(|| clip.position.to_string());
                Some(VisualLayerChoice {
                    address: VisualLayerAddress::Clip(clip.id),
                    name: format!("{} · {}", track.name, label),
                })
            })
        })
        .collect::<Vec<_>>();
    let edges = sequences
        .iter()
        .map(|sequence| (sequence.id, sequence_picture_sources(sequence)))
        .collect::<HashMap<_, _>>();
    for source in sequences {
        if source.total_duration()? > TimelineTime::ZERO
            && sequence_source_is_admitted(source.id, sequence.id, &edges)
        {
            choices.push(VisualLayerChoice {
                address: VisualLayerAddress::Sequence(source.id),
                name: source.name.clone(),
            });
        }
    }
    Ok(choices)
}

fn sequence_picture_sources(sequence: &Sequence) -> Vec<SequenceId> {
    let mut sources = Vec::new();
    let mut grades = sequence.timeline_grade.into_iter().collect::<HashSet<_>>();
    for track in sequence.video_tracks.iter().filter(|track| track.is_visible && !track.is_muted) {
        for clip in track.clips.iter().filter(|clip| !clip.is_disabled) {
            if let ClipContent::NestedSequence { sequence_id, .. } = clip.content {
                sources.push(sequence_id);
            }
            for effect in &clip.effects {
                add_effect_sources(effect, &mut sources);
            }
            grades.extend(clip.grade);
            if let Some(group) = clip
                .grade_group
                .and_then(|id| sequence.grade_groups.iter().find(|group| group.id == id))
            {
                grades.extend(group.pre_clip_grade);
                grades.extend(group.post_clip_grade);
            }
        }
    }
    for version in sequence
        .grade_definitions
        .iter()
        .filter(|definition| grades.contains(&definition.id))
        .filter_map(|definition| definition.active())
    {
        for node in &version.graph.nodes {
            if let GradeGraphNodeKind::Effect { effect, .. } = &node.kind {
                add_effect_sources(effect, &mut sources);
            }
        }
    }
    sources
}

fn add_effect_sources(
    effect: &mondrian_core::effect_data::EffectNode,
    sources: &mut Vec<SequenceId>,
) {
    if effect.is_enabled {
        sources.extend(effect.properties.iter().filter_map(|(_, property)| {
            match property.static_value() {
                PropertyValue::VisualSource(VisualSourceReference::Sequence {
                    sequence_id,
                    ..
                }) => Some(*sequence_id),
                _ => None,
            }
        }));
    }
}

fn sequence_source_is_admitted(
    source: SequenceId,
    consumer: SequenceId,
    edges: &HashMap<SequenceId, Vec<SequenceId>>,
) -> bool {
    let mut active = HashSet::new();
    let mut finished = HashSet::new();
    let mut work = vec![(source, false)];
    while let Some((sequence, leaving)) = work.pop() {
        if leaving {
            active.remove(&sequence);
            finished.insert(sequence);
        } else if !finished.contains(&sequence) {
            if sequence == consumer || !active.insert(sequence) {
                return false;
            }
            work.push((sequence, true));
            if let Some(children) = edges.get(&sequence) {
                work.extend(children.iter().map(|&child| (child, false)));
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::effect_data::{EffectNode, EffectType};
    use mondrian_core::{AssetId, Color, PropertyDescriptor};
    use mondrian_timeline::clip::Clip;

    fn solid() -> Clip {
        Clip::new_solid_color(
            AssetId::new(),
            Color::from_hex(0xffffff),
            TimelineTime::ZERO,
            TimelineTime::ONE,
        )
        .unwrap()
    }

    #[test]
    fn a_later_layer_cannot_be_used_as_a_second_input() {
        let mut sequence = Sequence::new("Consumer");
        let [earlier, consumer, later] = [solid(), solid(), solid()];
        let (earlier_id, consumer_id, later_id) = (earlier.id, consumer.id, later.id);
        sequence.video_tracks[0].add_clip(earlier).unwrap();
        sequence.video_tracks[1].add_clip(consumer).unwrap();
        sequence.video_tracks[2].add_clip(later).unwrap();
        let choices =
            visual_layer_choices(&sequence, std::slice::from_ref(&sequence), consumer_id).unwrap();
        let addresses = choices.into_iter().map(|choice| choice.address).collect::<Vec<_>>();
        assert_eq!(addresses, vec![VisualLayerAddress::Clip(earlier_id)]);

        sequence.video_tracks.swap(0, 2);
        let choices =
            visual_layer_choices(&sequence, std::slice::from_ref(&sequence), consumer_id).unwrap();
        let addresses = choices.into_iter().map(|choice| choice.address).collect::<Vec<_>>();
        assert_eq!(addresses, vec![VisualLayerAddress::Clip(later_id)]);
    }

    #[test]
    fn sequence_choices_reject_both_nested_and_auxiliary_paths_back_to_the_consumer() {
        let mut root = Sequence::new("Consumer");
        let consumer = solid();
        let consumer_id = consumer.id;
        root.video_tracks[0].add_clip(consumer).unwrap();
        let mut nested = Sequence::new("Ordinary path back");
        nested.video_tracks[0]
            .add_clip(
                Clip::new_nested_sequence(root.id, TimelineTime::ZERO, TimelineTime::ONE, None)
                    .unwrap(),
            )
            .unwrap();
        let mut auxiliary = Sequence::new("Auxiliary path back");
        let mut effect = EffectNode::new(EffectType::External("test.picker".into()));
        effect.properties.define(PropertyDescriptor::new(
            "source",
            "Picture source",
            PropertyValue::VisualSource(VisualSourceReference::Sequence {
                sequence_id: root.id,
                retime: Default::default(),
            }),
        ));
        let mut layer = solid();
        layer.add_effect_node(effect);
        auxiliary.video_tracks[0].add_clip(layer).unwrap();
        let mut independent = Sequence::new("Legal source");
        independent.video_tracks[0].add_clip(solid()).unwrap();
        let legal = independent.id;
        let choices = visual_layer_choices(
            &root,
            &[root.clone(), nested, auxiliary, independent],
            consumer_id,
        )
        .unwrap();
        let addresses = choices.into_iter().map(|choice| choice.address).collect::<Vec<_>>();
        assert_eq!(addresses, vec![VisualLayerAddress::Sequence(legal)]);
    }
}
