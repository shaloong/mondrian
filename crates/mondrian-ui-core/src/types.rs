//! UI 基础类型
//!
//! 与任何特定框架无关的通用 UI 原语。

use glam::Vec2;
use mondrian_core::{AssetId, ClipId, EffectId, TrackId};
use mondrian_editor_state::state::PanelKind;
use uuid::Uuid;

pub use crate::corner_radii::CornerRadii;

/// Color space of an encoded RGBA8 raster image submitted to the UI renderer.
///
/// This contract is deliberately separate from timeline working spaces: UI
/// raster images are presentation-boundary assets and must declare the color
/// space in which their bytes are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RasterImageColorSpace {
    /// IEC 61966-2-1 sRGB transfer and BT.709/sRGB primaries.
    Srgb,
    /// Display P3 primaries with the sRGB transfer function.
    DisplayP3,
}

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

/// Estimate rendered text width for a single-line string at a given font size.
///
/// CJK / wide characters (> U+2E80) are ~1.0 × font_size wide;
/// Latin, digits, and punctuation are ~0.55 × font_size wide.
/// This is an approximation — use `TextRenderer::measure_text` for exact values.
pub fn estimate_text_width(text: &str, font_size: f32) -> f32 {
    text.chars()
        .map(|ch| {
            if ch >= '\u{2E80}' {
                font_size
            } else {
                font_size * 0.55
            }
        })
        .sum()
}

/// Center `text` horizontally within `rect` at `font_size`, returning the x coordinate
/// to pass to `draw_text`. Returns `rect.x` for empty text.
pub fn center_text_x(rect: Rect, text: &str, font_size: f32) -> f32 {
    if text.is_empty() {
        return rect.x;
    }
    let tw = estimate_text_width(text, font_size);
    rect.x + (rect.width - tw).max(0.0) * 0.5
}

/// Snap a Point to the nearest pixel grid to reduce subpixel jitter during resize.
/// Use for text baseline positions where stable rendering matters.
pub fn snap_point(p: Point) -> Point {
    Point::new(p.x.round(), p.y.round())
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

    /// Return the visible overlap of two rectangles.
    pub fn intersection(&self, other: &Rect) -> Self {
        let left = self.x.max(other.x);
        let top = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        Self::from_min_max(left, top, right.max(left), bottom.max(top))
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
    Other(u16),
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DragPayload {
    Clip(ClipId),
    PanelTab(PanelKind),
    Asset(AssetId),
    AssetFolder(String),
    AssetSelection {
        assets: Vec<AssetId>,
        folders: Vec<String>,
    },
    Effect(EffectId),
    Track(TrackId),
    File(Vec<std::path::PathBuf>),
}

/// 分割方向（用于 Dock 分割器和工作区布局）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

/// 事件处理结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventResult {
    /// 事件已消费，停止冒泡
    Handled,
    /// 未消费，继续冒泡
    Ignored,
}

/// Source of a focus ownership change.
///
/// Focus ownership and visible focus indication are separate concerns:
/// pointer focus routes subsequent keyboard input to the clicked widget, but
/// only keyboard-style traversal should show focus rings by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusSource {
    /// Focus moved through keyboard traversal such as Tab / Shift+Tab.
    Keyboard,
    /// Focus moved because the user clicked or otherwise pointed at a widget.
    Pointer,
    /// Focus moved by app code, state repair, or other non-pointer/non-keyboard
    /// orchestration. This keeps ownership without implying a keyboard ring.
    Programmatic,
}

impl FocusSource {
    /// Whether widgets should show a keyboard focus affordance for this source.
    pub fn is_focus_visible(self) -> bool {
        matches!(self, Self::Keyboard)
    }
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
    /// IME composition preview (underlined text shown during composition)
    ImePreedit(String),
    /// IME committed text (final result of composition)
    ImeCommit(String),
    /// IME composition cancelled without committing text.
    ImeCancel,
    FocusGained {
        source: FocusSource,
    },
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
    /// Color sampled by the platform eyedropper. Sent by the app shell after
    /// the user clicks when `EyedropperRequest { active: true }` was set.
    EyedropperSample {
        color: mondrian_core::types::Color,
    },
    /// Eyedropper cancelled by the platform (e.g. user pressed Escape at the OS level).
    EyedropperCancel,
}

impl UiEvent {
    /// Build a keyboard-visible focus gained event.
    pub fn focus_gained_keyboard() -> Self {
        Self::FocusGained { source: FocusSource::Keyboard }
    }

    /// Build a pointer-originated focus gained event.
    pub fn focus_gained_pointer() -> Self {
        Self::FocusGained { source: FocusSource::Pointer }
    }

    /// Build a programmatic focus gained event.
    pub fn focus_gained_programmatic() -> Self {
        Self::FocusGained { source: FocusSource::Programmatic }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ═══════════════════════════════════════════════════════════════════════
    // WidgetId
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn widget_id_is_unique() {
        let a = WidgetId::new();
        let b = WidgetId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn widget_id_display_is_uuid_string() {
        let id = WidgetId::new();
        let display = id.to_string();
        assert_eq!(display.len(), 36); // standard UUID format
        assert!(display.contains('-'));
    }

    #[test]
    fn widget_id_default_is_not_zero() {
        let id = WidgetId::default();
        assert_ne!(id.0, Uuid::nil());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Size
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn size_zero_is_zero() {
        assert!(Size::ZERO.is_zero());
    }

    #[test]
    fn size_non_zero_is_not_zero() {
        assert!(!Size::new(10.0, 10.0).is_zero());
    }

    #[test]
    fn size_zero_width_is_zero() {
        assert!(Size::new(0.0, 10.0).is_zero());
    }

    #[test]
    fn size_zero_height_is_zero() {
        assert!(Size::new(10.0, 0.0).is_zero());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Point
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn point_to_vec2() {
        let p = Point::new(3.0, 4.0);
        let v = p.to_vec2();
        assert_eq!(v.x, 3.0);
        assert_eq!(v.y, 4.0);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Rect
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn rect_contains_center() {
        let r = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert!(r.contains(Point::new(50.0, 50.0)));
    }

    #[test]
    fn rect_intersection_returns_overlap() {
        let a = Rect::new(10.0, 20.0, 100.0, 80.0);
        let b = Rect::new(60.0, 10.0, 120.0, 50.0);

        assert_eq!(a.intersection(&b), Rect::new(60.0, 20.0, 50.0, 40.0));
    }

    #[test]
    fn rect_intersection_returns_zero_sized_rect_when_disjoint() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(20.0, 30.0, 5.0, 5.0);

        assert_eq!(a.intersection(&b), Rect::new(20.0, 30.0, 0.0, 0.0));
    }

    #[test]
    fn rect_contains_corners() {
        let r = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert!(r.contains(Point::new(0.0, 0.0)));
        assert!(r.contains(Point::new(100.0, 100.0)));
    }

    #[test]
    fn rect_does_not_contain_outside() {
        let r = Rect::new(10.0, 10.0, 100.0, 100.0);
        assert!(!r.contains(Point::new(5.0, 5.0)));
        assert!(!r.contains(Point::new(200.0, 200.0)));
    }

    #[test]
    fn rect_intersects_overlapping() {
        let a = Rect::new(0.0, 0.0, 100.0, 100.0);
        let b = Rect::new(50.0, 50.0, 100.0, 100.0);
        assert!(a.intersects(&b));
        assert!(b.intersects(&a));
    }

    #[test]
    fn rect_does_not_intersect_separated() {
        let a = Rect::new(0.0, 0.0, 100.0, 100.0);
        let b = Rect::new(200.0, 200.0, 100.0, 100.0);
        assert!(!a.intersects(&b));
    }

    #[test]
    fn rect_intersects_edge_touching() {
        // Touching edges: A's right edge at 100, B's left edge at 100
        let a = Rect::new(0.0, 0.0, 100.0, 100.0);
        let b = Rect::new(100.0, 0.0, 100.0, 100.0);
        // intersects check: a.x < b.x+b.w (0 < 200) && a.x+a.w > b.x (100 > 100 = false)
        assert!(
            !a.intersects(&b),
            "Edge-touching rects should not intersect"
        );
    }

    #[test]
    fn rect_min_max() {
        let r = Rect::new(10.0, 20.0, 30.0, 40.0);
        assert_eq!(r.min(), Point::new(10.0, 20.0));
        assert_eq!(r.max(), Point::new(40.0, 60.0));
    }

    #[test]
    fn rect_center() {
        let r = Rect::new(0.0, 0.0, 100.0, 50.0);
        assert_eq!(r.center(), Point::new(50.0, 25.0));
    }

    #[test]
    fn rect_inset() {
        let r = Rect::new(0.0, 0.0, 100.0, 100.0);
        let inner = r.inset(10.0, 5.0);
        assert_eq!(inner, Rect::new(10.0, 5.0, 80.0, 90.0));
    }

    #[test]
    fn rect_inset_clamps_to_zero() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        let inner = r.inset(100.0, 100.0);
        assert_eq!(inner.width, 0.0);
        assert_eq!(inner.height, 0.0);
    }

    #[test]
    fn rect_from_min_max() {
        let r = Rect::from_min_max(10.0, 20.0, 110.0, 120.0);
        assert_eq!(r, Rect::new(10.0, 20.0, 100.0, 100.0));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // LayoutConstraint
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn constraint_tight_fixes_size() {
        let c = LayoutConstraint::tight(50.0, 30.0);
        let s = c.constrain(Size::new(100.0, 100.0));
        assert_eq!(s, Size::new(50.0, 30.0));
    }

    #[test]
    fn constraint_loose_allows_growth() {
        let c = LayoutConstraint::loose(10.0, 10.0);
        let s = c.constrain(Size::new(5.0, 200.0));
        assert_eq!(s, Size::new(10.0, 200.0)); // clamped to min width
    }

    #[test]
    fn constraint_loose_preserves_larger() {
        let c = LayoutConstraint::loose(10.0, 10.0);
        let s = c.constrain(Size::new(50.0, 30.0));
        assert_eq!(s, Size::new(50.0, 30.0));
    }

    #[test]
    fn constraint_loose_constrains_width_to_max() {
        let s = LayoutConstraint::LOOSE.constrain(Size::new(f32::MAX, 0.0));
        assert_eq!(s.width, f32::MAX);
        assert_eq!(s.height, 0.0);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Modifiers
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn modifiers_none_is_clear() {
        let m = Modifiers::none();
        assert!(!m.ctrl);
        assert!(!m.alt);
        assert!(!m.shift);
        assert!(!m.meta);
    }

    #[test]
    fn modifiers_ctrl_sets_only_ctrl() {
        let m = Modifiers::ctrl();
        assert!(m.ctrl);
        assert!(!m.alt);
        assert!(!m.shift);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // UiEvent
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn event_mouse_down_has_position() {
        let evt = UiEvent::MouseDown {
            position: Point::new(10.0, 20.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        };
        match evt {
            UiEvent::MouseDown { position, .. } => {
                assert_eq!(position, Point::new(10.0, 20.0));
            }
            _ => panic!("expected MouseDown"),
        }
    }

    #[test]
    fn event_handled_and_ignored_are_distinct() {
        assert_ne!(EventResult::Handled, EventResult::Ignored);
    }
}

#[cfg(test)]
mod utility_tests {
    use super::*;

    // ── estimate_text_width ──────────────────────────────────────────────

    #[test]
    fn estimate_pure_ascii() {
        let w = estimate_text_width("Hello", 10.0);
        // 5 chars × 0.55 × 10.0 = 27.5
        assert!((w - 27.5).abs() < 0.01);
    }

    #[test]
    fn estimate_pure_cjk() {
        let w = estimate_text_width("你好世界", 14.0);
        // 4 chars × 14.0 = 56.0
        assert!((w - 56.0).abs() < 0.01);
    }

    #[test]
    fn estimate_mixed_cjk_ascii() {
        let w = estimate_text_width("时间线", 13.0);
        // 3 CJK chars × 13.0 = 39.0
        assert!((w - 39.0).abs() < 0.01);
    }

    #[test]
    fn estimate_empty_string() {
        assert_eq!(estimate_text_width("", 16.0), 0.0);
    }

    #[test]
    fn estimate_boundary_char() {
        // U+2E80 is CJK Radicals Supplement start — treated as wide
        let latin_w = estimate_text_width("A", 10.0); // 0x41 < 0x2E80
        assert!((latin_w - 5.5).abs() < 0.01);
        let cjk_w = estimate_text_width("\u{2E80}", 10.0);
        assert!((cjk_w - 10.0).abs() < 0.01);
        // Common CJK character
        let han_w = estimate_text_width("\u{4E00}", 10.0); // 一
        assert!((han_w - 10.0).abs() < 0.01);
    }

    // ── center_text_x ───────────────────────────────────────────────────

    #[test]
    fn center_empty_returns_rect_x() {
        let r = Rect::new(10.0, 0.0, 100.0, 20.0);
        assert_eq!(center_text_x(r, "", 13.0), 10.0);
    }

    #[test]
    fn center_ascii_in_rect() {
        let r = Rect::new(0.0, 0.0, 100.0, 20.0);
        // "Hi" at 10px → width = 2 × 5.5 = 11.0
        // center_x = 0 + (100 - 11) / 2 = 44.5
        let cx = center_text_x(r, "Hi", 10.0);
        assert!((cx - 44.5).abs() < 0.01);
    }

    #[test]
    fn center_wider_than_rect_clamps_to_zero() {
        let r = Rect::new(5.0, 0.0, 10.0, 20.0);
        // Very long text → estimated width > rect width → (10 - tw).max(0) = 0 → cx = 5.0
        let cx = center_text_x(r, "VeryLongText", 10.0);
        assert!((cx - 5.0).abs() < 0.01);
    }

    // ── snap_point ──────────────────────────────────────────────────────

    #[test]
    fn snap_integers_unchanged() {
        let p = snap_point(Point::new(10.0, 20.0));
        assert_eq!(p, Point::new(10.0, 20.0));
    }

    #[test]
    fn snap_halves_round_up() {
        let p = snap_point(Point::new(10.5, 20.5));
        assert_eq!(p, Point::new(11.0, 21.0));
    }

    #[test]
    fn snap_small_fractions() {
        let p = snap_point(Point::new(10.3, 20.7));
        assert_eq!(p, Point::new(10.0, 21.0));
    }
}
