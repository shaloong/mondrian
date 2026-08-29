//! Product policy for the scene-linear GPU working representation.
//!
//! Project color identity is independent from an execution representation. This
//! Module owns the narrower runtime decision between RGBA16F and RGBA32F so
//! Viewer, Export, compositing, and resource admission cannot silently invent
//! different precision policies.

use serde::{Deserialize, Serialize};

use crate::GpuColorFrameTextureFormat;

/// Scene-linear floating-point representation used by GPU working textures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GpuWorkingFloatFormat {
    /// IEEE binary16 RGBA storage.
    Rgba16Float,
    /// IEEE binary32 RGBA storage.
    Rgba32Float,
}

impl GpuWorkingFloatFormat {
    /// Renderer texture format implementing this working representation.
    pub const fn texture_format(self) -> GpuColorFrameTextureFormat {
        match self {
            Self::Rgba16Float => GpuColorFrameTextureFormat::Rgba16Float,
            Self::Rgba32Float => GpuColorFrameTextureFormat::Rgba32Float,
        }
    }

    /// Logical bytes retained by one RGBA pixel.
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Rgba16Float => 8,
            Self::Rgba32Float => 16,
        }
    }
}

/// Product request evaluated by the working-float policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GpuWorkingFloatPreference {
    /// Preserve the Float32 reference path regardless of optional Float16 evidence.
    RequireFloat32,
    /// Select Float16 only when every required qualification fact is present.
    PreferFloat16WhenQualified,
}

/// Stable explanation for one working-float decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GpuWorkingFloatDecisionReason {
    /// Product configuration explicitly requires the Float32 reference path.
    Float32Required,
    /// No independent sealed Float16 quality report was supplied.
    Float16QualityEvidenceMissing,
    /// The supplied Float16 quality report is incomplete or failed.
    Float16QualityEvidenceInvalid,
    /// No sealed same-device Float16/Float32 performance comparison was supplied.
    Float16PerformanceEvidenceMissing,
    /// The performance comparison is incomplete or shows no material benefit.
    Float16PerformanceEvidenceInvalid,
    /// One or more production execution Implementations are not Float16-qualified.
    Float16ImplementationUnqualified,
    /// Float16 passed every quality, performance, and Implementation gate.
    Float16Qualified,
}

/// Complete Float16 blocker set retained even when one primary reason is shown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GpuWorkingFloatBlockers {
    /// Independent end-to-end quality evidence is absent or invalid.
    pub quality_evidence: bool,
    /// Same-device performance evidence is absent, invalid, or non-beneficial.
    pub performance_evidence: bool,
    /// At least one production execution Implementation is not qualified.
    pub implementation_qualification: bool,
}

impl GpuWorkingFloatBlockers {
    /// Whether any gate prevents Float16 selection.
    pub const fn any(self) -> bool {
        self.quality_evidence || self.performance_evidence || self.implementation_qualification
    }
}

/// Independent end-to-end image-quality evidence for an RGBA16F candidate.
///
/// The policy does not reinterpret the corpus tolerance. A sealed qualification
/// runner owns that domain-specific verdict and publishes exact fingerprints so
/// the admitted report remains auditable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GpuWorkingFloatQualityEvidence {
    /// Qualification-suite schema interpreted by the sealed runner.
    pub suite_revision: u32,
    /// Stable content corpus identity.
    pub corpus_fingerprint: [u8; 32],
    /// Stable identity of the published quality report.
    pub report_fingerprint: [u8; 32],
    /// Exact adapter/device identity used for the end-to-end run.
    pub device_fingerprint: [u8; 32],
    /// Frames compared against the Float32 reference path.
    pub compared_frames: u64,
    /// Whether the reference comparison was produced independently from the candidate path.
    pub independently_verified: bool,
    /// Whether the sealed suite passed its published RGB, alpha, HDR, and out-of-range budgets.
    pub sealed_pass: bool,
}

impl GpuWorkingFloatQualityEvidence {
    fn is_admissible(self) -> bool {
        self.suite_revision != 0
            && self.compared_frames != 0
            && self.independently_verified
            && self.sealed_pass
            && fingerprint_is_present(self.corpus_fingerprint)
            && fingerprint_is_present(self.report_fingerprint)
            && fingerprint_is_present(self.device_fingerprint)
    }
}

/// Same-device performance evidence for an RGBA16F candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GpuWorkingFloatPerformanceEvidence {
    /// Qualification-suite schema interpreted by the benchmark runner.
    pub suite_revision: u32,
    /// Stable identity of the published benchmark report.
    pub report_fingerprint: [u8; 32],
    /// Exact adapter/device identity shared with quality evidence.
    pub device_fingerprint: [u8; 32],
    /// Timed frames in each representation after warm-up.
    pub measured_frames: u64,
    /// RGBA16F p95 end-to-end GPU frame time in nanoseconds.
    pub rgba16_p95_gpu_time_ns: u64,
    /// RGBA32F p95 end-to-end GPU frame time in nanoseconds.
    pub rgba32_p95_gpu_time_ns: u64,
    /// Peak active bytes measured for the RGBA16F working path.
    pub rgba16_peak_active_bytes: u64,
    /// Peak active bytes measured for the RGBA32F working path.
    pub rgba32_peak_active_bytes: u64,
    /// Whether the sealed benchmark completed without missing stages or samples.
    pub sealed_pass: bool,
}

impl GpuWorkingFloatPerformanceEvidence {
    fn is_admissible(self) -> bool {
        self.suite_revision != 0
            && self.measured_frames != 0
            && self.rgba16_p95_gpu_time_ns != 0
            && self.rgba32_p95_gpu_time_ns != 0
            && self.rgba16_peak_active_bytes != 0
            && self.rgba32_peak_active_bytes != 0
            && self.sealed_pass
            && self.rgba16_p95_gpu_time_ns < self.rgba32_p95_gpu_time_ns
            && self.rgba16_peak_active_bytes < self.rgba32_peak_active_bytes
            && fingerprint_is_present(self.report_fingerprint)
            && fingerprint_is_present(self.device_fingerprint)
    }
}

/// Float16 qualification state of every production execution Implementation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GpuWorkingFloat16ImplementationQualification {
    /// Working compositor pipelines, blends, transitions, and uploads.
    pub compositor: bool,
    /// Effect graph pointwise and external-domain execution.
    pub effects: bool,
    /// Qualifiers, masks, matte mixing, and alpha preservation.
    pub alpha_masks: bool,
    /// CPU-prefix to GPU continuation and native-source preparation.
    pub heterogeneous_execution: bool,
    /// Viewer and Export produce the same qualified working representation.
    pub viewer_export_parity: bool,
}

impl GpuWorkingFloat16ImplementationQualification {
    fn is_complete(self) -> bool {
        self.compositor
            && self.effects
            && self.alpha_masks
            && self.heterogeneous_execution
            && self.viewer_export_parity
    }
}

/// Immutable, auditable result of applying the working-float policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GpuWorkingFloatDecision {
    format: GpuWorkingFloatFormat,
    reason: GpuWorkingFloatDecisionReason,
    blockers: GpuWorkingFloatBlockers,
    quality_report_fingerprint: Option<[u8; 32]>,
    performance_report_fingerprint: Option<[u8; 32]>,
}

impl GpuWorkingFloatDecision {
    const fn float32(
        reason: GpuWorkingFloatDecisionReason,
        blockers: GpuWorkingFloatBlockers,
    ) -> Self {
        Self {
            format: GpuWorkingFloatFormat::Rgba32Float,
            reason,
            blockers,
            quality_report_fingerprint: None,
            performance_report_fingerprint: None,
        }
    }

    /// Selected working representation.
    pub const fn format(self) -> GpuWorkingFloatFormat {
        self.format
    }

    /// Stable selection reason.
    pub const fn reason(self) -> GpuWorkingFloatDecisionReason {
        self.reason
    }

    /// Complete blocker set for the rejected Float16 candidate.
    pub const fn blockers(self) -> GpuWorkingFloatBlockers {
        self.blockers
    }

    /// Quality report admitted for a Float16 selection, when present.
    pub const fn quality_report_fingerprint(self) -> Option<[u8; 32]> {
        self.quality_report_fingerprint
    }

    /// Performance report admitted for a Float16 selection, when present.
    pub const fn performance_report_fingerprint(self) -> Option<[u8; 32]> {
        self.performance_report_fingerprint
    }
}

/// Single product authority for choosing a GPU scene-linear working representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GpuWorkingFloatPolicy {
    preference: GpuWorkingFloatPreference,
}

impl GpuWorkingFloatPolicy {
    /// Create a policy with an explicit product preference.
    pub const fn new(preference: GpuWorkingFloatPreference) -> Self {
        Self { preference }
    }

    /// Evaluate optional Float16 evidence and fail closed to Float32.
    pub fn decide(
        self,
        quality: Option<GpuWorkingFloatQualityEvidence>,
        performance: Option<GpuWorkingFloatPerformanceEvidence>,
        implementations: GpuWorkingFloat16ImplementationQualification,
    ) -> GpuWorkingFloatDecision {
        if self.preference == GpuWorkingFloatPreference::RequireFloat32 {
            return GpuWorkingFloatDecision::float32(
                GpuWorkingFloatDecisionReason::Float32Required,
                GpuWorkingFloatBlockers::default(),
            );
        }
        let quality_admissible = quality.is_some_and(|evidence| evidence.is_admissible());
        let performance_admissible = quality.is_some_and(|quality| {
            performance.is_some_and(|performance| {
                performance.is_admissible()
                    && performance.device_fingerprint == quality.device_fingerprint
                    && performance.suite_revision == quality.suite_revision
            })
        });
        let implementation_admissible = implementations.is_complete();
        let blockers = GpuWorkingFloatBlockers {
            quality_evidence: !quality_admissible,
            performance_evidence: !performance_admissible,
            implementation_qualification: !implementation_admissible,
        };
        let Some(quality) = quality else {
            return GpuWorkingFloatDecision::float32(
                GpuWorkingFloatDecisionReason::Float16QualityEvidenceMissing,
                blockers,
            );
        };
        if !quality_admissible {
            return GpuWorkingFloatDecision::float32(
                GpuWorkingFloatDecisionReason::Float16QualityEvidenceInvalid,
                blockers,
            );
        }
        let Some(performance) = performance else {
            return GpuWorkingFloatDecision::float32(
                GpuWorkingFloatDecisionReason::Float16PerformanceEvidenceMissing,
                blockers,
            );
        };
        if !performance_admissible {
            return GpuWorkingFloatDecision::float32(
                GpuWorkingFloatDecisionReason::Float16PerformanceEvidenceInvalid,
                blockers,
            );
        }
        if !implementation_admissible {
            return GpuWorkingFloatDecision::float32(
                GpuWorkingFloatDecisionReason::Float16ImplementationUnqualified,
                blockers,
            );
        }
        GpuWorkingFloatDecision {
            format: GpuWorkingFloatFormat::Rgba16Float,
            reason: GpuWorkingFloatDecisionReason::Float16Qualified,
            blockers,
            quality_report_fingerprint: Some(quality.report_fingerprint),
            performance_report_fingerprint: Some(performance.report_fingerprint),
        }
    }
}

/// Current product policy: prefer Float16 only after sealed qualification.
pub const PRODUCT_GPU_WORKING_FLOAT_POLICY: GpuWorkingFloatPolicy =
    GpuWorkingFloatPolicy::new(GpuWorkingFloatPreference::PreferFloat16WhenQualified);

/// Current product decision. Float16 has no sealed quality/performance bundle
/// and not every production Implementation is qualified, so this is RGBA32F.
pub const PRODUCT_GPU_WORKING_FLOAT_DECISION: GpuWorkingFloatDecision =
    GpuWorkingFloatDecision::float32(
        GpuWorkingFloatDecisionReason::Float16QualityEvidenceMissing,
        GpuWorkingFloatBlockers {
            quality_evidence: true,
            performance_evidence: true,
            implementation_qualification: true,
        },
    );

/// Return the selected production working texture format.
pub const fn product_gpu_working_texture_format() -> GpuColorFrameTextureFormat {
    PRODUCT_GPU_WORKING_FLOAT_DECISION.format().texture_format()
}

/// Return logical bytes per pixel for the selected production working format.
pub const fn product_gpu_working_bytes_per_pixel() -> u32 {
    PRODUCT_GPU_WORKING_FLOAT_DECISION.format().bytes_per_pixel()
}

const fn fingerprint_is_present(fingerprint: [u8; 32]) -> bool {
    let mut index = 0;
    while index < fingerprint.len() {
        if fingerprint[index] != 0 {
            return true;
        }
        index += 1;
    }
    false
}
