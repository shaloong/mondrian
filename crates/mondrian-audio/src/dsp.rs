//! Equivalent scalar-reference and runtime-vectorized PCM kernels.

use crate::AudioKernelBackend;
use pulp::{Arch, Simd, WithSimd};

pub(crate) fn db_to_linear(db: f64) -> f32 {
    10.0_f64.powf(db / 20.0) as f32
}

pub(crate) fn expand_frame_db_to_interleaved_gains(
    frame_db: &[f64],
    channels: usize,
    destination: &mut [f32],
) {
    debug_assert_eq!(destination.len(), frame_db.len().saturating_mul(channels));
    for (frame, db) in frame_db.iter().copied().enumerate() {
        destination[frame * channels..(frame + 1) * channels].fill(db_to_linear(db));
    }
}

pub(crate) fn multiply_into(
    backend: AudioKernelBackend,
    destination: &mut [f32],
    source: &[f32],
    gains: &[f32],
) {
    debug_assert_eq!(destination.len(), source.len());
    debug_assert_eq!(destination.len(), gains.len());
    match backend {
        AudioKernelBackend::ScalarReference => {
            multiply_into_scalar(destination, source, gains);
        }
        AudioKernelBackend::RuntimeVectorized => {
            Arch::new().dispatch(MultiplyInto { destination, source, gains });
        }
    }
}

pub(crate) fn multiply_constant_into(
    backend: AudioKernelBackend,
    destination: &mut [f32],
    source: &[f32],
    gain: f32,
) {
    debug_assert_eq!(destination.len(), source.len());
    match backend {
        AudioKernelBackend::ScalarReference => {
            for (destination, source) in destination.iter_mut().zip(source) {
                *destination = *source * gain;
            }
        }
        AudioKernelBackend::RuntimeVectorized => {
            Arch::new().dispatch(MultiplyConstant { destination, source, gain });
        }
    }
}

pub(crate) fn multiply_in_place(backend: AudioKernelBackend, samples: &mut [f32], gains: &[f32]) {
    debug_assert_eq!(samples.len(), gains.len());
    match backend {
        AudioKernelBackend::ScalarReference => multiply_in_place_scalar(samples, gains),
        AudioKernelBackend::RuntimeVectorized => {
            Arch::new().dispatch(MultiplyInPlace { samples, gains });
        }
    }
}

pub(crate) fn multiply_constant_in_place(
    backend: AudioKernelBackend,
    samples: &mut [f32],
    gain: f32,
) {
    match backend {
        AudioKernelBackend::ScalarReference => {
            for sample in samples {
                *sample *= gain;
            }
        }
        AudioKernelBackend::RuntimeVectorized => {
            Arch::new().dispatch(MultiplyConstantInPlace { samples, gain });
        }
    }
}

pub(crate) fn add(backend: AudioKernelBackend, destination: &mut [f32], source: &[f32]) {
    debug_assert_eq!(destination.len(), source.len());
    match backend {
        AudioKernelBackend::ScalarReference => add_scalar(destination, source),
        AudioKernelBackend::RuntimeVectorized => {
            Arch::new().dispatch(Add { destination, source });
        }
    }
}

pub(crate) fn multiply_add(
    backend: AudioKernelBackend,
    destination: &mut [f32],
    source: &[f32],
    gains: &[f32],
) {
    debug_assert_eq!(destination.len(), source.len());
    debug_assert_eq!(destination.len(), gains.len());
    match backend {
        AudioKernelBackend::ScalarReference => multiply_add_scalar(destination, source, gains),
        AudioKernelBackend::RuntimeVectorized => {
            Arch::new().dispatch(MultiplyAdd { destination, source, gains });
        }
    }
}

pub(crate) fn multiply_add_constant(
    backend: AudioKernelBackend,
    destination: &mut [f32],
    source: &[f32],
    gain: f32,
) {
    debug_assert_eq!(destination.len(), source.len());
    match backend {
        AudioKernelBackend::ScalarReference => {
            for (destination, source) in destination.iter_mut().zip(source) {
                *destination += *source * gain;
            }
        }
        AudioKernelBackend::RuntimeVectorized => {
            Arch::new().dispatch(MultiplyAddConstant { destination, source, gain });
        }
    }
}

fn multiply_into_scalar(destination: &mut [f32], source: &[f32], gains: &[f32]) {
    for ((destination, source), gain) in destination.iter_mut().zip(source).zip(gains) {
        *destination = *source * *gain;
    }
}

fn multiply_in_place_scalar(samples: &mut [f32], gains: &[f32]) {
    for (sample, gain) in samples.iter_mut().zip(gains) {
        *sample *= *gain;
    }
}

fn add_scalar(destination: &mut [f32], source: &[f32]) {
    for (destination, source) in destination.iter_mut().zip(source) {
        *destination += *source;
    }
}

fn multiply_add_scalar(destination: &mut [f32], source: &[f32], gains: &[f32]) {
    for ((destination, source), gain) in destination.iter_mut().zip(source).zip(gains) {
        *destination += *source * *gain;
    }
}

struct MultiplyInto<'a> {
    destination: &'a mut [f32],
    source: &'a [f32],
    gains: &'a [f32],
}

struct MultiplyInPlace<'a> {
    samples: &'a mut [f32],
    gains: &'a [f32],
}

impl WithSimd for MultiplyInto<'_> {
    type Output = ();

    #[inline(always)]
    fn with_simd<S: Simd>(self, simd: S) {
        let (destination, destination_tail) = S::as_mut_simd_f32s(self.destination);
        let (source, source_tail) = S::as_simd_f32s(self.source);
        let (gains, gains_tail) = S::as_simd_f32s(self.gains);
        let zero = simd.splat_f32s(0.0);
        for ((destination, source), gain) in destination.iter_mut().zip(source).zip(gains) {
            *destination = simd.mul_add_f32s(*source, *gain, zero);
        }
        multiply_into_scalar(destination_tail, source_tail, gains_tail);
    }
}

struct Add<'a> {
    destination: &'a mut [f32],
    source: &'a [f32],
}

struct MultiplyAdd<'a> {
    destination: &'a mut [f32],
    source: &'a [f32],
    gains: &'a [f32],
}

struct MultiplyAddConstant<'a> {
    destination: &'a mut [f32],
    source: &'a [f32],
    gain: f32,
}

struct MultiplyConstant<'a> {
    destination: &'a mut [f32],
    source: &'a [f32],
    gain: f32,
}

struct MultiplyConstantInPlace<'a> {
    samples: &'a mut [f32],
    gain: f32,
}

impl WithSimd for MultiplyConstant<'_> {
    type Output = ();

    #[inline(always)]
    fn with_simd<S: Simd>(self, simd: S) {
        let (destination, destination_tail) = S::as_mut_simd_f32s(self.destination);
        let (source, source_tail) = S::as_simd_f32s(self.source);
        let gain = simd.splat_f32s(self.gain);
        let zero = simd.splat_f32s(0.0);
        for (destination, source) in destination.iter_mut().zip(source) {
            *destination = simd.mul_add_f32s(*source, gain, zero);
        }
        for (destination, source) in destination_tail.iter_mut().zip(source_tail) {
            *destination = *source * self.gain;
        }
    }
}

impl WithSimd for MultiplyInPlace<'_> {
    type Output = ();

    #[inline(always)]
    fn with_simd<S: Simd>(self, simd: S) {
        let (samples, sample_tail) = S::as_mut_simd_f32s(self.samples);
        let (gains, gain_tail) = S::as_simd_f32s(self.gains);
        let zero = simd.splat_f32s(0.0);
        for (samples, gains) in samples.iter_mut().zip(gains) {
            *samples = simd.mul_add_f32s(*samples, *gains, zero);
        }
        multiply_in_place_scalar(sample_tail, gain_tail);
    }
}

impl WithSimd for MultiplyConstantInPlace<'_> {
    type Output = ();

    #[inline(always)]
    fn with_simd<S: Simd>(self, simd: S) {
        let (samples, sample_tail) = S::as_mut_simd_f32s(self.samples);
        let gain = simd.splat_f32s(self.gain);
        let zero = simd.splat_f32s(0.0);
        for samples in samples {
            *samples = simd.mul_add_f32s(*samples, gain, zero);
        }
        for sample in sample_tail {
            *sample *= self.gain;
        }
    }
}

impl WithSimd for Add<'_> {
    type Output = ();

    #[inline(always)]
    fn with_simd<S: Simd>(self, simd: S) {
        let (destination, destination_tail) = S::as_mut_simd_f32s(self.destination);
        let (source, source_tail) = S::as_simd_f32s(self.source);
        let one = simd.splat_f32s(1.0);
        for (destination, source) in destination.iter_mut().zip(source) {
            *destination = simd.mul_add_f32s(*source, one, *destination);
        }
        add_scalar(destination_tail, source_tail);
    }
}

impl WithSimd for MultiplyAdd<'_> {
    type Output = ();

    #[inline(always)]
    fn with_simd<S: Simd>(self, simd: S) {
        let (destination, destination_tail) = S::as_mut_simd_f32s(self.destination);
        let (source, source_tail) = S::as_simd_f32s(self.source);
        let (gains, gains_tail) = S::as_simd_f32s(self.gains);
        for ((destination, source), gain) in destination.iter_mut().zip(source).zip(gains) {
            *destination = simd.mul_add_f32s(*source, *gain, *destination);
        }
        multiply_add_scalar(destination_tail, source_tail, gains_tail);
    }
}

impl WithSimd for MultiplyAddConstant<'_> {
    type Output = ();

    #[inline(always)]
    fn with_simd<S: Simd>(self, simd: S) {
        let (destination, destination_tail) = S::as_mut_simd_f32s(self.destination);
        let (source, source_tail) = S::as_simd_f32s(self.source);
        let gain = simd.splat_f32s(self.gain);
        for (destination, source) in destination.iter_mut().zip(source) {
            *destination = simd.mul_add_f32s(*source, gain, *destination);
        }
        for (destination, source) in destination_tail.iter_mut().zip(source_tail) {
            *destination += *source * self.gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_fma_equivalent(left: &[f32], right: &[f32]) {
        assert_eq!(left.len(), right.len());
        for (index, (left, right)) in left.iter().zip(right).enumerate() {
            assert!(
                (left - right).abs() <= 1.0e-6,
                "sample {index}: scalar={left}, vectorized={right}"
            );
        }
    }

    #[test]
    fn runtime_vectorized_kernels_match_scalar_reference() {
        let source = (0..1031).map(|index| index as f32 * 0.03125 - 7.0).collect::<Vec<_>>();
        let gains = (0..1031).map(|index| (index % 23) as f32 * 0.017).collect::<Vec<_>>();
        let initial = (0..1031).map(|index| (index % 11) as f32 * -0.125).collect::<Vec<_>>();

        let mut scalar = initial.clone();
        let mut vectorized = initial;

        multiply_into(
            AudioKernelBackend::ScalarReference,
            &mut scalar,
            &source,
            &gains,
        );
        multiply_into(
            AudioKernelBackend::RuntimeVectorized,
            &mut vectorized,
            &source,
            &gains,
        );
        assert_fma_equivalent(&scalar, &vectorized);

        scalar.fill(0.5);
        vectorized.fill(0.5);
        multiply_add(
            AudioKernelBackend::ScalarReference,
            &mut scalar,
            &source,
            &gains,
        );
        multiply_add(
            AudioKernelBackend::RuntimeVectorized,
            &mut vectorized,
            &source,
            &gains,
        );
        assert_fma_equivalent(&scalar, &vectorized);

        scalar.fill(0.5);
        vectorized.fill(0.5);
        multiply_add_constant(
            AudioKernelBackend::ScalarReference,
            &mut scalar,
            &source,
            0.375,
        );
        multiply_add_constant(
            AudioKernelBackend::RuntimeVectorized,
            &mut vectorized,
            &source,
            0.375,
        );
        assert_fma_equivalent(&scalar, &vectorized);

        scalar.copy_from_slice(&source);
        vectorized.copy_from_slice(&source);
        multiply_in_place(AudioKernelBackend::ScalarReference, &mut scalar, &gains);
        multiply_in_place(
            AudioKernelBackend::RuntimeVectorized,
            &mut vectorized,
            &gains,
        );
        assert_eq!(scalar, vectorized);

        multiply_constant_in_place(AudioKernelBackend::ScalarReference, &mut scalar, 0.375);
        multiply_constant_in_place(
            AudioKernelBackend::RuntimeVectorized,
            &mut vectorized,
            0.375,
        );
        assert_eq!(scalar, vectorized);

        multiply_constant_into(
            AudioKernelBackend::ScalarReference,
            &mut scalar,
            &source,
            0.375,
        );
        multiply_constant_into(
            AudioKernelBackend::RuntimeVectorized,
            &mut vectorized,
            &source,
            0.375,
        );
        assert_eq!(scalar, vectorized);

        scalar.fill(0.5);
        vectorized.fill(0.5);
        add(AudioKernelBackend::ScalarReference, &mut scalar, &source);
        add(
            AudioKernelBackend::RuntimeVectorized,
            &mut vectorized,
            &source,
        );
        assert_eq!(scalar, vectorized);
    }

    #[test]
    fn runtime_vectorized_kernels_match_scalar_for_short_block_tails() {
        for len in 0..=64 {
            let source = (0..len).map(|index| index as f32 * 0.25 - 2.0).collect::<Vec<_>>();
            let gains = (0..len).map(|index| (index % 7) as f32 * 0.125).collect::<Vec<_>>();
            let initial = (0..len).map(|index| (index % 5) as f32 * -0.5).collect::<Vec<_>>();

            let mut scalar = initial.clone();
            let mut vectorized = initial.clone();
            multiply_into(
                AudioKernelBackend::ScalarReference,
                &mut scalar,
                &source,
                &gains,
            );
            multiply_into(
                AudioKernelBackend::RuntimeVectorized,
                &mut vectorized,
                &source,
                &gains,
            );
            assert_eq!(scalar, vectorized, "multiply-into length {len}");

            multiply_constant_into(
                AudioKernelBackend::ScalarReference,
                &mut scalar,
                &source,
                0.375,
            );
            multiply_constant_into(
                AudioKernelBackend::RuntimeVectorized,
                &mut vectorized,
                &source,
                0.375,
            );
            assert_eq!(scalar, vectorized, "multiply-constant length {len}");

            scalar.copy_from_slice(&source);
            vectorized.copy_from_slice(&source);
            multiply_in_place(AudioKernelBackend::ScalarReference, &mut scalar, &gains);
            multiply_in_place(
                AudioKernelBackend::RuntimeVectorized,
                &mut vectorized,
                &gains,
            );
            assert_eq!(scalar, vectorized, "multiply-in-place length {len}");

            multiply_constant_in_place(AudioKernelBackend::ScalarReference, &mut scalar, 0.375);
            multiply_constant_in_place(
                AudioKernelBackend::RuntimeVectorized,
                &mut vectorized,
                0.375,
            );
            assert_eq!(
                scalar, vectorized,
                "multiply-constant-in-place length {len}"
            );

            scalar.fill(0.5);
            vectorized.fill(0.5);
            add(AudioKernelBackend::ScalarReference, &mut scalar, &source);
            add(
                AudioKernelBackend::RuntimeVectorized,
                &mut vectorized,
                &source,
            );
            assert_eq!(scalar, vectorized, "add length {len}");
        }
    }
}
