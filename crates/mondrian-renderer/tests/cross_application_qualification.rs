use mondrian_renderer::{
    import_external_color_reference_with_limits, ColorFrameAlpha, ColorReferenceDescriptor,
    ColorReferenceEncoding, ColorReferenceFrame, ColorReferenceOrigin, ColorReferencePayloadFormat,
    ColorReferencePixels, CrossApplicationAccuracyBudget, CrossApplicationArtifactEvidence,
    CrossApplicationFrameCoordinate, CrossApplicationPixelOrientation, CrossApplicationProducer,
    CrossApplicationProducerRequirement, CrossApplicationQualificationArtifact,
    CrossApplicationQualificationCase, CrossApplicationQualificationLimits,
    CrossApplicationQualificationProfile, CrossApplicationQualificationRun,
    CrossApplicationQualificationStatus, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
    PreparedCrossApplicationQualification, SrgbDisplayAccuracyBudget,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[path = "support/color_reference_image.rs"]
mod color_reference_image;

use color_reference_image::ImageColorReferenceDecoder;

const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SHA_C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const GIT_SHA: &str = "dddddddddddddddddddddddddddddddddddddddd";

fn producer_requirements() -> Vec<CrossApplicationProducerRequirement> {
    [
        CrossApplicationProducer::Mondrian,
        CrossApplicationProducer::Blender,
        CrossApplicationProducer::DaVinciResolve,
        CrossApplicationProducer::AdobePremierePro,
    ]
    .into_iter()
    .map(|producer| CrossApplicationProducerRequirement {
        producer,
        exact_version: format!("{producer:?}-version-1"),
        exact_build: format!("{producer:?}-build-1"),
    })
    .collect()
}

fn frame_coordinate() -> CrossApplicationFrameCoordinate {
    CrossApplicationFrameCoordinate {
        rate_numerator: 24_000,
        rate_denominator: 1_001,
        frame_index: 17,
    }
}

fn profile() -> CrossApplicationQualificationProfile {
    CrossApplicationQualificationProfile {
        schema_version: 1,
        producer_scope: mondrian_renderer::CrossApplicationProducerScope::FullCommercialMatrix,
        qualification_id: "commercial-cross-app-color-v1".to_owned(),
        edition: "2026-08-fixture".to_owned(),
        stimulus_sha256: SHA_A.to_owned(),
        required_producers: producer_requirements(),
        cases: vec![CrossApplicationQualificationCase {
            case_id: "sdr-srgb-alpha".to_owned(),
            required_producers: vec![
                CrossApplicationProducer::Mondrian,
                CrossApplicationProducer::Blender,
                CrossApplicationProducer::DaVinciResolve,
                CrossApplicationProducer::AdobePremierePro,
            ],
            payload_format: ColorReferencePayloadFormat::Png,
            encoding: ColorReferenceEncoding::SrgbDisplayRgba8,
            width: 2,
            height: 1,
            alpha: ColorFrameAlpha::StraightCoverage,
            orientation: CrossApplicationPixelOrientation::TopLeft,
            pixel_aspect_numerator: 1,
            pixel_aspect_denominator: 1,
            reference_white_nits: None,
            nominal_peak_nits: None,
            frame: frame_coordinate(),
            accuracy: CrossApplicationAccuracyBudget::SrgbDisplay {
                budget: SrgbDisplayAccuracyBudget::new(0.5, 0.25, 0.5, 0),
            },
        }],
        limits: CrossApplicationQualificationLimits {
            max_cases: 8,
            max_artifacts: 32,
            max_pixels_per_artifact: 8_294_400,
            max_encoded_bytes_per_artifact: 512 * 1024 * 1024,
        },
    }
}

fn expected_version(producer: CrossApplicationProducer) -> String {
    format!("{producer:?}-version-1")
}

fn expected_build(producer: CrossApplicationProducer) -> String {
    format!("{producer:?}-build-1")
}

fn producer_label(producer: CrossApplicationProducer) -> &'static str {
    match producer {
        CrossApplicationProducer::Mondrian => "Mondrian",
        CrossApplicationProducer::Blender => "Blender",
        CrossApplicationProducer::DaVinciResolve => "DaVinci Resolve",
        CrossApplicationProducer::AdobePremierePro => "Adobe Premiere Pro",
    }
}

fn artifact(
    producer: CrossApplicationProducer,
    pixels: Vec<u8>,
) -> CrossApplicationQualificationArtifact {
    let version = expected_version(producer);
    let origin = if producer == CrossApplicationProducer::Mondrian {
        ColorReferenceOrigin::MondrianRegression
    } else {
        ColorReferenceOrigin::IndependentApplication
    };
    CrossApplicationQualificationArtifact {
        evidence: CrossApplicationArtifactEvidence {
            run_id: "run-001".to_owned(),
            case_id: "sdr-srgb-alpha".to_owned(),
            producer,
            producer_version: version.clone(),
            producer_build: expected_build(producer),
            os_identity: "Windows-11-build-fixture".to_owned(),
            executable_sha256: SHA_B.to_owned(),
            native_project_sha256: SHA_B.to_owned(),
            render_settings_sha256: SHA_B.to_owned(),
            adapter_name: "qualification-fixture-adapter".to_owned(),
            adapter_version: "1.0.0".to_owned(),
            adapter_sha256: SHA_B.to_owned(),
            decoded_metadata_sha256: SHA_B.to_owned(),
            operator_attestation_sha256: SHA_B.to_owned(),
            artifact_sha256: SHA_C.to_owned(),
            encoded_byte_len: 128,
            frame: frame_coordinate(),
            orientation: CrossApplicationPixelOrientation::TopLeft,
            pixel_aspect_numerator: 1,
            pixel_aspect_denominator: 1,
            automatic_alignment_applied: false,
            decoder_color_conversion_applied: false,
        },
        frame: ColorReferenceFrame {
            descriptor: ColorReferenceDescriptor {
                schema_version: 1,
                reference_id: format!("sdr-srgb-alpha-{producer:?}"),
                origin,
                producer: producer_label(producer).to_owned(),
                producer_version: version,
                source_uri: None,
                source_artifact_sha256: SHA_A.to_owned(),
                content_sha256: SHA_C.to_owned(),
                payload_format: ColorReferencePayloadFormat::Png,
                width: 2,
                height: 1,
                encoding: ColorReferenceEncoding::SrgbDisplayRgba8,
                alpha: ColorFrameAlpha::StraightCoverage,
                reference_white_nits: None,
                nominal_peak_nits: None,
            },
            pixels: ColorReferencePixels::Rgba8(pixels),
        },
    }
}

fn run(artifacts: Vec<CrossApplicationQualificationArtifact>) -> CrossApplicationQualificationRun {
    CrossApplicationQualificationRun {
        run_id: "run-001".to_owned(),
        source_revision: GIT_SHA.to_owned(),
        source_clean: true,
        machine_report_sha256: SHA_C.to_owned(),
        artifacts,
    }
}

fn producers() -> [CrossApplicationProducer; 4] {
    [
        CrossApplicationProducer::Mondrian,
        CrossApplicationProducer::Blender,
        CrossApplicationProducer::DaVinciResolve,
        CrossApplicationProducer::AdobePremierePro,
    ]
}

fn reference_pixels() -> Vec<u8> {
    vec![0, 18, 255, 64, 255, 128, 0, 255]
}

fn float_profile(
    case_id: &str,
    encoding: ColorReferenceEncoding,
    accuracy: CrossApplicationAccuracyBudget,
    reference_white_nits: Option<f32>,
    nominal_peak_nits: Option<f32>,
) -> CrossApplicationQualificationProfile {
    let mut profile = profile();
    profile.cases = vec![CrossApplicationQualificationCase {
        case_id: case_id.to_owned(),
        required_producers: producers().to_vec(),
        payload_format: ColorReferencePayloadFormat::JsonFloat,
        encoding,
        width: 2,
        height: 1,
        alpha: ColorFrameAlpha::Opaque,
        orientation: CrossApplicationPixelOrientation::TopLeft,
        pixel_aspect_numerator: 1,
        pixel_aspect_denominator: 1,
        reference_white_nits,
        nominal_peak_nits,
        frame: frame_coordinate(),
        accuracy,
    }];
    profile
}

fn float_artifact(
    case_id: &str,
    producer: CrossApplicationProducer,
    encoding: ColorReferenceEncoding,
    pixels: Vec<[f32; 4]>,
    reference_white_nits: Option<f32>,
    nominal_peak_nits: Option<f32>,
) -> CrossApplicationQualificationArtifact {
    let mut artifact = artifact(producer, reference_pixels());
    artifact.evidence.case_id = case_id.to_owned();
    artifact.frame.descriptor.reference_id = format!("{case_id}-{producer:?}");
    artifact.frame.descriptor.payload_format = ColorReferencePayloadFormat::JsonFloat;
    artifact.frame.descriptor.encoding = encoding;
    artifact.frame.descriptor.alpha = ColorFrameAlpha::Opaque;
    artifact.frame.descriptor.reference_white_nits = reference_white_nits;
    artifact.frame.descriptor.nominal_peak_nits = nominal_peak_nits;
    artifact.frame.pixels = ColorReferencePixels::RgbaF32(pixels);
    artifact
}

#[test]
fn complete_matrix_is_qualified_pairwise_and_order_independent() {
    let prepared = PreparedCrossApplicationQualification::compile(profile()).expect("profile");
    let artifacts = producers()
        .into_iter()
        .map(|producer| artifact(producer, reference_pixels()))
        .collect::<Vec<_>>();
    let report = prepared.evaluate(run(artifacts.clone())).expect("qualification report");
    let reversed = prepared
        .evaluate(run(artifacts.into_iter().rev().collect()))
        .expect("reordered qualification report");

    assert_eq!(
        report.status,
        CrossApplicationQualificationStatus::Qualified
    );
    assert!(report.missing_artifacts.is_empty());
    assert_eq!(report.cases[0].comparisons.len(), 6);
    assert!(report.cases[0].comparisons.iter().all(|comparison| comparison.within_budget));
    assert!(report.verify_evidence());
    assert_eq!(report.evidence_sha256, reversed.evidence_sha256);
}

#[test]
fn linear_and_pq_cases_dispatch_their_distinct_accuracy_metrics() {
    let linear_profile = float_profile(
        "linear-rec2020",
        ColorReferenceEncoding::SceneLinearRec2020RgbaF32,
        CrossApplicationAccuracyBudget::SceneLinearRec2020 {
            budget: LinearRgbaAccuracyBudget {
                rgb: LinearAccuracyBudget::finite(0.001, 0.001, 0.001),
                alpha: LinearAccuracyBudget::finite(0.0, 0.0, 0.0),
            },
        },
        None,
        None,
    );
    let linear_pixels = vec![[-0.125, 0.18, 16.0, 1.0], [1.0, 2.0, 4.0, 1.0]];
    let linear_artifacts = producers()
        .into_iter()
        .map(|producer| {
            float_artifact(
                "linear-rec2020",
                producer,
                ColorReferenceEncoding::SceneLinearRec2020RgbaF32,
                linear_pixels.clone(),
                None,
                None,
            )
        })
        .collect();
    let linear_report = PreparedCrossApplicationQualification::compile(linear_profile)
        .expect("linear profile")
        .evaluate(run(linear_artifacts))
        .expect("linear report");
    assert_eq!(
        linear_report.status,
        CrossApplicationQualificationStatus::Qualified
    );
    assert!(matches!(
        linear_report.cases[0].comparisons[0].statistics,
        mondrian_renderer::CrossApplicationComparisonStatistics::SceneLinearRec2020 { .. }
    ));

    let pq_profile = float_profile(
        "pq-display",
        ColorReferenceEncoding::Bt2100PqRgbaF32,
        CrossApplicationAccuracyBudget::Bt2100PqDisplay {
            budget: mondrian_renderer::PqHdrDisplayAccuracyBudget::new(0.1, 0.1, 0.1, 0.0),
        },
        Some(203.0),
        Some(1_000.0),
    );
    let pq_pixels = vec![[0.0, 0.0, 0.0, 1.0], [0.5080784, 0.5080784, 0.5080784, 1.0]];
    let pq_artifacts = producers()
        .into_iter()
        .map(|producer| {
            float_artifact(
                "pq-display",
                producer,
                ColorReferenceEncoding::Bt2100PqRgbaF32,
                pq_pixels.clone(),
                Some(203.0),
                Some(1_000.0),
            )
        })
        .collect();
    let pq_report = PreparedCrossApplicationQualification::compile(pq_profile)
        .expect("PQ profile")
        .evaluate(run(pq_artifacts))
        .expect("PQ report");
    assert_eq!(
        pq_report.status,
        CrossApplicationQualificationStatus::Qualified
    );
    assert!(matches!(
        pq_report.cases[0].comparisons[0].statistics,
        mondrian_renderer::CrossApplicationComparisonStatistics::Bt2100PqDisplay { .. }
    ));
}

#[test]
fn missing_vendor_artifacts_are_incomplete_never_skipped_or_qualified() {
    let prepared = PreparedCrossApplicationQualification::compile(profile()).expect("profile");
    let report = prepared
        .evaluate(run(vec![artifact(
            CrossApplicationProducer::Mondrian,
            reference_pixels(),
        )]))
        .expect("incomplete report is still evidence");

    assert_eq!(
        report.status,
        CrossApplicationQualificationStatus::Incomplete
    );
    assert_eq!(report.missing_artifacts.len(), 3);
    assert!(report.cases[0].comparisons.is_empty());
    assert!(report.verify_evidence());
}

#[test]
fn a_pairwise_budget_failure_dominates_missing_evidence() {
    let prepared = PreparedCrossApplicationQualification::compile(profile()).expect("profile");
    let mut changed = reference_pixels();
    changed[0] = 255;
    let report = prepared
        .evaluate(run(vec![
            artifact(CrossApplicationProducer::Mondrian, reference_pixels()),
            artifact(CrossApplicationProducer::Blender, changed),
        ]))
        .expect("failed comparison report");

    assert_eq!(report.status, CrossApplicationQualificationStatus::Failed);
    assert!(!report.cases[0].comparisons[0].within_budget);
    assert_eq!(report.missing_artifacts.len(), 2);
}

#[test]
fn external_producer_cannot_use_mondrian_regression_provenance() {
    let prepared = PreparedCrossApplicationQualification::compile(profile()).expect("profile");
    let mut blender = artifact(CrossApplicationProducer::Blender, reference_pixels());
    blender.frame.descriptor.origin = ColorReferenceOrigin::MondrianRegression;
    let error = prepared
        .evaluate(run(vec![blender]))
        .expect_err("forged external provenance must fail closed");

    assert!(error.to_string().contains("artifact contract mismatch"));
}

#[test]
fn hidden_alignment_or_decoder_color_conversion_is_rejected() {
    let prepared = PreparedCrossApplicationQualification::compile(profile()).expect("profile");
    let mut aligned = artifact(CrossApplicationProducer::Blender, reference_pixels());
    aligned.evidence.automatic_alignment_applied = true;
    assert!(prepared.evaluate(run(vec![aligned])).is_err());

    let mut converted = artifact(CrossApplicationProducer::Blender, reference_pixels());
    converted.evidence.decoder_color_conversion_applied = true;
    assert!(prepared.evaluate(run(vec![converted])).is_err());
}

#[test]
fn unsupported_domain_and_metric_combinations_fail_during_profile_compile() {
    let mut invalid = profile();
    invalid.cases[0].encoding = ColorReferenceEncoding::DisplayP3Rgba8;

    let error = PreparedCrossApplicationQualification::compile(invalid)
        .expect_err("P3 must not be compared with the sRGB metric");
    assert!(error.to_string().contains("incompatible encoding/metric"));
}

#[test]
fn canonical_profile_identity_ignores_declaration_order() {
    let left = PreparedCrossApplicationQualification::compile(profile()).expect("profile");
    let mut reordered = profile();
    reordered.required_producers.reverse();
    reordered.cases[0].required_producers.reverse();
    let right = PreparedCrossApplicationQualification::compile(reordered).expect("profile");

    assert_eq!(left.profile_sha256(), right.profile_sha256());
}

#[test]
fn profile_requires_all_commercial_producers_and_bounded_resources() {
    let mut missing = profile();
    missing.required_producers.pop();
    assert!(PreparedCrossApplicationQualification::compile(missing).is_err());

    let mut unbounded = profile();
    unbounded.limits.max_pixels_per_artifact = u64::MAX;
    assert!(PreparedCrossApplicationQualification::compile(unbounded).is_err());
}

#[test]
fn linear_budget_schema_is_strict_when_embedded_in_a_profile() {
    let budget = CrossApplicationAccuracyBudget::SceneLinearRec2020 {
        budget: LinearRgbaAccuracyBudget {
            rgb: LinearAccuracyBudget::finite(0.001, 0.0001, 0.0005),
            alpha: LinearAccuracyBudget::finite(0.0, 0.0, 0.0),
        },
    };
    let json = serde_json::to_string(&budget).expect("serialize budget");
    let with_unknown = json.replacen('{', "{\"unknown\":true,", 1);

    assert!(serde_json::from_str::<CrossApplicationAccuracyBudget>(&with_unknown).is_err());
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SealedEvidenceManifest {
    schema_version: u32,
    run_id: String,
    source_revision: String,
    source_clean: bool,
    machine_report_sha256: String,
    artifacts: Vec<SealedArtifact>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SealedArtifact {
    evidence: CrossApplicationArtifactEvidence,
    descriptor_path: PathBuf,
    payload_path: PathBuf,
}

fn resolve_from_manifest(manifest_path: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        manifest_path.parent().unwrap_or_else(|| Path::new(".")).join(path)
    }
}

#[test]
#[ignore = "requires sealed local-restricted Blender/Resolve/Premiere evidence"]
fn sealed_cross_application_corpus_qualification() {
    let profile_path = std::env::var_os("MONDRIAN_CROSS_APPLICATION_PROFILE")
        .map(PathBuf::from)
        .expect("MONDRIAN_CROSS_APPLICATION_PROFILE must name the exact runtime profile");
    let evidence_path = std::env::var_os("MONDRIAN_CROSS_APPLICATION_EVIDENCE")
        .map(PathBuf::from)
        .expect("MONDRIAN_CROSS_APPLICATION_EVIDENCE must name the sealed evidence manifest");
    let report_path = std::env::var_os("MONDRIAN_CROSS_APPLICATION_REPORT")
        .map(PathBuf::from)
        .expect("MONDRIAN_CROSS_APPLICATION_REPORT must name a new report path");
    assert!(!report_path.exists(), "report path must not already exist");

    let profile: CrossApplicationQualificationProfile = serde_json::from_slice(
        &std::fs::read(&profile_path).expect("read runtime qualification profile"),
    )
    .expect("parse strict runtime qualification profile");
    let prepared = PreparedCrossApplicationQualification::compile(profile)
        .expect("compile runtime qualification profile");
    let manifest: SealedEvidenceManifest = serde_json::from_slice(
        &std::fs::read(&evidence_path).expect("read sealed evidence manifest"),
    )
    .expect("parse strict sealed evidence manifest");
    assert_eq!(
        manifest.schema_version, 1,
        "evidence schema must be version 1"
    );

    let mut artifacts = Vec::with_capacity(manifest.artifacts.len());
    for artifact in manifest.artifacts {
        let descriptor_path = resolve_from_manifest(&evidence_path, &artifact.descriptor_path);
        let payload_path = resolve_from_manifest(&evidence_path, &artifact.payload_path);
        let descriptor: ColorReferenceDescriptor = serde_json::from_slice(
            &std::fs::read(descriptor_path).expect("read artifact descriptor"),
        )
        .expect("parse strict artifact descriptor");
        let payload = std::fs::read(payload_path).expect("read encoded artifact payload");
        assert_eq!(
            u64::try_from(payload.len()).expect("payload size must fit u64"),
            artifact.evidence.encoded_byte_len,
            "encoded payload byte count must match acquisition evidence"
        );
        let frame = import_external_color_reference_with_limits(
            descriptor,
            &payload,
            &ImageColorReferenceDecoder,
            prepared.import_limits(),
        )
        .expect("strict bounded artifact import");
        artifacts
            .push(CrossApplicationQualificationArtifact { evidence: artifact.evidence, frame });
    }
    let report = prepared
        .evaluate(CrossApplicationQualificationRun {
            run_id: manifest.run_id,
            source_revision: manifest.source_revision,
            source_clean: manifest.source_clean,
            machine_report_sha256: manifest.machine_report_sha256,
            artifacts,
        })
        .expect("evaluate sealed cross-application evidence");
    assert!(report.verify_evidence(), "report digest must self-verify");
    std::fs::write(
        &report_path,
        serde_json::to_vec_pretty(&report).expect("serialize qualification report"),
    )
    .expect("publish qualification report");
    assert_eq!(
        report.status,
        CrossApplicationQualificationStatus::Qualified,
        "sealed qualification requires a complete passing matrix"
    );
}

#[test]
fn checked_in_supervisor_policy_binds_the_stimulus_and_fail_closed_runner() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("renderer crate must be inside the workspace");
    let policy_path =
        repository.join("tests/validation/cross-application-color-qualification.json");
    let policy: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&policy_path).expect("read checked-in supervisor policy"),
    )
    .expect("parse checked-in supervisor policy");
    let stimulus_relative = policy["stimulus"]["path"].as_str().expect("stimulus path");
    let stimulus = std::fs::read(repository.join(stimulus_relative)).expect("read stimulus");
    let actual_stimulus_sha = format!("{:x}", Sha256::digest(&stimulus));

    assert_eq!(policy["schema_version"], 1);
    assert_eq!(policy["execution_policy"], "sealed-required");
    assert_eq!(policy["stimulus"]["sha256"], actual_stimulus_sha);
    assert_eq!(policy["runner"]["serial_cargo_jobs"], 1);
    assert_eq!(policy["acceptance"]["qualified_status_required"], true);
    assert_eq!(policy["acceptance"]["missing_artifacts_forbidden"], true);
    let script = std::fs::read_to_string(
        repository.join("scripts/validation/invoke-cross-application-color-qualification.ps1"),
    )
    .expect("read checked-in supervisor");
    assert!(script.contains("--exact"));
    assert!(script.contains("--ignored"));
    assert!(script.contains("-j"));
    assert!(script.contains("source_clean"));
    assert!(script.contains("qualified"));
    assert!(!script.contains("return 0"));
}

#[test]
fn local_blender_premiere_scope_is_explicit_and_never_weakens_legacy_matrix() {
    let mut local = profile();
    local
        .required_producers
        .retain(|entry| entry.producer != CrossApplicationProducer::DaVinciResolve);
    for case in &mut local.cases {
        case.required_producers
            .retain(|producer| *producer != CrossApplicationProducer::DaVinciResolve);
    }
    assert!(PreparedCrossApplicationQualification::compile(local.clone()).is_err());
    local.producer_scope = mondrian_renderer::CrossApplicationProducerScope::BlenderAndPremiere;
    assert!(PreparedCrossApplicationQualification::compile(local.clone()).is_err());
    local.schema_version = 2;
    assert!(PreparedCrossApplicationQualification::compile(local.clone()).is_ok());
    local.required_producers.pop();
    assert!(PreparedCrossApplicationQualification::compile(local).is_err());
}
