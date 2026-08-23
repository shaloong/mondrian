//! Exact Track-owned audio authoring evidence for composed Golden stages.

use crate::app::AppState;
use anyhow::{ensure, Context};
use mondrian_core::{AudioRoleId, SequenceId, TrackId};
use mondrian_timeline::{
    audio::{
        AudioProcessingScope, AudioRole, AudioRoute, AudioRouteSource, AudioTrackMixerChannel,
        AudioTransition,
    },
    Track, TrackType,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

/// Exact author projection owned or directly referenced by a stable audio Track set.
///
/// Shared Program Outputs and Buses are deliberately excluded: adding an
/// unrelated Track may extend their inputs without changing the captured Track
/// authoring. Direct Track Routes, referenced Processing Scopes, Roles, and
/// Transitions remain part of the projection.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(super) struct GoldenAudioTrackAuthoringAnchor {
    sequence_id: SequenceId,
    track_ids: Vec<TrackId>,
    projection: Value,
}

#[derive(Serialize)]
struct AudioTrackAuthoringProjection<'a> {
    tracks: Vec<&'a Track>,
    track_channels: Vec<(TrackId, &'a AudioTrackMixerChannel)>,
    processing_scopes: Vec<&'a AudioProcessingScope>,
    roles: Vec<&'a AudioRole>,
    routes: Vec<&'a AudioRoute>,
    transitions: Vec<&'a AudioTransition>,
}

/// Capture one exact, order-stable Track-owned audio author projection.
pub(super) fn capture_audio_track_authoring(
    state: &AppState,
    sequence_id: SequenceId,
    track_ids: &[TrackId],
) -> anyhow::Result<GoldenAudioTrackAuthoringAnchor> {
    ensure!(
        !track_ids.is_empty(),
        "audio authoring evidence requires at least one Track"
    );
    let unique_track_ids = track_ids.iter().copied().collect::<BTreeSet<_>>();
    ensure!(
        unique_track_ids.len() == track_ids.len(),
        "audio authoring evidence contains duplicate Track identities"
    );

    let sequence = state
        .sequence_by_id(sequence_id)
        .with_context(|| format!("audio evidence Sequence is absent: {sequence_id}"))?;
    let tracks = track_ids
        .iter()
        .map(|track_id| {
            let track = sequence
                .audio_tracks
                .iter()
                .find(|track| track.id == *track_id)
                .with_context(|| format!("audio evidence Track is absent: {track_id}"))?;
            ensure!(
                track.track_type == TrackType::Audio,
                "audio evidence Track has non-audio type: {track_id}"
            );
            Ok(track)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let edit_ids = tracks
        .iter()
        .flat_map(|track| &track.clips)
        .flat_map(|clip| &clip.audio_components)
        .map(|edit| edit.id)
        .collect::<BTreeSet<_>>();
    let scope_ids = tracks
        .iter()
        .flat_map(|track| &track.clips)
        .flat_map(|clip| &clip.audio_components)
        .map(|edit| edit.processing.scope_id)
        .collect::<BTreeSet<_>>();
    let mut role_ids = tracks
        .iter()
        .flat_map(|track| &track.clips)
        .flat_map(|clip| &clip.audio_components)
        .filter_map(|edit| edit.role_id)
        .collect::<BTreeSet<_>>();
    close_role_ancestors(&sequence.audio_roles, &mut role_ids)?;

    let track_channels = track_ids
        .iter()
        .map(|track_id| {
            sequence
                .audio_program
                .track_channels
                .get(track_id)
                .map(|channel| (*track_id, channel))
                .with_context(|| format!("audio evidence Track channel is absent: {track_id}"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let processing_scopes = sequence
        .audio_program
        .processing_scopes
        .iter()
        .filter(|scope| scope_ids.contains(&scope.id))
        .collect::<Vec<_>>();
    ensure!(
        processing_scopes.len() == scope_ids.len(),
        "audio evidence cannot resolve every referenced Processing Scope"
    );
    let roles = sequence
        .audio_roles
        .iter()
        .filter(|role| role_ids.contains(&role.id))
        .collect::<Vec<_>>();
    ensure!(
        roles.len() == role_ids.len(),
        "audio evidence cannot resolve every referenced Role"
    );
    let routes = sequence
        .audio_program
        .routes
        .iter()
        .filter(|route| {
            matches!(
                route.source,
                AudioRouteSource::Track { track_id, .. } if unique_track_ids.contains(&track_id)
            )
        })
        .collect::<Vec<_>>();
    let transitions = sequence
        .audio_program
        .transitions
        .iter()
        .filter(|transition| {
            edit_ids.contains(&transition.left) || edit_ids.contains(&transition.right)
        })
        .collect::<Vec<_>>();
    let projection = serde_json::to_value(AudioTrackAuthoringProjection {
        tracks,
        track_channels,
        processing_scopes,
        roles,
        routes,
        transitions,
    })?;

    Ok(GoldenAudioTrackAuthoringAnchor {
        sequence_id,
        track_ids: track_ids.to_vec(),
        projection,
    })
}

fn close_role_ancestors(
    roles: &[AudioRole],
    role_ids: &mut BTreeSet<AudioRoleId>,
) -> anyhow::Result<()> {
    let mut pending = role_ids.iter().copied().collect::<Vec<_>>();
    while let Some(role_id) = pending.pop() {
        let role = roles
            .iter()
            .find(|role| role.id == role_id)
            .with_context(|| format!("audio evidence Role is absent: {role_id}"))?;
        if let Some(parent_id) = role.parent_id
            && role_ids.insert(parent_id)
        {
            pending.push(parent_id);
        }
    }
    Ok(())
}
