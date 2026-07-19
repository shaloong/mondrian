//! Frame-grid and SMPTE display formatting, separate from exact author time.

use crate::{FramePosition, FrameRounding, Rational, TimelineTime, TimelineTimeError};
use serde::{Deserialize, Serialize};

/// Counting convention used to label an integer frame grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmpteCountingMode {
    /// Count every nominal frame label. Uses `:` separators.
    NonDropFrame,
    /// Skip label numbers according to SMPTE drop-frame rules. Uses `;`.
    DropFrame,
}

/// Persisted choice for presenting Sequence positions.
///
/// Frame display remains relative to the Sequence origin. Timecode display
/// applies [`TimelineDisplaySettings::timecode_start_frame`] on the resolved
/// Sequence frame grid; it never changes authored [`TimelineTime`] values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "counting_mode", rename_all = "snake_case")]
pub enum TimelineDisplayFormat {
    /// Display SMPTE labels on the Sequence evaluation grid.
    Timecode(SmpteCountingMode),
    /// Display signed Sequence-relative frame offsets.
    Frames,
}

/// Persisted Sequence position-display settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineDisplaySettings {
    /// Active position-label format.
    pub format: TimelineDisplayFormat,
    /// Actual frame-grid offset whose label is shown at Sequence time zero.
    ///
    /// For drop-frame timecode this is an actual frame count, not the nominal
    /// label number. It is retained while frame display is active so changing
    /// the presentation preference does not erase the Sequence timecode origin.
    pub timecode_start_frame: i64,
}

impl TimelineDisplaySettings {
    /// Construct SMPTE timecode display settings.
    pub const fn timecode(mode: SmpteCountingMode, timecode_start_frame: i64) -> Self {
        Self {
            format: TimelineDisplayFormat::Timecode(mode),
            timecode_start_frame,
        }
    }

    /// Construct Sequence-relative frame display settings.
    pub const fn frames(timecode_start_frame: i64) -> Self {
        Self {
            format: TimelineDisplayFormat::Frames,
            timecode_start_frame,
        }
    }

    /// Resolve these persisted settings against the Sequence evaluation rate.
    pub fn resolve(
        self,
        frame_rate: Rational,
    ) -> Result<TimelineDisplayContract, DisplayTimecodeError> {
        TimelineDisplayContract::new(frame_rate, self)
    }
}

impl Default for TimelineDisplaySettings {
    fn default() -> Self {
        Self::frames(0)
    }
}

/// Resolved frame-grid presentation contract shared by Viewer and Timeline UI.
///
/// The contract owns exact author-time projection, SMPTE counting, origin
/// application, overflow behavior, and frame-display semantics. Callers may
/// choose label density but may not reconstruct timecode arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineDisplayContract(TimelineDisplayImplementation);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimelineDisplayImplementation {
    Frames { frame_rate: Rational },
    Timecode(SmpteDisplayTimecodeContract),
}

impl TimelineDisplayContract {
    /// Resolve persisted display settings against one Sequence evaluation rate.
    pub fn new(
        frame_rate: Rational,
        settings: TimelineDisplaySettings,
    ) -> Result<Self, DisplayTimecodeError> {
        validate_positive_frame_rate(frame_rate)?;
        match settings.format {
            TimelineDisplayFormat::Frames => {
                Ok(Self(TimelineDisplayImplementation::Frames { frame_rate }))
            }
            TimelineDisplayFormat::Timecode(mode) => {
                SmpteDisplayTimecodeContract::new(frame_rate, mode, settings.timecode_start_frame)
                    .map(TimelineDisplayImplementation::Timecode)
                    .map(Self)
            }
        }
    }

    /// Sequence evaluation rate used by this display contract.
    pub const fn frame_rate(self) -> Rational {
        match self.0 {
            TimelineDisplayImplementation::Frames { frame_rate } => frame_rate,
            TimelineDisplayImplementation::Timecode(contract) => contract.frame_rate(),
        }
    }

    /// Persisted display format represented by this resolved contract.
    pub const fn format(self) -> TimelineDisplayFormat {
        match self.0 {
            TimelineDisplayImplementation::Frames { .. } => TimelineDisplayFormat::Frames,
            TimelineDisplayImplementation::Timecode(contract) => {
                TimelineDisplayFormat::Timecode(contract.mode())
            }
        }
    }

    /// Resolved SMPTE contract, when the active display format is timecode.
    pub const fn timecode_contract(self) -> Option<SmpteDisplayTimecodeContract> {
        match self.0 {
            TimelineDisplayImplementation::Frames { .. } => None,
            TimelineDisplayImplementation::Timecode(contract) => Some(contract),
        }
    }

    /// Format one signed Sequence-relative evaluation-frame offset.
    pub fn format_frame_offset(self, frame: i64) -> Result<String, DisplayTimecodeError> {
        match self.0 {
            TimelineDisplayImplementation::Frames { .. } => Ok(frame.to_string()),
            TimelineDisplayImplementation::Timecode(contract) => {
                Ok(contract.timecode_at_frame(frame)?.label())
            }
        }
    }

    /// Project exact author time onto this display grid and format it.
    pub fn format_timeline_time(
        self,
        time: TimelineTime,
        rounding: FrameRounding,
    ) -> Result<String, DisplayTimecodeError> {
        match self.0 {
            TimelineDisplayImplementation::Frames { frame_rate } => time
                .to_frame_position(frame_rate, rounding)
                .map(|position| position.frame.to_string())
                .map_err(DisplayTimecodeError::from),
            TimelineDisplayImplementation::Timecode(contract) => {
                Ok(contract.timecode_at_time(time, rounding)?.label())
            }
        }
    }
}

impl Default for TimelineDisplayContract {
    fn default() -> Self {
        Self(TimelineDisplayImplementation::Frames { frame_rate: Rational::FPS_30 })
    }
}

/// Validated SMPTE counting contract for one resolved Sequence frame grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmpteDisplayTimecodeContract {
    frame_rate: Rational,
    mode: SmpteCountingMode,
    start_frame: i64,
}

impl SmpteDisplayTimecodeContract {
    /// Construct and validate a display-timecode contract.
    pub fn new(
        frame_rate: Rational,
        mode: SmpteCountingMode,
        start_frame: i64,
    ) -> Result<Self, DisplayTimecodeError> {
        validate_smpte_frame_rate(frame_rate, mode)?;
        Ok(Self { frame_rate, mode, start_frame })
    }

    /// Sequence evaluation rate used for label counting.
    pub const fn frame_rate(self) -> Rational {
        self.frame_rate
    }

    /// SMPTE counting convention.
    pub const fn mode(self) -> SmpteCountingMode {
        self.mode
    }

    /// Actual frame-grid offset whose label appears at Sequence time zero.
    pub const fn start_frame(self) -> i64 {
        self.start_frame
    }

    /// Resolve one signed Sequence-relative frame offset to a SMPTE label value.
    pub fn timecode_at_frame(
        self,
        frame_offset: i64,
    ) -> Result<SmpteDisplayTimecode, DisplayTimecodeError> {
        let frame = self
            .start_frame
            .checked_add(frame_offset)
            .ok_or(DisplayTimecodeError::FrameOffsetOverflow)?;
        SmpteDisplayTimecode::from_frame_position(
            FramePosition::new(frame, frame_time_base(self.frame_rate)),
            self.mode,
        )
    }

    /// Project exact author time once onto the Sequence frame grid and label it.
    pub fn timecode_at_time(
        self,
        time: TimelineTime,
        rounding: FrameRounding,
    ) -> Result<SmpteDisplayTimecode, DisplayTimecodeError> {
        let frame = time.to_frame_position(self.frame_rate, rounding)?;
        self.timecode_at_frame(frame.frame)
    }
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
        validate_positive_frame_rate(frame_rate)?;
        let nominal = nominal_frames_per_second(frame_rate)?;
        let negative = position.frame < 0;
        let magnitude = position.frame.unsigned_abs();
        let label_frame = match mode {
            SmpteCountingMode::NonDropFrame => magnitude,
            SmpteCountingMode::DropFrame => {
                let dropped_per_minute = dropped_frames_per_minute(frame_rate)?;
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

    /// Render `HH:MM:SS` without the frame field.
    pub fn clock_label(self) -> String {
        let sign = if self.negative { "-" } else { "" };
        format!(
            "{sign}{:02}:{:02}:{:02}",
            self.hours, self.minutes, self.seconds
        )
    }

    /// Render `MM:SS` for a compact sub-hour ruler label.
    pub fn minute_second_label(self) -> String {
        let sign = if self.negative { "-" } else { "" };
        format!("{sign}{:02}:{:02}", self.minutes, self.seconds)
    }
}

fn frame_time_base(frame_rate: Rational) -> Rational {
    Rational::new(frame_rate.den, frame_rate.num)
}

fn validate_positive_frame_rate(frame_rate: Rational) -> Result<(), DisplayTimecodeError> {
    if frame_rate.num <= 0 || frame_rate.den <= 0 {
        Err(DisplayTimecodeError::InvalidFrameRate)
    } else {
        Ok(())
    }
}

fn validate_smpte_frame_rate(
    frame_rate: Rational,
    mode: SmpteCountingMode,
) -> Result<(), DisplayTimecodeError> {
    validate_positive_frame_rate(frame_rate)?;
    nominal_frames_per_second(frame_rate)?;
    if mode == SmpteCountingMode::DropFrame {
        dropped_frames_per_minute(frame_rate)?;
    }
    Ok(())
}

fn dropped_frames_per_minute(frame_rate: Rational) -> Result<u64, DisplayTimecodeError> {
    match (frame_rate.num, frame_rate.den) {
        (30_000, 1_001) => Ok(2),
        (60_000, 1_001) => Ok(4),
        _ => Err(DisplayTimecodeError::UnsupportedDropFrameRate),
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
    /// Adding the configured origin to a requested frame exceeded `i64`.
    #[error("display timecode frame offset overflow")]
    FrameOffsetOverflow,
    /// Exact author time could not be resolved on the display grid.
    #[error(transparent)]
    TimelineTime(#[from] TimelineTimeError),
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

    #[test]
    fn resolved_contract_applies_origin_after_exact_grid_projection() {
        let settings = TimelineDisplaySettings::timecode(SmpteCountingMode::DropFrame, 107_892);
        let contract = settings.resolve(Rational::FPS_2997).expect("display contract");

        assert_eq!(
            contract
                .format_timeline_time(TimelineTime::ZERO, FrameRounding::Nearest)
                .expect("origin label"),
            "01:00:00;00"
        );
        assert_eq!(
            contract.format_frame_offset(-1).expect("negative offset"),
            "00:59:59;29"
        );
    }

    #[test]
    fn frame_display_never_applies_timecode_origin() {
        let contract = TimelineDisplaySettings::frames(107_892)
            .resolve(Rational::FPS_2997)
            .expect("frame display");

        assert_eq!(contract.format_frame_offset(-7).expect("frame label"), "-7");
    }

    #[test]
    fn resolved_contract_rejects_incompatible_drop_frame_rate() {
        let error = TimelineDisplaySettings::timecode(SmpteCountingMode::DropFrame, 0)
            .resolve(Rational::FPS_25)
            .expect_err("25 fps has no drop-frame counting contract");

        assert_eq!(error, DisplayTimecodeError::UnsupportedDropFrameRate);
    }

    #[test]
    fn display_settings_round_trip_the_tagged_schema() {
        let settings = TimelineDisplaySettings::timecode(SmpteCountingMode::DropFrame, 107_892);
        let json = serde_json::to_string(&settings).expect("serialize display settings");
        let restored: TimelineDisplaySettings =
            serde_json::from_str(&json).expect("deserialize display settings");

        assert_eq!(restored, settings);
        assert!(json.contains("drop_frame"));
    }
}
