//! UI 基础类型
//!
//! 与任何特定框架无关的通用 UI 原语。

use glam::Vec2;
use mondrian_core::{AssetId, ClipId, EffectId, TrackId};
use uuid::Uuid;

// ═══════════════════════════════════════════════════════════════════════════════════
// Widget ID（手动实现，不依赖 define_id! 宏）
// ═══════════════════════════════════════════════════════════════════════════════════

/// Widget 唯一标识
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WidgetId(pub Uuid);

impl WidgetId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for WidgetId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for WidgetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ═══════════════════════════════════════════════════════════════════════════════════
// 几何
// ═══════════════════════════════════════════════════════════════════════════════════

/// 2D 尺寸
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

impl Size {
    pub const ZERO: Self = Self { width: 0.0, height: 0.0 };

    pub fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }

    pub fn is_zero(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }
}

/// 2D 点
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn to_vec2(self) -> Vec2 {
        Vec2::new(self.x, self.y)
    }
}

/// 矩形区域（屏幕坐标，左上角原点）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0, width: 0.0, height: 0.0 };

    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self { x, y, width, height }
    }

    pub fn from_min_max(min_x: f32, min_y: f32, max_x: f32, max_y: f32) -> Self {
        Self {
            x: min_x,
            y: min_y,
            width: max_x - min_x,
            height: max_y - min_y,
        }
    }

    pub fn min(&self) -> Point {
        Point::new(self.x, self.y)
    }

    pub fn max(&self) -> Point {
        Point::new(self.x + self.width, self.y + self.height)
    }

    pub fn center(&self) -> Point {
        Point::new(self.x + self.width * 0.5, self.y + self.height * 0.5)
    }

    pub fn contains(&self, point: Point) -> bool {
        point.x >= self.x
            && point.x <= self.x + self.width
            && point.y >= self.y
            && point.y <= self.y + self.height
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.width
            && self.x + self.width > other.x
            && self.y < other.y + other.height
            && self.y + self.height > other.y
    }

    pub fn inset(&self, dx: f32, dy: f32) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
            width: (self.width - 2.0 * dx).max(0.0),
            height: (self.height - 2.0 * dy).max(0.0),
        }
    }
}

/// 布局约束
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutConstraint {
    pub min: Size,
    pub max: Size,
}

impl LayoutConstraint {
    /// 无约束
    pub const LOOSE: Self = Self {
        min: Size { width: 0.0, height: 0.0 },
        max: Size { width: f32::MAX, height: f32::MAX },
    };

    /// 严格固定尺寸
    pub fn tight(width: f32, height: f32) -> Self {
        Self {
            min: Size { width, height },
            max: Size { width, height },
        }
    }

    /// 最小尺寸，无上限
    pub fn loose(min_width: f32, min_height: f32) -> Self {
        Self {
            min: Size { width: min_width, height: min_height },
            max: Size { width: f32::MAX, height: f32::MAX },
        }
    }

    /// 将约束夹紧到给定尺寸
    pub fn constrain(&self, size: Size) -> Size {
        Size {
            width: size.width.clamp(self.min.width, self.max.width),
            height: size.height.clamp(self.min.height, self.max.height),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════════
// 输入事件
// ═══════════════════════════════════════════════════════════════════════════════════

/// 鼠标按钮
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// 修饰键
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

impl Modifiers {
    pub fn ctrl() -> Self {
        Self { ctrl: true, ..Default::default() }
    }

    pub fn shift() -> Self {
        Self { shift: true, ..Default::default() }
    }

    pub fn none() -> Self {
        Self::default()
    }
}

/// 键盘按键
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    // 字母
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    // 数字
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    // 功能键
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    // 导航
    Escape,
    Tab,
    Enter,
    Space,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Left,
    Right,
    Up,
    Down,
    // 修饰键
    LeftShift,
    RightShift,
    LeftCtrl,
    RightCtrl,
    LeftAlt,
    RightAlt,
    LeftMeta,
    RightMeta,
}

/// 拖拽载荷 —— 跨 Widget 的拖拽数据
#[derive(Debug, Clone)]
pub enum DragPayload {
    Clip(ClipId),
    Asset(AssetId),
    Effect(EffectId),
    Track(TrackId),
    File(Vec<std::path::PathBuf>),
}

/// 事件处理结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventResult {
    /// 事件已消费，停止冒泡
    Handled,
    /// 未消费，继续冒泡
    Ignored,
}

/// 用户输入事件
#[derive(Debug, Clone)]
pub enum UiEvent {
    MouseDown {
        position: Point,
        button: MouseButton,
        modifiers: Modifiers,
    },
    MouseUp {
        position: Point,
        button: MouseButton,
        modifiers: Modifiers,
    },
    MouseMove {
        position: Point,
        modifiers: Modifiers,
    },
    MouseWheel {
        delta: f32,
        position: Point,
        modifiers: Modifiers,
    },
    KeyDown {
        key: KeyCode,
        modifiers: Modifiers,
    },
    KeyUp {
        key: KeyCode,
        modifiers: Modifiers,
    },
    TextInput(String),
    FocusGained,
    FocusLost,
    DragEnter {
        payload: DragPayload,
        position: Point,
    },
    DragOver {
        position: Point,
    },
    DragLeave,
    Drop {
        payload: DragPayload,
        position: Point,
    },
}
