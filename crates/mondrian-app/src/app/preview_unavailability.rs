//! Typed terminal reasons why Preview cannot publish the current output.
//!
//! This Module preserves three facts across Timeline, media, CPU/GPU execution,
//! Headless, and Window Adapters:
//!
//! - whether the absence is expected, correctness-blocked, or an execution failure;
//! - the production stage that owns the reason;
//! - diagnostic detail that never becomes the classification authority.
//!
//! Callers may project this contract into UI text or gate evidence, but may not
//! reconstruct a classification from the detail string.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Terminal disposition of a Preview request that produced no current output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewUnavailabilityDisposition {
    /// The current authoring state intentionally contains no publishable image.
    NoContent,
    /// A known contract or dependency prevents a semantically correct output.
    Blocked,
    /// An admitted production execution attempt failed.
    Failed,
}

/// Production stage that owns one Preview unavailability reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewOutputStage {
    /// No active Project/Sequence can be evaluated.
    Project,
    /// Display or surface intent cannot be resolved correctly.
    DisplayContract,
    /// Canonical Timeline evaluation or nested traversal cannot complete.
    TimelineEvaluation,
    /// An authored media dependency cannot be resolved to a canonical request.
    MediaResolution,
    /// Concrete decode failed or is retained in terminal failure memory.
    MediaDecode,
    /// A generated Timeline source could not be materialized correctly.
    GeneratedSource,
    /// Input color interpretation rejected the media.
    InputColor,
    /// A decoded payload cannot enter the requested working-frame path.
    InputAdaptation,
    /// Timeline effects or layer compositing is blocked or failed.
    TimelineComposite,
    /// Program Output identity or transform cannot be resolved or executed.
    ProgramOutput,
    /// Preview-only monitor adaptation cannot be resolved or executed.
    MonitorAdaptation,
    /// Final raster extent, payload, or resource packaging is invalid.
    FramePackaging,
    /// GPU layer lowering or compositing cannot execute.
    GpuComposite,
}

/// Typed terminal reason why Preview could not publish the current output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewUnavailability {
    disposition: PreviewUnavailabilityDisposition,
    stage: PreviewOutputStage,
    detail: String,
}

impl PreviewUnavailability {
    /// Construct an expected no-content result.
    pub fn no_content(stage: PreviewOutputStage, detail: impl Into<String>) -> Self {
        Self::new(PreviewUnavailabilityDisposition::NoContent, stage, detail)
    }

    /// Construct a fail-closed correctness or dependency blocker.
    pub fn blocked(stage: PreviewOutputStage, detail: impl Into<String>) -> Self {
        Self::new(PreviewUnavailabilityDisposition::Blocked, stage, detail)
    }

    /// Construct a failed production execution attempt.
    pub fn failed(stage: PreviewOutputStage, detail: impl Into<String>) -> Self {
        Self::new(PreviewUnavailabilityDisposition::Failed, stage, detail)
    }

    fn new(
        disposition: PreviewUnavailabilityDisposition,
        stage: PreviewOutputStage,
        detail: impl Into<String>,
    ) -> Self {
        let detail = detail.into();
        let detail = if detail.trim().is_empty() {
            "preview output is unavailable without additional diagnostic detail".to_owned()
        } else {
            detail
        };
        Self { disposition, stage, detail }
    }

    /// Return the terminal disposition.
    pub const fn disposition(&self) -> PreviewUnavailabilityDisposition {
        self.disposition
    }

    /// Return the owning production stage.
    pub const fn stage(&self) -> PreviewOutputStage {
        self.stage
    }

    /// Return non-authoritative human-readable diagnostic detail.
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// Add typed execution context without changing disposition or stage.
    pub fn with_context(&self, context: impl fmt::Display) -> Self {
        Self::new(
            self.disposition,
            self.stage,
            format!("{context}: {}", self.detail),
        )
    }

    /// Return the stable machine-readable classification code.
    pub const fn code(&self) -> &'static str {
        match (self.disposition, self.stage) {
            (PreviewUnavailabilityDisposition::NoContent, PreviewOutputStage::Project) => {
                "preview.no_content.project"
            }
            (
                PreviewUnavailabilityDisposition::NoContent,
                PreviewOutputStage::TimelineEvaluation,
            ) => "preview.no_content.timeline",
            (PreviewUnavailabilityDisposition::NoContent, _) => "preview.no_content.other",
            (PreviewUnavailabilityDisposition::Blocked, PreviewOutputStage::DisplayContract) => {
                "preview.blocked.display_contract"
            }
            (PreviewUnavailabilityDisposition::Blocked, PreviewOutputStage::TimelineEvaluation) => {
                "preview.blocked.timeline_evaluation"
            }
            (PreviewUnavailabilityDisposition::Blocked, PreviewOutputStage::MediaResolution) => {
                "preview.blocked.media_resolution"
            }
            (PreviewUnavailabilityDisposition::Blocked, PreviewOutputStage::MediaDecode) => {
                "preview.blocked.media_decode"
            }
            (PreviewUnavailabilityDisposition::Blocked, PreviewOutputStage::GeneratedSource) => {
                "preview.blocked.generated_source"
            }
            (PreviewUnavailabilityDisposition::Blocked, PreviewOutputStage::InputColor) => {
                "preview.blocked.input_color"
            }
            (PreviewUnavailabilityDisposition::Blocked, PreviewOutputStage::InputAdaptation) => {
                "preview.blocked.input_adaptation"
            }
            (PreviewUnavailabilityDisposition::Blocked, PreviewOutputStage::TimelineComposite) => {
                "preview.blocked.timeline_composite"
            }
            (PreviewUnavailabilityDisposition::Blocked, PreviewOutputStage::ProgramOutput) => {
                "preview.blocked.program_output"
            }
            (PreviewUnavailabilityDisposition::Blocked, PreviewOutputStage::MonitorAdaptation) => {
                "preview.blocked.monitor_adaptation"
            }
            (PreviewUnavailabilityDisposition::Blocked, PreviewOutputStage::GpuComposite) => {
                "preview.blocked.gpu_composite"
            }
            (PreviewUnavailabilityDisposition::Blocked, _) => "preview.blocked.other",
            (PreviewUnavailabilityDisposition::Failed, PreviewOutputStage::TimelineEvaluation) => {
                "preview.failed.timeline_evaluation"
            }
            (PreviewUnavailabilityDisposition::Failed, PreviewOutputStage::MediaResolution) => {
                "preview.failed.media_resolution"
            }
            (PreviewUnavailabilityDisposition::Failed, PreviewOutputStage::MediaDecode) => {
                "preview.failed.media_decode"
            }
            (PreviewUnavailabilityDisposition::Failed, PreviewOutputStage::GeneratedSource) => {
                "preview.failed.generated_source"
            }
            (PreviewUnavailabilityDisposition::Failed, PreviewOutputStage::InputAdaptation) => {
                "preview.failed.input_adaptation"
            }
            (PreviewUnavailabilityDisposition::Failed, PreviewOutputStage::TimelineComposite) => {
                "preview.failed.timeline_composite"
            }
            (PreviewUnavailabilityDisposition::Failed, PreviewOutputStage::ProgramOutput) => {
                "preview.failed.program_output"
            }
            (PreviewUnavailabilityDisposition::Failed, PreviewOutputStage::MonitorAdaptation) => {
                "preview.failed.monitor_adaptation"
            }
            (PreviewUnavailabilityDisposition::Failed, PreviewOutputStage::FramePackaging) => {
                "preview.failed.frame_packaging"
            }
            (PreviewUnavailabilityDisposition::Failed, PreviewOutputStage::GpuComposite) => {
                "preview.failed.gpu_composite"
            }
            (PreviewUnavailabilityDisposition::Failed, _) => "preview.failed.other",
        }
    }
}

impl fmt::Display for PreviewUnavailability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.detail)
    }
}

/// Per-stage observation counts for unavailable Preview production outputs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewOutputStageBreakdown {
    /// Project/active-Sequence observations.
    pub project: u64,
    /// Display-contract observations.
    pub display_contract: u64,
    /// Timeline-evaluation observations.
    pub timeline_evaluation: u64,
    /// Media-resolution observations.
    pub media_resolution: u64,
    /// Media-decode observations.
    pub media_decode: u64,
    /// Generated-source observations.
    pub generated_source: u64,
    /// Input-color observations.
    pub input_color: u64,
    /// Input-adaptation observations.
    pub input_adaptation: u64,
    /// Timeline-composite observations.
    pub timeline_composite: u64,
    /// Program Output observations.
    pub program_output: u64,
    /// Monitor-adaptation observations.
    pub monitor_adaptation: u64,
    /// Final frame-packaging observations.
    pub frame_packaging: u64,
    /// GPU-composite observations.
    pub gpu_composite: u64,
}

impl PreviewOutputStageBreakdown {
    fn observe(&mut self, stage: PreviewOutputStage) {
        let counter = match stage {
            PreviewOutputStage::Project => &mut self.project,
            PreviewOutputStage::DisplayContract => &mut self.display_contract,
            PreviewOutputStage::TimelineEvaluation => &mut self.timeline_evaluation,
            PreviewOutputStage::MediaResolution => &mut self.media_resolution,
            PreviewOutputStage::MediaDecode => &mut self.media_decode,
            PreviewOutputStage::GeneratedSource => &mut self.generated_source,
            PreviewOutputStage::InputColor => &mut self.input_color,
            PreviewOutputStage::InputAdaptation => &mut self.input_adaptation,
            PreviewOutputStage::TimelineComposite => &mut self.timeline_composite,
            PreviewOutputStage::ProgramOutput => &mut self.program_output,
            PreviewOutputStage::MonitorAdaptation => &mut self.monitor_adaptation,
            PreviewOutputStage::FramePackaging => &mut self.frame_packaging,
            PreviewOutputStage::GpuComposite => &mut self.gpu_composite,
        };
        *counter = counter.saturating_add(1);
    }
}

/// Bounded aggregate evidence for unavailable Preview production outputs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewUnavailabilityEvidenceSnapshot {
    /// Total unavailable production-output observations.
    pub observations: u64,
    /// Expected no-content observations.
    pub no_content: u64,
    /// Correctness/dependency blocker observations.
    pub blocked: u64,
    /// Failed production execution observations.
    pub failed: u64,
    /// Observation counts grouped by owning stage.
    pub stages: PreviewOutputStageBreakdown,
    /// Disposition of the most recent observation.
    pub last_disposition: Option<PreviewUnavailabilityDisposition>,
    /// Owning stage of the most recent observation.
    pub last_stage: Option<PreviewOutputStage>,
}

/// Mutable collector retained only by the production Preview Runtime.
#[derive(Debug, Clone, Default)]
pub(crate) struct PreviewUnavailabilityEvidence {
    snapshot: PreviewUnavailabilityEvidenceSnapshot,
    last: Option<PreviewUnavailability>,
}

impl PreviewUnavailabilityEvidence {
    pub(crate) fn observe(&mut self, reason: &PreviewUnavailability) {
        self.snapshot.observations = self.snapshot.observations.saturating_add(1);
        match reason.disposition {
            PreviewUnavailabilityDisposition::NoContent => {
                self.snapshot.no_content = self.snapshot.no_content.saturating_add(1);
            }
            PreviewUnavailabilityDisposition::Blocked => {
                self.snapshot.blocked = self.snapshot.blocked.saturating_add(1);
            }
            PreviewUnavailabilityDisposition::Failed => {
                self.snapshot.failed = self.snapshot.failed.saturating_add(1);
            }
        }
        self.snapshot.stages.observe(reason.stage);
        self.snapshot.last_disposition = Some(reason.disposition);
        self.snapshot.last_stage = Some(reason.stage);
        self.last = Some(reason.clone());
    }

    pub(crate) const fn snapshot(&self) -> PreviewUnavailabilityEvidenceSnapshot {
        self.snapshot
    }

    pub(crate) fn last(&self) -> Option<PreviewUnavailability> {
        self.last.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_keeps_disposition_stage_and_latest_detail_separate() {
        let mut evidence = PreviewUnavailabilityEvidence::default();
        evidence.observe(&PreviewUnavailability::no_content(
            PreviewOutputStage::TimelineEvaluation,
            "empty frame",
        ));
        evidence.observe(&PreviewUnavailability::failed(
            PreviewOutputStage::TimelineComposite,
            "effect execution failed",
        ));

        let snapshot = evidence.snapshot();
        assert_eq!(snapshot.observations, 2);
        assert_eq!(snapshot.no_content, 1);
        assert_eq!(snapshot.failed, 1);
        assert_eq!(snapshot.stages.timeline_evaluation, 1);
        assert_eq!(snapshot.stages.timeline_composite, 1);
        assert_eq!(
            snapshot.last_stage,
            Some(PreviewOutputStage::TimelineComposite)
        );
        assert_eq!(
            evidence.last().as_ref().map(PreviewUnavailability::code),
            Some("preview.failed.timeline_composite")
        );
    }
}
