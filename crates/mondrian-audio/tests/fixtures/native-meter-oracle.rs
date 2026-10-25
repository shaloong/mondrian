// Test-only facts shared by the two owned DLLs, never by the host/PCM oracle.
// Fixed reference: quarter zero = 2/rate seconds, quarter duration = 3/4 sec,
// meter = 7/8. All admitted integers are exactly representable in binary64;
// Q uses one final division, independently of the host's wide-ratio algorithm.

#[derive(Debug, Clone, Copy)]
pub struct Expected {
    pub quarter_numerator: i64,
    pub quarter_denominator: i64,
    pub bar: i32,
}

pub fn expected(sample: i64, rate: u32) -> Option<Expected> {
    if !matches!(rate, 44_100 | 48_000) {
        return None;
    }
    let boundary = 2 + 21 * i64::from(rate) / 8;
    if !(-8..=40).contains(&sample) && !(boundary - 16..=boundary + 40).contains(&sample) {
        return None;
    }
    Some(Expected {
        quarter_numerator: (sample - 2) * 4,
        quarter_denominator: i64::from(rate) * 3,
        bar: i32::try_from((8 * i128::from(sample - 2)).div_euclid(21 * i128::from(rate))).ok()?,
    })
}

/// Start from the actual native bar observation, then advance by exact samples.
/// Callers independently validate that observation before touching native PCM.
pub fn pcm_tag(start: i64, sample: i64, rate: u32, actual_bar: i32, signature: f32) -> Option<f32> {
    let delta = expected(sample, rate)?.bar - expected(start, rate)?.bar;
    Some((actual_bar + delta) as f32 / 16.0 + signature)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_meter_facts_match_independent_fraction_literals() {
        // Python Fraction, followed by one float() / fixed-point conversion.
        for (sample, rate, bar, ticks, quarter_bits) in [
            (0, 48_000, -1, -119_305, 0xbf0d_208a_5a91_2e32),
            (2, 48_000, 0, 0, 0),
            (126_002, 48_000, 1, 7_516_192_768, 0x400c_0000_0000_0000),
            (0, 44_100, -1, -129_855, 0xbf0f_b3f6_5f17_ec9e),
            (115_764, 44_100, 0, 7_516_160_304, 0x400b_fff8_1302_683a),
            (115_765, 44_100, 1, 7_516_225_232, 0x400c_0007_ecfd_97c6),
        ] {
            let value = expected(sample, rate).unwrap();
            let scaled = i128::from(value.quarter_numerator) * (1_i128 << 31);
            let denominator = i128::from(value.quarter_denominator);
            let actual_ticks = (scaled.abs() + denominator / 2) / denominator * scaled.signum();
            let bits = (value.quarter_numerator as f64 / value.quarter_denominator as f64).to_bits();
            assert_eq!(
                (value.bar, actual_ticks as i64, bits),
                (bar, ticks, quarter_bits)
            );
        }
        assert!(expected(0, 96_000).is_none());
        assert!(expected(i64::MAX, 48_000).is_none());
    }
}
