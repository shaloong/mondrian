use super::*;
use crate::app::SelectedClipRef;
use mondrian_core::automation::{PropertyMutation, PropertyValue};
use mondrian_timeline::clip::Transform2D;

fn create_state_with_video_clips(count: usize) -> (AppState, TrackId, Vec<ClipId>, Rational) {
    let mut state = AppState::new();
    state.test_set_sequence(Some(Sequence::new("test")));
    let tb = state.active_sequence().expect("sequence should exist").time_base();
    let track_id = state.active_sequence().expect("sequence should exist").video_tracks[0].id;
    let mut clip_ids = Vec::new();
    for index in 0..count {
        let clip =
            Clip::new(AssetId::new(), tt((index as i64) * 30, tb), tt(20, tb)).expect("valid clip");
        clip_ids.push(clip.id);
        state
            .active_sequence_mut_uncommitted()
            .expect("sequence should exist")
            .video_tracks[0]
            .add_clip(clip)
            .expect("add clip");
    }
    (state, track_id, clip_ids, tb)
}

fn clip_ref(track_id: TrackId, clip_id: ClipId) -> SelectedClipRef {
    SelectedClipRef { track_id, is_video_track: true, clip_id }
}

fn property_address(state: &AppState, clip_id: ClipId, path: &str) -> AnimationParameterAddress {
    state
        .active_sequence()
        .and_then(|sequence| sequence.find_clip(clip_id))
        .and_then(|clip| clip.property_bag().ok())
        .and_then(|bag| bag.address_for_path(path))
        .expect("property address")
}

fn key_selection(
    state: &AppState,
    clip_id: ClipId,
    path: &str,
    time: TimelineTime,
) -> AnimationKeyframeSelection {
    let property = property_address(state, clip_id, path);
    let keyframe_id = state
        .active_sequence()
        .and_then(|sequence| sequence.find_clip(clip_id))
        .and_then(|clip| clip.property_bag().ok())
        .and_then(|bag| {
            bag.property_by_address(&property)
                .and_then(|(_, property)| property.keyframe_at(time))
        })
        .map(|keyframe| keyframe.id)
        .expect("keyframe");
    AnimationKeyframeSelection {
        property: AnimationPropertySelection { clip_id, property },
        keyframe_id,
    }
}

#[test]
fn switching_active_animation_clip_clears_selection_and_bubble_host() {
    let (mut state, _track_id, clip_ids, _tb) = create_state_with_video_clips(2);
    let selected = AnimationKeyframeSelection {
        property: AnimationPropertySelection {
            clip_id: clip_ids[0],
            property: property_address(&state, clip_ids[0], Transform2D::OPACITY_PATH),
        },
        keyframe_id: KeyframeId::new(),
    };
    state.select_animation_keyframe_only(selected);
    state.set_animation_bubble_host(AnimationBubbleHost::Graph);

    let position = property_address(&state, clip_ids[1], Transform2D::POSITION_PATH);
    state.set_active_animation_property(clip_ids[1], position);

    assert!(state.selected_animation_keyframes_for_clip(clip_ids[0]).is_empty());
    assert_eq!(state.animation_bubble_host(), None);
    assert_eq!(
        state
            .animation_selection
            .active_property
            .as_ref()
            .map(|selection| selection.clip_id),
        Some(clip_ids[1])
    );
}

#[test]
fn switching_active_animation_property_within_clip_preserves_selection_and_bubble_host() {
    let (mut state, _track_id, clip_ids, _tb) = create_state_with_video_clips(1);
    let selected = AnimationKeyframeSelection {
        property: AnimationPropertySelection {
            clip_id: clip_ids[0],
            property: property_address(&state, clip_ids[0], Transform2D::OPACITY_PATH),
        },
        keyframe_id: KeyframeId::new(),
    };
    state.select_animation_keyframe_only(selected.clone());
    state.set_animation_bubble_host(AnimationBubbleHost::Timeline);

    let position = property_address(&state, clip_ids[0], Transform2D::POSITION_PATH);
    state.set_active_animation_property(clip_ids[0], position);

    assert!(state.is_animation_keyframe_selected(&selected));
    assert_eq!(
        state.animation_bubble_host(),
        Some(AnimationBubbleHost::Timeline)
    );
}

#[test]
fn clearing_animation_selection_preserves_last_active_property_per_clip() {
    let (mut state, _track_id, clip_ids, _tb) = create_state_with_video_clips(2);

    let position = property_address(&state, clip_ids[0], Transform2D::POSITION_PATH);
    let opacity = property_address(&state, clip_ids[1], Transform2D::OPACITY_PATH);
    state.set_active_animation_property(clip_ids[0], position);
    state.set_active_animation_property(clip_ids[1], opacity);
    state.clear_animation_selection();

    assert_eq!(
        state.active_animation_property_path(clip_ids[0]),
        Some(Transform2D::POSITION_PATH.to_owned())
    );
    assert_eq!(
        state.active_animation_property_path(clip_ids[1]),
        Some(Transform2D::OPACITY_PATH.to_owned())
    );
    assert!(state.animation_selection.active_property.is_none());
}

#[test]
fn selected_animation_interpolation_mode_returns_none_for_mixed_modes() {
    let (mut state, track_id, clip_ids, tb) = create_state_with_video_clips(1);
    let selection = clip_ref(track_id, clip_ids[0]);
    let start = tt(0, tb);
    let end = tt(10, tb);

    state
        .mutate_clip_property(
            selection,
            PropertyMutation::SetKeyframe {
                path: Transform2D::OPACITY_PATH.to_string(),
                keyframe: Keyframe::linear(start, PropertyValue::Float(0.0)),
            },
            "set start keyframe",
        )
        .expect("set start");
    state
        .mutate_clip_property(
            selection,
            PropertyMutation::SetKeyframe {
                path: Transform2D::OPACITY_PATH.to_string(),
                keyframe: Keyframe::from_preset(
                    end,
                    PropertyValue::Float(1.0),
                    InterpolationType::Hold,
                ),
            },
            "set end keyframe",
        )
        .expect("set end");
    state.set_animation_keyframe_selection(vec![
        key_selection(&state, clip_ids[0], Transform2D::OPACITY_PATH, start),
        key_selection(&state, clip_ids[0], Transform2D::OPACITY_PATH, end),
    ]);

    assert_eq!(state.selected_animation_interpolation_mode(selection), None);
}

#[test]
fn copy_paste_animation_keyframes_reassigns_ids_and_updates_selection() {
    let (mut state, track_id, clip_ids, tb) = create_state_with_video_clips(1);
    let selection = clip_ref(track_id, clip_ids[0]);
    let first = tt(5, tb);
    let second = tt(10, tb);
    let destination = tt(20, tb);

    for (time, value) in [(first, 0.2), (second, 0.8)] {
        state
            .mutate_clip_property(
                selection,
                PropertyMutation::SetKeyframe {
                    path: Transform2D::OPACITY_PATH.to_string(),
                    keyframe: Keyframe::linear(time, PropertyValue::Float(value)),
                },
                "seed keyframe",
            )
            .expect("seed keyframe");
    }

    let before_ids = state
        .clip_snapshot(selection)
        .and_then(|clip| clip.property_bag().ok())
        .and_then(|bag| bag.property(Transform2D::OPACITY_PATH).cloned())
        .map(|property| {
            [first, second]
                .into_iter()
                .map(|time| property.keyframe_at(time).expect("original keyframe").id)
                .collect::<Vec<_>>()
        })
        .expect("before ids");

    state.set_animation_keyframe_selection(vec![
        key_selection(&state, clip_ids[0], Transform2D::OPACITY_PATH, first),
        key_selection(&state, clip_ids[0], Transform2D::OPACITY_PATH, second),
    ]);
    assert!(state.copy_selected_animation_keyframes(selection).expect("copy"));
    assert!(state.paste_animation_keyframes(selection, destination).expect("paste"));

    let offset = second.checked_sub(first).expect("valid keyframe offset");
    let pasted_times = vec![
        destination,
        destination.checked_add(offset).expect("valid pasted time"),
    ];
    let property = state
        .clip_snapshot(selection)
        .and_then(|clip| clip.property_bag().ok())
        .and_then(|bag| bag.property(Transform2D::OPACITY_PATH).cloned())
        .expect("property");
    let pasted_ids = pasted_times
        .iter()
        .map(|time| property.keyframe_at(*time).expect("pasted keyframe").id)
        .collect::<Vec<_>>();
    assert!(pasted_ids.iter().all(|id| !before_ids.contains(id)));

    let selected_times = state
        .selected_animation_keyframes_for_clip(clip_ids[0])
        .into_iter()
        .map(|selection| {
            property
                .keyframe_by_id(selection.keyframe_id)
                .expect("selected pasted key")
                .time
        })
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(selected_times, pasted_times.into_iter().collect());
}
