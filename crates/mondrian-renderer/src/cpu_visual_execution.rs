//! Owner-local parallel and runtime-vectorized CPU visual kernels.
//!
//! This Module deliberately keeps Rayon behind the renderer Interface. Preview
//! and Export owners each retain one bounded pool through their
//! `TimelineCompositeScratch`; no work enters Rayon's process-global pool.

use pulp::{Arch, Simd, WithSimd};
use rayon::prelude::*;

const DEFAULT_PARALLEL_PIXEL_THRESHOLD: usize = 256 * 1024;
const DEFAULT_SIMD_PIXEL_THRESHOLD: usize = 4 * 1024;
pub(crate) const MAX_OWNER_WORKERS: usize = 8;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CpuVisualKernelEvidence {
    pub(crate) parallel_dispatches: u64,
    pub(crate) runtime_vectorized_pixels: u64,
}

pub(crate) struct CpuVisualExecutionSession {
    pool: Option<rayon::ThreadPool>,
    pool_creation_attempted: bool,
    worker_count: usize,
    parallel_pixel_threshold: usize,
    simd_pixel_threshold: usize,
}

impl Default for CpuVisualExecutionSession {
    fn default() -> Self {
        let available = std::thread::available_parallelism().map_or(1, usize::from);
        let worker_count = available.saturating_sub(1).clamp(1, MAX_OWNER_WORKERS);
        Self {
            pool: None,
            pool_creation_attempted: false,
            worker_count,
            parallel_pixel_threshold: DEFAULT_PARALLEL_PIXEL_THRESHOLD,
            simd_pixel_threshold: DEFAULT_SIMD_PIXEL_THRESHOLD,
        }
    }
}

impl CpuVisualExecutionSession {
    pub(crate) fn reconfigure(&mut self, worker_count: usize, parallel_pixel_threshold: usize) {
        self.pool = None;
        self.pool_creation_attempted = false;
        self.worker_count = worker_count.clamp(1, MAX_OWNER_WORKERS);
        self.parallel_pixel_threshold = parallel_pixel_threshold.max(1);
    }

    fn ensure_pool(&mut self) -> Option<&rayon::ThreadPool> {
        if self.worker_count <= 1 {
            return None;
        }
        if !self.pool_creation_attempted {
            self.pool_creation_attempted = true;
            self.pool = rayon::ThreadPoolBuilder::new()
                .num_threads(self.worker_count)
                .thread_name(|index| format!("mondrian-cpu-visual-{index}"))
                .build()
                .ok();
        }
        self.pool.as_ref()
    }

    pub(crate) fn blend_normal_full_frame(
        &mut self,
        destination: &mut [[f32; 4]],
        source: &[[f32; 4]],
        opacity: f32,
    ) -> CpuVisualKernelEvidence {
        debug_assert_eq!(destination.len(), source.len());
        if destination.len() >= self.parallel_pixel_threshold
            && let Some(pool) = self.ensure_pool()
        {
            let chunk_pixels = parallel_chunk_pixels(destination.len(), pool.current_num_threads());
            pool.install(|| {
                destination
                    .par_chunks_mut(chunk_pixels)
                    .zip(source.par_chunks(chunk_pixels))
                    .for_each(|(destination, source)| {
                        blend_normal_scalar(destination, source, opacity);
                    });
            });
            return CpuVisualKernelEvidence {
                parallel_dispatches: 1,
                runtime_vectorized_pixels: 0,
            };
        }
        blend_normal_scalar(destination, source, opacity);
        CpuVisualKernelEvidence::default()
    }

    pub(crate) fn cross_dissolve(
        &mut self,
        output: &mut [[f32; 4]],
        left: &[[f32; 4]],
        right: &[[f32; 4]],
        progress: f32,
    ) -> CpuVisualKernelEvidence {
        debug_assert_eq!(output.len(), left.len());
        debug_assert_eq!(output.len(), right.len());
        let progress = progress.clamp(0.0, 1.0);
        let arch = Arch::new();
        let vectorized = !matches!(arch, Arch::Scalar)
            && progress.is_finite()
            && output.len() >= self.simd_pixel_threshold
            && left.iter().all(|pixel| pixel[3] == 1.0)
            && right.iter().all(|pixel| pixel[3] == 1.0);
        if output.len() >= self.parallel_pixel_threshold
            && let Some(pool) = self.ensure_pool()
        {
            let chunk_pixels = parallel_chunk_pixels(output.len(), pool.current_num_threads());
            pool.install(|| {
                output
                    .par_chunks_mut(chunk_pixels)
                    .zip(left.par_chunks(chunk_pixels))
                    .zip(right.par_chunks(chunk_pixels))
                    .for_each(|((output, left), right)| {
                        cross_dissolve_chunk(arch, output, left, right, progress, vectorized);
                    });
            });
            return CpuVisualKernelEvidence {
                parallel_dispatches: 1,
                runtime_vectorized_pixels: if vectorized { output.len() as u64 } else { 0 },
            };
        }
        cross_dissolve_chunk(arch, output, left, right, progress, vectorized);
        CpuVisualKernelEvidence {
            parallel_dispatches: 0,
            runtime_vectorized_pixels: if vectorized { output.len() as u64 } else { 0 },
        }
    }
}

fn parallel_chunk_pixels(pixel_count: usize, workers: usize) -> usize {
    pixel_count.div_ceil(workers.saturating_mul(4).max(1)).max(1024)
}

fn blend_normal_scalar(destination: &mut [[f32; 4]], source: &[[f32; 4]], opacity: f32) {
    for (destination, source) in destination.iter_mut().zip(source) {
        *destination = blend_normal_pixel(*destination, *source, opacity);
    }
}

#[inline]
pub(crate) fn blend_normal_pixel(base: [f32; 4], blend: [f32; 4], opacity: f32) -> [f32; 4] {
    let base_alpha = base[3].clamp(0.0, 1.0);
    let blend_alpha = (blend[3] * opacity).clamp(0.0, 1.0);
    if !mondrian_effects::has_positive_coverage(blend_alpha) {
        return base;
    }
    if !mondrian_effects::has_positive_coverage(base_alpha) {
        return [blend[0], blend[1], blend[2], blend_alpha];
    }

    let inverse_blend_alpha = 1.0 - blend_alpha;
    let output_alpha = blend_alpha + base_alpha * inverse_blend_alpha;
    if !mondrian_effects::has_positive_coverage(output_alpha) {
        return [0.0; 4];
    }
    let base_weight = base_alpha * inverse_blend_alpha;
    [
        (blend[0] * blend_alpha + base[0] * base_weight) / output_alpha,
        (blend[1] * blend_alpha + base[1] * base_weight) / output_alpha,
        (blend[2] * blend_alpha + base[2] * base_weight) / output_alpha,
        output_alpha,
    ]
}

fn cross_dissolve_chunk(
    arch: Arch,
    output: &mut [[f32; 4]],
    left: &[[f32; 4]],
    right: &[[f32; 4]],
    progress: f32,
    vectorized: bool,
) {
    if vectorized {
        arch.dispatch(OpaqueCrossDissolve {
            output: output.as_flattened_mut(),
            left: left.as_flattened(),
            right: right.as_flattened(),
            progress,
        });
    } else {
        for ((output, left), right) in output.iter_mut().zip(left).zip(right) {
            *output = mondrian_effects::mix_straight_rgba(*left, *right, progress);
        }
    }
}

struct OpaqueCrossDissolve<'a> {
    output: &'a mut [f32],
    left: &'a [f32],
    right: &'a [f32],
    progress: f32,
}

impl WithSimd for OpaqueCrossDissolve<'_> {
    type Output = ();

    #[inline(always)]
    fn with_simd<S: Simd>(self, simd: S) {
        let (output, output_tail) = S::as_mut_simd_f32s(self.output);
        let (left, left_tail) = S::as_simd_f32s(self.left);
        let (right, right_tail) = S::as_simd_f32s(self.right);
        let right_weight = simd.splat_f32s(self.progress);
        let left_weight = simd.splat_f32s(1.0 - self.progress);
        for ((output, left), right) in output.iter_mut().zip(left).zip(right) {
            *output = simd.add_f32s(
                simd.mul_f32s(*left, left_weight),
                simd.mul_f32s(*right, right_weight),
            );
        }
        for ((output, left), right) in output_tail.iter_mut().zip(left_tail).zip(right_tail) {
            *output = *left * (1.0 - self.progress) + *right * self.progress;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_vectorized_opaque_dissolve_matches_canonical_scalar() {
        let left = (0..4103)
            .map(|index| [index as f32 * 0.0001, -0.25, 1.5, 1.0])
            .collect::<Vec<_>>();
        let right = (0..4103)
            .map(|index| [1.0 - index as f32 * 0.00003, 0.75, -0.5, 1.0])
            .collect::<Vec<_>>();
        let mut expected = vec![[0.0; 4]; left.len()];
        for ((output, left), right) in expected.iter_mut().zip(&left).zip(&right) {
            *output = mondrian_effects::mix_straight_rgba(*left, *right, 0.37);
        }
        let mut actual = vec![[0.0; 4]; left.len()];
        cross_dissolve_chunk(Arch::new(), &mut actual, &left, &right, 0.37, true);
        for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            for channel in 0..4 {
                assert!(
                    (actual[channel] - expected[channel]).abs() <= 1.0e-6,
                    "pixel {index} channel {channel}: actual={} expected={}",
                    actual[channel],
                    expected[channel]
                );
            }
        }
    }
}
