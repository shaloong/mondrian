//! Immutable Sequence projection, preservation analysis, and native encoding.

use crate::{formats, import::enforce_report_policy, *};
use mondrian_core::{
    automation::ParameterResourceReference, effect_data::EffectType, FramePosition, FrameRounding,
    GradeGraphNodeKind, Rational, TimeScale, TimelineDisplayFormat, TimelineTime,
};
use mondrian_timeline::{ClipKind, Sequence, TrackType, VideoTransitionType};
use std::collections::HashMap;

/// Immutable export preparation request.
pub struct InterchangeExportRequest<'a> {
    /// Exact qualified native profile.
    pub profile: InterchangeFormatProfile,
    /// Canonical immutable Sequence author state.
    pub sequence: &'a Sequence,
    /// Immutable Asset Library projection for every referenced Asset.
    pub assets: &'a [InterchangeAssetSnapshot],
    /// Caller policy for non-blocking unpreserved semantics.
    pub loss_policy: InterchangeLossPolicy,
    /// Closed resource limits for native encoding and helper execution.
    pub limits: InterchangeLimits,
}

/// Publication-ready native artifact and its inseparable conformance evidence.
#[derive(Debug, Clone)]
pub struct PreparedInterchangeArtifact {
    profile: InterchangeFormatProfile,
    bytes: Vec<u8>,
    report: InterchangeConformanceReport,
}

impl PreparedInterchangeArtifact {
    /// Qualified native profile used to prepare these bytes.
    pub const fn profile(&self) -> InterchangeFormatProfile {
        self.profile
    }
    /// Native artifact bytes, still coupled to their report by this value.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    /// Complete conformance and loss evidence.
    pub const fn report(&self) -> &InterchangeConformanceReport {
        &self.report
    }
    /// Consume the prepared value while retaining report coupling at the call
    /// site; durable publication belongs to the product/storage Adapter.
    pub fn into_parts(self) -> (Vec<u8>, InterchangeConformanceReport) {
        (self.bytes, self.report)
    }
}

/// Prepare an OTIO/CMX/FCP XML artifact. Binary AAF uses the explicitly
/// qualified helper overload.
pub fn prepare_export(
    request: InterchangeExportRequest<'_>,
) -> Result<PreparedInterchangeArtifact, InterchangeError> {
    request.limits.validate()?;
    if request.profile == InterchangeFormatProfile::AafEditProtocolV1 {
        return Err(InterchangeError::AafHelperUnavailable {
            reason: "use prepare_aaf_export_with_toolchain".to_owned(),
        });
    }
    let (timeline, mut findings) =
        project_sequence(request.sequence, request.assets, request.profile)?;
    let semantic = serde_json::to_vec(&timeline)?;
    let encoded = formats::encode(request.profile, &timeline, request.limits)?;
    findings.extend(encoded.findings);
    finish_export(
        request.profile,
        encoded.bytes,
        semantic,
        findings,
        request.loss_policy,
    )
}

/// Prepare metadata-only AAF through an already-qualified isolated helper.
pub fn prepare_aaf_export_with_toolchain(
    request: InterchangeExportRequest<'_>,
    toolchain: &dyn AafToolchain,
) -> Result<PreparedInterchangeArtifact, InterchangeError> {
    if request.profile != InterchangeFormatProfile::AafEditProtocolV1 {
        return Err(InterchangeError::AafHelperFailed {
            reason: "AAF overload requires the AAF profile".to_owned(),
        });
    }
    request.limits.validate()?;
    toolchain.identity().validate(request.limits.max_string_bytes)?;
    let (timeline, mut findings) =
        project_sequence(request.sequence, request.assets, request.profile)?;
    let semantic = serde_json::to_vec(&timeline)?;
    let bridge = formats::aaf::encode_helper_input(&timeline, request.limits)?;
    findings.extend(bridge.findings);
    let bytes = toolchain.encode_aaf(&bridge.bytes, request.limits)?;
    if bytes.len() > request.limits.max_bytes {
        return Err(InterchangeError::LimitExceeded {
            limit_name: "AAF bytes",
            actual: bytes.len(),
            maximum: request.limits.max_bytes,
        });
    }
    toolchain::validate_aaf_binary(&bytes)?;
    finish_export(
        request.profile,
        bytes,
        semantic,
        findings,
        request.loss_policy,
    )
}

fn finish_export(
    profile: InterchangeFormatProfile,
    bytes: Vec<u8>,
    semantic: Vec<u8>,
    findings: Vec<InterchangeFinding>,
    policy: InterchangeLossPolicy,
) -> Result<PreparedInterchangeArtifact, InterchangeError> {
    let report = InterchangeConformanceReport::new(
        profile,
        InterchangeDirection::Export,
        &bytes,
        &semantic,
        findings,
    );
    enforce_report_policy(&report, policy)?;
    Ok(PreparedInterchangeArtifact { profile, bytes, report })
}

fn project_sequence(
    sequence: &Sequence,
    assets: &[InterchangeAssetSnapshot],
    profile: InterchangeFormatProfile,
) -> Result<(formats::InterchangeTimeline, Vec<InterchangeFinding>), InterchangeError> {
    let rate = sequence.settings.frame_rate;
    let asset_map = assets.iter().map(|asset| (asset.asset_id, asset)).collect::<HashMap<_, _>>();
    let mut media_map = HashMap::<InterchangeMediaKey, InterchangeMediaReference>::new();
    let mut tracks = Vec::new();
    let mut findings = Vec::new();
    for track in sequence.video_tracks.iter().chain(&sequence.audio_tracks) {
        let kind = match track.track_type {
            TrackType::Video => InterchangeTrackKind::Video,
            TrackType::Audio => InterchangeTrackKind::Audio,
            TrackType::Subtitle => {
                findings.push(owner_finding(
                    "SUBTITLE_TRACK_OMITTED",
                    InterchangeFindingSeverity::Warning,
                    InterchangeDisposition::Omitted,
                    "timeline.track",
                    sequence,
                    Some(track.id),
                    None,
                    None,
                ));
                continue;
            }
        };
        let mut clips = Vec::new();
        for clip in &track.clips {
            if clip.kind() != ClipKind::Media {
                findings.push(owner_finding(
                    match clip.kind() {
                        ClipKind::NestedSequence => "NESTED_SEQUENCE_FLATTEN_REQUIRED",
                        _ => "GENERATOR_OR_ADJUSTMENT_OMITTED",
                    },
                    InterchangeFindingSeverity::Warning,
                    InterchangeDisposition::Omitted,
                    "timeline.clip",
                    sequence,
                    Some(track.id),
                    Some(clip.id),
                    None,
                ));
                continue;
            }
            if clip.source_time_scale() != TimeScale::ONE {
                findings.push(owner_finding(
                    "SPEED_EFFECT_UNSUPPORTED",
                    InterchangeFindingSeverity::Blocker,
                    InterchangeDisposition::Unsupported,
                    "timeline.retime",
                    sequence,
                    Some(track.id),
                    Some(clip.id),
                    None,
                ));
                continue;
            }
            let Some(asset_id) = clip.media_asset_id() else {
                continue;
            };
            let asset = asset_map.get(&asset_id).copied().ok_or_else(|| {
                InterchangeError::UnsupportedSemantics {
                    profile,
                    reason: format!("Asset {asset_id} has no immutable interchange snapshot"),
                }
            })?;
            let media_key = InterchangeMediaKey(format!("asset:{asset_id}"));
            let interpretation = clip.media_interpretation();
            let editorial_source = interpretation
                .and_then(|value| value.editorial_source.clone())
                .or_else(|| asset.editorial_source.clone());
            let color_space = interpretation
                .and_then(|value| value.color_space_override)
                .or(asset.color_space);
            let native_color_space = if profile == InterchangeFormatProfile::OtioJsonV1 {
                color_space
            } else {
                None
            };
            let static_grade =
                project_static_grade(sequence, clip, profile, track.id, &mut findings)?;
            let reference = InterchangeMediaReference {
                key: media_key.clone(),
                name: Some(asset.name.clone()),
                proposed_locator: asset.locator.clone(),
                editorial_source: editorial_source.clone(),
                color_space: native_color_space,
            };
            media_map.entry(media_key.clone()).or_insert(reference);
            if asset.locator.is_none() {
                findings.push(owner_finding(
                    "LOCATOR_RELINK_REQUIRED",
                    InterchangeFindingSeverity::Warning,
                    InterchangeDisposition::RelinkRequired,
                    "media.locator",
                    sequence,
                    Some(track.id),
                    Some(clip.id),
                    None,
                ));
            }
            if !clip.effects.is_empty() || !clip.masks.is_empty() {
                findings.push(owner_finding(
                    "EFFECT_OR_MASK_OMITTED",
                    InterchangeFindingSeverity::Warning,
                    InterchangeDisposition::Omitted,
                    "visual.processing",
                    sequence,
                    Some(track.id),
                    Some(clip.id),
                    None,
                ));
            }
            if clip.grade.is_some() {
                let disposition =
                    if profile == InterchangeFormatProfile::OtioJsonV1 && static_grade.is_some() {
                        InterchangeDisposition::RepresentedByExtension
                    } else {
                        InterchangeDisposition::Omitted
                    };
                findings.push(owner_finding(
                    "GRADE_GRAPH_EXTENSION_REQUIRED",
                    if disposition == InterchangeDisposition::Omitted {
                        InterchangeFindingSeverity::Warning
                    } else {
                        InterchangeFindingSeverity::Info
                    },
                    disposition,
                    "color.grade",
                    sequence,
                    Some(track.id),
                    Some(clip.id),
                    None,
                ));
            }
            if color_space.is_some() && profile != InterchangeFormatProfile::OtioJsonV1 {
                findings.push(owner_finding(
                    "COLOR_IDENTITY_OMITTED",
                    InterchangeFindingSeverity::Warning,
                    InterchangeDisposition::Omitted,
                    "color.input_identity",
                    sequence,
                    Some(track.id),
                    Some(clip.id),
                    None,
                ));
            }
            clips.push(formats::InterchangeClip {
                key: clip.id.to_string(),
                media_key,
                name: clip.label.clone(),
                record_start: exact_frame(clip.position, rate, profile, "Clip position")?,
                duration: exact_frame(clip.duration, rate, profile, "Clip duration")?,
                source_start: exact_frame(
                    clip.source_origin(),
                    rate,
                    profile,
                    "Clip source origin",
                )?,
                enabled: !clip.is_disabled,
                editorial_source,
                color_space: native_color_space,
                static_grade: if profile == InterchangeFormatProfile::OtioJsonV1 {
                    static_grade
                } else {
                    None
                },
            });
        }
        tracks.push(formats::InterchangeTrack {
            name: track.name.clone(),
            kind,
            enabled: if kind == InterchangeTrackKind::Video {
                track.is_visible && !track.is_muted
            } else {
                !track.is_muted
            },
            locked: track.is_locked,
            clips,
        });
    }
    let mut transitions = Vec::new();
    for transition in &sequence.video_transitions {
        if !transition.is_enabled {
            continue;
        }
        if !matches!(
            transition.transition_type,
            VideoTransitionType::CrossDissolve
        ) {
            findings.push(owner_finding(
                "TRANSITION_UNSUPPORTED",
                InterchangeFindingSeverity::Warning,
                InterchangeDisposition::Omitted,
                "timeline.transition",
                sequence,
                None,
                None,
                Some(transition.id),
            ));
            continue;
        }
        transitions.push(formats::InterchangeTransition {
            key: transition.id.to_string(),
            left_clip_key: transition.left.to_string(),
            right_clip_key: transition.right.to_string(),
            start: exact_frame(
                transition.sequence_range.start,
                rate,
                profile,
                "Transition start",
            )?,
            duration: exact_frame(
                transition.sequence_range.duration,
                rate,
                profile,
                "Transition duration",
            )?,
        });
    }
    if sequence.timeline_grade.is_some() || !sequence.grade_groups.is_empty() {
        findings.push(owner_finding(
            "SEQUENCE_GRADE_HIERARCHY_OMITTED",
            InterchangeFindingSeverity::Warning,
            InterchangeDisposition::Omitted,
            "color.grade",
            sequence,
            None,
            None,
            None,
        ));
    }
    let start_timecode = match sequence.settings.timeline_display.format {
        TimelineDisplayFormat::Timecode(mode) => Some(
            mondrian_core::SmpteTimecodeReference::new(
                rate,
                mode,
                sequence.settings.timeline_display.timecode_start_frame,
            )
            .map_err(|error| InterchangeError::UnsupportedSemantics {
                profile,
                reason: error.to_string(),
            })?,
        ),
        TimelineDisplayFormat::Frames => None,
    };
    Ok((
        formats::InterchangeTimeline {
            name: sequence.name.clone(),
            rate_num: rate.num,
            rate_den: rate.den,
            start_timecode,
            media: media_map.into_values().collect(),
            tracks,
            transitions,
        },
        findings,
    ))
}

fn exact_frame(
    time: TimelineTime,
    rate: Rational,
    profile: InterchangeFormatProfile,
    field: &str,
) -> Result<i64, InterchangeError> {
    let position = time.to_frame_position(rate, FrameRounding::Nearest)?;
    let restored = TimelineTime::from_frame_position(FramePosition::new(
        position.frame,
        Rational::new(rate.den, rate.num),
    ))?;
    if restored != time {
        return Err(InterchangeError::UnsupportedSemantics {
            profile,
            reason: format!("{field} is not aligned to the Sequence frame grid"),
        });
    }
    Ok(position.frame)
}

fn project_static_grade(
    sequence: &Sequence,
    clip: &mondrian_timeline::Clip,
    profile: InterchangeFormatProfile,
    track_id: mondrian_core::TrackId,
    findings: &mut Vec<InterchangeFinding>,
) -> Result<Option<formats::InterchangeStaticGrade>, InterchangeError> {
    let Some(definition_id) = clip.grade else {
        return Ok(None);
    };
    let Some(definition) =
        sequence.grade_definitions.iter().find(|value| value.id == definition_id)
    else {
        return Err(InterchangeError::UnsupportedSemantics {
            profile,
            reason: format!(
                "Clip {} references missing Grade Definition {definition_id}",
                clip.id
            ),
        });
    };
    let Some(version) = definition.active() else {
        return Err(InterchangeError::UnsupportedSemantics {
            profile,
            reason: format!("Grade Definition {definition_id} has no active version"),
        });
    };
    if definition.versions.len() > 1 {
        findings.push(owner_finding(
            "GRADE_VERSIONS_NOT_PRESERVED",
            InterchangeFindingSeverity::Warning,
            InterchangeDisposition::Omitted,
            "color.grade_versions",
            sequence,
            Some(track_id),
            Some(clip.id),
            None,
        ));
    }
    let by_id = version
        .graph
        .nodes
        .iter()
        .map(|node| (node.id, node))
        .collect::<HashMap<_, _>>();
    let mut cursor = version.graph.output;
    let mut effects = Vec::new();
    loop {
        let node =
            by_id
                .get(&cursor)
                .copied()
                .ok_or_else(|| InterchangeError::UnsupportedSemantics {
                    profile,
                    reason: "Grade graph output traversal failed".to_owned(),
                })?;
        match &node.kind {
            GradeGraphNodeKind::Input => break,
            GradeGraphNodeKind::Effect { input, effect } => {
                effects.push(effect);
                cursor = *input;
            }
            GradeGraphNodeKind::Parallel { .. } | GradeGraphNodeKind::Layer { .. } => {
                findings.push(owner_finding(
                    "GRADE_GRAPH_NONLINEAR_OMITTED",
                    InterchangeFindingSeverity::Warning,
                    InterchangeDisposition::Omitted,
                    "color.grade",
                    sequence,
                    Some(track_id),
                    Some(clip.id),
                    None,
                ));
                return Ok(None);
            }
        }
    }
    effects.reverse();
    if effects
        .iter()
        .any(|effect| effect.properties.iter().any(|(_, property)| property.is_animated()))
    {
        findings.push(owner_finding(
            "GRADE_AUTOMATION_OMITTED",
            InterchangeFindingSeverity::Warning,
            InterchangeDisposition::Omitted,
            "color.grade",
            sequence,
            Some(track_id),
            Some(clip.id),
            None,
        ));
        return Ok(None);
    }

    let mut cdl = None;
    let mut lut = None;
    let mut seen_lut = false;
    for effect in effects.into_iter().filter(|effect| effect.is_enabled) {
        match effect.effect_type {
            EffectType::AscCdl if cdl.is_none() && !seen_lut => {
                let vec3 =
                    |name: &str, fallback: glam::Vec3| -> Result<[f32; 3], InterchangeError> {
                        let id = EffectType::AscCdl.parameter_id(name).map_err(|error| {
                            InterchangeError::UnsupportedSemantics {
                                profile,
                                reason: format!("invalid ASC CDL parameter contract: {error}"),
                            }
                        })?;
                        Ok(effect
                            .evaluate_vec3_parameter(&id, TimelineTime::ZERO, fallback)
                            .to_array())
                    };
                let saturation_id =
                    EffectType::AscCdl.parameter_id("saturation").map_err(|error| {
                        InterchangeError::UnsupportedSemantics {
                            profile,
                            reason: format!("invalid ASC CDL parameter contract: {error}"),
                        }
                    })?;
                cdl = Some(formats::InterchangeAscCdl {
                    slope: vec3("slope", glam::Vec3::ONE)?,
                    offset: vec3("offset", glam::Vec3::ZERO)?,
                    power: vec3("power", glam::Vec3::ONE)?,
                    saturation: effect.evaluate_f32_parameter(
                        &saturation_id,
                        TimelineTime::ZERO,
                        1.0,
                    ),
                });
            }
            EffectType::Lut3D if lut.is_none() => {
                seen_lut = true;
                let parameter_id = |name: &str| {
                    EffectType::Lut3D.parameter_id(name).map_err(|error| {
                        InterchangeError::UnsupportedSemantics {
                            profile,
                            reason: format!("invalid LUT parameter contract: {error}"),
                        }
                    })
                };
                let path_id = parameter_id("path")?;
                let processing_id = parameter_id("processing_space")?;
                let intensity_id = parameter_id("intensity")?;
                let uri = match effect.evaluate_resource_parameter(&path_id, TimelineTime::ZERO) {
                    Some(ParameterResourceReference::ExternalFile { path }) => {
                        path.to_string_lossy().into_owned()
                    }
                    Some(ParameterResourceReference::Uri { uri }) => uri,
                    _ => {
                        findings.push(owner_finding(
                            "LUT_REFERENCE_UNRESOLVED",
                            InterchangeFindingSeverity::Blocker,
                            InterchangeDisposition::Unsupported,
                            "color.lut",
                            sequence,
                            Some(track_id),
                            Some(clip.id),
                            None,
                        ));
                        return Ok(None);
                    }
                };
                let digest = effect
                    .params
                    .get("interchange_sha256")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| {
                        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
                    .map(str::to_owned);
                let Some(digest_sha256) = digest else {
                    findings.push(owner_finding(
                        "LUT_DIGEST_MISSING",
                        InterchangeFindingSeverity::Blocker,
                        InterchangeDisposition::Unsupported,
                        "color.lut",
                        sequence,
                        Some(track_id),
                        Some(clip.id),
                        None,
                    ));
                    return Ok(None);
                };
                let Some(processing_space) = effect
                    .evaluate_enum_parameter(&processing_id, TimelineTime::ZERO)
                    .filter(|value| value != "unassigned")
                else {
                    findings.push(owner_finding(
                        "LUT_PROCESSING_SPACE_MISSING",
                        InterchangeFindingSeverity::Blocker,
                        InterchangeDisposition::Unsupported,
                        "color.lut",
                        sequence,
                        Some(track_id),
                        Some(clip.id),
                        None,
                    ));
                    return Ok(None);
                };
                lut = Some(formats::InterchangeLutReference {
                    uri,
                    digest_sha256,
                    processing_space,
                    intensity: effect.evaluate_f32_parameter(
                        &intensity_id,
                        TimelineTime::ZERO,
                        1.0,
                    ),
                });
            }
            _ => {
                findings.push(owner_finding(
                    "GRADE_NODE_UNSUPPORTED",
                    InterchangeFindingSeverity::Warning,
                    InterchangeDisposition::Omitted,
                    "color.grade",
                    sequence,
                    Some(track_id),
                    Some(clip.id),
                    None,
                ));
                return Ok(None);
            }
        }
    }
    Ok(Some(formats::InterchangeStaticGrade { cdl, lut }))
}

fn owner_finding(
    code: &str,
    severity: InterchangeFindingSeverity,
    disposition: InterchangeDisposition,
    domain: &str,
    sequence: &Sequence,
    track_id: Option<mondrian_core::TrackId>,
    clip_id: Option<mondrian_core::ClipId>,
    transition_id: Option<mondrian_core::VideoTransitionId>,
) -> InterchangeFinding {
    InterchangeFinding {
        code: code.to_owned(),
        severity,
        disposition,
        domain: domain.to_owned(),
        owner: InterchangeOwnerAddress {
            sequence_id: Some(sequence.id),
            track_id,
            clip_id,
            transition_id,
            ..Default::default()
        },
        original: None,
        result: None,
        approval_required: disposition.is_unpreserved(),
        reversible: !matches!(disposition, InterchangeDisposition::Omitted),
    }
}
