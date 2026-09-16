//! Platform display probing abstraction for the Display Output Contract v2.
//!
//! This module provides a `PlatformDisplayProbe` trait that abstracts OS-level
//! display queries (monitor identity, ICC profile, HDR capability) into a
//! testable interface. Real platform implementations live in the app layer;
//! tests use fake providers that return deterministic snapshots.
//!
//! ## Design principles
//!
//! 1. **No OS queries scattered in UI logic.** All platform display probing
//!    goes through this trait.
//! 2. **Fake providers for tests.** Core display contract logic must be testable
//!    without a real monitor.
//! 3. **Fail-closed for unknown capabilities.** If the platform cannot provide
//!    ICC/HDR data, the probe returns structured `Unknown`/`Unsupported` statuses.

use crate::color_models::{
    DisplayCalibrationPolicy, DisplayManagementPolicy, MonitorOutputIntent, ViewerDisplayMode,
};
use crate::display_contract::*;
use crate::types::ColorSpace;

/// Provider of platform display information for the Display Output Contract v2.
///
/// Implementations query the OS (via winit/wgpu) for monitor identity, surface
/// capabilities, ICC profile status, and HDR metadata. Tests use
/// `FakeDisplayProbe` to return deterministic snapshots.
pub trait PlatformDisplayProbe {
    /// Build a display output snapshot from the current platform state.
    ///
    /// This queries:
    /// - winit window's current monitor for display identity/position/size
    /// - wgpu surface capabilities for format/color-space support
    /// - wgpu display_hdr_info for HDR metadata
    /// - OS ICC profile status from the active platform adapter
    /// - The user's display management policy for viewer mode / monitor profile
    ///
    /// Returns a complete `DisplayOutputSnapshot` with validation status,
    /// blockers, and warnings.
    fn current_display_snapshot(
        &self,
        policy: &DisplayManagementPolicy,
        output_color_space: ColorSpace,
    ) -> DisplayOutputSnapshot;

    /// Whether the platform can discover OS-level ICC profiles.
    fn supports_os_icc_discovery(&self) -> bool;

    /// Whether the platform can query OS-level HDR display metadata.
    fn supports_os_hdr_metadata(&self) -> bool;
}

/// Fake display probe for testing display contract logic without a real monitor.
///
/// Returns pre-configured snapshots that allow tests to verify:
/// - ICC fail-closed behavior
/// - HDR diagnosis chain
/// - Monitor switch invalidation
/// - SDR normal path
/// - Diagnostics serialization
#[derive(Debug, Clone)]
pub struct FakeDisplayProbe {
    /// The snapshot to return from `current_display_snapshot()`.
    pub snapshot: DisplayOutputSnapshot,
    /// Whether OS ICC discovery is supported.
    pub os_icc_discovery: bool,
    /// Whether OS HDR metadata is supported.
    pub os_hdr_metadata: bool,
}

impl FakeDisplayProbe {
    /// Create a fake probe that returns a default SDR pass snapshot.
    pub fn sdr_pass() -> Self {
        Self {
            snapshot: DisplayOutputSnapshot {
                display_management_policy: DisplayManagementPolicy::default(),
                display_id: DisplayId {
                    name: Some("Fake Monitor".to_owned()),
                    position: (0, 0),
                    physical_size: (3840, 2160),
                },
                platform: DisplayPlatform::Windows,
                scale_factor: ScaleFactorPpm::from_f64(1.0),
                surface_format: "Bgra8UnormSrgb".to_owned(),
                surface_color_space: "Srgb".to_owned(),
                surface_hdr_mode: "SdrOnly".to_owned(),
                supported_surface_color_spaces: vec!["Srgb".to_owned()],
                requested_viewer_mode: "Sdr".to_owned(),
                requested_output_color_space: "Rec709".to_owned(),
                resolved_output_color_space: "Rec709".to_owned(),
                ocio_display: Some("sRGB".to_owned()),
                ocio_view: Some("sRGB".to_owned()),
                monitor_profile_status: MonitorProfileStatus::ManagedColorSpace {
                    color_space: ColorSpace::Srgb,
                    source: MonitorProfileSource::UserConfigured,
                },
                hdr_status: HdrStatus::NotRequested,
                validation_status: DisplayValidationStatus::Pass,
                warnings: vec![],
                blockers: vec![],
                refresh_reason: "TestInit".to_owned(),
            },
            os_icc_discovery: false,
            os_hdr_metadata: false,
        }
    }

    /// Create a fake probe where ICC profile is requested but unsupported.
    pub fn icc_profile_unsupported() -> Self {
        let mut s = Self::sdr_pass();
        s.snapshot.monitor_profile_status = MonitorProfileStatus::IccProfileUnsupported {
            feature_code: "os_icc_profile".to_owned(),
            profile_path: None,
            reason: "OS ICC profile discovery not implemented".to_owned(),
        };
        s.snapshot.blockers.push(DisplayOutputBlocker::MonitorIccProfileUnsupported {
            feature_code: "os_icc_profile".to_owned(),
            profile_path: None,
            reason: "OS ICC profile discovery not implemented".to_owned(),
        });
        s.snapshot.validation_status = DisplayValidationStatus::Fail;
        s
    }

    /// Create a fake probe where ICC profile is invalid.
    pub fn icc_profile_invalid(path: &str) -> Self {
        let mut s = Self::sdr_pass();
        s.snapshot.monitor_profile_status = MonitorProfileStatus::IccProfileReadError {
            profile_path: Some(path.to_owned()),
            reason: "failed to parse ICC profile".to_owned(),
        };
        s.snapshot.blockers.push(DisplayOutputBlocker::MonitorIccProfileInvalid {
            profile_path: Some(path.to_owned()),
            reason: "failed to parse ICC profile".to_owned(),
        });
        s.snapshot.validation_status = DisplayValidationStatus::Fail;
        s
    }

    /// Create a fake probe where ICC profile was read but cannot be mapped to OCIO.
    pub fn icc_profile_unmapped(path: &str) -> Self {
        let mut s = Self::sdr_pass();
        s.snapshot.monitor_profile_status = MonitorProfileStatus::IccProfileUnmapped {
            profile_path: Some(path.to_owned()),
            parsed_color_space: Some(ColorSpace::DisplayP3),
            reason: "no OCIO display matches Display P3".to_owned(),
        };
        s.snapshot.blockers.push(DisplayOutputBlocker::MonitorIccProfileUnmapped {
            profile_path: Some(path.to_owned()),
            parsed_color_space: Some(ColorSpace::DisplayP3),
            reason: "no OCIO display matches Display P3".to_owned(),
        });
        s.snapshot.validation_status = DisplayValidationStatus::Fail;
        s
    }

    /// Create a fake probe where HDR is requested but monitor capability is unknown.
    pub fn hdr_monitor_unknown() -> Self {
        let mut s = Self::sdr_pass();
        s.snapshot.requested_viewer_mode = "HdrPq".to_owned();
        s.snapshot.resolved_output_color_space = "Rec2100Pq".to_owned();
        s.snapshot.hdr_status = HdrStatus::RequestedMonitorUnknown {
            mode: "HdrPq".to_owned(),
            reason: "display_hdr_info unavailable".to_owned(),
        };
        s.snapshot.blockers.push(DisplayOutputBlocker::MonitorHdrCapabilityUnknown {
            reason: "display_hdr_info unavailable".to_owned(),
        });
        s.snapshot.validation_status = DisplayValidationStatus::Fail;
        s
    }

    /// Create a fake probe where HDR is requested but surface doesn't support it.
    pub fn hdr_surface_unsupported() -> Self {
        let mut s = Self::sdr_pass();
        s.snapshot.requested_viewer_mode = "HdrPq".to_owned();
        s.snapshot.resolved_output_color_space = "Rec2100Pq".to_owned();
        s.snapshot.hdr_status = HdrStatus::RequestedSurfaceUnsupported {
            mode: "HdrPq".to_owned(),
            surface_format: "Bgra8UnormSrgb".to_owned(),
            surface_color_space: "Srgb".to_owned(),
        };
        s.snapshot.blockers.push(DisplayOutputBlocker::UnsupportedHdrSwapchainOrEdr {
            hdr_mode: "HdrPq".to_owned(),
        });
        s.snapshot.validation_status = DisplayValidationStatus::Fail;
        s
    }

    /// Create a fake probe simulating a monitor switch (different display).
    pub fn monitor_switched(new_name: &str, new_position: (i32, i32)) -> Self {
        let mut s = Self::sdr_pass();
        s.snapshot.display_id.name = Some(new_name.to_owned());
        s.snapshot.display_id.position = new_position;
        s.snapshot.refresh_reason = "WindowMoved".to_owned();
        s
    }
}

impl PlatformDisplayProbe for FakeDisplayProbe {
    fn current_display_snapshot(
        &self,
        policy: &DisplayManagementPolicy,
        output_color_space: ColorSpace,
    ) -> DisplayOutputSnapshot {
        let mut snapshot = self.snapshot.clone();
        snapshot.display_management_policy = policy.clone();
        snapshot.requested_viewer_mode = format!("{:?}", policy.viewer_mode());
        snapshot.requested_output_color_space = format!("{output_color_space:?}");
        snapshot
    }

    fn supports_os_icc_discovery(&self) -> bool {
        self.os_icc_discovery
    }

    fn supports_os_hdr_metadata(&self) -> bool {
        self.os_hdr_metadata
    }
}

/// Resolve the monitor profile status from the user's display management policy
/// and the platform's ICC capabilities.
///
/// This is the fail-closed ICC resolution logic:
/// - If no ICC profile is requested → `NotRequested`
/// - If ICC is requested but OS discovery is unsupported → `IccProfileUnsupported`
/// - If ICC is requested, OS can discover, but parsing fails → `IccProfileReadError`
/// - If ICC is parsed but cannot be mapped to OCIO → `IccProfileUnmapped`
/// - If ICC is parsed and maps to a managed color space → `ManagedColorSpace`
pub fn resolve_monitor_profile_status(
    policy: &DisplayManagementPolicy,
    platform_supports_icc: bool,
) -> MonitorProfileStatus {
    match policy.calibration() {
        DisplayCalibrationPolicy::Disabled => match policy.monitor_output() {
            MonitorOutputIntent::ColorSpace(cs) => MonitorProfileStatus::ManagedColorSpace {
                color_space: *cs,
                source: MonitorProfileSource::UserConfigured,
            },
            MonitorOutputIntent::MatchProgramOutput
            | MonitorOutputIntent::OcioDisplayView { .. } => MonitorProfileStatus::NotRequested,
        },
        DisplayCalibrationPolicy::OsDefault | DisplayCalibrationPolicy::IccProfilePath(_) => {
            let profile_reference = match policy.calibration() {
                DisplayCalibrationPolicy::OsDefault => "os-default".to_owned(),
                DisplayCalibrationPolicy::IccProfilePath(path) => path.clone(),
                DisplayCalibrationPolicy::Disabled => unreachable!("matched above"),
            };
            if !platform_supports_icc {
                MonitorProfileStatus::IccProfileUnsupported {
                    feature_code: "os_icc_profile".to_owned(),
                    profile_path: Some(profile_reference),
                    reason: "OS ICC profile discovery not implemented".to_owned(),
                }
            } else {
                // Platform supports ICC but we haven't actually read the profile yet.
                // This path will be filled in by real platform implementation.
                MonitorProfileStatus::Unknown {
                    reason: "ICC profile resolution not yet implemented".to_owned(),
                }
            }
        }
    }
}

/// Resolve the HDR status from the user's viewer mode policy and surface/monitor capabilities.
///
/// This is the HDR diagnosis chain:
/// - No HDR requested → `NotRequested`
/// - HDR requested, surface supports it, monitor supports it → `RequestedSupported`
/// - HDR requested but surface doesn't support it → `RequestedSurfaceUnsupported`
/// - HDR requested, surface supports it, but monitor HDR unknown → `RequestedMonitorUnknown`
/// - HDR requested, surface supports it, but monitor doesn't support it → `RequestedMonitorUnsupported`
pub fn resolve_hdr_status(
    viewer_mode: ViewerDisplayMode,
    output_color_space: ColorSpace,
    surface_supports_hdr: bool,
    surface_hdr_mode: &str,
    monitor_hdr_known: bool,
    monitor_hdr_supported: bool,
) -> HdrStatus {
    let monitor = if !monitor_hdr_known {
        MonitorHdrReadiness::Unknown {
            reason: "display HDR state not available from OS".to_owned(),
        }
    } else if monitor_hdr_supported {
        MonitorHdrReadiness::Ready { evidence: "monitor reports HDR ready".to_owned() }
    } else {
        MonitorHdrReadiness::Unsupported {
            evidence: "monitor reports no HDR support or HDR is disabled".to_owned(),
        }
    };
    resolve_hdr_status_with_monitor_evidence(
        viewer_mode,
        output_color_space,
        surface_supports_hdr,
        surface_hdr_mode,
        monitor,
    )
}

/// Monitor-side readiness state with native probe evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonitorHdrReadiness {
    /// The monitor, OS compositor, and active output path are HDR/EDR ready.
    Ready { evidence: String },
    /// The native API explicitly reports that HDR is unsupported or disabled.
    Unsupported { evidence: String },
    /// Hardware support or current compositor state cannot be confirmed.
    Unknown { reason: String },
}

/// Resolve HDR status while preserving platform-native capability evidence.
pub fn resolve_hdr_status_with_monitor_evidence(
    viewer_mode: ViewerDisplayMode,
    output_color_space: ColorSpace,
    surface_supports_hdr: bool,
    surface_hdr_mode: &str,
    monitor: MonitorHdrReadiness,
) -> HdrStatus {
    let resolved = viewer_mode.resolve(output_color_space);
    if !resolved.is_hdr() {
        return HdrStatus::NotRequested;
    }

    let mode = match resolved {
        crate::color_models::ResolvedViewerDisplayMode::HdrPq => "HdrPq",
        crate::color_models::ResolvedViewerDisplayMode::HdrHlg => "HdrHlg",
        crate::color_models::ResolvedViewerDisplayMode::Sdr => {
            return HdrStatus::Unknown {
                reason: "unexpected SDR in HDR resolution path".to_owned(),
            };
        }
    };

    if !surface_supports_hdr {
        return HdrStatus::RequestedSurfaceUnsupported {
            mode: mode.to_owned(),
            surface_format: "unknown".to_owned(),
            surface_color_space: surface_hdr_mode.to_owned(),
        };
    }

    match monitor {
        MonitorHdrReadiness::Ready { evidence } => HdrStatus::RequestedSupported {
            mode: mode.to_owned(),
            evidence: format!("surface={surface_hdr_mode} {evidence}"),
        },
        MonitorHdrReadiness::Unsupported { evidence } => {
            HdrStatus::RequestedMonitorUnsupported { mode: mode.to_owned(), evidence }
        }
        MonitorHdrReadiness::Unknown { reason } => {
            HdrStatus::RequestedMonitorUnknown { mode: mode.to_owned(), reason }
        }
    }
}

/// Compute display output blockers from the resolved statuses.
///
/// This translates `MonitorProfileStatus` and `HdrStatus` into structured
/// `DisplayOutputBlocker` variants for the health report.
pub fn compute_display_blockers(
    monitor_profile: &MonitorProfileStatus,
    hdr_status: &HdrStatus,
    surface_format: &str,
    output_color_space: &str,
) -> Vec<DisplayOutputBlocker> {
    let mut blockers = Vec::new();

    // Surface contract check: HDR output on SDR-only surface
    if matches!(hdr_status, HdrStatus::RequestedSurfaceUnsupported { .. }) {
        // Already captured by HDR status below, but also emit as surface contract mismatch
        // if the output color space is HDR but the surface format is SDR-only.
        let is_hdr_output = output_color_space.contains("Rec2100")
            || output_color_space.contains("Pq")
            || output_color_space.contains("Hlg");
        let is_sdr_surface =
            !surface_format.contains("Rgba16Float") && !surface_format.contains("Rgb10a2Unorm");
        if is_hdr_output && is_sdr_surface {
            blockers.push(DisplayOutputBlocker::SurfaceContractMismatch {
                surface_format: surface_format.to_owned(),
                output_color_space: output_color_space.to_owned(),
            });
        }
    }

    // Surface contract check: wide-gamut output on sRGB-only surface
    let is_wide_gamut_output = output_color_space.contains("DisplayP3")
        || output_color_space.contains("Rec2020")
        || output_color_space.contains("Rec2100");
    let is_srgb_only_surface =
        surface_format.contains("UnormSrgb") && !surface_format.contains("Rgba16Float");
    if is_wide_gamut_output
        && is_srgb_only_surface
        && !matches!(hdr_status, HdrStatus::RequestedSurfaceUnsupported { .. })
    {
        // The surface is sRGB-only but the output needs wide gamut.
        // This is a contract mismatch unless the HDR status already covers it.
        blockers.push(DisplayOutputBlocker::UnsupportedDisplayColorSpace {
            display_color_space: output_color_space.to_owned(),
        });
    }

    // ICC fail-closed blockers
    match monitor_profile {
        MonitorProfileStatus::IccProfileUnsupported { feature_code, profile_path, reason } => {
            blockers.push(DisplayOutputBlocker::MonitorIccProfileUnsupported {
                feature_code: feature_code.clone(),
                profile_path: profile_path.clone(),
                reason: reason.clone(),
            });
        }
        MonitorProfileStatus::IccProfileReadError { profile_path, reason } => {
            blockers.push(DisplayOutputBlocker::MonitorIccProfileInvalid {
                profile_path: profile_path.clone(),
                reason: reason.clone(),
            });
        }
        MonitorProfileStatus::IccProfileUnmapped { profile_path, parsed_color_space, reason } => {
            blockers.push(DisplayOutputBlocker::MonitorIccProfileUnmapped {
                profile_path: profile_path.clone(),
                parsed_color_space: *parsed_color_space,
                reason: reason.clone(),
            });
        }
        MonitorProfileStatus::Unknown { reason } => {
            blockers.push(DisplayOutputBlocker::MonitorIccProfileUnsupported {
                feature_code: "os_icc_profile".to_owned(),
                profile_path: None,
                reason: reason.clone(),
            });
        }
        MonitorProfileStatus::NotRequested
        | MonitorProfileStatus::ManagedColorSpace { .. }
        | MonitorProfileStatus::ManagedIccCalibration { .. } => {}
    }

    // HDR fail-closed blockers
    match hdr_status {
        HdrStatus::RequestedSurfaceUnsupported { mode, .. } => {
            blockers.push(DisplayOutputBlocker::UnsupportedHdrSwapchainOrEdr {
                hdr_mode: mode.clone(),
            });
        }
        HdrStatus::RequestedMonitorUnknown { reason, .. } => {
            blockers
                .push(DisplayOutputBlocker::MonitorHdrCapabilityUnknown { reason: reason.clone() });
        }
        HdrStatus::RequestedMonitorUnsupported { mode, evidence } => {
            blockers.push(DisplayOutputBlocker::MonitorHdrCapabilityUnsupported {
                hdr_mode: mode.clone(),
                evidence: evidence.clone(),
            });
        }
        HdrStatus::RequestedSwapchainMismatch { mode, .. } => {
            blockers.push(DisplayOutputBlocker::UnsupportedHdrSwapchainOrEdr {
                hdr_mode: mode.clone(),
            });
        }
        HdrStatus::Unknown { reason } => {
            blockers
                .push(DisplayOutputBlocker::MonitorHdrCapabilityUnknown { reason: reason.clone() });
        }
        HdrStatus::NotRequested | HdrStatus::RequestedSupported { .. } => {}
    }

    blockers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_sdr_probe_returns_valid_snapshot() {
        let probe = FakeDisplayProbe::sdr_pass();
        let policy = DisplayManagementPolicy::default();
        let snapshot = probe.current_display_snapshot(&policy, ColorSpace::Rec709);
        assert!(snapshot.is_valid());
        assert_eq!(snapshot.validation_status, DisplayValidationStatus::Pass);
        assert!(snapshot.blockers.is_empty());
    }

    #[test]
    fn fake_icc_unsupported_probe_returns_blocker() {
        let probe = FakeDisplayProbe::icc_profile_unsupported();
        let policy = DisplayManagementPolicy::default();
        let snapshot = probe.current_display_snapshot(&policy, ColorSpace::Rec709);
        assert!(!snapshot.is_valid());
        assert_eq!(snapshot.validation_status, DisplayValidationStatus::Fail);
        assert!(snapshot.blockers.iter().any(|b| b.code() == "monitor_icc_profile_unsupported"));
    }

    #[test]
    fn fake_icc_invalid_probe_returns_blocker() {
        let probe = FakeDisplayProbe::icc_profile_invalid("/path/to/bad.icc");
        let snapshot =
            probe.current_display_snapshot(&DisplayManagementPolicy::default(), ColorSpace::Rec709);
        assert!(!snapshot.is_valid());
        assert!(snapshot.blockers.iter().any(|b| b.code() == "monitor_icc_profile_invalid"));
    }

    #[test]
    fn fake_icc_unmapped_probe_fails_closed() {
        let probe = FakeDisplayProbe::icc_profile_unmapped("/path/to/display.icc");
        let snapshot =
            probe.current_display_snapshot(&DisplayManagementPolicy::default(), ColorSpace::Rec709);
        assert!(!snapshot.is_valid());
        assert_eq!(snapshot.validation_status, DisplayValidationStatus::Fail);
        assert!(snapshot.blockers.iter().any(|b| b.code() == "monitor_icc_profile_unmapped"));
    }

    #[test]
    fn fake_hdr_monitor_unknown_returns_blocker() {
        let probe = FakeDisplayProbe::hdr_monitor_unknown();
        let snapshot =
            probe.current_display_snapshot(&DisplayManagementPolicy::default(), ColorSpace::Rec709);
        assert!(!snapshot.is_valid());
        assert!(snapshot.blockers.iter().any(|b| b.code() == "monitor_hdr_capability_unknown"));
    }

    #[test]
    fn fake_hdr_surface_unsupported_returns_blocker() {
        let probe = FakeDisplayProbe::hdr_surface_unsupported();
        let snapshot =
            probe.current_display_snapshot(&DisplayManagementPolicy::default(), ColorSpace::Rec709);
        assert!(!snapshot.is_valid());
        assert!(snapshot.blockers.iter().any(|b| b.code() == "unsupported_hdr_swapchain_or_edr"));
    }

    #[test]
    fn resolve_monitor_profile_not_requested() {
        let policy = DisplayManagementPolicy::default();
        let status = resolve_monitor_profile_status(&policy, false);
        assert_eq!(status, MonitorProfileStatus::NotRequested);
    }

    #[test]
    fn resolve_monitor_profile_icc_unsupported_on_platform() {
        let policy = DisplayManagementPolicy::default()
            .with_calibration(DisplayCalibrationPolicy::IccProfilePath(
                std::env::temp_dir().join("test.icc").display().to_string(),
            ))
            .expect("absolute ICC path");
        let status = resolve_monitor_profile_status(&policy, false);
        match &status {
            MonitorProfileStatus::IccProfileUnsupported { feature_code, .. } => {
                assert_eq!(feature_code, "os_icc_profile");
            }
            other => panic!("expected IccProfileUnsupported, got {other:?}"),
        }
    }

    #[test]
    fn resolve_monitor_profile_color_space() {
        let policy = DisplayManagementPolicy::default()
            .with_monitor_output(MonitorOutputIntent::ColorSpace(ColorSpace::DisplayP3))
            .expect("Display P3 monitor target");
        let status = resolve_monitor_profile_status(&policy, false);
        assert_eq!(
            status,
            MonitorProfileStatus::ManagedColorSpace {
                color_space: ColorSpace::DisplayP3,
                source: MonitorProfileSource::UserConfigured,
            }
        );
    }

    #[test]
    fn resolve_hdr_not_requested() {
        let status = resolve_hdr_status(
            ViewerDisplayMode::Sdr,
            ColorSpace::Rec709,
            true,
            "Srgb",
            true,
            true,
        );
        assert_eq!(status, HdrStatus::NotRequested);
    }

    #[test]
    fn resolve_hdr_supported() {
        let status = resolve_hdr_status(
            ViewerDisplayMode::HdrPq,
            ColorSpace::Rec2100Pq,
            true,
            "Bt2100Pq",
            true,
            true,
        );
        assert!(matches!(status, HdrStatus::RequestedSupported { .. }));
    }

    #[test]
    fn resolve_hdr_keeps_native_monitor_evidence() {
        let status = resolve_hdr_status_with_monitor_evidence(
            ViewerDisplayMode::HdrPq,
            ColorSpace::Rec2100Pq,
            true,
            "Bt2100Pq",
            MonitorHdrReadiness::Ready {
                evidence: "backend=macos-app-kit current_headroom_ppm=1600000".to_owned(),
            },
        );

        assert!(matches!(
            status,
            HdrStatus::RequestedSupported { ref evidence, .. }
                if evidence.contains("macos-app-kit")
                    && evidence.contains("current_headroom_ppm=1600000")
        ));
    }

    #[test]
    fn resolve_hdr_surface_unsupported() {
        let status = resolve_hdr_status(
            ViewerDisplayMode::HdrPq,
            ColorSpace::Rec2100Pq,
            false,
            "Srgb",
            true,
            true,
        );
        assert!(matches!(
            status,
            HdrStatus::RequestedSurfaceUnsupported { .. }
        ));
    }

    #[test]
    fn resolve_hdr_monitor_unknown() {
        let status = resolve_hdr_status(
            ViewerDisplayMode::HdrPq,
            ColorSpace::Rec2100Pq,
            true,
            "Bt2100Pq",
            false,
            false,
        );
        assert!(matches!(status, HdrStatus::RequestedMonitorUnknown { .. }));
    }

    #[test]
    fn resolve_hdr_monitor_unsupported() {
        let status = resolve_hdr_status(
            ViewerDisplayMode::HdrPq,
            ColorSpace::Rec2100Pq,
            true,
            "Bt2100Pq",
            true,
            false,
        );
        assert!(matches!(
            status,
            HdrStatus::RequestedMonitorUnsupported { .. }
        ));
    }

    #[test]
    fn compute_blockers_icc_unsupported() {
        let profile = MonitorProfileStatus::IccProfileUnsupported {
            feature_code: "os_icc_profile".to_owned(),
            profile_path: None,
            reason: "not implemented".to_owned(),
        };
        let hdr = HdrStatus::NotRequested;
        let blockers = compute_display_blockers(&profile, &hdr, "Bgra8UnormSrgb", "Rec709");
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].code(), "monitor_icc_profile_unsupported");
    }

    #[test]
    fn compute_blockers_icc_invalid() {
        let profile = MonitorProfileStatus::IccProfileReadError {
            profile_path: Some("bad.icc".to_owned()),
            reason: "parse error".to_owned(),
        };
        let hdr = HdrStatus::NotRequested;
        let blockers = compute_display_blockers(&profile, &hdr, "Bgra8UnormSrgb", "Rec709");
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].code(), "monitor_icc_profile_invalid");
    }

    #[test]
    fn compute_blockers_icc_unmapped_fails_closed() {
        let profile = MonitorProfileStatus::IccProfileUnmapped {
            profile_path: Some("display.icc".to_owned()),
            parsed_color_space: Some(ColorSpace::DisplayP3),
            reason: "no OCIO display/view match".to_owned(),
        };
        let hdr = HdrStatus::NotRequested;
        let blockers = compute_display_blockers(&profile, &hdr, "Bgra8UnormSrgb", "DisplayP3");
        assert_eq!(blockers.len(), 2);
        assert!(blockers.iter().any(|b| b.code() == "monitor_icc_profile_unmapped"));
        assert!(blockers.iter().any(|b| b.code() == "unsupported_display_color_space"));
    }

    #[test]
    fn compute_blockers_hdr_surface_unsupported() {
        let profile = MonitorProfileStatus::NotRequested;
        let hdr = HdrStatus::RequestedSurfaceUnsupported {
            mode: "HdrPq".to_owned(),
            surface_format: "Bgra8UnormSrgb".to_owned(),
            surface_color_space: "Srgb".to_owned(),
        };
        let blockers = compute_display_blockers(&profile, &hdr, "Bgra8UnormSrgb", "Rec709");
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].code(), "unsupported_hdr_swapchain_or_edr");
    }

    #[test]
    fn compute_blockers_hdr_monitor_unknown() {
        let profile = MonitorProfileStatus::NotRequested;
        let hdr = HdrStatus::RequestedMonitorUnknown {
            mode: "HdrPq".to_owned(),
            reason: "unknown".to_owned(),
        };
        let blockers = compute_display_blockers(&profile, &hdr, "Rgba16Float", "Rec2100Pq");
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].code(), "monitor_hdr_capability_unknown");
    }

    #[test]
    fn compute_blockers_sdr_no_blockers() {
        let profile = MonitorProfileStatus::NotRequested;
        let hdr = HdrStatus::NotRequested;
        let blockers = compute_display_blockers(&profile, &hdr, "Bgra8UnormSrgb", "Rec709");
        assert!(blockers.is_empty());
    }

    #[test]
    fn compute_blockers_combined_icc_and_hdr() {
        let profile = MonitorProfileStatus::IccProfileUnsupported {
            feature_code: "os_icc_profile".to_owned(),
            profile_path: None,
            reason: "not implemented".to_owned(),
        };
        let hdr = HdrStatus::RequestedMonitorUnknown {
            mode: "HdrPq".to_owned(),
            reason: "unknown".to_owned(),
        };
        let blockers = compute_display_blockers(&profile, &hdr, "Rgba16Float", "Rec2100Pq");
        assert_eq!(blockers.len(), 2);
        let codes: Vec<&str> = blockers.iter().map(|b| b.code()).collect();
        assert!(codes.contains(&"monitor_icc_profile_unsupported"));
        assert!(codes.contains(&"monitor_hdr_capability_unknown"));
    }

    #[test]
    fn monitor_switch_changes_display_id() {
        let probe_a = FakeDisplayProbe::sdr_pass();
        let probe_b = FakeDisplayProbe::monitor_switched("Monitor B", (1920, 0));
        let policy = DisplayManagementPolicy::default();
        let snap_a = probe_a.current_display_snapshot(&policy, ColorSpace::Rec709);
        let snap_b = probe_b.current_display_snapshot(&policy, ColorSpace::Rec709);
        assert_ne!(snap_a.display_id, snap_b.display_id);
        assert_ne!(snap_a.contract_identity(), snap_b.contract_identity());
    }

    #[test]
    fn compute_blockers_surface_contract_mismatch_hdr_on_sdr() {
        let profile = MonitorProfileStatus::NotRequested;
        let hdr = HdrStatus::RequestedSurfaceUnsupported {
            mode: "HdrPq".to_owned(),
            surface_format: "Bgra8UnormSrgb".to_owned(),
            surface_color_space: "Srgb".to_owned(),
        };
        let blockers = compute_display_blockers(&profile, &hdr, "Bgra8UnormSrgb", "Rec2100Pq");
        let codes: Vec<&str> = blockers.iter().map(|b| b.code()).collect();
        assert!(
            codes.contains(&"surface_contract_mismatch"),
            "should have surface_contract_mismatch for HDR output on SDR surface, got: {codes:?}"
        );
        assert!(
            codes.contains(&"unsupported_hdr_swapchain_or_edr"),
            "should also have unsupported_hdr_swapchain_or_edr, got: {codes:?}"
        );
    }

    #[test]
    fn compute_blockers_wide_gamut_on_srgb_surface() {
        let profile = MonitorProfileStatus::NotRequested;
        let hdr = HdrStatus::NotRequested;
        let blockers = compute_display_blockers(&profile, &hdr, "Bgra8UnormSrgb", "DisplayP3");
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].code(), "unsupported_display_color_space");
    }

    #[test]
    fn compute_blockers_wide_gamut_on_rgba16float_no_mismatch() {
        let profile = MonitorProfileStatus::NotRequested;
        let hdr = HdrStatus::NotRequested;
        let blockers = compute_display_blockers(&profile, &hdr, "Rgba16Float", "DisplayP3");
        assert!(blockers.is_empty());
    }

    #[test]
    fn compute_blockers_sdr_output_on_srgb_no_blockers() {
        let profile = MonitorProfileStatus::NotRequested;
        let hdr = HdrStatus::NotRequested;
        let blockers = compute_display_blockers(&profile, &hdr, "Bgra8UnormSrgb", "Rec709");
        assert!(blockers.is_empty());
    }
}
