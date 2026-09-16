//! Strict CMX 3600 A-mode picture conform Adapter.

use super::{
    finding, validate_timeline, EncodedFormat, InterchangeClip, InterchangeTimeline,
    InterchangeTrack, InterchangeTransition, ParsedFormat,
};
use crate::{
    InterchangeDisposition, InterchangeError, InterchangeFindingSeverity, InterchangeFormatProfile,
    InterchangeLimits, InterchangeMediaKey, InterchangeMediaReference, InterchangeTrackKind,
};
use mondrian_core::timeline_data::EditorialSourceIdentity;
use mondrian_core::{
    Rational, SmpteCountingMode, SmpteDisplayTimecodeContract, SmpteTimecodeReference,
};
use std::collections::HashMap;

const PROFILE: InterchangeFormatProfile = InterchangeFormatProfile::Cmx3600;

#[derive(Debug)]
struct Event {
    number: String,
    reel: String,
    edit: char,
    dissolve: i64,
    source_in: i64,
    source_out: i64,
    record_in: i64,
    record_out: i64,
}

pub(crate) fn parse(
    bytes: &[u8],
    limits: InterchangeLimits,
    explicit_rate: Option<Rational>,
) -> Result<ParsedFormat, InterchangeError> {
    let rate =
        explicit_rate.ok_or_else(|| malformed("CMX import requires an explicit frame rate"))?;
    let mode_hint = std::str::from_utf8(bytes)
        .map_err(|error| malformed(error.to_string()))?
        .lines()
        .find_map(|line| line.trim().strip_prefix("FCM:"))
        .map(str::trim);
    let mode = match mode_hint {
        Some("DROP FRAME") => SmpteCountingMode::DropFrame,
        Some("NON-DROP FRAME") => SmpteCountingMode::NonDropFrame,
        Some(other) => return Err(malformed(format!("unsupported FCM declaration '{other}'"))),
        None => return Err(malformed("FCM declaration is required")),
    };
    let contract = SmpteDisplayTimecodeContract::new(rate, mode, 0)
        .map_err(|error| malformed(error.to_string()))?;
    let text = std::str::from_utf8(bytes).map_err(|error| malformed(error.to_string()))?;
    let title = text
        .lines()
        .find_map(|line| line.strip_prefix("TITLE:"))
        .map(str::trim)
        .unwrap_or("Untitled CMX")
        .to_owned();
    let mut events = Vec::new();
    for (line_index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty()
            || line.starts_with('*')
            || line.starts_with("TITLE:")
            || line.starts_with("FCM:")
        {
            continue;
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 8
            || fields[0].len() != 3
            || !fields[0].bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(malformed(format!(
                "line {} is not a strict CMX event",
                line_index + 1
            )));
        }
        if fields[2] != "V" {
            return Err(InterchangeError::UnsupportedSemantics {
                profile: PROFILE,
                reason: format!("line {} is not a picture-only V event", line_index + 1),
            });
        }
        let edit = fields[3].chars().next().ok_or_else(|| malformed("missing edit type"))?;
        let (dissolve, tc_index) = match edit {
            'C' => (0, 4),
            'D' => {
                if fields.len() < 9 {
                    return Err(malformed(format!(
                        "line {} dissolve duration is missing",
                        line_index + 1
                    )));
                }
                let value = fields[4].parse::<i64>().map_err(|_| {
                    malformed(format!(
                        "line {} has invalid dissolve duration",
                        line_index + 1
                    ))
                })?;
                (value, 5)
            }
            _ => {
                return Err(InterchangeError::UnsupportedSemantics {
                    profile: PROFILE,
                    reason: format!(
                        "line {} edit type '{edit}' is outside C/D subset",
                        line_index + 1
                    ),
                })
            }
        };
        if fields.len() != tc_index + 4 {
            return Err(malformed(format!(
                "line {} has unexpected fields",
                line_index + 1
            )));
        }
        let parse_tc = |value: &str| {
            contract.parse_label(value).map_err(|error| {
                malformed(format!(
                    "line {} invalid timecode '{value}': {error}",
                    line_index + 1
                ))
            })
        };
        events.push(Event {
            number: fields[0].to_owned(),
            reel: fields[1].to_owned(),
            edit,
            dissolve,
            source_in: parse_tc(fields[tc_index])?,
            source_out: parse_tc(fields[tc_index + 1])?,
            record_in: parse_tc(fields[tc_index + 2])?,
            record_out: parse_tc(fields[tc_index + 3])?,
        });
    }
    if events.len() > 999 {
        return Err(InterchangeError::LimitExceeded {
            limit_name: "CMX events",
            actual: events.len(),
            maximum: 999,
        });
    }
    let start_frame = events.first().map(|event| event.record_in).unwrap_or(0);
    let start_timecode = Some(
        SmpteTimecodeReference::new(rate, mode, start_frame)
            .map_err(|error| malformed(error.to_string()))?,
    );
    let mut media = HashMap::new();
    let mut clips = Vec::new();
    let mut transitions = Vec::new();
    let mut previous_clip: Option<String> = None;
    for event in events {
        if event.source_out <= event.source_in || event.record_out <= event.record_in {
            return Err(malformed(format!(
                "event {} has non-positive range",
                event.number
            )));
        }
        if event.source_out - event.source_in != event.record_out - event.record_in {
            return Err(InterchangeError::UnsupportedSemantics {
                profile: PROFILE,
                reason: format!("event {} implies speed or duration mismatch", event.number),
            });
        }
        if event.reel == "BL" {
            previous_clip = None;
            continue;
        }
        if event.reel.len() > 8 || !event.reel.is_ascii() {
            return Err(malformed(format!(
                "event {} reel is not an 8-character ASCII CMX reel",
                event.number
            )));
        }
        let media_key = InterchangeMediaKey(format!("reel:{}", event.reel));
        let editorial_source = EditorialSourceIdentity::new(
            Some(event.reel.clone()),
            Some(
                SmpteTimecodeReference::new(rate, mode, 0)
                    .map_err(|error| malformed(error.to_string()))?,
            ),
            None,
        )
        .map_err(|error| malformed(error.to_string()))?;
        media.entry(media_key.clone()).or_insert_with(|| InterchangeMediaReference {
            key: media_key.clone(),
            name: Some(event.reel.clone()),
            proposed_locator: None,
            editorial_source: Some(editorial_source.clone()),
            color_space: None,
        });
        let clip_key = format!("cmx-event-{}", event.number);
        if event.edit == 'D' {
            let left = previous_clip.clone().ok_or_else(|| {
                malformed(format!(
                    "event {} dissolve has no preceding picture event",
                    event.number
                ))
            })?;
            if event.dissolve <= 0 || event.dissolve > event.record_out - event.record_in {
                return Err(malformed(format!(
                    "event {} dissolve duration is invalid",
                    event.number
                )));
            }
            transitions.push(InterchangeTransition {
                key: format!("cmx-dissolve-{}", event.number),
                left_clip_key: left,
                right_clip_key: clip_key.clone(),
                start: event.record_in - start_frame,
                duration: event.dissolve,
            });
        }
        clips.push(InterchangeClip {
            key: clip_key.clone(),
            media_key,
            name: Some(event.reel),
            record_start: event.record_in - start_frame,
            duration: event.record_out - event.record_in,
            source_start: event.source_in,
            enabled: true,
            editorial_source: Some(editorial_source),
            color_space: None,
            static_grade: None,
        });
        previous_clip = Some(clip_key);
    }
    let timeline = InterchangeTimeline {
        name: title,
        rate_num: rate.num,
        rate_den: rate.den,
        start_timecode,
        media: media.into_values().collect(),
        tracks: vec![InterchangeTrack {
            name: "V1".to_owned(),
            kind: InterchangeTrackKind::Video,
            enabled: true,
            locked: false,
            clips,
        }],
        transitions,
    };
    validate_timeline(PROFILE, &timeline, limits)?;
    Ok(ParsedFormat { timeline, findings: Vec::new() })
}

pub(crate) fn encode(
    timeline: &InterchangeTimeline,
    limits: InterchangeLimits,
) -> Result<EncodedFormat, InterchangeError> {
    validate_timeline(PROFILE, timeline, limits)?;
    let video_tracks = timeline
        .tracks
        .iter()
        .filter(|track| track.kind == InterchangeTrackKind::Video && track.enabled)
        .collect::<Vec<_>>();
    let track =
        video_tracks
            .first()
            .copied()
            .ok_or_else(|| InterchangeError::UnsupportedSemantics {
                profile: PROFILE,
                reason: "CMX export requires one enabled video Track".to_owned(),
            })?;
    if track.clips.len() > 999 {
        return Err(InterchangeError::LimitExceeded {
            limit_name: "CMX events",
            actual: track.clips.len(),
            maximum: 999,
        });
    }
    let rate = Rational::new(timeline.rate_num, timeline.rate_den);
    validate_cmx_rate(rate)?;
    let start_reference = match timeline.start_timecode {
        Some(reference) => reference,
        None => SmpteTimecodeReference::parse_start(
            rate,
            SmpteCountingMode::NonDropFrame,
            "01:00:00:00",
        )
        .map_err(|error| malformed(error.to_string()))?,
    };
    if start_reference.frame_rate() != rate {
        return Err(malformed("Sequence and timecode rates differ"));
    }
    let contract = SmpteDisplayTimecodeContract::new(rate, start_reference.mode(), 0)
        .map_err(|error| malformed(error.to_string()))?;
    let media = timeline
        .media
        .iter()
        .map(|reference| (&reference.key, reference))
        .collect::<HashMap<_, _>>();
    let transitions = timeline
        .transitions
        .iter()
        .map(|transition| (transition.right_clip_key.as_str(), transition))
        .collect::<HashMap<_, _>>();
    let mut findings = Vec::new();
    if video_tracks.len() > 1
        || timeline
            .tracks
            .iter()
            .any(|track| track.kind == InterchangeTrackKind::Audio && !track.clips.is_empty())
    {
        findings.push(finding(
            "CMX_MULTITRACK_OMITTED",
            InterchangeFindingSeverity::Warning,
            InterchangeDisposition::Omitted,
            "timeline.tracks",
            None,
        ));
    }
    if timeline.start_timecode.is_none() {
        findings.push(finding(
            "SOURCE_TIMECODE_SYNTHESIZED",
            InterchangeFindingSeverity::Warning,
            InterchangeDisposition::Synthesized,
            "sequence.timecode",
            None,
        ));
    }
    let mut reel_by_media = HashMap::new();
    let mut used_reels = HashMap::<String, InterchangeMediaKey>::new();
    for clip in &track.clips {
        let reference = media
            .get(&clip.media_key)
            .ok_or_else(|| malformed(format!("missing media '{}'", clip.media_key.0)))?;
        let original = clip
            .editorial_source
            .as_ref()
            .and_then(|identity| identity.reel_name())
            .or_else(|| {
                reference.editorial_source.as_ref().and_then(|identity| identity.reel_name())
            })
            .unwrap_or(reference.name.as_deref().unwrap_or("AX"));
        let reel = cmx_reel(original);
        if let Some(existing) = used_reels.insert(reel.clone(), clip.media_key.clone())
            && existing != clip.media_key
        {
            return Err(InterchangeError::UnsupportedSemantics {
                profile: PROFILE,
                reason: format!("reel truncation collision on '{reel}'"),
            });
        }
        if reel != original {
            findings.push(finding(
                "REEL_TRUNCATED_OR_NORMALIZED",
                InterchangeFindingSeverity::Warning,
                InterchangeDisposition::Normalized,
                "media.reel",
                Some(clip.key.clone()),
            ));
        }
        reel_by_media.insert(clip.media_key.clone(), reel);
    }
    let mut output = format!(
        "TITLE: {}\r\nFCM: {}\r\n\r\n",
        timeline.name,
        if start_reference.mode() == SmpteCountingMode::DropFrame {
            "DROP FRAME"
        } else {
            "NON-DROP FRAME"
        }
    );
    let label = |actual: i64| -> Result<String, InterchangeError> {
        contract
            .timecode_at_frame(actual)
            .map(|value| value.label())
            .map_err(|error| malformed(error.to_string()))
    };
    for (index, clip) in track.clips.iter().enumerate() {
        let event = index + 1;
        let reel = reel_by_media
            .get(&clip.media_key)
            .ok_or_else(|| malformed("missing reel projection"))?;
        let source_out = clip
            .source_start
            .checked_add(clip.duration)
            .ok_or_else(|| malformed("source range overflow"))?;
        let record_in = start_reference
            .start_frame()
            .checked_add(clip.record_start)
            .ok_or_else(|| malformed("record range overflow"))?;
        let record_out = record_in
            .checked_add(clip.duration)
            .ok_or_else(|| malformed("record range overflow"))?;
        if let Some(transition) = transitions.get(clip.key.as_str()) {
            output.push_str(&format!("{event:03}  {reel:<8} V     D {duration:03} {src_in} {src_out} {rec_in} {rec_out}\r\n", duration = transition.duration, src_in = label(clip.source_start)?, src_out = label(source_out)?, rec_in = label(record_in)?, rec_out = label(record_out)?));
        } else {
            output.push_str(&format!(
                "{event:03}  {reel:<8} V     C        {src_in} {src_out} {rec_in} {rec_out}\r\n",
                src_in = label(clip.source_start)?,
                src_out = label(source_out)?,
                rec_in = label(record_in)?,
                rec_out = label(record_out)?
            ));
        }
        output.push_str(&format!(
            "* FROM CLIP NAME: {}\r\n",
            clip.name.as_deref().unwrap_or(&clip.key)
        ));
    }
    let bytes = output.into_bytes();
    if bytes.len() > limits.max_bytes {
        return Err(InterchangeError::LimitExceeded {
            limit_name: "bytes",
            actual: bytes.len(),
            maximum: limits.max_bytes,
        });
    }
    Ok(EncodedFormat { bytes, findings })
}

fn cmx_reel(value: &str) -> String {
    let projected = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .take(8)
        .collect::<String>();
    if projected.is_empty() {
        "AX".to_owned()
    } else {
        projected
    }
}

fn validate_cmx_rate(rate: Rational) -> Result<(), InterchangeError> {
    let admitted = [
        Rational::FPS_23976,
        Rational::FPS_24,
        Rational::FPS_25,
        Rational::FPS_2997,
        Rational::FPS_30,
    ];
    if admitted.contains(&rate) {
        Ok(())
    } else {
        Err(InterchangeError::UnsupportedSemantics {
            profile: PROFILE,
            reason: format!("frame rate {rate} is outside the CMX profile"),
        })
    }
}

fn malformed(reason: impl Into<String>) -> InterchangeError {
    InterchangeError::MalformedArtifact { profile: PROFILE, reason: reason.into() }
}
