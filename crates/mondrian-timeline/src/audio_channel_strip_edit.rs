//! Atomic authoring operations for normative Audio Channel Strip controls.
//!
//! Input trim and fader are fixed signal-chain stages, not synthetic Rack
//! processors. This Module gives them one typed address, inspection, lock
//! admission, and transaction Implementation across Track, Bus, and Output.

use crate::audio::{
    AudioAuthoringError, AudioChannelStrip, AudioChannelStripOwner, FADER_DB_PARAMETER_ID,
};
use crate::sequence::Sequence;
use mondrian_core::{
    ExactAutomationCurve, ExactAutomationKeyframe, KeyframeId, MixBusId, ParameterId,
    ProgramOutputId, TrackId,
};
use serde::{Deserialize, Serialize};

/// Fine-grained mutation of one normative Audio Channel Strip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum AudioChannelStripEdit {
    /// Replace the static input trim applied before the pre-fader Rack.
    SetInputTrimDb {
        /// Decibel value validated by the complete Audio Program contract.
        value: f64,
    },
    /// Replace the fader value only when no automation curve is authoritative.
    SetFaderDb {
        /// Decibel value validated by the complete Audio Program contract.
        value: f64,
    },
    /// Insert, replace, or move one exact Sequence-time fader keyframe.
    UpsertFaderKeyframe {
        /// Complete keyframe state with stable identity.
        keyframe: ExactAutomationKeyframe,
    },
    /// Remove one fader keyframe by stable identity.
    RemoveFaderKeyframe {
        /// Keyframe to remove.
        keyframe_id: KeyframeId,
    },
    /// Remove fader automation and install one explicit static value.
    ClearFaderAutomation {
        /// Static decibel value retained after the curve is removed.
        fader_db: f64,
    },
}

/// Complete Channel Strip address and edit intent for one author transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioChannelStripEditRequest {
    /// Stable typed strip owner.
    pub owner: AudioChannelStripOwner,
    /// Exact normative-stage mutation.
    pub edit: AudioChannelStripEdit,
}

/// Receipt from one successful Channel Strip edit attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioChannelStripEditOutcome {
    /// Whether canonical author state changed.
    pub changed: bool,
}

/// Fail-closed Channel Strip address or mutation error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioChannelStripAddressError {
    /// Track owner does not exist in the Sequence audio Timeline.
    #[error("Audio Channel Strip references an unknown audio Track: {0}")]
    UnknownTrack(TrackId),
    /// Mix Bus owner is absent from the Sequence Audio Program.
    #[error("Audio Channel Strip references an unknown Mix Bus: {0}")]
    UnknownBus(MixBusId),
    /// Program Output owner is absent from the Sequence Audio Program.
    #[error("Audio Channel Strip references an unknown Program Output: {0}")]
    UnknownProgramOutput(ProgramOutputId),
}

/// Temporary author-state condition blocking an otherwise valid strip edit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioChannelStripEditBlocker {
    /// A persistent Track lock forbids editing its Channel Strip.
    #[error("Audio Channel Strip cannot modify locked Track: {0}")]
    LockedTrack(TrackId),
}

/// Fail-closed Channel Strip address, admission, or mutation error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioChannelStripEditError {
    /// Stable owner could not be resolved in this Sequence snapshot.
    #[error(transparent)]
    Address(#[from] AudioChannelStripAddressError),
    /// Valid owner is currently protected by authoring policy.
    #[error(transparent)]
    Blocked(#[from] AudioChannelStripEditBlocker),
    /// A static fader edit would change a value that is not signal authority.
    #[error(
        "Audio Channel Strip fader automation is active; edit the curve or clear it explicitly"
    )]
    FaderAutomationActive,
    /// Keyframe identity is absent from the fader automation curve.
    #[error("Audio Channel Strip fader has no keyframe {0}")]
    UnknownKeyframe(KeyframeId),
    /// A distinct keyframe already owns the requested exact time.
    #[error("Audio Channel Strip fader already has another keyframe at the requested time")]
    KeyframeTimeCollision,
    /// Exact automation construction or interpolation failed.
    #[error("Audio Channel Strip fader automation is invalid: {reason}")]
    InvalidAutomation {
        /// Stable diagnostic detail from the exact automation contract.
        reason: String,
    },
    /// Complete resulting audio author state is invalid.
    #[error("Audio Channel Strip edit produced invalid author state: {0}")]
    AuthorState(AudioAuthoringError),
}

/// Read-only projection of one resolved Audio Channel Strip and edit admission.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioChannelStripInspection<'a> {
    strip: &'a AudioChannelStrip,
    edit_blocker: Option<AudioChannelStripEditBlocker>,
}

impl<'a> AudioChannelStripInspection<'a> {
    /// Resolved canonical Channel Strip.
    pub fn strip(&self) -> &'a AudioChannelStrip {
        self.strip
    }

    /// Authoritative reason this valid strip cannot currently be edited.
    pub fn edit_blocker(&self) -> Option<&AudioChannelStripEditBlocker> {
        self.edit_blocker.as_ref()
    }

    /// Whether an edit request against this strip may enter a transaction.
    pub fn is_editable(&self) -> bool {
        self.edit_blocker.is_none()
    }
}

/// Resolve a Channel Strip and inspect Track-lock admission from one snapshot.
pub fn inspect_audio_channel_strip<'a>(
    sequence: &'a Sequence,
    owner: AudioChannelStripOwner,
) -> Result<AudioChannelStripInspection<'a>, AudioChannelStripAddressError> {
    let strip = resolve_strip(sequence, owner)?;
    let edit_blocker = match owner {
        AudioChannelStripOwner::Track { track_id } => sequence
            .audio_tracks
            .iter()
            .find(|track| track.id == track_id)
            .ok_or(AudioChannelStripAddressError::UnknownTrack(track_id))?
            .is_locked
            .then_some(AudioChannelStripEditBlocker::LockedTrack(track_id)),
        AudioChannelStripOwner::Bus { .. } | AudioChannelStripOwner::ProgramOutput { .. } => None,
    };
    Ok(AudioChannelStripInspection { strip, edit_blocker })
}

/// Resolve one Channel Strip for read-only consumers by stable typed owner.
pub fn audio_channel_strip(
    sequence: &Sequence,
    owner: AudioChannelStripOwner,
) -> Result<&AudioChannelStrip, AudioChannelStripAddressError> {
    resolve_strip(sequence, owner)
}

/// Apply one normative Channel Strip edit atomically to a Sequence candidate.
///
/// Address and lock admission happen before the copy-on-write candidate
/// detaches. The input Sequence is replaced only after complete Audio Program
/// validation, and canonical no-ops produce no author revision.
pub fn apply_audio_channel_strip_edit(
    sequence: &mut Sequence,
    request: &AudioChannelStripEditRequest,
) -> Result<AudioChannelStripEditOutcome, AudioChannelStripEditError> {
    ensure_editable(sequence, request.owner)?;
    let mut candidate = sequence.clone();
    let changed = apply_to_strip(
        resolve_strip_mut(&mut candidate, request.owner)?,
        &request.edit,
    )?;
    if !changed {
        return Ok(AudioChannelStripEditOutcome { changed: false });
    }
    candidate
        .audio_program
        .validate(
            &candidate.audio_tracks,
            &candidate.audio_roles,
            candidate.settings.audio_channel_layout,
        )
        .map_err(AudioChannelStripEditError::AuthorState)?;
    *sequence = candidate;
    Ok(AudioChannelStripEditOutcome { changed: true })
}

fn ensure_editable(
    sequence: &Sequence,
    owner: AudioChannelStripOwner,
) -> Result<(), AudioChannelStripEditError> {
    let inspection = inspect_audio_channel_strip(sequence, owner)?;
    if let Some(blocker) = inspection.edit_blocker {
        Err(blocker.into())
    } else {
        Ok(())
    }
}

fn resolve_strip(
    sequence: &Sequence,
    owner: AudioChannelStripOwner,
) -> Result<&AudioChannelStrip, AudioChannelStripAddressError> {
    match owner {
        AudioChannelStripOwner::Track { track_id } => {
            if !sequence.audio_tracks.iter().any(|track| track.id == track_id) {
                return Err(AudioChannelStripAddressError::UnknownTrack(track_id));
            }
            sequence
                .audio_program
                .track_channels
                .get(&track_id)
                .map(|channel| &channel.strip)
                .ok_or(AudioChannelStripAddressError::UnknownTrack(track_id))
        }
        AudioChannelStripOwner::Bus { bus_id } => sequence
            .audio_program
            .buses
            .iter()
            .find(|bus| bus.id == bus_id)
            .map(|bus| &bus.strip)
            .ok_or(AudioChannelStripAddressError::UnknownBus(bus_id)),
        AudioChannelStripOwner::ProgramOutput { output_id } => sequence
            .audio_program
            .outputs
            .iter()
            .find(|output| output.id == output_id)
            .map(|output| &output.strip)
            .ok_or(AudioChannelStripAddressError::UnknownProgramOutput(
                output_id,
            )),
    }
}

fn resolve_strip_mut(
    sequence: &mut Sequence,
    owner: AudioChannelStripOwner,
) -> Result<&mut AudioChannelStrip, AudioChannelStripEditError> {
    match owner {
        AudioChannelStripOwner::Track { track_id } => sequence
            .audio_program
            .track_channels
            .get_mut(&track_id)
            .map(|channel| &mut channel.strip)
            .ok_or(AudioChannelStripAddressError::UnknownTrack(track_id).into()),
        AudioChannelStripOwner::Bus { bus_id } => sequence
            .audio_program
            .buses
            .iter_mut()
            .find(|bus| bus.id == bus_id)
            .map(|bus| &mut bus.strip)
            .ok_or(AudioChannelStripAddressError::UnknownBus(bus_id).into()),
        AudioChannelStripOwner::ProgramOutput { output_id } => sequence
            .audio_program
            .outputs
            .iter_mut()
            .find(|output| output.id == output_id)
            .map(|output| &mut output.strip)
            .ok_or(AudioChannelStripAddressError::UnknownProgramOutput(output_id).into()),
    }
}

fn apply_to_strip(
    strip: &mut AudioChannelStrip,
    edit: &AudioChannelStripEdit,
) -> Result<bool, AudioChannelStripEditError> {
    match edit {
        AudioChannelStripEdit::SetInputTrimDb { value } => {
            if strip.input_trim_db == *value {
                return Ok(false);
            }
            strip.input_trim_db = *value;
            Ok(true)
        }
        AudioChannelStripEdit::SetFaderDb { value } => {
            if strip.fader_automation.is_some() {
                return Err(AudioChannelStripEditError::FaderAutomationActive);
            }
            if strip.fader_db == *value {
                return Ok(false);
            }
            strip.fader_db = *value;
            Ok(true)
        }
        AudioChannelStripEdit::UpsertFaderKeyframe { keyframe } => {
            let mut curve = match &strip.fader_automation {
                Some(curve) => curve.clone(),
                None => ExactAutomationCurve::new(
                    ParameterId::new_static(FADER_DB_PARAMETER_ID),
                    strip.fader_db,
                )
                .map_err(|error| {
                    AudioChannelStripEditError::InvalidAutomation { reason: error.to_string() }
                })?,
            };
            if curve
                .keyframes
                .iter()
                .any(|candidate| candidate.id != keyframe.id && candidate.time == keyframe.time)
            {
                return Err(AudioChannelStripEditError::KeyframeTimeCollision);
            }
            curve.keyframes.retain(|candidate| candidate.id != keyframe.id);
            curve.set_keyframe(keyframe.clone()).map_err(|error| {
                AudioChannelStripEditError::InvalidAutomation { reason: error.to_string() }
            })?;
            if strip.fader_automation.as_ref() == Some(&curve) {
                return Ok(false);
            }
            strip.fader_automation = Some(curve);
            Ok(true)
        }
        AudioChannelStripEdit::RemoveFaderKeyframe { keyframe_id } => {
            let mut curve = strip
                .fader_automation
                .clone()
                .ok_or(AudioChannelStripEditError::UnknownKeyframe(*keyframe_id))?;
            let before = curve.keyframes.len();
            curve.keyframes.retain(|candidate| candidate.id != *keyframe_id);
            if curve.keyframes.len() == before {
                return Err(AudioChannelStripEditError::UnknownKeyframe(*keyframe_id));
            }
            if curve.keyframes.is_empty() {
                strip.fader_db = curve.default_value;
                strip.fader_automation = None;
            } else {
                strip.fader_automation = Some(curve);
            }
            Ok(true)
        }
        AudioChannelStripEdit::ClearFaderAutomation { fader_db } => {
            if strip.fader_automation.is_none() && strip.fader_db == *fader_db {
                return Ok(false);
            }
            strip.fader_db = *fader_db;
            strip.fader_automation = None;
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{AudioChannelStrip, AudioMixBus};
    use mondrian_core::TimelineTime;

    fn request(
        owner: AudioChannelStripOwner,
        edit: AudioChannelStripEdit,
    ) -> AudioChannelStripEditRequest {
        AudioChannelStripEditRequest { owner, edit }
    }

    #[test]
    fn one_interface_edits_track_bus_and_output_normative_stages() {
        let mut sequence = Sequence::new("Channel Strip authoring");
        let track_id = sequence.audio_tracks[0].id;
        let bus_id = MixBusId::new();
        sequence.audio_program.buses.push(AudioMixBus {
            id: bus_id,
            name: "Dialog".to_owned(),
            strip: AudioChannelStrip::default(),
        });
        let output_id = sequence.audio_program.outputs[0].id;
        let track_owner = AudioChannelStripOwner::Track { track_id };
        let bus_owner = AudioChannelStripOwner::Bus { bus_id };
        let output_owner = AudioChannelStripOwner::ProgramOutput { output_id };
        let edits = [
            (
                track_owner,
                AudioChannelStripEdit::SetInputTrimDb { value: -3.0 },
            ),
            (bus_owner, AudioChannelStripEdit::SetFaderDb { value: -6.0 }),
            (
                output_owner,
                AudioChannelStripEdit::SetFaderDb { value: -1.0 },
            ),
        ];
        for (owner, edit) in edits {
            assert!(
                apply_audio_channel_strip_edit(&mut sequence, &request(owner, edit))
                    .expect("valid strip edit")
                    .changed
            );
        }
        assert_eq!(
            audio_channel_strip(&sequence, track_owner).expect("Track").input_trim_db,
            -3.0
        );
        assert_eq!(
            audio_channel_strip(&sequence, bus_owner).expect("Bus").fader_db,
            -6.0
        );
        assert_eq!(
            audio_channel_strip(&sequence, output_owner).expect("Output").fader_db,
            -1.0
        );
    }

    #[test]
    fn fader_automation_is_exact_canonical_and_never_faked_as_static() {
        let mut sequence = Sequence::new("Fader automation");
        let owner = AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id };
        let keyframe = ExactAutomationKeyframe::linear(
            TimelineTime::new(1, 48_000).expect("sample time"),
            -9.0,
        );
        let keyframe_id = keyframe.id;
        apply_audio_channel_strip_edit(
            &mut sequence,
            &request(
                owner,
                AudioChannelStripEdit::UpsertFaderKeyframe { keyframe },
            ),
        )
        .expect("keyframe");
        let strip = audio_channel_strip(&sequence, owner).expect("Track");
        assert_eq!(
            strip.fader_automation.as_ref().expect("curve").keyframes[0].id,
            keyframe_id
        );

        let before = sequence.clone();
        assert_eq!(
            apply_audio_channel_strip_edit(
                &mut sequence,
                &request(owner, AudioChannelStripEdit::SetFaderDb { value: -3.0 }),
            ),
            Err(AudioChannelStripEditError::FaderAutomationActive)
        );
        assert_eq!(sequence, before);

        apply_audio_channel_strip_edit(
            &mut sequence,
            &request(
                owner,
                AudioChannelStripEdit::RemoveFaderKeyframe { keyframe_id },
            ),
        )
        .expect("remove last key");
        let strip = audio_channel_strip(&sequence, owner).expect("Track");
        assert!(strip.fader_automation.is_none());
        assert_eq!(strip.fader_db, 0.0);
    }

    #[test]
    fn lock_invalid_value_collision_and_noop_fail_or_commit_atomically() {
        let mut sequence = Sequence::new("Channel Strip admission");
        let track_id = sequence.audio_tracks[0].id;
        let owner = AudioChannelStripOwner::Track { track_id };
        assert!(
            !apply_audio_channel_strip_edit(
                &mut sequence,
                &request(owner, AudioChannelStripEdit::SetFaderDb { value: 0.0 }),
            )
            .expect("no-op")
            .changed
        );

        let before_invalid = sequence.clone();
        assert!(matches!(
            apply_audio_channel_strip_edit(
                &mut sequence,
                &request(owner, AudioChannelStripEdit::SetInputTrimDb { value: 25.0 }),
            ),
            Err(AudioChannelStripEditError::AuthorState(
                AudioAuthoringError::InvalidGain
            ))
        ));
        assert_eq!(sequence, before_invalid);

        let time = TimelineTime::ONE;
        let first = ExactAutomationKeyframe::linear(time, -3.0);
        let second = ExactAutomationKeyframe::linear(time, -6.0);
        apply_audio_channel_strip_edit(
            &mut sequence,
            &request(
                owner,
                AudioChannelStripEdit::UpsertFaderKeyframe { keyframe: first },
            ),
        )
        .expect("first key");
        let before_collision = sequence.clone();
        assert_eq!(
            apply_audio_channel_strip_edit(
                &mut sequence,
                &request(
                    owner,
                    AudioChannelStripEdit::UpsertFaderKeyframe { keyframe: second }
                ),
            ),
            Err(AudioChannelStripEditError::KeyframeTimeCollision)
        );
        assert_eq!(sequence, before_collision);

        sequence.audio_tracks[0].is_locked = true;
        let before_locked = sequence.clone();
        assert_eq!(
            apply_audio_channel_strip_edit(
                &mut sequence,
                &request(
                    owner,
                    AudioChannelStripEdit::ClearFaderAutomation { fader_db: -2.0 }
                ),
            ),
            Err(AudioChannelStripEditError::Blocked(
                AudioChannelStripEditBlocker::LockedTrack(track_id)
            ))
        );
        assert_eq!(sequence, before_locked);
    }
}
