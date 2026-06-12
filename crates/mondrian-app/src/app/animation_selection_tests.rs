use super::*;
use crate::app::SelectedClipRef;
use mondrian_core::automation::{PropertyMutation, PropertyValue};
use mondrian_timeline::clip::Transform2D;

fn create_state_with_video_clips(count: usize) -> (AppState, TrackId, Vec<ClipId>, Rational) {
    let mut state = AppState::new();
    state.sequence = Some(Sequence::new("test"));
    let tb = state.sequence.as_ref().expect("sequence should exist").time_base();
    let track_id = state.sequence.as_ref().expect("sequence should exist").video_tracks[0].id;
    let mut clip_ids = Vec::new();
    for index in 0..count {
        let clip = Clip::new(
            AssetId::new(),
            TimeCode::new((index as i64) * 30, tb),
            TimeCode::new(20, tb),
        );
        clip_ids.push(clip.id);
        state.sequence.as_mut().expect("sequence should exist").video_tracks[0]
            .add_clip(clip)
            .expect("add clip");
    }
    (state, track_id, clip_ids, tb)
}

fn clip_ref(track_id: TrackId, clip_id: ClipId) -> SelectedClipRef {
    SelectedClipRef { track_id, is_video_track: true, clip_id }
}

#[test]
fn switching_active_animation_clip_clears_selection_and_bubble_host() {
    let (mut state, _track_id, clip_ids, _tb) = create_state_with_video_clips(2);
    let selected = AnimationKeyframeSelection {
        clip_id: clip_ids[0],
        path: Transform2D::OPACITY_PATH.to_string(),
        time: 0,
    };
    state.select_animation_keyframe_only(selected);
    state.set_animation_bubble_host(AnimationBubbleHost::Graph);

    state.set_active_animation_property(clip_ids[1], Transform2D::POSITION_PATH.to_string());

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
        clip_id: clip_ids[0],
        path: Transform2D::OPACITY_PATH.to_string(),
        time: 0,
    };
    state.select_animation_keyframe_only(selected.clone());
    state.set_animation_bubble_host(AnimationBubbleHost::Timeline);

    state.set_active_animation_property(clip_ids[0], Transform2D::POSITION_PATH.to_string());

    assert!(state.is_animation_keyframe_selected(&selected));
    assert_eq!(
        state.animation_bubble_host(),
        Some(AnimationBubbleHost::Timeline)
    );
}

#[test]
fn clearing_animation_selection_preserves_last_active_property_per_clip() {
    let (mut state, _track_id, clip_ids, _tb) = create_state_with_video_clips(2);

    state.set_active_animation_property(clip_ids[0], Transform2D::POSITION_PATH.to_string());
    state.set_active_animation_property(clip_ids[1], Transform2D::OPACITY_PATH.to_string());
    state.clear_animation_selection();

    assert_eq!(
        state.active_animation_property_path(clip_ids[0]),
        Some(Transform2D::POSITION_PATH)
    );
    assert_eq!(
        state.active_animation_property_path(clip_ids[1]),
        Some(Transform2D::OPACITY_PATH)
    );
    assert!(state.animation_selection.active_property.is_none());
}

#[test]
fn selected_animation_interpolation_mode_returns_none_for_mixed_modes() {
    let (mut state, track_id, clip_ids, tb) = create_state_with_video_clips(1);
    let selection = clip_ref(track_id, clip_ids[0]);
    let start = mondrian_core::automation::timecode_to_ticks(TimeCode::new(0, tb));
    let end = mondrian_core::automation::timecode_to_ticks(TimeCode::new(10, tb));

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
        AnimationKeyframeSelection {
            clip_id: clip_ids[0],
            path: Transform2D::OPACITY_PATH.to_string(),
            time: start,
        },
        AnimationKeyframeSelection {
            clip_id: clip_ids[0],
            path: Transform2D::OPACITY_PATH.to_string(),
            time: end,
        },
    ]);

    assert_eq!(state.selected_animation_interpolation_mode(selection), None);
}

#[test]
fn copy_paste_animation_keyframes_reassigns_ids_and_updates_selection() {
    let (mut state, track_id, clip_ids, tb) = create_state_with_video_clips(1);
    let selection = clip_ref(track_id, clip_ids[0]);
    let first = mondrian_core::automation::timecode_to_ticks(TimeCode::new(5, tb));
    let second = mondrian_core::automation::timecode_to_ticks(TimeCode::new(10, tb));
    let destination = mondrian_core::automation::timecode_to_ticks(TimeCode::new(20, tb));

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
        AnimationKeyframeSelection {
            clip_id: clip_ids[0],
            path: Transform2D::OPACITY_PATH.to_string(),
            time: first,
        },
        AnimationKeyframeSelection {
            clip_id: clip_ids[0],
            path: Transform2D::OPACITY_PATH.to_string(),
            time: second,
        },
    ]);
    assert!(state.copy_selected_animation_keyframes(selection).expect("copy"));
    assert!(state.paste_animation_keyframes(selection, destination).expect("paste"));

    let pasted_times = vec![destination, destination + (second - first)];
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
        .map(|selection| selection.time)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(selected_times, pasted_times.into_iter().collect());
}
