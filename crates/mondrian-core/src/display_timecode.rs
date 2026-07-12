//! SMPTE display-timecode formatting, separate from exact author time.

use crate::{FramePosition, Rational};

/// Counting convention used to label an integer frame grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmpteCountingMode {
    /// Count every nominal frame label. Uses `:` separators.
    NonDropFrame,
    /// Skip label numbers according to SMPTE drop-frame rules. Uses `;`.
    DropFrame,
}

/// A decomposed SMPTE display label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmpteDisplayTimecode {
    /// Whether the source grid position was negative.
    pub negative: bool,
    /// Hours, wrapped at 24 as required for a conventional label.
    pub hours: u8,
    /// Minutes within the hour.
    pub minutes: u8,
    /// Seconds within the minute.
    pub seconds: u8,
    /// Nominal frame label within the second.
    pub frames: u8,
    /// Counting convention used for this label.
    pub mode: SmpteCountingMode,
}

impl SmpteDisplayTimecode {
    /// Convert one already-resolved frame-grid position into a display label.
    pub fn from_frame_position(
        position: FramePosition,
        mode: SmpteCountingMode,
    ) -> Result<Self, DisplayTimecodeError> {
        let frame_rate = Rational::new(position.time_base.den, position.time_base.num);
        if frame_rate.num <= 0 || frame_rate.den <= 0 {
            return Err(DisplayTimecodeError::InvalidFrameRate);
        }
        let nominal = nominal_frames_per_second(frame_rate)?;
        let negative = position.frame < 0;
        let magnitude = position.frame.unsigned_abs();
        let label_frame = match mode {
            SmpteCountingMode::NonDropFrame => magnitude,
            SmpteCountingMode::DropFrame => {
                let dropped_per_minute = match (frame_rate.num, frame_rate.den) {
                    (30_000, 1_001) => 2_u64,
                    (60_000, 1_001) => 4_u64,
                    _ => return Err(DisplayTimecodeError::UnsupportedDropFrameRate),
                };
                drop_frame_label_number(magnitude, u64::from(nominal), dropped_per_minute)
            }
        };
        let frames_per_second = u64::from(nominal);
        let frames_per_minute = frames_per_second * 60;
        let frames_per_hour = frames_per_minute * 60;
        let frames_per_day = frames_per_hour * 24;
        let label_frame = label_frame % frames_per_day;
        Ok(Self {
            negative,
            hours: u8::try_from(label_frame / frames_per_hour)
                .map_err(|_| DisplayTimecodeError::Overflow)?,
            minutes: u8::try_from((label_frame / frames_per_minute) % 60)
                .map_err(|_| DisplayTimecodeError::Overflow)?,
            seconds: u8::try_from((label_frame / frames_per_second) % 60)
                .map_err(|_| DisplayTimecodeError::Overflow)?,
            frames: u8::try_from(label_frame % frames_per_second)
                .map_err(|_| DisplayTimecodeError::Overflow)?,
            mode,
        })
    }

    /// Render a conventional `HH:MM:SS:FF` or `HH:MM:SS;FF` label.
    pub fn label(self) -> String {
        let sign = if self.negative { "-" } else { "" };
        let separator = match self.mode {
            SmpteCountingMode::NonDropFrame => ':',
            SmpteCountingMode::DropFrame => ';',
        };
        format!(
            "{sign}{:02}:{:02}:{:02}{separator}{:02}",
            self.hours, self.minutes, self.seconds, self.frames
        )
    }
}

fn nominal_frames_per_second(frame_rate: Rational) -> Result<u8, DisplayTimecodeError> {
    let rounded = ((i128::from(frame_rate.num) + i128::from(frame_rate.den) / 2)
        / i128::from(frame_rate.den)) as i64;
    if !(1..=120).contains(&rounded) {
        return Err(DisplayTimecodeError::UnsupportedNominalRate);
    }
    u8::try_from(rounded).map_err(|_| DisplayTimecodeError::Overflow)
}

fn drop_frame_label_number(frame: u64, nominal: u64, dropped_per_minute: u64) -> u64 {
    let frames_per_10_minutes = nominal * 600 - dropped_per_minute * 9;
    let frames_per_minute = nominal * 60 - dropped_per_minute;
    let ten_minute_blocks = frame / frames_per_10_minutes;
    let remainder = frame % frames_per_10_minutes;
    let completed_drop_minutes = if remainder < dropped_per_minute {
        0
    } else {
        (remainder - dropped_per_minute) / frames_per_minute
    };
    frame + dropped_per_minute * 9 * ten_minute_blocks + dropped_per_minute * completed_drop_minutes
}

/// Invalid or unsupported SMPTE display request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DisplayTimecodeError {
    /// Frame rate and time base must be positive.
    #[error("display timecode frame rate must be positive")]
    InvalidFrameRate,
    /// The current display formatter supports nominal rates up to 120 fps.
    #[error("unsupported nominal display timecode rate")]
    UnsupportedNominalRate,
    /// Drop-frame labels are defined here only for 30000/1001 and 60000/1001.
    #[error("drop-frame timecode requires 30000/1001 or 60000/1001 fps")]
    UnsupportedDropFrameRate,
    /// A decomposed component did not fit its contract.
    #[error("display timecode conversion overflow")]
    Overflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_drop_frame_is_a_presentation_of_a_frame_grid() {
        let position = FramePosition::new(75, Rational::new(1, 25));
        let label =
            SmpteDisplayTimecode::from_frame_position(position, SmpteCountingMode::NonDropFrame)
                .expect("timecode")
                .label();
        assert_eq!(label, "00:00:03:00");
    }

    #[test]
    fn drop_frame_skips_labels_but_not_media_frames() {
        let position = FramePosition::new(1_800, Rational::new(1_001, 30_000));
        let label =
            SmpteDisplayTimecode::from_frame_position(position, SmpteCountingMode::DropFrame)
                .expect("timecode")
                .label();
        assert_eq!(label, "00:01:00;02");
    }

    #[test]
    fn drop_frame_rejects_rates_without_a_defined_counting_rule() {
        let error = SmpteDisplayTimecode::from_frame_position(
            FramePosition::new(0, Rational::new(1, 25)),
            SmpteCountingMode::DropFrame,
        )
        .expect_err("unsupported");
        assert_eq!(error, DisplayTimecodeError::UnsupportedDropFrameRate);
    }
}
