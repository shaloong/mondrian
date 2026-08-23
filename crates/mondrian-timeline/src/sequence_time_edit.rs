//! Shared Sequence-time automation transforms for structural editorial edits.
//!
//! Insert and Extract own different Clip/Transition topology, but they must not
//! develop separate interpretations of which Track, Bus, Route, or Program
//! automation follows editorial time. This private Module centralizes that
//! owner-closure traversal while leaving edit policy with each public command.

use crate::{
    audio::{
        AudioChannelStrip, AudioProcessorRack, AudioProgram, AudioRouteDestination,
        AudioRouteSource, ProgramOutputMainSource,
    },
    sequence::Sequence,
};
use mondrian_core::{ExactAutomationCurve, TimelineTime, TimelineTimeRange, TrackId};
use std::collections::BTreeSet;

/// Exact Sequence-time topology transform applied to following automation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SequenceTimeEdit {
    /// Open a positive interval at one boundary.
    Insert {
        at: TimelineTime,
        duration: TimelineTime,
    },
    /// Remove one half-open interval and close its gap.
    Extract { range: TimelineTimeRange },
}

/// Apply one structural time transform to automation whose complete input
/// closure follows the supplied Track set.
pub(crate) fn edit_sequence_automation(
    sequence: &mut Sequence,
    edited_tracks: &BTreeSet<TrackId>,
    edit: SequenceTimeEdit,
) -> mondrian_core::Result<()> {
    for track in &mut sequence.video_tracks {
        if edited_tracks.contains(&track.id) {
            edit_animated_property(&mut track.opacity, edit)?;
        }
    }

    let edited_audio_tracks = sequence
        .audio_tracks
        .iter()
        .filter(|track| edited_tracks.contains(&track.id))
        .map(|track| track.id)
        .collect::<BTreeSet<_>>();
    edit_audio_program(
        &mut sequence.audio_program,
        &edited_audio_tracks,
        sequence.audio_tracks.len(),
        edit,
    )
}

fn edit_audio_program(
    program: &mut AudioProgram,
    edited_tracks: &BTreeSet<TrackId>,
    audio_track_count: usize,
    edit: SequenceTimeEdit,
) -> mondrian_core::Result<()> {
    for track_id in edited_tracks {
        if let Some(channel) = program.track_channels.get_mut(track_id) {
            edit_channel_strip(&mut channel.strip, edit)?;
        }
    }

    let mut edited_buses = BTreeSet::new();
    loop {
        let mut changed = false;
        for bus in &program.buses {
            if edited_buses.contains(&bus.id) {
                continue;
            }
            let sources = program
                .routes
                .iter()
                .filter(|route| route.destination == AudioRouteDestination::Bus(bus.id))
                .map(|route| route.source)
                .collect::<Vec<_>>();
            if !sources.is_empty()
                && sources
                    .iter()
                    .all(|source| source_follows(*source, edited_tracks, &edited_buses))
            {
                edited_buses.insert(bus.id);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    for bus in &mut program.buses {
        if edited_buses.contains(&bus.id) {
            edit_channel_strip(&mut bus.strip, edit)?;
        }
    }
    for route in &mut program.routes {
        if source_follows(route.source, edited_tracks, &edited_buses) {
            edit_optional_curve(&mut route.gain_automation, edit)?;
        }
    }

    for output in &mut program.outputs {
        let follows = match output.main_source {
            ProgramOutputMainSource::RoutedInputs => {
                let sources = program
                    .routes
                    .iter()
                    .filter(|route| route.destination == AudioRouteDestination::Output(output.id))
                    .map(|route| route.source)
                    .collect::<Vec<_>>();
                !sources.is_empty()
                    && sources
                        .iter()
                        .all(|source| source_follows(*source, edited_tracks, &edited_buses))
            }
            ProgramOutputMainSource::SemanticProjection { .. } => {
                audio_track_count > 0 && edited_tracks.len() == audio_track_count
            }
        };
        if follows {
            edit_channel_strip(&mut output.strip, edit)?;
        }
    }
    Ok(())
}

fn source_follows(
    source: AudioRouteSource,
    edited_tracks: &BTreeSet<TrackId>,
    edited_buses: &BTreeSet<mondrian_core::MixBusId>,
) -> bool {
    match source {
        AudioRouteSource::Track { track_id, .. } => edited_tracks.contains(&track_id),
        AudioRouteSource::Bus { bus_id, .. } => edited_buses.contains(&bus_id),
    }
}

fn edit_channel_strip(
    strip: &mut AudioChannelStrip,
    edit: SequenceTimeEdit,
) -> mondrian_core::Result<()> {
    edit_optional_curve(&mut strip.fader_automation, edit)?;
    edit_rack(&mut strip.pre_fader, edit)?;
    edit_rack(&mut strip.post_fader, edit)
}

fn edit_rack(rack: &mut AudioProcessorRack, edit: SequenceTimeEdit) -> mondrian_core::Result<()> {
    for processor in &mut rack.processors {
        for parameter in processor.parameters.values_mut() {
            edit_exact_curve(&mut parameter.automation, edit)?;
        }
    }
    Ok(())
}

fn edit_optional_curve(
    curve: &mut Option<ExactAutomationCurve>,
    edit: SequenceTimeEdit,
) -> mondrian_core::Result<()> {
    if let Some(curve) = curve {
        edit_exact_curve(curve, edit)?;
    }
    Ok(())
}

fn edit_exact_curve(
    curve: &mut ExactAutomationCurve,
    edit: SequenceTimeEdit,
) -> mondrian_core::Result<()> {
    match edit {
        SequenceTimeEdit::Insert { at, duration } => {
            curve.shift_keyframes_at_or_after(at, duration).map_err(automation_error)?;
        }
        SequenceTimeEdit::Extract { range } => {
            curve.extract_time_range(range).map_err(automation_error)?;
        }
    }
    Ok(())
}

fn edit_animated_property(
    property: &mut mondrian_core::AnimatedProperty,
    edit: SequenceTimeEdit,
) -> mondrian_core::Result<()> {
    match edit {
        SequenceTimeEdit::Insert { at, duration } => {
            property.shift_keyframes_at_or_after(at, duration)?;
        }
        SequenceTimeEdit::Extract { range } => property.extract_time_range(range)?,
    }
    Ok(())
}

fn automation_error(error: impl std::fmt::Display) -> mondrian_core::MondrianError {
    mondrian_core::MondrianError::WorkflowStepFailed {
        step_id: "sequence_time_automation_edit".to_owned(),
        reason: error.to_string(),
    }
}
