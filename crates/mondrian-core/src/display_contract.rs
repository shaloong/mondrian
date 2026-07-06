//! Display Output Contract v2 — industrial-grade display management model.
//!
//! This module defines the canonical display output contract that bridges
//! OS monitor/surface capabilities, user policy, OCIO display/view resolution,
//! and the GPU preview/export output boundary. Both preview and export must
//! consume the same contract — only scheduling differs, not color interpretation.
//!
//! ## Design principles
//!
//! 1. **One pipeline, OCIO is the engine.** Media metadata → user override →
//!    Mondrian color interpretation → OCIO color space/display-view resolution →
//!    render graph → preview/export/cache.
//!
//! 2. **Display management must be real, not plausible.** Unknown monitor / ICC /
//!    HDR state is never silently treated as Rec.709 / sRGB / Standard.
//!
//! 3. **ICC must be real or fail-closed.** If the OS ICC profile cannot be read,
//!    we do not fall back to Rec.709. We record an
//!    `IccProfileUnsupported` / `IccProfileUnmapped` status and emit a blocker.
//!
//! 4. **HDR/EDR must be realistic.** wgpu SurfaceColorSpace support does not
//!    equal real HDR display correctness. We distinguish requested mode, surface
//!    capability, monitor capability, swapchain state, and actual output boundary.
//!
//! 5. **Multi-monitor switching must invalidate contracts.** Window move,
//!    monitor change, scale factor change, and surface reconfiguration all
//!    trigger a full contract re-resolve.

use crate::types::ColorSpace;
use serde::{Deserialize, Serialize};

// ── Display identification ───────────────────────────────────────────────────

/// Stable display identifier for contract generation and cache invalidation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DisplayId {
    /// OS-assigned display name (e.g. "DELL U2723QE"), if available.
    pub name: Option<String>,
    /// Display origin in virtual desktop space.
    pub position: (i32, i32),
    /// Physical display size in pixels.
    pub physical_size: (u32, u32),
}

/// Display scale factor stored as parts-per-million for integer comparison.
/// 1.0x = 1_000_000, 2.0x = 2_000_000.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScaleFactorPpm(pub u32);

impl ScaleFactorPpm {
    /// Create from a floating-point scale factor (e.g. 1.0, 2.0).
    /// NaN and infinite values are clamped to the default (1.0x = 1_000_000).
    pub fn from_f64(scale: f64) -> Self {
        if !scale.is_finite() {
            return Self::default();
        }
        Self((scale * 1_000_000.0).round().clamp(0.0, u32::MAX as f64) as u32)
    }

    /// Convert to floating-point.
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / 1_000_000.0
    }
}

impl Default for ScaleFactorPpm {
    fn default() -> Self {
        Self(1_000_000)
    }
}

/// Platform the display is running on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DisplayPlatform {
    Windows,
    Macos,
    Linux,
    Unknown,
}

impl std::fmt::Display for DisplayPlatform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Windows => write!(f, "Windows"),
            Self::Macos => write!(f, "macOS"),
            Self::Linux => write!(f, "Linux"),
            Self::Unknown => write!(f, "Unknown"),
        }
    }
}

// ── Monitor profile status ───────────────────────────────────────────────────

/// Status of the OS monitor ICC profile for the current display.
///
/// If the user requests an ICC profile (`MonitorProfileReference::IccProfile`)
/// and the OS cannot provide one, the contract **must not** silently fall back
/// to Rec.709 / sRGB. Instead it records one of the failure statuses below and
/// emits a `DisplayOutputBlocker::MonitorIccProfileUnsupported` or
/// `MonitorIccProfileInvalid`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MonitorProfileStatus {
    /// No ICC profile was requested by the user.
    NotRequested,
    /// The monitor profile maps to a known managed color space.
    ManagedColorSpace {
        /// The resolved color space.
        color_space: ColorSpace,
        /// How this mapping was obtained.
        source: MonitorProfileSource,
    },
    /// OS ICC profile discovery is not implemented on this platform.
    IccProfileUnsupported {
        /// Stable feature code (e.g. `os_icc_profile`).
        feature_code: String,
        /// Profile path or identifier, if known.
        profile_path: Option<String>,
        /// Human-readable reason.
        reason: String,
    },
    /// An ICC profile was found but could not be read.
    IccProfileReadError {
        /// Profile path or identifier.
        profile_path: Option<String>,
        /// Human-readable error.
        reason: String,
    },
    /// An ICC profile was read but could not be mapped to an OCIO display/view.
    IccProfileUnmapped {
        /// Profile path or identifier.
        profile_path: Option<String>,
        /// Parsed color space, if any.
        parsed_color_space: Option<ColorSpace>,
        /// Human-readable reason.
        reason: String,
    },
    /// Monitor profile status could not be determined.
    Unknown {
        /// Human-readable reason.
        reason: String,
    },
}

impl std::fmt::Display for MonitorProfileStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRequested => write!(f, "Not Requested"),
            Self::ManagedColorSpace { color_space, source } => {
                write!(f, "Managed {color_space:?} via {source:?}")
            }
            Self::IccProfileUnsupported { reason, .. } => {
                write!(f, "ICC Unsupported: {reason}")
            }
            Self::IccProfileReadError { reason, .. } => write!(f, "ICC Read Error: {reason}"),
            Self::IccProfileUnmapped { reason, .. } => write!(f, "ICC Unmapped: {reason}"),
            Self::Unknown { reason } => write!(f, "Unknown: {reason}"),
        }
    }
}

/// How a monitor profile was resolved to a managed color space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MonitorProfileSource {
    /// The user explicitly configured a color space.
    UserConfigured,
    /// Resolved from the OCIO display configuration.
    OcioConfig,
    /// Inferred from the OS ICC profile (currently not implemented on any platform).
    OsIccProfile,
}

// ── HDR status ───────────────────────────────────────────────────────────────

/// Diagnosed HDR/EDR status of the current display.
///
/// The key distinction: wgpu `SurfaceColorSpace` support for Bt2100Pq / Bt2100Hlg
/// does NOT equal real HDR display correctness. This status tracks the full chain:
/// requested mode → surface format support → monitor HDR capability → swapchain state.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HdrStatus {
    /// No HDR was requested by the user.
    NotRequested,
    /// HDR was requested and the surface + monitor support it.
    RequestedSupported {
        /// The requested HDR mode (Pq or Hlg).
        mode: String,
        /// Evidence: surface format, surface color space, monitor HDR info.
        evidence: String,
    },
    /// HDR was requested but the surface format/color space cannot carry it.
    RequestedSurfaceUnsupported {
        /// The requested HDR mode.
        mode: String,
        /// Current surface format.
        surface_format: String,
        /// Current surface color space.
        surface_color_space: String,
    },
    /// HDR was requested but the monitor's HDR capability is unknown.
    RequestedMonitorUnknown {
        /// The requested HDR mode.
        mode: String,
        /// Why the monitor HDR capability is unknown.
        reason: String,
    },
    /// HDR was requested but the monitor explicitly does not support it.
    RequestedMonitorUnsupported {
        /// The requested HDR mode.
        mode: String,
        /// Evidence (e.g. display_hdr_info.max_luminance == 0).
        evidence: String,
    },
    /// HDR was requested but the swapchain configuration is inconsistent.
    RequestedSwapchainMismatch {
        /// The requested HDR mode.
        mode: String,
        /// Current surface format.
        surface_format: String,
        /// Current surface color space.
        surface_color_space: String,
    },
    /// HDR status could not be determined.
    Unknown {
        /// Human-readable reason.
        reason: String,
    },
}

impl std::fmt::Display for HdrStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRequested => write!(f, "Not Requested"),
            Self::RequestedSupported { mode, evidence } => {
                write!(f, "HDR {mode} Supported: {evidence}")
            }
            Self::RequestedSurfaceUnsupported { mode, .. } => {
                write!(f, "HDR {mode} Surface Unsupported")
            }
            Self::RequestedMonitorUnknown { mode, reason } => {
                write!(f, "HDR {mode} Monitor Unknown: {reason}")
            }
            Self::RequestedMonitorUnsupported { mode, evidence } => {
                write!(f, "HDR {mode} Monitor Unsupported: {evidence}")
            }
            Self::RequestedSwapchainMismatch { mode, .. } => {
                write!(f, "HDR {mode} Swapchain Mismatch")
            }
            Self::Unknown { reason } => write!(f, "HDR Unknown: {reason}"),
        }
    }
}

// ── Display output blocker ───────────────────────────────────────────────────

/// Structured reasons the display output contract is invalid.
///
/// These are the industrial-grade blocker categories for display management.
/// Each blocker has a stable machine-readable code, a diagnostic area, and
/// a suggested action for health reports.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DisplayOutputBlocker {
    /// Surface format/color space does not satisfy the output boundary.
    SurfaceContractMismatch {
        /// Current surface format.
        surface_format: String,
        /// Requested output color space.
        output_color_space: String,
    },
    /// Display color space is not supported by the GPU/surface.
    UnsupportedDisplayColorSpace {
        /// Unsupported display color space name.
        display_color_space: String,
    },
    /// HDR swapchain or EDR mode is not supported or mismatched.
    UnsupportedHdrSwapchainOrEdr {
        /// Current HDR mode description.
        hdr_mode: String,
    },
    /// OS ICC profile is unsupported on this platform.
    MonitorIccProfileUnsupported {
        /// Stable feature code.
        feature_code: String,
        /// Profile path, if known.
        profile_path: Option<String>,
        /// Human-readable reason.
        reason: String,
    },
    /// OS ICC profile was found but is invalid or unreadable.
    MonitorIccProfileInvalid {
        /// Profile path or identifier.
        profile_path: Option<String>,
        /// Human-readable error.
        reason: String,
    },
    /// OS ICC profile was parsed but cannot be mapped to an OCIO display/view.
    MonitorIccProfileUnmapped {
        /// Profile path or identifier.
        profile_path: Option<String>,
        /// Parsed color space, if any.
        parsed_color_space: Option<ColorSpace>,
        /// Human-readable reason.
        reason: String,
    },
    /// Monitor HDR capability is unknown — cannot claim HDR correctness.
    MonitorHdrCapabilityUnknown {
        /// Why the capability is unknown.
        reason: String,
    },
    /// Monitor explicitly does not support the requested HDR mode.
    MonitorHdrCapabilityUnsupported {
        /// The requested HDR mode.
        hdr_mode: String,
        /// Evidence.
        evidence: String,
    },
    /// Display contract is stale because the window moved to another monitor.
    DisplayMovedContractStale {
        /// Previous display name.
        previous_display: Option<String>,
        /// New display name.
        new_display: Option<String>,
    },
    /// No matching OCIO display/view pair was found for the requested output.
    OcioDisplayViewMissing {
        /// Requested display, if any.
        display: Option<String>,
        /// Requested view, if any.
        view: Option<String>,
    },
    /// OCIO config is unavailable for display/view resolution.
    OcioConfigUnavailable,
}

impl std::fmt::Display for DisplayOutputBlocker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.description())
    }
}

impl DisplayOutputBlocker {
    /// Machine-readable code for this blocker.
    pub fn code(&self) -> &'static str {
        match self {
            Self::SurfaceContractMismatch { .. } => "surface_contract_mismatch",
            Self::UnsupportedDisplayColorSpace { .. } => "unsupported_display_color_space",
            Self::UnsupportedHdrSwapchainOrEdr { .. } => "unsupported_hdr_swapchain_or_edr",
            Self::MonitorIccProfileUnsupported { .. } => "monitor_icc_profile_unsupported",
            Self::MonitorIccProfileInvalid { .. } => "monitor_icc_profile_invalid",
            Self::MonitorIccProfileUnmapped { .. } => "monitor_icc_profile_unmapped",
            Self::MonitorHdrCapabilityUnknown { .. } => "monitor_hdr_capability_unknown",
            Self::MonitorHdrCapabilityUnsupported { .. } => "monitor_hdr_capability_unsupported",
            Self::DisplayMovedContractStale { .. } => "display_moved_contract_stale",
            Self::OcioDisplayViewMissing { .. } => "ocio_display_view_missing",
            Self::OcioConfigUnavailable => "ocio_config_unavailable",
        }
    }

    /// Human-readable description of this blocker.
    pub fn description(&self) -> String {
        match self {
            Self::SurfaceContractMismatch { surface_format, output_color_space } => {
                format!("Surface {surface_format} cannot carry output {output_color_space}")
            }
            Self::UnsupportedDisplayColorSpace { display_color_space } => {
                format!("Display color space {display_color_space} not supported")
            }
            Self::UnsupportedHdrSwapchainOrEdr { hdr_mode } => {
                format!("HDR swapchain/EDR mode {hdr_mode} not supported")
            }
            Self::MonitorIccProfileUnsupported { reason, .. } => {
                format!("ICC profile unsupported: {reason}")
            }
            Self::MonitorIccProfileInvalid { reason, .. } => {
                format!("ICC profile invalid: {reason}")
            }
            Self::MonitorIccProfileUnmapped { parsed_color_space, reason, .. } => {
                format!("ICC profile unmapped ({parsed_color_space:?}): {reason}")
            }
            Self::MonitorHdrCapabilityUnknown { reason } => {
                format!("Monitor HDR capability unknown: {reason}")
            }
            Self::MonitorHdrCapabilityUnsupported { hdr_mode, evidence } => {
                format!("Monitor HDR unsupported for {hdr_mode}: {evidence}")
            }
            Self::DisplayMovedContractStale { previous_display, new_display } => {
                format!(
                    "Display changed from {} to {}",
                    previous_display.as_deref().unwrap_or("?"),
                    new_display.as_deref().unwrap_or("?"),
                )
            }
            Self::OcioDisplayViewMissing { display, view } => {
                format!("OCIO display/view not found: {display:?} {view:?}")
            }
            Self::OcioConfigUnavailable => "OCIO config unavailable".to_owned(),
        }
    }

    /// Diagnostic area code for health report root-cause attribution.
    pub fn area_code(&self) -> &'static str {
        match self {
            Self::SurfaceContractMismatch { .. }
            | Self::UnsupportedDisplayColorSpace { .. }
            | Self::UnsupportedHdrSwapchainOrEdr { .. } => "DisplayContract",
            Self::MonitorIccProfileUnsupported { .. } | Self::MonitorIccProfileInvalid { .. } => {
                "MonitorProfile"
            }
            Self::MonitorIccProfileUnmapped { .. } => "MonitorProfile",
            Self::MonitorHdrCapabilityUnknown { .. }
            | Self::MonitorHdrCapabilityUnsupported { .. } => "MonitorHdr",
            Self::DisplayMovedContractStale { .. } => "DisplayLifecycle",
            Self::OcioDisplayViewMissing { .. } | Self::OcioConfigUnavailable => "OcioConfig",
        }
    }

    /// Suggested action code for health report follow-up.
    pub fn action_code(&self) -> &'static str {
        match self {
            Self::SurfaceContractMismatch { .. }
            | Self::UnsupportedDisplayColorSpace { .. }
            | Self::UnsupportedHdrSwapchainOrEdr { .. } => "configure_display_contract",
            Self::MonitorIccProfileUnsupported { .. } => "configure_monitor_icc_profile",
            Self::MonitorIccProfileInvalid { .. } => "map_icc_profile_to_ocio_display",
            Self::MonitorIccProfileUnmapped { .. } => "map_icc_profile_to_ocio_display",
            Self::MonitorHdrCapabilityUnknown { .. } => "inspect_monitor_hdr_capability",
            Self::MonitorHdrCapabilityUnsupported { .. } => "configure_display_contract",
            Self::DisplayMovedContractStale { .. } => "move_window_display_contract_refresh",
            Self::OcioDisplayViewMissing { .. } | Self::OcioConfigUnavailable => {
                "prepare_ocio_gpu_resources"
            }
        }
    }
}

// ── Display validation status ────────────────────────────────────────────────

/// Overall validation status of the display output contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DisplayValidationStatus {
    /// All checks passed — no blockers, no unknowns for requested capabilities.
    Pass,
    /// Contract is valid but some capability is unknown and the user has not
    /// requested it. This is a warning, not a failure.
    Warn,
    /// Contract has blockers — the requested display output cannot be satisfied.
    Fail,
}

impl std::fmt::Display for DisplayValidationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pass => write!(f, "Pass"),
            Self::Warn => write!(f, "Warn"),
            Self::Fail => write!(f, "Fail"),
        }
    }
}

// ── Display Output Snapshot ──────────────────────────────────────────────────

/// Point-in-time snapshot of the display output contract.
///
/// This is the canonical type that both preview and export consume to
/// determine the display output boundary. It captures:
/// - OS monitor identity and capabilities
/// - Surface format and color space
/// - User-requested viewer mode
/// - Resolved OCIO display/view pair
/// - Monitor profile status (ICC)
/// - HDR status (full chain diagnosis)
/// - Validation result with blockers and warnings
///
/// The snapshot is regenerated on every display contract refresh event
/// (resize, scale factor change, window move, surface format change,
/// display policy change, OCIO config generation change).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayOutputSnapshot {
    /// Stable display identifier.
    pub display_id: DisplayId,
    /// Platform.
    pub platform: DisplayPlatform,
    /// Scale factor stored as parts-per-million (e.g. 1.0x = 1_000_000).
    pub scale_factor: ScaleFactorPpm,
    /// Surface format chosen for the swapchain.
    pub surface_format: String,
    /// Surface color space chosen for the swapchain.
    pub surface_color_space: String,
    /// Surface HDR mode.
    pub surface_hdr_mode: String,
    /// All surface color spaces supported by the current format.
    pub supported_surface_color_spaces: Vec<String>,
    /// User-requested viewer mode (Sdr / HdrPq / HdrHlg / MatchOutputColorSpace).
    pub requested_viewer_mode: String,
    /// User-requested output color space.
    pub requested_output_color_space: String,
    /// Resolved output color space after display policy application.
    pub resolved_output_color_space: String,
    /// OCIO display name, if resolved.
    pub ocio_display: Option<String>,
    /// OCIO view name, if resolved.
    pub ocio_view: Option<String>,
    /// Monitor ICC profile status.
    pub monitor_profile_status: MonitorProfileStatus,
    /// HDR/EDR status (full chain diagnosis).
    pub hdr_status: HdrStatus,
    /// Overall validation status.
    pub validation_status: DisplayValidationStatus,
    /// Non-blocking warnings.
    pub warnings: Vec<DisplayOutputWarning>,
    /// Blocking issues that prevent correct display output.
    pub blockers: Vec<DisplayOutputBlocker>,
    /// Why this snapshot was generated.
    pub refresh_reason: String,
}

/// Non-blocking display output warning.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DisplayOutputWarning {
    /// Stable warning code.
    pub code: String,
    /// Human-readable description.
    pub description: String,
}

impl DisplayOutputSnapshot {
    /// Whether the display output contract is valid (no blockers).
    pub fn is_valid(&self) -> bool {
        self.blockers.is_empty()
    }

    /// Whether any capability is unknown.
    pub fn has_unknown_capabilities(&self) -> bool {
        matches!(
            self.monitor_profile_status,
            MonitorProfileStatus::Unknown { .. }
                | MonitorProfileStatus::IccProfileUnsupported { .. }
                | MonitorProfileStatus::IccProfileReadError { .. }
                | MonitorProfileStatus::IccProfileUnmapped { .. }
        ) || matches!(
            self.hdr_status,
            HdrStatus::Unknown { .. } | HdrStatus::RequestedMonitorUnknown { .. }
        )
    }

    /// Compute a deterministic hash for cache invalidation.
    pub fn contract_generation(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        self.display_id.hash(&mut hasher);
        self.scale_factor.hash(&mut hasher);
        self.surface_format.hash(&mut hasher);
        self.surface_color_space.hash(&mut hasher);
        self.surface_hdr_mode.hash(&mut hasher);
        self.requested_viewer_mode.hash(&mut hasher);
        self.requested_output_color_space.hash(&mut hasher);
        self.resolved_output_color_space.hash(&mut hasher);
        self.ocio_display.hash(&mut hasher);
        self.ocio_view.hash(&mut hasher);
        self.monitor_profile_status.hash(&mut hasher);
        self.hdr_status.hash(&mut hasher);
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sdr_pass_snapshot() -> DisplayOutputSnapshot {
        DisplayOutputSnapshot {
            display_id: DisplayId {
                name: Some("Test Monitor".to_owned()),
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
        }
    }

    #[test]
    fn sdr_pass_snapshot_is_valid() {
        let snapshot = sdr_pass_snapshot();
        assert!(snapshot.is_valid());
        assert!(!snapshot.has_unknown_capabilities());
        assert_eq!(snapshot.validation_status, DisplayValidationStatus::Pass);
    }

    #[test]
    fn contract_generation_changes_with_display_id() {
        let a = sdr_pass_snapshot();
        let mut b = sdr_pass_snapshot();
        b.display_id.name = Some("Different Monitor".to_owned());
        assert_ne!(a.contract_generation(), b.contract_generation());
    }

    #[test]
    fn contract_generation_changes_with_surface_format() {
        let a = sdr_pass_snapshot();
        let mut b = sdr_pass_snapshot();
        b.surface_format = "Rgba16Float".to_owned();
        assert_ne!(a.contract_generation(), b.contract_generation());
    }

    #[test]
    fn contract_generation_changes_with_scale_factor() {
        let a = sdr_pass_snapshot();
        let mut b = sdr_pass_snapshot();
        b.scale_factor = ScaleFactorPpm::from_f64(2.0);
        assert_ne!(a.contract_generation(), b.contract_generation());
    }

    #[test]
    fn contract_generation_changes_with_viewer_mode() {
        let a = sdr_pass_snapshot();
        let mut b = sdr_pass_snapshot();
        b.requested_viewer_mode = "HdrPq".to_owned();
        assert_ne!(a.contract_generation(), b.contract_generation());
    }

    #[test]
    fn contract_generation_changes_with_ocio_view() {
        let a = sdr_pass_snapshot();
        let mut b = sdr_pass_snapshot();
        b.ocio_view = Some("Filmic".to_owned());
        assert_ne!(a.contract_generation(), b.contract_generation());
    }

    #[test]
    fn icc_profile_unsupported_has_unknown_capabilities() {
        let mut snapshot = sdr_pass_snapshot();
        snapshot.monitor_profile_status = MonitorProfileStatus::IccProfileUnsupported {
            feature_code: "os_icc_profile".to_owned(),
            profile_path: None,
            reason: "OS ICC discovery not implemented".to_owned(),
        };
        assert!(snapshot.has_unknown_capabilities());
    }

    #[test]
    fn icc_profile_read_error_has_unknown_capabilities() {
        let mut snapshot = sdr_pass_snapshot();
        snapshot.monitor_profile_status = MonitorProfileStatus::IccProfileReadError {
            profile_path: Some("/path/to/profile.icc".to_owned()),
            reason: "file not found".to_owned(),
        };
        assert!(snapshot.has_unknown_capabilities());
    }

    #[test]
    fn icc_profile_unmapped_has_unknown_capabilities() {
        let mut snapshot = sdr_pass_snapshot();
        snapshot.monitor_profile_status = MonitorProfileStatus::IccProfileUnmapped {
            profile_path: Some("/path/to/profile.icc".to_owned()),
            parsed_color_space: Some(ColorSpace::DciP3),
            reason: "no OCIO display match".to_owned(),
        };
        assert!(snapshot.has_unknown_capabilities());
    }

    #[test]
    fn hdr_unknown_has_unknown_capabilities() {
        let mut snapshot = sdr_pass_snapshot();
        snapshot.hdr_status = HdrStatus::RequestedMonitorUnknown {
            mode: "HdrPq".to_owned(),
            reason: "display_hdr_info unavailable".to_owned(),
        };
        assert!(snapshot.has_unknown_capabilities());
    }

    #[test]
    fn blocker_codes_are_distinct() {
        let blockers = [
            DisplayOutputBlocker::SurfaceContractMismatch {
                surface_format: "Bgra8UnormSrgb".to_owned(),
                output_color_space: "Rec709".to_owned(),
            },
            DisplayOutputBlocker::UnsupportedDisplayColorSpace {
                display_color_space: "Rec2020".to_owned(),
            },
            DisplayOutputBlocker::UnsupportedHdrSwapchainOrEdr { hdr_mode: "HdrPq".to_owned() },
            DisplayOutputBlocker::MonitorIccProfileUnsupported {
                feature_code: "os_icc_profile".to_owned(),
                profile_path: None,
                reason: "not implemented".to_owned(),
            },
            DisplayOutputBlocker::MonitorIccProfileInvalid {
                profile_path: Some("bad.icc".to_owned()),
                reason: "parse error".to_owned(),
            },
            DisplayOutputBlocker::MonitorIccProfileUnmapped {
                profile_path: Some("display.icc".to_owned()),
                parsed_color_space: Some(ColorSpace::DciP3),
                reason: "no OCIO display/view match".to_owned(),
            },
            DisplayOutputBlocker::MonitorHdrCapabilityUnknown { reason: "no HDR info".to_owned() },
            DisplayOutputBlocker::MonitorHdrCapabilityUnsupported {
                hdr_mode: "HdrPq".to_owned(),
                evidence: "max_luminance=0".to_owned(),
            },
            DisplayOutputBlocker::DisplayMovedContractStale {
                previous_display: Some("A".to_owned()),
                new_display: Some("B".to_owned()),
            },
            DisplayOutputBlocker::OcioDisplayViewMissing { display: None, view: None },
            DisplayOutputBlocker::OcioConfigUnavailable,
        ];
        let mut codes: Vec<&str> = blockers.iter().map(|b| b.code()).collect();
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), blockers.len(), "blocker codes must be unique");
    }

    #[test]
    fn all_blockers_have_action_codes() {
        let blockers = [
            DisplayOutputBlocker::SurfaceContractMismatch {
                surface_format: "Bgra8UnormSrgb".to_owned(),
                output_color_space: "Rec709".to_owned(),
            },
            DisplayOutputBlocker::UnsupportedDisplayColorSpace {
                display_color_space: "Rec2020".to_owned(),
            },
            DisplayOutputBlocker::UnsupportedHdrSwapchainOrEdr { hdr_mode: "HdrPq".to_owned() },
            DisplayOutputBlocker::MonitorIccProfileUnsupported {
                feature_code: "os_icc_profile".to_owned(),
                profile_path: None,
                reason: "not implemented".to_owned(),
            },
            DisplayOutputBlocker::MonitorIccProfileInvalid {
                profile_path: None,
                reason: "error".to_owned(),
            },
            DisplayOutputBlocker::MonitorIccProfileUnmapped {
                profile_path: None,
                parsed_color_space: Some(ColorSpace::DciP3),
                reason: "unmapped".to_owned(),
            },
            DisplayOutputBlocker::MonitorHdrCapabilityUnknown { reason: "unknown".to_owned() },
            DisplayOutputBlocker::MonitorHdrCapabilityUnsupported {
                hdr_mode: "HdrPq".to_owned(),
                evidence: "evidence".to_owned(),
            },
            DisplayOutputBlocker::DisplayMovedContractStale {
                previous_display: None,
                new_display: None,
            },
            DisplayOutputBlocker::OcioDisplayViewMissing { display: None, view: None },
            DisplayOutputBlocker::OcioConfigUnavailable,
        ];
        for blocker in &blockers {
            assert!(!blocker.code().is_empty());
            assert!(!blocker.area_code().is_empty());
            assert!(!blocker.action_code().is_empty());
        }
    }

    #[test]
    fn monitor_profile_status_serde_roundtrip() {
        let status = MonitorProfileStatus::IccProfileUnsupported {
            feature_code: "os_icc_profile".to_owned(),
            profile_path: Some("/test".to_owned()),
            reason: "not implemented".to_owned(),
        };
        let json = serde_json::to_string(&status).unwrap();
        let decoded: MonitorProfileStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(status, decoded);
    }

    #[test]
    fn hdr_status_serde_roundtrip() {
        let status = HdrStatus::RequestedMonitorUnknown {
            mode: "HdrPq".to_owned(),
            reason: "display_hdr_info unavailable".to_owned(),
        };
        let json = serde_json::to_string(&status).unwrap();
        let decoded: HdrStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(status, decoded);
    }

    #[test]
    fn display_output_snapshot_serde_roundtrip() {
        let snapshot = sdr_pass_snapshot();
        let json = serde_json::to_string(&snapshot).unwrap();
        let decoded: DisplayOutputSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snapshot, decoded);
    }

    #[test]
    fn display_validation_status_warn_has_correct_display() {
        assert_eq!(DisplayValidationStatus::Pass.to_string(), "Pass");
        assert_eq!(DisplayValidationStatus::Warn.to_string(), "Warn");
        assert_eq!(DisplayValidationStatus::Fail.to_string(), "Fail");
    }

    #[test]
    fn monitor_profile_status_display_format() {
        assert_eq!(
            MonitorProfileStatus::NotRequested.to_string(),
            "Not Requested"
        );
        let icc_unsup = MonitorProfileStatus::IccProfileUnsupported {
            feature_code: "os_icc_profile".to_owned(),
            profile_path: None,
            reason: "not implemented".to_owned(),
        };
        assert!(icc_unsup.to_string().contains("not implemented"));
        let managed = MonitorProfileStatus::ManagedColorSpace {
            color_space: ColorSpace::DciP3,
            source: MonitorProfileSource::UserConfigured,
        };
        assert!(managed.to_string().contains("DciP3"));
    }

    #[test]
    fn hdr_status_display_format() {
        assert_eq!(HdrStatus::NotRequested.to_string(), "Not Requested");
        let unsupported = HdrStatus::RequestedMonitorUnknown {
            mode: "HdrPq".to_owned(),
            reason: "no info".to_owned(),
        };
        assert!(unsupported.to_string().contains("Monitor Unknown"));
    }

    #[test]
    fn display_output_blocker_display_format() {
        let blocker = DisplayOutputBlocker::MonitorIccProfileUnsupported {
            feature_code: "os_icc_profile".to_owned(),
            profile_path: None,
            reason: "not implemented".to_owned(),
        };
        assert!(blocker.to_string().contains("ICC profile unsupported"));
    }

    #[test]
    fn scale_factor_ppm_nan_produces_default() {
        let ppm = ScaleFactorPpm::from_f64(f64::NAN);
        assert_eq!(ppm, ScaleFactorPpm::default());
    }

    #[test]
    fn scale_factor_ppm_infinity_produces_default() {
        let ppm = ScaleFactorPpm::from_f64(f64::INFINITY);
        assert_eq!(ppm, ScaleFactorPpm::default());
    }

    #[test]
    fn scale_factor_ppm_zero_produces_zero() {
        let ppm = ScaleFactorPpm::from_f64(0.0);
        assert_eq!(ppm, ScaleFactorPpm(0));
    }

    #[test]
    fn hdr_unknown_status_has_unknown_capabilities() {
        let mut snapshot = sdr_pass_snapshot();
        snapshot.hdr_status = HdrStatus::Unknown { reason: "test".to_owned() };
        assert!(snapshot.has_unknown_capabilities());
    }

    #[test]
    fn hdr_requested_swapchain_mismatch_has_no_unknown_capabilities() {
        let mut snapshot = sdr_pass_snapshot();
        snapshot.hdr_status = HdrStatus::RequestedSwapchainMismatch {
            mode: "HdrPq".to_owned(),
            surface_format: "Bgra8UnormSrgb".to_owned(),
            surface_color_space: "Srgb".to_owned(),
        };
        assert!(!snapshot.has_unknown_capabilities());
    }
}
