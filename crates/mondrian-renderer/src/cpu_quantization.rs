//! Exact terminal encoded-float to RGBA8 conversion, shared by CPU outputs.
//!
//! This is representation conversion only: OCIO owns the preceding color math.
//! No scheduling, additional frame scratch, or process-global workers belong here.

pub(crate) fn quantize_rgba8(pixels: &[[f32; 4]]) -> Vec<u8> {
    let channels = pixels.as_flattened();
    let mut output = vec![0; channels.len()];
    quantize_into(channels, &mut output);
    output
}

fn quantize_scalar(input: &[f32], output: &mut [u8]) {
    for (output, channel) in output.iter_mut().zip(input) {
        *output = (channel.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn quantize_into(input: &[f32], output: &mut [u8]) {
    debug_assert_eq!(input.len(), output.len());
    quantize_scalar(input, output);
}

#[cfg(target_arch = "x86_64")]
fn quantize_into(input: &[f32], output: &mut [u8]) {
    use std::arch::x86_64::*;

    debug_assert_eq!(input.len(), output.len());
    let mut sources = input.chunks_exact(16);
    let mut destinations = output.chunks_exact_mut(16);
    // SAFETY: SSE2 is part of the x86_64 baseline. Every load below addresses
    // four f32s within a complete 16-channel source chunk, and each store writes
    // exactly one complete 16-byte destination chunk. Unaligned access is
    // intentional. Source and destination borrow different element buffers;
    // all incomplete chunks remain in the safe scalar tail.
    unsafe {
        let zero = _mm_setzero_ps();
        let one = _mm_set1_ps(1.0);
        let scale = _mm_set1_ps(255.0);
        let half = _mm_set1_ps(0.5);
        let increment = _mm_set1_epi32(1);
        for (source, destination) in sources.by_ref().zip(destinations.by_ref()) {
            let quantize = |offset| {
                // MAXPS selects its second operand for NaN. Map it to zero,
                // exactly the byte produced by the canonical Rust u8 cast.
                let bounded = _mm_min_ps(
                    _mm_max_ps(_mm_loadu_ps(source.as_ptr().add(offset)), zero),
                    one,
                );
                let scaled = _mm_mul_ps(bounded, scale);
                let base = _mm_cvttps_epi32(scaled);
                // base is an exact integer in [0,255], so base + 0.5 is exact.
                // Comparing against it implements ties-away without the
                // premature rounding that adding 0.5 to scaled would cause.
                let threshold = _mm_add_ps(_mm_cvtepi32_ps(base), half);
                let round_up =
                    _mm_and_si128(_mm_castps_si128(_mm_cmpge_ps(scaled, threshold)), increment);
                _mm_add_epi32(base, round_up)
            };
            let low = _mm_packs_epi32(quantize(0), quantize(4));
            let high = _mm_packs_epi32(quantize(8), quantize(12));
            _mm_storeu_si128(destination.as_mut_ptr().cast(), _mm_packus_epi16(low, high));
        }
    }
    quantize_scalar(sources.remainder(), destinations.into_remainder());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge_inputs() -> Vec<f32> {
        let mut inputs = vec![
            0.0,
            -0.0,
            1.0,
            -1.0,
            1.5,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::MAX,
            f32::MIN,
            f32::MIN_POSITIVE,
        ];
        for bits in [
            1,
            0x8000_0001,
            0x007f_ffff,
            0x807f_ffff,
            0x7fc1_2345,
            0xffc1_2345,
            0x7f81_2345,
            0xff81_2345,
        ] {
            inputs.push(f32::from_bits(bits));
        }
        for code in 0..255 {
            let center = ((code as f32 + 0.5) / 255.0).to_bits();
            for bits in center - 8..=center + 8 {
                inputs.push(f32::from_bits(bits));
            }
        }
        inputs
    }

    fn assert_canonical(input: &[f32]) {
        let mut expected = vec![0; input.len()];
        quantize_scalar(input, &mut expected);
        let mut actual = vec![0; input.len()];
        quantize_into(input, &mut actual);
        assert_eq!(actual, expected);
    }

    #[test]
    fn all_half_code_neighbors_special_values_and_random_bits_match_scalar() {
        let mut inputs = edge_inputs();
        let mut rng = 0x1234_5678_u32;
        for _ in 0..1_000_000 {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            inputs.push(f32::from_bits(rng));
        }
        assert_canonical(&inputs);
    }

    #[test]
    fn input_corpus_rejects_add_half_then_truncate_shortcut() {
        assert!(edge_inputs().iter().any(|channel| {
            let scaled = channel.clamp(0.0, 1.0) * 255.0;
            scaled.round() as u8 != (scaled + 0.5) as u8
        }));
    }

    #[test]
    fn every_tail_alignment_and_output_guard_is_preserved() {
        let inputs = edge_inputs();
        for offset in 0..16 {
            for length in 0..81 {
                let input = &inputs[offset..offset + length];
                let before: Vec<u32> = input.iter().map(|value| value.to_bits()).collect();
                let mut guarded = vec![0xa5; length + 32];
                quantize_into(input, &mut guarded[offset..offset + length]);
                let mut expected = vec![0; length];
                quantize_scalar(input, &mut expected);
                assert_eq!(&guarded[offset..offset + length], expected);
                assert!(guarded[..offset].iter().all(|byte| *byte == 0xa5));
                assert!(guarded[offset + length..].iter().all(|byte| *byte == 0xa5));
                assert_eq!(
                    input.iter().map(|value| value.to_bits()).collect::<Vec<_>>(),
                    before
                );
            }
        }
    }

    #[test]
    fn packed_rgba_preserves_channel_order_and_exact_extent() {
        let input = edge_inputs();
        let pixels: Vec<[f32; 4]> =
            input.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).collect();
        for length in [0, 1, 2, 3, 4, 5, 17, pixels.len()] {
            let result = quantize_rgba8(&pixels[..length]);
            let mut expected = vec![0; length * 4];
            quantize_scalar(pixels[..length].as_flattened(), &mut expected);
            assert_eq!(result, expected);
        }
    }

    #[test]
    #[ignore = "isolated quantization attribution only, not an App performance gate"]
    fn original_loop_and_production_quantization_probe() {
        use std::{hint::black_box, time::Instant};
        let pixels: Vec<[f32; 4]> = (0..960 * 540)
            .map(|pixel| {
                std::array::from_fn(|channel| {
                    ((pixel * 4 + channel) % 1013) as f32 / 1000.0 - 0.006
                })
            })
            .collect();
        let mut samples = [Vec::new(), Vec::new()];
        for run in 0..40 {
            for rotation in 0..2 {
                let index = (rotation + run) % 2;
                let input = black_box(&pixels);
                let started = Instant::now();
                let output = if index == 0 {
                    let mut output = Vec::with_capacity(input.len() * 4);
                    for pixel in input {
                        for channel in pixel {
                            output.push((channel.clamp(0.0, 1.0) * 255.0).round() as u8);
                        }
                    }
                    output
                } else {
                    quantize_rgba8(input)
                };
                black_box(&output);
                samples[index].push(started.elapsed().as_micros());
            }
        }
        for (label, mut samples) in ["original", "production"].into_iter().zip(samples) {
            samples.sort_unstable();
            eprintln!(
                "{label}: min={}us median={}us max={}us",
                samples[0], samples[20], samples[39]
            );
        }
    }
}
