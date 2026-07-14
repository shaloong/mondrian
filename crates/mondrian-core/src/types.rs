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
define_id!(AudioContributionId, "音频贡献 ID");
define_id!(AudioSourceComponentId, "音频源组件 ID");
define_id!(AudioTransitionId, "音频转场 ID");
define_id!(AudioRouteId, "音频路由 ID");
define_id!(AudioProcessorInstanceId, "音频处理器实例 ID");
define_id!(MixBusId, "混音总线 ID");
define_id!(ProgramOutputId, "节目输出 ID");
define_id!(AudioRoleId, "音频角色 ID");
define_id!(JobId, "渲染任务 ID");
define_id!(MaskId, "蒙版 ID");

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
    /// Apple Log with BT.2020 primaries.
    AppleLogBt2020,
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
            ColorSpace::Rec709
            | ColorSpace::Rec601Pal
            | ColorSpace::Rec601Ntsc
            | ColorSpace::Srgb => Ok(Self::LinearRec709),
            ColorSpace::Rec2020 | ColorSpace::Rec2100Hlg | ColorSpace::Rec2100Pq => {
                Ok(Self::LinearRec2020)
            }
            ColorSpace::DisplayP3 => Ok(Self::LinearP3D65),
            ColorSpace::AppleLogBt2020
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
/// Encoded identities resolve source/delivery color spaces. Working identities
/// resolve linear scene/render spaces. Processor caches include this enum so
/// equal primaries with different transfer semantics cannot alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "role", content = "space", rename_all = "snake_case")]
pub enum OcioColorSpaceIdentity {
    /// Encoded source, display, or delivery color space.
    Encoded(ColorSpace),
    /// Linear-light rendering color space.
    Working(WorkingColorSpace),
}

impl OcioColorSpaceIdentity {
    /// Return the encoded source/delivery space when this identity is encoded.
    pub const fn encoded(self) -> Option<ColorSpace> {
        match self {
            Self::Encoded(space) => Some(space),
            Self::Working(_) => None,
        }
    }

    /// Return the linear rendering space when this identity is a working space.
    pub const fn working(self) -> Option<WorkingColorSpace> {
        match self {
            Self::Encoded(_) => None,
            Self::Working(space) => Some(space),
        }
    }
}

impl From<ColorSpace> for OcioColorSpaceIdentity {
    fn from(value: ColorSpace) -> Self {
        Self::Encoded(value)
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
    /// Mondrian Standard v1's embedded OCIO configuration.
    #[serde(rename = "mondrian_default_ocio_v1")]
    V1,
}

/// Exact SHA-256 identity of the OCIO config text in a Standard package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MondrianStandardConfigDigest {
    /// Digest of `mondrian_default_ocio_v1.ocio`.
    #[serde(rename = "3d2612a216abab75491a7b45db82f0d9e14aee6a51aaf2e35be0216e9e28569f")]
    V1,
}

/// Exact SHA-256 identity of a complete Standard config-and-resource package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MondrianStandardPackageDigest {
    /// Digest of Standard v1's config plus every embedded resource.
    #[serde(rename = "11e381b7e91e3ed0d3a17155829df9fed7644bfe45b34df6bbb3c2205c342e8b")]
    V1,
}

/// Stable versioned identity of the Standard compositing working space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MondrianStandardWorkingSpaceId {
    /// Scene-linear, D65, Rec.2020 primaries, unbounded float RGB.
    #[serde(rename = "linear_rec2020_v1")]
    LinearRec2020V1,
}

/// Stable versioned identity of Standard v1's default SDR rendering transform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MondrianStandardViewTransformId {
    /// Mondrian Standard SDR scene-to-display rendering transform v1.
    #[serde(rename = "mondrian_standard_sdr_v1")]
    SdrV1,
}

/// Fully pinned identity persisted for a Mondrian Standard project mode.
///
/// Every field is intentionally required. The single-variant identity types
/// make mismatched or partially edited alpha project files fail to deserialize
/// instead of silently resolving to the current bundled package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MondrianStandardPackageIdentity {
    package_id: MondrianStandardPackageId,
    package_version: MondrianStandardVersion,
    config_id: MondrianStandardConfigId,
    config_sha256: MondrianStandardConfigDigest,
    package_sha256: MondrianStandardPackageDigest,
    working_space_id: MondrianStandardWorkingSpaceId,
    working_space_version: MondrianStandardVersion,
    default_view_transform_id: MondrianStandardViewTransformId,
    default_view_transform_version: MondrianStandardVersion,
}

impl MondrianStandardPackageIdentity {
    /// Exact identity of the only package supported by this alpha schema.
    pub const V1: Self = Self {
        package_id: MondrianStandardPackageId::MondrianStandard,
        package_version: MondrianStandardVersion::V1,
        config_id: MondrianStandardConfigId::V1,
        config_sha256: MondrianStandardConfigDigest::V1,
        package_sha256: MondrianStandardPackageDigest::V1,
        working_space_id: MondrianStandardWorkingSpaceId::LinearRec2020V1,
        working_space_version: MondrianStandardVersion::V1,
        default_view_transform_id: MondrianStandardViewTransformId::SdrV1,
        default_view_transform_version: MondrianStandardVersion::V1,
    };

    /// Stable package identifier used by diagnostics and fingerprints.
    pub const fn package_id(self) -> &'static str {
        "mondrian_standard"
    }

    /// Stable embedded OCIO config identifier.
    pub const fn config_id(self) -> &'static str {
        "mondrian_default_ocio_v1"
    }

    /// Exact SHA-256 digest of the embedded OCIO config text.
    pub const fn config_sha256(self) -> &'static str {
        "3d2612a216abab75491a7b45db82f0d9e14aee6a51aaf2e35be0216e9e28569f"
    }

    /// Exact SHA-256 digest of the config and all embedded resources.
    pub const fn package_sha256(self) -> &'static str {
        "11e381b7e91e3ed0d3a17155829df9fed7644bfe45b34df6bbb3c2205c342e8b"
    }

    /// Versioned working-space identity pinned by this package.
    pub const fn working_space_id(self) -> &'static str {
        "linear_rec2020_v1"
    }

    /// Versioned default view-transform identity pinned by this package.
    pub const fn default_view_transform_id(self) -> &'static str {
        "mondrian_standard_sdr_v1"
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
        /// Explicit config source resolved before the first processor request.
        source: OcioConfigSource,
    },
}

impl ColorEngine {
    /// Select the exact Mondrian Standard package supported by this schema.
    pub const fn mondrian_standard() -> Self {
        Self::MondrianStandard { package: MondrianStandardPackageIdentity::V1 }
    }

    /// Return the pinned Standard package identity when this is Standard mode.
    pub const fn mondrian_standard_package(&self) -> Option<MondrianStandardPackageIdentity> {
        match self {
            Self::MondrianStandard { package } => Some(*package),
            Self::Aces { .. } | Self::CustomOcio { .. } => None,
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
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OcioConfigSource {
    /// Mondrian 内置默认 OCIO config。
    ///
    /// 这是 Standard / Simple 模式使用的固定 OCIO package 来源。UI 可以隐藏
    /// OCIO 细节，但外部 config 不能覆盖该版本的定义。
    #[serde(rename = "mondrian_default")]
    MondrianDefault,
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
            Self::MondrianDefault => write!(f, "Mondrian Default OCIO"),
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
    fn working_color_space_conversion_separates_gamut_from_transfer() {
        assert_eq!(
            WorkingColorSpace::try_from(ColorSpace::Rec2100Pq),
            Ok(WorkingColorSpace::LinearRec2020)
        );
        assert_eq!(
            WorkingColorSpace::try_from(ColorSpace::Srgb),
            Ok(WorkingColorSpace::LinearRec709)
        );
        assert!(matches!(
            WorkingColorSpace::try_from(ColorSpace::SonySLog3SGamut3Cine),
            Err(InvalidWorkingColorSpace { color_space: ColorSpace::SonySLog3SGamut3Cine })
        ));
    }

    #[test]
    fn ocio_identity_keeps_encoded_and_working_spaces_distinct() {
        assert_ne!(
            OcioColorSpaceIdentity::Encoded(ColorSpace::Rec709),
            OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec709)
        );
    }

    // ── OCIO config source / color engine serde ────────────────────────────

    #[test]
    fn ocio_config_source_round_trip() {
        let sources = vec![
            OcioConfigSource::MondrianDefault,
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
            source: OcioConfigSource::Builtin { name: "aces_1.2".into() },
        };
        let json = serde_json::to_string(&engine).expect("serialize ColorEngine::CustomOcio");
        let back: ColorEngine =
            serde_json::from_str(&json).expect("deserialize ColorEngine::CustomOcio");
        assert_eq!(back, engine);

        let smart = ColorEngine::mondrian_standard();
        let json2 = serde_json::to_string(&smart).expect("serialize MondrianStandard");
        let back2: ColorEngine =
            serde_json::from_str(&json2).expect("deserialize MondrianStandard");
        assert_eq!(back2, smart);
        assert!(json2.contains("\"package_id\":\"mondrian_standard\""));
        assert!(json2.contains(MondrianStandardPackageIdentity::V1.config_sha256()));
        assert!(json2.contains(MondrianStandardPackageIdentity::V1.package_sha256()));
        assert!(json2.contains("\"working_space_id\":\"linear_rec2020_v1\""));
        assert!(json2.contains("\"default_view_transform_id\":\"mondrian_standard_sdr_v1\""));

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

        let mut incomplete =
            serde_json::to_value(ColorEngine::mondrian_standard()).expect("serialize Standard");
        incomplete["package"]
            .as_object_mut()
            .expect("package object")
            .remove("working_space_version");
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
