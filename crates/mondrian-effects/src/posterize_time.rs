//! Posterize time: hold each output on a reduced frame rate.
//!
//! This effect has no pixel kernel at all. Its whole behaviour is one temporal
//! read: the output instant is quantized onto the authored frame rate, and the
//! graph samples whatever the effect already produced at that earlier instant:
//!
//! ```text
//! R            = clamp(frames_per_second, 0.009995, 999)   authored rate
//! step_ticks   = 1e6 / R                                   one held step
//! now_ticks    = round(t * 1e6)                            output instant
//! k(t)         = floor(now_ticks / step_ticks + snap)      integer step index
//! held_ticks   = k(t) * step_ticks                         held instant
//! offset       = held_ticks - now_ticks                    <= 0, history request
//! ```
//!
//! Both instants live on the same microsecond grid, so the offset is an exact
//! tick difference rather than the difference of two rounded projections. If
//! that difference is zero the builder emits **no node at all**: the graph stays
//! the pass-through chain, so an already-quantized timeline costs nothing and
//! cannot drift.
//!
//! # Why the read is one `TemporalFrameBlend`
//!
//! [`crate::EffectRenderOp::TemporalFrameBlend`] is the existing carrier for
//! "evaluate my input at another instant and hand the pixels back". Echo uses
//! it per tap with `mix == 1.0` as well, where a mix of one is exactly the
//! straight-alpha copy this effect needs, so the finite temporal executor, the
//! device schedule and the Export path all apply without a second operation
//! kind. Posterize time therefore adds **no** render op: only the effect type,
//! its authoring surface and its temporal contract.
//!
//! # Held, not averaged
//!
//! `mix == 1.0` is the whole semantics: the held instant *replaces* the current
//! one. Nothing in this module blends two instants; averaging neighbouring
//! frames would be motion blur, which is a different effect with its own
//! shutter controls.
//!
//! # Idempotence and the snap
//!
//! `q` is a projection onto the grid `1/R`, so applying it twice equals
//! applying it once; the tests pin that property because it is what makes the
//! history request stable when the executor rebuilds the graph at the sampled
//! instant. The `snap` term exists so that an instant which is arithmetically
//! on the grid is not pushed a whole step back by floating-point error: a
//! value within three quarters of one microsecond tick of the grid counts as
//! on the grid, and a relative `1e-9` allowance covers very large indices.
//! Without the snap, a timeline whose own frame rate divides the posterize rate
//! would flicker between "held" and "one step back" depending on rounding
//! noise, and quantizing the instant the effect itself produced would slide a
//! whole step backwards.
//!
//! # Bounds
//!
//! The rate range is the authored one (`0.009995..=999`), so the largest
//! possible lookback is just under `1 / 0.009995` seconds, about 100.05 s. The
//! author-time temporal extent is derived from the authored rate and is always
//! finite; a temporal effect that declares [`crate::EffectTemporalInputExtent::UNBOUNDED`]
//! is rejected by the finite executor, so this module never leaves it unbounded.

use mondrian_core::TimelineTime;

/// Quantization grid for authored seconds, matching Echo's input boundary.
pub const POSTERIZE_TIME_TIMESCALE: u32 = 1_000_000;

/// Slowest authored rate, in output steps per second.
pub const POSTERIZE_TIME_MIN_RATE: f64 = 0.009_995;

/// Fastest authored rate, in output steps per second.
pub const POSTERIZE_TIME_MAX_RATE: f64 = 999.0;

/// Authored default: one held step per 24th of a second.
pub const POSTERIZE_TIME_DEFAULT_RATE: f64 = 24.0;

/// Relative tolerance under which a scaled instant counts as on the grid.
const POSTERIZE_TIME_SNAP: f64 = 1.0e-9;

/// A validated posterize-time rate.
///
/// The only state is the authored rate; every other value is derived from it
/// and from the instant being evaluated, so the type is `Copy` and cannot drift
/// between the graph builder, the contract and the tests.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PosterizeTimeGrade {
    frames_per_second: f64,
}

impl PosterizeTimeGrade {
    /// Validate an authored rate.
    pub fn new(frames_per_second: f64) -> Result<Self, PosterizeTimeError> {
        if !frames_per_second.is_finite() {
            return Err(PosterizeTimeError::NonFiniteRate { frames_per_second });
        }
        if !(POSTERIZE_TIME_MIN_RATE..=POSTERIZE_TIME_MAX_RATE).contains(&frames_per_second) {
            return Err(PosterizeTimeError::RateOutOfRange { frames_per_second });
        }
        Ok(Self { frames_per_second })
    }

    /// The authored rate, in held steps per second.
    pub const fn frames_per_second(self) -> f64 {
        self.frames_per_second
    }

    /// One held step on the quantization grid.
    ///
    /// The duration is rounded *up* onto the tick grid so that it always covers
    /// [`Self::sample_offset`] for every instant; a rounded-down step would
    /// leave the last tick of a held span outside the declared extent and the
    /// executor would refuse the sample.
    pub fn step(self) -> Result<TimelineTime, PosterizeTimeError> {
        let ticks = f64::from(POSTERIZE_TIME_TIMESCALE) / self.frames_per_second;
        let rounded = ticks.ceil();
        if !rounded.is_finite() || rounded < 1.0 || rounded > i64::MAX as f64 {
            return Err(PosterizeTimeError::StepOverflow {
                frames_per_second: self.frames_per_second,
            });
        }
        TimelineTime::new(rounded as i64, i64::from(POSTERIZE_TIME_TIMESCALE)).map_err(|_| {
            PosterizeTimeError::StepOverflow { frames_per_second: self.frames_per_second }
        })
    }

    /// The integer held step that owns `now`.
    pub fn step_index(self, now: TimelineTime) -> i64 {
        let step_ticks = f64::from(POSTERIZE_TIME_TIMESCALE) / self.frames_per_second;
        let now_ticks = now.to_f64() * f64::from(POSTERIZE_TIME_TIMESCALE);
        if !step_ticks.is_finite() || step_ticks <= 0.0 || !now_ticks.is_finite() {
            return i64::MIN;
        }
        snapped_step_index(now_ticks / step_ticks, step_ticks)
    }

    /// The signed history offset the graph must sample.
    ///
    /// The result is always `<= 0`: this effect only ever reads the past. A
    /// positive result would demand a future frame, which the executor must not
    /// invent, so such an instant is reported as "on the grid" and yields zero.
    pub fn sample_offset(self, now: TimelineTime) -> Result<TimelineTime, PosterizeTimeError> {
        let step_ticks = f64::from(POSTERIZE_TIME_TIMESCALE) / self.frames_per_second;
        let now_ticks = now.to_f64() * f64::from(POSTERIZE_TIME_TIMESCALE);
        if !step_ticks.is_finite() || step_ticks < 1.0 || !now_ticks.is_finite() {
            return Ok(TimelineTime::ZERO);
        }
        let index = snapped_step_index(now_ticks / step_ticks, step_ticks);
        if index == i64::MIN {
            return Ok(TimelineTime::ZERO);
        }
        let held_ticks = index as f64 * step_ticks;
        if !held_ticks.is_finite() || held_ticks < i64::MIN as f64 || held_ticks > i64::MAX as f64 {
            return Err(PosterizeTimeError::StepOverflow {
                frames_per_second: self.frames_per_second,
            });
        }
        let delta = held_ticks.round() - now_ticks.round();
        if delta >= 0.0 || delta < i64::MIN as f64 {
            return Ok(TimelineTime::ZERO);
        }
        TimelineTime::new(delta as i64, i64::from(POSTERIZE_TIME_TIMESCALE)).map_err(|_| {
            PosterizeTimeError::StepOverflow { frames_per_second: self.frames_per_second }
        })
    }
}

/// Floor a scaled instant onto the step grid, snapping near-integers.
///
/// A held instant is stored as a whole microsecond tick, so it can sit up to
/// half a tick below the ideal grid point. The snap therefore allows three
/// quarters of one tick in scaled units, plus a relative term for very large
/// indices, before a value counts as off the grid. Without the tick allowance
/// the projection would not be idempotent: quantizing an instant the effect
/// itself just produced would slide one whole step backwards.
fn snapped_step_index(scaled: f64, step_ticks: f64) -> i64 {
    if !scaled.is_finite() {
        return i64::MIN;
    }
    let nearest = scaled.round();
    let tick_tolerance = 0.75 / step_ticks;
    let tolerance = tick_tolerance + POSTERIZE_TIME_SNAP * nearest.abs().max(1.0);
    let snapped = if (scaled - nearest).abs() <= tolerance {
        nearest
    } else {
        scaled
    };
    let floored = snapped.floor();
    if floored < i64::MIN as f64 {
        i64::MIN
    } else if floored > i64::MAX as f64 {
        i64::MAX
    } else {
        floored as i64
    }
}

/// Why an authored posterize-time rate cannot be honoured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PosterizeTimeError {
    /// The authored rate is not a finite number.
    NonFiniteRate { frames_per_second: f64 },
    /// The authored rate is outside the declared authoring range.
    RateOutOfRange { frames_per_second: f64 },
    /// One held step cannot be represented on the quantization grid.
    StepOverflow { frames_per_second: f64 },
}

impl std::fmt::Display for PosterizeTimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonFiniteRate { .. } => {
                write!(formatter, "抽帧的帧速率必须是有限数值")
            }
            Self::RateOutOfRange { frames_per_second } => write!(
                formatter,
                "抽帧的帧速率 {frames_per_second} 超出 0.009995 到 999 的范围"
            ),
            Self::StepOverflow { .. } => write!(formatter, "抽帧的保持时长超出可表示范围"),
        }
    }
}

impl std::error::Error for PosterizeTimeError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(numerator: i64, denominator: i64) -> TimelineTime {
        TimelineTime::new(numerator, denominator).expect("test instant")
    }

    fn grade(rate: f64) -> PosterizeTimeGrade {
        PosterizeTimeGrade::new(rate).expect("declared rate")
    }

    #[test]
    fn the_authored_range_is_exactly_the_declared_one() {
        assert!(PosterizeTimeGrade::new(POSTERIZE_TIME_MIN_RATE).is_ok());
        assert!(PosterizeTimeGrade::new(POSTERIZE_TIME_MAX_RATE).is_ok());
        assert_eq!(
            PosterizeTimeGrade::new(0.0),
            Err(PosterizeTimeError::RateOutOfRange { frames_per_second: 0.0 })
        );
        assert_eq!(
            PosterizeTimeGrade::new(999.001),
            Err(PosterizeTimeError::RateOutOfRange { frames_per_second: 999.001 })
        );
        assert!(matches!(
            PosterizeTimeGrade::new(f64::NAN),
            Err(PosterizeTimeError::NonFiniteRate { .. })
        ));
        assert_eq!(grade(POSTERIZE_TIME_DEFAULT_RATE).frames_per_second(), 24.0);
    }

    #[test]
    fn an_instant_on_the_grid_asks_for_nothing() {
        for rate in [24.0, 25.0, 30.0, 60.0] {
            let grade = grade(rate);
            for index in 0..16 {
                let instant = at(index, rate as i64);
                assert!(
                    grade.sample_offset(instant).expect("offset").is_zero(),
                    "instant {index}/{rate} is on the grid and must be a pass-through"
                );
            }
        }
    }

    #[test]
    fn an_instant_off_the_grid_reads_backwards() {
        let rate = grade(24.0);
        // 0.05 s is 1.2 steps at 24 fps, so the held instant is step 1.
        let instant = at(1, 20);
        let offset = rate.sample_offset(instant).expect("offset");
        assert!(offset.is_negative(), "held instants are always in the past");
        let held = instant.checked_add(offset).expect("held instant");
        let expected = 1.0 / 24.0;
        assert!(
            (held.to_f64() - expected).abs() <= f64::from(POSTERIZE_TIME_TIMESCALE).recip(),
            "held instant {} must land on step 1 ({expected})",
            held.to_f64()
        );
        // The held instant is itself a pass-through.
        assert!(rate.sample_offset(held).expect("offset").is_zero());
    }

    #[test]
    fn the_quantization_is_a_projection() {
        for rate in [0.009_995, 1.0, 23.976, 24.0, 29.97, 60.0, 999.0] {
            let grade = grade(rate);
            for tick in 0..24 {
                let now = at(tick * 37, 7);
                let offset = grade.sample_offset(now).expect("offset");
                assert!(offset.is_negative() || offset.is_zero());
                let held = now.checked_add(offset).expect("held instant");
                assert!(
                    grade.sample_offset(held).expect("offset").is_zero(),
                    "quantizing the held instant at {rate} fps must be a pass-through"
                );
            }
        }
    }

    #[test]
    fn the_declared_extent_always_covers_the_computed_offset() {
        for rate in [0.009_995, 1.0, 23.976, 24.0, 29.97, 24000.0 / 1001.0, 999.0] {
            let grade = grade(rate);
            let step = grade.step().expect("step").to_f64();
            assert!(step > 0.0, "a held step is never zero");
            for tick in 0..64 {
                let now = at(tick * 13, 11);
                let offset = grade.sample_offset(now).expect("offset");
                let distance = offset.to_f64().abs();
                assert!(
                    distance <= step + f64::from(POSTERIZE_TIME_TIMESCALE).recip(),
                    "lookback {distance} must not exceed the declared step {step}"
                );
            }
        }
    }

    #[test]
    fn the_slowest_rate_holds_for_about_a_hundred_seconds() {
        let slowest = grade(POSTERIZE_TIME_MIN_RATE).step().expect("step");
        assert!((slowest.to_f64() - 100.050_029).abs() <= 1.0e-5);
        let fastest = grade(POSTERIZE_TIME_MAX_RATE).step().expect("step");
        assert!((fastest.to_f64() - 0.001_002).abs() <= 1.0e-9);
        let default = grade(POSTERIZE_TIME_DEFAULT_RATE).step().expect("step");
        assert!((default.to_f64() - 0.041_667).abs() <= 1.0e-9);
    }

    #[test]
    fn the_snap_keeps_an_exact_boundary_on_the_grid() {
        let rate = grade(30.0);
        // 0.7 s is exactly 21 steps at 30 fps, but 0.7 * 30 is 20.999999...
        let instant = at(7, 10);
        assert!(rate.sample_offset(instant).expect("offset").is_zero());
        // A tenth of a tick later is not on the grid.
        let nudged = at(7001, 10_000);
        assert!(rate.sample_offset(nudged).expect("offset").is_negative());
    }

    #[test]
    fn evaluation_is_deterministic() {
        let rate = grade(23.976);
        let instant = at(123_457, 1000);
        let first = rate.sample_offset(instant).expect("offset");
        let second = rate.sample_offset(instant).expect("offset");
        assert_eq!(first, second);
        assert_eq!(rate.step_index(instant), grade(23.976).step_index(instant));
    }
}
