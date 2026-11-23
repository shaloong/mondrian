use super::{
    clip_info, enum_options, required_enum, required_float, required_int, timecode, title_error,
};
use crate::automation::{
    ParameterInvalidValuePolicy, ParameterNumericContract, ParameterTextMode, ParameterUnit,
    PropertyBag, PropertyDescriptor, PropertyValue,
};
use crate::{
    DisplayTimecodeError, ParameterId, Rational, Result, SmpteCountingMode, TimelineTime,
    TitleClipInfoField, TitleTimecodeFormat, TrackId,
};

/// Stable author paths for an independently positioned metadata line.
#[derive(Debug, Clone, Copy)]
pub struct TitleMetadataLinePaths {
    pub enabled: &'static str,
    pub field: &'static str,
    pub position: &'static str,
    pub scale: &'static str,
    pub track: &'static str,
    pub format: &'static str,
    pub time_origin: &'static str,
    pub offset: &'static str,
    pub start_frame: &'static str,
}

impl TitleMetadataLinePaths {
    pub fn contains(self, path: &str) -> bool {
        [
            self.enabled,
            self.field,
            self.position,
            self.scale,
            self.track,
            self.format,
            self.time_origin,
            self.offset,
            self.start_frame,
        ]
        .contains(&path)
    }
}

macro_rules! line_paths {
    ($n:literal) => {
        TitleMetadataLinePaths {
            enabled: concat!("title.metadata.line_", $n, ".enabled"),
            field: concat!("title.metadata.line_", $n, ".field"),
            position: concat!("title.metadata.line_", $n, ".position"),
            scale: concat!("title.metadata.line_", $n, ".scale"),
            track: concat!("title.metadata.line_", $n, ".track"),
            format: concat!("title.metadata.line_", $n, ".format"),
            time_origin: concat!("title.metadata.line_", $n, ".time_origin"),
            offset: concat!("title.metadata.line_", $n, ".offset"),
            start_frame: concat!("title.metadata.line_", $n, ".start_frame"),
        }
    };
}

pub const TITLE_METADATA_LINE_PATHS: [TitleMetadataLinePaths; 5] = [
    line_paths!("1"),
    line_paths!("2"),
    line_paths!("3"),
    line_paths!("4"),
    line_paths!("5"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleMetadataField {
    Clip(TitleClipInfoField),
    SequenceTimecode,
    SourceTimecode,
}

impl TitleMetadataField {
    pub const fn is_timecode(self) -> bool {
        matches!(self, Self::SequenceTimecode | Self::SourceTimecode)
    }

    pub const fn uses_asset_metadata(self) -> bool {
        matches!(self, Self::Clip(field) if field.uses_asset_metadata())
    }
}

/// Evaluated author state of one line, before the renderer resolves its source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TitleMetadataLine {
    pub enabled: bool,
    pub field: TitleMetadataField,
    pub position: [f32; 2],
    pub scale_percent: f32,
    pub track_id: Option<TrackId>,
    pub format: TitleTimecodeFormat,
    pub offset_frames: i64,
    pub custom_start_frame: Option<i64>,
}

impl TitleMetadataLine {
    pub(super) fn read_from(
        properties: &PropertyBag,
        paths: TitleMetadataLinePaths,
    ) -> Result<Self> {
        let time = TimelineTime::ZERO;
        let field = match required_enum(properties, paths.field, time)?.as_str() {
            "clip_name" => TitleMetadataField::Clip(TitleClipInfoField::ClipName),
            "reel_name" => TitleMetadataField::Clip(TitleClipInfoField::ReelName),
            "asset_name" => TitleMetadataField::Clip(TitleClipInfoField::AssetName),
            "file_name" => TitleMetadataField::Clip(TitleClipInfoField::FileName),
            "media_frame_rate" => TitleMetadataField::Clip(TitleClipInfoField::MediaFrameRate),
            "sequence_timecode" => TitleMetadataField::SequenceTimecode,
            "source_timecode" => TitleMetadataField::SourceTimecode,
            _ => return Err(title_error("unknown Metadata Burn-in field")),
        };
        let Some(PropertyValue::Vec2(position)) = properties.evaluate(paths.position, time) else {
            return Err(title_error(
                "Metadata Burn-in position must have two components",
            ));
        };
        let format = match required_enum(properties, paths.format, time)?.as_str() {
            "smpte" => TitleTimecodeFormat::Smpte,
            "frames" => TitleTimecodeFormat::Frames,
            "feet_35mm" => TitleTimecodeFormat::FeetAndFrames35mm,
            "feet_16mm" => TitleTimecodeFormat::FeetAndFrames16mm,
            _ => return Err(title_error("unknown Metadata Burn-in timecode format")),
        };
        let custom_start_frame = match required_enum(properties, paths.time_origin, time)?.as_str()
        {
            "original" => None,
            "custom_origin" => Some(required_int(properties, paths.start_frame, time)?),
            _ => return Err(title_error("unknown Metadata Burn-in time origin")),
        };
        Ok(Self {
            enabled: timecode::flag(properties, paths.enabled)?,
            field,
            position: position.to_array(),
            scale_percent: required_float(properties, paths.scale, time)?,
            track_id: clip_info::read_metadata_track(properties, paths.track)?,
            format,
            offset_frames: required_int(properties, paths.offset, time)?,
            custom_start_frame,
        })
    }

    pub fn format_timecode(
        self,
        rate: Rational,
        counting: SmpteCountingMode,
        original_start_frame: i64,
        frame: i64,
    ) -> std::result::Result<String, DisplayTimecodeError> {
        let start = self.custom_start_frame.unwrap_or(original_start_frame);
        let frame = frame
            .checked_add(self.offset_frames)
            .ok_or(DisplayTimecodeError::FrameOffsetOverflow)?;
        timecode::format_label(self.format, frame, rate, counting, start)
    }
}

const FIELD_CHOICES: &[(&str, &str)] = &[
    ("clip_name", "Clip name"),
    ("reel_name", "Reel name"),
    ("asset_name", "Asset Library name"),
    ("file_name", "File name"),
    ("sequence_timecode", "Sequence timecode"),
    ("source_timecode", "Source timecode"),
    ("media_frame_rate", "Media frame rate"),
];

/// Old Projects and standalone queued exports both serialize these option lists.
pub(super) fn upgrade_field_options(properties: &mut PropertyBag) {
    let current = enum_options(FIELD_CHOICES);
    let legacy = &current[..current.len() - 1];
    for paths in TITLE_METADATA_LINE_PATHS {
        if let Some(property) = properties.property(paths.field)
            && property.descriptor.schema.enum_options == legacy
            && property.validate().is_ok()
        {
            let mut property = property.clone();
            property.descriptor.schema.enum_options.clone_from(&current);
            properties.upsert(property);
        }
    }
}

pub(super) fn descriptors() -> impl Iterator<Item = PropertyDescriptor> {
    let field = |path: &'static str, name, value| {
        let mut descriptor = PropertyDescriptor::new(path, name, value)
            .with_parameter_id(
                ParameterId::new(format!("mondrian.{path}")).expect("metadata line parameter ID"),
            )
            .with_animatable(false);
        let (_, control) = path.rsplit_once('.').expect("metadata line control name");
        descriptor.schema.message_id = format!("mondrian.title.metadata.{control}.label");
        descriptor
    };
    let numeric = |min, max, step| {
        ParameterNumericContract::closed(min, max, Some(step), ParameterInvalidValuePolicy::Reject)
            .expect("metadata line numeric contract")
    };
    const VERTICAL_ANCHORS: [f32; 5] = [0.93, 0.83, 0.73, 0.63, 0.53];
    TITLE_METADATA_LINE_PATHS
        .into_iter()
        .enumerate()
        .flat_map(move |(index, paths)| {
            let frames = || numeric(-1_000_000_000.0, 1_000_000_000.0, 1.0);
            [
                field(paths.enabled, "启用", PropertyValue::Bool(index == 0)),
                field(
                    paths.field,
                    "元数据",
                    PropertyValue::Enum(
                        if index == 0 {
                            "sequence_timecode"
                        } else {
                            "clip_name"
                        }
                        .into(),
                    ),
                )
                .with_enum_options(enum_options(FIELD_CHOICES)),
                field(
                    paths.position,
                    "位置",
                    PropertyValue::Vec2([0.38, VERTICAL_ANCHORS[index]].into()),
                )
                .with_numeric_contract(ParameterUnit::Unitless, numeric(0.0, 1.0, 0.001)),
                field(paths.scale, "行大小", PropertyValue::Float(100.0))
                    .with_numeric_contract(ParameterUnit::Percent, numeric(1.0, 400.0, 1.0)),
                field(paths.track, "源轨道", PropertyValue::Text(String::new()))
                    .with_text_mode(ParameterTextMode::SingleLine),
                field(paths.format, "格式", PropertyValue::Enum("smpte".into())).with_enum_options(
                    enum_options(&[
                        ("smpte", "SMPTE"),
                        ("frames", "Frames"),
                        ("feet_35mm", "35 mm"),
                        ("feet_16mm", "16 mm"),
                    ]),
                ),
                field(
                    paths.time_origin,
                    "时间显示",
                    PropertyValue::Enum("original".into()),
                )
                .with_enum_options(enum_options(&[
                    ("original", "Original origin"),
                    ("custom_origin", "Custom origin"),
                ])),
                field(paths.offset, "偏移", PropertyValue::Int(0))
                    .with_numeric_contract(ParameterUnit::Unitless, frames()),
                field(paths.start_frame, "起始时间码", PropertyValue::Int(0))
                    .with_numeric_contract(ParameterUnit::Unitless, frames()),
            ]
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_burn_in_reads_legacy_export_titles_with_stable_line_addresses() {
        let current = crate::BasicTitle::new_metadata_burn_in("Arial").unwrap();
        let mut legacy = serde_json::to_value(&current).unwrap();
        for paths in TITLE_METADATA_LINE_PATHS {
            let options = legacy["properties"]["properties"][paths.field]["descriptor"]["schema"]
                ["enum_options"]
                .as_array_mut()
                .unwrap();
            assert_eq!(options.pop().unwrap()["key"], "media_frame_rate");
        }
        let restored: crate::BasicTitle = serde_json::from_value(legacy).unwrap();
        restored.validate_author_state().unwrap();
        assert_eq!(restored, current);
    }
}
