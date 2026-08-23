//! UI-independent mapping from display-contract evidence to Preview blockers.

use mondrian_core::display_contract::{DisplayOutputBlocker, DisplayOutputSnapshot};

use crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlocker;

/// Project every display-contract blocker into the Preview execution taxonomy.
pub(crate) fn preview_blockers_from_snapshot(
    snapshot: &DisplayOutputSnapshot,
) -> Vec<PreviewGpuOutputBlocker> {
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
