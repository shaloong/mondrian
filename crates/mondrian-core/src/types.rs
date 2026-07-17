//! 核心基础类型定义
//!
//! 包含时间码、帧率、分辨率、颜色、矩形区域等所有模块共用的值类型。

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use uuid::Uuid;

// ─── 强类型 ID ────────────────────────────────────────────────────────────────

macro_rules! define_id {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

define_id!(ProjectId, "项目 ID");
define_id!(SequenceId, "序列（时间线）ID");
define_id!(TrackId, "轨道 ID");
define_id!(ClipId, "剪辑片段 ID");
define_id!(AssetId, "素材资产 ID");
define_id!(CharacterId, "角色 ID");
define_id!(SceneId, "场景 ID");
define_id!(EffectId, "效果节点 ID");
define_id!(AnimationTrackId, "动画轨道 ID");
define_id!(KeyframeId, "关键帧 ID");
define_id!(AudioComponentEditId, "音频组件编辑 ID");
define_id!(AudioProcessingScopeId, "音频处理作用域 ID");
define_id!(AudioSourceComponentId, "音频源组件 ID");
define_id!(AudioTransitionId, "音频转场 ID");
define_id!(AudioRouteId, "音频路由 ID");
define_id!(AudioProcessorInstanceId, "音频处理器实例 ID");
define_id!(MixBusId, "混音总线 ID");
define_id!(ProgramOutputId, "节目输出 ID");
define_id!(AudioRoleId, "音频角色 ID");
define_id!(JobId, "渲染任务 ID");
define_id!(MaskId, "蒙版 ID");

impl AudioSourceComponentId {
    /// Stable logical identity for the asset's explicitly selected primary
    /// audio component. Adapters must reject, not reinterpret, unknown IDs.
    pub const fn primary() -> Self {
        Self(Uuid::from_u128(0x8f43_8809_9067_4d17_90ec_2bb7_b172_462e))
    }
}

// ─── 时间码（帧精确，有理数）──────────────────────────────────────────────────

/// 有理数（分子/分母）
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Rational {
    pub num: i64,
    pub den: i64,
}

impl Rational {
    pub const fn new(num: i64, den: i64) -> Self {
        Self { num, den }
    }

    pub fn to_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    /// 约分
    pub fn reduce(self) -> Self {
        let g = gcd(self.num.unsigned_abs(), self.den.unsigned_abs()) as i64;
        Self { num: self.num / g, den: self.den / g }
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// 常用帧率预设
impl Rational {
    pub const FPS_10: Self = Self::new(10, 1);
    pub const FPS_12: Self = Self::new(12, 1);
    pub const FPS_125: Self = Self::new(25, 2);
    pub const FPS_15: Self = Self::new(15, 1);
    pub const FPS_24: Self = Self::new(24, 1);
    pub const FPS_23976: Self = Self::new(24000, 1001); // 23.976...
    pub const FPS_25: Self = Self::new(25, 1);
    pub const FPS_30: Self = Self::new(30, 1);
    pub const FPS_2997: Self = Self::new(30000, 1001); // 29.97 NTSC
    pub const FPS_50: Self = Self::new(50, 1);
    pub const FPS_60: Self = Self::new(60, 1);
    pub const FPS_5994: Self = Self::new(60000, 1001);

    pub const SEQUENCE_FRAME_RATES: [Self; 12] = [
        Self::FPS_10,
        Self::FPS_12,
        Self::FPS_125,
        Self::FPS_15,
        Self::FPS_23976,
        Self::FPS_24,
        Self::FPS_25,
        Self::FPS_2997,
        Self::FPS_30,
        Self::FPS_50,
        Self::FPS_5994,
        Self::FPS_60,
    ];
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{}/{}", self.num, self.den)
        }
    }
}

/// 帧精确时间码
///
/// 内部以帧数 + 时间基（帧率的倒数）表示：
/// `seconds = frame * time_base = frame * (1 / fps)`
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FramePosition {
    /// 帧编号（可为负数，用于 offset）
    pub frame: i64,
    /// 时间基 = 1/fps，例如 25fps → Rational{1, 25}
    pub time_base: Rational,
}

impl FramePosition {
    pub const ZERO: Self = Self { frame: 0, time_base: Rational::FPS_25 };

    pub fn new(frame: i64, time_base: Rational) -> Self {
        Self { frame, time_base }
    }
}

// ─── 分辨率 ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Resolution {
    pub const HD: Self = Self { width: 1280, height: 720 };
    pub const FHD: Self = Self { width: 1920, height: 1080 };
    pub const UHD4K: Self = Self { width: 3840, height: 2160 };
    pub const DCI4K: Self = Self { width: 4096, height: 2160 };

    pub fn aspect_ratio(self) -> f32 {
        self.width as f32 / self.height as f32
    }
}

impl fmt::Display for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}×{}", self.width, self.height)
    }
}

// ─── 颜色（线性 RGBA f32）─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const BLACK: Self = Self { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
    pub const WHITE: Self = Self { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };
    pub const TRANSPARENT: Self = Self { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };

    pub fn from_hex(hex: u32) -> Self {
        let r = ((hex >> 16) & 0xFF) as f32 / 255.0;
        let g = ((hex >> 8) & 0xFF) as f32 / 255.0;
        let b = (hex & 0xFF) as f32 / 255.0;
        Self { r, g, b, a: 1.0 }
    }

    pub fn lerp(self, other: Self, t: f32) -> Self {
        Self {
            r: self.r + (other.r - self.r) * t,
            g: self.g + (other.g - self.g) * t,
            b: self.b + (other.b - self.b) * t,
            a: self.a + (other.a - self.a) * t,
        }
    }
}

// ─── 矩形区域 ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self { x, y, width, height }
    }

    pub fn contains_point(self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.x + self.width && py >= self.y && py <= self.y + self.height
    }
}

// ─── 混合模式 ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Dissolve,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Subtract,
    DarkerColor,
    LighterColor,
    LinearBurn,
    LinearDodge,
    VividLight,
    LinearLight,
    PinLight,
    HardMix,
    Divide,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

// ─── 色彩空间 ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ColorSpace {
    #[default]
    Rec709,
    /// 625-line Rec.601 / PAL-family SDR using BT.470BG primaries.
    Rec601Pal,
    /// 525-line Rec.601 / NTSC-family SDR using SMPTE 170M primaries.
    Rec601Ntsc,
    Rec2100Hlg,
    Rec2100Pq,
    Srgb,
    Rec2020,
    /// Apple Display P3: P3-D65 primaries with the IEC 61966-2-1 sRGB curve.
    DisplayP3,
    /// Scene-linear BT.709/sRGB primaries, typically from float image media.
    LinearRec709,
    /// Scene-linear BT.2020 primaries, typically from float image media.
    LinearRec2020,
    /// Scene-linear P3-D65 primaries, typically from float image media.
    LinearP3D65,
    /// ACES2065-1/AP0 scene-linear interchange media.
    Aces2065_1,
    /// ACEScg/AP1 scene-linear media.
    AcesCg,
    /// ACEScct/AP1 scene-referred logarithmic media.
    AcesCct,
    /// Apple Log with BT.2020 primaries.
    AppleLogBt2020,
    /// Sony S-Log2 with S-Gamut primaries.
    SonySLog2SGamut,
    /// Sony S-Log3 with S-Gamut3 primaries.
    SonySLog3SGamut3,
    /// Sony S-Log3 with S-Gamut3.Cine primaries.
    SonySLog3SGamut3Cine,
    /// ARRI LogC3 EI800 with ARRI Wide Gamut 3 primaries.
    ArriLogC3WideGamut3,
    /// ARRI LogC4 with ARRI Wide Gamut 4 primaries.
    ArriLogC4WideGamut4,
    /// Canon Log 2 with Cinema Gamut D55 primaries.
    CanonLog2CinemaGamutD55,
    /// Canon Log 3 with Cinema Gamut D55 primaries.
    CanonLog3CinemaGamutD55,
    /// Panasonic V-Log with V-Gamut primaries.
    PanasonicVLogVGamut,
    /// RED Log3G10 with REDWideGamutRGB primaries.
    RedLog3G10WideGamutRgb,
    /// Blackmagic Film Gen 5 with Blackmagic Wide Gamut Gen 5 primaries.
    BlackmagicFilmWideGamutGen5,
    /// DJI D-Log with D-Gamut primaries.
    DjiDLogDGamut,
    /// DaVinci Intermediate with DaVinci Wide Gamut primaries.
    DavinciIntermediateWideGamut,
}

/// Linear-light color space used for rendering, effects, and compositing.
///
/// Working spaces describe chromaticities only; they never imply a camera or
/// display transfer function. This prevents encoded source/output identities
/// such as PQ, HLG, or LogC from entering linear-light processing by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkingColorSpace {
    /// Linear-light BT.709/sRGB primaries with a D65 white point.
    #[default]
    LinearRec709,
    /// Linear-light BT.2020 primaries with a D65 white point.
    LinearRec2020,
    /// Linear-light P3-D65 primaries.
    LinearP3D65,
    /// ACEScg/AP1 scene-linear working space.
    AcesCg,
}

/// Error returned when an encoded source/output space is not a valid working space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{color_space:?} cannot be used as a linear working color space")]
pub struct InvalidWorkingColorSpace {
    /// Encoded color space rejected by the conversion.
    pub color_space: ColorSpace,
}

impl TryFrom<ColorSpace> for WorkingColorSpace {
    type Error = InvalidWorkingColorSpace;

    fn try_from(color_space: ColorSpace) -> Result<Self, Self::Error> {
        match color_space {
            ColorSpace::LinearRec709 => Ok(Self::LinearRec709),
            ColorSpace::LinearRec2020 => Ok(Self::LinearRec2020),
            ColorSpace::LinearP3D65 => Ok(Self::LinearP3D65),
            ColorSpace::AcesCg => Ok(Self::AcesCg),
            ColorSpace::Rec709
            | ColorSpace::Rec601Pal
            | ColorSpace::Rec601Ntsc
            | ColorSpace::Rec2100Hlg
            | ColorSpace::Rec2100Pq
            | ColorSpace::Srgb
            | ColorSpace::Rec2020
            | ColorSpace::DisplayP3
            | ColorSpace::Aces2065_1
            | ColorSpace::AcesCct
            | ColorSpace::AppleLogBt2020
            | ColorSpace::SonySLog2SGamut
            | ColorSpace::SonySLog3SGamut3
            | ColorSpace::SonySLog3SGamut3Cine
            | ColorSpace::ArriLogC3WideGamut3
            | ColorSpace::ArriLogC4WideGamut4
            | ColorSpace::CanonLog2CinemaGamutD55
            | ColorSpace::CanonLog3CinemaGamutD55
            | ColorSpace::PanasonicVLogVGamut
            | ColorSpace::RedLog3G10WideGamutRgb
            | ColorSpace::BlackmagicFilmWideGamutGen5
            | ColorSpace::DjiDLogDGamut
            | ColorSpace::DavinciIntermediateWideGamut => {
                Err(InvalidWorkingColorSpace { color_space })
            }
        }
    }
}

/// Explicit OCIO processor endpoint identity.
///
/// Color identities resolve external source/delivery color spaces, including
/// scene-linear float media. Working identities resolve internal render spaces.
/// Processor caches include this enum so equal chromaticities in different
/// pipeline roles cannot alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "role", content = "space", rename_all = "snake_case")]
pub enum OcioColorSpaceIdentity {
    /// External source, display, or delivery color space.
    Color(ColorSpace),
    /// Linear-light rendering color space.
    Working(WorkingColorSpace),
}

impl OcioColorSpaceIdentity {
    /// Return the external source/delivery space when present.
    pub const fn color(self) -> Option<ColorSpace> {
        match self {
            Self::Color(space) => Some(space),
            Self::Working(_) => None,
        }
    }

    /// Return the linear rendering space when this identity is a working space.
    pub const fn working(self) -> Option<WorkingColorSpace> {
        match self {
            Self::Color(_) => None,
            Self::Working(space) => Some(space),
        }
    }
}

impl From<ColorSpace> for OcioColorSpaceIdentity {
    fn from(value: ColorSpace) -> Self {
        Self::Color(value)
    }
}

impl From<WorkingColorSpace> for OcioColorSpaceIdentity {
    fn from(value: WorkingColorSpace) -> Self {
        Self::Working(value)
    }
}

/// Versioned Mondrian Standard package semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MondrianStandardVersion {
    /// First immutable Mondrian Standard OCIO package contract.
    V1,
    /// Second package contract adding a display-referred Rec.2020 SDR output.
    V2,
    /// Third package contract adding the target-aware segmented Standard SDR View.
    V3,
}

/// Stable product identifier for the bundled Mondrian Standard package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MondrianStandardPackageId {
    /// Mondrian's stock-OCIO product package.
    #[serde(rename = "mondrian_standard")]
    MondrianStandard,
}

/// Stable identity for the OCIO config contained in a Standard package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MondrianStandardConfigId {
    /// Mondrian Standard v2's embedded OCIO configuration.
    #[serde(rename = "mondrian_default_ocio_v2")]
    V2,
}

/// Exact SHA-256 identity of the OCIO config text in a Standard package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MondrianStandardConfigDigest {
    /// Digest of `mondrian_default_ocio_v2.ocio`.
    #[serde(rename = "99416ff04d756a3d490778d4a6e3df32a24b5adbea1c7fa890c5c7caa31a78f6")]
    V2,
}

/// Exact SHA-256 identity of a complete Standard config-and-resource package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MondrianStandardPackageDigest {
    /// Digest of Standard v2's config plus every embedded resource.
    #[serde(rename = "3f8bd02e4c79f081c9c09fba3053161c4af14de6effcf6d76b78e2339b9c2881")]
    V2,
    /// Digest of Standard v3's config, segmented SDR graph, and HDR resource.
    #[serde(rename = "462bea568f3babcd76030fd219a54dcde8a6fa2acece31e20883a9ba98501e61")]
    V3,
}

/// Stable versioned identity of the Standard compositing working space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MondrianStandardWorkingSpaceId {
    /// Scene-linear, D65, Rec.2020 primaries, unbounded float RGB.
    #[serde(rename = "linear_rec2020_v1")]
    LinearRec2020V1,
}

/// Stable versioned identity of Standard v1's SDR rendering transform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MondrianStandardSdrViewTransformId {
    /// Mondrian Standard SDR scene-to-display rendering transform v1.
    #[serde(rename = "mondrian_standard_sdr_v1")]
    V1,
    /// Target-aware, SDR-preserving segmented rendering transform v2.
    #[serde(rename = "mondrian_standard_sdr_v2")]
    V2,
}

/// Stable versioned identity of Standard v1's 1000-nit HDR rendering transform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MondrianStandardHdrViewTransformId {
    /// Mondrian Standard 1000-nit scene-to-display rendering transform v1.
    #[serde(rename = "mondrian_standard_hdr_1000_nits_v1")]
    Hdr1000V1,
}

/// Fully pinned identity persisted for a Mondrian Standard project mode.
///
/// Every field is intentionally required. Runtime package resolution accepts
/// only one complete declared identity and rejects mismatched or partially
/// edited project payloads instead of resolving them to the current package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MondrianStandardPackageIdentity {
    package_id: MondrianStandardPackageId,
    package_version: MondrianStandardVersion,
    config_id: MondrianStandardConfigId,
    config_sha256: MondrianStandardConfigDigest,
    package_sha256: MondrianStandardPackageDigest,
    working_space_id: MondrianStandardWorkingSpaceId,
    working_space_version: MondrianStandardVersion,
    sdr_view_transform_id: MondrianStandardSdrViewTransformId,
    sdr_view_transform_version: MondrianStandardVersion,
    hdr_view_transform_id: MondrianStandardHdrViewTransformId,
    hdr_view_transform_version: MondrianStandardVersion,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SerializedMondrianStandardPackageIdentity {
    package_id: MondrianStandardPackageId,
    package_version: MondrianStandardVersion,
    config_id: MondrianStandardConfigId,
    config_sha256: MondrianStandardConfigDigest,
    package_sha256: MondrianStandardPackageDigest,
    working_space_id: MondrianStandardWorkingSpaceId,
    working_space_version: MondrianStandardVersion,
    sdr_view_transform_id: MondrianStandardSdrViewTransformId,
    sdr_view_transform_version: MondrianStandardVersion,
    hdr_view_transform_id: MondrianStandardHdrViewTransformId,
    hdr_view_transform_version: MondrianStandardVersion,
}

impl<'de> Deserialize<'de> for MondrianStandardPackageIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let serialized = SerializedMondrianStandardPackageIdentity::deserialize(deserializer)?;
        let identity = Self {
            package_id: serialized.package_id,
            package_version: serialized.package_version,
            config_id: serialized.config_id,
            config_sha256: serialized.config_sha256,
            package_sha256: serialized.package_sha256,
            working_space_id: serialized.working_space_id,
            working_space_version: serialized.working_space_version,
            sdr_view_transform_id: serialized.sdr_view_transform_id,
            sdr_view_transform_version: serialized.sdr_view_transform_version,
            hdr_view_transform_id: serialized.hdr_view_transform_id,
            hdr_view_transform_version: serialized.hdr_view_transform_version,
        };
        if identity == Self::V2 || identity == Self::V3 {
            Ok(identity)
        } else {
            Err(serde::de::Error::custom(
                "unsupported or internally inconsistent Mondrian Standard package identity",
            ))
        }
    }
}

/// One OCIO role binding pinned by a Custom OCIO project.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomOcioRoleIdentity {
    role: String,
    color_space: String,
}

impl CustomOcioRoleIdentity {
    pub(crate) fn new(role: String, color_space: String) -> Self {
        Self { role, color_space }
    }

    /// Exact role name authored by the pinned config.
    pub fn role(&self) -> &str {
        &self.role
    }

    /// Exact color-space name bound to the role.
    pub fn color_space(&self) -> &str {
        &self.color_space
    }
}

/// Effective look selection pinned for a Custom OCIO display/view.
///
/// A tagged value is used instead of `Option<String>` so a missing project
/// field cannot deserialize as an implicit `None`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "selection", rename_all = "snake_case", deny_unknown_fields)]
pub enum CustomOcioLookIdentity {
    /// The selected display/view applies no looks.
    None,
    /// Exact OCIO looks expression authored on the selected display/view.
    DisplayView { looks: String },
}

/// One exact Custom OCIO rendering View bound to a Mondrian output target.
///
/// OCIO display/view names do not, by themselves, identify the encoded signal
/// that Mondrian must tag in a deliverable. The binding therefore persists both
/// the standardized output identity and OCIO's exact display color-space name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomOcioOutputIdentity {
    output_color_space: ColorSpace,
    display: String,
    view: String,
    display_color_space: String,
    look: CustomOcioLookIdentity,
}

impl CustomOcioOutputIdentity {
    pub(crate) fn from_resolved(
        output_color_space: ColorSpace,
        display: String,
        view: String,
        display_color_space: String,
        look: CustomOcioLookIdentity,
    ) -> Self {
        Self {
            output_color_space,
            display,
            view,
            display_color_space,
            look,
        }
    }

    /// Reconstruct one persisted output binding from already-pinned fields.
    pub fn from_pinned_parts(
        output_color_space: ColorSpace,
        display: String,
        view: String,
        display_color_space: String,
        look: CustomOcioLookIdentity,
    ) -> Result<Self, String> {
        let output =
            Self::from_resolved(output_color_space, display, view, display_color_space, look);
        output.validate_structure()?;
        Ok(output)
    }

    fn validate_structure(&self) -> Result<(), String> {
        if !self.output_color_space.is_display_referred() {
            return Err(format!(
                "Custom OCIO output binding target {:?} is not display-referred",
                self.output_color_space
            ));
        }
        for (field, value) in [
            ("display", self.display.as_str()),
            ("view", self.view.as_str()),
            ("display color space", self.display_color_space.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!(
                    "Custom OCIO output binding {field} must not be blank"
                ));
            }
        }
        if matches!(&self.look, CustomOcioLookIdentity::DisplayView { looks } if looks.trim().is_empty())
        {
            return Err("Custom OCIO display/view look expression must not be blank".to_owned());
        }
        Ok(())
    }

    /// Standardized encoded output identity used for scopes and container tags.
    pub const fn output_color_space(&self) -> ColorSpace {
        self.output_color_space
    }

    /// Exact OCIO display name.
    pub fn display(&self) -> &str {
        &self.display
    }

    /// Exact OCIO View name under [`Self::display`].
    pub fn view(&self) -> &str {
        &self.view
    }

    /// Exact OCIO display color-space endpoint reported for this View.
    pub fn display_color_space(&self) -> &str {
        &self.display_color_space
    }

    /// Effective look expression of this display/view.
    pub fn look(&self) -> &CustomOcioLookIdentity {
        &self.look
    }
}

/// A project-authored OCIO dynamic-property override.
///
/// Values are canonical strings because OCIO dynamic properties include both
/// scalar and structured grading values. Mondrian currently persists an empty
/// list and fails closed on non-empty values until the corresponding typed
/// editing/execution contract is implemented.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomOcioDynamicPropertyIdentity {
    property: String,
    value: String,
}

impl CustomOcioDynamicPropertyIdentity {
    /// Exact OCIO dynamic-property name.
    pub fn property(&self) -> &str {
        &self.property
    }

    /// Canonical serialized property value.
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// Fully pinned, reproducible identity of a Custom OpenColorIO project mode.
///
/// The source locator is intentionally insufficient by itself: a path or
/// environment variable may later resolve to different config text or LUT
/// resources. `config_sha256` identifies the primary config content,
/// `resolved_cache_id` identifies the parsed OCIO graph, and
/// `processor_graph_sha256` covers the executable routes and their resources.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct CustomOcioProjectIdentity {
    source: OcioConfigSource,
    config_sha256: String,
    resolved_cache_id: String,
    processor_graph_sha256: String,
    working_space: String,
    outputs: Vec<CustomOcioOutputIdentity>,
    roles: Vec<CustomOcioRoleIdentity>,
    dynamic_properties: Vec<CustomOcioDynamicPropertyIdentity>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SerializedCustomOcioProjectIdentity {
    source: OcioConfigSource,
    config_sha256: String,
    resolved_cache_id: String,
    processor_graph_sha256: String,
    working_space: String,
    outputs: Vec<CustomOcioOutputIdentity>,
    roles: Vec<CustomOcioRoleIdentity>,
    dynamic_properties: Vec<CustomOcioDynamicPropertyIdentity>,
}

impl<'de> Deserialize<'de> for CustomOcioProjectIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let serialized = SerializedCustomOcioProjectIdentity::deserialize(deserializer)?;
        Self::from_pinned_parts(
            serialized.source,
            serialized.config_sha256,
            serialized.resolved_cache_id,
            serialized.processor_graph_sha256,
            serialized.working_space,
            serialized.outputs,
            serialized.roles,
            serialized.dynamic_properties,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl CustomOcioProjectIdentity {
    pub(crate) fn from_resolved(
        source: OcioConfigSource,
        config_sha256: String,
        resolved_cache_id: String,
        processor_graph_sha256: String,
        working_space: String,
        outputs: Vec<CustomOcioOutputIdentity>,
        roles: Vec<CustomOcioRoleIdentity>,
    ) -> Self {
        Self {
            source,
            config_sha256,
            resolved_cache_id,
            processor_graph_sha256,
            working_space,
            outputs,
            roles,
            dynamic_properties: Vec::new(),
        }
    }

    /// Reconstruct a persisted identity from already-pinned fields.
    ///
    /// This performs structural validation only. The selected config and all
    /// dependencies are verified against the fields by `ColorEngine::ensure_loaded`
    /// and every processor-construction boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn from_pinned_parts(
        source: OcioConfigSource,
        config_sha256: String,
        resolved_cache_id: String,
        processor_graph_sha256: String,
        working_space: String,
        mut outputs: Vec<CustomOcioOutputIdentity>,
        mut roles: Vec<CustomOcioRoleIdentity>,
        dynamic_properties: Vec<CustomOcioDynamicPropertyIdentity>,
    ) -> Result<Self, String> {
        if matches!(source, OcioConfigSource::MondrianStandard { .. }) {
            return Err("Custom OCIO cannot claim the Mondrian Standard config source".to_owned());
        }
        if config_sha256.len() != 64 || !config_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("Custom OCIO config SHA-256 must contain exactly 64 hex digits".to_owned());
        }
        if processor_graph_sha256.len() != 64
            || !processor_graph_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(
                "Custom OCIO processor-graph SHA-256 must contain exactly 64 hex digits".to_owned(),
            );
        }
        for (field, value) in [
            ("resolved cache-id", resolved_cache_id.as_str()),
            ("working space", working_space.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("Custom OCIO {field} must not be blank"));
            }
        }
        if outputs.is_empty() {
            return Err("Custom OCIO project must pin at least one output binding".to_owned());
        }
        for output in &outputs {
            output.validate_structure()?;
        }
        outputs.sort_by_key(|output| {
            ColorSpace::ALL
                .iter()
                .position(|candidate| *candidate == output.output_color_space)
                .unwrap_or(usize::MAX)
        });
        if outputs
            .windows(2)
            .any(|pair| pair[0].output_color_space == pair[1].output_color_space)
        {
            return Err("Custom OCIO output binding targets must be unique".to_owned());
        }
        for (index, output) in outputs.iter().enumerate() {
            if outputs[index + 1..].iter().any(|candidate| {
                candidate.display == output.display && candidate.view == output.view
            }) {
                return Err(format!(
                    "Custom OCIO display/view '{}/{}' cannot label multiple output targets",
                    output.display, output.view
                ));
            }
        }
        if roles
            .iter()
            .any(|role| role.role.trim().is_empty() || role.color_space.trim().is_empty())
        {
            return Err("Custom OCIO role names and color spaces must not be blank".to_owned());
        }
        roles.sort_by(|left, right| left.role.cmp(&right.role));
        if roles.windows(2).any(|pair| pair[0].role == pair[1].role) {
            return Err("Custom OCIO role names must be unique".to_owned());
        }
        if dynamic_properties
            .iter()
            .any(|property| property.property.trim().is_empty() || property.value.trim().is_empty())
        {
            return Err(
                "Custom OCIO dynamic-property names and values must not be blank".to_owned(),
            );
        }
        Ok(Self {
            source,
            config_sha256: config_sha256.to_ascii_lowercase(),
            resolved_cache_id,
            processor_graph_sha256: processor_graph_sha256.to_ascii_lowercase(),
            working_space,
            outputs,
            roles,
            dynamic_properties,
        })
    }

    /// Config locator selected when this identity was created.
    pub fn source(&self) -> &OcioConfigSource {
        &self.source
    }

    /// SHA-256 of the primary authored/serialized config content.
    pub fn config_sha256(&self) -> &str {
        &self.config_sha256
    }

    /// OCIO cache identity of the parsed config graph.
    pub fn resolved_cache_id(&self) -> &str {
        &self.resolved_cache_id
    }

    /// SHA-256 over all working-space routes and selected output processors.
    pub fn processor_graph_sha256(&self) -> &str {
        &self.processor_graph_sha256
    }

    /// Exact scene-linear working color-space name.
    pub fn working_space(&self) -> &str {
        &self.working_space
    }

    /// Complete output bindings sorted by standardized output identity.
    pub fn outputs(&self) -> &[CustomOcioOutputIdentity] {
        &self.outputs
    }

    /// Resolve the one View explicitly bound to a standardized output target.
    pub fn output(&self, output_color_space: ColorSpace) -> Option<&CustomOcioOutputIdentity> {
        self.outputs
            .iter()
            .find(|output| output.output_color_space == output_color_space)
    }

    /// Complete sorted role mapping of the pinned config.
    pub fn roles(&self) -> &[CustomOcioRoleIdentity] {
        &self.roles
    }

    /// Project-level dynamic-property overrides.
    pub fn dynamic_properties(&self) -> &[CustomOcioDynamicPropertyIdentity] {
        &self.dynamic_properties
    }
}

impl MondrianStandardPackageIdentity {
    /// Exact identity of the legacy Standard v2 package.
    pub const V2: Self = Self {
        package_id: MondrianStandardPackageId::MondrianStandard,
        package_version: MondrianStandardVersion::V2,
        config_id: MondrianStandardConfigId::V2,
        config_sha256: MondrianStandardConfigDigest::V2,
        package_sha256: MondrianStandardPackageDigest::V2,
        working_space_id: MondrianStandardWorkingSpaceId::LinearRec2020V1,
        working_space_version: MondrianStandardVersion::V1,
        sdr_view_transform_id: MondrianStandardSdrViewTransformId::V1,
        sdr_view_transform_version: MondrianStandardVersion::V1,
        hdr_view_transform_id: MondrianStandardHdrViewTransformId::Hdr1000V1,
        hdr_view_transform_version: MondrianStandardVersion::V1,
    };

    /// Exact identity of the current Standard v3 package.
    pub const V3: Self = Self {
        package_id: MondrianStandardPackageId::MondrianStandard,
        package_version: MondrianStandardVersion::V3,
        config_id: MondrianStandardConfigId::V2,
        config_sha256: MondrianStandardConfigDigest::V2,
        package_sha256: MondrianStandardPackageDigest::V3,
        working_space_id: MondrianStandardWorkingSpaceId::LinearRec2020V1,
        working_space_version: MondrianStandardVersion::V1,
        sdr_view_transform_id: MondrianStandardSdrViewTransformId::V2,
        sdr_view_transform_version: MondrianStandardVersion::V2,
        hdr_view_transform_id: MondrianStandardHdrViewTransformId::Hdr1000V1,
        hdr_view_transform_version: MondrianStandardVersion::V1,
    };

    /// Stable package identifier used by diagnostics and fingerprints.
    pub const fn package_id(self) -> &'static str {
        "mondrian_standard"
    }

    /// Stable embedded OCIO config identifier.
    pub const fn config_id(self) -> &'static str {
        "mondrian_default_ocio_v2"
    }

    /// Exact SHA-256 digest of the embedded OCIO config text.
    pub const fn config_sha256(self) -> &'static str {
        "99416ff04d756a3d490778d4a6e3df32a24b5adbea1c7fa890c5c7caa31a78f6"
    }

    /// Exact SHA-256 digest of the config and all embedded resources.
    pub const fn package_sha256(self) -> &'static str {
        match self.package_sha256 {
            MondrianStandardPackageDigest::V2 => {
                "3f8bd02e4c79f081c9c09fba3053161c4af14de6effcf6d76b78e2339b9c2881"
            }
            MondrianStandardPackageDigest::V3 => {
                "462bea568f3babcd76030fd219a54dcde8a6fa2acece31e20883a9ba98501e61"
            }
        }
    }

    /// Versioned working-space identity pinned by this package.
    pub const fn working_space_id(self) -> &'static str {
        "linear_rec2020_v1"
    }

    /// Runtime working-space value defined by this immutable package.
    pub const fn working_color_space(self) -> WorkingColorSpace {
        match self.working_space_id {
            MondrianStandardWorkingSpaceId::LinearRec2020V1 => WorkingColorSpace::LinearRec2020,
        }
    }

    /// Versioned SDR view-transform identity pinned by this package.
    pub const fn sdr_view_transform_id(self) -> &'static str {
        match self.sdr_view_transform_id {
            MondrianStandardSdrViewTransformId::V1 => "mondrian_standard_sdr_v1",
            MondrianStandardSdrViewTransformId::V2 => "mondrian_standard_sdr_v2",
        }
    }

    /// Versioned 1000-nit HDR view-transform identity pinned by this package.
    pub const fn hdr_view_transform_id(self) -> &'static str {
        "mondrian_standard_hdr_1000_nits_v1"
    }
}

/// 色彩空间转换 provider 的统一分发点。
///
/// 该枚举选择产品级 OCIO 模式与配置来源。最终输出意图仍由
/// `OutputTransformIntent` 独立选择，renderer 不从可选字符串猜测产品语义。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ColorEngine {
    /// Mondrian Standard default mode.
    ///
    /// 这是产品化策略入口：普通用户看到简化 UI，底层选择 Mondrian 固定发布、
    /// 版本化且不可由外部配置覆盖的 OCIO package。必需的 config/processor
    /// 不可用时应显式报错。
    #[serde(rename = "mondrian_standard")]
    MondrianStandard {
        /// Complete immutable package identity saved with the project.
        package: MondrianStandardPackageIdentity,
    },
    /// Official ACES configuration pinned by a product preset.
    Aces {
        /// Exact ACES/OCIO package release selected by the project.
        preset: AcesConfigPreset,
    },
    /// User- or studio-supplied OpenColorIO configuration.
    CustomOcio {
        /// Complete config and project-semantic identity.
        identity: Box<CustomOcioProjectIdentity>,
    },
}

impl ColorEngine {
    /// Select the exact Mondrian Standard package supported by this schema.
    pub const fn mondrian_standard() -> Self {
        Self::MondrianStandard { package: MondrianStandardPackageIdentity::V3 }
    }

    /// Return the pinned Standard package identity when this is Standard mode.
    pub const fn mondrian_standard_package(&self) -> Option<MondrianStandardPackageIdentity> {
        match self {
            Self::MondrianStandard { package } => Some(*package),
            Self::Aces { .. } | Self::CustomOcio { .. } => None,
        }
    }

    /// Return the pinned Custom OCIO project identity for this mode.
    pub fn custom_ocio_identity(&self) -> Option<&CustomOcioProjectIdentity> {
        match self {
            Self::CustomOcio { identity } => Some(identity.as_ref()),
            Self::MondrianStandard { .. } | Self::Aces { .. } => None,
        }
    }
}

impl Default for ColorEngine {
    fn default() -> Self {
        Self::mondrian_standard()
    }
}

/// Versioned official ACES configurations exposed as a first-class project mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AcesConfigPreset {
    /// OCIO Studio Config v4.0.0, ACES 2.0, authored for OCIO 2.5.
    #[default]
    StudioV4Aces2Ocio25,
    /// OCIO CG Config v4.0.0, ACES 2.0, authored for OCIO 2.5.
    CgV4Aces2Ocio25,
}

impl AcesConfigPreset {
    /// Display identity pinned by this immutable ACES config release.
    pub const fn default_display(self) -> &'static str {
        "sRGB - Display"
    }

    /// View identity pinned by this immutable ACES config release.
    pub const fn default_view(self) -> &'static str {
        "ACES 2.0 - SDR 100 nits (Rec.709)"
    }

    /// Exact default display/view identity of this preset.
    pub const fn default_display_view(self) -> (&'static str, &'static str) {
        (self.default_display(), self.default_view())
    }

    /// Resolve an encoded Program Output target to the exact rendering View
    /// shipped by this immutable ACES config preset.
    ///
    /// `None` means the preset has no rendering View for that target. Callers
    /// must not substitute the preset default and relabel the resulting signal.
    pub const fn output_display_view(
        self,
        output_color_space: ColorSpace,
    ) -> Option<(&'static str, &'static str)> {
        match (self, output_color_space) {
            (_, ColorSpace::Srgb) => Some(("sRGB - Display", "ACES 2.0 - SDR 100 nits (Rec.709)")),
            (_, ColorSpace::Rec709) => Some((
                "Rec.1886 Rec.709 - Display",
                "ACES 2.0 - SDR 100 nits (Rec.709)",
            )),
            (_, ColorSpace::DisplayP3) => {
                Some(("Display P3 - Display", "ACES 2.0 - SDR 100 nits (P3 D65)"))
            }
            (Self::StudioV4Aces2Ocio25, ColorSpace::Rec2100Hlg) => Some((
                "Rec.2100-HLG - Display",
                "ACES 2.0 - HDR 1000 nits (P3 D65)",
            )),
            (_, ColorSpace::Rec2100Pq) => Some((
                "Rec.2100-PQ - Display",
                "ACES 2.0 - HDR 1000 nits (Rec.2020)",
            )),
            _ => None,
        }
    }

    /// Exact built-in OCIO registry identifier for this immutable preset.
    pub const fn builtin_name(self) -> &'static str {
        match self {
            Self::StudioV4Aces2Ocio25 => "studio-config-v4.0.0_aces-v2.0_ocio-v2.5",
            Self::CgV4Aces2Ocio25 => "cg-config-v4.0.0_aces-v2.0_ocio-v2.5",
        }
    }

    /// Resolve the preset to the shared OCIO config-source contract.
    pub fn ocio_source(self) -> OcioConfigSource {
        OcioConfigSource::Builtin { name: self.builtin_name().to_owned() }
    }
}

/// 如何定位 OCIO 配置。
///
/// 类似达芬奇的色彩科学选择器（预设）和 Nuke 的显式 OCIO 来源。
/// 每个来源都必须独立解析成功，不能静默回退到其他来源。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum OcioConfigSource {
    /// Mondrian 内置默认 OCIO config。
    ///
    /// 这是 Standard / Simple 模式使用的固定 OCIO package 来源。UI 可以隐藏
    /// OCIO 细节，但外部 config 不能覆盖该版本的定义。
    #[serde(rename = "mondrian_standard")]
    MondrianStandard {
        /// Exact immutable Standard package whose OCIO graph must be assembled.
        package: MondrianStandardPackageIdentity,
    },
    /// 使用 `OCIO` 环境变量（行业标准）。
    /// 未设置或指向缺失文件时必须显式报错，不能扫描系统路径回退。
    #[default]
    #[serde(rename = "environment")]
    Environment,
    /// 使用内置 OCIO 配置（如 ACES 1.2、CG Config）。
    /// 名称可通过 [`crate::ocio::builtin_config_names`] 获取。
    #[serde(rename = "builtin")]
    Builtin { name: String },
    /// 显式指定 `config.ocio` 文件路径。
    #[serde(rename = "path")]
    Path { path: PathBuf },
}

impl fmt::Display for OcioConfigSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MondrianStandard { package } => {
                write!(f, "Mondrian Standard ({})", package.package_sha256())
            }
            Self::Environment => write!(f, "$OCIO"),
            Self::Builtin { name } => write!(f, "内置: {name}"),
            Self::Path { path } => write!(f, "{}", path.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_scene_linear_color_spaces_convert_to_working_spaces() {
        assert_eq!(
            WorkingColorSpace::try_from(ColorSpace::LinearRec2020),
            Ok(WorkingColorSpace::LinearRec2020)
        );
        assert_eq!(
            WorkingColorSpace::try_from(ColorSpace::AcesCg),
            Ok(WorkingColorSpace::AcesCg)
        );
        assert!(matches!(
            WorkingColorSpace::try_from(ColorSpace::Rec2100Pq),
            Err(InvalidWorkingColorSpace { color_space: ColorSpace::Rec2100Pq })
        ));
    }

    #[test]
    fn ocio_identity_keeps_external_color_and_working_spaces_distinct() {
        assert_ne!(
            OcioColorSpaceIdentity::Color(ColorSpace::LinearRec709),
            OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec709)
        );
    }

    // ── OCIO config source / color engine serde ────────────────────────────

    #[test]
    fn ocio_config_source_round_trip() {
        let sources = vec![
            OcioConfigSource::MondrianStandard { package: MondrianStandardPackageIdentity::V3 },
            OcioConfigSource::Environment,
            OcioConfigSource::Builtin { name: "aces_1.2".into() },
            OcioConfigSource::Path { path: PathBuf::from("/tmp/config.ocio") },
        ];
        for source in &sources {
            let json = serde_json::to_string(source).expect("serialize");
            let back: OcioConfigSource = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(
                &back, source,
                "round-trip failed for {source:?}: json={json}"
            );
        }
    }

    #[test]
    fn color_engine_with_ocio_source_round_trip() {
        let engine = ColorEngine::CustomOcio {
            identity: Box::new(CustomOcioProjectIdentity::from_resolved(
                OcioConfigSource::Builtin { name: "aces_1.2".into() },
                "a".repeat(64),
                "resolved-config-cache-id".to_owned(),
                "b".repeat(64),
                "ACEScg".to_owned(),
                vec![CustomOcioOutputIdentity::from_resolved(
                    ColorSpace::Srgb,
                    "sRGB".to_owned(),
                    "ACES 1.0 - SDR Video".to_owned(),
                    "Utility - sRGB - Texture".to_owned(),
                    CustomOcioLookIdentity::None,
                )],
                vec![CustomOcioRoleIdentity::new(
                    "scene_linear".to_owned(),
                    "ACEScg".to_owned(),
                )],
            )),
        };
        let json = serde_json::to_string(&engine).expect("serialize ColorEngine::CustomOcio");
        let back: ColorEngine =
            serde_json::from_str(&json).expect("deserialize ColorEngine::CustomOcio");
        assert_eq!(back, engine);
        assert!(json.contains("\"config_sha256\""));
        assert!(json.contains("\"resolved_cache_id\""));
        assert!(json.contains("\"outputs\""));
        assert!(json.contains("\"display_color_space\":\"Utility - sRGB - Texture\""));
        assert!(json.contains("\"dynamic_properties\":[]"));

        let mut incomplete_custom: serde_json::Value =
            serde_json::from_str(&json).expect("Custom OCIO JSON value");
        incomplete_custom["identity"]
            .as_object_mut()
            .expect("Custom OCIO identity object")
            .remove("resolved_cache_id");
        assert!(serde_json::from_value::<ColorEngine>(incomplete_custom).is_err());

        let mut duplicate_output: serde_json::Value =
            serde_json::from_str(&json).expect("Custom OCIO JSON value");
        let outputs = duplicate_output["identity"]["outputs"]
            .as_array_mut()
            .expect("Custom OCIO outputs");
        let mut duplicate = outputs[0].clone();
        duplicate["output_color_space"] = serde_json::json!("Rec2100Pq");
        outputs.push(duplicate);
        assert!(
            serde_json::from_value::<ColorEngine>(duplicate_output).is_err(),
            "one display/view cannot claim two standardized output labels"
        );

        let smart = ColorEngine::mondrian_standard();
        let json2 = serde_json::to_string(&smart).expect("serialize MondrianStandard");
        let back2: ColorEngine =
            serde_json::from_str(&json2).expect("deserialize MondrianStandard");
        assert_eq!(back2, smart);
        assert!(json2.contains("\"package_id\":\"mondrian_standard\""));
        assert!(json2.contains(MondrianStandardPackageIdentity::V3.config_sha256()));
        assert!(json2.contains(MondrianStandardPackageIdentity::V3.package_sha256()));
        assert!(json2.contains("\"working_space_id\":\"linear_rec2020_v1\""));
        assert!(json2.contains("\"sdr_view_transform_id\":\"mondrian_standard_sdr_v2\""));
        assert!(json2.contains("\"hdr_view_transform_id\":\"mondrian_standard_hdr_1000_nits_v1\""));

        let aces = ColorEngine::Aces { preset: AcesConfigPreset::StudioV4Aces2Ocio25 };
        let json3 = serde_json::to_string(&aces).expect("serialize ACES mode");
        let back3: ColorEngine = serde_json::from_str(&json3).expect("deserialize ACES mode");
        assert_eq!(back3, aces);
        assert_eq!(
            AcesConfigPreset::StudioV4Aces2Ocio25.ocio_source(),
            OcioConfigSource::Builtin {
                name: "studio-config-v4.0.0_aces-v2.0_ocio-v2.5".to_owned()
            }
        );
    }

    #[test]
    fn mondrian_standard_identity_rejects_legacy_or_tampered_payloads() {
        let legacy = serde_json::json!({"mode": "mondrian_standard"});
        assert!(serde_json::from_value::<ColorEngine>(legacy).is_err());

        let mut tampered =
            serde_json::to_value(ColorEngine::mondrian_standard()).expect("serialize Standard");
        tampered["package"]["package_sha256"] = serde_json::Value::String("0".repeat(64));
        assert!(serde_json::from_value::<ColorEngine>(tampered).is_err());

        let v2 = ColorEngine::MondrianStandard { package: MondrianStandardPackageIdentity::V2 };
        let v2_round_trip: ColorEngine = serde_json::from_value(
            serde_json::to_value(&v2).expect("serialize legacy Standard v2"),
        )
        .expect("deserialize supported legacy Standard v2");
        assert_eq!(v2_round_trip, v2);

        let mut mixed_identity =
            serde_json::to_value(ColorEngine::mondrian_standard()).expect("serialize Standard");
        mixed_identity["package"]["package_version"] = serde_json::json!("v2");
        assert!(serde_json::from_value::<ColorEngine>(mixed_identity).is_err());

        let mut swapped_view =
            serde_json::to_value(ColorEngine::mondrian_standard()).expect("serialize Standard");
        swapped_view["package"]["sdr_view_transform_id"] =
            serde_json::Value::String("mondrian_standard_hdr_1000_nits_v1".to_owned());
        assert!(serde_json::from_value::<ColorEngine>(swapped_view).is_err());

        let mut incomplete =
            serde_json::to_value(ColorEngine::mondrian_standard()).expect("serialize Standard");
        incomplete["package"]
            .as_object_mut()
            .expect("package object")
            .remove("hdr_view_transform_version");
        assert!(serde_json::from_value::<ColorEngine>(incomplete).is_err());
    }
}

// ── Asset source ──────────────────────────────────────────────────────

/// Describes where an asset's content originates from.
///
/// Replaces the ad-hoc `mondrian://` URI scheme previously used to
/// distinguish file-based media from generated content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetSource {
    /// A file on the local filesystem.
    File(PathBuf),
    /// A synthetically generated asset (solid color, adjustment layer, etc.).
    Generated(GeneratedAssetKind),
    /// Remote URL — placeholder for future cloud asset support.
    Remote(String),
}

/// Kind of synthetically generated asset content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub enum GeneratedAssetKind {
    SolidColor,
    AdjustmentLayer,
}
