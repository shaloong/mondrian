//! Sealed cross-application color-reference qualification.
//!
//! This Module compiles one strict qualification profile, validates acquisition
//! evidence, dispatches comparisons in the declared color domain, and produces
//! one deterministic report. Vendor launch and file decoding remain external
//! Adapters; neither is allowed to reinterpret the qualification verdict.

use crate::{
    compare_linear_rgba, compare_pq_hdr_display_rgba, compare_srgb_display_rgba8, ColorFrameAlpha,
    ColorReferenceEncoding, ColorReferenceFrame, ColorReferenceOrigin, ColorReferencePayloadFormat,
    ColorReferencePixels, LinearRgbaAccuracyBudget, LinearRgbaAccuracyReport,
    PqHdrDisplayAccuracyBudget, PqHdrDisplayAccuracyReport, SrgbDisplayAccuracyBudget,
    SrgbDisplayAccuracyReport,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

const QUALIFICATION_SCHEMA_VERSION: u32 = 1;
const REPORT_SCHEMA_VERSION: u32 = 1;
const HARD_MAX_CASES: usize = 64;
const HARD_MAX_ARTIFACTS: usize = 256;
const HARD_MAX_PIXELS_PER_ARTIFACT: u64 = 33_554_432;
const HARD_MAX_ENCODED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Producer identities admitted by the commercial cross-application matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossApplicationProducer {
    /// Mondrian's ordinary product rendering/export path.
    Mondrian,
    /// Blender.
    Blender,
    /// Blackmagic Design DaVinci Resolve.
    DaVinciResolve,
    /// Adobe Premiere Pro.
    AdobePremierePro,
}

impl CrossApplicationProducer {
    fn descriptor_producer(self) -> &'static str {
        match self {
            Self::Mondrian => "Mondrian",
            Self::Blender => "Blender",
            Self::DaVinciResolve => "DaVinci Resolve",
            Self::AdobePremierePro => "Adobe Premiere Pro",
        }
    }

    fn expected_origin(self) -> ColorReferenceOrigin {
        match self {
            Self::Mondrian => ColorReferenceOrigin::MondrianRegression,
            Self::Blender | Self::DaVinciResolve | Self::AdobePremierePro => {
                ColorReferenceOrigin::IndependentApplication
            }
        }
    }
}

/// Exact producer identity frozen by a qualification profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossApplicationProducerRequirement {
    /// Stable producer family.
    pub producer: CrossApplicationProducer,
    /// Exact product version, never a range or rolling channel.
    pub exact_version: String,
    /// Exact product build identity.
    pub exact_build: String,
}

/// Exact temporal location of one reference frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossApplicationFrameCoordinate {
    /// Rational frame-rate numerator.
    pub rate_numerator: u32,
    /// Rational frame-rate denominator.
    pub rate_denominator: u32,
    /// Zero-based frame index; display timecode is not frame identity.
    pub frame_index: i64,
}

/// Decoded row/origin orientation; qualification never auto-flips frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossApplicationPixelOrientation {
    /// First decoded row is the top image row.
    TopLeft,
    /// First decoded row is the bottom image row.
    BottomLeft,
}

/// Domain-specific accuracy policy for one qualification case.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "metric", rename_all = "snake_case", deny_unknown_fields)]
pub enum CrossApplicationAccuracyBudget {
    /// Numeric scene-linear Rec.2020 RGBA comparison.
    SceneLinearRec2020 {
        /// Independent RGB and alpha limits.
        budget: LinearRgbaAccuracyBudget,
    },
    /// Perceptual sRGB display comparison using CIEDE2000.
    SrgbDisplay {
        /// CIEDE2000 and encoded-alpha limits.
        budget: SrgbDisplayAccuracyBudget,
    },
    /// Perceptual BT.2100 PQ comparison using Delta E ITP.
    Bt2100PqDisplay {
        /// Delta E ITP and alpha limits.
        budget: PqHdrDisplayAccuracyBudget,
    },
}

impl CrossApplicationAccuracyBudget {
    fn encoding(self) -> ColorReferenceEncoding {
        match self {
            Self::SceneLinearRec2020 { .. } => ColorReferenceEncoding::SceneLinearRec2020RgbaF32,
            Self::SrgbDisplay { .. } => ColorReferenceEncoding::SrgbDisplayRgba8,
            Self::Bt2100PqDisplay { .. } => ColorReferenceEncoding::Bt2100PqRgbaF32,
        }
    }
}

/// One fixed stimulus/output case in a qualification profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossApplicationQualificationCase {
    /// Stable case identity.
    pub case_id: String,
    /// Producers required for this case. Mondrian and at least one external app are mandatory.
    pub required_producers: Vec<CrossApplicationProducer>,
    /// Encoded payload format supplied to the reference importer.
    pub payload_format: ColorReferencePayloadFormat,
    /// Exact decoded color-domain interpretation.
    pub encoding: ColorReferenceEncoding,
    /// Expected raster width.
    pub width: u32,
    /// Expected raster height.
    pub height: u32,
    /// Expected alpha interpretation.
    pub alpha: ColorFrameAlpha,
    /// Expected decoded row/origin orientation.
    pub orientation: CrossApplicationPixelOrientation,
    /// Pixel-aspect numerator.
    pub pixel_aspect_numerator: u32,
    /// Pixel-aspect denominator.
    pub pixel_aspect_denominator: u32,
    /// Expected display reference white, when applicable.
    pub reference_white_nits: Option<f32>,
    /// Expected nominal display peak, when applicable.
    pub nominal_peak_nits: Option<f32>,
    /// Exact frame location.
    pub frame: CrossApplicationFrameCoordinate,
    /// Metric and limits; this must match `encoding` exactly.
    pub accuracy: CrossApplicationAccuracyBudget,
}

/// Bounded resource policy for a qualification run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossApplicationQualificationLimits {
    /// Largest number of cases admitted by the profile.
    pub max_cases: u16,
    /// Largest number of artifacts admitted in one run.
    pub max_artifacts: u16,
    /// Largest declared raster admitted per artifact.
    pub max_pixels_per_artifact: u64,
    /// Largest encoded payload admitted per artifact before decoding.
    pub max_encoded_bytes_per_artifact: u64,
}

/// Strict authoring profile compiled before any vendor evidence is trusted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossApplicationQualificationProfile {
    /// Contract schema version. Version 1 is currently required.
    pub schema_version: u32,
    /// Stable profile identity.
    pub qualification_id: String,
    /// Immutable profile edition.
    pub edition: String,
    /// SHA-256 of the fixed analytic stimulus manifest.
    pub stimulus_sha256: String,
    /// Exact producer versions/builds required by the profile.
    pub required_producers: Vec<CrossApplicationProducerRequirement>,
    /// Required case matrix.
    pub cases: Vec<CrossApplicationQualificationCase>,
    /// Explicit run resource bounds.
    pub limits: CrossApplicationQualificationLimits,
}

/// Validated, canonical qualification plan.
#[derive(Debug, Clone)]
pub struct PreparedCrossApplicationQualification {
    profile: CrossApplicationQualificationProfile,
    profile_sha256: String,
}

impl PreparedCrossApplicationQualification {
    /// Validate and canonicalize one profile before acquiring artifacts.
    pub fn compile(
        mut profile: CrossApplicationQualificationProfile,
    ) -> Result<Self, CrossApplicationQualificationError> {
        validate_profile(&profile)?;
        profile.required_producers.sort_by_key(|requirement| requirement.producer);
        for case in &mut profile.cases {
            case.required_producers.sort();
        }
        profile.cases.sort_by(|left, right| left.case_id.cmp(&right.case_id));
        let profile_sha256 = digest_serializable(&profile)?;
        Ok(Self { profile, profile_sha256 })
    }

    /// Canonical SHA-256 of the validated profile.
    pub fn profile_sha256(&self) -> &str {
        &self.profile_sha256
    }

    /// Pre-decode admission limits that every qualification Adapter must apply.
    pub fn import_limits(&self) -> crate::ColorReferenceImportLimits {
        crate::ColorReferenceImportLimits::new(
            usize::try_from(self.profile.limits.max_encoded_bytes_per_artifact)
                .unwrap_or(usize::MAX),
            usize::try_from(self.profile.limits.max_pixels_per_artifact).unwrap_or(usize::MAX),
        )
    }

    /// Evaluate one non-spliceable acquisition run.
    pub fn evaluate(
        &self,
        run: CrossApplicationQualificationRun,
    ) -> Result<CrossApplicationQualificationReport, CrossApplicationQualificationError> {
        let CrossApplicationQualificationRun {
            run_id,
            source_revision,
            source_clean,
            machine_report_sha256,
            artifacts: run_artifacts,
        } = run;
        validate_identity("run_id", &run_id)?;
        validate_source_revision(&source_revision)?;
        validate_sha256("machine_report_sha256", &machine_report_sha256)?;
        if !source_clean {
            return Err(CrossApplicationQualificationError::DirtySource);
        }
        if run_artifacts.len() > usize::from(self.profile.limits.max_artifacts) {
            return Err(CrossApplicationQualificationError::ArtifactLimitExceeded {
                actual: run_artifacts.len(),
                maximum: usize::from(self.profile.limits.max_artifacts),
            });
        }

        let requirements = self.requirements();
        let mut artifacts = BTreeMap::new();
        for artifact in run_artifacts {
            self.validate_artifact(&run_id, &artifact)?;
            let key = (
                artifact.evidence.case_id.clone(),
                artifact.evidence.producer,
            );
            if artifacts.insert(key.clone(), artifact).is_some() {
                return Err(CrossApplicationQualificationError::DuplicateArtifact {
                    case_id: key.0,
                    producer: key.1,
                });
            }
        }
        for key in artifacts.keys() {
            if !requirements.contains(key) {
                return Err(CrossApplicationQualificationError::UnexpectedArtifact {
                    case_id: key.0.clone(),
                    producer: key.1,
                });
            }
        }

        let missing_artifacts = requirements
            .difference(&artifacts.keys().cloned().collect())
            .map(|(case_id, producer)| CrossApplicationMissingArtifact {
                case_id: case_id.clone(),
                producer: *producer,
            })
            .collect::<Vec<_>>();
        let mut case_reports = Vec::with_capacity(self.profile.cases.len());
        let mut any_failure = false;
        for case in &self.profile.cases {
            let available = case
                .required_producers
                .iter()
                .filter_map(|producer| {
                    artifacts
                        .get(&(case.case_id.clone(), *producer))
                        .map(|artifact| (*producer, artifact))
                })
                .collect::<Vec<_>>();
            let mut comparisons = Vec::new();
            for left_index in 0..available.len() {
                for right_index in (left_index + 1)..available.len() {
                    let (left_producer, left) = available[left_index];
                    let (right_producer, right) = available[right_index];
                    let statistics = compare_case(case, &left.frame, &right.frame)?;
                    let within_budget = statistics.within_budget();
                    any_failure |= !within_budget;
                    comparisons.push(CrossApplicationPairComparison {
                        left_producer,
                        right_producer,
                        statistics,
                        within_budget,
                    });
                }
            }
            case_reports.push(CrossApplicationCaseReport {
                case_id: case.case_id.clone(),
                present_producers: available.iter().map(|(producer, _)| *producer).collect(),
                comparisons,
            });
        }
        let status = if any_failure {
            CrossApplicationQualificationStatus::Failed
        } else if missing_artifacts.is_empty() {
            CrossApplicationQualificationStatus::Qualified
        } else {
            CrossApplicationQualificationStatus::Incomplete
        };
        let artifact_evidence =
            artifacts.values().map(|artifact| artifact.evidence.clone()).collect();
        let mut report = CrossApplicationQualificationReport {
            schema_version: REPORT_SCHEMA_VERSION,
            qualification_id: self.profile.qualification_id.clone(),
            edition: self.profile.edition.clone(),
            profile_sha256: self.profile_sha256.clone(),
            stimulus_sha256: self.profile.stimulus_sha256.clone(),
            run_id,
            source_revision,
            machine_report_sha256,
            status,
            missing_artifacts,
            artifact_evidence,
            cases: case_reports,
            evidence_sha256: String::new(),
        };
        report.evidence_sha256 = report_digest(&report)?;
        Ok(report)
    }

    fn requirements(&self) -> BTreeSet<(String, CrossApplicationProducer)> {
        self.profile
            .cases
            .iter()
            .flat_map(|case| {
                case.required_producers.iter().map(|producer| (case.case_id.clone(), *producer))
            })
            .collect()
    }

    fn validate_artifact(
        &self,
        run_id: &str,
        artifact: &CrossApplicationQualificationArtifact,
    ) -> Result<(), CrossApplicationQualificationError> {
        let evidence = &artifact.evidence;
        validate_identity("artifact.run_id", &evidence.run_id)?;
        validate_identity("artifact.case_id", &evidence.case_id)?;
        for (field, value) in [
            ("producer_version", evidence.producer_version.as_str()),
            ("producer_build", evidence.producer_build.as_str()),
            ("os_identity", evidence.os_identity.as_str()),
            ("adapter_name", evidence.adapter_name.as_str()),
            ("adapter_version", evidence.adapter_version.as_str()),
        ] {
            validate_identity(field, value)?;
        }
        for (field, value) in [
            ("executable_sha256", evidence.executable_sha256.as_str()),
            (
                "native_project_sha256",
                evidence.native_project_sha256.as_str(),
            ),
            (
                "render_settings_sha256",
                evidence.render_settings_sha256.as_str(),
            ),
            ("adapter_sha256", evidence.adapter_sha256.as_str()),
            (
                "decoded_metadata_sha256",
                evidence.decoded_metadata_sha256.as_str(),
            ),
            (
                "operator_attestation_sha256",
                evidence.operator_attestation_sha256.as_str(),
            ),
        ] {
            validate_sha256(field, value)?;
        }
        if evidence.run_id != run_id {
            return Err(CrossApplicationQualificationError::MixedRun {
                expected: run_id.to_owned(),
                actual: evidence.run_id.clone(),
            });
        }
        if evidence.encoded_byte_len > self.profile.limits.max_encoded_bytes_per_artifact {
            return Err(
                CrossApplicationQualificationError::EncodedByteLimitExceeded {
                    actual: evidence.encoded_byte_len,
                    maximum: self.profile.limits.max_encoded_bytes_per_artifact,
                },
            );
        }
        let case = self
            .profile
            .cases
            .iter()
            .find(|case| case.case_id == evidence.case_id)
            .ok_or_else(|| CrossApplicationQualificationError::UnknownCase {
                case_id: evidence.case_id.clone(),
            })?;
        let producer = self
            .profile
            .required_producers
            .iter()
            .find(|requirement| requirement.producer == evidence.producer)
            .ok_or(CrossApplicationQualificationError::UnknownProducer {
                producer: evidence.producer,
            })?;
        if producer.exact_version != evidence.producer_version
            || producer.exact_build != evidence.producer_build
        {
            return Err(
                CrossApplicationQualificationError::ProducerIdentityMismatch {
                    producer: evidence.producer,
                },
            );
        }
        if evidence.frame != case.frame {
            return Err(
                CrossApplicationQualificationError::FrameCoordinateMismatch {
                    case_id: case.case_id.clone(),
                },
            );
        }
        let descriptor = &artifact.frame.descriptor;
        let descriptor_matches = descriptor.producer == evidence.producer.descriptor_producer()
            && descriptor.producer_version == evidence.producer_version
            && descriptor.origin == evidence.producer.expected_origin()
            && descriptor.source_artifact_sha256 == self.profile.stimulus_sha256
            && descriptor.content_sha256 == evidence.artifact_sha256
            && descriptor.payload_format == case.payload_format
            && descriptor.encoding == case.encoding
            && descriptor.width == case.width
            && descriptor.height == case.height
            && descriptor.alpha == case.alpha
            && descriptor.reference_white_nits == case.reference_white_nits
            && descriptor.nominal_peak_nits == case.nominal_peak_nits;
        validate_sha256("artifact_sha256", &evidence.artifact_sha256)?;
        if !descriptor_matches {
            return Err(
                CrossApplicationQualificationError::ArtifactContractMismatch {
                    case_id: case.case_id.clone(),
                    producer: evidence.producer,
                },
            );
        }
        if evidence.orientation != case.orientation
            || evidence.pixel_aspect_numerator != case.pixel_aspect_numerator
            || evidence.pixel_aspect_denominator != case.pixel_aspect_denominator
            || evidence.automatic_alignment_applied
            || evidence.decoder_color_conversion_applied
        {
            return Err(
                CrossApplicationQualificationError::ArtifactContractMismatch {
                    case_id: case.case_id.clone(),
                    producer: evidence.producer,
                },
            );
        }
        let pixels = u64::from(descriptor.width) * u64::from(descriptor.height);
        if pixels > self.profile.limits.max_pixels_per_artifact {
            return Err(CrossApplicationQualificationError::PixelLimitExceeded {
                actual: pixels,
                maximum: self.profile.limits.max_pixels_per_artifact,
            });
        }
        Ok(())
    }
}

/// Acquisition evidence attached to exactly one decoded artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossApplicationArtifactEvidence {
    /// Non-spliceable run identity.
    pub run_id: String,
    /// Profile case identity.
    pub case_id: String,
    /// Producer family.
    pub producer: CrossApplicationProducer,
    /// Observed exact producer version.
    pub producer_version: String,
    /// Observed exact producer build.
    pub producer_build: String,
    /// OS/build identity used for acquisition.
    pub os_identity: String,
    /// SHA-256 of the producer executable or signed installation inventory.
    pub executable_sha256: String,
    /// SHA-256 of the producer-native project.
    pub native_project_sha256: String,
    /// SHA-256 of the complete actual render/export settings dump.
    pub render_settings_sha256: String,
    /// Acquisition Adapter identity.
    pub adapter_name: String,
    /// Acquisition Adapter version.
    pub adapter_version: String,
    /// SHA-256 of the acquisition Adapter source/package.
    pub adapter_sha256: String,
    /// SHA-256 of decoder/channel/sample/metadata evidence.
    pub decoded_metadata_sha256: String,
    /// SHA-256 of the operator attestation; screenshots alone are insufficient.
    pub operator_attestation_sha256: String,
    /// SHA-256 of the encoded artifact, equal to the frame descriptor content hash.
    pub artifact_sha256: String,
    /// Encoded artifact size observed before bounded decode.
    pub encoded_byte_len: u64,
    /// Exact frame location exported by the producer.
    pub frame: CrossApplicationFrameCoordinate,
    /// Actual decoded row/origin orientation.
    pub orientation: CrossApplicationPixelOrientation,
    /// Actual pixel-aspect numerator.
    pub pixel_aspect_numerator: u32,
    /// Actual pixel-aspect denominator.
    pub pixel_aspect_denominator: u32,
    /// Whether acquisition resized, cropped, aligned, or otherwise registered pixels.
    pub automatic_alignment_applied: bool,
    /// Whether the decoder applied any ICC/transfer/primary/range conversion.
    pub decoder_color_conversion_applied: bool,
}

/// One admitted artifact and its acquisition evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct CrossApplicationQualificationArtifact {
    /// Acquisition evidence.
    pub evidence: CrossApplicationArtifactEvidence,
    /// Strictly imported decoded frame.
    pub frame: ColorReferenceFrame,
}

/// One complete acquisition run. Artifacts from different runs cannot be merged.
#[derive(Debug, Clone, PartialEq)]
pub struct CrossApplicationQualificationRun {
    /// Non-empty run identity shared by every artifact.
    pub run_id: String,
    /// Clean Mondrian source revision used for the run.
    pub source_revision: String,
    /// Whether the source checkout was clean at acquisition time.
    pub source_clean: bool,
    /// SHA-256 of the machine/OS/GPU/driver inventory report.
    pub machine_report_sha256: String,
    /// Admitted artifacts, in any order.
    pub artifacts: Vec<CrossApplicationQualificationArtifact>,
}

/// Missing required matrix cell in an incomplete run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossApplicationMissingArtifact {
    /// Missing case identity.
    pub case_id: String,
    /// Missing producer.
    pub producer: CrossApplicationProducer,
}

/// Domain-specific statistics retained in the sealed report.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(tag = "metric", rename_all = "snake_case")]
pub enum CrossApplicationComparisonStatistics {
    /// Scene-linear numeric statistics.
    SceneLinearRec2020 {
        /// Full scene-linear numeric report.
        report: LinearRgbaAccuracyReport,
    },
    /// sRGB CIEDE2000 statistics.
    SrgbDisplay {
        /// Full sRGB perceptual report.
        report: SrgbDisplayAccuracyReport,
    },
    /// PQ Delta E ITP statistics.
    Bt2100PqDisplay {
        /// Full BT.2100 PQ perceptual report.
        report: PqHdrDisplayAccuracyReport,
    },
}

impl CrossApplicationComparisonStatistics {
    fn within_budget(self) -> bool {
        match self {
            Self::SceneLinearRec2020 { report } => report.within_budget,
            Self::SrgbDisplay { report } => report.within_budget,
            Self::Bt2100PqDisplay { report } => report.within_budget,
        }
    }
}

/// One pairwise comparison. Cross-app agreement is not an absolute public-spec oracle.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CrossApplicationPairComparison {
    /// Canonically ordered left producer.
    pub left_producer: CrossApplicationProducer,
    /// Canonically ordered right producer.
    pub right_producer: CrossApplicationProducer,
    /// Full metric distribution.
    pub statistics: CrossApplicationComparisonStatistics,
    /// Whether every declared limit passed.
    pub within_budget: bool,
}

/// Case-level matrix result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CrossApplicationCaseReport {
    /// Case identity.
    pub case_id: String,
    /// Producers present in this run.
    pub present_producers: Vec<CrossApplicationProducer>,
    /// Complete pairwise comparisons among present producers.
    pub comparisons: Vec<CrossApplicationPairComparison>,
}

/// Stable qualification verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossApplicationQualificationStatus {
    /// Every required artifact was present and every comparison passed.
    Qualified,
    /// One or more comparisons exceeded the profile budget.
    Failed,
    /// Required external evidence was absent; this is never a pass or skip.
    Incomplete,
}

/// Deterministic report emitted by the qualification Module.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CrossApplicationQualificationReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Qualification profile identity.
    pub qualification_id: String,
    /// Qualification profile edition.
    pub edition: String,
    /// Canonical profile SHA-256.
    pub profile_sha256: String,
    /// Fixed stimulus manifest SHA-256.
    pub stimulus_sha256: String,
    /// Non-spliceable run identity.
    pub run_id: String,
    /// Clean Mondrian source revision.
    pub source_revision: String,
    /// Machine inventory SHA-256.
    pub machine_report_sha256: String,
    /// Overall verdict.
    pub status: CrossApplicationQualificationStatus,
    /// Required matrix cells absent from the run.
    pub missing_artifacts: Vec<CrossApplicationMissingArtifact>,
    /// Canonically ordered acquisition evidence for every present artifact.
    pub artifact_evidence: Vec<CrossApplicationArtifactEvidence>,
    /// Case and pairwise results in canonical order.
    pub cases: Vec<CrossApplicationCaseReport>,
    /// SHA-256 over every preceding report field.
    pub evidence_sha256: String,
}

impl CrossApplicationQualificationReport {
    /// Verify the report's deterministic evidence digest.
    pub fn verify_evidence(&self) -> bool {
        report_digest(self).is_ok_and(|digest| digest == self.evidence_sha256)
    }
}

/// Failure to compile or evaluate a cross-application qualification.
#[derive(Debug, Error)]
pub enum CrossApplicationQualificationError {
    /// Only schema version 1 is accepted.
    #[error("unsupported cross-application qualification schema {actual}; expected 1")]
    UnsupportedSchema {
        /// Observed schema version.
        actual: u32,
    },
    /// An identity field was empty or a placeholder.
    #[error("invalid cross-application qualification identity field '{field}'")]
    InvalidIdentity {
        /// Stable invalid field name.
        field: &'static str,
    },
    /// A digest was not lowercase hexadecimal SHA-256.
    #[error("cross-application qualification field '{field}' must be a lowercase SHA-256")]
    InvalidSha256 {
        /// Stable invalid digest field name.
        field: &'static str,
    },
    /// The clean Git source revision was not lowercase 40-hex SHA-1.
    #[error("cross-application qualification source revision must be a lowercase 40-hex Git SHA")]
    InvalidSourceRevision,
    /// Producer requirements must contain each commercial matrix producer exactly once.
    #[error("qualification profile must require Mondrian, Blender, DaVinci Resolve, and Adobe Premiere Pro exactly once")]
    InvalidProducerClosure,
    /// Case identity was duplicated.
    #[error("duplicate qualification case '{case_id}'")]
    DuplicateCase {
        /// Duplicated case identity.
        case_id: String,
    },
    /// A case producer was duplicated or not declared globally.
    #[error("invalid producer closure for qualification case '{case_id}'")]
    InvalidCaseProducerClosure {
        /// Invalid case identity.
        case_id: String,
    },
    /// A case metric does not match its declared encoding.
    #[error("qualification case '{case_id}' has an incompatible encoding/metric pair")]
    IncompatibleMetric {
        /// Case with incompatible color semantics.
        case_id: String,
    },
    /// A case extent or frame-rate contract was invalid.
    #[error("qualification case '{case_id}' has an invalid extent or frame coordinate")]
    InvalidCaseGeometry {
        /// Case with invalid geometry/time.
        case_id: String,
    },
    /// Profile resource limits were zero or exceeded hard safety caps.
    #[error("qualification profile resource limits are invalid")]
    InvalidResourceLimits,
    /// Case count exceeded the configured limit.
    #[error("qualification profile has {actual} cases, exceeding limit {maximum}")]
    CaseLimitExceeded {
        /// Observed case count.
        actual: usize,
        /// Configured maximum case count.
        maximum: usize,
    },
    /// Run artifact count exceeded the configured limit.
    #[error("qualification run has {actual} artifacts, exceeding limit {maximum}")]
    ArtifactLimitExceeded {
        /// Observed artifact count.
        actual: usize,
        /// Configured maximum artifact count.
        maximum: usize,
    },
    /// Encoded artifact exceeded the configured byte limit.
    #[error("qualification artifact has {actual} encoded bytes, exceeding limit {maximum}")]
    EncodedByteLimitExceeded {
        /// Observed encoded byte count.
        actual: u64,
        /// Configured maximum encoded byte count.
        maximum: u64,
    },
    /// Decoded raster exceeded the configured pixel limit.
    #[error("qualification artifact has {actual} pixels, exceeding limit {maximum}")]
    PixelLimitExceeded {
        /// Observed pixel count.
        actual: u64,
        /// Configured maximum pixel count.
        maximum: u64,
    },
    /// Qualification requires a clean source checkout.
    #[error("cross-application qualification cannot run against a dirty source checkout")]
    DirtySource,
    /// An artifact came from another run.
    #[error("artifact run identity '{actual}' does not match qualification run '{expected}'")]
    MixedRun {
        /// Qualification run identity.
        expected: String,
        /// Artifact run identity.
        actual: String,
    },
    /// The artifact case is unknown.
    #[error("artifact references unknown qualification case '{case_id}'")]
    UnknownCase {
        /// Unknown case identity.
        case_id: String,
    },
    /// The artifact producer is not in the profile.
    #[error("artifact producer {producer:?} is not in the qualification profile")]
    UnknownProducer {
        /// Unknown producer.
        producer: CrossApplicationProducer,
    },
    /// The exact producer version/build did not match the profile.
    #[error("artifact producer identity does not match the frozen profile for {producer:?}")]
    ProducerIdentityMismatch {
        /// Producer whose exact version/build differed.
        producer: CrossApplicationProducer,
    },
    /// Duplicate matrix cell.
    #[error("duplicate artifact for case '{case_id}' and producer {producer:?}")]
    DuplicateArtifact {
        /// Duplicated case identity.
        case_id: String,
        /// Duplicated producer.
        producer: CrossApplicationProducer,
    },
    /// Artifact did not belong to the declared case matrix.
    #[error("unexpected artifact for case '{case_id}' and producer {producer:?}")]
    UnexpectedArtifact {
        /// Unexpected case identity.
        case_id: String,
        /// Unexpected producer.
        producer: CrossApplicationProducer,
    },
    /// The exported frame location did not match the profile.
    #[error("artifact frame coordinate does not match qualification case '{case_id}'")]
    FrameCoordinateMismatch {
        /// Case whose frame coordinate differed.
        case_id: String,
    },
    /// Imported frame semantics or provenance did not match evidence/profile.
    #[error("artifact contract mismatch for case '{case_id}' and producer {producer:?}")]
    ArtifactContractMismatch {
        /// Case whose artifact contract differed.
        case_id: String,
        /// Producer whose artifact contract differed.
        producer: CrossApplicationProducer,
    },
    /// Imported pixel storage was incompatible with the case metric.
    #[error("artifact pixel storage is incompatible with qualification case '{case_id}'")]
    PixelStorageMismatch {
        /// Case with incompatible decoded storage.
        case_id: String,
    },
    /// A local accuracy primitive rejected the comparison.
    #[error("qualification comparison failed for case '{case_id}': {message}")]
    Accuracy {
        /// Case whose accuracy comparison was rejected.
        case_id: String,
        /// Stable underlying diagnostic text.
        message: String,
    },
    /// Deterministic profile/report serialization failed.
    #[error("qualification evidence serialization failed: {message}")]
    Serialization {
        /// Serialization diagnostic.
        message: String,
    },
}

fn validate_profile(
    profile: &CrossApplicationQualificationProfile,
) -> Result<(), CrossApplicationQualificationError> {
    if profile.schema_version != QUALIFICATION_SCHEMA_VERSION {
        return Err(CrossApplicationQualificationError::UnsupportedSchema {
            actual: profile.schema_version,
        });
    }
    validate_identity("qualification_id", &profile.qualification_id)?;
    validate_identity("edition", &profile.edition)?;
    validate_sha256("stimulus_sha256", &profile.stimulus_sha256)?;
    let expected = BTreeSet::from([
        CrossApplicationProducer::Mondrian,
        CrossApplicationProducer::Blender,
        CrossApplicationProducer::DaVinciResolve,
        CrossApplicationProducer::AdobePremierePro,
    ]);
    let actual = profile
        .required_producers
        .iter()
        .map(|requirement| requirement.producer)
        .collect::<BTreeSet<_>>();
    if actual != expected || actual.len() != profile.required_producers.len() {
        return Err(CrossApplicationQualificationError::InvalidProducerClosure);
    }
    for requirement in &profile.required_producers {
        validate_identity("exact_version", &requirement.exact_version)?;
        validate_identity("exact_build", &requirement.exact_build)?;
    }
    let limits = profile.limits;
    if limits.max_cases == 0
        || limits.max_artifacts == 0
        || limits.max_pixels_per_artifact == 0
        || limits.max_encoded_bytes_per_artifact == 0
        || usize::from(limits.max_cases) > HARD_MAX_CASES
        || usize::from(limits.max_artifacts) > HARD_MAX_ARTIFACTS
        || limits.max_pixels_per_artifact > HARD_MAX_PIXELS_PER_ARTIFACT
        || limits.max_encoded_bytes_per_artifact > HARD_MAX_ENCODED_BYTES
    {
        return Err(CrossApplicationQualificationError::InvalidResourceLimits);
    }
    if profile.cases.is_empty() || profile.cases.len() > usize::from(limits.max_cases) {
        return Err(CrossApplicationQualificationError::CaseLimitExceeded {
            actual: profile.cases.len(),
            maximum: usize::from(limits.max_cases),
        });
    }
    let mut case_ids = BTreeSet::new();
    let mut covered = BTreeSet::new();
    for case in &profile.cases {
        validate_identity("case_id", &case.case_id)?;
        if !case_ids.insert(case.case_id.clone()) {
            return Err(CrossApplicationQualificationError::DuplicateCase {
                case_id: case.case_id.clone(),
            });
        }
        let case_producers = case.required_producers.iter().copied().collect::<BTreeSet<_>>();
        if case_producers.len() != case.required_producers.len()
            || !case_producers.contains(&CrossApplicationProducer::Mondrian)
            || case_producers.len() < 2
            || !case_producers.is_subset(&expected)
        {
            return Err(
                CrossApplicationQualificationError::InvalidCaseProducerClosure {
                    case_id: case.case_id.clone(),
                },
            );
        }
        covered.extend(case_producers);
        if case.encoding != case.accuracy.encoding() {
            return Err(CrossApplicationQualificationError::IncompatibleMetric {
                case_id: case.case_id.clone(),
            });
        }
        let payload_matches = match case.encoding {
            ColorReferenceEncoding::SrgbDisplayRgba8 => {
                case.payload_format == ColorReferencePayloadFormat::Png
            }
            ColorReferenceEncoding::SceneLinearRec2020RgbaF32
            | ColorReferenceEncoding::Bt2100PqRgbaF32 => matches!(
                case.payload_format,
                ColorReferencePayloadFormat::OpenExr | ColorReferencePayloadFormat::JsonFloat
            ),
            ColorReferenceEncoding::DisplayP3Rgba8
            | ColorReferenceEncoding::Bt2100HlgRgbaF32
            | ColorReferenceEncoding::CieLabD50F32 => false,
        };
        if !payload_matches || !accuracy_budget_is_valid(case.accuracy) {
            return Err(CrossApplicationQualificationError::IncompatibleMetric {
                case_id: case.case_id.clone(),
            });
        }
        let luminance_matches = match case.encoding {
            ColorReferenceEncoding::Bt2100PqRgbaF32 => {
                match (case.reference_white_nits, case.nominal_peak_nits) {
                    (Some(white), Some(peak)) => {
                        white.is_finite() && peak.is_finite() && white > 0.0 && peak >= white
                    }
                    _ => false,
                }
            }
            _ => case.reference_white_nits.is_none() && case.nominal_peak_nits.is_none(),
        };
        if !luminance_matches {
            return Err(CrossApplicationQualificationError::IncompatibleMetric {
                case_id: case.case_id.clone(),
            });
        }
        let pixels = u64::from(case.width) * u64::from(case.height);
        if case.width == 0
            || case.height == 0
            || case.frame.rate_numerator == 0
            || case.frame.rate_denominator == 0
            || case.pixel_aspect_numerator == 0
            || case.pixel_aspect_denominator == 0
            || pixels > limits.max_pixels_per_artifact
        {
            return Err(CrossApplicationQualificationError::InvalidCaseGeometry {
                case_id: case.case_id.clone(),
            });
        }
    }
    if covered != expected {
        return Err(CrossApplicationQualificationError::InvalidProducerClosure);
    }
    let required_artifacts =
        profile.cases.iter().map(|case| case.required_producers.len()).sum::<usize>();
    if required_artifacts > usize::from(limits.max_artifacts) {
        return Err(CrossApplicationQualificationError::ArtifactLimitExceeded {
            actual: required_artifacts,
            maximum: usize::from(limits.max_artifacts),
        });
    }
    Ok(())
}

fn accuracy_budget_is_valid(budget: CrossApplicationAccuracyBudget) -> bool {
    let finite_non_negative =
        |values: &[f64]| values.iter().all(|value| value.is_finite() && *value >= 0.0);
    match budget {
        CrossApplicationAccuracyBudget::SceneLinearRec2020 { budget } => finite_non_negative(&[
            budget.rgb.max_absolute_error,
            budget.rgb.max_root_mean_square_error,
            budget.rgb.max_percentile_99_absolute_error,
            budget.alpha.max_absolute_error,
            budget.alpha.max_root_mean_square_error,
            budget.alpha.max_percentile_99_absolute_error,
        ]),
        CrossApplicationAccuracyBudget::SrgbDisplay { budget } => finite_non_negative(&[
            budget.max_delta_e_2000,
            budget.max_mean_delta_e_2000,
            budget.max_percentile_99_delta_e_2000,
        ]),
        CrossApplicationAccuracyBudget::Bt2100PqDisplay { budget } => finite_non_negative(&[
            budget.max_delta_e_itp,
            budget.max_mean_delta_e_itp,
            budget.max_percentile_99_delta_e_itp,
            budget.max_alpha_absolute_error,
        ]),
    }
}

fn compare_case(
    case: &CrossApplicationQualificationCase,
    left: &ColorReferenceFrame,
    right: &ColorReferenceFrame,
) -> Result<CrossApplicationComparisonStatistics, CrossApplicationQualificationError> {
    let storage_error = || CrossApplicationQualificationError::PixelStorageMismatch {
        case_id: case.case_id.clone(),
    };
    match case.accuracy {
        CrossApplicationAccuracyBudget::SceneLinearRec2020 { budget } => {
            let (ColorReferencePixels::RgbaF32(left), ColorReferencePixels::RgbaF32(right)) =
                (&left.pixels, &right.pixels)
            else {
                return Err(storage_error());
            };
            compare_linear_rgba(left, right, budget)
                .map(|report| CrossApplicationComparisonStatistics::SceneLinearRec2020 { report })
                .map_err(|error| CrossApplicationQualificationError::Accuracy {
                    case_id: case.case_id.clone(),
                    message: error.to_string(),
                })
        }
        CrossApplicationAccuracyBudget::SrgbDisplay { budget } => {
            let (ColorReferencePixels::Rgba8(left), ColorReferencePixels::Rgba8(right)) =
                (&left.pixels, &right.pixels)
            else {
                return Err(storage_error());
            };
            compare_srgb_display_rgba8(left, right, budget)
                .map(|report| CrossApplicationComparisonStatistics::SrgbDisplay { report })
                .map_err(|error| CrossApplicationQualificationError::Accuracy {
                    case_id: case.case_id.clone(),
                    message: error.to_string(),
                })
        }
        CrossApplicationAccuracyBudget::Bt2100PqDisplay { budget } => {
            let (ColorReferencePixels::RgbaF32(left), ColorReferencePixels::RgbaF32(right)) =
                (&left.pixels, &right.pixels)
            else {
                return Err(storage_error());
            };
            compare_pq_hdr_display_rgba(left, right, budget)
                .map(|report| CrossApplicationComparisonStatistics::Bt2100PqDisplay { report })
                .map_err(|error| CrossApplicationQualificationError::Accuracy {
                    case_id: case.case_id.clone(),
                    message: error.to_string(),
                })
        }
    }
}

fn validate_identity(
    field: &'static str,
    value: &str,
) -> Result<(), CrossApplicationQualificationError> {
    let value = value.trim();
    if value.is_empty()
        || ["unknown", "unset", "tbd", "placeholder", "n/a"]
            .iter()
            .any(|placeholder| value.eq_ignore_ascii_case(placeholder))
    {
        Err(CrossApplicationQualificationError::InvalidIdentity { field })
    } else {
        Ok(())
    }
}

fn validate_sha256(
    field: &'static str,
    value: &str,
) -> Result<(), CrossApplicationQualificationError> {
    if value.len() == 64
        && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(CrossApplicationQualificationError::InvalidSha256 { field })
    }
}

fn validate_source_revision(value: &str) -> Result<(), CrossApplicationQualificationError> {
    if value.len() == 40
        && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(CrossApplicationQualificationError::InvalidSourceRevision)
    }
}

fn digest_serializable<T: Serialize>(
    value: &T,
) -> Result<String, CrossApplicationQualificationError> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        CrossApplicationQualificationError::Serialization { message: error.to_string() }
    })?;
    Ok(sha256_hex(&bytes))
}

fn report_digest(
    report: &CrossApplicationQualificationReport,
) -> Result<String, CrossApplicationQualificationError> {
    let mut canonical = report.clone();
    canonical.evidence_sha256.clear();
    digest_serializable(&canonical)
}

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}
