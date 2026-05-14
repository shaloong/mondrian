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
    Add,
    Subtract,
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

/// 色彩引擎 —— 色彩空间转换的统一分发点。
///
/// 所有色彩转换都通过此枚举的方法进行，编译器保证穷尽 match 分发，不会出现
/// "选了变体但无实现"的静默 bug。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ColorEngine {
    /// Mondrian 内置数学管线：标准 OETF/EOTF 曲线 + 色域矩阵 + ACES tone map。
    #[default]
    MondrianSmart,
    /// OpenColorIO v2.5.1 配置驱动管线。
    Ocio {
        /// OCIO 配置来源。`ensure_ocio_loaded` 在首次转换前根据此来源加载配置。
        source: OcioConfigSource,
    },
}

/// 如何定位 OCIO 配置。
///
/// 类似达芬奇的色彩科学选择器（预设）和 Nuke 的 OCIO 解析顺序
///（`$OCIO` → 内置 → 自定义路径）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OcioConfigSource {
    /// 使用 `OCIO` 环境变量（行业标准）。
    /// 未设置时自动回退到系统标准路径。
    #[default]
    #[serde(rename = "environment")]
    Environment,
    /// 使用内置 OCIO 配置（如 ACES 1.2、CG Config）。
    /// 名称可通过 [`crate::ocio::builtin_config_names`] 获取。
    #[serde(rename = "builtin")]
    Builtin(String),
    /// 显式指定 `config.ocio` 文件路径。
    #[serde(rename = "path")]
    Path(PathBuf),
}

impl fmt::Display for OcioConfigSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Environment => write!(f, "$OCIO"),
            Self::Builtin(name) => write!(f, "内置: {name}"),
            Self::Path(p) => write!(f, "{}", p.display()),
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
    fn time_range_contains() {
        let range = TimeRange::new(
            TimeCode::new(10, Rational::new(1, 25)),
            TimeCode::new(20, Rational::new(1, 25)),
        );
        assert!(range.contains(TimeCode::new(15, Rational::new(1, 25))));
        assert!(!range.contains(TimeCode::new(5, Rational::new(1, 25))));
    }
}
