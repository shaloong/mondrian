use super::{enum_options, required_enum, required_text, title_error};
use crate::automation::{
    ParameterInvalidValuePolicy, ParameterNumericContract, ParameterTextMode, ParameterUnit,
    PropertyBag, PropertyDescriptor, PropertyValue,
};
use crate::{AssetId, BasicTitle, ParameterId, Result, TimelineTime, TrackId};
use std::{collections::HashMap, path::Path};

impl BasicTitle {
    pub const CLIP_INFO_POSITION_PATH: &'static str = "title.clip_info.position";
    pub const CLIP_INFO_FIELD_PATH: &'static str = "title.clip_info.field";
    pub const CLIP_INFO_TRACK_PATH: &'static str = "title.clip_info.track";

    pub const CLIP_INFO_PROPERTY_PATHS: [&'static str; 3] = [
        Self::CLIP_INFO_FIELD_PATH,
        Self::CLIP_INFO_TRACK_PATH,
        Self::CLIP_INFO_POSITION_PATH,
    ];
}

/// Metadata supplied by an editorial Clip at the requested Sequence time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleClipInfoField {
    ClipName,
    ReelName,
    AssetName,
    FileName,
}

impl TitleClipInfoField {
    pub const fn uses_asset_names(self) -> bool {
        matches!(self, Self::AssetName | Self::FileName)
    }
}

/// The two independent names observed for a file-backed Asset by a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleAssetNames {
    pub asset_name: Option<String>,
    pub file_name: Option<String>,
}

impl TitleAssetNames {
    pub fn from_source(asset_name: Option<&str>, path: &Path) -> Self {
        Self {
            asset_name: asset_name.map(str::to_owned),
            file_name: path.file_name().and_then(|name| name.to_str()).map(str::to_owned),
        }
    }
}

/// Names frozen from a host's Asset Library or an admitted export snapshot.
pub type TitleAssetMetadata = HashMap<AssetId, TitleAssetNames>;

/// Resolved author controls for a Clip Info title.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TitleClipInfo {
    pub field: TitleClipInfoField,
    /// Absent selects the topmost eligible video track.
    pub track_id: Option<TrackId>,
    /// Normalized anchor in the owner Sequence's author raster.
    pub position: [f32; 2],
}

impl TitleClipInfo {
    pub(super) fn read_from(properties: &PropertyBag) -> Result<Self> {
        let field = match required_enum(
            properties,
            BasicTitle::CLIP_INFO_FIELD_PATH,
            TimelineTime::ZERO,
        )?
        .as_str()
        {
            "clip_name" => TitleClipInfoField::ClipName,
            "reel_name" => TitleClipInfoField::ReelName,
            "asset_name" => TitleClipInfoField::AssetName,
            "file_name" => TitleClipInfoField::FileName,
            _ => return Err(title_error("unknown Clip Info field")),
        };
        let track_id = read_metadata_track(properties, BasicTitle::CLIP_INFO_TRACK_PATH)?;
        let Some(PropertyValue::Vec2(position)) =
            properties.evaluate(BasicTitle::CLIP_INFO_POSITION_PATH, TimelineTime::ZERO)
        else {
            return Err(title_error("Clip Info position must have two components"));
        };
        Ok(Self { field, track_id, position: position.to_array() })
    }
}

pub(super) fn read_metadata_track(properties: &PropertyBag, path: &str) -> Result<Option<TrackId>> {
    let track = required_text(properties, path, TimelineTime::ZERO)?;
    if track.is_empty() {
        return Ok(None);
    }
    let id = track
        .parse::<uuid::Uuid>()
        .map_err(|_| title_error("Metadata source track must be a video Track ID"))?;
    if id.is_nil() || id.to_string() != track {
        return Err(title_error(
            "Metadata source track must use a canonical Track ID",
        ));
    }
    Ok(Some(TrackId(id)))
}

pub(super) fn descriptors() -> [PropertyDescriptor; 3] {
    let field = |path, id, label, value| {
        PropertyDescriptor::new(path, label, value)
            .with_parameter_id(ParameterId::new_static(id))
            .with_animatable(false)
    };
    [
        field(
            BasicTitle::CLIP_INFO_FIELD_PATH,
            "mondrian.title.clip_info.field",
            "显示",
            PropertyValue::Enum("clip_name".to_owned()),
        )
        .with_enum_options(enum_options(&[
            ("clip_name", "Clip name"),
            ("reel_name", "Reel name"),
            ("asset_name", "Asset Library name"),
            ("file_name", "File name"),
        ])),
        field(
            BasicTitle::CLIP_INFO_TRACK_PATH,
            "mondrian.title.clip_info.track",
            "源轨道",
            PropertyValue::Text(String::new()),
        )
        .with_text_mode(ParameterTextMode::SingleLine),
        field(
            BasicTitle::CLIP_INFO_POSITION_PATH,
            "mondrian.title.clip_info.position",
            "位置",
            PropertyValue::Vec2([0.5, 0.88].into()),
        )
        .with_numeric_contract(
            ParameterUnit::Unitless,
            ParameterNumericContract::closed(
                0.0,
                1.0,
                Some(0.001),
                ParameterInvalidValuePolicy::Reject,
            )
            .expect("valid Clip Info normalized coordinate range"),
        ),
    ]
}
