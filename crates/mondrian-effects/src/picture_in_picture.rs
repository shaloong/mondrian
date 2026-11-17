//! Picture in picture (TRF-04): resamples the frame into one authored window
//! and paints that window, its border and its drop shadow over the original.
//!
//! Geometry. Pixels are measured at `(x + 0.5, y + 0.5)`. The window is
//! `wpx = width * W`, `hpx = height * H` pixels wide (both at least one pixel);
//! with `lock_aspect` the height is derived from the width instead:
//!
//! ```text
//! hpx = wpx * (W / H) / clamp(aspect_ratio, 0.05, 20)
//! ```
//!
//! so `aspect_ratio` is the window's own pixel aspect, where `1` is square.
//! The nine-grid `anchor` places the window centre, `rotation` turns it, and
//! every further test happens in the window's local frame
//!
//! ```text
//! q = R(-theta) * (p - c)
//! ```
//!
//! `u = q.x / (wpx / 2)` and `v = q.y / (hpx / 2)` put the window inside the
//! unit square. `style` picks the shape: a rounded rectangle whose signed
//! distance `qd = |q| - (half - r)`, `d = length(max(qd, 0)) + min(max(qd.x,
//! qd.y), 0) - r` is negative inside, or an ellipse whose normalised radial
//! distance `(length(u, v) - 1) * min(half)` is negative inside as well. Both
//! distances therefore share one coverage formula that feathers the edge
//! outward, and neither distance drives the source mapping: they only shape
//! coverage, so the ellipse approximation is confined to the one-pixel
//! anti-aliasing band.
//!
//! The source is fitted into the window by `content`: `stretch` maps the whole
//! frame into the window, `contain` keeps the frame's aspect and leaves the
//! remainder transparent (out-of-frame samples are exactly `(0, 0, 0, 0)`), and
//! `cover` keeps the aspect and crops the overflow. `scale` zooms the content
//! about its centre and `pan` shifts it by a fraction of the window size. The
//! sample point is
//!
//! ```text
//! src = c_source + q * fit / max(scale, 1e-3) + (pan_x * wpx / fit.x, ...)
//! ```
//!
//! and the frame is read bilinearly with clamped coordinates, exactly like
//! [`crate::resample::sample_bilinear_clamped`].
//!
//! Painting. The frame is premultiplied once, then every output pixel is
//! composed in premultiplied "source over" order: the shadow, the fill, the
//! resampled content, and finally the border. The fill only shows where the
//! content does not cover the window (`1 - content alpha`) and can reuse the
//! border colour (`fill_matching`). The border is the anti-aliased band between
//! the shape's edge and the same shape inset by
//! `|border_width| * min(wpx, hpx) / 2`; the authored sign is inert, so a
//! negative width paints the same inward band. The shadow is the same shape,
//! zoomed by `shadow_zoom` about the window centre, pushed along
//! `shadow_angle` by `shadow_distance * min(W, H)` pixels and softened by
//! `shadow_blur * 0.5 * min(W, H)` pixels. Neither the fill, the border nor the
//! shadow inherits the window opacity: they author their own.
//!
//! A pixel that the window and the shadow both miss keeps its input value
//! exactly, and a pixel that ends without coverage is canonicalised to
//! `(0, 0, 0, 0)`. RGB is never clamped. The pass needs no halo because every
//! read is clamped to the frame it already has (design §7).
//!
//! GPU parity. [`PictureInPictureGrade::gpu_controls`] packs the resolved
//! geometry into the shared five-cell spatial uniform plus one dedicated
//! five-cell block, so the passed-in frame size is applied on the CPU exactly
//! as the shader sees it (design §5).

use std::hash::{Hash, Hasher};

use thiserror::Error;

use crate::coverage::straight_rgba_from_premultiplied;
use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Largest corner radius or border thickness, in pixels, that the geometry
/// resolves before it clamps (design §4).
pub const PIP_MAX_THICKNESS: f32 = 4_000.0;

/// Documented content fit.
pub const PIP_DEFAULT_CONTENT: PictureInPictureContent = PictureInPictureContent::Stretch;
/// Documented window scale.
pub const PIP_DEFAULT_SCALE: f32 = 1.0;
/// Documented horizontal pan.
pub const PIP_DEFAULT_PAN_X: f32 = 0.0;
/// Documented vertical pan.
pub const PIP_DEFAULT_PAN_Y: f32 = 0.0;
/// Documented edge softness, in pixels.
pub const PIP_DEFAULT_ANTI_ALIAS: f32 = 1.0;
/// Documented nine-grid position.
pub const PIP_DEFAULT_ANCHOR: PictureInPictureAnchor = PictureInPictureAnchor::TopLeft;
/// Documented aspect lock.
pub const PIP_DEFAULT_LOCK_ASPECT: bool = false;
/// Documented window width, as a fraction of the frame width.
pub const PIP_DEFAULT_WIDTH: f32 = 0.25;
/// Documented window height, as a fraction of the frame height.
pub const PIP_DEFAULT_HEIGHT: f32 = 0.3;
/// Documented window pixel aspect.
pub const PIP_DEFAULT_ASPECT_RATIO: f32 = 1.0;
/// Documented window style.
pub const PIP_DEFAULT_STYLE: PictureInPictureStyle = PictureInPictureStyle::RoundedRect;
/// Documented corner rounding.
pub const PIP_DEFAULT_ROUNDING: f32 = 0.35;
/// Documented window rotation, in degrees.
pub const PIP_DEFAULT_ROTATION: f32 = 0.0;
/// Documented window opacity.
pub const PIP_DEFAULT_OPACITY: f32 = 1.0;
/// Documented border switch.
pub const PIP_DEFAULT_BORDER: f32 = 0.0;
/// Documented border width.
pub const PIP_DEFAULT_BORDER_WIDTH: f32 = -0.015;
/// Documented border red.
pub const PIP_DEFAULT_BORDER_RED: f32 = 1.0;
/// Documented border green.
pub const PIP_DEFAULT_BORDER_GREEN: f32 = 1.0;
/// Documented border blue.
pub const PIP_DEFAULT_BORDER_BLUE: f32 = 1.0;
/// Documented border opacity.
pub const PIP_DEFAULT_BORDER_OPACITY: f32 = 1.0;
/// Documented "fill matches the border" switch.
pub const PIP_DEFAULT_FILL_MATCHING: f32 = 1.0;
/// Documented fill switch.
pub const PIP_DEFAULT_FILL: f32 = 0.0;
/// Documented fill red.
pub const PIP_DEFAULT_FILL_RED: f32 = 0.5;
/// Documented fill green.
pub const PIP_DEFAULT_FILL_GREEN: f32 = 0.5;
/// Documented fill blue.
pub const PIP_DEFAULT_FILL_BLUE: f32 = 0.5;
/// Documented fill opacity.
pub const PIP_DEFAULT_FILL_OPACITY: f32 = 1.0;
/// Documented drop shadow switch.
pub const PIP_DEFAULT_SHADOW: f32 = 1.0;
/// Documented drop shadow strength.
pub const PIP_DEFAULT_SHADOW_STRENGTH: f32 = 0.65;
/// Documented drop shadow red.
pub const PIP_DEFAULT_SHADOW_RED: f32 = 0.0;
/// Documented drop shadow green.
pub const PIP_DEFAULT_SHADOW_GREEN: f32 = 0.0;
/// Documented drop shadow blue.
pub const PIP_DEFAULT_SHADOW_BLUE: f32 = 0.0;
/// Documented drop shadow angle, in degrees.
pub const PIP_DEFAULT_SHADOW_ANGLE: f32 = 45.0;
/// Documented drop shadow distance.
pub const PIP_DEFAULT_SHADOW_DISTANCE: f32 = 0.02;
/// Documented drop shadow zoom.
pub const PIP_DEFAULT_SHADOW_ZOOM: f32 = 1.0;
/// Documented drop shadow blur.
pub const PIP_DEFAULT_SHADOW_BLUR: f32 = 0.45;

/// Smallest accepted window scale.
pub const PIP_MIN_SCALE: f32 = 0.01;
/// Largest accepted window scale.
pub const PIP_MAX_SCALE: f32 = 20.0;
/// Largest accepted edge softness, in pixels.
pub const PIP_MAX_ANTI_ALIAS: f32 = 16.0;
/// Smallest accepted window width or height.
pub const PIP_MIN_WINDOW: f32 = 0.01;
/// Smallest accepted window pixel aspect.
pub const PIP_MIN_ASPECT_RATIO: f32 = 0.05;
/// Largest accepted window pixel aspect.
pub const PIP_MAX_ASPECT_RATIO: f32 = 20.0;
/// Largest accepted rotation or shadow angle, in degrees.
pub const PIP_MAX_ANGLE: f32 = 180.0;
/// Largest accepted border width magnitude.
pub const PIP_MAX_BORDER_WIDTH: f32 = 0.5;
/// Largest accepted shadow distance magnitude.
pub const PIP_MAX_SHADOW_DISTANCE: f32 = 0.5;
/// Largest accepted shadow zoom.
pub const PIP_MAX_SHADOW_ZOOM: f32 = 5.0;
/// Smallest accepted content scale; guards the sample-point division.
const PIP_MIN_CONTENT_SCALE: f32 = 1.0e-3;

/// How the source frame is fitted into the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PictureInPictureContent {
    /// Map the whole frame into the window, ignoring the frame's aspect.
    Stretch = 0,
    /// Keep the frame's aspect and leave the remainder transparent.
    Contain = 1,
    /// Keep the frame's aspect and crop the overflow.
    Cover = 2,
}

impl PictureInPictureContent {
    /// Every mode in display order.
    pub const ALL: [Self; 3] = [Self::Stretch, Self::Contain, Self::Cover];

    /// Documented default.
    pub const DEFAULT: Self = Self::Stretch;

    /// Packed tag used by the graph, the schema and the spatial pass.
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// Inverse of [`Self::code`]; out-of-range codes are authoring errors.
    pub fn from_code(code: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|content| content.code() == code)
    }

    /// Stable persisted key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Stretch => "stretch",
            Self::Contain => "contain",
            Self::Cover => "cover",
        }
    }

    /// Inverse of [`Self::as_key`].
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|content| content.as_key() == key)
    }

    /// Chinese caption.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Stretch => "拉伸",
            Self::Contain => "保持比例适合",
            Self::Cover => "保持比例裁剪",
        }
    }
}

/// The window's outline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PictureInPictureStyle {
    /// Rounded rectangle; `rounding` picks the corner radius.
    RoundedRect = 1,
    /// Ellipse; `rounding` is unused.
    Ellipse = 2,
}

impl PictureInPictureStyle {
    /// Every style in display order.
    pub const ALL: [Self; 2] = [Self::RoundedRect, Self::Ellipse];

    /// Documented default.
    pub const DEFAULT: Self = Self::RoundedRect;

    /// Packed tag used by the graph, the schema and the spatial pass.
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// Inverse of [`Self::code`]; out-of-range codes are authoring errors.
    pub fn from_code(code: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|style| style.code() == code)
    }

    /// Stable persisted key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::RoundedRect => "rounded_rect",
            Self::Ellipse => "ellipse",
        }
    }

    /// Inverse of [`Self::as_key`].
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|style| style.as_key() == key)
    }

    /// Chinese caption.
    pub const fn label(self) -> &'static str {
        match self {
            Self::RoundedRect => "圆角矩形",
            Self::Ellipse => "椭圆",
        }
    }
}

/// Where the window sits on the nine-grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PictureInPictureAnchor {
    /// Left column, top row.
    TopLeft = 0,
    /// Centre column, top row.
    Top = 1,
    /// Right column, top row.
    TopRight = 2,
    /// Left column, middle row.
    Left = 3,
    /// Centre column, middle row.
    Centre = 4,
    /// Right column, middle row.
    Right = 5,
    /// Left column, bottom row.
    BottomLeft = 6,
    /// Centre column, bottom row.
    Bottom = 7,
    /// Right column, bottom row.
    BottomRight = 8,
}

impl PictureInPictureAnchor {
    /// Every anchor in code order.
    pub const ALL: [Self; 9] = [
        Self::TopLeft,
        Self::Top,
        Self::TopRight,
        Self::Left,
        Self::Centre,
        Self::Right,
        Self::BottomLeft,
        Self::Bottom,
        Self::BottomRight,
    ];

    /// Documented default.
    pub const DEFAULT: Self = Self::TopLeft;

    /// Packed tag used by the graph, the schema and the spatial pass.
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// Inverse of [`Self::code`]; out-of-range codes are authoring errors.
    pub fn from_code(code: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|anchor| anchor.code() == code)
    }

    /// Nine-grid row, `0..=2`.
    pub const fn row(self) -> u8 {
        self.code() / 3
    }

    /// Nine-grid column, `0..=2`.
    pub const fn column(self) -> u8 {
        self.code() % 3
    }

    /// Stable persisted key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::TopLeft => "top_left",
            Self::Top => "top",
            Self::TopRight => "top_right",
            Self::Left => "left",
            Self::Centre => "centre",
            Self::Right => "right",
            Self::BottomLeft => "bottom_left",
            Self::Bottom => "bottom",
            Self::BottomRight => "bottom_right",
        }
    }

    /// Inverse of [`Self::as_key`].
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|anchor| anchor.as_key() == key)
    }

    /// Chinese caption.
    pub const fn label(self) -> &'static str {
        match self {
            Self::TopLeft => "左上",
            Self::Top => "顶部",
            Self::TopRight => "右上",
            Self::Left => "左侧",
            Self::Centre => "居中",
            Self::Right => "右侧",
            Self::BottomLeft => "左下",
            Self::Bottom => "底部",
            Self::BottomRight => "右下",
        }
    }
}

/// One authored picture in picture configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PictureInPictureParameters {
    /// How the frame is fitted into the window.
    pub content: PictureInPictureContent,
    /// Content zoom about the window centre.
    pub scale: f32,
    /// Horizontal content shift, as a fraction of the window width.
    pub pan_x: f32,
    /// Vertical content shift, as a fraction of the window height.
    pub pan_y: f32,
    /// Window edge softness, in pixels.
    pub anti_alias: f32,
    /// Nine-grid position of the window centre.
    pub anchor: PictureInPictureAnchor,
    /// Derive the height from the width and `aspect_ratio`.
    pub lock_aspect: bool,
    /// Window width, as a fraction of the frame width.
    pub width: f32,
    /// Window height, as a fraction of the frame height.
    pub height: f32,
    /// Window pixel aspect, where `1` is square.
    pub aspect_ratio: f32,
    /// Window outline.
    pub style: PictureInPictureStyle,
    /// Corner rounding, as a fraction of the short half axis.
    pub rounding: f32,
    /// Window rotation, in degrees.
    pub rotation: f32,
    /// Window coverage.
    pub opacity: f32,
    /// Paint the border.
    pub border: f32,
    /// Border width; the magnitude is the band width.
    pub border_width: f32,
    /// Border red.
    pub border_red: f32,
    /// Border green.
    pub border_green: f32,
    /// Border blue.
    pub border_blue: f32,
    /// Border coverage.
    pub border_opacity: f32,
    /// Reuse the border colour for the fill.
    pub fill_matching: f32,
    /// Paint the fill behind the content.
    pub fill: f32,
    /// Fill red.
    pub fill_red: f32,
    /// Fill green.
    pub fill_green: f32,
    /// Fill blue.
    pub fill_blue: f32,
    /// Fill coverage.
    pub fill_opacity: f32,
    /// Paint the drop shadow.
    pub shadow: f32,
    /// Drop shadow coverage.
    pub shadow_strength: f32,
    /// Drop shadow red.
    pub shadow_red: f32,
    /// Drop shadow green.
    pub shadow_green: f32,
    /// Drop shadow blue.
    pub shadow_blue: f32,
    /// Drop shadow angle, in degrees.
    pub shadow_angle: f32,
    /// Drop shadow offset, as a fraction of the shorter frame side.
    pub shadow_distance: f32,
    /// Drop shadow size multiplier about the window centre.
    pub shadow_zoom: f32,
    /// Drop shadow soft edge, as a fraction of the shorter frame side.
    pub shadow_blur: f32,
}

impl Default for PictureInPictureParameters {
    fn default() -> Self {
        Self {
            content: PIP_DEFAULT_CONTENT,
            scale: PIP_DEFAULT_SCALE,
            pan_x: PIP_DEFAULT_PAN_X,
            pan_y: PIP_DEFAULT_PAN_Y,
            anti_alias: PIP_DEFAULT_ANTI_ALIAS,
            anchor: PIP_DEFAULT_ANCHOR,
            lock_aspect: PIP_DEFAULT_LOCK_ASPECT,
            width: PIP_DEFAULT_WIDTH,
            height: PIP_DEFAULT_HEIGHT,
            aspect_ratio: PIP_DEFAULT_ASPECT_RATIO,
            style: PIP_DEFAULT_STYLE,
            rounding: PIP_DEFAULT_ROUNDING,
            rotation: PIP_DEFAULT_ROTATION,
            opacity: PIP_DEFAULT_OPACITY,
            border: PIP_DEFAULT_BORDER,
            border_width: PIP_DEFAULT_BORDER_WIDTH,
            border_red: PIP_DEFAULT_BORDER_RED,
            border_green: PIP_DEFAULT_BORDER_GREEN,
            border_blue: PIP_DEFAULT_BORDER_BLUE,
            border_opacity: PIP_DEFAULT_BORDER_OPACITY,
            fill_matching: PIP_DEFAULT_FILL_MATCHING,
            fill: PIP_DEFAULT_FILL,
            fill_red: PIP_DEFAULT_FILL_RED,
            fill_green: PIP_DEFAULT_FILL_GREEN,
            fill_blue: PIP_DEFAULT_FILL_BLUE,
            fill_opacity: PIP_DEFAULT_FILL_OPACITY,
            shadow: PIP_DEFAULT_SHADOW,
            shadow_strength: PIP_DEFAULT_SHADOW_STRENGTH,
            shadow_red: PIP_DEFAULT_SHADOW_RED,
            shadow_green: PIP_DEFAULT_SHADOW_GREEN,
            shadow_blue: PIP_DEFAULT_SHADOW_BLUE,
            shadow_angle: PIP_DEFAULT_SHADOW_ANGLE,
            shadow_distance: PIP_DEFAULT_SHADOW_DISTANCE,
            shadow_zoom: PIP_DEFAULT_SHADOW_ZOOM,
            shadow_blur: PIP_DEFAULT_SHADOW_BLUR,
        }
    }
}

/// Reports the first authored value that falls outside its range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("picture_in_picture.{parameter} must be finite and within its documented range")]
pub struct PictureInPictureError {
    parameter: &'static str,
}

impl PictureInPictureError {
    /// Stable snake_case id of the offending parameter.
    pub const fn parameter(self) -> &'static str {
        self.parameter
    }
}

/// The shared spatial pass cells and the dedicated picture in picture block
/// produced by [`PictureInPictureGrade::gpu_controls`].
pub type PictureInPictureGpuControls = ([[f32; 4]; 5], [[f32; 4]; 5]);

/// A validated picture in picture configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PictureInPictureGrade {
    parameters: PictureInPictureParameters,
}

impl PictureInPictureGrade {
    /// Validates one authored configuration.
    pub fn new(parameters: PictureInPictureParameters) -> Result<Self, PictureInPictureError> {
        let numeric = [
            ("scale", parameters.scale, PIP_MIN_SCALE, PIP_MAX_SCALE),
            ("pan_x", parameters.pan_x, -1.0, 1.0),
            ("pan_y", parameters.pan_y, -1.0, 1.0),
            ("anti_alias", parameters.anti_alias, 0.0, PIP_MAX_ANTI_ALIAS),
            ("width", parameters.width, PIP_MIN_WINDOW, 1.0),
            ("height", parameters.height, PIP_MIN_WINDOW, 1.0),
            (
                "aspect_ratio",
                parameters.aspect_ratio,
                PIP_MIN_ASPECT_RATIO,
                PIP_MAX_ASPECT_RATIO,
            ),
            ("rounding", parameters.rounding, 0.0, 1.0),
            (
                "rotation",
                parameters.rotation,
                -PIP_MAX_ANGLE,
                PIP_MAX_ANGLE,
            ),
            ("opacity", parameters.opacity, 0.0, 1.0),
            ("border", parameters.border, 0.0, 1.0),
            (
                "border_width",
                parameters.border_width,
                -PIP_MAX_BORDER_WIDTH,
                PIP_MAX_BORDER_WIDTH,
            ),
            ("border_red", parameters.border_red, 0.0, 1.0),
            ("border_green", parameters.border_green, 0.0, 1.0),
            ("border_blue", parameters.border_blue, 0.0, 1.0),
            ("border_opacity", parameters.border_opacity, 0.0, 1.0),
            ("fill_matching", parameters.fill_matching, 0.0, 1.0),
            ("fill", parameters.fill, 0.0, 1.0),
            ("fill_red", parameters.fill_red, 0.0, 1.0),
            ("fill_green", parameters.fill_green, 0.0, 1.0),
            ("fill_blue", parameters.fill_blue, 0.0, 1.0),
            ("fill_opacity", parameters.fill_opacity, 0.0, 1.0),
            ("shadow", parameters.shadow, 0.0, 1.0),
            ("shadow_strength", parameters.shadow_strength, 0.0, 1.0),
            ("shadow_red", parameters.shadow_red, 0.0, 1.0),
            ("shadow_green", parameters.shadow_green, 0.0, 1.0),
            ("shadow_blue", parameters.shadow_blue, 0.0, 1.0),
            (
                "shadow_angle",
                parameters.shadow_angle,
                -PIP_MAX_ANGLE,
                PIP_MAX_ANGLE,
            ),
            (
                "shadow_distance",
                parameters.shadow_distance,
                -PIP_MAX_SHADOW_DISTANCE,
                PIP_MAX_SHADOW_DISTANCE,
            ),
            (
                "shadow_zoom",
                parameters.shadow_zoom,
                0.0,
                PIP_MAX_SHADOW_ZOOM,
            ),
            ("shadow_blur", parameters.shadow_blur, 0.0, 1.0),
        ];
        for (parameter, value, minimum, maximum) in numeric {
            if !value.is_finite() || !(minimum..=maximum).contains(&value) {
                return Err(PictureInPictureError { parameter });
            }
        }
        Ok(Self { parameters })
    }

    /// Documented defaults.
    pub fn documented_defaults() -> Self {
        Self::new(PictureInPictureParameters::default())
            .expect("the documented picture in picture defaults are in range")
    }

    /// The authored values.
    pub const fn parameters(&self) -> &PictureInPictureParameters {
        &self.parameters
    }

    /// Whether the configuration leaves every pixel untouched.
    ///
    /// A window that covers nothing, with the shadow, the border and the fill
    /// all off, has no visible output at all, so the graph can skip the node.
    pub const fn is_identity(&self) -> bool {
        self.parameters.opacity <= 0.0
            && self.parameters.shadow < 0.5
            && self.parameters.border < 0.5
            && self.parameters.fill < 0.5
    }

    /// Relative cost of one spatial pass.
    pub const fn cost(&self) -> u8 {
        5
    }

    /// Input halo. Every read clamps to the frame, so no padding is needed.
    pub const fn input_halo(&self) -> (u32, u32) {
        (0, 0)
    }

    /// The pixels the window occupies at one frame size, as
    /// `[width, height]`.
    pub fn window_pixels(&self, width: usize, height: usize) -> [f32; 2] {
        self.geometry(width.max(1), height.max(1)).window
    }

    /// Resolves the frame-dependent window geometry exactly once.
    ///
    /// The CPU kernel reads the numbers through this helper and
    /// [`Self::gpu_controls`] packs the same fields, so both execution paths
    /// share one source of truth for the resolved pixel sizes and the centre.
    fn geometry(&self, width: usize, height: usize) -> PictureInPictureGeometry {
        let parameters = &self.parameters;
        let frame = [width as f32, height as f32];
        let mut window = [
            (parameters.width * frame[0]).max(1.0),
            (parameters.height * frame[1]).max(1.0),
        ];
        if parameters.lock_aspect {
            let aspect = parameters.aspect_ratio.clamp(PIP_MIN_ASPECT_RATIO, PIP_MAX_ASPECT_RATIO);
            window[1] = (window[0] * (frame[0] / frame[1]) / aspect).max(1.0);
        }
        // The nine-grid offsets use the resolved fractions so a locked aspect
        // never pushes the window over the frame edge; without the lock this is
        // exactly the documented `1 - width` / `1 - height` form.
        let offset_x = f32::from(parameters.anchor.column()) - 1.0;
        let offset_y = f32::from(parameters.anchor.row()) - 1.0;
        let centre = [
            (0.5 + offset_x * (1.0 - window[0] / frame[0]) / 2.0) * frame[0],
            (0.5 + offset_y * (1.0 - window[1] / frame[1]) / 2.0) * frame[1],
        ];
        let theta = parameters.rotation.to_radians();
        let half = [window[0] / 2.0, window[1] / 2.0];
        let radius =
            (parameters.rounding.clamp(0.0, 1.0) * half[0].min(half[1])).min(PIP_MAX_THICKNESS);
        let fit = self.fit(window, frame);
        PictureInPictureGeometry {
            frame,
            window,
            centre,
            cos_theta: theta.cos(),
            sin_theta: theta.sin(),
            theta,
            half,
            radius,
            edge: parameters.anti_alias.max(0.5),
            fit,
            transparent_out_of_bounds: parameters.content == PictureInPictureContent::Contain,
        }
    }

    /// Content fit factor for one resolved window size.
    fn fit(&self, window: [f32; 2], frame: [f32; 2]) -> [f32; 2] {
        match self.parameters.content {
            PictureInPictureContent::Stretch => [frame[0] / window[0], frame[1] / window[1]],
            PictureInPictureContent::Contain => {
                let ratio = (window[0] / frame[0]).min(window[1] / frame[1]);
                [1.0 / ratio, 1.0 / ratio]
            }
            PictureInPictureContent::Cover => {
                let ratio = (window[0] / frame[0]).max(window[1] / frame[1]);
                [1.0 / ratio, 1.0 / ratio]
            }
        }
    }

    /// Packs the grade into the spatial pass uniform cells.
    ///
    /// The first block is the shared five-cell spatial uniform in its documented
    /// order: `[W, H, transparent out of bounds, 0]`, `[wpx, hpx, theta, 0]`,
    /// `[scale, pan x, pan y, anti alias]`,
    /// `[centre x, centre y, rounding px, content code]` and
    /// `[style code, opacity, border flag, border width px]`. The `params.w` lane
    /// stays zero because the recorder overwrites it with the linear-filter
    /// flag. The second block carries the colours and the shadow: `[shadow rgb,
    /// strength]`, `[shadow angle, distance px, zoom, blur px]`, `[fill rgb,
    /// fill opacity]`, `[fill flag, fill matching flag, shadow flag, border
    /// opacity]` and `[border rgb, 0]`.
    pub fn gpu_controls(&self, width: usize, height: usize) -> PictureInPictureGpuControls {
        if width == 0 || height == 0 {
            return ([[0.0; 4]; 5], [[0.0; 4]; 5]);
        }
        let parameters = &self.parameters;
        let geometry = self.geometry(width, height);
        let shorter = geometry.frame[0].min(geometry.frame[1]);
        let half_min = geometry.half[0].min(geometry.half[1]);
        let border_width = (parameters.border_width.abs() * half_min).min(PIP_MAX_THICKNESS);
        let shadow_blur = parameters.shadow_blur.clamp(0.0, 1.0) * 0.5 * shorter;
        let shadow_distance = parameters.shadow_distance * shorter;
        let border_rgb = [
            parameters.border_red,
            parameters.border_green,
            parameters.border_blue,
        ];
        // §5 packs the authored fill colour; the shader swaps in the border
        // colour when the fill-matching flag is set.
        let fill_rgb = [
            parameters.fill_red,
            parameters.fill_green,
            parameters.fill_blue,
        ];
        let shared = [
            [
                geometry.frame[0],
                geometry.frame[1],
                flag(geometry.transparent_out_of_bounds),
                0.0,
            ],
            [geometry.window[0], geometry.window[1], geometry.theta, 0.0],
            [
                parameters.scale,
                parameters.pan_x,
                parameters.pan_y,
                parameters.anti_alias,
            ],
            [
                geometry.centre[0],
                geometry.centre[1],
                geometry.radius,
                f32::from(parameters.content.code()),
            ],
            [
                f32::from(parameters.style.code()),
                parameters.opacity,
                flag(parameters.border >= 0.5),
                border_width,
            ],
        ];
        let dedicated = [
            [
                parameters.shadow_red,
                parameters.shadow_green,
                parameters.shadow_blue,
                parameters.shadow_strength,
            ],
            [
                parameters.shadow_angle.to_radians(),
                shadow_distance,
                parameters.shadow_zoom,
                shadow_blur,
            ],
            [
                fill_rgb[0],
                fill_rgb[1],
                fill_rgb[2],
                parameters.fill_opacity,
            ],
            [
                flag(parameters.fill >= 0.5),
                flag(parameters.fill_matching >= 0.5),
                flag(parameters.shadow >= 0.5),
                parameters.border_opacity,
            ],
            [border_rgb[0], border_rgb[1], border_rgb[2], 0.0],
        ];
        (shared, dedicated)
    }

    /// Feeds the whole configuration into a graph or plan fingerprint.
    pub fn hash_state<H: Hasher>(&self, state: &mut H) {
        let parameters = &self.parameters;
        parameters.content.code().hash(state);
        parameters.anchor.code().hash(state);
        parameters.style.code().hash(state);
        parameters.lock_aspect.hash(state);
        for value in [
            parameters.scale,
            parameters.pan_x,
            parameters.pan_y,
            parameters.anti_alias,
            parameters.width,
            parameters.height,
            parameters.aspect_ratio,
            parameters.rounding,
            parameters.rotation,
            parameters.opacity,
            parameters.border,
            parameters.border_width,
            parameters.border_red,
            parameters.border_green,
            parameters.border_blue,
            parameters.border_opacity,
            parameters.fill_matching,
            parameters.fill,
            parameters.fill_red,
            parameters.fill_green,
            parameters.fill_blue,
            parameters.fill_opacity,
            parameters.shadow,
            parameters.shadow_strength,
            parameters.shadow_red,
            parameters.shadow_green,
            parameters.shadow_blue,
            parameters.shadow_angle,
            parameters.shadow_distance,
            parameters.shadow_zoom,
            parameters.shadow_blur,
        ] {
            value.to_bits().hash(state);
        }
    }
}

impl Hash for PictureInPictureGrade {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash_state(state);
    }
}

/// Frame-dependent window geometry shared by the kernel and the uniform packer.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PictureInPictureGeometry {
    /// Frame size in pixels.
    frame: [f32; 2],
    /// Window size in pixels, both at least one.
    window: [f32; 2],
    /// Window centre in pixel coordinates.
    centre: [f32; 2],
    /// Cosine of the window rotation.
    cos_theta: f32,
    /// Sine of the window rotation.
    sin_theta: f32,
    /// Window rotation in radians, as the shader receives it.
    theta: f32,
    /// Half the window size in pixels.
    half: [f32; 2],
    /// Corner radius in pixels.
    radius: f32,
    /// Anti-aliasing width in pixels.
    edge: f32,
    /// Content fit factor.
    fit: [f32; 2],
    /// Whether `contain` reads outside the frame as exactly transparent.
    transparent_out_of_bounds: bool,
}

impl PictureInPictureGeometry {
    /// Maps one frame position into the window's local frame.
    #[inline]
    fn local(&self, position: [f32; 2], centre: [f32; 2]) -> [f32; 2] {
        let offset_x = position[0] - centre[0];
        let offset_y = position[1] - centre[1];
        [
            self.cos_theta * offset_x + self.sin_theta * offset_y,
            -self.sin_theta * offset_x + self.cos_theta * offset_y,
        ]
    }
}

/// Signed distance of the authored shape, in pixels.
///
/// Both styles are negative inside, so one coverage formula serves both edges
/// (design §3.2's per-style sign is normalised to that convention; the WGSL
/// pass uses the same form, and parity is the acceptance criterion).
#[inline]
fn shape_distance(
    local: [f32; 2],
    half: [f32; 2],
    radius: f32,
    style: PictureInPictureStyle,
) -> f32 {
    match style {
        PictureInPictureStyle::RoundedRect => {
            let inset = [
                local[0].abs() - (half[0] - radius),
                local[1].abs() - (half[1] - radius),
            ];
            let outside = (inset[0].max(0.0).powi(2) + inset[1].max(0.0).powi(2)).sqrt();
            let inside = inset[0].max(inset[1]).min(0.0);
            outside + inside - radius
        }
        PictureInPictureStyle::Ellipse => {
            let u = local[0] / half[0];
            let v = local[1] / half[1];
            ((u * u + v * v).sqrt() - 1.0) * half[0].min(half[1])
        }
    }
}

/// Coverage of the authored shape at one signed distance.
#[inline]
fn shape_coverage(distance: f32, edge: f32) -> f32 {
    1.0 - smoothstep(0.0, edge, distance)
}

/// The painted band of the border, given the edge distance and the resolved
/// band width in pixels.
#[inline]
fn border_ring(distance: f32, border_width: f32, edge: f32) -> f32 {
    let outer = shape_coverage(distance, edge);
    let inset = shape_coverage(distance + border_width, edge);
    (outer - inset).abs().clamp(0.0, 1.0)
}

/// Packs a boolean uniform lane as `0.0` or `1.0`, like WGSL `f32(bool)`.
#[inline]
fn flag(value: bool) -> f32 {
    if value {
        1.0
    } else {
        0.0
    }
}

/// Hermite interpolation, matching WGSL `smoothstep`.
#[inline]
fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    if edge0 >= edge1 {
        return if value < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// One premultiplied source-over step.
#[inline]
fn over(destination: [f32; 4], colour: [f32; 3], alpha: f32) -> [f32; 4] {
    let keep = 1.0 - alpha;
    [
        colour[0] * alpha + destination[0] * keep,
        colour[1] * alpha + destination[1] * keep,
        colour[2] * alpha + destination[2] * keep,
        alpha + destination[3] * keep,
    ]
}

/// Applies the picture in picture composite to one Float32 frame with a
/// cooperative checkpoint.
///
/// Returns `Ok(false)` when the buffers are inconsistent, and `Ok(true)` after
/// writing the whole frame.
pub fn apply_picture_in_picture_controlled<E>(
    primary: &mut [[f32; 4]],
    width: u32,
    height: u32,
    grade: &PictureInPictureGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    checkpoint()?;
    let Some(pixel_count) = (width as usize).checked_mul(height as usize) else {
        return Ok(false);
    };
    if pixel_count == 0 || primary.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }

    let frame_width = width as usize;
    let frame_height = height as usize;
    let frame = &mut primary[..pixel_count];
    premultiply_rgba_controlled(frame, checkpoint)?;
    let source = frame.to_vec();

    let parameters = grade.parameters();
    let geometry = grade.geometry(frame_width, frame_height);
    let frame_size = geometry.frame;
    let edge = geometry.edge;
    let style = parameters.style;
    let window_alpha_scale = parameters.opacity.clamp(0.0, 1.0);
    let content_scale = parameters.scale.max(PIP_MIN_CONTENT_SCALE);
    let border_flag = parameters.border >= 0.5;
    let fill_flag = parameters.fill >= 0.5;
    let shadow_flag = parameters.shadow >= 0.5;
    let half_min = geometry.half[0].min(geometry.half[1]);
    let border_width = (parameters.border_width.abs() * half_min).min(PIP_MAX_THICKNESS);
    let border_colour = [
        parameters.border_red,
        parameters.border_green,
        parameters.border_blue,
    ];
    let fill_colour = if parameters.fill_matching >= 0.5 {
        border_colour
    } else {
        [
            parameters.fill_red,
            parameters.fill_green,
            parameters.fill_blue,
        ]
    };
    let fill_alpha_scale = parameters.fill_opacity.clamp(0.0, 1.0);
    let border_alpha_scale = parameters.border_opacity.clamp(0.0, 1.0);
    let shadow_colour = [
        parameters.shadow_red,
        parameters.shadow_green,
        parameters.shadow_blue,
    ];
    let shadow_strength = parameters.shadow_strength.clamp(0.0, 1.0);
    let shorter = frame_size[0].min(frame_size[1]);
    let shadow_angle = parameters.shadow_angle.to_radians();
    let shadow_offset = parameters.shadow_distance * shorter;
    let shadow_zoom = parameters.shadow_zoom.clamp(0.0, PIP_MAX_SHADOW_ZOOM);
    let shadow_blur = parameters.shadow_blur.clamp(0.0, 1.0) * 0.5 * shorter;
    let shadow_half = [
        geometry.half[0] * shadow_zoom,
        geometry.half[1] * shadow_zoom,
    ];
    let shadow_radius = geometry.radius * shadow_zoom;
    let shadow_centre = [
        geometry.centre[0] + shadow_angle.cos() * shadow_offset,
        geometry.centre[1] + shadow_angle.sin() * shadow_offset,
    ];
    let fit = geometry.fit;
    let transparent_out_of_bounds = geometry.transparent_out_of_bounds;
    let window_metrics = [geometry.window[0] / fit[0], geometry.window[1] / fit[1]];

    // The rows are split into checkpoint-sized chunks so the kernel reports
    // progress about every `CHECKPOINT_STRIDE` rows without threading the
    // checkpoint into the parallel worker closure.
    let mut row_start = 0;
    while row_start < frame_height {
        let rows_end = (row_start + CHECKPOINT_STRIDE).min(frame_height);
        crate::parallel::for_each_row_band(
            frame,
            frame_width,
            row_start..rows_end,
            |start, band| {
                for (row_offset, row_pixels) in band.chunks_mut(frame_width).enumerate() {
                    let row = start + row_offset;
                    let centre_y = row as f32 + 0.5;
                    for (column, pixel) in row_pixels.iter_mut().enumerate() {
                        let position = [column as f32 + 0.5, centre_y];
                        let local = geometry.local(position, geometry.centre);
                        let distance = shape_distance(local, geometry.half, geometry.radius, style);
                        let coverage = shape_coverage(distance, edge);
                        let window_alpha = coverage * window_alpha_scale;
                        let mut shadow_alpha = 0.0;
                        if shadow_flag {
                            let shadow_local = geometry.local(position, shadow_centre);
                            let shadow_distance =
                                shape_distance(shadow_local, shadow_half, shadow_radius, style);
                            let shadow_coverage = if shadow_blur <= 0.0 {
                                if shadow_distance <= 0.0 {
                                    1.0
                                } else {
                                    0.0
                                }
                            } else {
                                1.0 - smoothstep(-shadow_blur, shadow_blur, shadow_distance)
                            };
                            shadow_alpha = shadow_strength * shadow_coverage;
                        }
                        // A pixel that neither the window nor the shadow touches
                        // keeps its input value: it is still premultiplied here,
                        // and the unpremultiply pass restores the straight frame.
                        if window_alpha <= 0.0 && shadow_alpha <= 0.0 {
                            continue;
                        }

                        let mut result = *pixel;
                        if shadow_alpha > 0.0 {
                            result = over(result, shadow_colour, shadow_alpha);
                        }

                        let mut content = [0.0_f32; 4];
                        if window_alpha > 0.0 {
                            let source_position = [
                                frame_size[0] * 0.5
                                    + local[0] * fit[0] / content_scale
                                    + parameters.pan_x * window_metrics[0],
                                frame_size[1] * 0.5
                                    + local[1] * fit[1] / content_scale
                                    + parameters.pan_y * window_metrics[1],
                            ];
                            let inside = !transparent_out_of_bounds
                                || (source_position[0] >= 0.0
                                    && source_position[0] < frame_size[0]
                                    && source_position[1] >= 0.0
                                    && source_position[1] < frame_size[1]);
                            if inside {
                                content =
                                    straight_rgba_from_premultiplied(sample_bilinear_clamped(
                                        &source,
                                        frame_width,
                                        frame_height,
                                        f64::from(source_position[0] - 0.5),
                                        f64::from(source_position[1] - 0.5),
                                    ));
                            }
                        }
                        let content_alpha = content[3] * window_alpha;

                        if fill_flag {
                            let fill_alpha =
                                fill_alpha_scale * window_alpha * (1.0 - content_alpha);
                            result = over(result, fill_colour, fill_alpha);
                        }

                        result = over(result, [content[0], content[1], content[2]], content_alpha);

                        if border_flag {
                            let ring = border_ring(distance, border_width, edge);
                            let border_alpha = border_alpha_scale * ring;
                            result = over(result, border_colour, border_alpha);
                        }

                        // The composite stays premultiplied: the frame-wide
                        // unpremultiply below unassociates every pixel exactly
                        // once, including the ones this loop left untouched, so
                        // the per-pixel result must not be unassociated here.
                        // A pixel that ends without coverage carries no colour.
                        *pixel = if result[3] > 0.0 { result } else { [0.0; 4] };
                    }
                }
            },
        );
        checkpoint()?;
        row_start = rows_end;
    }

    unpremultiply_rgba_controlled(frame, checkpoint)?;
    checkpoint()?;
    Ok(true)
}

/// Applies the picture in picture composite to one Float32 frame.
///
/// Returns `false` when the buffers are inconsistent.
pub fn apply_picture_in_picture_f32(
    primary: &mut [[f32; 4]],
    width: u32,
    height: u32,
    grade: &PictureInPictureGrade,
) -> bool {
    apply_picture_in_picture_controlled(primary, width, height, grade, &mut || {
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

/// Rows between two cooperative checkpoints.
const CHECKPOINT_STRIDE: usize = 4096;

#[cfg(test)]
mod tests {
    // The fixtures below start from the documented defaults and then poke one
    // authored field, which is exactly the shape this lint discourages.
    #![allow(clippy::field_reassign_with_default)]

    use super::*;
    use std::collections::hash_map::DefaultHasher;

    fn grade(parameters: PictureInPictureParameters) -> PictureInPictureGrade {
        PictureInPictureGrade::new(parameters).expect("the test grade is inside its ranges")
    }

    fn apply(
        frame: &mut [[f32; 4]],
        width: usize,
        height: usize,
        parameters: PictureInPictureParameters,
    ) {
        let grade = grade(parameters);
        assert!(apply_picture_in_picture_f32(
            frame,
            u32::try_from(width).expect("width fits in u32"),
            u32::try_from(height).expect("height fits in u32"),
            &grade,
        ));
    }

    /// A frame whose red and green channels encode the pixel position.
    fn gradient(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut frame = Vec::with_capacity(width * height);
        for row in 0..height {
            for column in 0..width {
                frame.push([
                    (column as f32 + 0.5) / width as f32,
                    (row as f32 + 0.5) / height as f32,
                    0.25,
                    1.0,
                ]);
            }
        }
        frame
    }

    fn pixel(frame: &[[f32; 4]], width: usize, column: usize, row: usize) -> [f32; 4] {
        frame[row * width + column]
    }

    fn assert_close(actual: f32, expected: f32, tolerance: f32, what: &str) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "{what}: {actual} vs {expected}"
        );
    }

    fn assert_pixel_close(actual: [f32; 4], expected: [f32; 4], tolerance: f32, what: &str) {
        for channel in 0..4 {
            assert!(
                (actual[channel] - expected[channel]).abs() <= tolerance,
                "{what}: channel {channel}: {actual:?} vs {expected:?}"
            );
        }
    }

    fn hash_of(grade: &PictureInPictureGrade) -> u64 {
        let mut hasher = DefaultHasher::new();
        grade.hash_state(&mut hasher);
        hasher.finish()
    }

    /// The documented §3.3 source point of one frame pixel.
    fn source_point(
        grade: &PictureInPictureGrade,
        width: usize,
        height: usize,
        column: usize,
        row: usize,
    ) -> [f32; 2] {
        let parameters = grade.parameters();
        let geometry = grade.geometry(width, height);
        let local = geometry.local([column as f32 + 0.5, row as f32 + 0.5], geometry.centre);
        let scale = parameters.scale.max(PIP_MIN_CONTENT_SCALE);
        [
            geometry.frame[0] * 0.5
                + local[0] * geometry.fit[0] / scale
                + parameters.pan_x * geometry.window[0] / geometry.fit[0],
            geometry.frame[1] * 0.5
                + local[1] * geometry.fit[1] / scale
                + parameters.pan_y * geometry.window[1] / geometry.fit[1],
        ]
    }

    /// Writes one parameter by its schema id, rounding the enum-valued floats
    /// the same way the graph builder does.
    fn assign(parameters: &mut PictureInPictureParameters, name: &str, value: f32) {
        let code = || u8::try_from(value.round() as i32).expect("the code is positive");
        match name {
            "content" => {
                parameters.content =
                    PictureInPictureContent::from_code(code()).expect("content code is valid");
            }
            "style" => {
                parameters.style =
                    PictureInPictureStyle::from_code(code()).expect("style code is valid");
            }
            "anchor" => {
                parameters.anchor =
                    PictureInPictureAnchor::from_code(code()).expect("anchor code is valid");
            }
            "lock_aspect" => parameters.lock_aspect = value >= 0.5,
            "scale" => parameters.scale = value,
            "pan_x" => parameters.pan_x = value,
            "pan_y" => parameters.pan_y = value,
            "anti_alias" => parameters.anti_alias = value,
            "width" => parameters.width = value,
            "height" => parameters.height = value,
            "aspect_ratio" => parameters.aspect_ratio = value,
            "rounding" => parameters.rounding = value,
            "rotation" => parameters.rotation = value,
            "opacity" => parameters.opacity = value,
            "border" => parameters.border = value,
            "border_width" => parameters.border_width = value,
            "border_red" => parameters.border_red = value,
            "border_green" => parameters.border_green = value,
            "border_blue" => parameters.border_blue = value,
            "border_opacity" => parameters.border_opacity = value,
            "fill_matching" => parameters.fill_matching = value,
            "fill" => parameters.fill = value,
            "fill_red" => parameters.fill_red = value,
            "fill_green" => parameters.fill_green = value,
            "fill_blue" => parameters.fill_blue = value,
            "fill_opacity" => parameters.fill_opacity = value,
            "shadow" => parameters.shadow = value,
            "shadow_strength" => parameters.shadow_strength = value,
            "shadow_red" => parameters.shadow_red = value,
            "shadow_green" => parameters.shadow_green = value,
            "shadow_blue" => parameters.shadow_blue = value,
            "shadow_angle" => parameters.shadow_angle = value,
            "shadow_distance" => parameters.shadow_distance = value,
            "shadow_zoom" => parameters.shadow_zoom = value,
            "shadow_blur" => parameters.shadow_blur = value,
            other => panic!("unknown parameter {other}"),
        }
    }

    /// A parameters value that differs from the documented default.
    fn authored_parameters(name: &str) -> PictureInPictureParameters {
        let value = match name {
            "content" => 1.0,
            "scale" => 2.0,
            "pan_x" => 0.25,
            "pan_y" => -0.25,
            "anti_alias" => 2.0,
            "anchor" => 4.0,
            "lock_aspect" => 1.0,
            "width" => 0.4,
            "height" => 0.5,
            "aspect_ratio" => 1.5,
            "style" => 2.0,
            "rounding" => 0.5,
            "rotation" => 30.0,
            "opacity" => 0.5,
            "border" => 1.0,
            "border_width" => 0.1,
            "border_red" => 0.25,
            "border_green" => 0.25,
            "border_blue" => 0.25,
            "border_opacity" => 0.5,
            "fill_matching" => 0.0,
            "fill" => 1.0,
            "fill_red" => 0.25,
            "fill_green" => 0.25,
            "fill_blue" => 0.25,
            "fill_opacity" => 0.5,
            "shadow" => 0.0,
            "shadow_strength" => 0.25,
            "shadow_red" => 0.25,
            "shadow_green" => 0.25,
            "shadow_blue" => 0.25,
            "shadow_angle" => 90.0,
            "shadow_distance" => 0.1,
            "shadow_zoom" => 2.0,
            "shadow_blur" => 0.25,
            other => panic!("unknown parameter {other}"),
        };
        let mut parameters = PictureInPictureParameters::default();
        assign(&mut parameters, name, value);
        parameters
    }

    #[test]
    fn documented_defaults_are_the_authored_defaults() {
        let parameters = PictureInPictureParameters::default();
        assert_eq!(parameters.content, PIP_DEFAULT_CONTENT);
        assert_eq!(parameters.anchor, PIP_DEFAULT_ANCHOR);
        assert_eq!(parameters.style, PIP_DEFAULT_STYLE);
        assert!(!parameters.lock_aspect);
        let numeric = [
            (parameters.scale, PIP_DEFAULT_SCALE),
            (parameters.pan_x, PIP_DEFAULT_PAN_X),
            (parameters.pan_y, PIP_DEFAULT_PAN_Y),
            (parameters.anti_alias, PIP_DEFAULT_ANTI_ALIAS),
            (parameters.width, PIP_DEFAULT_WIDTH),
            (parameters.height, PIP_DEFAULT_HEIGHT),
            (parameters.aspect_ratio, PIP_DEFAULT_ASPECT_RATIO),
            (parameters.rounding, PIP_DEFAULT_ROUNDING),
            (parameters.rotation, PIP_DEFAULT_ROTATION),
            (parameters.opacity, PIP_DEFAULT_OPACITY),
            (parameters.border, PIP_DEFAULT_BORDER),
            (parameters.border_width, PIP_DEFAULT_BORDER_WIDTH),
            (parameters.border_red, PIP_DEFAULT_BORDER_RED),
            (parameters.border_green, PIP_DEFAULT_BORDER_GREEN),
            (parameters.border_blue, PIP_DEFAULT_BORDER_BLUE),
            (parameters.border_opacity, PIP_DEFAULT_BORDER_OPACITY),
            (parameters.fill_matching, PIP_DEFAULT_FILL_MATCHING),
            (parameters.fill, PIP_DEFAULT_FILL),
            (parameters.fill_red, PIP_DEFAULT_FILL_RED),
            (parameters.fill_green, PIP_DEFAULT_FILL_GREEN),
            (parameters.fill_blue, PIP_DEFAULT_FILL_BLUE),
            (parameters.fill_opacity, PIP_DEFAULT_FILL_OPACITY),
            (parameters.shadow, PIP_DEFAULT_SHADOW),
            (parameters.shadow_strength, PIP_DEFAULT_SHADOW_STRENGTH),
            (parameters.shadow_red, PIP_DEFAULT_SHADOW_RED),
            (parameters.shadow_green, PIP_DEFAULT_SHADOW_GREEN),
            (parameters.shadow_blue, PIP_DEFAULT_SHADOW_BLUE),
            (parameters.shadow_angle, PIP_DEFAULT_SHADOW_ANGLE),
            (parameters.shadow_distance, PIP_DEFAULT_SHADOW_DISTANCE),
            (parameters.shadow_zoom, PIP_DEFAULT_SHADOW_ZOOM),
            (parameters.shadow_blur, PIP_DEFAULT_SHADOW_BLUR),
        ];
        for (actual, expected) in numeric {
            assert_eq!(actual, expected);
        }
        let grade = grade(parameters);
        assert!(!grade.is_identity());
        assert_eq!(grade.cost(), 5);
        assert_eq!(grade.input_halo(), (0, 0));
        let window = grade.window_pixels(24, 16);
        assert_close(window[0], 6.0, 1.0e-5, "window width");
        assert_close(window[1], 4.8, 1.0e-5, "window height");
    }

    #[test]
    fn codes_and_keys_round_trip() {
        for content in PictureInPictureContent::ALL {
            assert_eq!(
                PictureInPictureContent::from_code(content.code()),
                Some(content)
            );
            assert_eq!(
                PictureInPictureContent::from_key(content.as_key()),
                Some(content)
            );
            assert!(!content.label().is_empty());
        }
        for style in PictureInPictureStyle::ALL {
            assert_eq!(PictureInPictureStyle::from_code(style.code()), Some(style));
            assert_eq!(PictureInPictureStyle::from_key(style.as_key()), Some(style));
            assert!(!style.label().is_empty());
        }
        for anchor in PictureInPictureAnchor::ALL {
            assert_eq!(
                PictureInPictureAnchor::from_code(anchor.code()),
                Some(anchor)
            );
            assert_eq!(
                PictureInPictureAnchor::from_key(anchor.as_key()),
                Some(anchor)
            );
            assert!(!anchor.label().is_empty());
        }
        assert_eq!(PictureInPictureContent::Stretch.code(), 0);
        assert_eq!(PictureInPictureContent::Cover.code(), 2);
        assert_eq!(PictureInPictureStyle::RoundedRect.code(), 1);
        assert_eq!(PictureInPictureStyle::Ellipse.code(), 2);
        assert_eq!(PictureInPictureAnchor::TopLeft.code(), 0);
        assert_eq!(PictureInPictureAnchor::Centre.code(), 4);
        assert_eq!(PictureInPictureAnchor::BottomRight.code(), 8);
        assert_eq!(PictureInPictureAnchor::BottomRight.row(), 2);
        assert_eq!(PictureInPictureAnchor::BottomRight.column(), 2);
        assert_eq!(PictureInPictureContent::from_code(3), None);
        assert_eq!(PictureInPictureStyle::from_code(0), None);
        assert_eq!(PictureInPictureAnchor::from_code(9), None);
        assert_eq!(PictureInPictureAnchor::from_key("nowhere"), None);
    }

    #[test]
    fn params_outside_their_documented_ranges_error() {
        let cases = [
            ("scale", 0.001, 40.0),
            ("pan_x", -2.0, 2.0),
            ("pan_y", -2.0, 2.0),
            ("anti_alias", -1.0, 40.0),
            ("width", 0.0, 2.0),
            ("height", 0.0, 2.0),
            ("aspect_ratio", 0.01, 40.0),
            ("rounding", -0.5, 2.0),
            ("rotation", -400.0, 400.0),
            ("opacity", -0.5, 2.0),
            ("border", -0.5, 2.0),
            ("border_width", -2.0, 2.0),
            ("border_red", -0.5, 2.0),
            ("border_green", -0.5, 2.0),
            ("border_blue", -0.5, 2.0),
            ("border_opacity", -0.5, 2.0),
            ("fill_matching", -0.5, 2.0),
            ("fill", -0.5, 2.0),
            ("fill_red", -0.5, 2.0),
            ("fill_green", -0.5, 2.0),
            ("fill_blue", -0.5, 2.0),
            ("fill_opacity", -0.5, 2.0),
            ("shadow", -0.5, 2.0),
            ("shadow_strength", -0.5, 2.0),
            ("shadow_red", -0.5, 2.0),
            ("shadow_green", -0.5, 2.0),
            ("shadow_blue", -0.5, 2.0),
            ("shadow_angle", -400.0, 400.0),
            ("shadow_distance", -2.0, 2.0),
            ("shadow_zoom", -1.0, 6.0),
            ("shadow_blur", -1.0, 2.0),
        ];
        for (name, low, high) in cases {
            for value in [low, high] {
                let mut parameters = PictureInPictureParameters::default();
                assign(&mut parameters, name, value);
                let error = PictureInPictureGrade::new(parameters)
                    .expect_err("out of range values are rejected");
                assert_eq!(error.parameter(), name);
            }
        }
        let mut parameters = PictureInPictureParameters::default();
        parameters.scale = f32::NAN;
        assert_eq!(
            PictureInPictureGrade::new(parameters).expect_err("NaN is rejected").parameter(),
            "scale"
        );
    }

    #[test]
    fn identity_needs_the_window_off_with_no_border_fill_or_shadow() {
        let mut parameters = PictureInPictureParameters::default();
        parameters.opacity = 0.0;
        parameters.shadow = 0.0;
        assert!(grade(parameters).is_identity());

        parameters.opacity = 0.5;
        assert!(!grade(parameters).is_identity());

        let mut parameters = PictureInPictureParameters::default();
        parameters.opacity = 0.0;
        assert!(!grade(parameters).is_identity(), "the shadow is still on");

        let mut parameters = PictureInPictureParameters::default();
        parameters.opacity = 0.0;
        parameters.shadow = 0.0;
        parameters.border = 1.0;
        assert!(!grade(parameters).is_identity());

        let mut parameters = PictureInPictureParameters::default();
        parameters.opacity = 0.0;
        parameters.shadow = 0.0;
        parameters.fill = 1.0;
        assert!(!grade(parameters).is_identity());
    }

    #[test]
    fn gpu_controls_pack_the_documented_layout() {
        let default_grade = grade(PictureInPictureParameters::default());
        let (shared, dedicated) = default_grade.gpu_controls(24, 16);
        assert_pixel_close(shared[0], [24.0, 16.0, 0.0, 0.0], 1.0e-6, "size");
        assert_pixel_close(shared[1], [6.0, 4.8, 0.0, 0.0], 1.0e-4, "window");
        assert_pixel_close(shared[2], [1.0, 0.0, 0.0, 1.0], 1.0e-6, "extra");
        assert_pixel_close(shared[3], [3.0, 2.4, 0.84, 0.0], 1.0e-4, "aces");
        assert_pixel_close(shared[4], [1.0, 1.0, 0.0, 0.036], 1.0e-4, "extra1");
        assert_pixel_close(dedicated[0], [0.0, 0.0, 0.0, 0.65], 1.0e-6, "shadow");
        assert_pixel_close(
            dedicated[1],
            [45.0_f32.to_radians(), 0.32, 1.0, 3.6],
            1.0e-4,
            "shadow geometry",
        );
        assert_pixel_close(dedicated[2], [0.5, 0.5, 0.5, 1.0], 1.0e-6, "fill");
        assert_pixel_close(dedicated[3], [0.0, 1.0, 1.0, 1.0], 1.0e-6, "flags");
        assert_pixel_close(dedicated[4], [1.0, 1.0, 1.0, 0.0], 1.0e-6, "border");

        let mut parameters = PictureInPictureParameters::default();
        parameters.content = PictureInPictureContent::Contain;
        parameters.style = PictureInPictureStyle::Ellipse;
        parameters.border = 1.0;
        let (shared, _) = grade(parameters).gpu_controls(24, 16);
        assert_eq!(
            shared[0][2], 1.0,
            "contain publishes the out of bounds flag"
        );
        assert_eq!(shared[3][3], 1.0, "content code");
        assert_eq!(shared[4][0], 2.0, "style code");
        assert_eq!(shared[4][2], 1.0, "border flag");
        assert_eq!(
            grade(PictureInPictureParameters::default()).gpu_controls(0, 0).0,
            [[0.0; 4]; 5]
        );
    }

    #[test]
    fn the_nine_grid_anchors_place_the_window() {
        let default_grade = grade(PictureInPictureParameters::default());
        let window = default_grade.window_pixels(24, 16);
        for anchor in PictureInPictureAnchor::ALL {
            let mut parameters = PictureInPictureParameters::default();
            parameters.anchor = anchor;
            let (shared, _) = grade(parameters).gpu_controls(24, 16);
            let offset_x = f32::from(anchor.column()) - 1.0;
            let offset_y = f32::from(anchor.row()) - 1.0;
            let expected = [
                (0.5 + offset_x * (1.0 - window[0] / 24.0) / 2.0) * 24.0,
                (0.5 + offset_y * (1.0 - window[1] / 16.0) / 2.0) * 16.0,
            ];
            assert_close(shared[3][0], expected[0], 1.0e-4, anchor.as_key());
            assert_close(shared[3][1], expected[1], 1.0e-4, anchor.as_key());
        }
        let mut parameters = PictureInPictureParameters::default();
        parameters.anchor = PictureInPictureAnchor::Centre;
        let centre = grade(parameters).gpu_controls(24, 16).0[3];
        assert_close(centre[0], 12.0, 1.0e-4, "centred window");
        assert_close(centre[1], 8.0, 1.0e-4, "centred window");
    }

    #[test]
    fn lock_aspect_rescales_the_window_height() {
        let mut parameters = PictureInPictureParameters::default();
        parameters.width = 0.5;
        parameters.lock_aspect = true;
        let locked = grade(parameters).window_pixels(24, 16);
        assert_close(locked[0], 12.0, 1.0e-4, "locked width");
        assert_close(locked[1], 18.0, 1.0e-4, "square window on a 3:2 frame");

        parameters.aspect_ratio = 2.0;
        let wide = grade(parameters).window_pixels(24, 16);
        assert_close(wide[1], 9.0, 1.0e-4, "twice as wide as tall");

        parameters.lock_aspect = false;
        let free = grade(parameters).window_pixels(24, 16);
        assert_close(free[0], 12.0, 1.0e-4, "free width");
        assert_close(free[1], 4.8, 1.0e-4, "free height");
    }

    #[test]
    fn a_full_frame_window_reproduces_the_input() {
        let width = 8;
        let height = 6;
        let mut frame = gradient(width, height);
        for pixel in &mut frame {
            pixel[0] *= 1.5;
            pixel[1] *= 2.0;
        }
        let input = frame.clone();
        let mut parameters = PictureInPictureParameters::default();
        parameters.width = 1.0;
        parameters.height = 1.0;
        parameters.anchor = PictureInPictureAnchor::Centre;
        apply(&mut frame, width, height, parameters);
        assert_eq!(frame, input, "the full frame window is an exact identity");
    }

    #[test]
    fn contain_and_cover_map_the_source_differently() {
        let width = 12;
        let height = 8;
        let source = gradient(width, height);
        let mut parameters = PictureInPictureParameters::default();
        parameters.width = 0.5;
        parameters.height = 0.25;
        parameters.anchor = PictureInPictureAnchor::Centre;
        parameters.rounding = 0.0;
        parameters.anti_alias = 0.0;
        parameters.shadow = 0.0;
        parameters.border = 0.0;
        parameters.fill = 0.0;

        let mut contained = source.clone();
        parameters.content = PictureInPictureContent::Contain;
        let contain_grade = grade(parameters);
        apply(&mut contained, width, height, *contain_grade.parameters());
        let column = 3;
        let row = 3;
        assert_eq!(
            pixel(&contained, width, column, row),
            pixel(&source, width, column, row),
            "contain reads outside the frame as transparent"
        );

        let mut covered = source.clone();
        parameters.content = PictureInPictureContent::Cover;
        let cover_grade = grade(parameters);
        let point = source_point(&cover_grade, width, height, column, row);
        assert_close(point[0], 1.0, 1.0e-4, "cover source x");
        assert_close(point[1], 3.0, 1.0e-4, "cover source y");
        let expected = sample_bilinear_clamped(
            &source,
            width,
            height,
            f64::from(point[0] - 0.5),
            f64::from(point[1] - 0.5),
        );
        apply(&mut covered, width, height, *cover_grade.parameters());
        assert_pixel_close(
            pixel(&covered, width, column, row),
            expected,
            1.0e-5,
            "cover samples the cropped source",
        );
    }

    #[test]
    fn rotation_rotates_the_content_with_the_window() {
        let width = 8;
        let height = 8;
        let mut frame = vec![[0.2, 0.2, 0.2, 1.0]; width * height];
        for stripe in frame.iter_mut().take(width) {
            *stripe = [1.0, 1.0, 1.0, 1.0];
        }
        let input = frame.clone();
        let mut parameters = PictureInPictureParameters::default();
        parameters.width = 1.0;
        parameters.height = 1.0;
        parameters.anchor = PictureInPictureAnchor::Centre;
        parameters.rounding = 0.0;
        parameters.anti_alias = 0.5;
        parameters.rotation = 90.0;
        parameters.shadow = 0.0;
        let rotated = grade(parameters);
        apply(&mut frame, width, height, parameters);
        assert_pixel_close(
            pixel(&frame, width, 7, 2),
            [1.0, 1.0, 1.0, 1.0],
            1.0e-4,
            "the stripe runs down the window",
        );
        assert_pixel_close(
            pixel(&frame, width, 2, 2),
            [0.2, 0.2, 0.2, 1.0],
            1.0e-4,
            "the rest of the window keeps the backdrop",
        );
        let point = source_point(&rotated, width, height, 7, 2);
        let expected = sample_bilinear_clamped(
            &input,
            width,
            height,
            f64::from(point[0] - 0.5),
            f64::from(point[1] - 0.5),
        );
        assert_pixel_close(
            pixel(&frame, width, 7, 2),
            expected,
            1.0e-5,
            "the documented rotation mapping",
        );
    }

    #[test]
    fn rounded_rect_and_ellipse_feather_different_edges() {
        let width = 16;
        let height = 16;
        let source = gradient(width, height);
        let mut parameters = PictureInPictureParameters::default();
        parameters.width = 0.5;
        parameters.height = 0.5;
        parameters.anchor = PictureInPictureAnchor::Centre;
        parameters.rounding = 0.5;
        parameters.anti_alias = 1.0;
        parameters.shadow = 0.0;
        parameters.border = 0.0;
        parameters.fill = 0.0;

        let mut rounded = source.clone();
        parameters.style = PictureInPictureStyle::RoundedRect;
        apply(&mut rounded, width, height, parameters);
        let corner = pixel(&rounded, width, 4, 4);
        assert_ne!(
            corner,
            pixel(&source, width, 4, 4),
            "the rounded corner is still covered"
        );
        assert_pixel_close(
            pixel(&rounded, width, 8, 8),
            sample_bilinear_clamped(
                &source,
                width,
                height,
                f64::from(source_point(&grade(parameters), width, height, 8, 8)[0] - 0.5),
                f64::from(source_point(&grade(parameters), width, height, 8, 8)[1] - 0.5),
            ),
            1.0e-5,
            "the window centre shows the source",
        );

        let mut ellipse = source.clone();
        parameters.style = PictureInPictureStyle::Ellipse;
        apply(&mut ellipse, width, height, parameters);
        // The unified negative-inside distance feathers the edge one pixel
        // outward, so the probe sits just beyond that band.
        assert_eq!(
            pixel(&ellipse, width, 2, 2),
            pixel(&source, width, 2, 2),
            "the ellipse corner is outside the shape"
        );
        assert_ne!(
            pixel(&ellipse, width, 8, 8),
            [0.0; 4],
            "the ellipse still paints its centre"
        );
    }

    #[test]
    fn border_paints_the_band_inside_the_edge() {
        let width = 12;
        let height = 8;
        let source = gradient(width, height);
        let mut parameters = PictureInPictureParameters::default();
        parameters.width = 0.5;
        parameters.height = 0.5;
        parameters.anchor = PictureInPictureAnchor::Centre;
        parameters.rounding = 0.0;
        parameters.anti_alias = 0.0;
        parameters.shadow = 0.0;
        parameters.fill = 0.0;
        parameters.border = 1.0;
        parameters.border_width = -0.49;
        parameters.border_red = 0.0;
        parameters.border_green = 0.0;
        parameters.border_blue = 1.0;
        let resolved = grade(parameters);
        let mut frame = source.clone();
        apply(&mut frame, width, height, parameters);
        assert_pixel_close(
            pixel(&frame, width, 8, 4),
            [0.0, 0.0, 1.0, 1.0],
            0.02,
            "the band hugs the inner edge",
        );
        let point = source_point(&resolved, width, height, 6, 4);
        let expected = sample_bilinear_clamped(
            &source,
            width,
            height,
            f64::from(point[0] - 0.5),
            f64::from(point[1] - 0.5),
        );
        assert_pixel_close(
            pixel(&frame, width, 6, 4),
            expected,
            1.0e-5,
            "the middle of the window is not the border",
        );
        assert_eq!(
            pixel(&frame, width, 1, 4),
            pixel(&source, width, 1, 4),
            "outside the window the frame is untouched"
        );
        assert_eq!(
            pixel(&frame, width, 1, 4)[3],
            1.0,
            "the untouched pixel stays opaque"
        );
    }

    #[test]
    fn fill_paints_the_letterbox_and_fill_matching_reuses_the_border_colour() {
        let width = 12;
        let height = 8;
        let source = gradient(width, height);
        let mut parameters = PictureInPictureParameters::default();
        parameters.width = 0.5;
        parameters.height = 0.25;
        parameters.anchor = PictureInPictureAnchor::Centre;
        parameters.rounding = 0.0;
        parameters.anti_alias = 0.0;
        parameters.shadow = 0.0;
        parameters.border = 0.0;
        parameters.content = PictureInPictureContent::Contain;
        parameters.fill = 1.0;
        parameters.fill_opacity = 1.0;
        parameters.fill_red = 1.0;
        parameters.fill_green = 0.0;
        parameters.fill_blue = 1.0;
        parameters.fill_matching = 0.0;
        parameters.border_red = 0.0;
        parameters.border_green = 0.0;
        parameters.border_blue = 1.0;

        let mut frame = source.clone();
        apply(&mut frame, width, height, parameters);
        assert_pixel_close(
            pixel(&frame, width, 3, 3),
            [1.0, 0.0, 1.0, 1.0],
            1.0e-5,
            "the letterbox takes the fill colour",
        );

        parameters.fill_matching = 1.0;
        let mut matched = source.clone();
        apply(&mut matched, width, height, parameters);
        assert_pixel_close(
            pixel(&matched, width, 3, 3),
            [0.0, 0.0, 1.0, 1.0],
            1.0e-5,
            "fill matching reuses the border colour",
        );
    }

    #[test]
    fn shadow_offsets_zooms_and_softens_the_shape() {
        let width = 24;
        let height = 16;
        let source = gradient(width, height);

        let mut parameters = PictureInPictureParameters::default();
        parameters.opacity = 0.0;
        parameters.shadow_strength = 1.0;
        parameters.shadow_distance = 0.3;
        parameters.shadow_angle = 0.0;
        parameters.shadow_blur = 0.0;
        parameters.shadow_zoom = 1.0;

        let mut frame = source.clone();
        apply(&mut frame, width, height, parameters);
        assert_pixel_close(
            pixel(&frame, width, 5, 2),
            [0.0, 0.0, 0.0, 1.0],
            1.0e-5,
            "the shadow lands on the offset side",
        );
        assert_eq!(
            pixel(&frame, width, 2, 2),
            pixel(&source, width, 2, 2),
            "the shadow does not reach the far side"
        );

        parameters.shadow_zoom = 2.0;
        let mut zoomed = source.clone();
        apply(&mut zoomed, width, height, parameters);
        assert_ne!(
            pixel(&zoomed, width, 2, 2),
            pixel(&source, width, 2, 2),
            "the zoomed shadow reaches further"
        );

        parameters.shadow_zoom = 1.0;
        parameters.shadow_blur = 1.0;
        let mut blurred = source.clone();
        apply(&mut blurred, width, height, parameters);
        let softened = pixel(&blurred, width, 2, 2);
        assert_ne!(
            softened,
            pixel(&source, width, 2, 2),
            "the blur tail is wide"
        );
        assert!(
            softened[0] > 0.0 && softened[0] < 1.0,
            "the blurred edge is a partial shadow: {softened:?}"
        );
    }

    #[test]
    fn a_transparent_frame_still_receives_the_window() {
        let width = 8;
        let height = 8;
        let mut frame = vec![[0.0; 4]; width * height];
        let mut parameters = PictureInPictureParameters::default();
        parameters.width = 0.5;
        parameters.height = 0.5;
        parameters.anchor = PictureInPictureAnchor::Centre;
        parameters.rounding = 0.0;
        parameters.anti_alias = 0.0;
        parameters.shadow = 0.0;
        parameters.fill = 1.0;
        parameters.fill_matching = 0.0;
        parameters.fill_red = 0.25;
        parameters.fill_green = 0.5;
        parameters.fill_blue = 0.75;
        apply(&mut frame, width, height, parameters);
        assert_pixel_close(
            pixel(&frame, width, 4, 4),
            [0.25, 0.5, 0.75, 1.0],
            1.0e-6,
            "the fill paints the transparent source",
        );
        assert_eq!(
            pixel(&frame, width, 0, 0),
            [0.0; 4],
            "the outside stays transparent"
        );
    }

    #[test]
    fn uncovered_pixels_keep_their_input_bits() {
        let width = 24;
        let height = 16;
        let mut frame = gradient(width, height);
        let input = frame.clone();
        let mut parameters = PictureInPictureParameters::default();
        parameters.shadow = 0.0;
        apply(&mut frame, width, height, parameters);
        assert_eq!(
            pixel(&frame, width, 20, 14),
            pixel(&input, width, 20, 14),
            "the far corner is untouched bit for bit"
        );
        assert_ne!(
            pixel(&frame, width, 2, 1),
            pixel(&input, width, 2, 1),
            "the window repaints its interior"
        );
    }

    #[test]
    fn transparent_input_normalises_to_transparent_black() {
        let width = 4;
        let height = 4;
        let mut frame = vec![[0.8, 0.6, 0.4, 0.0]; width * height];
        let mut parameters = PictureInPictureParameters::default();
        parameters.width = 0.5;
        parameters.height = 0.5;
        parameters.anchor = PictureInPictureAnchor::Centre;
        parameters.shadow = 0.0;
        parameters.border = 0.0;
        parameters.fill = 0.0;
        apply(&mut frame, width, height, parameters);
        for value in &frame {
            assert_eq!(*value, [0.0; 4], "zero coverage normalises to zero");
        }
    }

    #[test]
    fn degenerate_and_inconsistent_frames_are_rejected() {
        let parameters = PictureInPictureParameters::default();
        let grade = grade(parameters);
        let mut frame = gradient(4, 4);
        assert!(!apply_picture_in_picture_f32(&mut frame, 0, 4, &grade));
        assert!(!apply_picture_in_picture_f32(&mut frame, 4, 0, &grade));
        assert!(!apply_picture_in_picture_f32(&mut [], 0, 0, &grade));
        assert!(
            !apply_picture_in_picture_f32(&mut frame, 5, 4, &grade),
            "a short buffer is rejected"
        );
        assert!(
            !apply_picture_in_picture_f32(&mut frame, 4, 5, &grade),
            "a short buffer is rejected"
        );
    }

    #[test]
    fn a_one_by_one_frame_is_processed() {
        let mut frame = vec![[0.5, 0.25, 0.75, 1.0]];
        let mut parameters = PictureInPictureParameters::default();
        parameters.width = 1.0;
        parameters.height = 1.0;
        parameters.anchor = PictureInPictureAnchor::Centre;
        apply(&mut frame, 1, 1, parameters);
        assert_pixel_close(frame[0], [0.5, 0.25, 0.75, 1.0], 1.0e-6, "one pixel");
    }

    #[test]
    fn checkpoint_can_interrupt_the_kernel() {
        #[derive(Debug, PartialEq, Eq)]
        struct Interrupted;
        let mut frame = gradient(8, 8);
        let grade = grade(PictureInPictureParameters::default());
        let mut calls = 0;
        let result = apply_picture_in_picture_controlled(&mut frame, 8, 8, &grade, &mut || {
            calls += 1;
            if calls > 1 {
                Err(Interrupted)
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Err(Interrupted));
    }

    #[test]
    fn hash_covers_every_authored_control() {
        let default = grade(PictureInPictureParameters::default());
        assert_eq!(hash_of(&default), hash_of(&default), "hashing is stable");
        let controls = [
            "content",
            "scale",
            "pan_x",
            "pan_y",
            "anti_alias",
            "anchor",
            "lock_aspect",
            "width",
            "height",
            "aspect_ratio",
            "style",
            "rounding",
            "rotation",
            "opacity",
            "border",
            "border_width",
            "border_red",
            "border_green",
            "border_blue",
            "border_opacity",
            "fill_matching",
            "fill",
            "fill_red",
            "fill_green",
            "fill_blue",
            "fill_opacity",
            "shadow",
            "shadow_strength",
            "shadow_red",
            "shadow_green",
            "shadow_blue",
            "shadow_angle",
            "shadow_distance",
            "shadow_zoom",
            "shadow_blur",
        ];
        for control in controls {
            let changed = grade(authored_parameters(control));
            assert_ne!(
                hash_of(&default),
                hash_of(&changed),
                "{control} participates in the hash"
            );
        }
    }
}
