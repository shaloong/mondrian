//! Two-phase import: inspect untrusted bytes, then materialize with strong
//! product-owned Asset bindings. Neither phase mutates Project author state.

use crate::{formats, *};
use mondrian_core::{
    automation::{ParameterResourceReference, PropertyValue},
    effect_data::EffectType,
    AuthoringList, ClipId, FramePosition, GradeDefinition, GradeGraph, GradeGraphNode,
    GradeGraphNodeId, GradeGraphNodeKind, Rational, SmpteCountingMode, TimelineDisplaySettings,
    TimelineTime, TimelineTimeRange,
};
use mondrian_timeline::{AudioProgram, Clip, Sequence, Track, VideoTransition};
use std::collections::HashMap;

/// Immutable inspection request for one foreign artifact.
#[derive(Debug, Clone)]
pub struct InterchangeImportRequest {
    /// Exact qualified native profile.
    pub profile: InterchangeFormatProfile,
    /// Untrusted native artifact bytes.
    pub bytes: Vec<u8>,
    /// Required for CMX 3600, whose text does not carry a trustworthy exact
    /// rate. Ignored when the selected native format owns an exact rate.
    pub explicit_frame_rate: Option<Rational>,
    /// Closed resource limits for parsing.
    pub limits: InterchangeLimits,
}

/// Parsed, bounded import candidate. Its internal model is not an alternate
/// public timeline; callers can only inspect media requirements and report.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedInterchangeImport {
    profile: InterchangeFormatProfile,
    timeline: formats::InterchangeTimeline,
    media: Vec<InterchangeMediaReference>,
    report: InterchangeConformanceReport,
}

impl PreparedInterchangeImport {
    /// Selected qualified profile.
    pub const fn profile(&self) -> InterchangeFormatProfile {
        self.profile
    }
    /// Media references that require explicit Asset Library binding.
    pub fn media_references(&self) -> &[InterchangeMediaReference] {
        &self.media
    }
    /// Complete preservation/loss evidence from native inspection.
    pub const fn report(&self) -> &InterchangeConformanceReport {
        &self.report
    }
}

/// Stable mapping from foreign item identities to newly constructed author IDs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterchangeEntityMap {
    /// Newly materialized canonical Sequence identity.
    pub sequence_id: mondrian_core::SequenceId,
    /// Foreign Track address to canonical Track identity mappings.
    pub tracks: Vec<(String, mondrian_core::TrackId)>,
    /// Foreign Clip address to canonical Clip identity mappings.
    pub clips: Vec<(String, ClipId)>,
    /// Foreign Transition address to canonical Transition identity mappings.
    pub transitions: Vec<(String, mondrian_core::VideoTransitionId)>,
}

/// Detached complete Sequence plus evidence ready for one App-owned authoring
/// transaction.
#[derive(Debug, Clone)]
pub struct InterchangeImportCandidate {
    /// Detached canonical Sequence ready for author validation and commit.
    pub sequence: Sequence,
    /// Stable mapping suitable for selection and diagnostic presentation.
    pub entity_map: InterchangeEntityMap,
    /// Complete inspection conformance and loss evidence.
    pub report: InterchangeConformanceReport,
}

/// Parse and inspect one native artifact without resolving locators or changing
/// any Project/Asset state.
pub fn inspect_import(
    request: InterchangeImportRequest,
) -> Result<PreparedInterchangeImport, InterchangeError> {
    request.limits.validate()?;
    if request.bytes.len() > request.limits.max_bytes {
        return Err(InterchangeError::LimitExceeded {
            limit_name: "bytes",
            actual: request.bytes.len(),
            maximum: request.limits.max_bytes,
        });
    }
    let parsed = formats::parse(
        request.profile,
        &request.bytes,
        request.limits,
        request.explicit_frame_rate,
    )?;
    build_prepared_import(request.profile, &request.bytes, parsed)
}

/// Inspect binary AAF through an already-qualified isolated helper.
pub fn inspect_aaf_with_toolchain(
    bytes: &[u8],
    limits: InterchangeLimits,
    toolchain: &dyn AafToolchain,
) -> Result<PreparedInterchangeImport, InterchangeError> {
    limits.validate()?;
    toolchain.identity().validate(limits.max_string_bytes)?;
    if bytes.len() > limits.max_bytes {
        return Err(InterchangeError::LimitExceeded {
            limit_name: "bytes",
            actual: bytes.len(),
            maximum: limits.max_bytes,
        });
    }
    toolchain::validate_aaf_binary(bytes)?;
    let bridge = toolchain.decode_aaf(bytes, limits)?;
    let parsed = formats::aaf::parse_helper_output(&bridge, limits)?;
    build_prepared_import(InterchangeFormatProfile::AafEditProtocolV1, bytes, parsed)
}

fn build_prepared_import(
    profile: InterchangeFormatProfile,
    source: &[u8],
    parsed: formats::ParsedFormat,
) -> Result<PreparedInterchangeImport, InterchangeError> {
    let semantic = serde_json::to_vec(&parsed.timeline)?;
    let report = InterchangeConformanceReport::new(
        profile,
        InterchangeDirection::Import,
        source,
        &semantic,
        parsed.findings,
    );
    Ok(PreparedInterchangeImport {
        profile,
        media: parsed.timeline.media.clone(),
        timeline: parsed.timeline,
        report,
    })
}

/// Materialize a detached Sequence after the product has resolved every media
/// reference to an existing strong Asset ID.
pub fn materialize_import(
    prepared: &PreparedInterchangeImport,
    bindings: &[InterchangeMediaBinding],
    loss_policy: InterchangeLossPolicy,
) -> Result<InterchangeImportCandidate, InterchangeError> {
    enforce_report_policy(&prepared.report, loss_policy)?;
    let mut binding_map = HashMap::new();
    for binding in bindings {
        if binding_map.insert(binding.key.clone(), binding.asset_id).is_some() {
            return Err(InterchangeError::DuplicateMediaBinding { key: binding.key.0.clone() });
        }
    }
    for reference in &prepared.media {
        if !binding_map.contains_key(&reference.key) {
            return Err(InterchangeError::MissingMediaBinding { key: reference.key.0.clone() });
        }
    }
    let timeline = &prepared.timeline;
    let rate = Rational::new(timeline.rate_num, timeline.rate_den);
    let settings = mondrian_timeline::SequenceSettings {
        frame_rate: rate,
        timeline_display: match timeline.start_timecode {
            Some(reference) => {
                TimelineDisplaySettings::timecode(reference.mode(), reference.start_frame())
            }
            None => TimelineDisplaySettings::timecode(SmpteCountingMode::NonDropFrame, 0),
        },
        ..Default::default()
    };
    let mut sequence = Sequence::with_settings(timeline.name.clone(), settings)?;
    let mut video_tracks = Vec::new();
    let mut audio_tracks = Vec::new();
    let mut entity_map = InterchangeEntityMap {
        sequence_id: sequence.id,
        tracks: Vec::new(),
        clips: Vec::new(),
        transitions: Vec::new(),
    };
    let mut clip_ids = HashMap::<String, ClipId>::new();
    for (track_index, source_track) in timeline.tracks.iter().enumerate() {
        let mut track = match source_track.kind {
            InterchangeTrackKind::Video => Track::new_video(&source_track.name),
            InterchangeTrackKind::Audio => Track::new_audio(&source_track.name),
        };
        track.is_visible = source_track.enabled;
        track.is_muted = !source_track.enabled;
        for source_clip in &source_track.clips {
            let asset_id = binding_map.get(&source_clip.media_key).copied().ok_or_else(|| {
                InterchangeError::MissingMediaBinding { key: source_clip.media_key.0.clone() }
            })?;
            let position = frame_time(source_clip.record_start, rate)?;
            let duration = frame_time(source_clip.duration, rate)?;
            let mut clip = Clip::new(asset_id, position, duration)?;
            clip.set_source_origin(frame_time(source_clip.source_start, rate)?)?;
            clip.is_disabled = !source_clip.enabled;
            clip.label = source_clip.name.clone();
            if let Some(interpretation) = clip.media_interpretation_mut() {
                interpretation.editorial_source = source_clip.editorial_source.clone();
                interpretation.color_space_override = source_clip.color_space;
            }
            if let Some(grade) = &source_clip.static_grade {
                clip.grade = Some(materialize_static_grade(&mut sequence, grade)?);
            }
            let clip_id = clip.id;
            track.add_clip(clip)?;
            if clip_ids.insert(source_clip.key.clone(), clip_id).is_some() {
                return Err(InterchangeError::MalformedArtifact {
                    profile: prepared.profile,
                    reason: format!("duplicate Clip key '{}'", source_clip.key),
                });
            }
            entity_map.clips.push((source_clip.key.clone(), clip_id));
        }
        track.is_locked = source_track.locked;
        entity_map.tracks.push((format!("track-{track_index}"), track.id));
        match source_track.kind {
            InterchangeTrackKind::Video => video_tracks.push(track),
            InterchangeTrackKind::Audio => audio_tracks.push(track),
        }
    }
    sequence.video_tracks = AuthoringList::from(video_tracks);
    sequence.audio_tracks = AuthoringList::from(audio_tracks);
    sequence.audio_program =
        AudioProgram::for_tracks(sequence.audio_tracks.iter().map(|track| track.id));
    for source_transition in &timeline.transitions {
        let left = clip_ids.get(&source_transition.left_clip_key).copied().ok_or_else(|| {
            InterchangeError::MalformedArtifact {
                profile: prepared.profile,
                reason: format!(
                    "missing Transition left '{}': internal validation drift",
                    source_transition.left_clip_key
                ),
            }
        })?;
        let right = clip_ids.get(&source_transition.right_clip_key).copied().ok_or_else(|| {
            InterchangeError::MalformedArtifact {
                profile: prepared.profile,
                reason: format!(
                    "missing Transition right '{}': internal validation drift",
                    source_transition.right_clip_key
                ),
            }
        })?;
        let range = TimelineTimeRange::new(
            frame_time(source_transition.start, rate)?,
            frame_time(source_transition.duration, rate)?,
        )?;
        let transition = VideoTransition::cross_dissolve(left, right, range);
        entity_map.transitions.push((source_transition.key.clone(), transition.id));
        sequence.video_transitions.push(transition);
    }
    // The App performs Project color-environment validation and exactly one
    // authoring transaction when it appends this detached candidate.
    Ok(InterchangeImportCandidate {
        sequence,
        entity_map,
        report: prepared.report.clone(),
    })
}

fn frame_time(frame: i64, rate: Rational) -> Result<TimelineTime, InterchangeError> {
    Ok(TimelineTime::from_frame_position(FramePosition::new(
        frame,
        Rational::new(rate.den, rate.num),
    ))?)
}

fn materialize_static_grade(
    sequence: &mut Sequence,
    grade: &formats::InterchangeStaticGrade,
) -> Result<mondrian_core::GradeDefinitionId, InterchangeError> {
    let mut graph = GradeGraph::identity();
    let mut input = graph.output;
    if let Some(cdl) = &grade.cdl {
        let mut effect = mondrian_effects::instantiate_effect_node(EffectType::AscCdl)
            .map_err(effect_instantiation_error)?;
        for (name, value) in [
            ("slope", cdl.slope),
            ("offset", cdl.offset),
            ("power", cdl.power),
        ] {
            effect.set_static_value_by_parameter(
                &EffectType::AscCdl.parameter_id(name).map_err(|error| {
                    InterchangeError::UnsupportedSemantics {
                        profile: InterchangeFormatProfile::OtioJsonV1,
                        reason: error.to_string(),
                    }
                })?,
                PropertyValue::Vec3(glam::Vec3::from_array(value)),
            )?;
        }
        effect.set_static_value_by_parameter(
            &EffectType::AscCdl.parameter_id("saturation").map_err(|error| {
                InterchangeError::UnsupportedSemantics {
                    profile: InterchangeFormatProfile::OtioJsonV1,
                    reason: error.to_string(),
                }
            })?,
            PropertyValue::Float(cdl.saturation),
        )?;
        input = append_grade_effect(&mut graph, input, effect);
    }
    if let Some(lut) = &grade.lut {
        let mut effect = mondrian_effects::instantiate_effect_node(EffectType::Lut3D)
            .map_err(effect_instantiation_error)?;
        let resource = if lut.uri.contains("://") {
            ParameterResourceReference::Uri { uri: lut.uri.clone() }
        } else {
            ParameterResourceReference::ExternalFile { path: std::path::PathBuf::from(&lut.uri) }
        };
        for (name, value) in [
            ("path", PropertyValue::Resource(resource)),
            (
                "processing_space",
                PropertyValue::Enum(lut.processing_space.clone()),
            ),
            ("intensity", PropertyValue::Float(lut.intensity)),
        ] {
            effect.set_static_value_by_parameter(
                &EffectType::Lut3D.parameter_id(name).map_err(|error| {
                    InterchangeError::UnsupportedSemantics {
                        profile: InterchangeFormatProfile::OtioJsonV1,
                        reason: error.to_string(),
                    }
                })?,
                value,
            )?;
        }
        effect.params = serde_json::json!({"interchange_sha256": lut.digest_sha256});
        input = append_grade_effect(&mut graph, input, effect);
    }
    graph.output = input;
    graph.validate_author_state()?;
    let mut definition = GradeDefinition::new("Imported Static CDL/LUT");
    let active = definition.active_version;
    definition
        .versions
        .iter_mut()
        .find(|version| version.id == active)
        .ok_or_else(|| InterchangeError::UnsupportedSemantics {
            profile: InterchangeFormatProfile::OtioJsonV1,
            reason: "new Grade Definition has no active version".to_owned(),
        })?
        .graph = graph;
    let id = definition.id;
    sequence.grade_definitions.push(definition);
    Ok(id)
}

fn append_grade_effect(
    graph: &mut GradeGraph,
    input: GradeGraphNodeId,
    effect: mondrian_core::effect_data::EffectNode,
) -> GradeGraphNodeId {
    let id = GradeGraphNodeId::new();
    graph.nodes.push(GradeGraphNode {
        id,
        kind: GradeGraphNodeKind::Effect { input, effect },
    });
    id
}

fn effect_instantiation_error(
    error: mondrian_effects::EffectInstantiationError,
) -> InterchangeError {
    InterchangeError::UnsupportedSemantics {
        profile: InterchangeFormatProfile::OtioJsonV1,
        reason: format!("registered color Effect could not be instantiated: {error}"),
    }
}

pub(crate) fn enforce_report_policy(
    report: &InterchangeConformanceReport,
    policy: InterchangeLossPolicy,
) -> Result<(), InterchangeError> {
    if report.has_blockers()
        || (report.requires_loss_approval() && policy == InterchangeLossPolicy::RejectUnpreserved)
    {
        Err(InterchangeError::ConformanceRejected { report: Box::new(report.clone()) })
    } else {
        Ok(())
    }
}
