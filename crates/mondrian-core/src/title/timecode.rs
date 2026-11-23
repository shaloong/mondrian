use serde::{Deserialize, Serialize};

use super::{enum_options, required_enum, required_int, title_error};
use crate::automation::{
    ParameterInvalidValuePolicy, ParameterNumericContract, ParameterUnit, PropertyBag,
    PropertyDescriptor, PropertyValue,
};
use crate::{BasicTitle, ParameterId};
use crate::{
    DisplayTimecodeError, FrameRounding, Rational, SmpteCountingMode, SmpteDisplayTimecodeContract,
    SmpteTimecodeReference, TimelineDisplayFormat, TimelineDisplaySettings, TimelineTime,
    VisualSourceReference,
};

impl BasicTitle {
    pub const TIMECODE_FORMAT_PATH: &'static str = "title.timecode.format";
    pub const TIMECODE_SOURCE_PATH: &'static str = "title.timecode.source";
    pub const TIMECODE_MEDIA_CLIP_PATH: &'static str = "title.timecode.media_clip";
    pub const TIMECODE_MEDIA_CLIP_ID: &'static str = "mondrian.title.timecode.media_clip";
    pub const TIMECODE_RATE_PATH: &'static str = "title.timecode.rate";
    pub const TIMECODE_DROP_PATH: &'static str = "title.timecode.drop_frame";
    pub const TIMECODE_START_PATH: &'static str = "title.timecode.start_frame";
    pub const TIMECODE_POSITION_PATH: &'static str = "title.timecode.position";
    pub const TIMECODE_BOX_PATH: &'static str = "title.timecode.show_box";
    pub const TIMECODE_COMPOSITE_ORIGINAL_PATH: &'static str = "title.timecode.composite_original";

    /// Property order for the timecode controls in authoring adapters.
    pub const TIMECODE_PROPERTY_PATHS: [&'static str; 9] = [
        Self::TIMECODE_FORMAT_PATH,
        Self::TIMECODE_SOURCE_PATH,
        Self::TIMECODE_MEDIA_CLIP_PATH,
        Self::TIMECODE_RATE_PATH,
        Self::TIMECODE_DROP_PATH,
        Self::TIMECODE_START_PATH,
        Self::TIMECODE_POSITION_PATH,
        Self::TIMECODE_BOX_PATH,
        Self::TIMECODE_COMPOSITE_ORIGINAL_PATH,
    ];
}

pub(super) fn descriptors() -> [PropertyDescriptor; 9] {
    let field = |path, id, name, value| {
        PropertyDescriptor::new(path, name, value)
            .with_parameter_id(ParameterId::new_static(id))
            .with_animatable(false)
    };
    let numeric = |min, max, step| {
        ParameterNumericContract::closed(min, max, Some(step), ParameterInvalidValuePolicy::Reject)
            .expect("valid timecode numeric contract")
    };
    [
        field(
            BasicTitle::TIMECODE_FORMAT_PATH,
            "mondrian.title.timecode.format",
            "显示格式",
            PropertyValue::Enum("smpte".into()),
        )
        .with_enum_options(enum_options(&[
            ("smpte", "SMPTE"),
            ("frames", "Frames"),
            ("feet_35mm", "35 mm feet + frames"),
            ("feet_16mm", "16 mm feet + frames"),
        ])),
        field(
            BasicTitle::TIMECODE_SOURCE_PATH,
            "mondrian.title.timecode.source",
            "时间源",
            PropertyValue::Enum("sequence".into()),
        )
        .with_enum_options(enum_options(&[
            ("sequence", "Sequence"),
            ("media", "Media source"),
            ("custom", "Custom clock"),
        ])),
        PropertyDescriptor::new(
            BasicTitle::TIMECODE_MEDIA_CLIP_PATH,
            "媒体片段",
            PropertyValue::VisualSource(VisualSourceReference::Unbound),
        )
        .with_parameter_id(ParameterId::new_static(BasicTitle::TIMECODE_MEDIA_CLIP_ID))
        .with_animatable(false),
        field(
            BasicTitle::TIMECODE_RATE_PATH,
            "mondrian.title.timecode.rate",
            "帧率",
            PropertyValue::Int(30),
        )
        .with_numeric_contract(ParameterUnit::Unitless, numeric(1.0, 1000.0, 1.0)),
        field(
            BasicTitle::TIMECODE_DROP_PATH,
            "mondrian.title.timecode.drop_frame",
            "丢帧",
            PropertyValue::Bool(false),
        ),
        field(
            BasicTitle::TIMECODE_START_PATH,
            "mondrian.title.timecode.start_frame",
            "起始帧",
            PropertyValue::Int(0),
        )
        .with_numeric_contract(
            ParameterUnit::Unitless,
            numeric(-100_000.0, 10_000_000.0, 1.0),
        ),
        field(
            BasicTitle::TIMECODE_POSITION_PATH,
            "mondrian.title.timecode.position",
            "文本位置",
            PropertyValue::Vec2(TitleTimecode::default().position.into()),
        )
        .with_numeric_contract(
            ParameterUnit::Pixels,
            numeric(-1_000_000.0, 1_000_000.0, 0.1),
        ),
        field(
            BasicTitle::TIMECODE_BOX_PATH,
            "mondrian.title.timecode.show_box",
            "显示方框",
            PropertyValue::Bool(true),
        ),
        field(
            BasicTitle::TIMECODE_COMPOSITE_ORIGINAL_PATH,
            "mondrian.title.timecode.composite_original",
            "合成原图",
            PropertyValue::Bool(true),
        ),
    ]
}

pub(super) fn flag(properties: &PropertyBag, path: &str) -> crate::Result<bool> {
    match properties.evaluate(path, TimelineTime::ZERO) {
        Some(PropertyValue::Bool(value)) => Ok(value),
        _ => Err(title_error(format!(
            "Timecode Title property `{path}` must be boolean"
        ))),
    }
}

pub(super) fn show_box(properties: &PropertyBag) -> crate::Result<bool> {
    flag(properties, BasicTitle::TIMECODE_BOX_PATH)
}

pub(super) fn media_reference(properties: &PropertyBag) -> crate::Result<VisualSourceReference> {
    match properties.evaluate(BasicTitle::TIMECODE_MEDIA_CLIP_PATH, TimelineTime::ZERO) {
        Some(PropertyValue::VisualSource(
            reference @ (VisualSourceReference::Unbound | VisualSourceReference::Clip { .. }),
        )) => Ok(reference),
        _ => Err(title_error(
            "Timecode Title source must reference a media Clip",
        )),
    }
}

/// Presentation format of a generated Timecode Title.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TitleTimecodeFormat {
    #[default]
    Smpte,
    Frames,
    FeetAndFrames35mm,
    FeetAndFrames16mm,
}

/// Authored clock for a generated Timecode Title.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TitleTimecodeSource {
    /// Follow the owner Sequence's frame grid and timecode origin.
    #[default]
    Sequence,
    /// Follow a media Clip's editorial timecode and exact sampled picture.
    Media { reference: VisualSourceReference },
    /// Count from the title Clip's source-local time origin.
    Custom {
        frames_per_second: u16,
        drop_frame: bool,
        start_frame: i64,
    },
}

/// Placement-local timecode and the resolved frame on its own source grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TitleMediaTimecodeSample {
    pub reference: SmpteTimecodeReference,
    pub frame: i64,
}

/// Decoded author clock settings. The title stores these in its property bag.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TitleTimecode {
    pub format: TitleTimecodeFormat,
    pub source: TitleTimecodeSource,
    /// Top-left layout anchor in the owner Sequence's author pixels.
    pub position: [f32; 2],
}

impl Default for TitleTimecode {
    fn default() -> Self {
        Self {
            format: TitleTimecodeFormat::Smpte,
            source: TitleTimecodeSource::Sequence,
            position: [25.6, 25.6],
        }
    }
}

impl TitleTimecode {
    pub(super) fn read_from(properties: &PropertyBag) -> crate::Result<Self> {
        let read_enum = |path| required_enum(properties, path, TimelineTime::ZERO);
        let read_int = |path| required_int(properties, path, TimelineTime::ZERO);
        let format = match read_enum(BasicTitle::TIMECODE_FORMAT_PATH)?.as_str() {
            "smpte" => TitleTimecodeFormat::Smpte,
            "frames" => TitleTimecodeFormat::Frames,
            "feet_35mm" => TitleTimecodeFormat::FeetAndFrames35mm,
            "feet_16mm" => TitleTimecodeFormat::FeetAndFrames16mm,
            _ => return Err(title_error("unknown Timecode Title display format")),
        };
        let reference = media_reference(properties)?;
        let source = match read_enum(BasicTitle::TIMECODE_SOURCE_PATH)?.as_str() {
            "sequence" => TitleTimecodeSource::Sequence,
            "media" => TitleTimecodeSource::Media { reference },
            "custom" => TitleTimecodeSource::Custom {
                frames_per_second: u16::try_from(read_int(BasicTitle::TIMECODE_RATE_PATH)?)
                    .map_err(|_| title_error("Timecode Title frame rate is out of range"))?,
                drop_frame: flag(properties, BasicTitle::TIMECODE_DROP_PATH)?,
                start_frame: read_int(BasicTitle::TIMECODE_START_PATH)?,
            },
            _ => return Err(title_error("unknown Timecode Title clock source")),
        };
        let Some(PropertyValue::Vec2(position)) =
            properties.evaluate(BasicTitle::TIMECODE_POSITION_PATH, TimelineTime::ZERO)
        else {
            return Err(title_error(
                "Timecode Title position must be a two-component point",
            ));
        };
        Ok(Self { format, source, position: position.to_array() })
    }

    pub(super) fn write_to(self, properties: &mut PropertyBag) -> crate::Result<()> {
        let format = match self.format {
            TitleTimecodeFormat::Smpte => "smpte",
            TitleTimecodeFormat::Frames => "frames",
            TitleTimecodeFormat::FeetAndFrames35mm => "feet_35mm",
            TitleTimecodeFormat::FeetAndFrames16mm => "feet_16mm",
        };
        properties.set_static_value(
            BasicTitle::TIMECODE_FORMAT_PATH,
            PropertyValue::Enum(format.into()),
        )?;
        properties.set_static_value(
            BasicTitle::TIMECODE_POSITION_PATH,
            PropertyValue::Vec2(self.position.into()),
        )?;
        let source_key = match self.source {
            TitleTimecodeSource::Sequence => "sequence",
            TitleTimecodeSource::Media { reference } => {
                properties.set_static_value(
                    BasicTitle::TIMECODE_MEDIA_CLIP_PATH,
                    PropertyValue::VisualSource(reference),
                )?;
                "media"
            }
            TitleTimecodeSource::Custom { frames_per_second, drop_frame, start_frame } => {
                properties.set_static_value(
                    BasicTitle::TIMECODE_RATE_PATH,
                    PropertyValue::Int(frames_per_second.into()),
                )?;
                properties.set_static_value(
                    BasicTitle::TIMECODE_DROP_PATH,
                    PropertyValue::Bool(drop_frame),
                )?;
                properties.set_static_value(
                    BasicTitle::TIMECODE_START_PATH,
                    PropertyValue::Int(start_frame),
                )?;
                "custom"
            }
        };
        properties.set_static_value(
            BasicTitle::TIMECODE_SOURCE_PATH,
            PropertyValue::Enum(source_key.into()),
        )?;
        Ok(())
    }

    pub(super) fn validate(self) -> crate::Result<()> {
        if !self.position.into_iter().all(f32::is_finite) {
            return Err(super::title_error("Timecode Title position must be finite"));
        }
        if let TitleTimecodeSource::Custom { frames_per_second, drop_frame, .. } = self.source {
            let (rate, mode) = custom_clock(frames_per_second, drop_frame)
                .map_err(|error| super::title_error(error.to_string()))?;
            if self.format == TitleTimecodeFormat::Smpte {
                SmpteDisplayTimecodeContract::new(rate, mode, 0)
                    .map_err(|error| super::title_error(error.to_string()))?;
            }
        }
        Ok(())
    }

    /// Resolve the label from exact owner and Clip-local times.
    pub fn label(
        self,
        sequence_time: TimelineTime,
        clip_time: TimelineTime,
        sequence_rate: Rational,
        sequence_display: TimelineDisplaySettings,
    ) -> Result<String, DisplayTimecodeError> {
        self.label_with_media_sample(
            sequence_time,
            clip_time,
            sequence_rate,
            sequence_display,
            None,
        )
    }

    pub(super) fn label_with_media_sample(
        self,
        sequence_time: TimelineTime,
        clip_time: TimelineTime,
        sequence_rate: Rational,
        sequence_display: TimelineDisplaySettings,
        media_sample: Option<TitleMediaTimecodeSample>,
    ) -> Result<String, DisplayTimecodeError> {
        let (frame, rate, counting, start) = match self.source {
            TitleTimecodeSource::Sequence => {
                let counting = match sequence_display.format {
                    TimelineDisplayFormat::Timecode(mode) => mode,
                    TimelineDisplayFormat::Frames => SmpteCountingMode::NonDropFrame,
                };
                (
                    sequence_time.to_frame_position(sequence_rate, FrameRounding::Floor)?.frame,
                    sequence_rate,
                    counting,
                    sequence_display.timecode_start_frame,
                )
            }
            TitleTimecodeSource::Media { .. } => {
                let sample =
                    media_sample.ok_or(DisplayTimecodeError::MissingMediaTimecodeSample)?;
                (
                    sample.frame,
                    sample.reference.frame_rate(),
                    sample.reference.mode(),
                    sample.reference.start_frame(),
                )
            }
            TitleTimecodeSource::Custom { frames_per_second, drop_frame, start_frame } => {
                let (rate, mode) = custom_clock(frames_per_second, drop_frame)?;
                (
                    clip_time.to_frame_position(rate, FrameRounding::Floor)?.frame,
                    rate,
                    mode,
                    start_frame,
                )
            }
        };
        format_label(self.format, frame, rate, counting, start)
    }
}

pub(super) fn format_label(
    format: TitleTimecodeFormat,
    frame: i64,
    rate: Rational,
    counting: SmpteCountingMode,
    start: i64,
) -> Result<String, DisplayTimecodeError> {
    if format == TitleTimecodeFormat::Smpte {
        return Ok(SmpteDisplayTimecodeContract::new(rate, counting, start)?
            .timecode_at_frame(frame)?
            .label());
    }
    let frame = start.checked_add(frame).ok_or(DisplayTimecodeError::FrameOffsetOverflow)?;
    Ok(match format {
        TitleTimecodeFormat::Frames => frame.to_string(),
        TitleTimecodeFormat::FeetAndFrames35mm => film_label(frame, 16),
        TitleTimecodeFormat::FeetAndFrames16mm => film_label(frame, 40),
        TitleTimecodeFormat::Smpte => unreachable!("SMPTE has its own counting contract"),
    })
}

fn custom_clock(
    frames_per_second: u16,
    drop_frame: bool,
) -> Result<(Rational, SmpteCountingMode), DisplayTimecodeError> {
    if !(1..=1000).contains(&frames_per_second) {
        return Err(DisplayTimecodeError::UnsupportedNominalRate);
    }
    let rate = match (drop_frame, frames_per_second) {
        (false, rate) => Rational::new(i64::from(rate), 1),
        (true, 30) => Rational::FPS_2997,
        (true, 60) => Rational::FPS_5994,
        (true, _) => return Err(DisplayTimecodeError::UnsupportedDropFrameRate),
    };
    let mode = if drop_frame {
        SmpteCountingMode::DropFrame
    } else {
        SmpteCountingMode::NonDropFrame
    };
    Ok((rate, mode))
}

fn film_label(frame: i64, frames_per_foot: u64) -> String {
    let sign = if frame < 0 { "-" } else { "" };
    let magnitude = frame.unsigned_abs();
    let feet = magnitude / frames_per_foot;
    let remaining = magnitude % frames_per_foot;
    format!("{sign}{feet}+{remaining:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_frame_uses_owner_grid_and_actual_start_frame() {
        let clock = TitleTimecode::default();
        let at_minute = TimelineDisplaySettings::timecode(SmpteCountingMode::DropFrame, 1800);
        assert_eq!(
            clock
                .label(
                    TimelineTime::ZERO,
                    TimelineTime::ZERO,
                    Rational::FPS_2997,
                    at_minute
                )
                .unwrap(),
            "00:01:00;02",
        );
        let at_ten_minutes = TimelineDisplaySettings::timecode(SmpteCountingMode::DropFrame, 17982);
        assert_eq!(
            clock
                .label(
                    TimelineTime::ZERO,
                    TimelineTime::ZERO,
                    Rational::FPS_2997,
                    at_ten_minutes
                )
                .unwrap(),
            "00:10:00;00",
        );
    }

    #[test]
    fn custom_clock_uses_local_time_and_floors_negative_subframes() {
        let mut clock = TitleTimecode {
            source: TitleTimecodeSource::Custom {
                frames_per_second: 24,
                drop_frame: false,
                start_frame: -16,
            },
            format: TitleTimecodeFormat::Frames,
            ..TitleTimecode::default()
        };
        let label = |clock: TitleTimecode, clip_time| {
            clock
                .label(
                    TimelineTime::new(100, 1).unwrap(),
                    clip_time,
                    Rational::FPS_30,
                    TimelineDisplaySettings::frames(600),
                )
                .unwrap()
        };
        assert_eq!(label(clock, TimelineTime::new(-1, 48).unwrap()), "-17");
        clock.format = TitleTimecodeFormat::FeetAndFrames35mm;
        assert_eq!(label(clock, TimelineTime::ZERO), "-1+00");
        clock.format = TitleTimecodeFormat::FeetAndFrames16mm;
        assert_eq!(label(clock, TimelineTime::new(56, 24).unwrap()), "1+00");
    }

    #[test]
    fn unsupported_custom_clock_and_overflow_fail_closed() {
        let clock = TitleTimecode {
            source: TitleTimecodeSource::Custom {
                frames_per_second: 24,
                drop_frame: true,
                start_frame: 0,
            },
            ..TitleTimecode::default()
        };
        assert!(clock.validate().is_err());
        let clock = TitleTimecode {
            format: TitleTimecodeFormat::Frames,
            ..TitleTimecode::default()
        };
        assert_eq!(
            clock.label(
                TimelineTime::new(1, 1).unwrap(),
                TimelineTime::ZERO,
                Rational::FPS_30,
                TimelineDisplaySettings::frames(i64::MAX)
            ),
            Err(DisplayTimecodeError::FrameOffsetOverflow),
        );
    }

    #[test]
    fn title_serializes_authored_clock_without_a_generated_label() {
        let clock = TitleTimecode {
            format: TitleTimecodeFormat::FeetAndFrames16mm,
            source: TitleTimecodeSource::Custom {
                frames_per_second: 60,
                drop_frame: true,
                start_frame: 80,
            },
            position: [12.0, 90.0],
        };
        let title = crate::BasicTitle::new_timecode(clock, "Arial").unwrap();
        let value = serde_json::to_value(&title).unwrap();
        assert_eq!(value["content_kind"], "timecode");
        let reopened: crate::BasicTitle = serde_json::from_value(value).unwrap();
        reopened.validate_author_state().unwrap();
        assert_eq!(reopened.timecode().unwrap(), Some(clock));
        assert!(reopened.evaluate(TimelineTime::ZERO).unwrap().text.is_empty());
    }
}
