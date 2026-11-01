//! Matte refine (MSK-01 遮罩精修): refines the alpha layer a frame already carries.
//!
//! The stage is self-contained: it cleans the colour fringe of a soft edge, grows
//! or shrinks the matte, smooths it, feathers it, and finally recombines the
//! optionally cleaned source with the refined matte. The alpha is refined on a
//! matte plane that copies the value into every channel with `a = 1`, exactly
//! like keylight, so every morphology and blur pass reads and writes only `.x`.
//!
//! Stages, in graph order:
//!
//! 1. **edge colour cleanup** — every pixel with `0 < a < 1` takes the straight
//!    colour of the strongest of eight taps at `other_edge_radius` and mixes it
//!    in by `clean_amount / 100`.
//! 2. **smooth** — a per-axis box average of the matte whose radius falls to
//!    `smooth * (1 - edge_detail)` where the alpha gradient is strong. It runs
//!    before the shift when `extend_smooth` is on and after it otherwise.
//! 3. **shift** — separable max (grow) and min (shrink) filters. Growing runs
//!    before shrinking, and both clamp at the frame edge.
//! 4. **feather** — a separable box blur over the matte.
//! 5. **final** — contrast on the matte plus the two view modes, folded into the
//!    composite so no extra pass is needed.
//!
//! All arithmetic is f32 and the working frame stays straight-alpha. The box
//! blur used by the feather premultiplies once and unpremultiplies once, which
//! is a no-op on the matte plane because `a == 1`; the cleanup mixes straight
//! colours because that is the documented fringe-repair semantics.
//!
//! # Uniform packing
//!
//! | pass | `params` | `extra` | `extra1` |
//! | --- | --- | --- | --- |
//! | cleanup | `[radius, clean_amount/100, 0, 0]` | `[0; 4]` | `[0; 4]` |
//! | smooth (per axis) | `[smooth, axis, edge_detail, read_frame_alpha]` | `[0; 4]` | `[0; 4]` |
//! | morphology (per axis) | `[radius, axis, mode, read_frame_alpha]` | `[0; 4]` | `[0; 4]` |
//! | feather (per axis) | `[0, axis, whole, fraction]` | `[normalization, 1, 0, 0]` | `[read_frame_alpha, 0, 0, 0]` |
//! | composite | `[view_map, view_edge_region, contrast, read_frame_alpha]` | `[0; 4]` | `[0; 4]` |
//!
//! `axis` is `0.0` for horizontal and `1.0` for vertical, `mode` is `0.0` for min
//! (shrink) and `1.0` for max (grow), and `read_frame_alpha` is `1.0` only for
//! the first matte pass, which reads the frame's `.a` instead of a matte `.x`.
//! That flag is what removes the separate extract pass from the plan, so
//! [`MatteRefineGrade::pass_count`] is exactly the number of GPU passes.
//!
//! # Reserved parameters
//!
//! `tremor_reduction`, `tremor_amount`, `more_motion_blur`, `per_frame_samples`
//! and `shutter_angle_degrees` are validated and carried through the grade, but
//! they do not change the image. They are reserved for the temporal sampling the
//! design brief leaves open, exactly like the transform shutter parameters.

use std::fmt;

use crate::box_blur::{box_blur_frame_f32, BoxBlurDirection};

/// A checkpoint is offered at least every this many pixels.
const CHECKPOINT_STRIDE: usize = 4096;

/// The eight cleanup taps, clockwise from the left.
const CLEANUP_TAPS: [(isize, isize); 8] = [
    (-1, 0),
    (1, 0),
    (0, -1),
    (0, 1),
    (-1, -1),
    (1, -1),
    (-1, 1),
    (1, 1),
];

/// Largest authored edge detail.
pub const MATTE_REFINE_AUTHOR_MAX_EDGE_DETAIL: f32 = 1.0;
/// Largest authored other-edge tap radius in pixels.
pub const MATTE_REFINE_AUTHOR_MAX_OTHER_EDGE_RADIUS_PIXELS: f32 = 250.0;
/// Largest authored view amount.
pub const MATTE_REFINE_AUTHOR_MAX_VIEW: f32 = 1.0;
/// Largest authored smooth radius in pixels.
pub const MATTE_REFINE_AUTHOR_MAX_SMOOTH_PIXELS: f32 = 50.0;
/// Largest authored feather radius in pixels.
pub const MATTE_REFINE_AUTHOR_MAX_FEATHER_PIXELS: f32 = 1000.0;
/// Largest authored contrast amount.
pub const MATTE_REFINE_AUTHOR_MAX_CONTRAST: f32 = 100.0;
/// Largest authored edge shift in pixels, in either direction.
pub const MATTE_REFINE_AUTHOR_MAX_SHIFT_EDGE_PIXELS: f32 = 100.0;
/// Largest authored edge colour cleanup amount.
pub const MATTE_REFINE_AUTHOR_MAX_CLEAN_AMOUNT: f32 = 100.0;
/// Largest authored expand radius in pixels.
pub const MATTE_REFINE_AUTHOR_MAX_EXPAND_RADIUS_PIXELS: f32 = 80.0;
/// Largest reserved tremor reduction.
pub const MATTE_REFINE_AUTHOR_MAX_TREMOR_REDUCTION: f32 = 4.0;
/// Largest reserved tremor amount.
pub const MATTE_REFINE_AUTHOR_MAX_TREMOR_AMOUNT: f32 = 400.0;
/// Largest reserved extra motion blur amount.
pub const MATTE_REFINE_AUTHOR_MAX_MORE_MOTION_BLUR: f32 = 1.0;
/// Largest reserved per-frame sample count.
pub const MATTE_REFINE_AUTHOR_MAX_PER_FRAME_SAMPLES: f32 = 128.0;
/// Largest reserved shutter angle in degrees.
pub const MATTE_REFINE_AUTHOR_MAX_SHUTTER_ANGLE_DEGREES: f32 = 720.0;

/// Documented default edge detail.
pub const MATTE_REFINE_DEFAULT_EDGE_DETAIL: f32 = 1.0;
/// Documented default other-edge tap radius in pixels.
pub const MATTE_REFINE_DEFAULT_OTHER_EDGE_RADIUS_PIXELS: f32 = 10.0;
/// Documented default edge-region view amount: off.
pub const MATTE_REFINE_DEFAULT_VIEW_EDGE_REGION: f32 = 0.0;
/// Documented default smooth radius in pixels.
pub const MATTE_REFINE_DEFAULT_SMOOTH_PIXELS: f32 = 0.0;
/// Documented default feather radius in pixels.
pub const MATTE_REFINE_DEFAULT_FEATHER_PIXELS: f32 = 0.0;
/// Documented default contrast amount.
pub const MATTE_REFINE_DEFAULT_CONTRAST: f32 = 0.0;
/// Documented default edge shift in pixels.
pub const MATTE_REFINE_DEFAULT_SHIFT_EDGE_PIXELS: f32 = 0.0;
/// Documented default for the better-quality toggle.
pub const MATTE_REFINE_DEFAULT_BETTER_QUALITY: bool = false;
/// Documented default for the edge colour cleanup toggle.
pub const MATTE_REFINE_DEFAULT_CLEAN_EDGE_COLOR: bool = false;
/// Documented default edge colour cleanup amount.
pub const MATTE_REFINE_DEFAULT_CLEAN_AMOUNT: f32 = 100.0;
/// Documented default for the extend-smooth toggle.
pub const MATTE_REFINE_DEFAULT_EXTEND_SMOOTH: bool = false;
/// Documented default expand radius in pixels.
pub const MATTE_REFINE_DEFAULT_EXPAND_RADIUS_PIXELS: f32 = 0.0;
/// Documented default matte-map view amount: off.
pub const MATTE_REFINE_DEFAULT_VIEW_MAP: f32 = 0.0;
/// Documented default reserved tremor reduction.
pub const MATTE_REFINE_DEFAULT_TREMOR_REDUCTION: f32 = 1.0;
/// Documented default reserved tremor amount.
pub const MATTE_REFINE_DEFAULT_TREMOR_AMOUNT: f32 = 50.0;
/// Documented default reserved extra motion blur amount.
pub const MATTE_REFINE_DEFAULT_MORE_MOTION_BLUR: f32 = 0.0;
/// Documented default reserved per-frame sample count.
pub const MATTE_REFINE_DEFAULT_PER_FRAME_SAMPLES: f32 = 8.0;
/// Documented default reserved shutter angle in degrees.
pub const MATTE_REFINE_DEFAULT_SHUTTER_ANGLE_DEGREES: f32 = 180.0;

/// The rejection reason of one [`MatteRefineGrade`] field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MatteRefineError {
    /// One authored parameter is not finite or outside its documented range.
    OutOfRange {
        /// The parameter name as it appears in the checklist.
        parameter: &'static str,
        /// The rejected value.
        value: f32,
        /// The smallest accepted value.
        minimum: f32,
        /// The largest accepted value.
        maximum: f32,
    },
}

impl fmt::Display for MatteRefineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfRange {
                parameter,
                value,
                minimum,
                maximum,
            } => write!(
                formatter,
                "matte refine {parameter} {value} must be a finite value inside [{minimum}, {maximum}]"
            ),
        }
    }
}

impl std::error::Error for MatteRefineError {}

/// Accepts one authored value only when it is finite and inside its range.
fn checked(
    parameter: &'static str,
    value: f32,
    minimum: f32,
    maximum: f32,
) -> Result<f32, MatteRefineError> {
    if value.is_finite() && (minimum..=maximum).contains(&value) {
        Ok(value)
    } else {
        Err(MatteRefineError::OutOfRange { parameter, value, minimum, maximum })
    }
}

/// The authored parameters of one matte refine operation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatteRefineGrade {
    edge_detail: f32,
    other_edge_radius: f32,
    view_edge_region: f32,
    smooth: f32,
    feather: f32,
    contrast: f32,
    shift_edge: f32,
    better_quality: bool,
    clean_edge_color: bool,
    clean_amount: f32,
    extend_smooth: bool,
    expand_radius: f32,
    view_map: f32,
    tremor_reduction: f32,
    tremor_amount: f32,
    more_motion_blur: f32,
    per_frame_samples: f32,
    shutter_angle_degrees: f32,
}

impl MatteRefineGrade {
    /// Builds a grade from authored parameters.
    ///
    /// # Errors
    ///
    /// Returns the [`MatteRefineError`] of the first field outside its
    /// documented range.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        edge_detail: f32,
        other_edge_radius: f32,
        view_edge_region: f32,
        smooth: f32,
        feather: f32,
        contrast: f32,
        shift_edge: f32,
        better_quality: bool,
        clean_edge_color: bool,
        clean_amount: f32,
        extend_smooth: bool,
        expand_radius: f32,
        view_map: f32,
        tremor_reduction: f32,
        tremor_amount: f32,
        more_motion_blur: f32,
        per_frame_samples: f32,
        shutter_angle_degrees: f32,
    ) -> Result<Self, MatteRefineError> {
        Ok(Self {
            edge_detail: checked(
                "edge detail",
                edge_detail,
                0.0,
                MATTE_REFINE_AUTHOR_MAX_EDGE_DETAIL,
            )?,
            other_edge_radius: checked(
                "other edge radius",
                other_edge_radius,
                0.0,
                MATTE_REFINE_AUTHOR_MAX_OTHER_EDGE_RADIUS_PIXELS,
            )?,
            view_edge_region: checked(
                "view edge region",
                view_edge_region,
                0.0,
                MATTE_REFINE_AUTHOR_MAX_VIEW,
            )?,
            smooth: checked("smooth", smooth, 0.0, MATTE_REFINE_AUTHOR_MAX_SMOOTH_PIXELS)?,
            feather: checked(
                "feather",
                feather,
                0.0,
                MATTE_REFINE_AUTHOR_MAX_FEATHER_PIXELS,
            )?,
            contrast: checked("contrast", contrast, 0.0, MATTE_REFINE_AUTHOR_MAX_CONTRAST)?,
            shift_edge: checked(
                "shift edge",
                shift_edge,
                -MATTE_REFINE_AUTHOR_MAX_SHIFT_EDGE_PIXELS,
                MATTE_REFINE_AUTHOR_MAX_SHIFT_EDGE_PIXELS,
            )?,
            better_quality,
            clean_edge_color,
            clean_amount: checked(
                "clean amount",
                clean_amount,
                0.0,
                MATTE_REFINE_AUTHOR_MAX_CLEAN_AMOUNT,
            )?,
            extend_smooth,
            expand_radius: checked(
                "expand radius",
                expand_radius,
                0.0,
                MATTE_REFINE_AUTHOR_MAX_EXPAND_RADIUS_PIXELS,
            )?,
            view_map: checked("view map", view_map, 0.0, MATTE_REFINE_AUTHOR_MAX_VIEW)?,
            tremor_reduction: checked(
                "tremor reduction",
                tremor_reduction,
                1.0,
                MATTE_REFINE_AUTHOR_MAX_TREMOR_REDUCTION,
            )?,
            tremor_amount: checked(
                "tremor amount",
                tremor_amount,
                0.0,
                MATTE_REFINE_AUTHOR_MAX_TREMOR_AMOUNT,
            )?,
            more_motion_blur: checked(
                "more motion blur",
                more_motion_blur,
                0.0,
                MATTE_REFINE_AUTHOR_MAX_MORE_MOTION_BLUR,
            )?,
            per_frame_samples: checked(
                "per frame samples",
                per_frame_samples,
                2.0,
                MATTE_REFINE_AUTHOR_MAX_PER_FRAME_SAMPLES,
            )?,
            shutter_angle_degrees: checked(
                "shutter angle",
                shutter_angle_degrees,
                1.0,
                MATTE_REFINE_AUTHOR_MAX_SHUTTER_ANGLE_DEGREES,
            )?,
        })
    }

    /// The documented defaults.
    ///
    /// # Errors
    ///
    /// The defaults are all inside their ranges, so this never fails; the result
    /// keeps the same shape as [`Self::new`].
    pub fn documented_defaults() -> Result<Self, MatteRefineError> {
        Self::new(
            MATTE_REFINE_DEFAULT_EDGE_DETAIL,
            MATTE_REFINE_DEFAULT_OTHER_EDGE_RADIUS_PIXELS,
            MATTE_REFINE_DEFAULT_VIEW_EDGE_REGION,
            MATTE_REFINE_DEFAULT_SMOOTH_PIXELS,
            MATTE_REFINE_DEFAULT_FEATHER_PIXELS,
            MATTE_REFINE_DEFAULT_CONTRAST,
            MATTE_REFINE_DEFAULT_SHIFT_EDGE_PIXELS,
            MATTE_REFINE_DEFAULT_BETTER_QUALITY,
            MATTE_REFINE_DEFAULT_CLEAN_EDGE_COLOR,
            MATTE_REFINE_DEFAULT_CLEAN_AMOUNT,
            MATTE_REFINE_DEFAULT_EXTEND_SMOOTH,
            MATTE_REFINE_DEFAULT_EXPAND_RADIUS_PIXELS,
            MATTE_REFINE_DEFAULT_VIEW_MAP,
            MATTE_REFINE_DEFAULT_TREMOR_REDUCTION,
            MATTE_REFINE_DEFAULT_TREMOR_AMOUNT,
            MATTE_REFINE_DEFAULT_MORE_MOTION_BLUR,
            MATTE_REFINE_DEFAULT_PER_FRAME_SAMPLES,
            MATTE_REFINE_DEFAULT_SHUTTER_ANGLE_DEGREES,
        )
    }

    /// The authored edge detail.
    #[must_use]
    pub const fn edge_detail(&self) -> f32 {
        self.edge_detail
    }

    /// The authored other-edge tap radius in pixels.
    #[must_use]
    pub const fn other_edge_radius(&self) -> f32 {
        self.other_edge_radius
    }

    /// The authored view-edge-region amount.
    #[must_use]
    pub const fn view_edge_region(&self) -> f32 {
        self.view_edge_region
    }

    /// The authored smooth radius in pixels.
    #[must_use]
    pub const fn smooth(&self) -> f32 {
        self.smooth
    }

    /// The authored feather radius in pixels.
    #[must_use]
    pub const fn feather(&self) -> f32 {
        self.feather
    }

    /// The authored contrast amount.
    #[must_use]
    pub const fn contrast(&self) -> f32 {
        self.contrast
    }

    /// The authored edge shift in pixels.
    #[must_use]
    pub const fn shift_edge(&self) -> f32 {
        self.shift_edge
    }

    /// Whether the better-quality toggle is on.
    #[must_use]
    pub const fn better_quality(&self) -> bool {
        self.better_quality
    }

    /// Whether the edge colour cleanup is on.
    #[must_use]
    pub const fn clean_edge_color(&self) -> bool {
        self.clean_edge_color
    }

    /// The authored edge colour cleanup amount.
    #[must_use]
    pub const fn clean_amount(&self) -> f32 {
        self.clean_amount
    }

    /// Whether smoothing runs before the shift.
    #[must_use]
    pub const fn extend_smooth(&self) -> bool {
        self.extend_smooth
    }

    /// The authored expand radius in pixels.
    #[must_use]
    pub const fn expand_radius(&self) -> f32 {
        self.expand_radius
    }

    /// The authored view-map amount.
    #[must_use]
    pub const fn view_map(&self) -> f32 {
        self.view_map
    }

    /// The reserved tremor reduction.
    #[must_use]
    pub const fn tremor_reduction(&self) -> f32 {
        self.tremor_reduction
    }

    /// The reserved tremor amount.
    #[must_use]
    pub const fn tremor_amount(&self) -> f32 {
        self.tremor_amount
    }

    /// The reserved extra motion blur amount.
    #[must_use]
    pub const fn more_motion_blur(&self) -> f32 {
        self.more_motion_blur
    }

    /// The reserved per-frame sample count.
    #[must_use]
    pub const fn per_frame_samples(&self) -> f32 {
        self.per_frame_samples
    }

    /// The reserved shutter angle in degrees.
    #[must_use]
    pub const fn shutter_angle_degrees(&self) -> f32 {
        self.shutter_angle_degrees
    }

    /// The grow radius: the positive shift plus the expand radius.
    #[must_use]
    pub fn grow_radius(&self) -> f32 {
        self.shift_edge.max(0.0) + self.expand_radius
    }

    /// The shrink radius: the negative shift.
    #[must_use]
    pub fn shrink_radius(&self) -> f32 {
        (-self.shift_edge).max(0.0)
    }

    /// The smooth radius rounded to a whole number of pixels.
    #[must_use]
    pub fn smooth_radius(&self) -> u32 {
        self.smooth.round() as u32
    }

    /// How many times each blur axis runs: the quality toggle doubles it.
    #[must_use]
    pub fn blur_iterations(&self) -> u32 {
        if self.better_quality {
            2
        } else {
            1
        }
    }

    /// Whether the matte grows before the final composite.
    #[must_use]
    pub fn grows(&self) -> bool {
        self.grow_radius().round() >= 1.0
    }

    /// Whether the matte shrinks before the final composite.
    #[must_use]
    pub fn shrinks(&self) -> bool {
        self.shrink_radius().round() >= 1.0
    }

    /// Whether the matte is smoothed.
    #[must_use]
    pub fn smooths(&self) -> bool {
        self.smooth_radius() >= 1
    }

    /// Whether the matte is feathered.
    #[must_use]
    pub fn feathers(&self) -> bool {
        self.feather > 0.0
    }

    /// Whether the edge colour cleanup pass runs.
    #[must_use]
    pub fn cleans_edge_color(&self) -> bool {
        self.clean_edge_color && self.clean_amount > 0.0
    }

    /// Whether the edge-region view is active.
    #[must_use]
    pub fn views_edge_region(&self) -> bool {
        self.view_edge_region > 0.0
    }

    /// Whether the matte view is active.
    #[must_use]
    pub fn views_map(&self) -> bool {
        self.view_map > 0.0
    }

    /// Every authored value is at its documented neutral default.
    ///
    /// The reserved temporal parameters do not take part: changing one of them
    /// cannot change the image, so the builder elides the stage.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.edge_detail == MATTE_REFINE_DEFAULT_EDGE_DETAIL
            && self.other_edge_radius == MATTE_REFINE_DEFAULT_OTHER_EDGE_RADIUS_PIXELS
            && self.view_edge_region == MATTE_REFINE_DEFAULT_VIEW_EDGE_REGION
            && self.smooth == MATTE_REFINE_DEFAULT_SMOOTH_PIXELS
            && self.feather == MATTE_REFINE_DEFAULT_FEATHER_PIXELS
            && self.contrast == MATTE_REFINE_DEFAULT_CONTRAST
            && self.shift_edge == MATTE_REFINE_DEFAULT_SHIFT_EDGE_PIXELS
            && self.better_quality == MATTE_REFINE_DEFAULT_BETTER_QUALITY
            && self.clean_edge_color == MATTE_REFINE_DEFAULT_CLEAN_EDGE_COLOR
            && self.clean_amount == MATTE_REFINE_DEFAULT_CLEAN_AMOUNT
            && self.extend_smooth == MATTE_REFINE_DEFAULT_EXTEND_SMOOTH
            && self.expand_radius == MATTE_REFINE_DEFAULT_EXPAND_RADIUS_PIXELS
            && self.view_map == MATTE_REFINE_DEFAULT_VIEW_MAP
    }

    /// The number of GPU passes the stage records.
    ///
    /// This is the single source of truth for the renderer's
    /// `texture_pass_count`, so the scratch pool and the kernel cannot drift.
    #[must_use]
    pub fn pass_count(&self) -> u32 {
        let mut count = 1; // final composite
        if self.cleans_edge_color() {
            count += 1;
        }
        if self.smooths() {
            count += 2 * self.blur_iterations();
        }
        if self.grows() {
            count += 2;
        }
        if self.shrinks() {
            count += 2;
        }
        if self.feathers() {
            count += 2 * self.blur_iterations();
        }
        count
    }

    /// The cleanup pass uniforms: `[radius, clean_amount/100, 0, 0]`.
    #[must_use]
    pub fn cleanup_gpu_controls(&self) -> [f32; 4] {
        [
            self.other_edge_radius.round(),
            self.clean_amount / 100.0,
            0.0,
            0.0,
        ]
    }

    /// The smooth pass uniforms: `[smooth, axis, edge_detail, read_frame_alpha]`.
    #[must_use]
    pub fn smooth_gpu_controls(&self, axis: f32, read_frame_alpha: bool) -> [f32; 4] {
        [
            self.smooth_radius() as f32,
            axis,
            self.edge_detail,
            f32::from(u8::from(read_frame_alpha)),
        ]
    }

    /// The morphology pass uniforms:
    /// `[radius, axis, mode, read_frame_alpha]`.
    #[must_use]
    pub fn morphology_gpu_controls(
        &self,
        radius: f32,
        axis: f32,
        dilate: bool,
        read_frame_alpha: bool,
    ) -> [f32; 4] {
        [
            radius.round(),
            axis,
            f32::from(u8::from(dilate)),
            f32::from(u8::from(read_frame_alpha)),
        ]
    }

    /// The feather pass uniforms:
    /// `([0, axis, whole, fraction], [normalization, 1, 0, 0], [read, 0, 0, 0])`.
    #[must_use]
    pub fn feather_gpu_controls(
        &self,
        axis: f32,
        whole_radius: u32,
        fraction: f32,
        normalization: f32,
        read_frame_alpha: bool,
    ) -> ([f32; 4], [f32; 4], [f32; 4]) {
        (
            [0.0, axis, whole_radius as f32, fraction],
            [normalization, 1.0, 0.0, 0.0],
            [f32::from(u8::from(read_frame_alpha)), 0.0, 0.0, 0.0],
        )
    }

    /// The composite uniforms:
    /// `([view_map, view_edge_region, contrast, read_frame_alpha], [0; 4])`.
    #[must_use]
    pub fn composite_gpu_controls(&self, read_frame_alpha: bool) -> ([f32; 4], [f32; 4]) {
        (
            [
                f32::from(u8::from(self.views_map())),
                f32::from(u8::from(self.views_edge_region())),
                self.contrast,
                f32::from(u8::from(read_frame_alpha)),
            ],
            [0.0; 4],
        )
    }

    /// The final composite of one pixel.
    ///
    /// `colour` is the optionally cleaned straight-alpha source, `matte` the
    /// refined alpha, and `edge_strength` the alpha gradient of the refined
    /// matte. The two view modes win over the normal output; the map view wins
    /// over the edge-region view when both are on.
    #[must_use]
    pub fn composite(&self, colour: [f32; 4], matte: f32, edge_strength: f32) -> [f32; 4] {
        let mut alpha = matte;
        if self.contrast > 0.0 {
            let factor = 1.0 + self.contrast / 50.0;
            alpha = ((alpha - 0.5) * factor + 0.5).clamp(0.0, 1.0);
        }
        if self.views_map() {
            [alpha, alpha, alpha, 1.0]
        } else if self.views_edge_region() {
            [colour[0], colour[1], colour[2], edge_strength]
        } else {
            [colour[0], colour[1], colour[2], alpha]
        }
    }

    /// Hashes the authored state with the same field order the graph uses.
    pub(crate) fn hash_state<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash;
        for value in [
            self.edge_detail,
            self.other_edge_radius,
            self.view_edge_region,
            self.smooth,
            self.feather,
            self.contrast,
            self.shift_edge,
            self.clean_amount,
            self.expand_radius,
            self.view_map,
            self.tremor_reduction,
            self.tremor_amount,
            self.more_motion_blur,
            self.per_frame_samples,
            self.shutter_angle_degrees,
        ] {
            value.to_bits().hash(state);
        }
        self.better_quality.hash(state);
        self.clean_edge_color.hash(state);
        self.extend_smooth.hash(state);
    }
}

/// Apply matte refine to one straight-alpha rgba f32 frame.
///
/// Returns `false` only when the frame is empty or cannot be addressed; the
/// grade is assumed to be validated already.
pub fn matte_refine_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &MatteRefineGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_matte_refine_f32_controlled(pixels, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

/// The checkpointed kernel behind [`matte_refine_frame_f32`].
pub(crate) fn apply_matte_refine_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &MatteRefineGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    let source: Vec<[f32; 4]> = working[..pixel_count].to_vec();
    checkpoint()?;

    // Stage 1: the optional edge colour cleanup, over a copy of the source. The
    // taps read the untouched source so the result does not depend on scan
    // order, exactly like the GPU pass that reads the bound input texture.
    let cleaned = if grade.cleans_edge_color() {
        Some(clean_edge_colours(
            &source,
            width,
            height,
            grade.other_edge_radius().round() as usize,
            grade.clean_amount() / 100.0,
            checkpoint,
        )?)
    } else {
        None
    };
    checkpoint()?;
    let colour_source: &[[f32; 4]] = cleaned.as_deref().unwrap_or(&source);

    // Stage 2: the matte plane. The alpha is copied into every channel with
    // `a = 1`, so every later filter reads and writes `.x` only.
    let mut matte = Vec::with_capacity(pixel_count);
    for (index, pixel) in source.iter().enumerate() {
        if index % CHECKPOINT_STRIDE == 0 {
            checkpoint()?;
        }
        matte.push([pixel[3], pixel[3], pixel[3], 1.0]);
    }
    checkpoint()?;
    let mut spare = vec![[0.0_f32; 4]; pixel_count];
    let iterations = grade.blur_iterations();

    // Stage 3: an optional smooth, before the shift when `extend_smooth` is on.
    if grade.smooths() && grade.extend_smooth() {
        smooth_matte(
            &mut matte,
            &mut spare,
            width,
            height,
            grade.smooth_radius(),
            grade.edge_detail(),
            iterations,
            checkpoint,
        )?;
        checkpoint()?;
    }

    // Stage 4: grow, then shrink, both separable and edge-clamped.
    if grade.grows() {
        morphology_matte(
            &mut matte,
            width,
            height,
            grade.grow_radius().round() as usize,
            true,
        );
        checkpoint()?;
    }
    if grade.shrinks() {
        morphology_matte(
            &mut matte,
            width,
            height,
            grade.shrink_radius().round() as usize,
            false,
        );
        checkpoint()?;
    }

    // Stage 5: the smooth that runs after the shift.
    if grade.smooths() && !grade.extend_smooth() {
        smooth_matte(
            &mut matte,
            &mut spare,
            width,
            height,
            grade.smooth_radius(),
            grade.edge_detail(),
            iterations,
            checkpoint,
        )?;
        checkpoint()?;
    }

    // Stage 6: the feather. The box kernel premultiplies and unpremultiplies
    // once, which is a no-op on the matte plane because every `a` is 1.
    if grade.feathers()
        && !box_blur_frame_f32(
            &mut matte,
            width,
            height,
            grade.feather(),
            iterations,
            BoxBlurDirection::HorizontalAndVertical,
            true,
        )
    {
        return Ok(false);
    }
    checkpoint()?;

    // Stage 7: the final composite, over row bands like every other spatial
    // kernel.
    let frame = &mut working[..pixel_count];
    let rows = pixel_count / width;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (row_offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + row_offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let index = row * width + column;
                let strength = matte_edge_strength(&matte, width, height, row, column);
                *pixel = grade.composite(colour_source[index], matte[index][0], strength);
            }
        }
    });
    checkpoint()?;
    Ok(true)
}

/// Replaces the colour fringe of every soft pixel with the strongest nearby
/// straight colour.
fn clean_edge_colours<E>(
    source: &[[f32; 4]],
    width: usize,
    height: usize,
    radius: usize,
    amount: f32,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<Vec<[f32; 4]>, E> {
    let mut cleaned = source.to_vec();
    for row in 0..height {
        for column in 0..width {
            let index = row * width + column;
            if index.is_multiple_of(CHECKPOINT_STRIDE) {
                checkpoint()?;
            }
            let alpha = source[index][3];
            if alpha <= 0.0 || alpha >= 1.0 {
                continue;
            }
            let mut best_alpha = -1.0_f32;
            let mut best_colour = [source[index][0], source[index][1], source[index][2]];
            for (delta_row, delta_column) in CLEANUP_TAPS {
                let sample_row = (row as isize + delta_row * radius as isize)
                    .clamp(0, height as isize - 1) as usize;
                let sample_column = (column as isize + delta_column * radius as isize)
                    .clamp(0, width as isize - 1) as usize;
                let tap = source[sample_row * width + sample_column];
                if tap[3] > best_alpha {
                    best_alpha = tap[3];
                    best_colour = [tap[0], tap[1], tap[2]];
                }
            }
            if best_alpha <= 0.0 {
                continue;
            }
            cleaned[index] = [
                source[index][0] + (best_colour[0] - source[index][0]) * amount,
                source[index][1] + (best_colour[1] - source[index][1]) * amount,
                source[index][2] + (best_colour[2] - source[index][2]) * amount,
                alpha,
            ];
        }
    }
    Ok(cleaned)
}

/// Smooths the matte plane with a per-pixel box radius.
///
/// The radius shrinks toward `max_radius * (1 - edge_detail)` where the alpha
/// gradient is strong, which keeps detail on the edge while wide flat regions
/// get the full smoothing.
fn smooth_matte<E>(
    matte: &mut [[f32; 4]],
    spare: &mut [[f32; 4]],
    width: usize,
    height: usize,
    max_radius: u32,
    edge_detail: f32,
    iterations: u32,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    let edge_detail = edge_detail.clamp(0.0, 1.0);
    for _ in 0..iterations {
        smooth_axis(
            matte,
            spare,
            width,
            height,
            max_radius,
            edge_detail,
            SmoothAxis::Horizontal,
        );
        checkpoint()?;
        smooth_axis(
            spare,
            matte,
            width,
            height,
            max_radius,
            edge_detail,
            SmoothAxis::Vertical,
        );
        checkpoint()?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum SmoothAxis {
    Horizontal,
    Vertical,
}

/// One separable smooth pass.
fn smooth_axis(
    source: &[[f32; 4]],
    destination: &mut [[f32; 4]],
    width: usize,
    height: usize,
    max_radius: u32,
    edge_detail: f32,
    axis: SmoothAxis,
) {
    let max_radius = max_radius as f32;
    for row in 0..height {
        for column in 0..width {
            let strength = matte_edge_strength(source, width, height, row, column);
            let radius = (max_radius * (1.0 - edge_detail * strength))
                .round()
                .clamp(0.0, max_radius) as isize;
            let mut sum = 0.0_f32;
            let mut count = 0.0_f32;
            for offset in -radius..=radius {
                let (sample_row, sample_column) = match axis {
                    SmoothAxis::Horizontal => (
                        row,
                        (column as isize + offset).clamp(0, width as isize - 1) as usize,
                    ),
                    SmoothAxis::Vertical => (
                        (row as isize + offset).clamp(0, height as isize - 1) as usize,
                        column,
                    ),
                };
                sum += source[sample_row * width + sample_column][0];
                count += 1.0;
            }
            let value = sum / count;
            destination[row * width + column] = [value, value, value, 1.0];
        }
    }
}

/// The alpha gradient strength of one matte pixel, in `[0, 1]`.
fn matte_edge_strength(
    matte: &[[f32; 4]],
    width: usize,
    height: usize,
    row: usize,
    column: usize,
) -> f32 {
    let left = matte[row * width + column.saturating_sub(1)][0];
    let right = matte[row * width + (column + 1).min(width - 1)][0];
    let up = matte[row.saturating_sub(1) * width + column][0];
    let down = matte[(row + 1).min(height - 1) * width + column][0];
    let horizontal = 0.5 * (right - left);
    let vertical = 0.5 * (down - up);
    (2.0 * (horizontal.abs() + vertical.abs())).clamp(0.0, 1.0)
}

/// Grows (`dilate`) or shrinks the matte with two separable filter passes.
///
/// The horizontal pass writes a scratch plane and the vertical pass moves it
/// back, exactly like the GPU ping-pong; both axes clamp at the frame edge.
fn morphology_matte(
    matte: &mut [[f32; 4]],
    width: usize,
    height: usize,
    radius: usize,
    dilate: bool,
) {
    if radius == 0 {
        return;
    }
    let mut horizontal = vec![[0.0_f32; 4]; width * height];
    for row in 0..height {
        for column in 0..width {
            let mut value = if dilate { 0.0_f32 } else { 1.0_f32 };
            for offset in 0..=(2 * radius) {
                let sample = column as isize + offset as isize - radius as isize;
                let clamped = sample.clamp(0, width as isize - 1) as usize;
                let candidate = matte[row * width + clamped][0];
                value = if dilate {
                    value.max(candidate)
                } else {
                    value.min(candidate)
                };
            }
            horizontal[row * width + column] = [value, value, value, 1.0];
        }
    }
    for column in 0..width {
        for row in 0..height {
            let mut value = if dilate { 0.0_f32 } else { 1.0_f32 };
            for offset in 0..=(2 * radius) {
                let sample = row as isize + offset as isize - radius as isize;
                let clamped = sample.clamp(0, height as isize - 1) as usize;
                let candidate = horizontal[clamped * width + column][0];
                value = if dilate {
                    value.max(candidate)
                } else {
                    value.min(candidate)
                };
            }
            matte[row * width + column] = [value, value, value, 1.0];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> MatteRefineGrade {
        MatteRefineGrade::documented_defaults().expect("documented defaults are valid")
    }

    fn frame(width: usize, height: usize, colour: [f32; 3]) -> Vec<[f32; 4]> {
        vec![[colour[0], colour[1], colour[2], 1.0]; width * height]
    }

    fn step(width: usize, height: usize, opaque_columns: usize) -> Vec<[f32; 4]> {
        let mut pixels = frame(width, height, [0.2, 0.4, 0.6]);
        for row in 0..height {
            for column in 0..width {
                pixels[row * width + column][3] = if column < opaque_columns { 1.0 } else { 0.0 };
            }
        }
        pixels
    }

    fn island(
        width: usize,
        height: usize,
        first_column: usize,
        last_column: usize,
    ) -> Vec<[f32; 4]> {
        let mut pixels = frame(width, height, [0.2, 0.4, 0.6]);
        for row in 0..height {
            for column in first_column..=last_column {
                pixels[row * width + column][3] = 1.0;
            }
            for column in 0..width {
                if !(first_column..=last_column).contains(&column) {
                    pixels[row * width + column][3] = 0.0;
                }
            }
        }
        pixels
    }

    fn alpha(pixels: &[[f32; 4]], width: usize, column: usize, row: usize) -> f32 {
        pixels[row * width + column][3]
    }

    fn close(actual: f32, expected: f32) -> bool {
        (actual - expected).abs() <= 1.0e-6
    }

    #[test]
    fn documented_defaults_are_neutral() {
        let grade = defaults();
        assert_eq!(grade.edge_detail(), 1.0);
        assert_eq!(grade.other_edge_radius(), 10.0);
        assert_eq!(grade.view_edge_region(), 0.0);
        assert_eq!(grade.smooth(), 0.0);
        assert_eq!(grade.feather(), 0.0);
        assert_eq!(grade.contrast(), 0.0);
        assert_eq!(grade.shift_edge(), 0.0);
        assert!(!grade.better_quality());
        assert!(!grade.clean_edge_color());
        assert_eq!(grade.clean_amount(), 100.0);
        assert!(!grade.extend_smooth());
        assert_eq!(grade.expand_radius(), 0.0);
        assert_eq!(grade.view_map(), 0.0);
        assert_eq!(grade.tremor_reduction(), 1.0);
        assert_eq!(grade.tremor_amount(), 50.0);
        assert_eq!(grade.more_motion_blur(), 0.0);
        assert_eq!(grade.per_frame_samples(), 8.0);
        assert_eq!(grade.shutter_angle_degrees(), 180.0);
        assert!(grade.is_identity());
        assert_eq!(grade.pass_count(), 1);
    }

    #[test]
    fn out_of_range_parameters_are_rejected() {
        for (label, update) in [
            (
                "edge detail",
                MatteRefineGrade { edge_detail: 1.5, ..defaults() },
            ),
            (
                "edge detail",
                MatteRefineGrade { edge_detail: -0.1, ..defaults() },
            ),
            (
                "other edge radius",
                MatteRefineGrade { other_edge_radius: 251.0, ..defaults() },
            ),
            (
                "view edge region",
                MatteRefineGrade { view_edge_region: 2.0, ..defaults() },
            ),
            ("smooth", MatteRefineGrade { smooth: 51.0, ..defaults() }),
            (
                "feather",
                MatteRefineGrade { feather: 1001.0, ..defaults() },
            ),
            (
                "contrast",
                MatteRefineGrade { contrast: 101.0, ..defaults() },
            ),
            (
                "shift edge",
                MatteRefineGrade { shift_edge: 101.0, ..defaults() },
            ),
            (
                "shift edge",
                MatteRefineGrade { shift_edge: -101.0, ..defaults() },
            ),
            (
                "clean amount",
                MatteRefineGrade { clean_amount: 101.0, ..defaults() },
            ),
            (
                "expand radius",
                MatteRefineGrade { expand_radius: 81.0, ..defaults() },
            ),
            ("view map", MatteRefineGrade { view_map: 1.5, ..defaults() }),
            (
                "tremor reduction",
                MatteRefineGrade { tremor_reduction: 0.5, ..defaults() },
            ),
            (
                "tremor amount",
                MatteRefineGrade { tremor_amount: 401.0, ..defaults() },
            ),
            (
                "more motion blur",
                MatteRefineGrade { more_motion_blur: 1.5, ..defaults() },
            ),
            (
                "per frame samples",
                MatteRefineGrade { per_frame_samples: 1.0, ..defaults() },
            ),
            (
                "shutter angle",
                MatteRefineGrade { shutter_angle_degrees: 721.0, ..defaults() },
            ),
        ] {
            let error = MatteRefineGrade::new(
                update.edge_detail,
                update.other_edge_radius,
                update.view_edge_region,
                update.smooth,
                update.feather,
                update.contrast,
                update.shift_edge,
                update.better_quality,
                update.clean_edge_color,
                update.clean_amount,
                update.extend_smooth,
                update.expand_radius,
                update.view_map,
                update.tremor_reduction,
                update.tremor_amount,
                update.more_motion_blur,
                update.per_frame_samples,
                update.shutter_angle_degrees,
            )
            .expect_err("out-of-range value must be rejected");
            assert!(matches!(
                error,
                MatteRefineError::OutOfRange { parameter, .. } if parameter == label
            ));
        }
    }

    #[test]
    fn non_finite_parameters_are_rejected() {
        let error = MatteRefineGrade::new(
            f32::NAN,
            10.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            false,
            false,
            100.0,
            false,
            0.0,
            0.0,
            1.0,
            50.0,
            0.0,
            8.0,
            180.0,
        )
        .expect_err("nan must be rejected");
        assert!(matches!(error, MatteRefineError::OutOfRange { .. }));
    }

    #[test]
    fn identity_leaves_the_frame_untouched() {
        let mut pixels = step(9, 3, 4);
        let before = pixels.clone();
        assert!(matte_refine_frame_f32(&mut pixels, 9, 3, &defaults()));
        assert_eq!(pixels, before);
    }

    #[test]
    fn feather_widens_the_soft_band() {
        let grade = MatteRefineGrade { feather: 1.0, ..defaults() };
        let mut pixels = step(9, 3, 5);
        assert!(matte_refine_frame_f32(&mut pixels, 9, 3, &grade));
        assert!(close(alpha(&pixels, 9, 3, 1), 1.0));
        assert!(close(alpha(&pixels, 9, 4, 1), 2.0 / 3.0));
        assert!(close(alpha(&pixels, 9, 5, 1), 1.0 / 3.0));
        assert!(close(alpha(&pixels, 9, 6, 1), 0.0));
    }

    #[test]
    fn shift_grows_the_matte() {
        let grade = MatteRefineGrade { shift_edge: 2.0, ..defaults() };
        let mut pixels = step(9, 3, 5);
        assert!(matte_refine_frame_f32(&mut pixels, 9, 3, &grade));
        assert!(close(alpha(&pixels, 9, 6, 1), 1.0));
        assert!(close(alpha(&pixels, 9, 7, 1), 0.0));
    }

    #[test]
    fn shift_shrinks_the_matte() {
        let grade = MatteRefineGrade { shift_edge: -2.0, ..defaults() };
        let mut pixels = step(9, 3, 5);
        assert!(matte_refine_frame_f32(&mut pixels, 9, 3, &grade));
        assert!(close(alpha(&pixels, 9, 2, 1), 1.0));
        assert!(close(alpha(&pixels, 9, 3, 1), 0.0));
    }

    #[test]
    fn expand_and_shift_add_up() {
        let shift_only = MatteRefineGrade { shift_edge: 2.0, ..defaults() };
        let mut with_shift = island(20, 3, 8, 11);
        assert!(matte_refine_frame_f32(&mut with_shift, 20, 3, &shift_only));
        assert!(close(alpha(&with_shift, 20, 6, 1), 1.0));
        assert!(close(alpha(&with_shift, 20, 5, 1), 0.0));

        let grown = MatteRefineGrade { shift_edge: 2.0, expand_radius: 3.0, ..defaults() };
        let mut with_expand = island(20, 3, 8, 11);
        assert!(matte_refine_frame_f32(&mut with_expand, 20, 3, &grown));
        assert!(close(alpha(&with_expand, 20, 3, 1), 1.0));
        assert!(close(alpha(&with_expand, 20, 2, 1), 0.0));
    }

    #[test]
    fn grow_runs_before_shrink() {
        let grow_first = MatteRefineGrade { shift_edge: -2.0, expand_radius: 4.0, ..defaults() };
        let mut pixels = island(20, 3, 8, 11);
        assert!(matte_refine_frame_f32(&mut pixels, 20, 3, &grow_first));
        // Growing by 4 and shrinking by 2 leaves the island four columns wide
        // again, moved two pixels to the left.
        assert!(close(alpha(&pixels, 20, 6, 1), 1.0));
        assert!(close(alpha(&pixels, 20, 5, 1), 0.0));
        assert!(close(alpha(&pixels, 20, 13, 1), 1.0));
        assert!(close(alpha(&pixels, 20, 14, 1), 0.0));
    }

    #[test]
    fn contrast_hardens_the_matte() {
        let mut pixels = frame(3, 1, [0.2, 0.4, 0.6]);
        pixels[1][3] = 0.4;
        let grade = MatteRefineGrade { contrast: 100.0, ..defaults() };
        assert!(matte_refine_frame_f32(&mut pixels, 3, 1, &grade));
        assert!(close(alpha(&pixels, 3, 1, 0), 0.2));
    }

    #[test]
    fn edge_colour_cleanup_replaces_the_dark_fringe() {
        let mut pixels = frame(9, 3, [0.1, 0.3, 0.1]);
        for row in 0..3 {
            for column in 0..9 {
                let index = row * 9 + column;
                pixels[index][3] = match column {
                    0..=4 => 1.0,
                    5 => 0.5,
                    _ => 0.0,
                };
            }
        }
        for row in 0..3 {
            for column in 0..=4 {
                pixels[row * 9 + column] = [1.0, 1.0, 1.0, 1.0];
            }
        }
        let grade = MatteRefineGrade {
            clean_edge_color: true,
            other_edge_radius: 2.0,
            ..defaults()
        };
        assert!(matte_refine_frame_f32(&mut pixels, 9, 3, &grade));
        let fringe = pixels[9 + 5];
        assert!(close(fringe[0], 1.0));
        assert!(close(fringe[1], 1.0));
        assert!(close(fringe[2], 1.0));
        assert!(close(fringe[3], 0.5));
        // Opaque and transparent pixels never change.
        assert!(close(pixels[2][0], 1.0));
        assert!(close(pixels[8][3], 0.0));
    }

    #[test]
    fn edge_colour_cleanup_mixes_by_amount() {
        let mut pixels = frame(9, 3, [0.1, 0.3, 0.1]);
        for row in 0..3 {
            for column in 0..9 {
                pixels[row * 9 + column][3] = match column {
                    0..=4 => 1.0,
                    5 => 0.5,
                    _ => 0.0,
                };
            }
        }
        for row in 0..3 {
            for column in 0..=4 {
                pixels[row * 9 + column] = [1.0, 1.0, 1.0, 1.0];
            }
        }
        let grade = MatteRefineGrade {
            clean_edge_color: true,
            other_edge_radius: 2.0,
            clean_amount: 50.0,
            ..defaults()
        };
        assert!(matte_refine_frame_f32(&mut pixels, 9, 3, &grade));
        assert!(close(pixels[9 + 5][0], 0.55));
    }

    #[test]
    fn view_map_shows_the_refined_alpha() {
        let grade = MatteRefineGrade { feather: 1.0, view_map: 1.0, ..defaults() };
        let mut pixels = step(9, 3, 5);
        assert!(matte_refine_frame_f32(&mut pixels, 9, 3, &grade));
        let pixel = pixels[9 + 4];
        assert!(close(pixel[0], 2.0 / 3.0));
        assert!(close(pixel[1], 2.0 / 3.0));
        assert!(close(pixel[2], 2.0 / 3.0));
        assert!(close(pixel[3], 1.0));
    }

    #[test]
    fn view_edge_region_shows_the_gradient() {
        let grade = MatteRefineGrade { feather: 1.0, view_edge_region: 1.0, ..defaults() };
        let mut pixels = step(9, 3, 5);
        assert!(matte_refine_frame_f32(&mut pixels, 9, 3, &grade));
        // Far from the edge the band is empty; on the edge it is wide.
        assert!(close(alpha(&pixels, 9, 1, 1), 0.0));
        let edge = pixels[9 + 4];
        assert!(close(edge[3], 2.0 / 3.0));
        assert!(close(edge[0], 0.2));
        assert!(close(edge[1], 0.4));
        assert!(close(edge[2], 0.6));
    }

    #[test]
    fn view_map_wins_over_the_edge_region_view() {
        let grade = MatteRefineGrade { view_map: 1.0, view_edge_region: 1.0, ..defaults() };
        let mut pixels = frame(3, 1, [0.2, 0.4, 0.6]);
        pixels[1][3] = 0.25;
        assert!(matte_refine_frame_f32(&mut pixels, 3, 1, &grade));
        assert!(close(pixels[1][3], 1.0));
        assert!(close(pixels[1][0], 0.25));
    }

    #[test]
    fn transparent_frames_stay_transparent() {
        let mut pixels = vec![[0.0, 0.0, 0.0, 0.0]; 12 * 5];
        let grade = MatteRefineGrade {
            feather: 2.0,
            shift_edge: 4.0,
            contrast: 50.0,
            ..defaults()
        };
        assert!(matte_refine_frame_f32(&mut pixels, 12, 5, &grade));
        assert!(pixels.iter().all(|pixel| pixel[3] == 0.0));
    }

    #[test]
    fn bad_geometry_is_rejected() {
        let mut empty = Vec::new();
        assert!(!matte_refine_frame_f32(&mut empty, 0, 5, &defaults()));
        let mut short = vec![[0.0, 0.0, 0.0, 1.0]; 3];
        assert!(!matte_refine_frame_f32(&mut short, 4, 4, &defaults()));
        let mut huge = vec![[0.0, 0.0, 0.0, 1.0]; 4];
        assert!(!matte_refine_frame_f32(
            &mut huge,
            usize::MAX,
            2,
            &defaults()
        ));
    }

    #[test]
    fn reserved_parameters_are_validated_but_inert() {
        let grade = MatteRefineGrade {
            tremor_reduction: 4.0,
            tremor_amount: 400.0,
            more_motion_blur: 1.0,
            per_frame_samples: 128.0,
            shutter_angle_degrees: 720.0,
            ..defaults()
        };
        assert!(grade.is_identity());
        let mut pixels = step(9, 3, 4);
        let before = pixels.clone();
        assert!(matte_refine_frame_f32(&mut pixels, 9, 3, &grade));
        assert_eq!(pixels, before);
    }

    #[test]
    fn pass_count_matches_the_stage_plan() {
        assert_eq!(defaults().pass_count(), 1);
        assert_eq!(
            MatteRefineGrade { feather: 12.0, ..defaults() }.pass_count(),
            3
        );
        assert_eq!(
            MatteRefineGrade { smooth: 6.0, ..defaults() }.pass_count(),
            3
        );
        assert_eq!(
            MatteRefineGrade { shift_edge: -4.0, ..defaults() }.pass_count(),
            3
        );
        assert_eq!(
            MatteRefineGrade { shift_edge: 2.0, expand_radius: 2.0, ..defaults() }.pass_count(),
            3
        );
        assert_eq!(
            MatteRefineGrade { shift_edge: -2.0, expand_radius: 4.0, ..defaults() }.pass_count(),
            5
        );
        assert_eq!(
            MatteRefineGrade {
                clean_edge_color: true,
                smooth: 6.0,
                feather: 8.0,
                better_quality: true,
                ..defaults()
            }
            .pass_count(),
            10
        );
    }

    #[test]
    fn gpu_controls_pack_the_passes() {
        let grade = MatteRefineGrade {
            edge_detail: 0.5,
            other_edge_radius: 10.0,
            smooth: 6.0,
            view_map: 1.0,
            contrast: 60.0,
            clean_edge_color: true,
            ..defaults()
        };
        assert_eq!(grade.cleanup_gpu_controls(), [10.0, 1.0, 0.0, 0.0]);
        assert_eq!(grade.smooth_gpu_controls(0.0, false), [6.0, 0.0, 0.5, 0.0]);
        assert_eq!(grade.smooth_gpu_controls(1.0, true), [6.0, 1.0, 0.5, 1.0]);
        assert_eq!(
            grade.morphology_gpu_controls(4.0, 1.0, true, true),
            [4.0, 1.0, 1.0, 1.0]
        );
        assert_eq!(
            grade.morphology_gpu_controls(4.4, 0.0, false, false),
            [4.0, 0.0, 0.0, 0.0]
        );
        assert_eq!(
            grade.feather_gpu_controls(0.0, 2, 0.5, 5.0, true),
            (
                [0.0, 0.0, 2.0, 0.5],
                [5.0, 1.0, 0.0, 0.0],
                [1.0, 0.0, 0.0, 0.0]
            )
        );
        assert_eq!(
            grade.composite_gpu_controls(true),
            ([1.0, 0.0, 60.0, 1.0], [0.0; 4])
        );
    }

    #[test]
    fn smooth_keeps_flat_regions() {
        let mut pixels = frame(7, 5, [0.3, 0.3, 0.3]);
        for pixel in &mut pixels {
            pixel[3] = 0.5;
        }
        let grade = MatteRefineGrade { smooth: 3.0, ..defaults() };
        assert!(matte_refine_frame_f32(&mut pixels, 7, 5, &grade));
        assert!(pixels.iter().all(|pixel| close(pixel[3], 0.5)));
    }

    #[test]
    fn better_quality_blurs_more() {
        let single = MatteRefineGrade { feather: 2.0, ..defaults() };
        let mut once = step(9, 3, 5);
        assert!(matte_refine_frame_f32(&mut once, 9, 3, &single));

        let double = MatteRefineGrade { feather: 2.0, better_quality: true, ..defaults() };
        let mut twice = step(9, 3, 5);
        assert!(matte_refine_frame_f32(&mut twice, 9, 3, &double));

        // The blurred step is linear around column 4, so compare just inside
        // the step: the second box pass keeps pulling opaque alpha outwards.
        assert!(close(alpha(&once, 9, 3, 1), 0.8));
        assert!(close(alpha(&twice, 9, 3, 1), 0.76));
    }

    #[test]
    fn checkpoint_cancellation_stops_the_kernel() {
        let grade = MatteRefineGrade { feather: 8.0, ..defaults() };
        let mut pixels = step(9, 3, 5);
        let mut calls = 0_usize;
        let result: Result<bool, &'static str> =
            super::apply_matte_refine_f32_controlled(&mut pixels, 9, 3, &grade, &mut || {
                calls += 1;
                if calls >= 2 {
                    Err("cancelled")
                } else {
                    Ok(())
                }
            });
        assert_eq!(result, Err("cancelled"));
    }

    #[test]
    fn hash_state_is_stable_for_equal_grades() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::Hasher;

        fn hash(grade: &MatteRefineGrade) -> u64 {
            let mut hasher = DefaultHasher::new();
            grade.hash_state(&mut hasher);
            hasher.finish()
        }

        assert_eq!(hash(&defaults()), hash(&defaults()));
        assert_ne!(
            hash(&defaults()),
            hash(&MatteRefineGrade { feather: 1.0, ..defaults() })
        );
        assert_ne!(
            hash(&defaults()),
            hash(&MatteRefineGrade { tremor_amount: 200.0, ..defaults() })
        );
    }
}
