//! Bind CPAL's CoreAudio host timestamps to the actual callback observation.
//!
//! CoreAudio supplies the render cycle's host time, which can lie in the future
//! relative to callback entry. Subtracting playback - callback and attaching
//! that delay to `Instant::now()` loses that lead. Both native timestamps and
//! `mach_absolute_time` use the same clock; sample it instead of assuming the
//! supplied callback timestamp names the current instant.

use cpal::{Error, ErrorKind, OutputCallbackInfo, OutputStreamTimestamp, StreamInstant};
use std::time::{Duration, Instant};

#[repr(C)]
#[derive(Default)]
struct MachTimebase {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_timebase_info(info: *mut MachTimebase) -> i32;
    fn mach_absolute_time() -> u64;
}

pub(super) struct CoreAudioHostClock {
    timebase: MachTimebase,
}

impl CoreAudioHostClock {
    pub(super) fn new() -> Result<Self, Error> {
        let mut timebase = MachTimebase::default();
        // Public mach API; `timebase` has the SDK's mach_timebase_info_data_t ABI.
        let status = unsafe { mach_timebase_info(&mut timebase) };
        if status != 0 || timebase.numer == 0 || timebase.denom == 0 {
            return Err(Error::with_message(
                ErrorKind::BackendError,
                format!("CoreAudio host clock timebase unavailable: status={status}"),
            ));
        }
        Ok(Self { timebase })
    }

    pub(super) fn observe_tail_delay(
        &self,
        info: &OutputCallbackInfo,
        callback_span: Duration,
        observed_at: Instant,
    ) -> Option<(Duration, Duration)> {
        // CPAL uses this same integer conversion for its CoreAudio StreamInstant.
        let ticks = unsafe { mach_absolute_time() };
        let nanos =
            u128::from(ticks) * u128::from(self.timebase.numer) / u128::from(self.timebase.denom);
        let native_now = StreamInstant::new(
            u64::try_from(nanos / 1_000_000_000).ok()?,
            (nanos % 1_000_000_000) as u32,
        );
        let query_span = Instant::now().checked_duration_since(observed_at)?;
        tail_delay(info.timestamp(), native_now, callback_span, query_span)
    }
}

fn tail_delay(
    timestamp: OutputStreamTimestamp,
    native_now: StreamInstant,
    callback_span: Duration,
    query_span: Duration,
) -> Option<(Duration, Duration)> {
    timestamp.playback.checked_duration_since(timestamp.callback)?;
    let tail = timestamp.playback.checked_add(callback_span)?;
    // A late callback can name a predicted tail already in the past. This is
    // still an estimate, never an observed hardware playhead.
    let delay = tail.saturating_duration_since(native_now);
    // Keep the full portable callback bound and add the cross-clock sampling
    // bracket plus the sub-nanosecond integer conversion bound.
    let uncertainty =
        callback_span.checked_add(query_span)?.checked_add(Duration::from_nanos(1))?;
    Some((delay, uncertainty))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn future_render_timestamp_keeps_native_lead_in_tail_delay() {
        let now = StreamInstant::from_nanos(1_000_000_000);
        let callback = now.checked_add(Duration::from_millis(12)).unwrap();
        let timestamp = OutputStreamTimestamp {
            callback,
            playback: callback.checked_add(Duration::from_millis(14)).unwrap(),
        };
        assert_eq!(
            tail_delay(
                timestamp,
                now,
                Duration::from_millis(5),
                Duration::from_micros(3)
            ),
            Some((Duration::from_millis(31), Duration::from_nanos(5_003_001)))
        );
    }

    #[test]
    fn past_prediction_retains_uncertainty_and_invalid_order_is_rejected() {
        let now = StreamInstant::from_nanos(1_000_000_000);
        let timestamp = OutputStreamTimestamp {
            callback: now.checked_sub(Duration::from_millis(20)).unwrap(),
            playback: now.checked_sub(Duration::from_millis(10)).unwrap(),
        };
        assert_eq!(
            tail_delay(timestamp, now, Duration::from_millis(5), Duration::ZERO),
            Some((Duration::ZERO, Duration::from_nanos(5_000_001)))
        );
        let inverted = OutputStreamTimestamp {
            callback: timestamp.playback,
            playback: timestamp.callback,
        };
        assert!(tail_delay(inverted, now, Duration::from_millis(5), Duration::ZERO).is_none());
        assert!(tail_delay(timestamp, now, Duration::MAX, Duration::ZERO).is_none());
    }
}
