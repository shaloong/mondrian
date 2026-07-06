//! Structured GPU output blocker taxonomy for the preview/viewer path.
//!
//! Every reason the GPU color output path cannot execute is captured as a typed
//! enum variant.  Blockers are recorded per-frame so that health reports,
//! JSONL diagnostics, and tests can inspect individual failure modes without
//! opaque counters.
//!
//! The taxonomy covers three layers:
//!
//! 1. **Renderer OCIO blockers** — shader extraction / backend preparation
//!    failures detected during `RenderGpuOutputBoundaryRuntime` planning.
//! 2. **Display contract blockers** — surface format / color space / HDR
//!    constraints detected by the app-window before GPU recording.
//! 3. **Frame residency / scheduling blockers** — working-frame or external
//!    texture lifecycle issues detected during app-window scheduling.

use mondrian_renderer::RenderColorStageGpuBlockerBreakdown;
use serde::{Deserialize, Serialize};

/// Typed reasons the GPU color output boundary cannot execute for preview.
///
/// Every reason is a typed enum variant — no opaque counters or string-scattered
/// diagnostics.  Health reports carry per-variant codes, areas, and actions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PreviewGpuOutputBlocker {
    /// OCIO config is not loaded or unavailable for GPU shader extraction.
    OcioConfigNotLoaded,
    /// OCIO processor could not be created for the requested transform.
    OcioProcessorUnavailable,
    /// OCIO GPU shader extraction failed (transpilation, Naga, or backend error).
    OcioGpuShaderExtractionFailed {
        /// Human-readable extraction failure reason.
        reason: String,
    },
    /// Backend shader module has not been prepared from the translated shader.
    ShaderModuleNotPrepared,
    /// OCIO LUT/uniform resources are not connected to a concrete bind group.
    OcioResourceBindGroupNotPrepared,
    /// Fullscreen wrapper shader is missing.
    FullscreenWrapperNotPrepared,
    /// Final render pipeline/render-pass node is missing.
    RenderPipelineNotPrepared,
    /// Surface format does not support the requested output color space.
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
    /// The working frame is not GPU-resident and must be uploaded before the
    /// GPU output boundary.  This is the normal current-path behavior (CPU
    /// compositing → upload → GPU output transform).  It becomes a real
    /// blocker only if the upload fails or the frame contract is incompatible.
    FrameNotGpuResident,
    /// Compositing fell back to legacy RGBA8 before the GPU output boundary.
    LegacyRgba8CompositeBoundary {
        /// Number of legacy RGBA8 composites in the frame.
        legacy_composites: u64,
    },
    /// CPU fallback was explicitly requested for this output boundary.
    CpuFallbackRequested {
        /// Reason for the CPU fallback.
        reason: String,
    },
    /// A feature is explicitly unsupported in the current implementation.
    ///
    /// This covers architectural limitations that cannot be resolved without
    /// OS-level integration (ICC profiles, HDR/EDR display queries).
    UnsupportedFeature {
        /// Stable feature code.
        feature: String,
        /// Human-readable explanation.
        reason: String,
    },
}

impl PreviewGpuOutputBlocker {
    /// Machine-readable code for this blocker variant.
    pub fn code(&self) -> &'static str {
        match self {
            Self::OcioConfigNotLoaded => "ocio_config_not_loaded",
            Self::OcioProcessorUnavailable => "ocio_processor_unavailable",
            Self::OcioGpuShaderExtractionFailed { .. } => "ocio_gpu_shader_extraction_failed",
            Self::ShaderModuleNotPrepared => "shader_module_not_prepared",
            Self::OcioResourceBindGroupNotPrepared => "ocio_resource_bind_group_not_prepared",
            Self::FullscreenWrapperNotPrepared => "fullscreen_wrapper_not_prepared",
            Self::RenderPipelineNotPrepared => "render_pipeline_not_prepared",
            Self::SurfaceContractMismatch { .. } => "surface_contract_mismatch",
            Self::UnsupportedDisplayColorSpace { .. } => "unsupported_display_color_space",
            Self::UnsupportedHdrSwapchainOrEdr { .. } => "unsupported_hdr_swapchain_or_edr",
            Self::FrameNotGpuResident => "frame_not_gpu_resident",
            Self::LegacyRgba8CompositeBoundary { .. } => "legacy_rgba8_composite_boundary",
            Self::CpuFallbackRequested { .. } => "cpu_fallback_requested",
            Self::UnsupportedFeature { .. } => "unsupported_feature",
        }
    }

    /// Human-readable description of this blocker.
    pub fn description(&self) -> String {
        match self {
            Self::OcioConfigNotLoaded => "OCIO config not loaded".to_owned(),
            Self::OcioProcessorUnavailable => "OCIO processor unavailable".to_owned(),
            Self::OcioGpuShaderExtractionFailed { reason } => {
                format!("OCIO GPU shader extraction failed: {reason}")
            }
            Self::ShaderModuleNotPrepared => "Backend shader module not prepared".to_owned(),
            Self::OcioResourceBindGroupNotPrepared => {
                "OCIO resource bind group not prepared".to_owned()
            }
            Self::FullscreenWrapperNotPrepared => "Fullscreen wrapper not prepared".to_owned(),
            Self::RenderPipelineNotPrepared => "Render pipeline not prepared".to_owned(),
            Self::SurfaceContractMismatch { surface_format, output_color_space } => {
                format!(
                    "Surface format {surface_format} does not support output color space {output_color_space}"
                )
            }
            Self::UnsupportedDisplayColorSpace { display_color_space } => {
                format!("Display color space {display_color_space} is not supported")
            }
            Self::UnsupportedHdrSwapchainOrEdr { hdr_mode } => {
                format!("HDR swapchain/EDR mode {hdr_mode} is not supported")
            }
            Self::FrameNotGpuResident => {
                "Working frame is not GPU-resident (CPU→GPU upload required)".to_owned()
            }
            Self::LegacyRgba8CompositeBoundary { legacy_composites } => {
                format!(
                    "Compositing fell back to legacy RGBA8 ({legacy_composites} legacy composites)"
                )
            }
            Self::CpuFallbackRequested { reason } => {
                format!("CPU fallback requested: {reason}")
            }
            Self::UnsupportedFeature { feature, reason } => {
                format!("Unsupported feature {feature}: {reason}")
            }
        }
    }

    /// Diagnostic area code for health report root-cause attribution.
    pub fn area_code(&self) -> &'static str {
        match self {
            Self::OcioConfigNotLoaded
            | Self::OcioProcessorUnavailable
            | Self::OcioGpuShaderExtractionFailed { .. }
            | Self::ShaderModuleNotPrepared
            | Self::OcioResourceBindGroupNotPrepared
            | Self::FullscreenWrapperNotPrepared
            | Self::RenderPipelineNotPrepared => "GpuColorPath",
            Self::SurfaceContractMismatch { .. }
            | Self::UnsupportedDisplayColorSpace { .. }
            | Self::UnsupportedHdrSwapchainOrEdr { .. } => "DisplayContract",
            Self::FrameNotGpuResident => "FrameResidency",
            Self::LegacyRgba8CompositeBoundary { .. } => "CompositePath",
            Self::CpuFallbackRequested { .. } => "CpuFallback",
            Self::UnsupportedFeature { .. } => "UnsupportedFeature",
        }
    }

    /// Suggested action code for health report follow-up.
    pub fn action_code(&self) -> &'static str {
        match self {
            Self::OcioConfigNotLoaded | Self::OcioProcessorUnavailable => {
                "prepare_ocio_gpu_resources"
            }
            Self::OcioGpuShaderExtractionFailed { .. } => "prepare_ocio_gpu_resources",
            Self::ShaderModuleNotPrepared
            | Self::OcioResourceBindGroupNotPrepared
            | Self::FullscreenWrapperNotPrepared
            | Self::RenderPipelineNotPrepared => "inspect_gpu_blocker_breakdown",
            Self::SurfaceContractMismatch { .. }
            | Self::UnsupportedDisplayColorSpace { .. }
            | Self::UnsupportedHdrSwapchainOrEdr { .. } => "configure_display_contract",
            Self::FrameNotGpuResident => "ensure_gpu_frame_residency",
            Self::LegacyRgba8CompositeBoundary { .. } => "avoid_legacy_rgba8_boundary",
            Self::CpuFallbackRequested { .. } => "investigate_cpu_fallback",
            Self::UnsupportedFeature { .. } => "document_unsupported_feature",
        }
    }

    /// Human-readable action description for health reports.
    pub fn action_description(&self) -> &'static str {
        match self {
            Self::OcioConfigNotLoaded | Self::OcioProcessorUnavailable => {
                "Load or reload the OCIO config and verify processor availability."
            }
            Self::OcioGpuShaderExtractionFailed { .. } => {
                "Inspect OCIO GPU shader extraction for the failing transform."
            }
            Self::ShaderModuleNotPrepared
            | Self::OcioResourceBindGroupNotPrepared
            | Self::FullscreenWrapperNotPrepared
            | Self::RenderPipelineNotPrepared => "Inspect renderer GPU color blocker breakdown.",
            Self::SurfaceContractMismatch { .. }
            | Self::UnsupportedDisplayColorSpace { .. }
            | Self::UnsupportedHdrSwapchainOrEdr { .. } => {
                "Reconfigure display output contract to match the output boundary."
            }
            Self::FrameNotGpuResident => {
                "Ensure the working frame is GPU-resident before output boundary recording."
            }
            Self::LegacyRgba8CompositeBoundary { .. } => {
                "Migrate legacy RGBA8 composite reasons back to float/linear."
            }
            Self::CpuFallbackRequested { .. } => {
                "Investigate why CPU fallback was requested and resolve the underlying cause."
            }
            Self::UnsupportedFeature { .. } => {
                "Document the unsupported feature limitation and track for future implementation."
            }
        }
    }
}

/// Structured breakdown of GPU output blockers for a preview/viewer frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewGpuOutputBlockerBreakdown {
    /// OCIO config not loaded.
    pub ocio_config_not_loaded: u64,
    /// OCIO processor unavailable.
    pub ocio_processor_unavailable: u64,
    /// OCIO GPU shader extraction failed.
    pub ocio_gpu_shader_extraction_failed: u64,
    /// Backend shader module not prepared.
    pub shader_module_not_prepared: u64,
    /// OCIO resource bind group not prepared.
    pub ocio_resource_bind_group_not_prepared: u64,
    /// Fullscreen wrapper not prepared.
    pub fullscreen_wrapper_not_prepared: u64,
    /// Render pipeline not prepared.
    pub render_pipeline_not_prepared: u64,
    /// Surface contract mismatch.
    pub surface_contract_mismatch: u64,
    /// Unsupported display color space.
    pub unsupported_display_color_space: u64,
    /// Unsupported HDR swapchain or EDR.
    pub unsupported_hdr_swapchain_or_edr: u64,
    /// Frame not GPU-resident.
    pub frame_not_gpu_resident: u64,
    /// Legacy RGBA8 composite boundary.
    pub legacy_rgba8_composite_boundary: u64,
    /// CPU fallback requested.
    pub cpu_fallback_requested: u64,
    /// Explicitly unsupported features.
    pub unsupported_features: u64,
}

impl PreviewGpuOutputBlockerBreakdown {
    /// Total counted blockers.
    pub fn total(self) -> u64 {
        self.ocio_config_not_loaded
            .saturating_add(self.ocio_processor_unavailable)
            .saturating_add(self.ocio_gpu_shader_extraction_failed)
            .saturating_add(self.shader_module_not_prepared)
            .saturating_add(self.ocio_resource_bind_group_not_prepared)
            .saturating_add(self.fullscreen_wrapper_not_prepared)
            .saturating_add(self.render_pipeline_not_prepared)
            .saturating_add(self.surface_contract_mismatch)
            .saturating_add(self.unsupported_display_color_space)
            .saturating_add(self.unsupported_hdr_swapchain_or_edr)
            .saturating_add(self.frame_not_gpu_resident)
            .saturating_add(self.legacy_rgba8_composite_boundary)
            .saturating_add(self.cpu_fallback_requested)
            .saturating_add(self.unsupported_features)
    }

    /// Whether all counts are zero.
    pub fn is_empty(self) -> bool {
        self.total() == 0
    }

    /// Record one blocker into the breakdown.
    pub fn record(&mut self, blocker: &PreviewGpuOutputBlocker) {
        match blocker {
            PreviewGpuOutputBlocker::OcioConfigNotLoaded => {
                self.ocio_config_not_loaded = self.ocio_config_not_loaded.saturating_add(1);
            }
            PreviewGpuOutputBlocker::OcioProcessorUnavailable => {
                self.ocio_processor_unavailable = self.ocio_processor_unavailable.saturating_add(1);
            }
            PreviewGpuOutputBlocker::OcioGpuShaderExtractionFailed { .. } => {
                self.ocio_gpu_shader_extraction_failed =
                    self.ocio_gpu_shader_extraction_failed.saturating_add(1);
            }
            PreviewGpuOutputBlocker::ShaderModuleNotPrepared => {
                self.shader_module_not_prepared = self.shader_module_not_prepared.saturating_add(1);
            }
            PreviewGpuOutputBlocker::OcioResourceBindGroupNotPrepared => {
                self.ocio_resource_bind_group_not_prepared =
                    self.ocio_resource_bind_group_not_prepared.saturating_add(1);
            }
            PreviewGpuOutputBlocker::FullscreenWrapperNotPrepared => {
                self.fullscreen_wrapper_not_prepared =
                    self.fullscreen_wrapper_not_prepared.saturating_add(1);
            }
            PreviewGpuOutputBlocker::RenderPipelineNotPrepared => {
                self.render_pipeline_not_prepared =
                    self.render_pipeline_not_prepared.saturating_add(1);
            }
            PreviewGpuOutputBlocker::SurfaceContractMismatch { .. } => {
                self.surface_contract_mismatch = self.surface_contract_mismatch.saturating_add(1);
            }
            PreviewGpuOutputBlocker::UnsupportedDisplayColorSpace { .. } => {
                self.unsupported_display_color_space =
                    self.unsupported_display_color_space.saturating_add(1);
            }
            PreviewGpuOutputBlocker::UnsupportedHdrSwapchainOrEdr { .. } => {
                self.unsupported_hdr_swapchain_or_edr =
                    self.unsupported_hdr_swapchain_or_edr.saturating_add(1);
            }
            PreviewGpuOutputBlocker::FrameNotGpuResident => {
                self.frame_not_gpu_resident = self.frame_not_gpu_resident.saturating_add(1);
            }
            PreviewGpuOutputBlocker::LegacyRgba8CompositeBoundary { .. } => {
                self.legacy_rgba8_composite_boundary =
                    self.legacy_rgba8_composite_boundary.saturating_add(1);
            }
            PreviewGpuOutputBlocker::CpuFallbackRequested { .. } => {
                self.cpu_fallback_requested = self.cpu_fallback_requested.saturating_add(1);
            }
            PreviewGpuOutputBlocker::UnsupportedFeature { .. } => {
                self.unsupported_features = self.unsupported_features.saturating_add(1);
            }
        }
    }

    /// Add counts from another breakdown.
    pub fn accumulate(&mut self, other: Self) {
        self.ocio_config_not_loaded =
            self.ocio_config_not_loaded.saturating_add(other.ocio_config_not_loaded);
        self.ocio_processor_unavailable =
            self.ocio_processor_unavailable.saturating_add(other.ocio_processor_unavailable);
        self.ocio_gpu_shader_extraction_failed = self
            .ocio_gpu_shader_extraction_failed
            .saturating_add(other.ocio_gpu_shader_extraction_failed);
        self.shader_module_not_prepared =
            self.shader_module_not_prepared.saturating_add(other.shader_module_not_prepared);
        self.ocio_resource_bind_group_not_prepared = self
            .ocio_resource_bind_group_not_prepared
            .saturating_add(other.ocio_resource_bind_group_not_prepared);
        self.fullscreen_wrapper_not_prepared = self
            .fullscreen_wrapper_not_prepared
            .saturating_add(other.fullscreen_wrapper_not_prepared);
        self.render_pipeline_not_prepared = self
            .render_pipeline_not_prepared
            .saturating_add(other.render_pipeline_not_prepared);
        self.surface_contract_mismatch =
            self.surface_contract_mismatch.saturating_add(other.surface_contract_mismatch);
        self.unsupported_display_color_space = self
            .unsupported_display_color_space
            .saturating_add(other.unsupported_display_color_space);
        self.unsupported_hdr_swapchain_or_edr = self
            .unsupported_hdr_swapchain_or_edr
            .saturating_add(other.unsupported_hdr_swapchain_or_edr);
        self.frame_not_gpu_resident =
            self.frame_not_gpu_resident.saturating_add(other.frame_not_gpu_resident);
        self.legacy_rgba8_composite_boundary = self
            .legacy_rgba8_composite_boundary
            .saturating_add(other.legacy_rgba8_composite_boundary);
        self.cpu_fallback_requested =
            self.cpu_fallback_requested.saturating_add(other.cpu_fallback_requested);
        self.unsupported_features =
            self.unsupported_features.saturating_add(other.unsupported_features);
    }

    /// Build from a renderer-level `RenderColorStageGpuBlockerBreakdown`.
    ///
    /// Maps renderer-native OCIO blockers to the preview-level taxonomy.
    pub fn from_renderer_breakdown(breakdown: RenderColorStageGpuBlockerBreakdown) -> Self {
        Self {
            ocio_config_not_loaded: breakdown.ocio_config_not_loaded,
            ocio_processor_unavailable: breakdown.ocio_processor_unavailable,
            ocio_gpu_shader_extraction_failed: breakdown.ocio_gpu_shader_extraction_failed,
            shader_module_not_prepared: breakdown.shader_module_not_prepared,
            ocio_resource_bind_group_not_prepared: breakdown.ocio_resource_bind_group_not_prepared,
            fullscreen_wrapper_not_prepared: breakdown.fullscreen_wrapper_not_prepared,
            render_pipeline_not_prepared: breakdown.render_pipeline_not_prepared,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_gpu_output_blocker_codes_are_stable() {
        let blockers = [
            PreviewGpuOutputBlocker::OcioConfigNotLoaded,
            PreviewGpuOutputBlocker::OcioProcessorUnavailable,
            PreviewGpuOutputBlocker::OcioGpuShaderExtractionFailed { reason: "test".to_owned() },
            PreviewGpuOutputBlocker::ShaderModuleNotPrepared,
            PreviewGpuOutputBlocker::OcioResourceBindGroupNotPrepared,
            PreviewGpuOutputBlocker::FullscreenWrapperNotPrepared,
            PreviewGpuOutputBlocker::RenderPipelineNotPrepared,
            PreviewGpuOutputBlocker::SurfaceContractMismatch {
                surface_format: "Bgra8Unorm".to_owned(),
                output_color_space: "Rec709".to_owned(),
            },
            PreviewGpuOutputBlocker::UnsupportedDisplayColorSpace {
                display_color_space: "DisplayP3".to_owned(),
            },
            PreviewGpuOutputBlocker::UnsupportedHdrSwapchainOrEdr { hdr_mode: "HdrPq".to_owned() },
            PreviewGpuOutputBlocker::FrameNotGpuResident,
            PreviewGpuOutputBlocker::LegacyRgba8CompositeBoundary { legacy_composites: 2 },
            PreviewGpuOutputBlocker::CpuFallbackRequested { reason: "test".to_owned() },
            PreviewGpuOutputBlocker::UnsupportedFeature {
                feature: "os_icc_profile".to_owned(),
                reason: "not implemented".to_owned(),
            },
        ];

        for blocker in &blockers {
            assert!(!blocker.code().is_empty(), "code for {blocker:?}");
            assert!(
                !blocker.description().is_empty(),
                "description for {blocker:?}"
            );
            assert!(!blocker.area_code().is_empty(), "area_code for {blocker:?}");
            assert!(
                !blocker.action_code().is_empty(),
                "action_code for {blocker:?}"
            );
            assert!(
                !blocker.action_description().is_empty(),
                "action_description for {blocker:?}"
            );
        }
    }

    #[test]
    fn preview_gpu_output_blocker_breakdown_record_and_total() {
        let mut breakdown = PreviewGpuOutputBlockerBreakdown::default();
        assert!(breakdown.is_empty());
        assert_eq!(breakdown.total(), 0);

        breakdown.record(&PreviewGpuOutputBlocker::OcioConfigNotLoaded);
        breakdown.record(&PreviewGpuOutputBlocker::ShaderModuleNotPrepared);
        breakdown.record(&PreviewGpuOutputBlocker::SurfaceContractMismatch {
            surface_format: "Bgra8Unorm".to_owned(),
            output_color_space: "Rec709".to_owned(),
        });
        breakdown.record(&PreviewGpuOutputBlocker::FrameNotGpuResident);
        breakdown
            .record(&PreviewGpuOutputBlocker::CpuFallbackRequested { reason: "test".to_owned() });
        breakdown.record(&PreviewGpuOutputBlocker::UnsupportedFeature {
            feature: "os_icc_profile".to_owned(),
            reason: "not implemented".to_owned(),
        });

        assert_eq!(breakdown.total(), 6);
        assert!(!breakdown.is_empty());
        assert_eq!(breakdown.ocio_config_not_loaded, 1);
        assert_eq!(breakdown.shader_module_not_prepared, 1);
        assert_eq!(breakdown.surface_contract_mismatch, 1);
        assert_eq!(breakdown.frame_not_gpu_resident, 1);
        assert_eq!(breakdown.cpu_fallback_requested, 1);
        assert_eq!(breakdown.unsupported_features, 1);
    }

    #[test]
    fn preview_gpu_output_blocker_breakdown_accumulate() {
        let mut a = PreviewGpuOutputBlockerBreakdown::default();
        a.record(&PreviewGpuOutputBlocker::OcioConfigNotLoaded);
        a.record(&PreviewGpuOutputBlocker::OcioConfigNotLoaded);

        let mut b = PreviewGpuOutputBlockerBreakdown::default();
        b.record(&PreviewGpuOutputBlocker::OcioConfigNotLoaded);
        b.record(&PreviewGpuOutputBlocker::FrameNotGpuResident);

        a.accumulate(b);
        assert_eq!(a.ocio_config_not_loaded, 3);
        assert_eq!(a.frame_not_gpu_resident, 1);
        assert_eq!(a.total(), 4);
    }

    #[test]
    fn from_renderer_breakdown_maps_common_blockers() {
        let renderer_breakdown = RenderColorStageGpuBlockerBreakdown {
            shader_module_not_prepared: 1,
            ocio_resource_bind_group_not_prepared: 2,
            fullscreen_wrapper_not_prepared: 0,
            render_pipeline_not_prepared: 1,
            ocio_config_not_loaded: 0,
            ocio_processor_unavailable: 0,
            ocio_gpu_shader_extraction_failed: 3,
        };

        let preview_breakdown =
            PreviewGpuOutputBlockerBreakdown::from_renderer_breakdown(renderer_breakdown);
        assert_eq!(preview_breakdown.shader_module_not_prepared, 1);
        assert_eq!(preview_breakdown.ocio_resource_bind_group_not_prepared, 2);
        assert_eq!(preview_breakdown.render_pipeline_not_prepared, 1);
        assert_eq!(preview_breakdown.ocio_gpu_shader_extraction_failed, 3);
        assert_eq!(preview_breakdown.surface_contract_mismatch, 0);
        assert_eq!(preview_breakdown.total(), 7);
    }

    #[test]
    fn all_blocker_variants_have_distinct_codes() {
        let codes = [
            PreviewGpuOutputBlocker::OcioConfigNotLoaded.code(),
            PreviewGpuOutputBlocker::OcioProcessorUnavailable.code(),
            PreviewGpuOutputBlocker::OcioGpuShaderExtractionFailed { reason: String::new() }.code(),
            PreviewGpuOutputBlocker::ShaderModuleNotPrepared.code(),
            PreviewGpuOutputBlocker::OcioResourceBindGroupNotPrepared.code(),
            PreviewGpuOutputBlocker::FullscreenWrapperNotPrepared.code(),
            PreviewGpuOutputBlocker::RenderPipelineNotPrepared.code(),
            PreviewGpuOutputBlocker::SurfaceContractMismatch {
                surface_format: String::new(),
                output_color_space: String::new(),
            }
            .code(),
            PreviewGpuOutputBlocker::UnsupportedDisplayColorSpace {
                display_color_space: String::new(),
            }
            .code(),
            PreviewGpuOutputBlocker::UnsupportedHdrSwapchainOrEdr { hdr_mode: String::new() }
                .code(),
            PreviewGpuOutputBlocker::FrameNotGpuResident.code(),
            PreviewGpuOutputBlocker::LegacyRgba8CompositeBoundary { legacy_composites: 0 }.code(),
            PreviewGpuOutputBlocker::CpuFallbackRequested { reason: String::new() }.code(),
            PreviewGpuOutputBlocker::UnsupportedFeature {
                feature: String::new(),
                reason: String::new(),
            }
            .code(),
        ];
        let mut unique = codes.to_vec();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), codes.len(), "blocker codes must be unique");
    }
}
