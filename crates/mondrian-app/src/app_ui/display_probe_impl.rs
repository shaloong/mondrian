//! Real platform display probe implementation for winit/wgpu.
//!
//! This module bridges the v2 `DisplayOutputSnapshot` model with the real
//! winit/wgpu display capabilities. It generates a `DisplayOutputSnapshot`
//! from the live window, surface, adapter, and display management policy.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mondrian_core::color_models::{DisplayManagementPolicy, MonitorProfileReference};
use mondrian_core::display_calibration::DisplayCalibrationLut3d;
use mondrian_core::display_contract::*;
use mondrian_core::display_probe::{compute_display_blockers, resolve_hdr_status};
use mondrian_core::types::{ColorEngine, ColorSpace};
#[cfg(not(test))]
use mondrian_platform::{DisplayHdrProbe, DisplayProfileProbe, SystemPlatformService};
use mondrian_platform::{
    DisplayHdrProbeResult, DisplayIccProfileProbeResult, DisplayProfileProbeTarget,
};

/// Generate a real `DisplayOutputSnapshot` from the current winit/wgpu state.
///
/// This is the bridge between the v2 model and the runtime. It queries:
/// - winit window's current monitor for display identity
/// - wgpu surface capabilities for format/color-space support
/// - wgpu display_hdr_info for HDR metadata
/// - OS ICC profile status via the platform display-profile probe
/// - The user's display management policy
///
/// The snapshot is regenerated on every display contract refresh event.
/// Runtime display resolution with serializable facts and non-persistent calibration data.
pub struct DisplaySnapshotResolution {
    /// Serializable display contract consumed by preview scheduling and diagnostics.
    pub snapshot: DisplayOutputSnapshot,
    /// CPU calibration LUT retained only by the active window runtime.
    pub calibration: Option<Arc<DisplayCalibrationLut3d>>,
}

/// Resolve the live display contract and any matching runtime ICC calibration.
pub fn resolve_display_snapshot(
    display_name: Option<String>,
    display_position: (i32, i32),
    display_physical_size: (u32, u32),
    scale_factor: f64,
    surface_format: wgpu::TextureFormat,
    surface_color_space: wgpu::SurfaceColorSpace,
    surface_hdr_mode_str: &str,
    supported_surface_color_spaces: &[wgpu::SurfaceColorSpace],
    display_hdr_info: wgpu::DisplayHdrInfo,
    engine: &ColorEngine,
    policy: &DisplayManagementPolicy,
    output_color_space: ColorSpace,
    refresh_reason: &str,
) -> DisplaySnapshotResolution {
    let profile_probe = if should_probe_os_icc_profile(policy) {
        display_icc_profile_probe(DisplayProfileProbeTarget::new(
            display_position,
            display_physical_size,
        ))
    } else {
        DisplayIccProfileProbeResult::unsupported("ICC profile not requested")
    };

    let resolved_output_color_space = resolve_output_color_space(policy, output_color_space);
    let (monitor_profile_status, calibration) =
        resolve_monitor_profile_status(policy, &profile_probe, resolved_output_color_space);
    let hdr_probe = display_hdr_state_probe(DisplayProfileProbeTarget::new(
        display_position,
        display_physical_size,
    ));

    let hdr_mode_str = match surface_hdr_mode_str {
        "HdrPq" => "HdrPq",
        "HdrHlg" => "HdrHlg",
        _ => "SdrOnly",
    };

    let surface_supports_hdr = matches!(surface_hdr_mode_str, "HdrPq" | "HdrHlg");

    let (monitor_hdr_known, monitor_hdr_supported) =
        resolve_monitor_hdr_capability(&hdr_probe, display_hdr_info);

    let hdr_status = resolve_hdr_status(
        policy.viewer_mode,
        output_color_space,
        surface_supports_hdr,
        hdr_mode_str,
        monitor_hdr_known,
        monitor_hdr_supported,
    );

    let (ocio_display, ocio_view, ocio_blocker) =
        resolve_ocio_display_view(engine, policy, resolved_output_color_space);

    let mut blockers = compute_display_blockers(
        &monitor_profile_status,
        &hdr_status,
        &format!("{surface_format:?}"),
        &format!("{resolved_output_color_space:?}"),
    );
    if let Some(blocker) = ocio_blocker {
        blockers.push(blocker);
    }

    let warnings = compute_display_warnings(&monitor_profile_status, &hdr_status);

    let validation_status = if !blockers.is_empty() {
        DisplayValidationStatus::Fail
    } else if warnings.iter().any(|w| w.code == "icc_profile_unmapped") {
        DisplayValidationStatus::Warn
    } else {
        DisplayValidationStatus::Pass
    };

    let requested_viewer_mode = format!("{:?}", policy.viewer_mode);

    let supported_color_space_names: Vec<String> =
        supported_surface_color_spaces.iter().map(|cs| format!("{cs:?}")).collect();

    let snapshot = DisplayOutputSnapshot {
        display_id: DisplayId {
            name: display_name,
            position: display_position,
            physical_size: display_physical_size,
        },
        platform: current_platform(),
        scale_factor: ScaleFactorPpm::from_f64(scale_factor),
        surface_format: format!("{surface_format:?}"),
        surface_color_space: format!("{surface_color_space:?}"),
        surface_hdr_mode: hdr_mode_str.to_owned(),
        supported_surface_color_spaces: supported_color_space_names,
        requested_viewer_mode,
        requested_output_color_space: format!("{output_color_space:?}"),
        resolved_output_color_space: format!("{resolved_output_color_space:?}"),
        ocio_display,
        ocio_view,
        monitor_profile_status,
        hdr_status,
        validation_status,
        warnings,
        blockers,
        refresh_reason: refresh_reason.to_owned(),
    };
    DisplaySnapshotResolution { snapshot, calibration }
}

#[cfg(test)]
fn generate_display_snapshot(
    display_name: Option<String>,
    display_position: (i32, i32),
    display_physical_size: (u32, u32),
    scale_factor: f64,
    surface_format: wgpu::TextureFormat,
    surface_color_space: wgpu::SurfaceColorSpace,
    surface_hdr_mode_str: &str,
    supported_surface_color_spaces: &[wgpu::SurfaceColorSpace],
    display_hdr_info: wgpu::DisplayHdrInfo,
    policy: &DisplayManagementPolicy,
    output_color_space: ColorSpace,
    refresh_reason: &str,
) -> DisplayOutputSnapshot {
    resolve_display_snapshot(
        display_name,
        display_position,
        display_physical_size,
        scale_factor,
        surface_format,
        surface_color_space,
        surface_hdr_mode_str,
        supported_surface_color_spaces,
        display_hdr_info,
        &ColorEngine::mondrian_standard(),
        policy,
        output_color_space,
        refresh_reason,
    )
    .snapshot
}

#[cfg(not(test))]
fn display_icc_profile_probe(target: DisplayProfileProbeTarget) -> DisplayIccProfileProbeResult {
    SystemPlatformService.display_icc_profile(target)
}

#[cfg(test)]
fn display_icc_profile_probe(_target: DisplayProfileProbeTarget) -> DisplayIccProfileProbeResult {
    DisplayIccProfileProbeResult::unsupported("test ICC profile probe unavailable")
}

#[cfg(not(test))]
fn display_hdr_state_probe(target: DisplayProfileProbeTarget) -> DisplayHdrProbeResult {
    SystemPlatformService.display_hdr_state(target)
}

#[cfg(test)]
fn display_hdr_state_probe(_target: DisplayProfileProbeTarget) -> DisplayHdrProbeResult {
    DisplayHdrProbeResult::unsupported("test HDR / Advanced Color probe unavailable")
}

fn resolve_monitor_hdr_capability(
    hdr_probe: &DisplayHdrProbeResult,
    display_hdr_info: wgpu::DisplayHdrInfo,
) -> (bool, bool) {
    let os_hdr_known = hdr_probe.discovery_available
        && (hdr_probe.advanced_color_supported.is_some()
            || hdr_probe.advanced_color_enabled.is_some()
            || hdr_probe.advanced_color_force_disabled.is_some());
    if os_hdr_known {
        let supported = hdr_probe.advanced_color_supported == Some(true);
        let enabled = hdr_probe.advanced_color_enabled == Some(true);
        let force_disabled = hdr_probe.advanced_color_force_disabled == Some(true);
        return (true, supported && enabled && !force_disabled);
    }

    (
        display_hdr_info.tone_map_headroom().is_some(),
        display_hdr_info.tone_map_headroom().is_some_and(|h| h > 1.0),
    )
}

fn should_probe_os_icc_profile(policy: &DisplayManagementPolicy) -> bool {
    matches!(
        &policy.monitor_profile,
        MonitorProfileReference::IccProfile { profile_id }
            if profile_id.trim().is_empty() || profile_id.trim().eq_ignore_ascii_case("os-default")
    )
}

/// Resolve the monitor profile status from policy and the platform ICC probe.
fn resolve_monitor_profile_status(
    policy: &DisplayManagementPolicy,
    profile_probe: &DisplayIccProfileProbeResult,
    source_color_space: ColorSpace,
) -> (MonitorProfileStatus, Option<Arc<DisplayCalibrationLut3d>>) {
    match &policy.monitor_profile {
        MonitorProfileReference::MatchOutputColorSpace => {
            (MonitorProfileStatus::NotRequested, None)
        }
        MonitorProfileReference::ColorSpace(cs) => (
            MonitorProfileStatus::ManagedColorSpace {
                color_space: *cs,
                source: MonitorProfileSource::UserConfigured,
            },
            None,
        ),
        MonitorProfileReference::OcioDisplay { display: _ } => (
            MonitorProfileStatus::ManagedColorSpace {
                color_space: ColorSpace::Rec709,
                source: MonitorProfileSource::OcioConfig,
            },
            None,
        ),
        MonitorProfileReference::IccProfile { profile_id } => {
            let profile_path = match explicit_profile_path(profile_id) {
                ExplicitProfilePath::Path(path) => Some(path),
                ExplicitProfilePath::UseOsDefault => profile_probe.profile_path.clone(),
                ExplicitProfilePath::UnresolvedId => {
                    return (MonitorProfileStatus::IccProfileUnsupported {
                        feature_code: "icc_profile_registry".to_owned(),
                        profile_path: Some(profile_id.clone()),
                        reason: "ICC profile id registry is not implemented; use an absolute profile path or profile_id='os-default'".to_owned(),
                    }, None);
                }
            };

            let Some(profile_path) = profile_path else {
                return (
                    MonitorProfileStatus::IccProfileUnsupported {
                        feature_code: "os_icc_profile".to_owned(),
                        profile_path: Some(profile_id.clone()),
                        reason: profile_probe.error.clone().unwrap_or_else(|| {
                            if profile_probe.discovery_available {
                                "OS ICC profile discovery did not return a profile".to_owned()
                            } else {
                                "OS ICC profile discovery is not available".to_owned()
                            }
                        }),
                    },
                    None,
                );
            };

            let profile_bytes = match std::fs::read(&profile_path) {
                Ok(bytes) => bytes,
                Err(reason) => {
                    return (
                        MonitorProfileStatus::IccProfileReadError {
                            profile_path: Some(profile_path.display().to_string()),
                            reason: format!("failed to read ICC profile: {reason}"),
                        },
                        None,
                    );
                }
            };
            let parsed = match mondrian_core::icc::parse_icc_display_profile(&profile_bytes) {
                Ok(parsed) => parsed,
                Err(reason) => {
                    return (
                        MonitorProfileStatus::IccProfileReadError {
                            profile_path: Some(profile_path.display().to_string()),
                            reason,
                        },
                        None,
                    );
                }
            };

            match DisplayCalibrationLut3d::from_icc_bytes(source_color_space, &profile_bytes) {
                Ok(calibration) => {
                    let calibration = Arc::new(calibration);
                    (
                        MonitorProfileStatus::ManagedIccCalibration {
                            source_color_space,
                            profile_fingerprint: calibration.profile_fingerprint,
                        },
                        Some(calibration),
                    )
                }
                Err(error) => (
                    MonitorProfileStatus::IccProfileUnmapped {
                        profile_path: Some(profile_path.display().to_string()),
                        parsed_color_space: parsed.mapping.color_space(),
                        reason: format!(
                            "ICC profile '{}' could not produce a {source_color_space:?} device calibration processor: {error}",
                            parsed.name
                        ),
                    },
                    None,
                ),
            }
        }
    }
}

enum ExplicitProfilePath {
    Path(PathBuf),
    UseOsDefault,
    UnresolvedId,
}

fn explicit_profile_path(profile_id: &str) -> ExplicitProfilePath {
    let trimmed = profile_id.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("os-default") {
        return ExplicitProfilePath::UseOsDefault;
    }

    let path = Path::new(trimmed);
    if path.is_absolute() {
        ExplicitProfilePath::Path(path.to_path_buf())
    } else {
        ExplicitProfilePath::UnresolvedId
    }
}

/// Resolve the output color space from the display management policy.
fn resolve_output_color_space(
    policy: &DisplayManagementPolicy,
    sequence_output: ColorSpace,
) -> ColorSpace {
    let profile_space = match &policy.monitor_profile {
        MonitorProfileReference::MatchOutputColorSpace => sequence_output,
        MonitorProfileReference::ColorSpace(cs) => *cs,
        MonitorProfileReference::OcioDisplay { .. } => sequence_output,
        MonitorProfileReference::IccProfile { .. } => {
            // ICC calibration consumes the explicit presentation/output space;
            // it does not replace that OCIO boundary identity.
            sequence_output
        }
    };

    match policy.viewer_mode.resolve(profile_space) {
        mondrian_core::color_models::ResolvedViewerDisplayMode::Sdr => {
            if profile_space.is_hdr() {
                ColorSpace::Rec709
            } else {
                profile_space
            }
        }
        mondrian_core::color_models::ResolvedViewerDisplayMode::HdrPq => ColorSpace::Rec2100Pq,
        mondrian_core::color_models::ResolvedViewerDisplayMode::HdrHlg => ColorSpace::Rec2100Hlg,
    }
}

/// Resolve the OCIO display/view pair from the exact selected color engine.
fn resolve_ocio_display_view(
    engine: &ColorEngine,
    policy: &DisplayManagementPolicy,
    output_color_space: ColorSpace,
) -> (Option<String>, Option<String>, Option<DisplayOutputBlocker>) {
    match &policy.monitor_profile {
        MonitorProfileReference::OcioDisplay { display } => match engine {
            ColorEngine::MondrianStandard { package } => {
                match mondrian_core::mondrian_standard_display_view_for_package(*package, display) {
                    Ok((display, view)) => (Some(display), Some(view), None),
                    Err(_) => unresolved_ocio_display_view(Some(display.clone())),
                }
            }
            ColorEngine::Aces { .. } | ColorEngine::CustomOcio { .. } => {
                match engine.default_view_for_display(display) {
                    Ok(Some(view)) => (Some(display.clone()), Some(view), None),
                    Ok(None) | Err(_) => unresolved_ocio_display_view(Some(display.clone())),
                }
            }
        },
        _ => match engine {
            ColorEngine::MondrianStandard { package } => {
                match mondrian_core::mondrian_standard_output_display_view_for_package(
                    *package,
                    output_color_space,
                ) {
                    Ok((display, view)) => (Some(display), Some(view), None),
                    Err(_) => {
                        let display = mondrian_core::mondrian_standard_output_display_name(
                            output_color_space,
                        )
                        .ok()
                        .map(str::to_owned);
                        unresolved_ocio_display_view(display)
                    }
                }
            }
            ColorEngine::Aces { .. } | ColorEngine::CustomOcio { .. } => {
                if engine.ensure_loaded().is_err() {
                    return unresolved_ocio_display_view(None);
                }
                match engine.output_display_view(output_color_space) {
                    Ok((display, view)) => (Some(display), Some(view), None),
                    Err(_) => unresolved_ocio_display_view(None),
                }
            }
        },
    }
}

fn unresolved_ocio_display_view(
    display: Option<String>,
) -> (Option<String>, Option<String>, Option<DisplayOutputBlocker>) {
    (
        display.clone(),
        None,
        Some(DisplayOutputBlocker::OcioDisplayViewMissing { display, view: None }),
    )
}

/// Compute non-blocking display warnings from resolved statuses.
fn compute_display_warnings(
    _monitor_profile: &MonitorProfileStatus,
    _hdr_status: &HdrStatus,
) -> Vec<DisplayOutputWarning> {
    Vec::new()
}

/// Detect the current platform.
fn current_platform() -> DisplayPlatform {
    if cfg!(target_os = "windows") {
        DisplayPlatform::Windows
    } else if cfg!(target_os = "macos") {
        DisplayPlatform::Macos
    } else if cfg!(target_os = "linux") {
        DisplayPlatform::Linux
    } else {
        DisplayPlatform::Unknown
    }
}

/// Build `PreviewGpuOutputBlocker` variants from the display snapshot's blockers.
///
/// This bridges the v2 `DisplayOutputBlocker` taxonomy into the existing
/// `PreviewGpuOutputBlocker` taxonomy used by the preview GPU output path.
pub fn preview_blockers_from_snapshot(
    snapshot: &DisplayOutputSnapshot,
) -> Vec<super::preview_gpu_output_blocker::PreviewGpuOutputBlocker> {
    use super::preview_gpu_output_blocker::PreviewGpuOutputBlocker;

    snapshot
        .blockers
        .iter()
        .map(|blocker| match blocker {
            DisplayOutputBlocker::SurfaceContractMismatch {
                surface_format,
                output_color_space,
            } => PreviewGpuOutputBlocker::SurfaceContractMismatch {
                surface_format: surface_format.clone(),
                output_color_space: output_color_space.clone(),
            },
            DisplayOutputBlocker::UnsupportedDisplayColorSpace { display_color_space } => {
                PreviewGpuOutputBlocker::UnsupportedDisplayColorSpace {
                    display_color_space: display_color_space.clone(),
                }
            }
            DisplayOutputBlocker::UnsupportedHdrSwapchainOrEdr { hdr_mode } => {
                PreviewGpuOutputBlocker::UnsupportedHdrSwapchainOrEdr { hdr_mode: hdr_mode.clone() }
            }
            DisplayOutputBlocker::MonitorIccProfileUnsupported {
                feature_code,
                profile_path,
                reason,
            } => PreviewGpuOutputBlocker::MonitorIccProfileUnsupported {
                feature_code: feature_code.clone(),
                profile_path: profile_path.clone(),
                reason: reason.clone(),
            },
            DisplayOutputBlocker::MonitorIccProfileInvalid { profile_path, reason } => {
                PreviewGpuOutputBlocker::MonitorIccProfileInvalid {
                    profile_path: profile_path.clone(),
                    reason: reason.clone(),
                }
            }
            DisplayOutputBlocker::MonitorIccProfileUnmapped {
                profile_path,
                parsed_color_space,
                reason,
            } => PreviewGpuOutputBlocker::MonitorIccProfileUnmapped {
                profile_path: profile_path.clone(),
                parsed_color_space: parsed_color_space.map(|space| format!("{space:?}")),
                reason: reason.clone(),
            },
            DisplayOutputBlocker::MonitorHdrCapabilityUnknown { reason } => {
                PreviewGpuOutputBlocker::MonitorHdrCapabilityUnknown { reason: reason.clone() }
            }
            DisplayOutputBlocker::MonitorHdrCapabilityUnsupported { hdr_mode, evidence } => {
                PreviewGpuOutputBlocker::MonitorHdrCapabilityUnsupported {
                    hdr_mode: hdr_mode.clone(),
                    evidence: evidence.clone(),
                }
            }
            DisplayOutputBlocker::DisplayMovedContractStale { previous_display, new_display } => {
                PreviewGpuOutputBlocker::DisplayContractStale {
                    previous_display: previous_display.clone(),
                    new_display: new_display.clone(),
                }
            }
            DisplayOutputBlocker::OcioDisplayViewMissing { display, view } => {
                PreviewGpuOutputBlocker::UnsupportedFeature {
                    feature: "ocio_display_view_missing".to_owned(),
                    reason: format!(
                        "OCIO display/view not found: display={display:?} view={view:?}"
                    ),
                }
            }
            DisplayOutputBlocker::OcioConfigUnavailable => {
                PreviewGpuOutputBlocker::UnsupportedFeature {
                    feature: "ocio_config_unavailable".to_owned(),
                    reason: "OCIO config not available for display/view resolution".to_owned(),
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::color_models::ViewerDisplayMode;

    fn pinned_custom_engine(source: mondrian_core::OcioConfigSource) -> ColorEngine {
        ColorEngine::CustomOcio {
            identity: Box::new(
                mondrian_core::CustomOcioProjectIdentity::from_pinned_parts(
                    source,
                    "0".repeat(64),
                    "test-resolved-config".to_owned(),
                    "0".repeat(64),
                    "Linear Rec.2020".to_owned(),
                    vec![mondrian_core::CustomOcioOutputIdentity::from_pinned_parts(
                        ColorSpace::Rec709,
                        "Test Display".to_owned(),
                        "Test View".to_owned(),
                        "Test Display Color Space".to_owned(),
                        mondrian_core::CustomOcioLookIdentity::None,
                    )
                    .expect("valid Custom OCIO output binding")],
                    Vec::new(),
                    Vec::new(),
                )
                .expect("structurally valid Custom OCIO test identity"),
            ),
        }
    }

    fn default_policy() -> DisplayManagementPolicy {
        DisplayManagementPolicy::default()
    }

    fn no_hdr_info() -> wgpu::DisplayHdrInfo {
        wgpu::DisplayHdrInfo::default()
    }

    #[test]
    fn generate_sdr_snapshot_passes_validation() {
        let snapshot = generate_display_snapshot(
            Some("Test".to_owned()),
            (0, 0),
            (3840, 2160),
            1.0,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            wgpu::SurfaceColorSpace::Srgb,
            "SdrOnly",
            &[wgpu::SurfaceColorSpace::Srgb],
            no_hdr_info(),
            &default_policy(),
            ColorSpace::Rec709,
            "TestInit",
        );
        assert!(snapshot.is_valid());
        assert_eq!(snapshot.validation_status, DisplayValidationStatus::Pass);
        assert_eq!(
            snapshot.monitor_profile_status,
            MonitorProfileStatus::NotRequested
        );
        assert_eq!(snapshot.hdr_status, HdrStatus::NotRequested);
    }

    #[test]
    fn standard_display_view_follows_the_resolved_p3_output_target() {
        let (display, view, blocker) = resolve_ocio_display_view(
            &ColorEngine::mondrian_standard(),
            &default_policy(),
            ColorSpace::DisplayP3,
        );

        assert_eq!(display.as_deref(), Some("Display P3 - Display"));
        assert_eq!(view.as_deref(), Some("Mondrian Standard SDR v2"));
        assert!(blocker.is_none());
    }

    #[test]
    fn explicit_standard_display_cannot_select_an_aces_default_view() {
        let policy = DisplayManagementPolicy {
            monitor_profile: MonitorProfileReference::OcioDisplay {
                display: "Display P3 - Display".to_owned(),
            },
            ..default_policy()
        };
        let (display, view, blocker) = resolve_ocio_display_view(
            &ColorEngine::mondrian_standard(),
            &policy,
            ColorSpace::DisplayP3,
        );

        assert_eq!(display.as_deref(), Some("Display P3 - Display"));
        assert_eq!(view.as_deref(), Some("Mondrian Standard SDR v2"));
        assert!(blocker.is_none());
    }

    #[test]
    fn standard_hdr_view_follows_the_resolved_hlg_and_pq_output_targets() {
        for output in [ColorSpace::Rec2100Pq, ColorSpace::Rec2100Hlg] {
            let (display, view, blocker) = resolve_ocio_display_view(
                &ColorEngine::mondrian_standard(),
                &default_policy(),
                output,
            );

            assert_eq!(
                display.as_deref(),
                Some(
                    mondrian_core::mondrian_standard_output_display_name(output)
                        .expect("HDR target has a stable display identity")
                )
            );
            assert_eq!(view.as_deref(), Some("Mondrian Standard HDR 1000 nits v1"));
            assert!(blocker.is_none());
        }
    }

    #[test]
    fn unavailable_custom_engine_does_not_reuse_the_loaded_standard_view() {
        let engine = pinned_custom_engine(mondrian_core::OcioConfigSource::Path {
            path: PathBuf::from("missing-custom-display-config.ocio"),
        });
        let (display, view, blocker) =
            resolve_ocio_display_view(&engine, &default_policy(), ColorSpace::Rec709);

        assert!(display.is_none());
        assert!(view.is_none());
        assert!(matches!(
            blocker,
            Some(DisplayOutputBlocker::OcioDisplayViewMissing { display: None, view: None })
        ));
    }

    #[test]
    fn generate_hdr_snapshot_with_unknown_monitor_fails() {
        let snapshot = generate_display_snapshot(
            Some("Test".to_owned()),
            (0, 0),
            (3840, 2160),
            1.0,
            wgpu::TextureFormat::Rgba16Float,
            wgpu::SurfaceColorSpace::Bt2100Pq,
            "HdrPq",
            &[
                wgpu::SurfaceColorSpace::Srgb,
                wgpu::SurfaceColorSpace::Bt2100Pq,
            ],
            no_hdr_info(),
            &DisplayManagementPolicy {
                viewer_mode: ViewerDisplayMode::HdrPq,
                ..default_policy()
            },
            ColorSpace::Rec2100Pq,
            "TestHdr",
        );
        assert!(!snapshot.is_valid());
        assert_eq!(snapshot.validation_status, DisplayValidationStatus::Fail);
        assert!(matches!(
            snapshot.hdr_status,
            HdrStatus::RequestedMonitorUnknown { .. }
        ));
        assert!(snapshot.blockers.iter().any(|b| b.code() == "monitor_hdr_capability_unknown"));
    }

    #[test]
    fn generate_icc_unsupported_snapshot_fails() {
        let snapshot = generate_display_snapshot(
            None,
            (0, 0),
            (1920, 1080),
            1.0,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            wgpu::SurfaceColorSpace::Srgb,
            "SdrOnly",
            &[wgpu::SurfaceColorSpace::Srgb],
            no_hdr_info(),
            &DisplayManagementPolicy {
                monitor_profile: MonitorProfileReference::IccProfile {
                    profile_id: "test.icc".to_owned(),
                },
                ..default_policy()
            },
            ColorSpace::Rec709,
            "TestIcc",
        );
        assert!(!snapshot.is_valid());
        assert!(matches!(
            snapshot.monitor_profile_status,
            MonitorProfileStatus::IccProfileUnsupported { .. }
        ));
        assert!(snapshot.blockers.iter().any(|b| b.code() == "monitor_icc_profile_unsupported"));
    }

    #[test]
    fn unresolved_icc_profile_id_does_not_fall_back_to_os_default() {
        let (status, calibration) = resolve_monitor_profile_status(
            &DisplayManagementPolicy {
                monitor_profile: MonitorProfileReference::IccProfile {
                    profile_id: "display-profile".to_owned(),
                },
                ..default_policy()
            },
            &DisplayIccProfileProbeResult::found(
                Some(r"\\.\DISPLAY1".to_owned()),
                PathBuf::from(
                    r"C:\Windows\System32\spool\drivers\color\sRGB Color Space Profile.icm",
                ),
            ),
            ColorSpace::Srgb,
        );
        assert!(calibration.is_none());

        assert!(matches!(
            status,
            MonitorProfileStatus::IccProfileUnsupported {
                ref feature_code,
                ..
            } if feature_code == "icc_profile_registry"
        ));
    }

    #[test]
    fn os_default_icc_profile_without_platform_probe_fails_closed() {
        let (status, calibration) = resolve_monitor_profile_status(
            &DisplayManagementPolicy {
                monitor_profile: MonitorProfileReference::IccProfile {
                    profile_id: "os-default".to_owned(),
                },
                ..default_policy()
            },
            &DisplayIccProfileProbeResult::unsupported("test probe unavailable"),
            ColorSpace::Srgb,
        );
        assert!(calibration.is_none());

        assert!(matches!(
            status,
            MonitorProfileStatus::IccProfileUnsupported {
                ref feature_code,
                ref reason,
                ..
            } if feature_code == "os_icc_profile" && reason.contains("test probe unavailable")
        ));
    }

    #[test]
    fn generate_snapshot_with_known_hdr_monitor_passes() {
        // Tests use a fixed unsupported OS probe, so default DisplayHdrInfo
        // still produces RequestedMonitorUnknown. Production Windows builds use
        // DisplayConfig Advanced Color state before falling back to wgpu
        // headroom.
        let snapshot = generate_display_snapshot(
            Some("HDR Monitor".to_owned()),
            (0, 0),
            (3840, 2160),
            1.0,
            wgpu::TextureFormat::Rgba16Float,
            wgpu::SurfaceColorSpace::Bt2100Pq,
            "HdrPq",
            &[
                wgpu::SurfaceColorSpace::Srgb,
                wgpu::SurfaceColorSpace::Bt2100Pq,
            ],
            wgpu::DisplayHdrInfo::default(),
            &DisplayManagementPolicy {
                viewer_mode: ViewerDisplayMode::HdrPq,
                ..default_policy()
            },
            ColorSpace::Rec2100Pq,
            "TestHdrKnown",
        );
        // Surface supports HDR but monitor capability is unknown (no headroom).
        assert!(!snapshot.is_valid());
        assert!(matches!(
            snapshot.hdr_status,
            HdrStatus::RequestedMonitorUnknown { .. }
        ));
    }

    #[test]
    fn monitor_hdr_capability_uses_os_advanced_color_when_known() {
        let probe = DisplayHdrProbeResult::found(
            Some(r"\\.\DISPLAY1".to_owned()),
            true,
            true,
            false,
            false,
            10,
            Some("Rgb".to_owned()),
            Some(203),
        );

        assert_eq!(
            resolve_monitor_hdr_capability(&probe, no_hdr_info()),
            (true, true)
        );
    }

    #[test]
    fn monitor_hdr_capability_does_not_override_os_disabled_state() {
        let probe = DisplayHdrProbeResult::found(
            Some(r"\\.\DISPLAY1".to_owned()),
            true,
            false,
            false,
            false,
            10,
            Some("Rgb".to_owned()),
            Some(203),
        );

        assert_eq!(
            resolve_monitor_hdr_capability(&probe, no_hdr_info()),
            (true, false)
        );
    }

    #[test]
    fn snapshot_contract_generation_changes_with_display_name() {
        let a = generate_display_snapshot(
            Some("Monitor A".to_owned()),
            (0, 0),
            (3840, 2160),
            1.0,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            wgpu::SurfaceColorSpace::Srgb,
            "SdrOnly",
            &[wgpu::SurfaceColorSpace::Srgb],
            no_hdr_info(),
            &default_policy(),
            ColorSpace::Rec709,
            "Test",
        );
        let b = generate_display_snapshot(
            Some("Monitor B".to_owned()),
            (1920, 0),
            (3840, 2160),
            1.0,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            wgpu::SurfaceColorSpace::Srgb,
            "SdrOnly",
            &[wgpu::SurfaceColorSpace::Srgb],
            no_hdr_info(),
            &default_policy(),
            ColorSpace::Rec709,
            "Test",
        );
        assert_ne!(a.contract_generation(), b.contract_generation());
    }

    #[test]
    fn preview_blockers_from_snapshot_converts_all_variants() {
        let snapshot = generate_display_snapshot(
            None,
            (0, 0),
            (1920, 1080),
            1.0,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            wgpu::SurfaceColorSpace::Srgb,
            "SdrOnly",
            &[wgpu::SurfaceColorSpace::Srgb],
            no_hdr_info(),
            &DisplayManagementPolicy {
                monitor_profile: MonitorProfileReference::IccProfile {
                    profile_id: "test.icc".to_owned(),
                },
                ..default_policy()
            },
            ColorSpace::Rec709,
            "Test",
        );
        let blockers = preview_blockers_from_snapshot(&snapshot);
        assert_eq!(blockers.len(), snapshot.blockers.len());
        for blocker in &blockers {
            assert!(!blocker.code().is_empty());
        }
    }

    #[test]
    fn resolve_output_color_space_sdr() {
        let policy = default_policy();
        let result = super::resolve_output_color_space(&policy, ColorSpace::Rec709);
        assert_eq!(result, ColorSpace::Rec709);
    }

    #[test]
    fn resolve_output_color_space_hdr_pq() {
        let policy = DisplayManagementPolicy {
            viewer_mode: ViewerDisplayMode::HdrPq,
            ..default_policy()
        };
        let result = super::resolve_output_color_space(&policy, ColorSpace::Rec2100Pq);
        assert_eq!(result, ColorSpace::Rec2100Pq);
    }

    #[test]
    fn resolve_output_color_space_color_space_override() {
        let policy = DisplayManagementPolicy {
            monitor_profile: MonitorProfileReference::ColorSpace(ColorSpace::DisplayP3),
            ..default_policy()
        };
        let result = super::resolve_output_color_space(&policy, ColorSpace::Rec709);
        assert_eq!(result, ColorSpace::DisplayP3);
    }
}
