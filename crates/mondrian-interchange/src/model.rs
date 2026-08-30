//! Public format profiles, media binding, and immutable request snapshots.

use mondrian_core::timeline_data::EditorialSourceIdentity;
use mondrian_core::{AssetId, ColorSpace};
use serde::{Deserialize, Serialize};

/// Qualified, version-pinned interchange profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterchangeFormatProfile {
    /// OpenTimelineIO native JSON using the pinned 0.18.1 core schema map.
    OtioJsonV1,
    /// Strict CMX 3600 A-mode picture conform EDL.
    Cmx3600,
    /// Apple Final Cut Pro 7 `xmeml version=5` sequence subset.
    Fcp7XmlV5,
    /// AAF Edit Protocol metadata-only external-media subset.
    AafEditProtocolV1,
}

impl InterchangeFormatProfile {
    /// Stable profile identifier carried in reports and helper handshakes.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OtioJsonV1 => "otio_json_v1",
            Self::Cmx3600 => "cmx3600",
            Self::Fcp7XmlV5 => "fcp7_xml_v5",
            Self::AafEditProtocolV1 => "aaf_edit_protocol_v1",
        }
    }
}

/// Whether a report was produced while reading or writing an artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterchangeDirection {
    /// Evidence produced while inspecting a foreign artifact.
    Import,
    /// Evidence produced while preparing a native artifact.
    Export,
}

/// Caller policy for semantics that the selected profile cannot preserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterchangeLossPolicy {
    /// Reject every approximation, omission, flatten, or required relink.
    RejectUnpreserved,
    /// Allow non-blocking losses and return the complete report for approval.
    AllowWithReport,
}

/// Track kind in the common exact interchange representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterchangeTrackKind {
    /// Picture track.
    Video,
    /// Sound track.
    Audio,
}

/// Stable foreign media identity used before an Asset Library binding exists.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterchangeMediaKey(pub String);

/// Untrusted media description discovered in an interchange artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterchangeMediaReference {
    /// Stable key used by Clip records and binding requests.
    pub key: InterchangeMediaKey,
    /// Foreign display name; never a locator authority.
    pub name: Option<String>,
    /// Source-provided path or URI retained only for relink presentation.
    pub proposed_locator: Option<String>,
    /// Placement/source identity retained for conform and round trip.
    pub editorial_source: Option<EditorialSourceIdentity>,
    /// Explicit media color identity, when the format provides one.
    pub color_space: Option<ColorSpace>,
}

/// Product-owned binding from a foreign key to an existing strong Asset ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterchangeMediaBinding {
    /// Foreign identity returned by inspection.
    pub key: InterchangeMediaKey,
    /// Existing canonical Asset Library record selected by the product.
    pub asset_id: AssetId,
}

/// Export-side immutable Asset Library projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterchangeAssetSnapshot {
    /// Canonical Asset identity referenced by the Sequence.
    pub asset_id: AssetId,
    /// Immutable export display name.
    pub name: String,
    /// Optional external-media locator projected into the native format.
    pub locator: Option<String>,
    /// Optional source identity used for conform and round trip.
    pub editorial_source: Option<EditorialSourceIdentity>,
    /// Optional canonical input color identity.
    pub color_space: Option<ColorSpace>,
}
