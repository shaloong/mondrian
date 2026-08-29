//! Versioned qualification contract for the realtime Viewer GPU matrix.
//!
//! The hardware producer lives in an ignored integration test, but it submits
//! the production [`crate::ViewerGpuExecutionRuntime`] and hands its raw
//! observations to this module. This keeps workload identity and pass/fail
//! policy out of benchmark scripts and prevents a renamed or weakened smoke
//! test from becoming qualification evidence.

use mondrian_core::ColorSpace;
use serde::{Deserialize, Serialize};

use crate::profile::GpuTimestampStageDurations;

/// Environment binding selecting realtime performance execution policy.
pub const REALTIME_PERFORMANCE_EXECUTION_POLICY_ENV: &str =
    "MONDRIAN_REALTIME_PERFORMANCE_EXECUTION_POLICY";
/// Exact policy value required by the sealed matrix supervisor.
pub const SEALED_REALTIME_PERFORMANCE_EXECUTION_POLICY: &str = "sealed-required";
/// Schema for [`RealtimeVisualPerformanceReport`].
pub const REALTIME_VISUAL_PERFORMANCE_REPORT_SCHEMA_VERSION: u32 = 1;
/// Stable profile interpreted by the coordinated matrix supervisor.
pub const REALTIME_VISUAL_PERFORMANCE_PROFILE: &str = "realtime_visual_gpu_matrix_v1";

/// Whether missing hardware may be treated as a developer-only skip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealtimePerformanceExecutionPolicy {
    /// Local development may omit a hardware-only sample.
    DevelopmentOptional,
    /// Qualification must fail when any capability or sample is unavailable.
    SealedRequired,
}

impl RealtimePerformanceExecutionPolicy {
    /// Resolve the exact process policy, rejecting unknown values.
    pub fn from_environment() -> Result<Self, RealtimePerformancePolicyError> {
        match std::env::var(REALTIME_PERFORMANCE_EXECUTION_POLICY_ENV) {
            Ok(value) => Self::from_value(Some(&value)),
            Err(std::env::VarError::NotPresent) => Self::from_value(None),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(RealtimePerformancePolicyError::NonUnicodePolicy)
            }
        }
    }

    /// Parse an explicit policy value.
    pub fn from_value(value: Option<&str>) -> Result<Self, RealtimePerformancePolicyError> {
        match value {
            None | Some("development-optional") => Ok(Self::DevelopmentOptional),
            Some(SEALED_REALTIME_PERFORMANCE_EXECUTION_POLICY) => Ok(Self::SealedRequired),
            Some(value) => {
                Err(RealtimePerformancePolicyError::InvalidPolicy { value: value.to_owned() })
            }
        }
    }

    /// Whether unavailable hardware is a terminal gate failure.
    pub const fn hardware_required(self) -> bool {
        matches!(self, Self::SealedRequired)
    }
}

/// Invalid realtime performance execution policy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RealtimePerformancePolicyError {
    /// The process environment could not be represented as Unicode.
    #[error("realtime performance execution policy is not valid Unicode")]
    NonUnicodePolicy,
    /// Only the two closed policy values are recognized.
    #[error("unsupported realtime performance execution policy: {value}")]
    InvalidPolicy {
        /// Rejected environment value.
        value: String,
    },
}

/// Stable identity of one required visual workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeVisualScenarioId {
    /// Four-layer 3840x2160 at 60 Hz with effects, HDR output and scopes.
    Uhd4k60HdrMultilayerEffectsScopes,
    /// Two-layer 7680x4320 at 30 Hz with effects, HDR output and scopes.
    EightK30HdrMultilayerEffectsScopes,
}

impl RealtimeVisualScenarioId {
    /// Every scenario required by the sealed profile, in report order.
    pub const ALL: [Self; 2] = [
        Self::Uhd4k60HdrMultilayerEffectsScopes,
        Self::EightK30HdrMultilayerEffectsScopes,
    ];

    /// Return the immutable workload and budget contract.
    pub const fn contract(self) -> RealtimeVisualWorkload {
        match self {
            Self::Uhd4k60HdrMultilayerEffectsScopes => RealtimeVisualWorkload {
                scenario: self,
                width: 3_840,
                height: 2_160,
                frame_rate_numerator: 60,
                frame_rate_denominator: 1,
                layer_count: 4,
                point_effects_per_layer: 2,
                output_color_space: ColorSpace::Rec2100Pq,
                program_scopes_required: true,
                warmup_frames: 4,
                measured_frames: 60,
                gpu_p95_budget_us: 16_000,
                cpu_record_p95_budget_us: 2_000,
                resource_grant: crate::ViewerGpuExecutionResourceGrant::professional_realtime(),
            },
            Self::EightK30HdrMultilayerEffectsScopes => RealtimeVisualWorkload {
                scenario: self,
                width: 7_680,
                height: 4_320,
                frame_rate_numerator: 30,
                frame_rate_denominator: 1,
                layer_count: 2,
                point_effects_per_layer: 2,
                output_color_space: ColorSpace::Rec2100Pq,
                program_scopes_required: true,
                warmup_frames: 2,
                measured_frames: 30,
                gpu_p95_budget_us: 32_000,
                cpu_record_p95_budget_us: 2_000,
                resource_grant: crate::ViewerGpuExecutionResourceGrant::professional_realtime(),
            },
        }
    }
}

/// Exact visual workload identity and fixed qualification budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeVisualWorkload {
    /// Scenario identity.
    pub scenario: RealtimeVisualScenarioId,
    /// Working and output width.
    pub width: u32,
    /// Working and output height.
    pub height: u32,
    /// Exact cadence numerator.
    pub frame_rate_numerator: u32,
    /// Exact cadence denominator.
    pub frame_rate_denominator: u32,
    /// Bottom-to-top contributing source layers.
    pub layer_count: u32,
    /// Point operations authored on every layer and expected to fuse.
    pub point_effects_per_layer: u32,
    /// Program and monitor output identity.
    pub output_color_space: ColorSpace,
    /// Whether demand-driven Program Output scopes must execute every frame.
    pub program_scopes_required: bool,
    /// Frames excluded from percentile samples while retained resources warm.
    pub warmup_frames: u32,
    /// Required hardware timestamp sample count.
    pub measured_frames: u32,
    /// Nearest-rank p95 hardware GPU deadline.
    pub gpu_p95_budget_us: u64,
    /// Nearest-rank p95 CPU command-recording deadline.
    pub cpu_record_p95_budget_us: u64,
    /// Exact demand-driven texture grant shared with the product's
    /// professional Viewer resource class.
    pub resource_grant: crate::ViewerGpuExecutionResourceGrant,
}

/// Immutable GPU/driver identity attached to every visual report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeVisualAdapterIdentity {
    /// Adapter description reported by wgpu.
    pub name: String,
    /// Numeric PCI/vendor identity when available.
    pub vendor: u32,
    /// Numeric device identity when available.
    pub device: u32,
    /// Backend name used for this execution.
    pub backend: String,
    /// Driver name.
    pub driver: String,
    /// Driver information/version.
    pub driver_info: String,
    /// Whether the device enabled complete encoder timestamp support.
    pub timestamp_queries: bool,
}

/// Warm-path allocation and cache deltas over measured frames only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeVisualWarmPathEvidence {
    /// Exact-contract texture pool hits.
    pub texture_pool_hits: u64,
    /// New texture allocations after warmup.
    pub texture_pool_misses: u64,
    /// Pool evictions after warmup.
    pub texture_pool_evictions: u64,
    /// OCIO shader extractions after warmup.
    pub shader_extractions: u64,
    /// Static color-pipeline preparations after warmup.
    pub static_pipeline_preparations: u64,
    /// Concrete color backend object preparations after warmup.
    pub backend_object_preparations: u64,
    /// Scope pipeline creations after warmup.
    pub scope_pipeline_creations: u64,
    /// Scope buffer allocations after warmup.
    pub scope_buffer_allocations: u64,
    /// Scope display texture allocations after warmup.
    pub scope_texture_allocations: u64,
}

/// Runtime execution counters over measured frames only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeVisualFrameEvidence {
    /// Frames successfully recorded and submitted.
    pub submitted_frames: u64,
    /// Frames that crossed the move-only presentation output seam.
    pub presentation_output_leases: u64,
    /// Frames producing GPU scope products.
    pub program_scopes_frames: u64,
    /// Native working-linear composite operations.
    pub gpu_native_composites: u64,
    /// Point operations actually fused into layer passes.
    pub fused_point_operations: u64,
    /// Composite work that fell back to CPU.
    pub cpu_fallback_composites: u64,
    /// GPU color stages executed by the production Viewer path.
    pub gpu_color_stages: u64,
    /// CPU-to-GPU color stages in the measured visual route.
    pub upload_stages: u64,
    /// GPU-to-CPU color stages in the measured visual route.
    pub readback_stages: u64,
    /// Structured GPU blockers reported by the color planner.
    pub gpu_blockers: u64,
    /// Outputs whose carrier did not match HDR Float16 policy.
    pub output_format_mismatches: u64,
    /// Samples discarded because the bounded timestamp ring was full.
    pub discarded_timestamp_samples: u64,
}

/// Raw observation produced by the real-device integration gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealtimeVisualPerformanceObservation {
    /// Claimed workload; evaluator requires exact equality with the profile.
    pub workload: RealtimeVisualWorkload,
    /// Physical adapter and timestamp capability.
    pub adapter: RealtimeVisualAdapterIdentity,
    /// Complete-frame hardware timestamp samples.
    pub gpu_samples_us: Vec<u64>,
    /// CPU command-recording samples paired one-to-one with GPU samples.
    pub cpu_record_samples_us: Vec<u64>,
    /// Ordered stage samples paired one-to-one with GPU samples.
    pub gpu_stage_samples: Vec<GpuTimestampStageDurations>,
    /// Execution evidence from measured frames.
    pub frames: RealtimeVisualFrameEvidence,
    /// Resource/cache deltas after warmup.
    pub warm_path: RealtimeVisualWarmPathEvidence,
}

/// Nearest-rank p50/p95/p99 sample summary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeVisualQuantiles {
    /// Number of samples represented.
    pub samples: u32,
    /// Nearest-rank median.
    pub p50_us: u64,
    /// Nearest-rank 95th percentile.
    pub p95_us: u64,
    /// Nearest-rank 99th percentile.
    pub p99_us: u64,
    /// Maximum sample.
    pub max_us: u64,
}

/// Quantiles for each production Viewer GPU stage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeVisualStageQuantiles {
    /// Working composite, including fused per-layer point effects.
    pub working_composite: RealtimeVisualQuantiles,
    /// Viewer crop/resize stage.
    pub spatial: RealtimeVisualQuantiles,
    /// Program Output color transform.
    pub program_output_boundary: RealtimeVisualQuantiles,
    /// Preview-only Program-to-monitor adaptation.
    pub monitor_adaptation: RealtimeVisualQuantiles,
    /// Demand-driven Program Output scopes.
    pub program_scopes: RealtimeVisualQuantiles,
    /// False color/zebra/gamut monitoring stage, disabled in this profile.
    pub signal_monitoring: RealtimeVisualQuantiles,
    /// Display calibration stage, disabled in this profile.
    pub display_calibration: RealtimeVisualQuantiles,
}

/// One stable pass/fail fact in a visual performance report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeVisualPerformanceCheck {
    /// Stable tooling key.
    pub code: &'static str,
    /// Observed numeric value.
    pub observed: u64,
    /// Exact required value or upper/lower bound.
    pub limit: u64,
    /// Comparison represented by this check.
    pub relation: RealtimeVisualCheckRelation,
    /// Whether the comparison passed.
    pub passed: bool,
}

/// Closed comparison vocabulary for matrix checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeVisualCheckRelation {
    /// `observed == limit`.
    Equal,
    /// `observed >= limit`.
    AtLeast,
    /// `observed <= limit`.
    AtMost,
}

/// Overall visual performance verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeVisualPerformanceVerdict {
    /// Every fixed workload, evidence and timing check passed.
    Pass,
    /// At least one required fact failed or was absent.
    Fail,
}

/// Serializable, fail-closed result consumed by the coordinated supervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeVisualPerformanceReport {
    /// Report schema.
    pub schema_version: u32,
    /// Qualification profile identity.
    pub profile: &'static str,
    /// Scenario report identity.
    pub scenario: RealtimeVisualScenarioId,
    /// Exact workload admitted by the evaluator.
    pub workload: RealtimeVisualWorkload,
    /// Physical execution identity.
    pub adapter: RealtimeVisualAdapterIdentity,
    /// Complete-frame hardware GPU timing.
    pub gpu_time: RealtimeVisualQuantiles,
    /// CPU record timing, excluding final offline timestamp mapping.
    pub cpu_record_time: RealtimeVisualQuantiles,
    /// Hardware GPU stage attribution.
    pub gpu_stages: RealtimeVisualStageQuantiles,
    /// Production route evidence.
    pub frames: RealtimeVisualFrameEvidence,
    /// Warm-path cache/allocation evidence.
    pub warm_path: RealtimeVisualWarmPathEvidence,
    /// Complete fixed check set.
    pub checks: Vec<RealtimeVisualPerformanceCheck>,
    /// Stable failed-check keys.
    pub root_causes: Vec<&'static str>,
    /// Operator/developer actions derived from failed checks.
    pub actions: Vec<&'static str>,
    /// Overall result.
    pub verdict: RealtimeVisualPerformanceVerdict,
    /// Compatibility convenience equal to `verdict == pass`.
    pub passed: bool,
}

/// Evaluate one real-device observation against its immutable profile.
pub fn evaluate_realtime_visual_performance(
    observation: RealtimeVisualPerformanceObservation,
) -> RealtimeVisualPerformanceReport {
    let expected = observation.workload.scenario.contract();
    let expected_samples = u64::from(expected.measured_frames);
    let expected_composites = expected_samples;
    let expected_layer_executions =
        expected_samples.saturating_mul(u64::from(expected.layer_count));
    let expected_effects =
        expected_layer_executions.saturating_mul(u64::from(expected.point_effects_per_layer));
    let mut checks = vec![
        check_equal(
            "realtime_visual_workload_identity",
            u64::from(observation.workload == expected),
            1,
        ),
        check_equal(
            "realtime_visual_timestamp_capability",
            u64::from(observation.adapter.timestamp_queries),
            1,
        ),
        check_equal(
            "realtime_visual_gpu_sample_coverage",
            observation.gpu_samples_us.len() as u64,
            expected_samples,
        ),
        check_equal(
            "realtime_visual_cpu_sample_coverage",
            observation.cpu_record_samples_us.len() as u64,
            expected_samples,
        ),
        check_equal(
            "realtime_visual_stage_sample_coverage",
            observation.gpu_stage_samples.len() as u64,
            expected_samples,
        ),
        check_equal(
            "realtime_visual_submitted_frames",
            observation.frames.submitted_frames,
            expected_samples,
        ),
        check_equal(
            "realtime_visual_presentation_output_coverage",
            observation.frames.presentation_output_leases,
            expected_samples,
        ),
        check_equal(
            "realtime_visual_program_scopes_coverage",
            observation.frames.program_scopes_frames,
            expected_samples,
        ),
        check_at_least(
            "realtime_visual_gpu_native_composites",
            observation.frames.gpu_native_composites,
            expected_composites,
        ),
        check_at_least(
            "realtime_visual_fused_point_effects",
            observation.frames.fused_point_operations,
            expected_effects,
        ),
        check_equal(
            "realtime_visual_cpu_fallback_composites",
            observation.frames.cpu_fallback_composites,
            0,
        ),
        check_at_least(
            "realtime_visual_gpu_color_execution",
            observation.frames.gpu_color_stages,
            expected_samples,
        ),
        check_equal(
            "realtime_visual_upload_stages",
            observation.frames.upload_stages,
            0,
        ),
        check_equal(
            "realtime_visual_readback_stages",
            observation.frames.readback_stages,
            0,
        ),
        check_equal(
            "realtime_visual_gpu_blockers",
            observation.frames.gpu_blockers,
            0,
        ),
        check_equal(
            "realtime_visual_hdr_output_format",
            observation.frames.output_format_mismatches,
            0,
        ),
        check_equal(
            "realtime_visual_timestamp_ring_drops",
            observation.frames.discarded_timestamp_samples,
            0,
        ),
        check_at_least(
            "realtime_visual_texture_pool_hits",
            observation.warm_path.texture_pool_hits,
            expected_samples,
        ),
        check_equal(
            "realtime_visual_texture_allocations_after_warmup",
            observation.warm_path.texture_pool_misses,
            0,
        ),
        check_equal(
            "realtime_visual_texture_evictions_after_warmup",
            observation.warm_path.texture_pool_evictions,
            0,
        ),
        check_equal(
            "realtime_visual_shader_extractions_after_warmup",
            observation.warm_path.shader_extractions,
            0,
        ),
        check_equal(
            "realtime_visual_static_pipeline_preparations_after_warmup",
            observation.warm_path.static_pipeline_preparations,
            0,
        ),
        check_equal(
            "realtime_visual_backend_object_preparations_after_warmup",
            observation.warm_path.backend_object_preparations,
            0,
        ),
        check_equal(
            "realtime_visual_scope_pipeline_creations_after_warmup",
            observation.warm_path.scope_pipeline_creations,
            0,
        ),
        check_equal(
            "realtime_visual_scope_buffer_allocations_after_warmup",
            observation.warm_path.scope_buffer_allocations,
            0,
        ),
        check_equal(
            "realtime_visual_scope_texture_allocations_after_warmup",
            observation.warm_path.scope_texture_allocations,
            0,
        ),
    ];

    let gpu_time = quantiles(&observation.gpu_samples_us);
    let cpu_record_time = quantiles(&observation.cpu_record_samples_us);
    checks.push(check_at_most(
        "realtime_visual_gpu_p95_deadline",
        gpu_time.p95_us,
        expected.gpu_p95_budget_us,
    ));
    checks.push(check_at_most(
        "realtime_visual_cpu_record_p95_deadline",
        cpu_record_time.p95_us,
        expected.cpu_record_p95_budget_us,
    ));

    let root_causes: Vec<_> =
        checks.iter().filter(|check| !check.passed).map(|check| check.code).collect();
    let actions = root_causes.iter().map(|code| action_for_check(code)).collect::<Vec<_>>();
    let passed = root_causes.is_empty();

    RealtimeVisualPerformanceReport {
        schema_version: REALTIME_VISUAL_PERFORMANCE_REPORT_SCHEMA_VERSION,
        profile: REALTIME_VISUAL_PERFORMANCE_PROFILE,
        scenario: observation.workload.scenario,
        workload: observation.workload,
        adapter: observation.adapter,
        gpu_time,
        cpu_record_time,
        gpu_stages: stage_quantiles(&observation.gpu_stage_samples),
        frames: observation.frames,
        warm_path: observation.warm_path,
        checks,
        root_causes,
        actions,
        verdict: if passed {
            RealtimeVisualPerformanceVerdict::Pass
        } else {
            RealtimeVisualPerformanceVerdict::Fail
        },
        passed,
    }
}

fn check_equal(code: &'static str, observed: u64, limit: u64) -> RealtimeVisualPerformanceCheck {
    RealtimeVisualPerformanceCheck {
        code,
        observed,
        limit,
        relation: RealtimeVisualCheckRelation::Equal,
        passed: observed == limit,
    }
}

fn check_at_least(code: &'static str, observed: u64, limit: u64) -> RealtimeVisualPerformanceCheck {
    RealtimeVisualPerformanceCheck {
        code,
        observed,
        limit,
        relation: RealtimeVisualCheckRelation::AtLeast,
        passed: observed >= limit,
    }
}

fn check_at_most(code: &'static str, observed: u64, limit: u64) -> RealtimeVisualPerformanceCheck {
    RealtimeVisualPerformanceCheck {
        code,
        observed,
        limit,
        relation: RealtimeVisualCheckRelation::AtMost,
        passed: observed <= limit,
    }
}

fn quantiles(samples: &[u64]) -> RealtimeVisualQuantiles {
    if samples.is_empty() {
        return RealtimeVisualQuantiles::default();
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let nearest = |percentile: usize| {
        let rank = percentile
            .saturating_mul(sorted.len())
            .saturating_add(99)
            .checked_div(100)
            .unwrap_or(sorted.len())
            .clamp(1, sorted.len());
        sorted[rank - 1]
    };
    RealtimeVisualQuantiles {
        samples: u32::try_from(sorted.len()).unwrap_or(u32::MAX),
        p50_us: nearest(50),
        p95_us: nearest(95),
        p99_us: nearest(99),
        max_us: sorted.last().copied().unwrap_or_default(),
    }
}

fn stage_quantiles(samples: &[GpuTimestampStageDurations]) -> RealtimeVisualStageQuantiles {
    let field = |read: fn(&GpuTimestampStageDurations) -> u64| {
        quantiles(&samples.iter().map(read).collect::<Vec<_>>())
    };
    RealtimeVisualStageQuantiles {
        working_composite: field(|sample| sample.through_working_composite_us),
        spatial: field(|sample| sample.spatial_us),
        program_output_boundary: field(|sample| sample.program_output_boundary_us),
        monitor_adaptation: field(|sample| sample.monitor_adaptation_us),
        program_scopes: field(|sample| sample.program_scopes_us),
        signal_monitoring: field(|sample| sample.signal_monitoring_us),
        display_calibration: field(|sample| sample.display_calibration_us),
    }
}

fn action_for_check(code: &str) -> &'static str {
    if code.contains("sample") || code.contains("timestamp") {
        "repeat on a timestamp-capable qualified adapter and inspect query-ring coverage"
    } else if code.contains("allocation")
        || code.contains("pool")
        || code.contains("pipeline")
        || code.contains("shader")
        || code.contains("backend_object")
    {
        "inspect warm-path resource identity and retained runtime cache invalidation"
    } else if code.contains("p95") {
        "inspect per-stage GPU and CPU attribution before changing the fixed workload budget"
    } else if code.contains("effect") || code.contains("composite") {
        "inspect prepared effect fusion and GPU composite execution diagnostics"
    } else {
        "inspect the production Viewer route and preserve fail-closed evidence"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passing_observation(
        scenario: RealtimeVisualScenarioId,
    ) -> RealtimeVisualPerformanceObservation {
        let workload = scenario.contract();
        let samples = workload.measured_frames as usize;
        let frames = u64::from(workload.measured_frames);
        let composites = frames.saturating_mul(u64::from(workload.layer_count));
        RealtimeVisualPerformanceObservation {
            workload,
            adapter: RealtimeVisualAdapterIdentity {
                name: "qualified-test-adapter".to_owned(),
                vendor: 1,
                device: 2,
                backend: "Vulkan".to_owned(),
                driver: "test".to_owned(),
                driver_info: "1".to_owned(),
                timestamp_queries: true,
            },
            gpu_samples_us: vec![workload.gpu_p95_budget_us.saturating_sub(1); samples],
            cpu_record_samples_us: vec![
                workload.cpu_record_p95_budget_us.saturating_sub(1);
                samples
            ],
            gpu_stage_samples: vec![
                GpuTimestampStageDurations {
                    through_working_composite_us: 100,
                    spatial_us: 20,
                    program_output_boundary_us: 30,
                    monitor_adaptation_us: 10,
                    program_scopes_us: 40,
                    signal_monitoring_us: 0,
                    display_calibration_us: 0,
                };
                samples
            ],
            frames: RealtimeVisualFrameEvidence {
                submitted_frames: frames,
                presentation_output_leases: frames,
                program_scopes_frames: frames,
                gpu_native_composites: composites,
                fused_point_operations: composites
                    .saturating_mul(u64::from(workload.point_effects_per_layer)),
                gpu_color_stages: frames,
                ..RealtimeVisualFrameEvidence::default()
            },
            warm_path: RealtimeVisualWarmPathEvidence {
                texture_pool_hits: frames,
                ..RealtimeVisualWarmPathEvidence::default()
            },
        }
    }

    #[test]
    fn sealed_visual_matrix_is_fixed_and_complete() {
        let contracts = RealtimeVisualScenarioId::ALL.map(RealtimeVisualScenarioId::contract);
        assert_eq!(contracts[0].width, 3_840);
        assert_eq!(contracts[0].height, 2_160);
        assert_eq!(contracts[0].frame_rate_numerator, 60);
        assert_eq!(contracts[1].width, 7_680);
        assert_eq!(contracts[1].height, 4_320);
        assert!(contracts.iter().all(|contract| {
            contract.layer_count >= 2
                && contract.point_effects_per_layer >= 2
                && contract.output_color_space.is_hdr()
                && contract.program_scopes_required
                && contract.measured_frames >= contract.frame_rate_numerator
        }));
    }

    #[test]
    fn evaluator_passes_complete_observations_for_both_scenarios() {
        for scenario in RealtimeVisualScenarioId::ALL {
            let report = evaluate_realtime_visual_performance(passing_observation(scenario));
            assert!(report.passed, "{:?}", report.root_causes);
            assert_eq!(report.verdict, RealtimeVisualPerformanceVerdict::Pass);
            assert_eq!(report.gpu_time.samples, scenario.contract().measured_frames);
            assert_eq!(
                report.gpu_stages.program_scopes.samples,
                scenario.contract().measured_frames
            );
        }
    }

    #[test]
    fn evaluator_rejects_weakened_workload_missing_execution_and_warm_allocations() {
        let scenario = RealtimeVisualScenarioId::Uhd4k60HdrMultilayerEffectsScopes;
        let mut observation = passing_observation(scenario);
        observation.workload.layer_count = 1;
        observation.frames.presentation_output_leases -= 1;
        observation.frames.fused_point_operations = 0;
        observation.frames.readback_stages = 1;
        observation.warm_path.texture_pool_misses = 1;
        for sample in observation.gpu_samples_us.iter_mut().skip(56) {
            *sample = scenario.contract().gpu_p95_budget_us + 1;
        }

        let report = evaluate_realtime_visual_performance(observation);
        assert!(!report.passed);
        for code in [
            "realtime_visual_workload_identity",
            "realtime_visual_presentation_output_coverage",
            "realtime_visual_fused_point_effects",
            "realtime_visual_readback_stages",
            "realtime_visual_texture_allocations_after_warmup",
            "realtime_visual_gpu_p95_deadline",
        ] {
            assert!(
                report.root_causes.contains(&code),
                "missing {code}: {:?}",
                report.root_causes
            );
        }
    }

    #[test]
    fn execution_policy_is_closed_and_defaults_to_development_optional() {
        assert_eq!(
            RealtimePerformanceExecutionPolicy::from_value(None),
            Ok(RealtimePerformanceExecutionPolicy::DevelopmentOptional)
        );
        assert_eq!(
            RealtimePerformanceExecutionPolicy::from_value(Some("sealed-required")),
            Ok(RealtimePerformanceExecutionPolicy::SealedRequired)
        );
        assert!(RealtimePerformanceExecutionPolicy::from_value(Some("sealed")).is_err());
    }
}
