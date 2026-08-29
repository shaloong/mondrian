use mondrian_renderer::{
    product_gpu_working_bytes_per_pixel, GpuWorkingFloat16ImplementationQualification,
    GpuWorkingFloatBlockers, GpuWorkingFloatDecisionReason, GpuWorkingFloatFormat,
    GpuWorkingFloatPerformanceEvidence, GpuWorkingFloatQualityEvidence,
    PRODUCT_GPU_WORKING_FLOAT_DECISION, PRODUCT_GPU_WORKING_FLOAT_POLICY,
};

const DEVICE: [u8; 32] = [3; 32];

fn quality() -> GpuWorkingFloatQualityEvidence {
    GpuWorkingFloatQualityEvidence {
        suite_revision: 7,
        corpus_fingerprint: [1; 32],
        report_fingerprint: [2; 32],
        device_fingerprint: DEVICE,
        compared_frames: 512,
        independently_verified: true,
        sealed_pass: true,
    }
}

fn performance() -> GpuWorkingFloatPerformanceEvidence {
    GpuWorkingFloatPerformanceEvidence {
        suite_revision: 7,
        report_fingerprint: [4; 32],
        device_fingerprint: DEVICE,
        measured_frames: 2_000,
        rgba16_p95_gpu_time_ns: 6_000_000,
        rgba32_p95_gpu_time_ns: 8_000_000,
        rgba16_peak_active_bytes: 64 * 1024 * 1024,
        rgba32_peak_active_bytes: 128 * 1024 * 1024,
        sealed_pass: true,
    }
}

fn qualified_implementations() -> GpuWorkingFloat16ImplementationQualification {
    GpuWorkingFloat16ImplementationQualification {
        compositor: true,
        effects: true,
        alpha_masks: true,
        heterogeneous_execution: true,
        viewer_export_parity: true,
    }
}

#[test]
fn production_path_is_auditable_float32_until_float16_is_qualified() {
    assert_eq!(
        PRODUCT_GPU_WORKING_FLOAT_DECISION.format(),
        GpuWorkingFloatFormat::Rgba32Float
    );
    assert_eq!(
        PRODUCT_GPU_WORKING_FLOAT_DECISION.reason(),
        GpuWorkingFloatDecisionReason::Float16QualityEvidenceMissing
    );
    assert_eq!(product_gpu_working_bytes_per_pixel(), 16);
    assert_eq!(
        PRODUCT_GPU_WORKING_FLOAT_DECISION.blockers(),
        GpuWorkingFloatBlockers {
            quality_evidence: true,
            performance_evidence: true,
            implementation_qualification: true,
        }
    );
}

#[test]
fn policy_fails_closed_at_quality_performance_and_implementation_gates() {
    let policy = PRODUCT_GPU_WORKING_FLOAT_POLICY;
    assert_eq!(
        policy.decide(None, None, Default::default()).reason(),
        GpuWorkingFloatDecisionReason::Float16QualityEvidenceMissing
    );

    let mut invalid_quality = quality();
    invalid_quality.independently_verified = false;
    assert_eq!(
        policy.decide(Some(invalid_quality), None, Default::default()).reason(),
        GpuWorkingFloatDecisionReason::Float16QualityEvidenceInvalid
    );
    assert_eq!(
        policy.decide(Some(quality()), None, Default::default()).reason(),
        GpuWorkingFloatDecisionReason::Float16PerformanceEvidenceMissing
    );

    let mut slower = performance();
    slower.rgba16_p95_gpu_time_ns = slower.rgba32_p95_gpu_time_ns;
    assert_eq!(
        policy.decide(Some(quality()), Some(slower), Default::default()).reason(),
        GpuWorkingFloatDecisionReason::Float16PerformanceEvidenceInvalid
    );
    assert_eq!(
        policy.decide(Some(quality()), Some(performance()), Default::default()).reason(),
        GpuWorkingFloatDecisionReason::Float16ImplementationUnqualified
    );
}

#[test]
fn only_complete_same_device_evidence_can_select_float16() {
    let decision = PRODUCT_GPU_WORKING_FLOAT_POLICY.decide(
        Some(quality()),
        Some(performance()),
        qualified_implementations(),
    );
    assert_eq!(decision.format(), GpuWorkingFloatFormat::Rgba16Float);
    assert_eq!(
        decision.reason(),
        GpuWorkingFloatDecisionReason::Float16Qualified
    );
    assert_eq!(decision.quality_report_fingerprint(), Some([2; 32]));
    assert_eq!(decision.performance_report_fingerprint(), Some([4; 32]));
    assert!(!decision.blockers().any());
}
