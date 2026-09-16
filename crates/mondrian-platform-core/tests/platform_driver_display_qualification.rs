use mondrian_platform_core::{
    DisplayProbeBackend, PlatformDriverDisplayQualificationProfile, PlatformQualificationCampaign,
    PlatformQualificationCellObservation, PlatformQualificationCellRequirement,
    PlatformQualificationDriverIdentity, PlatformQualificationEnvironment,
    PlatformQualificationError, PlatformQualificationEvidenceKind,
    PlatformQualificationEvidenceReport, PlatformQualificationEvidenceRequirement,
    PlatformQualificationLimits, PlatformQualificationProductArtifact, PlatformQualificationReport,
    PlatformQualificationScenarioEvidence, PlatformQualificationScenarioRequirement,
    PlatformQualificationStatus, PreparedPlatformDriverDisplayQualification,
    QualificationAdapterKind, QualificationDisplayScenario, QualificationGraphicsBackend,
    QualificationHdrPresentation, QualificationHdrTransferFunction, QualificationPlatform,
    QualificationPresentationTransfer, QualificationSurfaceColorSpace,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

const SOURCE_REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
const BUILD_MANIFEST_SHA256: &str =
    "123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0";
const ROW_SUPERVISOR: &str =
    include_str!("../../../scripts/validation/invoke-platform-driver-display-row.ps1");
const MATRIX_RESOLVER: &str =
    include_str!("../../../scripts/validation/resolve-platform-driver-display-matrix.ps1");
const LANE_VERIFIER: &str =
    include_str!("../../../scripts/validation/verify-platform-driver-display-lane.ps1");
const SOURCE_VERIFIER: &str =
    include_str!("../../../scripts/validation/verify-platform-driver-display-source.ps1");
const GPU_SOURCE_SUPERVISOR: &str =
    include_str!("../../../scripts/validation/invoke-platform-gpu-color-source.ps1");
const PLATFORM_SOURCE_SUPERVISOR: &str =
    include_str!("../../../scripts/validation/invoke-platform-display-probe-source.ps1");
const GPU_SOURCE_PROFILE: &str =
    include_str!("../../../tests/validation/platform-gpu-color-gates.json");
const BUNDLE_VERIFIER: &str =
    include_str!("../../../scripts/validation/verify-platform-driver-display-matrix.ps1");
const MATRIX_POLICY: &str =
    include_str!("../../../tests/validation/platform-driver-display-matrix.json");

#[test]
#[ignore = "requires a sealed multi-machine campaign supplied by the qualification supervisor"]
fn sealed_platform_driver_display_campaign() {
    let profile_path = std::env::var("MONDRIAN_PLATFORM_QUALIFICATION_PROFILE")
        .expect("qualification profile path must be supplied");
    let campaign_path = std::env::var("MONDRIAN_PLATFORM_QUALIFICATION_CAMPAIGN")
        .expect("qualification campaign path must be supplied");
    let report_path = std::env::var("MONDRIAN_PLATFORM_QUALIFICATION_REPORT")
        .expect("qualification report path must be supplied");
    let report = evaluate_sealed_files(
        Path::new(&profile_path),
        Path::new(&campaign_path),
        Path::new(&report_path),
    )
    .expect("sealed campaign must evaluate and publish");
    assert_eq!(report.status, PlatformQualificationStatus::Qualified);
    assert!(report.missing_cells.is_empty());
    assert!(report.verify_evidence());
}

#[test]
fn sealed_file_adapter_is_bounded_create_only_and_self_verifying() {
    let directory = tempfile::tempdir().expect("temporary qualification directory must exist");
    let profile_path = directory.path().join("profile.json");
    let campaign_path = directory.path().join("campaign.json");
    let report_path = directory.path().join("report.json");
    let profile = complete_profile();
    let prepared = PreparedPlatformDriverDisplayQualification::compile(profile.clone())
        .expect("profile must compile");
    fs::write(
        &profile_path,
        serde_json::to_vec_pretty(&profile).expect("profile must serialize"),
    )
    .expect("profile must be written");
    fs::write(
        &campaign_path,
        serde_json::to_vec_pretty(&complete_campaign(&profile, &prepared))
            .expect("campaign must serialize"),
    )
    .expect("campaign must be written");

    let report = evaluate_sealed_files(&profile_path, &campaign_path, &report_path)
        .expect("sealed files must evaluate");
    assert!(report.verify_evidence());
    let written: serde_json::Value =
        read_bounded_json(&report_path, 8 * 1024 * 1024).expect("written report must parse");
    assert_eq!(written["evidence_sha256"], report.evidence_sha256);
    assert!(evaluate_sealed_files(&profile_path, &campaign_path, &report_path).is_err());
}

#[test]
fn supervisors_bind_rows_builds_owner_verifiers_and_self_contained_bundles() {
    for required in [
        "BuildManifestPath",
        "common_release_candidate_required",
        "common_build_manifest_required",
        "build_provenance",
        "runtime_image",
        "owner_verifier_script",
        "sealed-row.json",
        "source_evidence",
        "environment_snapshots",
    ] {
        assert!(
            ROW_SUPERVISOR.contains(required),
            "row supervisor lost {required}"
        );
    }
    for required in [
        "sealed_row_path",
        "cell_observation_sha256",
        "row_artifact_manifest_sha256",
        "resolved-campaign.json",
        "evidence-closure.json",
        "MONDRIAN_PLATFORM_QUALIFICATION_REPLAY_EXECUTABLE",
        "owner_verifier_script",
        "runtime-image",
        "source_evidence.entries",
        "environment_snapshots",
    ] {
        assert!(
            MATRIX_RESOLVER.contains(required),
            "matrix resolver lost {required}"
        );
    }
    for required in [
        "platform-display-probe-source-replay-v1",
        "gpu-color-source-replay-v1",
        "viewer-display-source-replay-v1",
        "capture_authority_manifest_required",
        "platform-gpu-color-gates.json",
    ] {
        assert!(
            MATRIX_POLICY.contains(required),
            "matrix policy lost {required}"
        );
    }
    for required in [
        "platform producer source role closure",
        "GPU source role closure",
        "Viewer source role closure",
        "display_output_contract",
        "qualification_record_sequence",
        "Invoke-DisplayContractReplay",
        "measurement_sha256",
        "producer_executable_sha256",
        "Native producer did not qualify scenario",
        "Invoke-DisplayCalibrationReplay",
    ] {
        assert!(
            SOURCE_VERIFIER.contains(required),
            "source verifier lost {required}"
        );
    }
    for required in [
        "platform-display-probe-v1",
        "gpu-color-qualification-v1",
        "viewer-display-qualification-v1",
        "zero_readback_stages",
        "capability_skip_observed",
        "executed_runtime_image_sha256",
    ] {
        assert!(
            LANE_VERIFIER.contains(required),
            "lane verifier lost {required}"
        );
    }
    for required in [
        "bundle top-level closure",
        "bundled evidence file closure",
        "build-manifest",
        "total_evidence_bytes",
        "ExpectedSourceSha",
        "ExpectedRuntimeProfilePath",
        "ExpectedVerifierToolsManifestSha256",
        "platform_qualification_replay",
        "Replayed Matrix evaluator report differs",
        "environment-before",
        "source_evidence.entries",
        "ExpectedCaptureAuthorityManifestSha256",
        "Copy-AdmittedFile",
        "bundled evidence final recheck",
        "trusted-source.zip",
        "Assert-AnchorSnapshotUnchanged",
    ] {
        assert!(
            BUNDLE_VERIFIER.contains(required),
            "bundle verifier lost {required}"
        );
    }
    for required in [
        "MONDRIAN_GPU_COLOR_GATE_MEASUREMENT_OUTPUT",
        "test result: ok\\. 1 passed",
        "measurement_sha256",
        "source-evidence-template.json",
        "MONDRIAN_QUALIFICATION_CHALLENGE_ID",
        "test_executable_sha256",
    ] {
        assert!(
            GPU_SOURCE_SUPERVISOR.contains(required),
            "GPU source supervisor lost {required}"
        );
    }
    for required in [
        "mondrian-platform-display-probe-source",
        "authority-challenge.json",
        "capture-plan.json",
        "producer-binary",
        "source-evidence-template.json",
        "TransitionAcknowledgementDirectory",
        "resolved_native_display_path_id",
    ] {
        assert!(
            PLATFORM_SOURCE_SUPERVISOR.contains(required),
            "platform source supervisor lost {required}"
        );
    }
    for required in [
        "standard-all-views-accuracy",
        "max_delta_e_itp",
        "all_transforms_within_budget",
        "diagnostic_skip_forbidden",
    ] {
        assert!(
            GPU_SOURCE_PROFILE.contains(required),
            "GPU source profile lost {required}"
        );
    }
    assert!(!MATRIX_RESOLVER.contains("ProductArtifactPath"));
}

#[test]
fn complete_cross_machine_matrix_is_qualified_and_order_independent() {
    let profile = complete_profile();
    let prepared = PreparedPlatformDriverDisplayQualification::compile(profile.clone())
        .expect("profile must compile");
    let mut campaign = complete_campaign(&profile, &prepared);

    let first = prepared.evaluate(campaign.clone()).expect("campaign must qualify");
    campaign.cells.reverse();
    for cell in &mut campaign.cells {
        cell.probe_backends.reverse();
        cell.reports.reverse();
        cell.scenarios.reverse();
    }
    let reordered = prepared.evaluate(campaign).expect("reordered campaign must qualify");

    assert_eq!(first.status, PlatformQualificationStatus::Qualified);
    assert!(first.missing_cells.is_empty());
    assert!(first.verify_evidence());
    assert_eq!(first, reordered);
}

#[test]
fn missing_cells_are_incomplete_and_an_executed_failure_dominates() {
    let profile = complete_profile();
    let prepared = PreparedPlatformDriverDisplayQualification::compile(profile.clone())
        .expect("profile must compile");
    let mut campaign = complete_campaign(&profile, &prepared);
    campaign.cells.truncate(1);

    let incomplete = prepared.evaluate(campaign.clone()).expect("missing cells are a verdict");
    assert_eq!(incomplete.status, PlatformQualificationStatus::Incomplete);
    assert_eq!(incomplete.missing_cells.len(), 2);

    campaign.cells[0].reports[0].status = PlatformQualificationStatus::Failed;
    let failed = prepared.evaluate(campaign).expect("executed failure is a verdict");
    assert_eq!(failed.status, PlatformQualificationStatus::Failed);
}

#[test]
fn reused_runs_and_cross_source_or_environment_evidence_are_rejected() {
    let profile = complete_profile();
    let prepared = PreparedPlatformDriverDisplayQualification::compile(profile.clone())
        .expect("profile must compile");

    let mut duplicate_run = complete_campaign(&profile, &prepared);
    duplicate_run.cells[1].cell_run_id = duplicate_run.cells[0].cell_run_id.clone();
    assert!(matches!(
        prepared.evaluate(duplicate_run),
        Err(PlatformQualificationError::DuplicateCellRun { .. })
    ));

    let mut mixed_source = complete_campaign(&profile, &prepared);
    mixed_source.cells[0].reports[0].source_revision = "f".repeat(40);
    assert!(matches!(
        prepared.evaluate(mixed_source),
        Err(PlatformQualificationError::ReportBindingMismatch { .. })
    ));

    let mut changed_driver = complete_campaign(&profile, &prepared);
    changed_driver.cells[0].environment.driver = PlatformQualificationDriverIdentity::Explicit {
        name: "vendor-driver".to_owned(),
        version: "32.0.1".to_owned(),
    };
    assert!(matches!(
        prepared.evaluate(changed_driver),
        Err(PlatformQualificationError::EnvironmentMismatch { .. })
    ));

    let mut drifted = complete_campaign(&profile, &prepared);
    drifted.cells[0].environment_after_sha256 = "a".repeat(64);
    assert!(matches!(
        prepared.evaluate(drifted),
        Err(PlatformQualificationError::EnvironmentDrift { .. })
    ));

    let mut different_product = complete_campaign(&profile, &prepared);
    different_product.cells[0].product_artifact.sha256 = "b".repeat(64);
    assert!(matches!(
        prepared.evaluate(different_product),
        Err(PlatformQualificationError::ReportBindingMismatch { .. })
    ));

    let mut different_runtime_image = complete_campaign(&profile, &prepared);
    different_runtime_image.cells[0].product_artifact.runtime_image_sha256 = "c".repeat(64);
    assert!(matches!(
        prepared.evaluate(different_runtime_image),
        Err(PlatformQualificationError::ReportBindingMismatch { .. })
    ));

    let mut relabeled_release = complete_campaign(&profile, &prepared);
    relabeled_release.release_candidate_id = "mondrian-2026.1-rc2".to_owned();
    assert!(matches!(
        prepared.evaluate(relabeled_release),
        Err(PlatformQualificationError::BuildBindingMismatch { .. })
    ));

    let mut relabeled_manifest = complete_campaign(&profile, &prepared);
    relabeled_manifest.build_manifest_sha256 = "b".repeat(64);
    assert!(matches!(
        prepared.evaluate(relabeled_manifest),
        Err(PlatformQualificationError::BuildBindingMismatch { .. })
    ));

    let mut self_labeled_owner = complete_campaign(&profile, &prepared);
    self_labeled_owner.cells[0].reports[0].owner = "untrusted-wrapper".to_owned();
    assert!(matches!(
        prepared.evaluate(self_labeled_owner),
        Err(PlatformQualificationError::ReportBindingMismatch { .. })
    ));
}

#[test]
fn profile_and_scenario_contracts_fail_closed() {
    let mut drm_only = complete_profile();
    drm_only.cells[2].required_probe_backends = vec![DisplayProbeBackend::LinuxDrmSysfs];
    assert!(matches!(
        PreparedPlatformDriverDisplayQualification::compile(drm_only),
        Err(PlatformQualificationError::InvalidProbeClosure { .. })
    ));

    let mut software = complete_profile();
    software.cells[0].environment.adapter_kind = QualificationAdapterKind::Software;
    assert!(matches!(
        PreparedPlatformDriverDisplayQualification::compile(software),
        Err(PlatformQualificationError::SoftwareAdapter)
    ));

    let mut wrong_macos_driver = complete_profile();
    wrong_macos_driver.cells[1].environment.driver =
        PlatformQualificationDriverIdentity::Explicit {
            name: "windows-shaped-driver".to_owned(),
            version: "1.0".to_owned(),
        };
    assert!(matches!(
        PreparedPlatformDriverDisplayQualification::compile(wrong_macos_driver),
        Err(PlatformQualificationError::InvalidDriverIdentity)
    ));

    let mut false_native_pq_macos = complete_profile();
    let mac_hdr = false_native_pq_macos.cells[1]
        .required_scenarios
        .iter_mut()
        .find(|requirement| requirement.scenario == QualificationDisplayScenario::HdrPq)
        .expect("macOS HDR scenario must exist");
    mac_hdr.transfer = QualificationPresentationTransfer::SurfaceCodeValues;
    mac_hdr.minimum_bits_per_color_channel = Some(10);
    mac_hdr.minimum_peak_luminance_nits = Some(400);
    mac_hdr.minimum_edr_headroom_ppm = None;
    mac_hdr.hdr_presentation = Some(QualificationHdrPresentation::NativePq);
    assert!(matches!(
        PreparedPlatformDriverDisplayQualification::compile(false_native_pq_macos),
        Err(PlatformQualificationError::InvalidCellRequirements { .. })
    ));

    let mut relabeled_surface = complete_profile();
    relabeled_surface.cells[0].required_scenarios[0].surface_color_space =
        QualificationSurfaceColorSpace::Bt2100Pq;
    assert!(matches!(
        PreparedPlatformDriverDisplayQualification::compile(relabeled_surface),
        Err(PlatformQualificationError::InvalidCellRequirements { .. })
    ));

    let profile = complete_profile();
    let prepared = PreparedPlatformDriverDisplayQualification::compile(profile.clone())
        .expect("profile must compile");
    for scenario in [
        QualificationDisplayScenario::DisplayP3,
        QualificationDisplayScenario::HdrPq,
        QualificationDisplayScenario::ManagedIcc,
    ] {
        let mut campaign = complete_campaign(&profile, &prepared);
        let evidence = campaign.cells[0]
            .scenarios
            .iter_mut()
            .find(|evidence| evidence.scenario == scenario)
            .expect("scenario must exist");
        match scenario {
            QualificationDisplayScenario::DisplayP3 => evidence.wide_color_active = Some(false),
            QualificationDisplayScenario::HdrPq => {
                evidence.active_hdr_transfer = Some(QualificationHdrTransferFunction::Hlg);
            }
            QualificationDisplayScenario::ManagedIcc => evidence.icc_profile_sha256 = None,
            QualificationDisplayScenario::SdrSrgb => unreachable!(),
        }
        assert!(matches!(
            prepared.evaluate(campaign),
            Err(PlatformQualificationError::ScenarioContractMismatch { scenario: actual, .. })
                if actual == scenario
        ));
    }

    let mut readback = complete_campaign(&profile, &prepared);
    readback.cells[0].scenarios[0].zero_readback_stages = false;
    assert!(matches!(
        prepared.evaluate(readback),
        Err(PlatformQualificationError::ScenarioContractMismatch { .. })
    ));
}

#[test]
fn schema_is_strict_and_report_digest_detects_tampering() {
    let profile = complete_profile();
    let mut json = serde_json::to_value(&profile).expect("profile must serialize");
    json.as_object_mut()
        .expect("profile must be an object")
        .insert("undeclared".to_owned(), serde_json::json!(true));
    assert!(serde_json::from_value::<PlatformDriverDisplayQualificationProfile>(json).is_err());

    let prepared = PreparedPlatformDriverDisplayQualification::compile(profile.clone())
        .expect("profile must compile");
    let mut report = prepared
        .evaluate(complete_campaign(&profile, &prepared))
        .expect("campaign must qualify");
    assert!(report.verify_evidence());
    report.campaign_id.push_str("-tampered");
    assert!(!report.verify_evidence());
}

#[test]
fn linux_scenario_union_may_span_wayland_and_x11_rows() {
    let mut profile = complete_profile();
    let linux = profile.cells.pop().expect("Linux reference cell must exist");
    let mut x11 = linux.clone();
    x11.cell_id = "linux-vulkan-x11-reference".to_owned();
    x11.environment.window_system = "X11".to_owned();
    x11.environment.compositor = "KWin X11 6.0".to_owned();
    x11.environment.display_identity = "linux-x11-opaque-display".to_owned();
    x11.environment.display_inventory_sha256 = digest("linux-x11-display-inventory");
    x11.required_probe_backends = vec![DisplayProbeBackend::X11RootProperty];
    x11.required_scenarios.retain(|scenario| {
        matches!(
            scenario.scenario,
            QualificationDisplayScenario::SdrSrgb | QualificationDisplayScenario::ManagedIcc
        )
    });

    let mut wayland = linux;
    wayland.required_scenarios.retain(|scenario| {
        matches!(
            scenario.scenario,
            QualificationDisplayScenario::DisplayP3 | QualificationDisplayScenario::HdrPq
        )
    });
    profile.cells.extend([x11, wayland]);

    PreparedPlatformDriverDisplayQualification::compile(profile)
        .expect("Linux platform scenario coverage may be split across exact rows");
}

#[test]
fn proprietary_linux_stack_does_not_require_a_mesa_version() {
    let mut profile = complete_profile();
    profile.cells[2].environment.driver = PlatformQualificationDriverIdentity::LinuxStack {
        kernel_version: "6.11.0-26-generic".to_owned(),
        drm_driver: "nvidia-drm 580.65.06".to_owned(),
        vulkan_driver: "NVIDIA".to_owned(),
        vulkan_driver_version: "580.65.06".to_owned(),
        mesa_version: None,
    };

    PreparedPlatformDriverDisplayQualification::compile(profile)
        .expect("proprietary Linux Vulkan stacks must remain representable");
}

#[test]
fn target_specific_artifact_cannot_be_relabelled_across_platforms() {
    let profile = complete_profile();
    let prepared = PreparedPlatformDriverDisplayQualification::compile(profile.clone())
        .expect("profile must compile");
    let mut campaign = complete_campaign(&profile, &prepared);
    campaign.cells[0].product_artifact.target_triple = "x86_64-unknown-linux-gnu".to_owned();
    campaign.cells[0].product_artifact.package_kind = "linux-appimage".to_owned();
    assert!(matches!(
        prepared.evaluate(campaign),
        Err(PlatformQualificationError::InvalidProductArtifact)
    ));

    let mut wrong_architecture = complete_campaign(&profile, &prepared);
    wrong_architecture.cells[1].environment.architecture = "x86_64".to_owned();
    assert!(matches!(
        prepared.evaluate(wrong_architecture),
        Err(PlatformQualificationError::EnvironmentMismatch { .. })
    ));
}

fn complete_profile() -> PlatformDriverDisplayQualificationProfile {
    PlatformDriverDisplayQualificationProfile {
        schema_version: 1,
        qualification_id: "commercial-platform-driver-display".to_owned(),
        edition: "2026.1".to_owned(),
        cells: vec![
            cell_requirement(
                "windows-dx12-reference",
                QualificationPlatform::Windows,
                QualificationGraphicsBackend::Dx12,
                "Windows 11 24H2",
                "26100.3915",
                "Win32",
                "DWM 10.0.26100",
                PlatformQualificationDriverIdentity::Explicit {
                    name: "vendor-driver".to_owned(),
                    version: "32.0.0".to_owned(),
                },
                vec![
                    DisplayProbeBackend::WindowsWcs,
                    DisplayProbeBackend::WindowsDisplayConfig,
                ],
            ),
            cell_requirement(
                "macos-metal-reference",
                QualificationPlatform::MacOs,
                QualificationGraphicsBackend::Metal,
                "macOS 15.5",
                "24F74",
                "Quartz",
                "WindowServer 645.3",
                PlatformQualificationDriverIdentity::OsBundled { os_build: "24F74".to_owned() },
                vec![
                    DisplayProbeBackend::MacOsCoreGraphics,
                    DisplayProbeBackend::MacOsAppKit,
                ],
            ),
            cell_requirement(
                "linux-vulkan-wayland-reference",
                QualificationPlatform::Linux,
                QualificationGraphicsBackend::Vulkan,
                "Ubuntu 24.04.2 LTS",
                "6.11.0-26-generic",
                "Wayland",
                "Mutter 46.2",
                PlatformQualificationDriverIdentity::LinuxStack {
                    kernel_version: "6.11.0-26-generic".to_owned(),
                    drm_driver: "amdgpu 6.11.0".to_owned(),
                    vulkan_driver: "RADV".to_owned(),
                    vulkan_driver_version: "24.2.8".to_owned(),
                    mesa_version: Some("24.2.8".to_owned()),
                },
                vec![DisplayProbeBackend::WaylandColorManagementV1],
            ),
        ],
        limits: PlatformQualificationLimits { max_cells: 8, max_items_per_cell: 12 },
    }
}

#[allow(clippy::too_many_arguments)]
fn cell_requirement(
    cell_id: &str,
    platform: QualificationPlatform,
    graphics_backend: QualificationGraphicsBackend,
    os_version: &str,
    os_build: &str,
    window_system: &str,
    compositor: &str,
    driver: PlatformQualificationDriverIdentity,
    required_probe_backends: Vec<DisplayProbeBackend>,
) -> PlatformQualificationCellRequirement {
    PlatformQualificationCellRequirement {
        cell_id: cell_id.to_owned(),
        environment: PlatformQualificationEnvironment {
            platform,
            architecture: match platform {
                QualificationPlatform::MacOs => "aarch64",
                QualificationPlatform::Windows | QualificationPlatform::Linux => "x86_64",
            }
            .to_owned(),
            os_version: os_version.to_owned(),
            os_build: os_build.to_owned(),
            window_system: window_system.to_owned(),
            compositor: compositor.to_owned(),
            graphics_backend,
            adapter_name: format!("{cell_id}-physical-gpu"),
            adapter_vendor: "exact-vendor-id".to_owned(),
            adapter_device_id: format!("{cell_id}-device-id"),
            renderer_driver: format!("{cell_id}-renderer-driver"),
            renderer_driver_info: format!("{cell_id}-renderer-driver-info"),
            adapter_kind: QualificationAdapterKind::Hardware,
            driver,
            display_identity: format!("{cell_id}-opaque-display"),
            native_display_path_id: format!("{cell_id}-native-display-path"),
            display_inventory_sha256: digest(format!("{cell_id}-display-inventory")),
        },
        required_probe_backends,
        required_scenarios: scenario_requirements(platform),
        required_evidence: vec![
            evidence_requirement(
                PlatformQualificationEvidenceKind::PlatformProbe,
                "mondrian-platform",
                "platform-display-probe-v1",
            ),
            evidence_requirement(
                PlatformQualificationEvidenceKind::GpuColor,
                "mondrian-renderer",
                "gpu-color-qualification-v1",
            ),
            evidence_requirement(
                PlatformQualificationEvidenceKind::ViewerDisplay,
                "mondrian-app",
                "viewer-display-qualification-v1",
            ),
        ],
    }
}

fn evidence_requirement(
    kind: PlatformQualificationEvidenceKind,
    owner: &str,
    verifier_id: &str,
) -> PlatformQualificationEvidenceRequirement {
    PlatformQualificationEvidenceRequirement {
        kind,
        owner: owner.to_owned(),
        verifier_id: verifier_id.to_owned(),
        report_schema_version: 1,
    }
}

fn scenario_requirements(
    platform: QualificationPlatform,
) -> Vec<PlatformQualificationScenarioRequirement> {
    let sdr_bits = (platform == QualificationPlatform::Windows).then_some(8);
    let wide_color_bits = (platform == QualificationPlatform::Windows).then_some(10);
    vec![
        scenario_requirement(
            QualificationDisplayScenario::SdrSrgb,
            QualificationSurfaceColorSpace::Srgb,
            QualificationPresentationTransfer::SurfaceCodeValues,
            sdr_bits,
            None,
            None,
            None,
        ),
        scenario_requirement(
            QualificationDisplayScenario::DisplayP3,
            QualificationSurfaceColorSpace::DisplayP3,
            QualificationPresentationTransfer::SurfaceCodeValues,
            wide_color_bits,
            None,
            None,
            None,
        ),
        if platform == QualificationPlatform::MacOs {
            scenario_requirement(
                QualificationDisplayScenario::HdrPq,
                QualificationSurfaceColorSpace::ExtendedLinearEdr,
                QualificationPresentationTransfer::ExtendedLinearValues,
                None,
                None,
                Some(1_250_000),
                Some(QualificationHdrPresentation::MacOsEdr),
            )
        } else {
            scenario_requirement(
                QualificationDisplayScenario::HdrPq,
                QualificationSurfaceColorSpace::Bt2100Pq,
                QualificationPresentationTransfer::SurfaceCodeValues,
                wide_color_bits,
                Some(400),
                None,
                Some(QualificationHdrPresentation::NativePq),
            )
        },
        scenario_requirement(
            QualificationDisplayScenario::ManagedIcc,
            QualificationSurfaceColorSpace::Srgb,
            QualificationPresentationTransfer::DeviceCodeValues,
            sdr_bits,
            None,
            None,
            None,
        ),
    ]
}

fn scenario_requirement(
    scenario: QualificationDisplayScenario,
    surface_color_space: QualificationSurfaceColorSpace,
    transfer: QualificationPresentationTransfer,
    minimum_bits_per_color_channel: Option<u8>,
    minimum_peak_luminance_nits: Option<u32>,
    minimum_edr_headroom_ppm: Option<u32>,
    hdr_presentation: Option<QualificationHdrPresentation>,
) -> PlatformQualificationScenarioRequirement {
    PlatformQualificationScenarioRequirement {
        scenario,
        surface_color_space,
        transfer,
        minimum_bits_per_color_channel,
        minimum_peak_luminance_nits,
        minimum_edr_headroom_ppm,
        hdr_presentation,
    }
}

fn complete_campaign(
    profile: &PlatformDriverDisplayQualificationProfile,
    prepared: &PreparedPlatformDriverDisplayQualification,
) -> PlatformQualificationCampaign {
    PlatformQualificationCampaign {
        campaign_id: "commercial-release-candidate-2026-1".to_owned(),
        source_revision: SOURCE_REVISION.to_owned(),
        release_candidate_id: "mondrian-2026.1-rc1".to_owned(),
        build_manifest_sha256: BUILD_MANIFEST_SHA256.to_owned(),
        cells: profile
            .cells
            .iter()
            .enumerate()
            .map(|(index, cell)| observation(cell, index, prepared.profile_sha256()))
            .collect(),
    }
}

fn observation(
    requirement: &PlatformQualificationCellRequirement,
    index: usize,
    profile_sha256: &str,
) -> PlatformQualificationCellObservation {
    let cell_run_id = format!("{}-run-{}", requirement.cell_id, index + 1);
    let machine_report_sha256 = digest(format!("{}-machine", requirement.cell_id));
    let environment_sha256 = digest_serializable(&requirement.environment);
    let product_artifact = product_artifact(requirement.environment.platform);
    let viewer_report_sha256 = digest(format!("{}-viewer", requirement.cell_id));
    let reports = [
        PlatformQualificationEvidenceKind::PlatformProbe,
        PlatformQualificationEvidenceKind::GpuColor,
        PlatformQualificationEvidenceKind::ViewerDisplay,
    ]
    .into_iter()
    .map(|kind| PlatformQualificationEvidenceReport {
        kind,
        owner: match kind {
            PlatformQualificationEvidenceKind::PlatformProbe => "mondrian-platform",
            PlatformQualificationEvidenceKind::GpuColor => "mondrian-renderer",
            PlatformQualificationEvidenceKind::ViewerDisplay => "mondrian-app",
        }
        .to_owned(),
        verifier_id: match kind {
            PlatformQualificationEvidenceKind::PlatformProbe => "platform-display-probe-v1",
            PlatformQualificationEvidenceKind::GpuColor => "gpu-color-qualification-v1",
            PlatformQualificationEvidenceKind::ViewerDisplay => "viewer-display-qualification-v1",
        }
        .to_owned(),
        profile_sha256: profile_sha256.to_owned(),
        report_schema_version: 1,
        status: PlatformQualificationStatus::Qualified,
        report_sha256: if kind == PlatformQualificationEvidenceKind::ViewerDisplay {
            viewer_report_sha256.clone()
        } else {
            digest(format!("{}-{kind:?}-report", requirement.cell_id))
        },
        raw_evidence_sha256: digest(format!("{}-{kind:?}-raw", requirement.cell_id)),
        product_artifact_sha256: product_artifact.sha256.clone(),
        runtime_image_sha256: product_artifact.runtime_image_sha256.clone(),
        release_candidate_id: "mondrian-2026.1-rc1".to_owned(),
        build_manifest_sha256: BUILD_MANIFEST_SHA256.to_owned(),
        build_provenance_sha256: product_artifact.build_provenance_sha256.clone(),
        source_revision: SOURCE_REVISION.to_owned(),
        machine_report_sha256: machine_report_sha256.clone(),
        environment_sha256: environment_sha256.clone(),
        cell_run_id: cell_run_id.clone(),
    })
    .collect();
    let scenarios = requirement
        .required_scenarios
        .iter()
        .map(|scenario| {
            scenario_evidence(
                scenario,
                &viewer_report_sha256,
                &environment_sha256,
                &requirement.cell_id,
            )
        })
        .collect();
    PlatformQualificationCellObservation {
        cell_id: requirement.cell_id.clone(),
        cell_run_id,
        source_revision: SOURCE_REVISION.to_owned(),
        machine_report_sha256,
        environment: requirement.environment.clone(),
        product_artifact,
        release_candidate_id: "mondrian-2026.1-rc1".to_owned(),
        build_manifest_sha256: BUILD_MANIFEST_SHA256.to_owned(),
        environment_before_sha256: environment_sha256.clone(),
        environment_after_sha256: environment_sha256,
        probe_backends: requirement.required_probe_backends.clone(),
        reports,
        scenarios,
    }
}

fn scenario_evidence(
    requirement: &PlatformQualificationScenarioRequirement,
    viewer_report_sha256: &str,
    environment_sha256: &str,
    cell_id: &str,
) -> PlatformQualificationScenarioEvidence {
    let is_p3 = requirement.scenario == QualificationDisplayScenario::DisplayP3;
    let is_pq = requirement.scenario == QualificationDisplayScenario::HdrPq;
    let is_icc = requirement.scenario == QualificationDisplayScenario::ManagedIcc;
    PlatformQualificationScenarioEvidence {
        scenario: requirement.scenario,
        status: PlatformQualificationStatus::Qualified,
        report_sha256: viewer_report_sha256.to_owned(),
        output_contract_sha256: digest(format!("{cell_id}-{:?}-contract", requirement.scenario)),
        environment_sha256: environment_sha256.to_owned(),
        operator_attestation_sha256: digest(format!(
            "{cell_id}-{:?}-operator",
            requirement.scenario
        )),
        viewer_ready: true,
        display_contract_valid: true,
        external_texture_presented: true,
        zero_readback_stages: true,
        carrier_reuse_observed: is_p3 || is_pq,
        operator_observation_passed: true,
        capability_skip_observed: false,
        surface_color_space: requirement.surface_color_space,
        transfer: requirement.transfer,
        bits_per_color_channel: requirement.minimum_bits_per_color_channel,
        wide_color_supported: is_p3.then_some(true),
        wide_color_active: is_p3.then_some(true),
        hdr_supported: is_pq.then_some(true),
        hdr_enabled: is_pq.then_some(true),
        active_hdr_transfer: (is_pq
            && requirement.hdr_presentation == Some(QualificationHdrPresentation::NativePq))
        .then_some(QualificationHdrTransferFunction::Pq),
        peak_luminance_nits: requirement.minimum_peak_luminance_nits,
        edr_headroom_ppm: requirement.minimum_edr_headroom_ppm,
        hdr_presentation: requirement.hdr_presentation,
        icc_profile_sha256: is_icc.then(|| digest(format!("{cell_id}-icc-profile"))),
        icc_processor_sha256: is_icc.then(|| digest(format!("{cell_id}-icc-processor"))),
    }
}

fn product_artifact(platform: QualificationPlatform) -> PlatformQualificationProductArtifact {
    let (target_triple, package_kind) = match platform {
        QualificationPlatform::Windows => ("x86_64-pc-windows-msvc", "windows-exe"),
        QualificationPlatform::MacOs => ("aarch64-apple-darwin", "macos-dmg"),
        QualificationPlatform::Linux => ("x86_64-unknown-linux-gnu", "linux-appimage"),
    };
    PlatformQualificationProductArtifact {
        platform,
        target_triple: target_triple.to_owned(),
        package_kind: package_kind.to_owned(),
        sha256: digest(target_triple),
        runtime_image_sha256: digest(format!("{target_triple}-runtime-image")),
        build_provenance_sha256: digest(format!("{target_triple}-build-provenance")),
    }
}

fn digest_serializable(value: &impl Serialize) -> String {
    digest(serde_json::to_vec(value).expect("test value must serialize"))
}

fn digest(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value.as_ref()))
}

fn evaluate_sealed_files(
    profile_path: &Path,
    campaign_path: &Path,
    report_path: &Path,
) -> Result<PlatformQualificationReport, String> {
    let profile: PlatformDriverDisplayQualificationProfile =
        read_bounded_json(profile_path, 1024 * 1024)?;
    let campaign: PlatformQualificationCampaign =
        read_bounded_json(campaign_path, 8 * 1024 * 1024)?;
    let prepared = PreparedPlatformDriverDisplayQualification::compile(profile)
        .map_err(|error| error.to_string())?;
    let report = prepared.evaluate(campaign).map_err(|error| error.to_string())?;
    if report.status != PlatformQualificationStatus::Qualified
        || !report.missing_cells.is_empty()
        || !report.verify_evidence()
    {
        return Err("sealed campaign is not complete and qualified".to_owned());
    }
    let bytes = serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(report_path)
        .map_err(|error| error.to_string())?;
    output.write_all(&bytes).map_err(|error| error.to_string())?;
    output.sync_all().map_err(|error| error.to_string())?;
    Ok(report)
}

fn read_bounded_json<T: serde::de::DeserializeOwned>(
    path: &Path,
    maximum_bytes: u64,
) -> Result<T, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > maximum_bytes {
        return Err("sealed JSON input is outside its size bound".to_owned());
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}
