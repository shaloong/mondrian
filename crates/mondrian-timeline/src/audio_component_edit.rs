//! Stable-address authoring for placement-local audio Component controls.
//!
//! This Module owns Component address resolution, Track-lock admission,
//! atomic mutation, and complete Audio Program validation. Product and media
//! Adapters may supply exact values, but they never walk or mutate Timeline
//! storage directly.

use crate::audio::{
    AudioAuthoringError, AudioComponentChannelMapping, AudioComponentEdit, AudioComponentSource,
    AudioFade,
};
use crate::clip::Clip;
use crate::sequence::Sequence;
use mondrian_core::{AudioComponentEditId, ClipId, TrackId};
use serde::{Deserialize, Serialize};

/// Stable address of one placement-local audio Component Edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioComponentAddress {
    /// Owning audio Track.
    pub track_id: TrackId,
    /// Owning Clip occurrence.
    pub clip_id: ClipId,
    /// Stable placement-local Component Edit identity.
    pub edit_id: AudioComponentEditId,
}

/// One independently meaningful Component mutation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum AudioComponentMutation {
    /// Select one canonical media Component or nested public Output.
    ///
    /// The Timeline Module validates Clip/source-kind coherence and duplicate
    /// media selections. The App Adapter must additionally prove the requested
    /// recoverable media catalog or Project-contained nested Output exists.
    SetSource {
        /// Stable logical source identity in the owning Clip's source domain.
        value: AudioComponentSource,
    },
    /// Include or exclude this Component from the compiled Audio Program.
    SetEnabled {
        /// New contribution state.
        value: bool,
    },
    /// Replace the unautomated post-processing placement gain.
    SetVolumeDb {
        /// Decibel value validated by the complete authoring contract.
        value: f64,
    },
    /// Replace the unautomated stereo pan/balance value.
    SetPan {
        /// Normalized value in the inclusive interval `[-1, 1]`.
        value: f64,
    },
    /// Replace the unary fade beginning at the Clip in edge.
    SetFadeIn {
        /// `None` removes the fade.
        value: Option<AudioFade>,
    },
    /// Replace the unary fade ending at the Clip out edge.
    SetFadeOut {
        /// `None` removes the fade.
        value: Option<AudioFade>,
    },
    /// Replace the complete channel-mapping policy or exact sparse matrix.
    ///
    /// Matrix replacement is deliberately atomic: a partially edited matrix
    /// never enters an immutable author snapshot.
    SetChannelMapping {
        /// Standard fail-closed policy or one exact authored matrix.
        value: AudioComponentChannelMapping,
    },
    /// Change one coefficient in the currently explicit matrix.
    ///
    /// Expected layouts make retained or scripted stale actions fail closed.
    /// The Implementation rebuilds and validates one complete canonical matrix
    /// before committing the candidate.
    SetExplicitChannelMixGain {
        /// Source layout observed when this edit intent was created.
        expected_source_layout: mondrian_core::AudioChannelLayout,
        /// Destination layout observed when this edit intent was created.
        expected_destination_layout: mondrian_core::AudioChannelLayout,
        /// Zero-based source channel.
        source_channel: u8,
        /// Zero-based destination channel.
        destination_channel: u8,
        /// Linear-amplitude coefficient; zero removes the sparse edge.
        gain: f64,
    },
}

/// Complete Component mutation intent for one author transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioComponentEditRequest {
    /// Stable Component address.
    pub address: AudioComponentAddress,
    /// Exact field mutation.
    pub mutation: AudioComponentMutation,
}

/// Receipt from one successful Component mutation attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioComponentEditOutcome {
    /// Whether canonical author state changed.
    pub changed: bool,
}

/// Fail-closed Component address error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioComponentAddressError {
    /// Track is absent from the Sequence audio Timeline.
    #[error("Audio Component references an unknown audio Track: {0}")]
    UnknownTrack(TrackId),
    /// Clip is absent from the addressed Track.
    #[error("Audio Component references unknown Clip {clip_id} on Track {track_id}")]
    UnknownClip {
        /// Addressed audio Track.
        track_id: TrackId,
        /// Missing Clip occurrence.
        clip_id: ClipId,
    },
    /// Component Edit is absent from the addressed Clip.
    #[error("Audio Component references unknown Component Edit {0}")]
    UnknownComponentEdit(AudioComponentEditId),
}

/// Temporary author-state condition blocking an otherwise valid edit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioComponentEditBlocker {
    /// A persistent Track lock protects every placement-local Component field.
    #[error("Audio Component cannot modify locked Track: {0}")]
    LockedTrack(TrackId),
}

/// Fail-closed Component address, admission, or author-state failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioComponentEditError {
    /// Stable address could not be resolved in this Sequence snapshot.
    #[error(transparent)]
    Address(#[from] AudioComponentAddressError),
    /// Valid Component is currently protected by authoring policy.
    #[error(transparent)]
    Blocked(#[from] AudioComponentEditBlocker),
    /// Static volume is not signal authority while its automation is active.
    #[error("Audio Component volume automation is active; edit or clear the curve explicitly")]
    VolumeAutomationActive,
    /// Static pan is not signal authority while its automation is active.
    #[error("Audio Component pan automation is active; edit or clear the curve explicitly")]
    PanAutomationActive,
    /// Coefficient editing requires the exact explicit matrix the caller inspected.
    #[error("Audio Component explicit channel matrix changed before coefficient edit")]
    ExplicitChannelMatrixChanged,
    /// New coefficient or reconstructed sparse matrix is invalid.
    #[error("Audio Component channel coefficient is invalid: {0}")]
    ChannelMatrix(mondrian_core::AudioChannelMixMatrixError),
    /// Complete resulting audio author state is invalid.
    #[error("Audio Component edit produced invalid author state: {0}")]
    AuthorState(AudioAuthoringError),
}

/// Read-only projection of one resolved Component and edit admission.
#[derive(Debug, Clone)]
pub struct AudioComponentInspection<'a> {
    owning_clip: &'a Clip,
    component: &'a AudioComponentEdit,
    destination_layout: mondrian_core::AudioChannelLayout,
    edit_blocker: Option<AudioComponentEditBlocker>,
}

impl<'a> AudioComponentInspection<'a> {
    /// Canonical Clip placement that owns this Component Edit.
    pub fn owning_clip(&self) -> &'a Clip {
        self.owning_clip
    }

    /// Resolved canonical placement-local Component Edit.
    pub fn component(&self) -> &'a AudioComponentEdit {
        self.component
    }

    /// Sequence-owned signal layout every explicit matrix must target.
    pub const fn destination_layout(&self) -> mondrian_core::AudioChannelLayout {
        self.destination_layout
    }

    /// Authoritative reason this Component cannot currently be edited.
    pub fn edit_blocker(&self) -> Option<&AudioComponentEditBlocker> {
        self.edit_blocker.as_ref()
    }

    /// Whether a mutation may enter an author transaction.
    pub fn is_editable(&self) -> bool {
        self.edit_blocker.is_none()
    }
}

/// Resolve one Component and inspect Track-lock admission from one snapshot.
pub fn inspect_audio_component(
    sequence: &Sequence,
    address: AudioComponentAddress,
) -> Result<AudioComponentInspection<'_>, AudioComponentAddressError> {
    let track = sequence
        .audio_tracks
        .iter()
        .find(|track| track.id == address.track_id)
        .ok_or(AudioComponentAddressError::UnknownTrack(address.track_id))?;
    let clip = track.clips.iter().find(|clip| clip.id == address.clip_id).ok_or(
        AudioComponentAddressError::UnknownClip {
            track_id: address.track_id,
            clip_id: address.clip_id,
        },
    )?;
    let component = clip
        .audio_components
        .iter()
        .find(|component| component.id == address.edit_id)
        .ok_or(AudioComponentAddressError::UnknownComponentEdit(
            address.edit_id,
        ))?;
    Ok(AudioComponentInspection {
        owning_clip: clip,
        component,
        destination_layout: sequence.settings.audio_channel_layout,
        edit_blocker: track.is_locked.then_some(AudioComponentEditBlocker::LockedTrack(track.id)),
    })
}

/// Apply one placement-local Component edit atomically to a Sequence candidate.
///
/// The input Sequence is replaced only after complete Audio Program validation;
/// canonical no-ops produce no author revision.
pub fn apply_audio_component_edit(
    sequence: &mut Sequence,
    request: &AudioComponentEditRequest,
) -> Result<AudioComponentEditOutcome, AudioComponentEditError> {
    let inspection = inspect_audio_component(sequence, request.address)?;
    if let Some(blocker) = inspection.edit_blocker {
        return Err(blocker.into());
    }

    let mut candidate = sequence.clone();
    let component = resolve_component_mut(&mut candidate, request.address)?;
    let changed = apply_mutation(component, &request.mutation)?;
    if !changed {
        return Ok(AudioComponentEditOutcome { changed: false });
    }
    candidate
        .audio_program
        .validate(
            &candidate.audio_tracks,
            &candidate.audio_roles,
            candidate.settings.audio_channel_layout,
        )
        .map_err(AudioComponentEditError::AuthorState)?;
    *sequence = candidate;
    Ok(AudioComponentEditOutcome { changed: true })
}

fn resolve_component_mut(
    sequence: &mut Sequence,
    address: AudioComponentAddress,
) -> Result<&mut AudioComponentEdit, AudioComponentAddressError> {
    let track = sequence
        .audio_tracks
        .iter_mut()
        .find(|track| track.id == address.track_id)
        .ok_or(AudioComponentAddressError::UnknownTrack(address.track_id))?;
    let clip = track.clips.iter_mut().find(|clip| clip.id == address.clip_id).ok_or(
        AudioComponentAddressError::UnknownClip {
            track_id: address.track_id,
            clip_id: address.clip_id,
        },
    )?;
    clip.audio_components
        .iter_mut()
        .find(|component| component.id == address.edit_id)
        .ok_or(AudioComponentAddressError::UnknownComponentEdit(
            address.edit_id,
        ))
}

fn apply_mutation(
    component: &mut AudioComponentEdit,
    mutation: &AudioComponentMutation,
) -> Result<bool, AudioComponentEditError> {
    match mutation {
        AudioComponentMutation::SetSource { value } => {
            Ok(replace(&mut component.source, value.clone()))
        }
        AudioComponentMutation::SetEnabled { value } => Ok(replace(&mut component.enabled, *value)),
        AudioComponentMutation::SetVolumeDb { value } => {
            if component.volume_automation.is_some() {
                return Err(AudioComponentEditError::VolumeAutomationActive);
            }
            Ok(replace(&mut component.volume_db, *value))
        }
        AudioComponentMutation::SetPan { value } => {
            if component.pan_automation.is_some() {
                return Err(AudioComponentEditError::PanAutomationActive);
            }
            Ok(replace(&mut component.pan, *value))
        }
        AudioComponentMutation::SetFadeIn { value } => {
            Ok(replace(&mut component.fades.fade_in, *value))
        }
        AudioComponentMutation::SetFadeOut { value } => {
            Ok(replace(&mut component.fades.fade_out, *value))
        }
        AudioComponentMutation::SetChannelMapping { value } => {
            Ok(replace(&mut component.channel_mapping, value.clone()))
        }
        AudioComponentMutation::SetExplicitChannelMixGain {
            expected_source_layout,
            expected_destination_layout,
            source_channel,
            destination_channel,
            gain,
        } => {
            let AudioComponentChannelMapping::Explicit(matrix) = &component.channel_mapping else {
                return Err(AudioComponentEditError::ExplicitChannelMatrixChanged);
            };
            if matrix.source_layout() != *expected_source_layout
                || matrix.destination_layout() != *expected_destination_layout
            {
                return Err(AudioComponentEditError::ExplicitChannelMatrixChanged);
            }
            let mut entries = matrix
                .entries()
                .iter()
                .copied()
                .filter(|entry| {
                    entry.source_channel() != *source_channel
                        || entry.destination_channel() != *destination_channel
                })
                .collect::<Vec<_>>();
            if *gain != 0.0 {
                entries.push(
                    mondrian_core::AudioChannelMixEntry::new(
                        *source_channel,
                        *destination_channel,
                        *gain,
                    )
                    .map_err(AudioComponentEditError::ChannelMatrix)?,
                );
            }
            let updated = mondrian_core::AudioChannelMixMatrix::new(
                *expected_source_layout,
                *expected_destination_layout,
                entries,
            )
            .map_err(AudioComponentEditError::ChannelMatrix)?;
            Ok(replace(
                &mut component.channel_mapping,
                AudioComponentChannelMapping::Explicit(updated),
            ))
        }
    }
}

fn replace<T: PartialEq>(target: &mut T, value: T) -> bool {
    if *target == value {
        return false;
    }
    *target = value;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{AudioComponentChannelMapping, AudioFadeCurve};
    use crate::clip::Clip;
    use mondrian_core::{
        AssetId, AudioChannelLayout, AudioChannelMixMatrix, AudioSourceComponentId, TimelineTime,
    };

    fn sequence_with_component() -> (Sequence, AudioComponentAddress) {
        let mut sequence = Sequence::new("Component authoring");
        let track_id = sequence.audio_tracks[0].id;
        let clip = Clip::new(
            AssetId::new(),
            TimelineTime::ZERO,
            TimelineTime::new(10, 1).expect("duration"),
        )
        .expect("Clip");
        let clip_id = sequence
            .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
            .expect("audio Clip");
        let edit_id = sequence.audio_tracks[0].clips[0].audio_components[0].id;
        (
            sequence,
            AudioComponentAddress { track_id, clip_id, edit_id },
        )
    }

    fn request(
        address: AudioComponentAddress,
        mutation: AudioComponentMutation,
    ) -> AudioComponentEditRequest {
        AudioComponentEditRequest { address, mutation }
    }

    #[test]
    fn one_interface_edits_static_fields_and_exact_matrix_atomically() {
        let (mut sequence, address) = sequence_with_component();
        let alternate_source = AudioSourceComponentId::new();
        let fade = AudioFade {
            duration: TimelineTime::new(1, 2).expect("fade"),
            curve: AudioFadeCurve::EqualPower,
        };
        let matrix = AudioChannelMixMatrix::standard(
            AudioChannelLayout::Mono,
            sequence.settings.audio_channel_layout,
        )
        .expect("standard matrix");
        for mutation in [
            AudioComponentMutation::SetSource {
                value: AudioComponentSource::Media { component_id: alternate_source },
            },
            AudioComponentMutation::SetEnabled { value: false },
            AudioComponentMutation::SetVolumeDb { value: -6.0 },
            AudioComponentMutation::SetPan { value: 0.25 },
            AudioComponentMutation::SetFadeIn { value: Some(fade) },
            AudioComponentMutation::SetChannelMapping {
                value: AudioComponentChannelMapping::Explicit(matrix.clone()),
            },
        ] {
            assert!(
                apply_audio_component_edit(&mut sequence, &request(address, mutation))
                    .expect("valid mutation")
                    .changed
            );
        }
        let inspection = inspect_audio_component(&sequence, address).expect("Component");
        assert_eq!(
            inspection.component().source,
            AudioComponentSource::Media { component_id: alternate_source }
        );
        assert_eq!(inspection.owning_clip().id, address.clip_id);
        assert!(!inspection.component().enabled);
        assert_eq!(inspection.component().volume_db, -6.0);
        assert_eq!(inspection.component().pan, 0.25);
        assert_eq!(inspection.component().fades.fade_in, Some(fade));
        assert_eq!(
            inspection.component().channel_mapping,
            AudioComponentChannelMapping::Explicit(matrix)
        );
        assert_eq!(
            inspection.destination_layout(),
            sequence.settings.audio_channel_layout
        );
    }

    #[test]
    fn noops_locks_and_invalid_candidates_preserve_the_snapshot() {
        let (mut sequence, address) = sequence_with_component();
        assert!(
            !apply_audio_component_edit(
                &mut sequence,
                &request(address, AudioComponentMutation::SetVolumeDb { value: 0.0 }),
            )
            .expect("no-op")
            .changed
        );
        assert!(
            !apply_audio_component_edit(
                &mut sequence,
                &request(
                    address,
                    AudioComponentMutation::SetSource {
                        value: AudioComponentSource::Media {
                            component_id: AudioSourceComponentId::primary(),
                        },
                    },
                ),
            )
            .expect("source no-op")
            .changed
        );

        let before_wrong_source = sequence.clone();
        assert!(matches!(
            apply_audio_component_edit(
                &mut sequence,
                &request(
                    address,
                    AudioComponentMutation::SetSource {
                        value: AudioComponentSource::NestedOutput {
                            output_id: mondrian_core::ProgramOutputId::new(),
                        },
                    },
                ),
            ),
            Err(AudioComponentEditError::AuthorState(
                AudioAuthoringError::InvalidComponentSource(id)
            )) if id == address.edit_id
        ));
        assert_eq!(sequence, before_wrong_source);

        let before_invalid = sequence.clone();
        let wrong_destination = AudioChannelMixMatrix::identity(AudioChannelLayout::Mono);
        assert!(matches!(
            apply_audio_component_edit(
                &mut sequence,
                &request(
                    address,
                    AudioComponentMutation::SetChannelMapping {
                        value: AudioComponentChannelMapping::Explicit(wrong_destination),
                    },
                ),
            ),
            Err(AudioComponentEditError::AuthorState(
                AudioAuthoringError::ChannelMappingDestinationMismatch(id)
            )) if id == address.edit_id
        ));
        assert_eq!(sequence, before_invalid);

        sequence.audio_tracks[0].is_locked = true;
        let before_locked = sequence.clone();
        assert_eq!(
            apply_audio_component_edit(
                &mut sequence,
                &request(address, AudioComponentMutation::SetEnabled { value: false }),
            ),
            Err(AudioComponentEditError::Blocked(
                AudioComponentEditBlocker::LockedTrack(address.track_id)
            ))
        );
        assert_eq!(sequence, before_locked);
    }

    #[test]
    fn duplicate_media_and_nested_source_selection_is_rejected_atomically() {
        let (mut media_sequence, first_address) = sequence_with_component();
        let alternate_media_source = AudioSourceComponentId::new();
        let mut second_media_edit =
            media_sequence.audio_tracks[0].clips[0].audio_components[0].clone();
        second_media_edit.id = AudioComponentEditId::new();
        second_media_edit.source =
            AudioComponentSource::Media { component_id: alternate_media_source };
        let second_media_address =
            AudioComponentAddress { edit_id: second_media_edit.id, ..first_address };
        media_sequence.audio_tracks[0].clips[0].audio_components.push(second_media_edit);
        media_sequence
            .audio_program
            .validate(
                &media_sequence.audio_tracks,
                &media_sequence.audio_roles,
                media_sequence.settings.audio_channel_layout,
            )
            .expect("distinct media Component sources");
        let media_before = media_sequence.clone();
        assert!(matches!(
            apply_audio_component_edit(
                &mut media_sequence,
                &request(
                    second_media_address,
                    AudioComponentMutation::SetSource {
                        value: AudioComponentSource::Media {
                            component_id: AudioSourceComponentId::primary(),
                        },
                    },
                ),
            ),
            Err(AudioComponentEditError::AuthorState(
                AudioAuthoringError::InvalidComponentSource(id)
            )) if id == second_media_address.edit_id
        ));
        assert_eq!(media_sequence, media_before);

        let mut nested_sequence = Sequence::new("nested Component authoring");
        let nested_track_id = nested_sequence.audio_tracks[0].id;
        let initial_output = mondrian_core::ProgramOutputId::new();
        let alternate_output = mondrian_core::ProgramOutputId::new();
        let nested_clip = Clip::new_nested_sequence(
            mondrian_core::SequenceId::new(),
            TimelineTime::ZERO,
            TimelineTime::new(10, 1).expect("duration"),
            None,
        )
        .expect("nested Clip");
        let nested_clip_id = nested_sequence
            .add_nested_audio_clip(nested_track_id, nested_clip, initial_output)
            .expect("nested audio Clip");
        let mut second_nested_edit =
            nested_sequence.audio_tracks[0].clips[0].audio_components[0].clone();
        second_nested_edit.id = AudioComponentEditId::new();
        second_nested_edit.source =
            AudioComponentSource::NestedOutput { output_id: alternate_output };
        let second_nested_address = AudioComponentAddress {
            track_id: nested_track_id,
            clip_id: nested_clip_id,
            edit_id: second_nested_edit.id,
        };
        nested_sequence.audio_tracks[0].clips[0]
            .audio_components
            .push(second_nested_edit);
        nested_sequence
            .audio_program
            .validate(
                &nested_sequence.audio_tracks,
                &nested_sequence.audio_roles,
                nested_sequence.settings.audio_channel_layout,
            )
            .expect("distinct child Outputs");
        let nested_before = nested_sequence.clone();
        assert!(matches!(
            apply_audio_component_edit(
                &mut nested_sequence,
                &request(
                    second_nested_address,
                    AudioComponentMutation::SetSource {
                        value: AudioComponentSource::NestedOutput {
                            output_id: initial_output,
                        },
                    },
                ),
            ),
            Err(AudioComponentEditError::AuthorState(
                AudioAuthoringError::InvalidComponentSource(id)
            )) if id == second_nested_address.edit_id
        ));
        assert_eq!(nested_sequence, nested_before);
    }

    #[test]
    fn static_controls_cannot_override_active_automation_authority() {
        let (mut sequence, address) = sequence_with_component();
        let component = &mut sequence.audio_tracks[0].clips[0].audio_components[0];
        component.volume_automation = Some(mondrian_core::ExactAutomationCurve {
            parameter_id: mondrian_core::ParameterId::new_static(
                crate::audio::CLIP_VOLUME_DB_PARAMETER_ID,
            ),
            default_value: 0.0,
            keyframes: mondrian_core::AuthoringList::new(),
        });
        let before = sequence.clone();
        assert_eq!(
            apply_audio_component_edit(
                &mut sequence,
                &request(address, AudioComponentMutation::SetVolumeDb { value: -3.0 }),
            ),
            Err(AudioComponentEditError::VolumeAutomationActive)
        );
        assert_eq!(sequence, before);
    }

    #[test]
    fn coefficient_edits_are_layout_guarded_and_rebuild_the_matrix_once() {
        let (mut sequence, address) = sequence_with_component();
        let matrix = AudioChannelMixMatrix::identity(AudioChannelLayout::Stereo);
        apply_audio_component_edit(
            &mut sequence,
            &request(
                address,
                AudioComponentMutation::SetChannelMapping {
                    value: AudioComponentChannelMapping::Explicit(matrix),
                },
            ),
        )
        .expect("explicit matrix");
        assert!(
            apply_audio_component_edit(
                &mut sequence,
                &request(
                    address,
                    AudioComponentMutation::SetExplicitChannelMixGain {
                        expected_source_layout: AudioChannelLayout::Stereo,
                        expected_destination_layout: AudioChannelLayout::Stereo,
                        source_channel: 0,
                        destination_channel: 0,
                        gain: 0.0,
                    },
                ),
            )
            .expect("remove edge")
            .changed
        );
        let AudioComponentChannelMapping::Explicit(updated) =
            &inspect_audio_component(&sequence, address)
                .expect("Component")
                .component()
                .channel_mapping
        else {
            panic!("expected explicit matrix");
        };
        assert_eq!(updated.entries().len(), 1);
        assert_eq!(updated.entries()[0].source_channel(), 1);

        let before_stale = sequence.clone();
        assert_eq!(
            apply_audio_component_edit(
                &mut sequence,
                &request(
                    address,
                    AudioComponentMutation::SetExplicitChannelMixGain {
                        expected_source_layout: AudioChannelLayout::Mono,
                        expected_destination_layout: AudioChannelLayout::Stereo,
                        source_channel: 0,
                        destination_channel: 0,
                        gain: 1.0,
                    },
                ),
            ),
            Err(AudioComponentEditError::ExplicitChannelMatrixChanged)
        );
        assert_eq!(sequence, before_stale);
    }
}
