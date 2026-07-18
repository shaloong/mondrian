//! Equivalent scalar-reference and runtime-vectorized PCM kernels.

use crate::AudioKernelBackend;
use pulp::{Arch, Simd, WithSimd};

pub(crate) fn multiply_add(
    backend: AudioKernelBackend,
    destination: &mut [f32],
    source: &[f32],
    gains: &[f32],
) {
    debug_assert_eq!(destination.len(), source.len());
    debug_assert_eq!(destination.len(), gains.len());
    match backend {
        AudioKernelBackend::ScalarReference => {
            multiply_add_scalar(destination, source, gains);
        }
        AudioKernelBackend::RuntimeVectorized => {
            Arch::new().dispatch(MultiplyAdd { destination, source, gains });
        }
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

pub(crate) fn add(backend: AudioKernelBackend, destination: &mut [f32], source: &[f32]) {
    debug_assert_eq!(destination.len(), source.len());
    match backend {
        AudioKernelBackend::ScalarReference => add_scalar(destination, source),
        AudioKernelBackend::RuntimeVectorized => {
            Arch::new().dispatch(Add { destination, source });
        }
    }
}

fn multiply_add_scalar(destination: &mut [f32], source: &[f32], gains: &[f32]) {
    for ((destination, source), gain) in destination.iter_mut().zip(source).zip(gains) {
        *destination = source.mul_add(*gain, *destination);
    }
}

fn multiply_into_scalar(destination: &mut [f32], source: &[f32], gains: &[f32]) {
    for ((destination, source), gain) in destination.iter_mut().zip(source).zip(gains) {
        *destination = *source * *gain;
    }
}

fn add_scalar(destination: &mut [f32], source: &[f32]) {
    for (destination, source) in destination.iter_mut().zip(source) {
        *destination += *source;
    }
}

struct MultiplyAdd<'a> {
    destination: &'a mut [f32],
    source: &'a [f32],
    gains: &'a [f32],
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

struct MultiplyInto<'a> {
    destination: &'a mut [f32],
    source: &'a [f32],
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

struct MultiplyConstant<'a> {
    destination: &'a mut [f32],
    source: &'a [f32],
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_vectorized_kernels_match_scalar_reference() {
        let source = (0..1031).map(|index| index as f32 * 0.03125 - 7.0).collect::<Vec<_>>();
        let gains = (0..1031).map(|index| (index % 23) as f32 * 0.017).collect::<Vec<_>>();
        let initial = (0..1031).map(|index| (index % 11) as f32 * -0.125).collect::<Vec<_>>();

        let mut scalar = initial.clone();
        let mut vectorized = initial;
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
        assert_eq!(scalar, vectorized);

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
}
