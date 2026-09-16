use mondrian_core::{
    automation::{ParameterResourceReference, PropertyValue},
    effect_data::EffectType,
    timeline_data::EditorialSourceIdentity,
    AssetId, AuthoringList, ColorSpace, GradeDefinition, GradeGraph, GradeGraphNode,
    GradeGraphNodeId, GradeGraphNodeKind, ProjectColorEnvironment, Rational, SmpteCountingMode,
    SmpteTimecodeReference, TimelineDisplaySettings, TimelineTime,
};
use mondrian_interchange::*;
use mondrian_timeline::{AudioProgram, Clip, Sequence, Track};

fn tt(frames: i64, rate: Rational) -> TimelineTime {
    TimelineTime::new(frames * rate.den, rate.num).expect("frame time")
}

fn fixture() -> (Sequence, Vec<InterchangeAssetSnapshot>, AssetId) {
    let rate = Rational::FPS_25;
    let asset_id = AssetId::new();
    let source_identity = EditorialSourceIdentity::new(
        Some("A001".to_owned()),
        Some(
            SmpteTimecodeReference::parse_start(
                rate,
                SmpteCountingMode::NonDropFrame,
                "10:00:00:00",
            )
            .expect("timecode"),
        ),
        Some("source-a001".to_owned()),
    )
    .expect("identity");
    let mut sequence = Sequence::new("Commercial");
    sequence.settings.frame_rate = rate;
    sequence.settings.timeline_display =
        TimelineDisplaySettings::timecode(SmpteCountingMode::NonDropFrame, 90_000);
    let mut track = Track::new_video("V1");
    let mut first = Clip::new(asset_id, tt(0, rate), tt(50, rate)).expect("first Clip");
    first.set_source_origin(tt(250, rate)).expect("source in");
    first.label = Some("A001_C001".to_owned());
    first.media_interpretation_mut().expect("interpretation").editorial_source =
        Some(source_identity.clone());
    let mut second = Clip::new(asset_id, tt(75, rate), tt(25, rate)).expect("second Clip");
    second.set_source_origin(tt(500, rate)).expect("source in");
    second.label = Some("A001_C002".to_owned());
    second.media_interpretation_mut().expect("interpretation").editorial_source =
        Some(source_identity.clone());
    track.add_clip(first).expect("first");
    track.add_clip(second).expect("second");
    sequence.video_tracks = AuthoringList::from(vec![track]);
    sequence.audio_tracks = AuthoringList::new();
    sequence.audio_program = AudioProgram::for_tracks([]);
    let assets = vec![InterchangeAssetSnapshot {
        asset_id,
        name: "A001.mov".to_owned(),
        locator: Some("file:///media/A001.mov".to_owned()),
        editorial_source: Some(source_identity),
        color_space: None,
    }];
    (sequence, assets, asset_id)
}

fn round_trip(profile: InterchangeFormatProfile, explicit_rate: Option<Rational>) {
    let (sequence, assets, asset_id) = fixture();
    let artifact = prepare_export(InterchangeExportRequest {
        profile,
        sequence: &sequence,
        assets: &assets,
        loss_policy: InterchangeLossPolicy::AllowWithReport,
        limits: InterchangeLimits::default(),
    })
    .expect("prepare export");
    assert!(!artifact.report().has_blockers());
    let prepared = inspect_import(InterchangeImportRequest {
        profile,
        bytes: artifact.bytes().to_vec(),
        explicit_frame_rate: explicit_rate,
        limits: InterchangeLimits::default(),
    })
    .expect("inspect import");
    assert_eq!(prepared.media_references().len(), 1);
    let candidate = materialize_import(
        &prepared,
        &[InterchangeMediaBinding {
            key: prepared.media_references()[0].key.clone(),
            asset_id,
        }],
        InterchangeLossPolicy::AllowWithReport,
    )
    .expect("materialize");
    assert_eq!(candidate.sequence.settings.frame_rate, Rational::FPS_25);
    assert_eq!(candidate.sequence.video_tracks.len(), 1);
    assert_eq!(candidate.sequence.video_tracks[0].clips.len(), 2);
    assert_eq!(
        candidate.sequence.video_tracks[0].clips[0].position,
        tt(0, Rational::FPS_25)
    );
    assert_eq!(
        candidate.sequence.video_tracks[0].clips[1].position,
        tt(75, Rational::FPS_25)
    );
    candidate
        .sequence
        .validate_author_contract(&ProjectColorEnvironment::default())
        .expect("valid detached Sequence");
}

#[test]
fn otio_native_round_trip_preserves_sparse_frame_geometry() {
    round_trip(InterchangeFormatProfile::OtioJsonV1, None);
}

#[test]
fn cmx_round_trip_requires_and_uses_explicit_rate() {
    round_trip(InterchangeFormatProfile::Cmx3600, Some(Rational::FPS_25));
    let error = inspect_import(InterchangeImportRequest {
        profile: InterchangeFormatProfile::Cmx3600,
        bytes: b"TITLE: X\nFCM: NON-DROP FRAME\n".to_vec(),
        explicit_frame_rate: None,
        limits: InterchangeLimits::default(),
    })
    .expect_err("explicit rate required");
    assert!(error.to_string().contains("explicit frame rate"));
}

#[test]
fn fcp7_xml_round_trip_preserves_sequence_and_source_timecode() {
    round_trip(InterchangeFormatProfile::Fcp7XmlV5, None);
}

#[test]
fn otio_extension_round_trips_color_identity_cdl_and_digest_bound_lut() {
    let (mut sequence, assets, asset_id) = fixture();
    let mut definition = GradeDefinition::new("CDL + LUT");
    let mut graph = GradeGraph::identity();
    let mut input = graph.output;
    let mut cdl = mondrian_effects::instantiate_effect_node(EffectType::AscCdl).expect("CDL");
    cdl.set_static_value_by_parameter(
        &EffectType::AscCdl.parameter_id("slope").expect("id"),
        PropertyValue::Vec3(glam::Vec3::new(1.1, 1.0, 0.9)),
    )
    .expect("slope");
    input = append_grade_effect(&mut graph, input, cdl);
    let mut lut = mondrian_effects::instantiate_effect_node(EffectType::Lut3D).expect("LUT");
    lut.set_static_value_by_parameter(
        &EffectType::Lut3D.parameter_id("path").expect("id"),
        PropertyValue::Resource(ParameterResourceReference::Uri {
            uri: "asset://show/look.cube".to_owned(),
        }),
    )
    .expect("path");
    lut.set_static_value_by_parameter(
        &EffectType::Lut3D.parameter_id("processing_space").expect("id"),
        PropertyValue::Enum("scene_linear".to_owned()),
    )
    .expect("processing space");
    lut.params = serde_json::json!({"interchange_sha256": "ab".repeat(32)});
    input = append_grade_effect(&mut graph, input, lut);
    graph.output = input;
    let active = definition.active_version;
    definition
        .versions
        .iter_mut()
        .find(|version| version.id == active)
        .expect("active")
        .graph = graph;
    let definition_id = definition.id;
    sequence.grade_definitions.push(definition);
    let clip = &mut sequence.video_tracks[0].clips[0];
    clip.grade = Some(definition_id);
    clip.media_interpretation_mut().expect("interpretation").color_space_override =
        Some(ColorSpace::AcesCct);

    let artifact = prepare_export(InterchangeExportRequest {
        profile: InterchangeFormatProfile::OtioJsonV1,
        sequence: &sequence,
        assets: &assets,
        loss_policy: InterchangeLossPolicy::RejectUnpreserved,
        limits: InterchangeLimits::default(),
    })
    .expect("lossless declared OTIO extension");
    assert_eq!(
        artifact.report().counts_by_disposition.get("represented_by_extension"),
        Some(&1)
    );
    let prepared = inspect_import(InterchangeImportRequest {
        profile: InterchangeFormatProfile::OtioJsonV1,
        bytes: artifact.bytes().to_vec(),
        explicit_frame_rate: None,
        limits: InterchangeLimits::default(),
    })
    .expect("inspect");
    let imported = materialize_import(
        &prepared,
        &[InterchangeMediaBinding {
            key: prepared.media_references()[0].key.clone(),
            asset_id,
        }],
        InterchangeLossPolicy::RejectUnpreserved,
    )
    .expect("materialize");
    let clip = &imported.sequence.video_tracks[0].clips[0];
    assert_eq!(
        clip.media_interpretation().expect("interpretation").color_space_override,
        Some(ColorSpace::AcesCct)
    );
    let grade_id = clip.grade.expect("imported grade");
    let grade = imported
        .sequence
        .grade_definitions
        .iter()
        .find(|grade| grade.id == grade_id)
        .expect("Grade Definition");
    assert_eq!(grade.active().expect("active").graph.nodes.len(), 3);
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

#[test]
fn xml_runtime_parser_rejects_dtd_and_entity_input() {
    let error = inspect_import(InterchangeImportRequest {
        profile: InterchangeFormatProfile::Fcp7XmlV5,
        bytes: br#"<?xml version="1.0"?><!DOCTYPE xmeml [<!ENTITY xxe SYSTEM "file:///etc/passwd">]><xmeml version="5"></xmeml>"#.to_vec(),
        explicit_frame_rate: None,
        limits: InterchangeLimits::default(),
    })
    .expect_err("DTD forbidden");
    assert!(error.to_string().contains("DTD/entity"));
}

#[test]
fn materialization_never_fabricates_unresolved_asset_ids() {
    let (sequence, assets, _) = fixture();
    let artifact = prepare_export(InterchangeExportRequest {
        profile: InterchangeFormatProfile::OtioJsonV1,
        sequence: &sequence,
        assets: &assets,
        loss_policy: InterchangeLossPolicy::AllowWithReport,
        limits: InterchangeLimits::default(),
    })
    .expect("export");
    let prepared = inspect_import(InterchangeImportRequest {
        profile: InterchangeFormatProfile::OtioJsonV1,
        bytes: artifact.bytes().to_vec(),
        explicit_frame_rate: None,
        limits: InterchangeLimits::default(),
    })
    .expect("inspect");
    let error = materialize_import(&prepared, &[], InterchangeLossPolicy::AllowWithReport)
        .expect_err("binding required");
    assert!(matches!(
        error,
        InterchangeError::MissingMediaBinding { .. }
    ));
}

#[test]
fn loss_policy_rejects_omitted_effect_semantics() {
    let (mut sequence, assets, _) = fixture();
    sequence.video_tracks.push(Track::new_video("V2"));
    let error = prepare_export(InterchangeExportRequest {
        profile: InterchangeFormatProfile::Cmx3600,
        sequence: &sequence,
        assets: &assets,
        loss_policy: InterchangeLossPolicy::RejectUnpreserved,
        limits: InterchangeLimits::default(),
    })
    .expect_err("second video Track is lossy in CMX");
    assert!(matches!(
        error,
        InterchangeError::ConformanceRejected { .. }
    ));
}

#[test]
fn color_loss_is_reported_and_unbound_lut_digest_fails_closed() {
    let (mut sequence, assets, _) = fixture();
    sequence.video_tracks[0].clips[0]
        .media_interpretation_mut()
        .expect("interpretation")
        .color_space_override = Some(ColorSpace::AcesCct);
    let artifact = prepare_export(InterchangeExportRequest {
        profile: InterchangeFormatProfile::Cmx3600,
        sequence: &sequence,
        assets: &assets,
        loss_policy: InterchangeLossPolicy::AllowWithReport,
        limits: InterchangeLimits::default(),
    })
    .expect("loss report");
    assert!(artifact
        .report()
        .findings
        .iter()
        .any(|finding| finding.code == "COLOR_IDENTITY_OMITTED"));

    let mut definition = GradeDefinition::new("Unbound LUT");
    let mut graph = GradeGraph::identity();
    let mut lut = mondrian_effects::instantiate_effect_node(EffectType::Lut3D).expect("LUT");
    lut.set_static_value_by_parameter(
        &EffectType::Lut3D.parameter_id("path").expect("id"),
        PropertyValue::Resource(ParameterResourceReference::Uri {
            uri: "asset://show/unbound.cube".to_owned(),
        }),
    )
    .expect("path");
    lut.set_static_value_by_parameter(
        &EffectType::Lut3D.parameter_id("processing_space").expect("id"),
        PropertyValue::Enum("scene_linear".to_owned()),
    )
    .expect("processing space");
    let input = graph.output;
    let output = append_grade_effect(&mut graph, input, lut);
    graph.output = output;
    definition.versions[0].graph = graph;
    let definition_id = definition.id;
    sequence.grade_definitions.push(definition);
    sequence.video_tracks[0].clips[0].grade = Some(definition_id);
    let error = prepare_export(InterchangeExportRequest {
        profile: InterchangeFormatProfile::OtioJsonV1,
        sequence: &sequence,
        assets: &assets,
        loss_policy: InterchangeLossPolicy::AllowWithReport,
        limits: InterchangeLimits::default(),
    })
    .expect_err("LUT digest is required even under allow-with-report");
    let report = error.conformance_report().expect("rejected report remains available");
    assert!(report.has_blockers());
    assert!(report.findings.iter().any(|finding| finding.code == "LUT_DIGEST_MISSING"));
}

#[derive(Debug)]
struct BridgeEchoToolchain {
    identity: AafToolchainIdentity,
}

const AAF_CFB_MAGIC: [u8; 8] = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];

impl AafToolchain for BridgeEchoToolchain {
    fn decode_aaf(
        &self,
        input: &[u8],
        _limits: InterchangeLimits,
    ) -> Result<Vec<u8>, InterchangeError> {
        Ok(input[AAF_CFB_MAGIC.len()..].to_vec())
    }
    fn encode_aaf(
        &self,
        bridge: &[u8],
        _limits: InterchangeLimits,
    ) -> Result<Vec<u8>, InterchangeError> {
        Ok(AAF_CFB_MAGIC.into_iter().chain(bridge.iter().copied()).collect())
    }
    fn identity(&self) -> &AafToolchainIdentity {
        &self.identity
    }
}

#[test]
fn aaf_binary_boundary_uses_only_the_qualified_bridge_contract() {
    let toolchain = BridgeEchoToolchain {
        identity: AafToolchainIdentity {
            implementation: "test.echo".to_owned(),
            version: "1".to_owned(),
            bridge_contract_version: 1,
            engine: "test".to_owned(),
        },
    };
    let (sequence, assets, asset_id) = fixture();
    let artifact = prepare_aaf_export_with_toolchain(
        InterchangeExportRequest {
            profile: InterchangeFormatProfile::AafEditProtocolV1,
            sequence: &sequence,
            assets: &assets,
            loss_policy: InterchangeLossPolicy::AllowWithReport,
            limits: InterchangeLimits::default(),
        },
        &toolchain,
    )
    .expect("AAF bridge export");
    let prepared =
        inspect_aaf_with_toolchain(artifact.bytes(), InterchangeLimits::default(), &toolchain)
            .expect("AAF bridge import");
    let candidate = materialize_import(
        &prepared,
        &[InterchangeMediaBinding {
            key: prepared.media_references()[0].key.clone(),
            asset_id,
        }],
        InterchangeLossPolicy::AllowWithReport,
    )
    .expect("materialize");
    assert_eq!(candidate.sequence.video_tracks[0].clips.len(), 2);

    let error = inspect_aaf_with_toolchain(b"not-an-aaf", InterchangeLimits::default(), &toolchain)
        .expect_err("non-CFB input must fail before the helper");
    assert!(error.to_string().contains("Compound File Binary"));

    let wrong_contract = BridgeEchoToolchain {
        identity: AafToolchainIdentity {
            implementation: "test.echo".to_owned(),
            version: "1".to_owned(),
            bridge_contract_version: AAF_BRIDGE_CONTRACT_VERSION + 1,
            engine: "test".to_owned(),
        },
    };
    let error = inspect_aaf_with_toolchain(
        &AAF_CFB_MAGIC,
        InterchangeLimits::default(),
        &wrong_contract,
    )
    .expect_err("wrong bridge contract must fail before helper execution");
    assert!(matches!(
        error,
        InterchangeError::AafHelperUnavailable { .. }
    ));
}

#[test]
fn json_depth_and_input_byte_limits_fail_closed() {
    let limits = InterchangeLimits { max_bytes: 4, ..Default::default() };
    let error = inspect_import(InterchangeImportRequest {
        profile: InterchangeFormatProfile::OtioJsonV1,
        bytes: b"12345".to_vec(),
        explicit_frame_rate: None,
        limits,
    })
    .expect_err("byte limit");
    assert!(matches!(error, InterchangeError::LimitExceeded { .. }));

    let limits = InterchangeLimits { max_nesting_depth: 2, ..Default::default() };
    let error = inspect_import(InterchangeImportRequest {
        profile: InterchangeFormatProfile::Fcp7XmlV5,
        bytes: br#"<xmeml version="5"><sequence><media><video/></media></sequence></xmeml>"#
            .to_vec(),
        explicit_frame_rate: None,
        limits,
    })
    .expect_err("XML nesting limit");
    assert!(matches!(error, InterchangeError::LimitExceeded { .. }));
}
