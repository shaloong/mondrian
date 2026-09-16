//! Final Cut Pro 7 `xmeml version=5` Sequence Adapter.

use super::{
    finding, validate_timeline, EncodedFormat, InterchangeClip, InterchangeTimeline,
    InterchangeTrack, InterchangeTransition, ParsedFormat,
};
use crate::{
    InterchangeDisposition, InterchangeError, InterchangeFindingSeverity, InterchangeFormatProfile,
    InterchangeLimits, InterchangeMediaKey, InterchangeMediaReference, InterchangeTrackKind,
};
use mondrian_core::timeline_data::EditorialSourceIdentity;
use mondrian_core::{Rational, SmpteCountingMode, SmpteTimecodeReference};
use roxmltree::{Document, Node};
use std::collections::HashMap;

const PROFILE: InterchangeFormatProfile = InterchangeFormatProfile::Fcp7XmlV5;

#[derive(Debug, Clone, Default)]
struct FileRecord {
    name: Option<String>,
    pathurl: Option<String>,
    reel: Option<String>,
    timecode: Option<String>,
}

pub(crate) fn parse(
    bytes: &[u8],
    limits: InterchangeLimits,
) -> Result<ParsedFormat, InterchangeError> {
    let text = std::str::from_utf8(bytes).map_err(|error| malformed(error.to_string()))?;
    let upper = text.to_ascii_uppercase();
    if upper.contains("<!DOCTYPE") || upper.contains("<!ENTITY") {
        return Err(malformed(
            "DTD/entity declarations are forbidden at runtime",
        ));
    }
    let document = Document::parse(text).map_err(|error| malformed(error.to_string()))?;
    let actual_depth = document
        .descendants()
        .filter(Node::is_element)
        .map(|node| node.ancestors().filter(Node::is_element).count())
        .max()
        .unwrap_or(0);
    if actual_depth > limits.max_nesting_depth {
        return Err(InterchangeError::LimitExceeded {
            limit_name: "XML nesting depth",
            actual: actual_depth,
            maximum: limits.max_nesting_depth,
        });
    }
    let root = document.root_element();
    if root.tag_name().name() != "xmeml" || root.attribute("version") != Some("5") {
        return Err(malformed("root must be xmeml version=5"));
    }
    let sequences = root
        .descendants()
        .filter(|node| node.has_tag_name("sequence"))
        .collect::<Vec<_>>();
    if sequences.len() != 1 {
        return Err(malformed("profile requires exactly one top-level sequence"));
    }
    let sequence = sequences[0];
    let rate = parse_rate(child(sequence, "rate")?)?;
    let start_timecode = child_optional(sequence, "timecode")
        .map(|node| parse_timecode(node, rate))
        .transpose()?;
    let mut files = HashMap::<String, FileRecord>::new();
    for file in sequence.descendants().filter(|node| node.has_tag_name("file")) {
        let Some(id) = file.attribute("id") else {
            continue;
        };
        let entry = files.entry(id.to_owned()).or_default();
        if let Some(value) = child_text_optional(file, "name") {
            entry.name = Some(value.to_owned());
        }
        if let Some(value) = child_text_optional(file, "pathurl") {
            entry.pathurl = Some(value.to_owned());
        }
        if let Some(value) = file
            .descendants()
            .find(|node| node.has_tag_name("reel"))
            .and_then(|node| child_text_optional(node, "name"))
        {
            entry.reel = Some(value.to_owned());
        }
        if let Some(value) =
            child_optional(file, "timecode").and_then(|node| child_text_optional(node, "string"))
        {
            entry.timecode = Some(value.to_owned());
        }
    }
    let mut media_map = HashMap::<InterchangeMediaKey, InterchangeMediaReference>::new();
    let mut tracks = Vec::new();
    let mut transitions = Vec::new();
    let media_node = child(sequence, "media")?;
    for (container_name, kind) in [
        ("video", InterchangeTrackKind::Video),
        ("audio", InterchangeTrackKind::Audio),
    ] {
        let Some(container) = child_optional(media_node, container_name) else {
            continue;
        };
        for (track_index, track_node) in
            container.children().filter(|node| node.has_tag_name("track")).enumerate()
        {
            let mut clips = Vec::new();
            let mut previous_clip: Option<String> = None;
            let mut pending_transition: Option<(String, i64, i64, String)> = None;
            for (item_index, item) in track_node.children().filter(Node::is_element).enumerate() {
                if item.has_tag_name("transitionitem") {
                    let start = parse_i64_text(item, "start")?;
                    let end = parse_i64_text(item, "end")?;
                    let alignment = child_text_optional(item, "alignment").unwrap_or("center");
                    if alignment != "center" && alignment != "start" && alignment != "end" {
                        return Err(InterchangeError::UnsupportedSemantics {
                            profile: PROFILE,
                            reason: format!(
                                "transition alignment '{alignment}' is outside the subset"
                            ),
                        });
                    }
                    let duration = end
                        .checked_sub(start)
                        .ok_or_else(|| malformed("transition range overflow"))?;
                    if duration <= 0 {
                        return Err(malformed("transition duration must be positive"));
                    }
                    let left = previous_clip
                        .clone()
                        .ok_or_else(|| malformed("transition has no left clipitem"))?;
                    pending_transition = Some((
                        left,
                        start,
                        duration,
                        format!("xml-transition-{container_name}-{track_index}-{item_index}"),
                    ));
                    continue;
                }
                if !item.has_tag_name("clipitem") {
                    continue;
                }
                let id = item.attribute("id").map(str::to_owned).unwrap_or_else(|| {
                    format!("xml-clip-{container_name}-{track_index}-{item_index}")
                });
                let start = parse_i64_text(item, "start")?;
                let end = parse_i64_text(item, "end")?;
                let source_in = parse_i64_text(item, "in")?;
                if start < 0 || end <= start || source_in < 0 {
                    return Err(malformed(format!(
                        "clipitem '{id}' has invalid half-open range"
                    )));
                }
                let file_node = child(item, "file")?;
                let file_id = file_node
                    .attribute("id")
                    .ok_or_else(|| malformed(format!("clipitem '{id}' file id is required")))?;
                let file = files.get(file_id).cloned().unwrap_or_default();
                let key = InterchangeMediaKey(format!("fcp-file:{file_id}"));
                let source_tc = file
                    .timecode
                    .as_deref()
                    .map(|label| {
                        SmpteTimecodeReference::parse_start(rate, mode_for_label(label), label)
                            .map_err(|error| malformed(error.to_string()))
                    })
                    .transpose()?;
                let editorial_source = EditorialSourceIdentity::new(
                    file.reel.clone(),
                    source_tc,
                    Some(file_id.to_owned()),
                )
                .map_err(|error| malformed(error.to_string()))?;
                media_map.entry(key.clone()).or_insert_with(|| InterchangeMediaReference {
                    key: key.clone(),
                    name: file.name.clone(),
                    proposed_locator: file.pathurl.clone(),
                    editorial_source: Some(editorial_source.clone()),
                    color_space: None,
                });
                clips.push(InterchangeClip {
                    key: id.clone(),
                    media_key: key,
                    name: child_text_optional(item, "name").map(str::to_owned),
                    record_start: start,
                    duration: end - start,
                    source_start: source_in,
                    enabled: child_text_optional(item, "enabled") != Some("FALSE"),
                    editorial_source: Some(editorial_source),
                    color_space: None,
                    static_grade: None,
                });
                if let Some((left, transition_start, duration, key)) = pending_transition.take() {
                    transitions.push(InterchangeTransition {
                        key,
                        left_clip_key: left,
                        right_clip_key: id.clone(),
                        start: transition_start.max(0),
                        duration,
                    });
                }
                previous_clip = Some(id);
            }
            if pending_transition.is_some() {
                return Err(malformed("transition has no right clipitem"));
            }
            tracks.push(InterchangeTrack {
                name: child_text_optional(track_node, "name").unwrap_or("").to_owned(),
                kind,
                enabled: child_text_optional(track_node, "enabled") != Some("FALSE"),
                locked: child_text_optional(track_node, "locked") == Some("TRUE"),
                clips,
            });
        }
    }
    let timeline = InterchangeTimeline {
        name: child_text_optional(sequence, "name").unwrap_or("Untitled XML").to_owned(),
        rate_num: rate.num,
        rate_den: rate.den,
        start_timecode,
        media: media_map.into_values().collect(),
        tracks,
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
    let rate = Rational::new(timeline.rate_num, timeline.rate_den);
    let (timebase, ntsc) = encode_rate(rate)?;
    let media = timeline
        .media
        .iter()
        .map(|reference| (&reference.key, reference))
        .collect::<HashMap<_, _>>();
    let file_ids = timeline
        .media
        .iter()
        .enumerate()
        .map(|(index, reference)| (reference.key.clone(), format!("file-{}", index + 1)))
        .collect::<HashMap<_, _>>();
    let transitions_by_right = timeline
        .transitions
        .iter()
        .map(|transition| (transition.right_clip_key.as_str(), transition))
        .collect::<HashMap<_, _>>();
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xmeml version=\"5\">\n  <sequence id=\"sequence-1\">\n");
    push_element(&mut xml, 4, "name", &timeline.name);
    push_rate(&mut xml, 4, timebase, ntsc);
    if let Some(reference) = timeline.start_timecode {
        push_timecode(&mut xml, 4, reference, timebase, ntsc)?;
    }
    xml.push_str("    <media>\n");
    for (container, kind) in [
        ("video", InterchangeTrackKind::Video),
        ("audio", InterchangeTrackKind::Audio),
    ] {
        xml.push_str(&format!("      <{container}>\n"));
        for (track_index, track) in
            timeline.tracks.iter().filter(|track| track.kind == kind).enumerate()
        {
            xml.push_str("        <track>\n");
            push_element(&mut xml, 10, "name", &track.name);
            push_element(
                &mut xml,
                10,
                "enabled",
                if track.enabled { "TRUE" } else { "FALSE" },
            );
            push_element(
                &mut xml,
                10,
                "locked",
                if track.locked { "TRUE" } else { "FALSE" },
            );
            for (clip_index, clip) in track.clips.iter().enumerate() {
                if let Some(transition) = transitions_by_right.get(clip.key.as_str()) {
                    xml.push_str(&format!(
                        "          <transitionitem id=\"transition-{track_index}-{clip_index}\">\n"
                    ));
                    push_element(&mut xml, 12, "start", &transition.start.to_string());
                    push_element(
                        &mut xml,
                        12,
                        "end",
                        &(transition.start + transition.duration).to_string(),
                    );
                    push_element(&mut xml, 12, "alignment", "center");
                    xml.push_str("            <effect><name>Cross Dissolve</name><effectid>Cross Dissolve</effectid><effecttype>transition</effecttype><mediatype>video</mediatype></effect>\n");
                    xml.push_str("          </transitionitem>\n");
                }
                let reference = media
                    .get(&clip.media_key)
                    .ok_or_else(|| malformed(format!("missing media '{}'", clip.media_key.0)))?;
                let file_id = file_ids
                    .get(&clip.media_key)
                    .ok_or_else(|| malformed("missing file identity projection"))?;
                xml.push_str(&format!(
                    "          <clipitem id=\"{}\">\n",
                    xml_escape(&clip.key)
                ));
                push_element(
                    &mut xml,
                    12,
                    "name",
                    clip.name.as_deref().or(reference.name.as_deref()).unwrap_or(&clip.key),
                );
                push_element(
                    &mut xml,
                    12,
                    "enabled",
                    if clip.enabled { "TRUE" } else { "FALSE" },
                );
                push_element(&mut xml, 12, "start", &clip.record_start.to_string());
                push_element(
                    &mut xml,
                    12,
                    "end",
                    &(clip.record_start + clip.duration).to_string(),
                );
                push_element(&mut xml, 12, "in", &clip.source_start.to_string());
                push_element(
                    &mut xml,
                    12,
                    "out",
                    &(clip.source_start + clip.duration).to_string(),
                );
                push_rate(&mut xml, 12, timebase, ntsc);
                xml.push_str(&format!("            <file id=\"{file_id}\">\n"));
                push_element(
                    &mut xml,
                    14,
                    "name",
                    reference.name.as_deref().unwrap_or(&reference.key.0),
                );
                if let Some(locator) = &reference.proposed_locator {
                    push_element(&mut xml, 14, "pathurl", locator);
                }
                if let Some(identity) =
                    clip.editorial_source.as_ref().or(reference.editorial_source.as_ref())
                {
                    if let Some(reel) = identity.reel_name() {
                        xml.push_str("              <reel>\n");
                        push_element(&mut xml, 16, "name", reel);
                        xml.push_str("              </reel>\n");
                    }
                    if let Some(source_tc) = identity.source_timecode() {
                        push_timecode(&mut xml, 14, source_tc, timebase, ntsc)?;
                    }
                }
                xml.push_str("            </file>\n          </clipitem>\n");
            }
            xml.push_str("        </track>\n");
        }
        xml.push_str(&format!("      </{container}>\n"));
    }
    xml.push_str("    </media>\n  </sequence>\n</xmeml>\n");
    let bytes = xml.into_bytes();
    if bytes.len() > limits.max_bytes {
        return Err(InterchangeError::LimitExceeded {
            limit_name: "bytes",
            actual: bytes.len(),
            maximum: limits.max_bytes,
        });
    }
    Ok(EncodedFormat {
        bytes,
        findings: vec![finding(
            "FCP7_DTD_RUNTIME_SKIPPED",
            InterchangeFindingSeverity::Info,
            InterchangeDisposition::Preserved,
            "validation",
            None,
        )],
    })
}

fn parse_rate(node: Node<'_, '_>) -> Result<Rational, InterchangeError> {
    let timebase = child_text(node, "timebase")?
        .parse::<i64>()
        .map_err(|_| malformed("rate.timebase is invalid"))?;
    let ntsc = child_text_optional(node, "ntsc").unwrap_or("FALSE") == "TRUE";
    let rate = if ntsc {
        Rational::new(timebase * 1_000, 1_001)
    } else {
        Rational::new(timebase, 1)
    };
    encode_rate(rate)?;
    Ok(rate)
}

fn encode_rate(rate: Rational) -> Result<(i64, bool), InterchangeError> {
    match (rate.num, rate.den) {
        (24_000, 1_001) => Ok((24, true)),
        (30_000, 1_001) => Ok((30, true)),
        (60_000, 1_001) => Ok((60, true)),
        (24, 1) | (25, 1) | (30, 1) | (50, 1) | (60, 1) => Ok((rate.num, false)),
        _ => Err(InterchangeError::UnsupportedSemantics {
            profile: PROFILE,
            reason: format!("rate {rate} cannot be represented by xmeml timebase/ntsc"),
        }),
    }
}

fn parse_timecode(
    node: Node<'_, '_>,
    rate: Rational,
) -> Result<SmpteTimecodeReference, InterchangeError> {
    let label = child_text(node, "string")?;
    SmpteTimecodeReference::parse_start(rate, mode_for_label(label), label)
        .map_err(|error| malformed(error.to_string()))
}
fn mode_for_label(label: &str) -> SmpteCountingMode {
    if label.contains(';') {
        SmpteCountingMode::DropFrame
    } else {
        SmpteCountingMode::NonDropFrame
    }
}
fn push_rate(xml: &mut String, indent: usize, timebase: i64, ntsc: bool) {
    xml.push_str(&format!(
        "{}<rate><timebase>{timebase}</timebase><ntsc>{}</ntsc></rate>\n",
        " ".repeat(indent),
        if ntsc { "TRUE" } else { "FALSE" }
    ));
}
fn push_timecode(
    xml: &mut String,
    indent: usize,
    reference: SmpteTimecodeReference,
    timebase: i64,
    ntsc: bool,
) -> Result<(), InterchangeError> {
    let label = reference
        .timecode_at_media_frame(0)
        .map_err(|error| malformed(error.to_string()))?
        .label();
    xml.push_str(&format!("{}<timecode>\n", " ".repeat(indent)));
    push_rate(xml, indent + 2, timebase, ntsc);
    push_element(xml, indent + 2, "string", &label);
    push_element(
        xml,
        indent + 2,
        "frame",
        &reference.start_frame().to_string(),
    );
    xml.push_str(&format!("{}</timecode>\n", " ".repeat(indent)));
    Ok(())
}
fn push_element(xml: &mut String, indent: usize, name: &str, value: &str) {
    xml.push_str(&format!(
        "{}<{name}>{}</{name}>\n",
        " ".repeat(indent),
        xml_escape(value)
    ));
}
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn child<'a>(node: Node<'a, 'a>, name: &str) -> Result<Node<'a, 'a>, InterchangeError> {
    child_optional(node, name)
        .ok_or_else(|| malformed(format!("{}.{name} is required", node.tag_name().name())))
}
fn child_optional<'a>(node: Node<'a, 'a>, name: &str) -> Option<Node<'a, 'a>> {
    node.children().find(|child| child.has_tag_name(name))
}
fn child_text<'a>(node: Node<'a, 'a>, name: &str) -> Result<&'a str, InterchangeError> {
    child_text_optional(node, name).ok_or_else(|| {
        malformed(format!(
            "{}.{name} text is required",
            node.tag_name().name()
        ))
    })
}
fn child_text_optional<'a>(node: Node<'a, 'a>, name: &str) -> Option<&'a str> {
    child_optional(node, name).and_then(|child| child.text())
}
fn parse_i64_text(node: Node<'_, '_>, name: &str) -> Result<i64, InterchangeError> {
    child_text(node, name)?.parse::<i64>().map_err(|_| {
        malformed(format!(
            "{}.{name} is not an integer",
            node.tag_name().name()
        ))
    })
}
fn malformed(reason: impl Into<String>) -> InterchangeError {
    InterchangeError::MalformedArtifact { profile: PROFILE, reason: reason.into() }
}
