//! Domain-qualified author-time projection for audio automation surfaces.

use crate::{
    inspect_audio_automation, inspect_audio_component, AudioAutomationAddressError,
    AudioAutomationTarget, AudioComponentAddress, AudioComponentAddressError, Sequence,
};
use mondrian_core::{
    AudioComponentEditId, AudioProcessingScopeId, AuthoringTimeDomain, DomainTime,
    TimelineTimeError, TimelineTimeRange,
};

/// Failure to resolve the current coordinate of an addressed audio author curve.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioAutomationTimeError {
    /// The curve address no longer exists or is not an animatable numeric target.
    #[error(transparent)]
    AutomationAddress(#[from] AudioAutomationAddressError),
    /// The explicitly selected occurrence no longer exists.
    #[error(transparent)]
    ComponentAddress(#[from] AudioComponentAddressError),
    /// The supplied playhead does not belong to this Sequence.
    #[error("Audio automation playhead belongs to another author domain")]
    SequenceDomainMismatch,
    /// A shared Scope requires a selected occurrence, even if only one is bound.
    #[error("Audio Processing Scope {0} requires an explicit Component occurrence")]
    MissingScopeOccurrence(AudioProcessingScopeId),
    /// The selected occurrence belongs to another Scope.
    #[error("Component {edit_id} is not bound to Audio Processing Scope {scope_id}")]
    ScopeOccurrenceMismatch {
        /// Requested Scope.
        scope_id: AudioProcessingScopeId,
        /// Selected occurrence.
        edit_id: AudioComponentEditId,
    },
    /// Invalid author state cannot produce an owner-local coordinate.
    #[error("Audio Component {0} has a negative author origin")]
    NegativeOwnerOrigin(AudioComponentEditId),
    /// An execution callback falls outside its exact half-open occurrence.
    #[error("Audio execution callback is outside Component {0}")]
    OutsideOccurrence(AudioComponentEditId),
    /// This interface does not interpret another authoring domain.
    #[error("Unsupported audio automation author domain: {0:?}")]
    UnsupportedDomain(AuthoringTimeDomain),
    /// Exact placement or owner-time arithmetic is not representable.
    #[error(transparent)]
    Time(#[from] TimelineTimeError),
}

/// Resolve a Sequence playhead into the target's exact audio author domain.
///
/// Placement-local targets carry their own stable Component address. Shared
/// Processing Scopes require an explicit occurrence; no first-binding or
/// viewport-origin fallback exists. Outside a Clip, projection clamps to its
/// closed placement extent, matching visual Inspector boundary policy. Source
/// slip, source rate and visual Clip author origins do not change audio owners.
/// Reading time does not mutate author state or bypass edit/Track-lock admission.
pub fn audio_automation_author_time(
    sequence: &Sequence,
    target: &AudioAutomationTarget,
    sequence_time: DomainTime,
    scope_occurrence: Option<AudioComponentAddress>,
) -> Result<DomainTime, AudioAutomationTimeError> {
    project_audio_automation_time(sequence, target, sequence_time, scope_occurrence, true)
}

/// Resolve a native Process callback in the exact addressed execution owner.
///
/// Unlike UI playhead projection, an execution observation is refused outside
/// its actual half-open occurrence. No boundary key or default value is made.
pub fn audio_automation_execution_time(
    sequence: &Sequence,
    target: &AudioAutomationTarget,
    sequence_time: DomainTime,
    scope_occurrence: Option<AudioComponentAddress>,
) -> Result<DomainTime, AudioAutomationTimeError> {
    project_audio_automation_time(sequence, target, sequence_time, scope_occurrence, false)
}

fn project_audio_automation_time(
    sequence: &Sequence,
    target: &AudioAutomationTarget,
    sequence_time: DomainTime,
    scope_occurrence: Option<AudioComponentAddress>,
    clamp_playhead: bool,
) -> Result<DomainTime, AudioAutomationTimeError> {
    if sequence_time.domain != AuthoringTimeDomain::Sequence(sequence.id) {
        return Err(AudioAutomationTimeError::SequenceDomainMismatch);
    }
    let domain = inspect_audio_automation(sequence, target)?.time_domain();
    let (address, scope) = match domain {
        AuthoringTimeDomain::Sequence(_) => return Ok(sequence_time),
        AuthoringTimeDomain::AudioComponentEdit(_) => {
            let address = match target {
                AudioAutomationTarget::ComponentVolume { track_id, clip_id, edit_id }
                | AudioAutomationTarget::ComponentPan { track_id, clip_id, edit_id } => {
                    AudioComponentAddress {
                        track_id: *track_id,
                        clip_id: *clip_id,
                        edit_id: *edit_id,
                    }
                }
                _ => return Err(AudioAutomationTimeError::UnsupportedDomain(domain)),
            };
            (address, None)
        }
        AuthoringTimeDomain::AudioProcessingScope(scope_id) => (
            scope_occurrence.ok_or(AudioAutomationTimeError::MissingScopeOccurrence(scope_id))?,
            Some(scope_id),
        ),
        _ => return Err(AudioAutomationTimeError::UnsupportedDomain(domain)),
    };
    let occurrence = inspect_audio_component(sequence, address)?;
    let component = occurrence.component();
    if let Some(scope_id) = scope
        && component.processing.scope_id != scope_id
    {
        return Err(AudioAutomationTimeError::ScopeOccurrenceMismatch {
            scope_id,
            edit_id: address.edit_id,
        });
    }
    let origin = if scope.is_some() {
        component.processing.scope_in
    } else {
        component.local_time_in
    };
    if origin.is_negative() {
        return Err(AudioAutomationTimeError::NegativeOwnerOrigin(
            address.edit_id,
        ));
    }
    let clip = occurrence.owning_clip();
    let placement = TimelineTimeRange::new(clip.position, clip.duration)?;
    let end = placement.end()?;
    let projected = if clamp_playhead {
        sequence_time.time.clamp(placement.start, end)
    } else {
        if sequence_time.time < placement.start || sequence_time.time >= end {
            return Err(AudioAutomationTimeError::OutsideOccurrence(address.edit_id));
        }
        sequence_time.time
    };
    let elapsed = projected.checked_sub(placement.start)?;
    Ok(DomainTime::new(domain, origin.checked_add(elapsed)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{AssetId, AudioSourceComponentId, SequenceId, TimelineTime};

    fn tt(numerator: i64, denominator: i64) -> TimelineTime {
        TimelineTime::new(numerator, denominator).expect("exact fixture time")
    }

    #[test]
    fn current_audio_time_uses_explicit_owner_origins_and_occurrences() {
        let mut sequence = Sequence::new("Audio author clock");
        let track_id = sequence.audio_tracks[0].id;
        let clip = crate::Clip::new(AssetId::new(), tt(10, 1), tt(4, 1)).expect("clip");
        sequence
            .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
            .expect("audio clip");
        let clip = &mut sequence.audio_tracks[0].clips[0];
        clip.clip_time_in = tt(100, 1);
        clip.audio_components[0].local_time_in = tt(2, 3);
        clip.audio_components[0].processing.scope_in = tt(7, 5);
        let address = AudioComponentAddress {
            track_id,
            clip_id: clip.id,
            edit_id: clip.audio_components[0].id,
        };
        let scope_id = clip.audio_components[0].processing.scope_id;
        let component = AudioAutomationTarget::ComponentVolume {
            track_id,
            clip_id: address.clip_id,
            edit_id: address.edit_id,
        };
        let scope = AudioAutomationTarget::ProcessingScopeInputGain { scope_id };
        let playhead = |time| DomainTime::new(AuthoringTimeDomain::Sequence(sequence.id), time);
        for (time, elapsed) in [
            (tt(9, 1), TimelineTime::ZERO),
            (tt(41, 4), tt(1, 4)),
            (tt(14, 1), tt(4, 1)),
            (tt(20, 1), tt(4, 1)),
        ] {
            assert_eq!(
                audio_automation_author_time(&sequence, &component, playhead(time), None)
                    .expect("component"),
                DomainTime::new(
                    AuthoringTimeDomain::AudioComponentEdit(address.edit_id),
                    tt(2, 3).checked_add(elapsed).expect("time")
                )
            );
            assert_eq!(
                audio_automation_author_time(&sequence, &scope, playhead(time), Some(address))
                    .expect("scope"),
                DomainTime::new(
                    AuthoringTimeDomain::AudioProcessingScope(scope_id),
                    tt(7, 5).checked_add(elapsed).expect("time")
                )
            );
        }
        assert_eq!(
            audio_automation_author_time(&sequence, &scope, playhead(tt(41, 4)), None),
            Err(AudioAutomationTimeError::MissingScopeOccurrence(scope_id))
        );
        let fader = AudioAutomationTarget::ChannelFader {
            owner: crate::AudioChannelStripOwner::Track { track_id },
        };
        assert_eq!(
            audio_automation_author_time(&sequence, &fader, playhead(tt(-1, 3)), None)
                .expect("Sequence"),
            playhead(tt(-1, 3))
        );
        assert_eq!(
            audio_automation_author_time(
                &sequence,
                &scope,
                DomainTime::new(AuthoringTimeDomain::Sequence(SequenceId::new()), tt(41, 4)),
                Some(address)
            ),
            Err(AudioAutomationTimeError::SequenceDomainMismatch)
        );
    }
    #[test]
    fn shared_scope_time_never_chooses_another_binding_and_rejects_stale_context() {
        let mut sequence = Sequence::new("Shared scope occurrences");
        let track_id = sequence.audio_tracks[0].id;
        for start in [tt(10, 1), tt(20, 1)] {
            let clip = crate::Clip::new(AssetId::new(), start, tt(2, 1)).expect("clip");
            sequence
                .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
                .expect("clip");
        }
        let first = &sequence.audio_tracks[0].clips[0];
        let scope_id = first.audio_components[0].processing.scope_id;
        let first_address = AudioComponentAddress {
            track_id,
            clip_id: first.id,
            edit_id: first.audio_components[0].id,
        };
        let second = &mut sequence.audio_tracks[0].clips[1];
        let second_address = AudioComponentAddress {
            track_id,
            clip_id: second.id,
            edit_id: second.audio_components[0].id,
        };
        let other_scope_id = second.audio_components[0].processing.scope_id;
        let target = AudioAutomationTarget::ProcessingScopeInputGain { scope_id };
        let playhead = DomainTime::new(AuthoringTimeDomain::Sequence(sequence.id), tt(41, 2));
        assert_eq!(
            audio_automation_author_time(&sequence, &target, playhead, Some(second_address)),
            Err(AudioAutomationTimeError::ScopeOccurrenceMismatch {
                scope_id,
                edit_id: second_address.edit_id
            })
        );
        let second = &mut sequence.audio_tracks[0].clips[1];
        second.audio_components[0].processing.scope_id = scope_id;
        second.audio_components[0].processing.scope_in = tt(9, 1);
        let before = sequence.clone();
        assert_eq!(
            audio_automation_author_time(&sequence, &target, playhead, Some(first_address))
                .expect("first"),
            DomainTime::new(
                AuthoringTimeDomain::AudioProcessingScope(scope_id),
                tt(2, 1)
            )
        );
        assert_eq!(
            audio_automation_author_time(&sequence, &target, playhead, Some(second_address))
                .expect("second"),
            DomainTime::new(
                AuthoringTimeDomain::AudioProcessingScope(scope_id),
                tt(19, 2)
            )
        );
        assert_eq!(
            sequence, before,
            "time inspection must never mutate author state"
        );
        let wrong_target =
            AudioAutomationTarget::ProcessingScopeInputGain { scope_id: other_scope_id };
        assert!(matches!(
            audio_automation_author_time(&sequence, &wrong_target, playhead, Some(second_address)),
            Err(AudioAutomationTimeError::ScopeOccurrenceMismatch { .. })
        ));
        sequence.audio_tracks[0].clips.remove(1);
        assert!(matches!(
            audio_automation_author_time(&sequence, &target, playhead, Some(second_address)),
            Err(AudioAutomationTimeError::ComponentAddress(
                AudioComponentAddressError::UnknownClip { .. }
            ))
        ));
        let first = &mut sequence.audio_tracks[0].clips[0];
        first.audio_components[0].processing.scope_in = tt(i64::MAX, 1);
        assert_eq!(
            audio_automation_author_time(&sequence, &target, playhead, Some(first_address)),
            Err(AudioAutomationTimeError::Time(TimelineTimeError::Overflow))
        );
        first_origin_negative(&mut sequence, first_address, &target, playhead);
    }

    fn first_origin_negative(
        sequence: &mut Sequence,
        address: AudioComponentAddress,
        target: &AudioAutomationTarget,
        playhead: DomainTime,
    ) {
        sequence.audio_tracks[0].clips[0].audio_components[0].processing.scope_in = tt(-1, 1);
        assert_eq!(
            audio_automation_author_time(sequence, target, playhead, Some(address)),
            Err(AudioAutomationTimeError::NegativeOwnerOrigin(
                address.edit_id
            ))
        );
    }
}

#[cfg(test)]
mod native_execution_time_contracts {
    use super::*;
    use mondrian_core::{AssetId, AudioSourceComponentId, TimelineTime};
    fn tt(n: i64, d: i64) -> TimelineTime {
        TimelineTime::new(n, d).expect("exact time")
    }
    #[test]
    fn audio_execution_time_refuses_outside_occurrence_without_clamping_keys() {
        let mut sequence = Sequence::new("Native exact callback");
        let track = sequence.audio_tracks[0].id;
        let clip = crate::Clip::new(AssetId::new(), tt(10, 1), tt(2, 1)).expect("clip");
        sequence
            .add_media_audio_clip(track, clip, AudioSourceComponentId::primary())
            .expect("native occurrence");
        let clip = &mut sequence.audio_tracks[0].clips[0];
        clip.audio_components[0].processing.scope_in = tt(7, 5);
        let occurrence = AudioComponentAddress {
            track_id: track,
            clip_id: clip.id,
            edit_id: clip.audio_components[0].id,
        };
        let scope = clip.audio_components[0].processing.scope_id;
        let target = AudioAutomationTarget::ProcessingScopeInputGain { scope_id: scope };
        let position = |time| DomainTime::new(AuthoringTimeDomain::Sequence(sequence.id), time);
        for time in [tt(9, 1), tt(12, 1), tt(13, 1)] {
            assert_eq!(
                audio_automation_execution_time(
                    &sequence,
                    &target,
                    position(time),
                    Some(occurrence)
                ),
                Err(AudioAutomationTimeError::OutsideOccurrence(
                    occurrence.edit_id
                ))
            );
        }
        assert_eq!(
            audio_automation_execution_time(
                &sequence,
                &target,
                position(tt(41, 4)),
                Some(occurrence)
            )
            .expect("inside"),
            DomainTime::new(AuthoringTimeDomain::AudioProcessingScope(scope), tt(33, 20))
        );
        assert_eq!(
            audio_automation_author_time(&sequence, &target, position(tt(13, 1)), Some(occurrence))
                .expect("UI clamp remains valid"),
            DomainTime::new(AuthoringTimeDomain::AudioProcessingScope(scope), tt(17, 5))
        );
    }
}
