//! Shared color report vocabulary for preview/export/viewer convergence.
//!
//! This module defines the canonical check, root-cause, and action codes that
//! preview, export, and viewer health reports share. Preview and export reports
//! must use these codes directly; viewer reports may use their own codes but
//! should map shared concepts to these canonical forms for cross-report
//! comparison.

// ── Shared check codes ────────────────────────────────────────────────────────

/// Check codes shared between preview and export color health reports.
///
/// These are the canonical codes that preview/export normalization functions
/// use when comparing report signatures. Reports may emit additional
/// context-specific codes, but shared codes must use these exact strings.
pub mod check {
    /// All composites stayed on the float/linear path.
    pub const FULLY_FLOAT_LINEAR: &str = "fully_float_linear";
    /// GPU color scheduling has no blockers.
    pub const GPU_PATH_READY: &str = "gpu_path_ready";
    /// Number of GPU color stage blockers.
    pub const GPU_BLOCKERS: &str = "gpu_blockers";
    /// Number of upload/readback transfer stages.
    pub const TRANSFER_STAGES: &str = "transfer_stages";
    /// Total structured legacy RGBA8 fallback reasons.
    pub const LEGACY_REASON_TOTAL: &str = "legacy_reason_total";
    /// Inputs rejected by missing-metadata policy.
    pub const POLICY_REJECTIONS: &str = "policy_rejections";
    /// Number of CPU output fallback frames.
    pub const CPU_OUTPUT_FALLBACK_FRAMES: &str = "cpu_output_fallback_frames";
    /// Total structured GPU output blockers.
    pub const GPU_OUTPUT_BLOCKERS: &str = "gpu_output_blockers";
}

/// All shared check codes for iteration/comparison.
pub const SHARED_CHECK_CODES: &[&str] = &[
    check::FULLY_FLOAT_LINEAR,
    check::GPU_PATH_READY,
    check::GPU_BLOCKERS,
    check::TRANSFER_STAGES,
    check::LEGACY_REASON_TOTAL,
    check::POLICY_REJECTIONS,
    check::CPU_OUTPUT_FALLBACK_FRAMES,
    check::GPU_OUTPUT_BLOCKERS,
];

// ── Shared root cause codes ───────────────────────────────────────────────────

/// Canonical root cause codes. Preview/export may emit context-prefixed
/// versions (e.g., `preview_gpu_color_stage_blocked`), but normalization
/// functions map them to these canonical forms.
pub mod root_cause {
    /// Color health evidence was not captured.
    pub const MISSING_COLOR_EVIDENCE: &str = "missing_color_evidence";
    /// GPU color stage has blockers preventing native execution.
    pub const GPU_COLOR_STAGE_BLOCKED: &str = "gpu_color_stage_blocked";
    /// Upload/readback transfer stages are present.
    pub const TRANSFER_STAGE_PRESENT: &str = "transfer_stage_present";
    /// Composites fell back to legacy RGBA8 path.
    pub const LEGACY_RGBA8_COMPOSITE_PATH: &str = "legacy_rgba8_composite_path";
    /// Missing-metadata policy rejected a media source.
    pub const INPUT_COLOR_POLICY_REJECTED_SOURCE: &str = "input_color_policy_rejected_source";
    /// Preview output boundary fell back to CPU RGBA8.
    pub const CPU_OUTPUT_FALLBACK: &str = "cpu_output_fallback";
    /// Preview GPU output boundary has structured blockers.
    pub const GPU_OUTPUT_BLOCKED: &str = "gpu_output_blocked";
}

// ── Shared action codes ───────────────────────────────────────────────────────

/// Canonical action codes.
pub mod action {
    /// Inspect the color evidence capture path.
    pub const INSPECT_COLOR_EVIDENCE: &str = "inspect_color_evidence";
    /// Inspect per-asset color diagnostics.
    pub const INSPECT_ASSET_COLOR_DIAGNOSTICS: &str = "inspect_asset_color_diagnostics";
    /// Inspect renderer GPU color blocker breakdown.
    pub const INSPECT_GPU_BLOCKERS: &str = "inspect_gpu_blockers";
    /// Trace and remove upload/readback transfer stages.
    pub const REMOVE_TRANSFER_STAGE: &str = "remove_transfer_stage";
    /// Migrate legacy RGBA8 composite reasons back to float/linear.
    pub const MIGRATE_LEGACY_COMPOSITE_REASON: &str = "migrate_legacy_composite_reason";
    /// Investigate why CPU output fallback was used.
    pub const INVESTIGATE_CPU_FALLBACK: &str = "investigate_cpu_fallback";
    /// Prepare OCIO GPU resources (config, processor, shader extraction).
    pub const PREPARE_OCIO_GPU_RESOURCES: &str = "prepare_ocio_gpu_resources";
    /// Configure display output contract to match the output boundary.
    pub const CONFIGURE_DISPLAY_CONTRACT: &str = "configure_display_contract";
    /// Inspect preview GPU output blocker breakdown.
    pub const INSPECT_PREVIEW_GPU_OUTPUT_BLOCKERS: &str = "inspect_preview_gpu_output_blockers";
}

// ── Normalization functions ───────────────────────────────────────────────────

/// Normalize a preview/export-specific root cause code to its canonical form.
///
/// Preview and export may emit context-prefixed codes (e.g.,
/// `preview_gpu_color_stage_blocked` or `export_gpu_color_stage_blocked`).
/// This function strips the context prefix and returns the shared canonical
/// code for cross-report comparison.
pub fn normalize_root_cause_code(code: &str) -> &str {
    match code {
        "missing_preview_color_evidence" | "missing_export_color_evidence" => {
            root_cause::MISSING_COLOR_EVIDENCE
        }
        "preview_gpu_color_stage_blocked" | "export_gpu_color_stage_blocked" => {
            root_cause::GPU_COLOR_STAGE_BLOCKED
        }
        "preview_transfer_stage_present" | "export_transfer_stage_present" => {
            root_cause::TRANSFER_STAGE_PRESENT
        }
        "preview_cpu_output_fallback" | "export_cpu_output_fallback" => {
            root_cause::CPU_OUTPUT_FALLBACK
        }
        "preview_gpu_output_blocked" => root_cause::GPU_OUTPUT_BLOCKED,
        other => other,
    }
}

/// Normalize a preview/export-specific action code to its canonical form.
pub fn normalize_action_code(code: &str) -> &str {
    match code {
        "inspect_preview_diagnostics" | "inspect_export_render_path" => {
            action::INSPECT_COLOR_EVIDENCE
        }
        "inspect_preview_asset_color_diagnostics" | "inspect_asset_color_diagnostics" => {
            action::INSPECT_ASSET_COLOR_DIAGNOSTICS
        }
        "inspect_preview_gpu_blockers" | "inspect_export_gpu_blockers" => {
            action::INSPECT_GPU_BLOCKERS
        }
        "remove_preview_transfer_stage" | "remove_export_transfer_stage" => {
            action::REMOVE_TRANSFER_STAGE
        }
        "migrate_preview_legacy_composite_reason" | "migrate_legacy_composite_reason" => {
            action::MIGRATE_LEGACY_COMPOSITE_REASON
        }
        "investigate_cpu_fallback" => action::INVESTIGATE_CPU_FALLBACK,
        "prepare_ocio_gpu_resources" => action::PREPARE_OCIO_GPU_RESOURCES,
        "configure_display_contract" => action::CONFIGURE_DISPLAY_CONTRACT,
        "inspect_preview_gpu_output_blockers" => action::INSPECT_PREVIEW_GPU_OUTPUT_BLOCKERS,
        other => other,
    }
}

/// Check whether a check code is in the shared vocabulary.
pub fn is_shared_check_code(code: &str) -> bool {
    SHARED_CHECK_CODES.contains(&code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_check_codes_are_canonical() {
        assert_eq!(check::FULLY_FLOAT_LINEAR, "fully_float_linear");
        assert_eq!(check::GPU_PATH_READY, "gpu_path_ready");
        assert_eq!(check::GPU_BLOCKERS, "gpu_blockers");
        assert_eq!(check::TRANSFER_STAGES, "transfer_stages");
        assert_eq!(check::LEGACY_REASON_TOTAL, "legacy_reason_total");
        assert_eq!(check::POLICY_REJECTIONS, "policy_rejections");
        assert_eq!(
            check::CPU_OUTPUT_FALLBACK_FRAMES,
            "cpu_output_fallback_frames"
        );
        assert_eq!(check::GPU_OUTPUT_BLOCKERS, "gpu_output_blockers");
    }

    #[test]
    fn normalize_root_cause_strips_context_prefix() {
        assert_eq!(
            normalize_root_cause_code("preview_gpu_color_stage_blocked"),
            root_cause::GPU_COLOR_STAGE_BLOCKED
        );
        assert_eq!(
            normalize_root_cause_code("export_gpu_color_stage_blocked"),
            root_cause::GPU_COLOR_STAGE_BLOCKED
        );
        assert_eq!(
            normalize_root_cause_code("missing_preview_color_evidence"),
            root_cause::MISSING_COLOR_EVIDENCE
        );
        assert_eq!(
            normalize_root_cause_code("legacy_rgba8_composite_path"),
            "legacy_rgba8_composite_path"
        );
        assert_eq!(
            normalize_root_cause_code("preview_cpu_output_fallback"),
            root_cause::CPU_OUTPUT_FALLBACK
        );
        assert_eq!(
            normalize_root_cause_code("export_cpu_output_fallback"),
            root_cause::CPU_OUTPUT_FALLBACK
        );
        assert_eq!(
            normalize_root_cause_code("preview_gpu_output_blocked"),
            root_cause::GPU_OUTPUT_BLOCKED
        );
    }

    #[test]
    fn normalize_action_strips_context_prefix() {
        assert_eq!(
            normalize_action_code("inspect_preview_diagnostics"),
            action::INSPECT_COLOR_EVIDENCE
        );
        assert_eq!(
            normalize_action_code("inspect_export_render_path"),
            action::INSPECT_COLOR_EVIDENCE
        );
        assert_eq!(
            normalize_action_code("migrate_legacy_composite_reason"),
            action::MIGRATE_LEGACY_COMPOSITE_REASON
        );
        assert_eq!(
            normalize_action_code("investigate_cpu_fallback"),
            action::INVESTIGATE_CPU_FALLBACK
        );
        assert_eq!(
            normalize_action_code("prepare_ocio_gpu_resources"),
            action::PREPARE_OCIO_GPU_RESOURCES
        );
        assert_eq!(
            normalize_action_code("configure_display_contract"),
            action::CONFIGURE_DISPLAY_CONTRACT
        );
        assert_eq!(
            normalize_action_code("inspect_preview_gpu_output_blockers"),
            action::INSPECT_PREVIEW_GPU_OUTPUT_BLOCKERS
        );
    }

    #[test]
    fn is_shared_check_code_works() {
        assert!(is_shared_check_code("fully_float_linear"));
        assert!(is_shared_check_code("gpu_blockers"));
        assert!(is_shared_check_code("cpu_output_fallback_frames"));
        assert!(is_shared_check_code("gpu_output_blockers"));
        assert!(!is_shared_check_code("diagnosed_frames_present"));
        assert!(!is_shared_check_code("media_warnings"));
    }
}
