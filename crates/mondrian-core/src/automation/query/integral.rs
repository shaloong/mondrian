//! Bounded exact summation of Float64 products for signed curve areas.
//!
//! Two finite Float64 significands multiply into at most 106 bits. Keeping
//! their binary exponents until final rounding avoids intermediate overflow,
//! underflow and cancellation loss. Storage is fixed, independent of key count.

const WORDS: usize = 68;
const BASE_EXPONENT: i32 = -2148;
const SIGN: u64 = 1 << 63;
const FRACTION: u64 = (1 << 52) - 1;

pub(super) struct IntegralAccumulator {
    positive: [u64; WORDS],
    negative: [u64; WORDS],
}

impl IntegralAccumulator {
    pub(super) fn empty() -> Self {
        Self { positive: [0; WORDS], negative: [0; WORDS] }
    }

    pub(super) fn add_product(&mut self, a: f64, b: f64, subtract: bool) {
        if a == 0.0 || b == 0.0 {
            return;
        }
        debug_assert!(a.is_finite() && b.is_finite());
        let (a_significand, a_exponent) = split(a);
        let (b_significand, b_exponent) = split(b);
        let mut product = u128::from(a_significand) * u128::from(b_significand);
        let shift = (a_exponent + b_exponent - BASE_EXPONENT) as usize;
        let mut word = shift / 64;
        let offset = shift % 64;
        let negative = ((a.to_bits() ^ b.to_bits()) & SIGN != 0) ^ subtract;
        let words = if negative {
            &mut self.negative
        } else {
            &mut self.positive
        };
        while product != 0 {
            let part = product as u64;
            add_word(words, word, part << offset);
            if offset != 0 {
                add_word(words, word + 1, part >> (64 - offset));
            }
            word += 1;
            product >>= 64;
        }
    }

    pub(super) fn finish(self) -> Option<f64> {
        let comparison = self.positive.iter().rev().cmp(self.negative.iter().rev());
        let (mut magnitude, smaller, sign) = match comparison {
            std::cmp::Ordering::Equal => return Some(0.0),
            std::cmp::Ordering::Greater => (self.positive, self.negative, 0),
            std::cmp::Ordering::Less => (self.negative, self.positive, SIGN),
        };
        let mut borrow = false;
        for index in 0..WORDS {
            let (difference, first_borrow) = magnitude[index].overflowing_sub(smaller[index]);
            let (difference, second_borrow) = difference.overflowing_sub(u64::from(borrow));
            magnitude[index] = difference;
            borrow = first_borrow || second_borrow;
        }
        let last = magnitude.iter().rposition(|word| *word != 0)?;
        let mut top = last * 64 + (63 - magnitude[last].leading_zeros() as usize);
        let normal = top as i32 + BASE_EXPONENT >= -1022;
        let shift = if normal {
            top - 52
        } else {
            (-1074 - BASE_EXPONENT) as usize
        };
        let mut significand = shifted_word(&magnitude, shift);
        let guard = bit(&magnitude, shift - 1);
        let sticky = any_below(&magnitude, shift - 1);
        if guard && (sticky || significand & 1 != 0) {
            significand += 1;
        }
        if !normal {
            return Some(f64::from_bits(sign | significand));
        }
        if significand == 1 << 53 {
            significand >>= 1;
            top += 1;
        }
        let exponent = top as i32 + BASE_EXPONENT;
        if exponent > 1023 {
            return None;
        }
        let bits = sign | (((exponent + 1023) as u64) << 52) | (significand & FRACTION);
        Some(f64::from_bits(bits))
    }
}

fn split(value: f64) -> (u64, i32) {
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    if exponent == 0 {
        (bits & FRACTION, -1074)
    } else {
        ((bits & FRACTION) | (1 << 52), exponent - 1075)
    }
}

fn add_word(words: &mut [u64; WORDS], mut index: usize, mut value: u64) {
    while value != 0 {
        let (sum, carry) = words[index].overflowing_add(value);
        words[index] = sum;
        value = u64::from(carry);
        index += 1;
    }
}

fn shifted_word(words: &[u64; WORDS], shift: usize) -> u64 {
    let index = shift / 64;
    let offset = shift % 64;
    let lower = words[index] >> offset;
    if offset == 0 {
        lower
    } else {
        lower | words.get(index + 1).copied().unwrap_or(0) << (64 - offset)
    }
}

fn bit(words: &[u64; WORDS], index: usize) -> bool {
    words[index / 64] & (1 << (index % 64)) != 0
}

fn any_below(words: &[u64; WORDS], index: usize) -> bool {
    let word = index / 64;
    words[..word].iter().any(|value| *value != 0)
        || words[word] & ((1_u64 << (index % 64)) - 1) != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn exact_product_rounding_matches_ieee_at_range_edges() {
        for a in [
            f64::MAX,
            f64::MIN_POSITIVE,
            f64::from_bits(1),
            -f64::from_bits(1),
            1e-300,
            1.0,
            -1.0,
        ] {
            for b in [0.5, 1.5, 2.0, -3.0] {
                let mut sum = IntegralAccumulator::empty();
                sum.add_product(a, b, false);
                let expected = a * b;
                if expected.is_finite() {
                    assert_eq!(
                        sum.finish().expect("finite product").to_bits(),
                        expected.to_bits(),
                        "{a} * {b}"
                    );
                } else {
                    assert!(sum.finish().is_none());
                }
            }
        }
    }

    #[test]
    fn overflowing_products_cancel_without_losing_small_remainder() {
        let mut sum = IntegralAccumulator::empty();
        sum.add_product(f64::MAX, 2.0, false);
        sum.add_product(1e-300, 1.0, false);
        sum.add_product(f64::MAX, 2.0, true);
        assert_eq!(sum.finish(), Some(1e-300));
    }

    proptest! {
        #[test]
        fn product_plus_value_matches_independent_fused_rounding(a in any::<u64>(), b in any::<u64>(), c in any::<u64>()) {
            let (a, b, c) = (f64::from_bits(a), f64::from_bits(b), f64::from_bits(c));
            prop_assume!(a.is_finite() && b.is_finite() && c.is_finite());
            let mut sum = IntegralAccumulator::empty();
            sum.add_product(a, b, false);
            sum.add_product(c, 1.0, false);
            let expected = a.mul_add(b, c);
            if expected.is_finite() {
                let actual = sum.finish().expect("finite fused result");
                if expected == 0.0 { prop_assert_eq!(actual, expected); }
                else { prop_assert_eq!(actual.to_bits(), expected.to_bits()); }
            } else { prop_assert!(sum.finish().is_none()); }
        }
    }
}
