//! Page turn: lift the frame like a sheet of paper and roll it over the fold.
//!
//! The effect partitions the frame along an authored fold line. Everything past
//! the cylinder is peeled away, the cylinder itself shows the back of the sheet,
//! and the strip folded over it shows the flat back. The roll and the flat back
//! are both lit by a single directional light, and the back is painted with the
//! authored paper colour, so the result reads as paper rather than as a mirrored
//! copy of the frame.

use std::hash::{Hash, Hasher};

/// Fold line runs along the free direction chosen by the classic UI.
pub const PAGE_TURN_DEFAULT_FOLD_DIRECTION: f32 = -60.0;
/// Cylinder radius in pixels.
pub const PAGE_TURN_DEFAULT_FOLD_RADIUS: f32 = 50.0;
/// Direction of the light that shades the back of the sheet.
pub const PAGE_TURN_DEFAULT_LIGHT_DIRECTION: f32 = -60.0;
/// Fold point, as a fraction of the frame.
pub const PAGE_TURN_DEFAULT_FOLD_POSITION: [f32; 2] = [0.5, 0.5];
/// Back page index. Kept for parity with the host; the back is always the paper.
pub const PAGE_TURN_DEFAULT_BACK_PAGE: f32 = 6.0;
/// Opacity of the back of the sheet, in percent.
pub const PAGE_TURN_DEFAULT_BACK_OPACITY: f32 = 85.0;
/// Colour painted on the back of the sheet.
pub const PAGE_TURN_DEFAULT_PAPER_COLOUR: [f32; 3] = [0.77, 0.77, 0.71];
/// Smallest fold point coordinate, as a fraction of the frame.
pub const PAGE_TURN_MIN_FOLD_POSITION: f32 = -1.0;
/// Largest fold point coordinate, as a fraction of the frame.
pub const PAGE_TURN_MAX_FOLD_POSITION: f32 = 2.0;
/// Smallest authored fold direction, in degrees.
pub const PAGE_TURN_MIN_FOLD_DIRECTION: f32 = -180.0;
/// Largest authored fold direction, in degrees.
pub const PAGE_TURN_MAX_FOLD_DIRECTION: f32 = 180.0;
/// Smallest cylinder radius, in pixels.
pub const PAGE_TURN_MIN_FOLD_RADIUS: f32 = 4.0;
/// Largest cylinder radius, in pixels.
pub const PAGE_TURN_MAX_FOLD_RADIUS: f32 = 1000.0;
/// Smallest light direction, in degrees.
pub const PAGE_TURN_MIN_LIGHT_DIRECTION: f32 = -180.0;
/// Largest light direction, in degrees.
pub const PAGE_TURN_MAX_LIGHT_DIRECTION: f32 = 180.0;
/// Smallest back page index.
pub const PAGE_TURN_MIN_BACK_PAGE: f32 = 0.0;
/// Largest back page index.
pub const PAGE_TURN_MAX_BACK_PAGE: f32 = 1000.0;
/// Smallest back opacity, in percent.
pub const PAGE_TURN_MIN_BACK_OPACITY: f32 = 0.0;
/// Largest back opacity, in percent.
pub const PAGE_TURN_MAX_BACK_OPACITY: f32 = 100.0;

/// Ambient share of the back lighting.
const PAGE_TURN_AMBIENT: f32 = 0.55;
/// Diffuse share of the back lighting.
const PAGE_TURN_DIFFUSE: f32 = 0.45;
/// Height component of the light vector.
const PAGE_TURN_LIGHT_HEIGHT: f32 = 0.5;
/// Converts the authored back opacity from percent to a unit interval.
const PAGE_TURN_PERCENT_SCALE: f32 = 0.01;
/// Half of a pixel, used to map authored fractions onto pixel centres.
const PAGE_TURN_HALF_PIXEL: f32 = 0.5;

/// Corner the sheet is lifted from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageTurnControls {
    /// Free fold line, steered by the fold direction.
    Classic,
    /// Lift from the top left corner.
    TopLeft,
    /// Lift from the top right corner.
    TopRight,
    /// Lift from the bottom left corner.
    BottomLeft,
    /// Lift from the bottom right corner.
    #[default]
    BottomRight,
}

impl PageTurnControls {
    /// Every real menu entry.
    pub const ALL: [Self; 5] = [
        Self::Classic,
        Self::TopLeft,
        Self::TopRight,
        Self::BottomLeft,
        Self::BottomRight,
    ];

    /// Menu entry used by the documented defaults.
    pub const DEFAULT: Self = Self::BottomRight;

    /// Stable key stored in project files.
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::TopLeft => "top_left",
            Self::TopRight => "top_right",
            Self::BottomLeft => "bottom_left",
            Self::BottomRight => "bottom_right",
        }
    }

    /// Localized menu caption.
    pub fn label(self) -> &'static str {
        match self {
            Self::Classic => "经典界面",
            Self::TopLeft => "左上角",
            Self::TopRight => "右上角",
            Self::BottomLeft => "左下角",
            Self::BottomRight => "右下角",
        }
    }

    /// Parses a stored key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.as_key() == key)
    }

    /// Numeric tag handed to the GPU pass.
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Classic => 2,
            Self::TopLeft => 3,
            Self::TopRight => 4,
            Self::BottomLeft => 5,
            Self::BottomRight => 6,
        }
    }
}

/// Which side of the sheet is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageTurnRender {
    /// Draw the folded back over the page that stays behind.
    #[default]
    FrontAndBack,
    /// Draw only the back of the sheet.
    BackPage,
    /// Draw only the page that stays behind.
    FrontPage,
}

impl PageTurnRender {
    /// Every menu entry.
    pub const ALL: [Self; 3] = [Self::FrontAndBack, Self::BackPage, Self::FrontPage];

    /// Menu entry used by the documented defaults.
    pub const DEFAULT: Self = Self::FrontAndBack;

    /// Stable key stored in project files.
    pub fn as_key(self) -> &'static str {
        match self {
            Self::FrontAndBack => "front_and_back",
            Self::BackPage => "back_page",
            Self::FrontPage => "front_page",
        }
    }

    /// Localized menu caption.
    pub fn label(self) -> &'static str {
        match self {
            Self::FrontAndBack => "正面与背面",
            Self::BackPage => "背面",
            Self::FrontPage => "正面",
        }
    }

    /// Parses a stored key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.as_key() == key)
    }

    /// Numeric tag handed to the GPU pass.
    pub fn as_u8(self) -> u8 {
        match self {
            Self::FrontAndBack => 1,
            Self::BackPage => 2,
            Self::FrontPage => 3,
        }
    }
}

/// Authored values of one page turn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageTurnParameters {
    /// Corner the sheet is lifted from.
    pub controls: PageTurnControls,
    /// Fold point, as a fraction of the frame.
    pub fold_position: [f32; 2],
    /// Fold line direction for the classic UI, in degrees.
    pub fold_direction: f32,
    /// Cylinder radius in pixels.
    pub fold_radius: f32,
    /// Direction of the light that shades the back, in degrees.
    pub light_direction: f32,
    /// Which side of the sheet is drawn.
    pub render: PageTurnRender,
    /// Back page index. The back is always the paper colour.
    pub back_page: f32,
    /// Opacity of the back of the sheet, in percent.
    pub back_opacity: f32,
    /// Colour painted on the back of the sheet.
    pub paper_colour: [f32; 3],
}

impl Default for PageTurnParameters {
    fn default() -> Self {
        Self {
            controls: PageTurnControls::DEFAULT,
            fold_position: PAGE_TURN_DEFAULT_FOLD_POSITION,
            fold_direction: PAGE_TURN_DEFAULT_FOLD_DIRECTION,
            fold_radius: PAGE_TURN_DEFAULT_FOLD_RADIUS,
            light_direction: PAGE_TURN_DEFAULT_LIGHT_DIRECTION,
            render: PageTurnRender::DEFAULT,
            back_page: PAGE_TURN_DEFAULT_BACK_PAGE,
            back_opacity: PAGE_TURN_DEFAULT_BACK_OPACITY,
            paper_colour: PAGE_TURN_DEFAULT_PAPER_COLOUR,
        }
    }
}

/// Rejected authored value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("page_turn.{parameter} must be finite and within its documented range")]
pub struct PageTurnError {
    /// Name of the offending parameter.
    pub parameter: &'static str,
}

impl PageTurnError {
    /// Name of the offending parameter.
    pub fn parameter(self) -> &'static str {
        self.parameter
    }
}

/// Four float cells handed to the spatial pass. The recorder prepends the frame
/// size, so the shader sees five cells in total.
pub type PageTurnGpuControls = ([f32; 4], [f32; 4], [f32; 4], [f32; 4]);

/// A validated page turn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageTurnGrade {
    parameters: PageTurnParameters,
}

fn check(
    value: f32,
    lower: f32,
    upper: f32,
    parameter: &'static str,
) -> Result<f32, PageTurnError> {
    if value.is_finite() && (lower..=upper).contains(&value) {
        Ok(value)
    } else {
        Err(PageTurnError { parameter })
    }
}

impl PageTurnGrade {
    /// Validates the authored values.
    pub fn new(parameters: PageTurnParameters) -> Result<Self, PageTurnError> {
        check(
            parameters.fold_position[0],
            PAGE_TURN_MIN_FOLD_POSITION,
            PAGE_TURN_MAX_FOLD_POSITION,
            "fold_position",
        )?;
        check(
            parameters.fold_position[1],
            PAGE_TURN_MIN_FOLD_POSITION,
            PAGE_TURN_MAX_FOLD_POSITION,
            "fold_position",
        )?;
        check(
            parameters.fold_direction,
            PAGE_TURN_MIN_FOLD_DIRECTION,
            PAGE_TURN_MAX_FOLD_DIRECTION,
            "fold_direction",
        )?;
        check(
            parameters.fold_radius,
            PAGE_TURN_MIN_FOLD_RADIUS,
            PAGE_TURN_MAX_FOLD_RADIUS,
            "fold_radius",
        )?;
        check(
            parameters.light_direction,
            PAGE_TURN_MIN_LIGHT_DIRECTION,
            PAGE_TURN_MAX_LIGHT_DIRECTION,
            "light_direction",
        )?;
        check(
            parameters.back_page,
            PAGE_TURN_MIN_BACK_PAGE,
            PAGE_TURN_MAX_BACK_PAGE,
            "back_page",
        )?;
        check(
            parameters.back_opacity,
            PAGE_TURN_MIN_BACK_OPACITY,
            PAGE_TURN_MAX_BACK_OPACITY,
            "back_opacity",
        )?;
        for channel in parameters.paper_colour {
            check(channel, 0.0, 1.0, "paper_colour")?;
        }
        Ok(Self { parameters })
    }

    /// Grade built from the documented defaults.
    pub fn documented_defaults() -> Self {
        Self { parameters: PageTurnParameters::default() }
    }

    /// Authored values.
    pub fn parameters(self) -> PageTurnParameters {
        self.parameters
    }

    /// A page turn always changes the frame: whether the sheet leaves the frame
    /// depends on the frame size, which the grade cannot see.
    pub fn is_identity(self) -> bool {
        false
    }

    /// Relative cost of the pass.
    pub fn cost(self) -> u8 {
        3
    }

    /// Reach of the pass, in pixels. The fold can reach any pixel, so the plane
    /// is never tiled.
    pub fn input_halo(self) -> (u32, u32) {
        (u32::MAX, u32::MAX)
    }

    /// Fold line normal in frame space.
    ///
    /// Corner modes pin the fold to the corner's diagonal; the classic UI turns
    /// the line with the authored direction.
    pub fn fold_normal(self) -> [f32; 2] {
        let parameters = self.parameters;
        match parameters.controls {
            PageTurnControls::Classic => {
                let radians = parameters.fold_direction.to_radians();
                [-radians.sin(), radians.cos()]
            }
            PageTurnControls::TopLeft => [
                -std::f32::consts::FRAC_1_SQRT_2,
                -std::f32::consts::FRAC_1_SQRT_2,
            ],
            PageTurnControls::TopRight => [
                std::f32::consts::FRAC_1_SQRT_2,
                -std::f32::consts::FRAC_1_SQRT_2,
            ],
            PageTurnControls::BottomLeft => [
                -std::f32::consts::FRAC_1_SQRT_2,
                std::f32::consts::FRAC_1_SQRT_2,
            ],
            PageTurnControls::BottomRight => [
                std::f32::consts::FRAC_1_SQRT_2,
                std::f32::consts::FRAC_1_SQRT_2,
            ],
        }
    }

    /// Fold point in pixel-index space.
    pub fn fold_point(self, width: f32, height: f32) -> [f32; 2] {
        [
            self.parameters.fold_position[0] * width - PAGE_TURN_HALF_PIXEL,
            self.parameters.fold_position[1] * height - PAGE_TURN_HALF_PIXEL,
        ]
    }

    /// Length of the lifted side, in pixels. Zero when the sheet stays flat.
    pub fn lifted_span(self, width: f32, height: f32) -> f32 {
        if width <= 0.0 || height <= 0.0 {
            return 0.0;
        }
        let point = self.fold_point(width, height);
        let normal = self.fold_normal();
        let corners = [
            [0.0, 0.0],
            [width - 1.0, 0.0],
            [0.0, height - 1.0],
            [width - 1.0, height - 1.0],
        ];
        corners
            .into_iter()
            .map(|corner| (corner[0] - point[0]) * normal[0] + (corner[1] - point[1]) * normal[1])
            .fold(0.0_f32, f32::max)
    }

    /// Packs the values the GPU pass needs.
    ///
    /// `params` carries the fold point and normal, `extra` the cylinder radius,
    /// the lifted span and the light direction, `aces` the paper colour and the
    /// back opacity, and `extra1` the render tag and the light height. The
    /// recorder prepends the frame size.
    pub fn gpu_controls(self, width: f32, height: f32) -> PageTurnGpuControls {
        let point = self.fold_point(width, height);
        let normal = self.fold_normal();
        let radians = self.parameters.light_direction.to_radians();
        (
            [point[0], point[1], normal[0], normal[1]],
            [
                self.parameters.fold_radius,
                self.lifted_span(width, height),
                radians.cos(),
                radians.sin(),
            ],
            [
                self.parameters.paper_colour[0],
                self.parameters.paper_colour[1],
                self.parameters.paper_colour[2],
                self.parameters.back_opacity * PAGE_TURN_PERCENT_SCALE,
            ],
            [
                self.parameters.render.as_u8() as f32,
                PAGE_TURN_LIGHT_HEIGHT,
                0.0,
                0.0,
            ],
        )
    }

    /// Folds every authored value into an operation hash.
    pub fn hash_state<H: Hasher>(self, state: &mut H) {
        let parameters = self.parameters;
        parameters.controls.as_u8().hash(state);
        parameters.fold_position[0].to_bits().hash(state);
        parameters.fold_position[1].to_bits().hash(state);
        parameters.fold_direction.to_bits().hash(state);
        parameters.fold_radius.to_bits().hash(state);
        parameters.light_direction.to_bits().hash(state);
        parameters.render.as_u8().hash(state);
        parameters.back_page.to_bits().hash(state);
        parameters.back_opacity.to_bits().hash(state);
        parameters.paper_colour[0].to_bits().hash(state);
        parameters.paper_colour[1].to_bits().hash(state);
        parameters.paper_colour[2].to_bits().hash(state);
    }
}

/// Bilinear tap on the premultiplied frame, clamped to the frame.
fn sample_premultiplied(
    frame: &[[f32; 4]],
    width: usize,
    height: usize,
    x: f32,
    y: f32,
) -> [f32; 4] {
    let last_x = (width - 1) as f32;
    let last_y = (height - 1) as f32;
    let x = x.clamp(0.0, last_x);
    let y = y.clamp(0.0, last_y);
    let x0 = x.floor();
    let y0 = y.floor();
    let x1 = (x0 + 1.0).min(last_x);
    let y1 = (y0 + 1.0).min(last_y);
    let fx = x - x0;
    let fy = y - y0;
    let columns = [x0 as usize, x1 as usize];
    let rows = [y0 as usize, y1 as usize];
    let weights = [
        (1.0 - fx) * (1.0 - fy),
        fx * (1.0 - fy),
        (1.0 - fx) * fy,
        fx * fy,
    ];
    let mut sample = [0.0_f32; 4];
    for (index, weight) in weights.into_iter().enumerate() {
        let cell = rows[index / 2] * width + columns[index % 2];
        for channel in 0..4 {
            sample[channel] += frame[cell][channel] * weight;
        }
    }
    crate::coverage::straight_rgba_from_premultiplied(sample)
}

/// Height component of the light vector, normalized together with its frame
/// direction by [`light_vector`].
///
/// Lifts the frame like a sheet of paper and rolls it over the fold.
pub fn page_turn_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: PageTurnGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_page_turn_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Page turn with an explicit cancellation checkpoint.
///
/// Returns `Ok(false)` when the frame cannot be executed so the caller can
/// report an explicit unsupported-op error.
pub(crate) fn apply_page_turn_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: PageTurnGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }

    let frame_width = width as f32;
    let frame_height = height as f32;
    let lifted = grade.lifted_span(frame_width, frame_height);
    if lifted <= 0.0 {
        return Ok(true);
    }

    let frame = &mut working[..pixel_count];
    let source = frame.to_vec();
    let mut premultiplied = source.clone();
    crate::straight_alpha::premultiply_rgba_controlled(&mut premultiplied, checkpoint)?;

    let parameters = grade.parameters();
    let point = grade.fold_point(frame_width, frame_height);
    let normal = grade.fold_normal();
    // The fold line runs perpendicular to its normal.
    let along = [normal[1], -normal[0]];
    let radius = parameters.fold_radius;
    let roll_span = std::f32::consts::PI * radius;
    let flat_span = (lifted - roll_span).max(0.0);
    let light_radians = parameters.light_direction.to_radians();
    let light = [
        light_radians.cos(),
        light_radians.sin(),
        PAGE_TURN_LIGHT_HEIGHT,
    ];
    let light_length = (light[0] * light[0] + light[1] * light[1] + light[2] * light[2]).sqrt();
    let light = [
        light[0] / light_length,
        light[1] / light_length,
        light[2] / light_length,
    ];
    let normal_light = normal[0] * light[0] + normal[1] * light[1];
    let paper = parameters.paper_colour;
    let back_opacity = parameters.back_opacity * PAGE_TURN_PERCENT_SCALE;
    let render_back = parameters.render != PageTurnRender::FrontPage;
    let render_front = parameters.render != PageTurnRender::BackPage;
    checkpoint()?;

    let rows = pixel_count / width;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + offset;
            let pixel_y = row as f32;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let pixel_x = column as f32;
                let dx = pixel_x - point[0];
                let dy = pixel_y - point[1];
                let distance = dx * normal[0] + dy * normal[1];
                let cell = row * width + column;
                if distance > radius {
                    // Past the cylinder: the sheet has lifted off the frame.
                    *pixel = [0.0; 4];
                    continue;
                }
                let back = if distance > 0.0 {
                    let ratio = (distance / radius).clamp(0.0, 1.0);
                    let angle = std::f32::consts::PI - ratio.asin();
                    let source_distance = (radius * angle).min(lifted);
                    let dot = angle.sin() * normal_light - angle.cos() * light[2];
                    Some((source_distance, dot))
                } else if flat_span > 0.0 && distance >= -flat_span {
                    Some(((roll_span - distance).min(lifted), light[2]))
                } else {
                    None
                };
                match back {
                    Some((source_distance, dot)) if render_back => {
                        let shade = PAGE_TURN_AMBIENT + PAGE_TURN_DIFFUSE * dot.max(0.0);
                        let along_distance = dx * along[0] + dy * along[1];
                        let sample_x =
                            point[0] + along_distance * along[0] - source_distance * normal[0];
                        let sample_y =
                            point[1] + along_distance * along[1] - source_distance * normal[1];
                        let sample =
                            sample_premultiplied(&premultiplied, width, height, sample_x, sample_y);
                        *pixel = [
                            paper[0] * shade,
                            paper[1] * shade,
                            paper[2] * shade,
                            sample[3] * back_opacity,
                        ];
                    }
                    Some(_) => {
                        *pixel = [0.0; 4];
                    }
                    None if render_front => {
                        *pixel = source[cell];
                    }
                    None => {
                        *pixel = [0.0; 4];
                    }
                }
            }
        }
    });

    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(parameters: PageTurnParameters) -> PageTurnGrade {
        PageTurnGrade::new(parameters).expect("the parameters must be accepted")
    }

    fn white_frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        vec![[1.0, 1.0, 1.0, 1.0]; width * height]
    }

    #[test]
    fn documented_defaults_are_the_authored_defaults() {
        let grade = PageTurnGrade::documented_defaults();
        let parameters = grade.parameters();
        assert_eq!(parameters.controls, PageTurnControls::BottomRight);
        assert_eq!(parameters.fold_position, [0.5, 0.5]);
        assert_eq!(parameters.fold_direction, -60.0);
        assert_eq!(parameters.fold_radius, 50.0);
        assert_eq!(parameters.light_direction, -60.0);
        assert_eq!(parameters.render, PageTurnRender::FrontAndBack);
        assert_eq!(parameters.back_page, 6.0);
        assert_eq!(parameters.back_opacity, 85.0);
        assert_eq!(parameters.paper_colour, [0.77, 0.77, 0.71]);
    }

    #[test]
    fn corner_controls_pin_the_fold_normal() {
        let base = PageTurnParameters::default();
        let normals = [
            (PageTurnControls::TopLeft, [-0.70710677, -0.70710677]),
            (PageTurnControls::TopRight, [0.70710677, -0.70710677]),
            (PageTurnControls::BottomLeft, [-0.70710677, 0.70710677]),
            (PageTurnControls::BottomRight, [0.70710677, 0.70710677]),
        ];
        for (controls, expected) in normals {
            let grade = valid(PageTurnParameters { controls, ..base });
            let normal = grade.fold_normal();
            assert!((normal[0] - expected[0]).abs() < 1e-6);
            assert!((normal[1] - expected[1]).abs() < 1e-6);
        }
    }

    #[test]
    fn classic_fold_turns_the_line_with_the_direction() {
        let grade = valid(PageTurnParameters {
            controls: PageTurnControls::Classic,
            fold_direction: 90.0,
            ..PageTurnParameters::default()
        });
        let normal = grade.fold_normal();
        assert!((normal[0] + 1.0).abs() < 1e-6);
        assert!(normal[1].abs() < 1e-6);
    }

    #[test]
    fn lifted_span_reaches_the_far_corner() {
        let grade = PageTurnGrade::documented_defaults();
        let span = grade.lifted_span(100.0, 100.0);
        assert!((span - 70.003_6).abs() < 0.01, "span was {span}");
    }

    #[test]
    fn lifted_span_is_zero_when_the_fold_sits_outside() {
        let grade = valid(PageTurnParameters {
            fold_position: [1.5, 1.5],
            ..PageTurnParameters::default()
        });
        assert_eq!(grade.lifted_span(100.0, 100.0), 0.0);
    }

    #[test]
    fn rejects_a_fold_radius_outside_its_range() {
        let error = PageTurnGrade::new(PageTurnParameters {
            fold_radius: 3.0,
            ..PageTurnParameters::default()
        })
        .expect_err("the radius is too small");
        assert_eq!(error.parameter(), "fold_radius");
    }

    #[test]
    fn rejects_a_back_opacity_outside_its_range() {
        let error = PageTurnGrade::new(PageTurnParameters {
            back_opacity: 120.0,
            ..PageTurnParameters::default()
        })
        .expect_err("the opacity is too large");
        assert_eq!(error.parameter(), "back_opacity");
    }

    #[test]
    fn rejects_a_paper_colour_outside_the_unit_interval() {
        let error = PageTurnGrade::new(PageTurnParameters {
            paper_colour: [1.2, 0.5, 0.5],
            ..PageTurnParameters::default()
        })
        .expect_err("the colour is too bright");
        assert_eq!(error.parameter(), "paper_colour");
    }

    #[test]
    fn rejects_a_fold_position_outside_its_range() {
        let error = PageTurnGrade::new(PageTurnParameters {
            fold_position: [0.5, 2.5],
            ..PageTurnParameters::default()
        })
        .expect_err("the fold point is too far");
        assert_eq!(error.parameter(), "fold_position");
    }

    #[test]
    fn rejects_a_non_finite_direction() {
        let error = PageTurnGrade::new(PageTurnParameters {
            light_direction: f32::NAN,
            ..PageTurnParameters::default()
        })
        .expect_err("the direction is not finite");
        assert_eq!(error.parameter(), "light_direction");
    }

    #[test]
    fn controls_keys_round_trip() {
        for controls in PageTurnControls::ALL {
            assert_eq!(
                PageTurnControls::from_key(controls.as_key()),
                Some(controls)
            );
            assert!(!controls.label().is_empty());
        }
    }

    #[test]
    fn render_keys_round_trip() {
        for render in PageTurnRender::ALL {
            assert_eq!(PageTurnRender::from_key(render.as_key()), Some(render));
            assert!(!render.label().is_empty());
        }
    }

    #[test]
    fn unknown_control_key_is_rejected() {
        assert_eq!(PageTurnControls::from_key("diagonal"), None);
        assert_eq!(PageTurnRender::from_key("both"), None);
    }

    #[test]
    fn gpu_controls_carry_the_fold_and_the_light() {
        let grade = PageTurnGrade::documented_defaults();
        let (params, extra, aces, extra1) = grade.gpu_controls(100.0, 100.0);
        assert!((params[0] - 49.5).abs() < 1e-6);
        assert!((params[1] - 49.5).abs() < 1e-6);
        assert!((params[2] - 0.707_106_77).abs() < 1e-6);
        assert!((params[3] - 0.707_106_77).abs() < 1e-6);
        assert_eq!(extra[0], 50.0);
        assert!((extra[1] - 70.003_6).abs() < 0.01);
        assert!((extra[2] - 0.5).abs() < 1e-6);
        assert!((extra[3] + 0.866_025_4).abs() < 1e-6);
        assert_eq!(aces[..3], [0.77, 0.77, 0.71]);
        assert!((aces[3] - 0.85).abs() < 1e-6);
        assert_eq!(extra1[0], 1.0);
        assert_eq!(extra1[1], PAGE_TURN_LIGHT_HEIGHT);
    }

    #[test]
    fn hash_changes_with_the_fold_direction() {
        use std::collections::hash_map::DefaultHasher;
        let base = PageTurnGrade::documented_defaults();
        let turned = valid(PageTurnParameters {
            fold_direction: 30.0,
            ..PageTurnParameters::default()
        });
        let mut left = DefaultHasher::new();
        base.hash_state(&mut left);
        let mut right = DefaultHasher::new();
        turned.hash_state(&mut right);
        assert_ne!(left.finish(), right.finish());
    }

    #[test]
    fn hash_is_stable_for_equal_grades() {
        use std::collections::hash_map::DefaultHasher;
        let mut left = DefaultHasher::new();
        PageTurnGrade::documented_defaults().hash_state(&mut left);
        let mut right = DefaultHasher::new();
        PageTurnGrade::documented_defaults().hash_state(&mut right);
        assert_eq!(left.finish(), right.finish());
    }

    #[test]
    fn frame_f32_reports_unusable_geometry() {
        let mut frame: Vec<[f32; 4]> = Vec::new();
        assert!(!page_turn_frame_f32(
            &mut frame,
            0,
            4,
            PageTurnGrade::documented_defaults()
        ));
    }

    #[test]
    fn a_flat_sheet_leaves_the_frame_untouched() {
        let mut frame = white_frame(12, 8);
        let before = frame.clone();
        let grade = valid(PageTurnParameters {
            fold_position: [1.5, 1.5],
            ..PageTurnParameters::default()
        });
        assert!(page_turn_frame_f32(&mut frame, 12, 8, grade));
        assert_eq!(frame, before);
    }

    #[test]
    fn the_peeled_corner_becomes_transparent() {
        let mut frame = white_frame(200, 150);
        assert!(page_turn_frame_f32(
            &mut frame,
            200,
            150,
            PageTurnGrade::documented_defaults()
        ));
        let corner = frame[149 * 200 + 199];
        assert_eq!(corner, [0.0; 4]);
    }

    #[test]
    fn the_roll_band_paints_the_back_with_the_paper_colour() {
        let mut frame = white_frame(200, 150);
        assert!(page_turn_frame_f32(
            &mut frame,
            200,
            150,
            PageTurnGrade::documented_defaults()
        ));
        let pixel = frame[92 * 200 + 117];
        assert!((pixel[3] - 0.85).abs() < 1e-5, "alpha was {}", pixel[3]);
        assert!(pixel[0] > 0.3 && pixel[0] < 0.77, "red was {}", pixel[0]);
        assert_eq!(pixel[0], pixel[1]);
        assert!(pixel[2] <= pixel[0]);
    }

    #[test]
    fn the_flat_back_is_brighter_than_the_roll() {
        let mut frame = white_frame(400, 300);
        assert!(page_turn_frame_f32(
            &mut frame,
            400,
            300,
            PageTurnGrade::documented_defaults()
        ));
        let roll = frame[167 * 400 + 217];
        let flat = frame[128 * 400 + 178];
        assert!(flat[3] > 0.8, "flat alpha was {}", flat[3]);
        assert!(roll[3] > 0.8, "roll alpha was {}", roll[3]);
        assert!(flat[0] > roll[0], "flat {} vs roll {}", flat[0], roll[0]);
    }

    #[test]
    fn the_front_page_hides_the_back() {
        let mut frame = white_frame(200, 150);
        let grade = valid(PageTurnParameters {
            render: PageTurnRender::FrontPage,
            ..PageTurnParameters::default()
        });
        assert!(page_turn_frame_f32(&mut frame, 200, 150, grade));
        assert_eq!(frame[92 * 200 + 117][3], 0.0);
    }

    #[test]
    fn the_back_page_hides_the_kept_front() {
        let mut frame = white_frame(200, 150);
        let grade = valid(PageTurnParameters {
            render: PageTurnRender::BackPage,
            ..PageTurnParameters::default()
        });
        assert!(page_turn_frame_f32(&mut frame, 200, 150, grade));
        assert_eq!(frame[0][3], 0.0);
        assert!((frame[92 * 200 + 117][3] - 0.85).abs() < 1e-5);
    }

    #[test]
    fn zero_back_opacity_clears_the_back() {
        let mut frame = white_frame(200, 150);
        let grade =
            valid(PageTurnParameters { back_opacity: 0.0, ..PageTurnParameters::default() });
        assert!(page_turn_frame_f32(&mut frame, 200, 150, grade));
        assert_eq!(frame[92 * 200 + 117][3], 0.0);
    }

    #[test]
    fn a_transparent_source_keeps_the_back_transparent() {
        let mut frame = vec![[0.2, 0.3, 0.4, 0.0]; 200 * 150];
        assert!(page_turn_frame_f32(
            &mut frame,
            200,
            150,
            PageTurnGrade::documented_defaults()
        ));
        assert_eq!(frame[92 * 200 + 117][3], 0.0);
    }

    #[test]
    fn the_paper_colour_reaches_the_back() {
        let mut frame = white_frame(200, 150);
        let grade = valid(PageTurnParameters {
            paper_colour: [1.0, 0.0, 0.0],
            ..PageTurnParameters::default()
        });
        assert!(page_turn_frame_f32(&mut frame, 200, 150, grade));
        let pixel = frame[92 * 200 + 117];
        assert!(pixel[0] > 0.5);
        assert_eq!(pixel[1], 0.0);
    }

    #[test]
    fn cost_and_halo_describe_the_pass() {
        let grade = PageTurnGrade::documented_defaults();
        assert_eq!(grade.cost(), 3);
        assert_eq!(grade.input_halo(), (u32::MAX, u32::MAX));
        assert!(!grade.is_identity());
    }
}
