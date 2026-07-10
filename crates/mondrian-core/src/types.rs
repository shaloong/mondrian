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
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
pub struct TimeCode {
    /// 帧编号（可为负数，用于 offset）
    pub frame: i64,
    /// 时间基 = 1/fps，例如 25fps → Rational{1, 25}
    pub time_base: Rational,
}

impl TimeCode {
    pub const ZERO: Self = Self { frame: 0, time_base: Rational::FPS_25 };

    pub fn new(frame: i64, time_base: Rational) -> Self {
        Self { frame, time_base }
    }

    /// 从秒数构造（四舍五入到最近帧）
    pub fn from_secs(secs: f64, fps: Rational) -> Self {
        let frame = (secs * fps.to_f64()).round() as i64;
        Self { frame, time_base: Rational::new(fps.den, fps.num) }
    }

    /// 转换为秒（浮点）
    pub fn to_secs(self) -> f64 {
        self.frame as f64 * self.time_base.to_f64()
    }

    /// 转换为毫秒
    pub fn to_millis(self) -> f64 {
        self.to_secs() * 1000.0
    }

    /// 格式化为 SMPTE 时间码字符串 HH:MM:SS:FF
    pub fn to_smpte(self) -> String {
        let fps = (1.0 / self.time_base.to_f64()).round() as i64;
        let total_secs = self.frame / fps;
        let ff = self.frame % fps;
        let ss = total_secs % 60;
        let mm = (total_secs / 60) % 60;
        let hh = total_secs / 3600;
        format!("{hh:02}:{mm:02}:{ss:02}:{ff:02}")
    }
}

impl std::ops::Add for TimeCode {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        // 假设相同时间基
        Self {
            frame: self.frame + rhs.frame,
            time_base: self.time_base,
        }
    }
}

impl std::ops::Sub for TimeCode {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self {
            frame: self.frame - rhs.frame,
            time_base: self.time_base,
        }
    }
}

impl fmt::Display for TimeCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_smpte())
    }
}

/// 时间范围 [start, end)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: TimeCode,
    pub end: TimeCode,
}

impl TimeRange {
    pub fn new(start: TimeCode, end: TimeCode) -> Self {
        Self { start, end }
    }

    pub fn duration(self) -> TimeCode {
        self.end - self.start
    }

    pub fn contains(self, t: TimeCode) -> bool {
        t >= self.start && t < self.end
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
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
    Rec2100Hlg,
    Rec2100Pq,
    Srgb,
    Rec2020,
    DciP3,
    AppleLog,
    SLog3,
    ArriLogC4,
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
            ColorSpace::Rec709 | ColorSpace::Srgb => Ok(Self::LinearRec709),
            ColorSpace::Rec2020 | ColorSpace::Rec2100Hlg | ColorSpace::Rec2100Pq => {
                Ok(Self::LinearRec2020)
            }
            ColorSpace::DciP3 => Ok(Self::LinearP3D65),
            ColorSpace::AppleLog | ColorSpace::SLog3 | ColorSpace::ArriLogC4 => {
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

/// 色彩引擎 —— 色彩空间转换的统一分发点。
///
/// 所有色彩转换都通过此枚举的方法进行，编译器保证穷尽 match 分发，不会出现
/// "选了变体但无实现"的静默 bug。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ColorEngine {
    /// Mondrian 默认智能模式。
    ///
    /// 这是产品化策略入口：普通用户看到简化 UI，底层必须使用 Mondrian
    /// 内置 OCIO config / processor。当前构建未提供内置 config 时应显式报错。
    #[default]
    MondrianSmart,
    /// OpenColorIO v2.5.2 配置驱动管线。
    Ocio {
        /// OCIO 配置来源。`ensure_ocio_loaded` 在首次转换前根据此来源加载配置。
        source: OcioConfigSource,
    },
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
    /// 这是 Standard / Simple 模式使用的默认来源。UI 可以隐藏 OCIO 细节，
    /// 但底层仍按 OCIO config / processor 执行。
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
    fn timecode_smpte_format() {
        let tc = TimeCode::new(75, Rational::new(1, 25)); // 3 seconds = 00:00:03:00
        assert_eq!(tc.to_smpte(), "00:00:03:00");
    }

    #[test]
    fn timecode_to_secs() {
        let tc = TimeCode::from_secs(1.5, Rational::FPS_25);
        let half_frame_secs = 1.0 / (2.0 * Rational::FPS_25.to_f64());
        assert!((tc.to_secs() - 1.5).abs() <= half_frame_secs + 1e-9);
    }

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
            WorkingColorSpace::try_from(ColorSpace::SLog3),
            Err(InvalidWorkingColorSpace { color_space: ColorSpace::SLog3 })
        ));
    }

    #[test]
    fn ocio_identity_keeps_encoded_and_working_spaces_distinct() {
        assert_ne!(
            OcioColorSpaceIdentity::Encoded(ColorSpace::Rec709),
            OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec709)
        );
    }

    #[test]
    fn time_range_contains() {
        let range = TimeRange::new(
            TimeCode::new(10, Rational::new(1, 25)),
            TimeCode::new(20, Rational::new(1, 25)),
        );
        assert!(range.contains(TimeCode::new(15, Rational::new(1, 25))));
        assert!(!range.contains(TimeCode::new(5, Rational::new(1, 25))));
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
        let engine = ColorEngine::Ocio {
            source: OcioConfigSource::Builtin { name: "aces_1.2".into() },
        };
        let json = serde_json::to_string(&engine).expect("serialize ColorEngine::Ocio");
        let back: ColorEngine = serde_json::from_str(&json).expect("deserialize ColorEngine::Ocio");
        assert_eq!(back, engine);

        let smart = ColorEngine::MondrianSmart;
        let json2 = serde_json::to_string(&smart).expect("serialize MondrianSmart");
        let back2: ColorEngine = serde_json::from_str(&json2).expect("deserialize MondrianSmart");
        assert_eq!(back2, smart);
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
