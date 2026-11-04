//! Whole-frame cost model for choosing between a GPU spatial chain and the CPU.
//!
//! The driver ([`super::gpu_spatial_chain_plan`]) only answers *can* a chain run
//! on the GPU. This module answers *is it worth it*. The repository's fallback
//! granularity is the whole frame, so exactly two candidates exist for one
//! layer of one frame:
//!
//! * run the lowered chain on the layer's own GPU raster and composite on the
//!   GPU, or
//! * run the layer's effect graph on the CPU, composite on the CPU, and upload
//!   the finished frame once.
//!
//! The two candidates differ only in where that layer's raster is processed, so
//! the model is narrow and explainable: every term is either a per-pixel rate on
//! the layer raster, a per-pass fixed cost, or the one full-frame upload the CPU
//! candidate can never avoid.
//!
//! Units are microseconds in the estimate functions and nanoseconds in the
//! constants, so a rate table can be read straight off a timing harness.
//!
//! All constants live here. [`choose_spatial_chain`] is pure: it never touches a
//! device, never records a command, and never mutates process state, so the same
//! plan, extent, and preference always produce the same choice.

use mondrian_effects::EffectGpuSpatialPass;

use super::{GpuSpatialChainBlocker, GpuSpatialChainPlan, GpuSpatialStagePlan};

/// Environment variable that overrides the estimate for every chain.
pub const SPATIAL_CHAIN_PREFERENCE_ENV: &str = "MONDRIAN_SPATIAL_CHAIN";

/// Fixed cost of preparing one GPU spatial chain (validation, pool bind, encoding).
pub const GPU_CHAIN_SETUP_NS: f64 = 2_000.0;

/// Fixed cost of one recorded render or compute pass (encode and submit).
pub const GPU_PASS_OVERHEAD_NS: f64 = 45_000.0;

/// Conservative per-pixel cost of one spatial texture pass with no measured value.
pub const GPU_DEFAULT_NS_PER_PX: f64 = 0.6;

/// Per-pixel cost of one fused point operation inside a standalone point stage.
pub const GPU_POINT_NS_PER_PX: f64 = 1.2;

/// Effective read plus write bandwidth used by the per-stage copy-out.
///
/// The driver copies each finished stage into a table resource: 32 B per pixel
/// (read plus write) at this rate.
pub const GPU_EFFECTIVE_BANDWIDTH_BYTES_PER_NS: f64 = 200.0;

/// Fixed cost of the whole-frame CPU fallback (session reconfigure and raster allocation).
pub const CPU_CHAIN_SETUP_NS: f64 = 48_000.0;

/// Fixed cost of the staging upload the CPU-composited frame pays every frame.
pub const CPU_FRAME_UPLOAD_SETUP_NS: f64 = 12_000.0;

/// Per-pixel cost of compositing one layer on the CPU.
pub const CPU_COMPOSITE_NS_PER_PX: f64 = 1.0;

/// Effective bandwidth of the upload that returns a CPU-composited frame to the GPU.
pub const CPU_UPLOAD_BANDWIDTH_BYTES_PER_NS: f64 = 200.0;

/// Fixed cost of uploading a CPU-resident layer source before the GPU chain can start.
pub const GPU_SOURCE_UPLOAD_SETUP_NS: f64 = 45_000.0;

/// How much cheaper the CPU must be before the estimate switches paths.
///
/// The margin keeps a steady timeline from flapping between the two paths when
/// the two estimates are within measurement noise of each other.
pub const CPU_CHOICE_HYSTERESIS: f64 = 1.15;

/// How strongly the caller wants to override the estimate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SpatialChainPreference {
    /// Weigh both candidates and take the cheaper one.
    #[default]
    Auto,
    /// Keep the chain on the GPU unless admission refuses it.
    ForceGpu,
    /// Keep the whole frame on the CPU.
    ForceCpu,
}

impl SpatialChainPreference {
    /// Parse the authored override; unknown text is `None`, not an error.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "gpu" => Some(Self::ForceGpu),
            "cpu" => Some(Self::ForceCpu),
            _ => None,
        }
    }

    /// Read [`SPATIAL_CHAIN_PREFERENCE_ENV`], defaulting to [`Self::Auto`].
    ///
    /// Callers read this once per frame: the variable only steers the estimate
    /// and can never make an unsupported chain run on the GPU.
    pub fn from_env() -> Self {
        std::env::var(SPATIAL_CHAIN_PREFERENCE_ENV)
            .ok()
            .and_then(|value| Self::parse(&value))
            .unwrap_or_default()
    }

    /// Stable name used by diagnostics and the environment variable.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::ForceGpu => "gpu",
            Self::ForceCpu => "cpu",
        }
    }
}

/// Which candidate one layer's spatial chain should use this frame.
#[derive(Debug, Clone, PartialEq)]
pub struct SpatialChainChoice {
    /// Whether the chain runs on the GPU this frame.
    pub use_gpu: bool,
    /// Estimated GPU-candidate cost in microseconds.
    pub gpu_us: f64,
    /// Estimated CPU-candidate cost in microseconds.
    pub cpu_us: f64,
    /// Why the estimate (or the override) landed where it did.
    pub reason: SpatialChainChoiceReason,
}

impl SpatialChainChoice {
    /// Microseconds the chosen candidate is expected to save over the other.
    ///
    /// A choice that was never estimated (a refused plan) reports no saving
    /// rather than a fabricated one.
    pub fn estimated_savings_us(&self) -> f64 {
        if self.reason.is_unavailable() {
            return 0.0;
        }
        let savings = if self.use_gpu {
            self.cpu_us - self.gpu_us
        } else {
            self.gpu_us - self.cpu_us
        };
        savings.max(0.0)
    }
}

/// Why one layer's spatial chain runs where it does.
#[derive(Debug, Clone, PartialEq)]
pub enum SpatialChainChoiceReason {
    /// Admission refused the chain, so the CPU is the only correct path.
    PlanUnavailable(GpuSpatialChainBlocker),
    /// The chain is point-only and its source is already GPU-resident, so the
    /// existing fused point path applies unchanged and there is nothing to weigh.
    GpuEverywhere,
    /// The estimate did not prove the CPU cheaper by [`CPU_CHOICE_HYSTERESIS`].
    GpuCheaper,
    /// The CPU is cheaper by at least [`CPU_CHOICE_HYSTERESIS`].
    CpuCheaper {
        /// Fraction of the GPU cost the CPU candidate saves.
        margin: f64,
    },
    /// The authored preference overrode the estimate.
    Forced(SpatialChainPreference),
}

impl SpatialChainChoiceReason {
    /// Machine-readable code for diagnostics.
    pub fn code(&self) -> &'static str {
        match self {
            Self::PlanUnavailable(_) => "spatial_chain_plan_unavailable",
            Self::GpuEverywhere => "spatial_chain_gpu_everywhere",
            Self::GpuCheaper => "spatial_chain_gpu_cheaper",
            Self::CpuCheaper { .. } => "spatial_chain_cpu_cheaper",
            Self::Forced(_) => "spatial_chain_forced",
        }
    }

    fn is_unavailable(&self) -> bool {
        matches!(self, Self::PlanUnavailable(_))
    }
}

/// Accumulated evidence of every spatial-chain choice an owner made.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpatialChainChoiceDiagnostics {
    /// Layers whose chain ran on the GPU.
    pub gpu_choices: u64,
    /// Layers whose chain stayed on the whole-frame CPU path.
    pub cpu_choices: u64,
    /// Estimated microseconds saved across every choice.
    pub estimated_savings_us: u64,
}

impl SpatialChainChoiceDiagnostics {
    /// Record one choice and its estimated saving.
    pub fn record(&mut self, choice: &SpatialChainChoice) {
        if choice.use_gpu {
            self.gpu_choices = self.gpu_choices.saturating_add(1);
        } else {
            self.cpu_choices = self.cpu_choices.saturating_add(1);
        }
        let savings = choice.estimated_savings_us().round();
        if savings > 0.0 && savings.is_finite() {
            let savings = savings.min(u64::MAX as f64) as u64;
            self.estimated_savings_us = self.estimated_savings_us.saturating_add(savings);
        }
    }

    /// Merge another accumulator into this one.
    pub fn accumulate(&mut self, other: Self) {
        self.gpu_choices = self.gpu_choices.saturating_add(other.gpu_choices);
        self.cpu_choices = self.cpu_choices.saturating_add(other.cpu_choices);
        self.estimated_savings_us =
            self.estimated_savings_us.saturating_add(other.estimated_savings_us);
    }

    /// Whether nothing was recorded.
    pub fn is_empty(&self) -> bool {
        self.gpu_choices == 0 && self.cpu_choices == 0 && self.estimated_savings_us == 0
    }
}

/// Choose the candidate for one layer's spatial chain.
///
/// Pure and device-free: [`GpuSpatialChainPlan`] has already proven every stage
/// can run, so this only weighs cost. See [`choose_spatial_chain_for_plan`] for
/// the admission-aware entry point a caller that may have been refused wants.
pub fn choose_spatial_chain(
    plan: &GpuSpatialChainPlan,
    width: u32,
    height: u32,
    source_on_gpu: bool,
    preference: SpatialChainPreference,
) -> SpatialChainChoice {
    let pixels = raster_pixels(width, height);
    let gpu_ns = estimate_gpu_ns(plan, pixels, source_on_gpu);
    let cpu_ns = estimate_cpu_ns(plan, pixels);
    if preference == SpatialChainPreference::Auto && plan.spatial_passes == 0 && source_on_gpu {
        // A point-only chain on a GPU-resident source is exactly the composite
        // the fused point path already records; there is no second candidate to
        // weigh and no upload to pay, so it stays on the GPU unconditionally.
        return SpatialChainChoice {
            use_gpu: true,
            gpu_us: gpu_ns / 1_000.0,
            cpu_us: cpu_ns / 1_000.0,
            reason: SpatialChainChoiceReason::GpuEverywhere,
        };
    }
    decide(gpu_ns, cpu_ns, preference)
}

/// Choose for a plan that admission may have refused.
///
/// A refused chain always chooses the CPU regardless of the authored preference
/// and still carries the blocker, so "why this frame went CPU" stays observable.
pub fn choose_spatial_chain_for_plan(
    plan: Result<&GpuSpatialChainPlan, &GpuSpatialChainBlocker>,
    width: u32,
    height: u32,
    source_on_gpu: bool,
    preference: SpatialChainPreference,
) -> SpatialChainChoice {
    match plan {
        Ok(plan) => choose_spatial_chain(plan, width, height, source_on_gpu, preference),
        Err(blocker) => SpatialChainChoice {
            use_gpu: false,
            gpu_us: 0.0,
            cpu_us: 0.0,
            reason: SpatialChainChoiceReason::PlanUnavailable(blocker.clone()),
        },
    }
}

fn decide(gpu_ns: f64, cpu_ns: f64, preference: SpatialChainPreference) -> SpatialChainChoice {
    let gpu_us = gpu_ns / 1_000.0;
    let cpu_us = cpu_ns / 1_000.0;
    match preference {
        SpatialChainPreference::ForceGpu => SpatialChainChoice {
            use_gpu: true,
            gpu_us,
            cpu_us,
            reason: SpatialChainChoiceReason::Forced(SpatialChainPreference::ForceGpu),
        },
        SpatialChainPreference::ForceCpu => SpatialChainChoice {
            use_gpu: false,
            gpu_us,
            cpu_us,
            reason: SpatialChainChoiceReason::Forced(SpatialChainPreference::ForceCpu),
        },
        SpatialChainPreference::Auto => {
            if cpu_us * CPU_CHOICE_HYSTERESIS < gpu_us {
                let margin = if gpu_us > 0.0 {
                    (gpu_us - cpu_us) / gpu_us
                } else {
                    0.0
                };
                SpatialChainChoice {
                    use_gpu: false,
                    gpu_us,
                    cpu_us,
                    reason: SpatialChainChoiceReason::CpuCheaper { margin },
                }
            } else {
                SpatialChainChoice {
                    use_gpu: true,
                    gpu_us,
                    cpu_us,
                    reason: SpatialChainChoiceReason::GpuCheaper,
                }
            }
        }
    }
}

/// Estimated GPU-candidate cost in nanoseconds.
fn estimate_gpu_ns(plan: &GpuSpatialChainPlan, pixels: f64, source_on_gpu: bool) -> f64 {
    let mut total = GPU_CHAIN_SETUP_NS;
    for stage in &plan.stages {
        match stage {
            GpuSpatialStagePlan::Spatial { pass, scratch_textures, .. } => {
                let passes = f64::from(*scratch_textures);
                total += passes * (pixels * gpu_ns_per_px(*pass) + GPU_PASS_OVERHEAD_NS);
                total += copy_out_ns(pixels);
            }
            GpuSpatialStagePlan::PointOps { operations } => {
                total += point_stage_ns(pixels, operations.len());
            }
        }
    }
    if !source_on_gpu {
        total += upload_ns(pixels);
    }
    total
}

/// Estimated whole-frame CPU fallback cost in nanoseconds.
fn estimate_cpu_ns(plan: &GpuSpatialChainPlan, pixels: f64) -> f64 {
    let mut total = CPU_CHAIN_SETUP_NS;
    for stage in &plan.stages {
        // One CPU kernel call per spatial stage: the kernel's own internal
        // passes are already part of its measured per-pixel rate.
        if let GpuSpatialStagePlan::Spatial { pass, .. } = stage {
            total += pixels * cpu_ns_per_px(*pass);
        }
    }
    total += pixels * CPU_COMPOSITE_NS_PER_PX;
    total += cpu_upload_full_ns(pixels);
    total
}

/// Raster pixels of one layer as a rate multiplier.
fn raster_pixels(width: u32, height: u32) -> f64 {
    f64::from(width) * f64::from(height)
}

/// Per-pixel GPU cost of one texture pass of this kernel.
fn gpu_ns_per_px(pass: EffectGpuSpatialPass) -> f64 {
    match pass.name() {
        // Separable blurs and multi-pass morphology stay close to the two
        // texture reads and one write that bound every pass.
        "box_blur" | "channel_blur" => 0.5,
        // Neighbourhood kernels and whole-chain morphology jobs cost more.
        "convolution" | "glow" | "matte_refine" | "deband" => 0.9,
        // The arc remap is a four-tap bilinear read and one write.
        "warp" => 0.6,
        // Magnify adds the blend-mode branch on top of the same bilinear read.
        "magnify" => 0.8,
        // Wire removal reads up to two bilinear taps and writes one pixel.
        "wire_removal" => 0.6,
        // Roughen edges folds a two-octave fractal field, one bilinear tap and
        // one write per pixel.
        "roughen_edges" => 1.2,
        // Selective colour is one pass of arithmetic per pixel: ten uniform
        // cell reads and one write, with no neighbour taps.
        "selective_color" => 0.6,
        // Stroke scans up to a hundred and twelve taps per pixel before the
        // band composite, so it sits far above one-pass arithmetic.
        "stroke" => 3.0,
        // The median gathers the whole window eight times over, so it costs more
        // than a separable blur pass despite the small GPU radius cap.
        "median" => 4.0,
        // Dust & scratches gathers the same window as the median, so it carries
        // the same cap and the same rate; the gate is a few extra comparisons on
        // top of the selection.
        "dust_scratches" => 4.0,
        // The bezier warp runs up to thirteen Newton iterations per pixel, and
        // every iteration evaluates the patch, both partials, and the corners.
        "bezier_warp" => 6.0,
        // The sphere rotation is one trigonometric remap and one bilinear tap
        // per pixel; there is no window or iteration on top of it.
        "vr_rotate_sphere" => 1.2,
        _ => GPU_DEFAULT_NS_PER_PX,
    }
}

/// Per-pixel CPU cost of one whole kernel, as measured by the effects harness.
///
/// Rates are effective multi-core rates, so they already include the row-band
/// parallelism `throughput.rs` reports.
fn cpu_ns_per_px(pass: EffectGpuSpatialPass) -> f64 {
    match pass.name() {
        "box_blur" | "offset" | "mirror" | "flip" | "split" => 1.0,
        "channel_blur" => 3.0,
        "directional_blur" | "convolution" | "glow" | "fractal_noise" | "deband" => 3.0,
        "twirl" | "spherize" | "dent" | "polar_coordinates" | "lens_distortion" => 2.0,
        "warp" => 2.0,
        // Includes the blend-mode branch, roughly fifty percent above a pure
        // remap of the same footprint.
        "magnify" => 3.0,
        // Up to two bilinear taps averaged behind a per-row band test.
        "wire_removal" => 2.0,
        // Two fractal octaves, a central difference and a bilinear tap.
        "roughen_edges" => 4.0,
        // Nine weights, nine folded rows, and one CMYK round trip per pixel.
        "selective_color" => 2.0,
        // Fifty-six to a hundred and twelve nearest-neighbour taps with a
        // bracket decision per tap: the most expensive CPU kernel in the family.
        "stroke" => 6.0,
        // The sliding histogram touches each window twice per axis.
        "median" => 3.0,
        // Same sliding histogram as the median plus the deviation gate.
        "dust_scratches" => 3.0,
        // Thirteen f64 Newton iterations in the worst case; far above a remap.
        "bezier_warp" => 8.0,
        // Two transcendental chains, the matrix product and one per-pixel
        // unpremultiply.
        "vr_rotate_sphere" => 6.0,
        "radial_blur" | "turbulent_displace" | "keylight" | "matte_refine" => 4.0,
        "minimax" | "choker" | "find_edges" | "emboss" | "color_emboss" => 2.5,
        _ => 2.0,
    }
}

/// GPU cost of one standalone full-raster point stage.
fn point_stage_ns(pixels: f64, operations: usize) -> f64 {
    let operations = f64::from(u32::try_from(operations).unwrap_or(u32::MAX));
    GPU_PASS_OVERHEAD_NS + pixels * GPU_POINT_NS_PER_PX * operations.max(1.0)
}

/// GPU cost of copying one finished stage into a table resource.
fn copy_out_ns(pixels: f64) -> f64 {
    pixels * 32.0 / GPU_EFFECTIVE_BANDWIDTH_BYTES_PER_NS
}

/// GPU cost of uploading a CPU-resident layer source.
fn upload_ns(pixels: f64) -> f64 {
    GPU_SOURCE_UPLOAD_SETUP_NS + pixels * 16.0 / GPU_EFFECTIVE_BANDWIDTH_BYTES_PER_NS
}

/// CPU cost of uploading the composited frame back to the GPU.
fn cpu_upload_full_ns(pixels: f64) -> f64 {
    CPU_FRAME_UPLOAD_SETUP_NS + pixels * 16.0 / CPU_UPLOAD_BANDWIDTH_BYTES_PER_NS
}

#[cfg(test)]
mod tests {
    use mondrian_effects::{ChannelBlurDirection, ChannelBlurGrade, EffectGpuPointOp, TwirlGrade};

    use super::*;

    fn spatial_stage(pass: EffectGpuSpatialPass, passes: u32) -> GpuSpatialStagePlan {
        GpuSpatialStagePlan::Spatial {
            pass,
            scratch_textures: passes,
            horizontal_halo: pass.horizontal_halo(),
            vertical_halo: pass.vertical_halo(),
        }
    }

    /// Resolve the plan the way `gpu_spatial_chain_plan` would for these stages.
    fn chain_plan(stages: Vec<GpuSpatialStagePlan>) -> GpuSpatialChainPlan {
        let mut spatial_passes = 0_u32;
        let mut point_stages = 0_u32;
        let mut largest = 0_u32;
        for stage in &stages {
            match stage {
                GpuSpatialStagePlan::Spatial { scratch_textures, .. } => {
                    spatial_passes = spatial_passes.saturating_add(*scratch_textures);
                    largest = largest.max(*scratch_textures);
                }
                GpuSpatialStagePlan::PointOps { .. } => {
                    point_stages = point_stages.saturating_add(1);
                }
            }
        }
        GpuSpatialChainPlan {
            stages,
            spatial_passes,
            point_stages,
            scratch_textures: usize::try_from(largest).unwrap_or(usize::MAX).saturating_add(1),
            scratch_bytes: 0,
        }
    }

    /// A one-pass Twirl chain, matching the `9x7` compositor fixture.
    fn twirl_plan(width: u32, height: u32) -> GpuSpatialChainPlan {
        let grade = TwirlGrade::new(45.0, 80.0, [width as f32 * 0.5, height as f32 * 0.5])
            .expect("valid twirl grade");
        chain_plan(vec![spatial_stage(
            EffectGpuSpatialPass::Twirl { grade },
            1,
        )])
    }

    #[test]
    fn cost_chooses_gpu_for_a_single_cheap_pass() {
        let plan = twirl_plan(9, 7);
        let choice = choose_spatial_chain(&plan, 9, 7, true, SpatialChainPreference::Auto);

        assert!(
            choice.use_gpu,
            "a one-pass chain on a GPU source must stay on the GPU"
        );
        assert_eq!(choice.reason, SpatialChainChoiceReason::GpuCheaper);
        assert!(
            choice.gpu_us < choice.cpu_us,
            "GPU estimate must be the cheaper one"
        );
    }

    #[test]
    fn cost_chooses_gpu_for_a_4k_blur_chain() {
        let grade = ChannelBlurGrade::new(12.0, 12.0, 12.0, 12.0, 0.5, ChannelBlurDirection::Both)
            .expect("valid channel blur grade");
        let plan = chain_plan(vec![spatial_stage(
            EffectGpuSpatialPass::ChannelBlur { grade },
            2,
        )]);
        let choice = choose_spatial_chain(&plan, 3840, 2160, true, SpatialChainPreference::Auto);

        assert!(
            choice.use_gpu,
            "a 4K separable blur is far cheaper on the GPU"
        );
        assert_eq!(choice.reason, SpatialChainChoiceReason::GpuCheaper);
        assert!(choice.cpu_us > choice.gpu_us);
    }

    #[test]
    fn cost_chooses_cpu_when_the_source_is_on_the_cpu_and_the_chain_is_one_point_stage() {
        let plan = chain_plan(vec![GpuSpatialStagePlan::PointOps {
            operations: vec![EffectGpuPointOp::Grain { amount: 0.05 }],
        }]);
        let choice = choose_spatial_chain(&plan, 1, 1, false, SpatialChainPreference::Auto);

        assert!(
            !choice.use_gpu,
            "the upload dominates a 1x1 CPU-resident source"
        );
        assert!(
            matches!(choice.reason, SpatialChainChoiceReason::CpuCheaper { margin } if margin > 0.0),
            "unexpected reason {:?}",
            choice.reason
        );
        assert!(choice.cpu_us * CPU_CHOICE_HYSTERESIS < choice.gpu_us);
    }

    #[test]
    fn forced_preferences_override_the_estimate() {
        let plan = twirl_plan(9, 7);
        let forced_gpu = choose_spatial_chain(&plan, 9, 7, true, SpatialChainPreference::ForceGpu);
        assert!(forced_gpu.use_gpu);
        assert_eq!(
            forced_gpu.reason,
            SpatialChainChoiceReason::Forced(SpatialChainPreference::ForceGpu)
        );

        // The tiny CPU-resident point chain the estimate rejects.
        let point_plan = chain_plan(vec![GpuSpatialStagePlan::PointOps {
            operations: Vec::new(),
        }]);
        let forced_cpu =
            choose_spatial_chain(&point_plan, 1, 1, false, SpatialChainPreference::ForceCpu);
        assert!(!forced_cpu.use_gpu);
        assert_eq!(
            forced_cpu.reason,
            SpatialChainChoiceReason::Forced(SpatialChainPreference::ForceCpu)
        );
        let auto = choose_spatial_chain(&point_plan, 1, 1, false, SpatialChainPreference::Auto);
        assert!(!auto.use_gpu);
        assert_ne!(auto.reason, forced_cpu.reason);
    }

    #[test]
    fn plan_blockers_always_choose_cpu() {
        let blocker = GpuSpatialChainBlocker::ScratchBudgetExceeded {
            required_bytes: 1_064_000_000,
            budget_bytes: super::super::GPU_SPATIAL_SCRATCH_BUDGET_BYTES,
        };
        for preference in [
            SpatialChainPreference::Auto,
            SpatialChainPreference::ForceGpu,
            SpatialChainPreference::ForceCpu,
        ] {
            let choice = choose_spatial_chain_for_plan(Err(&blocker), 3840, 2160, true, preference);
            assert!(
                !choice.use_gpu,
                "{preference:?} must not override a refused chain"
            );
            assert_eq!(
                choice.reason,
                SpatialChainChoiceReason::PlanUnavailable(blocker.clone())
            );
            assert_eq!(choice.estimated_savings_us(), 0.0);
        }
    }

    #[test]
    fn hysteresis_prevents_a_flip_within_15_percent() {
        let gpu_ns = 1_000_000.0;

        // The CPU is only 5% cheaper, which is inside the band: the GPU keeps
        // the frame instead of flapping on measurement noise.
        let inside = decide(gpu_ns, gpu_ns * 0.95, SpatialChainPreference::Auto);
        assert!(
            inside.use_gpu,
            "a 5% saving is inside the 15% hysteresis band"
        );
        assert_eq!(inside.reason, SpatialChainChoiceReason::GpuCheaper);

        // The same holds when the CPU is the more expensive candidate.
        let dearer = decide(gpu_ns, gpu_ns * 1.05, SpatialChainPreference::Auto);
        assert!(dearer.use_gpu);

        // Past the band the estimate does flip.
        let flipped = decide(gpu_ns, gpu_ns * 0.80, SpatialChainPreference::Auto);
        assert!(!flipped.use_gpu);
        assert!(matches!(
            flipped.reason,
            SpatialChainChoiceReason::CpuCheaper { margin } if (margin - 0.2).abs() < 1e-9
        ));
    }

    #[test]
    fn cost_preference_parses_the_environment_override() {
        assert_eq!(
            SpatialChainPreference::parse("auto"),
            Some(SpatialChainPreference::Auto)
        );
        assert_eq!(
            SpatialChainPreference::parse("GPU"),
            Some(SpatialChainPreference::ForceGpu)
        );
        assert_eq!(
            SpatialChainPreference::parse(" cpu "),
            Some(SpatialChainPreference::ForceCpu)
        );
        assert_eq!(SpatialChainPreference::parse("gpu-ish"), None);
        assert_eq!(SpatialChainPreference::parse(""), None);
        for preference in [
            SpatialChainPreference::Auto,
            SpatialChainPreference::ForceGpu,
            SpatialChainPreference::ForceCpu,
        ] {
            assert_eq!(
                SpatialChainPreference::parse(preference.as_str()),
                Some(preference)
            );
        }
    }

    #[test]
    fn cost_diagnostics_record_both_directions() {
        let plan = twirl_plan(9, 7);
        let gpu = choose_spatial_chain(&plan, 9, 7, true, SpatialChainPreference::Auto);
        let cpu = choose_spatial_chain(&plan, 9, 7, true, SpatialChainPreference::ForceCpu);
        let mut diagnostics = SpatialChainChoiceDiagnostics::default();
        diagnostics.record(&gpu);
        diagnostics.record(&cpu);
        assert_eq!(diagnostics.gpu_choices, 1);
        assert_eq!(diagnostics.cpu_choices, 1);
        assert!(diagnostics.estimated_savings_us > 0);
        assert!(!diagnostics.is_empty());
        let mut again = SpatialChainChoiceDiagnostics::default();
        again.accumulate(diagnostics);
        assert_eq!(again, diagnostics);
    }
}
