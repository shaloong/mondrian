//! Real release cost evidence for changing complete Float32 Effect Mix frames.

use mondrian_core::ColorFrameAlpha;
use mondrian_effects::{
    mix_float_frames_controlled, EffectColorDomain, EffectFloatFrame, EffectFloatFrameError,
    EffectFrameExtent, EffectValueFormat, EffectWorkingPrecision,
};
use std::{hint::black_box, time::Instant};

#[derive(Clone, Copy)]
struct Pattern {
    left: [u32; 4],
    right: [u32; 4],
    expected: [u32; 4],
}
// Exact Fraction image evaluation + ordered-bit Float32 midpoint search.
// None of the pairs are equal, and amount is neither zero nor one.
const PATTERNS: [Pattern; 8] = [
    Pattern {
        left: [0xc0800000, 0x41000000, 0x3e800000, 0x3e800000],
        right: [0x40000000, 0xbf800000, 0x40e00000, 0x3f400000],
        expected: [0x330e38e3, 0x40000000, 0x40980000, 0x3ee66666],
    },
    Pattern {
        left: [0x41000000, 0xc0800000, 0x3e000000, 0x3f000000],
        right: [0xbf800000, 0x40400000, 0x40800000, 0x3e800000],
        expected: [0x40b80000, 0xc0100000, 0x3f8c0000, 0x3ecccccd],
    },
    Pattern {
        left: [0x3f800000, 0xbf800000, 0x42000000, 0x3f400000],
        right: [0xc1800000, 0x3f000000, 0xc2000000, 0x3e000000],
        expected: [0xbf333334, 0xbf59999a, 0x41cccccd, 0x3f000000],
    },
    Pattern {
        left: [0xc0000000, 0x41400000, 0x3c800000, 0x3e000000],
        right: [0x41000000, 0xc0000000, 0x41000000, 0x3f600000],
        expected: [0x40c78788, 0x3ef0f0ef, 0x40d2e969, 0x3ed9999a],
    },
    Pattern {
        left: [0x40000000, 0x40800000, 0xc1800000, 0x3f200000],
        right: [0xc0800000, 0x41800000, 0x3e800000, 0x3d800000],
        expected: [0x3fd00000, 0x40980000, 0xc16fc000, 0x3ecccccd],
    },
    Pattern {
        left: [0x3f000000, 0xc1000000, 0x42800000, 0x3d800000],
        right: [0x40400000, 0x3d800000, 0xc2800000, 0x3f200000],
        expected: [0x402b2164, 0xbf7d37a7, 0xc23d37a7, 0x3e933333],
    },
    Pattern {
        left: [0xc1800000, 0x3d800000, 0x40400000, 0x3f600000],
        right: [0x42000000, 0xc1000000, 0x3f800000, 0x3e800000],
        expected: [0xc1051eb8, 0xbf9d1eb9, 0x402b851f, 0x3f200000],
    },
    Pattern {
        left: [0x40800000, 0xc0400000, 0x3a800000, 0x3f100000],
        right: [0xc0400000, 0x41000000, 0x40000000, 0x3f500000],
        expected: [0x3f10e7d9, 0x40195bc6, 0x3f7b4c14, 0x3f29999a],
    },
];

#[derive(Debug, PartialEq, Eq)]
enum Stop {
    Canceled,
    Frame(EffectFloatFrameError),
}
impl From<EffectFloatFrameError> for Stop {
    fn from(value: EffectFloatFrameError) -> Self {
        Self::Frame(value)
    }
}

fn fill(base: &mut EffectFloatFrame, source: &mut EffectFloatFrame, phase: usize) {
    for (index, (left, right)) in base.pixels_mut().iter_mut().zip(source.pixels_mut()).enumerate()
    {
        let pattern = PATTERNS[(index + phase) % PATTERNS.len()];
        *left = pattern.left.map(f32::from_bits);
        *right = pattern.right.map(f32::from_bits);
    }
}
fn verify(base: &EffectFloatFrame, phase: usize) {
    assert_eq!(base.alpha(), ColorFrameAlpha::StraightCoverage);
    for (index, pixel) in base.pixels().iter().enumerate() {
        assert_eq!(
            pixel.map(f32::to_bits),
            PATTERNS[(index + phase) % PATTERNS.len()].expected,
            "independent exact expected bits at real pixel {index}"
        );
    }
}

#[test]
#[ignore = "meaningful HD/UHD release timing; run explicitly and retain the JSON cost evidence"]
fn mix_exact_changing_hd_uhd_release_cost_and_bounded_cancellation() {
    if cfg!(debug_assertions) {
        panic!("cost evidence requires --release");
    }
    let format = EffectValueFormat::new(
        EffectWorkingPrecision::Float32,
        EffectColorDomain::SceneLinearRgb,
        ColorFrameAlpha::StraightCoverage,
    );
    for (width, height) in [(1920_u32, 1080_u32), (3840, 2160)] {
        let extent = EffectFrameExtent::new(width, height);
        let count = width as usize * height as usize;
        let mut base = EffectFloatFrame::from_pixels(
            extent,
            format,
            mondrian_core::ImageSampling::UNIT_SQUARE,
            vec![[0.0; 4]; count],
        )
        .expect("actual base owner");
        let mut source = EffectFloatFrame::from_pixels(
            extent,
            format,
            mondrian_core::ImageSampling::UNIT_SQUARE,
            vec![[0.0; 4]; count],
        )
        .expect("actual source owner");
        let base_address = base.pixels().as_ptr();
        let source_address = source.pixels().as_ptr();
        let raster_bytes = count * std::mem::size_of::<[f32; 4]>();
        let block_checks = count.div_ceil(65_536);
        let mix_checks = count.div_ceil(4096);
        let expected_checks = 5 + 3 * block_checks + mix_checks;
        for iteration in 0..3 {
            let phase = iteration * 3;
            fill(&mut base, &mut source, phase); // prepare outside timed work
            let mut calls = 0;
            let start = Instant::now();
            mix_float_frames_controlled(
                black_box(&mut base),
                black_box(source.view()),
                black_box(f32::from_bits(0x3ecccccd)),
                &mut || {
                    calls += 1;
                    Ok::<(), Stop>(())
                },
            )
            .expect("real complete typed Mix");
            let elapsed = start.elapsed();
            black_box(base.pixels());
            verify(&base, phase); // independent full-raster correctness, outside timer
            assert_eq!(base.pixels().as_ptr(), base_address);
            assert_eq!(source.pixels().as_ptr(), source_address);
            assert_eq!(
                calls, expected_checks,
                "all admission/mix/output checkpoints included"
            );
            println!(
                "MIX_EXACT_COST {}",
                serde_json::json!({
                    "width":width,"height":height,"iteration":iteration,
                    "pixels":count,"milliseconds":elapsed.as_secs_f64()*1000.0,
                    "nanoseconds_per_pixel":elapsed.as_nanos() as f64/count as f64,
                    "full_checks":calls,"raster_owner_bytes":2*raster_bytes,
                    "additional_output_raster_bytes":0,
                    "independent_patterns":PATTERNS.len(),"equal_or_endpoint_shortcuts":false,
                    "frame_deadline_acceptance":false
                })
            );
        }
        fill(&mut base, &mut source, 1);
        // Cancel halfway through actual Mix, after both complete admissions.
        let cancel_at = 1 + 2 * (block_checks + 1) + (mix_checks / 2).max(1);
        let mut calls = 0;
        let start = Instant::now();
        let result = mix_float_frames_controlled(
            &mut base,
            source.view(),
            f32::from_bits(0x3ecccccd),
            &mut || {
                calls += 1;
                if calls == cancel_at {
                    Err(Stop::Canceled)
                } else {
                    Ok(())
                }
            },
        );
        let elapsed = start.elapsed();
        assert_eq!(result, Err(Stop::Canceled));
        assert_eq!(
            calls, cancel_at,
            "no work/checks after the actual typed cancellation"
        );
        assert_eq!(base.pixels().as_ptr(), base_address);
        assert_eq!(source.pixels().as_ptr(), source_address);
        println!(
            "MIX_EXACT_CANCEL {}",
            serde_json::json!({
                "width":width,"height":height,"cancel_at":cancel_at,"observed_checks":calls,
                "milliseconds":elapsed.as_secs_f64()*1000.0,
                "complete_frame_published":false,"mix_pixels_between_checks":4096,
                "admission_pixels_between_checks":65_536
            })
        );
    }
}
