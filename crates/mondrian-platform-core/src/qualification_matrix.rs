//! Cross-platform GPU/driver/display qualification matrix.
//!
//! The Module owns profile closure, exact environment matching, evidence
//! correlation, scenario semantics, and deterministic verdicts. OS capture,
//! product execution, and filesystem sealing remain external Adapters.

use crate::DisplayProbeBackend;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

const PROFILE_SCHEMA_VERSION: u32 = 1;
const REPORT_SCHEMA_VERSION: u32 = 1;
const HARD_MAX_CELLS: usize = 32;
const HARD_MAX_ITEMS_PER_CELL: usize = 16;

/// Operating-system family represented by one qualification cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationPlatform {
    /// Microsoft Windows.
    Windows,
    /// Apple macOS.
    MacOs,
    /// Desktop Linux.
    Linux,
}

/// Production graphics backend represented by one qualification cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationGraphicsBackend {
    /// Direct3D 12 through wgpu.
    Dx12,
    /// Metal through wgpu.
    Metal,
    /// Vulkan through wgpu.
    Vulkan,
}

/// Physical implementation class for the graphics adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationAdapterKind {
    /// A physical production GPU.
    Hardware,
    /// A software rasterizer such as WARP or llvmpipe.
    Software,
}

/// Platform-correct exact driver identity.
///
/// Windows drivers normally expose an explicit package version, macOS Metal
/// ships with the OS build, and Linux qualification must bind the complete
/// kernel/DRM/Vulkan stack, plus Mesa when applicable, instead of flattening it
/// into one optional string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlatformQualificationDriverIdentity {
    /// Independently versioned driver package.
    Explicit {
        /// Exact driver implementation name.
        name: String,
        /// Exact package version/build.
        version: String,
    },
    /// Driver distributed as part of an operating-system build.
    OsBundled {
        /// Exact OS build carrying the driver.
        os_build: String,
    },
    /// Linux Vulkan/DRM graphics stack identity.
    LinuxStack {
        /// Exact kernel version.
        kernel_version: String,
        /// Exact kernel DRM driver identity/version.
        drm_driver: String,
        /// Exact Vulkan ICD/driver implementation.
        vulkan_driver: String,
        /// Exact Vulkan driver version.
        vulkan_driver_version: String,
        /// Exact Mesa version when the stack uses Mesa.
        mesa_version: Option<String>,
    },
}

/// Exact target-specific product artifact qualified by one matrix cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformQualificationProductArtifact {
    /// Operating-system family targeted by the package.
    pub platform: QualificationPlatform,
    /// Exact Rust/platform target triple or equivalent distribution target.
    pub target_triple: String,
    /// Exact immutable package kind, such as `windows-exe`, `macos-dmg`, or
    /// `linux-appimage`.
    pub package_kind: String,
    /// SHA-256 of the exact executable/package bytes.
    pub sha256: String,
    /// SHA-256 of the executable image actually launched for Viewer evidence.
    /// This is distinct from a DMG, MSIX, AppImage, or package archive hash.
    pub runtime_image_sha256: String,
    /// SHA-256 of the target-specific build provenance report.
    pub build_provenance_sha256: String,
}

/// Physical Viewer presentation scenario required by the commercial matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationDisplayScenario {
    /// Standard-dynamic-range sRGB presentation.
    SdrSrgb,
    /// Wide-gamut Display P3 presentation.
    DisplayP3,
    /// BT.2100 PQ HDR presentation.
    HdrPq,
    /// Managed SDR presentation through an OS-selected ICC profile.
    ManagedIcc,
}

/// Final Viewer payload interpretation at the UI presentation Seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationPresentationTransfer {
    /// Standard target code values decoded by the surface carrier.
    SurfaceCodeValues,
    /// ICC-calibrated device code values.
    DeviceCodeValues,
    /// Linear extended-range values consumed by the macOS EDR carrier.
    ExtendedLinearValues,
}

/// Platform-native presentation carrier used for a PQ program scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationHdrPresentation {
    /// Active link/surface transfer is SMPTE ST 2084 / PQ.
    NativePq,
    /// PQ program content is presented through the macOS linear EDR carrier.
    MacOsEdr,
}

/// Controlled presentation surface identity for a qualification scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationSurfaceColorSpace {
    /// Standard sRGB surface encoding.
    Srgb,
    /// Display P3 surface encoding.
    DisplayP3,
    /// Native BT.2100 PQ surface encoding.
    Bt2100Pq,
    /// macOS linear extended-range EDR carrier.
    ExtendedLinearEdr,
}

/// HDR transfer function observed from the active platform output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationHdrTransferFunction {
    /// SMPTE ST 2084 / PQ.
    Pq,
    /// ARIB STD-B67 / HLG.
    Hlg,
}

/// Independent evidence lane required for every qualification cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformQualificationEvidenceKind {
    /// Native ICC/HDR/display probing and machine inventory.
    PlatformProbe,
    /// Sealed real-device GPU color execution.
    GpuColor,
    /// Physical Viewer display execution and operator observation.
    ViewerDisplay,
}

/// Exact owner contract for one independently verified evidence lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformQualificationEvidenceRequirement {
    /// Evidence lane.
    pub kind: PlatformQualificationEvidenceKind,
    /// Module that must verify and seal the lane.
    pub owner: String,
    /// Exact owner verifier/gate identity.
    pub verifier_id: String,
    /// Exact accepted lane-report schema version.
    pub report_schema_version: u32,
}

/// Stable terminal status for evidence and aggregate reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformQualificationStatus {
    /// Required evidence is complete and passing.
    Qualified,
    /// Executed evidence is terminally failing.
    Failed,
    /// A required cell or evidence lane is absent.
    Incomplete,
}

/// Exact machine, driver, window-system, and display identity for one cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformQualificationEnvironment {
    /// Operating-system family.
    pub platform: QualificationPlatform,
    /// Exact architecture label.
    pub architecture: String,
    /// Exact OS product/version identity.
    pub os_version: String,
    /// Exact OS build/kernel identity.
    pub os_build: String,
    /// Exact window-system identity, such as Win32, Quartz, Wayland, or X11.
    pub window_system: String,
    /// Exact desktop compositor/window-manager identity and version.
    pub compositor: String,
    /// Production graphics backend.
    pub graphics_backend: QualificationGraphicsBackend,
    /// Exact GPU adapter name.
    pub adapter_name: String,
    /// Exact GPU vendor identity.
    pub adapter_vendor: String,
    /// Exact PCI/registry/platform device identity.
    pub adapter_device_id: String,
    /// Exact renderer API driver label reported by the active adapter.
    pub renderer_driver: String,
    /// Exact renderer API driver detail/version string reported by the active adapter.
    pub renderer_driver_info: String,
    /// Whether the adapter is physical hardware or a software rasterizer.
    pub adapter_kind: QualificationAdapterKind,
    /// Platform-correct exact driver identity.
    pub driver: PlatformQualificationDriverIdentity,
    /// Operator-assigned physical display identity.
    pub display_identity: String,
    /// Exact platform-native output/path identity used by the product and OS probes.
    pub native_display_path_id: String,
    /// SHA-256 of the complete display/link inventory without hardware serials.
    pub display_inventory_sha256: String,
}

/// Exact expected behavior for one physical Viewer scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformQualificationScenarioRequirement {
    /// Scenario identity.
    pub scenario: QualificationDisplayScenario,
    /// Exact target surface color-space label.
    pub surface_color_space: QualificationSurfaceColorSpace,
    /// Required presentation payload interpretation.
    pub transfer: QualificationPresentationTransfer,
    /// Minimum active output bits per color channel when the platform exposes it.
    pub minimum_bits_per_color_channel: Option<u8>,
    /// Minimum active/advertised peak luminance for HDR, otherwise absent.
    pub minimum_peak_luminance_nits: Option<u32>,
    /// Minimum macOS EDR headroom in parts per million, otherwise absent.
    pub minimum_edr_headroom_ppm: Option<u32>,
    /// Required HDR carrier for a PQ program scenario, otherwise absent.
    pub hdr_presentation: Option<QualificationHdrPresentation>,
}

/// One exact machine/GPU/driver/display cell in a runtime profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformQualificationCellRequirement {
    /// Stable cell identity.
    pub cell_id: String,
    /// Exact frozen environment.
    pub environment: PlatformQualificationEnvironment,
    /// Native probe backends required for this environment.
    pub required_probe_backends: Vec<DisplayProbeBackend>,
    /// Required physical display scenarios.
    pub required_scenarios: Vec<PlatformQualificationScenarioRequirement>,
    /// Required evidence lanes.
    pub required_evidence: Vec<PlatformQualificationEvidenceRequirement>,
}

/// Explicit resource limits for a qualification profile/campaign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformQualificationLimits {
    /// Largest number of exact cells.
    pub max_cells: u16,
    /// Largest number of probe, report, or scenario rows per cell.
    pub max_items_per_cell: u16,
}

/// Strict runtime matrix profile. Every mutable environment fact is exact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformDriverDisplayQualificationProfile {
    /// Contract schema version. Version 1 is required.
    pub schema_version: u32,
    /// Stable matrix identity.
    pub qualification_id: String,
    /// Immutable profile edition.
    pub edition: String,
    /// Exact cells covering every platform and display scenario.
    pub cells: Vec<PlatformQualificationCellRequirement>,
    /// Resource limits.
    pub limits: PlatformQualificationLimits,
}

/// Validated, canonically ordered matrix plan.
#[derive(Debug, Clone)]
pub struct PreparedPlatformDriverDisplayQualification {
    profile: PlatformDriverDisplayQualificationProfile,
    profile_sha256: String,
}

impl PreparedPlatformDriverDisplayQualification {
    /// Validate semantic closure and canonicalize one exact runtime profile.
    pub fn compile(
        mut profile: PlatformDriverDisplayQualificationProfile,
    ) -> Result<Self, PlatformQualificationError> {
        validate_profile(&profile)?;
        for cell in &mut profile.cells {
            cell.required_probe_backends.sort();
            cell.required_evidence.sort_by_key(|requirement| requirement.kind);
            cell.required_scenarios.sort_by_key(|row| row.scenario);
        }
        profile.cells.sort_by(|left, right| left.cell_id.cmp(&right.cell_id));
        let profile_sha256 = digest_serializable(&profile)?;
        Ok(Self { profile, profile_sha256 })
    }

    /// Canonical SHA-256 of the compiled runtime profile.
    pub fn profile_sha256(&self) -> &str {
        &self.profile_sha256
    }

    /// Evaluate one cross-machine campaign against the exact profile.
    pub fn evaluate(
        &self,
        campaign: PlatformQualificationCampaign,
    ) -> Result<PlatformQualificationReport, PlatformQualificationError> {
        validate_identity("campaign_id", &campaign.campaign_id)?;
        validate_source_revision(&campaign.source_revision)?;
        validate_identity("release_candidate_id", &campaign.release_candidate_id)?;
        validate_sha256("build_manifest_sha256", &campaign.build_manifest_sha256)?;
        if campaign.cells.len() > usize::from(self.profile.limits.max_cells) {
            return Err(PlatformQualificationError::CellLimitExceeded {
                actual: campaign.cells.len(),
                maximum: usize::from(self.profile.limits.max_cells),
            });
        }

        let mut observations = BTreeMap::new();
        let mut run_ids = BTreeSet::new();
        for observation in campaign.cells {
            validate_identity("cell_run_id", &observation.cell_run_id)?;
            validate_sha256("machine_report_sha256", &observation.machine_report_sha256)?;
            validate_sha256(
                "environment_before_sha256",
                &observation.environment_before_sha256,
            )?;
            validate_sha256(
                "environment_after_sha256",
                &observation.environment_after_sha256,
            )?;
            validate_product_artifact(&observation.product_artifact)?;
            if observation.source_revision != campaign.source_revision {
                return Err(PlatformQualificationError::SourceRevisionMismatch {
                    cell_id: observation.cell_id,
                });
            }
            if !run_ids.insert(observation.cell_run_id.clone()) {
                return Err(PlatformQualificationError::DuplicateCellRun {
                    run_id: observation.cell_run_id,
                });
            }
            let cell_id = observation.cell_id.clone();
            if observations.insert(cell_id.clone(), observation).is_some() {
                return Err(PlatformQualificationError::DuplicateCell { cell_id });
            }
        }

        let expected_ids = self
            .profile
            .cells
            .iter()
            .map(|cell| cell.cell_id.clone())
            .collect::<BTreeSet<_>>();
        for cell_id in observations.keys() {
            if !expected_ids.contains(cell_id) {
                return Err(PlatformQualificationError::UnexpectedCell {
                    cell_id: cell_id.clone(),
                });
            }
        }

        let mut missing_cells = Vec::new();
        let mut cell_reports = Vec::with_capacity(self.profile.cells.len());
        let mut any_failed = false;
        let mut any_incomplete = false;
        for requirement in &self.profile.cells {
            let Some(observation) = observations.get(&requirement.cell_id) else {
                missing_cells.push(requirement.cell_id.clone());
                any_incomplete = true;
                continue;
            };
            let cell_report = evaluate_cell(
                requirement,
                observation,
                &self.profile_sha256,
                &campaign.release_candidate_id,
                &campaign.build_manifest_sha256,
                usize::from(self.profile.limits.max_items_per_cell),
            )?;
            any_failed |= cell_report.status == PlatformQualificationStatus::Failed;
            any_incomplete |= cell_report.status == PlatformQualificationStatus::Incomplete;
            cell_reports.push(cell_report);
        }
        let status = if any_failed {
            PlatformQualificationStatus::Failed
        } else if any_incomplete {
            PlatformQualificationStatus::Incomplete
        } else {
            PlatformQualificationStatus::Qualified
        };
        let mut report = PlatformQualificationReport {
            schema_version: REPORT_SCHEMA_VERSION,
            qualification_id: self.profile.qualification_id.clone(),
            edition: self.profile.edition.clone(),
            profile_sha256: self.profile_sha256.clone(),
            campaign_id: campaign.campaign_id,
            source_revision: campaign.source_revision,
            release_candidate_id: campaign.release_candidate_id,
            build_manifest_sha256: campaign.build_manifest_sha256,
            status,
            missing_cells,
            cells: cell_reports,
            evidence_sha256: String::new(),
        };
        report.evidence_sha256 = report_digest(&report)?;
        Ok(report)
    }
}

/// One sealed report lane attached to a matrix cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformQualificationEvidenceReport {
    /// Evidence lane.
    pub kind: PlatformQualificationEvidenceKind,
    /// Module that independently verified and sealed the lane report.
    pub owner: String,
    /// Exact owner verifier/gate that produced the receipt.
    pub verifier_id: String,
    /// Canonical matrix profile SHA-256 carried by the sealed wrapper.
    pub profile_sha256: String,
    /// Positive schema version of the independently verified lane report.
    pub report_schema_version: u32,
    /// Terminal lane status.
    pub status: PlatformQualificationStatus,
    /// SHA-256 of the independently verified lane report.
    pub report_sha256: String,
    /// SHA-256 of the bounded raw evidence bundle consumed by the lane owner.
    pub raw_evidence_sha256: String,
    /// SHA-256 of the exact executable/package qualified by this lane.
    pub product_artifact_sha256: String,
    /// SHA-256 of the product runtime image bound by this lane.
    pub runtime_image_sha256: String,
    /// Release-candidate identity carried by the owner-verified report.
    pub release_candidate_id: String,
    /// Cross-target build-manifest SHA-256 carried by the owner-verified report.
    pub build_manifest_sha256: String,
    /// Target-specific build-provenance SHA-256 carried by the report.
    pub build_provenance_sha256: String,
    /// Exact source revision carried by that report.
    pub source_revision: String,
    /// Machine report SHA-256 carried by that report.
    pub machine_report_sha256: String,
    /// Canonical observed environment SHA-256 carried by that report.
    pub environment_sha256: String,
    /// Cell run identity carried by the sealed wrapper.
    pub cell_run_id: String,
}

/// Physical scenario evidence extracted by a platform-specific capture Adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformQualificationScenarioEvidence {
    /// Scenario identity.
    pub scenario: QualificationDisplayScenario,
    /// Terminal scenario status.
    pub status: PlatformQualificationStatus,
    /// Viewer display report SHA-256.
    pub report_sha256: String,
    /// Exact Display Output Contract SHA-256.
    pub output_contract_sha256: String,
    /// Exact row environment SHA-256 carried by the Viewer report.
    pub environment_sha256: String,
    /// Operator attestation SHA-256 bound to this display/scenario.
    pub operator_attestation_sha256: String,
    /// Viewer health was Ready for the qualified presentation.
    pub viewer_ready: bool,
    /// Display Output Contract validation had no blocker.
    pub display_contract_valid: bool,
    /// A registered external texture was actually presented.
    pub external_texture_presented: bool,
    /// The presentation path recorded no CPU/GPU readback stage.
    pub zero_readback_stages: bool,
    /// The native surface carrier was reused after allocation.
    pub carrier_reuse_observed: bool,
    /// The operator observation passed for this exact display/stimulus.
    pub operator_observation_passed: bool,
    /// Whether the scenario attempted to skip unsupported capability.
    pub capability_skip_observed: bool,
    /// Actual surface color-space label.
    pub surface_color_space: QualificationSurfaceColorSpace,
    /// Actual payload interpretation at presentation.
    pub transfer: QualificationPresentationTransfer,
    /// Active output bits per color channel when exposed by the platform.
    pub bits_per_color_channel: Option<u8>,
    /// Physical/link wider-than-sRGB capability.
    pub wide_color_supported: Option<bool>,
    /// Active desktop wider-than-sRGB encoding.
    pub wide_color_active: Option<bool>,
    /// Physical/link HDR capability.
    pub hdr_supported: Option<bool>,
    /// Active desktop HDR/EDR state.
    pub hdr_enabled: Option<bool>,
    /// Active output HDR transfer function.
    pub active_hdr_transfer: Option<QualificationHdrTransferFunction>,
    /// Reported active/advertised maximum luminance.
    pub peak_luminance_nits: Option<u32>,
    /// Active macOS EDR headroom in parts per million.
    pub edr_headroom_ppm: Option<u32>,
    /// Observed platform-native HDR presentation carrier.
    pub hdr_presentation: Option<QualificationHdrPresentation>,
    /// OS-selected ICC payload SHA-256 for managed ICC.
    pub icc_profile_sha256: Option<String>,
    /// Prepared device calibration processor/LUT SHA-256 for managed ICC.
    pub icc_processor_sha256: Option<String>,
}

/// Evidence observed for one exact profile cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformQualificationCellObservation {
    /// Profile cell identity.
    pub cell_id: String,
    /// Unique run identity; it cannot be reused by another cell.
    pub cell_run_id: String,
    /// Clean source revision used by every lane.
    pub source_revision: String,
    /// Machine inventory report SHA-256.
    pub machine_report_sha256: String,
    /// Exact observed environment.
    pub environment: PlatformQualificationEnvironment,
    /// Exact target-specific executable/package qualified by this row.
    pub product_artifact: PlatformQualificationProductArtifact,
    /// Release-candidate identity bound by the row sealer.
    pub release_candidate_id: String,
    /// Cross-target build-manifest SHA-256 bound by the row sealer.
    pub build_manifest_sha256: String,
    /// Environment SHA-256 captured before the row executed.
    pub environment_before_sha256: String,
    /// Environment SHA-256 captured after the row executed.
    pub environment_after_sha256: String,
    /// Native backends that actually produced probe evidence.
    pub probe_backends: Vec<DisplayProbeBackend>,
    /// Independently sealed evidence reports.
    pub reports: Vec<PlatformQualificationEvidenceReport>,
    /// Physical display scenario evidence.
    pub scenarios: Vec<PlatformQualificationScenarioEvidence>,
}

/// Cross-machine campaign. Distinct cell runs must share one clean source revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformQualificationCampaign {
    /// Non-empty campaign identity.
    pub campaign_id: String,
    /// Exact lowercase 40-hex Git revision shared by every cell.
    pub source_revision: String,
    /// Product release-candidate identity shared by every target artifact.
    pub release_candidate_id: String,
    /// SHA-256 of the manifest binding every target-specific build artifact.
    pub build_manifest_sha256: String,
    /// Cell observations in any order.
    pub cells: Vec<PlatformQualificationCellObservation>,
}

/// Deterministic result for one present cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlatformQualificationCellReport {
    /// Cell identity.
    pub cell_id: String,
    /// Unique cell run identity.
    pub cell_run_id: String,
    /// Machine report SHA-256.
    pub machine_report_sha256: String,
    /// Exact qualified environment.
    pub environment: PlatformQualificationEnvironment,
    /// Exact target-specific product artifact.
    pub product_artifact: PlatformQualificationProductArtifact,
    /// Present native probe backends.
    pub probe_backends: Vec<DisplayProbeBackend>,
    /// Missing required probe backends.
    pub missing_probe_backends: Vec<DisplayProbeBackend>,
    /// Present evidence reports.
    pub reports: Vec<PlatformQualificationEvidenceReport>,
    /// Missing evidence lanes.
    pub missing_evidence: Vec<PlatformQualificationEvidenceKind>,
    /// Present physical display scenarios.
    pub scenarios: Vec<PlatformQualificationScenarioEvidence>,
    /// Missing physical display scenarios.
    pub missing_scenarios: Vec<QualificationDisplayScenario>,
    /// Cell verdict.
    pub status: PlatformQualificationStatus,
}

/// Deterministic aggregate report for one exact-source campaign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlatformQualificationReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Runtime profile identity.
    pub qualification_id: String,
    /// Runtime profile edition.
    pub edition: String,
    /// Canonical runtime profile SHA-256.
    pub profile_sha256: String,
    /// Campaign identity.
    pub campaign_id: String,
    /// Exact source revision shared by every cell.
    pub source_revision: String,
    /// Release-candidate identity shared by all target artifacts.
    pub release_candidate_id: String,
    /// SHA-256 of the cross-target build manifest.
    pub build_manifest_sha256: String,
    /// Aggregate verdict.
    pub status: PlatformQualificationStatus,
    /// Entire required cells absent from the campaign.
    pub missing_cells: Vec<String>,
    /// Present cell reports in canonical order.
    pub cells: Vec<PlatformQualificationCellReport>,
    /// SHA-256 over every preceding report field.
    pub evidence_sha256: String,
}

impl PlatformQualificationReport {
    /// Verify the report's deterministic evidence digest.
    pub fn verify_evidence(&self) -> bool {
        report_digest(self).is_ok_and(|digest| digest == self.evidence_sha256)
    }
}

/// Failure to compile or evaluate a platform qualification matrix.
#[derive(Debug, Error)]
pub enum PlatformQualificationError {
    /// Only schema version 1 is accepted.
    #[error("unsupported platform qualification schema {actual}; expected 1")]
    UnsupportedSchema {
        /// Observed schema version.
        actual: u32,
    },
    /// An identity field was empty or a placeholder.
    #[error("invalid platform qualification identity field '{field}'")]
    InvalidIdentity {
        /// Invalid field name.
        field: &'static str,
    },
    /// A digest was not lowercase hexadecimal SHA-256.
    #[error("platform qualification field '{field}' must be a lowercase SHA-256")]
    InvalidSha256 {
        /// Invalid field name.
        field: &'static str,
    },
    /// Source revision was not lowercase 40-hex Git SHA-1.
    #[error("platform qualification source revision must be a lowercase 40-hex Git SHA")]
    InvalidSourceRevision,
    /// Resource limits were zero or exceeded hard caps.
    #[error("platform qualification resource limits are invalid")]
    InvalidResourceLimits,
    /// Profile/campaign cell count exceeded its limit.
    #[error("platform qualification has {actual} cells, exceeding limit {maximum}")]
    CellLimitExceeded {
        /// Observed cell count.
        actual: usize,
        /// Configured cell limit.
        maximum: usize,
    },
    /// Cell identity was duplicated.
    #[error("duplicate platform qualification cell '{cell_id}'")]
    DuplicateCell {
        /// Duplicated cell identity.
        cell_id: String,
    },
    /// A campaign contained an undeclared cell.
    #[error("unexpected platform qualification cell '{cell_id}'")]
    UnexpectedCell {
        /// Unexpected cell identity.
        cell_id: String,
    },
    /// Cell run identity was reused.
    #[error("duplicate platform qualification cell run '{run_id}'")]
    DuplicateCellRun {
        /// Reused run identity.
        run_id: String,
    },
    /// Required platform/backend/scenario closure was incomplete.
    #[error("platform qualification profile does not cover Windows/DX12, macOS/Metal, Linux/Vulkan and every required display scenario on each platform")]
    IncompleteProfileCoverage,
    /// A cell's platform and graphics backend were incompatible.
    #[error("platform qualification cell '{cell_id}' uses an incompatible graphics backend")]
    IncompatibleGraphicsBackend {
        /// Invalid cell identity.
        cell_id: String,
    },
    /// A profile attempted to qualify a software rasterizer.
    #[error("platform qualification requires a physical hardware adapter")]
    SoftwareAdapter,
    /// Driver identity did not use the platform-correct exact representation.
    #[error("platform qualification driver identity is invalid for its platform")]
    InvalidDriverIdentity,
    /// Product artifact target/package metadata did not match its platform.
    #[error("platform qualification product artifact target is invalid")]
    InvalidProductArtifact,
    /// Required evidence or scenario identities were duplicated or invalid.
    #[error("platform qualification cell '{cell_id}' has an invalid requirement set")]
    InvalidCellRequirements {
        /// Invalid cell identity.
        cell_id: String,
    },
    /// Native probe requirements cannot prove the declared platform scenario.
    #[error("platform qualification cell '{cell_id}' lacks a required native probe backend")]
    InvalidProbeClosure {
        /// Invalid cell identity.
        cell_id: String,
    },
    /// Observed exact environment differed from the runtime profile.
    #[error("platform qualification environment mismatch for cell '{cell_id}'")]
    EnvironmentMismatch {
        /// Mismatched cell identity.
        cell_id: String,
    },
    /// A row or owner report was relabeled onto another release/build manifest.
    #[error("platform qualification build binding mismatch for cell '{cell_id}'")]
    BuildBindingMismatch {
        /// Mismatched cell identity.
        cell_id: String,
    },
    /// Environment identity changed during one atomic cell run.
    #[error("platform qualification environment drifted during cell '{cell_id}'")]
    EnvironmentDrift {
        /// Drifted cell identity.
        cell_id: String,
    },
    /// A cell used another source revision.
    #[error("platform qualification source revision mismatch for cell '{cell_id}'")]
    SourceRevisionMismatch {
        /// Mismatched cell identity.
        cell_id: String,
    },
    /// A present probe/report/scenario row was duplicated or undeclared.
    #[error("platform qualification evidence set is invalid for cell '{cell_id}'")]
    InvalidEvidenceSet {
        /// Invalid cell identity.
        cell_id: String,
    },
    /// A report did not bind the cell source, machine, or run identity.
    #[error("platform qualification report binding mismatch for cell '{cell_id}'")]
    ReportBindingMismatch {
        /// Mismatched cell identity.
        cell_id: String,
    },
    /// Physical scenario facts contradicted the exact requirement.
    #[error("platform qualification scenario {scenario:?} does not satisfy cell '{cell_id}'")]
    ScenarioContractMismatch {
        /// Mismatched cell identity.
        cell_id: String,
        /// Mismatched scenario.
        scenario: QualificationDisplayScenario,
    },
    /// Canonical profile/report serialization failed.
    #[error("platform qualification evidence serialization failed: {message}")]
    Serialization {
        /// Serialization diagnostic.
        message: String,
    },
}

fn validate_profile(
    profile: &PlatformDriverDisplayQualificationProfile,
) -> Result<(), PlatformQualificationError> {
    if profile.schema_version != PROFILE_SCHEMA_VERSION {
        return Err(PlatformQualificationError::UnsupportedSchema {
            actual: profile.schema_version,
        });
    }
    validate_identity("qualification_id", &profile.qualification_id)?;
    validate_identity("edition", &profile.edition)?;
    if profile.limits.max_cells == 0
        || profile.limits.max_items_per_cell == 0
        || usize::from(profile.limits.max_cells) > HARD_MAX_CELLS
        || usize::from(profile.limits.max_items_per_cell) > HARD_MAX_ITEMS_PER_CELL
    {
        return Err(PlatformQualificationError::InvalidResourceLimits);
    }
    if profile.cells.is_empty() || profile.cells.len() > usize::from(profile.limits.max_cells) {
        return Err(PlatformQualificationError::CellLimitExceeded {
            actual: profile.cells.len(),
            maximum: usize::from(profile.limits.max_cells),
        });
    }

    let all_evidence = BTreeSet::from([
        PlatformQualificationEvidenceKind::PlatformProbe,
        PlatformQualificationEvidenceKind::GpuColor,
        PlatformQualificationEvidenceKind::ViewerDisplay,
    ]);
    let all_scenarios = BTreeSet::from([
        QualificationDisplayScenario::SdrSrgb,
        QualificationDisplayScenario::DisplayP3,
        QualificationDisplayScenario::HdrPq,
        QualificationDisplayScenario::ManagedIcc,
    ]);
    let mut ids = BTreeSet::new();
    let mut environments = BTreeSet::new();
    let mut platform_scenarios: BTreeMap<QualificationPlatform, BTreeSet<_>> = BTreeMap::new();
    let mut platform_backends = BTreeSet::new();
    for cell in &profile.cells {
        validate_identity("cell_id", &cell.cell_id)?;
        validate_environment(&cell.environment)?;
        if !ids.insert(cell.cell_id.clone()) {
            return Err(PlatformQualificationError::DuplicateCell {
                cell_id: cell.cell_id.clone(),
            });
        }
        let environment_key = digest_serializable(&cell.environment)?;
        if !environments.insert(environment_key) {
            return Err(PlatformQualificationError::DuplicateCell {
                cell_id: cell.cell_id.clone(),
            });
        }
        let expected_backend = match cell.environment.platform {
            QualificationPlatform::Windows => QualificationGraphicsBackend::Dx12,
            QualificationPlatform::MacOs => QualificationGraphicsBackend::Metal,
            QualificationPlatform::Linux => QualificationGraphicsBackend::Vulkan,
        };
        if cell.environment.graphics_backend != expected_backend {
            return Err(PlatformQualificationError::IncompatibleGraphicsBackend {
                cell_id: cell.cell_id.clone(),
            });
        }
        platform_backends.insert((cell.environment.platform, cell.environment.graphics_backend));
        if cell.required_probe_backends.len() > usize::from(profile.limits.max_items_per_cell)
            || cell.required_scenarios.len() > usize::from(profile.limits.max_items_per_cell)
            || cell.required_evidence.len() > usize::from(profile.limits.max_items_per_cell)
        {
            return Err(PlatformQualificationError::InvalidCellRequirements {
                cell_id: cell.cell_id.clone(),
            });
        }
        let probes = cell.required_probe_backends.iter().copied().collect::<BTreeSet<_>>();
        let evidence = cell
            .required_evidence
            .iter()
            .map(|requirement| requirement.kind)
            .collect::<BTreeSet<_>>();
        let scenarios =
            cell.required_scenarios.iter().map(|row| row.scenario).collect::<BTreeSet<_>>();
        if probes.len() != cell.required_probe_backends.len()
            || evidence.len() != cell.required_evidence.len()
            || scenarios.len() != cell.required_scenarios.len()
            || evidence != all_evidence
            || scenarios.is_empty()
        {
            return Err(PlatformQualificationError::InvalidCellRequirements {
                cell_id: cell.cell_id.clone(),
            });
        }
        for requirement in &cell.required_evidence {
            validate_identity("evidence_owner", &requirement.owner)?;
            validate_identity("evidence_verifier_id", &requirement.verifier_id)?;
            if requirement.report_schema_version == 0 {
                return Err(PlatformQualificationError::InvalidCellRequirements {
                    cell_id: cell.cell_id.clone(),
                });
            }
        }
        for scenario in &cell.required_scenarios {
            validate_scenario_requirement(&cell.cell_id, cell.environment.platform, scenario)?;
            validate_probe_closure(cell, scenario.scenario, &probes)?;
        }
        platform_scenarios
            .entry(cell.environment.platform)
            .or_default()
            .extend(scenarios);
    }
    let expected_backends = BTreeSet::from([
        (
            QualificationPlatform::Windows,
            QualificationGraphicsBackend::Dx12,
        ),
        (
            QualificationPlatform::MacOs,
            QualificationGraphicsBackend::Metal,
        ),
        (
            QualificationPlatform::Linux,
            QualificationGraphicsBackend::Vulkan,
        ),
    ]);
    if platform_backends != expected_backends
        || platform_scenarios.len() != 3
        || platform_scenarios.values().any(|scenarios| scenarios != &all_scenarios)
    {
        return Err(PlatformQualificationError::IncompleteProfileCoverage);
    }
    Ok(())
}

fn validate_environment(
    environment: &PlatformQualificationEnvironment,
) -> Result<(), PlatformQualificationError> {
    for (field, value) in [
        ("architecture", environment.architecture.as_str()),
        ("os_version", environment.os_version.as_str()),
        ("os_build", environment.os_build.as_str()),
        ("window_system", environment.window_system.as_str()),
        ("compositor", environment.compositor.as_str()),
        ("adapter_name", environment.adapter_name.as_str()),
        ("adapter_vendor", environment.adapter_vendor.as_str()),
        ("adapter_device_id", environment.adapter_device_id.as_str()),
        ("renderer_driver", environment.renderer_driver.as_str()),
        (
            "renderer_driver_info",
            environment.renderer_driver_info.as_str(),
        ),
        ("display_identity", environment.display_identity.as_str()),
        (
            "native_display_path_id",
            environment.native_display_path_id.as_str(),
        ),
    ] {
        validate_identity(field, value)?;
    }
    validate_driver_identity(environment)?;
    if environment.adapter_kind != QualificationAdapterKind::Hardware {
        return Err(PlatformQualificationError::SoftwareAdapter);
    }
    validate_sha256(
        "display_inventory_sha256",
        &environment.display_inventory_sha256,
    )
}

fn validate_driver_identity(
    environment: &PlatformQualificationEnvironment,
) -> Result<(), PlatformQualificationError> {
    let valid_variant = match (&environment.platform, &environment.driver) {
        (
            QualificationPlatform::Windows,
            PlatformQualificationDriverIdentity::Explicit { name, version },
        ) => {
            validate_identity("driver_name", name)?;
            validate_identity("driver_version", version)?;
            true
        }
        (
            QualificationPlatform::MacOs,
            PlatformQualificationDriverIdentity::OsBundled { os_build },
        ) => {
            validate_identity("driver_os_build", os_build)?;
            os_build == &environment.os_build
        }
        (
            QualificationPlatform::Linux,
            PlatformQualificationDriverIdentity::LinuxStack {
                kernel_version,
                drm_driver,
                vulkan_driver,
                vulkan_driver_version,
                mesa_version,
            },
        ) => {
            validate_identity("kernel_version", kernel_version)?;
            validate_identity("drm_driver", drm_driver)?;
            validate_identity("vulkan_driver", vulkan_driver)?;
            validate_identity("vulkan_driver_version", vulkan_driver_version)?;
            if let Some(mesa_version) = mesa_version {
                validate_identity("mesa_version", mesa_version)?;
            }
            kernel_version == &environment.os_build
        }
        _ => false,
    };
    if valid_variant {
        Ok(())
    } else {
        Err(PlatformQualificationError::InvalidDriverIdentity)
    }
}

fn validate_product_artifact(
    artifact: &PlatformQualificationProductArtifact,
) -> Result<(), PlatformQualificationError> {
    validate_identity("target_triple", &artifact.target_triple)?;
    validate_identity("package_kind", &artifact.package_kind)?;
    validate_sha256("product_artifact_sha256", &artifact.sha256)?;
    validate_sha256("runtime_image_sha256", &artifact.runtime_image_sha256)?;
    validate_sha256("build_provenance_sha256", &artifact.build_provenance_sha256)?;
    let target_matches = match artifact.platform {
        QualificationPlatform::Windows => {
            artifact.target_triple.ends_with("-pc-windows-msvc")
                && matches!(
                    artifact.package_kind.as_str(),
                    "windows-exe" | "windows-msix"
                )
        }
        QualificationPlatform::MacOs => {
            artifact.target_triple.ends_with("-apple-darwin")
                && artifact.package_kind == "macos-dmg"
        }
        QualificationPlatform::Linux => {
            artifact.target_triple.ends_with("-unknown-linux-gnu")
                && matches!(
                    artifact.package_kind.as_str(),
                    "linux-appimage" | "linux-deb"
                )
        }
    };
    if target_matches {
        Ok(())
    } else {
        Err(PlatformQualificationError::InvalidProductArtifact)
    }
}

fn validate_scenario_requirement(
    cell_id: &str,
    platform: QualificationPlatform,
    requirement: &PlatformQualificationScenarioRequirement,
) -> Result<(), PlatformQualificationError> {
    let valid = match requirement.scenario {
        QualificationDisplayScenario::HdrPq => match platform {
            QualificationPlatform::Windows => {
                requirement.transfer == QualificationPresentationTransfer::SurfaceCodeValues
                    && requirement.surface_color_space == QualificationSurfaceColorSpace::Bt2100Pq
                    && requirement.minimum_bits_per_color_channel.is_some_and(|bits| bits >= 10)
                    && requirement.minimum_peak_luminance_nits.is_some_and(|nits| nits > 0)
                    && requirement.minimum_edr_headroom_ppm.is_none()
                    && requirement.hdr_presentation == Some(QualificationHdrPresentation::NativePq)
            }
            QualificationPlatform::Linux => {
                requirement.transfer == QualificationPresentationTransfer::SurfaceCodeValues
                    && requirement.surface_color_space == QualificationSurfaceColorSpace::Bt2100Pq
                    && requirement.minimum_bits_per_color_channel.is_none()
                    && requirement.minimum_peak_luminance_nits.is_some_and(|nits| nits > 0)
                    && requirement.minimum_edr_headroom_ppm.is_none()
                    && requirement.hdr_presentation == Some(QualificationHdrPresentation::NativePq)
            }
            QualificationPlatform::MacOs => {
                requirement.transfer == QualificationPresentationTransfer::ExtendedLinearValues
                    && requirement.surface_color_space
                        == QualificationSurfaceColorSpace::ExtendedLinearEdr
                    && requirement.minimum_bits_per_color_channel.is_none()
                    && requirement.minimum_peak_luminance_nits.is_none()
                    && requirement
                        .minimum_edr_headroom_ppm
                        .is_some_and(|headroom| headroom > 1_000_000)
                    && requirement.hdr_presentation == Some(QualificationHdrPresentation::MacOsEdr)
            }
        },
        QualificationDisplayScenario::ManagedIcc => {
            requirement.transfer == QualificationPresentationTransfer::DeviceCodeValues
                && requirement.surface_color_space == QualificationSurfaceColorSpace::Srgb
                && platform_exposes_link_bits(platform, requirement.minimum_bits_per_color_channel)
                && requirement.minimum_peak_luminance_nits.is_none()
                && requirement.minimum_edr_headroom_ppm.is_none()
                && requirement.hdr_presentation.is_none()
        }
        QualificationDisplayScenario::SdrSrgb => {
            requirement.transfer == QualificationPresentationTransfer::SurfaceCodeValues
                && requirement.surface_color_space == QualificationSurfaceColorSpace::Srgb
                && platform_exposes_link_bits(platform, requirement.minimum_bits_per_color_channel)
                && requirement.minimum_peak_luminance_nits.is_none()
                && requirement.minimum_edr_headroom_ppm.is_none()
                && requirement.hdr_presentation.is_none()
        }
        QualificationDisplayScenario::DisplayP3 => {
            requirement.transfer == QualificationPresentationTransfer::SurfaceCodeValues
                && requirement.surface_color_space == QualificationSurfaceColorSpace::DisplayP3
                && platform_exposes_link_bits(platform, requirement.minimum_bits_per_color_channel)
                && requirement.minimum_peak_luminance_nits.is_none()
                && requirement.minimum_edr_headroom_ppm.is_none()
                && requirement.hdr_presentation.is_none()
        }
    };
    if valid {
        Ok(())
    } else {
        Err(PlatformQualificationError::InvalidCellRequirements { cell_id: cell_id.to_owned() })
    }
}

fn platform_exposes_link_bits(platform: QualificationPlatform, bits: Option<u8>) -> bool {
    match platform {
        QualificationPlatform::Windows => bits.is_some_and(|bits| bits > 0),
        QualificationPlatform::MacOs | QualificationPlatform::Linux => bits.is_none(),
    }
}

fn validate_probe_closure(
    cell: &PlatformQualificationCellRequirement,
    scenario: QualificationDisplayScenario,
    probes: &BTreeSet<DisplayProbeBackend>,
) -> Result<(), PlatformQualificationError> {
    let valid = match (cell.environment.platform, scenario) {
        (QualificationPlatform::Windows, QualificationDisplayScenario::ManagedIcc) => {
            probes.contains(&DisplayProbeBackend::WindowsColorProfileDisplayDefault)
                || probes.contains(&DisplayProbeBackend::WindowsWcs)
        }
        (QualificationPlatform::Windows, _) => {
            probes.contains(&DisplayProbeBackend::WindowsDisplayConfig)
        }
        (QualificationPlatform::MacOs, QualificationDisplayScenario::HdrPq) => {
            probes.contains(&DisplayProbeBackend::MacOsCoreGraphics)
                && probes.contains(&DisplayProbeBackend::MacOsAppKit)
        }
        (QualificationPlatform::MacOs, _) => {
            probes.contains(&DisplayProbeBackend::MacOsCoreGraphics)
        }
        (QualificationPlatform::Linux, QualificationDisplayScenario::DisplayP3)
        | (QualificationPlatform::Linux, QualificationDisplayScenario::HdrPq) => {
            probes.contains(&DisplayProbeBackend::WaylandColorManagementV1)
        }
        (QualificationPlatform::Linux, QualificationDisplayScenario::ManagedIcc) => {
            probes.contains(&DisplayProbeBackend::WaylandColorManagementV1)
                || probes.contains(&DisplayProbeBackend::X11RootProperty)
        }
        (QualificationPlatform::Linux, QualificationDisplayScenario::SdrSrgb) => !probes.is_empty(),
    };
    if valid {
        Ok(())
    } else {
        Err(PlatformQualificationError::InvalidProbeClosure { cell_id: cell.cell_id.clone() })
    }
}

fn evaluate_cell(
    requirement: &PlatformQualificationCellRequirement,
    observation: &PlatformQualificationCellObservation,
    profile_sha256: &str,
    release_candidate_id: &str,
    build_manifest_sha256: &str,
    maximum_items: usize,
) -> Result<PlatformQualificationCellReport, PlatformQualificationError> {
    if observation.environment != requirement.environment
        || observation.product_artifact.platform != requirement.environment.platform
        || !target_matches_architecture(
            &observation.product_artifact.target_triple,
            &requirement.environment.architecture,
        )
    {
        return Err(PlatformQualificationError::EnvironmentMismatch {
            cell_id: requirement.cell_id.clone(),
        });
    }
    if observation.release_candidate_id != release_candidate_id
        || observation.build_manifest_sha256 != build_manifest_sha256
    {
        return Err(PlatformQualificationError::BuildBindingMismatch {
            cell_id: requirement.cell_id.clone(),
        });
    }
    if observation.probe_backends.len() > maximum_items
        || observation.reports.len() > maximum_items
        || observation.scenarios.len() > maximum_items
    {
        return Err(PlatformQualificationError::InvalidEvidenceSet {
            cell_id: requirement.cell_id.clone(),
        });
    }
    let probe_set = observation.probe_backends.iter().copied().collect::<BTreeSet<_>>();
    let report_map = observation
        .reports
        .iter()
        .map(|report| (report.kind, report))
        .collect::<BTreeMap<_, _>>();
    let scenario_map = observation
        .scenarios
        .iter()
        .map(|scenario| (scenario.scenario, scenario))
        .collect::<BTreeMap<_, _>>();
    if probe_set.len() != observation.probe_backends.len()
        || report_map.len() != observation.reports.len()
        || scenario_map.len() != observation.scenarios.len()
    {
        return Err(PlatformQualificationError::InvalidEvidenceSet {
            cell_id: requirement.cell_id.clone(),
        });
    }
    let expected_probes =
        requirement.required_probe_backends.iter().copied().collect::<BTreeSet<_>>();
    let expected_reports = requirement
        .required_evidence
        .iter()
        .map(|requirement| (requirement.kind, requirement))
        .collect::<BTreeMap<_, _>>();
    let expected_report_kinds = expected_reports.keys().copied().collect::<BTreeSet<_>>();
    let expected_scenarios = requirement
        .required_scenarios
        .iter()
        .map(|row| row.scenario)
        .collect::<BTreeSet<_>>();
    if !probe_set.is_subset(&expected_probes)
        || !report_map.keys().all(|kind| expected_reports.contains_key(kind))
        || !scenario_map.keys().all(|scenario| expected_scenarios.contains(scenario))
    {
        return Err(PlatformQualificationError::InvalidEvidenceSet {
            cell_id: requirement.cell_id.clone(),
        });
    }

    let missing_probe_backends =
        expected_probes.difference(&probe_set).copied().collect::<Vec<_>>();
    let present_reports = report_map.keys().copied().collect::<BTreeSet<_>>();
    let missing_evidence =
        expected_report_kinds.difference(&present_reports).copied().collect::<Vec<_>>();
    let present_scenarios = scenario_map.keys().copied().collect::<BTreeSet<_>>();
    let missing_scenarios =
        expected_scenarios.difference(&present_scenarios).copied().collect::<Vec<_>>();
    let environment_sha256 = digest_serializable(&observation.environment)?;
    if observation.environment_before_sha256 != environment_sha256
        || observation.environment_after_sha256 != environment_sha256
    {
        return Err(PlatformQualificationError::EnvironmentDrift {
            cell_id: requirement.cell_id.clone(),
        });
    }
    for report in report_map.values() {
        let Some(evidence_requirement) = expected_reports.get(&report.kind) else {
            return Err(PlatformQualificationError::InvalidEvidenceSet {
                cell_id: requirement.cell_id.clone(),
            });
        };
        validate_sha256("evidence_profile_sha256", &report.profile_sha256)?;
        validate_sha256("report_sha256", &report.report_sha256)?;
        validate_sha256("raw_evidence_sha256", &report.raw_evidence_sha256)?;
        validate_sha256(
            "evidence_product_artifact_sha256",
            &report.product_artifact_sha256,
        )?;
        validate_sha256(
            "evidence_runtime_image_sha256",
            &report.runtime_image_sha256,
        )?;
        validate_sha256(
            "evidence_build_manifest_sha256",
            &report.build_manifest_sha256,
        )?;
        validate_sha256(
            "evidence_build_provenance_sha256",
            &report.build_provenance_sha256,
        )?;
        validate_sha256("evidence_environment_sha256", &report.environment_sha256)?;
        if report.owner != evidence_requirement.owner
            || report.verifier_id != evidence_requirement.verifier_id
            || report.report_schema_version != evidence_requirement.report_schema_version
            || report.profile_sha256 != profile_sha256
            || report.product_artifact_sha256 != observation.product_artifact.sha256
            || report.runtime_image_sha256 != observation.product_artifact.runtime_image_sha256
            || report.release_candidate_id != release_candidate_id
            || report.build_manifest_sha256 != build_manifest_sha256
            || report.build_provenance_sha256
                != observation.product_artifact.build_provenance_sha256
            || report.environment_sha256 != environment_sha256
            || report.source_revision != observation.source_revision
            || report.machine_report_sha256 != observation.machine_report_sha256
            || report.cell_run_id != observation.cell_run_id
        {
            return Err(PlatformQualificationError::ReportBindingMismatch {
                cell_id: requirement.cell_id.clone(),
            });
        }
    }
    let viewer_report_sha = report_map
        .get(&PlatformQualificationEvidenceKind::ViewerDisplay)
        .map(|report| report.report_sha256.as_str());
    for scenario_requirement in &requirement.required_scenarios {
        if let Some(evidence) = scenario_map.get(&scenario_requirement.scenario) {
            validate_scenario_evidence(
                requirement,
                scenario_requirement,
                evidence,
                viewer_report_sha,
                &environment_sha256,
            )?;
        }
    }

    let any_failed = report_map
        .values()
        .any(|report| report.status == PlatformQualificationStatus::Failed)
        || scenario_map
            .values()
            .any(|scenario| scenario.status == PlatformQualificationStatus::Failed);
    let any_incomplete = !missing_probe_backends.is_empty()
        || !missing_evidence.is_empty()
        || !missing_scenarios.is_empty()
        || report_map
            .values()
            .any(|report| report.status == PlatformQualificationStatus::Incomplete)
        || scenario_map
            .values()
            .any(|scenario| scenario.status == PlatformQualificationStatus::Incomplete);
    let status = if any_failed {
        PlatformQualificationStatus::Failed
    } else if any_incomplete {
        PlatformQualificationStatus::Incomplete
    } else {
        PlatformQualificationStatus::Qualified
    };
    let mut probe_backends = observation.probe_backends.clone();
    probe_backends.sort();
    let mut reports = observation.reports.clone();
    reports.sort_by_key(|report| report.kind);
    let mut scenarios = observation.scenarios.clone();
    scenarios.sort_by_key(|scenario| scenario.scenario);
    Ok(PlatformQualificationCellReport {
        cell_id: requirement.cell_id.clone(),
        cell_run_id: observation.cell_run_id.clone(),
        machine_report_sha256: observation.machine_report_sha256.clone(),
        environment: observation.environment.clone(),
        product_artifact: observation.product_artifact.clone(),
        probe_backends,
        missing_probe_backends,
        reports,
        missing_evidence,
        scenarios,
        missing_scenarios,
        status,
    })
}

fn target_matches_architecture(target_triple: &str, architecture: &str) -> bool {
    if target_triple.starts_with("x86_64-") {
        architecture == "x86_64"
    } else if target_triple.starts_with("aarch64-") {
        architecture == "aarch64"
    } else {
        false
    }
}

fn validate_scenario_evidence(
    cell: &PlatformQualificationCellRequirement,
    requirement: &PlatformQualificationScenarioRequirement,
    evidence: &PlatformQualificationScenarioEvidence,
    viewer_report_sha: Option<&str>,
    environment_sha256: &str,
) -> Result<(), PlatformQualificationError> {
    for (field, value) in [
        ("scenario_report_sha256", evidence.report_sha256.as_str()),
        (
            "output_contract_sha256",
            evidence.output_contract_sha256.as_str(),
        ),
        (
            "operator_attestation_sha256",
            evidence.operator_attestation_sha256.as_str(),
        ),
        (
            "scenario_environment_sha256",
            evidence.environment_sha256.as_str(),
        ),
    ] {
        validate_sha256(field, value)?;
    }
    let carrier_matches = match requirement.scenario {
        QualificationDisplayScenario::DisplayP3 | QualificationDisplayScenario::HdrPq => {
            evidence.carrier_reuse_observed
        }
        QualificationDisplayScenario::SdrSrgb | QualificationDisplayScenario::ManagedIcc => true,
    };
    let bits_match = match (
        evidence.bits_per_color_channel,
        requirement.minimum_bits_per_color_channel,
    ) {
        (Some(actual), Some(minimum)) => actual >= minimum,
        (None, None) => true,
        _ => false,
    };
    let common_matches = viewer_report_sha == Some(evidence.report_sha256.as_str())
        && evidence.environment_sha256 == environment_sha256
        && evidence.viewer_ready
        && evidence.display_contract_valid
        && evidence.external_texture_presented
        && evidence.zero_readback_stages
        && carrier_matches
        && evidence.operator_observation_passed
        && !evidence.capability_skip_observed
        && evidence.surface_color_space == requirement.surface_color_space
        && evidence.transfer == requirement.transfer
        && bits_match;
    let scenario_matches = match requirement.scenario {
        QualificationDisplayScenario::SdrSrgb => evidence.hdr_enabled != Some(true),
        QualificationDisplayScenario::DisplayP3 => {
            evidence.wide_color_supported == Some(true) && evidence.wide_color_active == Some(true)
        }
        QualificationDisplayScenario::HdrPq => {
            evidence.hdr_supported == Some(true)
                && evidence.hdr_enabled == Some(true)
                && evidence.hdr_presentation == requirement.hdr_presentation
                && match requirement.hdr_presentation {
                    Some(QualificationHdrPresentation::NativePq) => {
                        evidence.active_hdr_transfer == Some(QualificationHdrTransferFunction::Pq)
                            && match (
                                evidence.peak_luminance_nits,
                                requirement.minimum_peak_luminance_nits,
                            ) {
                                (Some(actual), Some(minimum)) => actual >= minimum,
                                _ => false,
                            }
                            && evidence.edr_headroom_ppm.is_none()
                    }
                    Some(QualificationHdrPresentation::MacOsEdr) => {
                        evidence.active_hdr_transfer.is_none()
                            && evidence.peak_luminance_nits.is_none()
                            && match (
                                evidence.edr_headroom_ppm,
                                requirement.minimum_edr_headroom_ppm,
                            ) {
                                (Some(actual), Some(minimum)) => actual >= minimum,
                                _ => false,
                            }
                    }
                    None => false,
                }
        }
        QualificationDisplayScenario::ManagedIcc => {
            evidence.icc_profile_sha256.as_deref().is_some_and(is_sha256)
                && evidence.icc_processor_sha256.as_deref().is_some_and(is_sha256)
        }
    };
    if evidence.status != PlatformQualificationStatus::Qualified
        || common_matches && scenario_matches
    {
        Ok(())
    } else {
        Err(PlatformQualificationError::ScenarioContractMismatch {
            cell_id: cell.cell_id.clone(),
            scenario: requirement.scenario,
        })
    }
}

fn validate_identity(field: &'static str, value: &str) -> Result<(), PlatformQualificationError> {
    let value = value.trim();
    if value.is_empty()
        || ["unknown", "unset", "tbd", "placeholder", "n/a", "latest"]
            .iter()
            .any(|placeholder| value.eq_ignore_ascii_case(placeholder))
    {
        Err(PlatformQualificationError::InvalidIdentity { field })
    } else {
        Ok(())
    }
}

fn validate_sha256(field: &'static str, value: &str) -> Result<(), PlatformQualificationError> {
    if is_sha256(value) {
        Ok(())
    } else {
        Err(PlatformQualificationError::InvalidSha256 { field })
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_source_revision(value: &str) -> Result<(), PlatformQualificationError> {
    if value.len() == 40
        && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(PlatformQualificationError::InvalidSourceRevision)
    }
}

fn digest_serializable<T: Serialize>(value: &T) -> Result<String, PlatformQualificationError> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        PlatformQualificationError::Serialization { message: error.to_string() }
    })?;
    Ok(sha256_hex(&bytes))
}

fn report_digest(
    report: &PlatformQualificationReport,
) -> Result<String, PlatformQualificationError> {
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
