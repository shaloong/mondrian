//! Native OpenTimelineIO JSON Adapter (pinned declared subset).

use super::{
    finding, validate_timeline, EncodedFormat, InterchangeClip, InterchangeTimeline,
    InterchangeTrack, InterchangeTransition, ParsedFormat,
};
use crate::{
    InterchangeDisposition, InterchangeError, InterchangeFindingSeverity, InterchangeFormatProfile,
    InterchangeLimits, InterchangeMediaKey, InterchangeMediaReference, InterchangeTrackKind,
};
use serde_json::{json, Value};
use std::collections::HashMap;

const PROFILE: InterchangeFormatProfile = InterchangeFormatProfile::OtioJsonV1;

pub(crate) fn parse(
    bytes: &[u8],
    limits: InterchangeLimits,
) -> Result<ParsedFormat, InterchangeError> {
    let root: Value = serde_json::from_slice(bytes).map_err(|error| {
        InterchangeError::MalformedArtifact { profile: PROFILE, reason: error.to_string() }
    })?;
    validate_json_shape(&root, 0, limits)?;
    require_schema(&root, "Timeline.")?;
    let name = string_field(&root, "name").unwrap_or("Untitled OTIO").to_owned();
    let metadata = root.get("metadata").and_then(Value::as_object);
    let mondrian = metadata.and_then(|value| value.get("mondrian")).and_then(Value::as_object);
    let (rate_num, rate_den) = match mondrian {
        Some(metadata) => (
            i64_field_object(metadata, "rate_num")?,
            i64_field_object(metadata, "rate_den")?,
        ),
        None => {
            let rate = infer_otio_rate(&root)?;
            (rate, 1)
        }
    };
    let start_timecode = mondrian
        .and_then(|metadata| metadata.get("start_timecode"))
        .filter(|value| !value.is_null())
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|error| malformed(error.to_string()))?;
    let stack = root.get("tracks").ok_or_else(|| malformed("Timeline.tracks is required"))?;
    require_schema(stack, "Stack.")?;
    let track_values = array_field(stack, "children")?;
    let mut media_by_key = HashMap::<InterchangeMediaKey, InterchangeMediaReference>::new();
    let mut tracks = Vec::with_capacity(track_values.len());
    let mut transitions = Vec::new();
    let mut findings = Vec::new();

    for (track_index, value) in track_values.iter().enumerate() {
        require_schema(value, "Track.")?;
        let kind = match string_field(value, "kind").unwrap_or("Video") {
            "Video" => InterchangeTrackKind::Video,
            "Audio" => InterchangeTrackKind::Audio,
            other => {
                return Err(InterchangeError::UnsupportedSemantics {
                    profile: PROFILE,
                    reason: format!("Track kind '{other}' is outside the declared subset"),
                });
            }
        };
        let mut clips = Vec::new();
        let mut record_cursor = 0_i64;
        let children = array_field(value, "children")?;
        let mut pending_transition: Option<(String, i64, i64, String)> = None;
        let mut previous_clip_key: Option<String> = None;
        for (child_index, child) in children.iter().enumerate() {
            let schema = schema_name(child)?;
            if schema.starts_with("Gap.") {
                let (_, duration) =
                    parse_time_range(child.get("source_range"), rate_num, rate_den)?;
                record_cursor = record_cursor
                    .checked_add(duration)
                    .ok_or_else(|| malformed("record range overflow"))?;
            } else if schema.starts_with("Transition.") {
                let in_offset = parse_rational_time(child.get("in_offset"), rate_num, rate_den)?;
                let out_offset = parse_rational_time(child.get("out_offset"), rate_num, rate_den)?;
                if in_offset < 0 || out_offset < 0 || in_offset + out_offset <= 0 {
                    return Err(malformed(
                        "Transition offsets must be non-negative and non-zero",
                    ));
                }
                let left = previous_clip_key
                    .clone()
                    .ok_or_else(|| malformed("Transition has no left Clip"))?;
                pending_transition = Some((
                    left,
                    in_offset,
                    out_offset,
                    format!("otio-t{track_index}-{child_index}"),
                ));
            } else if schema.starts_with("Clip.") {
                let (source_start, duration) =
                    parse_time_range(child.get("source_range"), rate_num, rate_den)?;
                let clip_key = child
                    .get("metadata")
                    .and_then(|value| value.get("mondrian"))
                    .and_then(|value| value.get("clip_key"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("otio-c{track_index}-{child_index}"));
                let media_ref = child
                    .get("media_reference")
                    .ok_or_else(|| malformed("Clip.media_reference is required"))?;
                let media_schema = schema_name(media_ref)?;
                if !media_schema.starts_with("ExternalReference.")
                    && !media_schema.starts_with("MissingReference.")
                {
                    return Err(InterchangeError::UnsupportedSemantics {
                        profile: PROFILE,
                        reason: format!(
                            "media reference '{media_schema}' is outside the declared subset"
                        ),
                    });
                }
                let target_url = string_field(media_ref, "target_url").map(str::to_owned);
                let media_key = media_ref
                    .get("metadata")
                    .and_then(|value| value.get("mondrian"))
                    .and_then(|value| value.get("media_key"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| target_url.clone())
                    .unwrap_or_else(|| format!("missing-{track_index}-{child_index}"));
                let media_key = InterchangeMediaKey(media_key);
                let editorial_source = media_ref
                    .get("metadata")
                    .and_then(|value| value.get("mondrian"))
                    .and_then(|value| value.get("editorial_source"))
                    .filter(|value| !value.is_null())
                    .map(|value| serde_json::from_value(value.clone()))
                    .transpose()
                    .map_err(|error| malformed(error.to_string()))?;
                let reference = InterchangeMediaReference {
                    key: media_key.clone(),
                    name: string_field(media_ref, "name").map(str::to_owned),
                    proposed_locator: target_url,
                    editorial_source: editorial_source.clone(),
                    color_space: None,
                };
                if let Some(existing) = media_by_key.insert(media_key.clone(), reference.clone())
                    && existing != reference
                {
                    return Err(InterchangeError::ConflictingMediaReference { key: media_key.0 });
                }
                let enabled = child.get("enabled").and_then(Value::as_bool).unwrap_or(true);
                let clip_metadata = child.get("metadata").and_then(|value| value.get("mondrian"));
                let color_space = clip_metadata
                    .and_then(|value| value.get("color_space"))
                    .filter(|value| !value.is_null())
                    .map(|value| serde_json::from_value(value.clone()))
                    .transpose()
                    .map_err(|error| malformed(error.to_string()))?;
                let static_grade = clip_metadata
                    .and_then(|value| value.get("static_grade"))
                    .filter(|value| !value.is_null())
                    .map(|value| serde_json::from_value(value.clone()))
                    .transpose()
                    .map_err(|error| malformed(error.to_string()))?;
                clips.push(InterchangeClip {
                    key: clip_key.clone(),
                    media_key,
                    name: string_field(child, "name").map(str::to_owned),
                    record_start: record_cursor,
                    duration,
                    source_start,
                    enabled,
                    editorial_source,
                    color_space,
                    static_grade,
                });
                if let Some((left, in_offset, out_offset, key)) = pending_transition.take() {
                    transitions.push(InterchangeTransition {
                        key,
                        left_clip_key: left,
                        right_clip_key: clip_key.clone(),
                        start: record_cursor
                            .checked_sub(out_offset)
                            .ok_or_else(|| malformed("Transition precedes zero"))?,
                        duration: in_offset
                            .checked_add(out_offset)
                            .ok_or_else(|| malformed("Transition duration overflow"))?,
                    });
                }
                previous_clip_key = Some(clip_key);
                record_cursor = record_cursor
                    .checked_add(duration)
                    .ok_or_else(|| malformed("record range overflow"))?;
            } else {
                findings.push(finding(
                    "OTIO_SCHEMA_UNSUPPORTED",
                    InterchangeFindingSeverity::Blocker,
                    InterchangeDisposition::Unsupported,
                    "timeline.item",
                    Some(format!("tracks[{track_index}].children[{child_index}]")),
                ));
            }
        }
        if pending_transition.is_some() {
            return Err(malformed("Transition has no right Clip"));
        }
        tracks.push(InterchangeTrack {
            name: string_field(value, "name").unwrap_or("").to_owned(),
            kind,
            enabled: value.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            locked: false,
            clips,
        });
    }

    let timeline = InterchangeTimeline {
        name,
        rate_num,
        rate_den,
        start_timecode,
        media: media_by_key.into_values().collect(),
        tracks,
        transitions,
    };
    validate_timeline(PROFILE, &timeline, limits)?;
    Ok(ParsedFormat { timeline, findings })
}

pub(crate) fn encode(
    timeline: &InterchangeTimeline,
    limits: InterchangeLimits,
) -> Result<EncodedFormat, InterchangeError> {
    validate_timeline(PROFILE, timeline, limits)?;
    let media = timeline
        .media
        .iter()
        .map(|reference| (reference.key.clone(), reference))
        .collect::<HashMap<_, _>>();
    let transitions_by_right = timeline
        .transitions
        .iter()
        .map(|transition| (transition.right_clip_key.as_str(), transition))
        .collect::<HashMap<_, _>>();
    let mut track_values = Vec::with_capacity(timeline.tracks.len());
    for track in &timeline.tracks {
        let mut children = Vec::new();
        let mut cursor = 0_i64;
        for clip in &track.clips {
            if clip.record_start > cursor {
                children.push(json!({
                    "OTIO_SCHEMA": "Gap.1",
                    "source_range": time_range(0, clip.record_start - cursor, timeline),
                    "metadata": {}
                }));
            }
            if let Some(transition) = transitions_by_right.get(clip.key.as_str()) {
                let cut = clip.record_start;
                let out_offset = cut
                    .checked_sub(transition.start)
                    .ok_or_else(|| malformed("Transition start follows cut"))?;
                let in_offset = transition
                    .duration
                    .checked_sub(out_offset)
                    .ok_or_else(|| malformed("Transition offsets are invalid"))?;
                children.push(json!({
                    "OTIO_SCHEMA": "Transition.1",
                    "name": "Cross Dissolve",
                    "transition_type": "SMPTE_Dissolve",
                    "in_offset": rational_time(in_offset, timeline),
                    "out_offset": rational_time(out_offset, timeline),
                    "metadata": {"mondrian": {"transition_key": transition.key}}
                }));
            }
            let reference = media.get(&clip.media_key).ok_or_else(|| {
                malformed(format!("missing media reference '{}'", clip.media_key.0))
            })?;
            let media_schema = if reference.proposed_locator.is_some() {
                "ExternalReference.1"
            } else {
                "MissingReference.1"
            };
            children.push(json!({
                "OTIO_SCHEMA": "Clip.2",
                "name": clip.name,
                "enabled": clip.enabled,
                "source_range": time_range(clip.source_start, clip.duration, timeline),
                "media_reference": {
                    "OTIO_SCHEMA": media_schema,
                    "name": reference.name,
                    "target_url": reference.proposed_locator,
                    "available_range": Value::Null,
                    "metadata": {"mondrian": {
                        "media_key": reference.key.0,
                        "editorial_source": reference.editorial_source
                    }}
                },
                "metadata": {"mondrian": {
                    "clip_key": clip.key,
                    "color_space": clip.color_space,
                    "static_grade": clip.static_grade
                }}
            }));
            cursor = clip
                .record_start
                .checked_add(clip.duration)
                .ok_or_else(|| malformed("record range overflow"))?;
        }
        track_values.push(json!({
            "OTIO_SCHEMA": "Track.1",
            "name": track.name,
            "kind": match track.kind { InterchangeTrackKind::Video => "Video", InterchangeTrackKind::Audio => "Audio" },
            "enabled": track.enabled,
            "children": children,
            "metadata": {"mondrian": {"locked": track.locked}}
        }));
    }
    let root = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": timeline.name,
        "global_start_time": timeline.start_timecode.map(|reference| rational_time(reference.start_frame(), timeline)),
        "tracks": {"OTIO_SCHEMA": "Stack.1", "name": "tracks", "children": track_values, "metadata": {}},
        "metadata": {"mondrian": {
            "profile": PROFILE.as_str(),
            "rate_num": timeline.rate_num,
            "rate_den": timeline.rate_den,
            "start_timecode": timeline.start_timecode
        }}
    });
    let bytes = serde_json::to_vec_pretty(&root)?;
    if bytes.len() > limits.max_bytes {
        return Err(InterchangeError::LimitExceeded {
            limit_name: "bytes",
            actual: bytes.len(),
            maximum: limits.max_bytes,
        });
    }
    Ok(EncodedFormat { bytes, findings: Vec::new() })
}

fn rational_time(value: i64, timeline: &InterchangeTimeline) -> Value {
    json!({"OTIO_SCHEMA": "RationalTime.1", "value": value, "rate": timeline.rate_num as f64 / timeline.rate_den as f64})
}

fn time_range(start: i64, duration: i64, timeline: &InterchangeTimeline) -> Value {
    json!({"OTIO_SCHEMA": "TimeRange.1", "start_time": rational_time(start, timeline), "duration": rational_time(duration, timeline)})
}

fn parse_time_range(
    value: Option<&Value>,
    rate_num: i64,
    rate_den: i64,
) -> Result<(i64, i64), InterchangeError> {
    let value = value.ok_or_else(|| malformed("source_range is required"))?;
    require_schema(value, "TimeRange.")?;
    let start = parse_rational_time(value.get("start_time"), rate_num, rate_den)?;
    let duration = parse_rational_time(value.get("duration"), rate_num, rate_den)?;
    if start < 0 || duration <= 0 {
        return Err(malformed(
            "source range must have non-negative start and positive duration",
        ));
    }
    Ok((start, duration))
}

fn parse_rational_time(
    value: Option<&Value>,
    rate_num: i64,
    rate_den: i64,
) -> Result<i64, InterchangeError> {
    let value = value.ok_or_else(|| malformed("RationalTime is required"))?;
    require_schema(value, "RationalTime.")?;
    let frame = value
        .get("value")
        .and_then(Value::as_i64)
        .ok_or_else(|| malformed("RationalTime.value must be an integer frame"))?;
    let rate = value
        .get("rate")
        .and_then(Value::as_f64)
        .ok_or_else(|| malformed("RationalTime.rate is required"))?;
    let expected = rate_num as f64 / rate_den as f64;
    if !rate.is_finite() || (rate - expected).abs() > 0.000_001 {
        return Err(malformed("mixed or non-finite RationalTime rate"));
    }
    Ok(frame)
}

fn infer_otio_rate(root: &Value) -> Result<i64, InterchangeError> {
    let rate = root
        .get("global_start_time")
        .and_then(|value| value.get("rate"))
        .and_then(Value::as_f64)
        .ok_or_else(|| malformed("fractional rate requires metadata.mondrian rate_num/rate_den"))?;
    if !rate.is_finite() || rate.fract() != 0.0 || !(1.0..=120.0).contains(&rate) {
        return Err(malformed(
            "OTIO rate must be a supported integer without Mondrian exact-rate metadata",
        ));
    }
    Ok(rate as i64)
}

fn validate_json_shape(
    value: &Value,
    depth: usize,
    limits: InterchangeLimits,
) -> Result<(), InterchangeError> {
    if depth > limits.max_nesting_depth {
        return Err(InterchangeError::LimitExceeded {
            limit_name: "JSON nesting depth",
            actual: depth,
            maximum: limits.max_nesting_depth,
        });
    }
    match value {
        Value::String(text) if text.len() > limits.max_string_bytes => {
            Err(InterchangeError::LimitExceeded {
                limit_name: "string bytes",
                actual: text.len(),
                maximum: limits.max_string_bytes,
            })
        }
        Value::Array(values) => {
            for value in values {
                validate_json_shape(value, depth + 1, limits)?;
            }
            Ok(())
        }
        Value::Object(values) => {
            for (key, value) in values {
                if key.len() > limits.max_string_bytes {
                    return Err(InterchangeError::LimitExceeded {
                        limit_name: "string bytes",
                        actual: key.len(),
                        maximum: limits.max_string_bytes,
                    });
                }
                validate_json_shape(value, depth + 1, limits)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn require_schema(value: &Value, prefix: &str) -> Result<(), InterchangeError> {
    let schema = schema_name(value)?;
    if schema.starts_with(prefix) {
        Ok(())
    } else {
        Err(malformed(format!("expected {prefix} schema, got {schema}")))
    }
}
fn schema_name(value: &Value) -> Result<&str, InterchangeError> {
    string_field(value, "OTIO_SCHEMA").ok_or_else(|| malformed("OTIO_SCHEMA is required"))
}
fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}
fn array_field<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>, InterchangeError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| malformed(format!("{key} must be an array")))
}
fn i64_field_object(
    value: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<i64, InterchangeError> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| malformed(format!("metadata.mondrian.{key} is required")))
}
fn malformed(reason: impl Into<String>) -> InterchangeError {
    InterchangeError::MalformedArtifact { profile: PROFILE, reason: reason.into() }
}
